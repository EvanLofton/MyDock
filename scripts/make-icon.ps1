# Dock 的软件图标生成器（纯 PowerShell + System.Drawing 画，不依赖任何外部素材）
#
# 为什么要自己画而不是"上网找一张"：
#   把网上的图片塞进 MIT 仓库是**版权坑** —— 大多数图标站的素材不允许再分发，
#   而所谓"免费"往往只指个人使用。自己画没有这个问题，而且能和软件观感对齐
#   （深色玻璃 + 蓝色强调色，和 Dock 面板/右键菜单同一套配色）。
#
# 画面（1024×1024 画布）：
#   · 圆角方（接近 macOS squircle 观感）+ 竖直渐变深色玻璃 + 顶部反光 + 细描边
#   · 中间一条"程序坞"：一排 5 个圆角方块，**中间那个被放大并抬起** —— 这正是本软件
#     最有辨识度的功能（悬停鱼眼放大），一眼看出是"程序坞"
#   · 中间方块用软件自己的强调色 #0a84ff
#
# ⚠️ 为什么用**纯 PowerShell** 而不是 `Add-Type` 编译一段 C#：
#   本机 PowerShell 7 上 `System.Drawing` 的类型被转发到 `System.Drawing.Common` /
#   `System.Drawing.Primitives`，而 `Graphics` 的实现又落在**私有程序集**
#   `System.Private.Windows.GdiPlus` 里 —— 编译 C# 时引用不到它（CS0012），
#   但 PowerShell 直接调这些类型没问题（运行时会加载）。实测踩过这个坑。
#
# 用法： pwsh -File scripts/make-icon.ps1
# 产物： app/src-tauri/icons/icon.png（1024，Tauri 打包用）
#        app/src-tauri/icons/icon.ico（16/24/32/48/64/128/256 多尺寸）
#        docs/images/icon.png（README 展示用，256）

param(
  [int]$Size = 1024,
  [string]$RepoRoot = (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$script:ARGB = [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
$script:HighQ = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$script:AA = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$script:Pad = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality

function New-RoundRect {
    param([System.Drawing.RectangleF]$R, [single]$Radius)
    $p = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = $Radius * 2
    $p.AddArc($R.X, $R.Y, $d, $d, 180, 90)
    $p.AddArc($R.Right - $d, $R.Y, $d, $d, 270, 90)
    $p.AddArc($R.Right - $d, $R.Bottom - $d, $d, $d, 0, 90)
    $p.AddArc($R.X, $R.Bottom - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Get-Lighter {
    param([System.Drawing.Color]$C, [single]$F)
    return [System.Drawing.Color]::FromArgb(
        $C.A,
        [Math]::Min(255, [int]($C.R + (255 - $C.R) * $F)),
        [Math]::Min(255, [int]($C.G + (255 - $C.G) * $F)),
        [Math]::Min(255, [int]($C.B + (255 - $C.B) * $F)))
}

function New-IconBitmap {
    param([int]$S)
    $bmp = [System.Drawing.Bitmap]::new($S, $S, $script:ARGB)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = $script:AA
    $g.InterpolationMode = $script:HighQ
    $g.PixelOffsetMode = $script:Pad
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = $S * 0.055
    $body = [System.Drawing.RectangleF]::new($pad, $pad, $S - $pad * 2, $S - $pad * 2)
    $radius = $S * 0.215
    $path = New-RoundRect -R $body -Radius $radius
    try {
        # ① 深色玻璃底（上浅下深 = 有厚度）
        $lg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
            $body, [System.Drawing.Color]::FromArgb(255, 47, 52, 64),
            [System.Drawing.Color]::FromArgb(255, 18, 20, 26), [single]90)
        $g.FillPath($lg, $path); $lg.Dispose()
        # ② 顶部反光：白色渐隐
        $hlRect = [System.Drawing.RectangleF]::new($body.X, $body.Y, $body.Width, $body.Height * 0.5 + $radius)
        $hl = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
            $hlRect, [System.Drawing.Color]::FromArgb(70, 255, 255, 255),
            [System.Drawing.Color]::FromArgb(0, 255, 255, 255), [single]90)
        $hlPath = New-RoundRect -R $hlRect -Radius $radius
        $g.FillPath($hl, $hlPath); $hlPath.Dispose(); $hl.Dispose()
        # ③ 细描边
        $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(90, 255, 255, 255), [single]($S * 0.006))
        $pen.Alignment = [System.Drawing.Drawing2D.PenAlignment]::Inset
        $g.DrawPath($pen, $path); $pen.Dispose()

        # ④ 中间的程序坞：**深色底座** + 5 个方块（中间放大抬起）
        #
        # 为什么底座要暗：第一版把底座画成浅色玻璃条，结果和白色方块糊在一起、
        # 两侧方块贴边像被截断。压暗之后方块才"浮"在底座上，一眼是程序坞。
        $baseTile = $S * 0.155
        $scale = @(0.55, 0.78, 1.0, 0.78, 0.55)
        $gap = $baseTile * 0.22
        $padX = $baseTile * 0.30
        $widths = @(); $tilesW = 0.0
        foreach ($sc in $scale) { $w = $baseTile * $sc; $widths += $w; $tilesW += $w }
        $tilesW += $gap * ($scale.Count - 1)
        $barW = $tilesW + 2 * $padX
        $barH = $baseTile * 1.46
        $bar = [System.Drawing.RectangleF]::new(
            $body.X + ($body.Width - $barW) / 2, $body.Y + $body.Height * 0.47, $barW, $barH)
        $bottom = $bar.Bottom - $baseTile * 0.23      # 方块底边（与底座留一点内边距）

        # 蓝色柔光：强调色透出玻璃（中心方块后面那一团）
        $glowPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $glowPath.AddEllipse($bar.X + $bar.Width * 0.30, $bar.Y - $barH * 0.35, $bar.Width * 0.40, $barH * 1.7)
        $pg = [System.Drawing.Drawing2D.PathGradientBrush]::new($glowPath)
        $pg.CenterColor = [System.Drawing.Color]::FromArgb(120, 10, 132, 255)
        $pg.SurroundColors = [System.Drawing.Color[]]@([System.Drawing.Color]::FromArgb(0, 10, 132, 255))
        $g.FillPath($pg, $glowPath); $pg.Dispose(); $glowPath.Dispose()

        $barPath = New-RoundRect -R $bar -Radius ($barH * 0.36)
        $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
            $bar, [System.Drawing.Color]::FromArgb(150, 0, 0, 0),
            [System.Drawing.Color]::FromArgb(205, 0, 0, 0), [single]90)
        $g.FillPath($bg, $barPath); $bg.Dispose()
        $bpen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(70, 255, 255, 255), [single]($S * 0.004))
        $bpen.Alignment = [System.Drawing.Drawing2D.PenAlignment]::Inset
        $g.DrawPath($bpen, $barPath); $bpen.Dispose(); $barPath.Dispose()

        $x = $bar.X + $padX
        for ($i = 0; $i -lt $scale.Count; $i++) {
            $w = $widths[$i]
            $lift = $baseTile * 0.30 * $scale[$i]     # 越靠中间抬得越高
            $tile = [System.Drawing.RectangleF]::new($x, $bottom - $w - $lift, $w, $w)
            $tp = New-RoundRect -R $tile -Radius ($w * 0.30)
            if ($i -eq 2) {
                $col = [System.Drawing.Color]::FromArgb(255, 10, 132, 255)
            } elseif ($i -eq 1 -or $i -eq 3) {
                $col = [System.Drawing.Color]::FromArgb(225, 255, 255, 255)
            } else {
                $col = [System.Drawing.Color]::FromArgb(150, 255, 255, 255)
            }
            $tb = [System.Drawing.Drawing2D.LinearGradientBrush]::new($tile, (Get-Lighter -C $col -F 0.22), $col, [single]90)
            $g.FillPath($tb, $tp); $tb.Dispose(); $tp.Dispose()
            $x += $w + $gap
        }
    } finally {
        $path.Dispose(); $g.Dispose()
    }
    return $bmp
}

function Get-IconScaled {
    param([int]$S, [int]$Target)
    $big = New-IconBitmap -S $S
    $small = [System.Drawing.Bitmap]::new($Target, $Target, $script:ARGB)
    $g = [System.Drawing.Graphics]::FromImage($small)
    $g.InterpolationMode = $script:HighQ
    $g.PixelOffsetMode = $script:Pad
    $g.SmoothingMode = $script:AA
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($big, [System.Drawing.Rectangle]::new(0, 0, $Target, $Target))
    $g.Dispose(); $big.Dispose()
    return $small
}

# 32bpp DIB（ICO 里的 BMP 条目）：自底向上的 BGRA 行 + 全 0 的 AND 掩码
#
# ⚠️ 写成"往传入的流里写"而不是"返回 byte[]"：PowerShell 会把函数返回的数组
# **展开**成多个对象，跨函数传字节数组极容易拿到 Object[]（实测 ICO 只写出 118 字节的
# 目录头、payload 全空，就是踩了这个）。
function Write-DibTo {
    param([System.Drawing.Bitmap]$Bmp, [System.IO.Stream]$To)
    $w = $Bmp.Width; $h = $Bmp.Height
    $bw = [System.IO.BinaryWriter]::new($To, [System.Text.Encoding]::UTF8, $true)
    $bw.Write([int]40); $bw.Write([int]$w); $bw.Write([int]($h * 2))
    $bw.Write([int16]1); $bw.Write([int16]32)
    $bw.Write([int]0); $bw.Write([int]($w * $h * 4))
    $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0)
    $rect = [System.Drawing.Rectangle]::new(0, 0, $w, $h)
    $data = $Bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, $script:ARGB)
    try {
        $row = [byte[]]::new($w * 4)
        for ($y = $h - 1; $y -ge 0; $y--) {
            [System.Runtime.InteropServices.Marshal]::Copy([IntPtr]::Add($data.Scan0, $y * $data.Stride), $row, 0, $row.Length)
            $bw.Write($row)
        }
    } finally { $Bmp.UnlockBits($data) }
    $maskRow = [int](([Math]::Floor(($w + 31) / 32)) * 4)
    $bw.Write([byte[]]::new($maskRow * $h))
    $bw.Flush()
}

# 多尺寸 ICO：小尺寸用 BMP（兼容性最好），128/256 用 PNG（体积小很多）
function Write-Ico {
    param([string]$Path, [int[]]$Sizes, [int]$SourceSize)
    $streams = @(); $metas = @()
    foreach ($s in $Sizes) {
        $bmp = Get-IconScaled -S $SourceSize -Target $s
        $ms = [System.IO.MemoryStream]::new()
        try {
            if ($s -ge 128) { $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png) }
            else { Write-DibTo -Bmp $bmp -To $ms }
        } finally { $bmp.Dispose() }
        $metas += [pscustomobject]@{ Size = $s; Len = [int]$ms.Length }
        $streams += , $ms
    }
    $fs = [System.IO.FileStream]::new($Path, [System.IO.FileMode]::Create)
    $bw = [System.IO.BinaryWriter]::new($fs)
    try {
        $bw.Write([int16]0); $bw.Write([int16]1); $bw.Write([int16]$metas.Count)
        $offset = 6 + 16 * $metas.Count
        foreach ($m in $metas) {
            $dim = if ($m.Size -ge 256) { 0 } else { $m.Size }
            $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
            $bw.Write([int16]1); $bw.Write([int16]32)
            $bw.Write([int]$m.Len); $bw.Write([int]$offset)
            $offset += $m.Len
        }
        foreach ($ms in $streams) {
            $bw.Write([byte[]]$ms.ToArray())
            $ms.Dispose()
        }
        $bw.Flush()
    } finally { $bw.Dispose(); $fs.Dispose() }
}

$iconsDir = Join-Path $RepoRoot "app\src-tauri\icons"
$docsDir = Join-Path $RepoRoot "docs\images"
New-Item -ItemType Directory -Force -Path $iconsDir, $docsDir | Out-Null

Write-Host "生成 $Size×$Size 主图…"
$main = New-IconBitmap -S $Size
$main.Save((Join-Path $iconsDir "icon.png"), [System.Drawing.Imaging.ImageFormat]::Png)
$main.Dispose()

Write-Host "生成多尺寸 ICO（16/24/32/48/64/128/256）…"
Write-Ico -Path (Join-Path $iconsDir "icon.ico") -Sizes @(16, 24, 32, 48, 64, 128, 256) -SourceSize $Size

Write-Host "生成 README 展示用 256×256…"
$show = Get-IconScaled -S $Size -Target 256
$show.Save((Join-Path $docsDir "icon.png"), [System.Drawing.Imaging.ImageFormat]::Png)
$show.Dispose()

Get-ChildItem (Join-Path $iconsDir "icon.png"), (Join-Path $iconsDir "icon.ico"), (Join-Path $docsDir "icon.png") |
  ForEach-Object { Write-Host ("  {0,-44} {1,7:N1} KB" -f $_.FullName.Replace($RepoRoot, ""), ($_.Length / 1KB)) }
