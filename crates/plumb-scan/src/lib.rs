//! Volume scanners for plumb: they turn a directory tree or an NTFS `$MFT` into a `plumb_core::Tree`.

pub mod merge;
pub mod mft_scan;
#[cfg(windows)]
pub mod select;
#[cfg(windows)]
pub mod volume;
#[cfg(windows)]
pub mod walk;

use plumb_core::TreeBuilder;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counters a scanner updates while it runs. Read them from another thread to show progress.
#[derive(Default)]
pub struct Progress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    /// Allocated bytes seen so far.
    pub bytes: AtomicU64,
}

impl Progress {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.dirs.load(Ordering::Relaxed),
            self.files.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

#[derive(Debug)]
pub enum ScanError {
    /// The root path could not be resolved.
    Root(std::io::Error),
    /// The MFT scanner could not run or failed; the message says why. Nothing was added to the tree.
    Mft(String),
}

pub trait Scanner {
    /// Scans `root` (a drive root like `C:\` or any directory) and appends its nodes to `out`.
    /// The first node pushed is the root, named after the path without a trailing backslash.
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError>;
}
