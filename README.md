<!--
  Copyright 2026 EvoRule Project

  SPDX-License-Identifier: AGPL-3.0-or-later

  This file is part of EvoRule, licensed under GNU Affero General Public License v3 or later.
-->

# evorule-rule

**EvoRule 数据治理扩展 — JSON 规则资产库**

> 决策点 ①:数据模型与 Schema
> 在 [evorule](https://gitee.com/evorule/evorule) 基础仓之上,提供规则资产的存储、版本、依赖、检索、审计等数据治理能力。

---

## 在 EvoRule 生态中的位置

EvoRule 是一个多层分仓的项目族,**evorule-rule 是数据治理层扩展**:

| 仓 | 角色 | 当前状态 |
|---|---|---|
| [evorule](https://gitee.com/evorule/evorule) | 基础仓(TCB + Reactor + Governance + CLI) | v0.3.1(已发布) |
| **evorule-rule**(本仓) | 数据治理扩展(规则资产库 + HTTP API) | **v0.2.0(首次对外发布,2026-08-22)** |
| [evorule-server](https://gitee.com/evorule/evorule-server) | 独立 HTTP API 仓 | 长期目标 |
| [evo-agent](https://gitee.com/evorule/evo-agent) | LLM 编排(应用层) | v0.1.0 |

> **本仓不替代 evorule 基础仓,只在其上叠加数据治理能力**。规则最终仍由 evorule 基础仓执行。

## 当前状态(2026-08-22,首次对外发布 v0.2.0)

- **版本**:**v0.2.0** — 数据治理 MVP + REST API + 认证 + LLM 客户端 + PG 双后端 + 密钥管理 全部落地
- **决策点进度**(对应 D:\evo-agent\_design_docs\ 30-47 号):
  - ✅ ① 数据模型 / ② 版本单位 / ③ 版本语义 / ④ 生命周期 / ⑥ 快照包
  - 🔧 ⑦ LLM 客户端(本仓 37 号已落,等 evo-agent 07f E1 serve 联通)
  - 🔧 ⑧ 多租户(token 层认证已落,RBAC + 4 角色递进为 v0.2.x 后续)
  - ⏳ ⑨ 白标 + Docker 化(v0.3+)
- **已落 HTTP handlers**:`auth` / `bundle` / `datasets` / `deps` / `entries` / `keys` / `llm` / `search` / `admin/backend`
- **测试**:108 passed,0 failed(`cargo test --workspace` + `cargo test --workspace --features postgres`)
- **Lint**:`cargo clippy --workspace --all-targets -- -D warnings` 干净
- **Cargo dependencies**:serde / serde_json / thiserror / tracing / sha2 / subtle / rand / base64 / rusqlite(bundled)/ ureq / axum / tokio / tower / http-body-util + 可选 `sqlx`(门控,`--features postgres`)
- **CHANGELOG**:[CHANGELOG.md](CHANGELOG.md) — 详细增量
- **API 稳定性**:0.x 阶段不承诺,SemVer 0.x MINOR 允许含破坏性变更(参见 [VERSION_STRATEGY §2.2](https://gitee.com/evorule/evorule/blob/main/VERSION_STRATEGY.md))

## 文档

按 [Diátaxis](https://diataxis.fr/) 框架组织,详见 [docs/](docs/):

| 入口 | 说明 |
|---|---|
| [docs/introduction.md](docs/introduction.md) | 文档导航(从哪儿开始看) |
| [docs/explanation/doc-boundaries.md](docs/explanation/doc-boundaries.md) | L1/L2/L3 文档边界与公开发布规则 |
| [docs/SUMMARY.md](docs/SUMMARY.md) | 完整目录(mdbook 渲染入口) |

> 公开文档(`docs/`)用 `mdbook build` 在本地渲染;最终 HTML 站点由 Gitee Pages 发布。

## 路线图

- **v0.2** ✅(2026-08-22):Schema 稳定化 + REST API + 认证 + LLM 客户端 + PG schema + 密钥管理 K1/K4/K6 — 详见 [CHANGELOG §0.2.0](CHANGELOG.md#020---2026-08-22)
- **v0.3**(下一步):决策点 ⑤ 数据源绑定契约校验(需 evorule-server 协同)/ ⑦ LLM 服务化(等 evo-agent 07f E1 serve)/ ⑧ 多租户 SQL 层隔离 + RBAC 4 角色
- **v0.5**:性能基线 + 公开 API 锁定
- **v1.0**:决策点 ⑨ 白标 + Docker 化 + 与 evorule 仓同步进入 production-ready 评估

## 许可证

AGPL-3.0-or-later — 与 [evorule 基础仓](https://gitee.com/evorule/evorule) 一致。

商业许可另议:evorulelab@gmail.com

## 联系方式

- **Gitee**:<https://gitee.com/evorule/evorule-rule>
- **Gitee 父仓**:<https://gitee.com/evorule/evorule>
- **邮箱**:evorulelab@gmail.com
- **组织**:EvoRule Lab

---

_本仓是 EvoRule 生态的数据治理扩展。规则不言语,它们只运行。我们是首批见证者。_
