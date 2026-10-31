# plumb

kind: app

- Version and changelog: `version` in each of `crates/plumb/Cargo.toml`, `crates/plumb-core/Cargo.toml` and `crates/plumb-scan/Cargo.toml` (one shared version, tag `vX.Y.Z` equals it); `CHANGELOG.md` in Common Changelog form (`~/.claude/VERSIONING.md`), no Unreleased section. The bump steps are in `docs/releasing.md`.
- Deferred work: `PARKED.md`. Release steps: `docs/releasing.md`.
- Long runs, benchmarks and profiles go through `~/.claude/runs/run.ps1` (`~/.claude/RUNS.md`); rows land in `docs/runs.md`.
- New specs and plans (`docs/superpowers/`) start with front matter `status: draft | active | done | superseded-by: <file>`.
