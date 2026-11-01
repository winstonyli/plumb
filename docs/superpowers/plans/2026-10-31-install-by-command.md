---
status: draft
---

# Install by command (v0.11) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (four small sequential tasks that share one script). Steps use checkbox (`- [ ]`) syntax.

**Goal:** `scoop bucket add plumb https://github.com/winstonyli/plumb` then `scoop install plumb` installs the newest release, and `release.yml` keeps `bucket/plumb.json` current on every non-prerelease tag.

**Architecture:** One PowerShell script writes `bucket/plumb.json` from a tag and a sha256. A new `Bucket` step in `release.yml`, after Publish, runs it and commits to `main` (prerelease tags only print the result). No Rust changes.

**Tech Stack:** PowerShell (5.1 locally, 7 on the runner), Scoop, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-31-install-by-command-design.md` (approved 2026-10-31). Roadmap item: v0.11 in `ROADMAP.md`. Winget and the `extras` bucket are parked (`PARKED.md`).

## Global Constraints

- Do not push, tag, bump the version or publish unasked; the `-rcN` tag run (Task 3 Step 5) and the real `v0.11.0` are the user's go-ahead each. The CHANGELOG entry is written by the user's bump commit, not here.
- Manifest file is UTF-8 without BOM, LF line endings, ends with a newline.
- `license` is `MIT|Apache-2.0`; `description` wording (below) is for the user to review.
- Edit workflow and doc files with the Edit tool. Never `python -` / stdin python.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Review Focus

- The bump step fails after Publish (for example `main` protected): the release must stand and the step go red, with the manual fallback in `docs/releasing.md` (Task 4).
- A `-rcN` tag must not commit anything: after the rc run, `origin/main` has no new commit (Task 3 Step 5).
- A tag with a suffix, a tag without `v`, and a non-lowercase or wrong-length hash are rejected by the script (Task 1 test).
- Running the bump twice for the same tag gives an identical file, so the step makes no empty commit (Task 1 test, Task 3 `bucket already current` branch).
- `main` moved between checkout and push: `git pull --rebase` before the push handles it (Task 3).

---

### Task 1: The bump script, test first

**Files:** Create `.github/scripts/bump-manifest.tests.ps1`, `.github/scripts/bump-manifest.ps1`.

**Interfaces:** Produces `bump-manifest.ps1 -Tag vX.Y.Z -Sha256 <64 lowercase hex> [-Root <dir>]`; writes `<Root>/bucket/plumb.json` (Root defaults to the repo root).

- [ ] **Step 1: Write the test** `.github/scripts/bump-manifest.tests.ps1`:

```powershell
# Run by hand after changing bump-manifest.ps1: powershell -NoProfile -File .github/scripts/bump-manifest.tests.ps1
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'bump-manifest.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) "bump-manifest-test-$PID"
New-Item -ItemType Directory $root | Out-Null
function Check($ok, $what) { if (-not $ok) { throw "FAIL: $what" } }
try {
    $h1 = 'c7271136a037af14180d7097fa04af6a76a7ce6372c408a15eddaa6df4f730e1' # v0.10.1's real .sha256
    $h2 = 'a' * 64
    $file = Join-Path $root 'bucket/plumb.json'

    & $script -Tag v0.10.1 -Sha256 $h1 -Root $root
    $raw = [IO.File]::ReadAllText($file)
    $m = $raw | ConvertFrom-Json
    Check ($m.version -eq '0.10.1') 'version drops the v'
    Check ($m.url -eq 'https://github.com/winstonyli/plumb/releases/download/v0.10.1/plumb-v0.10.1-windows-x64.zip') 'url'
    Check ($m.hash -eq $h1) 'hash'
    Check ($m.extract_dir -eq 'plumb-v0.10.1-windows-x64') 'extract_dir'
    Check ($m.autoupdate.url -like '*v$version/plumb-v$version-windows-x64.zip') 'autoupdate url keeps $version'
    Check (-not $raw.Contains("`r") -and $raw.EndsWith("`n")) 'LF endings and a final newline'
    Check ([IO.File]::ReadAllBytes($file)[0] -ne 0xEF) 'no BOM'

    & $script -Tag v0.10.1 -Sha256 $h1 -Root $root
    Check ([IO.File]::ReadAllText($file) -eq $raw) 'the same tag twice gives an identical file'

    & $script -Tag v0.11.0 -Sha256 $h2 -Root $root
    $m = Get-Content $file -Raw | ConvertFrom-Json
    Check ($m.version -eq '0.11.0' -and $m.hash -eq $h2) 'a new tag replaces version and hash'

    foreach ($bad in @(@('0.11.0', $h1), @('v0.11.0-rc1', $h1), @('v0.11', $h1), @('v0.11.0', $h1.ToUpper()), @('v0.11.0', 'abc'))) {
        $threw = $false
        try { & $script -Tag $bad[0] -Sha256 $bad[1] -Root $root } catch { $threw = $true }
        Check $threw "rejects tag '$($bad[0])' with hash '$($bad[1])'"
    }
    'bump-manifest tests ok'
} finally {
    Remove-Item -Recurse -Force $root
}
```

- [ ] **Step 2: Run it and watch it fail.** `powershell -NoProfile -File .github/scripts/bump-manifest.tests.ps1`. Expected: an error that `bump-manifest.ps1` is not recognized, exit 1.
- [ ] **Step 3: Write the script** `.github/scripts/bump-manifest.ps1`:

```powershell
# Writes bucket/plumb.json (the Scoop manifest) for release tag vX.Y.Z and the sha256 of its zip.
# Used by the Bucket step of release.yml; run by hand to repair a failed bump (docs/releasing.md).
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$Sha256,
    [string]$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
)
$ErrorActionPreference = 'Stop'
if ($Tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw "tag '$Tag' is not vX.Y.Z" }
$version = $Matches[1]
if ($Sha256 -cnotmatch '^[0-9a-f]{64}$') { throw "sha256 '$Sha256' is not 64 lowercase hex digits" }

# $version, $url, $sha256 and $basename inside the autoupdate block are Scoop's own placeholders.
$manifest = @'
{
    "version": "__VERSION__",
    "description": "Fast disk-usage explorer for Windows: scans an NTFS drive from its MFT, shows a size-sorted tree and a treemap, and follows changes live.",
    "homepage": "https://github.com/winstonyli/plumb",
    "license": "MIT|Apache-2.0",
    "url": "https://github.com/winstonyli/plumb/releases/download/v__VERSION__/plumb-v__VERSION__-windows-x64.zip",
    "hash": "__SHA256__",
    "extract_dir": "plumb-v__VERSION__-windows-x64",
    "bin": "plumb.exe",
    "shortcuts": [
        [
            "plumb.exe",
            "plumb"
        ]
    ],
    "checkver": "github",
    "autoupdate": {
        "url": "https://github.com/winstonyli/plumb/releases/download/v$version/plumb-v$version-windows-x64.zip",
        "hash": {
            "url": "$url.sha256",
            "regex": "$sha256\\s+$basename"
        },
        "extract_dir": "plumb-v$version-windows-x64"
    }
}
'@
$text = $manifest.Replace('__VERSION__', $version).Replace('__SHA256__', $Sha256).Replace("`r`n", "`n")
$dir = Join-Path $Root 'bucket'
New-Item -ItemType Directory -Force $dir | Out-Null
[IO.File]::WriteAllText((Join-Path $dir 'plumb.json'), $text + "`n", (New-Object Text.UTF8Encoding $false))
```

- [ ] **Step 4: Run the test and watch it pass.** Same command. Expected: `bump-manifest tests ok`, exit 0, and no `bump-manifest-test-*` directory left in `%TEMP%`.
- [ ] **Step 5: Commit** "Release: a script that writes the Scoop manifest from a tag and a hash" (files: both scripts).

### Task 2: The bucket manifest, proved with Scoop

**Files:** Create `bucket/plumb.json`.

**Interfaces:** Consumes Task 1's script.

- [ ] **Step 1: Generate it for the latest release.** `powershell -NoProfile -File .github/scripts/bump-manifest.ps1 -Tag v0.10.1 -Sha256 c7271136a037af14180d7097fa04af6a76a7ce6372c408a15eddaa6df4f730e1`. Confirm the hash is still the release's: `gh release download v0.10.1 -p '*.sha256' -O -` prints it first.
- [ ] **Step 2: Install from the file, without launching the GUI.** `scoop install "$PWD\bucket\plumb.json"`. Expected: it downloads the zip, passes the hash check, and ends with `'plumb' (0.10.1) was installed successfully!`. Then `scoop list plumb` shows 0.10.1, `Test-Path "$env:USERPROFILE\scoop\shims\plumb.exe"` is `True`, and `Test-Path "$env:USERPROFILE\scoop\apps\plumb\current\plumb.exe"` is `True`.
- [ ] **Step 3: Uninstall.** `scoop uninstall plumb`. Expected: the shim and the Start Menu shortcut are gone (`Test-Path` on the shim is `False`).
- [ ] **Step 4: Check the `autoupdate` block against the real release.** Copy `bucket\plumb.json` to a temp directory, then `& "$(scoop prefix scoop)\bin\checkver.ps1" -App plumb -Dir <that temp directory> -ForceUpdate`. Expected: no error, and the rewritten file has `version` 0.10.1 and the same `hash` as Step 1 (this proves the `url`, `$url.sha256` and `regex` lines resolve). If the hash differs or the regex fails, fix the `regex` line in the script's template, rerun Task 1 Step 4 and this task.
- [ ] **Step 5: Commit** "Bucket: a Scoop manifest for plumb 0.10.1" (file: `bucket/plumb.json`).

### Task 3: The workflow step

**Files:** Modify `.github/workflows/release.yml`.

**Interfaces:** Consumes the script; relies on `$env:ASSET` (set in Package) and the asset's `.sha256`.

- [ ] **Step 1: Add install lines to the release notes.** In the `Notes` step, immediately before `$body | Out-File -Encoding utf8 notes.md`, add:

```yaml
          if ("${{ github.ref_name }}" -notlike "*-*") {
            $body += ""
            $body += 'Install with Scoop: `scoop bucket add plumb https://github.com/winstonyli/plumb`, then `scoop install plumb`.'
          }
```

- [ ] **Step 2: Add the `Bucket` step** after the `Publish` step (same indentation as the other steps):

```yaml
      - name: Bucket
        run: |
          $tag = "${{ github.ref_name }}"
          $sha = ((Get-Content "$env:ASSET.sha256" -Raw) -split '\s+')[0]
          $script = Join-Path $env:RUNNER_TEMP "bump-manifest.ps1"
          Copy-Item .github/scripts/bump-manifest.ps1 $script
          function Invoke-Git { git @args; if ($LASTEXITCODE) { throw "git $args failed ($LASTEXITCODE)" } }
          if ($tag -like "*-*") {
            # A prerelease only shows what the real release would write.
            $dry = Join-Path $env:RUNNER_TEMP "bucket-dry"
            & $script -Tag ($tag -replace '-.*$', '') -Sha256 $sha -Root $dry
            Get-Content "$dry/bucket/plumb.json"
            return
          }
          Invoke-Git fetch origin main
          Invoke-Git checkout -B main origin/main
          & $script -Tag $tag -Sha256 $sha -Root (Get-Location).Path
          Invoke-Git config user.name "github-actions[bot]"
          Invoke-Git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          Invoke-Git add bucket/plumb.json
          git diff --cached --quiet
          if ($LASTEXITCODE -eq 0) { "bucket already current"; return }
          Invoke-Git commit -m "Bump bucket to $tag"
          Invoke-Git pull --rebase origin main
          Invoke-Git push origin HEAD:main
```

- [ ] **Step 3: Check the file parses.** `git diff --stat` shows only `release.yml`; read the diff once for indentation (the step must line up with `- name: Publish`). If PyYAML is installed, `python -P` on a one-line script file that loads the workflow is enough; if not, skip, because Step 5 is the real check.
- [ ] **Step 4: Commit** "Release: the Bucket step commits the Scoop manifest to main on a release tag" (file: `release.yml`).
- [ ] **Step 5: Ask the user to approve a throwaway `-rcN` tag** (the workflow then runs for about 9 minutes). On approval: `git tag v0.11.0-rc1 && git push origin v0.11.0-rc1`; wait for the run with `gh run watch`; expected: all steps green, the `Bucket` log prints a manifest with version `0.11.0` and that tag's real hash, and `git fetch origin && git log origin/main -1` is still the commit before the tag (no bot commit). Then `gh release delete v0.11.0-rc1 --cleanup-tag -y`. A failed run: fix, and use `-rc2`, never move the tag (`docs/releasing.md`).

### Task 4: Docs

**Files:** Modify `docs/releasing.md`.

- [ ] **Step 1: Add this section to `docs/releasing.md`** after the numbered steps, and in step 3 add "After the run, `git pull`: the bucket bump is a commit on `main`."

```markdown
## The Scoop bucket

`bucket/plumb.json` is the Scoop manifest (`scoop bucket add plumb https://github.com/winstonyli/plumb`, then `scoop install plumb`). On a non-prerelease tag the `Bucket` step of `release.yml` rewrites it with `.github/scripts/bump-manifest.ps1` (version from the tag, sha256 from the zip's `.sha256`) and commits `Bump bucket to <tag>` to `main`. A `-rcN` tag runs the script into a temporary directory and prints the manifest; nothing is committed. If the step fails after the release is published (for example `main` is protected), the release stands: run `powershell -NoProfile -File .github/scripts/bump-manifest.ps1 -Tag vX.Y.Z -Sha256 <hash from the .sha256 asset>`, commit and push. After changing the script, run `powershell -NoProfile -File .github/scripts/bump-manifest.tests.ps1`.
```

- [ ] **Step 2: Docs check.** `grep -rn -i "scoop\|winget" ROADMAP.md PARKED.md docs/releasing.md CLAUDE.md` finds no line that still promises winget or contradicts the above.
- [ ] **Step 3: Full tests.** `cargo test --workspace` still passes (nothing in Rust changed; this guards against a stray edit).
- [ ] **Step 4: Commit** "Docs: how the Scoop bucket is released".
- [ ] **Step 5: Leave the rest to the user.** Report that v0.11 is complete once the user bumps and tags `v0.11.0` and the bot commit `Bump bucket to v0.11.0` appears on `main`; at that point the spec and this plan go to `status: done` and the ROADMAP v0.11 line moves to the changelog (per `~/.claude/VERSIONING.md`).
