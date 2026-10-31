use super::*;
use plumb_core::synth::u;
use std::fs;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Temp {
        let p = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("plumb-watch-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Temp(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(name: &str, action: u32) -> Vec<u8> {
    let n: Vec<u8> = u(name).iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut b = Vec::new();
    b.extend_from_slice(&0u32.to_le_bytes()); // next offset, patched by the caller
    b.extend_from_slice(&action.to_le_bytes());
    b.extend_from_slice(&(n.len() as u32).to_le_bytes());
    b.extend_from_slice(&n);
    b
}

#[test]
fn parse_notify_reads_chained_records() {
    let mut first = record("a\\b.txt", 1);
    let next = first.len() as u32;
    first[0..4].copy_from_slice(&next.to_le_bytes());
    first.extend(record("c", 2));
    assert_eq!(parse_notify(&first), [(1, u("a\\b.txt")), (2, u("c"))]);
    assert!(parse_notify(&first[..10]).is_empty(), "a truncated buffer yields nothing and does not panic");
}

#[test]
fn a_record_dirties_the_parent_and_a_new_directory_is_listed_whole() {
    let mut d = Dirty::default();
    d.record(&u("a\\b\\f.txt"), false);
    d.record(&u("a\\n"), true);
    d.record(&u("top.txt"), false);
    let mut got: Vec<(String, bool)> = d.dirs.iter().map(|(k, v)| (String::from_utf16_lossy(k), *v)).collect();
    got.sort();
    assert_eq!(got, [("".into(), false), ("a".into(), false), ("a\\b".into(), false), ("a\\n".into(), true)]);
}

fn shared(held: bool) -> Shared {
    Shared { dirty: Mutex::new(Dirty::default()), cv: Condvar::new(), stop: AtomicBool::new(false), held: AtomicBool::new(held) }
}

fn root_of(t: &Temp) -> Vec<u16> {
    t.0.as_os_str().encode_wide().collect()
}

#[test]
fn a_held_watch_reports_only_overflow_and_a_released_one_lists() {
    let t = Temp::new("cycle");
    fs::write(t.0.join("a.txt"), b"hi").unwrap();
    let root = root_of(&t);
    let cfg = WatchConfig::default();
    let mut buf = vec![0u64; 4096];
    let sh = shared(true);
    sh.dirty.lock().unwrap().note(&[], false);
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle), "held");
    sh.held.store(false, Ordering::Release);
    let Cycle::Listings(l) = cycle(&sh, &root, &cfg, 0, &mut buf) else { panic!("released: listings") };
    assert_eq!(l.len(), 1);
    assert!(l[0].entries.as_ref().unwrap().iter().any(|e| e.name == u("a.txt")));
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle), "taken");
    sh.dirty.lock().unwrap().overflow = true;
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Stop(Stop::Overflow)));
}

#[test]
fn too_many_dirty_directories_stop_the_watch_only_once_released() {
    let t = Temp::new("many");
    let root = root_of(&t);
    let cfg = WatchConfig { window: Duration::from_millis(10), max_dirty: 2 };
    let mut buf = vec![0u64; 4096];
    let sh = shared(true);
    for d in ["a", "b", "c"] {
        sh.dirty.lock().unwrap().note(&u(d), false);
    }
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle));
    sh.held.store(false, Ordering::Release);
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Stop(Stop::TooMany)));
}

#[test]
fn a_new_directory_is_listed_down_to_its_files_and_parents_come_first() {
    let t = Temp::new("deep");
    fs::create_dir_all(t.0.join("n").join("m")).unwrap();
    fs::write(t.0.join("n").join("m").join("f.txt"), b"x").unwrap();
    let mut buf = vec![0u64; 4096];
    let mut dirs = HashMap::new();
    dirs.insert(u("n\\m"), false);
    dirs.insert(u("n"), true);
    let l = list_dirty(&root_of(&t), dirs, 0, &mut buf);
    let names: Vec<String> = l.iter().map(|x| String::from_utf16_lossy(&x.dir)).collect();
    assert_eq!(names, ["n", "n\\m"], "each once, parents first");
    assert!(l[1].entries.as_ref().unwrap().iter().any(|e| e.name == u("f.txt")));
}

/// Polls the watcher until `want` accepts a listing, or the deadline passes.
fn wait_listing(w: &Watcher, secs: u64, want: &dyn Fn(&Listing) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match w.try_recv() {
            Some(Event::Listings(ls)) if ls.iter().any(want) => return true,
            Some(Event::Stopped(s)) => panic!("watch stopped: {s:?}"),
            Some(_) => {}
            None => std::thread::park_timeout(Duration::from_millis(10)),
        }
    }
    false
}

#[test]
fn a_real_watch_reports_changes_made_while_held_and_after_release() {
    let t = Temp::new("real");
    fs::create_dir_all(t.0.join("sub")).unwrap();
    let w = Watcher::start(&t.0, WatchConfig { window: Duration::from_millis(30), max_dirty: 1000 }, || {}).unwrap();
    fs::write(t.0.join("sub").join("early.bin"), vec![1u8; 4096]).unwrap();
    // The reader has seen it once the parent is in the dirty map: nothing may be sent before release.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !w.shared.dirty.lock().unwrap().dirs.contains_key(&u("sub")) {
        assert!(Instant::now() < deadline, "the reader never saw the write");
        std::thread::park_timeout(Duration::from_millis(5));
    }
    assert!(w.try_recv().is_none(), "held");
    w.release();
    let early = |l: &Listing| l.dir == u("sub") && l.entries.as_ref().is_ok_and(|e| e.iter().any(|x| x.name == u("early.bin")));
    assert!(wait_listing(&w, 10, &early), "a change made while held arrives after release");
    fs::create_dir_all(t.0.join("new").join("inner")).unwrap();
    fs::write(t.0.join("new").join("inner").join("f.txt"), b"x").unwrap();
    let inner = |l: &Listing| l.dir == u("new\\inner") && l.entries.as_ref().is_ok_and(|e| e.iter().any(|x| x.name == u("f.txt")));
    assert!(wait_listing(&w, 10, &inner), "a new directory is listed down to its files");
    w.stop();
}
