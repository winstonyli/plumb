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
