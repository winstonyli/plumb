---
status: done
---

# Delete from the tree and the map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Mark files and folders on the tree or the map, see what that frees, and delete them in one batch (Recycle Bin or permanent).

**Architecture:** A pure `plumb_core::basket` holds the marked paths per drive and does the arithmetic (topmost nodes, bytes, guards). `plumb::app` owns the basket (per-drive, in `Parked`), the keyboard and menu entry points, the chip's numbers and the tiles to tint. `plumb::trash` wraps `IFileOperation` and reports a verdict per path; `plumb::dialog` is the `TaskDialog` front; `window.rs` runs the commit on a worker thread and applies the outcome. The tree and map follow the delete through the existing live updates and directory sync.

**Tech Stack:** Rust 2024, `windows` 0.62 (`IFileOperation`, `TaskDialogIndirect`), `windows-core` 0.62 (for `#[implement]`), Direct2D drawing as in `draw.rs`.

**Spec:** `docs/superpowers/specs/2026-10-15-plumb-delete-design.md` (read its "Spike results" section too).

## Global Constraints

- Windows only for `trash`, `dialog` and the drawing; `basket`, `fmt::path_list` and all `App` logic stay pure and are unit-tested without a window.
- The basket holds full paths exactly as `Tree::path` gives them (`C:\dir\file`); matching is exact (every path comes from the tree).
- Not markable: a drive root (no parent), `%SystemRoot%` and anything under it, anything while the drive is scanning or refreshing.
- Recycle uses `FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`; permanent delete uses neither. Results come from the progress sink, never from `PerformOperations`' return value; success is `hr >= 0`.
- No compatibility shims. Use the Edit tool for any code with backslashes (the Bash tool collapses them). Never let `python` read stdin.
- `cargo ... -j 8`, one cargo at a time on `target/`.
- Tests that delete files use only a directory created by the test under `%LOCALAPPDATA%` (long path form, never `%TEMP%`'s 8.3 form) and permanent delete; a recycle in a test would litter the user's Recycle Bin.

## Review Focus

1. A folder and one of its children both marked: bytes and the commit list count the folder once (Task 1 `covered`, Task 4).
2. A marked path that no longer exists at commit (deleted or renamed meanwhile): reported missing, dropped from the basket, never an error (Tasks 2 and 4).
3. Switching drives and back keeps each drive's marks; a fresh first scan clears them, a background rescan keeps them (Task 4).
4. `Del` while the filter box has focus edits the text and marks nothing; `Del` with no selection does nothing (Task 4).
5. Marking while a scan or refresh runs, or a drive root or the Windows folder, is refused with a reason shown, never silently ignored (Tasks 1 and 4).

---

### Task 1: `Tree::find` and `plumb_core::basket`

**Files:**
- Modify: `crates/plumb-core/src/tree.rs` (add `find`), `crates/plumb-core/src/lib.rs` (add `pub mod basket;`), `crates/plumb/src/restore.rs` (use `Tree::find`, delete its `resolve`)
- Create: `crates/plumb-core/src/basket.rs`

**Interfaces:**
- Produces: `Tree::find(&self, path: &str) -> Option<NodeId>`; `plumb_core::basket::{Basket, refusal}` with the signatures below.

- [ ] **Step 1: Move `resolve` into the tree.** In `tree.rs`, after `pub fn path`, add:

```rust
    /// The node at `path` ("C:\dir\file", as [`Tree::path`] gives it), matching names exactly.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let mut parts = path.split('\\');
        let first = parts.next()?;
        let mut cur = *self.roots.iter().find(|&&r| self.name_string(r) == first)?;
        for part in parts {
            cur = self.children(cur).find(|&c| self.name_string(c) == part)?;
        }
        Some(cur)
    }
```

In `restore.rs` delete `pub(crate) fn resolve` and its doc comment, replace the two call sites in the file's non-test code (`resolve(tree, p)` at lines ~32 and ~37) with `tree.find(p)`, and in its tests replace `resolve(t, X)` / `resolve(&t, X)` / `resolve(&new, X)` with `t.find(X)` / `t.find(X)` / `new.find(X)` (`grep -n "resolve" crates/plumb/src/restore.rs` must then show nothing). `dirsync::resolve` is a different function (UTF-16, case-insensitive): leave it.

- [ ] **Step 2: Write `basket.rs` with its tests first.** Create `crates/plumb-core/src/basket.rs`:

```rust
//! The paths marked for deletion and the arithmetic over them. Pure.
use crate::node::NodeId;
use crate::tree::Tree;
use std::collections::HashSet;

/// Marked paths, in the order they were marked, as [`Tree::path`] gives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Basket {
    paths: Vec<String>,
}

impl Basket {
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.iter().any(|p| p == path)
    }

    /// Whether `path` is marked or lies under a marked folder.
    pub fn covers(&self, path: &str) -> bool {
        self.paths.iter().any(|p| path == p || path.strip_prefix(p.as_str()).is_some_and(|rest| rest.starts_with('\\')))
    }

    /// Marks `path`, or unmarks it if it was marked; true when it is marked now.
    pub fn toggle(&mut self, path: &str) -> bool {
        match self.paths.iter().position(|p| p == path) {
            Some(i) => {
                self.paths.remove(i);
                false
            }
            None => {
                self.paths.push(path.to_string());
                true
            }
        }
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    pub fn remove_all(&mut self, gone: &[String]) {
        self.paths.retain(|p| !gone.contains(p));
    }

    /// The nodes the marks stand for that still exist and have no marked ancestor: what a delete touches.
    pub fn covered(&self, tree: &Tree) -> Vec<NodeId> {
        let ids: Vec<NodeId> = self.paths.iter().filter_map(|p| tree.find(p)).collect();
        let set: HashSet<NodeId> = ids.iter().copied().collect();
        ids.into_iter()
            .filter(|&id| {
                let mut up = tree.parent[id];
                while !up.is_none() {
                    if set.contains(&up) {
                        return false;
                    }
                    up = tree.parent[up];
                }
                true
            })
            .collect()
    }

    /// Allocated bytes under the nodes in [`Basket::covered`]; an upper bound, since a hard-linked file's other names free nothing.
    pub fn total_bytes(&self, tree: &Tree) -> u64 {
        self.covered(tree).iter().map(|&id| tree.subtree_size[id]).sum()
    }
}

/// Why `id` may not be marked, or `None` if it may: a drive root, or `system_root` ("C:\Windows") and anything under it.
pub fn refusal(tree: &Tree, id: NodeId, system_root: &str) -> Option<&'static str> {
    if tree.parent[id].is_none() {
        return Some("a drive root cannot be deleted");
    }
    let path = tree.path(id);
    let under = path.get(..system_root.len()).is_some_and(|head| head.eq_ignore_ascii_case(system_root))
        && (path.len() == system_root.len() || path.as_bytes()[system_root.len()] == b'\\');
    under.then_some("the Windows folder is protected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeFlags;
    use crate::tree::TreeBuilder;

    /// C:\{a\{x 10, y 20}, Windows\{sys 500}, big 1000}
    fn tree() -> Tree {
        let mut b = TreeBuilder::new();
        let c = b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        let a = b.push_str(c, "a", 0, NodeFlags::DIR);
        b.push_str(a, "x", 10, NodeFlags::empty());
        b.push_str(a, "y", 20, NodeFlags::empty());
        let w = b.push_str(c, "Windows", 0, NodeFlags::DIR);
        b.push_str(w, "sys", 500, NodeFlags::empty());
        b.push_str(c, "big", 1000, NodeFlags::empty());
        b.finish().unwrap()
    }

    #[test]
    fn toggle_marks_then_unmarks() {
        let mut b = Basket::default();
        assert!(b.toggle(r"C:\a"));
        assert!(b.contains(r"C:\a"));
        assert!(!b.toggle(r"C:\a"));
        assert!(b.is_empty());
    }

    #[test]
    fn covers_a_marked_folders_contents_but_not_a_sibling_with_the_same_prefix() {
        let mut b = Basket::default();
        b.toggle(r"C:\a");
        assert!(b.covers(r"C:\a") && b.covers(r"C:\a\x"));
        assert!(!b.covers(r"C:\ab"), "C:\\ab is not inside C:\\a");
    }

    #[test]
    fn a_marked_folder_and_its_child_count_the_folder_once() {
        let t = tree();
        let mut b = Basket::default();
        b.toggle(r"C:\a\x");
        b.toggle(r"C:\a");
        b.toggle(r"C:\big");
        let ids = b.covered(&t);
        let mut paths: Vec<String> = ids.iter().map(|&i| t.path(i)).collect();
        paths.sort();
        assert_eq!(paths, vec![r"C:\a".to_string(), r"C:\big".to_string()]);
        assert_eq!(b.total_bytes(&t), 30 + 1000);
    }

    #[test]
    fn a_path_that_no_longer_resolves_is_dropped_and_a_removed_node_too() {
        let mut t = tree();
        let mut b = Basket::default();
        b.toggle(r"C:\nope");
        b.toggle(r"C:\big");
        assert_eq!(b.covered(&t).len(), 1);
        let big = t.find(r"C:\big").unwrap();
        assert!(t.remove(big));
        assert!(b.covered(&t).is_empty(), "a dead node does not resolve");
        assert_eq!(b.total_bytes(&t), 0);
    }

    #[test]
    fn remove_all_unmarks_only_the_named_paths() {
        let mut b = Basket::default();
        b.toggle(r"C:\a");
        b.toggle(r"C:\big");
        b.remove_all(&[r"C:\a".to_string()]);
        assert_eq!(b.len(), 1);
        assert!(b.contains(r"C:\big"));
    }

    #[test]
    fn roots_and_the_windows_folder_are_refused() {
        let t = tree();
        let at = |p: &str| t.find(p).unwrap();
        assert!(refusal(&t, at("C:"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\Windows"), r"C:\Windows").is_some());
        assert!(refusal(&t, at(r"C:\Windows\sys"), r"c:\windows").is_some(), "case does not matter");
        assert!(refusal(&t, at(r"C:\a"), r"C:\Windows").is_none());
        assert!(refusal(&t, at(r"C:\big"), r"C:\Win").is_none(), "a longer sibling name is not inside");
    }
}
```

- [ ] **Step 3: Register the module and run.** Add `pub mod basket;` to `crates/plumb-core/src/lib.rs` beside the other modules. Run `cargo test -p plumb-core -p plumb -j 8 basket restore`. Expected: the six `basket` tests and the `restore` tests pass.
- [ ] **Step 4: Full check and commit.** `cargo clippy --workspace --all-targets -j 8` clean, then `git add -A crates && git commit -m "Core: Tree::find and the delete basket"` (with the attribution line).

---

### Task 2: `trash` module, the v6 manifest and the Cargo features

**Files:**
- Create: `crates/plumb/build.rs`, `crates/plumb/src/trash.rs`
- Modify: `crates/plumb/Cargo.toml`, `crates/plumb/src/lib.rs`

**Interfaces:**
- Produces: `plumb::trash::{Verdict, Outcome, delete}`:
  - `enum Verdict { Deleted, Missing, Failed(i32) }` (the `i32` is an `HRESULT`)
  - `struct Outcome { path: String, verdict: Verdict }`
  - `fn delete(paths: Vec<String>, permanent: bool, owner: isize) -> Vec<Outcome>`: blocking, call it from a worker thread; `owner` is a raw `HWND` for the shell's progress window, `0` means no owner and silent flags (tests).

- [ ] **Step 1: Cargo and manifest.** In `crates/plumb/Cargo.toml` add `"Win32_System_Com",` to the `windows` feature list (after `Win32_System_DataExchange`'s neighbour, keep alphabetical) and, under `[target.'cfg(windows)'.dependencies]`, `windows-core = "0.62"`. Create `crates/plumb/build.rs`:

```rust
fn main() {
    // Common Controls v6: `TaskDialogIndirect` does not exist in the v5 library the exe gets without a manifest.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
```

- [ ] **Step 2: Write the failing tests.** Create `crates/plumb/src/trash.rs` with only the types, an empty-bodied `pub fn delete(..) -> Vec<Outcome> { Vec::new() }`, and these tests at the bottom (the module is `cfg(windows)`, registered in Step 4):

```rust
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
    fn nothing_to_delete_does_not_touch_the_shell() {
        assert!(delete(Vec::new(), true, 0).is_empty());
    }
}
```

Run `cargo test -p plumb -j 8 trash` after registering the module (Step 4's `lib.rs` line); expected: the first two FAIL (empty result), the third passes.

- [ ] **Step 3: Implement.** Replace the stub with the real module (keep the tests):

```rust
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

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`: what an item the shell never reached is reported as.
const CANCELLED: i32 = 0x800704C7u32 as i32;

type Results = Arc<Mutex<HashMap<String, i32>>>;

/// Calls the sink with each item's result, keyed by its file-system path.
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
        let mut flags = if silent { (FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMATION).0 as u32 } else { 0 };
        if !permanent {
            flags |= (FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0) as u32; // drop the `.0` on FOFX_RECYCLEONDELETE if it is a plain u32 const
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
                match results.get(&path) {
                    Some(&hr) if hr >= 0 => Verdict::Deleted,
                    Some(&hr) => Verdict::Failed(hr),
                    // No report (the shell named the path differently, or never got to it): trust the disk.
                    None if std::fs::symlink_metadata(&path).is_err() => Verdict::Deleted,
                    None => Verdict::Failed(setup.as_ref().map_or(CANCELLED, |e| e.code().0)),
                }
            };
            Outcome { path, verdict }
        })
        .collect()
}
```

- [ ] **Step 4: Register and run.** In `crates/plumb/src/lib.rs` add, with the other `#[cfg(windows)]` modules, `#[cfg(windows)] pub mod trash;`. Run `cargo test -p plumb -j 8 trash`. Expected: all three pass. If `SetOperationFlags` or the flag arithmetic does not type-check, mirror what compiled in the spike: `let mut flags = FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMATION; flags |= FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE; op.SetOperationFlags(FILEOPERATION_FLAGS(flags.0 as u32))?;`.
- [ ] **Step 5: Prove the manifest took.** `cargo build -p plumb -j 8`, then check the embedded dependency: `rg -c "Microsoft.Windows.Common-Controls" target/debug/plumb.exe` prints a count of at least 1 (the manifest XML is stored uncompressed in the exe). If it prints nothing, the link args did not apply: stop and report.
- [ ] **Step 6: Check, commit.** `cargo test --workspace -j 8` and clippy clean; `cargo run -p plumb -- --selftest` still prints `selftest ok`. Commit "Trash: IFileOperation batch with a verdict per path; comctl32 v6 manifest".

---

### Task 3: `fmt::path_list` and the dialog

**Files:**
- Modify: `crates/plumb/src/fmt.rs`, `crates/plumb/src/lib.rs`
- Create: `crates/plumb/src/dialog.rs`

**Interfaces:**
- Produces: `fmt::path_list(paths: &[String], max: usize) -> String`; `plumb::dialog::{Choice, choose, confirm_permanent}`:
  - `enum Choice { Recycle, Permanent, Clear, Cancel }`
  - `fn choose(owner: HWND, paths: &[String], bytes: u64) -> Choice`
  - `fn confirm_permanent(owner: HWND, count: usize, bytes: u64) -> bool`

- [ ] **Step 1: Failing test for `path_list`.** In `fmt.rs`'s `tests` module add:

```rust
    #[test]
    fn a_path_list_shows_the_first_few_and_counts_the_rest() {
        let p: Vec<String> = ["C:\\a", "C:\\b", "C:\\c"].map(String::from).into();
        assert_eq!(path_list(&p, 5), "C:\\a\nC:\\b\nC:\\c");
        assert_eq!(path_list(&p, 2), "C:\\a\nC:\\b\nand 1 more");
        assert_eq!(path_list(&[], 2), "");
    }
```

Run `cargo test -p plumb -j 8 fmt::` (fails: no function).

- [ ] **Step 2: Implement** (in `fmt.rs`, before `#[cfg(test)]`):

```rust
/// The first `max` paths, one a line, then "and N more".
pub fn path_list(paths: &[String], max: usize) -> String {
    let mut out = paths.iter().take(max).map(String::as_str).collect::<Vec<_>>().join("\n");
    if paths.len() > max {
        out.push_str(&format!("\nand {} more", paths.len() - max));
    }
    out
}
```

Run the test again: passes.

- [ ] **Step 3: Write `dialog.rs`.** Register with `#[cfg(windows)] pub mod dialog;` in `lib.rs`.

```rust
//! The commit dialog: what is about to be deleted, and how.
use crate::fmt;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::*;
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Recycle,
    Permanent,
    /// Unmark everything.
    Clear,
    Cancel,
}

const RECYCLE: i32 = 1001;
const PERMANENT: i32 = 1002;
const CLEAR: i32 = 1003;
/// The paths shown before "and N more".
const SHOWN: usize = 20;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn plural(n: usize) -> String {
    format!("{n} item{}", if n == 1 { "" } else { "s" })
}

/// Asks what to do with the marked `paths` (`bytes` is what they hold, an upper bound).
pub fn choose(owner: HWND, paths: &[String], bytes: u64) -> Choice {
    let (title, head, body, list) = (
        wide("plumb"),
        wide(&format!("Delete {}? Up to {}.", plural(paths.len()), fmt::size(bytes))),
        wide("Recycle moves them to the Recycle Bin (Explorer can undo it). Delete permanently cannot be undone."),
        wide(&fmt::path_list(paths, SHOWN)),
    );
    let (recycle, permanent, clear) = (wide("Recycle"), wide("Delete permanently"), wide("Unmark all"));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: RECYCLE, pszButtonText: PCWSTR(recycle.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: PERMANENT, pszButtonText: PCWSTR(permanent.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: CLEAR, pszButtonText: PCWSTR(clear.as_ptr()) },
    ];
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_EXPAND_FOOTER_AREA,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        pszExpandedInformation: PCWSTR(list.as_ptr()),
        pszExpandedControlText: wide("Show the paths").as_ptr().into(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: RECYCLE,
        ..Default::default()
    };
    let mut id = 0;
    match unsafe { TaskDialogIndirect(&cfg, Some(&mut id), None, None) } {
        Ok(()) => match id {
            RECYCLE => Choice::Recycle,
            PERMANENT => Choice::Permanent,
            CLEAR => Choice::Clear,
            _ => Choice::Cancel,
        },
        Err(_) => Choice::Cancel,
    }
}

/// The second question before a permanent delete.
pub fn confirm_permanent(owner: HWND, count: usize, bytes: u64) -> bool {
    let (title, head, body) = (
        wide("plumb"),
        wide(&format!("Permanently delete {}?", plural(count))),
        wide(&format!("Up to {} will be removed for good. This cannot be undone.", fmt::size(bytes))),
    );
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        nDefaultButton: IDNO.0,
        ..Default::default()
    };
    let mut id = 0;
    unsafe { TaskDialogIndirect(&cfg, Some(&mut id), None, None) }.is_ok() && id == IDYES.0
}
```

Fix the one deliberate shortcut in the draft: `pszExpandedControlText` above borrows a temporary, so bind it first (`let toggle = wide("Show the paths");` before `cfg`, then `pszExpandedControlText: PCWSTR(toggle.as_ptr())`). Every `wide(...)` buffer must be a named local that outlives the `TaskDialogIndirect` call.

- [ ] **Step 4: Build and eyeball.** `cargo build -p plumb -j 8` compiles (the `IDNO`/`IDYES` constants live in `windows::Win32::UI::WindowsAndMessaging` if `Controls` does not export them: import them from there). The dialog itself is shown in Task 6's manual run; it needs no headless test (it is a thin call over text built by the tested `path_list`).
- [ ] **Step 5: Check, commit.** Tests and clippy clean; commit "Dialog: the delete confirmation, and fmt::path_list".

---

### Task 4: the basket in `App`

**Files:**
- Modify: `crates/plumb/src/app.rs` (all of it; tests in its `mod tests`)

**Interfaces:**
- Consumes: `plumb_core::basket::{Basket, refusal}`, `Tree::find`.
- Produces (all on `App` unless noted):
  - `enum MarkState { Mark, Unmark, Refused(&'static str) }` and `struct CommitRequest { pub drive: usize, pub paths: Vec<String>, pub bytes: u64 }` (module level, `pub`)
  - `Effect::Commit` (new variant)
  - `fn mark_state(&self, id: NodeId) -> MarkState`, `fn toggle_mark(&mut self, id: NodeId)`
  - `fn commit_request(&mut self) -> Option<CommitRequest>`, `fn set_deleting(&mut self, on: bool)`, `fn finish_delete(&mut self, gone: &[String], failed: &[(String, String)])`
  - `fn basket_label(&self) -> Option<String>`, `fn basket_active(&self) -> bool`, `fn marked_tiles(&self) -> &[usize]`, `fn basket(&self) -> &Basket`
  - `pub notice` shows as the first part of the status line until the next click or key.

- [ ] **Step 1: Write the failing tests** (append to `mod tests` in `app.rs`; they pin the Review Focus items and the shape of the API):

```rust
    /// A non-root file and a non-root folder that has children, and one of that folder's children.
    fn picks(a: &App) -> (NodeId, NodeId, NodeId) {
        let t = a.tree.as_ref().unwrap();
        let all = || (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none());
        let file = all().find(|&i| !t.has_children(i)).unwrap();
        let dir = all().find(|&i| t.has_children(i)).unwrap();
        (file, dir, t.children(dir).next().unwrap())
    }

    fn select(a: &mut App, id: NodeId) {
        let (t, m) = (a.tree.as_ref().unwrap(), a.model.as_mut().unwrap());
        m.reveal(t, id);
        m.select(id);
    }

    #[test]
    fn del_marks_and_unmarks_the_selection_and_the_filter_box_keeps_its_own_del() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        select(&mut a, file);
        a.handle(Input::Edit(Edit::Delete));
        assert_eq!(a.basket().len(), 1);
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "the second Del unmarks");
        a.handle(Input::Find); // focus the filter box
        a.handle(Input::Edit(Edit::Delete));
        assert!(a.basket().is_empty(), "with the filter box focused, Del edits text and marks nothing");
        let mut none = app();
        none.handle(Input::Edit(Edit::Delete));
        assert!(none.basket().is_empty(), "no tree, nothing selected: nothing happens");
    }

    #[test]
    fn a_folder_and_its_child_marked_together_count_the_folder_once() {
        let mut a = scanned();
        let (_, dir, child) = picks(&a);
        a.toggle_mark(child);
        a.toggle_mark(dir);
        a.update_map(1.0);
        let t = a.tree.as_ref().unwrap();
        assert_eq!(a.basket_label(), Some(format!("1 marked, up to {}", fmt::size(t.subtree_size[dir]))));
        let req = a.commit_request().unwrap();
        assert_eq!(req.paths, vec![t.path(dir)], "the commit lists the folder, not its child");
        assert_eq!(req.bytes, t.subtree_size[dir]);
    }

    #[test]
    fn roots_and_scans_are_refused_with_a_reason_that_reaches_the_status_line() {
        let mut a = scanned();
        let root = a.tree.as_ref().unwrap().roots[0];
        assert_eq!(a.mark_state(root), MarkState::Refused("a drive root cannot be deleted"));
        a.toggle_mark(root);
        assert!(a.basket().is_empty());
        assert!(a.status_line(true).starts_with("cannot mark: a drive root cannot be deleted"), "{}", a.status_line(true));
        a.handle(Input::Click(5.0, a.layout.header().y + 5.0)); // any click clears the notice
        assert!(!a.status_line(true).starts_with("cannot mark"));
        let (file, ..) = picks(&a);
        a.refreshing = true;
        assert_eq!(a.mark_state(file), MarkState::Refused("the drive is being scanned"));
    }

    #[test]
    fn marks_survive_a_drive_switch_a_rescan_keeps_them_and_a_first_scan_clears_them() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        let mut other = Parked::default();
        a.swap_slot(&mut other);
        assert!(a.basket().is_empty(), "the other drive has no marks");
        a.swap_slot(&mut other);
        assert_eq!(a.basket().len(), 1, "coming back finds them");
        a.refreshing = true;
        a.refreshing = false;
        assert_eq!(a.basket().len(), 1, "a background rescan leaves them");
        a.begin_scan(0);
        assert!(a.basket().is_empty(), "a fresh scan starts clean");
    }

    #[test]
    fn a_marked_path_that_is_gone_at_commit_is_dropped_not_deleted() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        assert!(a.tree.as_mut().unwrap().remove(file));
        assert!(a.commit_request().is_none());
        assert!(a.basket().is_empty());
        assert!(a.status_line(true).starts_with("the marked items are already gone"), "{}", a.status_line(true));
    }

    #[test]
    fn a_commit_refuses_a_busy_drive_and_a_second_delete() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        a.toggle_mark(file);
        a.refreshing = true;
        assert!(a.commit_request().is_none());
        a.refreshing = false;
        assert!(a.commit_request().is_some());
        a.set_deleting(true);
        assert!(a.commit_request().is_none(), "one delete at a time");
    }

    #[test]
    fn a_finished_delete_unmarks_what_went_and_keeps_what_failed() {
        let mut a = scanned();
        let (file, dir, _) = picks(&a);
        a.toggle_mark(file);
        a.toggle_mark(dir);
        let t = a.tree.as_ref().unwrap();
        let (gone, kept) = (t.path(file), t.path(dir));
        a.set_deleting(true);
        a.finish_delete(std::slice::from_ref(&gone), &[(kept.clone(), "access denied".into())]);
        assert!(a.basket().contains(&kept) && !a.basket().contains(&gone));
        assert!(a.status_line(true).starts_with("1 of 2 not deleted: access denied; still marked"), "{}", a.status_line(true));
        assert!(a.commit_request().is_some(), "the delete is over, so a new commit may start");
    }

    #[test]
    fn the_chip_is_clickable_only_while_something_is_marked() {
        let mut a = scanned();
        let (file, ..) = picks(&a);
        let chip = a.layout.with_basket(true).basket_chip();
        assert_eq!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0)), Effect::None, "nothing marked: no chip, no effect");
        a.toggle_mark(file);
        a.update_map(1.0);
        let chip = a.layout.with_basket(a.basket_active()).basket_chip();
        assert_eq!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0)), Effect::Commit);
    }
```

These do not compile yet; that is the failing state (`cargo test -p plumb -j 8 --no-run` errors on the missing items).

- [ ] **Step 2: Add the state.** In `app.rs`:
  1. Imports: `use plumb_core::basket::{self, Basket};` and `use std::collections::HashSet;` (skip either already imported).
  2. Module-level types (beside `Effect`):

```rust
/// What the context menu and `Del` can do with a node.
#[derive(Debug, PartialEq)]
pub enum MarkState {
    Mark,
    Unmark,
    Refused(&'static str),
}

/// What a commit would delete: the topmost marked paths of one drive and what they hold.
pub struct CommitRequest {
    pub drive: usize,
    pub paths: Vec<String>,
    pub bytes: u64,
}

/// The basket counted against the tree: derived, so rebuilt when the marks, the tree or the tiles change.
#[derive(Default)]
struct BasketView {
    key: Option<(u64, (usize, usize, u64), u64)>,
    items: usize,
    bytes: u64,
    /// Indices into `map_tiles` to tint: tiles at or under a marked node.
    tiles: Vec<usize>,
}
```

  3. `Effect`: add `/// Ask what to delete (the basket chip). Commit,`.
  4. `Parked`: add `basket: Basket,` (derive `Default` already covers it).
  5. `App`: add fields `basket: Basket, basket_rev: u64, basket_view: BasketView, deleting: bool, notice: String, system_root: String,`; initialise in `App::new`: `basket: Basket::default(), basket_rev: 0, basket_view: BasketView::default(), deleting: false, notice: String::new(), system_root: std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),`.
  6. `swap_slot`: add `swap(&mut self.basket, &mut p.basket); self.basket_rev += 1;` before `self.map_reset()`.
  7. `begin_scan`: add `self.basket.clear(); self.basket_rev += 1;`.
  8. `handle`: at the top, after `self.layout.cancel = self.cancelable();`, add
     `self.layout.basket = self.basket_active();` and
     `if matches!(input, Input::Click(..) | Input::DoubleClick(..) | Input::RightClick(..) | Input::Key(_) | Input::Edit(_) | Input::Enter | Input::Escape | Input::Find) { self.notice.clear(); }` (hover, wheel, drag and resize keep it).
     Replace the arm `Input::Edit(e) => return self.filter_edit(e),` by
     `Input::Edit(Edit::Delete) if !self.filter.ui.focus => self.toggle_mark_selected(),` followed by the original arm.
  9. `click`: add the arm `Hit::Basket => return Effect::Commit,` next to `Hit::Cancel => return Effect::Cancel,`.
  10. `button_at`: add `Hit::Basket => Some((l.basket_chip(), "Review and delete the marked items".into())),`.
  11. `status_line`: after `let mut parts: Vec<String> = ...;` insert `if !self.notice.is_empty() { parts.insert(0, self.notice.clone()); }`.
  12. Rename the existing `pub fn update_map(&mut self, px: f32)` to `fn update_tiles(&mut self, px: f32)` and add:

```rust
    /// Brings the map and the basket's numbers up to date for the next paint.
    pub fn update_map(&mut self, px: f32) {
        self.update_tiles(px);
        self.refresh_basket();
    }
```

  13. The new methods (an `impl App` block near `map_selected_tile`):

```rust
    pub fn basket(&self) -> &Basket {
        &self.basket
    }

    pub fn basket_active(&self) -> bool {
        !self.basket.is_empty()
    }

    pub fn marked_tiles(&self) -> &[usize] {
        &self.basket_view.tiles
    }

    /// "3 marked, up to 12.4 GB" for the status bar chip.
    pub fn basket_label(&self) -> Option<String> {
        self.basket_active().then(|| format!("{} marked, up to {}", self.basket_view.items, fmt::size(self.basket_view.bytes)))
    }

    pub fn mark_state(&self, id: NodeId) -> MarkState {
        let Some(tree) = &self.tree else { return MarkState::Refused("no drive is shown") };
        if self.basket.contains(&tree.path(id)) {
            return MarkState::Unmark;
        }
        if self.cancelable() {
            return MarkState::Refused("the drive is being scanned");
        }
        basket::refusal(tree, id, &self.system_root).map_or(MarkState::Mark, MarkState::Refused)
    }

    /// Marks or unmarks `id`; a refusal goes to the status line.
    pub fn toggle_mark(&mut self, id: NodeId) {
        if let MarkState::Refused(why) = self.mark_state(id) {
            self.notice = format!("cannot mark: {why}");
            return;
        }
        let Some(path) = self.tree.as_ref().map(|t| t.path(id)) else { return };
        self.basket.toggle(&path);
        self.basket_rev += 1;
    }

    fn toggle_mark_selected(&mut self) {
        if let Some(id) = self.model.as_ref().and_then(|m| m.selected()) {
            self.toggle_mark(id);
        }
    }

    /// Rebuilds what the chip and the tiles show when the marks, the tree or the tiles changed.
    fn refresh_basket(&mut self) {
        let Some(tree) = &self.tree else {
            self.basket_view = BasketView::default();
            return;
        };
        if self.basket.is_empty() {
            if self.basket_view.key.is_some() {
                self.basket_view = BasketView::default();
            }
            return;
        }
        let fingerprint = (tree.len(), tree.dead(), tree.roots.first().map_or(0, |&r| tree.subtree_size[r]));
        let key = (self.basket_rev, fingerprint, self.map_gen);
        if self.basket_view.key == Some(key) {
            return;
        }
        let ids = self.basket.covered(tree);
        let set: HashSet<NodeId> = ids.iter().copied().collect();
        let under = |mut n: NodeId| {
            while !n.is_none() {
                if set.contains(&n) {
                    return true;
                }
                n = tree.parent[n];
            }
            false
        };
        let tiles = self.map_tiles.iter().enumerate().filter(|(_, t)| t.kind != Kind::Rest && under(t.node)).map(|(i, _)| i).collect();
        self.basket_view = BasketView { key: Some(key), items: ids.len(), bytes: ids.iter().map(|&i| tree.subtree_size[i]).sum(), tiles };
    }

    pub fn set_deleting(&mut self, on: bool) {
        self.deleting = on;
    }

    /// What a commit would delete right now, or `None` (with the reason on the status line).
    pub fn commit_request(&mut self) -> Option<CommitRequest> {
        if self.deleting {
            self.notice = "a delete is already running".into();
            return None;
        }
        if self.cancelable() {
            self.notice = "cannot delete while the drive is being scanned".into();
            return None;
        }
        let (drive, tree) = (self.active?, self.tree.as_ref()?);
        let ids: Vec<NodeId> = self.basket.covered(tree).into_iter().filter(|&i| basket::refusal(tree, i, &self.system_root).is_none()).collect();
        if ids.is_empty() {
            self.basket.clear();
            self.basket_rev += 1;
            self.notice = "the marked items are already gone".into();
            return None;
        }
        let paths = ids.iter().map(|&i| tree.path(i)).collect();
        let bytes = ids.iter().map(|&i| tree.subtree_size[i]).sum();
        Some(CommitRequest { drive, paths, bytes })
    }

    /// A delete ended: `gone` (deleted or already missing) leave the basket, `failed` (path, reason) stay.
    pub fn finish_delete(&mut self, gone: &[String], failed: &[(String, String)]) {
        self.deleting = false;
        self.basket.remove_all(gone);
        self.basket_rev += 1;
        self.notice = match failed.first() {
            None => format!("deleted {} item{}", gone.len(), if gone.len() == 1 { "" } else { "s" }),
            Some((_, why)) => format!("{} of {} not deleted: {why}; still marked", failed.len(), gone.len() + failed.len()),
        };
    }
```

- [ ] **Step 3: Layout for the chip** (needed for the tests to compile; the drawing is Task 5). In `layout.rs`: add `pub const BASKET_W: f32 = 190.0;`, a field `pub basket: bool` on `Layout` (default `false` in `Layout::new`), `with_basket`, `basket_chip`, `Hit::Basket` and its hit-test arm (after the cancel arm):

```rust
    pub fn with_basket(&self, basket: bool) -> Self {
        Self { basket, ..self.clone() }
    }

    /// The marked-items chip in the status bar, left of the Cancel button (or of the map button when there is none).
    pub fn basket_chip(&self) -> Rect {
        let right = if self.cancel { self.cancel_button().x } else { self.map_button().x } - 4.0;
        let s = self.status();
        Rect { x: (right - BASKET_W).max(0.0), y: s.y + 2.0, w: BASKET_W, h: s.h - 4.0 }
    }
```

  and `if self.basket && self.basket_chip().contains(x, y) { return Hit::Basket; }` after the cancel check in `hit`; plus a `Hit::Basket` variant with the doc `/// The marked-items chip in the status bar.`. Add this layout test to `layout.rs`'s tests:

```rust
    #[test]
    fn the_basket_chip_sits_left_of_cancel_and_the_map_button_without_overlap() {
        let l = Layout::new(900.0, 600.0).with_basket(true);
        let chip = l.basket_chip();
        assert!(chip.x + chip.w <= l.map_button().x, "left of the map button");
        let with_cancel = l.with_cancel(true);
        assert!(with_cancel.basket_chip().x + with_cancel.basket_chip().w <= with_cancel.cancel_button().x, "left of Cancel when it shows");
        assert_eq!(l.hit(chip.x + 3.0, chip.y + 3.0, 0), Hit::Basket);
        assert_ne!(Layout::new(900.0, 600.0).hit(chip.x + 3.0, chip.y + 3.0, 0), Hit::Basket, "hidden unless something is marked");
    }
```

- [ ] **Step 4: Run.** `cargo test -p plumb -j 8`. Expected: the new `app` tests and the layout test pass, the rest of the suite is unchanged. If `the_chip_is_clickable_only_while_something_is_marked` fails at its first click (a click on the unmarked chip area), that is the click landing on the legend or status text: it must return `Effect::None`; investigate before going on.
- [ ] **Step 5: Check, commit.** `cargo clippy --workspace --all-targets -j 8` clean; commit "App: the delete basket, marks, guards and the commit request".

---

### Task 5: drawing: marked rows, marked tiles, the chip

**Files:**
- Modify: `crates/plumb/src/draw.rs`, `crates/plumb/src/app.rs` (`with_scene`), `crates/plumb/tests/render.rs`

**Interfaces:**
- Consumes: `App::basket()`, `App::marked_tiles()`, `App::basket_label()`, `Layout::basket_chip`.
- Produces: `View.basket: &Basket`, `MapView.marked: &[usize]`, `Scene.basket: Option<&str>` (the chip's label; `None` hides it).

- [ ] **Step 1: Failing render tests** (append to `tests/render.rs`; update every `View { .. }`, `MapView { .. }` and `Scene { .. }` literal in the file for the new fields: `View { ..., basket: &Basket::default() }`, `MapView { ..., marked: &[] }`, `Scene { ..., basket: None }`, with `use plumb_core::basket::Basket;`):

```rust
#[test]
fn a_marked_tile_is_tinted_without_redrawing_the_cached_tiles_and_the_chip_shows_its_label() {
    use plumb_core::treemap::layout as map_layout;
    let gfx = Gfx::warp().expect("WARP device");
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    for (i, size) in [900_000u64, 400_000].into_iter().enumerate() {
        b.push_str(root, &format!("f{i}.mp4"), size, plumb_core::NodeFlags::empty());
    }
    let tree = b.finish().unwrap();
    let mut layout = Layout::new(W as f32, H as f32);
    layout.map_on = true;
    let tiles = map_layout(&tree, root, layout.map().unwrap(), 1.0);
    let classes = plumb_core::treemap::class_bytes(&tiles);
    let painter = Painter::new(&gfx).unwrap();
    let big = tiles.iter().position(|t| t.bytes == 900_000).unwrap();
    let mid = { let r = tiles[big].rect; ((r.x + r.w / 2.0) as u32, (r.y + r.h / 2.0) as u32) };
    let frame = |marked: &[usize], chip: Option<&str>| {
        let scene = Scene {
            layout: layout.with_basket(chip.is_some()), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None,
            view: None, message: "", loading: false, status: "", banner: None, filter: FilterBar::default(), basket: chip,
            map: Some(MapView { tiles: &tiles, generation: 1, now: 0, selected: None, hover: None, classes: &classes, marked }),
        };
        let target = bitmap(&gfx, D2D1_BITMAP_OPTIONS_TARGET);
        painter.paint(&gfx, &target, 96.0, 0.0, &scene).expect("EndDraw");
        read_back(&gfx, &target)
    };
    let plain = frame(&[], None);
    assert_eq!(painter.map_builds(), 1);
    let marked = frame(&[big], Some("1 marked, up to 900 KB"));
    assert_eq!(painter.map_builds(), 1, "a mark is drawn over the cached tiles, not into them");
    assert_ne!(rgb_at(&plain, mid.0, mid.1), rgb_at(&marked, mid.0, mid.1), "the marked tile is tinted");
    let chip = Layout::new(W as f32, H as f32).with_basket(true).basket_chip();
    let (cx, cy) = ((chip.x + 4.0) as u32, (chip.y + chip.h / 2.0) as u32);
    assert_ne!(rgb_at(&plain, cx, cy), rgb_at(&marked, cx, cy), "the chip is drawn in the status bar");
}

#[test]
fn a_marked_row_is_tinted() {
    let gfx = Gfx::warp().expect("WARP device");
    let mut b = plumb_core::TreeBuilder::new();
    let root = b.push_str(plumb_core::NodeId::NONE, "root", 0, plumb_core::NodeFlags::DIR);
    b.push_str(root, "target.bin", 5_000, plumb_core::NodeFlags::empty());
    let tree = b.finish().unwrap();
    let model = RowModel::new(&tree);
    let layout = Layout::new(W as f32, H as f32);
    let mut basket = Basket::default();
    let shot = |basket: &Basket| {
        let scene = Scene {
            layout: layout.clone(), theme: Theme::new(true), chips: &[], active_chip: None, sort: (1, true), admin: None, hot: None,
            view: Some(View { tree: &tree, model: &model, scroll: 0, partial: false, unscanned: None, hover: None, found: None, basket }),
            message: "", loading: false, status: "", banner: None, filter: FilterBar::default(), basket: None, map: None,
        };
        render_scene(&gfx, &scene, "plumb-marked-row")
    };
    let before = shot(&basket);
    basket.toggle("root\\target.bin");
    let after = shot(&basket);
    let row = layout.row(1);
    let (x, y) = ((row.x + row.w - 4.0) as u32, (row.y + row.h / 2.0) as u32);
    assert_ne!(rgb_at(&before, x, y), rgb_at(&after, x, y), "the marked row's background changes");
    let first = layout.row(0);
    assert_eq!(rgb_at(&before, (first.x + first.w - 4.0) as u32, (first.y + 8.0) as u32), rgb_at(&after, (first.x + first.w - 4.0) as u32, (first.y + 8.0) as u32), "other rows are untouched");
}
```

Run `cargo test -p plumb -j 8 --test render --no-run`: fails to compile on the new fields (the failing state).

- [ ] **Step 2: Implement the fields and drawing.** In `draw.rs`:
  1. `View`: add `pub basket: &'a Basket,` (`use plumb_core::basket::Basket;`). `MapView`: add `/// Indices into `tiles` of tiles at or under a marked node: tinted over the cached bitmap. pub marked: &'a [usize],`. `Scene`: add `/// The marked-items chip's label, or `None` for no chip. pub basket: Option<&'a str>,`.
  2. Rows (`fn rows`): after the selection/hover fill and before the hairline, and make `fg` dim when marked:

```rust
            let marked = !v.basket.is_empty() && v.basket.covers(&v.tree.path(id));
            if marked {
                self.fill(r, D2D1_COLOR_F { a: 0.45, ..t.error });
            }
```

     and change `let fg = if row.dim { t.dim } else { t.fg };` to `let fg = if row.dim || marked { t.dim } else { t.fg };`. (`marked` must be declared before `fg`; place the new lines right after the `if v.model.selected() == Some(id) { .. } else if v.hover == Some(i) { .. }` block.)
  3. Map (`fn map`), after the hover frame and before the selection frame:

```rust
        for &i in m.marked {
            if let Some(tile) = m.tiles.get(i) {
                self.fill(tile.rect, D2D1_COLOR_F { a: 0.5, ..t.error });
                self.frame(tile.rect, D2D1_COLOR_F { a: 0.9, ..t.error });
            }
        }
```

  4. Chip: in the status-bar code (`Painter::scene`/where `reserved` is computed), set `let reserved = MAP_BTN_W + 8.0 + if l.cancel { CANCEL_W + 4.0 } else { 0.0 } + if l.basket { BASKET_W + 4.0 } else { 0.0 };` (import `BASKET_W`) and, after the Cancel button is drawn, add:

```rust
        if let (true, Some(label)) = (l.basket, s.basket) {
            let r = l.basket_chip();
            self.fill(r, D2D1_COLOR_F { a: 0.8, ..t.error });
            self.centered(label, r, t.fg);
        }
```

  5. `app.rs::with_scene`: pass `basket: &self.basket` in the `View` literal, `marked: &self.basket_view.tiles` in the `MapView` literal, and in the `Scene` literal `layout: self.layout.with_cancel(self.cancelable()).with_basket(self.basket_active()),` and `basket: basket_label.as_deref(),` with `let basket_label = self.basket_label();` beside the other locals at the top of `with_scene`.
  6. Update the remaining `View`/`MapView`/`Scene` literals the compiler reports (in `draw.rs` unit tests if any, and `tests/render.rs`).

- [ ] **Step 3: Run.** `cargo test -p plumb -j 8`. Expected: both render tests pass and the earlier render tests (`the_map_tiles_are_drawn_once_per_generation...`) still pass. `PLUMB_SHOT_DIR=<dir>` keeps PNGs if you want to look.
- [ ] **Step 4: Check, commit.** Clippy clean; commit "Draw: marked rows and tiles, and the basket chip".

---

### Task 6: window wiring: menu, commit, outcome

**Files:**
- Modify: `crates/plumb/src/window.rs`

**Interfaces:**
- Consumes: `App::{mark_state, toggle_mark, commit_request, set_deleting, finish_delete}`, `Effect::Commit`, `MarkState`, `dialog::{choose, confirm_permanent, Choice}`, `trash::{delete, Outcome, Verdict}`.

- [ ] **Step 1: Context menu entry.** Add `const CMD_MARK: usize = 4;` beside `CMD_REVEAL`. Change `context_menu(hwnd: HWND, path: String, at: (i32, i32))` to `context_menu(hwnd: HWND, id: NodeId, path: String, mark: MarkState, at: (i32, i32))`. After the "Copy path" item add a separator and the entry (greyed with the reason when refused):

```rust
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        match &mark {
            MarkState::Mark => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Mark for deletion\tDel")); }
            MarkState::Unmark => { let _ = AppendMenuW(menu, MF_STRING, CMD_MARK, w!("Unmark\tDel")); }
            MarkState::Refused(why) => {
                let text: Vec<u16> = format!("Cannot mark: {why}").encode_utf16().chain([0]).collect();
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, CMD_MARK, PCWSTR(text.as_ptr()));
            }
        }
```

     and in the `match cmd.0 as usize` add `CMD_MARK => { with(|w| { w.app.toggle_mark(id); w.invalidate(); }); }`. At the `Effect::Menu(id)` arm, compute the state in the same `with` that reads the path:

```rust
        Effect::Menu(id) => {
            let info = with(|w| w.app.tree.as_ref().map(|t| (fmt::display_path(&t.path(id)), w.app.mark_state(id)))).flatten();
            if let Some((p, mark)) = info {
                context_menu(hwnd, id, p, mark, at);
            }
        }
```

     (Import `MF_SEPARATOR`, `MF_GRAYED` with the other menu flags, and `crate::app::MarkState`.)

- [ ] **Step 2: Commit flow.** Add `const WM_DELETED: u32 = WM_APP + 4;` beside `WM_FILTER`. Add the effect arm `Effect::Commit => commit(hwnd),` and:

```rust
/// Asks what to delete and runs it on a worker thread; the result comes back as `WM_DELETED`.
/// Runs with no borrow held: the dialogs run their own message loops.
fn commit(hwnd: HWND) {
    let Some(req) = with(|w| w.app.commit_request()).flatten() else {
        with(|w| w.invalidate());
        return;
    };
    let choice = dialog::choose(hwnd, &req.paths, req.bytes);
    let permanent = choice == dialog::Choice::Permanent;
    match choice {
        dialog::Choice::Cancel => {}
        dialog::Choice::Clear => {
            with(|w| {
                w.app.clear_basket();
                w.invalidate();
            });
        }
        _ if permanent && !dialog::confirm_permanent(hwnd, req.paths.len(), req.bytes) => {}
        _ => {
            with(|w| w.app.set_deleting(true));
            let (owner, drive, paths) = (hwnd.0 as isize, req.drive, req.paths);
            std::thread::spawn(move || {
                let out = trash::delete(paths, permanent, owner);
                let boxed = Box::into_raw(Box::new((drive, out)));
                // The window may be gone by now; then the box leaks, which is harmless at exit.
                let _ = unsafe { PostMessageW(Some(HWND(owner as *mut _)), WM_DELETED, WPARAM(0), LPARAM(boxed as isize)) };
            });
        }
    }
    with(|w| w.invalidate());
}
```

     `App::clear_basket` is new: add it in Task 4 step 2 as `pub fn clear_basket(&mut self) { self.basket.clear(); self.basket_rev += 1; }` (`begin_scan` uses the same two lines).

- [ ] **Step 3: Apply the outcome.** In the window procedure, add near the `WM_SCAN_DONE` arm:

```rust
        WM_DELETED => {
            let (drive, out) = *unsafe { Box::from_raw(lparam.0 as *mut (usize, Vec<trash::Outcome>)) };
            with(|w| w.finish_delete(drive, out));
            LRESULT(0)
        }
```

     and on the `Win` impl (window.rs `impl Win`):

```rust
    /// A delete batch ended: unmark what went, keep what failed, say so on the status line.
    fn finish_delete(&mut self, drive: usize, out: Vec<trash::Outcome>) {
        let reason = |hr: i32| windows::core::Error::from(windows::core::HRESULT(hr)).message();
        let gone: Vec<String> = out.iter().filter(|o| !matches!(o.verdict, trash::Verdict::Failed(_))).map(|o| o.path.clone()).collect();
        let failed: Vec<(String, String)> = out.iter().filter_map(|o| match o.verdict { trash::Verdict::Failed(hr) => Some((o.path.clone(), reason(hr))), _ => None }).collect();
        if self.in_slot(drive, |w| w.app.finish_delete(&gone, &failed)).is_none() {
            self.app.set_deleting(false); // the drive's slot was evicted meanwhile
        }
        self.invalidate();
    }
```

     Import `crate::{dialog, trash}`. If `in_slot` for the shown drive makes `finish_delete` run on `self.app` directly, that is the intended path.

- [ ] **Step 3b: Del on the map.** `key_input` already maps Delete to `Input::Edit(Edit::Delete)`, so `Del` works for the tree and the map alike (it marks the *selected* node; a map click selects). Nothing to add. Confirm by reading `key_input` once.
- [ ] **Step 4: Build and run the suite.** `cargo build -p plumb -j 8`, `cargo test --workspace -j 8`, `cargo clippy --workspace --all-targets -j 8`, `cargo run -p plumb -- --selftest`, `cargo run -p plumb -- --selftest-watch`. All clean/ok.
- [ ] **Step 5: Manual run (the dialog, the shell prompts, Explorer undo, tooltips).** Build release into the normal target dir and run it against a scratch folder you create (never a real one): `cargo run --release -p plumb`, scan `C:`, mark a file you created under `%LOCALAPPDATA%\plumb-manual\` with `Del`, check: the row tints; the chip reads "1 marked, up to ..."; the chip click shows the dialog with the path list expandable; Recycle removes it from the tree within a few seconds, and Explorer's Undo (Ctrl+Z in a folder window) brings it back; mark a folder and a child, the chip counts one; open a second file with an exclusive lock (`powershell` `[IO.File]::Open(path,'Open','Read','None')`), mark and Delete permanently: the row stays marked with the reason on the status line; tooltips and the context menu still look right under the v6 manifest (compare to the `docs/shots` capture). Record what you saw; if the tree does not drop a deleted row within ~10 s, add the fallback in the next step.
- [ ] **Step 6 (only if Step 5 showed stale rows): explicit parent sync.** In `Window::finish_delete`, after a successful `in_slot`, for each deleted path remove its node: `if let Some(t) = &mut w.app.tree { if let Some(id) = t.find(&path) { t.remove(id); } }` then `w.app.rows_changed()` through whatever call the live path uses (`grep -n "take_touched" crates/plumb/src` to copy it). The live/dirsync appliers are idempotent for an already-removed node (their tests: "applying a batch twice changes nothing"), so this is a second source for the same fact, not a second path. If Step 5 passed, skip this step and note that in the commit message.
- [ ] **Step 7: Commit.** "Window: mark from the menu and Del, commit through the dialog, apply the result".

---

### Task 7: docs and the final pass

**Files:**
- Modify: `CHANGELOG.md`, `docs/superpowers/specs/2026-10-15-plumb-delete-design.md`, `docs/superpowers/specs/2026-10-11-plumb-disk-design.md`

- [ ] **Step 1: Docs.**
  - `CHANGELOG.md` `[Unreleased]` → `### Added`: "`plumb`: delete from the tree and the map. `Del` or the context menu marks the selected row or tile (a basket per drive; marked rows and tiles are tinted); the status bar chip shows how many are marked and how much they hold, and opens a dialog to Recycle (undoable in Explorer) or delete permanently, in one shell batch. Drive roots, the Windows folder and a drive that is scanning cannot be marked; a failed item stays marked with its reason. `plumb-core`: `basket`, `Tree::find`. The exe now embeds a Common Controls v6 manifest."
  - Delete design: set the status line to "implemented 2026-10-15"; replace the "Testing" bullet about `--selftest` with "`trash`'s own tests delete files in a scratch directory permanently; the dialog and the Recycle path are checked by hand (Task 6 step 5 of the plan)"; change the Guards sentence "matching is case-insensitive" wherever it appears to "exact" (the basket compares exact paths; only the Windows-folder guard ignores case); remove the stale `README` mention in Risks (the project has none) and keep the disk-design line.
  - Disk design: in the row `| Delete and cleanup suggestions | Read-only tool by decision |` append ` (deletion reversed 2026-10-15: see 2026-10-15-plumb-delete-design.md; cleanup suggestions still out)`.
- [ ] **Step 2: Final verification.** `cargo test --workspace -j 8`, `cargo clippy --workspace --all-targets -j 8`, `cargo run -p plumb -- --selftest`, `cargo run -p plumb -- --selftest-watch`; `tasklist | findstr /i plumb` shows nothing left; `git status` clean except `docs/shots/`; no scratch files left under `%LOCALAPPDATA%` (`dir $env:LOCALAPPDATA\plumb-trash-*, $env:LOCALAPPDATA\plumb-manual*`).
- [ ] **Step 3: Commit.** "Docs: delete feature documented".
