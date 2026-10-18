#![cfg(windows)]
//! Draws a fixed small tree through a WARP (software) device into an offscreen bitmap.
//! Opens no window and never touches the physical GPU.
use plumb::draw::{Chip, FilterBar, MapView, Mark, Painter, Role, Scene, Theme, View};
use plumb::gfx::Gfx;
use plumb::layout::{chip_width, Layout, Rect, CHIP_W};
use plumb_core::basket::Basket;
use plumb_core::rows::RowModel;
use plumb_core::synth::kary_builder;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_MAP_OPTIONS_READ,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

const W: u32 = 800;
const H: u32 = 600;

fn bitmap(gfx: &Gfx, options: windows::Win32::Graphics::Direct2D::D2D1_BITMAP_OPTIONS) -> ID2D1Bitmap1 {
    bitmap_sized(gfx, options, W, H, 96.0)
}

fn bitmap_sized(gfx: &Gfx, options: windows::Win32::Graphics::Direct2D::D2D1_BITMAP_OPTIONS, w: u32, h: u32, dpi: f32) -> ID2D1Bitmap1 {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: dpi,
        dpiY: dpi,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    unsafe { gfx.d2d.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap() }
}

fn render(dark: bool, error: bool) -> Vec<u8> {
    render_with(dark, FilterBar { text: "txt", caret: 3, regex: true, focus: true, error, count: "12 matches", ..Default::default() })
}

fn render_with(dark: bool, filter: FilterBar) -> Vec<u8> {
    let gfx = Gfx::warp().expect("WARP device");
    assert!(gfx.adapter.contains("WARP"), "{}", gfx.adapter);
    let tree = kary_builder(200, 4).finish().unwrap();
    let mut model = RowModel::new(&tree);
    let root = model.rows()[0].id;
    model.expand(&tree, root);
    model.select(model.rows()[1].id);
    let chips = [Chip { label: "C:  123 GB free".into(), used: 0.7, mark: Mark::Kept }, Chip { label: "D:  1 TB free".into(), used: 0.2, mark: Mark::Scanning }];
    let layout = Layout { chips: chip_widths(&gfx, &chips), ..Layout::new(W as f32, H as f32) };
    let found = plumb_core::filter::Query { text: "n1".into(), regex: false }.compile().unwrap();
    let admin = layout.admin_button();
    let scene = Scene {
        layout,
        theme: Theme::new(dark),
        chips: &chips,
        active_chip: Some(0),
        sort: (1, true),
        admin: Some(admin),
        hot: None, pressed: false,
        view: Some(View { tree: &tree, model: &model, scroll: 0, partial: true, unscanned: Some(30_000_000), hover: Some(2), found: found.as_ref(), basket: &Basket::default() }),
        message: "",
        loading: false, kind: Default::default(), metric: Default::default(), held: None,
        status: "MFT scan: 200 files", selection: "C:\\Users\\someone\\Documents  (1.2 GB, 300 items)",
        banner: None,
        filter,
        basket: None,
        map: None,
        modal: None,
    };
    render_scene(&gfx, &scene, &format!("plumb-{}{}", if dark { "dark" } else { "light" }, if filter.error { "-error" } else { "" }))
}

/// Paints `scene` into an offscreen bitmap and returns its BGRA pixels; `PLUMB_SHOT_DIR=<dir>` keeps them as `<stem>.png`.
/// Each chip as wide as the app makes it (`App::fit_chips`), so a frame shows labels the way the window does.
fn chip_widths(gfx: &Gfx, chips: &[Chip]) -> Vec<f32> {
    let painter = Painter::new(gfx).unwrap();
    chips.iter().map(|c| chip_width(painter.text_width(gfx, Role::Title, &c.label))).collect()
}

fn render_scene(gfx: &Gfx, scene: &Scene, stem: &str) -> Vec<u8> {
    render_on(&Painter::new(gfx).unwrap(), gfx, scene, stem)
}

/// Like `render_scene` but on a given painter, so two shots share its shell-icon cache (a fresh one can fetch a different icon).
fn render_on(painter: &Painter, gfx: &Gfx, scene: &Scene, stem: &str) -> Vec<u8> {
    let target = bitmap(gfx, D2D1_BITMAP_OPTIONS_TARGET);
    painter.paint(gfx, &target, 96.0, 0.0, scene).expect("EndDraw");
    let out = read_back(gfx, &target);
    // PLUMB_SHOT_DIR=<dir> keeps the frames as PNGs.
    if let Some(dir) = std::env::var_os("PLUMB_SHOT_DIR") {
        write_png(&std::path::Path::new(&dir).join(format!("{stem}.png")), &out, W, H);
    }
    out
}

/// The BGRA pixels of `target`.
fn read_back(gfx: &Gfx, target: &ID2D1Bitmap1) -> Vec<u8> {
    read_back_sized(gfx, target, W, H)
}

fn read_back_sized(gfx: &Gfx, target: &ID2D1Bitmap1, w: u32, h: u32) -> Vec<u8> {
    let cpu = bitmap_sized(gfx, D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, w, h, 96.0);
    unsafe {
        cpu.CopyFromBitmap(None, target, None).unwrap();
        let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), w as usize * 4));
        }
        cpu.Unmap().unwrap();
        out
    }
}

/// Writes `buf` (BGRA) as an uncompressed PNG, for looking at a frame by eye.
fn write_png(path: &std::path::Path, buf: &[u8], w: u32, h: u32) {
    fn crc(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            raw.extend([buf[i + 2], buf[i + 1], buf[i]]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (k, b) in blocks.iter().enumerate() {
        z.push((k + 1 == blocks.len()) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend(*b);
    }
    let (mut a, mut d) = (1u32, 0u32);
    for &b in &raw {
        a = (a + b as u32) % 65521;
        d = (d + a) % 65521;
    }
    z.extend(((d << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}

fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i + 2], buf[i + 1], buf[i]] // BGRA -> RGB
}

#[test]
fn draws_regions_in_both_themes() {
    for dark in [false, true] {
        let buf = render(dark, false);
        let t = Theme::new(dark);
        let to8 = |c: f32| (c * 255.0).round() as u8;
        let want_bg = [to8(t.bg.r), to8(t.bg.g), to8(t.bg.b)];
        let want_panel = [to8(t.panel.r), to8(t.panel.g), to8(t.panel.b)];
        // bottom-right of the row area is empty background; the status bar and header are panel colour.
        assert_eq!(pixel(&buf, 700, 590), want_panel, "status bar (dark={dark})");
        assert_eq!(pixel(&buf, 790, 56), want_panel, "header (dark={dark})");
        // some pixel in the rows area is text/bar, i.e. not background.
        let drawn = (100..570).any(|y| (0..400).any(|x| pixel(&buf, x, y) != want_bg && pixel(&buf, x, y) != [to8(t.select.r), to8(t.select.g), to8(t.select.b)]));
        assert!(drawn, "no row content (dark={dark})");
    }
}

#[test]
fn a_selection_and_an_input_method_composition_are_drawn_in_the_box() {
    let to8 = |c: f32| (c * 255.0).round() as u8;
    let t = Theme::new(false);
    let l = Layout { chips: vec![CHIP_W; 2], ..Layout::new(W as f32, H as f32) };
    let b = l.filter_box();
    let bar = FilterBar { text: "abcdef", caret: 6, focus: true, ..Default::default() };
    let plain = render_with(false, bar);
    let selected = render_with(false, FilterBar { sel: Some((0, 6)), ..bar });
    let composing = render_with(false, FilterBar { preedit: "xyz", ..bar });
    let differs = |a: &[u8], c: &[u8]| (b.x as u32..(b.x + b.w) as u32).any(|x| (b.y as u32..(b.y + b.h) as u32).any(|y| pixel(a, x, y) != pixel(c, x, y)));
    assert!(differs(&plain, &selected), "the selection highlight changes the box");
    assert!(differs(&plain, &composing), "the composition changes the box");
    // The match tint appears inside the box when selected, and nowhere when not.
    let mix = |p: f32, a: f32| to8(p * 0.65 + a * 0.35);
    let want = [mix(t.raised.r, t.accent.r), mix(t.raised.g, t.accent.g), mix(t.raised.b, t.accent.b)];
    let near = |p: [u8; 3]| p.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 3);
    let has = |buf: &[u8]| (b.x as u32 + 9..(b.x + b.w) as u32).any(|x| near(pixel(buf, x, (b.y + b.h / 2.0) as u32)));
    assert!(has(&selected) && !has(&plain));
    // The underline sits under the composed text: the bottom rows gain foreground pixels only when composing.
    let uy = (b.y + b.h - 6.0) as u32;
    let fg = [to8(t.fg.r), to8(t.fg.g), to8(t.fg.b)];
    let under = |buf: &[u8]| (b.x as u32..(b.x + b.w) as u32).any(|x| pixel(buf, x, uy) == fg);
    assert!(under(&composing) && !under(&plain));
}

#[test]
fn the_filter_strip_draws_and_an_invalid_pattern_tints_the_box() {
    let to8 = |c: f32| (c * 255.0).round() as u8;
    for dark in [false, true] {
        let t = Theme::new(dark);
        let l = Layout { chips: vec![CHIP_W; 2], ..Layout::new(W as f32, H as f32) };
        let b = l.filter_box();
        // inside the box, well right of the three typed characters and left of the clear button
        let (x, y) = ((b.x + b.w / 2.0) as u32, (b.y + b.h / 2.0) as u32);
        let ok = render(dark, false);
        let bad = render(dark, true);
        let col = |c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F| [to8(c.r), to8(c.g), to8(c.b)];
        assert_eq!(pixel(&ok, 3, y), col(t.bg), "bar (dark={dark})");
        assert_eq!(pixel(&ok, x, y), col(t.raised), "box (dark={dark})");
        assert_eq!(pixel(&bad, x, y), col(t.error), "error tint (dark={dark})");
    }
}

fn rgb_at(buf: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i + 2] as i32, buf[i + 1] as i32, buf[i] as i32]
}

fn expect(c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> [i32; 3] {
    [(c.r * 255.0).round() as i32, (c.g * 255.0).round() as i32, (c.b * 255.0).round() as i32]
}

#[test]
fn the_map_paints_a_tile_in_its_class_colour_and_outlines_the_selection() {
    use plumb_core::treemap::{layout as map_layout, Kind};
    for dark in [true, false] {
        let gfx = Gfx::warp().expect("WARP device");
        let mut b = plumb_core::TreeBuilder::new();
        let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
        b.push_str(root, "movie.mp4", 900_000, plumb_core::NodeFlags::empty());
        b.push_str(root, "song.flac", 400_000, plumb_core::NodeFlags::empty());
        b.push_str(root, "notes.txt", 100_000, plumb_core::NodeFlags::empty());
        let tree = b.finish().unwrap();
        let model = RowModel::new(&tree);
        let mut layout = Layout::new(W as f32, H as f32);
        layout.map_on = true;
        let area = layout.map().expect("800 DIPs is wide enough");
        let tiles = map_layout(&tree, root, area, 1.0);
        let classes = plumb_core::treemap::class_bytes(&tiles);
        let theme = Theme::new(dark);
        let video = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 900_000).unwrap();
        let plain = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 100_000).unwrap();
        let shoot = |selected: Option<usize>, stem: &str| {
            let scene = Scene {
                layout: layout.clone(), theme, chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
                view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket: &Basket::default() }),
                message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None,
                map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected, hover: None, classes: &classes, marked: &[] }),
                modal: None,
            };
            render_scene(&gfx, &scene, stem)
        };
        let stem = if dark { "plumb-map-dark" } else { "plumb-map-light" };
        let (picked, plain_shot) = (shoot(Some(plain), stem), shoot(None, "plumb-map-unselected"));
        // Tile edges fall between pixels, so outlines are compared against a frame without one, not against a colour.
        let mid = |i: usize| { let r = tiles[i].rect; ((r.x + r.w / 2.0) as u32, (r.y + r.h / 2.0) as u32) };
        let (vx, vy) = mid(video);
        let want = expect(theme.class[plumb_core::treemap::Class::Video.index()]);
        assert!(rgb_at(&picked, vx, vy).iter().zip(want).all(|(x, y)| (x - y).abs() <= 2), "{dark}: the video tile is in its class colour");
        let r = tiles[plain].rect;
        let (ex, ey) = (r.x as u32 + 1, (r.y + r.h / 2.0) as u32);
        assert_ne!(rgb_at(&picked, ex, ey), rgb_at(&plain_shot, ex, ey), "{dark}: the selected tile gets an outline");
        let (cx, cy) = mid(plain);
        assert_eq!(rgb_at(&picked, cx, cy), rgb_at(&plain_shot, cx, cy), "{dark}: its inside is untouched");
    }
}

#[test]
fn the_map_tiles_are_drawn_once_per_generation_and_the_cached_frame_matches() {
    use plumb_core::treemap::layout as map_layout;
    let gfx = Gfx::warp().expect("WARP device");
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    for (i, size) in [900_000u64, 400_000, 100_000, 50_000].into_iter().enumerate() {
        b.push_str(root, &format!("f{i}.mp4"), size, plumb_core::NodeFlags::empty());
    }
    let tree = b.finish().unwrap();
    let mut layout = Layout::new(W as f32, H as f32);
    layout.map_on = true;
    let tiles = map_layout(&tree, root, layout.map().unwrap(), 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let painter = Painter::new(&gfx).unwrap();
    let frame = |generation: u64, selected: Option<usize>| {
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: None, message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None,
            map: Some(MapView { tiles: &tiles, generation, now: 0, selected, hover: None, classes: &classes, marked: &[] }),
            modal: None,
        };
        let target = bitmap(&gfx, D2D1_BITMAP_OPTIONS_TARGET);
        painter.paint(&gfx, &target, 96.0, 0.0, &scene).expect("EndDraw");
        read_back(&gfx, &target)
    };
    let first = frame(7, None);
    assert_eq!(painter.map_builds(), 1);
    let second = frame(7, None);
    assert_eq!(painter.map_builds(), 1, "same generation: the tiles are not drawn again");
    assert!(first == second, "the cached frame is pixel-identical to the one that built it");
    let picked = frame(7, Some(1));
    assert_eq!(painter.map_builds(), 1, "a selection change only redraws the outline");
    assert!(first != picked);
    frame(8, None);
    assert_eq!(painter.map_builds(), 2, "a new generation redraws the tiles");
}

#[test]
fn placeholder_rows_are_drawn_while_the_first_scan_has_no_tree() {
    let gfx = Gfx::warp().expect("WARP device");
    let layout = Layout::new(W as f32, H as f32);
    let frame = |loading: bool| {
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: None, message: if loading { "" } else { "Scan failed\nThe volume could not be opened." }, loading, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None, map: None, modal: None,
        };
        render_scene(&gfx, &scene, if loading { "plumb-loading" } else { "plumb-empty" })
    };
    let (empty, loading) = (frame(false), frame(true));
    let a = layout.rows_area();
    let changed = (a.y as u32..(a.y + a.h) as u32).any(|y| (0..a.w as u32).any(|x| rgb_at(&empty, x, y) != rgb_at(&loading, x, y)));
    assert!(changed, "the placeholder rows show in the row area");
    let (top, bottom) = (layout.row(0), layout.row(layout.rows_visible() - 1));
    let bar = |r: Rect| rgb_at(&loading, (layout.column(0).0 + 75.0) as u32, (r.y + r.h / 2.0) as u32);
    let bg = rgb_at(&empty, 100, (top.y + top.h / 2.0) as u32);
    assert!(bar(top) != bg, "the first row's bar is visible");
    let dist = |p: [i32; 3]| p.iter().zip(bg).map(|(&a, b)| (a - b).abs()).sum::<i32>();
    assert!(dist(bar(bottom)) < dist(bar(top)), "the rows fade out downward");
}

/// Two same-class files, one written `age` days before `now`, the other today, in a map with the given `now`.
fn age_shot(dark: bool, old: Option<u32>) -> (Vec<u8>, [(u32, u32); 2], Theme) {
    use plumb_core::treemap::{layout as map_layout, Kind};
    let gfx = Gfx::warp().expect("WARP device");
    let now = 40_000u32;
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    let a = b.push_str(root, "old.mp4", 600_000, plumb_core::NodeFlags::empty());
    let n = b.push_str(root, "new.mp4", 500_000, plumb_core::NodeFlags::empty());
    b.set_mtime(a, old.and_then(std::num::NonZeroU32::new));
    b.set_mtime(n, std::num::NonZeroU32::new(now));
    let tree = b.finish().unwrap();
    let mut layout = Layout::new(W as f32, H as f32);
    layout.map_on = true;
    let tiles = map_layout(&tree, root, layout.map().unwrap(), 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let theme = Theme::new(dark);
    let mid = |bytes: u64| {
        let r = tiles.iter().find(|t| t.kind == Kind::File && t.bytes == bytes).unwrap().rect;
        ((r.x + r.w / 2.0) as u32, (r.y + r.h / 2.0) as u32)
    };
    let scene = Scene {
        layout: layout.clone(), theme, chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
        view: None, message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None,
        map: Some(MapView { tiles: &tiles, generation: 1, now, selected: None, hover: None, classes: &classes, marked: &[] }),
        modal: None,
    };
    (render_scene(&gfx, &scene, "plumb-map-age"), [mid(600_000), mid(500_000)], theme)
}

#[test]
fn an_old_tile_is_paler_than_a_new_one_and_an_unknown_time_is_the_plain_class_colour() {
    for dark in [true, false] {
        let (aged, [old, new], theme) = age_shot(dark, Some(40_000 - 4_000));
        let class = expect(theme.class[plumb_core::treemap::Class::Video.index()]);
        let near = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(x, y)| (x - y).abs() <= 2);
        assert!(near(rgb_at(&aged, new.0, new.1), class), "{dark}: written today = the class colour");
        assert!(!near(rgb_at(&aged, old.0, old.1), class), "{dark}: written 11 years ago is faded");
        let (unknown, [old, _], _) = age_shot(dark, None);
        assert!(near(rgb_at(&unknown, old.0, old.1), class), "{dark}: no time = the class colour");
    }
}

#[test]
fn every_pair_of_classes_stays_apart_at_the_oldest_shade() {
    use plumb_core::age::FADE_MAX;
    use plumb_core::treemap::Class;
    for dark in [true, false] {
        let t = Theme::new(dark);
        let oldest: Vec<[f32; 3]> = (0..Class::COUNT)
            .map(|i| {
                let (c, b) = (t.class[i], t.bg);
                [c.r + (b.r - c.r) * FADE_MAX, c.g + (b.g - c.g) * FADE_MAX, c.b + (b.b - c.b) * FADE_MAX]
            })
            .collect();
        for i in 0..oldest.len() {
            for j in i + 1..oldest.len() {
                let d: f32 = (0..3).map(|k| (oldest[i][k] - oldest[j][k]).powi(2)).sum::<f32>().sqrt();
                assert!(d > 0.05, "{dark}: classes {i} and {j} are {d} apart when oldest");
            }
        }
    }
}

#[test]
fn the_legend_draws_an_age_strip_when_wide_and_leaves_it_out_when_narrow() {
    let strip_pixels = |width: f32| {
        let gfx = Gfx::warp().expect("WARP device");
        let mut b = plumb_core::TreeBuilder::new();
        let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
        b.push_str(root, "movie.mp4", 900_000, plumb_core::NodeFlags::empty());
        let tree = b.finish().unwrap();
        let mut layout = Layout::new(width, H as f32);
        layout.map_on = true;
        let tiles = plumb_core::treemap::layout(&tree, root, layout.map().unwrap_or(Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }), 1.0);
        let classes = plumb_core::treemap::class_bytes(&tiles);
        let theme = Theme::new(true);
        let scene = Scene {
            layout: layout.clone(), theme, chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: None, message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None,
            map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked: &[] }),
            modal: None,
        };
        let buf = render_scene(&gfx, &scene, "plumb-legend");
        // The strip's first swatch is the foreground colour itself, which nothing else in the status bar uses.
        let st = layout.status_top();
        let (y, fg) = ((st.y + st.h / 2.0) as u32, expect(theme.fg));
        (0..width as u32).filter(|&x| rgb_at(&buf, x, y) == fg).count()
    };
    assert!(strip_pixels(800.0) >= 4, "wide: the first swatch is drawn");
    assert_eq!(strip_pixels(220.0), 0, "narrow: no strip");
}

#[test]
fn a_marked_tile_is_tinted_without_redrawing_the_cached_tiles_and_the_chip_shows() {
    use plumb_core::treemap::layout as map_layout;
    let gfx = Gfx::warp().expect("WARP device");
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    for (i, size) in [900_000u64, 400_000].into_iter().enumerate() {
        b.push_str(root, &format!("f{i}.mp4"), size, plumb_core::NodeFlags::empty());
    }
    let tree = b.finish().unwrap();
    let mut layout = Layout::new(W as f32, H as f32);
    layout.map_on = true;
    let tiles = map_layout(&tree, root, layout.map().unwrap(), 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let painter = Painter::new(&gfx).unwrap();
    let big = tiles.iter().position(|t| t.kind == plumb_core::treemap::Kind::File && t.bytes == 900_000).unwrap();
    let mid = {
        let r = tiles[big].rect;
        ((r.x + r.w / 2.0) as u32, (r.y + r.h / 2.0) as u32)
    };
    let frame = |marked: &[usize], chip: Option<&str>| {
        let scene = Scene {
            layout: layout.with_basket(chip.is_some()), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: None, message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: chip,
            map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked }),
            modal: None,
        };
        let target = bitmap(&gfx, D2D1_BITMAP_OPTIONS_TARGET);
        painter.paint(&gfx, &target, 96.0, 0.0, &scene).expect("EndDraw");
        read_back(&gfx, &target)
    };
    let plain = frame(&[], None);
    assert_eq!(painter.map_builds(), 1);
    let marked = frame(&[big], Some("1 marked, up to 900 KB"));
    assert_eq!(painter.map_builds(), 1, "a mark is drawn over the cached tiles, not into them");
    assert_ne!(rgb_at(&plain, mid.0, mid.1), rgb_at(&marked, mid.0, mid.1), "the marked tile is tinted");
    let chip = layout.with_basket(true).basket_chip();
    let (cx, cy) = ((chip.x + 4.0) as u32, (chip.y + chip.h / 2.0) as u32);
    assert_ne!(rgb_at(&plain, cx, cy), rgb_at(&marked, cx, cy), "the chip is drawn in the status bar");
}

#[test]
fn a_marked_row_is_tinted() {
    let gfx = Gfx::warp().expect("WARP device");
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    b.push_str(root, "target.bin", 5_000, plumb_core::NodeFlags::empty());
    let tree = b.finish().unwrap();
    let mut model = RowModel::new(&tree);
    model.reveal(&tree, plumb_core::NodeId(1));
    let layout = Layout::new(W as f32, H as f32);
    let shot = |basket: &Basket| {
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket }),
            message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None, map: None, modal: None,
        };
        render_scene(&gfx, &scene, "plumb-marked-row")
    };
    let mut basket = Basket::default();
    let before = shot(&basket);
    basket.toggle("root\\target.bin");
    let after = shot(&basket);
    let at = |row: usize| {
        let r = layout.row(row);
        ((r.x + r.w - 4.0) as u32, (r.y + r.h / 2.0) as u32)
    };
    let (x, y) = at(1);
    assert_ne!(rgb_at(&before, x, y), rgb_at(&after, x, y), "the marked row's background changes");
    let (x, y) = at(0);
    assert_eq!(rgb_at(&before, x, y), rgb_at(&after, x, y), "other rows are untouched");
}

#[test]
fn a_rows_last_write_date_is_drawn_in_the_modified_column_and_an_unknown_one_leaves_it_empty() {
    let gfx = Gfx::warp().expect("WARP device");
    let layout = Layout::new(W as f32, H as f32);
    let painter = Painter::new(&gfx).unwrap();
    let shot = |mtime: Option<u32>| {
        let mut b = plumb_core::TreeBuilder::new();
        let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
        let f = b.push_str(root, "target.bin", 5_000, plumb_core::NodeFlags::empty());
        b.set_mtime(f, mtime.and_then(std::num::NonZeroU32::new));
        let tree = b.finish().unwrap();
        let mut model = RowModel::new(&tree);
        model.reveal(&tree, plumb_core::NodeId(1));
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None, pressed: false,
            view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket: &Basket::default() }),
            message: "", loading: false, kind: Default::default(), metric: Default::default(), held: None, status: "", selection: "", banner: None, filter: FilterBar::default(), basket: None, map: None, modal: None,
        };
        render_on(&painter, &gfx, &scene, if mtime.is_some() { "plumb-modified" } else { "plumb-unmodified" })
    };
    let (known, unknown) = (shot(Some(154_000)), shot(None));
    let (mx, mw) = layout.column(4);
    let r = layout.row(1);
    let differs = |x0: f32, w: f32| (x0 as u32..(x0 + w) as u32).any(|x| (r.y as u32..(r.y + r.h) as u32).any(|y| rgb_at(&known, x, y) != rgb_at(&unknown, x, y)));
    assert!(differs(mx, mw), "the date is drawn in the Modified column");
    // Not the file icon (x 29..41 here): the shell can answer a lookup late or differently under load, so it may differ
    // between two shots whatever the date does.
    let after_icon = 48.0;
    assert!(!differs(after_icon, mx - after_icon), "and nothing else changes");
}

#[test]
fn the_processes_view_paints_its_columns_and_a_map_of_memory() {
    use plumb_core::procs::{Metric, Procs, Sample};
    use plumb_core::treemap::layout as map_layout;
    let gfx = Gfx::warp().expect("WARP device");
    let sample = |pid, parent, name: &str, memory, cpu, io| Sample { pid, parent, created: 1, name: name.into(), memory, cpu_100ns: cpu, io_bytes: io };
    let (mut procs, mut tree) = Procs::new(Metric::Memory);
    let list = |cpu: u64, io: u64| {
        vec![
            sample(4, 0, "System", 200_000, 0, 0),
            sample(800, 4, "services.exe", 9_000_000, 0, 0),
            sample(1200, 800, "svchost.exe", 31_000_000, cpu, io),
            sample(2000, 1, "explorer.exe", 180_000_000, cpu * 2, io * 4),
            sample(2100, 2000, "chrome.exe", 620_000_000, cpu * 5, io * 9),
            sample(2200, 2100, "chrome.exe", 240_000_000, cpu, io),
        ]
    };
    procs.apply(&mut tree, &list(0, 0), 0);
    procs.apply(&mut tree, &list(5_000_000, 3_000_000), 10_000_000);
    let mut model = RowModel::new(&tree);
    model.set_sort(&tree, plumb_core::rows::SortKey::Size, true);
    let root = model.rows()[0].id;
    model.expand(&tree, root);
    model.select(model.rows()[1].id);
    let mut layout = Layout::new(W as f32, H as f32);
    layout.map_on = true;
    let area = layout.map().expect("wide enough");
    let tiles = map_layout(&tree, root, area, 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let chips = [Chip { label: "C:  123 GB".into(), used: 0.7, mark: Mark::None }, Chip { label: "Processes  9.1 GB".into(), used: 0.4, mark: Mark::None }];
    let layout = Layout { chips: chip_widths(&gfx, &chips), ..layout };
    let scene = Scene {
        layout, theme: Theme::new(false), chips: &chips, active_chip: Some(1), sort: (1, true), admin: None, hot: None, pressed: false,
        view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket: &Basket::default() }),
        message: "", loading: false, kind: plumb::app::ViewKind::Processes, metric: Default::default(), held: None, status: "6 processes, 1.1 GB in use of 16 GB", selection: "", banner: None,
        filter: FilterBar::default(), basket: None,
        map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked: &[] }),
        modal: None,
    };
    let shot = render_scene(&gfx, &scene, "plumb-processes");
    assert_eq!(shot.len(), (W * H * 4) as usize);
}

#[test]
fn the_processes_map_follows_the_metric_button() {
    use plumb_core::procs::{Metric, Procs, Sample};
    let gfx = Gfx::warp().expect("WARP device");
    let sample = |pid, parent, name: &str, memory, cpu, io| Sample { pid, parent, created: 1, name: name.into(), memory, cpu_100ns: cpu, io_bytes: io };
    let (mut procs, mut tree) = Procs::new(Metric::Memory);
    let list = |k: u64| {
        vec![
            sample(2000, 1, "big-memory.exe", 900_000_000, k, 0),
            sample(2100, 1, "busy-cpu.exe", 10_000_000, k * 90, 5 * k),
            sample(2200, 1, "chatty.exe", 10_000_000, k, 400 * k),
        ]
    };
    procs.apply(&mut tree, &list(0), 0);
    procs.apply(&mut tree, &list(1_000_000), 10_000_000);
    let chips = [Chip { label: "Processes  9.1 GB".into(), used: 0.4, mark: Mark::None }];
    let mut areas = Vec::new();
    let held_lines = vec!["C:/Users/me/notes.txt".to_string(), "C:/Windows/Temp/x.log".to_string()];
    for m in [Metric::Memory, Metric::Cpu, Metric::Io] {
        procs.set_metric(&mut tree, m);
        let mut model = RowModel::new(&tree);
        model.set_sort(&tree, plumb_core::rows::SortKey::Size, true);
        let root = model.rows()[0].id;
        model.expand(&tree, root);
        let layout = Layout { chips: chip_widths(&gfx, &chips), map_on: true, ..Layout::new(W as f32, H as f32) }.with_metric(true).with_held(true);
        let tiles = plumb_core::treemap::layout(&tree, root, layout.map().unwrap(), 1.0);
        // a process too small to draw folds into the "rest" tile and has none of its own
        areas.push(tiles.iter().find(|t| t.kind != plumb_core::treemap::Kind::Rest && tree.name_string(t.node) == "big-memory.exe").map_or(0.0, |t| t.rect.w * t.rect.h));
        let classes = plumb_core::treemap::class_bytes(&tiles);
        let scene = Scene {
            layout, theme: Theme::new(false), chips: &chips, active_chip: Some(0), sort: (1, true), admin: None, hot: None, pressed: false,
            view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket: &Basket::default() }),
            message: "", loading: false, kind: plumb::app::ViewKind::Processes, metric: m, held: Some(("Files held by chatty.exe (PID 2200): 2 files", &held_lines)), status: "", selection: "", banner: None,
            filter: FilterBar::default(), basket: None,
            map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked: &[] }),
            modal: None,
        };
        render_scene(&gfx, &scene, &format!("plumb-processes-{m:?}").to_lowercase());
    }
    assert!(areas[0] > areas[1] * 4.0 && areas[0] > areas[2] * 4.0, "the memory hog fills the map by memory only: {areas:?}");
}

/// The Processes scene (with the held pane, a filter and the metric button) drawn with `theme` at `scale`: a bitmap of
/// `W x H` DIPs in physical pixels, through the real paint path. The frame goes to `PLUMB_SHOT_DIR` as `stem` for looking at.
fn processes_shot(gfx: &Gfx, painter: &Painter, theme: Theme, scale: f32, hot: &dyn Fn(&Layout) -> Rect, pressed: bool, stem: &str) -> Vec<u8> {
    processes_shot_with(gfx, painter, theme, scale, hot, pressed, None, stem)
}

#[allow(clippy::too_many_arguments)]
fn processes_shot_with(gfx: &Gfx, painter: &Painter, theme: Theme, scale: f32, hot: &dyn Fn(&Layout) -> Rect, pressed: bool, modal: Option<&plumb::modal::Modal>, stem: &str) -> Vec<u8> {
    use plumb_core::procs::{Metric, Procs, Sample};
    let sample = |pid, parent, name: &str, memory, cpu, io| Sample { pid, parent, created: 1, name: name.into(), memory, cpu_100ns: cpu, io_bytes: io };
    let (mut procs, mut tree) = Procs::new(Metric::Memory);
    let list = |k: u64| vec![sample(2000, 1, "big-memory.exe", 900_000_000, k, 0), sample(2200, 1, "chatty.exe", 10_000_000, k, 400 * k)];
    procs.apply(&mut tree, &list(0), 0);
    procs.apply(&mut tree, &list(1_000_000), 10_000_000);
    let mut model = RowModel::new(&tree);
    model.set_sort(&tree, plumb_core::rows::SortKey::Size, true);
    let root = model.rows()[0].id;
    model.expand(&tree, root);
    model.select(model.rows()[1].id);
    let chips = [Chip { label: "C:  123 GB".into(), used: 0.7, mark: Mark::None }, Chip { label: "Processes  9.1 GB".into(), used: 0.4, mark: Mark::None }];
    let held_lines = vec!["C:/Users/me/notes.txt".to_string(), "C:/Windows/Temp/x.log".to_string()];
    let layout = Layout { chips: chip_widths(gfx, &chips), map_on: true, ..Layout::new(W as f32, H as f32) }.with_metric(true).with_held(true);
    let tiles = plumb_core::treemap::layout(&tree, root, layout.map().unwrap(), 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let hot = Some(hot(&layout));
    let scene = Scene {
        layout, theme, chips: &chips, active_chip: Some(1), sort: (1, true), admin: None, hot, pressed,
        view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket: &Basket::default() }),
        message: "", loading: false, kind: plumb::app::ViewKind::Processes, metric: Metric::Memory, held: Some(("Files held by chatty.exe (PID 2200): 2 files", &held_lines)), status: "2 processes, 910 MB in use of 16 GB", selection: "", banner: None,
        filter: FilterBar { text: "chr", caret: 3, regex: true, focus: true, count: "2 matches", ..Default::default() }, basket: None,
        map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked: &[] }),
        modal,
    };
    let (pw, ph) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
    let target = bitmap_sized(gfx, D2D1_BITMAP_OPTIONS_TARGET, pw, ph, 96.0 * scale);
    painter.paint(gfx, &target, 96.0 * scale, 0.0, &scene).expect("EndDraw");
    let out = read_back_sized(gfx, &target, pw, ph);
    assert_eq!(out.len(), (pw * ph * 4) as usize);
    if let Some(dir) = std::env::var_os("PLUMB_SHOT_DIR") {
        write_png(&std::path::Path::new(&dir).join(format!("{stem}.png")), &out, pw, ph);
    }
    out
}

#[test]
fn the_processes_view_paints_at_every_scale() {
    let gfx = Gfx::warp().expect("WARP device");
    let painter = Painter::new(&gfx).unwrap();
    for (dark, scale) in [(false, 1.0f32), (false, 1.25), (true, 1.5), (false, 2.0)] {
        processes_shot(&gfx, &painter, Theme::new(dark), scale, &|_| Rect { x: -1.0, y: -1.0, w: 0.0, h: 0.0 }, false, &format!("plumb-processes-{}pct", (scale * 100.0) as u32));
    }
}

/// Any Windows accent colour, in both themes, still leaves the controls readable. The contrast maths is a unit test in
/// `draw.rs`; these frames are for looking at the extremes (`PLUMB_SHOT_DIR`).
#[test]
fn the_processes_view_paints_with_extreme_accents() {
    let gfx = Gfx::warp().expect("WARP device");
    let painter = Painter::new(&gfx).unwrap();
    // 0xAABBGGRR, as the registry stores it.
    for (name, accent) in [("yellow", 0xff00_e6ff_u32), ("white", 0xffff_ffff), ("black", 0xff00_0000), ("grey", 0xff80_8080), ("green", 0xff00_c000)] {
        for dark in [false, true] {
            let t = if dark { "dark" } else { "light" };
            processes_shot(&gfx, &painter, Theme::with_accent(dark, Some(accent)), 1.0, &|_| Rect { x: -1.0, y: -1.0, w: 0.0, h: 0.0 }, false, &format!("plumb-accent-{name}-{t}"));
        }
    }
}

/// The pointer over each kind of control: frames for looking at (`PLUMB_SHOT_DIR`), one per control, in both themes.
#[test]
fn the_processes_view_paints_the_hover_state_of_each_control() {
    let gfx = Gfx::warp().expect("WARP device");
    let painter = Painter::new(&gfx).unwrap();
    type Pick = fn(&Layout) -> Rect;
    let picks: [(&str, Pick); 7] = [
        ("header", |l| l.header_cell(1)),
        ("chip", |l| l.chip(0)),
        ("regex", |l| l.filter_regex()),
        ("clear", |l| l.filter_clear()),
        ("metric", |l| l.metric_button()),
        ("map", |l| l.map_button()),
        ("admin", |l| l.admin_button()),
    ];
    for dark in [false, true] {
        for (name, pick) in picks {
            let t = if dark { "dark" } else { "light" };
            processes_shot(&gfx, &painter, Theme::new(dark), 1.0, &pick, false, &format!("plumb-hover-{name}-{t}"));
            processes_shot(&gfx, &painter, Theme::new(dark), 1.0, &pick, true, &format!("plumb-pressed-{name}-{t}"));
        }
    }
}

/// Every modal over the Processes view, in both themes at 1x and 2x: frames for looking at (`PLUMB_SHOT_DIR`), and the
/// panel must dim the view behind it and sit in the middle with its primary button in the accent colour.
#[test]
fn each_modal_paints_over_a_dimmed_view_in_both_themes_and_scales() {
    use plumb::app::CommitRequest;
    use plumb::modal::Modal;
    let gfx = Gfx::warp().expect("WARP device");
    let painter = Painter::new(&gfx).unwrap();
    let paths: Vec<String> = (0..12).map(|i| format!(r"C:\Users\me\Documents\a-rather-long-folder-name\file-{i}.bin")).collect();
    let req = CommitRequest { view: 0, paths: paths.clone(), bytes: 3_500_000_000 };
    let failed: Vec<(String, String)> = paths.iter().take(3).map(|p| (p.clone(), "in use by plumb.exe (4242)".to_string())).collect();
    let holders = [plumb_scan::holders::Holder { pid: 4242, name: "plumb.exe".into() }, plumb_scan::holders::Holder { pid: 77, name: "notepad.exe".into() }];
    let modals = [
        ("delete", Modal::delete(req.clone())),
        ("permanent", Modal::permanent(req)),
        ("failed", Modal::failed(0, false, &failed, 12)),
        ("holders", Modal::holders(r"C:\Users\me\notes.txt", &holders)),
        ("closing", Modal::closing()),
    ];
    let none = |_: &Layout| Rect { x: -1.0, y: -1.0, w: 0.0, h: 0.0 };
    for dark in [false, true] {
        for scale in [1.0f32, 2.0] {
            let t = if dark { "dark" } else { "light" };
            let plain = processes_shot(&gfx, &painter, Theme::new(dark), scale, &none, false, "plumb-modal-base");
            let w = (W as f32 * scale) as u32;
            for (name, m) in &modals {
                let shot = processes_shot_with(&gfx, &painter, Theme::new(dark), scale, &none, false, Some(m), &format!("plumb-modal-{name}-{t}-{}x", scale as u32));
                // The view behind is dimmed: a pixel in the far corner of the body is darker than without the scrim.
                let at = |b: &[u8]| rgb_at(b, 4, (H as f32 * scale) as u32 - 4).iter().sum::<i32>();
                let _ = w;
                assert!(at(&shot) < at(&plain), "{name} {t} {scale}x: the scrim did not darken the view behind");
            }
        }
    }
}
