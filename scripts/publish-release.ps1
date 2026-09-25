# 发布一个 GitHub Release（幂等，可重复跑）
#
# 为什么要有这个脚本：Release 的创建/资产上传要调 GitHub API，而**手工在命令行里拼
# 这些调用反复踩坑**（实测：令牌提取带 CR、PATCH 偶发 401、upload_url 里的
# `{?name,label}` 模板没去掉导致 "hostname could not be parsed"）。收敛成一个脚本，
# 每步都自检，失败就明确报出来。
#
# 令牌来源：本机 Git Credential Manager 里已缓存的 github.com 凭据（`git credential fill`）。
# **脚本不打印令牌**，也不把它写到任何文件里。
#
# 用法：
#   pwsh -File scripts/publish-release.ps1 -Tag v1.0.1 -ReleaseName "Dock v1.0.1" `
#        -NotesFile release-notes.md -AssetDir app\src-tauri\target\release\bundle
#
# 行为：
#   1. Release 不存在就创建，已存在就**只更新正文**（不重复创建）
#   2. 资产按文件名去重上传（已存在的跳过），所以重跑安全
#   3. 每一步都打印 HTTP 结果；任何一步失败以非 0 退出

param(
  [Parameter(Mandatory = $true)][string]$Tag,
  [string]$ReleaseName = $Tag,
  [string]$NotesFile,
  [string]$Notes,
  [Parameter(Mandatory = $true)][string]$AssetDir,
  [string]$AssetPattern = "Dock_*",
  [string]$Repo = "EvanLofton/MyDock",
  [string]$RepoRoot = (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
)

$ErrorActionPreference = "Stop"

function Get-GitHubToken {
  # ⚠️ `git credential fill` 的输出在 Windows 上是 CRLF；而且它可能同时吐多行，
  # 所以按"找 password= 前缀"的方式取，并对结果做一次自检（下面会验令牌）。
  $raw = "protocol=https`nhost=github.com`n`n" | git credential fill 2>&1
  $pw = $null
  foreach ($line in @($raw)) {
    $s = [string]$line
    $i = $s.IndexOf("password=")
    if ($i -ge 0) { $pw = $s.Substring($i + 9).Trim() }
  }
  if (-not $pw -or $pw.Length -lt 10) { throw "没能从 Git Credential Manager 取到 github.com 的令牌（先在命令行 git push 一次即可缓存）" }
  return $pw
}

$token = Get-GitHubToken
$H = @{ Authorization = "Bearer $token"; "User-Agent" = "dock-release"; Accept = "application/vnd.github+json" }

Write-Host "1) 校验令牌…"
$me = Invoke-RestMethod "https://api.github.com/user" -Headers $H -TimeoutSec 30
Write-Host ("   ✓ 已认证为 " + $me.login)

# 正文：优先文件，其次内联文本
$body = ""
if ($NotesFile) {
  $p = if (Test-Path $NotesFile) { $NotesFile } else { Join-Path $RepoRoot $NotesFile }
  if (-not (Test-Path $p)) { throw "找不到说明文件 $p" }
  $body = [System.IO.File]::ReadAllText((Resolve-Path $p), [System.Text.Encoding]::UTF8)
} elseif ($Notes) { $body = $Notes }

Write-Host "2) Release（$Tag）…"
$rel = $null
try { $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/tags/$Tag" -Headers $H -TimeoutSec 30 } catch { $rel = $null }
if ($rel) {
  Write-Host ("   已存在（id=" + $rel.id + "），更新正文")
  $payload = @{ name = $ReleaseName; body = $body } | ConvertTo-Json -Depth 4
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/$($rel.id)" -Method Patch `
    -Headers $H -ContentType "application/json; charset=utf-8" -Body ([System.Text.Encoding]::UTF8.GetBytes($payload)) -TimeoutSec 60
} else {
  Write-Host "   不存在，创建"
  $payload = @{ tag_name = $Tag; name = $ReleaseName; body = $body; draft = $false; prerelease = $false } | ConvertTo-Json -Depth 4
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -Method Post `
    -Headers $H -ContentType "application/json; charset=utf-8" -Body ([System.Text.Encoding]::UTF8.GetBytes($payload)) -TimeoutSec 60
}
Write-Host ("   ✓ " + $rel.html_url)

Write-Host "3) 上传资产（已存在的跳过）…"
# ⚠️ upload_url 长这样：https://uploads.github.com/.../assets{?name,label}
#    必须把 `{?name,label}` 模板切掉，否则拼出来的 URL 解析不了（实测踩过）
$upload = ($rel.upload_url -split "\{")[0]
$have = @{}
foreach ($a in @($rel.assets)) { $have[$a.name] = $a }
$assets = Get-ChildItem -Path (Join-Path $RepoRoot $AssetDir) -Recurse -File -Filter $AssetPattern |
  Where-Object { $_.Extension -in ".exe", ".msi", ".zip" }
if (-not $assets) { throw "在 $AssetDir 下没找到匹配 $AssetPattern 的资产" }
foreach ($f in $assets) {
  if ($have.ContainsKey($f.Name)) { Write-Host ("   - 跳过（已存在） " + $f.Name); continue }
  $uri = $upload + "?name=" + [uri]::EscapeDataString($f.Name)
  $r = Invoke-RestMethod -Uri $uri -Method Post -Headers $H -ContentType "application/octet-stream" -InFile $f.FullName -TimeoutSec 900
  Write-Host ("   ✓ " + $r.name + "  " + [math]::Round($r.size / 1MB, 2) + " MB")
}

Write-Host "4) 完成。SHA256（贴进说明里用）："
foreach ($f in $assets) {
  Write-Host ("   " + (Get-FileHash $f.FullName -Algorithm SHA256).Hash + "  " + $f.Name)
}
