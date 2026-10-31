---
status: draft
---

# Files held refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Files held list for the selected process re-reads itself when the process's open-handle count has changed (at most once per 10 snapshots), and when the selected row is clicked again, keeping the scroll position.

**Architecture:** The lookup (`handles::open_files`) reads the whole system handle table (75-96 ms for a busy process), so it is not polled. The Processes snapshot already walks `SystemProcessInformation`; the handle count is a free `u32` at byte offset 96 of each entry (next to the parent id at 88). It is carried as `ProcInfo.handles`. The app counts snapshots (`Held.age`, one per `set_procs`), so the 10 s gate is the poll count and needs no clock, and it stops when polling stops. A refresh is one flag, `Held.stale`, set by two causes (the count changed after 10 snapshots; the selected row clicked again). The window's existing `held_poll` (run on every paint) takes the flag with `held_take_refresh()` and starts the same worker thread as the first lookup, now a shared `spawn_held`. The old list stays on screen until the answer replaces it.

**Tech Stack:** Rust; `plumb-scan::procs`, `plumb` (`app.rs`, `window.rs`, `window/selftest.rs`).

**Spec:** `ROADMAP.md` v0.10 (second part); the design and measurements are in `PARKED.md` ("The Files held list cannot refresh"); the pane is specified in `docs/superpowers/specs/2026-10-16-plumb-views-design.md`.

## Global Constraints

- Idle CPU stays under 1 % (`PLUMB_PROCS_SECS=60 plumb --selftest-processes` reports it); worst case of the design is about 0.8 % of a core, near 0 when quiet.
- The first lookup is unchanged: it still waits for the 300 ms settle (`HELD_SETTLE_MS`) and shows "reading…".
- A refresh never blanks the list: the old lines stay until the new answer arrives, and the scroll offset is kept and clamped.
- No compatibility shims. Build with `CARGO_TARGET_DIR=target/alt`; delete it afterwards. Edits use the Edit tool; commits are small; nothing is pushed or tagged.
- GUI selftests run hidden as the existing ones do (no focus stealing); no stray processes.

## Review Focus

- A process whose handle count changes every second (the busy case): at most one lookup per 10 snapshots, never one overlapping another.
- Selecting a different process while a refresh is in flight: the late answer for the old process is dropped (epoch), the new selection starts clean (`stale` and `refreshing` reset).
- A refresh answer that is shorter than the scroll offset: the offset is clamped, not past the end.
- A process that exits while selected, or whose lookup is denied: no refresh loop (only a ready, readable list refreshes).
- Clicking the selected row's expander/icon (folder toggle) must not count as a re-click (`again` requires `folder.is_none()`; reviewer checks it by reading, no unit test reaches a row hit).
- The hidden-window case: a snapshot alone (no paint) must start the refresh, which is why `procs_update` calls `held_poll`.

---

### Task 1: Read the handle count (`plumb-scan`)

**Files:**
- Modify: `crates/plumb-scan/src/procs.rs` (`ProcEntry`, `read`, test)

**Interfaces:**
- Produces: `ProcEntry.handles: u32`.

- [ ] **Step 1: Write the failing test.** In the `tests` module of `procs.rs`, after `our_own_process_is_listed_with_a_plausible_working_set`, add:

```rust
    #[test]
    fn the_handle_count_follows_the_handles_a_process_opens() {
        let me = std::process::id();
        let count = || snapshot().unwrap().iter().find(|p| p.pid == me).expect("own pid listed").handles;
        let before = count();
        assert!(before > 0, "a process always holds handles");
        let dir = std::env::temp_dir().join(format!("plumb-handles-{me}"));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::fs::File> = (0..20).map(|i| std::fs::File::create(dir.join(format!("f{i}"))).unwrap()).collect();
        let during = count();
        drop(files);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(during >= before + 20, "{during} while 20 more files were open, {before} before");
    }
```

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-scan the_handle_count 2>&1 | grep -E "^error|test result"`
Expected: compile error, `no field handles`.

- [ ] **Step 3: Implement.** In `ProcEntry` add after `pub io_bytes: u64,`:

```rust
    /// Open handles of all kinds (`HandleCount`, byte offset 96); a cheap sign that the files held may have changed.
    pub handles: u32,
```

and in `read`, in the `ProcEntry { ... }` literal add `handles: u32::from_le_bytes(e[96..100].try_into().unwrap()),` after `io_bytes: ...,`.

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-scan procs 2>&1 | grep -E "^error|test result"`
Expected: pass. (The `plumb` crate builds on `ProcEntry` fields only by name, so nothing else breaks.)

- [ ] **Step 5: Commit.**
```bash
git add crates/plumb-scan/src/procs.rs
git commit -m "Processes snapshot: read each process's handle count"
```

---

### Task 2: Decide when to look again (`plumb::app`)

**Files:**
- Modify: `crates/plumb/src/app.rs` (`ProcInfo`, `Held`, `held_sync`, `held_request`, `held_done`, `set_procs`, `Hit::Row` in `click`; new `held_take_refresh`), `crates/plumb/src/window/procs.rs:139` (fill `handles`)
- Test: `crates/plumb/src/app.rs` tests module (next to `selecting_a_process_requests_its_files_once_it_settles`)

**Interfaces:**
- Consumes: `ProcEntry.handles`.
- Produces: `ProcInfo.handles: u32`; `const HELD_REFRESH_SNAPSHOTS: u32 = 10`; `App::held_take_refresh(&mut self) -> Option<(u64, u32)>`: the `(epoch, pid)` to look up again, once, when a refresh is due.

- [ ] **Step 1: Write the failing tests.** Add to the `app.rs` tests module (they use `process_app()` from the neighbouring tests: one process `ping.exe`, pid 77, selected by the tests below). Add a helper next to them:

```rust
    /// One Processes snapshot as `set_procs` sees it: pid 77 with `handles` open handles.
    fn snapshot_with(a: &mut App, handles: u32) {
        let info = HashMap::from([(77u32, ProcInfo { path: None, created: 5, handles })]);
        a.set_procs(2, info, String::new(), (16 << 30, 4 << 30));
    }

    /// `process_app` with pid 77 selected, its first lookup asked and answered with `files`.
    fn held_ready(files: &[&str]) -> App {
        let (mut a, id) = process_app();
        snapshot_with(&mut a, 100);
        a.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (epoch, _) = a.held_request().unwrap();
        a.held_done(epoch, Ok(HandleList { files: files.iter().map(|s| s.to_string()).collect(), skipped: 0 }));
        a
    }
```

```rust
    #[test]
    fn a_changed_handle_count_refreshes_the_list_after_ten_snapshots() {
        let mut a = held_ready(&["C:/a"]);
        for _ in 0..HELD_REFRESH_SNAPSHOTS - 1 {
            snapshot_with(&mut a, 120);
            assert_eq!(a.held_take_refresh(), None, "changed, but not ten snapshots since the lookup");
        }
        snapshot_with(&mut a, 120);
        let (epoch, pid) = a.held_take_refresh().expect("ten snapshots and a changed count");
        assert_eq!(pid, 77);
        assert_eq!(a.held_take_refresh(), None, "asked once");
        assert!(a.held_text().unwrap().1.contains(&"C:/a".to_string()), "the old list stays until the answer");
        a.held_done(epoch, Ok(HandleList { files: vec!["C:/b".into()], skipped: 0 }));
        assert_eq!(a.held_text().unwrap().1, vec!["C:/b".to_string()]);
    }

    #[test]
    fn an_unchanged_handle_count_never_refreshes() {
        let mut a = held_ready(&["C:/a"]);
        for _ in 0..3 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 100);
            assert_eq!(a.held_take_refresh(), None);
        }
    }

    #[test]
    fn a_busy_process_is_looked_up_at_most_once_per_ten_snapshots_and_never_twice_at_once() {
        let mut a = held_ready(&["C:/a"]);
        let mut asked = 0;
        for n in 0..4 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 200 + n); // a different count every snapshot
            if let Some((epoch, _)) = a.held_take_refresh() {
                asked += 1;
                snapshot_with(&mut a, 500 + n);
                assert_eq!(a.held_take_refresh(), None, "one is already in flight");
                a.held_done(epoch, Ok(HandleList::default()));
            }
        }
        assert!(asked <= 4, "{asked} lookups in {} snapshots", 4 * HELD_REFRESH_SNAPSHOTS);
        assert!(asked >= 3, "{asked}: a changing count must keep refreshing");
    }

    #[test]
    fn clicking_the_selected_row_again_refreshes_at_once() {
        let mut a = held_ready(&["C:/a"]);
        a.held_mark_stale(); // what a click on the selected row does
        let (epoch, pid) = a.held_take_refresh().expect("a re-click asks again without waiting for ten snapshots");
        assert_eq!(pid, 77);
        a.held_done(epoch, Ok(HandleList { files: vec!["C:/c".into()], skipped: 0 }));
        assert_eq!(a.held_text().unwrap().1, vec!["C:/c".to_string()]);
    }

    #[test]
    fn a_refresh_answer_for_another_selection_is_dropped_and_the_scroll_is_clamped() {
        let many: Vec<String> = (0..20).map(|i| format!("C:/f{i:02}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut a = held_ready(&refs);
        a.held_scroll(100); // as far down as it goes
        a.held_mark_stale();
        let (epoch, _) = a.held_take_refresh().unwrap();
        let shorter = HandleList { files: many[..crate::layout::HELD_ROWS + 1].to_vec(), skipped: 0 };
        a.held_done(epoch, Ok(shorter));
        let (_, lines) = a.held_text().unwrap();
        assert_eq!(lines.len(), crate::layout::HELD_ROWS, "the offset is clamped to the shorter list");
        // a late answer after the selection moved is dropped
        let (mut b, id) = process_app();
        snapshot_with(&mut b, 100);
        b.model.as_mut().unwrap().select(id);
        b.held_sync();
        let (e1, _) = b.held_request().unwrap();
        b.held_done(e1, Ok(HandleList { files: vec!["C:/x".into()], skipped: 0 }));
        b.held_mark_stale();
        let (e2, _) = b.held_take_refresh().unwrap();
        let root = b.model.as_ref().unwrap().rows()[0].id;
        b.model.as_mut().unwrap().select(root);
        b.held_sync();
        b.held_done(e2, Ok(HandleList { files: vec!["C:/y".into()], skipped: 0 }));
        assert!(b.held_text().is_none(), "no pane for the root, and the old answer did not revive one");
    }

    #[test]
    fn a_denied_or_failed_lookup_is_not_refreshed() {
        let (mut a, id) = process_app();
        snapshot_with(&mut a, 100);
        a.model.as_mut().unwrap().select(id);
        a.held_sync();
        let (epoch, _) = a.held_request().unwrap();
        a.held_done(epoch, Err(HandleError::Denied));
        for _ in 0..2 * HELD_REFRESH_SNAPSHOTS {
            snapshot_with(&mut a, 999);
            assert_eq!(a.held_take_refresh(), None);
        }
        a.held_mark_stale();
        assert_eq!(a.held_take_refresh(), None, "nothing to refresh when it could not be read");
    }
```

(`HandleError` and `HandleList` are already imported in `app.rs`; if `HandleList` has no `Default`, it does: `#[derive(Debug, Default)]` at `handles.rs:19`.)

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb --lib held 2>&1 | grep -E "^error|test result" | head`
Expected: compile errors (`no field handles` on `ProcInfo`; `held_take_refresh`, `held_mark_stale`, `HELD_REFRESH_SNAPSHOTS` not found).

- [ ] **Step 3: Implement.**

`ProcInfo` gains `/// Open handles when last seen; a change suggests the files held changed.\n    pub handles: u32,`. In `window/procs.rs:139` the literal becomes `ProcInfo { path: e.path, created: e.created, handles: e.handles }`.

`Held` gains (all default false/0):

```rust
    /// The handle count when the list was last asked for.
    handles: u32,
    /// Snapshots since the list was last asked for.
    age: u32,
    /// A fresh look is wanted and not yet started.
    stale: bool,
    /// A fresh look is in flight (the old list stays on screen meanwhile).
    refreshing: bool,
```

Near the top of the file with the other consts: `/// Snapshots (one a second while the view is on screen) between two refreshes of the Files held list.\nconst HELD_REFRESH_SNAPSHOTS: u32 = 10;`

Private helper:

```rust
    /// The handle count of the process the pane is about, from the latest snapshot.
    fn held_handles(&self) -> Option<u32> {
        self.proc_info.get(&self.held.pid?).map(|i| i.handles)
    }

    /// Whether the pane shows a list that was read (a refresh has something to replace).
    fn held_readable(&self) -> bool {
        matches!(self.held.state, HeldState::Ready(Ok(_)))
    }
```

`held_request`: after `self.held.state = HeldState::Loading;` add `self.held.handles = self.held_handles().unwrap_or(0); self.held.age = 0;`. (`held_sync`'s `Held { .. }` literal gets `..Held::default()` for the new fields, keeping `pid`, `epoch`, `state`, `scroll` as written.)

`set_procs`: at the end add

```rust
        self.held.age = self.held.age.saturating_add(1);
        if self.held_readable() && !self.held.refreshing && self.held.age >= HELD_REFRESH_SNAPSHOTS && self.held_handles().is_some_and(|n| n != self.held.handles) {
            self.held.stale = true;
        }
```

New methods:

```rust
    /// Wants a fresh look at the files held (the selected row was clicked again). Does nothing when there is no
    /// readable list to replace.
    pub fn held_mark_stale(&mut self) {
        if self.held_readable() && !self.held.refreshing {
            self.held.stale = true;
        }
    }

    /// The (epoch, pid) to look up again, once, if a refresh is due; the old list stays until `held_done`.
    pub fn held_take_refresh(&mut self) -> Option<(u64, u32)> {
        let pid = self.held.pid.filter(|_| self.held.stale && !self.held.refreshing && self.held_readable())?;
        self.held.stale = false;
        self.held.refreshing = true;
        self.held.handles = self.held_handles().unwrap_or(self.held.handles);
        self.held.age = 0;
        Some((self.held.epoch, pid))
    }
```

`held_done`: accept the answer when `epoch` matches and the state is `Loading` or `refreshing` is set; on accept set `refreshing = false` and clamp the scroll:

```rust
    pub fn held_done(&mut self, epoch: u64, result: Result<HandleList, HandleError>) {
        if epoch == self.held.epoch && (matches!(self.held.state, HeldState::Loading) || self.held.refreshing) {
            self.held.refreshing = false;
            self.held.state = HeldState::Ready(result);
            self.held_scroll(0); // keeps the offset, clamped to the new list
        }
    }
```

`held_scroll(0)` already clamps to `0..=n - HELD_ROWS`; with an `Err` result `n` is 0 and the offset becomes 0.

`Hit::Row(i)` in `click` (`app.rs:1360`) becomes:

```rust
            Hit::Row(i) => {
                let folder = self.icon_at(i, x);
                let mut again = false;
                if let (Some(id), Some(t), Some(m)) = (self.row_at(i), &self.tree, &mut self.model) {
                    again = folder.is_none() && m.selected() == Some(id);
                    m.select(id);
                    if folder.is_some() {
                        m.toggle(t, id);
                        self.set_scroll(self.scroll);
                    }
                }
                if again {
                    self.held_mark_stale();
                }
            }
```

(If the borrow checker rejects `self.set_scroll` inside the `if let`, that is the existing code's shape; leave it as it is.) `held_mark_stale` is a no-op for a disk view or a pending/denied list, so a re-click there does nothing. The wiring (three lines) is covered by the selftest in Task 3, not a unit test.

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb --lib 2>&1 | grep -E "^error|test result|FAILED"`
Expected: all pass, including the six new tests and the existing `held` tests.

- [ ] **Step 5: Commit.**
```bash
git add crates/plumb/src/app.rs crates/plumb/src/window/procs.rs
git commit -m "Files held: refresh when the handle count changed, or on a re-click"
```

---

### Task 3: Start the refresh in the window, and prove it end to end

**Files:**
- Modify: `crates/plumb/src/window.rs` (`held_poll`, the `WM_TIMER`/`TIMER_HELD` handler), `crates/plumb/src/window/selftest.rs` (the Processes selftest)

**Interfaces:**
- Consumes: `App::held_take_refresh`.
- Produces: `fn spawn_held(hwnd: HWND, epoch: u64, pid: u32)` in `window.rs`, the one place the lookup thread starts.

- [ ] **Step 1: Extract `spawn_held`.** Replace the thread-spawning body of the `WM_TIMER if wp.0 == TIMER_HELD` arm with a call to a new free function, and add the function below `held_poll`'s `impl`:

```rust
/// Looks up the files `pid` holds on a worker thread and posts the answer back as `WM_HELD` (the epoch tells the app
/// which selection it was for).
fn spawn_held(hwnd: HWND, epoch: u64, pid: u32) {
    let owner = hwnd.0 as isize;
    std::thread::spawn(move || {
        let boxed = Box::into_raw(Box::new((epoch, plumb_scan::handles::open_files(pid, HELD_PER_HANDLE))));
        if unsafe { PostMessageW(Some(HWND(owner as *mut _)), WM_HELD, WPARAM(0), LPARAM(boxed as isize)) }.is_err() {
            drop(unsafe { Box::from_raw(boxed) });
        }
    });
}
```

The arm becomes:

```rust
        WM_TIMER if wp.0 == TIMER_HELD => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_HELD) };
            if let Some(Some((epoch, pid))) = with(|w| w.app.held_request()) {
                spawn_held(hwnd, epoch, pid);
            }
            LRESULT(0)
        }
```

- [ ] **Step 2: Start refreshes from `held_poll`, and call it after each snapshot.** `held_poll` runs from `WM_PAINT` (so after a click) but a hidden window never paints, and a snapshot that only changes a count need not repaint; so `procs_update` calls it too. `held_poll` becomes:

```rust
    fn held_poll(&mut self) {
        if self.app.held_sync() {
            unsafe { SetTimer(Some(self.hwnd), TIMER_HELD, HELD_SETTLE_MS, None) };
        }
        if let Some((epoch, pid)) = self.app.held_take_refresh() {
            spawn_held(self.hwnd, epoch, pid);
        }
    }
```

and in `procs_update` (`window.rs:933`), after the `if st.update(...) { ... }` block and before `self.invalidate();`, add `self.held_poll();`.

- [ ] **Step 3: Build and run the unit tests.**
Run: `CARGO_TARGET_DIR=target/alt cargo test --workspace --locked 2>&1 | grep -E "^error|test result|FAILED"` and `CARGO_TARGET_DIR=target/alt cargo clippy --workspace --all-targets 2>&1 | tail -3`
Expected: pass, clippy clean.

- [ ] **Step 4: Extend the Processes selftest.** In `selftest.rs`, after the wheel assertions and before `drop(extra);` add a step that closes the ten extra files and waits for the pane to notice (about 10 snapshots, so up to ~15 s):

```rust
        // The list follows the process: close the ten extra files and the pane re-reads itself once the handle count
        // has changed and ten snapshots have passed (no click, no new selection).
        // The title reads "Files held by <name> (PID n): <count> files ..."; take the count.
        let count_of = |w: &mut Win| -> Option<usize> {
            let title = w.app.with_scene(false, |s| s.held.map(|(h, _)| h.to_string()))?;
            title.split(": ").nth(1)?.split(' ').next()?.parse().ok()
        };
        let n0 = with(count_of).flatten().ok_or_else(|| fail("the pane has no file count".into()))?;
        drop(extra);
        wait("the pane noticed the closed files", 35, &|w| count_of(w).is_some_and(|n| n + 10 <= n0))?;
```

and remove the now-duplicate `drop(extra);` that follows. (35 s: a refresh may already have fired just before the drop, so the one that sees the closed files can be a full ten snapshots later, plus the lookup.) Update the closing `Ok(format!(...))` text to say "...the wheel scrolls it and it refreshes when the process closes files".

- [ ] **Step 5: Run the selftest hidden and the idle check.**
Run (from a release build in `target/alt`, as the existing selftests are run; it opens no visible window): `PLUMB_PROCS_SECS=60 target/alt/release/plumb.exe --selftest-processes` after `CARGO_TARGET_DIR=target/alt cargo build --release -p plumb`.
Expected: prints the OK line including the refresh clause; the idle line reports under 1 % of one core. If the selftest cannot run unattended, record that in `PARKED.md` as a live check (as the 0.9.0 ones are) and say so in the closing report.

- [ ] **Step 6: Commit.**
```bash
git add crates/plumb/src/window.rs crates/plumb/src/window/selftest.rs
git commit -m "Files held: the window starts a refresh when the app asks, with a selftest"
```

---

### Task 4: Docs

**Files:**
- Modify: `PARKED.md` (delete "The Files held list cannot refresh"; add the new live-check line if Step 3.5 could not run), `docs/superpowers/specs/2026-10-16-plumb-views-design.md` (the Files held section and its parked list), this plan's front matter.

- [ ] **Step 1:** Delete the PARKED entry; keep its measurements by moving the cost sentence ("a lookup costs 75-96 ms ... 1.6 % of a core") into the views spec's Files held section as the reason for the handle-count rule.
- [ ] **Step 2: Grep for stale wording.**
Run: `grep -rniE "cannot refresh|read once|refresh" PARKED.md ROADMAP.md CLAUDE.md docs crates --include=*.md --include=*.rs | grep -v "docs/superpowers/plans/" | grep -i "held"`
Expected: no remaining claim that the list is read once; fix each hit.
- [ ] **Step 3:** Set `status: done`; commit.
```bash
git add PARKED.md docs
git commit -m "Docs: the Files held refresh landed; PARKED and the views spec follow"
```
