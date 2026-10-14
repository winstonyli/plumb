//! The directory watch behind live updates for walker-scanned trees. A reader thread turns
//! `ReadDirectoryChangesW` records into a map of dirty directories; a lister thread re-reads them
//! every `window` and sends what they look like now. The kernel only says "something changed in
//! this directory"; the file system says what is true, so renames, reordering and repeated events
//! need no special cases. The watch starts *held*: dirty directories accumulate (so changes made
//! while the scan runs are not lost) and nothing is sent until `release`.
use crate::listing::{Entry, Listing};
use crate::walk::{cluster_size, list_dir};
use std::collections::{HashMap, HashSet};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileAttributesW, OPEN_EXISTING, ReadDirectoryChangesW, FILE_FLAG_OVERLAPPED, FILE_NOTIFY_CHANGE,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows::core::PCWSTR;

/// The network limit; plenty locally (the spike's busiest call used 630 bytes).
const BUF_BYTES: usize = 64 * 1024;
const LIST_BUF_BYTES: usize = 256 * 1024;
const ACTION_ADDED: u32 = 1;
const ACTION_RENAMED_NEW: u32 = 5;
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;
const BS: u16 = b'\\' as u16;

#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// The kernel buffer overflowed: changes were missed.
    Overflow,
    /// So many directories changed at once that a rescan is cheaper.
    TooMany,
    Failed(String),
}

#[derive(Debug)]
pub enum Event {
    /// What the dirty directories look like now, parents before children.
    Listings(Vec<Listing>),
    /// The watch has ended and will send nothing more.
    Stopped(Stop),
}

#[derive(Clone, Copy)]
pub struct WatchConfig {
    /// How long changes are collected before their directories are re-read.
    pub window: Duration,
    /// More dirty directories than this (once released) is a `Stop::TooMany`.
    pub max_dirty: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self { window: Duration::from_millis(250), max_dirty: 50_000 }
    }
}

/// Dirty directories below the root (`\`-separated; empty is the root). The flag marks a
/// directory that is new: its whole subtree must be listed.
#[derive(Default)]
struct Dirty {
    dirs: HashMap<Vec<u16>, bool>,
    overflow: bool,
    failed: Option<String>,
}

impl Dirty {
    fn note(&mut self, dir: &[u16], deep: bool) {
        *self.dirs.entry(dir.to_vec()).or_insert(false) |= deep;
    }

    /// One change record: the parent of `rel` is dirty; a directory that appeared is listed whole.
    fn record(&mut self, rel: &[u16], new_dir: bool) {
        let parent = rel.iter().rposition(|&c| c == BS).map_or(&rel[..0], |i| &rel[..i]);
        self.note(parent, false);
        if new_dir {
            self.note(rel, true);
        }
    }
}

struct Shared {
    dirty: Mutex<Dirty>,
    cv: Condvar,
    stop: AtomicBool,
    held: AtomicBool,
}

/// `(action, name)` for every record in a `FILE_NOTIFY_INFORMATION` buffer.
fn parse_notify(buf: &[u8]) -> Vec<(u32, Vec<u16>)> {
    let u32_at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 12 <= buf.len() {
        let (next, action, len) = (u32_at(off) as usize, u32_at(off + 4), u32_at(off + 8) as usize);
        if off + 12 + len > buf.len() {
            break;
        }
        let name = buf[off + 12..off + 12 + len].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push((action, name));
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

/// `root` (no trailing backslash) joined with `rel`; the root itself keeps its backslash.
fn abs(root: &[u16], rel: &[u16]) -> Vec<u16> {
    let mut p = root.to_vec();
    p.push(BS);
    p.extend_from_slice(rel);
    p
}

fn rel_join(dir: &[u16], name: &[u16]) -> Vec<u16> {
    let mut p = dir.to_vec();
    if !p.is_empty() {
        p.push(BS);
    }
    p.extend_from_slice(name);
    p
}

fn depth(d: &[u16]) -> usize {
    if d.is_empty() { 0 } else { 1 + d.iter().filter(|&&c| c == BS).count() }
}

/// A directory that exists now and is not a symlink or junction.
fn is_real_dir(root: &[u16], rel: &[u16]) -> bool {
    let mut p = abs(root, rel);
    p.push(0);
    let a = unsafe { GetFileAttributesW(PCWSTR(p.as_ptr())) };
    a != u32::MAX && a & FILE_ATTRIBUTE_DIRECTORY.0 != 0 && a & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
}

const FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0
        | FILE_NOTIFY_CHANGE_DIR_NAME.0
        | FILE_NOTIFY_CHANGE_ATTRIBUTES.0
        | FILE_NOTIFY_CHANGE_SIZE.0
        | FILE_NOTIFY_CHANGE_LAST_WRITE.0,
);
const ERROR_OPERATION_ABORTED: u32 = 995;

/// One read request and the buffer the kernel fills for it. The kernel writes into both until the
/// request completes, so a slot must outlive its request: see `drain`.
struct Slot {
    buf: Vec<u64>,
    ov: Box<OVERLAPPED>,
    pending: bool,
}

// The event handle and the buffer are only touched by whichever thread owns the slot.
unsafe impl Send for Slot {}

impl Slot {
    fn new() -> windows::core::Result<Slot> {
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null())? };
        Ok(Slot { buf: vec![0u64; BUF_BYTES / 8], ov: Box::new(OVERLAPPED { hEvent: event, ..Default::default() }), pending: false })
    }

    /// Queues a read; it completes when something in the tree changes.
    fn arm(&mut self, handle: HANDLE) -> windows::core::Result<()> {
        *self.ov = OVERLAPPED { hEvent: self.ov.hEvent, ..Default::default() };
        unsafe { ReadDirectoryChangesW(handle, self.buf.as_mut_ptr().cast(), BUF_BYTES as u32, true, FILTER, None, Some(&mut *self.ov), None)? };
        self.pending = true;
        Ok(())
    }

    /// Waits for the read to finish: the bytes it returned, or its error.
    fn wait(&mut self, handle: HANDLE) -> windows::core::Result<u32> {
        let mut got = 0u32;
        let r = unsafe { GetOverlappedResult(handle, &*self.ov, &mut got, true) };
        self.pending = false;
        r.map(|()| got)
    }

    /// Cancels the read if one is queued and waits until the kernel is done with the buffer.
    fn drain(&mut self, handle: HANDLE) {
        if self.pending {
            unsafe {
                let _ = CancelIoEx(handle, Some(&*self.ov));
            }
            let _ = self.wait(handle);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.ov.hEvent);
        }
    }
}

/// Two reads are always queued, so the gap between one completing and the next being issued
/// never leaves the directory unwatched; the finished one is copied out and re-queued at once.
fn read_loop(handle: isize, root: Vec<u16>, sh: Arc<Shared>, mut slots: [Slot; 2]) {
    let h = HANDLE(handle as *mut _);
    let mut i = 0;
    let end = loop {
        let r = slots[i].wait(h);
        if sh.stop.load(Ordering::Acquire) {
            break None;
        }
        let got = match r {
            Ok(0) => break Some(None),
            Ok(n) => n as usize,
            Err(e) if e.code().0 as u32 & 0xFFFF == ERROR_OPERATION_ABORTED => break None,
            Err(e) => break Some(Some(e)),
        };
        let bytes = unsafe { std::slice::from_raw_parts(slots[i].buf.as_ptr().cast::<u8>(), got) }.to_vec();
        if let Err(e) = slots[i].arm(h) {
            break Some(Some(e));
        }
        i = 1 - i;
        // The attribute check is a system call: do it before taking the lock.
        let recs: Vec<(Vec<u16>, bool)> = parse_notify(&bytes)
            .into_iter()
            .map(|(action, name)| {
                let new_dir = matches!(action, ACTION_ADDED | ACTION_RENAMED_NEW) && is_real_dir(&root, &name);
                (name, new_dir)
            })
            .collect();
        let mut d = sh.dirty.lock().unwrap();
        for (name, new_dir) in recs {
            d.record(&name, new_dir);
        }
    };
    if let Some(e) = end {
        let mut d = sh.dirty.lock().unwrap();
        match e {
            // A read that returns nothing means the kernel buffer overflowed.
            None => d.overflow = true,
            Some(e) if e.code().0 as u32 & 0xFFFF == ERROR_NOTIFY_ENUM_DIR => d.overflow = true,
            Some(e) => d.failed = Some(format!("the directory watch failed: {e}")),
        }
        sh.cv.notify_all();
    }
    for s in &mut slots {
        s.drain(h);
    }
}

/// Lists the dirty directories (new ones recursively, never through reparse points), parents
/// before children, each directory once.
fn list_dirty(root: &[u16], dirs: HashMap<Vec<u16>, bool>, cluster: u64, buf: &mut [u64]) -> Vec<Listing> {
    let mut order: Vec<(Vec<u16>, bool)> = dirs.into_iter().collect();
    order.sort_by_key(|(_, deep)| !*deep); // whole subtrees first, so their members are skipped later
    let mut seen: HashSet<Vec<u16>> = HashSet::new();
    let mut out = Vec::new();
    for (dir, deep) in order {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            if !seen.insert(d.clone()) {
                continue;
            }
            let entries: Result<Vec<Entry>, ()> = list_dir(&abs(root, &d), buf, cluster);
            if deep && let Ok(es) = &entries {
                stack.extend(es.iter().filter(|e| e.is_dir && !e.reparse).map(|e| rel_join(&d, &e.name)));
            }
            out.push(Listing { dir: d, entries });
        }
    }
    out.sort_by_key(|l| depth(&l.dir));
    out
}

enum Cycle {
    Idle,
    Listings(Vec<Listing>),
    Stop(Stop),
}

/// One lister turn. Overflow and failure are reported even while held; listings only once released.
fn cycle(sh: &Shared, root: &[u16], cfg: &WatchConfig, cluster: u64, buf: &mut [u64]) -> Cycle {
    let dirs = {
        let mut d = sh.dirty.lock().unwrap();
        if d.overflow {
            return Cycle::Stop(Stop::Overflow);
        }
        if let Some(m) = d.failed.take() {
            return Cycle::Stop(Stop::Failed(m));
        }
        if sh.held.load(Ordering::Acquire) || d.dirs.is_empty() {
            return Cycle::Idle;
        }
        if d.dirs.len() > cfg.max_dirty {
            return Cycle::Stop(Stop::TooMany);
        }
        std::mem::take(&mut d.dirs)
    };
    Cycle::Listings(list_dirty(root, dirs, cluster, buf))
}

fn lister_loop(root: Vec<u16>, sh: Arc<Shared>, cfg: WatchConfig, tx: Sender<Event>, notify: impl Fn()) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    let cluster = cluster_size(&abs(&root, &[]));
    let mut buf = vec![0u64; LIST_BUF_BYTES / 8];
    loop {
        {
            let g = sh.dirty.lock().unwrap();
            let _ = sh.cv.wait_timeout(g, cfg.window).unwrap();
        }
        if sh.stop.load(Ordering::Acquire) {
            return;
        }
        match cycle(&sh, &root, &cfg, cluster, &mut buf) {
            Cycle::Idle => {}
            Cycle::Listings(l) => {
                if tx.send(Event::Listings(l)).is_err() {
                    return;
                }
                notify();
            }
            Cycle::Stop(s) => {
                let _ = tx.send(Event::Stopped(s));
                notify();
                return;
            }
        }
    }
}

pub struct Watcher {
    events: Receiver<Event>,
    shared: Arc<Shared>,
    handle: isize,
    reader: Option<JoinHandle<()>>,
    lister: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching `root` and everything below it, held. `notify` runs on the lister thread
    /// after every event is queued (post a window message from it); keep it cheap.
    pub fn start(root: &Path, cfg: WatchConfig, notify: impl Fn() + Send + 'static) -> Result<Watcher, String> {
        // The verbatim form (long-path prefix), as the walker uses, so deep paths list too.
        let root = std::fs::canonicalize(root).map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        while wide.len() > 2 && wide.last() == Some(&BS) {
            wide.pop();
        }
        let mut p = abs(&wide, &[]);
        p.push(0);
        let h = unsafe {
            CreateFileW(
                PCWSTR(p.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
        let shared = Arc::new(Shared {
            dirty: Mutex::new(Dirty::default()),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            held: AtomicBool::new(true),
        });
        // Both reads are queued before this returns, so nothing that happens afterwards is missed.
        let slots = (|| Ok::<_, windows::core::Error>([Slot::new()?, Slot::new()?]))()
            .and_then(|mut s| s.iter_mut().try_for_each(|s| s.arm(h)).map(|()| s));
        let slots = match slots {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    let _ = CloseHandle(h);
                }
                return Err(format!("cannot watch {}: {e}", root.display()));
            }
        };
        let (tx, events) = mpsc::channel();
        let handle = h.0 as isize;
        let reader = {
            let (root, sh) = (wide.clone(), shared.clone());
            std::thread::spawn(move || read_loop(handle, root, sh, slots))
        };
        let lister = {
            let sh = shared.clone();
            std::thread::spawn(move || lister_loop(wide, sh, cfg, tx, notify))
        };
        Ok(Watcher { events, shared, handle, reader: Some(reader), lister: Some(lister) })
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Lets the lister send what has accumulated (and everything after).
    pub fn release(&self) {
        self.shared.held.store(false, Ordering::Release);
        self.shared.cv.notify_all();
    }

    /// Stops both threads and closes the directory. Blocks for a few milliseconds (the reads are cancelled
    /// until the reader exits); call it off the UI thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.cv.notify_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        if let Some(t) = self.reader.take() {
            while !t.is_finished() && Instant::now() < deadline {
                unsafe {
                    let _ = CancelIoEx(HANDLE(self.handle as *mut _), None);
                }
                std::thread::park_timeout(Duration::from_millis(5));
            }
            if !t.is_finished() {
                return; // stuck in the kernel: leave the handle open rather than close it under the read
            }
            let _ = t.join();
        }
        if let Some(t) = self.lister.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.handle as *mut _));
        }
        self.handle = 0;
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if self.handle != 0 {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests;
