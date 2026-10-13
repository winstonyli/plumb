//! Pure geometry in device-independent pixels (DIPs). No Win32 types, so it tests anywhere.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub const PICKER_H: f32 = 44.0;
pub const HEADER_H: f32 = 24.0;
pub const STATUS_H: f32 = 24.0;
pub const ROW_H: f32 = 22.0;
pub const SCROLLBAR_W: f32 = 12.0;
pub const CHIP_W: f32 = 104.0;
pub const CHIP_GAP: f32 = 8.0;
pub const ADMIN_W: f32 = 168.0;
/// Room kept at the right end of the picker bar for the system's minimize, maximize and close buttons,
/// which Windows draws over the top of the window (the title bar is part of the client area).
pub const CAPTION_RESERVE: f32 = 150.0;
pub const BANNER_H: f32 = 28.0;
pub const RESCAN_W: f32 = 80.0;
pub const FILTER_H: f32 = 32.0;
pub const FILTER_REGEX_W: f32 = 72.0;
pub const FILTER_COUNT_W: f32 = 150.0;
pub const FILTER_CLEAR_W: f32 = 24.0;
pub const CANCEL_W: f32 = 72.0;
pub const INDENT: f32 = 20.0;
/// Starting widths of the Size, % of parent and Items columns; Name takes the rest.
pub const COL_W: [f32; 3] = [100.0, 130.0, 90.0];
/// Narrowest a dragged column may get, and narrowest the Name column may be squeezed.
pub const MIN_COL_W: f32 = 50.0;
pub const MIN_NAME_W: f32 = 120.0;
/// Half-width of the grab zone around a column's left edge in the header.
const GRIP: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Chip(usize),
    /// Column header: 0 Name, 1 Size, 2 % of parent, 3 Items.
    Header(usize),
    /// The left edge of column 1..=3 in the header, where a drag resizes that column.
    Divider(usize),
    /// Index into the visible window (0 is the first drawn row).
    Row(usize),
    Scrollbar,
    /// The Rescan button on the stale banner.
    Rescan,
    /// The Cancel button in the status bar, shown while a scan runs.
    Cancel,
    /// The filter text box (not its clear button).
    FilterBox,
    FilterRegex,
    FilterClear,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    /// A banner strip sits between the picker and the header.
    pub banner: bool,
    /// A Cancel button sits at the right end of the status bar.
    pub cancel: bool,
    /// Widths of the Size, % of parent and Items columns.
    pub col_w: [f32; 3],
}

impl Layout {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h, banner: false, cancel: false, col_w: COL_W }
    }

    /// Sets the width of column `c` (1..=3), keeping it at least `MIN_COL_W` and the Name column at least `MIN_NAME_W`.
    pub fn set_col_w(&mut self, c: usize, w: f32) {
        let others: f32 = self.col_w.iter().sum::<f32>() - self.col_w[c - 1];
        let max = (self.rows_area().w - MIN_NAME_W - others).max(MIN_COL_W);
        self.col_w[c - 1] = w.clamp(MIN_COL_W, max);
    }

    pub fn with_cancel(self, cancel: bool) -> Self {
        Self { cancel, ..self }
    }

    pub fn with_banner(self, banner: bool) -> Self {
        Self { banner, ..self }
    }

    /// Top edge of the filter strip: below the picker, and below the banner when there is one.
    fn filter_top(&self) -> f32 {
        PICKER_H + if self.banner { BANNER_H } else { 0.0 }
    }

    /// Top edge of the header: below the filter strip.
    fn top(&self) -> f32 {
        self.filter_top() + FILTER_H
    }

    pub fn filter_strip(&self) -> Rect {
        Rect { x: 0.0, y: self.filter_top(), w: self.w, h: FILTER_H }
    }

    /// Match count or status text, right-aligned in the strip.
    pub fn filter_count(&self) -> Rect {
        Rect { x: (self.w - 8.0 - FILTER_COUNT_W).max(0.0), y: self.filter_top(), w: FILTER_COUNT_W, h: FILTER_H }
    }

    pub fn filter_regex(&self) -> Rect {
        let x = (self.filter_count().x - 8.0 - FILTER_REGEX_W).max(0.0);
        Rect { x, y: self.filter_top() + 4.0, w: FILTER_REGEX_W, h: FILTER_H - 8.0 }
    }

    /// The text box, including its clear button at the right end.
    pub fn filter_box(&self) -> Rect {
        let w = (self.filter_regex().x - 8.0 - 8.0).max(60.0);
        Rect { x: 8.0, y: self.filter_top() + 4.0, w, h: FILTER_H - 8.0 }
    }

    pub fn filter_clear(&self) -> Rect {
        let b = self.filter_box();
        Rect { x: b.x + b.w - FILTER_CLEAR_W, y: b.y, w: FILTER_CLEAR_W, h: b.h }
    }

    pub fn banner_strip(&self) -> Rect {
        Rect { x: 0.0, y: PICKER_H, w: self.w, h: BANNER_H }
    }

    /// "Rescan", right-aligned in the banner.
    pub fn rescan_button(&self) -> Rect {
        Rect { x: (self.w - 8.0 - RESCAN_W).max(0.0), y: PICKER_H + 3.0, w: RESCAN_W, h: BANNER_H - 6.0 }
    }

    pub fn chip(&self, i: usize) -> Rect {
        Rect { x: 8.0 + i as f32 * (CHIP_W + CHIP_GAP), y: 6.0, w: CHIP_W, h: PICKER_H - 12.0 }
    }

    /// "Run as administrator", right-aligned in the picker bar.
    pub fn admin_button(&self) -> Rect {
        Rect { x: (self.w - CAPTION_RESERVE - 8.0 - ADMIN_W).max(0.0), y: 6.0, w: ADMIN_W, h: PICKER_H - 12.0 }
    }

    pub fn header(&self) -> Rect {
        Rect { x: 0.0, y: self.top(), w: self.w, h: HEADER_H }
    }

    /// Row area, excluding the header and scrollbar.
    pub fn rows_area(&self) -> Rect {
        let y = self.top() + HEADER_H;
        Rect { x: 0.0, y, w: (self.w - SCROLLBAR_W).max(0.0), h: (self.h - y - STATUS_H).max(0.0) }
    }

    pub fn scrollbar(&self) -> Rect {
        let a = self.rows_area();
        Rect { x: a.x + a.w, y: a.y, w: SCROLLBAR_W.min(self.w), h: a.h }
    }

    /// "Cancel", right-aligned in the status bar.
    pub fn cancel_button(&self) -> Rect {
        let s = self.status();
        Rect { x: (s.x + s.w - 8.0 - CANCEL_W).max(0.0), y: s.y + 2.0, w: CANCEL_W, h: s.h - 4.0 }
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

    /// Left edge of column `c` (0 Name .. 3 Items) and its width.
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
        if self.banner && self.rescan_button().contains(x, y) {
            return Hit::Rescan;
        }
        if self.filter_clear().contains(x, y) {
            return Hit::FilterClear;
        }
        if self.filter_box().contains(x, y) {
            return Hit::FilterBox;
        }
        if self.filter_regex().contains(x, y) {
            return Hit::FilterRegex;
        }
        if self.header().contains(x, y) {
            if let Some(c) = (1..4).find(|&c| (x - self.column(c).0).abs() <= GRIP) {
                return Hit::Divider(c);
            }
            return (0..4).find(|&c| { let (cx, cw) = self.column(c); x >= cx && x < cx + cw }).map_or(Hit::None, Hit::Header);
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
        assert_eq!(l.rows_area().y, PICKER_H + FILTER_H + HEADER_H);
        assert_eq!(l.rows_area().y + l.rows_area().h + STATUS_H, 600.0);
        assert_eq!(l.rows_visible(), ((600.0 - 44.0 - 32.0 - 24.0 - 24.0) / 22.0) as usize);
    }

    #[test]
    fn columns_fill_the_row_width() {
        let l = Layout::new(800.0, 600.0);
        let (x3, w3) = l.column(3);
        assert_eq!(x3 + w3, l.rows_area().w);
        assert_eq!(l.column(0).0 + l.column(0).1, l.column(1).0);
    }

    #[test]
    fn hit_testing() {
        let l = Layout::new(800.0, 600.0);
        assert_eq!(l.hit(10.0, 10.0, 3), Hit::Chip(0));
        assert_eq!(l.hit(10.0 + 2.0 * (CHIP_W + CHIP_GAP), 10.0, 3), Hit::Chip(2));
        assert_eq!(l.hit(10.0 + 3.0 * (CHIP_W + CHIP_GAP), 10.0, 3), Hit::None);
        assert_eq!(l.hit(5.0, PICKER_H + FILTER_H + 5.0, 3), Hit::Header(0));
        assert_eq!(l.hit(l.column(2).0 + 20.0, PICKER_H + FILTER_H + 5.0, 3), Hit::Header(2));
        assert_eq!(l.hit(5.0, PICKER_H + FILTER_H + HEADER_H + 1.0, 3), Hit::Row(0));
        assert_eq!(l.hit(5.0, PICKER_H + FILTER_H + HEADER_H + 3.0 * ROW_H + 1.0, 3), Hit::Row(3));
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
        assert_eq!(l.column(3).0 + l.column(3).1, l.rows_area().w, "the right edge stays put");
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
        assert_eq!(b.hit(5.0, PICKER_H + BANNER_H + FILTER_H + 5.0, 3), Hit::Header(0));
        assert!(b.rows_visible() < l.rows_visible());
    }

    #[test]
    fn the_filter_strip_sits_above_the_header_and_its_controls_hit() {
        let l = Layout::new(800.0, 600.0);
        assert_eq!(l.filter_strip().y, PICKER_H);
        assert_eq!(l.header().y, PICKER_H + FILTER_H);
        let (b, r, c, n) = (l.filter_box(), l.filter_regex(), l.filter_clear(), l.filter_count());
        assert!(b.x + b.w <= r.x && r.x + r.w <= n.x && n.x + n.w <= 800.0, "{b:?} {r:?} {n:?}");
        assert_eq!(l.hit(b.x + b.w / 2.0, b.y + 2.0, 3), Hit::FilterBox);
        assert_eq!(l.hit(c.x + 2.0, c.y + 2.0, 3), Hit::FilterClear);
        assert_eq!(l.hit(r.x + 2.0, r.y + 2.0, 3), Hit::FilterRegex);
        assert_eq!(l.hit(n.x + 2.0, n.y + 2.0, 3), Hit::None);
        let banner = l.with_banner(true);
        assert_eq!(banner.filter_strip().y, PICKER_H + BANNER_H);
        assert_eq!(banner.header().y, PICKER_H + BANNER_H + FILTER_H);
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
}
