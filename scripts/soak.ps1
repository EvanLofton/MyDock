# 长稳采样：每 N 秒记录 Dock 本体 + 它的 WebView2 子进程的内存 / 句柄 / 线程 / CPU。
#
# 为什么需要脚本：验收标准里有一条「连续运行 8 小时不崩」，而这一条**只能靠真实流逝的时间**
# （会话里跑不出来）。这个脚本负责把证据留下来，你正常用电脑就行 —— 空闲态采样是关键，
# 但真实使用（点图标、开菜单、展开文件夹）本身就是最好的负载。
#
# 用法：
#   pwsh -File scripts/soak.ps1                 # 后台常驻，每 120 秒一行，追加到 docs/soak-log.txt
#   pwsh -File scripts/soak.ps1 -Once           # 只采一次并打印（自检这个脚本本身）
#   pwsh -File scripts/soak.ps1 -IntervalSeconds 60
#
# 停止：关掉那个 pwsh 进程即可（PID 写在 .soak-pid）。
#
# ⚠️ 采样本身要便宜：`Get-CimInstance Win32_Process` 一次约几十毫秒，只在采样时调用；
#    睡眠期间零开销（不用轮询）。

param(
  [int]$IntervalSeconds = 120,
  [switch]$Once,
  [string]$LogPath,
  # 连续多少次看不到 Dock 就收工（默认 30 次 = 约 1 小时）。期间会照常记"进程不在"。
  [int]$MaxMisses = 30
)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $LogPath) { $LogPath = Join-Path $root "docs\soak-log.txt" }

function Get-DockTree {
  param([int]$RootPid)
  # Dock 本体 + 它的后代进程（WebView2 的 browser/gpu/renderer 都是它的子孙）
  $all = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Select-Object ProcessId, ParentProcessId, Name, WorkingSetSize
  $byParent = @{}
  foreach ($p in $all) {
    if (-not $byParent.ContainsKey([int]$p.ParentProcessId)) { $byParent[[int]$p.ParentProcessId] = @() }
    $byParent[[int]$p.ParentProcessId] += $p
  }
  $out = New-Object System.Collections.ArrayList
  $queue = New-Object System.Collections.Queue
  $queue.Enqueue($RootPid)
  $seen = @{}
  while ($queue.Count -gt 0) {
    $pid0 = [int]$queue.Dequeue()
    if ($seen.ContainsKey($pid0)) { continue }
    $seen[$pid0] = $true
    if ($byParent.ContainsKey($pid0)) {
      foreach ($c in $byParent[$pid0]) {
        [void]$out.Add($c)
        $queue.Enqueue([int]$c.ProcessId)
      }
    }
  }
  return $out
}

function Sample {
  $dock = Get-Process dock-app -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $dock) { return $null }
  $tree = Get-DockTree -RootPid $dock.Id
  $ws = $dock.WorkingSet64 / 1MB
  $pv = $dock.PrivateMemorySize64 / 1MB
  $children = 0; $childWs = 0.0; $childPv = 0.0
  foreach ($c in $tree) {
    if ($c.Name -eq 'dock-app.exe') { continue }   # 跳过本体
    $children++
    $childWs += $c.WorkingSetSize / 1MB
    $cp = Get-Process -Id $c.ProcessId -ErrorAction SilentlyContinue
    if ($cp) { $childPv += $cp.PrivateMemorySize64 / 1MB }
  }
  [pscustomobject]@{
    Time     = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss")
    Pid      = $dock.Id
    WsMB     = [math]::Round($ws + $childWs, 1)
    PvtMB    = [math]::Round($pv + $childPv, 1)
    Handles  = $dock.HandleCount
    Threads  = $dock.Threads.Count
    Children = $children
    CpuSec   = [math]::Round($dock.TotalProcessorTime.TotalSeconds, 1)
  }
}

if ($Once) {
  $s = Sample
  if (-not $s) { Write-Host "Dock 没在跑" -ForegroundColor Red; exit 1 }
  $s | Format-List
  exit 0
}

if (-not (Test-Path $LogPath)) {
  "# 长稳测试（每 $IntervalSeconds 秒采样一次）" | Out-File $LogPath -Encoding utf8
  "# 构建：$(Get-Date -Format 'yyyy-MM-dd HH:mm') 起的当前构建；负载：日常使用（空闲 + 手动操作）" | Out-File $LogPath -Append -Encoding utf8
  "# 列：时间,私有MB(本体+WebView2),工作集MB,句柄,线程,子进程数,累计CPU秒" | Out-File $LogPath -Append -Encoding utf8
} else {
  "" | Out-File $LogPath -Append -Encoding utf8
  "# ---- 新一段采样（$IntervalSeconds 秒/次）$(Get-Date -Format 'yyyy-MM-dd HH:mm') ----" | Out-File $LogPath -Append -Encoding utf8
}

$me = $PID
Set-Content (Join-Path $root ".soak-pid") $me
Write-Host "采样器 PID $me，每 $IntervalSeconds 秒写一行到 $LogPath"

$first = $null
$misses = 0
while ($true) {
  $s = Sample
  if (-not $s) {
    # ⚠️ **不能退出**：Dock 重启（改配置 / 重新构建 / 我们自己 kill 它）之间有几百毫秒到几秒的空档，
    # 早期版本在这里 break，于是每次重启都把长稳测试掐断（实测踩到：日志里那句"进程已退出"）。
    # 改成"记一行、继续等"，连续太多次才收工。
    $misses++
    "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss'),进程不在（等待重启，第 $misses 次）,,,,,," |
      Out-File $LogPath -Append -Encoding utf8
    if ($misses -ge $MaxMisses) {
      Write-Host "连续 $misses 次没看到 Dock（约 $([math]::Round($misses * $IntervalSeconds / 60)) 分钟），采样结束"
      break
    }
    Start-Sleep -Seconds $IntervalSeconds
    continue
  }
  if ($misses -gt 0) {
    "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss'),（Dock 回来了，前面空档 $misses 次）,,,,,," |
      Out-File $LogPath -Append -Encoding utf8
    $misses = 0
    $first = $null   # 重启后 CPU 基准要重取，否则会把两次运行的差值当成一次
  }
  # ⚠️ CPU 这一列必须是**每区间**的增量 —— `$first` 每次循环都要更新。
  # 早期版本忘了更新，于是这一列是从头累计的（越跑越大，看不出趋势）。实测踩到。
  $delta = ""
  if ($first) {
    $delta = "$([math]::Round(($s.CpuSec - $first.CpuSec) / $IntervalSeconds * 100, 2))%"
  }
  $first = $s
  "$($s.Time),$($s.PvtMB),$($s.WsMB),$($s.Handles),$($s.Threads),$($s.Children),$($s.CpuSec),$delta" |
    Out-File $LogPath -Append -Encoding utf8
  Start-Sleep -Seconds $IntervalSeconds
}
