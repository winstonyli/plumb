//! Picks the scanner for a volume: the MFT reader when the volume is NTFS and the process is
//! elevated, the directory walker otherwise, and the walker again if the MFT scan fails.

use crate::mft_scan::MftReport;
use crate::volume::{MftScanner, is_elevated, is_ntfs, is_volume_root};
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
    pub mft: Option<MftReport>,
}

/// Scans `root` with the best available scanner.
pub fn scan_auto(root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<ScanReport, ScanError> {
    let mut note = None;
    if is_volume_root(root) && is_ntfs(root) {
        if is_elevated() {
            match MftScanner::default().scan_with_stats(root, out, progress) {
                Ok(mft) => return Ok(ScanReport { used: Used::Mft, note: None, mft: Some(mft) }),
                Err(e) => {
                    // The failed attempt added nothing to `out`; undo its progress counts too.
                    for c in [&progress.dirs, &progress.files, &progress.bytes] {
                        c.store(0, Ordering::Relaxed);
                    }
                    if let Some(p) = &progress.preview {
                        p.reset();
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
    fn only_whole_volumes_are_volume_roots() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        for s in [system.clone(), format!("{system}\\"), format!("{}\\", system.to_lowercase())] {
            let device = crate::volume::volume_device(Path::new(&s)).unwrap_or_else(|| panic!("{s} is a volume root"));
            let text = String::from_utf16_lossy(&device[..device.len() - 1]);
            assert!(text.starts_with(r"\\?\Volume{") && !text.ends_with('\\'), "{text}");
        }
        for s in [format!("{system}\\Windows"), "\\\\srv\\share".into(), "1:\\".into(), String::new()] {
            assert!(!is_volume_root(Path::new(&s)), "{s}");
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
