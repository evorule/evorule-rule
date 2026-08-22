<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright (C) 2026 EvoRule Project -->

# reference/ — 字典式参考

> **面向需要查阅确切信息的开发者**:API、CLI、配置项、字段。

适合:用的时候翻一翻,看完就走,不需要从头读到尾。

## 写什么

- **准确、完整**:字段、类型、默认值、约束
- **简洁、无废话**:不解释为什么,只写"是什么"
- **尽量自动生成**:API/CLI 文档从代码或注解生成,**不**手抄

## 不要写在这里

- ❌ 教程式引导 → 去 [tutorial/](../tutorial/)
- ❌ 任务步骤 → 去 [how-to/](../how-to/)
- ❌ "为什么这么设计" → 去 [explanation/](../explanation/)

## 命名规范

按"对象"命名(API 名 / CLI 子命令 / 配置文件名),**不**按"任务"命名。
