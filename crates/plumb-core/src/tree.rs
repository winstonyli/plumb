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
