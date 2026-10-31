//! The paths marked for deletion and the arithmetic over them. Pure.
use crate::node::NodeId;
use crate::tree::Tree;
use std::collections::HashSet;

/// Marked paths, in the order they were marked, as [`Tree::path`] gives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Basket {
    paths: Vec<String>,
}

impl Basket {
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.iter().any(|p| p == path)
    }

    /// Whether `path` is marked or lies under a marked folder.
    pub fn covers(&self, path: &str) -> bool {
        self.paths.iter().any(|p| path == p || path.strip_prefix(p.as_str()).is_some_and(|rest| rest.starts_with('\\')))
    }

    /// Marks `path`, or unmarks it if it was marked; true when it is marked now.
    pub fn toggle(&mut self, path: &str) -> bool {
        match self.paths.iter().position(|p| p == path) {
            Some(i) => {
                self.paths.remove(i);
                false
            }
            None => {
                self.paths.push(path.to_string());
                true
            }
        }
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// Drops the marks that are one of `gone` or lie under one: what a deleted folder takes with it.
    pub fn remove_under(&mut self, gone: &[String]) {
        self.paths.retain(|p| !gone.iter().any(|g| p == g || p.strip_prefix(g.as_str()).is_some_and(|r| r.starts_with('\\'))));
    }

    /// Drops the marks that no longer resolve in `tree`.
    pub fn prune(&mut self, tree: &Tree) {
        self.paths.retain(|p| tree.find(p).is_some());
    }

    /// The nodes the marks stand for that still exist and have no marked ancestor: what a delete touches.
    pub fn covered(&self, tree: &Tree) -> Vec<NodeId> {
        let ids: Vec<NodeId> = self.paths.iter().filter_map(|p| tree.find(p)).collect();
        let set: HashSet<NodeId> = ids.iter().copied().collect();
        ids.into_iter()
            .filter(|&id| {
                let mut up = tree.parent[id];
                while !up.is_none() {
                    if set.contains(&up) {
                        return false;
                    }
                    up = tree.parent[up];
                }
                true
            })
            .collect()
    }
}

/// Why `id` may not be marked, or `None` if it may: a drive root, or `system_root` ("C:\Windows") and anything under it.
pub fn refusal(tree: &Tree, id: NodeId, system_root: &str) -> Option<&'static str> {
    if tree.parent[id].is_none() {
        return Some("a drive root cannot be deleted");
    }
    let path = tree.path(id);
    let starts = |a: &str, b: &str| a.get(..b.len()).is_some_and(|h| h.eq_ignore_ascii_case(b)) && (a.len() == b.len() || a.as_bytes()[b.len()] == b'\\');
    if starts(&path, system_root) || starts(system_root, &path) {
        return Some("the Windows folder is protected");
    }
    // Win32 rewrites these names (a trailing dot or space is dropped), so a delete could hit another file; the orphan folder is not a real one.
    let mut parts = path.split('\\').skip(1);
    if parts.any(|p| p == "<orphans>" || p.ends_with('.') || p.ends_with(' ')) {
        return Some("this name cannot be deleted safely");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeFlags;
    use crate::tree::TreeBuilder;

    /// C:\{a\{x 10, y 20}, Windows\{sys 500}, big 1000}
    fn tree() -> Tree {
        let mut b = TreeBuilder::new();
        let c = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let a = b.push_str(c, "a", 0, NodeFlags::DIR);
        b.push_str(a, "x", 10, NodeFlags::empty());
        b.push_str(a, "y", 20, NodeFlags::empty());
        let w = b.push_str(c, "Windows", 0, NodeFlags::DIR);
        b.push_str(w, "sys", 500, NodeFlags::empty());
        b.push_str(c, "big", 1000, NodeFlags::empty());
        b.finish().unwrap()
    }

    #[test]
    fn toggle_marks_then_unmarks() {
        let mut b = Basket::default();
        assert!(b.toggle(r"C:\a"));
        assert!(b.contains(r"C:\a"));
        assert!(!b.toggle(r"C:\a"));
        assert!(b.is_empty());
    }

    #[test]
    fn covers_a_marked_folders_contents_but_not_a_sibling_with_the_same_prefix() {
        let mut b = Basket::default();
        b.toggle(r"C:\a");
        assert!(b.covers(r"C:\a") && b.covers(r"C:\a\x"));
        assert!(!b.covers(r"C:\ab"), "C:\\ab is not inside C:\\a");
    }

    #[test]
    fn a_marked_folder_and_its_child_count_the_folder_once() {
        let t = tree();
        let mut b = Basket::default();
        b.toggle(r"C:\a\x");
        b.toggle(r"C:\a");
        b.toggle(r"C:\big");
        let ids = b.covered(&t);
        let mut paths: Vec<String> = ids.iter().map(|&i| t.path(i)).collect();
        paths.sort();
        assert_eq!(paths, vec![r"C:\a".to_string(), r"C:\big".to_string()]);
    }

    #[test]
    fn a_path_that_no_longer_resolves_is_dropped_and_a_removed_node_too() {
        let mut t = tree();
        let mut b = Basket::default();
        b.toggle(r"C:\nope");
        b.toggle(r"C:\big");
        assert_eq!(b.covered(&t).len(), 1);
        let big = t.find(r"C:\big").unwrap();
        assert!(t.remove(big));
        assert!(b.covered(&t).is_empty(), "a dead node does not resolve");
    }

    #[test]
    fn roots_and_the_windows_folder_are_refused() {
        let t = tree();
        let at = |p: &str| t.find(p).unwrap();
        assert!(refusal(&t, at("C:"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\Windows"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\Windows\sys"), r"c:\windows").is_some(), "case does not matter");
        assert!(refusal(&t, at(r"C:\a"), r"C:\Windows").is_none());
        assert!(refusal(&t, at(r"C:\big"), r"C:\Win").is_none(), "a longer sibling name is not inside");
    }

    #[test]
    fn names_win32_would_rewrite_orphans_and_ancestors_of_the_system_root_are_refused() {
        let mut b = TreeBuilder::new();
        let c = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let d = b.push_str(c, "d", 0, NodeFlags::DIR);
        b.push_str(d, "report.", 5, NodeFlags::empty());
        b.push_str(d, "pad ", 5, NodeFlags::empty());
        let o = b.push_str(c, "<orphans>", 0, NodeFlags::DIR);
        b.push_str(o, "lost", 5, NodeFlags::empty());
        let os = b.push_str(c, "OS", 0, NodeFlags::DIR);
        b.push_str(os, "Windows", 0, NodeFlags::DIR);
        let t = b.finish().unwrap();
        let at = |p: &str| t.find(p).unwrap();
        assert!(refusal(&t, at(r"C:\d\report."), r"C:\Windows").is_some(), "trailing dot");
        assert!(refusal(&t, at(r"C:\d\pad "), r"C:\Windows").is_some(), "trailing space");
        assert!(refusal(&t, at(r"C:\<orphans>"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\<orphans>\lost"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\OS"), r"C:\OS\Windows").is_some(), "an ancestor of the system root");
        assert!(refusal(&t, at(r"C:\d"), r"C:\OS\Windows").is_none());
    }

    #[test]
    fn prune_drops_marks_that_no_longer_resolve_and_remove_under_drops_a_gone_folders_marks() {
        let t = tree();
        let mut b = Basket::default();
        for p in [r"C:\a", r"C:\a\x", r"C:\nope", r"C:\big"] {
            b.toggle(p);
        }
        b.prune(&t);
        assert!(!b.contains(r"C:\nope") && b.len() == 3);
        b.remove_under(&[r"C:\a".to_string()]);
        assert_eq!(b.len(), 1);
        assert!(b.contains(r"C:\big"));
    }
}
