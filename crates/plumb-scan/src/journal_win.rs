//! The Windows side of the journal reader: `JournalSource` for an open NTFS volume. Needs
//! administrator rights, like the rest of the volume access. Nothing here can be unit-tested
//! without elevation; the `journal-check` example exercises it end to end.

use crate::journal::usn::{REF_MASK, UsnBatch, parse_usn_buffer};
use crate::journal::{JournalError, JournalInfo, JournalSource};
use crate::volume::Volume;
use std::ffi::c_void;
use std::io;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileIdType, FileStandardInfo, GetFileInformationByHandleEx, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_FILE_RECORD, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, NTFS_FILE_RECORD_INPUT_BUFFER,
    READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0, USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE,
    USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_HARD_LINK_CHANGE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_REPARSE_POINT_CHANGE,
};

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;

/// What changes a file's name, place, size, kind or link status. Timestamps and attributes
/// (`BASIC_INFO_CHANGE`) and the `CLOSE` marker change nothing plumb shows, so they are not asked for.
const REASONS: u32 = USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_REPARSE_POINT_CHANGE
    | USN_REASON_HARD_LINK_CHANGE;

const READ_BUFFER: usize = 64 * 1024;
const RECORD_HEADER: usize = 12; // NTFS_FILE_RECORD_OUTPUT_BUFFER up to FileRecordBuffer

fn win32(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

fn journal_error(e: &windows::core::Error, what: &str) -> JournalError {
    match win32(e) {
        ERROR_JOURNAL_ENTRY_DELETED => JournalError::Wrapped,
        ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => JournalError::Disabled,
        // Unverified: what a stale journal ID returns. The journal-spike example prints the code.
        ERROR_INVALID_PARAMETER if what == "reading the journal" => JournalError::Wrapped,
        code => JournalError::Other(format!("{what} failed (error {code}): {e}")),
    }
}

impl Volume {
    fn ioctl<I, O>(&self, code: u32, input: Option<&I>, output: *mut O, out_len: usize) -> windows::core::Result<u32> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.map(|i| (i as *const I).cast::<c_void>()),
                input.map_or(0, |_| std::mem::size_of::<I>() as u32),
                Some(output.cast::<c_void>()),
                out_len as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok(returned)
    }
}

impl JournalSource for Volume {
    fn query(&self) -> Result<JournalInfo, JournalError> {
        let mut d = USN_JOURNAL_DATA_V0::default();
        self.ioctl::<(), _>(FSCTL_QUERY_USN_JOURNAL, None, &mut d, std::mem::size_of::<USN_JOURNAL_DATA_V0>())
            .map_err(|e| journal_error(&e, "querying the journal"))?;
        Ok(JournalInfo { id: d.UsnJournalID, next_usn: d.NextUsn })
    }

    fn read(&self, from: i64, journal_id: u64, wait: bool) -> Result<UsnBatch, JournalError> {
        let input = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: from,
            ReasonMask: REASONS,
            ReturnOnlyOnClose: 0,
            // Unverified units: the journal-check example times an idle wait to confirm about a second.
            Timeout: u64::from(wait),
            BytesToWaitFor: u64::from(wait),
            UsnJournalID: journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 2,
        };
        let mut buf = vec![0u8; READ_BUFFER];
        let n = self
            .ioctl(FSCTL_READ_USN_JOURNAL, Some(&input), buf.as_mut_ptr(), buf.len())
            .map_err(|e| journal_error(&e, "reading the journal"))?;
        parse_usn_buffer(&buf[..n as usize]).ok_or_else(|| JournalError::Other("the journal returned a malformed buffer".into()))
    }

    fn file_record(&self, record: u64, buf: &mut [u8]) -> io::Result<Option<u64>> {
        let input = NTFS_FILE_RECORD_INPUT_BUFFER { FileReferenceNumber: record as i64 };
        let mut out = vec![0u8; RECORD_HEADER + buf.len() + 1024];
        match self.ioctl(FSCTL_GET_NTFS_FILE_RECORD, Some(&input), out.as_mut_ptr(), out.len()) {
            Ok(n) => {
                let n = n as usize;
                let number = u64::from_le_bytes(out[0..8].try_into().unwrap()) & REF_MASK;
                let len = u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize;
                if len != buf.len() || n < RECORD_HEADER + len {
                    return Err(io::Error::other(format!("record {number} is {len} bytes, expected {}", buf.len())));
                }
                buf.copy_from_slice(&out[RECORD_HEADER..RECORD_HEADER + len]);
                Ok(Some(number))
            }
            Err(e) if matches!(win32(&e), ERROR_FILE_NOT_FOUND | ERROR_HANDLE_EOF) => Ok(None),
            Err(e) => Err(io::Error::from_raw_os_error(win32(&e) as i32)),
        }
    }

    fn allocated_size(&self, frn: u64) -> io::Result<Option<u64>> {
        let mut id = FILE_ID_DESCRIPTOR { dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32, Type: FileIdType, ..Default::default() };
        id.Anonymous.FileId = frn as i64;
        let h = match unsafe {
            OpenFileById(
                self.0,
                &id,
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        } {
            Ok(h) => h,
            Err(e) if matches!(win32(&e), ERROR_FILE_NOT_FOUND | ERROR_INVALID_PARAMETER) => return Ok(None),
            Err(e) => return Err(io::Error::from_raw_os_error(win32(&e) as i32)),
        };
        let mut info = FILE_STANDARD_INFO::default();
        let r = unsafe {
            GetFileInformationByHandleEx(h, FileStandardInfo, (&mut info as *mut FILE_STANDARD_INFO).cast(), std::mem::size_of::<FILE_STANDARD_INFO>() as u32)
        };
        close(h);
        r.map_err(|e| io::Error::from_raw_os_error(win32(&e) as i32))?;
        Ok((!info.DeletePending).then_some(info.AllocationSize.max(0) as u64))
    }
}

fn close(h: HANDLE) {
    unsafe {
        let _ = CloseHandle(h);
    }
}
