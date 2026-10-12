//! `cargo run --release -p plumb-scan --example journal-check -- [drive letter]` (elevated).
//!
//! Drives the real journal reader through create, grow, rename, move and delete of files in a
//! scratch folder at the root of the drive (default C), and checks that each change shows up as the
//! expected delta. Exits non-zero if any step fails. Also times an idle blocking read, which is how
//! the `Timeout` units are confirmed.

use plumb_scan::journal::state::FileState;
use plumb_scan::journal::{Delta, Event, JournalConfig, JournalReader, JournalSource};
use plumb_scan::volume::{Volume, is_elevated};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

struct Watch {
    reader: JournalReader,
    seen: Vec<Delta>,
}

impl Watch {
    /// Waits until `pred` holds for some delta seen so far (including earlier ones), up to 10 s.
    fn wait(&mut self, what: &str, pred: impl Fn(&Delta) -> bool) -> Option<FileState> {
        let t0 = Instant::now();
        loop {
            let hit = self.seen.iter().rev().find(|d| pred(d));
            if let Some(d) = hit {
                println!("ok    {what} ({} ms)", t0.elapsed().as_millis());
                return match d {
                    Delta::Upsert(s) => Some(s.clone()),
                    Delta::Remove { .. } => None,
                };
            }
            if t0.elapsed() > Duration::from_secs(10) {
                println!("FAIL  {what}: nothing matching in 10 s");
                FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                return None;
            }
            match self.reader.recv_timeout(Duration::from_millis(100)) {
                Some(Event::Deltas(d)) => self.seen.extend(d),
                Some(Event::Stopped(why)) => {
                    println!("FAIL  {what}: the reader stopped: {why:?}");
                    FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                    return None;
                }
                None => {}
            }
        }
    }
}

static FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn named(n: &'static str) -> impl Fn(&Delta) -> bool {
    move |d| matches!(d, Delta::Upsert(s) if String::from_utf16_lossy(&s.name) == n)
}

fn main() {
    if !is_elevated() {
        eprintln!("journal-check needs an elevated shell");
        std::process::exit(2);
    }
    let letter = std::env::args().nth(1).and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase();
    let vol = Volume::open(letter).expect("open volume");
    let info = vol.query().expect("query journal");
    println!("journal {:#x}, next usn {}", info.id, info.next_usn);

    // How long does a blocking read wait when nothing happens? (Other activity may cut it short.)
    // Repeats from where the last read ended until one comes back empty: that wait is the timeout.
    let mut from = info.next_usn;
    for _ in 0..8 {
        let t = Instant::now();
        let b = vol.read(from, info.id, true).expect("idle read");
        println!("idle blocking read: {} ms, {} entries (expect about 1000 ms with 0 entries on a quiet drive)", t.elapsed().as_millis(), b.entries.len());
        from = b.next_usn;
        if b.entries.is_empty() {
            break;
        }
    }
    let t = Instant::now();
    vol.read(info.next_usn, info.id, false).expect("non-blocking read");
    println!("non-blocking read: {} ms", t.elapsed().as_millis());

    let reader = JournalReader::start(Volume::open(letter).unwrap(), info, JournalConfig::default(), || {});
    let mut w = Watch { reader, seen: Vec::new() };

    let id = std::process::id();
    let root = format!("{letter}:\\plumb-journal-check-{id}");
    let (d, e) = (format!("{root}\\d{id}"), format!("{root}\\e{id}"));
    let (a, b2, c) = (format!("a{id}.bin"), format!("b{id}.bin"), format!("c{id}.bin"));
    let (dn, en) = (format!("d{id}"), format!("e{id}"));
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let (an, bn, cn, dn, en) = (leak(a.clone()), leak(b2.clone()), leak(c.clone()), leak(dn), leak(en));

    fs::create_dir_all(&d).unwrap();
    let dir = w.wait("create folder", named(dn));
    let fill = |path: &Path, mib: usize| {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![0x5Au8; mib << 20]).unwrap();
        f.sync_all().unwrap();
    };
    fill(&Path::new(&d).join(&a), 1);
    let file = w.wait("create 1 MiB file under the folder", |x| matches!(x, Delta::Upsert(s) if String::from_utf16_lossy(&s.name) == an && s.size >= 1 << 20));
    if let (Some(dir), Some(file)) = (&dir, &file) {
        check("file's parent is the folder", file.parent == dir.record);
    }

    fill(&Path::new(&d).join(&a), 3);
    w.wait("grow to 3 MiB", |x| matches!(x, Delta::Upsert(s) if String::from_utf16_lossy(&s.name) == an && s.size >= 3 << 20));

    fs::rename(Path::new(&d).join(&a), Path::new(&d).join(&b2)).unwrap();
    let renamed = w.wait("rename", named(bn));
    if let (Some(file), Some(renamed)) = (&file, &renamed) {
        check("rename keeps the record", file.record == renamed.record);
    }

    fs::create_dir_all(&e).unwrap();
    let dir2 = w.wait("create second folder", named(en));
    fs::rename(Path::new(&d).join(&b2), Path::new(&e).join(&c)).unwrap();
    let moved = w.wait("move into the second folder (new name)", named(cn));
    if let (Some(moved), Some(dir2)) = (&moved, &dir2) {
        check("moved file's parent is the second folder", moved.parent == dir2.record);
    }

    let rec = moved.map(|m| m.record);
    fs::remove_file(Path::new(&e).join(&c)).unwrap();
    w.wait("delete the file", |x| matches!(x, Delta::Remove { record } if Some(*record) == rec));
    fs::remove_dir_all(&root).unwrap();
    if let Some(dir) = &dir {
        w.wait("delete the first folder", |x| matches!(x, Delta::Remove { record } if *record == dir.record));
    }

    let s = w.reader.stats();
    use std::sync::atomic::Ordering::Relaxed;
    println!(
        "stats: reads {}, entries {}, flushes {}, records read {}, deltas {}, size fallbacks {}, parse errors {}",
        s.reads.load(Relaxed), s.entries.load(Relaxed), s.flushes.load(Relaxed), s.records_read.load(Relaxed),
        s.deltas.load(Relaxed), s.size_fallbacks.load(Relaxed), s.parse_errors.load(Relaxed)
    );
    w.reader.stop();
    if FAILED.load(Relaxed) {
        println!("FAILED");
        std::process::exit(1);
    }
    println!("all steps passed");
}

fn check(what: &str, ok: bool) {
    println!("{}  {what}", if ok { "ok  " } else { "FAIL" });
    if !ok {
        FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
