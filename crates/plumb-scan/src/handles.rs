//! The files a process has open: its handles, duplicated into this process and asked for a path.
//!
//! `NtQueryInformationProcess(ProcessHandleInformation)` lists the target's handles with their object types. For the
//! File ones, `DuplicateHandle` brings a copy over. Asking a copy what it is (`GetFileType`, a path) can
//! block forever on a synchronous pipe with a read pending, so every question is asked on a worker thread that is
//! given up on after a timeout; a pathological process cannot leak more than `MAX_HUNG` threads.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{GetFileType, GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS, FILE_NAME_NORMALIZED, FILE_TYPE_DISK, VOLUME_NAME_DOS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_INFORMATION};

/// Threads given up on before the rest of a process's handles are skipped unread.
const MAX_HUNG: usize = 8;
const PROCESS_HANDLE_INFORMATION: u32 = 51;

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
    /// The process's handle list could not be read.
    Query(i32),
}

/// `PROCESS_HANDLE_SNAPSHOT_INFORMATION` (class 51): a count and a reserved word, then one 40-byte entry per handle.
/// Each entry is five words: the handle value, its handle count, its pointer count, the granted access (low half)
/// with the object type index (high half), and the attributes with a reserved half.
const WORDS_PER_ENTRY: usize = 5;

/// The handles `process` owns, as (value, object type index), from its own snapshot; reading the system-wide table
/// instead cost about 60 ms a lookup, the snapshot of one process under 2 ms.
fn handle_snapshot(process: HANDLE) -> Result<Vec<(usize, u32)>, HandleError> {
    let buf = crate::nt::query_process(process, PROCESS_HANDLE_INFORMATION, 256 << 10).map_err(HandleError::Query)?;
    let count = (buf[0] as usize).min((buf.len() - 2) / WORDS_PER_ENTRY);
    Ok((0..count).map(|i| (buf[2 + i * WORDS_PER_ENTRY] as usize, (buf[2 + i * WORDS_PER_ENTRY + 3] >> 32) as u32)).collect())
}

/// The object type index of a File on this boot, learned from a file this process opens; `None` when it could not be
/// learned, and then every handle is asked about.
fn file_type_index() -> Option<u32> {
    static INDEX: OnceLock<Option<u32>> = OnceLock::new();
    *INDEX.get_or_init(|| {
        let file = std::fs::File::open(std::env::current_exe().ok()?).ok()?;
        let me = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, std::process::id()) }.ok()?;
        let found = handle_snapshot(me).ok()?.into_iter().find(|&(v, _)| v == file.as_raw_handle() as usize).map(|(_, t)| t);
        let _ = unsafe { CloseHandle(me) };
        found
    })
}

/// Every handle value of `process` that may be a file: those of the File object type, or all of them when the type is unknown.
fn handle_values(process: HANDLE) -> Result<Vec<usize>, HandleError> {
    let file = file_type_index();
    Ok(handle_snapshot(process)?.into_iter().filter(|&(_, t)| file.is_none_or(|f| t == f)).map(|(v, _)| v).collect())
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
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE | PROCESS_QUERY_INFORMATION, false, pid) }.map_err(|_| HandleError::Denied)?;
    let target = Arc::new(Target(process));
    let values = Arc::new(handle_values(process)?);
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

    #[test]
    fn a_file_this_process_holds_is_listed_and_only_files_are_probed() {
        let path = std::env::temp_dir().join(format!("plumb-handles-fast-{}.tmp", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let _held = std::fs::File::open(&path).unwrap();
        assert!(file_type_index().is_some(), "the File object type is learned on this boot");
        let me = unsafe { OpenProcess(PROCESS_DUP_HANDLE | PROCESS_QUERY_INFORMATION, false, std::process::id()) }.unwrap();
        let (all, files) = (handle_snapshot(me).unwrap().len(), handle_values(me).unwrap().len());
        assert!(files < all, "the filter drops handles of other types: {files} of {all}");
        let _ = unsafe { CloseHandle(me) };
        let list = open_files(std::process::id(), Duration::from_millis(500)).unwrap();
        let long = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
        let want = long.strip_prefix(r"\\?\").unwrap_or(&long).to_lowercase();
        assert!(list.files.iter().any(|f| f.to_lowercase() == want), "the held file is listed: {:?}", list.files);
        let _ = std::fs::remove_file(&path);
    }

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
        let name = plumb_core::wide(&format!(r"\\.\pipe\plumb-spike-{}", std::process::id()));
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
