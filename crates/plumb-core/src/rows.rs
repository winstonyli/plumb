use crate::bits;
use crate::filter::Keep;
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
    /// Shown only because the user opened a folder under a filter; it does not match.
    pub dim: bool,
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
    /// The active filter, if any.
    filter: Option<Rc<Keep>>,
    /// Ancestors of matches the user collapsed while a filter is active (they are open by default).
    closed: Vec<u64>,
    /// Scratch for `reposition`: nodes whose sort position may have changed. All false between calls.
    mark: Vec<bool>,
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
            filter: None,
            closed: Vec::new(),
            mark: Vec::new(),
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

    pub fn is_expanded(&self, id: NodeId) -> bool {
        match &self.filter {
            Some(k) if k.is_ancestor(id) => !bits::get(&self.closed, id.idx()),
            _ => bits::get(&self.expanded, id.idx()),
        }
    }

    fn set_expanded(&mut self, id: NodeId, on: bool) {
        if self.filter.as_ref().is_some_and(|k| k.is_ancestor(id)) {
            bits::set(&mut self.closed, id.idx(), !on);
        } else {
            bits::set(&mut self.expanded, id.idx(), on);
        }
    }

    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool) {
        self.key = key;
        self.desc = desc;
        self.cache.clear();
        self.rebuild(tree);
    }

    /// Installs (or, with `None`, removes) a filter and rebuilds the rows. A selection the filter
    /// hides moves to its nearest visible ancestor, or is cleared. Folders the user closed under an
    /// earlier filter stay closed while a filter stays active; they reopen when it is removed or
    /// replaces none.
    pub fn set_filter(&mut self, tree: &Tree, keep: Option<Rc<Keep>>) {
        if self.filter.is_none() || keep.is_none() {
            self.closed.clear();
        }
        self.filter = keep;
        self.rebuild(tree);
        while let Some(s) = self.selected {
            if self.row_index(s).is_some() {
                break;
            }
            let p = tree.parent[s];
            self.selected = (!p.is_none()).then_some(p);
        }
    }

    /// Swaps in a rebuilt keep set for the filter that is already active, without rebuilding the
    /// rows. Call [`RowModel::refresh`] next: it clears the stale sort caches and builds the rows.
    pub fn update_keep(&mut self, keep: Rc<Keep>) {
        debug_assert!(self.filter.is_some(), "update_keep needs an active filter");
        self.filter = Some(keep);
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
        ids.sort_unstable_by(|&a, &b| self.order(tree, a, b));
        let rc: Rc<[NodeId]> = ids.into();
        self.cache.insert(parent, Rc::clone(&rc));
        rc
    }

    /// The sort: by the key in the chosen direction, ties by id so the order is total.
    fn order(&self, tree: &Tree, a: NodeId, b: NodeId) -> Ordering {
        let o = match self.key {
            SortKey::Size => tree.subtree_size[a].cmp(&tree.subtree_size[b]),
            SortKey::Items => tree.subtree_files[a].cmp(&tree.subtree_files[b]),
            SortKey::Name => cmp_names(tree.name(a), tree.name(b)),
        };
        (if self.desc { o.reverse() } else { o }).then(a.cmp(&b))
    }

    /// Fixes the cached sorted lists after `touched` nodes changed, without re-sorting them. A node
    /// that changed, and every ancestor of it (their sums moved), may sit in the wrong place in its
    /// parent's list; everything else still sorts as before. Per affected list: drop those nodes,
    /// sort the (few) live ones, and merge them back in one pass.
    fn reposition(&mut self, tree: &Tree, touched: &[NodeId]) {
        if self.cache.is_empty() {
            return;
        }
        if self.mark.len() < tree.len() {
            self.mark.resize(tree.len(), false);
        }
        let mut dirty: Vec<NodeId> = Vec::new();
        for &t in touched {
            let mut cur = t;
            while !cur.is_none() && !self.mark[cur] {
                self.mark[cur] = true;
                dirty.push(cur);
                cur = tree.parent[cur];
            }
        }
        let mut by_parent: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &d in &dirty {
            // A node's own list may hold children that moved away (the old parent of a reparent).
            if self.cache.contains_key(&d) {
                by_parent.entry(d).or_default();
            }
            let p = tree.parent[d];
            if self.cache.contains_key(&p) {
                let v = by_parent.entry(p).or_default();
                if !tree.flags[d].contains(NodeFlags::DEAD) {
                    v.push(d);
                }
            }
        }
        for (p, mut fresh) in by_parent {
            fresh.sort_unstable_by(|&a, &b| self.order(tree, a, b));
            let old = Rc::clone(&self.cache[&p]);
            let mut out: Vec<NodeId> = Vec::with_capacity(old.len() + fresh.len());
            let mut next = fresh.iter().copied().peekable();
            for &e in old.iter().filter(|&&e| !self.mark[e]) {
                while let Some(f) = next.next_if(|&f| self.order(tree, f, e) == Ordering::Less) {
                    out.push(f);
                }
                out.push(e);
            }
            out.extend(next);
            self.cache.insert(p, out.into());
        }
        for d in dirty {
            self.mark[d] = false;
        }
    }

    fn push_children(&mut self, tree: &Tree, parent: NodeId, depth: u16, out: &mut Vec<Row>) {
        let kids = self.sorted_children(tree, parent);
        let filter = self.filter.clone();
        // Under a filter the roots and the children of an ancestor of a match are cut down to the
        // kept ones. Below a leaf match, or below a node the filter hides, everything shows, dimmed.
        let cut = filter.as_ref().is_some_and(|k| parent.is_none() || k.is_ancestor(parent));
        for &c in kids.iter() {
            let dim = filter.as_ref().is_some_and(|k| !k.is_kept(c));
            if dim && cut {
                continue;
            }
            out.push(Row { id: c, depth, dim });
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
        if self.is_expanded(id) || !tree.has_children(id) {
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
                if !tree.has_children(id) {
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

    /// Brings the rows up to date after tree mutations; it consumes the tree's `take_touched()` list.
    /// Keeps the selected row at the same screen row and returns the scroll that does so (not
    /// clamped to the window height; the caller clamps). If the selected node is gone or hidden,
    /// its nearest visible ancestor is selected and `scroll` is returned unchanged.
    pub fn refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize {
        let touched = tree.take_touched();
        let tree = &*tree;
        // A long list (a scan being previewed) costs more to walk, ancestor by ancestor, than the
        // sorted children lists cost to rebuild, so drop them all.
        if touched.len() > 4096 {
            self.cache.clear();
        } else {
            self.reposition(tree, &touched);
        }
        let anchor = self.selected_index().map(|i| (self.rows[i].id, i as isize - scroll as isize));
        let words = tree.len().div_ceil(64);
        if self.expanded.len() < words {
            self.expanded.resize(words, 0);
        }
        self.rebuild(tree);
        while let Some(s) = self.selected {
            if self.row_index(s).is_some() {
                break;
            }
            let p = tree.parent[s];
            self.selected = (!p.is_none()).then_some(p);
        }
        let new = match (self.selected_index(), anchor) {
            (Some(i), Some((id, row))) if Some(id) == self.selected => (i as isize - row).max(0) as usize,
            _ => scroll,
        };
        new.min(self.rows.len().saturating_sub(1))
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
        tree.flags[id].contains(NodeFlags::DIR) && tree.has_children(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{TreeBuilder, tests::sample};

    // sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
    fn ids(m: &RowModel) -> Vec<u32> {
        m.rows().iter().map(|r| r.id.0).collect()
    }

    /// Every cached sorted list equals a fresh sort of the same parent's live children.
    fn assert_cache_fresh(m: &RowModel, tree: &Tree) {
        for (&p, list) in &m.cache {
            if !p.is_none() && tree.flags[p].contains(NodeFlags::DEAD) {
                continue; // nothing reads the list of a removed folder
            }
            let mut want: Vec<NodeId> = if p.is_none() { tree.roots.clone() } else { tree.children(p).collect() };
            want.sort_unstable_by(|&a, &b| m.order(tree, a, b));
            assert_eq!(&list[..], &want[..], "stale sorted list for parent {p:?}");
        }
    }

    #[test]
    fn repositioning_keeps_every_cached_list_equal_to_a_fresh_sort() {
        for key in [SortKey::Size, SortKey::Items, SortKey::Name] {
            let mut tree = crate::synth::kary_builder(400, 5).finish().unwrap();
            let mut m = RowModel::new(&tree);
            m.set_sort(&tree, key, true);
            for id in 0..80 {
                m.expand(&tree, NodeId(id)); // fills the caches for the upper levels
            }
            let mut seed = 0x9E37_79B9_7F4A_7C15u64;
            let mut rnd = |n: u64| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (seed >> 33) % n
            };
            for step in 0..400 {
                let live: Vec<NodeId> = (0..tree.len() as u32).map(NodeId).filter(|&n| !tree.flags[n].contains(NodeFlags::DEAD)).collect();
                let dirs: Vec<NodeId> = live.iter().copied().filter(|&n| tree.flags[n].contains(NodeFlags::DIR)).collect();
                let pick = |v: &[NodeId], r: u64| v[r as usize % v.len()];
                for _ in 0..1 + rnd(3) {
                    match rnd(5) {
                        0 => {
                            let name = format!("n{step}x{}", rnd(100)).encode_utf16().collect::<Vec<u16>>();
                            let (dir, size) = (pick(&dirs, rnd(1000)), rnd(5000));
                            tree.add_child(dir, &name, size, NodeFlags::empty());
                        }
                        1 => tree.set_size(pick(&live, rnd(1000)), rnd(5000)),
                        2 => {
                            let id = pick(&live, rnd(1000));
                            tree.remove(id);
                        }
                        3 => {
                            let name = format!("r{step}{}", rnd(100)).encode_utf16().collect::<Vec<u16>>();
                            tree.rename(pick(&live, rnd(1000)), &name);
                        }
                        _ => {
                            let (id, to) = (pick(&live, rnd(1000)), pick(&dirs, rnd(1000)));
                            tree.reparent(id, to);
                        }
                    }
                }
                m.refresh(&mut tree, 0);
                assert_cache_fresh(&m, &tree);
            }
        }
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
}
