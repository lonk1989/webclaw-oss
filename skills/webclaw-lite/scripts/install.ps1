# Requires WEBCLAW_LITE_BASE (release static prefix) or WEBCLAW_WORKER_URL (full exe URL).
$ErrorActionPreference = "Stop"
$base = $env:WEBCLAW_LITE_BASE
$url = $env:WEBCLAW_WORKER_URL
if (-not $url) {
  if (-not $base) { throw "Set WEBCLAW_LITE_BASE or WEBCLAW_WORKER_URL before install." }
  $url = ($base.TrimEnd("/") + "/webclaw-lite-windows-x64.exe")
}
$homeDir = if ($env:WEBCLAW_HOME) { $env:WEBCLAW_HOME } else { Join-Path $env:LOCALAPPDATA "webclaw" }
$bin = Join-Path $homeDir "bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null
$dest = Join-Path $bin "webclaw-lite.exe"
Write-Host "Downloading $url -> $dest"
Invoke-WebRequest -Uri $url -OutFile $dest
& $dest install
