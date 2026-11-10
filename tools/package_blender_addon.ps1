# Packages the Blender add-on as a zip that "Install from Disk" accepts.
#
# The archive holds the module folder at its root (`n_shade_bridge/...`), which
# is the layout Blender's extension installer expects and the same one a legacy
# add-on install accepts.  Compiled bytecode is excluded: a .pyc from another
# machine must never shadow the sources.
#
#   powershell -ExecutionPolicy Bypass -File tools/package_blender_addon.ps1
#
# Re-running overwrites the archive, so the file is always the current build.

param(
    [string]$Source = "blender/n_shade_bridge",
    [string]$OutZip = "blender/n_shade_bridge.zip",
    [string]$RootName = "n_shade_bridge"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Test-Path $Source)) {
    throw "Add-on source not found: $Source"
}
$manifest = Join-Path $Source "blender_manifest.toml"
if (-not (Test-Path $manifest)) {
    throw "Missing $manifest - the archive would not install as an extension."
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression

$base = (Resolve-Path $Source).Path
$outDir = Split-Path $OutZip -Parent
if (-not $outDir) { $outDir = "." }
$zipFull = Join-Path (Resolve-Path $outDir).Path (Split-Path $OutZip -Leaf)
Remove-Item $zipFull -Force -ErrorAction SilentlyContinue

$archive = [System.IO.Compression.ZipFile]::Open(
    $zipFull, [System.IO.Compression.ZipArchiveMode]::Create
)
$count = 0
try {
    Get-ChildItem $base -Recurse -File |
        Where-Object {
            $_.FullName -notmatch '\\__pycache__\\' -and $_.Extension -ne '.pyc'
        } |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($base.Length).TrimStart('\', '/').Replace('\', '/')
            $entry = $archive.CreateEntry(
                "$RootName/$relative", [System.IO.Compression.CompressionLevel]::Optimal
            )
            $stream = $entry.Open()
            try {
                $bytes = [System.IO.File]::ReadAllBytes($_.FullName)
                $stream.Write($bytes, 0, $bytes.Length)
            } finally {
                $stream.Dispose()
            }
            $count++
        }
} finally {
    $archive.Dispose()
}

$version = (Select-String -Path $manifest -Pattern '^version\s*=' |
    Select-Object -First 1).Line -replace '.*=\s*"', '' -replace '".*', ''
Write-Output ("entries=" + $count + " version=" + $version)
Write-Output ("zip=" + $zipFull)
Write-Output ("bytes=" + (Get-Item $zipFull).Length)
