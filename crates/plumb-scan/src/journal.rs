//! The NTFS change-journal reader behind live updates. A thread reads the USN journal, collects
//! the files that changed for a short window, re-reads each one's MFT record and sends the result
//! as deltas. It is state-based: the journal only says "record N changed", the MFT record says
//! what is true now, so record reuse, repeated renames and directory moves need no special cases.
//!
//! Everything here is plain Rust over the [`JournalSource`] trait; the Windows implementation is
//! in `journal_win`.

pub mod state;
pub mod usn;

use plumb_core::mft::ParsedRecord;
use state::{FileState, Seen, observe};
use std::collections::BTreeSet;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use usn::UsnBatch;

const RECORD: usize = 1024;
/// A blocking read that comes back sooner than this with nothing new is not really blocking.
const MIN_IDLE_WAIT: Duration = Duration::from_millis(20);

/// A journal's identity and where it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalInfo {
    pub id: u64,
    pub next_usn: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    /// Entries we still needed were overwritten, or the journal was recreated: changes were missed.
    Wrapped,
    /// The journal is off or being deleted.
    Disabled,
    Other(String),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Wrapped => write!(f, "the change journal wrapped or was recreated"),
            JournalError::Disabled => write!(f, "the change journal is not active"),
            JournalError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// The volume calls the reader needs; a fake implements them in tests.
pub trait JournalSource: Send {
    fn query(&self) -> Result<JournalInfo, JournalError>;
    /// Entries from `from` on, and where to read next. With `wait`, blocks for up to about a
    /// second for something new; without it, returns at once. No entries means nothing new.
    fn read(&self, from: i64, journal_id: u64, wait: bool) -> Result<UsnBatch, JournalError>;
    /// Fills `buf` with the MFT record `record`, or with the nearest in-use record below it.
    /// Returns the record number actually returned, or `None` if there is none.
    fn file_record(&self, record: u64, buf: &mut [u8]) -> io::Result<Option<u64>>;
    /// Allocated bytes of the unnamed stream of the file with reference `frn`; `None` if it is gone.
    fn allocated_size(&self, frn: u64) -> io::Result<Option<u64>>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delta {
    /// The file exists with this state. The receiver decides whether it is new or changed.
    Upsert(FileState),
    Remove { record: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    Requested,
    /// Changes were missed: the tree must be rescanned.
    Wrapped,
    Disabled,
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Deltas(Vec<Delta>),
    /// The reader has ended and will send nothing more.
    Stopped(Stop),
}

#[derive(Clone, Copy, Debug)]
pub struct JournalConfig {
    /// How long changes are collected before their files are re-read.
    pub window: Duration,
    /// Flush early once this many distinct files are waiting.
    pub max_pending: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self { window: Duration::from_millis(250), max_pending: 50_000 }
    }
}

/// Counters for the status bar and the spike; read from any thread.
#[derive(Default, Debug)]
pub struct JournalStats {
    pub reads: AtomicU64,
    pub entries: AtomicU64,
    pub flushes: AtomicU64,
    pub records_read: AtomicU64,
    pub deltas: AtomicU64,
    /// Files sized by asking the file system because their data stream is in an extension record.
    pub size_fallbacks: AtomicU64,
    pub parse_errors: AtomicU64,
    /// Records that could not be read at all; they are skipped and retried at their next change.
    pub record_errors: AtomicU64,
}

pub struct JournalReader {
    events: Receiver<Event>,
    stop: Option<Sender<()>>,
    stats: Arc<JournalStats>,
    thread: Option<JoinHandle<()>>,
}

impl JournalReader {
    /// Starts reading at `from.next_usn`. `notify` runs on the reader's thread after every event
    /// is queued (post a window message from it); keep it cheap.
    pub fn start<S: JournalSource + 'static>(
        src: S,
        from: JournalInfo,
        cfg: JournalConfig,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let (tx, events) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let stats = Arc::new(JournalStats::default());
        let thread = {
            let stats = stats.clone();
            std::thread::spawn(move || run(src, from, cfg, tx, stop_rx, stats, notify))
        };
        Self { events, stop: Some(stop), stats, thread: Some(thread) }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn recv_timeout(&self, d: Duration) -> Option<Event> {
        self.events.recv_timeout(d).ok()
    }

    pub fn stats(&self) -> &JournalStats {
        &self.stats
    }

    /// Asks the reader to stop and waits for it (up to about a second: it may be mid-wait).
    pub fn stop(mut self) {
        self.stop = None; // dropping the sender is the signal
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn to_stop(e: JournalError) -> Stop {
    match e {
        JournalError::Wrapped => Stop::Wrapped,
        JournalError::Disabled => Stop::Disabled,
        JournalError::Other(s) => Stop::Failed(s),
    }
}

fn run<S: JournalSource>(
    src: S,
    info: JournalInfo,
    cfg: JournalConfig,
    tx: Sender<Event>,
    stop: Receiver<()>,
    stats: Arc<JournalStats>,
    notify: impl Fn(),
) {
    let send = |e: Event| {
        let ok = tx.send(e).is_ok();
        notify();
        ok
    };
    let stopped = || !matches!(stop.try_recv(), Err(TryRecvError::Empty));
    let mut usn = info.next_usn;
    let mut pending: BTreeSet<u64> = BTreeSet::new();
    let mut since: Option<Instant> = None;
    let reason = loop {
        if stopped() {
            return;
        }
        let blocking = pending.is_empty();
        let started = Instant::now();
        let batch = match src.read(usn, info.id, blocking) {
            Ok(b) => b,
            Err(e) => break to_stop(e),
        };
        stats.reads.fetch_add(1, Ordering::Relaxed);
        usn = batch.next_usn;
        let idle = batch.entries.is_empty();
        if !idle {
            stats.entries.fetch_add(batch.entries.len() as u64, Ordering::Relaxed);
            since.get_or_insert_with(Instant::now);
            pending.extend(batch.entries.iter().map(|e| e.record));
        }
        let Some(t0) = since else {
            // A blocking read that returns at once with nothing would make this a busy loop.
            if idle && blocking && started.elapsed() < MIN_IDLE_WAIT {
                match stop.recv_timeout(MIN_IDLE_WAIT * 5) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
            }
            continue;
        };
        if t0.elapsed() >= cfg.window || pending.len() >= cfg.max_pending {
            match flush(&src, &mut pending, &stats) {
                Ok(deltas) if deltas.is_empty() => {}
                Ok(deltas) => {
                    if !send(Event::Deltas(deltas)) {
                        return;
                    }
                }
                Err(e) => break to_stop(e),
            }
            since = None;
        } else if idle {
            // Nothing new: wait out the rest of the window, or until asked to stop.
            match stop.recv_timeout(cfg.window.saturating_sub(t0.elapsed())) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return,
            }
        }
    };
    // Whatever was collected before the journal ended is still true; send it first.
    let reason = match flush(&src, &mut pending, &stats) {
        Ok(deltas) if deltas.is_empty() => reason,
        Ok(deltas) => {
            if !send(Event::Deltas(deltas)) {
                return;
            }
            reason
        }
        Err(e) => to_stop(e),
    };
    send(Event::Stopped(reason));
}

/// Re-reads every pending file's MFT record and returns what is true now.
fn flush<S: JournalSource>(src: &S, pending: &mut BTreeSet<u64>, stats: &JournalStats) -> Result<Vec<Delta>, JournalError> {
    stats.flushes.fetch_add(1, Ordering::Relaxed);
    let mut out = Vec::new();
    let mut done: BTreeSet<u64> = BTreeSet::new();
    let (mut buf, mut scratch) = (vec![0u8; RECORD], ParsedRecord::default());
    while let Some(record) = pending.pop_first() {
        if !done.insert(record) {
            continue;
        }
        buf.fill(0);
        stats.records_read.fetch_add(1, Ordering::Relaxed);
        let got = match src.file_record(record, &mut buf) {
            Ok(got) => got,
            Err(_) => {
                stats.record_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        if got != Some(record) {
            out.push(Delta::Remove { record });
            continue;
        }
        match observe(record, &mut buf, &mut scratch) {
            Err(_) => {
                stats.parse_errors.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Seen::Gone) => out.push(Delta::Remove { record }),
            Ok(Seen::Extension(base)) => {
                if base != record && !done.contains(&base) {
                    pending.insert(base);
                }
            }
            Ok(Seen::Live { mut state, needs_size }) => {
                if needs_size {
                    stats.size_fallbacks.fetch_add(1, Ordering::Relaxed);
                    match src.allocated_size(state.frn()) {
                        Ok(Some(size)) => state.size = size,
                        Ok(None) => {
                            out.push(Delta::Remove { record });
                            continue;
                        }
                        Err(_) => {} // keep 0; the next change to this file fixes it
                    }
                }
                out.push(Delta::Upsert(state));
            }
        }
    }
    stats.deltas.fetch_add(out.len() as u64, Ordering::Relaxed);
    Ok(out)
}

#[cfg(test)]
mod tests;
