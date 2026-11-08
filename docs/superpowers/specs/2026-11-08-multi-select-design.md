---
status: done
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
`picked: Vec<u64>` (bitset over node indices via `bits::{get,set}`, which grow on write; no resize needed), `picked_n: usize` (set bits, kept by every
method that writes `picked`), `anchor: Option<NodeId>`.

Invariant: `picked` holds only ids that have a row, so `picked_n` is the visible count and `targets()` needs no
visibility test. The focus may be unpicked (after Ctrl+click off). `collapse` clears the bits of the rows it drains
(O(drained)); `rebuild` (filter, sort, refresh) keeps only ids that still have a row, skipped when `picked_n == 0`
(it already walks all rows). When `set_filter` or `refresh` moves the focus to an ancestor (or clears it), they do the
same to the set: `picked = {focus}`, or empty with no focus, so actions follow the focus as they do today. `select(id)`
of a hidden id (`reveal` under a filter) leaves `picked_n == 1` with no row; accepted, as the focus is hidden then too.
Cost: `targets()` is O(rows) per action; the status line and menu read `picked_n`, O(1).

| Method | Effect |
|---|---|
| `select(id)` (existing signature) | `picked = {id}`, `selected = anchor = id` |
| `extend_to(id)` | `picked =` rows between the anchor and `id` by row order, inclusive; `selected = id`. The anchor is kept; if it has no row (collapsed away or filtered) or is `None`, the current focus is the anchor, or `id` if there is no focus |
| `toggle_pick(id)` | flip `id`; `selected = anchor = id`. Unpicking the last row leaves an empty set (Del and Ctrl+C then do nothing, as in Explorer) |
| `select_all()` | `picked =` every row; focus and anchor unchanged (focus `None`: the first row); no-op on an empty list |
| `targets() -> Vec<NodeId>` | picked rows in row order |
| `picked_count()` | `picked_n` |
| `is_picked(id)` | for drawing |
| `handle_key(tree, key, page, extend)` | as today for the focus; with `extend`, Up/Down/PageUp/PageDown/Home/End end in `extend_to(new focus)` instead of `select`. Left/Right behave as plain (they `select`, dropping a set) |
| `select_parent()` | `select(parent row)` if the focus has a parent with a row (Backspace) |

A rescan or a view switch builds a fresh `RowModel` (`Snapshot::take/apply`, `restore.rs`), which restores only the expanded set, the focus and the scroll; the picked set and anchor are dropped and the set becomes `{focus}`. Accepted: a rescan replaces the tree under the selection.

### Input (plumb)

Modifiers travel in the event, read by `GetKeyState` at dispatch: a held-modifier flag in `App` would have to survive
key-up, focus loss and Alt-Tab.

```rust
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods { pub shift: bool, pub ctrl: bool }
```

`Input::Click(x, y, Mods)` and `Input::Key(Key, Mods)`. `DoubleClick` stays as is: when Shift or Ctrl is down the
window sends `Click` instead (a quick Ctrl+click pair arrives as `WM_LBUTTONDBLCLK` and must not expand the folder).
Tests pass `Mods::default()` (50 `Input::Click(` and 12 `Input::Key(` hits across the crate, plus the modal patterns at `app.rs` ~961, ~985,
~1019, ~1033). New inputs: `Input::Refresh`, `Input::ContextMenu`. Ctrl+A reuses `Edit::SelectAll` and Backspace
`Edit::Backspace`. The outcome-reset list at `app.rs:1019` gains `Refresh` and `ContextMenu`.

| Gesture | Result |
|---|---|
| click on a row | `select(id)` (the held-pane "again" rule as today, for a plain click only) |
| Shift+click | `extend_to(id)` |
| Ctrl+click | `toggle_pick(id)` |
| folder icon | opens or closes and selects as today; modifiers ignored |
| press on a row (plain or Shift; not Ctrl, not on a folder icon, and not the second press of a double-click, which has already toggled the folder), then drag | `extend_to(row under the pointer)`, clamped to the rows on screen and the last row. Past the top or bottom edge the list scrolls 1 to 5 rows per tick by the distance beyond it. State `App.row_drag: bool`, set by the press and cleared on `Release`. After each `Click` the window reads `app.row_drag` and arms a timer (about 40 ms, registered with the others) when it is set; `Release` and the timer's own check disarm it. The timer replays `Input::Drag` at the last pointer position so a held pointer keeps scrolling; when `GetKeyState(VK_LBUTTON) >= 0` (capture lost, no `WM_LBUTTONUP`) it sends `Input::Release` and stops |
| Shift+Up/Down/PgUp/PgDn/Home/End | `handle_key` with `extend` |
| Ctrl+A, filter not focused | `select_all()` (guarded like the existing `Edit::Delete if !focus` arm) |
| right-click on a picked row | selection and focus kept, menu for the set; on an unpicked row: `select(id)`, then the menu |
| Del, filter not focused | `mark_selected()` (below) |
| Ctrl+C, list focused | non-empty paths of `targets()`, one per line (`\r\n`) |
| Enter, filter not focused | toggles the focus row open or closed |
| Backspace, filter not focused | `select_parent()` |
| F5 | no tree: `Effect::Scan(active)`; a tree and nothing cancelable: `Effect::Rescan(active)` (in the Processes view this only kicks the sampler, `window.rs:671`); a view not active or a scan running: nothing |
| Shift+F10, Apps key | the Apps key arrives as `WM_KEYDOWN`, Shift+F10 as `WM_SYSKEYDOWN` (F10 is a system key; the new arm handles only Shift+F10 and passes every other Alt/F10 combination to `DefWindowProc`); both emit `Input::ContextMenu`; the app scrolls the focus row into view and returns `Effect::Menu(focus)`; the window places the menu at the row's left edge, converting the app's dip rectangle (an accessor `focus_row_rect()`) to client pixels with `dpi` and `inset` as the mouse path does, then `context_menu`'s `ClientToScreen` |

The key-repeat guard that exists for Del (`lp` bit 30, `window.rs:1429`) extends to Enter, F5, Ctrl+A and Backspace
(repeat would flip a folder, queue rescans). Filter-box shortcuts keep priority: with the box focused, Shift+Left /
Right / Home / End select text (existing branch), Ctrl+A selects the box text, Enter and Backspace act in the box.

### Set-wide mark and copy (`select.rs`)

`mark_selected()` serves Del and the menu's Mark/Unmark entry. Over the **topmost** targets (no picked ancestor, so
`select_all` then Del marks the roots, not every row), a target counts as marked if `basket.contains(path)`, the exact test `mark_state` uses today. If every topmost target is marked, it unmarks: it removes the exact mark of every picked row.
Otherwise it marks each topmost target not exactly marked (so one child of a marked folder is marked, as today). Refused rows are skipped; the status line says how many
and the first reason.

Copy (Ctrl+C and the menu's Copy paths) takes the non-empty paths of all of `targets()`, nested ones included, one per
line (CRLF-separated).

### Menu over a set

`RowMenu` keeps `raw` (the single row's tree path; empty for a set) and replaces `path` with `paths: Vec<String>`
(the file-system paths to reveal and copy), and gains `count: usize`. `row_menu(id)` reads the model: if `id` is
picked and `picked_count() > 1` it builds the set form. With `count > 1` the window offers only **Mark selected** or
**Unmark selected** (`MarkState` aggregated over the topmost targets: Unmark if all are exactly marked, Mark if any can be
marked, otherwise Refused with the first reason; the entry calls the public `App::mark_selected()`; `CMD_MARK` takes that path when `count > 1` and `toggle_mark_path(&raw)` otherwise) and **Copy paths (n)** (n =
`count`; not offered when every path is empty, as in the Processes view). Open, Reveal, Who is using this, Properties
and the process commands stay single-row. A single selection builds today's menu. `Effect::Menu(NodeId)` is unchanged.

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
  (-> set-wide copy), `RowMenu.path` (-> `paths`, plus `count`), the `selected`-only highlight in `draw.rs`.
- **Tripwire:** `app.rs` grows only by `Input` and delegations; behaviour lives in `app/select.rs`. `key_input` gains
  arms for F5/Apps/Shift+F10; if it grows past a screen it splits first.

## Checks

- Core (`rows/pick.rs`): extend in both directions, from no anchor, with a stale anchor; toggle on, off, and from
  nothing picked; `select_all` (empty list, no focus); collapse and filter drop hidden picks, `picked_n` stays exact; focus moved to an ancestor yields `{focus}`;
  focus `None` empties the set; `handle_key` with `extend` per key; Left/Right drop a set; `select_parent`.
- App (`select.rs`): each gesture row; Del all-marked / mixed / with a refused row / over nested picks / a child of a marked folder (marks, as today); Ctrl+A with and
  without filter focus; Ctrl+click is never an expand; drag past an edge scrolls and extends; Ctrl press starts no
  drag; right-click on picked vs unpicked; F5 per case; Enter and Backspace guarded by filter focus; `Click` with
  Shift on the focus row does not fire the held-pane "again".
- Render test: picked rows highlighted, only the focus has the bar.
- GUI selftest: Shift+Down twice, assert three rows picked; existing selftests still exit 0.
- Fast loop: `cargo test -p plumb-core rows` and `-p plumb select` (seconds); full suite and selftests once at the end.

## Risks

- `targets()` over millions of rows after `select_all`: one O(rows) walk per action, not per paint (paints read `picked_n`).
- A timer left running when capture is lost is handled by the `VK_LBUTTON` check above.
- `Window::selected_path` and `row_menu` change shape together; both are covered by the tests above.
