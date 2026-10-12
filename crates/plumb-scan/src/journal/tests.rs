use super::usn::UsnEntry;
use super::*;
use plumb_core::mft::apply_fixups;
use plumb_core::synth::RecordBuilder;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;

/// A scripted volume: a queue of journal reads, a table of MFT records and of file-system sizes.
#[derive(Default)]
struct Fake {
    reads: Mutex<VecDeque<Result<UsnBatch, JournalError>>>,
    mft: Mutex<BTreeMap<u64, Vec<u8>>>,
    sizes: Mutex<HashMap<u64, Option<u64>>>,
    io_error_on: Mutex<Option<u64>>,
}

impl JournalSource for Arc<Fake> {
    fn query(&self) -> Result<JournalInfo, JournalError> {
        Ok(JournalInfo { id: 7, next_usn: 0 })
    }
    fn read(&self, from: i64, _id: u64, wait: bool) -> Result<UsnBatch, JournalError> {
        if let Some(r) = self.reads.lock().unwrap().pop_front() {
            return r;
        }
        if wait {
            std::thread::sleep(Duration::from_millis(5)); // stands in for the blocking wait
        }
        Ok(UsnBatch { next_usn: from, entries: vec![] })
    }
    fn file_record(&self, record: u64, buf: &mut [u8]) -> io::Result<Option<u64>> {
        if *self.io_error_on.lock().unwrap() == Some(record) {
            return Err(io::Error::other("boom"));
        }
        let mft = self.mft.lock().unwrap();
        Ok(mft.range(..=record).next_back().map(|(&n, bytes)| {
            buf.copy_from_slice(bytes);
            n
        }))
    }
    fn allocated_size(&self, frn: u64) -> io::Result<Option<u64>> {
        Ok(self.sizes.lock().unwrap().get(&frn).copied().flatten())
    }
}

fn batch(next: i64, records: &[u64]) -> Result<UsnBatch, JournalError> {
    Ok(UsnBatch { next_usn: next, entries: records.iter().map(|&record| UsnEntry { record, usn: next, reason: 0x100 }).collect() })
}

fn end() -> Result<UsnBatch, JournalError> {
    Err(JournalError::Other("end".into()))
}

impl Fake {
    fn script(self: &Arc<Self>, reads: Vec<Result<UsnBatch, JournalError>>) {
        self.reads.lock().unwrap().extend(reads);
    }
    fn put(self: &Arc<Self>, record: u64, rec: RecordBuilder) {
        self.mft.lock().unwrap().insert(record, fixed(rec));
    }
}

/// A record the way `FSCTL_GET_NTFS_FILE_RECORD` returns it: fixups already undone.
fn fixed(rec: RecordBuilder) -> Vec<u8> {
    let mut b = rec.build();
    apply_fixups(&mut b).unwrap();
    b
}

fn file(parent: u64, name: &str, alloc: u64) -> RecordBuilder {
    RecordBuilder::file().file_name(parent, name, 1).data_nonresident(alloc, alloc)
}

fn cfg(window_ms: u64) -> JournalConfig {
    JournalConfig { window: Duration::from_millis(window_ms), max_pending: 50_000 }
}

/// Runs the reader until it stops and returns its events.
fn run_to_end(fake: &Arc<Fake>, cfg: JournalConfig) -> (Vec<Event>, JournalReader) {
    let reader = JournalReader::start(fake.clone(), JournalInfo { id: 7, next_usn: 0 }, cfg, || {});
    let mut events = Vec::new();
    loop {
        let e = reader.recv_timeout(Duration::from_secs(10)).expect("the reader stopped sending");
        let last = matches!(e, Event::Stopped(_));
        events.push(e);
        if last {
            return (events, reader);
        }
    }
}

fn deltas(events: &[Event]) -> impl Iterator<Item = &Delta> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Deltas(d) => Some(d),
            _ => None,
        })
        .flatten()
}

fn upserts(events: &[Event]) -> Vec<&FileState> {
    deltas(events)
        .filter_map(|d| match d {
            Delta::Upsert(s) => Some(s),
            _ => None,
        })
        .collect()
}

fn removes(events: &[Event]) -> Vec<u64> {
    deltas(events)
        .filter_map(|d| match d {
            Delta::Remove { record } => Some(*record),
            _ => None,
        })
        .collect()
}

#[test]
fn a_burst_of_writes_to_one_file_is_one_upsert() {
    let f = Arc::new(Fake::default());
    f.put(31, file(30, "a.txt", 4096));
    f.script(vec![batch(1, &[31, 31, 31]), end()]);
    let (events, reader) = run_to_end(&f, cfg(0));
    let ups = upserts(&events);
    assert_eq!(ups.len(), 1);
    assert_eq!((ups[0].record, ups[0].parent, ups[0].size), (31, 30, 4096));
    assert_eq!(events.last(), Some(&Event::Stopped(Stop::Failed("end".into()))));
    assert_eq!(reader.stats().entries.load(Ordering::Relaxed), 3);
    assert_eq!(reader.stats().records_read.load(Ordering::Relaxed), 1);
}

#[test]
fn changes_in_separate_reads_inside_the_window_are_one_batch() {
    let f = Arc::new(Fake::default());
    f.put(31, file(30, "a", 4096));
    f.put(32, file(30, "b", 8192));
    f.script(vec![batch(1, &[31]), batch(2, &[32, 31])]); // then the fake goes idle
    let reader = JournalReader::start(f.clone(), JournalInfo { id: 7, next_usn: 0 }, cfg(150), || {});
    let t = Instant::now();
    let Some(Event::Deltas(d)) = reader.recv_timeout(Duration::from_secs(5)) else { panic!("no flush after the window") };
    assert!(t.elapsed() >= Duration::from_millis(150), "flushed early: {:?}", t.elapsed());
    let recs: Vec<u64> = d.iter().map(|x| match x { Delta::Upsert(s) => s.record, Delta::Remove { record } => *record }).collect();
    assert_eq!(recs, vec![31, 32], "both reads in one batch, ordered by record number, each once");
    reader.stop();
}

#[test]
fn a_deleted_file_is_removed() {
    let f = Arc::new(Fake::default());
    f.put(10, file(5, "other", 4096)); // the nearest in-use record below 31
    let mut free = fixed(file(5, "was-here", 4096));
    free[0x16] &= !1; // record 41 is still in the MFT but no longer in use
    f.mft.lock().unwrap().insert(41, free);
    f.script(vec![batch(1, &[31, 41]), end()]);
    let (events, _) = run_to_end(&f, cfg(0));
    assert_eq!(removes(&events), vec![31, 41]);
    assert!(upserts(&events).is_empty());
}

#[test]
fn a_record_reused_for_a_directory_is_just_the_new_state() {
    let f = Arc::new(Fake::default());
    f.put(31, RecordBuilder::dir().file_name(5, "now-a-folder", 1));
    f.script(vec![batch(1, &[31]), end()]);
    let (events, _) = run_to_end(&f, cfg(0));
    let ups = upserts(&events);
    assert_eq!((ups.len(), ups[0].is_dir, ups[0].size), (1, true, 0));
}

#[test]
fn an_extension_record_is_replaced_by_its_base() {
    for script in [vec![90, 77, 90], vec![90]] {
        let f = Arc::new(Fake::default());
        f.put(77, file(5, "base", 4096));
        f.put(90, RecordBuilder::file().extension_of((1 << 48) | 77));
        f.script(vec![batch(1, &script), end()]);
        let (events, reader) = run_to_end(&f, cfg(0));
        let recs: Vec<u64> = upserts(&events).iter().map(|s| s.record).collect();
        assert_eq!(recs, vec![77], "one upsert for the base, none for the extension ({script:?})");
        assert_eq!(reader.stats().records_read.load(Ordering::Relaxed), 2, "the extension, then its base");
    }
}

#[test]
fn a_file_with_its_data_elsewhere_is_sized_by_the_file_system() {
    let f = Arc::new(Fake::default());
    f.put(50, RecordBuilder::file().file_name(5, "fragmented", 1));
    f.put(51, RecordBuilder::file().file_name(5, "vanished", 1));
    f.sizes.lock().unwrap().insert(50 | 1 << 48, Some(1 << 30));
    f.sizes.lock().unwrap().insert(51 | 1 << 48, None);
    f.script(vec![batch(1, &[50, 51]), end()]);
    let (events, reader) = run_to_end(&f, cfg(0));
    assert_eq!(upserts(&events).iter().map(|s| (s.record, s.size)).collect::<Vec<_>>(), vec![(50, 1 << 30)]);
    assert_eq!(removes(&events), vec![51]);
    assert_eq!(reader.stats().size_fallbacks.load(Ordering::Relaxed), 2);
}

#[test]
fn what_was_collected_is_sent_before_a_wrap_or_a_disable_is_reported() {
    for (err, want) in [(JournalError::Wrapped, Stop::Wrapped), (JournalError::Disabled, Stop::Disabled)] {
        let f = Arc::new(Fake::default());
        f.put(31, file(30, "a", 4096));
        f.script(vec![batch(1, &[31]), Err(err)]);
        let (events, _) = run_to_end(&f, cfg(10_000));
        assert_eq!(upserts(&events).len(), 1, "{events:?}");
        assert_eq!(events.last(), Some(&Event::Stopped(want)));
    }
}

#[test]
fn a_full_queue_flushes_before_the_window_ends() {
    let f = Arc::new(Fake::default());
    f.put(31, file(30, "a", 4096));
    f.put(32, file(30, "b", 4096));
    f.script(vec![batch(1, &[31, 32])]);
    let reader = JournalReader::start(f.clone(), JournalInfo { id: 7, next_usn: 0 }, JournalConfig { window: Duration::from_secs(60), max_pending: 2 }, || {});
    let Some(Event::Deltas(d)) = reader.recv_timeout(Duration::from_secs(5)) else { panic!("no early flush") };
    assert_eq!(d.len(), 2);
    reader.stop();
}

#[test]
fn unparseable_records_are_counted_and_skipped() {
    let f = Arc::new(Fake::default());
    f.mft.lock().unwrap().insert(31, vec![0xAB; RECORD]);
    f.script(vec![batch(1, &[31]), end()]);
    let (events, reader) = run_to_end(&f, cfg(0));
    assert!(upserts(&events).is_empty() && removes(&events).is_empty());
    assert_eq!(reader.stats().parse_errors.load(Ordering::Relaxed), 1);
}

#[test]
fn a_record_that_cannot_be_read_is_counted_and_skipped_and_the_rest_still_arrive() {
    let f = Arc::new(Fake::default());
    f.put(32, file(30, "ok", 4096));
    *f.io_error_on.lock().unwrap() = Some(31);
    f.script(vec![batch(1, &[31, 32]), end()]);
    let (events, reader) = run_to_end(&f, cfg(0));
    assert_eq!(upserts(&events).iter().map(|s| s.record).collect::<Vec<_>>(), vec![32]);
    assert_eq!(reader.stats().record_errors.load(Ordering::Relaxed), 1);
    assert_eq!(events.last(), Some(&Event::Stopped(Stop::Failed("end".into()))));
}

#[test]
fn an_idle_read_that_returns_at_once_is_not_spun_on() {
    let f = Arc::new(Fake::default()); // the fake sleeps 5 ms per blocking read, less than MIN_IDLE_WAIT
    let reader = JournalReader::start(f.clone(), JournalInfo { id: 7, next_usn: 0 }, cfg(0), || {});
    assert!(reader.recv_timeout(Duration::from_millis(400)).is_none());
    let reads = reader.stats().reads.load(Ordering::Relaxed);
    reader.stop();
    assert!(reads < 20, "{reads} reads in 400 ms");
}

#[test]
fn stop_returns_promptly_even_while_idle_or_mid_window() {
    for pending in [false, true] {
        let f = Arc::new(Fake::default());
        f.put(31, file(30, "a", 4096));
        if pending {
            f.script(vec![batch(1, &[31])]);
        }
        let reader = JournalReader::start(f.clone(), JournalInfo { id: 7, next_usn: 0 }, cfg(60_000), || {});
        let t = Instant::now();
        reader.stop();
        assert!(t.elapsed() < Duration::from_secs(5), "pending={pending}");
    }
}

#[test]
fn notify_runs_once_per_event() {
    let f = Arc::new(Fake::default());
    f.put(31, file(30, "a", 4096));
    f.script(vec![batch(1, &[31]), end()]);
    let n = Arc::new(AtomicU64::new(0));
    let n2 = n.clone();
    let reader = JournalReader::start(f.clone(), JournalInfo { id: 7, next_usn: 0 }, cfg(0), move || {
        n2.fetch_add(1, Ordering::SeqCst);
    });
    while !matches!(reader.recv_timeout(Duration::from_secs(5)), Some(Event::Stopped(_))) {}
    assert_eq!(n.load(Ordering::SeqCst), 2, "one Deltas, one Stopped");
}
