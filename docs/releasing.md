# Releasing

A release is a `v*` tag. `.github/workflows/release.yml` runs on the tag: `cargo test --workspace --locked`, a release build, then a GitHub Release with `plumb-<tag>-windows-x64.zip` (plumb.exe + CHANGELOG.md), its `.sha256`, and the CHANGELOG section for that version as notes. A tag containing `-` (e.g. `v0.8.0-rc1`) is published as a prerelease. The whole run takes about 9 minutes.

1. Bump `version` in the three crates (`plumb-core`, `plumb-scan`, `plumb`) and run `cargo check` to update `Cargo.lock`.
2. In `CHANGELOG.md`, rename `[Unreleased]` to `[x.y.z] - date` and add a fresh empty `[Unreleased]`.
3. Commit, push `main`, then `git tag vx.y.z && git push origin vx.y.z`.

To test workflow changes, push a throwaway `-rcN` tag, then delete the release and tag (`gh release delete <tag> --cleanup-tag -y`). A failed run leaves its tag behind; use a new `-rcN` rather than moving it.

The exe is unsigned (SmartScreen may warn); the notes say so.

Gotcha: in the Publish step, `@pre` must be a real array. `$x = if (...) { "--flag" }` yields a string, and splatting it passes one argument per character; wrap it as `@(if ...)`.
