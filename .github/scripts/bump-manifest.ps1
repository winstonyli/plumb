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
    "description": "Fast disk-usage explorer for Windows.",
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
