# Fetch the Intel Open Image Denoise runtime used by the portable build.
#
# OIDN is loaded dynamically by ns_bake; shipping it next to N-shade.exe makes
# the portable archive self-contained instead of depending on an OIDN install
# or on PATH. The script verifies the official release archive before extracting
# only the runtime files the app needs (CPU + CUDA devices, TBB and licenses).
#
#   powershell -ExecutionPolicy Bypass -File tools/fetch_oidn.ps1
#
# Pass -OutDir to place the files elsewhere. `tools/package_release.ps1` receives
# that directory through -ExtraFile, which flattens its files next to the
executable.

param(
    [string]$Version = "2.3.1",
    [string]$OutDir = "oidn",
    # Official release archives use the x64 Windows build for the portable app.
    [string]$Sha256 = "61f579986d5b5c0e2f3f96db46869a77191f45bc001e558c3d91450b053894f0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$archiveName = "oidn-$Version.x64.windows.zip"
$url = "https://github.com/OpenImageDenoise/oidn/releases/download/v$Version/$archiveName"
$cache = Join-Path $env:TEMP $archiveName

if (-not (Test-Path $cache)) {
    Write-Output "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $cache -UseBasicParsing
}

$actual = (Get-FileHash $cache -Algorithm SHA256).Hash.ToLowerInvariant()
if ($Sha256 -and $actual -ne $Sha256.ToLowerInvariant()) {
    throw "OpenImageDenoise archive checksum mismatch: expected $Sha256, got $actual"
}

$extract = Join-Path $env:TEMP ("oidn-$Version-extract")
$package = Join-Path $extract ("oidn-$Version.x64.windows")
if (Test-Path $extract) {
    Remove-Item $extract -Recurse -Force
}
Expand-Archive -Path $cache -DestinationPath $extract -Force

$bin = Join-Path $package "bin"
$doc = Join-Path $package "doc"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# The API, its core, the CPU device and the CUDA device. SYCL/HIP are omitted:
# they need extra Intel/AMD runtimes and the app falls back to CPU cleanly.
$runtime = @(
    "OpenImageDenoise.dll",
    "OpenImageDenoise_core.dll",
    "OpenImageDenoise_device_cpu.dll",
    "OpenImageDenoise_device_cuda.dll",
    "tbb12.dll",
    "tbbbind.dll",
    "tbbbind_2_0.dll",
    "tbbbind_2_5.dll"
)
foreach ($name in $runtime) {
    $source = Join-Path $bin $name
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $OutDir $name) -Force
    } else {
        Write-Warning "OpenImageDenoise runtime file missing: $name"
    }
}

# Keep the license and third-party notices inside the portable archive. They
# are copied under OIDN-* names so they cannot collide with the project
# licenses.
Copy-Item (Join-Path $doc "LICENSE.txt") (Join-Path $OutDir "OIDN-LICENSE.txt") -Force
$notices = @(
    "third-party-programs.txt",
    "third-party-programs-oneTBB.txt",
    "third-party-programs-DPCPP.txt",
    "third-party-programs-oneDNN.txt"
)
$noticeText = New-Object System.Text.StringBuilder
foreach ($name in $notices) {
    $source = Join-Path $doc $name
    if (Test-Path $source) {
        [void]$noticeText.AppendLine("===== $name =====")
        [void]$noticeText.AppendLine((Get-Content $source -Raw))
        [void]$noticeText.AppendLine()
    }
}
Set-Content -Path (Join-Path $OutDir "OIDN-THIRD-PARTY-PROGRAMS.txt") `
    -Value $noticeText.ToString() -Encoding UTF8

$files = Get-ChildItem $OutDir -File
$total = ($files | Measure-Object -Property Length -Sum).Sum
Write-Output ("OIDN $Version -> ${OutDir}: " + $files.Count + " files, " + [math]::Round($total / 1MB, 1) + " MiB")
