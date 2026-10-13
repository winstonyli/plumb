#![cfg(windows)]
//! Draws a fixed small tree through a WARP (software) device into an offscreen bitmap.
//! Opens no window and never touches the physical GPU.
use plumb::draw::{Chip, FilterBar, Mark, Painter, Scene, Theme, View};
use plumb::gfx::Gfx;
use plumb::layout::Layout;
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
    let gfx = Gfx::warp().expect("WARP device");
    assert!(gfx.adapter.contains("WARP"), "{}", gfx.adapter);
    let tree = kary_builder(200, 4).finish().unwrap();
    let mut model = RowModel::new(&tree);
    let root = model.rows()[0].id;
    model.expand(&tree, root);
    model.select(model.rows()[1].id);
    let chips = [Chip { label: "C:  123 GB free".into(), used: 0.7, mark: Mark::Kept }, Chip { label: "D:  1 TB free".into(), used: 0.2, mark: Mark::Scanning }];
    let scene = Scene {
        layout: Layout::new(W as f32, H as f32),
        theme: Theme::new(dark),
        chips: &chips,
        active_chip: Some(0),
        sort: (1, true),
        admin: Some(plumb::layout::Layout::new(W as f32, H as f32).admin_button()),
        view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false }),
        message: "",
        status: "MFT scan: 200 files",
        banner: None,
        filter: FilterBar { text: "txt", caret: 3, regex: true, focus: true, error, count: "12 matches", ..Default::default() },
    };
    let target = bitmap(&gfx, D2D1_BITMAP_OPTIONS_TARGET);
    Painter::new(&gfx).unwrap().paint(&gfx, &target, 96.0, &scene).expect("EndDraw");
    let cpu = bitmap(&gfx, D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
    unsafe {
        cpu.CopyFromBitmap(None, &target, None).unwrap();
        let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
        let mut out = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H as usize {
            out.extend_from_slice(std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), W as usize * 4));
        }
        cpu.Unmap().unwrap();
        // PLUMB_SHOT_DIR=<dir> keeps the frames as PNGs.
        if let Some(dir) = std::env::var_os("PLUMB_SHOT_DIR") {
            let name = format!("plumb-{}{}.png", if dark { "dark" } else { "light" }, if error { "-error" } else { "" });
            write_png(&std::path::Path::new(&dir).join(name), &out);
        }
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
        assert_eq!(pixel(&buf, 790, 88), want_panel, "header (dark={dark})");
        // some pixel in the rows area is text/bar, i.e. not background.
        let drawn = (100..570).any(|y| (0..400).any(|x| pixel(&buf, x, y) != want_bg && pixel(&buf, x, y) != [to8(t.select.r), to8(t.select.g), to8(t.select.b)]));
        assert!(drawn, "no row content (dark={dark})");
    }
}

#[test]
fn the_filter_strip_draws_and_an_invalid_pattern_tints_the_box() {
    let to8 = |c: f32| (c * 255.0).round() as u8;
    for dark in [false, true] {
        let t = Theme::new(dark);
        let l = Layout::new(W as f32, H as f32);
        let b = l.filter_box();
        // inside the box, well right of the three typed characters and left of the clear button
        let (x, y) = ((b.x + b.w / 2.0) as u32, (b.y + b.h / 2.0) as u32);
        let ok = render(dark, false);
        let bad = render(dark, true);
        let col = |c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F| [to8(c.r), to8(c.g), to8(c.b)];
        assert_eq!(pixel(&ok, 3, y), col(t.panel), "strip (dark={dark})");
        assert_eq!(pixel(&ok, x, y), col(t.bg), "box (dark={dark})");
        assert_eq!(pixel(&bad, x, y), col(t.error), "error tint (dark={dark})");
    }
}
