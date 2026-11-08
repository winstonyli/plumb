use super::*;
use crate::tree::tests::sample;

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
// With C: open the rows are 0, 1, 4, 6 (largest first); with big open as well: 0, 1, 3, 2, 4, 6.

fn open() -> (Tree, RowModel) {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    (t, m)
}

fn n(ids: &[u32]) -> Vec<NodeId> {
    ids.iter().map(|&i| NodeId(i)).collect()
}

#[test]
fn select_picks_only_that_row_and_makes_it_the_focus() {
    let (_, mut m) = open();
    m.select(NodeId(4));
    assert_eq!((m.targets(), m.picked_count(), m.selected()), (n(&[4]), 1, Some(NodeId(4))));
    m.select(NodeId(6));
    assert_eq!(m.targets(), n(&[6]));
    assert!(!m.is_picked(NodeId(4)));
}

#[test]
fn extend_picks_the_rows_between_the_anchor_and_the_target_in_either_direction() {
    let (_, mut m) = open();
    m.select(NodeId(4));
    m.extend_to(NodeId(0));
    assert_eq!(m.targets(), n(&[0, 1, 4]));
    assert_eq!(m.selected(), Some(NodeId(0)));
    m.extend_to(NodeId(6));
    assert_eq!(m.targets(), n(&[4, 6]), "the anchor stayed on 4");
}

#[test]
fn extend_with_nothing_selected_picks_just_the_target() {
    let (_, mut m) = open();
    m.extend_to(NodeId(4));
    assert_eq!((m.targets(), m.selected()), (n(&[4]), Some(NodeId(4))));
}

#[test]
fn a_stale_anchor_is_replaced_by_the_focus_and_stays_replaced() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(2));
    m.extend_to(NodeId(6)); // 2, 4, 6 picked; focus 6; anchor 2
    m.collapse(NodeId(1)); // 3 and 2 leave the list
    assert_eq!(m.targets(), n(&[4, 6]));
    m.extend_to(NodeId(4)); // anchor 2 has no row: the focus (6) stands in
    assert_eq!((m.targets(), m.selected()), (n(&[4, 6]), Some(NodeId(4))));
    m.extend_to(NodeId(1)); // and it is the anchor now
    assert_eq!(m.targets(), n(&[1, 4, 6]));
}

#[test]
fn toggle_flips_one_row_and_the_last_unpick_leaves_an_empty_set() {
    let (_, mut m) = open();
    m.select(NodeId(1));
    m.toggle_pick(NodeId(4));
    assert_eq!((m.targets(), m.selected()), (n(&[1, 4]), Some(NodeId(4))));
    m.toggle_pick(NodeId(4));
    assert_eq!(m.targets(), n(&[1]));
    m.toggle_pick(NodeId(1));
    assert_eq!((m.targets(), m.picked_count(), m.selected()), (vec![], 0, Some(NodeId(1))));
    m.toggle_pick(NodeId(6));
    assert_eq!(m.targets(), n(&[6]), "from nothing picked");
}

#[test]
fn select_all_picks_every_row_and_keeps_the_focus() {
    let (_, mut m) = open();
    m.select_all();
    assert_eq!((m.targets(), m.selected()), (n(&[0, 1, 4, 6]), Some(NodeId(0))), "no focus: the first row");
    m.select(NodeId(4));
    m.select_all();
    assert_eq!((m.picked_count(), m.selected()), (4, Some(NodeId(4))));
}

#[test]
fn collapsing_drops_the_picks_it_hides_and_the_count_stays_exact() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(0));
    m.extend_to(NodeId(6));
    assert_eq!(m.picked_count(), 6);
    m.collapse(NodeId(1));
    assert_eq!((m.picked_count(), m.targets()), (4, n(&[0, 1, 4, 6])));
    assert!(!m.is_picked(NodeId(3)));
}

#[test]
fn a_refresh_that_removes_rows_prunes_the_set_and_a_lost_focus_takes_it_to_its_parent() {
    let mut t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.select(NodeId(1));
    m.extend_to(NodeId(6)); // 1, 4, 6; focus 6
    t.remove(NodeId(4));
    m.refresh(&mut t, 0);
    assert_eq!((m.targets(), m.picked_count()), (n(&[1, 6]), 2), "the removed row left the set");
    t.remove(NodeId(6));
    m.refresh(&mut t, 0);
    assert_eq!((m.selected(), m.targets()), (Some(NodeId(0)), n(&[0])), "the focus fell back to its parent and the set followed");
}

#[test]
fn shift_keys_extend_and_left_right_end_the_set() {
    let (t, mut m) = open();
    m.select(NodeId(0));
    m.handle_key(&t, Key::Down, 10, true);
    m.handle_key(&t, Key::Down, 10, true);
    assert_eq!((m.targets(), m.selected()), (n(&[0, 1, 4]), Some(NodeId(4))));
    m.handle_key(&t, Key::Up, 10, true);
    assert_eq!(m.targets(), n(&[0, 1]));
    m.handle_key(&t, Key::End, 10, true);
    assert_eq!(m.targets(), n(&[0, 1, 4, 6]));
    m.handle_key(&t, Key::Home, 10, true);
    assert_eq!(m.targets(), n(&[0]));
    m.handle_key(&t, Key::PageDown, 2, true);
    assert_eq!(m.targets(), n(&[0, 1, 4]));
    m.handle_key(&t, Key::PageUp, 2, true);
    assert_eq!(m.targets(), n(&[0]));
    m.handle_key(&t, Key::End, 10, true);
    m.handle_key(&t, Key::Left, 10, true);
    assert_eq!(m.picked_count(), 1, "Left is a plain move");
}

#[test]
fn a_plain_key_move_picks_just_the_new_focus() {
    let (t, mut m) = open();
    m.select(NodeId(0));
    m.handle_key(&t, Key::Down, 10, true);
    m.handle_key(&t, Key::Down, 10, false);
    assert_eq!((m.targets(), m.selected()), (n(&[4]), Some(NodeId(4))));
}

#[test]
fn select_parent_moves_to_the_parent_row_and_stops_at_a_root() {
    let t = sample();
    let mut m = RowModel::new(&t);
    m.expand(&t, NodeId(0));
    m.expand(&t, NodeId(1));
    m.select(NodeId(3));
    m.select_parent(&t);
    assert_eq!((m.selected(), m.targets()), (Some(NodeId(1)), n(&[1])));
    m.select_parent(&t);
    m.select_parent(&t);
    assert_eq!(m.selected(), Some(NodeId(0)), "a root has no parent");
}

#[test]
fn the_count_matches_the_picked_rows_after_any_mix_of_operations() {
    let t = crate::synth::kary_builder(60, 3).finish().unwrap();
    let mut m = RowModel::new(&t);
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for _ in 0..600 {
        let rows = m.rows().to_vec();
        let pick = |r: u64| rows[r as usize % rows.len()].id;
        match rnd(7) {
            0 if rnd(3) == 0 => m.select(NodeId(rnd(t.len() as u64) as u32)),
            1 if rnd(3) == 0 => m.toggle_pick(NodeId(rnd(t.len() as u64) as u32)),
            0 => m.select(pick(rnd(1000))),
            1 => m.toggle_pick(pick(rnd(1000))),
            2 => m.extend_to(pick(rnd(1000))),
            3 => m.select_all(),
            4 => m.collapse(pick(rnd(1000))),
            5 => m.expand(&t, pick(rnd(1000))),
            _ => m.handle_key(&t, [Key::Up, Key::Down, Key::PageDown, Key::Home, Key::End][rnd(5) as usize], 5, rnd(2) == 0),
        }
        let want: Vec<NodeId> = m.rows().iter().map(|r| r.id).filter(|&id| m.is_picked(id)).collect();
        assert_eq!(m.targets(), want);
        assert_eq!(m.picked_count(), want.len(), "a pick without a row, or a miscounted one");
    }
}

#[test]
fn select_and_toggle_on_a_row_less_node_move_the_focus_but_pick_nothing() {
    let (_, mut m) = open(); // 2 (f1) is under the collapsed `big`: no row
    m.select(NodeId(4));
    m.select(NodeId(2));
    assert_eq!((m.picked_count(), m.targets(), m.selected()), (0, vec![], Some(NodeId(2))));
    m.select(NodeId(4));
    m.toggle_pick(NodeId(2));
    assert_eq!((m.picked_count(), m.targets(), m.selected()), (1, n(&[4]), Some(NodeId(2))));
}
