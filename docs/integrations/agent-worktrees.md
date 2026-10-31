# 多代理协作：一个 Agent 一个工作树

> 这条规矩是被一次**侥幸**换来的：W1 阶段三个子代理同时写同一个工作树，
> 只是因为各自文件不相交才没出事。下一次就没这么好运了。

---

## 1. 规则

| # | 规则 | 说明 |
|---|---|---|
| R1 | **一个 Agent 一个 worktree** | 用 `scripts/new-agent-worktree.ps1`（或 `.sh`）创建，路径 `.worktrees/<name>`，分支 `agent/<name>` |
| R2 | **只在自己的工作树里写** | 传给子代理的提示词里必须写明**绝对路径**，并附一句"只允许修改该目录下的文件" |
| R3 | **Agent 自己提交** | 子代理完成时在自己的分支提交；队长用 `git diff main..agent/<name>` 审阅 |
| R4 | **队长只合，不并行写** | 合并期间队长不再派新任务到同一区域 |
| R5 | **单写者文件** | 见 §3 的清单：任何 Agent 要改都必须先向队长申请 |
| R6 | **删树前先合并** | `git worktree remove` 之前确认分支已合入，否则改动会随目录消失 |

---

## 2. 用法

### 创建

```powershell
# Windows
pwsh scripts/new-agent-worktree.ps1 -Name rust-parser -Base main
# 输出：.worktrees\rust-parser  (branch: agent/rust-parser)
```

```bash
# Linux / macOS / CI
bash scripts/new-agent-worktree.sh rust-parser main
```

### 派活时的提示词模板

```
你的工作目录是 D:\...\yunong\.worktrees\rust-parser（git worktree，分支 agent/rust-parser）。
只允许修改该目录下的文件；不要切换到主工作树，也不要修改其他工作树。
完成后在该目录内 git add -A && git commit，并回报：分支名、commit hash、改动文件清单。
```

### 收尾

```powershell
git -C . diff main..agent/rust-parser            # 审阅
git merge --no-ff agent/rust-parser              # 合入
git worktree remove .worktrees/rust-parser       # 删树（分支保留）
git branch -d agent/rust-parser                  # 可选：删分支
```

---

## 3. 单写者文件（任何 Agent 改动前必须申请）

这些文件是"全局契约"，并行改必冲突，而且冲突后很难判断谁对：

| 路径 | 为什么 |
|---|---|
| `crates/shared/**` | 端、云、文档的唯一契约源（ADR-13） |
| `Cargo.toml` / `Cargo.lock` | 依赖与 workspace 结构 |
| `docs/04-ADR.md` | 架构决策唯一事实来源 |
| `docs/0{1,2,3}-*.md` | 产品/设计/接口三份基线 |
| `.github/workflows/**` | CI 行为，改错会让所有人的 PR 变红 |
| `app/pubspec.yaml` | 客户端依赖 |

---

## 4. 为什么不用"合并队列"或"多克隆"

- **多克隆**：每个克隆要重新装依赖、重新编译，磁盘与时间成本翻倍；worktree 共享同一个 `.git`，切换成本接近零。
- **合并队列**：需要托管平台能力，小团队用不上；worktree + 单写者清单足够。

---

## 5. 常见坑

1. **主工作树被子代理偷偷写了**：提示词里必须给绝对路径，并要求它 `pwd` 自证。
2. **忘了 `.worktrees/` 加进 `.gitignore`**：会出现"工作树里套工作树"的诡异状态。
3. **在 worktree 里跑 `cargo test`**：会重新编译一份 `target/`（每个 worktree 一份）。要么接受，要么用 `CARGO_TARGET_DIR` 指向共享目录。
4. **删树时还有未提交改动**：`git worktree remove` 会拒绝；先提交或 `--force`（force 会丢改动，慎用）。
5. **Windows 路径过长**：把仓库放在盘符根附近（如 `D:\yunong`），别放深层目录。
