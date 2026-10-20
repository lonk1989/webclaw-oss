# 人机回填封装
param(
  [string]$RunDir = "",
  [string]$Text = "",
  [switch]$Ok,
  [switch]$Cancel
)

$HomeDir = if ($env:WEBCLAW_HOME) { $env:WEBCLAW_HOME } else { Join-Path $env:LOCALAPPDATA "webclaw" }
$Lite = Join-Path $HomeDir "bin\webclaw-lite.exe"
if (-not (Test-Path $Lite)) { Write-Error "未找到 $Lite，请先 install"; exit 2 }

$args = @("human-reply")
if ($RunDir) { $args += @("--run-dir", $RunDir) }
if ($Cancel) { $args += "--cancel" }
elseif ($Ok) { $args += "--ok" }
elseif ($Text) { $args += @("--text", $Text) }
else { Write-Error "需要 -Text / -Ok / -Cancel"; exit 2 }

& $Lite @args
exit $LASTEXITCODE
