# Roadmap

Ordered milestones (format: `~/.claude/VERSIONING.md`). Tagging moves a line to `CHANGELOG.md`. Deferred work is in `PARKED.md`.

- v0.11 **Install by command** - plumb installs and updates with `scoop install` (a bucket in this repo) and `winget install --manifest` (a manifest in this repo; the public winget-pkgs listing is parked), from a public repository under MIT OR Apache-2.0.
    - Done when: the repository is public; a Scoop bucket in this repo and a winget manifest in this repo both install a tagged release; and `release.yml` bumps both on a tag.
