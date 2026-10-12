# plumb live updates, plan 3: UI wiring

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Task 4 step 2 needs the user** (an elevated shell); stop there and ask.

**Goal:** On an elevated NTFS drive the tree on screen follows the change journal: it applies the reader's deltas through the record table, refreshes the rows at most four times a second, shows a live marker, rescans in the background when it has to (restoring selection, scroll and expansion by path), and tells the user when live updates have stopped.

**Architecture:** Everything that can be tested without a window is plain Rust in the `plumb` crate: `live` (record table, delta application with parking and orphans, the 4 Hz pacer, the rescan policy) and `restore` (a path-based snapshot of the view). `App` gains a `LiveStatus`, a `refreshing` flag, `swap_tree` and `refresh_rows`; `Layout` gains an optional banner strip. `window.rs` is the only Win32 glue: the scan thread opens the volume and reads the journal position *before* scanning, the UI thread starts a `JournalReader` when the scan lands, a posted `WM_JOURNAL` message drains it, and one on-demand timer paces the row refreshes.

**Tech Stack:** Rust nightly (edition 2024), `windows` 0.62 (one new feature, `Win32_System_SystemInformation`, for `GetLocalTime`). Crates: `plumb` only.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md` (sections "Applying deltas" and "Failure, rescan and compaction"). Plans 1 and 2 are done; this plan consumes `Tree::{add_child, set_size, rename, remove, reparent, dead, take_touched}`, `RowModel::refresh`, `JournalReader`, `Delta`, `FileState` and `MftReport::records`.

## Global Constraints

- Live updates apply to **elevated NTFS drives scanned by the MFT scanner** only. Walker-scanned and non-elevated drives behave exactly as in 0.1.0 (no marker, no banner).
- Deltas are applied on the UI thread with a budget of **about 5 ms** per message; repaints and row refreshes caused by live updates are capped at **4 Hz**.
- `refresh` runs **at most once per repaint tick** and **not at all** when no delta changed the tree.
- An upsert whose parent is unknown is parked and retried; after **2 settle passes** it goes under `<orphans>`.
- Automatic rescans: **at most one per volume per 10 minutes**, and **none within 30 s of user input**. A wrapped journal that cannot rescan falls back to the stale banner.
- Stale banner wording: `live updates stopped: <reason>; the tree may be out of date`, with a **Rescan** button.
- Compaction rescan when dead nodes exceed **25% of live nodes or pass 1M** (this plan adds a floor of 10 000 dead nodes, see below).
- The old tree stays on screen with `refreshing…` in the status bar while a rescan runs; the swap restores selection, scroll and expansion by path.
- No compatibility shims. Windows source files in the working copy are CRLF (`core.autocrlf=true`); the patches below are LF and are applied with `git apply --ignore-whitespace`.
- If another `cargo` is running, use a separate `CARGO_TARGET_DIR` (a second build blocks on the lock and looks like a stall).

## Decisions made while building (where this plan departs from or fills in the spec)

| Topic | Decision | Why |
|---|---|---|
| "Each frame" | There is no per-frame loop: the UI repaints on demand. Deltas are applied in budgeted slices (`WM_JOURNAL` re-posts itself while the queue is not empty, so input messages interleave), and one timer (`TIMER_LIVE`, 250 ms) refreshes the rows. The timer exists only while there is something to do. | An always-on timer would wake an idle app four times a second. |
| Slow refreshes | After a refresh that took `t`, the next is no sooner than `max(250 ms, 5t)`. | `refresh` after one add in a 1M-child folder measured 244 ms (re-sort); this keeps refreshing under about a sixth of the time. |
| Parking | `settle` runs when the queue empties. It retries the parked upserts until a pass places none (one placement can unblock another, e.g. a new folder chain arriving child first), then ages the rest; a second failed pass orphans them. | A single pass orphaned deep new chains that arrived in the wrong order. |
| Folder swaps | An upsert that would put a folder inside itself is refused by `reparent`; it is parked, not dropped. | Two folders that swap places produce two events in arbitrary order; dropping the first left the tree wrong. |
| Compaction floor | The 25% rule also needs at least 10 000 dead nodes. | Without a floor a tiny tree with one deleted file (1 dead of 3 live) would rescan. |
| Restored expansion | Only folders that are expanded **and on screen** are restored. | The row model keeps the expanded bit of folders under a collapsed parent, but a snapshot is taken from the visible rows. Cheap to extend if it matters. |
| Live marker time | The marker shows the clock time of the last **row refresh** that followed a change, not of the journal record. | The journal's own timestamps are not read; the difference is under a second. |
| Hard links | Only the first name of a hard-linked file is tracked (inherited from plan 2). A live upsert of a multi-link file uses the state's size and sets `HARDLINK`. | Matches the scanner's rule closely enough; exact parity would need the whole name list. |
| Elevated end-to-end check | `plumb --selftest-live` (administrator only): scans the temp directory's drive in a hidden window, then creates, grows, renames and deletes a file and waits for the tree to follow each step. | The glue (posted message, timer, refresh) has no other test; `live` and `restore` are covered by unit tests. |

## File structure

| File | Responsibility |
|---|---|
| `crates/plumb/src/live.rs` (new) | `Live` (record table, `apply`, parking, orphans), `needs_compaction`, `Pacer`, `RescanPolicy`. No Win32. |
| `crates/plumb/src/live/tests.rs` (new) | 16 tests, including a seeded random oracle. |
| `crates/plumb/src/restore.rs` (new) | `Snapshot` (take and apply by path), `resolve`. No Win32. |
| `crates/plumb/src/app.rs` | `LiveStatus`, `refreshing`, `swap_tree`, `refresh_rows`, `set_live`, `live_changed`, `Effect::Rescan`, status line, banner text. |
| `crates/plumb/src/layout.rs` | Banner strip, Rescan button, `Hit::Rescan`; the header and rows shift down by the banner height. |
| `crates/plumb/src/draw.rs` | Draws the banner. |
| `crates/plumb/src/window.rs` | Opens the journal before the scan, `Watch`, `WM_JOURNAL`, `TIMER_LIVE`, background rescan, `selftest_live`. |
| `crates/plumb/src/main.rs`, `Cargo.toml`, `tests/render.rs` | `--selftest-live` flag, one `windows` feature, `banner: None` in the render scene. |

---

### Task 1: The `live` module

**Files:**
- Create: `crates/plumb/src/live.rs`, `crates/plumb/src/live/tests.rs`
- Modify: `crates/plumb/src/lib.rs`

**Interfaces:**
- Consumes: `plumb_scan::journal::Delta`, `plumb_scan::journal::state::FileState`, `Tree`, `NodeId`, `NodeFlags`.
- Produces: `Live::new(records: Vec<NodeId>) -> Live`, `Live::apply(&mut self, &mut Tree, &mut VecDeque<Delta>, deadline: Instant) -> usize` (tree changes made), `Live::pending(&self) -> bool`, `needs_compaction(&Tree) -> bool`, `Pacer::{new, due, done}`, `RescanPolicy::allow(drive: char, now: Instant, last_input: Instant) -> bool`, `REPAINT: Duration`.

- [ ] **Step 1: Create the module and its tests**

<!-- file: crates/plumb/src/live.rs -->
```rust
//! Live updates, the part that needs no window: applying journal deltas to the tree through the
//! record table, pacing the row refreshes, and deciding when an automatic rescan is allowed.
use plumb_core::{NodeFlags, NodeId, Tree};
use plumb_scan::journal::Delta;
use plumb_scan::journal::state::FileState;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// NTFS keeps the volume root in file record 5.
const ROOT_RECORD: u64 = 5;
/// Settle passes an upsert may wait for its parent before it goes under `<orphans>`.
const ORPHAN_AFTER: u8 = 2;
/// Deltas applied between two looks at the clock.
const CLOCK_EVERY: usize = 256;
/// Rows are refreshed at most this often (4 Hz).
pub const REPAINT: Duration = Duration::from_millis(250);
/// A rescan for compaction needs at least this many dead nodes, whatever the ratio.
const MIN_DEAD: usize = 10_000;
const MAX_DEAD: usize = 1_000_000;
/// At most one automatic rescan per volume in this long ...
const RESCAN_GAP: Duration = Duration::from_secs(600);
/// ... and none this soon after the user last touched the window.
const INPUT_IDLE: Duration = Duration::from_secs(30);

/// The record-number-to-node table plus upserts waiting for their parent.
pub struct Live {
    records: Vec<NodeId>,
    /// Upserts whose parent is unknown, and how many settle passes they have waited.
    parked: Vec<(FileState, u8)>,
    orphans: NodeId,
    changes: usize,
}

impl Live {
    /// `records` is `MftReport::records` of the scan that built the tree.
    pub fn new(records: Vec<NodeId>) -> Self {
        Self { records, parked: Vec::new(), orphans: NodeId::NONE, changes: 0 }
    }

    /// True while an upsert is waiting for its parent, so the caller keeps ticking.
    pub fn pending(&self) -> bool {
        !self.parked.is_empty()
    }

    /// Applies queued deltas until the queue is empty or `deadline` passes. When it empties,
    /// parked upserts get another try. Returns how many tree changes were made (zero means the
    /// rows need no refresh).
    pub fn apply(&mut self, tree: &mut Tree, queue: &mut VecDeque<Delta>, deadline: Instant) -> usize {
        let before = self.changes;
        let mut n = 0usize;
        while let Some(d) = queue.pop_front() {
            match d {
                Delta::Upsert(s) => self.upsert_or_park(tree, s),
                Delta::Remove { record } => self.remove(tree, record),
            }
            n += 1;
            if n.is_multiple_of(CLOCK_EVERY) && Instant::now() >= deadline {
                break;
            }
        }
        if queue.is_empty() {
            self.settle(tree);
        }
        self.changes - before
    }

    fn node(&self, tree: &Tree, record: u64) -> Option<NodeId> {
        let id = *self.records.get(usize::try_from(record).ok()?)?;
        (!id.is_none() && !tree.flags[id].contains(NodeFlags::DEAD)).then_some(id)
    }

    fn upsert_or_park(&mut self, tree: &mut Tree, s: FileState) {
        if s.record == ROOT_RECORD {
            return;
        }
        match self.node(tree, s.parent) {
            Some(parent) if self.upsert(tree, &s, parent) => {}
            _ => self.parked.push((s, 0)),
        }
    }

    /// Puts `s` in the tree under `parent`: adds it, or fixes whichever of kind, name, place and
    /// size differ. False if it could not be placed yet (moving it would put it inside itself,
    /// which happens when two folders swap places and their events arrive in the wrong order).
    fn upsert(&mut self, tree: &mut Tree, s: &FileState, parent: NodeId) -> bool {
        let mut flags = NodeFlags::empty();
        flags.set(NodeFlags::DIR, s.is_dir);
        flags.set(NodeFlags::REPARSE, s.reparse);
        flags.set(NodeFlags::HARDLINK, s.links > 1);
        let size = if s.is_dir { 0 } else { s.size };
        let record = s.record as usize;
        if let Some(id) = self.node(tree, s.record) {
            if tree.flags[id].contains(NodeFlags::DIR) == s.is_dir {
                if tree.parent[id] != parent {
                    if !tree.reparent(id, parent) {
                        return false;
                    }
                    self.changes += 1;
                }
                if tree.name(id) != s.name.as_slice() {
                    tree.rename(id, &s.name);
                    self.changes += 1;
                }
                if tree.size[id] != size {
                    tree.set_size(id, size);
                    self.changes += 1;
                }
                return true;
            }
            // A file became a folder or the reverse under the same record number.
            tree.remove(id);
            self.changes += 1;
        }
        if self.records.len() <= record {
            self.records.resize(record + 1, NodeId::NONE);
        }
        self.records[record] = tree.add_child(parent, &s.name, size, flags);
        self.changes += 1;
        true
    }

    fn remove(&mut self, tree: &mut Tree, record: u64) {
        if let Some(id) = self.node(tree, record) {
            if tree.remove(id) {
                self.changes += 1;
            }
            self.records[record as usize] = NodeId::NONE;
        }
    }

    /// Retries the parked upserts until a pass places none (one placement can unblock another).
    /// Those still unplaced wait a settle pass, or after `ORPHAN_AFTER` passes go under `<orphans>`.
    fn settle(&mut self, tree: &mut Tree) {
        let mut waiting = std::mem::take(&mut self.parked);
        loop {
            let mut placed = false;
            let mut still = Vec::new();
            for (s, age) in waiting {
                match self.node(tree, s.parent) {
                    Some(p) if self.upsert(tree, &s, p) => placed = true,
                    _ => still.push((s, age)),
                }
            }
            waiting = still;
            if !placed {
                break;
            }
        }
        for (s, age) in waiting {
            if age + 1 < ORPHAN_AFTER {
                self.parked.push((s, age + 1));
            } else if let Some(o) = self.orphans(tree) {
                self.upsert(tree, &s, o);
            }
        }
    }

    /// The `<orphans>` folder under the root: the one the scan made, or a new one.
    fn orphans(&mut self, tree: &mut Tree) -> Option<NodeId> {
        if !self.orphans.is_none() && !tree.flags[self.orphans].contains(NodeFlags::DEAD) {
            return Some(self.orphans);
        }
        let root = self.node(tree, ROOT_RECORD)?;
        let name: Vec<u16> = "<orphans>".encode_utf16().collect();
        let found = tree.children(root).find(|&c| tree.name(c) == name.as_slice());
        let id = found.unwrap_or_else(|| {
            self.changes += 1;
            tree.add_child(root, &name, 0, NodeFlags::DIR)
        });
        self.orphans = id;
        Some(id)
    }
}

/// True when dead nodes are worth a rescan: over a quarter of the live ones (and at least
/// `MIN_DEAD`), or past `MAX_DEAD`.
pub fn needs_compaction(tree: &Tree) -> bool {
    let dead = tree.dead();
    dead > MAX_DEAD || (dead >= MIN_DEAD && dead * 4 > tree.len() - dead)
}

/// Keeps row refreshes at or under 4 Hz, and backs off when a refresh is slow so refreshing
/// never takes more than about a sixth of the time.
pub struct Pacer {
    next: Instant,
}

impl Pacer {
    pub fn new(now: Instant) -> Self {
        Self { next: now }
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Call after a refresh that finished at `now` and took `took`.
    pub fn done(&mut self, now: Instant, took: Duration) {
        self.next = now + REPAINT.max(took * 5);
    }
}

/// When an automatic rescan may start.
#[derive(Default)]
pub struct RescanPolicy {
    last: HashMap<char, Instant>,
}

impl RescanPolicy {
    /// True (and recorded) if `drive` has not been rescanned automatically in the last ten
    /// minutes and the user has not touched the window in the last 30 seconds.
    pub fn allow(&mut self, drive: char, now: Instant, last_input: Instant) -> bool {
        let recent = self.last.get(&drive).is_some_and(|&t| now.duration_since(t) < RESCAN_GAP);
        if recent || now.duration_since(last_input) < INPUT_IDLE {
            return false;
        }
        self.last.insert(drive, now);
        true
    }
}

#[cfg(test)]
mod tests;
```

<!-- file: crates/plumb/src/live/tests.rs -->
```rust
use super::*;
use plumb_core::TreeBuilder;
use std::collections::BTreeMap;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn st(record: u64, parent: u64, name: &str, size: u64, is_dir: bool) -> FileState {
    FileState { record, sequence: 1, parent, name: u(name), size, is_dir, reparse: false, links: 1 }
}

fn up(record: u64, parent: u64, name: &str, size: u64) -> Delta {
    Delta::Upsert(st(record, parent, name, size, false))
}

fn dir(record: u64, parent: u64, name: &str) -> Delta {
    Delta::Upsert(st(record, parent, name, 0, true))
}

/// C: (record 5) with docs (30) holding a.txt (31, 100) and b.txt (33, 50), and tmp (32).
fn fixture() -> (Tree, Live) {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
    let a = b.push_str(docs, "a.txt", 100, NodeFlags::empty());
    let tmp = b.push_str(root, "tmp", 0, NodeFlags::DIR);
    let c = b.push_str(docs, "b.txt", 50, NodeFlags::empty());
    let mut records = vec![NodeId::NONE; 40];
    for (r, n) in [(5, root), (30, docs), (31, a), (32, tmp), (33, c)] {
        records[r] = n;
    }
    (b.finish().unwrap(), Live::new(records))
}

fn run(live: &mut Live, tree: &mut Tree, deltas: Vec<Delta>) -> usize {
    let mut q: VecDeque<Delta> = deltas.into();
    live.apply(tree, &mut q, Instant::now() + Duration::from_secs(10))
}

/// Path -> (size, is_dir) for every live node.
fn snapshot(t: &Tree) -> BTreeMap<String, (u64, bool)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<NodeId> = t.roots.clone();
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.subtree_size[n], t.flags[n].contains(NodeFlags::DIR)));
        stack.extend(t.children(n));
    }
    out
}

fn size_of(t: &Tree, path: &str) -> Option<u64> {
    snapshot(t).get(path).map(|v| v.0)
}

#[test]
fn a_new_file_appears_and_its_size_reaches_every_ancestor() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![up(34, 32, "new.bin", 70)]), 1);
    assert_eq!(size_of(&t, "C:\\tmp\\new.bin"), Some(70));
    assert_eq!(size_of(&t, "C:\\tmp"), Some(70));
    assert_eq!(size_of(&t, "C:"), Some(220));
}

#[test]
fn growing_renaming_and_moving_in_one_upsert_all_apply() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![up(31, 32, "moved.txt", 400)]);
    assert_eq!(size_of(&t, "C:\\tmp\\moved.txt"), Some(400));
    assert_eq!(size_of(&t, "C:\\docs\\a.txt"), None);
    assert_eq!(size_of(&t, "C:\\docs"), Some(50));
    assert_eq!(size_of(&t, "C:"), Some(450));
}

#[test]
fn a_repeated_upsert_changes_nothing() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![up(31, 30, "a.txt", 100), dir(30, 5, "docs")]), 0);
    assert!(t.take_touched().is_empty());
}

#[test]
fn removing_a_folder_drops_its_files_and_the_record_can_be_reused() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![Delta::Remove { record: 30 }, Delta::Remove { record: 31 }, Delta::Remove { record: 33 }]);
    assert_eq!(size_of(&t, "C:"), Some(0));
    assert_eq!(size_of(&t, "C:\\docs"), None);
    run(&mut l, &mut t, vec![up(31, 32, "again.txt", 9)]);
    assert_eq!(size_of(&t, "C:\\tmp\\again.txt"), Some(9));
    // Removing what is not known, or already gone, is ignored.
    assert_eq!(run(&mut l, &mut t, vec![Delta::Remove { record: 30 }, Delta::Remove { record: 9999 }]), 0);
}

#[test]
fn a_child_that_arrives_before_its_new_parent_is_placed_in_the_same_call() {
    let (mut t, mut l) = fixture();
    let deltas = vec![up(52, 51, "deep.txt", 5), dir(51, 50, "two"), dir(50, 5, "one")];
    run(&mut l, &mut t, deltas);
    assert_eq!(size_of(&t, "C:\\one\\two\\deep.txt"), Some(5));
    assert!(!l.pending());
    assert_eq!(size_of(&t, "C:\\<orphans>"), None);
}

#[test]
fn an_unknown_parent_waits_one_pass_and_the_second_orphans_it() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![up(60, 99, "lost.txt", 4)]);
    assert!(l.pending());
    assert_eq!(size_of(&t, "C:"), Some(150));
    // The parent turns up in the next batch.
    run(&mut l, &mut t, vec![dir(99, 5, "late")]);
    assert_eq!(size_of(&t, "C:\\late\\lost.txt"), Some(4));
    assert!(!l.pending());

    run(&mut l, &mut t, vec![up(61, 98, "never.txt", 6)]);
    assert!(l.pending());
    run(&mut l, &mut t, vec![]);
    assert!(!l.pending());
    assert_eq!(size_of(&t, "C:\\<orphans>\\never.txt"), Some(6));
    // A later orphan reuses the same folder.
    run(&mut l, &mut t, vec![up(62, 97, "also.txt", 1)]);
    run(&mut l, &mut t, vec![]);
    assert_eq!(size_of(&t, "C:\\<orphans>"), Some(7));
}

#[test]
fn two_folders_swapping_places_end_up_right_whatever_the_order() {
    // Before: C:\docs\sub. After: C:\sub\docs. Both events arrive, in the worst order.
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
    let sub = b.push_str(docs, "sub", 0, NodeFlags::DIR);
    let mut records = vec![NodeId::NONE; 40];
    for (r, n) in [(5, root), (30, docs), (31, sub)] {
        records[r] = n;
    }
    let (mut t, mut l) = (b.finish().unwrap(), Live::new(records));
    run(&mut l, &mut t, vec![dir(30, 31, "docs"), dir(31, 5, "sub")]);
    let s = snapshot(&t);
    assert!(s.contains_key("C:\\sub\\docs"), "{:?}", s.keys().collect::<Vec<_>>());
    assert!(!s.contains_key("C:\\docs"));
}

#[test]
fn a_file_that_becomes_a_folder_is_replaced() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![dir(31, 30, "a.txt")]);
    let s = snapshot(&t);
    assert_eq!(s["C:\\docs\\a.txt"], (0, true));
    assert_eq!(size_of(&t, "C:"), Some(50));
}

#[test]
fn the_volume_root_record_is_never_touched() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![dir(5, 5, "C:")]), 0);
    assert!(!l.pending());
}

#[test]
fn records_beyond_the_table_grow_it() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![dir(100_000, 5, "far"), up(100_001, 100_000, "f", 3)]);
    assert_eq!(size_of(&t, "C:\\far\\f"), Some(3));
}

#[test]
fn an_expired_deadline_stops_after_one_clock_check_and_the_rest_waits() {
    let (mut t, mut l) = fixture();
    let mut q: VecDeque<Delta> = (0..1000).map(|i| up(1000 + i, 5, &format!("f{i}"), 1)).collect();
    let n = l.apply(&mut t, &mut q, Instant::now() - Duration::from_secs(1));
    assert_eq!(n, CLOCK_EVERY);
    assert_eq!(q.len(), 1000 - CLOCK_EVERY);
    let n = l.apply(&mut t, &mut q, Instant::now() + Duration::from_secs(10));
    assert_eq!(n, 1000 - CLOCK_EVERY);
    assert_eq!(size_of(&t, "C:"), Some(1150));
}

#[test]
fn compaction_needs_both_a_ratio_and_a_floor() {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "R", 0, NodeFlags::DIR);
    let ids: Vec<_> = (0..100_000).map(|i| b.push_str(root, &format!("f{i}"), 1, NodeFlags::empty())).collect();
    let mut t = b.finish().unwrap();
    for &i in &ids[..9_999] {
        t.remove(i);
    }
    assert!(!needs_compaction(&t), "9999 dead is under the 10000 floor");
    for &i in &ids[9_999..20_000] {
        t.remove(i);
    }
    assert!(!needs_compaction(&t), "20000 dead of 80001 live is exactly a quarter, not over");
    t.remove(ids[20_000]);
    assert!(needs_compaction(&t), "20001 dead of 80000 live is over a quarter");
}

#[test]
fn a_tiny_tree_with_one_dead_node_does_not_ask_for_a_rescan() {
    let (mut t, _) = fixture();
    t.remove(NodeId(2));
    assert!(!needs_compaction(&t));
}

#[test]
fn the_pacer_allows_four_refreshes_a_second_and_backs_off_when_they_are_slow() {
    let t0 = Instant::now();
    let mut p = Pacer::new(t0);
    assert!(p.due(t0));
    p.done(t0, Duration::from_millis(10));
    assert!(!p.due(t0 + Duration::from_millis(249)));
    assert!(p.due(t0 + Duration::from_millis(250)));
    p.done(t0, Duration::from_millis(244));
    assert!(!p.due(t0 + Duration::from_millis(1219)));
    assert!(p.due(t0 + Duration::from_millis(1220)));
}

#[test]
fn rescans_are_rate_limited_per_volume_and_wait_for_the_user_to_go_quiet() {
    let t0 = Instant::now();
    let min = |m: u64| Duration::from_secs(60 * m);
    let mut p = RescanPolicy::default();
    // The user was active 10 s ago: not yet.
    assert!(!p.allow('C', t0 + min(1), t0 + min(1) - Duration::from_secs(10)));
    assert!(p.allow('C', t0 + min(1), t0));
    assert!(!p.allow('C', t0 + min(5), t0), "second rescan within ten minutes");
    assert!(p.allow('D', t0 + min(5), t0), "another volume has its own clock");
    assert!(p.allow('C', t0 + min(11), t0));
}

/// A tiny xorshift so the oracle below is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Clone)]
struct Truth {
    parent: u64,
    name: String,
    size: u64,
    is_dir: bool,
}

fn truth_snapshot(t: &BTreeMap<u64, Truth>) -> BTreeMap<String, (u64, bool)> {
    fn path(t: &BTreeMap<u64, Truth>, r: u64) -> String {
        if r == 5 { "C:".into() } else { format!("{}\\{}", path(t, t[&r].parent), t[&r].name) }
    }
    let mut out = BTreeMap::new();
    for (&r, n) in t {
        let size = if n.is_dir {
            t.iter().filter(|(c, v)| !v.is_dir && is_under(t, **c, r)).map(|(_, v)| v.size).sum()
        } else {
            n.size
        };
        out.insert(path(t, r), (size, n.is_dir));
    }
    out
}

fn is_under(t: &BTreeMap<u64, Truth>, mut r: u64, anc: u64) -> bool {
    while r != 5 {
        r = t[&r].parent;
        if r == anc {
            return true;
        }
    }
    anc == 5
}

#[test]
fn random_batches_in_any_order_match_a_tree_built_from_the_final_state() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut truth: BTreeMap<u64, Truth> = BTreeMap::new();
        truth.insert(5, Truth { parent: 5, name: "C:".into(), size: 0, is_dir: true });
        let mut next = 30u64;
        // Start from an empty drive, then grow it through batches like a busy volume.
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let mut records = vec![NodeId::NONE; 8];
        records[5] = root;
        let (mut tree, mut live) = (b.finish().unwrap(), Live::new(records));

        for _round in 0..30 {
            let mut touched: Vec<u64> = Vec::new();
            for _ in 0..1 + rng.below(12) {
                let keys: Vec<u64> = truth.keys().copied().filter(|&k| k != 5).collect();
                let dirs: Vec<u64> = truth.iter().filter(|(_, v)| v.is_dir).map(|(&k, _)| k).collect();
                match rng.below(6) {
                    0 | 1 => {
                        let parent = dirs[rng.below(dirs.len())];
                        let is_dir = rng.below(3) == 0;
                        truth.insert(next, Truth { parent, name: format!("n{next}"), size: if is_dir { 0 } else { 1 + rng.below(500) as u64 }, is_dir });
                        touched.push(next);
                        next += 1;
                    }
                    2 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        if !truth[&k].is_dir {
                            truth.get_mut(&k).unwrap().size = rng.below(900) as u64;
                            touched.push(k);
                        }
                    }
                    3 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        truth.get_mut(&k).unwrap().name = format!("r{}", rng.next() % 100_000);
                        touched.push(k);
                    }
                    4 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        let to = dirs[rng.below(dirs.len())];
                        if to != k && !is_under(&truth, to, k) {
                            truth.get_mut(&k).unwrap().parent = to;
                            touched.push(k);
                        }
                    }
                    5 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        let gone: Vec<u64> = truth.keys().copied().filter(|&c| c == k || (c != 5 && is_under(&truth, c, k))).collect();
                        for c in gone {
                            truth.remove(&c);
                            touched.push(c);
                        }
                    }
                    _ => {}
                }
            }
            // What the reader reports: each touched record once, as it is now, in no useful order.
            touched.sort_unstable();
            touched.dedup();
            let mut deltas: Vec<Delta> = touched
                .iter()
                .map(|&r| match truth.get(&r) {
                    Some(n) => Delta::Upsert(st(r, n.parent, &n.name, n.size, n.is_dir)),
                    None => Delta::Remove { record: r },
                })
                .collect();
            for i in (1..deltas.len()).rev() {
                deltas.swap(i, rng.below(i + 1));
            }
            run(&mut live, &mut tree, deltas);
            run(&mut live, &mut tree, vec![]);
            assert!(!live.pending(), "seed {seed}");
            assert_eq!(snapshot(&tree), truth_snapshot(&truth), "seed {seed}");
        }
    }
}
```

- [ ] **Step 2: Register the module in `lib.rs`**

<!-- patch -->
```diff
--- a/crates/plumb/src/lib.rs
+++ b/crates/plumb/src/lib.rs
@@ -1,4 +1,5 @@
 pub mod app;
 pub mod fmt;
 pub mod layout;
+pub mod live;
 #[cfg(windows)]
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p plumb --lib live`
Expected: `test result: ok. 16 passed`.

- [ ] **Step 4: Mutation check (do not commit)**

In `Live::upsert`, change the `return false;` after `if !tree.reparent(id, parent) {` to `return true;`. Run the same command. Expected: `two_folders_swapping_places_end_up_right_whatever_the_order` fails. Undo the edit by hand (the file is not committed yet, so git cannot restore it) and rerun: 16 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/plumb/src/live.rs crates/plumb/src/live crates/plumb/src/lib.rs
git commit -m "Add the live module: record table, delta application, pacing and rescan policy"
```

### Task 2: The `restore` module

**Files:**
- Create: `crates/plumb/src/restore.rs`
- Modify: `crates/plumb/src/lib.rs`

**Interfaces:**
- Produces: `Snapshot::take(&Tree, &RowModel, scroll: usize) -> Snapshot`, `Snapshot::apply(&self, &Tree, SortKey, desc: bool, visible: usize) -> (RowModel, usize)` (the new model and its scroll), `resolve(&Tree, path: &str) -> Option<NodeId>` (crate-visible).

- [ ] **Step 1: Create the module**

<!-- file: crates/plumb/src/restore.rs -->
```rust
//! Carries the view over to a rescanned tree: what is expanded, what is selected and where it
//! sits on screen, all by path, since node ids mean nothing in the new tree.
use plumb_core::rows::{RowModel, SortKey};
use plumb_core::{NodeId, Tree};

pub struct Snapshot {
    /// Paths of the expanded folders that are on screen, shallowest first.
    expanded: Vec<String>,
    selected: Option<String>,
    /// Screen row the selection was on.
    screen_row: usize,
    scroll: usize,
}

impl Snapshot {
    pub fn take(tree: &Tree, model: &RowModel, scroll: usize) -> Self {
        let mut expanded: Vec<String> =
            model.rows().iter().filter(|r| model.is_expanded(r.id)).map(|r| tree.path(r.id)).collect();
        expanded.sort_by_key(|p| p.matches('\\').count());
        let selected = model.selected().map(|s| tree.path(s));
        let screen_row = model.selected_index().map_or(0, |i| i.saturating_sub(scroll));
        Self { expanded, selected, screen_row, scroll }
    }

    /// A model for `tree` with the same sort, the same folders expanded and the same row selected
    /// (or its nearest surviving ancestor), plus the scroll that keeps it on the same screen row.
    /// Paths that no longer exist are dropped.
    pub fn apply(&self, tree: &Tree, key: SortKey, desc: bool, visible: usize) -> (RowModel, usize) {
        let mut model = RowModel::new(tree);
        model.set_sort(tree, key, desc);
        for p in &self.expanded {
            if let Some(id) = resolve(tree, p) {
                model.expand(tree, id);
            }
        }
        let found = self.selected.as_deref().and_then(|mut p| loop {
            if let Some(id) = resolve(tree, p) {
                break Some(id);
            }
            p = &p[..p.rfind('\\')?];
        });
        let max = model.rows().len().saturating_sub(visible);
        let scroll = match found {
            Some(id) => {
                model.select(id);
                model.selected_index().map_or(0, |i| i.saturating_sub(self.screen_row))
            }
            None => self.scroll,
        };
        (model, scroll.min(max))
    }
}

/// The node at `path` ("C:\dir\file"), matching names exactly.
pub(crate) fn resolve(tree: &Tree, path: &str) -> Option<NodeId> {
    let mut parts = path.split('\\');
    let first = parts.next()?;
    let mut cur = *tree.roots.iter().find(|&&r| tree.name_string(r) == first)?;
    for part in parts {
        cur = tree.children(cur).find(|&c| tree.name_string(c) == part)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::{NodeFlags, TreeBuilder};

    /// C:\{a\{x, y}, b\{z}, big} with a few filler files in a so scrolling means something.
    fn tree(drop_b: bool) -> Tree {
        let mut t = TreeBuilder::new();
        let root = t.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let a = t.push_str(root, "a", 0, NodeFlags::DIR);
        for i in 0..30 {
            t.push_str(a, &format!("f{i:02}"), 10 + i, NodeFlags::empty());
        }
        t.push_str(a, "x", 5, NodeFlags::empty());
        if !drop_b {
            let b = t.push_str(root, "b", 0, NodeFlags::DIR);
            t.push_str(b, "z", 7, NodeFlags::empty());
        }
        t.push_str(root, "big", 1000, NodeFlags::empty());
        t.finish().unwrap()
    }

    fn state(t: &Tree) -> (RowModel, usize) {
        let mut m = RowModel::new(t);
        m.set_sort(t, SortKey::Name, false);
        let root = m.rows()[0].id;
        m.expand(t, root);
        let a = resolve(t, "C:\\a").unwrap();
        m.expand(t, a);
        m.select(resolve(t, "C:\\a\\f10").unwrap());
        let scroll = m.selected_index().unwrap() - 4;
        (m, scroll)
    }

    #[test]
    fn paths_resolve_exactly() {
        let t = tree(false);
        assert_eq!(resolve(&t, "C:"), Some(NodeId(0)));
        assert_eq!(t.path(resolve(&t, "C:\\b\\z").unwrap()), "C:\\b\\z");
        assert_eq!(resolve(&t, "C:\\B"), None);
        assert_eq!(resolve(&t, "D:"), None);
    }

    #[test]
    fn expansion_selection_and_screen_row_survive_a_rescan() {
        let old = tree(false);
        let (m, scroll) = state(&old);
        let snap = Snapshot::take(&old, &m, scroll);
        let new = tree(false);
        let (m2, s2) = snap.apply(&new, SortKey::Name, false, 20);
        assert_eq!(new.path(m2.selected().unwrap()), "C:\\a\\f10");
        assert_eq!(m2.selected_index().unwrap() - s2, 4);
        assert!(m2.is_expanded(resolve(&new, "C:\\a").unwrap()));
        assert!(!m2.is_expanded(resolve(&new, "C:\\b").unwrap()));
    }

    #[test]
    fn a_selection_that_is_gone_falls_back_to_its_nearest_surviving_ancestor() {
        let old = tree(false);
        let mut m = RowModel::new(&old);
        let root = m.rows()[0].id;
        m.expand(&old, root);
        let b = resolve(&old, "C:\\b").unwrap();
        m.expand(&old, b);
        m.select(resolve(&old, "C:\\b\\z").unwrap());
        let snap = Snapshot::take(&old, &m, 0);
        let new = tree(true);
        let (m2, _) = snap.apply(&new, SortKey::Name, false, 20);
        assert_eq!(new.path(m2.selected().unwrap()), "C:");
    }

    #[test]
    fn scroll_is_clamped_when_the_new_tree_is_shorter() {
        let old = tree(false);
        let (m, scroll) = state(&old);
        let mut snap = Snapshot::take(&old, &m, scroll);
        snap.selected = None;
        snap.expanded.clear();
        snap.scroll = 500;
        let (m2, s2) = snap.apply(&old, SortKey::Name, false, 20);
        assert_eq!(s2, m2.rows().len().saturating_sub(20));
    }
}
```

- [ ] **Step 2: Register it in `lib.rs`**

<!-- patch -->
```diff
--- a/crates/plumb/src/lib.rs
+++ b/crates/plumb/src/lib.rs
@@ -2,4 +2,5 @@
 pub mod fmt;
 pub mod layout;
 pub mod live;
+pub mod restore;
 #[cfg(windows)]
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p plumb --lib restore`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/plumb/src/restore.rs crates/plumb/src/lib.rs
git commit -m "Add the restore module: carry expansion, selection and scroll across a rescan by path"
```

### Task 3: App state, the banner and the window glue

One task (and one commit) because `Effect::Rescan` must be handled in `window.rs` for the crate to compile on Windows.

**Files:**
- Modify: `crates/plumb/Cargo.toml`, `crates/plumb/src/app.rs`, `crates/plumb/src/draw.rs`, `crates/plumb/src/layout.rs`, `crates/plumb/src/main.rs`, `crates/plumb/src/window.rs`, `crates/plumb/tests/render.rs`

**Interfaces:**
- Consumes: `Live`, `Pacer`, `RescanPolicy`, `needs_compaction`, `REPAINT` (Task 1), `Snapshot` (Task 2), `JournalReader`, `JournalConfig`, `JournalInfo`, `Event`, `Stop`, `Delta`, `Volume`.
- Produces: `LiveStatus::{Off, On { last }, Stale(String)}`, `App::{live, refreshing, set_live, live_changed, swap_tree, refresh_rows}`, `Effect::Rescan(usize)`, `Layout::{banner, with_banner, banner_strip, rescan_button}`, `Hit::Rescan`, `Scene::banner`, `window::selftest_live`.

- [ ] **Step 1: Apply the patch**

What it does, by file:
- `layout.rs`: `Layout.banner`, `with_banner`, `banner_strip`, `rescan_button`, `Hit::Rescan`; `header()` and `rows_area()` start below the banner when there is one. One new test.
- `draw.rs` and `tests/render.rs`: `Scene.banner: Option<&str>`; the strip is drawn on the panel colour with the text on the left and a Rescan button on the right.
- `app.rs`: the state listed under Produces, the status line (`live`, `live, last change HH:MM:SS`, `live updates stopped`, `refreshing…`), `banner()`, the click on Rescan (ignored while a scan or rescan runs), and four tests.
- `window.rs`: `open_journal` runs in the scan thread before `scan_auto` (elevated NTFS drive roots only) and the volume plus journal position travel back with the tree in `Scanned`; `scan_done` swaps or installs the tree and starts the `Watch`; `journal_event` / `apply_live` / `live_tick` / `journal_stopped` / `auto_rescan` as described under Decisions; `drop_watch` joins the reader off the UI thread; user input stamps `last_input`; `selftest_live`.
- `main.rs`: `--selftest-live`. `Cargo.toml`: `Win32_System_SystemInformation`.

<!-- patch -->
```diff
diff --git a/crates/plumb/Cargo.toml b/crates/plumb/Cargo.toml
index bce65d2..8dff519 100644
--- a/crates/plumb/Cargo.toml
+++ b/crates/plumb/Cargo.toml
@@ -24,6 +24,7 @@ windows = { workspace = true, features = [
     "Win32_System_LibraryLoader",
     "Win32_System_Memory",
     "Win32_System_Registry",
+    "Win32_System_SystemInformation",
     "Win32_System_WindowsProgramming",
     "Win32_UI_HiDpi",
     "Win32_UI_Input_KeyboardAndMouse",
diff --git a/crates/plumb/src/app.rs b/crates/plumb/src/app.rs
index c55d25d..3ea0ca0 100644
--- a/crates/plumb/src/app.rs
+++ b/crates/plumb/src/app.rs
@@ -3,6 +3,7 @@
 use crate::draw::{Chip, Scene, Theme, View};
 use crate::fmt;
 use crate::layout::{Hit, Layout};
+use crate::restore::Snapshot;
 use plumb_core::rows::{Key, RowModel, SortKey};
 use plumb_core::{NodeId, Tree};
 
@@ -19,6 +20,16 @@ pub enum Scan {
     Failed(String),
 }
 
+/// Whether the tree is being kept up to date from the change journal.
+#[derive(Clone, Debug, PartialEq)]
+pub enum LiveStatus {
+    Off,
+    /// `last` is the clock time of the last applied change.
+    On { last: Option<String> },
+    /// Live updates ended; the tree may be out of date.
+    Stale(String),
+}
+
 pub enum Input {
     Click(f32, f32),
     DoubleClick(f32, f32),
@@ -41,6 +52,8 @@ pub enum Effect {
     Menu(NodeId),
     /// Relaunch elevated.
     Elevate,
+    /// Rescan the drive at this index in the background, keeping the old tree on screen.
+    Rescan(usize),
 }
 
 /// The sort each header column stands for; "% of parent" orders by size.
@@ -61,6 +74,9 @@ pub struct App {
     /// Show the "Run as administrator" button (the process is not elevated).
     pub can_elevate: bool,
     pub adapter: String,
+    pub live: LiveStatus,
+    /// A background rescan is running while the old tree stays on screen.
+    pub refreshing: bool,
     /// Header column last clicked (0 Name, 1 Size, 2 % of parent, 3 Items) and direction.
     sort: (usize, bool),
     /// While dragging the scrollbar thumb: distance from the thumb's top to the grab point.
@@ -72,6 +88,7 @@ impl App {
         Self {
             layout, drives, adapter, active: None, tree: None, model: None, scroll: 0,
             can_elevate: false, scan: Scan::Idle, summary: String::new(), sort: (1, true), grab: None,
+            live: LiveStatus::Off, refreshing: false,
         }
     }
 
@@ -86,6 +103,8 @@ impl App {
         self.model = None;
         self.scroll = 0;
         self.summary.clear();
+        self.set_live(LiveStatus::Off);
+        self.refreshing = false;
         self.scan = Scan::Running { drive, dirs: 0, files: 0, bytes: 0 };
     }
 
@@ -108,6 +127,45 @@ impl App {
         self.scroll = 0;
         self.summary = summary;
         self.scan = Scan::Idle;
+        self.set_live(LiveStatus::Off);
+    }
+
+    /// Replaces the tree with a rescanned one, keeping the sort, the expanded folders and the
+    /// selection (by path) and where it sits on screen.
+    pub fn swap_tree(&mut self, tree: Tree, summary: String) {
+        let kept = match (&self.tree, &self.model) {
+            (Some(t), Some(m)) => Some(Snapshot::take(t, m, self.scroll)),
+            _ => None,
+        };
+        let Some(snap) = kept else { return self.finish_scan(tree, summary) };
+        let (model, scroll) = snap.apply(&tree, sort_key(self.sort.0), self.sort.1, self.layout.rows_visible());
+        self.tree = Some(tree);
+        self.model = Some(model);
+        self.scroll = scroll;
+        self.summary = summary;
+        self.refreshing = false;
+        self.set_live(LiveStatus::Off);
+    }
+
+    /// Brings the rows up to date after the tree changed in place.
+    pub fn refresh_rows(&mut self) {
+        if let (Some(t), Some(m)) = (&mut self.tree, &mut self.model) {
+            let s = m.refresh(t, self.scroll);
+            self.set_scroll(s);
+        }
+    }
+
+    pub fn set_live(&mut self, live: LiveStatus) {
+        self.layout = self.layout.with_banner(matches!(live, LiveStatus::Stale(_)));
+        self.live = live;
+        self.set_scroll(self.scroll);
+    }
+
+    /// Records the clock time of the last change applied by live updates.
+    pub fn live_changed(&mut self, at: String) {
+        if let LiveStatus::On { last } = &mut self.live {
+            *last = Some(at);
+        }
     }
 
     pub fn fail_scan(&mut self, msg: String) {
@@ -133,7 +191,7 @@ impl App {
     pub fn handle(&mut self, input: Input) -> Effect {
         match input {
             Input::Resize(w, h) => {
-                self.layout = Layout::new(w, h);
+                self.layout = Layout::new(w, h).with_banner(self.layout.banner);
                 self.set_scroll(self.scroll);
             }
             Input::Wheel(rows) => self.set_scroll((self.scroll as i64 + rows as i64).max(0) as usize),
@@ -179,6 +237,11 @@ impl App {
         }
         match self.layout.hit(x, y, self.drives.len()) {
             Hit::Chip(i) if !matches!(self.scan, Scan::Running { .. }) => return Effect::Scan(i),
+            Hit::Rescan => {
+                if let Some(i) = self.active.filter(|_| !self.refreshing && !matches!(self.scan, Scan::Running { .. })) {
+                    return Effect::Rescan(i);
+                }
+            }
             Hit::Header(c) => {
                 let desc = if self.sort.0 == c { !self.sort.1 } else { c != 0 };
                 self.sort = (c, desc);
@@ -230,14 +293,31 @@ impl App {
     }
 
     fn status(&self) -> String {
-        let head = if self.summary.is_empty() { "plumb" } else { &self.summary };
-        format!("{head}   |   {}", self.adapter)
+        let mut parts = vec![if self.summary.is_empty() { "plumb".to_string() } else { self.summary.clone() }];
+        match &self.live {
+            LiveStatus::Off => {}
+            LiveStatus::On { last: None } => parts.push("live".into()),
+            LiveStatus::On { last: Some(t) } => parts.push(format!("live, last change {t}")),
+            LiveStatus::Stale(_) => parts.push("live updates stopped".into()),
+        }
+        if self.refreshing {
+            parts.push("refreshing\u{2026}".into());
+        }
+        parts.push(self.adapter.clone());
+        parts.join("   |   ")
+    }
+
+    fn banner(&self) -> Option<String> {
+        match &self.live {
+            LiveStatus::Stale(why) => Some(format!("live updates stopped: {why}; the tree may be out of date")),
+            _ => None,
+        }
     }
 
     /// Builds the scene for `dark` and passes it to `f`; the scene borrows locals made here.
     pub fn with_scene<R>(&self, dark: bool, f: impl FnOnce(&Scene) -> R) -> R {
         let chips = self.chips();
-        let (message, status) = (self.message(), self.status());
+        let (message, status, banner) = (self.message(), self.status(), self.banner());
         let view = match (&self.tree, &self.model) {
             (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.scroll }),
             _ => None,
@@ -252,6 +332,7 @@ impl App {
             view,
             message: &message,
             status: &status,
+            banner: banner.as_deref(),
         })
     }
 }
@@ -392,6 +473,73 @@ mod tests {
         assert_eq!(a.scroll, s);
     }
 
+    #[test]
+    fn a_stale_banner_shifts_the_table_and_its_button_asks_for_a_rescan() {
+        let mut a = scanned();
+        let rows = a.layout.rows_visible();
+        a.set_live(LiveStatus::Stale("the journal is off".into()));
+        assert!(a.layout.banner && a.layout.rows_visible() < rows);
+        assert_eq!(a.banner().unwrap(), "live updates stopped: the journal is off; the tree may be out of date");
+        let b = a.layout.rescan_button();
+        assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0)), Effect::Rescan(0));
+        a.refreshing = true;
+        assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0)), Effect::None);
+        a.handle(Input::Resize(900.0, 700.0));
+        assert!(a.layout.banner, "a resize keeps the banner");
+        a.set_live(LiveStatus::Off);
+        assert!(!a.layout.banner && a.banner().is_none());
+    }
+
+    #[test]
+    fn the_status_line_shows_the_live_marker_and_refreshing() {
+        let mut a = scanned();
+        assert_eq!(a.status(), "done   |   test");
+        a.set_live(LiveStatus::On { last: None });
+        assert_eq!(a.status(), "done   |   live   |   test");
+        a.live_changed("12:03:41".into());
+        a.refreshing = true;
+        assert_eq!(a.status(), "done   |   live, last change 12:03:41   |   refreshing\u{2026}   |   test");
+        a.set_live(LiveStatus::Stale("x".into()));
+        a.live_changed("12:04:00".into());
+        assert!(a.status().contains("live updates stopped") && !a.status().contains("12:04"));
+    }
+
+    #[test]
+    fn a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row() {
+        let mut a = scanned();
+        a.handle(Input::Click(5.0, PICKER_H + 2.0)); // sort by Name ascending
+        let t = a.tree.as_ref().unwrap();
+        let m = a.model.as_mut().unwrap();
+        let first = m.rows()[1].id;
+        m.expand(t, first);
+        let kid = m.rows()[2].id;
+        m.select(kid);
+        a.scroll = 0;
+        let path = a.tree.as_ref().unwrap().path(kid);
+        a.refreshing = true;
+        a.swap_tree(kary_builder(300, 3).finish().unwrap(), "again".into());
+        let (t, m) = (a.tree.as_ref().unwrap(), a.model.as_ref().unwrap());
+        assert_eq!(t.path(m.selected().unwrap()), path);
+        assert_eq!(a.sort(), (0, false));
+        assert!(m.is_expanded(m.rows()[1].id));
+        assert_eq!(a.summary, "again");
+        assert!(!a.refreshing);
+    }
+
+    #[test]
+    fn refresh_rows_picks_up_tree_changes_and_keeps_the_selection_in_place() {
+        let mut a = scanned();
+        let sel = a.model.as_ref().unwrap().rows()[3].id;
+        a.model.as_mut().unwrap().select(sel);
+        let n = a.model.as_ref().unwrap().rows().len();
+        let root = a.model.as_ref().unwrap().rows()[0].id;
+        a.tree.as_mut().unwrap().add_child(root, &[b'n' as u16, b'w' as u16], 1 << 40, plumb_core::NodeFlags::empty());
+        a.refresh_rows();
+        let m = a.model.as_ref().unwrap();
+        assert_eq!(m.rows().len(), n + 1);
+        assert_eq!(m.selected(), Some(sel));
+    }
+
     #[test]
     fn resize_reclamps_scroll() {
         let mut a = scanned();
diff --git a/crates/plumb/src/draw.rs b/crates/plumb/src/draw.rs
index 3187c6c..6a58ed7 100644
--- a/crates/plumb/src/draw.rs
+++ b/crates/plumb/src/draw.rs
@@ -1,7 +1,7 @@
 //! Draws the whole window with Direct2D onto a target bitmap. Reads state, owns none.
 use crate::fmt;
 use crate::gfx::Gfx;
-use crate::layout::{Layout, Rect, INDENT, ROW_H};
+use crate::layout::{Layout, Rect, INDENT, RESCAN_W, ROW_H};
 use plumb_core::rows::RowModel;
 use plumb_core::{NodeFlags, Tree};
 use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
@@ -14,6 +14,9 @@ use windows::Win32::Graphics::DirectWrite::{
 };
 use windows::core::{w, Result};
 
+/// Banner text stops this far from the right edge, short of the Rescan button.
+const RESCAN_RESERVED: f32 = RESCAN_W + 24.0;
+
 fn rgb(hex: u32) -> D2D1_COLOR_F {
     D2D1_COLOR_F {
         r: ((hex >> 16) & 255) as f32 / 255.0,
@@ -79,6 +82,8 @@ pub struct Scene<'a> {
     pub view: Option<View<'a>>,
     pub message: &'a str,
     pub status: &'a str,
+    /// Text of the stale banner; its strip and Rescan button come from `layout`.
+    pub banner: Option<&'a str>,
 }
 
 pub struct Painter {
@@ -157,6 +162,14 @@ impl Pen<'_> {
             self.fill(r, t.panel);
             self.text("Run as administrator", Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, t.accent, false);
         }
+        if let Some(text) = s.banner {
+            let b = l.banner_strip();
+            self.fill(b, t.panel);
+            self.text(text, Rect { x: b.x + 8.0, y: b.y, w: b.w - RESCAN_RESERVED, h: b.h }, t.fg, false);
+            let r = l.rescan_button();
+            self.fill(r, t.select);
+            self.text("Rescan", Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, t.accent, false);
+        }
         let h = l.header();
         self.fill(h, t.panel);
         for (c, name) in ["Name", "Size", "% of parent", "Items"].iter().enumerate() {
diff --git a/crates/plumb/src/layout.rs b/crates/plumb/src/layout.rs
index f11a187..be29cef 100644
--- a/crates/plumb/src/layout.rs
+++ b/crates/plumb/src/layout.rs
@@ -22,6 +22,8 @@ pub const SCROLLBAR_W: f32 = 12.0;
 pub const CHIP_W: f32 = 104.0;
 pub const CHIP_GAP: f32 = 8.0;
 pub const ADMIN_W: f32 = 168.0;
+pub const BANNER_H: f32 = 28.0;
+pub const RESCAN_W: f32 = 80.0;
 pub const INDENT: f32 = 16.0;
 /// Widths of the Size, % of parent and Items columns; Name takes the rest.
 pub const COL_W: [f32; 3] = [100.0, 130.0, 90.0];
@@ -34,6 +36,8 @@ pub enum Hit {
     /// Index into the visible window (0 is the first drawn row).
     Row(usize),
     Scrollbar,
+    /// The Rescan button on the stale banner.
+    Rescan,
     None,
 }
 
@@ -41,11 +45,31 @@ pub enum Hit {
 pub struct Layout {
     pub w: f32,
     pub h: f32,
+    /// A banner strip sits between the picker and the header.
+    pub banner: bool,
 }
 
 impl Layout {
     pub fn new(w: f32, h: f32) -> Self {
-        Self { w, h }
+        Self { w, h, banner: false }
+    }
+
+    pub fn with_banner(self, banner: bool) -> Self {
+        Self { banner, ..self }
+    }
+
+    /// Top edge of the header: below the picker, and below the banner when there is one.
+    fn top(&self) -> f32 {
+        PICKER_H + if self.banner { BANNER_H } else { 0.0 }
+    }
+
+    pub fn banner_strip(&self) -> Rect {
+        Rect { x: 0.0, y: PICKER_H, w: self.w, h: BANNER_H }
+    }
+
+    /// "Rescan", right-aligned in the banner.
+    pub fn rescan_button(&self) -> Rect {
+        Rect { x: (self.w - 8.0 - RESCAN_W).max(0.0), y: PICKER_H + 3.0, w: RESCAN_W, h: BANNER_H - 6.0 }
     }
 
     pub fn chip(&self, i: usize) -> Rect {
@@ -58,12 +82,12 @@ impl Layout {
     }
 
     pub fn header(&self) -> Rect {
-        Rect { x: 0.0, y: PICKER_H, w: self.w, h: HEADER_H }
+        Rect { x: 0.0, y: self.top(), w: self.w, h: HEADER_H }
     }
 
     /// Row area, excluding the header and scrollbar.
     pub fn rows_area(&self) -> Rect {
-        let y = PICKER_H + HEADER_H;
+        let y = self.top() + HEADER_H;
         Rect { x: 0.0, y, w: (self.w - SCROLLBAR_W).max(0.0), h: (self.h - y - STATUS_H).max(0.0) }
     }
 
@@ -122,6 +146,9 @@ impl Layout {
                 return Hit::Chip(i);
             }
         }
+        if self.banner && self.rescan_button().contains(x, y) {
+            return Hit::Rescan;
+        }
         if self.header().contains(x, y) {
             return (0..4).find(|&c| { let (cx, cw) = self.column(c); x >= cx && x < cx + cw }).map_or(Hit::None, Hit::Header);
         }
@@ -173,6 +200,18 @@ mod tests {
         assert_eq!(l.hit(5.0, 595.0, 3), Hit::None);
     }
 
+    #[test]
+    fn a_banner_pushes_the_table_down_and_has_a_button() {
+        let l = Layout::new(800.0, 600.0);
+        let b = l.with_banner(true);
+        assert_eq!(b.header().y, l.header().y + BANNER_H);
+        assert_eq!(b.rows_area().y + b.rows_area().h + STATUS_H, 600.0);
+        assert_eq!(b.hit(b.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);
+        assert_eq!(l.hit(l.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Header(3));
+        assert_eq!(b.hit(5.0, PICKER_H + BANNER_H + 5.0, 3), Hit::Header(0));
+        assert!(b.rows_visible() < l.rows_visible());
+    }
+
     #[test]
     fn thumb_tracks_scroll_and_inverts() {
         let l = Layout::new(800.0, 600.0);
diff --git a/crates/plumb/src/main.rs b/crates/plumb/src/main.rs
index 209d60b..febe82a 100644
--- a/crates/plumb/src/main.rs
+++ b/crates/plumb/src/main.rs
@@ -5,6 +5,8 @@ fn main() {
     {
         let r = if std::env::args().any(|a| a == "--selftest") {
             plumb::window::selftest().map(|a| println!("selftest ok: {a}"))
+        } else if std::env::args().any(|a| a == "--selftest-live") {
+            plumb::window::selftest_live().map(|a| println!("selftest-live ok: {a}"))
         } else {
             plumb::window::run()
         };
diff --git a/crates/plumb/src/window.rs b/crates/plumb/src/window.rs
index c4cdf4e..1c4994e 100644
--- a/crates/plumb/src/window.rs
+++ b/crates/plumb/src/window.rs
@@ -1,16 +1,21 @@
 //! The Win32 window: message loop, swapchain, input translation, scan thread, context menu.
-//! All state lives on the UI thread in `STATE`; the scan thread only reports through a channel.
-use crate::app::{App, DriveInfo, Effect, Input};
+//! All state lives on the UI thread in `STATE`; the scan and journal threads only report through channels.
+use crate::app::{App, DriveInfo, Effect, Input, LiveStatus};
 use crate::draw::Painter;
 use crate::gfx::Gfx;
 use crate::layout::Layout;
+use crate::live::{needs_compaction, Live, Pacer, RescanPolicy, REPAINT};
 use plumb_core::rows::Key;
 use plumb_core::{NodeId, Tree, TreeBuilder};
+use plumb_scan::journal::{Delta, Event, JournalConfig, JournalInfo, JournalReader, JournalSource, Stop};
 use plumb_scan::select::{scan_auto, ScanReport, Used};
+use plumb_scan::volume::{drive_letter, is_elevated, is_ntfs, Volume};
 use plumb_scan::{Progress, ScanError};
 use std::cell::RefCell;
+use std::collections::VecDeque;
 use std::path::Path;
 use std::sync::mpsc::{channel, Receiver};
+use std::sync::atomic::{AtomicBool, Ordering};
 use std::sync::Arc;
 use std::time::{Duration, Instant};
 use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
@@ -32,6 +37,7 @@ use windows::Win32::Graphics::Gdi::{
 };
 use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
 use windows::Win32::System::LibraryLoader::GetModuleHandleW;
+use windows::Win32::System::SystemInformation::GetLocalTime;
 use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
 use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
 use windows::Win32::UI::HiDpi::{
@@ -46,12 +52,53 @@ use windows::Win32::UI::WindowsAndMessaging::*;
 use windows::core::{w, Interface, Result, BOOL, PCWSTR};
 
 const WM_SCAN_DONE: u32 = WM_APP + 1;
+const WM_JOURNAL: u32 = WM_APP + 2;
 const TIMER_PROGRESS: usize = 1;
+const TIMER_LIVE: usize = 2;
+/// Longest a batch of journal deltas may hold the UI thread.
+const APPLY_BUDGET: Duration = Duration::from_millis(5);
 const CMD_REVEAL: usize = 1;
 const CMD_COPY: usize = 2;
 const CMD_PROPS: usize = 3;
 
-type ScanResult = std::result::Result<(Tree, ScanReport, Duration), String>;
+struct Scanned {
+    tree: Tree,
+    report: ScanReport,
+    elapsed: Duration,
+    /// The volume and journal position to follow, read before the scan began so changes made
+    /// during it are replayed. Present only for elevated NTFS drives.
+    journal: Option<(Volume, JournalInfo)>,
+}
+
+type ScanResult = std::result::Result<Scanned, String>;
+
+/// Everything live updates need for the drive on screen.
+struct Watch {
+    reader: JournalReader,
+    live: Live,
+    queue: VecDeque<Delta>,
+    pacer: Pacer,
+    /// The tree changed since the rows were last refreshed.
+    dirty: bool,
+    /// `TIMER_LIVE` is running.
+    armed: bool,
+}
+
+fn open_journal(root: &str) -> Option<(Volume, JournalInfo)> {
+    let root = Path::new(root);
+    let letter = drive_letter(root)?;
+    if !is_elevated() || !is_ntfs(root) {
+        return None;
+    }
+    let vol = Volume::open(letter).ok()?;
+    let info = vol.query().ok()?;
+    Some((vol, info))
+}
+
+fn local_time() -> String {
+    let t = unsafe { GetLocalTime() };
+    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
+}
 
 struct Win {
     hwnd: HWND,
@@ -64,6 +111,14 @@ struct Win {
     dark: bool,
     progress: Arc<Progress>,
     rx: Option<Receiver<ScanResult>>,
+    /// The scan in flight is a rescan: the old tree stays on screen until it finishes.
+    background: bool,
+    watch: Option<Watch>,
+    policy: RescanPolicy,
+    /// When the user last clicked, scrolled or pressed a key.
+    last_input: Instant,
+    /// A `WM_JOURNAL` is already queued, so the reader need not post another.
+    posted: Arc<AtomicBool>,
 }
 
 thread_local! {
@@ -146,7 +201,11 @@ impl Win {
         let mut app = App::new(layout, drives, gfx.adapter.clone());
         app.can_elevate = !plumb_scan::volume::is_elevated();
         let dark = dark_mode();
-        let win = Self { hwnd, app, gfx, painter, swap, target: None, dpi, dark, progress: Arc::default(), rx: None };
+        let win = Self {
+            hwnd, app, gfx, painter, swap, target: None, dpi, dark, progress: Arc::default(), rx: None,
+            background: false, watch: None, policy: RescanPolicy::default(), last_input: Instant::now(),
+            posted: Arc::default(),
+        };
         win.apply_title_theme();
         Ok(win)
     }
@@ -233,11 +292,25 @@ impl Win {
 
     fn start_scan(&mut self, drive: usize) {
         let root = format!("{}:\\", self.app.drives[drive].letter);
-        self.spawn_scan(drive, root);
+        self.spawn_scan(drive, root, false);
+    }
+
+    /// Rescans in the background, keeping the old tree on screen until the new one is ready.
+    fn start_rescan(&mut self, drive: usize) {
+        if self.rx.is_none() {
+            let root = format!("{}:\\", self.app.drives[drive].letter);
+            self.spawn_scan(drive, root, true);
+        }
     }
 
-    fn spawn_scan(&mut self, drive: usize, root: String) {
-        self.app.begin_scan(drive);
+    fn spawn_scan(&mut self, drive: usize, root: String, background: bool) {
+        self.background = background;
+        if background {
+            self.app.refreshing = true;
+        } else {
+            self.drop_watch();
+            self.app.begin_scan(drive);
+        }
         self.progress = Arc::default();
         let progress = self.progress.clone();
         let (tx, rx) = channel();
@@ -245,6 +318,7 @@ impl Win {
         let hwnd = self.hwnd.0 as isize;
         std::thread::spawn(move || {
             let t0 = Instant::now();
+            let journal = open_journal(&root);
             let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                 let mut b = TreeBuilder::new();
                 let report = scan_auto(Path::new(&root), &mut b, &progress).map_err(|e| match e {
@@ -252,13 +326,15 @@ impl Win {
                     ScanError::Mft(m) => m,
                 })?;
                 let tree = b.finish().map_err(|e| format!("{e:?}"))?;
-                Ok((tree, report, t0.elapsed()))
+                Ok(Scanned { tree, report, elapsed: t0.elapsed(), journal })
             }));
             let msg = run.unwrap_or_else(|_| Err("the scanner panicked".to_string()));
             let _ = tx.send(msg);
             let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_SCAN_DONE, WPARAM(0), LPARAM(0)) };
         });
-        unsafe { SetTimer(Some(self.hwnd), TIMER_PROGRESS, 100, None) };
+        if !background {
+            unsafe { SetTimer(Some(self.hwnd), TIMER_PROGRESS, 100, None) };
+        }
         self.invalidate();
     }
 
@@ -273,17 +349,154 @@ impl Win {
         let Ok(msg) = rx.try_recv() else { return };
         self.rx = None;
         let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_PROGRESS) };
+        let background = std::mem::take(&mut self.background);
         match msg {
-            Ok((tree, report, elapsed)) => {
-                let files = tree.subtree_files[NodeId(0)] as u64;
-                let s = summary(report.used, files, elapsed, report.mft.as_ref().map(|m| m.stats.corrupt), report.note.as_deref());
-                self.app.finish_scan(tree, s);
+            Ok(sc) => {
+                let files = sc.tree.subtree_files[NodeId(0)] as u64;
+                let corrupt = sc.report.mft.as_ref().map(|m| m.stats.corrupt);
+                let s = summary(sc.report.used, files, sc.elapsed, corrupt, sc.report.note.as_deref());
+                if background {
+                    self.app.swap_tree(sc.tree, s);
+                    self.drop_watch();
+                } else {
+                    self.app.finish_scan(sc.tree, s);
+                }
+                if let (Some((vol, info)), Some(mft)) = (sc.journal, sc.report.mft) {
+                    self.start_watch(vol, info, mft.records);
+                }
+            }
+            Err(e) if background => {
+                self.app.refreshing = false;
+                self.app.set_live(LiveStatus::Stale(format!("the rescan failed: {e}")));
             }
             Err(e) => self.app.fail_scan(e),
         }
         self.invalidate();
     }
 
+    fn start_watch(&mut self, vol: Volume, info: JournalInfo, records: Vec<NodeId>) {
+        let hwnd = self.hwnd.0 as isize;
+        let posted = self.posted.clone();
+        let reader = JournalReader::start(vol, info, JournalConfig::default(), move || {
+            if !posted.swap(true, Ordering::AcqRel) {
+                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
+            }
+        });
+        self.watch = Some(Watch {
+            reader, live: Live::new(records), queue: VecDeque::new(), pacer: Pacer::new(Instant::now()),
+            dirty: false, armed: false,
+        });
+        self.app.set_live(LiveStatus::On { last: None });
+    }
+
+    /// Stops following the journal. The reader may be mid-wait, so it is joined off the UI thread.
+    fn drop_watch(&mut self) {
+        if let Some(w) = self.watch.take() {
+            if w.armed {
+                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_LIVE) };
+            }
+            let reader = w.reader;
+            std::thread::spawn(move || reader.stop());
+        }
+    }
+
+    /// `WM_JOURNAL`: moves what the reader has queued into the delta queue and applies some of it.
+    fn journal_event(&mut self) {
+        self.posted.store(false, Ordering::Release);
+        let mut stop = None;
+        if let Some(w) = &mut self.watch {
+            while let Some(ev) = w.reader.try_recv() {
+                match ev {
+                    Event::Deltas(d) => w.queue.extend(d),
+                    Event::Stopped(s) => {
+                        stop = Some(s);
+                        break;
+                    }
+                }
+            }
+        }
+        self.apply_live();
+        if let Some(s) = stop {
+            self.journal_stopped(s);
+        }
+    }
+
+    /// Applies queued deltas for at most `APPLY_BUDGET`; leftovers continue on the next message.
+    fn apply_live(&mut self) {
+        let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) else { return };
+        let n = w.live.apply(tree, &mut w.queue, Instant::now() + APPLY_BUDGET);
+        w.dirty |= n > 0;
+        if !w.queue.is_empty() {
+            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
+        }
+        if !w.armed && (w.dirty || !w.queue.is_empty() || w.live.pending()) {
+            unsafe { SetTimer(Some(self.hwnd), TIMER_LIVE, REPAINT.as_millis() as u32, None) };
+            w.armed = true;
+        }
+    }
+
+    /// `TIMER_LIVE`: at most every `REPAINT`, refreshes the rows once if the tree changed.
+    fn live_tick(&mut self) {
+        self.apply_live();
+        let due = self.watch.as_ref().is_some_and(|w| w.dirty && w.pacer.due(Instant::now()));
+        if due {
+            let t0 = Instant::now();
+            self.app.refresh_rows();
+            self.app.live_changed(local_time());
+            self.invalidate();
+            if let Some(w) = &mut self.watch {
+                w.pacer.done(Instant::now(), t0.elapsed());
+                w.dirty = false;
+            }
+            if self.app.tree.as_ref().is_some_and(needs_compaction) {
+                self.auto_rescan();
+            }
+        }
+        if let Some(w) = &mut self.watch
+            && w.armed
+            && !w.dirty
+            && w.queue.is_empty()
+            && !w.live.pending()
+        {
+            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_LIVE) };
+            w.armed = false;
+        }
+    }
+
+    /// The reader ended: bring the tree up to date, then rescan or say why updates stopped.
+    fn journal_stopped(&mut self, stop: Stop) {
+        if let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) {
+            w.live.apply(tree, &mut w.queue, Instant::now() + Duration::from_millis(50));
+        }
+        self.app.refresh_rows();
+        self.drop_watch();
+        self.app.set_live(LiveStatus::Off);
+        let wrapped = stop == Stop::Wrapped;
+        let why = match stop {
+            Stop::Requested => return,
+            Stop::Wrapped => "the change journal wrapped, so changes were missed".to_string(),
+            Stop::Disabled => "the change journal is not active".to_string(),
+            Stop::Failed(m) => m,
+        };
+        if !(wrapped && self.auto_rescan()) {
+            self.app.set_live(LiveStatus::Stale(why));
+        }
+        self.invalidate();
+    }
+
+    /// Starts a background rescan if the policy allows (or one is already running).
+    fn auto_rescan(&mut self) -> bool {
+        if self.rx.is_some() {
+            return true;
+        }
+        let Some(i) = self.app.active else { return false };
+        if !self.policy.allow(self.app.drives[i].letter, Instant::now(), self.last_input) {
+            return false;
+        }
+        self.start_rescan(i);
+        true
+    }
+
     fn selected_path(&self) -> Option<String> {
         let id = self.app.model.as_ref()?.selected()?;
         Some(display_path(&self.app.tree.as_ref()?.path(id)))
@@ -407,6 +620,9 @@ fn effect(hwnd: HWND, e: Effect, at: (i32, i32)) {
         Effect::Scan(i) => {
             with(|w| w.start_scan(i));
         }
+        Effect::Rescan(i) => {
+            with(|w| w.start_rescan(i));
+        }
         Effect::Elevate => {
             let letter = with(|w| w.app.active.map(|i| w.app.drives[i].letter)).flatten();
             if relaunch_elevated(letter) {
@@ -430,6 +646,7 @@ fn pos(l: LPARAM) -> (i32, i32) {
 fn dispatch(hwnd: HWND, l: LPARAM, make: impl FnOnce(f32, f32) -> Input) {
     let Some((e, _)) = with(|w| {
         let (x, y) = w.dip(l);
+        w.last_input = Instant::now();
         let e = w.app.handle(make(x, y));
         w.invalidate();
         (e, ())
@@ -505,6 +722,7 @@ extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRES
         WM_MOUSEWHEEL => {
             let delta = (wp.0 >> 16) as i16 as i32;
             with(|w| {
+                w.last_input = Instant::now();
                 w.app.handle(Input::Wheel(-delta / 120 * 3));
                 w.invalidate();
             });
@@ -518,6 +736,7 @@ extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRES
                 }
             } else if let Some(k) = key_of(wp.0 as u16) {
                 with(|w| {
+                    w.last_input = Instant::now();
                     w.app.handle(Input::Key(k));
                     w.invalidate();
                 });
@@ -528,6 +747,14 @@ extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRES
             with(|w| w.poll_progress());
             LRESULT(0)
         }
+        WM_TIMER if wp.0 == TIMER_LIVE => {
+            with(|w| w.live_tick());
+            LRESULT(0)
+        }
+        WM_JOURNAL => {
+            with(|w| w.journal_event());
+            LRESULT(0)
+        }
         WM_SCAN_DONE => {
             with(|w| w.scan_done());
             LRESULT(0)
@@ -615,7 +842,7 @@ pub fn selftest() -> Result<String> {
     let hwnd = create_window(false)?;
     let win = Win::new(hwnd, crate::drives::list())?;
     STATE.with(|s| *s.borrow_mut() = Some(win));
-    with(|w| w.spawn_scan(0, dir.to_string_lossy().into_owned()));
+    with(|w| w.spawn_scan(0, dir.to_string_lossy().into_owned(), false));
     let deadline = Instant::now() + Duration::from_secs(30);
     let mut msg = MSG::default();
     while with(|w| w.app.tree.is_none() && !matches!(w.app.scan, crate::app::Scan::Failed(_))) == Some(true) {
@@ -642,6 +869,75 @@ pub fn selftest() -> Result<String> {
     out.expect("state present")
 }
 
+/// Elevated only. Scans the temp directory's drive in a hidden window, then creates, grows,
+/// renames and deletes a file there and checks the tree follows each change within 20 s.
+/// Returns a one-line summary. Run it from an administrator prompt: `plumb --selftest-live`.
+pub fn selftest_live() -> Result<String> {
+    use windows::Win32::Foundation::E_FAIL;
+    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
+    if !is_elevated() {
+        return Err(fail("live updates need an elevated process".into()));
+    }
+    let dir = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| fail(e.to_string()))?;
+    let dir = dir.join(format!("plumb-live-{}", std::process::id()));
+    let shown = dir.to_string_lossy().trim_start_matches(r"\\?\").to_string();
+    let letter = shown.chars().next().unwrap_or('C').to_ascii_uppercase();
+    let hwnd = create_window(false)?;
+    let win = Win::new(hwnd, crate::drives::list())?;
+    STATE.with(|s| *s.borrow_mut() = Some(win));
+    unsafe { SetTimer(Some(hwnd), 99, 50, None) }; // wakes the message wait so deadlines are checked
+
+    // Blocks in GetMessageW until `cond` holds; every message goes through the real window procedure.
+    let wait = |what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool| -> Result<()> {
+        let deadline = Instant::now() + Duration::from_secs(secs);
+        let mut msg = MSG::default();
+        while with(|w| !cond(w)) == Some(true) {
+            if Instant::now() > deadline {
+                return Err(fail(format!("timed out waiting for: {what}")));
+            }
+            unsafe {
+                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
+                    let _ = TranslateMessage(&msg);
+                    DispatchMessageW(&msg);
+                }
+            }
+        }
+        Ok(())
+    };
+    let size_at = |w: &mut Win, rel: &str| -> Option<u64> {
+        let t = w.app.tree.as_ref()?;
+        let id = crate::restore::resolve(t, &format!("{shown}\\{rel}"))?;
+        Some(t.subtree_size[id])
+    };
+    let dir_gone = |w: &mut Win| w.app.tree.as_ref().is_some_and(|t| crate::restore::resolve(t, &shown).is_none());
+    let run = || -> Result<String> {
+        let i = with(|w| w.app.drives.iter().position(|d| d.letter == letter)).flatten();
+        let i = i.ok_or_else(|| fail(format!("drive {letter}: is not in the drive list")))?;
+        with(|w| w.spawn_scan(i, format!("{letter}:\\"), false));
+        wait("the scan and the journal reader", 180, &|w| w.watch.is_some())?;
+        std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
+        std::fs::write(dir.join("f.bin"), vec![1u8; 1 << 20]).map_err(|e| fail(e.to_string()))?;
+        wait("the new file", 20, &|w| size_at(w, "f.bin") == Some(1 << 20))?;
+        wait("the live marker", 20, &|w| matches!(&w.app.live, LiveStatus::On { last: Some(_) }))?;
+        std::fs::write(dir.join("f.bin"), vec![1u8; 3 << 20]).map_err(|e| fail(e.to_string()))?;
+        wait("the grown file", 20, &|w| size_at(w, "f.bin") == Some(3 << 20))?;
+        std::fs::rename(dir.join("f.bin"), dir.join("g.bin")).map_err(|e| fail(e.to_string()))?;
+        wait("the rename", 20, &|w| size_at(w, "g.bin") == Some(3 << 20) && size_at(w, "f.bin").is_none())?;
+        std::fs::remove_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
+        wait("the delete", 20, &|w| size_at(w, "g.bin").is_none() && dir_gone(w))?;
+        let stale = with(|w| matches!(w.app.live, LiveStatus::Stale(_))).unwrap_or(false);
+        if stale {
+            return Err(fail("live updates reported stale".into()));
+        }
+        Ok(format!("{letter}: create, grow, rename and delete followed"))
+    };
+    let out = run();
+    let _ = std::fs::remove_dir_all(&dir);
+    unsafe { DestroyWindow(hwnd)? };
+    STATE.with(|s| *s.borrow_mut() = None);
+    out
+}
+
 #[cfg(test)]
 mod tests {
     use super::*;
diff --git a/crates/plumb/tests/render.rs b/crates/plumb/tests/render.rs
index 853e022..2d7dea2 100644
--- a/crates/plumb/tests/render.rs
+++ b/crates/plumb/tests/render.rs
@@ -46,6 +46,7 @@ fn render(dark: bool) -> Vec<u8> {
         view: Some(View { tree: &tree, model: &model, scroll: 0 }),
         message: "",
         status: "MFT scan: 200 files",
+        banner: None,
     };
     let target = bitmap(&gfx, D2D1_BITMAP_OPTIONS_TARGET);
     Painter::new(&gfx).unwrap().paint(&gfx, &target, 96.0, &scene).expect("EndDraw");
```

- [ ] **Step 2: Build, test and lint**

Run: `cargo test --workspace` then `cargo clippy -p plumb --all-targets`
Expected: every suite ok; `plumb` lib `45 passed` (20 before this plan, 16 `live`, 4 `restore`, 4 `app`, 1 `layout`); `plumb-core` 55, `plumb-scan` 50. Clippy prints no warning for `live.rs`, `restore.rs`, `layout.rs`, `draw.rs` or `window.rs` (older warnings in `app.rs`, `fmt.rs`, `plumb-core` and `plumb-scan` remain).

- [ ] **Step 3: Hidden-window selftest on the real GPU**

Run: `cargo test -p plumb -- --ignored`
Expected: `hidden_window_scans_and_renders ... ok` (this scans a temp folder, not a drive root, so no journal is opened).

- [ ] **Step 4: Non-elevated check of the new flag**

Run: `cargo run -p plumb -- --selftest-live` from a normal prompt.
Expected: exits 1 with `plumb: live updates need an elevated process`.

- [ ] **Step 5: Commit**

```bash
git add crates/plumb
git commit -m "Wire live updates into the window: journal watch, budgeted apply, 4 Hz refresh, background rescan, stale banner"
```

### Task 4: Elevated check, manual look and docs

**Files:**
- Modify: `CHANGELOG.md`, `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md`

- [ ] **Step 1: Check nothing else is running**

Run (PowerShell): `Get-Process plumb, cargo -ErrorAction SilentlyContinue`
Expected: nothing left over from earlier steps. Defender real-time protection state is worth a note only if timings are reported.

- [ ] **Step 2: Elevated end-to-end check (needs the user)**

Ask the user to run this from an administrator PowerShell, from the repository root:

```powershell
cargo run -p plumb --release -- --selftest-live
```

Expected: `selftest-live ok: C: create, grow, rename and delete followed` (the letter is the temp directory's drive), after the drive's MFT scan (a few seconds) plus about a second for each of the four changes. A timeout names the step that did not happen. If it fails, stop and debug before going on; do not weaken the check.

- [ ] **Step 3: Manual look (needs the user, elevated)**

Run `cargo run -p plumb --release -- --scan C` elevated (the relaunch button does the same). With the window open, in a scratch folder on that drive: create a big file, delete it, rename a folder. Check, and write down what was seen:
1. The status bar shows `live` and then `live, last change HH:MM:SS` within about a second of each change.
2. The selected row stays where it was on screen while rows above it change.
3. Scrolling with the wheel while a build or copy is running stays smooth (no multi-second freezes).
4. Optional: `fsutil usn deletejournal /d C:` then `fsutil usn createjournal m=33554432 a=8388608 C:` makes the journal vanish; the banner should appear (or a background rescan run if 30 s have passed since the last input) and Rescan should clear it. Re-enable the journal afterwards if it was on.

- [ ] **Step 4: Update the docs**

Apply the patch below (the changelog), then in the spec change plan 3 in the "Plans" list to **Done** with the plan file name, and add a short "Plan 3 as built" section listing the rows of the Decisions table above and the outcome of steps 2 and 3 (one line each, with the date).

<!-- patch -->
```diff
--- a/CHANGELOG.md
+++ b/CHANGELOG.md
@@ -5,6 +5,7 @@
 ## [Unreleased]

 ### Added
+- `plumb`: live updates. On an elevated NTFS drive scanned with the MFT scanner, the tree follows the change journal: new, grown, renamed, moved and deleted files appear within about a second, the status bar shows "live, last change HH:MM:SS", and the selection stays on the same screen row. A wrapped journal or a pile of dead nodes triggers a background rescan (at most once per 10 minutes per drive, never within 30 s of input) that keeps the old tree on screen, then swaps the new one in with the sort, expanded folders, selection and scroll restored by path. If live updates stop for any other reason, a banner says why and offers a Rescan button. `plumb --selftest-live` (elevated) checks create, grow, rename and delete end to end.
 - `plumb-scan`: NTFS change-journal reader (`journal` module: USN buffer parser, per-file state from the MFT record, a reader thread with a 250 ms coalescing window, `JournalSource` trait with a Windows implementation for `Volume`), elevated `journal-check` and `journal-spike` examples, and the MFT scan now also returns the record-number-to-node table (`MftReport`). Not yet used by the UI.
 - `plumb-core`: in-place tree mutation (`add_child`, `set_size`, `rename`, `remove`, `reparent`, `NodeFlags::DEAD`) and `RowModel::refresh`, which keeps the selected row at the same screen row. Foundation for live updates (M2); not yet wired into the UI.

```

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md docs
git commit -m "Record live-updates plan 3 results: UI wiring as built"
```

## Self-review

**Spec coverage.** Record table applied with a 5 ms budget and 4 Hz repaints: Task 3 (`APPLY_BUDGET`, `REPAINT`, `Pacer`). `refresh` once per tick and skipped when nothing changed: `live_tick` (`dirty` is set only when `Live::apply` returns a non-zero change count). Parking and `<orphans>` after 2 passes: Task 1. Live marker with last-change time: `LiveStatus::On` and the status line. Wrapped journal rescan with rate limit, input-idle check, old tree kept, "refreshing…", restore by path: `auto_rescan`, `RescanPolicy`, `swap_tree`, `Snapshot`. Stale banner with Rescan for disabled, failed and un-rescannable cases: `journal_stopped`, `Layout` banner. Compaction at 25% / 1M: `needs_compaction` in `live_tick`. `JournalInfo` before the scan, reader started at it when the scan lands: `open_journal` and `start_watch`. Gaps: none against the spec's UI section; the departures are in the Decisions table.

**Placeholder scan.** No TBD or "similar to" steps; every code step carries its code. Task 4 steps 2 and 3 need a person, and say what to run and what to expect.

**Type consistency.** `Live::apply` returns `usize` and `window.rs` uses it as a change count (`n > 0`). `Snapshot::apply` returns `(RowModel, usize)` and `App::swap_tree` uses both. `Effect::Rescan(usize)` is produced in `App::click` and consumed in `window::effect`. `Layout::with_banner` is the only way the flag changes and `App::set_live` and `Input::Resize` both go through it.

**Adversarial passes while building.** (1) The first oracle run had a multiplication overflow in its own seed setup and a wrong compaction threshold in a test; both were mistakes in the tests. (2) Reading `reparent` against the swap case showed that a refused move must be retried, not dropped (test added, mutation-checked). (3) A single settle pass would orphan a three-deep new folder chain delivered child-first; settle now loops to a fixpoint. (4) Re-arming `SetTimer` on every message would keep postponing the tick under a steady stream, so the timer is armed once. (5) A 5 ms slice per 250 ms tick would drain 100k deltas in seconds, so the slices re-post `WM_JOURNAL`. (6) `JournalReader::stop` can block up to a second, so it runs on a helper thread. (7) The 25% rule alone rescans a three-node tree after one delete: floor added. Known untested paths (need a real journal): the posted-message handshake, the timer arming and `journal_stopped`; `selftest_live` covers the first two and a journal deletion (Task 4 step 3.4) the third.
