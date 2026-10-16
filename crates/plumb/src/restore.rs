//! Carries the view over to a rescanned tree: what is expanded, what is selected and where it
//! sits on screen, all by path, since node ids mean nothing in the new tree.
use plumb_core::rows::{RowModel, SortKey};
use plumb_core::Tree;

pub struct Snapshot {
    /// Paths of the expanded folders that are on screen, shallowest first.
    expanded: Vec<String>,
    selected: Option<String>,
    /// Screen row the selection was on.
    screen_row: usize,
    scroll: usize,
}

impl Snapshot {
    pub fn take(tree: &Tree, model: &RowModel, scroll: usize) -> Self {
        let mut expanded: Vec<String> =
            model.rows().iter().filter(|r| model.is_expanded(r.id)).map(|r| tree.path(r.id)).collect();
        expanded.sort_by_key(|p| p.matches('\\').count());
        let selected = model.selected().map(|s| tree.path(s));
        let screen_row = model.selected_index().map_or(0, |i| i.saturating_sub(scroll));
        Self { expanded, selected, screen_row, scroll }
    }

    /// A model for `tree` with the same sort, the same folders expanded and the same row selected
    /// (or its nearest surviving ancestor), plus the scroll that keeps it on the same screen row.
    /// Paths that no longer exist are dropped.
    pub fn apply(&self, tree: &Tree, key: SortKey, desc: bool, visible: usize) -> (RowModel, usize) {
        let mut model = RowModel::new(tree);
        model.set_sort(tree, key, desc);
        for p in &self.expanded {
            if let Some(id) = tree.find(p) {
                model.expand(tree, id);
            }
        }
        let found = self.selected.as_deref().and_then(|mut p| loop {
            if let Some(id) = tree.find(p) {
                break Some(id);
            }
            p = &p[..p.rfind('\\')?];
        });
        let max = model.rows().len().saturating_sub(visible);
        let scroll = match found {
            Some(id) => {
                model.select(id);
                model.selected_index().map_or(0, |i| i.saturating_sub(self.screen_row))
            }
            None => self.scroll,
        };
        (model, scroll.min(max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::{NodeFlags, NodeId, TreeBuilder};

    /// C:\{a\{x, y}, b\{z}, big} with a few filler files in a so scrolling means something.
    fn tree(drop_b: bool) -> Tree {
        let mut t = TreeBuilder::new();
        let root = t.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let a = t.push_str(root, "a", 0, NodeFlags::DIR);
        for i in 0..30 {
            t.push_str(a, &format!("f{i:02}"), 10 + i, NodeFlags::empty());
        }
        t.push_str(a, "x", 5, NodeFlags::empty());
        if !drop_b {
            let b = t.push_str(root, "b", 0, NodeFlags::DIR);
            t.push_str(b, "z", 7, NodeFlags::empty());
        }
        t.push_str(root, "big", 1000, NodeFlags::empty());
        t.finish().unwrap()
    }

    fn state(t: &Tree) -> (RowModel, usize) {
        let mut m = RowModel::new(t);
        m.set_sort(t, SortKey::Name, false);
        let root = m.rows()[0].id;
        m.expand(t, root);
        let a = t.find("C:\\a").unwrap();
        m.expand(t, a);
        m.select(t.find("C:\\a\\f10").unwrap());
        let scroll = m.selected_index().unwrap() - 4;
        (m, scroll)
    }

    #[test]
    fn paths_resolve_exactly() {
        let t = tree(false);
        assert_eq!(t.find("C:"), Some(NodeId(0)));
        assert_eq!(t.path(t.find("C:\\b\\z").unwrap()), "C:\\b\\z");
        assert_eq!(t.find("C:\\B"), None);
        assert_eq!(t.find("D:"), None);
    }

    #[test]
    fn expansion_selection_and_screen_row_survive_a_rescan() {
        let old = tree(false);
        let (m, scroll) = state(&old);
        let snap = Snapshot::take(&old, &m, scroll);
        let new = tree(false);
        let (m2, s2) = snap.apply(&new, SortKey::Name, false, 20);
        assert_eq!(new.path(m2.selected().unwrap()), "C:\\a\\f10");
        assert_eq!(m2.selected_index().unwrap() - s2, 4);
        assert!(m2.is_expanded(new.find("C:\\a").unwrap()));
        assert!(!m2.is_expanded(new.find("C:\\b").unwrap()));
    }

    #[test]
    fn a_selection_that_is_gone_falls_back_to_its_nearest_surviving_ancestor() {
        let old = tree(false);
        let mut m = RowModel::new(&old);
        let root = m.rows()[0].id;
        m.expand(&old, root);
        let b = old.find("C:\\b").unwrap();
        m.expand(&old, b);
        m.select(old.find("C:\\b\\z").unwrap());
        let snap = Snapshot::take(&old, &m, 0);
        let new = tree(true);
        let (m2, _) = snap.apply(&new, SortKey::Name, false, 20);
        assert_eq!(new.path(m2.selected().unwrap()), "C:");
    }

    #[test]
    fn scroll_is_clamped_when_the_new_tree_is_shorter() {
        let old = tree(false);
        let (m, scroll) = state(&old);
        let mut snap = Snapshot::take(&old, &m, scroll);
        snap.selected = None;
        snap.expanded.clear();
        snap.scroll = 500;
        let (m2, s2) = snap.apply(&old, SortKey::Name, false, 20);
        assert_eq!(s2, m2.rows().len().saturating_sub(20));
    }
}
