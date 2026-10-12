//! `cargo run --release -p plumb-scan --example journal-spike -- [drive letter] [seconds]` (elevated).
//!
//! Answers the open questions about the journal on a real drive, then prints a block to paste
//! into the design doc: record rates and reasons while you use the machine normally (start a
//! build or copy while it runs), whether `FSCTL_GET_NTFS_FILE_RECORD` output parses, how often
//! records need the extension-record fallback, and which error codes the odd cases return.

use plumb_core::mft::ParsedRecord;
use plumb_scan::journal::state::{Seen, observe};
use plumb_scan::journal::JournalSource;
use plumb_scan::volume::{Volume, is_elevated};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

fn main() {
    if !is_elevated() {
        eprintln!("journal-spike needs an elevated shell");
        std::process::exit(2);
    }
    let mut args = std::env::args().skip(1);
    let letter = args.next().and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase();
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let vol = Volume::open(letter).expect("open volume");
    let info = vol.query().expect("query journal");
    println!("== journal on {letter}: id {:#x}, next usn {}", info.id, info.next_usn);

    // Timeout / BytesToWaitFor: how long does a waiting read block, and does a non-waiting one return at once?
    for wait in [true, false] {
        let t = Instant::now();
        let b = vol.read(info.next_usn, info.id, wait).expect("read");
        println!("== read(wait={wait}): {} ms, {} entries", t.elapsed().as_millis(), b.entries.len());
    }

    // What error does a stale journal id give? A start USN far in the past?
    println!("== wrong journal id: {:?}", vol.read(info.next_usn, info.id ^ 1, false).err());
    println!("== usn 0 (before the first entry, if the journal has wrapped): {:?}", vol.read(0, info.id, false).map(|b| b.entries.len()));

    // Rates and reasons over `secs` seconds.
    println!("== reading for {secs} s; use the machine normally (build, copy, browse)");
    let mut usn = info.next_usn;
    let mut records: BTreeSet<u64> = BTreeSet::new();
    let mut reasons: BTreeMap<u32, u64> = BTreeMap::new();
    let (mut total, mut windows, mut distinct_per_window_max) = (0u64, 0u64, 0usize);
    let (mut win_start, mut win_set) = (Instant::now(), BTreeSet::new());
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let b = vol.read(usn, info.id, true).expect("read");
        usn = b.next_usn;
        for e in &b.entries {
            total += 1;
            *reasons.entry(e.reason).or_default() += 1;
            records.insert(e.record);
            win_set.insert(e.record);
        }
        if win_start.elapsed() >= Duration::from_millis(250) {
            windows += 1;
            distinct_per_window_max = distinct_per_window_max.max(win_set.len());
            win_set.clear();
            win_start = Instant::now();
        }
    }
    println!("== {total} entries in {secs} s = {:.1}/s; {} distinct files; busiest 250 ms window touched {distinct_per_window_max} files ({windows} windows)", total as f64 / secs as f64, records.len());
    let mut top: Vec<_> = reasons.into_iter().collect();
    top.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (r, n) in top.iter().take(8) {
        println!("   reason {r:#010x}: {n}");
    }

    // Do the MFT records parse? How many are extension records / need the size fallback?
    let (mut ok, mut bad, mut gone, mut ext, mut needs, mut other_num) = (0, 0, 0, 0, 0, 0);
    let mut first_bad = None;
    let (mut buf, mut scratch) = (vec![0u8; 1024], ParsedRecord::default());
    for &r in records.iter().take(5000) {
        buf.fill(0);
        match vol.file_record(r, &mut buf) {
            Ok(Some(n)) if n == r => match observe(r, &buf, &mut scratch) {
                Ok(Seen::Gone) => gone += 1,
                Ok(Seen::Extension(_)) => ext += 1,
                Ok(Seen::Live { needs_size, .. }) => {
                    ok += 1;
                    needs += usize::from(needs_size);
                }
                Err(e) => {
                    bad += 1;
                    first_bad.get_or_insert((r, e));
                }
            },
            Ok(Some(_)) | Ok(None) => other_num += 1,
            Err(e) => println!("   file_record({r}) failed: {e}"),
        }
    }
    println!("== MFT records: {ok} live, {gone} free, {ext} extension, {other_num} not present (nearest lower returned), {bad} unparseable {first_bad:?}; {needs} live files needed the size fallback");

    // Odd requests.
    let mut b = vec![0u8; 1024];
    println!("== record 5 (root): {:?}", vol.file_record(5, &mut b).map(|n| n.map(|n| observe(n, &b, &mut scratch).map(|s| format!("{s:?}").chars().take(60).collect::<String>()))));
    println!("== record 2^40 (beyond the MFT): {:?}", vol.file_record(1 << 40, &mut vec![0u8; 1024]));
}
