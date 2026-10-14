#![cfg(windows)]
//! Draws a fixed small tree through a WARP (software) device into an offscreen bitmap.
//! Opens no window and never touches the physical GPU.
use plumb::draw::{Chip, FilterBar, MapView, Mark, Painter, Scene, Theme, View};
use plumb::gfx::Gfx;
use plumb::layout::{Layout, CHIP_W};
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
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    unsafe { gfx.d2d.CreateBitmap(D2D_SIZE_U { width: W, height: H }, None, 0, &props).unwrap() }
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
    let layout = Layout { chips: vec![CHIP_W; chips.len()], ..Layout::new(W as f32, H as f32) };
    let found = plumb_core::filter::Query { text: "n1".into(), regex: false }.compile().unwrap();
    let admin = layout.admin_button();
    let scene = Scene {
        layout,
        theme: Theme::new(dark),
        chips: &chips,
        active_chip: Some(0),
        sort: (1, true),
        admin: Some(admin),
        hot: None,
        view: Some(View { tree: &tree, model: &model, scroll: 0, partial: true, unscanned: Some(30_000_000), hover: Some(2), found: found.as_ref() }),
        message: "",
        status: "MFT scan: 200 files",
        adapter: "WARP",
        banner: None,
        filter,
        map: None,
    };
    render_scene(&gfx, &scene, &format!("plumb-{}{}", if dark { "dark" } else { "light" }, if filter.error { "-error" } else { "" }))
}

/// Paints `scene` into an offscreen bitmap and returns its BGRA pixels; `PLUMB_SHOT_DIR=<dir>` keeps them as `<stem>.png`.
fn render_scene(gfx: &Gfx, scene: &Scene, stem: &str) -> Vec<u8> {
    let target = bitmap(gfx, D2D1_BITMAP_OPTIONS_TARGET);
    Painter::new(gfx).unwrap().paint(gfx, &target, 96.0, 0.0, scene).expect("EndDraw");
    let out = read_back(gfx, &target);
    // PLUMB_SHOT_DIR=<dir> keeps the frames as PNGs.
    if let Some(dir) = std::env::var_os("PLUMB_SHOT_DIR") {
        write_png(&std::path::Path::new(&dir).join(format!("{stem}.png")), &out);
    }
    out
}

/// The BGRA pixels of `target`.
fn read_back(gfx: &Gfx, target: &ID2D1Bitmap1) -> Vec<u8> {
    let cpu = bitmap(gfx, D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
    unsafe {
        cpu.CopyFromBitmap(None, target, None).unwrap();
        let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
        let mut out = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H as usize {
            out.extend_from_slice(std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), W as usize * 4));
        }
        cpu.Unmap().unwrap();
        out
    }
}

/// Writes `buf` (BGRA) as an uncompressed PNG, for looking at a frame by eye.
fn write_png(path: &std::path::Path, buf: &[u8]) {
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
    for y in 0..H as usize {
        raw.push(0);
        for x in 0..W as usize {
            let i = (y * W as usize + x) * 4;
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
    ihdr.extend(W.to_be_bytes());
    ihdr.extend(H.to_be_bytes());
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
    let want = [mix(t.panel.r, t.accent.r), mix(t.panel.g, t.accent.g), mix(t.panel.b, t.accent.b)];
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
        assert_eq!(pixel(&ok, x, y), col(t.panel), "box (dark={dark})");
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
        let theme = Theme::new(dark);
        let video = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 900_000).unwrap();
        let plain = tiles.iter().position(|t| t.kind == Kind::File && t.bytes == 100_000).unwrap();
        let shoot = |selected: Option<usize>, stem: &str| {
            let scene = Scene {
                layout: layout.clone(), theme, chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None,
                view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None }),
                message: "", status: "", adapter: "WARP", banner: None, filter: FilterBar::default(),
                map: Some(MapView { tiles: &tiles, generation: 1, selected, hover: None, classes: [true; 10] }),
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
    let painter = Painter::new(&gfx).unwrap();
    let frame = |generation: u64, selected: Option<usize>| {
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None,
            view: None, message: "", status: "", adapter: "WARP", banner: None, filter: FilterBar::default(),
            map: Some(MapView { tiles: &tiles, generation, selected, hover: None, classes: [true; 10] }),
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
