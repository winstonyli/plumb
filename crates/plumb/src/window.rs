//! The Win32 window: message loop, swapchain, input translation, scan thread, context menu.
//! All state lives on the UI thread in `STATE`; the scan thread only reports through a channel.
use crate::app::{App, DriveInfo, Effect, Input};
use crate::draw::Painter;
use crate::gfx::Gfx;
use crate::layout::Layout;
use plumb_core::rows::Key;
use plumb_core::{NodeId, Tree, TreeBuilder};
use plumb_scan::select::{scan_auto, ScanReport, Used};
use plumb_scan::{Progress, ScanError};
use std::cell::RefCell;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_IGNORE, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1, IDXGIFactory2, IDXGISurface,
    IDXGISwapChain1, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_SCALING_NONE,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, InvalidateRect, MonitorFromWindow, ValidateRect, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, ShellExecuteW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{w, Interface, Result, BOOL, PCWSTR};

const WM_SCAN_DONE: u32 = WM_APP + 1;
const TIMER_PROGRESS: usize = 1;
const CMD_REVEAL: usize = 1;
const CMD_COPY: usize = 2;
const CMD_PROPS: usize = 3;

type ScanResult = std::result::Result<(Tree, ScanReport, Duration), String>;

struct Win {
    hwnd: HWND,
    app: App,
    gfx: Gfx,
    painter: Painter,
    swap: IDXGISwapChain1,
    target: Option<ID2D1Bitmap1>,
    dpi: f32,
    dark: bool,
    progress: Arc<Progress>,
    rx: Option<Receiver<ScanResult>>,
}

thread_local! {
    static STATE: RefCell<Option<Win>> = const { RefCell::new(None) };
}

/// Runs `f` on the window state. Returns `None` if there is no state yet or it is already
/// borrowed (a message arrived while we were inside a call such as `TrackPopupMenu`).
fn with<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn dark_mode() -> bool {
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
fn adapter_for(hwnd: HWND) -> Option<IDXGIAdapter> {
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

fn client_px(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

fn make_swapchain(gfx: &Gfx, hwnd: HWND) -> Result<IDXGISwapChain1> {
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
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    unsafe { factory.CreateSwapChainForHwnd(&gfx.d3d, hwnd, &desc, None, None) }
}

impl Win {
    fn new(hwnd: HWND, drives: Vec<DriveInfo>) -> Result<Self> {
        let gfx = Gfx::hardware(adapter_for(hwnd).as_ref())?;
        let swap = make_swapchain(&gfx, hwnd)?;
        let painter = Painter::new(&gfx)?;
        let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
        let (pw, ph) = client_px(hwnd);
        let layout = Layout::new(pw as f32 * 96.0 / dpi, ph as f32 * 96.0 / dpi);
        let app = App::new(layout, drives, gfx.adapter.clone());
        let dark = dark_mode();
        let win = Self { hwnd, app, gfx, painter, swap, target: None, dpi, dark, progress: Arc::default(), rx: None };
        win.apply_title_theme();
        Ok(win)
    }

    fn apply_title_theme(&self) {
        let v = BOOL::from(self.dark);
        let _ = unsafe {
            DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &v as *const _ as *const _, 4)
        };
    }

    fn target(&mut self) -> Result<ID2D1Bitmap1> {
        if let Some(t) = &self.target {
            return Ok(t.clone());
        }
        let surface: IDXGISurface = unsafe { self.swap.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
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
    fn render(&mut self) {
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

    fn try_render(&mut self) -> Result<()> {
        let target = self.target()?;
        self.app.with_scene(self.dark, |s| self.painter.paint(&self.gfx, &target, self.dpi, s))?;
        let hr = unsafe { self.swap.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)) };
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            return Err(hr.into());
        }
        hr.ok()
    }

    fn recreate_device(&mut self) -> Result<()> {
        self.target = None;
        let gfx = Gfx::hardware(adapter_for(self.hwnd).as_ref())?;
        self.swap = make_swapchain(&gfx, self.hwnd)?;
        self.painter = Painter::new(&gfx)?;
        self.app.adapter = gfx.adapter.clone();
        self.gfx = gfx;
        Ok(())
    }

    fn resize(&mut self) {
        self.target = None;
        let (w, h) = client_px(self.hwnd);
        let _ = unsafe {
            self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
        };
        self.app.handle(Input::Resize(w as f32 * 96.0 / self.dpi, h as f32 * 96.0 / self.dpi));
    }

    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x * 96.0 / self.dpi, y * 96.0 / self.dpi)
    }

    fn invalidate(&self) {
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    fn start_scan(&mut self, drive: usize) {
        let root = format!("{}:\\", self.app.drives[drive].letter);
        self.spawn_scan(drive, root);
    }

    fn spawn_scan(&mut self, drive: usize, root: String) {
        self.app.begin_scan(drive);
        self.progress = Arc::default();
        let progress = self.progress.clone();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut b = TreeBuilder::new();
                let report = scan_auto(Path::new(&root), &mut b, &progress).map_err(|e| match e {
                    ScanError::Root(e) => e.to_string(),
                    ScanError::Mft(m) => m,
                })?;
                let tree = b.finish().map_err(|e| format!("{e:?}"))?;
                Ok((tree, report, t0.elapsed()))
            }));
            let msg = run.unwrap_or_else(|_| Err("the scanner panicked".to_string()));
            let _ = tx.send(msg);
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_SCAN_DONE, WPARAM(0), LPARAM(0)) };
        });
        unsafe { SetTimer(Some(self.hwnd), TIMER_PROGRESS, 100, None) };
        self.invalidate();
    }

    fn poll_progress(&mut self) {
        let (d, f, b) = self.progress.snapshot();
        self.app.progress(d, f, b);
        self.invalidate();
    }

    fn scan_done(&mut self) {
        let Some(rx) = &self.rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.rx = None;
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_PROGRESS) };
        match msg {
            Ok((tree, report, elapsed)) => {
                let files = tree.subtree_files[NodeId(0)] as u64;
                let s = summary(report.used, files, elapsed, report.mft.as_ref().map(|m| m.0.corrupt), report.note.as_deref());
                self.app.finish_scan(tree, s);
            }
            Err(e) => self.app.fail_scan(e),
        }
        self.invalidate();
    }

    fn selected_path(&self) -> Option<String> {
        let id = self.app.model.as_ref()?.selected()?;
        Some(display_path(&self.app.tree.as_ref()?.path(id)))
    }
}

/// "C:" is the drive's root node; shell calls need "C:\".
pub fn display_path(p: &str) -> String {
    if p.ends_with(':') { format!("{p}\\") } else { p.to_string() }
}

pub fn summary(used: Used, files: u64, elapsed: Duration, corrupt: Option<u64>, note: Option<&str>) -> String {
    let scanner = match used {
        Used::Mft => "MFT scan",
        Used::Walk => "Directory walk",
    };
    let mut s = format!("{scanner}: {} files in {:.1}s", crate::fmt::count(files), elapsed.as_secs_f64());
    if let Some(c) = corrupt.filter(|&c| c > 0) {
        s.push_str(&format!(", {c} records skipped"));
    }
    if let Some(n) = note {
        s.push_str(&format!(" ({n})"));
    }
    s
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn copy_to_clipboard(hwnd: HWND, text: &str) {
    let u = wide(text);
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, u.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(u.as_ptr(), p, u.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(13, Some(HANDLE(h.0))); // CF_UNICODETEXT
            }
        }
        let _ = CloseClipboard();
    }
}

fn reveal(path: &str) {
    let args = wide(&format!("/select,\"{path}\""));
    unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
}

fn properties(path: &str) {
    let p = wide(path);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        lpVerb: w!("properties"),
        lpFile: PCWSTR(p.as_ptr()),
        nShow: SW_SHOW.0,
        ..Default::default()
    };
    let _ = unsafe { ShellExecuteExW(&mut info) };
}

/// Shows the context menu at the cursor and runs the chosen command. Called with no borrow held,
/// because `TrackPopupMenu` runs its own message loop.
fn context_menu(hwnd: HWND, path: String, at: (i32, i32)) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, CMD_REVEAL, w!("Reveal in Explorer"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_COPY, w!("Copy path"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_PROPS, w!("Properties"));
        let mut pt = POINT { x: at.0, y: at.1 };
        let _ = ClientToScreen(hwnd, &mut pt);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            CMD_REVEAL => reveal(&path),
            CMD_COPY => copy_to_clipboard(hwnd, &path),
            CMD_PROPS => properties(&path),
            _ => {}
        }
    }
}

fn effect(hwnd: HWND, e: Effect, at: (i32, i32)) {
    match e {
        Effect::None => {}
        Effect::Scan(i) => {
            with(|w| w.start_scan(i));
        }
        Effect::Menu(id) => {
            let path = with(|w| w.app.tree.as_ref().map(|t| display_path(&t.path(id)))).flatten();
            if let Some(p) = path {
                context_menu(hwnd, p, at);
            }
        }
    }
}

fn pos(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

/// Sends `input` to the app, then runs any effect and repaints.
fn dispatch(hwnd: HWND, l: LPARAM, make: impl FnOnce(f32, f32) -> Input) {
    let Some((e, _)) = with(|w| {
        let (x, y) = w.dip(l);
        let e = w.app.handle(make(x, y));
        w.invalidate();
        (e, ())
    }) else {
        return;
    };
    effect(hwnd, e, pos(l));
}

fn key_of(vk: u16) -> Option<Key> {
    Some(match vk {
        x if x == VK_UP.0 => Key::Up,
        x if x == VK_DOWN.0 => Key::Down,
        x if x == VK_LEFT.0 => Key::Left,
        x if x == VK_RIGHT.0 => Key::Right,
        x if x == VK_PRIOR.0 => Key::PageUp,
        x if x == VK_NEXT.0 => Key::PageDown,
        x if x == VK_HOME.0 => Key::Home,
        x if x == VK_END.0 => Key::End,
        _ => return None,
    })
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            with(|w| w.render());
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if wp.0 != SIZE_MINIMIZED as usize {
                with(|w| {
                    w.resize();
                    w.invalidate();
                });
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            with(|w| w.dpi = (wp.0 & 0xffff) as f32);
            let _ = unsafe {
                SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE)
            };
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            dispatch(hwnd, lp, Input::Click);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            dispatch(hwnd, lp, Input::DoubleClick);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = unsafe { ReleaseCapture() };
            dispatch(hwnd, lp, |_, _| Input::Release);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            dispatch(hwnd, lp, Input::RightClick);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if wp.0 & 1 != 0 {
                dispatch(hwnd, lp, |_, y| Input::Drag(y));
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as i16 as i32;
            with(|w| {
                w.app.handle(Input::Wheel(-delta / 120 * 3));
                w.invalidate();
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            if ctrl && wp.0 == 'C' as usize {
                if let Some(Some(p)) = with(|w| w.selected_path()) {
                    copy_to_clipboard(hwnd, &p);
                }
            } else if let Some(k) = key_of(wp.0 as u16) {
                with(|w| {
                    w.app.handle(Input::Key(k));
                    w.invalidate();
                });
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_PROGRESS => {
            with(|w| w.poll_progress());
            LRESULT(0)
        }
        WM_SCAN_DONE => {
            with(|w| w.scan_done());
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            with(|w| {
                let dark = dark_mode();
                if dark != w.dark {
                    w.dark = dark;
                    w.apply_title_theme();
                    w.invalidate();
                }
            });
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn create_window(visible: bool) -> Result<HWND> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst: HMODULE = GetModuleHandleW(None)?;
        let class = w!("PlumbWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
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
        let _ = SetWindowPos(hwnd, None, 0, 0, (1100 * dpi / 96) as i32, (760 * dpi / 96) as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        if visible {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        Ok(hwnd)
    }
}

/// Runs the app until the window closes.
pub fn run() -> Result<()> {
    let hwnd = create_window(true)?;
    let win = Win::new(hwnd, crate::drives::list())?;
    eprintln!("plumb: {}", win.gfx.adapter);
    STATE.with(|s| *s.borrow_mut() = Some(win));
    with(|w| w.invalidate());
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    STATE.with(|s| *s.borrow_mut() = None);
    Ok(())
}

/// Builds the window hidden, scans a small temp directory on a worker thread through the real
/// message loop and swapchain, and returns "adapter; summary". Never shows a window.
pub fn selftest() -> Result<String> {
    let dir = std::env::temp_dir().join(format!("plumb-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).ok();
    std::fs::write(dir.join("sub").join("f.bin"), vec![1u8; 100_000]).ok();
    let hwnd = create_window(false)?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    with(|w| w.spawn_scan(0, dir.to_string_lossy().into_owned()));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut msg = MSG::default();
    while with(|w| w.app.tree.is_none() && !matches!(w.app.scan, crate::app::Scan::Failed(_))) == Some(true) {
        if Instant::now() > deadline {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "scan did not finish in 30 s"));
        }
        unsafe {
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                std::thread::yield_now();
            }
        }
    }
    let out = with(|w| {
        w.resize();
        let sub = w.app.tree.as_ref().map(|t| t.subtree_size[NodeId(0)]);
        w.try_render().map(|_| format!("{}; {}; root {:?} bytes", w.gfx.adapter, w.app.summary, sub))
    });
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out.expect("state present")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_root_gets_a_trailing_backslash() {
        assert_eq!(display_path("C:"), "C:\\");
        assert_eq!(display_path("C:\\Windows"), "C:\\Windows");
    }

    #[test]
    fn summary_lists_scanner_counts_skips_and_notes() {
        let s = summary(Used::Mft, 1234, Duration::from_millis(2500), Some(3), Some("note"));
        assert_eq!(s, "MFT scan: 1,234 files in 2.5s, 3 records skipped (note)");
        assert_eq!(summary(Used::Walk, 5, Duration::from_secs(1), None, None), "Directory walk: 5 files in 1.0s");
    }
}
