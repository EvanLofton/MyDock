# 生成安装向导用的位图（NSIS 只吃 BMP，尺寸必须严格符合）
#
#   icons/installer-header.bmp   150×57   —— 安装/卸载向导**每个页面**右上角的横幅
#   icons/installer-sidebar.bmp  164×314  —— 欢迎页与完成页左侧的大图
#
# 为什么需要它：不配这两个，NSIS 会画出默认的灰白占位图 —— 用户下载安装包第一眼
# 看到的就是那个"没做完"的样子。素材全部本地生成（图标是 `make-icon.ps1` 画的），
# 不引入任何第三方资源。
#
# 用法： pwsh -File scripts/make-installer-art.ps1

param(
  [string]$RepoRoot = (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$iconsDir = Join-Path $RepoRoot "app\src-tauri\icons"
$iconPath = Join-Path $iconsDir "icon.png"
if (-not (Test-Path $iconPath)) { throw "缺少 $iconPath —— 先跑 scripts/make-icon.ps1" }

function New-Art {
  param([int]$W, [int]$H, [string]$Title, [single]$IconSize, [single]$TitleSize, [string]$Out)
  $bmp = [System.Drawing.Bitmap]::new($W, $H, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  try {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit

    # 深色渐变底（和图标、Dock 面板同一套配色）
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
      [System.Drawing.Rectangle]::new(0, 0, $W, $H),
      [System.Drawing.Color]::FromArgb(255, 26, 29, 36),
      [System.Drawing.Color]::FromArgb(255, 12, 13, 17), [single]90)
    $g.FillRectangle($bg, 0, 0, $W, $H); $bg.Dispose()

    # 右下角一点蓝光，避免整块死黑
    $glow = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $glow.AddEllipse($W - [int]($W * 0.9), [int]($H * 0.45), [int]($W * 1.6), [int]($H * 0.9))
    $pg = [System.Drawing.Drawing2D.PathGradientBrush]::new($glow)
    $pg.CenterColor = [System.Drawing.Color]::FromArgb(70, 10, 132, 255)
    $pg.SurroundColors = [System.Drawing.Color[]]@([System.Drawing.Color]::FromArgb(0, 10, 132, 255))
    $g.FillPath($pg, $glow); $pg.Dispose(); $glow.Dispose()

    # 图标 + 文字（横幅横排，侧栏竖排）
    $icon = [System.Drawing.Image]::FromFile($iconPath)
    $font = [System.Drawing.Font]::new("Microsoft YaHei UI", $TitleSize, [System.Drawing.FontStyle]::Bold)
    $brush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 245, 246, 250))
    if ($W -gt $H) {
      $iy = [int](($H - $IconSize) / 2)
      $g.DrawImage($icon, [System.Drawing.Rectangle]::new(12, $iy, $IconSize, $IconSize))
      $g.DrawString($Title, $font, $brush, (12 + $IconSize + 10), [single](($H - $TitleSize * 1.5) / 2))
    } else {
      $ix = [int](($W - $IconSize) / 2)
      $g.DrawImage($icon, [System.Drawing.Rectangle]::new($ix, 58, $IconSize, $IconSize))
      $fmt = [System.Drawing.StringFormat]::new()
      $fmt.Alignment = [System.Drawing.StringAlignment]::Center
      $rect = [System.Drawing.RectangleF]::new(0, (58 + $IconSize + 16), $W, 40)
      $g.DrawString($Title, $font, $brush, $rect, $fmt)
      $fmt.Dispose()
    }
    $icon.Dispose(); $font.Dispose(); $brush.Dispose()
  } finally { $g.Dispose() }

  # NSIS 的位图要求：24 位、无 alpha（Format24bppRgb 已经满足）
  $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Bmp)
  $bmp.Dispose()
  Write-Host ("  {0,-42} {1}x{2}  {3:N0} KB" -f $Out.Replace($RepoRoot, ""), $W, $H, ((Get-Item $Out).Length / 1KB))
}

New-Art -W 150 -H 57  -Title "Dock" -IconSize 34 -TitleSize 13 -Out (Join-Path $iconsDir "installer-header.bmp")
New-Art -W 164 -H 314 -Title "Dock" -IconSize 96 -TitleSize 22 -Out (Join-Path $iconsDir "installer-sidebar.bmp")
