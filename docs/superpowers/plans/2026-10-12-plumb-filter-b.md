# Name filter, plan B: the window (strip, text box, worker, live updates)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The plumb window gets a filter strip above the column headers. Typing in it (Ctrl+F focuses it) filters the tree to matching names and their folders, on a worker thread, and the filter stays correct while live updates change the tree.

**Architecture:** `plumb-core` (plan A) already does matching, hits, the keep set and the filtered row model. Plan B adds, in the `plumb` crate: a pure text-box reducer (`filter_ui`), the strip geometry in `layout`, strip drawing and dimmed rows in `draw`, the filter state and its rules in `app/filter.rs` (debounced run, generation counter, install, live re-test), and the Win32 glue in `window.rs` (`WM_CHAR`, key routing, paste, a 100 ms timer, the worker thread and `WM_FILTER`). One small core addition, `RowModel::update_keep`, lets a live refresh swap in a rebuilt keep set without building the rows twice.

**Tech Stack:** Rust 2024, `windows` crate (Direct2D/DirectWrite/Win32), `plumb-core::filter` from plan A.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-filter-design.md` (amended by Task 7; see "Spec amendments" below).

## Global Constraints

- Matching, hits, keep set and row filtering are `plumb-core`'s (plan A); the `plumb` crate never re-implements them.
- Matching is name-only and case-insensitive, substring by default and regex with the toggle; an empty query means no filter.
- A new query shows the previous result until the new one lands; an invalid pattern keeps the previous result and tints the box.
- The debounce is 100 ms; the scan runs on a worker thread; results of an older generation are dropped.
- Typed text is capped at 256 characters (`filter_ui::MAX_CHARS`).
- IME composition (`WM_IME_*`) is out of scope; `WM_CHAR` covers direct input only. Recorded in the changelog.
- Files are edited with the Edit tool (working copies are CRLF, the index is LF); new files are written with the Write tool.
- Run cargo with `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3` and at most `-j 8`.
- No stray processes: every long command runs to completion; nothing stays in the background. Python is not used.
- GUI runs are hidden (`--selftest`) or offscreen (WARP); nothing pops a window over the user's work.

## Spec amendments made while planning

Task 7 writes these into the spec. Each is a deliberate simplification or a correction found by reading the code.

1. **Up/Down/PageUp/PageDown always go to the tree**, even while the box has focus, so a user can type and then arrow through the results. Left/Right/Home/End/Backspace/Delete go to the box while it has focus. (Spec said all arrows go to the box.)
2. **A click puts the caret at the end of the text**, not under the pointer; placing it under the pointer needs DirectWrite hit-testing in the app layer. Ctrl+F selects the existing text so typing replaces it.
3. **Ctrl+C is unchanged** (copies the selected row's path). Copying the box text is out of scope.
4. **The match count lives in the strip only**; the status line is unchanged.
5. **The worker's result travels over an `mpsc` channel**, with a plain `PostMessageW(WM_FILTER)` after each send, instead of a `Box` in `LPARAM` with a posted-flag handshake: there is at most one message per run, so no coalescing is needed and no raw pointer crosses the message queue.
6. **No `restore::Snapshot` on filter-on.** `RowModel` already keeps the user's expansion aside (`expanded` vs `closed`), so clearing is `set_filter(None)` plus the scroll saved at install. `Snapshot` is still used for a tree swap, after the filter is taken off the old rows first. No `RowModel::user_expanded` is needed.
7. **Long text is clipped, not scrolled**: a query wider than the box shows an ellipsis and the caret may be off-screen.

## File structure

- Modify `crates/plumb-core/src/rows.rs`, `rows/filter_tests.rs`: `update_keep`.
- Create `crates/plumb/src/filter_ui.rs`: `FilterUi`, `Edit`, `push_utf16`.
- Modify `crates/plumb/src/lib.rs`: `pub mod filter_ui;`.
- Modify `crates/plumb/src/layout.rs`: the strip rectangles, `Hit::{FilterBox, FilterRegex, FilterClear}`, tests.
- Modify `crates/plumb/src/draw.rs`: `Theme.error`, `FilterBar`, `Scene.filter`, strip painting, dim rows, `text_width`.
- Modify `crates/plumb/tests/render.rs`: new coordinates, the strip and the error tint.
- Modify `crates/plumb/src/app.rs`: `filter` field, new `Input`/`Effect` variants, click and key routing, `swap_tree`/`begin_scan`/`finish_scan`/`refresh_rows` hooks, test coordinates.
- Create `crates/plumb/src/app/filter.rs`: the filter state and its tests.
- Modify `crates/plumb/src/window.rs`: message wiring, worker, paste, selftest.
- Modify `CHANGELOG.md`, the spec.

---

### Task 1: `RowModel::update_keep`

A live refresh must install a rebuilt keep set and then rebuild the rows once (with the sort caches cleared). `set_filter` rebuilds immediately, so calling it and then `refresh` would build the rows twice.

**Files:**
- Modify: `crates/plumb-core/src/rows.rs` (after `set_filter`)
- Test: `crates/plumb-core/src/rows/filter_tests.rs`

**Interfaces:**
- Produces: `RowModel::update_keep(&mut self, keep: Rc<Keep>)`.

- [ ] **Step 1: Write the failing test.** Append to `rows/filter_tests.rs`:

```rust
#[test]
fn update_keep_then_refresh_equals_a_fresh_filter() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "f")));
    // a new match under `mid` (4): its folder and C: must now show it
    let new = t.add_child(NodeId(4), &u("fresh"), 10, crate::NodeFlags::empty());
    let k = keep(&t, "f");
    m.update_keep(k.clone());
    m.refresh(&mut t, 0);
    let mut fresh = RowModel::new(&t);
    fresh.set_filter(&t, Some(k));
    assert_eq!(ids(&m), ids(&fresh));
    assert!(ids(&m).contains(&new.0));
}
```

- [ ] **Step 2: Run it to see it fail.** `cargo test -p plumb-core --lib update_keep -j 8` → fails to compile: no method `update_keep`.

- [ ] **Step 3: Implement.** In `rows.rs`, directly after `set_filter`:

```rust
    /// Swaps in a rebuilt keep set for the filter that is already active, without rebuilding the
    /// rows. Call [`RowModel::refresh`] next: it clears the stale sort caches and builds the rows.
    pub fn update_keep(&mut self, keep: Rc<Keep>) {
        debug_assert!(self.filter.is_some(), "update_keep needs an active filter");
        self.filter = Some(keep);
    }
```

- [ ] **Step 4: Run it.** `cargo test -p plumb-core --lib -j 8` → all pass (the new test included).

- [ ] **Step 5: Commit.**

```bash
git add crates/plumb-core/src/rows.rs crates/plumb-core/src/rows/filter_tests.rs
git commit -m "Add RowModel::update_keep for live filter refreshes"
```

---

### Task 2: The text box reducer

**Files:**
- Create: `crates/plumb/src/filter_ui.rs`
- Modify: `crates/plumb/src/lib.rs` (add `pub mod filter_ui;` after `pub mod fmt;`)

**Interfaces:**
- Produces: `MAX_CHARS: usize`; `enum Edit { Char(char), Backspace, Delete, Left, Right, Home, End, SelectAll, Paste(String) }`; `struct FilterUi { text: String, caret: usize, regex: bool, focus: bool, all: bool }` (Default) with `edit(&mut self, Edit) -> bool` (true if the text changed) and `clear(&mut self) -> bool` (true if there was text); `push_utf16(pending: &mut Option<u16>, unit: u16) -> Option<char>`.

- [ ] **Step 1: Write the file** (tests included, so they fail only if the code is wrong):

```rust
//! The filter text box as a pure reducer: no Win32 types, so every edit tests anywhere.

/// Longest query accepted, in characters: a guard against pasting a book into the box.
pub const MAX_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    SelectAll,
    /// Pasted text; line breaks and other control characters are dropped.
    Paste(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterUi {
    pub text: String,
    /// Caret position as a byte offset on a char boundary.
    pub caret: usize,
    pub regex: bool,
    pub focus: bool,
    /// The whole text is selected: the next character or paste replaces it.
    pub all: bool,
}

impl FilterUi {
    /// Applies `e`; true if the text changed.
    pub fn edit(&mut self, e: Edit) -> bool {
        let selected = std::mem::take(&mut self.all);
        match e {
            Edit::Char(c) => self.insert(c.encode_utf8(&mut [0; 4]), selected),
            Edit::Paste(s) => self.insert(&s, selected),
            Edit::Backspace | Edit::Delete if selected => self.clear(),
            Edit::Backspace => match self.prev(self.caret) {
                Some(p) => {
                    self.text.drain(p..self.caret);
                    self.caret = p;
                    true
                }
                None => false,
            },
            Edit::Delete => match self.next(self.caret) {
                Some(n) => {
                    self.text.drain(self.caret..n);
                    true
                }
                None => false,
            },
            Edit::Left => {
                self.caret = if selected { 0 } else { self.prev(self.caret).unwrap_or(0) };
                false
            }
            Edit::Right => {
                self.caret = if selected { self.text.len() } else { self.next(self.caret).unwrap_or(self.text.len()) };
                false
            }
            Edit::Home => {
                self.caret = 0;
                false
            }
            Edit::End => {
                self.caret = self.text.len();
                false
            }
            Edit::SelectAll => {
                self.all = !self.text.is_empty();
                false
            }
        }
    }

    /// Empties the box; true if there was text.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        self.all = false;
        had
    }

    fn insert(&mut self, s: &str, replace: bool) -> bool {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            self.all = replace; // an ignored key keeps the selection
            return false;
        }
        if replace {
            self.text.clear();
            self.caret = 0;
        }
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let clean: String = clean.chars().take(room).collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
        replace || !clean.is_empty()
    }

    fn prev(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }
}

/// Folds the UTF-16 units of `WM_CHAR` into characters: a high surrogate is held in `pending` until
/// its low half arrives. A lone surrogate is dropped.
pub fn push_utf16(pending: &mut Option<u16>, unit: u16) -> Option<char> {
    match unit {
        0xD800..=0xDBFF => {
            *pending = Some(unit);
            None
        }
        0xDC00..=0xDFFF => {
            let high = pending.take()?;
            char::decode_utf16([high, unit]).next()?.ok()
        }
        _ => {
            *pending = None;
            char::from_u32(unit as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> FilterUi {
        let mut u = FilterUi::default();
        for c in s.chars() {
            u.edit(Edit::Char(c));
        }
        u
    }

    #[test]
    fn typing_inserts_at_the_caret_and_reports_a_change() {
        let mut u = typed("ac");
        u.edit(Edit::Left);
        assert!(u.edit(Edit::Char('b')));
        assert_eq!((u.text.as_str(), u.caret), ("abc", 2));
    }

    #[test]
    fn backspace_and_delete_remove_whole_characters() {
        let mut u = typed("a\u{e9}\u{1F600}");
        assert!(u.edit(Edit::Backspace));
        assert_eq!(u.text, "a\u{e9}");
        u.edit(Edit::Left); // before the e-acute
        assert!(u.edit(Edit::Delete));
        assert_eq!(u.text, "a");
        assert!(!u.edit(Edit::Delete));
        assert!(u.edit(Edit::Backspace));
        assert!(!u.edit(Edit::Backspace));
        assert_eq!(u.text, "");
    }

    #[test]
    fn arrows_home_and_end_move_without_changing_the_text() {
        let mut u = typed("ab");
        assert!(!u.edit(Edit::Left));
        assert_eq!(u.caret, 1);
        u.edit(Edit::Home);
        u.edit(Edit::Left);
        assert_eq!(u.caret, 0);
        u.edit(Edit::End);
        u.edit(Edit::Right);
        assert_eq!((u.caret, u.text.as_str()), (2, "ab"));
    }

    #[test]
    fn select_all_then_typing_replaces_and_arrows_collapse() {
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        assert!(u.all);
        assert!(u.edit(Edit::Char('n')));
        assert_eq!((u.text.as_str(), u.all), ("n", false));
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        assert!(u.edit(Edit::Backspace));
        assert_eq!(u.text, "");
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        u.edit(Edit::Left);
        assert_eq!((u.caret, u.text.as_str(), u.all), (0, "old", false));
        u.edit(Edit::SelectAll);
        u.edit(Edit::Right);
        assert_eq!(u.caret, 3);
    }

    #[test]
    fn paste_drops_line_breaks_and_controls() {
        let mut u = typed("ab");
        u.edit(Edit::Left);
        assert!(u.edit(Edit::Paste("x\r\ny\tz".into())));
        assert_eq!((u.text.as_str(), u.caret), ("axyzb", 4));
    }

    #[test]
    fn control_characters_are_ignored_and_keep_a_selection() {
        let mut u = typed("ab");
        u.edit(Edit::SelectAll);
        assert!(!u.edit(Edit::Char('\u{8}')));
        assert!(u.all);
        assert_eq!(u.text, "ab");
    }

    #[test]
    fn the_text_is_capped() {
        let mut u = FilterUi::default();
        assert!(u.edit(Edit::Paste("a".repeat(MAX_CHARS + 50))));
        assert_eq!(u.text.chars().count(), MAX_CHARS);
        assert!(!u.edit(Edit::Char('b')));
    }

    #[test]
    fn clear_reports_whether_there_was_text() {
        let mut u = typed("x");
        assert!(u.clear());
        assert!(!u.clear());
        assert_eq!((u.text.as_str(), u.caret), ("", 0));
    }

    #[test]
    fn utf16_units_combine_into_characters() {
        let mut p = None;
        assert_eq!(push_utf16(&mut p, 'a' as u16), Some('a'));
        let pair: Vec<u16> = "\u{1F600}".encode_utf16().collect();
        assert_eq!(push_utf16(&mut p, pair[0]), None);
        assert_eq!(push_utf16(&mut p, pair[1]), Some('\u{1F600}'));
        assert_eq!(push_utf16(&mut p, pair[1]), None); // a lone low half is dropped
        push_utf16(&mut p, pair[0]);
        assert_eq!(push_utf16(&mut p, 'b' as u16), Some('b')); // a lone high half is dropped
    }
}
```

- [ ] **Step 2: Register the module.** In `lib.rs`, add `pub mod filter_ui;` after `pub mod fmt;`.

- [ ] **Step 3: Run.** `cargo test -p plumb --lib filter_ui -j 8` → 9 pass.

- [ ] **Step 4: Commit.**

```bash
git add crates/plumb/src/filter_ui.rs crates/plumb/src/lib.rs
git commit -m "Add the filter text box reducer"
```

---

### Task 3: The strip in the layout

The strip (`FILTER_H` = 32) sits between the picker (and the stale banner, when shown) and the column headers. Left to right: the text box (with a clear button at its right end), the Regex toggle, the count text.

**Files:**
- Modify: `crates/plumb/src/layout.rs`
- Modify: `crates/plumb/src/app.rs` (test coordinates only)
- Modify: `crates/plumb/tests/render.rs` (coordinates only)

**Interfaces:**
- Produces: constants `FILTER_H`, `FILTER_REGEX_W`, `FILTER_COUNT_W`, `FILTER_CLEAR_W`; `Layout::{filter_strip, filter_box, filter_clear, filter_regex, filter_count}() -> Rect`; `Hit::{FilterBox, FilterRegex, FilterClear}`.

- [ ] **Step 1: Update the existing layout tests first** (they define the new geometry). In `layout.rs` tests:
  - `regions_tile_the_window`: `assert_eq!(l.rows_area().y, PICKER_H + FILTER_H + HEADER_H);` and `assert_eq!(l.rows_visible(), ((600.0 - 44.0 - 32.0 - 24.0 - 24.0) / 22.0) as usize);`
  - `hit_testing`: every `PICKER_H + 5.0` that targets the header becomes `PICKER_H + FILTER_H + 5.0` (two places); every `PICKER_H + HEADER_H + …` becomes `PICKER_H + FILTER_H + HEADER_H + …` (two places).
  - `a_banner_pushes_the_table_down_and_has_a_button`: replace the line `assert_eq!(l.hit(l.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Header(3));` with `assert_ne!(l.hit(l.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);` and change `b.hit(5.0, PICKER_H + BANNER_H + 5.0, 3)` to `b.hit(5.0, PICKER_H + BANNER_H + FILTER_H + 5.0, 3)`.
  - Add the new test:

```rust
    #[test]
    fn the_filter_strip_sits_above_the_header_and_its_controls_hit() {
        let l = Layout::new(800.0, 600.0);
        assert_eq!(l.filter_strip().y, PICKER_H);
        assert_eq!(l.header().y, PICKER_H + FILTER_H);
        let (b, r, c, n) = (l.filter_box(), l.filter_regex(), l.filter_clear(), l.filter_count());
        assert!(b.x + b.w <= r.x && r.x + r.w <= n.x && n.x + n.w <= 800.0, "{b:?} {r:?} {n:?}");
        assert_eq!(l.hit(b.x + b.w / 2.0, b.y + 2.0, 3), Hit::FilterBox);
        assert_eq!(l.hit(c.x + 2.0, c.y + 2.0, 3), Hit::FilterClear);
        assert_eq!(l.hit(r.x + 2.0, r.y + 2.0, 3), Hit::FilterRegex);
        assert_eq!(l.hit(n.x + 2.0, n.y + 2.0, 3), Hit::None);
        let banner = l.with_banner(true);
        assert_eq!(banner.filter_strip().y, PICKER_H + BANNER_H);
        assert_eq!(banner.header().y, PICKER_H + BANNER_H + FILTER_H);
        assert_eq!(banner.hit(banner.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);
    }
```

- [ ] **Step 2: Update the two other users of the old geometry.**
  - `app.rs` tests: add `FILTER_H` to the `use crate::layout::{…}` line; `row_y` becomes `PICKER_H + FILTER_H + HEADER_H + i as f32 * ROW_H + 1.0`; `let hy = PICKER_H + 2.0;` becomes `PICKER_H + FILTER_H + 2.0`; `a.handle(Input::Click(5.0, PICKER_H + 2.0))` becomes `PICKER_H + FILTER_H + 2.0`.
  - `tests/render.rs`: `pixel(&buf, 790, 60)` (header) becomes `pixel(&buf, 790, 88)`; the rows check `(68..570)` becomes `(100..570)`.

- [ ] **Step 3: Run to see them fail.** `cargo test -p plumb --lib layout -j 8` → compile errors (`FILTER_H`, `filter_box`, `Hit::FilterBox` missing).

- [ ] **Step 4: Implement.** In `layout.rs`:

Constants (after `RESCAN_W`):

```rust
pub const FILTER_H: f32 = 32.0;
pub const FILTER_REGEX_W: f32 = 72.0;
pub const FILTER_COUNT_W: f32 = 150.0;
pub const FILTER_CLEAR_W: f32 = 24.0;
```

`Hit` gains (before `None`):

```rust
    /// The filter text box (not its clear button).
    FilterBox,
    FilterRegex,
    FilterClear,
```

Replace `top()` and add the strip methods:

```rust
    /// Top edge of the filter strip: below the picker, and below the banner when there is one.
    fn filter_top(&self) -> f32 {
        PICKER_H + if self.banner { BANNER_H } else { 0.0 }
    }

    /// Top edge of the header: below the filter strip.
    fn top(&self) -> f32 {
        self.filter_top() + FILTER_H
    }

    pub fn filter_strip(&self) -> Rect {
        Rect { x: 0.0, y: self.filter_top(), w: self.w, h: FILTER_H }
    }

    /// Match count or status text, right-aligned in the strip.
    pub fn filter_count(&self) -> Rect {
        Rect { x: (self.w - 8.0 - FILTER_COUNT_W).max(0.0), y: self.filter_top(), w: FILTER_COUNT_W, h: FILTER_H }
    }

    pub fn filter_regex(&self) -> Rect {
        let x = (self.filter_count().x - 8.0 - FILTER_REGEX_W).max(0.0);
        Rect { x, y: self.filter_top() + 4.0, w: FILTER_REGEX_W, h: FILTER_H - 8.0 }
    }

    /// The text box, including its clear button at the right end.
    pub fn filter_box(&self) -> Rect {
        let w = (self.filter_regex().x - 8.0 - 8.0).max(60.0);
        Rect { x: 8.0, y: self.filter_top() + 4.0, w, h: FILTER_H - 8.0 }
    }

    pub fn filter_clear(&self) -> Rect {
        let b = self.filter_box();
        Rect { x: b.x + b.w - FILTER_CLEAR_W, y: b.y, w: FILTER_CLEAR_W, h: b.h }
    }
```

In `hit()`, after the Rescan check and before the header check:

```rust
        if self.filter_clear().contains(x, y) {
            return Hit::FilterClear;
        }
        if self.filter_box().contains(x, y) {
            return Hit::FilterBox;
        }
        if self.filter_regex().contains(x, y) {
            return Hit::FilterRegex;
        }
```

The clear button is tested before the box because it sits inside it.

- [ ] **Step 5: Run.** `cargo test -p plumb --lib -j 8` and `cargo test -p plumb --test render -j 8` → pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb/src/layout.rs crates/plumb/src/app.rs crates/plumb/tests/render.rs
git commit -m "Add the filter strip to the layout"
```

---

### Task 4: Drawing the strip and dimmed rows

**Files:**
- Modify: `crates/plumb/src/draw.rs`
- Modify: `crates/plumb/src/app.rs` (`with_scene` passes an empty bar for now; Task 5 fills it)
- Modify: `crates/plumb/tests/render.rs`

**Interfaces:**
- Produces: `draw::FilterBar<'a> { text, caret, regex, focus, all, error, count }` (Copy, Default); `Scene.filter: FilterBar<'a>`; `Theme.error`.

- [ ] **Step 1: Extend the render test first.** In `tests/render.rs`: import `FilterBar` (`use plumb::draw::{Chip, FilterBar, Painter, Scene, Theme, View};`); change `fn render(dark: bool)` to `fn render(dark: bool, error: bool)`, and add to the `Scene` literal:

```rust
        filter: FilterBar { text: "txt", caret: 3, regex: true, focus: true, error, count: "12 matches", ..Default::default() },
```

Update the existing call `render(dark)` to `render(dark, false)`. Add:

```rust
#[test]
fn the_filter_strip_draws_and_an_invalid_pattern_tints_the_box() {
    let to8 = |c: f32| (c * 255.0).round() as u8;
    for dark in [false, true] {
        let t = Theme::new(dark);
        let l = Layout::new(W as f32, H as f32);
        let b = l.filter_box();
        // inside the box, well right of the three typed characters and left of the clear button
        let (x, y) = ((b.x + b.w / 2.0) as u32, (b.y + b.h / 2.0) as u32);
        let ok = render(dark, false);
        let bad = render(dark, true);
        let col = |c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F| [to8(c.r), to8(c.g), to8(c.b)];
        assert_eq!(pixel(&ok, 3, y), col(t.panel), "strip (dark={dark})");
        assert_eq!(pixel(&ok, x, y), col(t.bg), "box (dark={dark})");
        assert_eq!(pixel(&bad, x, y), col(t.error), "error tint (dark={dark})");
    }
}
```

- [ ] **Step 2: Run to see it fail.** `cargo test -p plumb --test render -j 8` → compile errors (no `FilterBar`, no `Scene.filter`, no `Theme.error`).

- [ ] **Step 3: Implement in `draw.rs`.**

Imports: `use crate::layout::{Layout, Rect, FILTER_CLEAR_W, INDENT, RESCAN_W, ROW_H};` and add `DWRITE_TEXT_METRICS` to the DirectWrite import list.

`Theme` gains `pub error: D2D1_COLOR_F,` set to `rgb(0x5c2b2e)` (dark) and `rgb(0xfbd5d8)` (light).

New type and `Scene` field:

```rust
/// What the filter strip shows: the text box state plus the count text.
#[derive(Clone, Copy, Default)]
pub struct FilterBar<'a> {
    pub text: &'a str,
    /// Caret as a byte offset into `text`.
    pub caret: usize,
    pub regex: bool,
    pub focus: bool,
    /// The whole text is selected.
    pub all: bool,
    /// The pattern did not compile: the box is tinted.
    pub error: bool,
    /// "1,240 matches", "searching…", "invalid pattern", or empty.
    pub count: &'a str,
}
```

`Scene` gains `pub filter: FilterBar<'a>,` (after `banner`).

In `Pen`:

```rust
    fn frame(&self, r: Rect, c: D2D1_COLOR_F) {
        for e in [
            Rect { h: 1.0, ..r },
            Rect { y: r.y + r.h - 1.0, h: 1.0, ..r },
            Rect { w: 1.0, ..r },
            Rect { x: r.x + r.w - 1.0, w: 1.0, ..r },
        ] {
            self.fill(e, c);
        }
    }

    fn text_width(&self, s: &str) -> f32 {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.gfx.dw.CreateTextLayout(&u, &self.painter.left, 10_000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return 0.0;
            }
            m.widthIncludingTrailingWhitespace
        }
    }

    fn filter_bar(&self, s: &Scene) {
        let (t, l, f) = (&s.theme, &s.layout, &s.filter);
        self.fill(l.filter_strip(), t.panel);
        let b = l.filter_box();
        self.fill(b, if f.error { t.error } else { t.bg });
        self.frame(b, if f.focus { t.accent } else { t.select });
        let inner = Rect { x: b.x + 8.0, y: b.y, w: (b.w - 16.0 - FILTER_CLEAR_W).max(0.0), h: b.h };
        if f.text.is_empty() {
            if !f.focus {
                self.text("Filter by name  (Ctrl+F)", inner, t.dim, false);
            }
        } else {
            if f.all {
                let w = self.text_width(f.text).min(inner.w);
                self.fill(Rect { x: inner.x, y: b.y + 4.0, w, h: b.h - 8.0 }, t.select);
            }
            self.text(f.text, inner, t.fg, false);
            let c = l.filter_clear();
            self.text("\u{00D7}", Rect { w: c.w - 6.0, ..c }, t.dim, true);
        }
        if f.focus && !f.all {
            let x = inner.x + self.text_width(f.text.get(..f.caret).unwrap_or(f.text));
            if x <= inner.x + inner.w {
                self.fill(Rect { x, y: b.y + 5.0, w: 1.0, h: b.h - 10.0 }, t.fg);
            }
        }
        let r = l.filter_regex();
        self.fill(r, if f.regex { t.select } else { t.bg });
        self.frame(r, if f.regex { t.accent } else { t.select });
        self.text("Regex", Rect { x: r.x + 16.0, w: r.w - 16.0, ..r }, if f.regex { t.accent } else { t.dim }, false);
        let c = l.filter_count();
        self.text(f.count, c, if f.error { t.fg } else { t.dim }, true);
    }
```

In `draw()`, after the banner block and before the header block, add `self.filter_bar(s);`.

In `rows()`: after `let dir = …` add `let fg = if row.dim { t.dim } else { t.fg };` and replace `t.fg` by `fg` in the four `self.text(...)` calls of the row (name, size, percentage, items).

If `CreateTextLayout` or `GetMetrics` has a different signature in the pinned `windows` version, adjust to it (the compiler says); the behaviour is "width of `s` including trailing whitespace, in DIPs".

- [ ] **Step 4: `with_scene`.** In `app.rs` add `filter: crate::draw::FilterBar::default(),` to the `Scene` literal (Task 5 replaces it).

- [ ] **Step 5: Run.** `cargo test -p plumb --test render -j 8` and `cargo test -p plumb --lib -j 8` → pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb/src/draw.rs crates/plumb/src/app.rs crates/plumb/tests/render.rs
git commit -m "Draw the filter strip and dim rows under a leaf match"
```

---

### Task 5: Filter state in the app

This is the core of plan B: everything except Win32. The App gets a `Filter` (text box, generation, dirty flag, error, the run in flight, the installed result) in a child module `app/filter.rs`.

**Flow.** An edit changes the query: `bump()` (new generation, cancels the old scan through a shared atomic, forgets the run in flight). An emptied box clears the filter at once. Otherwise the app marks the results dirty and returns `Effect::Filter`; the window starts a 100 ms timer. When it fires the window calls `start_filter()`, which compiles the query (an invalid one sets the error and keeps the previous result) and returns a `FilterJob` (a name snapshot, the matcher, the generation, the cancel atomic) for a worker thread. The worker hands back `Hits`; `filter_ready` drops stale generations, re-tests the nodes changed since the snapshot, builds the keep set and installs it. `refresh_rows` (live updates) re-tests the changed nodes and rebuilds the keep set when one of them was or became part of it. A new or swapped tree resets the results and leaves `dirty` set so the window starts a run on it.

**Files:**
- Create: `crates/plumb/src/app/filter.rs`
- Modify: `crates/plumb/src/app.rs`

**Interfaces:**
- Consumes: `FilterUi`, `Edit` (Task 2); `Layout::filter_*`, `Hit::Filter*` (Task 3); `FilterBar` (Task 4); `RowModel::update_keep` (Task 1); `plumb_core::filter::*`.
- Produces: `app::filter::FilterJob { names, matcher, generation, cancel }`; `App::{start_filter() -> Option<FilterJob>, filter_ready(u64, Hits), filter_dirty() -> bool, filter_installed() -> bool, filter_ui() -> &FilterUi}`; `Input::{Edit(Edit), Find, Escape, Enter, Tab}`; `Effect::Filter`.

- [ ] **Step 1: App wiring in `app.rs`.**
  - Top: `mod filter;` and `pub use filter::FilterJob;` and `use crate::filter_ui::Edit;`.
  - `Input` gains:

```rust
    /// A text edit for the filter box (ignored unless it has focus).
    Edit(Edit),
    /// Ctrl+F: focus the filter box with its text selected.
    Find,
    Escape,
    Enter,
    Tab,
```

  - `Effect` gains `/// Start the debounce timer for a filter run.` `Filter,`.
  - `App` gains the field `filter: filter::Filter,` (initialised `filter: Default::default()` in `App::new`).
  - `begin_scan` and `finish_scan`: call `self.filter_reset();` as their first statement. `swap_tree`: first statement `if self.filter.installed.is_some() { self.unfilter_rows(); }`, and call `self.filter_reset();` just before the final `self.set_live(LiveStatus::Off);` of the success path (the early `finish_scan` return already resets).
  - `refresh_rows`: first statement `self.filter_live();`.
  - `handle`: replace the `Input::Key(k)` arm and add the new arms:

```rust
            Input::Key(k) => {
                if self.filter.ui.focus
                    && let Some(e) = filter::edit_for_key(k)
                {
                    return self.filter_edit(e);
                }
                if let (Some(t), Some(m)) = (&self.tree, &mut self.model) {
                    m.handle_key(t, k, self.layout.rows_visible().max(1));
                    let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
                    self.set_scroll(s);
                }
            }
            Input::Edit(e) => return self.filter_edit(e),
            Input::Find => {
                self.filter.ui.focus = true;
                self.filter.ui.edit(Edit::SelectAll);
            }
            Input::Escape => return self.filter_escape(),
            Input::Enter => self.filter_enter(),
            Input::Tab => self.filter.ui.focus = !self.filter.ui.focus,
```

  - `click`: replace the body after the admin-button check so the hit is computed once and the filter controls run first:

```rust
        let hit = self.layout.hit(x, y, self.drives.len());
        match hit {
            Hit::FilterBox => {
                self.filter.ui.focus = true;
                self.filter.ui.all = false;
                self.filter.ui.caret = self.filter.ui.text.len();
                return Effect::None;
            }
            Hit::FilterRegex => {
                self.filter.ui.focus = true;
                self.filter.ui.regex = !self.filter.ui.regex;
                return self.filter_changed();
            }
            Hit::FilterClear => {
                self.filter.ui.focus = true;
                return if self.filter.ui.clear() { self.filter_changed() } else { Effect::None };
            }
            _ => self.filter.ui.focus = false,
        }
        match hit {
            /* the existing arms, unchanged */
        }
```

  - `with_scene`: `let count = self.filter_count();` next to the other locals, and `filter: self.filter_bar(&count),` in the `Scene` literal (replacing Task 4's placeholder).

- [ ] **Step 2: Write `app/filter.rs`** with the code below and its tests.

```rust
//! The name filter as application state: the text box, the debounced worker run, installing its
//! result, and keeping it current while the tree changes. No Win32 types.
use super::{App, Effect};
use crate::draw::FilterBar;
use crate::filter_ui::{Edit, FilterUi};
use crate::fmt;
use plumb_core::NodeId;
use plumb_core::filter::{FilterError, Hits, Keep, Matcher, Names, Query};
use plumb_core::rows::Key;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Everything a worker thread needs to scan one snapshot of the names.
pub struct FilterJob {
    pub names: Names,
    pub matcher: Matcher,
    pub generation: u64,
    /// Holds the newest generation; the scan gives up once it differs from `generation`.
    pub cancel: Arc<AtomicU64>,
}

/// The pattern, hits and keep set of the last result installed.
pub(super) struct Installed {
    matcher: Matcher,
    hits: Hits,
    keep: Rc<Keep>,
}

#[derive(Default)]
pub(super) struct Filter {
    pub ui: FilterUi,
    /// Bumped by every change to the query or the tree; results of older runs are dropped.
    generation: u64,
    cancel: Arc<AtomicU64>,
    /// The installed result does not match the text or the tree, so a run is wanted.
    pub dirty: bool,
    error: Option<FilterError>,
    /// The generation and pattern of the run in flight.
    running: Option<(u64, Matcher)>,
    /// Nodes changed since the running scan took its snapshot.
    touched: Vec<NodeId>,
    pub installed: Option<Installed>,
    /// Scroll position from before the filter was first installed.
    saved_scroll: usize,
}

impl Filter {
    fn query(&self) -> Query {
        Query { text: self.ui.text.clone(), regex: self.ui.regex }
    }

    /// The query or the tree changed: cancel the scan in flight and forget it.
    fn bump(&mut self) {
        self.generation += 1;
        self.cancel.store(self.generation, Ordering::Relaxed);
        self.running = None;
        self.touched.clear();
        self.error = None;
    }
}

/// Left/Right/Home/End edit the box while it has focus; the other keys stay with the tree.
pub(super) fn edit_for_key(k: Key) -> Option<Edit> {
    match k {
        Key::Left => Some(Edit::Left),
        Key::Right => Some(Edit::Right),
        Key::Home => Some(Edit::Home),
        Key::End => Some(Edit::End),
        _ => None,
    }
}

impl App {
    pub fn filter_ui(&self) -> &FilterUi {
        &self.filter.ui
    }

    /// True when the installed result is out of date, so `start_filter` has work to do.
    pub fn filter_dirty(&self) -> bool {
        self.filter.dirty
    }

    pub fn filter_installed(&self) -> bool {
        self.filter.installed.is_some()
    }

    /// "1,240 matches", "searching…", "invalid pattern", or empty.
    pub(super) fn filter_count(&self) -> String {
        let f = &self.filter;
        if let Some(e) = &f.error {
            return match e {
                FilterError::Invalid(_) => "invalid pattern",
                FilterError::TooComplex => "pattern too complex",
            }
            .into();
        }
        if f.ui.text.is_empty() {
            return String::new();
        }
        if f.dirty {
            return "searching\u{2026}".into();
        }
        match &f.installed {
            Some(i) => {
                let n = i.keep.matches();
                format!("{} {}", fmt::count(n as u64), if n == 1 { "match" } else { "matches" })
            }
            None => String::new(),
        }
    }

    pub(super) fn filter_bar<'a>(&'a self, count: &'a str) -> FilterBar<'a> {
        let u = &self.filter.ui;
        FilterBar {
            text: &u.text,
            caret: u.caret,
            regex: u.regex,
            focus: u.focus,
            all: u.all,
            error: self.filter.error.is_some(),
            count,
        }
    }

    /// The text or the Regex toggle changed. An emptied box clears the filter at once; anything
    /// else waits for the debounce timer.
    pub(super) fn filter_changed(&mut self) -> Effect {
        self.filter.bump();
        if self.filter.ui.text.is_empty() {
            self.clear_filter_results();
            return Effect::None;
        }
        self.filter.dirty = true;
        Effect::Filter
    }

    pub(super) fn filter_edit(&mut self, e: Edit) -> Effect {
        if !self.filter.ui.focus {
            return Effect::None;
        }
        if self.filter.ui.edit(e) { self.filter_changed() } else { Effect::None }
    }

    /// Esc: clears the text if there is any, and takes the focus off the box.
    pub(super) fn filter_escape(&mut self) -> Effect {
        self.filter.ui.focus = false;
        if self.filter.ui.clear() { self.filter_changed() } else { Effect::None }
    }

    /// Enter in the box: focus moves to the tree, on its first row.
    pub(super) fn filter_enter(&mut self) {
        if !self.filter.ui.focus {
            return;
        }
        self.filter.ui.focus = false;
        if let Some(m) = &mut self.model
            && let Some(id) = m.rows().first().map(|r| r.id)
        {
            m.select(id);
        }
        self.set_scroll(0);
    }

    /// Removes the filter from the rows and forgets the installed result.
    fn clear_filter_results(&mut self) {
        self.filter.dirty = false;
        if self.filter.installed.take().is_some() {
            self.unfilter_rows();
        }
    }

    /// Takes the filter off the rows and returns to the scroll position from before it.
    pub(super) fn unfilter_rows(&mut self) {
        let (vis, saved) = (self.layout.rows_visible(), self.filter.saved_scroll);
        let s = match (&self.tree, &mut self.model) {
            (Some(t), Some(m)) => {
                m.set_filter(t, None);
                Some(m.scroll_to_selected(saved, vis))
            }
            _ => None,
        };
        if let Some(s) = s {
            self.set_scroll(s);
        }
    }

    /// The tree is being replaced: drop everything derived from the old one. With text in the box
    /// `dirty` stays set, so the window starts a run on the new tree.
    pub(super) fn filter_reset(&mut self) {
        self.filter.bump();
        self.filter.installed = None;
        self.filter.dirty = !self.filter.ui.text.is_empty();
    }

    /// Compiles the query and snapshots the names for a worker, if a run is wanted. An invalid
    /// pattern sets the error and keeps the previous result on screen.
    pub fn start_filter(&mut self) -> Option<FilterJob> {
        let tree = self.tree.as_ref()?;
        // Nothing to do when the result is current, or when a scan for this very query is running.
        if !self.filter.dirty || self.filter.running.is_some() {
            return None;
        }
        match self.filter.query().compile() {
            Err(e) => {
                self.filter.error = Some(e);
                self.filter.dirty = false;
                None
            }
            Ok(None) => {
                self.clear_filter_results();
                None
            }
            Ok(Some(matcher)) => {
                let f = &mut self.filter;
                f.error = None;
                f.touched.clear();
                f.running = Some((f.generation, matcher.clone()));
                Some(FilterJob { names: Names::of(tree), matcher, generation: f.generation, cancel: f.cancel.clone() })
            }
        }
    }

    /// Installs a finished scan unless a newer query or tree has replaced it.
    pub fn filter_ready(&mut self, generation: u64, mut hits: Hits) {
        let f = &mut self.filter;
        let Some((_, matcher)) = f.running.take_if(|(g, _)| *g == generation) else { return };
        let (Some(tree), Some(model)) = (&self.tree, &mut self.model) else { return };
        // The tree kept changing while the worker scanned: re-test what changed since the snapshot
        // (nodes added since are covered by `update`, which also tests everything past the end).
        hits.update(tree, &matcher, &f.touched);
        f.touched.clear();
        let keep = Rc::new(Keep::build(tree, &hits));
        if f.installed.is_none() {
            f.saved_scroll = self.scroll;
        }
        model.set_filter(tree, Some(keep.clone()));
        f.installed = Some(Installed { matcher, hits, keep });
        f.dirty = false;
        self.set_scroll(0);
    }

    /// Before the rows refresh: re-tests the changed nodes against the installed pattern and
    /// rebuilds the keep set when one of them was, or became, part of it. Also logs the changes
    /// for a run in flight.
    pub(super) fn filter_live(&mut self) {
        let (Some(tree), Some(model)) = (&self.tree, &mut self.model) else { return };
        let f = &mut self.filter;
        let touched = tree.touched();
        if touched.is_empty() {
            return;
        }
        if f.running.is_some() {
            f.touched.extend_from_slice(touched);
        }
        let Some(i) = &mut f.installed else { return };
        i.hits.update(tree, &i.matcher, touched);
        if touched.iter().any(|&id| i.keep.is_kept(id) || i.hits.get(id)) {
            i.keep = Rc::new(Keep::build(tree, &i.hits));
            model.update_keep(i.keep.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DriveInfo, Input};
    use crate::layout::{Layout, FILTER_H, HEADER_H, PICKER_H, ROW_H};
    use crate::restore::resolve;
    use plumb_core::{NodeFlags, Tree, TreeBuilder};

    /// C:\{docs\{notes.txt, todo.md}, src\{main.rs, deep\more.txt}, readme.md}
    fn tree() -> Tree {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
        b.push_str(docs, "notes.txt", 30, NodeFlags::empty());
        b.push_str(docs, "todo.md", 20, NodeFlags::empty());
        let src = b.push_str(root, "src", 0, NodeFlags::DIR);
        b.push_str(src, "main.rs", 50, NodeFlags::empty());
        let deep = b.push_str(src, "deep", 0, NodeFlags::DIR);
        b.push_str(deep, "more.txt", 10, NodeFlags::empty());
        b.push_str(root, "readme.md", 5, NodeFlags::empty());
        b.finish().unwrap()
    }

    fn app() -> App {
        let drives = vec![DriveInfo { letter: 'C', total: 1000, free: 500 }];
        let mut a = App::new(Layout::new(800.0, 600.0), drives, "test".into());
        a.begin_scan(0);
        a.finish_scan(tree(), "done".into());
        a
    }

    fn names(a: &App) -> Vec<String> {
        let t = a.tree.as_ref().unwrap();
        a.model.as_ref().unwrap().rows().iter().map(|r| t.name_string(r.id)).collect()
    }

    fn row_y(i: usize) -> f32 {
        PICKER_H + FILTER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Focuses the box (as Ctrl+F does) and types `s`; returns the last effect.
    fn type_text(a: &mut App, s: &str) -> Effect {
        a.handle(Input::Find);
        let mut e = Effect::None;
        for c in s.chars() {
            e = a.handle(Input::Edit(Edit::Char(c)));
        }
        e
    }

    /// Runs the wanted scan on this thread, the way the worker would.
    fn run(a: &mut App) {
        let j = a.start_filter().expect("a run was wanted");
        let h = j.names.scan(&j.matcher, &j.cancel, j.generation).expect("not cancelled");
        a.filter_ready(j.generation, h);
    }

    #[test]
    fn typing_then_running_shows_matches_and_their_folders() {
        let mut a = app();
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
        assert_eq!(type_text(&mut a, "txt"), Effect::Filter);
        assert!(a.filter_dirty());
        assert_eq!(a.filter_count(), "searching\u{2026}");
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"], "the old rows stay until the result lands");
        run(&mut a);
        assert_eq!(names(&a), ["C:", "src", "deep", "more.txt", "docs", "notes.txt"]);
        assert_eq!(a.filter_count(), "2 matches");
        assert!(!a.filter_dirty() && a.filter_installed());
    }

    #[test]
    fn a_result_from_an_older_query_is_dropped() {
        let mut a = app();
        type_text(&mut a, "md");
        let j = a.start_filter().unwrap();
        a.handle(Input::Edit(Edit::Char('x'))); // a newer query cancels the scan
        assert!(j.names.scan(&j.matcher, &j.cancel, j.generation).is_none());
        let h = j.names.scan(&j.matcher, &AtomicU64::new(j.generation), j.generation).unwrap();
        a.filter_ready(j.generation, h);
        assert!(!a.filter_installed() && a.filter_dirty());
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
    }

    #[test]
    fn escape_clears_the_filter_and_restores_the_view() {
        let mut a = app();
        let before = names(&a);
        let root = a.model.as_ref().unwrap().selected();
        type_text(&mut a, "txt");
        run(&mut a);
        assert_ne!(names(&a), before);
        assert_eq!(a.handle(Input::Escape), Effect::None);
        assert_eq!(names(&a), before);
        assert!(!a.filter_installed() && !a.filter_dirty());
        assert_eq!((a.filter_ui().text.as_str(), a.filter_ui().focus), ("", false));
        assert_eq!(a.model.as_ref().unwrap().selected(), root);
    }

    #[test]
    fn escape_in_the_tree_clears_a_filter_too() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.handle(Input::Tab); // focus to the tree
        assert!(!a.filter_ui().focus);
        a.handle(Input::Escape);
        assert_eq!(names(&a).len(), 4);
        assert_eq!(a.filter_ui().text, "");
    }

    #[test]
    fn an_invalid_pattern_keeps_the_previous_result_and_says_so() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        let shown = names(&a);
        let r = a.layout.filter_regex();
        assert_eq!(a.handle(Input::Click(r.x + 2.0, r.y + 2.0)), Effect::Filter); // regex on, "txt" still valid
        a.handle(Input::Edit(Edit::Char('('))); // "txt(" is not
        assert!(a.start_filter().is_none());
        assert_eq!(a.filter_count(), "invalid pattern");
        assert!(a.filter_bar("").error && a.filter_installed());
        assert_eq!(names(&a), shown);
        a.handle(Input::Edit(Edit::Backspace));
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches");
        assert!(!a.filter_bar("").error);
    }

    #[test]
    fn the_regex_toggle_changes_how_the_text_is_read() {
        let mut a = app();
        type_text(&mut a, "t.t");
        run(&mut a);
        assert_eq!(a.filter_count(), "0 matches");
        assert!(names(&a).is_empty(), "with no match even the root is hidden");
        assert_eq!(a.model.as_ref().unwrap().selected(), None);
        let r = a.layout.filter_regex();
        assert_eq!(a.handle(Input::Click(r.x + 2.0, r.y + 2.0)), Effect::Filter);
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches"); // t, any character, t: "txt" in both names
    }

    #[test]
    fn the_clear_button_empties_the_box() {
        let mut a = app();
        let c = a.layout.filter_clear();
        assert_eq!(a.handle(Input::Click(c.x + 2.0, c.y + 2.0)), Effect::None);
        assert!(a.filter_ui().focus);
        type_text(&mut a, "txt");
        run(&mut a);
        assert_eq!(a.handle(Input::Click(c.x + 2.0, c.y + 2.0)), Effect::None);
        assert_eq!((a.filter_ui().text.as_str(), a.filter_installed()), ("", false));
        assert_eq!(names(&a).len(), 4);
    }

    #[test]
    fn live_changes_keep_the_filtered_rows_current() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        let src = resolve(a.tree.as_ref().unwrap(), "C:\\src").unwrap();
        let t = a.tree.as_mut().unwrap();
        t.add_child(src, &u("new.txt"), 40, NodeFlags::empty());
        a.refresh_rows();
        assert!(names(&a).contains(&"new.txt".to_string()));
        assert_eq!(a.filter_count(), "3 matches");
        let more = resolve(a.tree.as_ref().unwrap(), "C:\\src\\deep\\more.txt").unwrap();
        a.tree.as_mut().unwrap().rename(more, &u("more.rs"));
        a.refresh_rows();
        let n = names(&a);
        assert!(!n.contains(&"more.rs".to_string()) && !n.contains(&"deep".to_string()), "{n:?}");
        assert_eq!(a.filter_count(), "2 matches");
        let notes = resolve(a.tree.as_ref().unwrap(), "C:\\docs\\notes.txt").unwrap();
        a.tree.as_mut().unwrap().remove(notes);
        a.refresh_rows();
        assert!(!names(&a).contains(&"docs".to_string()));
        assert_eq!(a.filter_count(), "1 match");
    }

    #[test]
    fn changes_made_while_the_worker_scans_are_not_lost() {
        let mut a = app();
        type_text(&mut a, "txt");
        let j = a.start_filter().unwrap(); // the snapshot is taken here
        let src = resolve(a.tree.as_ref().unwrap(), "C:\\src").unwrap();
        let main = resolve(a.tree.as_ref().unwrap(), "C:\\src\\main.rs").unwrap();
        let t = a.tree.as_mut().unwrap();
        t.add_child(src, &u("late.txt"), 7, NodeFlags::empty()); // past the snapshot's end
        t.rename(main, &u("main.txt")); // inside it
        a.refresh_rows();
        let h = j.names.scan(&j.matcher, &j.cancel, j.generation).unwrap();
        a.filter_ready(j.generation, h);
        let n = names(&a);
        assert!(n.contains(&"late.txt".to_string()) && n.contains(&"main.txt".to_string()), "{n:?}");
        assert_eq!(a.filter_count(), "4 matches");
    }

    #[test]
    fn a_swapped_tree_drops_the_filter_and_asks_for_a_new_run() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.swap_tree(tree(), "again".into());
        assert!(!a.filter_installed() && a.filter_dirty());
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
        run(&mut a);
        assert_eq!(names(&a), ["C:", "src", "deep", "more.txt", "docs", "notes.txt"]);
    }

    #[test]
    fn a_new_scan_keeps_the_text_and_wants_a_run_when_it_finishes() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.begin_scan(0);
        assert!(a.start_filter().is_none(), "no tree yet");
        a.finish_scan(tree(), "done".into());
        assert!(a.filter_dirty());
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches");
    }

    #[test]
    fn keys_go_to_the_box_only_while_it_has_focus() {
        let mut a = app();
        let root = a.model.as_ref().unwrap().selected();
        a.handle(Input::Edit(Edit::Char('z'))); // not focused: ignored
        assert_eq!(a.filter_ui().text, "");
        let b = a.layout.filter_box();
        a.handle(Input::Click(b.x + 10.0, b.y + 5.0));
        assert!(a.filter_ui().focus);
        a.handle(Input::Edit(Edit::Char('a')));
        a.handle(Input::Edit(Edit::Char('b')));
        a.handle(Input::Key(Key::Left));
        assert_eq!(a.filter_ui().caret, 1);
        assert_eq!(a.model.as_ref().unwrap().selected(), root, "Left edited the box, not the tree");
        a.handle(Input::Key(Key::Down));
        assert_ne!(a.model.as_ref().unwrap().selected(), root, "Down still moves the tree selection");
        a.handle(Input::Click(20.0, row_y(1)));
        assert!(!a.filter_ui().focus);
        a.handle(Input::Edit(Edit::Char('z')));
        assert_eq!(a.filter_ui().text, "ab");
    }

    #[test]
    fn find_selects_the_text_enter_and_tab_move_focus() {
        let mut a = app();
        type_text(&mut a, "ab");
        a.handle(Input::Tab);
        assert!(!a.filter_ui().focus);
        a.handle(Input::Find);
        assert!(a.filter_ui().focus && a.filter_ui().all);
        a.handle(Input::Edit(Edit::Char('q')));
        assert_eq!(a.filter_ui().text, "q");
        a.handle(Input::Key(Key::Down)); // move the selection off the first row
        a.handle(Input::Enter);
        assert!(!a.filter_ui().focus);
        let m = a.model.as_ref().unwrap();
        assert_eq!(m.selected(), Some(m.rows()[0].id));
        a.handle(Input::Enter); // not focused: nothing happens
        a.handle(Input::Tab);
        assert!(a.filter_ui().focus);
    }

    #[test]
    fn sorting_under_a_filter_keeps_the_filter() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.handle(Input::Click(5.0, PICKER_H + FILTER_H + 2.0)); // sort by Name ascending
        assert_eq!(names(&a), ["C:", "docs", "notes.txt", "src", "deep", "more.txt"]);
    }
}
```

- [ ] **Step 3: Run.** `cargo test -p plumb --lib -j 8` → all pass. If a name-order assertion differs from the real sort order, trace it by hand against the tree (sizes: docs 50, src 60, readme.md 5) before changing the expectation.

- [ ] **Step 4: Commit.**

```bash
git add crates/plumb/src/app.rs crates/plumb/src/app/filter.rs
git commit -m "Add the filter state: debounced run, install, live re-test"
```

---

### Task 6: Window wiring

**Files:**
- Modify: `crates/plumb/src/window.rs`

**Interfaces:**
- Consumes: `App::{start_filter, filter_ready, filter_dirty, filter_installed}`, `FilterJob`, `Input::{Edit, Find, Escape, Enter, Tab}`, `Effect::Filter`, `push_utf16`.

- [ ] **Step 1: Imports and constants.**
  - `use crate::filter_ui::{push_utf16, Edit};`, `use plumb_core::filter::Hits;`, `use std::sync::mpsc::{channel, Receiver, Sender};` (merge with the existing `channel, Receiver` import).
  - `KeyboardAndMouse` import list gains `VK_BACK, VK_DELETE, VK_ESCAPE, VK_RETURN, VK_TAB`; `Foundation` gains `HGLOBAL`; `DataExchange` gains `GetClipboardData`.
  - Constants: `const WM_FILTER: u32 = WM_APP + 3;`, `const TIMER_FILTER: usize = 3;`, `const FILTER_DEBOUNCE_MS: u32 = 100;`.

- [ ] **Step 2: `Win` fields and methods.** Fields:

```rust
    /// Finished filter scans; the worker posts `WM_FILTER` after each send.
    filter_tx: Sender<(u64, Hits)>,
    filter_rx: Receiver<(u64, Hits)>,
    /// First half of a surrogate pair seen by `WM_CHAR`.
    high_surrogate: Option<u16>,
```

In `Win::new`: `let (filter_tx, filter_rx) = channel();` and add the three fields (`high_surrogate: None`) to the struct literal. Methods in `impl Win`:

```rust
    /// (Re)starts the debounce: the scan begins 100 ms after the last keystroke.
    fn arm_filter(&self) {
        unsafe { SetTimer(Some(self.hwnd), TIMER_FILTER, FILTER_DEBOUNCE_MS, None) };
    }

    /// Scans the names for the current query on a worker thread, if the app wants a run.
    fn run_filter(&mut self) {
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_FILTER) };
        if let Some(job) = self.app.start_filter() {
            let (tx, hwnd) = (self.filter_tx.clone(), self.hwnd.0 as isize);
            std::thread::spawn(move || {
                if let Some(hits) = job.names.scan(&job.matcher, &job.cancel, job.generation) {
                    let _ = tx.send((job.generation, hits));
                    let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_FILTER, WPARAM(0), LPARAM(0)) };
                }
            });
        }
        self.invalidate();
    }

    /// `WM_FILTER`: installs the finished scans (stale ones are dropped by the app).
    fn filter_done(&mut self) {
        while let Ok((generation, hits)) = self.filter_rx.try_recv() {
            self.app.filter_ready(generation, hits);
        }
        self.invalidate();
    }
```

The worker thread runs at normal priority on purpose: a scan is 30 to 200 ms even at 5M nodes (measured in plan A) and the user is waiting for it, so `LONG_RUNS.md`'s below-normal rule for long jobs does not apply. It uses one core and no GPU.

In `scan_done`, immediately before its final `self.invalidate();` add `if self.app.filter_dirty() { self.run_filter(); }`.

- [ ] **Step 3: Input.**

```rust
/// Sends `input` to the app, then runs any effect.
fn send(hwnd: HWND, input: Input) {
    let Some(e) = with(|w| {
        w.last_input = Instant::now();
        let e = w.app.handle(input);
        w.invalidate();
        e
    }) else {
        return;
    };
    effect(hwnd, e, (0, 0));
}

/// The clipboard's text, if it has any.
fn paste_text(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while n < 65_536 && *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// The app input a key press stands for. Ctrl+C is handled by the caller.
fn key_input(vk: u16, ctrl: bool, hwnd: HWND) -> Option<Input> {
    if ctrl {
        match vk {
            v if v == b'F' as u16 => return Some(Input::Find),
            v if v == b'A' as u16 => return Some(Input::Edit(Edit::SelectAll)),
            v if v == b'V' as u16 => return paste_text(hwnd).map(|s| Input::Edit(Edit::Paste(s))),
            _ => {}
        }
    }
    Some(match vk {
        x if x == VK_BACK.0 => Input::Edit(Edit::Backspace),
        x if x == VK_DELETE.0 => Input::Edit(Edit::Delete),
        x if x == VK_RETURN.0 => Input::Enter,
        x if x == VK_ESCAPE.0 => Input::Escape,
        x if x == VK_TAB.0 => Input::Tab,
        x => Input::Key(key_of(x)?),
    })
}
```

`effect()` gains `Effect::Filter => { with(|w| w.arm_filter()); }`.

In `wndproc`, replace the `WM_KEYDOWN` arm and add `WM_CHAR`, the timer and `WM_FILTER`:

```rust
        WM_CHAR => {
            if let Some(Some(c)) = with(|w| push_utf16(&mut w.high_surrogate, wp.0 as u16)) {
                send(hwnd, Input::Edit(Edit::Char(c)));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            if ctrl && wp.0 == 'C' as usize {
                if let Some(Some(p)) = with(|w| w.selected_path()) {
                    copy_to_clipboard(hwnd, &p);
                }
            } else if let Some(input) = key_input(wp.0 as u16, ctrl, hwnd) {
                send(hwnd, input);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FILTER => {
            with(|w| w.run_filter());
            LRESULT(0)
        }
        WM_FILTER => {
            with(|w| w.filter_done());
            LRESULT(0)
        }
```

- [ ] **Step 4: Extend `selftest` to cover the filter end to end** (real message loop, real worker thread, real swapchain). First factor the pump loop: add

```rust
/// Pumps window messages until `cond` holds; errors after `secs`.
fn pump_until(what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut msg = MSG::default();
    while with(|w| !cond(w)) == Some(true) {
        if Instant::now() > deadline {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                format!("timed out waiting for: {what}"),
            ));
        }
        unsafe {
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                std::thread::yield_now();
            }
        }
    }
    Ok(())
}
```

Replace the hand-written loop in `selftest` (from `let deadline = …` to the closing brace of the `while`) with `pump_until("the scan", 30, &|w| w.app.tree.is_some() || matches!(w.app.scan, crate::app::Scan::Failed(_)))?;`. Then, before `let out = with(|w| {`, add:

```rust
    // The filter, end to end: type into the box, let the worker scan, check the rows.
    with(|w| {
        w.app.handle(Input::Find);
        for c in "bin".chars() {
            w.app.handle(Input::Edit(Edit::Char(c)));
        }
        w.run_filter();
    });
    pump_until("the filter result", 10, &|w| w.app.filter_installed())?;
    let filtered = with(|w| w.app.model.as_ref().map(|m| m.rows().len())).flatten();
    if filtered != Some(3) {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("filter \"bin\" should show 3 rows (root, sub, f.bin), got {filtered:?}"),
        ));
    }
```

and extend the `format!` in `out` to end with `; filter ok: {} rows` using `filtered.unwrap_or(0)`. (`tests/selftest.rs` only checks `"selftest ok"`, `"1 files"` and `"102400"`, which stay.)

- [ ] **Step 5: Build and run the hidden selftest.** `cargo build -p plumb -j 8`, then `cargo test -p plumb -j 8 -- --include-ignored` (runs the render test, the lib tests and the hidden-window selftest; it opens no visible window). Expected: all pass, and the selftest output contains `filter ok: 3 rows`.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb/src/window.rs
git commit -m "Wire the filter into the window: keys, paste, debounce, worker, WM_FILTER"
```

---

### Task 7: Docs, final checks, hand-off

**Files:**
- Modify: `CHANGELOG.md`, `docs/superpowers/specs/2026-10-12-plumb-filter-design.md`

- [ ] **Step 1: Changelog.** In `[Unreleased]`, replace the existing `plumb-core` filter bullet's trailing "Not yet wired into the window." and add:

```markdown
- `plumb`: name filter. Ctrl+F (or a click) focuses a strip above the column headers; typing shows only the files and folders whose name contains the text (case-insensitive), with the folders on the way to them open. The Regex toggle reads the text as a regular expression; an invalid pattern keeps the previous result and tints the box. A matching folder with no matching content is shown collapsed and opens to dimmed rows. Esc clears the filter and restores the previous expansion and scroll. The scan runs on a worker thread 100 ms after the last keystroke, and the filter follows live updates and rescans.

### Known limitations
- The filter box takes direct keyboard input only: no IME composition, no mouse text selection or undo, and a click puts the caret at the end of the text. Text wider than the box is clipped.
- A query matching most of a very large tree opens most folders; the first install on a 2M-node tree takes about 0.3 s on the UI thread.
```

(If `[Unreleased]` already has a "Known limitations" heading, merge into it.)

- [ ] **Step 2: Spec.** Change `Status: draft for review, 2026-10-12.` to `Status: implemented (plans A and B), 2026-10-12.`; add a section `## Amendments made while implementing` listing the seven items from "Spec amendments" at the top of this plan, worded as decisions; in the Row model section replace the sentence about recording a `restore::Snapshot` with the `set_filter(None)` plus saved-scroll behaviour; in the Worker section replace "a `Box<(generation, Hits)>`" with the channel.

- [ ] **Step 3: Full verification.**
  - `cargo test --workspace -j 8` (note which tests are `#[ignore]`d and run them with `-- --include-ignored` for the `plumb` crate).
  - `cargo clippy --workspace --all-targets -j 8` and report any new warnings in the files this plan touched.
  - `git diff --stat 3dc4f35` sanity: only the files listed above changed.
  - Grep for leftovers: `grep -n "FilterBar::default()" crates/plumb/src` (must be none in `app.rs`), `grep -rn "TODO\|todo!" crates/plumb/src/app crates/plumb/src/filter_ui.rs`.

- [ ] **Step 4: Commit.**

```bash
git add CHANGELOG.md docs
git commit -m "Document the window filter and the spec amendments"
```

- [ ] **Step 5: Manual check for the user** (a visual pass the tests cannot do): run `plumb`, scan a drive, press Ctrl+F, type `txt`, watch "searching…" then "N matches"; toggle Regex and type `(` to see the red tint and "invalid pattern"; press Down to move through results; Esc to clear and see the earlier expansion return; resize narrow to see the strip degrade.

## Self-review

- **Spec coverage.** Strip with box, Regex toggle, count and clear (Tasks 3, 4); focus/keys/paste/Esc/Enter/Tab (Tasks 5, 6); debounce, generation counter, worker, stale drop (Tasks 5, 6); invalid pattern keeps previous result and tints (Tasks 4, 5); touched ids re-tested on install and live (Task 5); tree swap and new scan restart the run (Task 5); dim rows (Task 4); restore on clear (Task 5); render test with strip and tint (Task 4); IME gap recorded (Task 7). Amendments 1-7 account for every place this plan departs from the spec.
- **Placeholders.** None: every step carries its code or exact edit.
- **Types.** `Edit`, `FilterUi` (Task 2) are used unchanged in Tasks 5 and 6; `FilterBar` fields (Task 4) match `filter_bar()` (Task 5); `Layout::filter_*` and `Hit::Filter*` (Task 3) match their uses in `click` (Task 5) and the draw code (Task 4); `FilterJob` fields match `run_filter` (Task 6); `update_keep` (Task 1) is called in `filter_live` (Task 5).
