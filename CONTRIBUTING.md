# 贡献指南

> 云农之窗：**凭据不出端，隐私在本地，知识在云端**。
> 动手之前先读 `docs/04-ADR.md` 的前三条——三条信任域是所有设计的边界。

---

## 1. 分支策略

| 分支 | 用途 | 规则 |
|---|---|---|
| `main` | 可发布版本 | 保护分支，**只能经 PR 合入**，禁止直接 push / force push |
| `dev` | 日常集成分支 | CI 全绿才允许合入；功能分支默认从这里切 |
| `feature/<短描述>` | 单个功能或修复 | 短生命周期，合入后删除 |
| `release/1.x` | 发布后修复 | 只修 bug，不再加功能 |
| `refactor/v1` | 1.0 大重构长分支 | 按 WBS 里程碑定期合入 `dev`，**禁止长期脱钩** |

流程：`feature/*` → PR → `dev` → 发布时 PR → `main` → 打 `v*` 标签触发发布工作流。

## 2. Commit 约定（Conventional Commits）

格式：`<type>(<scope>): <subject>`

- **type**：`feat` / `fix` / `docs` / `refactor` / `test` / `perf` / `build` / `ci` / `chore`
- **scope**：`core` / `server` / `shared` / `app` / `bench` / `docs` / `ci` / `deps`
- **subject**：祈使句、不带句号，中英文皆可；正文写"为什么"，而不是复述 diff
- **破坏性变更**：type 后加 `!`（如 `feat(shared)!: ...`），并在正文写 `BREAKING CHANGE:`

示例：

```
feat(core): 课表解析支持单双周与连堂
fix(server): 套餐通道超时后未释放并发名额
docs(adr): 补充 ADR-11 关于多端同步派生数据的边界
feat(shared)!: ChatRequest 增加 tool_call_id 字段

BREAKING CHANGE: 端与服务端需同时升级，旧字段不再兼容
```

## 3. 本地检查（提 PR 前请全部跑一遍）

```bash
# Rust workspace：格式 / 静态检查 / 单测
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Flutter 端（在 app/ 下执行）
cd app
flutter pub get
flutter analyze
flutter test

# 文档：markdown 表格与相对链接校验
python scripts/check_docs.py docs

# 可选：评测结果与基线对比（回归超 10% 会被 bench workflow 阻断）
python bench/run.py --cases bench/cases/smoke.jsonl \
  --baseline bench/baseline.json --out bench/results/local.json
```

CI（`.github/workflows/ci.yml`）跑的就是上面这些，本地能过 CI 基本就能过。

## 4. PR 要求

- PR 描述请用仓库模板：变更说明 / 关联 ADR 或 issue / 验证方式 / 是否影响 `shared` / 自检清单。
- 必须 CI 全绿（rust、flutter、docs、migrations 四个 job），并至少一位 CODEOWNERS 审查通过。
- 一个 PR 只做一件事；顺手重构请单独提 PR，避免审查失焦。
- 改动数据库需新增迁移文件 `crates/server/migrations/NNNN_描述.sql`，**只增不改**已合入的迁移。
- 不允许提交：密钥、keystore、`key.properties`、`.env`、真实校务数据、会话正文。
- 合并方式：压缩合并（squash），标题即 commit message，遵守第 2 节约定。

## 5. ADR 先行原则

**先定边界，再写代码。** 以下情形必须先提 ADR（或修订现有 ADR），在 `docs/04-ADR.md` 定稿后再动实现：

- 凭据、Cookie、Session、BYOK Key 的存储位置或流转路径发生变化；
- 云端是否会接触到正文/消息内容（ADR-02 的边界）；
- `crates/shared` 的契约变更（ADR-13：三协议共用单一契约）；
- 存储选型、并发与容量口径、许可证与数据权利。

ADR 变更与代码变更**同 PR 或先后紧邻的 PR**；只改代码不改 ADR 会被要求返工。

## 6. 代码风格

- 由 `.editorconfig` 统一：UTF-8、LF、缩进 4 空格，`*.{yml,yaml,json,md}` 用 2 空格。
- Rust：`rustfmt` 默认风格；公共 API 写中文文档注释，说明边界与失败语义。
- Dart：`dart format` 默认风格；UI 与 `core` 之间只经 FFI 契约通信。
- 注释写"为什么"，不写"做了什么"；不要留注释掉的死代码。

---

## 多代理协作（AI Agent 并行开发）

多人 / 多 Agent 同时改一个仓库时，**一个 Agent 一个 Git worktree**，否则会出现写入竞态（文件被互相覆盖，且很难发现）。

```powershell
pwsh scripts/new-agent-worktree.ps1 -Name rust-parser    # 创建 .worktrees/rust-parser，分支 agent/rust-parser
git diff main..agent/rust-parser                          # 审阅
git merge --no-ff agent/rust-parser                       # 合入
git worktree remove .worktrees/rust-parser                # 收尾
```

规则与"单写者文件"清单见 [docs/integrations/agent-worktrees.md](docs/integrations/agent-worktrees.md)。

---

## Python / 实验分支（EXP）

> 写给**只写 Python** 的贡献者（包括 v0 Demo 的原作者）：
> **你不需要会 Rust，也不需要会 Flutter。**

| 你想做的事 | 该去哪里 |
|---|---|
| 修 v0 Demo 的 bug、补文档、补测试 | `experiments/v0-legacy/`（历史留档，不参与构建） |
| 做新实验（数据抓取、OCR、报告生成…） | `experiments/<你的特性>/` |
| 改生产代码 | `crates/`（Rust）、`app/`（Flutter）—— 需要对应背景，走正常 PR 评审 |

### 分支模型

``````
main ──┬────────────────────────────► 生产
       └── EXP ─────────────────────► 实验（定期 git merge main 保持同步）
``````

- `EXP` **不是孤立历史**，是从 `main` 分出来的长期分支，所以合并不会打架；
- `experiments/` 里的代码**不参与 CI 构建**，不会把主线弄红；
- 成熟实验由维护者评审后提升进主线。

### 三条硬性要求

1. 不放凭据（Cookie / API Key / 学号 / 个人绝对路径）；
2. 自带 `README.md`（怎么跑、能做什么、不能做什么）；
3. 有依赖就写 `requirements.txt`。

### 常用命令

``````bash
git switch EXP
git merge main                     # 同步主线
# 在 experiments/ 下写代码
git add -A && git commit -m "exp: 说明"
git push origin EXP
``````

详见 [experiments/README.md](experiments/README.md)。