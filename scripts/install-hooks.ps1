# scripts/install-hooks.ps1
# 一键装 evorule-rule 的 git hooks(commit 自动 push,保持 working tree 永远同步)
# 用法: pwsh scripts/install-hooks.ps1

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

$hookDir = Join-Path (Get-Location) 'scripts/hooks'
$hookNames = @('post-commit')

Write-Host "==> 装 evorule-rule git hooks..."
Write-Host "    hooks 目录: $hookDir"
Write-Host "    hooks: $($hookNames -join ', ')"
Write-Host "    策略: 只装 post-commit(自动 push),不强加 pre-push 门禁(cargo test 太慢会卡开发)"

# 1. 配 core.hooksPath 指向 scripts/hooks
git config core.hooksPath 'scripts/hooks'
Write-Host "    [OK] git config core.hooksPath = scripts/hooks"

# 2. 确保 scripts/hooks 下的 hook 文件可执行(Windows + Unix 都要)
foreach ($name in $hookNames) {
    $path = Join-Path $hookDir $name
    if (-not (Test-Path $path)) {
        Write-Error "  [MISS] $path not found, 请先创建"
        exit 1
    }
    # Git on Windows 不需要 +x,但 Cygwin / WSL 可能要
    # 用 icacls 给当前用户加 Read+Execute
    & icacls $path /grant "${env:USERNAME}:(RX)" /Q | Out-Null
    Write-Host "    [OK] $name"
}

# 3. 给本地仓库配 user(防止 commit author 缺失)
$userEmail = git config user.email
$userName = git config user.name
if ([string]::IsNullOrWhiteSpace($userEmail) -or [string]::IsNullOrWhiteSpace($userName)) {
    git config user.email 'evorulelab@gmail.com'
    git config user.name 'EvoRule Project'
    Write-Host "    [OK] git config user.email/name = evorulelab@gmail.com / EvoRule Project"
}

Write-Host ""
Write-Host "==> 装好,验证:"
Write-Host "    git config core.hooksPath 期望 = scripts/hooks"
Write-Host "    git config user.email     期望 = evorulelab@gmail.com"
Write-Host ""
Write-Host "==> 行为:"
Write-Host "    - git commit 后: 自动 push 当前 branch 到 origin(失败不阻塞 commit)"
Write-Host ""
Write-Host "==> 卸载(如要):"
Write-Host "    git config --unset core.hooksPath"
Write-Host ""
Write-Host "==> 不装的钩子(2026-08-22 决策):"
Write-Host "    pre-push 门禁(cargo test)太慢,108 测试 60s+ 不完,会卡开发节奏"
Write-Host "    用户原话:'想以后绝对保持 workingtree 干净, 已积累大量未推送内容'"
Write-Host "    post-commit 自动 push 已解决核心诉求,pre-push 留给 CI(Gitee Go 启用时)"
