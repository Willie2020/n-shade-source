param(
    [Parameter(Mandatory = $true)][string]$In,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$X = 0,
    [int]$Y = 0,
    [int]$W = 200,
    [int]$H = 200,
    [int]$Scale = 1
)

# Crop a screenshot region and optionally upscale it with nearest-neighbour
# sampling. Used to inspect icon rendering and small UI details.
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile((Resolve-Path $In).Path)
$region = New-Object System.Drawing.Rectangle $X, $Y, $W, $H
$bmp = New-Object System.Drawing.Bitmap ($W * $Scale), ($H * $Scale)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$dest = New-Object System.Drawing.Rectangle 0, 0, $bmp.Width, $bmp.Height
$g.DrawImage($src, $dest, $region, [System.Drawing.GraphicsUnit]::Pixel)
$bmp.Save($Out)
$g.Dispose()
$bmp.Dispose()
$src.Dispose()
