//! What the UI says about one row or tile: the facts are gathered once into a `Description` (a file or folder, or a
//! process), and the selection line, the map tooltip and the row tooltip are three renderings of it, in one fixed order.
//! It holds values that format lazily, so a tooltip is built without allocating.
use crate::app::ProcInfo;
use crate::attrs::Attrs;
use crate::fmt::{self, Show};
use plumb_core::{NodeFlags, NodeId, Tree};
use std::fmt::Write as _;

/// When a thing last changed: "modified" for a file or folder, "started" for a process.
#[derive(Clone, Copy, Debug)]
pub struct When {
    pub label: &'static str,
    pub day: u32,
    pub now: u32,
}

#[derive(Clone)]
pub struct Description<'a> {
    /// The path of a file or folder, the name of a process.
    pub who: Show<'a>,
    /// A process's id; `None` for a file or folder.
    pub pid: Option<u32>,
    pub size: Show<'static>,
    /// What a process's size counts and over what: ("memory", "this process").
    pub scope: Option<(&'static str, &'static str)>,
    /// A folder's item count.
    pub items: Option<Show<'static>>,
    pub when: Option<When>,
    /// A process's own CPU (tenths of a percent) and I/O (bytes a second).
    pub numbers: Option<(u64, u64)>,
    /// A process's executable.
    pub exe: Option<&'a str>,
    pub attrs: Attrs,
}

/// A file or folder `id` of `tree`, whose size is `bytes`; `now` is today as a day count.
pub fn file(tree: &Tree, id: NodeId, bytes: u64, now: u32, attrs: Attrs) -> Description<'_> {
    Description {
        who: fmt::node_path(tree, id),
        pid: None,
        size: fmt::size(bytes),
        scope: None,
        items: tree.flags[id].contains(NodeFlags::DIR).then(|| fmt::noun(tree.subtree_files[id] as u64, "item", "items")),
        when: tree.mtime[id].map(|d| When { label: "modified", day: d.get(), now }),
        numbers: None,
        exe: None,
        attrs,
    }
}

/// A process `id` of the Processes tree, whose size (`size`, counted in `metric`) is already worded; `now` is today as a
/// local day count.
pub fn process<'a>(tree: &'a Tree, id: NodeId, size: Show<'static>, metric: &'static str, info: Option<&'a ProcInfo>, now: u32, attrs: Attrs) -> Description<'a> {
    let [cpu, io, pid] = tree.aux.get(id.idx()).copied().unwrap_or_default();
    Description {
        who: fmt::node_name(tree, id),
        pid: Some(pid as u32),
        size,
        scope: Some((metric, if tree.flags[id].contains(NodeFlags::DIR) { "this and its children" } else { "this process" })),
        items: None,
        when: info.filter(|i| i.created > 0).map(|i| When { label: "started", day: fmt::local_day(i.created), now }),
        numbers: Some((cpu, io)),
        exe: info.and_then(|i| i.path.as_deref()),
        attrs,
    }
}

impl Description<'_> {
    fn write_who(&self, out: &mut String) {
        let _ = match self.pid {
            Some(pid) => write!(out, "{}  (PID {pid})", self.who),
            None => write!(out, "{}", self.who),
        };
    }

    /// The selection line: the size and item count (led by the path when `with_who`; a process is always named), then
    /// the attributes.
    pub fn write_line(&self, with_who: bool, out: &mut String) {
        if self.pid.is_some() {
            self.write_who(out);
            out.push_str("  ");
        } else if with_who {
            self.write_who(out);
            out.push_str("  (");
        }
        let _ = write!(out, "{}", self.size);
        if let Some(items) = &self.items {
            let _ = write!(out, ", {items}");
        }
        if self.pid.is_none() && with_who {
            out.push(')');
        }
        if !self.attrs.is_empty() {
            let _ = write!(out, "  {}", self.attrs.words());
        }
    }

    /// The tooltip: who, then the facts, the executable, and the attributes (as words, or with `meanings` one a line).
    pub fn write_tip(&self, meanings: bool, out: &mut String) {
        self.write_who(out);
        let _ = write!(out, "\n{}", self.size);
        if let Some((metric, scope)) = self.scope {
            let _ = write!(out, " {metric} in {scope}");
        }
        if let Some(items) = &self.items {
            let _ = write!(out, ", {items}");
        }
        if let Some(w) = self.when {
            let _ = write!(out, ", {} {}, {}", w.label, fmt::date(w.day), fmt::age(w.now, w.day));
        }
        if let Some((cpu, io)) = self.numbers {
            let _ = write!(out, "; own CPU {}, I/O {}", fmt::tenths(cpu), fmt::rate(io));
        }
        if let Some(exe) = self.exe {
            let _ = write!(out, "\n{exe}");
        }
        if meanings {
            for a in self.attrs.iter() {
                let _ = write!(out, "\n{}: {}", a.word(), a.meaning());
            }
        } else if !self.attrs.is_empty() {
            let _ = write!(out, "\n{}", self.attrs.words());
        }
    }

    #[cfg(test)]
    pub fn line(&self, with_who: bool) -> String {
        let mut s = String::new();
        self.write_line(with_who, &mut s);
        s
    }

    #[cfg(test)]
    pub fn tip(&self, meanings: bool) -> String {
        let mut s = String::new();
        self.write_tip(meanings, &mut s);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs::Attr;
    use plumb_core::synth::flat_tree;

    #[test]
    fn a_file_reads_the_same_in_the_selection_line_and_the_tip() {
        let tree = flat_tree(3);
        let d = file(&tree, NodeId(1), 2048, 150_000, Attrs::default());
        let path = fmt::node_path(&tree, NodeId(1)).to_string();
        assert_eq!(d.line(true), format!("{path}  (2.0 KB)"));
        assert_eq!(d.line(false), "2.0 KB");
        assert_eq!(d.tip(false), format!("{path}\n2.0 KB"));
    }

    #[test]
    fn the_facts_come_in_one_order_for_a_process() {
        let tree = flat_tree(3);
        let info = ProcInfo { path: Some("C:\\Windows\\ping.exe".into()), created: 0, handles: 0, suspended: false, elevated: false };
        let d = Description { pid: Some(77), scope: Some(("memory", "this process")), numbers: Some((5, 2048)), exe: info.path.as_deref(), ..file(&tree, NodeId(1), 4096, 1, Attrs::default()) };
        let name = fmt::node_path(&tree, NodeId(1)).to_string();
        assert_eq!(d.tip(false), format!("{name}  (PID 77)\n4.0 KB memory in this process; own CPU 0.5%, I/O 2.0 KB/s\nC:\\Windows\\ping.exe"));
        assert_eq!(d.line(false), format!("{name}  (PID 77)  4.0 KB"), "a process is always named");
    }

    #[test]
    fn attributes_follow_the_facts_as_words_or_as_meanings() {
        let tree = flat_tree(3);
        let d = Description { attrs: Attrs::of_list(&[Attr::System, Attr::Cloud]), ..file(&tree, NodeId(1), 1, 1, Attrs::default()) };
        assert!(d.line(true).ends_with("  system, cloud"), "{}", d.line(true));
        assert!(d.tip(false).ends_with("\nsystem, cloud"));
        let t = d.tip(true);
        assert!(t.contains("\nsystem: ") && t.contains("\ncloud: ") && !t.ends_with("system, cloud"), "{t}");
        for a in Attr::ALL {
            assert!(!a.meaning().contains(':'), "{}: the tooltip already puts a colon after the word: {}", a.word(), a.meaning());
        }
    }
}
