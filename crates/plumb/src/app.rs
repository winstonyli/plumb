//! Window-independent application state: what is selected, scrolled and scanning, and how
//! input changes it. No Win32 types, so the whole interaction model tests anywhere.
mod flash;
pub use flash::Flash;
mod marks;
mod filter;
mod held;
mod map;
mod select;
mod toast;

use crate::attrs::{self, Attrs};
use crate::describe::{self, Description};
use crate::draw::{Chip, MapView, Mark, Scene, Theme, View};
use crate::filter_ui::Edit;
use crate::fmt;
use crate::layout::{chip_width, Hit, Layout, Rect, CHIP_W, COL_SCORE, COL_W, INDENT, MAP_MIN_W, MIN_COL_W, PICKER_H, ROW_H};
use crate::modal::{Ask, Modal, Pick, Victim};
use crate::outcome::Outcome;
use toast::Toast;
use crate::restore::Snapshot;
use crate::state::{MapState, SortPref, State, ViewPrefs};
use crate::view_spec::{metric_help, metric_label, metric_noun, ColId};
use plumb_core::basket::{self, Basket};
use plumb_core::rows::{Key, RowModel, SortKey};
use plumb_core::procs::Metric;
use plumb_scan::handles::{HandleError, HandleList};
use plumb_core::treemap::{self, Class, Kind, Tile};
use plumb_core::{NodeFlags, NodeId, Tree};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use std::fmt::Write as _;

use marks::BasketView;
use held::{Held, HELD_REFRESH_SNAPSHOTS};
pub use marks::{CommitRequest, MarkState};
pub use filter::FilterJob;

/// What a chip opens. The row table is the same for every kind (Name plus five columns); the kind supplies the
/// titles, what each column sorts by, and the text of the last three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewKind {
    /// A drive or mount point: the file tree.
    #[default]
    Disk,
    /// The running processes: a tree by parent, sized by memory.
    Processes,
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
    /// Every thread is suspended.
    pub suspended: bool,
    /// Runs as administrator.
    pub elevated: bool,
}

/// What a row's context menu offers (`App::row_menu`).
pub struct RowMenu {
    /// The path in the tree's terms, to mark by; empty for a set (marking goes through `App::mark_selected`).
    pub raw: String,
    /// The file system paths to reveal and copy, the ones that are known: one for a row, those of every picked row for a
    /// set.
    pub paths: Vec<String>,
    /// How many rows the menu acts on: more than one only for a picked set.
    pub count: usize,
    pub mark: MarkState,
    /// Whether to offer "Who is using this".
    pub who: bool,
    /// The processes End process (the first) and End process tree (all) act on.
    pub end: Option<Vec<Victim>>,
    /// The process Suspend or Resume acts on, and whether it is suspended now.
    pub pause: Option<(Victim, bool)>,
    /// What Suspend process tree acts on (the tree's processes not suspended) and Resume process tree (those suspended);
    /// empty when the entry is not offered.
    pub suspend_tree: Vec<Victim>,
    pub resume_tree: Vec<Victim>,
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
    /// A first scan, its partial tree on screen; `paused` once the preview stopped following the scan.
    Running { view: usize, dirs: u64, files: u64, bytes: u64, paused: bool },
    /// A background rescan: the old tree stays on screen until the new one is ready.
    Refreshing,
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

/// The modifier keys held when a click or a key arrived, read by the window at that moment: a flag kept in `App` would
/// have to survive key-up, focus loss and Alt-Tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
}

pub enum Input {
    Click(f32, f32, Mods),
    DoubleClick(f32, f32),
    RightClick(f32, f32),
    /// Rows to scroll (positive is down) with the pointer at this x and y: over the Files held pane it scrolls that.
    Wheel(i32, f32, f32),
    Key(Key, Mods),
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
    /// F5: scan the shown view again.
    Refresh,
    /// Shift+F10 or the Apps key: the context menu of the focus row.
    ContextMenu,
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
    /// Put this text on the clipboard.
    Copy(String),
    /// End this process (a modal was answered Yes); `name` is for the failure message.
    End(Vec<Victim>),
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

fn tile_tip(tree: &Tree, t: &Tile, now: u32, attrs: Attrs, out: &mut String) {
    let _ = match t.kind {
        Kind::Rest => write!(out, "{}, {}", fmt::noun(u64::from(t.count), "smaller item", "smaller items"), fmt::size(t.bytes)),
        Kind::Own => write!(out, "{} itself  ({})", fmt::node_path(tree, t.node), fmt::size(t.bytes)),
        _ => {
            describe::file(tree, t.node, t.bytes, now, attrs).write_tip(false, out);
            Ok(())
        }
    };
}

/// What the app holds for one view: `App::slot` for the one on screen, a parked copy for the
/// rest. The filter box text and the sort stay with the app; everything derived from this view's
/// tree travels with it.
#[derive(Default)]
pub struct Slot {
    pub tree: Option<Tree>,
    pub model: Option<RowModel>,
    pub scroll: usize,
    pub scan: Scan,
    /// One line about the last finished scan (files, time, skipped records); empty before any.
    pub summary: String,
    pub live: LiveStatus,
    filter: filter::Filter,
    pub basket: Basket,
    /// Rows that live updates just changed, tinted while they fade.
    pub flash: Flash,
    /// The Processes view's extra facts about each process, by pid.
    pub proc_info: HashMap<u32, ProcInfo>,
    /// What the Processes view's map and Memory column weigh by.
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

    /// A background rescan is running while the old tree stays on screen.
    pub fn refreshing(&self) -> bool {
        matches!(self.scan, Scan::Refreshing)
    }

    pub fn scan(&self) -> &Scan {
        &self.scan
    }
}

/// One view kind's sort (a position in its columns) and the widths of its five resizable columns.
#[derive(Clone, Copy)]
struct Prefs {
    sort: (usize, bool),
    col_w: [f32; 5],
}

impl Default for Prefs {
    fn default() -> Self {
        Self { sort: (COL_SCORE, true), col_w: COL_W }
    }
}

/// What the left button holds since it went down: one value, so two drags cannot be live.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pointer {
    Up,
    /// Down on a `hot` control (an icon button or a header cell).
    Button,
    /// Dragging the scrollbar thumb; `grab` is the distance from the thumb's top to the grab point.
    Thumb { grab: f32 },
    /// Dragging a column's left edge: the column, and the pointer x and column width at the start.
    Column { col: usize, x0: f32, w0: f32 },
    /// Dragging the map divider: the pointer x and the pane width at the start.
    MapDivider { x0: f32, w0: f32 },
    /// Down on a row: moving the pointer extends the selection (`drag_select`).
    Rows,
    /// Down in the filter box: moving the pointer extends its text selection (the window places the caret).
    Filter,
}

pub struct App {
    pub layout: Layout,
    pub views: Vec<ViewInfo>,
    active: Option<usize>,
    /// What each view off screen holds, shown on its chip; set by the window.
    pub marks: Vec<Mark>,
    /// The visible row under the mouse.
    pub hover: Option<usize>,
    /// The icon button under the mouse.
    pub hot: Option<Rect>,
    /// What the left button holds since it went down (`Up` while it is up).
    pointer: Pointer,
    /// What swaps when another view is put on screen.
    pub slot: Slot,
    /// Show the "Run as administrator" button (the process is not elevated).
    pub can_elevate: bool,
    /// The system's accent colour (`0xRRGGBB`), set by the window; `None` keeps plumb's blue.
    pub accent: Option<u32>,
    pub adapter: String,
    /// Each view kind's sort and widths, indexed by `ViewKind`: the truth `layout.col_w` is loaded from (`set_active`).
    prefs: [Prefs; 2],
    /// The pointer is over the scrollbar thumb.
    pub thumb_hover: bool,
    /// The modals waiting to be answered, the first on screen; while any is up it takes all the input.
    modals: VecDeque<Modal>,
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
    /// Now in unix seconds, taken when the tiles were last rebuilt: tiles are shaded by age against it.
    map_now: u32,
    /// Bytes per class among the tiles, largest first: the legend.
    map_classes: Vec<(treemap::Class, u64)>,
    /// The tile under the pointer.
    pub map_hover: Option<usize>,
    /// The files the selected process holds (see `held_sync`).
    held: Held,
    /// Bumped when the marks change or the shown view's basket is swapped.
    basket_rev: u64,
    basket_view: BasketView,
    /// A delete is running on the worker thread.
    deleting: bool,
    /// A one-off message (a refused mark, what an action did), shown over the rows for a while.
    pub(crate) toast: Toast,
    /// The pointer is on the toast, which holds its time.
    toast_hot: bool,
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
            layout, views, adapter, active: None, marks: Vec::new(), hover: None, hot: None, pointer: Pointer::Up, thumb_hover: false, modals: VecDeque::new(), slot: Slot::default(),
            can_elevate: false, accent: None, prefs: [Prefs::default(); 2],
            map_root: None, map_seen: None, map_tiles: Vec::new(), map_key: None, map_dirty: true, map_gen: 0, map_now: 0, map_classes: Vec::new(), map_hover: None,
            held: Held::default(), basket_rev: 0, basket_view: BasketView::default(), deleting: false, toast: Toast::default(), toast_hot: false, stash: HashMap::new(), skipped: Vec::new(),
            system_root: std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        }
    }

    /// The left button is down on a control, or on the scrollbar thumb (which draws pressed while it is dragged).
    pub fn pressed(&self) -> bool {
        matches!(self.pointer, Pointer::Button | Pointer::Thumb { .. })
    }

    /// The left button went down in the filter box and is still down: the window extends its selection as the pointer moves.
    pub fn filter_dragging(&self) -> bool {
        self.pointer == Pointer::Filter
    }

    /// Sizes each chip to its label: `measure` gives a text's width in DIPs.
    pub fn fit_chips(&mut self, measure: &dyn Fn(&str) -> f32) {
        self.layout.chips = self.chips().iter().map(|c| chip_width(measure(&c.label))).collect();
    }

    /// The kind of the view on screen (a disk when none is).
    pub fn kind(&self) -> ViewKind {
        self.active.and_then(|i| self.views.get(i)).map_or(ViewKind::Disk, |d| d.kind)
    }

    /// The view on screen, if any.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// The one writer of `active`: the kind decides which sort and widths apply, so `layout.col_w` follows it.
    pub fn set_active(&mut self, a: Option<usize>) {
        self.active = a;
        self.layout.set_widths(self.prefs[self.kind() as usize].col_w);
    }

    /// The header column the rows are sorted by (for the view on screen), and whether descending.
    pub fn sort(&self) -> (usize, bool) {
        self.prefs[self.kind() as usize].sort
    }

    /// Takes the remembered column widths and sort (before any scan, so the first tree is sorted by it).
    pub fn restore(&mut self, s: &State) {
        for (kind, saved) in [(ViewKind::Disk, &s.disk), (ViewKind::Processes, &s.processes)] {
            let cols = &kind.spec().cols;
            let pos = |name: &str| ColId::parse(name).and_then(|id| cols.iter().position(|c| c.id == id));
            let p = &mut self.prefs[kind as usize];
            if let Some(i) = saved.sort.as_ref().and_then(|s| pos(&s.col)) {
                p.sort = (i, saved.sort.as_ref().is_some_and(|s| s.descending));
            }
            for (name, &w) in &saved.widths {
                if let Some(i) = pos(name).filter(|&i| i > 0 && w.is_finite()) {
                    p.col_w[i - 1] = w.clamp(MIN_COL_W, 4000.0);
                }
            }
        }
        self.set_active(self.active);
        self.layout.map_on = s.map.visible;
        if s.map.width.is_finite() {
            self.layout.map_w = s.map.width.clamp(MAP_MIN_W, 4000.0);
        }
    }

    /// The part of the remembered state the app owns; the window fills in its placement.
    pub fn remembered(&self) -> State {
        let saved = |kind: ViewKind| {
            let (cols, p) = (&kind.spec().cols, &self.prefs[kind as usize]);
            ViewPrefs {
                widths: (1..cols.len()).map(|i| (cols[i].id.name().to_string(), p.col_w[i - 1])).collect(),
                sort: Some(SortPref { col: cols[p.sort.0].id.name().to_string(), descending: p.sort.1 }),
            }
        };
        State { window: None, disk: saved(ViewKind::Disk), processes: saved(ViewKind::Processes), map: MapState { visible: self.layout.map_on, width: self.layout.map_w } }
    }

    /// Exchanges the per-view state with `p`. The result is consistent on its own (banner and
    /// Cancel button follow the new state) but leaves the filter alone: see `switch_slot`.
    pub fn swap_slot(&mut self, p: &mut Slot) {
        use std::mem::swap;
        swap(&mut self.slot, p);
        swap(&mut self.slot.filter.ui, &mut p.filter.ui); // the query text belongs to the window
        self.basket_rev += 1;
        self.layout = self.layout_now();
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
        self.set_active(Some(view));
        self.slot.tree = None;
        self.slot.model = None;
        self.slot.scroll = 0;
        self.slot.flash.clear();
        self.slot.summary.clear();
        self.set_live(LiveStatus::Off);
        self.slot.scan = Scan::Running { view, dirs: 0, files: 0, bytes: 0, paused: false };
    }

    pub fn progress(&mut self, d: u64, f: u64, b: u64) {
        if let Scan::Running { dirs, files, bytes, .. } = &mut self.slot.scan {
            (*dirs, *files, *bytes) = (d, f, b);
        }
    }

    /// A model for `tree` in the given sort, with the root expanded and selected so there is something to see.
    fn fresh_model(tree: &Tree, key: SortKey, desc: bool) -> RowModel {
        let mut model = RowModel::new(tree);
        model.set_sort(tree, key, desc);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(tree, r);
            model.select(r);
        }
        model
    }

    /// After an install that ends a scan or a rescan: nothing is running, live is off.
    fn settle(&mut self, summary: String) {
        self.slot.flash.clear();
        self.slot.summary = summary;
        self.slot.scan = Scan::Idle;
        self.set_live(LiveStatus::Off);
    }

    /// Installs a finished tree. The root is expanded and selected so there is something to see.
    pub fn finish_scan(&mut self, tree: Tree, summary: String) {
        self.filter_reset();
        self.map_reset();
        let model = Self::fresh_model(&tree, self.kind().spec().cols[self.sort().0].sort, self.sort().1);
        self.slot.tree = Some(tree);
        self.slot.model = Some(model);
        self.slot.scroll = 0;
        self.settle(summary);
    }

    /// The preview applier just created `self.slot.tree` or dropped it (a scanner fell back to
    /// another): build the rows for the tree, or show nothing. The scan state is untouched, so
    /// the progress line keeps counting.
    pub fn preview_installed(&mut self) {
        self.filter_reset();
        self.map_reset();
        self.slot.scroll = 0;
        self.slot.model = None;
        self.slot.flash.clear();
        let (key, desc) = (self.kind().spec().cols[self.sort().0].sort, self.sort().1);
        let Some(tree) = &mut self.slot.tree else { return };
        tree.take_touched(); // the model is built from the tree as it is now
        self.slot.model = Some(Self::fresh_model(tree, key, desc));
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
        let (model, scroll) = snap.apply(&tree, self.kind().spec().cols[self.sort().0].sort, self.sort().1, self.layout.rows_visible());
        self.slot.tree = Some(tree);
        self.slot.model = Some(model);
        self.slot.scroll = scroll;
        self.filter_reset();
        self.settle(summary);
    }

    /// Tints the rows the tree has changed since the last refresh (call before `refresh_rows`, which consumes them).
    pub fn flash_changes(&mut self, now: Instant) {
        if let Some(t) = &self.slot.tree {
            self.slot.flash.mark(t.touched(), now);
        }
    }

    /// Whether any row is still tinted, so the fade timer must run.
    pub fn flashing(&self) -> bool {
        self.slot.flash.active()
    }

    /// Forgets the tints that have faded out; true while any remain.
    pub fn flash_tick(&mut self, now: Instant) -> bool {
        self.slot.flash.tick(now)
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

    /// The layout with every flag taken from the app's state (`toast` is kept: `dismiss_toast` owns it).
    fn layout_now(&self) -> Layout {
        Layout { cancel: self.cancelable(), basket: self.basket_active(), procs: self.kind() == ViewKind::Processes, banner: matches!(self.slot.live, LiveStatus::Stale(_)), ..self.layout.clone() }
    }

    pub fn set_live(&mut self, live: LiveStatus) {
        self.slot.live = live;
        self.layout = self.layout_now();
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
        matches!(self.slot.scan, Scan::Running { .. } | Scan::Refreshing)
    }

    /// The user cancelled the scan in flight. A first scan leaves no tree; a background rescan
    /// leaves the old one on screen.
    pub fn cancel_scan(&mut self) {
        match self.slot.scan {
            Scan::Running { .. } => {
                self.slot.tree = None;
                self.slot.model = None;
                self.slot.scan = Scan::Cancelled;
                self.map_reset();
            }
            Scan::Refreshing => self.slot.scan = Scan::Idle,
            _ => {}
        }
    }

    pub fn fail_scan(&mut self, msg: String) {
        self.slot.tree = None;
        self.slot.model = None;
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
            && (matches!(self.pointer, Pointer::Column { .. } | Pointer::MapDivider { .. })
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
            self.write_status(true, true, out);
            let _ = write!(out, "\n{}", self.adapter);
            return Some(TipKey::Area(bar));
        }
        if let Hit::Header(crate::layout::COL_SCORE) = self.layout.hit(x, y, self.views.len()) {
            out.push_str(self.kind().spec().score_help);
            return Some(TipKey::Area(self.layout.header_cell(crate::layout::COL_SCORE)));
        }
        if let Hit::Row(i) = self.layout.hit(x, y, self.views.len()) {
            let (nx, nw) = self.layout.column(0);
            let row = self.row_at(i).filter(|&id| x >= nx && x < nx + nw && !self.row_attrs(id).is_empty());
            if let (Some(id), Some(tree)) = (row, self.slot.tree.as_ref()) {
                self.describe_sized(tree, id, tree.subtree_size[id]).write_tip(true, out);
                return Some(TipKey::Area(Rect { x: nx, w: nw, ..self.layout.row(i) }));
            }
        }
        let t = self.map_tiles.get(treemap::hit(&self.map_tiles, x, y).filter(|_| matches!(self.layout.hit(x, y, self.views.len()), Hit::Map))?)?;
        let tree = self.slot.tree.as_ref()?;
        if self.kind() == ViewKind::Processes { self.process_tip(tree, t, out) } else { tile_tip(tree, t, self.map_now, self.row_attrs(t.node), out) }
        Some(TipKey::Tile(t.node, t.kind))
    }

    /// A map tile of the Processes view: name and pid, the numbers, the executable and when it started.
    fn process_tip(&self, tree: &Tree, t: &Tile, out: &mut String) {
        let (metric, size) = (metric_noun(self.slot.proc_metric), (self.kind().spec().size_text)(self.slot.proc_metric, t.bytes));
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
        if t.kind == Kind::Own {
            let pid = tree.aux.get(t.node.idx()).map_or(0, |a| a[2]);
            let _ = write!(out, "{}  (PID {pid})\n{size} {metric} in the process itself", fmt::node_name(tree, t.node));
            return;
        }
        self.describe_sized(tree, t.node, t.bytes).write_tip(false, out);
    }

    /// What the UI says about `id` (`crate::describe`), its size counted as `bytes`.
    fn describe_sized<'a>(&'a self, tree: &'a Tree, id: NodeId, bytes: u64) -> Description<'a> {
        let attrs = self.row_attrs(id);
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2]) as u32;
            describe::process(tree, id, (self.kind().spec().size_text)(self.slot.proc_metric, bytes), metric_noun(self.slot.proc_metric), self.slot.proc_info.get(&pid), plumb_core::age::now_secs(), attrs)
        } else {
            describe::file(tree, id, bytes, self.map_now, attrs)
        }
    }

    /// The Processes status line: how many processes, and what the Size column counts. The total is the root row's.
    fn write_process_status(&self, out: &mut String) -> bool {
        if self.slot.tree.as_ref().is_none_or(|t| t.roots.is_empty()) {
            return false;
        }
        let _ = write!(out, "{}, sized by {}", fmt::noun(self.slot.proc_info.len() as u64, "process", "processes"), metric_noun(self.slot.proc_metric));
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
    /// (to reveal and copy). A process offers its executable; the path is empty when that is unknown (protected processes), and the menu then has no file actions.
    /// The last element is whether to offer "Who is using this": a file on a disk.
    pub fn menu_target(&self, id: NodeId) -> Option<(String, String, MarkState, bool)> {
        let tree = self.slot.tree.as_ref()?;
        if self.kind() == ViewKind::Processes {
            tree.aux.get(id.idx())?;
            return Some((String::new(), self.fs_path(id), self.mark_state(id), false));
        }
        let file = !tree.flags[id].contains(NodeFlags::DIR);
        Some((tree.path(id), self.fs_path(id), self.mark_state(id), file))
    }

    /// The file system path of row `id` (what Reveal and Copy path use); empty when it is not known (a process whose
    /// executable cannot be read).
    pub fn fs_path(&self, id: NodeId) -> String {
        let Some(tree) = self.slot.tree.as_ref() else { return String::new() };
        if self.kind() == ViewKind::Processes {
            let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2] as u32);
            return self.slot.proc_info.get(&pid).and_then(|i| i.path.clone()).unwrap_or_default();
        }
        fmt::node_path(tree, id).to_string()
    }

    /// The tree the context menu's process-tree entries of `id` act on (End, Suspend, Resume; the process alone is the
    /// first): the process first, then its descendants top-down, each with
    /// the pid and creation time that identify it. Only in the Processes view, and not for the Idle and System processes
    /// (pids 0 and 4). A process missing from the last snapshot (no creation time) is left out.
    pub fn tree_targets(&self, id: NodeId) -> Option<Vec<Victim>> {
        let tree = self.slot.tree.as_ref()?;
        let victim = |n: NodeId| self.process_at(n);
        let first = victim(id)?;
        let mut out = vec![first];
        let mut stack: Vec<NodeId> = tree.children(id).collect();
        stack.reverse();
        while let Some(n) = stack.pop() {
            out.extend(victim(n));
            let before = stack.len();
            stack.extend(tree.children(n));
            stack[before..].reverse();
        }
        Some(out)
    }

    /// The attributes of row `id` (`crate::attrs`).
    pub fn row_attrs(&self, id: NodeId) -> Attrs {
        self.slot.tree.as_ref().map(|t| attrs::of(t, id, (self.kind() == ViewKind::Processes).then_some(&self.slot.proc_info))).unwrap_or_default()
    }

    /// Everything the context menu of `id` offers: for a row inside a picked set of more than one, the set's menu.
    pub fn row_menu(&self, id: NodeId) -> Option<RowMenu> {
        if let Some(m) = &self.slot.model
            && m.is_picked(id)
            && m.picked_count() > 1
        {
            return Some(self.set_menu());
        }
        let (raw, path, mark, who) = self.menu_target(id)?;
        let (suspend_tree, resume_tree) = self.pause_trees(id);
        let paths = if path.is_empty() { Vec::new() } else { vec![path] };
        Some(RowMenu { raw, paths, count: 1, mark, who, end: self.tree_targets(id), pause: self.pause_target(id), suspend_tree, resume_tree })
    }

    /// The menu of a picked set: copy the paths, and mark or unmark the topmost rows together. Open, Reveal, Who is using
    /// this, Properties and the process commands stay single-row.
    fn set_menu(&self) -> RowMenu {
        let ids = self.slot.model.as_ref().map(|m| m.targets()).unwrap_or_default();
        let paths = ids.iter().map(|&i| self.fs_path(i)).filter(|p| !p.is_empty()).collect();
        let states: Vec<MarkState> = self.topmost().into_iter().map(|i| self.mark_state(i)).collect();
        let mark = if states.iter().all(|s| *s == MarkState::Unmark) {
            MarkState::Unmark
        } else if states.contains(&MarkState::Mark) {
            MarkState::Mark
        } else {
            states.iter().find_map(|s| if let MarkState::Refused(why) = s { Some(MarkState::Refused(why)) } else { None }).unwrap_or(MarkState::Refused("nothing to mark"))
        };
        RowMenu { raw: String::new(), paths, count: ids.len(), mark, who: false, end: None, pause: None, suspend_tree: Vec::new(), resume_tree: Vec::new() }
    }

    /// The tree of `id` (`tree_targets`) as Suspend process tree and Resume process tree would act on it. Suspend acts on
    /// the processes not suspended, so the system's suspension count does not nest. Resume acts on the whole tree, offered
    /// once any member is suspended: resuming a running process does nothing, and a member such as a console host can stay
    /// suspended without reading as suspended (its threads wait on something else), so resuming only those that read
    /// suspended would leave it frozen. A side that is only the row itself is empty, the entry for the process alone
    /// being the same action.
    pub fn pause_trees(&self, id: NodeId) -> (Vec<Victim>, Vec<Victim>) {
        let Some(all) = self.tree_targets(id) else { return Default::default() };
        let row = all[0].pid;
        let suspended = |v: &Victim| self.slot.proc_info.get(&v.pid).is_some_and(|i| i.suspended);
        let resume = if all.iter().any(suspended) { all.clone() } else { Vec::new() };
        let suspend: Vec<Victim> = all.into_iter().filter(|v| !suspended(v)).collect();
        let more = |l: Vec<Victim>| if l.len() == 1 && l[0].pid == row { Vec::new() } else { l };
        (more(suspend), more(resume))
    }

    /// The process a Processes row stands for, as the pid and creation time that identify it. `None` outside the
    /// Processes view, for the Idle and System processes (pids 0 and 4), and for a process missing from the last
    /// snapshot (no creation time).
    fn process_at(&self, n: NodeId) -> Option<Victim> {
        let tree = self.slot.tree.as_ref()?;
        if self.kind() != ViewKind::Processes {
            return None;
        }
        let pid = tree.aux.get(n.idx())?[2] as u32;
        let created = self.slot.proc_info.get(&pid)?.created;
        (pid > 4).then(|| Victim { pid, created, name: fmt::node_name(tree, n).to_string() })
    }

    /// What "Suspend process" or "Resume process" in the context menu of `id` acts on, and whether it is suspended now
    /// (so the menu offers Resume).
    pub fn pause_target(&self, id: NodeId) -> Option<(Victim, bool)> {
        let v = self.process_at(id)?;
        let suspended = self.slot.proc_info.get(&v.pid)?.suspended;
        Some((v, suspended))
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
        if pick == Pick::Copy {
            // Copying answers nothing: the modal stays, and its button says it was done.
            let Some(m) = self.modals.front_mut() else { return Effect::None };
            let Some(text) = m.copy.clone() else { return Effect::None };
            if let Some(b) = m.buttons.iter_mut().find(|b| b.pick == Pick::Copy) {
                b.label = "Copied";
            }
            return Effect::Copy(text);
        }
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
            (Ask::End(victims), Pick::Yes) => Effect::End(victims),
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
            Input::Click(x, y, _) | Input::DoubleClick(x, y) => match pick_at(m, x, y) {
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
            Input::Key(Key::Left | Key::Up, _) => {
                m.step(false);
                Effect::None
            }
            Input::Key(Key::Right | Key::Down, _) => {
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

    /// Shows what an action did, as a toast.
    pub fn report(&mut self, outcome: Outcome) {
        self.toast.show(outcome);
        self.layout.toast = self.toast.lines().len();
    }

    /// A toast is showing, so the window ticks it.
    pub fn toasting(&self) -> bool {
        self.toast.active()
    }

    /// Spends the toast's time up to `now`; true while it is still showing.
    pub fn toast_tick(&mut self, now: Instant) -> bool {
        let on = self.toast.tick(now, self.toast_hot);
        self.layout.toast = self.toast.lines().len();
        on
    }

    fn dismiss_toast(&mut self) {
        self.toast.dismiss();
        self.toast_hot = false;
        self.layout.toast = 0;
    }

    pub fn handle(&mut self, input: Input) -> Effect {
        self.layout = self.layout_now();
        if self.modal().is_some() && !matches!(input, Input::Resize(..) | Input::Release) {
            return self.modal_input(input);
        }
        match input {
            Input::Resize(w, h) => {
                (self.layout.w, self.layout.h) = (w, h);
                self.set_scroll(self.slot.scroll);
            }
            Input::Wheel(rows, x, y) if self.layout.procs && self.layout.held_pane().contains(x, y) => self.held_scroll(rows),
            Input::Wheel(rows, ..) => self.set_scroll((self.slot.scroll as i64 + rows as i64).max(0) as usize),
            Input::Key(k, mods) => {
                if self.slot.filter.ui.focus
                    && let Some(e) = filter::edit_for_key(k)
                {
                    return self.filter_edit(e);
                }
                self.move_focus(k, mods);
            }
            Input::Edit(Edit::Delete) if !self.slot.filter.ui.focus => self.mark_selected(),
            Input::Edit(Edit::SelectAll) if !self.slot.filter.ui.focus => self.select_all_rows(),
            Input::Enter if self.slot.filter.ui.focus => self.filter_enter(),
            Input::Enter => self.toggle_focus_row(),
            Input::Edit(Edit::Backspace) if !self.slot.filter.ui.focus => self.select_parent_row(),
            Input::Refresh => return self.refresh_key(),
            Input::ContextMenu => return self.context_menu_key(),
            Input::Edit(e) => return self.filter_edit(e),
            Input::Find => {
                self.slot.filter.ui.focus = true;
                self.slot.filter.ui.edit(Edit::SelectAll);
            }
            Input::Escape if self.toast.active() => self.dismiss_toast(),
            Input::Escape => {
                let used = self.slot.filter.ui.focus || !self.slot.filter.ui.text.is_empty();
                let e = self.filter_escape();
                return if !used && self.cancelable() { Effect::Cancel } else { e };
            }
            Input::Tab => self.slot.filter.ui.focus = !self.slot.filter.ui.focus,
            Input::Hover(x, y) => {
                self.toast_hot = matches!(self.layout.hit(x, y, self.views.len()), Hit::Toast);
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
            Input::Click(x, y, mods) => return self.click(x, y, mods),
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
                    return self.click(x, y, Mods::default());
                };
                if self.icon_at(i, x).is_some() {
                    return self.click(x, y, Mods::default());
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
                    if !(m.is_picked(id) && m.picked_count() > 1) {
                        m.select(id);
                    }
                    return Effect::Menu(id);
                } else if matches!(self.layout.hit(x, y, self.views.len()), Hit::Map)
                    && let Some(id) = treemap::hit(&self.map_tiles, x, y).and_then(|i| self.select_from_map(i))
                {
                    return Effect::Menu(id);
                }
            }
            Input::Drag(x, y) => {
                match self.pointer {
                    Pointer::Column { col, x0, w0 } => {
                        // The edge is the column's left side, so moving it left widens the column.
                        self.layout.set_col_w(col, w0 + x0 - x);
                        // Stored as it moves, and only the dragged column: the others may be clamped narrow by a small window.
                        let k = self.kind() as usize;
                        self.prefs[k].col_w[col - 1] = self.layout.col_w[col - 1];
                    }
                    Pointer::Thumb { grab } => {
                        let s = self.layout.scroll_for_thumb_y(self.total_rows(), y - grab);
                        self.set_scroll(s);
                    }
                    Pointer::MapDivider { x0, w0 } => self.layout.set_map_w(w0 + x0 - x),
                    Pointer::Rows => self.drag_select(y),
                    Pointer::Up | Pointer::Button | Pointer::Filter => {} // the window moves the filter's caret itself
                }
            }
            Input::Release => self.pointer = Pointer::Up,
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

    fn click(&mut self, x: f32, y: f32, mods: Mods) -> Effect {
        // The arms below replace this when the press starts a drag; it also ends any drag a lost release left behind.
        self.pointer = if self.hot.is_some_and(|r| r.contains(x, y)) { Pointer::Button } else { Pointer::Up };
        if self.can_elevate && self.layout.admin_button().contains(x, y) {
            return Effect::Elevate;
        }
        let hit = self.layout.hit(x, y, self.views.len());
        match hit {
            Hit::Toast => {
                self.dismiss_toast();
                return Effect::None;
            }
            Hit::FilterBox => {
                self.slot.filter.ui.focus = true;
                self.pointer = Pointer::Filter;
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
                if let Some(i) = self.active.filter(|_| !self.cancelable()) {
                    return Effect::Rescan(i);
                }
            }
            Hit::Header(c) => {
                let desc = if self.sort().0 == c { !self.sort().1 } else { c != 0 };
                let k = self.kind() as usize;
                self.prefs[k].sort = (c, desc);
                let kind = self.kind();
                if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
                    m.set_sort(t, kind.spec().cols[c].sort, desc);
                    self.slot.scroll = 0;
                }
            }
            Hit::Divider(c) => self.pointer = Pointer::Column { col: c, x0: x, w0: self.layout.col_w[c - 1] },
            Hit::MapButton => self.layout.map_on = !self.layout.map_on,
            Hit::MetricButton => return Effect::Metric(self.slot.proc_metric.next()),
            Hit::MapDivider => self.pointer = Pointer::MapDivider { x0: x, w0: self.layout.map_w },
            Hit::Map => {
                if let Some(i) = treemap::hit(&self.map_tiles, x, y) {
                    self.select_from_map(i);
                }
            }
            Hit::Row(i) => self.click_row(i, x, mods),
            Hit::Scrollbar => {
                let total = self.total_rows();
                match self.layout.thumb(total, self.slot.scroll) {
                    Some(t) if y >= t.y && y < t.y + t.h => self.pointer = Pointer::Thumb { grab: y - t.y },
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
            Scan::Idle | Scan::Refreshing => out.push_str("No drive selected\nSelect a drive above to scan it."),
            Scan::Running { view, .. } if self.views[*view].kind == ViewKind::Processes => out.push_str("Reading processes\u{2026}"),
            Scan::Running { view, dirs, files, bytes, .. } => {
                // Once a tree is listed, its root row already counts the files and bytes: only the folders are news.
                let _ = write!(out, "Scanning {}  {}", self.views[*view].title(), fmt::noun(*dirs, "folder", "folders"));
                if self.slot.tree.is_none() {
                    let _ = write!(out, ", {}, {}", fmt::noun(*files, "file", "files"), fmt::size(*bytes));
                }
            }
            Scan::Failed(m) => {
                let _ = write!(out, "Scan failed\n{m}");
            }
            Scan::Cancelled => out.push_str("Scan cancelled\nSelect a drive above to scan it again."),
        }
    }

    /// The selected row's full path (a process: its name and PID), or a set's count and total; first in the status line.
    fn selection(&self) -> Option<String> {
        let mut s = String::new();
        self.write_selection(&mut s).then_some(s)
    }

    /// Writes [`App::selection`]; false (writing nothing) when no row is picked. A set leads with how many rows are in it.
    fn write_selection(&self, out: &mut String) -> bool {
        let (Some(tree), Some(m)) = (self.slot.tree.as_ref(), self.slot.model.as_ref()) else { return false };
        let (Some(id), n) = (m.selected(), m.picked_count()) else { return false };
        if n == 0 {
            return false;
        }
        if n > 1 {
            // A set is told by its count and total; a picked folder and a picked file under it count once.
            let total: u64 = self.topmost().iter().map(|&i| tree.subtree_size[i]).sum();
            let _ = write!(out, "{n} selected, {}", (self.kind().spec().size_text)(self.slot.proc_metric, total));
            return true;
        }
        self.describe_sized(tree, id, tree.subtree_size[id]).write_line(out);
        true
    }

    /// The status line without the selection: the status bar's second row, the selection having a row of its own.
    fn state_line(&self) -> String {
        let mut s = String::new();
        self.write_status(false, false, &mut s);
        s
    }

    /// Writes the whole status line (the status bar's tooltip carries it) to `out`, which it appends to: the compact line
    /// (`full` false) has no path and no time of the last change.
    fn write_status(&self, full: bool, with_selection: bool, out: &mut String) {
        let start = out.len();
        let sep = |out: &mut String| {
            if out.len() > start {
                out.push_str("   |   ");
            }
        };
        if with_selection {
            let mark = out.len();
            sep(out);
            if !self.write_selection(out) {
                out.truncate(mark);
            }
        }
        sep(out);
        match &self.slot.scan {
            Scan::Running { paused, .. } => {
                self.write_message(out);
                if *paused {
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
            LiveStatus::On { last } => {
                sep(out);
                match last {
                    Some(t) if full => {
                        let _ = write!(out, "live, last change {t}");
                    }
                    _ => out.push_str("live"),
                }
            }
            // The banner says it, with the reason; the tooltip, which has no banner, keeps the words.
            LiveStatus::Stale(_) if full => {
                sep(out);
                out.push_str("live updates stopped");
            }
            LiveStatus::Stale(_) => {}
        }
        if self.slot.refreshing() {
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
        self.used(view)?.checked_sub(bytes)
    }

    /// The used space of the disk view `view`'s drive; `None` for the processes view or a drive that reports no size.
    pub fn used(&self, view: usize) -> Option<u64> {
        let d = self.views.get(view).filter(|d| d.kind == ViewKind::Disk && d.total > 0)?;
        Some(d.total.saturating_sub(d.free))
    }

    /// Builds the scene for `dark` and passes it to `f`; the scene borrows locals made here.
    pub fn with_scene<R>(&self, dark: bool, f: impl FnOnce(&Scene) -> R) -> R {
        let chips = self.chips();
        let (message, status, banner) = (self.message(), self.state_line(), self.banner());
        let held = self.held_text();
        let toast = self.toast.lines();
        let selection = self.selection().unwrap_or_default();
        let basket_label = self.basket_label();
        let running = matches!(self.slot.scan, Scan::Running { .. });
        let count = self.filter_count();
        let view = match (&self.slot.tree, &self.slot.model) {
            (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.slot.scroll, partial: matches!(self.slot.scan, Scan::Running { .. }), unscanned: self.unscanned(), used: self.active.and_then(|v| self.used(v)), hover: self.hover, found: self.filter_matcher(), basket: &self.slot.basket, flash: &self.slot.flash, procs: (self.kind() == ViewKind::Processes).then_some(&self.slot.proc_info) }),
            _ => None,
        };
        let map = self.layout.map_shown().then(|| MapView { tiles: &self.map_tiles, generation: self.map_gen, now: self.map_now, selected: self.map_selected_tile(), hover: self.map_hover, classes: &self.map_classes, marked: &self.basket_view.tiles });
        f(&Scene {
            layout: Layout { toast: toast.len(), ..self.layout_now() },
            metric: self.slot.proc_metric,
            held: held.as_ref().map(|(h, l)| (h.as_str(), *l)),
            theme: Theme::with_accent(dark, self.accent),
            chips: &chips,
            active_chip: self.active,
            sort: self.sort(),
            kind: self.kind(),
            admin: self.can_elevate.then(|| self.layout.admin_button()),
            // The scrollbar thumb counts as a control under the pointer, and is pressed while it is dragged.
            hot: self.hot.or(self.thumb().filter(|_| self.thumb_hover || matches!(self.pointer, Pointer::Thumb { .. }))),
            pressed: self.pressed(),
            modal: self.modals.front(),
            view,
            // The status bar already says what a running scan has found.
            message: if running { "" } else { &message },
            loading: running && self.slot.tree.is_none(),
            status: &status,
            toast: &toast,
            toast_ok: self.toast.outcome.ok,
            selection: &selection,
            banner: banner.as_deref(),
            filter: self.filter_bar(&count),
            map,
            basket: basket_label.as_deref(),
        })
    }
}

#[cfg(test)]
mod tests;
