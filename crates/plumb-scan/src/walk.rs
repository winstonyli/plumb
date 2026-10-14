//! `WalkScanner`: lists directories with `GetFileInformationByHandleEx`; works on any volume
//! without admin rights. A hard-linked file is counted on the first link the scan reaches, which
//! varies with thread timing; the others are flagged `HARDLINK` and count 0, as in the MFT scanner.

use crate::listing::Entry;
use crate::preview::PreviewMsg;
use crate::{Progress, ScanError, Scanner};
use plumb_core::{NodeFlags, NodeId, TreeBuilder};
use std::collections::HashSet;
use std::ffi::{OsString, c_void};
use std::hash::{BuildHasherDefault, Hasher};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, GetDiskFreeSpaceW, GetVolumePathNameW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_EXTD_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows::core::PCWSTR;

const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const BUF_BYTES: usize = 256 * 1024;

/// A file id is already well spread, so one multiply is all the hashing the set needs.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub struct WalkScanner {
    pub threads: usize,
}

impl Default for WalkScanner {
    /// `min(cores - 4, 8)` workers, at least 1.
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self { threads: cores.saturating_sub(4).clamp(1, 8) }
    }
}

struct Job {
    id: NodeId,
    path: Vec<u16>,
}

struct Done {
    id: NodeId,
    /// The listed directory's path, returned so children can extend it.
    path: Vec<u16>,
    /// `Err` when the directory could not be opened or listed (or the worker panicked).
    entries: Result<Vec<Entry>, ()>,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Lists one directory. `dir` is a `\\?\`-style wide path without a trailing NUL.
/// Bytes per cluster of the volume holding `path` (a wide path without NUL); 0 if unknown.
pub(crate) fn cluster_size(path: &[u16]) -> u64 {
    let mut p = path.to_vec();
    p.push(0);
    let mut volume = [0u16; 261];
    let (mut sectors, mut bytes) = (0u32, 0u32);
    unsafe {
        if GetVolumePathNameW(PCWSTR(p.as_ptr()), &mut volume).is_err() {
            return 0;
        }
        match GetDiskFreeSpaceW(PCWSTR(volume.as_ptr()), Some(&mut sectors), Some(&mut bytes), None, None) {
            Ok(()) => sectors as u64 * bytes as u64,
            Err(_) => 0,
        }
    }
}

/// `cluster` is the volume's cluster size (0 = unknown).
pub(crate) fn list_dir(dir: &[u16], buf: &mut [u64], cluster: u64) -> Result<Vec<Entry>, ()> {
    let mut p = dir.to_vec();
    p.push(0);
    let h = unsafe {
        CreateFileW(
            PCWSTR(p.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|_| ())?;
    let h = Handle(h);
    let mut out = Vec::new();
    let mut class = FileIdExtdDirectoryRestartInfo;
    loop {
        let r = unsafe {
            GetFileInformationByHandleEx(h.0, class, buf.as_mut_ptr().cast::<c_void>(), (buf.len() * 8) as u32)
        };
        class = FileIdExtdDirectoryInfo;
        if r.is_err() {
            // ERROR_NO_MORE_FILES (18) ends a complete listing; anything else is a failure.
            let code = windows::core::Error::from_thread().code().0 as u32 & 0xFFFF;
            return if code == 18 { Ok(out) } else { Err(()) };
        }
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            let e = unsafe { std::ptr::read_unaligned(base.add(off).cast::<FILE_ID_EXTD_DIR_INFO>()) };
            let name_ptr = unsafe { base.add(off + std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName)) };
            let n = e.FileNameLength as usize / 2;
            let name: Vec<u16> =
                (0..n).map(|i| unsafe { std::ptr::read_unaligned(name_ptr.cast::<u16>().add(i)) }).collect();
            let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
            if !dot {
                let is_dir = e.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                let reparse = e.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
                    && matches!(e.ReparsePointTag, IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK);
                // Directories count as 0: the MFT scanner does not count index allocation either.
                // Data stored inside the MFT record reports its length rounded to 8 bytes, which is
                // never a whole number of clusters. It uses no clusters, so count it as 0 (as the
                // MFT scanner does). Real allocations are whole clusters.
                let raw = e.AllocationSize as u64;
                let alloc = if is_dir || (cluster != 0 && !raw.is_multiple_of(cluster)) { 0 } else { raw };
                let id = e.FileId.Identifier;
                let id = u64::from_le_bytes(id[..8].try_into().unwrap()) ^ u64::from_le_bytes(id[8..].try_into().unwrap());
                out.push(Entry { name, alloc, is_dir, reparse, id });
            }
            if e.NextEntryOffset == 0 {
                break;
            }
            off += e.NextEntryOffset as usize;
        }
    }
}

fn join(dir: &[u16], name: &[u16]) -> Vec<u16> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    p.extend_from_slice(dir);
    if p.last() != Some(&(b'\\' as u16)) {
        p.push(b'\\' as u16);
    }
    p.extend_from_slice(name);
    p
}

impl Scanner for WalkScanner {
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError> {
        let root = std::fs::canonicalize(root).map_err(ScanError::Root)?; // verbatim `\\?\` form
        let root_wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        let cluster = cluster_size(&root_wide);
        let shown = OsString::from_wide(&root_wide).to_string_lossy().into_owned();
        let shown = shown.strip_prefix(r"\\?\").unwrap_or(&shown).trim_end_matches('\\').to_string();

        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done_rx) = mpsc::channel::<Done>();

        let root_id = out.push_str(NodeId::NONE, &shown, 0, NodeFlags::DIR);
        progress.emit(|| {
            Some(PreviewMsg::Add { parent: NodeId::NONE, name: shown.encode_utf16().collect(), size: 0, flags: NodeFlags::DIR })
        });
        progress.dirs.fetch_add(1, Ordering::Relaxed);
        job_tx.send(Job { id: root_id, path: root_wide }).unwrap();
        let mut outstanding = 1usize;
        // Ids of files that use space, to spot hard links; only this thread touches it.
        let mut seen: HashSet<u64, BuildHasherDefault<IdHasher>> = HashSet::default();

        std::thread::scope(|s| {
            for _ in 0..self.threads.max(1) {
                let (rx, tx) = (Arc::clone(&job_rx), done_tx.clone());
                s.spawn(move || {
                    unsafe {
                        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                    let mut buf = vec![0u64; BUF_BYTES / 8];
                    // The lock is held only while waiting for a job, never while listing.
                    while let Ok(job) = rx.lock().unwrap().recv() {
                        if progress.cancelled() {
                            break;
                        }
                        let entries = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            list_dir(&job.path, &mut buf, cluster)
                        }))
                        .unwrap_or(Err(()));
                        if tx.send(Done { id: job.id, path: job.path, entries }).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(done_tx);

            // This thread owns the builder: it assigns ids and queues subdirectories.
            while outstanding > 0 {
                if progress.cancelled() {
                    break;
                }
                let Ok(done) = done_rx.recv() else { break };
                outstanding -= 1;
                let Ok(entries) = done.entries else {
                    out.add_flags(done.id, NodeFlags::DENIED);
                    progress.emit(|| Some(PreviewMsg::Denied(done.id)));
                    continue;
                };
                for e in entries {
                    let mut flags = NodeFlags::empty();
                    if e.is_dir {
                        flags |= NodeFlags::DIR;
                    }
                    if e.reparse {
                        flags |= NodeFlags::REPARSE;
                    }
                    // A file seen before under another name is a hard link: it uses no further space.
                    let mut size = e.alloc;
                    if !e.is_dir && size > 0 && !seen.insert(e.id) {
                        flags |= NodeFlags::HARDLINK;
                        size = 0;
                    }
                    let id = out.push(done.id, &e.name, size, flags);
                    progress.emit(|| Some(PreviewMsg::Add { parent: done.id, name: e.name.to_vec(), size, flags }));
                    if e.is_dir {
                        progress.dirs.fetch_add(1, Ordering::Relaxed);
                    } else {
                        progress.files.fetch_add(1, Ordering::Relaxed);
                        progress.bytes.fetch_add(size, Ordering::Relaxed);
                    }
                    if e.is_dir && !e.reparse {
                        job_tx.send(Job { id, path: join(&done.path, &e.name) }).unwrap();
                        outstanding += 1;
                    }
                }
            }
            drop(job_tx); // workers see a closed queue and exit
        });
        if progress.cancelled() { Err(ScanError::Cancelled) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests;
