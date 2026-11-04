# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — governance rule-serve deployment kit: database backup.
#
# File-level SQLite backup (copy of the database file plus WAL sidecars when
# present) with a retention policy, plus optional registration of a daily
# backup scheduled task. Read-only against the live database; safe to run
# while the service is resident.
#
# Usage:
#   .\scripts\backup-rule-serve.ps1                     # one backup now
#   .\scripts\backup-rule-serve.ps1 -RegisterSchedule   # also register daily 03:30 task
#   .\scripts\backup-rule-serve.ps1 -Restore <backup.db> # restore into the deployment root
#
param(
  [string]$DeployRoot = (Join-Path 'D:\' 'evorule-deploy'),
  [string]$BackupDir = '',
  [int]$RetainCount = 14,
  [switch]$RegisterSchedule,
  [string]$ScheduleTime = '03:30',
  [string]$BackupTaskName = 'EvoRuleRuleServeBackup',
  [string]$Restore = '',
  [int]$Port = 18081,
  [string]$TaskName = 'EvoRuleRuleServe'
)

$ErrorActionPreference = 'Stop'
$Base = "http://127.0.0.1:$Port"
$dbPath = Join-Path $DeployRoot 'rule-serve\data\rule.db'
if ($BackupDir -eq '') { $BackupDir = Join-Path $DeployRoot 'backups' }

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

# ---- restore mode -------------------------------------------------------------
if ($Restore -ne '') {
  if (-not (Test-Path $Restore)) { throw "backup file not found: $Restore" }
  Write-Host "[restore] stopping resident task"
  # Kill by executable path (unique per deployment root): scheduled-task stop
  # alone is unreliable (process can outlive the task state or be respawned).
  $exePath = Join-Path $DeployRoot 'evorule-rule-serve.exe'
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
  Copy-Item $Restore $dbPath -Force
  foreach ($suffix in @('-wal', '-shm')) {
    $side = "$Restore$suffix"
    if (Test-Path $side) { Copy-Item $side "$dbPath$suffix" -Force }
    else { Remove-Item "$dbPath$suffix" -Force -ErrorAction SilentlyContinue }
  }
  Write-Host "[restore] database restored from $Restore"
  Start-ScheduledTask -TaskName $TaskName
  if (-not (Wait-Health 90)) { throw "instance not healthy on $Base after restore" }
  Write-Host "[restore] instance healthy: $Base/v1/health"
  exit 0
}

# ---- backup mode ---------------------------------------------------------------
if (-not (Test-Path $dbPath)) { throw "database not found: $dbPath" }
New-Item -ItemType Directory -Force -Path $BackupDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$target = Join-Path $BackupDir "rule-$stamp.db"
Copy-Item $dbPath $target -Force
foreach ($suffix in @('-wal', '-shm')) {
  if (Test-Path "$dbPath$suffix") { Copy-Item "$dbPath$suffix" "$target$suffix" -Force }
}
$size = (Get-Item $target).Length
Write-Host "[backup] $target ($size bytes)"

# ---- retention: keep the newest N backups --------------------------------------
$all = @(Get-ChildItem $BackupDir -Filter 'rule-*.db' | Sort-Object Name -Descending)
if ($all.Count -gt $RetainCount) {
  $all | Select-Object -Skip $RetainCount | ForEach-Object {
    Remove-Item $_.FullName -Force
    Remove-Item "$($_.FullName)-wal" -Force -ErrorAction SilentlyContinue
    Remove-Item "$($_.FullName)-shm" -Force -ErrorAction SilentlyContinue
    Write-Host "[retention] pruned $($_.Name)"
  }
}

# ---- optional daily schedule ----------------------------------------------------
if ($RegisterSchedule) {
  $existing = Get-ScheduledTask -TaskName $BackupTaskName -ErrorAction SilentlyContinue
  if ($existing) { Unregister-ScheduledTask -TaskName $BackupTaskName -Confirm:$false }
  $scriptPath = Join-Path $PSScriptRoot 'backup-rule-serve.ps1'
  $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$scriptPath`"" `
    -WorkingDirectory $DeployRoot
  $t = [datetime]::ParseExact($ScheduleTime, 'HH:mm', $null)
  $trigger = New-ScheduledTaskTrigger -Daily -At $t
  $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 30)
  Register-ScheduledTask -TaskName $BackupTaskName -Action $action -Trigger $trigger -Settings $settings | Out-Null
  Write-Host "[schedule] daily backup task registered: $BackupTaskName at $ScheduleTime"
}
