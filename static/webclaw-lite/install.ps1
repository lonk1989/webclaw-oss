# Install webclaw-lite from a Release/static prefix (or full exe URL).
# Env:
#   WEBCLAW_LITE_BASE  — directory prefix (default: GitHub Releases latest)
#   WEBCLAW_WORKER_URL — full .exe URL (wins over base)
#   WEBCLAW_HOME       — data root (default %LOCALAPPDATA%\webclaw)
$ErrorActionPreference = "Stop"
$base = $env:WEBCLAW_LITE_BASE
$url = $env:WEBCLAW_WORKER_URL
if (-not $url) {
  if (-not $base) {
    $base = "https://github.com/lonk1989/webclaw-oss/releases/latest/download"
  }
  $url = ($base.TrimEnd("/") + "/webclaw-lite-windows-x64.exe")
}
$homeDir = if ($env:WEBCLAW_HOME) { $env:WEBCLAW_HOME } else { Join-Path $env:LOCALAPPDATA "webclaw" }
$bin = Join-Path $homeDir "bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null
$dest = Join-Path $bin "webclaw-lite.exe"
Write-Host "Downloading $url -> $dest"
Invoke-WebRequest -Uri $url -OutFile $dest
& $dest install
