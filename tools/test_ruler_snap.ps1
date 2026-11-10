param(
    [string]$Out = "output/ruler_snap.png",
    [int]$Width = 1440,
    [int]$Height = 840,
    [int]$WarmupSeconds = 7,
    [switch]$NoRuler
)

# Draws a cut line with the pen while Shift is held, so every node snaps to
# the 30 degree ruler placed by --demo-ruler. Scattered clicks should end up
# on one straight line.

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Ruler {
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public struct POINT { public int X; public int Y; }
}
"@

$ErrorActionPreference = "Stop"

function Move-To([int]$x, [int]$y) {
    [Win32Ruler]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 30
}

function Click-At([int]$x, [int]$y) {
    Move-To $x $y
    [Win32Ruler]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 45
    [Win32Ruler]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 220
}

function Key-Down([byte]$code) { [Win32Ruler]::keybd_event($code, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }
function Key-Up([byte]$code) { [Win32Ruler]::keybd_event($code, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
if (!(Test-Path $exe)) {
    throw "Build the app first: cargo build -p ns_app"
}

$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32Ruler]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
$launchArgs = if ($NoRuler) { @("--empty") } else { @("--empty", "--demo-ruler") }
$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $launchArgs
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "window not found" }
    [Win32Ruler]::ShowWindow($handle, 9) | Out-Null
    [Win32Ruler]::SetWindowPos($handle, [IntPtr]::Zero, 0, 0, $Width, $Height, 0x0014) | Out-Null
    Start-Sleep -Milliseconds 700
    [Win32Ruler]::SetWindowPos($handle, [IntPtr](-1), 0, 0, 0, 0, 0x0001) | Out-Null
    [Win32Ruler]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200

    $rect = New-Object Win32Ruler+RECT
    [Win32Ruler]::GetClientRect($handle, [ref]$rect) | Out-Null
    $origin = New-Object Win32Ruler+POINT
    [Win32Ruler]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $w = $rect.Right - $rect.Left
    $h = $rect.Bottom - $rect.Top
    $cx = $origin.X
    $cy = $origin.Y

    # Pen tool in the rail.
    Click-At ($cx + 20) ($cy + 220)

    # Shift held for the whole stroke: every node snaps to the ruler angle.
    Key-Down 0x10
    Click-At ($cx + 620) ($cy + 250)
    Click-At ($cx + 760) ($cy + 400)
    Click-At ($cx + 900) ($cy + 300)
    # Enter ends the path open, which is the cut line.
    Key-Down 0x0D
    Key-Up 0x0D
    Key-Up 0x10
    Start-Sleep -Milliseconds 900

    Move-To ($screen.Right - 1) ($screen.Bottom - 1)
    Start-Sleep -Milliseconds 900

    $process.Refresh()
    if ($process.HasExited) {
        throw "the app exited during the interaction (code $($process.ExitCode))"
    }
    [Win32Ruler]::GetClientRect($handle, [ref]$rect) | Out-Null
    Write-Host "client ${w}x${h} at $($origin.X),$($origin.Y)"

    $shotHeight = [int][Math]::Min([int]$h, [int]($screen.Bottom - $origin.Y))
    Write-Host "capture ${w}x$shotHeight"
    $bitmap = New-Object System.Drawing.Bitmap([int]$w, [int]$shotHeight)
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
