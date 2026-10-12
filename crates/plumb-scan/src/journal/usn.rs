//! Parsing the output of `FSCTL_READ_USN_JOURNAL` (version 2 records, which is what NTFS returns
//! when asked for major version 2). Only what live updates need is kept: which file changed.

/// The low 48 bits of a file reference are the MFT record number; the high 16 are its sequence.
pub const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const HEADER: usize = 8; // the next USN, before the first record
const V2_FIXED: usize = 60; // USN_RECORD_V2 up to and including FileNameOffset

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsnEntry {
    /// MFT record number of the file that changed.
    pub record: u64,
    pub usn: i64,
    pub reason: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct UsnBatch {
    /// Where the next read should start.
    pub next_usn: i64,
    pub entries: Vec<UsnEntry>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Parses a read buffer: the next USN, then back-to-back records. `None` if it is malformed
/// (too short, a record that overruns the buffer, or a version other than 2).
pub fn parse_usn_buffer(buf: &[u8]) -> Option<UsnBatch> {
    if buf.len() < HEADER {
        return None;
    }
    let mut batch = UsnBatch { next_usn: u64_at(buf, 0) as i64, entries: Vec::new() };
    let mut off = HEADER;
    while off < buf.len() {
        let rest = &buf[off..];
        if rest.len() < V2_FIXED {
            return None;
        }
        let len = u32_at(rest, 0) as usize;
        if len < V2_FIXED || len > rest.len() || u16_at(rest, 4) != 2 {
            return None;
        }
        batch.entries.push(UsnEntry {
            record: u64_at(rest, 8) & REF_MASK,
            usn: u64_at(rest, 24) as i64,
            reason: u32_at(rest, 40),
        });
        off += len.next_multiple_of(8);
    }
    Some(batch)
}

#[cfg(test)]
pub(crate) fn encode(next_usn: i64, entries: &[(u64, u32)]) -> Vec<u8> {
    let mut b = (next_usn as u64).to_le_bytes().to_vec();
    for (i, &(frn, reason)) in entries.iter().enumerate() {
        let name: Vec<u16> = format!("f{i}").encode_utf16().collect();
        let len = (V2_FIXED + name.len() * 2).next_multiple_of(8);
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[24..32].copy_from_slice(&(100 + i as i64).to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        r[58..60].copy_from_slice(&(V2_FIXED as u16).to_le_bytes());
        for (k, u) in name.iter().enumerate() {
            r[V2_FIXED + 2 * k..V2_FIXED + 2 * k + 2].copy_from_slice(&u.to_le_bytes());
        }
        b.extend(r);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_buffer_is_just_the_next_usn() {
        let b = parse_usn_buffer(&encode(4096, &[])).unwrap();
        assert_eq!(b, UsnBatch { next_usn: 4096, entries: vec![] });
    }

    #[test]
    fn records_are_read_back_to_back_and_the_sequence_is_dropped() {
        let frn = (7u64 << 48) | 1234; // sequence 7, record 1234
        let b = parse_usn_buffer(&encode(900, &[(frn, 0x100), (55, 0x8000_0000), (frn, 2)])).unwrap();
        assert_eq!(b.next_usn, 900);
        let got: Vec<_> = b.entries.iter().map(|e| (e.record, e.usn, e.reason)).collect();
        assert_eq!(got, vec![(1234, 100, 0x100), (55, 101, 0x8000_0000), (1234, 102, 2)]);
    }

    #[test]
    fn malformed_buffers_are_rejected() {
        assert!(parse_usn_buffer(&[1, 2, 3]).is_none());
        let good = encode(1, &[(5, 1)]);
        assert!(parse_usn_buffer(&good[..good.len() - 8]).is_none(), "a record overruns the buffer");
        let mut v3 = good.clone();
        v3[8 + 4] = 3;
        assert!(parse_usn_buffer(&v3).is_none(), "version 3");
        let mut short = good.clone();
        short[8..12].copy_from_slice(&10u32.to_le_bytes());
        assert!(parse_usn_buffer(&short).is_none(), "record length below the fixed part");
    }
}
