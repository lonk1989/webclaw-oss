# 构建本机轻客户端并 stage 到 static/webclaw-lite
# 用法: .\scripts\build_lite_windows.ps1
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$SrcTauri = Join-Path $Root "src-tauri"

Push-Location $SrcTauri
try {
  Write-Host "[build] cargo build --release --bin webclaw-lite --no-default-features"
  cargo build --release --bin webclaw-lite --no-default-features
} finally {
  Pop-Location
}

# workspace target 可能在 src-tauri 或 apps/vision-coord-client 或 monorepo 根
$candidates = @(
  (Join-Path $SrcTauri "target\release\webclaw-lite.exe"),
  (Join-Path $Root "target\release\webclaw-lite.exe"),
  (Join-Path (Split-Path $Root) "target\release\webclaw-lite.exe"),
  (Join-Path (Split-Path (Split-Path $Root)) "target\release\webclaw-lite.exe")
)
$Exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $Exe) {
  Write-Error "找不到 webclaw-lite.exe，请检查 cargo target 目录"
  exit 1
}
Write-Host "[build] found $Exe"
& (Join-Path $PSScriptRoot "stage_lite_static.ps1") -WithBinary $Exe
