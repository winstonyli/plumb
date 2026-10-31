---
status: draft
---

# Install by command (v0.11)

Roadmap v0.11: after this, a user can `scoop install` plumb from a bucket in this repo and install it from a winget manifest in this repo, and `release.yml` keeps both pointing at the newest tag. Decided 2026-10-31: the manifest lives in this repo only (submitting to `microsoft/winget-pkgs` is in `PARKED.md`); the release workflow commits the bump straight to `main`.

## What a user gets

```
scoop bucket add plumb https://github.com/winstonyli/plumb
scoop install plumb            # later: scoop update plumb
winget install --manifest <checkout>\winget\WinstonLi.Plumb\<version>
```

The winget path needs a checkout, and `winget settings --enable LocalManifestFiles` (administrator, one time). It installs but does not update: `winget upgrade` needs a source, which only the parked winget-pkgs submission provides. The roadmap line says so.

## Facts the design rests on (checked 2026-10-31)

- The release zip `plumb-<tag>-windows-x64.zip` holds one folder of the same name, containing `plumb.exe`, `CHANGELOG.md` and both licenses. Its `.sha256` reads `<hash>  <file>`.
- Tags are `vX.Y.Z`; `-` marks a prerelease. `release.yml` has `contents: write` only and checks out the tag (detached).
- No Windows manifest in the crate requests elevation (no `requestedExecutionLevel` anywhere), so the exe runs as invoker, which shims need.
- `scoop` and `winget` v1.29 are on the dev machine; `wingetcreate` is not and is not needed.

## Files

- `bucket/plumb.json`: Scoop manifest. `version`, `description`, `homepage`, `license` `MIT|Apache-2.0`, `url` and `hash` of the zip, `extract_dir` `plumb-v<version>-windows-x64`, `bin` `plumb.exe`, a Start Menu shortcut, `checkver` `github`, and `autoupdate` (`url`, `extract_dir`, `hash` from `$url.sha256`). With `autoupdate`, `scoop checkver` can also refresh it by hand.
- `winget/WinstonLi.Plumb/<version>/`: three files (version, installer, defaultLocale), schema 1.6.0. Installer: `InstallerType: zip`, `NestedInstallerType: portable`, `NestedInstallerFiles` with `RelativeFilePath: plumb-v<version>-windows-x64\plumb.exe` and `PortableCommandAlias: plumb`, `Architecture: x64`, `Scope: user`, `InstallerUrl`, upper-case `InstallerSha256`. Only the newest version directory is kept.
- `.github/scripts/bump-manifests.ps1 -Tag vX.Y.Z -Sha256 <hex> [-Root <dir>]`: the one place that writes both manifests from the tag and the hash alone. It deletes older winget version directories.
- `.github/workflows/release.yml`: a step after Publish. For a non-prerelease tag: fetch and check out `main`, run the script with the hash it already computed, commit `Bump manifests to <tag>` as `github-actions[bot]`, `git pull --rebase`, push. For a prerelease tag (`-rcN`): run the script into a temp `-Root` and print `git diff --stat`; no commit, no push. The release notes gain the two install commands.
- `docs/releasing.md`: one paragraph on the manifests and the failure mode below. `ROADMAP.md`: v0.11 wording matches "winget installs, does not update". `CHANGELOG.md`: an Added entry at the bump.

## Decisions to review

- **Identifier and publisher.** `WinstonLi.Plumb`, publisher `Winston Li`. Permanent once anyone installs by it; say if you want another.
- **Scoop license string.** `MIT|Apache-2.0` (Scoop's form for a choice of licenses).
- **Version directory per release.** One directory kept, replaced on each bump, so history is in git.

## Failure mode

The bump step runs after the release is published. If it fails (for example `main` is protected, or `main` moved and the rebase conflicts), the release is still good and the step is red; fix by running the script locally with the tag and hash and pushing. The tag is never moved. Rationale: a failed bump must not block or undo a release.

## Testing

- Script, offline: run it into a temp `-Root` for `v0.10.1` with that release's real hash; assert the three winget files and `bucket/plumb.json` parse, contain the version and URL, and that a second run for another tag leaves one winget version directory. Left behind as `.github/scripts/bump-manifests.tests.ps1`, run by hand.
- Scoop, local: `scoop install .\bucket\plumb.json` against v0.10.1, check `plumb.exe` is on the shim path and starts, then `scoop uninstall plumb`.
- winget, local: `winget validate --manifest <dir>` (no administrator). The install itself needs `LocalManifestFiles`, a security setting this work does not change; the user runs `winget install --manifest <dir>` once and reports.
- Workflow: a throwaway `-rcN` tag exercises everything but the commit and push (per `docs/releasing.md`); the push is first exercised by the real `v0.11.0` tag. The risk is that a push failure is found after publishing; the failure mode above bounds it.

## Not in scope

winget-pkgs submission and `winget upgrade` (parked), code signing, an installer (MSI), arm64 builds, a separate bucket repository.

## Done when

The repository is public (it is); `scoop install` from the bucket and `winget install --manifest` both install a tagged release; and `release.yml` bumps both on a non-prerelease tag.
