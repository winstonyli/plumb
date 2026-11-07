use bitflags::bitflags;
use std::ops::{Index, IndexMut};

/// Index of a node in a [`crate::Tree`]. `NodeId::NONE` means "no node".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeFlags {
    /// SYSTEM and CLOUD, from a Win32 file-attribute word (hidden or system; offline, recall-on-open or
    /// recall-on-data-access).
    pub fn from_attributes(attrs: u32) -> NodeFlags {
        const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
        const CLOUD: u32 = 0x1000 | 0x4_0000 | 0x40_0000;
        let mut f = NodeFlags::empty();
        f.set(NodeFlags::SYSTEM, attrs & HIDDEN_OR_SYSTEM != 0);
        f.set(NodeFlags::CLOUD, attrs & CLOUD != 0);
        f
    }
}

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
        /// A hidden or system file or folder.
        const SYSTEM = 32;
        /// A cloud placeholder: its data is not on this disk until it is opened.
        const CLOUD = 64;
        /// What a scanner reads from the directory entry itself besides folder or file.
        const TRAITS = Self::REPARSE.bits() | Self::SYSTEM.bits() | Self::CLOUD.bits();
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
    fn attributes_give_system_for_hidden_or_system_and_cloud_for_a_placeholder() {
        assert_eq!(NodeFlags::from_attributes(0x20), NodeFlags::empty(), "archive only");
        assert_eq!(NodeFlags::from_attributes(0x2), NodeFlags::SYSTEM);
        assert_eq!(NodeFlags::from_attributes(0x4 | 0x20), NodeFlags::SYSTEM);
        assert_eq!(NodeFlags::from_attributes(0x40_0000), NodeFlags::CLOUD);
        assert_eq!(NodeFlags::from_attributes(0x1000 | 0x2), NodeFlags::SYSTEM | NodeFlags::CLOUD);
    }

    #[test]
    fn vec_indexes_by_node_id() {
        #[allow(clippy::useless_vec)] // the impl under test is for Vec, not arrays
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
