//! The process list as a [`Tree`], kept in step with successive snapshots by editing it in place (the same edit calls
//! the live file-system watch uses), so `RowModel` selection, scroll and expansion survive an update.
//!
//! Pure: no Windows types. The window converts its snapshot to [`Sample`]s.
use crate::{NodeFlags, NodeId, Tree, TreeBuilder};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Sample {
    pub pid: u32,
    pub parent: u32,
    /// Creation time; with the pid it identifies a process (pids are reused).
    pub created: u64,
    pub name: String,
    pub memory: u64,
    /// Private bytes (commit), which no other process shares.
    pub private: u64,
    /// User plus kernel time so far, 100 ns units.
    pub cpu_100ns: u64,
    /// Bytes transferred so far.
    pub io_bytes: u64,
}

/// What `Tree::size` (and so the map and the Size column) holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    /// Working set.
    Memory,
    /// Private bytes: committed memory no other process shares.
    Private,
    /// Tenths of a percent of one core.
    Cpu,
    /// Bytes per second.
    Io,
}

impl Metric {
    /// The metric a click on the map's metric button moves to: Memory, Private, CPU, I/O, and round again.
    pub fn next(self) -> Self {
        match self {
            Metric::Memory => Metric::Private,
            Metric::Private => Metric::Cpu,
            Metric::Cpu => Metric::Io,
            Metric::Io => Metric::Memory,
        }
    }
}

struct Known {
    id: NodeId,
    memory: u64,
    private: u64,
    cpu_tenths: u64,
    io_rate: u64,
    prev: (u64, u64),
}

impl Known {
    fn value(&self, m: Metric) -> u64 {
        match m {
            Metric::Memory => self.memory,
            Metric::Private => self.private,
            Metric::Cpu => self.cpu_tenths,
            Metric::Io => self.io_rate,
        }
    }
}

/// The bookkeeping that keeps a process [`Tree`] in step with snapshots; the tree itself belongs to the caller (the
/// app holds it like any other).
pub struct Procs {
    root: NodeId,
    metric: Metric,
    known: HashMap<(u32, u64), Known>,
}

/// The pid of a process node (0 for the synthetic root).
pub fn pid(tree: &Tree, id: NodeId) -> u32 {
    tree.aux.get(id.idx()).map_or(0, |a| a[2] as u32)
}

impl Procs {
    /// A new, empty process tree (just the synthetic root `Processes`) and the bookkeeping for it.
    pub fn new(metric: Metric) -> (Self, Tree) {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "Processes", 0, NodeFlags::DIR);
        (Self { root, metric, known: HashMap::new() }, b.finish().expect("a one-node tree"))
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Brings `tree` in line with `samples`; `elapsed_100ns` is the time since the previous call (0 = unknown, which
    /// gives zero rates).
    pub fn apply(&mut self, tree: &mut Tree, samples: &[Sample], elapsed_100ns: u64) {
        let seen: HashSet<(u32, u64)> = samples.iter().map(|s| (s.pid, s.created)).collect();
        // 1. Add what is new, under the root; they are moved to their parents below.
        for s in samples {
            if !self.known.contains_key(&(s.pid, s.created)) {
                let name: Vec<u16> = s.name.encode_utf16().collect();
                let id = tree.add_child(self.root, &name, 0, NodeFlags::empty());
                self.known.insert((s.pid, s.created), Known { id, memory: 0, private: 0, cpu_tenths: 0, io_rate: 0, prev: (s.cpu_100ns, s.io_bytes) });
            }
        }
        // 2. Parents: the live process with the parent pid, if it is older than the child (a younger one is a reused
        // pid). Idle (0) and System (4) always hang under the root.
        let by_pid: HashMap<u32, &Sample> = samples.iter().map(|s| (s.pid, s)).collect();
        for s in samples {
            let id = self.known[&(s.pid, s.created)].id;
            let wanted = by_pid
                .get(&s.parent)
                .filter(|p| s.pid > 4 && p.pid != s.pid && p.created <= s.created)
                .map_or(self.root, |p| self.known[&(p.pid, p.created)].id);
            if tree.parent[id] != wanted && !tree.reparent(id, wanted) {
                tree.reparent(id, self.root);
            }
        }
        // 3. What exited (its children already moved away).
        let gone: Vec<(u32, u64)> = self.known.keys().filter(|k| !seen.contains(k)).copied().collect();
        for k in gone {
            if let Some(k) = self.known.remove(&k) {
                tree.remove(k.id);
            }
        }
        // 4. A process is a folder while it has children and a file (one tile in the map) when it has none.
        for k in self.known.values() {
            tree.set_dir(k.id, tree.has_children(k.id));
        }
        // 5. Numbers.
        for s in samples {
            let k = self.known.get_mut(&(s.pid, s.created)).unwrap();
            let (dcpu, dio) = (s.cpu_100ns.saturating_sub(k.prev.0), s.io_bytes.saturating_sub(k.prev.1));
            k.prev = (s.cpu_100ns, s.io_bytes);
            (k.cpu_tenths, k.io_rate) = if elapsed_100ns == 0 {
                (0, 0)
            } else {
                let e = u128::from(elapsed_100ns);
                ((u128::from(dcpu) * 1000 / e) as u64, (u128::from(dio) * 10_000_000 / e) as u64)
            };
            // System Idle is not a program: its time is the machine's idle time, so it weighs nothing.
            if s.pid == 0 {
                (k.cpu_tenths, k.io_rate) = (0, 0);
            }
            k.memory = if s.pid == 0 { 0 } else { s.memory };
            k.private = if s.pid == 0 { 0 } else { s.private };
            tree.set_aux(k.id, [k.cpu_tenths, k.io_rate, u64::from(s.pid)]);
            let v = k.value(self.metric);
            if tree.size[k.id] != v {
                tree.set_size(k.id, v);
            }
        }
    }

    /// Switches what `Tree::size` holds; the roll-up follows.
    pub fn set_metric(&mut self, tree: &mut Tree, m: Metric) {
        self.metric = m;
        for k in self.known.values() {
            let v = k.value(m);
            if tree.size[k.id] != v {
                tree.set_size(k.id, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metric_cycles_through_all_four() {
        let mut m = Metric::Memory;
        let mut seen = vec![m];
        for _ in 0..3 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen, [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io]);
        assert_eq!(m.next(), Metric::Memory);
    }

    #[test]
    fn private_bytes_weigh_each_process_and_roll_up_through_parents() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100), s(11, 10, 2, "b.exe", 60)], 0);
        assert_eq!((p.tree.size[node(&p, 10)], p.tree.subtree_size[node(&p, 10)]), (100, 160));
        p.set_metric(Metric::Private);
        assert_eq!((p.tree.size[node(&p, 10)], p.tree.subtree_size[node(&p, 10)]), (50, 80), "own private bytes, and the family's");
        p.set_metric(Metric::Memory);
        assert_eq!(p.tree.subtree_size[node(&p, 10)], 160);
    }

    /// The bookkeeping and its tree together, as the tests drive them.
    struct ProcTree {
        sync: Procs,
        tree: Tree,
        root: NodeId,
    }
    impl ProcTree {
        fn new() -> Self {
            let (sync, tree) = Procs::new(Metric::Memory);
            Self { sync, tree, root: NodeId(0) }
        }
        fn apply(&mut self, samples: &[Sample], elapsed: u64) {
            self.sync.apply(&mut self.tree, samples, elapsed);
        }
        fn set_metric(&mut self, m: Metric) {
            self.sync.set_metric(&mut self.tree, m);
        }
        fn pid(&self, id: NodeId) -> u32 {
            pid(&self.tree, id)
        }
    }

    fn s(pid: u32, parent: u32, created: u64, name: &str, memory: u64) -> Sample {
        Sample { pid, parent, created, name: name.into(), memory, private: memory / 2, cpu_100ns: 0, io_bytes: 0 }
    }
    fn node(p: &ProcTree, pid: u32) -> NodeId {
        (0..p.tree.len() as u32).map(NodeId).find(|&n| !p.tree.flags[n].contains(NodeFlags::DEAD) && p.pid(n) == pid && n != p.root).unwrap()
    }
    /// The pid of the node's parent; 0 when it hangs under the synthetic root.
    fn parent_pid(p: &ProcTree, pid: u32) -> u32 {
        let par = p.tree.parent[node(p, pid)];
        if par == p.root { 0 } else { p.pid(par) }
    }

    #[test]
    fn system_idle_weighs_nothing_under_any_metric() {
        let idle = |cpu| Sample { cpu_100ns: cpu, io_bytes: cpu, ..s(0, 0, 1, "System Idle Process", 1 << 20) };
        let busy = |cpu| Sample { cpu_100ns: cpu, io_bytes: cpu, ..s(10, 1, 2, "a.exe", 100) };
        for m in [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io] {
            let mut p = ProcTree::new();
            p.set_metric(m);
            p.apply(&[idle(0), busy(0)], 0);
            p.apply(&[idle(15_000_000), busy(1_000_000)], 10_000_000);
            assert_eq!(p.tree.size[node(&p, 0)], 0, "{m:?}");
            assert!(p.tree.size[node(&p, 10)] > 0, "{m:?}");
        }
    }

    #[test]
    fn a_process_is_a_folder_only_while_it_has_children() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100)], 0);
        let a = node(&p, 10);
        assert!(!p.tree.flags[a].contains(NodeFlags::DIR));
        assert_eq!(p.tree.subtree_files[p.root], 1);
        p.apply(&[s(10, 1, 1, "a.exe", 100), s(11, 10, 2, "b.exe", 50)], 10_000_000);
        assert!(p.tree.flags[a].contains(NodeFlags::DIR));
        assert_eq!(p.tree.subtree_files[p.root], 1, "a family counts its leaves");
        p.apply(&[s(10, 1, 1, "a.exe", 100)], 10_000_000);
        assert!(!p.tree.flags[a].contains(NodeFlags::DIR), "the last child left");
        assert_eq!(p.tree.subtree_files[p.root], 1);
    }

    #[test]
    fn a_new_process_is_added_under_its_parent() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100), s(11, 10, 2, "b.exe", 50)], 0);
        assert_eq!(parent_pid(&p, 11), 10);
        assert_eq!(p.tree.subtree_size[node(&p, 10)], 150);
        assert_eq!(p.tree.name_string(node(&p, 11)), "b.exe");
    }

    #[test]
    fn an_existing_process_keeps_its_node_id_and_updates_its_size() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100)], 0);
        let id = node(&p, 10);
        p.tree.take_touched();
        p.apply(&[s(10, 1, 1, "a.exe", 300)], 10_000_000);
        assert_eq!(node(&p, 10), id);
        assert_eq!(p.tree.size[id], 300);
        assert!(p.tree.touched().contains(&id));
    }

    #[test]
    fn an_exited_process_is_removed_and_its_children_become_roots() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100), s(11, 10, 2, "b.exe", 50)], 0);
        let dead = node(&p, 10);
        p.apply(&[s(11, 10, 2, "b.exe", 50)], 10_000_000);
        assert!(p.tree.flags[dead].contains(NodeFlags::DEAD));
        assert_eq!(parent_pid(&p, 11), 0);
        assert_eq!(p.tree.subtree_size[p.root], 50);
    }

    #[test]
    fn a_reused_pid_with_a_new_creation_time_is_a_new_node() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100)], 0);
        let old = node(&p, 10);
        p.apply(&[s(10, 1, 5, "c.exe", 70)], 10_000_000);
        assert!(p.tree.flags[old].contains(NodeFlags::DEAD));
        let new = node(&p, 10);
        assert_ne!(new, old);
        assert_eq!(p.tree.name_string(new), "c.exe");
    }

    #[test]
    fn a_parent_pid_that_points_at_a_descendant_makes_a_root_not_a_cycle() {
        let mut p = ProcTree::new();
        // 10 and 11 each name the other as parent, both created together: a cycle by pid.
        p.apply(&[s(10, 11, 5, "a.exe", 1), s(11, 10, 5, "b.exe", 1)], 0);
        assert_eq!(p.tree.subtree_size[p.root], 2);
        let (a, b) = (parent_pid(&p, 10), parent_pid(&p, 11));
        assert!(a == 0 || b == 0, "one of them is a root: {a} {b}");
    }

    #[test]
    fn a_parent_created_after_its_child_is_a_reused_pid_and_not_a_parent() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 9, "late.exe", 1), s(11, 10, 2, "child.exe", 1)], 0);
        assert_eq!(parent_pid(&p, 11), 0);
    }

    #[test]
    fn pid_0_and_4_hang_under_the_root_and_pid_0_adds_no_memory() {
        let mut p = ProcTree::new();
        p.apply(&[s(0, 0, 0, "System Idle Process", 8192), s(4, 0, 0, "System", 100), s(300, 4, 1, "smss.exe", 10)], 0);
        assert_eq!(parent_pid(&p, 0), 0);
        assert_eq!(parent_pid(&p, 4), 0);
        assert_eq!(parent_pid(&p, 300), 4);
        assert_eq!(p.tree.subtree_size[p.root], 110);
    }

    #[test]
    fn cpu_is_the_delta_over_the_elapsed_time() {
        let mut p = ProcTree::new();
        let mut a = s(10, 1, 1, "a.exe", 1);
        p.apply(&[a.clone()], 0);
        assert_eq!(p.tree.aux[node(&p, 10).idx()][0], 0);
        a.cpu_100ns = 5_000_000;
        a.io_bytes = 2_000;
        p.apply(&[a], 10_000_000);
        assert_eq!(p.tree.aux[node(&p, 10).idx()], [500, 2_000, 10]);
    }

    #[test]
    fn switching_the_metric_rewrites_size_and_the_subtree_sums() {
        let mut p = ProcTree::new();
        let mut a = s(10, 1, 1, "a.exe", 100);
        let mut b = s(11, 10, 2, "b.exe", 50);
        p.apply(&[a.clone(), b.clone()], 0);
        (a.cpu_100ns, b.cpu_100ns) = (1_000_000, 2_000_000);
        p.apply(&[a, b], 10_000_000);
        p.set_metric(Metric::Cpu);
        assert_eq!(p.tree.size[node(&p, 10)], 100);
        assert_eq!(p.tree.subtree_size[p.root], 300);
        p.set_metric(Metric::Memory);
        assert_eq!(p.tree.subtree_size[p.root], 150);
    }

    #[test]
    fn a_removed_process_leaves_a_dead_node_and_the_live_count_is_right() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 1), s(11, 1, 2, "b.exe", 1)], 0);
        assert_eq!(p.tree.dead(), 0);
        p.apply(&[s(11, 1, 2, "b.exe", 1)], 10_000_000);
        assert_eq!(p.tree.dead(), 1);
        assert_eq!(p.tree.children(p.root).count(), 1);
    }
}
