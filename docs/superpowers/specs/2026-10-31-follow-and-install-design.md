---
status: active
---

# One live-source trait, one pacer, one tree-install tail (cleanup A7, A8)

Cleanup items A7 and A8 (`PARKED.md`, evidence in `docs/superpowers/plans/2026-10-31-cleanup.md`), picked 2026-10-31 as "h1" and "k2". No behavior changes: the same tests pass and `--selftest-live` shows the same result. The point is fewer places that must agree.

## What is wrong today (checked 2026-10-31)

**A8, live sources.**
- `window/watch.rs` `enum Source { Journal, Dir }` is matched in five `Watch` methods (`drain`, `apply`, `queued`, `backlog`, `stop`). It is constructed in exactly two places (`start_watch`, `install_dir_watch`) and read nowhere else.
- The dirty / pacer / refresh sequence exists twice. `Watch` has `pacer` and `dirty`, used in `live_tick`. `Win` has `preview_pacer` and `preview_dirty` (`window.rs`), used in `preview_tick`. Both are "mark dirty when changes were applied; refresh the rows when dirty and the pacer is due; tell the pacer how long it took; clear dirty". `Win::preview_pacer` is reset at two sites: when a scan starts (a new `Preview`) and when a view with a running preview comes back (`unpark`).

**A7, tree install.** `App::finish_scan`, `preview_installed` and `swap_tree` (`app.rs`) repeat two blocks:
- *Build a model for a tree*: `RowModel::new`, `set_sort(kind.sort_key(sort.0), sort.1)`, then expand and select the first row. In `finish_scan` and `preview_installed`, and in `swap_tree` through `finish_scan` when there is no snapshot.
- *Settle after a finished install*: `scan = Idle`, `refreshing = false`, `preview_paused = false`, `set_live(Off)`. In `finish_scan` and `swap_tree`. `finish_scan` does not clear `refreshing` (it never needs to: a first scan has none), so the two copies already differ.

## Design

### Live sources: one trait, one pacer

```rust
// window/watch.rs
pub(super) trait Follow: Send {   // Send: `drop_watch` moves the whole `Watch` to a thread to stop it
    /// Moves what the source has queued into its queue; `Some` if the source has ended.
    fn drain(&mut self) -> Option<Ended>;
    /// Applies queued changes until `deadline`; returns the tree changes made.
    fn apply(&mut self, tree: &mut Tree, deadline: Instant) -> usize;
    fn queued(&self) -> bool;
    /// Work left that a timer tick should finish. Defaults to `queued`.
    fn backlog(&self) -> bool { self.queued() }
    /// Stops the source (blocks briefly: call off the UI thread).
    fn stop(self: Box<Self>);
}
struct JournalFollow { reader: JournalReader, live: Live, queue: VecDeque<Delta> }
struct DirFollow { watcher: Watcher, queue: VecDeque<Listing> }
```

`Watch.src` becomes `Box<dyn Follow>`; `Watch` loses `drain`, `apply`, `queued`, `backlog` and `stop` bodies (they forward) and `enum Source` is deleted. The match arms move unchanged into the two `impl Follow` blocks.

```rust
// live.rs, beside Pacer
pub struct Paced { pacer: Pacer, dirty: bool }
impl Paced {
    pub fn new(now: Instant) -> Self;
    pub fn mark(&mut self, changed: bool);   // dirty |= changed
    pub fn dirty(&self) -> bool;
    pub fn due(&self, now: Instant) -> bool; // dirty && pacer.due(now)
    pub fn done(&mut self, now: Instant, took: Duration); // pacer.done, dirty = false
    pub fn reset(&mut self, now: Instant);   // fresh pacer, not dirty
}
```

`Watch { src, paced, armed }` and `Preview` gains `paced: Paced`; `Win::preview_pacer` and `Win::preview_dirty` are deleted. The two reset sites become `Preview::new` (already the construction at scan start) and `paced.reset(now)` in `unpark`. This is equivalent because both old reset sites coincide with a new or returning `Preview`; a parked view's preview never refreshed rows, so it never used the shared fields.

Not shared on purpose: the preview is not a `Follow`. It has no `drain`, applies records through `Preview::apply` with different results (`reset`, `created`, `changes`), and runs only during a scan. Only the pacing is common, so only `Paced` is shared.

### Tree install: two helpers, no mode enum

Two private `App` helpers replace the repeated blocks; `finish_scan`, `preview_installed` and `swap_tree` stay as the three entry points the rest of the code calls and become short.

```rust
/// A model for `tree` in the current sort, with the root expanded and selected.
fn fresh_model(tree: &Tree, key: SortKey, desc: bool) -> RowModel; // an associated fn: callers hold a borrow of `self.slot.tree`
/// After an install that ends a scan or a rescan: nothing is running, no preview is paused, live is off.
fn settle(&mut self, summary: String); // sets summary, scan = Idle, refreshing = false, preview_paused = false, set_live(Off)
```

`finish_scan` = `filter_reset; map_reset; install tree + fresh_model; scroll = 0; settle(summary)`. `preview_installed` = `filter_reset; map_reset; scroll = 0; model = fresh_model` (no `settle`: the scan keeps counting). `swap_tree` keeps its snapshot logic and ends with `settle(summary)`; with no snapshot it still falls to `finish_scan`. `begin_scan` is unchanged (it clears rather than installs).

## Decisions to review

- **No `install_tree(how)` enum** (earlier "i1" idea). The three entry points differ in what they leave alone (`preview_installed` keeps scan, summary and live; `swap_tree` keeps the view). A mode enum would be a dispatcher with those differences as branches; the two helpers are exactly the parts that really repeat. Recommended; the alternative is the enum, which adds a branch per difference.
- **`finish_scan` now also clears `refreshing`** through `settle`. This is unreachable-by-construction today (`begin_scan` clears it and a rescan goes through `swap_tree`), so no behavior changes; a test pins it.
- **`Box<dyn Follow>` (the trait is `Send`)**, not an enum with a `Follow` impl per variant. There are two implementors and no hot path (a few calls per 250 ms tick), so a vtable costs nothing and removes the enum. Recommended; the enum stays if a third source never comes and you prefer no dynamic dispatch.
- **Phase enum** (merging `scan`, `refreshing`, `preview_paused`) is not part of this. It is parked with its trigger (`PARKED.md`).

## Tests

- `live/tests.rs`: `Paced` is not due until marked; due once marked and the pacer allows; `done` clears dirty and backs off by `max(REPAINT, took * 5)`; `reset` clears both.
- `app.rs`: `finish_scan` after `refreshing = true` clears it (the pinned line above); `preview_installed` leaves `scan`, `summary` and `live` alone; `swap_tree` without a snapshot settles the same as `finish_scan`. The existing install tests (`app.rs` around the `refreshing` assertions) must pass unchanged.
- The existing `journal` and `dirsync` tests and `--selftest-live` cover both sources through `Watch`. A small test with a fake `Follow` checks that `Watch` drains, applies and reports a backlog without a real journal.
- Gate: `cargo test --workspace --locked` (414 before), clippy clean, and `--selftest-live` by the user (elevated).

## Out of scope

A2 to A6 (still parked), the phase enum, and any change to timers, the apply budget or the `REPAINT` pace.
