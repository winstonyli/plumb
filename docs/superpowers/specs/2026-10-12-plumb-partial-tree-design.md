# plumb: partial tree while a scan runs (milestone M3, part 2)

Status: approved, 2026-10-12. Builds on `2026-10-11-plumb-disk-design.md` ("no partial tree while a scan runs" was a v0.1 limitation), on the live-updates design (`Live`, `Tree` mutation, `swap_tree`, `Pacer`) and on the filter design (the filter follows tree changes through the touched-id log).

## Goal

Show the tree as soon as a scan has found something, let the user browse it while the scan runs, and finish with the exact result. The scan itself must not get slower.

## Decisions

| Question | Decision |
|---|---|
| What is shown | A live tree: folders appear as they are found and show the bytes found so far. The rows update about 4 times a second |
| Which scanners | Both. The walker (no admin) and the MFT scanner (elevated) |
| Mechanism | Incremental deltas into a preview tree owned by the UI thread. Not snapshot-and-swap, and no unification with the exact build |
| Exactness | The preview is approximate. The exact tree is still built by `TreeBuilder::finish` / `MftMerger::finish`, unchanged, and replaces the preview when the scan ends |
| What is interactive | Everything: expand, collapse, select, sort, scroll, the filter. The drive chips stay disabled. The status line shows "scanning..." and the running counts |
| Slow consumer | Best effort. If the UI falls behind by more than the cap, the scanner never blocks: the preview stops following and says so |
| Rescans | A rescan with a tree on screen keeps today's behaviour (old tree stays, new one swaps in). No preview |

## Why not the alternatives

Measured figures are from `docs/benchmarks.md` (2026-10-11 and 2026-10-12), not re-measured for this spec.

- **Snapshot and swap.** Each snapshot is O(nodes so far). Cheap for `TreeBuilder` (`finish` is 33-38 ms for 5M nodes) but the MFT merge is 0.6-0.9 s per snapshot, so a 3-10 s scan would get one or two. Rejected for the user's choice of live deltas.
- **Exact incremental (delete `finish`, build the final tree through the deltas).** Per-node ancestor sums cost about 2-3 s of single-threaded work for 4.3M nodes against 35 ms for the batch pass, put per-record resolution inside the merge lock or onto one thread, lose the determinism guarantee in `merge.rs` and the parent-ordered layout, and touch about 15 files that use `TreeBuilder`. Rejected: the preview is replaced at the end anyway, so exactness during the scan is not worth that cost.

## Data flow

```
scanner threads --PreviewMsg--> bounded queue --> UI thread: apply in a 5 ms slice per tick --> preview Tree
scanner threads --(unchanged)--> TreeBuilder / MftMerger --finish--> exact Tree --swap_tree--> replaces the preview
```

- `PreviewMsg` is a new enum in `plumb-scan`:
  - `Add { parent: NodeId, name: Vec<u16>, size: u64, flags: NodeFlags }` (walker),
  - `Denied(NodeId)` (walker; a directory that could not be listed),
  - `Upsert(FileState)` (MFT; `FileState` and its record rules are the journal's),
  - `Size { record: u64, size: u64 }` (MFT; sizes found in extension records),
  - `Reset` (a scanner gave up and another one starts, see below).
- Scanners get an optional sender, passed with `Progress` (the exact signature is the plan's decision). With no sender they behave as today, so tests and `scan-compare` are unaffected.
- The queue is bounded by a count (about 200k messages, tens of MB) and uses `try_send`. A send that finds it full sets a sticky `lagged` flag and drops that message and every later one; it never blocks the scanner. Sticky matters for the walker: a gap would put every later id out of step, so nothing after the first drop may get through.

## Walker preview

The walker's coordinator thread owns the builder and pushes nodes in a fixed order, so the ids it assigns are 0, 1, 2, ... in push order. For every push it sends `Add`. The preview tree replays them with `Tree::add_child` in arrival order, so its ids equal the builder's ids and no lookup table is needed. The first message is the root (`parent = NodeId::NONE`), which the preview creates as a root rather than a child.

After a dropped message the preview stops following (see Slow consumer); the prefix it has applied is still consistent.

`add_child` pushes at the head of the parent's child list; row order comes from the sort, so that does not show.

## MFT preview

- A parser sends one `Upsert` per in-use base record that has a name. The `FileState` comes from the helper `observe` already uses, factored out so both share it: the name is the one with the lowest (parent record, name), which is the same name that carries the size in the exact build, so a hard-linked file's size appears once, under that name. The preview applies it with `Live`'s existing upsert logic: add under the parent if it exists, otherwise park, and fix kind, name, place and size when the record is seen again.
- Records in the MFT are in creation order, so a parent is usually placed before its children. A child whose parent has not arrived is parked.
- **Size from extension records.** Large, fragmented files carry their data attribute in extension records (the 220 GB gap found when the progress counter was fixed). Parsers send `Size { record, size }` for them. If the file's node exists, `set_size`; otherwise the size is remembered in a small map and applied when the node is created.
- **Changes to `Live` for preview use** (the journal path keeps today's behaviour):
  1. A `preview` mode where parked children are not sent to `<orphans>` after two settle passes; they wait for the scan to end.
  2. Parked upserts are indexed by parent record (a map from parent record to waiting children) so placing a directory releases exactly its waiting children. The current `settle` retries every parked item on every pass, which is quadratic if parents arrive late.
  3. The root node and `records[5]` are created before the first message is applied (the current code ignores record 5).
  4. A parent must be a directory to adopt a child; a record whose parent node is a file stays parked. (`upsert_or_park` today checks only that the parent node exists.)
- The preview's own approximations, all dropped by the final swap: only one name per hard-linked file is shown (the other names are absent; sizes are unaffected because only the carrier name has size); unresolved cycles; children whose parent never arrives (or arrives as a file) stay parked and are not shown; records that are freed later are not removed.

## Slow consumer and errors

- **Falling behind.** When the sender sees a full queue it sets `lagged`. The UI stops applying messages, keeps the preview as it is, and shows "preview paused" in the status line until the exact tree arrives. The scan is unaffected either way.
- **Scanner fallback.** `scan_auto` may start the MFT scanner, fail, and fall back to the walker. The failing scanner sends `Reset` after its threads have ended (the existing code already undoes the progress counts there). The UI drops the preview tree and starts a new one.
- **Scan failure.** `Scan::Failed` clears the preview as `fail_scan` does the tree today.
- **Scan finishes very early.** If the exact tree is ready before any preview message is applied, nothing is previewed; the flow is today's.
- **Cancel / drive switch.** Not new: chips are disabled while a scan runs, and window close ends the process.

## App and window

- `Scan::Running` stays for the whole scan (so the counters and status line keep working). A new `App::preview_install` creates `App.tree` and `App.model` from the preview on the first applied batch, expands and selects the root, clears `scroll`, and calls `filter_reset` as every tree replacement does.
- A preview timer applies messages for at most 5 ms (the live-update `APPLY_BUDGET`), then calls `refresh_rows` and invalidates. Its pace is `REPAINT` (250 ms) stretched by the existing `Pacer`, because a refresh re-sorts any folder that changed (244 ms for 1M children) and a huge folder must not make the window stall four times a second. `refresh_rows` already runs `filter_live` first, so an active filter keeps its matches and count current; typing a filter into a preview tree works through the normal worker path.
- The timer stops when the exact tree arrives; messages still queued at that point are discarded. `scan_done` then runs `swap_tree` (not `finish_scan`) when a preview is on screen, so sort, expansion, selection and scroll are restored by path. The filter is reset by `swap_tree` as it is today and re-run if the box has text.
- The drive chips and the Rescan button are already disabled while `Scan::Running` (`app.rs` guards both), so a tree on screen during a scan needs no new guard. Code that treats `app.tree.is_some()` as "the scan is done" must test `Scan::Idle` instead; the selftest's `pump_until` is one.
- The progress counters and the "scanning..." status line are unchanged. Live updates (journal) start only after the exact tree is installed, as today.
- Memory: the preview is a second tree next to the builder (about 200 MB more at 5M nodes) until the swap.

## Tests

- **Walker:** a scan with a sender yields a preview whose ids, names, flags and sizes equal the builder's tree, node for node (fixture and a generated deep/wide tree).
- **MFT:** over random record orders, after every message is applied the preview's per-path sizes equal the exact tree's, except for the documented approximations (a hard-link fixture asserts the difference rather than hiding it). Extension-record size arriving before and after its base. Parents arriving after children, including a chain in reverse order, release in one pass (the parent-indexed map).
- **Slow consumer:** a full queue sets `lagged`, the scan completes with the exact result, and scan time is within noise of a run with no sender.
- **Fallback:** a failing MFT scan sends `Reset`; the preview restarts from the walker.
- **App:** `swap_tree` after a preview keeps the user's expansion, selection and scroll; a filter typed mid-scan survives the swap; sort changes during the preview persist.
- **Selftest (`plumb --selftest`):** the hidden window scans the fixture with a sender attached and checks the final tree and the filter as today. The fixture scan can finish before the first preview tick, so preview content is covered by the core and app tests, not the selftest.
- **Real drive (manual, elevated and not):** the first rows appear within a second on `C:`; the final size matches the progress counter and a run with the preview off.

## Risks and open questions

- **Preview throughput on the MFT scanner.** Measured by a spike on `C:` (2026-10-12, `docs/benchmarks.md`): 4.09M upserts apply in 507 ms (124 ns each, 16% of the 3.2 s scan), the preview's root size equals the exact tree's, and nothing stays parked. The worst of 1122 slices took 18 ms against the 5 ms budget; the plan pre-reserves tree capacity and re-measures.
- **Parked memory.** A `FileState` with a name is about 100 bytes. With the parent index, worst case is the whole volume parked (reverse order); the queue cap does not bound `parked`. Cap parked entries (for example 1M) and freeze the preview past it, like `lagged`.
- **`Tree::touched` growth.** The log grows until `refresh` consumes it; a tick that applies without refreshing would grow it. The tick always refreshes.
- **Two scanners, two feeders.** The walker replays pushes and the MFT scanner sends records, so there are two feeders, not one. This is accepted: they are small, and the apply side is shared (`Tree`, `Live`).

## Out of scope

A preview for rescans, cancelling a scan, per-folder "still scanning" markers, a `≥` mark on partial sizes (cheap to add later), exact incremental builds, unifying the batch and incremental builders.
