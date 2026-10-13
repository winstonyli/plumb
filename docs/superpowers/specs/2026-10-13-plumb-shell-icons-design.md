# Shell icons in the name column

Rows show Explorer's icon for the node: one shared folder icon, and for files the icon of the extension.

- **Source.** `SHGetFileInfoW("x.<ext>", NORMAL|DIRECTORY, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES | SMALL/LARGE)`. With `USEFILEATTRIBUTES` the shell resolves the type from the registry and never touches the disk, so a scan tree of millions of nodes costs one lookup per distinct extension. Per-file icons (exe resources, thumbnails, `.lnk` targets, cloud overlays) are out of scope: they need the real path and disk I/O per row.
- **Key.** Lowercased extension after the last dot (empty for none or a leading-dot name); all folders share one key. `icons::key`.
- **Pixels.** `HICON` -> `GetIconInfo` -> `GetDIBits` (32-bit top-down BGRA). Icons without an alpha channel get alpha from the AND mask. Then premultiplied, because Direct2D bitmaps are premultiplied. `icons::load`.
- **Cache.** `Painter.icons`, keyed by (key, folder, open, large). A failed lookup is not remembered, so a transient failure retries on the next paint. Direct2D bitmaps belong to the device, so the cache is rebuilt with the painter on device loss. Cleared whole at 512 entries.
- **Size.** Drawn into a 16-DIP square; 16 px source below 144 dpi, 32 px at or above, cubic-scaled.
- **Fallback.** When the shell returns nothing, the previous drawn folder/page is used.
- **Threading/blocking.** Lookups run on the UI thread during paint, synchronously. The first sight of an extension costs a registry lookup (sub-millisecond when warm); not budgeted. Revisit only if a cold-cache paint is seen to hitch.
- **Not verified live:** appearance with real file types in a running window (the render test tree has folders only); the unit test checks folder, `txt`, and an unknown extension return a visible icon at both sizes.

## Replacing the chevrons
The chevron column is gone. Expanded folders use the shell's open-folder icon (`SHGFI_OPENICON`). A folder with children gets a small triangle inlaid on its icon (right when closed, down when open, a fixed dark brown that reads on the shell's yellow in both themes); an empty folder has none. The shell has no empty/full folder icon, so this is drawn. Expansion is toggled by double-click or the keyboard as before; the chevron was never a separate click target. `INDENT` is 20 so a child's icon does not touch its parent's.
