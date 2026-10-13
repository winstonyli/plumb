use super::*;
use plumb_scan::journal::state::FileState;
use plumb_scan::preview::{channel, PreviewMsg};
use std::collections::BTreeMap;
use std::time::Duration;

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn state(record: u64, parent: u64, name: &str, size: u64, is_dir: bool) -> FileState {
    FileState { record, sequence: 1, parent, name: name.encode_utf16().collect(), size, is_dir, reparse: false, links: 1 }
}

/// path -> (size, is_dir) for every node but the root's own name.
fn listing(t: &Tree) -> BTreeMap<String, (u64, bool)> {
    let root = t.roots[0];
    let mut out = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.size[n], t.flags[n].contains(NodeFlags::DIR)));
        stack.extend(t.children(n));
    }
    out
}

/// A deterministic pseudo-random sequence (no dependency).
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

#[test]
fn the_walker_replay_equals_the_builder_tree() {
    let (tx, rx) = channel(1 << 16);
    let mut b = TreeBuilder::new();
    let mut seed = 7;
    let push = |b: &mut TreeBuilder, parent: NodeId, name: &str, size: u64, flags: NodeFlags| {
        let units: Vec<u16> = name.encode_utf16().collect();
        tx.send(PreviewMsg::Add { parent, name: units.clone(), size, flags });
        b.push(parent, &units, size, flags)
    };
    let root = push(&mut b, NodeId::NONE, "C:", 0, NodeFlags::DIR);
    let mut dirs = vec![root];
    for i in 0..500u32 {
        let parent = dirs[lcg(&mut seed) as usize % dirs.len()];
        if lcg(&mut seed).is_multiple_of(4) {
            dirs.push(push(&mut b, parent, &format!("d{i}"), 0, NodeFlags::DIR));
        } else {
            push(&mut b, parent, &format!("f{i}"), lcg(&mut seed) % 5000, NodeFlags::empty());
        }
    }
    let mut p = Preview::new(rx);
    let mut tree = None;
    let a = p.apply(&mut tree, far());
    assert!(a.created);
    let (got, want) = (tree.unwrap(), b.finish().unwrap());
    assert_eq!(got.len(), want.len());
    for i in 0..want.len() as u32 {
        let id = NodeId(i);
        assert_eq!((got.parent[id], got.name(id), got.size[id], got.flags[id]), (want.parent[id], want.name(id), want.size[id], want.flags[id]));
    }
    assert_eq!(got.subtree_size[NodeId(0)], want.subtree_size[NodeId(0)]);
}

#[test]
fn mft_records_in_any_order_build_the_same_tree() {
    // 40 directories and 200 files under record 5, with parents of every record before or after it.
    let mut seed = 99;
    let mut states = Vec::new();
    let mut dirs = vec![5u64];
    for r in 6..246u64 {
        let parent = dirs[lcg(&mut seed) as usize % dirs.len()];
        let is_dir = r < 46;
        states.push(state(r, parent, &format!("n{r}"), if is_dir { 0 } else { lcg(&mut seed) % 9000 }, is_dir));
        if is_dir {
            dirs.push(r);
        }
    }
    let mut results = Vec::new();
    for round in 0..4 {
        let mut order = states.clone();
        for i in (1..order.len()).rev() {
            order.swap(i, lcg(&mut seed) as usize % (i + 1));
        }
        if round == 0 {
            order.sort_by_key(|s| s.record);
        }
        let (tx, rx) = channel(1 << 12);
        tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 300 });
        for s in order {
            tx.send(PreviewMsg::Upsert(s));
        }
        let (mut p, mut tree) = (Preview::new(rx), None);
        p.apply(&mut tree, far());
        let t = tree.unwrap();
        assert_eq!(t.len(), 241, "round {round}: every record placed");
        results.push(listing(&t));
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(results[0].len(), 241);
}

#[test]
fn a_parent_that_is_a_file_keeps_its_child_parked() {
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "file", 10, false)));
    tx.send(PreviewMsg::Upsert(state(7, 6, "child", 10, false)));
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 2, "root and the file; the child waits");
}

#[test]
fn an_extension_size_applies_whether_it_arrives_before_or_after_the_base() {
    for before in [true, false] {
        let (tx, rx) = channel(64);
        tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
        if before {
            tx.send(PreviewMsg::Size { record: 6, size: 4096 });
        }
        tx.send(PreviewMsg::Upsert(state(6, 5, "big", 0, false)));
        if !before {
            tx.send(PreviewMsg::Size { record: 6, size: 4096 });
        }
        let (mut p, mut tree) = (Preview::new(rx), None);
        p.apply(&mut tree, far());
        let t = tree.unwrap();
        assert_eq!(t.subtree_size[NodeId(0)], 4096, "before = {before}");
    }
}

#[test]
fn reset_drops_the_tree_and_a_new_scanner_starts_clean() {
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    tx.send(PreviewMsg::Reset);
    tx.send(PreviewMsg::Add { parent: NodeId::NONE, name: "C:".encode_utf16().collect(), size: 0, flags: NodeFlags::DIR });
    let (mut p, mut tree) = (Preview::new(rx), None);
    let a = p.apply(&mut tree, far());
    assert!(a.created && a.reset, "the old tree was dropped and the walker's root created");
    assert_eq!(tree.unwrap().len(), 1);
}

#[test]
fn a_reset_unfreezes_a_frozen_preview() {
    // A file parent keeps the child parked; MAX_PARKED is too big to hit, so freeze by an
    // out-of-step walker id instead.
    let (tx, rx) = channel(64);
    tx.send(PreviewMsg::Add { parent: NodeId::NONE, name: "C:".encode_utf16().collect(), size: 0, flags: NodeFlags::DIR });
    tx.send(PreviewMsg::Add { parent: NodeId(9), name: "x".encode_utf16().collect(), size: 1, flags: NodeFlags::empty() });
    tx.send(PreviewMsg::Add { parent: NodeId(0), name: "after".encode_utf16().collect(), size: 1, flags: NodeFlags::empty() });
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, far());
    assert_eq!(tree.as_ref().unwrap().len(), 1, "frozen at the bad id; the later add is ignored");
    assert!(p.paused());
    tx.send(PreviewMsg::Reset);
    tx.send(PreviewMsg::Begin { root: "D:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 2);
    assert!(!p.paused());
}

#[test]
fn a_lagged_sender_pauses_the_preview_after_the_queue_drains() {
    let (tx, rx) = channel(2);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10 });
    tx.send(PreviewMsg::Upsert(state(6, 5, "a", 1, false)));
    tx.send(PreviewMsg::Upsert(state(7, 5, "b", 1, false))); // dropped
    let (mut p, mut tree) = (Preview::new(rx), None);
    assert!(!p.paused());
    p.apply(&mut tree, far());
    assert!(p.paused());
    assert_eq!(tree.unwrap().len(), 2);
}

#[test]
fn the_time_budget_stops_a_long_batch() {
    let (tx, rx) = channel(1 << 14);
    tx.send(PreviewMsg::Begin { root: "C:".into(), nodes: 10_000 });
    for r in 6..6 + 5000u64 {
        tx.send(PreviewMsg::Upsert(state(r, 5, "x", 1, false)));
    }
    let (mut p, mut tree) = (Preview::new(rx), None);
    p.apply(&mut tree, Instant::now()); // already past: stops at the first clock look
    let first = tree.as_ref().unwrap().len();
    assert!(first < 5001, "stopped early, had {first}");
    p.apply(&mut tree, far());
    assert_eq!(tree.unwrap().len(), 5001);
}
