<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright (C) 2026 EvoRule Project -->

# evorule-rule 文档导航

> 本目录是 evorule-rule 仓的**公开文档**,按 [Diátaxis](https://diataxis.fr/) 框架组织。
> 四类文档各司其职,不要混在一起写。
> 内部工作文档(PLAN/REPORT/验证/调试)走 `D:\knowledge\2-Projects\evorule-rule/`(项目级 vault)。

## 四类文档,各取所需

| 你想做什么 | 看哪里 | 用途 |
|---|---|---|
| 第一次接触,想跑通 | [tutorial/](./tutorial/) | 手把手教学,一步一步带你完成 |
| 有具体问题要解决 | [how-to/](./how-to/) | 任务式指南,以问题为导向 |
| 查 API / 配置 / 命令 | [reference/](./reference/) | 字典式参考,准确但无解释 |
| 想理解为什么这么设计 | [explanation/](./explanation/) | 概念与原理,讨论式 |

**不知道该看哪类?** 先问自己:"我在学 / 我在解决 / 我在查 / 我在理解?" —— 对应到上面四类之一。

## 补充目录

- [adr/](./adr/) — 架构决策记录(ADR),记录重要技术决策与历史
- [operations/](./operations/) — 构建、部署、测试、运维

## 根目录与本目录的关系

按 EvoRule 公开边界约定:
- **根 `*.md`**(README、CHANGELOG、ROADMAP、DESIGN_PHILOSOPHY 等)是 L1 公开
- **本目录 `docs/`** 是 L1 公开的结构化补充
- **各 crate 的 README/SPEC/NOTICE/CHANGELOG** 是 L1 公开
- **`文档/`**(gitignore 保护)是 L2/L3 内部,**新内容改走 vault,本目录保留作历史归档**
