# plumb: modified age on the treemap

Status: design, reviewed against the code in two rounds, 2026-10-15 (not implemented). Extends `2026-10-14-plumb-treemap-design.md`, where colour by modified age was parked.

## Decisions (from the brainstorm)

| Question | Decision |
|---|---|
| What age answers | Staleness: old big files stand out. |
| How it shows | File type stays the hue. Modified age shades it: newest = the class's full colour, older = a muted, darker version of the same hue. |
| Mode switch | None. Age shading is always on. |
| Ramp | Continuous, on a log scale of age, with a floor so the oldest files keep most of their hue. |
| Which time | Last-write (modified) time. Access time is unreliable on NTFS. |
| Unknown time | Plain class colour (not "oldest"), so unknown never reads as ancient. |
| Folders | Only files are coloured, so only a file's own time matters. No per-folder aggregate. A folder's own time is stored too and shown in its tooltip. |

## Data (`plumb-core`)

- `Tree.mtime: Vec<Option<NonZeroU32>>`: days since 1601-01-01 (FILETIME / 864e9). `None` is unknown. A FILETIME of 0 maps to `None`. The niche keeps it at 4 bytes a node (+4 on about 45, roughly 9%; 20 MB at 5M nodes).
- Stored for every node, directories included.
- Setter, not a wider constructor: `TreeBuilder::set_mtime(id, m)` and `Tree::set_mtime(id, m)` next to `add_flags`/`set_size`. `push`, `push_str` and `add_child` keep their signatures (about 30 files call them, mostly tests); a new node starts at `None`. `Tree::add_child` and `remove`/`reparent` need no mtime logic.
- `Tile` (treemap) carries the file's `mtime`. `Rest` and folder tiles carry `None`.

## Scanners (`plumb-scan`, `plumb-core::mft`)

- Walker: `FileIdExtdDirectoryInfo` already returns `LastWriteTime`; it goes into `Entry.mtime`, no extra syscall.
- MFT: `mft::parse_record` already walks attributes (`ATTR_FILE_NAME`, `ATTR_DATA`). Add `$STANDARD_INFORMATION` (type 0x10, always resident, first attribute): the last-write time is at value offset 0x08 (0x00 created, 0x08 modified, 0x10 MFT-changed, 0x18 accessed). `ParsedRecord.mtime` carries it; extension records do not repeat it.
- `MftMerger` keeps compact per-record arrays (`flags`, `alloc`, `names`) sized to the MFT, so it gains `mtime: Vec<u32>` (+4 bytes a record, freed with the merger). Every hard link of a record gets the record's time (the time belongs to the file, not the name).
- Preview (`plumb::preview`, `plumb-scan::preview`): `FileState.mtime` (new field, from the parsed record) is applied with the rest of the state, via `set_mtime` after `add_child`.
- Live:
  - The USN path builds `FileState` by parsing the file record fetched with `FSCTL_GET_NTFS_FILE_RECORD` (`journal/state.rs`), the same `parse_record`. The time therefore comes with that parse and costs no extra syscall (`FileStandardInfo` there is only for the delete-pending and allocation-size check).
  - The directory watch re-lists, so `Entry.mtime` arrives with it.
  - `live.rs` and `dirsync.rs` update a node only when its size differs. They also compare `mtime` and call `set_mtime` when it changed, counting a change; otherwise a rewrite that keeps the same allocation would keep its old shade until the next scan.
  - Scan swaps the whole tree, so a rescan needs nothing extra.

## Rendering (`plumb`)

- Shade: `t = clamp(ln(age_days) / ln(AGE_FULL_DAYS), 0, 1)` with `AGE_FULL_DAYS` about 3650 (ten years). The tile colour is the class colour mixed toward the map's background colour by `FADE_MAX * t` (`FADE_MAX` about 0.5, so the oldest tile keeps half its colour). Mixing toward the background is the one mechanism that reads as "dimmer" in both palettes (`Painter::new(dark)` has two) without HSL maths or per-theme branches. Age under one day is `t = 0`. The floor keeps hues distinct.
- "Now" is read when the tiles are rebuilt (once per layout generation). The bitmap cache key is unchanged, there is no timer redraw, and idle cost stays zero. Tiles age only when the map next redraws.
- Tooltip: adds `modified 2024-03-09 (2.1 y ago)`, or nothing for `None`.
- Legend: type entries unchanged. A small continuous age strip (bright to dim) with "new" and "old" end labels sits at the right edge. It is dropped before any type entry when space is short.

## Risks and open questions

- 11 hues times a continuous shade may blur at the dim end. Mitigation: `FADE_MAX` below 1 and a render test that every pair of classes stays distinguishable at `t = 1`. If it is still muddy on a real drive, quantising to a few bands is one function.
- Memory: +4 bytes a node in the tree, plus 4 a record in the merger at its scan-time peak (about 20 MB each at 5M). `benchmarks.md` has the 5M-node baseline to re-measure against.
- Whether "ten years" and `FADE_MAX` are right: two constants, decided on a real drive.
- Not in scope, parked: a Modified column in the tree rows, and sorting by it.

## Testing

- Core: the shade function is monotonic, holds its floor, and `None` gives the plain colour. The MFT parse reads a known `$STANDARD_INFORMATION` (synthetic record in `synth.rs`).
- Parity: the walker and the MFT scanner give the same day for the same file (`scan-compare` extended).
- Render (WARP): an old and a new tile of one class differ; all class pairs are distinguishable at `t = 1`; the age strip is drawn and dropped when narrow.
- Selftests: unchanged; live tests check an `mtime` moves after a write.

## Milestones

1. `Tree.mtime`, builder, `Tile.mtime`, shade function with tests.
2. Walker, then MFT, then preview and live paths, with parity checks.
3. Painter shading, tooltip, age strip; render tests.
4. Measure memory (tree and merger peak); tune the constants on a real drive; CHANGELOG and spec status.
