# Roadmap

Ordered milestones (format: `~/.claude/VERSIONING.md`). Tagging moves a line to `CHANGELOG.md`. Deferred work is in `PARKED.md`.

- v0.10 **The Processes view tells the truth** - A parent process's own memory shows in the map, the Files held list stays current while a process is selected, and private bytes is offered beside working set.
    - Done when: a parent with a large own working set has a tile for it inside its frame and the frame's children no longer fill it; the Files held list refreshes within the PARKED design's bounds (handle count changed and 10 s since the last lookup) and idle CPU stays under 1 %; the metric button offers private bytes; and the three matching `PARKED.md` entries (own memory, Files held refresh, shared pages counted per process) are deleted (own memory and shared pages: done).
- v0.11 **Install by command** - plumb installs and updates with `scoop install` and `winget install`, from a public repository under MIT OR Apache-2.0.
    - Done when: the repository is public; a Scoop bucket in this repo and a winget manifest both install a tagged release; and `release.yml` bumps both on a tag.
