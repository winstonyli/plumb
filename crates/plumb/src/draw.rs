//! Draws the whole window with Direct2D onto a target bitmap. Reads state, owns none.
use crate::fmt;
use crate::gfx::Gfx;
use crate::layout::{Layout, Rect, CANCEL_W, FILTER_CLEAR_W, INDENT, RESCAN_W, ROW_H};
use plumb_core::rows::RowModel;
use plumb_core::{NodeFlags, Tree};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1SolidColorBrush, D2D1_DRAW_TEXT_OPTIONS_CLIP};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::core::{w, Result};

/// Banner text stops this far from the right edge, short of the Rescan button.
const RESCAN_RESERVED: f32 = RESCAN_W + 24.0;

fn rgb(hex: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 255) as f32 / 255.0,
        g: ((hex >> 8) & 255) as f32 / 255.0,
        b: (hex & 255) as f32 / 255.0,
        a: 1.0,
    }
}

fn rc(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: D2D1_COLOR_F,
    pub panel: D2D1_COLOR_F,
    pub fg: D2D1_COLOR_F,
    pub dim: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    pub bar: D2D1_COLOR_F,
    pub select: D2D1_COLOR_F,
    /// Box tint for a pattern that does not compile.
    pub error: D2D1_COLOR_F,
}

impl Theme {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                bg: rgb(0x1e1e1e), panel: rgb(0x2b2b2b), fg: rgb(0xeeeeee), dim: rgb(0x9a9a9a),
                accent: rgb(0x4c9aff), bar: rgb(0x3b5f8f), select: rgb(0x37373d), error: rgb(0x5c2b2e),
            }
        } else {
            Self {
                bg: rgb(0xffffff), panel: rgb(0xf0f0f0), fg: rgb(0x1a1a1a), dim: rgb(0x6b6b6b),
                accent: rgb(0x0b63ce), bar: rgb(0xb5d1f5), select: rgb(0xdbe8fb), error: rgb(0xfbd5d8),
            }
        }
    }
}

/// What a drive that is not on screen is holding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mark {
    #[default]
    None,
    /// A tree is kept: coming back is instant.
    Kept,
    /// A scan is running in the background.
    Scanning,
}

pub struct Chip {
    pub label: String,
    /// Fraction of the drive in use, 0..=1.
    pub used: f32,
    pub mark: Mark,
}

/// What the filter strip shows: the text box state plus the count text.
#[derive(Clone, Copy, Default)]
pub struct FilterBar<'a> {
    pub text: &'a str,
    /// Caret as a byte offset into `text`.
    pub caret: usize,
    pub regex: bool,
    pub focus: bool,
    /// The whole text is selected.
    pub all: bool,
    /// The pattern did not compile: the box is tinted.
    pub error: bool,
    /// "1,240 matches", "searching...", "invalid pattern", or empty.
    pub count: &'a str,
}

pub struct View<'a> {
    pub tree: &'a Tree,
    pub model: &'a RowModel,
    pub scroll: usize,
    /// The scan is still running, so sizes and counts are what has been found so far.
    pub partial: bool,
}

pub struct Scene<'a> {
    pub layout: Layout,
    pub theme: Theme,
    pub chips: &'a [Chip],
    pub active_chip: Option<usize>,
    /// Where to draw the "Run as administrator" button, if shown.
    pub admin: Option<Rect>,
    /// Header column the rows are sorted by, and whether descending.
    pub sort: (usize, bool),
    /// `None` while no tree exists; `message` is shown instead.
    pub view: Option<View<'a>>,
    pub message: &'a str,
    pub status: &'a str,
    /// Text of the stale banner; its strip and Rescan button come from `layout`.
    pub banner: Option<&'a str>,
    pub filter: FilterBar<'a>,
}

pub struct Painter {
    left: IDWriteTextFormat,
    right: IDWriteTextFormat,
}

impl Painter {
    pub fn new(gfx: &Gfx) -> Result<Self> {
        let make = |align| unsafe {
            let f = gfx.dw.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                w!("en-us"),
            )?;
            f.SetTextAlignment(align)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = gfx.dw.CreateEllipsisTrimmingSign(&f)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            f.SetTrimming(&trim, &sign)?;
            Ok::<_, windows::core::Error>(f)
        };
        Ok(Self { left: make(DWRITE_TEXT_ALIGNMENT_LEADING)?, right: make(DWRITE_TEXT_ALIGNMENT_TRAILING)? })
    }

    /// Draws `scene` into `target` (a Direct2D target bitmap) at `dpi`.
    pub fn paint(&self, gfx: &Gfx, target: &ID2D1Bitmap1, dpi: f32, scene: &Scene) -> Result<()> {
        let ctx = &gfx.d2d;
        unsafe {
            ctx.SetTarget(target);
            ctx.SetDpi(dpi, dpi);
            ctx.BeginDraw();
            let brush = ctx.CreateSolidColorBrush(&scene.theme.bg, None)?;
            Pen { gfx, brush, painter: self }.draw(scene);
            let r = ctx.EndDraw(None, None);
            ctx.SetTarget(None);
            r
        }
    }
}

struct Pen<'a> {
    gfx: &'a Gfx,
    brush: ID2D1SolidColorBrush,
    painter: &'a Painter,
}

impl Pen<'_> {
    fn fill(&self, r: Rect, c: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.FillRectangle(&rc(r), &self.brush);
        }
    }

    fn text(&self, s: &str, r: Rect, c: D2D1_COLOR_F, right: bool) {
        let u: Vec<u16> = s.encode_utf16().collect();
        let f = if right { &self.painter.right } else { &self.painter.left };
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.DrawText(&u, f, &rc(r), &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    fn frame(&self, r: Rect, c: D2D1_COLOR_F) {
        for e in [
            Rect { h: 1.0, ..r },
            Rect { y: r.y + r.h - 1.0, h: 1.0, ..r },
            Rect { w: 1.0, ..r },
            Rect { x: r.x + r.w - 1.0, w: 1.0, ..r },
        ] {
            self.fill(e, c);
        }
    }

    fn text_width(&self, s: &str) -> f32 {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.gfx.dw.CreateTextLayout(&u, &self.painter.left, 10_000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return 0.0;
            }
            m.widthIncludingTrailingWhitespace
        }
    }

    fn filter_bar(&self, s: &Scene) {
        let (t, l, f) = (&s.theme, &s.layout, &s.filter);
        self.fill(l.filter_strip(), t.panel);
        let b = l.filter_box();
        self.fill(b, if f.error { t.error } else { t.bg });
        self.frame(b, if f.focus { t.accent } else { t.select });
        let inner = Rect { x: b.x + 8.0, y: b.y, w: (b.w - 16.0 - FILTER_CLEAR_W).max(0.0), h: b.h };
        if f.text.is_empty() {
            if !f.focus {
                self.text("Filter by name  (Ctrl+F)", inner, t.dim, false);
            }
        } else {
            if f.all {
                let w = self.text_width(f.text).min(inner.w);
                self.fill(Rect { x: inner.x, y: b.y + 4.0, w, h: b.h - 8.0 }, t.select);
            }
            self.text(f.text, inner, t.fg, false);
            let c = l.filter_clear();
            self.text("\u{00D7}", Rect { w: c.w - 6.0, ..c }, t.dim, true);
        }
        if f.focus && !f.all {
            let x = inner.x + self.text_width(f.text.get(..f.caret).unwrap_or(f.text));
            if x <= inner.x + inner.w {
                self.fill(Rect { x, y: b.y + 5.0, w: 1.0, h: b.h - 10.0 }, t.fg);
            }
        }
        let r = l.filter_regex();
        self.fill(r, if f.regex { t.select } else { t.bg });
        self.frame(r, if f.regex { t.accent } else { t.select });
        self.text("Regex", Rect { x: r.x + 16.0, w: r.w - 16.0, ..r }, if f.regex { t.accent } else { t.dim }, false);
        self.text(f.count, l.filter_count(), if f.error { t.fg } else { t.dim }, true);
    }

    fn draw(&self, s: &Scene) {
        let t = &s.theme;
        let l = &s.layout;
        unsafe { self.gfx.d2d.Clear(Some(&t.bg)) };
        self.picker(s);
        if let Some(r) = s.admin {
            self.fill(r, t.panel);
            self.text("Run as administrator", Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, t.accent, false);
        }
        if let Some(text) = s.banner {
            let b = l.banner_strip();
            self.fill(b, t.panel);
            self.text(text, Rect { x: b.x + 8.0, y: b.y, w: b.w - RESCAN_RESERVED, h: b.h }, t.fg, false);
            let r = l.rescan_button();
            self.fill(r, t.select);
            self.text("Rescan", Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, t.accent, false);
        }
        self.filter_bar(s);
        let h = l.header();
        self.fill(h, t.panel);
        for (c, name) in ["Name", "Size", "% of parent", "Items"].iter().enumerate() {
            let (x, w) = l.column(c);
            let arrow = if s.sort.0 == c { if s.sort.1 { " \u{25BC}" } else { " \u{25B2}" } } else { "" };
            let (label, color) = (format!("{name}{arrow}"), if arrow.is_empty() { t.dim } else { t.fg });
            self.text(&label, Rect { x: x + 8.0, y: h.y, w: w - 16.0, h: h.h }, color, c == 1 || c == 3);
        }
        match &s.view {
            Some(v) => self.rows(s, v),
            None => {
                let a = l.rows_area();
                self.text(s.message, Rect { x: a.x + 16.0, y: a.y + 16.0, w: a.w - 32.0, h: ROW_H }, t.dim, false);
            }
        }
        let st = l.status();
        self.fill(st, t.panel);
        let reserved = if l.cancel { CANCEL_W + 8.0 } else { 0.0 };
        self.text(s.status, Rect { x: st.x + 8.0, y: st.y, w: (st.w - 16.0 - reserved).max(0.0), h: st.h }, t.dim, false);
        if l.cancel {
            let r = l.cancel_button();
            self.fill(r, t.select);
            self.text("Cancel", Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, t.accent, false);
        }
    }

    fn picker(&self, s: &Scene) {
        let t = &s.theme;
        for (i, chip) in s.chips.iter().enumerate() {
            let r = s.layout.chip(i);
            self.fill(r, if s.active_chip == Some(i) { t.select } else { t.panel });
            self.fill(Rect { x: r.x, y: r.y + r.h - 3.0, w: r.w * chip.used.clamp(0.0, 1.0), h: 3.0 }, t.accent);
            self.text(&chip.label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h - 3.0 }, t.fg, false);
            let dot = match chip.mark {
                Mark::None => None,
                Mark::Kept => Some(t.bar),
                Mark::Scanning => Some(t.accent),
            };
            if let Some(c) = dot {
                self.fill(Rect { x: r.x + r.w - 10.0, y: r.y + 4.0, w: 6.0, h: 6.0 }, c);
            }
        }
    }

    fn rows(&self, s: &Scene, v: &View) {
        let t = &s.theme;
        let l = &s.layout;
        for (i, row) in v.model.visible_window(v.scroll, l.rows_visible()).iter().enumerate() {
            let r = l.row(i);
            let id = row.id;
            if v.model.selected() == Some(id) {
                self.fill(r, t.select);
            }
            let parent = v.tree.parent[id];
            let denom = if parent.is_none() { v.tree.subtree_size[id] } else { v.tree.subtree_size[parent] };
            let frac = fmt::fraction(v.tree.subtree_size[id], denom);
            let fg = if row.dim { t.dim } else { t.fg };
            let dir = v.tree.flags[id].contains(NodeFlags::DIR);
            let mark = if !dir { "  " } else if v.model.is_expanded(id) { "\u{25BE} " } else { "\u{25B8} " };
            let mut name = format!("{mark}{}", v.tree.name_string(id));
            if v.tree.flags[id].contains(NodeFlags::DENIED) {
                name.push_str("  (access denied)");
            }
            let (nx, nw) = l.column(0);
            let indent = row.depth as f32 * INDENT;
            self.text(&name, Rect { x: nx + 8.0 + indent, y: r.y, w: (nw - 16.0 - indent).max(0.0), h: r.h }, fg, false);
            let (sx, sw) = l.column(1);
            let lower = if v.partial { "\u{2265} " } else { "" };
            self.text(&format!("{lower}{}", fmt::size(v.tree.subtree_size[id])), Rect { x: sx, y: r.y, w: sw - 8.0, h: r.h }, fg, true);
            let (px, pw) = l.column(2);
            self.fill(Rect { x: px + 4.0, y: r.y + 3.0, w: (pw - 8.0) * frac, h: r.h - 6.0 }, t.bar);
            self.text(&format!("{:.1}%", frac * 100.0), Rect { x: px + 8.0, y: r.y, w: pw - 16.0, h: r.h }, fg, false);
            let (ix, iw) = l.column(3);
            self.text(&format!("{lower}{}", fmt::count(v.tree.subtree_files[id] as u64)), Rect { x: ix, y: r.y, w: iw - 8.0, h: r.h }, fg, true);
        }
        if let Some(th) = l.thumb(v.model.rows().len(), v.scroll) {
            self.fill(l.scrollbar(), t.panel);
            self.fill(Rect { x: th.x + 2.0, w: th.w - 4.0, ..th }, t.dim);
        }
    }
}
