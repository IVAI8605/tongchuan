# 生成 Windows 图标 assets\tongchuan.ico。
# 图形与安卓端 res\drawable\ic_launcher.xml 完全同一套：0F6CBD 蓝底 + 一上一下两支白箭。
# 用脚本按尺寸重绘而不是缩放一张位图，是为了让 16×16 这种小尺寸也保持清晰。
param(
    [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not $Out) {
    $Out = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'assets\tongchuan.ico'
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null

function New-Frame([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    # 安卓矢量图按 108 单位画，这里按比例换算
    $s = $size / 108.0
    $bg = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 15, 108, 189))
    $fg = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
    $g.FillRectangle($bg, 0, 0, $size, $size)

    function P([double]$x, [double]$y) {
        New-Object System.Drawing.PointF([single]($x * $s), [single]($y * $s))
    }

    # 左箭：竖条在 34~42，箭尖朝下（y 68→84）
    $g.FillRectangle($fg, [single](34 * $s), [single](26 * $s), [single](8 * $s), [single](42 * $s))
    $g.FillPolygon($fg, [System.Drawing.PointF[]]@((P 24 68), (P 52 68), (P 38 84)))
    # 右箭：竖条在 66~74，箭尖朝上（y 40→24）
    $g.FillRectangle($fg, [single](66 * $s), [single](40 * $s), [single](8 * $s), [single](42 * $s))
    $g.FillPolygon($fg, [System.Drawing.PointF[]]@((P 60 40), (P 88 40), (P 74 24)))

    $g.Dispose()
    $bg.Dispose()
    $fg.Dispose()
    return $bmp
}

# 现代图标都是多尺寸存放，Explorer 按需挑最合适的一张
$sizes = @(256, 128, 64, 48, 32, 16)
$frames = @()
foreach ($s in $sizes) {
    $bmp = New-Frame $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $frames += , $ms.ToArray()
    $ms.Dispose()
}

# 手写 ICO 目录：PNG 帧比 BMP 帧省事，且 Vista 之后都支持
$fs = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([UInt16]0)                 # reserved
$bw.Write([UInt16]1)                 # type = icon
$bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }   # 256 在 ICO 里记作 0
    $bw.Write([Byte]$dim)
    $bw.Write([Byte]$dim)
    $bw.Write([Byte]0)               # 调色板数
    $bw.Write([Byte]0)               # reserved
    $bw.Write([UInt16]1)             # color planes
    $bw.Write([UInt16]32)            # bpp
    $bw.Write([UInt32]$frames[$i].Length)
    $bw.Write([UInt32]$offset)
    $offset += $frames[$i].Length
}
foreach ($f in $frames) { $bw.Write($f) }
$bw.Flush()
$bw.Dispose()
$fs.Dispose()

Write-Output ("OK icon {0} ({1} bytes, sizes {2})" -f $Out, (Get-Item $Out).Length, ($sizes -join '/'))
