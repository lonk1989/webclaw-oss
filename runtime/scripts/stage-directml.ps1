# 打包前把 ort 附带的 DirectML.dll 放进 NSIS 资源。
# 安装器再把它复制到 exe 同目录（LoadLibrary 不会搜 resources\）。
# 仅 Windows：由 tauri.windows.conf.json 的 beforeBundleCommand 调用。
$ErrorActionPreference = "Stop"
$roots = @(
  (Join-Path $PSScriptRoot "..\target\release"),
  (Join-Path $PSScriptRoot "..\..\target\release")
)
$src = $null
foreach ($root in $roots) {
  $p = Join-Path $root "DirectML.dll"
  if (Test-Path -LiteralPath $p) {
    $src = $p
    break
  }
}
if (-not $src) {
  throw "未找到 DirectML.dll。请先 cargo build --release（ort 会把它拷到 target/release）。"
}
$destDir = Join-Path $PSScriptRoot "..\resources"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "DirectML.dll"
Copy-Item -LiteralPath $src -Destination $dest -Force
Write-Host "staged DirectML.dll -> $dest"
