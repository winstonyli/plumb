# plumb: disk pane design (v1)

Status: draft for review, 2026-10-11.

plumb is a maximally performant native Windows replacement for Task Manager, WinDirStat and similar tools. It will eventually have process, disk and search views over one shared data layer. **This spec covers only the first slice: the disk pane.** Everything else is parked (see "Out of scope").

## Decisions so far

| Question | Decision |
|---|---|
| Build vs reuse | Build one unified native app (existing tools remain the benchmark) |
| Interface | Native GUI, custom-drawn |
| Rendering | Rust + `windows-rs`; Direct3D 11 device; Direct2D + DirectWrite for UI; custom Direct3D 11 renderers only where profiling demands (e.g. a future treemap) |
| v1 content | Disk first |
| Elevation | Single process. MFT scanner if launched elevated, parallel directory walker otherwise |
| Freshness | Static tree in M1; live updates via the NTFS change journal in M2 |
| Persistence | None in v1 (fresh scan each launch) |
| Views | Size-sorted folder tree only (no treemap) |
| Actions | Read-only: reveal in Explorer, copy path, properties |
| Process model | One process; scanner behind a trait so it can move into a child process later |

## Architecture

One workspace, two crates.

- `plumb-core`: pure Rust, no UI or Win32 types. The tree, `NodeId`, `NodeFlags`, size accounting, the MFT record parser (takes bytes), the row model and sorting. Builds fast, tests without a GPU or elevation, holds all criterion benchmarks.
- `plumb`: the Windows binary. Volume and file I/O, the `Scanner` implementations, the Win32 window, and Direct3D 11 / Direct2D / DirectWrite drawing. Dependencies: `windows` (0.62.2), `bitflags`; dev: `criterion`.

Three layers inside: **scanner** fills the **tree**; the **UI** only reads the finished tree.

## Tree (`plumb-core`)

Struct-of-arrays, no per-node allocations, `#[repr(transparent)] struct NodeId(u32)` with `NodeId::NONE = u32::MAX` and `Index<NodeId>` impls on the arrays (one place for the `usize` conversion). Name offsets are plain `u32` into the string pool.

| Field | Type | Meaning |
|---|---|---|
| `parent`, `first_child`, `next_sibling` | `NodeId` | links |
| `name_off`, `name_len` | `u32`, `u16` | UTF-16 name in a shared pool |
| `size` | `u64` | own allocated bytes |
| `subtree_size` | `u64` | own + descendants |
| `subtree_files` | `u32` | files in subtree (Items column) |
| `flags` | `NodeFlags` | `bitflags` struct over `u8`: `DIR`, `REPARSE`, `HARDLINK`, `DENIED` |

39 bytes per node plus the name (about 40 bytes of UTF-16 on average); 5M files is roughly 400 MB.

Size accounting:
- Counted: **allocated size on disk**, not logical length. For compressed or sparse NTFS streams the MFT scanner uses the stream's compressed size field, so it matches the walker's `AllocationSize`.
- Hard links: the file appears under each parent, but its size is counted once. "Once" is decided in the merge pass in record-number order (not by whichever thread parses first), so trees are deterministic. Later links have size 0 and the `HARDLINK` flag.
- Only name-surrogate reparse points (symlinks and junctions/mount points) get `REPARSE` and are never followed. Other reparse tags (cloud placeholders such as OneDrive, dedup, WIM) are treated as ordinary files and directories. The tag comes from the directory entry's `EaSize` in the walker and from the `$REPARSE_POINT` attribute in the MFT scanner.
- Access-denied directories are listed with `DENIED`; the scan continues.
- `subtree_size` and `subtree_files` are filled in one bottom-up pass after the scan. The finished tree is read-only and needs no locks.

## Scanners (`plumb`)

Both implement `Scanner::scan(root, &mut TreeBuilder, &Progress)`, where `root` is a drive root or any directory, and produce equivalent trees; they live in the `plumb-scan` crate. Default worker count is `min(cores - 4, 8)`, at BelowNormal priority.

### `MftScanner` (elevated, NTFS only)
- Open `\\.\X:`; read `FSCTL_GET_NTFS_VOLUME_DATA`, then read file record 0 at `MftStartLcn` and decode its `$DATA` data runs (no `FSCTL_GET_RETRIEVAL_POINTERS`). If the runs do not cover `MftValidDataLength` (an `$ATTRIBUTE_LIST`), fail and fall back to the walker; only 1024-byte records on volumes whose cluster size is a multiple of 1024 are supported. Stream `$MFT` in 4 MB cluster-aligned chunks (volume reads must be sector-aligned). One sequential pass, no per-file I/O.
- A reader thread feeds a pool of parsers. Per 1 KiB record: apply update-sequence fixups, then read:
  - `$FILE_NAME`: parent reference, name, namespace. DOS-only (8.3) names are skipped so each file appears once.
  - `$DATA`: allocated size of the unnamed stream. Alternate data streams are ignored in v1.
  - Record header flags (offset 0x16, bit 0x02): the directory flag. `$REPARSE_POINT`: the reparse tag.
  - Extension records are folded into their base record through the base-reference field.
- Data stored resident inside the MFT record counts as 0 allocated bytes (it uses no clusters). The walker applies the same rule: an allocation that is not a whole number of clusters is resident data and counts as 0.
- Parent links use the record number of the parent reference and are resolved in a second pass, because a parent can come after its child.

### `WalkScanner` (any volume, no admin)
- `GetFileInformationByHandleEx` with `FileIdExtdDirectoryInfo` / `FileIdExtdDirectoryRestartInfo`, 256 KB buffers. Each entry already contains name, attributes, `AllocationSize` and the file id; no file is opened. (A documented API, so no ntdll linking.)
- A coordinator thread owns the `TreeBuilder` and queues directories on a shared job queue; workers only list directories. Directories count as 0 allocated bytes (the MFT scanner does not count index allocation either).
- Hard links: the listing carries each file's id, so the coordinator counts a file once, on the first link it reaches (the others are flagged `HARDLINK` at 0, as in the MFT scanner). Which link carries the bytes depends on thread timing; totals are stable.

### Choosing and failing
- At startup, check whether the token is elevated (the manifest is `asInvoker`; elevation is the user's choice via "Run as administrator"). NTFS volumes use `MftScanner` if elevated, otherwise `WalkScanner`; non-NTFS volumes always use the walker. The status bar shows which scanner ran per volume, and when the walker ran on NTFS it says that running as administrator is faster.
- If `MftScanner` cannot open the volume or hits a fatal error, it falls back to `WalkScanner` for that volume **with an announcement in the status bar**.
- Corrupt records are skipped and counted; the count is shown.
- Volumes scan and display independently. A panic in a scanner thread ends that volume's scan with an error, not the app.

## UI (`plumb`)

### Window and rendering
- One top-level Win32 window, per-monitor-v2 DPI aware, light or dark to match the system. Everything inside is custom-drawn; there are no child controls.
- One Direct3D 11 device with a flip-model swapchain; a Direct2D device context draws to the back buffer; DirectWrite does the text.
- Adapter: the GPU that owns the window's monitor (found via DXGI), logged at startup with its name and backend, so frames are never copied between adapters.
- On `DXGI_ERROR_DEVICE_REMOVED` (e.g. eGPU unplugged), recreate the device and swapchain and repaint.
- Redraw only on input, resize, or scan progress (throttled to 10 Hz). No timers or animation while idle.

### Layout
Drive picker (one chip per drive letter from `GetLogicalDrives`, with used and free space from `GetDiskFreeSpaceEx`; click to scan; folder-only mount points get a chip too; a drive is identified by its root path) at the top, tree view in the middle, status bar at the bottom. The status bar shows scanner used, scan time, file count, skipped-record count and the chosen adapter. During a scan the tree view shows progress; the tree appears when the scan finishes.

### Tree view
- Columns: Name, Size (allocated), % of parent (bar behind the number), Items.
- Virtualized: only visible rows (about 50) are laid out and drawn. Expansion state is a bitset keyed by `NodeId`; the row list is a flat `Vec<NodeId>` with depths.
- Sort by Name, Size or Items; default Size descending. Children are sorted when a folder is first expanded, and the order is cached for that folder.
- Mouse: wheel, drag a drawn scrollbar, click to select, double-click to expand. Keyboard: arrows, Left/Right to collapse and expand, Home/End, PageUp/PageDown.
- Context menu: Reveal in Explorer, Copy path, Properties. Paths are built by walking parent links; no per-node path storage.

## Testing and benchmarks

- Unit tests in `plumb-core`: hand-built MFT records (fixups, extension records, DOS names, hard links), tree sums, row model (expand, collapse, sort, keyboard selection, visible window for a scroll offset).
- `scan-compare` (needs an elevated run): both scanners on a fixture directory, including one hard-linked pair compared by its total; the MFT tree is compared on the subtree under that directory, and every path and size must agree.
- Render smoke test: a fixed small tree drawn through a WARP (software) device into an offscreen bitmap; asserts no failing HRESULT. It opens no window and does not touch the physical GPU.
- Criterion for the pure-CPU paths: MFT record parsing over synthetic records, the bottom-up sum over a synthetic 5M-node tree, sorting 1M children, and row-model expand and visible-window queries. Benchmarks run only when CPU load is under about 20%; load is reported with the numbers.
- Real-drive scans are not criterion benchmarks (disk cache dominates). The status bar and a run log record wall time per phase (read, parse, link, sum); comparisons against WizTree are on the same drive with a cold-cache note.
- A startup script measures cold launch to first frame, idle memory and idle CPU.

## Performance targets

These are my estimates, to be measured, not claims:
- Cold start to first frame under 150 ms.
- Idle: about 0% CPU and under 30 MB before any scan.
- MFT scan of a ~1M-file volume in a few seconds, comparable to WizTree.
- UI stays responsive during a scan.

## Milestones

- **M1:** workspace, `plumb-core` tree and parser with tests and benchmarks, both scanners, window and tree view, drive picker, status bar.
- **M2:** live updates through the NTFS change journal (elevated NTFS only). Record `NextUsn` (`FSCTL_QUERY_USN_JOURNAL`) before the scan starts so changes during the scan are replayed afterwards. The journal has no sizes, so a size change re-reads that file's MFT record. M2 breaks the "finished tree is read-only" rule: a journal thread sends deltas over a channel and the UI thread applies them between frames, so there is still no locking. Volumes with the journal disabled, or whose journal wrapped past our start point, keep their static tree and say so. Details are refined in a follow-up spec.

## Out of scope for v1 (parked)

| Item | Why parked |
|---|---|
| Processes pane | Disk first; needs its own design |
| ~~Treemap~~ | Shipped (see `2026-10-14-plumb-treemap-design.md`): a Direct2D split pane; the Direct3D 11 path stays unused |
| Saved tree plus journal replay on startup | Fresh scan each launch is simplest |
| ~~Live updates for non-NTFS / unelevated volumes (`ReadDirectoryChangesW`)~~ | Shipped (0.4); also the fallback when the journal is off or unopenable (0.6) |
| ~~Partial tree display while a scan runs~~ | Shipped (preview, 0.3) |
| ~~Name filter~~ | Shipped (0.4, regex toggle and match tint); a separate search *view* is still parked |
| Alternate data streams | Rarely large; counted as 0. Measured 2026-10-14 (`docs/benchmarks.md`): not worth the cost |
| Delete and cleanup suggestions | Read-only tool by decision (deletion reversed 2026-10-15: see 2026-10-15-plumb-delete-design.md; cleanup suggestions still out) |
| Accessibility (UI Automation) | Custom-drawn UI gap, known |
| Elevated scanner child process | Single process chosen; scanner stays behind the trait |

## Parked leftovers (as of 0.6.0)

| Item | Why parked |
|---|---|
| Mixed-DPI window restore | Needs a second monitor at another scale; same-DPI restore is verified |
| Wrapped-journal rescan | No tool forces the journal to wrap; the rescan path is only unit-tested |
| Light-mode tooltip colours, live theme switch | Dark mode verified; light not checked by eye |
| Live updates when the dir watch starts after the scan | Only after an MFT-to-walker fallback; changes made during that scan are missed |
| Which hard link carries the bytes (walker) | Depends on thread timing; totals are stable, per-folder attribution is not |

## Open questions to verify during M1

- ~~Whether the thread count and BelowNormal priority starve the UI thread during a scan.~~ Checked by hand 2026-10-11: no stutter noticed while dragging the window during scans. Not measured as frame times; revisit if it shows up.
- The memory cost per node for real drives versus the ~39 bytes plus name estimate.
- That cloud-placeholder files report the same allocated size from both scanners. (Compressed and sparse files were verified equal by `scan-compare` on 2026-10-11.)

## Implementation plans

Work is split into three plans, each producing tested software on its own:

1. `plumb-core`: tree, row model, MFT record parser, synthetic inputs, benchmarks (`docs/superpowers/plans/2026-10-11-plumb-core.md`).
2a. `plumb-scan`: `Scanner`, `MftMerger`, `WalkScanner` (`docs/superpowers/plans/2026-10-11-plumb-scan-a.md`).
2b. `MftScanner` volume reader, scanner selection and fallback, `scan-compare` (`docs/superpowers/plans/2026-10-11-plumb-scan-b.md`; done, verified elevated).
3a. UI rendering and layout: `plumb` crate with layout, formatting, device stack and Direct2D painter, WARP smoke test (`docs/superpowers/plans/2026-10-11-plumb-ui-a.md`; done).
3b. UI window: Win32 window and loop, swapchain, input, drive picker, scan wiring, context menu, Run as administrator button (`docs/superpowers/plans/2026-10-11-plumb-ui-b.md`; done, manual checklist passed 2026-10-11).
