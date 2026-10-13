//! Window-independent application state: what is selected, scrolled and scanning, and how
//! input changes it. No Win32 types, so the whole interaction model tests anywhere.
mod filter;

use crate::draw::{Chip, Mark, Scene, Theme, View};
use crate::filter_ui::Edit;
use crate::fmt;
use crate::layout::{Hit, Layout, Rect, INDENT, PICKER_H};
use crate::restore::Snapshot;
use plumb_core::rows::{Key, RowModel, SortKey};
use plumb_core::{NodeId, Tree};

pub use filter::FilterJob;

#[derive(Clone, Debug, PartialEq)]
pub struct DriveInfo {
    pub letter: char,
    pub total: u64,
    pub free: u64,
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
}

/// The sort each header column stands for; "% of parent" orders by size.
fn sort_key(col: usize) -> SortKey {
    [SortKey::Name, SortKey::Size, SortKey::Size, SortKey::Items][col]
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
}

impl Parked {
    pub fn has_tree(&self) -> bool {
        self.tree.is_some()
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
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
    /// Header column last clicked (0 Name, 1 Size, 2 % of parent, 3 Items) and direction.
    sort: (usize, bool),
    /// While dragging the scrollbar thumb: distance from the thumb's top to the grab point.
    grab: Option<f32>,
    /// While dragging a column edge: the column, and the pointer x and column width at the start.
    col_drag: Option<(usize, f32, f32)>,
    filter: filter::Filter,
}

impl App {
    pub fn new(layout: Layout, drives: Vec<DriveInfo>, adapter: String) -> Self {
        let layout = Layout { chips: drives.len(), ..layout };
        Self {
            layout, drives, adapter, active: None, marks: Vec::new(), hover: None, hot: None, tree: None, model: None, scroll: 0,
            can_elevate: false, scan: Scan::Idle, summary: String::new(), sort: (1, true), grab: None, col_drag: None,
            live: LiveStatus::Off, refreshing: false, preview_paused: false, filter: Default::default(),
        }
    }

    /// The header column the rows are sorted by, and whether descending.
    pub fn sort(&self) -> (usize, bool) {
        self.sort
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
        self.layout = self.layout.with_banner(matches!(self.live, LiveStatus::Stale(_))).with_cancel(self.cancelable());
        self.set_scroll(self.scroll);
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

    /// The partial tree of a scan that is no longer followed on screen is dropped.
    pub fn discard_partial(&mut self) {
        self.tree = None;
        self.model = None;
        self.scroll = 0;
        self.preview_paused = false;
    }

    pub fn begin_scan(&mut self, drive: usize) {
        self.filter_reset();
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
        let mut model = RowModel::new(&tree);
        model.set_sort(&tree, sort_key(self.sort.0), self.sort.1);
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
        self.scroll = 0;
        self.model = None;
        let Some(tree) = &mut self.tree else { return };
        tree.take_touched(); // the model is built from the tree as it is now
        let mut model = RowModel::new(tree);
        model.set_sort(tree, sort_key(self.sort.0), self.sort.1);
        if let Some(r) = model.rows().first().map(|r| r.id) {
            model.expand(tree, r);
            model.select(r);
        }
        self.model = Some(model);
    }

    /// Replaces the tree with a rescanned one, keeping the sort, the expanded folders and the
    /// selection (by path) and where it sits on screen.
    pub fn swap_tree(&mut self, tree: Tree, summary: String) {
        if self.filter.installed.is_some() {
            self.unfilter_rows();
        }
        let kept = match (&self.tree, &self.model) {
            (Some(t), Some(m)) => Some(Snapshot::take(t, m, self.scroll)),
            _ => None,
        };
        let Some(snap) = kept else { return self.finish_scan(tree, summary) };
        let (model, scroll) = snap.apply(&tree, sort_key(self.sort.0), self.sort.1, self.layout.rows_visible());
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
        }
        self.refreshing = false;
    }

    pub fn fail_scan(&mut self, msg: String) {
        self.tree = None;
        self.model = None;
        self.preview_paused = false;
        self.scan = Scan::Failed(msg);
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
        self.col_drag.is_some() || matches!(self.layout.hit(x, y, self.drives.len()), Hit::Divider(_))
    }

    /// The icon button at (x, y), if there is one there, with its tooltip: these buttons carry no label.
    fn button_at(&self, x: f32, y: f32) -> Option<(Rect, &'static str)> {
        let l = &self.layout;
        if self.can_elevate && l.admin_button().contains(x, y) {
            return Some((l.admin_button(), "Run as administrator"));
        }
        match l.hit(x, y, self.drives.len()) {
            Hit::Rescan => Some((l.rescan_button(), "Rescan")),
            Hit::Cancel => Some((l.cancel_button(), "Cancel scan")),
            Hit::FilterRegex => Some((l.filter_regex(), "Regular expression")),
            _ => None,
        }
    }

    /// The tooltip at (x, y): an icon button's name, or a drive chip's full free space (its label shows
    /// only the free size, to fit).
    pub fn tip_at(&self, x: f32, y: f32) -> Option<String> {
        if let Hit::Chip(i) = self.layout.hit(x, y, self.drives.len()) {
            let d = self.drives.get(i)?;
            return Some(format!("{}: {} free of {}", d.letter, fmt::size(d.free), fmt::size(d.total)));
        }
        self.button_at(x, y).map(|b| b.1.to_string())
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
        match input {
            Input::Resize(w, h) => {
                self.layout = Layout { w, h, ..self.layout };
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
            }
            Input::Click(x, y) => return self.click(x, y),
            // Windows sends the second press of a quick pair as a double-click instead of a click, so
            // anywhere but a row it must act as a click, or the second press of a header, button or
            // scrollbar would be lost.
            Input::DoubleClick(x, y) => {
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
                    && let (Some(id), Some(m)) = (self.row_at(i), &mut self.model) {
                        m.select(id);
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
                }
            }
            Input::Release => {
                self.grab = None;
                self.col_drag = None;
            }
        }
        Effect::None
    }

    fn click(&mut self, x: f32, y: f32) -> Effect {
        if self.can_elevate && self.layout.admin_button().contains(x, y) {
            return Effect::Elevate;
        }
        let hit = self.layout.hit(x, y, self.drives.len());
        match hit {
            Hit::FilterBox => {
                self.filter.ui.focus = true;
                self.filter.ui.all = false;
                self.filter.ui.caret = self.filter.ui.text.len();
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
                if let (Some(t), Some(m)) = (&self.tree, &mut self.model) {
                    m.set_sort(t, sort_key(c), desc);
                    self.scroll = 0;
                }
            }
            Hit::Divider(c) => self.col_drag = Some((c, x, self.layout.col_w[c - 1])),
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
                label: format!("{}:  {}", d.letter, fmt::size(d.free)),
                used: 1.0 - fmt::fraction(d.free, d.total),
                mark: if self.active == Some(i) { Mark::None } else { self.marks.get(i).copied().unwrap_or_default() },
            })
            .collect()
    }

    fn message(&self) -> String {
        match &self.scan {
            Scan::Idle => "Select a drive above to scan it.".into(),
            Scan::Running { drive, dirs, files, bytes } => format!(
                "Scanning {}:  {} folders, {} files, {}",
                self.drives[*drive].letter, fmt::count(*dirs), fmt::count(*files), fmt::size(*bytes)
            ),
            Scan::Failed(m) => format!("Scan failed: {m}"),
            Scan::Cancelled => "Scan cancelled. Select a drive above to scan it again.".into(),
        }
    }

    fn status(&self) -> String {
        let head = match &self.scan {
            Scan::Running { .. } => {
                let paused = if self.preview_paused { "   (preview paused)" } else { "" };
                format!("{}{paused}", self.message())
            }
            _ if self.summary.is_empty() => "plumb".to_string(),
            _ => self.summary.clone(),
        };
        let mut parts = vec![head];
        match &self.live {
            LiveStatus::Off => {}
            LiveStatus::On { last: None } => parts.push("live".into()),
            LiveStatus::On { last: Some(t) } => parts.push(format!("live, last change {t}")),
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
        let (message, status, banner) = (self.message(), self.status(), self.banner());
        let count = self.filter_count();
        let view = match (&self.tree, &self.model) {
            (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.scroll, partial: matches!(self.scan, Scan::Running { .. }), unscanned: self.unscanned(), hover: self.hover }),
            _ => None,
        };
        f(&Scene {
            layout: self.layout.with_cancel(self.cancelable()),
            theme: Theme::new(dark),
            chips: &chips,
            active_chip: self.active,
            sort: self.sort,
            admin: self.can_elevate.then(|| self.layout.admin_button()),
            hot: self.hot,
            view,
            message: &message,
            status: &status,
            adapter: &self.adapter,
            banner: banner.as_deref(),
            filter: self.filter_bar(&count),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CHIP_GAP, CHIP_W, HEADER_H, PICKER_H, ROW_H};
    use plumb_core::synth::kary_builder;

    fn app() -> App {
        let drives = vec![
            DriveInfo { letter: 'C', total: 1000, free: 250 },
            DriveInfo { letter: 'D', total: 1000, free: 900 },
        ];
        App::new(Layout::new(800.0, 600.0), drives, "test".into())
    }

    fn scanned() -> App {
        let mut a = app();
        a.begin_scan(0);
        a.finish_scan(kary_builder(300, 3).finish().unwrap(), "done".into());
        a
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
        assert!(a.status().starts_with("Scanning"));
        // The user selects another row, then the exact tree (the same shape) arrives.
        let second = a.model.as_ref().unwrap().rows()[1].id;
        a.model.as_mut().unwrap().select(second);
        let name = a.tree.as_ref().unwrap().path(second);
        a.swap_tree(kary_builder(60, 3).finish().unwrap(), "done".into());
        assert!(matches!(a.scan, Scan::Idle));
        let sel = a.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.tree.as_ref().unwrap().path(sel), name, "the selection survives by path");
        assert!(a.status().starts_with("done"));
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
    fn the_status_line_shows_the_live_marker_and_refreshing() {
        let mut a = scanned();
        assert_eq!(a.status(), "done");
        a.set_live(LiveStatus::On { last: None });
        assert_eq!(a.status(), "done   |   live");
        a.live_changed("12:03:41".into());
        a.refreshing = true;
        assert_eq!(a.status(), "done   |   live, last change 12:03:41   |   refreshing\u{2026}");
        a.set_live(LiveStatus::Stale("x".into()));
        a.live_changed("12:04:00".into());
        assert!(a.status().contains("live updates stopped") && !a.status().contains("12:04"));
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
}
