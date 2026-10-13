# Shell icons in the name column

Rows show Explorer's icon for the node: one shared folder icon, and for files the icon of the extension.

- **Source.** `SHGetFileInfoW("x.<ext>", NORMAL|DIRECTORY, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES | SMALL/LARGE)`. With `USEFILEATTRIBUTES` the shell resolves the type from the registry and never touches the disk, so a scan tree of millions of nodes costs one lookup per distinct extension. Per-file icons (exe resources, thumbnails, `.lnk` targets, cloud overlays) are out of scope: they need the real path and disk I/O per row.
- **Key.** Lowercased extension after the last dot (empty for none or a leading-dot name); all folders share one key. `icons::key`.
- **Pixels.** `HICON` -> `GetIconInfo` -> `GetDIBits` (32-bit top-down BGRA). Icons without an alpha channel get alpha from the AND mask. Then premultiplied, because Direct2D bitmaps are premultiplied. `icons::load`.
- **Cache.** `Painter.icons`, keyed by (key, folder, large), holding `Option<ID2D1Bitmap1>` (a miss is remembered). Direct2D bitmaps belong to the device, so the cache is rebuilt with the painter on device loss. Cleared whole at 512 entries.
- **Size.** Drawn into a 16-DIP square; 16 px source below 144 dpi, 32 px at or above, cubic-scaled.
- **Fallback.** When the shell returns nothing, the previous drawn folder/page is used.
- **Threading/blocking.** Lookups run on the UI thread during paint, synchronously. The first sight of an extension costs a registry lookup (sub-millisecond when warm); not budgeted. Revisit only if a cold-cache paint is seen to hitch.
- **Not verified live:** appearance with real file types in a running window (the render test tree has folders only); the unit test checks folder, `txt`, and an unknown extension return a visible icon at both sizes.
