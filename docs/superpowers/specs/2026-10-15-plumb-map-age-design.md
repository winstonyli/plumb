# plumb: modified age on the treemap

Status: design, 2026-10-15 (not implemented). Extends `2026-10-14-plumb-treemap-design.md`, where colour by modified age was parked.

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
- `TreeBuilder` takes it per node; live updates set it through the same path that sets a size.
- `Tile` (treemap) carries the file's `mtime`. `Rest` and folder tiles carry `None`.

## Scanners (`plumb-scan`, `plumb-core::mft`)

- Walker: `FileIdExtdDirectoryInfo` already returns `LastWriteTime`; it goes into `Entry.mtime`, no extra syscall.
- MFT: `mft::parse_record` already walks attributes (`ATTR_FILE_NAME`, `ATTR_DATA`). Add `$STANDARD_INFORMATION` (type 0x10, always resident, first attribute): the last-write time is at value offset 0x08 (0x00 created, 0x08 modified, 0x10 MFT-changed, 0x18 accessed). `ParsedRecord.mtime` carries it; extension records do not repeat it.
- Preview (`plumb::preview`, `plumb-scan::preview`): `FileState.mtime` is applied with the rest of the state.
- Live:
  - The directory watch re-lists, so `Entry.mtime` arrives with it.
  - The USN path opens a changed record by id and calls `FileStandardInfo` today. It adds `FileBasicInfo` (one more syscall per changed file). To measure: that cost under churn. If it matters, a write event's time can be taken as "now" and only renames re-read it.

## Rendering (`plumb`)

- Shade: `t = clamp(ln(age_days) / ln(AGE_FULL_DAYS), 0, 1)` with `AGE_FULL_DAYS` about 3650 (ten years). The tile colour is the class colour with saturation scaled by `1 - 0.45 t` and lightness by `1 - 0.25 t` (constants: `FADE_SAT`, `FADE_LIGHT`). Age under one day is `t = 0`. The floor keeps the oldest at 55% saturation, so hues stay distinct.
- "Now" is read when the tiles are rebuilt (once per layout generation). The bitmap cache key is unchanged, there is no timer redraw, and idle cost stays zero. Tiles age only when the map next redraws.
- Tooltip: adds `modified 2024-03-09 (2.1 y ago)`, or nothing for `None`.
- Legend: type entries unchanged. A small continuous age strip (bright to dim) with "new" and "old" end labels sits at the right edge. It is dropped before any type entry when space is short.

## Risks and open questions

- 11 hues times a continuous shade may blur at the dim end. Mitigation: the floor above and a render test that every pair of classes stays distinguishable at `t = 1`. If it is still muddy on a real drive, quantising to a few bands is one function.
- The USN `FileBasicInfo` cost (above).
- Whether "ten years" is the right top of the scale: a one-constant decision on a real drive.
- Memory: +4 bytes a node. `benchmarks.md` has the 5M-node baseline to re-measure against.

## Testing

- Core: the shade function is monotonic, holds its floor, and `None` gives the plain colour. The MFT parse reads a known `$STANDARD_INFORMATION` (synthetic record in `synth.rs`).
- Parity: the walker and the MFT scanner give the same day for the same file (`scan-compare` extended).
- Render (WARP): an old and a new tile of one class differ; all class pairs are distinguishable at `t = 1`; the age strip is drawn and dropped when narrow.
- Selftests: unchanged; live tests check an `mtime` moves after a write.

## Milestones

1. `Tree.mtime`, builder, `Tile.mtime`, shade function with tests.
2. Walker, then MFT, then preview and live paths, with parity checks.
3. Painter shading, tooltip, age strip; render tests.
4. Measure memory and USN cost; tune the constants on a real drive; CHANGELOG and spec status.
