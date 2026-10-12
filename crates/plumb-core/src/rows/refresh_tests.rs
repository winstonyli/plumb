use super::*;
use crate::synth::{flat_tree, kary_builder};
use crate::tree::tests::sample;

fn ids(m: &RowModel) -> Vec<u32> {
    m.rows().iter().map(|r| r.id.0).collect()
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Expands every directory that has children, until nothing more expands.
fn expand_all(m: &mut RowModel, t: &Tree) {
    while let Some(id) = m.rows().iter().map(|r| r.id).find(|&id| m.is_expandable(t, id) && !m.is_expanded(id)) {
        m.expand(t, id);
    }
}

#[test]
fn adding_a_big_file_keeps_the_selected_row_on_screen_where_it_was() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let x = m.rows()[50].id;
    m.select(x);
    t.add_child(NodeId(0), &u("huge"), 1 << 40, NodeFlags::empty());
    let scroll = m.refresh(&mut t, 40); // selected was at screen row 10
    assert_eq!(m.rows()[1].id, NodeId(101), "the big file sorts first");
    assert_eq!(m.selected_index(), Some(51));
    assert_eq!(scroll, 41);
}

#[test]
fn a_removed_selection_falls_back_to_its_parent() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let x = m.rows()[50].id;
    m.select(x);
    t.remove(x);
    let scroll = m.refresh(&mut t, 40);
    assert_eq!(m.selected(), Some(NodeId(0)));
    assert_eq!(m.rows().len(), 100);
    assert_eq!(scroll, 40);
}

#[test]
fn a_selection_moved_under_a_collapsed_folder_falls_back_to_that_folder() {
    let mut t = sample(); // 0=C:, 1=big, 2=f1, 3=f2, 4=mid, 5=g, 6=small
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.select(NodeId(6));
    assert!(t.reparent(NodeId(6), NodeId(1)));
    m.refresh(&mut t, 0);
    assert_eq!(ids(&m), vec![0, 1, 4]);
    assert_eq!(m.selected(), Some(NodeId(1)));
}

#[test]
fn scroll_is_clamped_when_rows_disappear() {
    let mut t = flat_tree(100);
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    for id in 1..=90 {
        t.remove(NodeId(id));
    }
    let scroll = m.refresh(&mut t, 80);
    assert_eq!(m.rows().len(), 11);
    assert_eq!(scroll, 10);
}

#[test]
fn new_directories_expand_without_growing_the_bitset_by_hand() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    let d = t.add_child(NodeId(0), &u("d"), 0, NodeFlags::DIR);
    t.add_child(d, &u("x"), 5, NodeFlags::empty());
    assert!(!m.is_expanded(d), "ids past the bitset read as collapsed");
    m.refresh(&mut t, 0);
    m.expand(&t, d);
    assert!(m.is_expanded(d));
    assert!(ids(&m).contains(&8), "the new file shows under the new folder");
}

#[test]
fn a_directory_whose_children_all_died_is_not_expandable() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    t.remove(NodeId(5)); // mid's only child
    m.refresh(&mut t, 0);
    assert!(!m.is_expandable(&t, NodeId(4)));
    m.expand(&t, NodeId(4));
    assert!(!m.is_expanded(NodeId(4)));
}

/// After a pile of mutations, a refreshed model shows exactly what a model built from scratch
/// on the same tree shows: the sort caches were invalidated where, and only where, needed.
#[test]
fn refresh_matches_a_fresh_model_after_random_mutations() {
    for (key, desc) in [(SortKey::Size, true), (SortKey::Name, false), (SortKey::Items, true)] {
        let mut t = kary_builder(300, 3).finish().unwrap();
        let mut m = RowModel::new(&t);
        m.set_sort(&t, key, desc);
        expand_all(&mut m, &t);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for step in 0..600 {
            let live: Vec<NodeId> = (1..t.len() as u32)
                .map(NodeId)
                .filter(|&n| !t.flags[n].contains(NodeFlags::DEAD))
                .collect();
            let dirs: Vec<NodeId> = (0..t.len() as u32)
                .map(NodeId)
                .filter(|&n| t.flags[n].contains(NodeFlags::DIR) && !t.flags[n].contains(NodeFlags::DEAD))
                .collect();
            let files: Vec<NodeId> = live.iter().copied().filter(|&n| !t.flags[n].contains(NodeFlags::DIR)).collect();
            match rnd(4) {
                0 => {
                    let p = dirs[rnd(dirs.len())];
                    t.add_child(p, &u(&format!("a{step}")), rnd(5000) as u64, NodeFlags::empty());
                }
                1 if !files.is_empty() => t.set_size(files[rnd(files.len())], rnd(5000) as u64),
                2 if live.len() > 1 => {
                    t.remove(live[rnd(live.len())]);
                }
                _ if !live.is_empty() => {
                    t.reparent(live[rnd(live.len())], dirs[rnd(dirs.len())]);
                }
                _ => {}
            }
            if step % 20 == 19 {
                m.refresh(&mut t, 0);
                expand_all(&mut m, &t);
                let mut fresh = RowModel::new(&t);
                fresh.set_sort(&t, key, desc);
                expand_all(&mut fresh, &t);
                assert_eq!(ids(&m), ids(&fresh), "{key:?} after step {step}");
            }
        }
    }
}
