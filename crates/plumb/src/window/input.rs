//! The keyboard, mouse and IME input helpers: message parameters to `Input`, the clipboard paste, key maps.
use super::*;

pub(super) fn pos(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

/// Sends `input` to the app, then runs any effect and repaints.
pub(super) fn dispatch(hwnd: HWND, l: LPARAM, make: impl FnOnce(f32, f32) -> Input) {
    let Some((e, _)) = with(|w| {
        let (x, y) = w.dip(l);
        w.last_input = Instant::now();
        let e = w.app.handle(make(x, y));
        w.sync_timers();
        w.invalidate();
        (e, ())
    }) else {
        return;
    };
    effect(hwnd, e, pos(l));
}

/// Sends `input` to the app, then runs any effect.
pub(super) fn send(hwnd: HWND, input: Input) {
    let Some(e) = with(|w| {
        w.last_input = Instant::now();
        let e = w.app.handle(input);
        w.sync_timers();
        w.invalidate();
        e
    }) else {
        return;
    };
    effect(hwnd, e, (0, 0));
}

/// The clipboard's text, if it has any.
pub(super) fn paste_text(hwnd: HWND) -> Option<String> {
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
pub(super) fn ime_string(hwnd: HWND, which: IME_COMPOSITION_STRING, flags: IME_COMPOSITION_STRING) -> Option<String> {
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
pub(super) fn key_input(vk: u16, mods: Mods, hwnd: HWND) -> Option<Input> {
    if mods.ctrl {
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
        x if x == VK_F5.0 => Input::Refresh,
        x => Input::Key(key_of(x)?, mods),
    })
}

/// Keys that act once per press: a held key must not flip a mark or a folder, or queue a rescan, on every repeat.
pub(super) fn once_per_press(vk: u16, ctrl: bool) -> bool {
    [VK_DELETE.0, VK_RETURN.0, VK_F5.0, VK_BACK.0].contains(&vk) || (ctrl && vk == b'A' as u16)
}

/// Client pixels of a row's bottom-left corner: where the keyboard's context menu opens. `r` is in DIPs below the title bar's `inset`.
pub(super) fn menu_anchor(r: crate::layout::Rect, dpi: f32, inset: f32) -> (i32, i32) {
    let k = dpi / 96.0;
    ((r.x * k) as i32, ((r.y + r.h + inset) * k) as i32)
}

/// Shift+F10 is the keyboard's context menu; any other F10 combination is Windows' (the menu bar, the system menu).
pub(super) fn is_key_menu_chord(vk: usize, shift: bool) -> bool {
    vk == VK_F10.0 as usize && shift
}

/// The button that starts a drag: the physical left one, or the right one when the buttons are swapped.
pub(super) fn primary_button(swapped: bool) -> windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY {
    if swapped { VK_RBUTTON } else { VK_LBUTTON }
}

/// What the drag timer sends: the pointer's place while the button is down, `Release` once it is up (a release over another
/// window sends no `WM_LBUTTONUP`). Nothing when the button is down but the pointer can't be read.
pub(super) fn drag_step(down: bool, at: Option<(f32, f32)>) -> Option<Input> {
    if down { at.map(|(x, y)| Input::Drag(x, y)) } else { Some(Input::Release) }
}

/// Opens the context menu of the focus row from the keyboard (Apps key, Shift+F10), at that row's left edge.
pub(super) fn key_menu(hwnd: HWND) {
    let Some((e, at)) = with(|w| {
        w.last_input = Instant::now();
        let e = w.app.handle(Input::ContextMenu);
        let at = w.app.focus_row_rect().map_or((0, 0), |r| menu_anchor(r, w.dpi, w.inset));
        w.invalidate();
        (e, at)
    }) else {
        return;
    };
    effect(hwnd, e, at);
}

/// The Shift and Ctrl keys as they are now.
pub(super) fn mods_now() -> Mods {
    let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    Mods { shift: down(VK_SHIFT.0 as i32), ctrl: down(VK_CONTROL.0 as i32) }
}

pub(super) fn key_of(vk: u16) -> Option<Key> {
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
