# Opens a layer mask, paints a stroke with synthetic input and captures the
# window. Used to check the mask editor end to end.
# Usage: powershell -ExecutionPolicy Bypass -File tools/test_mask.ps1 [-Out output/ui/mask_test.png]
param(
    [string]$Out = "output/ui/mask_test.png",
    [int]$WarmupSeconds = 10,
    [int]$Width = 1280,
    [int]$Height = 720,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Mask {
    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")]
    public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public struct POINT { public int X; public int Y; }
}
"@

function Move-To([int]$x, [int]$y) {
    [Win32Mask]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 16
}

function Drag-To([int]$x1, [int]$y1, [int]$x2, [int]$y2) {
    Move-To $x1 $y1
    [Win32Mask]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le 30; $i++) {
        $x = [int]($x1 + ($x2 - $x1) * $i / 30)
        $y = [int]($y1 + ($y2 - $y1) * $i / 30)
        Move-To $x $y
    }
    [Win32Mask]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 400
}

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
$arguments = @("--demo-blend-modes", "--paint-mask") + $ExtraArgs
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32Mask]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $arguments
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32Mask]::SetWindowPos($handle, [IntPtr](-1), 0, 0, $Width, $Height, 0x0000) | Out-Null
    [Win32Mask]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 900

    $client = New-Object Win32Mask+RECT
    [Win32Mask]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32Mask+POINT
    [Win32Mask]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $w = $client.Right - $client.Left
    $h = $client.Bottom - $client.Top

    # Paint a diagonal stroke across the middle of the viewport.
    $cx = $origin.X + [int]($w * 0.52)
    $cy = $origin.Y + [int]($h * 0.52)
    Drag-To ($cx - 140) ($cy - 90) ($cx + 140) ($cy + 90)
    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 1500

    $bitmap = New-Object System.Drawing.Bitmap($w, [Math]::Min($h, $screen.Bottom - $origin.Y))
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
