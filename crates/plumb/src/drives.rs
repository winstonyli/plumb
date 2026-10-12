//! Drive enumeration for the picker.
use crate::app::DriveInfo;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOTE, DRIVE_REMOVABLE};
use windows::core::PCWSTR;

/// Fixed, removable and network drives that report a size, in letter order.
pub fn list() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u32)
        .filter(|i| mask >> i & 1 == 1)
        .filter_map(|i| {
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
            let root = PCWSTR(root.as_ptr());
            let kind = unsafe { GetDriveTypeW(root) };
            if ![DRIVE_FIXED, DRIVE_REMOVABLE, DRIVE_REMOTE].contains(&kind) {
                return None;
            }
            let (mut free, mut total) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(root, Some(&mut free), Some(&mut total), None) }.ok()?;
            (total > 0).then_some(DriveInfo { letter, total, free })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_the_system_drive_with_sane_sizes() {
        let d = super::list();
        let c = d.iter().find(|d| d.letter == 'C').expect("C: present");
        assert!(c.total > 0 && c.free <= c.total);
        assert!(d.windows(2).all(|w| w[0].letter < w[1].letter));
    }
}
