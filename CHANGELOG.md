# Changelog

All notable changes to plumb. Format follows [Keep a Changelog](https://keepachangelog.com/); all crates share one version and are bumped together at each tag.

## [Unreleased]

### Added
- `plumb-core`: `filter` module (case-insensitive substring or regex name matching with the `regex` crate, a name snapshot for worker threads, hits and the keep set) and `RowModel::set_filter`, which shows matches and their ancestors and dims rows under a leaf match. Not yet wired into the window.

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
