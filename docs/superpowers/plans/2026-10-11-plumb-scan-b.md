---
status: done
---

# plumb-scan (b): MFT Scanner Implementation Plan (Plan 2b of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline; the tasks depend on each other) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Task 4 needs the user to run commands in an elevated terminal; stop there and ask.**

**Goal:** Add the MFT scanner: decode where `$MFT` lives, stream it in aligned chunks through a reader thread and a parser pool into `MftMerger`, wrap it in a Windows `MftScanner`, add scanner selection with fallback, and an elevated `scan-compare` check against the walker.

**Architecture:** The pipeline (`mft_scan::scan_mft`) is pure Rust over a `VolumeRead` trait, so it is tested with an in-memory volume image (fragmented MFT, corrupt records, read failures) without elevation. `volume.rs` is the thin Windows layer (elevation check, file-system check, raw volume handle, `MftScanner`). `select::scan_auto` chooses MFT or walker and falls back with a note. Only the raw-volume glue needs an administrator to exercise; Task 4 does that.

**Tech Stack:** Rust 2024, `windows` 0.62.2 (adds features `Win32_System_IO`, `Win32_System_Ioctl`), `plumb-core`, `plumb-scan` (Plan 2a).

**Spec:** `docs/superpowers/specs/2026-10-11-plumb-disk-design.md` (section "Scanners"). Plans 1 and 2a are done and merged.

## Global Constraints

- Volume reads must be aligned to the sector size in offset and length. Reads are whole clusters, 4 MiB by default.
- Only 1024-byte file records on volumes whose cluster size is a multiple of 1024 are supported. Anything else is a `Layout` error, and `scan_auto` falls back to the walker.
- The scanner reads `$MFT`'s own record 0 at `MftStartLcn` and decodes its data runs. It does not use `FSCTL_GET_RETRIEVAL_POINTERS`. If the runs do not cover `MftValidDataLength` (a fragmented MFT with an `$ATTRIBUTE_LIST`), that is a `Layout` error.
- An error leaves the `TreeBuilder` untouched, so the caller can fall back.
- Worker count `min(cores - 4, 8)`, at least 1. Corrupt records are skipped and counted, never fatal.
- Tests that do not need elevation must not require it. The elevated check is `scan-compare` (Task 4).
- Cap build parallelism (`-j 4`). Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Not in this plan

Live updates (USN journal, milestone M2), the UI (Plan 3), the multi-record `$ATTRIBUTE_LIST` MFT.

## File Structure

| File | Responsibility |
|---|---|
| `crates/plumb-core/src/mft.rs` | `apply_fixups`, `Extent`, `mft_extents` (decode `$MFT`'s data runs) |
| `crates/plumb-core/src/synth.rs` | `RecordBuilder::mft_data` and run encoding for tests |
| `crates/plumb-scan/src/lib.rs` | Module list; `ScanError::Mft` |
| `crates/plumb-scan/src/mft_scan.rs` | `VolumeRead`, `MftLayout`, `MftOptions`, `MftError`, `scan_mft` |
| `crates/plumb-scan/src/mft_scan/tests.rs` | Pipeline tests on an in-memory volume image |
| `crates/plumb-scan/src/volume.rs` | `is_elevated`, `is_ntfs`, `drive_letter`, `Volume`, `MftScanner` |
| `crates/plumb-scan/src/select.rs` | `scan_auto`, `ScanReport`, `Used` |
| `crates/plumb-scan/examples/scan-compare.rs` | Elevated comparison of both scanners on a fixture |

---

### Task 1: Decode `$MFT`'s data runs (`plumb-core`)

**Files:**
- Modify: `crates/plumb-core/src/mft.rs`, `crates/plumb-core/src/synth.rs`

**Interfaces:**
- Produces:
  - `mft::apply_fixups(buf: &mut [u8]) -> Result<(), ParseError>` (the magic check and update-sequence fixups, now shared with `parse_record`).
  - `mft::Extent { lcn: u64, clusters: u64 }` and `mft::mft_extents(buf: &mut [u8]) -> Result<Vec<Extent>, ParseError>`: the ordered runs of record 0's unnamed non-resident `$DATA`; a missing or resident stream, a sparse run, or a negative LCN is `BadAttribute`.
  - `RecordBuilder::mft_data(self, runs: &[(u64, u64)], real: u64) -> Self` (start LCN, cluster count).

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` at the end of `crates/plumb-core/src/mft.rs` (before its final `}`):

```rust
    #[test]
    fn mft_extents_round_trip_including_backwards_runs() {
        let runs = [(1000u64, 10u64), (200, 5), (70_000, 300), (69_999, 1)];
        let mut rec = RecordBuilder::file().file_name(5, "$MFT", 1).mft_data(&runs, 0).build();
        let got = mft_extents(&mut rec).unwrap();
        let want: Vec<Extent> = runs.iter().map(|&(lcn, clusters)| Extent { lcn, clusters }).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn mft_extents_rejects_missing_resident_and_corrupt_data() {
        let mut none = RecordBuilder::file().file_name(5, "$MFT", 1).build();
        assert_eq!(mft_extents(&mut none), Err(ParseError::BadAttribute));
        let mut resident = RecordBuilder::file().data_resident(10).build();
        assert_eq!(mft_extents(&mut resident), Err(ParseError::BadAttribute));
        let mut bad = RecordBuilder::file().mft_data(&[(4, 1)], 0).build();
        bad[0] = b'X';
        assert_eq!(mft_extents(&mut bad), Err(ParseError::BadMagic));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-core --lib mft_extents`
Expected: compile errors, `cannot find function mft_extents` and `no function mft_data`.

- [ ] **Step 3: Write the implementation**

Replace `crates/plumb-core/src/mft.rs` with:

```rust
//! NTFS `$MFT` file-record parser: bytes in, plain structs out. No I/O.

const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;
const NAMESPACE_DOS: u8 = 2;
/// Attribute flags: compressed (0x0001) or sparse (0x8000).
const COMPRESSED_OR_SPARSE: u16 = 0x8001;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    BadMagic,
    BadFixup,
    BadAttribute,
}

/// One `$FILE_NAME` attribute. The name itself lives in [`ParsedRecord::name_pool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameEntry {
    /// Record number of the parent directory (the 16-bit sequence part is dropped).
    pub parent: u64,
    pub off: u32,
    pub len: u8,
    pub namespace: u8,
}

/// What the scanner needs from one file record. Reuse one value across records: `parse_record`
/// clears it, so steady-state parsing does not allocate.
#[derive(Default, Debug)]
pub struct ParsedRecord {
    pub in_use: bool,
    pub is_dir: bool,
    /// Record number of the base record if this is an extension record.
    pub base: Option<u64>,
    /// Names in the Win32, POSIX and Win32+DOS namespaces (DOS-only 8.3 names are dropped).
    pub names: Vec<NameEntry>,
    pub name_pool: Vec<u16>,
    /// Allocated bytes of the unnamed `$DATA` stream. `None`: no unnamed stream in this record.
    /// `Some(0)`: resident data, which uses no clusters.
    pub data_alloc: Option<u64>,
    pub reparse_tag: Option<u32>,
}

impl ParsedRecord {
    pub fn clear(&mut self) {
        self.in_use = false;
        self.is_dir = false;
        self.base = None;
        self.names.clear();
        self.name_pool.clear();
        self.data_alloc = None;
        self.reparse_tag = None;
    }

    pub fn name(&self, e: &NameEntry) -> &[u16] {
        &self.name_pool[e.off as usize..e.off as usize + e.len as usize]
    }

    /// Folds an extension record into its base record.
    pub fn merge_extension(&mut self, ext: &ParsedRecord) {
        let shift = self.name_pool.len() as u32;
        self.name_pool.extend_from_slice(&ext.name_pool);
        self.names.extend(ext.names.iter().map(|e| NameEntry { off: e.off + shift, ..*e }));
        if self.data_alloc.is_none() {
            self.data_alloc = ext.data_alloc;
        }
        if self.reparse_tag.is_none() {
            self.reparse_tag = ext.reparse_tag;
        }
    }
}

fn rd<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N], ParseError> {
    b.get(o..o + N).and_then(|s| s.try_into().ok()).ok_or(ParseError::BadAttribute)
}
fn u16_at(b: &[u8], o: usize) -> Result<u16, ParseError> {
    Ok(u16::from_le_bytes(rd(b, o)?))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, ParseError> {
    Ok(u32::from_le_bytes(rd(b, o)?))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, ParseError> {
    Ok(u64::from_le_bytes(rd(b, o)?))
}

/// The value bytes of a resident attribute `a`.
fn resident_value(a: &[u8]) -> Result<&[u8], ParseError> {
    let len = u32_at(a, 0x10)? as usize;
    let off = u16_at(a, 0x14)? as usize;
    a.get(off..off + len).ok_or(ParseError::BadAttribute)
}

/// Parses one file record. `buf` must be exactly one record (normally 1024 bytes). The
/// update-sequence fixups are undone in place, so a buffer can be parsed only once.
pub fn parse_record(buf: &mut [u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    out.clear();
    apply_fixups(buf)?;
    parse_attributes(buf, out)
}

/// Checks the `FILE` magic and undoes the update-sequence fixups in place. Do this once per record.
pub fn apply_fixups(buf: &mut [u8]) -> Result<(), ParseError> {
    if buf.get(0..4) != Some(b"FILE") {
        return Err(ParseError::BadMagic);
    }
    let usa_off = u16_at(buf, 4)? as usize;
    let usa_count = u16_at(buf, 6)? as usize;
    if usa_count == 0 || usa_off + 2 * usa_count > buf.len() || (usa_count - 1) * 512 > buf.len() {
        return Err(ParseError::BadFixup);
    }
    let usn = [buf[usa_off], buf[usa_off + 1]];
    for i in 1..usa_count {
        let end = i * 512 - 2;
        if buf[end..end + 2] != usn {
            return Err(ParseError::BadFixup);
        }
        buf[end] = buf[usa_off + 2 * i];
        buf[end + 1] = buf[usa_off + 2 * i + 1];
    }
    Ok(())
}

/// Reads the header and attributes of a record whose fixups are already applied.
fn parse_attributes(buf: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    let flags = u16_at(buf, 0x16)?;
    out.in_use = flags & 1 != 0;
    out.is_dir = flags & 2 != 0;
    let base = u64_at(buf, 0x20)?;
    out.base = (base != 0).then_some(base & REF_MASK);

    let used = (u32_at(buf, 0x18)? as usize).min(buf.len());
    let mut off = u16_at(buf, 0x14)? as usize;
    while off + 8 <= used {
        let kind = u32_at(buf, off)?;
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(buf, off + 4)? as usize;
        if len < 0x18 || len % 8 != 0 || off + len > used {
            return Err(ParseError::BadAttribute);
        }
        let a = &buf[off..off + len];
        match kind {
            ATTR_FILE_NAME => parse_file_name(a, out)?,
            ATTR_DATA => parse_data(a, out)?,
            ATTR_REPARSE_POINT => parse_reparse(a, out)?,
            _ => {}
        }
        off += len;
    }
    Ok(())
}

/// A contiguous run of clusters on the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub lcn: u64,
    pub clusters: u64,
}

/// The data runs of the unnamed `$DATA` stream of `$MFT` (file record 0): where the MFT lives on
/// the volume, in order. Undoes the fixups in `buf`. Sparse runs and a missing or resident stream
/// are errors. A fragmented MFT whose runs spill into an `$ATTRIBUTE_LIST` is not detected here:
/// the caller must check that the extents cover the MFT's valid length.
pub fn mft_extents(buf: &mut [u8]) -> Result<Vec<Extent>, ParseError> {
    apply_fixups(buf)?;
    let used = (u32_at(buf, 0x18)? as usize).min(buf.len());
    let mut off = u16_at(buf, 0x14)? as usize;
    while off + 8 <= used {
        let kind = u32_at(buf, off)?;
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(buf, off + 4)? as usize;
        if len < 0x18 || len % 8 != 0 || off + len > used {
            return Err(ParseError::BadAttribute);
        }
        let a = &buf[off..off + len];
        if kind == ATTR_DATA && a[9] == 0 && a[8] != 0 && u64_at(a, 0x10)? == 0 {
            return decode_runs(a.get(u16_at(a, 0x20)? as usize..).ok_or(ParseError::BadAttribute)?);
        }
        off += len;
    }
    Err(ParseError::BadAttribute)
}

/// Decodes NTFS mapping pairs: a header byte (low nibble: length bytes, high nibble: offset
/// bytes), the run length, then the signed offset from the previous run's start.
fn decode_runs(runs: &[u8]) -> Result<Vec<Extent>, ParseError> {
    let mut out = Vec::new();
    let (mut p, mut lcn) = (0usize, 0i64);
    while p < runs.len() && runs[p] != 0 {
        let (ls, os) = ((runs[p] & 0xF) as usize, (runs[p] >> 4) as usize);
        p += 1;
        // Offset size 0 is a sparse run, which `$MFT` never has.
        if ls == 0 || ls > 8 || os == 0 || os > 8 || p + ls + os > runs.len() {
            return Err(ParseError::BadAttribute);
        }
        let mut clusters = 0u64;
        for (i, b) in runs[p..p + ls].iter().enumerate() {
            clusters |= (*b as u64) << (8 * i);
        }
        let mut delta = 0i64;
        for (i, b) in runs[p + ls..p + ls + os].iter().enumerate() {
            delta |= (*b as i64) << (8 * i);
        }
        if runs[p + ls + os - 1] & 0x80 != 0 && os < 8 {
            delta |= -1i64 << (8 * os); // sign-extend
        }
        p += ls + os;
        lcn += delta;
        if lcn < 0 {
            return Err(ParseError::BadAttribute);
        }
        out.push(Extent { lcn: lcn as u64, clusters });
    }
    Ok(out)
}

fn parse_file_name(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] != 0 {
        return Ok(()); // $FILE_NAME is always resident; ignore anything else
    }
    let v = resident_value(a)?;
    if v.len() < 0x42 {
        return Err(ParseError::BadAttribute);
    }
    let namespace = v[0x41];
    if namespace == NAMESPACE_DOS {
        return Ok(());
    }
    let n = v[0x40] as usize;
    let chars = v.get(0x42..0x42 + 2 * n).ok_or(ParseError::BadAttribute)?;
    let off = out.name_pool.len() as u32;
    out.name_pool.extend(chars.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])));
    out.names.push(NameEntry { parent: u64_at(v, 0)? & REF_MASK, off, len: n as u8, namespace });
    Ok(())
}

fn parse_data(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[9] != 0 {
        return Ok(()); // named stream (alternate data stream): ignored in v1
    }
    if a[8] == 0 {
        out.data_alloc.get_or_insert(0);
        return Ok(());
    }
    if u64_at(a, 0x10)? != 0 {
        return Ok(()); // a later extent; the first extent carries the total sizes
    }
    let flags = u16_at(a, 0x0C)?;
    let compression_unit = u16_at(a, 0x22)?;
    let alloc = if flags & COMPRESSED_OR_SPARSE != 0 && compression_unit != 0 {
        u64_at(a, 0x40)? // compressed size: what is actually allocated
    } else {
        u64_at(a, 0x28)?
    };
    out.data_alloc = Some(alloc);
    Ok(())
}

fn parse_reparse(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] == 0 {
        out.reparse_tag = Some(u32_at(resident_value(a)?, 0)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::RecordBuilder;

    fn parse(mut rec: Vec<u8>) -> (Result<(), ParseError>, ParsedRecord) {
        let mut p = ParsedRecord::default();
        let r = parse_record(&mut rec, &mut p);
        (r, p)
    }

    fn names(p: &ParsedRecord) -> Vec<String> {
        p.names.iter().map(|e| String::from_utf16_lossy(p.name(e))).collect()
    }

    #[test]
    fn plain_file() {
        let (r, p) = parse(RecordBuilder::file().file_name(5, "a.txt", 1).data_nonresident(4096, 100).build());
        assert_eq!(r, Ok(()));
        assert!(p.in_use && !p.is_dir);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.names[0].parent, 5);
        assert_eq!(p.data_alloc, Some(4096));
        assert_eq!(p.base, None);
    }

    #[test]
    fn directory_flag() {
        let (_, p) = parse(RecordBuilder::dir().file_name(5, "d", 1).build());
        assert!(p.is_dir);
        assert_eq!(p.data_alloc, None);
    }

    #[test]
    fn dos_only_names_are_dropped_win32_and_posix_kept() {
        let rec = RecordBuilder::file()
            .file_name(5, "PROGRA~1", 2)
            .file_name(5, "Program Files", 1)
            .file_name(6, "both", 3)
            .file_name(7, "posix", 0)
            .build();
        let (_, p) = parse(rec);
        assert_eq!(names(&p), vec!["Program Files", "both", "posix"]);
    }

    #[test]
    fn hard_links_give_one_name_per_parent() {
        let (_, p) = parse(RecordBuilder::file().file_name(5, "x", 1).file_name(9, "y", 1).build());
        assert_eq!(p.names.iter().map(|e| e.parent).collect::<Vec<_>>(), vec![5, 9]);
    }

    #[test]
    fn fixups_restore_bytes_at_the_sector_end() {
        // A 200-char name covers byte offset 510, where the fixup value is stored.
        let long = "x".repeat(200);
        let (r, p) = parse(RecordBuilder::file().file_name(5, &long, 1).build());
        assert_eq!(r, Ok(()));
        assert_eq!(names(&p), vec![long]);
    }

    #[test]
    fn mismatched_fixup_is_rejected() {
        let mut rec = RecordBuilder::file().file_name(5, "a", 1).build();
        rec[510] ^= 1;
        assert_eq!(parse(rec).0, Err(ParseError::BadFixup));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut rec = RecordBuilder::file().build();
        rec[0] = b'X';
        assert_eq!(parse(rec).0, Err(ParseError::BadMagic));
    }

    #[test]
    fn oversized_attribute_length_is_rejected() {
        let mut rec = RecordBuilder::file().file_name(5, "a", 1).build();
        rec[0x3C..0x40].copy_from_slice(&5000u32.to_le_bytes());
        assert_eq!(parse(rec).0, Err(ParseError::BadAttribute));
    }

    #[test]
    fn resident_data_uses_no_clusters() {
        let (_, p) = parse(RecordBuilder::file().data_resident(100).build());
        assert_eq!(p.data_alloc, Some(0));
    }

    #[test]
    fn compressed_streams_use_the_compressed_size() {
        let (_, p) = parse(RecordBuilder::file().data_compressed(65536, 100_000, 4096).build());
        assert_eq!(p.data_alloc, Some(4096));
    }

    #[test]
    fn later_extents_and_named_streams_are_ignored() {
        let rec = RecordBuilder::file()
            .data_nonresident_at(16, 8192, 8192)
            .data_named_nonresident("Zone.Identifier", 4096, 30)
            .build();
        assert_eq!(parse(rec).1.data_alloc, None);
    }

    #[test]
    fn reparse_tag_is_read() {
        let (_, p) = parse(RecordBuilder::dir().reparse(0xA000_000C).build());
        assert_eq!(p.reparse_tag, Some(0xA000_000C));
    }

    #[test]
    fn extension_records_merge_into_the_base() {
        let (_, mut base) = parse(RecordBuilder::file().file_name(2, "a", 1).build());
        let ext_ref = 5 | (3u64 << 48);
        let (_, ext) = parse(
            RecordBuilder::file().extension_of(ext_ref).file_name(8, "b", 1).data_nonresident(8192, 8000).build(),
        );
        assert_eq!(ext.base, Some(5));
        base.merge_extension(&ext);
        assert_eq!(names(&base), vec!["a", "b"]);
        assert_eq!(base.names[1].parent, 8);
        assert_eq!(base.data_alloc, Some(8192));
    }

    #[test]
    fn parsing_again_into_the_same_value_starts_clean() {
        let mut p = ParsedRecord::default();
        let mut a = RecordBuilder::file().file_name(5, "first", 1).data_nonresident(4096, 1).build();
        parse_record(&mut a, &mut p).unwrap();
        let mut b = RecordBuilder::dir().file_name(6, "second", 1).build();
        parse_record(&mut b, &mut p).unwrap();
        assert_eq!(names(&p), vec!["second"]);
        assert_eq!(p.data_alloc, None);
        assert!(p.is_dir);
    }

    #[test]
    fn mft_extents_round_trip_including_backwards_runs() {
        let runs = [(1000u64, 10u64), (200, 5), (70_000, 300), (69_999, 1)];
        let mut rec = RecordBuilder::file().file_name(5, "$MFT", 1).mft_data(&runs, 0).build();
        let got = mft_extents(&mut rec).unwrap();
        let want: Vec<Extent> = runs.iter().map(|&(lcn, clusters)| Extent { lcn, clusters }).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn mft_extents_rejects_missing_resident_and_corrupt_data() {
        let mut none = RecordBuilder::file().file_name(5, "$MFT", 1).build();
        assert_eq!(mft_extents(&mut none), Err(ParseError::BadAttribute));
        let mut resident = RecordBuilder::file().data_resident(10).build();
        assert_eq!(mft_extents(&mut resident), Err(ParseError::BadAttribute));
        let mut bad = RecordBuilder::file().mft_data(&[(4, 1)], 0).build();
        bad[0] = b'X';
        assert_eq!(mft_extents(&mut bad), Err(ParseError::BadMagic));
    }
}
```

Replace `crates/plumb-core/src/synth.rs` with:

```rust
//! Synthetic inputs for tests and benchmarks: MFT records and big trees.

use crate::node::{NodeFlags, NodeId};
use crate::tree::{Tree, TreeBuilder};

pub const RECORD_SIZE: usize = 1024;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn resident_attr(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let value_off = align8(0x18 + 2 * units.len());
    let len = align8(value_off + value.len());
    let mut a = vec![0u8; len];
    put32(&mut a, 0, kind);
    put32(&mut a, 4, len as u32);
    a[9] = units.len() as u8;
    put16(&mut a, 10, 0x18);
    put32(&mut a, 0x10, value.len() as u32);
    put16(&mut a, 0x14, value_off as u16);
    for (i, u) in units.iter().enumerate() {
        put16(&mut a, 0x18 + 2 * i, *u);
    }
    a[value_off..value_off + value.len()].copy_from_slice(value);
    a
}

struct NonResident<'a> {
    name: &'a str,
    flags: u16,
    start_vcn: u64,
    compression_unit: u16,
    alloc: u64,
    real: u64,
    compressed: Option<u64>,
    /// Encoded mapping pairs (without the terminating zero byte).
    runs: Vec<u8>,
}

fn nonresident_data_attr(s: NonResident) -> Vec<u8> {
    let units: Vec<u16> = s.name.encode_utf16().collect();
    let header = if s.compressed.is_some() { 0x48 } else { 0x40 };
    let runlist_off = align8(header + 2 * units.len());
    let len = align8(runlist_off + s.runs.len() + 1);
    let mut a = vec![0u8; len];
    put32(&mut a, 0, 0x80);
    put32(&mut a, 4, len as u32);
    a[8] = 1;
    a[9] = units.len() as u8;
    put16(&mut a, 10, header as u16);
    put16(&mut a, 12, s.flags);
    put64(&mut a, 0x10, s.start_vcn);
    put16(&mut a, 0x20, runlist_off as u16);
    put16(&mut a, 0x22, s.compression_unit);
    put64(&mut a, 0x28, s.alloc);
    put64(&mut a, 0x30, s.real);
    put64(&mut a, 0x38, s.real);
    if let Some(c) = s.compressed {
        put64(&mut a, 0x40, c);
    }
    for (i, u) in units.iter().enumerate() {
        put16(&mut a, header + 2 * i, *u);
    }
    a[runlist_off..runlist_off + s.runs.len()].copy_from_slice(&s.runs);
    a
}

/// Encodes `(start lcn, cluster count)` runs as NTFS mapping pairs.
fn encode_runs(runs: &[(u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev = 0i64;
    for &(lcn, clusters) in runs {
        let delta = lcn as i64 - prev;
        prev = lcn as i64;
        let len_bytes = (8 - clusters.leading_zeros() as usize / 8).max(1);
        // Smallest signed width that holds `delta`.
        let off_bytes = (1..=8usize).find(|&n| n == 8 || (-(1i64 << (8 * n - 1))..(1i64 << (8 * n - 1))).contains(&delta)).unwrap();
        out.push(((off_bytes as u8) << 4) | len_bytes as u8);
        out.extend_from_slice(&clusters.to_le_bytes()[..len_bytes]);
        out.extend_from_slice(&delta.to_le_bytes()[..off_bytes]);
    }
    out
}

/// Builds a valid 1024-byte NTFS file record, update-sequence fixups included.
pub struct RecordBuilder {
    flags: u16,
    base: u64,
    attrs: Vec<Vec<u8>>,
}

impl RecordBuilder {
    pub fn file() -> Self {
        Self { flags: 1, base: 0, attrs: Vec::new() }
    }

    pub fn dir() -> Self {
        Self { flags: 3, base: 0, attrs: Vec::new() }
    }

    /// Marks this as an extension record of the record with file reference `base_ref`.
    pub fn extension_of(mut self, base_ref: u64) -> Self {
        self.base = base_ref;
        self
    }

    pub fn file_name(mut self, parent_record: u64, name: &str, namespace: u8) -> Self {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 0x42 + 2 * units.len()];
        put64(&mut v, 0, parent_record | (1 << 48));
        v[0x40] = units.len() as u8;
        v[0x41] = namespace;
        for (i, u) in units.iter().enumerate() {
            put16(&mut v, 0x42 + 2 * i, *u);
        }
        self.attrs.push(resident_attr(0x30, "", &v));
        self
    }

    pub fn data_resident(mut self, len: usize) -> Self {
        self.attrs.push(resident_attr(0x80, "", &vec![0u8; len]));
        self
    }

    pub fn data_nonresident(self, alloc: u64, real: u64) -> Self {
        self.data_nonresident_at(0, alloc, real)
    }

    pub fn data_nonresident_at(mut self, start_vcn: u64, alloc: u64, real: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name: "",
            flags: 0,
            start_vcn,
            compression_unit: 0,
            alloc,
            real,
            compressed: None,
            runs: Vec::new(),
        }));
        self
    }

    /// The unnamed `$DATA` of `$MFT` itself: non-resident with the given `(start lcn, clusters)` runs.
    pub fn mft_data(mut self, runs: &[(u64, u64)], real: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name: "",
            flags: 0,
            start_vcn: 0,
            compression_unit: 0,
            alloc: real,
            real,
            compressed: None,
            runs: encode_runs(runs),
        }));
        self
    }

    pub fn data_compressed(mut self, alloc: u64, real: u64, compressed: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name: "",
            flags: 0x0001,
            start_vcn: 0,
            compression_unit: 4,
            alloc,
            real,
            compressed: Some(compressed),
            runs: Vec::new(),
        }));
        self
    }

    pub fn data_named_nonresident(mut self, name: &str, alloc: u64, real: u64) -> Self {
        self.attrs.push(nonresident_data_attr(NonResident {
            name,
            flags: 0,
            start_vcn: 0,
            compression_unit: 0,
            alloc,
            real,
            compressed: None,
            runs: Vec::new(),
        }));
        self
    }

    pub fn reparse(mut self, tag: u32) -> Self {
        let mut v = vec![0u8; 8];
        put32(&mut v, 0, tag);
        self.attrs.push(resident_attr(0xC0, "", &v));
        self
    }

    pub fn build(self) -> Vec<u8> {
        let mut b = vec![0u8; RECORD_SIZE];
        b[0..4].copy_from_slice(b"FILE");
        put16(&mut b, 4, 0x30); // update sequence array offset
        put16(&mut b, 6, 3); // the update sequence number plus two sectors
        put16(&mut b, 0x10, 1);
        put16(&mut b, 0x14, 0x38); // first attribute
        put16(&mut b, 0x16, self.flags);
        put32(&mut b, 0x1C, RECORD_SIZE as u32);
        put64(&mut b, 0x20, self.base);
        let mut off = 0x38;
        for a in &self.attrs {
            b[off..off + a.len()].copy_from_slice(a);
            off += a.len();
        }
        put32(&mut b, off, 0xFFFF_FFFF);
        off += 8;
        assert!(off <= RECORD_SIZE, "attributes do not fit in one record");
        put32(&mut b, 0x18, off as u32);
        // Fixups: keep the last two bytes of each sector in the array and stamp the number there.
        let usn = [0x01u8, 0x00];
        b[0x30..0x32].copy_from_slice(&usn);
        for s in 0..2 {
            let end = (s + 1) * 512 - 2;
            let original = [b[end], b[end + 1]];
            b[0x32 + 2 * s..0x34 + 2 * s].copy_from_slice(&original);
            b[end..end + 2].copy_from_slice(&usn);
        }
        b
    }
}

/// The name `n<decimal>` as UTF-16 without allocating.
pub fn name_units(i: u32, buf: &mut [u16; 12]) -> &[u16] {
    let mut digits = [0u16; 10];
    let (mut n, mut k) = (i, 0);
    loop {
        digits[k] = b'0' as u16 + (n % 10) as u16;
        n /= 10;
        k += 1;
        if n == 0 {
            break;
        }
    }
    buf[0] = b'n' as u16;
    for j in 0..k {
        buf[1 + j] = digits[k - 1 - j];
    }
    &buf[..1 + k]
}

fn mix(i: u32) -> u64 {
    let mut x = i as u64 + 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A complete `fanout`-ary tree of `n` nodes: node `i`'s parent is `(i - 1) / fanout`.
/// Nodes with children are directories; the rest are files of pseudo-random size.
pub fn kary_builder(n: u32, fanout: u32) -> TreeBuilder {
    let mut b = TreeBuilder::with_capacity(n as usize, n as usize * 7);
    let mut buf = [0u16; 12];
    for i in 0..n {
        let parent = if i == 0 { NodeId::NONE } else { NodeId((i - 1) / fanout) };
        let is_dir = u64::from(i) * u64::from(fanout) + 1 < u64::from(n);
        let (size, flags) =
            if is_dir { (0, NodeFlags::DIR) } else { (mix(i) % (1 << 20), NodeFlags::empty()) };
        b.push(parent, name_units(i, &mut buf), size, flags);
    }
    b
}

/// One root directory with `children` files of pseudo-random size.
pub fn flat_tree(children: u32) -> Tree {
    let mut b = TreeBuilder::with_capacity(children as usize + 1, children as usize * 7);
    let mut buf = [0u16; 12];
    let root = b.push_str(NodeId::NONE, "root", 0, NodeFlags::DIR);
    for i in 0..children {
        b.push(root, name_units(i, &mut buf), mix(i) % (1 << 20), NodeFlags::empty());
    }
    b.finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_units_encodes_decimal() {
        let mut buf = [0u16; 12];
        assert_eq!(String::from_utf16_lossy(name_units(0, &mut buf)), "n0");
        assert_eq!(String::from_utf16_lossy(name_units(4_294_967_295, &mut buf)), "n4294967295");
    }

    #[test]
    fn kary_tree_is_valid_and_counts_files() {
        let t = kary_builder(1000, 4).finish().unwrap();
        let files = (0..1000).filter(|&i| !t.flags[NodeId(i)].contains(NodeFlags::DIR)).count() as u32;
        assert_eq!(t.subtree_files[NodeId(0)], files);
        assert_eq!(t.subtree_size[NodeId(0)], t.size.iter().sum::<u64>());
    }

    #[test]
    fn flat_tree_has_one_root_and_all_children() {
        let t = flat_tree(500);
        assert_eq!(t.len(), 501);
        assert_eq!(t.children(NodeId(0)).count(), 500);
    }

    #[test]
    fn records_are_one_kilobyte_with_fixup_stamps() {
        let r = RecordBuilder::file().build();
        assert_eq!(r.len(), RECORD_SIZE);
        assert_eq!(&r[0..4], b"FILE");
        assert_eq!(&r[510..512], &r[0x30..0x32]);
        assert_eq!(&r[1022..1024], &r[0x30..0x32]);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-core`
Expected: `test result: ok. 40 passed` (the 38 existing plus 2 new).

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Decode \$MFT data runs; share fixup code" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The MFT streaming pipeline

**Files:**
- Create: `crates/plumb-scan/src/mft_scan.rs`, `crates/plumb-scan/src/mft_scan/tests.rs`
- Modify: `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Consumes: `MftMerger` (Plan 2a), `mft::{mft_extents, parse_record, ParsedRecord, ParseError}` (Task 1), `Progress`.
- Produces:
  - `trait VolumeRead: Sync { fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>; }`
  - `MftLayout { bytes_per_sector, bytes_per_cluster, bytes_per_record: u32, mft_start_lcn, mft_valid_len: u64 }`, `MftOptions { threads: usize, chunk_bytes: usize }` (`Default`: `min(cores - 4, 8)` threads, 4 MiB), `MftError::{Read(io::Error), Layout(String), Record0(ParseError)}` (`Display`), `MftTimings { stream, link: Duration }`.
  - `scan_mft<V: VolumeRead>(vol: &V, layout: &MftLayout, root_name: &str, opts: &MftOptions, out: &mut TreeBuilder, progress: &Progress) -> Result<(MergeStats, MftTimings), MftError>`.
  - `ScanError::Mft(String)`.
  - Behavior: reads record 0, decodes the extents, checks they cover `mft_valid_len`, cuts the extents into cluster-aligned chunks, and parses chunks on `threads` threads outside the merger lock (batches are handed to the merger under the lock). Unallocated (all-zero) slots are skipped; records that fail to parse are counted in `MergeStats::corrupt`; records at or beyond `mft_valid_len / 1024` are ignored. `Progress.dirs` and `files` count in-use base records.

- [ ] **Step 1: Write the failing tests**

`crates/plumb-scan/src/mft_scan/tests.rs`:

```rust
use super::*;
use plumb_core::synth::RecordBuilder;
use plumb_core::{NodeId, Tree};
use std::collections::BTreeSet;

const BPC: usize = 4096;
const SECTOR: usize = 512;
// The test MFT: 40 records in two fragments, 24 at cluster 3 (6 clusters) and 16 at cluster 20.
const RUNS: [(u64, u64); 2] = [(3, 6), (20, 4)];
const RECORDS: u64 = 40;

struct Image {
    data: Vec<u8>,
    /// Reads that touch this byte offset fail.
    fail_at: Option<u64>,
}

impl VolumeRead for Image {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        assert_eq!(offset % SECTOR as u64, 0, "unaligned offset");
        assert_eq!(buf.len() % SECTOR, 0, "unaligned length");
        if let Some(f) = self.fail_at {
            if (offset..offset + buf.len() as u64).contains(&f) {
                return Err(io::Error::other("injected read failure"));
            }
        }
        buf.copy_from_slice(&self.data[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }
}

fn record_offset(n: u64) -> usize {
    let (lcn, base) = if n < 24 { (RUNS[0].0, 0) } else { (RUNS[1].0, 24) };
    lcn as usize * BPC + (n - base) as usize * RECORD
}

/// $MFT (record 0), the root (5), docs/{a.txt, b.txt}, big.bin and a hard-linked file in two dirs.
fn image() -> Image {
    let mut data = vec![0u8; 64 * BPC];
    let mut put = |n: u64, r: RecordBuilder| {
        let at = record_offset(n);
        data[at..at + RECORD].copy_from_slice(&r.build());
    };
    put(0, RecordBuilder::file().file_name(5, "$MFT", 1).mft_data(&RUNS, RECORDS * RECORD as u64));
    put(5, RecordBuilder::dir().file_name(5, ".", 1));
    put(30, RecordBuilder::dir().file_name(5, "docs", 1));
    put(31, RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(8192, 5000));
    put(32, RecordBuilder::file().file_name(30, "b.txt", 1).data_resident(10));
    put(33, RecordBuilder::file().file_name(5, "big.bin", 1).data_nonresident(1 << 20, 1 << 20));
    put(34, RecordBuilder::file().file_name(30, "link", 1).file_name(5, "link2", 1).data_nonresident(4096, 1));
    Image { data, fail_at: None }
}

fn layout() -> MftLayout {
    MftLayout {
        bytes_per_sector: SECTOR as u32,
        bytes_per_cluster: BPC as u32,
        bytes_per_record: RECORD as u32,
        mft_start_lcn: RUNS[0].0,
        mft_valid_len: RECORDS * RECORD as u64,
    }
}

fn run(img: &Image, l: &MftLayout, threads: usize, chunk_bytes: usize) -> Result<(Tree, MergeStats, Progress), MftError> {
    let mut b = TreeBuilder::new();
    let p = Progress::default();
    let (stats, _) = scan_mft(img, l, "X:", &MftOptions { threads, chunk_bytes }, &mut b, &p)?;
    Ok((b.finish().unwrap(), stats, p))
}

fn sorted(t: &Tree) -> BTreeSet<(String, u64)> {
    (0..t.len() as u32).map(|i| (t.path(NodeId(i)), t.size[NodeId(i)])).collect()
}

#[test]
fn scans_a_fragmented_mft() {
    let (t, stats, p) = run(&image(), &layout(), 2, 4 << 20).unwrap();
    let want: BTreeSet<(String, u64)> = [
        ("X:", 0),
        ("X:\\$MFT", RECORDS * RECORD as u64),
        ("X:\\docs", 0),
        ("X:\\docs\\a.txt", 8192),
        ("X:\\docs\\b.txt", 0),
        ("X:\\docs\\link", 0), // the second link (by parent record number) carries no size
        ("X:\\big.bin", 1 << 20),
        ("X:\\link2", 4096),
    ]
    .into_iter()
    .map(|(p, s)| (p.to_string(), s))
    .collect();
    assert_eq!(sorted(&t), want);
    assert_eq!(stats.corrupt, 0);
    let (dirs, files, _) = p.snapshot();
    assert_eq!((dirs, files), (2, 5)); // root and docs; $MFT, a, b, big, link
}

#[test]
fn chunk_size_and_thread_count_do_not_change_the_tree() {
    let (a, _, _) = run(&image(), &layout(), 1, 4 << 20).unwrap();
    for (threads, chunk) in [(1, BPC), (3, BPC), (4, 2 * BPC), (8, 4 << 20)] {
        let (b, _, _) = run(&image(), &layout(), threads, chunk).unwrap();
        assert_eq!(sorted(&a), sorted(&b), "threads {threads}, chunk {chunk}");
    }
}

#[test]
fn corrupt_records_are_counted_and_skipped() {
    let mut img = image();
    let at = record_offset(31);
    img.data[at + 510] ^= 0xFF; // breaks the fixup stamp of a.txt
    let (t, stats, _) = run(&img, &layout(), 2, BPC).unwrap();
    assert_eq!(stats.corrupt, 1);
    assert!(sorted(&t).iter().all(|(p, _)| !p.ends_with("a.txt")));
}

#[test]
fn a_failed_read_is_an_error_and_leaves_the_builder_empty() {
    let mut img = image();
    img.fail_at = Some(record_offset(35) as u64);
    let mut b = TreeBuilder::new();
    let r = scan_mft(&img, &layout(), "X:", &MftOptions { threads: 2, chunk_bytes: BPC }, &mut b, &Progress::default());
    assert!(matches!(r, Err(MftError::Read(_))));
    assert!(b.is_empty());
}

#[test]
fn unsupported_layouts_are_rejected() {
    let img = image();
    let mut l = layout();
    l.bytes_per_record = 4096;
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
    let mut l = layout();
    l.bytes_per_cluster = 512;
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
    let mut l = layout();
    l.mft_valid_len = 100 * RECORD as u64; // more than the extents cover
    assert!(matches!(run(&img, &l, 1, BPC), Err(MftError::Layout(_))));
}

#[test]
fn a_bad_record_zero_is_an_error() {
    let mut img = image();
    img.data[record_offset(0)] = b'X';
    assert!(matches!(run(&img, &layout(), 1, BPC), Err(MftError::Record0(_))));
}
```

In `crates/plumb-scan/src/lib.rs`, add `pub mod mft_scan;` after `pub mod merge;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-scan --lib mft_scan`
Expected: compile error `E0583: file not found for module mft_scan`.

- [ ] **Step 3: Write the implementation**

`crates/plumb-scan/src/mft_scan.rs`:

```rust
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
```

In `crates/plumb-scan/src/lib.rs`, add the variant to `ScanError` so it reads:

```rust
#[derive(Debug)]
pub enum ScanError {
    /// The root path could not be resolved.
    Root(std::io::Error),
    /// The MFT scanner could not run or failed; the message says why. Nothing was added to the tree.
    Mft(String),
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-scan`
Expected: `test result: ok. 22 passed` (10 merge, 6 walk, 6 mft_scan).

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Add MFT streaming pipeline over a VolumeRead trait" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Windows volume access, `MftScanner` and scanner selection

**Files:**
- Create: `crates/plumb-scan/src/volume.rs`, `crates/plumb-scan/src/select.rs`
- Modify: `crates/plumb-scan/Cargo.toml`, `crates/plumb-scan/src/lib.rs`

**Interfaces:**
- Consumes: Task 2's `scan_mft`, `MftLayout`, `MftOptions`, `VolumeRead`; `WalkScanner` (Plan 2a).
- Produces:
  - `volume::{is_elevated() -> bool, is_ntfs(root: &Path) -> bool, drive_letter(root: &Path) -> Option<char>}` (`drive_letter` accepts exactly `C:` or `C:\`, case-insensitive, and returns the uppercase letter).
  - `volume::Volume::{open(letter: char) -> io::Result<Volume>, layout(&self) -> io::Result<MftLayout>}` implementing `VolumeRead` with positional `ReadFile` calls.
  - `volume::MftScanner { pub options: MftOptions }` with `scan_with_stats(&self, root, out, progress) -> Result<(MergeStats, MftTimings), ScanError>` and `impl Scanner`. Errors are `ScanError::Mft`; the builder is untouched on error.
  - `select::{Used::{Mft, Walk}, ScanReport { used, note: Option<String>, mft: Option<(MergeStats, MftTimings)> }, scan_auto(root, out, progress) -> Result<ScanReport, ScanError>}`: MFT when `root` is a drive root on NTFS and the process is elevated; otherwise the walker. If the MFT scan fails it resets the progress counters and uses the walker with a note saying why. On NTFS without elevation the note says running as administrator is faster; there is no note for other volumes or plain directories.

- [ ] **Step 1: Add the dependency features and module declarations**

In `crates/plumb-scan/Cargo.toml`, the `windows` line becomes:

```toml
windows = { version = "0.62", features = ["Win32_Foundation", "Win32_Security", "Win32_Storage_FileSystem", "Win32_System_IO", "Win32_System_Ioctl", "Win32_System_Threading"] }
```

`crates/plumb-scan/src/lib.rs` should now start with these module lines (after the `//!` comment):

```rust
pub mod merge;
pub mod mft_scan;
#[cfg(windows)]
pub mod select;
#[cfg(windows)]
pub mod volume;
#[cfg(windows)]
pub mod walk;
```

- [ ] **Step 2: Add `volume.rs`**

`crates/plumb-scan/src/volume.rs`:

```rust
//! The Windows side of the MFT scanner: elevation check, file-system check, the raw volume handle
//! and `MftScanner`. Opening a volume needs administrator rights.

use crate::mft_scan::{MftLayout, MftOptions, MftTimings, VolumeRead, scan_mft};
use crate::merge::MergeStats;
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
    OPEN_EXISTING, ReadFile,
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

/// The drive letter if `root` is exactly a drive root (`C:` or `C:\`).
pub fn drive_letter(root: &Path) -> Option<char> {
    let s = root.to_str()?;
    let b = s.as_bytes();
    let ok = (b.len() == 2 || (b.len() == 3 && (b[2] == b'\\' || b[2] == b'/'))) && b[1] == b':' && b[0].is_ascii_alphabetic();
    ok.then(|| (b[0] as char).to_ascii_uppercase())
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

/// An open raw volume (`\\.\C:`), read-only.
pub struct Volume(HANDLE);

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
    pub fn open(letter: char) -> io::Result<Volume> {
        let path: Vec<u16> = format!("\\\\.\\{letter}:").encode_utf16().chain(Some(0)).collect();
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
    ) -> Result<(MergeStats, MftTimings), ScanError> {
        let letter = drive_letter(root).ok_or_else(|| ScanError::Mft("not a drive root".into()))?;
        let vol = Volume::open(letter).map_err(|e| ScanError::Mft(format!("cannot open volume {letter}: {e}")))?;
        let layout = vol.layout().map_err(|e| ScanError::Mft(format!("cannot query volume {letter}: {e}")))?;
        scan_mft(&vol, &layout, &format!("{letter}:"), &self.options, out, progress)
            .map_err(|e| ScanError::Mft(e.to_string()))
    }
}

impl Scanner for MftScanner {
    fn scan(&self, root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<(), ScanError> {
        self.scan_with_stats(root, out, progress).map(|_| ())
    }
}
```

- [ ] **Step 3: Write the failing tests**

`crates/plumb-scan/src/select.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_roots_are_recognised() {
        for (s, want) in [("C:", Some('C')), ("c:\\", Some('C')), ("C:/", Some('C')), ("C:\\Users", None), ("\\\\srv\\share", None), ("1:\\", None), ("", None)] {
            assert_eq!(drive_letter(Path::new(s)), want, "{s}");
        }
    }

    #[test]
    fn a_directory_is_walked_without_a_note() {
        let dir = std::env::temp_dir().join(format!("plumb-select-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        let mut b = TreeBuilder::new();
        let r = scan_auto(&dir, &mut b, &Progress::default()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((r.used, r.note, r.mft.is_none()), (Used::Walk, None, true));
        assert_eq!(b.len(), 2); // the directory and its file
    }

    #[test]
    fn without_elevation_the_mft_scanner_refuses_and_leaves_the_builder_empty() {
        if is_elevated() {
            return; // covered by the elevated check in the plan
        }
        let mut b = TreeBuilder::new();
        let tmp = std::env::temp_dir();
        let root = tmp.ancestors().last().unwrap().to_path_buf(); // e.g. C:\
        if !is_ntfs(&root) {
            return;
        }
        assert!(!is_elevated());
        assert!(MftScanner::default().scan_with_stats(&root, &mut b, &Progress::default()).is_err());
        assert!(b.is_empty());
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -j 4 -p plumb-scan --lib select`
Expected: compile errors, `cannot find function scan_auto` / `type Used`.

- [ ] **Step 5: Write the implementation**

Insert at the top of `crates/plumb-scan/src/select.rs`, above the `#[cfg(test)]` module:

```rust
//! Picks the scanner for a volume: the MFT reader when the volume is NTFS and the process is
//! elevated, the directory walker otherwise, and the walker again if the MFT scan fails.

use crate::merge::MergeStats;
use crate::mft_scan::MftTimings;
use crate::volume::{MftScanner, drive_letter, is_elevated, is_ntfs};
use crate::walk::WalkScanner;
use crate::{Progress, ScanError, Scanner};
use plumb_core::TreeBuilder;
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Used {
    Mft,
    Walk,
}

#[derive(Debug)]
pub struct ScanReport {
    pub used: Used,
    /// Something the user should be told (a fallback, or that elevation would be faster).
    pub note: Option<String>,
    /// Present when the MFT scanner ran.
    pub mft: Option<(MergeStats, MftTimings)>,
}

/// Scans `root` with the best available scanner.
pub fn scan_auto(root: &Path, out: &mut TreeBuilder, progress: &Progress) -> Result<ScanReport, ScanError> {
    let mut note = None;
    if drive_letter(root).is_some() && is_ntfs(root) {
        if is_elevated() {
            match MftScanner::default().scan_with_stats(root, out, progress) {
                Ok(mft) => return Ok(ScanReport { used: Used::Mft, note: None, mft: Some(mft) }),
                Err(e) => {
                    // The failed attempt added nothing to `out`; undo its progress counts too.
                    for c in [&progress.dirs, &progress.files, &progress.bytes] {
                        c.store(0, Ordering::Relaxed);
                    }
                    let ScanError::Mft(msg) = &e else { return Err(e) };
                    note = Some(format!("MFT scan failed ({msg}); used the directory walker instead"));
                }
            }
        } else {
            note = Some("scanned with the directory walker; running as administrator is faster".into());
        }
    }
    WalkScanner::default().scan(root, out, progress)?;
    Ok(ScanReport { used: Used::Walk, note, mft: None })
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -j 4 -p plumb-scan`
Expected: `test result: ok. 25 passed`. (The not-elevated test asserts that opening the volume fails cleanly; when the tests run elevated it returns early.)

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates
git commit -m "Add Windows volume access, MftScanner and scan_auto with fallback" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Elevated verification with `scan-compare` (needs the user)

**Files:**
- Create: `crates/plumb-scan/examples/scan-compare.rs`
- Modify: `docs/benchmarks.md`

- [ ] **Step 1: Add the example**

`crates/plumb-scan/examples/scan-compare.rs`:

```rust
//! Elevated check that the MFT scanner and the directory walker agree.
//!
//! `cargo run --release -p plumb-scan --example scan-compare -- C:\plumb-fixture`
//!
//! Run from an elevated terminal on an NTFS drive. The fixture directory must not exist; the
//! example creates it (plain, empty, tiny, resident-boundary, large, Unicode, long-name,
//! alternate-stream, compressed, sparse and junction entries), scans it with the walker, scans
//! the whole volume with the MFT scanner, compares the fixture's subtree in both, and deletes it.
//! Hard links are deliberately absent: the walker cannot see them.

use plumb_core::{NodeFlags, NodeId, Tree, TreeBuilder};
use plumb_scan::volume::{MftScanner, drive_letter, is_elevated};
use plumb_scan::walk::WalkScanner;
use plumb_scan::{Progress, Scanner};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn run(cmd: &str, args: &[&str]) {
    let out = Command::new(cmd).args(args).output().expect("spawn");
    assert!(out.status.success(), "{cmd} {args:?} failed: {}", String::from_utf8_lossy(&out.stdout));
}

fn make_fixture(root: &Path) {
    let p = |s: &str| root.join(s);
    for d in ["empty", "a\\b\\c", "unicode-\u{e9}\u{65e5}\u{672c}"] {
        fs::create_dir_all(p(d)).unwrap();
    }
    for (name, len) in [("a\\zero.bin", 0usize), ("a\\one.bin", 1), ("a\\r700.bin", 700), ("a\\b\\five.dat", 5000), ("a\\b\\c\\big.bin", 100_000), ("big3m.bin", 3_000_000)] {
        // pseudo-random-ish content so the file is not compressible by accident
        let data: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(2654435761) >> 7) as u8).collect();
        fs::write(p(name), data).unwrap();
    }
    fs::write(p(&format!("unicode-\u{e9}\u{65e5}\u{672c}\\{}.txt", "n".repeat(200))), b"long name").unwrap();
    fs::write(p("a\\stream.txt"), b"main").unwrap();
    fs::write(p("a\\stream.txt:ads"), vec![9u8; 50_000]).unwrap(); // alternate stream: ignored by both
    // Compressed: 1 MiB of zeros compresses to almost nothing.
    fs::write(p("compressed.bin"), vec![0u8; 1 << 20]).unwrap();
    run("compact", &["/c", &p("compressed.bin").to_string_lossy()]);
    // Sparse: 1 MiB file whose data is all deallocated.
    fs::write(p("sparse.bin"), vec![1u8; 1 << 20]).unwrap();
    run("fsutil", &["sparse", "setflag", &p("sparse.bin").to_string_lossy()]);
    run("fsutil", &["file", "setzerodata", "offset=0", &format!("length={}", 1 << 20), &p("sparse.bin").to_string_lossy()]);
    run("cmd", &["/C", "mklink", "/J", &p("junc").to_string_lossy(), &p("a").to_string_lossy()]);
}

fn remove_fixture(root: &Path) {
    let _ = Command::new("cmd").args(["/C", "rmdir"]).arg(root.join("junc")).output();
    let _ = fs::remove_dir_all(root);
}

type Entries = BTreeMap<String, (u64, bool, bool)>; // relative path (lowercase) -> (size, dir, reparse)

fn collect(t: &Tree, node: NodeId, prefix: &str, out: &mut Entries) {
    for c in t.children(node) {
        let path = if prefix.is_empty() { t.name_string(c) } else { format!("{prefix}\\{}", t.name_string(c)) };
        out.insert(
            path.to_lowercase(),
            (t.size[c], t.flags[c].contains(NodeFlags::DIR), t.flags[c].contains(NodeFlags::REPARSE)),
        );
        collect(t, c, &path, out);
    }
}

fn descend(t: &Tree, from: NodeId, components: &[String]) -> Option<NodeId> {
    let mut cur = from;
    for c in components {
        cur = t.children(cur).find(|&k| t.name_string(k).eq_ignore_ascii_case(c))?;
    }
    Some(cur)
}

fn diff(walk: &Entries, mft: &Entries) -> Vec<String> {
    let mut d = Vec::new();
    for (k, w) in walk {
        match mft.get(k) {
            None => d.push(format!("only in walker: {k} {w:?}")),
            Some(m) if m != w => d.push(format!("differs: {k}: walker {w:?}, mft {m:?}")),
            _ => {}
        }
    }
    for k in mft.keys().filter(|k| !walk.contains_key(*k)) {
        d.push(format!("only in mft: {k}"));
    }
    d
}

fn main() {
    let fixture = PathBuf::from(std::env::args().nth(1).expect("usage: scan-compare <new directory on an NTFS drive>"));
    if !is_elevated() {
        eprintln!("scan-compare needs an elevated (administrator) terminal");
        std::process::exit(2);
    }
    assert!(!fixture.exists(), "{} already exists; pick a new directory", fixture.display());
    let comps: Vec<String> = fixture.components().filter_map(|c| match c { std::path::Component::Normal(s) => Some(s), _ => None }).map(|s| s.to_string_lossy().into_owned()).collect();
    let drive = PathBuf::from(format!("{}\\", fixture.to_string_lossy().chars().take(2).collect::<String>()));
    assert!(drive_letter(&drive).is_some(), "fixture must be on a drive letter");

    make_fixture(&fixture);
    let mut walk_entries = Entries::new();
    {
        let mut b = TreeBuilder::new();
        WalkScanner::default().scan(&fixture, &mut b, &Progress::default()).expect("walk");
        let t = b.finish().unwrap();
        collect(&t, NodeId(0), "", &mut walk_entries);
    }
    println!("walker: {} entries under the fixture", walk_entries.len());

    // The MFT on disk can lag the file system cache, so rescan until the two agree or time runs out.
    let deadline = Instant::now() + Duration::from_secs(90);
    let ok = loop {
        let (progress, mut b) = (Progress::default(), TreeBuilder::new());
        let (stats, timings) = MftScanner::default().scan_with_stats(&drive, &mut b, &progress).expect("mft scan");
        let t = b.finish().unwrap();
        let mut mft_entries = Entries::new();
        if let Some(node) = descend(&t, NodeId(0), &comps) {
            collect(&t, node, "", &mut mft_entries);
        }
        let d = diff(&walk_entries, &mft_entries);
        println!(
            "mft: {} nodes ({} orphans, {} corrupt), stream {:?}, link {:?}; {} differences",
            stats.nodes, stats.orphans, stats.corrupt, timings.stream, timings.link, d.len()
        );
        if d.is_empty() {
            break true;
        }
        if Instant::now() > deadline {
            d.iter().take(30).for_each(|l| println!("  {l}"));
            break false;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    remove_fixture(&fixture);
    println!("{}", if ok { "OK: the scanners agree" } else { "FAIL: the scanners disagree" });
    std::process::exit(if ok { 0 } else { 1 });
}
```

- [ ] **Step 2: Build it and check the unelevated refusal**

Run: `cargo build -j 4 --release -p plumb-scan --examples`
Run: `target\release\examples\scan-compare.exe C:\plumb-fixture`
Expected: build succeeds with no warnings; the run prints `scan-compare needs an elevated (administrator) terminal` and exits with code 2.

- [ ] **Step 3: STOP. Ask the user to run it elevated**

Ask the user to open an elevated terminal (Run as administrator), `cd` to the repository, make sure `C:\plumb-fixture` does not exist, and run:

```
target\release\examples\scan-compare.exe C:\plumb-fixture
```

It creates the fixture, scans it with the walker, scans the whole `C:` volume with the MFT scanner, rescans until the two agree (up to 90 s, because the MFT on disk lags the cache), prints the node count and the stream and link times, deletes the fixture, and prints `OK: the scanners agree` (exit 0) or the differences and `FAIL` (exit 1). Ask for the full output.

Expected on success: `OK: the scanners agree`. The fixture has a compressed file and a sparse file, so success also settles the spec's open question about allocated-size parity for those. A difference on `compressed.bin` or `sparse.bin` means the MFT scanner's size rule for compressed or sparse streams (the compressed-size field at offset 0x40 of the `$DATA` attribute) is wrong or incomplete: fix the rule in `parse_data` in `plumb-core/src/mft.rs`, add a test that reproduces it, and rerun. A `Layout` or `Read` error from the MFT scan means the volume glue in `volume.rs` needs fixing; read the message, fix, rerun.

- [ ] **Step 4: Record the result**

Append to `docs/benchmarks.md` a section `## MFT scanner, full volume <date>` with the user's pasted output: the node count, orphans, corrupt count, stream and link times, the volume (`C:`, used space), and whether the machine was loaded. State the result of the compressed and sparse parity check.

- [ ] **Step 5: Commit**

```bash
git add crates docs
git commit -m "Add scan-compare example and MFT scan baseline" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Update the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-10-11-plumb-disk-design.md`

- [ ] **Step 1: Edit the spec**

- In "`MftScanner`", replace the first bullet's extent discovery ("`FSCTL_GET_NTFS_VOLUME_DATA`, retrieval pointers on `X:\$MFT`") with: read `FSCTL_GET_NTFS_VOLUME_DATA`, then read file record 0 at `MftStartLcn` and decode its `$DATA` data runs; if the runs do not cover `MftValidDataLength` (an `$ATTRIBUTE_LIST`), fail and fall back to the walker. Add that only 1024-byte records on volumes with clusters that are a multiple of 1024 bytes are supported.
- In "Open questions to verify during M1", delete the `FSCTL_GET_RETRIEVAL_POINTERS` question (no longer used) and, if Task 4 settled it, the allocated-size parity question for compressed and sparse files, recording the outcome in one line.
- In "Implementation plans", mark 2b with its plan path `docs/superpowers/plans/2026-10-11-plumb-scan-b.md`.

- [ ] **Step 2: Commit**

```bash
git add docs
git commit -m "Spec: MFT extents come from record 0; update open questions" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (2b scope):**
- Open `\\.\X:`, `FSCTL_GET_NTFS_VOLUME_DATA`, stream `$MFT` in 1-4 MB sector-aligned chunks, reader thread plus parser pool, one sequential pass: Tasks 2-3 (the `$MFT` extents are decoded from record 0 instead of `FSCTL_GET_RETRIEVAL_POINTERS`; Task 5 records that spec change).
- Per-record parsing and the merge pass: Plans 1 and 2a, used here unchanged.
- Choosing and failing (elevated + NTFS → MFT, else walker, fallback with a note, corrupt records counted, an error leaves the builder usable): Task 3, with the status-bar wording supplied by `ScanReport::note` for Plan 3 to show.
- Testing section: `scan-compare` on a hard-link-free fixture with the subtree comparison: Task 4. Phase timings (stream, link): `MftTimings`.
- Deferred: the UI (Plan 3), the USN live updates (M2), `$ATTRIBUTE_LIST`-fragmented MFTs.

**Placeholder scan:** none; all code blocks are the files that passed the tests. Task 4's recorded results come from the user's run.

**Type consistency:** `scan_mft`'s signature, `MftLayout`, `MftOptions`, `MftTimings`, `MergeStats`, `Volume`, `MftScanner::scan_with_stats`, `ScanReport` and `Used` are used identically across tasks and in the example. Test counts: core 40; plumb-scan 22 after Task 2 (10 + 6 + 6) and 25 after Task 3 (+3 select tests).
