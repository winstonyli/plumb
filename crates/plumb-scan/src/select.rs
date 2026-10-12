//! Picks the scanner for a volume: the MFT reader when the volume is NTFS and the process is
//! elevated, the directory walker otherwise, and the walker again if the MFT scan fails.

use crate::merge::MergeStats;
use crate::mft_scan::MftTimings;
use crate::volume::{MftScanner, drive_letter, is_elevated, is_ntfs};
use crate::walk::WalkScanner;
use crate::{Progress, ScanError, Scanner};
use plumb_core::TreeBuilder;
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Used {
    Mft,
    Walk,
}

#[derive(Debug)]
pub struct ScanReport {
    pub used: Used,
    /// Something the user should be told (a fallback, or that elevation would be faster).
    pub note: Option<String>,
    /// Present when the MFT scanner ran.
    pub mft: Option<(MergeStats, MftTimings)>,
}

/// Scans `root` with the best available scanner.
pub fn scan_auto(root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<ScanReport, ScanError> {
    let mut note = None;
    if drive_letter(root).is_some() && is_ntfs(root) {
        if is_elevated() {
            match MftScanner::default().scan_with_stats(root, out, progress) {
                Ok(mft) => return Ok(ScanReport { used: Used::Mft, note: None, mft: Some(mft) }),
                Err(e) => {
                    // The failed attempt added nothing to `out`; undo its progress counts too.
                    for c in [&progress.dirs, &progress.files, &progress.bytes] {
                        c.store(0, Ordering::Relaxed);
                    }
                    let ScanError::Mft(msg) = &e else { return Err(e) };
                    note = Some(format!("MFT scan failed ({msg}); used the directory walker instead"));
                }
            }
        } else {
            note = Some("scanned with the directory walker; running as administrator is faster".into());
        }
    }
    WalkScanner::default().scan(root, out, progress)?;
    Ok(ScanReport { used: Used::Walk, note, mft: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_roots_are_recognised() {
        for (s, want) in [("C:", Some('C')), ("c:\\", Some('C')), ("C:/", Some('C')), ("C:\\Users", None), ("\\\\srv\\share", None), ("1:\\", None), ("", None)] {
            assert_eq!(drive_letter(Path::new(s)), want, "{s}");
        }
    }

    #[test]
    fn a_directory_is_walked_without_a_note() {
        let dir = std::env::temp_dir().join(format!("plumb-select-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        let mut b = TreeBuilder::new();
        let r = scan_auto(&dir, &mut b, &Progress::default()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((r.used, r.note, r.mft.is_none()), (Used::Walk, None, true));
        assert_eq!(b.len(), 2); // the directory and its file
    }

    #[test]
    fn without_elevation_the_mft_scanner_refuses_and_leaves_the_builder_empty() {
        if is_elevated() {
            return; // covered by the elevated check in the plan
        }
        let mut b = TreeBuilder::new();
        let tmp = std::env::temp_dir();
        let root = tmp.ancestors().last().unwrap().to_path_buf(); // e.g. C:\
        if !is_ntfs(&root) {
            return;
        }
        assert!(!is_elevated());
        assert!(MftScanner::default().scan_with_stats(&root, &mut b, &Progress::default()).is_err());
        assert!(b.is_empty());
    }
}
