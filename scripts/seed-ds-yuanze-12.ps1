# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
# evorule-rule — T6 阻断项② 治理侧 ds-yuanze-01 扩展至完整 12 条(可复现)
#
# 前提:evorule-rule-serve 已在 18081 运行,且以引导管理员 admin/admin123456 初始化:
#   .\target\debug\evorule-rule-serve.exe --db ./data/rule.db --port 18081 `
#       --admin-user admin --admin-password admin123456 `
#       --allowed-origins "http://localhost:5174,http://127.0.0.1:5174" `
#       --secret evorule-rule-acceptance-secret
#
# 用法:.\scripts\seed-ds-yuanze-12.ps1 [-RulesJson <path>]
#   RulesJson 缺省: D:\yuanze-demos\server-config\rules\yuanze_rules.json(Phase 1 基线,12 条 branch)
#
# 期望: ds-yuanze-01 走 create_version(major → 新版本 Draft) → 声明 data_dependencies
#       (运行所需全集 7 个: inverse_kinematics_solver/robot_move_joints/llm_advisor/
#        shadow_ik_solver/sampling_service/rule_sandbox/config_persist, C1 补全) →
#       灌入完整 12 条(rule-compute-ik 重建 v2, 其余 v1;已存在则沿用) →
#       Draft→Candidate→Active→Published(二次确认)。回读: 12 条 / Published / 版本链可见。
#
# 幂等性: 已存在条目沿用;数据集已含 12 条 + Published + 依赖声明齐全(7 服务)
#       → 报告就绪跳过(不重复升版)。
# 注: 灌入后需重新发布才会更新对外 Published 版本内容(T6 端到端验收依赖)。

param(
  [string]$Base = 'http://127.0.0.1:18081',
  [string]$RulesJson = 'D:\yuanze-demos\server-config\rules\yuanze_rules.json',
  [string]$DsId = 'ds-yuanze-01'
)

$ErrorActionPreference = 'Stop'

function Api($Method, $Path, $Token, $BodyObj) {
  $headers = @{}
  if ($Token) { $headers['Authorization'] = "Bearer $Token" }
  $params = @{ Uri = "$Base$Path"; Method = $Method; Headers = $headers }
  if ($null -ne $BodyObj) {
    $params['ContentType'] = 'application/json'
    $params['Body'] = ($BodyObj | ConvertTo-Json -Depth 100 -Compress)
  }
  return Invoke-RestMethod @params
}

# ---- 1. 登录 ----
$login = Api 'POST' '/v1/auth/login' $null @{ tenant_id = 'default'; username = 'admin'; password = 'admin123456' }
$token = $login.access_token
Write-Host '[1] 登录成功(admin)'

# ---- 2. 读取 Phase 1 基线 12 条 branch ----
$src = Get-Content -Raw $RulesJson | ConvertFrom-Json
if ($src.kind -ne 'rule_set') { throw "yuanze_rules.json kind 应为 rule_set, 实际: $($src.kind)" }
$branches = @($src.transform)
if ($branches.Count -ne 12) { throw "yuanze_rules.json 应含 12 条 transform, 实际: $($branches.Count)" }

# entry_id 映射(按 branch 的 instruction_type 匹配, 保持与 Phase 1 基线一致)
$entryMap = @(
  @{ id = 'rule-compute-ik';         itype = 'compute_ik' },
  @{ id = 'rule-robot-move';         itype = 'robot_move' },
  @{ id = 'rule-validate-precision'; itype = 'validate_precision' },
  @{ id = 'rule-audit-alert';        itype = 'audit_alert' },
  @{ id = 'rule-shadow-validate';    itype = 'shadow_validate' },
  @{ id = 'rule-safety-rollback';    itype = 'safety_rollback' },
  @{ id = 'rule-sampling-decider';   itype = 'sampling_decider' },
  @{ id = 'rule-audit-compactor';    itype = 'audit_compactor' },
  @{ id = 'rule-evolution-scanner';  itype = 'evolution_scanner' },
  @{ id = 'rule-generate-patch';     itype = 'generate_patch' },
  @{ id = 'rule-sandbox-validate';   itype = 'sandbox_validate' },
  @{ id = 'rule-hotload-patch';      itype = 'hotload_patch' }
)
if ($entryMap.Count -ne $branches.Count) { throw "entryMap 与 branches 数量不一致" }

# 静态服务条目 → 条目级 data_source_binding(三层绑定: 规则符号 → 数据集声明 → 执行侧绑定)
$staticBindings = @{
  'rule-audit-alert'      = @(@{ rule_ref = 'rule_body.io_request[llm_advisor]'; service_name = 'llm_advisor' })
  'rule-safety-rollback'  = @(@{ rule_ref = 'rule_body.io_request[llm_advisor]'; service_name = 'llm_advisor' })
  'rule-generate-patch'   = @(@{ rule_ref = 'rule_body.io_request[llm_advisor]'; service_name = 'llm_advisor' })
  'rule-sandbox-validate' = @(@{ rule_ref = 'rule_body.io_request[rule_sandbox]'; service_name = 'rule_sandbox' })
  'rule-hotload-patch'    = @(@{ rule_ref = 'rule_body.io_request[config_persist]'; service_name = 'config_persist' })
}

# ---- 3. 数据集状态判定与版本推进 ----
$ds = $null
try { $ds = Api 'GET' "/v1/datasets/$DsId" $token $null } catch { $ds = $null }
if ($null -eq $ds) {
  throw "数据集 $DsId 不存在: 请先运行 .\scripts\acceptance-governance.ps1 或创建数据集"
}

$currentEntries = @(Api 'GET' "/v1/datasets/$DsId/entries?limit=100" $token $null).items
$existingIds = @($currentEntries | ForEach-Object { $_.entry_id })
$haveAll = (@($entryMap | Where-Object { $existingIds -contains $_.id })).Count -eq $entryMap.Count
# C1: 依赖声明完整性 = 运行所需全集 7 服务(动态引用服务也在核对范围)
$needServices = @('inverse_kinematics_solver','robot_move_joints','llm_advisor','shadow_ik_solver','sampling_service','rule_sandbox','config_persist')
$curDeps = Api 'GET' "/v1/deps/datasets/$DsId" $token $null
$depsComplete = (@($curDeps.services | Where-Object { $needServices -contains $_.service_name })).Count -eq $needServices.Count
if ($haveAll -and $ds.lifecycle.status -eq 'Published' -and $depsComplete) {
  Write-Host "[3] ds-yuanze-01 已含完整 12 条 + 依赖声明齐全(7 服务)且 Published, 就绪跳过"
} elseif ($ds.lifecycle.status -eq 'Draft') {
  # 已在 Draft 编辑态(上次中断或新建数据集)→ 直接继续灌入, 不重复升版
  Write-Host "[3] 已在 Draft 编辑态, 直接继续 (current=$($ds.versioning.current))"
} else {
  # 发布后内容变更 → 创建新版本(法规条款级 Major; 34 号 §6: 新版本 lifecycle 重置 Draft)
  $v = Api 'POST' "/v1/datasets/$DsId/versions" $token @{ kind = 'major' }
  Write-Host "[3] 创建新版本 $($v.new_version) (Draft); 版本链: $($v.chain -join ',')"
  $ds = Api 'GET' "/v1/datasets/$DsId" $token $null
}

# ---- 4. 声明数据集级 data_dependencies(三层绑定层 1; 供执行侧服务绑定核对 T6 阻断项①) ----
# C1 补全: 从 3 个补全至运行所需全集 7 个(动态引用服务也在声明/核对范围; sensitive 与治理侧
# 服务目录 SSOT 对齐: 仅 llm_advisor 涉外部 LLM API 标记 true, 其余 false)
$deps = @{
  inputs   = @()
  services = @(
    @{ service_name = 'inverse_kinematics_solver'; sensitive = $false; description = '机器人逆运动学求解(Phase 1 原生)' },
    @{ service_name = 'robot_move_joints';         sensitive = $false; description = '机器人关节移动(确定性, Phase 1 原生)' },
    @{ service_name = 'llm_advisor';               sensitive = $true;  description = 'LLM 建议服务(sensitive: 涉及外部 LLM API)' },
    @{ service_name = 'shadow_ik_solver';          sensitive = $false; description = '影子 IK 求解(对照验证)' },
    @{ service_name = 'sampling_service';          sensitive = $false; description = '采样服务' },
    @{ service_name = 'rule_sandbox';              sensitive = $false; description = '规则沙箱验证服务' },
    @{ service_name = 'config_persist';            sensitive = $false; description = '规则热加载持久化服务' }
  )
}
Api 'PUT' "/v1/deps/datasets/$DsId" $token $deps | Out-Null
Write-Host '[4] data_dependencies 已声明: 7 服务(运行所需全集, C1)'

# ---- 5. 灌入 12 条(幂等: 已存在沿用; rule-compute-ik 重建 v2 以对齐 Phase 1 基线含 tolerance) ----
for ($i = 0; $i -lt $entryMap.Count; $i++) {
  $m = $entryMap[$i]
  $branch = $branches[$i]
  # 校验映射的 instruction_type 与 branch 实际一致(防错位)
  $itype = $branch.params.domain.instruction_type
  if ($itype -ne $m.itype) {
    throw "entryMap[$i] 期望 $($m.itype), branch 实际 $itype —— 映射错位, 终止"
  }
  $body = @( , $branch )   # rule_body = transform 数组(单 branch)
  $binding = @()
  if ($staticBindings.ContainsKey($m.id)) { $binding = $staticBindings[$m.id] }

  $exist = $null
  try { $exist = Api 'GET' "/v1/entries/$($m.id)" $token $null } catch { $exist = $null }
  if ($null -ne $exist) {
    # rule-compute-ik 用 v2 重建(Phase 1 基线完整版); 其余沿用
    if ($m.id -eq 'rule-compute-ik') {
      $payload = @{
        dataset_id = $DsId
        entry_id   = $m.id
        version    = 2
        domain     = 'robot'
        tags       = @('yuanze', 'robot')
        rule_body  = $body
        data_source_binding = $binding
      }
      try {
        Api 'POST' '/v1/entries' $token $payload | Out-Null
        Write-Host "[5] $($m.id) v2 重建(Phase 1 基线含 tolerance)"
      } catch {
        Write-Host "[5] $($m.id) v2 已存在或重建失败: $($_.Exception.Message)"
      }
    } else {
      Write-Host "[5] $($m.id) 已存在, 沿用"
    }
    continue
  }
  $payload = @{
    dataset_id = $DsId
    entry_id   = $m.id
    version    = 1
    domain     = 'robot'
    tags       = @('yuanze', 'robot')
    rule_body  = $body
    data_source_binding = $binding
  }
  Api 'POST' '/v1/entries' $token $payload | Out-Null
  Write-Host "[5] 已灌入 $($m.id) (v1)"
}

# ---- 6. 5 态生命周期 + 审批发布 ----
$ds = Api 'GET' "/v1/datasets/$DsId" $token $null
if ($ds.lifecycle.status -ne 'Published') {
  if ($ds.lifecycle.status -eq 'Draft') {
    Api 'PATCH' "/v1/datasets/$DsId/lifecycle" $token @{ to = 'candidate' } | Out-Null
    Write-Host '[6] Draft -> Candidate'
    Api 'PATCH' "/v1/datasets/$DsId/lifecycle" $token @{ to = 'active' } | Out-Null
    Write-Host '    Candidate -> Active'
    Api 'POST' "/v1/datasets/$DsId/publish" $token @{ confirm = $true; reason = 'T6 阻断项②: ds-yuanze-01 扩展至完整 12 条(Phase 1 基线)' } | Out-Null
    Write-Host '    Active -> Published(二次确认)'
  } else {
    Write-Host "[6] 当前状态 $($ds.lifecycle.status) 非 Draft, 跳过发布流程(需人工介入)"
  }
} else {
  Write-Host '[6] 已是 Published, 跳过发布'
}

# ---- 7. 回读验收 ----
$final = Api 'GET' "/v1/datasets/$DsId" $token $null
$entries = @(Api 'GET' "/v1/datasets/$DsId/entries?limit=100" $token $null).items
$versions = Api 'GET' "/v1/datasets/$DsId/versions" $token $null
$depsCheck = Api 'GET' "/v1/deps/datasets/$DsId" $token $null

Write-Host ''
Write-Host '===== 验收结果(T6 阻断项②) ====='
Write-Host "生命周期: $($final.lifecycle.status) / 版本: current=$($versions.current) chain=$($versions.chain -join ',')"
Write-Host "条目数: $($entries.Count)(期望 12)"
Write-Host "deps services: $(($depsCheck.services | ForEach-Object { $_.service_name }) -join ',')"
$entries | Sort-Object entry_id | ForEach-Object { Write-Host "  - $($_.entry_id) v$($_.version) [$($_.status)]" }
Write-Host '=================================='
if ($entries.Count -ne 12) { throw "条目数 $($entries.Count) != 12 —— 灌入未达预期" }
