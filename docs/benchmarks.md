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

## UI

Manual checklist (plan 3b, Task 3) passed on 2026-10-11 with nothing to note: scan progress, responsiveness while scanning, tree navigation and sorting, context menu, light/dark, DPI, exit, and the Run as administrator relaunch. No frame times or idle CPU were measured; the "scan threads starve the UI" question was answered by feel only. Hidden-window `plumb --selftest` picked the Radeon 780M (the laptop display's adapter).

Idle check, 2026-10-11 (release build, launched minimized on the 780M, no scan; machine CPU 66% from other work, Defender state not checked): 0.00% of a core over 10 s; working set 38.6 MB, private bytes 29.0 MB (spec target: under 30 MB, so private meets it and working set does not); main window existed 220 ms after launch (not a first-frame time). Script: a throwaway PowerShell snippet, not kept. Repeat on a quiet machine before trusting the memory figure.

Where the idle memory goes (2026-10-11, throwaway example, release build; each figure is private bytes): process start 1.4 MB; creating the D3D11 + D2D + DirectWrite stack on the Radeon 780M 29.4 MB; on the WARP software device 2.7 MB. A full-window render target plus first paint adds about 4-9 MB (varied run to run) and then plateaus: 200 further paints grew nothing. So about 27 MB of the idle footprint is the AMD user-mode driver loaded by creating a hardware device; plumb's own share is a few MB. The working-set figure (38.6 MB) adds shared DLL pages on top. Meeting "under 30 MB" with a hardware device is not possible; using WARP for this mostly static text UI would, at the cost of CPU drawing and of breaking the spec's "adapter that owns the monitor" decision. Not done; parked.
