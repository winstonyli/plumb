# Partial tree while a scan runs: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While a scan runs, the window shows a live preview tree (folders appear as found, sizes grow, everything is interactive), then swaps in the exact tree at the end.

**Architecture:** Scanners send `PreviewMsg`s through a bounded, never-blocking channel carried by `Progress`. The UI thread owns a small applier (`plumb::preview::Preview`) that turns messages into `Tree` mutations in 5 ms slices on a 15 ms timer, refreshing rows at the live-update pace. The exact `TreeBuilder`/`MftMerger` builds are unchanged and replace the preview through `swap_tree`.

**Tech stack:** Rust 2024 (nightly: let chains), `plumb-core` (`Tree`), `plumb-scan` (walker, MFT scanner), `plumb` (window, `App`), std `mpsc::sync_channel`.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-partial-tree-design.md`

## Spec amendments made by this plan

1. **The MFT preview does not modify `Live`.** The spec proposed four changes to `Live` (preview mode, parent-indexed parking, directory-only parents, root up front). In a scan every record is seen once, so none of `Live`'s rename/move/remove/orphan machinery applies; a 40-line applier of its own (`preview.rs`, `Mft`) does exactly the four things and leaves the journal path untouched. The spike measured `Live`; the new applier does less per record, and the manual check in Task 9 re-measures.
2. **No separate `Hint` message:** `Begin { root, nodes }` carries the root name and a capacity hint for the MFT preview. The walker's first `Add` (parent `NONE`) is its root.
3. **Timers:** a new 15 ms preview timer applies messages (5 ms budget, about a third of the thread at most; the spike's 8 M records/s against a 1.3 M/s scan leaves a wide margin). Row refreshes are paced by `Pacer`.
4. **`RowModel::refresh` clears its sort cache outright** when the touched list is long (Task 6), because a preview tick touches hundreds of thousands of nodes and the per-touched ancestor walk would cost more than rebuilding.
5. **`swap_tree` also ends the scan** (`scan = Idle`) and `fail_scan` clears any preview tree (Task 7).

## Global constraints

- No compatibility shims. `Progress` gains one field; every scanner call site keeps its signature.
- With no sender attached, scanners behave exactly as today (tests, `scan-compare`, rescans).
- A scanner must never block on the preview: sends use `try_send`; a full queue sets a sticky `lagged` flag and drops that message and every later one.
- The preview is approximate; exactness is `finish()`'s job. Do not touch `merge.rs::finish`, `TreeBuilder::finish`, or `Live`.
- `cargo` with `-j 8`, `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3`. Python scripts that edit files: write to a file under the scratchpad, run `python3 -I -P f.py < /dev/null`; never python on stdin.
- Edits go through the Edit tool or a script that preserves CRLF (the repo files are CRLF).

## File map

- Create `crates/plumb-scan/src/preview.rs`: `PreviewMsg`, `PreviewTx`, `PreviewRx`, `channel`.
- Modify `crates/plumb-scan/src/lib.rs`: `pub mod preview;`, `Progress.preview`, `Progress::with_preview`, `Progress::emit`.
- Modify `crates/plumb-scan/src/journal/state.rs`: factor `file_state` out of `observe`.
- Modify `crates/plumb-scan/src/walk.rs`, `select.rs`: walker emits, fallback sends `Reset`.
- Modify `crates/plumb-scan/src/mft_scan.rs`: MFT emits `Begin`, `Upsert`, `Size`.
- Modify `crates/plumb-core/src/tree.rs`: `Tree::reserve`. `crates/plumb-core/src/rows.rs`: refresh cache clear.
- Create `crates/plumb/src/preview.rs` (+ `preview/tests.rs`): the applier.
- Modify `crates/plumb/src/lib.rs`, `app.rs`, `window.rs`: wiring and status.
- Docs: `CHANGELOG.md`, the spec, `docs/benchmarks.md`.

---

### Task 1: The preview channel in `plumb-scan`

**Files:**
- Create: `crates/plumb-scan/src/preview.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Produces: `PreviewMsg`, `PreviewTx` (`send`, `reset`, `lagged`), `PreviewRx` (`try_recv`, `lagged`), `preview::channel(cap) -> (PreviewTx, PreviewRx)`, `Progress::with_preview(PreviewTx) -> Progress`, `Progress::emit(&self, impl FnOnce() -> Option<PreviewMsg>)`.

- [ ] **Step 1: Write the module with its tests.** Create `crates/plumb-scan/src/preview.rs`:

```rust
//! What a running scan tells the window so it can show a partial tree. Best effort: sends never
//! block the scanner. Once the queue is full the sender is `lagged` for good and drops everything,
//! so the receiver's prefix stays consistent (the walker's node ids depend on it).
use crate::journal::state::FileState;
use plumb_core::{NodeFlags, NodeId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;

#[derive(Debug)]
pub enum PreviewMsg {
    /// MFT scan: the root's name and how many record slots there are (a capacity hint).
    Begin { root: String, nodes: usize },
    /// Walker: a node was pushed. Ids follow push order, so the receiver's ids equal the builder's.
    /// The first message has parent `NodeId::NONE` and is the root.
    Add { parent: NodeId, name: Vec<u16>, size: u64, flags: NodeFlags },
    /// Walker: the directory could not be listed.
    Denied(NodeId),
    /// MFT scan: a base record's file, with the name that carries its size.
    Upsert(FileState),
    /// MFT scan: a size found in an extension record of `record`.
    Size { record: u64, size: u64 },
    /// A scanner gave up and another starts: forget everything received so far.
    Reset,
}

#[derive(Clone)]
pub struct PreviewTx {
    tx: SyncSender<PreviewMsg>,
    lagged: Arc<AtomicBool>,
}

pub struct PreviewRx {
    rx: Receiver<PreviewMsg>,
    lagged: Arc<AtomicBool>,
}

/// A channel holding at most `cap` messages.
pub fn channel(cap: usize) -> (PreviewTx, PreviewRx) {
    let (tx, rx) = sync_channel(cap);
    let lagged = Arc::new(AtomicBool::new(false));
    (PreviewTx { tx, lagged: lagged.clone() }, PreviewRx { rx, lagged })
}

impl PreviewTx {
    pub fn lagged(&self) -> bool {
        self.lagged.load(Ordering::Relaxed)
    }

    /// Queues `msg`, or marks the sender lagged (dropping `msg` and every later message) if the
    /// queue is full or the receiver is gone.
    pub fn send(&self, msg: PreviewMsg) {
        if self.lagged() {
            return;
        }
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.lagged.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Tells the receiver to start over, and lets later messages through again. If the queue is
    /// too full to take the `Reset`, the sender stays lagged.
    pub fn reset(&self) {
        self.lagged.store(false, Ordering::Relaxed);
        self.send(PreviewMsg::Reset);
    }
}

impl PreviewRx {
    pub fn lagged(&self) -> bool {
        self.lagged.load(Ordering::Relaxed)
    }

    /// `None` when nothing is queued (or the sender is gone).
    pub fn try_recv(&self) -> Option<PreviewMsg> {
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_marks_the_sender_lagged_and_drops_everything_after() {
        let (tx, rx) = channel(2);
        for i in 0..5u64 {
            tx.send(PreviewMsg::Size { record: i, size: 0 });
        }
        assert!(tx.lagged() && rx.lagged());
        // Draining makes room, but nothing sent after the first drop may get through.
        let got: Vec<u64> = std::iter::from_fn(|| rx.try_recv())
            .map(|m| match m {
                PreviewMsg::Size { record, .. } => record,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(got, [0, 1]);
        tx.send(PreviewMsg::Size { record: 9, size: 0 });
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn reset_lets_messages_through_again() {
        let (tx, rx) = channel(2);
        for i in 0..5u64 {
            tx.send(PreviewMsg::Size { record: i, size: 0 });
        }
        while rx.try_recv().is_some() {}
        tx.reset();
        tx.send(PreviewMsg::Size { record: 7, size: 0 });
        assert!(matches!(rx.try_recv(), Some(PreviewMsg::Reset)));
        assert!(matches!(rx.try_recv(), Some(PreviewMsg::Size { record: 7, .. })));
        assert!(!tx.lagged());
    }

    #[test]
    fn a_dropped_receiver_lags_the_sender_without_panicking() {
        let (tx, rx) = channel(2);
        drop(rx);
        tx.send(PreviewMsg::Reset);
        assert!(tx.lagged());
    }
}
```

- [ ] **Step 2: `Progress` carries the sender.** In `crates/plumb-scan/src/lib.rs` add `pub mod preview;` after `pub mod mft_scan;`, and replace the `Progress` struct and impl:

```rust
/// Counters a scanner updates while it runs. Read them from another thread to show progress.
/// Optionally also carries the channel a partial-tree preview is sent through.
#[derive(Default)]
pub struct Progress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    /// Allocated bytes seen so far.
    pub bytes: AtomicU64,
    pub preview: Option<preview::PreviewTx>,
}

impl Progress {
    pub fn with_preview(tx: preview::PreviewTx) -> Self {
        Self { preview: Some(tx), ..Self::default() }
    }

    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.dirs.load(Ordering::Relaxed),
            self.files.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }

    /// Sends the message `make` builds, unless there is no preview or it has lagged (then `make`
    /// does not run, so a lagged preview costs the scanner nothing).
    pub fn emit(&self, make: impl FnOnce() -> Option<preview::PreviewMsg>) {
        if let Some(p) = &self.preview
            && !p.lagged()
            && let Some(m) = make()
        {
            p.send(m);
        }
    }
}
```

- [ ] **Step 3: Test and commit.**

Run: `cargo test -p plumb-scan -j 8 preview` (expect 3 passed), then `cargo test -p plumb-scan -j 8` (all pass; `Progress` literals elsewhere use `Progress::default()`).

```bash
git add crates/plumb-scan && git commit -m "Add the preview channel to plumb-scan's Progress"
```

---

### Task 2: Share the record-to-`FileState` rule

**Files:**
- Modify: `crates/plumb-scan/src/journal/state.rs`

**Interfaces:**
- Produces: `pub fn file_state(record: u64, rec: &ParsedRecord) -> Option<FileState>`: the lowest-(parent, name) name of a parsed base record, `None` if it has no name. `observe` calls it.

- [ ] **Step 1: Factor it out.** In `observe` (state.rs), replace everything from `let Some(first) = scratch.names...` through the `let state = FileState { ... };` and the final `Ok(Seen::Live { ... })` with the two lines below, and add the function above `observe`:

```rust
/// The state of a parsed base record, using the name that carries the size in the exact build:
/// the lowest (parent, name). `None` if the record has no name.
pub fn file_state(record: u64, rec: &ParsedRecord) -> Option<FileState> {
    let first = rec.names.iter().min_by(|a, b| (a.parent, rec.name(a)).cmp(&(b.parent, rec.name(b))))?;
    Some(FileState {
        record,
        sequence: rec.sequence,
        parent: first.parent,
        name: rec.name(first).to_vec(),
        size: if rec.is_dir { 0 } else { rec.data_alloc.unwrap_or(0) },
        is_dir: rec.is_dir,
        reparse: is_name_surrogate(rec.reparse_tag),
        links: rec.names.len() as u32,
    })
}
```

and in `observe` after the `scratch.base` early return:

```rust
    let Some(state) = file_state(record, scratch) else { return Ok(Seen::Gone) };
    Ok(Seen::Live { needs_size: !scratch.is_dir && scratch.data_alloc.is_none(), state })
```

- [ ] **Step 2: Run** `cargo test -p plumb-scan -j 8 state` (the existing `observe` tests cover it unchanged; expect all pass).

- [ ] **Step 3: Commit** `git commit -am "Factor file_state out of observe"`.

---

### Task 3: The walker emits; the fallback resets

**Files:**
- Modify: `crates/plumb-scan/src/walk.rs`, `crates/plumb-scan/src/select.rs`
- Test: `crates/plumb-scan/src/walk/tests.rs`

**Interfaces:**
- Consumes: `Progress::emit`, `PreviewMsg::{Add, Denied, Reset}`.
- Produces: with a sender attached, the walker sends one `Add` per `TreeBuilder` push in push order, plus `Denied`; `scan_auto` calls `progress.preview.reset()` when the MFT attempt fails.

- [ ] **Step 1: Failing test** in `walk/tests.rs` (uses the file's `fixture(tag)`, a `Fixture(PathBuf)`; `WalkScanner { threads }`):

```rust
#[test]
fn the_walker_sends_one_add_per_node_in_builder_order() {
    use crate::preview::{channel, PreviewMsg};
    let f = fixture("preview");
    let (tx, rx) = channel(1 << 16);
    let p = Progress::with_preview(tx);
    let mut b = TreeBuilder::new();
    WalkScanner { threads: 2 }.scan(&f.0, &mut b, &p).unwrap();
    let t = b.finish().unwrap();
    let mut adds = Vec::new();
    while let Some(m) = rx.try_recv() {
        if let PreviewMsg::Add { parent, name, size, flags } = m {
            adds.push((parent, name, size, flags));
        }
    }
    assert_eq!(adds.len(), t.len());
    for (i, (parent, name, size, flags)) in adds.iter().enumerate() {
        let id = NodeId(i as u32);
        assert_eq!((*parent, name.as_slice(), *size, *flags), (t.parent[id], t.name(id), t.size[id], t.flags[id]), "node {i}");
    }
}
```

Run it: it fails on `adds.len()` (0 messages sent).

- [ ] **Step 2: Emit.** In `walk.rs` add `use crate::preview::PreviewMsg;`. After the root push:

```rust
        progress.emit(|| {
            Some(PreviewMsg::Add { parent: NodeId::NONE, name: shown.encode_utf16().collect(), size: 0, flags: NodeFlags::DIR })
        });
```

In the denied branch:

```rust
                let Ok(entries) = done.entries else {
                    out.add_flags(done.id, NodeFlags::DENIED);
                    progress.emit(|| Some(PreviewMsg::Denied(done.id)));
                    continue;
                };
```

and after `let id = out.push(done.id, &e.name, e.alloc, flags);`:

```rust
                    progress.emit(|| Some(PreviewMsg::Add { parent: done.id, name: e.name.clone(), size: e.alloc, flags }));
```

- [ ] **Step 3: Reset on fallback.** In `select.rs`, inside the `Err(e)` arm after the counters are zeroed:

```rust
                    if let Some(p) = &progress.preview {
                        p.reset();
                    }
```

- [ ] **Step 4:** run the test (pass), `cargo test -p plumb-scan -j 8` (all pass), commit `git commit -am "Walker sends preview adds; fallback resets the preview"`.

---

### Task 4: The MFT scanner emits

**Files:**
- Modify: `crates/plumb-scan/src/mft_scan.rs`
- Test: `crates/plumb-scan/src/mft_scan/tests.rs`

**Interfaces:**
- Consumes: `file_state` (Task 2), `Progress::emit`.
- Produces: `Begin` once after the layout checks; one `Upsert` per in-use base record with a name (not record 5); one `Size` per in-use extension record that has an unnamed-data size.

- [ ] **Step 1: Failing test.** Uses the file's `image()`, `layout()`, `RECORDS` (records 0, 5, 30-34; 34 is a hard link with names `link` under 30 and `link2` under 5). Add:

```rust
#[test]
fn the_mft_scan_sends_begin_then_one_upsert_per_base_record_but_the_root() {
    use crate::preview::{channel, PreviewMsg};
    let (tx, rx) = channel(1 << 16);
    let p = Progress::with_preview(tx);
    let mut b = TreeBuilder::new();
    scan_mft(&image(), &layout(), "X:", &MftOptions { threads: 2, chunk_bytes: 4 << 20 }, &mut b, &p).unwrap();
    let mut msgs = Vec::new();
    while let Some(m) = rx.try_recv() {
        msgs.push(m);
    }
    assert!(matches!(msgs.first(), Some(PreviewMsg::Begin { root, nodes }) if root == "X:" && *nodes == RECORDS as usize));
    let mut upserts: Vec<_> = msgs.iter().filter_map(|m| if let PreviewMsg::Upsert(s) = m { Some(s) } else { None }).collect();
    upserts.sort_by_key(|s| s.record);
    assert_eq!(upserts.iter().map(|s| s.record).collect::<Vec<_>>(), [0, 30, 31, 32, 33, 34], "record 5 is not sent");
    let link = upserts.last().unwrap();
    assert_eq!((link.parent, String::from_utf16_lossy(&link.name).as_str()), (5, "link2"), "the lowest (parent, name) carries the size");
}
```

Run: fails (no messages).

- [ ] **Step 2: Emit.** In `mft_scan.rs` add `use crate::journal::state::file_state;` and `use crate::preview::PreviewMsg;`. In `scan_mft`, after `let record_count = ...;`:

```rust
    progress.emit(|| Some(PreviewMsg::Begin { root: root_name.to_string(), nodes: record_count as usize }));
```

In `parse_chunks`, in the `Ok(())` arm, after the existing `if slot.in_use && slot.base.is_none() { ... }` block and before `numbers.push(number)`:

```rust
                    if slot.in_use {
                        match slot.base {
                            None if number != 5 => progress.emit(|| file_state(number, slot).map(PreviewMsg::Upsert)),
                            // `base` is already masked to the record number, as `MftMerger::add` uses it.
                            Some(owner) => progress.emit(|| slot.data_alloc.map(|size| PreviewMsg::Size { record: owner, size })),
                            None => {}
                        }
                    }
```

The emit runs outside the merger lock. A base record's own `data_alloc` rides in its `Upsert`; only extension records send `Size`.

- [ ] **Step 3:** test passes; `cargo test -p plumb-scan -j 8`; commit `git commit -am "MFT scanner sends preview records"`.

---

### Task 5: `Tree::reserve` and the applier

**Files:**
- Modify: `crates/plumb-core/src/tree.rs`
- Create: `crates/plumb/src/preview.rs`, `crates/plumb/src/preview/tests.rs`; modify `crates/plumb/src/lib.rs` (`pub mod preview;`)

**Interfaces:**
- Consumes: `plumb_scan::preview::{PreviewMsg, PreviewRx}`.
- Produces:
  - `Tree::reserve(&mut self, nodes: usize, pool_units: usize)`.
  - `Preview::new(PreviewRx) -> Preview`.
  - `Preview::apply(&mut self, tree: &mut Option<Tree>, deadline: Instant) -> Applied`.
  - `Preview::paused(&self) -> bool`.
  - `struct Applied { pub changes: usize, pub created: bool, pub reset: bool }`.

- [ ] **Step 1: `Tree::reserve`** in `tree.rs` inside `impl Tree` (after `add_child`):

```rust
    /// Makes room for `nodes` more nodes and `pool_units` more name units, so a tree that is
    /// grown during a scan does not reallocate every array mid-way.
    pub fn reserve(&mut self, nodes: usize, pool_units: usize) {
        self.parent.reserve(nodes);
        self.first_child.reserve(nodes);
        self.next_sibling.reserve(nodes);
        self.size.reserve(nodes);
        self.subtree_size.reserve(nodes);
        self.subtree_files.reserve(nodes);
        self.flags.reserve(nodes);
        self.name_off.reserve(nodes);
        self.name_len.reserve(nodes);
        self.pool.reserve(pool_units);
    }
```

Add a test to `tree/mutation_tests.rs`: build a one-node tree, `reserve(1000, 5000)`, assert `t.parent.capacity() >= 1001` and that 1000 `add_child`s leave `t.parent.as_ptr()` unchanged.

- [ ] **Step 2: The applier.** Create `crates/plumb/src/preview.rs`:

```rust
//! The UI side of the partial tree: turns the messages a running scan sends into `Tree`
//! mutations. Approximate on purpose (the exact tree replaces this one when the scan ends).
use plumb_core::{NodeFlags, NodeId, Tree, TreeBuilder};
use plumb_scan::journal::state::FileState;
use plumb_scan::preview::{PreviewMsg, PreviewRx};
use std::collections::HashMap;
use std::time::Instant;

/// Messages applied between two looks at the clock.
const CLOCK_EVERY: usize = 256;
/// Records waiting for a parent that has not arrived. Past this the preview stops following.
const MAX_PARKED: usize = 1_000_000;
/// Average name length assumed when reserving the name pool for an MFT preview.
const NAME_UNITS: usize = 12;

enum Kind {
    /// Nothing received since the start or the last `Reset`.
    Idle,
    /// Walker: ids equal the sender's push order.
    Walk,
    Mft(Mft),
}

#[derive(Default)]
struct Mft {
    /// Record number -> node.
    records: Vec<NodeId>,
    /// Waiting children, by the record number of the parent they wait for.
    parked: HashMap<u64, Vec<FileState>>,
    parked_count: usize,
    /// Sizes from extension records whose file has no node yet (or has size 0).
    sizes: HashMap<u64, u64>,
}

pub struct Preview {
    rx: PreviewRx,
    kind: Kind,
    /// Stopped following: a message did not fit, or too many records are parked.
    frozen: bool,
    /// The last `apply` found the queue empty.
    drained: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Tree changes made (zero means the rows need no refresh).
    pub changes: usize,
    /// The tree was created by this call (the first message).
    pub created: bool,
    /// The tree was dropped by a `Reset`.
    pub reset: bool,
}

impl Preview {
    pub fn new(rx: PreviewRx) -> Self {
        Self { rx, kind: Kind::Idle, frozen: false, drained: false }
    }

    /// True once the preview has stopped following the scan and has nothing left to show.
    pub fn paused(&self) -> bool {
        self.drained && (self.frozen || self.rx.lagged())
    }

    /// Applies queued messages until the queue is empty or `deadline` passes. A frozen preview
    /// still drains (so the sender's queue never stays full) and still honours `Reset`.
    pub fn apply(&mut self, tree: &mut Option<Tree>, deadline: Instant) -> Applied {
        let mut out = Applied::default();
        let mut n = 0usize;
        self.drained = false;
        loop {
            let Some(msg) = self.rx.try_recv() else {
                self.drained = true;
                return out;
            };
            if !self.frozen || matches!(msg, PreviewMsg::Reset) {
                self.handle(msg, tree, &mut out);
            }
            n += 1;
            if n % CLOCK_EVERY == 0 && Instant::now() >= deadline {
                return out;
            }
        }
    }

    fn handle(&mut self, msg: PreviewMsg, tree: &mut Option<Tree>, out: &mut Applied) {
        match msg {
            PreviewMsg::Reset => {
                *tree = None;
                self.kind = Kind::Idle;
                self.frozen = false;
                out.reset = true;
                out.created = false;
                out.changes += 1;
            }
            PreviewMsg::Begin { root, nodes } => {
                let mut t = root_tree(&root.encode_utf16().collect::<Vec<_>>());
                t.reserve(nodes, nodes * NAME_UNITS);
                let mut records = vec![NodeId::NONE; nodes.max(6)];
                records[5] = NodeId(0);
                self.kind = Kind::Mft(Mft { records, ..Mft::default() });
                *tree = Some(t);
                out.created = true;
                out.changes += 1;
            }
            PreviewMsg::Add { parent, name, size, flags } => {
                if parent.is_none() {
                    if matches!(self.kind, Kind::Idle) {
                        *tree = Some(root_tree(&name));
                        self.kind = Kind::Walk;
                        out.created = true;
                        out.changes += 1;
                    }
                    return;
                }
                let (Kind::Walk, Some(t)) = (&self.kind, tree.as_mut()) else { return };
                if parent.idx() >= t.len() {
                    self.frozen = true; // out of step with the sender: stop rather than guess
                    return;
                }
                t.add_child(parent, &name, size, flags);
                out.changes += 1;
            }
            PreviewMsg::Denied(id) => {
                if let (Kind::Walk, Some(t)) = (&self.kind, tree.as_mut())
                    && id.idx() < t.len()
                {
                    t.flags[id] |= NodeFlags::DENIED;
                    out.changes += 1;
                }
            }
            PreviewMsg::Upsert(s) => {
                let (Kind::Mft(m), Some(t)) = (&mut self.kind, tree.as_mut()) else { return };
                out.changes += m.place(t, s);
                if m.parked_count > MAX_PARKED {
                    self.frozen = true;
                }
            }
            PreviewMsg::Size { record, size } => {
                let (Kind::Mft(m), Some(t)) = (&mut self.kind, tree.as_mut()) else { return };
                match m.node(record) {
                    Some(id) if t.size[id] == 0 && !t.flags[id].contains(NodeFlags::DIR) => {
                        t.set_size(id, size);
                        out.changes += 1;
                    }
                    Some(_) => {}
                    None => {
                        m.sizes.entry(record).or_insert(size);
                    }
                }
            }
        }
    }
}

/// A tree holding just the root.
fn root_tree(name: &[u16]) -> Tree {
    let mut b = TreeBuilder::new();
    b.push(NodeId::NONE, name, 0, NodeFlags::DIR);
    b.finish().expect("one node")
}

impl Mft {
    fn node(&self, record: u64) -> Option<NodeId> {
        let id = *self.records.get(usize::try_from(record).ok()?)?;
        (!id.is_none()).then_some(id)
    }

    fn is_dir_node(&self, t: &Tree, record: u64) -> Option<NodeId> {
        self.node(record).filter(|&id| t.flags[id].contains(NodeFlags::DIR))
    }

    /// Adds `s` under its parent, or parks it until the parent is placed. Placing a directory
    /// releases its waiting children. Returns the number of nodes added.
    fn place(&mut self, t: &mut Tree, s: FileState) -> usize {
        let mut added = 0;
        let mut work = vec![s];
        while let Some(s) = work.pop() {
            let Some(parent) = self.is_dir_node(t, s.parent) else {
                self.parked_count += 1;
                self.parked.entry(s.parent).or_default().push(s);
                continue;
            };
            let Ok(record) = usize::try_from(s.record) else { continue };
            if self.node(s.record).is_some() {
                continue; // a record is placed once
            }
            let mut size = if s.is_dir { 0 } else { s.size };
            if size == 0
                && !s.is_dir
                && let Some(ext) = self.sizes.remove(&s.record)
            {
                size = ext;
            }
            let mut flags = NodeFlags::empty();
            flags.set(NodeFlags::DIR, s.is_dir);
            flags.set(NodeFlags::REPARSE, s.reparse);
            let id = t.add_child(parent, &s.name, size, flags);
            if self.records.len() <= record {
                self.records.resize(record + 1, NodeId::NONE);
            }
            self.records[record] = id;
            added += 1;
            if s.is_dir
                && let Some(kids) = self.parked.remove(&s.record)
            {
                self.parked_count -= kids.len();
                work.extend(kids);
            }
        }
        added
    }
}

#[cfg(test)]
mod tests;
```

Note: `place` counts parking as no change, so a tick with only parked records reports 0 changes.

- [ ] **Step 3: Tests.** Create `crates/plumb/src/preview/tests.rs` (check `NodeId`, `NodeFlags`, `Tree::path`, `plumb_core::synth` helpers as you go):

```rust
use super::*;
use plumb_scan::journal::state::FileState;
use plumb_scan::preview::{channel, PreviewMsg};
use std::collections::BTreeMap;
use std::time::Duration;

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn state(record: u64, parent: u64, name: &str, size: u64, is_dir: bool) -> FileState {
    FileState { record, sequence: 1, parent, name: name.encode_utf16().collect(), size, is_dir, reparse: false, links: 1 }
}

/// path -> (size, is_dir) for every node but the root's own name.
fn listing(t: &Tree) -> BTreeMap<String, (u64, bool)> {
    let root = t.roots[0];
    let mut out = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.size[n], t.flags[n].contains(NodeFlags::DIR)));
        stack.extend(t.children(n));
    }
    out
}

/// A deterministic pseudo-random sequence (no dependency).
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

#[test]
fn the_walker_replay_equals_the_builder_tree() {
    let (tx, rx) = channel(1 << 16);
    let mut b = TreeBuilder::new();
    let mut seed = 7;
    let push = |b: &mut TreeBuilder, parent: NodeId, name: &str, size: u64, flags: NodeFlags| {
        let units: Vec<u16> = name.encode_utf16().collect();
        tx.send(PreviewMsg::Add { parent, name: units.clone(), size, flags });
        b.push(parent, &units, size, flags)
    };
    let root = push(&mut b, NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let mut dirs = vec![root];
    for i in 0..500u32 {
        let parent = dirs[lcg(&mut seed) as usize % dirs.len()];
        if lcg(&mut seed) % 4 == 0 {
            dirs.push(push(&mut b, parent, &format!("d{i}"), 0, NodeFlags::DIR));
        } else {
            push(&mut b, parent, &format!("f{i}"), lcg(&mut seed) % 5000, NodeFlags::empty());
        }
    }
    let mut p = Preview::new(rx);
    let mut tree = None;
    let a = p.apply(&mut tree, far());
    assert!(a.created);
    let (got, want) = (tree.unwrap(), b.finish().unwrap());
    assert_eq!(got.len(), want.len());
    for i in 0..want.len() as u32 {
        let id = NodeId(i);
        assert_eq!((got.parent[id], got.name(id), got.size[id], got.flags[id]), (want.parent[id], want.name(id), want.size[id], want.flags[id]));
    }
    assert_eq!(got.subtree_size[NodeId(0)], want.subtree_size[NodeId(0)]);
}

#[test]
fn mft_records_in_any_order_build_the_same_tree() {
    // 40 directories and 200 files under record 5, with parents of every record before or after it.
    let mut seed = 99;
    let mut states = Vec::new();
    let mut dirs = vec![5u64];
    for r in 6..246u64 {
        let parent = dirs[lcg(&mut seed) as usize % dirs.len()];
        let is_dir = r < 46;
        states.push(state(r, parent, &format!("n{r}"), if is_dir { 0 } else { lcg(&mut seed) % 9000 }, is_dir));
        if is_dir {
            dirs.push(r);
        }
    }
    let mut results = Vec::new();
    for round in 0..4 {
        let mut order = states.clone();
        for i in (1..order.len()).rev() {
            order.swap(i, lcg(&mut seed) as usize % (i + 1));
        }
        if round == 0 {
            order.sort_by_key(|s| s.record);
        }
        let (tx, rx) = channel(1 << 12);
        tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 300 });
        for s in order {
            tx.send(PreviewMsg::Upsert(s));
        }
        let (mut p, mut tree) = (Preview::new(rx), None);
        p.apply(&mut tree, far());
        let t = tree.unwrap();
        assert_eq!(t.len(), 241, "round {round}: every record placed");
        results.push(listing(&t));
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(results[0].len(), 241);
}

#[test]
fn a_parent_that_is_a_file_keeps_its_child_parked() {
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "file", 10, false)));
    tx.send(PreviewMsg::Upsert(state(7, 6, "child", 10, false)));
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 2, "root and the file; the child waits");
}

#[test]
fn an_extension_size_applies_whether_it_arrives_before_or_after_the_base() {
    for before in [true, false] {
        let (tx, rx) = channel(64);
        tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
        if before {
            tx.send(PreviewMsg::Size { record: 6, size: 4096 });
        }
        tx.send(PreviewMsg::Upsert(state(6, 5, "big", 0, false)));
        if !before {
            tx.send(PreviewMsg::Size { record: 6, size: 4096 });
        }
        let (mut p, mut tree) = (Preview::new(rx), None);
        p.apply(&mut tree, far());
        let t = tree.unwrap();
        assert_eq!(t.subtree_size[NodeId(0)], 4096, "before = {before}");
    }
}

#[test]
fn reset_drops_the_tree_and_a_new_scanner_starts_clean() {
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    tx.send(PreviewMsg::Reset);
    tx.send(PreviewMsg::Add { parent: NodeId::NONE, name: "C:".encode_utf16().collect(), size: 0, flags: NodeFlags::DIR });
    let (mut p, mut tree) = (Preview::new(rx), None);
    let a = p.apply(&mut tree, far());
    assert!(a.created && a.reset, "the old tree was dropped and the walker's root created");
    assert_eq!(tree.unwrap().len(), 1);
}

#[test]
fn a_reset_unfreezes_a_frozen_preview() {
    // A file parent keeps the child parked; MAX_PARKED is too big to hit, so freeze by an
    // out-of-step walker id instead.
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Add { parent: NodeId::NONE, name: "C:".encode_utf16().collect(), size: 0, flags: NodeFlags::DIR });
    tx.send(PreviewMsg::Add { parent: NodeId(9), name: "x".encode_utf16().collect(), size: 1, flags: NodeFlags::empty() });
    tx.send(PreviewMsg::Add { parent: NodeId(0), name: "after".encode_utf16().collect(), size: 1, flags: NodeFlags::empty() });
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, far());
    assert_eq!(tree.as_ref().unwrap().len(), 1, "frozen at the bad id; the later add is ignored");
    assert!(p.paused());
    tx.send(PreviewMsg::Reset);
    tx.send(PreviewMsg::Begin { root: "D:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 2);
    assert!(!p.paused());
}

#[test]
fn a_lagged_sender_pauses_the_preview_after_the_queue_drains() {
    let (tx, rx) = channel(2);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    tx.send(PreviewMsg::Upsert(state(7, 5, "b", 1, false))); // dropped
    let (mut p, mut tree) = (Preview::new(rx), None);
    assert!(!p.paused());
    p.apply(&mut tree, far());
    assert!(p.paused());
    assert_eq!(tree.unwrap().len(), 2);
}

#[test]
fn the_time_budget_stops_a_long_batch() {
    let (tx, rx) = channel(1 << 14);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10_000 });
    for r in 6..6 + 5000u64 {
        tx.send(PreviewMsg::Upsert(state(r, 5, "x", 1, false)));
    }
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, Instant::now()); // already past: stops at the first clock look
    let first = tree.as_ref().unwrap().len();
    assert!(first < 5001, "stopped early, had {first}");
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 5001);
}
```

- [ ] **Step 4: Run** `cargo test -p plumb-core -j 8 reserve` and `cargo test -p plumb -j 8 --lib preview`. Fix by tracing against the real types (for example, `TreeBuilder::push` returning `NodeId`; `tree.roots[0]`; `lcg` closure borrow of `tx` in the first test may need `tx` as a reference). Expected: all pass.

- [ ] **Step 5: Commit** `git add crates && git commit -m "Add Tree::reserve and the preview applier"`.

---

### Task 6: Cheaper `RowModel::refresh` for long touched lists

**Files:**
- Modify: `crates/plumb-core/src/rows.rs`
- Test: `crates/plumb-core/src/rows/refresh_tests.rs` (or wherever `refresh_matches_a_fresh_model` lives; grep for it)

**Interfaces:**
- Produces: `refresh` clears the sort cache outright when more than 4096 nodes were touched. Behavior is otherwise identical.

- [ ] **Step 1: Guard test** in `rows/refresh_tests.rs`, next to `refresh_matches_a_fresh_model_after_random_mutations` (read it and reuse its `expand_all` and comparison code). Add a sibling that builds `kary_builder(300, 3)`, sorts, expands all, then adds 5000 files under assorted directories with `add_child`, calls `refresh`, and asserts the rows equal those of a model built from scratch with the same sort and expansion. It passes before the change too (the change is an optimisation), so it is a guard; run it after Step 2 as well.

- [ ] **Step 2: The change.** In `refresh`, replace the `for t in touched { ... }` loop with:

```rust
        // A long list (a scan being previewed) costs more to walk, ancestor by ancestor, than the
        // sorted children lists cost to rebuild, so drop them all.
        if touched.len() > 4096 {
            self.cache.clear();
        } else {
            for t in touched {
                self.cache.remove(&t);
                let mut cur = t;
                while !cur.is_none() {
                    cur = tree.parent[cur];
                    self.cache.remove(&cur);
                }
            }
        }
```

- [ ] **Step 3:** `cargo test -p plumb-core -j 8` (all pass), commit `git commit -am "Clear the sort cache outright on a long touched list"`.

---

### Task 7: `App` support

**Files:**
- Modify: `crates/plumb/src/app.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `App::preview_installed(&mut self)`: `self.tree` was just created or dropped by the preview applier. Discards the tree's touched log, builds the model (sort, root expanded and selected) or clears it if the tree is `None`, resets scroll, `filter_reset`. One method for both, so a `Reset` followed by a new root in the same slice cannot wipe the new tree.
  - `pub preview_paused: bool`: the status line adds "preview paused".
  - `swap_tree` sets `scan = Idle`; `fail_scan` clears tree and model.
  - `status()` shows the running scan message instead of the summary while `Scan::Running` (the message line is drawn only when no tree is on screen, `draw.rs:261`).

- [ ] **Step 1: Tests** (append to `app.rs` tests; `scanned()` and `app()` exist):

```rust
    #[test]
    fn a_preview_tree_is_browsable_and_the_final_tree_keeps_the_view() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(60, 3).finish().unwrap());
        a.preview_installed();
        assert!(matches!(a.scan, Scan::Running { .. }), "the scan is still running");
        let rows = a.model.as_ref().unwrap().rows().len();
        assert!(rows > 1, "the root is expanded");
        assert!(a.status().starts_with("Scanning"));
        // The user selects another row, then the exact tree (the same shape) arrives.
        let second = a.model.as_ref().unwrap().rows()[1].id;
        a.model.as_mut().unwrap().select(second);
        let name = a.tree.as_ref().unwrap().path(second);
        a.swap_tree(kary_builder(60, 3).finish().unwrap(), "done".into());
        assert!(matches!(a.scan, Scan::Idle));
        let sel = a.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.tree.as_ref().unwrap().path(sel), name, "the selection survives by path");
        assert!(a.status().starts_with("done"));
    }

    #[test]
    fn a_failed_scan_drops_the_preview() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.fail_scan("boom".into());
        assert!(a.tree.is_none() && a.model.is_none());
    }

    #[test]
    fn a_dropped_preview_tree_clears_the_view_but_not_the_scan() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.tree = None; // what the applier does on a Reset
        a.preview_installed();
        assert!(a.model.is_none());
        assert!(matches!(a.scan, Scan::Running { .. }));
    }
```

(Use whatever the model's selected-node accessor is called; grep `pub fn selected` in `rows.rs`. Adjust the path-equality assertion if `kary_builder(60,3)` and `(80,3)` paths differ in naming; the first rows' names are `n1`, `n2`, ... by index, so they match.)

- [ ] **Step 2: Implement.** In `app.rs`:
  - Add `pub preview_paused: bool` to `App` (init `false`) and reset it in `begin_scan`.
  - Add, after `finish_scan`:

```rust
    /// The preview applier just created `self.tree` or dropped it (a scanner fell back to
    /// another): build the rows for the tree, or show nothing. The scan state is untouched, so
    /// the progress line keeps counting.
    pub fn preview_installed(&mut self) {
        self.filter_reset();
        self.scroll = 0;
        self.model = None;
        let Some(tree) = &mut self.tree else { return };
        tree.take_touched(); // the model is built from the tree as it is now
        let mut model = RowModel::new(tree);
        model.set_sort(tree, sort_key(self.sort.0), self.sort.1);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(tree, r);
            model.select(r);
        }
        self.model = Some(model);
    }
```

  - In `swap_tree`, before `self.set_live(LiveStatus::Off)` at its end add `self.scan = Scan::Idle;` and `self.preview_paused = false;`; in `finish_scan` also `self.preview_paused = false;`.
  - In `fail_scan`: `self.tree = None; self.model = None; self.preview_paused = false;` before setting `Failed`.
  - In `status()`: replace the first element with

```rust
        let head = match &self.scan {
            Scan::Running { .. } => {
                let paused = if self.preview_paused { "   (preview paused)" } else { "" };
                format!("{}{paused}", self.message())
            }
            _ if self.summary.is_empty() => "plumb".to_string(),
            _ => self.summary.clone(),
        };
        let mut parts = vec![head];
```

- [ ] **Step 3:** `cargo test -p plumb -j 8 --lib` (all pass; update any test that asserted the old `status()` text during a scan), commit `git commit -am "App: preview install, drop, and status"`.

---

### Task 8: Window wiring and selftest

**Files:**
- Modify: `crates/plumb/src/window.rs`

**Interfaces:**
- Consumes: Tasks 1, 5, 7.
- Produces: a 15 ms preview timer during a foreground scan; row refreshes paced by `Pacer`; the exact tree swapped in at the end.

- [ ] **Step 1: Constants and fields.**
  - Imports: `use crate::preview::Preview;`, `use plumb_scan::preview::channel;`.
  - Constants next to `TIMER_FILTER`: `const TIMER_PREVIEW: usize = 4;`, `const PREVIEW_TICK_MS: u32 = 15;`, `const PREVIEW_QUEUE: usize = 200_000;`.
  - `Win` fields: `preview: Option<Preview>`, `preview_pacer: Pacer`, `preview_dirty: bool`; initialised to `None`, `Pacer::new(Instant::now())`, `false` in `Win::new`.

- [ ] **Step 2: Start and tick.** In `spawn_scan`, replace `self.progress = Arc::default();` with:

```rust
        self.progress = if background {
            self.preview = None;
            Arc::default()
        } else {
            let (tx, rx) = channel(PREVIEW_QUEUE);
            self.preview = Some(Preview::new(rx));
            self.preview_pacer = Pacer::new(Instant::now());
            self.preview_dirty = false;
            Arc::new(Progress::with_preview(tx))
        };
```

(add `use plumb_scan::Progress;` if it is not already imported in that exact form; `Arc<Progress>` is the field type). In the `if !background { SetTimer(... TIMER_PROGRESS ...) }` block add `SetTimer(Some(self.hwnd), TIMER_PREVIEW, PREVIEW_TICK_MS, None)`. Add the method next to `poll_progress`:

```rust
    /// `TIMER_PREVIEW`: applies some of what the scan has found, and at the live-update pace
    /// refreshes the rows.
    fn preview_tick(&mut self) {
        let Some(p) = &mut self.preview else { return };
        let a = p.apply(&mut self.app.tree, Instant::now() + APPLY_BUDGET);
        if a.reset || a.created {
            self.app.preview_installed(); // builds the model from whatever tree there is now
        } else {
            self.preview_dirty |= a.changes > 0;
        }
        self.app.preview_paused = p.paused();
        if self.preview_dirty && self.preview_pacer.due(Instant::now()) {
            let t0 = Instant::now();
            self.app.refresh_rows();
            self.preview_pacer.done(Instant::now(), t0.elapsed());
            self.preview_dirty = false;
        }
        self.invalidate();
    }
```

(After a refresh the `touched` log is empty; after an install it was discarded, so the log never outgrows one pacing interval.)

- [ ] **Step 3: End of scan.** In `scan_done`, after `let background = ...;` add:

```rust
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_PREVIEW) };
        self.preview = None; // anything still queued is discarded; the exact tree is here
```

and change the success arm's branch to `if self.app.tree.is_some() { self.app.swap_tree(sc.tree, s); self.drop_watch(); } else { self.app.finish_scan(sc.tree, s); }`. `begin_scan` clears the tree, so on a foreground scan a tree on screen can only be the preview (and a background rescan always has one); with no preview yet, `finish_scan` runs as today. The error arm `Err(e) => self.app.fail_scan(e)` already clears the preview via Task 7.

- [ ] **Step 4: Dispatch and selftest.** Add to `wndproc` before the `TIMER_PROGRESS` arm: `WM_TIMER if wp.0 == TIMER_PREVIEW => { with(|w| w.preview_tick()); LRESULT(0) }`. In `selftest` change the first `pump_until` condition to `matches!(w.app.scan, crate::app::Scan::Idle | crate::app::Scan::Failed(_))` and keep the rest (the selftest's `spawn_scan(0, dir, false)` now attaches a sender, so the preview path runs; the filter assertions then check the final tree). `selftest_live` has the same wait loop; apply the same condition change there.

- [ ] **Step 5: Verify.** `cargo test -p plumb -j 8 -- --include-ignored` (render and selftest pass), `cargo clippy -p plumb --all-targets -j 8` (no new warnings in the files touched). Commit `git commit -am "Wire the partial tree into the window"`.

---

### Task 9: Docs, final checks, hand-off

**Files:**
- Modify: `CHANGELOG.md`, `docs/superpowers/specs/2026-10-12-plumb-partial-tree-design.md`, `docs/benchmarks.md` (only if re-measured)

- [ ] **Step 1: Changelog.** Under `[Unreleased]` / `### Added`:

```markdown
- `plumb`: partial tree while a scan runs. The tree appears within a second and folders fill in as they are found; sorting, expanding, selecting and the filter all work during the scan. When the scan ends the exact tree replaces the preview and your expansion, selection and scroll are kept. Works for both scanners. If the window falls too far behind the scan the status line says "preview paused" and the view stops changing until the exact tree arrives; the scan is not slowed.
- `plumb-scan`: `Progress` can carry a preview channel (`preview` module); `journal::state::file_state` shares the record-to-file rule with the journal reader.
- `plumb-core`: `Tree::reserve`; `RowModel::refresh` drops its sort cache outright when more than 4096 nodes changed.
```

and in Known limitations: "The preview is approximate: only the first name of a hard-linked file appears, and records whose parent never arrives are not shown. The exact tree replaces it at the end."; remove "No partial tree while a scan runs" if it is still listed under an older release's limitations only as history (leave history alone; add nothing there).

- [ ] **Step 2: Spec.** Set `Status: implemented, 2026-10-12.`; add the "Spec amendments" list from the top of this plan under `## Amendments made while implementing`; fix the Slow-consumer and Live sections to say the MFT preview has its own applier and `Live` is unchanged.

- [ ] **Step 3: Verification.** `cargo test --workspace -j 8`; `cargo test -p plumb -j 8 -- --include-ignored`; `cargo clippy --workspace --all-targets -j 8` (report only new warnings); `git diff --stat 2eaa21c` to confirm only the planned files changed; grep: `grep -rn "TODO\|todo!" crates/plumb/src/preview.rs crates/plumb-scan/src/preview.rs`.

- [ ] **Step 4: Commit** `git add CHANGELOG.md docs && git commit -m "Document the partial tree and the plan's amendments"`.

- [ ] **Step 5: Manual check for the user.** (a) Without admin, scan a large folder drive: rows appear within about a second, sizes grow, sort by Size works while scanning, Ctrl+F filters the half-built tree, the window stays responsive; at the end the summary line returns and your selected folder is still selected. (b) Elevated, scan `C:`: the tree appears and fills quickly; the final total equals the progress counter; the summary's elapsed time stays near the 3.2 s baseline (the emit work on the parser threads must not slow the scan; if it does, I measure with and without the sender); note if "preview paused" ever shows (if it does on a busy UI, `PREVIEW_QUEUE` is the knob: 200k messages is about 150 ms of MFT scan). (c) Report the worst stall you feel; if the window hitches, I measure the refresh (the first suspect is `RowModel::refresh` on a very large folder).

---

## Self-review

- **Spec coverage.** Walker preview (Task 3, 5), MFT preview with extension sizes (Tasks 2, 4, 5), bounded sticky queue and `lagged` (Task 1), `Reset` on fallback (Tasks 3, 5, 8), root and capacity (Task 5), parked cap and directory-only parent (Task 5), timers and `Pacer` pacing (Task 8), `swap_tree` at the end and `Scan::Idle` handling (Tasks 7, 8), status "preview paused" (Task 7), filter reset on install (Task 7), `pump_until` change (Task 8), tests for walker equality, MFT order independence, extension sizes, slow consumer, reset, app swap (Tasks 1, 3-5, 7), docs (Task 9). The spec's Live changes are replaced by amendment 1.
- **Placeholders.** None: every step carries code or an exact edit; the few "check X in the real code" notes name what to look at (the `base` value type in Task 4, the fixture helpers in Task 3, the selected-node accessor in Task 7).
- **Types.** `PreviewMsg`, `PreviewTx`, `PreviewRx`, `channel`, `Progress::{with_preview, emit}` (Task 1) are used with those names in Tasks 3, 4, 5, 8; `Preview::{new, apply, paused}` and `Applied { changes, created, reset }` (Task 5) match their uses in Task 8; `App::{preview_installed, preview_paused}` (Task 7) match Task 8.

## Stress-test log

Round 1 (against the real code) found and fixed: a `Reset` while frozen never unfroze the applier (and a frozen drain ignored the deadline); `preview_dropped` after a same-slice `Reset` + new root would have wiped the new tree (now one `preview_installed` that handles a missing tree); the reset test asserted the wrong flag; the status line would have lost the scan counts (the message line is drawn only without a tree, `draw.rs:261`); Task 3/4 tests used guessed helper names and a wrong thread-count constructor (now the real `fixture`, `WalkScanner { threads }`, `image()`, record set 0/30-34 and the hard-link carrier `link2`); the app test compared paths across differently shaped trees; `scan_done` can test `app.tree.is_some()` because `begin_scan` clears the tree; `Begin` now preallocates `records`; unused `mut` closure; the touched log is discarded on install.
Round 2 (re-read each task against the types it consumes) found only the stale `preview_dropped` mention in the Types check, fixed. Open items left to measurement, not guesses: queue size under a busy UI, `filter_live` cost with a 300k touched list while a filter is active, emit cost on the parser threads (Task 9 manual check).
