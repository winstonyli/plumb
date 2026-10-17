//! Draws the whole window with Direct2D onto a target bitmap. Reads state, owns none.
use crate::fmt;
use crate::gfx::Gfx;
use crate::icons;
use plumb_core::basket::Basket;
use plumb_core::age::{shade, FADE_MAX};
use plumb_core::filter::Matcher;
use crate::layout::{Layout, Rect, BASKET_W, CAPTION_RESERVE, FILTER_PAD, ICON_BTN, INDENT, METRIC_W, PICKER_H, ROW_H};
use plumb_core::treemap::{Class, Kind, Tile};
use plumb_core::rows::RowModel;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use plumb_core::{NodeFlags, Tree};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows_numerics::Vector2;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ROUNDED_RECT, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::core::{w, Result, PCWSTR};

/// Banner text stops this far from the right edge, short of the Rescan button.
const RESCAN_RESERVED: f32 = ICON_BTN + 24.0;

/// `a` moved toward `b` by `f` (0 = `a`, 1 = `b`).
fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, f: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: a.r + (b.r - a.r) * f, g: a.g + (b.g - a.g) * f, b: a.b + (b.b - a.b) * f, a: a.a }
}
/// Corner radius of the map's file tiles.
const TILE_RADIUS: f32 = 2.0;
/// Corner radius of controls: chips, buttons, the filter box.
const RADIUS: f32 = 4.0;

/// `c` with a fraction of its colour taken out toward the grey of the same brightness.
fn soften(c: D2D1_COLOR_F, f: f32) -> D2D1_COLOR_F {
    let y = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    mix(c, D2D1_COLOR_F { r: y, g: y, b: y, a: c.a }, f)
}

/// How much colour the map's tile fills give up, so they sit calmly beside the interface.
const TILE_SOFTEN: f32 = 0.18;

/// `c` as a Win32 COLORREF (`0x00BBGGRR`), for the window frame and the tooltip, which are not drawn by us.
pub fn colorref(c: D2D1_COLOR_F) -> u32 {
    let b = |v: f32| (v * 255.0).round() as u32;
    b(c.b) << 16 | b(c.g) << 8 | b(c.r)
}

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

/// The legend's age strip: `AGE_STEPS` swatches across `AGE_STRIP` DIPs, `AGE_STRIP_GAP` clear of the type entries.
const AGE_STRIP: f32 = 48.0;
const AGE_STRIP_GAP: f32 = 12.0;
const AGE_STEPS: usize = 8;

#[derive(Clone, Copy)]
pub struct Theme {
    /// Which of the two palettes this is (a cache key: colours are not comparable).
    pub dark: bool,
    pub bg: D2D1_COLOR_F,
    /// Chrome strips: the header, the status bar, the Files held pane.
    pub panel: D2D1_COLOR_F,
    /// A control's resting surface (chips, buttons, the filter box), a step above `bg` and `panel`.
    pub raised: D2D1_COLOR_F,
    /// Lines: a control's frame, the edge of a strip, the divider.
    pub border: D2D1_COLOR_F,
    pub fg: D2D1_COLOR_F,
    pub dim: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    /// A control that is on: the accent, faint.
    pub accent_soft: D2D1_COLOR_F,
    /// Marked for deletion: the tint of rows, tiles and the basket chip.
    pub danger: D2D1_COLOR_F,
    pub bar: D2D1_COLOR_F,
    /// The faded part of a bar: how much more it could still grow while scanning.
    pub bound: D2D1_COLOR_F,
    pub select: D2D1_COLOR_F,
    /// The row under the mouse: fainter than `select`.
    pub hover: D2D1_COLOR_F,
    /// Folder icons.
    pub folder: D2D1_COLOR_F,
    /// Box tint for a pattern that does not compile.
    pub error: D2D1_COLOR_F,
    /// Tile fill per file class, indexed by `Class::index()`.
    pub class: [D2D1_COLOR_F; Class::COUNT],
}

/// Contrast ratio of two colours (WCAG): 1 for the same colour, 21 for black on white.
fn contrast(a: D2D1_COLOR_F, b: D2D1_COLOR_F) -> f32 {
    let lum = |c: D2D1_COLOR_F| {
        let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    };
    let (x, y) = (lum(a) + 0.05, lum(b) + 0.05);
    x.max(y) / x.min(y)
}

/// The colour of a field `accented` derives from the accent.
const DERIVED: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

impl Theme {
    pub fn new(dark: bool) -> Self {
        Self::with_accent(dark, None)
    }

    /// The palette with the system's accent colour (`0xRRGGBB`) in place of plumb's own blue, if there is one.
    pub fn with_accent(dark: bool, accent: Option<u32>) -> Self {
        Self::base(dark).accented(accent.map(rgb).unwrap_or(if dark { rgb(0x4c9aff) } else { rgb(0x0b63ce) }))
    }

    /// Everything that follows the accent: the accent itself (nudged toward the text colour until it reads as text
    /// on a control's surface), the on-state tint, the share bar and, in light, the selection and hover tints.
    fn accented(self, accent: D2D1_COLOR_F) -> Self {
        let mut a = accent;
        for _ in 0..24 {
            if contrast(a, self.raised) >= 4.5 {
                break;
            }
            a = mix(a, self.fg, 0.08);
        }
        let dark = self.dark;
        Self {
            accent: a,
            accent_soft: mix(self.raised, a, if dark { 0.22 } else { 0.14 }),
            bar: mix(self.bg, a, if dark { 0.5 } else { 0.3 }),
            select: mix(self.bg, a, if dark { 0.2 } else { 0.146 }),
            hover: if dark { self.hover } else { mix(self.bg, a, 0.07) },
            ..self
        }
    }

    fn base(dark: bool) -> Self {
        if dark {
            Self {
                dark: true,
                bg: rgb(0x1e1e1e), panel: rgb(0x2b2b2b), raised: rgb(0x333336), border: rgb(0x454548), fg: rgb(0xeeeeee), dim: rgb(0x9a9a9a),
                accent: DERIVED, accent_soft: DERIVED, danger: rgb(0xd14040), bar: DERIVED, bound: rgb(0xb07a2a), select: DERIVED, error: rgb(0x5c2b2e),
                hover: rgb(0x2a2a2e), folder: rgb(0xe0b050),
                class: [0xd0605e, 0xd98c3f, 0x4fae72, 0xaa8138, 0x5a8fd6, 0x8f6fd0, 0x3fb0b0, 0xb06fa8, 0x7f9fb0, 0xa5b84a, 0x9a8c78].map(|h| soften(rgb(h), TILE_SOFTEN)),
            }
        } else {
            Self {
                dark: false,
                bg: rgb(0xffffff), panel: rgb(0xf0f0f0), raised: rgb(0xf7f7f7), border: rgb(0xd0d0d0), fg: rgb(0x1a1a1a), dim: rgb(0x6b6b6b),
                accent: DERIVED, accent_soft: DERIVED, danger: rgb(0xd14040), bar: DERIVED, bound: rgb(0xf0b860), select: DERIVED, error: rgb(0xfbd5d8),
                hover: DERIVED, folder: rgb(0xd09a2a),
                class: [0xc4423f, 0xcf7a22, 0x2f9a58, 0xbaad25, 0x2f6fc0, 0x7650c0, 0x1f9696, 0x9a4f92, 0x5f8296, 0x7f9a22, 0xb0a28c].map(|h| soften(rgb(h), TILE_SOFTEN)),
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
    /// The selected byte range of `text`, if any.
    pub sel: Option<(usize, usize)>,
    /// Text an input method is composing at the caret, drawn underlined.
    pub preedit: &'a str,
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
    /// While `partial`: bytes of the drive's used space the scan has not found yet, as far as the
    /// drive's used space says (`None` once the scan has counted more than that, when it says nothing).
    pub unscanned: Option<u64>,
    /// The visible row under the mouse.
    pub hover: Option<usize>,
    /// The name filter's pattern: the part of a name it matched is highlighted.
    pub found: Option<&'a Matcher>,
    /// Marked for deletion: these rows, and the rows under a marked folder, are tinted.
    pub basket: &'a Basket,
}

/// The treemap pane's content: tiles from `plumb_core::treemap`, plus what is drawn over them.
pub struct MapView<'a> {
    pub tiles: &'a [Tile],
    /// Changes whenever `tiles` does: the tiles are drawn into a cached bitmap once per generation.
    pub generation: u64,
    /// Today as a day count since 1601; tiles are shaded by their age against it. Changes only with `generation`.
    pub now: u32,
    /// Index of the tile of the selected node, if it has one.
    pub selected: Option<usize>,
    /// Index of the tile under the pointer.
    pub hover: Option<usize>,
    /// Bytes per class among the tiles, largest first, for the legend.
    pub classes: &'a [(Class, u64)],
    /// Indices into `tiles` of tiles at or under a marked node: tinted over the cached bitmap.
    pub marked: &'a [usize],
}

pub struct Scene<'a> {
    pub layout: Layout,
    pub theme: Theme,
    pub chips: &'a [Chip],
    pub active_chip: Option<usize>,
    /// Where to draw the "Run as administrator" button, if shown.
    pub admin: Option<Rect>,
    /// The icon button under the pointer, if any.
    pub hot: Option<Rect>,
    /// Header column the rows are sorted by, and whether descending.
    pub sort: (usize, bool),
    /// Which kind of view the rows belong to: the header titles and the last two columns follow it.
    pub kind: crate::app::ViewKind,
    /// What a Processes view's first number column and map hold.
    pub metric: plumb_core::procs::Metric,
    /// The "Files held" pane's title and file lines, when it is shown.
    pub held: Option<(&'a str, &'a [String])>,
    /// `None` while no tree exists; `message` is shown instead.
    pub view: Option<View<'a>>,
    /// Shown in the empty row area (idle, failed, cancelled).
    pub message: &'a str,
    /// A first scan is running and no tree exists yet: placeholder rows instead of `message`.
    pub loading: bool,
    /// The status bar's second row: notice, scan summary, live state.
    pub status: &'a str,
    /// The status bar's first row, left: the selected row's full path and size (empty when none).
    pub selection: &'a str,
    /// Text of the stale banner; its strip and Rescan button come from `layout`.
    pub banner: Option<&'a str>,
    pub filter: FilterBar<'a>,
    /// The marked-items chip's label, or `None` for no chip.
    pub basket: Option<&'a str>,
    /// `None` when the map pane is hidden.
    pub map: Option<MapView<'a>>,
}

/// How a piece of text ranks. Sizes and weights live only in `Painter::new`; every text draw names a role.
#[derive(Clone, Copy)]
pub enum Role {
    /// Content: names, sizes, paths.
    Body,
    /// Headings: the column headers and the Files held title.
    Title,
    /// Secondary state: the status line, the match count, the legend.
    Caption,
}

/// One role's text, set from the left and from the right.
struct Face {
    left: IDWriteTextFormat,
    right: IDWriteTextFormat,
}

pub struct Painter {
    body: Face,
    title: Face,
    caption: Face,
    center: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    /// Shell icons by (extension key, folder, open, large). Device bitmaps, so
    /// they live and die with the painter.
    icons: RefCell<HashMap<(String, bool, bool, bool), ID2D1Bitmap1>>,
    /// The map's tiles, drawn once per (generation, size, scale, palette) and blitted every frame.
    map_cache: RefCell<Option<MapCache>>,
    map_builds: Cell<usize>,
}

struct MapCache {
    bitmap: ID2D1Bitmap1,
    key: (u64, u32, u32, u32, bool),
}

/// Where the map's tile area sits in physical pixels: whole-pixel origin and size, so the cached
/// bitmap blits 1:1 without blurring. Returns (x, y, w, h).
fn map_pixels(area: Rect, dpi: f32) -> (f32, f32, u32, u32) {
    let s = dpi / 96.0;
    ((area.x * s).round(), (area.y * s).round(), (area.w * s).ceil().max(1.0) as u32, (area.h * s).ceil().max(1.0) as u32)
}

const ICON_CACHE_MAX: usize = 512;

impl Painter {
    pub fn new(gfx: &Gfx) -> Result<Self> {
        let make = |family: PCWSTR, size, weight: DWRITE_FONT_WEIGHT, align| unsafe {
            let f = gfx.dw.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
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
        let ui = w!("Segoe UI");
        let normal = DWRITE_FONT_WEIGHT_NORMAL;
        let face = |size, weight| Ok::<_, windows::core::Error>(Face { left: make(ui, size, weight, DWRITE_TEXT_ALIGNMENT_LEADING)?, right: make(ui, size, weight, DWRITE_TEXT_ALIGNMENT_TRAILING)? });
        Ok(Self {
            body: face(13.0, normal)?,
            title: face(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            caption: face(12.0, normal)?,
            // The symbols `.*` and `\u{00D7}` are centred; they are small, so they are set larger.
            center: make(ui, 16.0, normal, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            // Windows' icon font (Segoe MDL2 Assets): one glyph per button.
            glyph: make(w!("Segoe MDL2 Assets"), 14.0, normal, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            icons: RefCell::default(),
            map_cache: RefCell::default(),
            map_builds: Cell::new(0),
        })
    }

    fn face(&self, role: Role) -> &Face {
        match role {
            Role::Body => &self.body,
            Role::Title => &self.title,
            Role::Caption => &self.caption,
        }
    }

    /// The width in DIPs of `s` set on one line as `role`.
    pub fn text_width(&self, gfx: &Gfx, role: Role, s: &str) -> f32 {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = gfx.dw.CreateTextLayout(&u, &self.face(role).left, 10_000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return 0.0;
            }
            m.widthIncludingTrailingWhitespace
        }
    }

    /// Draws `scene` into `target` (a Direct2D target bitmap) at `dpi`, `inset` DIPs down from its top
    /// (the part of a maximized window's client area that overhangs the screen).
    pub fn paint(&self, gfx: &Gfx, target: &ID2D1Bitmap1, dpi: f32, inset: f32, scene: &Scene) -> Result<()> {
        let ctx = &gfx.d2d;
        self.refresh_map_cache(gfx, scene, dpi)?;
        unsafe {
            ctx.SetTarget(target);
            ctx.SetDpi(dpi, dpi);
            ctx.BeginDraw();
            ctx.SetTransform(&windows_numerics::Matrix3x2::translation(0.0, inset));
            let brush = ctx.CreateSolidColorBrush(&scene.theme.bg, None)?;
            Pen { gfx, brush, painter: self, large: dpi >= 144.0, dpi }.draw(scene);
            ctx.SetTransform(&windows_numerics::Matrix3x2::identity());
            let r = ctx.EndDraw(None, None);
            ctx.SetTarget(None);
            r
        }
    }
}

impl Painter {
    /// How many times the map's tiles have been drawn into the cache (for tests).
    pub fn map_builds(&self) -> usize {
        self.map_builds.get()
    }

    /// Draws the map's tiles into the cached bitmap unless it already holds this generation at this size, scale and palette.
    fn refresh_map_cache(&self, gfx: &Gfx, scene: &Scene, dpi: f32) -> Result<()> {
        let (Some(m), Some(area)) = (&scene.map, scene.layout.map()) else {
            *self.map_cache.borrow_mut() = None;
            return Ok(());
        };
        let (ox, oy, w, h) = map_pixels(area, dpi);
        let key = (m.generation, w, h, dpi.to_bits(), scene.theme.dark);
        if m.tiles.is_empty() {
            *self.map_cache.borrow_mut() = None;
            return Ok(());
        }
        if self.map_cache.borrow().as_ref().is_some_and(|c| c.key == key) {
            return Ok(());
        }
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let ctx = &gfx.d2d;
        unsafe {
            let bitmap = ctx.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props)?;
            ctx.SetTarget(&bitmap);
            ctx.SetDpi(dpi, dpi);
            ctx.BeginDraw();
            ctx.Clear(None);
            let s = dpi / 96.0;
            ctx.SetTransform(&windows_numerics::Matrix3x2::translation(-ox / s, -oy / s));
            let brush = ctx.CreateSolidColorBrush(&scene.theme.bg, None)?;
            let pen = Pen { gfx, brush, painter: self, large: false, dpi };
            for tile in m.tiles {
                pen.tile(tile, &scene.theme, m.now);
            }
            ctx.SetTransform(&windows_numerics::Matrix3x2::identity());
            let r = ctx.EndDraw(None, None);
            ctx.SetTarget(None);
            r?;
            *self.map_cache.borrow_mut() = Some(MapCache { bitmap, key });
        }
        self.map_builds.set(self.map_builds.get() + 1);
        Ok(())
    }
}

struct Pen<'a> {
    gfx: &'a Gfx,
    brush: ID2D1SolidColorBrush,
    painter: &'a Painter,
    /// Fetch the 32-pixel shell icons (drawn at 16 DIPs) rather than the 16-pixel ones.
    large: bool,
    /// The scale this frame is drawn at.
    dpi: f32,
}

impl Pen<'_> {
    fn fill(&self, r: Rect, c: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.FillRectangle(&rc(r), &self.brush);
        }
    }

    /// `r` filled with rounded corners.
    fn round(&self, r: Rect, c: D2D1_COLOR_F) {
        self.round_by(r, RADIUS, c);
    }

    /// `r` filled with corners of the given radius.
    fn round_by(&self, r: Rect, radius: f32, c: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rc(r), radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    /// A one-pixel rounded frame inside `r`.
    fn outline(&self, r: Rect, c: D2D1_COLOR_F) {
        let h = Rect { x: r.x + 0.5, y: r.y + 0.5, w: r.w - 1.0, h: r.h - 1.0 };
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rc(h), radiusX: RADIUS - 0.5, radiusY: RADIUS - 0.5 }, &self.brush, 1.0, None);
        }
    }

    /// `s` shortened to fit `w` by replacing the middle with an ellipsis: a path keeps its drive and its name.
    fn fit_middle(&self, s: &str, w: f32) -> String {
        if self.text_width(s) <= w {
            return s.to_string();
        }
        let c: Vec<char> = s.chars().collect();
        let build = |k: usize| format!("{}\u{2026}{}", c[..k.div_ceil(2)].iter().collect::<String>(), c[c.len() - k / 2..].iter().collect::<String>());
        // The most characters that still fit: widths grow with the count, so bisect.
        let (mut lo, mut hi) = (0, c.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.text_width(&build(mid)) <= w { lo = mid } else { hi = mid - 1 }
        }
        build(lo)
    }

    /// Body text.
    fn text(&self, s: &str, r: Rect, c: D2D1_COLOR_F, right: bool) {
        self.text_as(Role::Body, s, r, c, right);
    }

    fn text_as(&self, role: Role, s: &str, r: Rect, c: D2D1_COLOR_F, right: bool) {
        let face = self.painter.face(role);
        self.draw_text(s, if right { &face.right } else { &face.left }, r, c);
    }

    fn draw_text(&self, s: &str, f: &IDWriteTextFormat, r: Rect, c: D2D1_COLOR_F) {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.brush.SetColor(&c);
            self.gfx.d2d.DrawText(&u, f, &rc(r), &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// Fills `r` with `c` fading out to nothing from its left edge to its right: an estimate's soft end.
    fn fade(&self, r: Rect, c: D2D1_COLOR_F) {
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: D2D1_COLOR_F { a: c.a * 0.9, ..c } },
            D2D1_GRADIENT_STOP { position: 1.0, color: D2D1_COLOR_F { a: 0.0, ..c } },
        ];
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: r.x, Y: 0.0 }, endPoint: Vector2 { X: r.x + r.w, Y: 0.0 } };
        unsafe {
            let d2d = &self.gfx.d2d;
            let Ok(stops) = ID2D1RenderTarget::CreateGradientStopCollection(d2d, &stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let Ok(brush) = d2d.CreateLinearGradientBrush(&props, None, &stops) else { return };
            d2d.FillRectangle(&rc(r), &brush);
        }
    }

    fn frame(&self, r: Rect, c: D2D1_COLOR_F) {
        self.frame_w(r, 1.0, c);
    }

    /// A frame `t` DIPs thick, kept inside `r` and no thicker than a third of its short side.
    fn frame_w(&self, r: Rect, t: f32, c: D2D1_COLOR_F) {
        let t = t.min(r.w.min(r.h) / 3.0).max(1.0);
        for e in [
            Rect { h: t, ..r },
            Rect { y: r.y + r.h - t, h: t, ..r },
            Rect { w: t, ..r },
            Rect { x: r.x + r.w - t, w: t, ..r },
        ] {
            self.fill(e, c);
        }
    }

    fn text_width(&self, s: &str) -> f32 {
        self.painter.text_width(self.gfx, Role::Body, s)
    }

    fn filter_bar(&self, s: &Scene) {
        let (t, l, f) = (&s.theme, &s.layout, &s.filter);
        let b = l.filter_box();
        self.round(b, if f.error { t.error } else { t.raised });
        self.outline(b, if f.focus { t.accent } else { t.border });
        let inner = Rect { x: b.x + FILTER_PAD, y: b.y, w: (l.filter_clear().x - b.x - FILTER_PAD).max(0.0), h: b.h };
        let head = f.text.get(..f.caret).unwrap_or(f.text);
        let composing = !f.preedit.is_empty();
        if f.text.is_empty() && !composing {
            if !f.focus {
                self.text("Filter  (Ctrl+F)", inner, t.dim, false);
            }
        } else {
            if let Some((a, e)) = f.sel {
                let (x0, x1) = (self.text_width(&f.text[..a]).min(inner.w), self.text_width(&f.text[..e]).min(inner.w));
                // The same tint as the part of a name the filter matched.
                self.fill(Rect { x: inner.x + x0, y: b.y + 4.0, w: x1 - x0, h: b.h - 8.0 }, D2D1_COLOR_F { a: 0.35, ..t.accent });
            }
            if composing {
                let tail = f.text.get(f.caret..).unwrap_or("");
                self.text(&format!("{head}{}{tail}", f.preedit), inner, t.fg, false);
                let x0 = self.text_width(head).min(inner.w);
                let x1 = self.text_width(&format!("{head}{}", f.preedit)).min(inner.w);
                self.fill(Rect { x: inner.x + x0, y: b.y + b.h - 6.0, w: x1 - x0, h: 1.0 }, t.fg);
            } else {
                self.text(f.text, inner, t.fg, false);
            }
            if !f.text.is_empty() {
                let c = l.filter_clear();
                let lit = self.control(c, s, false, true);
                self.symbol("\u{00D7}", &self.painter.center, c, lit);
            }
        }
        if f.focus && f.sel.is_none() {
            let x = inner.x + self.text_width(&format!("{head}{}", f.preedit));
            if x <= inner.x + inner.w {
                self.fill(Rect { x, y: b.y + 5.0, w: 1.0, h: b.h - 10.0 }, t.fg);
            }
        }
        let r = l.filter_regex();
        let c = self.control(r, s, f.regex, true);
        self.symbol(".*", &self.painter.center, r, c);
        self.text_as(Role::Caption, f.count, l.filter_count(), if f.error { t.fg } else { t.dim }, true);
    }

    fn draw(&self, s: &Scene) {
        let t = &s.theme;
        let l = &s.layout;
        unsafe { self.gfx.d2d.Clear(Some(&t.bg)) };
        self.picker(s);
        // The system's caption buttons are drawn by DWM beneath the window: leave their corner transparent.
        let corner = Rect { x: l.w - CAPTION_RESERVE, y: 0.0, w: CAPTION_RESERVE, h: PICKER_H };
        unsafe {
            self.gfx.d2d.PushAxisAlignedClip(&rc(corner), D2D1_ANTIALIAS_MODE_ALIASED);
            self.gfx.d2d.Clear(Some(&D2D1_COLOR_F::default()));
            self.gfx.d2d.PopAxisAlignedClip();
        }
        if let Some(r) = s.admin {
            self.button(r, "\u{E83D}", s); // shield
        }
        if let Some(text) = s.banner {
            let b = l.banner_strip();
            self.fill(b, t.panel);
            self.text(text, Rect { x: b.x + 8.0, y: b.y, w: b.w - RESCAN_RESERVED, h: b.h }, t.fg, false);
            let r = l.rescan_button();
            self.button(r, "\u{E72C}", s); // refresh
        }
        self.filter_bar(s);
        let h = l.header();
        self.fill(h, t.panel);
        let partial = s.view.as_ref().is_some_and(|v| v.partial);
        for (c, name) in s.kind.titles(s.metric).iter().enumerate() {
            let (x, w) = l.column(c);
            let arrow = if s.sort.0 == c { if s.sort.1 { " \u{25BE}" } else { " \u{25B4}" } } else { "" };
            // During a scan the sums are what has been found so far: "at least", said once here.
            let atleast = if partial && (c == 1 || (c == 3 && s.kind == crate::app::ViewKind::Disk)) { "\u{2265} " } else { "" };
            let (label, color) = (format!("{atleast}{name}{arrow}"), if arrow.is_empty() { t.dim } else { t.fg });
            self.text_as(Role::Title, &label, Rect { x: x + 8.0, y: h.y, w: w - 16.0, h: h.h }, color, matches!(c, 1 | 3 | 4));
            if c > 0 {
                // The grip for resizing: a hairline at the column's left edge.
                self.fill(Rect { x, y: h.y + 5.0, w: 1.0, h: h.h - 10.0 }, D2D1_COLOR_F { a: 0.5, ..t.dim });
            }
        }
        match &s.view {
            Some(v) => self.rows(s, v),
            None if s.loading => self.skeleton(s),
            None => {
                let a = l.rows_area();
                self.text(s.message, Rect { x: a.x + 16.0, y: a.y + 16.0, w: a.w - 32.0, h: ROW_H }, t.dim, false);
            }
        }
        if let (true, Some((head, lines))) = (l.held, s.held) {
            let pane = l.held_pane();
            self.fill(pane, t.panel);
            self.fill(Rect { h: 1.0, ..pane }, t.border);
            let line = |i: usize| Rect { x: pane.x + 8.0, y: pane.y + i as f32 * ROW_H, w: (pane.w - 16.0).max(0.0), h: ROW_H };
            // The title is a band of its own, so it reads as a heading over the list.
            self.fill(Rect { y: pane.y + 1.0, h: ROW_H - 1.0, ..pane }, mix(t.panel, t.fg, 0.06));
            self.text_as(Role::Title, head, line(0), t.fg, false);
            for (i, f) in lines.iter().enumerate() {
                self.text(&self.fit_middle(f, line(i + 1).w), line(i + 1), t.dim, false);
            }
        }
        self.map(s);
        let st = l.status();
        self.fill(st, t.panel);
        self.fill(Rect { h: 1.0, ..st }, t.border);
        // Row 1: the selection, the legend and the buttons; row 2: the scan and live state, the full width.
        let row1 = l.status_top();
        let reserved = ICON_BTN + 8.0 + if l.cancel || l.metric { ICON_BTN + 4.0 } else { 0.0 } + if l.metric { METRIC_W + 4.0 } else { 0.0 } + if l.basket { BASKET_W + 4.0 } else { 0.0 };
        let right = Rect { x: row1.x + 8.0, w: (row1.w - 16.0 - reserved).max(0.0), ..row1 };
        let legend_w = self.legend(s, right, right.w * 0.6);
        let text_w = if legend_w > 0.0 { right.w - legend_w - 24.0 } else { right.w };
        let sel = self.fit_middle(s.selection, text_w.max(0.0));
        self.text(&sel, Rect { w: text_w.max(0.0), ..right }, t.dim, false);
        let row2 = Rect { y: st.y + row1.h, h: st.h - row1.h, ..st };
        self.text_as(Role::Caption, s.status, Rect { x: row2.x + 8.0, w: (row2.w - 16.0).max(0.0), ..row2 }, t.dim, false);
        if l.cancel {
            let r = l.cancel_button();
            self.button(r, "\u{E711}", s); // cancel
        }
        if let (true, Some(label)) = (l.basket, s.basket) {
            let r = l.basket_chip();
            self.round(r, t.danger);
            self.centered(label, r, rgb(0xffffff));
        }
        self.toggle_button(l.map_button(), "\u{E80A}", l.map_on, s); // grid
        if l.metric {
            let r = l.metric_button();
            let c = self.control(r, s, false, false);
            self.centered(crate::app::metric_label(s.metric), r, c);
        }
    }

    /// A control's surface, the one look chips, buttons and toggles share: raised and framed at rest, lifted with
    /// a brighter frame under the pointer, accent-tinted while on. A `ghost` control (one inside another surface)
    /// shows nothing at rest. Returns the text colour.
    fn control(&self, r: Rect, s: &Scene, active: bool, ghost: bool) -> D2D1_COLOR_F {
        let (t, hot) = (&s.theme, s.hot == Some(r));
        let (fill, line, text) = if active {
            (t.accent_soft, t.accent, t.accent)
        } else if hot {
            (mix(t.raised, t.fg, 0.10), t.dim, t.fg)
        } else {
            (t.raised, t.border, t.dim)
        };
        if !ghost || active || hot {
            self.round(r, fill);
            self.outline(r, line);
        }
        text
    }

    /// A framed icon button: quiet until the pointer is over it.
    fn button(&self, r: Rect, glyph: &str, s: &Scene) {
        self.toggle_button(r, glyph, false, s);
    }

    /// An icon button that shows the accent frame while `active`.
    fn toggle_button(&self, r: Rect, glyph: &str, active: bool, s: &Scene) {
        let c = self.control(r, s, active, false);
        self.symbol(glyph, &self.painter.glyph, r, c);
    }

    /// The map's class legend, right-aligned in `r`: each class present with its share of the drawn files, largest
    /// first, as many as fit in `max` DIPs (the smallest drop off).
    /// Returns the width used (0 when there is no map or nothing fits).
    fn legend(&self, s: &Scene, r: Rect, max: f32) -> f32 {
        let Some(m) = &s.map else { return 0.0 };
        let total: u64 = m.classes.iter().map(|&(_, b)| b).sum();
        let entries: Vec<(Class, String, f32)> = m
            .classes
            .iter()
            .map(|&(c, b)| {
                let share = b * 100 / total.max(1);
                let name = s.kind.class_label(c);
                let label = if share == 0 { format!("{name} <1%") } else { format!("{name} {share}%") };
                let w = self.painter.text_width(self.gfx, Role::Caption, &label);
                (c, label, w)
            })
            .collect();
        let (mut used, mut n) = (0.0, 0);
        for &(_, _, w) in &entries {
            if used + 14.0 + w > max {
                break;
            }
            used += 14.0 + w + 12.0;
            n += 1;
        }
        if n == 0 {
            return 0.0;
        }
        used -= 12.0;
        // The age strip, at the far right, when it fits beside the types: light to dark, new to old.
        let shift = if used + AGE_STRIP_GAP + AGE_STRIP <= max {
            let step = AGE_STRIP / AGE_STEPS as f32;
            for i in 0..AGE_STEPS {
                let c = mix(s.theme.fg, s.theme.panel, i as f32 / (AGE_STEPS - 1) as f32 * FADE_MAX);
                self.fill(Rect { x: r.x + r.w - AGE_STRIP + i as f32 * step, y: r.y + (r.h - 10.0) / 2.0, w: step, h: 10.0 }, c);
            }
            AGE_STRIP + AGE_STRIP_GAP
        } else {
            0.0
        };
        let mut x = r.x + r.w - shift - used;
        for (c, label, w) in &entries[..n] {
            let w = *w;
            self.fill(Rect { x, y: r.y + (r.h - 10.0) / 2.0, w: 10.0, h: 10.0 }, s.theme.class[c.index()]);
            self.text_as(Role::Caption, label, Rect { x: x + 14.0, w: w + 2.0, ..r }, s.theme.dim, false);
            x += 14.0 + w + 12.0;
        }
        used + shift
    }

    /// The treemap pane: tiles, then the selection and hover outlines over them.
    fn map(&self, s: &Scene) {
        let (t, l) = (&s.theme, &s.layout);
        let Some(pane) = l.map() else { return };
        let area = pane;
        if let Some(d) = l.map_divider() {
            self.fill(Rect { x: d.x + d.w / 2.0 - 0.5, w: 1.0, ..d }, t.border);
        }
        self.fill(pane, if s.view.is_some() { t.bg } else { t.panel }); // blank until there is a tree
        let Some(m) = &s.map else { return };
        unsafe { self.gfx.d2d.PushAxisAlignedClip(&rc(area), D2D1_ANTIALIAS_MODE_ALIASED) };
        if let Some(c) = self.painter.map_cache.borrow().as_ref() {
            let k = self.dpi / 96.0;
            let (ox, oy, w, h) = map_pixels(area, self.dpi);
            let dest = D2D_RECT_F { left: ox / k, top: oy / k, right: (ox + w as f32) / k, bottom: (oy + h as f32) / k };
            unsafe { self.gfx.d2d.DrawBitmap(&c.bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None) };
        }
        if let Some(h) = m.hover.and_then(|i| m.tiles.get(i)).filter(|_| m.hover != m.selected) {
            self.frame(h.rect, D2D1_COLOR_F { a: 0.85, ..t.fg });
        }
        for tile in m.marked.iter().filter_map(|&i| m.tiles.get(i)) {
            // Stripes and a light frame, not just a red tint: a tint alone reads as the Video class.
            let r = tile.rect;
            self.fill(r, D2D1_COLOR_F { a: 0.35, ..t.danger });
            let mut y = r.y + 3.0;
            while y + 2.0 < r.y + r.h {
                self.fill(Rect { y, h: 2.0, ..r }, D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.3 });
                y += 6.0;
            }
            self.frame_w(r, 2.0, D2D1_COLOR_F { a: 0.9, ..t.fg });
        }
        if let Some(sel) = m.selected.and_then(|i| m.tiles.get(i)) {
            self.frame(sel.rect, t.accent);
            if sel.rect.w >= 4.0 && sel.rect.h >= 4.0 {
                self.frame(Rect { x: sel.rect.x + 1.0, y: sel.rect.y + 1.0, w: sel.rect.w - 2.0, h: sel.rect.h - 2.0 }, t.accent);
            }
        }
        unsafe { self.gfx.d2d.PopAxisAlignedClip() };
    }

    fn tile(&self, tile: &Tile, t: &Theme, now: u32) {
        let r = tile.rect;
        match tile.kind {
            Kind::File => {
                let c = mix(t.class[tile.class.index()], t.bg, shade(now, tile.mtime) * FADE_MAX);
                if r.w >= 4.0 && r.h >= 4.0 {
                    // The tile sits inside a one-pixel gap of the pane's background, its corners rounded once it is big enough.
                    let inner = Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 };
                    if inner.w >= 8.0 && inner.h >= 8.0 { self.round_by(inner, TILE_RADIUS, c) } else { self.fill(inner, c) }
                } else {
                    self.fill(r, c);
                }
            }
            Kind::Frame => {
                self.fill(r, D2D1_COLOR_F { a: 0.10, ..t.fg });
                self.frame(r, D2D1_COLOR_F { a: 0.9, ..t.dim });
            }
            Kind::Solid => self.fill(r, D2D1_COLOR_F { a: 0.45, ..t.dim }),
            Kind::Rest => self.fill(r, D2D1_COLOR_F { a: 0.22, ..t.dim }),
        }
    }

    /// A symbol centred in `r` by its ink, not its line box: a period and a raised star, or a glyph with uneven
    /// side bearings, would sit off-centre under the font's metrics. (Words are centred by line box instead, so
    /// descenders do not pull them up.)
    fn symbol(&self, s: &str, f: &IDWriteTextFormat, r: Rect, c: D2D1_COLOR_F) {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.gfx.dw.CreateTextLayout(&u, f, r.w, r.h) else { return };
            // The overhangs say how far the ink reaches past each edge of the layout box (negative: inside it).
            let Ok(o) = layout.GetOverhangMetrics() else { return self.draw_text(s, f, r, c) };
            self.brush.SetColor(&c);
            let at = Vector2 { X: r.x - (o.right - o.left) / 2.0, Y: r.y - (o.bottom - o.top) / 2.0 };
            self.gfx.d2d.DrawTextLayout(at, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    /// `s` centred in `r`, both ways.
    fn centered(&self, s: &str, r: Rect, c: D2D1_COLOR_F) {
        self.draw_text(s, &self.painter.center, r, c);
    }

    /// The shell's icon for `name` (or for a folder) in a 16-DIP square at `x`, `y`; a small drawn
    /// folder or page when the shell has none.
    fn icon(&self, x: f32, y: f32, name: &str, dir: bool, open: bool, t: &Theme) {
        if let Some(bitmap) = self.shell_icon(name, dir, open) {
            let dest = D2D_RECT_F { left: x, top: y, right: x + 16.0, bottom: y + 16.0 };
            unsafe { self.gfx.d2d.DrawBitmap(&bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
        } else if dir {
            self.fill(Rect { x, y: y + 3.0, w: 5.0, h: 3.0 }, t.folder);
            self.fill(Rect { x, y: y + 5.0, w: 12.0, h: 8.0 }, t.folder);
        } else {
            self.frame(Rect { x: x + 1.0, y: y + 2.0, w: 10.0, h: 12.0 }, t.dim);
        }
    }

    /// A small triangle centred on `cx`, `cy`, inlaid on a folder: pointing right when closed, down when open.
    fn chevron(&self, cx: f32, cy: f32, open: bool) {
        let c = D2D1_COLOR_F { r: 0.40, g: 0.29, b: 0.07, a: 0.75 };
        for k in 0..3 {
            let (n, k) = (5.0 - 2.0 * k as f32, k as f32);
            let r = if open {
                Rect { x: cx - n / 2.0, y: cy - 1.0 + k, w: n, h: 1.0 }
            } else {
                Rect { x: cx - 1.0 + k, y: cy - n / 2.0, w: 1.0, h: n }
            };
            self.fill(r, c);
        }
    }

    fn shell_icon(&self, name: &str, dir: bool, open: bool) -> Option<ID2D1Bitmap1> {
        let key = (icons::key(name, dir), dir, open, self.large);
        let mut cache = self.painter.icons.borrow_mut();
        if let Some(b) = cache.get(&key) {
            return Some(b.clone());
        }
        // A failed lookup is not remembered: it is retried on the next paint.
        let img = icons::load(&key.0, dir, open, self.large)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let size = D2D_SIZE_U { width: img.size, height: img.size };
        let bitmap = unsafe { self.gfx.d2d.CreateBitmap(size, Some(img.bgra.as_ptr().cast()), img.size * 4, &props).ok()? };
        if cache.len() >= ICON_CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, bitmap.clone());
        Some(bitmap)
    }

    fn picker(&self, s: &Scene) {
        let t = &s.theme;
        for (i, chip) in s.chips.iter().enumerate() {
            let r = s.layout.chip(i);
            let active = s.active_chip == Some(i);
            let lit = self.control(r, s, active, false);
            // The bars sit inside the rounded frame, clear of its corners.
            let inner = Rect { x: r.x + RADIUS, w: r.w - 2.0 * RADIUS, ..r };
            self.fill(Rect { y: r.y + r.h - 4.0, w: inner.w * chip.used.clamp(0.0, 1.0), h: 3.0, ..inner }, t.accent);
            self.text(&chip.label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h - 3.0 }, if active { lit } else { t.fg }, false);
            // A strip along the top edge: neutral for a kept tree, the accent for a running scan.
            let strip = match chip.mark {
                Mark::None => None,
                Mark::Kept => Some(t.dim),
                Mark::Scanning => Some(t.accent),
            };
            if let Some(c) = strip {
                self.fill(Rect { y: r.y + 1.0, h: 3.0, ..inner }, c);
            }
        }
    }

    /// Placeholder rows in the shape of the real ones (icon, name, size, items), fading out downward: shown until
    /// the first tree arrives. The widths follow a fixed pattern of the row index.
    fn skeleton(&self, s: &Scene) {
        let (t, l) = (&s.theme, &s.layout);
        let n = l.rows_visible();
        let ((nx, nw), (sx, sw), (ix, iw)) = (l.column(0), l.column(1), l.column(3));
        for i in 0..n {
            let r = l.row(i);
            let c = D2D1_COLOR_F { a: 0.22 * (1.0 - i as f32 / n as f32), ..t.dim };
            let indent = [0.0, 1.0, 1.0, 2.0, 1.0, 0.0, 1.0, 2.0][i % 8] * INDENT;
            let (ax, mid) = (nx + 8.0 + indent, r.y + r.h / 2.0);
            self.fill(Rect { x: ax, y: mid - 7.0, w: 14.0, h: 14.0 }, c);
            let name_w = (70.0 + (i * 53 % 9) as f32 * 18.0).min((nw - 16.0 - indent - 22.0).max(0.0));
            self.fill(Rect { x: ax + 22.0, y: mid - 4.0, w: name_w, h: 8.0 }, c);
            let size_w = (40.0 + (i * 29 % 4) as f32 * 8.0).min((sw - 16.0).max(0.0));
            self.fill(Rect { x: sx + sw - 8.0 - size_w, y: mid - 4.0, w: size_w, h: 8.0 }, c);
            let items_w = 24.0_f32.min((iw - 16.0).max(0.0));
            self.fill(Rect { x: ix + iw - 8.0 - items_w, y: mid - 4.0, w: items_w, h: 8.0 }, c);
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
                self.fill(Rect { x: r.x, y: r.y, w: 3.0, h: r.h - 1.0 }, t.accent);
            } else if v.hover == Some(i) {
                self.fill(r, t.hover);
            }
            let marked = !v.basket.is_empty() && v.basket.covers(&v.tree.path(id));
            if marked {
                self.fill(r, D2D1_COLOR_F { a: 0.6, ..t.danger });
            }
            self.fill(Rect { x: r.x, y: r.y + r.h - 1.0, w: r.w, h: 1.0 }, t.panel); // hairline between rows
            let parent = v.tree.parent[id];
            let denom = if parent.is_none() { v.tree.subtree_size[id] } else { v.tree.subtree_size[parent] };
            let frac = fmt::fraction(v.tree.subtree_size[id], denom);
            let fg = if row.dim || marked { t.dim } else { t.fg };
            let side = t.dim; // the item count recedes behind the name and the size
            let dir = v.tree.flags[id].contains(NodeFlags::DIR);
            let mut name = v.tree.name_string(id).to_string();
            let found: Vec<_> = v.found.map(|m| m.find(&name).collect()).unwrap_or_default();
            if v.tree.flags[id].contains(NodeFlags::DENIED) {
                name.push_str("  (access denied)");
            }
            let (nx, nw) = l.column(0);
            let indent = row.depth as f32 * INDENT;
            let ax = nx + 8.0 + indent;
            let open = dir && v.model.is_expanded(id);
            let iy = r.y + (r.h - 16.0) / 2.0;
            self.icon(ax, iy, &name, dir && s.kind == crate::app::ViewKind::Disk, open, t);
            if dir && v.tree.has_children(id) {
                self.chevron(ax + 8.0, iy + 9.0, open);
            }
            let cell = Rect { x: ax + 22.0, y: r.y, w: (nw - 16.0 - indent - 22.0).max(0.0), h: r.h };
            for span in found {
                // A tint behind each part the filter matched, clipped to the name's cell.
                let x = cell.x + self.text_width(&name[..span.start]);
                let w = self.text_width(&name[span.clone()]).min(cell.x + cell.w - x);
                if w > 0.0 {
                    self.fill(Rect { x, y: r.y + 3.0, w, h: r.h - 6.0 }, D2D1_COLOR_F { a: 0.35, ..t.accent });
                }
            }
            self.text(&name, cell, fg, false);
            let (sx, sw) = l.column(1);
            self.text(&s.kind.size_text(s.metric, v.tree.subtree_size[id]), Rect { x: sx, y: r.y, w: sw - 8.0, h: r.h }, fg, true);
            let (px, pw) = l.column(2);
            let (bx, bw) = (px + 4.0, pw - 8.0);
            self.fill(Rect { x: bx, y: r.y + 3.0, w: bw * frac, h: r.h - 6.0 }, t.bar);
            // While scanning, the share could still grow: at most to where all the unscanned bytes land in this row.
            // (A root is already all of itself, so `high` never exceeds `frac` there and it gets no fade.)
            if let Some(rest) = v.unscanned.filter(|_| v.partial) {
                let (s, p, rest) = (v.tree.subtree_size[id] as f64, denom as f64, rest as f64);
                let high = (((s + rest) / (p + rest)) as f32).min(1.0);
                if high > frac {
                    self.fade(Rect { x: bx + bw * frac, y: r.y + 3.0, w: bw * (high - frac), h: r.h - 6.0 }, t.bound);
                }
            }
            self.text(&format!("{:.1}%", frac * 100.0), Rect { x: px + 8.0, y: r.y, w: pw - 16.0, h: r.h }, fg, false);
            let (third, fourth) = s.kind.last_two(v.tree, id);
            let (ix, iw) = l.column(3);
            self.text(&third, Rect { x: ix, y: r.y, w: iw - 8.0, h: r.h }, side, true);
            if let Some(text) = fourth {
                let (mx, mw) = l.column(4);
                self.text(&text, Rect { x: mx, y: r.y, w: mw - 8.0, h: r.h }, side, true);
            }
        }
        if let Some(th) = l.thumb(v.model.rows().len(), v.scroll) {
            self.fill(l.scrollbar(), t.panel);
            self.fill(Rect { x: th.x + 2.0, w: th.w - 4.0, ..th }, t.dim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_system_accent_reads_as_text_on_a_control_in_both_themes() {
        for dark in [false, true] {
            for hex in [0x000000, 0xffffff, 0xffb900, 0x0063b1, 0x7a7574, 0x4c9aff] {
                let t = Theme::with_accent(dark, Some(hex));
                assert!(contrast(t.accent, t.raised) >= 4.5, "{dark} {hex:06x} is {}", contrast(t.accent, t.raised));
            }
        }
    }

    #[test]
    fn a_colorref_is_blue_green_red() {
        assert_eq!(colorref(rgb(0x112233)), 0x332211);
    }

    #[test]
    fn without_a_system_accent_plumbs_own_blue_stays() {
        let t = Theme::new(false);
        assert!((t.accent.b - rgb(0x0b63ce).b).abs() < 0.02 && t.accent.r < 0.1);
        assert!(Theme::new(true).dark);
    }
}
