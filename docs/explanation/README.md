<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright (C) 2026 EvoRule Project -->

# explanation/ — 原理与设计讨论

> **面向想理解"为什么"的开发者**:设计动机、概念讨论、权衡。

适合:用着没问题,但想理解背后想法的人;做新设计前的参考资料。

## 写什么

- 为什么这么设计 / 不那么设计
- 概念之间的关系、术语定义
- 历史演变、曾考虑过但放弃的方案

## 不要写在这里

- ❌ "怎么用" → 去 [tutorial/](../tutorial/) 或 [how-to/](../how-to/)
- ❌ API 字段说明 → 去 [reference/](../reference/)
- ❌ 重要决策的正式记录 → 去 [adr/](../adr/)(ADR 是**不可变历史**,explanation 是**讨论**)

## 命名规范

`主题-副题.md`(如 `why-tcb-ignored.md`、`why-blake3-audit-chain.md`),
文件名可以透露"立场",比如带 `why-` 前缀。
