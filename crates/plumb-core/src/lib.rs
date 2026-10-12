//! Pure, UI-free data model for plumb: the tree, the row model and the MFT record parser.

pub mod mft;
pub mod node;
pub mod rows;
#[doc(hidden)]
pub mod synth;
pub mod tree;

pub use node::{NodeFlags, NodeId};
pub use tree::{Tree, TreeBuilder, TreeError};
