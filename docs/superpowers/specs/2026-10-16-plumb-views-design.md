# Views: more than drives (design)

Status: design, 2026-10-16. Target 0.9.0 (one release: the refactor, the process view, and "what holds it").

Goal: plumb as a one-stop place to browse and diagnose the machine. The first new view is Processes; disks stay as they are. Later views (startup items, services, registry, network) are out of scope here, but the design must make each one a new `Source`, not a new special case.

## What exists

- `Tree` (parent / first_child / next_sibling / size / subtree_size / flags / mtime, names pooled) is generic. `RowModel`, `treemap`, `filter` and the live `touched` set work on it.
- Everything around it is disk-shaped. A "drive" is a `Drive` slot (`window.rs`): a `Parked` app state plus a scan (`Progress`, `rx`, `Preview`), a USN-journal `Watch` and a `dir_pending` directory watcher. `swap_drive` / `in_slot` / `open` / `KEPT_DRIVES = 3` move slots on and off screen; `WM_SCAN_DONE` and `WM_JOURNAL` carry the slot index. See `2026-10-13-plumb-drive-slots-design.md`.
- The columns (Name, Size, Items, Modified), their sort keys, the basket / delete and the Modified-age map shading all assume files.

## Decisions (user, 2026-10-16)

1. A `Source` abstraction, not a parallel Processes mode: one table, one map, one filter.
2. Processes is read-only in v1: no end / suspend.
3. "What holds it" is in v1 (see section 4); it is the last piece, behind a spike.
4. All in 0.9.0.

## 1. Slots become views

`Drive` becomes `View`; `KEPT_DRIVES` becomes `KEPT_VIEWS`. A view is `{ kind: Drive(root) | Processes, state: Parked, source runtime }`. The chip row lists the drives, then a "Processes" chip. The slot mechanics (swap, in_slot, LRU eviction, marks on chips) stay as they are; only what a view *runs* varies, behind:

The row table is Name plus four columns (`layout.col_w: [f32; 4]`): Size, % of parent, Items, Modified. A view kind keeps that geometry (same widths, resize and state file) and supplies only data: `ViewKind::{Disk, Processes}` gives the column titles, the `SortKey` of each column, and the text of columns 3 and 4 (Disk: Items, Modified; Processes: CPU, I/O). No trait: there is one implementor per kind and no state to hold.

Two small changes in `plumb-core` keep `RowModel` and `treemap` unchanged in shape:

- **The map's weight is `Tree::size`.** For Processes, `size` holds the selected metric (memory, CPU or I/O), and switching metric rewrites it with `set_size` (the roll-up recomputes `subtree_size`; a few hundred nodes, so it is cheap). No weight function, no allocation in `layout`.
- **`Tree::aux: Vec<[u64; 3]>`** (empty for a disk tree, so it costs nothing there) holds CPU (tenths of a percent), I/O (bytes per second) and PID. `SortKey::Aux(i)` sorts by `aux[n][i]`; the closed `match` in `RowModel::sorted_children` gains one arm.

Disk-only machinery (scan, preview, journal, directory watch, basket, delete, age shading, shell icons by extension) is not part of the trait: it is the `DiskSource` runtime, and `Win` runs it only for a `Drive` view. A view's lifecycle is `Idle -> Loading -> Ready` for both kinds; Processes is `Ready` after its first snapshot (tens of ms) and then updates in place.

Refactor scope is real: `App` and `Win` carry about 4,000 lines of slot / scan logic, and `layout.column`, `draw`, `rows` (sort) and `treemap` (weights) read disk columns directly. The refactor lands first as its own commits with `--selftest-delete`, `--selftest-live`, `--selftest-watch` and the render tests green at each commit; behaviour of a drive view must not change (a screenshot diff of the render tests is the check).

## 2. Process view

- **Source of truth:** a snapshot every 1 s on a worker thread: `NtQuerySystemInformation(SystemProcessInformation)` for PID, parent PID, name, working set, private bytes, I/O counters and kernel + user times; image path via `QueryFullProcessImageNameW` where permitted.
- **Identity:** a node is keyed by `(PID, creation time)`, so a reused PID is a new node. A process whose parent is gone, was created after it (a reused PID), or would make a cycle hangs directly under one synthetic root node `Processes` (a forest is impossible: `Tree::add_child` takes no `NONE` parent and `Tree::remove` refuses roots).
- **Tree:** parent / child, `size` = working set, so `subtree_size` is a family's memory. System Idle (PID 0) and System (4) are listed, but PID 0's memory is forced to 0 so totals are real. Removed nodes stay `DEAD` in the vectors, so the tree grows with churn (about 100 bytes per exited process); past 20,000 dead nodes the window rebuilds it from the next snapshot (selection and expansion reset once).
- **Update:** each snapshot is reconciled with the tree using the same edit calls `live.rs` uses for the file system (add node, mark dead, change size), so `touched` drives the row refresh and the user's selection, scroll and expansion survive. CPU % and I/O rate are deltas between two snapshots, kept in the source (not in `Tree`), so the first snapshot shows 0 %.
- **Columns:** Name | Memory (default sort, descending) | % of parent | CPU % | I/O (bytes/s). The PID is appended to the name, dimmed (`chrome.exe  (1234)`); the path and start time are in the tooltip.
- **Map:** the same squarified map over the process tree. Metric choices: Memory (default), CPU, I/O; a small selector beside the map button. Tile colour is the process's class by image location (Windows, Program Files, user profile, other); no age shading.
- **Hover:** command line is not read in v1 (needs `PEB` reads); the tooltip is the name, PID, path, memory, CPU, I/O, start time.
- **Not elevated:** every process is listed; path, and later handles, of other users' and protected processes are blank with a "run as administrator for more" note (the existing elevate button).
- **Cost:** a 1 s poll runs only while the view is on screen or kept with its refresh turned off (a parked Processes view stops polling and shows a stale marker, like a parked disk without a watch). Target: under 1 % CPU at idle on this machine (measure it, record the number).

## 3. Actions

None in v1 for processes (`Del` and the basket do nothing there: `can_mark` refuses). The context menu offers Copy path, Open file location, and "Files held" (section 4).

## 4. What holds it

Two directions; they are different APIs and the first does not give the second.

- **File -> processes** (disk views and the delete failure dialog): Restart Manager (`RmStartSession`, `RmRegisterResources` with the file path, `RmGetList`) returns the programs using a file. The failure dialog's reason becomes "in use by <name> (PID)"; a tile / row context item "Who is using this" asks for the selected path. This is the parked item from the delete spec and needs no elevation for the user's own processes.
- **Process -> files** (Processes view): there is no Restart Manager call for it. It needs handle enumeration: `NtQuerySystemInformation(SystemExtendedHandleInformation)` filtered to the selected PID and to File objects, then each handle's name via `NtQueryObject(ObjectNameInformation)` after `DuplicateHandle` into plumb. `NtQueryObject` can block forever on some handles (named pipes with pending reads), so name lookups run on a worker with a per-handle timeout (System Informer does the same) and are skipped, not waited for. Shown in a list below the table for the selected process only (never for every process). Needs `PROCESS_DUP_HANDLE` on the target, so other users' and protected processes need elevation; otherwise the list says so.
- **Spike first** (before any UI): from a unelevated test, enumerate and name the file handles of our own process and of one other of the user's processes; confirm the timeout approach does not hang on a pipe. If this fails or is too slow, the process -> files half moves to 0.9.1 and the rest of 0.9.0 ships; the file -> processes half does not depend on it.

## 5. Tests

- A fake `Source` with scripted snapshots tests the trait, the reconcile (new, changed, vanished, PID reuse, reparenting) and the selection surviving an update, headless like the existing `app` tests.
- Render tests: the Processes view paints (columns, map) with a scripted tree, in the existing `tests/render.rs` style; drive-view frames must be byte-identical to before the refactor (compare against frames saved at the last 0.8.0 commit).
- `plumb --selftest-processes`: the real source lists our own PID with a plausible working set and path, a spawned child appears under us and disappears when it exits, within a few seconds.
- `--selftest-delete` gains a step: a held file's failure dialog names the holder (this process, since the test holds it) — extends the existing held-file step.
- The 1 s poll's idle CPU is measured and recorded in this file.

## 6. Out of scope (and why)

End / suspend / priority (risk; read-only first), command line and environment (PEB reads), per-process network and GPU, services, startup items, scheduled tasks, registry (each is a later `Source`), history graphs (the map and table are snapshots), a separate window.

## Open questions

- The picker bar's chips are fixed-width (`layout.chip`, `chip_x`): check that a Processes chip fits beside many drives (the bar may need to scroll or shorten labels).
- Every place that reads the disk columns (`layout.column` / `col_w`, `draw` headers and cells, `app` sort handling, the state file's three remembered widths) must be found by grep before the refactor; the state file keeps loading old files (a view with other columns remembers its own widths under its kind).

## Review log

Round 1 (against the code): found that the map and sort read `Tree` vectors directly, so the first draft's `weight` / `sort_key` trait methods would have forced changes in `treemap` and `rows`; replaced by "`size` is the selected metric" plus `Tree::aux`. Found that Restart Manager answers file -> processes only, so the first draft's "Files held by a process via `RmGetList`" was wrong; split into section 4's two directions with the handle-enumeration spike. Still to do in the plan: `TreeBuilder` and `Tree::add_child` must keep `aux` the same length as the other vectors (empty `aux` meaning "none"), and `Tree` equality / serialisation, if any, must ignore it.
