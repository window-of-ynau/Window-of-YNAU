# 云农之窗（Yunong）

> 把一个学校的事，装进一个 App：左边办事，右边问事。
> 凭据不出端 · 隐私在本地 · 知识在云端。

---

## 这是什么

面向云南农业大学学生的**校务聚合客户端 + AI 助理**：

- **左导航**：教务、成绩、课表、图书馆……点进去是 App 内 WebView，登录一次后免二次登录（Cookie 只留在本机）。
- **右对话**：自然语言问课表、加日程、查规章、读文档；支持 BYOK（自带 Key）与套餐两种通道。
- **日历**：课表按周次展开，AI 可增删日程，可导出 ICS。

**它不是**：教务爬虫、自动打卡工具、聊天机器人皮肤。

---

## 文档（先读这三份）

| 文档 | 作用 |
|---|---|
| [04-ADR](docs/04-ADR.md) | 14 条架构决策：边界划在哪、越界怎么防、塌了怎么退化 |
| [01-产品需求](docs/01-产品需求文档.md) | 功能清单、验收标准、版本路线、WBS |
| [02-系统设计](docs/02-系统设计文档.md) | 组件、数据模型、关键流程、容量与 CI |
| [03-接口](docs/03-接口文档.md) | 云端 HTTP / 端侧 FFI / WebView 桥 / 三协议 |

行业约定：**改架构先改 ADR，再改代码。**

---

## 仓库结构

```
crates/
  shared/   契约层：协议类型与错误码（端、云、文档的唯一来源）
  core/     端侧核心：记忆、上下文压缩、检索、日历、BYOK 直连
  server/   服务端：探针、校验、错误信封、代理（W2 补齐）
app/        Flutter 客户端（Android / Windows / Web 基础版）
bench/      评测：用例集、跑分脚本、基线（重构效果的裁判）
deploy/     容器与 compose
docs/       ADR、需求、设计、接口、集成方案
```

---

## 本地开发

### 后端

```bash
cargo test --workspace          # 全部单测（不需要数据库）
cargo run -p yunong-server      # 无数据库也能起：/healthz /readyz 可用
```

有 PostgreSQL 时：

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/yunong
psql "$DATABASE_URL" -f crates/server/migrations/0001_init.sql
```

### 客户端

```bash
cd app
flutter pub get
flutter run -d windows    # 或 -d <android-device>
flutter test
```

### 质量门禁（与 CI 一致）

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd app && flutter analyze && flutter test && cd ..
python scripts/check_docs.py docs
```

---

### 合并 agent 分支

`pwsh scripts/merge-agent.ps1 -Name <name>`：先审差异、检查单写者文件、在 agent 工作树内跑 fmt/clippy/test，全绿才合并。加 -DryRun 只看不合并。

### 多代理协作

并行开发时每个 Agent 用独立 worktree（`scripts/new-agent-worktree.ps1`），规则见 [docs/integrations/agent-worktrees.md](docs/integrations/agent-worktrees.md)。

## 当前进度

| 里程碑 | 状态 |
|---|---|
| W1 仓库骨架 + CI + 契约层 + 端侧核心逻辑 | ✅ 本次 |
| W2 鉴权、三协议适配、课表解析 | 🚧 进行中（3 个 agent worktree 并行） |
| W2b 并发闸门、流式代理、抽样记账 | ⬜ |
| W3 Flutter WebView 登录与取数、本地 SQLite | ⬜ |
| W4 课表解析与日历、工具调用闭环 | ⬜ |
| W5 知识库与 RAG、Benchmark 基线 | ⬜ |

详细任务与验收见 [需求文档 §8](docs/01-产品需求文档.md)。

---

## 许可

**双许可**：[AGPL-3.0](LICENSE)（社区）+ **商业许可**（闭源商用）。

- 个人/校内使用：直接遵守 AGPL-3.0；
- 闭源商用：需单独授权，见 [LICENSING.md](LICENSING.md)。

提交 PR 即表示同意以相同双许可方式授权你的贡献（CLA 流程待建立）。

---

## 安全

**不要提交 `.env`、Cookie、上游 API Key。** 发现漏洞请走 GitHub Security Advisory，见 [SECURITY.md](SECURITY.md)。
