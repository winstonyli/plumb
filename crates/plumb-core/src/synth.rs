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

    /// A `$STANDARD_INFORMATION` attribute whose last-write time (value offset 0x08) is `modified`.
    pub fn standard_info(mut self, modified: u64) -> Self {
        let mut v = vec![0u8; 0x30];
        put64(&mut v, 0x08, modified);
        self.attrs.push(resident_attr(0x10, "", &v));
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

/// `s` as UTF-16 without a terminator: how names are stored in a tree.
pub fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
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
