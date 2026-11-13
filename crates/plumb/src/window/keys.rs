//! Keyboard and text messages: keys, characters and the IME composition of the filter box.
use super::*;

/// Handles one of the keyboard messages; `None` leaves it to `DefWindowProc` (an IME message while the filter box's
/// focus is elsewhere, or an Alt or F10 combination that is not Shift+F10).
pub(super) fn on_key(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_IME_STARTCOMPOSITION if with(|w| w.app.filter_ui().focus).unwrap_or(false) => {
            // A selection is replaced by what is composed, as with typing.
            if with(|w| w.app.filter_ui().selection().is_some()).unwrap_or(false) {
                send(hwnd, Input::Edit(Edit::Backspace));
            }
            with(|w| {
                w.place_ime();
                w.invalidate();
            });
            Some(LRESULT(0))
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
            Some(LRESULT(0))
        }
        WM_IME_ENDCOMPOSITION if with(|w| w.app.filter_ui().focus).unwrap_or(false) => {
            with(|w| {
                w.app.set_preedit(String::new());
                w.invalidate();
            });
            Some(LRESULT(0))
        }
        WM_CHAR => {
            if let Some(Some(c)) = with(|w| push_utf16(&mut w.high_surrogate, wp.0 as u16)) {
                send(hwnd, Input::Edit(Edit::Char(c)));
            }
            Some(LRESULT(0))
        }
        WM_KEYDOWN => {
            let Mods { shift, ctrl } = mods_now();
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
                if let Err(why) = copy_to_clipboard(hwnd, &picked.unwrap_or_default()) {
                    menu::say(crate::outcome::Outcome::problem(format!("could not copy: {why}")));
                }
                if wp.0 == 'X' as usize {
                    send(hwnd, Input::Edit(Edit::Backspace));
                }
            } else if ctrl && wp.0 == 'C' as usize {
                if let Some(Some(text)) = with(|w| w.app.copy_text()) {
                    copy_rows(hwnd, &text);
                }
            } else if let (true, true, Some(to)) = (shift, in_box, motion) {
                send(hwnd, Input::Edit(Edit::Move { to, extend: true }));
            } else if lp.0 & (1 << 30) != 0 && !in_box && once_per_press(wp.0 as u16, ctrl) {
                // A held Del, Enter, F5, Backspace or Ctrl+A acts once, not on every repeat.
            } else if wp.0 == VK_APPS.0 as usize {
                if lp.0 & (1 << 30) == 0 {
                    key_menu(hwnd);
                }
            } else if let Some(input) = key_input(wp.0 as u16, Mods { shift, ctrl }, hwnd) {
                send(hwnd, input);
            }
            Some(LRESULT(0))
        }
        // F10 is a system key, so Shift+F10, the keyboard's context menu, arrives here. Every other Alt or F10 combination is
        // Windows': it falls through to `DefWindowProc`.
        WM_SYSKEYDOWN if is_key_menu_chord(wp.0, unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) => {
            if lp.0 & (1 << 30) == 0 {
                key_menu(hwnd);
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}
