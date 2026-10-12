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
        /// Removed after the scan; the slot stays so `NodeId`s never change.
        const DEAD = 16;
    }
}

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
