---
status: draft
---

# Multi-select and keyboard parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Shift/Ctrl-click, Shift+arrows, Ctrl+A and drag selection of rows, with Del, Ctrl+C and the context menu acting on the whole set, plus F5, Enter, Backspace and Shift+F10 / Apps key.

**Architecture:** `RowModel.selected` stays the focus row; a picked-row bitset and an anchor join it in a new `rows/pick.rs`. Modifier keys travel in `Input::Click` and `Input::Key` as a `Mods` value. All input-to-selection mapping, the set-wide mark and copy live in a new `app/select.rs`; `window.rs` only reads modifiers, maps keys and runs the drag timer.

**Tech Stack:** Rust 2024, `windows` crate (Win32), cargo workspace `plumb` / `plumb-core` / `plumb-scan`.

**Spec:** `docs/superpowers/specs/2026-11-08-multi-select-design.md` (approved 2026-11-08). The plan argues from it; where this plan differs it says so in a `Ruling`.

## Global Constraints

- kind: app. Do not bump any version, edit `CHANGELOG.md`, add a `ROADMAP.md` v0.12 line, tag, push or publish. The user says when the v0.12 feature set is done.
- Commits are small and topical; end each message with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` (second `-m`). Check `git diff --stat` before each commit.
- A selection never deletes anything; Del still only marks. The mark/delete flow is otherwise unchanged. Single-row Del behaves exactly as before (same status-line text `cannot mark: <reason>`).
- Filter-box shortcuts keep priority: with the box focused, Ctrl+A, Backspace, Enter, Shift+Left/Right/Home/End act on its text.
- Fast loop per edit: `cargo test -p plumb-core rows` and `cargo test -p plumb select` (seconds). Full suite, selftests and the final review run once, in the last task.
- Long runs (the full suite) go through `~/.claude/runs/run.ps1`. Never `python` on stdin. Leave no background processes.
- Files stay near today's sizes: `app.rs` gains only the `Input`/`Mods` types, field `row_drag`, and one-line delegations (it is 3076 lines already). Behaviour goes in `app/select.rs`.

## Review Focus

Failure modes the spec implies that no single task's happy-path tests exercise; each has a test in the named task.

1. Filter box focused: Ctrl+A, Backspace, Enter and Left/Right/Home/End (with or without Shift) edit its text and never change the row selection; Up/Down keep moving rows as today (T4, T6).
2. Ctrl+A then Del at a drive root: the root is refused with a status message, nothing is marked; Del twice toggles, never half-marks (T5).
3. A refresh that removes rows under a picked set (live update, rescan): the set shrinks to rows that exist and follows the focus to its ancestor; no pick without a row (T2).
4. Pointer capture lost mid-drag (Alt-Tab, a popup): the drag ends by itself and stops scrolling (T9, by reading the timer arm; no automated test).
5. Processes view: a set of rows with no executable path has no "Copy paths" entry, and its mark entry is a disabled "Cannot mark: it is a process" (T7).

## End-state fit

- **Adds:** `crates/plumb-core/src/rows/pick.rs` (+ `pick_tests.rs`), `crates/plumb/src/app/select.rs`, `Mods`, `Input::{Refresh, ContextMenu}`, `App::row_drag`, `App::{mark_selected, copy_text, focus_row_rect, row_dragging, fs_path}`, a drag timer, `crates/plumb-core/tests/modules_declared.rs`.
- **Replaces and deletes:** `RowModel::select` in `rows.rs` (moves to `pick.rs`); `App::toggle_mark_selected` (`marks.rs`); `Window::selected_path` (`window.rs`); `RowMenu.path` (becomes `paths` + `count`); the selected-only highlight branch in `draw.rs`; the `Input::Click(f32,f32)` / `Input::Key(Key)` shapes.
- **Fixes on the way:** `rows/refresh_tests.rs` and `rows/filter_tests.rs` (17 tests) have not been compiled since commit 7bf86da dropped their `mod` lines; T1 restores them and adds a guard so an undeclared source file fails a test.
- **Tripwire:** no branch is added to an existing dispatcher beyond one arm per new `Input` variant; selection behaviour is not spread through `app.rs`.

## File Structure

| File | Role |
|---|---|
| `crates/plumb-core/src/rows.rs` | fields `picked`, `picked_n`, `anchor`; `mod pick`; `handle_key(.., extend)`; `collapse`/`rebuild`/`set_filter`/`refresh` keep the set exact |
| `crates/plumb-core/src/rows/pick.rs` | `select`, `extend_to`, `toggle_pick`, `select_all`, `targets`, `picked_count`, `is_picked`, `select_parent`, pruning |
| `crates/plumb-core/src/rows/pick_tests.rs` | model tests |
| `crates/plumb-core/tests/modules_declared.rs` | guard: every source file is declared by its parent |
| `crates/plumb/src/app.rs` | `Mods`, `Input` changes, `row_drag` field, `RowMenu`, `row_menu`, delegations |
| `crates/plumb/src/app/select.rs` | row click, drag-select, keys, Ctrl+A, Enter, Backspace, F5, key menu, set mark, copy |
| `crates/plumb/src/app/marks.rs` | loses `toggle_mark_selected` |
| `crates/plumb/src/draw.rs` | `row_look` picks fill and bar |
| `crates/plumb/src/window.rs` | `mods_now`, key map, `WM_SYSKEYDOWN`, drag timer, `key_menu`, repeat guard |
| `crates/plumb/src/window/menu.rs` | set form of the context menu |
| `crates/plumb/src/window/selftest.rs` | Shift-extend check |
| `README.md` | one feature bullet |

---

### Task 1: Restore the orphaned row tests and guard against it

**Files:**
- Modify: `crates/plumb-core/src/rows.rs` (end of file)
- Create: `crates/plumb-core/tests/modules_declared.rs`
- Modify: `docs/superpowers/specs/2026-11-08-multi-select-design.md` (front matter `status: active`)
- Modify: `docs/superpowers/plans/2026-11-08-multi-select.md` (front matter `status: active`)

**Interfaces:**
- Consumes: nothing.
- Produces: `rows::refresh_tests` and `rows::filter_tests` compiled again (T2 appends a test to `filter_tests.rs`).

- [ ] **Step 1: Write the guard test**

Create `crates/plumb-core/tests/modules_declared.rs`:

```rust
//! Every source file is declared as a module by its parent. An undeclared file compiles to nothing and its tests stop
//! running without a sound: commit 7bf86da dropped two `mod` lines and 17 tests went dark.
use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source files (path under `crates/`, forward slashes) that are not declared by a parent on purpose. Only shrinks.
const ALLOWED: &[&str] = &[];

#[test]
fn every_source_file_is_declared_by_its_parent_module() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut undeclared = Vec::new();
    for c in fs::read_dir(crates).unwrap() {
        let src = c.unwrap().path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        for f in files {
            let stem = f.file_stem().unwrap().to_str().unwrap().to_string();
            let dir = f.parent().unwrap();
            if dir == src && matches!(stem.as_str(), "lib" | "main") || stem == "mod" || dir.ends_with("bin") {
                continue;
            }
            let parents = if dir == src { vec![src.join("lib.rs"), src.join("main.rs")] } else { vec![dir.with_extension("rs"), dir.join("mod.rs")] };
            let declared = parents.iter().filter_map(|p| fs::read_to_string(p).ok()).any(|t| t.contains(&format!("mod {stem};")) || t.contains(&format!("mod {stem} {{")));
            let rel = f.strip_prefix(crates).unwrap().to_string_lossy().replace('\\', "/");
            if !declared && !ALLOWED.contains(&rel.as_str()) {
                undeclared.push(rel);
            }
        }
    }
    assert!(undeclared.is_empty(), "source files no module declares (their tests never run): {undeclared:?}");
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p plumb-core --test modules_declared`
Expected: FAIL naming `plumb-core/src/rows/refresh_tests.rs` and `plumb-core/src/rows/filter_tests.rs`. If it names other files, read each: a file that is really dead gets declared or deleted; a file that is declared by `#[path]` goes in `ALLOWED` with a comment saying why. Report any such file in the closing report.

- [ ] **Step 3: Restore the two modules**

Append to the end of `crates/plumb-core/src/rows.rs`, after the closing `}` of `mod tests`:

```rust

#[cfg(test)]
mod refresh_tests;

#[cfg(test)]
mod filter_tests;
```

- [ ] **Step 4: Run both**

Run: `cargo test -p plumb-core`
Expected: PASS; the `rows::` list now includes `rows::refresh_tests::*` (8) and `rows::filter_tests::*` (9). (Checked on 2026-11-08: all 17 pass when restored.)

- [ ] **Step 5: Mark the spec and this plan active, commit**

Change `status: draft` to `status: active` in the front matter of the spec and of this plan.

```bash
git add crates/plumb-core docs/superpowers
git commit -m "Row model: compile refresh_tests and filter_tests again, and fail a test when a source file is left undeclared" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The picked set in the row model

**Files:**
- Create: `crates/plumb-core/src/rows/pick.rs`
- Create: `crates/plumb-core/src/rows/pick_tests.rs`
- Modify: `crates/plumb-core/src/rows.rs` (struct, `new`, delete `select`, `rebuild`, `collapse`, `set_filter`, `refresh`, `handle_key`, `mod` lines, existing test calls)
- Modify: `crates/plumb-core/src/rows/filter_tests.rs` (append one test)
- Modify: `crates/plumb/src/app.rs:1039` (one call gains `, false`)

**Interfaces:**
- Consumes: `bits::{get,set}`, `RowModel::row_index` (private, same module tree), `Tree::parent`.
- Produces (all `pub` on `RowModel`): `select(NodeId)`, `extend_to(NodeId)`, `toggle_pick(NodeId)`, `select_all()`, `targets() -> Vec<NodeId>`, `picked_count() -> usize`, `is_picked(NodeId) -> bool`, `select_parent(&Tree)`, `handle_key(&Tree, Key, usize, extend: bool)`.

**Ruling:** the spec writes `select_parent()` with no parameter; the parent comes from the tree, so it takes `&Tree` like `handle_key`.

- [ ] **Step 1: Write the failing tests**

Create `crates/plumb-core/src/rows/pick_tests.rs`:

```rust
use super::*;
use crate::tree::tests::sample;

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
// With C: open the rows are 0, 1, 4, 6 (largest first); with big open as well: 0, 1, 3, 2, 4, 6.

fn open() -> (Tree, RowModel) {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    (t, m)
}

fn n(ids: &[u32]) -> Vec<NodeId> {
    ids.iter().map(|&i| NodeId(i)).collect()
}

#[test]
fn select_picks_only_that_row_and_makes_it_the_focus() {
    let (_, mut m) = open();
    m.select(NodeId(4));
    assert_eq!((m.targets(), m.picked_count(), m.selected()), (n(&[4]), 1, Some(NodeId(4))));
    m.select(NodeId(6));
    assert_eq!(m.targets(), n(&[6]));
    assert!(!m.is_picked(NodeId(4)));
}

#[test]
fn extend_picks_the_rows_between_the_anchor_and_the_target_in_either_direction() {
    let (_, mut m) = open();
    m.select(NodeId(4));
    m.extend_to(NodeId(0));
    assert_eq!(m.targets(), n(&[0, 1, 4]));
    assert_eq!(m.selected(), Some(NodeId(0)));
    m.extend_to(NodeId(6));
    assert_eq!(m.targets(), n(&[4, 6]), "the anchor stayed on 4");
}

#[test]
fn extend_with_nothing_selected_picks_just_the_target() {
    let (_, mut m) = open();
    m.extend_to(NodeId(4));
    assert_eq!((m.targets(), m.selected()), (n(&[4]), Some(NodeId(4))));
}

#[test]
fn a_stale_anchor_is_replaced_by_the_focus_and_stays_replaced() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(2));
    m.extend_to(NodeId(6)); // 2, 4, 6 picked; focus 6; anchor 2
    m.collapse(NodeId(1)); // 3 and 2 leave the list
    assert_eq!(m.targets(), n(&[4, 6]));
    m.extend_to(NodeId(4)); // anchor 2 has no row: the focus (6) stands in
    assert_eq!((m.targets(), m.selected()), (n(&[4, 6]), Some(NodeId(4))));
    m.extend_to(NodeId(1)); // and it is the anchor now
    assert_eq!(m.targets(), n(&[1, 4, 6]));
}

#[test]
fn toggle_flips_one_row_and_the_last_unpick_leaves_an_empty_set() {
    let (_, mut m) = open();
    m.select(NodeId(1));
    m.toggle_pick(NodeId(4));
    assert_eq!((m.targets(), m.selected()), (n(&[1, 4]), Some(NodeId(4))));
    m.toggle_pick(NodeId(4));
    assert_eq!(m.targets(), n(&[1]));
    m.toggle_pick(NodeId(1));
    assert_eq!((m.targets(), m.picked_count(), m.selected()), (vec![], 0, Some(NodeId(1))));
    m.toggle_pick(NodeId(6));
    assert_eq!(m.targets(), n(&[6]), "from nothing picked");
}

#[test]
fn select_all_picks_every_row_and_keeps_the_focus() {
    let (_, mut m) = open();
    m.select_all();
    assert_eq!((m.targets(), m.selected()), (n(&[0, 1, 4, 6]), Some(NodeId(0))), "no focus: the first row");
    m.select(NodeId(4));
    m.select_all();
    assert_eq!((m.picked_count(), m.selected()), (4, Some(NodeId(4))));
}

#[test]
fn collapsing_drops_the_picks_it_hides_and_the_count_stays_exact() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(0));
    m.extend_to(NodeId(6));
    assert_eq!(m.picked_count(), 6);
    m.collapse(NodeId(1));
    assert_eq!((m.picked_count(), m.targets()), (4, n(&[0, 1, 4, 6])));
    assert!(!m.is_picked(NodeId(3)));
}

#[test]
fn a_refresh_that_removes_rows_prunes_the_set_and_a_lost_focus_takes_it_to_its_parent() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.select(NodeId(1));
    m.extend_to(NodeId(6)); // 1, 4, 6; focus 6
    t.remove(NodeId(4));
    m.refresh(&mut t, 0);
    assert_eq!((m.targets(), m.picked_count()), (n(&[1, 6]), 2), "the removed row left the set");
    t.remove(NodeId(6));
    m.refresh(&mut t, 0);
    assert_eq!((m.selected(), m.targets()), (Some(NodeId(0)), n(&[0])), "the focus fell back to its parent and the set followed");
}

#[test]
fn shift_keys_extend_and_left_right_end_the_set() {
    let (t, mut m) = open();
    m.select(NodeId(0));
    m.handle_key(&t, Key::Down, 10, true);
    m.handle_key(&t, Key::Down, 10, true);
    assert_eq!((m.targets(), m.selected()), (n(&[0, 1, 4]), Some(NodeId(4))));
    m.handle_key(&t, Key::Up, 10, true);
    assert_eq!(m.targets(), n(&[0, 1]));
    m.handle_key(&t, Key::End, 10, true);
    assert_eq!(m.targets(), n(&[0, 1, 4, 6]));
    m.handle_key(&t, Key::Home, 10, true);
    assert_eq!(m.targets(), n(&[0]));
    m.handle_key(&t, Key::PageDown, 2, true);
    assert_eq!(m.targets(), n(&[0, 1, 4]));
    m.handle_key(&t, Key::PageUp, 2, true);
    assert_eq!(m.targets(), n(&[0]));
    m.handle_key(&t, Key::End, 10, true);
    m.handle_key(&t, Key::Left, 10, true);
    assert_eq!(m.picked_count(), 1, "Left is a plain move");
}

#[test]
fn a_plain_key_move_picks_just_the_new_focus() {
    let (t, mut m) = open();
    m.select(NodeId(0));
    m.handle_key(&t, Key::Down, 10, true);
    m.handle_key(&t, Key::Down, 10, false);
    assert_eq!((m.targets(), m.selected()), (n(&[4]), Some(NodeId(4))));
}

#[test]
fn select_parent_moves_to_the_parent_row_and_stops_at_a_root() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(3));
    m.select_parent(&t);
    assert_eq!((m.selected(), m.targets()), (Some(NodeId(1)), n(&[1])));
    m.select_parent(&t);
    m.select_parent(&t);
    assert_eq!(m.selected(), Some(NodeId(0)), "a root has no parent");
}

#[test]
fn the_count_matches_the_picked_rows_after_any_mix_of_operations() {
    let t = crate::synth::kary_builder(60, 3).finish().unwrap();
    let mut m = RowModel::new(&t);
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for _ in 0..600 {
        let rows = m.rows().to_vec();
        let pick = |r: u64| rows[r as usize % rows.len()].id;
        match rnd(7) {
            0 => m.select(pick(rnd(1000))),
            1 => m.toggle_pick(pick(rnd(1000))),
            2 => m.extend_to(pick(rnd(1000))),
            3 => m.select_all(),
            4 => m.collapse(pick(rnd(1000))),
            5 => m.expand(&t, pick(rnd(1000))),
            _ => m.handle_key(&t, [Key::Up, Key::Down, Key::PageDown, Key::Home, Key::End][rnd(5) as usize], 5, rnd(2) == 0),
        }
        let want: Vec<NodeId> = m.rows().iter().map(|r| r.id).filter(|&id| m.is_picked(id)).collect();
        assert_eq!(m.targets(), want);
        assert_eq!(m.picked_count(), want.len(), "a pick without a row, or a miscounted one");
    }
}
```

Append to `crates/plumb-core/src/rows/filter_tests.rs`:

```rust
#[test]
fn a_filter_drops_the_picks_it_hides() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(2));
    m.toggle_pick(NodeId(3)); // 2 and 3 picked; focus 3
    m.set_filter(&t, Some(keep(&t, "f2")));
    assert_eq!((m.targets(), m.picked_count()), (vec![NodeId(3)], 1), "f1 is hidden, so its pick went");
    m.select(NodeId(0));
    m.set_filter(&t, None);
    m.select(NodeId(0));
    m.extend_to(NodeId(6));
    m.set_filter(&t, Some(keep(&t, "f")));
    assert_eq!((m.selected(), m.targets()), (Some(NodeId(0)), vec![NodeId(0)]), "the focus was hidden: the set follows it to its ancestor");
}
```

- [ ] **Step 2: Declare the module so the tests compile and fail**

In `crates/plumb-core/src/rows.rs`: add `mod pick;` on a new line after `use std::rc::Rc;`, and append after the `mod filter_tests;` lines:

```rust

#[cfg(test)]
mod pick_tests;
```

Run: `cargo test -p plumb-core rows`
Expected: FAIL to compile (`pick.rs` missing, `handle_key` takes 3 arguments, `extend_to` etc. undefined).

- [ ] **Step 3: Write `pick.rs`**

Create `crates/plumb-core/src/rows/pick.rs`:

```rust
//! The picked rows: the set that Shift and Ctrl clicks and Shift+arrows build, beside the focus row `selected` (the
//! cursor, which the rest of the program reads as "the selection"). Picks only ever name rows that exist, so
//! `picked_n` is the number the user sees and `targets` needs no visibility test.
use super::*;

/// Clears `id`'s bit and counts it out, for a caller that holds `picked` apart from the rest of the model.
pub(super) fn unpick(picked: &mut Vec<u64>, n: &mut usize, id: NodeId) {
    if bits::get(picked, id.idx()) {
        bits::set(picked, id.idx(), false);
        *n -= 1;
    }
}

impl RowModel {
    /// Picks only `id` and makes it the focus and the anchor.
    pub fn select(&mut self, id: NodeId) {
        self.clear_picks();
        self.set_pick(id, true);
        self.selected = Some(id);
        self.anchor = Some(id);
    }

    /// Picks the rows from the anchor to `id` by row order, inclusive, and moves the focus to `id`. The anchor stays;
    /// a missing one (none, or its row is collapsed or filtered away) is replaced by the focus, or by `id` when
    /// there is no focus either.
    pub fn extend_to(&mut self, id: NodeId) {
        let live = |m: &Self, n: Option<NodeId>| n.filter(|&n| m.row_index(n).is_some());
        let anchor = live(self, self.anchor).or(live(self, self.selected)).unwrap_or(id);
        let (Some(a), Some(b)) = (self.row_index(anchor), self.row_index(id)) else {
            self.select(id);
            return;
        };
        self.clear_picks();
        for i in a.min(b)..=a.max(b) {
            self.set_pick(self.rows[i].id, true);
        }
        self.selected = Some(id);
        self.anchor = Some(anchor);
    }

    /// Flips `id`'s pick and makes it the focus and the anchor. Unpicking the last row leaves nothing picked.
    pub fn toggle_pick(&mut self, id: NodeId) {
        self.set_pick(id, !self.is_picked(id));
        self.selected = Some(id);
        self.anchor = Some(id);
    }

    /// Picks every row. The focus and the anchor stay; with no focus it goes to the first row.
    pub fn select_all(&mut self) {
        let Some(first) = self.rows.first().map(|r| r.id) else { return };
        self.clear_picks();
        for i in 0..self.rows.len() {
            self.set_pick(self.rows[i].id, true);
        }
        if self.selected.is_none() {
            self.selected = Some(first);
        }
    }

    /// The picked rows, in row order.
    pub fn targets(&self) -> Vec<NodeId> {
        if self.picked_n == 0 {
            return Vec::new();
        }
        self.rows.iter().map(|r| r.id).filter(|&id| self.is_picked(id)).collect()
    }

    pub fn picked_count(&self) -> usize {
        self.picked_n
    }

    pub fn is_picked(&self, id: NodeId) -> bool {
        bits::get(&self.picked, id.idx())
    }

    /// Selects the focus row's parent, if that has a row.
    pub fn select_parent(&mut self, tree: &Tree) {
        let Some(s) = self.selected else { return };
        let p = tree.parent[s];
        if !p.is_none() && self.row_index(p).is_some() {
            self.select(p);
        }
    }

    fn set_pick(&mut self, id: NodeId, on: bool) {
        if self.is_picked(id) != on {
            bits::set(&mut self.picked, id.idx(), on);
            if on {
                self.picked_n += 1;
            } else {
                self.picked_n -= 1;
            }
        }
    }

    fn clear_picks(&mut self) {
        if self.picked_n > 0 {
            self.picked.fill(0);
            self.picked_n = 0;
        }
    }

    /// Keeps only the picks that have a row, after the rows were rebuilt.
    pub(super) fn prune_picks(&mut self) {
        if self.picked_n == 0 {
            return;
        }
        let mut keep = vec![0u64; self.picked.len()];
        let mut n = 0;
        for r in &self.rows {
            if self.is_picked(r.id) {
                bits::set(&mut keep, r.id.idx(), true);
                n += 1;
            }
        }
        self.picked = keep;
        self.picked_n = n;
    }

    /// After a rebuild moved the focus off a row that went (to an ancestor, or to nothing) the set follows it: the new
    /// focus alone, or nothing. `before` is the focus from before the rebuild.
    pub(super) fn follow_focus(&mut self, before: Option<NodeId>) {
        if self.selected == before {
            return;
        }
        match self.selected {
            Some(f) => self.select(f),
            None => {
                self.clear_picks();
                self.anchor = None;
            }
        }
    }
}
```

- [ ] **Step 4: Wire the model in `rows.rs`**

Apply these edits (each `old` text appears once):

1. In `pub struct RowModel`, after the `selected: Option<NodeId>,` line add:

```rust
    /// The rows picked with Shift or Ctrl (see `pick.rs`); `selected` is the focus, within them or beside them.
    picked: Vec<u64>,
    /// Set bits in `picked`; every method that writes `picked` keeps it exact.
    picked_n: usize,
    /// Where a Shift extension starts.
    anchor: Option<NodeId>,
```

2. In `RowModel::new`, after `selected: None,` add `picked: Vec::new(),`, `picked_n: 0,`, `anchor: None,` (one per line, same indentation).

3. Delete the old method (`pick.rs` owns it now):

```rust
    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
    }

```

4. `rebuild` becomes:

```rust
    fn rebuild(&mut self, tree: &Tree) {
        let mut rows = Vec::new();
        self.push_children(tree, NodeId::NONE, 0, &mut rows);
        self.rows = rows;
        self.prune_picks();
    }
```

5. In `collapse`, replace `self.rows.drain(i + 1..end);` with:

```rust
            for r in self.rows.drain(i + 1..end) {
                pick::unpick(&mut self.picked, &mut self.picked_n, r.id);
            }
```

6. In `set_filter`, replace the block from `self.rebuild(tree);` to the closing brace of the `while` with:

```rust
        self.rebuild(tree);
        let before = self.selected;
        while let Some(s) = self.selected {
            if self.row_index(s).is_some() {
                break;
            }
            let p = tree.parent[s];
            self.selected = (!p.is_none()).then_some(p);
        }
        self.follow_focus(before);
```

7. In `refresh`, the same: after `self.rebuild(tree);` insert `let before = self.selected;` and after the `while` loop's closing brace insert `self.follow_focus(before);` (the loop body is unchanged).

8. Replace `handle_key` (signature, the nothing-selected branch, the last line) so it reads:

```rust
    /// Moves the focus. `page` is the number of rows PageUp/PageDown move. With `extend`, the keys that move to another
    /// row (not Left and Right, which open and close folders) extend the picked range to the new focus instead of
    /// picking only it.
    pub fn handle_key(&mut self, tree: &Tree, key: Key, page: usize, extend: bool) {
        if self.rows.is_empty() {
            return;
        }
        let Some(cur) = self.selected_index() else {
            self.select(self.rows[0].id);
            return;
        };
```

(the `let last = ...` through the `let target = match key { ... };` block stays as is) and the final line `self.selected = Some(self.rows[target].id);` becomes:

```rust
        let to = self.rows[target].id;
        if extend && !matches!(key, Key::Left | Key::Right) {
            self.extend_to(to);
        } else {
            self.select(to);
        }
```

9. Update callers: in `rows.rs` tests, `sed -i -E '/m\.handle_key\(/ s/(handle_key\([^()]*)\)/\1, false)/' crates/plumb-core/src/rows.rs`; in `crates/plumb/src/app.rs:1039`, `m.handle_key(t, k, self.layout.rows_visible().max(1));` becomes `m.handle_key(t, k, self.layout.rows_visible().max(1), false);` (T4 turns that `false` into `mods.shift`). Then `grep -rn "handle_key(" crates` must show no call with three arguments.

- [ ] **Step 5: Run the model tests**

Run: `cargo test -p plumb-core rows`
Expected: PASS (all `rows::pick_tests::*`, the new filter test, and the 31 existing row tests).
If `the_count_matches_the_picked_rows...` fails, the assertion message says whether a pick had no row or the count drifted; fix the method that wrote `picked`, not the test.

- [ ] **Step 6: Whole-workspace compile check and commit**

Run: `cargo test -p plumb-core` then `cargo build -p plumb`
Expected: both succeed.

```bash
git add crates docs
git commit -m "Row model: a picked set beside the focus row, with Shift extension, Ctrl toggle, select all and parent" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Carry modifier keys in `Input` (no behaviour change)

**Files:**
- Modify: `crates/plumb/src/app.rs` (types, `modal_input`, `handle`)
- Modify: `crates/plumb/src/window.rs` (`mods_now`, `dispatch` closure, `key_input`)
- Modify (mechanical, via script): `crates/plumb/src/app.rs` tests, `crates/plumb/src/app/filter.rs` tests, `crates/plumb/src/window/selftest.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub struct Mods { pub shift: bool, pub ctrl: bool }` (`Clone, Copy, Debug, Default, PartialEq, Eq`) in `crate::app`; `Input::Click(f32, f32, Mods)`; `Input::Key(Key, Mods)`; `window::mods_now() -> Mods`.

- [ ] **Step 1: Types**

In `crates/plumb/src/app.rs` above `pub enum Input`, add:

```rust
/// The modifier keys held when a click or a key arrived, read by the window at that moment: a flag kept in `App` would
/// have to survive key-up, focus loss and Alt-Tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
}
```

Change the two variants to `Click(f32, f32, Mods),` and `Key(Key, Mods),`.

- [ ] **Step 2: Patterns in non-test code**

In `modal_input` (`app.rs` ~961, ~982, ~986): `Input::Click(x, y) | Input::DoubleClick(x, y)` becomes `Input::Click(x, y, _) | Input::DoubleClick(x, y)`; `Input::Key(Key::Left | Key::Up)` becomes `Input::Key(Key::Left | Key::Up, _)`; `Input::Key(Key::Right | Key::Down)` becomes `Input::Key(Key::Right | Key::Down, _)`.

In `handle`: the outcome-reset `matches!` (~1019) uses `Input::Click(..)` and `Input::Key(_)`: change `Input::Key(_)` to `Input::Key(..)`. The arms become `Input::Key(k, _) => {` (body unchanged for now) and `Input::Click(x, y, _) => return self.click(x, y),`.

- [ ] **Step 3: Constructors in test code, by script**

Write `C:\Users\WINSTO~1\AppData\Local\Temp\claude\C--Users-Winston-Li-Documents-GitHub-plumb\ef18114e-f0fc-43cd-956d-0c6d01aa977e\scratchpad\addmods.py` (any scratch path):

```python
import pathlib, re

def rewrite(text, start):
    head, body = text[:start], text[start:]
    out, pos = [], 0
    for m in re.finditer(r"Input::(Click|Key)\(", body):
        if m.start() < pos:
            continue
        depth, i = 1, m.end()
        while depth:
            depth += {"(": 1, ")": -1}.get(body[i], 0)
            i += 1
        out.append(body[pos:i - 1] + ", Default::default()")
        pos = i - 1
    out.append(body[pos:])
    return head + "".join(out)

root = pathlib.Path("crates/plumb/src")
for rel, marker in [("app.rs", "\nmod tests {"), ("app/filter.rs", "\nmod tests {"), ("window/selftest.rs", None)]:
    p = root / rel
    s = p.read_text(encoding="utf-8")
    p.write_text(rewrite(s, s.index(marker) if marker else 0), encoding="utf-8", newline="")
```

Run it from the repo root as a file with stdin closed: `python -P <path-to-addmods.py> < /dev/null`.
Then `git diff --stat` (expect `app.rs`, `app/filter.rs`, `window/selftest.rs` only) and read `git diff -U0 | grep '^[-+].*Input::\(Click\|Key\)'` for oddities (a constructor inside a macro, a closure). The non-test patterns from Step 2 are before the `mod tests` marker and are not touched.

- [ ] **Step 4: Window**

In `crates/plumb/src/window.rs`:

Add `Mods` to the `use crate::app::{...}` list on line 3.

Add near `key_of`:

```rust
/// The Shift and Ctrl keys as they are now.
fn mods_now() -> Mods {
    let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    Mods { shift: down(VK_SHIFT.0 as i32), ctrl: down(VK_CONTROL.0 as i32) }
}
```

`WM_LBUTTONDOWN`: replace `dispatch(hwnd, lp, Input::Click);` with

```rust
            let mods = mods_now();
            dispatch(hwnd, lp, |x, y| Input::Click(x, y, mods));
```

`key_input` takes the modifiers: change the signature to `fn key_input(vk: u16, mods: Mods, hwnd: HWND) -> Option<Input>`, `if ctrl {` to `if mods.ctrl {`, the last arm to `x => Input::Key(key_of(x)?, mods),`. In `WM_KEYDOWN` the call becomes `key_input(wp.0 as u16, Mods { shift, ctrl }, hwnd)`.

- [ ] **Step 5: Verify nothing changed**

Run: `cargo test -p plumb`
Expected: PASS with the same count as before this task (run `cargo test -p plumb 2>&1 | tail -3` on the previous commit first if the number is not known).

- [ ] **Step 6: Commit**

```bash
git diff --stat
git add crates
git commit -m "Input: Click and Key carry the modifier keys held" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Mouse and key selection in the app (`app/select.rs`)

**Files:**
- Create: `crates/plumb/src/app/select.rs`
- Modify: `crates/plumb/src/app.rs` (`mod select;`, field `row_drag`, `handle` arms, `click` row branch, `RightClick`, `Release`, `Drag`)

**Interfaces:**
- Consumes: `RowModel::{select, extend_to, toggle_pick, select_all, is_picked, picked_count, handle_key}` (T2), `Mods` (T3), `App::{icon_at, row_at, set_scroll, total_rows, held_mark_stale}`.
- Produces: `App::row_dragging(&self) -> bool` (pub); `pub(super)`: `click_row(i, x, mods)`, `drag_select(y)`, `move_focus(k, mods)`, `select_all_rows()`.

- [ ] **Step 1: Write the failing tests**

Create `crates/plumb/src/app/select.rs` containing only the module doc, `use super::*;` and this test module (the `impl` comes in Step 3):

```rust
//! Selecting several rows: Shift and Ctrl clicks, dragging over rows, Shift+arrows, Ctrl+A, and what Del and Ctrl+C do
//! with the set. The model keeps the set (`plumb_core::rows`); this maps input onto it.
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Input, ViewInfo, ViewKind};
    use crate::filter_ui::Edit;
    use crate::layout::{Layout, HEADER_H, INDENT, PICKER_H, ROW_H};
    use plumb_core::{NodeFlags, Tree, TreeBuilder};

    const NONE: Mods = Mods { shift: false, ctrl: false };
    const SHIFT: Mods = Mods { shift: true, ctrl: false };
    const CTRL: Mods = Mods { shift: false, ctrl: true };

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

    /// Every folder open: C:, src, main.rs, deep, more.txt, docs, notes.txt, todo.md, readme.md.
    pub(super) fn app_h(h: f32) -> App {
        let views = vec![ViewInfo { root: "C:\\".into(), total: 1000, free: 500, kind: ViewKind::Disk }];
        let mut a = App::new(Layout::new(800.0, h), views, "test".into());
        a.begin_scan(0);
        a.finish_scan(tree(), "done".into());
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
        while let Some(id) = m.rows().iter().map(|r| r.id).find(|&id| m.is_expandable(t, id) && !m.is_expanded(id)) {
            m.expand(t, id);
        }
        a
    }

    pub(super) fn names(a: &App) -> Vec<String> {
        let t = a.slot.tree.as_ref().unwrap();
        a.slot.model.as_ref().unwrap().rows().iter().map(|r| t.name_string(r.id)).collect()
    }

    pub(super) fn at(a: &App, name: &str) -> usize {
        names(a).iter().position(|n| n == name).unwrap_or_else(|| panic!("no row {name}"))
    }

    pub(super) fn picked(a: &App) -> Vec<String> {
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_ref().unwrap());
        m.targets().into_iter().map(|id| t.name_string(id)).collect()
    }

    pub(super) fn focus(a: &App) -> Option<String> {
        let (t, m) = (a.slot.tree.as_ref()?, a.slot.model.as_ref()?);
        m.selected().map(|id| t.name_string(id))
    }

    pub(super) fn row_y(i: usize) -> f32 {
        PICKER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    /// A press on the name of visible row `i` (clear of the folder icon).
    pub(super) fn press(a: &mut App, i: usize, mods: Mods) -> Effect {
        a.handle(Input::Click(150.0, row_y(i), mods))
    }

    pub(super) fn key(a: &mut App, k: Key, mods: Mods) {
        a.handle(Input::Key(k, mods));
    }

    #[test]
    fn the_fixture_lists_every_row_in_size_order() {
        let a = app_h(600.0);
        assert_eq!(names(&a), ["C:", "src", "main.rs", "deep", "more.txt", "docs", "notes.txt", "todo.md", "readme.md"]);
    }

    #[test]
    fn shift_click_picks_the_range_from_the_anchor_and_ctrl_click_flips_one_row() {
        let mut a = app_h(600.0);
        let (main, more, docs) = (at(&a, "main.rs"), at(&a, "more.txt"), at(&a, "docs"));
        press(&mut a, main, NONE);
        assert_eq!(picked(&a), ["main.rs"]);
        press(&mut a, more, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"]);
        press(&mut a, docs, CTRL);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt", "docs"]);
        press(&mut a, more, CTRL);
        assert_eq!(picked(&a), ["main.rs", "deep", "docs"]);
        press(&mut a, main, NONE);
        assert_eq!(picked(&a), ["main.rs"], "a plain click drops the set");
    }

    #[test]
    fn the_folder_icon_opens_and_closes_whatever_modifiers_are_held() {
        let mut a = app_h(600.0);
        let docs = at(&a, "docs");
        let x = a.layout.column(0).0 + 8.0 + INDENT + 4.0; // docs is at depth 1
        a.handle(Input::Click(x, row_y(docs), CTRL));
        assert!(!names(&a).contains(&"notes.txt".to_string()), "closed");
        assert_eq!(picked(&a), ["docs"]);
        assert!(!a.row_dragging(), "the icon starts no drag");
    }

    #[test]
    fn shift_arrows_extend_from_the_anchor_and_a_plain_arrow_ends_the_set() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        key(&mut a, Key::Down, SHIFT);
        key(&mut a, Key::Down, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"]);
        assert_eq!(focus(&a).as_deref(), Some("more.txt"));
        key(&mut a, Key::Up, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep"]);
        key(&mut a, Key::Down, NONE);
        assert_eq!(picked(&a), ["more.txt"]);
    }

    #[test]
    fn ctrl_a_picks_every_row_unless_the_filter_box_has_the_focus() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        a.handle(Input::Edit(Edit::SelectAll));
        assert_eq!(picked(&a).len(), 9);
        assert_eq!(focus(&a).as_deref(), Some("main.rs"), "the focus stays");
        press(&mut a, main, NONE);
        a.handle(Input::Find); // focus the box
        a.handle(Input::Edit(Edit::SelectAll));
        assert_eq!(picked(&a), ["main.rs"], "in the box, Ctrl+A selects its text");
    }

    #[test]
    fn shift_left_in_the_filter_box_leaves_the_row_selection_alone() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        a.handle(Input::Find);
        key(&mut a, Key::Left, SHIFT); // the window turns Shift+Left/Right/Home/End into box edits before this; Left stays the box's
        assert_eq!(picked(&a), ["main.rs"]);
    }

    #[test]
    fn a_drag_extends_to_the_row_under_the_pointer_and_a_ctrl_press_starts_none() {
        let mut a = app_h(600.0);
        press(&mut a, 2, NONE);
        assert!(a.row_dragging());
        a.handle(Input::Drag(150.0, row_y(5)));
        assert_eq!(picked(&a).len(), 4, "rows 2 to 5");
        a.handle(Input::Drag(150.0, row_y(1)));
        assert_eq!(picked(&a).len(), 2, "rows 1 to 2: the anchor stays on row 2");
        a.handle(Input::Release);
        assert!(!a.row_dragging());
        press(&mut a, 3, CTRL);
        assert!(!a.row_dragging(), "Ctrl starts no drag");
        let before = picked(&a);
        a.handle(Input::Drag(150.0, row_y(7)));
        assert_eq!(picked(&a), before);
    }

    #[test]
    fn dragging_past_the_bottom_edge_scrolls_and_keeps_extending() {
        let mut a = app_h(200.0);
        let vis = a.layout.rows_visible();
        assert!((2..9).contains(&vis), "the list must scroll for this test, {vis} rows fit");
        press(&mut a, 0, NONE);
        let area = a.layout.rows_area();
        a.handle(Input::Drag(150.0, area.y + area.h + 30.0));
        assert!(a.slot.scroll > 0, "scrolled");
        let n = a.slot.model.as_ref().unwrap().picked_count();
        assert!(n > vis - 1, "the range grew past the rows that were on screen: {n}");
        a.handle(Input::Release);
        a.handle(Input::Drag(150.0, area.y + 2.0));
        assert_eq!(a.slot.model.as_ref().unwrap().picked_count(), n, "no drag after release");
    }

    #[test]
    fn a_right_click_keeps_a_picked_set_and_otherwise_selects_the_row() {
        let mut a = app_h(600.0);
        let (main, more, docs) = (at(&a, "main.rs"), at(&a, "more.txt"), at(&a, "docs"));
        press(&mut a, main, NONE);
        press(&mut a, more, SHIFT);
        let id = a.slot.model.as_ref().unwrap().rows()[main].id;
        assert_eq!(a.handle(Input::RightClick(150.0, row_y(main))), Effect::Menu(id));
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"], "the set survives a right-click on one of its rows");
        a.handle(Input::RightClick(150.0, row_y(docs)));
        assert_eq!(picked(&a), ["docs"], "an unpicked row becomes the selection");
    }
}
```

Run: `cargo test -p plumb select`
Expected: FAIL to compile (`mod select;` not declared yet, `row_dragging` missing). Declare in Step 2.

- [ ] **Step 2: State and declarations in `app.rs`**

- After `mod map;` add `mod select;`.
- In `pub struct App`, after `map_drag: Option<(f32, f32)>,` add:

```rust
    /// The left button went down on a row and is still down: moving the pointer extends the selection (`drag_select`).
    row_drag: bool,
```

- In `App::new`, after `map_drag: None,` add `row_drag: false,`.

- [ ] **Step 3: The mapping in `select.rs`**

Insert above the `#[cfg(test)]` module:

```rust
impl App {
    /// Whether a press on a row is being dragged over more rows (the window keeps a timer running meanwhile).
    pub fn row_dragging(&self) -> bool {
        self.row_drag
    }

    /// A press on visible row `i` at x: Shift extends the selection to it, Ctrl flips it, a plain press selects it. The
    /// folder icon opens or closes the folder as always and ignores the modifiers. A press that is not Ctrl and not on
    /// the icon starts a drag.
    pub(super) fn click_row(&mut self, i: usize, x: f32, mods: Mods) {
        let folder = self.icon_at(i, x);
        let Some(id) = self.row_at(i) else { return };
        let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) else { return };
        let mut again = false;
        if folder.is_some() {
            m.select(id);
            m.toggle(t, id);
            self.set_scroll(self.slot.scroll);
        } else if mods.shift {
            m.extend_to(id);
        } else if mods.ctrl {
            m.toggle_pick(id);
        } else {
            again = m.selected() == Some(id);
            m.select(id);
        }
        self.row_drag = folder.is_none() && !mods.ctrl;
        if again {
            self.held_mark_stale();
        }
    }

    /// The pointer is down on a row and is at height `y`: the selection extends to the row under it, and the list scrolls
    /// 1 to 5 rows while the pointer is beyond the top or bottom of it, by how far beyond.
    pub(super) fn drag_select(&mut self, y: f32) {
        let area = self.layout.rows_area();
        let beyond = if y < area.y {
            y - area.y
        } else if y >= area.y + area.h {
            y - (area.y + area.h) + 1.0
        } else {
            0.0
        };
        if beyond != 0.0 {
            let step = ((beyond.abs() / ROW_H).ceil() as i64).clamp(1, 5);
            self.set_scroll((self.slot.scroll as i64 + step * beyond.signum() as i64).max(0) as usize);
        }
        let on_screen = ((y - area.y) / ROW_H).max(0.0) as usize;
        let idx = (self.slot.scroll + on_screen.min(self.layout.rows_visible().saturating_sub(1))).min(self.total_rows().saturating_sub(1));
        if let Some(m) = &mut self.slot.model
            && let Some(id) = m.rows().get(idx).map(|r| r.id)
        {
            m.extend_to(id);
        }
    }

    /// A moving key on the list: the focus moves, and with Shift the selection extends to it.
    pub(super) fn move_focus(&mut self, k: Key, mods: Mods) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
            m.handle_key(t, k, self.layout.rows_visible().max(1), mods.shift);
            let s = m.scroll_to_selected(self.slot.scroll, self.layout.rows_visible());
            self.set_scroll(s);
        }
    }

    /// Ctrl+A with the list focused.
    pub(super) fn select_all_rows(&mut self) {
        if let Some(m) = &mut self.slot.model {
            m.select_all();
        }
    }
}
```

`ROW_H` must be imported: add `use crate::layout::ROW_H;` below `use super::*;` if `ROW_H` is not already in scope through `super::*` (`app.rs` imports `layout::{.., INDENT, ..}` but not `ROW_H`; add it to that `use` list in `app.rs` instead so tests see it too).

- [ ] **Step 4: Route the input in `handle` and `click`**

In `handle`:

- Replace the `Input::Key(k, _) => {` arm with:

```rust
            Input::Key(k, mods) => {
                if self.slot.filter.ui.focus
                    && let Some(e) = filter::edit_for_key(k)
                {
                    return self.filter_edit(e);
                }
                self.move_focus(k, mods);
            }
```

- Add, before `Input::Edit(e) => return self.filter_edit(e),`:

```rust
            Input::Edit(Edit::SelectAll) if !self.slot.filter.ui.focus => self.select_all_rows(),
```

- `Input::Click(x, y, _) => return self.click(x, y),` becomes `Input::Click(x, y, mods) => return self.click(x, y, mods),` and `fn click(&mut self, x: f32, y: f32)` becomes `fn click(&mut self, x: f32, y: f32, mods: Mods)`. The `DoubleClick` arm's two `self.click(x, y)` calls become `self.click(x, y, Mods::default())`.

- In `click`, replace the whole `Hit::Row(i) => { ... }` arm with:

```rust
            Hit::Row(i) => self.click_row(i, x, mods),
```

- `Input::RightClick` row branch becomes:

```rust
                if let Hit::Row(i) = self.layout.hit(x, y, self.views.len())
                    && let (Some(id), Some(m)) = (self.row_at(i), &mut self.slot.model)
                {
                    if !(m.is_picked(id) && m.picked_count() > 1) {
                        m.select(id);
                    }
                    return Effect::Menu(id);
                } else if ...
```

- `Input::Drag`: append a branch after the `map_drag` one:

```rust
                } else if self.row_drag {
                    self.drag_select(y);
                }
```

- `Input::Release`: add `self.row_drag = false;`.

- [ ] **Step 5: Run**

Run: `cargo test -p plumb select`
Expected: PASS (9 tests in `app::select::tests`). If `edit_for_key` maps Down after all, drop nothing: the test uses Left, which it maps (the window already turns Shift+Left/Right/Home/End into box edits before `key_input`; Shift+Up/Down with the box focused extend rows, as plain Up/Down move them today).
Then `cargo test -p plumb` (everything still green).

- [ ] **Step 6: Commit**

```bash
git add crates
git commit -m "Selection: Shift and Ctrl click, Shift+arrows, Ctrl+A and drag over rows, in app/select.rs" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Del and copy over the whole set

**Files:**
- Modify: `crates/plumb/src/app/select.rs` (add `topmost`, `mark_selected`, `copy_text`; tests)
- Modify: `crates/plumb/src/app/marks.rs` (delete `toggle_mark_selected`)
- Modify: `crates/plumb/src/app.rs` (Del arm, `fs_path`, `menu_target` uses it)
- Modify: `crates/plumb/src/window.rs` (Ctrl+C uses `copy_text`; delete `selected_path`)

**Interfaces:**
- Consumes: `RowModel::{targets, is_picked}`, `App::{mark_state, basket, outcome}`, `Basket::{contains, toggle}`.
- Produces: `pub fn App::mark_selected(&mut self)`, `pub fn App::copy_text(&self) -> Option<String>`, `pub fn App::fs_path(&self, NodeId) -> String` (empty when unknown).

- [ ] **Step 1: Write the failing tests**

Add to `select.rs` `mod tests` (they share the helpers; add `use plumb_core::basket;` is not needed):

```rust
    #[test]
    fn del_marks_every_picked_row_then_unmarks_them_all() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2);
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "all marked: Del unmarks");
    }

    #[test]
    fn del_over_a_mix_marks_the_rows_not_yet_marked() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        a.handle(Input::Edit(Edit::Delete)); // notes marked
        press(&mut a, todo, SHIFT); // picks notes and todo
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2, "todo was marked, notes kept its mark");
    }

    #[test]
    fn del_over_nested_picks_marks_only_the_topmost() {
        let mut a = app_h(600.0);
        let (src, deep) = (at(&a, "src"), at(&a, "more.txt"));
        press(&mut a, src, NONE);
        press(&mut a, deep, SHIFT); // src, main.rs, deep, more.txt
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 1);
        let src_id = a.slot.model.as_ref().unwrap().rows()[src].id;
        assert!(a.basket().contains(&a.slot.tree.as_ref().unwrap().path(src_id)));
    }

    #[test]
    fn del_marks_a_child_of_a_marked_folder_as_it_always_did() {
        let mut a = app_h(600.0);
        let (src, main) = (at(&a, "src"), at(&a, "main.rs"));
        press(&mut a, src, NONE);
        a.handle(Input::Edit(Edit::Delete));
        press(&mut a, main, NONE);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2);
    }

    #[test]
    fn a_refused_row_is_skipped_and_the_status_line_says_why() {
        let mut a = app_h(600.0);
        press(&mut a, 0, NONE); // the drive root
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("cannot mark: it is a drive root"), "{}", status_str(&a, true));
        a.handle(Input::Edit(Edit::SelectAll)); // everything, root included: the root covers all, and is refused
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "Ctrl+A then Del at a drive root marks nothing");
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        a.slot.scan = Scan::Refreshing; // marks are refused while a scan runs
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("2 not marked: the drive is being scanned"), "{}", status_str(&a, true));
    }

    #[test]
    fn copy_text_lists_the_paths_of_every_picked_row_one_per_line() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        let m = a.slot.model.as_ref().unwrap();
        let want: Vec<String> = m.targets().into_iter().map(|id| a.fs_path(id)).collect();
        assert_eq!(want.len(), 2);
        assert!(want.iter().all(|p| p.ends_with(".txt") || p.ends_with(".md")), "{want:?}");
        assert_eq!(a.copy_text(), Some(want.join("\r\n")));
        a.handle(Input::Edit(Edit::SelectAll));
        let all = a.copy_text().unwrap();
        assert_eq!(all.split("\r\n").count(), 9, "nested rows are copied too");
    }
```

`status_str` is a helper in `app.rs`'s `mod tests` (private). Make it reachable: it is `fn status_str(a: &App, full: bool) -> String` in that module; if it is not `pub(super)`, add `pub(super)` to its declaration in `app.rs` tests and import it with `use crate::app::tests::status_str;` in `select.rs`'s test module (add the import line). If `app.rs`'s `mod tests` is not visible from the sibling module, instead write in `select.rs` tests: `fn status_str(a: &App) -> String { a.status_line() }` using the public status accessor that `app.rs`'s `status_str` wraps (read its two-line body first and copy it).

Run: `cargo test -p plumb select`
Expected: FAIL (`fs_path`, `copy_text` missing; Del still toggles one row).

- [ ] **Step 2: Implement**

In `app.rs`, split the file-system path out of `menu_target`:

```rust
    /// The file system path of row `id` (what Reveal and Copy path use); empty when it is not known (a process whose
    /// executable cannot be read).
    pub fn fs_path(&self, id: NodeId) -> String {
        let Some(tree) = self.slot.tree.as_ref() else { return String::new() };
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2] as u32);
            return self.slot.proc_info.get(&pid).and_then(|i| i.path.clone()).unwrap_or_default();
        }
        fmt::node_path(tree, id).to_string()
    }
```

and in `menu_target` replace the two path computations: the Processes branch becomes `return Some((String::new(), self.fs_path(id), self.mark_state(id), false));` (keep the `tree.aux.get(id.idx())?` bail-out line before it so a bad id still gives `None`), the disk branch becomes `Some((tree.path(id), self.fs_path(id), self.mark_state(id), file))`.

Replace the Del arm: `Input::Edit(Edit::Delete) if !self.slot.filter.ui.focus => self.mark_selected(),`.

Delete `toggle_mark_selected` from `marks.rs` (lines 94-98 plus the blank line).

Add to the `impl App` in `select.rs`:

```rust
    /// The picked rows that have no picked ancestor, in row order: what Del and the set menu act on, so that Ctrl+A then Del
    /// marks the roots and not every row under them.
    pub(super) fn topmost(&self) -> Vec<NodeId> {
        let (Some(t), Some(m)) = (&self.slot.tree, &self.slot.model) else { return Vec::new() };
        let covered = |id: NodeId| {
            let mut p = t.parent[id];
            while !p.is_none() {
                if m.is_picked(p) {
                    return true;
                }
                p = t.parent[p];
            }
            false
        };
        m.targets().into_iter().filter(|&id| !covered(id)).collect()
    }

    /// Del (and the menu's Mark selected): over the topmost picked rows, if each is marked exactly (the test `mark_state`
    /// uses) it unmarks the exact mark of every picked row; otherwise it marks each not yet marked. Rows that cannot be
    /// marked are skipped and the status line says how many and why.
    pub fn mark_selected(&mut self) {
        let Some(tree) = self.slot.tree.as_ref() else { return };
        let top = self.topmost();
        if top.is_empty() {
            return;
        }
        let marked: Vec<bool> = top.iter().map(|&id| self.slot.basket.contains(&tree.path(id))).collect();
        if marked.iter().all(|&m| m) {
            let exact: Vec<String> = self.slot.model.as_ref().map(|m| m.targets()).unwrap_or_default().into_iter().map(|id| tree.path(id)).filter(|p| self.slot.basket.contains(p)).collect();
            for p in &exact {
                self.slot.basket.toggle(p);
            }
            self.basket_rev += 1;
            return;
        }
        let (mut todo, mut refused) = (Vec::new(), Vec::new());
        for (&id, _) in top.iter().zip(&marked).filter(|(_, m)| !**m) {
            match self.mark_state(id) {
                MarkState::Refused(why) => refused.push(why),
                _ => todo.push(tree.path(id)),
            }
        }
        for p in &todo {
            self.slot.basket.toggle(p);
        }
        if !todo.is_empty() {
            self.basket_rev += 1;
        }
        if let Some(why) = refused.first() {
            self.outcome = Outcome::problem(if todo.is_empty() && refused.len() == 1 {
                format!("cannot mark: {why}")
            } else {
                format!("{} not marked: {why}", refused.len())
            });
        }
    }

    /// The file system paths of the picked rows, nested ones included, one per line (CRLF), for the clipboard; `None`
    /// when there are none to copy.
    pub fn copy_text(&self) -> Option<String> {
        let m = self.slot.model.as_ref()?;
        let paths: Vec<String> = m.targets().into_iter().map(|id| self.fs_path(id)).filter(|p| !p.is_empty()).collect();
        (!paths.is_empty()).then(|| paths.join("\r\n"))
    }
```

In `window.rs`: in `WM_KEYDOWN`, the Ctrl+C branch becomes

```rust
            } else if ctrl && wp.0 == 'C' as usize {
                if let Some(Some(text)) = with(|w| w.app.copy_text()) {
                    copy_to_clipboard(hwnd, &text);
                }
```

and delete `fn selected_path` (the `impl Win` method at ~916).

- [ ] **Step 3: Run**

Run: `cargo test -p plumb select` then `cargo test -p plumb`
Expected: PASS. The existing Del tests in `app.rs` (`del_marks_and_unmarks_the_selection_and_the_filter_box_keeps_its_own_del`, `roots_and_scans_are_refused_with_a_reason_that_reaches_the_status_line`) must still pass unchanged: that proves single-row Del and its `cannot mark: <reason>` wording are unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates
git commit -m "Del and Ctrl+C act on every picked row; the old one-row paths are gone" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Keyboard parity in the app: Enter, Backspace, F5, key-opened menu

**Files:**
- Modify: `crates/plumb/src/app.rs` (`Input::{Refresh, ContextMenu}`, arms, outcome-reset list)
- Modify: `crates/plumb/src/app/select.rs` (add `toggle_focus_row`, `select_parent_row`, `refresh_key`, `context_menu_key`, `focus_row_rect`; tests)

**Interfaces:**
- Consumes: `RowModel::{toggle, select_parent, selected_index, scroll_to_selected}`, `Layout::row`.
- Produces: `Input::Refresh`, `Input::ContextMenu`; `pub fn App::focus_row_rect(&self) -> Option<Rect>` (client dip rectangle of the focus row if it is on screen).

- [ ] **Step 1: Write the failing tests**

Add to `select.rs` `mod tests`:

```rust
    #[test]
    fn enter_opens_and_closes_the_focus_folder_and_the_box_keeps_its_own_enter() {
        let mut a = app_h(600.0);
        let docs = at(&a, "docs");
        press(&mut a, docs, NONE);
        a.handle(Input::Enter);
        assert!(!names(&a).contains(&"notes.txt".to_string()), "closed");
        a.handle(Input::Enter);
        assert!(names(&a).contains(&"notes.txt".to_string()), "open again");
        a.handle(Input::Find);
        a.handle(Input::Edit(Edit::Char('x')));
        a.handle(Input::Enter); // moves focus to the list, on the first row
        assert_eq!(focus(&a).as_deref(), Some("C:"));
    }

    #[test]
    fn backspace_selects_the_parent_and_the_box_keeps_its_own_backspace() {
        let mut a = app_h(600.0);
        let more = at(&a, "more.txt");
        press(&mut a, more, NONE);
        a.handle(Input::Edit(Edit::Backspace));
        assert_eq!(focus(&a).as_deref(), Some("deep"));
        assert_eq!(picked(&a), ["deep"]);
        a.handle(Input::Find);
        a.handle(Input::Edit(Edit::Char('q')));
        a.handle(Input::Edit(Edit::Backspace));
        assert_eq!(focus(&a).as_deref(), Some("deep"), "Backspace in the box edits its text");
        assert_eq!(a.filter_ui().text, "");
    }

    #[test]
    fn f5_scans_when_there_is_no_tree_rescans_with_one_and_waits_for_a_running_scan() {
        let mut a = app_h(600.0);
        a.active = Some(0);
        assert_eq!(a.handle(Input::Refresh), Effect::Rescan(0));
        a.slot.scan = Scan::Refreshing;
        assert_eq!(a.handle(Input::Refresh), Effect::None, "a scan is already running");
        a.slot.scan = Scan::Idle;
        a.fail_scan("boom".into());
        assert_eq!(a.handle(Input::Refresh), Effect::Scan(0), "no tree: F5 scans again");
        a.active = None;
        assert_eq!(a.handle(Input::Refresh), Effect::None, "no view shown");
    }

    #[test]
    fn the_context_menu_key_opens_the_focus_row_menu_and_brings_the_row_into_view() {
        let mut a = app_h(200.0);
        assert_eq!(a.handle(Input::ContextMenu), Effect::None, "nothing focused");
        press(&mut a, 0, NONE);
        key(&mut a, Key::End, NONE);
        a.slot.scroll = 0; // the focus is off screen
        let want = a.slot.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.handle(Input::ContextMenu), Effect::Menu(want));
        let r = a.focus_row_rect().expect("the row was scrolled into view");
        let area = a.layout.rows_area();
        assert!(r.y >= area.y && r.y + r.h <= area.y + area.h + 0.5, "{r:?} within {area:?}");
    }
```

`a.filter_ui()` is the public accessor `window.rs` uses (`w.app.filter_ui()`); `.text` is the field `filter.rs` uses (`self.slot.filter.ui.text`). If the field is private, assert through `a.filter_ui().text()`; read `filter_ui.rs` for the accessor and use that.

Run: `cargo test -p plumb select`
Expected: FAIL (`Input::Refresh` undefined).

- [ ] **Step 2: Implement**

In `app.rs`:

- `pub enum Input`: add

```rust
    /// F5: scan the shown view again.
    Refresh,
    /// Shift+F10 or the Apps key: the context menu of the focus row.
    ContextMenu,
```

- The outcome-reset `matches!` list gains `| Input::Refresh | Input::ContextMenu`.
- Replace `Input::Enter => self.filter_enter(),` with

```rust
            Input::Enter if self.slot.filter.ui.focus => self.filter_enter(),
            Input::Enter => self.toggle_focus_row(),
            Input::Edit(Edit::Backspace) if !self.slot.filter.ui.focus => self.select_parent_row(),
            Input::Refresh => return self.refresh_key(),
            Input::ContextMenu => return self.context_menu_key(),
```

(`Input::Edit(Edit::Backspace)` must precede the generic `Input::Edit(e) => return self.filter_edit(e)` arm; put these four lines in that order before it.)

In `select.rs` `impl App`:

```rust
    /// Enter on the list: opens or closes the focus row's folder.
    pub(super) fn toggle_focus_row(&mut self) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model)
            && let Some(id) = m.selected()
        {
            m.toggle(t, id);
            self.set_scroll(self.slot.scroll);
        }
    }

    /// Backspace on the list: selects the parent folder.
    pub(super) fn select_parent_row(&mut self) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
            m.select_parent(t);
            let s = m.scroll_to_selected(self.slot.scroll, self.layout.rows_visible());
            self.set_scroll(s);
        }
    }

    /// F5: no tree (never scanned, cancelled, failed) scans the shown view; a tree rescans it in the background; a scan in
    /// flight, or no view shown, does nothing. In the Processes view a rescan only wakes the sampler.
    pub(super) fn refresh_key(&mut self) -> Effect {
        match self.active {
            Some(i) if !self.cancelable() => if self.slot.tree.is_none() { Effect::Scan(i) } else { Effect::Rescan(i) },
            _ => Effect::None,
        }
    }

    /// Shift+F10 / the Apps key: scrolls the focus row into view and asks for its menu.
    pub(super) fn context_menu_key(&mut self) -> Effect {
        let vis = self.layout.rows_visible();
        let Some(m) = &self.slot.model else { return Effect::None };
        let (Some(id), Some(_)) = (m.selected(), m.selected_index()) else { return Effect::None };
        let s = m.scroll_to_selected(self.slot.scroll, vis);
        self.set_scroll(s);
        Effect::Menu(id)
    }

    /// Where the focus row is on screen (client DIPs), if it is.
    pub fn focus_row_rect(&self) -> Option<Rect> {
        let i = self.slot.model.as_ref()?.selected_index()?.checked_sub(self.slot.scroll)?;
        (i < self.layout.rows_visible()).then(|| self.layout.row(i))
    }
```

`Rect` is imported in `app.rs` (`layout::{.., Rect, ..}`) so `super::*` carries it.

- [ ] **Step 3: Run**

Run: `cargo test -p plumb select` then `cargo test -p plumb`
Expected: PASS. (`f5_scans...` relies on `fail_scan` clearing the tree: `app.rs:636`.)

- [ ] **Step 4: Commit**

```bash
git add crates
git commit -m "Keys: Enter opens or closes a folder, Backspace goes to the parent, F5 scans again, a key can open the row menu" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 7: The context menu over a set

**Files:**
- Modify: `crates/plumb/src/app.rs` (`RowMenu`, `row_menu`, new `set_menu`; tests)
- Modify: `crates/plumb/src/window/menu.rs` (`context_menu`)

**Interfaces:**
- Consumes: `App::{topmost, fs_path, mark_state, mark_selected}` (T5).
- Produces: `RowMenu { raw, paths: Vec<String>, count: usize, mark, who, end, pause, suspend_tree, resume_tree }`. `paths` is empty when unknown; one entry for a single row; the non-empty paths of all picked rows for a set. `count > 1` only for a set.

- [ ] **Step 1: Write the failing tests**

In `select.rs` `mod tests`:

```rust
    #[test]
    fn the_menu_of_a_picked_set_is_the_set_form_and_a_single_row_keeps_today_s() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        let id = a.slot.model.as_ref().unwrap().rows()[notes].id;
        let set = a.row_menu(id).unwrap();
        assert_eq!((set.count, set.paths.len(), set.raw.as_str()), (2, 2, ""));
        assert_eq!(set.mark, MarkState::Mark);
        assert!(!set.who && set.end.is_none() && set.pause.is_none());
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.row_menu(id).unwrap().mark, MarkState::Unmark, "every topmost row is marked");
        press(&mut a, notes, NONE);
        let one = a.row_menu(id).unwrap();
        assert_eq!((one.count, one.paths.len()), (1, 1));
        assert_eq!(one.raw, a.slot.tree.as_ref().unwrap().path(id));
    }

    #[test]
    fn a_set_with_nothing_markable_shows_the_first_reason() {
        let mut a = app_h(600.0);
        press(&mut a, 0, NONE);
        press(&mut a, 1, SHIFT); // the drive root and src
        let id = a.slot.model.as_ref().unwrap().rows()[0].id;
        assert_eq!(a.row_menu(id).unwrap().mark, MarkState::Refused("it is a drive root"), "the root covers src, so only the root is topmost");
    }
```

And in `app.rs`'s `mod tests` (it owns `process_app`), next to `a_process_row_names_its_pid_and_its_menu_acts_on_its_executable`:

```rust
    #[test]
    fn a_set_of_processes_has_nothing_to_copy_and_cannot_be_marked() {
        let (mut a, id) = process_app();
        let root = a.slot.model.as_ref().unwrap().rows()[0].id;
        {
            let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
            m.expand(t, root);
            m.select(root);
            m.extend_to(id);
        }
        let menu = a.row_menu(root).unwrap();
        assert_eq!(menu.count, 2);
        assert!(menu.paths.is_empty(), "no executable path is known: no Copy paths entry");
        assert_eq!(menu.mark, MarkState::Refused("it is a process"));
        assert_eq!(a.copy_text(), None);
    }
```

Run: `cargo test -p plumb`
Expected: FAIL to compile (`count`, `paths` missing).

- [ ] **Step 2: `RowMenu` and `row_menu` in `app.rs`**

Replace the struct's `path` field and add `count`:

```rust
pub struct RowMenu {
    /// The path in the tree's terms, to mark by; empty for a set (marking goes through `App::mark_selected`).
    pub raw: String,
    /// The file system paths to reveal and copy, the ones that are known: one for a row, those of every picked row for a
    /// set.
    pub paths: Vec<String>,
    /// How many rows the menu acts on: more than one only for a picked set.
    pub count: usize,
    pub mark: MarkState,
    ... (the rest unchanged)
}
```

Replace `row_menu` with:

```rust
    /// Everything the context menu of `id` offers: for a row inside a picked set of more than one, the set's menu.
    pub fn row_menu(&self, id: NodeId) -> Option<RowMenu> {
        if let Some(m) = &self.slot.model
            && m.is_picked(id)
            && m.picked_count() > 1
        {
            return Some(self.set_menu());
        }
        let (raw, path, mark, who) = self.menu_target(id)?;
        let (suspend_tree, resume_tree) = self.pause_trees(id);
        let paths = if path.is_empty() { Vec::new() } else { vec![path] };
        Some(RowMenu { raw, paths, count: 1, mark, who, end: self.tree_targets(id), pause: self.pause_target(id), suspend_tree, resume_tree })
    }

    /// The menu of a picked set: copy the paths, and mark or unmark the topmost rows together. Open, Reveal, Who is using
    /// this, Properties and the process commands stay single-row.
    fn set_menu(&self) -> RowMenu {
        let ids = self.slot.model.as_ref().map(|m| m.targets()).unwrap_or_default();
        let paths = ids.iter().map(|&i| self.fs_path(i)).filter(|p| !p.is_empty()).collect();
        let states: Vec<MarkState> = self.topmost().into_iter().map(|i| self.mark_state(i)).collect();
        let mark = if states.iter().all(|s| *s == MarkState::Unmark) {
            MarkState::Unmark
        } else if states.iter().any(|s| *s == MarkState::Mark) {
            MarkState::Mark
        } else {
            states.iter().find_map(|s| if let MarkState::Refused(why) = s { Some(MarkState::Refused(why)) } else { None }).unwrap_or(MarkState::Refused("nothing to mark"))
        };
        RowMenu { raw: String::new(), paths, count: ids.len(), mark, who: false, end: None, pause: None, suspend_tree: Vec::new(), resume_tree: Vec::new() }
    }
```

`topmost` is `pub(super)` in `select.rs`; `app.rs` is the parent so it can call it.

- [ ] **Step 3: The window side, `window/menu.rs`**

Change the destructure and build:

```rust
    let RowMenu { raw, paths, count, mark, who, end, pause, suspend_tree, resume_tree } = menu;
    let path = paths.first().cloned().unwrap_or_default(); // the single row's, when count is 1
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let files = count == 1 && !path.is_empty(); // a process whose executable is unknown has none
        let copy_set = count > 1 && !paths.is_empty();
        if files {
            (the three existing AppendMenuW lines for Reveal, Copy path, Properties)
        }
        if copy_set {
            let text = wide(&format!("Copy paths ({count})"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_COPY, PCWSTR(text.as_ptr()));
        }
```

`who`, `end`, `pause`, `suspend_tree`, `resume_tree` are already empty or false for a set, so their blocks need no change. The separator line becomes `if files || copy_set || end.is_some() || who {`. The mark entries get set wording:

```rust
        match (&mark, count > 1) {
            (MarkState::Mark, false) => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark for deletion\tDel")); }
            (MarkState::Mark, true) => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark selected\tDel")); }
            (MarkState::Unmark, false) => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark\tDel")); }
            (MarkState::Unmark, true) => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark selected\tDel")); }
            (MarkState::Refused(why), _) => {
                let text = wide(&format!("Cannot mark: {why}"));
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, CMD_MARK, PCWSTR(text.as_ptr()));
            }
        }
```

Commands: `CMD_COPY => copy_to_clipboard(hwnd, &paths.join("\r\n")),` (for one row `paths` has the one path, as before), and

```rust
            CMD_MARK => {
                with(|w| {
                    if count > 1 {
                        w.app.mark_selected();
                    } else {
                        w.app.toggle_mark_path(&raw);
                    }
                    w.invalidate();
                });
            }
```

`CMD_REVEAL`, `CMD_PROPS`, `CMD_WHO` keep using `path` (only reachable for count 1).

- [ ] **Step 4: Run and commit**

Run: `cargo test -p plumb select`, `cargo test -p plumb`, `cargo build -p plumb`
Expected: PASS; the `RowMenu` pattern in `window.rs`'s imports still compiles.

```bash
git add crates
git commit -m "Context menu over a picked set: Mark or Unmark selected, Copy paths" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Show the set: row highlight and the status line

**Files:**
- Modify: `crates/plumb/src/draw.rs` (`rows`, new `row_look`, test)
- Modify: `crates/plumb/src/app.rs` (`write_selection`; tests)

**Interfaces:**
- Consumes: `RowModel::{is_picked, selected, picked_count}`.
- Produces: `draw::row_look(&RowModel, NodeId, hover: bool) -> (RowFill, bool)` (private to `draw`); `write_selection` prefixed with `"{n} selected   |   "` when `picked_count() > 1`, and silent when nothing is picked.

**Ruling:** the spec's "render test" is a unit test of `row_look`, the pure function that decides each row's fill and bar; there is no Direct2D read-back harness for the list besides the GUI selftests, and those stay as they are.

- [ ] **Step 1: Write the failing tests**

In `draw.rs` `mod tests`:

```rust
    #[test]
    fn picked_rows_take_the_select_fill_and_only_the_focus_has_the_bar() {
        use plumb_core::{synth::flat_tree, rows::RowModel};
        let t = flat_tree(6);
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        let r: Vec<NodeId> = m.rows().iter().map(|r| r.id).collect();
        m.select(r[1]);
        m.extend_to(r[3]); // 1..=3 picked, focus 3
        assert_eq!(row_look(&m, r[1], false), (RowFill::Select, false));
        assert_eq!(row_look(&m, r[3], false), (RowFill::Select, true));
        assert_eq!(row_look(&m, r[4], true), (RowFill::Hover, false));
        assert_eq!(row_look(&m, r[4], false), (RowFill::None, false));
        m.toggle_pick(r[3]); // focus stays on 3, which is no longer picked
        assert_eq!(row_look(&m, r[3], false), (RowFill::None, true), "the cursor shows without a fill");
    }
```

In `app.rs` `mod tests`:

```rust
    #[test]
    fn the_status_line_counts_a_set_and_says_nothing_when_nothing_is_picked() {
        let mut a = scanned();
        {
            let m = a.slot.model.as_mut().unwrap();
            let r: Vec<NodeId> = m.rows().iter().map(|r| r.id).collect();
            m.select(r[0]);
            m.toggle_pick(r[0]);
        }
        assert_eq!(a.selection(true), None, "focus without a pick");
        a.handle(Input::Edit(Edit::SelectAll));
        let line = a.selection(true).unwrap();
        let n = a.slot.model.as_ref().unwrap().picked_count();
        assert!(n > 1 && line.starts_with(&format!("{n} selected   |   ")), "{line:?}");
    }
```

Run: `cargo test -p plumb`
Expected: FAIL to compile (`row_look`, `RowFill` missing).

- [ ] **Step 2: Implement**

In `draw.rs`, above `impl Painter`'s `rows` (module level, next to other small helpers):

```rust
/// How a row shows the selection: picked rows take the select fill (else the hover tint, else none), and the accent bar marks the focus row
/// (the cursor) whether or not it is picked.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RowFill {
    Select,
    Hover,
    None,
}

fn row_look(model: &RowModel, id: NodeId, hover: bool) -> (RowFill, bool) {
    let fill = if model.is_picked(id) {
        RowFill::Select
    } else if hover {
        RowFill::Hover
    } else {
        RowFill::None
    };
    (fill, model.selected() == Some(id))
}
```

In `fn rows`, replace the five lines `if v.model.selected() == Some(id) { ... } else if v.hover == Some(i) { ... }` with:

```rust
            let (fill, bar) = row_look(v.model, id, v.hover == Some(i));
            match fill {
                RowFill::Select => self.fill(r, t.select),
                RowFill::Hover => self.fill(r, t.hover),
                RowFill::None => {}
            }
            if bar {
                self.fill(Rect { x: r.x, y: r.y, w: 3.0, h: r.h - 1.0 }, t.accent);
            }
```

(`NodeId` and `RowModel` are already imported in `draw.rs`; if the test module cannot see `NodeId`, add `use plumb_core::NodeId;` inside it.)

In `app.rs` replace `write_selection`:

```rust
    /// Writes [`App::selection`]; false (writing nothing) when no row is picked. A set leads with how many rows are in it.
    fn write_selection(&self, full: bool, out: &mut String) -> bool {
        let (Some(tree), Some(m)) = (self.slot.tree.as_ref(), self.slot.model.as_ref()) else { return false };
        let (Some(id), n) = (m.selected(), m.picked_count()) else { return false };
        if n == 0 {
            return false;
        }
        if n > 1 {
            let _ = write!(out, "{n} selected   |   ");
        }
        self.describe_sized(tree, id, tree.subtree_size[id]).write_line(full, out);
        true
    }
```

(`std::fmt::Write as _` is already imported in `app.rs`.)

- [ ] **Step 3: Run and commit**

Run: `cargo test -p plumb`
Expected: PASS, including the existing status-line tests (`status_tail` strips `selection(true)`, which is unchanged for a single pick).

```bash
git add crates
git commit -m "Picked rows are highlighted, the focus keeps the bar, and the status line counts a set" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Window wiring: keys, drag timer, key-opened menu

**Files:**
- Modify: `crates/plumb/src/window.rs`

**Interfaces:**
- Consumes: `Input::{Refresh, ContextMenu}`, `App::{row_dragging, focus_row_rect, copy_text}`, `Mods`, `mods_now` (T3).
- Produces: nothing new for other tasks.

There is no automated test for this task (it is Win32 message handling). T10's selftest covers the app side; this task is verified by reading and by the manual check in Step 6.

- [ ] **Step 1: Constants and the timer slot**

Near the other timers add:

```rust
const TIMER_DRAG: usize = 7;
/// While a drag over rows is held: how often the pointer's position is replayed, so a pointer resting past an edge keeps scrolling.
const DRAG_TICK_MS: u32 = 40;
```

`timers: [bool; 4]` (struct field and `Win::new`) becomes `[bool; 5]`; update the doc comment to "progress, preview, live, flash and drag-scroll timers". In `sync_timers`'s `want` array append `(TIMER_DRAG, self.app.row_dragging(), DRAG_TICK_MS),` and fix the doc comment.

- [ ] **Step 2: Keep the timers in step with every input**

In `dispatch` and `send`, call `w.sync_timers();` right after `let e = w.app.handle(...)` (before `w.invalidate()`). `sync_timers` returns at once while the window state is borrowed.

- [ ] **Step 3: Double-click with a modifier, and the timer handler**

`WM_LBUTTONDBLCLK` becomes:

```rust
        WM_LBUTTONDBLCLK => {
            unsafe { SetCapture(hwnd) };
            let mods = mods_now();
            if mods.shift || mods.ctrl {
                // Two quick presses with Shift or Ctrl held are two selections; they must not open the folder.
                dispatch(hwnd, lp, |x, y| Input::Click(x, y, mods));
            } else {
                dispatch(hwnd, lp, Input::DoubleClick);
            }
            with(|w| w.filter_press(lp, true));
            LRESULT(0)
        }
```

Add beside the other `WM_TIMER` arms:

```rust
        WM_TIMER if wp.0 == TIMER_DRAG => {
            // The button is down on a row: replay the pointer's position, so holding it past an edge keeps scrolling. A
            // button let go after the window lost the capture sends no WM_LBUTTONUP, so end the drag here.
            if unsafe { GetKeyState(VK_LBUTTON.0 as i32) } < 0 {
                let mut p = POINT::default();
                if unsafe { GetCursorPos(&mut p).is_ok() && ScreenToClient(hwnd, &mut p).as_bool() } {
                    if let Some((x, y)) = with(|w| (p.x as f32 * 96.0 / w.dpi, p.y as f32 * 96.0 / w.dpi - w.inset)) {
                        send(hwnd, Input::Drag(x, y));
                    }
                }
            } else {
                send(hwnd, Input::Release);
            }
            LRESULT(0)
        }
```

- [ ] **Step 4: Keys**

In `key_input`'s match add `x if x == VK_F5.0 => Input::Refresh,` before the final arm. (Ctrl+A already maps to `Edit::SelectAll`, Backspace to `Edit::Backspace`, Enter to `Input::Enter`.)

Add:

```rust
/// Keys that act once per press: a held key must not flip a mark or a folder, or queue a rescan, on every repeat.
fn once_per_press(vk: u16, ctrl: bool) -> bool {
    [VK_DELETE.0, VK_RETURN.0, VK_F5.0, VK_BACK.0].contains(&vk) || (ctrl && vk == b'A' as u16)
}

/// Opens the context menu of the focus row from the keyboard (Apps key, Shift+F10), at that row's left edge.
fn key_menu(hwnd: HWND) {
    let Some((e, at)) = with(|w| {
        w.last_input = Instant::now();
        let e = w.app.handle(Input::ContextMenu);
        let at = w.app.focus_row_rect().map_or((0, 0), |r| {
            let k = w.dpi / 96.0;
            ((r.x * k) as i32, ((r.y + r.h + w.inset) * k) as i32)
        });
        w.invalidate();
        (e, at)
    }) else {
        return;
    };
    effect(hwnd, e, at);
}
```

In `WM_KEYDOWN`, the Del repeat branch becomes

```rust
            } else if lp.0 & (1 << 30) != 0 && !in_box && once_per_press(wp.0 as u16, ctrl) {
                // A held Del, Enter, F5, Backspace or Ctrl+A acts once, not on every repeat.
            } else if wp.0 == VK_APPS.0 as usize {
                if lp.0 & (1 << 30) == 0 {
                    key_menu(hwnd);
                }
            } else if let Some(input) = key_input(wp.0 as u16, Mods { shift, ctrl }, hwnd) {
```

Add the system-key arm right after `WM_KEYDOWN`'s arm:

```rust
        // F10 is a system key, so Shift+F10, the keyboard's context menu, arrives here. Every other Alt or F10 combination is
        // Windows': it falls through to `DefWindowProc`.
        WM_SYSKEYDOWN if wp.0 == VK_F10.0 as usize && unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 => {
            if lp.0 & (1 << 30) == 0 {
                key_menu(hwnd);
            }
            LRESULT(0)
        }
```

- [ ] **Step 5: Build**

Run: `cargo build -p plumb` and `cargo test -p plumb`
Expected: builds clean with no new warnings (`cargo build -p plumb 2>&1 | grep -c warning` equals its count before this task); tests PASS. If `VK_F5`, `VK_F10`, `VK_APPS`, `VK_LBUTTON` or `WM_SYSKEYDOWN` are not in scope, they come from `windows::Win32::UI::Input::KeyboardAndMouse` and `...WindowsAndMessaging`, both already imported with `*` in `window.rs`; check the existing `use` lines before adding any.

- [ ] **Step 6: Manual check, then commit**

Run the app (`cargo run -p plumb`, scan a folder) and check, reading the result yourself: Shift+click and Shift+Down pick a range; Ctrl+click toggles; dragging over rows picks them and holding the pointer below the list scrolls; F5 rescans; Enter opens/closes; Backspace goes up; Shift+F10 and the Apps key open the menu at the row; Ctrl+A then Ctrl+C then paste elsewhere shows all paths. Close the app. (If the user is away, skip the manual part, say so in the closing report, and rely on T10's selftest plus the unit tests.)

```bash
git add crates
git commit -m "Window: modifiers on clicks and keys, F5, Shift+F10 and the Apps key, drag-scroll timer, one-shot keys" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Selftest, docs, full verification, review

**Files:**
- Modify: `crates/plumb/src/window/selftest.rs` (`selftest`)
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-11-08-multi-select-design.md`, this plan (front matter `status: done`)

**Interfaces:** none.

- [ ] **Step 1: The Shift-extend check**

In `selftest()`, immediately before the `// The filter, end to end:` comment, insert:

```rust
    // Selecting several rows, through the real window state: open every folder, then Shift+Down twice from the first row.
    let picked = with(|w| {
        {
            let (Some(t), Some(m)) = (w.app.slot.tree.as_ref(), w.app.slot.model.as_mut()) else { return None };
            for _ in 0..2 {
                let open = m.rows().iter().map(|r| r.id).find(|&id| m.is_expandable(t, id) && !m.is_expanded(id));
                if let Some(id) = open {
                    m.expand(t, id);
                }
            }
            let first = m.rows().first()?.id;
            m.select(first);
        }
        let shift = Mods { shift: true, ctrl: false };
        w.app.handle(Input::Key(Key::Down, shift));
        w.app.handle(Input::Key(Key::Down, shift));
        w.app.slot.model.as_ref().map(|m| m.picked_count())
    })
    .flatten();
    if picked != Some(3) {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, format!("Shift+Down twice from the first row should pick 3 rows, got {picked:?}")));
    }
```

Update the summary `format!` at the end to append `; shift-extend ok`.

- [ ] **Step 2: Run the selftest and read it**

Run: `cargo build -p plumb` then `target/debug/plumb.exe --selftest` directly in the terminal (do not redirect its stdout to a file).
Expected: a line starting `selftest ok:` and ending `shift-extend ok` (or whatever the format ends with). The check ends the run before the filter section, so the `filter ok: 3 rows` part must still be present. Then run `--selftest-delete` and `--selftest-tooltip` the same way; both must exit 0 (they were passing before this work).

- [ ] **Step 3: Docs**

Add to `README.md`'s feature list, after the "Delete from the tree or the map" bullet:

```markdown
- **Select several rows:** Shift+click, Ctrl+click, Shift+arrows, Ctrl+A, or drag over rows; `Del`, Ctrl+C and the context menu then act on all of them. F5 rescans, Enter opens or closes a folder, Backspace goes to the parent, and Shift+F10 or the Apps key opens the menu of the focused row.
```

Grep for stale wording and fix what is found: `grep -rn "toggle_mark_selected\|selected_path\|RowMenu\b.*path\|Click(f32, f32)" --include=*.md --include=*.rs .` (excluding `target/` and this plan and the spec); `grep -n "select" PARKED.md ROADMAP.md docs/*.md` for any line that says single selection is all there is. Do not edit `ROADMAP.md` (no v0.12 line yet) or `CHANGELOG.md`.

Set `status: done` in this plan's and the spec's front matter. If anything was left undone, it goes into `PARKED.md` first.

- [ ] **Step 4: Full verification through the wrapper**

Run (from bash; one command):

```bash
powershell -NoProfile -File "$USERPROFILE/.claude/runs/run.ps1" -Kind gate -Name multiselect-full -Command "cargo test --workspace"
```

Expected: exit 0. Read the headline in the printed run record and `docs/runs.md` row. Check no stray `cargo`/`plumb` processes remain (`Get-Process cargo,plumb -ErrorAction SilentlyContinue`).

- [ ] **Step 5: Final whole-branch review**

Dispatch one reviewer with `model: opus` (the branch is well over 300 changed lines) on `git diff <T1-parent>..HEAD`, briefed with the spec, this plan's Review Focus and the rulings (`select_parent` takes `&Tree`; the render test is `row_look`). Fix Critical/Important findings test-first; record minors in the closing report.

- [ ] **Step 6: Commit**

```bash
git add -A docs README.md crates
git commit -m "Selftest: Shift+Down extends the selection; README lists the new keys; multi-select spec and plan done" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

Do not push, tag or bump. The closing report carries the Architecture, Docs and Review checks, and counts parked entries (none added by this work).

---

## Self-review

**Spec coverage.** Understanding/assumptions: hidden picks never act (T2 prune), drag starts on a row (T4). Model table: `select`, `extend_to`, `toggle_pick`, `select_all`, `targets`, `picked_count`, `is_picked`, `handle_key`, `select_parent` (T2); collapse/rebuild/filter/refresh invariants (T2); rescan dropping the set is `Snapshot` restoring only the focus, unchanged code (spec states it; no task). Input: `Mods`, `Click`/`Key` shapes (T3); gesture table rows: click/Shift/Ctrl/icon/drag/Shift+arrows/Ctrl+A/right-click (T4), Del/Ctrl+C (T5), Enter/Backspace/F5/Shift+F10 (T6, T9); `DoubleClick` with modifiers, repeat guard, `WM_SYSKEYDOWN`, drag timer (T9). Set mark and copy (T5). Menu over a set (T7). Drawing and status (T8). Parked (no task; entries exist). Checks: core (T2), app (T4-T7), render (T8, as `row_look`), GUI selftest (T10), fast loop (Global Constraints).

**Placeholder scan.** No TBD/TODO. Three steps tell the implementer to read one nearby item before wiring (`status_str` visibility in T5, `filter_ui` accessor name in T6, the `edit_for_key` Down mapping in T4); each names the fallback.

**Type consistency.** `Mods` fields `shift`/`ctrl` everywhere; `row_menu` fields `paths`/`count` in T7 match `menu.rs`; `mark_selected` is `pub` (called from `menu.rs` through `w.app`); `topmost` is `pub(super)` in `select.rs` and called from `app.rs` (parent) in `set_menu`; `fs_path` defined T5 and used T5/T7; `focus_row_rect` T6 and used T9; `row_dragging` T4 and used T9.

**Not tested automatically, by decision:** the drag timer and the Win32 key routing (T9); the held-pane "again" rule for a Shift-click on the focus row (it is structural: `again` is only computed in the plain branch of `click_row`); a rescan or view switch dropping the set (it is `Snapshot`, unchanged code). Review Focus item 3 is tested at the model level (T2), not through `Snapshot`.
