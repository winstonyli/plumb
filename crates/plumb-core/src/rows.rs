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
