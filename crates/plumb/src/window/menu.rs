//! The row context menu and the shell actions behind it (reveal, properties, copy).
use super::*;
use crate::outcome::{Failure, Outcome};

pub(super) const CMD_REVEAL: usize = 1;

pub(super) const CMD_COPY: usize = 2;

pub(super) const CMD_PROPS: usize = 3;

pub(super) const CMD_WHO: usize = 5;

pub(super) const CMD_MARK: usize = 4;

pub(super) const CMD_END: usize = 6;

pub(super) const CMD_END_TREE: usize = 7;

pub(super) const CMD_PAUSE: usize = 8;

pub(super) const CMD_SUSPEND_TREE: usize = 9;

pub(super) const CMD_RESUME_TREE: usize = 10;

/// Suspends `list` (or, with `resume`, resumes it) one process after another, then reports what failed as one `Failure`.
/// Called with no borrow held: each call goes to the system.
pub(super) fn pause_all(list: Vec<crate::modal::Victim>, resume: bool) {
    let failed: Vec<_> = list.iter().filter_map(|v| plumb_scan::procs::pause(v.pid, v.created, resume).err().map(|why| (v.clone(), why))).collect();
    let (verb, past) = if resume { ("resume", "resumed") } else { ("suspend", "suspended") };
    with(|w| {
        if failed.is_empty() {
            if let Some(p) = &w.run.procs {
                p.kick();
            }
        } else {
            let failure = process_failure(verb, past, &failed, list.len());
            w.app.report(failure.outcome(""));
            w.sync_timers();
            w.ask(Modal::process_failed(&failure));
        }
        w.invalidate();
    });
}

/// Shows the context menu at the cursor and runs the chosen command. Called with no borrow held,
/// because `TrackPopupMenu` runs its own message loop.
pub(super) fn context_menu(hwnd: HWND, menu: RowMenu, at: (i32, i32)) {
    let RowMenu { raw, paths, count, mark, who, end, pause, suspend_tree, resume_tree } = menu;
    let path = paths.first().cloned().unwrap_or_default(); // the single row's, when count is 1
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let files = count == 1 && !path.is_empty(); // a process whose executable is unknown has none
        let copy_set = count > 1 && !paths.is_empty();
        if files {
            let _ = AppendMenuW(menu, MF_STRING, CMD_REVEAL, w!("Reveal in Explorer"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_COPY, w!("Copy path"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_PROPS, w!("Properties"));
        }
        if copy_set {
            let text = wide(&format!("Copy paths ({})", paths.len())); // the rows with a path, not every picked row
            let _ = AppendMenuW(menu, MF_STRING, CMD_COPY, PCWSTR(text.as_ptr()));
        }
        if who {
            let _ = AppendMenuW(menu, MF_STRING, CMD_WHO, w!("Who is using this"));
        }
        if let Some(list) = &end {
            let _ = AppendMenuW(menu, MF_STRING, CMD_END, w!("End process"));
            if list.len() > 1 {
                let text = wide(&format!("End process tree ({})", list.len()));
                let _ = AppendMenuW(menu, MF_STRING, CMD_END_TREE, PCWSTR(text.as_ptr()));
            }
        }
        if let Some((_, suspended)) = &pause {
            let _ = AppendMenuW(menu, MF_STRING, CMD_PAUSE, if *suspended { w!("Resume process") } else { w!("Suspend process") });
        }
        for (cmd, verb, list) in [(CMD_SUSPEND_TREE, "Suspend", &suspend_tree), (CMD_RESUME_TREE, "Resume", &resume_tree)] {
            if !list.is_empty() {
                let text = wide(&format!("{verb} process tree ({})", list.len()));
                let _ = AppendMenuW(menu, MF_STRING, cmd, PCWSTR(text.as_ptr()));
            }
        }
        if files || copy_set || end.is_some() || who {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        match (&mark, count > 1) {
            (MarkState::Mark, false) => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark for deletion\tDel"));
            }
            (MarkState::Mark, true) => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark selected\tDel"));
            }
            (MarkState::Unmark, false) => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark\tDel"));
            }
            (MarkState::Unmark, true) => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark selected\tDel"));
            }
            (MarkState::Refused(why), _) => {
                let text = wide(&format!("Cannot mark: {why}"));
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, CMD_MARK, PCWSTR(text.as_ptr()));
            }
        }
        let mut pt = POINT { x: at.0, y: at.1 };
        let _ = ClientToScreen(hwnd, &mut pt);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            CMD_REVEAL => shell_result("reveal", &path, reveal(&path)),
            CMD_COPY => copy_rows(hwnd, &paths.join("\r\n")),
            CMD_PROPS => shell_result("show properties of", &path, properties(&path)),
            CMD_WHO => {
                // Restart Manager can be slow: ask on a worker, answer by message.
                let owner = hwnd.0 as isize;
                std::thread::spawn(move || {
                    let found = plumb_scan::holders::holders(&path);
                    let boxed = Box::into_raw(Box::new((path, found)));
                    if PostMessageW(Some(HWND(owner as *mut _)), WM_HOLDERS, WPARAM(0), LPARAM(boxed as isize)).is_err() {
                        drop(Box::from_raw(boxed));
                    }
                });
            }
            CMD_END | CMD_END_TREE => {
                if let Some(mut list) = end {
                    if cmd.0 as usize == CMD_END {
                        list.truncate(1);
                    }
                    with(|w| {
                        w.ask(Modal::end(list));
                        w.invalidate();
                    });
                }
            }
            CMD_PAUSE => {
                if let Some((v, suspended)) = pause {
                    pause_all(vec![v], suspended);
                }
            }
            CMD_SUSPEND_TREE => pause_all(suspend_tree, false),
            CMD_RESUME_TREE => pause_all(resume_tree, true),
            CMD_MARK => {
                with(|w| {
                    if count > 1 {
                        w.app.mark_selected();
                    } else {
                        w.app.toggle_mark_path(&raw);
                    }
                    w.invalidate();
                });
            }
            _ => {}
        }
    }
}

/// Shows `outcome` as the toast; called with no borrow held.
pub(super) fn say(outcome: Outcome) {
    with(|w| {
        w.app.report(outcome);
        w.sync_timers();
        w.invalidate();
    });
}

/// Reports a failed shell action on `path` as the shared "could not <verb> <path>: why" line.
fn shell_result(verb: &str, path: &str, result: std::result::Result<(), String>) {
    if let Err(why) = result {
        say(Failure::new(verb, verb, &[(path.to_string(), why)], 1, "").outcome(""));
    }
}

pub(super) fn reveal(path: &str) -> std::result::Result<(), String> {
    if !std::path::Path::new(path).exists() {
        return Err("it no longer exists".into());
    }
    let args = wide(&format!("/select,\"{path}\""));
    // `ShellExecuteW` reports success with a value above 32.
    let r = unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
    if r.0 as isize > 32 { Ok(()) } else { Err(format!("Explorer did not start (code {})", r.0 as isize)) }
}

fn properties(path: &str) -> std::result::Result<(), String> {
    let p = wide(path);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        lpVerb: w!("properties"),
        lpFile: PCWSTR(p.as_ptr()),
        nShow: SW_SHOW.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| e.message())
}

/// Copies `text` and says "copied N items", or why it could not.
pub(super) fn copy_rows(hwnd: HWND, text: &str) {
    say(match copy_to_clipboard(hwnd, text) {
        Ok(()) => Outcome::ok(format!("copied {}", crate::fmt::noun(text.lines().count() as u64, "item", "items"))),
        Err(why) => Outcome::problem(format!("could not copy: {why}")),
    });
}

pub(super) fn copy_to_clipboard(hwnd: HWND, text: &str) -> std::result::Result<(), String> {
    let u = wide(text);
    unsafe {
        OpenClipboard(Some(hwnd)).map_err(|_| "another program is using the clipboard".to_string())?;
        let _ = EmptyClipboard();
        let result = (|| {
            let h = GlobalAlloc(GMEM_MOVEABLE, u.len() * 2).map_err(|e| e.message())?;
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                return Err("out of memory".to_string());
            }
            std::ptr::copy_nonoverlapping(u.as_ptr(), p, u.len());
            let _ = GlobalUnlock(h);
            SetClipboardData(13, Some(HANDLE(h.0))).map(|_| ()).map_err(|e| e.message()) // CF_UNICODETEXT
        })();
        let _ = CloseClipboard();
        result
    }
}