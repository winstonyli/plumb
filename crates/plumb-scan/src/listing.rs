//! What a directory looked like when it was listed. Shared by the walker and the directory watch.

/// One directory entry. `alloc` is allocated bytes (0 for directories and for data that lives
/// inside the MFT record), as the walker counts them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u16>,
    pub alloc: u64,
    pub is_dir: bool,
    /// A symlink or junction: listed, never followed.
    pub reparse: bool,
    /// The file's id on its volume, the same for every hard link to it (the 128-bit id folded to 64 bits;
    /// NTFS ids fit in 64). Only meaningful for files.
    pub id: u64,
}

/// A directory and its entries, or `Err` if it could not be listed (it is gone, or not readable).
/// `dir` is the path below the watched root, `\`-separated, empty for the root itself.
#[derive(Debug)]
pub struct Listing {
    pub dir: Vec<u16>,
    pub entries: Result<Vec<Entry>, ()>,
}
