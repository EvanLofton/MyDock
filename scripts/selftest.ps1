# Dock 自检闸：一条命令跑完整套并给出结论。
#
# 为什么要做成脚本：
#   1. 自检必须在**配置副本**上跑（`DOCK_CONFIG_DIR`），跑完还要核对**真实配置没被改动**；
#   2. 自检要 `--features selftest` 构建，而那个产物会**覆盖** `dock-app.exe` ——
#      所以跑完必须重新做一次产品构建，否则你下次启动的是带脚手架的二进制；
#   3. 跑多久算完不能写死：日志静默一段时间即视为结束（实测整套约 2.5 分钟）。
#
# 用法：
#   pwsh -File scripts/selftest.ps1              # 跑全套（焦点矩阵 + 拖拽 + 菜单/大文件夹 + 系统位置）
#   pwsh -File scripts/selftest.ps1 -SkipBuild   # 已经构建过 selftest 版，直接跑
#
# 退出码：0 = 全过；1 = 有失败项（摘要会打印出来）。

param(
  [switch]$SkipBuild,
  [int]$QuietSeconds = 90,   # 日志静默多久算跑完
  [int]$HardLimitSeconds = 1200
)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root "app\src-tauri\target\debug\dock-app.exe"
$cfgDir = Join-Path $root ".selftest-cfg"
$real = Join-Path $env:APPDATA "io.github.evanlofton.mydock\config.json"
# ⚠️ 配置路径要从 tauri.conf.json 的 identifier 推导，**不要写死** ——
# 改 identifier 那一轮就踩过：脚本还在读旧 identifier 的配置，于是自检测的是另一份
# 数据（那份里的临时文件夹指向老目录），报出"测试结束后列表未还原"这种**假失败**。
$confPath = Join-Path $root "app\src-tauri\tauri.conf.json"
if (Test-Path $confPath) {
  $ident = (Get-Content $confPath -Raw -Encoding UTF8 | ConvertFrom-Json).identifier
  if ($ident) { $real = Join-Path $env:APPDATA "$ident\config.json" }
}
if (-not (Test-Path $real)) {
  # 新版还没跑过（没迁移过）→ 退回旧 identifier 的配置，别让闸直接罢工
  $legacy = Join-Path $env:APPDATA "dev.local.dock\config.json"
  if (Test-Path $legacy) {
    Write-Host "（新版配置还不存在，先用旧 identifier 的：$legacy）" -ForegroundColor DarkGray
    $real = $legacy
  }
}

if (-not (Test-Path $real)) { Write-Host "找不到真实配置 $real" -ForegroundColor Red; exit 1 }

# ---- 0. 环境体检：**有没有窗口盖住 Dock**
#
# 为什么必须查：Dock 是**不置顶**的（需求就是"只出现在桌面上"，见设计文档 §4.1.2），
# 所以一个最大化的浏览器 / 终端就能盖住它。这时自检里所有**需要真实输入**的检查
# （拖拽 / 右键菜单 / 大文件夹）会失败或被整段跳过，而只读 DOM 的检查照样通过 ——
# 那种"假红/假绿"极难查（本轮在这上面栽了两次：一次是任务栏弹出来盖住，
# 一次是一个最大化的 Chrome 窗口）。
# 与其让人对着莫名其妙的失败发愁，不如先体检、直接把原因说出来。
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;using System.Text;
public class COVER {
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(P p);
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h,uint f);
  [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h,StringBuilder s,int n);
  [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h,StringBuilder s,int n);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  public struct P{public int X,Y;}
  public static string At(int x,int y){ SetProcessDPIAware();
    IntPtr h=GetAncestor(WindowFromPoint(new P{X=x,Y=y}),2);
    var c=new StringBuilder(128); GetClassNameW(h,c,128);
    var t=new StringBuilder(256); GetWindowTextW(h,t,256);
    return c+" '"+t+"'"; }
}
'@
# Dock 现在还在跑，量它自己的中心点；量不到 Dock 就跳过体检（比如它本来就没开）
$dockProbe = [COVER]::At(960, 985)
if ((Get-Process dock-app -ErrorAction SilentlyContinue) -and $dockProbe -notmatch "Tauri Window") {
  Write-Host ""
  Write-Host "⚠️  有窗口盖住了 Dock 的位置 —— 需要真实输入的检查会失败或被跳过。" -ForegroundColor Yellow
  Write-Host "    那个点是: $dockProbe" -ForegroundColor Yellow
  Write-Host "    请先最小化它（或按 Win+D 显示桌面）再跑这个闸。" -ForegroundColor Yellow
  Write-Host "    （Dock 不置顶是刻意的，不是 bug。）" -ForegroundColor DarkGray
  exit 2
}

# ---- 0. 先停掉正在跑的 Dock —— 否则 `cargo build` 覆盖不了被占用的 dock-app.exe
Get-Process dock-app -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800

# ---- 1. 构建 selftest 版
if (-not $SkipBuild) {
  Write-Host "== 构建（--features selftest）==" -ForegroundColor Cyan
  Push-Location (Join-Path $root "app\src-tauri")
  cargo build --offline --features selftest 2>&1 | Select-String -Pattern "^error|Finished" | ForEach-Object { $_.Line }
  Pop-Location
  if ($LASTEXITCODE -ne 0) { Write-Host "构建失败" -ForegroundColor Red; exit 1 }
}

# ---- 2. 准备配置副本
Start-Sleep -Milliseconds 400
New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
Copy-Item $real (Join-Path $cfgDir "config.json") -Force
$beforeHash = (Get-FileHash $real -Algorithm SHA256).Hash
Write-Host "真实配置 SHA256（前） $beforeHash"

# 光标先挪开：避免一启动就把任务栏叫出来（会盖住 Dock，让所有需要真实输入的检查失败）
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class CUR{[DllImport("user32.dll")]public static extern bool SetCursorPos(int x,int y);}'
[void][CUR]::SetCursorPos(700, 300)

$env:DOCK_CONFIG_DIR = $cfgDir
$env:DOCK_MENUTEST = "1"
$env:DOCK_DRAGTEST = "1"
$env:DOCK_LOCTEST = "1"
Remove-Item Env:\DOCK_SELFTEST, Env:\DOCK_ICONBOX, Env:\DOCK_MENUSHOT, Env:\DOCK_DEBUG, Env:\DOCK_OPENSETTINGS -ErrorAction SilentlyContinue

$log = Join-Path $cfgDir "stdout.log"
Remove-Item $log -ErrorAction SilentlyContinue
Write-Host "== 启动自检 ==" -ForegroundColor Cyan
$p = Start-Process -FilePath $exe -RedirectStandardOutput $log -PassThru

$last = -1; $still = 0
for ($i = 0; $i -lt $HardLimitSeconds; $i += 5) {
  Start-Sleep -Seconds 5
  if ($p.HasExited) { Write-Host "进程自己退出（第 $i 秒）"; break }
  $len = 0
  if (Test-Path $log) { $len = (Get-Item $log).Length }
  if ($len -eq $last) { $still += 5 } else { $still = 0; $last = $len }
  if ($still -ge $QuietSeconds -and $len -gt 0) { Write-Host "日志静默 $QuietSeconds 秒 → 认为跑完（第 $i 秒）"; break }
}
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
Start-Sleep -Seconds 2

# ---- 3. 结论（按 UTF-8 读日志，别用控制台代码页）
$text = [System.IO.File]::ReadAllText($log, [System.Text.Encoding]::UTF8)
Write-Host "`n== 结果 ==" -ForegroundColor Cyan
($text -split "`n") | Where-Object { $_ -match "\[PASS\]|\[FAIL\]|小结|INVALID|-> 焦点|构建 " } | ForEach-Object { $_ }

$afterHash = (Get-FileHash $real -Algorithm SHA256).Hash
$untouched = $beforeHash -eq $afterHash
Write-Host ("`n真实配置是否被改动: " + $(if ($untouched) { "否（正确）" } else { "是（错误！）" })) -ForegroundColor $(if ($untouched) { "Green" } else { "Red" })

$fails = ($text -split "`n" | Where-Object { $_ -match "\[FAIL\]" }).Count
Write-Host ("失败项: $fails") -ForegroundColor $(if ($fails -eq 0 -and $untouched) { "Green" } else { "Red" })

# ---- 4. 还原产品构建（selftest 版会覆盖 dock-app.exe）
if (-not $SkipBuild) {
  Write-Host "`n== 重新做产品构建（把 selftest 版覆盖掉）==" -ForegroundColor Cyan
  Get-Process dock-app -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Milliseconds 800
  Push-Location (Join-Path $root "app\src-tauri")
  cargo build --offline 2>&1 | Select-String -Pattern "^error|Finished" | ForEach-Object { $_.Line }
  Pop-Location
}

exit $(if ($fails -eq 0 -and $untouched) { 0 } else { 1 })
