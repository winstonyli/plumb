//! Streams `$MFT` from a volume and feeds [`MftMerger`]. The volume is behind [`VolumeRead`], so
//! the whole pipeline is testable with an in-memory image; the Windows volume handle is in `volume`.

use crate::Progress;
use crate::merge::{MergeStats, MftMerger};
use plumb_core::TreeBuilder;
use plumb_core::mft::{ParseError, ParsedRecord, mft_extents, parse_record};
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

const RECORD: usize = 1024;

pub trait VolumeRead: Sync {
    /// Fills `buf` from byte `offset`. Offset and length are multiples of the sector size.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// The numbers from `FSCTL_GET_NTFS_VOLUME_DATA` that the scan needs.
#[derive(Clone, Copy, Debug)]
pub struct MftLayout {
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_record: u32,
    pub mft_start_lcn: u64,
    pub mft_valid_len: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct MftOptions {
    pub threads: usize,
    /// Read size. Rounded down to a whole number of clusters.
    pub chunk_bytes: usize,
}

impl Default for MftOptions {
    /// `min(cores - 4, 8)` parser threads (at least 1) and 4 MiB reads.
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self { threads: cores.saturating_sub(4).clamp(1, 8), chunk_bytes: 4 << 20 }
    }
}

#[derive(Debug)]
pub enum MftError {
    Read(io::Error),
    /// The volume is not laid out the way the scanner assumes (record size, cluster size, MFT extents).
    Layout(String),
    /// Record 0 (`$MFT` itself) could not be parsed.
    Record0(ParseError),
}

impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MftError::Read(e) => write!(f, "reading the volume failed: {e}"),
            MftError::Layout(s) => write!(f, "unsupported volume layout: {s}"),
            MftError::Record0(e) => write!(f, "cannot parse $MFT record 0: {e:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MftTimings {
    /// Reading and parsing the MFT (these overlap).
    pub stream: Duration,
    /// The merge pass: linking parents and building the tree.
    pub link: Duration,
}

struct Chunk {
    first_record: u64,
    data: Vec<u8>,
}

/// Scans the MFT of the volume behind `vol` and appends the tree to `out`. The root node is named
/// `root_name`. `out` is untouched if this returns an error.
pub fn scan_mft<V: VolumeRead>(
    vol: &V,
    layout: &MftLayout,
    root_name: &str,
    opts: &MftOptions,
    out: &mut TreeBuilder,
    progress: &Progress,
) -> Result<(MergeStats, MftTimings), MftError> {
    let t0 = Instant::now();
    let (sector, bpc) = (layout.bytes_per_sector as u64, layout.bytes_per_cluster as u64);
    if layout.bytes_per_record as usize != RECORD {
        return Err(MftError::Layout(format!("file records are {} bytes, not 1024", layout.bytes_per_record)));
    }
    if sector == 0 || bpc % sector != 0 || bpc % RECORD as u64 != 0 {
        return Err(MftError::Layout(format!("{sector}-byte sectors, {bpc}-byte clusters")));
    }

    // Record 0 describes where the rest of the MFT is.
    let first_read = (RECORD as u64).div_ceil(sector) * sector;
    let mut buf = vec![0u8; first_read as usize];
    vol.read_at(layout.mft_start_lcn * bpc, &mut buf).map_err(MftError::Read)?;
    let extents = mft_extents(&mut buf[..RECORD]).map_err(MftError::Record0)?;
    let covered: u64 = extents.iter().map(|e| e.clusters * bpc).sum();
    if covered < layout.mft_valid_len {
        return Err(MftError::Layout(format!(
            "$MFT extents cover {covered} bytes but {} are valid (attribute list?)",
            layout.mft_valid_len
        )));
    }
    let record_count = layout.mft_valid_len / RECORD as u64;

    // The reads: every extent cut into chunks of whole clusters.
    let chunk = (opts.chunk_bytes as u64 / bpc).max(1) * bpc;
    let mut plan: Vec<(u64, u64, u64)> = Vec::new(); // (disk offset, length, first record)
    let mut mft_pos = 0u64;
    'extents: for e in &extents {
        let (mut off, end) = (e.lcn * bpc, (e.lcn + e.clusters) * bpc);
        while off < end {
            if mft_pos >= layout.mft_valid_len {
                break 'extents;
            }
            let len = chunk.min(end - off);
            plan.push((off, len, mft_pos / RECORD as u64));
            off += len;
            mft_pos += len;
        }
    }

    let merger = Mutex::new(MftMerger::new(record_count as usize));
    let read_error: Mutex<Option<io::Error>> = Mutex::new(None);
    let threads = opts.threads.max(1);
    let (tx, rx) = mpsc::sync_channel::<Chunk>(threads * 2);
    let rx = Mutex::new(rx);

    std::thread::scope(|s| {
        s.spawn(|| {
            for &(off, len, first_record) in &plan {
                let mut data = vec![0u8; len as usize];
                if let Err(e) = vol.read_at(off, &mut data) {
                    *read_error.lock().unwrap() = Some(e);
                    break;
                }
                if tx.send(Chunk { first_record, data }).is_err() {
                    break;
                }
            }
            drop(tx); // parsers see a closed channel and finish
        });
        for _ in 0..threads {
            s.spawn(|| parse_chunks(&rx, &merger, record_count, progress));
        }
    });

    if let Some(e) = read_error.into_inner().unwrap() {
        return Err(MftError::Read(e));
    }
    let t1 = Instant::now();
    let stats = merger.into_inner().unwrap().finish(root_name, out);
    Ok((stats, MftTimings { stream: t1 - t0, link: t1.elapsed() }))
}

/// A parser thread: takes chunks until the reader is done, parses every record outside the lock,
/// then hands the batch to the merger.
fn parse_chunks(rx: &Mutex<mpsc::Receiver<Chunk>>, merger: &Mutex<MftMerger>, record_count: u64, progress: &Progress) {
    let mut pool: Vec<ParsedRecord> = Vec::new();
    let mut numbers: Vec<u64> = Vec::new();
    // The lock is held only while waiting for a chunk, never while parsing.
    while let Ok(mut chunk) = rx.lock().unwrap().recv() {
        let n = chunk.data.len() / RECORD;
        if pool.len() < n {
            pool.resize_with(n, ParsedRecord::default);
        }
        numbers.clear();
        let (mut corrupt, mut files, mut dirs) = (0u64, 0u64, 0u64);
        for (i, rec) in chunk.data.chunks_exact_mut(RECORD).enumerate() {
            let number = chunk.first_record + i as u64;
            if number >= record_count || rec[..4] == [0; 4] {
                continue; // beyond the valid length, or an unallocated slot
            }
            let slot = &mut pool[numbers.len()];
            match parse_record(rec, slot) {
                Ok(()) => {
                    if slot.in_use && slot.base.is_none() {
                        if slot.is_dir { dirs += 1 } else { files += 1 }
                    }
                    numbers.push(number);
                }
                Err(_) => corrupt += 1,
            }
        }
        {
            let mut m = merger.lock().unwrap();
            for (k, &number) in numbers.iter().enumerate() {
                m.add(number, &pool[k]);
            }
            for _ in 0..corrupt {
                m.note_corrupt();
            }
        }
        progress.dirs.fetch_add(dirs, Ordering::Relaxed);
        progress.files.fetch_add(files, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
