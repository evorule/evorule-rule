# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — governance rule-serve deployment kit: uplink key rotation.
#
# Dual-slot key rotation with a parallel window: the new key is issued and
# verified BEFORE the old key is revoked, so consumers that keep serving with
# the old key never observe an interruption. Triggers: rule-serve release or
# quarterly, whichever comes first.
#
# Sequence:
#   1. issue a new entries:propose key (plaintext returned once)
#   2. write the new plaintext to the custody file (ACL restricted)
#   3. inject the new key into the service registry copy + desktop gate copy
#   4. validation probe: a real propose with the NEW key must succeed (201),
#      the probe entry is removed right after
#   5. revoke the old key (immediate DELETE; per-request key lookup makes it
#      effective on the next call) and verify it now answers 401
#
# The old key_id is derived from the old plaintext (key_id = "key_" + last 16
# characters of the plaintext), no server-side lookup needed.
#
# Usage:
#   .\scripts\rotate-rule-serve-key.ps1 [-KeepOldKey]   # -KeepOldKey: emergency only
#
param(
  [string]$DeployRoot = (Join-Path 'D:\' 'evorule-deploy'),
  [int]$Port = 18081,
  [string]$AdminUser = 'admin',
  [string]$AdminPassword = '',
  [string]$ProbeDataset = 'companion-governance',
  [switch]$KeepOldKey
)

$ErrorActionPreference = 'Stop'
$Base = "http://127.0.0.1:$Port"
$pwPath = Join-Path $DeployRoot 'bootstrap-pw.txt'
$keyPath = Join-Path $DeployRoot 'companion-uplink-key.txt'
$registryPath = Join-Path $DeployRoot 'service_registry.json'

# ---- 0. preconditions ---------------------------------------------------------
if (-not (Test-Path $pwPath)) { throw "bootstrap credential file not found: $pwPath" }
if (-not (Test-Path $keyPath)) { throw "uplink key file not found: $keyPath (run deploy first)" }
if (-not (Test-Path $registryPath)) { throw "service registry not found: $registryPath (run deploy first)" }
$adminPassword = if ($AdminPassword -ne '') { $AdminPassword } else { (Get-Content $pwPath -Raw).Trim() }
$oldKey = (Get-Content $keyPath -Raw).Trim()
$oldKeyId = 'key_' + $oldKey.Substring($oldKey.Length - 16)

function Restrict-FileAcl([string]$Path) {
  icacls $Path /inheritance:r /grant:r "$($env:USERNAME):F" /grant:r "Administrators:F" /grant:r "SYSTEM:F" | Out-Null
}

$login = Invoke-RestMethod -Method Post -Uri "$Base/v1/auth/login" -ContentType 'application/json' `
  -Body (@{ tenant_id = 'default'; username = $AdminUser; password = $adminPassword } | ConvertTo-Json -Compress)
$authHeaders = @{ Authorization = "Bearer $($login.access_token)" }

# ---- 1. issue the new key (slot 2 goes live while slot 1 keeps serving) -------
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$resp = Invoke-RestMethod -Method Post -Uri "$Base/v1/api_keys" -Headers $authHeaders -ContentType 'application/json' `
  -Body (@{ name = "companion-uplink-$stamp"; scope = 'entries:propose' } | ConvertTo-Json -Compress)
$newKey = $resp.key
$newKeyId = $resp.key_id
Write-Host "[1] new key issued: $newKeyId (dual-slot window open: old + new both valid)"

# ---- 2. custody file (plaintext written once, ACL restricted) -----------------
Set-Content -Path $keyPath -Value $newKey -NoNewline -Encoding ASCII
Restrict-FileAcl $keyPath
Write-Host "[2] new plaintext written to $keyPath (ACL restricted)"

# ---- 3. registry copy + gate copy ---------------------------------------------
$registry = [System.IO.File]::ReadAllText($registryPath) | ConvertFrom-Json
if (-not $registry.'knowledge-propose') { throw 'registry has no knowledge-propose entry' }
$registry.'knowledge-propose'.headers.'X-Api-Key' = $newKey
[System.IO.File]::WriteAllText($registryPath, ($registry | ConvertTo-Json -Depth 64), (New-Object System.Text.UTF8Encoding($false)))
$gateDir = Join-Path $env:LOCALAPPDATA 'com.evorule.evo-agent-desktop'
New-Item -ItemType Directory -Force -Path $gateDir | Out-Null
Copy-Item $registryPath (Join-Path $gateDir 'service_registry.json') -Force
Write-Host '[3] service registry copy + gate copy updated to the new key'

# ---- 4. validation probe with the NEW key (must fully succeed) -----------------
$probeId = "probe-rotate-$stamp"
$probeBody = @{
  dataset_id = $ProbeDataset
  cause = 'key rotation validation probe'
  entry = @{
    entry_id = $probeId
    version = 1
    payload = @{ statement = 'Key rotation validation probe entry (removed right after creation).'; title = 'Rotation probe'; confidence = 0.6; findings = 'Created by rotate-rule-serve-key.ps1 to prove the new key can write, then deleted.' }
    schema_ref = 'builtin:knowledge/fact'
  }
} | ConvertTo-Json -Depth 10 -Compress
$probeResp = Invoke-RestMethod -Method Post -Uri "$Base/v1/invoke/propose_knowledge_entry" `
  -Headers @{ 'X-Api-Key' = $newKey } -ContentType 'application/json' -Body $probeBody
Write-Host "[4] validation probe with NEW key succeeded (entry $probeId landed as Draft)"
try {
  Invoke-RestMethod -Method Delete -Uri "$Base/v1/entries/$probeId" -Headers $authHeaders | Out-Null
  Write-Host '[4] probe entry removed (cleanup)'
} catch {
  Write-Host "[4] WARNING: probe entry cleanup failed: $($_.Exception.Message) — remove entry $probeId manually"
}

# ---- 5. revoke the old key + negative probe ------------------------------------
if ($KeepOldKey) {
  Write-Host '[5] -KeepOldKey given: old key kept live (emergency mode; rotate again later to revoke)'
} else {
  Invoke-RestMethod -Method Delete -Uri "$Base/v1/api_keys/$oldKeyId" -Headers $authHeaders | Out-Null
  Write-Host "[5] old key revoked: $oldKeyId"
  $negBody = @{
    dataset_id = $ProbeDataset
    cause = 'key rotation negative probe'
    entry = @{ entry_id = "probe-neg-$stamp"; version = 1; payload = @{}; schema_ref = '__liveness_probe_resolver_miss__' }
  } | ConvertTo-Json -Depth 10 -Compress
  try {
    Invoke-WebRequest -Method Post -Uri "$Base/v1/invoke/propose_knowledge_entry" -UseBasicParsing `
      -Headers @{ 'X-Api-Key' = $oldKey } -ContentType 'application/json' -Body $negBody | Out-Null
    throw 'negative probe unexpectedly succeeded with the revoked key'
  } catch {
    $code = 0
    if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
    if ($code -eq 401) { Write-Host '[5] negative probe: old key now answers 401 (revocation effective)' }
    else { throw "negative probe returned unexpected status $code" }
  }
}

Write-Host ''
Write-Host '===== rotation summary ====='
Write-Host "new key:  $newKeyId (live, custody file + registry + gate updated)"
if (-not $KeepOldKey) { Write-Host "old key:  $oldKeyId (revoked, 401 verified)" }
else { Write-Host "old key:  $oldKeyId (KEPT by request)" }
Write-Host "window:   dual-slot parallel window was open between step 1 and step 5; consumers serving the old key saw zero interruption"
Write-Host '============================'
