//! The row context menu and the shell actions behind it (reveal, properties, copy).
use super::*;

pub(super) const CMD_REVEAL: usize = 1;

pub(super) const CMD_COPY: usize = 2;

pub(super) const CMD_PROPS: usize = 3;

pub(super) const CMD_WHO: usize = 5;

pub(super) const CMD_MARK: usize = 4;

pub(super) const CMD_END: usize = 6;

pub(super) const CMD_END_TREE: usize = 7;

pub(super) const CMD_PAUSE: usize = 8;

/// Shows the context menu at the cursor and runs the chosen command. Called with no borrow held,
/// because `TrackPopupMenu` runs its own message loop.
pub(super) fn context_menu(hwnd: HWND, menu: RowMenu, at: (i32, i32)) {
    let RowMenu { raw, path, mark, who, end, pause } = menu;
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let files = !path.is_empty(); // a process whose executable is unknown has none
        if files {
            let _ = AppendMenuW(menu, MF_STRING, CMD_REVEAL, w!("Reveal in Explorer"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_COPY, w!("Copy path"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_PROPS, w!("Properties"));
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
        if files || end.is_some() || who {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        match &mark {
            MarkState::Mark => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark for deletion\tDel"));
            }
            MarkState::Unmark => {
                let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark\tDel"));
            }
            MarkState::Refused(why) => {
                let text = wide(&format!("Cannot mark: {why}"));
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, CMD_MARK, PCWSTR(text.as_ptr()));
            }
        }
        let mut pt = POINT { x: at.0, y: at.1 };
        let _ = ClientToScreen(hwnd, &mut pt);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            CMD_REVEAL => reveal(&path),
            CMD_COPY => copy_to_clipboard(hwnd, &path),
            CMD_PROPS => properties(&path),
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
                    let out = plumb_scan::procs::pause(v.pid, v.created, suspended);
                    with(|w| {
                        match out {
                            Ok(()) => w.run.procs.as_ref().map_or((), |p| p.kick()),
                            Err(why) => {
                                let (verb, past) = if suspended { ("resume", "resumed") } else { ("suspend", "suspended") };
                                let failure = process_failure(verb, past, &[(v.clone(), why)], 1);
                                w.app.report(failure.outcome(""));
                                w.ask(Modal::process_failed(&failure));
                            }
                        }
                        w.invalidate();
                    });
                }
            }
            CMD_MARK => {
                with(|w| {
                    w.app.toggle_mark_path(&raw);
                    w.invalidate();
                });
            }
            _ => {}
        }
    }
}

pub(super) fn reveal(path: &str) {
    let args = wide(&format!("/select,\"{path}\""));
    unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
}

pub(super) fn properties(path: &str) {
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

pub(super) fn copy_to_clipboard(hwnd: HWND, text: &str) {
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
