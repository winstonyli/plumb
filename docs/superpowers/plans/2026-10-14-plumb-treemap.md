---
status: done
---

# plumb treemap pane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A split pane beside the tree that draws the selected folder's subtree as a squarified treemap coloured by file type, sharing selection with the tree.

**Architecture:** Pure layout, classification and hit testing in a new `plumb_core::treemap` module (tiles in parent-before-child order). `plumb` gains a pane in `Layout`, a small block of map state in `App` that follows the tree's selection through one hook (`follow_selection`, run at the start of `update_map`, which runs before every paint), painting in `draw.rs` (direct Direct2D first), and `[map]` in `state.toml`.

**Tech Stack:** Rust 2024, `windows` crate (Direct2D/DirectWrite), serde/toml, criterion.

**Spec:** `docs/superpowers/specs/2026-10-14-plumb-treemap-design.md` (read it first; this plan implements it, deviations are listed at the end under "Deviations from the spec").

## Global Constraints

- Layout, colour classes and hit testing are pure code in `plumb-core` (no Win32 types, no `windows` crate).
- `MIN_AREA` is 9 physical px²; a folder tile with inner area under 64 physical px² is a `Solid` tile; nesting is capped at 24 levels. The caller passes one number, `px` = DIPs per physical pixel (`96.0 / dpi`); the module derives the three limits from it.
- Tiles are stored parent before child, so `hit` is a reverse scan returning the deepest tile.
- Children are ordered by `subtree_size` descending, ties by `NodeId`; layout of an unchanged tree is bit-identical.
- Map root = selected node if it is a folder, else its parent, else the tree's first root. A single click on the map never changes the root; a double-click on a folder tile does.
- Map ignores the name filter. Selection and hover outlines are drawn over the map, never baked in.
- Pane minimum width 160 DIPs; the tree keeps `MIN_NAME_W + Σ col_w + SCROLLBAR_W`; below tree minimum + divider + 160 the pane is hidden without changing the saved flag.
- `state.toml` gains `[map] visible = true`, `width = 420.0`, both `#[serde(default)]`; damaged values mean defaults.
- Idle cost is none: no timers; the layout is rebuilt only when dirty (tree changes already paced by the existing `Pacer`s, selection, root, size, DPI).
- Windows paths, CRLF files (git LF→CRLF warnings are harmless). Run cargo from the repo root `C:\Users\Winston Li\Documents\GitHub\plumb` with `$env:CARGO_TARGET_DIR="$env:TEMP\plumb-target"` and `-j 8` (LONG_RUNS.md: leave cores free). Change files with the Edit tool; no python reads stdin.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/plumb-core/src/rect.rs` | create | `Rect` (moved from `plumb::layout`), so core and UI share one rectangle type |
| `crates/plumb-core/src/treemap.rs` | create | `Class`, `classify`, `Tile`, `Kind`, `layout`, `hit`, `root_for` |
| `crates/plumb-core/src/treemap/tests.rs` | create | invariants and tables |
| `crates/plumb-core/src/lib.rs` | modify | `pub mod rect; pub mod treemap; pub use rect::Rect;` |
| `crates/plumb-core/src/rows.rs` | modify | `RowModel::reveal` |
| `crates/plumb-core/benches/core.rs` | modify | treemap benchmarks |
| `crates/plumb/src/layout.rs` | modify | re-export `Rect`; pane, divider, legend, map button, `tree_w`, hits |
| `crates/plumb/src/state.rs` | modify | `MapState`, `State.map` |
| `crates/plumb/src/app.rs` | modify | map state, `follow_selection`, `update_map`, clicks, drag, hover, tip, toggle |
| `crates/plumb/src/draw.rs` | modify | `Theme.class`, `MapView`, `Pen::map`, `toggle_button` |
| `crates/plumb/src/window.rs` | modify | Ctrl+M, `update_map` before paint, hover/leave repaint |
| `crates/plumb/tests/render.rs` | modify | `render_scene` helper, map pixel test |
| `CHANGELOG.md`, spec, disk-design spec | modify | docs |

---

### Task 1: Move `Rect` into `plumb-core`

**Files:**
- Create: `crates/plumb-core/src/rect.rs`
- Modify: `crates/plumb-core/src/lib.rs`, `crates/plumb/src/layout.rs:3-15`

**Interfaces:**
- Produces: `plumb_core::Rect { x, y, w, h: f32 }` with `Clone, Copy, Debug, PartialEq, Default` and `contains(&self, x: f32, y: f32) -> bool` (half-open). `plumb::layout::Rect` stays valid as a re-export, so no other file changes.

- [ ] **Step 1: Create `rect.rs`**

```rust
//! The rectangle shared by the treemap layout and the window layout, in whatever unit the caller uses.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Half-open: the left and top edges are inside, the right and bottom edges are not.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 };
        assert!(r.contains(1.0, 2.0) && r.contains(3.9, 5.9));
        assert!(!r.contains(4.0, 3.0) && !r.contains(2.0, 6.0) && !r.contains(0.9, 3.0));
    }
}
```

- [ ] **Step 2: Wire it up.** In `lib.rs` add `pub mod rect;` after `pub mod node;` and `pub use rect::Rect;` after the `pub use node::...` line. In `layout.rs` replace the `Rect` struct and its `impl` (lines 3-15) with `pub use plumb_core::Rect;`.

- [ ] **Step 3: Run** `cargo test --workspace -j 8` — Expected: all existing tests pass plus `contains_is_half_open`.

- [ ] **Step 4: Commit**

```bash
git add crates/plumb-core/src/rect.rs crates/plumb-core/src/lib.rs crates/plumb/src/layout.rs
git commit -m "Move Rect into plumb-core so the treemap and the window layout share it"
```

---

### Task 2: `Class` and `classify`

**Files:**
- Create: `crates/plumb-core/src/treemap.rs` (this task writes the classification half), `crates/plumb-core/src/treemap/tests.rs`
- Modify: `crates/plumb-core/src/lib.rs` (`pub mod treemap;`)

**Interfaces:**
- Produces: `treemap::Class` (`Video, Audio, Image, Archive, Document, Code, Executable, DiskImage, Database, Other`), `Class::ALL: [Class; 10]`, `Class::index(self) -> usize` (position in `ALL`), `Class::label(self) -> &'static str`, `treemap::classify(name: &[u16]) -> Class`.

- [ ] **Step 1: Failing test.** Create `treemap/tests.rs`:

```rust
use super::*;

fn class_of(name: &str) -> Class {
    classify(&name.encode_utf16().collect::<Vec<u16>>())
}

#[test]
fn classification_by_last_extension_ignores_case_and_defaults_to_other() {
    for (name, want) in [
        ("movie.MP4", Class::Video),
        ("song.flac", Class::Audio),
        ("IMG_1.JpG", Class::Image),
        ("a.tar.gz", Class::Archive),
        ("report.docx", Class::Document),
        ("lib.rs", Class::Code),
        ("setup.exe", Class::Executable),
        ("disk.vhdx", Class::DiskImage),
        ("data.sqlite3", Class::Database),
        ("noext", Class::Other),
        ("trailing.", Class::Other),
        (".hidden", Class::Other),
        ("x.y.", Class::Other),
        ("weird.\u{fc}n\u{ef}", Class::Other),
        ("a.verylongextension", Class::Other),
    ] {
        assert_eq!(class_of(name), want, "{name}");
    }
}

#[test]
fn all_lists_every_class_in_index_order() {
    for (i, c) in Class::ALL.iter().enumerate() {
        assert_eq!(c.index(), i);
        assert!(!c.label().is_empty());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p plumb-core treemap -j 8` — Expected: FAIL (module missing). Add `pub mod treemap;` to `lib.rs` after `pub mod tree;`... it must come in alphabetical position; after `synth`/before `tree` is fine.

- [ ] **Step 3: Implement.** Create `treemap.rs`:

```rust
//! Squarified treemap of a folder's subtree: pure layout, colour classes and hit testing.
//! No UI types, so it tests anywhere; `plumb` paints the tiles.

/// What kind of file a tile stands for, by extension. Colours are chosen by the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Video,
    Audio,
    Image,
    Archive,
    Document,
    Code,
    Executable,
    DiskImage,
    /// Databases and logs.
    Database,
    Other,
}

impl Class {
    pub const ALL: [Class; 10] = [
        Class::Video, Class::Audio, Class::Image, Class::Archive, Class::Document,
        Class::Code, Class::Executable, Class::DiskImage, Class::Database, Class::Other,
    ];

    /// Position in `ALL` (and in a theme's colour table).
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        ["Video", "Audio", "Images", "Archives", "Documents", "Code", "Programs", "Disk images", "Databases and logs", "Other"][self.index()]
    }
}

/// The class of a file named `name` (UTF-16, as the tree stores it): by the text after the last dot,
/// ASCII case-insensitively. No extension, an empty one or a non-ASCII one is `Other`.
pub fn classify(name: &[u16]) -> Class {
    let Some(dot) = name.iter().rposition(|&u| u == u16::from(b'.')) else { return Class::Other };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 8 {
        return Class::Other;
    }
    let mut buf = [0u8; 8];
    for (b, &u) in buf.iter_mut().zip(ext) {
        if u >= 0x80 {
            return Class::Other;
        }
        *b = (u as u8).to_ascii_lowercase();
    }
    match std::str::from_utf8(&buf[..ext.len()]).unwrap_or("") {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "m2ts" => Class::Video,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "wma" | "opus" => Class::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "avif" | "raw" | "cr2" | "nef" | "psd" | "svg" => Class::Image,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" => Class::Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "csv" | "epub" => Class::Document,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "js" | "ts" | "jsx" | "tsx" | "py" | "java" | "go" | "rb" | "php" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "xml" | "sh" | "ps1" | "bat" => Class::Code,
        "exe" | "dll" | "sys" | "msi" => Class::Executable,
        "vhd" | "vhdx" | "iso" | "vmdk" | "wim" => Class::DiskImage,
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "mdf" | "ldf" | "log" | "etl" => Class::Database,
        _ => Class::Other,
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run** `cargo test -p plumb-core treemap -j 8` — Expected: 2 passed.
- [ ] **Step 5: Commit** (`git add crates/plumb-core/src/treemap.rs crates/plumb-core/src/treemap crates/plumb-core/src/lib.rs`, message "Treemap: classify files into ten colour classes").

---

### Task 3: `layout`, `hit`, `root_for`

**Files:**
- Modify: `crates/plumb-core/src/treemap.rs` (append above `#[cfg(test)]`), `crates/plumb-core/src/treemap/tests.rs` (append)

**Interfaces:**
- Consumes: `Rect`, `Tree` (`children`, `subtree_size`, `size`, `subtree_files`, `flags`, `name`, `parent`, `roots`, `len`), `NodeFlags::{DIR, DEAD}`.
- Produces:
  - `pub const MAX_DEPTH: u8 = 24;`
  - `pub enum Kind { File, Frame, Solid, Rest }` (`Clone, Copy, Debug, PartialEq, Eq`)
  - `pub struct Tile { pub node: NodeId, pub rect: Rect, pub depth: u8, pub class: Class, pub kind: Kind, pub bytes: u64, pub count: u32 }` (`Clone, Copy, Debug, PartialEq`). `File`: `bytes` = size, `count` 1. `Frame`/`Solid`: `bytes` = `subtree_size`, `count` = `subtree_files`, `class` = `Other`. `Rest`: `node` = the folder it belongs to, `bytes` = merged bytes, `count` = number of merged items, `class` = `Other`.
  - `pub fn layout(tree: &Tree, root: NodeId, rect: Rect, px: f32) -> Vec<Tile>`: tiles of `root`'s descendants filling `rect` (root itself gets no tile); children of `root` have `depth` 0; empty if `root` is none, dead, out of range, or has no counted bytes; a non-folder `root` gives one `File` tile.
  - `pub fn hit(tiles: &[Tile], x: f32, y: f32) -> Option<usize>`
  - `pub fn root_for(tree: &Tree, selected: Option<NodeId>) -> Option<NodeId>`

- [ ] **Step 1: Failing tests.** Append to `treemap/tests.rs`:

```rust
use crate::node::{NodeFlags, NodeId};
use crate::tree::{Tree, TreeBuilder};

fn file(b: &mut TreeBuilder, p: NodeId, name: &str, size: u64) -> NodeId {
    b.push_str(p, name, size, NodeFlags::empty())
}

fn dir(b: &mut TreeBuilder, p: NodeId, name: &str) -> NodeId {
    b.push_str(p, name, 0, NodeFlags::DIR)
}

/// root: a (a1.rs 400, a2.rs 300, a3.png 100, nest (n1.log 250, n2.db 50)), c.mp4 900, d.zip 500, e.txt 120, zero.bin 0.
fn sample() -> (Tree, NodeId) {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let a = dir(&mut b, root, "a");
    file(&mut b, a, "a1.rs", 400);
    file(&mut b, a, "a2.rs", 300);
    file(&mut b, a, "a3.png", 100);
    let nest = dir(&mut b, a, "nest");
    file(&mut b, nest, "n1.log", 250);
    file(&mut b, nest, "n2.db", 50);
    file(&mut b, root, "c.mp4", 900);
    file(&mut b, root, "d.zip", 500);
    file(&mut b, root, "e.txt", 120);
    file(&mut b, root, "zero.bin", 0);
    (b.finish().unwrap(), root)
}

const R: Rect = Rect { x: 10.0, y: 20.0, w: 400.0, h: 300.0 };

fn area(r: Rect) -> f64 {
    r.w as f64 * r.h as f64
}

fn inside(r: Rect, o: Rect) -> bool {
    let e = 1e-3;
    r.x >= o.x - e && r.y >= o.y - e && r.x + r.w <= o.x + o.w + e && r.y + r.h <= o.y + o.h + e
}

fn overlap(a: Rect, b: Rect) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 0.0 && h > 0.0 { w as f64 * h as f64 } else { 0.0 }
}

/// The tiles directly inside frame `i`.
fn kids(tiles: &[Tile], tree: &Tree, i: usize) -> Vec<usize> {
    let f = tiles[i];
    (i + 1..tiles.len())
        .filter(|&j| {
            let t = tiles[j];
            t.depth == f.depth + 1 && if t.kind == Kind::Rest { t.node == f.node } else { tree.parent[t.node] == f.node }
        })
        .collect()
}

/// A group of sibling tiles fills `region`, stays inside it, does not overlap, and has areas proportional to bytes.
fn check_group(tiles: &[Tile], group: &[usize], region: Rect) {
    assert!(!group.is_empty());
    let sum: f64 = group.iter().map(|&i| area(tiles[i].rect)).sum();
    assert!((sum - area(region)).abs() <= 1.0, "areas {sum} vs region {}", area(region));
    let per_byte = area(tiles[group[0]].rect) / tiles[group[0]].bytes as f64;
    for (n, &i) in group.iter().enumerate() {
        assert!(inside(tiles[i].rect, region), "{:?} outside {region:?}", tiles[i].rect);
        let ratio = area(tiles[i].rect) / tiles[i].bytes as f64;
        assert!((ratio / per_byte - 1.0).abs() < 1e-3, "not proportional: {ratio} vs {per_byte}");
        for &j in &group[n + 1..] {
            assert!(overlap(tiles[i].rect, tiles[j].rect) < 1e-2, "overlap {:?} {:?}", tiles[i].rect, tiles[j].rect);
        }
    }
}

#[test]
fn layout_invariants_hold_at_every_level() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    let top: Vec<usize> = (0..tiles.len()).filter(|&i| tiles[i].depth == 0).collect();
    check_group(&tiles, &top, R);
    let mut frames = 0;
    for i in 0..tiles.len() {
        if tiles[i].kind == Kind::Frame {
            frames += 1;
            let r = tiles[i].rect;
            check_group(&tiles, &kids(&tiles, &tree, i), Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 });
        }
    }
    assert_eq!(frames, 2, "a and nest");
    assert_eq!(tiles.iter().filter(|t| t.kind == Kind::File).count(), 8, "the zero-size file has no tile");
    assert!(tiles.iter().all(|t| t.bytes > 0));
}

#[test]
fn tiles_come_parent_before_child_largest_first_with_classes() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    for (i, t) in tiles.iter().enumerate() {
        if t.depth > 0 {
            let p = tiles[..i].iter().rposition(|p| p.depth + 1 == t.depth).expect("a parent before it");
            assert!(tiles[p].kind == Kind::Frame && inside(t.rect, tiles[p].rect));
        }
    }
    assert_eq!((tiles[0].kind, tree.name_string(tiles[0].node)), (Kind::Frame, "a".to_string()), "a (1100) outweighs c.mp4 (900)");
    let mp4 = tiles.iter().find(|t| tree.name_string(t.node) == "c.mp4").unwrap();
    assert_eq!((mp4.kind, mp4.class, mp4.bytes, mp4.count), (Kind::File, Class::Video, 900, 1));
    let a = tiles[0];
    assert_eq!((a.bytes, a.count, a.class), (1100, 5, Class::Other));
}

#[test]
fn layout_is_deterministic_and_breaks_ties_by_node_id() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let ids: Vec<NodeId> = (0..6).map(|i| file(&mut b, root, &format!("f{i}"), 100)).collect();
    let tree = b.finish().unwrap();
    let (x, y) = (layout(&tree, root, R, 1.0), layout(&tree, root, R, 1.0));
    assert_eq!(x, y);
    assert_eq!(x.iter().map(|t| t.node).collect::<Vec<_>>(), ids);
}

#[test]
fn a_huge_folder_of_tiny_files_becomes_one_rest_tile() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    for _ in 0..200_000 {
        file(&mut b, root, "f", 1);
    }
    let tree = b.finish().unwrap();
    let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    let tiles = layout(&tree, root, r, 1.0);
    assert_eq!(tiles.len(), 1);
    assert_eq!((tiles[0].kind, tiles[0].node, tiles[0].count, tiles[0].bytes, tiles[0].rect), (Kind::Rest, root, 200_000, 200_000, r));
}

#[test]
fn small_siblings_merge_into_a_rest_tile_that_still_tiles_the_folder() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let big = file(&mut b, root, "big.bin", 1_000_000_000);
    for i in 0..3 {
        file(&mut b, root, &format!("s{i}"), 1);
    }
    let tree = b.finish().unwrap();
    let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    let tiles = layout(&tree, root, r, 1.0);
    assert_eq!(tiles.len(), 2);
    assert_eq!((tiles[0].node, tiles[0].kind), (big, Kind::File));
    assert_eq!((tiles[1].kind, tiles[1].count, tiles[1].bytes), (Kind::Rest, 3, 3));
    check_group(&tiles, &[0, 1], r);
}

#[test]
fn a_folder_too_small_to_enter_is_solid() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    file(&mut b, root, "big.bin", 9_950);
    let d = dir(&mut b, root, "small");
    file(&mut b, d, "x", 50);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, 1.0);
    let t = tiles.iter().find(|t| t.node == d).unwrap();
    assert_eq!((t.kind, t.bytes), (Kind::Solid, 50));
    assert_eq!(tiles.len(), 2, "nothing inside it");
}

#[test]
fn nesting_stops_at_the_depth_cap() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let mut cur = root;
    for i in 0..40 {
        cur = dir(&mut b, cur, &format!("d{i}"));
    }
    file(&mut b, cur, "x.bin", 1_000_000);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 2000.0, h: 2000.0 }, 1.0);
    assert_eq!(tiles.iter().map(|t| t.depth).max(), Some(MAX_DEPTH - 1));
    assert_eq!(tiles.last().map(|t| t.kind), Some(Kind::Solid));
}

#[test]
fn nothing_to_draw_gives_no_tiles_and_a_file_root_gives_one() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    file(&mut b, root, "z", 0);
    let f = file(&mut b, NodeId::NONE, "alone.mp4", 5);
    let tree = b.finish().unwrap();
    assert!(layout(&tree, root, R, 1.0).is_empty());
    assert!(layout(&tree, NodeId::NONE, R, 1.0).is_empty());
    assert!(layout(&tree, NodeId(99), R, 1.0).is_empty());
    assert!(layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 0.0, h: 10.0 }, 1.0).is_empty());
    let t = layout(&tree, f, R, 1.0);
    assert_eq!((t.len(), t[0].kind, t[0].class, t[0].rect), (1, Kind::File, Class::Video, R));
}

#[test]
fn hit_finds_the_deepest_tile_and_none_outside() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    let c = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
    for (i, t) in tiles.iter().enumerate().filter(|(_, t)| t.kind == Kind::File) {
        let (x, y) = c(t.rect);
        assert_eq!(hit(&tiles, x, y), Some(i));
    }
    let a = tiles[0].rect;
    assert_eq!(hit(&tiles, a.x + 0.5, a.y + 0.5), Some(0), "the frame's own border belongs to the frame");
    assert_eq!(hit(&tiles, 5.0, 5.0), None);
    assert_eq!(hit(&[], 1.0, 1.0), None);
}

#[test]
fn the_map_root_is_the_selected_folder_else_its_parent_else_the_first_root() {
    let (tree, root) = sample();
    let find = |n: &str| (0..tree.len() as u32).map(NodeId).find(|&i| tree.name_string(i) == n).unwrap();
    assert_eq!(root_for(&tree, None), Some(root));
    assert_eq!(root_for(&tree, Some(find("a"))), Some(find("a")));
    assert_eq!(root_for(&tree, Some(find("a1.rs"))), Some(find("a")));
    assert_eq!(root_for(&tree, Some(find("e.txt"))), Some(root));
    assert_eq!(root_for(&tree, Some(NodeId(999))), Some(root), "a stale selection means the root");
}
```

- [ ] **Step 2: Run** `cargo test -p plumb-core treemap -j 8` — Expected: compile FAIL (`layout`, `Kind`... undefined).

- [ ] **Step 3: Implement.** Add at the top of `treemap.rs` (below the `//!` lines) `use crate::node::{NodeFlags, NodeId}; use crate::rect::Rect; use crate::tree::Tree;` and append before `#[cfg(test)] mod tests;`:

```rust
/// Deepest nesting of folder frames; the tree can be deeper (a stack guard).
pub const MAX_DEPTH: u8 = 24;
/// A child with less area than this many physical pixels is merged into its folder's `Rest` tile.
const MIN_PX2: f64 = 9.0;
/// A folder whose inside (after its frame) has less area than this is drawn `Solid`, not entered.
const SOLID_PX2: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    /// A folder with its children laid out inside.
    Frame,
    /// A folder too small to enter.
    Solid,
    /// The merged small items of one folder.
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// For `Rest`, the folder it belongs to.
    pub node: NodeId,
    pub rect: Rect,
    /// Nesting level: the root's children are 0.
    pub depth: u8,
    pub class: Class,
    pub kind: Kind,
    pub bytes: u64,
    /// Files in the subtree for a folder, 1 for a file, merged items for `Rest`.
    pub count: u32,
}

/// Edges in f64, so tiles that share an edge share the very same number.
#[derive(Clone, Copy)]
struct Box64 {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Box64 {
    fn of(r: Rect) -> Self {
        Self { x0: r.x as f64, y0: r.y as f64, x1: (r.x + r.w) as f64, y1: (r.y + r.h) as f64 }
    }

    fn w(&self) -> f64 {
        self.x1 - self.x0
    }

    fn h(&self) -> f64 {
        self.y1 - self.y0
    }

    fn area(&self) -> f64 {
        self.w() * self.h()
    }

    fn rect(&self) -> Rect {
        Rect { x: self.x0 as f32, y: self.y0 as f32, w: (self.x1 - self.x0) as f32, h: (self.y1 - self.y0) as f32 }
    }
}

/// The tiles of `root`'s descendants filling `rect`, parent before child; `px` is the size of one
/// physical pixel in the caller's units (96 / dpi for DIPs). See the spec for the rules.
pub fn layout(tree: &Tree, root: NodeId, rect: Rect, px: f32) -> Vec<Tile> {
    let mut out = Vec::new();
    if root.is_none() || root.idx() >= tree.len() || tree.flags[root].contains(NodeFlags::DEAD) {
        return out;
    }
    let area = Box64::of(rect);
    if !(area.w() > 0.0 && area.h() > 0.0) {
        return out;
    }
    let ctx = Ctx { tree, px: px as f64 };
    if tree.flags[root].contains(NodeFlags::DIR) {
        ctx.fill(root, area, 0, &mut out);
    } else if tree.size[root] > 0 {
        ctx.emit(root, area, 0, &mut out);
    }
    out
}

struct Ctx<'a> {
    tree: &'a Tree,
    px: f64,
}

impl Ctx<'_> {
    /// Lays out the children of `dir` in `area`.
    fn fill(&self, dir: NodeId, area: Box64, depth: u8, out: &mut Vec<Tile>) {
        let t = self.tree;
        let total = t.subtree_size[dir].saturating_sub(t.size[dir]);
        let a = area.area();
        if total == 0 || !(a > 0.0) {
            return;
        }
        // One O(k) pass: the children big enough to get MIN_PX2 are kept, the rest are summed.
        let min_area = MIN_PX2 * self.px * self.px;
        let mut kept: Vec<(NodeId, u64)> = Vec::new();
        let (mut rest_bytes, mut rest_n) = (0u64, 0u32);
        for c in t.children(dir) {
            let w = t.subtree_size[c];
            if w == 0 {
                continue;
            }
            if w as f64 * a >= total as f64 * min_area {
                kept.push((c, w));
            } else {
                rest_bytes += w;
                rest_n += 1;
            }
        }
        kept.sort_unstable_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        if rest_n > 0 {
            kept.push((NodeId::NONE, rest_bytes)); // NONE stands for the Rest tile, laid out last
        }
        let weights: Vec<f64> = kept.iter().map(|k| k.1 as f64).collect();
        for (&(c, w), b) in kept.iter().zip(squarify(&weights, area)) {
            if c.is_none() {
                out.push(Tile { node: dir, rect: b.rect(), depth, class: Class::Other, kind: Kind::Rest, bytes: w, count: rest_n });
            } else {
                self.emit(c, b, depth, out);
            }
        }
    }

    /// The tile for `c` in `b`, and for a folder big enough, everything inside it.
    fn emit(&self, c: NodeId, b: Box64, depth: u8, out: &mut Vec<Tile>) {
        let t = self.tree;
        let tile = |kind, class, count| Tile { node: c, rect: b.rect(), depth, class, kind, bytes: t.subtree_size[c], count };
        if !t.flags[c].contains(NodeFlags::DIR) {
            out.push(tile(Kind::File, classify(t.name(c)), 1));
            return;
        }
        let inset = self.px;
        let inner = Box64 { x0: b.x0 + inset, y0: b.y0 + inset, x1: b.x1 - inset, y1: b.y1 - inset };
        let enter = depth + 1 < MAX_DEPTH && inner.w() > 0.0 && inner.h() > 0.0 && inner.area() >= SOLID_PX2 * self.px * self.px;
        if enter {
            out.push(tile(Kind::Frame, Class::Other, t.subtree_files[c]));
            self.fill(c, inner, depth + 1, out);
        } else {
            out.push(tile(Kind::Solid, Class::Other, t.subtree_files[c]));
        }
    }
}

/// Worst aspect ratio of a row of total area `s` (largest item `hi`, smallest `lo`) laid along `side`.
fn worst(s: f64, lo: f64, hi: f64, side: f64) -> f64 {
    ((side * side * hi) / (s * s)).max((s * s) / (side * side * lo))
}

/// Boxes tiling `area`, one per weight in order, with areas proportional to the weights (Bruls,
/// Huizing, van Wijk). The weights are positive and `area` has positive width and height.
fn squarify(weights: &[f64], area: Box64) -> Vec<Box64> {
    let n = weights.len();
    let scale = area.area() / weights.iter().sum::<f64>();
    let mut out = Vec::with_capacity(n);
    let mut rest = area;
    let mut i = 0;
    while i < n {
        // A wide remainder gets a column on its left edge, a tall one a strip along its top edge.
        let wide = rest.w() >= rest.h();
        let side = rest.w().min(rest.h());
        let first = weights[i] * scale;
        let (mut sum, mut lo, mut hi) = (first, first, first);
        let mut cur = worst(sum, lo, hi, side);
        let mut j = i + 1;
        while j < n {
            let a = weights[j] * scale;
            let (s2, lo2, hi2) = (sum + a, lo.min(a), hi.max(a));
            let w2 = worst(s2, lo2, hi2, side);
            if w2 > cur {
                break;
            }
            (sum, lo, hi, cur) = (s2, lo2, hi2, w2);
            j += 1;
        }
        let (near, far) = if wide { (rest.x0, rest.x1) } else { (rest.y0, rest.y1) };
        let edge = if j == n { far } else { (near + sum / side).min(far) };
        let (o0, o1) = if wide { (rest.y0, rest.y1) } else { (rest.x0, rest.x1) };
        let (mut prev, mut acc) = (o0, 0.0);
        for k in i..j {
            acc += weights[k] * scale;
            let next = if k + 1 == j { o1 } else { o0 + (o1 - o0) * (acc / sum) };
            out.push(if wide {
                Box64 { x0: near, x1: edge, y0: prev, y1: next }
            } else {
                Box64 { x0: prev, x1: next, y0: near, y1: edge }
            });
            prev = next;
        }
        if wide {
            rest.x0 = edge;
        } else {
            rest.y0 = edge;
        }
        i = j;
    }
    out
}

/// The deepest tile containing (x, y): tiles are stored parent before child, so the last match is it.
pub fn hit(tiles: &[Tile], x: f32, y: f32) -> Option<usize> {
    tiles.iter().rposition(|t| t.rect.contains(x, y))
}

/// The folder the map shows for a selection: the selected folder, else a file's parent, else the
/// tree's first root (no selection, or one that no longer exists).
pub fn root_for(tree: &Tree, selected: Option<NodeId>) -> Option<NodeId> {
    let alive = |n: NodeId| !n.is_none() && n.idx() < tree.len() && !tree.flags[n].contains(NodeFlags::DEAD);
    match selected.filter(|&s| alive(s)) {
        Some(s) if tree.flags[s].contains(NodeFlags::DIR) => Some(s),
        Some(s) => Some(if tree.parent[s].is_none() { s } else { tree.parent[s] }),
        None => tree.roots.first().copied(),
    }
}
```

- [ ] **Step 4: Run** `cargo test -p plumb-core treemap -j 8` — Expected: all pass. If `layout_invariants...` reports `areas X vs region Y` off by more than 1, the cause is f32 rounding in `Box64::rect`; widen nothing — investigate the squarify edges instead.
- [ ] **Step 5: Commit** (`git add crates/plumb-core/src/treemap.rs crates/plumb-core/src/treemap/tests.rs`, "Treemap: squarified layout, hit test and map root").

---

### Task 4: Benchmark

**Files:** Modify `crates/plumb-core/benches/core.rs`

**Interfaces:** Consumes `treemap::{layout, hit}`, `Rect`, `kary_builder`.

- [ ] **Step 1: Add** imports `use plumb_core::Rect; use plumb_core::treemap::{hit, layout};` and this function before `criterion_group!`:

```rust
fn bench_treemap(c: &mut Criterion) {
    let tree = kary_builder(5_000_000, 8).finish().unwrap();
    let mut g = c.benchmark_group("treemap");
    g.sample_size(20);
    for (name, w, h) in [("800x600", 800.0f32, 600.0f32), ("3840x2160", 3840.0, 2160.0)] {
        let rect = Rect { x: 0.0, y: 0.0, w, h };
        g.bench_function(format!("layout_root_{name}"), |b| b.iter(|| black_box(layout(&tree, NodeId(0), rect, 1.0))));
        g.bench_function(format!("layout_folder_{name}"), |b| b.iter(|| black_box(layout(&tree, NodeId(9), rect, 1.0))));
    }
    let tiles = layout(&tree, NodeId(0), Rect { x: 0.0, y: 0.0, w: 3840.0, h: 2160.0 }, 1.0);
    g.bench_function(format!("hit_{}_tiles", tiles.len()), |b| b.iter(|| black_box(hit(&tiles, black_box(1921.5), black_box(1080.5)))));
    g.finish();
}
```

and add `bench_treemap` to the `criterion_group!` list.

- [ ] **Step 2: Check CPU load** (`(Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 2 -MaxSamples 2).CounterSamples[-1].CookedValue` under ~20%, else note the load) and run `cargo bench -p plumb-core --bench core -j 8 -- treemap` (expect a few minutes: the 5M-node build dominates). Record the five numbers in the spec's "Performance targets" section as measured (Task 11 writes them up).
- [ ] **Step 3:** If `layout_root_3840x2160` is over 30 ms or `hit` over 0.2 ms, STOP and report to the user before continuing (the spec's fallbacks are per-folder sorted children, and a grid index).
- [ ] **Step 4: Commit** (`git add crates/plumb-core/benches/core.rs`, "Treemap: layout and hit benchmarks").

---

### Task 5: Window geometry for the pane

**Files:** Modify `crates/plumb/src/layout.rs` (+ its test module)

**Interfaces:**
- Consumes: `Rect` (re-export).
- Produces on `Layout`:
  - fields `pub map_on: bool` (wanted), `pub map_w: f32` (wanted width, DIPs); `Layout::new` gives `false` and `MAP_W`.
  - `pub fn map_shown(&self) -> bool`, `pub fn tree_w(&self) -> f32`, `pub fn set_map_w(&mut self, w: f32)`.
  - `pub fn map_pane(&self) -> Option<Rect>`, `map(&self) -> Option<Rect>` (tiles area, pane minus legend), `map_legend(&self) -> Option<Rect>`, `map_divider(&self) -> Option<Rect>`, `map_button(&self) -> Rect`.
  - `Hit::Map`, `Hit::MapDivider`, `Hit::MapButton`.
  - consts `DIVIDER_W = 5.0`, `MAP_MIN_W = 160.0`, `MAP_W = 420.0`, `LEGEND_H = 22.0`, `MAP_BTN_W = 32.0`.
  - `header()`, `rows_area()` and everything built on them use `tree_w()` instead of `w`.
  - `cancel_button()` moves left of the map button.

- [ ] **Step 1: Failing test** (append to the `tests` module):

```rust
    #[test]
    fn the_map_pane_sits_right_of_the_tree_and_squeezes_it() {
        let mut l = Layout::new(1200.0, 600.0);
        assert!(l.map().is_none() && l.tree_w() == 1200.0, "off by default");
        l.map_on = true;
        let (pane, div, area, leg) = (l.map_pane().unwrap(), l.map_divider().unwrap(), l.map().unwrap(), l.map_legend().unwrap());
        assert_eq!((div.x, pane.x), (l.tree_w(), l.tree_w() + DIVIDER_W));
        assert_eq!(pane.x + pane.w, 1200.0);
        assert_eq!((area.y, leg.y), (pane.y, area.y + area.h));
        assert_eq!(leg.y + leg.h, pane.y + pane.h);
        assert_eq!(pane.y + pane.h + STATUS_H, 600.0);
        assert_eq!(l.rows_area().w, l.tree_w() - SCROLLBAR_W);
        assert_eq!(l.header().w, l.tree_w());
        let y = PICKER_H + 5.0;
        assert_eq!(l.hit(5.0, y, 0), Hit::Header(0));
        assert_eq!(l.hit(div.x + 1.0, y, 0), Hit::MapDivider);
        assert_eq!(l.hit(area.x + 5.0, area.y + 5.0, 0), Hit::Map);
        assert_eq!(l.hit(leg.x + 5.0, leg.y + 5.0, 0), Hit::None);
        assert_eq!(l.hit(l.tree_w() - 5.0, 200.0, 0), Hit::Scrollbar);
        let b = l.map_button();
        assert_eq!(l.hit(b.x + 2.0, b.y + 2.0, 0), Hit::MapButton);
        assert!(l.cancel_button().x + l.cancel_button().w <= b.x, "Cancel sits left of the map button");
    }

    #[test]
    fn a_narrow_window_hides_the_pane_and_the_width_stays_within_limits() {
        let mut l = Layout::new(600.0, 600.0);
        l.map_on = true;
        assert!(!l.map_shown() && l.map().is_none() && l.tree_w() == 600.0);
        let mut l = Layout::new(1000.0, 600.0);
        l.map_on = true;
        l.set_map_w(10.0);
        assert_eq!(l.map_pane().unwrap().w, MAP_MIN_W);
        l.set_map_w(5000.0);
        assert!(l.tree_w() >= l.tree_min(), "{} < {}", l.tree_w(), l.tree_min());
        assert!(l.map_pane().unwrap().w >= MAP_MIN_W);
    }
```

- [ ] **Step 2: Run** `cargo test -p plumb layout -j 8` — Expected: compile FAIL.

- [ ] **Step 3: Implement.** In `layout.rs`:
  1. Constants after `GRIP`: `pub const DIVIDER_W: f32 = 5.0; pub const MAP_MIN_W: f32 = 160.0; pub const MAP_W: f32 = 420.0; pub const LEGEND_H: f32 = 22.0; pub const MAP_BTN_W: f32 = 32.0;`
  2. `Hit` gains `Map, MapDivider, MapButton,` (after `FilterClear`).
  3. `Layout` gains the two fields (doc comments: "The map pane is wanted" / "The pane's wanted width in DIPs; clamped when used"); `new` sets `map_on: false, map_w: MAP_W`.
  4. After `set_col_w` add:

```rust
    /// The narrowest the tree may get beside the map: a Name column at its minimum, the other columns and the scrollbar.
    fn tree_min(&self) -> f32 {
        MIN_NAME_W + self.col_w.iter().sum::<f32>() + SCROLLBAR_W
    }

    /// Whether the map pane is on screen: wanted, and the window is wide enough for the tree, the divider and the pane.
    pub fn map_shown(&self) -> bool {
        self.map_on && self.w >= self.tree_min() + DIVIDER_W + MAP_MIN_W
    }

    fn map_pane_w(&self) -> f32 {
        if self.map_shown() { self.map_w.clamp(MAP_MIN_W, self.w - self.tree_min() - DIVIDER_W) } else { 0.0 }
    }

    /// The width the tree's header and rows get: the whole window unless the map pane is shown.
    pub fn tree_w(&self) -> f32 {
        if self.map_shown() { self.w - self.map_pane_w() - DIVIDER_W } else { self.w }
    }

    /// Sets the wanted pane width, within what the window allows.
    pub fn set_map_w(&mut self, w: f32) {
        self.map_w = w.clamp(MAP_MIN_W, (self.w - self.tree_min() - DIVIDER_W).max(MAP_MIN_W));
    }

    fn body_h(&self) -> f32 {
        (self.h - self.top() - STATUS_H).max(0.0)
    }

    /// The whole pane, beside the header and rows.
    pub fn map_pane(&self) -> Option<Rect> {
        self.map_shown().then(|| Rect { x: self.tree_w() + DIVIDER_W, y: self.top(), w: self.map_pane_w(), h: self.body_h() })
    }

    /// Where the tiles go: the pane above its legend strip.
    pub fn map(&self) -> Option<Rect> {
        self.map_pane().map(|p| Rect { h: (p.h - LEGEND_H).max(0.0), ..p })
    }

    pub fn map_legend(&self) -> Option<Rect> {
        self.map_pane().map(|p| Rect { y: p.y + (p.h - LEGEND_H).max(0.0), h: LEGEND_H.min(p.h), ..p })
    }

    /// The strip between the tree and the pane, where a drag resizes the pane.
    pub fn map_divider(&self) -> Option<Rect> {
        self.map_shown().then(|| Rect { x: self.tree_w(), y: self.top(), w: DIVIDER_W, h: self.body_h() })
    }

    /// Shows or hides the pane, at the right end of the status bar.
    pub fn map_button(&self) -> Rect {
        let s = self.status();
        Rect { x: (s.x + s.w - 8.0 - MAP_BTN_W).max(0.0), y: s.y + 2.0, w: MAP_BTN_W, h: s.h - 4.0 }
    }
```
  5. `header()`: `w: self.tree_w()`; `rows_area()`: `w: (self.tree_w() - SCROLLBAR_W).max(0.0)`; `cancel_button()`: `x: (s.x + s.w - 8.0 - MAP_BTN_W - 4.0 - CANCEL_W).max(0.0)`.
  6. `hit()`: after the `filter_box` check and before `header` insert:

```rust
        if self.map_button().contains(x, y) {
            return Hit::MapButton;
        }
        if self.map_divider().is_some_and(|d| d.contains(x, y)) {
            return Hit::MapDivider;
        }
        if self.map().is_some_and(|m| m.contains(x, y)) {
            return Hit::Map;
        }
```

- [ ] **Step 4: Run** `cargo test -p plumb layout -j 8`; Expected: pass, and the existing `regions_tile_the_window`, `hit_testing` etc. unchanged.
- [ ] **Step 5: Commit** (`git add crates/plumb/src/layout.rs`, "Layout: a map pane, its divider and legend, and the status bar's map button").

---

### Task 6: `RowModel::reveal`

**Files:** Modify `crates/plumb-core/src/rows.rs` (method after `select`; test in the existing `tests` module)

**Interfaces:** Produces `RowModel::reveal(&mut self, tree: &Tree, id: NodeId)`: selects `id` and expands every ancestor, top first, so its row exists (unless a name filter hides it).

- [ ] **Step 1: Failing test** (in `rows.rs` `tests`):

```rust
    #[test]
    fn reveal_selects_a_node_and_opens_every_folder_above_it() {
        let tree = crate::synth::kary_builder(400, 3).finish().unwrap();
        let mut m = RowModel::new(&tree);
        let deep = NodeId(399);
        assert!(m.selected_index().is_none());
        m.reveal(&tree, deep);
        assert_eq!(m.selected(), Some(deep));
        assert!(m.selected_index().is_some(), "its row exists");
        let mut p = tree.parent[deep];
        while !p.is_none() {
            assert!(m.is_expanded(p), "{p:?} is open");
            p = tree.parent[p];
        }
    }
```

- [ ] **Step 2: Run** `cargo test -p plumb-core reveal -j 8` — FAIL (no method).
- [ ] **Step 3: Implement** after `select`:

```rust
    /// Selects `id` and opens every folder above it, top first, so its row exists. (A name filter can
    /// still hide a node that does not match it.)
    pub fn reveal(&mut self, tree: &Tree, id: NodeId) {
        let mut chain = Vec::new();
        let mut p = tree.parent[id];
        while !p.is_none() {
            chain.push(p);
            p = tree.parent[p];
        }
        for &a in chain.iter().rev() {
            self.expand(tree, a);
        }
        self.select(id);
    }
```
- [ ] **Step 4: Run** the test — pass. **Step 5: Commit** (`git add crates/plumb-core/src/rows.rs`, "RowModel::reveal opens the ancestors of a node and selects it").

---

### Task 7: `[map]` in `state.toml`

**Files:** Modify `crates/plumb/src/state.rs`

**Interfaces:** Produces `state::MapState { visible: bool, width: f32 }` (`Clone, Copy, Debug, PartialEq, Serialize, Deserialize`, `Default` = `{ true, 420.0 }`, `#[serde(default)]`) and `State.map: MapState`.

- [ ] **Step 1: Failing test** (in `state.rs` tests; also update the round-trip test's `State { .. }` literal with `map: MapState { visible: false, width: 333.5 }` and the `partial` assertion to check `partial.map == MapState::default()`):

```rust
    #[test]
    fn the_map_defaults_to_shown_and_a_damaged_value_falls_back() {
        assert_eq!(State::default().map, MapState { visible: true, width: 420.0 });
        let s = State::parse("[map]\nvisible = false\n");
        assert_eq!(s.map, MapState { visible: false, width: 420.0 }, "a missing key means its default");
        assert_eq!(State::parse("[map]\nwidth = \"wide\"\n"), State::default(), "a damaged file means defaults");
    }
```
- [ ] **Step 2: Run** — FAIL (no `MapState`).
- [ ] **Step 3: Implement**:

```rust
/// The treemap pane: shown or hidden, and its wanted width in DIPs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MapState {
    pub visible: bool,
    pub width: f32,
}

impl Default for MapState {
    fn default() -> Self {
        Self { visible: true, width: 420.0 }
    }
}
```
and in `State` add `pub map: MapState,` (doc: "The treemap pane."). `State` derives `Default`, `PartialEq`, `Clone`, `Debug` — all satisfied.
- [ ] **Step 4: Run** `cargo test -p plumb state -j 8` — pass (the app.rs `remembered()` literal breaks the build until Task 8; do the one-line fix there now: add `map: Default::default()` to the literal in `remembered()` so this task compiles on its own, Task 8 replaces it).
- [ ] **Step 5: Commit** (`git add crates/plumb/src/state.rs crates/plumb/src/app.rs`, "State: remember the map pane's visibility and width").

---

### Task 8: `App` — map state, selection sync, clicks, drag, hover, tip, toggle

**Files:** Modify `crates/plumb/src/app.rs` (+ its test module)

**Interfaces:**
- Consumes: `treemap::{self, Kind, Tile}`, `Layout::{map, map_pane, map_shown, set_map_w, map_button}`, `Hit::{Map, MapDivider, MapButton}`, `RowModel::reveal`, `fmt::{display_path, size}`.
- Produces on `App`:
  - `Input::ToggleMap`.
  - `pub map_hover: Option<usize>` (tile index under the pointer).
  - `pub fn update_map(&mut self, px: f32)` — call before painting; rebuilds the tiles only when dirty or the rectangle/`px`/root changed.
  - `pub fn map_tiles(&self) -> &[Tile]`, `pub fn map_classes(&self) -> [bool; 10]`, `pub fn map_selected_tile(&self) -> Option<usize>`.
  - `fn tile_tip(tree: &Tree, t: &Tile) -> String` (free function).
  - (`with_scene` building the `Scene`'s map view is Task 9's job; this task never touches `Scene`.)

- [ ] **Step 1: Failing tests** (append to `app.rs` `tests`; `use plumb_core::treemap::{self, Kind};` at the top of the module if absent):

```rust
    fn with_map() -> App {
        let mut a = scanned();
        a.layout.map_on = true;
        a.update_map(1.0);
        a
    }

    fn centre(t: &treemap::Tile) -> (f32, f32) {
        (t.rect.x + t.rect.w / 2.0, t.rect.y + t.rect.h / 2.0)
    }

    #[test]
    fn the_map_follows_the_trees_selection_and_ignores_its_own_clicks() {
        let mut a = with_map();
        assert_eq!(a.map_root, Some(NodeId(0)));
        a.handle(Input::Click(20.0, row_y(1)));
        a.update_map(1.0); // the hook that notices the selection moved
        let sel = a.model.as_ref().unwrap().selected();
        assert_eq!(a.map_root, treemap::root_for(a.tree.as_ref().unwrap(), sel), "a click in the tree re-roots the map");
        let (i, t) = a.map_tiles().iter().copied().enumerate().find(|(_, t)| t.kind == Kind::File).unwrap();
        let before = a.map_root;
        let (x, y) = centre(&t);
        a.handle(Input::Click(x, y));
        a.update_map(1.0);
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
        assert_eq!(a.map_root, before, "a click on the map keeps the root");
        assert_eq!(a.map_selected_tile(), Some(i));
        assert!(a.model.as_ref().unwrap().selected_index().is_some(), "its row is revealed in the tree");
    }

    #[test]
    fn double_clicking_a_folder_tile_roots_the_map_there() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::Frame).unwrap();
        let (x, y) = (t.rect.x + 0.5, t.rect.y + 0.5); // the frame's border, clear of its children
        a.handle(Input::Click(x, y));
        a.handle(Input::DoubleClick(x, y));
        assert_eq!(a.map_root, Some(t.node));
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
        a.update_map(1.0);
        assert!(a.map_tiles().iter().all(|k| k.depth > 0 || a.tree.as_ref().unwrap().parent[k.node] == t.node || k.kind == Kind::Rest));
    }

    #[test]
    fn a_double_click_on_a_file_tile_only_selects_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        let before = a.map_root;
        a.handle(Input::DoubleClick(x, y));
        assert_eq!((a.map_root, a.model.as_ref().unwrap().selected()), (before, Some(t.node)));
    }

    #[test]
    fn hovering_a_tile_gives_a_tip_and_leaving_the_map_clears_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        a.handle(Input::Hover(x, y));
        assert!(a.map_hover.is_some());
        let tip = a.tip_at(x, y).unwrap();
        assert!(tip.contains(&a.tree.as_ref().unwrap().name_string(t.node)), "{tip}");
        a.handle(Input::Hover(5.0, 5.0));
        assert_eq!((a.map_hover, a.tip_at(5.0, 5.0)), (None, None));
    }

    #[test]
    fn a_rest_tile_says_how_many_items_it_holds() {
        let tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Rest, bytes: 2048, count: 7 };
        assert!(tile_tip(&tree, &t).starts_with("7 smaller items, "));
        let f = treemap::Tile { node: NodeId(1), kind: Kind::File, count: 1, ..t };
        assert!(tile_tip(&tree, &f).contains(&tree.name_string(NodeId(1))));
    }

    #[test]
    fn right_clicking_a_tile_selects_it_and_asks_for_the_menu() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        assert_eq!(a.handle(Input::RightClick(x, y)), Effect::Menu(t.node));
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
    }

    #[test]
    fn the_toggle_and_the_divider_work_and_stay_within_limits() {
        let mut a = scanned();
        assert!(!a.layout.map_on);
        a.handle(Input::ToggleMap);
        assert!(a.layout.map_on);
        let b = a.layout.map_button();
        a.handle(Input::Click(b.x + 2.0, b.y + 2.0));
        assert!(!a.layout.map_on, "the status bar button toggles too");
        a.layout.map_on = true;
        a.layout.set_map_w(200.0); // at 800 DIPs the pane can reach 343: room to grow by 50
        let (d, w0) = (a.layout.map_divider().unwrap(), a.layout.map_w);
        assert!(a.over_divider(d.x + 1.0, d.y + 5.0));
        a.handle(Input::Click(d.x + 1.0, d.y + 5.0));
        a.handle(Input::Drag(d.x - 49.0, d.y + 5.0));
        assert_eq!(a.layout.map_w, w0 + 50.0, "dragging left widens the pane");
        a.handle(Input::Drag(-10_000.0, 0.0));
        assert!(a.layout.column(0).1 >= crate::layout::MIN_NAME_W, "the tree keeps its minimum");
        a.handle(Input::Drag(10_000.0, 0.0));
        assert_eq!(a.layout.map_pane().unwrap().w, crate::layout::MAP_MIN_W, "and the pane keeps its own");
        a.handle(Input::Release);
        let before = a.layout.map_w;
        a.handle(Input::Drag(0.0, 0.0));
        assert_eq!(a.layout.map_w, before, "released");
    }

    #[test]
    fn the_map_survives_a_tree_swap_and_an_emptied_root() {
        let mut a = with_map();
        a.swap_tree(kary_builder(50, 3).finish().unwrap(), "again".into());
        a.update_map(1.0);
        assert!(a.map_root.is_some() && !a.map_tiles().is_empty());
        a.begin_scan(1);
        a.update_map(1.0);
        assert!(a.map_tiles().is_empty() && a.map_hover.is_none(), "no tree, no tiles");
    }

    #[test]
    fn the_map_pane_state_is_remembered() {
        let mut a = app();
        a.layout.map_on = false;
        a.layout.map_w = 300.0;
        let s = a.remembered();
        let mut b = app();
        b.restore(&s);
        assert_eq!((b.layout.map_on, b.layout.map_w), (false, 300.0));
        let mut c = app();
        c.restore(&crate::state::State { map: crate::state::MapState { visible: true, width: f32::NAN }, ..Default::default() });
        assert!(c.layout.map_on && c.layout.map_w.is_finite() && c.layout.map_w >= crate::layout::MAP_MIN_W);
    }
```
 (The `tree_w` line in the divider test is deliberately loose; replace it by `assert!(a.layout.tree_w() >= a.layout.col_w.iter().sum::<f32>())` during implementation if clearer. The point: the pane never goes under 160 and the tree never under its minimum.)

- [ ] **Step 2: Run** `cargo test -p plumb app -j 8` — compile FAIL.

- [ ] **Step 3: Implement.**
  1. Imports: `use plumb_core::treemap::{self, Kind, Tile};`. `Input` gains `/// Ctrl+M or the status bar button: show or hide the map pane.\n ToggleMap,`.
  2. `App` fields (after `col_drag`), initialised in `new`:

```rust
    /// The folder the map shows; follows the tree's selection (see `follow_selection`).
    map_root: Option<NodeId>,
    /// The selection `map_root` was last chosen for: a change the map made itself is recorded here so it is not mistaken for one made in the tree.
    map_seen: Option<NodeId>,
    map_tiles: Vec<Tile>,
    /// Root and rectangle/scale bits the tiles were built for.
    map_key: Option<(NodeId, [u32; 5])>,
    map_dirty: bool,
    map_classes: [bool; 10],
    /// The tile under the pointer.
    pub map_hover: Option<usize>,
    /// While dragging the divider: the pointer x and the pane width at the start.
    map_drag: Option<(f32, f32)>,
```
  new(): `map_root: None, map_seen: None, map_tiles: Vec::new(), map_key: None, map_dirty: true, map_classes: [false; 10], map_hover: None, map_drag: None`.
  3. Add (near `select_from_map` below):

```rust
    /// Points the map at the folder for the tree's current selection, unless the map made that selection itself
    /// (`select_from_map` records it in `map_seen`). Runs from `update_map`, so every way the selection can change
    /// (mouse, keys, the filter, a rescan) is picked up before the next paint without hooking each one.
    fn follow_selection(&mut self) {
        let sel = self.model.as_ref().and_then(|m| m.selected());
        if sel == self.map_seen && self.map_root.is_some() == self.tree.is_some() {
            return;
        }
        self.map_seen = sel;
        self.map_root = self.tree.as_ref().and_then(|t| treemap::root_for(t, sel));
        self.map_dirty = true;
    }

    /// The tree was replaced, so every remembered id is meaningless.
    fn map_reset(&mut self) {
        self.map_seen = None;
        self.map_root = None;
        self.map_dirty = true;
        self.map_hover = None;
    }
```
  4. Call `self.map_reset();` in `finish_scan`, `preview_installed` (before its early `return` on a missing tree), `swap_tree`, `swap_slot` (after the swaps), `begin_scan`, `discard_partial`, `cancel_scan` and `fail_scan` (the last four only where they drop the tree). `refresh_rows` (tree changed in place, ids stay valid) ends with `self.map_dirty = true;`.
  5. In `handle_input`:
     - `Input::ToggleMap => self.layout.map_on = !self.layout.map_on,`
     - `Input::Hover`: after `self.hot = ...` add `self.map_hover = if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map) { treemap::hit(&self.map_tiles, x, y) } else { None };`
     - `Input::DoubleClick`: before `let Hit::Row(i) = ... else { return self.click(x, y) };` insert

```rust
                if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map) {
                    if let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                        && self.tree.as_ref().is_some_and(|t| t.flags[id].contains(NodeFlags::DIR))
                    {
                        self.map_root = Some(id);
                        self.map_dirty = true;
                    }
                    return Effect::None;
                }
```
     - `Input::RightClick`: add an `else if` after the Row case:

```rust
                else if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map)
                    && let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                {
                    return Effect::Menu(id);
                }
```
     (restructure the existing `if let Hit::Row(i) = ... && let (...)` into `if ... {} else if ... {}` keeping its body.)
     - `Input::Drag`: after the `else if let Some(g) = self.grab { .. }` branch add `else if let Some((x0, w0)) = self.map_drag { self.layout.set_map_w(w0 + x0 - x); }`.
     - `Input::Release`: add `self.map_drag = None;`.
  6. `click()`: in the second `match hit` add arms
     `Hit::MapButton => self.layout.map_on = !self.layout.map_on,`
     `Hit::MapDivider => self.map_drag = Some((x, self.layout.map_w)),`
     `Hit::Map => { if let Some(i) = treemap::hit(&self.map_tiles, x, y) { self.select_from_map(i); } }`
  7. New methods:

```rust
    /// Selects the node of map tile `i` (not `Rest`, which has none) and brings its row into view; the map root stays.
    fn select_from_map(&mut self, i: usize) -> Option<NodeId> {
        let t = *self.map_tiles.get(i)?;
        if t.kind == Kind::Rest {
            return None;
        }
        let (tree, m) = (self.tree.as_ref()?, self.model.as_mut()?);
        m.reveal(tree, t.node);
        self.map_seen = Some(t.node);
        let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
        self.set_scroll(s);
        Some(t.node)
    }

    /// Whether `map_root` still names a live node of the current tree.
    fn live_map_root(&self) -> Option<NodeId> {
        let t = self.tree.as_ref()?;
        let sel = self.model.as_ref().and_then(|m| m.selected());
        self.map_root.filter(|&r| r.idx() < t.len() && !t.flags[r].contains(NodeFlags::DEAD)).or_else(|| treemap::root_for(t, sel))
    }

    /// Rebuilds the tiles if the tree, the root, the pane's rectangle or the scale changed since they were built.
    /// `px` is the size of one physical pixel in DIPs (`96 / dpi`). Call before painting.
    pub fn update_map(&mut self, px: f32) {
        self.follow_selection();
        let (rect, root) = (self.layout.map(), self.live_map_root());
        let (Some(tree), Some(rect), Some(root)) = (&self.tree, rect, root) else {
            self.map_tiles.clear();
            self.map_key = None;
            self.map_classes = [false; 10];
            self.map_hover = None;
            return;
        };
        let key = (root, [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits(), rect.h.to_bits(), px.to_bits()]);
        if !self.map_dirty && self.map_key == Some(key) {
            return;
        }
        self.map_tiles = treemap::layout(tree, root, rect, px);
        self.map_classes = [false; 10];
        for t in self.map_tiles.iter().filter(|t| t.kind == Kind::File) {
            self.map_classes[t.class.index()] = true;
        }
        (self.map_key, self.map_dirty, self.map_hover) = (Some(key), false, None);
    }

    pub fn map_tiles(&self) -> &[Tile] {
        &self.map_tiles
    }

    pub fn map_classes(&self) -> [bool; 10] {
        self.map_classes
    }

    /// The tile of the selected node, if it has one (a node inside a `Rest` tile has none).
    pub fn map_selected_tile(&self) -> Option<usize> {
        let sel = self.model.as_ref()?.selected()?;
        self.map_tiles.iter().position(|t| t.node == sel && t.kind != Kind::Rest)
    }
```
  8. `restore()` after the column loop:

```rust
        self.layout.map_on = s.map.visible;
        if s.map.width.is_finite() {
            self.layout.map_w = s.map.width.clamp(MAP_MIN_W, 4000.0);
        }
```
   (`use crate::layout::MAP_MIN_W`); `remembered()` → `map: MapState { visible: self.layout.map_on, width: self.layout.map_w }` (import `MapState`).
  9. `over_divider`: `self.col_drag.is_some() || self.map_drag.is_some() || matches!(self.layout.hit(x, y, self.drives.len()), Hit::Divider(_) | Hit::MapDivider)`.
  10. `button_at`: add arm `Hit::MapButton => Some((l.map_button(), "Show or hide the map  (Ctrl+M)".into())),`.
  11. `tip_at`:

```rust
    pub fn tip_at(&self, x: f32, y: f32) -> Option<String> {
        self.button_at(x, y).map(|b| b.1).or_else(|| {
            let t = self.map_tiles.get(treemap::hit(&self.map_tiles, x, y).filter(|_| matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map))?)?;
            Some(tile_tip(self.tree.as_ref()?, t))
        })
    }
```
  and the free function near `sort_key`:

```rust
/// A map tile's tooltip: the path and size of what it stands for, or what a `Rest` tile holds.
fn tile_tip(tree: &Tree, t: &Tile) -> String {
    match t.kind {
        Kind::Rest => format!("{} smaller items, {}", fmt::count(t.count as u64), fmt::size(t.bytes)),
        _ => format!("{}  ({})", fmt::display_path(&tree.path(t.node)), fmt::size(t.bytes)),
    }
}
```
  (the test expects the prefix `"7 smaller items, "`; `fmt::count(7)` must print `7`.)

- [ ] **Step 4: Run** `cargo test -p plumb -j 8` — Expected: all pass (the `tip_at` test for the header `None` still holds).
- [ ] **Step 5: Commit** (`git add crates/plumb/src/app.rs`, "App: the map follows the tree's selection; clicks, drag, hover, tooltip and toggle").

---

### Task 9: Painting

**Files:** Modify `crates/plumb/src/draw.rs`, `crates/plumb/src/app.rs` (`with_scene` only), `crates/plumb/tests/render.rs` (and any other `Scene { .. }` literal: `grep -rn "Scene {" crates/`).

**Interfaces:**
- Produces: `Theme.class: [D2D1_COLOR_F; 10]` indexed by `Class::index()`; `pub struct MapView<'a> { pub tiles: &'a [Tile], pub selected: Option<usize>, pub hover: Option<usize>, pub classes: [bool; 10] }`; `Scene.map: Option<MapView<'a>>`.

- [ ] **Step 1: Failing test.** In `tests/render.rs` extract the paint-and-read part of `render_with` into `fn render_scene(gfx: &Gfx, scene: &Scene, stem: &str) -> Vec<u8>` (everything from `let target = bitmap(...)` through the PNG write; the PNG name becomes `format!("{stem}.png")`; `render_with` passes `format!("plumb-{}{}", if dark {"dark"} else {"light"}, if filter.error {"-error"} else {""})`), add `map: None` to its `Scene` literal, then add:

```rust
fn rgb_at(buf: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i + 2] as i32, buf[i + 1] as i32, buf[i] as i32]
}

fn expect(c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> [i32; 3] {
    [(c.r * 255.0).round() as i32, (c.g * 255.0).round() as i32, (c.b * 255.0).round() as i32]
}

#[test]
fn the_map_paints_a_tile_in_its_class_colour_and_outlines_the_selection() {
    use plumb_core::treemap::{layout as map_layout, Kind};
    for dark in [true, false] {
        let gfx = Gfx::warp().expect("WARP device");
        let mut b = plumb_core::TreeBuilder::new();
        let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
        b.push_str(root, "movie.mp4", 900_000, plumb_core::NodeFlags::empty());
        b.push_str(root, "song.flac", 400_000, plumb_core::NodeFlags::empty());
        b.push_str(root, "notes.txt", 100_000, plumb_core::NodeFlags::empty());
        let tree = b.finish().unwrap();
        let model = RowModel::new(&tree);
        let mut layout = Layout::new(W as f32, H as f32);
        layout.map_on = true;
        let area = layout.map().expect("800 DIPs is wide enough");
        let tiles = map_layout(&tree, root, area, 1.0);
        let theme = Theme::new(dark);
        let video = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 900_000).unwrap();
        let plain = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 100_000).unwrap();
        let shoot = |selected: Option<usize>, stem: &str| {
            let scene = Scene {
                layout: layout.clone(), theme, chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None,
                view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None }),
                message: "", status: "", adapter: "WARP", banner: None, filter: FilterBar::default(),
                map: Some(MapView { tiles: &tiles, selected, hover: None, classes: [true; 10] }),
            };
            render_scene(&gfx, &scene, stem)
        };
        let stem = if dark { "plumb-map-dark" } else { "plumb-map-light" };
        let (picked, plain_shot) = (shoot(Some(plain), stem), shoot(None, "plumb-map-unselected"));
        // Tile edges fall between pixels, so outlines are compared against a frame without one, not against a colour.
        let mid = |i: usize| { let r = tiles[i].rect; ((r.x + r.w / 2.0) as u32, (r.y + r.h / 2.0) as u32) };
        let (vx, vy) = mid(video);
        let want = expect(theme.class[plumb_core::treemap::Class::Video.index()]);
        assert!(rgb_at(&picked, vx, vy).iter().zip(want).all(|(x, y)| (x - y).abs() <= 2), "{dark}: the video tile is in its class colour");
        let r = tiles[plain].rect;
        let (ex, ey) = (r.x as u32 + 1, (r.y + r.h / 2.0) as u32);
        assert_ne!(rgb_at(&picked, ex, ey), rgb_at(&plain_shot, ex, ey), "{dark}: the selected tile gets an outline");
        let (cx, cy) = mid(plain);
        assert_eq!(rgb_at(&picked, cx, cy), rgb_at(&plain_shot, cx, cy), "{dark}: its inside is untouched");
    }
}
```
 (imports at the top of the file: `MapView` from `plumb::draw`; `Theme` is already there; `Layout` must be `Clone`, it is. The notes tile must be at least 5 DIPs wide so its centre is clear of the 2 px outline: 100 000 of 1 400 000 bytes in a 343 x 500 pane is about 12 000 px².)
- [ ] **Step 2: Run** `cargo test -p plumb --test render -j 8` — compile FAIL.
- [ ] **Step 3: Implement** in `draw.rs`:
  1. `use plumb_core::treemap::{Class, Kind, Tile};` (and `Rect` is already `layout::Rect`).
  2. `Theme` gains `/// Tile fill per file class, indexed by Class::index().\n pub class: [D2D1_COLOR_F; 10],`. Dark: `[rgb(0xd0605e), rgb(0xd98c3f), rgb(0x4fae72), rgb(0xb59a3c), rgb(0x5a8fd6), rgb(0x8f6fd0), rgb(0x3fb0b0), rgb(0xb06fa8), rgb(0x7f9fb0), rgb(0x808080)]`; light: `[rgb(0xc4423f), rgb(0xcf7a22), rgb(0x2f9a58), rgb(0xa08420), rgb(0x2f6fc0), rgb(0x7650c0), rgb(0x1f9696), rgb(0x9a4f92), rgb(0x5f8296), rgb(0x9a9a9a)]` (order: Video, Audio, Image, Archive, Document, Code, Executable, DiskImage, Database, Other; adjust by eye in Task 11 if two neighbours blur).
  3. `MapView` struct (fields above) and `Scene.map: Option<MapView<'a>>`. In `app.rs` `with_scene` build it and pass it: `let map = self.layout.map_shown().then(|| MapView { tiles: &self.map_tiles, selected: self.map_selected_tile(), hover: self.map_hover, classes: self.map_classes });` plus `map,` in the `Scene` literal (import `MapView` there). Grep every other `Scene {` literal (`grep -rn "Scene {" crates/`) and give it `map: None`.
  4. Pen changes:

```rust
    /// A framed icon button: quiet until the pointer is over it.
    fn button(&self, r: Rect, glyph: &str, s: &Scene) {
        self.toggle_button(r, glyph, false, s);
    }

    /// An icon button that shows the accent frame while `active`.
    fn toggle_button(&self, r: Rect, glyph: &str, active: bool, s: &Scene) {
        self.fill(r, s.theme.panel);
        self.frame(r, s.theme.select);
        let c = self.state(r, active, s);
        self.draw_text(glyph, &self.painter.glyph, r, c);
    }
```
  5. In `draw()`: after the `match &s.view { .. }` block add `self.map(s);`. In the status bar section: `let reserved = MAP_BTN_W + 8.0 + if l.cancel { CANCEL_W + 4.0 } else { 0.0 };` (replacing the old `reserved`) and after the Cancel button draw `self.toggle_button(l.map_button(), "\u{E80A}", l.map_on, s); // grid`. (Check the glyph on screen in Task 11; `\u{E8A9}` is the fallback.) Import `MAP_BTN_W` from `crate::layout`.
  6. New painting methods:

```rust
    /// The treemap pane: tiles, then the selection and hover outlines over them, then the legend.
    fn map(&self, s: &Scene) {
        let (t, l) = (&s.theme, &s.layout);
        let (Some(pane), Some(area), Some(legend)) = (l.map_pane(), l.map(), l.map_legend()) else { return };
        if let Some(d) = l.map_divider() {
            self.fill(Rect { x: d.x + d.w / 2.0 - 0.5, w: 1.0, ..d }, t.select);
        }
        self.fill(pane, if s.view.is_some() { t.bg } else { t.panel }); // blank until there is a tree
        let Some(m) = &s.map else { return };
        unsafe { self.gfx.d2d.PushAxisAlignedClip(&rc(area), D2D1_ANTIALIAS_MODE_ALIASED) };
        for tile in m.tiles {
            self.tile(tile, t);
        }
        if let Some(h) = m.hover.and_then(|i| m.tiles.get(i)).filter(|_| m.hover != m.selected) {
            self.frame(h.rect, D2D1_COLOR_F { a: 0.85, ..t.fg });
        }
        if let Some(sel) = m.selected.and_then(|i| m.tiles.get(i)) {
            self.frame(sel.rect, t.accent);
            if sel.rect.w >= 4.0 && sel.rect.h >= 4.0 {
                self.frame(Rect { x: sel.rect.x + 1.0, y: sel.rect.y + 1.0, w: sel.rect.w - 2.0, h: sel.rect.h - 2.0 }, t.accent);
            }
        }
        unsafe { self.gfx.d2d.PopAxisAlignedClip() };
        self.fill(legend, t.panel);
        let mut x = legend.x + 8.0;
        for c in Class::ALL.into_iter().filter(|c| m.classes[c.index()]) {
            let w = self.text_width(c.label());
            if x + 14.0 + w > legend.x + legend.w {
                break;
            }
            self.fill(Rect { x, y: legend.y + (legend.h - 10.0) / 2.0, w: 10.0, h: 10.0 }, t.class[c.index()]);
            self.text(c.label(), Rect { x: x + 14.0, y: legend.y, w: w + 2.0, h: legend.h }, t.dim, false);
            x += 14.0 + w + 12.0;
        }
    }

    fn tile(&self, tile: &Tile, t: &Theme) {
        let r = tile.rect;
        match tile.kind {
            Kind::File => {
                let c = t.class[tile.class.index()];
                if r.w >= 3.0 && r.h >= 3.0 {
                    // A darker edge: the dark fill, then the colour inside it (two draws, not five).
                    self.fill(r, D2D1_COLOR_F { r: c.r * 0.6, g: c.g * 0.6, b: c.b * 0.6, a: c.a });
                    self.fill(Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 }, c);
                } else {
                    self.fill(r, c);
                }
            }
            Kind::Frame => {
                self.fill(r, D2D1_COLOR_F { a: 0.07, ..t.fg });
                self.frame(r, D2D1_COLOR_F { a: 0.45, ..t.dim });
            }
            Kind::Solid => self.fill(r, D2D1_COLOR_F { a: 0.45, ..t.dim }),
            Kind::Rest => self.fill(r, D2D1_COLOR_F { a: 0.22, ..t.dim }),
        }
    }
```
- [ ] **Step 4: Run** `cargo test -p plumb -j 8` — pass. Then `$env:PLUMB_SHOT_DIR="$env:TEMP\plumb-shots"; cargo test -p plumb --test render -j 8` and Read `plumb-map-dark.png` / `plumb-map-light.png` to look at them (tiles, legend, divider, selection outline, status bar button).
- [ ] **Step 5: Commit** (`git add crates/plumb/src/draw.rs crates/plumb/tests/render.rs`, "Draw the treemap pane: class colours, frames, outlines, legend, map button").

---

### Task 10: Window glue

**Files:** Modify `crates/plumb/src/window.rs`

**Interfaces:** Consumes `App::{update_map, map_hover}`, `Input::ToggleMap`.

- [ ] **Step 1: Edits.**
  1. `try_render` (around line 500): before `self.app.with_scene(..)` add `self.app.update_map(96.0 / self.dpi);`.
  2. `WM_MOUSEMOVE` (no button): `let before = (w.app.hover, w.app.hot);` → `(w.app.hover, w.app.hot, w.app.map_hover)` and the comparison likewise.
  3. `WM_MOUSELEAVE`: `if w.app.hover.take().is_some() | w.app.hot.take().is_some() | w.app.map_hover.take().is_some() {`.
  4. `key_input`: in the `if ctrl` match add `v if v == b'M' as u16 => return Some(Input::ToggleMap),`.
  5. Check `WM_CHAR` (≈ line 1520): Ctrl+M also delivers `\r` as a character; confirm the handler drops control characters (the filter box's `insert` does, but if `WM_CHAR` maps `\r` to `Input::Enter` Ctrl+M would also fire Enter). If it does, skip control characters there.
- [ ] **Step 2: Run** `cargo build -p plumb -j 8` and `cargo test --workspace -j 8` — pass.
- [ ] **Step 3: Commit** (`git add crates/plumb/src/window.rs`, "Window: Ctrl+M, build the map before painting, repaint on map hover").

---

### Task 11: Run it, measure, document

**Files:** Modify `CHANGELOG.md`, `docs/superpowers/specs/2026-10-14-plumb-treemap-design.md`, `docs/superpowers/specs/2026-10-11-plumb-disk-design.md`

- [ ] **Step 1: Look.** Build release (`cargo build --release -p plumb -j 8`), start it with `--scan C:` (headless is impossible; the window opens). Check, with the in-app browser pane unavailable, via the render PNGs plus the user's eyes: dark and light class colours, legend, divider cursor, Ctrl+M, status bar button glyph, narrow-window hiding, live changes (create/delete a large file in a scanned folder: the map redraws within the live pacing), a scan in progress (the map follows the preview). Ask the user to look at the real window; do not leave a window popping over their work uninvited.
- [ ] **Step 2: Measure the paint.** Throwaway local patch (not committed): wrap `self.app.with_scene(..)` in `try_render` with `Instant` and `eprintln!` the ms when `PLUMB_PAINT_MS` is set. Record map-visible paint time on the real adapter for the C: root at the window's size and maximized, and the layout time (`update_map`) alongside. `git checkout crates/plumb/src/window.rs` afterwards (verify with `git diff --stat`).
- [ ] **Step 3: Decide.** Paint under ~4 ms: done, record the numbers. Over: do Task 12.
- [ ] **Step 4: Open questions.** At 100% and 200% DPI judge the 9 px² floor and the 24-level nesting on a real drive; if either is wrong, the change is one constant (`MIN_PX2`, `MAX_DEPTH`) plus the spec text.
- [ ] **Step 5: Docs.** `CHANGELOG.md` new "Unreleased" section (map pane, Ctrl+M, `[map]` state); spec status "implemented" with the measured numbers under "Performance targets"; disk-design spec: strike the parked treemap item. Record anything parked (age colouring etc. are already listed).
- [ ] **Step 6: Commit** the docs.

---

### Task 12 (only if Task 11 measures the paint over ~4 ms): cache the tiles in a bitmap

**Files:** Modify `crates/plumb/src/draw.rs`, `crates/plumb/src/app.rs`, `crates/plumb/src/window.rs`

The overlay split already exists (`Pen::map` draws tiles, then outlines). Draw the tiles into an offscreen `ID2D1Bitmap1` (`D2D1_BITMAP_OPTIONS_TARGET`, size = pane in physical pixels, created with `CreateBitmap`) when a `generation: u64` on `MapView` (bumped by `update_map` on every rebuild) or the pane size or the theme changes, and each frame `DrawBitmap` it into the pane and then draw only the overlays and legend. The cache lives in `Painter` (like the icon cache, `RefCell`), dropped with the device on recreate. Steps: (1) test: render twice with the same `generation` and check the second frame is pixel-identical and calls the tile loop once (count via a `Cell<usize>` in `Painter`, exposed `#[cfg(test)]`); (2) implement; (3) re-measure and record.

---

## Deviations from the spec

- `Layout` gets `map_on`/`map_w` plus `map_pane()/map()/map_legend()/map_divider()` rather than a single `map: Option<Rect>` and `divider_x`; same information, one place that decides visibility.
- `Rect` moves to `plumb-core` (one rectangle type) instead of core defining its own.
- `layout(tree, root, rect, px)` takes `px` (DIPs per physical pixel) instead of an options struct; the three limits are constants in the module.
- The map root is kept in sync by comparing the selection inside `update_map` (`follow_selection`, before every paint), not by hooking each place the tree selection can change.

## Stress-test log (3 rounds before handing over)

1. **Hand-run every test against the code.** Caught: the divider-drag test asserted `+50` but the pane is clamped to 343 DIPs at 800 wide (now starts from 200 and also checks both clamps); the render test compared an outline pixel to the accent colour although tile edges fall between pixels (now compares a frame with and without the selection, and the inside must be unchanged); `refresh_rows` only needed `map_dirty`, not a selection follow; the pane should be blank `panel` before a tree exists (spec) rather than `bg`.
2. **Compile-read the new code.** Caught: `Task 8` and `Task 9` both edited `Scene`, so a commit between them would not build (the `with_scene` edit moved to Task 9); the nine `follow_selection` call sites and the `handle` wrapper were more hooks than needed, replaced by one call at the start of `update_map` (runs before every paint, so keys, the filter and rescans are all covered); tests that assert on `map_root` after a tree click now call `update_map` first.
3. **Spec coverage and risk sweep.** Every spec section maps to a task (root choice 3/8, drawn set 3, filter ignored 8, reparse points 3 (size 0), live pacing 8, selection sync 6/8, toggle 5/8/10, empty states 3/9, layout 3, hit 3, colour 2/9, rendering 9/12, tooltip 8, window layout 5, state 7, tests across, benchmark 4, targets 4/11). Residual risks, deliberately left to execution: `Ctrl+M` also produces a `` `WM_CHAR` (Task 10 step 5 checks it is dropped); the status button glyph `U+E80A` is unverified (Task 9/11 look at it); class colours are a taste call (Task 11); a name filter that hides a revealed node leaves the selection without a row (accepted, noted on `reveal`).
