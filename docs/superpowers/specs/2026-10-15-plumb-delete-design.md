# plumb: deleting from the tree and the map (stage, then commit)

Status: design, 2026-10-15 (not implemented). Reverses the "Delete and cleanup suggestions: read-only tool by decision" row of `2026-10-11-plumb-disk-design.md`; that row is superseded for deletion only (cleanup suggestions stay out).

## Goal

Free space from inside plumb: mark files and folders in the tree or on the map, see how much that frees, then delete them in one operation, to the Recycle Bin or permanently.

## Behaviour

- **Mark.** `Del` toggles the mark on the selected row or tile; the context menu (tree row and map tile alike) gets "Mark for deletion" / "Unmark". Selection stays single; the basket is how several things are deleted at once. `Del` is ignored while the filter box has focus (it edits text there).
- **Show.** Marked rows get a red-tinted background and a dim name; marked tiles get a red tint and frame over the tile (drawn with the selection outline, over the cached tile bitmap, so a mark never rebuilds the map). A status-bar chip left of the map button reads "3 marked, up to 12.4 GB" and appears only while something is marked. The legend gives way to it, as it does for the cancel button.
- **Per drive.** The basket belongs to a drive, like the per-drive tree, scan and watch slots: switching drives shows that drive's marks and chip, and a commit covers one drive's basket. Marks survive switching away and back, and a rescan of the drive.
- **Count.** Bytes are the allocated sizes of the *topmost* marked nodes only (a node under a marked ancestor adds nothing). Hard-linked files are counted as the tree counts them, so the figure is an upper bound ("up to").
- **Commit.** Clicking the chip opens a dialog (a `TaskDialog`) that lists the marked paths (the first 20, then "and N more") with the total, and the buttons **Recycle**, **Delete permanently**, **Unmark all**, **Cancel**. "Delete permanently" asks once more, naming the count and size. Nothing on disk changes before a button that deletes is confirmed.
- **Run.** One `IFileOperation` batch on a worker thread (its own STA), owner window set to plumb, so the shell shows one progress window, handles UAC, and groups a recycle as one undo step in Explorer. Recycle uses `FOF_ALLOWUNDO` and `FOFX_RECYCLEONDELETE`; permanent delete uses neither; the shell's own prompt for "too large for the Recycle Bin" or "no Recycle Bin on this drive" is left on, so a recycle should never silently become permanent (spike 2 confirms the flag does this; if not, the dialog warns before Recycle on drives without a bin).
- **Result.** A progress sink reports each item. Deleted paths leave the basket; failed or cancelled ones stay marked, and a failure shows one summary line in the status bar ("2 of 5 not deleted: access denied; still marked") with the details in the hover text. The tree and map follow through the existing live updates and directory sync; no code removes nodes by hand.
- **Unmark all** is in the dialog; there is no Esc shortcut (Esc already belongs to the filter and dialogs).

## Guards

- Not markable: a drive root (a node with no parent), `%SystemRoot%` and anything under it, and anything while its drive's scan is still running (sizes and paths are partial). The context-menu entry is disabled with the reason as hover text; `Del` shows the same reason in the status bar.
- Reparse points: marking one deletes the link, never its target (the tree does not follow them, and `IFileOperation` removes the link itself).
- Commit re-validates every path: it must still exist, still not be guarded, and its drive must not be scanning; ones that fail are listed as "missing" and skipped, not errors.
- Running elevated lets the delete touch protected items; the `%SystemRoot%` guard is the only one that does not depend on privilege.

## Design

- `plumb-core::basket` (pure, unit-tested): `Basket { paths: Vec<String> }` (full paths as `Tree::path` gives them, compared case-insensitively), `toggle`, `contains`, `clear`, `remove(&[String])`, `covered(&Tree) -> Vec<NodeId>` (topmost nodes that still resolve) and `total_bytes(&Tree)`. Paths, not node ids, so the basket survives live updates and a drive switch. Resolution walks from the root by name (the basket is small).
- `plumb::app`: `marked: HashSet<NodeId>` for the shown drive, rebuilt from its basket whenever its tree changes (live update, scan end); the row and tile painters read it. Guard checks live in `basket::refusal(&Tree, NodeId, system_root) -> Option<&'static str>` so tests cover them.
- `plumb::trash` (new, Windows only): `delete(hwnd, paths, permanent) -> Vec<Outcome>` running the batch on a worker thread; the window gets one `WM_APP` message with the outcomes, and `window.rs` applies them (unmark the deleted, set the status line).
- `layout`: `basket_chip()` rectangle between the cancel button and the map button, with the same hit-test and layout-test treatment as the map button.
- Features: `Win32_System_Com` and the `Win32_UI_Shell` items for `IFileOperation`, `IFileOperationProgressSink`, `SHCreateItemFromParsingName`, `TaskDialogIndirect` (needs the ComCtl32 v6 manifest; checked in the first spike below).

## Testing

- `basket` unit tests: toggle and case-insensitive match, dedupe of nested marks, a path that no longer resolves is dropped, `total_bytes` counts a marked folder once, `refusal` for a root, the system root, a child of it, and a scanning drive.
- `layout` test: the chip sits between the cancel and map buttons and never overlaps them.
- `render` test: a marked tile differs from the unmarked frame and the cached tiles are not rebuilt by a mark (`map_builds` unchanged).
- `--selftest` gains a block: scan a temp directory, mark a file and a folder, commit *permanently* through `trash::delete` (the recycle path is not run in a selftest: it would leave items in the user's Recycle Bin), check the paths are gone, the outcomes are all `Ok`, and a second commit of the same paths reports them missing, not failed.
- Manual: recycle then Explorer's Undo; a locked file stays marked with its reason; cancel in the shell's progress window; an unmarkable item's reason; 100% and 200% DPI for the chip.

## Spikes before the plan

1. `TaskDialogIndirect` with command links in this exe: does it get ComCtl32 v6 (an embedded manifest) and does an expanded list render? If not, the fallback is a plain message box listing the first few paths.
2. `IFileOperation` on a worker STA with plumb's window as owner: progress window appears over plumb, a recycle shows in the Recycle Bin and undoes in Explorer, and a locked file yields a per-item error through the sink.

## Risks and open questions

- A destructive tool in what was a read-only one: the README and the disk design's non-goals line change with this.
- Directory sync lag: a delete on a folder the watcher does not cover would leave rows until the next sync; the selftest and the manual check decide whether an explicit sync of each deleted item's parent is needed (it would be one call per parent, not a second removal path).
- Folder sizes after a partial failure are corrected by the same sync.
- Not in scope, parked: multi-select, drag to the basket, a dockable basket list (the dialog's list is read-only), cleanup suggestions from age and size, undo inside plumb (Explorer's undo covers a recycle).
