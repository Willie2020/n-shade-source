# Drives the 3D layer gizmo with synthetic input, then switches to the 2D
# canvas so the reflected change can be checked in one screenshot.
param(
    [string]$Out = "output/ui/gizmo_after.png",
    [int]$WarmupSeconds = 11,
    [int]$Width = 1400,
    [int]$Height = 900
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Gz {
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
    [Win32Gz]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 16
}
function Drag-To([int]$x1, [int]$y1, [int]$x2, [int]$y2) {
    Move-To $x1 $y1
    [Win32Gz]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 70
    for ($i = 1; $i -le 30; $i++) {
        Move-To ([int]($x1 + ($x2 - $x1) * $i / 30)) ([int]($y1 + ($y2 - $y1) * $i / 30))
    }
    [Win32Gz]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}
function Click-At([int]$x, [int]$y) {
    Move-To $x $y
    [Win32Gz]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [Win32Gz]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32Gz]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$args = @("--demo-shape", "--view", "3d", "--gizmo", "move")
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $args
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32Gz]::SetWindowPos($handle, [IntPtr](-1), 0, 0, $Width, $Height, 0x0000) | Out-Null
    [Win32Gz]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200
    $client = New-Object Win32Gz+RECT
    [Win32Gz]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32Gz+POINT
    [Win32Gz]::ClientToScreen($handle, [ref]$origin) | Out-Null

    # Drag right and down across the model with the Move gizmo active.
    Drag-To ($origin.X + 700) ($origin.Y + 430) ($origin.X + 820) ($origin.Y + 500)

    # Switch to the 2D canvas to see the reflected change.
    Click-At ($origin.X + 540) ($origin.Y + 64)

    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 1200
    $w = $client.Right - $client.Left
    $h = [Math]::Min($client.Bottom - $client.Top, $screen.Bottom - $origin.Y)
    $bitmap = New-Object System.Drawing.Bitmap($w, $h)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($origin.X, $origin.Y, 0, 0, $bitmap.Size)
    $outPath = Join-Path $root $Out
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and !(Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir | Out-Null }
    $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $outPath"
}
finally {
    if (!$process.HasExited) { Stop-Process -Id $process.Id }
}
