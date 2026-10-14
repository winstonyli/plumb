# Elevated. Makes a throwaway 128 MB NTFS volume mounted on a folder only (no drive letter), for
# trying plumb's folder-mount chip, MFT scan and change journal on it. Uses diskpart, so it needs
# no optional Windows features.
#   .\folder-mount-test.ps1            create and mount it at C:\plumb-mount
#   .\folder-mount-test.ps1 -Remove    unmount and delete it
param([switch]$Remove, [string]$Mount = 'C:\plumb-mount', [string]$Vhd = "$env:TEMP\plumb-mount.vhdx")
$ErrorActionPreference = 'Stop'
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run from an elevated PowerShell' }

function Run-Dp([string[]]$Lines) {
    $f = Join-Path $env:TEMP 'plumb-diskpart.txt'
    Set-Content -Path $f -Value $Lines -Encoding ascii
    $out = diskpart /s $f
    Remove-Item $f
    $out
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed ($LASTEXITCODE)" }
}

if ($Remove) {
    if (Test-Path $Vhd) {
        Run-Dp "select vdisk file=`"$Vhd`"", 'detach vdisk' | Out-Null
        Remove-Item $Vhd -Force
    }
    if (Test-Path $Mount) { Remove-Item $Mount -Force -ErrorAction SilentlyContinue }
    'removed'
    return
}
if (Test-Path $Vhd) { throw "$Vhd exists; run with -Remove first" }
if (Test-Path $Mount) { throw "$Mount exists; pick another -Mount or remove it" }
New-Item -ItemType Directory $Mount | Out-Null
Run-Dp "create vdisk file=`"$Vhd`" maximum=128 type=expandable", 'attach vdisk', 'create partition primary',
    'format fs=ntfs quick label=plumbmount', 'remove all noerr', "assign mount=`"$Mount`""
1..200 | ForEach-Object { [IO.File]::WriteAllBytes("$Mount\f$_.bin", (New-Object byte[] 40000)) }
"mounted at $Mount with 200 files (about 8 MB)"
