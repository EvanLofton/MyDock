# 生成 GitHub 的 Social preview（1280×640 卡片图）
#
# 为什么要有它：GitHub 仓库设置里那张 "Social preview" 是分享链接时的缩略图，
# 不设的话就是灰底默认图。它**只能在网页上手动上传**（API 要 token），
# 所以这里只负责把图画好：scripts/make-social-preview.ps1 → docs/images/social-preview.png
#
# 素材全部来自仓库自身：docs/images/icon.png（自己画的图标）+ docs/images/dock.png（实拍截图），
# 没有引入任何第三方素材。
#
# 用法： pwsh -File scripts/make-social-preview.ps1

param(
  [string]$RepoRoot = (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Drawing.Common -ErrorAction SilentlyContinue

$W = 1280; $H = 640
$bmp = [System.Drawing.Bitmap]::new($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit

try {
    # 背景：深色渐变（和图标、Dock 面板同一套配色）+ 右上角蓝色柔光
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        [System.Drawing.Rectangle]::new(0, 0, $W, $H),
        [System.Drawing.Color]::FromArgb(255, 22, 24, 30),
        [System.Drawing.Color]::FromArgb(255, 12, 13, 17), [single]115)
    $g.FillRectangle($bg, 0, 0, $W, $H); $bg.Dispose()

    $glowPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $glowPath.AddEllipse(760, -260, 900, 700)
    $pg = [System.Drawing.Drawing2D.PathGradientBrush]::new($glowPath)
    $pg.CenterColor = [System.Drawing.Color]::FromArgb(90, 10, 132, 255)
    $pg.SurroundColors = [System.Drawing.Color[]]@([System.Drawing.Color]::FromArgb(0, 10, 132, 255))
    $g.FillPath($pg, $glowPath); $pg.Dispose(); $glowPath.Dispose()

    # 左侧：图标
    $iconPath = Join-Path $RepoRoot "docs\images\icon.png"
    $icon = [System.Drawing.Image]::FromFile($iconPath)
    $g.DrawImage($icon, [System.Drawing.Rectangle]::new(80, 86, 148, 148))
    $icon.Dispose()

    $yahei = "Microsoft YaHei UI"
    $title = [System.Drawing.Font]::new($yahei, 60, [System.Drawing.FontStyle]::Bold)
    $sub = [System.Drawing.Font]::new($yahei, 24)
    $body = [System.Drawing.Font]::new($yahei, 21)
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 245, 246, 250))
    $grey = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 165, 172, 186))
    $blue = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 10, 132, 255))

    $g.DrawString("Dock", $title, $white, 250, 78)
    $g.DrawString("Windows 11 上的 macOS 风格程序坞", $sub, $grey, 254, 178)

    # 三条要点（对应 README 里最想让人看到的三件事）
    $lines = @(
        "毛玻璃 · 悬停鱼眼放大 · 右键菜单（独立窗口）· 大文件夹",
        "拖拽排序 · 拖出即移除 · 自动隐藏 · 系统托盘 · 配置持久化",
        "与任务栏共存：按 rcWork 留空，零重叠"
    )
    $y = 250
    foreach ($l in $lines) {
        $g.FillEllipse($blue, 82, $y + 10, 7, 7)
        $g.DrawString($l, $body, $grey, 100, $y)
        $y += 38
    }

    # 底部：实拍截图（按宽度缩放），左上加一条蓝色装饰线强调"这是真东西"
    $shotPath = Join-Path $RepoRoot "docs\images\dock.png"
    $shot = [System.Drawing.Image]::FromFile($shotPath)
    $targetW = 1120
    $targetH = [int]($shot.Height * $targetW / $shot.Width)
    $g.FillRectangle($blue, 80, 392, 56, 4)
    $g.DrawImage($shot, [System.Drawing.Rectangle]::new(80, 420, $targetW, $targetH))
    $shot.Dispose()

    $title.Dispose(); $sub.Dispose(); $body.Dispose()
    $white.Dispose(); $grey.Dispose(); $blue.Dispose()
} finally {
    $g.Dispose()
}

$out = Join-Path $RepoRoot "docs\images\social-preview.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host ("  已生成 {0}  ({1:N0} KB)" -f $out.Replace($RepoRoot, ""), ((Get-Item $out).Length / 1KB))
Write-Host "  GitHub 上传位置：仓库 → Settings → Social preview → Upload an image"
