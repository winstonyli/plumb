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
