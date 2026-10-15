//! The UI side of the directory watch: applies directory listings to the tree. A listing says
//! what a directory looks like now, so the diff needs no event kinds and a repeated or late
//! listing changes nothing.
use plumb_core::{NodeFlags, NodeId, Tree};
use plumb_scan::listing::Listing;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// Names compare without regard to case, as NTFS does (a lowercase approximation of its table;
/// a miss makes the diff remove and re-add, which heals itself).
fn fold(name: &[u16]) -> String {
    String::from_utf16_lossy(name).to_lowercase()
}

/// The directory node at `dir` below the root (`\`-separated; empty is the root itself).
pub(crate) fn resolve(tree: &Tree, dir: &[u16]) -> Option<NodeId> {
    let mut cur = *tree.roots.first()?;
    if dir.is_empty() {
        return Some(cur);
    }
    for part in dir.split(|&c| c == b'\\' as u16) {
        let want = fold(part);
        cur = tree.children(cur).find(|&c| tree.flags[c].contains(NodeFlags::DIR) && fold(tree.name(c)) == want)?;
    }
    Some(cur)
}

/// The two flags that make a directory entry a different kind of thing.
fn kind(is_dir: bool, reparse: bool) -> NodeFlags {
    let mut f = NodeFlags::empty();
    f.set(NodeFlags::DIR, is_dir);
    f.set(NodeFlags::REPARSE, reparse);
    f
}

fn apply_one(tree: &mut Tree, l: &Listing) -> usize {
    let Ok(entries) = &l.entries else { return 0 };
    let Some(dir) = resolve(tree, &l.dir) else { return 0 };
    let mut have: HashMap<String, NodeId> = tree.children(dir).map(|c| (fold(tree.name(c)), c)).collect();
    let mask = NodeFlags::DIR | NodeFlags::REPARSE;
    let mut changes = 0;
    for e in entries {
        let want = kind(e.is_dir, e.reparse);
        let size = if e.is_dir { 0 } else { e.alloc };
        match have.remove(&fold(&e.name)) {
            Some(id) if tree.flags[id] & mask == want => {
                // A hard link the scan counted as 0 stays 0 (the file's other links hold its space).
                let size = if tree.flags[id].contains(NodeFlags::HARDLINK) { 0 } else { size };
                if tree.mtime[id] != e.mtime {
                    tree.set_mtime(id, e.mtime);
                    changes += 1;
                }
                if tree.size[id] != size {
                    tree.set_size(id, size);
                    changes += 1;
                }
            }
            Some(id) => {
                tree.remove(id);
                let id = tree.add_child(dir, &e.name, size, want);
                tree.set_mtime(id, e.mtime);
                changes += 2;
            }
            None => {
                let id = tree.add_child(dir, &e.name, size, want);
                tree.set_mtime(id, e.mtime);
                changes += 1;
            }
        }
    }
    for (_, id) in have {
        tree.remove(id);
        changes += 1;
    }
    changes
}

/// Applies queued listings until the queue is empty or `deadline` passes (at least one is always
/// applied). Returns the number of tree changes made.
pub fn apply(tree: &mut Tree, queue: &mut VecDeque<Listing>, deadline: Instant) -> usize {
    let mut changes = 0;
    while let Some(l) = queue.pop_front() {
        changes += apply_one(tree, &l);
        if Instant::now() >= deadline {
            break;
        }
    }
    changes
}

#[cfg(test)]
mod tests;
