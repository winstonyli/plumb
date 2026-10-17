//! The name filter as application state: the text box, the debounced worker run, installing its
//! result, and keeping it current while the tree changes. No Win32 types.
use super::{App, Effect};
use crate::draw::FilterBar;
use crate::filter_ui::{Edit, FilterUi, Motion};
use crate::fmt;
use crate::layout::{FILTER_PAD, Hit};
use plumb_core::NodeId;
use plumb_core::filter::{FilterError, Hits, Keep, Matcher, Names, Query};
use plumb_core::rows::Key;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Everything a worker thread needs to scan one snapshot of the names.
pub struct FilterJob {
    pub names: Names,
    pub matcher: Matcher,
    pub generation: u64,
    /// Holds the newest generation; the scan gives up once it differs from `generation`.
    pub cancel: Arc<AtomicU64>,
}

/// The pattern, hits and keep set of the last result installed.
pub(super) struct Installed {
    matcher: Matcher,
    hits: Hits,
    keep: Rc<Keep>,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub(super) struct Filter {
    pub ui: FilterUi,
    /// Bumped by every change to the query or the tree; results of older runs are dropped.
    generation: u64,
    cancel: Arc<AtomicU64>,
    /// The installed result does not match the text or the tree, so a run is wanted.
    pub dirty: bool,
    error: Option<FilterError>,
    /// The generation and pattern of the run in flight.
    running: Option<(u64, Matcher)>,
    /// Nodes changed since the running scan took its snapshot.
    touched: Vec<NodeId>,
    pub installed: Option<Installed>,
    /// Scroll position from before the filter was first installed.
    saved_scroll: usize,
}

impl Filter {
    fn query(&self) -> Query {
        Query { text: self.ui.text.clone(), regex: self.ui.regex }
    }

    /// The query or the tree changed: cancel the scan in flight and forget it.
    fn bump(&mut self) {
        // Unique across views: a result can only match the run that asked for it.
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        self.cancel.store(self.generation, Ordering::Relaxed);
        self.running = None;
        self.touched.clear();
        self.error = None;
    }
}

/// Left/Right/Home/End edit the box while it has focus; the other keys stay with the tree.
pub(super) fn edit_for_key(k: Key) -> Option<Edit> {
    match k {
        Key::Left => Some(Edit::Move { to: Motion::Left, extend: false }),
        Key::Right => Some(Edit::Move { to: Motion::Right, extend: false }),
        Key::Home => Some(Edit::Move { to: Motion::Home, extend: false }),
        Key::End => Some(Edit::Move { to: Motion::End, extend: false }),
        _ => None,
    }
}

impl App {
    pub fn filter_ui(&self) -> &FilterUi {
        &self.filter.ui
    }

    /// The input method's in-progress text at the caret (empty when it ends or is cancelled).
    pub fn set_preedit(&mut self, s: String) {
        self.filter.ui.preedit = s;
    }

    /// Whether (x, y) is in the filter box.
    pub fn over_filter_box(&self, x: f32, y: f32) -> bool {
        matches!(self.layout.hit(x, y, self.views.len()), Hit::FilterBox)
    }

    pub fn filter_box(&self) -> crate::layout::Rect {
        self.layout.filter_box()
    }

    /// Where the filter text starts on screen: the x a click is measured from.
    pub fn filter_origin(&self) -> f32 {
        self.layout.filter_box().x + FILTER_PAD
    }

    /// True when the installed result is out of date, so `start_filter` has work to do.
    pub fn filter_dirty(&self) -> bool {
        self.filter.dirty
    }

    pub fn filter_installed(&self) -> bool {
        self.filter.installed.is_some()
    }

    /// The pattern of the result on screen, for highlighting what it matched in the names.
    pub(super) fn filter_matcher(&self) -> Option<&Matcher> {
        self.filter.installed.as_ref().map(|i| &i.matcher)
    }

    /// "1,240 matches", "searching…", "invalid pattern", or empty.
    pub(super) fn filter_count(&self) -> String {
        let f = &self.filter;
        if let Some(e) = &f.error {
            return match e {
                FilterError::Invalid(_) => "invalid pattern",
                FilterError::TooComplex => "pattern too complex",
            }
            .into();
        }
        if f.ui.text.is_empty() {
            return String::new();
        }
        if f.dirty {
            return "searching\u{2026}".into();
        }
        match &f.installed {
            Some(i) => {
                let n = i.keep.matches();
                format!("{} {}", fmt::count(n as u64), if n == 1 { "match" } else { "matches" })
            }
            None => String::new(),
        }
    }

    pub(super) fn filter_bar<'a>(&'a self, count: &'a str) -> FilterBar<'a> {
        let u = &self.filter.ui;
        FilterBar {
            text: &u.text,
            caret: u.caret,
            regex: u.regex,
            focus: u.focus,
            sel: u.selection(),
            preedit: &u.preedit,
            error: self.filter.error.is_some(),
            count,
        }
    }

    /// The text or the Regex toggle changed. An emptied box clears the filter at once; anything
    /// else waits for the debounce timer.
    pub(super) fn filter_changed(&mut self) -> Effect {
        self.filter.bump();
        if self.filter.ui.text.is_empty() {
            self.clear_filter_results();
            return Effect::None;
        }
        self.filter.dirty = true;
        Effect::Filter
    }

    pub(super) fn filter_edit(&mut self, e: Edit) -> Effect {
        if !self.filter.ui.focus {
            return Effect::None;
        }
        if self.filter.ui.edit(e) { self.filter_changed() } else { Effect::None }
    }

    /// Esc: clears the text if there is any, and takes the focus off the box.
    pub(super) fn filter_escape(&mut self) -> Effect {
        self.filter.ui.focus = false;
        if self.filter.ui.clear() { self.filter_changed() } else { Effect::None }
    }

    /// Enter in the box: focus moves to the tree, on its first row.
    pub(super) fn filter_enter(&mut self) {
        if !self.filter.ui.focus {
            return;
        }
        self.filter.ui.focus = false;
        if let Some(m) = &mut self.model
            && let Some(id) = m.rows().first().map(|r| r.id)
        {
            m.select(id);
        }
        self.set_scroll(0);
    }

    /// Removes the filter from the rows and forgets the installed result.
    fn clear_filter_results(&mut self) {
        self.filter.dirty = false;
        if self.filter.installed.take().is_some() {
            self.unfilter_rows();
        }
    }

    /// Takes the filter off the rows and returns to the scroll position from before it.
    pub(super) fn unfilter_rows(&mut self) {
        let (vis, saved) = (self.layout.rows_visible(), self.filter.saved_scroll);
        let s = match (&self.tree, &mut self.model) {
            (Some(t), Some(m)) => {
                m.set_filter(t, None);
                Some(m.scroll_to_selected(saved, vis))
            }
            _ => None,
        };
        if let Some(s) = s {
            self.set_scroll(s);
        }
    }

    /// The tree is being replaced: drop everything derived from the old one. With text in the box
    /// `dirty` stays set, so the window starts a run on the new tree.
    pub(super) fn filter_reset(&mut self) {
        self.filter.bump();
        self.filter.installed = None;
        self.filter.dirty = !self.filter.ui.text.is_empty();
    }

    /// Compiles the query and snapshots the names for a worker, if a run is wanted. An invalid
    /// pattern sets the error and keeps the previous result on screen.
    pub fn start_filter(&mut self) -> Option<FilterJob> {
        let tree = self.tree.as_ref()?;
        // Nothing to do when the result is current, or when a scan for this very query is running.
        if !self.filter.dirty || self.filter.running.is_some() {
            return None;
        }
        match self.filter.query().compile() {
            Err(e) => {
                self.filter.error = Some(e);
                self.filter.dirty = false;
                None
            }
            Ok(None) => {
                self.clear_filter_results();
                None
            }
            Ok(Some(matcher)) => {
                let f = &mut self.filter;
                f.error = None;
                f.touched.clear();
                f.running = Some((f.generation, matcher.clone()));
                Some(FilterJob { names: Names::of(tree), matcher, generation: f.generation, cancel: f.cancel.clone() })
            }
        }
    }

    /// Installs a finished scan unless a newer query or tree has replaced it.
    pub fn filter_ready(&mut self, generation: u64, mut hits: Hits) {
        let f = &mut self.filter;
        let Some((_, matcher)) = f.running.take_if(|(g, _)| *g == generation) else { return };
        let (Some(tree), Some(model)) = (&self.tree, &mut self.model) else { return };
        // The tree kept changing while the worker scanned: re-test what changed since the snapshot
        // (nodes added since are covered by `update`, which also tests everything past the end).
        hits.update(tree, &matcher, &f.touched);
        f.touched.clear();
        let keep = Rc::new(Keep::build(tree, &hits));
        if f.installed.is_none() {
            f.saved_scroll = self.scroll;
        }
        model.set_filter(tree, Some(keep.clone()));
        f.installed = Some(Installed { matcher, hits, keep });
        f.dirty = false;
        self.set_scroll(0);
    }

    /// Before the rows refresh: re-tests the changed nodes against the installed pattern and
    /// rebuilds the keep set when one of them was, or became, part of it. Also logs the changes
    /// for a run in flight.
    pub(super) fn filter_live(&mut self) {
        let (Some(tree), Some(model)) = (&self.tree, &mut self.model) else { return };
        let f = &mut self.filter;
        let touched = tree.touched();
        if touched.is_empty() {
            return;
        }
        if f.running.is_some() {
            f.touched.extend_from_slice(touched);
        }
        let Some(i) = &mut f.installed else { return };
        i.hits.update(tree, &i.matcher, touched);
        if touched.iter().any(|&id| i.keep.is_kept(id) || i.hits.get(id)) {
            i.keep = Rc::new(Keep::build(tree, &i.hits));
            model.update_keep(i.keep.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ViewInfo, Input};
    use crate::layout::{Layout, HEADER_H, PICKER_H, ROW_H};
    use plumb_core::{NodeFlags, Tree, TreeBuilder};

    /// C:\{docs\{notes.txt, todo.md}, src\{main.rs, deep\more.txt}, readme.md}
    fn tree() -> Tree {
        let mut b = TreeBuilder::new();
        let root = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let docs = b.push_str(root, "docs", 0, NodeFlags::DIR);
        b.push_str(docs, "notes.txt", 30, NodeFlags::empty());
        b.push_str(docs, "todo.md", 20, NodeFlags::empty());
        let src = b.push_str(root, "src", 0, NodeFlags::DIR);
        b.push_str(src, "main.rs", 50, NodeFlags::empty());
        let deep = b.push_str(src, "deep", 0, NodeFlags::DIR);
        b.push_str(deep, "more.txt", 10, NodeFlags::empty());
        b.push_str(root, "readme.md", 5, NodeFlags::empty());
        b.finish().unwrap()
    }

    fn app() -> App {
        let views = vec![ViewInfo { root: "C:\\".into(), total: 1000, free: 500, kind: crate::app::ViewKind::Disk }];
        let mut a = App::new(Layout::new(800.0, 600.0), views, "test".into());
        a.begin_scan(0);
        a.finish_scan(tree(), "done".into());
        a
    }

    fn names(a: &App) -> Vec<String> {
        let t = a.tree.as_ref().unwrap();
        a.model.as_ref().unwrap().rows().iter().map(|r| t.name_string(r.id)).collect()
    }

    fn row_y(i: usize) -> f32 {
        PICKER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Focuses the box (as Ctrl+F does) and types `s`; returns the last effect.
    fn type_text(a: &mut App, s: &str) -> Effect {
        a.handle(Input::Find);
        let mut e = Effect::None;
        for c in s.chars() {
            e = a.handle(Input::Edit(Edit::Char(c)));
        }
        e
    }

    /// Runs the wanted scan on this thread, the way the worker would.
    fn run(a: &mut App) {
        let j = a.start_filter().expect("a run was wanted");
        let h = j.names.scan(&j.matcher, &j.cancel, j.generation).expect("not cancelled");
        a.filter_ready(j.generation, h);
    }

    #[test]
    fn typing_then_running_shows_matches_and_their_folders() {
        let mut a = app();
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
        assert_eq!(type_text(&mut a, "txt"), Effect::Filter);
        assert!(a.filter_dirty());
        assert_eq!(a.filter_count(), "searching\u{2026}");
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"], "the old rows stay until the result lands");
        run(&mut a);
        assert_eq!(names(&a), ["C:", "src", "deep", "more.txt", "docs", "notes.txt"]);
        assert_eq!(a.filter_count(), "2 matches");
        assert!(!a.filter_dirty() && a.filter_installed());
    }

    #[test]
    fn a_result_from_an_older_query_is_dropped() {
        let mut a = app();
        type_text(&mut a, "md");
        let j = a.start_filter().unwrap();
        a.handle(Input::Edit(Edit::Char('x'))); // a newer query cancels the scan
        assert!(j.names.scan(&j.matcher, &j.cancel, j.generation).is_none());
        let h = j.names.scan(&j.matcher, &AtomicU64::new(j.generation), j.generation).unwrap();
        a.filter_ready(j.generation, h);
        assert!(!a.filter_installed() && a.filter_dirty());
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
    }

    #[test]
    fn escape_clears_the_filter_and_restores_the_view() {
        let mut a = app();
        let before = names(&a);
        let root = a.model.as_ref().unwrap().selected();
        type_text(&mut a, "txt");
        run(&mut a);
        assert_ne!(names(&a), before);
        assert_eq!(a.handle(Input::Escape), Effect::None);
        assert_eq!(names(&a), before);
        assert!(!a.filter_installed() && !a.filter_dirty());
        assert_eq!((a.filter_ui().text.as_str(), a.filter_ui().focus), ("", false));
        assert_eq!(a.model.as_ref().unwrap().selected(), root);
    }

    #[test]
    fn escape_in_the_tree_clears_a_filter_too() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.handle(Input::Tab); // focus to the tree
        assert!(!a.filter_ui().focus);
        a.handle(Input::Escape);
        assert_eq!(names(&a).len(), 4);
        assert_eq!(a.filter_ui().text, "");
    }

    #[test]
    fn an_invalid_pattern_keeps_the_previous_result_and_says_so() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        let shown = names(&a);
        let r = a.layout.filter_regex();
        assert_eq!(a.handle(Input::Click(r.x + 2.0, r.y + 2.0)), Effect::Filter); // regex on, "txt" still valid
        a.handle(Input::Edit(Edit::Char('('))); // "txt(" is not
        assert!(a.start_filter().is_none());
        assert_eq!(a.filter_count(), "invalid pattern");
        assert!(a.filter_bar("").error && a.filter_installed());
        assert_eq!(names(&a), shown);
        a.handle(Input::Edit(Edit::Backspace));
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches");
        assert!(!a.filter_bar("").error);
    }

    #[test]
    fn the_regex_toggle_changes_how_the_text_is_read() {
        let mut a = app();
        type_text(&mut a, "t.t");
        run(&mut a);
        assert_eq!(a.filter_count(), "0 matches");
        assert!(names(&a).is_empty(), "with no match even the root is hidden");
        assert_eq!(a.model.as_ref().unwrap().selected(), None);
        let r = a.layout.filter_regex();
        assert_eq!(a.handle(Input::Click(r.x + 2.0, r.y + 2.0)), Effect::Filter);
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches"); // t, any character, t: "txt" in both names
    }

    #[test]
    fn the_clear_button_empties_the_box() {
        let mut a = app();
        let c = a.layout.filter_clear();
        assert_eq!(a.handle(Input::Click(c.x + 2.0, c.y + 2.0)), Effect::None);
        assert!(a.filter_ui().focus);
        type_text(&mut a, "txt");
        run(&mut a);
        assert_eq!(a.handle(Input::Click(c.x + 2.0, c.y + 2.0)), Effect::None);
        assert_eq!((a.filter_ui().text.as_str(), a.filter_installed()), ("", false));
        assert_eq!(names(&a).len(), 4);
    }

    #[test]
    fn live_changes_keep_the_filtered_rows_current() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        let src = a.tree.as_ref().unwrap().find("C:\\src").unwrap();
        let t = a.tree.as_mut().unwrap();
        t.add_child(src, &u("new.txt"), 40, NodeFlags::empty());
        a.refresh_rows();
        assert!(names(&a).contains(&"new.txt".to_string()));
        assert_eq!(a.filter_count(), "3 matches");
        let more = a.tree.as_ref().unwrap().find("C:\\src\\deep\\more.txt").unwrap();
        a.tree.as_mut().unwrap().rename(more, &u("more.rs"));
        a.refresh_rows();
        let n = names(&a);
        assert!(!n.contains(&"more.rs".to_string()) && !n.contains(&"deep".to_string()), "{n:?}");
        assert_eq!(a.filter_count(), "2 matches");
        let notes = a.tree.as_ref().unwrap().find("C:\\docs\\notes.txt").unwrap();
        a.tree.as_mut().unwrap().remove(notes);
        a.refresh_rows();
        assert!(!names(&a).contains(&"docs".to_string()));
        assert_eq!(a.filter_count(), "1 match");
    }

    #[test]
    fn changes_made_while_the_worker_scans_are_not_lost() {
        let mut a = app();
        type_text(&mut a, "txt");
        let j = a.start_filter().unwrap(); // the snapshot is taken here
        let src = a.tree.as_ref().unwrap().find("C:\\src").unwrap();
        let main = a.tree.as_ref().unwrap().find("C:\\src\\main.rs").unwrap();
        let t = a.tree.as_mut().unwrap();
        t.add_child(src, &u("late.txt"), 7, NodeFlags::empty()); // past the snapshot's end
        t.rename(main, &u("main.txt")); // inside it
        a.refresh_rows();
        let h = j.names.scan(&j.matcher, &j.cancel, j.generation).unwrap();
        a.filter_ready(j.generation, h);
        let n = names(&a);
        assert!(n.contains(&"late.txt".to_string()) && n.contains(&"main.txt".to_string()), "{n:?}");
        assert_eq!(a.filter_count(), "4 matches");
    }

    #[test]
    fn a_swapped_tree_drops_the_filter_and_asks_for_a_new_run() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.swap_tree(tree(), "again".into());
        assert!(!a.filter_installed() && a.filter_dirty());
        assert_eq!(names(&a), ["C:", "src", "docs", "readme.md"]);
        run(&mut a);
        assert_eq!(names(&a), ["C:", "src", "deep", "more.txt", "docs", "notes.txt"]);
    }

    #[test]
    fn a_new_scan_keeps_the_text_and_wants_a_run_when_it_finishes() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.begin_scan(0);
        assert!(a.start_filter().is_none(), "no tree yet");
        a.finish_scan(tree(), "done".into());
        assert!(a.filter_dirty());
        run(&mut a);
        assert_eq!(a.filter_count(), "2 matches");
    }

    #[test]
    fn keys_go_to_the_box_only_while_it_has_focus() {
        let mut a = app();
        let root = a.model.as_ref().unwrap().selected();
        a.handle(Input::Edit(Edit::Char('z'))); // not focused: ignored
        assert_eq!(a.filter_ui().text, "");
        let b = a.layout.filter_box();
        a.handle(Input::Click(b.x + 10.0, b.y + 5.0));
        assert!(a.filter_ui().focus);
        a.handle(Input::Edit(Edit::Char('a')));
        a.handle(Input::Edit(Edit::Char('b')));
        a.handle(Input::Key(Key::Left));
        assert_eq!(a.filter_ui().caret, 1);
        assert_eq!(a.model.as_ref().unwrap().selected(), root, "Left edited the box, not the tree");
        a.handle(Input::Key(Key::Down));
        assert_ne!(a.model.as_ref().unwrap().selected(), root, "Down still moves the tree selection");
        a.handle(Input::Click(20.0, row_y(1)));
        assert!(!a.filter_ui().focus);
        a.handle(Input::Edit(Edit::Char('z')));
        assert_eq!(a.filter_ui().text, "ab");
    }

    #[test]
    fn pressing_in_the_box_focuses_it_and_the_window_places_the_caret_by_offset() {
        let mut a = app();
        type_text(&mut a, "foo bar");
        a.handle(Input::Tab);
        let b = a.layout.filter_box();
        assert!(a.over_filter_box(b.x + 20.0, b.y + 5.0) && !a.over_filter_box(b.x - 20.0, b.y + 5.0));
        assert!(a.filter_origin() > b.x && a.filter_origin() < b.x + b.w);
        a.handle(Input::Click(b.x + 20.0, b.y + 5.0));
        assert!(a.filter_ui().focus);
        a.handle(Input::Edit(Edit::Place { at: 2, extend: false }));
        a.handle(Input::Edit(Edit::Place { at: 6, extend: true }));
        assert_eq!(a.filter_ui().selected_text(), Some("o ba"));
        a.handle(Input::Edit(Edit::Word(5)));
        assert_eq!(a.filter_ui().selected_text(), Some("bar"));
        assert_eq!(a.filter_bar("").sel, Some((4, 7)));
        a.handle(Input::Edit(Edit::Char('x')));
        assert_eq!(a.filter_ui().text, "foo x");
    }

    #[test]
    fn find_selects_the_text_enter_and_tab_move_focus() {
        let mut a = app();
        type_text(&mut a, "ab");
        a.handle(Input::Tab);
        assert!(!a.filter_ui().focus);
        a.handle(Input::Find);
        assert!(a.filter_ui().focus && a.filter_ui().selection() == Some((0, 2)));
        a.handle(Input::Edit(Edit::Char('q')));
        assert_eq!(a.filter_ui().text, "q");
        a.handle(Input::Key(Key::Down)); // move the selection off the first row
        a.handle(Input::Enter);
        assert!(!a.filter_ui().focus);
        let m = a.model.as_ref().unwrap();
        assert_eq!(m.selected(), Some(m.rows()[0].id));
        a.handle(Input::Enter); // not focused: nothing happens
        a.handle(Input::Tab);
        assert!(a.filter_ui().focus);
    }

    #[test]
    fn sorting_under_a_filter_keeps_the_filter() {
        let mut a = app();
        type_text(&mut a, "txt");
        run(&mut a);
        a.handle(Input::Click(5.0, PICKER_H + 2.0)); // sort by Name ascending
        assert_eq!(names(&a), ["C:", "docs", "notes.txt", "src", "deep", "more.txt"]);
    }
}
