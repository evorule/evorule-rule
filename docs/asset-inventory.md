# data/ 资产清单（人类可读摘要）

> 机读正本：[`data/ASSET-INVENTORY.json`](../data/ASSET-INVENTORY.json)（git-tracked，白名单收编）。
> 来源：尽调正本《evorule-rule-回合2尽调-资产清册与宪法保护-20261015.md》§1.2 八批次归属 / §1.3 敏感定性 / §1.4 库量级。
> 批次：RG-1 守护补齐批 件A1（2026-10-15）。
> 铁律：**data/ 每份数据都是资产，零删除**；坏件不删，源头对账补齐。

## 口径

- 实存资产 **90 件** = 顶层 **83**（尽调正本口径）+ `domain_schemas/` **7**（O-461 已入 git 半保护面）。
- 清单另自登记本体 1 件，机读条目合计 **91**。
- 守护级分布：**strict 87 / live 1 / sensitive 3**。

## 守护级三档

| 档 | 语义 | 件数 | 代表 |
|---|---|---|---|
| strict | 时点快照件：消失或改名=红灯（内容不比对） | 87 | bundle×7、脚本 34、输出 35、domain_schemas×7、smoke.db、serve 日志×2、清单本体 |
| live | 活体件：存在性+只读试连（mode=ro），不比对内容/尺寸 | 1 | rule.db（dev 栈活体正本，27 表/827 行） |
| sensitive | 敏感件：只查存在性，**校验器永不读内容** | 3 | admin-credentials.local.txt、jwt_secret.key、token.txt |

## 八批次归属（源头可回溯）

| 批次 | 资产组 |
|---|---|
| AE模板治理链+e1污染修复 | t3 系×15 脚本、bundle×6、t3 配对输出及定义快照 |
| H14回收链 | verify_cleanup.py |
| 北极星/A1-1知识资产化 | northstar-anchor v1.0/v1.1、e2e-knowledge-v1 |
| P0-1 live E2E | p01_live_e2e.py |
| 统帅T2背书批 | tb-gate-v1-bundle.json（human 背书时代唯一留证） |
| O-424清洗批18081重建 | serve-18081 双日志、rule.db 尾写 |
| ALE/SR-14线 | eval-bridge/task-card/test、eval-ruleset |
| T0'全链复验 | t0 系×6 脚本 + 9 件配对输出 |
| 未考（源头待对账） | t4 族×11、门禁核验对×4、散件（probe_datasets/show_ins）、敏感三件、smoke.db——**如实标注待考，不做虚构归属** |

## 校验器（件A2）

- 落点：[`tools/check_data_assets.py`](../tools/check_data_assets.py)（纯 stdlib）。
- 三态：少件→红灯 exit 1；清单外新件→警告 stderr exit 0；rule.db 只读试连失败→红灯。
- 已接入宪法仓 verify-all 第十三步（件A3，RG-1）。
- 守护语义边界（如实）：防「消失/改名/漂移出仓」，不防「内容篡改原位保留」——内容级防篡改由各件自身机制承载（bundle BLAKE3 全包哈希、rule.db auth_audits 审计链）。

## 更新纪律

1. data/ 新增任何文件，必须同步补登记 `ASSET-INVENTORY.json`（校验器 WARN 提醒，不红灯）；
2. data/ 任何件**不删不改不移**；确需处置先做源头对账并报统帅；
3. 敏感三件仅登记元数据，任何工具与流程不得读取其内容；
4. 清单本体为 git-tracked 管理文件，修订随 RG 档号 commit。
