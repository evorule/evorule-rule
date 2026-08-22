<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright (C) 2026 EvoRule Project -->

# 架构决策记录(ADR)

> 记录 evorule-rule 仓中**重要的架构决策**及其历史。

## 什么是 ADR

ADR(Architecture Decision Record)是一种轻量级文档,记录:

- 我们面临什么问题
- 考虑了哪些方案
- 最终选了哪个、为什么
- 选完后带来哪些后果

每一份 ADR 都是**不可变的历史快照**。决策若变更,写新 ADR 并 supersede 旧的,**不要回头改旧文件**。

## 写作规范

- **文件名**:`NNNN-kebab-case-title.md`,序号单调递增,不重用
- **模板**:复制 [template.md](./template.md) 开始写
- **状态**:Proposed → Accepted → (Deprecated | Superseded by ADR-XXXX)
- **完成后**:把新 ADR 链接加进下方目录

## 目录

(暂无,等第一个 ADR 落地后补)

## 何时写 ADR

- 选了某个框架 / 库 / 语言版本(如"为什么 Rust 1.74+ 而非 nightly")
- 引入或修改了**架构层面**的设计(如"数据模型分层原则")
- 改变了产品边界或外部接口(如"v0.2 改 JSON Schema 字段")
- 拒绝了某个看似合理的方案(如"不做实时协作,理由是 …")

**不要**为琐碎的实现细节写 ADR(变量命名、内部重构不算)。
