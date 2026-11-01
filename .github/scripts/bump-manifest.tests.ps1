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
