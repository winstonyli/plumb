//! Selecting several rows: Shift and Ctrl clicks, dragging over rows, Shift+arrows, Ctrl+A, and what Del and Ctrl+C do
//! with the set. The model keeps the set (`plumb_core::rows`); this maps input onto it.
use super::*;

impl App {
    /// Whether a press on a row is being dragged over more rows (the window keeps a timer running meanwhile).
    pub fn row_dragging(&self) -> bool {
        self.row_drag
    }

    /// A press on visible row `i` at x: Shift extends the selection to it, Ctrl flips it, a plain press selects it. The
    /// folder icon opens or closes the folder as always and ignores the modifiers. A press that is not Ctrl and not on
    /// the icon starts a drag.
    pub(super) fn click_row(&mut self, i: usize, x: f32, mods: Mods) {
        let folder = self.icon_at(i, x);
        let Some(id) = self.row_at(i) else { return };
        let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) else { return };
        let mut again = false;
        if folder.is_some() {
            m.select(id);
            m.toggle(t, id);
            self.set_scroll(self.slot.scroll);
        } else if mods.shift {
            m.extend_to(id);
        } else if mods.ctrl {
            m.toggle_pick(id);
        } else {
            again = m.selected() == Some(id);
            m.select(id);
        }
        self.row_drag = folder.is_none() && !mods.ctrl;
        if again {
            self.held_mark_stale();
        }
    }

    /// The pointer is down on a row and is at height `y`: the selection extends to the row under it, and the list scrolls
    /// 1 to 5 rows while the pointer is beyond the top or bottom of it, by how far beyond.
    pub(super) fn drag_select(&mut self, y: f32) {
        let area = self.layout.rows_area();
        let beyond = if y < area.y {
            y - area.y
        } else if y >= area.y + area.h {
            y - (area.y + area.h) + 1.0
        } else {
            0.0
        };
        if beyond != 0.0 {
            let step = ((beyond.abs() / ROW_H).ceil() as i64).clamp(1, 5);
            self.set_scroll((self.slot.scroll as i64 + step * beyond.signum() as i64).max(0) as usize);
        }
        let on_screen = ((y - area.y) / ROW_H).max(0.0) as usize;
        let idx = (self.slot.scroll + on_screen.min(self.layout.rows_visible().saturating_sub(1))).min(self.total_rows().saturating_sub(1));
        if let Some(m) = &mut self.slot.model
            && let Some(id) = m.rows().get(idx).map(|r| r.id)
        {
            m.extend_to(id);
        }
    }

    /// A moving key on the list: the focus moves, and with Shift the selection extends to it.
    pub(super) fn move_focus(&mut self, k: Key, mods: Mods) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
            m.handle_key(t, k, self.layout.rows_visible().max(1), mods.shift);
            let s = m.scroll_to_selected(self.slot.scroll, self.layout.rows_visible());
            self.set_scroll(s);
        }
    }

    /// Enter on the list: opens or closes the focus row's folder.
    pub(super) fn toggle_focus_row(&mut self) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model)
            && let Some(id) = m.selected()
        {
            m.toggle(t, id);
            self.set_scroll(self.slot.scroll);
        }
    }

    /// Backspace on the list: selects the parent folder.
    pub(super) fn select_parent_row(&mut self) {
        if let (Some(t), Some(m)) = (&self.slot.tree, &mut self.slot.model) {
            m.select_parent(t);
            let s = m.scroll_to_selected(self.slot.scroll, self.layout.rows_visible());
            self.set_scroll(s);
        }
    }

    /// F5: no tree (never scanned, cancelled, failed) scans the shown view; a tree rescans it in the background; a scan in
    /// flight, or no view shown, does nothing. In the Processes view a rescan only wakes the sampler.
    pub(super) fn refresh_key(&mut self) -> Effect {
        match self.active {
            Some(i) if !self.cancelable() => if self.slot.tree.is_none() { Effect::Scan(i) } else { Effect::Rescan(i) },
            _ => Effect::None,
        }
    }

    /// Shift+F10 / the Apps key: scrolls the focus row into view and asks for its menu.
    pub(super) fn context_menu_key(&mut self) -> Effect {
        let vis = self.layout.rows_visible();
        let Some(m) = &self.slot.model else { return Effect::None };
        let (Some(id), Some(_)) = (m.selected(), m.selected_index()) else { return Effect::None };
        let s = m.scroll_to_selected(self.slot.scroll, vis);
        self.set_scroll(s);
        Effect::Menu(id)
    }

    /// Where the focus row is on screen (client DIPs), if it is.
    pub fn focus_row_rect(&self) -> Option<Rect> {
        let i = self.slot.model.as_ref()?.selected_index()?.checked_sub(self.slot.scroll)?;
        (i < self.layout.rows_visible()).then(|| self.layout.row(i))
    }

    /// Ctrl+A with the list focused.
    pub(super) fn select_all_rows(&mut self) {
        if let Some(m) = &mut self.slot.model {
            m.select_all();
        }
    }

    /// The picked rows that have no picked ancestor, in row order: what Del and the set menu act on, so that Ctrl+A then Del
    /// marks the roots and not every row under them.
    pub(super) fn topmost(&self) -> Vec<NodeId> {
        let (Some(t), Some(m)) = (&self.slot.tree, &self.slot.model) else { return Vec::new() };
        let covered = |id: NodeId| {
            let mut p = t.parent[id];
            while !p.is_none() {
                if m.is_picked(p) {
                    return true;
                }
                p = t.parent[p];
            }
            false
        };
        m.targets().into_iter().filter(|&id| !covered(id)).collect()
    }

    /// Del (and the menu's Mark selected): over the topmost picked rows, if each is marked exactly (the test `mark_state`
    /// uses) it unmarks the exact mark of every picked row; otherwise it marks each not yet marked. Rows that cannot be
    /// marked are skipped and the status line says how many and why.
    pub fn mark_selected(&mut self) {
        let Some(tree) = self.slot.tree.as_ref() else { return };
        let top = self.topmost();
        if top.is_empty() {
            return;
        }
        let marked: Vec<bool> = top.iter().map(|&id| self.slot.basket.contains(&tree.path(id))).collect();
        if marked.iter().all(|&m| m) {
            let exact: Vec<String> = self.slot.model.as_ref().map(|m| m.targets()).unwrap_or_default().into_iter().map(|id| tree.path(id)).filter(|p| self.slot.basket.contains(p)).collect();
            for p in &exact {
                self.slot.basket.toggle(p);
            }
            self.basket_rev += 1;
            return;
        }
        let (mut todo, mut refused) = (Vec::new(), Vec::new());
        for (&id, _) in top.iter().zip(&marked).filter(|(_, m)| !**m) {
            match self.mark_state(id) {
                MarkState::Refused(why) => refused.push(why),
                _ => todo.push(tree.path(id)),
            }
        }
        for p in &todo {
            self.slot.basket.toggle(p);
        }
        if !todo.is_empty() {
            self.basket_rev += 1;
        }
        if let Some(why) = refused.first() {
            self.outcome = Outcome::problem(if todo.is_empty() && refused.len() == 1 {
                format!("cannot mark: {why}")
            } else {
                format!("{} not marked: {why}", refused.len())
            });
        }
    }

    /// The file system paths of the picked rows, nested ones included, one per line (CRLF), for the clipboard; `None`
    /// when there are none to copy.
    pub fn copy_text(&self) -> Option<String> {
        let m = self.slot.model.as_ref()?;
        let paths: Vec<String> = m.targets().into_iter().map(|id| self.fs_path(id)).filter(|p| !p.is_empty()).collect();
        (!paths.is_empty()).then(|| paths.join("\r\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Input, ViewInfo, ViewKind};
    use crate::filter_ui::Edit;
    use crate::layout::{Layout, HEADER_H, INDENT, PICKER_H, ROW_H};
    use plumb_core::{NodeFlags, Tree, TreeBuilder};

    const NONE: Mods = Mods { shift: false, ctrl: false };
    const SHIFT: Mods = Mods { shift: true, ctrl: false };
    const CTRL: Mods = Mods { shift: false, ctrl: true };

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

    /// Every folder open: C:, src, main.rs, deep, more.txt, docs, notes.txt, todo.md, readme.md.
    pub(super) fn app_h(h: f32) -> App {
        let views = vec![ViewInfo { root: "C:\\".into(), total: 1000, free: 500, kind: ViewKind::Disk }];
        let mut a = App::new(Layout::new(800.0, h), views, "test".into());
        a.begin_scan(0);
        a.finish_scan(tree(), "done".into());
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
        while let Some(id) = m.rows().iter().map(|r| r.id).find(|&id| m.is_expandable(t, id) && !m.is_expanded(id)) {
            m.expand(t, id);
        }
        a
    }

    pub(super) fn names(a: &App) -> Vec<String> {
        let t = a.slot.tree.as_ref().unwrap();
        a.slot.model.as_ref().unwrap().rows().iter().map(|r| t.name_string(r.id)).collect()
    }

    pub(super) fn at(a: &App, name: &str) -> usize {
        names(a).iter().position(|n| n == name).unwrap_or_else(|| panic!("no row {name}"))
    }

    pub(super) fn picked(a: &App) -> Vec<String> {
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_ref().unwrap());
        m.targets().into_iter().map(|id| t.name_string(id)).collect()
    }

    pub(super) fn focus(a: &App) -> Option<String> {
        let (t, m) = (a.slot.tree.as_ref()?, a.slot.model.as_ref()?);
        m.selected().map(|id| t.name_string(id))
    }

    pub(super) fn row_y(i: usize) -> f32 {
        PICKER_H + HEADER_H + i as f32 * ROW_H + 1.0
    }

    /// A press on the name of visible row `i` (clear of the folder icon).
    pub(super) fn press(a: &mut App, i: usize, mods: Mods) -> Effect {
        a.handle(Input::Click(150.0, row_y(i), mods))
    }

    pub(super) fn key(a: &mut App, k: Key, mods: Mods) {
        a.handle(Input::Key(k, mods));
    }

    #[test]
    fn the_fixture_lists_every_row_in_size_order() {
        let a = app_h(600.0);
        assert_eq!(names(&a), ["C:", "src", "main.rs", "deep", "more.txt", "docs", "notes.txt", "todo.md", "readme.md"]);
    }

    #[test]
    fn shift_click_picks_the_range_from_the_anchor_and_ctrl_click_flips_one_row() {
        let mut a = app_h(600.0);
        let (main, more, docs) = (at(&a, "main.rs"), at(&a, "more.txt"), at(&a, "docs"));
        press(&mut a, main, NONE);
        assert_eq!(picked(&a), ["main.rs"]);
        press(&mut a, more, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"]);
        press(&mut a, docs, CTRL);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt", "docs"]);
        press(&mut a, more, CTRL);
        assert_eq!(picked(&a), ["main.rs", "deep", "docs"]);
        press(&mut a, main, NONE);
        assert_eq!(picked(&a), ["main.rs"], "a plain click drops the set");
    }

    #[test]
    fn the_folder_icon_opens_and_closes_whatever_modifiers_are_held() {
        let mut a = app_h(600.0);
        let docs = at(&a, "docs");
        let x = a.layout.column(0).0 + 8.0 + INDENT + 4.0; // docs is at depth 1
        a.handle(Input::Click(x, row_y(docs), CTRL));
        assert!(!names(&a).contains(&"notes.txt".to_string()), "closed");
        assert_eq!(picked(&a), ["docs"]);
        assert!(!a.row_dragging(), "the icon starts no drag");
    }

    #[test]
    fn shift_arrows_extend_from_the_anchor_and_a_plain_arrow_ends_the_set() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        key(&mut a, Key::Down, SHIFT);
        key(&mut a, Key::Down, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"]);
        assert_eq!(focus(&a).as_deref(), Some("more.txt"));
        key(&mut a, Key::Up, SHIFT);
        assert_eq!(picked(&a), ["main.rs", "deep"]);
        key(&mut a, Key::Down, NONE);
        assert_eq!(picked(&a), ["more.txt"]);
    }

    #[test]
    fn ctrl_a_picks_every_row_unless_the_filter_box_has_the_focus() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        a.handle(Input::Edit(Edit::SelectAll));
        assert_eq!(picked(&a).len(), 9);
        assert_eq!(focus(&a).as_deref(), Some("main.rs"), "the focus stays");
        press(&mut a, main, NONE);
        a.handle(Input::Find); // focus the box
        a.handle(Input::Edit(Edit::SelectAll));
        assert_eq!(picked(&a), ["main.rs"], "in the box, Ctrl+A selects its text");
    }

    #[test]
    fn shift_left_in_the_filter_box_leaves_the_row_selection_alone() {
        let mut a = app_h(600.0);
        let main = at(&a, "main.rs");
        press(&mut a, main, NONE);
        a.handle(Input::Find);
        key(&mut a, Key::Left, SHIFT); // the window turns Shift+Left/Right/Home/End into box edits before this; Left stays the box's
        assert_eq!(picked(&a), ["main.rs"]);
    }

    #[test]
    fn a_drag_extends_to_the_row_under_the_pointer_and_a_ctrl_press_starts_none() {
        let mut a = app_h(600.0);
        press(&mut a, 2, NONE);
        assert!(a.row_dragging());
        a.handle(Input::Drag(150.0, row_y(5)));
        assert_eq!(picked(&a).len(), 4, "rows 2 to 5");
        a.handle(Input::Drag(150.0, row_y(1)));
        assert_eq!(picked(&a).len(), 2, "rows 1 to 2: the anchor stays on row 2");
        a.handle(Input::Release);
        assert!(!a.row_dragging());
        press(&mut a, 3, CTRL);
        assert!(!a.row_dragging(), "Ctrl starts no drag");
        let before = picked(&a);
        a.handle(Input::Drag(150.0, row_y(7)));
        assert_eq!(picked(&a), before);
    }

    #[test]
    fn dragging_past_the_bottom_edge_scrolls_and_keeps_extending() {
        let mut a = app_h(200.0);
        let vis = a.layout.rows_visible();
        assert!((2..9).contains(&vis), "the list must scroll for this test, {vis} rows fit");
        press(&mut a, 0, NONE);
        let area = a.layout.rows_area();
        a.handle(Input::Drag(150.0, area.y + area.h + 30.0));
        assert!(a.slot.scroll > 0, "scrolled");
        let n = a.slot.model.as_ref().unwrap().picked_count();
        assert!(n > vis - 1, "the range grew past the rows that were on screen: {n}");
        a.handle(Input::Release);
        a.handle(Input::Drag(150.0, area.y + 2.0));
        assert_eq!(a.slot.model.as_ref().unwrap().picked_count(), n, "no drag after release");
    }

    #[test]
    fn a_right_click_keeps_a_picked_set_and_otherwise_selects_the_row() {
        let mut a = app_h(600.0);
        let (main, more, docs) = (at(&a, "main.rs"), at(&a, "more.txt"), at(&a, "docs"));
        press(&mut a, main, NONE);
        press(&mut a, more, SHIFT);
        let id = a.slot.model.as_ref().unwrap().rows()[main].id;
        assert_eq!(a.handle(Input::RightClick(150.0, row_y(main))), Effect::Menu(id));
        assert_eq!(picked(&a), ["main.rs", "deep", "more.txt"], "the set survives a right-click on one of its rows");
        a.handle(Input::RightClick(150.0, row_y(docs)));
        assert_eq!(picked(&a), ["docs"], "an unpicked row becomes the selection");
    }

    fn status_str(a: &App, full: bool) -> String {
        let mut s = String::new();
        a.write_status(full, true, true, &mut s);
        s
    }

    #[test]
    fn del_marks_every_picked_row_then_unmarks_them_all() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2);
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "all marked: Del unmarks");
    }

    #[test]
    fn del_over_a_mix_marks_the_rows_not_yet_marked() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        a.handle(Input::Edit(Edit::Delete)); // notes marked
        press(&mut a, todo, SHIFT); // picks notes and todo
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2, "todo was marked, notes kept its mark");
    }

    #[test]
    fn del_over_nested_picks_marks_only_the_topmost() {
        let mut a = app_h(600.0);
        let (src, deep) = (at(&a, "src"), at(&a, "more.txt"));
        press(&mut a, src, NONE);
        press(&mut a, deep, SHIFT); // src, main.rs, deep, more.txt
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 1);
        let src_id = a.slot.model.as_ref().unwrap().rows()[src].id;
        assert!(a.basket().contains(&a.slot.tree.as_ref().unwrap().path(src_id)));
    }

    #[test]
    fn del_marks_a_child_of_a_marked_folder_as_it_always_did() {
        let mut a = app_h(600.0);
        let (src, main) = (at(&a, "src"), at(&a, "main.rs"));
        press(&mut a, src, NONE);
        a.handle(Input::Edit(Edit::Delete));
        press(&mut a, main, NONE);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 2);
    }

    #[test]
    fn a_refused_row_is_skipped_and_the_status_line_says_why() {
        let mut a = app_h(600.0);
        press(&mut a, 0, NONE); // the drive root
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("cannot mark: it is a drive root"), "{}", status_str(&a, true));
        a.handle(Input::Edit(Edit::SelectAll)); // everything, root included: the root covers all, and is refused
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "Ctrl+A then Del at a drive root marks nothing");
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        a.slot.scan = Scan::Refreshing; // marks are refused while a scan runs
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty());
        assert!(status_str(&a, true).starts_with("2 not marked: the drive is being scanned"), "{}", status_str(&a, true));
    }

    #[test]
    fn copy_text_lists_the_paths_of_every_picked_row_one_per_line() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        let m = a.slot.model.as_ref().unwrap();
        let want: Vec<String> = m.targets().into_iter().map(|id| a.fs_path(id)).collect();
        assert_eq!(want.len(), 2);
        assert!(want.iter().all(|p| p.ends_with(".txt") || p.ends_with(".md")), "{want:?}");
        assert_eq!(a.copy_text(), Some(want.join("\r\n")));
        a.handle(Input::Edit(Edit::SelectAll));
        let all = a.copy_text().unwrap();
        assert_eq!(all.split("\r\n").count(), 9, "nested rows are copied too");
    }

    #[test]
    fn enter_opens_and_closes_the_focus_folder_and_the_box_keeps_its_own_enter() {
        let mut a = app_h(600.0);
        let docs = at(&a, "docs");
        press(&mut a, docs, NONE);
        a.handle(Input::Enter);
        assert!(!names(&a).contains(&"notes.txt".to_string()), "closed");
        a.handle(Input::Enter);
        assert!(names(&a).contains(&"notes.txt".to_string()), "open again");
        a.handle(Input::Find);
        a.handle(Input::Edit(Edit::Char('x')));
        a.handle(Input::Enter); // moves focus to the list, on the first row
        assert!(!a.filter_ui().focus, "Enter left the box");
        assert_eq!(focus(&a), names(&a).first().cloned());
    }

    #[test]
    fn backspace_selects_the_parent_and_the_box_keeps_its_own_backspace() {
        let mut a = app_h(600.0);
        let more = at(&a, "more.txt");
        press(&mut a, more, NONE);
        a.handle(Input::Edit(Edit::Backspace));
        assert_eq!(focus(&a).as_deref(), Some("deep"));
        assert_eq!(picked(&a), ["deep"]);
        a.handle(Input::Find);
        a.handle(Input::Edit(Edit::Char('q')));
        a.handle(Input::Edit(Edit::Backspace));
        assert_eq!(focus(&a).as_deref(), Some("deep"), "Backspace in the box edits its text");
        assert_eq!(a.filter_ui().text, "");
    }

    #[test]
    fn f5_scans_when_there_is_no_tree_rescans_with_one_and_waits_for_a_running_scan() {
        let mut a = app_h(600.0);
        a.active = Some(0);
        assert_eq!(a.handle(Input::Refresh), Effect::Rescan(0));
        a.slot.scan = Scan::Refreshing;
        assert_eq!(a.handle(Input::Refresh), Effect::None, "a scan is already running");
        a.slot.scan = Scan::Idle;
        a.fail_scan("boom".into());
        assert_eq!(a.handle(Input::Refresh), Effect::Scan(0), "no tree: F5 scans again");
        a.active = None;
        assert_eq!(a.handle(Input::Refresh), Effect::None, "no view shown");
    }

    #[test]
    fn the_context_menu_key_opens_the_focus_row_menu_and_brings_the_row_into_view() {
        let views = vec![ViewInfo { root: "C:\\".into(), total: 1000, free: 500, kind: ViewKind::Disk }];
        let mut empty = App::new(Layout::new(800.0, 200.0), views, "test".into());
        assert_eq!(empty.handle(Input::ContextMenu), Effect::None, "no tree, nothing focused");
        let mut a = app_h(200.0);
        press(&mut a, 0, NONE);
        key(&mut a, Key::End, NONE);
        a.slot.scroll = 0; // the focus is off screen
        let want = a.slot.model.as_ref().unwrap().selected().unwrap();
        assert_eq!(a.handle(Input::ContextMenu), Effect::Menu(want));
        let r = a.focus_row_rect().expect("the row was scrolled into view");
        let area = a.layout.rows_area();
        assert!(r.y >= area.y && r.y + r.h <= area.y + area.h + 0.5, "{r:?} within {area:?}");
    }

    #[test]
    fn the_menu_of_a_picked_set_is_the_set_form_and_a_single_row_keeps_today_s() {
        let mut a = app_h(600.0);
        let (notes, todo) = (at(&a, "notes.txt"), at(&a, "todo.md"));
        press(&mut a, notes, NONE);
        press(&mut a, todo, SHIFT);
        let id = a.slot.model.as_ref().unwrap().rows()[notes].id;
        let set = a.row_menu(id).unwrap();
        assert_eq!((set.count, set.paths.len(), set.raw.as_str()), (2, 2, ""));
        assert_eq!(set.mark, MarkState::Mark);
        assert!(!set.who && set.end.is_none() && set.pause.is_none());
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.row_menu(id).unwrap().mark, MarkState::Unmark, "every topmost row is marked");
        press(&mut a, notes, NONE);
        let one = a.row_menu(id).unwrap();
        assert_eq!((one.count, one.paths.len()), (1, 1));
        assert_eq!(one.raw, a.slot.tree.as_ref().unwrap().path(id));
    }

    #[test]
    fn a_set_with_nothing_markable_shows_the_first_reason() {
        let mut a = app_h(600.0);
        press(&mut a, 0, NONE);
        press(&mut a, 1, SHIFT); // the drive root and src
        let id = a.slot.model.as_ref().unwrap().rows()[0].id;
        assert_eq!(a.row_menu(id).unwrap().mark, MarkState::Refused("it is a drive root"), "the root covers src, so only the root is topmost");
    }
}
