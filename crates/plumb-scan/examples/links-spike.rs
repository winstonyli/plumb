//! `cargo run --release -p plumb-scan --example links-spike -- <path> [sample files]`: what would it
//! cost the walker to see hard links (file ids in the listing) and alternate data streams (one
//! `FindFirstStreamW` per file), and how much would either change the totals?
use std::collections::HashSet;
use std::ffi::c_void;
use std::hash::{BuildHasherDefault, Hasher};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO,
    FILE_ID_EXTD_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileFullDirectoryInfo,
    FileFullDirectoryRestartInfo, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FindClose, FindFirstStreamW,
    FindNextStreamW, FindStreamInfoStandard, GetFileInformationByHandleEx, OPEN_EXISTING, WIN32_FIND_STREAM_DATA,
};
use windows::core::PCWSTR;

/// A file id is already well spread; one multiply is all the hashing the set needs.
#[derive(Default)]
struct IdHasher(u64);
impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_i64(&mut self, i: i64) {
        self.0 = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

struct Seen {
    name: Vec<u16>,
    attr: u32,
    alloc: u64,
    id: i64,
}

/// One directory's entries, with file ids when `ids` is set.
fn list(dir: &[u16], ids: bool, buf: &mut [u64], out: &mut Vec<Seen>) -> bool {
    let mut p = dir.to_vec();
    p.push(0);
    let Ok(h) = (unsafe {
        CreateFileW(PCWSTR(p.as_ptr()), FILE_LIST_DIRECTORY.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None)
    }) else {
        return false;
    };
    let mut first = true;
    loop {
        let class = match (ids, first) {
            (true, true) => FileIdExtdDirectoryRestartInfo,
            (true, false) => FileIdExtdDirectoryInfo,
            (false, true) => FileFullDirectoryRestartInfo,
            (false, false) => FileFullDirectoryInfo,
        };
        first = false;
        if unsafe { GetFileInformationByHandleEx(h, class, buf.as_mut_ptr().cast::<c_void>(), (buf.len() * 8) as u32) }.is_err() {
            break;
        }
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            let at = unsafe { base.add(off) };
            let (next, attr, alloc, id, len, name_off) = unsafe {
                if ids {
                    let e = std::ptr::read_unaligned(at.cast::<FILE_ID_EXTD_DIR_INFO>());
                    (e.NextEntryOffset, e.FileAttributes, e.AllocationSize as u64, i64::from_le_bytes(e.FileId.Identifier[..8].try_into().unwrap()), e.FileNameLength, std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName))
                } else {
                    let e = std::ptr::read_unaligned(at.cast::<FILE_FULL_DIR_INFO>());
                    (e.NextEntryOffset, e.FileAttributes, e.AllocationSize as u64, 0, e.FileNameLength, std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName))
                }
            };
            let name: Vec<u16> = (0..len as usize / 2).map(|i| unsafe { std::ptr::read_unaligned(at.add(name_off).cast::<u16>().add(i)) }).collect();
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                out.push(Seen { name, attr, alloc, id });
            }
            if next == 0 {
                break;
            }
            off += next as usize;
        }
    }
    unsafe {
        let _ = CloseHandle(h);
    }
    true
}

struct Walked {
    entries: u64,
    files: Vec<(Vec<u16>, u64, i64)>,
    secs: f64,
}

/// Walks `root` single-threaded, recording every plain file (path, alloc, id).
fn walk(root: &[u16], ids: bool) -> Walked {
    let t = Instant::now();
    let (mut buf, mut stack, mut entries, mut files) = (vec![0u64; 256 * 1024 / 8], vec![root.to_vec()], 0u64, Vec::new());
    let mut batch = Vec::new();
    while let Some(dir) = stack.pop() {
        batch.clear();
        list(&dir, ids, &mut buf, &mut batch);
        for e in batch.drain(..) {
            entries += 1;
            let mut path = dir.clone();
            path.push(b'\\' as u16);
            path.extend_from_slice(&e.name);
            if e.attr & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                continue;
            }
            if e.attr & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                stack.push(path);
            } else {
                files.push((path, e.alloc, e.id));
            }
        }
    }
    Walked { entries, files, secs: t.elapsed().as_secs_f64() }
}

fn streams(path: &[u16]) -> Vec<i64> {
    let mut p = path.to_vec();
    p.push(0);
    let mut d = WIN32_FIND_STREAM_DATA::default();
    let mut sizes = Vec::new();
    let Ok(h) = (unsafe { FindFirstStreamW(PCWSTR(p.as_ptr()), FindStreamInfoStandard, (&mut d as *mut WIN32_FIND_STREAM_DATA).cast(), None) }) else {
        return sizes;
    };
    loop {
        let name = String::from_utf16_lossy(&d.cStreamName);
        if !name.starts_with("::$DATA") {
            sizes.push(d.StreamSize);
        }
        if unsafe { FindNextStreamW(h, (&mut d as *mut WIN32_FIND_STREAM_DATA).cast()) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(HANDLE(h.0));
    }
    sizes
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: links-spike <path> [sample files]");
    let sample: usize = args.next().map_or(30_000, |s| s.parse().expect("sample must be a number"));
    let root = std::fs::canonicalize(root).expect("root");
    let root: Vec<u16> = root.as_os_str().encode_wide().collect();

    let _ = walk(&root, false); // warm the cache
    let plain = walk(&root, false);
    let withid = walk(&root, true);
    println!("listing, {} entries ({} files): plain {:.2}s ({:.2} us/entry), with file ids {:.2}s ({:.2} us/entry)", plain.entries, plain.files.len(), plain.secs, plain.secs * 1e6 / plain.entries as f64, withid.secs, withid.secs * 1e6 / withid.entries as f64);

    // The hard-link bookkeeping the walker would add per file: one set insert.
    let t = Instant::now();
    let mut seen: HashSet<i64, BuildHasherDefault<IdHasher>> = HashSet::with_capacity_and_hasher(withid.files.len(), Default::default());
    let (mut dups, mut dup_bytes, mut total) = (0u64, 0u64, 0u64);
    for (_, alloc, id) in &withid.files {
        total += alloc;
        if !seen.insert(*id) {
            dups += 1;
            dup_bytes += alloc;
        }
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "hard links: {dups} extra links among {} files; they hold {:.2} GiB of the {:.2} GiB counted ({:.1}%); the set costs {:.3}s ({:.3} us/file) and about {:.0} MiB",
        withid.files.len(), dup_bytes as f64 / (1u64 << 30) as f64, total as f64 / (1u64 << 30) as f64, dup_bytes as f64 * 100.0 / total.max(1) as f64,
        secs, secs * 1e6 / withid.files.len() as f64, seen.capacity() as f64 * 9.0 / (1u64 << 20) as f64
    );

    // Alternate data streams: sample every k-th file.
    let k = (withid.files.len() / sample.max(1)).max(1);
    let picked: Vec<&Vec<u16>> = withid.files.iter().step_by(k).map(|f| &f.0).collect();
    let t = Instant::now();
    let (mut with_named, mut named_bytes, mut big_named, mut big_files) = (0u64, 0i64, 0i64, 0u64);
    for p in &picked {
        let s = streams(p);
        if !s.is_empty() {
            with_named += 1;
            named_bytes += s.iter().sum::<i64>();
            let big: i64 = s.iter().filter(|&&n| n >= 700).sum(); // smaller streams live in the MFT record and use no clusters
            big_named += big;
            big_files += (big > 0) as u64;
        }
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "streams: sampled {} of {} files (every {k}th): {:.2} us/file; {with_named} have a named stream ({named_bytes} bytes), {big_files} a non-resident one ({big_named} bytes)",
        picked.len(), withid.files.len(), secs * 1e6 / picked.len() as f64
    );
    let _ = std::ffi::OsString::from_wide(&root); // keep the import used on every path
}
