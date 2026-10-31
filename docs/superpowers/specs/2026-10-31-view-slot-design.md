---
status: draft
---

# One struct per side for per-view state (cleanup item A1)

Item A1 of `docs/superpowers/plans/2026-10-31-cleanup.md`. Decided with the user, 2026-10-31: **g1** (one struct per
side, one swap), **h1** (the window's runtime separate from the app's slot). The `Vec<Slot>` form without swapping
(**g2**) is deferred, to be revisited after this lands: `PARKED.md`.

## Problem

Per-view state is declared three times on each side and kept in step by hand:

- `app.rs`: 12 fields in `App` (`tree`, `model`, `scroll`, `scan`, `summary`, `live`, `refreshing`, `preview_paused`,
  `filter`, `basket`, `proc_info`, `proc_metric`), the same 12 in `Parked`, and 13 `swap(` lines in `swap_slot`.
- `window.rs`: 8 fields in `Win` (`progress`, `rx`, `preview`, `background`, `watch`, `dir_pending`, `dir_failed`,
  `procs`), the same 8 in `View`, and 8 `swap(` lines in `swap_window_fields`.

A field added to one list and forgotten in another is a silent bug: a view that comes back with another view's value.

## Design

**Goal:** a per-view field is declared once and swapped by construction. Behaviour is unchanged.

- `app::Slot` replaces `Parked`: the 12 fields above, `#[derive(Default)]`, fields `pub`, the existing accessors
  (`has_tree`, `tree`, `tree_mut`, `scan`) kept. `App` holds `pub slot: Slot` in place of the 12 fields.
- `window::Run` is the 8 window fields, `#[derive(Default)]`. `Win` holds `run: Run` in place of them. `View` becomes
  `{ app: Slot, run: Run, used: u64 }`.
- `App::swap_slot(&mut self, p: &mut Slot)` becomes `swap(&mut self.slot, p)`, then `swap(&mut self.slot.filter.ui,
  &mut p.filter.ui)` (the query text belongs to the window), then the lines that already follow (`basket_rev += 1`,
  the layout flags, `set_scroll`, `map_reset`). `swap_window_fields` becomes `swap(&mut self.run, &mut d.run)`.
- Everything else (`held`, `map_*`, `sort`, pointer state, `borrowed`, `in_slot`, `park_active`, `unpark`, `evict`)
  stays as it is. `held` is not swapped today and is not made so.
- Access is by field path (`self.slot.tree`, `self.run.rx`), not by accessor methods, so disjoint borrows keep working.
  App-side code outside `App` reads `app.slot.tree` where it read `app.tree`.

**Cases subsumed:** every per-view field on both sides, and the three lists that mirror them. **Not subsumed:**
`filter.ui` (stays a swap-back), the layout flags and cache resets that follow a swap (they are consequences of the
new view, not state), and the `borrowed` mode (it controls timers, not data).

**Deleted:** `Parked`'s field list (replaced by `Slot`), 13 + 8 `swap(` lines, and the 20 mirrored field declarations
on `App`/`Win` and `View`.

## Steps

1. **App side.** Rename `Parked` to `Slot`, move the 12 fields of `App` into `slot`, rewrite `swap_slot`, fix every
   site (about 221 in `app.rs` and `app/*`, 14 in `window`, 44 in tests and selftests). Commit when `cargo test
   --workspace`, clippy and `--selftest`, `-watch`, `-delete`, `-tooltip` pass.
2. **Window side.** Add `Run`, move the 8 fields of `Win` and `View`, rewrite `swap_window_fields` (about 66 uses).
   Same checks.
3. **Add the guard.** A test that parks a view with every `Slot` and `Run` field set to a non-default value and
   checks that each survives a swap in, a swap out and an `in_slot` round trip. Written with a destructuring pattern
   (`let Slot { tree, model, .. } = ..` without `..`), so adding a field without updating the test fails to compile.

Each step is a mechanical rename with no logic change; the review is `git diff` showing only `self.x` to
`self.slot.x` and moved declarations.

## Risks

- A field that was deliberately not swapped (`held`) could be moved in by mistake; the list above is the whole set.
- `filter.ui`: the swap-back must stay after the whole-slot swap, or the query text would follow the view.
- `--selftest-live` needs elevation and cannot run unattended; the window step changes code it exercises. Ask the
  user to run it elevated after step 2.

## Check

`cargo test --workspace` (412 plus the guard), clippy clean, the four selftests per step, `--selftest-live`
elevated once after step 2. No timing claim is made, so no timed run.
