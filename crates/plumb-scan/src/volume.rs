//! The Windows side of the MFT scanner: elevation check, file-system check, the raw volume handle
//! and `MftScanner`. Opening a volume needs administrator rights.

use crate::mft_scan::{MftLayout, MftOptions, MftReport, VolumeRead, scan_mft};
use crate::{Progress, ScanError, Scanner};
use plumb_core::TreeBuilder;
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, OPEN_EXISTING, ReadFile,
};
use windows::Win32::System::IO::{DeviceIoControl, OVERLAPPED};
use windows::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

/// True if this process runs with an elevated (administrator) token.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// The device path of the volume mounted exactly at `root` (a drive root like `C:\` or a folder a
/// volume is mounted on): `\\?\Volume{guid}`, no trailing backslash, NUL-terminated. `None` for any other path.
pub fn volume_device(root: &Path) -> Option<Vec<u16>> {
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    if wide.is_empty() {
        return None;
    }
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let mut name = [0u16; 64];
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(wide.as_ptr()), &mut name) }.ok()?;
    let len = name.iter().position(|&c| c == 0)?;
    let mut device = name[..len].to_vec();
    if device.last() == Some(&(b'\\' as u16)) {
        device.pop();
    }
    device.push(0);
    Some(device)
}

/// True if `root` is the root of a volume: a drive root or a mount-point folder.
pub fn is_volume_root(root: &Path) -> bool {
    volume_device(root).is_some()
}

/// True if the volume mounted at `root` (a path like `C:\`) is NTFS.
pub fn is_ntfs(root: &Path) -> bool {
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let mut fs = [0u16; 32];
    let ok = unsafe { GetVolumeInformationW(PCWSTR(wide.as_ptr()), None, None, None, None, Some(&mut fs)) }.is_ok();
    ok && String::from_utf16_lossy(&fs).trim_end_matches('\0') == "NTFS"
}

/// An open raw volume, read-only.
pub struct Volume(pub(crate) HANDLE);

// A volume handle is only read through `ReadFile` with explicit offsets.
unsafe impl Send for Volume {}
unsafe impl Sync for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Volume {
    /// Opens the volume mounted at `root` (see [`volume_device`]).
    pub fn open(root: &Path) -> io::Result<Volume> {
        let path = volume_device(root).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a volume root"))?;
        let h = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_SEQUENTIAL_SCAN,
                None,
            )
        }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        Ok(Volume(h))
    }

    pub fn layout(&self) -> io::Result<MftLayout> {
        let mut d = NTFS_VOLUME_DATA_BUFFER::default();
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.0,
                FSCTL_GET_NTFS_VOLUME_DATA,
                None,
                0,
                Some((&mut d as *mut NTFS_VOLUME_DATA_BUFFER).cast::<c_void>()),
                std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        Ok(MftLayout {
            bytes_per_sector: d.BytesPerSector,
            bytes_per_cluster: d.BytesPerCluster,
            bytes_per_record: d.BytesPerFileRecordSegment,
            mft_start_lcn: d.MftStartLcn as u64,
            mft_valid_len: d.MftValidDataLength as u64,
        })
    }
}

impl VolumeRead for Volume {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let mut o = OVERLAPPED::default();
            o.Anonymous.Anonymous.Offset = pos as u32;
            o.Anonymous.Anonymous.OffsetHigh = (pos >> 32) as u32;
            let mut got = 0u32;
            unsafe { ReadFile(self.0, Some(&mut buf[done..]), Some(&mut got), Some(&mut o)) }
                .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read from volume"));
            }
            done += got as usize;
        }
        Ok(())
    }
}

/// Reads the MFT of an NTFS volume directly. Needs administrator rights.
#[derive(Default)]
pub struct MftScanner {
    pub options: MftOptions,
}

impl MftScanner {
    /// Like [`Scanner::scan`], but also returns the merge statistics and phase timings.
    /// `out` is untouched on error, so the caller can fall back to another scanner.
    pub fn scan_with_stats(
        &self,
        root: &Path,
        out: &mut TreeBuilder,
        progress: &Progress,
    ) -> Result<MftReport, ScanError> {
        let shown = root.to_string_lossy();
        let name = shown.trim_end_matches(['\\', '/']);
        let vol = Volume::open(root).map_err(|e| ScanError::Mft(format!("cannot open volume {name}: {e}")))?;
        let layout = vol.layout().map_err(|e| ScanError::Mft(format!("cannot query volume {name}: {e}")))?;
        scan_mft(&vol, &layout, name, &self.options, out, progress)
            .map_err(|e| match e {
                crate::mft_scan::MftError::Cancelled => ScanError::Cancelled,
                e => ScanError::Mft(e.to_string()),
            })
    }
}

impl Scanner for MftScanner {
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError> {
        self.scan_with_stats(root, out, progress).map(|_| ())
    }
}
