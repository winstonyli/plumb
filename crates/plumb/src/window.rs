//! The Win32 window: message loop, swapchain, input translation, scan thread, context menu.
//! All state lives on the UI thread in `STATE`; the scan and journal threads only report through channels.
use crate::app::{App, DriveInfo, Effect, Input, LiveStatus, Parked};
use crate::draw::{Mark, Painter};
use crate::filter_ui::{byte_at, push_utf16, Edit, Motion};
use crate::fmt;
use crate::state::Placement;
use crate::gfx::Gfx;
use crate::layout::{Layout, PICKER_H};
use crate::preview::Preview;
use crate::live::{needs_compaction, Live, Pacer, RescanPolicy, REPAINT};
use plumb_core::filter::Hits;
use plumb_core::rows::Key;
use plumb_core::{NodeId, Tree, TreeBuilder};
use plumb_scan::preview::channel as preview_channel;
use plumb_scan::journal::{Delta, Event, JournalConfig, JournalInfo, JournalReader, JournalSource, Stop};
use plumb_scan::listing::Listing;
use plumb_scan::watch::{Event as WatchEvent, Stop as WatchStop, WatchConfig, Watcher};
use plumb_scan::select::{scan_auto, ScanReport, Used};
use plumb_scan::volume::{is_elevated, is_ntfs, Volume};
use plumb_scan::{Progress, ScanError};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
};
use windows::Win32::Graphics::Dwm::{DwmDefWindowProc, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1, IDXGIFactory2, IDXGISurface,
    IDXGISwapChain1, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, ScreenToClient, InvalidateRect, MonitorFromWindow, ValidateRect, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{GetSystemMetricsForDpi, 
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, CANDIDATEFORM, CFS_CANDIDATEPOS,
    CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_RESULTSTR, IME_COMPOSITION_STRING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, ShellExecuteW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, MARGINS, NMHDR, NMTTDISPINFOW, TOOLTIPS_CLASSW, TTF_IDISHWND,
    TTM_ADDTOOLW, TTM_POP, TTM_RELAYEVENT, TTM_SETMAXTIPWIDTH, TTM_SETTIPBKCOLOR, TTM_SETTIPTEXTCOLOR, TTN_GETDISPINFOW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::core::{w, Interface, Result, BOOL, PCWSTR, PWSTR};

const WM_SCAN_DONE: u32 = WM_APP + 1;
const WM_JOURNAL: u32 = WM_APP + 2;
const WM_FILTER: u32 = WM_APP + 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_PROGRESS: usize = 1;
const TIMER_LIVE: usize = 2;
const TIMER_FILTER: usize = 3;
const FILTER_DEBOUNCE_MS: u32 = 100;
const TIMER_PREVIEW: usize = 4;
const PREVIEW_TICK_MS: u32 = 15;
/// Messages the partial-tree queue holds: about 150 ms of an MFT scan.
const PREVIEW_QUEUE: usize = 200_000;
/// Longest a batch of journal deltas may hold the UI thread.
const APPLY_BUDGET: Duration = Duration::from_millis(5);
const CMD_REVEAL: usize = 1;
const CMD_COPY: usize = 2;
const CMD_PROPS: usize = 3;

struct Scanned {
    tree: Tree,
    report: ScanReport,
    elapsed: Duration,
    /// The volume and journal position to follow, read before the scan began so changes made
    /// during it are replayed. Present only for elevated NTFS drives.
    journal: Option<(Volume, JournalInfo)>,
    root: String,
}

type ScanResult = std::result::Result<Scanned, String>;

/// What a live watch follows.
enum Source {
    Journal { reader: JournalReader, live: Live, queue: VecDeque<Delta> },
    Dir { watcher: Watcher, queue: VecDeque<Listing> },
}

/// Everything live updates need for the drive on screen.
struct Watch {
    src: Source,
    pacer: Pacer,
    /// The tree changed since the rows were last refreshed.
    dirty: bool,
    /// `TIMER_LIVE` is running.
    armed: bool,
}

/// Why a watch ended; `why` is `None` when it was asked to stop.
struct Ended {
    rescan: bool,
    why: Option<String>,
}

impl Watch {
    /// Moves what the source has queued into its queue; `Some` if the source has ended.
    fn drain(&mut self) -> Option<Ended> {
        let ended = |rescan: bool, why: &str| Some(Ended { rescan, why: Some(why.to_string()) });
        match &mut self.src {
            Source::Journal { reader, queue, .. } => {
                while let Some(ev) = reader.try_recv() {
                    match ev {
                        Event::Deltas(d) => queue.extend(d),
                        Event::Stopped(Stop::Requested) => return Some(Ended { rescan: false, why: None }),
                        Event::Stopped(Stop::Wrapped) => {
                            return ended(true, "the change journal wrapped, so changes were missed");
                        }
                        Event::Stopped(Stop::Disabled) => return ended(false, "the change journal is not active"),
                        Event::Stopped(Stop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
            Source::Dir { watcher, queue } => {
                while let Some(ev) = watcher.try_recv() {
                    match ev {
                        WatchEvent::Listings(l) => queue.extend(l),
                        WatchEvent::Stopped(WatchStop::Overflow) => {
                            return ended(true, "the directory watch overflowed, so changes were missed");
                        }
                        WatchEvent::Stopped(WatchStop::TooMany) => {
                            return ended(true, "too many folders changed at once");
                        }
                        WatchEvent::Stopped(WatchStop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
        }
        None
    }

    /// Applies queued changes for the budget; returns how many tree changes were made.
    fn apply(&mut self, tree: &mut Tree, deadline: Instant) -> usize {
        match &mut self.src {
            Source::Journal { live, queue, .. } => live.apply(tree, queue, deadline),
            Source::Dir { queue, .. } => crate::dirsync::apply(tree, queue, deadline),
        }
    }

    fn queued(&self) -> bool {
        match &self.src {
            Source::Journal { queue, .. } => !queue.is_empty(),
            Source::Dir { queue, .. } => !queue.is_empty(),
        }
    }

    /// Work left that a timer tick should finish.
    fn backlog(&self) -> bool {
        self.queued() || matches!(&self.src, Source::Journal { live, .. } if live.pending())
    }

    /// Stops the source (blocks briefly: call off the UI thread).
    fn stop(self) {
        match self.src {
            Source::Journal { reader, .. } => reader.stop(),
            Source::Dir { watcher, .. } => watcher.stop(),
        }
    }
}

fn open_journal(root: &str) -> Option<(Volume, JournalInfo)> {
    let root = Path::new(root);
    if !is_elevated() || !is_ntfs(root) {
        return None;
    }
    let vol = Volume::open(root).ok()?; // fails for anything but a whole volume
    let info = vol.query().ok()?;
    Some((vol, info))
}

fn local_time() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

/// Everything the window keeps per drive besides what the app holds (`Parked`). The fields of the
/// drive on screen live directly in `Win`; the others wait in `Win::parked` as one of these.
#[derive(Default)]
struct Drive {
    app: Parked,
    progress: Arc<Progress>,
    rx: Option<Receiver<ScanResult>>,
    /// What the scan has found so far; kept drives have none (a partial tree follows only the drive on screen).
    preview: Option<Preview>,
    background: bool,
    watch: Option<Watch>,
    dir_pending: Option<Watcher>,
    dir_failed: Option<String>,
    /// Order of last use, for eviction.
    used: u64,
}

impl Drive {
    /// Ends the drive's scan and watches. The watches may be mid-wait, so they stop off the UI thread.
    fn discard(self) {
        self.progress.cancel();
        if self.watch.is_none() && self.dir_pending.is_none() {
            return;
        }
        std::thread::spawn(move || {
            if let Some(w) = self.watch {
                w.stop();
            }
            if let Some(w) = self.dir_pending {
                w.stop();
            }
        });
    }
}

/// Drives whose trees are held at once, the one on screen included.
const KEPT_DRIVES: usize = 3;

struct Win {
    hwnd: HWND,
    app: App,
    gfx: Gfx,
    painter: Painter,
    swap: IDXGISwapChain1,
    comp: Comp,
    target: Option<ID2D1Bitmap1>,
    dpi: f32,
    /// DIPs of the client area's top that overhang the screen (a maximized window's frame); 0 otherwise.
    inset: f32,
    dark: bool,
    progress: Arc<Progress>,
    rx: Option<Receiver<ScanResult>>,
    /// The scan in flight is a rescan: the old tree stays on screen until it finishes.
    background: bool,
    watch: Option<Watch>,
    /// A directory watch started with the scan and held until the tree is installed.
    dir_pending: Option<Watcher>,
    /// Why the directory watch for the scan in flight could not start.
    dir_failed: Option<String>,
    policy: RescanPolicy,
    /// When the user last clicked, scrolled or pressed a key.
    last_input: Instant,
    /// A `WM_JOURNAL` is already queued, so the reader need not post another.
    posted: Arc<AtomicBool>,
    /// Finished filter scans; the worker posts `WM_FILTER` after each send.
    filter_tx: Sender<(u64, Hits)>,
    filter_rx: Receiver<(u64, Hits)>,
    /// First half of a surrogate pair seen by `WM_CHAR`.
    high_surrogate: Option<u16>,
    /// The left button went down in the filter box and is still held: moving extends the selection.
    filter_drag: bool,
    /// The partial tree of the foreground scan in progress.
    preview: Option<Preview>,
    preview_pacer: Pacer,
    /// The preview tree changed since the rows were last refreshed.
    preview_dirty: bool,
    /// The drives not on screen that hold a tree or a scan, by drive index.
    parked: Vec<Option<Drive>>,
    /// Counts drive switches, to find the least recently used.
    clock: u64,
    /// Which of the progress, preview and live timers are running.
    timers: [bool; 3],
    /// A parked drive is swapped in for one operation (`in_slot`): timers and the filter stay as they are.
    borrowed: bool,
    /// The tooltip control, the icon button it last described, and the text it reads from (`tip_text`).
    tooltip: HWND,
    tip: Option<String>,
    tip_buf: Vec<u16>,
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

/// The DirectComposition objects that put the swap chain on the window; they must outlive it.
struct Comp {
    _device: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

/// A swap chain with per-pixel alpha, shown through DirectComposition: an HWND swap chain cannot be
/// transparent, and the system's caption buttons (drawn by DWM under the window) need a transparent corner.
fn make_swapchain(gfx: &Gfx, hwnd: HWND) -> Result<(IDXGISwapChain1, Comp)> {
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
    fn new(hwnd: HWND, drives: Vec<DriveInfo>) -> Result<Self> {
        let gfx = Gfx::hardware(adapter_for(hwnd).as_ref())?;
        let (swap, comp) = make_swapchain(&gfx, hwnd)?;
        let painter = Painter::new(&gfx)?;
        let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
        let (pw, ph) = client_px(hwnd);
        let layout = Layout::new(pw as f32 * 96.0 / dpi, ph as f32 * 96.0 / dpi);
        let mut app = App::new(layout, drives, gfx.adapter.clone());
        app.can_elevate = !plumb_scan::volume::is_elevated();
        app.fit_chips(&|s| painter.text_width(&gfx, s));
        let dark = dark_mode();
        let (filter_tx, filter_rx) = channel();
        let win = Self {
            hwnd, app, gfx, painter, swap, comp, target: None, dpi, inset: 0.0, dark, progress: Arc::default(), rx: None,
            background: false, watch: None, dir_pending: None, dir_failed: None, policy: RescanPolicy::default(), last_input: Instant::now(),
            posted: Arc::default(), filter_tx, filter_rx, high_surrogate: None, filter_drag: false,
            preview: None, preview_pacer: Pacer::new(Instant::now()), preview_dirty: false,
            parked: Vec::new(), clock: 0, timers: [false; 3], borrowed: false,
            tooltip: Self::make_tooltip(hwnd), tip: None, tip_buf: Vec::new(),
        };
        win.apply_title_theme();
        Ok(win)
    }

    /// A tooltip control over the whole window, asking for its text (`WM_NOTIFY`) when the pointer rests.
    fn make_tooltip(hwnd: HWND) -> HWND {
        unsafe {
            let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES });
            let tip = CreateWindowExW(
                WS_EX_TOPMOST, TOOLTIPS_CLASSW, PCWSTR::null(), WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0, 0, 0, 0, Some(hwnd), None, None, None,
            )
            .unwrap_or_default();
            let tool = TTTOOLINFOW {
                // The size without `lParam` and `lpReserved`, which version 5 of comctl32 insists on.
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lParam) as u32,
                uFlags: TTF_IDISHWND, // mouse messages are relayed by hand (`WM_MOUSEMOVE`)
                hwnd,
                uId: hwnd.0 as usize,
                lpszText: PWSTR(usize::MAX as *mut u16), // LPSTR_TEXTCALLBACK
                ..Default::default()
            };
            SendMessageW(tip, TTM_ADDTOOLW, None, Some(LPARAM(&tool as *const _ as isize)));
            // Wraps a long path, and lets the status tooltip's line break through.
            SendMessageW(tip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(640)));
            tip
        }
    }

    /// The tooltip text under the pointer, as the wide string the control reads; empty for none.
    fn tip_text(&mut self) -> *const u16 {
        let mut p = POINT::default();
        let tip = unsafe { GetCursorPos(&mut p).is_ok() && ScreenToClient(self.hwnd, &mut p).as_bool() }
            .then(|| self.app.tip_at(p.x as f32 * 96.0 / self.dpi, p.y as f32 * 96.0 / self.dpi - self.inset))
            .flatten();
        self.tip_buf = tip.unwrap_or_default().encode_utf16().chain([0]).collect();
        self.tip_buf.as_ptr()
    }

    fn apply_title_theme(&self) {
        let v = BOOL::from(self.dark);
        let _ = unsafe {
            DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &v as *const _ as *const _, 4)
        };
        // The caption buttons are drawn by DWM on the frame behind a transparent corner of our picker bar
        // (see `draw`): colour that frame like the bar, and extend it over the bar's whole height.
        let bg: u32 = if self.dark { 0x1e1e1e } else { 0xffffff };
        let _ = unsafe { DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &bg as *const _ as *const _, 4) };
        let _ = unsafe { DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &bg as *const _ as *const _, 4) };
        // The tooltip control has no dark visual style: without one it takes the colours we set; in light it keeps the default.
        unsafe {
            if self.dark {
                let _ = SetWindowTheme(self.tooltip, w!(""), w!(""));
                SendMessageW(self.tooltip, TTM_SETTIPBKCOLOR, Some(WPARAM(0x2b2b2b)), None);
                SendMessageW(self.tooltip, TTM_SETTIPTEXTCOLOR, Some(WPARAM(0xe6e6e6)), None);
            } else {
                let _ = SetWindowTheme(self.tooltip, PCWSTR::null(), PCWSTR::null());
            }
        }
        let bar = ((PICKER_H + self.inset) * self.dpi / 96.0).ceil() as i32;
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: bar, cyBottomHeight: 0 };
        let _ = unsafe { DwmExtendFrameIntoClientArea(self.hwnd, &margins) };
    }

    fn target(&mut self) -> Result<ID2D1Bitmap1> {
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
        self.app.update_map(96.0 / self.dpi);
        self.app.with_scene(self.dark, |s| self.painter.paint(&self.gfx, &target, self.dpi, self.inset, s))?;
        let hr = unsafe { self.swap.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)) };
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            return Err(hr.into());
        }
        hr.ok()
    }

    fn recreate_device(&mut self) -> Result<()> {
        self.target = None;
        let gfx = Gfx::hardware(adapter_for(self.hwnd).as_ref())?;
        (self.swap, self.comp) = make_swapchain(&gfx, self.hwnd)?;
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
        // A maximized window overhangs the screen by its frame. The client area must still start at the
        // window's top (DWM places the caption buttons only then), so the content sits `inset` below it.
        let inset = if unsafe { IsZoomed(self.hwnd) }.as_bool() { frame_px(self.hwnd) as f32 * 96.0 / self.dpi } else { 0.0 };
        if inset != self.inset {
            self.inset = inset;
            self.apply_title_theme();
        }
        self.app.handle(Input::Resize(w as f32 * 96.0 / self.dpi, h as f32 * 96.0 / self.dpi - inset));
    }

    /// A client point in DIPs, measured from the top of the content (below the overhang).
    /// The filter text's byte offset under device-independent x, for placing the caret.
    fn filter_byte(&self, x: f32) -> usize {
        byte_at(&self.app.filter_ui().text, x - self.app.filter_origin(), &|s| self.painter.text_width(&self.gfx, s))
    }

    /// Presses in the filter box place the caret (shift extends the selection, a double-click picks a word).
    fn filter_press(&mut self, lp: LPARAM, double: bool) {
        let (x, y) = self.dip(lp);
        if !self.app.over_filter_box(x, y) {
            return;
        }
        let at = self.filter_byte(x);
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        self.app.handle(Input::Edit(if double { Edit::Word(at) } else { Edit::Place { at, extend: shift } }));
        self.filter_drag = true;
        self.invalidate();
    }

    /// Puts the input method's composition and candidate windows under the filter caret.
    fn place_ime(&self) {
        let u = self.app.filter_ui();
        let x = self.app.filter_origin() + self.painter.text_width(&self.gfx, &format!("{}{}", &u.text[..u.caret], u.preedit));
        let b = self.app.filter_box();
        let (px, py) = ((x * self.dpi / 96.0) as i32, ((b.y + b.h + self.inset) * self.dpi / 96.0) as i32);
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.0.is_null() {
                return;
            }
            let at = POINT { x: px, y: py };
            let _ = ImmSetCompositionWindow(himc, &COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: at, rcArea: RECT::default() });
            let _ = ImmSetCandidateWindow(himc, &CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_CANDIDATEPOS, ptCurrentPos: at, rcArea: RECT::default() });
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x * 96.0 / self.dpi, y * 96.0 / self.dpi - self.inset)
    }

    fn invalidate(&self) {
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    fn start_scan(&mut self, drive: usize) {
        let root = self.app.drives[drive].root.clone();
        self.open(drive, root);
    }

    /// Puts `drive` on screen: the state kept for it if there is any (a finished tree, or a scan
    /// that kept running), else a new scan of `root`. The drive on screen is kept, not cancelled.
    /// Choosing the drive already on screen scans it afresh.
    fn open(&mut self, drive: usize, root: String) {
        if self.app.active != Some(drive) {
            self.park_active();
            if self.unpark(drive) {
                return;
            }
        }
        self.spawn_scan(drive, root, false);
    }

    /// Exchanges all per-drive state, the app's and the window's, with `d`.
    fn swap_drive(&mut self, d: &mut Drive) {
        self.app.switch_slot(&mut d.app);
        self.swap_window_fields(d);
        self.sync_timers();
    }

    fn swap_window_fields(&mut self, d: &mut Drive) {
        use std::mem::swap;
        swap(&mut self.progress, &mut d.progress);
        swap(&mut self.rx, &mut d.rx);
        swap(&mut self.preview, &mut d.preview);
        swap(&mut self.background, &mut d.background);
        swap(&mut self.watch, &mut d.watch);
        swap(&mut self.dir_pending, &mut d.dir_pending);
        swap(&mut self.dir_failed, &mut d.dir_failed);
    }

    /// Takes the drive on screen away, keeping its tree, scan and watch unless there is nothing
    /// worth coming back to. A running scan keeps building its partial tree (`preview_tick`).
    fn park_active(&mut self) {
        let Some(old) = self.app.active.take() else { return };
        let mut d = Drive::default();
        self.swap_drive(&mut d);
        if d.app.has_tree() || d.rx.is_some() {
            self.clock += 1;
            d.used = self.clock;
            self.parked.resize_with(self.parked.len().max(old + 1), || None);
            self.parked[old] = Some(d);
            self.evict();
        } else {
            d.discard();
        }
        self.sync_marks();
    }

    /// Brings the state kept for `drive` on screen, if any. Call with nothing on screen.
    fn unpark(&mut self, drive: usize) -> bool {
        let Some(mut d) = self.parked.get_mut(drive).and_then(Option::take) else { return false };
        self.swap_drive(&mut d);
        self.app.active = Some(drive);
        if self.preview.is_some() {
            // The scan kept building its tree while the drive was away: make the rows for it as it is now.
            self.app.preview_installed();
            self.preview_pacer = Pacer::new(Instant::now());
            self.preview_dirty = false;
        }
        self.sync_marks();
        self.apply_live(); // what changed while it was away
        if self.app.filter_dirty() {
            self.run_filter();
        }
        self.invalidate();
        true
    }

    /// Lets go of the least recently used kept drives that have no scan running, so at most
    /// `KEPT_DRIVES` trees are held with the one on screen.
    fn evict(&mut self) {
        while self.parked.iter().flatten().filter(|d| d.app.has_tree()).count() >= KEPT_DRIVES {
            let lru = self.parked.iter().enumerate().filter_map(|(i, d)| d.as_ref().filter(|d| d.rx.is_none()).map(|d| (d.used, i))).min();
            let Some((_, i)) = lru else { return };
            if let Some(d) = self.parked[i].take() {
                d.discard();
            }
        }
    }

    /// Runs `f` with the kept state of `drive` swapped in, then puts it back; `f` sees that drive as
    /// the one on screen. `None` if nothing is kept for it. Timers and the filter are not touched.
    fn in_slot<R>(&mut self, drive: usize, f: impl FnOnce(&mut Self) -> R) -> Option<R> {
        if self.app.active == Some(drive) {
            return Some(f(self));
        }
        let mut d = self.parked.get_mut(drive).and_then(Option::take)?;
        let shown = self.app.active;
        self.borrowed = true;
        self.app.swap_slot(&mut d.app);
        self.swap_window_fields(&mut d);
        self.app.active = Some(drive);
        let r = f(self);
        self.app.active = shown;
        self.app.swap_slot(&mut d.app);
        self.swap_window_fields(&mut d);
        self.borrowed = false;
        self.parked[drive] = Some(d);
        self.sync_marks();
        Some(r)
    }

    /// Updates the chip marks from what the other drives hold.
    fn sync_marks(&mut self) {
        let marks = (0..self.app.drives.len())
            .map(|i| match self.parked.get(i).and_then(Option::as_ref) {
                Some(d) if d.rx.is_some() => Mark::Scanning,
                Some(d) if d.app.has_tree() => Mark::Kept,
                _ => Mark::None,
            })
            .collect();
        self.app.marks = marks;
    }

    /// Starts and stops the progress, preview and live timers to match the drive on screen.
    fn sync_timers(&mut self) {
        if self.borrowed {
            return;
        }
        let want = [
            (TIMER_PROGRESS, self.rx.is_some() && !self.background, 100),
            (TIMER_PREVIEW, self.preview.is_some() || self.parked.iter().flatten().any(|d| d.preview.is_some()), PREVIEW_TICK_MS),
            (TIMER_LIVE, self.watch.as_ref().is_some_and(|w| w.armed), REPAINT.as_millis() as u32),
        ];
        for (i, (id, on, ms)) in want.into_iter().enumerate() {
            if self.timers[i] != on {
                self.timers[i] = on;
                if on {
                    unsafe { SetTimer(Some(self.hwnd), id, ms, None) };
                } else {
                    let _ = unsafe { KillTimer(Some(self.hwnd), id) };
                }
            }
        }
    }

    /// Ends every scan and watch, for exit.
    fn shutdown(&mut self) {
        self.progress.cancel();
        for d in self.parked.drain(..).flatten() {
            d.discard();
        }
    }

    /// Rescans in the background, keeping the old tree on screen until the new one is ready.
    fn start_rescan(&mut self, drive: usize) {
        if self.rx.is_none() {
            let root = self.app.drives[drive].root.clone();
            self.spawn_scan(drive, root, true);
        }
    }

    /// Stops the scan in flight. Its thread winds down on its own and its result is dropped with
    /// the old channel. A first scan leaves no tree; a background rescan keeps the old one.
    fn cancel_scan(&mut self) {
        if self.rx.take().is_none() {
            return;
        }
        self.progress.cancel();
        self.preview = None;
        self.background = false;
        self.drop_pending();
        self.app.cancel_scan();
        self.sync_timers();
        self.invalidate();
    }

    fn spawn_scan(&mut self, drive: usize, root: String, background: bool) {
        if self.rx.is_some() {
            // The drive on screen is scanned afresh mid-scan: the old scan is abandoned, not waited for.
            self.progress.cancel();
        }
        self.background = background;
        if background {
            self.app.refreshing = true;
        } else {
            self.drop_watch();
            self.app.begin_scan(drive);
        }
        self.progress = if background {
            self.preview = None;
            Arc::default()
        } else {
            let (tx, rx) = preview_channel(PREVIEW_QUEUE);
            self.preview = Some(Preview::new(rx));
            self.preview_pacer = Pacer::new(Instant::now());
            self.preview_dirty = false;
            Arc::new(Progress::with_preview(tx))
        };
        // Opened here so the choice of watch is known before the scan starts: a volume whose journal
        // is off (a fresh one) or cannot be opened is watched by directory instead.
        let journal = open_journal(&root);
        self.start_dir_pending(&root, journal.is_some());
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
                    ScanError::Cancelled => "cancelled".to_string(),
                })?;
                let tree = b.finish().map_err(|e| format!("{e:?}"))?;
                Ok(Scanned { tree, report, elapsed: t0.elapsed(), journal, root: root.clone() })
            }));
            let msg = run.unwrap_or_else(|_| Err("the scanner panicked".to_string()));
            let _ = tx.send(msg);
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_SCAN_DONE, WPARAM(drive), LPARAM(0)) };
        });
        self.sync_timers();
        self.invalidate();
    }

    /// (Re)starts the debounce: the scan begins 100 ms after the last keystroke.
    fn arm_filter(&self) {
        unsafe { SetTimer(Some(self.hwnd), TIMER_FILTER, FILTER_DEBOUNCE_MS, None) };
    }

    /// Scans the names for the current query on a worker thread, if the app wants a run.
    fn run_filter(&mut self) {
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_FILTER) };
        if let Some(job) = self.app.start_filter() {
            let (tx, hwnd) = (self.filter_tx.clone(), self.hwnd.0 as isize);
            std::thread::spawn(move || {
                if let Some(hits) = job.names.scan(&job.matcher, &job.cancel, job.generation) {
                    let _ = tx.send((job.generation, hits));
                    let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_FILTER, WPARAM(0), LPARAM(0)) };
                }
            });
        }
        self.invalidate();
    }

    /// `WM_FILTER`: installs the finished scans (the app drops stale ones).
    fn filter_done(&mut self) {
        while let Ok((generation, hits)) = self.filter_rx.try_recv() {
            self.app.filter_ready(generation, hits);
        }
        self.invalidate();
    }

    fn poll_progress(&mut self) {
        let (d, f, b) = self.progress.snapshot();
        self.app.progress(d, f, b);
        self.invalidate();
    }

    /// `TIMER_PREVIEW`: applies some of what the scan has found, and at the live-update pace
    /// refreshes the rows.
    fn preview_tick(&mut self) {
        let deadline = Instant::now() + APPLY_BUDGET;
        // Scans of kept drives build their trees too, so a drive comes back with what was found.
        for d in self.parked.iter_mut().flatten() {
            if let Some(p) = &mut d.preview {
                p.apply(d.app.tree_mut(), deadline);
            }
        }
        let Some(p) = &mut self.preview else { return };
        let a = p.apply(&mut self.app.tree, deadline);
        if a.reset || a.created {
            self.app.preview_installed(); // builds the model from whatever tree there is now
        } else {
            self.preview_dirty |= a.changes > 0;
        }
        self.app.preview_paused = p.paused();
        if self.preview_dirty && self.preview_pacer.due(Instant::now()) {
            let t0 = Instant::now();
            self.app.refresh_rows();
            self.preview_pacer.done(Instant::now(), t0.elapsed());
            self.preview_dirty = false;
        }
        self.invalidate();
    }

    fn scan_done(&mut self) {
        let Some(rx) = &self.rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.rx = None;
        self.preview = None; // anything still queued is discarded; the exact tree is here
        let background = std::mem::take(&mut self.background);
        match msg {
            Ok(sc) => {
                let files = sc.tree.subtree_files[NodeId(0)] as u64;
                let corrupt = sc.report.mft.as_ref().map(|m| m.stats.corrupt);
                let s = summary(sc.report.used, files, sc.elapsed, corrupt, sc.report.note.as_deref());
                // `begin_scan` cleared the tree, so one on screen is the preview (or the tree
                // being rescanned); with none yet the scan finished before the first preview.
                if self.app.tree.is_some() {
                    self.app.swap_tree(sc.tree, s);
                    self.drop_watch();
                } else {
                    self.app.finish_scan(sc.tree, s);
                }
                // The MFT scan fell back to the walker, so the journal cannot be followed (it needs the
                // MFT's records): watch directories instead, from now on.
                if sc.journal.is_some() && sc.report.mft.is_none() {
                    self.start_dir_pending(&sc.root, false);
                }
                if let (Some((vol, info)), Some(mft)) = (sc.journal, sc.report.mft) {
                    self.drop_pending();
                    self.start_watch(vol, info, mft.records);
                } else if let Some(w) = self.dir_pending.take() {
                    self.install_dir_watch(w);
                } else if let Some(m) = self.dir_failed.take() {
                    self.app.set_live(LiveStatus::Stale(format!("live updates are off: {m}")));
                }
            }
            Err(e) if background => {
                self.drop_pending();
                self.app.refreshing = false;
                self.app.set_live(LiveStatus::Stale(format!("the rescan failed: {e}")));
            }
            Err(e) => {
                self.drop_pending();
                self.app.fail_scan(e);
            }
        }
        self.sync_timers();
        if self.app.filter_dirty() && !self.borrowed {
            self.run_filter();
        }
        self.invalidate();
    }

    fn start_watch(&mut self, vol: Volume, info: JournalInfo, records: Vec<NodeId>) {
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        let reader = JournalReader::start(vol, info, JournalConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        });
        self.watch = Some(Watch {
            src: Source::Journal { reader, live: Live::new(records), queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
    }

    /// Makes the directory watch started with the scan the live watch: releases what it collected
    /// while the scan ran and wakes the window to apply it.
    fn install_dir_watch(&mut self, watcher: Watcher) {
        watcher.release();
        self.watch = Some(Watch {
            src: Source::Dir { watcher, queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
    }

    /// Starts a held directory watch on `root` unless the change journal will follow it
    /// (`journal`). A watch that cannot start leaves the tree static.
    fn start_dir_pending(&mut self, root: &str, journal: bool) {
        self.drop_pending();
        self.dir_failed = None;
        if journal {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        match Watcher::start(Path::new(root), WatchConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        }) {
            Ok(w) => self.dir_pending = Some(w),
            Err(m) => self.dir_failed = Some(m),
        }
    }

    fn drop_pending(&mut self) {
        if let Some(w) = self.dir_pending.take() {
            std::thread::spawn(move || w.stop());
        }
    }

    /// Stops following changes. The source may be mid-wait, so it is joined off the UI thread.
    fn drop_watch(&mut self) {
        if let Some(w) = self.watch.take() {
            std::thread::spawn(move || w.stop());
            self.sync_timers();
        }
    }

    /// `WM_JOURNAL`: moves what the source has queued into its queue and applies some of it.
    fn journal_event(&mut self) {
        self.posted.store(false, Ordering::Release);
        for d in 0..self.parked.len().max(self.app.active.map_or(0, |a| a + 1)) {
            // The watches of drives off screen keep their trees current too.
            self.in_slot(d, |w| {
                let ended = w.watch.as_mut().and_then(Watch::drain);
                w.apply_live();
                if let Some(e) = ended {
                    w.watch_ended(e);
                }
            });
        }
    }

    /// Applies queued changes for at most `APPLY_BUDGET`; leftovers continue on the next message.
    fn apply_queued(&mut self) {
        let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) else { return };
        let n = w.apply(tree, Instant::now() + APPLY_BUDGET);
        w.dirty |= n > 0;
        if w.queued() {
            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
        }
    }

    /// `apply_queued`, and starts the live timer if there is work to refresh. A drive off screen
    /// keeps its changes marked until it is shown again.
    fn apply_live(&mut self) {
        self.apply_queued();
        if let Some(w) = &mut self.watch
            && !w.armed
            && (w.dirty || w.backlog())
            && !self.borrowed
        {
            w.armed = true;
            self.sync_timers();
        }
    }

    /// `TIMER_LIVE`: at most every `REPAINT`, refreshes the rows once if the tree changed.
    fn live_tick(&mut self) {
        self.apply_live();
        let due = self.watch.as_ref().is_some_and(|w| w.dirty && w.pacer.due(Instant::now()));
        if due {
            let t0 = Instant::now();
            self.app.refresh_rows();
            self.app.live_changed(local_time());
            self.invalidate();
            if let Some(w) = &mut self.watch {
                w.pacer.done(Instant::now(), t0.elapsed());
                w.dirty = false;
            }
            if self.app.tree.as_ref().is_some_and(needs_compaction) {
                self.auto_rescan();
            }
        }
        if let Some(w) = &mut self.watch
            && w.armed
            && !w.dirty
            && !w.backlog()
        {
            w.armed = false;
            self.sync_timers();
        }
    }

    /// The source ended: bring the tree up to date, then rescan or say why updates stopped.
    fn watch_ended(&mut self, e: Ended) {
        if let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.tree) {
            w.apply(tree, Instant::now() + Duration::from_millis(50));
        }
        self.app.refresh_rows();
        self.drop_watch();
        self.app.set_live(LiveStatus::Off);
        let Some(why) = e.why else { return };
        if !(e.rescan && self.auto_rescan()) {
            self.app.set_live(LiveStatus::Stale(why));
        }
        self.invalidate();
    }

    /// Starts a background rescan if the policy allows (or one is already running).
    fn auto_rescan(&mut self) -> bool {
        if self.rx.is_some() {
            return true;
        }
        let Some(i) = self.app.active else { return false };
        if !self.policy.allow(&self.app.drives[i].root, Instant::now(), self.last_input) {
            return false;
        }
        self.start_rescan(i);
        true
    }

    fn selected_path(&self) -> Option<String> {
        let id = self.app.model.as_ref()?.selected()?;
        Some(fmt::display_path(&self.app.tree.as_ref()?.path(id)))
    }
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

/// Arguments for the elevated copy: `--scan "C:"` restarts the scan the user was looking at. `title` is a
/// `DriveInfo::title`, which has no trailing backslash to escape the closing quote.
pub fn relaunch_args(title: Option<&str>) -> String {
    title.map(|t| format!("--scan \"{t}\"")).unwrap_or_default()
}

/// The volume (a `DriveInfo::title`) after `--scan`, if any.
pub fn parse_scan_arg(mut args: impl Iterator<Item = String>) -> Option<String> {
    args.find(|a| a == "--scan")?;
    args.next()
}

/// Starts this exe again with the `runas` verb. False if the user declined the UAC prompt.
fn relaunch_elevated(drive: Option<&str>) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let file = wide(&exe.to_string_lossy());
    let args = wide(&relaunch_args(drive));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.is_ok()
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
        Effect::Commit => {}
        Effect::Scan(i) => {
            with(|w| w.start_scan(i));
        }
        Effect::Filter => {
            with(|w| w.arm_filter());
        }
        Effect::Cancel => {
            with(|w| w.cancel_scan());
        }
        Effect::Rescan(i) => {
            with(|w| w.start_rescan(i));
        }
        Effect::Elevate => {
            let title = with(|w| w.app.active.map(|i| w.app.drives[i].title().to_string())).flatten();
            if relaunch_elevated(title.as_deref()) {
                let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            }
        }
        Effect::Menu(id) => {
            let path = with(|w| w.app.tree.as_ref().map(|t| fmt::display_path(&t.path(id)))).flatten();
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
        w.last_input = Instant::now();
        let e = w.app.handle(make(x, y));
        w.invalidate();
        (e, ())
    }) else {
        return;
    };
    effect(hwnd, e, pos(l));
}

/// Sends `input` to the app, then runs any effect.
fn send(hwnd: HWND, input: Input) {
    let Some(e) = with(|w| {
        w.last_input = Instant::now();
        let e = w.app.handle(input);
        w.invalidate();
        e
    }) else {
        return;
    };
    effect(hwnd, e, (0, 0));
}

/// The clipboard's text, if it has any.
fn paste_text(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while n < 65_536 && *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// One string of the current IME composition, if `flags` says it changed.
fn ime_string(hwnd: HWND, which: IME_COMPOSITION_STRING, flags: IME_COMPOSITION_STRING) -> Option<String> {
    if flags.0 & which.0 == 0 {
        return None;
    }
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.0.is_null() {
            return None;
        }
        let bytes = ImmGetCompositionStringW(himc, which, None, 0).max(0) as usize;
        let mut buf = vec![0u16; bytes / 2];
        if bytes > 0 {
            ImmGetCompositionStringW(himc, which, Some(buf.as_mut_ptr().cast()), bytes as u32);
        }
        let _ = ImmReleaseContext(hwnd, himc);
        Some(String::from_utf16_lossy(&buf))
    }
}

/// The app input a key press stands for. Ctrl+C is handled by the caller.
fn key_input(vk: u16, ctrl: bool, hwnd: HWND) -> Option<Input> {
    if ctrl {
        match vk {
            v if v == b'F' as u16 => return Some(Input::Find),
            v if v == b'M' as u16 => return Some(Input::ToggleMap),
            v if v == b'A' as u16 => return Some(Input::Edit(Edit::SelectAll)),
            v if v == b'V' as u16 => return paste_text(hwnd).map(|s| Input::Edit(Edit::Paste(s))),
            _ => {}
        }
    }
    Some(match vk {
        x if x == VK_BACK.0 => Input::Edit(Edit::Backspace),
        x if x == VK_DELETE.0 => Input::Edit(Edit::Delete),
        x if x == VK_RETURN.0 => Input::Enter,
        x if x == VK_ESCAPE.0 => Input::Escape,
        x if x == VK_TAB.0 => Input::Tab,
        x => Input::Key(key_of(x)?),
    })
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

/// Width of the invisible resize border around the window, in pixels at its DPI.
fn frame_px(hwnd: HWND) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

/// What the window frame is at screen point `lp`, for a window whose title bar is drawn by us: the
/// resize edges and corners (the top border is client area, so Windows no longer reports it), the
/// empty part of the picker bar as caption, anything else as client.
fn hit_test(hwnd: HWND, lp: LPARAM) -> LRESULT {
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

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // The system's caption buttons are drawn over our client area; DWM answers for them.
    let mut dwm = LRESULT(0);
    if unsafe { DwmDefWindowProc(hwnd, msg, wp, lp, &mut dwm) }.as_bool() {
        return dwm;
    }
    match msg {
        // The client area starts at the top of the window: the title bar is ours to draw.
        WM_NCCALCSIZE if wp.0 != 0 => {
            let params = unsafe { &mut *(lp.0 as *mut NCCALCSIZE_PARAMS) };
            let top = params.rgrc[0].top;
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            params.rgrc[0].top = top;
            LRESULT(0)
        }
        WM_NCHITTEST => hit_test(hwnd, lp),
        WM_PAINT => {
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            with(|w| w.render());
            LRESULT(0)
        }
        // Black in the extended frame is what DWM turns into glass (the backdrop under the caption buttons).
        WM_ERASEBKGND => {
            let mut r = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut r) };
            unsafe {
                use windows::Win32::Graphics::Gdi::{FillRect, GetStockObject, BLACK_BRUSH, HBRUSH, HDC};
                FillRect(HDC(wp.0 as _), &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
            LRESULT(1)
        }
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
            with(|w| {
                w.dpi = (wp.0 & 0xffff) as f32;
                w.apply_title_theme();
            });
            let _ = unsafe {
                SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE)
            };
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            dispatch(hwnd, lp, Input::Click);
            with(|w| w.filter_press(lp, false));
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            unsafe { SetCapture(hwnd) };
            dispatch(hwnd, lp, Input::DoubleClick);
            with(|w| w.filter_press(lp, true));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = unsafe { ReleaseCapture() };
            with(|w| w.filter_drag = false);
            dispatch(hwnd, lp, |_, _| Input::Release);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            dispatch(hwnd, lp, Input::RightClick);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if wp.0 & 1 != 0 {
                with(|w| {
                    if w.filter_drag {
                        let at = w.filter_byte(w.dip(lp).0);
                        w.app.handle(Input::Edit(Edit::Place { at, extend: true }));
                        w.invalidate();
                    }
                });
                dispatch(hwnd, lp, Input::Drag);
            } else {
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = unsafe { TrackMouseEvent(&mut t) }; // asks for WM_MOUSELEAVE
                with(|w| {
                    let (x, y) = w.dip(lp);
                    let before = (w.app.hover, w.app.hot, w.app.map_hover);
                    w.app.handle(Input::Hover(x, y));
                    if (w.app.hover, w.app.hot, w.app.map_hover) != before {
                        w.invalidate();
                    }
                    // Moving onto another icon button (or off one) hides the old tip so the next rest asks again.
                    let tip = w.app.tip_at(x, y);
                    if tip != w.tip {
                        w.tip = tip;
                        unsafe { SendMessageW(w.tooltip, TTM_POP, None, None) };
                    }
                    let msg = MSG { hwnd, message: WM_MOUSEMOVE, wParam: wp, lParam: lp, ..Default::default() };
                    unsafe { SendMessageW(w.tooltip, TTM_RELAYEVENT, None, Some(LPARAM(&msg as *const _ as isize))) };
                });
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let mut p = POINT::default();
            let over = unsafe { GetCursorPos(&mut p).is_ok() && ScreenToClient(hwnd, &mut p).as_bool() }
                && with(|w| {
                    let (x, y) = w.dip(LPARAM((p.x as isize & 0xffff) | ((p.y as isize & 0xffff) << 16)));
                    w.app.over_divider(x, y)
                })
                .unwrap_or(false);
            if over {
                unsafe { SetCursor(LoadCursorW(None, IDC_SIZEWE).ok()) };
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
        }
        WM_NOTIFY if unsafe { (*(lp.0 as *const NMHDR)).code } == TTN_GETDISPINFOW => {
            let text = with(|w| w.tip_text());            if let Some(text) = text {
                unsafe { (*(lp.0 as *mut NMTTDISPINFOW)).lpszText = PWSTR(text as *mut u16) };
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(|w| {
                if w.app.hover.take().is_some() | w.app.hot.take().is_some() | w.app.map_hover.take().is_some() {
                    w.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as i16 as i32;
            with(|w| {
                w.last_input = Instant::now();
                w.app.handle(Input::Wheel(-delta / 120 * 3));
                w.invalidate();
            });
            LRESULT(0)
        }
        WM_IME_STARTCOMPOSITION if with(|w| w.app.filter_ui().focus).unwrap_or(false) => {
            // A selection is replaced by what is composed, as with typing.
            if with(|w| w.app.filter_ui().selection().is_some()).unwrap_or(false) {
                send(hwnd, Input::Edit(Edit::Backspace));
            }
            with(|w| {
                w.place_ime();
                w.invalidate();
            });
            LRESULT(0)
        }
        WM_IME_COMPOSITION if with(|w| w.app.filter_ui().focus).unwrap_or(false) => {
            let flags = IME_COMPOSITION_STRING(lp.0 as u32);
            let (done, composing) = (ime_string(hwnd, GCS_RESULTSTR, flags), ime_string(hwnd, GCS_COMPSTR, flags));
            with(|w| w.app.set_preedit(composing.clone().unwrap_or_default()));
            if let Some(s) = done.filter(|s| !s.is_empty()) {
                send(hwnd, Input::Edit(Edit::Paste(s)));
            }
            with(|w| {
                w.place_ime();
                w.invalidate();
            });
            LRESULT(0)
        }
        WM_IME_ENDCOMPOSITION if with(|w| w.app.filter_ui().focus).unwrap_or(false) => {
            with(|w| {
                w.app.set_preedit(String::new());
                w.invalidate();
            });
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(Some(c)) = with(|w| push_utf16(&mut w.high_surrogate, wp.0 as u16)) {
                send(hwnd, Input::Edit(Edit::Char(c)));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            let in_box = with(|w| w.app.filter_ui().focus).unwrap_or(false);
            let picked = with(|w| w.app.filter_ui().selected_text().map(str::to_owned)).flatten();
            let motion = match key_of(wp.0 as u16) {
                Some(Key::Left) => Some(Motion::Left),
                Some(Key::Right) => Some(Motion::Right),
                Some(Key::Home) => Some(Motion::Home),
                Some(Key::End) => Some(Motion::End),
                _ => None,
            };
            if ctrl && (wp.0 == 'C' as usize || wp.0 == 'X' as usize) && in_box && picked.is_some() {
                copy_to_clipboard(hwnd, &picked.unwrap_or_default());
                if wp.0 == 'X' as usize {
                    send(hwnd, Input::Edit(Edit::Backspace));
                }
            } else if ctrl && wp.0 == 'C' as usize {
                if let Some(Some(p)) = with(|w| w.selected_path()) {
                    copy_to_clipboard(hwnd, &p);
                }
            } else if let (true, true, Some(to)) = (shift, in_box, motion) {
                send(hwnd, Input::Edit(Edit::Move { to, extend: true }));
            } else if let Some(input) = key_input(wp.0 as u16, ctrl, hwnd) {
                send(hwnd, input);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FILTER => {
            with(|w| w.run_filter());
            LRESULT(0)
        }
        WM_FILTER => {
            with(|w| w.filter_done());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_PREVIEW => {
            with(|w| w.preview_tick());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_PROGRESS => {
            with(|w| w.poll_progress());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_LIVE => {
            with(|w| w.live_tick());
            LRESULT(0)
        }
        WM_JOURNAL => {
            with(|w| w.journal_event());
            LRESULT(0)
        }
        WM_SCAN_DONE => {
            with(|w| {
                w.in_slot(wp.0, |w| w.scan_done());
                w.sync_timers(); // a kept drive's preview may have ended
            });
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
            with(|w| {
                crate::state::save(&crate::state::State { window: placement_of(hwnd), ..w.app.remembered() });
                w.shutdown();
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// A hidden window at the default size; `run` shows it where it was last left.
fn create_window() -> Result<HWND> {
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
        let _ = SetWindowPos(hwnd, None, 0, 0, (1100 * dpi / 96) as i32, (760 * dpi / 96) as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        Ok(hwnd)
    }
}

/// Shows the window in the remembered rectangle (Windows moves it onto a monitor that still exists), else at the default.
fn show_window(hwnd: HWND, saved: Option<Placement>) {
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
fn placement_of(hwnd: HWND) -> Option<Placement> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
    Some(Placement { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, maximized })
}

/// Runs the app until the window closes.
pub fn run() -> Result<()> {
    let hwnd = create_window()?;
    let win = Win::new(hwnd, crate::drives::list())?;
    eprintln!("plumb: {}", win.gfx.adapter);
    STATE.with(|s| *s.borrow_mut() = Some(win));
    // Placement first: the column widths clamp to the window they land in.
    let saved = crate::state::load();
    show_window(hwnd, saved.window);
    with(|w| w.app.restore(&saved));
    if let Some(title) = parse_scan_arg(std::env::args()) {
        with(|w| {
            if let Some(i) = w.app.drives.iter().position(|d| d.title().eq_ignore_ascii_case(title.trim_end_matches('\\'))) {
                w.start_scan(i);
            }
        });
    }
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

/// Pumps window messages until `cond` holds; errors after `secs`.
fn pump_until(what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut msg = MSG::default();
    while with(|w| !cond(w)) == Some(true) {
        if Instant::now() > deadline {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                format!("timed out waiting for: {what}"),
            ));
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
    Ok(())
}

/// Builds the window hidden, scans a small temp directory on a worker thread through the real
/// message loop and swapchain, and returns "adapter; summary". Never shows a window.
pub fn selftest() -> Result<String> {
    let dir = std::env::temp_dir().join(format!("plumb-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).ok();
    std::fs::write(dir.join("sub").join("f.bin"), vec![1u8; 100_000]).ok();
    let hwnd = create_window()?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    with(|w| w.spawn_scan(0, dir.to_string_lossy().into_owned(), false));
    pump_until("the scan", 30, &|w| matches!(w.app.scan, crate::app::Scan::Idle | crate::app::Scan::Failed(_)))?;
    // A scan that finishes in a kept drive must leave the preview of the drive on screen alone.
    with(|w| {
        w.app.drives.push(DriveInfo { root: "Z:\\".into(), total: 0, free: 0 });
        let other = w.app.drives.len() - 1;
        let root = dir.to_string_lossy().into_owned();
        w.spawn_scan(0, root.clone(), false);
        w.open(other, root); // keeps drive 0's scan going and scans `other`
        w.progress.cancel();
        w.rx = None; // so `other` never finishes by itself: only drive 0 does
        w.preview = Some(Preview::new(preview_channel(4).1));
    });
    pump_until("the kept drive's scan", 30, &|w| w.parked.first().and_then(|d| d.as_ref()).is_some_and(|d| d.app.has_tree()))?;
    if with(|w| w.preview.take().is_some()) != Some(true) {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "a kept drive's scan ending dropped the preview of the drive on screen"));
    }
    with(|w| w.open(0, dir.to_string_lossy().into_owned())); // back to drive 0 and its finished tree
    // A scan keeps building its tree while another drive is shown, and the drive comes back with it.
    let big = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    with(|w| w.spawn_scan(0, big.clone(), false));
    let started = |w: &mut Win| matches!(w.app.scan, crate::app::Scan::Running { dirs, .. } if dirs > 200) && w.app.tree.is_some();
    pump_until("the big scan to get going", 60, &started)?;
    let other = with(|w| {
        w.open(w.app.drives.len() - 1, dir.to_string_lossy().into_owned());
        w.parked.first().and_then(|d| d.as_ref()).and_then(|d| d.app.tree()).map_or(0, |t| t.len())
    })
    .unwrap_or(0);
    let away = |w: &mut Win| w.parked.first().and_then(|d| d.as_ref()).and_then(|d| d.app.tree()).map_or(0, |t| t.len());
    pump_until("the kept scan's tree to grow", 30, &|w| away(w) > other + 1000 || w.parked.first().and_then(|d| d.as_ref()).is_none_or(|d| d.rx.is_none()))?;
    with(|w| w.open(0, big.clone()));
    let back = with(|w| w.app.tree.is_some() && w.app.model.is_some() && (w.preview.is_some() || matches!(w.app.scan, crate::app::Scan::Idle)));
    if back != Some(true) {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "coming back to a drive that is still scanning showed no tree"));
    }
    with(|w| w.cancel_scan());
    with(|w| w.open(0, dir.to_string_lossy().into_owned()));
    pump_until("the small scan again", 30, &|w| matches!(w.app.scan, crate::app::Scan::Idle) && w.app.tree.is_some())?;
    // A scan that finishes while another drive is shown comes back finished: a tree, rows, no scan.
    with(|w| {
        w.spawn_scan(0, dir.to_string_lossy().into_owned(), false);
        w.open(w.app.drives.len() - 1, dir.to_string_lossy().into_owned());
    });
    pump_until("the parked scan to finish", 30, &|w| w.parked.first().and_then(|d| d.as_ref()).is_some_and(|d| d.rx.is_none() && d.app.has_tree()))?;
    with(|w| w.open(0, dir.to_string_lossy().into_owned()));
    let done = with(|w| w.app.tree.is_some() && w.app.model.is_some() && w.preview.is_none() && matches!(w.app.scan, crate::app::Scan::Idle));
    if done != Some(true) {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "a scan that finished while parked came back without a finished tree"));
    }
    // The filter, end to end: type into the box, let the worker scan, check the rows.
    with(|w| {
        w.app.handle(Input::Find);
        for c in "bin".chars() {
            w.app.handle(Input::Edit(Edit::Char(c)));
        }
        w.run_filter();
    });
    pump_until("the filter result", 10, &|w| w.app.filter_installed())?;
    let filtered = with(|w| w.app.model.as_ref().map(|m| m.rows().len())).flatten();
    if filtered != Some(3) {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("filter \"bin\" should show 3 rows (root, sub, f.bin), got {filtered:?}"),
        ));
    }
    let out = with(|w| {
        w.resize();
        let sub = w.app.tree.as_ref().map(|t| t.subtree_size[NodeId(0)]);
        w.try_render().map(|_| {
            format!("{}; {}; root {:?} bytes; filter ok: {} rows", w.gfx.adapter, w.app.summary, sub, filtered.unwrap_or(0))
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out.expect("state present")
}

/// Elevated only. Scans the temp directory's drive in a hidden window, then creates, grows,
/// renames and deletes a file there and checks the tree follows each change within 20 s.
/// Returns a one-line summary. Run it from an administrator prompt: `plumb --selftest-live`.
pub fn selftest_live() -> Result<String> {
    use windows::Win32::Foundation::E_FAIL;
    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
    if !is_elevated() {
        return Err(fail("live updates need an elevated process".into()));
    }
    let dir = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| fail(e.to_string()))?;
    let dir = dir.join(format!("plumb-live-{}", std::process::id()));
    let shown = dir.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let letter = shown.chars().next().unwrap_or('C').to_ascii_uppercase();
    let hwnd = create_window()?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    unsafe { SetTimer(Some(hwnd), 99, 50, None) }; // wakes the message wait so deadlines are checked

    // Blocks in GetMessageW until `cond` holds; every message goes through the real window procedure.
    let wait = |what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut msg = MSG::default();
        while with(|w| !cond(w)) == Some(true) {
            if Instant::now() > deadline {
                return Err(fail(format!("timed out waiting for: {what}")));
            }
            unsafe {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Ok(())
    };
    let size_at = |w: &mut Win, rel: &str| -> Option<u64> {
        let t = w.app.tree.as_ref()?;
        let id = t.find(&format!("{shown}\\{rel}"))?;
        Some(t.subtree_size[id])
    };
    let dir_gone = |w: &mut Win| w.app.tree.as_ref().is_some_and(|t| t.find(&shown).is_none());
    let run = || -> Result<String> {
        let i = with(|w| w.app.drives.iter().position(|d| d.title() == format!("{letter}:"))).flatten();
        let i = i.ok_or_else(|| fail(format!("drive {letter}: is not in the drive list")))?;
        with(|w| w.spawn_scan(i, format!("{letter}:\\"), false));
        wait("the scan and the journal reader", 180, &|w| w.watch.is_some())?;
        std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 1 << 20]).map_err(|e| fail(e.to_string()))?;
        wait("the new file", 20, &|w| size_at(w, "f.bin") == Some(1 << 20))?;
        wait("the live marker", 20, &|w| matches!(&w.app.live, LiveStatus::On { last: Some(_) }))?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 3 << 20]).map_err(|e| fail(e.to_string()))?;
        wait("the grown file", 20, &|w| size_at(w, "f.bin") == Some(3 << 20))?;
        std::fs::rename(dir.join("f.bin"), dir.join("g.bin")).map_err(|e| fail(e.to_string()))?;
        wait("the rename", 20, &|w| size_at(w, "g.bin") == Some(3 << 20) && size_at(w, "f.bin").is_none())?;
        std::fs::remove_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
        wait("the delete", 20, &|w| size_at(w, "g.bin").is_none() && dir_gone(w))?;
        let stale = with(|w| matches!(w.app.live, LiveStatus::Stale(_))).unwrap_or(false);
        if stale {
            return Err(fail("live updates reported stale".into()));
        }
        Ok(format!("{letter}: create, grow, rename and delete followed"))
    };
    let out = run();
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out
}

/// Not elevated. Scans a temp directory in a hidden window, then creates, grows, renames and
/// deletes files there and checks the tree follows each change within 20 s.
/// Run it from a normal prompt: `plumb --selftest-watch`.
pub fn selftest_watch() -> Result<String> {
    use windows::Win32::Foundation::E_FAIL;
    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
    if is_elevated() {
        return Err(fail("the directory watch is for non-elevated runs; the journal serves elevated ones".into()));
    }
    let dir = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| fail(e.to_string()))?;
    let dir2 = dir.join(format!("plumb-watch2-{}", std::process::id()));
    let dir = dir.join(format!("plumb-watch-{}", std::process::id()));
    let shown = dir.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).map_err(|e| fail(e.to_string()))?;
    std::fs::write(dir.join("sub").join("seed.bin"), vec![1u8; 4096]).map_err(|e| fail(e.to_string()))?;
    let hwnd = create_window()?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    unsafe { SetTimer(Some(hwnd), 99, 50, None) }; // wakes the message wait so deadlines are checked
    let wait = |what: &str, secs: u64, cond: &dyn Fn(&mut Win) -> bool| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut msg = MSG::default();
        while with(|w| !cond(w)) == Some(true) {
            if Instant::now() > deadline {
                return Err(fail(format!("timed out waiting for: {what}")));
            }
            unsafe {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Ok(())
    };
    let size_in = |t: &Tree, rel: &str| -> Option<u64> {
        // The root node is named by the whole scanned path, so look up below it, not by path string.
        let (parent, name) = rel.rsplit_once('\\').unwrap_or(("", rel));
        let parent: Vec<u16> = parent.encode_utf16().collect();
        let dir = crate::dirsync::resolve(t, &parent)?;
        let id = t.children(dir).find(|&c| t.name_string(c) == name)?;
        Some(t.subtree_size[id])
    };
    let size_at = |w: &mut Win, rel: &str| -> Option<u64> { size_in(w.app.tree.as_ref()?, rel) };
    let letter = shown.chars().next().unwrap_or('C').to_ascii_uppercase();
    let run = || -> Result<String> {
        let io = |e: std::io::Error| fail(e.to_string());
        let i = with(|w| w.app.drives.iter().position(|d| d.title() == format!("{letter}:"))).flatten();
        let i = i.ok_or_else(|| fail(format!("drive {letter}: is not in the drive list")))?;
        with(|w| w.spawn_scan(i, dir.to_string_lossy().into_owned(), false));
        wait("the scan and the directory watch", 60, &|w| w.watch.is_some())?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 1 << 20]).map_err(io)?;
        wait("the new file", 20, &|w| size_at(w, "f.bin") == Some(1 << 20))?;
        wait("the live marker", 20, &|w| matches!(&w.app.live, LiveStatus::On { last: Some(_) }))?;
        std::fs::write(dir.join("f.bin"), vec![1u8; 3 << 20]).map_err(io)?;
        wait("the grown file", 20, &|w| size_at(w, "f.bin") == Some(3 << 20))?;
        std::fs::rename(dir.join("f.bin"), dir.join("g.bin")).map_err(io)?;
        wait("the rename", 20, &|w| size_at(w, "g.bin") == Some(3 << 20) && size_at(w, "f.bin").is_none())?;
        std::fs::create_dir_all(dir.join("new").join("deep")).map_err(io)?;
        std::fs::write(dir.join("new").join("deep").join("x.bin"), vec![1u8; 8192]).map_err(io)?;
        wait("the new folder", 20, &|w| size_at(w, "new\\deep\\x.bin") == Some(8192))?;
        std::fs::remove_dir_all(dir.join("sub")).map_err(io)?;
        wait("the delete", 20, &|w| size_at(w, "sub").is_none() && size_at(w, "g.bin") == Some(3 << 20))?;
        if with(|w| matches!(w.app.live, LiveStatus::Stale(_))).unwrap_or(false) {
            return Err(fail("live updates reported stale".into()));
        }
        // Cancel a long scan of the whole drive, then replace one with another mid-scan.
        let big = format!("{letter}:\\");
        let big2 = big.clone();
        let running = |w: &mut Win| matches!(w.app.scan, crate::app::Scan::Running { dirs, .. } if dirs > 200);
        with(|w| w.spawn_scan(i, big.clone(), false));
        wait("the drive scan to get going", 60, &running)?;
        with(|w| w.cancel_scan());
        let cancelled = with(|w| matches!(w.app.scan, crate::app::Scan::Cancelled) && w.app.tree.is_none() && w.rx.is_none());
        if cancelled != Some(true) {
            return Err(fail("cancelling did not leave a cancelled, empty state".into()));
        }
        with(|w| w.spawn_scan(i, big, false));
        wait("the drive scan to get going again", 60, &running)?;
        with(|w| w.spawn_scan(i, dir.to_string_lossy().into_owned(), false));
        wait("the replacement scan", 60, &|w| w.watch.is_some() && size_at(w, "g.bin") == Some(3 << 20))?;
        // A second drive (made up; its root is another temp directory). Leaving the first keeps its
        // tree and watch: a change made while it is away shows when it comes back, with no rescan.
        let j = with(|w| {
            w.app.drives.push(DriveInfo { root: "Z:\\".into(), total: 0, free: 0 });
            w.app.drives.len() - 1
        })
        .ok_or_else(|| fail("no window".into()))?;
        std::fs::create_dir_all(&dir2).map_err(io)?;
        std::fs::write(dir2.join("x.bin"), vec![1u8; 8192]).map_err(io)?;
        let root2 = || dir2.to_string_lossy().into_owned();
        with(|w| w.open(j, root2()));
        wait("the second drive's scan", 60, &|w| w.watch.is_some() && size_at(w, "x.bin") == Some(8192))?;
        if with(|w| w.app.marks.get(i) == Some(&Mark::Kept)) != Some(true) {
            return Err(fail("the drive that is away is not marked as kept".into()));
        }
        std::fs::write(dir.join("h.bin"), vec![1u8; 2 << 20]).map_err(io)?;
        let away = |w: &mut Win, rel: &str| w.parked.get(i).and_then(|d| d.as_ref()).and_then(|d| d.app.tree()).and_then(|t| size_in(t, rel));
        wait("the change to reach the drive that is away", 20, &|w| away(w, "h.bin") == Some(2 << 20))?;
        with(|w| w.open(i, dir.to_string_lossy().into_owned()));
        let back = with(|w| w.rx.is_none() && w.watch.is_some() && size_at(w, "h.bin") == Some(2 << 20));
        if back != Some(true) {
            return Err(fail("coming back to a drive did not show its kept, current tree".into()));
        }
        // A scan in flight keeps going while another drive is shown, and is still running on return.
        with(|w| w.spawn_scan(i, big2.clone(), false));
        wait("the drive scan to get going once more", 60, &running)?;
        with(|w| w.open(j, root2()));
        let kept = with(|w| {
            w.watch.is_some()
                && size_at(w, "x.bin") == Some(8192)
                && w.parked.get(i).and_then(|d| d.as_ref()).is_some_and(|d| d.rx.is_some())
                && w.app.marks.get(i) == Some(&Mark::Scanning)
        });
        if kept != Some(true) {
            return Err(fail("the scan did not keep running while another drive was shown".into()));
        }
        with(|w| w.open(i, big2.clone()));
        let resumed = with(|w| w.rx.is_some() && matches!(w.app.scan, crate::app::Scan::Running { .. }));
        if resumed != Some(true) {
            return Err(fail("the running scan was not on screen after switching back".into()));
        }
        with(|w| w.cancel_scan());
        Ok("create, grow, rename, new folder and delete followed; cancel works; drives keep their trees, watches and scans when switched".to_string())
    };
    let out = run();
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_argument_round_trips() {
        // The command line the elevated copy gets, after the OS strips the quotes.
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter();
        assert_eq!(relaunch_args(Some("C:")), "--scan \"C:\"");
        assert_eq!(relaunch_args(Some("D:\\Mounts\\Data")), "--scan \"D:\\Mounts\\Data\"", "no trailing backslash to escape the quote");
        assert_eq!(parse_scan_arg(args(&["plumb.exe", "--scan", "D:\\Mounts\\Data"])), Some("D:\\Mounts\\Data".to_string()));
        assert_eq!(parse_scan_arg(args(&["plumb.exe"])), None);
        assert_eq!(parse_scan_arg(args(&["plumb.exe", "--scan"])), None);
        assert_eq!(relaunch_args(None), "");
    }

    #[test]
    fn summary_lists_scanner_counts_skips_and_notes() {
        let s = summary(Used::Mft, 1234, Duration::from_millis(2500), Some(3), Some("note"));
        assert_eq!(s, "MFT scan: 1,234 files in 2.5s, 3 records skipped (note)");
        assert_eq!(summary(Used::Walk, 5, Duration::from_secs(1), None, None), "Directory walk: 5 files in 1.0s");
    }
}
