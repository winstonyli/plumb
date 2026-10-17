//! Hidden-window end-to-end checks driven through the real message loop: `--selftest`, `--selftest-watch`, `--selftest-live`.
use super::*;

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
        w.app.drives.push(DriveInfo { root: "Z:\\".into(), total: 0, free: 0, kind: crate::app::ViewKind::Disk });
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
            w.app.drives.push(DriveInfo { root: "Z:\\".into(), total: 0, free: 0, kind: crate::app::ViewKind::Disk });
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

/// Answers the next dialogs this process shows, one button id each, from a helper thread; true if all were answered.
fn answer_dialogs(ids: Vec<i32>) -> std::thread::JoinHandle<bool> {
    use windows::Win32::UI::Controls::TDM_CLICK_BUTTON;
    let pid = std::process::id();
    std::thread::spawn(move || {
        let mut last = 0isize;
        for id in ids {
            let deadline = Instant::now() + Duration::from_secs(15);
            let found = loop {
                let mut seen = (0isize, pid, last);
                unsafe extern "system" fn each(h: HWND, p: LPARAM) -> BOOL {
                    let seen = unsafe { &mut *(p.0 as *mut (isize, u32, isize)) };
                    let (mut pid, mut class) = (0u32, [0u16; 16]);
                    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
                    let n = unsafe { GetClassNameW(h, &mut class) } as usize;
                    if pid == seen.1 && h.0 as isize != seen.2 && unsafe { IsWindowVisible(h) }.as_bool() && String::from_utf16_lossy(&class[..n]) == "#32770" {
                        seen.0 = h.0 as isize;
                        return false.into();
                    }
                    true.into()
                }
                let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut seen as *mut _ as isize)) };
                if seen.0 != 0 {
                    break Some(seen.0);
                }
                if Instant::now() > deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let Some(h) = found else { return false };
            last = h;
            // For a window capture from outside (`PLUMB_SELFTEST_PAUSE_MS`): the dialog stays up this long before it is answered.
            if let Some(ms) = std::env::var("PLUMB_SELFTEST_PAUSE_MS").ok().and_then(|v| v.parse().ok()) {
                std::thread::sleep(Duration::from_millis(ms));
            }
            unsafe { SendMessageW(HWND(h as *mut _), TDM_CLICK_BUTTON.0 as u32, Some(WPARAM(id as usize)), None) };
        }
        true
    })
}

/// Presses a button of the shell's own operation window (a DirectUI window the dialog helpers cannot click) through UI
/// Automation, found by its automation id; true once pressed.
fn press_shell_button(automation_id: &'static str) -> std::thread::JoinHandle<bool> {
    let pid = std::process::id();
    std::thread::spawn(move || {
        let script = format!(
            "Add-Type -AssemblyName UIAutomationClient; $A = [System.Windows.Automation.AutomationElement]; \
             $c = New-Object System.Windows.Automation.AndCondition((New-Object System.Windows.Automation.PropertyCondition($A::ProcessIdProperty, {pid})), \
             (New-Object System.Windows.Automation.PropertyCondition($A::AutomationIdProperty, '{automation_id}'))); \
             $end = (Get-Date).AddSeconds(20); \
             while ((Get-Date) -lt $end) {{ $b = $A::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c); \
             if ($b) {{ $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); 'pressed'; exit }}; Start-Sleep -Milliseconds 100 }}"
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "pressed")
    })
}

/// Not elevated. Marks files in a scratch directory, answers the real commit dialogs, and checks the disk, the
/// basket and the tree: Cancel keeps everything, a permanent delete (two dialogs) and a recycle each remove only
/// the marked items and the tree follows. Run it from a normal prompt: `plumb --selftest-delete`.
/// The file the recycle step sends to the Recycle Bin; named so it is recognisable there.
const REC: &str = "plumb-selftest-delete.tmp";
/// The file held open for the failure step.
const LOCKED: &str = "locked.bin";
/// The sparse file too big for the Recycle Bin.
const BIG: &str = "big.bin";

pub fn selftest_delete() -> Result<String> {
    use windows::Win32::Foundation::E_FAIL;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::UI::WindowsAndMessaging::{IDCANCEL, IDCLOSE, IDYES};
    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
    if is_elevated() {
        return Err(fail("run this from a normal prompt: an elevated delete is not what users see".into()));
    }
    let io = |e: std::io::Error| fail(e.to_string());
    let base = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or_else(|| fail("no LOCALAPPDATA".into()))?);
    let dir = base.join(format!("plumb-delete-{}", std::process::id()));
    let shown = dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).map_err(io)?;
    for f in ["keep.txt", "a.bin", "b.bin", r"folder\inner.bin", REC, LOCKED, BIG] {
        std::fs::write(dir.join(f), vec![1u8; 4096]).map_err(io)?;
    }
    let hwnd = create_window()?;
    let win = Win::new(hwnd, crate::drives::list())?;
    STATE.with(|s| *s.borrow_mut() = Some(win));
    unsafe { SetTimer(Some(hwnd), 99, 50, None) }; // keeps the watch's wake-ups flowing
    let letter = shown.chars().next().unwrap_or('C').to_ascii_uppercase();
    let at = |rel: &str| format!(r"{shown}\{rel}");
    let in_tree = |w: &mut Win, rel: &str| w.app.tree.as_ref().is_some_and(|t| t.find(&at(rel)).is_some());
    let exists = |rel: &str| dir.join(rel).exists();
    let mark = |rels: &[&str]| {
        with(|w| {
            for r in rels {
                w.app.toggle_mark_path(&at(r));
            }
        });
    };
    let run = || -> Result<String> {
        let i = with(|w| w.app.drives.iter().position(|d| d.title() == format!("{letter}:"))).flatten();
        let i = i.ok_or_else(|| fail(format!("drive {letter}: is not in the drive list")))?;
        with(|w| w.spawn_scan(i, shown.clone(), false));
        pump_until("the scan and the directory watch", 60, &|w| w.watch.is_some())?;
        pump_until("the scanned tree to hold the files", 5, &|w| in_tree(w, REC))?;

        // Cancel: nothing is touched, the marks stay.
        mark(&["a.bin", "folder"]);
        if with(|w| w.app.basket().len()) != Some(2) {
            return Err(fail("marking two items did not fill the basket".into()));
        }
        let h = answer_dialogs(vec![IDCANCEL.0]);
        commit(hwnd);
        if !h.join().unwrap_or(false) || !exists("a.bin") || !exists(r"folder\inner.bin") || with(|w| w.app.basket().len()) != Some(2) {
            return Err(fail("Cancel on the commit dialog changed something".into()));
        }

        // Delete permanently: a file and a folder go, the rest stays; the tree and the basket follow.
        let h = answer_dialogs(vec![crate::dialog::PERMANENT, IDYES.0]);
        commit(hwnd);
        if !h.join().unwrap_or(false) {
            return Err(fail("the permanent delete did not show its two dialogs".into()));
        }
        pump_until("the delete result", 30, &|w| !w.app.deleting())?;
        if exists("a.bin") || exists("folder") || !exists("b.bin") || !exists("keep.txt") {
            return Err(fail("the permanent delete removed the wrong things".into()));
        }
        let t0 = Instant::now();
        pump_until("the tree to drop the deleted items", 15, &|w| !in_tree(w, "a.bin") && !in_tree(w, "folder") && in_tree(w, "b.bin"))?;
        let lag = t0.elapsed();
        if with(|w| w.app.basket().len()) != Some(0) {
            return Err(fail("deleted items stayed marked".into()));
        }

        // Recycle: goes to the bin without further questions.
        mark(&[REC]);
        let h = answer_dialogs(vec![crate::dialog::RECYCLE]);
        commit(hwnd);
        if !h.join().unwrap_or(false) {
            return Err(fail("the recycle dialog never appeared".into()));
        }
        pump_until("the recycle result", 30, &|w| !w.app.deleting())?;
        if exists(REC) || !exists("keep.txt") {
            return Err(fail("the recycle removed the wrong things".into()));
        }
        pump_until("the tree to drop the recycled file", 15, &|w| !in_tree(w, REC) && in_tree(w, "keep.txt"))?;
        // It must be in the Recycle Bin (not deleted for good); take it out again so the test leaves nothing behind.
        let script = format!(
            "$s = New-Object -ComObject Shell.Application; $n = 0; foreach ($i in $s.Namespace(10).Items()) {{ if ($i.Name -like '{REC}') {{ \
             $n++; $r = $i.Path; Remove-Item -LiteralPath $r -Force; \
             $m = Join-Path (Split-Path $r) ((Split-Path $r -Leaf).Replace('$R', '$I')); Remove-Item -LiteralPath $m -Force -ErrorAction SilentlyContinue }} }}; $n"
        );
        let bin = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).stdin(std::process::Stdio::null()).output().map_err(io)?;
        if String::from_utf8_lossy(&bin.stdout).trim() != "1" {
            return Err(fail(format!("the recycled file is not in the Recycle Bin exactly once: {:?} {}", String::from_utf8_lossy(&bin.stdout), String::from_utf8_lossy(&bin.stderr))));
        }

        // A file too big for the Recycle Bin (sparse, 200 GB logical, no disk used): the shell asks whether to delete it
        // for good (confirmations stay on for a recycle). Answering No must keep the file (our failure dialog then lists it, closed
        // here); without the prompt a
        // recycle would silently become permanent.
        let big = dir.join(BIG);
        let sparse = std::process::Command::new("fsutil").args(["sparse", "setflag"]).arg(&big).stdin(std::process::Stdio::null()).status().map_err(|e| fail(format!("fsutil: {e}")))?;
        std::fs::OpenOptions::new().write(true).open(&big).and_then(|f| f.set_len(200 << 30)).map_err(|e| fail(format!("set_len: {e}")))?;
        if !sparse.success() {
            return Err(fail("could not make a sparse test file".into()));
        }
        mark(&[BIG]);
        let (h, no) = (answer_dialogs(vec![crate::dialog::RECYCLE, IDCLOSE.0]), press_shell_button("eltInterruptNoBtn"));
        commit(hwnd);
        pump_until("the too-big prompt", 40, &|_| h.is_finished() && no.is_finished())?;
        if !(h.join().unwrap_or(false) && no.join().unwrap_or(false)) {
            return Err(fail(format!("a recycle of a file too big for the Recycle Bin did not ask before deleting it (file still there: {})", exists(BIG))));
        }
        pump_until("the too-big recycle to finish", 30, &|w| !w.app.deleting())?;
        if !exists(BIG) {
            return Err(fail("a file too big for the Recycle Bin was deleted although the prompt was answered No".into()));
        }
        mark(&[BIG]); // unmark it; the directory is removed at the end
        pump_until("the unmark", 5, &|w| w.app.basket().is_empty())?;

        // A file held open, recycled (which fails the same way and leaves nothing in the bin): the shell's own error
        // dialog is off, so the failure reaches our dialog with its reason.
        // Retry asks again (still held: a second dialog), Close leaves the file in place and marked; once it is
        // released, the next commit removes it.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.join(LOCKED)).map_err(io)?;
        mark(&[LOCKED]);
        let h = answer_dialogs(vec![crate::dialog::RECYCLE, crate::dialog::RETRY, IDCLOSE.0]);
        commit(hwnd);
        pump_until("the failure dialogs", 30, &|_| h.is_finished())?;
        if !h.join().unwrap_or(false) {
            return Err(fail("a held file did not bring up the failure dialog twice (Retry, then Close)".into()));
        }
        pump_until("the failed delete to finish", 30, &|w| !w.app.deleting())?;
        if !exists(LOCKED) || with(|w| w.app.basket().len()) != Some(1) {
            return Err(fail("a failed delete removed the file or unmarked it".into()));
        }
        // The reason reached the status bar (its tooltip carries the whole line), not an empty one.
        let tip = with(|w| {
            let bar = w.app.layout.status();
            w.app.tip_at(bar.x + 40.0, bar.y + 5.0)
        })
        .flatten()
        .unwrap_or_default();
        if !tip.contains("not deleted: in use by plumb.exe (") {
            return Err(fail(format!("the failure's reason does not name this process on the status line: {tip:?}")));
        }
        drop(held);
        let h = answer_dialogs(vec![crate::dialog::PERMANENT, IDYES.0]);
        commit(hwnd);
        if !h.join().unwrap_or(false) {
            return Err(fail("the commit dialogs did not appear once the file was released".into()));
        }
        pump_until("the delete after the release", 30, &|w| !w.app.deleting())?;
        if exists(LOCKED) || with(|w| w.app.basket().len()) != Some(0) {
            return Err(fail("the released file was not deleted".into()));
        }
        Ok(format!("cancel keeps marks; permanent delete (2 dialogs) and recycle remove only the marked items; a file too big for the Recycle Bin gets the shell's prompt and survives No; a held file (recycled) shows the failure dialog (Retry, Close) with its reason on the status line and stays marked; tree followed in {lag:.1?}"))
    };
    let out = run();
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out
}

/// The Processes view in a hidden window: it lists this process with a plausible size, follows a child that starts
/// and exits, stops polling while another view is on screen, and comes back without a burst of CPU.
/// Run it with `plumb --selftest-processes`.
pub fn selftest_processes() -> Result<String> {
    use windows::Win32::Foundation::E_FAIL;
    let fail = |m: String| windows::core::Error::new(E_FAIL, m);
    let dir = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| fail(e.to_string()))?.join(format!("plumb-procs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
    std::fs::write(dir.join("f.bin"), vec![1u8; 4096]).map_err(|e| fail(e.to_string()))?;
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
    // The live node of a process, by pid.
    let node = |w: &Win, pid: u32| -> Option<NodeId> {
        let t = w.app.tree.as_ref()?;
        (0..t.len() as u32).map(NodeId).find(|&n| !t.flags[n].contains(plumb_core::NodeFlags::DEAD) && t.aux.get(n.idx()).is_some_and(|a| a[2] == u64::from(pid)) && n.0 != 0)
    };
    let parent_pid = |w: &Win, id: NodeId| -> u32 {
        let t = w.app.tree.as_ref().unwrap();
        t.aux.get(t.parent[id].idx()).map_or(0, |a| a[2] as u32)
    };
    let run = || -> Result<String> {
        let me = std::process::id();
        let pi = with(|w| w.app.drives.iter().position(|d| d.kind == ViewKind::Processes)).flatten().ok_or_else(|| fail("no Processes view in the list".into()))?;
        with(|w| w.start_scan(pi));
        wait("this process in the list", 20, &|w| node(w, me).is_some_and(|n| w.app.tree.as_ref().unwrap().size[n] > 0))?;
        let ws = with(|w| w.app.tree.as_ref().unwrap().size[node(w, me).unwrap()]).unwrap_or(0);
        let status = with(|w| w.app.with_scene(false, |s| s.status.to_string())).unwrap_or_default();
        if !status.contains(" processes, ") {
            return Err(fail(format!("status line: {status:?}")));
        }
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 4 127.0.0.1 >nul"]).spawn().map_err(|e| fail(e.to_string()))?;
        let cpid = child.id();
        wait("the child under this process", 5, &|w| node(w, cpid).is_some_and(|n| parent_pid(w, n) == me))?;
        let _ = child.wait();
        wait("the child gone", 10, &|w| node(w, cpid).is_none())?;
        // Another view on screen: the list is not followed, and comes back whole.
        let z = with(|w| {
            w.app.drives.push(DriveInfo { root: "Z:\\".into(), total: 0, free: 0, kind: ViewKind::Disk });
            let z = w.app.drives.len() - 1;
            w.open(z, dir.to_string_lossy().into_owned());
            z
        })
        .unwrap_or(0);
        wait("the other view's scan", 30, &|w| w.app.tree.is_some() && w.app.active == Some(z) && matches!(w.app.scan, crate::app::Scan::Idle))?;
        let parked_polling = with(|w| w.parked[pi].as_ref().and_then(|d| d.procs.as_ref()).map(procs::ProcState::polling)).flatten();
        if parked_polling != Some(false) || with(|w| w.procs_polling()) != Some(false) {
            return Err(fail(format!("polling while away: {parked_polling:?}")));
        }
        with(|w| w.open(pi, String::new()));
        let before = with(|w| w.procs.as_ref().map_or(0, procs::ProcState::updates)).unwrap_or(0);
        wait("polling again", 10, &|w| w.procs_polling() && w.procs.as_ref().is_some_and(|p| p.updates() > before))?;
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
        let worst = with(|w| {
            let t = w.app.tree.as_ref().unwrap();
            (0..t.len()).filter(|&i| !t.flags[i].contains(plumb_core::NodeFlags::DEAD)).map(|i| t.aux.get(i).map_or(0, |a| a[0])).max().unwrap_or(0)
        })
        .unwrap_or(0);
        if worst > cores * 1500 {
            return Err(fail(format!("a burst of CPU after returning: {worst} tenths of a percent")));
        }
        // PLUMB_PROCS_SECS=<n>: then sit on the view for n seconds and report the CPU the polling cost (the window is
        // hidden, so nothing is painted: this is the poll, the reconcile and the row refresh).
        let idle = match std::env::var("PLUMB_PROCS_SECS").ok().and_then(|v| v.parse::<u64>().ok()) {
            Some(n) => {
                let (cpu0, t0) = (process_cpu(), Instant::now());
                let until = t0 + Duration::from_secs(n);
                wait("the idle period", n + 5, &|_| Instant::now() >= until)?;
                let (cpu, wall) = (process_cpu() - cpu0, t0.elapsed());
                format!("; idle on the view for {:.0} s: {:.1} ms CPU ({:.2} % of one core)", wall.as_secs_f64(), cpu.as_secs_f64() * 1e3, 100.0 * cpu.as_secs_f64() / wall.as_secs_f64())
            }
            None => String::new(),
        };
        Ok(format!("this process {} in memory, a child followed, polling stopped while away{idle}", fmt::size(ws)))
    };
    let out = run();
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { DestroyWindow(hwnd)? };
    STATE.with(|s| *s.borrow_mut() = None);
    out
}

/// Kernel plus user time of this process.
fn process_cpu() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let _ = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32 | u64::from(f.dwLowDateTime)) * 100;
    Duration::from_nanos(t(k) + t(u))
}
