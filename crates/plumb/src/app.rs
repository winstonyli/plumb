//! Window-independent application state: what is selected, scrolled and scanning, and how
//! input changes it. No Win32 types, so the whole interaction model tests anywhere.
use crate::draw::{Chip, Scene, Theme, View};
use crate::fmt;
use crate::layout::{Hit, Layout};
use crate::restore::Snapshot;
use plumb_core::rows::{Key, RowModel, SortKey};
use plumb_core::{NodeId, Tree};

#[derive(Clone, Debug, PartialEq)]
pub struct DriveInfo {
    pub letter: char,
    pub total: u64,
    pub free: u64,
}

pub enum Scan {
    Idle,
    Running { drive: usize, dirs: u64, files: u64, bytes: u64 },
    Failed(String),
}

/// Whether the tree is being kept up to date from the change journal.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveStatus {
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
    /// Mouse moved with the left button held, at this y.
    Drag(f32),
    Release,
    Resize(f32, f32),
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
}

/// The sort each header column stands for; "% of parent" orders by size.
fn sort_key(col: usize) -> SortKey {
    [SortKey::Name, SortKey::Size, SortKey::Size, SortKey::Items][col]
}

pub struct App {
    pub layout: Layout,
    pub drives: Vec<DriveInfo>,
    pub active: Option<usize>,
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
    /// Header column last clicked (0 Name, 1 Size, 2 % of parent, 3 Items) and direction.
    sort: (usize, bool),
    /// While dragging the scrollbar thumb: distance from the thumb's top to the grab point.
    grab: Option<f32>,
}

impl App {
    pub fn new(layout: Layout, drives: Vec<DriveInfo>, adapter: String) -> Self {
        Self {
            layout, drives, adapter, active: None, tree: None, model: None, scroll: 0,
            can_elevate: false, scan: Scan::Idle, summary: String::new(), sort: (1, true), grab: None,
            live: LiveStatus::Off, refreshing: false,
        }
    }

    /// The header column the rows are sorted by, and whether descending.
    pub fn sort(&self) -> (usize, bool) {
        self.sort
    }

    pub fn begin_scan(&mut self, drive: usize) {
        self.active = Some(drive);
        self.tree = None;
        self.model = None;
        self.scroll = 0;
        self.summary.clear();
        self.set_live(LiveStatus::Off);
        self.refreshing = false;
        self.scan = Scan::Running { drive, dirs: 0, files: 0, bytes: 0 };
    }

    pub fn progress(&mut self, d: u64, f: u64, b: u64) {
        if let Scan::Running { dirs, files, bytes, .. } = &mut self.scan {
            (*dirs, *files, *bytes) = (d, f, b);
        }
    }

    /// Installs a finished tree. The root is expanded and selected so there is something to see.
    pub fn finish_scan(&mut self, tree: Tree, summary: String) {
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
        self.set_live(LiveStatus::Off);
    }

    /// Replaces the tree with a rescanned one, keeping the sort, the expanded folders and the
    /// selection (by path) and where it sits on screen.
    pub fn swap_tree(&mut self, tree: Tree, summary: String) {
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
        self.set_live(LiveStatus::Off);
    }

    /// Brings the rows up to date after the tree changed in place.
    pub fn refresh_rows(&mut self) {
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

    pub fn fail_scan(&mut self, msg: String) {
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

    fn row_at(&self, i: usize) -> Option<NodeId> {
        self.model.as_ref()?.rows().get(self.scroll + i).map(|r| r.id)
    }

    pub fn handle(&mut self, input: Input) -> Effect {
        match input {
            Input::Resize(w, h) => {
                self.layout = Layout::new(w, h).with_banner(self.layout.banner);
                self.set_scroll(self.scroll);
            }
            Input::Wheel(rows) => self.set_scroll((self.scroll as i64 + rows as i64).max(0) as usize),
            Input::Key(k) => {
                if let (Some(t), Some(m)) = (&self.tree, &mut self.model) {
                    m.handle_key(t, k, self.layout.rows_visible().max(1));
                    let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
                    self.set_scroll(s);
                }
            }
            Input::Click(x, y) => return self.click(x, y),
            Input::DoubleClick(x, y) => {
                if let Hit::Row(i) = self.layout.hit(x, y, self.drives.len()) {
                    if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.tree, &mut self.model) {
                        m.select(id);
                        m.toggle(t, id);
                        self.set_scroll(self.scroll);
                    }
                }
            }
            Input::RightClick(x, y) => {
                if let Hit::Row(i) = self.layout.hit(x, y, self.drives.len()) {
                    if let (Some(id), Some(m)) = (self.row_at(i), &mut self.model) {
                        m.select(id);
                        return Effect::Menu(id);
                    }
                }
            }
            Input::Drag(y) => {
                if let Some(g) = self.grab {
                    let s = self.layout.scroll_for_thumb_y(self.total_rows(), y - g);
                    self.set_scroll(s);
                }
            }
            Input::Release => self.grab = None,
        }
        Effect::None
    }

    fn click(&mut self, x: f32, y: f32) -> Effect {
        if self.can_elevate && self.layout.admin_button().contains(x, y) {
            return Effect::Elevate;
        }
        match self.layout.hit(x, y, self.drives.len()) {
            Hit::Chip(i) if !matches!(self.scan, Scan::Running { .. }) => return Effect::Scan(i),
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
            Hit::Row(i) => {
                if let (Some(id), Some(m)) = (self.row_at(i), &mut self.model) {
                    m.select(id);
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
            .map(|d| Chip {
                label: format!("{}:  {} free", d.letter, fmt::size(d.free)),
                used: 1.0 - fmt::fraction(d.free, d.total),
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
        }
    }

    fn status(&self) -> String {
        let mut parts = vec![if self.summary.is_empty() { "plumb".to_string() } else { self.summary.clone() }];
        match &self.live {
            LiveStatus::Off => {}
            LiveStatus::On { last: None } => parts.push("live".into()),
            LiveStatus::On { last: Some(t) } => parts.push(format!("live, last change {t}")),
            LiveStatus::Stale(_) => parts.push("live updates stopped".into()),
        }
        if self.refreshing {
            parts.push("refreshing\u{2026}".into());
        }
        parts.push(self.adapter.clone());
        parts.join("   |   ")
    }

    fn banner(&self) -> Option<String> {
        match &self.live {
            LiveStatus::Stale(why) => Some(format!("live updates stopped: {why}; the tree may be out of date")),
            _ => None,
        }
    }

    /// Builds the scene for `dark` and passes it to `f`; the scene borrows locals made here.
    pub fn with_scene<R>(&self, dark: bool, f: impl FnOnce(&Scene) -> R) -> R {
        let chips = self.chips();
        let (message, status, banner) = (self.message(), self.status(), self.banner());
        let view = match (&self.tree, &self.model) {
            (Some(tree), Some(model)) => Some(View { tree, model, scroll: self.scroll }),
            _ => None,
        };
        f(&Scene {
            layout: self.layout,
            theme: Theme::new(dark),
            chips: &chips,
            active_chip: self.active,
            sort: self.sort,
            admin: self.can_elevate.then(|| self.layout.admin_button()),
            view,
            message: &message,
            status: &status,
            banner: banner.as_deref(),
            filter: crate::draw::FilterBar::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CHIP_GAP, CHIP_W, FILTER_H, HEADER_H, PICKER_H, ROW_H};
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
        PICKER_H + FILTER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    #[test]
    fn clicking_a_chip_requests_a_scan_unless_one_is_running() {
        let mut a = app();
        assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0)), Effect::Scan(1));
        a.begin_scan(1);
        assert_eq!(a.handle(Input::Click(10.0, 10.0)), Effect::None);
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
    fn chips_show_free_space_and_used_fraction() {
        let c = app().chips();
        assert_eq!(c[0].label, "C:  250 B free");
        assert!((c[0].used - 0.75).abs() < 1e-6);
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
        let hy = PICKER_H + FILTER_H + 2.0;
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
        a.handle(Input::Drag(th.y + 2.0 + 100.0));
        assert!(a.scroll > 0);
        a.handle(Input::Release);
        let s = a.scroll;
        a.handle(Input::Drag(th.y + 300.0));
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
        assert_eq!(a.status(), "done   |   test");
        a.set_live(LiveStatus::On { last: None });
        assert_eq!(a.status(), "done   |   live   |   test");
        a.live_changed("12:03:41".into());
        a.refreshing = true;
        assert_eq!(a.status(), "done   |   live, last change 12:03:41   |   refreshing\u{2026}   |   test");
        a.set_live(LiveStatus::Stale("x".into()));
        a.live_changed("12:04:00".into());
        assert!(a.status().contains("live updates stopped") && !a.status().contains("12:04"));
    }

    #[test]
    fn a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row() {
        let mut a = scanned();
        a.handle(Input::Click(5.0, PICKER_H + FILTER_H + 2.0)); // sort by Name ascending
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
