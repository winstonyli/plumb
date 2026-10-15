# plumb: treemap pane design

Status: implemented, 2026-10-14 (unreleased; the real window has not been looked at yet). Extends `2026-10-11-plumb-disk-design.md` (the treemap was parked there).

## Decisions (from the brainstorm)

| Question | Decision |
|---|---|
| Where | A split pane beside the tree, sharing selection. |
| What it shows | The selected folder's subtree. Selecting a file shows its parent folder with the file highlighted. |
| Colour | By file type (extension category). Folders are outlines with a faint tint. Modified age shades the hue: designed in `2026-10-15-plumb-map-age-design.md` (not yet built). |
| Mouse | Click selects (and scrolls the tree row into view) without changing the map's root. Double-click on a folder roots the map there, one level down. Hover shows a tooltip with the path and size. Right-click opens the tree's menu. No breadcrumb. |
| Technique | Squarified layout recomputed on change, drawn with Direct2D. Not the D3D11 path. |

## Scope

In: layout, colour classes and hit-testing as pure code in `plumb-core`; the pane, divider, painting, tooltip and the toggle in `plumb`; saved pane width and visibility in `state.toml`.

Out (parked): cushion shading, a breadcrumb, a depth cap setting, the sunburst, highlighting filter matches on the map, a layout cache across folders, GPU rendering.

## Behaviour

- **Root of the map.** The map's root is app state, `map_root`. It is set to `map_root_for(selected)` (the selected node if it is a folder, else its parent; no selection means the tree's root) whenever the selection changes **from the tree** (mouse, keyboard, filter), and by a double-click on a folder tile (which selects that folder and roots the map there). A single click **on the map** only moves the selection and its outline: the root stays, or a click on a folder tile would drill at once and a double-click would mean nothing. To go up, select an ancestor in the tree. A root with no counted bytes (empty, or all hard-link duplicates) shows an empty pane, not an error.
- **What is drawn.** Every descendant file with `size > 0`, as a tile whose area is proportional to its `size` (allocated bytes). Folders are drawn as an outlined frame, inside which their children are laid out; a folder's weight is its `subtree_size` (directory nodes themselves have size 0). Within one folder, sibling areas are proportional to their weights; the 1 px frame inset means areas are not proportional across nesting levels.
- **Ignores the filter.** The map always shows the real tree; a name filter changes the tree rows only. (Highlighting matches on the map is parked.)
- **Reparse points** (junctions, symlinks) have no children and size 0: not drawn. Denied folders show whatever was counted.
- **Live changes and scans.** The map is marked dirty wherever `App` applies tree changes (preview, scan done, journal and directory-watch updates, all of which already end in a row refresh) and when the root, size or theme changes. It is rebuilt at the same pacing as the row refresh (`Pacer`), so a running scan or a burst of watch events does not re-lay-out every frame. (`Tree::take_touched` is consumed by the row model, so the map cannot use it.)
- **Selection sync.** A click on the map selects the tile's node (not for `Rest`, which has none), which expands its ancestors in the tree and scrolls the row into view. That needs a new `RowModel::reveal(tree, id)`: nothing expands ancestors on selection today. The selected node is outlined on the map (a node inside a `Rest` tile has no outline). Right-click on a tile selects it and opens the tree's menu; on `Rest` it does nothing. The selection outline is drawn over the map, not baked into it.
- **Toggle.** `Ctrl+M` and a button in the status bar's right edge show or hide the pane. The pane is shown only when the window is wide enough for the tree's minimum plus the divider plus the map's 160 px minimum; below that it is hidden without changing the saved flag. Visibility and the divider position are saved.
- **Empty states.** Before any scan: the pane is blank with the panel colour. During a scan: the map follows the preview tree.

## Layout (`plumb-core::treemap`)

`layout(tree, root, rect, opts) -> Vec<Tile>`, pure.

```
Tile { node: NodeId, rect: Rect (f32), depth: u8, class: Class, kind: File | Frame | Solid | Rest, bytes: u64, count: u32 }
// Frame: a folder with its children inside; Solid: a folder too small to enter. For Rest, `node` is the folder it belongs to; `bytes` and `count` describe the merged items.
```

- **Algorithm:** squarified (Bruls, Huizing, van Wijk). Children are ordered by `subtree_size` descending, ties by `NodeId`, so the layout is deterministic.
- **Bounded tile count.** A child whose area would be under `MIN_AREA` (9 physical px², roughly 3x3; the caller passes it in device-independent units for the current DPI) is not laid out. All such children of one folder are merged into one `Rest` tile (a neutral fill, no node; hovering gives the count and total). `Rest` takes part in the squarify as one last, smallest child, so the folder is still fully tiled. The number of tiles is therefore bounded by the pane's pixel area; the traversal is bounded by the children of the folders actually visited (a visited folder of a million files costs one O(k) pass).
- **No full sort of huge folders.** The cutoff is a size threshold, `parent_bytes * MIN_AREA / inner_area`, so one O(k) pass over a folder's children keeps those at or above it and sums the rest into `Rest`; only the kept children are sorted.
- **Nesting.** Folders nest as far as their tiles stay above `MIN_AREA` after the 1 px inset on each side, with a hard cap of 24 levels (stack guard; the tree can be deeper). A folder tile whose inner area is under 64 physical px² is not entered: it is a `Solid` tile in the neutral folder colour.
- **Invariants (tested):** every tile lies inside its parent; tiles of siblings do not overlap; the child tiles (including `Rest`) of a folder sum to its inner area (within one pixel of rounding); sibling areas are proportional to their weights within rounding; no zero-size file appears; layout of an unchanged tree is bit-identical.
- **Hit test:** `hit(&[Tile], x, y) -> Option<usize>` returns the deepest tile containing the point (tiles are stored parent before child, so a reverse scan finds it). It is O(tiles): at most about pane_area / 9 (around 100k worst case), under 0.2 ms; a grid index is the fallback if it ever measures slower.

## Colour (`plumb-core::treemap::Class`)

`classify(name: &[u16]) -> Class` by the last extension, case-insensitive, from a static table:
Video, Audio, Image, Archive, Document, Code, Executable (exe, dll, sys, msi), DiskImage (vhd, vhdx, iso, vmdk, wim), Database and log, Build output (pdb, rlib, rmeta, o, obj, lib, a, pyc, pyd, node), Other. Generic game-package formats (wad, ucas, pck, asar, bundle) count as Archive, dds as Image. No extension is Other.

`plumb` maps each class to a colour from the theme (`Theme` tokens in light and dark), chosen to stay distinguishable from each other in both. The legend sits in the status bar, right-aligned left of the buttons, and lists the classes that occur in the current view (short labels), each with its share of the bytes of the file tiles drawn ("Video 64%", "<1%" below one percent), largest first, as many as fit in 60% of the bar's width, so the smallest drop off first; the status text takes the rest. Because the bar must stay short, it shows the selection's size and items without its path, and "live" without the time of the last change; a tooltip over the bar carries the whole line and the graphics adapter name.

## Rendering (`plumb`)

- Painted by the existing Direct2D painter into the pane rectangle, clipped. Filled rectangles with a 1 px darker edge on file tiles, a 1 px frame on `Frame` tiles.
- **Redraw cost.** The app repaints the whole scene on any input, including hover, so drawing the tiles directly costs up to 32 ms per mouse move at 4K (measured, see Performance). The tiles are therefore drawn into an offscreen `ID2D1Bitmap1` when the layout, size, scale or palette changes, and each frame blits it and draws only the selection and hover outlines and the legend on top. Selection and hover are never baked into the bitmap.
- **Tooltip.** `App::tip_at` already returns a string for the control under the pointer and the window compares it to show or pop the tip; the map adds the tile under the pointer to it: full path and size, or "N smaller items, SIZE" for `Rest`.
- **Cursor and focus.** The map takes the mouse only; keyboard focus stays where it was. Arrow keys still move the tree selection, which moves the map.

## Window layout (`plumb::layout`)

`Layout` gains `map: Option<Rect>` and `divider_x`. The tree's columns, header and rows are laid out in the left part; the status bar and picker stay full width. Dragging the divider (a hit region of a few pixels, a resize cursor, like the column dividers) sets the saved width; clamped so the tree keeps its minimum and the map keeps at least 160 px.

## State

`state.toml` gains `[map] visible = true`, `width = 420.0` (both optional, `#[serde(default)]`); damaged or out-of-range values mean the defaults.

## Testing

- `plumb-core`: the invariants above on hand-built trees and on `synth` trees; ties, a single huge file, a folder of one million equal files (cutoff merges them, work stays small), zero-size trees, depth beyond the cap, classification table (case, no extension, double extension, trailing dot).
- `plumb`: `App` tests for root choice, click-to-select with tree sync, double-click drilling, the toggle and divider clamping; a render test (WARP) that a laid-out small tree paints tiles of the expected classes at known pixels.
- Criterion: `layout` on a synthetic 5M-node tree for the root and for a mid-size folder, at 800x600 and 3840x2160; `hit` over a 100k-tile result. Run at CPU load under about 20%.
- Manual: dark and light, a 4K and a small window, live changes while the map is visible, a scan in progress.

## Performance (measured 2026-10-14)

Release build, a synthetic 5M-node 8-ary tree for layout and a 3M-node one for paint, machine load 38-59% (above the 20% the plan asks for, so read the numbers as upper bounds), Defender real-time protection off, AMD Radeon 780M (the system default adapter; the eGPU was not selected).

| What | Target | Measured |
|---|---|---|
| Layout, root of 5M nodes, 3840x2160 | under 30 ms | 20.7 ms (20-22) |
| Layout, root, 800x600 | | 0.62 ms |
| Layout, a mid-size folder (node 9), 3840x2160 | under 5 ms typical | 30 ms (it holds 1/8 of the tree: not typical) |
| Hit test over 181k tiles | under 0.2 ms | 23 us |
| Direct paint of the tiles, 1280x800 at 100% (5.9k tiles) | under 4 ms | 4.4 ms |
| Direct paint of the tiles, 3840x2160 (47k tiles) | under 4 ms | 32 ms (WARP: 81 ms) |
| Cached bitmap blit plus overlays, same two cases | under 4 ms | 0.35 ms and 0.40 ms (WARP: 0.4 and 1.05 ms) |

Direct painting missed the target, so the tiles are drawn into an offscreen bitmap once per layout (a generation counter on `MapView`, the pane size, the scale and the palette are its key) and each frame blits it and draws only the outlines and legend. A layout change (selection, resize, divider drag, live change) still pays the direct cost once, so dragging the divider at 4K runs at roughly 25-30 frames a second. Idle cost: none.

## Open questions

- Is 9 px² the right floor, or should tiles under about 4 px on a side merge sooner for legibility? Decide on screen at 100% and 200% DPI.
- Is full nesting (up to the cap) readable, or should the default stop at two or three levels? The cap is one constant; the brainstorm's wording was "one or two levels", so decide by looking at a real drive.
- Does selecting through the filter (rows only partly visible) confuse the map? The map ignores the filter by decision; revisit if it does.

## Parked

- A cheaper rebuild during a divider drag (the tiles are redrawn on every move; about 25-30 frames a second at 4K).
- Per-folder cached sorted children, if the layout of a very large folder ever matters (a node holding 1/8 of 5M nodes takes 30 ms at 4K).

## Milestones

1. `plumb-core::treemap`: layout, classify, hit, with tests and the benchmark.
2. Pane, divider, painting, tooltip, selection sync, toggle and state in `plumb`.
3. Measure; add the cached bitmap only if the numbers ask for it.
