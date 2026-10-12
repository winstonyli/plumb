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
