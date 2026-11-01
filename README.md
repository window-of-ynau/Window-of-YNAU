# 云农之窗（Yunong）

> 把一个学校的事，装进一个 App：左边办事，右边问事。
> 凭据不出端 · 隐私在本地 · 知识在云端。

---

## 这是什么

面向云南农业大学学生的**校务聚合客户端 + AI 助理**：

- **左导航**：教务、成绩、课表、图书馆……App 内 WebView，登录一次后免二次登录（Cookie 只留在本机）。
- **右对话**：自然语言问课表、加日程、查规章、读文档；支持 BYOK（自带 Key）与套餐两种通道。
- **日历**：课表按周次展开，AI 可增删日程，可导出 ICS。

**它不是**：教务爬虫、自动打卡工具、聊天机器人皮肤。

---

## 现在能跑什么（诚实版）

已经**真的跑通**的（不是设计稿）：

| 能力 | 状态 | 证据 |
|---|---|---|
| 服务端容器化部署 | ✅ 可用 | `docker compose up` 起 PG16 + 服务端，`/healthz` `/readyz` 200 |
| 账号鉴权 | ✅ 可用 | Argon2id + 不透明令牌（只存 SHA-256）、注册/登录/刷新/登出，7 条真库用例 |
| 三协议流式网关 | ✅ 可用 | BYOK 直连 + 套餐代理，**真实 DeepSeek 端到端验证通过**（见下） |
| 并发闸门与限流 | ✅ 可用 | 按 turn 计名额，全局 50 + 单用户 1 + 同 turn 去重，429 带 `Retry-After` |
| 用量抽样记账 | ✅ 可用 | 每轮一行、`(user_id, turn_id, round)` 幂等；缺 usage 时估算并标 `estimated` |
| 课表解析与本地落库 | ✅ 可用 | Rust 解析 → SQLite（`terms`/`courses`）→ Flutter 列表；改版页面显式失败、不落库 |
| **WebView 登录与 Cookie 复用** | 🚧 **未完成（W6）** | 目前只有取数骨架 + 调试导入；真实校务登录尚未接通 |
| 工具调用闭环 / 日历 / 记忆 / RAG | ⬜ 未开始 | 见下方路线 |

真实上游实测（容器内，2026-11）：

```
POST /api/v1/chat/stream  →  event: meta  →  event: delta「成功」
                          →  event: usage(9/1, estimated=false)  →  event: done{stop}
```

---

## 文档（先读这几份）

| 文档 | 作用 |
|---|---|
| [04-ADR](docs/04-ADR.md) | 17 条架构决策：边界划在哪、越界怎么防、塌了怎么退化 |
| [01-产品需求](docs/01-产品需求文档.md) | 功能清单、验收标准、版本路线、WBS |
| [02-系统设计](docs/02-系统设计文档.md) | 组件、数据模型、关键流程、容量与 CI |
| [03-接口](docs/03-接口文档.md) | 云端 HTTP / 端侧 FFI / WebView 桥 / 三协议 |

行业约定：**改架构先改 ADR，再改代码。**

---

## 仓库结构

```
crates/
  shared/   契约层：协议类型、错误码、流式装配器（端、云、文档的唯一来源）
  core/     端侧核心：三协议适配、SSE 解析、SQLite 课表、FFI 出口
  server/   服务端：鉴权、流式代理、并发闸门、抽样记账、错误信封
app/        Flutter 客户端（Android / Windows）
bench/      评测：用例集、跑分脚本、基线
deploy/     容器、compose、幂等迁移执行器
docs/       ADR、需求、设计、接口、集成方案
```

---

## 快速开始

### 服务端（Docker，推荐）

```bash
cp deploy/.env.example deploy/.env     # 填 POSTGRES_PASSWORD / DATABASE_URL / UPSTREAM_*
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d --build
curl http://127.0.0.1:8080/healthz
```

`deploy/docker-compose.yml` 会先跑 `migrate`（幂等，`schema_migrations` 记账），再起服务端。

> 升级时**先跑迁移、再起新二进制**：启动自检发现缺列会拒绝启动并提示该跑哪个迁移。

### 服务端（本机直跑）

```bash
cargo run -p yunong-server        # 无 DATABASE_URL → 无数据库模式，只有探针与骨架接口
```

有 PostgreSQL 时按顺序执行 `crates/server/migrations/*.sql`（0001 → 0002 → 0003）。

### 客户端（Windows）

前置条件（缺了会以很隐晦的方式失败，见 [CONTRIBUTING §3.1](CONTRIBUTING.md)）：

- Windows **开发者模式**（Flutter 建插件符号链接需要）；
- **`nuget.exe` 在 PATH**（`flutter_inappwebview` 的 Windows 实现要用）；
- VS2022「使用 C++ 的桌面开发」+ Rust MSVC 工具链。

```powershell
pwsh scripts/build-windows-app.ps1        # release 一键打包（含 yunong_core.dll）
```

**不要只跑 `flutter build windows`**：它不会构建也不会复制 `yunong_core.dll`，漏了界面只会显示「端侧核心未就绪」。

### 质量门禁（与 CI 一致）

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd app && flutter analyze && flutter test && cd ..
python scripts/check_docs.py docs
```

真实上游冒烟（需要自己的 Key，`#[ignore]`，永不进 CI）：

```powershell
$env:YUNONG_REAL_API_KEY="sk-..."
cargo test -p yunong-core --test real_upstream -- --ignored --nocapture
```

---

### 合并 agent 分支

`pwsh scripts/merge-agent.ps1 -Name <name>`：先审差异、检查单写者文件、在 agent 工作树内跑 fmt/clippy/test，全绿才合并。加 `-DryRun` 只看不合并。

### 多代理协作

并行开发时每个 Agent 用独立 worktree（`scripts/new-agent-worktree.ps1`），规则见 [docs/integrations/agent-worktrees.md](docs/integrations/agent-worktrees.md)。

---

## 当前进度

编号与 [需求文档 §8 WBS](docs/01-产品需求文档.md) 一致。

| ID | 里程碑 | 状态 |
|---|---|---|
| W1 | 仓库骨架 + CI + 契约层 + 端侧核心逻辑 | ✅ 完成 |
| W2 | 服务端骨架：axum + PG + 迁移 + 错误信封 | ✅ 完成 |
| W3 | 鉴权与用户：注册/登录/Token | ✅ 完成 |
| W4 | ModelGateway：三协议 + 流式 + BYOK + 云端代理 | ✅ 完成（真实上游已验证） |
| W5 | 配额与记账：并发闸门、单轮上限、usage 记账 | ✅ 完成（16 并发实测通过；50 上限待上线观察） |
| W6 | Flutter 壳：导航 + WebView + Cookie 安全存储 + 取数注入 | 🚧 **进行中**：导航、课表页、解析→SQLite→列表、调试导入已完成；**真实登录与 Cookie 复用未接通** |
| W7 | Rust 本地核心：SQLite Memory、Keystore、文件解析、ICS | 🚧 课表落库已完成，其余未开始 |
| W8 | 上下文子系统：55% 触发压缩、头尾剪枝 | ⬜ |
| W9 | 课表周次展开 + 工具调用查询 + 日程 CRUD | ⬜ |
| W10 | 权限：能力白名单 + 确认弹窗 + Auto Review | ⬜ |
| W11 | 可观测 + Benchmark：指标、评测集、回归门禁 | ⬜ |
| W12 | 打包发布：Android 签名包、Windows 安装包 | ⬜ |

> 上一批「W2b」不是 WBS 里的编号，而是把 W3/W4/W5（+W6 的一部分）一次性推进的批次代号：
> 契约补齐 C1/C6/C9、双通道网关、SSE 代理与闸门、PG 集成测试、端侧 SQLite 课表、文档同步、交叉评审两轮。

**已知缺口**（不掩饰）：

- 真实校务登录、Cookie 复用未做（W6）；
- 工具调用闭环、日历、记忆、RAG 未做（W8/W9）；
- CI **没有 docker build job**，镜像问题目前靠人工发现；
- 并发上限只实测到 16 路，50 是保守设定，尚未压到边界。

---

## 许可

**双许可**：[AGPL-3.0](LICENSE)（社区）+ **商业许可**（闭源商用）。

- 个人/校内使用：直接遵守 AGPL-3.0；
- 闭源商用：需单独授权，见 [LICENSING.md](LICENSING.md)。

提交 PR 即表示同意以相同双许可方式授权你的贡献（CLA 流程待建立）。

---

## 安全

**不要提交 `.env`、Cookie、上游 API Key。** 发现漏洞请走 GitHub Security Advisory，见 [SECURITY.md](SECURITY.md)。
