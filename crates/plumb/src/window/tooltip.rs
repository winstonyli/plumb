//! The tooltip control: created over the whole window, asking for its text when the pointer rests.
use super::*;

/// The tooltip's frame colour (a `COLORREF`), the theme's border; read while the tooltip paints.
pub(super) static TIP_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Win {
    /// A tooltip control over the whole window, asking for its text (`WM_NOTIFY`) when the pointer rests.
    pub(super) fn make_tooltip(hwnd: HWND) -> HWND {
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
            // Goes away after a few seconds of stillness, as system tooltips do (left alone it stayed up for minutes).
            SendMessageW(tip, TTM_SETDELAYTIME, Some(WPARAM(TTDT_AUTOPOP as usize)), Some(LPARAM(6000)));
            tip
        }
    }

    /// Has the tooltip control ask for its text again (`tip_text`), keeping it where it is.
    pub(super) fn refresh_tip(&self) {
        let tool = TTTOOLINFOW {
            cbSize: std::mem::offset_of!(TTTOOLINFOW, lParam) as u32,
            uFlags: TTF_IDISHWND,
            hwnd: self.hwnd,
            uId: self.hwnd.0 as usize,
            lpszText: PWSTR(usize::MAX as *mut u16), // LPSTR_TEXTCALLBACK
            ..Default::default()
        };
        unsafe { SendMessageW(self.tooltip, TTM_UPDATETIPTEXTW, None, Some(LPARAM(&tool as *const _ as isize))) };
    }

    /// The tooltip text under the pointer, as the wide string the control reads; empty for none.
    pub(super) fn tip_text(&mut self) -> *const u16 {
        let mut p = POINT::default();
        let found = unsafe { GetCursorPos(&mut p).is_ok() && ScreenToClient(self.hwnd, &mut p).as_bool() }
            && self.app.tip_at(p.x as f32 * 96.0 / self.dpi, p.y as f32 * 96.0 / self.dpi - self.inset, &mut self.tip_last);
        if !found {
            self.tip_last.clear();
        }
        self.tip_buf.clear();
        self.tip_buf.extend(self.tip_last.encode_utf16());
        self.tip_buf.push(0);
        self.tip_buf.as_ptr()
    }
}
