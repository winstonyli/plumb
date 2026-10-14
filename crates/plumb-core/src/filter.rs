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

    /// Where each non-empty match lies in `name`, as byte offsets into it, for highlighting.
    pub fn find<'a>(&'a self, name: &'a str) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
        self.0.find_iter(name).map(|m| m.range()).filter(|r| !r.is_empty())
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
