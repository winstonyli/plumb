# plumb: live updates design (milestone M2)

Status: draft for review, 2026-10-12. Builds on `2026-10-11-plumb-disk-design.md`, which parked this work as M2 and which this spec amends (the "finished tree is read-only" rule).

## Goal

After a scan, keep the tree current as files change, without rescanning, while the user is looking at it. Selection and scroll position stay put. Scope is **elevated NTFS volumes only**, because only they have the change journal and a readable MFT. Everything else keeps its static tree, as today.

## Decisions

| Question | Decision |
|---|---|
| What a live update does to the view | Applied between frames; the selected node stays selected and stays at the same screen row; rows may reorder around it |
| When live updates cannot continue | Background rescan, rate-limited; stale banner with a Rescan button as the fallback |
| How the tree takes changes | Mutated in place, append-only: new nodes are appended, removed nodes are marked dead; `NodeId`s never change |
| Slot reuse | No free list in v1; compaction is the background rescan |
| Threading | A journal thread per volume sends deltas over a channel; the UI thread applies them between frames; no locks |

## Tree changes (`plumb-core`)

- New flag `NodeFlags::DEAD`. A removed node keeps its slot, its links and its name; `Tree::children` skips dead nodes. Nothing is unlinked, so removal is O(depth) for the size fix-up only, not O(siblings).
- `Tree::add_child(parent, name, size, flags) -> NodeId` appends a node and links it at the head of the parent's child list. The name goes on the end of the pool.
- `Tree::set_size(id, new_size)` changes a node's own size and adds the difference to `subtree_size` of the node and every ancestor. `subtree_files` is adjusted the same way when a file is added or removed.
- `Tree::remove(id)` marks the node and its whole subtree dead, subtracts the subtree's size and file count from the ancestors, and counts the dead nodes (`Tree::dead`).
- `Tree::rename(id, name)` appends the new name to the pool and repoints `name_off`/`name_len`; the old bytes stay behind as waste.
- `Tree::reparent(id, new_parent)` unlinks the node from its old parent's child list (a walk over that list, acceptable because moves are rare), subtracts its subtree size and file count from the old ancestors, links it under the new parent and adds them to the new ancestors. A moved directory keeps its children and its `NodeId`, so the children need no events of their own.
- Dead nodes cost about 80 bytes each (a 39-byte slot plus the name). The name pool stays append-only.
- Why no free list: `RowModel` keeps `NodeId`s in its expansion bitset, selection and sort cache, so reusing a slot would hand a new file the old file's expanded state or selection; the pool cannot reclaim names of different lengths anyway. A heavy-churn drive (100k creates an hour) grows by about 8 MB an hour; the rescan compacts it.

## Row model (`plumb-core`)

- `RowModel::refresh(&mut self, tree: &mut Tree, scroll: usize) -> usize` consumes `Tree::take_touched()` (so the list cannot be forgotten), drops the cached sorted order of every touched node and of its whole parent chain (their sizes changed), then rebuilds the flat row list and returns the anchored scroll. Cost: a change in a 1M-child folder re-sorts it (~244 ms measured); see `docs/benchmarks.md` for the parked incremental alternative.
- Anchoring: before refresh, record the selected node and its screen row (index in the row list minus scroll). After refresh, find the node's new row index and return the scroll that keeps it at the same screen row, clamped to range. If the selected node died, select its nearest live ancestor.
- Dead nodes never appear in rows. Their ids may linger in the expansion bitset; harmless, because they are never looked up again.

## Journal reader (`plumb-scan`)

New module, elevated NTFS only. It sits behind a small trait for the volume calls so tests can use a fake, as `MftScanner` does with `VolumeRead`.

- **Start:** before the scan begins, `FSCTL_QUERY_USN_JOURNAL` records the journal ID and `NextUsn`. The thread starts reading from that `NextUsn` when the scan finishes, so changes made during the scan are replayed.
- **Read loop:** `FSCTL_READ_USN_JOURNAL` with a wait (`BytesToWaitFor` and a timeout), so an idle volume does not poll. Reason mask: file create, delete, rename, size change (data extend/truncate/overwrite), close, basic-info and reparse changes.
- **Coalescing:** records are collected for about 250 ms and merged by file reference; a burst of writes to one file produces one update. Reusing a record number with a new sequence number counts as delete plus create.
- **Per changed file:** read its MFT record with `FSCTL_GET_NTFS_FILE_RECORD`. NTFS returns the nearest in-use record at or below the requested number, so if the returned number differs the file is gone and the delta is `Remove`. Otherwise `parse_record` (the existing parser) gives name(s), parent, allocated size, flags, reparse tag.
- **Deltas:** `Upsert { record, parent_record, name, size, flags }` and `Remove { record }`. Hard-linked files follow the existing rule: the size sits on the first name by (parent record, name).
- **Extension records:** a file whose size lives in extension records cannot be sized from its base record alone. For those, fall back to opening the file by ID and reading its allocation size. Counted in the stats so the frequency is visible.
- **Stops, with a reason:** `ERROR_JOURNAL_ENTRY_DELETED` or a journal ID change (the journal wrapped or was recreated: rescan), `ERROR_JOURNAL_NOT_ACTIVE` or delete in progress (disabled: banner), any other read error (banner).

## Applying deltas (`plumb`, UI thread)

- A record-number-to-`NodeId` table (`Vec<NodeId>`, about 17 MB for 4M files) is built when a volume's scan finishes, only for volumes with live updates. The MFT scanner's output is in record-number order, so it is filled in one pass.
- `Upsert` for a known record: `set_size` if the size changed, `rename` if the name changed, `reparent` if the parent changed (a different parent record is looked up through the table). The three can combine in one update.
- `Upsert` for an unknown record: `add_child` under the parent found through the table; if the parent is unknown (a create whose parent's own event has not been applied yet), the delta is parked for the next batch, and after two batches it goes under `<orphans>`.
- `Remove` for a known record: `remove`; unknown: ignored.
- The batch is drained from the channel each frame with a budget of about 5 ms; the rest waits for the next frame. Repaints from live updates are capped at 4 Hz, so a build tool hammering the volume does not pin the UI.
- The status bar shows a "live" marker with the time of the last applied change.

## Failure, rescan and compaction

- **Wrapped or recreated journal:** start a background rescan of that volume with the existing `scan_auto`, off the UI thread. The old tree stays on screen with "refreshing…" in the status bar. When the new tree is ready it replaces the old one; selection, scroll position and expanded folders are restored by matching paths, and anything that no longer exists is dropped.
- **Rate limit:** at most one automatic rescan per volume per 10 minutes, and not within 30 seconds of the last user input. A rescan that is not allowed falls back to the stale banner.
- **Disabled journal or read error:** keep the tree, show "live updates stopped: <reason>; the tree may be out of date" with a Rescan button.
- **Compaction:** when dead nodes exceed 25% of live nodes, or pass 1M, the same automatic rescan runs (same rate limit).
- **Cost:** during a rescan two trees exist briefly. At 5M nodes that is about 400 MB extra; acceptable, noted.

## Testing

- `plumb-core`: unit tests for `add_child`, `set_size`, `remove`, `rename`, `reparent` (sizes and counts up the parent chain, dead nodes skipped by `children`, sums still equal a from-scratch rebuild after a random sequence of operations); `RowModel::refresh` anchoring tests (selection keeps its screen row, dies to its parent, scroll clamps). A property-style check: apply random deltas to a tree and compare it to a tree built fresh from the final state.
- `plumb-scan`: the journal reader against a fake volume that returns scripted journal records and MFT records: coalescing, rename, delete, record reuse, wrap, disabled journal, extension-record fallback.
- Elevated manual check, as for the MFT scanner: create, grow, rename, move and delete files and folders on a scratch NTFS volume or folder while plumb is open; compare against a fresh scan with `scan-compare`.
- Criterion: applying 100k deltas to a 5M-node tree; `refresh` on a tree with 1M-child directories.

## Out of scope

| Item | Why |
|---|---|
| Live updates on non-NTFS or unelevated volumes (`ReadDirectoryChangesW`) | Different mechanism and lower fidelity; after this |
| Pool compaction / slot reuse | The rescan compacts; add only if measured memory growth matters |
| Persisting the tree and replaying the journal on the next launch | Still a fresh scan on each launch |
| Highlighting changed rows | Not requested; easy to add later |

## To verify in the first spike (not assumed)

- The units and semantics of the `Timeout` and `BytesToWaitFor` fields in `READ_USN_JOURNAL_DATA`.
- That `FSCTL_GET_NTFS_FILE_RECORD` on the volume handle returns what the parser expects, including after fixups, and how often files have extension records on a real system drive.
- Journal record rate on a busy `C:` (how many records per second during idle and during a build), to confirm the 250 ms coalescing window and 4 Hz repaint cap are sensible.
- That a directory rename or move produces events for the directory only, not for each descendant. `reparent` relies on this; if descendants also get events, they are harmless repeats (same parent, same size).

## Plans

1. **Done** (plan `2026-10-12-plumb-live-updates-a.md`): `plumb-core` mutation (`DEAD`, `add_child`, `set_size`, `remove`, `rename`, `reparent`) and `RowModel::refresh` with anchoring; tests and benchmarks; no Win32 needed.
2. **Done** (plan `2026-10-12-plumb-live-updates-b.md`): journal reader in `plumb-scan` with the fake volume; the spike and an elevated end-to-end check ran on 2026-10-12 (results below). Plan 3 consumes `JournalReader`, `Delta`, `FileState` and `MftReport::records`.
3. **Done** (plan `2026-10-12-plumb-live-updates-c.md`, 2026-10-12): UI wiring: record table, delta application with budget, live marker, rescan with state restoration, stale banner and Rescan button.

## Journal reader as built, and spike results (2026-10-12)

Where the built reader differs from the "Journal reader" section above:
- **State-based.** The journal only says "record N changed"; each changed record is re-read and reported as it is now (`Upsert(FileState)` or `Remove`). A reused record number is just an `Upsert` with the new state, so there is no sequence-number tracking and no separate delete-plus-create case. Directory moves and repeated renames need no special handling either.
- **Reason mask** is data extend/overwrite/truncation, file create/delete, rename-new-name, reparse change and hard-link change. `CLOSE` and `BASIC_INFO_CHANGE` are not asked for: neither changes a name, place, size or kind. The 60 s spike saw only reasons inside the mask's useful set.
- **Record table** comes from the MFT scan (`MftReport::records`: record number to the node of that record's first name). Other hard links are not tracked.
- **Source trait:** `read(from, journal_id, wait)` either waits up to about a second or returns at once. The reader waits out the 250 ms window itself, flushes early at 50 000 pending files, and does not busy-loop if a blocking read returns at once with nothing.
- **Unreadable or unparseable MFT records** are counted and skipped; only a failed journal read ends the reader, with `Wrapped`, `Disabled` or `Failed`.

Answers to "To verify in the first spike" (elevated, `C:`, Ryzen 7840U laptop, normal desktop use):
- **`FSCTL_GET_NTFS_FILE_RECORD`** returns the record with the update-sequence fixups already undone (the first run failed every record with `BadFixup` until the parser skipped them). After the fix: 563 live, 1 extension, 0 unparseable out of 872 distinct files seen. A record number beyond the MFT returns record 0 (the nearest in-use record below), which the reader treats as "not this record", i.e. a `Remove`.
- **Extension records:** 4 of 563 live files (0.7%) had their data stream outside the base record and were sized by `OpenFileById`; 1 record was an extension record. Cheap enough not to cache.
- **Journal rates:** 22 to 96 entries/s over two 60 s runs, 152 to 872 distinct files per minute, busiest 250 ms window 245 files. The 250 ms window and the 50 000 cap are comfortable. In the second run 308 of the changed records were no longer in use by the time they were read (short-lived files), which become `Remove` for nodes plan 3 does not know, and are ignored.
- **Directory moves:** `journal-check` moved a file between folders and got one `Upsert` with the new parent; the end-to-end check (create, grow to 3 MiB, rename, move, delete file and folder) passes, with each delta arriving about 260 ms after the change. Not separately checked: whether moving a directory with descendants emits events for them (harmless if it does: same state).
- **Stale journal ID** returns `ERROR_INVALID_PARAMETER`, mapped to `Wrapped`. `ERROR_JOURNAL_ENTRY_DELETED` for an overwritten start is mapped to `Wrapped` as well; reading from USN 0 on this drive returned entries (the journal has not wrapped past its first entry here).
- **`Timeout` / `BytesToWaitFor` units: still unverified.** Both runs happened on a busy `C:`, so the blocking read always returned early (2 to 93 ms) because new records arrived. The code passes 1 for each; if that is not about a second, the reader's spin guard slows an idle loop to one read per 100 ms instead of spinning. A quiet-volume run (for example a second NTFS drive with nothing happening) would settle it; parked.

## Plan 3 as built (2026-10-12)

- **Applying:** `live::Live` applies deltas in 5 ms slices (`WM_JOURNAL` re-posts itself while the queue is not empty); one on-demand 250 ms timer refreshes the rows once if the tree changed, and backs off to `max(250 ms, 5 x refresh time)` after a slow refresh. The timer is off when idle.
- **Parking:** `settle` retries parked upserts to a fixpoint, then a second failed pass puts them under `<orphans>`. A move that `reparent` refuses (two folders swapping places) is parked, not dropped.
- **Compaction:** the 25% rule also needs at least 10 000 dead nodes.
- **Rescan:** `RescanPolicy` (10 min per drive, 30 s after input); the journal is opened in the scan thread before the scan; `swap_tree` restores sort, on-screen expanded folders, selection and screen row by path.
- **Banner:** `LiveStatus::Stale(reason)` shows "live updates stopped: <reason>; the tree may be out of date" with a Rescan button; a refused automatic rescan after a wrapped journal falls back to it.
- **Checks:** `plumb --selftest-live` (administrator; output needs a pipe because the release build is a GUI-subsystem exe) passed on `C:`: create, grow, rename and delete followed. A manual look in the real window (elevated, `--scan C`) worked. Not exercised: the journal-deleted banner path and a wrapped-journal rescan on a real volume.
