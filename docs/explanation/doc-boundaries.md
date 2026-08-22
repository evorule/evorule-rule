<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright (C) 2026 EvoRule Project -->

# 文档约定与边界(L1 严格公开)

> evorule-rule 文档系统的"什么放哪"决策与原因。
> 如果你是贡献者,想加文档,**先读这个**再决定放哪里。

## 三个层级

| 层级 | 含义 | 放哪里 | 谁能看 |
|---|---|---|---|
| **L1 严格公开** | "产品是什么 / 怎么用 / 公开的设计" | 项目仓内,根 `*.md` + `docs/` + 各 crate `README/SPEC/NOTICE/CHANGELOG` | 所有人(commit 即发布) |
| **L2 内部** | "我们在做什么 / 怎么排期 / 内部决策" | `D:\knowledge\2-Projects\evorule-rule/` vault(本地 + Gitee 私人仓) | 你自己 + 团队 |
| **L3 敏感** | "未发布功能 / 客户数据 / 安全相关" | vault 4-Archive/ + 必要时加密 | 你自己 |

**黄金规则**:**默认 L1**,只有"含敏感信息"或"未拍板"才下沉到 L2。

## L1 的具体边界(可机器校验)

**L1 包含**(在 Cargo.toml `exclude` 之外):
- 根 `*.md`(README、CHANGELOG、ROADMAP、DESIGN_PHILOSOPHY 等)
- 根 `docs/` 全部内容(本目录)
- 各 crate 的 `README.md`、`SPEC.md`(如有)、`NOTICE.md`、`CHANGELOG.md`

**L1 不包含**(`Cargo.toml` 的 `exclude` 列表):
- 验证日志、调试输出、build artifact
- 运行时数据
- 原始证据、日志类

## 为什么这样分

- **L1 commit = 发布**:用户 clone 仓就能看到。**错的内容会立刻让项目掉价**。
- **L2 写完再考虑公开**:PLAN/REPORT 在 vault 里写,等"public doc time"再写干净版回根。
- **L3 永远不公开**:未来计划、未签 NDA 的客户功能讨论。

## 不要做的事

- ❌ 在根 `*.md` 写"我们在考虑" / "TODO" / "未来计划" → 公开了
- ❌ 在 `docs/` 写"内部代号 XYZ" / "客户 A 反馈" → 公开了
- ❌ 在 vault 写"已经发布的功能细节" → vault 没版本控制发布流程,会跟 L1 错位

## 自动化(预留)

`check_doc_safety` 脚本(待写)可以扫 L1 文件,看有没有 TODO / FIXME / 待补 等内部话术,作为 CI 检查。

## 历史

- 2026-08-22: 本约定落地(evorule-rule 仓)
