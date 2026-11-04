# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — governance rule-serve deployment kit: uninstall.
#
# Reverse of deploy: stop and unregister the scheduled tasks, remove the
# deployment assets (executable, service registry copy, credential files).
# The database is only removed with -PurgeData (default: kept). The desktop
# gate copy is NOT touched by default: it is a per-user shared file, not owned
# by a single deployment root. Pass -RemoveGate only when this machine has no
# other rule-serve deployment; armed = gate file present.
#
# Usage:
#   .\scripts\uninstall-rule-serve.ps1 [-Force] [-PurgeData] [-RemoveGate]
#
param(
  [string]$DeployRoot = (Join-Path 'D:\' 'evorule-deploy'),
  [string]$TaskName = 'EvoRuleRuleServe',
  [string]$BackupTaskName = 'EvoRuleRuleServeBackup',
  [switch]$PurgeData,
  [switch]$RemoveGate,
  [switch]$Force
)

$ErrorActionPreference = 'Stop'
$exePath = Join-Path $DeployRoot 'evorule-rule-serve.exe'
$gateFile = Join-Path $env:LOCALAPPDATA 'com.evorule.evo-agent-desktop\service_registry.json'

if (-not $Force) {
  $answer = Read-Host "uninstall rule-serve from $DeployRoot (tasks stopped, assets removed)? type YES to continue"
  if ($answer -ne 'YES') { Write-Host 'aborted'; exit 1 }
}

# ---- 1. stop + unregister scheduled tasks -----------------------------------
foreach ($tn in @($TaskName, $BackupTaskName)) {
  $t = Get-ScheduledTask -TaskName $tn -ErrorAction SilentlyContinue
  if ($t) {
    # Kill by executable path first (scheduled-task stop alone is unreliable),
    # wait until gone, then reset the task state and unregister.
    $deadline = (Get-Date).AddSeconds(15)
    do {
      Get-CimInstance Win32_Process -Filter "Name='$(Split-Path $exePath -Leaf)'" |
        Where-Object { $_.ExecutablePath -eq $exePath } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
      Start-Sleep -Milliseconds 800
      $left = @(Get-CimInstance Win32_Process -Filter "Name='$(Split-Path $exePath -Leaf)'" |
        Where-Object { $_.ExecutablePath -eq $exePath })
    } while ($left.Count -gt 0 -and (Get-Date) -lt $deadline)
    Stop-ScheduledTask -TaskName $tn -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $tn -Confirm:$false
    Write-Host "[1] scheduled task unregistered: $tn"
  } else {
    Write-Host "[1] scheduled task not present, skipped: $tn"
  }
}
# stragglers: kill any process still running from this deployment root
Get-CimInstance Win32_Process -Filter "Name='evorule-rule-serve.exe'" |
  Where-Object { $_.ExecutablePath -eq $exePath } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

# ---- 2. remove deployment assets ---------------------------------------------
foreach ($f in @(
  $exePath,
  "$exePath.bak",
  (Join-Path $DeployRoot 'service_registry.json'),
  (Join-Path $DeployRoot 'bootstrap-pw.txt'),
  (Join-Path $DeployRoot 'companion-uplink-key.txt')
)) {
  if (Test-Path $f) { Remove-Item $f -Force; Write-Host "[2] removed $f" }
}
if ($PurgeData) {
  $dataDir = Join-Path $DeployRoot 'rule-serve'
  if (Test-Path $dataDir) { Remove-Item $dataDir -Recurse -Force; Write-Host "[2] data directory removed: $dataDir" }
} else {
  Write-Host "[2] database kept: $(Join-Path $DeployRoot 'rule-serve\data\rule.db') (use -PurgeData to remove)"
}

# ---- 3. gate copy -------------------------------------------------------------
$gateFile = Join-Path $env:LOCALAPPDATA 'com.evorule.evo-agent-desktop\service_registry.json'
if ($RemoveGate) {
  if (Test-Path $gateFile) {
    Remove-Item $gateFile -Force
    Write-Host "[3] gate copy removed: $gateFile (desktop armed-gate returns to unarmed)"
  } else {
    Write-Host '[3] gate copy not present, skipped'
  }
} else {
  Write-Host "[3] gate copy left untouched: $gateFile"
  if (Test-Path $gateFile) {
    Write-Host '    NOTE: the gate copy is per-user shared state. If this deployment was the'
    Write-Host '    uplink source on this machine, restore it from the remaining deployment'
    Write-Host '    root (copy service_registry.json over the gate path) or pass -RemoveGate'
    Write-Host '    to return the desktop armed-gate to its unarmed state.'
  }
}

Write-Host ''
Write-Host '===== uninstall summary ====='
Write-Host "deployment root: $DeployRoot"
Write-Host 'restore hint: re-run scripts\deploy-rule-serve.ps1 to rebuild the deployment; the kept database (if any) is reused as-is.'
Write-Host '============================='
