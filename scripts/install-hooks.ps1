# scripts/install-hooks.ps1
# 门禁自检:校验本地门禁 hook 在场 + core.hooksPath 未被改指
# (2026-10-15 起 post-commit 自动推送已废除,push 一律人工手动执行;
#  本脚本不再安装任何 hook,只做自检)
# 用法: pwsh scripts/install-hooks.ps1

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

Write-Host "==> evorule-rule 门禁自检..."
$fail = $false

# 1. core.hooksPath 必须未设置(.git/hooks 门禁生效的前提)
$hooksPath = git config --get core.hooksPath 2>$null
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($hooksPath)) {
    Write-Host "    [OK] core.hooksPath 未设置(.git/hooks 门禁生效)"
} else {
    Write-Host "    [FAIL] core.hooksPath = $hooksPath(会旁路 .git/hooks 门禁)" -ForegroundColor Red
    Write-Host "           修复: git config --unset core.hooksPath"
    $fail = $true
}

# 2. .git/hooks 下两门禁必须在场
foreach ($name in @('commit-msg', 'pre-push')) {
    $path = Join-Path '.git/hooks' $name
    if (Test-Path $path) {
        Write-Host "    [OK] $path 在场"
    } else {
        Write-Host "    [FAIL] $path 缺失(门禁休眠)" -ForegroundColor Red
        $fail = $true
    }
}

# 3. 确认无 post-commit 自动推送残留
foreach ($p in @('.git/hooks/post-commit', 'scripts/hooks/post-commit')) {
    if (Test-Path $p) {
        Write-Host "    [FAIL] $p 在场(自动推送已废除,不应存在)" -ForegroundColor Red
        $fail = $true
    } else {
        Write-Host "    [OK] $p 不存在"
    }
}

Write-Host ""
if ($fail) {
    Write-Host "==> 自检未通过,按上述提示修复。" -ForegroundColor Red
    exit 1
}
Write-Host "==> 自检通过: 门禁在场,推送保持人工手动。"
