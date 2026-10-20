# Local acceptance: install -> status -> human-reply -> run (UTF-8 recipe)
$ErrorActionPreference = "Stop"
$Lite = $args[0]
if (-not $Lite) {
  $Lite = Join-Path (Split-Path (Split-Path $PSScriptRoot)) "target\release\webclaw-lite.exe"
}
if (-not (Test-Path $Lite)) { throw "missing webclaw-lite.exe: $Lite" }

$wcHome = Join-Path $env:TEMP ("webclaw-lite-accept-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$env:WEBCLAW_HOME = $wcHome
Write-Host "HOME=$wcHome"

& $Lite install | Out-Host
& $Lite status | Out-Host

$runDir = Join-Path $wcHome "runs\hitl-test"
$human = Join-Path $runDir "human"
New-Item -ItemType Directory -Force -Path $human | Out-Null
'{"promptId":"p1","taskId":"t1","kind":"image","hint":"test"}' | Set-Content (Join-Path $human "waiting_human.json") -Encoding utf8
& $Lite human-reply --run-dir $runDir --text "TESTCODE" | Out-Host
$reply = Get-Content (Join-Path $human "human_reply.txt") -Raw
if ($reply.Trim() -ne "TESTCODE") { throw "human-reply mismatch: $reply" }

$recipe = Join-Path $wcHome "smoke.webclaw"
[System.IO.File]::WriteAllText($recipe, "打开https://example.com，等待2", [System.Text.UTF8Encoding]::new($false))
$out = Join-Path $wcHome "out.jsonl"
$err = Join-Path $wcHome "err.txt"
$p = Start-Process -FilePath $Lite -ArgumentList @("run", "--recipe", $recipe, "--title", "accept") -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
if (-not $p.WaitForExit(120000)) {
  $p.Kill()
  throw "run timeout"
}
$raw = Get-Content $out -Raw
if ($raw -notmatch '"type":"done"') { throw "no done event" }
if ($raw -notmatch '"fail":0') { throw "run failed: $raw" }
Write-Host "ACCEPT_OK exit=$($p.ExitCode)"
