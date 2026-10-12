# plumb-scan (a): Merge Pass and WalkScanner Implementation Plan (Plan 2a of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline; the tasks depend on each other) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `plumb-scan` crate with the `Scanner` trait, the pure MFT merge pass (`MftMerger`) and the no-admin `WalkScanner`, all tested without elevation.

**Architecture:** `plumb-scan` depends on `plumb-core` and (on Windows) the `windows` crate. `MftMerger` takes parsed MFT records in any order and builds the tree: node order, parent links, hard links, extension records, orphans and parent cycles are all decided in record-number order, so the result does not depend on thread timing. `WalkScanner` runs a coordinator thread that owns the `TreeBuilder` plus N worker threads that list one directory each with `GetFileInformationByHandleEx`.

**Tech Stack:** Rust 2024, `windows` 0.62.2 (features `Win32_Foundation`, `Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_Threading`), `plumb-core`.

**Spec:** `docs/superpowers/specs/2026-10-11-plumb-disk-design.md` (sections "Tree", "Scanners"). Plan 1 (`plumb-core`) is done and merged.

## Global Constraints

- Allocated size is what counts, not logical length. Directories count as 0 in both scanners.
- Only name-surrogate reparse points (symlink `0xA000000C`, mount point/junction `0xA0000003`) get `REPARSE` and are never followed.
- The first node a scanner pushes is the root, named after the path without a `\\?\` prefix or trailing backslash.
- The merge pass breaks ties deterministically: the size-carrying hard link is the first name by (parent record number, name).
- Workers run at BelowNormal priority. Default worker count is `min(cores - 4, 8)`, at least 1.
- Tests never need elevation. They create their fixtures under `std::env::temp_dir()` and remove them.
- Cap build parallelism (`-j 4`). Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Changes to the spec's scanner design

These came out of designing the code; Task 6 updates the spec.
- `Scanner::scan` takes a root `&Path` (a drive root or any directory), not a "volume". This also lets the walker scan a fixture directory.
- The walker uses a coordinator thread that owns the `TreeBuilder` (so node ids need no locking) and a shared job queue, not a work-stealing deque. Workers only list directories.
- `TreeBuilder::add_flags` is new in `plumb-core` (the coordinator learns a directory is `DENIED` after pushing its node).

## Not in this plan (Plan 2b, needs an elevated run to verify)

The `$MFT` volume reader (extents, aligned chunk reads, parser pool feeding `MftMerger`), `MftScanner`, scanner selection and fallback, and `scan-compare`.

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Add `crates/plumb-scan` to the workspace |
| `crates/plumb-core/src/tree.rs` | Add `TreeBuilder::add_flags` |
| `crates/plumb-scan/Cargo.toml` | Crate manifest |
| `crates/plumb-scan/src/lib.rs` | `Scanner`, `Progress`, `ScanError`, module list |
| `crates/plumb-scan/src/merge.rs` | `MftMerger`, `MergeStats` |
| `crates/plumb-scan/src/walk.rs` | `WalkScanner` |
| `crates/plumb-scan/src/walk/tests.rs` | Walker tests on a temp-dir fixture |
| `crates/plumb-scan/examples/walk.rs` | CLI: scan a path, print counts and timing |

---

### Task 1: `TreeBuilder::add_flags`

**Files:**
- Modify: `crates/plumb-core/src/tree.rs`

**Interfaces:**
- Produces: `TreeBuilder::add_flags(&mut self, id: NodeId, flags: NodeFlags)` (ORs `flags` into the node's flags).

- [ ] **Step 1: Write the failing test**

In `crates/plumb-core/src/tree.rs`, inside `mod tests`, add before `fn bad_parent_is_an_error`:

```rust
    #[test]
    fn add_flags_after_push() {
        let mut b = TreeBuilder::new();
        let r = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
        b.add_flags(r, NodeFlags::DENIED);
        let t = b.finish().unwrap();
        assert!(t.flags[r].contains(NodeFlags::DIR | NodeFlags::DENIED));
    }

```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -j 4 -p plumb-core --lib add_flags`
Expected: compile error, `no method named add_flags`.

- [ ] **Step 3: Write the implementation**

In `impl TreeBuilder`, add before `pub fn push`:

```rust
    /// Adds `flags` to an already pushed node (e.g. DENIED, learned after the node was created).
    pub fn add_flags(&mut self, id: NodeId, flags: NodeFlags) {
        self.flags[id] |= flags;
    }

```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 38 passed`

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Add TreeBuilder::add_flags" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `plumb-scan` scaffold, `Scanner` and `Progress`

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/plumb-scan/Cargo.toml`, `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Produces:
  - `Progress { dirs: AtomicU64, files: AtomicU64, bytes: AtomicU64 }` (`Default`), `Progress::snapshot() -> (dirs, files, bytes)`.
  - `ScanError::Root(std::io::Error)`.
  - `trait Scanner { fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError>; }`

- [ ] **Step 1: Create the crate**

In the workspace `Cargo.toml`, change the members line to:

```toml
members = ["crates/plumb-core", "crates/plumb-scan"]
```

`crates/plumb-scan/Cargo.toml`:

```toml
[package]
name = "plumb-scan"
version = "0.1.0"
edition = "2024"

[dependencies]
plumb-core = { path = "../plumb-core" }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62", features = ["Win32_Foundation", "Win32_Security", "Win32_Storage_FileSystem", "Win32_System_Threading"] }
```

`crates/plumb-scan/src/lib.rs`:

```rust
//! Volume scanners for plumb: they turn a directory tree or an NTFS `$MFT` into a `plumb_core::Tree`.

use plumb_core::TreeBuilder;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counters a scanner updates while it runs. Read them from another thread to show progress.
#[derive(Default)]
pub struct Progress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    /// Allocated bytes seen so far.
    pub bytes: AtomicU64,
}

impl Progress {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.dirs.load(Ordering::Relaxed),
            self.files.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

#[derive(Debug)]
pub enum ScanError {
    /// The root path could not be resolved.
    Root(std::io::Error),
}

pub trait Scanner {
    /// Scans `root` (a drive root like `C:\` or any directory) and appends its nodes to `out`.
    /// The first node pushed is the root, named after the path without a trailing backslash.
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError>;
}
```

- [ ] **Step 2: Build**

Run: `cargo build -j 4 -p plumb-scan`
Expected: success (the first build downloads and compiles the `windows` crate, a minute or two).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock crates
git commit -m "Add plumb-scan crate with Scanner trait and Progress" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: MFT merge pass

**Files:**
- Create: `crates/plumb-scan/src/merge.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Consumes: `plumb_core::mft::{ParsedRecord, parse_record}`, `plumb_core::synth::RecordBuilder` (tests), `TreeBuilder`, `NodeFlags`, `NodeId`.
- Produces:
  - `MftMerger::new(record_count: usize)`, `add(&mut self, number: u64, rec: &ParsedRecord)`, `note_corrupt(&mut self)`, `finish(self, root_name: &str, out: &mut TreeBuilder) -> MergeStats`.
  - `MergeStats { records, nodes, orphans, corrupt: u64 }` (`Debug, Default, PartialEq, Eq`).
  - Behavior: `add` may be called in any order, mixing base and extension records; free records are skipped; `finish` pushes the root (record 5, named `root_name`) first, then one node per name of each file and one node per directory, in record-number order; the first name by (parent, name) carries a file's size and later names get size 0 and `HARDLINK`; nodes whose parent record is missing, free or not a directory (and the nodes of parent cycles) go under a synthetic `<orphans>` directory pushed last.

- [ ] **Step 1: Write the failing tests**

In `crates/plumb-scan/src/lib.rs`, add this line after the `//!` doc comment:

```rust
pub mod merge;
```

`crates/plumb-scan/src/merge.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::mft::parse_record;
    use plumb_core::synth::RecordBuilder;
    use plumb_core::Tree;

    fn feed(m: &mut MftMerger, number: u64, rec: RecordBuilder) {
        let mut buf = rec.build();
        let mut p = ParsedRecord::default();
        parse_record(&mut buf, &mut p).unwrap();
        m.add(number, &p);
    }

    fn root(m: &mut MftMerger) {
        feed(m, 5, RecordBuilder::dir().file_name(5, ".", 1));
    }

    fn build(m: MftMerger) -> (Tree, MergeStats) {
        let mut b = TreeBuilder::new();
        let stats = m.finish("X:", &mut b);
        (b.finish().unwrap(), stats)
    }

    fn paths(t: &Tree) -> Vec<String> {
        let mut v: Vec<String> = (0..t.len() as u32).map(|i| t.path(NodeId(i))).collect();
        v.sort();
        v
    }

    fn find(t: &Tree, path: &str) -> NodeId {
        (0..t.len() as u32).map(NodeId).find(|&i| t.path(i) == path).unwrap_or_else(|| panic!("no {path}"))
    }

    fn sample(order: &[usize]) -> MftMerger {
        let mut m = MftMerger::new(100);
        let recs = [
            (5, RecordBuilder::dir().file_name(5, ".", 1)),
            (30, RecordBuilder::dir().file_name(5, "docs", 1)),
            (31, RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(4096, 100)),
            (32, RecordBuilder::file().file_name(5, "b", 1).data_nonresident(8192, 8000)),
        ];
        let mut recs: Vec<_> = recs.into_iter().map(Some).collect();
        for &i in order {
            let (n, r) = recs[i].take().unwrap();
            feed(&mut m, n, r);
        }
        m
    }

    #[test]
    fn builds_tree_with_sums() {
        let (t, stats) = build(sample(&[0, 1, 2, 3]));
        assert_eq!(paths(&t), vec!["X:", "X:\\b", "X:\\docs", "X:\\docs\\a.txt"]);
        assert_eq!(t.subtree_size[NodeId(0)], 4096 + 8192);
        assert_eq!(stats, MergeStats { records: 3, nodes: 4, orphans: 0, corrupt: 0 });
    }

    #[test]
    fn arrival_order_does_not_matter() {
        let (a, _) = build(sample(&[0, 1, 2, 3]));
        let (b, _) = build(sample(&[3, 2, 1, 0]));
        assert_eq!(paths(&a), paths(&b));
        assert_eq!(a.subtree_size[NodeId(0)], b.subtree_size[NodeId(0)]);
        let names = |t: &Tree| (0..t.len() as u32).map(|i| t.name_string(NodeId(i))).collect::<Vec<_>>();
        assert_eq!(names(&a), names(&b)); // node order is by record number, not arrival
    }

    #[test]
    fn hard_links_count_once_on_the_first_link_by_parent() {
        let mut m = sample(&[0, 1, 2, 3]);
        feed(&mut m, 40, RecordBuilder::file().file_name(30, "x", 1).file_name(5, "y", 1).data_nonresident(4096, 10));
        let (t, _) = build(m);
        let y = find(&t, "X:\\y");
        let x = find(&t, "X:\\docs\\x");
        assert_eq!(t.size[y], 4096);
        assert!(!t.flags[y].contains(NodeFlags::HARDLINK));
        assert_eq!(t.size[x], 0);
        assert!(t.flags[x].contains(NodeFlags::HARDLINK));
        assert_eq!(t.subtree_size[NodeId(0)], 4096 + 8192 + 4096);
    }

    #[test]
    fn dos_names_do_not_create_nodes() {
        let mut m = sample(&[0]);
        feed(&mut m, 41, RecordBuilder::file().file_name(5, "PROGRA~1", 2).file_name(5, "Program Files", 1));
        let (t, _) = build(m);
        assert_eq!(paths(&t), vec!["X:", "X:\\Program Files"]);
    }

    #[test]
    fn extension_records_supply_size_and_names_before_or_after_the_base() {
        for ext_first in [false, true] {
            let mut m = sample(&[0]);
            let ext = || RecordBuilder::file().extension_of(31 | (1 << 48)).data_nonresident(8192, 8000);
            let base = || RecordBuilder::file().file_name(5, "big", 1);
            if ext_first {
                feed(&mut m, 90, ext());
                feed(&mut m, 31, base());
            } else {
                feed(&mut m, 31, base());
                feed(&mut m, 90, ext());
            }
            let (t, _) = build(m);
            assert_eq!(t.size[find(&t, "X:\\big")], 8192);
        }
    }

    #[test]
    fn free_records_are_skipped() {
        let mut m = sample(&[0]);
        let mut buf = RecordBuilder::file().file_name(5, "gone", 1).build();
        buf[0x16] = 0; // not in use
        let mut p = ParsedRecord::default();
        parse_record(&mut buf, &mut p).unwrap();
        m.add(50, &p);
        let (t, _) = build(m);
        assert_eq!(paths(&t), vec!["X:"]);
    }

    #[test]
    fn missing_parents_go_under_orphans() {
        let mut m = sample(&[0]);
        feed(&mut m, 60, RecordBuilder::file().file_name(77, "lost", 1).data_nonresident(4096, 1));
        let (t, stats) = build(m);
        assert_eq!(paths(&t), vec!["X:", "X:\\<orphans>", "X:\\<orphans>\\lost"]);
        assert_eq!(stats.orphans, 1);
        assert_eq!(t.subtree_size[NodeId(0)], 4096);
    }

    #[test]
    fn parent_cycles_are_cut() {
        let mut m = sample(&[0]);
        feed(&mut m, 50, RecordBuilder::dir().file_name(51, "d50", 1));
        feed(&mut m, 51, RecordBuilder::dir().file_name(50, "d51", 1));
        let (t, stats) = build(m);
        assert_eq!(stats.orphans, 1);
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn only_name_surrogate_reparse_points_are_flagged() {
        let mut m = sample(&[0]);
        feed(&mut m, 70, RecordBuilder::dir().file_name(5, "junction", 1).reparse(0xA000_0003));
        feed(&mut m, 71, RecordBuilder::dir().file_name(5, "cloud", 1).reparse(0x9000_001A));
        let (t, _) = build(m);
        assert!(t.flags[find(&t, "X:\\junction")].contains(NodeFlags::REPARSE));
        assert!(!t.flags[find(&t, "X:\\cloud")].contains(NodeFlags::REPARSE));
    }

    #[test]
    fn out_of_range_records_count_as_corrupt() {
        let mut m = MftMerger::new(10);
        root(&mut m);
        feed(&mut m, 500, RecordBuilder::file().file_name(5, "x", 1));
        m.note_corrupt();
        let (_, stats) = build(m);
        assert_eq!(stats.corrupt, 2);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-scan --lib merge`
Expected: compile errors, `cannot find type MftMerger` / `MergeStats`.

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-scan/src/merge.rs`, above the `#[cfg(test)]` module:

```rust
//! The MFT merge pass: collects parsed records (in any order, base and extension records mixed)
//! and builds the tree. Parent links, hard links and orphans are all decided here, in record-number
//! order, so the result does not depend on which thread parsed what first.

use plumb_core::mft::ParsedRecord;
use plumb_core::{NodeFlags, NodeId, TreeBuilder};

/// Record number of the root directory of every NTFS volume.
const ROOT_RECORD: usize = 5;
const NONE: u32 = u32::MAX;
const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

const F_SEEN: u8 = 1; // the base record was added
const F_IN_USE: u8 = 2;
const F_DIR: u8 = 4;
const F_REPARSE: u8 = 8; // name-surrogate reparse tag (symlink, junction)
const F_HAS_ALLOC: u8 = 16;

struct NameRec {
    /// Record number of the file this name belongs to (the base record for extensions).
    owner: u32,
    /// Record number of the parent directory.
    parent: u32,
    off: u32,
    len: u8,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// In-use records that produced at least one node.
    pub records: u64,
    pub nodes: u64,
    /// Nodes placed under the synthetic `<orphans>` directory.
    pub orphans: u64,
    /// Records the caller reported as unparseable, plus ones that pointed outside the MFT.
    pub corrupt: u64,
}

pub struct MftMerger {
    flags: Vec<u8>,
    alloc: Vec<u64>,
    names: Vec<NameRec>,
    pool: Vec<u16>,
    corrupt: u64,
}

impl MftMerger {
    /// `record_count` is the number of 1 KiB records in `$MFT` (its size divided by 1024).
    pub fn new(record_count: usize) -> Self {
        Self {
            flags: vec![0; record_count],
            alloc: vec![0; record_count],
            names: Vec::new(),
            pool: Vec::new(),
            corrupt: 0,
        }
    }

    /// Counts a record that failed to parse.
    pub fn note_corrupt(&mut self) {
        self.corrupt += 1;
    }

    /// Adds the parsed record number `number`. Base and extension records may arrive in any order.
    pub fn add(&mut self, number: u64, rec: &ParsedRecord) {
        let owner = rec.base.unwrap_or(number);
        let (Some(o), Ok(own32)) = (usize::try_from(owner).ok().filter(|&o| o < self.flags.len()), u32::try_from(owner))
        else {
            self.corrupt += 1;
            return;
        };
        if rec.base.is_none() {
            self.flags[o] |= F_SEEN;
        }
        if !rec.in_use {
            return;
        }
        if rec.base.is_none() {
            self.flags[o] |= F_IN_USE;
            if rec.is_dir {
                self.flags[o] |= F_DIR;
            }
        }
        for e in &rec.names {
            let Ok(parent) = u32::try_from(e.parent) else {
                self.corrupt += 1;
                continue;
            };
            let off = self.pool.len() as u32;
            self.pool.extend_from_slice(rec.name(e));
            self.names.push(NameRec { owner: own32, parent, off, len: e.len });
        }
        if let Some(a) = rec.data_alloc {
            if self.flags[o] & F_HAS_ALLOC == 0 {
                self.flags[o] |= F_HAS_ALLOC;
                self.alloc[o] = a;
            }
        }
        if matches!(rec.reparse_tag, Some(REPARSE_TAG_MOUNT_POINT | REPARSE_TAG_SYMLINK)) {
            self.flags[o] |= F_REPARSE;
        }
    }

    fn name(&self, n: &NameRec) -> &[u16] {
        &self.pool[n.off as usize..n.off as usize + n.len as usize]
    }

    /// Builds the nodes into `out`: the root (named `root_name`) first, then everything else.
    pub fn finish(self, root_name: &str, out: &mut TreeBuilder) -> MergeStats {
        let n = self.flags.len();
        let base = out.len() as u32;

        // Names grouped by owner, and within an owner ordered by (parent, name) so that
        // "which link carries the size" does not depend on arrival order.
        let mut order: Vec<u32> = (0..self.names.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| {
            let (a, b) = (&self.names[a as usize], &self.names[b as usize]);
            (a.owner, a.parent).cmp(&(b.owner, b.parent)).then_with(|| self.name(a).cmp(self.name(b)))
        });

        // Pass A: give every node an index. `first[rec]` is the index of the record's first node.
        let live = |r: usize| self.flags[r] & (F_SEEN | F_IN_USE) == (F_SEEN | F_IN_USE);
        let mut first = vec![NONE; n];
        let mut count_of = vec![0u32; n];
        let mut next = 1u32; // index 0 is the root
        let mut i = 0;
        while i < order.len() {
            let owner = self.names[order[i] as usize].owner as usize;
            let mut j = i;
            while j < order.len() && self.names[order[j] as usize].owner as usize == owner {
                j += 1;
            }
            if owner != ROOT_RECORD && live(owner) {
                let k = if self.flags[owner] & F_DIR != 0 { 1 } else { (j - i) as u32 };
                first[owner] = next;
                count_of[owner] = k;
                next += k;
            }
            i = j;
        }
        first[ROOT_RECORD] = 0;
        let node_count = next as usize;

        // Pass B: decide every node's parent (an index, or ORPHANS for the synthetic directory).
        let orphans_idx = next;
        let parent_of = |parent_rec: u32| -> u32 {
            let p = parent_rec as usize;
            if p < n && first[p] != NONE && (p == ROOT_RECORD || self.flags[p] & F_DIR != 0) {
                first[p]
            } else {
                orphans_idx
            }
        };
        // (index -> (owner record, name record, is the size-carrying link))
        let mut rows: Vec<(u32, u32, bool)> = Vec::with_capacity(node_count);
        let mut par: Vec<u32> = Vec::with_capacity(node_count);
        par.push(NONE);
        rows.push((ROOT_RECORD as u32, NONE, true));
        let mut i = 0;
        while i < order.len() {
            let owner = self.names[order[i] as usize].owner as usize;
            let mut j = i;
            while j < order.len() && self.names[order[j] as usize].owner as usize == owner {
                j += 1;
            }
            if owner != ROOT_RECORD && live(owner) {
                for (link, &ni) in order[i..i + count_of[owner] as usize].iter().enumerate() {
                    par.push(parent_of(self.names[ni as usize].parent));
                    rows.push((owner as u32, ni, link == 0));
                }
            }
            i = j;
        }

        // Directories that only lead to each other never reach the root: send them to <orphans>.
        // 0 = unvisited, 1 = on the current path, 2 = known to reach the root or <orphans>.
        let mut state = vec![0u8; node_count];
        state[0] = 2;
        let mut path: Vec<usize> = Vec::new();
        for start in 1..node_count {
            let mut cur = start;
            while state[cur] == 0 {
                state[cur] = 1;
                path.push(cur);
                let p = par[cur];
                if p == orphans_idx {
                    break;
                }
                cur = p as usize;
            }
            if state[cur] == 1 && par[cur] != orphans_idx {
                // `cur` is on the current path again: a cycle. Cut it there.
                par[cur] = orphans_idx;
            }
            for v in path.drain(..) {
                state[v] = 2;
            }
        }

        // Push in index order.
        let mut stats = MergeStats { corrupt: self.corrupt, ..Default::default() };
        out.push_str(NodeId::NONE, root_name, 0, NodeFlags::DIR);
        let mut used_orphans = false;
        let mut last_owner = u32::MAX;
        for (idx, &(owner, ni, carries)) in rows.iter().enumerate().skip(1) {
            let o = owner as usize;
            if owner != last_owner {
                stats.records += 1;
                last_owner = owner;
            }
            let is_dir = self.flags[o] & F_DIR != 0;
            let mut flags = NodeFlags::empty();
            if is_dir {
                flags |= NodeFlags::DIR;
            }
            if self.flags[o] & F_REPARSE != 0 {
                flags |= NodeFlags::REPARSE;
            }
            if !carries {
                flags |= NodeFlags::HARDLINK;
            }
            let size = if is_dir || !carries { 0 } else { self.alloc[o] };
            let p = par[idx];
            let parent = if p == orphans_idx {
                used_orphans = true;
                stats.orphans += 1;
                NodeId(base + orphans_idx)
            } else {
                NodeId(base + p)
            };
            out.push(parent, self.name(&self.names[ni as usize]), size, flags);
            stats.nodes += 1;
        }
        if used_orphans {
            out.push_str(NodeId(base), "<orphans>", 0, NodeFlags::DIR);
            stats.nodes += 1;
        }
        stats.nodes += 1; // the root
        stats
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-scan --lib merge`
Expected: `test result: ok. 10 passed`

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Add MFT merge pass: hard links, extensions, orphans, cycles" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `WalkScanner`

**Files:**
- Create: `crates/plumb-scan/src/walk.rs`, `crates/plumb-scan/src/walk/tests.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Consumes: `Scanner`, `Progress`, `ScanError` (Task 2), `TreeBuilder::add_flags` (Task 1).
- Produces: `WalkScanner { pub threads: usize }` with `Default` (`min(cores - 4, 8)`, at least 1) implementing `Scanner`. Behavior: canonicalizes `root` (a missing root gives `ScanError::Root`), pushes the root node first, lists each directory once with `FileFullDirectoryInfo` (256 KB buffers), never follows symlinks/junctions (they get `REPARSE | DIR` and no children), flags unlistable directories `DENIED`, takes file size from `AllocationSize` and directory size as 0, and updates `Progress` as entries arrive. Node order depends on thread timing; the set of paths and all sums do not. A panic in a worker is reported as a `DENIED` directory instead of hanging the scan.

- [ ] **Step 1: Write the failing tests**

In `crates/plumb-scan/src/lib.rs`, add below `pub mod merge;`:

```rust
#[cfg(windows)]
pub mod walk;
```

`crates/plumb-scan/src/walk/tests.rs`:

```rust
use super::*;
use plumb_core::Tree;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        // `rmdir` on the junction removes only the link, not its target.
        let _ = std::process::Command::new("cmd").args(["/C", "rmdir"]).arg(self.0.join("junc")).output();
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// root/{empty/, a/{small.txt, big.bin (100000 B)}, b/c/five.dat (5000 B), junc -> a}
fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("plumb-walk-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b").join("c")).unwrap();
    fs::write(root.join("a").join("small.txt"), b"hi").unwrap();
    fs::write(root.join("a").join("big.bin"), vec![7u8; 100_000]).unwrap();
    fs::write(root.join("b").join("c").join("five.dat"), vec![1u8; 5000]).unwrap();
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join("junc"))
        .arg(root.join("a"))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink /J failed");
    Fixture(root)
}

fn scan(root: &Path, threads: usize) -> (Tree, Progress) {
    let p = Progress::default();
    let mut b = TreeBuilder::new();
    WalkScanner { threads }.scan(root, &mut b, &p).unwrap();
    (b.finish().unwrap(), p)
}

/// Paths relative to the root node.
fn rel(t: &Tree, i: NodeId) -> String {
    t.path(i)[t.path(NodeId(0)).len() + 1..].to_string()
}

fn rel_paths(t: &Tree) -> BTreeSet<String> {
    (1..t.len() as u32).map(|i| rel(t, NodeId(i))).collect()
}

fn find(t: &Tree, path: &str) -> NodeId {
    (1..t.len() as u32).map(NodeId).find(|&i| rel(t, i) == path).unwrap()
}

#[test]
fn finds_every_entry_and_does_not_follow_junctions() {
    let f = fixture("paths");
    let (t, _) = scan(&f.0, 3);
    let want: BTreeSet<String> =
        ["empty", "a", "a\\small.txt", "a\\big.bin", "b", "b\\c", "b\\c\\five.dat", "junc"]
            .into_iter()
            .map(String::from)
            .collect();
    assert_eq!(rel_paths(&t), want);
    let junc = find(&t, "junc");
    assert!(t.flags[junc].contains(NodeFlags::REPARSE | NodeFlags::DIR));
    assert_eq!(t.children(junc).count(), 0);
    assert!(!t.flags[find(&t, "a")].contains(NodeFlags::REPARSE));
}

#[test]
fn sizes_are_allocated_bytes() {
    let f = fixture("sizes");
    let (t, _) = scan(&f.0, 2);
    let big = t.size[find(&t, "a\\big.bin")];
    assert!((100_000..=100_000 + 65_536).contains(&big) && big % 512 == 0, "big = {big}");
    let five = t.size[find(&t, "b\\c\\five.dat")];
    assert!((5000..=5000 + 65_536).contains(&five) && five % 512 == 0, "five = {five}");
    assert_eq!(t.size[find(&t, "a")], 0); // directories count as 0
    assert_eq!(t.subtree_size[find(&t, "a")], t.size[find(&t, "a\\small.txt")] + big);
}

#[test]
fn thread_count_does_not_change_the_result() {
    let f = fixture("threads");
    let (a, _) = scan(&f.0, 1);
    let (b, _) = scan(&f.0, 8);
    assert_eq!(rel_paths(&a), rel_paths(&b));
    assert_eq!(a.subtree_size[NodeId(0)], b.subtree_size[NodeId(0)]);
}

#[test]
fn progress_counts_dirs_and_files() {
    let f = fixture("progress");
    let (t, p) = scan(&f.0, 2);
    let (dirs, files, bytes) = p.snapshot();
    assert_eq!(dirs, 6); // root, empty, a, b, c, junc
    assert_eq!(files, 3);
    assert_eq!(bytes, t.subtree_size[NodeId(0)]);
}

#[test]
fn root_is_named_without_prefix_or_trailing_backslash() {
    let f = fixture("name");
    let (t, _) = scan(&f.0, 1);
    let name = t.name_string(NodeId(0));
    assert!(!name.starts_with("\\\\?\\") && !name.ends_with('\\'), "{name}");
    assert!(t.flags[NodeId(0)].contains(NodeFlags::DIR));
}

#[test]
fn missing_root_is_an_error() {
    let p = Progress::default();
    let mut b = TreeBuilder::new();
    let r = WalkScanner::default().scan(Path::new("C:\\definitely\\not\\here\\plumb"), &mut b, &p);
    assert!(matches!(r, Err(ScanError::Root(_))));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-scan --lib walk`
Expected: compile error `E0583: file not found for module walk`.

- [ ] **Step 3: Write the implementation**

`crates/plumb-scan/src/walk.rs`:

```rust
//! `WalkScanner`: lists directories with `GetFileInformationByHandleEx`; works on any volume
//! without admin rights. It cannot see hard links, so linked files are counted once per link.

use crate::{Progress, ScanError, Scanner};
use plumb_core::{NodeFlags, NodeId, TreeBuilder};
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FULL_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows::core::PCWSTR;

const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const BUF_BYTES: usize = 256 * 1024;

pub struct WalkScanner {
    pub threads: usize,
}

impl Default for WalkScanner {
    /// `min(cores - 4, 8)` workers, at least 1.
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self { threads: cores.saturating_sub(4).clamp(1, 8) }
    }
}

struct Entry {
    name: Vec<u16>,
    alloc: u64,
    is_dir: bool,
    /// A symlink or junction: listed, never followed.
    reparse: bool,
}

struct Job {
    id: NodeId,
    path: Vec<u16>,
}

struct Done {
    id: NodeId,
    /// The listed directory's path, returned so children can extend it.
    path: Vec<u16>,
    /// `Err` when the directory could not be opened or listed (or the worker panicked).
    entries: Result<Vec<Entry>, ()>,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Lists one directory. `dir` is a `\\?\`-style wide path without a trailing NUL.
fn list_dir(dir: &[u16], buf: &mut [u64]) -> Result<Vec<Entry>, ()> {
    let mut p = dir.to_vec();
    p.push(0);
    let h = unsafe {
        CreateFileW(
            PCWSTR(p.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|_| ())?;
    let h = Handle(h);
    let mut out = Vec::new();
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        let r = unsafe {
            GetFileInformationByHandleEx(h.0, class, buf.as_mut_ptr().cast::<c_void>(), (buf.len() * 8) as u32)
        };
        class = FileFullDirectoryInfo;
        if r.is_err() {
            // ERROR_NO_MORE_FILES (18) ends a complete listing; anything else is a failure.
            let code = windows::core::Error::from_thread().code().0 as u32 & 0xFFFF;
            return if code == 18 { Ok(out) } else { Err(()) };
        }
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            let e = unsafe { std::ptr::read_unaligned(base.add(off).cast::<FILE_FULL_DIR_INFO>()) };
            let name_ptr = unsafe { base.add(off + std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName)) };
            let n = e.FileNameLength as usize / 2;
            let name: Vec<u16> =
                (0..n).map(|i| unsafe { std::ptr::read_unaligned(name_ptr.cast::<u16>().add(i)) }).collect();
            let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
            if !dot {
                let is_dir = e.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                // For reparse points the directory entry's EaSize holds the reparse tag.
                let reparse = e.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
                    && matches!(e.EaSize, IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK);
                // Directories count as 0: the MFT scanner does not count index allocation either.
                let alloc = if is_dir { 0 } else { e.AllocationSize as u64 };
                out.push(Entry { name, alloc, is_dir, reparse });
            }
            if e.NextEntryOffset == 0 {
                break;
            }
            off += e.NextEntryOffset as usize;
        }
    }
}

fn join(dir: &[u16], name: &[u16]) -> Vec<u16> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    p.extend_from_slice(dir);
    if p.last() != Some(&(b'\\' as u16)) {
        p.push(b'\\' as u16);
    }
    p.extend_from_slice(name);
    p
}

impl Scanner for WalkScanner {
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError> {
        let root = std::fs::canonicalize(root).map_err(ScanError::Root)?; // verbatim `\\?\` form
        let root_wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        let shown = OsString::from_wide(&root_wide).to_string_lossy().into_owned();
        let shown = shown.strip_prefix(r"\\?\").unwrap_or(&shown).trim_end_matches('\\').to_string();

        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done_rx) = mpsc::channel::<Done>();

        let root_id = out.push_str(NodeId::NONE, &shown, 0, NodeFlags::DIR);
        progress.dirs.fetch_add(1, Ordering::Relaxed);
        job_tx.send(Job { id: root_id, path: root_wide }).unwrap();
        let mut outstanding = 1usize;

        std::thread::scope(|s| {
            for _ in 0..self.threads.max(1) {
                let (rx, tx) = (Arc::clone(&job_rx), done_tx.clone());
                s.spawn(move || {
                    unsafe {
                        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                    let mut buf = vec![0u64; BUF_BYTES / 8];
                    // The lock is held only while waiting for a job, never while listing.
                    while let Ok(job) = rx.lock().unwrap().recv() {
                        let entries = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            list_dir(&job.path, &mut buf)
                        }))
                        .unwrap_or(Err(()));
                        if tx.send(Done { id: job.id, path: job.path, entries }).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(done_tx);

            // This thread owns the builder: it assigns ids and queues subdirectories.
            while outstanding > 0 {
                let Ok(done) = done_rx.recv() else { break };
                outstanding -= 1;
                let Ok(entries) = done.entries else {
                    out.add_flags(done.id, NodeFlags::DENIED);
                    continue;
                };
                for e in entries {
                    let mut flags = NodeFlags::empty();
                    if e.is_dir {
                        flags |= NodeFlags::DIR;
                    }
                    if e.reparse {
                        flags |= NodeFlags::REPARSE;
                    }
                    let id = out.push(done.id, &e.name, e.alloc, flags);
                    if e.is_dir {
                        progress.dirs.fetch_add(1, Ordering::Relaxed);
                    } else {
                        progress.files.fetch_add(1, Ordering::Relaxed);
                        progress.bytes.fetch_add(e.alloc, Ordering::Relaxed);
                    }
                    if e.is_dir && !e.reparse {
                        job_tx.send(Job { id, path: join(&done.path, &e.name) }).unwrap();
                        outstanding += 1;
                    }
                }
            }
            drop(job_tx); // workers see a closed queue and exit
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-scan`
Expected: `test result: ok. 16 passed` (10 merge, 6 walk). The walk tests create junctions with `cmd /C mklink /J`, which needs no privileges.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Add WalkScanner" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Walk example and real-scan baseline

**Files:**
- Create: `crates/plumb-scan/examples/walk.rs`
- Modify: `docs/benchmarks.md`

- [ ] **Step 1: Add the example**

`crates/plumb-scan/examples/walk.rs`:

```rust
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
```

- [ ] **Step 2: Build and run it**

Run: `cargo build -j 4 --release -p plumb-scan --example walk`
Run (a directory of your choice with many files, e.g. your user profile; the thread counts are examples):
`target\release\examples\walk.exe "C:\Users\<you>" 1` then the same with `4`
Expected: one line each, like `4 threads: 405899 dirs, 3373330 files, 589.13 GiB allocated; scan 50.8s, sums 43ms`. The two runs must agree on dirs, files and GiB except for files that changed between runs; more threads must be faster.

- [ ] **Step 3: Record the numbers**

Append to `docs/benchmarks.md` a section `## Real-drive walker scans` with the date, the path scanned, CPU load before the run (`Get-Counter '\Processor(_Total)\% Processor Time'`), whether the cache was cold, and one table row per thread count (threads, dirs, files, GiB, scan time, sum time). The verification run for this plan measured 405,858 dirs and 3,373,078 files (589 GiB) in 124 s on 1 thread and 51 s on 4 threads, with the machine loaded and caches warm from an earlier directory walk; treat those as a rough reference only.

- [ ] **Step 4: Commit**

```bash
git add crates docs
git commit -m "Add walk example and real-scan baseline" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Update the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-10-11-plumb-disk-design.md`

- [ ] **Step 1: Edit the spec**

In the "Scanners (`plumb`)" section: change "Both implement `Scanner::scan(volume, &mut TreeBuilder, &Progress)`" to "Both implement `Scanner::scan(root, &mut TreeBuilder, &Progress)`, where `root` is a drive root or any directory; they live in the `plumb-scan` crate"; replace "Work-stealing queue of directories." with "A coordinator thread owns the `TreeBuilder` and queues directories on a shared job queue; workers only list directories."; add under `WalkScanner`: "Directories count as 0 allocated bytes (the MFT scanner does not count index allocation either)." In "Implementation plans", split item 2 into "2a. `plumb-scan`: `Scanner`, `MftMerger`, `WalkScanner` (`docs/superpowers/plans/2026-10-11-plumb-scan-a.md`)" and "2b. `MftScanner` volume reader, scanner selection and fallback, `scan-compare` (needs an elevated run)".

- [ ] **Step 2: Commit**

```bash
git add docs
git commit -m "Spec: scanner root path, coordinator design, plan 2a/2b split" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (2a scope):**
- Scanner trait with Progress: Task 2. Worker count `min(cores - 4, 8)` and BelowNormal: Task 4.
- Walker: `FileFullDirectoryInfo`, 256 KB buffers, `AllocationSize`, reparse tag from `EaSize`, name-surrogate-only `REPARSE`, `DENIED` on failure, no hard-link detection (documented in the module doc): Task 4. A panic in a scanner thread ends that directory, not the app: Task 4.
- Merge pass: extension records folded, DOS names (dropped by the parser) never become nodes, hard links counted once deterministically, parent resolution after the fact, corrupt count: Task 3. Orphans and parent cycles are additions beyond the spec, needed so `TreeBuilder::finish` cannot fail on damaged volumes.
- Deferred to 2b: volume reader, `MftScanner`, selection and fallback (including the status-bar announcement), `scan-compare`, the `FSCTL_GET_RETRIEVAL_POINTERS` check.
- Deferred to Plan 3: everything UI.

**Placeholder scan:** none; every code block is the file that passed the tests.

**Type consistency:** `Scanner::scan(&self, &Path, &mut TreeBuilder, &Progress)`, `Progress::snapshot`, `MftMerger::{new, add, note_corrupt, finish}`, `MergeStats` fields, `WalkScanner { threads }` and `TreeBuilder::add_flags` are used identically in the tasks and the tests. Test counts: core 38 after Task 1; plumb-scan 10 merge + 6 walk = 16.
