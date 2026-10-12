//! `cargo run --release -p plumb-scan --example walk -- <path> [threads]`: scan a directory with
//! the walker and print counts and timing.
use plumb_core::{NodeId, TreeBuilder};
use plumb_scan::walk::WalkScanner;
use plumb_scan::{Progress, Scanner};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: walk <path> [threads]");
    let scanner = match args.next() {
        Some(t) => WalkScanner { threads: t.parse().expect("threads must be a number") },
        None => WalkScanner::default(),
    };
    let (progress, mut builder) = (Progress::default(), TreeBuilder::new());
    let t0 = Instant::now();
    scanner.scan(root.as_ref(), &mut builder, &progress).expect("scan failed");
    let t1 = Instant::now();
    let tree = builder.finish().expect("tree");
    let t2 = Instant::now();
    let (dirs, files, _) = progress.snapshot();
    println!(
        "{} threads: {dirs} dirs, {files} files, {:.2} GiB allocated; scan {:?}, sums {:?}",
        scanner.threads,
        tree.subtree_size[NodeId(0)] as f64 / (1u64 << 30) as f64,
        t1 - t0,
        t2 - t1
    );
}
