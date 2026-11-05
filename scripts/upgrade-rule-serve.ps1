# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — governance rule-serve deployment kit: in-place upgrade.
#
# Replaces the server executable with a new build, keeps the old binary as
# .bak (one-click rollback), relies on automatic SQLite migration at startup
# (CREATE TABLE IF NOT EXISTS + tolerant ALTER ADD COLUMN), verifies health
# and data survival. Downtime is kept to seconds.
#
# A pre-upgrade database snapshot is taken automatically (fast local copy)
# before the swap.
#
# Usage:
#   .\scripts\upgrade-rule-serve.ps1 -NewExePath <path-to-new-exe> [-NewConsoleDir <dir>]
#   .\scripts\upgrade-rule-serve.ps1 -Rollback      # restore the .bak binary (+ console.bak)
#
param(
  [string]$NewExePath = '',
  # Optional: console frontend build output directory (base path /console/).
  # Swapped into <DeployRoot>\console within the same downtime window; the
  # previous assets are kept as console.bak (restored by -Rollback).
  [string]$NewConsoleDir = '',
  [switch]$Rollback,
  [string]$DeployRoot = (Join-Path 'D:\' 'evorule-deploy'),
  [int]$Port = 18081,
  [string]$TaskName = 'EvoRuleRuleServe',
  [int]$HealthTimeoutSec = 90
)

$ErrorActionPreference = 'Stop'
$Base = "http://127.0.0.1:$Port"
$exePath = Join-Path $DeployRoot 'evorule-rule-serve.exe'
$bakPath = "$exePath.bak"
$dbPath = Join-Path $DeployRoot 'rule-serve\data\rule.db'
$backupDir = Join-Path $DeployRoot 'backups'

function Wait-Health([int]$TimeoutSec) {
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    try {
      $r = Invoke-WebRequest -Uri "$Base/v1/health" -UseBasicParsing -TimeoutSec 5
      if ($r.StatusCode -eq 200) { return $true }
    } catch { Start-Sleep -Milliseconds 500 }
  }
  return $false
}

function Stop-ResidentTask {
  # Kill by executable path (unique per deployment root): scheduled-task stop
  # alone is unreliable (process can outlive the task state or be respawned by
  # restart settings).
  $deadline = (Get-Date).AddSeconds(15)
  do {
    Get-CimInstance Win32_Process -Filter "Name='evorule-rule-serve.exe'" |
      Where-Object { $_.ExecutablePath -eq $exePath } |
      ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    $still = @(Get-CimInstance Win32_Process -Filter "Name='evorule-rule-serve.exe'" |
      Where-Object { $_.ExecutablePath -eq $exePath })
  } while ($still.Count -gt 0 -and (Get-Date) -lt $deadline)
  Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
}

function Start-ResidentTask {
  Start-ScheduledTask -TaskName $TaskName
  if (-not (Wait-Health $HealthTimeoutSec)) { throw "instance not healthy on $Base within ${HealthTimeoutSec}s" }
  Write-Host "    resident instance healthy: $Base/v1/health"
}

if ($Rollback) {
  if (-not (Test-Path $bakPath)) { throw "no rollback binary found: $bakPath" }
  Write-Host '[rollback] stopping task'
  Stop-ResidentTask
  Copy-Item $bakPath $exePath -Force
  Write-Host "[rollback] previous binary restored: $bakPath -> $exePath"
  $consoleBak = Join-Path $DeployRoot 'console.bak'
  $consoleDir = Join-Path $DeployRoot 'console'
  if (Test-Path $consoleBak) {
    if (Test-Path $consoleDir) { Remove-Item $consoleDir -Recurse -Force }
    Rename-Item $consoleBak 'console'
    Write-Host '[rollback] previous console assets restored'
  }
  Start-ResidentTask
  Write-Host '[rollback] done. NOTE: a database written by a newer build may carry schema additions; the older binary tolerates unknown columns but a pre-upgrade snapshot should be restored if behavior regresses.'
  exit 0
}

if ($NewExePath -eq '') { throw 'either -NewExePath or -Rollback is required' }
if (-not (Test-Path $NewExePath)) { throw "NewExePath not found: $NewExePath" }
if (-not (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue)) {
  throw "scheduled task $TaskName not found; run deploy-rule-serve.ps1 first"
}

# ---- 1. pre-upgrade snapshot (fast local copy, includes WAL sidecars) ------
New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$snap = Join-Path $backupDir "pre-upgrade-$stamp.db"
Copy-Item $dbPath $snap -Force
foreach ($suffix in @('-wal', '-shm')) {
  if (Test-Path "$dbPath$suffix") { Copy-Item "$dbPath$suffix" "$snap$suffix" -Force }
}
Write-Host "[1] pre-upgrade snapshot: $snap"

# ---- 2. stop, swap binaries (keep .bak), restart ----------------------------
Write-Host '[2] stopping resident task'
Stop-ResidentTask
Copy-Item $exePath $bakPath -Force
Copy-Item $NewExePath $exePath -Force
Write-Host "[2] binary swapped (previous kept at $bakPath)"

# ---- 2.5 console assets swap + task command line (same downtime window) -----
$consoleDir = Join-Path $DeployRoot 'console'
$consoleBak = Join-Path $DeployRoot 'console.bak'
if ($NewConsoleDir -ne '') {
  if (-not (Test-Path $NewConsoleDir)) { throw "NewConsoleDir not found: $NewConsoleDir" }
  if (Test-Path $consoleBak) { Remove-Item $consoleBak -Recurse -Force }
  if (Test-Path $consoleDir) { Rename-Item $consoleDir 'console.bak' }
  Copy-Item $NewConsoleDir $consoleDir -Recurse -Force
  Write-Host "[2.5] console assets swapped (previous kept at $consoleBak)"
}
if (Test-Path $consoleDir) {
  # Ensure the resident command line carries --console-dir (deploy-registered
  # tasks from before the static face lack it). Same zero-credential shape.
  $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($task -and $task.Actions[0].Arguments -notmatch '--console-dir') {
    $newArgs = $task.Actions[0].Arguments + " --console-dir `"$consoleDir`""
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
      -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable
    $action = New-ScheduledTaskAction -Execute $exePath -Argument $newArgs -WorkingDirectory $DeployRoot
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings | Out-Null
    Write-Host '[2.5] resident task re-registered with --console-dir (zero credentials)'
  }
}

# ---- 3. start + health (schema migration runs automatically at startup) ----
Write-Host '[3] starting upgraded instance (automatic schema migration on startup)'
Start-ResidentTask

# ---- 4. data survival check -------------------------------------------------
$pwPath = Join-Path $DeployRoot 'bootstrap-pw.txt'
if (Test-Path $pwPath) {
  $adminPassword = (Get-Content $pwPath -Raw).Trim()
  try {
    $login = Invoke-RestMethod -Method Post -Uri "$Base/v1/auth/login" -ContentType 'application/json' `
      -Body (@{ tenant_id = 'default'; username = 'admin'; password = $adminPassword } | ConvertTo-Json -Compress)
    $ds = Invoke-RestMethod -Method Get -Uri "$Base/v1/datasets/companion-governance" -Headers @{ Authorization = "Bearer $($login.access_token)" }
    Write-Host "[4] data survival OK: companion-governance present (status=$($ds.lifecycle.status), version=$($ds.versioning.current))"
  } catch {
    Write-Host "[4] WARNING: post-upgrade data check failed: $($_.Exception.Message)"
    Write-Host '    verify manually; rollback available via -Rollback'
  }
} else {
  Write-Host '[4] bootstrap credential file missing, skipping data survival check (health only)'
}

Write-Host ''
Write-Host '===== upgrade summary ====='
Write-Host "binary:      $exePath (previous: $bakPath)"
if ($NewConsoleDir -ne '') { Write-Host "console:     $consoleDir (previous: $consoleBak)" }
elseif (Test-Path $consoleDir) { Write-Host "console:     $consoleDir (unchanged)" }
Write-Host "snapshot:    $snap"
Write-Host "endpoint:    $Base (health OK)"
Write-Host 'rollback:    .\scripts\upgrade-rule-serve.ps1 -Rollback'
Write-Host '==========================='
