use super::tests::sample;
use super::*;
use std::collections::BTreeMap;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn kids(t: &Tree, id: u32) -> Vec<u32> {
    t.children(NodeId(id)).map(|c| c.0).collect()
}

/// Path -> (subtree_size, subtree_files) for every live node.
fn summary(t: &Tree) -> BTreeMap<String, (u64, u32)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<NodeId> = t.roots.clone();
    while let Some(n) = stack.pop() {
        out.insert(t.path(n), (t.subtree_size[n], t.subtree_files[n]));
        stack.extend(t.children(n));
    }
    out
}

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)

#[test]
fn add_child_links_at_the_head_and_updates_every_ancestor() {
    let mut t = sample();
    let h = t.add_child(NodeId(4), &u("h"), 25, NodeFlags::empty());
    assert_eq!(h, NodeId(7));
    assert_eq!(kids(&t, 4), vec![7, 5]);
    assert_eq!((t.subtree_size[NodeId(4)], t.subtree_files[NodeId(4)]), (175, 2));
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (625, 5));
    assert_eq!(t.name_string(h), "h");
    assert_eq!(t.take_touched(), vec![h]);
    assert!(t.take_touched().is_empty());
}

#[test]
fn a_new_directory_adds_no_files_until_it_gets_some() {
    let mut t = sample();
    let d = t.add_child(NodeId(0), &u("d"), 0, NodeFlags::DIR);
    assert_eq!(t.subtree_files[NodeId(0)], 4);
    t.add_child(d, &u("x"), 10, NodeFlags::empty());
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (610, 5));
}

#[test]
fn set_size_goes_up_and_down() {
    let mut t = sample();
    t.set_size(NodeId(3), 500);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(0)]), (600, 800));
    t.set_size(NodeId(3), 100);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(0)]), (200, 400));
    assert_eq!(t.size[NodeId(3)], 100);
}

#[test]
fn remove_takes_the_subtree_out_and_children_skip_it() {
    let mut t = sample();
    assert!(t.remove(NodeId(1)));
    assert_eq!((t.subtree_size[NodeId(0)], t.subtree_files[NodeId(0)]), (200, 2));
    assert_eq!(kids(&t, 0), vec![4, 6]);
    assert_eq!(t.dead(), 3);
    assert!(t.flags[NodeId(2)].contains(NodeFlags::DEAD));
    assert!(!t.remove(NodeId(1)), "already dead");
    assert!(!t.remove(NodeId(0)), "roots cannot be removed");
    t.set_size(NodeId(2), 999);
    assert_eq!(t.subtree_size[NodeId(0)], 200, "dead nodes ignore set_size");
    assert!(!t.has_children(NodeId(2)));
}

#[test]
fn rename_changes_the_name_only() {
    let mut t = sample();
    t.rename(NodeId(6), &u("zzz"));
    assert_eq!(t.name_string(NodeId(6)), "zzz");
    assert_eq!(t.path(NodeId(6)), "C:\\zzz");
    assert_eq!(t.subtree_size[NodeId(0)], 600);
}

#[test]
fn reparent_moves_the_subtree_and_both_sides_add_up() {
    let mut t = sample();
    assert!(t.reparent(NodeId(2), NodeId(4))); // f1 (100) from big to mid
    assert_eq!(kids(&t, 1), vec![3]);
    assert_eq!(kids(&t, 4), vec![2, 5]);
    assert_eq!((t.subtree_size[NodeId(1)], t.subtree_size[NodeId(4)]), (300, 250));
    assert_eq!(t.subtree_size[NodeId(0)], 600);
    assert_eq!(t.path(NodeId(2)), "C:\\mid\\f1");
    // a middle child: children(0) is [1, 4, 6]; move 4 under 1
    assert!(t.reparent(NodeId(4), NodeId(1)));
    assert_eq!(kids(&t, 0), vec![1, 6]);
    assert_eq!(t.subtree_size[NodeId(1)], 550);
    assert_eq!(t.subtree_size[NodeId(0)], 600);
}

#[test]
fn reparent_refuses_cycles_roots_and_dead_nodes() {
    let mut t = sample();
    assert!(!t.reparent(NodeId(1), NodeId(1)));
    assert!(!t.reparent(NodeId(1), NodeId(2)), "into its own subtree");
    assert!(!t.reparent(NodeId(0), NodeId(1)), "roots stay roots");
    t.remove(NodeId(4));
    t.take_touched();
    assert!(!t.reparent(NodeId(6), NodeId(4)), "into a dead node");
    assert!(!t.reparent(NodeId(5), NodeId(0)), "a dead node");
    assert!(t.reparent(NodeId(6), NodeId(0)), "same parent is a no-op that succeeds");
    assert!(t.take_touched().is_empty(), "refusals and no-ops touch nothing");
}

struct Oracle {
    parent: Vec<Option<usize>>,
    name: Vec<String>,
    size: Vec<u64>,
    dir: Vec<bool>,
    alive: Vec<bool>,
}

impl Oracle {
    fn under(&self, mut i: usize, anc: usize) -> bool {
        loop {
            if i == anc {
                return true;
            }
            match self.parent[i] {
                Some(p) => i = p,
                None => return false,
            }
        }
    }

    fn live(&self, pred: impl Fn(usize) -> bool) -> Vec<usize> {
        (0..self.alive.len()).filter(|&i| self.alive[i] && pred(i)).collect()
    }

    /// The tree the live nodes would have if it had been scanned fresh.
    fn fresh(&self) -> Tree {
        let mut map = vec![NodeId::NONE; self.alive.len()];
        let mut next = 0;
        for i in 0..map.len() {
            if self.alive[i] {
                map[i] = NodeId(next);
                next += 1;
            }
        }
        let mut b = TreeBuilder::new();
        for i in self.live(|_| true) {
            let p = self.parent[i].map_or(NodeId::NONE, |p| map[p]);
            let flags = if self.dir[i] { NodeFlags::DIR } else { NodeFlags::empty() };
            b.push_str(p, &self.name[i], self.size[i], flags);
        }
        b.finish().unwrap()
    }
}

#[test]
fn random_mutations_match_a_fresh_build() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let mut b = TreeBuilder::new();
    b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
    let mut t = b.finish().unwrap();
    let mut o = Oracle {
        parent: vec![None],
        name: vec!["r".into()],
        size: vec![0],
        dir: vec![true],
        alive: vec![true],
    };
    for step in 0..3000 {
        let pick = rnd(100);
        if pick < 35 {
            let dirs = o.live(|i| o.dir[i]);
            let p = dirs[rnd(dirs.len())];
            let is_dir = rnd(4) == 0;
            let size = if is_dir { 0 } else { rnd(1000) as u64 };
            let name = format!("n{step}");
            let flags = if is_dir { NodeFlags::DIR } else { NodeFlags::empty() };
            let id = t.add_child(NodeId(p as u32), &u(&name), size, flags);
            assert_eq!(id.idx(), o.alive.len());
            o.parent.push(Some(p));
            o.name.push(name);
            o.size.push(size);
            o.dir.push(is_dir);
            o.alive.push(true);
        } else if pick < 60 {
            let files = o.live(|i| !o.dir[i]);
            if !files.is_empty() {
                let i = files[rnd(files.len())];
                let size = rnd(1000) as u64;
                t.set_size(NodeId(i as u32), size);
                o.size[i] = size;
            }
        } else if pick < 70 {
            let cand = o.live(|i| i != 0);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                let name = format!("m{step}");
                t.rename(NodeId(i as u32), &u(&name));
                o.name[i] = name;
            }
        } else if pick < 85 {
            let cand = o.live(|i| i != 0);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                assert!(t.remove(NodeId(i as u32)));
                for j in 0..o.alive.len() {
                    if o.alive[j] && o.under(j, i) {
                        o.alive[j] = false;
                    }
                }
            }
        } else {
            let cand = o.live(|i| i != 0);
            let dirs = o.live(|i| o.dir[i]);
            if !cand.is_empty() {
                let i = cand[rnd(cand.len())];
                let y = dirs[rnd(dirs.len())];
                let legal = !o.under(y, i);
                assert_eq!(t.reparent(NodeId(i as u32), NodeId(y as u32)), legal, "step {step}");
                if legal {
                    o.parent[i] = Some(y);
                }
            }
        }
        if step % 250 == 0 || step == 2999 {
            assert_eq!(summary(&t), summary(&o.fresh()), "after step {step}");
        }
    }
    assert_eq!(t.dead(), o.alive.iter().filter(|a| !**a).count());
}

#[test]
fn reserve_prevents_reallocation_while_growing() {
    let mut t = sample();
    t.reserve(1000, 5000);
    let (p, n) = (t.parent.as_ptr(), t.pool.as_ptr());
    for i in 0..1000 {
        t.add_child(NodeId(0), &u(&format!("n{i}")), 1, NodeFlags::empty());
    }
    assert_eq!((t.parent.as_ptr(), t.pool.as_ptr()), (p, n));
}

#[test]
fn mtime_starts_unknown_is_set_and_survives_add_child() {
    use std::num::NonZeroU32;
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
    let f = b.push_str(root, "f", 5, NodeFlags::empty());
    b.set_mtime(f, NonZeroU32::new(9));
    let mut t = b.finish().unwrap();
    assert_eq!((t.mtime[root], t.mtime[f]), (None, NonZeroU32::new(9)));
    let g = t.add_child(root, &[b'g' as u16], 1, NodeFlags::empty());
    assert_eq!(t.mtime[g], None);
    t.set_mtime(g, NonZeroU32::new(3));
    assert_eq!(t.mtime[g], NonZeroU32::new(3));
}
