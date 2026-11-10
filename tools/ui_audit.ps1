# Capture the editor at several window sizes and right-dock tabs for UI review.
# Usage: powershell -ExecutionPolicy Bypass -File tools/ui_audit.ps1 [-Prefix before]
param(
    [string]$Prefix = "audit",
    [int]$WarmupSeconds = 9
)

$ErrorActionPreference = "Stop"
$capture = Join-Path $PSScriptRoot "capture_window.ps1"
$shots = @(
    @{ Name = "1920_props"; W = 1920; H = 1032; Args = @{ DemoBlendModes = $true } },
    @{ Name = "1600_props"; W = 1600; H = 950; Args = @{ DemoBlendModes = $true } },
    @{ Name = "1280_props"; W = 1280; H = 720; Args = @{ DemoBlendModes = $true } },
    @{ Name = "1024_props"; W = 1024; H = 700; Args = @{ DemoBlendModes = $true } },
    @{ Name = "800_props"; W = 800; H = 600; Args = @{ DemoBlendModes = $true } },
    @{ Name = "1280_3d"; W = 1280; H = 720; Args = @{} },
    @{ Name = "1280_mesh"; W = 1280; H = 720; Args = @{ Tab = "mesh" } },
    @{ Name = "1280_bake"; W = 1280; H = 720; Args = @{ Tab = "bake" } },
    @{ Name = "1280_lightmap"; W = 1280; H = 720; Args = @{ Tab = "lightmap" } },
    @{ Name = "1280_pbr"; W = 1280; H = 720; Args = @{ Tab = "pbr" } }
)

foreach ($shot in $shots) {
    $params = @{
        Out = "output/ui/$Prefix" + "_" + $shot.Name + ".png"
        WarmupSeconds = $WarmupSeconds
        Width = $shot.W
        Height = $shot.H
    }
    foreach ($key in $shot.Args.Keys) { $params[$key] = $shot.Args[$key] }
    & $capture @params
}
