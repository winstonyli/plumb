//! Drive enumeration for the picker.
use crate::app::ViewInfo;
use windows::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumePathNamesForVolumeNameW,
};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOTE, DRIVE_REMOVABLE};
use windows::core::PCWSTR;

/// Fixed, removable and network drives that report a size, in letter order, then volumes that are
/// only mounted on a folder (no drive letter), by path; then the Processes view, whose "size" is the machine's memory.
pub fn list() -> Vec<ViewInfo> {
    let mask = unsafe { GetLogicalDrives() };
    let letters = (0..26u32).filter(|i| mask >> i & 1 == 1).filter_map(|i| probe(format!("{}:\\", (b'A' + i as u8) as char)));
    let (total, free) = plumb_scan::procs::physical_memory();
    let processes = ViewInfo { root: "Processes".into(), total, free, kind: crate::app::ViewKind::Processes };
    letters.chain(folder_mounts().into_iter().filter_map(probe)).chain([processes]).collect()
}

/// The drive at `root` if it is a kind the picker offers and reports a size.
fn probe(root: String) -> Option<ViewInfo> {
    let wide: Vec<u16> = root.encode_utf16().chain([0]).collect();
    let wide = PCWSTR(wide.as_ptr());
    let kind = unsafe { GetDriveTypeW(wide) };
    if ![DRIVE_FIXED, DRIVE_REMOVABLE, DRIVE_REMOTE].contains(&kind) {
        return None;
    }
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(wide, Some(&mut free), Some(&mut total), None) }.ok()?;
    (total > 0).then_some(ViewInfo { root, total, free, kind: crate::app::ViewKind::Disk })
}

/// One mount-point folder (with trailing backslash) for each volume that has no drive letter, sorted.
fn folder_mounts() -> Vec<String> {
    let mut found = Vec::new();
    let mut name = [0u16; 64];
    let Ok(find) = (unsafe { FindFirstVolumeW(&mut name) }) else { return found };
    loop {
        if let Some(path) = mount_folder(&name) {
            found.push(path);
        }
        if unsafe { FindNextVolumeW(find, &mut name) }.is_err() {
            break;
        }
    }
    let _ = unsafe { FindVolumeClose(find) };
    found.sort();
    found
}

/// The first folder the volume `name` is mounted on, or `None` if it has a drive letter or no path at all.
fn mount_folder(name: &[u16]) -> Option<String> {
    let mut paths = vec![0u16; 1024];
    let mut len = 0u32;
    unsafe { GetVolumePathNamesForVolumeNameW(PCWSTR(name.as_ptr()), Some(&mut paths), &mut len) }.ok()?;
    let paths: Vec<String> = paths[..len as usize].split(|&c| c == 0).filter(|p| !p.is_empty()).map(String::from_utf16_lossy).collect();
    let letter = |p: &String| p.len() == 3 && p.as_bytes()[1] == b':';
    if paths.iter().any(letter) {
        return None;
    }
    paths.into_iter().min()
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_the_system_drive_with_sane_sizes() {
        let d = super::list();
        let (processes, d): (Vec<_>, Vec<_>) = d.into_iter().partition(|d| d.kind == crate::app::ViewKind::Processes);
        assert_eq!(processes.len(), 1, "one Processes view, after the drives");
        assert!(processes[0].total > 0 && processes[0].free <= processes[0].total);
        let c = d.iter().find(|d| d.root == "C:\\").expect("C: present");
        assert!(c.total > 0 && c.free <= c.total);
        let letters: Vec<&str> = d.iter().map(|d| d.root.as_str()).filter(|r| r.len() == 3).collect();
        assert!(letters.windows(2).all(|w| w[0] < w[1]), "drive letters come in order");
        assert!(d.iter().all(|d| d.root.ends_with('\\')), "every root ends in a backslash");
    }
}
