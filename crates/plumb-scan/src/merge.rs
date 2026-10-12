//! The MFT merge pass: collects parsed records (in any order, base and extension records mixed)
//! and builds the tree. Parent links, hard links and orphans are all decided here, in record-number
//! order, so the result does not depend on which thread parsed what first.

use plumb_core::mft::ParsedRecord;
use plumb_core::{NodeFlags, NodeId, TreeBuilder};

/// Record number of the root directory of every NTFS volume.
const ROOT_RECORD: usize = 5;
const NONE: u32 = u32::MAX;
const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// True for the reparse tags the scanners treat as links to follow elsewhere (symlink, junction).
pub fn is_name_surrogate(tag: Option<u32>) -> bool {
    matches!(tag, Some(REPARSE_TAG_MOUNT_POINT | REPARSE_TAG_SYMLINK))
}

const F_SEEN: u8 = 1; // the base record was added
const F_IN_USE: u8 = 2;
const F_DIR: u8 = 4;
const F_REPARSE: u8 = 8; // name-surrogate reparse tag (symlink, junction)
const F_HAS_ALLOC: u8 = 16;

struct NameRec {
    /// Record number of the file this name belongs to (the base record for extensions).
    owner: u32,
    /// Record number of the parent directory.
    parent: u32,
    off: u32,
    len: u8,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// In-use records that produced at least one node.
    pub records: u64,
    pub nodes: u64,
    /// Nodes placed under the synthetic `<orphans>` directory.
    pub orphans: u64,
    /// Records the caller reported as unparseable, plus ones that pointed outside the MFT.
    pub corrupt: u64,
}

pub struct MftMerger {
    flags: Vec<u8>,
    alloc: Vec<u64>,
    names: Vec<NameRec>,
    pool: Vec<u16>,
    corrupt: u64,
}

impl MftMerger {
    /// `record_count` is the number of 1 KiB records in `$MFT` (its size divided by 1024).
    pub fn new(record_count: usize) -> Self {
        Self {
            flags: vec![0; record_count],
            alloc: vec![0; record_count],
            names: Vec::new(),
            pool: Vec::new(),
            corrupt: 0,
        }
    }

    /// Counts a record that failed to parse.
    pub fn note_corrupt(&mut self) {
        self.corrupt += 1;
    }

    /// Adds the parsed record number `number`. Base and extension records may arrive in any order.
    pub fn add(&mut self, number: u64, rec: &ParsedRecord) {
        let owner = rec.base.unwrap_or(number);
        let (Some(o), Ok(own32)) = (usize::try_from(owner).ok().filter(|&o| o < self.flags.len()), u32::try_from(owner))
        else {
            self.corrupt += 1;
            return;
        };
        if rec.base.is_none() {
            self.flags[o] |= F_SEEN;
        }
        if !rec.in_use {
            return;
        }
        if rec.base.is_none() {
            self.flags[o] |= F_IN_USE;
            if rec.is_dir {
                self.flags[o] |= F_DIR;
            }
        }
        for e in &rec.names {
            let Ok(parent) = u32::try_from(e.parent) else {
                self.corrupt += 1;
                continue;
            };
            let off = self.pool.len() as u32;
            self.pool.extend_from_slice(rec.name(e));
            self.names.push(NameRec { owner: own32, parent, off, len: e.len });
        }
        if let Some(a) = rec.data_alloc {
            if self.flags[o] & F_HAS_ALLOC == 0 {
                self.flags[o] |= F_HAS_ALLOC;
                self.alloc[o] = a;
            }
        }
        if is_name_surrogate(rec.reparse_tag) {
            self.flags[o] |= F_REPARSE;
        }
    }

    fn name(&self, n: &NameRec) -> &[u16] {
        &self.pool[n.off as usize..n.off as usize + n.len as usize]
    }

    /// Builds the nodes into `out`: the root (named `root_name`) first, then everything else.
    /// Also returns, for every record number, the node of its first name (`NodeId::NONE` if it has
    /// none); live updates use it to find the node a changed record belongs to.
    pub fn finish(self, root_name: &str, out: &mut TreeBuilder) -> (MergeStats, Vec<NodeId>) {
        let n = self.flags.len();
        let base = out.len() as u32;

        // Names grouped by owner, and within an owner ordered by (parent, name) so that
        // "which link carries the size" does not depend on arrival order.
        let mut order: Vec<u32> = (0..self.names.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| {
            let (a, b) = (&self.names[a as usize], &self.names[b as usize]);
            (a.owner, a.parent).cmp(&(b.owner, b.parent)).then_with(|| self.name(a).cmp(self.name(b)))
        });

        // Pass A: give every node an index. `first[rec]` is the index of the record's first node.
        let live = |r: usize| self.flags[r] & (F_SEEN | F_IN_USE) == (F_SEEN | F_IN_USE);
        let mut first = vec![NONE; n];
        let mut count_of = vec![0u32; n];
        let mut next = 1u32; // index 0 is the root
        let mut i = 0;
        while i < order.len() {
            let owner = self.names[order[i] as usize].owner as usize;
            let mut j = i;
            while j < order.len() && self.names[order[j] as usize].owner as usize == owner {
                j += 1;
            }
            if owner != ROOT_RECORD && live(owner) {
                let k = if self.flags[owner] & F_DIR != 0 { 1 } else { (j - i) as u32 };
                first[owner] = next;
                count_of[owner] = k;
                next += k;
            }
            i = j;
        }
        first[ROOT_RECORD] = 0;
        let node_count = next as usize;

        // Pass B: decide every node's parent (an index, or ORPHANS for the synthetic directory).
        let orphans_idx = next;
        let parent_of = |parent_rec: u32| -> u32 {
            let p = parent_rec as usize;
            if p < n && first[p] != NONE && (p == ROOT_RECORD || self.flags[p] & F_DIR != 0) {
                first[p]
            } else {
                orphans_idx
            }
        };
        // (index -> (owner record, name record, is the size-carrying link))
        let mut rows: Vec<(u32, u32, bool)> = Vec::with_capacity(node_count);
        let mut par: Vec<u32> = Vec::with_capacity(node_count);
        par.push(NONE);
        rows.push((ROOT_RECORD as u32, NONE, true));
        let mut i = 0;
        while i < order.len() {
            let owner = self.names[order[i] as usize].owner as usize;
            let mut j = i;
            while j < order.len() && self.names[order[j] as usize].owner as usize == owner {
                j += 1;
            }
            if owner != ROOT_RECORD && live(owner) {
                for (link, &ni) in order[i..i + count_of[owner] as usize].iter().enumerate() {
                    par.push(parent_of(self.names[ni as usize].parent));
                    rows.push((owner as u32, ni, link == 0));
                }
            }
            i = j;
        }

        // Directories that only lead to each other never reach the root: send them to <orphans>.
        // 0 = unvisited, 1 = on the current path, 2 = known to reach the root or <orphans>.
        let mut state = vec![0u8; node_count];
        state[0] = 2;
        let mut path: Vec<usize> = Vec::new();
        for start in 1..node_count {
            let mut cur = start;
            while state[cur] == 0 {
                state[cur] = 1;
                path.push(cur);
                let p = par[cur];
                if p == orphans_idx {
                    break;
                }
                cur = p as usize;
            }
            if state[cur] == 1 && par[cur] != orphans_idx {
                // `cur` is on the current path again: a cycle. Cut it there.
                par[cur] = orphans_idx;
            }
            for v in path.drain(..) {
                state[v] = 2;
            }
        }

        // Push in index order.
        let mut stats = MergeStats { corrupt: self.corrupt, ..Default::default() };
        out.push_str(NodeId::NONE, root_name, 0, NodeFlags::DIR);
        let mut used_orphans = false;
        let mut last_owner = u32::MAX;
        for (idx, &(owner, ni, carries)) in rows.iter().enumerate().skip(1) {
            let o = owner as usize;
            if owner != last_owner {
                stats.records += 1;
                last_owner = owner;
            }
            let is_dir = self.flags[o] & F_DIR != 0;
            let mut flags = NodeFlags::empty();
            if is_dir {
                flags |= NodeFlags::DIR;
            }
            if self.flags[o] & F_REPARSE != 0 {
                flags |= NodeFlags::REPARSE;
            }
            if !carries {
                flags |= NodeFlags::HARDLINK;
            }
            let size = if is_dir || !carries { 0 } else { self.alloc[o] };
            let p = par[idx];
            let parent = if p == orphans_idx {
                used_orphans = true;
                stats.orphans += 1;
                NodeId(base + orphans_idx)
            } else {
                NodeId(base + p)
            };
            out.push(parent, self.name(&self.names[ni as usize]), size, flags);
            stats.nodes += 1;
        }
        if used_orphans {
            out.push_str(NodeId(base), "<orphans>", 0, NodeFlags::DIR);
            stats.nodes += 1;
        }
        stats.nodes += 1; // the root
        let records = first.into_iter().map(|f| if f == NONE { NodeId::NONE } else { NodeId(base + f) }).collect();
        (stats, records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumb_core::mft::parse_record;
    use plumb_core::synth::RecordBuilder;
    use plumb_core::Tree;

    fn feed(m: &mut MftMerger, number: u64, rec: RecordBuilder) {
        let mut buf = rec.build();
        let mut p = ParsedRecord::default();
        parse_record(&mut buf, &mut p).unwrap();
        m.add(number, &p);
    }

    fn root(m: &mut MftMerger) {
        feed(m, 5, RecordBuilder::dir().file_name(5, ".", 1));
    }

    fn build(m: MftMerger) -> (Tree, MergeStats) {
        let mut b = TreeBuilder::new();
        let (stats, _) = m.finish("X:", &mut b);
        (b.finish().unwrap(), stats)
    }

    fn paths(t: &Tree) -> Vec<String> {
        let mut v: Vec<String> = (0..t.len() as u32).map(|i| t.path(NodeId(i))).collect();
        v.sort();
        v
    }

    fn find(t: &Tree, path: &str) -> NodeId {
        (0..t.len() as u32).map(NodeId).find(|&i| t.path(i) == path).unwrap_or_else(|| panic!("no {path}"))
    }

    fn sample(order: &[usize]) -> MftMerger {
        let mut m = MftMerger::new(100);
        let recs = [
            (5, RecordBuilder::dir().file_name(5, ".", 1)),
            (30, RecordBuilder::dir().file_name(5, "docs", 1)),
            (31, RecordBuilder::file().file_name(30, "a.txt", 1).data_nonresident(4096, 100)),
            (32, RecordBuilder::file().file_name(5, "b", 1).data_nonresident(8192, 8000)),
        ];
        let mut recs: Vec<_> = recs.into_iter().map(Some).collect();
        for &i in order {
            let (n, r) = recs[i].take().unwrap();
            feed(&mut m, n, r);
        }
        m
    }

    #[test]
    fn the_record_table_maps_record_numbers_to_nodes() {
        let mut b = TreeBuilder::new();
        let (_, records) = sample(&[0, 1, 2, 3]).finish("X:", &mut b);
        let t = b.finish().unwrap();
        assert_eq!(records.len(), 100);
        assert_eq!(t.path(records[5]), "X:");
        assert_eq!(t.path(records[30]), "X:\\docs");
        assert_eq!(t.path(records[31]), "X:\\docs\\a.txt");
        assert_eq!(t.path(records[32]), "X:\\b");
        assert!(records[6].is_none() && records[99].is_none(), "unused records have no node");
    }

    #[test]
    fn builds_tree_with_sums() {
        let (t, stats) = build(sample(&[0, 1, 2, 3]));
        assert_eq!(paths(&t), vec!["X:", "X:\\b", "X:\\docs", "X:\\docs\\a.txt"]);
        assert_eq!(t.subtree_size[NodeId(0)], 4096 + 8192);
        assert_eq!(stats, MergeStats { records: 3, nodes: 4, orphans: 0, corrupt: 0 });
    }

    #[test]
    fn arrival_order_does_not_matter() {
        let (a, _) = build(sample(&[0, 1, 2, 3]));
        let (b, _) = build(sample(&[3, 2, 1, 0]));
        assert_eq!(paths(&a), paths(&b));
        assert_eq!(a.subtree_size[NodeId(0)], b.subtree_size[NodeId(0)]);
        let names = |t: &Tree| (0..t.len() as u32).map(|i| t.name_string(NodeId(i))).collect::<Vec<_>>();
        assert_eq!(names(&a), names(&b)); // node order is by record number, not arrival
    }

    #[test]
    fn hard_links_count_once_on_the_first_link_by_parent() {
        let mut m = sample(&[0, 1, 2, 3]);
        feed(&mut m, 40, RecordBuilder::file().file_name(30, "x", 1).file_name(5, "y", 1).data_nonresident(4096, 10));
        let (t, _) = build(m);
        let y = find(&t, "X:\\y");
        let x = find(&t, "X:\\docs\\x");
        assert_eq!(t.size[y], 4096);
        assert!(!t.flags[y].contains(NodeFlags::HARDLINK));
        assert_eq!(t.size[x], 0);
        assert!(t.flags[x].contains(NodeFlags::HARDLINK));
        assert_eq!(t.subtree_size[NodeId(0)], 4096 + 8192 + 4096);
    }

    #[test]
    fn dos_names_do_not_create_nodes() {
        let mut m = sample(&[0]);
        feed(&mut m, 41, RecordBuilder::file().file_name(5, "PROGRA~1", 2).file_name(5, "Program Files", 1));
        let (t, _) = build(m);
        assert_eq!(paths(&t), vec!["X:", "X:\\Program Files"]);
    }

    #[test]
    fn extension_records_supply_size_and_names_before_or_after_the_base() {
        for ext_first in [false, true] {
            let mut m = sample(&[0]);
            let ext = || RecordBuilder::file().extension_of(31 | (1 << 48)).data_nonresident(8192, 8000);
            let base = || RecordBuilder::file().file_name(5, "big", 1);
            if ext_first {
                feed(&mut m, 90, ext());
                feed(&mut m, 31, base());
            } else {
                feed(&mut m, 31, base());
                feed(&mut m, 90, ext());
            }
            let (t, _) = build(m);
            assert_eq!(t.size[find(&t, "X:\\big")], 8192);
        }
    }

    #[test]
    fn free_records_are_skipped() {
        let mut m = sample(&[0]);
        let mut buf = RecordBuilder::file().file_name(5, "gone", 1).build();
        buf[0x16] = 0; // not in use
        let mut p = ParsedRecord::default();
        parse_record(&mut buf, &mut p).unwrap();
        m.add(50, &p);
        let (t, _) = build(m);
        assert_eq!(paths(&t), vec!["X:"]);
    }

    #[test]
    fn missing_parents_go_under_orphans() {
        let mut m = sample(&[0]);
        feed(&mut m, 60, RecordBuilder::file().file_name(77, "lost", 1).data_nonresident(4096, 1));
        let (t, stats) = build(m);
        assert_eq!(paths(&t), vec!["X:", "X:\\<orphans>", "X:\\<orphans>\\lost"]);
        assert_eq!(stats.orphans, 1);
        assert_eq!(t.subtree_size[NodeId(0)], 4096);
    }

    #[test]
    fn parent_cycles_are_cut() {
        let mut m = sample(&[0]);
        feed(&mut m, 50, RecordBuilder::dir().file_name(51, "d50", 1));
        feed(&mut m, 51, RecordBuilder::dir().file_name(50, "d51", 1));
        let (t, stats) = build(m);
        assert_eq!(stats.orphans, 1);
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn only_name_surrogate_reparse_points_are_flagged() {
        let mut m = sample(&[0]);
        feed(&mut m, 70, RecordBuilder::dir().file_name(5, "junction", 1).reparse(0xA000_0003));
        feed(&mut m, 71, RecordBuilder::dir().file_name(5, "cloud", 1).reparse(0x9000_001A));
        let (t, _) = build(m);
        assert!(t.flags[find(&t, "X:\\junction")].contains(NodeFlags::REPARSE));
        assert!(!t.flags[find(&t, "X:\\cloud")].contains(NodeFlags::REPARSE));
    }

    #[test]
    fn out_of_range_records_count_as_corrupt() {
        let mut m = MftMerger::new(10);
        root(&mut m);
        feed(&mut m, 500, RecordBuilder::file().file_name(5, "x", 1));
        m.note_corrupt();
        let (_, stats) = build(m);
        assert_eq!(stats.corrupt, 2);
    }
}
