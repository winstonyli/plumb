use std::num::NonZeroU32;

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
    /// Last-write day (days since 1601-01-01); `None` when unknown.
    pub mtime: Vec<Option<NonZeroU32>>,
    pub roots: Vec<NodeId>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    pool: Vec<u16>,
    /// Nodes marked [`NodeFlags::DEAD`] so far.
    dead: usize,
    /// Nodes whose size, name, parent or existence changed (and old parents) since the last
    /// [`Tree::take_touched`].
    touched: Vec<NodeId>,
    rev: u64,
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

    /// Live children of `id`, in list order.
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(self.first_child[id]).filter(|c| !c.is_none()), |&c| {
            Some(self.next_sibling[c]).filter(|n| !n.is_none())
        })
        .filter(|&c| !self.flags[c].contains(NodeFlags::DEAD))
    }

    pub fn has_children(&self, id: NodeId) -> bool {
        self.children(id).next().is_some()
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

    /// The node at `path` ("C:\dir\file", as [`Tree::path`] gives it), matching names exactly.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let mut parts = path.split('\\');
        let first = parts.next()?;
        let mut cur = *self.roots.iter().find(|&&r| self.name_string(r) == first)?;
        for part in parts {
            cur = self.children(cur).find(|&c| self.name_string(c) == part)?;
        }
        Some(cur)
    }

    /// Bumped by every change to a node: a cheap "has the tree changed" check.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Nodes marked dead so far (slots not yet reclaimed).
    pub fn dead(&self) -> usize {
        self.dead
    }

    /// Ids changed since the last call; pass them to `RowModel::refresh`.
    pub fn take_touched(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.touched)
    }

    /// Ids changed since the last [`Tree::take_touched`], without consuming them.
    pub fn touched(&self) -> &[NodeId] {
        &self.touched
    }

    /// The three name arrays, for [`crate::filter::Names`].
    pub(crate) fn name_arrays(&self) -> (&[u32], &[u16], &[u16]) {
        (&self.name_off, &self.name_len, &self.pool)
    }

    /// Adds (or subtracts) `size` bytes and `files` files to `from` and every ancestor.
    fn bump(&mut self, mut from: NodeId, size: u64, files: u32, add: bool) {
        while !from.is_none() {
            if add {
                self.subtree_size[from] += size;
                self.subtree_files[from] += files;
            } else {
                self.subtree_size[from] -= size;
                self.subtree_files[from] -= files;
            }
            from = self.parent[from];
        }
    }

    /// Appends a new node under `parent` (at the head of its child list) and returns its id.
    pub fn add_child(&mut self, parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId {
        let id = NodeId(self.parent.len() as u32);
        let files = u32::from(!flags.contains(NodeFlags::DIR));
        self.parent.push(parent);
        self.first_child.push(NodeId::NONE);
        self.next_sibling.push(self.first_child[parent]);
        self.first_child[parent] = id;
        self.size.push(size);
        self.subtree_size.push(size);
        self.subtree_files.push(files);
        self.flags.push(flags);
        self.mtime.push(None);
        self.name_off.push(self.pool.len() as u32);
        self.name_len.push(name.len() as u16);
        self.pool.extend_from_slice(name);
        self.bump(parent, size, files, true);
        self.touched.push(id);
        self.rev += 1;
        id
    }

    pub fn set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>) {
        self.mtime[id] = m;
    }

    /// Makes room for `nodes` more nodes and `pool_units` more name units, so a tree that is
    /// grown during a scan does not reallocate every array mid-way.
    pub fn reserve(&mut self, nodes: usize, pool_units: usize) {
        self.parent.reserve(nodes);
        self.first_child.reserve(nodes);
        self.next_sibling.reserve(nodes);
        self.size.reserve(nodes);
        self.subtree_size.reserve(nodes);
        self.subtree_files.reserve(nodes);
        self.flags.reserve(nodes);
        self.mtime.reserve(nodes);
        self.name_off.reserve(nodes);
        self.name_len.reserve(nodes);
        self.pool.reserve(pool_units);
    }

    /// Changes a node's own size and fixes the sums above it. Ignored for dead nodes.
    pub fn set_size(&mut self, id: NodeId, new: u64) {
        if self.flags[id].contains(NodeFlags::DEAD) {
            return;
        }
        let old = std::mem::replace(&mut self.size[id], new);
        if new >= old {
            self.bump(id, new - old, 0, true);
        } else {
            self.bump(id, old - new, 0, false);
        }
        self.touched.push(id);
        self.rev += 1;
    }

    /// Gives a node a new name (the old bytes stay in the pool as waste).
    pub fn rename(&mut self, id: NodeId, name: &[u16]) {
        self.name_off[id] = self.pool.len() as u32;
        self.name_len[id] = name.len() as u16;
        self.pool.extend_from_slice(name);
        self.touched.push(id);
        self.rev += 1;
    }

    /// Marks `id` and everything under it dead and takes its sizes out of its ancestors.
    /// False (and no change) for a root or an already dead node.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let parent = self.parent[id];
        if parent.is_none() || self.flags[id].contains(NodeFlags::DEAD) {
            return false;
        }
        let (size, files) = (self.subtree_size[id], self.subtree_files[id]);
        self.bump(parent, size, files, false);
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            stack.extend(self.children(n));
            self.flags[n] |= NodeFlags::DEAD;
            self.dead += 1;
        }
        self.touched.push(id);
        self.rev += 1;
        true
    }

    /// Moves `id` (with its whole subtree) under `new_parent`. False (and no change) if `id` is a
    /// root or dead, `new_parent` is dead, or `new_parent` is `id` or inside it.
    pub fn reparent(&mut self, id: NodeId, new_parent: NodeId) -> bool {
        let old = self.parent[id];
        let dead = |t: &Self, n: NodeId| t.flags[n].contains(NodeFlags::DEAD);
        if old.is_none() || dead(self, id) || dead(self, new_parent) {
            return false;
        }
        let mut up = new_parent;
        while !up.is_none() {
            if up == id {
                return false;
            }
            up = self.parent[up];
        }
        if old == new_parent {
            return true;
        }
        if self.first_child[old] == id {
            self.first_child[old] = self.next_sibling[id];
        } else {
            let mut prev = self.first_child[old];
            while self.next_sibling[prev] != id {
                prev = self.next_sibling[prev];
            }
            self.next_sibling[prev] = self.next_sibling[id];
        }
        let (size, files) = (self.subtree_size[id], self.subtree_files[id]);
        self.bump(old, size, files, false);
        self.parent[id] = new_parent;
        self.next_sibling[id] = self.first_child[new_parent];
        self.first_child[new_parent] = id;
        self.bump(new_parent, size, files, true);
        self.touched.push(old);
        self.rev += 1;
        self.touched.push(id);
        self.rev += 1;
        true
    }
}

/// Collects nodes in any order. A node's parent may be added after the node.
#[derive(Default)]
pub struct TreeBuilder {
    parent: Vec<NodeId>,
    size: Vec<u64>,
    flags: Vec<NodeFlags>,
    mtime: Vec<Option<NonZeroU32>>,
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
            mtime: Vec::with_capacity(nodes),
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

    pub fn set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>) {
        self.mtime[id] = m;
    }

    /// Adds `flags` to an already pushed node (e.g. DENIED, learned after the node was created).
    pub fn add_flags(&mut self, id: NodeId, flags: NodeFlags) {
        self.flags[id] |= flags;
    }

    pub fn push(&mut self, parent: NodeId, name: &[u16], size: u64, flags: NodeFlags) -> NodeId {
        let id = NodeId(u32::try_from(self.parent.len()).expect("too many nodes"));
        self.name_off.push(u32::try_from(self.pool.len()).expect("name pool too large"));
        self.name_len.push(u16::try_from(name.len()).expect("name longer than 65535 units"));
        self.pool.extend_from_slice(name);
        self.parent.push(parent);
        self.size.push(size);
        self.flags.push(flags);
        self.mtime.push(None);
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
            mtime: self.mtime,
            roots,
            name_off: self.name_off,
            name_len: self.name_len,
            pool: self.pool,
            dead: 0,
            touched: Vec::new(),
            rev: 0,
        })
    }
}

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
    fn add_flags_after_push() {
        let mut b = TreeBuilder::new();
        let r = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
        b.add_flags(r, NodeFlags::DENIED);
        let t = b.finish().unwrap();
        assert!(t.flags[r].contains(NodeFlags::DIR | NodeFlags::DENIED));
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

#[cfg(test)]
mod mutation_tests;
