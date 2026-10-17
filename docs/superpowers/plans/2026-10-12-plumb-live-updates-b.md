---
status: done
---

# plumb live updates, plan 2: the NTFS change-journal reader

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Task 6 needs the user** (an elevated shell); stop there and ask.

**Goal:** `plumb-scan` can follow an NTFS volume's change journal and turn it into a stream of per-file deltas (`Upsert(FileState)` / `Remove`), and the MFT scan also hands back the record-number-to-node table that plan 3 needs to apply them.

**Architecture:** A reader thread reads the USN journal, collects the record numbers that changed for a 250 ms window, re-reads each one's MFT record and reports what is true *now* (state-based, not event-based). All logic is plain Rust over a small `JournalSource` trait and is tested against a scripted fake; the Windows implementation is a thin FFI layer for `Volume`, checked by two elevated examples. Nothing touches the UI or the tree in this plan.

**Tech Stack:** Rust nightly (edition 2024), `windows` 0.62 (features already enabled in `plumb-scan`). Crates: `plumb-core` (one field), `plumb-scan`, and one line in `plumb`.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md` (section "Journal reader"). Plan 1 (`2026-10-12-plumb-live-updates-a.md`) is done. Plan 3 (UI wiring) follows and consumes `JournalReader`, `Delta`, `FileState` and `MftReport::records`.

## Global Constraints

- Deviations from the spec, all to be written back in Task 7:
  1. **State-based, no sequence tracking.** The spec said "reusing a record number with a new sequence number counts as delete plus create". Instead the journal is only a "record N changed" signal, and each changed record is re-read; a reused record simply yields an `Upsert` with the new state (the receiver compares `is_dir`, name, parent and size). Same end result, one code path.
  2. **Reason mask** is `DATA_EXTEND | DATA_OVERWRITE | DATA_TRUNCATION | FILE_CREATE | FILE_DELETE | RENAME_NEW_NAME | REPARSE_POINT_CHANGE | HARD_LINK_CHANGE`. `CLOSE` and `BASIC_INFO_CHANGE` (in the spec) are dropped: neither changes a name, place, size or kind, so asking for them only adds wake-ups. Task 6 confirms the mask misses nothing.
  3. **Extension-record fallback** is `OpenFileById` + `FileStandardInfo.AllocationSize` (as the spec says), used when a live file's base record has no unnamed data stream. Counted in `JournalStats::size_fallbacks`.
  4. **Record table**: the MFT scan returns it (`MftReport::records`) instead of plan 3 rebuilding it. It maps a record number to the node of that record's *first* name; other hard links are not tracked (live updates keep their first-name approximation; plan 3 documents it).
- Journal records are version 2 only (`MinMajorVersion = MaxMajorVersion = 2`).
- The reader never panics on odd data: an unparseable record is counted and skipped, an unreadable one is counted and skipped, and only a failed *journal* read ends the reader (with a `Stop` reason).
- A blocking journal read that returns at once with nothing must not turn the reader into a busy loop (guard in `run`).
- Files in the working copy have CRLF endings (`core.autocrlf=true`). Apply patches with `git apply --ignore-whitespace`; new files may be written with LF.
- Use a separate target dir if a user-run `plumb.exe` locks `target\debug`: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3`.
- Per `LONG_RUNS.md`/`WORKING_STYLE.md`: no fixed sleeps in tests except the fake's stand-in for a blocking read; no stray processes.

---

## File map

| File | Change |
|---|---|
| `crates/plumb-scan/src/merge.rs` | `finish` also returns the record table; `is_name_surrogate` helper |
| `crates/plumb-scan/src/mft_scan.rs` (+`tests.rs`), `volume.rs`, `select.rs`, `crates/plumb/src/window.rs`, `examples/scan-compare.rs` | carry `MftReport { stats, timings, records }` instead of a tuple |
| `crates/plumb-core/src/mft.rs` | `ParsedRecord::sequence` |
| `crates/plumb-scan/src/journal.rs` | new: trait, deltas, events, config, stats, the reader thread |
| `crates/plumb-scan/src/journal/usn.rs` | new: `FSCTL_READ_USN_JOURNAL` buffer parser |
| `crates/plumb-scan/src/journal/state.rs` | new: a changed record -> `FileState` |
| `crates/plumb-scan/src/journal/tests.rs` | new: fake volume and reader tests |
| `crates/plumb-scan/src/journal_win.rs` | new: `JournalSource for Volume` (FFI) |
| `crates/plumb-scan/examples/journal-check.rs`, `journal-spike.rs` | new: elevated end-to-end check and measurements |
| `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md`, `docs/benchmarks.md`, `CHANGELOG.md` | record deviations and findings |

---

### Task 1: The record table comes out of the MFT scan

**Files:**
- Modify: `crates/plumb-scan/src/merge.rs`, `mft_scan.rs`, `mft_scan/tests.rs`, `volume.rs`, `select.rs`, `crates/plumb/src/window.rs`, `crates/plumb-scan/examples/scan-compare.rs`

**Interfaces:**
- Produces:
  - `MftMerger::finish(self, root_name: &str, out: &mut TreeBuilder) -> (MergeStats, Vec<NodeId>)`: index = MFT record number, value = node of that record's first name, `NodeId::NONE` if none.
  - `pub struct MftReport { pub stats: MergeStats, pub timings: MftTimings, pub records: Vec<NodeId> }` in `mft_scan`; `scan_mft`, `MftScanner::scan_with_stats` return it; `ScanReport::mft: Option<MftReport>`.
  - `merge::is_name_surrogate(tag: Option<u32>) -> bool` (used in Task 3).
  - `Volume`'s field becomes `pub(crate)` (used in Task 5).

- [ ] **Step 1: Apply the patch** (`git apply --ignore-whitespace`). It includes a new test, `the_record_table_maps_record_numbers_to_nodes`, in `merge.rs`.

<!-- patch -->
```diff
diff --git a/crates/plumb-scan/examples/scan-compare.rs b/crates/plumb-scan/examples/scan-compare.rs
index 9ab5975..4bb707d 100644
--- a/crates/plumb-scan/examples/scan-compare.rs
+++ b/crates/plumb-scan/examples/scan-compare.rs
@@ -112,7 +112,8 @@ fn main() {
     let deadline = Instant::now() + Duration::from_secs(90);
     let ok = loop {
         let (progress, mut b) = (Progress::default(), TreeBuilder::new());
-        let (stats, timings) = MftScanner::default().scan_with_stats(&drive, &mut b, &progress).expect("mft scan");
+        let r = MftScanner::default().scan_with_stats(&drive, &mut b, &progress).expect("mft scan");
+        let (stats, timings) = (r.stats, r.timings);
         let t = b.finish().unwrap();
         let mut mft_entries = Entries::new();
         if let Some(node) = descend(&t, NodeId(0), &comps) {
diff --git a/crates/plumb-scan/src/merge.rs b/crates/plumb-scan/src/merge.rs
index 4491c17..f5e22ac 100644
--- a/crates/plumb-scan/src/merge.rs
+++ b/crates/plumb-scan/src/merge.rs
@@ -11,6 +11,11 @@ const NONE: u32 = u32::MAX;
 const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
 const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
 
+/// True for the reparse tags the scanners treat as links to follow elsewhere (symlink, junction).
+pub fn is_name_surrogate(tag: Option<u32>) -> bool {
+    matches!(tag, Some(REPARSE_TAG_MOUNT_POINT | REPARSE_TAG_SYMLINK))
+}
+
 const F_SEEN: u8 = 1; // the base record was added
 const F_IN_USE: u8 = 2;
 const F_DIR: u8 = 4;
@@ -97,7 +102,7 @@ impl MftMerger {
                 self.alloc[o] = a;
             }
         }
-        if matches!(rec.reparse_tag, Some(REPARSE_TAG_MOUNT_POINT | REPARSE_TAG_SYMLINK)) {
+        if is_name_surrogate(rec.reparse_tag) {
             self.flags[o] |= F_REPARSE;
         }
     }
@@ -107,7 +112,9 @@ impl MftMerger {
     }
 
     /// Builds the nodes into `out`: the root (named `root_name`) first, then everything else.
-    pub fn finish(self, root_name: &str, out: &mut TreeBuilder) -> MergeStats {
+    /// Also returns, for every record number, the node of its first name (`NodeId::NONE` if it has
+    /// none); live updates use it to find the node a changed record belongs to.
+    pub fn finish(self, root_name: &str, out: &mut TreeBuilder) -> (MergeStats, Vec<NodeId>) {
         let n = self.flags.len();
         let base = out.len() as u32;
 
@@ -237,7 +244,8 @@ impl MftMerger {
             stats.nodes += 1;
         }
         stats.nodes += 1; // the root
-        stats
+        let records = first.into_iter().map(|f| if f == NONE { NodeId::NONE } else { NodeId(base + f) }).collect();
+        (stats, records)
     }
 }
 
@@ -261,7 +269,7 @@ mod tests {
 
     fn build(m: MftMerger) -> (Tree, MergeStats) {
         let mut b = TreeBuilder::new();
-        let stats = m.finish("X:", &mut b);
+        let (stats, _) = m.finish("X:", &mut b);
         (b.finish().unwrap(), stats)
     }
 
@@ -291,6 +299,19 @@ mod tests {
         m
     }
 
+    #[test]
+    fn the_record_table_maps_record_numbers_to_nodes() {
+        let mut b = TreeBuilder::new();
+        let (_, records) = sample(&[0, 1, 2, 3]).finish("X:", &mut b);
+        let t = b.finish().unwrap();
+        assert_eq!(records.len(), 100);
+        assert_eq!(t.path(records[5]), "X:");
+        assert_eq!(t.path(records[30]), "X:\\docs");
+        assert_eq!(t.path(records[31]), "X:\\docs\\a.txt");
+        assert_eq!(t.path(records[32]), "X:\\b");
+        assert!(records[6].is_none() && records[99].is_none(), "unused records have no node");
+    }
+
     #[test]
     fn builds_tree_with_sums() {
         let (t, stats) = build(sample(&[0, 1, 2, 3]));
diff --git a/crates/plumb-scan/src/mft_scan.rs b/crates/plumb-scan/src/mft_scan.rs
index f66ab0e..0bbc511 100644
--- a/crates/plumb-scan/src/mft_scan.rs
+++ b/crates/plumb-scan/src/mft_scan.rs
@@ -3,7 +3,7 @@
 
 use crate::Progress;
 use crate::merge::{MergeStats, MftMerger};
-use plumb_core::TreeBuilder;
+use plumb_core::{NodeId, TreeBuilder};
 use plumb_core::mft::{ParseError, ParsedRecord, mft_extents, parse_record};
 use std::io;
 use std::sync::atomic::Ordering;
@@ -69,6 +69,15 @@ pub struct MftTimings {
     pub link: Duration,
 }
 
+/// What a finished MFT scan reports besides the tree.
+#[derive(Debug)]
+pub struct MftReport {
+    pub stats: MergeStats,
+    pub timings: MftTimings,
+    /// Record number -> node of that record's first name, or `NodeId::NONE`.
+    pub records: Vec<NodeId>,
+}
+
 struct Chunk {
     first_record: u64,
     data: Vec<u8>,
@@ -83,7 +92,7 @@ pub fn scan_mft<V: VolumeRead>(
     opts: &MftOptions,
     out: &mut TreeBuilder,
     progress: &Progress,
-) -> Result<(MergeStats, MftTimings), MftError> {
+) -> Result<MftReport, MftError> {
     let t0 = Instant::now();
     let (sector, bpc) = (layout.bytes_per_sector as u64, layout.bytes_per_cluster as u64);
     if layout.bytes_per_record as usize != RECORD {
@@ -153,8 +162,8 @@ pub fn scan_mft<V: VolumeRead>(
         return Err(MftError::Read(e));
     }
     let t1 = Instant::now();
-    let stats = merger.into_inner().unwrap().finish(root_name, out);
-    Ok((stats, MftTimings { stream: t1 - t0, link: t1.elapsed() }))
+    let (stats, records) = merger.into_inner().unwrap().finish(root_name, out);
+    Ok(MftReport { stats, timings: MftTimings { stream: t1 - t0, link: t1.elapsed() }, records })
 }
 
 /// A parser thread: takes chunks until the reader is done, parses every record outside the lock,
diff --git a/crates/plumb-scan/src/mft_scan/tests.rs b/crates/plumb-scan/src/mft_scan/tests.rs
index 259270d..c3df987 100644
--- a/crates/plumb-scan/src/mft_scan/tests.rs
+++ b/crates/plumb-scan/src/mft_scan/tests.rs
@@ -64,7 +64,7 @@ fn layout() -> MftLayout {
 fn run(img: &Image, l: &MftLayout, threads: usize, chunk_bytes: usize) -> Result<(Tree, MergeStats, Progress), MftError> {
     let mut b = TreeBuilder::new();
     let p = Progress::default();
-    let (stats, _) = scan_mft(img, l, "X:", &MftOptions { threads, chunk_bytes }, &mut b, &p)?;
+    let stats = scan_mft(img, l, "X:", &MftOptions { threads, chunk_bytes }, &mut b, &p)?.stats;
     Ok((b.finish().unwrap(), stats, p))
 }
 
diff --git a/crates/plumb-scan/src/select.rs b/crates/plumb-scan/src/select.rs
index 642eb32..4e74625 100644
--- a/crates/plumb-scan/src/select.rs
+++ b/crates/plumb-scan/src/select.rs
@@ -1,8 +1,7 @@
 //! Picks the scanner for a volume: the MFT reader when the volume is NTFS and the process is
 //! elevated, the directory walker otherwise, and the walker again if the MFT scan fails.
 
-use crate::merge::MergeStats;
-use crate::mft_scan::MftTimings;
+use crate::mft_scan::MftReport;
 use crate::volume::{MftScanner, drive_letter, is_elevated, is_ntfs};
 use crate::walk::WalkScanner;
 use crate::{Progress, ScanError, Scanner};
@@ -22,7 +21,7 @@ pub struct ScanReport {
     /// Something the user should be told (a fallback, or that elevation would be faster).
     pub note: Option<String>,
     /// Present when the MFT scanner ran.
-    pub mft: Option<(MergeStats, MftTimings)>,
+    pub mft: Option<MftReport>,
 }
 
 /// Scans `root` with the best available scanner.
diff --git a/crates/plumb-scan/src/volume.rs b/crates/plumb-scan/src/volume.rs
index 201a37c..a4b71bd 100644
--- a/crates/plumb-scan/src/volume.rs
+++ b/crates/plumb-scan/src/volume.rs
@@ -1,8 +1,7 @@
 //! The Windows side of the MFT scanner: elevation check, file-system check, the raw volume handle
 //! and `MftScanner`. Opening a volume needs administrator rights.
 
-use crate::mft_scan::{MftLayout, MftOptions, MftTimings, VolumeRead, scan_mft};
-use crate::merge::MergeStats;
+use crate::mft_scan::{MftLayout, MftOptions, MftReport, VolumeRead, scan_mft};
 use crate::{Progress, ScanError, Scanner};
 use plumb_core::TreeBuilder;
 use std::ffi::c_void;
@@ -63,7 +62,7 @@ pub fn is_ntfs(root: &Path) -> bool {
 }
 
 /// An open raw volume (`\\.\C:`), read-only.
-pub struct Volume(HANDLE);
+pub struct Volume(pub(crate) HANDLE);
 
 // A volume handle is only read through `ReadFile` with explicit offsets.
 unsafe impl Send for Volume {}
@@ -155,7 +154,7 @@ impl MftScanner {
         root: &Path,
         out: &mut TreeBuilder,
         progress: &Progress,
-    ) -> Result<(MergeStats, MftTimings), ScanError> {
+    ) -> Result<MftReport, ScanError> {
         let letter = drive_letter(root).ok_or_else(|| ScanError::Mft("not a drive root".into()))?;
         let vol = Volume::open(letter).map_err(|e| ScanError::Mft(format!("cannot open volume {letter}: {e}")))?;
         let layout = vol.layout().map_err(|e| ScanError::Mft(format!("cannot query volume {letter}: {e}")))?;
diff --git a/crates/plumb/src/window.rs b/crates/plumb/src/window.rs
index 87e44dc..c4cdf4e 100644
--- a/crates/plumb/src/window.rs
+++ b/crates/plumb/src/window.rs
@@ -276,7 +276,7 @@ impl Win {
         match msg {
             Ok((tree, report, elapsed)) => {
                 let files = tree.subtree_files[NodeId(0)] as u64;
-                let s = summary(report.used, files, elapsed, report.mft.as_ref().map(|m| m.0.corrupt), report.note.as_deref());
+                let s = summary(report.used, files, elapsed, report.mft.as_ref().map(|m| m.stats.corrupt), report.note.as_deref());
                 self.app.finish_scan(tree, s);
             }
             Err(e) => self.app.fail_scan(e),
```

- [ ] **Step 2: Run the tests.**

Run: `cargo test --workspace`
Expected: PASS (the `plumb-scan` lib has 26 tests), no new warnings.

- [ ] **Step 3: Commit.**

```bash
git add -A crates
git commit -m "Return the record-number table from the MFT scan (MftReport)"
```

---

### Task 2: Parsing journal read buffers

**Files:**
- Create: `crates/plumb-scan/src/journal.rs` (stub for now), `crates/plumb-scan/src/journal/usn.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Produces: `journal::usn::{REF_MASK, UsnEntry { record, usn, reason }, UsnBatch { next_usn, entries }, parse_usn_buffer(&[u8]) -> Option<UsnBatch>}`. `UsnEntry::record` has the sequence bits already removed.

- [ ] **Step 1: Create the stub module and declare it.** In `lib.rs`, add `pub mod journal;` above `pub mod merge;`.

<!-- file: crates/plumb-scan/src/journal.rs -->
```rust
//! The NTFS change-journal reader behind live updates (built up over the next tasks).

pub mod usn;
```

- [ ] **Step 2: Create the parser with its tests** (code and tests together; verified together).

<!-- file: crates/plumb-scan/src/journal/usn.rs -->
```rust
//! Parsing the output of `FSCTL_READ_USN_JOURNAL` (version 2 records, which is what NTFS returns
//! when asked for major version 2). Only what live updates need is kept: which file changed.

/// The low 48 bits of a file reference are the MFT record number; the high 16 are its sequence.
pub const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const HEADER: usize = 8; // the next USN, before the first record
const V2_FIXED: usize = 60; // USN_RECORD_V2 up to and including FileNameOffset

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsnEntry {
    /// MFT record number of the file that changed.
    pub record: u64,
    pub usn: i64,
    pub reason: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct UsnBatch {
    /// Where the next read should start.
    pub next_usn: i64,
    pub entries: Vec<UsnEntry>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Parses a read buffer: the next USN, then back-to-back records. `None` if it is malformed
/// (too short, a record that overruns the buffer, or a version other than 2).
pub fn parse_usn_buffer(buf: &[u8]) -> Option<UsnBatch> {
    if buf.len() < HEADER {
        return None;
    }
    let mut batch = UsnBatch { next_usn: u64_at(buf, 0) as i64, entries: Vec::new() };
    let mut off = HEADER;
    while off < buf.len() {
        let rest = &buf[off..];
        if rest.len() < V2_FIXED {
            return None;
        }
        let len = u32_at(rest, 0) as usize;
        if len < V2_FIXED || len > rest.len() || u16_at(rest, 4) != 2 {
            return None;
        }
        batch.entries.push(UsnEntry {
            record: u64_at(rest, 8) & REF_MASK,
            usn: u64_at(rest, 24) as i64,
            reason: u32_at(rest, 40),
        });
        off += len.next_multiple_of(8);
    }
    Some(batch)
}

#[cfg(test)]
pub(crate) fn encode(next_usn: i64, entries: &[(u64, u32)]) -> Vec<u8> {
    let mut b = (next_usn as u64).to_le_bytes().to_vec();
    for (i, &(frn, reason)) in entries.iter().enumerate() {
        let name: Vec<u16> = format!("f{i}").encode_utf16().collect();
        let len = (V2_FIXED + name.len() * 2).next_multiple_of(8);
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[24..32].copy_from_slice(&(100 + i as i64).to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        r[58..60].copy_from_slice(&(V2_FIXED as u16).to_le_bytes());
        for (k, u) in name.iter().enumerate() {
            r[V2_FIXED + 2 * k..V2_FIXED + 2 * k + 2].copy_from_slice(&u.to_le_bytes());
        }
        b.extend(r);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_buffer_is_just_the_next_usn() {
        let b = parse_usn_buffer(&encode(4096, &[])).unwrap();
        assert_eq!(b, UsnBatch { next_usn: 4096, entries: vec![] });
    }

    #[test]
    fn records_are_read_back_to_back_and_the_sequence_is_dropped() {
        let frn = (7u64 << 48) | 1234; // sequence 7, record 1234
        let b = parse_usn_buffer(&encode(900, &[(frn, 0x100), (55, 0x8000_0000), (frn, 2)])).unwrap();
        assert_eq!(b.next_usn, 900);
        let got: Vec<_> = b.entries.iter().map(|e| (e.record, e.usn, e.reason)).collect();
        assert_eq!(got, vec![(1234, 100, 0x100), (55, 101, 0x8000_0000), (1234, 102, 2)]);
    }

    #[test]
    fn malformed_buffers_are_rejected() {
        assert!(parse_usn_buffer(&[1, 2, 3]).is_none());
        let good = encode(1, &[(5, 1)]);
        assert!(parse_usn_buffer(&good[..good.len() - 8]).is_none(), "a record overruns the buffer");
        let mut v3 = good.clone();
        v3[8 + 4] = 3;
        assert!(parse_usn_buffer(&v3).is_none(), "version 3");
        let mut short = good.clone();
        short[8..12].copy_from_slice(&10u32.to_le_bytes());
        assert!(parse_usn_buffer(&short).is_none(), "record length below the fixed part");
    }
}
```

- [ ] **Step 3: Run the tests.**

Run: `cargo test -p plumb-scan --lib journal::usn`
Expected: 3 PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/plumb-scan
git commit -m "Parse FSCTL_READ_USN_JOURNAL buffers"
```

---

### Task 3: A changed record becomes a `FileState`

**Files:**
- Modify: `crates/plumb-core/src/mft.rs`, `crates/plumb-scan/src/journal.rs`
- Create: `crates/plumb-scan/src/journal/state.rs`

**Interfaces:**
- Consumes: `merge::is_name_surrogate` (Task 1), `usn::REF_MASK` (Task 2).
- Produces:
  - `ParsedRecord::sequence: u16` (record header offset 0x10).
  - `journal::state::FileState { record, sequence, parent, name: Vec<u16>, size, is_dir, reparse, links }` with `fn frn(&self) -> u64`.
  - `journal::state::Seen::{Gone, Extension(u64), Live { state, needs_size }}` and `observe(record: u64, buf: &mut [u8], scratch: &mut ParsedRecord) -> Result<Seen, ParseError>`.

- [ ] **Step 1: Add the parser field** (`git apply --ignore-whitespace`). It also extends the `plain_file` test.

<!-- patch -->
```diff
diff --git a/crates/plumb-core/src/mft.rs b/crates/plumb-core/src/mft.rs
index 1b0d0a6..1c13081 100644
--- a/crates/plumb-core/src/mft.rs
+++ b/crates/plumb-core/src/mft.rs
@@ -31,6 +31,8 @@ pub struct NameEntry {
 #[derive(Default, Debug)]
 pub struct ParsedRecord {
     pub in_use: bool,
+    /// The record's sequence number; with the record number it forms the 64-bit file reference.
+    pub sequence: u16,
     pub is_dir: bool,
     /// Record number of the base record if this is an extension record.
     pub base: Option<u64>,
@@ -46,6 +48,7 @@ pub struct ParsedRecord {
 impl ParsedRecord {
     pub fn clear(&mut self) {
         self.in_use = false;
+        self.sequence = 0;
         self.is_dir = false;
         self.base = None;
         self.names.clear();
@@ -124,6 +127,7 @@ pub fn apply_fixups(buf: &mut [u8]) -> Result<(), ParseError> {
 
 /// Reads the header and attributes of a record whose fixups are already applied.
 fn parse_attributes(buf: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
+    out.sequence = u16_at(buf, 0x10)?;
     let flags = u16_at(buf, 0x16)?;
     out.in_use = flags & 1 != 0;
     out.is_dir = flags & 2 != 0;
@@ -292,6 +296,7 @@ mod tests {
         assert_eq!(p.names[0].parent, 5);
         assert_eq!(p.data_alloc, Some(4096));
         assert_eq!(p.base, None);
+        assert_eq!(p.sequence, 1, "the record builder stamps sequence 1");
     }
 
     #[test]
```

- [ ] **Step 2: Declare the module.** In `journal.rs` add `pub mod state;` above `pub mod usn;`.

- [ ] **Step 3: Create `state.rs` with its tests.**

<!-- file: crates/plumb-scan/src/journal/state.rs -->
```rust
//! What a changed file looks like now, read from its MFT record. Live updates are state-based:
//! the journal only says "record N changed"; the record says what is true. That makes record
//! reuse, repeated renames and directory moves all the same case.

use super::usn::REF_MASK;
use crate::merge::is_name_surrogate;
use plumb_core::mft::{ParseError, ParsedRecord, parse_record};

/// One file as it is now. Names follow the scanner's rule: of a hard-linked file's names, the one
/// with the lowest (parent, name) is the one that carries the size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub record: u64,
    pub sequence: u16,
    pub parent: u64,
    pub name: Vec<u16>,
    /// Allocated bytes of the unnamed data stream; 0 for directories.
    pub size: u64,
    pub is_dir: bool,
    /// A symlink or junction.
    pub reparse: bool,
    /// How many names the record has; more than 1 means hard links.
    pub links: u32,
}

impl FileState {
    /// The 64-bit file reference (sequence in the high 16 bits), which `OpenFileById` wants.
    pub fn frn(&self) -> u64 {
        self.record | u64::from(self.sequence) << 48
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// The record is free, or has no usable name.
    Gone,
    /// An extension record: the file it belongs to must be read instead.
    Extension(u64),
    /// `needs_size`: a file whose base record has no unnamed data stream (it lives in an extension
    /// record), so `state.size` is 0 until the caller asks the file system.
    Live { state: FileState, needs_size: bool },
}

/// Parses record number `record` (`buf` is exactly one record; it is modified by the fixups).
pub fn observe(record: u64, buf: &mut [u8], scratch: &mut ParsedRecord) -> Result<Seen, ParseError> {
    parse_record(buf, scratch)?;
    if !scratch.in_use {
        return Ok(Seen::Gone);
    }
    if let Some(base) = scratch.base {
        return Ok(Seen::Extension(base & REF_MASK));
    }
    let Some(first) = scratch.names.iter().min_by(|a, b| (a.parent, scratch.name(a)).cmp(&(b.parent, scratch.name(b))))
    else {
        return Ok(Seen::Gone);
    };
    let state = FileState {
        record,
        sequence: scratch.sequence,
        parent: first.parent,
        name: scratch.name(first).to_vec(),
        size: if scratch.is_dir { 0 } else { scratch.data_alloc.unwrap_or(0) },
        is_dir: scratch.is_dir,
        reparse: is_name_surrogate(scratch.reparse_tag),
        links: scratch.names.len() as u32,
    };
    Ok(Seen::Live { needs_size: !scratch.is_dir && scratch.data_alloc.is_none(), state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::synth::RecordBuilder;

    fn seen(rec: RecordBuilder, number: u64) -> Seen {
        let mut buf = rec.build();
        observe(number, &mut buf, &mut ParsedRecord::default()).unwrap()
    }

    fn live(s: Seen) -> (FileState, bool) {
        match s {
            Seen::Live { state, needs_size } => (state, needs_size),
            other => panic!("not live: {other:?}"),
        }
    }

    #[test]
    fn a_plain_file() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(4096, 100), 31));
        assert_eq!((s.record, s.parent, s.size, s.is_dir, s.reparse, s.links), (31, 30, 4096, false, false, 1));
        assert_eq!(String::from_utf16_lossy(&s.name), "a.txt");
        assert!(!needs);
        assert_eq!(s.frn(), 31 | 1 << 48, "the record builder stamps sequence 1");
    }

    #[test]
    fn a_directory_has_no_size_and_never_needs_one() {
        let (s, needs) = live(seen(RecordBuilder::dir().file_name(5, "d", 1), 40));
        assert_eq!((s.is_dir, s.size, needs), (true, 0, false));
    }

    #[test]
    fn resident_data_is_size_zero_without_a_fallback() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(5, "tiny", 1).data_resident(10), 41));
        assert_eq!((s.size, needs), (0, false));
    }

    #[test]
    fn a_file_without_an_unnamed_stream_asks_for_its_size() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(5, "big", 1), 42));
        assert_eq!((s.size, needs), (0, true));
    }

    #[test]
    fn hard_links_report_the_lowest_parent_then_name() {
        let rec = RecordBuilder::file().file_name(30, "x", 1).file_name(5, "z", 1).file_name(5, "y", 1).data_nonresident(4096, 1);
        let (s, _) = live(seen(rec, 43));
        assert_eq!((s.parent, String::from_utf16_lossy(&s.name), s.links), (5, "y".to_string(), 3));
    }

    #[test]
    fn symlinks_are_marked() {
        let (s, _) = live(seen(RecordBuilder::dir().file_name(5, "j", 1).reparse(0xA000_0003), 44));
        assert!(s.reparse);
    }

    #[test]
    fn extension_records_point_at_their_base() {
        assert_eq!(seen(RecordBuilder::file().extension_of((3 << 48) | 77), 90), Seen::Extension(77));
    }

    #[test]
    fn a_free_record_is_gone() {
        let mut buf = RecordBuilder::file().file_name(5, "old", 1).data_nonresident(4096, 1).build();
        buf[0x16] &= !1; // clear the in-use flag
        assert_eq!(observe(9, &mut buf, &mut ParsedRecord::default()), Ok(Seen::Gone));
    }
}
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p plumb-core --lib mft` and `cargo test -p plumb-scan --lib journal::state`
Expected: PASS (8 new state tests).

- [ ] **Step 5: Commit.**

```bash
git add crates
git commit -m "Read a changed file's state from its MFT record"
```

---

### Task 4: The journal reader

**Files:**
- Replace: `crates/plumb-scan/src/journal.rs`
- Create: `crates/plumb-scan/src/journal/tests.rs`

**Interfaces:**
- Consumes: Tasks 2 and 3.
- Produces (all in `plumb_scan::journal`):
  - `trait JournalSource: Send { fn query(&self) -> Result<JournalInfo, JournalError>; fn read(&self, from: i64, journal_id: u64, wait: bool) -> Result<UsnBatch, JournalError>; fn file_record(&self, record: u64, buf: &mut [u8]) -> io::Result<Option<u64>>; fn allocated_size(&self, frn: u64) -> io::Result<Option<u64>>; }`
  - `JournalInfo { id: u64, next_usn: i64 }`, `JournalError::{Wrapped, Disabled, Other(String)}`
  - `Delta::{Upsert(FileState), Remove { record: u64 }}`, `Event::{Deltas(Vec<Delta>), Stopped(Stop)}`, `Stop::{Requested, Wrapped, Disabled, Failed(String)}`
  - `JournalConfig { window: Duration, max_pending: usize }` (`Default`: 250 ms, 50 000)
  - `JournalReader::start(src, from: JournalInfo, cfg, notify: impl Fn() + Send + 'static) -> JournalReader`, `.try_recv() -> Option<Event>`, `.recv_timeout(Duration) -> Option<Event>`, `.stats() -> &JournalStats`, `.stop(self)`
  - `JournalStats` atomics: `reads, entries, flushes, records_read, deltas, size_fallbacks, parse_errors, record_errors`.

Behaviour to keep in mind: records are flushed in ascending record order; an extension record is replaced by its base; a record the MFT no longer has in use (or that `file_record` answers with a *different, lower* record number) is a `Remove`; when the journal read fails, whatever was collected is sent first, then `Stopped`; dropping the reader (or `stop`) ends the thread within about a second.

- [ ] **Step 1: Write the tests** (the fake volume and 13 tests). Create `journal/tests.rs`:

<!-- file: crates/plumb-scan/src/journal/tests.rs -->
```rust
use super::usn::UsnEntry;
use super::*;
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
        self.mft.lock().unwrap().insert(record, rec.build());
    }
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
    let mut free = file(5, "was-here", 4096).build();
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
```

- [ ] **Step 2: Declare them and confirm the failure.** Append `#[cfg(test)]\nmod tests;` to the Task 2 stub of `journal.rs`.

Run: `cargo test -p plumb-scan --lib journal`
Expected: FAIL to compile (`JournalSource`, `JournalReader` ... not found).

- [ ] **Step 3: Implement.** Replace `journal.rs` entirely (it ends with the same `mod tests;` declaration):

<!-- file: crates/plumb-scan/src/journal.rs -->
```rust
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
```

- [ ] **Step 4: Run the tests, repeatedly** (they involve threads and one 150 ms window).

Run: `cargo test -p plumb-scan --lib journal` three times.
Expected: 24 PASS each time (usn 3 + state 8 + reader 13), about 0.4 s.

- [ ] **Step 5: Mutation-check** (restore each edit afterwards with a manual undo, then rerun to PASS):
  - In `run`, replace `if t0.elapsed() >= cfg.window || pending.len() >= cfg.max_pending {` with `if true {`: expect `changes_in_separate_reads_inside_the_window_are_one_batch` to FAIL.
  - In `flush`, replace `pending.insert(base);` with `let _ = base;`: expect `an_extension_record_is_replaced_by_its_base` to FAIL.
  - Set `MIN_IDLE_WAIT` to `Duration::from_millis(0)`: expect `an_idle_read_that_returns_at_once_is_not_spun_on` to FAIL.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb-scan
git commit -m "Add the change-journal reader over a JournalSource trait"
```

---

### Task 5: The Windows implementation and the two elevated examples

**Files:**
- Create: `crates/plumb-scan/src/journal_win.rs`, `examples/journal-check.rs`, `examples/journal-spike.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Consumes: `Volume` with a `pub(crate)` handle (Task 1), `JournalSource` (Task 4).
- Produces: `impl JournalSource for Volume`.
- Unverified until Task 6 (marked in the code): the units of `Timeout`/`BytesToWaitFor` (assumed seconds; the reader's spin guard protects meanwhile), the error a stale journal ID returns (mapped to `Wrapped`), and what `FSCTL_GET_NTFS_FILE_RECORD` returns for a record number beyond the MFT (mapped to `None`) and whether its bytes parse (assumed raw on-disk form, fixups not yet undone).

- [ ] **Step 1: Declare the module.** In `lib.rs` add, after `pub mod journal;`:

```rust
#[cfg(windows)]
pub mod journal_win;
```

- [ ] **Step 2: Create `journal_win.rs`.**

<!-- file: crates/plumb-scan/src/journal_win.rs -->
```rust
//! The Windows side of the journal reader: `JournalSource` for an open NTFS volume. Needs
//! administrator rights, like the rest of the volume access. Nothing here can be unit-tested
//! without elevation; the `journal-check` example exercises it end to end.

use crate::journal::usn::{REF_MASK, UsnBatch, parse_usn_buffer};
use crate::journal::{JournalError, JournalInfo, JournalSource};
use crate::volume::Volume;
use std::ffi::c_void;
use std::io;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileIdType, FileStandardInfo, GetFileInformationByHandleEx, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_FILE_RECORD, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, NTFS_FILE_RECORD_INPUT_BUFFER,
    READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0, USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE,
    USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_HARD_LINK_CHANGE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_REPARSE_POINT_CHANGE,
};

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;

/// What changes a file's name, place, size, kind or link status. Timestamps and attributes
/// (`BASIC_INFO_CHANGE`) and the `CLOSE` marker change nothing plumb shows, so they are not asked for.
const REASONS: u32 = USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_REPARSE_POINT_CHANGE
    | USN_REASON_HARD_LINK_CHANGE;

const READ_BUFFER: usize = 64 * 1024;
const RECORD_HEADER: usize = 12; // NTFS_FILE_RECORD_OUTPUT_BUFFER up to FileRecordBuffer

fn win32(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

fn journal_error(e: &windows::core::Error, what: &str) -> JournalError {
    match win32(e) {
        ERROR_JOURNAL_ENTRY_DELETED => JournalError::Wrapped,
        ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => JournalError::Disabled,
        // Unverified: what a stale journal ID returns. The journal-spike example prints the code.
        ERROR_INVALID_PARAMETER if what == "reading the journal" => JournalError::Wrapped,
        code => JournalError::Other(format!("{what} failed (error {code}): {e}")),
    }
}

impl Volume {
    fn ioctl<I, O>(&self, code: u32, input: Option<&I>, output: *mut O, out_len: usize) -> windows::core::Result<u32> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.map(|i| (i as *const I).cast::<c_void>()),
                input.map_or(0, |_| std::mem::size_of::<I>() as u32),
                Some(output.cast::<c_void>()),
                out_len as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok(returned)
    }
}

impl JournalSource for Volume {
    fn query(&self) -> Result<JournalInfo, JournalError> {
        let mut d = USN_JOURNAL_DATA_V0::default();
        self.ioctl::<(), _>(FSCTL_QUERY_USN_JOURNAL, None, &mut d, std::mem::size_of::<USN_JOURNAL_DATA_V0>())
            .map_err(|e| journal_error(&e, "querying the journal"))?;
        Ok(JournalInfo { id: d.UsnJournalID, next_usn: d.NextUsn })
    }

    fn read(&self, from: i64, journal_id: u64, wait: bool) -> Result<UsnBatch, JournalError> {
        let input = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: from,
            ReasonMask: REASONS,
            ReturnOnlyOnClose: 0,
            // Unverified units: the journal-check example times an idle wait to confirm about a second.
            Timeout: u64::from(wait),
            BytesToWaitFor: u64::from(wait),
            UsnJournalID: journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 2,
        };
        let mut buf = vec![0u8; READ_BUFFER];
        let n = self
            .ioctl(FSCTL_READ_USN_JOURNAL, Some(&input), buf.as_mut_ptr(), buf.len())
            .map_err(|e| journal_error(&e, "reading the journal"))?;
        parse_usn_buffer(&buf[..n as usize]).ok_or_else(|| JournalError::Other("the journal returned a malformed buffer".into()))
    }

    fn file_record(&self, record: u64, buf: &mut [u8]) -> io::Result<Option<u64>> {
        let input = NTFS_FILE_RECORD_INPUT_BUFFER { FileReferenceNumber: record as i64 };
        let mut out = vec![0u8; RECORD_HEADER + buf.len() + 1024];
        match self.ioctl(FSCTL_GET_NTFS_FILE_RECORD, Some(&input), out.as_mut_ptr(), out.len()) {
            Ok(n) => {
                let n = n as usize;
                let number = u64::from_le_bytes(out[0..8].try_into().unwrap()) & REF_MASK;
                let len = u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize;
                if len != buf.len() || n < RECORD_HEADER + len {
                    return Err(io::Error::other(format!("record {number} is {len} bytes, expected {}", buf.len())));
                }
                buf.copy_from_slice(&out[RECORD_HEADER..RECORD_HEADER + len]);
                Ok(Some(number))
            }
            Err(e) if matches!(win32(&e), ERROR_FILE_NOT_FOUND | ERROR_HANDLE_EOF) => Ok(None),
            Err(e) => Err(io::Error::from_raw_os_error(win32(&e) as i32)),
        }
    }

    fn allocated_size(&self, frn: u64) -> io::Result<Option<u64>> {
        let mut id = FILE_ID_DESCRIPTOR { dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32, Type: FileIdType, ..Default::default() };
        id.Anonymous.FileId = frn as i64;
        let h = match unsafe {
            OpenFileById(
                self.0,
                &id,
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        } {
            Ok(h) => h,
            Err(e) if matches!(win32(&e), ERROR_FILE_NOT_FOUND | ERROR_INVALID_PARAMETER) => return Ok(None),
            Err(e) => return Err(io::Error::from_raw_os_error(win32(&e) as i32)),
        };
        let mut info = FILE_STANDARD_INFO::default();
        let r = unsafe {
            GetFileInformationByHandleEx(h, FileStandardInfo, (&mut info as *mut FILE_STANDARD_INFO).cast(), std::mem::size_of::<FILE_STANDARD_INFO>() as u32)
        };
        close(h);
        r.map_err(|e| io::Error::from_raw_os_error(win32(&e) as i32))?;
        Ok((!info.DeletePending).then_some(info.AllocationSize.max(0) as u64))
    }
}

fn close(h: HANDLE) {
    unsafe {
        let _ = CloseHandle(h);
    }
}
```

- [ ] **Step 3: Create the two examples.**

<!-- file: crates/plumb-scan/examples/journal-check.rs -->
```rust
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
    let t = Instant::now();
    let b = vol.read(info.next_usn, info.id, true).expect("idle read");
    println!("idle blocking read: {} ms, {} entries (expect about 1000 ms on a quiet drive)", t.elapsed().as_millis(), b.entries.len());
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
```

<!-- file: crates/plumb-scan/examples/journal-spike.rs -->
```rust
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
            Ok(Some(n)) if n == r => match observe(r, &mut buf, &mut scratch) {
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
    println!("== record 5 (root): {:?}", vol.file_record(5, &mut b).map(|n| n.map(|n| observe(n, &mut b, &mut scratch).map(|s| format!("{s:?}").chars().take(60).collect::<String>()))));
    println!("== record 2^40 (beyond the MFT): {:?}", vol.file_record(1 << 40, &mut vec![0u8; 1024]));
}
```

- [ ] **Step 4: Build everything and run the unelevated sanity check.**

Run: `cargo test --workspace`, `cargo clippy --workspace --all-targets` (compare with the warnings before Task 1: the only ones in files this plan touches are the old ones at `merge.rs:99`, `mft_scan.rs:182` and `mft_scan/tests.rs:22`; the new files must have none), then `cargo run -q -p plumb-scan --example journal-check`.
Expected: all tests PASS; clippy shows no new warnings; `journal-check` prints `journal-check needs an elevated shell` and exits with code 2 (this is the only thing that can be checked without elevation).

- [ ] **Step 5: Commit.**

```bash
git add crates/plumb-scan
git commit -m "Implement JournalSource for Volume; add journal-check and journal-spike examples"
```

---

### Task 6: The elevated run (needs the user)

**STOP here and ask the user** to open an elevated PowerShell in the repo and run both commands, pasting the output back. Do not try to elevate. While the spike runs they should use the machine normally (a build, a copy, browsing) for the 60 seconds.

```powershell
cargo run --release -p plumb-scan --example journal-spike -- C 60
cargo run --release -p plumb-scan --example journal-check -- C
```

- [ ] **Step 1: Read the spike output against this table and fix what it shows.**

| Output | Meaning | Action |
|---|---|---|
| `read(wait=true)` about 800-1200 ms with 0 entries, `read(wait=false)` under 10 ms | `Timeout = 1` is one second | none |
| `read(wait=true)` under 50 ms with 0 entries | the wait is not honoured or `Timeout` has another unit | set `Timeout` in `journal_win.rs` to the right value for one second (for 100 ns units: `10_000_000`); re-run `journal-check` |
| `read(wait=true)` far above 2 s | units larger than assumed | scale `Timeout` down likewise |
| `wrong journal id:` shows an error code other than 87 | the mapping in `journal_error` is wrong | map that code to `Wrapped` |
| `usn 0` read errors with 1181 (`ERROR_JOURNAL_ENTRY_DELETED`) | confirms `Wrapped` for an overwritten start | none |
| `MFT records:` shows `unparseable` records with `BadFixup` | `FSCTL_GET_NTFS_FILE_RECORD` returns records with fixups already undone | make `observe` skip `apply_fixups` (split `parse_record` in `plumb-core` into `apply_fixups` + `parse_attributes`, both already separate functions) and re-run |
| `record 2^40` returns an error, not `Ok(None)` | beyond-the-MFT answer is another code | add that code to the `Ok(None)` arm of `file_record` |
| size fallback share above 5% of live files | the by-ID fallback will be common | note it; consider caching in plan 3 |
| `busiest 250 ms window` above 50 000 files | the early-flush cap is too low | raise `max_pending` |
| reason histogram shows frequent reasons outside the mask (compare against `0x...` values) | the mask misses something | add it |

- [ ] **Step 2: Read the `journal-check` output.** Every line must start with `ok`; the last line `all steps passed`; exit code 0. A `FAIL` names the step (create, grow, rename, move, delete): fix the cause (the tables above, or the reader), re-run.

- [ ] **Step 3: Record the findings.** In the spec, add a "Spike results (2026-MM-DD)" section under "To verify in the first spike" with the answers to its four questions and the numbers from the output (rates, window size, fallback share, error codes), plus the machine and load. Tick off or reword each item.

- [ ] **Step 4: Commit** any fixes together with the spec update.

```bash
git add -A
git commit -m "Journal reader: fixes and results from the first elevated run"
```

---

### Task 7: Docs

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md`, `CHANGELOG.md`

- [ ] **Step 1: Write the deviations back into the spec** ("Journal reader" section): state-based per-file re-read replaces the sequence-number rule; the reason mask without `CLOSE`/`BASIC_INFO_CHANGE` and why; the record table now comes from `MftReport::records`; hard links beyond the first name are not tracked; the trait's `read(wait)` shape; the spin guard; skipped (counted) unreadable records. Mark plan 2 done in "Plans" and note that plan 3 consumes `JournalReader`, `Delta`, `FileState` and `MftReport::records`.

- [ ] **Step 2: Changelog.** Under `## [Unreleased]` -> `### Added`: `plumb-scan`: NTFS change-journal reader (`journal` module) with elevated `journal-check` and `journal-spike` examples; the MFT scan now also returns the record-number table (`MftReport`). Not yet used by the UI.

- [ ] **Step 3: Whole-workspace check and commit.**

Run: `cargo test --workspace`
Expected: PASS.

```bash
git add docs CHANGELOG.md
git commit -m "Record live-updates plan 2 results"
```

---

## Self-review

- **Spec coverage:** start position and replay (`JournalInfo.next_usn`, caller takes it before the scan: plan 3 wires that); read loop with wait (`read(wait)`, mask); coalescing (window, `BTreeSet`); per-file MFT read with nearest-lower detection (`flush`); deltas; extension-record fallback; stop reasons (`Wrapped`, `Disabled`, `Failed`); the fake-volume test list in the spec (coalescing, rename/move, delete, record reuse, wrap, disabled, extension fallback) is covered, with rename/move being the same `Upsert` path as any change (exercised for real by `journal-check`). The record table (spec "Applying deltas") is Task 1. Everything about applying deltas, rescan, banner and UI is plan 3.
- **Type consistency:** `FileState`, `Seen`, `Delta`, `Event`, `Stop`, `UsnBatch`, `JournalInfo` are defined once and used with identical names in Tasks 3-6; the stub in Task 2 and the full `journal.rs` in Task 4 declare the same modules (`state`, `usn`, `tests`).
- **Stress-test rounds:** (1) a blocking read that returns at once would busy-loop -> spin guard + test; (2) one unreadable MFT record must not kill the reader -> counted and skipped + test; (3) the first draft of the window test passed through the error path without the window ever elapsing -> rewritten to wait for the timed flush and assert it was not early; mutation-checked.
- **Verified before writing:** all code and tests in this plan were built in a scratch worktree of `e04da05`: workspace tests pass (`plumb-core` 55, `plumb-scan` lib 50: 26 existing + 24 journal), the three mutations above fail the intended tests, clippy shows nothing in the new files. The Windows layer compiles and the unelevated path runs; its behaviour on a real journal is exactly what Task 6 measures.
