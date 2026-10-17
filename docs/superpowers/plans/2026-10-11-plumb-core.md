---
status: done
---

# plumb-core Implementation Plan (Plan 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline; the tasks depend on each other) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `plumb-core`, the pure Rust data layer for plumb's disk pane: the tree, the row model, the NTFS MFT record parser, synthetic test inputs and criterion benchmarks.

**Architecture:** A Cargo workspace with one crate, `plumb-core`, that has no UI and no Win32 types. The tree is struct-of-arrays indexed by a `NodeId(u32)` newtype; the row model turns a finished tree into the flat, sorted, virtualized list a tree view draws; the parser turns raw 1 KiB `$MFT` records into plain structs. Plans 2 (scanners) and 3 (UI, `plumb` binary crate) build on it.

**Tech Stack:** Rust 2024 edition (nightly 1.100 installed), `bitflags` 2, `criterion` 0.8 (dev only).

**Spec:** `docs/superpowers/specs/2026-10-11-plumb-disk-design.md` (sections "Tree", "Scanners" for the record format, "Testing and benchmarks").

## Global Constraints

- `plumb-core` has no UI and no Win32 types, and its only runtime dependency is `bitflags`. `criterion` is a dev-dependency only.
- Names are stored as UTF-16 (`u16`) in one shared pool; paths use `\` as the separator.
- Per-node cost stays at 39 bytes plus the name: `parent`, `first_child`, `next_sibling` (`NodeId`, 4 bytes each), `name_off` (`u32`), `name_len` (`u16`), `size` and `subtree_size` (`u64`), `subtree_files` (`u32`), `flags` (`NodeFlags`, 1 byte).
- DOS-only (namespace 2) names are dropped by the parser. Alternate data streams are ignored. Resident data counts as 0 allocated bytes. Compressed or sparse streams use the compressed size.
- Tests never open a window or touch a GPU.
- Build from a checkout whose path is short. This machine's MSVC linker crashes with `LNK1000` on very long source paths (a ~250-character scratch path did it); the repo at `C:\Users\Winston Li\Documents\GitHub\plumb` is fine.
- Cap build parallelism (`-j 4`). Before running benchmarks, check that CPU load is under about 20% and report the load with any numbers.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Not in this plan (Plans 2 and 3)

The `Scanner` trait, the MFT volume reader, the directory walker, hard-link resolution and parent resolution by record number (they run in the scanners' merge pass), the Win32 window, drawing, and the `plumb` binary crate.

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Workspace root |
| `.gitignore` | Ignore `/target` |
| `crates/plumb-core/Cargo.toml` | Crate manifest |
| `crates/plumb-core/src/lib.rs` | Module list and re-exports |
| `crates/plumb-core/src/node.rs` | `NodeId`, `NodeFlags`, `Vec<T>` indexing by `NodeId` |
| `crates/plumb-core/src/tree.rs` | `Tree`, `TreeBuilder`, `TreeError`: building, validation, subtree sums |
| `crates/plumb-core/src/rows.rs` | `RowModel`: expansion, sorting, selection, scrolling |
| `crates/plumb-core/src/synth.rs` | `RecordBuilder` (fake MFT records) and big synthetic trees, for tests and benchmarks |
| `crates/plumb-core/src/mft.rs` | `parse_record` and the parsed-record types |
| `crates/plumb-core/benches/core.rs` | Criterion benchmarks for the four CPU hot paths |
| `docs/benchmarks.md` | Baseline numbers with machine load |

---

### Task 1: Workspace scaffold, `NodeId` and `NodeFlags`

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `crates/plumb-core/Cargo.toml`, `crates/plumb-core/src/lib.rs`, `crates/plumb-core/src/node.rs`

**Interfaces:**
- Produces: `NodeId(pub u32)` with `NodeId::NONE`, `is_none()`, `idx() -> usize`; `Index<NodeId>` and `IndexMut<NodeId>` for `Vec<T>`; `NodeFlags` (`DIR`, `REPARSE`, `HARDLINK`, `DENIED`; `Default`, `Copy`).

- [ ] **Step 1: Create the scaffold and the failing tests**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/plumb-core"]
```

`.gitignore`:

```
/target
```

`crates/plumb-core/Cargo.toml`:

```toml
[package]
name = "plumb-core"
version = "0.1.0"
edition = "2024"

[dependencies]
bitflags = "2"
```

`crates/plumb-core/src/lib.rs`:

```rust
//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod node;

pub use node::{NodeFlags, NodeId};
```

`crates/plumb-core/src/node.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_distinct_and_newtype_is_four_bytes() {
        assert!(NodeId::NONE.is_none());
        assert!(!NodeId(0).is_none());
        assert_eq!(std::mem::size_of::<NodeId>(), 4);
        assert_eq!(std::mem::size_of::<NodeFlags>(), 1);
    }

    #[test]
    fn vec_indexes_by_node_id() {
        let mut v = vec![10u64, 20, 30];
        assert_eq!(v[NodeId(1)], 20);
        v[NodeId(2)] = 99;
        assert_eq!(v[2], 99);
    }

    #[test]
    fn flags_combine() {
        let f = NodeFlags::DIR | NodeFlags::DENIED;
        assert!(f.contains(NodeFlags::DIR));
        assert!(!f.contains(NodeFlags::REPARSE));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: compile errors, `cannot find type NodeId` / `NodeFlags` (the first run also downloads and builds `bitflags`).

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-core/src/node.rs`, above the `#[cfg(test)]` module:

```rust
use bitflags::bitflags;
use std::ops::{Index, IndexMut};

/// Index of a node in a [`crate::Tree`]. `NodeId::NONE` means "no node".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const NONE: NodeId = NodeId(u32::MAX);

    pub fn is_none(self) -> bool {
        self == Self::NONE
    }

    pub fn idx(self) -> usize {
        self.0 as usize
    }
}

// Implemented for `Vec`, not `[T]`: `Vec` already has a generic `Index` impl, so method
// resolution would never autoderef to a slice impl.
impl<T> Index<NodeId> for Vec<T> {
    type Output = T;
    fn index(&self, id: NodeId) -> &T {
        &self.as_slice()[id.idx()]
    }
}

impl<T> IndexMut<NodeId> for Vec<T> {
    fn index_mut(&mut self, id: NodeId) -> &mut T {
        &mut self.as_mut_slice()[id.idx()]
    }
}

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct NodeFlags: u8 {
        const DIR = 1;
        const REPARSE = 2;
        const HARDLINK = 4;
        const DENIED = 8;
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 3 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore crates
git commit -m "Add plumb-core scaffold with NodeId and NodeFlags" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `Tree` and `TreeBuilder`

**Files:**
- Create: `crates/plumb-core/src/tree.rs`
- Modify: `crates/plumb-core/src/lib.rs`

**Interfaces:**
- Consumes: `NodeId`, `NodeFlags` (Task 1).
- Produces:
  - `TreeBuilder::{new(), with_capacity(nodes: usize, pool_units: usize), len(), is_empty(), push(parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId, push_str(parent: NodeId, name: &str, size: u64, flags: NodeFlags) -> NodeId, finish(self) -> Result<Tree, TreeError>}`. A node's `parent` may be a node added later.
  - `Tree` public fields (all `Vec`, indexed by `NodeId`): `parent`, `first_child`, `next_sibling`, `size`, `subtree_size`, `subtree_files`, `flags`, and `roots: Vec<NodeId>`; methods `len()`, `is_empty()`, `name(id) -> &[u16]`, `name_string(id) -> String`, `children(id) -> impl Iterator<Item = NodeId>`, `path(id) -> String`.
  - `TreeError::{BadParent { node: NodeId }, Cycle}`.
  - Test helper `tree::tests::sample()` (a `pub(crate)` module, used by Task 3): `0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)`.

- [ ] **Step 1: Write the failing tests**

`crates/plumb-core/src/tree.rs`:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// C: -> big/{f1=100, f2=300}, mid/{g=150}, small=50
    pub(crate) fn sample() -> Tree {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let big = b.push_str(root, "big", 0, NodeFlags::DIR);
        b.push_str(big, "f1", 100, NodeFlags::empty());
        b.push_str(big, "f2", 300, NodeFlags::empty());
        let mid = b.push_str(root, "mid", 0, NodeFlags::DIR);
        b.push_str(mid, "g", 150, NodeFlags::empty());
        b.push_str(root, "small", 50, NodeFlags::empty());
        b.finish().unwrap()
    }

    #[test]
    fn sums_are_filled_bottom_up() {
        let t = sample();
        assert_eq!(t.len(), 7);
        assert_eq!(t.roots, vec![NodeId(0)]);
        assert_eq!(t.subtree_size[NodeId(0)], 600);
        assert_eq!(t.subtree_size[NodeId(1)], 400);
        assert_eq!(t.subtree_files[NodeId(0)], 4);
        assert_eq!(t.subtree_files[NodeId(1)], 2);
    }

    #[test]
    fn children_are_in_insertion_order() {
        let t = sample();
        let kids: Vec<_> = t.children(NodeId(0)).collect();
        assert_eq!(kids, vec![NodeId(1), NodeId(4), NodeId(6)]);
        assert_eq!(t.children(NodeId(2)).count(), 0);
    }

    #[test]
    fn names_and_paths() {
        let t = sample();
        assert_eq!(t.name_string(NodeId(3)), "f2");
        assert_eq!(t.path(NodeId(3)), "C:\\big\\f2");
    }

    #[test]
    fn parent_may_be_added_after_child() {
        let mut b = TreeBuilder::new();
        b.push_str(NodeId(1), "child", 7, NodeFlags::empty());
        b.push_str(NodeId::NONE, "root", 0, NodeFlags::DIR);
        let t = b.finish().unwrap();
        assert_eq!(t.subtree_size[NodeId(1)], 7);
        assert_eq!(t.path(NodeId(0)), "root\\child");
    }

    #[test]
    fn bad_parent_is_an_error() {
        let mut b = TreeBuilder::new();
        b.push_str(NodeId(5), "x", 1, NodeFlags::empty());
        assert_eq!(b.finish().err(), Some(TreeError::BadParent { node: NodeId(0) }));
    }

    #[test]
    fn cycles_are_an_error() {
        let mut b = TreeBuilder::new();
        b.push_str(NodeId(1), "a", 1, NodeFlags::DIR);
        b.push_str(NodeId(0), "b", 1, NodeFlags::DIR);
        assert_eq!(b.finish().err(), Some(TreeError::Cycle));
        let mut b = TreeBuilder::new();
        b.push_str(NodeId(0), "self", 1, NodeFlags::DIR);
        assert_eq!(b.finish().err(), Some(TreeError::Cycle));
    }
}
```

Replace `crates/plumb-core/src/lib.rs`:

```rust
//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod node;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use tree::{Tree, TreeBuilder, TreeError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: compile errors, `cannot find type Tree` / `TreeBuilder` / `TreeError`.

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-core/src/tree.rs`, above the `#[cfg(test)]` module:

```rust
use crate::node::{NodeFlags, NodeId};

#[derive(Debug, PartialEq, Eq)]
pub enum TreeError {
    /// `node`'s parent id is not a node in this tree.
    BadParent { node: NodeId },
    /// Some nodes cannot be reached from a root (a parent cycle).
    Cycle,
}

/// Finished, read-only tree stored as parallel arrays indexed by [`NodeId`].
pub struct Tree {
    pub parent: Vec<NodeId>,
    pub first_child: Vec<NodeId>,
    pub next_sibling: Vec<NodeId>,
    /// Own allocated bytes.
    pub size: Vec<u64>,
    /// Own plus descendants.
    pub subtree_size: Vec<u64>,
    /// Files (non-directories) in the subtree, including the node itself.
    pub subtree_files: Vec<u32>,
    pub flags: Vec<NodeFlags>,
    pub roots: Vec<NodeId>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    pool: Vec<u16>,
}

impl Tree {
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// UTF-16 name of `id`.
    pub fn name(&self, id: NodeId) -> &[u16] {
        let off = self.name_off[id] as usize;
        &self.pool[off..off + self.name_len[id] as usize]
    }

    pub fn name_string(&self, id: NodeId) -> String {
        String::from_utf16_lossy(self.name(id))
    }

    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(self.first_child[id]).filter(|c| !c.is_none()), |&c| {
            Some(self.next_sibling[c]).filter(|n| !n.is_none())
        })
    }

    /// Full path, built by walking parent links (no per-node path storage).
    pub fn path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while !cur.is_none() {
            parts.push(self.name_string(cur));
            cur = self.parent[cur];
        }
        parts.reverse();
        parts.join("\\")
    }
}

/// Collects nodes in any order. A node's parent may be added after the node.
#[derive(Default)]
pub struct TreeBuilder {
    parent: Vec<NodeId>,
    size: Vec<u64>,
    flags: Vec<NodeFlags>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    pool: Vec<u16>,
}

impl TreeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(nodes: usize, pool_units: usize) -> Self {
        Self {
            parent: Vec::with_capacity(nodes),
            size: Vec::with_capacity(nodes),
            flags: Vec::with_capacity(nodes),
            name_off: Vec::with_capacity(nodes),
            name_len: Vec::with_capacity(nodes),
            pool: Vec::with_capacity(pool_units),
        }
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    pub fn push(&mut self, parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId {
        let id = NodeId(u32::try_from(self.parent.len()).expect("too many nodes"));
        self.name_off.push(u32::try_from(self.pool.len()).expect("name pool too large"));
        self.name_len.push(u16::try_from(name.len()).expect("name longer than 65535 units"));
        self.pool.extend_from_slice(name);
        self.parent.push(parent);
        self.size.push(size);
        self.flags.push(flags);
        id
    }

    pub fn push_str(&mut self, parent: NodeId, name: &str, size: u64, flags: NodeFlags) -> NodeId {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.push(parent, &units, size, flags)
    }

    /// Links children, validates the parent structure and fills the subtree sums.
    pub fn finish(self) -> Result<Tree, TreeError> {
        let n = self.parent.len();
        let mut first_child = vec![NodeId::NONE; n];
        let mut next_sibling = vec![NodeId::NONE; n];
        let mut roots = Vec::new();
        // Walk backwards so each child list ends up in ascending id order.
        for i in (0..n).rev() {
            let id = NodeId(i as u32);
            let p = self.parent[id];
            if p.is_none() {
                roots.push(id);
            } else if p.idx() >= n {
                return Err(TreeError::BadParent { node: id });
            } else {
                next_sibling[id] = first_child[p];
                first_child[p] = id;
            }
        }
        roots.reverse();

        // Breadth-first order from the roots; anything left over is in a cycle.
        let mut order: Vec<NodeId> = Vec::with_capacity(n);
        order.extend_from_slice(&roots);
        let mut k = 0;
        while k < order.len() {
            let mut c = first_child[order[k]];
            while !c.is_none() {
                order.push(c);
                c = next_sibling[c];
            }
            k += 1;
        }
        if order.len() != n {
            return Err(TreeError::Cycle);
        }

        let mut subtree_size = self.size.clone();
        let mut subtree_files: Vec<u32> =
            self.flags.iter().map(|f| u32::from(!f.contains(NodeFlags::DIR))).collect();
        for &id in order.iter().rev() {
            let p = self.parent[id];
            if !p.is_none() {
                subtree_size[p] += subtree_size[id];
                subtree_files[p] += subtree_files[id];
            }
        }

        Ok(Tree {
            parent: self.parent,
            first_child,
            next_sibling,
            size: self.size,
            subtree_size,
            subtree_files,
            flags: self.flags,
            roots,
            name_off: self.name_off,
            name_len: self.name_len,
            pool: self.pool,
        })
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 9 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates
git commit -m "Add Tree and TreeBuilder with subtree sums and validation" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `RowModel`

**Files:**
- Create: `crates/plumb-core/src/rows.rs`
- Modify: `crates/plumb-core/src/lib.rs`

**Interfaces:**
- Consumes: `Tree`, `NodeId`, `NodeFlags`, and the test helper `tree::tests::sample()` (Task 2).
- Produces: `SortKey::{Name, Size, Items}`; `Key::{Up, Down, Left, Right, PageUp, PageDown, Home, End}`; `Row { id: NodeId, depth: u16 }`; `RowModel` with `new(&Tree)`, `rows() -> &[Row]`, `selected() -> Option<NodeId>`, `select(NodeId)`, `selected_index() -> Option<usize>`, `is_expanded(NodeId) -> bool`, `set_sort(&Tree, SortKey, desc: bool)`, `expand(&Tree, NodeId)`, `collapse(NodeId)`, `toggle(&Tree, NodeId)`, `handle_key(&Tree, Key, page: usize)`, `visible_window(scroll: usize, height: usize) -> &[Row]`, `scroll_to_selected(scroll: usize, height: usize) -> usize`, `is_expandable(&Tree, NodeId) -> bool`. Every method that reads the tree takes it as a parameter, so the caller must always pass the same tree. The default sort is Size, descending.

- [ ] **Step 1: Write the failing tests**

`crates/plumb-core/src/rows.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{TreeBuilder, tests::sample};

    // sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
    fn ids(m: &RowModel) -> Vec<u32> {
        m.rows().iter().map(|r| r.id.0).collect()
    }

    #[test]
    fn starts_with_roots_only() {
        let t = sample();
        let m = RowModel::new(&t);
        assert_eq!(ids(&m), vec![0]);
    }

    #[test]
    fn expand_inserts_children_sorted_by_size_desc() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        assert_eq!(ids(&m), vec![0, 1, 4, 6]); // big 400, mid 150, small 50
        m.expand(&t, NodeId(1));
        assert_eq!(ids(&m), vec![0, 1, 3, 2, 4, 6]); // f2 300 before f1 100
        assert_eq!(m.rows()[2].depth, 2);
    }

    #[test]
    fn collapse_removes_descendants_and_reexpand_restores_them() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        m.expand(&t, NodeId(1));
        m.collapse(NodeId(0));
        assert_eq!(ids(&m), vec![0]);
        m.expand(&t, NodeId(0));
        assert_eq!(ids(&m), vec![0, 1, 3, 2, 4, 6]); // big stayed expanded
    }

    #[test]
    fn expanding_a_leaf_does_nothing() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        m.expand(&t, NodeId(6));
        assert_eq!(ids(&m), vec![0, 1, 4, 6]);
        assert!(!m.is_expanded(NodeId(6)));
    }

    #[test]
    fn set_sort_reorders_and_keeps_expansion() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        m.expand(&t, NodeId(1));
        m.set_sort(&t, SortKey::Size, false);
        assert_eq!(ids(&m), vec![0, 6, 4, 1, 2, 3]); // ascending
    }

    #[test]
    fn name_sort_ignores_case() {
        let mut b = TreeBuilder::new();
        let r = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
        b.push_str(r, "b", 1, NodeFlags::empty());
        b.push_str(r, "A", 1, NodeFlags::empty());
        b.push_str(r, "c", 1, NodeFlags::empty());
        let t = b.finish().unwrap();
        let mut m = RowModel::new(&t);
        m.set_sort(&t, SortKey::Name, false);
        m.expand(&t, r);
        assert_eq!(ids(&m), vec![0, 2, 1, 3]);
    }

    #[test]
    fn items_sort_orders_by_file_count() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.set_sort(&t, SortKey::Items, true);
        m.expand(&t, NodeId(0));
        assert_eq!(ids(&m), vec![0, 1, 4, 6]); // big 2 items, then mid 1 and small 1 by id
    }

    #[test]
    fn keys_move_expand_and_collapse() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.handle_key(&t, Key::Down, 10); // nothing selected: selects first row
        assert_eq!(m.selected(), Some(NodeId(0)));
        m.handle_key(&t, Key::Right, 10); // expands C:
        assert_eq!(ids(&m), vec![0, 1, 4, 6]);
        m.handle_key(&t, Key::Right, 10); // moves to first child
        assert_eq!(m.selected(), Some(NodeId(1)));
        m.handle_key(&t, Key::Left, 10); // child is collapsed: goes to parent
        assert_eq!(m.selected(), Some(NodeId(0)));
        m.handle_key(&t, Key::Left, 10); // collapses C:
        assert_eq!(ids(&m), vec![0]);
        m.handle_key(&t, Key::Right, 10);
        m.handle_key(&t, Key::End, 10);
        assert_eq!(m.selected(), Some(NodeId(6)));
        m.handle_key(&t, Key::PageUp, 2);
        assert_eq!(m.selected(), Some(NodeId(1)));
        m.handle_key(&t, Key::Home, 10);
        assert_eq!(m.selected(), Some(NodeId(0)));
        m.handle_key(&t, Key::PageDown, 100);
        assert_eq!(m.selected(), Some(NodeId(6)));
        m.handle_key(&t, Key::Up, 10);
        assert_eq!(m.selected(), Some(NodeId(4)));
    }

    #[test]
    fn window_and_scroll() {
        let t = sample();
        let mut m = RowModel::new(&t);
        m.expand(&t, NodeId(0));
        assert_eq!(m.visible_window(1, 2).len(), 2);
        assert_eq!(m.visible_window(3, 10).len(), 1);
        assert_eq!(m.visible_window(99, 10).len(), 0);
        m.select(NodeId(6)); // row index 3
        assert_eq!(m.scroll_to_selected(0, 2), 2);
        assert_eq!(m.scroll_to_selected(3, 2), 3);
        m.select(NodeId(0));
        assert_eq!(m.scroll_to_selected(2, 2), 0);
    }

    #[test]
    fn expandable_means_directory_with_children() {
        let t = sample();
        let m = RowModel::new(&t);
        assert!(m.is_expandable(&t, NodeId(0)));
        assert!(!m.is_expandable(&t, NodeId(6)));
    }
}
```

Replace `crates/plumb-core/src/lib.rs`:

```rust
//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod node;
pub mod rows;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use tree::{Tree, TreeBuilder, TreeError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: compile errors, `cannot find type RowModel` / `SortKey` / `Key`.

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-core/src/rows.rs`, above the `#[cfg(test)]` module:

```rust
use crate::node::{NodeFlags, NodeId};
use crate::tree::Tree;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Items,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub id: NodeId,
    pub depth: u16,
}

/// Flat list of the rows currently visible in the tree view, plus selection and sort state.
/// Every method that reads the tree takes it as a parameter; pass the same tree each time.
pub struct RowModel {
    expanded: Vec<u64>,
    rows: Vec<Row>,
    selected: Option<NodeId>,
    key: SortKey,
    desc: bool,
    /// Sorted child lists, keyed by parent (`NodeId::NONE` is the list of roots).
    cache: HashMap<NodeId, Rc<[NodeId]>>,
}

fn cmp_names(a: &[u16], b: &[u16]) -> Ordering {
    let lower = |s: &[u16]| {
        char::decode_utf16(s.iter().copied())
            .flat_map(|c| c.unwrap_or('\u{FFFD}').to_lowercase())
            .collect::<Vec<char>>()
    };
    lower(a).cmp(&lower(b))
}

impl RowModel {
    /// Starts with the roots visible and nothing expanded; sorted by size, largest first.
    pub fn new(tree: &Tree) -> Self {
        let mut m = Self {
            expanded: vec![0; tree.len().div_ceil(64)],
            rows: Vec::new(),
            selected: None,
            key: SortKey::Size,
            desc: true,
            cache: HashMap::new(),
        };
        m.rebuild(tree);
        m
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
    }

    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded[id.idx() / 64] >> (id.idx() % 64) & 1 == 1
    }

    fn set_expanded(&mut self, id: NodeId, on: bool) {
        let (w, b) = (id.idx() / 64, id.idx() % 64);
        if on {
            self.expanded[w] |= 1 << b;
        } else {
            self.expanded[w] &= !(1 << b);
        }
    }

    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool) {
        self.key = key;
        self.desc = desc;
        self.cache.clear();
        self.rebuild(tree);
    }

    fn row_index(&self, id: NodeId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected.and_then(|id| self.row_index(id))
    }

    fn sorted_children(&mut self, tree: &Tree, parent: NodeId) -> Rc<[NodeId]> {
        if let Some(c) = self.cache.get(&parent) {
            return Rc::clone(c);
        }
        let mut ids: Vec<NodeId> =
            if parent.is_none() { tree.roots.clone() } else { tree.children(parent).collect() };
        let (key, desc) = (self.key, self.desc);
        ids.sort_unstable_by(|&a, &b| {
            let o = match key {
                SortKey::Size => tree.subtree_size[a].cmp(&tree.subtree_size[b]),
                SortKey::Items => tree.subtree_files[a].cmp(&tree.subtree_files[b]),
                SortKey::Name => cmp_names(tree.name(a), tree.name(b)),
            };
            (if desc { o.reverse() } else { o }).then(a.cmp(&b))
        });
        let rc: Rc<[NodeId]> = ids.into();
        self.cache.insert(parent, Rc::clone(&rc));
        rc
    }

    fn push_children(&mut self, tree: &Tree, parent: NodeId, depth: u16, out: &mut Vec<Row>) {
        let kids = self.sorted_children(tree, parent);
        for &c in kids.iter() {
            out.push(Row { id: c, depth });
            if self.is_expanded(c) {
                self.push_children(tree, c, depth + 1, out);
            }
        }
    }

    fn rebuild(&mut self, tree: &Tree) {
        let mut rows = Vec::new();
        self.push_children(tree, NodeId::NONE, 0, &mut rows);
        self.rows = rows;
    }

    pub fn expand(&mut self, tree: &Tree, id: NodeId) {
        if self.is_expanded(id) || tree.first_child[id].is_none() {
            return;
        }
        self.set_expanded(id, true);
        if let Some(i) = self.row_index(id) {
            let depth = self.rows[i].depth;
            let mut new_rows = Vec::new();
            self.push_children(tree, id, depth + 1, &mut new_rows);
            self.rows.splice(i + 1..i + 1, new_rows);
        }
    }

    pub fn collapse(&mut self, id: NodeId) {
        if !self.is_expanded(id) {
            return;
        }
        self.set_expanded(id, false);
        if let Some(i) = self.row_index(id) {
            let depth = self.rows[i].depth;
            let end = self.rows[i + 1..]
                .iter()
                .position(|r| r.depth <= depth)
                .map_or(self.rows.len(), |p| i + 1 + p);
            self.rows.drain(i + 1..end);
        }
    }

    pub fn toggle(&mut self, tree: &Tree, id: NodeId) {
        if self.is_expanded(id) {
            self.collapse(id);
        } else {
            self.expand(tree, id);
        }
    }

    /// Moves the selection. `page` is the number of rows PageUp/PageDown move.
    pub fn handle_key(&mut self, tree: &Tree, key: Key, page: usize) {
        if self.rows.is_empty() {
            return;
        }
        let Some(cur) = self.selected_index() else {
            self.selected = Some(self.rows[0].id);
            return;
        };
        let last = self.rows.len() - 1;
        let id = self.rows[cur].id;
        let target = match key {
            Key::Up => cur.saturating_sub(1),
            Key::Down => (cur + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            Key::PageUp => cur.saturating_sub(page.max(1)),
            Key::PageDown => (cur + page.max(1)).min(last),
            Key::Right => {
                if tree.first_child[id].is_none() {
                    cur
                } else if !self.is_expanded(id) {
                    self.expand(tree, id);
                    cur
                } else {
                    (cur + 1).min(last)
                }
            }
            Key::Left => {
                if self.is_expanded(id) {
                    self.collapse(id);
                    cur
                } else {
                    let p = tree.parent[id];
                    if p.is_none() { cur } else { self.row_index(p).unwrap_or(cur) }
                }
            }
        };
        self.selected = Some(self.rows[target].id);
    }

    /// The rows to draw for a view `height` rows tall, scrolled to row `scroll`.
    pub fn visible_window(&self, scroll: usize, height: usize) -> &[Row] {
        let start = scroll.min(self.rows.len());
        &self.rows[start..(start + height).min(self.rows.len())]
    }

    /// The smallest scroll change that brings the selected row into view.
    pub fn scroll_to_selected(&self, scroll: usize, height: usize) -> usize {
        match self.selected_index() {
            Some(i) if i < scroll => i,
            Some(i) if height > 0 && i >= scroll + height => i + 1 - height,
            _ => scroll,
        }
    }

    /// True if `id` is a directory with at least one child (something Right can expand).
    pub fn is_expandable(&self, tree: &Tree, id: NodeId) -> bool {
        tree.flags[id].contains(NodeFlags::DIR) && !tree.first_child[id].is_none()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 19 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates
git commit -m "Add RowModel: expansion, sorting, selection and scrolling" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Synthetic inputs (`synth`)

**Files:**
- Create: `crates/plumb-core/src/synth.rs`
- Modify: `crates/plumb-core/src/lib.rs`

**Interfaces:**
- Consumes: `NodeId`, `NodeFlags`, `Tree`, `TreeBuilder`.
- Produces:
  - `RECORD_SIZE: usize` (1024).
  - `RecordBuilder` with constructors `file()` and `dir()`; chainable `extension_of(base_ref: u64)`, `file_name(parent_record: u64, name: &str, namespace: u8)`, `data_resident(len: usize)`, `data_nonresident(alloc: u64, real: u64)`, `data_nonresident_at(start_vcn: u64, alloc: u64, real: u64)`, `data_compressed(alloc: u64, real: u64, compressed: u64)`, `data_named_nonresident(name: &str, alloc: u64, real: u64)`, `reparse(tag: u32)`; terminal `build() -> Vec<u8>` (a 1024-byte record with update-sequence fixups applied). `file_name` stamps sequence number 1 into the parent reference.
  - `name_units(i: u32, buf: &mut [u16; 12]) -> &[u16]`, `kary_builder(n: u32, fanout: u32) -> TreeBuilder`, `flat_tree(children: u32) -> Tree`.

- [ ] **Step 1: Write the failing tests**

`crates/plumb-core/src/synth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_units_encodes_decimal() {
        let mut buf = [0u16; 12];
        assert_eq!(String::from_utf16_lossy(name_units(0, &mut buf)), "n0");
        assert_eq!(String::from_utf16_lossy(name_units(4_294_967_295, &mut buf)), "n4294967295");
    }

    #[test]
    fn kary_tree_is_valid_and_counts_files() {
        let t = kary_builder(1000, 4).finish().unwrap();
        let files = (0..1000).filter(|&i| !t.flags[NodeId(i)].contains(NodeFlags::DIR)).count() as u32;
        assert_eq!(t.subtree_files[NodeId(0)], files);
        assert_eq!(t.subtree_size[NodeId(0)], t.size.iter().sum::<u64>());
    }

    #[test]
    fn flat_tree_has_one_root_and_all_children() {
        let t = flat_tree(500);
        assert_eq!(t.len(), 501);
        assert_eq!(t.children(NodeId(0)).count(), 500);
    }

    #[test]
    fn records_are_one_kilobyte_with_fixup_stamps() {
        let r = RecordBuilder::file().build();
        assert_eq!(r.len(), RECORD_SIZE);
        assert_eq!(&r[0..4], b"FILE");
        assert_eq!(&r[510..512], &r[0x30..0x32]);
        assert_eq!(&r[1022..1024], &r[0x30..0x32]);
    }
}
```

Replace `crates/plumb-core/src/lib.rs`:

```rust
//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod node;
pub mod rows;
#[doc(hidden)]
pub mod synth;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use tree::{Tree, TreeBuilder, TreeError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: compile errors, `cannot find function name_units` / `RecordBuilder` / `kary_builder`.

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-core/src/synth.rs`, above the `#[cfg(test)]` module:

```rust
//! Synthetic inputs for tests and benchmarks: MFT records and big trees.

use crate::node::{NodeFlags, NodeId};
use crate::tree::{Tree, TreeBuilder};

pub const RECORD_SIZE: usize = 1024;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn resident_attr(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let value_off = align8(0x18 + 2 * units.len());
    let len = align8(value_off + value.len());
    let mut a = vec![0u8; len];
    put32(&mut a, 0, kind);
    put32(&mut a, 4, len as u32);
    a[9] = units.len() as u8;
    put16(&mut a, 10, 0x18);
    put32(&mut a, 0x10, value.len() as u32);
    put16(&mut a, 0x14, value_off as u16);
    for (i, u) in units.iter().enumerate() {
        put16(&mut a, 0x18 + 2 * i, *u);
    }
    a[value_off..value_off + value.len()].copy_from_slice(value);
    a
}

struct NonResident<'a> {
    name: &'a str,
    flags: u16,
    start_vcn: u64,
    compression_unit: u16,
    alloc: u64,
    real: u64,
    compressed: Option<u64>,
}

fn nonresident_data_attr(s: NonResident) -> Vec<u8> {
    let units: Vec<u16> = s.name.encode_utf16().collect();
    let header = if s.compressed.is_some() { 0x48 } else { 0x40 };
    let runlist_off = align8(header + 2 * units.len());
    let len = align8(runlist_off + 8);
    let mut a = vec![0u8; len];
    put32(&mut a, 0, 0x80);
    put32(&mut a, 4, len as u32);
    a[8] = 1;
    a[9] = units.len() as u8;
    put16(&mut a, 10, header as u16);
    put16(&mut a, 12, s.flags);
    put64(&mut a, 0x10, s.start_vcn);
    put16(&mut a, 0x20, runlist_off as u16);
    put16(&mut a, 0x22, s.compression_unit);
    put64(&mut a, 0x28, s.alloc);
    put64(&mut a, 0x30, s.real);
    put64(&mut a, 0x38, s.real);
    if let Some(c) = s.compressed {
        put64(&mut a, 0x40, c);
    }
    for (i, u) in units.iter().enumerate() {
        put16(&mut a, header + 2 * i, *u);
    }
    a
}

/// Builds a valid 1024-byte NTFS file record, update-sequence fixups included.
pub struct RecordBuilder {
    flags: u16,
    base: u64,
    attrs: Vec<Vec<u8>>,
}

impl RecordBuilder {
    pub fn file() -> Self {
        Self { flags: 1, base: 0, attrs: Vec::new() }
    }

    pub fn dir() -> Self {
        Self { flags: 3, base: 0, attrs: Vec::new() }
    }

    /// Marks this as an extension record of the record with file reference `base_ref`.
    pub fn extension_of(mut self, base_ref: u64) -> Self {
        self.base = base_ref;
        self
    }

    pub fn file_name(mut self, parent_record: u64, name: &str, namespace: u8) -> Self {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 0x42 + 2 * units.len()];
        put64(&mut v, 0, parent_record | (1 << 48));
        v[0x40] = units.len() as u8;
        v[0x41] = namespace;
        for (i, u) in units.iter().enumerate() {
            put16(&mut v, 0x42 + 2 * i, *u);
        }
        self.attrs.push(resident_attr(0x30, "", &v));
        self
    }

    pub fn data_resident(mut self, len: usize) -> Self {
        self.attrs.push(resident_attr(0x80, "", &vec![0u8; len]));
        self
    }

    pub fn data_nonresident(self, alloc: u64, real: u64) -> Self {
        self.data_nonresident_at(0, alloc, real)
    }

    pub fn data_nonresident_at(mut self, start_vcn: u64, alloc: u64, real: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name: "",
            flags: 0,
            start_vcn,
            compression_unit: 0,
            alloc,
            real,
            compressed: None,
        }));
        self
    }

    pub fn data_compressed(mut self, alloc: u64, real: u64, compressed: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name: "",
            flags: 0x0001,
            start_vcn: 0,
            compression_unit: 4,
            alloc,
            real,
            compressed: Some(compressed),
        }));
        self
    }

    pub fn data_named_nonresident(mut self, name: &str, alloc: u64, real: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name,
            flags: 0,
            start_vcn: 0,
            compression_unit: 0,
            alloc,
            real,
            compressed: None,
        }));
        self
    }

    pub fn reparse(mut self, tag: u32) -> Self {
        let mut v = vec![0u8; 8];
        put32(&mut v, 0, tag);
        self.attrs.push(resident_attr(0xC0, "", &v));
        self
    }

    pub fn build(self) -> Vec<u8> {
        let mut b = vec![0u8; RECORD_SIZE];
        b[0..4].copy_from_slice(b"FILE");
        put16(&mut b, 4, 0x30); // update sequence array offset
        put16(&mut b, 6, 3); // the update sequence number plus two sectors
        put16(&mut b, 0x10, 1);
        put16(&mut b, 0x14, 0x38); // first attribute
        put16(&mut b, 0x16, self.flags);
        put32(&mut b, 0x1C, RECORD_SIZE as u32);
        put64(&mut b, 0x20, self.base);
        let mut off = 0x38;
        for a in &self.attrs {
            b[off..off + a.len()].copy_from_slice(a);
            off += a.len();
        }
        put32(&mut b, off, 0xFFFF_FFFF);
        off += 8;
        assert!(off <= RECORD_SIZE, "attributes do not fit in one record");
        put32(&mut b, 0x18, off as u32);
        // Fixups: keep the last two bytes of each sector in the array and stamp the number there.
        let usn = [0x01u8, 0x00];
        b[0x30..0x32].copy_from_slice(&usn);
        for s in 0..2 {
            let end = (s + 1) * 512 - 2;
            let original = [b[end], b[end + 1]];
            b[0x32 + 2 * s..0x34 + 2 * s].copy_from_slice(&original);
            b[end..end + 2].copy_from_slice(&usn);
        }
        b
    }
}

/// The name `n<decimal>` as UTF-16 without allocating.
pub fn name_units(i: u32, buf: &mut [u16; 12]) -> &[u16] {
    let mut digits = [0u16; 10];
    let (mut n, mut k) = (i, 0);
    loop {
        digits[k] = b'0' as u16 + (n % 10) as u16;
        n /= 10;
        k += 1;
        if n == 0 {
            break;
        }
    }
    buf[0] = b'n' as u16;
    for j in 0..k {
        buf[1 + j] = digits[k - 1 - j];
    }
    &buf[..1 + k]
}

fn mix(i: u32) -> u64 {
    let mut x = i as u64 + 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A complete `fanout`-ary tree of `n` nodes: node `i`'s parent is `(i - 1) / fanout`.
/// Nodes with children are directories; the rest are files of pseudo-random size.
pub fn kary_builder(n: u32, fanout: u32) -> TreeBuilder {
    let mut b = TreeBuilder::with_capacity(n as usize, n as usize * 7);
    let mut buf = [0u16; 12];
    for i in 0..n {
        let parent = if i == 0 { NodeId::NONE } else { NodeId((i - 1) / fanout) };
        let is_dir = u64::from(i) * u64::from(fanout) + 1 < u64::from(n);
        let (size, flags) =
            if is_dir { (0, NodeFlags::DIR) } else { (mix(i) % (1 << 20), NodeFlags::empty()) };
        b.push(parent, name_units(i, &mut buf), size, flags);
    }
    b
}

/// One root directory with `children` files of pseudo-random size.
pub fn flat_tree(children: u32) -> Tree {
    let mut b = TreeBuilder::with_capacity(children as usize + 1, children as usize * 7);
    let mut buf = [0u16; 12];
    let root = b.push_str(NodeId::NONE, "root", 0, NodeFlags::DIR);
    for i in 0..children {
        b.push(root, name_units(i, &mut buf), mix(i) % (1 << 20), NodeFlags::empty());
    }
    b.finish().unwrap()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 23 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates
git commit -m "Add synthetic MFT record builder and large tree generators" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: MFT record parser

**Files:**
- Create: `crates/plumb-core/src/mft.rs`
- Modify: `crates/plumb-core/src/lib.rs`

**Interfaces:**
- Consumes: `synth::RecordBuilder` (Task 4; tests only).
- Produces: `parse_record(buf: &mut [u8], out: &mut ParsedRecord) -> Result<(), ParseError>` (undoes the fixups in place, so each buffer can be parsed once); `ParseError::{BadMagic, BadFixup, BadAttribute}`; `NameEntry { parent: u64 /* record number */, off: u32, len: u8, namespace: u8 }`; `ParsedRecord { in_use: bool, is_dir: bool, base: Option<u64>, names: Vec<NameEntry>, name_pool: Vec<u16>, data_alloc: Option<u64>, reparse_tag: Option<u32> }` with `clear()`, `name(&NameEntry) -> &[u16]`, `merge_extension(&ParsedRecord)`.

- [ ] **Step 1: Write the failing tests**

`crates/plumb-core/src/mft.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::RecordBuilder;

    fn parse(mut rec: Vec<u8>) -> (Result<(), ParseError>, ParsedRecord) {
        let mut p = ParsedRecord::default();
        let r = parse_record(&mut rec, &mut p);
        (r, p)
    }

    fn names(p: &ParsedRecord) -> Vec<String> {
        p.names.iter().map(|e| String::from_utf16_lossy(p.name(e))).collect()
    }

    #[test]
    fn plain_file() {
        let (r, p) = parse(RecordBuilder::file().file_name(5, "a.txt", 1).data_nonresident(4096, 100).build());
        assert_eq!(r, Ok(()));
        assert!(p.in_use && !p.is_dir);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.names[0].parent, 5);
        assert_eq!(p.data_alloc, Some(4096));
        assert_eq!(p.base, None);
    }

    #[test]
    fn directory_flag() {
        let (_, p) = parse(RecordBuilder::dir().file_name(5, "d", 1).build());
        assert!(p.is_dir);
        assert_eq!(p.data_alloc, None);
    }

    #[test]
    fn dos_only_names_are_dropped_win32_and_posix_kept() {
        let rec = RecordBuilder::file()
            .file_name(5, "PROGRA~1", 2)
            .file_name(5, "Program Files", 1)
            .file_name(6, "both", 3)
            .file_name(7, "posix", 0)
            .build();
        let (_, p) = parse(rec);
        assert_eq!(names(&p), vec!["Program Files", "both", "posix"]);
    }

    #[test]
    fn hard_links_give_one_name_per_parent() {
        let (_, p) = parse(RecordBuilder::file().file_name(5, "x", 1).file_name(9, "y", 1).build());
        assert_eq!(p.names.iter().map(|e| e.parent).collect::<Vec<_>>(), vec![5, 9]);
    }

    #[test]
    fn fixups_restore_bytes_at_the_sector_end() {
        // A 200-char name covers byte offset 510, where the fixup value is stored.
        let long = "x".repeat(200);
        let (r, p) = parse(RecordBuilder::file().file_name(5, &long, 1).build());
        assert_eq!(r, Ok(()));
        assert_eq!(names(&p), vec![long]);
    }

    #[test]
    fn mismatched_fixup_is_rejected() {
        let mut rec = RecordBuilder::file().file_name(5, "a", 1).build();
        rec[510] ^= 1;
        assert_eq!(parse(rec).0, Err(ParseError::BadFixup));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut rec = RecordBuilder::file().build();
        rec[0] = b'X';
        assert_eq!(parse(rec).0, Err(ParseError::BadMagic));
    }

    #[test]
    fn oversized_attribute_length_is_rejected() {
        let mut rec = RecordBuilder::file().file_name(5, "a", 1).build();
        rec[0x3C..0x40].copy_from_slice(&5000u32.to_le_bytes());
        assert_eq!(parse(rec).0, Err(ParseError::BadAttribute));
    }

    #[test]
    fn resident_data_uses_no_clusters() {
        let (_, p) = parse(RecordBuilder::file().data_resident(100).build());
        assert_eq!(p.data_alloc, Some(0));
    }

    #[test]
    fn compressed_streams_use_the_compressed_size() {
        let (_, p) = parse(RecordBuilder::file().data_compressed(65536, 100_000, 4096).build());
        assert_eq!(p.data_alloc, Some(4096));
    }

    #[test]
    fn later_extents_and_named_streams_are_ignored() {
        let rec = RecordBuilder::file()
            .data_nonresident_at(16, 8192, 8192)
            .data_named_nonresident("Zone.Identifier", 4096, 30)
            .build();
        assert_eq!(parse(rec).1.data_alloc, None);
    }

    #[test]
    fn reparse_tag_is_read() {
        let (_, p) = parse(RecordBuilder::dir().reparse(0xA000_000C).build());
        assert_eq!(p.reparse_tag, Some(0xA000_000C));
    }

    #[test]
    fn extension_records_merge_into_the_base() {
        let (_, mut base) = parse(RecordBuilder::file().file_name(2, "a", 1).build());
        let ext_ref = 5 | (3u64 << 48);
        let (_, ext) = parse(
            RecordBuilder::file().extension_of(ext_ref).file_name(8, "b", 1).data_nonresident(8192, 8000).build(),
        );
        assert_eq!(ext.base, Some(5));
        base.merge_extension(&ext);
        assert_eq!(names(&base), vec!["a", "b"]);
        assert_eq!(base.names[1].parent, 8);
        assert_eq!(base.data_alloc, Some(8192));
    }

    #[test]
    fn parsing_again_into_the_same_value_starts_clean() {
        let mut p = ParsedRecord::default();
        let mut a = RecordBuilder::file().file_name(5, "first", 1).data_nonresident(4096, 1).build();
        parse_record(&mut a, &mut p).unwrap();
        let mut b = RecordBuilder::dir().file_name(6, "second", 1).build();
        parse_record(&mut b, &mut p).unwrap();
        assert_eq!(names(&p), vec!["second"]);
        assert_eq!(p.data_alloc, None);
        assert!(p.is_dir);
    }
}
```

Replace `crates/plumb-core/src/lib.rs`:

```rust
//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod mft;
pub mod node;
pub mod rows;
#[doc(hidden)]
pub mod synth;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use tree::{Tree, TreeBuilder, TreeError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: compile errors, `cannot find function parse_record` / `ParsedRecord` / `ParseError`.

- [ ] **Step 3: Write the implementation**

Insert at the top of `crates/plumb-core/src/mft.rs`, above the `#[cfg(test)]` module:

```rust
//! NTFS `$MFT` file-record parser: bytes in, plain structs out. No I/O.

const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;
const NAMESPACE_DOS: u8 = 2;
/// Attribute flags: compressed (0x0001) or sparse (0x8000).
const COMPRESSED_OR_SPARSE: u16 = 0x8001;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    BadMagic,
    BadFixup,
    BadAttribute,
}

/// One `$FILE_NAME` attribute. The name itself lives in [`ParsedRecord::name_pool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameEntry {
    /// Record number of the parent directory (the 16-bit sequence part is dropped).
    pub parent: u64,
    pub off: u32,
    pub len: u8,
    pub namespace: u8,
}

/// What the scanner needs from one file record. Reuse one value across records: `parse_record`
/// clears it, so steady-state parsing does not allocate.
#[derive(Default, Debug)]
pub struct ParsedRecord {
    pub in_use: bool,
    pub is_dir: bool,
    /// Record number of the base record if this is an extension record.
    pub base: Option<u64>,
    /// Names in the Win32, POSIX and Win32+DOS namespaces (DOS-only 8.3 names are dropped).
    pub names: Vec<NameEntry>,
    pub name_pool: Vec<u16>,
    /// Allocated bytes of the unnamed `$DATA` stream. `None`: no unnamed stream in this record.
    /// `Some(0)`: resident data, which uses no clusters.
    pub data_alloc: Option<u64>,
    pub reparse_tag: Option<u32>,
}

impl ParsedRecord {
    pub fn clear(&mut self) {
        self.in_use = false;
        self.is_dir = false;
        self.base = None;
        self.names.clear();
        self.name_pool.clear();
        self.data_alloc = None;
        self.reparse_tag = None;
    }

    pub fn name(&self, e: &NameEntry) -> &[u16] {
        &self.name_pool[e.off as usize..e.off as usize + e.len as usize]
    }

    /// Folds an extension record into its base record.
    pub fn merge_extension(&mut self, ext: &ParsedRecord) {
        let shift = self.name_pool.len() as u32;
        self.name_pool.extend_from_slice(&ext.name_pool);
        self.names.extend(ext.names.iter().map(|e| NameEntry { off: e.off + shift, ..*e }));
        if self.data_alloc.is_none() {
            self.data_alloc = ext.data_alloc;
        }
        if self.reparse_tag.is_none() {
            self.reparse_tag = ext.reparse_tag;
        }
    }
}

fn rd<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N], ParseError> {
    b.get(o..o + N).and_then(|s| s.try_into().ok()).ok_or(ParseError::BadAttribute)
}
fn u16_at(b: &[u8], o: usize) -> Result<u16, ParseError> {
    Ok(u16::from_le_bytes(rd(b, o)?))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, ParseError> {
    Ok(u32::from_le_bytes(rd(b, o)?))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, ParseError> {
    Ok(u64::from_le_bytes(rd(b, o)?))
}

/// The value bytes of a resident attribute `a`.
fn resident_value(a: &[u8]) -> Result<&[u8], ParseError> {
    let len = u32_at(a, 0x10)? as usize;
    let off = u16_at(a, 0x14)? as usize;
    a.get(off..off + len).ok_or(ParseError::BadAttribute)
}

/// Parses one file record. `buf` must be exactly one record (normally 1024 bytes). The
/// update-sequence fixups are undone in place, so a buffer can be parsed only once.
pub fn parse_record(buf: &mut [u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    out.clear();
    if buf.get(0..4) != Some(b"FILE") {
        return Err(ParseError::BadMagic);
    }
    let usa_off = u16_at(buf, 4)? as usize;
    let usa_count = u16_at(buf, 6)? as usize;
    if usa_count == 0 || usa_off + 2 * usa_count > buf.len() || (usa_count - 1) * 512 > buf.len() {
        return Err(ParseError::BadFixup);
    }
    let usn = [buf[usa_off], buf[usa_off + 1]];
    for i in 1..usa_count {
        let end = i * 512 - 2;
        if buf[end..end + 2] != usn {
            return Err(ParseError::BadFixup);
        }
        buf[end] = buf[usa_off + 2 * i];
        buf[end + 1] = buf[usa_off + 2 * i + 1];
    }

    let flags = u16_at(buf, 0x16)?;
    out.in_use = flags & 1 != 0;
    out.is_dir = flags & 2 != 0;
    let base = u64_at(buf, 0x20)?;
    out.base = (base != 0).then_some(base & REF_MASK);

    let used = (u32_at(buf, 0x18)? as usize).min(buf.len());
    let mut off = u16_at(buf, 0x14)? as usize;
    while off + 8 <= used {
        let kind = u32_at(buf, off)?;
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(buf, off + 4)? as usize;
        if len < 0x18 || len % 8 != 0 || off + len > used {
            return Err(ParseError::BadAttribute);
        }
        let a = &buf[off..off + len];
        match kind {
            ATTR_FILE_NAME => parse_file_name(a, out)?,
            ATTR_DATA => parse_data(a, out)?,
            ATTR_REPARSE_POINT => parse_reparse(a, out)?,
            _ => {}
        }
        off += len;
    }
    Ok(())
}

fn parse_file_name(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] != 0 {
        return Ok(()); // $FILE_NAME is always resident; ignore anything else
    }
    let v = resident_value(a)?;
    if v.len() < 0x42 {
        return Err(ParseError::BadAttribute);
    }
    let namespace = v[0x41];
    if namespace == NAMESPACE_DOS {
        return Ok(());
    }
    let n = v[0x40] as usize;
    let chars = v.get(0x42..0x42 + 2 * n).ok_or(ParseError::BadAttribute)?;
    let off = out.name_pool.len() as u32;
    out.name_pool.extend(chars.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])));
    out.names.push(NameEntry { parent: u64_at(v, 0)? & REF_MASK, off, len: n as u8, namespace });
    Ok(())
}

fn parse_data(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[9] != 0 {
        return Ok(()); // named stream (alternate data stream): ignored in v1
    }
    if a[8] == 0 {
        out.data_alloc.get_or_insert(0);
        return Ok(());
    }
    if u64_at(a, 0x10)? != 0 {
        return Ok(()); // a later extent; the first extent carries the total sizes
    }
    let flags = u16_at(a, 0x0C)?;
    let compression_unit = u16_at(a, 0x22)?;
    let alloc = if flags & COMPRESSED_OR_SPARSE != 0 && compression_unit != 0 {
        u64_at(a, 0x40)? // compressed size: what is actually allocated
    } else {
        u64_at(a, 0x28)?
    };
    out.data_alloc = Some(alloc);
    Ok(())
}

fn parse_reparse(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] == 0 {
        out.reparse_tag = Some(u32_at(resident_value(a)?, 0)?);
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core --lib`
Expected: `test result: ok. 37 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates
git commit -m "Add NTFS MFT file record parser" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Criterion benchmarks and baseline

**Files:**
- Modify: `crates/plumb-core/Cargo.toml`
- Create: `crates/plumb-core/benches/core.rs`, `docs/benchmarks.md`

**Interfaces:**
- Consumes: `parse_record`, `ParsedRecord`, `RowModel`, `synth::{RECORD_SIZE, RecordBuilder, flat_tree, kary_builder}`, `TreeBuilder::finish`.
- Produces: benchmark ids `mft/parse_1024_records`, `tree/finish_5m_nodes`, `rows/expand_1m_children_sort_by_size`, `rows/visible_window_of_50`.

- [ ] **Step 1: Add the dependency and the benchmark file**

Append to `crates/plumb-core/Cargo.toml`:

```toml

[dev-dependencies]
criterion = { version = "0.8", default-features = false, features = ["cargo_bench_support"] }

[[bench]]
name = "core"
harness = false
```

`crates/plumb-core/benches/core.rs`:

```rust
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use plumb_core::NodeId;
use plumb_core::mft::{ParsedRecord, parse_record};
use plumb_core::rows::RowModel;
use plumb_core::synth::{RECORD_SIZE, RecordBuilder, flat_tree, kary_builder};
use std::hint::black_box;

/// 1024 records, each with two names and a data stream, back to back (1 MiB).
fn record_chunk() -> Vec<u8> {
    let one = RecordBuilder::file()
        .file_name(5, "document-with-a-typical-name.txt", 1)
        .file_name(5, "DOCUME~1.TXT", 2)
        .data_nonresident(1 << 20, 1 << 20)
        .build();
    one.repeat(1024)
}

fn bench_parse(c: &mut Criterion) {
    let chunk = record_chunk();
    let mut g = c.benchmark_group("mft");
    g.throughput(Throughput::Bytes(chunk.len() as u64));
    g.bench_function("parse_1024_records", |b| {
        let mut out = ParsedRecord::default();
        b.iter_batched_ref(
            || chunk.clone(),
            |buf| {
                for rec in buf.chunks_exact_mut(RECORD_SIZE) {
                    parse_record(rec, &mut out).unwrap();
                    black_box(&out);
                }
            },
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn bench_sums(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree");
    g.sample_size(10);
    g.bench_function("finish_5m_nodes", |b| {
        b.iter_batched(
            || kary_builder(5_000_000, 8),
            |builder| black_box(builder.finish().unwrap()),
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn bench_rows(c: &mut Criterion) {
    let tree = flat_tree(1_000_000);
    let mut g = c.benchmark_group("rows");
    g.sample_size(10);
    g.bench_function("expand_1m_children_sort_by_size", |b| {
        b.iter_batched_ref(
            || RowModel::new(&tree),
            |m| {
                m.expand(&tree, NodeId(0));
                black_box(m.rows().len());
            },
            BatchSize::LargeInput,
        );
    });
    let mut expanded = RowModel::new(&tree);
    expanded.expand(&tree, NodeId(0));
    g.bench_function("visible_window_of_50", |b| {
        b.iter(|| black_box(expanded.visible_window(black_box(500_000), 50).len()));
    });
    g.finish();
}

criterion_group!(benches, bench_parse, bench_sums, bench_rows);
criterion_main!(benches);
```

- [ ] **Step 2: Smoke-run every benchmark once**

Run: `cargo bench -j 4 -p plumb-core --bench core -- --test`
Expected: four lines of `Testing <id>` each followed by `Success`.

- [ ] **Step 3: Check the machine is quiet, then run for real**

Run (PowerShell): `(Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 2 -MaxSamples 2).CounterSamples[-1].CookedValue; (Get-MpComputerStatus).RealTimeProtectionEnabled`
Expected: CPU under about 20 and `False`. If not, wait or report the load beside the numbers.

Run: `cargo bench -j 4 -p plumb-core --bench core`
Expected: criterion prints a time (and for `mft/parse_1024_records`, a throughput in MiB/s or GiB/s) for each of the four benchmarks. The 5M-node benchmark takes a few minutes, mostly in setup, which criterion excludes from the timing.

- [ ] **Step 4: Record the baseline**

Create `docs/benchmarks.md` with the real numbers from Step 3:

```markdown
# Benchmarks

Criterion benchmarks for the CPU hot paths: `cargo bench -p plumb-core --bench core`.
Run them only when CPU load is under ~20%; write the load beside the numbers.

## Baseline <date>

Machine: Ryzen 7 7840U laptop. CPU load during the run: <percent>. Defender real-time protection: <True/False>.

| Benchmark | Time | Throughput |
|---|---|---|
| mft/parse_1024_records | <time> | <MiB/s> |
| tree/finish_5m_nodes | <time> | |
| rows/expand_1m_children_sort_by_size | <time> | |
| rows/visible_window_of_50 | <time> | |
```

Replace each `<...>` with the measured value.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates docs
git commit -m "Add criterion benchmarks for parser, tree sums and row model" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage** (Plan 1 scope only):
- Tree fields, flags, `NodeId` newtype with `NONE` and `Index`, `bitflags`: Tasks 1-2.
- Size accounting: bottom-up sums, files count: Task 2; compressed size, resident = 0, DOS names dropped, named streams ignored, later extents ignored, reparse tag read, extension merge: Task 5. Hard-link "counted once" and parent resolution by record number are scanner merge-pass work (Plan 2).
- Row model: expansion bitset, flat row list with depths, sort by Name/Size/Items (default Size descending), per-folder sort cache, keyboard navigation, visible window, scroll to selection: Task 3.
- Testing section: MFT record unit tests (fixups, extension records, DOS names, hard links): Task 5; tree sums and row model tests: Tasks 2-3; the four criterion benchmarks and the load/Defender check: Task 6.
- Deferred to Plans 2-3: scanners, `scan-compare`, WARP render smoke test, window, drawing.

**Placeholder scan:** none. The `<...>` fields in `docs/benchmarks.md` are filled from the real run in Task 6, Step 4.

**Type consistency:** `NodeId`, `NodeFlags`, `Tree`, `TreeBuilder::{push, push_str, finish, with_capacity}`, `RowModel` methods and `RecordBuilder` methods are used with the same names and signatures across the tasks. Test counts add up: 3 + 6 + 10 + 4 + 14 = 37.
