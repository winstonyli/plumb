//! Window-independent application state: what is selected, scrolled and scanning, and how
//! input changes it. No Win32 types, so the whole interaction model tests anywhere.
mod filter;

use crate::draw::{Chip, MapView, Mark, Scene, Theme, View};
use crate::filter_ui::Edit;
use crate::fmt;
use crate::layout::{Hit, Layout, Rect, CHIP_PAD, CHIP_W, INDENT, MAP_MIN_W, PICKER_H};
use crate::restore::Snapshot;
use crate::state::{MapState, Sort, State};
use plumb_core::basket::{self, Basket};
use plumb_core::rows::{Key, RowModel, SortKey};
use plumb_core::procs::Metric;
use plumb_core::treemap::{self, Class, Kind, Tile};
use plumb_core::{NodeFlags, NodeId, Tree};
use std::collections::{HashMap, HashSet};

pub use filter::FilterJob;

/// What a chip opens. The row table is the same for every kind (Name plus four columns); the kind supplies the
/// titles, what each column sorts by, and the text of the last two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewKind {
    /// A drive or mount point: the file tree.
    #[default]
    Disk,
    /// The running processes: a tree by parent, sized by memory.
    Processes,
}

impl ViewKind {
    /// `metric` is what the Processes view's first number column holds; a drive ignores it.
    pub fn titles(self, metric: Metric) -> [&'static str; 5] {
        match self {
            ViewKind::Disk => ["Name", "Size", "% of parent", "Items", "Modified"],
            ViewKind::Processes => ["Name", metric_label(metric), "% of parent", "CPU", "I/O"],
        }
    }

    /// The text of a size-column value: bytes for a drive; a process's memory, CPU share or I/O rate by `metric`.
    pub fn size_text(self, metric: Metric, v: u64) -> String {
        match (self, metric) {
            (ViewKind::Processes, Metric::Cpu) => format!("{:.1}%", v as f64 / 10.0),
            (ViewKind::Processes, Metric::Io) => format!("{}/s", fmt::size(v)),
            _ => fmt::size(v),
        }
    }

    /// The sort each header column stands for; "% of parent" orders by size.
    pub fn sort_key(self, col: usize) -> SortKey {
        match self {
            ViewKind::Disk => [SortKey::Name, SortKey::Size, SortKey::Size, SortKey::Items, SortKey::Modified][col],
            ViewKind::Processes => [SortKey::Name, SortKey::Size, SortKey::Size, SortKey::Aux(0), SortKey::Aux(1)][col],
        }
    }

    /// What a map class is called in this view. Processes borrows four file classes to show where the executable
    /// lives (see `location_class`).
    pub fn class_label(self, c: Class) -> &'static str {
        match (self, c) {
            (ViewKind::Processes, Class::Executable) => "Windows",
            (ViewKind::Processes, Class::Document) => "Program Files",
            (ViewKind::Processes, Class::Code) => "User profile",
            (ViewKind::Processes, Class::Other) => "Elsewhere",
            _ => c.label(),
        }
    }

    /// The text of the last two columns for `id`; the second is `None` when the node has nothing to show there.
    pub fn last_two(self, tree: &Tree, id: NodeId) -> (String, Option<String>) {
        match self {
            ViewKind::Disk => (fmt::count(tree.subtree_files[id] as u64), tree.mtime[id].map(|d| fmt::date(d.get()))),
            ViewKind::Processes => {
                let [cpu, io, _] = tree.aux.get(id.idx()).copied().unwrap_or_default();
                (format!("{:.1}%", cpu as f64 / 10.0), Some(format!("{}/s", fmt::size(io))))
            }
        }
    }
}

/// The name of a map metric: the button's label and the first column's title.
pub fn metric_label(m: Metric) -> &'static str {
    match m {
        Metric::Memory => "Memory",
        Metric::Cpu => "CPU",
        Metric::Io => "I/O",
    }
}

/// What the Processes view knows about a process besides what its tree holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcInfo {
    /// The executable's path; `None` when the process could not be opened (another user's, protected).
    pub path: Option<String>,
    /// Creation time, FILETIME.
    pub created: u64,
}

/// The map class of a process by where its executable is: the Windows folder, Program Files, a user profile, or
/// elsewhere (and unknown).
pub fn location_class(path: Option<&str>, system_root: &str) -> Class {
    let Some(path) = path else { return Class::Other };
    let (p, root) = (path.to_lowercase(), system_root.trim_end_matches('\\').to_lowercase());
    if !root.is_empty() && p.starts_with(&root) && p[root.len()..].starts_with('\\') {
        Class::Executable
    } else if p.contains("\\program files") {
        Class::Document
    } else if p.contains("\\users\\") {
        Class::Code
    } else {
        Class::Other
    }
}

/// The status line of the Processes view.
pub fn process_summary(count: usize, used: u64, total: u64) -> String {
    format!("{count} processes, {} in use of {}", fmt::size(used), fmt::size(total))
}

#[derive(Clone, Debug, PartialEq)]
pub struct DriveInfo {
    /// Where the volume is mounted: a drive root like `C:\`, or a folder like `D:\Mounts\Data\`.
    pub root: String,
    pub total: u64,
    pub free: u64,
    pub kind: ViewKind,
}

impl DriveInfo {
    /// The root as shown and typed (`--scan`): `C:` or `D:\Mounts\Data`, no trailing backslash.
    pub fn title(&self) -> &str {
        self.root.trim_end_matches('\\')
    }

    /// The chip's label: a drive stays `C:`, a mount-point folder shows its own name.
    pub fn chip_name(&self) -> &str {
        let t = self.title();
        if t.len() == 2 { t } else { t.rsplit('\\').next().unwrap_or(t) }
    }
}

#[derive(Default)]
pub enum Scan {
    #[default]
    Idle,
    Running { drive: usize, dirs: u64, files: u64, bytes: u64 },
    /// The user stopped the scan; there is no tree.
    Cancelled,
    Failed(String),
}

/// Whether the tree is being kept up to date from the change journal.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LiveStatus {
    #[default]
    Off,
    /// `last` is the clock time of the last applied change.
    On { last: Option<String> },
    /// Live updates ended; the tree may be out of date.
    Stale(String),
}

pub enum Input {
    Click(f32, f32),
    DoubleClick(f32, f32),
    RightClick(f32, f32),
    /// Rows to scroll; positive is down.
    Wheel(i32),
    Key(Key),
    /// Mouse moved with the left button held, at this x and y.
    Drag(f32, f32),
    Release,
    Resize(f32, f32),
    /// A text edit for the filter box (ignored unless it has focus).
    Edit(Edit),
    /// Ctrl+F: focus the filter box with its text selected.
    Find,
    Escape,
    Enter,
    Tab,
    /// The mouse moved with no button held (or left the window: any point outside the rows).
    Hover(f32, f32),
    /// Ctrl+M or the status bar button: show or hide the map pane.
    ToggleMap,
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    /// Start scanning the drive at this index.
    Scan(usize),
    /// Show the context menu for this node.
    Menu(NodeId),
    /// Relaunch elevated.
    Elevate,
    /// Rescan the drive at this index in the background, keeping the old tree on screen.
    Rescan(usize),
    /// Start the debounce timer for a filter run.
    Filter,
    /// Stop the scan in flight.
    Cancel,
    /// Ask what to delete (the basket chip).
    Commit,
    /// The Processes view's map now weighs by this (the tree's sizes must be rewritten).
    Metric(Metric),
}

/// A map tile's tooltip: the path and size of what it stands for, or what a `Rest` tile holds.
/// What a tooltip is about: a button or the status bar (by its rectangle), or a map tile (by its node and kind, which
/// survive the tiles being rebuilt as a tree grows).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TipKey {
    Area(Rect),
    Tile(NodeId, Kind),
}

fn tile_tip(tree: &Tree, t: &Tile, now: u32) -> String {
    match t.kind {
        Kind::Rest => format!("{} smaller items, {}", fmt::count(t.count as u64), fmt::size(t.bytes)),
        _ => {
            let when = tree.mtime[t.node].map_or_else(String::new, |d| format!(", modified {}, {} ago", fmt::date(d.get()), fmt::age(now, d.get())));
            format!("{}  ({}{when})", fmt::display_path(&tree.path(t.node)), fmt::size(t.bytes))
        }
    }
}

/// Today as a day count since 1601-01-01.
fn today() -> u32 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    plumb_core::age::day_from_unix(secs)
}

/// What the app holds for one drive, kept while another drive is on screen. The filter box text
/// and the sort stay with the app; everything derived from this drive's tree travels with it.
#[derive(Default)]
pub struct Parked {
    tree: Option<Tree>,
    model: Option<RowModel>,
    scroll: usize,
    scan: Scan,
    summary: String,
    live: LiveStatus,
    refreshing: bool,
    preview_paused: bool,
    filter: filter::Filter,
    basket: Basket,
    proc_info: HashMap<u32, ProcInfo>,
    proc_metric: Metric,
}

/// What the context menu and `Del` can do with a node.
#[derive(Debug, PartialEq)]
pub enum MarkState {
    Mark,
    Unmark,
    Refused(&'static str),
}

/// What a commit would delete: the topmost marked paths of one drive and what they hold.
pub struct CommitRequest {
    pub drive: usize,
    pub paths: Vec<String>,
    pub bytes: u64,
}

/// The basket counted against the tree: derived, so rebuilt when the marks, the tree or the tiles change.
#[derive(Default)]
struct BasketView {
    key: Option<(u64, (usize, usize, u64), u64)>,
    items: usize,
    bytes: u64,
    /// Indices into `map_tiles` to tint: tiles at or under a marked node.
    tiles: Vec<usize>,
}

impl Parked {
    pub fn has_tree(&self) -> bool {
        self.tree.is_some()
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// The tree a kept drive's running scan builds (its rows are made when the drive comes back).
    pub fn tree_mut(&mut self) -> &mut Option<Tree> {
        &mut self.tree
    }

    pub fn scan(&self) -> &Scan {
        &self.scan
    }
}

pub struct App {
    pub layout: Layout,
    pub drives: Vec<DriveInfo>,
    pub active: Option<usize>,
    /// What each drive off screen holds, shown on its chip; set by the window.
    pub marks: Vec<Mark>,
    /// The visible row under the mouse.
    pub hover: Option<usize>,
    /// The icon button under the mouse.
    pub hot: Option<Rect>,
    pub tree: Option<Tree>,
    pub model: Option<RowModel>,
    pub scroll: usize,
    pub scan: Scan,
    /// One line about the last finished scan (scanner, time, counts); empty before any.
    pub summary: String,
    /// Show the "Run as administrator" button (the process is not elevated).
    pub can_elevate: bool,
    pub adapter: String,
    pub live: LiveStatus,
    /// A background rescan is running while the old tree stays on screen.
    pub refreshing: bool,
    /// The partial tree stopped following the scan; the status line says so.
    pub preview_paused: bool,
    /// Header column last clicked (0 Name, 1 Size, 2 % of parent, 3 Items, 4 Modified) and direction.
    sort: (usize, bool),
    /// While dragging the scrollbar thumb: distance from the thumb's top to the grab point.
    grab: Option<f32>,
    /// While dragging a column edge: the column, and the pointer x and column width at the start.
    col_drag: Option<(usize, f32, f32)>,
    /// The folder the map shows; follows the tree's selection (see `follow_selection`).
    map_root: Option<NodeId>,
    /// The selection `map_root` was last chosen for: a change the map made itself is recorded here so it is not mistaken for one made in the tree.
    map_seen: Option<NodeId>,
    map_tiles: Vec<Tile>,
    /// Root and rectangle/scale bits the tiles were built for.
    map_key: Option<(NodeId, [u32; 5])>,
    map_dirty: bool,
    /// Bumped whenever `map_tiles` is rebuilt, so the painter knows its cached bitmap is stale.
    map_gen: u64,
    /// Today as a day count since 1601, taken when the tiles were last rebuilt: tiles are shaded by age against it.
    map_now: u32,
    /// Bytes per class among the tiles, largest first: the legend.
    map_classes: Vec<(treemap::Class, u64)>,
    /// The tile under the pointer.
    pub map_hover: Option<usize>,
    /// While dragging the divider: the pointer x and the pane width at the start.
    map_drag: Option<(f32, f32)>,
    filter: filter::Filter,
    basket: Basket,
    /// The Processes view's extra facts about each process, by pid.
    proc_info: HashMap<u32, ProcInfo>,
    /// What the Processes view's map and Memory column weigh by.
    pub proc_metric: Metric,
    /// Bumped when the marks change or the shown drive's basket is swapped.
    basket_rev: u64,
    basket_view: BasketView,
    /// A delete is running on the worker thread.
    deleting: bool,
    /// A one-off message (a refused mark, a delete result) shown first on the status line until the next click or key.
    notice: String,
    /// Every failure of the last delete, one a line: part of the full status line (the hover text).
    notice_detail: Vec<String>,
    system_root: String,
    /// Marks of drives whose kept state was let go (see `Win::evict`): a later scan of that drive takes them back.
    stash: HashMap<usize, Basket>,
    /// Marks the last commit left out because they can no longer be deleted (path, why): reported with its result.
    skipped: Vec<(String, &'static str)>,
}

impl App {
    pub fn new(layout: Layout, drives: Vec<DriveInfo>, adapter: String) -> Self {
        let layout = Layout { chips: vec![CHIP_W; drives.len()], ..layout };
        Self {
            layout, drives, adapter, active: None, marks: Vec::new(), hover: None, hot: None, tree: None, model: None, scroll: 0,
            can_elevate: false, scan: Scan::Idle, summary: String::new(), sort: (1, true), grab: None, col_drag: None,
            map_root: None, map_seen: None, map_tiles: Vec::new(), map_key: None, map_dirty: true, map_gen: 0, map_now: 0, map_classes: Vec::new(), map_hover: None, map_drag: None,
            live: LiveStatus::Off, refreshing: false, preview_paused: false, filter: Default::default(),
            basket: Basket::default(), proc_info: HashMap::new(), proc_metric: Metric::default(), basket_rev: 0, basket_view: BasketView::default(), deleting: false, notice: String::new(), notice_detail: Vec::new(), stash: HashMap::new(), skipped: Vec::new(),
            system_root: std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        }
    }

    /// Sizes each chip to its label: `measure` gives a text's width in DIPs.
    pub fn fit_chips(&mut self, measure: &dyn Fn(&str) -> f32) {
        self.layout.chips = self.chips().iter().map(|c| (measure(&c.label) + 2.0 * CHIP_PAD).ceil()).collect();
    }

    /// The header column the rows are sorted by, and whether descending.
    /// The kind of the view on screen (a drive when none is).
    pub fn kind(&self) -> ViewKind {
        self.active.and_then(|i| self.drives.get(i)).map_or(ViewKind::Disk, |d| d.kind)
    }

    pub fn sort(&self) -> (usize, bool) {
        self.sort
    }

    /// Takes the remembered column widths and sort (before any scan, so the first tree is sorted by it).
    pub fn restore(&mut self, s: &State) {
        if let Some(Sort { column, descending }) = s.sort.filter(|s| s.column < 5) {
            self.sort = (column, descending);
        }
        if let Some(cols) = &s.columns {
            for (i, &w) in cols.iter().take(self.layout.col_w.len()).enumerate().filter(|(_, w)| w.is_finite()) {
                self.layout.set_col_w(i + 1, w);
            }
        }
        self.layout.map_on = s.map.visible;
        if s.map.width.is_finite() {
            self.layout.map_w = s.map.width.clamp(MAP_MIN_W, 4000.0);
        }
    }

    /// The part of the remembered state the app owns; the window fills in its placement.
    pub fn remembered(&self) -> State {
        let (column, descending) = self.sort;
        State { window: None, columns: Some(self.layout.col_w.to_vec()), sort: Some(Sort { column, descending }), map: MapState { visible: self.layout.map_on, width: self.layout.map_w } }
    }

    /// Exchanges the per-drive state with `p`. The result is consistent on its own (banner and
    /// Cancel button follow the new state) but leaves the filter alone: see `switch_slot`.
    pub fn swap_slot(&mut self, p: &mut Parked) {
        use std::mem::swap;
        swap(&mut self.tree, &mut p.tree);
        swap(&mut self.model, &mut p.model);
        swap(&mut self.scroll, &mut p.scroll);
        swap(&mut self.scan, &mut p.scan);
        swap(&mut self.summary, &mut p.summary);
        swap(&mut self.live, &mut p.live);
        swap(&mut self.refreshing, &mut p.refreshing);
        swap(&mut self.preview_paused, &mut p.preview_paused);
        swap(&mut self.filter, &mut p.filter);
        swap(&mut self.filter.ui, &mut p.filter.ui); // the query text belongs to the window
        swap(&mut self.basket, &mut p.basket);
        swap(&mut self.proc_info, &mut p.proc_info);
        swap(&mut self.proc_metric, &mut p.proc_metric);
        self.basket_rev += 1;
        self.layout = self.layout.with_banner(matches!(self.live, LiveStatus::Stale(_))).with_cancel(self.cancelable());
        self.set_scroll(self.scroll);
        self.map_reset();
    }

    /// Puts another drive on screen: the outgoing rows lose the filter so they come back whole, and
    /// the incoming ones are filtered afresh.
    pub fn switch_slot(&mut self, p: &mut Parked) {
        if self.filter.installed.is_some() {
            self.unfilter_rows();
        }
        self.filter_reset();
        self.swap_slot(p);
        self.filter_reset();
    }

    pub fn begin_scan(&mut self, drive: usize) {
        self.filter_reset();
        self.map_reset();
        if self.active != Some(drive) {
            // A rescan of the drive on screen keeps its marks; another drive brings back the ones it was let go with.
            self.basket = self.stash.remove(&drive).unwrap_or_default();
            self.basket_rev += 1;
        }
        self.active = Some(drive);
        self.tree = None;
        self.model = None;
        self.scroll = 0;
        self.summary.clear();
        self.set_live(LiveStatus::Off);
        self.refreshing = false;
        self.preview_paused = false;
        self.scan = Scan::Running { drive, dirs: 0, files: 0, bytes: 0 };
    }

    pub fn progress(&mut self, d: u64, f: u64, b: u64) {
        if let Scan::Running { dirs, files, bytes, .. } = &mut self.scan {
            (*dirs, *files, *bytes) = (d, f, b);
        }
    }

    /// Installs a finished tree. The root is expanded and selected so there is something to see.
    pub fn finish_scan(&mut self, tree: Tree, summary: String) {
        self.filter_reset();
        self.map_reset();
        let mut model = RowModel::new(&tree);
        model.set_sort(&tree, self.kind().sort_key(self.sort.0), self.sort.1);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(&tree, r);
            model.select(r);
        }
        self.tree = Some(tree);
        self.model = Some(model);
        self.scroll = 0;
        self.summary = summary;
        self.scan = Scan::Idle;
        self.preview_paused = false;
        self.set_live(LiveStatus::Off);
    }

    /// The preview applier just created `self.tree` or dropped it (a scanner fell back to
    /// another): build the rows for the tree, or show nothing. The scan state is untouched, so
    /// the progress line keeps counting.
    pub fn preview_installed(&mut self) {
        self.filter_reset();
        self.map_reset();
        self.scroll = 0;
        self.model = None;
        let key = self.kind().sort_key(self.sort.0);
        let Some(tree) = &mut self.tree else { return };
        tree.take_touched(); // the model is built from the tree as it is now
        let mut model = RowModel::new(tree);
        model.set_sort(tree, key, self.sort.1);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(tree, r);
            model.select(r);
        }
        self.model = Some(model);
    }

    /// Replaces the tree with a rescanned one, keeping the sort, the expanded folders and the
    /// selection (by path) and where it sits on screen.
    pub fn swap_tree(&mut self, tree: Tree, summary: String) {
        self.basket_rev += 1; // the marks are counted against the new tree
        if self.filter.installed.is_some() {
            self.unfilter_rows();
        }
        let kept = match (&self.tree, &self.model) {
            (Some(t), Some(m)) => Some(Snapshot::take(t, m, self.scroll)),
            _ => None,
        };
        self.map_reset();
        let Some(snap) = kept else { return self.finish_scan(tree, summary) };
        let (model, scroll) = snap.apply(&tree, self.kind().sort_key(self.sort.0), self.sort.1, self.layout.rows_visible());
        self.tree = Some(tree);
        self.model = Some(model);
        self.scroll = scroll;
        self.summary = summary;
        self.refreshing = false;
        self.scan = Scan::Idle;
        self.preview_paused = false;
        self.filter_reset();
        self.set_live(LiveStatus::Off);
    }

    /// Brings the rows up to date after the tree changed in place.
    pub fn refresh_rows(&mut self) {
        self.filter_live();
        if let (Some(t), Some(m)) = (&mut self.tree, &mut self.model) {
            let s = m.refresh(t, self.scroll);
            self.set_scroll(s);
        }
        self.map_dirty = true;
    }

    pub fn set_live(&mut self, live: LiveStatus) {
        self.layout = self.layout.with_banner(matches!(live, LiveStatus::Stale(_)));
        self.live = live;
        self.set_scroll(self.scroll);
    }

    /// Records the clock time of the last change applied by live updates.
    pub fn live_changed(&mut self, at: String) {
        if let LiveStatus::On { last } = &mut self.live {
            *last = Some(at);
        }
    }

    /// A scan is in flight (a first scan, or a background rescan).
    pub fn cancelable(&self) -> bool {
        self.refreshing || matches!(self.scan, Scan::Running { .. })
    }

    /// The user cancelled the scan in flight. A first scan leaves no tree; a background rescan
    /// leaves the old one on screen.
    pub fn cancel_scan(&mut self) {
        if matches!(self.scan, Scan::Running { .. }) {
            self.tree = None;
            self.model = None;
            self.preview_paused = false;
            self.scan = Scan::Cancelled;
            self.map_reset();
        }
        self.refreshing = false;
    }

    pub fn fail_scan(&mut self, msg: String) {
        self.tree = None;
        self.model = None;
        self.preview_paused = false;
        self.scan = Scan::Failed(msg);
        self.map_reset();
    }

    fn total_rows(&self) -> usize {
        self.model.as_ref().map_or(0, |m| m.rows().len())
    }

    fn max_scroll(&self) -> usize {
        self.total_rows().saturating_sub(self.layout.rows_visible())
    }

    fn set_scroll(&mut self, s: usize) {
        self.scroll = s.min(self.max_scroll());
    }

    /// The folder whose icon is at x on visible row `i`, if `x` is over that icon and the folder can expand.
    fn icon_at(&self, i: usize, x: f32) -> Option<NodeId> {
        let (t, m) = (self.tree.as_ref()?, self.model.as_ref()?);
        let row = m.rows().get(self.scroll + i)?;
        let left = self.layout.column(0).0 + 8.0 + row.depth as f32 * INDENT;
        (x >= left && x < left + 16.0 && m.is_expandable(t, row.id)).then_some(row.id)
    }

    /// Whether the pointer is on a column edge in the header (or one is being dragged), for the resize cursor.
    pub fn over_divider(&self, x: f32, y: f32) -> bool {
        self.col_drag.is_some()
            || self.map_drag.is_some()
            || matches!(self.layout.hit(x, y, self.drives.len()), Hit::Divider(_) | Hit::MapDivider)
    }

    /// The button at (x, y), if there is one there, with its tooltip: an icon button carries no label, and a
    /// drive chip's label shows only the free size, to fit. These are what light up under the pointer.
    fn button_at(&self, x: f32, y: f32) -> Option<(Rect, String)> {
        let l = &self.layout;
        if self.can_elevate && l.admin_button().contains(x, y) {
            return Some((l.admin_button(), "Run as administrator".into()));
        }
        match l.hit(x, y, self.drives.len()) {
            Hit::Chip(i) => {
                let d = self.drives.get(i)?;
                Some((l.chip(i), format!("{} {} free of {}", d.title(), fmt::size(d.free), fmt::size(d.total))))
            }
            Hit::Rescan => Some((l.rescan_button(), "Rescan".into())),
            Hit::Cancel => Some((l.cancel_button(), "Cancel scan".into())),
            Hit::Basket => Some((l.basket_chip(), "Review and delete the marked items".into())),
            Hit::FilterRegex => Some((l.filter_regex(), "Regular expression".into())),
            Hit::MapButton => Some((l.map_button(), "Show or hide the map  (Ctrl+M)".into())),
            Hit::MetricButton => Some((l.metric_button(), format!("The map and the first column weigh processes by {}; click for the next", metric_label(self.proc_metric)))),
            _ => None,
        }
    }

    /// The tooltip under (x, y): what it is about, and its text. The text may change while the target stays
    /// (a scan keeps adding to the totals), so the window compares targets to decide when to hide the tip.
    pub fn tip_target(&self, x: f32, y: f32) -> Option<(TipKey, String)> {
        if let Some((r, text)) = self.button_at(x, y) {
            return Some((TipKey::Area(r), text));
        }
        let bar = self.layout.status();
        if bar.contains(x, y) {
            return Some((TipKey::Area(bar), format!("{}
{}", self.status(), self.adapter)));
        }
        let t = self.map_tiles.get(treemap::hit(&self.map_tiles, x, y).filter(|_| matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map))?)?;
        let tree = self.tree.as_ref()?;
        let text = if self.kind() == ViewKind::Processes { self.process_tip(tree, t) } else { tile_tip(tree, t, self.map_now) };
        Some((TipKey::Tile(t.node, t.kind), text))
    }

    /// A map tile of the Processes view: name and pid, the numbers, the executable and when it started.
    fn process_tip(&self, tree: &Tree, t: &Tile) -> String {
        if t.kind == Kind::Rest {
            return format!("{} smaller processes, {}", fmt::count(t.count as u64), self.kind().size_text(self.proc_metric, t.bytes));
        }
        let [cpu, io, pid] = tree.aux.get(t.node.idx()).copied().unwrap_or_default();
        let mut tip = format!(
            "{}  (PID {pid})\n{} {} in this and its children; own CPU {:.1}%, I/O {}/s",
            tree.name_string(t.node), self.kind().size_text(self.proc_metric, t.bytes), metric_label(self.proc_metric).to_lowercase(), cpu as f64 / 10.0, fmt::size(io)
        );
        if let Some(info) = self.proc_info.get(&(pid as u32)) {
            if let Some(path) = &info.path {
                tip.push('\n');
                tip.push_str(path);
            }
            if info.created > 0 {
                let day = (info.created / 864_000_000_000) as u32;
                tip.push_str(&format!("\nstarted {}, {} ago", fmt::date(day), fmt::age(today(), day)));
            }
        }
        tip
    }

    /// The Processes view took a new snapshot: the facts about each process, the status line, and the chip's memory.
    pub fn set_procs(&mut self, drive: usize, info: HashMap<u32, ProcInfo>, summary: String, ram: (u64, u64)) {
        self.proc_info = info;
        self.summary = summary;
        if let Some(d) = self.drives.get_mut(drive) {
            (d.total, d.free) = ram;
        }
    }

    /// What the context menu of `id` acts on: the path in the tree's terms (to mark by) and as a file system path
    /// (to reveal and copy). A process offers its executable; none when that is unknown.
    pub fn menu_target(&self, id: NodeId) -> Option<(String, String, MarkState)> {
        let tree = self.tree.as_ref()?;
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx())?[2] as u32;
            let path = self.proc_info.get(&pid)?.path.clone()?;
            return Some((String::new(), path, self.mark_state(id)));
        }
        Some((tree.path(id), fmt::display_path(&tree.path(id)), self.mark_state(id)))
    }

    pub fn tip_at(&self, x: f32, y: f32) -> Option<String> {
        self.tip_target(x, y).map(|t| t.1)
    }

    /// Whether (x, y) is empty title-bar space, where a drag moves the window and a double-click maximizes
    /// it: the picker bar, away from the drive chips and the administrator button.
    pub fn is_caption(&self, x: f32, y: f32) -> bool {
        y < PICKER_H
            && !matches!(
                self.layout.hit(x, y, self.drives.len()),
                Hit::Chip(_) | Hit::FilterBox | Hit::FilterRegex | Hit::FilterClear
            )
            && !(self.can_elevate && self.layout.admin_button().contains(x, y))
    }

    fn row_at(&self, i: usize) -> Option<NodeId> {
        self.model.as_ref()?.rows().get(self.scroll + i).map(|r| r.id)
    }

    pub fn handle(&mut self, input: Input) -> Effect {
        self.layout.cancel = self.cancelable();
        self.layout.basket = self.basket_active();
        self.layout.metric = self.kind() == ViewKind::Processes;
        if matches!(input, Input::Click(..) | Input::DoubleClick(..) | Input::RightClick(..) | Input::Key(_) | Input::Edit(_) | Input::Enter | Input::Escape | Input::Find) {
            self.notice.clear();
            self.notice_detail.clear();
        }
        match input {
            Input::Resize(w, h) => {
                (self.layout.w, self.layout.h) = (w, h);
                self.set_scroll(self.scroll);
            }
            Input::Wheel(rows) => self.set_scroll((self.scroll as i64 + rows as i64).max(0) as usize),
            Input::Key(k) => {
                if self.filter.ui.focus
                    && let Some(e) = filter::edit_for_key(k)
                {
                    return self.filter_edit(e);
                }
                if let (Some(t), Some(m)) = (&self.tree, &mut self.model) {
                    m.handle_key(t, k, self.layout.rows_visible().max(1));
                    let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
                    self.set_scroll(s);
                }
            }
            Input::Edit(Edit::Delete) if !self.filter.ui.focus => self.toggle_mark_selected(),
            Input::Edit(e) => return self.filter_edit(e),
            Input::Find => {
                self.filter.ui.focus = true;
                self.filter.ui.edit(Edit::SelectAll);
            }
            Input::Escape => {
                let used = self.filter.ui.focus || !self.filter.ui.text.is_empty();
                let e = self.filter_escape();
                return if !used && self.cancelable() { Effect::Cancel } else { e };
            }
            Input::Enter => self.filter_enter(),
            Input::Tab => self.filter.ui.focus = !self.filter.ui.focus,
            Input::Hover(x, y) => {
                self.hover = match self.layout.hit(x, y, self.drives.len()) {
                    Hit::Row(i) if self.row_at(i).is_some() => Some(i),
                    _ => None,
                };
                self.hot = self.button_at(x, y).map(|b| b.0);
                self.map_hover = if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map) { treemap::hit(&self.map_tiles, x, y) } else { None };
            }
            Input::ToggleMap => self.layout.map_on = !self.layout.map_on,
            Input::Click(x, y) => return self.click(x, y),
            // Windows sends the second press of a quick pair as a double-click instead of a click, so
            // anywhere but a row it must act as a click, or the second press of a header, button or
            // scrollbar would be lost.
            Input::DoubleClick(x, y) => {
                if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map) {
                    if let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                        && self.tree.as_ref().is_some_and(|t| t.flags[id].contains(NodeFlags::DIR))
                    {
                        self.map_root = Some(id);
                        self.map_dirty = true;
                    }
                    return Effect::None;
                }
                let Hit::Row(i) = self.layout.hit(x, y, self.drives.len()) else {
                    return self.click(x, y);
                };
                if self.icon_at(i, x).is_some() {
                    return self.click(x, y);
                }
                if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.tree, &mut self.model) {
                    m.select(id);
                    m.toggle(t, id);
                    self.set_scroll(self.scroll);
                }
            }
            Input::RightClick(x, y) => {
                if let Hit::Row(i) = self.layout.hit(x, y, self.drives.len())
                    && let (Some(id), Some(m)) = (self.row_at(i), &mut self.model)
                {
                    m.select(id);
                    return Effect::Menu(id);
                } else if matches!(self.layout.hit(x, y, self.drives.len()), Hit::Map)
                    && let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                {
                    return Effect::Menu(id);
                }
            }
            Input::Drag(x, y) => {
                if let Some((c, x0, w0)) = self.col_drag {
                    // The edge is the column's left side, so moving it left widens the column.
                    self.layout.set_col_w(c, w0 + x0 - x);
                } else if let Some(g) = self.grab {
                    let s = self.layout.scroll_for_thumb_y(self.total_rows(), y - g);
                    self.set_scroll(s);
                } else if let Some((x0, w0)) = self.map_drag {
                    self.layout.set_map_w(w0 + x0 - x);
                }
            }
            Input::Release => {
                self.grab = None;
                self.col_drag = None;
                self.map_drag = None;
            }
        }
        Effect::None
    }

    /// Points the map at the folder for the tree's current selection, unless the map made that selection itself
    /// (`select_from_map` records it in `map_seen`). Runs from `update_map`, so every way the selection can change
    /// (mouse, keys, the filter, a rescan) is picked up before the next paint without hooking each one.
    fn follow_selection(&mut self) {
        let sel = self.model.as_ref().and_then(|m| m.selected());
        if sel == self.map_seen && self.map_root.is_some() == self.tree.is_some() {
            return;
        }
        self.map_seen = sel;
        self.map_root = self.tree.as_ref().and_then(|t| treemap::root_for(t, sel));
        self.map_dirty = true;
    }

    /// The tree was replaced, so every remembered id is meaningless.
    fn map_reset(&mut self) {
        self.map_seen = None;
        self.map_root = None;
        self.map_dirty = true;
        self.map_hover = None;
    }

    /// Selects the node of map tile `i` (not `Rest`, which has none) and brings its row into view; the map root stays.
    fn select_from_map(&mut self, i: usize) -> Option<NodeId> {
        let t = *self.map_tiles.get(i)?;
        if t.kind == Kind::Rest {
            return None;
        }
        let (tree, m) = (self.tree.as_ref()?, self.model.as_mut()?);
        m.reveal(tree, t.node);
        self.map_seen = Some(t.node);
        let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
        self.set_scroll(s);
        Some(t.node)
    }

    /// Whether `map_root` still names a live node of the current tree.
    fn live_map_root(&self) -> Option<NodeId> {
        let t = self.tree.as_ref()?;
        let sel = self.model.as_ref().and_then(|m| m.selected());
        self.map_root.filter(|&r| r.idx() < t.len() && !t.flags[r].contains(NodeFlags::DEAD)).or_else(|| treemap::root_for(t, sel))
    }

    /// Rebuilds the tiles if the tree, the root, the pane's rectangle or the scale changed since they were built.
    /// `px` is the size of one physical pixel in DIPs (`96 / dpi`). Call before painting.
    pub fn update_map(&mut self, px: f32) {
        self.update_tiles(px);
        self.refresh_basket();
    }

    fn update_tiles(&mut self, px: f32) {
        self.follow_selection();
        let (rect, root) = (self.layout.map(), self.live_map_root());
        let (Some(tree), Some(rect), Some(root)) = (&self.tree, rect, root) else {
            self.map_tiles.clear();
            self.map_key = None;
            self.map_classes.clear();
            self.map_hover = None;
            return;
        };
        let key = (root, [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits(), rect.h.to_bits(), px.to_bits()]);
        if !self.map_dirty && self.map_key == Some(key) {
            return;
        }
        self.map_tiles = if self.kind() == ViewKind::Processes {
            let by_location = |id: NodeId| {
                let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2]) as u32;
                location_class(self.proc_info.get(&pid).and_then(|i| i.path.as_deref()), &self.system_root)
            };
            treemap::layout_by(tree, root, rect, px, &by_location)
        } else {
            treemap::layout(tree, root, rect, px)
        };
        self.map_classes = treemap::class_bytes(&self.map_tiles);
        self.map_now = today();
        (self.map_key, self.map_dirty, self.map_hover) = (Some(key), false, None);
        self.map_gen += 1;
    }

    pub fn basket(&self) -> &Basket {
        &self.basket
    }

    pub fn basket_active(&self) -> bool {
        self.basket_view.items > 0
    }

    pub fn marked_tiles(&self) -> &[usize] {
        &self.basket_view.tiles
    }

    /// "3 marked, up to 12.4 GB" for the status bar chip.
    pub fn basket_label(&self) -> Option<String> {
        self.basket_active().then(|| format!("{} marked, up to {}", self.basket_view.items, fmt::size(self.basket_view.bytes)))
    }

    /// Keeps the marks of `drive`, whose state is being let go, for its next scan.
    pub fn stash_basket(&mut self, drive: usize, from: &mut Parked) {
        if !from.basket.is_empty() {
            self.stash.insert(drive, std::mem::take(&mut from.basket));
        }
    }

    pub fn clear_basket(&mut self) {
        self.basket.clear();
        self.basket_rev += 1;
    }

    pub fn mark_state(&self, id: NodeId) -> MarkState {
        let Some(tree) = &self.tree else { return MarkState::Refused("no drive is shown") };
        if self.kind() == ViewKind::Processes {
            return MarkState::Refused("processes cannot be deleted");
        }
        if self.basket.contains(&tree.path(id)) {
            return MarkState::Unmark;
        }
        if self.cancelable() {
            return MarkState::Refused("the drive is being scanned");
        }
        basket::refusal(tree, id, &self.system_root).map_or(MarkState::Mark, MarkState::Refused)
    }

    /// Marks or unmarks `id`; a refusal goes to the status line.
    pub fn toggle_mark(&mut self, id: NodeId) {
        if let MarkState::Refused(why) = self.mark_state(id) {
            self.notice = format!("cannot mark: {why}");
            return;
        }
        let Some(path) = self.tree.as_ref().map(|t| t.path(id)) else { return };
        self.basket.toggle(&path);
        self.basket_rev += 1;
    }

    /// Marks or unmarks the node at `path`, found afresh: the tree may have changed since the caller chose it.
    pub fn toggle_mark_path(&mut self, path: &str) {
        match self.tree.as_ref().and_then(|t| t.find(path)) {
            Some(id) => self.toggle_mark(id),
            None => self.notice = "that item is gone".into(),
        }
    }

    fn toggle_mark_selected(&mut self) {
        if let Some(id) = self.model.as_ref().and_then(|m| m.selected()) {
            self.toggle_mark(id);
        }
    }

    /// Rebuilds what the chip and the tiles show when the marks, the tree or the tiles changed.
    fn refresh_basket(&mut self) {
        let Some(tree) = &self.tree else {
            self.basket_view = BasketView::default();
            return;
        };
        if self.basket.is_empty() {
            if self.basket_view.key.is_some() {
                self.basket_view = BasketView::default();
            }
            return;
        }
        let fingerprint = (tree.len(), tree.dead(), tree.rev());
        let key = (self.basket_rev, fingerprint, self.map_gen);
        if self.basket_view.key == Some(key) {
            return;
        }
        let ids = self.basket.covered(tree);
        let set: HashSet<NodeId> = ids.iter().copied().collect();
        let under = |mut n: NodeId| {
            while !n.is_none() {
                if set.contains(&n) {
                    return true;
                }
                n = tree.parent[n];
            }
            false
        };
        let tiles = self.map_tiles.iter().enumerate().filter(|(_, t)| under(t.node)).map(|(i, _)| i).collect();
        self.basket_view = BasketView { key: Some(key), items: ids.len(), bytes: ids.iter().map(|&i| tree.subtree_size[i]).sum(), tiles };
    }

    pub fn deleting(&self) -> bool {
        self.deleting
    }

    /// Starts a delete if none runs and no scan does; otherwise says why not on the status line.
    pub fn begin_delete(&mut self) -> bool {
        if self.deleting {
            self.notice = "a delete is already running".into();
            return false;
        }
        if self.cancelable() {
            self.notice = "cannot delete while the drive is being scanned".into();
            return false;
        }
        self.deleting = true;
        true
    }

    pub fn set_deleting(&mut self, on: bool) {
        self.deleting = on;
    }

    /// What a commit would delete right now, or `None` (with the reason on the status line).
    pub fn commit_request(&mut self) -> Option<CommitRequest> {
        if self.deleting {
            self.notice = "a delete is already running".into();
            return None;
        }
        if self.cancelable() {
            self.notice = "cannot delete while the drive is being scanned".into();
            return None;
        }
        let (drive, tree) = (self.active?, self.tree.as_ref()?);
        self.basket.prune(tree);
        self.basket_rev += 1;
        // A mark that cannot be deleted now (a rename gave it an unsafe name) is dropped and reported with the result.
        let (ids, refused): (Vec<NodeId>, Vec<NodeId>) = self.basket.covered(tree).into_iter().partition(|&i| basket::refusal(tree, i, &self.system_root).is_none());
        self.skipped = refused.iter().map(|&i| (tree.path(i), basket::refusal(tree, i, &self.system_root).unwrap_or(""))).collect();
        let skipped: Vec<String> = self.skipped.iter().map(|(p, _)| p.clone()).collect();
        self.basket.remove_under(&skipped);
        self.basket_rev += 1;
        if ids.is_empty() {
            self.clear_basket();
            self.notice = match self.skipped.first() {
                Some((_, why)) => format!("{} marked item{} skipped: {why}", self.skipped.len(), if self.skipped.len() == 1 { "" } else { "s" }),
                None => "the marked items are already gone".into(),
            };
            self.notice_detail = self.skipped.drain(..).map(|(p, why)| format!("{p}: {why}")).collect();
            return None;
        }
        let paths = ids.iter().map(|&i| tree.path(i)).collect();
        let bytes = ids.iter().map(|&i| tree.subtree_size[i]).sum();
        Some(CommitRequest { drive, paths, bytes })
    }

    /// A delete ended: `gone` (deleted, `missing` of them already absent) leave the basket, `failed` (path, reason) stay.
    pub fn finish_delete(&mut self, gone: &[String], missing: usize, failed: &[(String, String)]) {
        self.deleting = false;
        self.basket.remove_under(gone);
        self.basket_rev += 1;
        let deleted = gone.len() - missing.min(gone.len());
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.notice = match failed.first() {
            None if missing == 0 => format!("deleted {deleted} item{}", plural(deleted)),
            None => format!("deleted {deleted} item{}, {missing} already gone", plural(deleted)),
            Some((_, why)) => format!("{} of {} not deleted: {why}; still marked", failed.len(), gone.len() + failed.len()),
        };
        self.notice_detail = failed.iter().map(|(p, why)| format!("{p}: {why}")).collect();
        if let Some((_, why)) = self.skipped.first() {
            self.notice += &format!("; {} skipped: {why}", self.skipped.len());
            self.notice_detail.extend(self.skipped.drain(..).map(|(p, why)| format!("{p}: {why}")));
        }
    }

    pub fn map_tiles(&self) -> &[Tile] {
        &self.map_tiles
    }

    /// The tile of the selected node, if it has one (a node inside a `Rest` tile has none).
    pub fn map_selected_tile(&self) -> Option<usize> {
        let sel = self.model.as_ref()?.selected()?;
        self.map_tiles.iter().position(|t| t.node == sel && t.kind != Kind::Rest)
    }

    fn click(&mut self, x: f32, y: f32) -> Effect {
        if self.can_elevate && self.layout.admin_button().contains(x, y) {
            return Effect::Elevate;
        }
        let hit = self.layout.hit(x, y, self.drives.len());
        match hit {
            Hit::FilterBox => {
                self.filter.ui.focus = true;
                return Effect::None;
            }
            Hit::FilterRegex => {
                self.filter.ui.focus = true;
                self.filter.ui.regex = !self.filter.ui.regex;
                return self.filter_changed();
            }
            Hit::FilterClear => {
                self.filter.ui.focus = true;
                return if self.filter.ui.clear() { self.filter_changed() } else { Effect::None };
            }
            _ => self.filter.ui.focus = false,
        }
        match hit {
            Hit::Cancel => return Effect::Cancel,
            Hit::Basket => return Effect::Commit,
            // Picking another drive mid-scan cancels the scan in flight; the same drive does nothing.
            Hit::Chip(i) if !(matches!(self.scan, Scan::Running { .. }) && self.active == Some(i)) => return Effect::Scan(i),
            Hit::Rescan => {
                if let Some(i) = self.active.filter(|_| !self.refreshing && !matches!(self.scan, Scan::Running { .. })) {
                    return Effect::Rescan(i);
                }
            }
            Hit::Header(c) => {
                let desc = if self.sort.0 == c { !self.sort.1 } else { c != 0 };
                self.sort = (c, desc);
                let kind = self.kind();
                if let (Some(t), Some(m)) = (&self.tree, &mut self.model) {
                    m.set_sort(t, kind.sort_key(c), desc);
                    self.scroll = 0;
                }
            }
            Hit::Divider(c) => self.col_drag = Some((c, x, self.layout.col_w[c - 1])),
            Hit::MapButton => self.layout.map_on = !self.layout.map_on,
            Hit::MetricButton => return Effect::Metric(self.proc_metric.next()),
            Hit::MapDivider => self.map_drag = Some((x, self.layout.map_w)),
            Hit::Map => {
                if let Some(i) = treemap::hit(&self.map_tiles, x, y) {
                    self.select_from_map(i);
                }
            }
            Hit::Row(i) => {
                let folder = self.icon_at(i, x);
                if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.tree, &mut self.model) {
                    m.select(id);
                    if folder.is_some() {
                        m.toggle(t, id);
                        self.set_scroll(self.scroll);
                    }
                }
            }
            Hit::Scrollbar => {
                let total = self.total_rows();
                match self.layout.thumb(total, self.scroll) {
                    Some(t) if y >= t.y && y < t.y + t.h => self.grab = Some(y - t.y),
                    Some(t) => {
                        let page = self.layout.rows_visible().max(1);
                        self.set_scroll(if y < t.y { self.scroll.saturating_sub(page) } else { self.scroll + page });
                    }
                    None => {}
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub fn chips(&self) -> Vec<Chip> {
        self.drives
            .iter()
            .enumerate()
            .map(|(i, d)| Chip {
                label: format!("{}  {}", d.chip_name(), fmt::size(d.free)),
                used: 1.0 - fmt::fraction(d.free, d.total),
                mark: if self.active == Some(i) { Mark::None } else { self.marks.get(i).copied().unwrap_or_default() },
            })
            .collect()
    }

    fn message(&self) -> String {
        match &self.scan {
            Scan::Idle => "Select a drive above to scan it.".into(),
            Scan::Running { drive, .. } if self.drives[*drive].kind == ViewKind::Processes => "Reading processes\u{2026}".into(),
            Scan::Running { drive, dirs, files, bytes } => format!(
                "Scanning {}  {} folders, {} files, {}",
                self.drives[*drive].title(), fmt::count(*dirs), fmt::count(*files), fmt::size(*bytes)
            ),
            Scan::Failed(m) => format!("Scan failed: {m}"),
            Scan::Cancelled => "Scan cancelled. Select a drive above to scan it again.".into(),
        }
    }

    /// The selected row's size and (for a folder) item count, led by its full path when `full`; first in the status line.
    fn selection(&self, full: bool) -> Option<String> {
        let (tree, id) = (self.tree.as_ref()?, self.model.as_ref()?.selected()?);
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2]);
            return Some(format!("{}  (PID {pid})  {}", tree.name_string(id), self.kind().size_text(self.proc_metric, tree.subtree_size[id])));
        }
        let path = fmt::display_path(&tree.path(id));
        let items = if tree.flags[id].contains(NodeFlags::DIR) { format!(", {} items", fmt::count(tree.subtree_files[id] as u64)) } else { String::new() };
        let size = format!("{}{items}", fmt::size(tree.subtree_size[id]));
        Some(if full { format!("{path}  ({size})") } else { size })
    }

    /// The whole status line (the status bar's tooltip carries it): the compact line has no path and no time of the last change.
    fn status(&self) -> String {
        self.status_line(true)
    }

    fn status_line(&self, full: bool) -> String {
        self.status_parts(full, true)
    }

    /// The status line without the selection: the status bar's second row, the selection having a row of its own.
    fn state_line(&self) -> String {
        self.status_parts(false, false)
    }

    fn status_parts(&self, full: bool, with_selection: bool) -> String {
        let head = match &self.scan {
            Scan::Running { .. } => {
                let paused = if self.preview_paused { "   (preview paused)" } else { "" };
                format!("{}{paused}", self.message())
            }
            Scan::Failed(_) | Scan::Cancelled => self.message(),
            _ if self.summary.is_empty() => self.message(),
            _ => self.summary.clone(),
        };
        let mut parts: Vec<String> = self.selection(full).filter(|_| with_selection).into_iter().chain([head]).collect();
        if !self.notice.is_empty() {
            parts.insert(0, self.notice.clone());
            if full && !self.notice_detail.is_empty() {
                parts.insert(1, self.notice_detail.join("; "));
            }
        }
        match &self.live {
            LiveStatus::Off => {}
            LiveStatus::On { last: None } => parts.push("live".into()),
            LiveStatus::On { last: Some(t) } if full => parts.push(format!("live, last change {t}")),
            LiveStatus::On { last: Some(_) } => parts.push("live".into()),
            LiveStatus::Stale(_) => parts.push("live updates stopped".into()),
        }
        if self.refreshing {
            parts.push("refreshing\u{2026}".into());
        }
        parts.join("   |   ")
    }

    fn banner(&self) -> Option<String> {
        match &self.live {
            LiveStatus::Stale(why) => Some(format!("live updates stopped: {why}; the tree may be out of date")),
            _ => None,
        }
    }

    /// What the running scan has yet to find, by the drive's used space; `None` when no scan runs or it
    /// has already counted more than the drive reports used (hard links, placeholders and compressed
    /// files can make it, and then the used space says nothing about what is left).
    pub fn unscanned(&self) -> Option<u64> {
        let Scan::Running { drive, bytes, .. } = self.scan else { return None };
        let d = self.drives.get(drive)?;
        d.total.saturating_sub(d.free).checked_sub(bytes)
    }

    /// Builds the scene for `dark` and passes it to `f`; the scene borrows locals made here.
    pub fn with_scene<R>(&self, dark: bool, f: impl FnOnce(&Scene) -> R) -> R {
        let chips = self.chips();
        let (message, status, banner) = (self.message(), self.state_line(), self.banner());
        let selection = self.selection(true).unwrap_or_default();
        let basket_label = self.basket_label();
        let running = matches!(self.scan, Scan::Running { .. });
        let count = self.filter_count();
        let view = match (&self.tree, &self.model) {
            (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.scroll, partial: matches!(self.scan, Scan::Running { .. }), unscanned: self.unscanned(), hover: self.hover, found: self.filter_matcher(), basket: &self.basket }),
            _ => None,
        };
        let map = self.layout.map_shown().then(|| MapView { tiles: &self.map_tiles, generation: self.map_gen, now: self.map_now, selected: self.map_selected_tile(), hover: self.map_hover, classes: &self.map_classes, marked: &self.basket_view.tiles });
        f(&Scene {
            layout: self.layout.with_cancel(self.cancelable()).with_basket(self.basket_active()).with_metric(self.kind() == ViewKind::Processes),
            metric: self.proc_metric,
            theme: Theme::new(dark),
            chips: &chips,
            active_chip: self.active,
            sort: self.sort,
            kind: self.kind(),
            admin: self.can_elevate.then(|| self.layout.admin_button()),
            hot: self.hot,
            view,
            // The status bar already says what a running scan has found.
            message: if running { "" } else { &message },
            loading: running && self.tree.is_none(),
            status: &status,
            selection: &selection,
            banner: banner.as_deref(),
            filter: self.filter_bar(&count),
            map,
            basket: basket_label.as_deref(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disk_view_keeps_its_titles_sorts_and_last_two_columns() {
        assert_eq!(ViewKind::Disk.titles(Metric::Cpu), ["Name", "Size", "% of parent", "Items", "Modified"]);
        let keys: Vec<_> = (0..5).map(|c| ViewKind::Disk.sort_key(c)).collect();
        assert_eq!(keys, [SortKey::Name, SortKey::Size, SortKey::Size, SortKey::Items, SortKey::Modified]);
        let mut b = plumb_core::TreeBuilder::new();
        let r = b.push_str(NodeId::NONE, "r", 0, plumb_core::NodeFlags::DIR);
        let f = b.push_str(r, "f", 5, plumb_core::NodeFlags::empty());
        b.set_mtime(f, std::num::NonZeroU32::new(154_000));
        let t = b.finish().unwrap();
        assert_eq!(ViewKind::Disk.last_two(&t, f), (fmt::count(1), Some(fmt::date(154_000))));
        assert_eq!(ViewKind::Disk.last_two(&t, r), (fmt::count(1), None), "no date, no fourth cell");
    }

    /// An app with a Processes view as its third drive, showing one process `ping.exe` (pid 77) under the root.
    fn process_app() -> (App, NodeId) {
        use plumb_core::procs::{Metric, Procs, Sample};
        let mut a = app();
        a.drives.push(DriveInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
        a.begin_scan(2);
        let (mut procs, mut tree) = Procs::new(Metric::Memory);
        procs.apply(&mut tree, &[Sample { pid: 77, parent: 1, created: 5, name: "ping.exe".into(), memory: 4096, cpu_100ns: 0, io_bytes: 0 }], 0);
        let id = tree.find("Processes\\ping.exe").expect("the process is under the root");
        a.finish_scan(tree, "x".into());
        (a, id)
    }

    #[test]
    fn a_process_is_coloured_by_where_its_executable_lives() {
        let sys = r"C:\Windows";
        assert_eq!(location_class(Some(r"C:\Windows\System32\svchost.exe"), sys), Class::Executable);
        assert_eq!(location_class(Some(r"c:\windows\explorer.exe"), sys), Class::Executable);
        assert_eq!(location_class(Some(r"C:\WindowsApps\x.exe"), sys), Class::Other, "a sibling folder is not the Windows folder");
        assert_eq!(location_class(Some(r"C:\Program Files (x86)\A\a.exe"), sys), Class::Document);
        assert_eq!(location_class(Some(r"C:\Users\me\AppData\Local\b.exe"), sys), Class::Code);
        assert_eq!(location_class(Some(r"D:\tools\c.exe"), sys), Class::Other);
        assert_eq!(location_class(None, sys), Class::Other);
        assert_eq!(ViewKind::Processes.class_label(Class::Code), "User profile");
        assert_eq!(ViewKind::Disk.class_label(Class::Code), "Code");
    }

    #[test]
    fn clicking_the_metric_button_cycles_memory_cpu_io_and_back() {
        let (mut a, _) = process_app();
        a.layout.map_on = true;
        let click = |a: &mut App| {
            let b = a.layout.with_metric(true).metric_button();
            a.handle(Input::Click(b.x + 5.0, b.y + 5.0))
        };
        assert_eq!(a.proc_metric, Metric::Memory);
        assert_eq!(click(&mut a), Effect::Metric(Metric::Cpu));
        a.proc_metric = Metric::Cpu; // the window applies the effect
        assert_eq!(click(&mut a), Effect::Metric(Metric::Io));
        a.proc_metric = Metric::Io;
        assert_eq!(click(&mut a), Effect::Metric(Metric::Memory));
        assert_eq!(ViewKind::Processes.titles(Metric::Io)[1], "I/O");
        assert_eq!(ViewKind::Processes.size_text(Metric::Cpu, 123), "12.3%");
        assert_eq!(ViewKind::Processes.size_text(Metric::Io, 2048), format!("{}/s", fmt::size(2048)));
        assert_eq!(ViewKind::Disk.size_text(Metric::Cpu, 2048), fmt::size(2048), "a drive ignores the metric");
    }

    #[test]
    fn a_process_view_cannot_mark() {
        let (a, id) = process_app();
        assert_eq!(a.mark_state(id), MarkState::Refused("processes cannot be deleted"));
    }

    #[test]
    fn the_process_status_line_names_the_count_and_memory() {
        assert_eq!(process_summary(412, 6 << 30, 16 << 30), format!("412 processes, {} in use of {}", fmt::size(6 << 30), fmt::size(16 << 30)));
        let (mut a, _) = process_app();
        a.set_procs(2, HashMap::new(), process_summary(1, 4096, 16 << 30), (16 << 30, 3 << 30));
        assert!(a.status().contains("1 processes, "), "{}", a.status());
        assert_eq!(a.drives[2].free, 3 << 30, "the chip follows the machine's free memory");
    }

    #[test]
    fn a_process_row_names_its_pid_and_its_menu_acts_on_its_executable() {
        let (mut a, id) = process_app();
        a.model.as_mut().unwrap().select(id);
        assert!(a.selection(true).unwrap().starts_with("ping.exe  (PID 77)"), "{:?}", a.selection(true));
        assert!(a.menu_target(id).is_none(), "no executable known: no menu");
        a.set_procs(2, HashMap::from([(77, ProcInfo { path: Some("C:\\Windows\\ping.exe".into()), created: 0 })]), String::new(), (1, 1));
        let (raw, path, mark) = a.menu_target(id).unwrap();
        assert_eq!((raw.as_str(), path.as_str()), ("", "C:\\Windows\\ping.exe"));
        assert_eq!(mark, MarkState::Refused("processes cannot be deleted"));
    }

    #[test]
    fn a_process_view_sorts_cpu_and_io_by_aux_and_shows_them_as_rates() {
        assert_eq!(ViewKind::Processes.titles(Metric::Memory)[3..], ["CPU", "I/O"]);
        assert_eq!((ViewKind::Processes.sort_key(3), ViewKind::Processes.sort_key(4)), (SortKey::Aux(0), SortKey::Aux(1)));
        let mut b = plumb_core::TreeBuilder::new();
        let r = b.push_str(NodeId::NONE, "r", 0, plumb_core::NodeFlags::DIR);
        let mut t = b.finish().unwrap();
        assert_eq!(ViewKind::Processes.last_two(&t, r), ("0.0%".into(), Some(format!("{}/s", fmt::size(0)))), "no aux yet");
        t.set_aux(r, [255, 2048, 99]);
        assert_eq!(ViewKind::Processes.last_two(&t, r), ("25.5%".into(), Some(format!("{}/s", fmt::size(2048)))));
    }
    use crate::layout::{CHIP_GAP, CHIP_W, HEADER_H, PICKER_H, ROW_H};
    use plumb_core::synth::kary_builder;

    fn app() -> App {
        let drives = vec![
            DriveInfo { root: "C:\\".into(), total: 1000, free: 250, kind: ViewKind::Disk },
            DriveInfo { root: "D:\\".into(), total: 1000, free: 900, kind: ViewKind::Disk },
        ];
        App::new(Layout::new(800.0, 600.0), drives, "test".into())
    }

    fn scanned() -> App {
        let mut a = app();
        a.begin_scan(0);
        a.finish_scan(kary_builder(300, 3).finish().unwrap(), "done".into());
        a
    }

    #[test]
    fn chips_are_sized_to_their_labels() {
        let mut a = app();
        a.drives[1].root = "D:\\Mounts\\A long mount name\\".into();
        a.fit_chips(&|s| s.chars().count() as f32 * 7.0);
        let w: Vec<f32> = (0..2).map(|i| a.layout.chip(i).w).collect();
        let labels: Vec<usize> = a.chips().iter().map(|c| c.label.chars().count()).collect();
        assert_eq!(w, vec![(labels[0] as f32 * 7.0 + 16.0).ceil(), (labels[1] as f32 * 7.0 + 16.0).ceil()]);
        assert!(w[1] > w[0], "a longer label gets a wider chip");
        assert_eq!(a.handle(Input::Click(a.layout.chip(1).x + 2.0, 10.0)), Effect::Scan(1), "the wide chip is clickable along its width");
    }

    #[test]
    fn a_mount_point_is_titled_by_its_path_and_chipped_by_its_name() {
        let drive = |root: &str| DriveInfo { root: root.into(), total: 1, free: 1, kind: ViewKind::Disk };
        let (c, m) = (drive("C:\\"), drive("D:\\Mounts\\Data\\"));
        assert_eq!((c.title(), c.chip_name()), ("C:", "C:"));
        assert_eq!((m.title(), m.chip_name()), ("D:\\Mounts\\Data", "Data"));
    }

    /// The status line minus the selection details that lead it.
    fn status_tail(a: &App) -> String {
        let s = a.status();
        match a.selection(true) {
            Some(sel) => s.strip_prefix(&sel).map_or(s.clone(), |t| t.strip_prefix("   |   ").unwrap_or(t).to_string()),
            None => s,
        }
    }

    fn row_y(i: usize) -> f32 {
        PICKER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    #[test]
    fn clicking_a_chip_requests_a_scan_and_another_drive_replaces_one_in_flight() {
        let mut a = app();
        assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0)), Effect::Scan(1));
        a.begin_scan(1);
        assert_eq!(a.handle(Input::Click(10.0, 10.0)), Effect::Scan(0), "another drive replaces the scan");
        assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0)), Effect::None, "the same drive is left alone");
    }

    #[test]
    fn the_cancel_button_and_escape_cancel_a_scan_only_while_one_runs() {
        let mut a = app();
        assert_eq!(a.handle(Input::Escape), Effect::None);
        a.begin_scan(0);
        let b = a.layout.with_cancel(true).cancel_button();
        assert_eq!(a.handle(Input::Click(b.x + 5.0, b.y + 5.0)), Effect::Cancel);
        assert_eq!(a.handle(Input::Escape), Effect::Cancel);
        // Esc that clears the filter does not cancel.
        a.filter.ui.focus = true;
        assert_ne!(a.handle(Input::Escape), Effect::Cancel);
        a.cancel_scan();
        assert!(a.message().starts_with("Scan cancelled") && a.tree.is_none() && !a.cancelable());
        assert_eq!(a.handle(Input::Escape), Effect::None);
    }

    #[test]
    fn switching_drives_keeps_each_drives_tree_selection_and_scan_apart() {
        let mut a = scanned();
        let sel = a.model.as_ref().and_then(|m| m.selected());
        a.refreshing = true;
        let mut away = Parked::default();
        a.switch_slot(&mut away);
        assert!(a.tree.is_none() && a.model.is_none() && !a.cancelable() && !a.refreshing);
        assert!(away.has_tree());
        a.begin_scan(1);
        assert!(matches!(a.scan, Scan::Running { drive: 1, .. }) && a.cancelable());
        let mut other = Parked::default();
        a.switch_slot(&mut other); // the running scan goes away, then the first drive comes back
        a.switch_slot(&mut away);
        assert!(a.tree.is_some() && a.refreshing && a.cancelable());
        assert_eq!(a.model.as_ref().and_then(|m| m.selected()), sel);
        assert!(matches!(other.scan(), Scan::Running { drive: 1, .. }) && !other.has_tree());
    }

    #[test]
    fn switching_drives_takes_the_filter_off_the_rows_and_refilters_the_other_drive() {
        let mut a = scanned();
        let all = a.total_rows();
        a.filter.ui.text = "zzz".into();
        a.filter_changed();
        let job = a.start_filter().expect("a run");
        let h = job.names.scan(&job.matcher, &job.cancel, job.generation).unwrap();
        a.filter_ready(job.generation, h);
        assert!(a.filter_installed());
        let mut away = Parked::default();
        a.switch_slot(&mut away);
        assert!(!a.filter_installed() && a.filter_ui().text == "zzz");
        a.switch_slot(&mut away);
        assert!(!a.filter_installed() && a.filter_dirty()); // asks for a fresh run on the returning tree
        assert_eq!(a.total_rows(), all);
    }

    #[test]
    fn cancelling_a_background_rescan_keeps_the_tree() {
        let mut a = scanned();
        a.refreshing = true;
        assert!(a.cancelable());
        a.cancel_scan();
        assert!(a.tree.is_some() && !a.refreshing && matches!(a.scan, Scan::Idle));
    }

    #[test]
    fn admin_button_only_works_when_shown() {
        let mut a = app();
        let b = a.layout.admin_button();
        let (x, y) = (b.x + 5.0, b.y + 5.0);
        assert_eq!(a.handle(Input::Click(x, y)), Effect::None);
        a.can_elevate = true;
        assert_eq!(a.handle(Input::Click(x, y)), Effect::Elevate);
    }

    #[test]
    fn a_chip_shows_what_its_drive_holds_unless_it_is_on_screen() {
        let mut a = app();
        a.marks = vec![Mark::Kept, Mark::Scanning];
        let m = |a: &App| a.chips().iter().map(|c| c.mark).collect::<Vec<_>>();
        assert_eq!(m(&a)[..2], [Mark::Kept, Mark::Scanning]);
        a.active = Some(0);
        assert_eq!(m(&a)[..2], [Mark::None, Mark::Scanning]);
    }

    #[test]
    fn chips_show_free_space_and_used_fraction() {
        let c = app().chips();
        assert_eq!(c[0].label, "C:  250 B");
        assert!((c[0].used - 0.75).abs() < 1e-6);
    }

    #[test]
    fn a_preview_tree_is_browsable_and_the_final_tree_keeps_the_view() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(60, 3).finish().unwrap());
        a.preview_installed();
        assert!(matches!(a.scan, Scan::Running { .. }), "the scan is still running");
        assert!(a.model.as_ref().unwrap().rows().len() > 1, "the root is expanded");
        assert!(status_tail(&a).starts_with("Scanning"));
        // The user selects another row, then the exact tree (the same shape) arrives.
        let second = a.model.as_ref().unwrap().rows()[1].id;
        a.model.as_mut().unwrap().select(second);
        let name = a.tree.as_ref().unwrap().path(second);
        a.swap_tree(kary_builder(60, 3).finish().unwrap(), "done".into());
        assert!(matches!(a.scan, Scan::Idle));
        let sel = a.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.tree.as_ref().unwrap().path(sel), name, "the selection survives by path");
        assert!(status_tail(&a).starts_with("done"));
    }

    #[test]
    fn a_failed_scan_drops_the_preview() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.fail_scan("boom".into());
        assert!(a.tree.is_none() && a.model.is_none());
    }

    #[test]
    fn a_dropped_preview_tree_clears_the_view_but_not_the_scan() {
        let mut a = app();
        a.begin_scan(0);
        a.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.tree = None; // what the applier does on a Reset
        a.preview_installed();
        assert!(a.model.is_none());
        assert!(matches!(a.scan, Scan::Running { .. }));
    }

    #[test]
    fn finishing_a_scan_shows_an_expanded_root() {
        let a = scanned();
        let m = a.model.as_ref().unwrap();
        assert!(m.rows().len() > 1);
        assert_eq!(m.selected(), Some(m.rows()[0].id));
        assert!(matches!(a.scan, Scan::Idle));
    }

    #[test]
    fn click_selects_and_double_click_toggles() {
        let mut a = scanned();
        let before = a.model.as_ref().unwrap().rows().len();
        a.handle(Input::Click(20.0, row_y(1)));
        let id = a.model.as_ref().unwrap().rows()[1].id;
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(id));
        a.handle(Input::DoubleClick(20.0, row_y(1)));
        assert!(a.model.as_ref().unwrap().rows().len() > before);
        a.handle(Input::DoubleClick(20.0, row_y(1)));
        assert_eq!(a.model.as_ref().unwrap().rows().len(), before);
    }

    #[test]
    fn clicking_a_folders_icon_toggles_it_and_a_double_click_on_it_toggles_again() {
        let mut a = scanned();
        let before = a.model.as_ref().unwrap().rows().len();
        let depth = a.model.as_ref().unwrap().rows()[1].depth as f32;
        let icon_x = a.layout.column(0).0 + 8.0 + depth * INDENT + 4.0;
        a.handle(Input::Click(icon_x, row_y(1)));
        let opened = a.model.as_ref().unwrap().rows().len();
        assert!(opened > before, "one click on the icon opens the folder");
        a.handle(Input::DoubleClick(icon_x, row_y(1)));
        assert_eq!(a.model.as_ref().unwrap().rows().len(), before, "the second press of a pair toggles again");
        a.handle(Input::Click(icon_x + 200.0, row_y(1)));
        assert_eq!(a.model.as_ref().unwrap().rows().len(), before, "a click on the name only selects");
    }

    #[test]
    fn dragging_a_column_edge_widens_the_column_and_release_ends_it() {
        let mut a = scanned();
        let y = a.layout.header().y + 4.0;
        let x = a.layout.column(1).0;
        let w = a.layout.column(1).1;
        assert!(a.over_divider(x, y));
        a.handle(Input::Click(x, y));
        a.handle(Input::Drag(x - 30.0, y));
        assert_eq!(a.layout.column(1).1, w + 30.0);
        assert!(a.over_divider(0.0, 0.0), "while dragging the cursor stays a resize cursor");
        a.handle(Input::Release);
        a.handle(Input::Drag(x - 60.0, y));
        assert_eq!(a.layout.column(1).1, w + 30.0, "released: further moves do nothing");
        assert!(!a.over_divider(0.0, 0.0));
    }

    #[test]
    fn the_unscanned_bytes_are_the_drives_used_space_less_what_the_scan_found_and_vanish_past_it() {
        let mut a = app();
        assert_eq!(a.unscanned(), None, "no scan, nothing to estimate");
        a.begin_scan(0); // drive C: 1000 total, 250 free
        assert_eq!(a.unscanned(), Some(750));
        a.progress(1, 1, 700);
        assert_eq!(a.unscanned(), Some(50));
        a.progress(1, 1, 751);
        assert_eq!(a.unscanned(), None, "the scan counted more than the drive says is used");
    }

    #[test]
    fn the_icon_buttons_have_tooltips_and_nothing_else_does() {
        let mut a = app();
        a.can_elevate = true;
        let (admin, regex) = (a.layout.admin_button(), a.layout.filter_regex());
        assert_eq!(a.tip_at(admin.x + 5.0, admin.y + 5.0).as_deref(), Some("Run as administrator"));
        assert_eq!(a.tip_at(regex.x + 5.0, regex.y + 5.0).as_deref(), Some("Regular expression"));
        assert_eq!(a.tip_at(a.layout.chip(0).x + 5.0, 10.0).as_deref(), Some("C: 250 B free of 1000 B"), "a chip gives its free space in full");
        assert_eq!(a.tip_at(a.layout.header().x + 5.0, a.layout.header().y + 5.0), None);
        a.can_elevate = false;
        assert_eq!(a.tip_at(admin.x + 5.0, admin.y + 5.0), None, "a hidden button has none");
    }

    #[test]
    fn the_icon_button_under_the_pointer_is_hot() {
        let mut a = app();
        a.can_elevate = true;
        let admin = a.layout.admin_button();
        a.handle(Input::Hover(admin.x + 5.0, admin.y + 5.0));
        assert_eq!(a.hot, Some(admin));
        a.handle(Input::Hover(a.layout.chip(0).x + 5.0, 10.0));
        assert_eq!(a.hot, Some(a.layout.chip(0)), "a drive chip lights up too");
        a.handle(Input::Hover(5.0, a.layout.header().y + 5.0));
        assert_eq!(a.hot, None);
    }

    #[test]
    fn only_empty_picker_bar_space_is_title_bar() {
        let mut a = app();
        a.can_elevate = true;
        let chip = a.layout.chip(0);
        let admin = a.layout.admin_button();
        assert!(!a.is_caption(chip.x + 5.0, chip.y + 5.0), "a drive chip is a control");
        assert!(!a.is_caption(admin.x + 5.0, admin.y + 5.0), "so is the administrator button");
        assert!(a.is_caption(a.layout.chip(5).x, 3.0), "past the chips the bar is draggable");
        assert!(!a.is_caption(300.0, PICKER_H + 5.0), "below the bar is content");
        a.can_elevate = false;
        assert!(a.is_caption(admin.x + 5.0, admin.y + 5.0), "no button, no control");
    }

    #[test]
    fn a_double_click_on_a_header_sorts_again() {
        let mut a = scanned();
        let h = a.layout.column(1).0 + 20.0;
        let y = a.layout.header().y + 4.0;
        let first = a.sort;
        a.handle(Input::Click(h, y));
        let once = a.sort;
        a.handle(Input::DoubleClick(h, y));
        assert_ne!(first, once);
        assert_ne!(once, a.sort, "the second press of the pair flips the sort again");
    }

    #[test]
    fn right_click_selects_and_asks_for_a_menu() {
        let mut a = scanned();
        let id = a.model.as_ref().unwrap().rows()[2].id;
        assert_eq!(a.handle(Input::RightClick(20.0, row_y(2))), Effect::Menu(id));
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(id));
    }

    #[test]
    fn wheel_and_keys_keep_the_selection_visible_and_scroll_in_range() {
        let mut a = scanned();
        let m = a.model.as_mut().unwrap();
        let t = a.tree.as_ref().unwrap();
        for _ in 0..8 {
            let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
            if let Some(r) = r { m.expand(t, r); }
        }
        let n = a.model.as_ref().unwrap().rows().len();
        let vis = a.layout.rows_visible();
        assert!(n > vis, "need more rows than fit: {n} vs {vis}");
        a.handle(Input::Wheel(10_000));
        assert_eq!(a.scroll, n - vis);
        a.handle(Input::Wheel(-10_000));
        assert_eq!(a.scroll, 0);
        a.handle(Input::Key(Key::End));
        assert_eq!(a.scroll, n - vis);
        a.handle(Input::Key(Key::Home));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn header_click_sorts_and_second_click_reverses() {
        let mut a = scanned();
        let name = |a: &App, i: usize| a.tree.as_ref().unwrap().name_string(a.model.as_ref().unwrap().rows()[i].id);
        let hy = PICKER_H + 2.0;
        assert_eq!(a.sort(), (1, true)); // default: Size descending
        a.handle(Input::Click(5.0, hy));
        assert_eq!(a.sort(), (0, false)); // Name ascending
        let first = name(&a, 1);
        a.handle(Input::Click(5.0, hy));
        assert_eq!(a.sort(), (0, true)); // Name descending
        assert_ne!(name(&a, 1), first);
    }

    #[test]
    fn dragging_the_thumb_scrolls() {
        let mut a = scanned();
        let t = a.tree.as_ref().unwrap();
        let m = a.model.as_mut().unwrap();
        for _ in 0..8 {
            let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
            if let Some(r) = r { m.expand(t, r); }
        }
        let total = m.rows().len();
        let th = a.layout.thumb(total, 0).unwrap();
        a.handle(Input::Click(795.0, th.y + 2.0));
        a.handle(Input::Drag(795.0, th.y + 2.0 + 100.0));
        assert!(a.scroll > 0);
        a.handle(Input::Release);
        let s = a.scroll;
        a.handle(Input::Drag(795.0, th.y + 300.0));
        assert_eq!(a.scroll, s);
    }

    #[test]
    fn a_stale_banner_shifts_the_table_and_its_button_asks_for_a_rescan() {
        let mut a = scanned();
        let rows = a.layout.rows_visible();
        a.set_live(LiveStatus::Stale("the journal is off".into()));
        assert!(a.layout.banner && a.layout.rows_visible() < rows);
        assert_eq!(a.banner().unwrap(), "live updates stopped: the journal is off; the tree may be out of date");
        let b = a.layout.rescan_button();
        assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0)), Effect::Rescan(0));
        a.refreshing = true;
        assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0)), Effect::None);
        a.handle(Input::Resize(900.0, 700.0));
        assert!(a.layout.banner, "a resize keeps the banner");
        a.set_live(LiveStatus::Off);
        assert!(!a.layout.banner && a.banner().is_none());
    }

    #[test]
    fn remembered_columns_and_sort_come_back_and_nonsense_is_ignored() {
        let mut a = app();
        a.layout.set_col_w(2, 140.0);
        a.sort = (3, false);
        let mut b = app();
        b.restore(&a.remembered());
        assert_eq!((b.sort(), b.layout.col_w), (a.sort(), a.layout.col_w));
        let mut c = app();
        let before = (c.sort(), c.layout.col_w);
        c.restore(&State { window: None, columns: Some(vec![f32::NAN, 1.0e9, -5.0]), sort: Some(Sort { column: 9, descending: true }), map: Default::default() });
        assert_eq!(c.sort(), before.0, "an unknown column is ignored");
        assert!(c.layout.col_w.iter().all(|w| w.is_finite() && *w > 0.0), "widths stay clamped");
        // A state file from before the Modified column has three widths: they apply, and the new column keeps its default.
        let mut d = app();
        d.restore(&State::parse("columns = [111.0, 99.0, 77.0]\n[sort]\ncolumn = 4\ndescending = true\n"));
        assert_eq!(d.layout.col_w[..3], [111.0, 99.0, 77.0]);
        assert_eq!(d.layout.col_w[3], crate::layout::COL_W[3]);
        assert_eq!(d.sort(), (4, true), "and the new column can be the remembered sort");
    }

    #[test]
    fn the_status_line_leads_with_the_selected_rows_path_size_and_items() {
        let mut a = scanned();
        let id = a.model.as_ref().unwrap().rows()[1].id;
        a.model.as_mut().unwrap().select(id);
        let tree = a.tree.as_ref().unwrap();
        let want = format!(
            "{}  ({}, {} items)   |   done",
            fmt::display_path(&tree.path(id)),
            fmt::size(tree.subtree_size[id]),
            fmt::count(tree.subtree_files[id] as u64)
        );
        assert_eq!(a.status(), want);
    }

    #[test]
    fn the_status_line_shows_the_live_marker_and_refreshing() {
        let mut a = scanned();
        assert_eq!(status_tail(&a), "done");
        a.set_live(LiveStatus::On { last: None });
        assert_eq!(status_tail(&a), "done   |   live");
        a.live_changed("12:03:41".into());
        a.refreshing = true;
        assert_eq!(status_tail(&a), "done   |   live, last change 12:03:41   |   refreshing\u{2026}");
        a.set_live(LiveStatus::Stale("x".into()));
        a.live_changed("12:04:00".into());
        assert!(a.status().contains("live updates stopped") && !a.status().contains("12:04"));
    }

    #[test]
    fn the_status_bar_shows_a_compact_line_and_its_tooltip_the_whole_one_and_the_adapter() {
        let mut a = scanned();
        let id = a.model.as_ref().unwrap().rows()[1].id;
        a.model.as_mut().unwrap().select(id);
        a.set_live(LiveStatus::On { last: None });
        a.live_changed("12:03:41".into());
        let tree = a.tree.as_ref().unwrap();
        let size = format!("{}, {} items", fmt::size(tree.subtree_size[id]), fmt::count(tree.subtree_files[id] as u64));
        assert_eq!(a.status_line(false), format!("{size}   |   done   |   live"), "no path, no time of the last change");
        assert!(a.status().contains("12:03:41") && a.status().contains(&fmt::display_path(&tree.path(id))));
        a.adapter = "Test GPU".into();
        let bar = a.layout.status();
        assert_eq!(a.tip_at(bar.x + 40.0, bar.y + 5.0), Some(format!("{}
Test GPU", a.status())));
    }

    #[test]
    fn a_tip_keeps_its_target_while_its_text_changes_and_loses_it_on_leaving() {
        let mut a = scanned();
        let bar = a.layout.status();
        let (x, y) = (bar.x + 40.0, bar.y + 5.0);
        let (key, before) = a.tip_target(x, y).unwrap();
        a.notice = "something happened".into();
        let (same, after) = a.tip_target(x, y).unwrap();
        assert_eq!(key, same, "the status bar is the same target");
        assert_ne!(before, after, "whose text moved on");
        a.can_elevate = true;
        let other = a.tip_target(a.layout.admin_button().x + 5.0, a.layout.admin_button().y + 5.0).map(|t| t.0);
        assert!(other.is_some() && other != Some(key), "another target is another key");
    }

    #[test]
    fn a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row() {
        let mut a = scanned();
        a.handle(Input::Click(5.0, PICKER_H + 2.0)); // sort by Name ascending
        let t = a.tree.as_ref().unwrap();
        let m = a.model.as_mut().unwrap();
        let first = m.rows()[1].id;
        m.expand(t, first);
        let kid = m.rows()[2].id;
        m.select(kid);
        a.scroll = 0;
        let path = a.tree.as_ref().unwrap().path(kid);
        a.refreshing = true;
        a.swap_tree(kary_builder(300, 3).finish().unwrap(), "again".into());
        let (t, m) = (a.tree.as_ref().unwrap(), a.model.as_ref().unwrap());
        assert_eq!(t.path(m.selected().unwrap()), path);
        assert_eq!(a.sort(), (0, false));
        assert!(m.is_expanded(m.rows()[1].id));
        assert_eq!(a.summary, "again");
        assert!(!a.refreshing);
    }

    #[test]
    fn refresh_rows_picks_up_tree_changes_and_keeps_the_selection_in_place() {
        let mut a = scanned();
        let sel = a.model.as_ref().unwrap().rows()[3].id;
        a.model.as_mut().unwrap().select(sel);
        let n = a.model.as_ref().unwrap().rows().len();
        let root = a.model.as_ref().unwrap().rows()[0].id;
        a.tree.as_mut().unwrap().add_child(root, &[b'n' as u16, b'w' as u16], 1 << 40, plumb_core::NodeFlags::empty());
        a.refresh_rows();
        let m = a.model.as_ref().unwrap();
        assert_eq!(m.rows().len(), n + 1);
        assert_eq!(m.selected(), Some(sel));
    }

    #[test]
    fn resize_reclamps_scroll() {
        let mut a = scanned();
        a.scroll = 5;
        a.handle(Input::Resize(800.0, 2000.0));
        assert_eq!(a.scroll, 0);
    }

    fn with_map() -> App {
        let mut a = scanned();
        a.layout.map_on = true;
        a.update_map(1.0);
        a
    }

    fn centre(t: &treemap::Tile) -> (f32, f32) {
        (t.rect.x + t.rect.w / 2.0, t.rect.y + t.rect.h / 2.0)
    }

    #[test]
    fn the_map_follows_the_trees_selection_and_ignores_its_own_clicks() {
        let mut a = with_map();
        assert_eq!(a.map_root, Some(NodeId(0)));
        a.handle(Input::Click(20.0, row_y(1)));
        a.update_map(1.0); // the hook that notices the selection moved
        let sel = a.model.as_ref().unwrap().selected();
        assert_eq!(a.map_root, treemap::root_for(a.tree.as_ref().unwrap(), sel), "a click in the tree re-roots the map");
        let (i, t) = a.map_tiles().iter().copied().enumerate().find(|(_, t)| t.kind == Kind::File).unwrap();
        let before = a.map_root;
        let (x, y) = centre(&t);
        a.handle(Input::Click(x, y));
        a.update_map(1.0);
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
        assert_eq!(a.map_root, before, "a click on the map keeps the root");
        assert_eq!(a.map_selected_tile(), Some(i));
        assert!(a.model.as_ref().unwrap().selected_index().is_some(), "its row is revealed in the tree");
    }

    #[test]
    fn double_clicking_a_folder_tile_roots_the_map_there() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::Frame).unwrap();
        let (x, y) = (t.rect.x + 0.5, t.rect.y + 0.5); // the frame's border, clear of its children
        a.handle(Input::Click(x, y));
        a.handle(Input::DoubleClick(x, y));
        assert_eq!(a.map_root, Some(t.node));
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
        a.update_map(1.0);
        assert!(a.map_tiles().iter().all(|k| k.depth > 0 || a.tree.as_ref().unwrap().parent[k.node] == t.node || k.kind == Kind::Rest));
    }

    #[test]
    fn a_double_click_on_a_file_tile_only_selects_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        let before = a.map_root;
        a.handle(Input::DoubleClick(x, y));
        assert_eq!((a.map_root, a.model.as_ref().unwrap().selected()), (before, Some(t.node)));
    }

    #[test]
    fn hovering_a_tile_gives_a_tip_and_leaving_the_map_clears_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        a.handle(Input::Hover(x, y));
        assert!(a.map_hover.is_some());
        let tip = a.tip_at(x, y).unwrap();
        assert!(tip.contains(&a.tree.as_ref().unwrap().name_string(t.node)), "{tip}");
        a.handle(Input::Hover(5.0, 5.0));
        assert_eq!((a.map_hover, a.tip_at(5.0, 5.0)), (None, None));
    }

    #[test]
    fn a_rest_tile_says_how_many_items_it_holds() {
        let tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Rest, bytes: 2048, count: 7, mtime: None };
        assert!(tile_tip(&tree, &t, 150_000).starts_with("7 smaller items, "));
        let f = treemap::Tile { node: NodeId(1), kind: Kind::File, count: 1, ..t };
        assert!(tile_tip(&tree, &f, 150_000).contains(&tree.name_string(NodeId(1))));
    }

    #[test]
    fn a_tile_tip_says_when_the_file_was_last_written() {
        let mut tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(1), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::File, bytes: 2048, count: 1, mtime: None };
        assert!(!tile_tip(&tree, &t, 154_000).contains("modified"));
        tree.set_mtime(NodeId(1), std::num::NonZeroU32::new(134_774 + 19_791));
        let tip = tile_tip(&tree, &t, 134_774 + 19_791 + 766);
        assert!(tip.ends_with(", modified 2024-03-09, 2.1 y ago)"), "{tip}");
    }

    #[test]
    fn right_clicking_a_tile_selects_it_and_asks_for_the_menu() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        assert_eq!(a.handle(Input::RightClick(x, y)), Effect::Menu(t.node));
        assert_eq!(a.model.as_ref().unwrap().selected(), Some(t.node));
    }

    #[test]
    fn the_toggle_and_the_divider_work_and_stay_within_limits() {
        let mut a = scanned();
        assert!(!a.layout.map_on);
        a.handle(Input::ToggleMap);
        assert!(a.layout.map_on);
        let b = a.layout.map_button();
        a.handle(Input::Click(b.x + 2.0, b.y + 2.0));
        assert!(!a.layout.map_on, "the status bar button toggles too");
        a.layout.map_on = true;
        a.layout.set_map_w(170.0); // at 800 DIPs the pane can reach 247: room to grow by 50
        let (d, w0) = (a.layout.map_divider().unwrap(), a.layout.map_w);
        assert!(a.over_divider(d.x + 1.0, d.y + 5.0));
        a.handle(Input::Click(d.x + 1.0, d.y + 5.0));
        a.handle(Input::Drag(d.x - 49.0, d.y + 5.0));
        assert_eq!(a.layout.map_w, w0 + 50.0, "dragging left widens the pane");
        a.handle(Input::Drag(-10_000.0, 0.0));
        assert!(a.layout.column(0).1 >= crate::layout::MIN_NAME_W, "the tree keeps its minimum");
        a.handle(Input::Drag(10_000.0, 0.0));
        assert_eq!(a.layout.map().unwrap().w, crate::layout::MAP_MIN_W, "and the pane keeps its own");
        a.handle(Input::Release);
        let before = a.layout.map_w;
        a.handle(Input::Drag(0.0, 0.0));
        assert_eq!(a.layout.map_w, before, "released");
    }

    #[test]
    fn the_map_survives_a_tree_swap_and_an_emptied_root() {
        let mut a = with_map();
        a.swap_tree(kary_builder(50, 3).finish().unwrap(), "again".into());
        a.update_map(1.0);
        assert!(a.map_root.is_some() && !a.map_tiles().is_empty());
        a.begin_scan(1);
        a.update_map(1.0);
        assert!(a.map_tiles().is_empty() && a.map_hover.is_none(), "no tree, no tiles");
    }

    #[test]
    fn the_map_pane_state_is_remembered() {
        let mut a = app();
        a.layout.map_on = false;
        a.layout.map_w = 300.0;
        let s = a.remembered();
        let mut b = app();
        b.restore(&s);
        assert_eq!((b.layout.map_on, b.layout.map_w), (false, 300.0));
        let mut c = app();
        c.restore(&crate::state::State { map: crate::state::MapState { visible: true, width: f32::NAN }, ..Default::default() });
        assert!(c.layout.map_on && c.layout.map_w.is_finite() && c.layout.map_w >= crate::layout::MAP_MIN_W);
    }

    /// A non-root file and a non-root folder that has children, and one of that folder's children.
    fn picks(a: &App) -> (NodeId, NodeId, NodeId) {
        let t = a.tree.as_ref().unwrap();
        let all = || (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none());
        let file = all().find(|&i| !t.has_children(i)).unwrap();
        let dir = all().find(|&i| t.has_children(i)).unwrap();
        (file, dir, t.children(dir).next().unwrap())
    }

    fn select(a: &mut App, id: NodeId) {
        let (t, m) = (a.tree.as_ref().unwrap(), a.model.as_mut().unwrap());
        m.reveal(t, id);
        m.select(id);
    }

    #[test]
    fn del_marks_and_unmarks_the_selection_and_the_filter_box_keeps_its_own_del() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        select(&mut a, file);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 1);
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "the second Del unmarks");
        a.handle(Input::Find); // focus the filter box
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "with the filter box focused, Del edits text and marks nothing");
        let mut none = app();
        none.handle(Input::Edit(Edit::Delete));
        assert!(none.basket().is_empty(), "no tree, nothing selected: nothing happens");
    }

    #[test]
    fn a_folder_and_its_child_marked_together_count_the_folder_once() {
        let mut a = scanned();
        let (_, dir, child) = picks(&a);
        a.toggle_mark(child);
        a.toggle_mark(dir);
        a.update_map(1.0);
        let t = a.tree.as_ref().unwrap();
        assert_eq!(a.basket_label(), Some(format!("1 marked, up to {}", fmt::size(t.subtree_size[dir]))));
        let (dir_path, dir_size) = (t.path(dir), t.subtree_size[dir]);
        let req = a.commit_request().unwrap();
        assert_eq!(req.paths, vec![dir_path], "the commit lists the folder, not its child");
        assert_eq!(req.bytes, dir_size);
    }

    #[test]
    fn roots_and_scans_are_refused_with_a_reason_that_reaches_the_status_line() {
        let mut a = scanned();
        let root = a.tree.as_ref().unwrap().roots[0];
        assert_eq!(a.mark_state(root), MarkState::Refused("a drive root cannot be deleted"));
        a.toggle_mark(root);
        assert!(a.basket().is_empty());
        assert!(a.status_line(true).starts_with("cannot mark: a drive root cannot be deleted"), "{}", a.status_line(true));
        a.handle(Input::Click(5.0, a.layout.header().y + 5.0)); // any click clears the notice
        assert!(!a.status_line(true).starts_with("cannot mark"));
        let (file, ..) = picks(&a);
        a.refreshing = true;
        assert_eq!(a.mark_state(file), MarkState::Refused("the drive is being scanned"));
    }

    #[test]
    fn marks_survive_a_drive_switch_a_rescan_keeps_them_and_a_first_scan_clears_them() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        let mut other = Parked::default();
        a.swap_slot(&mut other);
        assert!(a.basket().is_empty(), "the other drive has no marks");
        a.swap_slot(&mut other);
        assert_eq!(a.basket().len(), 1, "coming back finds them");
        let t = plumb_core::synth::kary_builder(300, 3).finish().unwrap();
        a.swap_tree(t, "rescanned".into());
        a.update_map(1.0);
        assert_eq!(a.basket().len(), 1, "a background rescan leaves them");
        assert!(a.basket_label().is_some());
        a.begin_scan(0);
        assert_eq!(a.basket().len(), 1, "rescanning the drive on screen keeps its marks");
        a.begin_scan(1);
        assert!(a.basket().is_empty(), "a first scan of another drive starts clean");
    }

    #[test]
    fn a_mark_that_can_no_longer_be_deleted_is_dropped_and_reported_with_the_result() {
        let mut a = scanned();
        let (bad, good) = {
            let t = a.tree.as_ref().unwrap();
            let mut leaves = (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none() && !t.has_children(i));
            (leaves.next().unwrap(), leaves.next().unwrap())
        };
        a.toggle_mark(bad);
        a.toggle_mark(good);
        let bad_path = a.tree.as_ref().unwrap().path(bad);
        a.system_root = bad_path.clone(); // now the first one is refused, as if renamed to an unsafe name
        let req = a.commit_request().expect("the other mark still goes");
        assert_eq!(req.paths.len(), 1);
        assert_eq!(a.basket().len(), 1, "the refused mark is dropped");
        a.finish_delete(&req.paths, 0, &[]);
        assert!(a.notice.contains("1 skipped: the Windows folder is protected"), "{}", a.notice);
        assert!(a.notice_detail.iter().any(|d| d.starts_with(&bad_path)));
        // Alone, it gives no request, only the reason.
        let mut a = scanned();
        a.toggle_mark(bad);
        a.system_root = a.tree.as_ref().unwrap().path(bad);
        assert!(a.commit_request().is_none());
        assert!(a.notice.starts_with("1 marked item skipped"), "{}", a.notice);
        assert!(a.basket().is_empty());
    }

    #[test]
    fn the_marks_of_a_drive_whose_state_was_let_go_come_back_with_its_next_scan() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        let mut gone = Parked::default();
        a.swap_slot(&mut gone); // the drive goes away with its mark
        a.stash_basket(2, &mut gone); // and is let go
        a.begin_scan(3);
        assert!(a.basket().is_empty(), "another drive does not get them");
        a.begin_scan(2);
        assert_eq!(a.basket().len(), 1, "its own scan does");
    }

    #[test]
    fn a_marked_path_that_is_gone_at_commit_is_dropped_not_deleted() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        assert!(a.tree.as_mut().unwrap().remove(file));
        assert!(a.commit_request().is_none());
        assert!(a.basket().is_empty());
        assert!(a.status_line(true).starts_with("the marked items are already gone"), "{}", a.status_line(true));
    }

    #[test]
    fn a_commit_refuses_a_busy_drive_and_a_second_delete() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        a.refreshing = true;
        assert!(a.commit_request().is_none());
        a.refreshing = false;
        assert!(a.commit_request().is_some());
        a.set_deleting(true);
        assert!(a.commit_request().is_none(), "one delete at a time");
    }

    #[test]
    fn a_finished_delete_unmarks_what_went_and_keeps_what_failed() {
        let mut a = scanned();
        let (file, dir, _) = picks(&a);
        a.toggle_mark(file);
        a.toggle_mark(dir);
        let t = a.tree.as_ref().unwrap();
        let (gone, kept) = (t.path(file), t.path(dir));
        a.set_deleting(true);
        a.finish_delete(std::slice::from_ref(&gone), 0, &[(kept.clone(), "access denied".into())]);
        assert!(a.basket().contains(&kept) && !a.basket().contains(&gone));
        assert!(a.status_line(true).starts_with("1 of 2 not deleted: access denied; still marked"), "{}", a.status_line(true));
        assert!(a.commit_request().is_some(), "the delete is over, so a new commit may start");
    }

    #[test]
    fn the_chip_is_clickable_only_while_something_is_marked() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        let chip = a.layout.with_basket(true).basket_chip();
        assert!(matches!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0)), Effect::None), "nothing marked: no chip, no effect");
        a.toggle_mark(file);
        a.update_map(1.0);
        assert!(matches!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0)), Effect::Commit));
    }

    #[test]
    fn a_deleted_folder_takes_its_childs_mark_and_a_stale_mark_is_pruned_at_commit() {
        let mut a = scanned();
        let (file, dir, child) = picks(&a);
        let t = a.tree.as_ref().unwrap();
        let (fp, dp) = (t.path(file), t.path(dir));
        a.toggle_mark(child);
        a.toggle_mark(dir);
        a.toggle_mark(file);
        a.set_deleting(true);
        a.finish_delete(std::slice::from_ref(&dp), 0, &[]);
        assert_eq!(a.basket().len(), 1, "the child's mark went with its folder");
        assert!(a.basket().contains(&fp));
        let mut b = scanned();
        let (file, dir, _) = picks(&b);
        b.toggle_mark(file);
        b.toggle_mark(dir);
        let fid = file;
        assert!(b.tree.as_mut().unwrap().remove(fid));
        let req = b.commit_request().unwrap();
        assert_eq!(req.paths.len(), 1);
        assert_eq!(b.basket().len(), 1, "the mark that no longer resolves is dropped, not kept to come back later");
    }

    #[test]
    fn a_mark_from_the_menu_is_found_again_by_path_and_a_vanished_path_marks_nothing() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        let p = a.tree.as_ref().unwrap().path(file);
        a.toggle_mark_path(&p);
        assert!(a.basket().contains(&p));
        a.toggle_mark_path("nowhere\\at all");
        assert_eq!(a.basket().len(), 1);
        assert!(a.status_line(true).starts_with("that item is gone"));
    }

    #[test]
    fn del_with_a_tree_but_nothing_selected_marks_nothing() {
        let mut a = scanned();
        a.model = Some(RowModel::new(a.tree.as_ref().unwrap()));
        assert!(a.model.as_ref().unwrap().selected().is_none());
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty());
    }

    #[test]
    fn a_renamed_marked_item_drops_off_the_chip_and_the_tiles() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        a.update_map(1.0);
        assert!(a.basket_label().is_some());
        let n: Vec<u16> = "renamed".encode_utf16().collect();
        a.tree.as_mut().unwrap().rename(file, &n);
        a.update_map(1.0);
        assert!(a.basket_label().is_none(), "the mark no longer names anything, so no chip");
        assert!(!a.basket_active());
    }

    #[test]
    fn a_delete_report_counts_missing_apart_and_keeps_every_reason_for_the_hover() {
        let mut a = scanned();
        a.set_deleting(true);
        let gone: Vec<String> = vec!["X-a".into(), "X-b".into()];
        a.finish_delete(&gone, 1, &[("X-c".into(), "in use".into()), ("X-d".into(), "denied".into())]);
        let short = a.status_line(false);
        assert!(short.starts_with("2 of 4 not deleted: in use; still marked"), "{short}");
        assert!(a.status_line(true).contains("X-d: denied"), "{}", a.status_line(true));
        let mut b = scanned();
        b.finish_delete(&gone, 1, &[]);
        assert!(b.status_line(false).starts_with("deleted 1 item, 1 already gone"), "{}", b.status_line(false));
    }

    #[test]
    fn begin_delete_refuses_a_scanning_drive_and_a_second_delete() {
        let mut a = scanned();
        assert!(a.begin_delete());
        assert!(a.deleting());
        assert!(!a.begin_delete(), "one at a time");
        a.set_deleting(false);
        a.refreshing = true;
        assert!(!a.begin_delete());
        assert!(a.status_line(true).starts_with("cannot delete while the drive is being scanned"));
    }
}
