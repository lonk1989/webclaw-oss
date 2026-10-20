# 跑菜谱封装
param(
  [string]$Recipe = "",
  [switch]$Stdin,
  [string]$Title = "skill",
  [string]$ParamsJson = ""
)

$HomeDir = if ($env:WEBCLAW_HOME) { $env:WEBCLAW_HOME } else { Join-Path $env:LOCALAPPDATA "webclaw" }
$Lite = Join-Path $HomeDir "bin\webclaw-lite.exe"
if (-not (Test-Path $Lite)) { Write-Error "未找到 $Lite，请先 install"; exit 2 }

$args = @("run", "--title", $Title)
if ($ParamsJson) { $args += @("--params", $ParamsJson) }
if ($Stdin) {
  $args += "--stdin"
  & $Lite @args
} elseif ($Recipe) {
  $args += @("--recipe", $Recipe)
  & $Lite @args
} else {
  Write-Error "需要 -Recipe 或 -Stdin"
  exit 2
}
exit $LASTEXITCODE
