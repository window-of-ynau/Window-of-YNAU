# 云农之窗（Yunong）

[![CI](https://github.com/window-of-ynau/Window-of-YNAU/actions/workflows/ci.yml/badge.svg)](https://github.com/window-of-ynau/Window-of-YNAU/actions/workflows/ci.yml)

面向云南农业大学学生的校务聚合客户端与 AI 助理。

左侧为校务入口（教务、成绩、课表、图书馆等，以 WebView 承载），右侧为对话区，可用自然语言查询课表、添加日程、检索规章与文档。支持 BYOK（用户自带上游 Key）与套餐代理两种接入方式。

**设计原则**：凭据不出端，隐私在本地，知识在云端。

本项目不是教务爬虫、自动打卡工具或聊天机器人前端。

---

## 目录

- [功能](#功能)
- [技术栈](#技术栈)
- [仓库结构](#仓库结构)
- [架构文档](#架构文档)
- [快速开始](#快速开始)
- [构建客户端](#构建客户端)
- [测试与质量门禁](#测试与质量门禁)
- [项目状态](#项目状态)
- [已知限制](#已知限制)
- [贡献](#贡献)
- [许可](#许可)
- [安全](#安全)

---

## 功能

| 模块 | 说明 | 状态 |
|---|---|---|
| 账号鉴权 | Argon2id 口令哈希、不透明访问令牌（服务端仅存 SHA-256 摘要）、注册 / 登录 / 刷新 / 登出 | 已实现 |
| 模型网关 | 三种上游协议（OpenAI Completions / OpenAI Responses / Anthropic Messages）适配、流式传输、BYOK 直连与套餐代理双通道 | 已实现 |
| 流式代理 | 服务端 SSE 转发、心跳、客户端断开时停止消费上游、错误信封统一 | 已实现 |
| 并发控制 | 按 turn 计数的并发闸门（全局上限、单用户单 turn、同 turn 重入拒绝），超限返回 429 与 `Retry-After` | 已实现 |
| 用量记账 | 按轮抽样写入 `usage_samples`，`(user_id, turn_id, round)` 唯一且幂等；上游缺失用量时估算并标记 | 已实现 |
| 课表 | 教务课表 HTML 解析、SQLite 落库、按学期查询与展示 | 已实现 |
| 校务登录与会话复用 | WebView 登录、Cookie 持久化、登录态检测与取数注入 | 开发中 |
| 日历与日程 | 课表按周次展开、日程增删改、ICS 导出 | 未开始 |
| 本地记忆与上下文 | SQLite 记忆、上下文压缩、检索 | 未开始 |
| 工具调用闭环 | 多轮工具调用编排、权限白名单与确认 | 未开始 |

---

## 技术栈

- **服务端**：Rust、axum、Tokio、PostgreSQL（pgvector）、reqwest（rustls）
- **端侧核心**：Rust（cdylib），通过 C ABI 与 Flutter 通信
- **客户端**：Flutter（Windows / Android），WebView 使用 `flutter_inappwebview`
- **部署**：Docker、Docker Compose
- **CI**：GitHub Actions（Rust、Flutter、文档、迁移）

---

## 仓库结构

```
crates/
  shared/   契约层：协议类型、错误码、流式装配器（端、云与文档的唯一来源）
  core/     端侧核心：协议适配、SSE 解析、SQLite 课表、C ABI 出口
  server/   服务端：鉴权、流式代理、并发闸门、用量记账、错误信封
app/        Flutter 客户端
bench/      评测用例、跑分脚本与基线
deploy/     容器镜像、compose 编排、迁移执行器
docs/       架构决策、需求、设计、接口、集成方案
scripts/    开发与合并脚本
```

---

## 架构文档

| 文档 | 内容 |
|---|---|
| [架构决策（ADR）](docs/04-ADR.md) | 17 条架构决策，含边界、失效模式与替代方案 |
| [产品需求](docs/01-产品需求文档.md) | 功能清单、验收标准、版本路线与 WBS |
| [系统设计](docs/02-系统设计文档.md) | 组件划分、数据模型、关键流程、容量规划与 CI |
| [接口文档](docs/03-接口文档.md) | 云端 HTTP 接口、端侧 FFI、WebView 桥、上游协议 |

约定：架构变更先更新 ADR，再修改代码。

---

## 快速开始

### 服务端（Docker）

```bash
cp deploy/.env.example deploy/.env      # 配置 POSTGRES_PASSWORD、DATABASE_URL、UPSTREAM_*
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d --build
curl http://127.0.0.1:8080/healthz
```

编排包含三个服务：`db`（PostgreSQL 16 + pgvector）、`migrate`（幂等迁移执行器）、`server`。`server` 在迁移完成后启动。

服务端启动时校验数据库结构，缺少必要列或表时拒绝启动并输出对应迁移文件路径。升级流程为：先执行迁移，再启动新版本。

### 服务端（本地）

```bash
cargo run -p yunong-server        # 未配置 DATABASE_URL 时以降级模式启动，仅提供探针接口
```

启用数据库时，按文件名顺序执行 `crates/server/migrations/` 下的迁移脚本：

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/yunong
for f in crates/server/migrations/*.sql; do psql "$DATABASE_URL" -f "$f"; done
```

### 环境变量

完整列表见 [deploy/.env.example](deploy/.env.example) 与 [系统设计 §11](docs/02-系统设计文档.md)。常用项：

| 变量 | 默认值 | 说明 |
|---|---|---|
| `DATABASE_URL` | 空 | 未设置时以降级模式运行 |
| `YUNONG_BIND` | `127.0.0.1:8080` | 监听地址；容器内由 compose 固定为 `0.0.0.0:8080` |
| `UPSTREAM_BASE_URL` / `UPSTREAM_API_KEY` | 空 | 套餐通道使用的上游地址与密钥，可按协议分别覆盖 |
| `CONCURRENCY_LIMIT` | `50` | 全局在途 turn 上限 |
| `MAX_INPUT_TOKENS` | `32000` | 单轮输入上限 |
| `USAGE_SAMPLE_PERCENT` | `1` | 用量抽样比例 |

---

## 构建客户端

### 前置条件

| 条件 | 说明 |
|---|---|
| Windows 开发者模式 | Flutter 构建插件时需要创建符号链接 |
| `nuget.exe` 在 PATH | `flutter_inappwebview` 的 Windows 实现依赖它下载 `nlohmann.json` |
| Visual Studio 2022「使用 C++ 的桌面开发」 | Windows 原生构建 |
| Rust MSVC 工具链 | 端侧核心（cdylib） |

### 打包

```powershell
pwsh scripts/build-windows-app.ps1            # release
pwsh scripts/build-windows-app.ps1 -Debug     # debug
```

脚本依次执行：构建端侧核心（`yunong_core.dll`）、构建 Flutter 应用、将 DLL 复制到可执行文件同目录。

`flutter build windows` 本身不处理端侧核心 DLL；缺少该文件时应用可启动，但端侧功能不可用。详见 [CONTRIBUTING §3.1](CONTRIBUTING.md)。

---

## 测试与质量门禁

CI 与本地使用同一套检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd app && flutter analyze && flutter test && cd ..

python scripts/check_docs.py docs
```

数据库相关测试默认忽略，需连接真实 PostgreSQL 执行：

```bash
cargo test --workspace -- --include-ignored
```

上游连通性测试需要真实密钥，默认忽略：

```powershell
$env:YUNONG_REAL_API_KEY="sk-..."
cargo test -p yunong-core --test real_upstream -- --ignored --nocapture
```

---

## 项目状态

里程碑编号与 [产品需求 §8](docs/01-产品需求文档.md) 的 WBS 一致。

| ID | 里程碑 | 状态 |
|---|---|---|
| W1 | 仓库骨架、CI、契约层、端侧核心逻辑 | 完成 |
| W2 | 服务端骨架：axum、PostgreSQL、迁移、错误信封 | 完成 |
| W3 | 鉴权与用户 | 完成 |
| W4 | 模型网关：三协议适配、流式、BYOK 与代理 | 完成 |
| W5 | 配额与记账：并发闸门、单轮上限、用量记账 | 完成 |
| W6 | Flutter 壳：导航、WebView、会话存储、取数注入 | 开发中 |
| W7 | 本地核心：SQLite 记忆、凭据存储、文件解析、ICS | 部分完成 |
| W8 | 上下文子系统 | 未开始 |
| W9 | 课表周次展开、工具调用查询、日程管理 | 未开始 |
| W10 | 权限：能力白名单、确认流程、Auto Review | 未开始 |
| W11 | 可观测性与 Benchmark | 未开始 |
| W12 | 打包发布 | 未开始 |

---

## 已知限制

- 校务 WebView 登录与会话复用尚未完成（W6），当前课表功能通过导入课表 HTML 验证解析与落库链路；
- 工具调用闭环、日历、本地记忆与 RAG 未实现（W8 / W9）；
- CI 未包含容器镜像构建任务，镜像问题目前依赖人工验证；
- 并发上限经过 16 路并发实测，`CONCURRENCY_LIMIT=50` 为保守设定，尚未进行边界压测；
- 上游连通性仅在 DeepSeek 上完成端到端验证，Anthropic 与 OpenAI Responses 协议目前以契约测试覆盖。

---

## 贡献

开发流程、提交规范、单写者文件与多代理协作约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。

分支策略：`main` 为受保护分支，需通过全部必需检查与至少一次评审。

---

## 许可

双许可：

- 个人与校内使用：[AGPL-3.0](LICENSE)；
- 闭源商用：需单独授权，见 [LICENSING.md](LICENSING.md)。

提交 PR 即表示同意以相同双许可方式授权贡献。CLA 流程待建立。

---

## 安全

请勿提交 `.env`、Cookie 或上游 API Key。漏洞报告请通过 GitHub Security Advisory，详见 [SECURITY.md](SECURITY.md)。
