# Bakes a bevel normal map, sends it to the canvas as the base layer, scrolls
# the Bake dock to the result actions, and captures the 2D canvas plus 3D view.
# Usage: powershell -ExecutionPolicy Bypass -File tools/test_bake_canvas.ps1
param(
    [string]$Mesh = "output/blender_cube.obj",
    [string]$OutUi = "output/ui/bake_canvas_ui.png",
    [string]$Out3d = "output/ui/bake_canvas_3d.png",
    [int]$WarmupSeconds = 22,
    [int]$Width = 1400,
    [int]$Height = 900
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32BakeCanvas {
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public struct POINT { public int X; public int Y; }
}
"@

function Move-To([int]$x, [int]$y) {
    [Win32BakeCanvas]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 16
}

function Click-At([int]$x, [int]$y) {
    Move-To $x $y
    [Win32BakeCanvas]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [Win32BakeCanvas]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 600
}

function Scroll-Down([int]$x, [int]$y, [int]$notches) {
    Move-To $x $y
    for ($i = 0; $i -lt $notches; $i++) {
        [Win32BakeCanvas]::mouse_event(0x0800, 0, 0, [uint32]4294967176, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
    }
    Start-Sleep -Milliseconds 600
}

function Save-Client([IntPtr]$handle, [string]$relativePath) {
    $rect = New-Object Win32BakeCanvas+RECT
    [Win32BakeCanvas]::GetClientRect($handle, [ref]$rect) | Out-Null
    $origin = New-Object Win32BakeCanvas+POINT
    [Win32BakeCanvas]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $w = $rect.Right - $rect.Left
    $h = [Math]::Min($rect.Bottom - $rect.Top, $screen.Bottom - $origin.Y)
    $bitmap = New-Object System.Drawing.Bitmap($w, $h)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($origin.X, $origin.Y, 0, 0, $bitmap.Size)
    $root = Split-Path -Parent $PSScriptRoot
    $outPath = Join-Path $root $relativePath
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and !(Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir | Out-Null }
    $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $outPath"
}

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
$arguments = @(
    "--demo-shape",
    "--mesh", $Mesh,
    "--bake",
    "--bake-mode", "bevel",
    "--bake-to-canvas",
    "--bake-resolution", "512",
    "--view", "2d"
)
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32BakeCanvas]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $arguments
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32BakeCanvas]::SetWindowPos($handle, [IntPtr](-1), 0, 0, $Width, $Height, 0x0000) | Out-Null
    [Win32BakeCanvas]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200

    $client = New-Object Win32BakeCanvas+RECT
    [Win32BakeCanvas]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32BakeCanvas+POINT
    [Win32BakeCanvas]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $w = $client.Right - $client.Left
    $h = $client.Bottom - $client.Top

    # Scroll the Bake dock down to the result actions.
    Scroll-Down ($origin.X + $w - 170) ($origin.Y + [int]($h * 0.6)) 4
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 900
    Save-Client $handle $OutUi

    # Switch to the 3D preview and capture the blended result on the mesh.
    Click-At ($origin.X + 470) ($origin.Y + 64)
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 1600
    Save-Client $handle $Out3d
}
finally {
    if (!$process.HasExited) { Stop-Process -Id $process.Id }
}
