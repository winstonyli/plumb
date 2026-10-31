use super::*;
use crate::synth::u;
use crate::filter::{Hits, Names, Query};
use crate::synth::kary_builder;
use crate::tree::tests::sample;
use std::collections::HashSet;
use std::sync::atomic::AtomicU64;

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
fn ids(m: &RowModel) -> Vec<u32> {
    m.rows().iter().map(|r| r.id.0).collect()
}

fn hits(t: &Tree, text: &str) -> (crate::filter::Matcher, Hits) {
    let m = Query { text: text.into(), regex: false }.compile().unwrap().unwrap();
    let h = Names::of(t).scan(&m, &AtomicU64::new(1), 1).unwrap();
    (m, h)
}

fn keep(t: &Tree, text: &str) -> Rc<Keep> {
    Rc::new(Keep::build(t, &hits(t, text).1))
}

#[test]
fn a_filter_shows_matches_and_the_folders_on_the_way_open() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "f")));
    assert_eq!(ids(&m), vec![0, 1, 3, 2]); // C:, big, f2 (300), f1 (100)
    assert!(m.rows().iter().all(|r| !r.dim));
    assert_eq!(m.rows()[2].depth, 2);
}

#[test]
fn a_leaf_match_is_collapsed_and_opens_to_dimmed_children() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "mid")));
    assert_eq!(ids(&m), vec![0, 4]);
    m.expand(&t, NodeId(4));
    assert_eq!(ids(&m), vec![0, 4, 5]);
    assert!(m.rows()[2].dim && !m.rows()[1].dim);
}

#[test]
fn clearing_the_filter_restores_the_users_own_expansion() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    let before = ids(&m);
    m.set_filter(&t, Some(keep(&t, "small")));
    assert_eq!(ids(&m), vec![0, 6]);
    m.set_filter(&t, None);
    assert_eq!(ids(&m), before);
}

#[test]
fn collapsing_an_ancestor_under_a_filter_does_not_touch_the_users_expansion() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "f")));
    m.collapse(NodeId(1));
    assert_eq!(ids(&m), vec![0, 1]);
    m.expand(&t, NodeId(1));
    assert_eq!(ids(&m), vec![0, 1, 3, 2]);
    m.collapse(NodeId(1));
    m.set_filter(&t, None);
    assert_eq!(ids(&m), vec![0], "the user never expanded anything");
}

#[test]
fn a_selection_the_filter_hides_moves_to_its_nearest_visible_ancestor() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.select(NodeId(6));
    m.set_filter(&t, Some(keep(&t, "f")));
    assert_eq!(m.selected(), Some(NodeId(0)));
    m.select(NodeId(3));
    m.set_filter(&t, Some(keep(&t, "f2")));
    assert_eq!(m.selected(), Some(NodeId(3)));
}

#[test]
fn sort_changes_apply_under_a_filter() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "f")));
    m.set_sort(&t, SortKey::Name, false);
    assert_eq!(ids(&m), vec![0, 1, 2, 3]); // f1 before f2
}

/// Independent oracle: walk the whole tree, keep matches and their ancestors, descend only through
/// ancestors, children by size descending then id.
fn oracle(t: &Tree, text: &str) -> Vec<u32> {
    let needle = text.to_lowercase();
    let live = |n: NodeId| !t.flags[n].contains(NodeFlags::DEAD);
    let hit = |n: NodeId| live(n) && t.name_string(n).to_lowercase().contains(&needle);
    let mut anc: HashSet<NodeId> = HashSet::new();
    for i in 0..t.len() as u32 {
        if hit(NodeId(i)) {
            let mut p = t.parent[NodeId(i)];
            while !p.is_none() {
                anc.insert(p);
                p = t.parent[p];
            }
        }
    }
    let kept = |n: NodeId| hit(n) || anc.contains(&n);
    fn walk(t: &Tree, kids: Vec<NodeId>, kept: &dyn Fn(NodeId) -> bool, anc: &HashSet<NodeId>, out: &mut Vec<u32>) {
        let mut kids: Vec<NodeId> = kids.into_iter().filter(|&k| kept(k)).collect();
        kids.sort_by_key(|&k| (std::cmp::Reverse(t.subtree_size[k]), k));
        for k in kids {
            out.push(k.0);
            if anc.contains(&k) {
                walk(t, t.children(k).collect(), kept, anc, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(t, t.roots.clone(), &kept, &anc, &mut out);
    out
}

#[test]
fn filtered_rows_match_the_oracle_on_a_generated_tree() {
    let t = kary_builder(300, 3).finish().unwrap();
    for q in ["n1", "n29", "7", "N2", "zzz", "n"] {
        let mut m = RowModel::new(&t);
        m.set_filter(&t, Some(keep(&t, q)));
        assert_eq!(ids(&m), oracle(&t, q), "query {q:?}");
    }
}

#[test]
fn filtered_rows_follow_live_changes_like_a_fresh_filter() {
    let mut t = sample();
    let (matcher, mut h) = hits(&t, "f");
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(Rc::new(Keep::build(&t, &h))));
    t.add_child(NodeId(4), &u("ff"), 5, NodeFlags::empty());
    t.remove(NodeId(2));
    h.update(&t, &matcher, t.touched());
    m.set_filter(&t, Some(Rc::new(Keep::build(&t, &h))));
    m.refresh(&mut t, 0);
    assert_eq!(ids(&m), oracle(&t, "f"));
    assert_eq!(ids(&m), vec![0, 1, 3, 4, 7]); // big (300), mid (155) with ff
}

#[test]
fn update_keep_then_refresh_equals_a_fresh_filter() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.set_filter(&t, Some(keep(&t, "f")));
    // a new match under `mid` (4): its folder and C: must now show it
    let new = t.add_child(NodeId(4), &u("fresh"), 10, crate::NodeFlags::empty());
    let k = keep(&t, "f");
    m.update_keep(k.clone());
    m.refresh(&mut t, 0);
    let mut fresh = RowModel::new(&t);
    fresh.set_filter(&t, Some(k));
    assert_eq!(ids(&m), ids(&fresh));
    assert!(ids(&m).contains(&new.0));
}
