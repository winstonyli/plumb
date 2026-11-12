//! What differs between the view kinds, as one const table each: the header titles and sorts, the cells of the Score,
//! Size and last two columns, the Score tooltip and the map class names.
//!
//! The Name and "% of parent" cells are drawn by `draw.rs` (icon, chevron, pills, filter tint; a bar from the share
//! of the parent), so they are not here. Alignment and dimness follow column position. Behaviour that differs by
//! kind (menus, kill, held files, map tiles) stays `kind() == Processes` code.
use crate::app::ViewKind;
use crate::fmt::{self, Show};
use plumb_core::procs::Metric;
use plumb_core::rows::SortKey;
use plumb_core::treemap::Class;
use plumb_core::{NodeId, Tree};

/// The name of a map metric: the button's label and the first column's title.
pub fn metric_label(m: Metric) -> &'static str {
    match m {
        Metric::Memory => "Memory",
        Metric::Private => "Private",
        Metric::Cpu => "CPU",
        Metric::Io => "I/O",
    }
}

/// What a map metric counts, as a noun inside a sentence ("4 KB private", "12.3% CPU").
pub fn metric_noun(m: Metric) -> &'static str {
    match m {
        Metric::Memory => "memory",
        Metric::Private => "private",
        Metric::Cpu => "CPU",
        Metric::Io => "I/O",
    }
}

/// One line saying what a map metric counts, for the metric button's tooltip.
pub fn metric_help(m: Metric) -> &'static str {
    match m {
        Metric::Memory => "the working set: resident pages, shared ones counted once per process",
        Metric::Private => "private bytes: committed memory not shared with other processes",
        Metric::Cpu => "share of one core",
        Metric::Io => "bytes transferred per second",
    }
}

/// A column by what it holds, the stable name of a saved sort or width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColId {
    Name,
    Score,
    Pct,
    Size,
    Items,
    Modified,
    Cpu,
    Io,
}

impl ColId {
    const ALL: [ColId; 8] = [ColId::Name, ColId::Score, ColId::Pct, ColId::Size, ColId::Items, ColId::Modified, ColId::Cpu, ColId::Io];

    /// The id as saved in the state file.
    pub fn name(self) -> &'static str {
        match self {
            ColId::Name => "name",
            ColId::Score => "score",
            ColId::Pct => "pct",
            ColId::Size => "size",
            ColId::Items => "items",
            ColId::Modified => "modified",
            ColId::Cpu => "cpu",
            ColId::Io => "io",
        }
    }

    pub fn parse(s: &str) -> Option<ColId> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }
}

#[derive(Clone, Copy)]
pub struct ColSpec {
    pub id: ColId,
    /// `Metric` is what the Processes view's first number column holds; the others ignore it.
    pub title: fn(Metric) -> &'static str,
    pub sort: SortKey,
    /// Whether the header says "at least" while a scan is running (its sum is what has been found so far).
    pub partial: bool,
}

pub struct ViewSpec {
    pub cols: [ColSpec; 6],
    /// The Score cell: stale bytes for a disk, a load in tenths of a percent for processes.
    pub score_text: fn(&Tree, NodeId) -> Show<'static>,
    /// The Score header's tooltip.
    pub score_help: &'static str,
    /// The text of a size-column value: bytes for a disk; a process's memory, CPU share or I/O rate by the metric.
    pub size_text: fn(Metric, u64) -> Show<'static>,
    /// The text of the last two columns; the second is `None` when the node has nothing to show there.
    pub last_two: fn(&Tree, NodeId) -> (Show<'static>, Option<Show<'static>>),
    /// What a map class is called in this view.
    pub class_label: fn(Class) -> &'static str,
}

impl ViewSpec {
    pub fn titles(&self, metric: Metric) -> [&'static str; 6] {
        self.cols.map(|c| (c.title)(metric))
    }
}

impl ViewKind {
    pub fn spec(self) -> &'static ViewSpec {
        match self {
            ViewKind::Disk => &DISK,
            ViewKind::Processes => &PROCESSES,
        }
    }
}

const fn col(id: ColId, title: fn(Metric) -> &'static str, sort: SortKey, partial: bool) -> ColSpec {
    ColSpec { id, title, sort, partial }
}

static DISK: ViewSpec = ViewSpec {
    cols: [
        col(ColId::Name, |_| "Name", SortKey::Name, false),
        col(ColId::Score, |_| "Stale", SortKey::Score, true),
        col(ColId::Pct, |_| "% of parent", SortKey::Size, false),
        col(ColId::Size, |_| "Size", SortKey::Size, true),
        col(ColId::Items, |_| "Items", SortKey::Items, true),
        col(ColId::Modified, |_| "Modified", SortKey::Modified, false),
    ],
    score_text: |tree, id| fmt::size(tree.subtree_score[id]),
    score_help: "Size not modified for a long time (counts in from 30 days, in full after 2 years); a folder adds its files",
    size_text: |_, v| fmt::size(v),
    last_two: |tree, id| (fmt::count(tree.subtree_files[id] as u64), tree.mtime[id].map(|d| fmt::date(fmt::local_day(d.get())))),
    class_label: |c| c.label(),
};

static PROCESSES: ViewSpec = ViewSpec {
    cols: [
        col(ColId::Name, |_| "Name", SortKey::Name, false),
        col(ColId::Score, |_| "Load", SortKey::Score, false),
        col(ColId::Pct, |_| "% of parent", SortKey::Size, false),
        col(ColId::Size, metric_label, SortKey::Size, true),
        col(ColId::Cpu, |_| "CPU", SortKey::Aux(0), false),
        col(ColId::Io, |_| "I/O", SortKey::Aux(1), false),
    ],
    score_text: |tree, id| fmt::tenths(tree.subtree_score[id]),
    score_help: "The most a process uses of CPU, memory or I/O, as a share of the machine, steady over about 30 s; a parent adds its children",
    size_text: |m, v| match m {
        Metric::Cpu => fmt::tenths(v),
        Metric::Io => fmt::rate(v),
        _ => fmt::size(v),
    },
    last_two: |tree, id| {
        let [cpu, io, _] = tree.aux.get(id.idx()).copied().unwrap_or_default();
        (fmt::tenths(cpu), Some(fmt::rate(io)))
    },
    class_label: |c| match c {
        Class::Executable => "Windows",
        Class::Document => "Program Files",
        Class::Code => "User profile",
        Class::Other => "Elsewhere",
        c => c.label(),
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [ViewKind; 2] = [ViewKind::Disk, ViewKind::Processes];

    #[test]
    fn every_id_round_trips_through_its_saved_name() {
        for id in ColId::ALL {
            assert_eq!(ColId::parse(id.name()), Some(id));
        }
        assert_eq!(ColId::parse("nope"), None);
    }

    #[test]
    fn every_kind_has_six_distinct_column_ids() {
        for k in KINDS {
            let ids: Vec<_> = k.spec().cols.iter().map(|c| c.id).collect();
            for (i, a) in ids.iter().enumerate() {
                assert!(ids[i + 1..].iter().all(|b| b != a), "{k:?} repeats {a:?}");
            }
        }
    }

    #[test]
    fn the_first_four_columns_are_the_same_in_both_kinds() {
        let first = |k: ViewKind| k.spec().cols[..4].iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(first(ViewKind::Disk), [ColId::Name, ColId::Score, ColId::Pct, ColId::Size]);
        assert_eq!(first(ViewKind::Disk), first(ViewKind::Processes));
    }

    #[test]
    fn the_tables_say_what_the_methods_said() {
        assert_eq!(ViewKind::Disk.spec().titles(Metric::Cpu), ["Name", "Stale", "% of parent", "Size", "Items", "Modified"]);
        assert_eq!(ViewKind::Processes.spec().titles(Metric::Io), ["Name", "Load", "% of parent", "I/O", "CPU", "I/O"]);
        assert_eq!(ViewKind::Processes.spec().titles(Metric::Private)[3], "Private");
        let sorts = |k: ViewKind| k.spec().cols.map(|c| c.sort);
        assert_eq!(sorts(ViewKind::Disk), [SortKey::Name, SortKey::Score, SortKey::Size, SortKey::Size, SortKey::Items, SortKey::Modified]);
        assert_eq!(sorts(ViewKind::Processes), [SortKey::Name, SortKey::Score, SortKey::Size, SortKey::Size, SortKey::Aux(0), SortKey::Aux(1)]);
        let partial = |k: ViewKind| k.spec().cols.map(|c| c.partial);
        assert_eq!(partial(ViewKind::Disk), [false, true, false, true, true, false]);
        assert_eq!(partial(ViewKind::Processes), [false, false, false, true, false, false]);
    }
}
