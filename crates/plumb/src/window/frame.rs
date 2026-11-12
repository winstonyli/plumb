//! The window's frame and its pixels: DWM theme and accent, the swap chain and device, the non-client hit test,
//! creation and placement.
use super::*;

/// The accent colour the user chose in Windows settings, as `0xRRGGBB`.
pub(super) fn system_accent() -> Option<u32> {
    let mut v = 0u32;
    let mut n = 4u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut n))
    }
    .is_ok()
    // The value is 0xAABBGGRR.
    .then_some((v & 0xff) << 16 | (v >> 8 & 0xff) << 8 | (v >> 16 & 0xff))
}

pub(super) fn dark_mode() -> bool {
    let mut v = 1u32;
    let mut n = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut n),
        )
    };
    r.is_ok() && v == 0
}

/// The adapter that drives the monitor `hwnd` is on, so frames never cross adapters.
pub(super) fn adapter_for(hwnd: HWND) -> Option<IDXGIAdapter> {
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for i in 0.. {
        let adapter = unsafe { factory.EnumAdapters1(i) }.ok()?;
        for j in 0.. {
            let Ok(out) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            if unsafe { out.GetDesc() }.is_ok_and(|d| d.Monitor == mon) {
                return adapter.cast().ok();
            }
        }
    }
    None
}

pub(super) fn client_px(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

/// The DirectComposition objects that put the swap chain on the window; they must outlive it.
pub(super) struct Comp {
    _device: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

/// A swap chain with per-pixel alpha, shown through DirectComposition: an HWND swap chain cannot be
/// transparent, and the system's caption buttons (drawn by DWM under the window) need a transparent corner.
pub(super) fn make_swapchain(gfx: &Gfx, hwnd: HWND) -> Result<(IDXGISwapChain1, Comp)> {
    let (width, height) = client_px(hwnd);
    let dxgi: IDXGIDevice = gfx.d3d.cast()?;
    let factory: IDXGIFactory2 = unsafe { dxgi.GetAdapter()?.GetParent()? };
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: BOOL(0),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };
    unsafe {
        let swap = factory.CreateSwapChainForComposition(&gfx.d3d, &desc, None)?;
        let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
        let target = device.CreateTargetForHwnd(hwnd, true)?;
        let visual = device.CreateVisual()?;
        visual.SetContent(&swap)?;
        target.SetRoot(&visual)?;
        device.Commit()?;
        Ok((swap, Comp { _device: device, _target: target, _visual: visual }))
    }
}

impl Win {
    pub(super) fn apply_title_theme(&self) {
        let v = BOOL::from(self.dark);
        let _ = unsafe {
            DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &v as *const _ as *const _, 4)
        };
        // The caption buttons are drawn by DWM on the frame behind a transparent corner of our picker bar
        // (see `draw`): colour that frame like the bar, and extend it over the bar's whole height.
        let theme = crate::draw::Theme::new(self.dark);
        let (bg, edge) = (crate::draw::colorref(theme.bg), crate::draw::colorref(theme.border));
        let _ = unsafe { DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &bg as *const _ as *const _, 4) };
        let _ = unsafe { DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &edge as *const _ as *const _, 4) };
        // The tooltip takes the theme's colours in both themes: it has no dark visual style, and in light the system look
        // would be the one surface that ignores the accent-tinted palette. Without a visual style it takes the colours we set.
        unsafe {
            let _ = SetWindowTheme(self.tooltip, w!(""), w!(""));
            SendMessageW(self.tooltip, TTM_SETTIPBKCOLOR, Some(WPARAM(crate::draw::colorref(theme.panel) as usize)), None);
            SendMessageW(self.tooltip, TTM_SETTIPTEXTCOLOR, Some(WPARAM(crate::draw::colorref(theme.fg) as usize)), None);
            TIP_FRAME.store(crate::draw::colorref(theme.border), std::sync::atomic::Ordering::Relaxed);
        }
        let bar = ((PICKER_H + self.inset) * self.dpi / 96.0).ceil() as i32;
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: bar, cyBottomHeight: 0 };
        let _ = unsafe { DwmExtendFrameIntoClientArea(self.hwnd, &margins) };
    }

    pub(super) fn target(&mut self) -> Result<ID2D1Bitmap1> {
        if let Some(t) = &self.target {
            return Ok(t.clone());
        }
        let surface: IDXGISurface = unsafe { self.swap.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let t = unsafe { self.gfx.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&props))? };
        self.target = Some(t.clone());
        Ok(t)
    }

    /// Draws and presents one frame. A lost device is rebuilt and the frame drawn again.
    pub(super) fn render(&mut self) {
        for attempt in 0..2 {
            match self.try_render() {
                Ok(()) => return,
                Err(e) if attempt == 0 => {
                    eprintln!("plumb: render failed ({e}); recreating the device");
                    if let Err(e) = self.recreate_device() {
                        eprintln!("plumb: device recreation failed: {e}");
                        return;
                    }
                }
                Err(e) => eprintln!("plumb: render failed again: {e}"),
            }
        }
    }

    pub(super) fn try_render(&mut self) -> Result<()> {
        let target = self.target()?;
        self.app.update_map(96.0 / self.dpi);
        self.app.with_scene(self.dark, |s| self.painter.paint(&self.gfx, &target, self.dpi, self.inset, s))?;
        let hr = unsafe { self.swap.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)) };
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            return Err(hr.into());
        }
        hr.ok()
    }

    pub(super) fn recreate_device(&mut self) -> Result<()> {
        self.target = None;
        let gfx = Gfx::hardware(adapter_for(self.hwnd).as_ref())?;
        (self.swap, self.comp) = make_swapchain(&gfx, self.hwnd)?;
        self.painter = Painter::new(&gfx)?;
        self.app.adapter = gfx.adapter.clone();
        self.gfx = gfx;
        Ok(())
    }

    pub(super) fn resize(&mut self) {
        self.target = None;
        let (w, h) = client_px(self.hwnd);
        let _ = unsafe {
            self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
        };
        // A maximized window overhangs the screen by its frame. The client area must still start at the
        // window's top (DWM places the caption buttons only then), so the content sits `inset` below it.
        let inset = if unsafe { IsZoomed(self.hwnd) }.as_bool() { frame_px(self.hwnd) as f32 * 96.0 / self.dpi } else { 0.0 };
        if inset != self.inset {
            self.inset = inset;
            self.apply_title_theme();
        }
        self.app.handle(Input::Resize(w as f32 * 96.0 / self.dpi, h as f32 * 96.0 / self.dpi - inset));
    }
}

/// The smallest outer window size, in pixels, at `dpi`: the layout drops the map and squeezes the name column well before this.
pub(super) fn min_track_size(dpi: f32) -> (i32, i32) {
    let k = dpi / 96.0;
    ((MIN_WINDOW.0 * k).round() as i32, (MIN_WINDOW.1 * k).round() as i32)
}

pub(super) const MIN_WINDOW: (f32, f32) = (640.0, 400.0);

/// Width of the invisible resize border around the window, in pixels at its DPI.
pub(super) fn frame_px(hwnd: HWND) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

/// What the window frame is at screen point `lp`, for a window whose title bar is drawn by us: the
/// resize edges and corners (the top border is client area, so Windows no longer reports it), the
/// empty part of the picker bar as caption, anything else as client.
pub(super) fn hit_test(hwnd: HWND, lp: LPARAM) -> LRESULT {
    let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
    let mut wr = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut wr) };
    if !unsafe { IsZoomed(hwnd) }.as_bool() {
        let b = frame_px(hwnd);
        let (l, r, t, d) = (p.x < wr.left + b, p.x >= wr.right - b, p.y < wr.top + b, p.y >= wr.bottom - b);
        let code = match (l, r, t, d) {
            (true, _, true, _) => HTTOPLEFT,
            (_, true, true, _) => HTTOPRIGHT,
            (true, _, _, true) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (_, _, true, _) => HTTOP,
            (_, _, _, true) => HTBOTTOM,
            (true, ..) => HTLEFT,
            (_, true, ..) => HTRIGHT,
            _ => 0,
        };
        if code != 0 {
            return LRESULT(code as isize);
        }
    }
    let _ = unsafe { ScreenToClient(hwnd, &mut p) };
    let caption = with(|w| {
        let k = 96.0 / w.dpi;
        w.app.is_caption(p.x as f32 * k, p.y as f32 * k - w.inset)
    })
    .unwrap_or(false);
    LRESULT(if caption { HTCAPTION } else { HTCLIENT } as isize)
}

/// A hidden window at the default size; `run` shows it where it was last left.
pub(super) fn create_window() -> Result<HWND> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst: HMODULE = GetModuleHandleW(None)?;
        let class = w!("PlumbWindow");
        // The exe's icon resource (id 1, see build.rs), at the sizes the shell asks for so the .ico's own 16/24/32 entries are used.
        let icon = |cx, cy| {
            LoadImageW(Some(inst.into()), PCWSTR(std::ptr::without_provenance(1)) /* MAKEINTRESOURCE(1): icon.rc id */, IMAGE_ICON, GetSystemMetrics(cx), GetSystemMetrics(cy), LR_DEFAULTCOLOR | LR_SHARED)
                .map(|h| HICON(h.0))
                .unwrap_or_default()
        };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hIcon: icon(SM_CXICON, SM_CYICON),
            hIconSm: icon(SM_CXSMICON, SM_CYSMICON),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0), class, w!("plumb"), WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT, CW_USEDEFAULT, 1100, 760, None, None, Some(inst.into()), None,
        )?;
        // Scale the default size once the window's monitor DPI is known.
        let dpi = GetDpiForWindow(hwnd);
        let _ = SetWindowPos(hwnd, None, 0, 0, (1100 * dpi / 96) as i32, (760 * dpi / 96) as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        Ok(hwnd)
    }
}

/// Shows the window in the remembered rectangle (Windows moves it onto a monitor that still exists), else at the default.
pub(super) fn show_window(hwnd: HWND, saved: Option<Placement>) {
    unsafe {
        let Some(p) = saved.filter(Placement::is_sane) else {
            let _ = ShowWindow(hwnd, SW_SHOW);
            return;
        };
        let wp = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: if p.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL }.0 as u32,
            rcNormalPosition: RECT { left: p.x, top: p.y, right: p.x + p.w, bottom: p.y + p.h },
            ..Default::default()
        };
        let _ = SetWindowPlacement(hwnd, &wp);
    }
}

/// The window's restored rectangle, even while it is maximized or minimized.
pub(super) fn placement_of(hwnd: HWND) -> Option<Placement> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
    Some(Placement { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, maximized })
}
