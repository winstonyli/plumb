#![cfg(windows)]
//! Draws a fixed small tree through a WARP (software) device into an offscreen bitmap.
//! Opens no window and never touches the physical GPU.
use plumb::draw::{Chip, Painter, Scene, Theme, View};
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

fn render(dark: bool) -> Vec<u8> {
    let gfx = Gfx::warp().expect("WARP device");
    assert!(gfx.adapter.contains("WARP"), "{}", gfx.adapter);
    let tree = kary_builder(200, 4).finish().unwrap();
    let mut model = RowModel::new(&tree);
    let root = model.rows()[0].id;
    model.expand(&tree, root);
    model.select(model.rows()[1].id);
    let chips = [Chip { label: "C:  123 GB free".into(), used: 0.7 }, Chip { label: "D:  1 TB free".into(), used: 0.2 }];
    let scene = Scene {
        layout: Layout::new(W as f32, H as f32),
        theme: Theme::new(dark),
        chips: &chips,
        active_chip: Some(0),
        sort: (1, true),
        admin: Some(plumb::layout::Layout::new(W as f32, H as f32).admin_button()),
        view: Some(View { tree: &tree, model: &model, scroll: 0 }),
        message: "",
        status: "MFT scan: 200 files",
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
        out
    }
}

fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i + 2], buf[i + 1], buf[i]] // BGRA -> RGB
}

#[test]
fn draws_regions_in_both_themes() {
    for dark in [false, true] {
        let buf = render(dark);
        let t = Theme::new(dark);
        let to8 = |c: f32| (c * 255.0).round() as u8;
        let want_bg = [to8(t.bg.r), to8(t.bg.g), to8(t.bg.b)];
        let want_panel = [to8(t.panel.r), to8(t.panel.g), to8(t.panel.b)];
        // bottom-right of the row area is empty background; the status bar and header are panel colour.
        assert_eq!(pixel(&buf, 700, 590), want_panel, "status bar (dark={dark})");
        assert_eq!(pixel(&buf, 790, 60), want_panel, "header (dark={dark})");
        // some pixel in the rows area is text/bar, i.e. not background.
        let drawn = (68..570).any(|y| (0..400).any(|x| pixel(&buf, x, y) != want_bg && pixel(&buf, x, y) != [to8(t.select.r), to8(t.select.g), to8(t.select.b)]));
        assert!(drawn, "no row content (dark={dark})");
    }
}
