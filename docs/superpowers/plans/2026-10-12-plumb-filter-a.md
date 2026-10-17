---
status: done
---

# Name filter, plan A: the core (matcher, hits, keep set, row model)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `plumb-core` can compile a name query, scan a snapshot of the names off-thread, turn the hits into a keep set (matches plus ancestors) and make `RowModel` show only that set, with dimmed rows below a leaf match.

**Architecture:** A new `filter` module owns `Query`/`Matcher` (a case-insensitive `regex::Regex`, substring text escaped), `Names` (a clone of the tree's name arrays a worker can own), `Hits` (a bitset of name matches) and `Keep` (live hits plus all their ancestors). `RowModel` holds an optional `Rc<Keep>`; `push_children` skips children outside it where the parent is an ancestor of a match. A small `bits` helper shares the bitset code. Nothing here touches the window; plan B wires the worker, the strip and text input.

**Tech Stack:** Rust 2024, `regex` 1.13 (already in `Cargo.lock` via criterion and in the local cargo cache), criterion for the benchmark.

**Spec:** `docs/superpowers/specs/2026-10-12-plumb-filter-design.md`

## Global Constraints

- Names are UTF-16 (`Tree::name` returns `&[u16]`); matching converts to UTF-8 lossily.
- Matching is case-insensitive in both modes; substring mode escapes the text (`regex::escape`).
- An empty query means no filter (`compile` returns `Ok(None)`).
- Dead nodes (`NodeFlags::DEAD`) are never hits and never kept.
- `RowModel::new` and every existing `RowModel` behaviour are unchanged while no filter is set.
- Files are edited with the Edit tool (working copies are CRLF); new files are written with the Write tool.
- Run cargo with `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3` and at most `-j 8`.
- No stray processes: every long command is run to completion, nothing is left in the background.

## Spec refinement made while planning

The spec says a folder that matches is shown collapsed and, when opened, shows its non-matching children dimmed. A folder that matches *and* contains matches is an ancestor, and ancestors show only kept children. So the dimmed view appears only under a **leaf match** (a matching folder with no matching descendants) or below an already hidden node. Task 3 records this in the spec.

## File structure

- Create `crates/plumb-core/src/bits.rs`: `get`/`set` on `Vec<u64>` bitsets.
- Create `crates/plumb-core/src/filter.rs` and `filter/tests.rs`: the module.
- Modify `crates/plumb-core/src/tree.rs`: `name_arrays`, `touched`.
- Modify `crates/plumb-core/src/rows.rs`, create `rows/filter_tests.rs`: filter support.
- Modify `crates/plumb-core/src/lib.rs`, `Cargo.toml` (workspace), `crates/plumb-core/Cargo.toml`, `benches/core.rs`, `CHANGELOG.md`, the spec.

---

### Task 1: The filter module

**Files:**
- Create: `crates/plumb-core/src/bits.rs`, `crates/plumb-core/src/filter.rs`, `crates/plumb-core/src/filter/tests.rs`
- Modify: `Cargo.toml`, `crates/plumb-core/Cargo.toml`, `crates/plumb-core/src/lib.rs`, `crates/plumb-core/src/tree.rs`

**Interfaces:**
- Produces (used by Task 2 and plan B):
  - `Query { pub text: String, pub regex: bool }`, `Query::compile(&self) -> Result<Option<Matcher>, FilterError>`
  - `FilterError::{Invalid(String), TooComplex}`
  - `Matcher::is_match(&self, name: &[u16], buf: &mut String) -> bool` (`Clone`, `Send`)
  - `Names::of(&Tree) -> Names` (`Send`), `Names::len`, `Names::is_empty`, `Names::scan(&self, &Matcher, cancel: &AtomicU64, generation: u64) -> Option<Hits>`
  - `Hits::{len, get(NodeId), set(NodeId, bool), count, update(&mut self, &Tree, &Matcher, touched: &[NodeId])}` (`Send`)
  - `Keep::{build(&Tree, &Hits) -> Keep, is_hit, is_ancestor, is_kept, matches() -> usize}`
  - `Tree::touched(&self) -> &[NodeId]`

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` (workspace), under `[workspace.dependencies]` after `bitflags = "2"` add:

```toml
regex = "1.13"
```

In `crates/plumb-core/Cargo.toml`, under `[dependencies]` after `bitflags.workspace = true` add:

```toml
regex.workspace = true
```

- [ ] **Step 2: Create `crates/plumb-core/src/bits.rs`**

```rust
//! Bitsets over node indices, stored as `Vec<u64>`. Reading past the end gives `false`; writing
//! past the end grows the vector.

pub(crate) fn get(v: &[u64], i: usize) -> bool {
    v.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
}

pub(crate) fn set(v: &mut Vec<u64>, i: usize, on: bool) {
    let (w, b) = (i / 64, i % 64);
    if w >= v.len() {
        v.resize(w + 1, 0);
    }
    if on {
        v[w] |= 1 << b;
    } else {
        v[w] &= !(1 << b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_set_and_growth() {
        let mut v = Vec::new();
        assert!(!get(&v, 1000));
        set(&mut v, 130, true);
        assert!(get(&v, 130) && !get(&v, 129) && !get(&v, 131));
        set(&mut v, 130, false);
        assert!(!get(&v, 130));
        set(&mut v, 5000, false);
        assert!(!get(&v, 5000));
    }
}
```

- [ ] **Step 3: Edit `crates/plumb-core/src/lib.rs`**

Replace its module list with:

```rust
pub(crate) mod bits;
pub mod filter;
pub mod mft;
pub mod node;
pub mod rows;
#[doc(hidden)]
pub mod synth;
pub mod tree;
```

(The `pub use` lines stay.)

- [ ] **Step 4: Edit `crates/plumb-core/src/tree.rs`**

After `take_touched` (the function ending `std::mem::take(&mut self.touched)\n    }`), add:

```rust

    /// Ids changed since the last [`Tree::take_touched`], without consuming them.
    pub fn touched(&self) -> &[NodeId] {
        &self.touched
    }

    /// The three name arrays, for [`crate::filter::Names`].
    pub(crate) fn name_arrays(&self) -> (&[u32], &[u16], &[u16]) {
        (&self.name_off, &self.name_len, &self.pool)
    }
```

- [ ] **Step 5: Create `crates/plumb-core/src/filter.rs`**

```rust
//! Name filter: a compiled query, a snapshot of the names that a worker thread can scan, and the
//! set of nodes to keep (matches plus their ancestors).

use crate::bits;
use crate::node::{NodeFlags, NodeId};
use crate::tree::Tree;
use regex::{Regex, RegexBuilder};
use std::sync::atomic::{AtomicU64, Ordering};

/// What the user typed. In substring mode the text is matched literally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterError {
    /// The pattern does not parse; the message is the regex engine's.
    Invalid(String),
    /// The pattern compiles to more than the engine's size limit.
    TooComplex,
}

/// A compiled, case-insensitive pattern.
#[derive(Clone, Debug)]
pub struct Matcher(Regex);

impl Query {
    /// `Ok(None)` for an empty query, which means "no filter".
    pub fn compile(&self) -> Result<Option<Matcher>, FilterError> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let pattern = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .map(|r| Some(Matcher(r)))
            .map_err(|e| match e {
                regex::Error::CompiledTooBig(_) => FilterError::TooComplex,
                e => FilterError::Invalid(e.to_string()),
            })
    }
}

impl Matcher {
    /// Matches one UTF-16 name. `buf` is scratch space reused between calls; unpaired surrogates
    /// become U+FFFD.
    pub fn is_match(&self, name: &[u16], buf: &mut String) -> bool {
        buf.clear();
        buf.extend(char::decode_utf16(name.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
        self.0.is_match(buf)
    }
}

/// A copy of the tree's name storage that another thread can own while the tree keeps changing.
pub struct Names {
    off: Vec<u32>,
    len: Vec<u16>,
    pool: Vec<u16>,
}

impl Names {
    pub fn of(tree: &Tree) -> Names {
        let (off, len, pool) = tree.name_arrays();
        Names { off: off.to_vec(), len: len.to_vec(), pool: pool.to_vec() }
    }

    pub fn len(&self) -> usize {
        self.off.len()
    }

    pub fn is_empty(&self) -> bool {
        self.off.is_empty()
    }

    fn name(&self, i: usize) -> &[u16] {
        let o = self.off[i] as usize;
        &self.pool[o..o + self.len[i] as usize]
    }

    /// Tests every name. Returns `None` as soon as `cancel` no longer equals `generation`
    /// (checked every 65 536 names, and first thing).
    pub fn scan(&self, m: &Matcher, cancel: &AtomicU64, generation: u64) -> Option<Hits> {
        let n = self.len();
        let mut hits = Hits { bits: vec![0; n.div_ceil(64)], len: n };
        let mut buf = String::new();
        for i in 0..n {
            if i % 65_536 == 0 && cancel.load(Ordering::Relaxed) != generation {
                return None;
            }
            if m.is_match(self.name(i), &mut buf) {
                bits::set(&mut hits.bits, i, true);
            }
        }
        Some(hits)
    }
}

/// One bit per node: its name matches. Dead nodes may still carry a bit; [`Keep::build`] skips them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    bits: Vec<u64>,
    len: usize,
}

impl Hits {
    /// Number of nodes covered.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, id: NodeId) -> bool {
        bits::get(&self.bits, id.idx())
    }

    pub fn set(&mut self, id: NodeId, on: bool) {
        bits::set(&mut self.bits, id.idx(), on);
        self.len = self.len.max(id.idx() + 1);
    }

    /// Number of set bits.
    pub fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Brings the hits up to date with `tree`: tests every node past the end of the covered range
    /// (added since the scan), then re-tests the `touched` ones. Dead nodes get no bit.
    pub fn update(&mut self, tree: &Tree, m: &Matcher, touched: &[NodeId]) {
        let mut buf = String::new();
        let mut test = |hits: &mut Hits, id: NodeId| {
            let on = !tree.flags[id].contains(NodeFlags::DEAD) && m.is_match(tree.name(id), &mut buf);
            hits.set(id, on);
        };
        for i in self.len..tree.len() {
            test(self, NodeId(i as u32));
        }
        for &id in touched {
            if !id.is_none() && id.idx() < tree.len() {
                test(self, id);
            }
        }
    }
}

/// The nodes a filtered view keeps: live matches, and every ancestor of a live match.
#[derive(Clone, Debug, Default)]
pub struct Keep {
    hit: Vec<u64>,
    anc: Vec<u64>,
    count: usize,
}

impl Keep {
    /// O(live hits + kept nodes): the walk up from a hit stops at the first ancestor already marked.
    pub fn build(tree: &Tree, hits: &Hits) -> Keep {
        let n = tree.len();
        let words = n.div_ceil(64);
        let mut k = Keep { hit: vec![0; words], anc: vec![0; words], count: 0 };
        for (wi, &word) in hits.bits.iter().enumerate() {
            let mut w = word;
            while w != 0 {
                let i = wi * 64 + w.trailing_zeros() as usize;
                w &= w - 1;
                if i >= n {
                    continue;
                }
                let id = NodeId(i as u32);
                if tree.flags[id].contains(NodeFlags::DEAD) {
                    continue;
                }
                bits::set(&mut k.hit, i, true);
                k.count += 1;
                let mut cur = tree.parent[id];
                while !cur.is_none() && !bits::get(&k.anc, cur.idx()) {
                    bits::set(&mut k.anc, cur.idx(), true);
                    cur = tree.parent[cur];
                }
            }
        }
        k
    }

    pub fn is_hit(&self, id: NodeId) -> bool {
        bits::get(&self.hit, id.idx())
    }

    /// True if some live match is strictly below `id`.
    pub fn is_ancestor(&self, id: NodeId) -> bool {
        bits::get(&self.anc, id.idx())
    }

    pub fn is_kept(&self, id: NodeId) -> bool {
        self.is_hit(id) || self.is_ancestor(id)
    }

    /// Number of live matches.
    pub fn matches(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 6: Create `crates/plumb-core/src/filter/tests.rs`**

```rust
use super::*;
use crate::node::NodeFlags;
use crate::tree::tests::sample;

// sample(): 0=C:, 1=big, 2=f1, 3=f2, 4=mid, 5=g, 6=small
fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn matcher(text: &str, regex: bool) -> Matcher {
    Query { text: text.into(), regex }.compile().unwrap().unwrap()
}

fn hit_ids(h: &Hits) -> Vec<u32> {
    (0..h.len() as u32).filter(|&i| h.get(NodeId(i))).collect()
}

fn scan(t: &Tree, m: &Matcher) -> Hits {
    Names::of(t).scan(m, &AtomicU64::new(1), 1).unwrap()
}

#[test]
fn substring_mode_escapes_the_text_and_ignores_case() {
    let mut buf = String::new();
    let m = matcher("c++", false);
    assert!(m.is_match(&u("ABC++x"), &mut buf));
    assert!(!m.is_match(&u("abc"), &mut buf));
    assert!(matcher("file(1)", false).is_match(&u("My File(1).txt"), &mut buf));
}

#[test]
fn regex_mode_uses_the_pattern() {
    let mut buf = String::new();
    let m = matcher(r"^img_\d+$", true);
    assert!(m.is_match(&u("IMG_42"), &mut buf));
    assert!(!m.is_match(&u("img_x"), &mut buf));
}

#[test]
fn matching_folds_case_beyond_ascii() {
    let mut buf = String::new();
    assert!(matcher("ÉCOLE", false).is_match(&u("école.txt"), &mut buf));
}

#[test]
fn surrogate_pairs_match_and_lone_surrogates_do_not_panic() {
    let mut buf = String::new();
    let m = matcher("\u{1F4C1}", false);
    assert!(m.is_match(&u("\u{1F4C1}docs"), &mut buf));
    assert!(!m.is_match(&[0x61, 0xD800, 0x62], &mut buf));
}

#[test]
fn an_empty_query_is_no_filter() {
    assert!(Query { text: String::new(), regex: true }.compile().unwrap().is_none());
}

#[test]
fn bad_patterns_report_why() {
    assert!(matches!(Query { text: "(".into(), regex: true }.compile(), Err(FilterError::Invalid(_))));
    assert!(matches!(Query { text: "[a".into(), regex: true }.compile(), Err(FilterError::Invalid(_))));
    assert_eq!(
        Query { text: r"((\w{100}){100}){100}".into(), regex: true }.compile().unwrap_err(),
        FilterError::TooComplex
    );
}

#[test]
fn scan_finds_the_matching_names() {
    let t = sample();
    assert_eq!(hit_ids(&scan(&t, &matcher("m", false))), vec![4, 6]); // mid, small
    assert!(scan(&t, &matcher("zzz", false)).count() == 0);
}

#[test]
fn scan_stops_when_the_generation_moves_on() {
    let t = sample();
    let cancel = AtomicU64::new(2);
    assert!(Names::of(&t).scan(&matcher("m", false), &cancel, 1).is_none());
}

#[test]
fn keep_is_the_matches_and_their_ancestors() {
    let t = sample();
    let k = Keep::build(&t, &scan(&t, &matcher("f", false))); // f1, f2
    let kept: Vec<u32> = (0..7).filter(|&i| k.is_kept(NodeId(i))).collect();
    assert_eq!(kept, vec![0, 1, 2, 3]);
    assert!(k.is_ancestor(NodeId(1)) && !k.is_ancestor(NodeId(2)));
    assert!(k.is_hit(NodeId(2)) && !k.is_hit(NodeId(1)));
    assert_eq!(k.matches(), 2);
}

#[test]
fn a_matching_folder_with_a_matching_child_is_both() {
    let mut t = sample();
    t.add_child(NodeId(1), &u("bigger"), 1, NodeFlags::empty()); // id 7, under big
    let k = Keep::build(&t, &scan(&t, &matcher("big", false)));
    assert!(k.is_hit(NodeId(1)) && k.is_hit(NodeId(7)));
    assert!(k.is_ancestor(NodeId(1)), "big matches and so does something below it");
    assert!(k.is_ancestor(NodeId(0)) && !k.is_ancestor(NodeId(7)));
    assert!(!k.is_kept(NodeId(2)), "its other children are not kept");
}

#[test]
fn dead_nodes_are_never_kept() {
    let mut t = sample();
    let h = scan(&t, &matcher("f", false));
    t.remove(NodeId(2));
    let k = Keep::build(&t, &h);
    assert!(!k.is_kept(NodeId(2)));
    assert!(k.is_kept(NodeId(3)) && k.is_ancestor(NodeId(1)));
    t.remove(NodeId(3));
    let k = Keep::build(&t, &h);
    assert_eq!(k.matches(), 0);
    assert!(!k.is_kept(NodeId(0)));
}

#[test]
fn update_follows_adds_renames_and_removals() {
    let mut t = sample();
    let m = matcher("f", false);
    let mut h = scan(&t, &m);
    assert_eq!(hit_ids(&h), vec![2, 3]);
    let new = t.add_child(NodeId(4), &u("ff"), 5, NodeFlags::empty());
    t.rename(NodeId(5), &u("f-g"));
    t.remove(NodeId(2));
    h.update(&t, &m, t.touched());
    assert_eq!(hit_ids(&h), vec![3, 5, new.0]);
    assert_eq!(h.len(), t.len());
    t.rename(NodeId(5), &u("g"));
    h.update(&t, &m, t.touched());
    assert!(!h.get(NodeId(5)));
}

#[test]
fn update_covers_nodes_added_after_the_scan_even_if_untouched() {
    let mut t = sample();
    let m = matcher("f", false);
    let h0 = scan(&t, &m);
    t.add_child(NodeId(0), &u("fz"), 1, NodeFlags::empty());
    let _ = t.take_touched();
    let mut h = h0;
    h.update(&t, &m, &[]);
    assert!(h.get(NodeId(7)));
}
```

- [ ] **Step 7: Run the tests**

Run: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3 cargo test -p plumb-core -j 8 filter bits`
Expected: all pass. If `bad_patterns_report_why` returns `Invalid` for the too-complex pattern, replace its pattern with `((\w{100}){100}){100}{100}` (a larger one) and note it in the commit message; if the regex crate reports nothing but succeeds slowly, switch the assertion to the first two bad patterns only and record why.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "Add the filter module: query, matcher, name snapshot, hits and keep set"
```

---

### Task 2: Filtered rows

**Files:**
- Modify: `crates/plumb-core/src/rows.rs`
- Create: `crates/plumb-core/src/rows/filter_tests.rs`

**Interfaces:**
- Consumes: `Keep` from Task 1.
- Produces: `Row.dim: bool`; `RowModel::set_filter(&mut self, tree: &Tree, keep: Option<Rc<Keep>>)`; `RowModel::is_expanded` is filter-aware.

- [ ] **Step 1: Edit imports and `Row` in `rows.rs`**

Replace `use crate::node::{NodeFlags, NodeId};` with:

```rust
use crate::bits;
use crate::filter::Keep;
use crate::node::{NodeFlags, NodeId};
```

Replace the `Row` struct with:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub id: NodeId,
    pub depth: u16,
    /// Shown only because the user opened a folder under a filter; it does not match.
    pub dim: bool,
}
```

- [ ] **Step 2: Edit the `RowModel` struct and `new`**

Add two fields after `cache`:

```rust
    /// The active filter, if any.
    filter: Option<Rc<Keep>>,
    /// Ancestors of matches the user collapsed while a filter is active (they are open by default).
    closed: Vec<u64>,
```

In `RowModel::new`, add `filter: None,` and `closed: Vec::new(),` after `cache: HashMap::new(),`.

- [ ] **Step 3: Replace `is_expanded` and `set_expanded`**

```rust
    pub fn is_expanded(&self, id: NodeId) -> bool {
        match &self.filter {
            Some(k) if k.is_ancestor(id) => !bits::get(&self.closed, id.idx()),
            _ => bits::get(&self.expanded, id.idx()),
        }
    }

    fn set_expanded(&mut self, id: NodeId, on: bool) {
        if self.filter.as_ref().is_some_and(|k| k.is_ancestor(id)) {
            bits::set(&mut self.closed, id.idx(), !on);
        } else {
            bits::set(&mut self.expanded, id.idx(), on);
        }
    }
```

- [ ] **Step 4: Replace `push_children`**

```rust
    fn push_children(&mut self, tree: &Tree, parent: NodeId, depth: u16, out: &mut Vec<Row>) {
        let kids = self.sorted_children(tree, parent);
        let filter = self.filter.clone();
        // Under a filter the roots and the children of an ancestor of a match are cut down to the
        // kept ones. Below a leaf match, or below a node the filter hides, everything shows, dimmed.
        let cut = filter.as_ref().is_some_and(|k| parent.is_none() || k.is_ancestor(parent));
        for &c in kids.iter() {
            let dim = filter.as_ref().is_some_and(|k| !k.is_kept(c));
            if dim && cut {
                continue;
            }
            out.push(Row { id: c, depth, dim });
            if self.is_expanded(c) {
                self.push_children(tree, c, depth + 1, out);
            }
        }
    }
```

- [ ] **Step 5: Add `set_filter`** after `set_sort`

```rust
    /// Installs (or, with `None`, removes) a filter and rebuilds the rows. A selection the filter
    /// hides moves to its nearest visible ancestor, or is cleared. Folders the user closed under an
    /// earlier filter stay closed while a filter stays active; they reopen when it is removed or
    /// replaces none.
    pub fn set_filter(&mut self, tree: &Tree, keep: Option<Rc<Keep>>) {
        if self.filter.is_none() || keep.is_none() {
            self.closed.clear();
        }
        self.filter = keep;
        self.rebuild(tree);
        while let Some(s) = self.selected {
            if self.row_index(s).is_some() {
                break;
            }
            let p = tree.parent[s];
            self.selected = (!p.is_none()).then_some(p);
        }
    }
```

- [ ] **Step 6: Register the test module**

At the bottom of `rows.rs`, after `mod refresh_tests;` add:

```rust

#[cfg(test)]
mod filter_tests;
```

- [ ] **Step 7: Create `crates/plumb-core/src/rows/filter_tests.rs`**

```rust
use super::*;
use crate::filter::{Hits, Names, Query};
use crate::synth::kary_builder;
use crate::tree::tests::sample;
use std::collections::HashSet;
use std::sync::atomic::AtomicU64;

// sample(): 0=C:, 1=big, 2=f1(100), 3=f2(300), 4=mid, 5=g(150), 6=small(50)
fn ids(m: &RowModel) -> Vec<u32> {
    m.rows().iter().map(|r| r.id.0).collect()
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
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
```

Note: in the last test `ff` (id 7) lives under `mid`, so `mid` becomes an ancestor and shows only `ff`; `big` keeps only `f2`. Sizes: big 300, mid 5 + 150 = 155.

- [ ] **Step 8: Run all core tests**

Run: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3 cargo test -p plumb-core -j 8`
Expected: everything passes (55 old tests + the new ones). If `a_selection_the_filter_hides_...` fails on its second half, the cause is `set_filter` keeping a selection that is still visible; verify by printing `ids(&m)`.

- [ ] **Step 9: Mutation check (do not commit)**

In `push_children` change `if dim && cut` to `if dim && false`; run `cargo test -p plumb-core filter_tests`. Expected: several tests fail (`a_filter_shows_matches...`, the oracle). Restore with `git checkout crates/plumb-core/src/rows.rs`-style revert of that one line only (use the Edit tool), then rerun and confirm all pass.

- [ ] **Step 10: Run the workspace tests and clippy on the changed crate**

Run: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3 cargo test --workspace -j 8` then `cargo clippy -p plumb-core -j 8`
Expected: tests pass; clippy shows no new warnings in `filter.rs`, `bits.rs`, `rows.rs` (old warnings elsewhere stay).

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "Filter the row model: keep set, auto-open ancestors, dimmed rows below leaf matches"
```

---

### Task 3: Benchmark, spec and changelog

**Files:**
- Modify: `crates/plumb-core/benches/core.rs`, `docs/superpowers/specs/2026-10-12-plumb-filter-design.md`, `CHANGELOG.md`

- [ ] **Step 1: Add the benchmark**

In `benches/core.rs`, add to the imports `use plumb_core::filter::{Keep, Names, Query};` and `use std::sync::atomic::AtomicU64;`. Before `criterion_group!` add:

```rust
fn bench_filter(c: &mut Criterion) {
    let mut g = c.benchmark_group("filter");
    g.sample_size(10);
    for n in [1_000_000u32, 5_000_000] {
        let tree = flat_tree(n);
        let sub = Query { text: "n99".into(), regex: false }.compile().unwrap().unwrap();
        let re = Query { text: r"^n9\d*7$".into(), regex: true }.compile().unwrap().unwrap();
        let names = Names::of(&tree);
        let cancel = AtomicU64::new(1);
        g.bench_function(format!("snapshot_{n}"), |b| b.iter(|| black_box(Names::of(&tree))));
        g.bench_function(format!("scan_substring_{n}"), |b| b.iter(|| black_box(names.scan(&sub, &cancel, 1))));
        g.bench_function(format!("scan_regex_{n}"), |b| b.iter(|| black_box(names.scan(&re, &cancel, 1))));
        let hits = names.scan(&sub, &cancel, 1).unwrap();
        g.bench_function(format!("keep_{n}"), |b| b.iter(|| black_box(Keep::build(&tree, &hits))));
    }
    g.finish();
}
```

Change the `criterion_group!` line to include `bench_filter`.

- [ ] **Step 2: Run it**

Run: `CARGO_TARGET_DIR=C:/Users/WINSTO~1/AppData/Local/Temp/t3 cargo bench -p plumb-core -j 8 -- filter` (expect a few minutes; the 5M tree builds once per size; do not run other heavy jobs meanwhile). Record the times for snapshot, both scans and keep at 1M and 5M. Decision rule from the spec: if `snapshot_5000000` is over 30 ms, or `scan_regex_5000000` over 1 s, note it as a risk for plan B (the worker hides scan cost; the snapshot runs on the UI thread).

- [ ] **Step 3: Record results and the spec refinement**

In the spec, in "Row model", after the line about a user-opened hit folder, add: `A matching folder that also contains matches is an ancestor and shows only kept children; the dimmed view appears only under a leaf match or below a hidden node.` In the spec's "Risks" section, add the measured numbers from Step 2 on one line.

In `CHANGELOG.md` under `## [Unreleased]` add:

```markdown
### Added
- `plumb-core`: `filter` module (case-insensitive substring or regex name matching with the `regex` crate, a name snapshot for worker threads, hits and the keep set) and `RowModel::set_filter`, which shows matches and their ancestors and dims rows under a leaf match. Not yet wired into the window.
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Benchmark the filter; record the leaf-match rule and measured costs"
```

---

## Self-review

- **Spec coverage:** matcher and modes, invalid/too-complex patterns, snapshot, generation cancel, hits, keep set, auto-open ancestors, user expansion kept aside, leaf match collapsed and dimmed, selection fallback, live update via `Hits::update` and the `touched` peek, benchmark: all have a task. Plan B covers the worker/`WM_FILTER`, debounce, in-flight touched tracking, the strip, text input, Ctrl+F/Esc, dim drawing, status text, restore snapshot on clear (it lives in the `plumb` crate), IME note and the changelog entry for users.
- **Placeholders:** none.
- **Types:** `Keep::is_ancestor/is_hit/is_kept/matches`, `Hits::{get,set,update,count,len}`, `Names::{of,scan}`, `Query::compile`, `RowModel::set_filter(&Tree, Option<Rc<Keep>>)`, `Row.dim` are used consistently across tasks. `Hits` covers `Send` because it is plain vectors; `Matcher` is `Send + Sync` because `Regex` is.
- **Known risk:** the too-complex pattern in the test depends on the regex size limit; Step 7 of Task 1 says what to do if it does not trip.
