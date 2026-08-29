# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — Phase 2 F1 治理接线验收脚本(可复现)
#
# 前提:evorule-rule-serve 已在 18081 运行,且以引导管理员 admin/admin123456 初始化(默认租户 default):
#   .\target\debug\evorule-rule-serve.exe --db ./data/rule.db --port 18081 `
#       --admin-user admin --admin-password admin123456 `
#       --allowed-origins "http://localhost:5174,http://127.0.0.1:5174" `
#       --secret evorule-rule-acceptance-secret
#
# 用法:.\scripts\acceptance-governance.ps1
# 期望:数据集 ds-yuanze-01 走 Draft→Candidate→Active→Published,
#       灌入 1 条 yuanze 业务规则(compute_ik),版本链可见。
#
# 注意:以 admin 单账号跑全流程验证闭环;角色分离(rule_engineer 建/灌/候选,
#       approver 激活/发布)由 evorule-rule 后端强制,已由后端单测覆盖
#       (test_publish_requires_second_confirm_and_tenant_guard)。

$ErrorActionPreference = 'Stop'
$base = 'http://127.0.0.1:18081'
$dsId = 'ds-yuanze-01'
$entryId = 'rule-compute-ik'

function Api($Method, $Path, $Token, $BodyObj) {
  $headers = @{}
  if ($Token) { $headers['Authorization'] = "Bearer $Token" }
  $params = @{ Uri = "$base$Path"; Method = $Method; Headers = $headers }
  if ($null -ne $BodyObj) {
    $params['ContentType'] = 'application/json'
    $params['Body'] = ($BodyObj | ConvertTo-Json -Depth 30 -Compress)
  }
  return Invoke-RestMethod @params
}

# ---- 1. 登录 ----
$login = Api 'POST' '/v1/auth/login' $null @{ tenant_id = 'default'; username = 'admin'; password = 'admin123456' }
$token = $login.access_token
Write-Host '[1] 登录成功(admin)'

# ---- 2. 创建数据集(已存在则沿用) ----
$ds = $null
try { $ds = Api 'GET' "/v1/datasets/$dsId" $token $null } catch { $ds = $null }
if ($null -eq $ds) {
  $ds = Api 'POST' '/v1/datasets' $token @{
    dataset_id = $dsId
    name       = 'yuanze 机器人质量管控规则集'
    description = '基于 yuanze_rules.json 的机器人质量管控规则(Phase 2 治理接线验收)'
    domain     = @('robot', 'quality')
    tags       = @('yuanze', '机器人')
    visibility = 'private'
  }
  Write-Host '[2] 数据集已创建'
} else {
  Write-Host '[2] 数据集已存在,沿用'
}
Write-Host "    当前状态: $($ds.lifecycle.status) / $($ds.versioning.current)"

# ---- 3. 灌入 yuanze 业务规则(compute_ik,evorule 原生 JSON 零转译) ----
$ruleBody = @(
  @{
    type   = 'branch'
    params = @{
      domain  = @{ type = 'instruction'; instruction_type = 'compute_ik' }
      on_true = @(
        @{
          type   = 'branch'
          params = @{
            domain  = @{ type = 'exists'; path = '__exec__.payload.__io_results__.call_service' }
            on_true = @(
              @{ type = 'set'; params = @{ attr = 'service_result'; operation = 'set'; value = '__exec__.payload.__io_results__.call_service' } }
            )
            on_false = @(
              @{ type = 'set'; params = @{ attr = '_ik_args.target_pose'; operation = 'set'; value = '__exec__.instruction.params.target_pose' } },
              @{ type = 'set'; params = @{ attr = '_ik_args.solver_type'; operation = 'set'; value = '__exec__.instruction.params.solver_type' } },
              @{
                type   = 'io_request'
                params = @{ io_type = 'call_service'; service_name = '__exec__.instruction.params.service_name'; args = '__exec__.payload._ik_args' }
              }
            )
          }
        }
      )
    }
  }
)
$entry = $null
try { $entry = Api 'GET' "/v1/entries/$entryId" $token $null } catch { $entry = $null }
if ($null -eq $entry) {
  $entry = Api 'POST' "/v1/datasets/$dsId/entries" $token @{
    entry_id = $entryId
    version  = 1
    domain   = 'robot'
    rule_body = $ruleBody
  }
  Write-Host "[3] 已灌入规则 $entryId"
} else {
  Write-Host "[3] 规则 $entryId 已存在,沿用"
}

# ---- 4. 5 态生命周期 + 审批发布 ----
Api 'PATCH' "/v1/datasets/$dsId/lifecycle" $token @{ to = 'candidate' } | Out-Null
Write-Host '[4] Draft -> Candidate'
Api 'PATCH' "/v1/datasets/$dsId/lifecycle" $token @{ to = 'active' } | Out-Null
Write-Host '    Candidate -> Active'
Api 'POST' "/v1/datasets/$dsId/publish" $token @{ confirm = $true; reason = 'Phase 2 治理接线验收:二次确认通过' } | Out-Null
Write-Host '    Active -> Published(二次确认)'

# ---- 5. 回读验收 ----
$final = Api 'GET' "/v1/datasets/$dsId" $token $null
$entries = Api 'GET' "/v1/datasets/$dsId/entries?limit=100" $token $null
$versions = Api 'GET' "/v1/datasets/$dsId/versions" $token $null

Write-Host ''
Write-Host '===== 验收结果 ====='
Write-Host "生命周期: $($final.lifecycle.status)"
Write-Host "状态历史: $($final.lifecycle.state_history.Count) 步"
$final.lifecycle.state_history | ForEach-Object { Write-Host "  $($_.from) -> $($_.to) @ $($_.at) by $($_.by): $($_.cause)" }
Write-Host "版本: current=$($versions.current) chain=$($versions.chain -join ',')"
Write-Host "条目数: $($entries.items.Count)($($entries.items[0].entry_id) v$($entries.items[0].version))"
Write-Host '=================================='
