//! The files a process has open: its handles, duplicated into this process and asked for a path.
//!
//! `NtQuerySystemInformation(SystemExtendedHandleInformation)` lists every handle in the system. For the ones that
//! belong to the target, `DuplicateHandle` brings a copy over. Asking a copy what it is (`GetFileType`, a path) can
//! block forever on a synchronous pipe with a read pending, so every question is asked on a worker thread that is
//! given up on after a timeout; a pathological process cannot leak more than `MAX_HUNG` threads.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{GetFileType, GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS, FILE_NAME_NORMALIZED, FILE_TYPE_DISK, VOLUME_NAME_DOS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

/// Threads given up on before the rest of a process's handles are skipped unread.
const MAX_HUNG: usize = 8;
const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;

#[derive(Debug, Default)]
pub struct HandleList {
    /// Paths of the files (disk objects) the process has open, sorted, without duplicates.
    pub files: Vec<String>,
    /// Handles whose name could not be had in time, or at all.
    pub skipped: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandleError {
    /// The process cannot be opened for handle duplication (another user's, protected, or gone).
    Denied,
    /// The handle table could not be read.
    Query(i32),
}

/// `SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX` on x64 (40 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    object: usize,
    pid: usize,
    handle: usize,
    access: u32,
    backtrace: u16,
    type_index: u16,
    attributes: u32,
    reserved: u32,
}

/// Every handle value in the system's table that belongs to `pid`.
fn handle_values(pid: u32) -> Result<Vec<usize>, HandleError> {
    let buf = crate::nt::query(SYSTEM_EXTENDED_HANDLE_INFORMATION, 4 << 20).map_err(HandleError::Query)?;
    let count = buf[0] as usize;
    let entries = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(2).cast::<Entry>(), count) };
    Ok(entries.iter().filter(|e| e.pid == pid as usize).map(|e| e.handle).collect())
}

enum Msg {
    Started(usize),
    Done(usize, Found),
    Finished(usize),
}

/// What a worker found out about one handle.
enum Found {
    NotDisk,
    /// Could not be copied here (a kind of object that cannot be duplicated, or access denied): not a file we can name.
    NoCopy,
    Path(String),
    Failed,
}

/// The target process, closed when the last holder (the caller or a worker still blocked on it) lets go.
struct Target(HANDLE);

unsafe impl Send for Target {}
unsafe impl Sync for Target {}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn probe(target: &Target, value: usize) -> Found {
    let mut dup = HANDLE::default();
    let ok = unsafe { DuplicateHandle(target.0, HANDLE(value as *mut _), GetCurrentProcess(), &mut dup, 0, false, DUPLICATE_SAME_ACCESS) };
    if ok.is_err() {
        return Found::NoCopy;
    }
    let found = if unsafe { GetFileType(dup) } != FILE_TYPE_DISK {
        Found::NotDisk
    } else {
        let mut name = vec![0u16; 1024];
        let n = unsafe { GetFinalPathNameByHandleW(dup, &mut name, GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0)) } as usize;
        if n == 0 || n > name.len() {
            Found::Failed
        } else {
            let s = String::from_utf16_lossy(&name[..n]);
            Found::Path(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
        }
    };
    let _ = unsafe { CloseHandle(dup) };
    found
}

/// The files `pid` has open. `per_handle` bounds each question; see the module comment.
pub fn open_files(pid: u32, per_handle: Duration) -> Result<HandleList, HandleError> {
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }.map_err(|_| HandleError::Denied)?;
    let target = Arc::new(Target(process));
    let values = Arc::new(handle_values(pid)?);
    let next = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0)); // the worker generation allowed to take handles
    let (tx, rx) = channel::<Msg>();
    // One worker at a time asks the questions; a worker that hangs is left behind and a new one takes over, so a
    // late message from an old generation is ignored.
    let spawn = |epoch: usize| {
        let (values, next, tx, current, target) = (values.clone(), next.clone(), tx.clone(), current.clone(), target.clone());
        std::thread::spawn(move || {
            loop {
                if current.load(Ordering::SeqCst) != epoch {
                    return; // given up on while it was blocked: its handle was counted as skipped, and a newer worker has the rest
                }
                let Some(&v) = values.get(next.fetch_add(1, Ordering::SeqCst)) else {
                    let _ = tx.send(Msg::Finished(epoch));
                    return;
                };
                if tx.send(Msg::Started(epoch)).is_err() {
                    return;
                }
                if tx.send(Msg::Done(epoch, probe(&target, v))).is_err() {
                    return; // the caller is gone
                }
            }
        });
    };
    let (mut epoch, mut hung) = (0usize, 0usize);
    spawn(epoch);
    let mut list = HandleList::default();
    let mut asking = false; // the current worker has taken a handle and not answered
    loop {
        match rx.recv_timeout(per_handle) {
            Ok(Msg::Started(g)) if g == epoch => asking = true,
            Ok(Msg::Done(g, found)) if g == epoch => {
                asking = false;
                match found {
                    Found::Path(p) => list.files.push(p),
                    Found::Failed => list.skipped += 1,
                    Found::NotDisk | Found::NoCopy => {}
                }
            }
            Ok(Msg::Finished(g)) if g == epoch => break,
            Ok(_) => {} // an abandoned worker's late word
            Err(RecvTimeoutError::Timeout) => {
                if asking {
                    list.skipped += 1;
                }
                hung += 1;
                if hung >= MAX_HUNG {
                    list.skipped += values.len().saturating_sub(next.load(Ordering::SeqCst).min(values.len()));
                    break;
                }
                epoch += 1;
                current.store(epoch, Ordering::SeqCst);
                asking = false;
                spawn(epoch);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    current.store(usize::MAX, Ordering::SeqCst); // nobody takes more handles for a caller that has left
    list.files.sort_by_key(|f| f.to_lowercase());
    list.files.dedup_by_key(|f| f.to_lowercase());
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The spike: the numbers below are recorded in the views design. `cargo test -p plumb-scan handles -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_file_this_process_holds_is_listed_and_a_blocked_pipe_does_not_hang_the_call() {
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
        let path = std::env::temp_dir().join(format!("plumb-handles-{}.tmp", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let _held = std::fs::File::open(&path).unwrap();
        // A synchronous pipe server with ConnectNamedPipe pending on another thread: the classic hang for name queries.
        let name: Vec<u16> = format!(r"\\.\pipe\plumb-spike-{}", std::process::id()).encode_utf16().chain([0]).collect();
        let pipe = unsafe { CreateNamedPipeW(windows::core::PCWSTR(name.as_ptr()), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, None) };
        assert!(!pipe.is_invalid());
        let raw = pipe.0 as isize;
        std::thread::spawn(move || {
            let _ = unsafe { ConnectNamedPipe(HANDLE(raw as *mut _), None) };
        });
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        let list = open_files(std::process::id(), Duration::from_millis(500)).unwrap();
        let took = t.elapsed();
        eprintln!("HANDLES own process: {} files, {} skipped, {:?}", list.files.len(), list.skipped, took);
        let long = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
        let want = long.strip_prefix(r"\\?\").unwrap_or(&long).to_lowercase();
        assert!(list.files.iter().any(|f| f.to_lowercase() == want), "the held file is listed: {:?}", list.files);
        assert!(took < Duration::from_secs(10), "the pipe did not hang the call: {took:?}");
        if let Some(pid) = std::env::var("PLUMB_SPIKE_PID").ok().and_then(|v| v.parse().ok()) {
            let t = Instant::now();
            let r = open_files(pid, Duration::from_millis(500));
            eprintln!("HANDLES pid {pid}: {:?} in {:?}", r.as_ref().map(|l| (l.files.len(), l.skipped)), t.elapsed());
        }
        let _ = std::fs::remove_file(&path);
    }
}
