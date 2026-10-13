# plumb: name filter design (milestone M3, part 1)

Status: draft for review, 2026-10-12. Builds on `2026-10-11-plumb-disk-design.md` (search and filter were parked "after v1") and on the live-updates design, whose tree mutation, `RowModel::refresh` and `restore` module this reuses.

## Goal

Type part of a name and see only the files and folders that match, with the folders on the way to them, without leaving the tree view. Sizes stay the real totals. The window never stalls on a 1M to 5M node tree, and the filter keeps working while live updates change the tree.

## Decisions

| Question | Decision |
|---|---|
| What a query does to the tree | Filters it: shows matches plus their ancestors (not "find next") |
| What matches | Name only (not the path), case-insensitive, in both modes |
| Pattern modes | Plain substring by default; a Regex toggle switches to a regular expression. Same compiled `regex::Regex` either way (substring text is escaped) |
| Regex engine | `regex` crate (linear time, no backtracking). Not `fancy-regex`: backtracking can hang on a 1M-name tree |
| Invalid regex | Keep the previous results on screen, tint the box, no popup |
| Which folders are open | Every ancestor of a match is open while filtering; the user's own expansion is kept aside and restored on clear |
| A folder whose own name matches | Shown collapsed; its non-matching children appear only if the user opens it, and then dimmed |
| Where matching runs | A worker thread; the UI shows the previous result until the new one lands |
| UI | An always-visible strip above the column headers: text box, Regex toggle, match count, clear button. Ctrl+F focuses it, Esc clears it and returns focus to the tree |

## Dependency

`regex` is in `Cargo.lock` only through `criterion` (a dev-dependency), so this adds it as a real dependency of `plumb-core`. Default features (Unicode case folding is needed for case-insensitive non-ASCII names). Build-time and binary-size cost are recorded in the plan. The crate's default compiled-size limit stays on, so a pattern that is too large to compile reports "pattern too complex" like any other invalid pattern.

## Matching (`plumb-core`, new module `filter`)

- `Query { text: String, regex: bool }`; `Query::compile() -> Result<Matcher, FilterError>`. Substring mode builds `(?i)` + `regex::escape(text)`; regex mode builds `(?i)` + text. An empty text compiles to "no filter".
- Names are UTF-16 in the tree (`Tree::name` returns `&[u16]`). The matcher converts each name to UTF-8 into one reused `String` buffer (lossy for unpaired surrogates) and calls `is_match`. This conversion, not the match, is the main per-name cost; the benchmark covers it.
- `Names` is the snapshot a worker can own: clones of `name_off`, `name_len` and `pool` (about 40 MB for 1M nodes), taken on the UI thread by `Tree::name_snapshot()`, plus the node count and a `generation: u64` the app bumps on every query change and tree swap.
- `Names::scan(&Matcher, cancel: &AtomicU64, generation) -> Option<Hits>` walks node ids `0..len`, sets a bit in `Hits` for each name that matches, and returns `None` if `cancel` no longer equals `generation` (checked every 64k names).
- `Hits` is a bitset over `NodeId` plus the node count it was computed for. Dead nodes are not excluded by the worker (it cannot see flags); the keep-set builder skips them.
- `Filter` (UI-thread state, in `plumb-core`): the active `Matcher`, the current `Hits`, and the **keep set** (a second bitset: every hit that is alive, plus all its ancestors). `Filter::rebuild_keep(&Tree)` walks up from each live hit and stops at the first node already kept, so the cost is O(hits + kept nodes), not O(hits x depth).

## Worker and threading (`plumb`)

- One worker thread per query run, spawned on each change of the query (after a 100 ms quiet period, so a burst of keystrokes starts one run). Starting a run bumps `generation`; an older run sees the mismatch and returns early.
- The worker posts `WM_FILTER` (a new `WM_APP` message, same posted-flag handshake as `WM_JOURNAL`) with a `Box<(generation, Hits)>`. The handler drops results whose generation is stale, otherwise installs them and refreshes the rows.
- While a run is in flight the strip shows "searching..." and the old rows stay. A tree swap (rescan) bumps the generation and restarts the run for the current query.
- Cost of the snapshot is paid on the UI thread per run. If the benchmark shows it matters (more than about 30 ms at 5M nodes), the fallback is sharing the pool as `Arc` chunks, which is a larger `Tree` change and is not part of this spec.

## Row model (`plumb-core`)

- `RowModel` stores an optional `Rc<Keep>` (the keep bitset plus the strict-ancestor set derived from it), set with `set_filter(tree, Some(keep))` / `set_filter(tree, None)`; every row-building method reads it, so expand, collapse and sort all respect the filter. `Filter` itself (matcher, hits) stays in `App`.
- While active, `push_children` skips any child not in the keep set. A node is shown expanded if it is a *strict ancestor of a hit* (its keep bit is set and it is not itself a leaf hit), tracked in a separate `filter_expanded` set so the user's `expanded` bitset is untouched. A user-opened hit folder uses the normal `expanded` bit; its non-kept children are included and marked `Row.dim = true`. Below a non-kept node everything is unfiltered and dim (opening a dim folder shows all its children, dim).
- If the selected node is not kept when the filter is installed or changes, the nearest kept ancestor is selected (the same rule `refresh` uses for removed nodes); if none, the selection is cleared.
- Turning the filter on records a `restore::Snapshot` (expansion, selection, scroll, by path); turning it off applies it. Selection that was changed while filtering is kept if the node still exists.
- `Row` gains `dim: bool`; `draw` draws dim rows at reduced text alpha.
- The sort cache keeps the unfiltered sorted children; filtering is a skip during `push_children`, so sort changes and size changes behave as before.

## Live updates

- `RowModel::refresh` consumes the tree's touched list, so the app first takes a copy (`Tree::touched()`, a peek) for the filter, then refreshes the rows.
- Every touched node is re-tested on the UI thread with the compiled `Matcher` (one name each): `Hits` grows to the new node count, dead nodes lose their bit. Deltas arrive in a bounded budget (5 ms), so this is bounded the same way.
- Renames, moves and removals can *remove* an ancestor's reason to be kept, and `touched` does not say which kind of change it was. Rule: if any touched node is in the keep set or became a hit, rebuild the keep set from the hits (O(hits + kept), about a millisecond per 100k kept nodes); otherwise do nothing. Writes to a kept file therefore cost one rebuild per 250 ms refresh. Optimised only if profiling asks.
- **Changes during a run.** The worker's snapshot goes stale while it runs. The app records every id touched since the snapshot; on install it re-tests those ids and every id at or beyond the snapshot's node count, so the installed result is current.

## UI (`plumb`)

- `Layout` gains a filter strip between the drive chips and the column headers: text box (flexible width), a Regex toggle, a match count ("1,240 matches", "searching...", "invalid pattern"), and a clear button. `Hit::{FilterBox, FilterRegex, FilterClear}`.
- New `App` state `FilterUi { text, caret, regex, focus }`. `WM_CHAR` feeds the text box only when it has focus. Supported editing: insert, Backspace, Delete, Left/Right, Home/End, Ctrl+A (select all, then typing replaces), Ctrl+V paste (single line, newlines stripped), Esc (clear and unfocus). No mouse selection, no undo; a click places the caret.
- Keyboard routing: while the box has focus, arrow keys, Home/End and Enter go to the box (Enter moves focus to the tree and selects the first row); Tab and Esc move focus. Esc in the tree with a filter active also clears the filter. Ctrl+C with no selection in the box still copies the selected row's path.
- The status line shows the filtered count and the unfiltered totals stay in the Size column.

## Tests

- `plumb-core` unit tests: substring escaping (`c++`, `file(1)`), regex mode, case-insensitivity including non-ASCII, empty query, invalid regex, UTF-16 surrogate pair names, `scan` cancellation, keep-set ancestor stopping, dead nodes excluded.
- `RowModel`: filtered rows equal a brute-force "walk the whole tree and keep matches and ancestors" oracle over random trees and queries; folder-match-collapsed; user expansion restored on clear; dim flag on a user-opened hit folder; sort change under a filter.
- Live: random add/rename/remove under an active filter equals a fresh filter on the final tree (the `refresh_matches_a_fresh_model` pattern).
- `plumb` tests: text-box editing (a pure `FilterUi` reducer), stale generation dropped, strip layout, WARP render test with the strip and an error tint.
- Benchmark (criterion): snapshot, scan and keep-set build at 1M and 5M nodes, substring and a realistic regex.

## Risks and open questions

- **Snapshot cost and memory** at 5M nodes (about 200 MB transient). Measured first; fallback described above.
- **Text input is the largest new UI piece.** IME composition (`WM_IME_*`) is out of scope; `WM_CHAR` covers direct input only. Known gap, recorded in the changelog.
- **Per-keystroke thread spawn:** one short-lived thread per run is simple; a long-lived worker with a channel is the alternative if spawn cost shows up.
- Match count counts hits (not kept ancestors).

## Out of scope

Path matching, regex lookaround (`fancy-regex`), an invert toggle, filtering by size or age, find-next/previous, saved filters, IME composition, mouse text selection.
