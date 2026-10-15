//! NTFS `$MFT` file-record parser: bytes in, plain structs out. No I/O.

const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
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
    /// The record's sequence number; with the record number it forms the 64-bit file reference.
    pub sequence: u16,
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
    /// Last-write day from `$STANDARD_INFORMATION` (base records only).
    pub mtime: Option<std::num::NonZeroU32>,
}

impl ParsedRecord {
    pub fn clear(&mut self) {
        self.in_use = false;
        self.sequence = 0;
        self.is_dir = false;
        self.base = None;
        self.names.clear();
        self.name_pool.clear();
        self.data_alloc = None;
        self.reparse_tag = None;
        self.mtime = None;
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

/// Like [`parse_record`] for a record whose fixups are already undone, which is what
/// `FSCTL_GET_NTFS_FILE_RECORD` returns.
pub fn parse_fixed_up_record(buf: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    out.clear();
    if buf.get(0..4) != Some(b"FILE") {
        return Err(ParseError::BadMagic);
    }
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
    out.sequence = u16_at(buf, 0x10)?;
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
        if len < 0x18 || !len.is_multiple_of(8) || off + len > used {
            return Err(ParseError::BadAttribute);
        }
        let a = &buf[off..off + len];
        match kind {
            ATTR_STANDARD_INFORMATION => parse_standard_information(a, out)?,
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
        if len < 0x18 || !len.is_multiple_of(8) || off + len > used {
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

fn parse_standard_information(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] != 0 {
        return Ok(()); // always resident; ignore anything else
    }
    let v = resident_value(a)?;
    out.mtime = crate::age::day_of(u64_at(v, 0x08)?);
    Ok(())
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
    out.name_pool.extend(chars.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])));
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
    fn standard_information_gives_the_last_write_day() {
        let ft = 5 * crate::age::DAY_TICKS + 123;
        let (r, p) = parse(RecordBuilder::file().standard_info(ft).file_name(5, "a.txt", 1).data_resident(10).build());
        assert_eq!(r, Ok(()));
        assert_eq!(p.mtime, std::num::NonZeroU32::new(5));
        let (_, p) = parse(RecordBuilder::file().file_name(5, "b", 1).build());
        assert_eq!(p.mtime, None);
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
        assert_eq!(p.sequence, 1, "the record builder stamps sequence 1");
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
