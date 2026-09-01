# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — UV-030 V1 LLM 三操作契约验收脚本(可复现,无 Key;链路 B:rule → evo-agent 37 号契约)
#
# 场景:
#   [1..3] 三操作契约 E2E:rule 代理 /v1/llm/ops/{op} → evo-agent serve(mock 内容)
#          断言 37 号 §4 响应骨架(operation/request_id/task_id/status/result/llm_generated 溯源)
#   [4]    LlmOpAudit 入库:GET /v1/audits/llm 三条 completed + GET /v1/llm/audits/stats 统计
#   [5]    LLM 不可达如实报错:停 evo-agent → 代理调用快速失败(不挂死)且审计落 status=failed
#   [6]    未知操作显式拒绝(404 语义经 rule 侧 bad_request)
#
# 前提:D:\evo-agent 与本仓已构建(target/debug 二进制存在;缺则先 cargo build)
# 用法:.\scripts\acceptance-llm-ops.ps1
# 说明:mock 内容经 EVO_AGENT_LLM_MOCK_CONTENT 注入(离线契约验证开关,evo-agent llm_ops.rs),
#       三 op 输出规整要求不同(draft:对象 / gen_tests:test_cases 数组 / explain:explanation 字符串),
#       故每 op 独立启动 evo-agent;gate two(llm_generated 只达 Draft)为 store 层强约束,
#       由 evorule-rule 单测锁定(test_llm_generated_entry_cannot_leave_draft 等),本脚本不重复。

param([switch]$RealLlm)

$ErrorActionPreference = 'Stop'

$agentExe = 'D:\evo-agent\target\debug\evo-agent.exe'
$ruleExe  = Join-Path $PSScriptRoot '..\target\debug\evorule-rule-serve.exe'
$rulePort = 18099
$agentPort = 18090
$base     = "http://127.0.0.1:$rulePort"

foreach ($exe in @($agentExe, $ruleExe)) {
  if (-not (Test-Path $exe)) { throw "二进制不存在: $exe — 请先在对应仓 cargo build" }
}

$tmpRoot = Join-Path $env:TEMP ("uv030-llm-ops-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $tmpRoot | Out-Null

$script:agentProc = $null
$script:ruleProc = $null
$failures = New-Object System.Collections.Generic.List[string]

function Assert-True($Cond, $Label) {
  if ($Cond) { Write-Host "  [PASS] $Label" }
  else { Write-Host "  [FAIL] $Label"; $script:failures.Add($Label) }
}

function Start-Agent($MockContent) {
  # $MockContent 非空 → mock handler(离线契约验证);空 → 真实 handler(需 MINIMAX_API_KEY 等 env)
  if ($MockContent) { $env:EVO_AGENT_LLM_MOCK_CONTENT = $MockContent }
  $script:agentProc = Start-Process -FilePath $agentExe `
    -ArgumentList @('serve', '--port', "$agentPort", '--no-auth') `
    -WorkingDirectory 'D:\evo-agent' -PassThru -WindowStyle Hidden
  # 就绪轮询(/ops 对未知名返回 404 也算就绪)
  $deadline = (Get-Date).AddSeconds(20)
  while ((Get-Date) -lt $deadline) {
    try {
      $r = Invoke-WebRequest -Uri "http://127.0.0.1:$agentPort/ops/__probe__" -Method POST `
        -ContentType 'application/json' -Body '{}' -UseBasicParsing -TimeoutSec 2
      if ($r.StatusCode -ge 200) { return }
    } catch {
      if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -eq 404) { return }
      Start-Sleep -Milliseconds 400
    }
  }
  throw "evo-agent serve 20s 未就绪"
}

function Stop-Agent {
  if ($script:agentProc -and -not $script:agentProc.HasExited) {
    Stop-Process -Id $script:agentProc.Id -Force -ErrorAction SilentlyContinue
  }
  $script:agentProc = $null
  Remove-Item Env:EVO_AGENT_LLM_MOCK_CONTENT -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 300
}

function Api($Method, $Path, $Token, $BodyObj) {
  $headers = @{}
  if ($Token) { $headers['Authorization'] = "Bearer $Token" }
  $params = @{ Uri = "$base$Path"; Method = $Method; Headers = $headers; UseBasicParsing = $true }
  if ($null -ne $BodyObj) {
    $params['ContentType'] = 'application/json'
    # PS 5.1 对字符串 Body 默认 Latin-1 编码,中文(LLM 产物回传)会被搅碎 — 必须显式 UTF-8 字节
    $params['Body'] = [System.Text.Encoding]::UTF8.GetBytes(($BodyObj | ConvertTo-Json -Depth 30 -Compress))
  }
  return Invoke-RestMethod @params
}

try {
  # ---- 启动治理服务(临时库 + 引导管理员) ----
  $dbPath = Join-Path $tmpRoot 'rule.db'
  $script:ruleProc = Start-Process -FilePath $ruleExe `
    -ArgumentList @('--db', $dbPath, '--port', "$rulePort", `
      '--admin-user', 'admin', '--admin-password', 'uv030-acceptance', `
      '--secret', 'uv030-acceptance-secret', `
      '--llm-base-url', "http://127.0.0.1:$agentPort") `
    -WorkingDirectory (Join-Path $PSScriptRoot '..') -PassThru -WindowStyle Hidden
  # 就绪探针超时须大于 Argon2id 引导创建/校验耗时(实测 ~5s),否则登录恒超时误判未就绪
  $deadline = (Get-Date).AddSeconds(60)
  $ready = $false
  $lastErr = ''
  while ((Get-Date) -lt $deadline) {
    try { $null = Invoke-RestMethod -Uri "$base/v1/auth/login" -Method POST -ContentType 'application/json' `
      -Body '{"tenant_id":"default","username":"admin","password":"uv030-acceptance"}' -TimeoutSec 10; $ready = $true; break }
    catch { $lastErr = $_.Exception.Message; Start-Sleep -Milliseconds 500 }
  }
  if (-not $ready) {
    if ($script:ruleProc -and -not $script:ruleProc.HasExited) {
      throw "evorule-rule-serve 30s 未就绪(进程存活,最后探针错误: $lastErr)"
    } else {
      throw "evorule-rule-serve 30s 未就绪(进程已退出,退出码 $($script:ruleProc.ExitCode))"
    }
  }
  $login = Api 'POST' '/v1/auth/login' $null @{ tenant_id = 'default'; username = 'admin'; password = 'uv030-acceptance' }
  $token = $login.access_token
  Write-Host '[0] evorule-rule-serve 就绪 + 管理员登录成功'

  # ---- 场景 1..3:三操作契约 E2E(每 op 独立 mock 启动) ----
  $opCases = @(
    @{ op = 'draft_rule';   mock = '{"name":"mock-rule","description":"mock draft","body":{"type":"branch","params":{}}}';
       check = { param($resp) $null -ne $resp.result.rule } },
    @{ op = 'gen_tests';    mock = '{"test_cases":[{"name":"t1","input":{"x":1}}]}';
       check = { param($resp) $resp.result.test_cases.Count -ge 1 } },
    @{ op = 'explain_rule'; mock = '{"explanation":"mock explanation text"}';
       check = { param($resp) $resp.result.explanation -is [string] } }
  )
  $n = 0
  foreach ($case in $opCases) {
    $n++
    Write-Host "[$n] 链路 B 契约 E2E: $($case.op)"
    Start-Agent $case.mock
    try {
      $resp = Api 'POST' "/v1/llm/ops/$($case.op)" $token @{ model = 'mock-model'; params = @{ demo = $true } }
      Assert-True ($resp.operation -eq $case.op)                       "$($case.op): operation 回显"
      Assert-True ($resp.status -eq 'completed')                       "$($case.op): status=completed(同步主路径)"
      Assert-True ($null -eq $resp.task_id)                            "$($case.op): task_id=null(MVP 同步预留)"
      Assert-True (& $case.check $resp)                                "$($case.op): result 输出规整符合 op 语义"
      Assert-True ($resp.llm_generated.model -eq 'mock-model')         "$($case.op): llm_generated.model 溯源"
      Assert-True ($resp.llm_generated.operation -eq $case.op)         "$($case.op): llm_generated.operation 溯源"
      Assert-True ($null -ne $resp.llm_generated.timestamp)            "$($case.op): llm_generated.timestamp 溯源"
      Assert-True ($resp.errors -eq $null)                             "$($case.op): errors 为空"
    } finally { Stop-Agent }
  }

  # ---- 场景 4:LlmOpAudit 入库 ----
  Write-Host '[4] LlmOpAudit 入库(37 号 §8)'
  $audits = Api 'GET' '/v1/audits/llm?limit=50' $token $null
  $items = @($audits.items)
  Assert-True ($items.Count -ge 3) "审计列表 ≥3 条(实际 $($items.Count))"
  foreach ($op in @('draft_rule', 'gen_tests', 'explain_rule')) {
    $hit = $items | Where-Object { $_.operation -eq $op -and $_.status -eq 'completed' }
    Assert-True ($null -ne $hit) "审计含 $op completed 条目"
  }
  Assert-True (($items | Where-Object { $_.model -eq 'mock-model' }).Count -ge 3) "审计 model 溯源落库"
  $stats = Api 'GET' '/v1/llm/audits/stats' $token $null
  Assert-True (($null -ne $stats) -and ($stats.completed -ge 3)) "审计统计端点可用(completed=$($stats.completed) total=$($stats.total))"

  # ---- 场景 5:LLM 不可达如实报错 + 失败入审计 ----
  Write-Host '[5] LLM 不可达如实报错(evo-agent 停机)'
  $errBody = $null
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  try {
    $null = Api 'POST' '/v1/llm/ops/draft_rule' $token @{ model = 'mock-model'; params = @{} }
    Assert-True $false "不可达时应报错(却返回了成功)"
  } catch {
    $sw.Stop()
    $elapsed = $sw.ElapsedMilliseconds
    $errBody = $_.ErrorDetails.Message
    Assert-True ($elapsed -lt 10000) "不可达快速失败(${elapsed}ms,不挂死)"
    Assert-True ($null -ne $errBody -and $errBody -ne '') "错误响应含如实错误文本"
  }
  $audits2 = Api 'GET' '/v1/audits/llm?limit=50' $token $null
  $failed = @($audits2.items) | Where-Object { $_.status -eq 'failed' -and $_.operation -eq 'draft_rule' }
  Assert-True ($null -ne $failed) "失败调用落审计(status=failed + error 文本)"

  # ---- 场景 6:未知操作显式拒绝 ----
  Write-Host '[6] 未知操作显式拒绝'
  try {
    $null = Api 'POST' '/v1/llm/ops/__nope__' $token @{ params = @{} }
    Assert-True $false "未知操作应被拒绝(却返回了成功)"
  } catch {
    $code = [int]$_.Exception.Response.StatusCode
    Assert-True ($code -eq 400 -or $code -eq 404) "未知操作返回 400/404(实际 $code)"
  }

  # ---- 场景 7(可选,-RealLlm):真实 LLM 三操作(MiniMax;Key 仅经 env MINIMAX_API_KEY 注入,不落盘) ----
  if ($RealLlm) {
    if (-not $env:MINIMAX_API_KEY) { throw '场景 7 需要 env MINIMAX_API_KEY(一次性 Key,验收后作废)' }
    Write-Host '[7] 真实 LLM 三操作(MiniMax MiniMax-Text-01)'
    $env:MINIMAX_MODEL = 'MiniMax-Text-01'
    Start-Agent ''
    try {
      $draft = Api 'POST' '/v1/llm/ops/draft_rule' $token @{
        model = 'MiniMax-Text-01'; request_id = 'uv030-real-draft'
        params = @{ 需求文本 = '订单金额大于 1000 且用户等级为 VIP 时,给予 95 折优惠'; 领域 = '电商促销' } }
      Assert-True ($draft.status -eq 'completed') '真实 draft_rule: completed'
      Assert-True ($null -ne $draft.result.rule) '真实 draft_rule: result.rule 为对象'
      Assert-True ($draft.llm_generated.model -eq 'MiniMax-Text-01') '真实 draft_rule: 溯源 model'

      $tests = Api 'POST' '/v1/llm/ops/gen_tests' $token @{
        model = 'MiniMax-Text-01'; request_id = 'uv030-real-tests'
        params = @{ rule = $draft.result.rule } }
      Assert-True ($tests.status -eq 'completed') '真实 gen_tests: completed'
      Assert-True ($tests.result.test_cases.Count -ge 1) "真实 gen_tests: test_cases≥1(实际 $($tests.result.test_cases.Count))"

      $expl = Api 'POST' '/v1/llm/ops/explain_rule' $token @{
        model = 'MiniMax-Text-01'; request_id = 'uv030-real-explain'
        params = @{ rule = $draft.result.rule } }
      Assert-True ($expl.status -eq 'completed') '真实 explain_rule: completed'
      Assert-True ($expl.result.explanation -is [string] -and $expl.result.explanation.Length -gt 0) '真实 explain_rule: explanation 非空'

      $audits3 = Api 'GET' '/v1/audits/llm?limit=50' $token $null
      $realHits = @($audits3.items) | Where-Object { $_.model -eq 'MiniMax-Text-01' -and $_.status -eq 'completed' }
      Assert-True ($realHits.Count -ge 3) "真实三操作落 LlmOpAudit(实际 $($realHits.Count) 条 model=MiniMax-Text-01)"
    } finally {
      Stop-Agent
      Remove-Item Env:MINIMAX_MODEL -ErrorAction SilentlyContinue
    }
  } else {
    Write-Host '[7] 跳过(未指定 -RealLlm;真实 LLM 验收需 -RealLlm + env MINIMAX_API_KEY)'
  }

  # ---- 收尾报告 ----
  Write-Host ''
  if ($failures.Count -eq 0) {
    Write-Host '=== UV-030 V1 验收:全部通过 ===' -ForegroundColor Green
    exit 0
  } else {
    Write-Host "=== UV-030 V1 验收:$($failures.Count) 项失败 ===" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
  }
}
finally {
  Stop-Agent
  if ($script:ruleProc -and -not $script:ruleProc.HasExited) {
    Stop-Process -Id $script:ruleProc.Id -Force -ErrorAction SilentlyContinue
  }
  # 临时目录仅本脚本创建的 rule.db,确认存在后删除
  if (Test-Path $tmpRoot) { Remove-Item -Recurse -Force $tmpRoot }
}
