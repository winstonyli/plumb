---
status: done
---

# plumb live updates, plan 1: core mutation and `RowModel::refresh`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `plumb-core` can change a finished tree in place (add, resize, rename, remove, reparent) and `RowModel::refresh` brings the visible rows up to date while keeping the selected row at the same screen row.

**Architecture:** The tree stays struct-of-arrays and append-only: new nodes are appended, removed nodes get `NodeFlags::DEAD`, `NodeId`s never change. Every mutation keeps `subtree_size` / `subtree_files` correct up the parent chain and records the nodes whose child list changed in a `touched` list. `RowModel::refresh` consumes that list, drops the stale sorted-children caches, rebuilds rows and returns the new scroll. No Win32 code is involved.

**Tech Stack:** Rust nightly (edition 2024), `bitflags`, `criterion`. Crate: `plumb-core` only.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md` (sections "Tree changes" and "Row model"). This is plan 1 of 3; plans 2 (journal reader) and 3 (UI wiring) come later.

## Global Constraints

- Tree mutations never change an existing `NodeId`, and never reuse a slot (no free list).
- `subtree_size` and `subtree_files` of every live node always equal what a fresh build of the live nodes would give.
- `Tree::children` never yields dead nodes.
- `RowModel::refresh` is the only supported way to bring a model up to date after mutations; it clamps the returned scroll to the row count but not to the window height (the caller does that).
- Deviation from the spec: `refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize` takes the tree mutably and consumes `tree.take_touched()` itself, instead of taking a `touched: &[NodeId]` slice. This keeps the touched list private to `Tree` and makes it impossible to forget a node.
- Files in the working copy have CRLF endings (`core.autocrlf=true`). Apply patches with `git apply --ignore-whitespace`; new files may be written with LF (git normalizes on commit).
- Use a separate target dir if a user-run `plumb.exe` locks `target\debug`: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3`.

---

## File map

| File | Change |
|---|---|
| `crates/plumb-core/src/node.rs` | add `NodeFlags::DEAD` |
| `crates/plumb-core/src/tree.rs` | `dead`/`touched` fields; `children` skips dead; `has_children`, `dead`, `take_touched`, `add_child`, `set_size`, `rename`, `remove`, `reparent` |
| `crates/plumb-core/src/tree/mutation_tests.rs` | new: 8 tests incl. a 3000-step randomized oracle comparison |
| `crates/plumb-core/src/rows.rs` | `has_children` replaces `first_child.is_none()`; `set_expanded` grows the bitset; `refresh` |
| `crates/plumb-core/src/rows/refresh_tests.rs` | new: 7 tests incl. refresh-versus-fresh-model after random mutations |
| `crates/plumb-core/benches/core.rs` | benchmarks: 100k deltas on 5M nodes; `refresh` on 1M children |
| `docs/benchmarks.md`, `CHANGELOG.md`, spec "Plans" | record results and the API deviation |

---

### Task 1: Tree mutation API

**Files:**
- Modify: `crates/plumb-core/src/node.rs`, `crates/plumb-core/src/tree.rs`
- Create: `crates/plumb-core/src/tree/mutation_tests.rs`

**Interfaces:**
- Produces (on `Tree`):
  - `fn has_children(&self, id: NodeId) -> bool`
  - `fn dead(&self) -> usize`
  - `fn take_touched(&mut self) -> Vec<NodeId>`
  - `fn add_child(&mut self, parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId`
  - `fn set_size(&mut self, id: NodeId, new: u64)`
  - `fn rename(&mut self, id: NodeId, name: &[u16])`
  - `fn remove(&mut self, id: NodeId) -> bool` (false for roots and already-dead nodes)
  - `fn reparent(&mut self, id: NodeId, new_parent: NodeId) -> bool` (false for roots, dead nodes, cycles; true and untouched if the parent is unchanged)
- Consumes: `Tree` internals, `tests::sample()` (nodes: 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)).

- [ ] **Step 1: Write the failing tests.** Create `crates/plumb-core/src/tree/mutation_tests.rs` with exactly:

```rust
use super::tests::sample;
use super::*;
use std::collections::BTreeMap;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn kids(t: &Tree, id: u32) -> Vec<u32> {
    t.children(NodeId(id)).map(|c| c.0).collect()
}

/// Path -> (subtree_size, subtree_files) for every live node.
fn summary(t: &Tree) -> BTreeMap<String, (u64, u32)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<NodeId> = t.roots.clone();
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.subtree_size[n], t.subtree_files[n]));
        stack.extend(t.children(n));
    }
    out
}

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)

#[test]
fn add_child_links_at_the_head_and_updates_every_ancestor() {
    let mut t = sample();
    let h = t.add_child(NodeId(4), &u("h"), 25, NodeFlags::empty());
    assert_eq!(h, NodeId(7));
    assert_eq!(kids(&t, 4), vec![7, 5]);
    assert_eq!((t.subtree_size[NodeId(4)], t.subtree_files[NodeId(4)]), (175, 2));
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (625, 5));
    assert_eq!(t.name_string(h), "h");
    assert_eq!(t.take_touched(), vec![h]);
    assert!(t.take_touched().is_empty());
}

#[test]
fn a_new_directory_adds_no_files_until_it_gets_some() {
    let mut t = sample();
    let d = t.add_child(NodeId(0), &u("d"), 0, NodeFlags::DIR);
    assert_eq!(t.subtree_files[NodeId(0)], 4);
    t.add_child(d, &u("x"), 10, NodeFlags::empty());
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (610, 5));
}

#[test]
fn set_size_goes_up_and_down() {
    let mut t = sample();
    t.set_size(NodeId(3), 500);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(0)]), (600, 800));
    t.set_size(NodeId(3), 100);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(0)]), (200, 400));
    assert_eq!(t.size[NodeId(3)], 100);
}

#[test]
fn remove_takes_the_subtree_out_and_children_skip_it() {
    let mut t = sample();
    assert!(t.remove(NodeId(1)));
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (200, 2));
    assert_eq!(kids(&t, 0), vec![4, 6]);
    assert_eq!(t.dead(), 3);
    assert!(t.flags[NodeId(2)].contains(NodeFlags::DEAD));
    assert!(!t.remove(NodeId(1)), "already dead");
    assert!(!t.remove(NodeId(0)), "roots cannot be removed");
    t.set_size(NodeId(2), 999);
    assert_eq!(t.subtree_size[NodeId(0)], 200, "dead nodes ignore set_size");
    assert!(!t.has_children(NodeId(2)));
}

#[test]
fn rename_changes_the_name_only() {
    let mut t = sample();
    t.rename(NodeId(6), &u("zzz"));
    assert_eq!(t.name_string(NodeId(6)), "zzz");
    assert_eq!(t.path(NodeId(6)), "C:\\zzz");
    assert_eq!(t.subtree_size[NodeId(0)], 600);
}

#[test]
fn reparent_moves_the_subtree_and_both_sides_add_up() {
    let mut t = sample();
    assert!(t.reparent(NodeId(2), NodeId(4))); // f1 (100) from big to mid
    assert_eq!(kids(&t, 1), vec![3]);
    assert_eq!(kids(&t, 4), vec![2, 5]);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(4)]), (300, 250));
    assert_eq!(t.subtree_size[NodeId(0)], 600);
    assert_eq!(t.path(NodeId(2)), "C:\\mid\\f1");
    // a middle child: children(0) is [1, 4, 6]; move 4 under 1
    assert!(t.reparent(NodeId(4), NodeId(1)));
    assert_eq!(kids(&t, 0), vec![1, 6]);
    assert_eq!(t.subtree_size[NodeId(1)], 550);
    assert_eq!(t.subtree_size[NodeId(0)], 600);
}

#[test]
fn reparent_refuses_cycles_roots_and_dead_nodes() {
    let mut t = sample();
    assert!(!t.reparent(NodeId(1), NodeId(1)));
    assert!(!t.reparent(NodeId(1), NodeId(2)), "into its own subtree");
    assert!(!t.reparent(NodeId(0), NodeId(1)), "roots stay roots");
    t.remove(NodeId(4));
    t.take_touched();
    assert!(!t.reparent(NodeId(6), NodeId(4)), "into a dead node");
    assert!(!t.reparent(NodeId(5), NodeId(0)), "a dead node");
    assert!(t.reparent(NodeId(6), NodeId(0)), "same parent is a no-op that succeeds");
    assert!(t.take_touched().is_empty(), "refusals and no-ops touch nothing");
}

struct Oracle {
    parent: Vec<Option<usize>>,
    name: Vec<String>,
    size: Vec<u64>,
    dir: Vec<bool>,
    alive: Vec<bool>,
}

impl Oracle {
    fn under(&self, mut i: usize, anc: usize) -> bool {
        loop {
            if i == anc {
                return true;
            }
            match self.parent[i] {
                Some(p) => i = p,
                None => return false,
            }
        }
    }

    fn live(&self, pred: impl Fn(usize) -> bool) -> Vec<usize> {
        (0..self.alive.len()).filter(|&i| self.alive[i] && pred(i)).collect()
    }

    /// The tree the live nodes would have if it had been scanned fresh.
    fn fresh(&self) -> Tree {
        let mut map = vec![NodeId::NONE; self.alive.len()];
        let mut next = 0;
        for i in 0..map.len() {
            if self.alive[i] {
                map[i] = NodeId(next);
                next += 1;
            }
        }
        let mut b = TreeBuilder::new();
        for i in self.live(|_| true) {
            let p = self.parent[i].map_or(NodeId::NONE, |p| map[p]);
            let flags = if self.dir[i] { NodeFlags::DIR } else { NodeFlags::empty() };
            b.push_str(p, &self.name[i], self.size[i], flags);
        }
        b.finish().unwrap()
    }
}

#[test]
fn random_mutations_match_a_fresh_build() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let mut b = TreeBuilder::new();
    b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
    let mut t = b.finish().unwrap();
    let mut o = Oracle {
        parent: vec![None],
        name: vec!["r".into()],
        size: vec![0],
        dir: vec![true],
        alive: vec![true],
    };
    for step in 0..3000 {
        let pick = rnd(100);
        if pick < 35 {
            let dirs = o.live(|i| o.dir[i]);
            let p = dirs[rnd(dirs.len())];
            let is_dir = rnd(4) == 0;
            let size = if is_dir { 0 } else { rnd(1000) as u64 };
            let name = format!("n{step}");
            let flags = if is_dir { NodeFlags::DIR } else { NodeFlags::empty() };
            let id = t.add_child(NodeId(p as u32), &u(&name), size, flags);
            assert_eq!(id.idx(), o.alive.len());
            o.parent.push(Some(p));
            o.name.push(name);
            o.size.push(size);
            o.dir.push(is_dir);
            o.alive.push(true);
        } else if pick < 60 {
            let files = o.live(|i| !o.dir[i]);
            if !files.is_empty() {
                let i = files[rnd(files.len())];
                let size = rnd(1000) as u64;
                t.set_size(NodeId(i as u32), size);
                o.size[i] = size;
            }
        } else if pick < 70 {
            let cand = o.live(|i| i != 0);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                let name = format!("m{step}");
                t.rename(NodeId(i as u32), &u(&name));
                o.name[i] = name;
            }
        } else if pick < 85 {
            let cand = o.live(|i| i != 0);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                assert!(t.remove(NodeId(i as u32)));
                for j in 0..o.alive.len() {
                    if o.alive[j] && o.under(j, i) {
                        o.alive[j] = false;
                    }
                }
            }
        } else {
            let cand = o.live(|i| i != 0);
            let dirs = o.live(|i| o.dir[i]);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                let y = dirs[rnd(dirs.len())];
                let legal = !o.under(y, i);
                assert_eq!(t.reparent(NodeId(i as u32), NodeId(y as u32)), legal, "step {step}");
                if legal {
                    o.parent[i] = Some(y);
                }
            }
        }
        if step % 250 == 0 || step == 2999 {
            assert_eq!(summary(&t), summary(&o.fresh()), "after step {step}");
        }
    }
    assert_eq!(t.dead(), o.alive.iter().filter(|a| !**a).count());
}
```

- [ ] **Step 2: Apply the declaration and confirm the failure.** The test module is declared at the end of `tree.rs` by the patch in Step 3; for now append `#[cfg(test)]\nmod mutation_tests;` to `tree.rs`, then:

Run: `cargo test -p plumb-core --lib mutation`
Expected: FAIL to compile (`no method named add_child`, `DEAD` missing).

- [ ] **Step 3: Implement.** Remove the two lines added in Step 2 (the patch below re-adds them), then apply both patches (`git apply --ignore-whitespace <file>`, or edit by hand to match):

`node.rs`:

```diff
diff --git a/crates/plumb-core/src/node.rs b/crates/plumb-core/src/node.rs
index 1805ca7..88dc4fe 100644
--- a/crates/plumb-core/src/node.rs
+++ b/crates/plumb-core/src/node.rs
@@ -40,6 +40,8 @@ bitflags! {
         const REPARSE = 2;
         const HARDLINK = 4;
         const DENIED = 8;
+        /// Removed after the scan; the slot stays so `NodeId`s never change.
+        const DEAD = 16;
     }
 }
 
```

`tree.rs`:

```diff
diff --git a/crates/plumb-core/src/tree.rs b/crates/plumb-core/src/tree.rs
index f8c3b82..7dc2d7a 100644
--- a/crates/plumb-core/src/tree.rs
+++ b/crates/plumb-core/src/tree.rs
@@ -24,6 +24,11 @@ pub struct Tree {
     name_off: Vec<u32>,
     name_len: Vec<u16>,
     pool: Vec<u16>,
+    /// Nodes marked [`NodeFlags::DEAD`] so far.
+    dead: usize,
+    /// Nodes whose size, name, parent or existence changed (and old parents) since the last
+    /// [`Tree::take_touched`].
+    touched: Vec<NodeId>,
 }
 
 impl Tree {
@@ -45,10 +50,16 @@ impl Tree {
         String::from_utf16_lossy(self.name(id))
     }
 
+    /// Live children of `id`, in list order.
     pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
         std::iter::successors(Some(self.first_child[id]).filter(|c| !c.is_none()), |&c| {
             Some(self.next_sibling[c]).filter(|n| !n.is_none())
         })
+        .filter(|&c| !self.flags[c].contains(NodeFlags::DEAD))
+    }
+
+    pub fn has_children(&self, id: NodeId) -> bool {
+        self.children(id).next().is_some()
     }
 
     /// Full path, built by walking parent links (no per-node path storage).
@@ -62,6 +73,129 @@ impl Tree {
         parts.reverse();
         parts.join("\\")
     }
+
+    /// Nodes marked dead so far (slots not yet reclaimed).
+    pub fn dead(&self) -> usize {
+        self.dead
+    }
+
+    /// Ids changed since the last call; pass them to `RowModel::refresh`.
+    pub fn take_touched(&mut self) -> Vec<NodeId> {
+        std::mem::take(&mut self.touched)
+    }
+
+    /// Adds (or subtracts) `size` bytes and `files` files to `from` and every ancestor.
+    fn bump(&mut self, mut from: NodeId, size: u64, files: u32, add: bool) {
+        while !from.is_none() {
+            if add {
+                self.subtree_size[from] += size;
+                self.subtree_files[from] += files;
+            } else {
+                self.subtree_size[from] -= size;
+                self.subtree_files[from] -= files;
+            }
+            from = self.parent[from];
+        }
+    }
+
+    /// Appends a new node under `parent` (at the head of its child list) and returns its id.
+    pub fn add_child(&mut self, parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId {
+        let id = NodeId(self.parent.len() as u32);
+        let files = u32::from(!flags.contains(NodeFlags::DIR));
+        self.parent.push(parent);
+        self.first_child.push(NodeId::NONE);
+        self.next_sibling.push(self.first_child[parent]);
+        self.first_child[parent] = id;
+        self.size.push(size);
+        self.subtree_size.push(size);
+        self.subtree_files.push(files);
+        self.flags.push(flags);
+        self.name_off.push(self.pool.len() as u32);
+        self.name_len.push(name.len() as u16);
+        self.pool.extend_from_slice(name);
+        self.bump(parent, size, files, true);
+        self.touched.push(id);
+        id
+    }
+
+    /// Changes a node's own size and fixes the sums above it. Ignored for dead nodes.
+    pub fn set_size(&mut self, id: NodeId, new: u64) {
+        if self.flags[id].contains(NodeFlags::DEAD) {
+            return;
+        }
+        let old = std::mem::replace(&mut self.size[id], new);
+        if new >= old {
+            self.bump(id, new - old, 0, true);
+        } else {
+            self.bump(id, old - new, 0, false);
+        }
+        self.touched.push(id);
+    }
+
+    /// Gives a node a new name (the old bytes stay in the pool as waste).
+    pub fn rename(&mut self, id: NodeId, name: &[u16]) {
+        self.name_off[id] = self.pool.len() as u32;
+        self.name_len[id] = name.len() as u16;
+        self.pool.extend_from_slice(name);
+        self.touched.push(id);
+    }
+
+    /// Marks `id` and everything under it dead and takes its sizes out of its ancestors.
+    /// False (and no change) for a root or an already dead node.
+    pub fn remove(&mut self, id: NodeId) -> bool {
+        let parent = self.parent[id];
+        if parent.is_none() || self.flags[id].contains(NodeFlags::DEAD) {
+            return false;
+        }
+        let (size, files) = (self.subtree_size[id], self.subtree_files[id]);
+        self.bump(parent, size, files, false);
+        let mut stack = vec![id];
+        while let Some(n) = stack.pop() {
+            stack.extend(self.children(n));
+            self.flags[n] |= NodeFlags::DEAD;
+            self.dead += 1;
+        }
+        self.touched.push(id);
+        true
+    }
+
+    /// Moves `id` (with its whole subtree) under `new_parent`. False (and no change) if `id` is a
+    /// root or dead, `new_parent` is dead, or `new_parent` is `id` or inside it.
+    pub fn reparent(&mut self, id: NodeId, new_parent: NodeId) -> bool {
+        let old = self.parent[id];
+        let dead = |t: &Self, n: NodeId| t.flags[n].contains(NodeFlags::DEAD);
+        if old.is_none() || dead(self, id) || dead(self, new_parent) {
+            return false;
+        }
+        let mut up = new_parent;
+        while !up.is_none() {
+            if up == id {
+                return false;
+            }
+            up = self.parent[up];
+        }
+        if old == new_parent {
+            return true;
+        }
+        if self.first_child[old] == id {
+            self.first_child[old] = self.next_sibling[id];
+        } else {
+            let mut prev = self.first_child[old];
+            while self.next_sibling[prev] != id {
+                prev = self.next_sibling[prev];
+            }
+            self.next_sibling[prev] = self.next_sibling[id];
+        }
+        let (size, files) = (self.subtree_size[id], self.subtree_files[id]);
+        self.bump(old, size, files, false);
+        self.parent[id] = new_parent;
+        self.next_sibling[id] = self.first_child[new_parent];
+        self.first_child[new_parent] = id;
+        self.bump(new_parent, size, files, true);
+        self.touched.push(old);
+        self.touched.push(id);
+        true
+    }
 }
 
 /// Collects nodes in any order. A node's parent may be added after the node.
@@ -180,6 +314,8 @@ impl TreeBuilder {
             name_off: self.name_off,
             name_len: self.name_len,
             pool: self.pool,
+            dead: 0,
+            touched: Vec::new(),
         })
     }
 }
@@ -264,3 +400,6 @@ pub(crate) mod tests {
         assert_eq!(b.finish().err(), Some(TreeError::Cycle));
     }
 }
+
+#[cfg(test)]
+mod mutation_tests;
```

Notes for the reader of the patch: `bump` walks the parent chain adding or subtracting a size and file count; `remove` marks the whole subtree dead and subtracts once at the top; `reparent` refuses cycles by walking up from the new parent; moving a node to its current parent is a successful no-op that leaves `touched` empty.

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p plumb-core --lib`
Expected: PASS, no warnings (the existing tests plus 8 new).

- [ ] **Step 5: Mutation-check the oracle test.** Temporarily delete the line `self.bump(old, size, files, false);` in `reparent`, run `cargo test -p plumb-core --lib`, expect `reparent_moves_the_subtree_and_both_sides_add_up` and `random_mutations_match_a_fresh_build` to FAIL, then restore the line with `git checkout`-free manual undo and rerun to PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb-core/src/node.rs crates/plumb-core/src/tree.rs crates/plumb-core/src/tree/mutation_tests.rs
git commit -m "Add in-place tree mutation: add_child, set_size, rename, remove, reparent"
```

---

### Task 2: `RowModel::refresh` with anchoring

**Files:**
- Modify: `crates/plumb-core/src/rows.rs`
- Create: `crates/plumb-core/src/rows/refresh_tests.rs`

**Interfaces:**
- Consumes: Task 1 (`has_children`, `take_touched`, `remove`, `reparent`, `add_child`, `DEAD`), `synth::{flat_tree, kary_builder}`.
- Produces: `RowModel::refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize`.

- [ ] **Step 1: Write the failing tests.** Create `crates/plumb-core/src/rows/refresh_tests.rs` with exactly:

```rust
use super::*;
use crate::synth::{flat_tree, kary_builder};
use crate::tree::tests::sample;

fn ids(m: &RowModel) -> Vec<u32> {
    m.rows().iter().map(|r| r.id.0).collect()
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Expands every directory that has children, until nothing more expands.
fn expand_all(m: &mut RowModel, t: &Tree) {
    while let Some(id) = m.rows().iter().map(|r| r.id).find(|&id| m.is_expandable(t, id) && !m.is_expanded(id)) {
        m.expand(t, id);
    }
}

#[test]
fn adding_a_big_file_keeps_the_selected_row_on_screen_where_it_was() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let x = m.rows()[50].id;
    m.select(x);
    t.add_child(NodeId(0), &u("huge"), 1 << 40, NodeFlags::empty());
    let scroll = m.refresh(&mut t, 40); // selected was at screen row 10
    assert_eq!(m.rows()[1].id, NodeId(101), "the big file sorts first");
    assert_eq!(m.selected_index(), Some(51));
    assert_eq!(scroll, 41);
}

#[test]
fn a_removed_selection_falls_back_to_its_parent() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let x = m.rows()[50].id;
    m.select(x);
    t.remove(x);
    let scroll = m.refresh(&mut t, 40);
    assert_eq!(m.selected(), Some(NodeId(0)));
    assert_eq!(m.rows().len(), 100);
    assert_eq!(scroll, 40);
}

#[test]
fn a_selection_moved_under_a_collapsed_folder_falls_back_to_that_folder() {
    let mut t = sample(); // 0=C:, 1=big, 2=f1, 3=f2, 4=mid, 5=g, 6=small
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.select(NodeId(6));
    assert!(t.reparent(NodeId(6), NodeId(1)));
    m.refresh(&mut t, 0);
    assert_eq!(ids(&m), vec![0, 1, 4]);
    assert_eq!(m.selected(), Some(NodeId(1)));
}

#[test]
fn scroll_is_clamped_when_rows_disappear() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    for id in 1..=90 {
        t.remove(NodeId(id));
    }
    let scroll = m.refresh(&mut t, 80);
    assert_eq!(m.rows().len(), 11);
    assert_eq!(scroll, 10);
}

#[test]
fn new_directories_expand_without_growing_the_bitset_by_hand() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let d = t.add_child(NodeId(0), &u("d"), 0, NodeFlags::DIR);
    t.add_child(d, &u("x"), 5, NodeFlags::empty());
    assert!(!m.is_expanded(d), "ids past the bitset read as collapsed");
    m.refresh(&mut t, 0);
    m.expand(&t, d);
    assert!(m.is_expanded(d));
    assert!(ids(&m).contains(&8), "the new file shows under the new folder");
}

#[test]
fn a_directory_whose_children_all_died_is_not_expandable() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    t.remove(NodeId(5)); // mid's only child
    m.refresh(&mut t, 0);
    assert!(!m.is_expandable(&t, NodeId(4)));
    m.expand(&t, NodeId(4));
    assert!(!m.is_expanded(NodeId(4)));
}

/// After a pile of mutations, a refreshed model shows exactly what a model built from scratch
/// on the same tree shows: the sort caches were invalidated where, and only where, needed.
#[test]
fn refresh_matches_a_fresh_model_after_random_mutations() {
    for (key, desc) in [(SortKey::Size, true), (SortKey::Name, false), (SortKey::Items, true)] {
        let mut t = kary_builder(300, 3).finish().unwrap();
        let mut m = RowModel::new(&t);
        m.set_sort(&t, key, desc);
        expand_all(&mut m, &t);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for step in 0..600 {
            let live: Vec<NodeId> = (1..t.len() as u32)
                .map(NodeId)
                .filter(|&n| !t.flags[n].contains(NodeFlags::DEAD))
                .collect();
            let dirs: Vec<NodeId> = (0..t.len() as u32)
                .map(NodeId)
                .filter(|&n| t.flags[n].contains(NodeFlags::DIR) && !t.flags[n].contains(NodeFlags::DEAD))
                .collect();
            let files: Vec<NodeId> = live.iter().copied().filter(|&n| !t.flags[n].contains(NodeFlags::DIR)).collect();
            match rnd(4) {
                0 => {
                    let p = dirs[rnd(dirs.len())];
                    t.add_child(p, &u(&format!("a{step}")), rnd(5000) as u64, NodeFlags::empty());
                }
                1 if !files.is_empty() => t.set_size(files[rnd(files.len())], rnd(5000) as u64),
                2 if live.len() > 1 => {
                    t.remove(live[rnd(live.len())]);
                }
                _ if !live.is_empty() => {
                    t.reparent(live[rnd(live.len())], dirs[rnd(dirs.len())]);
                }
                _ => {}
            }
            if step % 20 == 19 {
                m.refresh(&mut t, 0);
                expand_all(&mut m, &t);
                let mut fresh = RowModel::new(&t);
                fresh.set_sort(&t, key, desc);
                expand_all(&mut fresh, &t);
                assert_eq!(ids(&m), ids(&fresh), "{key:?} after step {step}");
            }
        }
    }
}
```

- [ ] **Step 2: Declare the module and confirm the failure.** Append `#[cfg(test)]\nmod refresh_tests;` to `rows.rs`.

Run: `cargo test -p plumb-core --lib refresh`
Expected: FAIL to compile (`no method named refresh`).

- [ ] **Step 3: Implement.** Remove the two declaration lines, then apply (`git apply --ignore-whitespace`):

```diff
diff --git a/crates/plumb-core/src/rows.rs b/crates/plumb-core/src/rows.rs
index 95171f7..81d80e8 100644
--- a/crates/plumb-core/src/rows.rs
+++ b/crates/plumb-core/src/rows.rs
@@ -78,11 +78,14 @@ impl RowModel {
     }
 
     pub fn is_expanded(&self, id: NodeId) -> bool {
-        self.expanded[id.idx() / 64] >> (id.idx() % 64) & 1 == 1
+        self.expanded.get(id.idx() / 64).is_some_and(|w| w >> (id.idx() % 64) & 1 == 1)
     }
 
     fn set_expanded(&mut self, id: NodeId, on: bool) {
         let (w, b) = (id.idx() / 64, id.idx() % 64);
+        if w >= self.expanded.len() {
+            self.expanded.resize(w + 1, 0);
+        }
         if on {
             self.expanded[w] |= 1 << b;
         } else {
@@ -142,7 +145,7 @@ impl RowModel {
     }
 
     pub fn expand(&mut self, tree: &Tree, id: NodeId) {
-        if self.is_expanded(id) || tree.first_child[id].is_none() {
+        if self.is_expanded(id) || !tree.has_children(id) {
             return;
         }
         self.set_expanded(id, true);
@@ -196,7 +199,7 @@ impl RowModel {
             Key::PageUp => cur.saturating_sub(page.max(1)),
             Key::PageDown => (cur + page.max(1)).min(last),
             Key::Right => {
-                if tree.first_child[id].is_none() {
+                if !tree.has_children(id) {
                     cur
                 } else if !self.is_expanded(id) {
                     self.expand(tree, id);
@@ -218,6 +221,41 @@ impl RowModel {
         self.selected = Some(self.rows[target].id);
     }
 
+    /// Brings the rows up to date after tree mutations; it consumes the tree's `take_touched()` list.
+    /// Keeps the selected row at the same screen row and returns the scroll that does so (not
+    /// clamped to the window height; the caller clamps). If the selected node is gone or hidden,
+    /// its nearest visible ancestor is selected and `scroll` is returned unchanged.
+    pub fn refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize {
+        let touched = tree.take_touched();
+        let tree = &*tree;
+        for t in touched {
+            self.cache.remove(&t);
+            let mut cur = t;
+            while !cur.is_none() {
+                cur = tree.parent[cur];
+                self.cache.remove(&cur);
+            }
+        }
+        let anchor = self.selected_index().map(|i| (self.rows[i].id, i as isize - scroll as isize));
+        let words = tree.len().div_ceil(64);
+        if self.expanded.len() < words {
+            self.expanded.resize(words, 0);
+        }
+        self.rebuild(tree);
+        while let Some(s) = self.selected {
+            if self.row_index(s).is_some() {
+                break;
+            }
+            let p = tree.parent[s];
+            self.selected = (!p.is_none()).then_some(p);
+        }
+        let new = match (self.selected_index(), anchor) {
+            (Some(i), Some((id, row))) if Some(id) == self.selected => (i as isize - row).max(0) as usize,
+            _ => scroll,
+        };
+        new.min(self.rows.len().saturating_sub(1))
+    }
+
     /// The rows to draw for a view `height` rows tall, scrolled to row `scroll`.
     pub fn visible_window(&self, scroll: usize, height: usize) -> &[Row] {
         let start = scroll.min(self.rows.len());
@@ -235,7 +273,7 @@ impl RowModel {
 
     /// True if `id` is a directory with at least one child (something Right can expand).
     pub fn is_expandable(&self, tree: &Tree, id: NodeId) -> bool {
-        tree.flags[id].contains(NodeFlags::DIR) && !tree.first_child[id].is_none()
+        tree.flags[id].contains(NodeFlags::DIR) && tree.has_children(id)
     }
 }
 
@@ -372,3 +410,6 @@ mod tests {
         assert!(!m.is_expandable(&t, NodeId(6)));
     }
 }
+
+#[cfg(test)]
+mod refresh_tests;
```

Notes: expandability now uses `has_children` so a folder whose children all died is a leaf; `set_expanded` grows the bitset because new directories get ids past its end; `refresh` invalidates the cache of every touched node and of its whole parent chain (sizes changed there, so the Size/Items orders did too), rebuilds, and walks the selection up to the nearest visible ancestor if it vanished.

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p plumb-core --lib`
Expected: PASS (existing + 8 + 7), no warnings.

- [ ] **Step 5: Mutation-check.** Temporarily replace the cache-invalidation loop in `refresh` with `let _ = t;`; expect 6 of the 7 refresh tests to FAIL; restore and rerun to PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb-core/src/rows.rs crates/plumb-core/src/rows/refresh_tests.rs
git commit -m "Add RowModel::refresh: rebuild rows after mutations, keep the selection on its screen row"
```

---

### Task 3: Benchmarks, docs, whole-workspace check

**Files:**
- Modify: `crates/plumb-core/benches/core.rs`, `docs/benchmarks.md`, `CHANGELOG.md`, `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md`

- [ ] **Step 1: Add the benchmarks** (`git apply --ignore-whitespace`):

```diff
diff --git a/crates/plumb-core/benches/core.rs b/crates/plumb-core/benches/core.rs
index 9527c6a..120530d 100644
--- a/crates/plumb-core/benches/core.rs
+++ b/crates/plumb-core/benches/core.rs
@@ -1,5 +1,5 @@
 use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
-use plumb_core::NodeId;
+use plumb_core::{NodeFlags, NodeId};
 use plumb_core::mft::{ParsedRecord, parse_record};
 use plumb_core::rows::RowModel;
 use plumb_core::synth::{RECORD_SIZE, RecordBuilder, flat_tree, kary_builder};
@@ -70,5 +70,63 @@ fn bench_rows(c: &mut Criterion) {
     g.finish();
 }
 
-criterion_group!(benches, bench_parse, bench_sums, bench_rows);
+fn bench_mutation(c: &mut Criterion) {
+    let mut g = c.benchmark_group("mutation");
+    g.sample_size(10);
+    g.throughput(Throughput::Elements(100_000));
+    // Leaves of the 8-ary tree start at node 625_000; directories are below that.
+    g.bench_function("apply_100k_deltas_to_5m_nodes", |b| {
+        b.iter_batched_ref(
+            || kary_builder(5_000_000, 8).finish().unwrap(),
+            |t| {
+                let name: Vec<u16> = "renamed".encode_utf16().collect();
+                for i in 0..100_000u32 {
+                    let leaf = NodeId(625_000 + i * 37);
+                    match i % 4 {
+                        0 => t.set_size(leaf, u64::from(i) * 13),
+                        1 => {
+                            t.add_child(NodeId(i % 600_000), &name, 4096, NodeFlags::empty());
+                        }
+                        2 => t.rename(leaf, &name),
+                        _ => {
+                            t.remove(leaf);
+                        }
+                    }
+                }
+                black_box(t.dead());
+            },
+            BatchSize::LargeInput,
+        );
+    });
+    g.finish();
+}
+
+fn bench_refresh(c: &mut Criterion) {
+    let mut g = c.benchmark_group("rows");
+    g.sample_size(10);
+    g.bench_function("refresh_after_one_add_in_1m_children", |b| {
+        b.iter_batched_ref(
+            || {
+                let mut tree = flat_tree(1_000_000);
+                let mut m = RowModel::new(&tree);
+                m.expand(&tree, NodeId(0));
+                m.select(m.rows()[500_000].id);
+                tree.add_child(NodeId(0), &[0x61], 1 << 41, NodeFlags::empty());
+                (tree, m)
+            },
+            |(tree, m)| black_box(m.refresh(tree, 499_990)),
+            BatchSize::LargeInput,
+        );
+    });
+    g.bench_function("refresh_nothing_touched_in_1m_children", |b| {
+        let mut tree = flat_tree(1_000_000);
+        let mut m = RowModel::new(&tree);
+        m.expand(&tree, NodeId(0));
+        m.select(m.rows()[500_000].id);
+        b.iter(|| black_box(m.refresh(&mut tree, 499_990)));
+    });
+    g.finish();
+}
+
+criterion_group!(benches, bench_parse, bench_sums, bench_rows, bench_mutation, bench_refresh);
 criterion_main!(benches);
```

- [ ] **Step 2: Check load, then run them.** Per `LONG_RUNS.md`: confirm CPU under ~20% and free memory above 2 GB (the 5M-node tree is a few hundred MB), report Defender state.

Run: `cargo bench -p plumb-core --bench core -- "mutation|refresh"`
Expected: three results. Reference run on 2026-10-12, with the machine at ~49% CPU (so pessimistic) and Defender state unchecked: 100k deltas on 5M nodes about 53 ms (about 0.5 us per delta); `refresh` after one add into a 1M-child folder about 244 ms (the folder is re-sorted); `refresh` with nothing touched about 6.9 ms (rebuilds 1M rows).

- [ ] **Step 3: Record results.** Add a `## Live updates (core) <date>` section to `docs/benchmarks.md` with your numbers, the load and Defender state, and these two findings to carry into plan 3:
  - A single change in a very large folder costs a full re-sort (about 244 ms at 1M children), far beyond the 5 ms apply budget. Plan 3 must coalesce so the refresh happens at most once per repaint tick, and may need an incremental re-sort for huge folders (parked until measured in the UI).
  - A no-op refresh still rebuilds the row list (about 7 ms at 1M rows); plan 3 should skip `refresh` when nothing was applied.

- [ ] **Step 4: Changelog and spec.** Under `## [Unreleased]` in `CHANGELOG.md` add `### Added` with: `plumb-core`: in-place tree mutation (`add_child`, `set_size`, `rename`, `remove`, `reparent`, `NodeFlags::DEAD`) and `RowModel::refresh` (foundation for live updates, not yet wired into the UI). In the spec's "Row model" section, change the `refresh` signature to `refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize` (consumes `Tree::take_touched`), and mark plan 1 done in "Plans".

- [ ] **Step 5: Whole-workspace check.**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets`
Expected: PASS, no new warnings.

- [ ] **Step 6: Commit.**

```bash
git add crates/plumb-core/benches/core.rs docs/benchmarks.md CHANGELOG.md docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md
git commit -m "Benchmark tree mutation and refresh; record live-updates plan 1 results"
```

---

## Self-review

- Spec coverage: `DEAD`, `add_child`, `set_size`, `remove`, `rename`, `reparent` (Task 1); `refresh` with anchoring, ancestor fallback, dead nodes absent from rows (Task 2); the spec's testing list (oracle comparison, anchoring, clamping) and both Criterion benchmarks (Tasks 1-3). The `touched` signature deviation is stated in Global Constraints and written back to the spec in Task 3.
- Not in this plan: the record-number table, deltas, the journal and any UI (plans 2 and 3).
- Verified before writing: the patches and tests in this plan were run in a scratch copy of the repo at `58d63e8`: 55 `plumb-core` lib tests pass with no warnings, and deliberately breaking `reparent` or the cache invalidation makes the intended tests fail.
