//! Live updates, the part that needs no window: applying journal deltas to the tree through the
//! record table, pacing the row refreshes, and deciding when an automatic rescan is allowed.
use plumb_core::{NodeFlags, NodeId, Tree};
use plumb_scan::journal::Delta;
use plumb_scan::journal::state::FileState;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// NTFS keeps the volume root in file record 5.
const ROOT_RECORD: u64 = 5;
/// Settle passes an upsert may wait for its parent before it goes under `<orphans>`.
const ORPHAN_AFTER: u8 = 2;
/// Deltas applied between two looks at the clock.
const CLOCK_EVERY: usize = 256;
/// Rows are refreshed at most this often (4 Hz).
pub const REPAINT: Duration = Duration::from_millis(250);
/// A rescan for compaction needs at least this many dead nodes, whatever the ratio.
const MIN_DEAD: usize = 10_000;
const MAX_DEAD: usize = 1_000_000;
/// At most one automatic rescan per volume in this long ...
const RESCAN_GAP: Duration = Duration::from_secs(600);
/// ... and none this soon after the user last touched the window.
const INPUT_IDLE: Duration = Duration::from_secs(30);

/// The record-number-to-node table plus upserts waiting for their parent.
pub struct Live {
    records: Vec<NodeId>,
    /// Upserts whose parent is unknown, and how many settle passes they have waited.
    parked: Vec<(FileState, u8)>,
    orphans: NodeId,
    changes: usize,
}

impl Live {
    /// `records` is `MftReport::records` of the scan that built the tree.
    pub fn new(records: Vec<NodeId>) -> Self {
        Self { records, parked: Vec::new(), orphans: NodeId::NONE, changes: 0 }
    }

    /// True while an upsert is waiting for its parent, so the caller keeps ticking.
    pub fn pending(&self) -> bool {
        !self.parked.is_empty()
    }

    /// Applies queued deltas until the queue is empty or `deadline` passes. When it empties,
    /// parked upserts get another try. Returns how many tree changes were made (zero means the
    /// rows need no refresh).
    pub fn apply(&mut self, tree: &mut Tree, queue: &mut VecDeque<Delta>, deadline: Instant) -> usize {
        let before = self.changes;
        let mut n = 0usize;
        while let Some(d) = queue.pop_front() {
            match d {
                Delta::Upsert(s) => self.upsert_or_park(tree, s),
                Delta::Remove { record } => self.remove(tree, record),
            }
            n += 1;
            if n.is_multiple_of(CLOCK_EVERY) && Instant::now() >= deadline {
                break;
            }
        }
        if queue.is_empty() {
            self.settle(tree);
        }
        self.changes - before
    }

    fn node(&self, tree: &Tree, record: u64) -> Option<NodeId> {
        let id = *self.records.get(usize::try_from(record).ok()?)?;
        (!id.is_none() && !tree.flags[id].contains(NodeFlags::DEAD)).then_some(id)
    }

    fn upsert_or_park(&mut self, tree: &mut Tree, s: FileState) {
        if s.record == ROOT_RECORD {
            return;
        }
        match self.node(tree, s.parent) {
            Some(parent) if self.upsert(tree, &s, parent) => {}
            _ => self.parked.push((s, 0)),
        }
    }

    /// Puts `s` in the tree under `parent`: adds it, or fixes whichever of kind, name, place and
    /// size differ. False if it could not be placed yet (moving it would put it inside itself,
    /// which happens when two folders swap places and their events arrive in the wrong order).
    fn upsert(&mut self, tree: &mut Tree, s: &FileState, parent: NodeId) -> bool {
        let mut flags = NodeFlags::empty();
        flags.set(NodeFlags::DIR, s.is_dir);
        flags.set(NodeFlags::REPARSE, s.reparse);
        flags.set(NodeFlags::HARDLINK, s.links > 1);
        let size = if s.is_dir { 0 } else { s.size };
        let record = s.record as usize;
        if let Some(id) = self.node(tree, s.record) {
            if tree.flags[id].contains(NodeFlags::DIR) == s.is_dir {
                if tree.parent[id] != parent {
                    if !tree.reparent(id, parent) {
                        return false;
                    }
                    self.changes += 1;
                }
                if tree.name(id) != s.name.as_slice() {
                    tree.rename(id, &s.name);
                    self.changes += 1;
                }
                if tree.size[id] != size {
                    tree.set_size(id, size);
                    self.changes += 1;
                }
                return true;
            }
            // A file became a folder or the reverse under the same record number.
            tree.remove(id);
            self.changes += 1;
        }
        if self.records.len() <= record {
            self.records.resize(record + 1, NodeId::NONE);
        }
        self.records[record] = tree.add_child(parent, &s.name, size, flags);
        self.changes += 1;
        true
    }

    fn remove(&mut self, tree: &mut Tree, record: u64) {
        if let Some(id) = self.node(tree, record) {
            if tree.remove(id) {
                self.changes += 1;
            }
            self.records[record as usize] = NodeId::NONE;
        }
    }

    /// Retries the parked upserts until a pass places none (one placement can unblock another).
    /// Those still unplaced wait a settle pass, or after `ORPHAN_AFTER` passes go under `<orphans>`.
    fn settle(&mut self, tree: &mut Tree) {
        let mut waiting = std::mem::take(&mut self.parked);
        loop {
            let mut placed = false;
            let mut still = Vec::new();
            for (s, age) in waiting {
                match self.node(tree, s.parent) {
                    Some(p) if self.upsert(tree, &s, p) => placed = true,
                    _ => still.push((s, age)),
                }
            }
            waiting = still;
            if !placed {
                break;
            }
        }
        for (s, age) in waiting {
            if age + 1 < ORPHAN_AFTER {
                self.parked.push((s, age + 1));
            } else if let Some(o) = self.orphans(tree) {
                self.upsert(tree, &s, o);
            }
        }
    }

    /// The `<orphans>` folder under the root: the one the scan made, or a new one.
    fn orphans(&mut self, tree: &mut Tree) -> Option<NodeId> {
        if !self.orphans.is_none() && !tree.flags[self.orphans].contains(NodeFlags::DEAD) {
            return Some(self.orphans);
        }
        let root = self.node(tree, ROOT_RECORD)?;
        let name: Vec<u16> = "<orphans>".encode_utf16().collect();
        let found = tree.children(root).find(|&c| tree.name(c) == name.as_slice());
        let id = found.unwrap_or_else(|| {
            self.changes += 1;
            tree.add_child(root, &name, 0, NodeFlags::DIR)
        });
        self.orphans = id;
        Some(id)
    }
}

/// True when dead nodes are worth a rescan: over a quarter of the live ones (and at least
/// `MIN_DEAD`), or past `MAX_DEAD`.
pub fn needs_compaction(tree: &Tree) -> bool {
    let dead = tree.dead();
    dead > MAX_DEAD || (dead >= MIN_DEAD && dead * 4 > tree.len() - dead)
}

/// Keeps row refreshes at or under 4 Hz, and backs off when a refresh is slow so refreshing
/// never takes more than about a sixth of the time.
pub struct Pacer {
    next: Instant,
}

impl Pacer {
    pub fn new(now: Instant) -> Self {
        Self { next: now }
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Call after a refresh that finished at `now` and took `took`.
    pub fn done(&mut self, now: Instant, took: Duration) {
        self.next = now + REPAINT.max(took * 5);
    }
}

/// When an automatic rescan may start.
#[derive(Default)]
pub struct RescanPolicy {
    last: HashMap<String, Instant>,
}

impl RescanPolicy {
    /// True (and recorded) if the volume at `root` has not been rescanned automatically in the last ten
    /// minutes and the user has not touched the window in the last 30 seconds.
    pub fn allow(&mut self, root: &str, now: Instant, last_input: Instant) -> bool {
        let recent = self.last.get(root).is_some_and(|&t| now.duration_since(t) < RESCAN_GAP);
        if recent || now.duration_since(last_input) < INPUT_IDLE {
            return false;
        }
        self.last.insert(root.to_string(), now);
        true
    }
}

#[cfg(test)]
mod tests;
