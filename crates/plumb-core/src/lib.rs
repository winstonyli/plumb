//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod age;
pub mod basket;
pub(crate) mod bits;
pub mod filter;
pub mod mft;
pub mod node;
pub mod procs;
pub mod rect;
pub mod rows;
#[doc(hidden)]
pub mod synth;
pub mod treemap;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use rect::Rect;
pub use tree::{Tree, TreeBuilder, TreeError};

/// `s` as NUL-terminated UTF-16, the form Win32 wide-string arguments take.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
