# Views (Processes + what holds it) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Processes view beside the drives (tree by parent process, sized by memory, map-able by memory / CPU / I/O), plus "what holds it": which program holds a file (failed delete, context item) and which files a process holds. Ships as 0.9.0.

**Architecture:** A view is still a `Drive` slot; a `DriveInfo` gains a `ViewKind` (`Disk` | `Processes`). The kind picks the column titles, the sort keys and the cells of columns 3 and 4 (everything else in the row table, the map and the filter is shared). Processes has no scan: a worker thread polls a process snapshot every second and a pure reconciler edits the slot's `Tree` in place (`add_child` / `set_size` / `reparent` / `remove`), so row ids, selection and expansion survive.

**Tech Stack:** Rust, `windows` crate (add `Wdk`-free: own `repr(C)` for `SYSTEM_PROCESS_INFORMATION` via `ntdll!NtQuerySystemInformation`, Restart Manager `Win32_System_RestartManager`), existing `plumb-core` / `plumb-scan` / `plumb`.

**Spec:** `docs/superpowers/specs/2026-10-16-plumb-views-design.md` (Task 0 below corrects two points in it).

## Global Constraints

- Disk views must not change: render frames of a drive view are byte-identical before / after Tasks 2–3 (compare `PLUMB_SHOT_DIR` PNGs from the 0.8.0 commit `f3b7d30`); `--selftest-delete`, `--selftest-live`, `--selftest-watch` stay green at every commit.
- No compatibility shims; delete what a new mechanism supersedes. The state file keeps loading old files (three remembered column widths).
- Long / GUI runs follow `~/.claude/LONG_RUNS.md` (no focus stealing beyond the existing selftests; BelowNormal; no stray processes).
- Python never reads stdin. Subagents editing files must be told so.
- Build in `CARGO_TARGET_DIR=target/alt` while the user's `plumb.exe` runs; delete it afterwards.
- Windows only (`#[cfg(windows)]` as the surrounding code does).
- Read-only Processes: no end / suspend; `Del` and the basket do nothing there.

## Rulings (decided while writing this plan; each costs little if wrong)

- **Columns reuse the 5-column geometry.** The row table is Name + 4 columns today (Size, % of parent, Items, Modified; `layout.col_w: [f32; 4]`). Processes is Name | Memory | % of parent | CPU | I/O. Same widths, same resize and state file. The PID is appended to the name (`chrome.exe  (1234)`, dimmed, like `(access denied)`), the path and start time are in the tooltip. The spec's "Path / PID columns" are dropped.
- **No `Source` trait object.** What differs per kind is data and four small functions (titles, sort keys, cell text for columns 3 and 4, map metrics), as methods on `ViewKind`. A trait would have one implementor per kind and no state.
- **Names stay `Drive` / `DriveInfo` / `KEPT_DRIVES`.** A rename across ~4,000 lines is churn with no behaviour; the last task (optional) does it as one mechanical commit if the user wants it.
- **Rebuild vs edit:** edit in place (see Architecture), because `RowModel` expansion and selection are keyed by `NodeId`.

## Review Focus

- A process that exits and whose PID is reused within one poll (same PID, new creation time) is a new node, not an update.
- A parent that exits while children live on: children become roots (or re-parent to the new nearest live ancestor), and the tree never has a cycle (PID 0 / 4, and parent PIDs that point at a descendant, which Windows allows).
- Parking the Processes view while it polls: polling stops, the chip shows stale; unparking resumes and the first snapshot after a long gap must not show a huge CPU % (deltas over the real elapsed time).
- Non-elevated: protected processes give no path / no handles; the UI says so instead of showing blanks as zero.
- `NtQueryObject` hanging on a pipe handle must not hang the UI or leak the worker.
- The failure dialog with no holder found (Restart Manager returns nothing, or the file vanished) keeps the old reason.

---

### Task 0: Correct the spec

**Files:** Modify `docs/superpowers/specs/2026-10-16-plumb-views-design.md`.

- [ ] **Step 1:** In section 1 replace the `trait Source` block and the two bullets after it with: columns are `Name | Size-metric | % of parent | col3 | col4`; `ViewKind` supplies titles, `SortKey`s, and the text of columns 3 and 4 (`Disk`: Items, Modified; `Processes`: CPU, I/O); `Tree::aux: Vec<[u64; 3]>` (CPU, I/O, PID) and `SortKey::Aux(i)` stay; `Tree::size` is the selected map metric. In section 2 replace the "Columns" bullet with the Rulings above (PID in the name, no Path column), and the "Tree" bullet's "a process whose parent is gone becomes a root" with "hangs directly under one synthetic root node `Processes`" (a forest is impossible: `Tree::add_child` takes no `NONE` parent and `Tree::remove` refuses roots); add the churn limit (dead nodes stay until a rebuild at 20,000).
- [ ] **Step 2:** Commit: `git commit -am "Spec: views use the shared 5-column table; no Source trait"` (plus the Co-Authored-By line).

### Task 1: Spike — file handles of a process (gate for Task 9)

**Files:** Create `crates/plumb-scan/src/handles.rs`; modify `crates/plumb-scan/src/lib.rs` (`pub mod handles;`), `crates/plumb-scan/Cargo.toml` (add `Win32_System_Diagnostics_ToolHelp` only if needed, `Win32_System_LibraryLoader` for `GetProcAddress`).

**Interfaces:**
- Produces: `pub fn open_files(pid: u32, per_handle_timeout: Duration) -> Result<HandleList, HandleError>` where `pub struct HandleList { pub files: Vec<String>, pub skipped: usize }` (paths as NT-to-DOS converted strings; `skipped` counts handles whose name lookup timed out or failed) and `HandleError::Denied` when the target cannot be opened for `PROCESS_DUP_HANDLE`.

- [ ] **Step 1: Write the ignored spike test** (`#[test] #[ignore]` at the bottom of `handles.rs`): create a temp file, open it with `File::open`, call `open_files(std::process::id(), 500ms)`, assert the list contains the temp path (case-insensitive) and that `skipped` is small; also create a named pipe server handle (`std::fs::OpenOptions` on `\\.\pipe\plumb-spike`, via `CreateNamedPipeW` with a pending read) and assert the call still returns in under 5 s. Then call it for the parent shell's PID (`std::env::var("PLUMB_SPIKE_PID")`) and print the count.
- [ ] **Step 2: Run, expect failure** `cargo test -p plumb-scan handles -- --ignored` → fails: `open_files` not defined.
- [ ] **Step 3: Implement.** `NtQuerySystemInformation(SystemExtendedHandleInformation = 64)` into a growing buffer (retry on `STATUS_INFO_LENGTH_MISMATCH` `0xC0000004`, doubling from 4 MiB); parse `SYSTEM_HANDLE_INFORMATION_EX { NumberOfHandles: usize, Reserved: usize, Handles[] }` with entries `{ Object: usize, UniqueProcessId: usize, HandleValue: usize, GrantedAccess: u32, CreatorBackTraceIndex: u16, ObjectTypeIndex: u16, HandleAttributes: u32, Reserved: u32 }` (40 bytes); keep entries with `UniqueProcessId == pid`; open the process with `PROCESS_DUP_HANDLE`; for each handle `DuplicateHandle(target, h, current, &mut dup, 0, false, DUPLICATE_SAME_ACCESS)`, the probe (`GetFileType(dup) == FILE_TYPE_DISK`, then `GetFinalPathNameByHandleW(dup, VOLUME_NAME_DOS)` with the `\\?\` prefix stripped) **runs entirely on a worker thread** joined with `recv_timeout(per_handle_timeout)`: `GetFileType` itself can hang on a synchronous pipe handle with a read pending, so it must not run on the caller. On timeout count the handle as skipped and let the worker finish detached (it owns only its duplicate, which it closes when the call returns); after 8 hung workers stop probing that process (`skipped` = the rest), so a pathological process cannot leak threads without bound. `GetFinalPathNameByHandleW` is used instead of `NtQueryObject`: for disk handles it does not hang and it avoids the NT-path conversion.
- [ ] **Step 4: Run, expect pass; record the numbers** (handle count, ms for our process, ms for a busy process like `explorer.exe`) in the test's output and in `docs/superpowers/specs/2026-10-16-plumb-views-design.md` section 4.
- [ ] **Step 5: Gate.** If the call for an ordinary user process takes over 2 s, or hangs, or `FILE_TYPE_DISK` filtering misses real files: stop, ledger a Ruling that Task 9 moves to 0.9.1, and continue with the other tasks. Commit either way (`git commit -m "Spike: open files of a process (handles.rs)"`).

### Task 2: `Tree::aux` and `SortKey::Aux`

**Files:** Modify `crates/plumb-core/src/tree.rs`, `crates/plumb-core/src/rows.rs`; tests in the same files.

**Interfaces:**
- Produces: `Tree.aux: Vec<[u64; 3]>` (public; **empty** for a disk tree, otherwise one entry per node), `Tree::set_aux(&mut self, id: NodeId, v: [u64; 3])`, `SortKey::Aux(u8)`.
- `TreeBuilder` and `add_child` keep `aux` empty unless `set_aux` has been called on that tree; `set_aux` first fills `aux` with zeros to `len()`; `add_child` pushes a zero entry when `aux` is non-empty.

- [ ] **Step 1: Failing tests** (in `tree.rs` tests): `aux_is_empty_for_a_built_tree`; `set_aux_grows_aux_to_the_node_count_and_add_child_keeps_it_in_step` (build 2 nodes, `set_aux(1, [5,6,7])`, `add_child(...)`, assert `aux.len() == tree.len()` and `aux[1] == [5,6,7]`, new node zeros); in `rows.rs` tests: `sorting_by_aux_orders_by_that_slot` (tree with 3 children and aux slot 0 = 3,1,2; `set_sort(&tree, SortKey::Aux(0), true)`; assert the row order is 3,1,2 descending → ids by value).
- [ ] **Step 2:** `cargo test -p plumb-core aux` → FAIL (items missing).
- [ ] **Step 3: Implement.** Add the field (initialised `Vec::new()` in `TreeBuilder::finish`), `set_aux`, the `add_child` push, and in `rows.rs::sorted_children`'s `match` the arm `SortKey::Aux(i) => tree.aux.get(a).map_or(0, |v| v[i as usize]).cmp(&tree.aux.get(b).map_or(0, |v| v[i as usize]))`. Check `Tree::remove` / `reparent` need no change (ids are stable; removed nodes keep their slot, as the other vectors do).
- [ ] **Step 4:** `cargo test --release -p plumb-core` → all pass. Commit.

### Task 3: Columns by view kind (drive frames unchanged)

**Files:** Modify `crates/plumb/src/app.rs` (`DriveInfo`, `sort_key`, `State` sort column mapping), `crates/plumb/src/draw.rs` (header titles at the `["Name", "Size", ...]` loop; row cells for columns 3 and 4; `Scene` gets `kind: ViewKind`), `crates/plumb/src/drives.rs` (`DriveInfo { kind: ViewKind::Disk, .. }`), `crates/plumb/tests/render.rs` (Scene initialisers).

**Interfaces:**
- Produces in `app.rs`: `#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)] pub enum ViewKind { #[default] Disk, Processes }` with `fn titles(self) -> [&'static str; 5]` (`Disk`: `["Name","Size","% of parent","Items","Modified"]`; `Processes`: `["Name","Memory","% of parent","CPU","I/O"]`), `fn sort_key(self, col: usize) -> SortKey` (`Disk`: today's table; `Processes`: `[Name, Size, Size, Aux(0), Aux(1)]`), `fn col3_4(self, tree: &Tree, id: NodeId) -> (String, Option<String>)` (Disk: `(count(subtree_files), mtime.map(date))`; Processes: `(format!("{:.1}%", aux[id][0] as f64 / 10.0)`, `Some(rate(aux[id][1]))`; CPU stored in tenths of a percent, I/O in bytes per second).
- `DriveInfo.kind: ViewKind`; `Scene.kind: ViewKind`.

- [ ] **Step 1: Failing tests** (`app.rs` tests): `a_disk_view_keeps_its_columns_and_sorts` asserts `ViewKind::Disk.titles()` equals the five strings above and `sort_key(0..5)` equals today's array; `a_process_view_sorts_cpu_and_io_by_aux` asserts `Processes.sort_key(3) == SortKey::Aux(0)` and `(4) == SortKey::Aux(1)`; `col3_4` for a Disk tree node equals what `draw.rs` formatted before (`fmt::count`, `fmt::date`).
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3: Implement.** Add the enum and methods, delete the free `fn sort_key(col)` (callers use `self.kind().sort_key(col)`; `grep -n "sort_key(" crates/plumb/src` shows them all), put `kind` on `DriveInfo`, set it in `drives::probe`, thread `kind` into `Scene` (`with_scene`) from the active drive (`Disk` when none), draw the header from `kind.titles()` (keep the `atleast` rule for Disk only), and replace the two cell draws in `rows()` with `kind.col3_4(...)`; the row's icon call passes `dir && kind == ViewKind::Disk`, so a process shows its executable's icon (shell icons are by extension) while the chevron still keys on `dir && has_children`. Update every `Scene { .. }` initialiser in `tests/render.rs` with `kind: ViewKind::Disk`.
- [ ] **Step 4: Byte-identity check.** Build the 0.8.0 commit's render PNGs (`git stash; git checkout f3b7d30 -- . ` is **not** allowed; instead `git worktree add ../plumb-base f3b7d30` and run `PLUMB_SHOT_DIR=<dir1> cargo test --release -p plumb --test render` there, then the same in this tree into `<dir2>`; `fc /b` or `cmp` every PNG in both). The status-bar-independent frames must be identical; any difference means the refactor changed a disk view. Remove the worktree afterwards.
- [ ] **Step 5:** `cargo test --release --workspace` → pass; `plumb.exe --selftest-delete` → ok. Commit.

### Task 4: Process snapshot (Windows)

**Files:** Create `crates/plumb-scan/src/procs.rs`; modify `crates/plumb-scan/src/lib.rs`, `crates/plumb-scan/Cargo.toml` (`Win32_System_LibraryLoader`).

**Interfaces:**
- Produces: `pub struct ProcEntry { pub pid: u32, pub parent: u32, pub created: u64 /* FILETIME */, pub name: String, pub working_set: u64, pub private_bytes: u64, pub cpu_100ns: u64 /* user+kernel */, pub io_bytes: u64 /* read+write+other transfer */, pub path: Option<String> }`; `pub fn snapshot() -> Result<Vec<ProcEntry>, ProcError>`; `pub fn physical_memory() -> (u64 /*total*/, u64 /*available*/)`.

- [ ] **Step 1: Failing test:** `our_own_process_is_listed_with_a_plausible_working_set`: `snapshot()` contains `std::process::id()`; `working_set` is within 20 % of `GetProcessMemoryInfo(GetCurrentProcess()).WorkingSetSize` read right after; `created` equals `GetProcessTimes(GetCurrentProcess()).creation`; `name` equals the exe's file name (case-insensitive); a spawned `cmd /c ping -n 3 127.0.0.1 >nul` child appears with `parent == our pid`; `physical_memory().0 > 1 GiB`.
- [ ] **Step 2:** `cargo test -p plumb-scan procs` → FAIL.
- [ ] **Step 3: Implement.** Resolve `NtQuerySystemInformation` with `GetProcAddress(GetModuleHandleW("ntdll.dll"), "NtQuerySystemInformation")`; class `SystemProcessInformation = 5`; buffer doubles from 1 MiB on `STATUS_INFO_LENGTH_MISMATCH`. Parse by offsets (x64 `SYSTEM_PROCESS_INFORMATION`): `NextEntryOffset u32 @0`, `CreateTime i64 @32`, `UserTime i64 @40`, `KernelTime i64 @48`, `ImageName { Length u16 @56, ptr @64 }` (UTF-16 bytes), `UniqueProcessId @80`, `InheritedFromUniqueProcessId @88`, `WorkingSetSize @144`, `PrivatePageCount @200`, `ReadTransferCount @232`, `WriteTransferCount @240`, `OtherTransferCount @248`; stop at `NextEntryOffset == 0`. PID 0's `ImageName` is empty: name it `System Idle Process`. Path: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW`, `None` when denied. `physical_memory` via `GlobalMemoryStatusEx`. If the test fails on a field, fix the offset from the struct's documented layout (the test is the check; do not guess a second time without printing the raw bytes).
- [ ] **Step 4:** run → pass. Commit.

### Task 5: Pure reconciler (`plumb-core`)

**Files:** Create `crates/plumb-core/src/procs.rs`; modify `crates/plumb-core/src/lib.rs` (`pub mod procs;`).

**Interfaces:**
- Consumes: a plain input struct (so core stays free of Windows): `pub struct Sample { pub pid: u32, pub parent: u32, pub created: u64, pub name: String, pub memory: u64, pub cpu_100ns: u64, pub io_bytes: u64 }`.
- Produces: `pub struct ProcTree { pub tree: Tree, .. }`, `ProcTree::new()`, `ProcTree::apply(&mut self, samples: &[Sample], elapsed_100ns: u64)`, `ProcTree::set_metric(&mut self, m: Metric)`, `pub enum Metric { Memory, Cpu, Io }`, `ProcTree::pid(&self, id: NodeId) -> u32`. After `apply`: `tree.size[id]` is the selected metric (memory bytes; CPU in tenths of a percent of one core × 1; I/O in bytes per second), `tree.aux[id] = [cpu_tenths_percent, io_bytes_per_sec, pid]`. The tree has one synthetic root node `Processes` (flag `DIR`, created by `ProcTree::new` through `TreeBuilder`); every process is a descendant of it, and a process with no live parent hangs directly under it. (`Tree::add_child` cannot take `NodeId::NONE` as parent and `Tree::remove` refuses a root, so a forest of real roots is not possible.)

- [ ] **Step 1: Failing tests** (in `procs.rs`): `a_new_process_is_added_under_its_parent`; `an_existing_process_keeps_its_node_id_and_updates_its_size`; `an_exited_process_is_removed_and_its_children_become_roots` (the parent exits; the children are re-parented to the synthetic root before `remove`, which would otherwise kill their whole subtree); `a_reused_pid_with_a_new_creation_time_is_a_new_node`; `a_parent_pid_that_points_at_a_descendant_makes_a_root_not_a_cycle`; `pid_0_and_4_hang_under_the_root_and_pid_0_adds_no_memory` (PID 0's working set is forced to 0, so the root's total is the sum of the real processes); `cpu_is_the_delta_over_the_elapsed_time` (cpu_100ns from 0 to 5_000_000 over 10_000_000 elapsed → 500 tenths = 50.0 %; first apply shows 0); `switching_the_metric_rewrites_size_and_the_subtree_sums`; `a_removed_process_leaves_a_dead_node_and_the_live_count_is_right`.
- Known limit, to record in the spec: removed nodes stay in the vectors (`Tree::remove` marks them `DEAD`), so the tree grows with process churn (about 100 bytes per exited process); when `tree.dead() > 20_000` the window rebuilds the `ProcTree` from the next snapshot, which resets selection and expansion once.
- [ ] **Step 2:** `cargo test -p plumb-core procs` → FAIL.
- [ ] **Step 3: Implement** with `HashMap<(u32, u64), NodeId>` (identity → node), per-node previous `(cpu_100ns, io_bytes)`, and the order: (1) mark each existing identity seen or not; (2) for samples not yet known, `add_child(root, name_utf16, 0, NodeFlags::DIR)` (`DIR` is what lets `RowModel` expand a node that has children: `rows.rs` line ~394) — add all new nodes under the synthetic root first; (3) set each node's parent: wanted parent = the node for `(parent_pid, *)` that was alive at this sample and whose `created <= child.created` (a parent created after its child is a reused PID: treat as no parent); if different from the current parent, `tree.reparent(id, wanted)`; it returns false when `wanted` is inside `id` (a cycle), and then `reparent(id, root)`; a process with no usable parent goes to `root`; (4) `remove` identities not seen (children were already re-parented in step 3 because their wanted parent no longer exists); (5) update metrics and `set_aux`; sizes with `set_size`. `elapsed_100ns == 0` or the first apply gives zero rates.
- [ ] **Step 4:** run → pass; also `cargo test --release -p plumb-core`. Commit.

### Task 6: The Processes slot in the window

**Files:** Modify `crates/plumb/src/drives.rs` (append the Processes entry), `crates/plumb/src/app.rs` (hide scan / basket / filter-of-disk paths for `Processes`; status text; tip text), `crates/plumb/src/window.rs` (`spawn_scan` / `unpark` / `evict` / `park_active` handling; poller; `WM_PROCS`), `crates/plumb/src/draw.rs` (the chip draws memory used / total like a drive's used / total); `crates/plumb/src/window/selftest.rs` (new `selftest_processes`), `crates/plumb/src/main.rs` (`--selftest-processes`).

**Interfaces:**
- Consumes: `plumb_scan::procs::{snapshot, physical_memory, ProcEntry}`, `plumb_core::procs::{ProcTree, Sample, Metric}`.
- Produces: `DriveInfo { root: "Processes".into(), total: ram_total, free: ram_available, kind: ViewKind::Processes }` as the last entry of `drives::list()`; `Win::procs: Option<ProcTree>` and `Drive.procs: Option<ProcPoll>` (poller handle: `Arc<AtomicBool>` stop flag + `JoinHandle`), a window message `WM_PROCS` carrying `(drive index, Box<Vec<ProcEntry>>, elapsed)`.

This task needs line-level reading before editing; do these reads first and ledger what each seam is: `spawn_scan` (what it sets up for a scan), `WM_SCAN_DONE` handler, `sync_timers`, `preview_tick`, `apply_live`, `Drive::discard`, `App::with_scene`, `App::message`, `App::status_parts`, `MarkState` / `can_mark`. The behaviours below are the acceptance criteria.

- [ ] **Step 1: Failing selftest + tests.**
  - `app.rs` tests (headless, no window): `a_process_view_cannot_mark` (`can_mark` → `Refused("processes cannot be deleted")` when the active drive's kind is `Processes`); `the_process_status_line_names_the_count_and_memory` (`"N processes, X in use of Y"`).
  - `selftest_processes` (real window, no dialogs): open the Processes chip; `pump_until` the tree has a node whose `aux[.][2] == our pid` with `size > 0`; spawn `cmd /c ping -n 4 127.0.0.1 >nul` and see it appear under our pid within 5 s and vanish within 10 s; switch to the first drive and back: the Processes tree is still there, its polling stopped while away (`Win` exposes `procs_polling()`; assert false while parked, true after return) and the first snapshot after returning shows no CPU % above 100 × cores for any node.
- [ ] **Step 2:** run → FAIL (no Processes entry).
- [ ] **Step 3: Implement.** `drives::list()` appends the Processes entry (refreshing `free` on every call). In `spawn_scan`: when `drives[drive].kind == Processes`, instead of a scan create the tree via `ProcTree::new()`, set `App` summary / scan state to "ready after the first snapshot" (reuse `Scan::Running` until `WM_PROCS` #1, then the state a finished scan gets), start the poller thread (1 s `snapshot()`, posts `WM_PROCS`, exits when its flag is set), and skip journal / watch / dir_pending / preview. In the `WM_PROCS` handler: `in_slot(drive, ..)` → convert `ProcEntry` to `Sample`, `procs.apply(samples, elapsed)`, then `App` refreshes the rows from `tree.take_touched()` exactly as `apply_live` does after a file-system change (find and call the same function); skip when the slot is gone. `park_active` / `Drive::discard` stop the poller (set the flag; do not join on the UI thread); `unpark` restarts it. The `evict` count treats a Processes slot like a tree-holding drive. `can_mark` refuses for `Processes`; the basket chip never shows; the rescan button there means "refresh now" (one extra poll). The status line: `format!("{} processes, {} in use of {}", n, size(total_ws), size(total_ram))`. The tooltip for a row (`tip_target` Tile / row path): name, PID, path, memory, CPU, I/O, started.
- [ ] **Step 4:** `cargo test --release --workspace` and `plumb.exe --selftest-processes`, `--selftest-delete`, `--selftest-live`, `--selftest-watch` → all ok. Capture a window screenshot of the Processes view (`cap.ps1`-style, no focus steal) and look at it: columns, chip, tree.
- [ ] **Step 5: Measure the poll's cost:** `(Get-Process plumb).CPU` over 60 s with the view on screen and idle; record the number in the spec (section 2). If it exceeds 1 % of one core, lower the work (skip `QueryFullProcessImageNameW` after the first time for a known identity) and re-measure. Commit.

### Task 6b: Map colouring (done, unplanned)

The first Processes render was one grey block: every node was DIR-flagged, so every tile was `Class::Other`. Fix: `Tree::set_dir`; `Procs` keeps the DIR flag equal to "has children" (leaf processes are file tiles); `treemap::layout_by` takes a class function; `app::location_class` colours a process by its executable (Windows / Program Files / User profile / Elsewhere) and `ViewKind::class_label` names the legend entries.

### Task 7: Map metric (Memory / CPU / I/O)

**Files:** Modify `crates/plumb/src/layout.rs` (a `metric_button()` rect left of the map button, shown only when `kind == Processes`), `crates/plumb/src/draw.rs`, `crates/plumb/src/app.rs` (hit, click cycles the metric, tooltip), `crates/plumb/src/window.rs` (`WM_PROCS` handler / click calls `ProcTree::set_metric` then rebuilds the map and rows).

**Interfaces:** Consumes `ProcTree::set_metric(Metric)` (Task 5). The Size / Memory column header, the % column and the map all follow `tree.size`, so the first column's title becomes `Memory` / `CPU` / `I/O` with the metric: `ViewKind::titles` takes the metric (`titles(self, metric: Metric)`; Disk ignores it).

- [ ] **Step 1: Failing tests:** `layout`: `the_metric_button_sits_left_of_the_map_button_only_for_processes`; `app`: `clicking_the_metric_button_cycles_memory_cpu_io_and_back` (headless with a scripted `ProcTree` installed); `render`: the Processes frame with each metric paints (no panic) and the map's tile areas differ between Memory and CPU for a tree with unequal values.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement as described. **Step 4:** `cargo test --release --workspace`; screenshot each metric. Commit.

### Task 8: Who is using this file (Restart Manager)

**Files:** Create `crates/plumb-scan/src/holders.rs`; modify `crates/plumb-scan/src/lib.rs`, `crates/plumb-scan/Cargo.toml` (`Win32_System_RestartManager`); modify `crates/plumb/src/window.rs` (`finish_delete` reasons), `crates/plumb/src/trash.rs` (`Outcome` / reason enrichment), `crates/plumb/src/dialog.rs` (nothing: the reason string carries it), `crates/plumb/src/app.rs` (context item).

**Interfaces:**
- Produces: `pub struct Holder { pub pid: u32, pub name: String }`, `pub fn holders(path: &str) -> Vec<Holder>` (empty on any error, never panics, never blocks longer than ~2 s because `RmGetList` can be slow: run it on the caller's worker thread; the delete worker already is one).
- In `window.rs::start_delete`'s worker (it runs `trash::delete`, then posts `WM_DELETED` with `(drive, permanent, Vec<Outcome>)`): after `trash::delete`, for every `Failed(hr)` with `trash::reason(hr) == "in use by another program"` call `holders(path)` on that worker and add the result to the payload as `HashMap<String, Vec<Holder>>`; `finish_delete` (UI thread, builds the `(path, reason)` pairs) then uses `trash::with_holders(&reason, holders)` so the reason reads `in use by chrome.exe (1234)` / `in use by chrome.exe (1234) and 2 more`. `holders` never runs on the UI thread.

- [ ] **Step 1: Failing tests:** `holders.rs`: `a_file_this_process_holds_names_this_process` (create a temp file, keep it open with `share_mode(0)`, `holders(path)` contains `pid == std::process::id()`); `a_file_nobody_holds_has_no_holders`; `a_missing_path_has_no_holders`. `trash.rs`: `a_sharing_violation_reason_names_the_holder_when_there_is_one` for the pure formatter `with_holders(reason: &str, holders: &[Holder]) -> String`. Extend `selftest_delete`'s held-file step: the status tooltip must contain `in use by plumb.exe` (the process holding the file in the test) instead of the generic text — update the existing assertion string accordingly.
- [ ] **Step 2:** run → FAIL. **Step 3: Implement** `RmStartSession` (key buffer `[u16; CCH_RM_SESSION_KEY + 1]`), `RmRegisterResources(session, Some(&[path]), None, None)`, `RmGetList` with the usual two-call size dance (`ERROR_MORE_DATA` → allocate `needed` entries, retry up to 3 times because the list can grow between calls), map `RM_PROCESS_INFO.Process.dwProcessId` and `strAppName`, `RmEndSession` in a drop guard. **Step 4:** `cargo test --release --workspace`; `plumb.exe --selftest-delete`. Commit.
- [ ] **Step 5:** Context item "Who is using this" (tree row and map tile menus, disk views only): runs `holders` on a worker and shows a TaskDialog list (`PID  name`), or "Nothing is using it." Test: headless `app` test that the item is offered for a file in a disk view and absent in Processes. Commit.

### Task 9: Files a process holds (only if Task 1's gate passed)

**Files:** Modify `crates/plumb/src/app.rs` (selected process → pane state), `crates/plumb/src/layout.rs` (a bottom pane below the rows in Processes, hidden otherwise), `crates/plumb/src/draw.rs`, `crates/plumb/src/window.rs` (worker call and message `WM_HANDLES`).

**Interfaces:** Consumes `plumb_scan::handles::open_files(pid, 500ms)`. A "Files held" pane (a list, 6 rows, scrolls) under the rows when a process row is selected, filled on a worker thread when the selection settles for 300 ms, cancelled (result ignored by selection generation) when the selection moves; text `N files` / `N files, M not readable` / `run as administrator to see this process's files` for `HandleError::Denied`.

- [ ] **Step 1: Failing tests:** headless `app`: `selecting_a_process_requests_its_files_once_it_settles` (no request on rapid selection changes; one request after the settle with the right PID), `a_stale_result_for_a_previous_selection_is_ignored`; `layout`: the pane takes height from the rows only for Processes and `regions_tile_the_window` still holds.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** extend `selftest_processes`: select our own process row, wait for the pane, assert it lists a file we hold open (open a temp file first). Screenshot. Commit.

### Task 10: Docs, changelog, release prep

**Files:** Modify `CHANGELOG.md` (`[Unreleased]` → Added: the Processes view, who-is-using-this, files held; Changed: none), `docs/superpowers/specs/2026-10-16-plumb-views-design.md` (Status: implemented; the measured numbers; Task 1's result), `docs/superpowers/specs/2026-10-15-plumb-delete-design.md` (the Restart Manager item is no longer parked), `README` if it lists features (`grep -n "treemap\|Modified" README.md`).

- [ ] **Step 1:** Edit the files. **Step 2:** `cargo test --release --workspace`, `cargo clippy --release --workspace --all-targets` (no warnings), all four selftests. **Step 3:** Commit. **Step 4:** Release steps are the user's call: version bump to 0.9.0 in the three crates, `[Unreleased]` → `[0.9.0] - <date>`, push, tag `v0.9.0-rc1`, then `v0.9.0` (needs an explicit go-ahead; last time the sandbox asked for it twice).

### Task 11 (optional, on request): Rename `Drive` → `View`

One mechanical commit: `Drive`→`View`, `DriveInfo`→`ViewInfo`, `KEPT_DRIVES`→`KEPT_VIEWS`, `app.drives`→`app.views` across `crates/plumb`; no behaviour change; tests and selftests green; byte-identical frames as in Task 3 Step 4.

---

## Self-review

- **Spec coverage:** slots → views (Tasks 3, 6, 11), Source (replaced by `ViewKind`, ruled), process view (4–7), actions none (6), what holds it both directions (8, 9 behind the spike 1), tests (each task, plus `--selftest-processes`), out of scope unchanged. The spec's idle-CPU measurement is Task 6 Step 5.
- **Placeholder scan:** Tasks 6 and 9 name seams to read first rather than giving line-level edits, because the slot / scan / live code is ~4,000 lines whose exact lines the executor must read; each has acceptance tests and exact behaviours instead. Everything else carries signatures and test names.
- **Type consistency:** `ViewKind` (Task 3) is used by 6 and 7; `Metric` (Task 5) by 7; `aux` layout `[cpu_tenths_percent, io_bytes_per_sec, pid]` is consistent between Tasks 2, 3 (`col3_4`) and 5; `ProcEntry` (plumb-scan, Task 4) → `Sample` (plumb-core, Task 5) conversion is in Task 6.
- **Review Focus coverage:** PID reuse and cycles → Task 5 tests; parking and CPU gap → Task 6 selftest; non-elevated → Tasks 4 (`path: None`), 9 (`Denied` text); hanging handle → Task 1 spike (pipe test) and the `FILE_TYPE_DISK` filter; no holder found → Task 8 (`with_holders` with an empty list keeps the reason).
