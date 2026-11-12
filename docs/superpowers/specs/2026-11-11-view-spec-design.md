---
status: active
---

# One descriptor per view kind

Origin: cleanup plan `docs/superpowers/plans/2026-11-11-cleanup.md` items A1, A2, A5 (parked in `PARKED.md`). Follows
v0.13. The user skipped the questions; every call below is mine until they decide it. Review: round 1 (one Sonnet
reviewer, fact-check and a fewer-concepts pass) corrected the counts, cut `Align`, the dim tag and the three capability
fields, and added the state-handling steps.

## Problem

The row table is "six columns, parameterised by `ViewKind`", but the parameters are spread over hand-indexed code:

- `ViewKind` (`app.rs:41-125`) has seven methods that `match` on the kind or on `(kind, Metric)`: `titles`,
  `score_text`, `score_help`, `size_text`, `sort_key`, `class_label`, `last_two`. `size_text` is also called outside the
  rows, with plain values: tile tip, process tip, status line (`app.rs:782, 812, 1364`).
- `draw.rs` reads the table by position; `COL_THIRD`/`COL_FOURTH` exist only because columns 4 and 5 mean different
  things per kind (non-test uses: `draw.rs:699` header, `1063` skeleton, `1176`, `1179` rows). The "at least" marker
  is `Size` for both kinds, plus `Score` and column 4 for Disk only (`draw.rs:699`).
- About 17 non-test sites test `kind() == Processes` for behaviour (`app.rs` 507, 776, 810, 844, 856, 885, 939,
  1110-1111, 1403, 1467, 1472, plus `views[..].kind` at 1333 and 1452; `app/held.rs:41`, `app/marks.rs:63`,
  `app/map.rs:55`, `window.rs:711`). Only about 5 are table data (the layout flags and `draw.rs:1145`'s folder icon).
- `Layout`'s `cancel`, `basket`, `metric`, `held`, `banner`, `toast` flags are copied from `App` at 4 non-test sites
  (`app.rs` 508 `swap_slot`, 642 banner, 1108-1111 `handle`, 1472 `scene`). `metric` and `held` are always set from the
  same expression: one bit stored twice.
- `State.sort.column` is a header index: it means "Items" on a disk and "CPU" on processes, and `State.columns` widths
  are shared by both kinds. Positional state also lives in `Hit::Header(c)` and `layout.col_w`.

## Goal and non-goals

Goal: the per-kind differences that are data live in one const table per kind, read by the header, the Score, Size and
last-two cells, sort and the tooltip; the layout flags are derived in one place; saved sort and widths belong to a
view kind.

Non-goals: no change to what any view shows or how it behaves; no change to the process menu, kill, suspend or
held-files logic (behaviour stays code, D1); `Slot`/`Run` (cleanup A4) and where pure code lives (A7) are untouched. The
Name and % of parent cells stay drawn by `draw.rs`: Name carries the icon, chevron, pills and filter tint, and % of
parent computes a bar from `denom`, `used`, `unscanned` and `partial` (`draw.rs:1130-1174`); neither is a function of
`(tree, id)`.

## Design

### ViewSpec (A1)

`const DISK: ViewSpec` and `const PROCESSES: ViewSpec`, reached by `ViewKind::spec(self) -> &'static ViewSpec`: plain
data and `fn` pointers, no trait, no generics.

- `cols: [ColSpec; 6]`, `ColSpec { id: ColId, title: fn(Metric) -> &'static str, sort: SortKey, partial: bool }`.
  `ColId` is an enum (`Name, Score, Pct, Size, Items, Modified, Cpu, Io`); `sort` carries the `Aux` index once (the
  `Aux(0/1)` in `sort_key` and the `aux[0/1]` reads in `last_two` are one fact today, written twice). `partial` is true
  for `Size` in both kinds and for `Score` and column 4 in Disk.
- Cell text: `score_text: fn(&Tree, NodeId) -> Show`, `size_text: fn(Metric, u64) -> Show` (kept as a value function:
  three callers pass plain values), `last_two: fn(&Tree, NodeId) -> (Show, Option<Show>)`. The row loop keeps drawing
  Score only when `subtree_score != 0` and in `fg`, the others in the fixed dim; alignment and dimness follow column
  position, not table fields.
- `score_help: &'static str`; `class_label: fn(Class) -> &'static str`.
- No capability fields: `has_metric`, `has_held` and `folder_icons` are the one bit "Processes" and stay
  `kind() == Processes`, except that `Layout.metric` and `Layout.held` merge into one `Layout.procs`.

Removed: the seven `ViewKind` methods, `COL_THIRD`, `COL_FOURTH` (replaced by `COL_COUNT - 1` where the last column is
meant), the `s.kind == Disk` tests in `draw.rs` that only pick text or the marker. `COL_SCORE`, `COL_PCT`, `COL_SIZE`
stay (the same four columns in both kinds).

### Layout flags (A2)

One `App::layout_now(&self) -> Layout` builds every flag; the four sites call it, and `with_cancel`/`with_basket` and
`Scene`'s hand-built `Layout { .. }` go. `Layout` keeps owning geometry.

### Sort and widths per view kind (A5)

- Storage: `State` holds `disk` and `processes` tables, each `{ sort: { col: ColId, descending }, widths: { ColId: f32 } }`.
  The old top-level `sort` and `columns` keys are dropped; an old file parses to defaults (`#[serde(default)]` ignores
  unknown keys), a one-time reset of sort and widths, no shim.
- In memory, `App` holds both kinds' sort and widths at once (`restore()` runs before any view is active,
  `window.rs:1758-1760`), and keeps `sort` as a header position; `ColId` exists only at the serde edge and in the
  table.
- Touch points: `swap_slot` (`app.rs:502`) swaps sort and widths when the kind changes; `fresh_model`'s resort
  (`app.rs:571-606`) reads the active kind's sort; `remembered()` saves both kinds; `Hit::Header` and `layout.col_w`
  stay positional.

## Decisions

**Decide D1 (what the spec holds).** Searched: capability fields (removed: one bit, already `kind()`); `Align` and a dim
tag (removed: follow position); a cell function for Name and %-of-parent (not expressible). Remaining question:
- d1 **Data and pure fns only** (recommended; chosen 2026-11-12 as k1, with A3 as the follow-up): behaviour stays behind `kind()` branches. Pro: no new abstraction;
  Con: about 12 branches remain (real behaviour differences).
- d2 **A trait with behaviour methods** (`menu`, `kill`, `held`, `map_tiles`). Pro: no `kind()` branches; Con: puts
  the process code behind an interface with one other implementor, and `Slot`/`Run` (A4) must change with it.

**Decide D2 (the stored format).** Searched: `ColId` itself. Per-kind storage already fixes the bug of one index
meaning two things; `ColId` only protects against a reorder inside a kind, which the Score column just did.
- e1 **Per-kind tables keyed by `ColId`** (recommended; chosen 2026-11-12 as g1): the 0.13 misalignment cannot recur; the next MINOR (0.14.0) per
  `VERSIONING.md`. Con: one more concept, and one more reset on upgrade.
- e3 **Per-kind tables of positions** (`sort` index, `widths` list), no `ColId`. Pro: one concept fewer; Con: a
  future reorder repeats the 0.13 misalignment.
- e2 **A1 and A2 only**, `State` untouched (0.13.x, no format change). Pro: no reset; Con: `Sort` keeps meaning two
  things.

**Decide D3 (order and releases).**
- f1 **Three commits, one release** (recommended; chosen 2026-11-12 as i1): S1 `ViewSpec`, S2 `layout_now`, S3 per-kind state with the 0.14.0
  bump; each step passes the render and selftest suites alone.
- f2 **S1+S2 as 0.13.1, S3 as 0.14.0**. Pro: the refactor ships alone; Con: a release with no visible change.

## Checks

- S1: tests and clippy green; render tests and selftest unchanged (the existing `titles`, `sort_key`, `last_two` tests
  become table-driven over `spec()`). Guard test: each kind has six distinct `ColId`s and the first four `ColId`s
  (`Name, Score, Pct, Size`) agree across kinds (titles, cells and help differ by design).
- S3: a state round-trip per kind; an old-format file parses to defaults; the selftest's sort checks. The old-format
  tests (`app.rs:2504-2515`, `state.rs:90-106`) are rewritten.
- Size: the cleanup plan's "about -150 lines" is not re-measured and is probably optimistic once the state tests are
  rewritten.

## End-state fit

`ViewSpec`, `ColSpec`, `ColId` and the two consts in a new `crates/plumb/src/view_spec.rs`; `layout_now` in `app.rs`;
per-kind state in `state.rs`. Replaces the seven `ViewKind` methods, `COL_THIRD`/`COL_FOURTH`, `Layout.metric`/`held`
(now `procs`), the hand-built `Layout` copies and `State.sort`/`State.columns`. Deletes about 5 `kind()` branches and
their per-kind tests.

## Open

- If a future per-kind difference needs its own alignment or dimness, add the field then.
