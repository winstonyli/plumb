# Benchmarks

Criterion benchmarks for the CPU hot paths: `cargo bench -p plumb-core --bench core`.
Run them only when CPU load is under ~20%; write the load beside the numbers.

## Baseline 2026-10-11

Machine: Ryzen 7 7840U laptop. CPU load during the run: ~24% (other sessions' jobs; slightly over target). Defender real-time protection: off.

| Benchmark | Time (low / mid / high) | Throughput |
|---|---|---|
| mft/parse_1024_records | 30.8 / 32.0 / 33.4 µs | ~30 GiB/s of record bytes |
| tree/finish_5m_nodes | 33.4 / 35.6 / 38.1 ms | |
| rows/expand_1m_children_sort_by_size | 185 / 199 / 220 ms | |
| rows/visible_window_of_50 | 2.0 / 2.1 / 2.2 ns | |

Caveats: the parse benchmark repeats one identical record (hot cache, perfectly predicted branches) and only reads the headers and attributes, so it is a best case; real MFT parsing will be slower. The synthetic k-ary tree is laid out in parent order, which makes `finish` cache-friendly; scanner-built trees will be less so.

## Real-drive walker scans 2026-10-11

`crates/plumb-scan` example `walk` over `C:\Users\Winston Li` (NTFS, internal SSD), release build, no admin. CPU load before the run: ~41% (other sessions' jobs), so these are not clean numbers. Caches were warm: the same tree had been walked minutes earlier. Defender real-time protection: off.

| Threads | Dirs | Files | Allocated | Scan | Sums |
|---|---|---|---|---|---|
| 1 | 405,693 | 3,373,213 | 588.92 GiB | 39.5 s | 36 ms |
| 4 | 405,705 | 3,373,278 | 588.92 GiB | 41.5 s | 36 ms |
| 8 | 405,714 | 3,373,332 | 588.92 GiB | 43.8 s | 41 ms |

Counts differ slightly between runs because other processes were creating files.

Observations:
- More threads did not help in this warm, loaded run (about 12 µs per entry on one thread). An earlier cold run in the verification session went from 124 s on 1 thread to 51 s on 4 threads, so threads help when I/O latency dominates.
- Open question: with warm caches, is the limit the coordinator thread (it pushes every entry) or the kernel's directory-query path? Test on a quiet machine, and profile the coordinator, before tuning.

## MFT scanner, full volume 2026-10-11

`C:` (NTFS), elevated, release build, `scan-compare` (which also builds a 16-entry fixture, so each pass includes that). Machine load was not measured; other sessions were active. Defender real-time protection: off.

| Run | Nodes | Stream (read + parse) | Link (merge) |
|---|---|---|---|
| First (cold) | 4,307,176 | 8.84 s | 0.76 s |
| Warm, typical | ~4.307M | 2.7-3.5 s | 0.6-0.9 s |
| Final run | 4,308,122 | 4.64 s | 0.60 s |

Compare the walker over the user profile (3.37M files, 40-125 s). The MFT scan covers the whole volume in a few seconds, so the walker is the fallback only. Orphans: 0. Corrupt records: 0.

Parity: `scan-compare` reports 0 differences between the walker and the MFT scanner on a fixture with plain, empty, 1-byte, resident-boundary, large, Unicode, 200-character-name, alternate-stream, compressed, sparse and junction entries. The first run found the walker counting resident data as its rounded length; fixed in `50d402e`.

## Live updates, core mutation 2026-10-12

Machine load during the run: ~49% CPU (other work; well over the 20% target, so pessimistic). Defender real-time protection: off. Criterion 10 samples.

| Benchmark | Time (low / mid / high) | Throughput |
|---|---|---|
| mutation/apply_100k_deltas_to_5m_nodes | 48.0 / 53.1 / 58.9 ms | ~1.9 M deltas/s (mix of set_size, add_child, rename, remove) |
| rows/refresh_after_one_add_in_1m_children | 8.3 / 8.9 / 9.6 ms | was 244 ms before `reposition` (2026-10-12, ~49% machine load) |
| rows/refresh_nothing_touched_in_1m_children | 6.6 / 6.9 / 7.2 ms | |

Findings for plan 3 (UI wiring):
- One change in a very large folder re-sorts that folder (244 ms at 1M children, far over the 5 ms apply budget). Coalesce so `refresh` runs at most once per repaint tick. Done: `RowModel::reposition` drops the touched nodes and their ancestors from each affected cached list and merges them back in one pass (no re-sort); the remaining ~9 ms is the merge copy plus the row-list rebuild.
- A no-op `refresh` still rebuilds the row list (~7 ms at 1M visible rows); skip it when no delta was applied.

## UI

Manual checklist (plan 3b, Task 3) passed on 2026-10-11 with nothing to note: scan progress, responsiveness while scanning, tree navigation and sorting, context menu, light/dark, DPI, exit, and the Run as administrator relaunch. No frame times or idle CPU were measured; the "scan threads starve the UI" question was answered by feel only. Hidden-window `plumb --selftest` picked the Radeon 780M (the laptop display's adapter).

Idle check, 2026-10-11 (release build, launched minimized on the 780M, no scan; machine CPU 66% from other work, Defender state not checked): 0.00% of a core over 10 s; working set 38.6 MB, private bytes 29.0 MB (spec target: under 30 MB, so private meets it and working set does not); main window existed 220 ms after launch (not a first-frame time). Script: a throwaway PowerShell snippet, not kept. Repeat on a quiet machine before trusting the memory figure.

Where the idle memory goes (2026-10-11, throwaway example, release build; each figure is private bytes): process start 1.4 MB; creating the D3D11 + D2D + DirectWrite stack on the Radeon 780M 29.4 MB; on the WARP software device 2.7 MB. A full-window render target plus first paint adds about 4-9 MB (varied run to run) and then plateaus: 200 further paints grew nothing. So about 27 MB of the idle footprint is the AMD user-mode driver loaded by creating a hardware device; plumb's own share is a few MB. The working-set figure (38.6 MB) adds shared DLL pages on top. Meeting "under 30 MB" with a hardware device is not possible; using WARP for this mostly static text UI would, at the cost of CPU drawing and of breaking the spec's "adapter that owns the monitor" decision. Not done; parked.

Filter highlight cost, 2026-10-14 (throwaway ignored test, release build, WARP offscreen device, 1600x1400 target, 59 visible rows of a 5000-node tree, `Painter::paint` timed over 100 frames after a warm-up; machine CPU 14%, Defender real-time protection off; WARP is software, so it is only a bound on the CPU-side recording work, not a GPU frame time): no filter 0.52 and 0.96 ms/frame in two runs; filter `n` (1 span per name) 0.49 and 0.99; regex `.` (every character a span, 2-3 per name here) 0.48 and 0.99. The run-to-run spread is larger than any effect of the highlights, so measuring each span with a fresh DirectWrite layout costs nothing visible at this row count. Not tried: very long names with hundreds of spans. Nothing to optimise; the test was not kept.

## Walker: hard links and alternate data streams, 2026-10-14

`crates/plumb-scan/examples/links-spike.rs` (kept), release build, single-threaded, warm cache, not elevated, machine CPU 21%, Defender real-time protection off. It lists a tree with `FileFullDirectoryInfo` and with `FileIdExtdDirectoryInfo` (the same listing plus a file id per entry), counts files whose id was already seen, and times `FindFirstStreamW` on a sample of files.

| Tree | Entries | Listing plain / with ids | Extra links (share of counted bytes) | Set cost | Streams: us/file, named found |
|---|---|---|---|---|---|
| `C:\Windows` | 298,843 | 13.30 / 13.75 us per entry | 71,237 (22.5%, 11.7 of 51.9 GiB) | 0.025 us/file, ~4 MiB | 162, 0 of 34,088 |
| `C:\Users\Winston Li\Documents` | 834,116 | 11.76 / 11.23 | 11,945 (10.8%, 24.3 of 225.2 GiB; cargo `target` dirs) | 0.030 us/file, ~8 MiB | 114, 0 of 20,556 |
| `C:\Program Files` | 60,947 | 5.93 / 5.70 | 83 (0.4%) | 0.011 us/file | 92, 0 of 54,625 |
| `C:\Users\Winston Li\Downloads` | 1,510 | 5.45 / 5.75 | 0 | - | 106, 1 of 1,341 (26 bytes, resident) |

- **Hard links:** the walker counts every link, the MFT scanner counts a file once (`HARDLINK`), so the two disagree by 10-22% on trees with `WinSxS` or cargo output. Asking for file ids costs nothing measurable in the listing (the differences are within run-to-run noise, both ways), and de-duplicating is one hash-set insert per file (about 12 bytes each). Cheap and worth doing. The id is 128 bits; NTFS uses the low 64. Done in 0.9.0: the walker now counts a file once.
- **Alternate data streams:** 92-162 us per file to look (an open per file), 7-12x the cost of listing an entry, and none of about 110,000 sampled files had a stream that uses clusters (the one named stream found was a 26-byte `Zone.Identifier`, resident in the MFT record, so counted as 0 either way). The MFT scanner also ignores them (`parse_data`: "ignored in v1"), so the scanners agree. Not worth the cost; the gap stays, now measured. Only a sample was probed, not whole drives.
- The `C:\Users\Winston Li` tree did not finish in 140 s (not measured).

## Partial-tree spike, MFT records through `Live::apply` 2026-10-12

Throwaway example (deleted after the run; the code replayed `scan_auto`'s MFT records in record order as `Delta::Upsert`s through `Live::apply` into a fresh tree, 4096-record chunks, 5 ms slices, timing only the applying). `C:` elevated, release build, machine load not measured, Defender state not recorded.

| Measure | Value |
|---|---|
| MFT scan | 3.19 s wall (stream 2.46 s, link 0.56 s), 4,364,019 nodes, 4,586,496 record slots |
| Upserts applied | 4,086,853 in 507 ms: 124 ns each, 8.06 M/s, 16% of the scan wall time |
| Slices | 1122; worst 17.99 ms (at record 3,321,856), over the 5 ms budget; cause not found (a `Vec` regrowth or a parked retry) |
| Parked at the end | none |
| Preview tree | 4,086,855 nodes against 4,364,019 exact (only the first name of a hard-linked file); root subtree 832,802,152,448 B, identical to the exact tree |

Conclusion: the UI thread keeps up with an MFT scan with a wide margin. The worst slice is one outlier to chase in the plan (pre-reserve capacity, then re-measure).
