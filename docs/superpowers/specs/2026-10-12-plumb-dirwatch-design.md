# plumb: live updates without admin (directory watch)

Status: draft for review, 2026-10-12. Builds on `2026-10-12-plumb-live-updates-design.md` (tree mutation, `Pacer`, `RescanPolicy`, the stale banner), `2026-10-12-plumb-partial-tree-design.md` and the filter design. The live-updates spec parked this as "different mechanism and lower fidelity; after this".

## Goal

After a walker scan, keep the tree current as files change, without admin and on any filesystem the walker can scan. The elevated NTFS path keeps the change journal, which is more faithful; this adds the other half.

## Decisions

| Question | Decision |
|---|---|
| Which scans | Those the walker certainly runs: not (elevated and NTFS). An elevated scan that falls back to the walker after an MFT failure stays static, as today |
| Mechanism | One recursive `ReadDirectoryChangesW` handle on the drive root. Events only mark directories dirty; the filesystem is re-read to learn what is true now |
| How a change is applied | Re-list the dirty directory with the walker's `list_dir` and diff it against the tree's children by name. Not per-event interpretation, which would need rename pairing and per-kind cases and would still need the diff as its fallback |
| Gap between listing and watching | The watcher starts when the scan starts and collects dirty directories during the scan; they are applied once the tree is installed. No gap |
| When it cannot continue | Buffer overflow, too many dirty directories, or a failed handle: background rescan under the existing `RescanPolicy`, otherwise the stale banner |
| Fidelity | Documented limits below |

## Components

### Watcher (`plumb-scan::watch`, Windows)

Two threads per watch, both at below-normal priority.

- **Reader.** Opens the root with `FILE_LIST_DIRECTORY | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED` and the full share mode, issues `ReadDirectoryChangesW` with a 64 KB buffer (the network limit), `bWatchSubtree = TRUE` and the filter `FILE_NOTIFY_CHANGE_FILE_NAME | DIR_NAME | ATTRIBUTES | SIZE | LAST_WRITE`, and reissues the read at once after each completion. It never lists. For each record it adds to a shared dirty map keyed by the parent directory's relative path. A record whose action is `ADDED` or `RENAMED_NEW_NAME` and which is a directory (the name cannot say, so the reader checks it with `GetFileAttributesW` on the new path; a failure counts as gone) marks that directory as `deep`: the lister lists its whole subtree. A zero-byte completion or `ERROR_NOTIFY_ENUM_DIR` is an overflow.
- **Lister.** Every 250 ms (`WatchConfig::window`) takes the dirty map and lists each directory with `list_dir`; a `deep` directory is listed recursively, without descending reparse points, as the walker does. It sends `Event::Listings(Vec<Listing>)` ordered parents first (by path depth), where `Listing { dir: Vec<u16> /* relative path, \ separated, empty = root */, entries: Result<Vec<Entry>, ()> }`. During the scan it keeps accumulating and sends nothing until the consumer asks (`release()`); batches are never dropped.
- **Stops.** `Event::Stopped(Stop::Overflow | Stop::TooMany | Stop::Failed(String))`. `TooMany` is more than 50 000 dirty directories at once once the watch is released (the tree is stale anyway; rescan). While held there is no cap: the map is bounded by the number of directories on the drive, and a scan that just finished should not be thrown away for having been long.
- `Watcher::start(root, config, wake) -> Result<Watcher, String>`, `try_recv`, `release`, `stop` (cancels the I/O and joins), mirroring `JournalReader`.

`Entry` and `list_dir` move from private to `pub(crate)`; `Entry` is re-exported publicly because `Listing` carries it.

### Applier (`plumb::dirsync`, UI thread)

`DirSync::apply(tree, queue, deadline) -> usize` consumes `Listing`s in order within the 5 ms `APPLY_BUDGET`, like `Live::apply`.

- Resolve `dir` to a node by walking the relative path from the root, comparing names case-insensitively (a simple lowercase fold; a mismatch makes the diff remove and re-add, which heals itself). Unresolvable: skip (its parent's listing, earlier in the batch or already applied, adds it first).
- `entries == Err`: skip. The parent's listing removes a vanished directory.
- Diff `entries` against the node's live children by case-folded name:
  - in the listing only: `add_child` (name, `alloc`, DIR and REPARSE flags);
  - in the tree only: `remove`;
  - in both with the same kind: `set_size` if the size differs; flags updated if REPARSE changed;
  - in both with a different kind (file became directory or the reverse): `remove` then `add_child`.
- Directories added this way arrive with their own `deep` listings in the same batch, parents first, so they fill in.
- Idempotent: a duplicate or late batch changes nothing.
- The tree's `touched` log and the existing `refresh_rows` path (filter included) are unchanged.

### Window and app

- A new `Watch` variant: `Watch::Dir { watcher, sync, queue, pacer, dirty, armed }`, next to the journal one (an enum over the two; `apply_live`, `live_tick`, `drop_watch`, `journal_stopped` dispatch on it). `TIMER_LIVE`, `Pacer` and the `needs_compaction` auto-rescan are shared.
- Start: in `spawn_scan`, when `!(is_elevated() && is_ntfs(root))`, start a watcher (held). On `scan_done` (foreground or swap) release it and install it as the live watch, dropping the previous one. A scan that fails drops it. For a background rescan the old watch keeps applying until the swap; the new watcher is held until then.
- Overflow and `TooMany` go through the same path as a wrapped journal: `auto_rescan` if the policy allows, else `LiveStatus::Stale("the directory watch overflowed")`.
- Status: `live, last change HH:MM:SS` as today.
- `--selftest-watch` (non-elevated): scans a temp directory, then creates, grows, renames and deletes files and checks the tree follows each within 20 s.

## Known limits

- NTFS reports size changes when the writer closes or flushes the file, so a file being written shows its old size until then.
- A renamed folder is a remove plus an add: its expansion state resets and its subtree is re-listed.
- Hard links are not tracked (the walker counts each name).
- Network and other filesystems report what their driver reports; a missed notification is not noticed until the next rescan.
- Case folding is a lowercase approximation of NTFS's table.
- An elevated scan that falls back to the walker has no live updates.

## Tests

- **Applier (pure):** add, remove, grow, shrink, kind change, new directory with a nested listing in one batch, duplicate batch, unresolvable directory, case-only difference, root listing; sizes and file counts against a tree built from scratch from the final state.
- **Watcher (temp directory):** create, grow, rename, delete and a new directory with files produce listings that, applied to a walker-built tree, equal a fresh walk. Waits use deadlines, not sleeps.
- **Overflow:** a fake that reports overflow ends the watcher with `Stop::Overflow`; `TooMany` at the threshold.
- **Hold and release:** changes made after `start` and before `release` are applied, in order.
- **Selftest:** `--selftest-watch`.

## Risks and open questions

- **Spike first.** Not verified: that a non-elevated process can watch a drive root and receive events for subdirectories it cannot read; the event rate on a busy `C:`; whether 64 KB overflows in normal use. The spike watches `C:\` for 30 s and reports events per second, distinct directories, whether any overflow occurred, and a sample of paths from protected folders. If events from unreadable folders are missing the design still holds (they are not in the tree either); if the rate overflows 64 KB the design needs a larger buffer on local volumes or filters.
- **Event storms.** Browser caches and temp directories churn constantly. The dirty map coalesces per directory, so the cost is directories per window, not events. If the spike shows tens of thousands per window, the lister needs a time budget per cycle and a skip list for directories re-listed every cycle.
- **Memory.** The dirty map is capped by `TooMany`; listings are transient.
- **Gap between scan and listing.** A directory the walker listed early in a long scan can change before the scan ends; the watcher's dirty set covers it. The reverse (a change the walker saw and the watcher also reports) re-lists harmlessly.

## Out of scope

Per-event interpretation, tracking hard links, live updates after an elevated walker fallback, following junctions, a larger buffer or filters unless the spike asks for them.
