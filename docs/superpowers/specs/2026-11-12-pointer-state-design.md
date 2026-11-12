---
status: draft
---

# Pointer state as one value (cleanup plan item B2)

## Intent

"Which thing is the left button holding" is spread over six fields: `pressed`, `grab`, `col_drag`, `map_drag`, `row_drag`
(on `App`) and `filter_drag` (on `Win`). Only one can be live, yet nothing says so: `Release` clears five by hand, the
window clears the sixth in `WM_LBUTTONUP`, and a view switch and the drag timer's Release path clear some of them
(`PARKED.md`, multi-select leftovers (a)). One enum makes the exclusivity a fact of the type and gives every clearing path
one assignment.

Goal: one `Pointer` value on `App` replaces the six. Non-goals: `hover`, `hot`, `thumb_hover`, `map_hover` and the tooltip
target stay as they are (they are "what is under the pointer", independent of the button, and `hot` must stay set while a
button is down); no change to hit-testing (parked P5); no change to what any drag does.

## Facts checked in the code (2026-11-12)

- A press targets exactly one thing: `click` sets `pressed` only when the point is inside `hot` (an icon button), and a
  divider, the map divider, the thumb and a row are not `hot` rectangles; `Scene.pressed` already ORs in `grab`
  (`app.rs` `with_scene`). So the six are mutually exclusive today in every path that sets them.
- Readers: `Input::Drag` is an `if/else` chain over four of them; `over_divider` reads two; `row_dragging()` feeds the
  window's drag timer; `with_scene` reads `pressed` and `grab`; `Input::Release` clears five. 27 mentions in `app.rs`, 2 in
  `app/select.rs`, 6 in `window.rs`; tests read `a.pressed` (4 asserts) and set `a.col_drag` (3 places).
- `filter_drag` is on `Win` because the caret position needs a DirectWrite hit-test (`filter_byte`); `App` only receives
  `Input::Edit(Place { at, extend })`.

## Design

```rust
/// What the left button is holding since it went down: one value, so two drags cannot be live.
enum Pointer {
    Up,
    /// Down on a `hot` control (an icon button or a header cell).
    Button,
    /// Dragging the scrollbar thumb; `grab` is the distance from the thumb's top to the grab point.
    Thumb { grab: f32 },
    /// Dragging a column's left edge: the column, the pointer x and the column width at the start.
    Column { col: usize, x0: f32, w0: f32 },
    /// Dragging the map divider: the pointer x and the pane width at the start.
    MapDivider { x0: f32, w0: f32 },
    /// Down on a row; moving extends the selection (`drag_select`).
    Rows,
    /// Down in the filter box; moving extends its text selection (decision f).
    Filter,
}
```

- `App.pointer: Pointer`, private; `App::pressed()` is `matches!(self.pointer, Button | Thumb { .. })` (the draw
  `Scene.pressed` stays a `bool`, so `draw.rs` and the render tests do not change).
- `click` assigns `pointer` at its top, as `pressed` is assigned today (`Button` if the point is inside `hot`, else `Up`),
  and the arms override it: `Hit::Divider` -> `Column`, `Hit::MapDivider` -> `MapDivider`, thumb -> `Thumb`, a row
  (`click_row`) -> `Rows`, `Hit::FilterBox` -> `Filter`. `click` has many early returns (Elevate, Toast, filter, Chip,
  Cancel, Rescan, Metric), so an assignment at the end would be skipped; at the top it also ends any stale drag.
- `Input::Drag` becomes one `match self.pointer`; `Input::Release` becomes `self.pointer = Pointer::Up`.
- `over_divider` reads `matches!(self.pointer, Column { .. } | MapDivider { .. })`; `row_dragging()` reads `Rows`.

## Decisions (open until the user answers)

**f. The filter-box drag.** f1 (recommended): `Pointer::Filter` lives in `App`; `click`'s `Hit::FilterBox` arm sets it
(the double-click path reaches `click` too), `WM_MOUSEMOVE` asks `app.filter_dragging()`; `WM_LBUTTONUP`'s
`filter_drag = false` is deleted because `Release` ends it. Deletes the sixth field and fixes the timer-path half of
leftover (a); no new `App` method beyond the getter. f2: leave `filter_drag` on `Win`, five fields become one, the filter drag stays a second flag cleared in
`WM_LBUTTONUP`. f2 is smaller but keeps one "which drag" flag outside the value.

**v. What else ends a drag.** v1 (recommended): `Release` and `set_active` (a view switch) both set `Up`; fixes the view
switch half of leftover (a); it is a behaviour change unrelated to the state shape, so it can be left out. v2:
`Release` and the top of `click` only (a stale drag also ends at the next press). Focus loss mid-drag (leftover (b)) is
left parked either way: `WM_CAPTURECHANGED` is not handled anywhere today, so it would be a new message handler that
needs a window to test.

## End-state fit

- **Where:** `Pointer` and the `pointer` field sit in `app.rs` beside the fields they replace; `pressed()` there;
  `row_dragging()` stays in `select.rs` (reads `Rows`); no new file (the reviewer's deletion pass: the type is 8 lines
  and `app.rs` is already split by `tests.rs`). `window.rs` loses `filter_drag` (f1).
- **Replaces/deletes:** 6 fields and their initialisers; five clear lines in `Release`; the drag `if/else` chain; two
  `is_some()` terms in `over_divider`; the `WM_LBUTTONUP` flag clear (f1).
- **Does not subsume:** hover state (`hover`, `hot`, `thumb_hover`, `map_hover`, tooltip), the P1 "resolved pointer"
  idea beyond the drag half, and P5 (hit-test once per input).
- **Guard:** none can exist for "a seventh drag flag"; the enum is the guard (a new drag is a new variant). Parked entry
  P1 is rewritten to the hover half only when this lands.

## Tests

- Existing: the three `a.col_drag = Some(..)` setups become `a.pointer = Pointer::Column { .. }`; the four `a.pressed`
  asserts read `a.pressed()`; `select.rs`'s `row_dragging` asserts are unchanged.
- New (one each, written first and seen failing): (1) pressing a divider, then a row, leaves only `Rows` live; (2) a view
  switch mid-column-drag ends it (v1); (3) with f1, `Release` ends a filter drag; (4) `Release` from each variant gives
  `Up` (one loop over a list of presses).

## Review focus

- A press that starts nothing (empty area, an early-return arm) must leave `Up` or `Button`, not the previous value:
  `click` assigns at its top.
- `Button` pressed on a `hot` that changes between press and release: unchanged from today (`pressed` was a plain bool).
- A drag started, then a modal opens: `handle` routes all input except Resize and Release to `modal_input`, which ignores
  Drag, so the drag is frozen and `Release` still ends it; unchanged.
- A Button press and a Filter press never coincide (`FilterRegex` and `FilterClear` win over `FilterBox` in `layout.rs`
  hit-testing), so `Button` and `Filter` do not need to overlap.

## Size

About -20 lines net, no new file; 6 fields become 1. Mechanical apart from decisions f and v.

## Review

One round: a Sonnet reviewer briefed to find a shape with fewer concepts and to fact-check. It found no variant to remove
and confirmed mutual exclusion; it caught three errors, fixed above: `click` assigns at its top (early returns), the
modal case, and that `WM_CAPTURECHANGED` is not handled today; it also removed `begin_filter_drag` and `app/pointer.rs`.
