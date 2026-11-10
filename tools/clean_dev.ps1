# Remove debug and profiling build artifacts while keeping the release build.
#
#   powershell -ExecutionPolicy Bypass -File tools/clean_dev.ps1
#
# The dev profile keeps line tables only and drops dependency debug info (see
# Cargo.toml), but rust-analyzer and test binaries still grow target/debug over
# time. This script reclaims that space without paying for a release rebuild;
# `cargo clean` would delete target/release as well.
#
# rust-analyzer keeps a proc-macro helper DLL loaded, so `cargo clean` can fail
# with "Access is denied". The fallback deletes everything that is not locked.
# Close Zed (or rerun after it settles) to keep target/debug fully absent.

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

# `dev` builds to target/debug, not target/dev.
$artifacts = [ordered]@{
    "dev"       = "target/debug"
    "profiling" = "target/profiling"
}

foreach ($profile in $artifacts.Keys) {
    $path = $artifacts[$profile]
    if (-not (Test-Path -LiteralPath $path)) {
        continue
    }

    cargo clean --profile $profile
    if ($LASTEXITCODE -eq 0 -and -not (Test-Path -LiteralPath $path)) {
        continue
    }

    Write-Warning "$profile artifacts are partially locked; removing what is not in use."
    Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue |
        ForEach-Object {
            try { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction Stop } catch {}
        }
    Get-ChildItem -LiteralPath $path -Recurse -Directory -Force -ErrorAction SilentlyContinue |
        Sort-Object { $_.FullName.Length } -Descending |
        ForEach-Object {
            try { Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction Stop } catch {}
        }
    try { Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop } catch {}

    if (Test-Path -LiteralPath $path) {
        $remaining = (Get-ChildItem -LiteralPath $path -Recurse -File -Force `
                -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
        Write-Warning ("Still locked: {0:N1} MB (rust-analyzer releases it on restart)" -f ($remaining / 1MB))
    }
}

if (Test-Path -LiteralPath "target/debug") {
    Write-Output "target/debug exists again - rust-analyzer is rebuilding check artifacts."
}
Write-Output "Kept target/release and dist/."
