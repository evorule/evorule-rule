# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — governance rule-serve deployment kit: first-time / re-run deploy.
#
# Idempotent installer for a local, scheduled-task based rule-serve deployment.
# Safe to re-run: existing directories, bootstrap credential, datasets and API
# keys are reused; nothing is re-issued when already in place.
#
# Invariant: the resident scheduled task command line carries ZERO credentials.
# The bootstrap credential is used exactly once (one-shot admin initialization)
# and is then kept on disk under ACL as the operator re-entry secret.
#
# Usage:
#   .\scripts\deploy-rule-serve.ps1 [-ExeSource <path-to-exe>] `
#       [-DeployRoot <dir>] [-Port 18081] [-TaskName EvoRuleRuleServe] `
#       [-AdminUser admin] [-AdminPassword <pw>] [-AllowedOrigins "a,b"] `
#       [-SkipSeed] [-SkipGateCopy]
#
# What it does, in order:
#   1. create deployment directory layout
#   2. place the server executable (copy from -ExeSource, or reuse in-place)
#   3. bootstrap credential (reuse file, or take -AdminPassword, or prompt)
#   4. one-shot admin initialization on a fresh database, then shut down
#   5. register the resident scheduled task (logon trigger, auto-restart,
#      zero-credential command line) and start it
#   6. seed the four governance knowledge datasets (idempotent)
#   7. ensure the uplink API key exists (reuse after a liveness probe)
#   8. inject the service registry copy (+ gate copy for the desktop side)
#
param(
  [string]$ExeSource = '',
  [string]$DeployRoot = (Join-Path 'D:\' 'evorule-deploy'),
  [int]$Port = 18081,
  [string]$TaskName = 'EvoRuleRuleServe',
  [string]$AdminUser = 'admin',
  [string]$AdminPassword = '',
  # Console integration: loopback dev origins of the console frontend dev
  # server. Loopback-only, so the CORS allowance does not widen the reachable
  # surface beyond the local machine; pass '' to stay same-origin only.
  [string]$AllowedOrigins = 'http://localhost:5174,http://127.0.0.1:5174',
  [switch]$SkipSeed,
  [switch]$SkipGateCopy
)

$ErrorActionPreference = 'Stop'
$Base = "http://127.0.0.1:$Port"

function Wait-Health([string]$Url, [int]$TimeoutSec) {
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    try {
      $r = Invoke-WebRequest -Uri "$Url/v1/health" -UseBasicParsing -TimeoutSec 5
      if ($r.StatusCode -eq 200) { return $true }
    } catch { Start-Sleep -Milliseconds 500 }
  }
  return $false
}

function Stop-ServeProcesses([string]$ExePath) {
  # Scheduled-task stop alone is unreliable (the process can outlive the task
  # state or be respawned by restart settings). Kill by executable path
  # (unique per deployment root), wait until gone, then reset the task state.
  $deadline = (Get-Date).AddSeconds(15)
  do {
    Get-CimInstance Win32_Process -Filter "Name='$(Split-Path $ExePath -Leaf)'" |
      Where-Object { $_.ExecutablePath -eq $ExePath } |
      ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    $left = @(Get-CimInstance Win32_Process -Filter "Name='$(Split-Path $ExePath -Leaf)'" |
      Where-Object { $_.ExecutablePath -eq $ExePath })
  } while ($left.Count -gt 0 -and (Get-Date) -lt $deadline)
}

function Restrict-FileAcl([string]$Path) {
  # Credential custody: plain files under the deployment root are restricted
  # to the current user, Administrators and SYSTEM (inheritance removed).
  icacls $Path /inheritance:r /grant:r "$($env:USERNAME):F" /grant:r "Administrators:F" /grant:r "SYSTEM:F" | Out-Null
}

# ---- 1. directory layout ----------------------------------------------------
$dataDir = Join-Path $DeployRoot 'rule-serve\data'
$backupDir = Join-Path $DeployRoot 'backups'
New-Item -ItemType Directory -Force -Path $dataDir, $backupDir | Out-Null
$exePath = Join-Path $DeployRoot 'evorule-rule-serve.exe'
$dbPath = Join-Path $dataDir 'rule.db'
$pwPath = Join-Path $DeployRoot 'bootstrap-pw.txt'
$keyPath = Join-Path $DeployRoot 'companion-uplink-key.txt'
$registryPath = Join-Path $DeployRoot 'service_registry.json'
Write-Host "[1] deployment root: $DeployRoot"

# ---- 2. executable ----------------------------------------------------------
if ($ExeSource -ne '') {
  if (-not (Test-Path $ExeSource)) { throw "ExeSource not found: $ExeSource" }
  $sameBinary = (Test-Path $exePath) -and
    ((Get-FileHash $ExeSource).Hash -eq (Get-FileHash $exePath).Hash)
  if ($sameBinary) {
    Write-Host '[2] executable identical to ExeSource, reused'
  } else {
    # Different binary on a re-run = deploy doubles as upgrade: stop the
    # resident task, swap, restart (seconds-level downtime).
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    $alive = @(Get-CimInstance Win32_Process -Filter "Name='$(Split-Path $exePath -Leaf)'" |
      Where-Object { $_.ExecutablePath -eq $exePath }).Count -gt 0
    $wasRunning = ($task -and ($task.State -eq 'Running' -or $alive))
    if ($wasRunning) {
      Stop-ServeProcesses $exePath
      Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    }
    Copy-Item $ExeSource $exePath -Force
    Write-Host "[2] executable placed from $ExeSource"
    if ($wasRunning) {
      Start-ScheduledTask -TaskName $TaskName
      if (-not (Wait-Health $Base 60)) { throw 'resident instance not healthy after binary swap' }
      Write-Host '[2] resident task restarted after binary swap, healthy'
    }
  }
} elseif (Test-Path $exePath) {
  Write-Host '[2] executable already in place, reused'
} else {
  throw 'no executable in place and no -ExeSource given'
}

# ---- 3. bootstrap credential ------------------------------------------------
if (Test-Path $pwPath) {
  $adminPassword = (Get-Content $pwPath -Raw).Trim()
  Write-Host '[3] bootstrap credential reused from disk'
} elseif ($AdminPassword -ne '') {
  Set-Content -Path $pwPath -Value $AdminPassword -NoNewline -Encoding ASCII
  Restrict-FileAcl $pwPath
  Write-Host '[3] bootstrap credential written (ACL restricted)'
} else {
  $sec = Read-Host 'enter bootstrap admin password' -AsSecureString
  $b = [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
  $AdminPassword = [System.Runtime.InteropServices.Marshal]::PtrToStringBSTR($b)
  Set-Content -Path $pwPath -Value $AdminPassword -NoNewline -Encoding ASCII
  Restrict-FileAcl $pwPath
  Write-Host '[3] bootstrap credential written from prompt (ACL restricted)'
}

# ---- 4. one-shot admin initialization (fresh database only) -----------------
$freshDb = -not (Test-Path $dbPath)
if ($freshDb) {
  Write-Host '[4] fresh database: one-shot admin initialization'
  $bootArgs = @("--db", "`"$dbPath`"", "--port", "$Port",
                "--admin-user", $AdminUser, "--admin-password", $AdminPassword)
  if ($AllowedOrigins -ne '') { $bootArgs += @('--allowed-origins', "`"$AllowedOrigins`"") }
  $proc = Start-Process -FilePath $exePath -ArgumentList $bootArgs -WorkingDirectory $DeployRoot -WindowStyle Hidden -PassThru
  try {
    if (-not (Wait-Health $Base 60)) { throw 'bootstrap instance did not become healthy in 60s' }
    Write-Host '[4] bootstrap instance healthy (admin + tenant initialized, jwt secret persisted)'
  } finally {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
  }
} else {
  Write-Host '[4] database exists, skipping admin initialization'
}

# ---- 5. resident scheduled task (zero-credential command line) --------------
$taskArgs = @('--db', "`"$dbPath`"", '--port', "$Port")
if ($AllowedOrigins -ne '') { $taskArgs += @('--allowed-origins', "`"$AllowedOrigins`"") }
$taskArgLine = ($taskArgs -join ' ')
$existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) {
  $cur = $existing.Actions[0]
  if ($cur.Execute -eq $exePath -and $cur.Arguments -eq $taskArgLine) {
    Write-Host '[5] scheduled task already registered with matching command line, reused'
    if ($existing.State -eq 'Running') {
      # nothing to do; service already resident
    } else {
      Start-ScheduledTask -TaskName $TaskName
    }
  } else {
    Write-Host '[5] scheduled task command line drifted, re-registering'
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    $existing = $null
  }
}
if (-not $existing) {
  $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
  $settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable
  $action = New-ScheduledTaskAction -Execute $exePath -Argument $taskArgLine -WorkingDirectory $DeployRoot
  Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings | Out-Null
  Start-ScheduledTask -TaskName $TaskName
  Write-Host '[5] scheduled task registered (logon trigger, restart on failure, zero credentials)'
}
if (-not (Wait-Health $Base 60)) { throw "resident instance not healthy on $Base" }
Write-Host "[5] resident instance healthy: $Base/v1/health"

# ---- helper: authenticated API call -----------------------------------------
$login = Invoke-RestMethod -Method Post -Uri "$Base/v1/auth/login" -ContentType 'application/json' `
  -Body (@{ tenant_id = 'default'; username = $AdminUser; password = $adminPassword } | ConvertTo-Json -Compress)
$token = $login.access_token
$authHeaders = @{ Authorization = "Bearer $token" }

# ---- 6. seed the four governance knowledge datasets (idempotent) ------------
$datasets = @(
  @{ id = 'companion-governance';  name = 'Companion governance knowledge';   desc = 'Knowledge entries proposed by the companion uplink chain (Draft gating server-side).' },
  @{ id = 'rule-failure-digest';   name = 'Rule failure digest';              desc = 'Digest of rule failure events from the execution side (reserved, activated with the failure pipeline).' },
  @{ id = 'mcp-server-proposals';  name = 'MCP server proposals';             desc = 'MCP server adoption proposals awaiting human review (reserved).' },
  @{ id = 'rule-packs';            name = 'Rule packs distribution source';   desc = 'Distribution source for curated rule packs (reserved, activated with the pack pipeline).' }
)
if ($SkipSeed) {
  Write-Host '[6] seeding skipped (-SkipSeed)'
} else {
  foreach ($d in $datasets) {
    $found = $null
    try {
      $found = Invoke-RestMethod -Method Get -Uri "$Base/v1/datasets/$($d.id)" -Headers $authHeaders
    } catch { $found = $null }
    if ($found) {
      Write-Host "[6] dataset $($d.id) exists, reused"
    } else {
      Invoke-RestMethod -Method Post -Uri "$Base/v1/datasets" -Headers $authHeaders -ContentType 'application/json' `
        -Body (@{ dataset_id = $d.id; name = $d.name; description = $d.desc; dataset_kind = 'knowledge'; tags = @('governance') } | ConvertTo-Json -Compress) | Out-Null
      Write-Host "[6] dataset $($d.id) created (kind=knowledge)"
    }
  }
}

# ---- 7. uplink API key (reuse when live, issue once) ------------------------
function Test-UplinkKey([string]$Plain) {
  # Liveness probe without side effects: propose with a resolver-missing
  # schema_ref. Auth runs first, so 401 = key invalid/revoked and ANY other
  # response (400/404/422/500 internal_error for the resolver miss) = the key
  # authenticated; nothing is ever written.
  $body = @{
    dataset_id = 'companion-governance'
    cause = 'deploy key liveness probe'
    entry = @{ entry_id = "probe-deploy-$(Get-Date -Format 'yyyyMMddHHmmss')"; version = 1; payload = @{}; schema_ref = '__liveness_probe_resolver_miss__' }
  } | ConvertTo-Json -Depth 10 -Compress
  try {
    Invoke-WebRequest -Method Post -Uri "$Base/v1/invoke/propose_knowledge_entry" -UseBasicParsing `
      -Headers @{ 'X-Api-Key' = $Plain } -ContentType 'application/json' -Body $body | Out-Null
    return $true
  } catch {
    $code = 0
    if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
    if ($code -eq 401) { return $false }
    if ($code -eq 0) { throw }
    return $true  # authenticated but rejected later (400/404/422): key is live
  }
}

if (Test-Path $keyPath) {
  $uplinkKey = (Get-Content $keyPath -Raw).Trim()
  if (Test-UplinkKey $uplinkKey) {
    Write-Host '[7] uplink key in place and live, reused'
  } else {
    Write-Host '[7] uplink key file stale (revoked/invalid), re-issuing'
    $resp = Invoke-RestMethod -Method Post -Uri "$Base/v1/api_keys" -Headers $authHeaders -ContentType 'application/json' `
      -Body (@{ name = 'companion-uplink'; scope = 'entries:propose' } | ConvertTo-Json -Compress)
    $uplinkKey = $resp.key
    Set-Content -Path $keyPath -Value $uplinkKey -NoNewline -Encoding ASCII
    Restrict-FileAcl $keyPath
  }
} else {
  $resp = Invoke-RestMethod -Method Post -Uri "$Base/v1/api_keys" -Headers $authHeaders -ContentType 'application/json' `
    -Body (@{ name = 'companion-uplink'; scope = 'entries:propose' } | ConvertTo-Json -Compress)
  $uplinkKey = $resp.key
  Set-Content -Path $keyPath -Value $uplinkKey -NoNewline -Encoding ASCII
  Restrict-FileAcl $keyPath
  Write-Host '[7] uplink key issued (plaintext stored once, ACL restricted)'
}

# ---- 8. service registry injection + gate copy ------------------------------
$registry = $null
if (Test-Path $registryPath) {
  # .NET ReadAllText: always UTF-8 (Get-Content defaults to ANSI codepage and
  # garbles non-ASCII registry descriptions on Windows PowerShell 5.1)
  $registry = [System.IO.File]::ReadAllText($registryPath) | ConvertFrom-Json
} else {
  $registry = [pscustomobject]@{}
}
function Ensure-Member($Obj, [string]$Name, $Value) {
  if ($Obj.PSObject.Properties[$Name]) { $Obj.$Name = $Value }
  else { $Obj | Add-Member -NotePropertyName $Name -NotePropertyValue $Value }
}
Ensure-Member $registry 'knowledge-propose' ([pscustomobject]@{
  url = "$Base/v1/invoke/propose_knowledge_entry"
  method = 'POST'
  headers = [pscustomobject]@{ 'X-Api-Key' = $uplinkKey }
  timeout_ms = 10000
  version = '1.0.0'
  description = 'Knowledge entry propose channel (Draft). X-Api-Key injected by the deployment kit; no credential value is ever committed to the repository.'
})
# Machine transition channel stays keyless: approvals are human-review only.
Ensure-Member $registry 'knowledge-transition' ([pscustomobject]@{
  url = "$Base/v1/invoke/transition_entry"
  method = 'POST'
  headers = [pscustomobject]@{ 'X-Api-Key' = '' }
  timeout_ms = 10000
  version = '1.0.0'
  description = 'Knowledge entry machine transition channel. Key intentionally not issued; approvals are human-review only.'
})
# Written without BOM: the gate copy is consumed by a Rust JSON parser.
[System.IO.File]::WriteAllText($registryPath, ($registry | ConvertTo-Json -Depth 64), (New-Object System.Text.UTF8Encoding($false)))
Write-Host '[8] service registry copy injected'

if ($SkipGateCopy) {
  Write-Host '[8] gate copy skipped (-SkipGateCopy)'
} else {
  $gateDir = Join-Path $env:LOCALAPPDATA 'com.evorule.evo-agent-desktop'
  New-Item -ItemType Directory -Force -Path $gateDir | Out-Null
  Copy-Item $registryPath (Join-Path $gateDir 'service_registry.json') -Force
  Write-Host '[8] gate copy in place (desktop armed-gate reads file presence)'
}

# ---- summary ----------------------------------------------------------------
Write-Host ''
Write-Host '===== deploy summary ====='
Write-Host "root:        $DeployRoot"
Write-Host "endpoint:    $Base (health OK)"
Write-Host "task:        $TaskName (zero-credential command line)"
Write-Host "datasets:    $($datasets.id -join ', ')"
Write-Host "uplink key:  $keyPath"
Write-Host "registry:    $registryPath"
if (-not $SkipGateCopy) { Write-Host "gate copy:   $(Join-Path $env:LOCALAPPDATA 'com.evorule.evo-agent-desktop\service_registry.json')" }
Write-Host '=========================='
