# Drives a slider drag with synthetic mouse input while grabbing rapid
# screenshots, then reports regions that change between frames.
# Usage: powershell -ExecutionPolicy Bypass -File tools/flicker_probe.ps1 [-Extra "--demo-blend-modes"]
param(
    [int]$WarmupSeconds = 12,
    [int]$Width = 1440,
    [int]$Height = 900,
    [string]$OutDir = "output/ui/flicker",
    [int]$Frames = 24,
    [string[]]$Extra = @("--demo-blend-modes"),
    [int]$SliderY = 772,
    [int]$SliderX = -190
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Fl {
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

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
$arguments = $Extra
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32Fl]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $arguments
$outPath = Join-Path $root $OutDir
if (!(Test-Path $outPath)) { New-Item -ItemType Directory -Path $outPath | Out-Null }
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32Fl]::SetWindowPos($handle, [IntPtr](-1), 0, 0, $Width, $Height, 0x0000) | Out-Null
    [Win32Fl]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200

    $client = New-Object Win32Fl+RECT
    [Win32Fl]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32Fl+POINT
    [Win32Fl]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $w = $client.Right - $client.Left
    $h = [Math]::Min($client.Bottom - $client.Top, $screen.Bottom - $origin.Y)
    $bitmap = New-Object System.Drawing.Bitmap($w, $h)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $clone = { $g = [System.Drawing.Graphics]::FromImage($script:bmp2); $g.DrawImageUnscaled($bitmap, 0, 0); $g.Dispose(); $script:bmp2.Clone() }
    $script:bmp2 = New-Object System.Drawing.Bitmap($w, $h)

    # Grab the right dock's "Radius (px)" slider track for the selected shape.
    # It sits at the right edge of the properties dock.
    $sliderX = $origin.X + $w + $SliderX
    $sliderY = $origin.Y + $SliderY
    # Move first, then press, so the press lands on the slider track.
    [Win32Fl]::SetCursorPos($sliderX, $sliderY) | Out-Null
    Start-Sleep -Milliseconds 350
    [Win32Fl]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 200

    $index = 0
    for ($i = 0; $i -lt $Frames; $i++) {
        # Sweep the drag slowly back and forth over the track.
        $dx = [int](60 * [Math]::Sin($i / 3.0))
        [Win32Fl]::SetCursorPos($sliderX + $dx, $sliderY) | Out-Null
        $graphics.CopyFromScreen($origin.X, $origin.Y, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $outPath ("f{0:d2}.png" -f $index)), [System.Drawing.Imaging.ImageFormat]::Png)
        $index++
        Start-Sleep -Milliseconds 90
    }
    [Win32Fl]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    [Win32Fl]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $Frames frames to $outPath"
}
finally {
    if (!$process.HasExited) { Stop-Process -Id $process.Id }
}
