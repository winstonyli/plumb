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

    pub fn remove_all(&mut self, gone: &[String]) {
        self.paths.retain(|p| !gone.contains(p));
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

    /// Allocated bytes under the nodes in [`Basket::covered`]; an upper bound, since a hard-linked file's other names free nothing.
    pub fn total_bytes(&self, tree: &Tree) -> u64 {
        self.covered(tree).iter().map(|&id| tree.subtree_size[id]).sum()
    }
}

/// Why `id` may not be marked, or `None` if it may: a drive root, or `system_root` ("C:\Windows") and anything under it.
pub fn refusal(tree: &Tree, id: NodeId, system_root: &str) -> Option<&'static str> {
    if tree.parent[id].is_none() {
        return Some("a drive root cannot be deleted");
    }
    let path = tree.path(id);
    let under = path.get(..system_root.len()).is_some_and(|head| head.eq_ignore_ascii_case(system_root))
        && (path.len() == system_root.len() || path.as_bytes()[system_root.len()] == b'\\');
    under.then_some("the Windows folder is protected")
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
        assert_eq!(b.total_bytes(&t), 30 + 1000);
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
        assert_eq!(b.total_bytes(&t), 0);
    }

    #[test]
    fn remove_all_unmarks_only_the_named_paths() {
        let mut b = Basket::default();
        b.toggle(r"C:\a");
        b.toggle(r"C:\big");
        b.remove_all(&[r"C:\a".to_string()]);
        assert_eq!(b.len(), 1);
        assert!(b.contains(r"C:\big"));
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
}
