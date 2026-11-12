---
status: active
---

# Score column: a steady load for processes, stale size for files (v0.13 candidate)

Decided with the user, 2026-11-11: **g1** (no AI; deterministic help only, see `PARKED.md`), **i1** (one Score column,
summed up the tree, a different weight per view), **j1** (process load = smoothed bottleneck share), **k1** (file score =
age-weighted size).

## Understanding

**Outcome.** A Score column in both views that answers "what is worth my attention". Processes: one load number that
holds still, so a sort by it does not reshuffle every second. Files and folders: how many bytes have sat unused for a
long time, summed up the tree, so a folder ranks by the stale space under it.

**Constraints (the user's words).** "A stable load metric for processes that doesn't constantly change"; "a score for
files/items would also be helpful". No AI.

**Success.** The score is one column slot, sortable, resizable and tooltip-explained; its definition is a pure function
with tests; a recorded process trace shows the shown value changing rarely; the file score survives live updates.

**Assumptions (change on review).** Unknown age counts as not stale. Ages are measured against the time of the scan, not
re-aged while the window stays open. I/O is judged against a fixed reference speed (below).

## Today (verified against the code, round 1)

- `Tree` keeps own `size` and rolled-up `subtree_size` / `subtree_files`. `bump` walks the ancestors on add, remove,
  `set_size` and `reparent`; `set_dir` walks them itself and fixes only `subtree_files`; `finish` fills the sums in one
  pass (`plumb-core/src/tree.rs`).
- `Tree::set_mtime` only writes the field: it does not skip DEAD nodes and marks nothing touched. `Tree::aux` is
  `[cpu, io, pid]` for the Processes tree; `SortKey::Aux(i)` sorts by a node's own value, not a roll-up.
- `Procs::apply` makes `cpu_tenths` (tenths of one core: `dcpu * 1000 / elapsed`) and `io_rate` (bytes a second) from the
  difference between two snapshots, one or two seconds apart (`window/procs.rs`: `POLL`, `POLL_BACKGROUND`). They are the
  jitter. System Idle (pid 0) is forced to zero; a process is keyed by (pid, created); the reconciler rebuilds `Procs`
  (and so all its state) when too many nodes are dead or after a view switch (`window/procs.rs` ~161-170).
- Disk trees are built in the scan thread (`window.rs` ~744) and the preview (`preview.rs` `root_tree`), and edited by the
  live journal and the directory watcher (`live.rs`, `dirsync.rs`) through `Tree` methods only. `age::now_secs()` exists.
- Four number columns are hard-coded in many places (sites listed under "The column").

## Design

**Place.** The definition lives in a new `plumb-core/src/score.rs` (pure, no Windows types). The roll-up lives in `Tree`
beside `subtree_size`. The process smoothing lives in `Procs`. The column is drawn and sorted like the others.

### The channel (`Tree`)

- One new array, `subtree_score: Vec<u64>`, reserved with the others (`reserve`), 8 bytes a node on top of about 43.
- A file has no children, so its own score *is* its `subtree_score`. Only files score; a folder's `subtree_score` is the
  sum of its descendants. `Tree::rescore(id)`: for a live, non-folder node, compute the new own score from the node's
  current size and mtime and add the difference to `subtree_score[id]` and every ancestor, and touch `id` when it
  changed. Called from `set_size`, `set_mtime` and `finish`; `add_child` needs no call (a new node has no mtime, which
  scores 0) and `remove` / `reparent` carry `subtree_score[id]` through `bump` like size. `rescore` is a no-op for a
  DEAD node or a folder. The old score is read from `subtree_score[id]` before the size or mtime is overwritten.
- `Tree::now: Option<u32>` (unix seconds). A tree without it (the Processes tree, tests, synthetic trees) scores nothing.
  Set by `TreeBuilder::set_now` before `finish` and by the preview's `root_tree`, both from `age::now_secs()`; sites:
  the scan thread and `root_tree`. Tests that check Stale set it.
- `set_dir` is used only on the Processes tree (no `now`); it gets a `debug_assert!(self.now.is_none())`, not a hook. `rescore` asserts the node has no children (`debug_assert!(self.now.is_none() || !self.has_children(id))`); the disk appliers already remove and re-add a node whose DIR flag flips (`live.rs` ~121, `dirsync.rs` ~45).
- Scoring tests edit through `set_mtime` (a direct write to `mtime[..]` bypasses `rescore`).
- `set_mtime` now touches a node when its own score changed, so a live change to an old file flashes its row like any
  other change.
- `Tree::add_score(id, delta: i64)` is the one roll-up primitive: it adds a signed delta to `id` and its ancestors
  (`bump` with a score term) and touches `id`. `rescore` computes the new own score and calls `add_score(new - old)`;
  `Procs::apply` calls it with the difference between the new and old shown score.
- `Tree::set_now(u32)` stores `now` and, if the tree is already finished, recomputes every file's score with the same
  pass `finish` runs (factored once), so a test can score a finished tree.

### Files: age-weighted size (`score::file_score`)

`weight(age)` in per mille: 0 under 30 days, rising in a straight line to 1000 at 2 years, 1000 beyond. Unknown or future
mtime weighs 0. `file_score = size * weight / 1000` (128-bit intermediate). The column shows `fmt::size(subtree_score)`
(with the "≥" partial marker while a scan runs, like Size), headed "Stale"; tooltip: "size not modified for a long time
(counts in from 30 days, in full after 2 years); a folder adds its files". Constants `STALE_FROM`, `STALE_FULL` in
`score.rs`.

### Processes: smoothed bottleneck share (`score::Load`, kept in `Procs::Known`)

Per process three shares of what the machine has, in percent: `cpu = cpu_tenths / 10 / cores` (a thread pegged on 16
cores is 6%: it is light for the machine), `mem = private / ram` (not smoothed; memory is steady), `io = io_rate /
IO_FULL` with `IO_FULL = 500 MB/s` (a SATA SSD's sequential speed; `score.rs` constant). CPU and I/O are smoothed with an
exponential average over real elapsed time, `ema += (x - ema) * (1 - exp(-dt / TAU))`, `TAU = 30 s`, so a 1 s poll, a
2 s background poll and a short foreground catch-up all smooth alike; `dt = 0` changes nothing. Both averages start at 0
(the first CPU delta is zero today), so a new heavy process reaches about 63% of its load after 30 s and 90% after about a minute. System Idle stays 0.
The score is the largest of the three. Whole percent resolution means most processes read 0-2%; ties sort by id as
today.

**Dead band.** The shown score is a whole percent that moves only when the underlying value is a full point away:
`if x < 0.5 { shown = 0 } else if (x - shown).abs() >= 1.0 { shown = x.round() }`: the snap to 0 lets an idle process leave 1% (the average only decays toward 0), so idle processes add nothing to their parents. An unchanged shown value is not sent to the tree, so the displayed
number, the sort order and the repaint all stay quiet. (This replaces the Low/Medium/High bands floated in the
discussion: one rule instead of thresholds plus a hold time.)

**Roll-up.** A parent's score is the sum of its family's shown scores, as `Size` already sums family CPU; it can exceed
100% and is shown uncapped. Headed "Load"; tooltip "the most a process uses of CPU, memory or I/O, as a share of the
machine, steady over about 30 s; a parent adds its children".

`Procs` lives in `crates/plumb-core/src/procs.rs`; `Procs::new(metric)` returns `(Self, Tree)` today and becomes `Procs::new(metric, cores, ram)` (callers: the `window/procs.rs` rebuild, the procs.rs tests, the app.rs tests), `ram` being total physical memory (the right element of `Snapshot.ram`'s tuple, `physical_memory()`), since `apply` takes none. The reconciler creates it from the first snapshot; `Snapshot` gains `cores` (the logical
processor count, read once by the poller with `GetActiveProcessorCount`, not the affinity-limited
`available_parallelism`). A rebuild of `Procs` (many dead nodes, a view switch) without carrying state restarts the averages, so the column
would read low for about a minute after one. The rebuild fires when dead nodes pass `MAX_DEAD` (20_000), so churny sessions (builds) can hit it repeatedly. Decided 2026-11-11 (m1): the rebuild carries each process's smoothed state (EMAs and shown value) into the new `Procs`, keyed by (pid, created), so the order does not reshuffle; a test rebuilds mid-trace and checks the shown values are unchanged.

### The column

- A fifth number column, default width 72 (not 96: at the 640 DIP minimum window five columns of 100/130/90/96/72 leave
  Name 140, against 120 for `MIN_NAME_W`; with 96 Name would be 116). A test pins `Layout::new(640, 400).column(0).1 >=
  MIN_NAME_W`.
- Sites that hard-code four numbers or five columns, all moved to five numbers and six columns: `Layout::col_w` and its
  doc (layout.rs ~70, 115), `COL_W`, `column`, `set_col_w`, the header hit-test loop and drag-resize (layout.rs ~400, 403),
  `ViewKind::sort_key` (app.rs ~68-72) and the title array (app.rs ~51), `draw.rs` header (~692: titles, right-align at
  ~703, the "≥" marker at ~700), the skeleton rows (~1063) and the row cells (~1175; `last_two` stays, the score is a third
  cell from a new `ViewKind::score_text`), `app.rs` ~1264 (`col_w[c - 1]`), the saved-state loader (`s.column < 5` in
  app.rs ~463, so a saved Score sort is restored), and the tests that name column indices (app.rs ~1521, 1955, 2452;
  layout.rs ~475, 512-518; render.rs ~534).
- Header tooltips do not exist today: `App::tip_target` (app.rs ~733-745) handles buttons, the status bar and rows only, and `Hit::Header` is never consulted. Add a header branch with a `TipKey` and a per-view text source for the Score column (the other headers get none).
- More sites: draw.rs right-align `matches!(c, 1 | 3 | 4)` (~701) gains 5, and the "≥" test (~699) gains a `c == 5` Disk case; layout.rs `hit` loops `(1..5)` / `(0..5)` (~399, 403) become `(1..6)` / `(0..6)`; `titles` returns `[&str; 6]`; the app.rs ~2452 test (`col_w[..3]`, `COL_W[3]`) is extended.
- `SortKey::Score` reads `tree.subtree_score` in `rows.rs::order`; it is `sort_key(5)` in both views.
- The map hides at a wider window: `tree_min` (MIN_NAME_W + columns + scrollbar) goes from 548 to 620, so the map appears from 785 DIP wide instead of 713 (layout.rs `tree_min`, `map_shown`; their tests and the render shots move with it). The default sort and the Size column are unchanged.

## Tests

- `score.rs`: `weight` at 0, 29, 30, 365, 730 days and 10 years, an unknown and a future mtime; `file_score` at the
  extremes (no overflow at `u64::MAX`); the smoother against a hand-computed sequence for 1 s and 2 s steps, `dt = 0`,
  and convergence; the dead band holds a ±0.9 jitter and follows a ±1.0 move and decays to 0 once x < 0.5 (an idle process shows 0, not 1); a steady x in 0.5-0.99 may show 0 or 1 depending on history, so tests do not assert one value there; the share formulas (units).
- `Tree`: a drift test in the style of `rows.rs` ~486-499: random `add_child`, `set_size`, `set_mtime`, `remove`,
  `reparent` (and `set_size` on folders and the root, which must change nothing) on a tree with `now`, after each step every `subtree_score` equals a recompute by walking the tree with `file_score` (an oracle in the test; there is no Tree-to-builder rebuild); DEAD
  and folder nodes never change the sums; a tree without `now` stays zero.
- `Procs`: a recorded trace (a table in the test) where raw CPU jitters 5-25%: the shown score changes at most a few times
  while the raw value changes every sample; a parent sums its children; an exited process leaves no residue; pid 0 stays
  0; a rebuilt `Procs` starts at 0.
- UI: `sort_key(5)` per view; `column(5)` fills the row width; the 640 DIP Name width above; a saved Score sort is
  restored; a render shot with the column at its minimum; a `--selftest` check that a real scan shows a Stale value for an
  old file.

## End-state fit

- New: `score.rs`; `Tree::subtree_score`, `now`, `rescore`, `add_score`; `SortKey::Score`; a fifth column; `cores` on
  `Snapshot`.
- Replaces nothing: Items, Modified, CPU and I/O stay (the raw numbers stay useful beside a steady one). Deletes nothing.
  No shim.
- The safety grade from the known-space table later multiplies into `file_score`; the function takes `size` and `mtime`
  now and leaves room.
- Kept from the draft after the deletion pass: EMA plus dead band (an average alone rounds back and forth at x.5, a dead
  band alone cannot hold 5-25% jitter), the I/O term, the age ramp. Removed: the Capacity struct, memory smoothing,
  per-method score hooks, the bands.

## Limits and parked (to add to `PARKED.md` with the implementation)

- Ages do not move while a window stays open; a rescan (F5) or a live change refreshes it. Trigger: a report of a stale
  score after days open.
- `IO_FULL` is fixed, not the disk's measured speed. Trigger: an NVMe-speed process always reading low, or a slow disk
  reading too high.
- A modified time is a weak proxy for "unused" (backups, game assets read often).
- Age ramp vs a single cutoff: decided for the ramp 2026-11-11 (p1; surveyed against cutoff, stepped tiers and a
  user-set cutoff). A cutoff would drop `weight()`, one constant and the 128-bit step; revisit only if the column
  should read as an exact byte count.
