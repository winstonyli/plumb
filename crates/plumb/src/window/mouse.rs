//! Mouse messages: presses, drags, moves, the wheel and the resize cursor, translated into `Input`s for the app.
use super::*;

/// Handles one of the mouse messages; `None` leaves it to `DefWindowProc`.
pub(super) fn on_mouse(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            let mods = mods_now();
            dispatch(hwnd, lp, |x, y| Input::Click(x, y, mods));
            with(|w| w.filter_press(lp, false));
            Some(LRESULT(0))
        }
        WM_LBUTTONDBLCLK => {
            unsafe { SetCapture(hwnd) };
            let mods = mods_now();
            if mods.shift || mods.ctrl {
                // Two quick presses with Shift or Ctrl held are two selections; they must not open the folder.
                dispatch(hwnd, lp, |x, y| Input::Click(x, y, mods));
            } else {
                dispatch(hwnd, lp, Input::DoubleClick);
            }
            with(|w| w.filter_press(lp, true));
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let _ = unsafe { ReleaseCapture() };
            dispatch(hwnd, lp, |_, _| Input::Release);
            Some(LRESULT(0))
        }
        WM_RBUTTONUP => {
            dispatch(hwnd, lp, Input::RightClick);
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE => {
            if wp.0 & 1 != 0 {
                with(|w| {
                    if w.app.filter_dragging() {
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
                    let before = (w.app.hover, w.app.hot, w.app.map_hover, w.app.thumb_hover);
                    w.app.handle(Input::Hover(x, y));
                    if (w.app.hover, w.app.hot, w.app.map_hover, w.app.thumb_hover) != before {
                        w.invalidate();
                    }
                    // Moving onto another target (or off one) hides the old tip so the next rest asks again; the same
                    // target whose text changed (a scan adds to its totals) is refreshed in place, not hidden.
                    let key = w.app.tip_target(x, y, &mut w.tip_scratch);
                    if key != w.tip {
                        w.tip = key;
                        unsafe { SendMessageW(w.tooltip, TTM_POP, None, None) };
                    } else if key.is_some() && w.tip_scratch != w.tip_last {
                        w.refresh_tip();
                    }
                    let msg = MSG { hwnd, message: WM_MOUSEMOVE, wParam: wp, lParam: lp, ..Default::default() };
                    unsafe { SendMessageW(w.tooltip, TTM_RELAYEVENT, None, Some(LPARAM(&msg as *const _ as isize))) };
                });
            }
            Some(LRESULT(0))
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
                Some(LRESULT(1))
            } else {
                Some(unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
            }
        }
        WM_MOUSELEAVE => {
            with(|w| {
                if w.app.hover.take().is_some() | w.app.hot.take().is_some() | w.app.map_hover.take().is_some() | std::mem::take(&mut w.app.thumb_hover) {
                    w.invalidate();
                }
            });
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as i16 as i32;
            let mut lines = 3u32;
            let _ = unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some(&mut lines as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
            with(|w| {
                w.last_input = Instant::now();
                // The setting is lines per notch, or (u32::MAX, WHEEL_PAGESCROLL) a whole page.
                let per_notch = if lines == u32::MAX { w.app.layout.rows_visible() as i32 } else { lines as i32 };
                let rows = wheel_rows(&mut w.wheel_rem, delta, per_notch);
                // The wheel message carries screen coordinates.
                let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
                let _ = unsafe { ScreenToClient(w.hwnd, &mut p) };
                let k = 96.0 / w.dpi;
                w.app.handle(Input::Wheel(rows, p.x as f32 * k, p.y as f32 * k - w.inset));
                w.invalidate();
            });
            Some(LRESULT(0))
        }
        _ => None,
    }
}
