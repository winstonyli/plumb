//! `WM_TIMER`: what each of the window's timers does when it fires.
use super::*;

/// Runs the work of timer `id` (a `TIMER_*`); an id that is not one of ours does nothing.
pub(super) fn on_timer(hwnd: HWND, id: usize) {
    match id {
        TIMER_DRAG => {
            // The button is down on a row: replay the pointer's position, so holding it past an edge keeps scrolling. A
            // button let go after the window lost the capture sends no WM_LBUTTONUP, so end the drag here.
            // The asynchronous state: this thread's own input state never sees a release over another window. It is
            // physical, so with the buttons swapped the primary button is the right one.
            let primary = primary_button(unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0);
            let down = unsafe { GetAsyncKeyState(primary.0 as i32) } < 0;
            let mut p = POINT::default();
            let at = if unsafe { GetCursorPos(&mut p).is_ok() && ScreenToClient(hwnd, &mut p).as_bool() } {
                with(|w| (p.x as f32 * 96.0 / w.dpi, p.y as f32 * 96.0 / w.dpi - w.inset))
            } else {
                None
            };
            if let Some(input) = drag_step(down, at) {
                send(hwnd, input);
            }
        }
        TIMER_HELD => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_HELD) };
            if let Some(Some((epoch, pid))) = with(|w| w.app.held_request()) {
                spawn_held(hwnd, epoch, pid);
            }
        }
        TIMER_FILTER => {
            with(|w| w.run_filter());
        }
        TIMER_PREVIEW => {
            with(|w| w.preview_tick());
        }
        TIMER_PROGRESS => {
            with(|w| w.poll_progress());
        }
        TIMER_TOAST => {
            with(|w| {
                w.app.toast_tick(Instant::now());
                w.sync_timers();
                w.invalidate();
            });
        }
        TIMER_FLASH => {
            with(|w| w.flash_tick());
        }
        TIMER_LIVE => {
            with(|w| w.live_tick());
        }
        _ => {}
    }
}
