//! What a changed file looks like now, read from its MFT record. Live updates are state-based:
//! the journal only says "record N changed"; the record says what is true. That makes record
//! reuse, repeated renames and directory moves all the same case.

use super::usn::REF_MASK;
use crate::merge::is_name_surrogate;
use plumb_core::mft::{ParseError, ParsedRecord, parse_fixed_up_record};

/// One file as it is now. Names follow the scanner's rule: of a hard-linked file's names, the one
/// with the lowest (parent, name) is the one that carries the size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub record: u64,
    pub sequence: u16,
    pub parent: u64,
    pub name: Vec<u16>,
    /// Allocated bytes of the unnamed data stream; 0 for directories.
    pub size: u64,
    pub is_dir: bool,
    /// A symlink or junction.
    pub reparse: bool,
    /// How many names the record has; more than 1 means hard links.
    pub links: u32,
}

impl FileState {
    /// The 64-bit file reference (sequence in the high 16 bits), which `OpenFileById` wants.
    pub fn frn(&self) -> u64 {
        self.record | u64::from(self.sequence) << 48
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// The record is free, or has no usable name.
    Gone,
    /// An extension record: the file it belongs to must be read instead.
    Extension(u64),
    /// `needs_size`: a file whose base record has no unnamed data stream (it lives in an extension
    /// record), so `state.size` is 0 until the caller asks the file system.
    Live { state: FileState, needs_size: bool },
}

/// Parses record number `record`. `buf` is exactly one record as `FSCTL_GET_NTFS_FILE_RECORD`
/// returns it: the update-sequence fixups are already undone.
pub fn observe(record: u64, buf: &[u8], scratch: &mut ParsedRecord) -> Result<Seen, ParseError> {
    parse_fixed_up_record(buf, scratch)?;
    if !scratch.in_use {
        return Ok(Seen::Gone);
    }
    if let Some(base) = scratch.base {
        return Ok(Seen::Extension(base & REF_MASK));
    }
    let Some(first) = scratch.names.iter().min_by(|a, b| (a.parent, scratch.name(a)).cmp(&(b.parent, scratch.name(b))))
    else {
        return Ok(Seen::Gone);
    };
    let state = FileState {
        record,
        sequence: scratch.sequence,
        parent: first.parent,
        name: scratch.name(first).to_vec(),
        size: if scratch.is_dir { 0 } else { scratch.data_alloc.unwrap_or(0) },
        is_dir: scratch.is_dir,
        reparse: is_name_surrogate(scratch.reparse_tag),
        links: scratch.names.len() as u32,
    };
    Ok(Seen::Live { needs_size: !scratch.is_dir && scratch.data_alloc.is_none(), state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::mft::apply_fixups;
    use plumb_core::synth::RecordBuilder;

    /// A record the way the file system hands it out: fixups undone.
    fn fixed(rec: RecordBuilder) -> Vec<u8> {
        let mut buf = rec.build();
        apply_fixups(&mut buf).unwrap();
        buf
    }

    fn seen(rec: RecordBuilder, number: u64) -> Seen {
        observe(number, &fixed(rec), &mut ParsedRecord::default()).unwrap()
    }

    fn live(s: Seen) -> (FileState, bool) {
        match s {
            Seen::Live { state, needs_size } => (state, needs_size),
            other => panic!("not live: {other:?}"),
        }
    }

    #[test]
    fn a_plain_file() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(4096, 100), 31));
        assert_eq!((s.record, s.parent, s.size, s.is_dir, s.reparse, s.links), (31, 30, 4096, false, false, 1));
        assert_eq!(String::from_utf16_lossy(&s.name), "a.txt");
        assert!(!needs);
        assert_eq!(s.frn(), 31 | 1 << 48, "the record builder stamps sequence 1");
    }

    #[test]
    fn a_directory_has_no_size_and_never_needs_one() {
        let (s, needs) = live(seen(RecordBuilder::dir().file_name(5, "d", 1), 40));
        assert_eq!((s.is_dir, s.size, needs), (true, 0, false));
    }

    #[test]
    fn resident_data_is_size_zero_without_a_fallback() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(5, "tiny", 1).data_resident(10), 41));
        assert_eq!((s.size, needs), (0, false));
    }

    #[test]
    fn a_file_without_an_unnamed_stream_asks_for_its_size() {
        let (s, needs) = live(seen(RecordBuilder::file().file_name(5, "big", 1), 42));
        assert_eq!((s.size, needs), (0, true));
    }

    #[test]
    fn hard_links_report_the_lowest_parent_then_name() {
        let rec = RecordBuilder::file().file_name(30, "x", 1).file_name(5, "z", 1).file_name(5, "y", 1).data_nonresident(4096, 1);
        let (s, _) = live(seen(rec, 43));
        assert_eq!((s.parent, String::from_utf16_lossy(&s.name), s.links), (5, "y".to_string(), 3));
    }

    #[test]
    fn symlinks_are_marked() {
        let (s, _) = live(seen(RecordBuilder::dir().file_name(5, "j", 1).reparse(0xA000_0003), 44));
        assert!(s.reparse);
    }

    #[test]
    fn extension_records_point_at_their_base() {
        assert_eq!(seen(RecordBuilder::file().extension_of((3 << 48) | 77), 90), Seen::Extension(77));
    }

    #[test]
    fn a_free_record_is_gone() {
        let mut buf = fixed(RecordBuilder::file().file_name(5, "old", 1).data_nonresident(4096, 1));
        buf[0x16] &= !1; // clear the in-use flag
        assert_eq!(observe(9, &buf, &mut ParsedRecord::default()), Ok(Seen::Gone));
    }
}
