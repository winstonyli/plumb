# plumb

kind: app

- Version field: `version` in each of `crates/plumb/Cargo.toml`, `crates/plumb-core/Cargo.toml` and `crates/plumb-scan/Cargo.toml` (one shared version; bump all three and `Cargo.lock` in the bump commit). Tag `vX.Y.Z` equals it.
- Changelog: `CHANGELOG.md`. It still has an `[Unreleased]` section from the old format; the Common Changelog format (`~/.claude/VERSIONING.md`) applies from the next release entry.
- Deferred work: `PARKED.md`. Release steps: `docs/releasing.md`.
- Long runs, benchmarks and profiles go through `~/.claude/runs/run.ps1` (`~/.claude/RUNS.md`); rows land in `docs/runs.md`.
- New specs and plans (`docs/superpowers/`) start with front matter `status: draft | active | done | superseded-by: <file>`.
