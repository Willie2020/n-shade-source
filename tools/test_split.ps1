# Exercises the split viewport: drags the divider, focuses the 2D pane and
# switches the pane orientation. Captures each state for visual inspection.
# Usage: powershell -ExecutionPolicy Bypass -File tools/test_split.ps1
param(
    [string]$OutBefore = "output/ui/split_before.png",
    [string]$OutDragged = "output/ui/split_dragged.png",
    [string]$OutFocus = "output/ui/split_focus_2d.png",
    [string]$OutStacked = "output/ui/split_stacked_after.png",
    [int]$WarmupSeconds = 13,
    [int]$Width = 1500,
    [int]$Height = 950
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Split {
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
    [Win32Split]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 16
}

function Drag-To([int]$x1, [int]$y1, [int]$x2, [int]$y2) {
    Move-To $x1 $y1
    [Win32Split]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le 24; $i++) {
        Move-To ([int]($x1 + ($x2 - $x1) * $i / 24)) ([int]($y1 + ($y2 - $y1) * $i / 24))
    }
    [Win32Split]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

function Click-At([int]$x, [int]$y) {
    Move-To $x $y
    [Win32Split]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [Win32Split]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 600
}

function Save-Client([IntPtr]$handle, [string]$relativePath) {
    $rect = New-Object Win32Split+RECT
    [Win32Split]::GetClientRect($handle, [ref]$rect) | Out-Null
    $origin = New-Object Win32Split+POINT
    [Win32Split]::ClientToScreen($handle, [ref]$origin) | Out-Null
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
    "--mesh", "output/blender_cube.obj",
    "--split",
    "--view", "3d"
)
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32Split]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $arguments
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32Split]::SetWindowPos($handle, [IntPtr](-1), 0, 0, $Width, $Height, 0x0000) | Out-Null
    [Win32Split]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200

    $client = New-Object Win32Split+RECT
    [Win32Split]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32Split+POINT
    [Win32Split]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $w = $client.Right - $client.Left
    $h = $client.Bottom - $client.Top
    $cx = $origin.X + [int]($w * 0.68)
    $cy = $origin.Y + [int]($h * 0.55)

    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 700
    Save-Client $handle $OutBefore

    # Drag the divider toward the 2D pane so the 3D view grows.
    Drag-To ($origin.X + 790) ($origin.Y + 470) ($origin.X + 880) ($origin.Y + 470)
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 800
    Save-Client $handle $OutDragged

    # Click the 2D pane to move focus and swap the contextual header.
    Click-At $cx $cy
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 800
    Save-Client $handle $OutFocus

    # Toggle side-by-side to stacked via the header orientation button.
    Click-At ($origin.X + 1120) ($origin.Y + 64)
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 1000
    Save-Client $handle $OutStacked
}
finally {
    if (!$process.HasExited) { Stop-Process -Id $process.Id }
}
