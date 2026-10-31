use super::*;
use plumb_core::synth::u;
use plumb_core::TreeBuilder;
use plumb_scan::listing::Entry;
use std::collections::BTreeMap;
use std::time::Duration;

fn file(name: &str, alloc: u64) -> Entry {
    Entry { name: u(name), alloc, is_dir: false, reparse: false, id: 0, mtime: None }
}

fn dir(name: &str) -> Entry {
    Entry { name: u(name), alloc: 0, is_dir: true, reparse: false, id: 0, mtime: None }
}

fn listing(d: &str, entries: Vec<Entry>) -> Listing {
    Listing { dir: u(d), entries: Ok(entries) }
}

/// R/{a/{b.txt 4096}, c.txt 8192}
fn tree() -> Tree {
    let mut b = TreeBuilder::new();
    let r = b.push_str(NodeId::NONE, "R", 0, NodeFlags::DIR);
    let a = b.push_str(r, "a", 0, NodeFlags::DIR);
    b.push_str(a, "b.txt", 4096, NodeFlags::empty());
    b.push_str(r, "c.txt", 8192, NodeFlags::empty());
    b.finish().unwrap()
}

/// path -> (subtree size, subtree files) for every live node.
fn snap(t: &Tree) -> BTreeMap<String, (u64, u32)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![t.roots[0]];
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.subtree_size[n], t.subtree_files[n]));
        stack.extend(t.children(n));
    }
    out
}

fn run(t: &mut Tree, ls: Vec<Listing>) -> usize {
    let mut q: VecDeque<Listing> = ls.into();
    apply(t, &mut q, Instant::now() + Duration::from_secs(60))
}

#[test]
fn adds_removes_and_resizes_match_a_tree_built_from_the_final_state() {
    let mut t = tree();
    run(&mut t, vec![listing("", vec![dir("a"), file("c.txt", 16384), file("d.txt", 100)]), listing("a", vec![])]);
    let mut b = TreeBuilder::new();
    let r = b.push_str(NodeId::NONE, "R", 0, NodeFlags::DIR);
    b.push_str(r, "a", 0, NodeFlags::DIR);
    b.push_str(r, "c.txt", 16384, NodeFlags::empty());
    b.push_str(r, "d.txt", 100, NodeFlags::empty());
    assert_eq!(snap(&t), snap(&b.finish().unwrap()));
}

#[test]
fn a_hard_link_counted_as_zero_stays_zero_when_its_folder_is_listed_again() {
    let mut b = TreeBuilder::new();
    let r = b.push_str(NodeId::NONE, "R", 0, NodeFlags::DIR);
    b.push_str(r, "first", 8192, NodeFlags::empty());
    b.push_str(r, "second", 0, NodeFlags::HARDLINK);
    let mut t = b.finish().unwrap();
    run(&mut t, vec![listing("", vec![file("first", 8192), file("second", 8192)])]);
    assert_eq!(snap(&t)["R"].0, 8192, "the file is still counted once");
}

#[test]
fn a_file_that_became_a_directory_is_replaced() {
    let mut t = tree();
    run(&mut t, vec![listing("", vec![dir("a"), dir("c.txt")])]);
    let s = snap(&t);
    assert_eq!(s["R\\c.txt"], (0, 0));
    assert_eq!(s["R"].0, 4096, "b.txt is still under a until a is listed");
}

#[test]
fn a_new_directory_fills_in_from_nested_listings_in_one_batch() {
    let mut t = tree();
    run(
        &mut t,
        vec![
            listing("", vec![dir("a"), file("c.txt", 8192), dir("n")]),
            listing("n", vec![dir("m")]),
            listing("n\\m", vec![file("f", 512)]),
        ],
    );
    let s = snap(&t);
    assert_eq!(s["R\\n\\m\\f"], (512, 1));
    assert_eq!(s["R"], (4096 + 8192 + 512, 3));
}

#[test]
fn applying_a_batch_twice_changes_nothing_the_second_time() {
    let mut t = tree();
    let batch = || vec![listing("", vec![dir("a"), file("c.txt", 1), file("e", 7)])];
    assert!(run(&mut t, batch()) > 0);
    let before = snap(&t);
    assert_eq!(run(&mut t, batch()), 0);
    assert_eq!(snap(&t), before);
}

#[test]
fn unresolvable_directories_and_failed_listings_are_skipped() {
    let mut t = tree();
    let before = snap(&t);
    let n = run(&mut t, vec![listing("x\\y", vec![file("z", 1)]), Listing { dir: u("a"), entries: Err(()) }]);
    assert_eq!(n, 0);
    assert_eq!(snap(&t), before);
}

#[test]
fn names_are_matched_without_regard_to_case() {
    let mut t = tree();
    assert_eq!(run(&mut t, vec![listing("", vec![dir("A"), file("C.TXT", 8192)]), listing("A", vec![file("B.txt", 4096)])]), 0);
}

#[test]
fn the_deadline_stops_after_the_first_listing_and_keeps_the_rest_queued() {
    let mut t = tree();
    let mut q: VecDeque<Listing> = vec![listing("", vec![dir("a"), file("c.txt", 1)]), listing("a", vec![])].into();
    let n = apply(&mut t, &mut q, Instant::now());
    assert!(n > 0);
    assert_eq!(q.len(), 1);
}

#[test]
fn the_last_write_day_follows_an_entry_even_when_its_size_does_not_change() {
    let day = std::num::NonZeroU32::new;
    let mut t = tree();
    let c = node_at(&t, r"R\c.txt");
    let mut same = file("c.txt", 8192);
    same.mtime = day(5);
    assert_ne!(run(&mut t, vec![listing("", vec![dir("a"), same.clone()]), listing("a", vec![file("b.txt", 4096)])]), 0);
    assert_eq!(t.mtime[c], day(5));
    assert_eq!(run(&mut t, vec![listing("", vec![dir("a"), same]), listing("a", vec![file("b.txt", 4096)])]), 0);
    let mut fresh = file("d.txt", 1);
    fresh.mtime = day(6);
    run(&mut t, vec![listing("", vec![dir("a"), file("c.txt", 8192), fresh]), listing("a", vec![file("b.txt", 4096)])]);
    assert_eq!(t.mtime[node_at(&t, r"R\d.txt")], day(6));
}

fn node_at(t: &Tree, path: &str) -> NodeId {
    (0..t.len() as u32).map(NodeId).find(|&i| t.path(i) == path).unwrap_or_else(|| panic!("no {path}"))
}
