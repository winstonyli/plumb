# Directory watch (live updates without admin): implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After a walker scan, keep the tree current without admin: a recursive `ReadDirectoryChangesW` watch marks directories dirty, a lister re-reads them, and the UI diffs each listing against the tree.

**Architecture:** `plumb-scan::watch` runs a reader thread (events -> dirty map) and a lister thread (every 250 ms: re-list dirty directories, send `Listing`s). `plumb::dirsync` applies listings to the `Tree` in 5 ms slices. `window.rs` holds either the journal source or the directory source behind one `Watch` type, reusing `TIMER_LIVE`, `Pacer`, auto-rescan and the stale banner.

**Tech stack:** Rust 2024 nightly, `windows` 0.62, `plumb-core::Tree`, std threads/channels/condvar.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-dirwatch-design.md` (spike result recorded there).

## Spec amendments made by this plan

1. `Entry` and `Listing` live in a portable module `plumb_scan::listing`, so the applier is testable without Windows APIs. `Stop::Requested` is dropped from the directory watcher (it only exists for the journal).
2. Reader is synchronous: one blocking `ReadDirectoryChangesW` per loop. The kernel buffers changes between calls (documented), so there is no gap. `stop()` cancels with `CancelIoEx` in a retry loop until the reader exits (a cancel can miss a reader that is between calls).
3. A reparse-point directory is never `deep` (the walker does not descend into them either).
4. The overflow/`TooMany`/hold logic is one pure function `cycle` so it is unit-tested without a real watch.

## Global constraints

- `cargo` with `-j 8`, `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3`. Python only as a script file run with `python3 -I -P f.py < /dev/null`, never on stdin.
- Repo files are CRLF; edit with the Edit tool or a script that converts (`replace('\r\n','\n')` on read, back on write).
- Never block the UI thread on a watcher join: `stop()` runs on a spawned thread from the window.
- No fixed sleeps in tests: wait on a condition with a deadline.
- Do not change `Live`, the journal reader, or the walker's behavior.
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## File map

- Create `crates/plumb-scan/src/listing.rs` (portable): `Entry`, `Listing`.
- Modify `crates/plumb-scan/src/walk.rs`: use `listing::Entry`; `list_dir`, `cluster_size` become `pub(crate)`.
- Create `crates/plumb-scan/src/watch.rs` (windows): watcher.
- Modify `crates/plumb-scan/src/lib.rs`: `pub mod listing;`, `#[cfg(windows)] pub mod watch;`; Cargo features if a missing Win32 module appears.
- Create `crates/plumb/src/dirsync.rs` + `dirsync/tests.rs`; modify `crates/plumb/src/lib.rs`.
- Modify `crates/plumb/src/window.rs`, `main.rs`.
- Docs: `CHANGELOG.md`, the spec status.

---

### Task 1: Portable `Entry` and `Listing`

**Files:** Create `crates/plumb-scan/src/listing.rs`; modify `walk.rs`, `lib.rs`.

**Interfaces:** Produces `listing::Entry { pub name: Vec<u16>, pub alloc: u64, pub is_dir: bool, pub reparse: bool }` (Clone, Debug, PartialEq, Eq) and `listing::Listing { pub dir: Vec<u16>, pub entries: Result<Vec<Entry>, ()> }` (Debug). `walk::{list_dir, cluster_size}` are `pub(crate)`.

- [ ] **Step 1:** Create `listing.rs`:

```rust
//! What a directory looked like when it was listed. Shared by the walker and the directory watch.

/// One directory entry. `alloc` is allocated bytes (0 for directories and for data that lives
/// inside the MFT record), as the walker counts them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u16>,
    pub alloc: u64,
    pub is_dir: bool,
    /// A symlink or junction: listed, never followed.
    pub reparse: bool,
}

/// A directory and its entries, or `Err` if it could not be listed (it is gone, or not readable).
/// `dir` is the path below the watched root, `\`-separated, empty for the root itself.
#[derive(Debug)]
pub struct Listing {
    pub dir: Vec<u16>,
    pub entries: Result<Vec<Entry>, ()>,
}
```

- [ ] **Step 2:** In `lib.rs` add `pub mod listing;` after `pub mod journal;` and `#[cfg(windows)]\npub mod watch;` after `pub mod volume;`. In `walk.rs` delete the private `struct Entry { ... }` block, add `use crate::listing::Entry;`, and change `fn cluster_size` and `fn list_dir` to `pub(crate) fn`. (`watch` does not exist yet: add the `pub mod watch;` line in Task 3 instead.)
- [ ] **Step 3:** `cargo test -p plumb-scan -j 8` passes unchanged. Commit `git commit -am "Move Entry to a portable listing module"` (after `git add crates`).

---

### Task 2: The applier (`plumb::dirsync`)

**Files:** Create `crates/plumb/src/dirsync.rs`, `crates/plumb/src/dirsync/tests.rs`; modify `crates/plumb/src/lib.rs` (`pub mod dirsync;` after `pub mod app;`).

**Interfaces:** Consumes `listing::{Entry, Listing}`. Produces `dirsync::apply(tree: &mut Tree, queue: &mut VecDeque<Listing>, deadline: Instant) -> usize` (tree changes made; always applies at least one listing if any is queued).

- [ ] **Step 1: Tests first.** Create `dirsync/tests.rs`:

```rust
use super::*;
use plumb_core::TreeBuilder;
use plumb_scan::listing::Entry;
use std::collections::BTreeMap;
use std::time::Duration;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn file(name: &str, alloc: u64) -> Entry {
    Entry { name: u(name), alloc, is_dir: false, reparse: false }
}

fn dir(name: &str) -> Entry {
    Entry { name: u(name), alloc: 0, is_dir: true, reparse: false }
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
```

- [ ] **Step 2: Implement.** Create `dirsync.rs`:

```rust
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
                if tree.size[id] != size {
                    tree.set_size(id, size);
                    changes += 1;
                }
            }
            Some(id) => {
                tree.remove(id);
                tree.add_child(dir, &e.name, size, want);
                changes += 2;
            }
            None => {
                tree.add_child(dir, &e.name, size, want);
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
```

- [ ] **Step 3:** Run `cargo test -p plumb -j 8 --lib dirsync` (7 pass). Commit `git add crates && git commit -m "Add the directory-listing applier"`.

---

### Task 3: The watcher (`plumb-scan::watch`)

**Files:** Create `crates/plumb-scan/src/watch.rs`; modify `lib.rs` (add `#[cfg(windows)]\npub mod watch;`).

**Interfaces:**
- Consumes: `listing::{Entry, Listing}`, `walk::{list_dir, cluster_size}`.
- Produces: `watch::{Watcher, WatchConfig, Event, Stop}`; `Watcher::start(root: &Path, cfg: WatchConfig, notify: impl Fn() + Send + 'static) -> Result<Watcher, String>` (starts **held**), `try_recv(&self) -> Option<Event>`, `release(&self)`, `stop(self)`; `Event::{Listings(Vec<Listing>), Stopped(Stop)}`; `Stop::{Overflow, TooMany, Failed(String)}`; `WatchConfig { window: Duration, max_dirty: usize }` with `Default` (250 ms, 50 000).

- [ ] **Step 1: Write the module** (tests included at the bottom). Create `watch.rs`:

```rust
//! The directory watch behind live updates for walker-scanned trees. A reader thread turns
//! `ReadDirectoryChangesW` records into a map of dirty directories; a lister thread re-reads them
//! every `window` and sends what they look like now. The kernel only says "something changed in
//! this directory"; the file system says what is true, so renames, reordering and repeated events
//! need no special cases. The watch starts *held*: dirty directories accumulate (so changes made
//! while the scan runs are not lost) and nothing is sent until `release`.
use crate::listing::{Entry, Listing};
use crate::walk::{cluster_size, list_dir};
use std::collections::{HashMap, HashSet};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileAttributesW, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::CancelIoEx;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows::core::PCWSTR;

/// The network limit; plenty locally (the spike's busiest call used 630 bytes).
const BUF_BYTES: usize = 64 * 1024;
const LIST_BUF_BYTES: usize = 256 * 1024;
const ACTION_ADDED: u32 = 1;
const ACTION_RENAMED_NEW: u32 = 5;
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;
const BS: u16 = b'\\' as u16;

#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// The kernel buffer overflowed: changes were missed.
    Overflow,
    /// So many directories changed at once that a rescan is cheaper.
    TooMany,
    Failed(String),
}

#[derive(Debug)]
pub enum Event {
    /// What the dirty directories look like now, parents before children.
    Listings(Vec<Listing>),
    /// The watch has ended and will send nothing more.
    Stopped(Stop),
}

#[derive(Clone, Copy)]
pub struct WatchConfig {
    /// How long changes are collected before their directories are re-read.
    pub window: Duration,
    /// More dirty directories than this (once released) is a `Stop::TooMany`.
    pub max_dirty: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self { window: Duration::from_millis(250), max_dirty: 50_000 }
    }
}

/// Dirty directories below the root (`\`-separated; empty is the root). The flag marks a
/// directory that is new: its whole subtree must be listed.
#[derive(Default)]
struct Dirty {
    dirs: HashMap<Vec<u16>, bool>,
    overflow: bool,
    failed: Option<String>,
}

impl Dirty {
    fn note(&mut self, dir: &[u16], deep: bool) {
        *self.dirs.entry(dir.to_vec()).or_insert(false) |= deep;
    }

    /// One change record: the parent of `rel` is dirty; a directory that appeared is listed whole.
    fn record(&mut self, rel: &[u16], new_dir: bool) {
        let parent = rel.iter().rposition(|&c| c == BS).map_or(&rel[..0], |i| &rel[..i]);
        self.note(parent, false);
        if new_dir {
            self.note(rel, true);
        }
    }
}

struct Shared {
    dirty: Mutex<Dirty>,
    cv: Condvar,
    stop: AtomicBool,
    held: AtomicBool,
}

/// `(action, name)` for every record in a `FILE_NOTIFY_INFORMATION` buffer.
fn parse_notify(buf: &[u8]) -> Vec<(u32, Vec<u16>)> {
    let u32_at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 12 <= buf.len() {
        let (next, action, len) = (u32_at(off) as usize, u32_at(off + 4), u32_at(off + 8) as usize);
        if off + 12 + len > buf.len() {
            break;
        }
        let name = buf[off + 12..off + 12 + len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push((action, name));
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

/// `root` (no trailing backslash) joined with `rel`; the root itself keeps its backslash.
fn abs(root: &[u16], rel: &[u16]) -> Vec<u16> {
    let mut p = root.to_vec();
    p.push(BS);
    p.extend_from_slice(rel);
    p
}

fn rel_join(dir: &[u16], name: &[u16]) -> Vec<u16> {
    let mut p = dir.to_vec();
    if !p.is_empty() {
        p.push(BS);
    }
    p.extend_from_slice(name);
    p
}

fn depth(d: &[u16]) -> usize {
    if d.is_empty() { 0 } else { 1 + d.iter().filter(|&&c| c == BS).count() }
}

/// A directory that exists now and is not a symlink or junction.
fn is_real_dir(root: &[u16], rel: &[u16]) -> bool {
    let mut p = abs(root, rel);
    p.push(0);
    let a = unsafe { GetFileAttributesW(PCWSTR(p.as_ptr())) };
    a != u32::MAX && a & FILE_ATTRIBUTE_DIRECTORY.0 != 0 && a & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
}

fn read_loop(handle: isize, root: Vec<u16>, sh: Arc<Shared>) {
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE;
    let mut buf = vec![0u64; BUF_BYTES / 8];
    loop {
        let mut got = 0u32;
        let r = unsafe {
            ReadDirectoryChangesW(HANDLE(handle as *mut _), buf.as_mut_ptr().cast(), BUF_BYTES as u32, true, filter, Some(&mut got), None, None)
        };
        if sh.stop.load(Ordering::Acquire) {
            return;
        }
        match r {
            Err(e) => {
                let mut d = sh.dirty.lock().unwrap();
                if e.code().0 as u32 & 0xFFFF == ERROR_NOTIFY_ENUM_DIR {
                    d.overflow = true;
                } else {
                    d.failed = Some(format!("the directory watch failed: {e}"));
                }
                sh.cv.notify_all();
                return;
            }
            Ok(()) if got == 0 => {
                sh.dirty.lock().unwrap().overflow = true;
                sh.cv.notify_all();
                return;
            }
            Ok(()) => {
                let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), got as usize) };
                // The attribute check is a system call: do it before taking the lock.
                let recs: Vec<(Vec<u16>, bool)> = parse_notify(bytes)
                    .into_iter()
                    .map(|(action, name)| {
                        let new_dir = matches!(action, ACTION_ADDED | ACTION_RENAMED_NEW) && is_real_dir(&root, &name);
                        (name, new_dir)
                    })
                    .collect();
                let mut d = sh.dirty.lock().unwrap();
                for (name, new_dir) in recs {
                    d.record(&name, new_dir);
                }
            }
        }
    }
}

/// Lists the dirty directories (new ones recursively, never through reparse points), parents
/// before children, each directory once.
fn list_dirty(root: &[u16], dirs: HashMap<Vec<u16>, bool>, cluster: u64, buf: &mut [u64]) -> Vec<Listing> {
    let mut order: Vec<(Vec<u16>, bool)> = dirs.into_iter().collect();
    order.sort_by_key(|(_, deep)| !*deep); // whole subtrees first, so their members are skipped later
    let mut seen: HashSet<Vec<u16>> = HashSet::new();
    let mut out = Vec::new();
    for (dir, deep) in order {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            if !seen.insert(d.clone()) {
                continue;
            }
            let entries: Result<Vec<Entry>, ()> = list_dir(&abs(root, &d), buf, cluster);
            if deep && let Ok(es) = &entries {
                stack.extend(es.iter().filter(|e| e.is_dir && !e.reparse).map(|e| rel_join(&d, &e.name)));
            }
            out.push(Listing { dir: d, entries });
        }
    }
    out.sort_by_key(|l| depth(&l.dir));
    out
}

enum Cycle {
    Idle,
    Listings(Vec<Listing>),
    Stop(Stop),
}

/// One lister turn. Overflow and failure are reported even while held; listings only once released.
fn cycle(sh: &Shared, root: &[u16], cfg: &WatchConfig, cluster: u64, buf: &mut [u64]) -> Cycle {
    let dirs = {
        let mut d = sh.dirty.lock().unwrap();
        if d.overflow {
            return Cycle::Stop(Stop::Overflow);
        }
        if let Some(m) = d.failed.take() {
            return Cycle::Stop(Stop::Failed(m));
        }
        if sh.held.load(Ordering::Acquire) || d.dirs.is_empty() {
            return Cycle::Idle;
        }
        if d.dirs.len() > cfg.max_dirty {
            return Cycle::Stop(Stop::TooMany);
        }
        std::mem::take(&mut d.dirs)
    };
    Cycle::Listings(list_dirty(root, dirs, cluster, buf))
}

fn lister_loop(root: Vec<u16>, sh: Arc<Shared>, cfg: WatchConfig, tx: Sender<Event>, notify: impl Fn()) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    let cluster = cluster_size(&abs(&root, &[]));
    let mut buf = vec![0u64; LIST_BUF_BYTES / 8];
    loop {
        {
            let g = sh.dirty.lock().unwrap();
            let _ = sh.cv.wait_timeout(g, cfg.window).unwrap();
        }
        if sh.stop.load(Ordering::Acquire) {
            return;
        }
        match cycle(&sh, &root, &cfg, cluster, &mut buf) {
            Cycle::Idle => {}
            Cycle::Listings(l) => {
                if tx.send(Event::Listings(l)).is_err() {
                    return;
                }
                notify();
            }
            Cycle::Stop(s) => {
                let _ = tx.send(Event::Stopped(s));
                notify();
                return;
            }
        }
    }
}

pub struct Watcher {
    events: Receiver<Event>,
    shared: Arc<Shared>,
    handle: isize,
    reader: Option<JoinHandle<()>>,
    lister: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching `root` and everything below it, held. `notify` runs on the lister thread
    /// after every event is queued (post a window message from it); keep it cheap.
    pub fn start(root: &Path, cfg: WatchConfig, notify: impl Fn() + Send + 'static) -> Result<Watcher, String> {
        // The verbatim form (long-path prefix), as the walker uses, so deep paths list too.
        let root = std::fs::canonicalize(root).map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        while wide.len() > 2 && wide.last() == Some(&BS) {
            wide.pop();
        }
        let mut p = abs(&wide, &[]);
        p.push(0);
        let h = unsafe {
            CreateFileW(
                PCWSTR(p.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
        let shared = Arc::new(Shared {
            dirty: Mutex::new(Dirty::default()),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            held: AtomicBool::new(true),
        });
        let (tx, events) = mpsc::channel();
        let handle = h.0 as isize;
        let reader = {
            let (root, sh) = (wide.clone(), shared.clone());
            std::thread::spawn(move || read_loop(handle, root, sh))
        };
        let lister = {
            let sh = shared.clone();
            std::thread::spawn(move || lister_loop(wide, sh, cfg, tx, notify))
        };
        Ok(Watcher { events, shared, handle, reader: Some(reader), lister: Some(lister) })
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Lets the lister send what has accumulated (and everything after).
    pub fn release(&self) {
        self.shared.held.store(false, Ordering::Release);
        self.shared.cv.notify_all();
    }

    /// Stops both threads and closes the directory. Blocks for a few milliseconds (a read that
    /// has not started yet is cancelled again until it exits); call it off the UI thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.cv.notify_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        if let Some(t) = self.reader.take() {
            while !t.is_finished() && Instant::now() < deadline {
                unsafe {
                    let _ = CancelIoEx(HANDLE(self.handle as *mut _), None);
                }
                std::thread::park_timeout(Duration::from_millis(5));
            }
            if !t.is_finished() {
                return; // stuck in the kernel: leave the handle open rather than close it under the read
            }
            let _ = t.join();
        }
        if let Some(t) = self.lister.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.handle as *mut _));
        }
        self.handle = 0;
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if self.handle != 0 {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 2: Tests** in `watch/tests.rs`:

```rust
use super::*;
use std::fs;
use std::path::PathBuf;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Temp {
        let p = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("plumb-watch-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Temp(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(name: &str, action: u32) -> Vec<u8> {
    let n: Vec<u8> = u(name).iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut b = Vec::new();
    b.extend_from_slice(&0u32.to_le_bytes()); // next offset, patched by the caller
    b.extend_from_slice(&action.to_le_bytes());
    b.extend_from_slice(&(n.len() as u32).to_le_bytes());
    b.extend_from_slice(&n);
    b
}

#[test]
fn parse_notify_reads_chained_records() {
    let mut first = record("a\\b.txt", 1);
    let next = first.len() as u32;
    first[0..4].copy_from_slice(&next.to_le_bytes());
    first.extend(record("c", 2));
    assert_eq!(parse_notify(&first), [(1, u("a\\b.txt")), (2, u("c"))]);
    assert!(parse_notify(&first[..10]).is_empty(), "a truncated buffer yields nothing and does not panic");
}

#[test]
fn a_record_dirties_the_parent_and_a_new_directory_is_listed_whole() {
    let mut d = Dirty::default();
    d.record(&u("a\\b\\f.txt"), false);
    d.record(&u("a\\n"), true);
    d.record(&u("top.txt"), false);
    let mut got: Vec<(String, bool)> = d.dirs.iter().map(|(k, v)| (String::from_utf16_lossy(k), *v)).collect();
    got.sort();
    assert_eq!(got, [("".into(), false), ("a".into(), false), ("a\\b".into(), false), ("a\\n".into(), true)]);
}

fn shared(held: bool) -> Shared {
    Shared { dirty: Mutex::new(Dirty::default()), cv: Condvar::new(), stop: AtomicBool::new(false), held: AtomicBool::new(held) }
}

fn root_of(t: &Temp) -> Vec<u16> {
    t.0.as_os_str().encode_wide().collect()
}

#[test]
fn a_held_watch_reports_only_overflow_and_a_released_one_lists() {
    let t = Temp::new("cycle");
    fs::write(t.0.join("a.txt"), b"hi").unwrap();
    let root = root_of(&t);
    let cfg = WatchConfig::default();
    let mut buf = vec![0u64; 4096];
    let sh = shared(true);
    sh.dirty.lock().unwrap().note(&[], false);
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle), "held");
    sh.held.store(false, Ordering::Release);
    let Cycle::Listings(l) = cycle(&sh, &root, &cfg, 0, &mut buf) else { panic!("released: listings") };
    assert_eq!(l.len(), 1);
    assert!(l[0].entries.as_ref().unwrap().iter().any(|e| e.name == u("a.txt")));
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle), "taken");
    sh.dirty.lock().unwrap().overflow = true;
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Stop(Stop::Overflow)));
}

#[test]
fn too_many_dirty_directories_stop_the_watch_only_once_released() {
    let t = Temp::new("many");
    let root = root_of(&t);
    let cfg = WatchConfig { window: Duration::from_millis(10), max_dirty: 2 };
    let mut buf = vec![0u64; 4096];
    let sh = shared(true);
    for d in ["a", "b", "c"] {
        sh.dirty.lock().unwrap().note(&u(d), false);
    }
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Idle));
    sh.held.store(false, Ordering::Release);
    assert!(matches!(cycle(&sh, &root, &cfg, 0, &mut buf), Cycle::Stop(Stop::TooMany)));
}

#[test]
fn a_new_directory_is_listed_down_to_its_files_and_parents_come_first() {
    let t = Temp::new("deep");
    fs::create_dir_all(t.0.join("n").join("m")).unwrap();
    fs::write(t.0.join("n").join("m").join("f.txt"), b"x").unwrap();
    let mut buf = vec![0u64; 4096];
    let mut dirs = HashMap::new();
    dirs.insert(u("n\\m"), false);
    dirs.insert(u("n"), true);
    let l = list_dirty(&root_of(&t), dirs, 0, &mut buf);
    let names: Vec<String> = l.iter().map(|x| String::from_utf16_lossy(&x.dir)).collect();
    assert_eq!(names, ["n", "n\\m"], "each once, parents first");
    assert!(l[1].entries.as_ref().unwrap().iter().any(|e| e.name == u("f.txt")));
}

/// Polls the watcher until `want` accepts a listing, or the deadline passes.
fn wait_listing(w: &Watcher, secs: u64, want: &dyn Fn(&Listing) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match w.try_recv() {
            Some(Event::Listings(ls)) if ls.iter().any(want) => return true,
            Some(Event::Stopped(s)) => panic!("watch stopped: {s:?}"),
            Some(_) => {}
            None => std::thread::park_timeout(Duration::from_millis(10)),
        }
    }
    false
}

#[test]
fn a_real_watch_reports_changes_made_while_held_and_after_release() {
    let t = Temp::new("real");
    fs::create_dir_all(t.0.join("sub")).unwrap();
    let w = Watcher::start(&t.0, WatchConfig { window: Duration::from_millis(30), max_dirty: 1000 }, || {}).unwrap();
    fs::write(t.0.join("sub").join("early.bin"), vec![1u8; 4096]).unwrap();
    // The reader has seen it once the parent is in the dirty map: nothing may be sent before release.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !w.shared.dirty.lock().unwrap().dirs.contains_key(&u("sub")) {
        assert!(Instant::now() < deadline, "the reader never saw the write");
        std::thread::park_timeout(Duration::from_millis(5));
    }
    assert!(w.try_recv().is_none(), "held");
    w.release();
    let early = |l: &Listing| l.dir == u("sub") && l.entries.as_ref().is_ok_and(|e| e.iter().any(|x| x.name == u("early.bin")));
    assert!(wait_listing(&w, 10, &early), "a change made while held arrives after release");
    fs::create_dir_all(t.0.join("new").join("inner")).unwrap();
    fs::write(t.0.join("new").join("inner").join("f.txt"), b"x").unwrap();
    let inner = |l: &Listing| l.dir == u("new\\inner") && l.entries.as_ref().is_ok_and(|e| e.iter().any(|x| x.name == u("f.txt")));
    assert!(wait_listing(&w, 10, &inner), "a new directory is listed down to its files");
    w.stop();
}
```

(The `watch` module needs `#[cfg(test)] mod tests;` as written and the file at `watch/tests.rs`.) If `Dirty::record` indexing errors on `&rel[..0]`, it is correct for empty-parent. If `list_dir` requires a larger buffer than 4096 u64 in tests, use `vec![0u64; 256 * 1024 / 8]`.

- [ ] **Step 3:** Run `cargo test -p plumb-scan -j 8 watch` (all pass); fix mismatches by tracing (Win32 feature names: if `GetFileAttributesW` or `CancelIoEx` is unresolved, add the missing feature to `crates/plumb-scan/Cargo.toml`; `Win32_System_IO` and `Win32_Storage_FileSystem` are already there). Then `cargo test --workspace -j 8` and commit `git add crates && git commit -m "Add the directory watcher"`.

---

### Task 4: Window integration

**Files:** Modify `crates/plumb/src/window.rs`, `crates/plumb/src/main.rs`.

**Interfaces:** Consumes `Watcher`, `WatchConfig`, `watch::{Event as WatchEvent, Stop as WatchStop}`, `dirsync::apply`, `Listing`. No new public API besides `selftest_watch() -> Result<String>`.

- [ ] **Step 1: Types.** Imports (`use` lines near the existing ones): `use plumb_scan::listing::Listing;`, `use plumb_scan::watch::{Event as WatchEvent, Stop as WatchStop, WatchConfig, Watcher};`. Replace the `struct Watch { ... }` with:

```rust
/// What a live watch follows.
enum Source {
    Journal { reader: JournalReader, live: Live, queue: VecDeque<Delta> },
    Dir { watcher: Watcher, queue: VecDeque<Listing> },
}

/// Everything live updates need for the drive on screen.
struct Watch {
    src: Source,
    pacer: Pacer,
    /// The tree changed since the rows were last refreshed.
    dirty: bool,
    /// `TIMER_LIVE` is running.
    armed: bool,
}

/// Why a watch ended; `why` is `None` when it was asked to stop.
struct Ended {
    rescan: bool,
    why: Option<String>,
}

impl Watch {
    /// Moves what the source has queued into its queue; `Some` if the source has ended.
    fn drain(&mut self) -> Option<Ended> {
        let ended = |rescan: bool, why: &str| Some(Ended { rescan, why: Some(why.to_string()) });
        match &mut self.src {
            Source::Journal { reader, queue, .. } => {
                while let Some(ev) = reader.try_recv() {
                    match ev {
                        Event::Deltas(d) => queue.extend(d),
                        Event::Stopped(Stop::Requested) => return Some(Ended { rescan: false, why: None }),
                        Event::Stopped(Stop::Wrapped) => {
                            return ended(true, "the change journal wrapped, so changes were missed");
                        }
                        Event::Stopped(Stop::Disabled) => return ended(false, "the change journal is not active"),
                        Event::Stopped(Stop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
            Source::Dir { watcher, queue } => {
                while let Some(ev) = watcher.try_recv() {
                    match ev {
                        WatchEvent::Listings(l) => queue.extend(l),
                        WatchEvent::Stopped(WatchStop::Overflow) => {
                            return ended(true, "the directory watch overflowed, so changes were missed");
                        }
                        WatchEvent::Stopped(WatchStop::TooMany) => {
                            return ended(true, "too many folders changed at once");
                        }
                        WatchEvent::Stopped(WatchStop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
        }
        None
    }

    /// Applies queued changes for the budget; returns how many tree changes were made.
    fn apply(&mut self, tree: &mut Tree, deadline: Instant) -> usize {
        match &mut self.src {
            Source::Journal { live, queue, .. } => live.apply(tree, queue, deadline),
            Source::Dir { queue, .. } => crate::dirsync::apply(tree, queue, deadline),
        }
    }

    fn queued(&self) -> bool {
        match &self.src {
            Source::Journal { queue, .. } => !queue.is_empty(),
            Source::Dir { queue, .. } => !queue.is_empty(),
        }
    }

    /// Work left that a timer tick should finish.
    fn backlog(&self) -> bool {
        self.queued() || matches!(&self.src, Source::Journal { live, .. } if live.pending())
    }

    /// Stops the source (blocks briefly: call off the UI thread).
    fn stop(self) {
        match self.src {
            Source::Journal { reader, .. } => reader.stop(),
            Source::Dir { watcher, .. } => watcher.stop(),
        }
    }
}
```

- [ ] **Step 2: `Win` fields and constructor.** Add `dir_pending: Option<Watcher>,` (doc: "A directory watch started with the scan and held until the tree is installed.") to `Win` and `dir_pending: None,` to `Win::new`.

- [ ] **Step 3: Replace the watch functions** (`start_watch`, `drop_watch`, `journal_event`, `apply_live`, `live_tick`, `journal_stopped`) with:

```rust
    fn start_watch(&mut self, vol: Volume, info: JournalInfo, records: Vec<NodeId>) {
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        let reader = JournalReader::start(vol, info, JournalConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        });
        self.watch = Some(Watch {
            src: Source::Journal { reader, live: Live::new(records), queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
    }

    /// Makes the directory watch started with the scan the live watch: releases what it collected
    /// while the scan ran and wakes the window to apply it.
    fn install_dir_watch(&mut self, watcher: Watcher) {
        watcher.release();
        self.watch = Some(Watch {
            src: Source::Dir { watcher, queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
    }

    /// Starts a held directory watch on `root` when the walker will scan it (not an elevated NTFS
    /// drive, which gets the change journal). A watch that cannot start leaves the tree static.
    fn start_dir_pending(&mut self, root: &str) {
        self.drop_pending();
        if is_elevated() && is_ntfs(Path::new(root)) {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        self.dir_pending = Watcher::start(Path::new(root), WatchConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        })
        .ok();
    }

    fn drop_pending(&mut self) {
        if let Some(w) = self.dir_pending.take() {
            std::thread::spawn(move || w.stop());
        }
    }

    /// Stops following changes. The source may be mid-wait, so it is joined off the UI thread.
    fn drop_watch(&mut self) {
        if let Some(w) = self.watch.take() {
            if w.armed {
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_LIVE) };
            }
            std::thread::spawn(move || w.stop());
        }
    }

    /// `WM_JOURNAL`: moves what the source has queued into its queue and applies some of it.
    fn journal_event(&mut self) {
        self.posted.store(false, Ordering::Release);
        let ended = self.watch.as_mut().and_then(Watch::drain);
        self.apply_live();
        if let Some(e) = ended {
            self.watch_ended(e);
        }
    }

    /// Applies queued changes for at most `APPLY_BUDGET`; leftovers continue on the next message.
    fn apply_live(&mut self) {
        let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) else { return };
        let n = w.apply(tree, Instant::now() + APPLY_BUDGET);
        w.dirty |= n > 0;
        if w.queued() {
            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
        }
        if !w.armed && (w.dirty || w.backlog()) {
            unsafe { SetTimer(Some(self.hwnd), TIMER_LIVE, REPAINT.as_millis() as u32, None) };
            w.armed = true;
        }
    }

    /// `TIMER_LIVE`: at most every `REPAINT`, refreshes the rows once if the tree changed.
    fn live_tick(&mut self) {
        self.apply_live();
        let due = self.watch.as_ref().is_some_and(|w| w.dirty && w.pacer.due(Instant::now()));
        if due {
            let t0 = Instant::now();
            self.app.refresh_rows();
            self.app.live_changed(local_time());
            self.invalidate();
            if let Some(w) = &mut self.watch {
                w.pacer.done(Instant::now(), t0.elapsed());
                w.dirty = false;
            }
            if self.app.tree.as_ref().is_some_and(needs_compaction) {
                self.auto_rescan();
            }
        }
        if let Some(w) = &mut self.watch
            && w.armed
            && !w.dirty
            && !w.backlog()
        {
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_LIVE) };
            w.armed = false;
        }
    }

    /// The source ended: bring the tree up to date, then rescan or say why updates stopped.
    fn watch_ended(&mut self, e: Ended) {
        if let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) {
            w.apply(tree, Instant::now() + Duration::from_millis(50));
        }
        self.app.refresh_rows();
        self.drop_watch();
        self.app.set_live(LiveStatus::Off);
        let Some(why) = e.why else { return };
        if !(e.rescan && self.auto_rescan()) {
            self.app.set_live(LiveStatus::Stale(why));
        }
        self.invalidate();
    }
```

(Keep `journal_stopped`'s old body only if another caller uses it: grep first; delete it and its `Stop` handling otherwise. `Stop`, `Event` imports remain used by `Watch::drain`.)

- [ ] **Step 4: Start and install.** In `spawn_scan`, after the `self.progress = ...;` block add `self.start_dir_pending(&root);` (before `root` is moved into the thread closure). In `scan_done`, replace the `if let (Some((vol, info)), Some(mft)) = (sc.journal, sc.report.mft) { self.start_watch(vol, info, mft.records); }` with:

```rust
                if let (Some((vol, info)), Some(mft)) = (sc.journal, sc.report.mft) {
                    self.drop_pending();
                    self.start_watch(vol, info, mft.records);
                } else if let Some(w) = self.dir_pending.take() {
                    self.install_dir_watch(w);
                }
```

and in both error arms (`Err(e) if background` and `Err(e)`) call `self.drop_pending();` first. (`WM_DESTROY` only quits; the process exit ends the threads, so it needs nothing.)

- [ ] **Step 5: `selftest_watch`** (non-elevated; scans a temp directory so it needs no drive, then follows create, grow, rename, delete). Add after `selftest_live`:

```rust
/// Not elevated. Scans a temp directory in a hidden window, then creates, grows, renames and
/// deletes files there and checks the tree follows each change within 20 s.
/// Run it from a normal prompt: `plumb --selftest-watch`.
pub fn selftest_watch() -> Result<String> {
    use windows::Win32::Foundation::E_FAIL;
    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
    if is_elevated() {
        return Err(fail("the directory watch is for non-elevated runs; the journal serves elevated ones".into()));
    }
    let dir = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| fail(e.to_string()))?;
    let dir = dir.join(format!("plumb-watch-{}", std::process::id()));
    let shown = dir.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).map_err(|e| fail(e.to_string()))?;
    std::fs::write(dir.join("sub").join("seed.bin"), vec![1u8; 4096]).map_err(|e| fail(e.to_string()))?;
    let hwnd = create_window(false)?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    unsafe { SetTimer(Some(hwnd), 99, 50, None) }; // wakes the message wait so deadlines are checked
    let wait = |what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut msg = MSG::default();
        while with(|w| !cond(w)) == Some(true) {
            if Instant::now() > deadline {
                return Err(fail(format!("timed out waiting for: {what}")));
            }
            unsafe {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Ok(())
    };
    let size_at = |w: &mut Win, rel: &str| -> Option<u64> {
        let t = w.app.tree.as_ref()?;
        // The root node is named by the whole scanned path, so look up below it, not by path string.
        let rel: Vec<u16> = rel.encode_utf16().collect();
        let id = crate::dirsync::resolve(t, &rel)?;
        Some(t.subtree_size[id])
    };
    let letter = shown.chars().next().unwrap_or('C').to_ascii_uppercase();
    let run = || -> Result<String> {
        let io = |e: std::io::Error| fail(e.to_string());
        let i = with(|w| w.app.drives.iter().position(|d| d.letter == letter)).flatten();
        let i = i.ok_or_else(|| fail(format!("drive {letter}: is not in the drive list")))?;
        with(|w| w.spawn_scan(i, dir.to_string_lossy().into_owned(), false));
        wait("the scan and the directory watch", 60, &|w| w.watch.is_some())?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 1 << 20]).map_err(io)?;
        wait("the new file", 20, &|w| size_at(w, "f.bin") == Some(1 << 20))?;
        wait("the live marker", 20, &|w| matches!(&w.app.live, LiveStatus::On { last: Some(_) }))?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 3 << 20]).map_err(io)?;
        wait("the grown file", 20, &|w| size_at(w, "f.bin") == Some(3 << 20))?;
        std::fs::rename(dir.join("f.bin"), dir.join("g.bin")).map_err(io)?;
        wait("the rename", 20, &|w| size_at(w, "g.bin") == Some(3 << 20) && size_at(w, "f.bin").is_none())?;
        std::fs::create_dir_all(dir.join("new").join("deep")).map_err(io)?;
        std::fs::write(dir.join("new").join("deep").join("x.bin"), vec![1u8; 8192]).map_err(io)?;
        wait("the new folder", 20, &|w| size_at(w, "new\\deep\\x.bin") == Some(8192))?;
        std::fs::remove_dir_all(dir.join("sub")).map_err(io)?;
        wait("the delete", 20, &|w| size_at(w, "sub").is_none() && size_at(w, "g.bin") == Some(3 << 20))?;
        if with(|w| matches!(w.app.live, LiveStatus::Stale(_))).unwrap_or(false) {
            return Err(fail("live updates reported stale".into()));
        }
        Ok("create, grow, rename, new folder and delete followed".to_string())
    };
    let out = run();
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out
}
```

In `main.rs` add the arm `else if std::env::args().any(|a| a == "--selftest-watch") { plumb::window::selftest_watch().map(|a| println!("selftest-watch ok: {a}")) }`.

- [ ] **Step 6: Verify.** `cargo build -p plumb -j 8`, `cargo test --workspace -j 8`, then run `plumb.exe --selftest-watch` and `--selftest` (both print "ok"; 90 s timeout each; no admin). `cargo clippy --workspace --all-targets -j 8` shows no new warning in the files touched. Commit `git commit -am "Wire the directory watch into the window"`.

---

### Task 5: Docs and hand-off

- [ ] **Step 1:** `CHANGELOG.md`, under `[Unreleased]` / `### Added`:

```markdown
- `plumb`: live updates without admin. After a directory-walk scan (any drive when not elevated, or non-NTFS drives) the tree follows changes: new, grown, renamed and deleted files and folders appear within about a second and the status bar shows "live, last change HH:MM:SS". A recursive directory watch notes which folders changed; those are re-read and compared with the tree. Changes made while the scan was running are applied when it finishes. A buffer overflow or a flood of changes triggers the same background rescan as a wrapped journal. `plumb --selftest-watch` checks create, grow, rename, new folder and delete end to end.
- `plumb-scan`: `watch` module (directory watcher) and `listing` module (`Entry`, `Listing`).
```

and under Known limitations: "Directory-watch live updates see a file's size when the writer closes or flushes it; a renamed folder is re-listed (its expansion resets); hard links are not tracked; an elevated scan that falls back to the walker has no live updates; a watch that cannot be opened leaves the tree static without a message."

- [ ] **Step 2:** Spec: set `Status: implemented, 2026-10-12 (plan: ...)` and add `## Amendments made while implementing` with the four items from the top of this plan.
- [ ] **Step 3:** `cargo test --workspace -j 8`, `git diff --stat <first commit of this plan>^`, commit `git add -A docs CHANGELOG.md && git commit -m "Document the directory watch"`.
- [ ] **Step 4: Manual check for the user (non-admin).** (a) Scan `C:`; leave plumb open; save a file in a folder you have expanded: its size appears within about a second and the bar shows "live". (b) Create and delete a folder with files in Explorer: it appears and vanishes. (c) Start a large copy or a build: the tree keeps up; note whether "the directory watch overflowed" or "too many folders changed" ever shows (then `max_dirty` or the 64 KB buffer is the knob). (d) Elevated scan of `C:` still shows the journal behavior.

---

## Self-review

- **Spec coverage:** reader/lister/hold/release/overflow/TooMany (Task 3); applier incl. kind change, case folding, nested new directories, idempotence, unresolvable, budget (Task 2); window wiring with pending watcher at scan start, install after swap, background rescan with the old watch running until the swap, error arms drop the pending one (Task 4); selftest and docs (Tasks 4, 5). The "elevated walker fallback has no watch" and "elevated NTFS gets the journal" rules are both in `start_dir_pending`.
- **Placeholders:** none; the two "fix by tracing" notes in Tasks 2 and 3 name the exact thing to check (the `R` subtree sum, the Win32 feature name).
- **Types:** `Entry`/`Listing` (Task 1) are used in Tasks 2, 3, 4; `dirsync::apply` (Task 2) in `Watch::apply`; `Watcher::{start, try_recv, release, stop}`, `WatchConfig`, `Event`, `Stop` (Task 3) in Task 4; `Ended`, `Source`, `Watch::{drain, apply, queued, backlog, stop}` are defined and used within Task 4.

## Stress-test log

Two rounds against the real code (`walk.rs`, `window.rs`, `tree.rs`, `node.rs`, `restore.rs`), not compiled.

1. **Round 1 caught:** the walker names the root node by the whole scanned path (backslashes included), so `restore::resolve` cannot find paths in `selftest_watch` (now `dirsync::resolve`, made `pub(crate)`); the selftest passed drive index 0 instead of the temp directory's drive; the watched root was not canonicalized (long paths would fail to list); the applier tests used `Entry` and `TreeBuilder` without importing them; one test asserted `R` = 0 where `b.txt` still counts 4096; a "fix by tracing" note that was really a known value; a nonexistent `WM_DESTROY` drop.
2. **Round 2 caught:** a watch that cannot be opened leaves the tree static silently (accepted, recorded as a known limit). Checked and fine: held-phase overflow is delivered after install; foreground and background rescans replace the pending watcher; elevated-NTFS-walker-fallback gets no watch; `Watcher::stop` after `Drop`; synchronous `ReadDirectoryChangesW` cancels via `CancelIoEx`.

Residual risk: compile-level details (windows 0.62 signatures for `ReadDirectoryChangesW`, `GetFileAttributesW`) are traced, not built; the executor fixes them in Task 3 step 3.
