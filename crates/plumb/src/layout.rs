//! Pure geometry in device-independent pixels (DIPs). No Win32 types, so it tests anywhere.

pub use plumb_core::Rect;

pub const PICKER_H: f32 = 44.0;
pub const HEADER_H: f32 = 24.0;
/// One row of the status bar: the selection, legend and buttons on the first, the scan and live state on the second.
pub const STATUS_ROW_H: f32 = 26.0;
pub const STATUS_H: f32 = 2.0 * STATUS_ROW_H;
pub const ROW_H: f32 = 22.0;
pub const SCROLLBAR_W: f32 = 12.0;
/// A drive chip's width until its label has been measured (see `App::fit_chips`).
pub const CHIP_W: f32 = 104.0;
/// Space either side of a chip's label.
pub const CHIP_PAD: f32 = 8.0;
/// Space between the filter box's left edge and its text.
pub const FILTER_PAD: f32 = 8.0;
pub const CHIP_GAP: f32 = 8.0;
/// The side of every icon button: administrator (a shield), Rescan (a refresh arrow), Cancel (a cross) and the map toggle.
/// They are squares, centred in the band they sit in (see `icon_button`).
pub const ICON_BTN: f32 = 22.0;
/// Room kept at the right end of the picker bar for the system's minimize, maximize and close buttons,
/// which Windows draws over the top of the window (the title bar is part of the client area).
pub const CAPTION_RESERVE: f32 = 150.0;
pub const BANNER_H: f32 = 28.0;
/// What follows the filter box: gap, match count.
const FILTER_AFTER_W: f32 = 8.0 + FILTER_COUNT_W;
pub const FILTER_BOX_H: f32 = 28.0;
pub const FILTER_BOX_W: f32 = 304.0;
pub const FILTER_BOX_MIN: f32 = 184.0;
/// The Regex toggle sits inside the right end of the filter box, inset 4 px from its edges: a square.
pub const FILTER_REGEX_W: f32 = FILTER_BOX_H - 8.0;
pub const FILTER_COUNT_W: f32 = 110.0;
pub const FILTER_CLEAR_W: f32 = 24.0;
pub const BASKET_W: f32 = 190.0;
pub const METRIC_W: f32 = 72.0;
/// The Processes view's "Files held" pane: a title line and this many file lines.
pub const HELD_ROWS: usize = 6;
pub const HELD_H: f32 = (HELD_ROWS as f32 + 1.0) * ROW_H;
pub const INDENT: f32 = 20.0;
/// Starting widths of the Size, % of parent, Items and Modified columns; Name takes the rest.
pub const COL_W: [f32; 4] = [100.0, 130.0, 90.0, 96.0];
/// Narrowest a dragged column may get, and narrowest the Name column may be squeezed.
pub const MIN_COL_W: f32 = 50.0;
pub const MIN_NAME_W: f32 = 120.0;
/// Half-width of the grab zone around a column's left edge in the header.
const GRIP: f32 = 4.0;
pub const DIVIDER_W: f32 = 5.0;
pub const MAP_MIN_W: f32 = 160.0;
pub const MAP_W: f32 = 420.0;

/// An icon button: a square of `ICON_BTN` with its right edge at `right`, centred vertically in `band`.
fn icon_button(right: f32, band: Rect) -> Rect {
    Rect { x: (right - ICON_BTN).max(0.0), y: band.y + (band.h - ICON_BTN) / 2.0, w: ICON_BTN, h: ICON_BTN }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Chip(usize),
    /// Column header: 0 Name, 1 Size, 2 % of parent, 3 Items, 4 Modified.
    Header(usize),
    /// The left edge of column 1..=4 in the header, where a drag resizes that column.
    Divider(usize),
    /// Index into the visible window (0 is the first drawn row).
    Row(usize),
    Scrollbar,
    /// The Rescan button on the stale banner.
    Rescan,
    /// The Cancel button in the status bar, shown while a scan runs.
    Cancel,
    /// The marked-items chip in the status bar.
    Basket,
    /// The filter text box (not its clear button).
    FilterBox,
    FilterRegex,
    FilterClear,
    /// The treemap's tile area.
    Map,
    /// The strip between the tree and the map, where a drag resizes the pane.
    MapDivider,
    /// The status bar's show/hide button.
    MapButton,
    /// The Processes view's map metric button (Memory / CPU / I/O).
    MetricButton,
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    /// A banner strip sits between the picker and the header.
    pub banner: bool,
    /// A Cancel button sits at the right end of the status bar.
    pub cancel: bool,
    /// The marked-items chip sits in the status bar, left of Cancel and the map button.
    pub basket: bool,
    /// The metric button sits in the status bar, left of where Cancel shows (the Processes view).
    pub metric: bool,
    /// The "Files held" pane sits under the rows (the Processes view).
    pub held: bool,
    /// Widths of the Size, % of parent, Items and Modified columns.
    pub col_w: [f32; 4],
    /// The width of each drive chip, left to right: their sum decides where the filter box starts.
    pub chips: Vec<f32>,
    /// The map pane is wanted.
    pub map_on: bool,
    /// The pane's wanted width in DIPs; clamped when used.
    pub map_w: f32,
}

impl Layout {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h, banner: false, cancel: false, basket: false, metric: false, held: false, col_w: COL_W, chips: Vec::new(), map_on: false, map_w: MAP_W }
    }

    /// Sets the width of column `c` (1..=4), keeping it at least `MIN_COL_W` and the Name column at least `MIN_NAME_W`.
    pub fn set_col_w(&mut self, c: usize, w: f32) {
        let others: f32 = self.col_w.iter().sum::<f32>() - self.col_w[c - 1];
        let max = (self.rows_area().w - MIN_NAME_W - others).max(MIN_COL_W);
        self.col_w[c - 1] = w.clamp(MIN_COL_W, max);
    }

    /// The narrowest the tree may get beside the map: a Name column at its minimum, the other columns and the scrollbar.
    fn tree_min(&self) -> f32 {
        MIN_NAME_W + self.col_w.iter().sum::<f32>() + SCROLLBAR_W
    }

    /// Whether the map pane is on screen: wanted, and the window is wide enough for the tree, the divider and the pane.
    pub fn map_shown(&self) -> bool {
        self.map_on && self.w >= self.tree_min() + DIVIDER_W + MAP_MIN_W
    }

    fn map_pane_w(&self) -> f32 {
        if self.map_shown() { self.map_w.clamp(MAP_MIN_W, self.w - self.tree_min() - DIVIDER_W) } else { 0.0 }
    }

    /// The width the tree's header and rows get: the whole window unless the map pane is shown.
    pub fn tree_w(&self) -> f32 {
        if self.map_shown() { self.w - self.map_pane_w() - DIVIDER_W } else { self.w }
    }

    /// Sets the wanted pane width, within what the window allows.
    pub fn set_map_w(&mut self, w: f32) {
        self.map_w = w.clamp(MAP_MIN_W, (self.w - self.tree_min() - DIVIDER_W).max(MAP_MIN_W));
    }

    fn body_h(&self) -> f32 {
        (self.h - self.top() - STATUS_H).max(0.0)
    }

    /// The whole pane, beside the header and rows: the tiles fill it (the legend is in the status bar).
    pub fn map(&self) -> Option<Rect> {
        self.map_shown().then(|| Rect { x: self.tree_w() + DIVIDER_W, y: self.top(), w: self.map_pane_w(), h: self.body_h() })
    }

    /// The strip between the tree and the pane, where a drag resizes the pane.
    pub fn map_divider(&self) -> Option<Rect> {
        self.map_shown().then(|| Rect { x: self.tree_w(), y: self.top(), w: DIVIDER_W, h: self.body_h() })
    }

    /// Shows or hides the pane, at the right end of the status bar.
    pub fn map_button(&self) -> Rect {
        let s = self.status_top();
        icon_button(s.x + s.w - 8.0, s)
    }


    /// What the map weighs the processes by, left of the Cancel slot so the two never overlap.
    pub fn metric_button(&self) -> Rect {
        let c = self.cancel_button();
        Rect { x: (c.x - 4.0 - METRIC_W).max(0.0), w: METRIC_W, ..c }
    }

    pub fn with_held(&self, held: bool) -> Self {
        Self { held, ..self.clone() }
    }

    /// The "Files held" pane, across the tree's width above the status bar.
    pub fn held_pane(&self) -> Rect {
        Rect { x: 0.0, y: (self.h - STATUS_H - HELD_H).max(0.0), w: self.tree_w(), h: HELD_H }
    }

    pub fn with_metric(&self, metric: bool) -> Self {
        Self { metric, ..self.clone() }
    }

    pub fn with_basket(&self, basket: bool) -> Self {
        Self { basket, ..self.clone() }
    }

    /// The marked-items chip in the status bar, left of the Cancel button (or of the map button when there is none).
    pub fn basket_chip(&self) -> Rect {
        let right = if self.cancel { self.cancel_button().x } else { self.map_button().x } - 4.0;
        Rect { x: (right - BASKET_W).max(0.0), w: BASKET_W, ..self.map_button() }
    }

    pub fn with_cancel(&self, cancel: bool) -> Self {
        Self { cancel, ..self.clone() }
    }

    pub fn with_banner(&self, banner: bool) -> Self {
        Self { banner, ..self.clone() }
    }

    /// Top edge of the header: below the picker bar, and below the banner when there is one.
    fn top(&self) -> f32 {
        PICKER_H + if self.banner { BANNER_H } else { 0.0 }
    }

    /// The filter box shares the picker bar: after the drive chips, before the administrator button. It
    /// takes `FILTER_BOX_W` when there is room and shrinks (to `FILTER_BOX_MIN`) when there is not.
    /// The match count follows it; the Regex toggle is inside it.
    pub fn filter_box(&self) -> Rect {
        let room = self.admin_button().x - 8.0 - self.filter_left() - FILTER_AFTER_W;
        Rect { x: self.filter_left(), y: (PICKER_H - FILTER_BOX_H) / 2.0, w: room.clamp(FILTER_BOX_MIN, FILTER_BOX_W), h: FILTER_BOX_H }
    }

    /// Left edge of the filter box: after the drive chips.
    fn filter_left(&self) -> f32 {
        self.chip_x(self.chips.len()) + 8.0
    }

    /// Left edge of chip `i` (the end of the last chip, plus a gap, for `i` past it).
    fn chip_x(&self, i: usize) -> f32 {
        8.0 + self.chips.iter().take(i).map(|w| w + CHIP_GAP).sum::<f32>()
    }

    /// The Regex toggle: at the right end inside the filter box.
    pub fn filter_regex(&self) -> Rect {
        let b = self.filter_box();
        Rect { x: b.x + b.w - 4.0 - FILTER_REGEX_W, y: b.y + 4.0, w: FILTER_REGEX_W, h: b.h - 8.0 }
    }

    /// Match count or status text, after the filter box.
    pub fn filter_count(&self) -> Rect {
        let b = self.filter_box();
        Rect { x: b.x + b.w + 8.0, y: 0.0, w: FILTER_COUNT_W, h: PICKER_H }
    }

    /// The clear button: inside the box, left of the Regex toggle.
    pub fn filter_clear(&self) -> Rect {
        let (b, r) = (self.filter_box(), self.filter_regex());
        Rect { x: r.x - FILTER_CLEAR_W, y: b.y, w: FILTER_CLEAR_W, h: b.h }
    }

    pub fn banner_strip(&self) -> Rect {
        Rect { x: 0.0, y: PICKER_H, w: self.w, h: BANNER_H }
    }

    /// "Rescan", right-aligned in the banner.
    pub fn rescan_button(&self) -> Rect {
        icon_button(self.w - 8.0, self.banner_strip())
    }

    pub fn chip(&self, i: usize) -> Rect {
        Rect { x: self.chip_x(i), y: 6.0, w: self.chips.get(i).copied().unwrap_or(CHIP_W), h: PICKER_H - 12.0 }
    }

    /// "Run as administrator" (a shield), right-aligned in the picker bar.
    pub fn admin_button(&self) -> Rect {
        icon_button(self.w - CAPTION_RESERVE - 8.0, Rect { x: 0.0, y: 0.0, w: self.w, h: PICKER_H })
    }

    pub fn header(&self) -> Rect {
        Rect { x: 0.0, y: self.top(), w: self.tree_w(), h: HEADER_H }
    }

    /// Row area, excluding the header and scrollbar.
    pub fn rows_area(&self) -> Rect {
        let y = self.top() + HEADER_H;
        let pane = if self.held { HELD_H } else { 0.0 };
        Rect { x: 0.0, y, w: (self.tree_w() - SCROLLBAR_W).max(0.0), h: (self.h - y - STATUS_H - pane).max(0.0) }
    }

    pub fn scrollbar(&self) -> Rect {
        let a = self.rows_area();
        Rect { x: a.x + a.w, y: a.y, w: SCROLLBAR_W.min(self.w), h: a.h }
    }

    /// "Cancel", right-aligned in the status bar.
    pub fn cancel_button(&self) -> Rect {
        let s = self.status_top();
        icon_button(self.map_button().x - 4.0, s)
    }

    /// The status bar's first row, where the buttons sit.
    pub fn status_top(&self) -> Rect {
        Rect { h: STATUS_ROW_H, ..self.status() }
    }

    pub fn status(&self) -> Rect {
        Rect { x: 0.0, y: (self.h - STATUS_H).max(0.0), w: self.w, h: STATUS_H }
    }

    /// Whole rows that fit in the row area.
    pub fn rows_visible(&self) -> usize {
        (self.rows_area().h / ROW_H) as usize
    }

    pub fn row(&self, i: usize) -> Rect {
        let a = self.rows_area();
        Rect { x: 0.0, y: a.y + i as f32 * ROW_H, w: a.w, h: ROW_H }
    }

    /// Left edge of column `c` (0 Name .. 4 Modified) and its width.
    pub fn column(&self, c: usize) -> (f32, f32) {
        let fixed: f32 = self.col_w.iter().sum();
        let name_w = (self.rows_area().w - fixed).max(60.0);
        let x = name_w + self.col_w[..c.saturating_sub(1)].iter().sum::<f32>();
        if c == 0 { (0.0, name_w) } else { (x, self.col_w[c - 1]) }
    }

    /// Scrollbar thumb for `total` rows scrolled to `scroll`; `None` when everything fits.
    pub fn thumb(&self, total: usize, scroll: usize) -> Option<Rect> {
        let vis = self.rows_visible();
        if total <= vis || vis == 0 {
            return None;
        }
        let bar = self.scrollbar();
        let h = (bar.h * vis as f32 / total as f32).max(24.0).min(bar.h);
        let max_scroll = (total - vis) as f32;
        let y = bar.y + (bar.h - h) * (scroll as f32 / max_scroll).min(1.0);
        Some(Rect { x: bar.x, y, w: bar.w, h })
    }

    /// Scroll offset for a thumb whose top is dragged to `y`.
    pub fn scroll_for_thumb_y(&self, total: usize, y: f32) -> usize {
        let vis = self.rows_visible();
        let Some(t) = self.thumb(total, 0) else { return 0 };
        let bar = self.scrollbar();
        let frac = ((y - bar.y) / (bar.h - t.h).max(1.0)).clamp(0.0, 1.0);
        (frac * (total - vis) as f32).round() as usize
    }

    pub fn hit(&self, x: f32, y: f32, chips: usize) -> Hit {
        for i in 0..chips {
            if self.chip(i).contains(x, y) {
                return Hit::Chip(i);
            }
        }
        if self.cancel && self.cancel_button().contains(x, y) {
            return Hit::Cancel;
        }
        if self.basket && self.basket_chip().contains(x, y) {
            return Hit::Basket;
        }
        if self.metric && self.metric_button().contains(x, y) {
            return Hit::MetricButton;
        }
        if self.banner && self.rescan_button().contains(x, y) {
            return Hit::Rescan;
        }
        if self.filter_clear().contains(x, y) {
            return Hit::FilterClear;
        }
        if self.filter_regex().contains(x, y) {
            return Hit::FilterRegex;
        }
        if self.filter_box().contains(x, y) {
            return Hit::FilterBox;
        }
        if self.map_button().contains(x, y) {
            return Hit::MapButton;
        }
        if self.map_divider().is_some_and(|d| d.contains(x, y)) {
            return Hit::MapDivider;
        }
        if self.map().is_some_and(|m| m.contains(x, y)) {
            return Hit::Map;
        }
        if self.header().contains(x, y) {
            if let Some(c) = (1..5).find(|&c| (x - self.column(c).0).abs() <= GRIP) {
                return Hit::Divider(c);
            }
            return (0..5).find(|&c| { let (cx, cw) = self.column(c); x >= cx && x < cx + cw }).map_or(Hit::None, Hit::Header);
        }
        if self.scrollbar().contains(x, y) {
            return Hit::Scrollbar;
        }
        let a = self.rows_area();
        if a.contains(x, y) {
            let i = ((y - a.y) / ROW_H) as usize;
            if i < self.rows_visible() {
                return Hit::Row(i);
            }
        }
        Hit::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_tile_the_window() {
        let l = Layout::new(800.0, 600.0);
        assert_eq!(l.rows_area().y, PICKER_H + HEADER_H);
        assert_eq!(l.rows_area().y + l.rows_area().h + STATUS_H, 600.0);
        assert_eq!(l.rows_visible(), ((600.0 - PICKER_H - HEADER_H - STATUS_H) / ROW_H) as usize);
    }

    #[test]
    fn the_status_bar_has_two_rows_and_the_buttons_sit_in_the_first() {
        let l = Layout::new(800.0, 600.0).with_cancel(true).with_basket(true);
        let (bar, top) = (l.status(), l.status_top());
        assert_eq!((bar.h, top.h * 2.0, top.y), (STATUS_H, STATUS_H, bar.y));
        for r in [l.map_button(), l.cancel_button(), l.basket_chip()] {
            assert!(r.y >= top.y && r.y + r.h <= top.y + top.h, "{r:?} in the first row");
        }
    }

    #[test]
    fn the_metric_button_sits_left_of_the_cancel_slot_and_is_hit_only_when_shown() {
        let l = Layout::new(900.0, 600.0);
        let b = l.metric_button();
        assert!(b.x + b.w <= l.cancel_button().x, "left of the Cancel slot");
        assert_eq!((b.y, b.h), (l.map_button().y, l.map_button().h), "in the status bar's first row");
        assert_ne!(l.hit(b.x + 2.0, b.y + 2.0, 0), Hit::MetricButton, "not shown by default");
        assert_eq!(l.with_metric(true).hit(b.x + 2.0, b.y + 2.0, 0), Hit::MetricButton);
    }

    #[test]
    fn every_icon_button_is_the_same_square_inside_its_band() {
        let l = Layout::new(900.0, 600.0).with_banner(true).with_cancel(true);
        let bands = [(l.admin_button(), 0.0, PICKER_H), (l.rescan_button(), PICKER_H, PICKER_H + BANNER_H), (l.cancel_button(), l.status_top().y, l.status_top().y + STATUS_ROW_H), (l.map_button(), l.status_top().y, l.status_top().y + STATUS_ROW_H)];
        for (b, top, bottom) in bands {
            assert_eq!((b.w, b.h), (ICON_BTN, ICON_BTN));
            assert!(b.y >= top && b.y + b.h <= bottom, "{b:?} inside {top}..{bottom}");
            assert_eq!(b.y - top, bottom - (b.y + b.h), "centred");
        }
    }

    #[test]
    fn the_held_pane_takes_height_from_the_rows_only_when_shown() {
        let l = Layout::new(900.0, 600.0);
        let h = l.with_held(true);
        assert_eq!(l.rows_area().h - h.rows_area().h, HELD_H);
        let (rows, pane) = (h.rows_area(), h.held_pane());
        assert_eq!(rows.y + rows.h, pane.y, "the pane starts where the rows end");
        assert_eq!(pane.y + pane.h + STATUS_H, 600.0, "and ends at the status bar");
    }

    #[test]
    fn columns_fill_the_row_width() {
        let l = Layout::new(800.0, 600.0);
        let (x4, w4) = l.column(4);
        assert_eq!(x4 + w4, l.rows_area().w);
        assert_eq!(l.column(0).0 + l.column(0).1, l.column(1).0);
    }

    #[test]
    fn chips_follow_their_own_widths_and_the_filter_starts_after_them() {
        let l = Layout { chips: vec![60.0, 140.0, 80.0], ..Layout::new(1400.0, 600.0) };
        assert_eq!((l.chip(0).x, l.chip(0).w), (8.0, 60.0));
        assert_eq!((l.chip(1).x, l.chip(1).w), (8.0 + 60.0 + CHIP_GAP, 140.0));
        assert_eq!(l.chip(2).x, 8.0 + 60.0 + 140.0 + 2.0 * CHIP_GAP);
        assert_eq!(l.hit(l.chip(1).x + 1.0, 10.0, 3), Hit::Chip(1));
        assert_eq!(l.hit(l.chip(0).x + 61.0, 10.0, 3), Hit::None, "the gap after a narrow chip is not part of the next one");
        assert!(l.filter_box().x >= l.chip(2).x + 80.0);
    }

    #[test]
    fn hit_testing() {
        let l = Layout { chips: vec![CHIP_W; 3], ..Layout::new(800.0, 600.0) };
        assert_eq!(l.hit(10.0, 10.0, 3), Hit::Chip(0));
        assert_eq!(l.hit(10.0 + 2.0 * (CHIP_W + CHIP_GAP), 10.0, 3), Hit::Chip(2));
        assert_eq!(l.hit(10.0 + 3.0 * (CHIP_W + CHIP_GAP), 10.0, 3), Hit::None);
        assert_eq!(l.hit(5.0, PICKER_H + 5.0, 3), Hit::Header(0));
        assert_eq!(l.hit(l.column(2).0 + 20.0, PICKER_H + 5.0, 3), Hit::Header(2));
        assert_eq!(l.hit(5.0, PICKER_H + HEADER_H + 1.0, 3), Hit::Row(0));
        assert_eq!(l.hit(5.0, PICKER_H + HEADER_H + 3.0 * ROW_H + 1.0, 3), Hit::Row(3));
        assert_eq!(l.hit(795.0, 200.0, 3), Hit::Scrollbar);
        assert_eq!(l.hit(5.0, 595.0, 3), Hit::None);
    }

    #[test]
    fn dragging_a_column_edge_resizes_it_within_limits() {
        let mut l = Layout::new(800.0, 600.0);
        let y = l.header().y + 4.0;
        let edge = l.column(2).0;
        assert_eq!(l.hit(edge, y, 0), Hit::Divider(2));
        assert_eq!(l.hit(edge + 10.0, y, 0), Hit::Header(2));
        l.set_col_w(2, 200.0);
        assert_eq!(l.column(2).1, 200.0);
        assert_eq!(l.column(4).0 + l.column(4).1, l.rows_area().w, "the right edge stays put");
        l.set_col_w(2, 1.0);
        assert_eq!(l.column(2).1, MIN_COL_W);
        l.set_col_w(2, 10_000.0);
        assert!(l.column(0).1 >= MIN_NAME_W);
    }

    #[test]
    fn the_admin_button_stays_clear_of_the_system_caption_buttons() {
        let l = Layout::new(800.0, 600.0);
        let b = l.admin_button();
        assert!(b.x + b.w <= l.w - CAPTION_RESERVE);
    }

    #[test]
    fn a_banner_pushes_the_table_down_and_has_a_button() {
        let l = Layout::new(800.0, 600.0);
        let b = l.with_banner(true);
        assert_eq!(b.header().y, l.header().y + BANNER_H);
        assert_eq!(b.rows_area().y + b.rows_area().h + STATUS_H, 600.0);
        assert_eq!(b.hit(b.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);
        assert_ne!(l.hit(l.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);
        assert_eq!(b.hit(5.0, PICKER_H + BANNER_H + 5.0, 3), Hit::Header(0));
        assert!(b.rows_visible() < l.rows_visible());
    }

    #[test]
    fn the_filter_shares_the_picker_bar_and_its_controls_hit() {
        let l = Layout { chips: vec![CHIP_W; 3], ..Layout::new(1400.0, 600.0) };
        assert_eq!(l.header().y, PICKER_H);
        assert!(l.filter_box().x >= l.chip(2).x + CHIP_W, "after the chips");
        assert_eq!(l.filter_box().w, FILTER_BOX_W, "full width when there is room");
        assert!(l.filter_count().x + l.filter_count().w <= l.admin_button().x, "clear of the administrator button");
        let tight = Layout { chips: vec![CHIP_W; 2], ..Layout::new(800.0, 600.0) };
        assert!(tight.filter_box().w >= FILTER_BOX_MIN && tight.filter_box().w < FILTER_BOX_W, "the box shrinks to fit");
        assert!(tight.filter_count().x + tight.filter_count().w <= tight.admin_button().x, "and nothing overlaps");
        let (b, r, c, n) = (l.filter_box(), l.filter_regex(), l.filter_clear(), l.filter_count());
        assert!(c.x + c.w <= r.x && r.x + r.w <= b.x + b.w && b.x + b.w <= n.x && n.x + n.w <= l.w, "{b:?} {r:?} {c:?} {n:?}");
        assert!(c.x >= b.x + 8.0 + 40.0, "the buttons leave the text some room");
        assert_eq!(l.hit(b.x + b.w / 2.0, b.y + 2.0, 3), Hit::FilterBox);
        assert_eq!(l.hit(c.x + 2.0, c.y + 2.0, 3), Hit::FilterClear);
        assert_eq!(l.hit(r.x + 2.0, r.y + 2.0, 3), Hit::FilterRegex);
        assert_eq!(l.hit(n.x + 2.0, n.y + 2.0, 3), Hit::None);
        let banner = l.with_banner(true);
        assert_eq!(banner.header().y, PICKER_H + BANNER_H);
        assert_eq!(banner.hit(banner.rescan_button().x + 1.0, PICKER_H + 5.0, 3), Hit::Rescan);
    }

    #[test]
    fn thumb_tracks_scroll_and_inverts() {
        let l = Layout::new(800.0, 600.0);
        assert!(l.thumb(5, 0).is_none());
        let top = l.thumb(1000, 0).unwrap();
        let bot = l.thumb(1000, 1000 - l.rows_visible()).unwrap();
        assert_eq!(top.y, l.scrollbar().y);
        assert!((bot.y + bot.h - (l.scrollbar().y + l.scrollbar().h)).abs() < 0.01);
        for s in [0, 1, 500, 1000 - l.rows_visible()] {
            let t = l.thumb(1000, s).unwrap();
            let back = l.scroll_for_thumb_y(1000, t.y);
            assert!(back.abs_diff(s) <= 1, "{s} -> {back}");
        }
    }

    #[test]
    fn the_map_pane_sits_right_of_the_tree_and_squeezes_it() {
        let mut l = Layout::new(1200.0, 600.0);
        assert!(l.map().is_none() && l.tree_w() == 1200.0, "off by default");
        l.map_on = true;
        let (pane, div) = (l.map().unwrap(), l.map_divider().unwrap());
        assert_eq!((div.x, pane.x), (l.tree_w(), l.tree_w() + DIVIDER_W));
        assert_eq!(pane.x + pane.w, 1200.0);
        assert_eq!(pane.y + pane.h + STATUS_H, 600.0);
        assert_eq!(l.rows_area().w, l.tree_w() - SCROLLBAR_W);
        assert_eq!(l.header().w, l.tree_w());
        let y = PICKER_H + 5.0;
        assert_eq!(l.hit(5.0, y, 0), Hit::Header(0));
        assert_eq!(l.hit(div.x + 1.0, y, 0), Hit::MapDivider);
        assert_eq!(l.hit(pane.x + 5.0, pane.y + 5.0, 0), Hit::Map);
        assert_eq!(l.hit(l.tree_w() - 5.0, 200.0, 0), Hit::Scrollbar);
        let b = l.map_button();
        assert_eq!(l.hit(b.x + 2.0, b.y + 2.0, 0), Hit::MapButton);
        assert!(l.cancel_button().x + l.cancel_button().w <= b.x, "Cancel sits left of the map button");
    }

    #[test]
    fn a_narrow_window_hides_the_pane_and_the_width_stays_within_limits() {
        let mut l = Layout::new(600.0, 600.0);
        l.map_on = true;
        assert!(!l.map_shown() && l.map().is_none() && l.tree_w() == 600.0);
        let mut l = Layout::new(1000.0, 600.0);
        l.map_on = true;
        l.set_map_w(10.0);
        assert_eq!(l.map().unwrap().w, MAP_MIN_W);
        l.set_map_w(5000.0);
        assert!(l.tree_w() >= l.tree_min(), "{} < {}", l.tree_w(), l.tree_min());
        assert!(l.map().unwrap().w >= MAP_MIN_W);
    }

    #[test]
    fn the_basket_chip_sits_left_of_cancel_and_the_map_button_without_overlap() {
        let l = Layout::new(900.0, 600.0).with_basket(true);
        let chip = l.basket_chip();
        assert!(chip.x + chip.w <= l.map_button().x, "left of the map button");
        let with_cancel = l.with_cancel(true);
        assert!(with_cancel.basket_chip().x + with_cancel.basket_chip().w <= with_cancel.cancel_button().x, "left of Cancel when it shows");
        assert_eq!(l.hit(chip.x + 3.0, chip.y + 3.0, 0), Hit::Basket);
        assert_ne!(Layout::new(900.0, 600.0).hit(chip.x + 3.0, chip.y + 3.0, 0), Hit::Basket, "hidden unless something is marked");
    }
}
