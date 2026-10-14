# Changelog

All notable changes to plumb. Format follows [Keep a Changelog](https://keepachangelog.com/); all crates share one version and are bumped together at each tag.

## [Unreleased]

### Added
- `plumb`: while a scan runs, the % of parent bar fades out in amber past its solid part to the most that row could still grow, estimated from the drive's used space less what the scan has found. The fade is hidden once the scan has counted more than the drive reports used (hard links and placeholders can do that). See `docs/superpowers/specs/2026-10-13-plumb-scan-bounds-design.md`.
- `plumb`: drag a column edge in the header to resize the Size, % of parent and Items columns (Name keeps at least 120 px; the pointer becomes a resize cursor over an edge). Clicking a folder's icon expands or collapses it.

### Fixed
- `plumb`: after maximizing, the minimize, maximize and close buttons did nothing. DWM answers for them only when the client area starts at the window's top, and we had moved that down by the frame; the client area now keeps the window's top and the content is drawn and hit-tested below the overhang.
- `plumb`: button labels (Regex, Admin, Rescan, Cancel) were left-aligned with a fixed inset; they are now centred in the button.
- `plumb`: the second press of a quick double-click on a column header, button, drive chip or scrollbar was dropped (Windows delivers it as a double-click); it now acts as a click, so a double-click on a header flips the sort twice.

### Changed
- `plumb`: drive chips show just the free size ("D:  925.3 GB"), which no longer clips; hovering a chip shows "D: 925.3 GB free of 1.0 TB" and lights it up the way the icon buttons do (lighter fill, visible frame).
- `plumb`: the Administrator, Rescan and Cancel buttons are now icons (a shield, a refresh arrow, a cross) and the Regex toggle is `.*`, drawn from Windows' Segoe MDL2 Assets font; each has a tooltip ("Run as administrator", "Rescan", "Cancel scan", "Regular expression"). The buttons are gray until the pointer is over them (then they light up), so the shield no longer looks switched on; the Regex toggle keeps its accent when on. This frees about 140 px in the picker bar, so the filter box keeps its full width longer and the old "Admin" short label is gone.
- `plumb`: the filter box and match count moved into the picker bar, after the drive chips, saving a row, and the Regex toggle sits inside the right end of the box (after the clear button). The box is 304 px wide when there is room and shrinks to 184 px when there is not. The placeholder is now "Filter  (Ctrl+F)".
- `plumb`: the drive picker bar is now the title bar: the client area reaches the window's top edge (`WM_NCCALCSIZE`), empty bar space drags and double-click-maximizes the window, and Windows still draws its own minimize/maximize/close buttons (so snap layouts work) in a transparent corner of the bar, coloured to match it. This needs a DirectComposition swap chain with premultiplied alpha (an HWND swap chain cannot be transparent), a black `WM_ERASEBKGND` so DWM sees glass, and our own resize-edge hit testing.
- `plumb`: visual polish: a smaller sort triangle, an accent bar on the selected row, a smaller and softer triangle on folder icons, visible header grips, and the "at least" sign shown once in the Size and Items headers during a scan instead of on every row.
- `plumb`: a hairline separates rows.
- `plumb`: rows show Explorer's own icons for folders (open or closed) and file types, replacing the drawn folder, the drawn page and the expand chevrons. A folder with children carries a small triangle inlaid on its icon (right when closed, down when open); an empty folder has none. Icons are fetched by extension, cached per painter, 32-pixel at 150% scaling and up; the drawn ones remain as the fallback when the shell has none. `INDENT` is now 20.

## [0.4.0] - 2026-10-13

Cancel a scan, keep each drive's tree and scan when switching drives, and a visual refresh.

### Added
- `plumb`: a running scan can be cancelled with the Cancel button in the status bar or Esc. A cancelled first scan leaves no tree; a cancelled background rescan keeps the old one. `plumb-scan`: `Progress::cancel` and `ScanError::Cancelled`; both scanners stop within one directory or one read chunk.

### Fixed
- `plumb`: the sorted column's header showed the literal text `{25BC}` instead of an arrow.

### Changed
- `plumb`: picking another drive no longer cancels the scan or discards the finished tree. Each drive keeps its tree, selection, live watch and any scan in flight (up to three trees at once); coming back is instant. A strip along the top of a drive's chip marks it: gray for a kept tree, blue for a scan running in the background.
- `plumb`: the Items column is dimmed, and the administrator, Rescan and Cancel buttons have a frame.
- `plumb`: folders and files have icons, the row under the mouse is highlighted, and the status bar has a divider with the graphics adapter at its right.
- `plumb`: while a scan runs, sizes and file counts carry a leading ≥ (they are what has been found so far).
- `plumb-core`: a live change in a huge folder repairs its cached sort order by merge instead of re-sorting (one add among 1M children: 244 ms to 9 ms).

## [0.3.0] - 2026-10-12

Name filter, partial tree while a scan runs, and live updates without admin.

### Added
- `plumb-core`: `filter` module (case-insensitive substring or regex name matching with the `regex` crate, a name snapshot for worker threads, hits and the keep set) and `RowModel::set_filter`, which shows matches and their ancestors and dims rows under a leaf match.
- `plumb`: name filter. Ctrl+F (or a click) focuses a strip above the column headers; typing shows only the files and folders whose name contains the text (case-insensitive), with the folders on the way to them open. The Regex toggle reads the text as a regular expression; an invalid pattern keeps the previous result and tints the box. A matching folder with no matching content is shown collapsed and opens to dimmed rows. Esc clears the filter and restores the previous expansion and scroll. The scan runs on a worker thread 100 ms after the last keystroke, and the filter follows live updates and rescans.

- `plumb`: partial tree while a scan runs. The tree appears within a second and folders fill in as they are found; sorting, expanding, selecting and the filter all work during the scan. When the scan ends the exact tree replaces the preview and your expansion, selection and scroll are kept. Works for both scanners. If the window falls too far behind the scan the status line says "preview paused" and the view stops changing until the exact tree arrives; the scan is not slowed. Design: `docs/superpowers/specs/2026-10-12-plumb-partial-tree-design.md`.
- `plumb-scan`: `Progress` can carry a preview channel (`preview` module); `journal::state::file_state` shares the record-to-file rule with the journal reader.
- `plumb-core`: `Tree::reserve`; `RowModel::refresh` drops its sort cache outright when more than 4096 nodes changed.
- `plumb`: live updates without admin. After a directory-walk scan (any drive when not elevated, or a non-NTFS drive) the tree follows changes: new, grown, renamed and deleted files and folders appear within about a second and the status bar shows "live, last change HH:MM:SS". A recursive directory watch notes which folders changed; those are re-read and compared with the tree. Changes made while the scan was running are applied when it finishes. A buffer overflow or a flood of changes triggers the same background rescan as a wrapped journal. `plumb --selftest-watch` checks create, grow, rename, new folder and delete end to end.
- `plumb-scan`: `watch` module (directory watcher) and `listing` module (`Entry`, `Listing`); `plumb::dirsync` applies listings to the tree.

### Known limitations
- The partial tree is approximate: only the first name of a hard-linked file appears, and records whose parent never arrives are not shown. The exact tree replaces it at the end. It needs about 200 MB more memory at 5M nodes until then.
- The filter box takes direct keyboard input only: no IME composition, no mouse text selection or undo, and a click puts the caret at the end of the text. Text wider than the box is clipped.
- A query matching most of a very large tree opens most folders; the first install on a 2M-node tree takes about 0.3 s on the UI thread.
- Directory-watch live updates see a file's size when the writer closes or flushes it; a renamed folder is re-listed (its expansion resets); hard links are not tracked; an elevated scan that falls back to the walker has no live updates; a watch that cannot be opened shows the stale banner with the reason.

## [0.2.0] - 2026-10-12

Live updates (milestone M2). Design: `docs/superpowers/specs/2026-10-12-plumb-live-updates-design.md`.

### Added
- `plumb`: live updates. On an elevated NTFS drive scanned with the MFT scanner, the tree follows the change journal: new, grown, renamed, moved and deleted files appear within about a second, the status bar shows "live, last change HH:MM:SS", and the selection stays on the same screen row. A wrapped journal or a pile of dead nodes triggers a background rescan (at most once per 10 minutes per drive, never within 30 s of input) that keeps the old tree on screen, then swaps the new one in with the sort, expanded folders, selection and scroll restored by path. If live updates stop for any other reason, a banner says why and offers a Rescan button. `plumb --selftest-live` (elevated) checks create, grow, rename and delete end to end.
- `plumb-scan`: NTFS change-journal reader (`journal` module: USN buffer parser, per-file state from the MFT record, a reader thread with a 250 ms coalescing window, `JournalSource` trait with a Windows implementation for `Volume`), elevated `journal-check` and `journal-spike` examples, and the MFT scan now also returns the record-number-to-node table (`MftReport`).
- `plumb-core`: in-place tree mutation (`add_child`, `set_size`, `rename`, `remove`, `reparent`, `NodeFlags::DEAD`) and `RowModel::refresh`, which keeps the selected row at the same screen row.

### Known limitations
- Live updates need an elevated process on an NTFS drive scanned with the MFT scanner; the walker path stays static after a scan.
- Only the first hard link of a file is tracked.
- Re-sorting a very large folder on each refresh is not incremental (about 244 ms for 1M children).
- Unverified: the journal `Timeout`/`BytesToWaitFor` units (no volume tried was quiet enough to see a timeout; the reader's spin guard bounds the cost); an automatic rescan after a wrapped journal on a real volume (only the fake-source tests cover it; deleting and recreating the journal gives the stale banner instead).

## [0.1.0] - 2026-10-12

First usable disk pane (milestone M1). Design: `docs/superpowers/specs/2026-10-11-plumb-disk-design.md`.

### Added
- `plumb-core`: struct-of-arrays file tree with bottom-up size and file-count sums, the virtualized row model (expand/collapse, sort by name/size/items, keyboard navigation, scroll to selection), and an NTFS MFT record parser (fixups, data runs, extension records, resident data, reparse tags). Criterion benchmarks for parsing, tree finishing and the row model.
- `plumb-scan`: `MftScanner` (streams `$MFT` from the raw volume, elevated NTFS only), `WalkScanner` (parallel directory walker, any volume, no admin), and `scan_auto`, which picks the MFT scanner when elevated on NTFS and otherwise the walker, falling back with a note if the MFT scan fails. `scan-compare` example checks the two agree (0 differences on the fixture).
- `plumb`: Win32 window drawn with Direct3D 11, Direct2D and DirectWrite on the adapter that drives the window's monitor; per-monitor DPI; light and dark to match the system; drive picker with free space; tree view with Name, Size, % of parent bar and Items columns; sort by header with direction arrows; mouse and keyboard navigation; scrollbar drag; context menu (Reveal in Explorer, Copy path, Properties); status bar with scanner, time, file count, skipped records and adapter; device-loss recovery; "Run as administrator" button that relaunches elevated and restarts the scan.
- Tests: WARP render smoke test, hidden-window `plumb --selftest` (opt-in: `cargo test -p plumb -- --ignored`).

### Known limitations
- The walker cannot detect hard links and may double-count them.
- An MFT with an `$ATTRIBUTE_LIST` is unsupported (falls back to the walker); only 1024-byte records on volumes whose cluster size is a multiple of 1024.
- No partial tree while a scan runs; the tree appears when the scan finishes.
- The tree is static after a scan (live updates are planned for 0.2.0).
- Alternate data streams count as 0; folder mount points are not offered in the picker.
- Idle memory is about 29 MB private (38.6 MB working set), of which about 27 MB is the GPU driver.
- Unverified: cloud-placeholder size parity between the scanners; UI frame time during a scan (checked by feel only).
