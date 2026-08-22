# evorule-rule 发版工作流

> 本仓的 commit / push / tag / version 节奏约定。一次搞清楚,以后不用再被"何时发版"困扰。

## 1. 核心原则(对齐 evorule 仓 VERSION_STRATEGY)

| 节奏 | 频率 | 触发 |
|---|---|---|
| **commit** | 每天 1+ 个 | 每写完 1 个模块 / 修 1 个 bug / 改 1 个 API |
| **push** | 跟 commit 同步 | **自动**(post-commit hook) |
| **tag** | 0.x 早期每 2-4 周 1 个 MINOR | "对外宣布这批工作 ready" |
| **version bump** | 跟 tag 同步 | `Cargo.toml [package].version` 在 tag 时刻改 |
| **crates.io publish** | 跟 tag 同步(可选) | 同上 |

> **不要把 commit 和 tag 混为一谈。** commit 是开发历史,tag 是发布历史。

## 2. 日常 commit(自动 push,绝不堆积)

```bash
# 写完一个模块
git add .
git commit -m "feat: 加 LDAP 认证"
# ↑ post-commit hook 自动 git push origin main
```

**hook 装好后,根本不需要手动 `git push`**。working tree 永远保持推送同步,绝不会出现"积累大量未推送内容"的情况。

### hook 装法(新机器 / 重新 clone 后)

```bash
# Windows PowerShell
pwsh scripts/install-hooks.ps1

# 或手动
git config core.hooksPath scripts/hooks
```

### hook 行为细则

- **post-commit**: commit 完立刻 push 当前 branch
  - 无 origin 远端 → 跳过(允许本地仓 / 离线开发)
  - amend commit → 跳过(避免循环)
  - push 失败 → 警告但不阻塞 commit(允许网络断时继续开发,下次 push 自动累积)
- **pre-push**: push 前在 main/master 分支跑 `cargo test --workspace`
  - 非 Rust 仓 → 跳过
  - 非主分支 → 跳过(避免开发分支反复编译拖慢)
  - 测试失败 → 阻止 push(防止 working commit 没编译就推)

## 3. 发版(tag + version bump)

**何时打 tag**:
- ✅ 1 个新功能完成 → MINOR(0.X.0,如 0.2.0 → 0.3.0)
- ✅ 1 个 bug 修完 → PATCH(0.0.X,如 0.2.0 → 0.2.1)
- ❌ 重构 / 文档 / 测试 / build 配置 → 不打 tag,commit 即足够

**判断决策树**:

```
改动内容?
├─ 新功能 / 新模块 / 新 endpoint → MINOR (0.X.0)
├─ 修 bug / 改文档 / 内部重构 → PATCH (0.0.X)
└─ 0.x 阶段破坏性变更(API 改名) → MINOR(不升 MAJOR,§2.2)
```

**发版流程**(5 步,建议用 `scripts/release.ps1` 自动化,见 §4):

1. 改 `Cargo.toml [package].version`(SSOT)
2. 在 `CHANGELOG.md` 加新章节(Keep a Changelog 格式)
3. 确认 `LICENSE` / `README.md` 版本号同步
4. `git add . && git commit -m "chore(release): vX.Y.Z — ..."`(post-commit hook 自动 push)
5. `git tag -a vX.Y.Z -m "..." && git push origin vX.Y.Z`

## 4. 自动化建议(未来)

`scripts/release.ps1` 可以包办 §3 的 5 步,减少手工出错:

```powershell
# 用法: pwsh scripts/release.ps1 -NewVersion 0.3.0
# 1. bump Cargo.toml
# 2. (调用) 用户在 CHANGELOG.md 加章节(自动模板)
# 3. (调用) README.md 更新版本号
# 4. 1 个 release commit(走 post-commit hook 自动 push)
# 5. 1 个 annotated tag
# 6. push tag 到 origin
```

待 v0.3.0 发版时实现。

## 5. 跟 evorule 仓(基础仓)的发版协调

evorule-rule 是 **evorule 仓的扩展**,但发版独立:

| 仓 | 版本号 | 发版频率 | crates.io |
|---|---|---|---|
| evorule | 0.3.1(2026-08-18) | 跟 evorule-rule 异步 | ✅ 已发 |
| evorule-rule | 0.2.0(2026-08-22) | 跟 evorule 异步 | ❌ 用户决定暂不发 |

evorule-rule 发版**不需要**等 evorule 仓,反之亦然。但 schema/接口变更要相互通知(在 CHANGELOG 显式记录)。

## 6. 不要做的事

- ❌ commit 后**忘了 push**(装 hook 后自动解决)
- ❌ 改 `Cargo.toml [package].version` 但**忘改 CHANGELOG / README**(发版 checklist 必备)
- ❌ 每 commit 都打 tag(commit 不是 release)
- ❌ 0.x 阶段升 MAJOR(0.x 阶段允许破坏性变更,SemVer §2.2)
- ❌ 跳过 `cargo test` 直接 push(pre-push hook 自动阻止)
- ❌ 把 `target/` / `docs/book/` commit 进仓(.gitignore 已挡)

## 7. 检查清单(发版前)

- [ ] `Cargo.toml [package].version` = `X.Y.Z`
- [ ] `CHANGELOG.md` 有 `## [X.Y.Z] - YYYY-MM-DD` 章节(🆕/🔄/🐛/📦 分类)
- [ ] `README.md` 状态、决策点进度、路线图三处版本号同步
- [ ] `LICENSE` 存在(AGPL-3.0-or-later,首次发版必备)
- [ ] `cargo test --workspace` 0 failed
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 干净
- [ ] `git status` clean(没有 uncommitted / untracked)
- [ ] 当前 branch = `main`,远端同步
- [ ] 1 个 `chore(release): vX.Y.Z` commit
- [ ] 1 个 `vX.Y.Z` annotated tag + push 到 origin

---

_本工作流是 v0.2.0 release 配套,解决"积累大量未推送内容"问题。_
_hooks 装好后,日常 commit 自动 push,working tree 永远干净。_
