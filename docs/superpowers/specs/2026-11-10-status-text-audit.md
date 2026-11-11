---
status: done
---

# Status text audit: who says what

Why now: the 2026-11-10 screenshot of a scan of `D:` showed `6.2 GB` and `344,372` in the selection line, in the scan line under it, and in the root row's columns. This lists every surface that words a fact, the facts it words, and where two surfaces say the same one. Read from `app.rs` (`write_status`, `write_message`, `write_selection`, `write_process_status`, `banner`), `describe.rs`, `draw.rs` (status rows), `window.rs` (`summary`). Items marked *(unchecked)* were read from code only.

## Surfaces and what each owns today

| Surface | Wording | Built by |
|---|---|---|
| Rows (columns + tag chips) | name, size, % of parent, items, modified, attribute tags (denied, system, hardlink, ...) | `draw.rs` rows |
| Selection line (status row 1) | path, size, items (folders), attribute words; "N selected" for a set | `Description::write_line` |
| State line (status row 2) | scan progress, the post-scan summary, process totals, "live" / "live updates stopped", "refreshing" | `write_status` / `write_message` / `summary` |
| Banner | "live updates stopped: why; the tree may be out of date" | `App::banner` |
| Toast | the outcome of an action | `App::report` |
| Row tooltip | path, size, items, modified, executable, attribute meanings | `Description::write_tip` |
| Status tooltip | the whole status line with the full path and the time of the last change, and the GPU adapter | `write_status(full)` |
| Header chips | drive used / free; Processes memory | `chips()` |
| Basket chip + tinted tiles | count and bytes of what is marked | `basket_label` |

## Duplications

- **D1 size and items, always.** The selection line repeats the Size and Items columns of the row it describes. Evidence: `Description::write_line` writes `size` then `items`; the focus row shows both in its columns.
- **D2 attributes, always.** The selection line writes `attrs.words()` ("denied system") beside the tag chips on the same row.
- **D3 scan totals.** The scan line says `N folders, N files, X GB` while the root row shows the same size and file count growing, and with the root selected the selection line shows them a third time (screenshot, 2026-11-10: `6.2 GB` and `344,372` three times). Only the folder count is new.
- **D4 post-scan summary.** "Directory walk: N files in Ts" repeats the root row's Items for the file count; the scanner name, the time, "% of the used space", skipped records and "unreadable" are the new facts.
- **D5 live state.** Row 2 says "live updates stopped" and the banner says the same with the reason. The banner owns it; row 2 repeats its head.
- **D6 process totals: not a chip duplicate.** The Processes chip shows the machine's free memory (`ViewInfo.free`, set by `set_procs`; the disk chips show free space the same way), while row 2 shows the metric's total over all processes ("N processes, X memory"), so they are different numbers. Row 2's total is the Processes tree's synthetic root `Processes` (`plumb-core/src/procs.rs:87`), which is listed like a drive's root row, so the total repeats that row's Size column: the same shape as D3 (read from code, not seen on screen). A separate wording issue: a chip's number is free space but is not labelled so.
- **Not duplicates:** the row tooltip and the selection line are two renderings of one `Description`, the richer one on demand; the status tooltip repeats the state line with the full path by design; the `%` bar and its number are one value drawn twice (a style choice).
- **Misleading, not repeated:** with several rows selected the line reads "N selected | <focus row's size>", which looks like a total and is one row's size.

## Proposed rule

Each fact has one owner, and a surface that is not its owner does not repeat it:

- The row owns its own size, items, modified time and attributes (columns and tags).
- The selection line owns what a row cannot show: the full path, and the size of a set. For one row it is the path alone.
- The state line owns what no row shows: scan progress that is not in the tree yet (folders found), the summary's scanner, time, coverage and unreadable count, process totals, and the live state (the head only while the banner is up).
- The banner owns the reason a watch stopped; the toast owns action outcomes.
- Tooltips may restate their target in full; they are on demand.

This is `a1` in the 2026-11-10 discussion; it removes D1 and D2 by deleting `size`/`items`/`attrs` from `write_line` (the `with_who` form), and D3 and D4 by dropping the bytes and the file count from the progress and summary words only where the tree already shows them. It does not remove D5, which is the banner's head by design, and D6 with D3 by the same rule: the totals row 2 would keep are the ones no row shows, and the root row shows them here.

## Open

- **Decided 2026-11-10 (a1):** a multi-selection line reads "N selected, X" with X the real total (picked rows with no picked ancestor, `topmost()`); the "misleading" bullet above is fixed.
- **Checked 2026-11-10 (n1):** at the 50 DIP minimum the Size cell is 42 DIP wide and clips (CLIP option). Measured Body widths: "1.0 KB" 35, "999.9 GB" 51, "512.3 MB" 54, "1,023.9 GB" 61. So values from about 100 MB up are cut at the minimum; removing size from the selection line would hide it there (the tooltip still has it). Decided (b1): `MIN_COL_W` 50 -> 64, so the 56 DIP cell fits "999.9 GB" (51).
- **Decided (b3, as recommended): keep "N files" in the summary**; it is the only place a finished scan states its file count, and the root row counts items, not files.

## Outcome (c1, 2026-11-10)

Built as proposed, with the open questions as decided above:

- D1, D2: the selection line is the path alone (a process: name and PID); `Description::write_line` lost its size, items and attribute words, and the `with_who` form is gone.
- D3: while a scan runs, the line says `N folders` once a tree is listed (files and bytes are the root row's); before the first tree it keeps the full counts.
- D4: unchanged by decision b3.
- D5: row 2 leaves out "live updates stopped" while the banner is up; the status tooltip keeps it.
- D6: row 2 reads `N processes, sized by <metric>`; the total is the root row's.
