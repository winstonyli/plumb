# plumb

A fast disk-usage explorer for Windows, with a live view of running processes.

- **Disk view:** a sortable tree of folders and files beside a treemap. Tiles are coloured by file type and fade with the file's last-write age. A name filter (with a regex toggle) narrows both.
- **Fast scans:** on an NTFS volume, run elevated and plumb reads the master file table directly. Otherwise it walks the directories. A partial tree shows while a scan runs.
- **Stays current:** elevated NTFS drives follow the change journal; other drives use a directory watcher. New, grown, renamed, moved and deleted files appear without a rescan.
- **Delete from the tree or the map:** mark items with `Del`, then send them to the Recycle Bin or delete them permanently, each after a confirmation. If a delete fails, plumb names the program that holds the file ("Who is using this" is also in the context menu).
- **Select several rows:** Shift+click, Ctrl+click, Shift+arrows, Ctrl+A, or drag over rows; `Del`, Ctrl+C and the context menu then act on all of them. F5 rescans, Enter opens or closes a folder, Backspace goes to the parent, and Shift+F10 or the Apps key opens the menu of the focused row.
- **A score column first:** the first column after Name is Stale (Disk) or Load (Processes), and the view opens sorted by it, so what is worth your attention comes first. Hover the header for its definition; click it to sort.
- **Processes view:** every running process as a tree by parent, with memory, private bytes, CPU and I/O, the same map, a pane of the files the selected process has open, and End process, End process tree (after a confirmation) and Suspend or Resume for a process or its whole tree in the context menu.

## Install

With [Scoop](https://scoop.sh):

```powershell
scoop bucket add plumb https://github.com/winstonyli/plumb
scoop install plumb
```

Or download `plumb-<version>-windows-x64.zip` from the [releases page](https://github.com/winstonyli/plumb/releases) and check it against the `.sha256` file next to it. The exe is unsigned, so Windows SmartScreen may warn on first run.

## Build

```powershell
cargo build --release -p plumb
```

The exe is `target\release\plumb.exe`. Release steps are in [docs/releasing.md](docs/releasing.md); the changelog is [CHANGELOG.md](CHANGELOG.md).

## License

MIT OR Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
