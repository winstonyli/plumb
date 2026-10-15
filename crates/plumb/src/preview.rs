//! The UI side of the partial tree: turns the messages a running scan sends into `Tree`
//! mutations. Approximate on purpose (the exact tree replaces this one when the scan ends).
use plumb_core::{NodeFlags, NodeId, Tree, TreeBuilder};
use plumb_scan::journal::state::FileState;
use plumb_scan::preview::{PreviewMsg, PreviewRx};
use std::collections::HashMap;
use std::time::Instant;

/// Messages applied between two looks at the clock.
const CLOCK_EVERY: usize = 256;
/// Records waiting for a parent that has not arrived. Past this the preview stops following.
const MAX_PARKED: usize = 1_000_000;
/// Average name length assumed when reserving the name pool for an MFT preview.
const NAME_UNITS: usize = 12;

enum Kind {
    /// Nothing received since the start or the last `Reset`.
    Idle,
    /// Walker: ids equal the sender's push order.
    Walk,
    Mft(Mft),
}

#[derive(Default)]
struct Mft {
    /// Record number -> node.
    records: Vec<NodeId>,
    /// Waiting children, by the record number of the parent they wait for.
    parked: HashMap<u64, Vec<FileState>>,
    parked_count: usize,
    /// Sizes from extension records whose file has no node yet (or has size 0).
    sizes: HashMap<u64, u64>,
}

pub struct Preview {
    rx: PreviewRx,
    kind: Kind,
    /// Stopped following: a message did not fit, or too many records are parked.
    frozen: bool,
    /// The last `apply` found the queue empty.
    drained: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Tree changes made (zero means the rows need no refresh).
    pub changes: usize,
    /// The tree was created by this call (the first message).
    pub created: bool,
    /// The tree was dropped by a `Reset`.
    pub reset: bool,
}

impl Preview {
    pub fn new(rx: PreviewRx) -> Self {
        Self { rx, kind: Kind::Idle, frozen: false, drained: false }
    }

    /// True once the preview has stopped following the scan and has nothing left to show.
    pub fn paused(&self) -> bool {
        self.drained && (self.frozen || self.rx.lagged())
    }

    /// Applies queued messages until the queue is empty or `deadline` passes. A frozen preview
    /// still drains (so the sender's queue never stays full) and still honours `Reset`.
    pub fn apply(&mut self, tree: &mut Option<Tree>, deadline: Instant) -> Applied {
        let mut out = Applied::default();
        let mut n = 0usize;
        self.drained = false;
        loop {
            let Some(msg) = self.rx.try_recv() else {
                self.drained = true;
                return out;
            };
            if !self.frozen || matches!(msg, PreviewMsg::Reset) {
                self.handle(msg, tree, &mut out);
            }
            n += 1;
            if n.is_multiple_of(CLOCK_EVERY) && Instant::now() >= deadline {
                return out;
            }
        }
    }

    fn handle(&mut self, msg: PreviewMsg, tree: &mut Option<Tree>, out: &mut Applied) {
        match msg {
            PreviewMsg::Reset => {
                *tree = None;
                self.kind = Kind::Idle;
                self.frozen = false;
                out.reset = true;
                out.created = false;
                out.changes += 1;
            }
            PreviewMsg::Begin { root, nodes } => {
                let mut t = root_tree(&root.encode_utf16().collect::<Vec<_>>());
                t.reserve(nodes, nodes * NAME_UNITS);
                let mut records = vec![NodeId::NONE; nodes.max(6)];
                records[5] = NodeId(0);
                self.kind = Kind::Mft(Mft { records, ..Mft::default() });
                *tree = Some(t);
                out.created = true;
                out.changes += 1;
            }
            PreviewMsg::Add { parent, name, size, flags, mtime: _ } => {
                if parent.is_none() {
                    if matches!(self.kind, Kind::Idle) {
                        *tree = Some(root_tree(&name));
                        self.kind = Kind::Walk;
                        out.created = true;
                        out.changes += 1;
                    }
                    return;
                }
                let (Kind::Walk, Some(t)) = (&self.kind, tree.as_mut()) else { return };
                if parent.idx() >= t.len() {
                    self.frozen = true; // out of step with the sender: stop rather than guess
                    return;
                }
                t.add_child(parent, &name, size, flags);
                out.changes += 1;
            }
            PreviewMsg::Denied(id) => {
                if let (Kind::Walk, Some(t)) = (&self.kind, tree.as_mut())
                    && id.idx() < t.len()
                {
                    t.flags[id] |= NodeFlags::DENIED;
                    out.changes += 1;
                }
            }
            PreviewMsg::Upsert(s) => {
                let (Kind::Mft(m), Some(t)) = (&mut self.kind, tree.as_mut()) else { return };
                out.changes += m.place(t, s);
                if m.parked_count > MAX_PARKED {
                    self.frozen = true;
                }
            }
            PreviewMsg::Size { record, size } => {
                let (Kind::Mft(m), Some(t)) = (&mut self.kind, tree.as_mut()) else { return };
                match m.node(record) {
                    Some(id) if t.size[id] == 0 && !t.flags[id].contains(NodeFlags::DIR) => {
                        t.set_size(id, size);
                        out.changes += 1;
                    }
                    Some(_) => {}
                    None => {
                        m.sizes.entry(record).or_insert(size);
                    }
                }
            }
        }
    }
}

/// A tree holding just the root.
fn root_tree(name: &[u16]) -> Tree {
    let mut b = TreeBuilder::new();
    b.push(NodeId::NONE, name, 0, NodeFlags::DIR);
    b.finish().expect("one node")
}

impl Mft {
    fn node(&self, record: u64) -> Option<NodeId> {
        let id = *self.records.get(usize::try_from(record).ok()?)?;
        (!id.is_none()).then_some(id)
    }

    fn is_dir_node(&self, t: &Tree, record: u64) -> Option<NodeId> {
        self.node(record).filter(|&id| t.flags[id].contains(NodeFlags::DIR))
    }

    /// Adds `s` under its parent, or parks it until the parent is placed. Placing a directory
    /// releases its waiting children. Returns the number of nodes added.
    fn place(&mut self, t: &mut Tree, s: FileState) -> usize {
        let mut added = 0;
        let mut work = vec![s];
        while let Some(s) = work.pop() {
            let Some(parent) = self.is_dir_node(t, s.parent) else {
                self.parked_count += 1;
                self.parked.entry(s.parent).or_default().push(s);
                continue;
            };
            let Ok(record) = usize::try_from(s.record) else { continue };
            if self.node(s.record).is_some() {
                continue; // a record is placed once
            }
            let mut size = if s.is_dir { 0 } else { s.size };
            if size == 0
                && !s.is_dir
                && let Some(ext) = self.sizes.remove(&s.record)
            {
                size = ext;
            }
            let mut flags = NodeFlags::empty();
            flags.set(NodeFlags::DIR, s.is_dir);
            flags.set(NodeFlags::REPARSE, s.reparse);
            let id = t.add_child(parent, &s.name, size, flags);
            if self.records.len() <= record {
                self.records.resize(record + 1, NodeId::NONE);
            }
            self.records[record] = id;
            added += 1;
            if s.is_dir
                && let Some(kids) = self.parked.remove(&s.record)
            {
                self.parked_count -= kids.len();
                work.extend(kids);
            }
        }
        added
    }
}

#[cfg(test)]
mod tests;
