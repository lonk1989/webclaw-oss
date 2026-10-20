# Sync Skill + install scripts into static/webclaw-lite for static hosting.
# Usage: .\scripts\stage_lite_static.ps1 [-WithBinary <path-to-exe>]
param(
  [string]$WithBinary = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Skill = Join-Path $Root "skills\webclaw-lite"
$Out = Join-Path $Root "static\webclaw-lite"
$Release = Join-Path $Out "release"

New-Item -ItemType Directory -Force -Path $Release | Out-Null

Copy-Item -Force (Join-Path $Skill "SKILL.md") $Out
Copy-Item -Force (Join-Path $Skill "CLI.md") $Out
Copy-Item -Force (Join-Path $Skill "MOUNT.md") $Out
Copy-Item -Force (Join-Path $Skill "README.md") $Out
Copy-Item -Force (Join-Path $Skill "scripts\install.ps1") (Join-Path $Out "install.ps1")
Copy-Item -Force (Join-Path $Skill "scripts\install.sh") (Join-Path $Out "install.sh")

$Ver = "0.0.0"
$Cargo = Join-Path $Root "src-tauri\Cargo.toml"
if (Test-Path $Cargo) {
  $m = Select-String -Path $Cargo -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
  if ($m) { $Ver = $m.Matches.Groups[1].Value }
}

$sha = ""
$size = 0
$file = "webclaw-lite-windows-x64.exe"

if ($WithBinary -and (Test-Path $WithBinary)) {
  $dest = Join-Path $Release $file
  Copy-Item -Force $WithBinary $dest
  $sha = (Get-FileHash -Path $dest -Algorithm SHA256).Hash.ToLowerInvariant()
  $size = (Get-Item $dest).Length
  $hashFile = Join-Path $Release ($file + ".sha256")
  Set-Content -Path $hashFile -Value ("{0}  {1}" -f $sha, $file) -Encoding ascii
  Write-Host "[stage] binary -> $dest"
}

$publishedAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
$latestObj = [ordered]@{
  version     = $Ver
  notes       = "webclaw-lite local agent (Skill/CLI)"
  publishedAt = $publishedAt
  platform    = "windows"
  arch        = "x86_64"
  file        = $file
  sha256      = $sha
  size        = $size
}
$latestPath = Join-Path $Release "latest.json"
$latestObj | ConvertTo-Json | Set-Content -Path $latestPath -Encoding utf8

$gitkeep = Join-Path $Release ".gitkeep"
if (-not (Test-Path $gitkeep)) {
  New-Item -ItemType File -Path $gitkeep -Force | Out-Null
}

Write-Host "[stage] -> $Out"
Write-Host "[stage] upload static/webclaw-lite to /agent/webclaw-server/webclaw-lite/"
