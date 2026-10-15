//! Elevated check that the MFT scanner and the directory walker agree.
//!
//! `cargo run --release -p plumb-scan --example scan-compare -- C:\plumb-fixture`
//!
//! Run from an elevated terminal on an NTFS drive. The fixture directory must not exist; the
//! example creates it (plain, empty, tiny, resident-boundary, large, Unicode, long-name,
//! alternate-stream, compressed, sparse and junction entries), scans it with the walker, scans
//! the whole volume with the MFT scanner, compares the fixture's subtree in both, and deletes it.
//! One hard-linked pair is included. Each scanner counts the file on a different link, so the pair is
//! compared by its total (the file counted once) rather than link by link.
//! Last-write days must agree to within one day (the walker reads them from the directory index, the MFT scanner from the record).

use plumb_core::{NodeFlags, NodeId, Tree, TreeBuilder};
use plumb_scan::volume::{MftScanner, is_elevated, is_volume_root};
use plumb_scan::walk::WalkScanner;
use plumb_scan::{Progress, Scanner};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn run(cmd: &str, args: &[&str]) {
    let out = Command::new(cmd).args(args).output().expect("spawn");
    assert!(out.status.success(), "{cmd} {args:?} failed: {}", String::from_utf8_lossy(&out.stdout));
}

fn make_fixture(root: &Path) {
    let p = |s: &str| root.join(s);
    for d in ["empty", "a\\b\\c", "unicode-\u{e9}\u{65e5}\u{672c}"] {
        fs::create_dir_all(p(d)).unwrap();
    }
    for (name, len) in [("a\\zero.bin", 0usize), ("a\\one.bin", 1), ("a\\r700.bin", 700), ("a\\b\\five.dat", 5000), ("a\\b\\c\\big.bin", 100_000), ("big3m.bin", 3_000_000)] {
        // pseudo-random-ish content so the file is not compressible by accident
        let data: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(2654435761) >> 7) as u8).collect();
        fs::write(p(name), data).unwrap();
    }
    fs::write(p(&format!("unicode-\u{e9}\u{65e5}\u{672c}\\{}.txt", "n".repeat(200))), b"long name").unwrap();
    fs::write(p("a\\stream.txt"), b"main").unwrap();
    fs::write(p("a\\stream.txt:ads"), vec![9u8; 50_000]).unwrap(); // alternate stream: ignored by both
    fs::hard_link(p("a\\b\\c\\big.bin"), p("linked.bin")).unwrap();
    // Compressed: 1 MiB of zeros compresses to almost nothing.
    fs::write(p("compressed.bin"), vec![0u8; 1 << 20]).unwrap();
    run("compact", &["/c", &p("compressed.bin").to_string_lossy()]);
    // Sparse: 1 MiB file whose data is all deallocated.
    fs::write(p("sparse.bin"), vec![1u8; 1 << 20]).unwrap();
    run("fsutil", &["sparse", "setflag", &p("sparse.bin").to_string_lossy()]);
    run("fsutil", &["file", "setzerodata", "offset=0", &format!("length={}", 1 << 20), &p("sparse.bin").to_string_lossy()]);
    run("cmd", &["/C", "mklink", "/J", &p("junc").to_string_lossy(), &p("a").to_string_lossy()]);
}

fn remove_fixture(root: &Path) {
    let _ = Command::new("cmd").args(["/C", "rmdir"]).arg(root.join("junc")).output();
    let _ = fs::remove_dir_all(root);
}

type Entries = BTreeMap<String, (u64, bool, bool, u32)>; // relative path (lowercase) -> (size, dir, reparse, last-write day or 0)

fn collect(t: &Tree, node: NodeId, prefix: &str, out: &mut Entries) {
    for c in t.children(node) {
        let path = if prefix.is_empty() { t.name_string(c) } else { format!("{prefix}\\{}", t.name_string(c)) };
        out.insert(
            path.to_lowercase(),
            (t.size[c], t.flags[c].contains(NodeFlags::DIR), t.flags[c].contains(NodeFlags::REPARSE), t.mtime[c].map_or(0, |d| d.get())),
        );
        collect(t, c, &path, out);
    }
}

/// The two names of the hard-linked file; the scanners may put its bytes on either.
const LINKS: [&str; 2] = ["a\\b\\c\\big.bin", "linked.bin"];

/// Replaces the pair's sizes by their total under one key, and checks one link carries nothing.
fn fold_links(e: &mut Entries, who: &str) -> Option<String> {
    let sizes: Vec<u64> = LINKS.iter().filter_map(|l| e.get(*l).map(|v| v.0)).collect();
    if !(sizes.len() == 2 && sizes.contains(&0) && sizes.iter().any(|&s| s >= 100_000)) {
        return Some(format!("{who}: the hard-linked pair should count once, got {sizes:?}"));
    }
    for l in LINKS {
        e.get_mut(l).unwrap().0 = 0;
    }
    e.insert("<hard link total>".into(), (sizes.iter().sum(), false, false, 0));
    None
}

fn descend(t: &Tree, from: NodeId, components: &[String]) -> Option<NodeId> {
    let mut cur = from;
    for c in components {
        cur = t.children(cur).find(|&k| t.name_string(k).eq_ignore_ascii_case(c))?;
    }
    Some(cur)
}

fn diff(walk: &Entries, mft: &Entries) -> Vec<String> {
    let mut d = Vec::new();
    for (k, w) in walk {
        match mft.get(k) {
            None => d.push(format!("only in walker: {k} {w:?}")),
            Some(m) if (m.0, m.1, m.2) != (w.0, w.1, w.2) || m.3.abs_diff(w.3) > 1 => d.push(format!("differs: {k}: walker {w:?}, mft {m:?}")),
            _ => {}
        }
    }
    for k in mft.keys().filter(|k| !walk.contains_key(*k)) {
        d.push(format!("only in mft: {k}"));
    }
    d
}

fn main() {
    let fixture = PathBuf::from(std::env::args().nth(1).expect("usage: scan-compare <new directory on an NTFS drive>"));
    if !is_elevated() {
        eprintln!("scan-compare needs an elevated (administrator) terminal");
        std::process::exit(2);
    }
    assert!(!fixture.exists(), "{} already exists; pick a new directory", fixture.display());
    let comps: Vec<String> = fixture.components().filter_map(|c| match c { std::path::Component::Normal(s) => Some(s), _ => None }).map(|s| s.to_string_lossy().into_owned()).collect();
    let drive = PathBuf::from(format!("{}\\", fixture.to_string_lossy().chars().take(2).collect::<String>()));
    assert!(is_volume_root(&drive), "fixture must be on a drive letter");

    make_fixture(&fixture);
    let mut walk_entries = Entries::new();
    {
        let mut b = TreeBuilder::new();
        WalkScanner::default().scan(&fixture, &mut b, &Progress::default()).expect("walk");
        let t = b.finish().unwrap();
        collect(&t, NodeId(0), "", &mut walk_entries);
    }
    if let Some(m) = fold_links(&mut walk_entries, "walker") {
        remove_fixture(&fixture);
        panic!("{m}");
    }
    println!("walker: {} entries under the fixture", walk_entries.len());

    // The MFT on disk can lag the file system cache, so rescan until the two agree or time runs out.
    let deadline = Instant::now() + Duration::from_secs(90);
    let ok = loop {
        let (progress, mut b) = (Progress::default(), TreeBuilder::new());
        let r = MftScanner::default().scan_with_stats(&drive, &mut b, &progress).expect("mft scan");
        let (stats, timings) = (r.stats, r.timings);
        let t = b.finish().unwrap();
        let mut mft_entries = Entries::new();
        if let Some(node) = descend(&t, NodeId(0), &comps) {
            collect(&t, node, "", &mut mft_entries);
        }
        let bad = fold_links(&mut mft_entries, "mft");
        let mut d = if bad.is_some() { Vec::new() } else { diff(&walk_entries, &mft_entries) };
        d.extend(bad);
        println!(
            "mft: {} nodes ({} orphans, {} corrupt), stream {:?}, link {:?}; {} differences",
            stats.nodes, stats.orphans, stats.corrupt, timings.stream, timings.link, d.len()
        );
        if d.is_empty() {
            break true;
        }
        if Instant::now() > deadline {
            d.iter().take(30).for_each(|l| println!("  {l}"));
            break false;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    remove_fixture(&fixture);
    println!("{}", if ok { "OK: the scanners agree" } else { "FAIL: the scanners disagree" });
    std::process::exit(if ok { 0 } else { 1 });
}
