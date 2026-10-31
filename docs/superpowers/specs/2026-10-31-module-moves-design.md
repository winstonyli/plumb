---
status: done
---

# Module moves out of `app.rs` and `window.rs` (cleanup item A9)

Pure moves, no behaviour or interface change, done before the per-view `Slot` refactor (A1 in
`docs/superpowers/plans/2026-10-31-cleanup.md`) so that diff touches smaller files. `app.rs` is 3,177 lines (one
`impl App` of about 1,250) and `window.rs` 2,055 (one `impl Win` of about 800). Line numbers are at `a4d494b`.

## Mechanism

A child module may use its parent's private items, so an `impl App { .. }` block in `app/held.rs` reads and writes
`App`'s private fields with no visibility change. Each move is: cut the methods and the types only they use into a
child module, add `mod x;` and a `use` for any type the parent still names. Tests stay in the parent's `mod tests`
(`use super::*` keeps working through the `use`s). Each move is its own commit, checked by `cargo test --workspace`
and clippy; the `--selftest*` runs cover `window.rs` and run once at the end.

## Moves

`app.rs` (all `impl App` methods unless noted):

| New module | Takes | Lines |
|---|---|---|
| `app/held.rs` | `HeldState`, `Held`, `HELD_REFRESH_SNAPSHOTS`, `held_*` methods (`held_sync` .. `held_text`) | 116-147, 843-932 |
| `app/marks.rs` | `MarkState`, `CommitRequest`, `BasketView`; `basket` .. `finish_delete` (marks, `refresh_basket`, delete commit) | 330-353, 1245-1415 |
| `app/map.rs` | `map_reset`, `select_from_map`, `live_map_root`, `update_map`, `update_tiles`, `map_tiles`, `map_selected_tile` | 1181-1244, 1416-1425 |

`window.rs`:

| New module | Takes | Lines |
|---|---|---|
| `window/tooltip.rs` | `TIP_FRAME`, `make_tooltip`, `refresh_tip`, `tip_text` (the `tooltip`/`tip*` fields stay on `Win`) | 77-78, 465-517 |
| `window/watch.rs` | `Source`, `Scanned`, `ScanResult`, `Watch`, `Ended`, `open_journal`, `local_time`; the `Win` methods `start_watch` .. `auto_rescan` | 110-226, 1085-1243 |
| `window/menu.rs` | `CMD_*`, `context_menu`, `reveal`, `properties`, `copy_to_clipboard` | 104-108, 1265-1383 |

Expected result: `app.rs` about 2,550 lines, `window.rs` about 1,400. Nothing is renamed and no signature changes.

## Not moved

The `swap_slot` / `swap_window_fields` / `in_slot` lists and the filter fields stay put: A1 replaces them. `handle`
and `click` stay (each is one dispatcher over `Input`; their size is A2/A5's question).

## Check

Method bodies byte-identical before and after (a `git diff --color-moved=zebra` review per commit), 412 tests,
clippy clean, then `--selftest`, `--selftest-watch`, `--selftest-delete`, `--selftest-tooltip`, `--selftest-live`.

## Result (2026-10-31)

Done in `a000258` (`app`) and the commit after it (`window`). `app/basket.rs` is `app/marks.rs` because `basket` clashes with the imported `plumb_core::basket`. `app.rs` is 2,781 lines (estimate 2,550), `window.rs` 1,635 (estimate 1,400): the estimates counted the methods but not the doc comments and blank lines the moves also carry less than predicted. Bodies are unchanged (every removed line reappears, checked by a sorted line diff); types and methods the parent still names became `pub(super)`. 412 tests, clippy clean; `--selftest`, `-watch`, `-delete` and `-tooltip` pass. Not run: `--selftest-live`, which needs an elevated process; the code it covers moved byte-for-byte.
