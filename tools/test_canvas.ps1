param(
    [string]$Out = "output/canvas_test.png",
    [int]$WarmupSeconds = 7
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32 {
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
    [Win32]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 12
}

function Click-At([int]$x, [int]$y) {
    Move-To $x $y
    [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
}

function Drag-To([int]$x1, [int]$y1, [int]$x2, [int]$y2) {
    Move-To $x1 $y1
    [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le 20; $i++) {
        $x = [int]($x1 + ($x2 - $x1) * $i / 20)
        $y = [int]($y1 + ($y2 - $y1) * $i / 20)
        Move-To $x $y
    }
    [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\n-shade.exe"
if (!(Test-Path $exe)) {
    throw "Build the app first: cargo build -p ns_app"
}

$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) {
        throw "N-shade window not found"
    }
    # Keep the window above other apps while the synthetic input runs.
    [Win32]::SetWindowPos($handle, [IntPtr](-1), 0, 0, 0, 0, 0x0003) | Out-Null
    [Win32]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 700

    $client = New-Object Win32+RECT
    [Win32]::GetClientRect($handle, [ref]$client) | Out-Null
    $origin = New-Object Win32+POINT
    [Win32]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $left = $origin.X
    $top = $origin.Y
    Write-Host "client origin: $left,$top size $($client.Right)x$($client.Bottom)"

    # Select the emblem star in the canvas and drag it to the right.
    Drag-To ($left + 610) ($top + 425) ($left + 700) ($top + 455)

    # Activate the Rect tool and drag a new rectangle in an empty area.
    Click-At ($left + 426) ($top + 17)
    Drag-To ($left + 400) ($top + 560) ($left + 560) ($top + 650)

    # Switch the canvas to the height output produced by the same compute pass.
    Click-At ($left + 760) ($top + 17)

    # Capture the client area.
    Start-Sleep -Milliseconds 800
    $width = $client.Right - $client.Left
    $height = $client.Bottom - $client.Top
    $workArea = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $height = [Math]::Min($height, $workArea.Bottom - $top)
    $bitmap = New-Object System.Drawing.Bitmap($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($left, $top, 0, 0, $bitmap.Size)
    $outPath = Join-Path $root $Out
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and !(Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir | Out-Null
    }
    $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $outPath"
}
finally {
    if (!$process.HasExited) {
        Stop-Process -Id $process.Id
    }
}
