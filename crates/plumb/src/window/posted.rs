//! The messages the worker threads post to the window when they have a result, and what the window does with each.
use super::*;

/// Handles one of `WM_HELD`, `WM_FILTER`, `WM_JOURNAL`, `WM_DELETED`, `WM_HOLDERS`, `WM_PROCS`, `WM_SCAN_DONE`. The `lp` of the
/// ones that carry a result is a leaked `Box`, taken back here.
pub(super) fn on_posted(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
    match msg {
        WM_HELD => {
            let (epoch, result) = *unsafe { Box::from_raw(lp.0 as *mut (u64, std::result::Result<plumb_scan::handles::HandleList, plumb_scan::handles::HandleError>)) };
            with(|w| {
                w.app.held_done(epoch, result);
                w.invalidate();
            });
        }
        WM_FILTER => {
            with(|w| w.filter_done());
        }
        WM_JOURNAL => {
            with(|w| w.journal_event());
        }
        WM_DELETED => {
            let raw = lp.0 as *mut (usize, bool, Vec<trash::Outcome>);
            let done = with(|w| {
                let (view, permanent, out) = *unsafe { Box::from_raw(raw) };
                // What failed and why, to retry only those.
                if let Some((failure, paths)) = w.finish_delete(view, out) {
                    w.ask(Modal::failed(view, permanent, &failure, paths));
                    w.invalidate();
                }
            });
            // The state was borrowed (the box is gone only once taken): try again rather than lose the result.
            if done.is_none() {
                let _ = unsafe { PostMessageW(Some(hwnd), WM_DELETED, WPARAM(0), lp) };
            }
        }
        WM_HOLDERS => {
            let (path, found) = *unsafe { Box::from_raw(lp.0 as *mut (String, Vec<plumb_scan::holders::Holder>)) };
            with(|w| {
                w.ask(Modal::holders(&path, &found));
                w.invalidate();
            });
        }
        WM_PROCS => {
            let snap = unsafe { Box::from_raw(lp.0 as *mut procs::Snapshot) };
            with(move |w| {
                let view = snap.view;
                w.in_slot(view, |w| w.procs_update(*snap));
            });
        }
        WM_SCAN_DONE => {
            with(|w| {
                w.in_slot(wp.0, |w| w.scan_done());
                w.sync_timers(); // a kept view's preview may have ended
            });
        }
        _ => {}
    }
}
