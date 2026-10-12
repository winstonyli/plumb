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
pub const INDENT: f32 = 16.0;
/// Widths of the Size, % of parent and Items columns; Name takes the rest.
pub const COL_W: [f32; 3] = [100.0, 130.0, 90.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Chip(usize),
    /// Column header: 0 Name, 1 Size, 2 % of parent, 3 Items.
    Header(usize),
    /// Index into the visible window (0 is the first drawn row).
    Row(usize),
    Scrollbar,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
}

impl Layout {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }

    pub fn chip(&self, i: usize) -> Rect {
        Rect { x: 8.0 + i as f32 * (CHIP_W + CHIP_GAP), y: 6.0, w: CHIP_W, h: PICKER_H - 12.0 }
    }

    pub fn header(&self) -> Rect {
        Rect { x: 0.0, y: PICKER_H, w: self.w, h: HEADER_H }
    }

    /// Row area, excluding the header and scrollbar.
    pub fn rows_area(&self) -> Rect {
        let y = PICKER_H + HEADER_H;
        Rect { x: 0.0, y, w: (self.w - SCROLLBAR_W).max(0.0), h: (self.h - y - STATUS_H).max(0.0) }
    }

    pub fn scrollbar(&self) -> Rect {
        let a = self.rows_area();
        Rect { x: a.x + a.w, y: a.y, w: SCROLLBAR_W.min(self.w), h: a.h }
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
        let fixed: f32 = COL_W.iter().sum();
        let name_w = (self.rows_area().w - fixed).max(60.0);
        let x = name_w + COL_W[..c.saturating_sub(1)].iter().sum::<f32>();
        if c == 0 { (0.0, name_w) } else { (x, COL_W[c - 1]) }
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
        if self.header().contains(x, y) {
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
        assert_eq!(l.rows_area().y, PICKER_H + HEADER_H);
        assert_eq!(l.rows_area().y + l.rows_area().h + STATUS_H, 600.0);
        assert_eq!(l.rows_visible(), ((600.0 - 44.0 - 24.0 - 24.0) / 22.0) as usize);
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
        assert_eq!(l.hit(5.0, PICKER_H + 5.0, 3), Hit::Header(0));
        assert_eq!(l.hit(l.column(2).0 + 1.0, PICKER_H + 5.0, 3), Hit::Header(2));
        assert_eq!(l.hit(5.0, PICKER_H + HEADER_H + 1.0, 3), Hit::Row(0));
        assert_eq!(l.hit(5.0, PICKER_H + HEADER_H + 3.0 * ROW_H + 1.0, 3), Hit::Row(3));
        assert_eq!(l.hit(795.0, 200.0, 3), Hit::Scrollbar);
        assert_eq!(l.hit(5.0, 595.0, 3), Hit::None);
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
