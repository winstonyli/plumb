//! `NtQuerySystemInformation` and `NtQueryInformationProcess`, which have no import library: found in ntdll at run time.
use std::ffi::c_void;
use windows::core::s;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

type NtQuery = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;

/// The system information of `class`, in a buffer of 8-byte words (so structs with u64 fields can be read in place).
/// The buffer starts at `initial` bytes and grows while the answer does not fit; `Err` carries the NTSTATUS
/// (0 when ntdll has no such function).
pub fn query(class: u32, initial: usize) -> Result<Vec<u64>, i32> {
    let nt: NtQuery = unsafe {
        let ntdll = GetModuleHandleA(s!("ntdll.dll")).map_err(|e| e.code().0)?;
        std::mem::transmute(GetProcAddress(ntdll, s!("NtQuerySystemInformation")).ok_or(0)?)
    };
    let mut buf: Vec<u64> = vec![0; initial.div_ceil(8)];
    loop {
        let mut need = 0u32;
        let status = unsafe { nt(class, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut need) };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // The answer can grow between the two calls: leave room.
            buf = vec![0; ((need as usize).max(buf.len() * 8) * 2).div_ceil(8)];
            continue;
        }
        return if status < 0 { Err(status) } else { Ok(buf) };
    }
}

type NtQueryProcess = unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;

/// The information of `class` about `process`, in a buffer of 8-byte words, grown as `query` does; `Err` carries the
/// NTSTATUS (0 when ntdll has no such function).
pub fn query_process(process: HANDLE, class: u32, initial: usize) -> Result<Vec<u64>, i32> {
    let nt: NtQueryProcess = unsafe {
        let ntdll = GetModuleHandleA(s!("ntdll.dll")).map_err(|e| e.code().0)?;
        std::mem::transmute(GetProcAddress(ntdll, s!("NtQueryInformationProcess")).ok_or(0)?)
    };
    let mut buf: Vec<u64> = vec![0; initial.div_ceil(8)];
    loop {
        let mut need = 0u32;
        let status = unsafe { nt(process, class, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut need) };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            buf = vec![0; ((need as usize).max(buf.len() * 8) * 2).div_ceil(8)];
            continue;
        }
        return if status < 0 { Err(status) } else { Ok(buf) };
    }
}
