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
    /// Score of the subtree: for a file, [`crate::score::file_score`] (zero when the tree has no clock, see
    /// [`Tree::set_now`]); for a folder, the sum over its descendants; the Processes view adds its own through
    /// [`Tree::add_score`].
    pub subtree_score: Vec<u64>,
    pub flags: Vec<NodeFlags>,
    /// Last-write time (unix seconds); `None` when unknown.
    pub mtime: Vec<Option<NonZeroU32>>,
    /// Three more numbers per node for a source to sort by (the process view: CPU, I/O, PID). Empty for a tree
    /// that never called [`Tree::set_aux`]; otherwise one entry per node.
    pub aux: Vec<[u64; 3]>,
    pub roots: Vec<NodeId>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    pool: Vec<u16>,
    /// Nodes marked [`NodeFlags::DEAD`] so far.
    dead: usize,
    /// Unix seconds the file scores are measured against; `None` for a tree that scores nothing.
    now: Option<u32>,
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

    /// Writes the name of `id` (invalid UTF-16 as U+FFFD, like [`Tree::name_string`]) without a `String` of its own.
    pub fn write_name(&self, id: NodeId, out: &mut impl std::fmt::Write) -> std::fmt::Result {
        for c in char::decode_utf16(self.name(id).iter().copied()) {
            out.write_char(c.unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
        Ok(())
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
        let mut s = String::new();
        let _ = self.write_path(id, &mut s);
        s
    }

    /// Writes the path [`Tree::path`] gives, without building it first.
    pub fn write_path(&self, id: NodeId, out: &mut impl std::fmt::Write) -> std::fmt::Result {
        let parent = self.parent[id];
        if !parent.is_none() {
            self.write_path(parent, out)?;
            out.write_char('\\')?;
        }
        self.write_name(id, out)
    }

    /// The node at `path` ("C:\dir\file", as [`Tree::path`] gives it), matching names exactly.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        // A root is named by the whole scanned path ("C:" or "D:\Mounts\Data"), so match it as a prefix.
        let (mut cur, rest) = self.roots.iter().find_map(|&r| {
            let name = self.name_string(r);
            let rest = path.strip_prefix(name.as_str())?;
            (rest.is_empty() || rest.starts_with('\\')).then_some((r, rest))
        })?;
        for part in rest.split('\\').skip(1) {
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

    /// Adds (or subtracts) `size` bytes, `files` files and `score` to `from` and every ancestor.
    fn bump(&mut self, mut from: NodeId, size: u64, files: u32, score: u64, add: bool) {
        while !from.is_none() {
            if add {
                self.subtree_size[from] += size;
                self.subtree_files[from] += files;
                self.subtree_score[from] += score;
            } else {
                self.subtree_size[from] -= size;
                self.subtree_files[from] -= files;
                self.subtree_score[from] -= score;
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
        self.subtree_score.push(0);
        self.flags.push(flags);
        self.mtime.push(None);
        if !self.aux.is_empty() {
            self.aux.push([0; 3]);
        }
        self.name_off.push(self.pool.len() as u32);
        self.name_len.push(name.len() as u16);
        self.pool.extend_from_slice(name);
        self.bump(parent, size, files, 0, true);
        self.touched.push(id);
        self.rev += 1;
        id
    }

    pub fn set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>) {
        self.mtime[id] = m;
        self.rescore(id);
    }

    /// Gives the tree a clock (unix seconds) and scores every file against it.
    pub fn set_now(&mut self, now: u32) {
        self.now = Some(now);
        for i in 0..self.len() as u32 {
            self.rescore(NodeId(i));
        }
    }

    /// Adds a signed `delta` to `id`'s score and every ancestor's, and counts `id` as changed.
    pub fn add_score(&mut self, id: NodeId, delta: i64) {
        if delta == 0 {
            return;
        }
        self.bump(id, 0, 0, delta.unsigned_abs(), delta > 0);
        self.touched.push(id);
        self.rev += 1;
    }

    /// Brings a file's score in line with its current size and modified time. Nothing for a tree without a clock,
    /// a dead node or a folder.
    fn rescore(&mut self, id: NodeId) {
        let Some(now) = self.now else { return };
        if self.flags[id].contains(NodeFlags::DEAD) || self.flags[id].contains(NodeFlags::DIR) {
            return;
        }
        debug_assert!(!self.has_children(id), "only a file has a score of its own");
        let new = crate::score::file_score(self.size[id], self.mtime[id], now);
        let delta = i64::try_from(new).expect("a score is a size") - i64::try_from(self.subtree_score[id]).expect("a score is a size");
        self.add_score(id, delta);
    }

    /// Replaces the node's `TRAITS` flags (reparse, system, cloud) with those of `traits`.
    pub fn set_traits(&mut self, id: NodeId, traits: NodeFlags) {
        let f = &mut self.flags[id];
        *f = (*f - NodeFlags::TRAITS) | (traits & NodeFlags::TRAITS);
    }

    /// Makes `id` a folder (it can be expanded, and the map frames what is in it) or a file (one item in its
    /// parents' counts, a tile of its own). Ignored when it already is, or for a dead node.
    pub fn set_dir(&mut self, id: NodeId, dir: bool) {
        if self.flags[id].contains(NodeFlags::DEAD) || self.flags[id].contains(NodeFlags::DIR) == dir {
            return;
        }
        debug_assert!(self.now.is_none(), "a scored (disk) tree removes and re-adds a node whose kind changes");
        self.flags[id].set(NodeFlags::DIR, dir);
        let mut up = id;
        while !up.is_none() {
            // A folder counts no item of its own, a file counts itself.
            self.subtree_files[up] = if dir { self.subtree_files[up] - 1 } else { self.subtree_files[up] + 1 };
            up = self.parent[up];
        }
        self.touched.push(id);
        self.rev += 1;
    }

    /// Sets the node's aux numbers (the first call gives every node zeros), and counts the node as changed so a
    /// sort by them repositions it.
    pub fn set_aux(&mut self, id: NodeId, v: [u64; 3]) {
        if self.aux.is_empty() {
            self.aux = vec![[0; 3]; self.parent.len()];
        }
        if self.aux[id.idx()] != v {
            self.aux[id.idx()] = v;
            self.touched.push(id);
            self.rev += 1;
        }
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
        self.subtree_score.reserve(nodes);
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
            self.bump(id, new - old, 0, 0, true);
        } else {
            self.bump(id, old - new, 0, 0, false);
        }
        self.touched.push(id);
        self.rev += 1;
        self.rescore(id);
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
        let (size, files, score) = (self.subtree_size[id], self.subtree_files[id], self.subtree_score[id]);
        self.bump(parent, size, files, score, false);
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
        let (size, files, score) = (self.subtree_size[id], self.subtree_files[id], self.subtree_score[id]);
        self.bump(old, size, files, score, false);
        self.parent[id] = new_parent;
        self.next_sibling[id] = self.first_child[new_parent];
        self.first_child[new_parent] = id;
        self.bump(new_parent, size, files, score, true);
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
    now: Option<u32>,
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
            now: None,
        }
    }

    /// Gives the finished tree a clock (unix seconds), so its files score against it.
    pub fn set_now(&mut self, now: u32) {
        self.now = Some(now);
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
        let mut subtree_score: Vec<u64> = match self.now {
            Some(now) => (0..n)
                .map(|i| if self.flags[i].contains(NodeFlags::DIR) { 0 } else { crate::score::file_score(self.size[i], self.mtime[i], now) })
                .collect(),
            None => vec![0; n],
        };
        for &id in order.iter().rev() {
            let p = self.parent[id];
            if !p.is_none() {
                subtree_size[p] += subtree_size[id];
                subtree_files[p] += subtree_files[id];
                subtree_score[p] += subtree_score[id];
            }
        }

        Ok(Tree {
            parent: self.parent,
            first_child,
            next_sibling,
            size: self.size,
            subtree_size,
            subtree_files,
            subtree_score,
            flags: self.flags,
            mtime: self.mtime,
            aux: Vec::new(),
            roots,
            name_off: self.name_off,
            name_len: self.name_len,
            pool: self.pool,
            dead: 0,
            now: self.now,
            touched: Vec::new(),
            rev: 0,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn aux_is_empty_for_a_built_tree_and_set_aux_fills_it_in_step_with_the_nodes() {
        let mut t = sample();
        assert!(t.aux.is_empty());
        t.set_aux(NodeId(1), [5, 6, 7]);
        assert_eq!(t.aux.len(), t.len());
        assert_eq!(t.aux[1], [5, 6, 7]);
        let n = t.add_child(NodeId(0), &[b'x' as u16], 1, NodeFlags::empty());
        assert_eq!(t.aux.len(), t.len());
        assert_eq!(t.aux[n.idx()], [0; 3]);
        t.take_touched();
        t.set_aux(NodeId(1), [5, 6, 7]);
        assert!(t.touched().is_empty(), "an unchanged value is not a change");
        t.set_aux(NodeId(1), [5, 6, 8]);
        assert_eq!(t.touched(), &[NodeId(1)]);
    }

    #[test]
    fn set_dir_moves_a_node_between_file_and_folder_and_fixes_the_item_counts_above() {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
        let a = b.push_str(root, "a", 5, NodeFlags::empty());
        let mut t = b.finish().unwrap();
        assert_eq!((t.subtree_files[a], t.subtree_files[root]), (1, 1));
        t.take_touched();
        t.set_dir(a, true);
        assert_eq!((t.subtree_files[a], t.subtree_files[root]), (0, 0));
        assert_eq!(t.touched(), &[a]);
        t.set_dir(a, true);
        assert_eq!(t.touched(), &[a], "no change, no touch");
        t.set_dir(a, false);
        assert_eq!((t.subtree_files[a], t.subtree_files[root]), (1, 1));
    }

    #[test]
    fn find_resolves_below_a_root_named_by_a_whole_folder_path() {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, r"D:\Mounts\Data", 0, NodeFlags::DIR);
        let sub = b.push_str(root, "sub", 0, NodeFlags::DIR);
        b.push_str(sub, "f", 1, NodeFlags::empty());
        let t = b.finish().unwrap();
        let f = t.find(r"D:\Mounts\Data\sub\f").unwrap();
        assert_eq!(t.path(f), r"D:\Mounts\Data\sub\f");
        assert_eq!(t.find(r"D:\Mounts\Data"), Some(root));
        assert!(t.find(r"D:\Mounts\DataX").is_none() && t.find(r"D:\Mounts").is_none(), "only at a path boundary");
    }

    const DAY: u32 = 86_400;
    const NOW: u32 = 2_000_000_000;
    fn ago(days: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(NOW - days * DAY)
    }

    /// r/{d/{a=1000 (3 years old), b=1000 (365 days old)}, e/{}, c=1000 (no mtime)}; `now` decides whether it scores.
    fn scored_with(now: bool) -> (Tree, [NodeId; 6]) {
        let mut b = TreeBuilder::new();
        if now {
            b.set_now(NOW);
        }
        let r = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
        let d = b.push_str(r, "d", 0, NodeFlags::DIR);
        let a = b.push_str(d, "a", 1000, NodeFlags::empty());
        let bb = b.push_str(d, "b", 1000, NodeFlags::empty());
        let e = b.push_str(r, "e", 0, NodeFlags::DIR);
        let c = b.push_str(r, "c", 1000, NodeFlags::empty());
        b.set_mtime(a, ago(1095));
        b.set_mtime(bb, ago(365));
        (b.finish().unwrap(), [r, d, a, bb, e, c])
    }

    pub(crate) fn scored() -> (Tree, [NodeId; 6]) {
        scored_with(true)
    }

    fn scores(t: &Tree, ids: &[NodeId]) -> Vec<u64> {
        ids.iter().map(|&i| t.subtree_score[i]).collect()
    }

    #[test]
    fn files_score_by_age_and_folders_sum_them() {
        let (t, [r, d, a, b, e, c]) = scored();
        assert_eq!(scores(&t, &[a, b, c, d, e, r]), [1000, 478, 0, 1478, 0, 1478]);
    }

    #[test]
    fn a_live_change_moves_the_sums_and_touches_the_file() {
        let (mut t, [r, d, a, b, e, c]) = scored();
        t.take_touched();
        t.set_mtime(c, ago(1095));
        assert_eq!((t.subtree_score[c], t.subtree_score[r]), (1000, 2478));
        assert!(t.touched().contains(&c));
        t.set_size(a, 2000);
        assert_eq!((t.subtree_score[a], t.subtree_score[d], t.subtree_score[r]), (2000, 2478, 3478));
        // The order of a rewrite does not matter.
        let f = t.add_child(e, &[b'f' as u16], 0, NodeFlags::empty());
        t.set_size(f, 1000);
        t.set_mtime(f, ago(1095));
        let g = t.add_child(e, &[b'g' as u16], 0, NodeFlags::empty());
        t.set_mtime(g, ago(1095));
        t.set_size(g, 1000);
        assert_eq!((t.subtree_score[f], t.subtree_score[g], t.subtree_score[e], t.subtree_score[r]), (1000, 1000, 2000, 5478));
        assert!(t.remove(a));
        assert_eq!((t.subtree_score[d], t.subtree_score[r]), (478, 3478));
        assert!(t.reparent(b, e));
        assert_eq!(scores(&t, &[d, e, r]), [0, 2478, 3478]);
    }

    #[test]
    fn a_tree_without_now_scores_nothing() {
        let (mut t, [r, _d, a, _b, _e, c]) = scored_with(false);
        t.set_mtime(c, ago(1095));
        t.set_size(a, 2000);
        assert!(t.subtree_score.iter().all(|&s| s == 0));
        assert_eq!(t.subtree_score.len(), t.len());
        assert_eq!(t.subtree_score[r], 0);
    }

    #[test]
    fn set_now_on_a_finished_tree_scores_it() {
        let (mut t, [r, d, a, b, e, c]) = scored_with(false);
        assert!(t.subtree_score.iter().all(|&s| s == 0));
        t.set_now(NOW);
        assert_eq!(scores(&t, &[a, b, c, d, e, r]), [1000, 478, 0, 1478, 0, 1478]);
    }

    #[test]
    fn folders_and_dead_nodes_never_change_the_sums() {
        let (mut t, [r, d, a, _b, _e, _c]) = scored();
        t.set_size(r, 5);
        t.set_size(d, 5);
        assert_eq!((t.subtree_score[d], t.subtree_score[r]), (1478, 1478));
        assert!(t.remove(a));
        let after = t.subtree_score[r];
        t.set_mtime(a, None);
        t.set_size(a, 9);
        assert_eq!(t.subtree_score[r], after);
    }

    #[test]
    fn add_score_adds_signed_deltas_up_the_chain() {
        let (mut t, [r, _d, _a, _b, _e, c]) = scored();
        t.take_touched();
        t.add_score(c, 7);
        t.add_score(c, -3);
        assert_eq!((t.subtree_score[c], t.subtree_score[r]), (4, 1482));
        assert!(t.touched().contains(&c));
    }

    /// The score of `id` by walking: its own file score plus its live children's.
    fn oracle(t: &Tree, id: NodeId) -> u64 {
        let own = if t.flags[id].contains(NodeFlags::DIR) { 0 } else { crate::score::file_score(t.size[id], t.mtime[id], NOW) };
        own + t.children(id).filter(|&c| !t.flags[c].contains(NodeFlags::DEAD)).map(|c| oracle(t, c)).sum::<u64>()
    }

    #[test]
    fn scores_stay_equal_to_a_walk_under_random_edits() {
        let (mut t, _) = scored();
        let mut seed = 12345u64;
        let mut rnd = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for step in 0..500 {
            let live: Vec<NodeId> = (0..t.len() as u32).map(NodeId).filter(|&n| !t.flags[n].contains(NodeFlags::DEAD)).collect();
            let dirs: Vec<NodeId> = live.iter().copied().filter(|&n| t.flags[n].contains(NodeFlags::DIR)).collect();
            let pick = |v: &[NodeId], r: u64| v[r as usize % v.len()];
            let (any, dir) = (pick(&live, rnd(1000)), pick(&dirs, rnd(1000)));
            let mtime = if rnd(4) == 0 { None } else { ago(rnd(1000) as u32 + 1) };
            match rnd(6) {
                0 => {
                    let flags = if rnd(3) == 0 { NodeFlags::DIR } else { NodeFlags::empty() };
                    let id = t.add_child(dir, &[b'n' as u16], rnd(1_000_000), flags);
                    t.set_mtime(id, mtime);
                }
                1 => t.set_size(any, rnd(1_000_000)),
                2 => t.set_mtime(any, mtime),
                3 => {
                    t.remove(any);
                }
                4 => {
                    t.reparent(any, dir);
                }
                _ => t.set_size(dir, rnd(1000)),
            }
            for &n in &live {
                if !t.flags[n].contains(NodeFlags::DEAD) {
                    assert_eq!(t.subtree_score[n], oracle(&t, n), "step {step}, node {n:?}");
                }
            }
        }
    }

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
