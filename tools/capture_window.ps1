param(
    [string]$Out = "output/window.png",
    [int]$WarmupSeconds = 10,
    [switch]$Stress,
    [switch]$Empty,
    [string]$Mesh = "",
    [switch]$UvOverlay,
    [switch]$UvOverlay2d,
    [string]$UvOpacity = "",
    [string]$Pbr = "",
    [switch]$PbrAuto,
    [string]$PbrChannel = "",
    [string]$View = "",
    [string]$PreviewShape = "",
    [string]$PreviewParallax = "",
    [string]$PreviewHeightScale = "",
    [string]$PreviewTessellation = "",
    [switch]$Split,
    [switch]$SplitStacked,
    [string]$Tab = "",
    [switch]$HeightView,
    [switch]$IntegrateHeight,
    [switch]$DemoShape,
    [switch]$DemoDepth,
    [switch]$DemoGroups,
    [switch]$DemoVector,
    [switch]$DemoBoolean,
    [switch]$DemoCut,
    [switch]$DemoRuler,
    [switch]$DemoSmooth,
    [switch]$DemoCuts,
    [switch]$DemoSweep,
    [switch]$DemoCable,
    [switch]$DemoEdgeBevel,
    [switch]$DemoBlendModes,
    [switch]$DemoCompound,
    [switch]$DemoCombine,
    [switch]$DemoText,
    [string]$Svg = "",
    [string]$Photo = "",
    [string]$Alpha = "",
    [string]$NormalMap = "",
    [switch]$Paint,
    [switch]$PaintMask,
    [switch]$CurveEditor,
    [switch]$Bake,
    [switch]$BakePreview,
    [switch]$ShowCage,
    [switch]$BakeToCanvas,
    [string]$BakeHigh = "",
    [string]$BakeMode = "",
    [string]$BakeOut = "",
    [int]$BakeResolution = 0,
    [string]$BakeRadius = "",
    [string]$BakeIntensity = "",
    [switch]$Lightmap,
    [switch]$LightmapPreview,
    [string]$LightmapEnv = "",
    [string]$LightmapOut = "",
    [int]$LightmapResolution = 0,
    [int]$LightmapSamples = 0,
    [int]$LightmapBounces = 0,
    [string]$LightmapFill = "",
    [string]$LightmapFormat = "",
    [string]$LightmapLighting = "",
    [string]$LightmapDenoiser = "",
    [string]$LightmapBackend = "",
    [string]$LightmapOidnDevice = "",
    [switch]$Export,
    [int]$ExportResolution = 0,
    [string]$ExportTextures = "",
    [string]$ExportTiles = "",
    [string]$ExportDir = "",
    [int]$Width = 0,
    [int]$Height = 0,
    [string]$Exe = "",
    # Any other app flags, e.g. -Extra "--demo-mask-mixing","--paint-mask"
    [string[]]$Extra = @(),
    # Client-space clicks before capturing, e.g. -Click "1166,247"
    [string[]]$Click = @(),
    # Clicks that hold Shift or Ctrl, for multi-selection figures.
    [string[]]$ShiftClick = @(),
    [string[]]$CtrlClick = @(),
    # Mouse wheel: "x,y,delta" in client space; negative delta scrolls down.
    [string[]]$Wheel = @(),
    # Wheel entries applied before clicks, for scrolling to a control that is
    # below the fold. Existing -Wheel entries still run after the clicks.
    [string[]]$PreWheel = @(),
    # Client-space right clicks, e.g. -RightClick "20,258"
    [string[]]$RightClick = @(),
    # Left clicks applied after the right clicks, for choosing a context-menu
    # item: -RightClick "169,228" -PostClick "207,573"
    [string[]]$PostClick = @(),
    # Left clicks applied after the drags, e.g. to press Apply once a layer has
    # been dropped into an operation slot: -PostDragClick "1206,906"
    [string[]]$PostDragClick = @(),
    # Client-space drags, e.g. -Drag "800,340:920,440"
    [string[]]$Drag = @(),
    # Drags with Alt held, e.g. to duplicate a layer: -AltDrag "800,340:920,440"
    [string[]]$AltDrag = @(),
    # Drags that keep the primary button down through the capture, for live
    # lasso/stroke figures. The button is released after the screenshot.
    [string[]]$HoldDrag = @(),
    # Alt-drags that press Shift partway through, for the 45° duplicate lock:
    # -AltShiftDrag "640,620:648,540"
    [string[]]$AltShiftDrag = @(),
    # Keys pressed after the clicks: enter, escape, delete, space, tab, c, v, g,
    # arrows, or ctrl/shift combinations. Entries can be comma or semicolon
    # separated, e.g. -Keys "delete;ctrl+z" or -Keys "ctrl+c;ctrl+v".
    [string[]]$Keys = @(),
    # Leave the pointer here for the capture, e.g. -Hover "800,420". Hover-only
    # overlays (smart guides) need the cursor to stay over the canvas.
    [string]$Hover = ""
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32 {
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")]
    public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extraInfo);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public struct POINT { public int X; public int Y; }
}
"@

$root = Split-Path -Parent $PSScriptRoot
$exe = if ($Exe) { (Resolve-Path $Exe).Path } else { Join-Path $root "target\debug\n-shade.exe" }
if (!(Test-Path $exe)) {
    throw "Build the app first: cargo build -p ns_app"
}

# Force-killing a freshly started Bevy process can leave a Windows hard-error
# dialog owned by csrss on the desktop. It is not a normal window (it cannot
# be closed with WM_CLOSE), but it can be moved off-screen so it never lands
# in the next capture.
function Hide-HardErrorDialogs {
    Get-Process csrss -ErrorAction SilentlyContinue | ForEach-Object {
        try {
            if ($_.MainWindowTitle -like "*Application Error*") {
                [Win32]::SetWindowPos(
                    $_.MainWindowHandle, [IntPtr]::Zero, 4000, 400, 0, 0, 0x0011
                ) | Out-Null
            }
        } catch { }
    }
}
Hide-HardErrorDialogs

$arguments = @()
if ($Stress) { $arguments += "--stress" }
if ($Empty) { $arguments += "--empty" }
if ($Mesh) { $arguments += @("--mesh", $Mesh) }
if ($UvOverlay) { $arguments += "--uv-overlay" }
if ($UvOverlay2d) { $arguments += "--uv-overlay-2d" }
if ($UvOpacity) { $arguments += @("--uv-opacity", $UvOpacity) }
if ($Pbr) { $arguments += @("--pbr", $Pbr) }
if ($PbrAuto) { $arguments += "--pbr-auto" }
if ($PbrChannel) { $arguments += @("--pbr-channel", $PbrChannel) }
if ($View) { $arguments += @("--view", $View) }
if ($PreviewShape) { $arguments += @("--preview-shape", $PreviewShape) }
if ($PreviewParallax) { $arguments += @("--preview-parallax", $PreviewParallax) }
if ($PreviewHeightScale) { $arguments += @("--preview-height-scale", $PreviewHeightScale) }
if ($PreviewTessellation) { $arguments += @("--preview-tessellation", $PreviewTessellation) }
if ($Split) { $arguments += "--split" }
if ($SplitStacked) { $arguments += "--split-stacked" }
if ($Tab) { $arguments += @("--tab", $Tab) }
if ($HeightView) { $arguments += "--height" }
if ($IntegrateHeight) { $arguments += "--integrate-height" }
if ($DemoShape) { $arguments += "--demo-shape" }
if ($DemoDepth) { $arguments += "--demo-depth" }
if ($DemoGroups) { $arguments += "--demo-groups" }
if ($DemoVector) { $arguments += "--demo-vector" }
if ($DemoBoolean) { $arguments += "--demo-boolean" }
if ($DemoCut) { $arguments += "--demo-cut" }
if ($DemoRuler) { $arguments += "--demo-ruler" }
if ($DemoSmooth) { $arguments += "--demo-smooth" }
if ($DemoCuts) { $arguments += "--demo-cuts" }
if ($DemoSweep) { $arguments += "--demo-sweep" }
if ($DemoCable) { $arguments += "--demo-cable" }
if ($DemoEdgeBevel) { $arguments += "--demo-edge-bevel" }
if ($DemoBlendModes) { $arguments += "--demo-blend-modes" }
if ($DemoCompound) { $arguments += "--demo-compound" }
if ($DemoCombine) { $arguments += "--demo-combine" }
if ($DemoText) { $arguments += "--demo-text" }
if ($Svg) { $arguments += @("--svg", $Svg) }
if ($Photo) { $arguments += @("--photo", $Photo) }
if ($Alpha) { $arguments += @("--alpha", $Alpha) }
if ($NormalMap) { $arguments += @("--normal", $NormalMap) }
if ($Paint) { $arguments += "--paint" }
if ($PaintMask) { $arguments += "--paint-mask" }
if ($CurveEditor) { $arguments += "--curve-editor" }
if ($Bake) { $arguments += "--bake" }
if ($BakePreview) { $arguments += "--bake-preview" }
if ($ShowCage) { $arguments += "--show-cage" }
if ($BakeToCanvas) { $arguments += "--bake-to-canvas" }
if ($BakeHigh) { $arguments += @("--bake-high", $BakeHigh) }
if ($BakeMode) { $arguments += @("--bake-mode", $BakeMode) }
if ($BakeOut) { $arguments += @("--bake-out", $BakeOut) }
if ($BakeResolution -gt 0) { $arguments += @("--bake-resolution", $BakeResolution) }
if ($BakeRadius) { $arguments += @("--bake-radius", $BakeRadius) }
if ($BakeIntensity) { $arguments += @("--bake-intensity", $BakeIntensity) }
if ($Lightmap) { $arguments += "--lightmap" }
if ($LightmapPreview) { $arguments += "--lightmap-preview" }
if ($LightmapEnv) { $arguments += @("--lightmap-env", $LightmapEnv) }
if ($LightmapOut) { $arguments += @("--lightmap-out", $LightmapOut) }
if ($LightmapResolution -gt 0) { $arguments += @("--lightmap-resolution", $LightmapResolution) }
if ($LightmapSamples -gt 0) { $arguments += @("--lightmap-samples", $LightmapSamples) }
if ($LightmapBounces -gt 0) { $arguments += @("--lightmap-bounces", $LightmapBounces) }
if ($LightmapFill) { $arguments += @("--lightmap-fill", $LightmapFill) }
if ($LightmapFormat) { $arguments += @("--lightmap-format", $LightmapFormat) }
if ($LightmapLighting) { $arguments += @("--lightmap-lighting", $LightmapLighting) }
if ($LightmapDenoiser) { $arguments += @("--lightmap-denoiser", $LightmapDenoiser) }
if ($LightmapBackend) { $arguments += @("--lightmap-backend", $LightmapBackend) }
if ($LightmapOidnDevice) { $arguments += @("--lightmap-oidn-device", $LightmapOidnDevice) }
if ($Export) { $arguments += "--export" }
if ($ExportResolution -gt 0) { $arguments += @("--export-resolution", $ExportResolution) }
if ($ExportTextures) { $arguments += @("--export-textures", $ExportTextures) }
if ($ExportTiles) { $arguments += @("--export-tiles", $ExportTiles) }
if ($ExportDir) { $arguments += @("--export-dir", $ExportDir) }
if ($Extra.Count -gt 0) { $arguments += $Extra }
# Park the cursor off-window so hover tooltips do not end up in the capture.
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[Win32]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
if ($arguments.Count -gt 0) {
    $process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -ArgumentList $arguments
} else {
    $process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru
}
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) {
        throw "N-shade window not found"
    }

    [Win32]::ShowWindow($handle, 9) | Out-Null
    if ($Width -gt 0 -and $Height -gt 0) {
        # SWP_NOZORDER (0x4) | SWP_NOACTIVATE (0x10)
        [Win32]::SetWindowPos($handle, [IntPtr]::Zero, 0, 0, $Width, $Height, 0x0014) | Out-Null
        Start-Sleep -Milliseconds 700
    }
    # Windows may refuse SetForegroundWindow for background processes. Move the
    # window to the top-left and pin it topmost so CopyFromScreen sees the full
    # client area instead of whatever happened to be in front.
    # SWP_NOSIZE (0x1).
    [Win32]::SetWindowPos($handle, [IntPtr](-1), 0, 0, 0, 0, 0x0001) | Out-Null
    [Win32]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 1200

    $rect = New-Object Win32+RECT
    [Win32]::GetClientRect($handle, [ref]$rect) | Out-Null
    $origin = New-Object Win32+POINT
    [Win32]::ClientToScreen($handle, [ref]$origin) | Out-Null
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    $workArea = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $height = [Math]::Min($height, $workArea.Bottom - $origin.Y)
    if ($width -le 0 -or $height -le 0) {
        throw "Invalid client rect ${width}x${height}"
    }

    # Semicolon-separated entries also work, which is what -File argument
    # passing ends up producing: -Click "1166,247;1300,300"
    $preWheelTargets = @()
    foreach ($entry in $PreWheel) { $preWheelTargets += $entry.Split(";") }
    foreach ($entry in $preWheelTargets) {
        $parts = $entry.Split(",")
        if ($parts.Count -ne 3) { throw "PreWheel expects 'x,y,delta', got '$entry'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 150
        $delta = [System.BitConverter]::ToUInt32([System.BitConverter]::GetBytes([int]$parts[2]), 0)
        [Win32]::mouse_event(0x0800, 0, 0, $delta, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 400
    }
    $clickTargets = @()
    foreach ($entry in $Click) { $clickTargets += $entry.Split(";") }
    foreach ($target in $clickTargets) {
        $parts = $target.Split(",")
        if ($parts.Count -ne 2) { throw "Click expects 'x,y', got '$target'" }
        $clickX = $origin.X + [int]$parts[0]
        $clickY = $origin.Y + [int]$parts[1]
        [Win32]::SetCursorPos($clickX, $clickY) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 500
    }
    # Modifier clicks: hold Shift/Ctrl for the whole press so row selections
    # extend instead of replacing the current set.
    foreach ($m in @(@{ Param = $ShiftClick; Key = 0x10 }, @{ Param = $CtrlClick; Key = 0x11 })) {
        $modTargets = @()
        foreach ($entry in $m.Param) { $modTargets += $entry.Split(";") }
        foreach ($target in $modTargets) {
            $parts = $target.Split(",")
            if ($parts.Count -ne 2) { throw "Modifier click expects 'x,y', got '$target'" }
            [Win32]::keybd_event($m.Key, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 60
            [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
            Start-Sleep -Milliseconds 120
            [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 60
            [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 60
            [Win32]::keybd_event($m.Key, 0, 2, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 500
        }
    }
    foreach ($entry in $Wheel) {
        $parts = $entry.Split(",")
        if ($parts.Count -ne 3) { throw "Wheel expects 'x,y,delta', got '$entry'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 150
        $delta = [System.BitConverter]::ToUInt32([System.BitConverter]::GetBytes([int]$parts[2]), 0)
        [Win32]::mouse_event(0x0800, 0, 0, $delta, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 400
    }
    if ($Click.Count -gt 0 -or $RightClick.Count -gt 0 -or $PostClick.Count -gt 0 -or $PostDragClick.Count -gt 0 -or $Drag.Count -gt 0 -or $AltDrag.Count -gt 0 -or $HoldDrag.Count -gt 0 -or $ShiftClick.Count -gt 0 -or $CtrlClick.Count -gt 0 -or $Wheel.Count -gt 0 -or $Keys.Count -gt 0) {
        # Park the cursor off-window again so no hover state is captured.
        [Win32]::SetCursorPos($screen.Right - 1, $screen.Bottom - 1) | Out-Null
        Start-Sleep -Milliseconds 700
    }
    # Right clicks open context menus, e.g. the lasso flyout.
    $rightTargets = @()
    foreach ($entry in $RightClick) { $rightTargets += $entry.Split(";") }
    foreach ($target in $rightTargets) {
        $parts = $target.Split(",")
        if ($parts.Count -ne 2) { throw "RightClick expects 'x,y', got '$target'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [Win32]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 600
    }

    # Menu items need a left click after the menu is open; -Click runs before
    # -RightClick, so these get their own pass.
    $postTargets = @()
    foreach ($entry in $PostClick) { $postTargets += $entry.Split(";") }
    foreach ($target in $postTargets) {
        $parts = $target.Split(",")
        if ($parts.Count -ne 2) { throw "PostClick expects 'x,y', got '$target'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 150
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 500
    }

    # Drags press, move in steps and release, e.g. a lasso box.
    $dragTargets = @()
    foreach ($entry in $Drag) { $dragTargets += $entry.Split(";") }
    foreach ($target in $dragTargets) {
        $ends = $target.Split(":")
        if ($ends.Count -ne 2) { throw "Drag expects 'x1,y1:x2,y2', got '$target'" }
        $from = $ends[0].Split(",")
        $to = $ends[1].Split(",")
        if ($from.Count -ne 2 -or $to.Count -ne 2) { throw "Drag expects 'x1,y1:x2,y2', got '$target'" }
        $x1 = $origin.X + [int]$from[0]
        $y1 = $origin.Y + [int]$from[1]
        $x2 = $origin.X + [int]$to[0]
        $y2 = $origin.Y + [int]$to[1]
        [Win32]::SetCursorPos($x1, $y1) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        for ($step = 1; $step -le 24; $step++) {
            $x = [int]($x1 + ($x2 - $x1) * $step / 24)
            $y = [int]($y1 + ($y2 - $y1) * $step / 24)
            [Win32]::SetCursorPos($x, $y) | Out-Null
            Start-Sleep -Milliseconds 18
        }
        # Let the drop target see the hover before the release.
        Start-Sleep -Milliseconds 200
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 700
    }

    # Alt+Shift-drags press Alt first, then Shift a few samples into the move,
    # matching the common "start duplicating, then lock the axis" gesture.
    $altShiftDragTargets = @()
    foreach ($entry in $AltShiftDrag) { $altShiftDragTargets += $entry.Split(";") }
    foreach ($target in $altShiftDragTargets) {
        $ends = $target.Split(":")
        if ($ends.Count -ne 2) { throw "AltShiftDrag expects 'x1,y1:x2,y2', got '$target'" }
        $from = $ends[0].Split(",")
        $to = $ends[1].Split(",")
        if ($from.Count -ne 2 -or $to.Count -ne 2) { throw "AltShiftDrag expects 'x1,y1:x2,y2', got '$target'" }
        $x1 = $origin.X + [int]$from[0]
        $y1 = $origin.Y + [int]$from[1]
        $x2 = $origin.X + [int]$to[0]
        $y2 = $origin.Y + [int]$to[1]
        [Win32]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        [Win32]::SetCursorPos($x1, $y1) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        for ($step = 1; $step -le 24; $step++) {
            if ($step -eq 4) {
                [Win32]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
                Start-Sleep -Milliseconds 80
            }
            $x = [int]($x1 + ($x2 - $x1) * $step / 24)
            $y = [int]($y1 + ($y2 - $y1) * $step / 24)
            [Win32]::SetCursorPos($x, $y) | Out-Null
            Start-Sleep -Milliseconds 18
        }
        Start-Sleep -Milliseconds 200
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        [Win32]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        [Win32]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 700
    }

    # Alt-drags press Alt for the whole gesture so the drop duplicates.
    $altDragTargets = @()
    foreach ($entry in $AltDrag) { $altDragTargets += $entry.Split(";") }
    foreach ($target in $altDragTargets) {
        $ends = $target.Split(":")
        if ($ends.Count -ne 2) { throw "AltDrag expects 'x1,y1:x2,y2', got '$target'" }
        $from = $ends[0].Split(",")
        $to = $ends[1].Split(",")
        if ($from.Count -ne 2 -or $to.Count -ne 2) { throw "AltDrag expects 'x1,y1:x2,y2', got '$target'" }
        $x1 = $origin.X + [int]$from[0]
        $y1 = $origin.Y + [int]$from[1]
        $x2 = $origin.X + [int]$to[0]
        $y2 = $origin.Y + [int]$to[1]
        [Win32]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        [Win32]::SetCursorPos($x1, $y1) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        for ($step = 1; $step -le 24; $step++) {
            $x = [int]($x1 + ($x2 - $x1) * $step / 24)
            $y = [int]($y1 + ($y2 - $y1) * $step / 24)
            [Win32]::SetCursorPos($x, $y) | Out-Null
            Start-Sleep -Milliseconds 18
        }
        Start-Sleep -Milliseconds 200
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        [Win32]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 700
    }

    # Hold-drags leave the primary button down so in-progress overlays (lasso
    # rubber band, brush stroke, pen segment) are visible in the capture.
    $holdTargets = @()
    foreach ($entry in $HoldDrag) { $holdTargets += $entry.Split(";") }
    foreach ($target in $holdTargets) {
        $ends = $target.Split(":")
        if ($ends.Count -ne 2) { throw "HoldDrag expects 'x1,y1:x2,y2', got '$target'" }
        $from = $ends[0].Split(",")
        $to = $ends[1].Split(",")
        if ($from.Count -ne 2 -or $to.Count -ne 2) { throw "HoldDrag expects 'x1,y1:x2,y2', got '$target'" }
        $x1 = $origin.X + [int]$from[0]
        $y1 = $origin.Y + [int]$from[1]
        $x2 = $origin.X + [int]$to[0]
        $y2 = $origin.Y + [int]$to[1]
        [Win32]::SetCursorPos($x1, $y1) | Out-Null
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
        for ($step = 1; $step -le 24; $step++) {
            $x = [int]($x1 + ($x2 - $x1) * $step / 24)
            $y = [int]($y1 + ($y2 - $y1) * $step / 24)
            [Win32]::SetCursorPos($x, $y) | Out-Null
            Start-Sleep -Milliseconds 18
        }
        Start-Sleep -Milliseconds 250
    }

    # Clicks that must follow a drag, e.g. Apply after dropping a source layer
    # into an operation slot.
    $postDragTargets = @()
    foreach ($entry in $PostDragClick) { $postDragTargets += $entry.Split(";") }
    foreach ($target in $postDragTargets) {
        $parts = $target.Split(",")
        if ($parts.Count -ne 2) { throw "PostDragClick expects 'x,y', got '$target'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 150
        [Win32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 500
    }

    if ($Keys.Count -gt 0) {
        $keyCodes = @{ "enter" = 0x0D; "return" = 0x0D; "escape" = 0x1B; "esc" = 0x1B; "delete" = 0x2E; "space" = 0x20; "tab" = 0x09; "z" = 0x5A; "c" = 0x43; "v" = 0x56; "g" = 0x47; "r" = 0x52; "o" = 0x4F; "s" = 0x53; "p" = 0x50; "l" = 0x4C; "b" = 0x42; "h" = 0x48; "e" = 0x45; "questionmark" = 0xBF; "left" = 0x25; "up" = 0x26; "right" = 0x27; "down" = 0x28 }
        $keyTargets = @()
        foreach ($entry in $Keys) { $keyTargets += $entry.Split(@(";", ",")) }
        foreach ($key in $keyTargets) {
            $name = $key.ToLowerInvariant()
            $ctrl = $false
            $shiftKey = $false
            while ($true) {
                if ($name.StartsWith("ctrl+")) { $ctrl = $true; $name = $name.Substring(5); continue }
                if ($name.StartsWith("shift+")) { $shiftKey = $true; $name = $name.Substring(6); continue }
                break
            }
            if (!$keyCodes.ContainsKey($name)) { throw "Unknown key '$key'" }
            $code = [byte]$keyCodes[$name]
            if ($ctrl) { [Win32]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40 }
            if ($shiftKey) { [Win32]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40 }
            [Win32]::keybd_event($code, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 60
            [Win32]::keybd_event($code, 0, 2, [UIntPtr]::Zero)
            if ($shiftKey) { Start-Sleep -Milliseconds 40; [Win32]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero) }
            if ($ctrl) { Start-Sleep -Milliseconds 40; [Win32]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero) }
            Start-Sleep -Milliseconds 500
        }
    }

    # Leave the pointer over the canvas so hover-only overlays (smart guides)
    # appear in the capture.
    if ($Hover) {
        $parts = $Hover.Split(",")
        if ($parts.Count -ne 2) { throw "Hover expects 'x,y', got '$Hover'" }
        [Win32]::SetCursorPos($origin.X + [int]$parts[0], $origin.Y + [int]$parts[1]) | Out-Null
        Start-Sleep -Milliseconds 700
    }

    $bitmap = New-Object System.Drawing.Bitmap($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($origin.X, $origin.Y, 0, 0, $bitmap.Size)

    $outPath = Join-Path $root $Out
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and !(Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir | Out-Null
    }
    $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $outPath"
    if ($holdTargets.Count -gt 0) {
        [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 80
    }
}
finally {
    [Win32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    if (!$process.HasExited) {
        # Close the window first: a normal shutdown leaves no hard-error
        # dialog behind, which a forced kill can produce.
        $null = $process.CloseMainWindow()
        if (!$process.WaitForExit(4000)) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            Start-Sleep -Milliseconds 900
        }
    }
    Hide-HardErrorDialogs
}
