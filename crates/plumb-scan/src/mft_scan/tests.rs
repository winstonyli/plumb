use super::*;
use plumb_core::synth::RecordBuilder;
use plumb_core::{NodeId, Tree};
use std::collections::BTreeSet;

const BPC: usize = 4096;
const SECTOR: usize = 512;
// The test MFT: 40 records in two fragments, 24 at cluster 3 (6 clusters) and 16 at cluster 20.
const RUNS: [(u64, u64); 2] = [(3, 6), (20, 4)];
const RECORDS: u64 = 40;

struct Image {
    data: Vec<u8>,
    /// Reads that touch this byte offset fail.
    fail_at: Option<u64>,
}

impl VolumeRead for Image {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        assert_eq!(offset % SECTOR as u64, 0, "unaligned offset");
        assert_eq!(buf.len() % SECTOR, 0, "unaligned length");
        if let Some(f) = self.fail_at {
            if (offset..offset + buf.len() as u64).contains(&f) {
                return Err(io::Error::other("injected read failure"));
            }
        }
        buf.copy_from_slice(&self.data[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }
}

fn record_offset(n: u64) -> usize {
    let (lcn, base) = if n < 24 { (RUNS[0].0, 0) } else { (RUNS[1].0, 24) };
    lcn as usize * BPC + (n - base) as usize * RECORD
}

/// $MFT (record 0), the root (5), docs/{a.txt, b.txt}, big.bin and a hard-linked file in two dirs.
fn image() -> Image {
    let mut data = vec![0u8; 64 * BPC];
    let mut put = |n: u64, r: RecordBuilder| {
        let at = record_offset(n);
        data[at..at + RECORD].copy_from_slice(&r.build());
    };
    put(0, RecordBuilder::file().file_name(5, "$MFT", 1).mft_data(&RUNS, RECORDS * RECORD as u64));
    put(5, RecordBuilder::dir().file_name(5, ".", 1));
    put(30, RecordBuilder::dir().file_name(5, "docs", 1));
    put(31, RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(8192, 5000));
    put(32, RecordBuilder::file().file_name(30, "b.txt", 1).data_resident(10));
    put(33, RecordBuilder::file().file_name(5, "big.bin", 1).data_nonresident(1 << 20, 1 << 20));
    put(34, RecordBuilder::file().file_name(30, "link", 1).file_name(5, "link2", 1).data_nonresident(4096, 1));
    Image { data, fail_at: None }
}

fn layout() -> MftLayout {
    MftLayout {
        bytes_per_sector: SECTOR as u32,
        bytes_per_cluster: BPC as u32,
        bytes_per_record: RECORD as u32,
        mft_start_lcn: RUNS[0].0,
        mft_valid_len: RECORDS * RECORD as u64,
    }
}

fn run(img: &Image, l: &MftLayout, threads: usize, chunk_bytes: usize) -> Result<(Tree, MergeStats, Progress), MftError> {
    let mut b = TreeBuilder::new();
    let p = Progress::default();
    let stats = scan_mft(img, l, "X:", &MftOptions { threads, chunk_bytes }, &mut b, &p)?.stats;
    Ok((b.finish().unwrap(), stats, p))
}

fn sorted(t: &Tree) -> BTreeSet<(String, u64)> {
    (0..t.len() as u32).map(|i| (t.path(NodeId(i)), t.size[NodeId(i)])).collect()
}

#[test]
fn scans_a_fragmented_mft() {
    let (t, stats, p) = run(&image(), &layout(), 2, 4 << 20).unwrap();
    let want: BTreeSet<(String, u64)> = [
        ("X:", 0),
        ("X:\\$MFT", RECORDS * RECORD as u64),
        ("X:\\docs", 0),
        ("X:\\docs\\a.txt", 8192),
        ("X:\\docs\\b.txt", 0),
        ("X:\\docs\\link", 0), // the second link (by parent record number) carries no size
        ("X:\\big.bin", 1 << 20),
        ("X:\\link2", 4096),
    ]
    .into_iter()
    .map(|(p, s)| (p.to_string(), s))
    .collect();
    assert_eq!(sorted(&t), want);
    assert_eq!(stats.corrupt, 0);
    let (dirs, files, bytes) = p.snapshot();
    assert_eq!((dirs, files), (2, 5)); // root and docs; $MFT, a, b, big, link
    // The hint counts each file's size once, as the tree does, wherever its record sits.
    assert_eq!(bytes, t.subtree_size[NodeId(0)]);
}

#[test]
fn chunk_size_and_thread_count_do_not_change_the_tree() {
    let (a, _, _) = run(&image(), &layout(), 1, 4 << 20).unwrap();
    for (threads, chunk) in [(1, BPC), (3, BPC), (4, 2 * BPC), (8, 4 << 20)] {
        let (b, _, _) = run(&image(), &layout(), threads, chunk).unwrap();
        assert_eq!(sorted(&a), sorted(&b), "threads {threads}, chunk {chunk}");
    }
}

#[test]
fn corrupt_records_are_counted_and_skipped() {
    let mut img = image();
    let at = record_offset(31);
    img.data[at + 510] ^= 0xFF; // breaks the fixup stamp of a.txt
    let (t, stats, _) = run(&img, &layout(), 2, BPC).unwrap();
    assert_eq!(stats.corrupt, 1);
    assert!(sorted(&t).iter().all(|(p, _)| !p.ends_with("a.txt")));
}

#[test]
fn a_failed_read_is_an_error_and_leaves_the_builder_empty() {
    let mut img = image();
    img.fail_at = Some(record_offset(35) as u64);
    let mut b = TreeBuilder::new();
    let r = scan_mft(&img, &layout(), "X:", &MftOptions { threads: 2, chunk_bytes: BPC }, &mut b, &Progress::default());
    assert!(matches!(r, Err(MftError::Read(_))));
    assert!(b.is_empty());
}

#[test]
fn unsupported_layouts_are_rejected() {
    let img = image();
    let mut l = layout();
    l.bytes_per_record = 4096;
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
    let mut l = layout();
    l.bytes_per_cluster = 512;
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
    let mut l = layout();
    l.mft_valid_len = 100 * RECORD as u64; // more than the extents cover
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
}

#[test]
fn a_bad_record_zero_is_an_error() {
    let mut img = image();
    img.data[record_offset(0)] = b'X';
    assert!(matches!(run(&img, &layout(), 1, BPC), Err(MftError::Record0(_))));
}
