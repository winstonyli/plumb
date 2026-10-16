//! Deleting paths through the shell: one `IFileOperation` batch to the Recycle Bin or for good, with a verdict per path.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::*;
use windows::core::{HRESULT, PCWSTR, Ref, Result, implement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Deleted,
    /// The path did not exist when the batch began.
    Missing,
    /// The shell refused or stopped: an `HRESULT`.
    Failed(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub path: String,
    pub verdict: Verdict,
}

/// `E_ABORT`: what an item the shell skipped or never reached is reported as.
const CANCELLED: i32 = 0x80004004u32 as i32;

type Results = Arc<Mutex<HashMap<String, i32>>>;

/// Receives each item's result from the shell, keyed by its file-system path.
#[implement(IFileOperationProgressSink)]
struct Sink(Results);

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> Result<()> { Ok(()) }
    fn FinishOperations(&self, _hr: HRESULT) -> Result<()> { Ok(()) }
    fn PreRenameItem(&self, _f: u32, _i: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostRenameItem(&self, _f: u32, _i: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PreMoveItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostMoveItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PreCopyItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostCopyItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PreDeleteItem(&self, _f: u32, _i: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PostDeleteItem(&self, _flags: u32, item: Ref<IShellItem>, hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
        let path = item.as_ref().and_then(|i| unsafe { i.GetDisplayName(SIGDN_FILESYSPATH).ok() }).and_then(|p| unsafe { p.to_string().ok() });
        if let Some(path) = path {
            self.0.lock().unwrap().insert(path, hr.0);
        }
        Ok(())
    }
    fn PreNewItem(&self, _f: u32, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostNewItem(&self, _f: u32, _d: Ref<IShellItem>, _n: &PCWSTR, _t: &PCWSTR, _a: u32, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn UpdateProgress(&self, _w: u32, _s: u32) -> Result<()> { Ok(()) }
    fn ResetTimer(&self) -> Result<()> { Ok(()) }
    fn PauseTimer(&self) -> Result<()> { Ok(()) }
    fn ResumeTimer(&self) -> Result<()> { Ok(()) }
}

/// COM initialised for the calling thread (an apartment, as the shell wants) until dropped.
struct Com(bool);

impl Com {
    fn init() -> Self {
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs the batch; `Err` only if it could not be set up or started (items then have no result).
fn perform(paths: &[&str], permanent: bool, owner: isize, results: &Results) -> Result<()> {
    let _com = Com::init();
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        let silent = owner == 0; // tests: no owner window to show the shell's progress or prompts over
        // Silent only for tests; otherwise the shell's own progress and prompts stay on.
        // Our dialogs already asked; the shell would ask a third time before a permanent delete.
        let mut flags = if silent { (FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMATION).0 } else if permanent { FOF_NOCONFIRMATION.0 } else { 0 };
        if !permanent {
            flags |= FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0;
        }
        op.SetOperationFlags(FILEOPERATION_FLAGS(flags as _))?;
        if !silent {
            op.SetOwnerWindow(HWND(owner as *mut _))?;
        }
        let sink: IFileOperationProgressSink = Sink(results.clone()).into();
        let _cookie = op.Advise(&sink)?;
        for p in paths {
            let wide: Vec<u16> = p.encode_utf16().chain([0]).collect();
            // An item the shell cannot name is left out: it gets no result and is judged below.
            let item: Result<IShellItem> = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None);
            if let Ok(item) = item {
                op.DeleteItem(&item, None)?;
            }
        }
        // The return value is Ok even when an item failed; the sink has the per-item results.
        let _ = op.PerformOperations();
    }
    Ok(())
}

/// Why a path failed, in words. The shell's own codes (`COPYENGINE_E_*`, facility 0x27) have no system message text, so the
/// ones a delete can meet are named here; anything else is the system's message, or the code itself.
pub fn reason(hr: i32) -> String {
    const NAMED: [(HRESULT, &str); 16] = [
        (COPYENGINE_E_SHARING_VIOLATION_SRC, "in use by another program"),
        (COPYENGINE_E_ACCESS_DENIED_SRC, "access denied"),
        (COPYENGINE_E_ACCESSDENIED_READONLY, "read-only"),
        (COPYENGINE_E_REQUIRES_ELEVATION, "needs administrator rights"),
        (COPYENGINE_E_PATH_NOT_FOUND_SRC, "path not found"),
        (COPYENGINE_E_PATH_TOO_DEEP_SRC, "path too long"),
        (COPYENGINE_E_DIR_NOT_EMPTY, "folder not empty"),
        (COPYENGINE_E_NET_DISCONNECT_SRC, "network disconnected"),
        (COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, "no Recycle Bin on this drive"),
        (COPYENGINE_E_RECYCLE_SIZE_TOO_BIG, "too big for the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_PATH_TOO_LONG, "path too long for the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_FORCE_NUKE, "cannot be moved to the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_UNKNOWN_ERROR, "the Recycle Bin could not take it"),
        (COPYENGINE_E_USER_CANCELLED, "cancelled"),
        (COPYENGINE_E_CANCELLED, "cancelled"),
        (COPYENGINE_E_BLOCKED_BY_DLP_POLICY, "blocked by policy"),
    ];
    let code = HRESULT(hr);
    if let Some((_, text)) = NAMED.iter().find(|(c, _)| *c == code) {
        return (*text).into();
    }
    let message = windows::core::Error::from(code).message();
    match message.trim() {
        "" => format!("error {:#010x}", hr as u32),
        m => m.into(),
    }
}

/// What happened to a path the batch was given: the disk decides. A path still there was not deleted, whatever the
/// shell reported (it also returns success codes for items the user skipped in its own prompts).
fn judge(report: Option<i32>, still_there: bool, setup: Option<i32>) -> Verdict {
    if !still_there {
        return Verdict::Deleted;
    }
    Verdict::Failed(match report {
        Some(hr) if hr < 0 => hr,
        _ => setup.unwrap_or(CANCELLED),
    })
}

/// Deletes `paths` (to the Recycle Bin unless `permanent`). Blocking: call from a worker thread.
/// `owner` is a raw `HWND` the shell's progress window and prompts attach to; 0 means none (silent).
pub fn delete(paths: Vec<String>, permanent: bool, owner: isize) -> Vec<Outcome> {
    let missing: Vec<bool> = paths.iter().map(|p| std::fs::symlink_metadata(p).is_err()).collect();
    let present: Vec<&str> = paths.iter().zip(&missing).filter(|(_, m)| !**m).map(|(p, _)| p.as_str()).collect();
    let results = Results::default();
    let setup = if present.is_empty() { None } else { perform(&present, permanent, owner, &results).err() };
    let results = results.lock().unwrap();
    paths
        .into_iter()
        .zip(missing)
        .map(|(path, gone)| {
            let verdict = if gone {
                Verdict::Missing
            } else {
                judge(results.get(&path).copied(), std::fs::symlink_metadata(&path).is_ok(), setup.as_ref().map(|e| e.code().0))
            };
            Outcome { path, verdict }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    /// A fresh directory under `%LOCALAPPDATA%` (long path form, which the shell reports back).
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join(format!("plumb-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        dir
    }

    fn p(dir: &std::path::Path, name: &str) -> String {
        dir.join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn a_permanent_delete_removes_files_and_folders_and_reports_a_missing_path() {
        let dir = scratch("perm");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("folder").join("inner.txt"), b"x").unwrap();
        let out = delete(vec![p(&dir, "a.txt"), p(&dir, "folder"), p(&dir, "gone.txt")], true, 0);
        let verdicts: Vec<_> = out.iter().map(|o| o.verdict).collect();
        assert_eq!(verdicts, vec![Verdict::Deleted, Verdict::Deleted, Verdict::Missing]);
        assert!(!dir.join("a.txt").exists() && !dir.join("folder").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_file_fails_alone_and_the_rest_still_go() {
        let dir = scratch("lock");
        std::fs::write(dir.join("ok.txt"), b"x").unwrap();
        std::fs::write(dir.join("locked.txt"), b"x").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.join("locked.txt")).unwrap();
        let out = delete(vec![p(&dir, "locked.txt"), p(&dir, "ok.txt")], true, 0);
        assert!(matches!(out[0].verdict, Verdict::Failed(hr) if hr < 0), "got {:?}", out[0].verdict);
        assert_eq!(out[1].verdict, Verdict::Deleted);
        assert!(dir.join("locked.txt").exists() && !dir.join("ok.txt").exists());
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_file_has_a_reason_in_words_and_an_unknown_code_shows_itself() {
        assert_eq!(reason(COPYENGINE_E_SHARING_VIOLATION_SRC.0), "in use by another program");
        assert_eq!(reason(0x8027ff00u32 as i32), "error 0x8027ff00", "a shell code without text");
        assert!(!reason(CANCELLED).is_empty(), "a system code keeps the system's text");
    }

    #[test]
    fn the_disk_decides_so_a_skipped_item_with_a_success_code_is_not_deleted() {
        assert_eq!(judge(Some(0x00270005), true, None), Verdict::Failed(CANCELLED), "success code, still there");
        assert_eq!(judge(Some(-5), true, None), Verdict::Failed(-5));
        assert_eq!(judge(None, false, None), Verdict::Deleted);
        assert_eq!(judge(Some(0), false, None), Verdict::Deleted);
    }

    #[test]
    fn nothing_to_delete_does_not_touch_the_shell() {
        assert!(delete(Vec::new(), true, 0).is_empty());
    }
}
