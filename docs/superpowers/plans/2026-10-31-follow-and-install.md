---
status: done
---

# One live-source trait, one pacer, one tree-install tail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the duplicated pacing loop, the `Source` enum matches and the repeated install blocks (cleanup A7, A8) without changing behavior.

**Architecture:** `Paced` (live.rs) replaces the pacer-plus-dirty pairs of `Watch` and `Win`'s preview fields; a `Follow: Send` trait with `JournalFollow` and `DirFollow` replaces `enum Source`; `App::fresh_model` and `App::settle` replace the repeated blocks in `finish_scan`, `preview_installed` and `swap_tree`.

**Tech Stack:** Rust 2024 workspace, crate `plumb` (`crates/plumb/src`), tests with `cargo test`.

**Spec:** `docs/superpowers/specs/2026-10-31-follow-and-install-design.md`

## Global Constraints

- No behavior change: `cargo test --workspace --locked` stays green (414 tests before, plus the new ones); `cargo clippy --workspace --all-targets` clean.
- `--selftest-live` (needs elevation) is run by the user at the end; sessions cannot.
- One concept per commit; commit message style is short and topical, ending with the `Co-Authored-By` trailer from the session reminder.
- Never push, tag or bump.
- Tasks 1, 2 and 3 touch disjoint code and may land in any order; the order below is the recommended one.

## Review Focus

- A view with a running preview that is switched away and back: its pacer resets, so the first refresh after returning is not delayed (Task 1).
- `drop_watch` moves the `Watch` to a thread: the trait object must be `Send` (Task 2; compile error otherwise).
- A first-scan finish after a background rescan flag was left set clears `refreshing` (Task 3 test).
- `preview_installed` must not touch `scan`, `summary`, `live` or `preview_paused` (Task 3 test).

---

### Task 1: `Paced`

**Files:**
- Modify: `crates/plumb/src/live.rs` (add `Paced` after `Pacer`)
- Modify: `crates/plumb/src/live/tests.rs` (test)
- Modify: `crates/plumb/src/preview.rs` (`Preview` gets `pub paced: Paced`)
- Modify: `crates/plumb/src/window.rs` (delete `preview_pacer`/`preview_dirty`, fix their uses, import)
- Modify: `crates/plumb/src/window/watch.rs` (`Watch` uses `paced`)

**Interfaces:**
- Produces: `live::Paced { new(now), mark(bool), dirty(), due(now), done(now, took), reset(now) }`.

- [ ] **Step 1: Write the failing test** in `live/tests.rs`, after `the_pacer_allows_four_refreshes...`:

```rust
#[test]
fn paced_is_due_only_when_marked_and_the_pacer_allows() {
    let t0 = Instant::now();
    let mut p = Paced::new(t0);
    assert!(!p.dirty() && !p.due(t0), "nothing changed, nothing to refresh");
    p.mark(false);
    assert!(!p.dirty());
    p.mark(true);
    assert!(p.dirty() && p.due(t0));
    p.done(t0, Duration::from_millis(10));
    assert!(!p.dirty(), "done clears dirty");
    p.mark(true);
    assert!(!p.due(t0 + Duration::from_millis(249)) && p.due(t0 + Duration::from_millis(250)));
    p.mark(false);
    assert!(p.dirty(), "an empty batch does not clear dirty");
    p.reset(t0);
    assert!(!p.dirty() && !p.due(t0));
}
```

- [ ] **Step 2: Run it:** `cargo test -p plumb paced_is_due` — Expected: FAIL to compile, `Paced` not found.

- [ ] **Step 3: Add `Paced`** to `live.rs` after `impl Pacer { ... }`:

```rust
/// A `Pacer` plus "the tree changed since the rows were last refreshed", as the live watch and the
/// scan preview both keep them.
pub struct Paced {
    pacer: Pacer,
    dirty: bool,
}

impl Paced {
    pub fn new(now: Instant) -> Self {
        Self { pacer: Pacer::new(now), dirty: false }
    }

    pub fn mark(&mut self, changed: bool) {
        self.dirty |= changed;
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Rows need a refresh and the pace allows it.
    pub fn due(&self, now: Instant) -> bool {
        self.dirty && self.pacer.due(now)
    }

    /// Call after a refresh that finished at `now` and took `took`.
    pub fn done(&mut self, now: Instant, took: Duration) {
        self.pacer.done(now, took);
        self.dirty = false;
    }

    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }
}
```

- [ ] **Step 4: Run the test** — Expected: PASS.

- [ ] **Step 5: Adopt it in `Preview` and `Win`.**
  - `preview.rs`: add a doc comment and `pub paced: Paced,` to `struct Preview`; `Preview::new` sets `paced: Paced::new(Instant::now())`; change the import `use crate::live::Records;` to `use crate::live::{Paced, Records};`.
  - `window.rs`: delete the fields `preview_pacer` and `preview_dirty` (with their doc comment) and their initialiser at `Win` construction. Change the import to `use crate::live::{needs_compaction, Live, Paced, RescanPolicy, REPAINT};` (`Paced` is used by `watch.rs` through `use super::*`; drop `Pacer` if nothing else uses it).
  - `unpark`: inside the `if self.run.preview.is_some()` block replace the two lines `self.preview_pacer = ...; self.preview_dirty = false;` with `if let Some(p) = &mut self.run.preview { p.paced.reset(Instant::now()); }`.
  - `start_scan` (the `else` branch that creates `Preview::new(rx)`): delete the two reset lines (`Preview::new` is fresh).
  - `preview_tick`: replace the `preview_dirty`/`preview_pacer` lines with
    ```rust
    if a.reset || a.created {
        self.app.preview_installed(); // builds the model from whatever tree there is now
    } else {
        p.paced.mark(a.changes > 0);
    }
    self.app.slot.preview_paused = p.paused();
    if p.paced.due(Instant::now()) {
        let t0 = Instant::now();
        self.app.refresh_rows();
        p.paced.done(Instant::now(), t0.elapsed());
    }
    self.invalidate();
    ```
  - `watch.rs`: `Watch` fields `pacer: Pacer` and `dirty: bool` become `paced: Paced`; both constructors set `paced: Paced::new(Instant::now())`; `apply_queued`: `w.paced.mark(n > 0)`; `apply_live`: `(w.paced.dirty() || w.backlog())`; `live_tick`: `let due = ... .is_some_and(|w| w.paced.due(Instant::now()));`, inside the `due` block `w.paced.done(Instant::now(), t0.elapsed());` (delete `w.dirty = false`), and the disarm test `&& !w.paced.dirty()`.

- [ ] **Step 6: Verify:** `cargo test --workspace --locked` — Expected: all pass (414 + 1); `cargo clippy --workspace --all-targets` — no warnings; `grep -rn "preview_pacer\|preview_dirty\|\.pacer\b" crates` — no matches.

- [ ] **Step 7: Commit:** `git add -A crates && git commit -m "Live, preview: one Paced for the dirty flag and the pacer"` (with the trailer).

### Task 2: `Follow`

**Files:**
- Modify: `crates/plumb/src/window/watch.rs` (delete `enum Source`, add trait and two impls, `Watch.src: Box<dyn Follow>`, forwarding methods, two constructors)

**Interfaces:**
- Consumes: `Watch { src, paced, armed }` from Task 1.
- Produces: `trait Follow: Send` with `drain`, `apply`, `queued`, `backlog` (default `queued`), `stop(self: Box<Self>)`; `Watch` keeps `drain`, `apply`, `queued`, `backlog`, `stop` as one-line forwards so callers are unchanged.

- [ ] **Step 1: Write the failing test** at the end of `window/watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        queue: usize,
        pending: bool,
        stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Follow for Fake {
        fn drain(&mut self) -> Option<Ended> {
            self.queue += 2;
            None
        }
        fn apply(&mut self, _: &mut Tree, _: Instant) -> usize {
            std::mem::take(&mut self.queue)
        }
        fn queued(&self) -> bool {
            self.queue > 0
        }
        fn backlog(&self) -> bool {
            self.queued() || self.pending
        }
        fn stop(self: Box<Self>) {
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_watch_forwards_to_its_source() {
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut w = Watch {
            src: Box::new(Fake { queue: 0, pending: true, stopped: stopped.clone() }),
            paced: Paced::new(Instant::now()),
            armed: false,
        };
        assert!(!w.queued() && w.backlog(), "pending work is a backlog even with an empty queue");
        assert!(w.drain().is_none() && w.queued());
        let mut tree = plumb_core::TreeBuilder::new();
        tree.push_str(plumb_core::NodeId::NONE, "C:", 0, plumb_core::NodeFlags::DIR);
        let mut tree = tree.finish().unwrap();
        assert_eq!(w.apply(&mut tree, Instant::now()), 2);
        w.stop();
        assert!(stopped.load(Ordering::SeqCst));
    }
}
```

(`Ordering` is `std::sync::atomic::Ordering`, imported in `window.rs` and reached through `super::*`. The names are `JournalFollow` and `DirFollow` because `window.rs` already imports `plumb_scan::journal::JournalSource`.)

- [ ] **Step 2: Run it:** `cargo test -p plumb a_watch_forwards` — Expected: FAIL to compile, `Follow` not found.

- [ ] **Step 3: Implement.** In `watch.rs`:
  - Replace `pub(super) enum Source { ... }` with:
    ```rust
    /// What a live watch follows: the change journal or the directory watcher.
    pub(super) trait Follow: Send {
        /// Moves what the source has queued into its queue; `Some` if the source has ended.
        fn drain(&mut self) -> Option<Ended>;
        /// Applies queued changes until `deadline`; returns how many tree changes were made.
        fn apply(&mut self, tree: &mut Tree, deadline: Instant) -> usize;
        fn queued(&self) -> bool;
        /// Work left that a timer tick should finish.
        fn backlog(&self) -> bool {
            self.queued()
        }
        /// Stops the source (blocks briefly: call off the UI thread).
        fn stop(self: Box<Self>);
    }

    struct JournalFollow {
        reader: JournalReader,
        live: Live,
        queue: VecDeque<Delta>,
    }

    struct DirFollow {
        watcher: Watcher,
        queue: VecDeque<Listing>,
    }
    ```
  - Move the `Source::Journal` arms of the old `drain`, `apply`, `queued`, `backlog` (`self.queued() || self.live.pending()`) and `stop` (`self.reader.stop()`) bodies into `impl Follow for JournalFollow`, and the `Source::Dir` arms into `impl Follow for DirFollow` (`apply` is `crate::dirsync::apply(tree, &mut self.queue, deadline)`, `stop` is `self.watcher.stop()`). The match arms are copied unchanged; only `queue` and `reader` become `self.queue` and `self.reader`.
  - `Watch.src` becomes `Box<dyn Follow>`; its methods become `self.src.drain()`, `self.src.apply(tree, deadline)`, `self.src.queued()`, `self.src.backlog()`, `self.src.stop()`.
  - `start_watch`: `src: Box::new(JournalFollow { reader, live: Live::new(records), queue: VecDeque::new() })`; `install_dir_watch`: `src: Box::new(DirFollow { watcher, queue: VecDeque::new() })`.

- [ ] **Step 4: Verify:** `cargo test --workspace --locked` — Expected: all pass (the new test included); `cargo clippy --workspace --all-targets` clean; `grep -rn "Source::" crates/plumb/src` — no matches.

- [ ] **Step 5: Commit:** `git add -A crates && git commit -m "Watch: a Follow trait for the journal and the directory watcher replaces the Source enum"` (with the trailer).

### Task 3: `fresh_model` and `settle`

**Files:**
- Modify: `crates/plumb/src/app.rs` (`finish_scan`, `preview_installed`, `swap_tree`, two new private fns, tests next to `a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row`)

**Interfaces:**
- Produces: `App::fresh_model(tree: &Tree, key: SortKey, desc: bool) -> RowModel` (associated fn), `App::settle(&mut self, summary: String)` (private).

- [ ] **Step 1: Write the failing tests** in the `app.rs` test module (use the helpers the neighbouring tests use: `scanned()`, `kary_builder`):

```rust
#[test]
fn finishing_a_scan_clears_every_scan_flag() {
    let mut a = scanned();
    a.slot.refreshing = true;
    a.slot.preview_paused = true;
    a.set_live(LiveStatus::On { last: None });
    a.finish_scan(kary_builder(10, 3).finish().unwrap(), "done".into());
    assert!(matches!(a.slot.scan, Scan::Idle) && !a.slot.refreshing && !a.slot.preview_paused);
    assert!(a.slot.live == LiveStatus::Off && a.slot.summary == "done");
}

#[test]
fn installing_a_preview_tree_leaves_the_scan_state_alone() {
    let mut a = scanned();
    a.begin_scan(0);
    a.slot.summary = "kept".into();
    a.slot.preview_paused = true;
    a.slot.tree = Some(kary_builder(10, 3).finish().unwrap());
    a.preview_installed();
    assert!(matches!(a.slot.scan, Scan::Running { .. }) && a.slot.preview_paused && a.slot.summary == "kept");
    assert!(a.slot.model.is_some() && a.slot.model.as_ref().and_then(|m| m.selected()).is_some());
}

#[test]
fn swapping_in_a_tree_with_nothing_to_keep_settles_like_a_finish() {
    let mut a = scanned();
    a.slot.tree = None;
    a.slot.model = None;
    a.slot.refreshing = true;
    a.swap_tree(kary_builder(10, 3).finish().unwrap(), "again".into());
    assert!(matches!(a.slot.scan, Scan::Idle) && !a.slot.refreshing && a.slot.summary == "again");
    assert!(a.slot.model.is_some());
}
```

- [ ] **Step 2: Run them:** `cargo test -p plumb finishing_a_scan_clears installing_a_preview swapping_in_a_tree` — Expected: the first FAILS (`finish_scan` leaves `refreshing` set); the other two pass against today's code (they pin behavior the refactor must keep). Run each name separately if the filter takes one pattern.

- [ ] **Step 3: Implement.** In `app.rs`, add near `finish_scan`:

```rust
/// A model for `tree` in the given sort, with the root expanded and selected so there is something to see.
fn fresh_model(tree: &Tree, key: SortKey, desc: bool) -> RowModel {
    let mut model = RowModel::new(tree);
    model.set_sort(tree, key, desc);
    if let Some(r) = model.rows().first().map(|r| r.id) {
        model.expand(tree, r);
        model.select(r);
    }
    model
}

/// After an install that ends a scan or a rescan: nothing is running, no preview is paused, live is off.
fn settle(&mut self, summary: String) {
    self.slot.summary = summary;
    self.slot.scan = Scan::Idle;
    self.slot.refreshing = false;
    self.slot.preview_paused = false;
    self.set_live(LiveStatus::Off);
}
```

and rewrite the three callers:

```rust
pub fn finish_scan(&mut self, tree: Tree, summary: String) {
    self.filter_reset();
    self.map_reset();
    let model = Self::fresh_model(&tree, self.kind().sort_key(self.sort.0), self.sort.1);
    self.slot.tree = Some(tree);
    self.slot.model = Some(model);
    self.slot.scroll = 0;
    self.settle(summary);
}

pub fn preview_installed(&mut self) {
    self.filter_reset();
    self.map_reset();
    self.slot.scroll = 0;
    self.slot.model = None;
    let key = self.kind().sort_key(self.sort.0);
    let Some(tree) = &mut self.slot.tree else { return };
    tree.take_touched(); // the model is built from the tree as it is now
    self.slot.model = Some(Self::fresh_model(tree, key, self.sort.1));
}
```

`swap_tree` keeps its body up to the snapshot apply; its tail (`self.slot.summary = summary; self.slot.refreshing = false; self.slot.scan = Scan::Idle; self.slot.preview_paused = false; self.filter_reset(); self.set_live(LiveStatus::Off);`) becomes `self.filter_reset(); self.settle(summary);` (`filter_reset` stays before `settle`, as today it runs before `set_live`). Keep the doc comments on the three functions. If `self.sort.1` or `self.kind()` borrows conflict with the `&mut self.slot.tree` borrow in `preview_installed`, copy them into locals before the `let Some(tree)` line, as `key` already is.

- [ ] **Step 4: Run the tests** — Expected: all three PASS. Then `cargo test --workspace --locked` — all pass; `cargo clippy --workspace --all-targets` clean.

- [ ] **Step 5: Commit:** `git add -A crates && git commit -m "App: fresh_model and settle replace the repeated blocks in finish_scan, preview_installed and swap_tree"` (with the trailer).

### Close

- [ ] **Step 1: Docs.** In `PARKED.md` delete the line "A7 scan lifecycle and A8 live sources: in progress ..." and change the group's title from "A2-A8" to "A2-A6" (its prose says "Each is a design-level cleanup"; no other edit). In `docs/superpowers/plans/2026-10-31-cleanup.md`, append to items A7 and A8 `**Done 2026-10-31** (<commits>): own plan `2026-10-31-follow-and-install.md``. `grep -rn "Source::\|preview_pacer\|preview_dirty\|finish_scan" docs *.md` to find stale wording; only dated history may keep it.
- [ ] **Step 2: Statuses.** Set this plan and the spec to `status: done` (leftovers: none; the phase enum is already in `PARKED.md`).
- [ ] **Step 3: Final review:** one fresh Sonnet reviewer over `git diff <spec commit>..HEAD` with the Review Focus list; fix findings with a failing test first.
- [ ] **Step 4: Commit** the docs, then tell the user to run `--selftest-live` (elevated).

## Self-review (2026-10-31)

- **Spec coverage:** `Paced` (Task 1), `Follow` with `Send` and `Box<dyn>` (Task 2), `fresh_model` and `settle` (Task 3), spec tests (Tasks 1 to 3: the fake `Follow`, the three install tests, the `Paced` test), `PARKED.md` upkeep (Close).
- **Placeholders:** none; the only "unchanged" instruction is Task 2's move of existing match arms, which names the source (`watch.rs` today).
- **Types:** `Paced` methods match their use in `window.rs` and `watch.rs`; `fresh_model(&Tree, SortKey, bool)` matches `RowModel::set_sort(&Tree, SortKey, bool)` and `sort.1` as the `desc` flag.
- **Risk:** Task 2's `Send` bound is the one compile-time trap; Task 1's `preview_tick` holds `p` across `self.app` calls, which is fine because they are disjoint fields (`self.invalidate()` runs after `p` is last used).
