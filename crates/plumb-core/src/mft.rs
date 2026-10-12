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
}
