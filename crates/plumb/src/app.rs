//! Window-independent application state: what is selected, scrolled and scanning, and how
//! input changes it. No Win32 types, so the whole interaction model tests anywhere.
mod marks;
mod filter;
mod held;
mod map;

use crate::draw::{Chip, MapView, Mark, Scene, Theme, View};
use crate::filter_ui::Edit;
use crate::fmt;
use crate::layout::{chip_width, Hit, Layout, Rect, CHIP_W, INDENT, MAP_MIN_W, PICKER_H};
use crate::modal::{Ask, Modal, Pick};
use crate::restore::Snapshot;
use crate::state::{MapState, Sort, State};
use plumb_core::basket::{self, Basket};
use plumb_core::rows::{Key, RowModel, SortKey};
use plumb_core::procs::Metric;
use plumb_scan::handles::{HandleError, HandleList};
use plumb_core::treemap::{self, Class, Kind, Tile};
use plumb_core::{NodeFlags, NodeId, Tree};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use marks::BasketView;
use held::{Held, HELD_REFRESH_SNAPSHOTS};
pub use marks::{CommitRequest, MarkState};
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
    /// `metric` is what the Processes view's first number column holds; a disk ignores it.
    pub fn titles(self, metric: Metric) -> [&'static str; 5] {
        match self {
            ViewKind::Disk => ["Name", "Size", "% of parent", "Items", "Modified"],
            ViewKind::Processes => ["Name", metric_label(metric), "% of parent", "CPU", "I/O"],
        }
    }

    /// The text of a size-column value: bytes for a disk; a process's memory, CPU share or I/O rate by `metric`.
    pub fn size_text(self, metric: Metric, v: u64) -> fmt::Show<'static> {
        match (self, metric) {
            (ViewKind::Processes, Metric::Cpu) => fmt::tenths(v),
            (ViewKind::Processes, Metric::Io) => fmt::rate(v),
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
    pub fn last_two(self, tree: &Tree, id: NodeId) -> (fmt::Show<'static>, Option<fmt::Show<'static>>) {
        match self {
            ViewKind::Disk => (fmt::count(tree.subtree_files[id] as u64), tree.mtime[id].map(|d| fmt::date(d.get()))),
            ViewKind::Processes => {
                let [cpu, io, _] = tree.aux.get(id.idx()).copied().unwrap_or_default();
                (fmt::tenths(cpu), Some(fmt::rate(io)))
            }
        }
    }
}

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

/// What the Processes view knows about a process besides what its tree holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcInfo {
    /// The executable's path; `None` when the process could not be opened (another user's, protected).
    pub path: Option<String>,
    /// Creation time, FILETIME.
    pub created: u64,
    /// Open handles when last seen; a change suggests the files held changed.
    pub handles: u32,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ViewInfo {
    /// Where the volume is mounted: a drive root like `C:\`, or a folder like `D:\Mounts\Data\`.
    pub root: String,
    pub total: u64,
    pub free: u64,
    pub kind: ViewKind,
}

impl ViewInfo {
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
    Running { view: usize, dirs: u64, files: u64, bytes: u64 },
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
    /// Rows to scroll (positive is down) with the pointer at this x and y: over the Files held pane it scrolls that.
    Wheel(i32, f32, f32),
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
    /// Start scanning the view at this index.
    Scan(usize),
    /// Show the context menu for this node.
    Menu(NodeId),
    /// Relaunch elevated.
    Elevate,
    /// Rescan the view at this index in the background, keeping the old tree on screen.
    Rescan(usize),
    /// Start the debounce timer for a filter run.
    Filter,
    /// Stop the scan in flight.
    Cancel,
    /// Delete these paths of the view, on a worker thread (a modal was answered).
    Delete { view: usize, paths: Vec<String>, permanent: bool },
    /// Close the window (the modal asked about a running delete was answered Yes).
    Quit,
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

/// What a button's tooltip says, short of writing it: the pointer moving over a button only needs its rectangle.
#[derive(Clone, Copy)]
enum ButtonTip {
    Text(&'static str),
    /// A view chip: its free space.
    Chip(usize),
    /// The map metric button: what the metric counts.
    Metric,
}

fn tile_tip(tree: &Tree, t: &Tile, now: u32, out: &mut String) {
    let _ = match t.kind {
        Kind::Rest => write!(out, "{}, {}", fmt::noun(u64::from(t.count), "smaller item", "smaller items"), fmt::size(t.bytes)),
        Kind::Own => write!(out, "{} itself  ({})", fmt::node_path(tree, t.node), fmt::size(t.bytes)),
        _ => {
            let _ = write!(out, "{}  ({}", fmt::node_path(tree, t.node), fmt::size(t.bytes));
            if let Some(d) = tree.mtime[t.node] {
                let _ = write!(out, ", modified {}, {}", fmt::date(d.get()), fmt::age(now, d.get()));
            }
            out.write_char(')')
        }
    };
}

/// Today as a day count since 1601-01-01.
fn today() -> u32 {
    plumb_core::age::day_from_unix(unix_secs())
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Now as a FILETIME (100 ns since 1601-01-01, UTC).
fn now_filetime() -> u64 {
    (unix_secs() + u64::from(plumb_core::age::UNIX_EPOCH_DAYS) * 86_400) * 10_000_000
}

/// What the app holds for one view, kept while another view is on screen. The filter box text
/// and the sort stay with the app; everything derived from this view's tree travels with it.
#[derive(Default)]
pub struct Slot {
    pub tree: Option<Tree>,
    pub model: Option<RowModel>,
    pub scroll: usize,
    pub scan: Scan,
    pub summary: String,
    pub live: LiveStatus,
    pub refreshing: bool,
    pub preview_paused: bool,
    filter: filter::Filter,
    pub basket: Basket,
    pub proc_info: HashMap<u32, ProcInfo>,
    pub proc_metric: Metric,
}

impl Slot {
    pub fn has_tree(&self) -> bool {
        self.tree.is_some()
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// The tree a kept view's running scan builds (its rows are made when the view comes back).
    pub fn tree_mut(&mut self) -> &mut Option<Tree> {
        &mut self.tree
    }

    pub fn scan(&self) -> &Scan {
        &self.scan
    }
}

pub struct App {
    pub layout: Layout,
    pub views: Vec<ViewInfo>,
    pub active: Option<usize>,
    /// What each view off screen holds, shown on its chip; set by the window.
    pub marks: Vec<Mark>,
    /// The visible row under the mouse.
    pub hover: Option<usize>,
    /// The icon button under the mouse.
    pub hot: Option<Rect>,
    /// The left button went down on `hot` and is still down.
    pub pressed: bool,
    /// What swaps when another view is put on screen.
    pub slot: Slot,
    /// Show the "Run as administrator" button (the process is not elevated).
    pub can_elevate: bool,
    /// The system's accent colour (`0xRRGGBB`), set by the window; `None` keeps plumb's blue.
    pub accent: Option<u32>,
    pub adapter: String,
    /// Header column last clicked (0 Name, 1 Size, 2 % of parent, 3 Items, 4 Modified) and direction.
    sort: (usize, bool),
    /// While dragging the scrollbar thumb: distance from the thumb's top to the grab point.
    grab: Option<f32>,
    /// The pointer is over the scrollbar thumb.
    pub thumb_hover: bool,
    /// The modals waiting to be answered, the first on screen; while any is up it takes all the input.
    modals: VecDeque<Modal>,
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
    /// The files the selected process holds (see `held_sync`).
    held: Held,
    /// Bumped when the marks change or the shown view's basket is swapped.
    basket_rev: u64,
    basket_view: BasketView,
    /// A delete is running on the worker thread.
    deleting: bool,
    /// A one-off message (a refused mark, a delete result) shown first on the status line until the next click or key.
    notice: String,
    /// Every failure of the last delete, one a line: part of the full status line (the hover text).
    notice_detail: Vec<String>,
    /// The notice reports success (nothing to alarm about); any other notice is drawn as a problem.
    notice_ok: bool,
    system_root: String,
    /// Marks of views whose kept state was let go (see `Win::evict`): a later scan of that view takes them back.
    stash: HashMap<usize, Basket>,
    /// Marks the last commit left out because they can no longer be deleted (path, why): reported with its result.
    skipped: Vec<(String, &'static str)>,
}

impl App {
    pub fn new(layout: Layout, views: Vec<ViewInfo>, adapter: String) -> Self {
        let layout = Layout { chips: vec![CHIP_W; views.len()], ..layout };
        Self {
            layout, views, adapter, active: None, marks: Vec::new(), hover: None, hot: None, pressed: false, thumb_hover: false, modals: VecDeque::new(), slot: Slot::default(),
            can_elevate: false, accent: None, sort: (1, true), grab: None, col_drag: None,
            map_root: None, map_seen: None, map_tiles: Vec::new(), map_key: None, map_dirty: true, map_gen: 0, map_now: 0, map_classes: Vec::new(), map_hover: None, map_drag: None,
            held: Held::default(), basket_rev: 0, basket_view: BasketView::default(), deleting: false, notice: String::new(), notice_detail: Vec::new(), notice_ok: false, stash: HashMap::new(), skipped: Vec::new(),
            system_root: std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        }
    }

    /// Sizes each chip to its label: `measure` gives a text's width in DIPs.
    pub fn fit_chips(&mut self, measure: &dyn Fn(&str) -> f32) {
        self.layout.chips = self.chips().iter().map(|c| chip_width(measure(&c.label))).collect();
    }

    /// The header column the rows are sorted by, and whether descending.
    /// The kind of the view on screen (a disk when none is).
    pub fn kind(&self) -> ViewKind {
        self.active.and_then(|i| self.views.get(i)).map_or(ViewKind::Disk, |d| d.kind)
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

    /// Exchanges the per-view state with `p`. The result is consistent on its own (banner and
    /// Cancel button follow the new state) but leaves the filter alone: see `switch_slot`.
    pub fn swap_slot(&mut self, p: &mut Slot) {
        use std::mem::swap;
        swap(&mut self.slot, p);
        swap(&mut self.slot.filter.ui, &mut p.filter.ui); // the query text belongs to the window
        self.basket_rev += 1;
        let procs = self.kind() == ViewKind::Processes;
        self.layout = self.layout.with_banner(matches!(self.slot.live, LiveStatus::Stale(_))).with_cancel(self.cancelable()).with_metric(procs).with_held(procs);
        self.set_scroll(self.slot.scroll);
        self.map_reset();
    }

    /// Puts another view on screen: the outgoing rows lose the filter so they come back whole, and
    /// the incoming ones are filtered afresh.
    pub fn switch_slot(&mut self, p: &mut Slot) {
        if self.slot.filter.installed.is_some() {
            self.unfilter_rows();
        }
        self.filter_reset();
        self.swap_slot(p);
        self.filter_reset();
    }

    pub fn begin_scan(&mut self, view: usize) {
        self.filter_reset();
        self.map_reset();
        if self.active != Some(view) {
            // A rescan of the view on screen keeps its marks; another view brings back the ones it was let go with.
            self.slot.basket = self.stash.remove(&view).unwrap_or_default();
            self.basket_rev += 1;
        }
        self.active = Some(view);
        self.slot.tree = None;
        self.slot.model = None;
        self.slot.scroll = 0;
        self.slot.summary.clear();
        self.set_live(LiveStatus::Off);
        self.slot.refreshing = false;
        self.slot.preview_paused = false;
        self.slot.scan = Scan::Running { view, dirs: 0, files: 0, bytes: 0 };
    }

    pub fn progress(&mut self, d: u64, f: u64, b: u64) {
        if let Scan::Running { dirs, files, bytes, .. } = &mut self.slot.scan {
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
        self.slot.tree = Some(tree);
        self.slot.model = Some(model);
        self.slot.scroll = 0;
        self.slot.summary = summary;
        self.slot.scan = Scan::Idle;
        self.slot.preview_paused = false;
        self.set_live(LiveStatus::Off);
    }

    /// The preview applier just created `self.slot.tree` or dropped it (a scanner fell back to
    /// another): build the rows for the tree, or show nothing. The scan state is untouched, so
    /// the progress line keeps counting.
    pub fn preview_installed(&mut self) {
        self.filter_reset();
        self.map_reset();
        self.slot.scroll = 0;
        self.slot.model = None;
        let key = self.kind().sort_key(self.sort.0);
        let Some(tree) = &mut self.slot.tree else { return };
        tree.take_touched(); // the model is built from the tree as it is now
        let mut model = RowModel::new(tree);
        model.set_sort(tree, key, self.sort.1);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(tree, r);
            model.select(r);
        }
        self.slot.model = Some(model);
    }

    /// Replaces the tree with a rescanned one, keeping the sort, the expanded folders and the
    /// selection (by path) and where it sits on screen.
    pub fn swap_tree(&mut self, tree: Tree, summary: String) {
        self.basket_rev += 1; // the marks are counted against the new tree
        if self.slot.filter.installed.is_some() {
            self.unfilter_rows();
        }
        let kept = match (&self.slot.tree, &self.slot.model) {
            (Some(t), Some(m)) => Some(Snapshot::take(t, m, self.slot.scroll)),
            _ => None,
        };
        self.map_reset();
        let Some(snap) = kept else { return self.finish_scan(tree, summary) };
        let (model, scroll) = snap.apply(&tree, self.kind().sort_key(self.sort.0), self.sort.1, self.layout.rows_visible());
        self.slot.tree = Some(tree);
        self.slot.model = Some(model);
        self.slot.scroll = scroll;
        self.slot.summary = summary;
        self.slot.refreshing = false;
        self.slot.scan = Scan::Idle;
        self.slot.preview_paused = false;
        self.filter_reset();
        self.set_live(LiveStatus::Off);
    }

    /// Brings the rows up to date after the tree changed in place.
    pub fn refresh_rows(&mut self) {
        self.filter_live();
        if let (Some(t), Some(m)) = (&mut self.slot.tree, &mut self.slot.model) {
            let s = m.refresh(t, self.slot.scroll);
            self.set_scroll(s);
        }
        self.map_dirty = true;
    }

    pub fn set_live(&mut self, live: LiveStatus) {
        self.layout = self.layout.with_banner(matches!(live, LiveStatus::Stale(_)));
        self.slot.live = live;
        self.set_scroll(self.slot.scroll);
    }

    /// Records the clock time of the last change applied by live updates.
    pub fn live_changed(&mut self, at: String) {
        if let LiveStatus::On { last } = &mut self.slot.live {
            *last = Some(at);
        }
    }

    /// A scan is in flight (a first scan, or a background rescan).
    pub fn cancelable(&self) -> bool {
        self.slot.refreshing || matches!(self.slot.scan, Scan::Running { .. })
    }

    /// The user cancelled the scan in flight. A first scan leaves no tree; a background rescan
    /// leaves the old one on screen.
    pub fn cancel_scan(&mut self) {
        if matches!(self.slot.scan, Scan::Running { .. }) {
            self.slot.tree = None;
            self.slot.model = None;
            self.slot.preview_paused = false;
            self.slot.scan = Scan::Cancelled;
            self.map_reset();
        }
        self.slot.refreshing = false;
    }

    pub fn fail_scan(&mut self, msg: String) {
        self.slot.tree = None;
        self.slot.model = None;
        self.slot.preview_paused = false;
        self.slot.scan = Scan::Failed(msg);
        self.map_reset();
    }

    fn total_rows(&self) -> usize {
        self.slot.model.as_ref().map_or(0, |m| m.rows().len())
    }

    /// The scrollbar thumb, if the rows overflow.
    fn thumb(&self) -> Option<Rect> {
        self.layout.thumb(self.total_rows(), self.slot.scroll)
    }

    fn max_scroll(&self) -> usize {
        self.total_rows().saturating_sub(self.layout.rows_visible())
    }

    fn set_scroll(&mut self, s: usize) {
        self.slot.scroll = s.min(self.max_scroll());
    }

    /// The folder whose icon is at x on visible row `i`, if `x` is over that icon and the folder can expand.
    fn icon_at(&self, i: usize, x: f32) -> Option<NodeId> {
        let (t, m) = (self.slot.tree.as_ref()?, self.slot.model.as_ref()?);
        let row = m.rows().get(self.slot.scroll + i)?;
        let left = self.layout.column(0).0 + 8.0 + row.depth as f32 * INDENT;
        (x >= left && x < left + 16.0 && m.is_expandable(t, row.id)).then_some(row.id)
    }

    /// Whether the pointer is on a column edge in the header (or one is being dragged), for the resize cursor.
    pub fn over_divider(&self, x: f32, y: f32) -> bool {
        self.modal().is_none()
            && (self.col_drag.is_some()
                || self.map_drag.is_some()
                || matches!(self.layout.hit(x, y, self.views.len()), Hit::Divider(_) | Hit::MapDivider))
    }

    /// The button at (x, y), if there is one there, with its tooltip: an icon button carries no label, and a
    /// view chip's label shows only the free size, to fit. These are what light up under the pointer.
    fn button_at(&self, x: f32, y: f32) -> Option<(Rect, ButtonTip)> {
        let l = &self.layout;
        if self.can_elevate && l.admin_button().contains(x, y) {
            return Some((l.admin_button(), ButtonTip::Text("Run as administrator")));
        }
        match l.hit(x, y, self.views.len()) {
            Hit::Chip(i) => self.views.get(i).map(|_| (l.chip(i), ButtonTip::Chip(i))),
            Hit::Rescan => Some((l.rescan_button(), ButtonTip::Text("Rescan"))),
            Hit::Cancel => Some((l.cancel_button(), ButtonTip::Text("Cancel scan"))),
            Hit::Basket => Some((l.basket_chip(), ButtonTip::Text("Review and delete the marked items"))),
            Hit::FilterRegex => Some((l.filter_regex(), ButtonTip::Text("Regular expression"))),
            Hit::FilterClear => Some((l.filter_clear(), ButtonTip::Text("Clear the filter"))),
            Hit::MapButton => Some((l.map_button(), ButtonTip::Text("Show or hide the map  (Ctrl+M)"))),
            Hit::MetricButton => Some((l.metric_button(), ButtonTip::Metric)),
            _ => None,
        }
    }

    fn write_button_tip(&self, tip: ButtonTip, out: &mut String) {
        match tip {
            ButtonTip::Text(s) => out.push_str(s),
            ButtonTip::Chip(i) => {
                if let Some(d) = self.views.get(i) {
                    let _ = write!(out, "{} {} free of {}", d.title(), fmt::size(d.free), fmt::size(d.total));
                }
            }
            ButtonTip::Metric => {
                let _ = write!(out, "The map and the first column weigh processes by {}: {}; click for the next", metric_label(self.slot.proc_metric), metric_help(self.slot.proc_metric));
            }
        }
    }

    /// The tooltip under (x, y): what it is about, and its text. The text may change while the target stays
    /// (a scan keeps adding to the totals), so the window compares targets to decide when to hide the tip.
    pub fn tip_target(&self, x: f32, y: f32, out: &mut String) -> Option<TipKey> {
        out.clear();
        if let Some((r, tip)) = self.button_at(x, y) {
            self.write_button_tip(tip, out);
            return Some(TipKey::Area(r));
        }
        let bar = self.layout.status();
        if bar.contains(x, y) {
            self.write_status(true, true, true, out);
            let _ = write!(out, "\n{}", self.adapter);
            return Some(TipKey::Area(bar));
        }
        let t = self.map_tiles.get(treemap::hit(&self.map_tiles, x, y).filter(|_| matches!(self.layout.hit(x, y, self.views.len()), Hit::Map))?)?;
        let tree = self.slot.tree.as_ref()?;
        if self.kind() == ViewKind::Processes { self.process_tip(tree, t, out) } else { tile_tip(tree, t, self.map_now, out) }
        Some(TipKey::Tile(t.node, t.kind))
    }

    /// A map tile of the Processes view: name and pid, the numbers, the executable and when it started.
    fn process_tip(&self, tree: &Tree, t: &Tile, out: &mut String) {
        let (metric, size) = (metric_noun(self.slot.proc_metric), self.kind().size_text(self.slot.proc_metric, t.bytes));
        if t.kind == Kind::Rest {
            // A parent whose own size is under the cutoff is merged here too, with no Own tile of its own.
            let own = tree.size[t.node] > 0 && !self.map_tiles.iter().any(|o| o.kind == Kind::Own && o.node == t.node);
            let n = u64::from(t.count) - u64::from(own);
            if n > 0 {
                let _ = write!(out, "{}", fmt::noun(n, "smaller process", "smaller processes"));
            }
            if n > 0 && own {
                out.push_str(" and ");
            }
            if own {
                let _ = write!(out, "its own {metric}");
            }
            let _ = write!(out, ", {size}");
            return;
        }
        let [cpu, io, pid] = tree.aux.get(t.node.idx()).copied().unwrap_or_default();
        let name = fmt::node_name(tree, t.node);
        if t.kind == Kind::Own {
            let _ = write!(out, "{name}  (PID {pid})\n{size} {metric} in the process itself");
            return;
        }
        let scope = if tree.flags[t.node].contains(NodeFlags::DIR) { "this and its children" } else { "this process" };
        let _ = write!(out, "{name}  (PID {pid})\n{size} {metric} in {scope}; own CPU {}, I/O {}", fmt::tenths(cpu), fmt::rate(io));
        if let Some(info) = self.slot.proc_info.get(&(pid as u32)) {
            if let Some(path) = &info.path {
                out.push('\n');
                out.push_str(path);
            }
            if info.created > 0 {
                let (day, now) = (fmt::local_day(info.created), fmt::local_day(now_filetime()));
                let _ = write!(out, "\nstarted {}, {}", fmt::date(day), fmt::age(now, day));
            }
        }
    }

    /// The Processes status line: how many processes, and what the map's metric adds up to over all of them (so the
    /// total says what it counts, and only Private bytes can honestly be set beside the machine's RAM).
    fn write_process_status(&self, out: &mut String) -> bool {
        let Some(tree) = self.slot.tree.as_ref() else { return false };
        let Some(&root) = tree.roots.first() else { return false };
        let total = tree.subtree_size[root];
        let _ = write!(out, "{}, {} {}", fmt::noun(self.slot.proc_info.len() as u64, "process", "processes"), self.kind().size_text(self.slot.proc_metric, total), metric_noun(self.slot.proc_metric));
        true
    }

    /// The Processes view took a new snapshot: the facts about each process and the chip's memory.
    pub fn set_procs(&mut self, view: usize, info: HashMap<u32, ProcInfo>, ram: (u64, u64)) {
        self.slot.proc_info = info;
        if let Some(d) = self.views.get_mut(view) {
            (d.total, d.free) = ram;
        }
        self.held.age = self.held.age.saturating_add(1);
        if self.held_readable() && !self.held.refreshing && self.held.age >= HELD_REFRESH_SNAPSHOTS && self.held_handles().is_some_and(|n| n != self.held.handles) {
            self.held.stale = true;
        }
    }

    /// What the context menu of `id` acts on: the path in the tree's terms (to mark by) and as a file system path
    /// (to reveal and copy). A process offers its executable; none when that is unknown.
    /// The last element is whether to offer "Who is using this": a file on a disk.
    pub fn menu_target(&self, id: NodeId) -> Option<(String, String, MarkState, bool)> {
        let tree = self.slot.tree.as_ref()?;
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx())?[2] as u32;
            let path = self.slot.proc_info.get(&pid)?.path.clone()?;
            return Some((String::new(), path, self.mark_state(id), false));
        }
        let file = !tree.flags[id].contains(NodeFlags::DIR);
        Some((tree.path(id), fmt::node_path(tree, id).to_string(), self.mark_state(id), file))
    }

    /// The tooltip text under (x, y), written to `out`; false (with `out` empty) for none.
    pub fn tip_at(&self, x: f32, y: f32, out: &mut String) -> bool {
        out.clear();
        self.modal().is_none() && self.tip_target(x, y, out).is_some()
    }

    /// The modal on screen, if any.
    pub fn modal(&self) -> Option<&Modal> {
        self.modals.front()
    }

    /// Where the on-screen modal's button with this meaning is, if it has one.
    pub fn modal_button(&self, pick: Pick) -> Option<Rect> {
        let m = self.modal()?;
        m.geometry(self.layout.w, self.layout.h).buttons.get(m.index_of(pick)?).copied()
    }

    /// Puts a modal up (after those already waiting).
    pub fn ask(&mut self, m: Modal) {
        self.modals.push_back(m);
        self.hot = None;
        self.hover = None;
        self.thumb_hover = false;
    }

    /// Asks how to delete the marked items (the basket chip), if there are any that can be.
    pub fn ask_delete(&mut self) {
        if let Some(req) = self.commit_request() {
            self.ask(Modal::delete(req));
        }
    }

    /// The window is closing while a delete runs; asked once.
    pub fn ask_close(&mut self) {
        if !self.modals.iter().any(|m| m.ask == Ask::Close) {
            self.ask(Modal::closing());
        }
    }

    /// The first modal was answered: what it was about and the answer decide what happens next.
    fn answer(&mut self, pick: Pick) -> Effect {
        let Some(m) = self.modals.pop_front() else { return Effect::None };
        self.hot = None;
        match (m.ask, pick) {
            (Ask::Delete(r), Pick::Recycle) => Effect::Delete { view: r.view, paths: r.paths, permanent: false },
            (Ask::Delete(r), Pick::Permanent) => {
                self.modals.push_front(Modal::permanent(r));
                Effect::None
            }
            (Ask::Delete(_), Pick::Unmark) => {
                self.clear_basket();
                Effect::None
            }
            (Ask::Permanent(r), Pick::Yes) => Effect::Delete { view: r.view, paths: r.paths, permanent: true },
            (Ask::Failed { view, permanent, paths }, Pick::Retry) => Effect::Delete { view, paths, permanent },
            (Ask::Close, Pick::Yes) => Effect::Quit,
            _ => Effect::None,
        }
    }

    /// Input while a modal is up: it alone hears the pointer and the keys.
    fn modal_input(&mut self, input: Input) -> Effect {
        let Some(m) = self.modals.front_mut() else { return Effect::None };
        let g = m.geometry(self.layout.w, self.layout.h);
        let pick_at = |m: &Modal, x, y| g.button_at(x, y).map(|i| m.buttons[i].pick);
        match input {
            Input::Hover(x, y) => {
                self.hot = g.button_at(x, y).map(|i| g.buttons[i]);
                Effect::None
            }
            Input::Click(x, y) | Input::DoubleClick(x, y) => match pick_at(m, x, y) {
                Some(p) => self.answer(p),
                // The scrim cancels, except while a delete runs: a stray click must not decide to close the window.
                None if !g.panel.contains(x, y) && m.ask != Ask::Close => {
                    let p = m.cancel;
                    self.answer(p)
                }
                None => Effect::None,
            },
            Input::Enter => {
                let p = m.buttons[m.focus].pick;
                self.answer(p)
            }
            Input::Escape => {
                let p = m.cancel;
                self.answer(p)
            }
            Input::Tab => {
                m.step(true);
                Effect::None
            }
            Input::Key(Key::Left | Key::Up) => {
                m.step(false);
                Effect::None
            }
            Input::Key(Key::Right | Key::Down) => {
                m.step(true);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Whether (x, y) is empty title-bar space, where a drag moves the window and a double-click maximizes
    /// it: the picker bar, away from the view chips and the administrator button.
    pub fn is_caption(&self, x: f32, y: f32) -> bool {
        y < PICKER_H
            && !matches!(
                self.layout.hit(x, y, self.views.len()),
                Hit::Chip(_) | Hit::FilterBox | Hit::FilterRegex | Hit::FilterClear
            )
            && !(self.can_elevate && self.layout.admin_button().contains(x, y))
    }

    fn row_at(&self, i: usize) -> Option<NodeId> {
        self.slot.model.as_ref()?.rows().get(self.slot.scroll + i).map(|r| r.id)
    }

    pub fn handle(&mut self, input: Input) -> Effect {
        self.layout.cancel = self.cancelable();
        self.layout.basket = self.basket_active();
        self.layout.metric = self.kind() == ViewKind::Processes;
        self.layout.held = self.kind() == ViewKind::Processes;
        if self.modal().is_none() && matches!(input, Input::Click(..) | Input::DoubleClick(..) | Input::RightClick(..) | Input::Key(_) | Input::Edit(_) | Input::Enter | Input::Escape | Input::Find) {
            self.notice.clear();
            self.notice_detail.clear();
            self.notice_ok = false;
        }
        if self.modal().is_some() && !matches!(input, Input::Resize(..) | Input::Release) {
            return self.modal_input(input);
        }
        match input {
            Input::Resize(w, h) => {
                (self.layout.w, self.layout.h) = (w, h);
                self.set_scroll(self.slot.scroll);
            }
            Input::Wheel(rows, x, y) if self.layout.held && self.layout.held_pane().contains(x, y) => self.held_scroll(rows),
            Input::Wheel(rows, ..) => self.set_scroll((self.slot.scroll as i64 + rows as i64).max(0) as usize),
            Input::Key(k) => {
                if self.slot.filter.ui.focus
                    && let Some(e) = filter::edit_for_key(k)
                {
                    return self.filter_edit(e);
                }
                if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
                    m.handle_key(t, k, self.layout.rows_visible().max(1));
                    let s = m.scroll_to_selected(self.slot.scroll, self.layout.rows_visible());
                    self.set_scroll(s);
                }
            }
            Input::Edit(Edit::Delete) if !self.slot.filter.ui.focus => self.toggle_mark_selected(),
            Input::Edit(e) => return self.filter_edit(e),
            Input::Find => {
                self.slot.filter.ui.focus = true;
                self.slot.filter.ui.edit(Edit::SelectAll);
            }
            Input::Escape => {
                let used = self.slot.filter.ui.focus || !self.slot.filter.ui.text.is_empty();
                let e = self.filter_escape();
                return if !used && self.cancelable() { Effect::Cancel } else { e };
            }
            Input::Enter => self.filter_enter(),
            Input::Tab => self.slot.filter.ui.focus = !self.slot.filter.ui.focus,
            Input::Hover(x, y) => {
                self.hover = match self.layout.hit(x, y, self.views.len()) {
                    Hit::Row(i) if self.row_at(i).is_some() => Some(i),
                    _ => None,
                };
                self.hot = self.button_at(x, y).map(|b| b.0).or(match self.layout.hit(x, y, self.views.len()) {
                    Hit::Header(c) => Some(self.layout.header_cell(c)),
                    _ => None,
                });
                self.thumb_hover = self.thumb().is_some_and(|t| t.contains(x, y));
                self.map_hover = if matches!(self.layout.hit(x, y, self.views.len()), Hit::Map) { treemap::hit(&self.map_tiles, x, y) } else { None };
            }
            Input::ToggleMap => self.layout.map_on = !self.layout.map_on,
            Input::Click(x, y) => return self.click(x, y),
            // Windows sends the second press of a quick pair as a double-click instead of a click, so
            // anywhere but a row it must act as a click, or the second press of a header, button or
            // scrollbar would be lost.
            Input::DoubleClick(x, y) => {
                if matches!(self.layout.hit(x, y, self.views.len()), Hit::Map) {
                    if let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                        && self.slot.tree.as_ref().is_some_and(|t| t.flags[id].contains(NodeFlags::DIR))
                    {
                        self.map_root = Some(id);
                        self.map_dirty = true;
                    }
                    return Effect::None;
                }
                let Hit::Row(i) = self.layout.hit(x, y, self.views.len()) else {
                    return self.click(x, y);
                };
                if self.icon_at(i, x).is_some() {
                    return self.click(x, y);
                }
                if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.slot.tree, &mut self.slot.model) {
                    m.select(id);
                    m.toggle(t, id);
                    self.set_scroll(self.slot.scroll);
                }
            }
            Input::RightClick(x, y) => {
                if let Hit::Row(i) = self.layout.hit(x, y, self.views.len())
                    && let (Some(id), Some(m)) = (self.row_at(i), &mut self.slot.model)
                {
                    m.select(id);
                    return Effect::Menu(id);
                } else if matches!(self.layout.hit(x, y, self.views.len()), Hit::Map)
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
                self.pressed = false;
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
        let sel = self.slot.model.as_ref().and_then(|m| m.selected());
        if sel == self.map_seen && self.map_root.is_some() == self.slot.tree.is_some() {
            return;
        }
        self.map_seen = sel;
        self.map_root = self.slot.tree.as_ref().and_then(|t| treemap::root_for(t, sel));
        self.map_dirty = true;
    }

    fn click(&mut self, x: f32, y: f32) -> Effect {
        self.pressed = self.hot.is_some_and(|r| r.contains(x, y));
        if self.can_elevate && self.layout.admin_button().contains(x, y) {
            return Effect::Elevate;
        }
        let hit = self.layout.hit(x, y, self.views.len());
        match hit {
            Hit::FilterBox => {
                self.slot.filter.ui.focus = true;
                return Effect::None;
            }
            Hit::FilterRegex => {
                self.slot.filter.ui.focus = true;
                self.slot.filter.ui.regex = !self.slot.filter.ui.regex;
                return self.filter_changed();
            }
            Hit::FilterClear => {
                self.slot.filter.ui.focus = true;
                return if self.slot.filter.ui.clear() { self.filter_changed() } else { Effect::None };
            }
            _ => self.slot.filter.ui.focus = false,
        }
        match hit {
            Hit::Cancel => return Effect::Cancel,
            Hit::Basket => self.ask_delete(),
            // Picking another view mid-scan cancels the scan in flight; the same view does nothing.
            Hit::Chip(i) if !(matches!(self.slot.scan, Scan::Running { .. }) && self.active == Some(i)) => return Effect::Scan(i),
            Hit::Rescan => {
                if let Some(i) = self.active.filter(|_| !self.slot.refreshing && !matches!(self.slot.scan, Scan::Running { .. })) {
                    return Effect::Rescan(i);
                }
            }
            Hit::Header(c) => {
                let desc = if self.sort.0 == c { !self.sort.1 } else { c != 0 };
                self.sort = (c, desc);
                let kind = self.kind();
                if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
                    m.set_sort(t, kind.sort_key(c), desc);
                    self.slot.scroll = 0;
                }
            }
            Hit::Divider(c) => self.col_drag = Some((c, x, self.layout.col_w[c - 1])),
            Hit::MapButton => self.layout.map_on = !self.layout.map_on,
            Hit::MetricButton => return Effect::Metric(self.slot.proc_metric.next()),
            Hit::MapDivider => self.map_drag = Some((x, self.layout.map_w)),
            Hit::Map => {
                if let Some(i) = treemap::hit(&self.map_tiles, x, y) {
                    self.select_from_map(i);
                }
            }
            Hit::Row(i) => {
                let folder = self.icon_at(i, x);
                let mut again = false;
                if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.slot.tree, &mut self.slot.model) {
                    again = folder.is_none() && m.selected() == Some(id);
                    m.select(id);
                    if folder.is_some() {
                        m.toggle(t, id);
                        self.set_scroll(self.slot.scroll);
                    }
                }
                if again {
                    self.held_mark_stale();
                }
            }
            Hit::Scrollbar => {
                let total = self.total_rows();
                match self.layout.thumb(total, self.slot.scroll) {
                    Some(t) if y >= t.y && y < t.y + t.h => self.grab = Some(y - t.y),
                    Some(t) => {
                        let page = self.layout.rows_visible().max(1);
                        self.set_scroll(if y < t.y { self.slot.scroll.saturating_sub(page) } else { self.slot.scroll + page });
                    }
                    None => {}
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub fn chips(&self) -> Vec<Chip> {
        self.views
            .iter()
            .enumerate()
            .map(|(i, d)| Chip {
                label: format!("{}  {}", d.chip_name(), fmt::size(d.free)),
                used: 1.0 - fmt::fraction(d.free, d.total),
                mark: if self.active == Some(i) { Mark::None } else { self.marks.get(i).copied().unwrap_or_default() },
            })
            .collect()
    }

    /// What the list says when there is no tree: a heading, then (after a newline) a hint or the reason.
    fn message(&self) -> String {
        let mut s = String::new();
        self.write_message(&mut s);
        s
    }

    fn write_message(&self, out: &mut String) {
        match &self.slot.scan {
            Scan::Idle => out.push_str("No drive selected\nSelect a drive above to scan it."),
            Scan::Running { view, .. } if self.views[*view].kind == ViewKind::Processes => out.push_str("Reading processes\u{2026}"),
            Scan::Running { view, dirs, files, bytes } => {
                let _ = write!(
                    out,
                    "Scanning {}  {}, {}, {}",
                    self.views[*view].title(), fmt::noun(*dirs, "folder", "folders"), fmt::noun(*files, "file", "files"), fmt::size(*bytes)
                );
            }
            Scan::Failed(m) => {
                let _ = write!(out, "Scan failed\n{m}");
            }
            Scan::Cancelled => out.push_str("Scan cancelled\nSelect a drive above to scan it again."),
        }
    }

    /// The selected row's size and (for a folder) item count, led by its full path when `full`; first in the status line.
    fn selection(&self, full: bool) -> Option<String> {
        let mut s = String::new();
        self.write_selection(full, &mut s).then_some(s)
    }

    /// Writes [`App::selection`]; false (writing nothing) when no row is selected.
    fn write_selection(&self, full: bool, out: &mut String) -> bool {
        let (Some(tree), Some(id)) = (self.slot.tree.as_ref(), self.slot.model.as_ref().and_then(|m| m.selected())) else { return false };
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2]);
            let _ = write!(out, "{}  (PID {pid})  {}", fmt::node_name(tree, id), self.kind().size_text(self.slot.proc_metric, tree.subtree_size[id]));
            return true;
        }
        if full {
            let _ = write!(out, "{}  (", fmt::node_path(tree, id));
        }
        let _ = write!(out, "{}", fmt::size(tree.subtree_size[id]));
        if tree.flags[id].contains(NodeFlags::DIR) {
            let _ = write!(out, ", {}", fmt::noun(tree.subtree_files[id] as u64, "item", "items"));
        }
        if full {
            out.push(')');
        }
        true
    }

    /// The status line without the selection and the notice: the status bar's second row, the selection having a row of its
    /// own and the notice being drawn first, in its own colour.
    fn state_line(&self) -> String {
        let mut s = String::new();
        self.write_status(false, false, false, &mut s);
        s
    }

    /// Writes the whole status line (the status bar's tooltip carries it) to `out`, which it appends to: the compact line
    /// (`full` false) has no path and no time of the last change.
    fn write_status(&self, full: bool, with_selection: bool, with_notice: bool, out: &mut String) {
        let start = out.len();
        let sep = |out: &mut String| {
            if out.len() > start {
                out.push_str("   |   ");
            }
        };
        if with_notice && !self.notice.is_empty() {
            sep(out);
            out.push_str(&self.notice);
            if full && !self.notice_detail.is_empty() {
                sep(out);
                for (i, d) in self.notice_detail.iter().enumerate() {
                    if i > 0 {
                        out.push_str("; ");
                    }
                    out.push_str(d);
                }
            }
        }
        if with_selection {
            let mark = out.len();
            sep(out);
            if !self.write_selection(full, out) {
                out.truncate(mark);
            }
        }
        sep(out);
        match &self.slot.scan {
            Scan::Running { .. } => {
                self.write_message(out);
                if self.slot.preview_paused {
                    out.push_str("   (preview paused)");
                }
            }
            Scan::Failed(_) | Scan::Cancelled => self.write_message(out),
            _ if self.kind() == ViewKind::Processes => {
                if !self.write_process_status(out) {
                    self.write_message(out);
                }
            }
            _ if self.slot.summary.is_empty() => self.write_message(out),
            _ => out.push_str(&self.slot.summary),
        }
        match &self.slot.live {
            LiveStatus::Off => {}
            LiveStatus::On { last: None } | LiveStatus::On { last: Some(_) } if !full => {
                sep(out);
                out.push_str("live");
            }
            LiveStatus::On { last: None } => {
                sep(out);
                out.push_str("live");
            }
            LiveStatus::On { last: Some(t) } => {
                sep(out);
                let _ = write!(out, "live, last change {t}");
            }
            LiveStatus::Stale(_) => {
                sep(out);
                out.push_str("live updates stopped");
            }
        }
        if self.slot.refreshing {
            sep(out);
            out.push_str("refreshing\u{2026}");
        }
    }

    fn banner(&self) -> Option<String> {
        match &self.slot.live {
            LiveStatus::Stale(why) => Some(format!("live updates stopped: {why}; the tree may be out of date")),
            _ => None,
        }
    }

    /// What the running scan has yet to find, by the drive's used space; `None` when no scan runs or it
    /// has already counted more than the drive reports used (hard links, placeholders and compressed
    /// files can make it, and then the used space says nothing about what is left).
    pub fn unscanned(&self) -> Option<u64> {
        let Scan::Running { view, bytes, .. } = self.slot.scan else { return None };
        let d = self.views.get(view)?;
        d.total.saturating_sub(d.free).checked_sub(bytes)
    }

    /// Builds the scene for `dark` and passes it to `f`; the scene borrows locals made here.
    pub fn with_scene<R>(&self, dark: bool, f: impl FnOnce(&Scene) -> R) -> R {
        let chips = self.chips();
        let (message, status, banner) = (self.message(), self.state_line(), self.banner());
        let held = self.held_text();
        let selection = self.selection(true).unwrap_or_default();
        let basket_label = self.basket_label();
        let running = matches!(self.slot.scan, Scan::Running { .. });
        let count = self.filter_count();
        let view = match (&self.slot.tree, &self.slot.model) {
            (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.slot.scroll, partial: matches!(self.slot.scan, Scan::Running { .. }), unscanned: self.unscanned(), hover: self.hover, found: self.filter_matcher(), basket: &self.slot.basket }),
            _ => None,
        };
        let map = self.layout.map_shown().then(|| MapView { tiles: &self.map_tiles, generation: self.map_gen, now: self.map_now, selected: self.map_selected_tile(), hover: self.map_hover, classes: &self.map_classes, marked: &self.basket_view.tiles });
        f(&Scene {
            layout: self.layout.with_cancel(self.cancelable()).with_basket(self.basket_active()).with_metric(self.kind() == ViewKind::Processes).with_held(self.kind() == ViewKind::Processes),
            metric: self.slot.proc_metric,
            held: held.as_ref().map(|(h, l)| (h.as_str(), *l)),
            theme: Theme::with_accent(dark, self.accent),
            chips: &chips,
            active_chip: self.active,
            sort: self.sort,
            kind: self.kind(),
            admin: self.can_elevate.then(|| self.layout.admin_button()),
            // The scrollbar thumb counts as a control under the pointer, and is pressed while it is dragged.
            hot: self.hot.or(self.thumb().filter(|_| self.thumb_hover || self.grab.is_some())),
            pressed: self.pressed || self.grab.is_some(),
            modal: self.modals.front(),
            view,
            // The status bar already says what a running scan has found.
            message: if running { "" } else { &message },
            loading: running && self.slot.tree.is_none(),
            status: &status,
            notice: &self.notice,
            notice_ok: self.notice_ok,
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

    /// The text-returning forms of the tooltip and status builders, which write into a buffer.
    fn tip_str(a: &App, x: f32, y: f32) -> Option<String> {
        let mut s = String::new();
        a.tip_at(x, y, &mut s).then_some(s)
    }

    fn target_str(a: &App, x: f32, y: f32) -> Option<(TipKey, String)> {
        let mut s = String::new();
        a.tip_target(x, y, &mut s).map(|k| (k, s))
    }

    fn tile_str(tree: &Tree, t: &Tile, now: u32) -> String {
        let mut s = String::new();
        tile_tip(tree, t, now, &mut s);
        s
    }

    fn proc_tip_str(a: &App, tree: &Tree, t: &Tile) -> String {
        let mut s = String::new();
        a.process_tip(tree, t, &mut s);
        s
    }

    fn status_str(a: &App, full: bool) -> String {
        let mut s = String::new();
        a.write_status(full, true, true, &mut s);
        s
    }

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

    /// An app with a Processes view as its third view, showing one process `ping.exe` (pid 77) under the root.
    fn process_app() -> (App, NodeId) {
        use plumb_core::procs::{Metric, Procs, Sample};
        let mut a = app();
        a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
        a.begin_scan(2);
        let (mut procs, mut tree) = Procs::new(Metric::Memory);
        procs.apply(&mut tree, &[Sample { pid: 77, parent: 1, created: 5, name: "ping.exe".into(), memory: 4096, private: 2048, cpu_100ns: 0, io_bytes: 0 }], 0);
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
    fn clicking_the_metric_button_cycles_memory_private_cpu_io_and_back() {
        let (mut a, _) = process_app();
        a.layout.map_on = true;
        let click = |a: &mut App| {
            let b = a.layout.with_metric(true).metric_button();
            a.handle(Input::Click(b.x + 5.0, b.y + 5.0))
        };
        assert_eq!(a.slot.proc_metric, Metric::Memory);
        assert_eq!(click(&mut a), Effect::Metric(Metric::Private));
        a.slot.proc_metric = Metric::Private; // the window applies the effect
        assert_eq!(click(&mut a), Effect::Metric(Metric::Cpu));
        a.slot.proc_metric = Metric::Cpu; // the window applies the effect
        assert_eq!(click(&mut a), Effect::Metric(Metric::Io));
        a.slot.proc_metric = Metric::Io;
        assert_eq!(click(&mut a), Effect::Metric(Metric::Memory));
        assert_eq!(ViewKind::Processes.titles(Metric::Io)[1], "I/O");
        assert_eq!(ViewKind::Processes.titles(Metric::Private)[1], "Private");
        assert_eq!(ViewKind::Processes.size_text(Metric::Private, 2048), fmt::size(2048));
        assert_eq!(ViewKind::Processes.size_text(Metric::Cpu, 123), "12.3%");
        assert_eq!(ViewKind::Processes.size_text(Metric::Io, 2048), fmt::rate(2048));
        assert_eq!(ViewKind::Disk.size_text(Metric::Cpu, 2048), fmt::size(2048), "a disk ignores the metric");
    }

    #[test]
    fn every_metric_has_a_label_and_a_one_line_definition() {
        for m in [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io] {
            assert!(!metric_label(m).is_empty() && !metric_help(m).is_empty(), "{m:?}");
        }
        assert!(metric_help(Metric::Private).contains("not shared"));
        assert!(metric_help(Metric::Memory).contains("working set"));
    }

    #[test]
    fn who_is_using_this_is_offered_for_a_file_on_a_drive_only() {
        let a = scanned();
        let tree = a.slot.tree.as_ref().unwrap();
        let dir = a.slot.model.as_ref().unwrap().rows()[0].id;
        let mut file = dir;
        while tree.flags[file].contains(NodeFlags::DIR) {
            file = tree.children(file).next().expect("every folder in the tree has something in it");
        }
        assert!(a.menu_target(file).unwrap().3, "a file");
        assert!(!a.menu_target(dir).unwrap().3, "a folder has no holders to ask about");
    }

    #[test]
    fn selecting_a_process_requests_its_files_once_it_settles() {
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        assert!(a.held_sync(), "the selection is a process");
        assert!(!a.held_sync(), "the same selection again changes nothing");
        assert_eq!(a.held_request().map(|r| r.1), Some(77));
        assert_eq!(a.held_request(), None, "asked once");
        assert!(a.held_text().unwrap().0.contains("ping.exe (PID 77)"), "{:?}", a.held_text());
    }

    #[test]
    fn a_lookup_in_flight_survives_slot_swaps_and_epochs_never_repeat() {
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (first, _) = a.held_request().unwrap();
        let mut other = Slot::default();
        a.swap_slot(&mut other); // a parked view's journal event swaps a slot in and out
        a.swap_slot(&mut other);
        a.held_done(first, Ok(HandleList { files: vec!["C:/x".into()], skipped: 0 }));
        assert!(a.held_text().unwrap().0.ends_with("1 file"), "{:?}", a.held_text());
        let root = a.slot.model.as_ref().unwrap().rows()[0].id;
        a.slot.model.as_mut().unwrap().select(root);
        a.held_sync();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (second, _) = a.held_request().unwrap();
        assert!(second > first, "{second} after {first}");
    }

    #[test]
    fn a_stale_result_for_a_previous_selection_is_ignored() {
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (old, _) = a.held_request().unwrap();
        let root = a.slot.model.as_ref().unwrap().rows()[0].id;
        a.slot.model.as_mut().unwrap().select(root); // the root: pid 0, no pane
        assert!(!a.held_sync());
        assert!(a.held_text().is_none());
        a.held_done(old, Ok(HandleList { files: vec!["C:/x".into()], skipped: 0 }));
        assert!(a.held_text().is_none(), "the answer for the old selection is dropped");
    }

    #[test]
    fn the_pane_reports_files_denied_and_overflow() {
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (g, _) = a.held_request().unwrap();
        a.held_done(g, Err(HandleError::Denied));
        assert!(a.held_text().unwrap().0.contains("run as administrator"));
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (g, _) = a.held_request().unwrap();
        let files: Vec<String> = (0..9).map(|i| format!("C:/f{i}")).collect();
        a.held_done(g, Ok(HandleList { files, skipped: 2 }));
        let (head, lines) = a.held_text().unwrap();
        assert!(head.ends_with("9 files (1\u{2013}6), 2 not readable"), "{head}");
        assert_eq!((lines.len(), lines.last().unwrap().as_str()), (crate::layout::HELD_ROWS, "C:/f5"));
        // The wheel over the pane scrolls it, clamped to the list; elsewhere it scrolls the rows; a new selection starts over.
        let pane = a.layout.held_pane();
        let (px, py) = (pane.x + 10.0, pane.y + 10.0);
        a.handle(Input::Wheel(2, px, py));
        let (head, lines) = a.held_text().unwrap();
        assert_eq!((head.contains("(3\u{2013}8)"), lines[0].as_str(), a.slot.scroll), (true, "C:/f2", 0), "{head}");
        a.handle(Input::Wheel(100, px, py));
        assert_eq!(a.held_text().unwrap().1[0], "C:/f3", "clamped so the last line is the last file");
        a.handle(Input::Wheel(-100, px, py));
        assert_eq!(a.held_text().unwrap().1[0], "C:/f0");
        a.handle(Input::Wheel(1, px, 5.0));
        assert_eq!(a.held_text().unwrap().1[0], "C:/f0", "over the rows it does not scroll the pane");
        a.handle(Input::Wheel(2, px, py));
        a.held.pid = None;
        a.held_sync();
        assert_eq!(a.held.scroll, 0, "a new selection starts at the top");
        // A process with nothing open says so in its title and has no lines.
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (g, _) = a.held_request().unwrap();
        a.held_done(g, Ok(HandleList { files: Vec::new(), skipped: 0 }));
        let (head, lines) = a.held_text().unwrap();
        assert!(head.ends_with("0 files") && lines.is_empty(), "{head}");
    }

    /// One Processes snapshot as `set_procs` sees it: pid 77 with `handles` open handles.
    fn snapshot_with(a: &mut App, handles: u32) {
        let info = HashMap::from([(77u32, ProcInfo { path: None, created: 5, handles })]);
        a.set_procs(2, info, (16 << 30, 4 << 30));
    }

    /// `process_app` with pid 77 selected, its first lookup asked and answered with `files`.
    fn held_ready(files: &[&str]) -> App {
        let (mut a, id) = process_app();
        snapshot_with(&mut a, 100);
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (epoch, _) = a.held_request().unwrap();
        a.held_done(epoch, Ok(HandleList { files: files.iter().map(|s| s.to_string()).collect(), skipped: 0 }));
        a
    }

    #[test]
    fn a_changed_handle_count_refreshes_the_list_after_ten_snapshots() {
        let mut a = held_ready(&["C:/a"]);
        for _ in 0..HELD_REFRESH_SNAPSHOTS - 1 {
            snapshot_with(&mut a, 120);
            assert_eq!(a.held_take_refresh(), None, "changed, but not ten snapshots since the lookup");
        }
        snapshot_with(&mut a, 120);
        let (epoch, pid) = a.held_take_refresh().expect("ten snapshots and a changed count");
        assert_eq!(pid, 77);
        assert_eq!(a.held_take_refresh(), None, "asked once");
        assert_eq!(a.held_text().unwrap().1, vec!["C:/a".to_string()], "the old list stays until the answer");
        a.held_done(epoch, Ok(HandleList { files: vec!["C:/b".into()], skipped: 0 }));
        assert_eq!(a.held_text().unwrap().1, vec!["C:/b".to_string()]);
    }

    #[test]
    fn an_unchanged_handle_count_never_refreshes() {
        let mut a = held_ready(&["C:/a"]);
        for _ in 0..3 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 100);
            assert_eq!(a.held_take_refresh(), None);
        }
    }

    #[test]
    fn a_busy_process_is_looked_up_at_most_once_per_ten_snapshots_and_never_twice_at_once() {
        let mut a = held_ready(&["C:/a"]);
        let mut asked = 0;
        for n in 0..4 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 200 + n); // a different count every snapshot
            if let Some((epoch, _)) = a.held_take_refresh() {
                asked += 1;
                snapshot_with(&mut a, 500 + n);
                assert_eq!(a.held_take_refresh(), None, "one is already in flight");
                a.held_done(epoch, Ok(HandleList::default()));
            }
        }
        assert!(asked <= 4, "{asked} lookups in {} snapshots", 4 * HELD_REFRESH_SNAPSHOTS);
        assert!(asked >= 3, "{asked}: a changing count must keep refreshing");
    }

    #[test]
    fn clicking_the_selected_row_again_refreshes_at_once() {
        let mut a = held_ready(&["C:/a"]);
        a.held_mark_stale(); // what a click on the selected row does
        let (epoch, pid) = a.held_take_refresh().expect("a re-click asks again without waiting for ten snapshots");
        assert_eq!(pid, 77);
        a.held_done(epoch, Ok(HandleList { files: vec!["C:/c".into()], skipped: 0 }));
        assert_eq!(a.held_text().unwrap().1, vec!["C:/c".to_string()]);
    }

    #[test]
    fn a_refresh_keeps_the_scroll_clamped_and_a_late_answer_for_another_selection_is_dropped() {
        let many: Vec<String> = (0..20).map(|i| format!("C:/f{i:02}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut a = held_ready(&refs);
        a.held_scroll(100); // as far down as it goes
        a.held_mark_stale();
        let (epoch, _) = a.held_take_refresh().unwrap();
        let shorter = HandleList { files: many[..crate::layout::HELD_ROWS + 1].to_vec(), skipped: 0 };
        a.held_done(epoch, Ok(shorter));
        let (_, lines) = a.held_text().unwrap();
        assert_eq!((lines.len(), lines[0].as_str()), (crate::layout::HELD_ROWS, "C:/f01"), "the offset is clamped to the shorter list");
        // a refresh answer arriving after the selection moved is dropped
        let mut b = held_ready(&["C:/x"]);
        b.held_mark_stale();
        let (e2, _) = b.held_take_refresh().unwrap();
        let root = b.slot.model.as_ref().unwrap().rows()[0].id;
        b.slot.model.as_mut().unwrap().select(root);
        b.held_sync();
        b.held_done(e2, Ok(HandleList { files: vec!["C:/y".into()], skipped: 0 }));
        assert!(b.held_text().is_none(), "no pane for the root, and the old answer did not revive one");
    }

    #[test]
    fn a_denied_or_failed_lookup_is_not_refreshed() {
        let (mut a, id) = process_app();
        snapshot_with(&mut a, 100);
        a.slot.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (epoch, _) = a.held_request().unwrap();
        a.held_done(epoch, Err(HandleError::Denied));
        for _ in 0..2 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 999);
            assert_eq!(a.held_take_refresh(), None);
        }
        a.held_mark_stale();
        assert_eq!(a.held_take_refresh(), None, "nothing to refresh when it could not be read");
    }

    #[test]
    fn a_process_tip_names_its_metric_and_its_scope() {
        let (a, id) = process_app(); // a leaf: no children
        let tree = a.slot.tree.as_ref().unwrap();
        let own = Tile { node: id, rect: plumb_core::Rect::default(), depth: 0, class: Class::Other, kind: Kind::Frame, bytes: 4096, count: 1, mtime: None };
        let tip = proc_tip_str(&a, tree, &own);
        assert!(tip.contains("memory in this process; own CPU"), "{tip}");
        assert!(!tip.contains("children"), "a process with none does not mention them: {tip}");
        for (m, noun) in [(Metric::Memory, "memory"), (Metric::Private, "private"), (Metric::Cpu, "CPU"), (Metric::Io, "I/O")] {
            assert_eq!(metric_noun(m), noun);
        }
    }

    #[test]
    fn the_rest_tile_of_a_process_view_counts_processes_and_names_a_merged_own_size() {
        let (mut a, id) = process_app(); // ping.exe: 4096 own bytes under the root
        let rest = |count| Tile { node: id, rect: plumb_core::Rect::default(), depth: 0, class: Class::Other, kind: Kind::Rest, bytes: 4096, count, mtime: None };
        let tree = a.slot.tree.take().unwrap();
        let tip = |a: &App, t: &Tile| proc_tip_str(a, &tree, t);
        assert_eq!(tip(&a, &rest(1)), format!("its own memory, {}", fmt::size(4096)), "only its own size merged");
        assert_eq!(tip(&a, &rest(3)), format!("2 smaller processes and its own memory, {}", fmt::size(4096)));
        a.map_tiles = vec![Tile { kind: Kind::Own, count: 1, ..rest(1) }]; // the own tile is drawn: the merged items are processes
        assert_eq!(tip(&a, &rest(1)), format!("1 smaller process, {}", fmt::size(4096)));
        assert_eq!(tip(&a, &rest(2)), format!("2 smaller processes, {}", fmt::size(4096)));
    }

    #[test]
    fn a_process_view_cannot_mark() {
        let (a, id) = process_app();
        assert_eq!(a.mark_state(id), MarkState::Refused("processes cannot be deleted"));
    }

    #[test]
    fn the_process_status_line_totals_what_the_metric_counts() {
        let (mut a, _) = process_app();
        a.set_procs(2, HashMap::from([(77u32, ProcInfo::default())]), (16 << 30, 3 << 30));
        assert!(status_str(&a, true).ends_with(&format!("1 process, {} memory", fmt::size(4096))), "{}", status_str(&a, true));
        assert_eq!(a.views[2].free, 3 << 30, "the chip follows the machine's free memory");
    }

    #[test]
    fn a_process_row_names_its_pid_and_its_menu_acts_on_its_executable() {
        let (mut a, id) = process_app();
        a.slot.model.as_mut().unwrap().select(id);
        assert!(a.selection(true).unwrap().starts_with("ping.exe  (PID 77)"), "{:?}", a.selection(true));
        assert!(a.menu_target(id).is_none(), "no executable known: no menu");
        a.set_procs(2, HashMap::from([(77, ProcInfo { path: Some("C:\\Windows\\ping.exe".into()), created: 0, handles: 0 })]), (1, 1));
        let (raw, path, mark, who) = a.menu_target(id).unwrap();
        assert!(!who, "a process has no holders to ask about");
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
        assert_eq!(ViewKind::Processes.last_two(&t, r), (fmt::tenths(0), Some(fmt::rate(0))), "no aux yet");
        t.set_aux(r, [255, 2048, 99]);
        assert_eq!(ViewKind::Processes.last_two(&t, r), (fmt::tenths(255), Some(fmt::rate(2048))));
    }
    use crate::layout::{CHIP_GAP, CHIP_W, HEADER_H, PICKER_H, ROW_H};
    use plumb_core::synth::kary_builder;

    fn app() -> App {
        let views = vec![
            ViewInfo { root: "C:\\".into(), total: 1000, free: 250, kind: ViewKind::Disk },
            ViewInfo { root: "D:\\".into(), total: 1000, free: 900, kind: ViewKind::Disk },
        ];
        App::new(Layout::new(800.0, 600.0), views, "test".into())
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
        a.views[1].root = "D:\\Mounts\\A long mount name\\".into();
        a.fit_chips(&|s| s.chars().count() as f32 * 7.0);
        let w: Vec<f32> = (0..2).map(|i| a.layout.chip(i).w).collect();
        let labels: Vec<usize> = a.chips().iter().map(|c| c.label.chars().count()).collect();
        assert_eq!(w, vec![(labels[0] as f32 * 7.0 + 16.0).ceil(), (labels[1] as f32 * 7.0 + 16.0).ceil()]);
        assert!(w[1] > w[0], "a longer label gets a wider chip");
        assert_eq!(a.handle(Input::Click(a.layout.chip(1).x + 2.0, 10.0)), Effect::Scan(1), "the wide chip is clickable along its width");
    }

    #[test]
    fn a_mount_point_is_titled_by_its_path_and_chipped_by_its_name() {
        let view = |root: &str| ViewInfo { root: root.into(), total: 1, free: 1, kind: ViewKind::Disk };
        let (c, m) = (view("C:\\"), view("D:\\Mounts\\Data\\"));
        assert_eq!((c.title(), c.chip_name()), ("C:", "C:"));
        assert_eq!((m.title(), m.chip_name()), ("D:\\Mounts\\Data", "Data"));
    }

    /// The status line minus the selection details that lead it.
    fn status_tail(a: &App) -> String {
        let s = status_str(a, true);
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
        assert_eq!(a.handle(Input::Click(10.0, 10.0)), Effect::Scan(0), "another view replaces the scan");
        assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0)), Effect::None, "the same view is left alone");
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
        a.slot.filter.ui.focus = true;
        assert_ne!(a.handle(Input::Escape), Effect::Cancel);
        a.cancel_scan();
        assert!(a.message().starts_with("Scan cancelled") && a.slot.tree.is_none() && !a.cancelable());
        assert_eq!(a.handle(Input::Escape), Effect::None);
    }

    #[test]
    fn switching_drives_keeps_each_drives_tree_selection_and_scan_apart() {
        let mut a = scanned();
        let sel = a.slot.model.as_ref().and_then(|m| m.selected());
        a.slot.refreshing = true;
        let mut away = Slot::default();
        a.switch_slot(&mut away);
        assert!(a.slot.tree.is_none() && a.slot.model.is_none() && !a.cancelable() && !a.slot.refreshing);
        assert!(away.has_tree());
        a.begin_scan(1);
        assert!(matches!(a.slot.scan, Scan::Running { view: 1, .. }) && a.cancelable());
        let mut other = Slot::default();
        a.switch_slot(&mut other); // the running scan goes away, then the first view comes back
        a.switch_slot(&mut away);
        assert!(a.slot.tree.is_some() && a.slot.refreshing && a.cancelable());
        assert_eq!(a.slot.model.as_ref().and_then(|m| m.selected()), sel);
        assert!(matches!(other.scan(), Scan::Running { view: 1, .. }) && !other.has_tree());
    }

    #[test]
    fn switching_drives_takes_the_filter_off_the_rows_and_refilters_the_other_drive() {
        let mut a = scanned();
        let all = a.total_rows();
        a.slot.filter.ui.text = "zzz".into();
        a.filter_changed();
        let job = a.start_filter().expect("a run");
        let h = job.names.scan(&job.matcher, &job.cancel, job.generation).unwrap();
        a.filter_ready(job.generation, h);
        assert!(a.filter_installed());
        let mut away = Slot::default();
        a.switch_slot(&mut away);
        assert!(!a.filter_installed() && a.filter_ui().text == "zzz");
        a.switch_slot(&mut away);
        assert!(!a.filter_installed() && a.filter_dirty()); // asks for a fresh run on the returning tree
        assert_eq!(a.total_rows(), all);
    }

    #[test]
    fn cancelling_a_background_rescan_keeps_the_tree() {
        let mut a = scanned();
        a.slot.refreshing = true;
        assert!(a.cancelable());
        a.cancel_scan();
        assert!(a.slot.tree.is_some() && !a.slot.refreshing && matches!(a.slot.scan, Scan::Idle));
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
        a.slot.tree = Some(kary_builder(60, 3).finish().unwrap());
        a.preview_installed();
        assert!(matches!(a.slot.scan, Scan::Running { .. }), "the scan is still running");
        assert!(a.slot.model.as_ref().unwrap().rows().len() > 1, "the root is expanded");
        assert!(status_tail(&a).starts_with("Scanning"));
        // The user selects another row, then the exact tree (the same shape) arrives.
        let second = a.slot.model.as_ref().unwrap().rows()[1].id;
        a.slot.model.as_mut().unwrap().select(second);
        let name = a.slot.tree.as_ref().unwrap().path(second);
        a.swap_tree(kary_builder(60, 3).finish().unwrap(), "done".into());
        assert!(matches!(a.slot.scan, Scan::Idle));
        let sel = a.slot.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.slot.tree.as_ref().unwrap().path(sel), name, "the selection survives by path");
        assert!(status_tail(&a).starts_with("done"));
    }

    #[test]
    fn a_failed_scan_drops_the_preview() {
        let mut a = app();
        a.begin_scan(0);
        a.slot.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.fail_scan("boom".into());
        assert!(a.slot.tree.is_none() && a.slot.model.is_none());
    }

    #[test]
    fn a_dropped_preview_tree_clears_the_view_but_not_the_scan() {
        let mut a = app();
        a.begin_scan(0);
        a.slot.tree = Some(kary_builder(10, 2).finish().unwrap());
        a.preview_installed();
        a.slot.tree = None; // what the applier does on a Reset
        a.preview_installed();
        assert!(a.slot.model.is_none());
        assert!(matches!(a.slot.scan, Scan::Running { .. }));
    }

    #[test]
    fn finishing_a_scan_shows_an_expanded_root() {
        let a = scanned();
        let m = a.slot.model.as_ref().unwrap();
        assert!(m.rows().len() > 1);
        assert_eq!(m.selected(), Some(m.rows()[0].id));
        assert!(matches!(a.slot.scan, Scan::Idle));
    }

    #[test]
    fn click_selects_and_double_click_toggles() {
        let mut a = scanned();
        let before = a.slot.model.as_ref().unwrap().rows().len();
        a.handle(Input::Click(20.0, row_y(1)));
        let id = a.slot.model.as_ref().unwrap().rows()[1].id;
        assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(id));
        a.handle(Input::DoubleClick(20.0, row_y(1)));
        assert!(a.slot.model.as_ref().unwrap().rows().len() > before);
        a.handle(Input::DoubleClick(20.0, row_y(1)));
        assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before);
    }

    #[test]
    fn clicking_a_folders_icon_toggles_it_and_a_double_click_on_it_toggles_again() {
        let mut a = scanned();
        let before = a.slot.model.as_ref().unwrap().rows().len();
        let depth = a.slot.model.as_ref().unwrap().rows()[1].depth as f32;
        let icon_x = a.layout.column(0).0 + 8.0 + depth * INDENT + 4.0;
        a.handle(Input::Click(icon_x, row_y(1)));
        let opened = a.slot.model.as_ref().unwrap().rows().len();
        assert!(opened > before, "one click on the icon opens the folder");
        a.handle(Input::DoubleClick(icon_x, row_y(1)));
        assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before, "the second press of a pair toggles again");
        a.handle(Input::Click(icon_x + 200.0, row_y(1)));
        assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before, "a click on the name only selects");
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
        assert_eq!(a.unscanned(), None, "the scan counted more than the view says is used");
    }

    #[test]
    fn the_icon_buttons_have_tooltips_and_nothing_else_does() {
        let mut a = app();
        a.can_elevate = true;
        let (admin, regex) = (a.layout.admin_button(), a.layout.filter_regex());
        assert_eq!(tip_str(&a, admin.x + 5.0, admin.y + 5.0).as_deref(), Some("Run as administrator"));
        assert_eq!(tip_str(&a, regex.x + 5.0, regex.y + 5.0).as_deref(), Some("Regular expression"));
        assert_eq!(tip_str(&a, a.layout.chip(0).x + 5.0, 10.0).as_deref(), Some("C: 250 B free of 1000 B"), "a chip gives its free space in full");
        assert_eq!(tip_str(&a, a.layout.header().x + 5.0, a.layout.header().y + 5.0), None);
        a.can_elevate = false;
        assert_eq!(tip_str(&a, admin.x + 5.0, admin.y + 5.0), None, "a hidden button has none");
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
        assert_eq!(a.hot, Some(a.layout.header_cell(0)), "a header cell lights up: it sorts");
        a.handle(Input::Hover(5.0, a.layout.rows_area().y + 5.0));
        assert_eq!(a.hot, None);
    }

    #[test]
    fn a_press_on_the_hot_control_is_pressed_until_the_button_is_released() {
        let mut a = app();
        let chip = a.layout.chip(0);
        let (x, y) = (chip.x + 5.0, chip.y + 5.0);
        a.handle(Input::Hover(x, y));
        a.handle(Input::Click(x, y));
        assert!(a.pressed);
        a.handle(Input::Release);
        assert!(!a.pressed);
        a.handle(Input::Hover(5.0, a.layout.rows_area().y + 5.0));
        a.handle(Input::Click(5.0, a.layout.rows_area().y + 5.0));
        assert!(!a.pressed, "nothing under the pointer to press");
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
        let id = a.slot.model.as_ref().unwrap().rows()[2].id;
        assert_eq!(a.handle(Input::RightClick(20.0, row_y(2))), Effect::Menu(id));
        assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(id));
    }

    #[test]
    fn wheel_and_keys_keep_the_selection_visible_and_scroll_in_range() {
        let mut a = scanned();
        let m = a.slot.model.as_mut().unwrap();
        let t = a.slot.tree.as_ref().unwrap();
        for _ in 0..8 {
            let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
            if let Some(r) = r { m.expand(t, r); }
        }
        let n = a.slot.model.as_ref().unwrap().rows().len();
        let vis = a.layout.rows_visible();
        assert!(n > vis, "need more rows than fit: {n} vs {vis}");
        a.handle(Input::Wheel(10_000, 5.0, 100.0));
        assert_eq!(a.slot.scroll, n - vis);
        a.handle(Input::Wheel(-10_000, 5.0, 100.0));
        assert_eq!(a.slot.scroll, 0);
        a.handle(Input::Key(Key::End));
        assert_eq!(a.slot.scroll, n - vis);
        a.handle(Input::Key(Key::Home));
        assert_eq!(a.slot.scroll, 0);
    }

    #[test]
    fn header_click_sorts_and_second_click_reverses() {
        let mut a = scanned();
        let name = |a: &App, i: usize| a.slot.tree.as_ref().unwrap().name_string(a.slot.model.as_ref().unwrap().rows()[i].id);
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
        let t = a.slot.tree.as_ref().unwrap();
        let m = a.slot.model.as_mut().unwrap();
        for _ in 0..8 {
            let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
            if let Some(r) = r { m.expand(t, r); }
        }
        let total = m.rows().len();
        let th = a.layout.thumb(total, 0).unwrap();
        a.handle(Input::Hover(795.0, th.y + 2.0));
        assert_eq!(a.with_scene(false, |s| (s.hot, s.pressed)), (Some(th), false), "the thumb lights up under the pointer");
        a.handle(Input::Click(795.0, th.y + 2.0));
        assert!(a.with_scene(false, |s| s.pressed), "and is pressed while dragged");
        a.handle(Input::Drag(795.0, th.y + 2.0 + 100.0));
        assert!(a.slot.scroll > 0);
        a.handle(Input::Release);
        assert!(a.with_scene(false, |s| !s.pressed));
        a.handle(Input::Hover(5.0, th.y));
        assert_eq!(a.with_scene(false, |s| s.hot), None, "off the thumb it is not hot");
        let s = a.slot.scroll;
        a.handle(Input::Drag(795.0, th.y + 300.0));
        assert_eq!(a.slot.scroll, s);
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
        a.slot.refreshing = true;
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
        let id = a.slot.model.as_ref().unwrap().rows()[1].id;
        a.slot.model.as_mut().unwrap().select(id);
        let tree = a.slot.tree.as_ref().unwrap();
        let want = format!(
            "{}  ({}, {} items)   |   done",
            fmt::node_path(tree, id),
            fmt::size(tree.subtree_size[id]),
            fmt::count(tree.subtree_files[id] as u64)
        );
        assert_eq!(status_str(&a, true), want);
    }

    #[test]
    fn the_status_line_shows_the_live_marker_and_refreshing() {
        let mut a = scanned();
        assert_eq!(status_tail(&a), "done");
        a.set_live(LiveStatus::On { last: None });
        assert_eq!(status_tail(&a), "done   |   live");
        a.live_changed("12:03:41".into());
        a.slot.refreshing = true;
        assert_eq!(status_tail(&a), "done   |   live, last change 12:03:41   |   refreshing\u{2026}");
        a.set_live(LiveStatus::Stale("x".into()));
        a.live_changed("12:04:00".into());
        assert!(status_str(&a, true).contains("live updates stopped") && !status_str(&a, true).contains("12:04"));
    }

    #[test]
    fn the_status_bar_shows_a_compact_line_and_its_tooltip_the_whole_one_and_the_adapter() {
        let mut a = scanned();
        let id = a.slot.model.as_ref().unwrap().rows()[1].id;
        a.slot.model.as_mut().unwrap().select(id);
        a.set_live(LiveStatus::On { last: None });
        a.live_changed("12:03:41".into());
        let tree = a.slot.tree.as_ref().unwrap();
        let size = format!("{}, {} items", fmt::size(tree.subtree_size[id]), fmt::count(tree.subtree_files[id] as u64));
        assert_eq!(status_str(&a, false), format!("{size}   |   done   |   live"), "no path, no time of the last change");
        assert!(status_str(&a, true).contains("12:03:41") && status_str(&a, true).contains(&fmt::node_path(tree, id).to_string()));
        a.adapter = "Test GPU".into();
        let bar = a.layout.status();
        assert_eq!(tip_str(&a, bar.x + 40.0, bar.y + 5.0), Some(format!("{}
Test GPU", status_str(&a, true))));
    }

    #[test]
    fn a_tip_keeps_its_target_while_its_text_changes_and_loses_it_on_leaving() {
        let mut a = scanned();
        let bar = a.layout.status();
        let (x, y) = (bar.x + 40.0, bar.y + 5.0);
        let (key, before) = target_str(&a, x, y).unwrap();
        a.notice = "something happened".into();
        let (same, after) = target_str(&a, x, y).unwrap();
        assert_eq!(key, same, "the status bar is the same target");
        assert_ne!(before, after, "whose text moved on");
        a.can_elevate = true;
        let other = target_str(&a, a.layout.admin_button().x + 5.0, a.layout.admin_button().y + 5.0).map(|t| t.0);
        assert!(other.is_some() && other != Some(key), "another target is another key");
    }

    #[test]
    fn a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row() {
        let mut a = scanned();
        a.handle(Input::Click(5.0, PICKER_H + 2.0)); // sort by Name ascending
        let t = a.slot.tree.as_ref().unwrap();
        let m = a.slot.model.as_mut().unwrap();
        let first = m.rows()[1].id;
        m.expand(t, first);
        let kid = m.rows()[2].id;
        m.select(kid);
        a.slot.scroll = 0;
        let path = a.slot.tree.as_ref().unwrap().path(kid);
        a.slot.refreshing = true;
        a.swap_tree(kary_builder(300, 3).finish().unwrap(), "again".into());
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_ref().unwrap());
        assert_eq!(t.path(m.selected().unwrap()), path);
        assert_eq!(a.sort(), (0, false));
        assert!(m.is_expanded(m.rows()[1].id));
        assert_eq!(a.slot.summary, "again");
        assert!(!a.slot.refreshing);
    }

    #[test]
    fn refresh_rows_picks_up_tree_changes_and_keeps_the_selection_in_place() {
        let mut a = scanned();
        let sel = a.slot.model.as_ref().unwrap().rows()[3].id;
        a.slot.model.as_mut().unwrap().select(sel);
        let n = a.slot.model.as_ref().unwrap().rows().len();
        let root = a.slot.model.as_ref().unwrap().rows()[0].id;
        a.slot.tree.as_mut().unwrap().add_child(root, &[b'n' as u16, b'w' as u16], 1 << 40, plumb_core::NodeFlags::empty());
        a.refresh_rows();
        let m = a.slot.model.as_ref().unwrap();
        assert_eq!(m.rows().len(), n + 1);
        assert_eq!(m.selected(), Some(sel));
    }

    #[test]
    fn resize_reclamps_scroll() {
        let mut a = scanned();
        a.slot.scroll = 5;
        a.handle(Input::Resize(800.0, 2000.0));
        assert_eq!(a.slot.scroll, 0);
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
        let sel = a.slot.model.as_ref().unwrap().selected();
        assert_eq!(a.map_root, treemap::root_for(a.slot.tree.as_ref().unwrap(), sel), "a click in the tree re-roots the map");
        let (i, t) = a.map_tiles().iter().copied().enumerate().find(|(_, t)| t.kind == Kind::File).unwrap();
        let before = a.map_root;
        let (x, y) = centre(&t);
        a.handle(Input::Click(x, y));
        a.update_map(1.0);
        assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
        assert_eq!(a.map_root, before, "a click on the map keeps the root");
        assert_eq!(a.map_selected_tile(), Some(i));
        assert!(a.slot.model.as_ref().unwrap().selected_index().is_some(), "its row is revealed in the tree");
    }

    #[test]
    fn double_clicking_a_folder_tile_roots_the_map_there() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::Frame).unwrap();
        let (x, y) = (t.rect.x + 0.5, t.rect.y + 0.5); // the frame's border, clear of its children
        a.handle(Input::Click(x, y));
        a.handle(Input::DoubleClick(x, y));
        assert_eq!(a.map_root, Some(t.node));
        assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
        a.update_map(1.0);
        assert!(a.map_tiles().iter().all(|k| k.depth > 0 || a.slot.tree.as_ref().unwrap().parent[k.node] == t.node || k.kind == Kind::Rest));
    }

    #[test]
    fn a_double_click_on_a_file_tile_only_selects_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        let before = a.map_root;
        a.handle(Input::DoubleClick(x, y));
        assert_eq!((a.map_root, a.slot.model.as_ref().unwrap().selected()), (before, Some(t.node)));
    }

    #[test]
    fn hovering_builds_every_kind_of_tip_without_allocating_once_the_buffer_has_grown() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (bar, admin) = (a.layout.status(), a.layout.admin_button());
        a.can_elevate = true;
        let mut buf = String::new();
        let spots = [centre(&t), (bar.x + 40.0, bar.y + 5.0), (admin.x + 5.0, admin.y + 5.0), (a.layout.chip(0).x + 5.0, 10.0)];
        for (x, y) in spots {
            assert!(a.tip_at(x, y, &mut buf), "a tip at {x},{y}"); // grows the buffer
            let want = buf.clone();
            let (found, n) = crate::alloc_count::allocations(|| a.tip_at(x, y, &mut buf));
            assert!(found && buf == want, "{buf}");
            assert_eq!(n, 0, "allocations for the tip {buf:?}");
        }
    }

    #[test]
    fn hovering_a_tile_gives_a_tip_and_leaving_the_map_clears_it() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        a.handle(Input::Hover(x, y));
        assert!(a.map_hover.is_some());
        let tip = tip_str(&a, x, y).unwrap();
        assert!(tip.contains(&a.slot.tree.as_ref().unwrap().name_string(t.node)), "{tip}");
        a.handle(Input::Hover(5.0, 5.0));
        assert_eq!((a.map_hover, tip_str(&a, 5.0, 5.0)), (None, None));
    }

    #[test]
    fn a_rest_tile_says_how_many_items_it_holds() {
        let tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Rest, bytes: 2048, count: 7, mtime: None };
        assert!(tile_str(&tree, &t, 150_000).starts_with("7 smaller items, "));
        let f = treemap::Tile { node: NodeId(1), kind: Kind::File, count: 1, ..t };
        assert!(tile_str(&tree, &f, 150_000).contains(&tree.name_string(NodeId(1))));
    }

    #[test]
    fn an_own_tile_tip_names_the_folder_itself() {
        let tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Own, bytes: 2048, count: 1, mtime: None };
        let tip = tile_str(&tree, &t, 150_000);
        assert!(tip.contains(&tree.name_string(NodeId(0))) && tip.contains("itself"), "{tip}");
    }

    #[test]
    fn a_parent_processs_own_memory_has_a_tile_that_selects_it_but_is_not_outlined() {
        use plumb_core::procs::{Metric, Procs, Sample};
        let mut a = app();
        a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
        a.begin_scan(2);
        let (mut procs, mut tree) = Procs::new(Metric::Memory);
        let s = |pid, parent, name: &str, memory| Sample { pid, parent, created: 5, name: name.into(), memory, private: memory / 2, cpu_100ns: 0, io_bytes: 0 };
        procs.apply(&mut tree, &[s(77, 1, "parent.exe", 400_000), s(78, 77, "child.exe", 100_000)], 0);
        let parent = tree.find("Processes\\parent.exe").expect("the parent is under the root");
        a.finish_scan(tree, "x".into());
        a.layout.map_on = true;
        a.update_map(1.0);
        let i = a.map_tiles().iter().position(|t| t.kind == Kind::Own && t.node == parent).expect("an own tile for the parent");
        assert_eq!(a.map_tiles()[i].bytes, 400_000);
        assert_eq!(a.select_from_map(i), Some(parent), "a click on the own tile selects the process");
        let sel = a.map_selected_tile().expect("the process is outlined");
        assert_eq!(a.map_tiles()[sel].kind, Kind::Frame, "by its frame, not by the own tile");
        let tip = proc_tip_str(&a, a.slot.tree.as_ref().unwrap(), &a.map_tiles()[i]);
        assert!(tip.contains("parent.exe") && tip.contains("itself"), "{tip}");
    }

    #[test]
    fn a_tile_tip_says_when_the_file_was_last_written() {
        let mut tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(1), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::File, bytes: 2048, count: 1, mtime: None };
        assert!(!tile_str(&tree, &t, 154_000).contains("modified"));
        tree.set_mtime(NodeId(1), std::num::NonZeroU32::new(plumb_core::age::UNIX_EPOCH_DAYS + 19_791));
        let tip = tile_str(&tree, &t, plumb_core::age::UNIX_EPOCH_DAYS + 19_791 + 766);
        assert!(tip.ends_with(", modified 2024-03-09, 2.1 y ago)"), "{tip}");
    }

    #[test]
    fn right_clicking_a_tile_selects_it_and_asks_for_the_menu() {
        let mut a = with_map();
        let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
        let (x, y) = centre(&t);
        assert_eq!(a.handle(Input::RightClick(x, y)), Effect::Menu(t.node));
        assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
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
        let t = a.slot.tree.as_ref().unwrap();
        let all = || (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none());
        let file = all().find(|&i| !t.has_children(i)).unwrap();
        let dir = all().find(|&i| t.has_children(i)).unwrap();
        (file, dir, t.children(dir).next().unwrap())
    }

    fn select(a: &mut App, id: NodeId) {
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
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
        let t = a.slot.tree.as_ref().unwrap();
        assert_eq!(a.basket_label(), Some(format!("1 marked, up to {}", fmt::size(t.subtree_size[dir]))));
        let (dir_path, dir_size) = (t.path(dir), t.subtree_size[dir]);
        let req = a.commit_request().unwrap();
        assert_eq!(req.paths, vec![dir_path], "the commit lists the folder, not its child");
        assert_eq!(req.bytes, dir_size);
    }

    #[test]
    fn roots_and_scans_are_refused_with_a_reason_that_reaches_the_status_line() {
        let mut a = scanned();
        let root = a.slot.tree.as_ref().unwrap().roots[0];
        assert_eq!(a.mark_state(root), MarkState::Refused("a drive root cannot be deleted"));
        a.toggle_mark(root);
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("cannot mark: a drive root cannot be deleted"), "{}", status_str(&a, true));
        a.handle(Input::Click(5.0, a.layout.header().y + 5.0)); // any click clears the notice
        assert!(!status_str(&a, true).starts_with("cannot mark"));
        let (file, ..) = picks(&a);
        a.slot.refreshing = true;
        assert_eq!(a.mark_state(file), MarkState::Refused("the drive is being scanned"));
    }

    #[test]
    fn marks_survive_a_drive_switch_a_rescan_keeps_them_and_a_first_scan_clears_them() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        let mut other = Slot::default();
        a.swap_slot(&mut other);
        assert!(a.basket().is_empty(), "the other view has no marks");
        a.swap_slot(&mut other);
        assert_eq!(a.basket().len(), 1, "coming back finds them");
        let t = plumb_core::synth::kary_builder(300, 3).finish().unwrap();
        a.swap_tree(t, "rescanned".into());
        a.update_map(1.0);
        assert_eq!(a.basket().len(), 1, "a background rescan leaves them");
        assert!(a.basket_label().is_some());
        a.begin_scan(0);
        assert_eq!(a.basket().len(), 1, "rescanning the view on screen keeps its marks");
        a.begin_scan(1);
        assert!(a.basket().is_empty(), "a first scan of another view starts clean");
    }

    #[test]
    fn a_mark_that_can_no_longer_be_deleted_is_dropped_and_reported_with_the_result() {
        let mut a = scanned();
        let (bad, good) = {
            let t = a.slot.tree.as_ref().unwrap();
            let mut leaves = (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none() && !t.has_children(i));
            (leaves.next().unwrap(), leaves.next().unwrap())
        };
        a.toggle_mark(bad);
        a.toggle_mark(good);
        let bad_path = a.slot.tree.as_ref().unwrap().path(bad);
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
        a.system_root = a.slot.tree.as_ref().unwrap().path(bad);
        assert!(a.commit_request().is_none());
        assert!(a.notice.starts_with("1 marked item skipped"), "{}", a.notice);
        assert!(a.basket().is_empty());
    }

    #[test]
    fn the_marks_of_a_drive_whose_state_was_let_go_come_back_with_its_next_scan() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        let mut gone = Slot::default();
        a.swap_slot(&mut gone); // the view goes away with its mark
        a.stash_basket(2, &mut gone); // and is let go
        a.begin_scan(3);
        assert!(a.basket().is_empty(), "another view does not get them");
        a.begin_scan(2);
        assert_eq!(a.basket().len(), 1, "its own scan does");
    }

    #[test]
    fn a_marked_path_that_is_gone_at_commit_is_dropped_not_deleted() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        assert!(a.slot.tree.as_mut().unwrap().remove(file));
        assert!(a.commit_request().is_none());
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("the marked items are already gone"), "{}", status_str(&a, true));
    }

    #[test]
    fn a_commit_refuses_a_busy_drive_and_a_second_delete() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        a.slot.refreshing = true;
        assert!(a.commit_request().is_none());
        a.slot.refreshing = false;
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
        let t = a.slot.tree.as_ref().unwrap();
        let (gone, kept) = (t.path(file), t.path(dir));
        a.set_deleting(true);
        a.finish_delete(std::slice::from_ref(&gone), 0, &[(kept.clone(), "access denied".into())]);
        assert!(a.basket().contains(&kept) && !a.basket().contains(&gone));
        assert!(status_str(&a, true).starts_with("1 of 2 not deleted: access denied; still marked"), "{}", status_str(&a, true));
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
        assert_eq!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0)), Effect::None);
        assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Delete(_))), "the chip asks how to delete");
    }

    /// A modal over a scanned app with one file marked, and the middle of one of its buttons.
    fn asking() -> (App, impl Fn(&App, Pick) -> (f32, f32)) {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        a.ask_delete();
        let centre = |a: &App, p| {
            let r = a.modal_button(p).expect("the button is there");
            (r.x + r.w / 2.0, r.y + r.h / 2.0)
        };
        (a, centre)
    }

    #[test]
    fn answering_a_modal_keeps_the_status_notice() {
        let (mut a, centre) = asking();
        a.notice = "1 of 1 not deleted: in use; still marked".into();
        let (x, y) = centre(&a, Pick::Cancel);
        a.handle(Input::Click(x, y));
        assert!(a.modal().is_none() && a.notice.starts_with("1 of 1 not deleted"), "{:?}", a.notice);
    }

    #[test]
    fn a_modal_takes_all_input_and_enter_escape_tab_and_clicks_answer_it() {
        let (mut a, centre) = asking();
        let (scroll, marks) = (a.slot.scroll, a.slot.basket.len());
        // Everything but its own buttons is swallowed, including the keys that would unmark or scroll the list.
        for i in [Input::Wheel(3, 5.0, 100.0), Input::Edit(Edit::Delete), Input::Key(Key::Down), Input::Find] {
            assert_eq!(a.handle(i), Effect::None);
        }
        assert!(a.modal().is_some() && a.slot.scroll == scroll && a.slot.basket.len() == marks);
        // A click on the scrim cancels (the marks stay); one inside the panel, off its buttons, does nothing.
        let p = a.modal().map(|m| m.geometry(a.layout.w, a.layout.h).panel).unwrap();
        a.handle(Input::Click(p.x + 2.0, p.y + 2.0));
        assert!(a.modal().is_some());
        assert_eq!(a.handle(Input::Click(5.0, 5.0)), Effect::None);
        assert!(a.modal().is_none() && a.slot.basket.len() == marks);
        a.ask_delete();
        // ... but not the "Close anyway?" question.
        a.handle(Input::Escape);
        a.ask_close();
        a.handle(Input::Click(5.0, 5.0));
        assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Close)));
        a.handle(Input::Escape);
        a.ask_delete();
        // Escape cancels; the marks stay.
        assert_eq!(a.handle(Input::Escape), Effect::None);
        assert!(a.modal().is_none() && a.slot.basket.len() == marks);
        // Enter presses the default: Recycle.
        a.ask_delete();
        let Effect::Delete { permanent, paths, .. } = a.handle(Input::Enter) else { panic!("Enter on the default button") };
        assert!(!permanent && paths.len() == 1 && a.modal().is_none());
        // Tab moves the focus; Enter then presses that button (Delete permanently asks a second question, defaulting to Cancel).
        a.ask_delete();
        a.handle(Input::Tab);
        assert_eq!(a.handle(Input::Enter), Effect::None);
        assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Permanent(_))));
        assert_eq!(a.handle(Input::Enter), Effect::None, "the default of the second question is Cancel");
        assert!(a.modal().is_none() && a.slot.basket.len() == marks);
        // A click on a button answers it: permanently, then Yes.
        a.ask_delete();
        let at = centre(&a, Pick::Permanent);
        a.handle(Input::Click(at.0, at.1));
        let at = centre(&a, Pick::Yes);
        assert!(matches!(a.handle(Input::Click(at.0, at.1)), Effect::Delete { permanent: true, .. }));
        // Unmark all clears the basket.
        a.ask_delete();
        let at = centre(&a, Pick::Unmark);
        a.handle(Input::Click(at.0, at.1));
        assert!(a.slot.basket.is_empty() && a.modal().is_none());
    }

    #[test]
    fn a_hovered_modal_button_is_hot_and_modals_queue_behind_each_other() {
        let (mut a, centre) = asking();
        let at = centre(&a, Pick::Cancel);
        a.handle(Input::Hover(at.0, at.1));
        assert_eq!(a.with_scene(false, |s| s.hot), a.modal_button(Pick::Cancel));
        a.handle(Input::Hover(1.0, 1.0));
        assert_eq!(a.with_scene(false, |s| s.hot), None);
        // A failure and a close question arriving over the delete question wait their turn, the close question once.
        a.ask(Modal::failed(0, false, &[("C:\\f".into(), "in use".into())], 1));
        a.ask_close();
        a.ask_close();
        assert_eq!(a.modals.len(), 3);
        a.handle(Input::Escape);
        assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Failed { .. })));
        let Effect::Delete { paths, .. } = a.handle(Input::Enter) else { panic!("Retry") };
        assert_eq!(paths, ["C:\\f"]);
        assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Close)));
        assert_eq!(a.handle(Input::Escape), Effect::None, "Esc keeps the window open");
        a.ask_close();
        a.handle(Input::Key(Key::Left));
        assert_eq!(a.handle(Input::Enter), Effect::Quit);
    }

    #[test]
    fn a_deleted_folder_takes_its_childs_mark_and_a_stale_mark_is_pruned_at_commit() {
        let mut a = scanned();
        let (file, dir, child) = picks(&a);
        let t = a.slot.tree.as_ref().unwrap();
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
        assert!(b.slot.tree.as_mut().unwrap().remove(fid));
        let req = b.commit_request().unwrap();
        assert_eq!(req.paths.len(), 1);
        assert_eq!(b.basket().len(), 1, "the mark that no longer resolves is dropped, not kept to come back later");
    }

    #[test]
    fn a_mark_from_the_menu_is_found_again_by_path_and_a_vanished_path_marks_nothing() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        let p = a.slot.tree.as_ref().unwrap().path(file);
        a.toggle_mark_path(&p);
        assert!(a.basket().contains(&p));
        a.toggle_mark_path("nowhere\\at all");
        assert_eq!(a.basket().len(), 1);
        assert!(status_str(&a, true).starts_with("that item is gone"));
    }

    #[test]
    fn del_with_a_tree_but_nothing_selected_marks_nothing() {
        let mut a = scanned();
        a.slot.model = Some(RowModel::new(a.slot.tree.as_ref().unwrap()));
        assert!(a.slot.model.as_ref().unwrap().selected().is_none());
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
        a.slot.tree.as_mut().unwrap().rename(file, &n);
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
        let short = status_str(&a, false);
        assert!(short.starts_with("2 of 4 not deleted: in use; still marked"), "{short}");
        assert!(status_str(&a, true).contains("X-d: denied"), "{}", status_str(&a, true));
        let mut b = scanned();
        b.finish_delete(&gone, 1, &[]);
        assert!(status_str(&b, false).starts_with("deleted 1 item, 1 already gone"), "{}", status_str(&b, false));
    }

    #[test]
    fn begin_delete_refuses_a_scanning_drive_and_a_second_delete() {
        let mut a = scanned();
        assert!(a.begin_delete());
        assert!(a.deleting());
        assert!(!a.begin_delete(), "one at a time");
        a.set_deleting(false);
        a.slot.refreshing = true;
        assert!(!a.begin_delete());
        assert!(status_str(&a, true).starts_with("cannot delete while the drive is being scanned"));
    }
}
