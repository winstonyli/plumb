use super::*;
use plumb_core::TreeBuilder;
use std::collections::BTreeMap;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn st(record: u64, parent: u64, name: &str, size: u64, is_dir: bool) -> FileState {
    FileState { record, sequence: 1, parent, name: u(name), size, is_dir, reparse: false, links: 1, mtime: None }
}

fn up(record: u64, parent: u64, name: &str, size: u64) -> Delta {
    Delta::Upsert(st(record, parent, name, size, false))
}

fn dir(record: u64, parent: u64, name: &str) -> Delta {
    Delta::Upsert(st(record, parent, name, 0, true))
}

/// C: (record 5) with docs (30) holding a.txt (31, 100) and b.txt (33, 50), and tmp (32).
fn fixture() -> (Tree, Live) {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
    let a = b.push_str(docs, "a.txt", 100, NodeFlags::empty());
    let tmp = b.push_str(root, "tmp", 0, NodeFlags::DIR);
    let c = b.push_str(docs, "b.txt", 50, NodeFlags::empty());
    let mut records = vec![NodeId::NONE; 40];
    for (r, n) in [(5, root), (30, docs), (31, a), (32, tmp), (33, c)] {
        records[r] = n;
    }
    (b.finish().unwrap(), Live::new(records))
}

fn run(live: &mut Live, tree: &mut Tree, deltas: Vec<Delta>) -> usize {
    let mut q: VecDeque<Delta> = deltas.into();
    live.apply(tree, &mut q, Instant::now() + Duration::from_secs(10))
}

/// Path -> (size, is_dir) for every live node.
fn snapshot(t: &Tree) -> BTreeMap<String, (u64, bool)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<NodeId> = t.roots.clone();
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.subtree_size[n], t.flags[n].contains(NodeFlags::DIR)));
        stack.extend(t.children(n));
    }
    out
}

fn size_of(t: &Tree, path: &str) -> Option<u64> {
    snapshot(t).get(path).map(|v| v.0)
}

#[test]
fn a_new_file_appears_and_its_size_reaches_every_ancestor() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![up(34, 32, "new.bin", 70)]), 1);
    assert_eq!(size_of(&t, "C:\\tmp\\new.bin"), Some(70));
    assert_eq!(size_of(&t, "C:\\tmp"), Some(70));
    assert_eq!(size_of(&t, "C:"), Some(220));
}

#[test]
fn growing_renaming_and_moving_in_one_upsert_all_apply() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![up(31, 32, "moved.txt", 400)]);
    assert_eq!(size_of(&t, "C:\\tmp\\moved.txt"), Some(400));
    assert_eq!(size_of(&t, "C:\\docs\\a.txt"), None);
    assert_eq!(size_of(&t, "C:\\docs"), Some(50));
    assert_eq!(size_of(&t, "C:"), Some(450));
}

#[test]
fn a_repeated_upsert_changes_nothing() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![up(31, 30, "a.txt", 100), dir(30, 5, "docs")]), 0);
    assert!(t.take_touched().is_empty());
}

#[test]
fn removing_a_folder_drops_its_files_and_the_record_can_be_reused() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![Delta::Remove { record: 30 }, Delta::Remove { record: 31 }, Delta::Remove { record: 33 }]);
    assert_eq!(size_of(&t, "C:"), Some(0));
    assert_eq!(size_of(&t, "C:\\docs"), None);
    run(&mut l, &mut t, vec![up(31, 32, "again.txt", 9)]);
    assert_eq!(size_of(&t, "C:\\tmp\\again.txt"), Some(9));
    // Removing what is not known, or already gone, is ignored.
    assert_eq!(run(&mut l, &mut t, vec![Delta::Remove { record: 30 }, Delta::Remove { record: 9999 }]), 0);
}

#[test]
fn a_child_that_arrives_before_its_new_parent_is_placed_in_the_same_call() {
    let (mut t, mut l) = fixture();
    let deltas = vec![up(52, 51, "deep.txt", 5), dir(51, 50, "two"), dir(50, 5, "one")];
    run(&mut l, &mut t, deltas);
    assert_eq!(size_of(&t, "C:\\one\\two\\deep.txt"), Some(5));
    assert!(!l.pending());
    assert_eq!(size_of(&t, "C:\\<orphans>"), None);
}

#[test]
fn an_unknown_parent_waits_one_pass_and_the_second_orphans_it() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![up(60, 99, "lost.txt", 4)]);
    assert!(l.pending());
    assert_eq!(size_of(&t, "C:"), Some(150));
    // The parent turns up in the next batch.
    run(&mut l, &mut t, vec![dir(99, 5, "late")]);
    assert_eq!(size_of(&t, "C:\\late\\lost.txt"), Some(4));
    assert!(!l.pending());

    run(&mut l, &mut t, vec![up(61, 98, "never.txt", 6)]);
    assert!(l.pending());
    run(&mut l, &mut t, vec![]);
    assert!(!l.pending());
    assert_eq!(size_of(&t, "C:\\<orphans>\\never.txt"), Some(6));
    // A later orphan reuses the same folder.
    run(&mut l, &mut t, vec![up(62, 97, "also.txt", 1)]);
    run(&mut l, &mut t, vec![]);
    assert_eq!(size_of(&t, "C:\\<orphans>"), Some(7));
}

#[test]
fn two_folders_swapping_places_end_up_right_whatever_the_order() {
    // Before: C:\docs\sub. After: C:\sub\docs. Both events arrive, in the worst order.
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
    let sub = b.push_str(docs, "sub", 0, NodeFlags::DIR);
    let mut records = vec![NodeId::NONE; 40];
    for (r, n) in [(5, root), (30, docs), (31, sub)] {
        records[r] = n;
    }
    let (mut t, mut l) = (b.finish().unwrap(), Live::new(records));
    run(&mut l, &mut t, vec![dir(30, 31, "docs"), dir(31, 5, "sub")]);
    let s = snapshot(&t);
    assert!(s.contains_key("C:\\sub\\docs"), "{:?}", s.keys().collect::<Vec<_>>());
    assert!(!s.contains_key("C:\\docs"));
}

#[test]
fn a_file_that_becomes_a_folder_is_replaced() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![dir(31, 30, "a.txt")]);
    let s = snapshot(&t);
    assert_eq!(s["C:\\docs\\a.txt"], (0, true));
    assert_eq!(size_of(&t, "C:"), Some(50));
}

#[test]
fn the_volume_root_record_is_never_touched() {
    let (mut t, mut l) = fixture();
    assert_eq!(run(&mut l, &mut t, vec![dir(5, 5, "C:")]), 0);
    assert!(!l.pending());
}

#[test]
fn records_beyond_the_table_grow_it() {
    let (mut t, mut l) = fixture();
    run(&mut l, &mut t, vec![dir(100_000, 5, "far"), up(100_001, 100_000, "f", 3)]);
    assert_eq!(size_of(&t, "C:\\far\\f"), Some(3));
}

#[test]
fn an_expired_deadline_stops_after_one_clock_check_and_the_rest_waits() {
    let (mut t, mut l) = fixture();
    let mut q: VecDeque<Delta> = (0..1000).map(|i| up(1000 + i, 5, &format!("f{i}"), 1)).collect();
    let n = l.apply(&mut t, &mut q, Instant::now() - Duration::from_secs(1));
    assert_eq!(n, CLOCK_EVERY);
    assert_eq!(q.len(), 1000 - CLOCK_EVERY);
    let n = l.apply(&mut t, &mut q, Instant::now() + Duration::from_secs(10));
    assert_eq!(n, 1000 - CLOCK_EVERY);
    assert_eq!(size_of(&t, "C:"), Some(1150));
}

#[test]
fn compaction_needs_both_a_ratio_and_a_floor() {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "R", 0, NodeFlags::DIR);
    let ids: Vec<_> = (0..100_000).map(|i| b.push_str(root, &format!("f{i}"), 1, NodeFlags::empty())).collect();
    let mut t = b.finish().unwrap();
    for &i in &ids[..9_999] {
        t.remove(i);
    }
    assert!(!needs_compaction(&t), "9999 dead is under the 10000 floor");
    for &i in &ids[9_999..20_000] {
        t.remove(i);
    }
    assert!(!needs_compaction(&t), "20000 dead of 80001 live is exactly a quarter, not over");
    t.remove(ids[20_000]);
    assert!(needs_compaction(&t), "20001 dead of 80000 live is over a quarter");
}

#[test]
fn a_tiny_tree_with_one_dead_node_does_not_ask_for_a_rescan() {
    let (mut t, _) = fixture();
    t.remove(NodeId(2));
    assert!(!needs_compaction(&t));
}

#[test]
fn the_pacer_allows_four_refreshes_a_second_and_backs_off_when_they_are_slow() {
    let t0 = Instant::now();
    let mut p = Pacer::new(t0);
    assert!(p.due(t0));
    p.done(t0, Duration::from_millis(10));
    assert!(!p.due(t0 + Duration::from_millis(249)));
    assert!(p.due(t0 + Duration::from_millis(250)));
    p.done(t0, Duration::from_millis(244));
    assert!(!p.due(t0 + Duration::from_millis(1219)));
    assert!(p.due(t0 + Duration::from_millis(1220)));
}

#[test]
fn rescans_are_rate_limited_per_volume_and_wait_for_the_user_to_go_quiet() {
    let t0 = Instant::now();
    let min = |m: u64| Duration::from_secs(60 * m);
    let mut p = RescanPolicy::default();
    // The user was active 10 s ago: not yet.
    assert!(!p.allow("C:\\", t0 + min(1), t0 + min(1) - Duration::from_secs(10)));
    assert!(p.allow("C:\\", t0 + min(1), t0));
    assert!(!p.allow("C:\\", t0 + min(5), t0), "second rescan within ten minutes");
    assert!(p.allow("D:\\", t0 + min(5), t0), "another volume has its own clock");
    assert!(p.allow("C:\\", t0 + min(11), t0));
}

/// A tiny xorshift so the oracle below is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Clone)]
struct Truth {
    parent: u64,
    name: String,
    size: u64,
    is_dir: bool,
}

fn truth_snapshot(t: &BTreeMap<u64, Truth>) -> BTreeMap<String, (u64, bool)> {
    fn path(t: &BTreeMap<u64, Truth>, r: u64) -> String {
        if r == 5 { "C:".into() } else { format!("{}\\{}", path(t, t[&r].parent), t[&r].name) }
    }
    let mut out = BTreeMap::new();
    for (&r, n) in t {
        let size = if n.is_dir {
            t.iter().filter(|(c, v)| !v.is_dir && is_under(t, **c, r)).map(|(_, v)| v.size).sum()
        } else {
            n.size
        };
        out.insert(path(t, r), (size, n.is_dir));
    }
    out
}

fn is_under(t: &BTreeMap<u64, Truth>, mut r: u64, anc: u64) -> bool {
    while r != 5 {
        r = t[&r].parent;
        if r == anc {
            return true;
        }
    }
    anc == 5
}

#[test]
fn random_batches_in_any_order_match_a_tree_built_from_the_final_state() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut truth: BTreeMap<u64, Truth> = BTreeMap::new();
        truth.insert(5, Truth { parent: 5, name: "C:".into(), size: 0, is_dir: true });
        let mut next = 30u64;
        // Start from an empty drive, then grow it through batches like a busy volume.
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let mut records = vec![NodeId::NONE; 8];
        records[5] = root;
        let (mut tree, mut live) = (b.finish().unwrap(), Live::new(records));

        for _round in 0..30 {
            let mut touched: Vec<u64> = Vec::new();
            for _ in 0..1 + rng.below(12) {
                let keys: Vec<u64> = truth.keys().copied().filter(|&k| k != 5).collect();
                let dirs: Vec<u64> = truth.iter().filter(|(_, v)| v.is_dir).map(|(&k, _)| k).collect();
                match rng.below(6) {
                    0 | 1 => {
                        let parent = dirs[rng.below(dirs.len())];
                        let is_dir = rng.below(3) == 0;
                        truth.insert(next, Truth { parent, name: format!("n{next}"), size: if is_dir { 0 } else { 1 + rng.below(500) as u64 }, is_dir });
                        touched.push(next);
                        next += 1;
                    }
                    2 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        if !truth[&k].is_dir {
                            truth.get_mut(&k).unwrap().size = rng.below(900) as u64;
                            touched.push(k);
                        }
                    }
                    3 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        truth.get_mut(&k).unwrap().name = format!("r{}", rng.next() % 100_000);
                        touched.push(k);
                    }
                    4 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        let to = dirs[rng.below(dirs.len())];
                        if to != k && !is_under(&truth, to, k) {
                            truth.get_mut(&k).unwrap().parent = to;
                            touched.push(k);
                        }
                    }
                    5 if !keys.is_empty() => {
                        let k = keys[rng.below(keys.len())];
                        let gone: Vec<u64> = truth.keys().copied().filter(|&c| c == k || (c != 5 && is_under(&truth, c, k))).collect();
                        for c in gone {
                            truth.remove(&c);
                            touched.push(c);
                        }
                    }
                    _ => {}
                }
            }
            // What the reader reports: each touched record once, as it is now, in no useful order.
            touched.sort_unstable();
            touched.dedup();
            let mut deltas: Vec<Delta> = touched
                .iter()
                .map(|&r| match truth.get(&r) {
                    Some(n) => Delta::Upsert(st(r, n.parent, &n.name, n.size, n.is_dir)),
                    None => Delta::Remove { record: r },
                })
                .collect();
            for i in (1..deltas.len()).rev() {
                deltas.swap(i, rng.below(i + 1));
            }
            run(&mut live, &mut tree, deltas);
            run(&mut live, &mut tree, vec![]);
            assert!(!live.pending(), "seed {seed}");
            assert_eq!(snapshot(&tree), truth_snapshot(&truth), "seed {seed}");
        }
    }
}
