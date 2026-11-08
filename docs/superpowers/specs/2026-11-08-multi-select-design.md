---
status: draft
---

# Multi-select and keyboard parity (v0.12 candidate)

Decided with the user, 2026-11-08: **o1** (selection separate from deletion marks), **p1** (multi-select), **p2**
(keyboard parity). Type-to-jump, multi-process actions and the column/wheel niceties are parked (`PARKED.md`).

## Understanding

**Outcome.** The list selects like other file-list apps: Shift+click, Ctrl+click, Shift+arrows, Ctrl+A, dragging over
rows, and a context menu, Del and Ctrl+C that act on everything selected. Plus the keyboard habits missing today: F5,
Enter, Backspace, Shift+F10 / the Apps key.

**Constraints (the user's words).** Marks stay a separate, deliberate two-step delete; a selection never deletes
anything. Nothing else about the delete flow changes.

**Success.** Each gesture works as in Explorer, covered by core and app tests; one GUI selftest drives a Shift-extend;
docs list the new keys.

**Assumptions (change on review).** Explorer is the reference. Dragging starts on a row (rows are full width, so
there is no empty space for a rubber band). Rows hidden by collapse or a filter keep their pick but never take part
in an action.

## Today

- `RowModel.selected: Option<NodeId>` is the only selection; about 90 sites read `selected()` or call `select(id)`.
- `Input::Click/Key` carry no modifier keys; `GetKeyState` is read only in `window.rs` (filter editing, Ctrl keys).
- Marks are the basket (`plumb-core/src/basket.rs`); `Del` runs `marks.rs:toggle_mark_selected`.
- `Edit::SelectAll` and `Edit::Backspace` act only with the filter box focused; Enter only there too
  (`filter.rs:filter_enter`). F5 and Shift+F10 are unmapped: `key_input`/`key_of` return `None`, and `WM_KEYDOWN` never
  calls `DefWindowProc`, so Windows sends no `WM_CONTEXTMENU` for the keyboard. Rescan exists only as `Hit::Rescan`.

## Design

### Placement

| Piece | Home |
|---|---|
| the picked set and its range rules | `crates/plumb-core/src/rows/pick.rs` (an `impl RowModel` submodule, like `filter_tests`/`refresh_tests`); fields stay in `RowModel` |
| input-to-selection mapping, Del and copy over a set, Enter, Backspace, F5 | `crates/plumb/src/app/select.rs` (new, like `marks.rs`, `held.rs`, `map.rs`; `app.rs` is 3076 lines and gains only the `Input` changes and delegations) |
| modifier reads, key mapping, drag-scroll timer, keyboard menu point | `window.rs` (owns `GetKeyState`, capture, timers) |

### Model (plumb-core)

`selected` stays the **focus** (the cursor row), so the ~90 readers are untouched. New state in `RowModel`:
`picked: Vec<u64>` (bitset over node indices, the `expanded` pattern), `picked_n: usize` (set bits, kept by every
method that writes `picked`), `anchor: Option<NodeId>`.

No invariant ties `picked` to the focus or to the visible rows. `targets()` walks `rows` and tests bits, so a hidden
or vanished id never acts, and an unpicked focus (after Ctrl+click off) is allowed. Nothing prunes `picked` on
collapse, filter, sort or rescan; a hidden pick reappears when its folder reopens. The one cleanup: when `set_filter`
or `refresh` leaves the focus `None`, `picked` is cleared. Cost of `targets()` and of `picked_count()` is O(rows),
paid only when `picked_n > 1` (the status line and menu read the count each paint).

| Method | Effect |
|---|---|
| `select(id)` (existing signature) | `picked = {id}`, `selected = anchor = id` |
| `extend_to(id)` | `picked =` rows between the anchor and `id` by row order, inclusive; `selected = id`. The anchor is kept; if it has no row (collapsed away, filtered, rescanned) or is `None`, the current focus is the anchor, or `id` if there is no focus |
| `toggle_pick(id)` | flip `id`; `selected = anchor = id`. Picking when nothing is picked first picks the focus, so Ctrl+click on a fresh list keeps the row you were on |
| `select_all()` | `picked =` every row; focus and anchor unchanged (focus `None`: the first row); no-op on an empty list |
| `targets() -> Vec<NodeId>` | picked rows in row order |
| `picked_count()` | `targets().len()`, short-cut to 0 or 1 from `picked_n` |
| `is_picked(id)` | for drawing |
| `handle_key(tree, key, page, extend)` | as today for the focus; with `extend`, Up/Down/PageUp/PageDown/Home/End end in `extend_to(new focus)` instead of `select`. Left/Right behave as plain (they `select`, dropping a set) |
| `select_parent()` | `select(parent row)` if the focus has a parent with a row (Backspace) |

### Input (plumb)

Modifiers travel in the event, read by `GetKeyState` at dispatch: a held-modifier flag in `App` would have to survive
key-up, focus loss and Alt-Tab.

```rust
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods { pub shift: bool, pub ctrl: bool }
```

`Input::Click(x, y, Mods)` and `Input::Key(Key, Mods)`. `DoubleClick` stays as is: when Shift or Ctrl is down the
window sends `Click` instead (a quick Ctrl+click pair arrives as `WM_LBUTTONDBLCLK` and must not expand the folder).
Tests pass `Mods::default()` (50 `Click` and 12 `Key` call sites, plus the modal patterns at `app.rs` ~961, ~985,
~1019, ~1033). New inputs: `Input::Refresh`, `Input::ContextMenu`. Ctrl+A reuses `Edit::SelectAll` and Backspace
`Edit::Backspace`. The outcome-reset list at `app.rs:1019` gains `Refresh` and `ContextMenu`.

| Gesture | Result |
|---|---|
| click on a row | `select(id)` (the held-pane "again" rule as today, for a plain click only) |
| Shift+click | `extend_to(id)` |
| Ctrl+click | `toggle_pick(id)` |
| folder icon | opens or closes and selects as today; modifiers ignored |
| press on a row (plain, Shift, or the second press of a plain double-click), then drag | `extend_to(row under the pointer)`, clamped to the rows on screen and the last row. A Ctrl press does not start a drag (it would replace the earlier picks). Past the top or bottom edge the list scrolls 1 to 5 rows per tick by the distance beyond it. State `App.row_drag: bool`, cleared on `Release`. A window timer (about 40 ms, registered with the others, running between press and release) replays `Input::Drag` at the last pointer position so a held pointer keeps scrolling; when `GetKeyState(VK_LBUTTON) >= 0` (capture lost, no `WM_LBUTTONUP`) it sends `Input::Release` and stops |
| Shift+Up/Down/PgUp/PgDn/Home/End | `handle_key` with `extend` |
| Ctrl+A, filter not focused | `select_all()` (guarded like the existing `Edit::Delete if !focus` arm) |
| right-click on a picked row | selection and focus kept, menu for the set; on an unpicked row: `select(id)`, then the menu |
| Del, filter not focused | over the topmost targets (those with no picked ancestor, so `select_all` then Del marks the roots, not every row): if all are marked, unmark them; otherwise mark each not yet marked. Refused rows are skipped; the status line says how many and the first reason |
| Ctrl+C, list focused | non-empty paths of `targets()`, one per line (`\r\n`) |
| Enter, filter not focused | toggles the focus row open or closed |
| Backspace, filter not focused | `select_parent()` |
| F5 | no tree: `Effect::Scan(active)`; a tree and nothing cancelable: `Effect::Rescan(active)` (in the Processes view this only kicks the sampler, `window.rs:671`); a view not active or a scan running: nothing |
| Shift+F10, Apps key | `WM_KEYDOWN` emits `Input::ContextMenu`; the app scrolls the focus row into view and returns `Effect::Menu(focus)`; the window places the menu at the row's left edge, converting the app's dip rectangle (an accessor `focus_row_rect()`) to client pixels with `dpi` and `inset` as the mouse path does, then `context_menu`'s `ClientToScreen` |

The key-repeat guard that exists for Del (`lp` bit 30, `window.rs:1429`) extends to Enter, F5, Ctrl+A and Backspace
(repeat would flip a folder, queue rescans). Filter-box shortcuts keep priority: with the box focused, Shift+Left /
Right / Home / End select text (existing branch), Ctrl+A selects the box text, Enter and Backspace act in the box.

### Menu over a set

`RowMenu` holds `raws: Vec<String>` and `paths: Vec<String>` (length 1 for a single row, replacing `raw`/`path`), so
there is one shape, not a set variant. `row_menu(id)` reads the model: if `id` is picked and `picked_count() > 1` it
builds the menu from `targets()`. With `raws.len() > 1` the window offers only **Mark/Unmark selected (n)**
(`MarkState` aggregated: Unmark if all topmost are marked, Mark if any can be, otherwise Refused with the first
reason) and **Copy paths (n)** (empty paths skipped; not offered when all are empty, as in the Processes view).
Open, Reveal, Who is using this, Properties and the process commands stay single-row. A single selection builds
today's menu. `Effect::Menu(NodeId)` is unchanged.

### Drawing and status

`draw.rs:1099` becomes: picked fills with the select colour, else hover; the accent bar marks the focus row only.
`App::write_selection` (`app.rs:1261-1271`, used by the status line and `draw.rs:277`) leads with "n selected" when
`picked_count() > 1`. Marks keep their tint and chip and read independently.

### Out of scope (parked)

Ctrl+Shift+click, Ctrl+arrow and Space (move the focus without changing the selection), Alt+Up, type-to-jump,
End/Suspend over a set, header column menu, Ctrl+wheel, a size total for a set. Entries in `PARKED.md`.

## End-state fit

- **Adds:** `rows/pick.rs`, `app/select.rs`, `Mods`, the picked set, two input variants.
- **Replaces, deleting the old form:** `toggle_mark_selected` (-> set-wide Del in `select.rs`), `Window::selected_path`
  (-> set-wide copy), `RowMenu.raw/path` (-> `raws/paths`), the `selected`-only highlight in `draw.rs`.
- **Tripwire:** `app.rs` grows only by `Input` and delegations; behaviour lives in `app/select.rs`. `key_input` gains
  arms for F5/Apps/Shift+F10; if it grows past a screen it splits first.

## Checks

- Core (`rows/pick.rs`): extend in both directions, from no anchor, with a stale anchor; toggle on, off, and from
  nothing picked; `select_all` (empty list, no focus); hidden picks stay out of `targets()` and return on reopen;
  focus `None` clears the set; `handle_key` with `extend` per key; Left/Right drop a set; `select_parent`.
- App (`select.rs`): each gesture row; Del all-marked / mixed / with a refused row / over nested picks; Ctrl+A with and
  without filter focus; Ctrl+click is never an expand; drag past an edge scrolls and extends; Ctrl press starts no
  drag; right-click on picked vs unpicked; F5 per case; Enter and Backspace guarded by filter focus; `Click` with
  Shift on the focus row does not fire the held-pane "again".
- Render test: picked rows highlighted, only the focus has the bar.
- GUI selftest: Shift+Down twice, assert three rows picked; existing selftests still exit 0.
- Fast loop: `cargo test -p plumb-core rows` and `-p plumb select` (seconds); full suite and selftests once at the end.

## Risks

- `targets()` over millions of rows after `select_all`: one O(rows) walk per action, not per paint (the paint reads
  `picked_n`).
- A timer left running when capture is lost is handled by the `VK_LBUTTON` check above.
- `Window::selected_path` and `row_menu` change shape together; both are covered by the tests above.
