# Assemble the portable Windows archive for distribution.
#
#   cargo build --release -p ns_app
#   powershell -ExecutionPolicy Bypass -File tools/package_release.ps1
#
# The script copies the release executable, the manual, the license and the
# third-party notices into dist/N-shade.zip, then writes sha256sum-compatible
# checksum files. It never modifies the executable, so an Authenticode
# signature applied beforehand stays valid.
#
# Optional pieces:
#   -ExtraFile oidn        include the OpenImageDenoise runtime fetched by
#                          tools/fetch_oidn.ps1
#   -AssetDir <path>       include a directory tree (defaults to the Blender add-on)

param(
    [string]$Exe = "target/release/n-shade.exe",
    [string]$OutDir = "dist",
    [string]$OutZip = "dist/N-shade.zip",
    [string[]]$ExtraFile = @("Manual.md", "THIRD_PARTY_NOTICES.md"),
    [string[]]$AssetDir = @("blender/n_shade_bridge"),
    [string]$ProductDirName = "N-shade"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Test-Path $Exe)) {
    throw "Release executable not found: $Exe. Run 'cargo build --release -p ns_app' first."
}
if (-not (Test-Path "LICENSE")) {
    throw "LICENSE is missing. The repository must ship its MIT license text."
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression

# Everything that goes into the archive, keyed by its path inside the zip.
# Forward slashes are written explicitly: Compress-Archive has a history of
# emitting backslash entry names, which non-Windows extractors turn into one
# flat file per directory instead of a tree.
$entries = [ordered]@{}
$entries["$ProductDirName/N-shade.exe"] = (Resolve-Path $Exe).Path

# Extras are flattened next to the executable. An OIDN directory therefore
# lands directly beside N-shade.exe, where the loader expects it.
foreach ($file in $ExtraFile) {
    if (-not (Test-Path $file)) {
        Write-Warning "Skipping missing extra file: $file"
        continue
    }
    foreach ($leaf in Get-ChildItem $file -File) {
        $entries["$ProductDirName/" + $leaf.Name] = $leaf.FullName
    }
}

$entries["$ProductDirName/LICENSE"] = (Resolve-Path "LICENSE").Path

foreach ($dir in $AssetDir) {
    if (-not (Test-Path $dir)) {
        Write-Warning "Skipping missing asset directory: $dir"
        continue
    }
    $base = (Resolve-Path $dir).Path
    Get-ChildItem $base -Recurse -File |
        Where-Object {
            $_.FullName -notmatch '\\__pycache__\\' -and $_.Extension -ne '.pyc'
        } |
        ForEach-Object {
            $parent = Split-Path $base -Parent
            $leaf = Split-Path $base -Leaf
            $relative = $_.FullName.Substring($base.Length).TrimStart('\', '/').Replace('\', '/')
            $rootName = if ($parent) { (Split-Path $parent -Leaf) + "/" + $leaf } else { $leaf }
            $entries["$ProductDirName/$rootName/$relative"] = $_.FullName
        }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zipFull = (Join-Path (Resolve-Path $OutDir).Path (Split-Path $OutZip -Leaf))
Remove-Item $zipFull -Force -ErrorAction SilentlyContinue

$archive = [System.IO.Compression.ZipFile]::Open($zipFull, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($key in $entries.Keys) {
        $entry = $archive.CreateEntry($key, [System.IO.Compression.CompressionLevel]::Optimal)
        $stream = $entry.Open()
        try {
            $bytes = [System.IO.File]::ReadAllBytes($entries[$key])
            $stream.Write($bytes, 0, $bytes.Length)
        } finally {
            $stream.Dispose()
        }
    }
} finally {
    $archive.Dispose()
}

$exeHash = (Get-FileHash $entries["$ProductDirName/N-shade.exe"] -Algorithm SHA256).Hash
$zipHash = (Get-FileHash $zipFull -Algorithm SHA256).Hash

Set-Content -Path (Join-Path $OutDir "N-shade.exe.sha256") `
    -Value ($exeHash + "  $ProductDirName/N-shade.exe") -Encoding ASCII -NoNewline
Set-Content -Path (Join-Path $OutDir "N-shade.zip.sha256") `
    -Value ($zipHash + "  N-shade.zip") -Encoding ASCII -NoNewline

Write-Output ("entries=" + $entries.Count)
Write-Output ("exe bytes=" + (Get-Item $entries["$ProductDirName/N-shade.exe"]).Length + " sha256=" + $exeHash)
Write-Output ("zip bytes=" + (Get-Item $zipFull).Length + " sha256=" + $zipHash)
