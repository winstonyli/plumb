---
status: done
---

> Note 2026-11-11: the shipped layout differs from this plan (score is column 1, tenths of a percent, relative dead band); the spec is current.

# Score column Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fifth, sortable "Score" column: "Stale" (age-weighted size, summed up the tree) in the disk view and "Load" (steady share of the machine, summed up the family) in the Processes view.

**Architecture:** A pure `plumb-core/src/score.rs` defines the formulas. `Tree` gains a rolled-up `subtree_score` channel beside `subtree_size`, fed by one primitive `add_score` (files rescore from size and mtime; `Procs` adds its smoothed shown load). The UI gains a fifth number column, a sort key and a header tooltip.

**Tech Stack:** Rust workspace (`plumb-core`, `plumb-scan`, `plumb`), Direct2D drawing, offscreen WARP render tests.

**Spec:** `docs/superpowers/specs/2026-11-11-score-column-design.md` (read it first; it holds the reasons and the decided options m1, p1).

## Global Constraints

- Edit files with the Edit/Write tools. Never run `python` reading stdin. PowerShell scripts: ASCII only.
- Any build or test run expected over a minute goes through `powershell -NoProfile -File "$env:USERPROFILE\.claude\runs\run.ps1" -Kind probe -Name <n> -Command "<cmd>"` (12 GB cap); use `-j 12`. Check for another cargo on the same target dir first.
- File score: `weight` per mille is 0 under `STALE_FROM` = 30 days, linear to 1000 at `STALE_FULL` = 730 days, 1000 beyond; unknown or future mtime weighs 0; `file_score = size * weight / 1000` with a `u128` intermediate.
- Process load: `TAU_SECS` = 30.0; `IO_FULL` = 500_000_000 B/s; CPU share = `cpu_tenths / 10 / cores`; memory share = `private / ram` (unsmoothed); score = max of the three in percent; dead band `if x < 0.5 { shown = 0 } else if (x - shown).abs() >= 1.0 { shown = x.round() }`.
- Column: default width 72 (`COL_W` = `[100.0, 130.0, 90.0, 96.0, 72.0]`); at 640 DIP wide, Name must stay >= `MIN_NAME_W` (120).
- Headers: disk "Stale", Processes "Load". Tooltip text is in Task 5.
- One commit per task; commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. No bump, tag or push.

## Review Focus

1. A file with a future mtime (clock skew) or an unknown one scores 0 with no underflow (Task 1, Task 2).
2. A file rewritten live (`set_size` then `set_mtime`, or the reverse) ends with the right score and the right ancestor sums either way (Task 2).
3. A reused pid (same pid, new `created`) leaves no residue from the exited process in its parents (Task 4).
4. `cores` or `ram` of 0 divides nothing (Task 1, Task 4).
5. A saved state from before this change (4 saved widths, no Score sort) loads, and a saved Score sort restores (Task 5).

---

### Task 1: The pure score definitions

**Files:**
- Create: `crates/plumb-core/src/score.rs`
- Modify: `crates/plumb-core/src/lib.rs` (declare `pub mod score;`; `tests/modules_declared.rs` checks this)
- Modify: `docs/superpowers/specs/2026-11-11-score-column-design.md` (front matter `status: active`)

**Interfaces:**
- Produces (all in `plumb_core::score`):
  - `pub const STALE_FROM: u32`, `STALE_FULL: u32` (seconds), `IO_FULL: u64`, `TAU_SECS: f64`
  - `pub fn weight(age_secs: u32) -> u32` (per mille)
  - `pub fn file_score(size: u64, mtime: Option<NonZeroU32>, now: u32) -> u64`
  - `pub fn cpu_share(cpu_tenths: u64, cores: u32) -> f64`, `pub fn mem_share(private: u64, ram: u64) -> f64`, `pub fn io_share(io_rate: u64) -> f64` (all percent; 0 when the divisor is 0)
  - `pub fn settle(shown: u64, x: f64) -> u64` (the dead band)
  - `#[derive(Default, Clone, Copy)] pub struct Load { cpu: f64, io: f64, shown: u64 }` with `pub fn step(&mut self, dt_secs: f64, cpu_pct: f64, io_pct: f64, mem_pct: f64) -> u64` returning the new shown value, and `pub fn shown(&self) -> u64`

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)] mod tests` in `score.rs`), with these names and assertions:
  - `weight_ramps_from_30_days_to_2_years`: `weight(0)`, `weight(29 * DAY)`, `weight(30 * DAY)` are 0; `weight(365 * DAY) == 478`; `weight(730 * DAY) == 1000`; `weight(3650 * DAY) == 1000`; `weight(u32::MAX) == 1000`. (`DAY = 86_400`.)
  - `file_score_is_zero_for_unknown_and_future_mtimes`: `file_score(1000, None, N) == 0`; `file_score(1000, NonZeroU32::new(N + 5), N) == 0`.
  - `file_score_does_not_overflow`: `file_score(u64::MAX, NonZeroU32::new(1), u32::MAX) == u64::MAX`; `file_score(1000, NonZeroU32::new(N - 365 * DAY), N) == 478`.
  - `shares_are_percents_and_guard_zero_divisors`: `cpu_share(250, 4) == 6.25`; `cpu_share(250, 0) == 0.0`; `mem_share(512, 1024) == 50.0`; `mem_share(1, 0) == 0.0`; `io_share(250_000_000) == 50.0`.
  - `the_dead_band_holds_small_moves_and_snaps_idle_to_zero`: `settle(5, 5.9) == 5`; `settle(5, 6.0) == 6`; `settle(5, 4.0) == 4`; `settle(1, 0.49) == 0`; `settle(1, 0.5) == 1`; `settle(0, 0.9) == 0`; `settle(0, 1.0) == 1`.
  - `the_average_follows_the_hand_computed_sequence`: a default `Load`; `step(1.0, 100.0, 0.0, 0.0) == 3` (ema 3.278), then `step(1.0, 100.0, 0.0, 0.0) == 6` (ema 6.449), then `step(0.0, 0.0, 0.0, 0.0) == 6` (`dt = 0` changes nothing, even with new inputs, because the average does not move and the band holds).
  - `a_constant_load_converges_and_an_idle_one_returns_to_zero`: 3000 steps of `dt = 1`, cpu 40 give `shown() == 40`; then 3000 steps of zeros give `shown() == 0`.
  - `memory_is_not_smoothed_and_the_largest_term_wins`: a default `Load`, `step(1.0, 0.0, 0.0, 30.0) == 30`.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p plumb-core score::` Expected: compile error, `score` module missing.
- [ ] **Step 3: Implement** the interface above in `score.rs`. `weight`: `(age - STALE_FROM) * 1000 / (STALE_FULL - STALE_FROM)` between the bounds (use `u64` arithmetic). `file_score`: `age = now.checked_sub(mtime)`; `None` age scores 0. `Load::step`: `a = 1.0 - (-dt / TAU_SECS).exp()`; `cpu += (cpu_pct - cpu) * a`, same for `io`; `x = max(cpu, io, mem_pct)`; `shown = settle(shown, x)`. Module doc says it is pure (no Windows types).
- [ ] **Step 4: Run to verify they pass.** Run: `cargo test -p plumb-core score::` Expected: 8 passed.
- [ ] **Step 5: Commit** `git add crates/plumb-core/src/score.rs crates/plumb-core/src/lib.rs docs/superpowers/specs/2026-11-11-score-column-design.md && git commit -m "Score definitions: age weight, file score, smoothed load with a dead band"`.

---

### Task 2: The `Tree` score channel

**Files:**
- Modify: `crates/plumb-core/src/tree.rs` (struct, `bump`, `add_child`, `set_mtime`, `set_size`, `set_dir`, `reserve`, `remove`, `reparent`, `TreeBuilder`, `finish`, tests)
- Modify: `crates/plumb-core/src/rows.rs` (`SortKey::Score` and its arm in `order`, ~line 11-18 and ~191)

**Interfaces:**
- Consumes: `score::file_score`.
- Produces:
  - `Tree::subtree_score: Vec<u64>` (public, one per node); private `now: Option<u32>`
  - `Tree::set_now(&mut self, now: u32)`: stores `now`, then calls `rescore` on every node
  - `Tree::add_score(&mut self, id: NodeId, delta: i64)`: adds the signed delta to `id` and every ancestor, touches `id`, bumps `rev`
  - private `Tree::rescore(&mut self, id: NodeId)`: no-op without `now`, for a DEAD node, or a DIR node; else `add_score(id, new_own - subtree_score[id])` when they differ
  - `TreeBuilder::set_now(&mut self, now: u32)`; `finish` seeds `subtree_score` for non-DIR nodes with `file_score` (zero everywhere without `now`) and sums it in the same reverse pass as `subtree_size`
  - `SortKey::Score` ordering by `subtree_score`

- [ ] **Step 1: Write the failing tests** in `tree.rs` `mod tests` (build trees with `TreeBuilder`, `set_now(NOW)`, mtimes via `set_mtime`):
  - `files_score_by_age_and_folders_sum_them`: root dir with files A (1000 B, mtime `NOW - 1095 * DAY`), B (1000 B, `NOW - 365 * DAY`), C (1000 B, no mtime). After `finish`: `subtree_score` is 1000, 478, 0 for A, B, C and 1478 for the root.
  - `a_live_change_moves_the_sums_and_touches_the_file`: continuing: `set_mtime(C, old)` gives C 1000, root 2478, and `touched()` contains C; `set_size(A, 2000)` gives A 2000, root 3478; the reverse order (`set_size` then `set_mtime`) on a fresh file ends equal; `remove(A)` takes root to 1478; `reparent(B, otherDir)` moves 478 between the two dirs' sums.
  - `a_tree_without_now_scores_nothing`: same data, no `set_now`: every `subtree_score` is 0 after `set_mtime` and `set_size`.
  - `set_now_on_a_finished_tree_scores_it`: finish without `now`, then `set_now(NOW)`: sums equal the first test's.
  - `folders_and_dead_nodes_never_change_the_sums`: `set_size(root, 5)` and `set_size(dir, 5)` leave all `subtree_score` unchanged; after `remove(A)`, `set_mtime(A, None)` and `set_size(A, 9)` leave the root unchanged.
  - `add_score_adds_signed_deltas_up_the_chain`: `add_score(leaf, 7)` then `add_score(leaf, -3)` gives 4 on the leaf and every ancestor.
  - `scores_stay_equal_to_a_walk_under_random_edits`: in the style of the drift test in `rows.rs` (~486-499): a seeded LCG drives 500 random `add_child`, `set_size`, `set_mtime`, `remove`, `reparent` (and `set_size` on folders) on a tree with `now`; after each step every live node's `subtree_score` equals an oracle: the sum of `file_score(size, mtime, now)` over its live non-DIR descendants and itself.
  - In `rows.rs`: a sort by `SortKey::Score` orders three siblings by `subtree_score`, ties by id as the other keys do.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p plumb-core tree:: rows::` Expected: compile errors (`set_now`, `subtree_score`, `SortKey::Score` missing).
- [ ] **Step 3: Implement.** `bump` gains a `score: u64` parameter and adds or subtracts it like `size` (callers: `add_child` passes 0; `set_size` 0; `remove` and `reparent` pass `subtree_score[id]`). `add_child` pushes 0 to `subtree_score`. `set_mtime` writes the field then calls `rescore`. `set_size` calls `rescore` after its `bump`. `set_dir` gets `debug_assert!(self.now.is_none())`. `rescore` asserts `debug_assert!(self.now.is_none() || !self.has_children(id))`. `reserve` reserves `subtree_score`. Convert `u64` to `i64` with a `debug_assert!` that the score fits (disk sizes are below 2^63).
- [ ] **Step 4: Run to verify they pass.** Run: `cargo test -p plumb-core` Expected: all pass (existing tests untouched).
- [ ] **Step 5: Commit** `git commit -am "Tree: rolled-up subtree_score, add_score primitive, file rescoring from size and mtime"`.

---

### Task 3: Give the disk trees a clock

**Files:**
- Modify: `crates/plumb/src/window.rs` (~744-751: the scan thread)
- Modify: `crates/plumb/src/preview.rs` (`root_tree`, ~160)

**Interfaces:**
- Consumes: `TreeBuilder::set_now(u32)`, `plumb_core::age::now_secs()`.

- [ ] **Step 1: Write the failing test** in `crates/plumb/src/preview/tests.rs`: `a_preview_tree_scores_old_files_as_they_arrive`: apply an `Add` for a dir under the root, then an `Add` of a 4096-byte file with `mtime = now - 3 years`; assert the file's and the root's `subtree_score` are 4096.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p plumb preview::` Expected: FAIL (score 0).
- [ ] **Step 3: Implement.** In `window.rs` call `b.set_now(plumb_core::age::now_secs())` on the scan thread's builder before `b.finish()`. In `root_tree` call `b.set_now(plumb_core::age::now_secs())` before `finish`. (The preview's later `add_child` + `set_mtime` calls then score by themselves; check the second `add_child` at `preview.rs` ~199 and set the mtime through `set_mtime` if it sets one.)
- [ ] **Step 4: Run to verify they pass.** Run: `cargo test -p plumb preview::` Expected: pass.
- [ ] **Step 5: Commit** `git commit -am "Disk trees carry the scan time so files score"`.

---

### Task 4: Process load in `Procs`, carried across rebuilds

**Files:**
- Modify: `crates/plumb-core/src/procs.rs` (`Known`, `Procs`, `new`, `apply`, tests)
- Modify: `crates/plumb-scan/src/procs.rs` (add `pub fn logical_cores() -> u32` beside `physical_memory`, via `GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)`; enable the windows feature if the build asks)
- Modify: `crates/plumb/src/window/procs.rs` (`Snapshot.cores`, poller, `update`)
- Modify: every `Procs::new` caller: `crates/plumb-core/src/procs.rs:203`, `crates/plumb/src/window/procs.rs:168`, `crates/plumb/src/app.rs` tests (~1538, 1908, 1934, 2719), `crates/plumb/tests/render.rs` (~550, 592, 645)

**Interfaces:**
- Consumes: `score::{Load, cpu_share, mem_share, io_share}`, `Tree::add_score`.
- Produces:
  - `Procs::new(metric: Metric, cores: u32, ram: u64) -> (Self, Tree)` (`ram` is total physical memory, the first element of `Snapshot.ram`)
  - `Procs::carry_from(&mut self, old: Procs)`: moves `old`'s per-process `Load` **and `prev` counters** (cpu and io totals) into a pending map keyed by (pid, created); a process created on the next `apply` takes both from it, so its first delta is real (otherwise `prev` restarts at the sample, the delta is 0 and the carried average takes a zero input). Pending entries are dropped after that `apply`.
  - `Known` gains `load: Load` and `applied: u64` (what has been added to the tree)
  - `Snapshot.cores: u32`

- [ ] **Step 1: Write the failing tests** in `procs.rs` `mod tests` (extend `ProcTree::new` to `Procs::new(Metric::Memory, 4, 1 << 30)`):
  - `load_is_zero_at_first_and_holds_still_under_jittering_cpu`: one process on a 4-core machine, raw CPU jittering between 5% and 25% of a core per second for 120 samples (a table in the test); count how often the node's `subtree_score` changes: at most 10 times, while the raw `cpu_tenths` changes on most samples; the first sample's score is 0.
  - `a_parent_sums_its_children_and_an_exit_leaves_no_residue`: parent plus two children each at a steady share; the parent's `subtree_score` equals the three shown scores summed; remove one child: the parent's score drops by exactly that child's shown score; a new process with the same pid but a new `created` starts at 0.
  - `system_idle_scores_zero`: pid 0 with huge cpu and io deltas stays 0.
  - `memory_share_is_not_smoothed`: a process with private bytes at 25% of `ram` shows 25 on its first sample.
  - `zero_cores_or_ram_divides_nothing`: `Procs::new(Metric::Memory, 0, 0)` with busy samples yields scores without panic.
  - `a_rebuild_carries_the_averages`: run a busy process (steady 40% of one core on 1 core) for 60 samples; build `(new, tree2) = Procs::new(...)`, `new.carry_from(old)`, apply the next sample (counters continuing); the process's shown score in `tree2` equals the old tree's last shown score.
  - In `window/procs.rs` or `app.rs` tests: existing `Procs::new` callers compile with the new arguments.
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p plumb-core procs::` Expected: compile errors (`new` arity, `carry_from`).
- [ ] **Step 3: Implement.** In `apply` step 1 give a new `Known` `load: pending.remove(&key).unwrap_or_default()` and `applied: 0`. In step 5, after the existing numbers: `dt = elapsed_100ns as f64 / 1e7`; for pid 0 all shares are 0; `shown = k.load.step(dt, cpu_share(k.cpu_tenths, cores), io_share(k.io_rate), mem_share(k.private, ram))`; if `shown != k.applied` call `tree.add_score(k.id, shown as i64 - k.applied as i64)` and set `k.applied = shown`. Step 3 (exit) needs no change: `remove` carries `subtree_score` through `bump`, and children were reparented first. In `window/procs.rs`: the poller reads `logical_cores()` once and puts it in each `Snapshot`; in `update`, on the rebuild branch take the old `Procs` (`self.sync.take()`), call `Procs::new(metric, cores, ram.0)`, then `carry_from(old)` when there was one.
- [ ] **Step 4: Run to verify they pass.** Run: `cargo test -p plumb-core procs:: && cargo test -p plumb window::procs` and `cargo test --workspace --no-run` (via `run.ps1`) Expected: pass and the whole workspace's tests compile.
- [ ] **Step 5: Commit** `git commit -am "Procs: steady load score, smoothed over 30 s, carried across rebuilds"`.

---

### Task 5: The Score column in the UI

**Files:**
- Modify: `crates/plumb/src/layout.rs` (`COL_W`, `col_w`, docs, `hit` loops, `tree_min` via `col_w`, tests)
- Modify: `crates/plumb/src/app.rs` (`titles` -> `[&str; 6]`, `sort_key` arrays, new `ViewKind::score_text`, `tip_target` header branch, saved-state loader `s.column < 5` -> `< 6`, tests)
- Modify: `crates/plumb/src/draw.rs` (header: right-align `matches!(c, 1 | 3 | 4 | 5)`, the "≥" marker; row cell; skeleton)
- Modify: `crates/plumb/src/fmt.rs` (whole-percent formatter if none exists)

**Interfaces:**
- Produces: `ViewKind::score_text(self, tree: &Tree, id: NodeId) -> fmt::Show<'static>`: Disk `fmt::size(tree.subtree_score[id])`, Processes the score as a whole percent (`12%`; add `fmt::percent(v: u64)` if `fmt` has none). `sort_key(5)` is `SortKey::Score` in both views. `titles(..)[5]` is "Stale" (Disk) or "Load" (Processes).

- [ ] **Step 1: Write the failing tests:**
  - `app.rs`: extend `a_disk_view_keeps_its_titles_sorts_and_last_two_columns` to `(0..6)` with `SortKey::Score` last, titles' last entry "Stale"; the Processes assertion (~1955) adds `sort_key(5) == SortKey::Score` and title "Load"; `score_text` for a Disk file with score 4096 is `fmt::size(4096)` and for a process with score 12 is the `12%` text.
  - `layout.rs`: `Layout::new(640.0, 400.0).column(0).1 >= MIN_NAME_W`; `column(5)` ends at the row's right edge (it fills the row width, like `column(4)` did); `hit` finds a divider at column 5's left edge and a header over column 5. Update the existing `tree_min`/`map_shown` threshold tests: the map appears from 785 DIP wide instead of 713 (`tree_min` 548 -> 620).
  - `app.rs` (~2439-2453): a saved state with 4 widths still loads and leaves `col_w[4] == COL_W[4]`; extend the `[..3]`/`[3]` asserts to `[3..] == COL_W[3..]`; a saved `column = 5` sort is restored.
  - `app.rs`: `tip_target` over the Score header returns `Some(TipKey::Area(header_cell(5)))` with the view's tooltip text; over the other headers it returns `None`.
  - `tests/render.rs`: next to `a_rows_last_write_date_is_drawn_in_the_modified_column...` (~514), a shot test `the_stale_column_is_drawn_for_an_old_file` (a file with a 3-year-old mtime, window 640 wide, `PLUMB_SHOT_DIR` supported) asserting the render succeeds and, with `now` set, differs from the same tree with a new file (pixel difference in the column's rect).
- [ ] **Step 2: Run to verify failure.** Run: `cargo test -p plumb --lib && cargo test -p plumb --test render` (via `run.ps1`) Expected: compile errors, then failures.
- [ ] **Step 3: Implement.** `COL_W: [f32; 5] = [100.0, 130.0, 90.0, 96.0, 72.0]`; `col_w: [f32; 5]`; `set_col_w` doc "(1..=5)"; `hit` loops `(1..6)` and `(0..6)`. `titles` and `sort_key` arrays gain their sixth entry. Header: add `c == 5` to the right-align match; the "≥" marker also when `c == 5 && Disk && partial`. Row: after the fourth cell draw `score_text` in column 5 (also when `fourth` is `None`); the skeleton (`draw.rs` ~1063) is left as it is (it draws no number-column placeholders beyond the third). `tip_target`: before the row branch, `if let Hit::Header(5) = self.layout.hit(x, y, self.views.len())` writes the text and returns `TipKey::Area(self.layout.header_cell(5))`. Texts: Disk "size not modified for a long time (counts in from 30 days, in full after 2 years); a folder adds its files"; Processes "the most a process uses of CPU, memory or I/O, as a share of the machine, steady over about 30 s; a parent adds its children". The loader accepts `s.column < 6`. Grep for any other `match` on `SortKey` that must now cover `Score`.
- [ ] **Step 4: Run to verify they pass.** Run: `cargo test --workspace` (via `run.ps1`, `-MaxMemGB 12`) Expected: all pass; open the shot with `PLUMB_SHOT_DIR` set and look at it.
- [ ] **Step 5: Commit** `git commit -am "Score column: Stale for files, Load for processes, sort and header tooltip"`.

---

### Task 6: Selftest, docs and parked entries

**Files:**
- Modify: the hidden-window selftest registry (grep `selftest` under `crates/plumb/src`) to add `a_real_scan_shows_stale_for_an_old_file`
- Modify: `PARKED.md`, `docs/superpowers/specs/2026-11-11-score-column-design.md` (`status: done`), this plan (`status: done`)
- Modify: any doc that lists the columns (grep `Modified` and `% of parent` in `README.md` and `docs/`)

- [ ] **Step 1: Write the selftest.** Create a temp directory with a file, set its modified time to 3 years ago with `File::set_modified`, scan it with the app's scan path in the hidden window, and assert the root row's score text is `fmt::size(allocated bytes of the file)` and the sort by Score puts the folder first.
- [ ] **Step 2: Run it.** Run: `target\debug\plumb.exe --selftest` (via `run.ps1`) Expected: all checks pass, including the new one.
- [ ] **Step 3: Docs.** Add three `PARKED.md` entries in the file's format: ages do not re-age while a window stays open (trigger: a stale score reported after days open); `IO_FULL` is fixed at 500 MB/s (trigger: an NVMe process always reading low); modified time is a weak proxy for unused (no trigger; by decision). Update the column lists the grep finds. Set both front matters to `status: done`.
- [ ] **Step 4: Check.** Run: `cargo clippy --workspace --all-targets` and `cargo test --workspace` Expected: clean and green.
- [ ] **Step 5: Commit** `git commit -am "Score column: selftest, docs, parked limits"`.

---

## End-state fit

- New: `plumb-core/src/score.rs` (the only home of the formulas); `Tree::subtree_score` with one primitive `add_score`; `SortKey::Score`; a fifth column. Replaces nothing; deletes nothing (CPU, I/O, Items and Modified stay).
- No special case per view in `Tree`: files and processes both feed `add_score`. The per-view difference is only the column text and tooltip in `ViewKind`.
- Guard: the random drift test (Task 2) fails on any future edit method that forgets the score term in `bump`.

## Self-review (run 2026-11-11)

- Review rounds: 1 (Sonnet, against the code). Caught: three missed `Procs::new` callers in `tests/render.rs` (the workspace tests would not compile after Task 4), and `carry_from` carrying the average but not the cpu/io counters (the first delta after a rebuild would be 0 and dip the average). Both fixed above.

- Spec coverage: channel (T2), clock (T3), files (T1/T2), processes with m1 carry (T4), column and tooltip (T5), tests and parked limits (T6). The spec's "a new TipKey" is not needed: `TipKey::Area(Rect)` already identifies a header cell.
- Type consistency: `Load::step` arguments and `Procs::new(metric, cores, ram)` are the same in T1, T4 and the spec; `add_score(id, i64)` is the same in T2 and T4.
- Proportion: plan is about two spec-lengths; code appears only as signatures, constants and test assertions.
