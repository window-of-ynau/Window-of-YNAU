# ADR-20 落地清单 · Go 工程规范 · RAG 抓取 worker 试点契约

> 状态：承接 [ADR-20](../04-ADR.md) 定稿（2026-11，lead-server 评审）。
> 范围：本文件只回答「**怎么落**」——模块去向、Go 工程规范、第一个试点的接口契约。
> 红线：**不重写已有 Rust**；不改动 `crates/core` 的 C ABI、`crates/shared` 契约、`crates/server` 现有 `/api/v1/*` 行为。

---

## 0. 一句话

Go 只做**新增**的控制面与外部 IO；热路径与所有既有实现保持 Rust；PG schema / 错误信封 / OpenAPI 是单一事实源；每个 Go 服务都必须带**可回退开关**。

---

## 1. 评审结论（边界与可行性）

| 维度 | 判断 | 依据 |
|---|---|---|
| 可行性 | 可执行 | Go 承接的是「低频、非 SSE、性能不敏感」的接入/外部 IO；不与 Rust 热路径竞争 |
| 边界 | 需澄清后成立 | ADR-20 边界表里的「认证、邀请码、配额」应读作**新增的管理面端点**，不是把现有 Rust 实现搬走 |
| 隔离方式 | 合理 | 独立进程 + HTTP/JSON（或 PG 表）+ 禁止 cgo：崩溃域、交叉编译、部署、回退全部解耦 |
| 数据所有权 | 必须写死 | PG schema 由 `crates/server/migrations` **唯一拥有**；Go 只读写，不建表、不跑迁移 |
| 契约 | 可落地 | 错误信封与 `ErrorCode` 复用 `crates/shared/src/error.rs`；`/api/v1/*` 以 OpenAPI 为单一事实源 |
| 主要风险 | 已知并接受 | 双语言 CI/依赖/安全扫描成本；Go GC 的 P99 尖刺（不用于数据面即可）；团队双栈负担 |
| 结论 | 维持 ADR-20 | 边界按 §2 澄清后执行；首个试点选 RAG 外部抓取 worker（纯外部 IO，可回退） |

**明确不迁（正确性 / 长尾敏感）**：SSE 流式代理、并发闸门、token 记账、RAG 检索与哈希 join / 两段式落库、不透明令牌鉴权。详见 §2。

---

## 2. 模块清单：现有路由 / 模块 → 去向

> 判定口径：**热路径或正确性敏感 → 留 Rust**；**新增且为管理面/外部 IO → Go**；**试点 → 独立进程 + 可回退**。

| 现有模块 / 路由（文件） | 现语言 | 将来去向 | 理由 / 约束 |
|---|---|---|---|
| `/healthz` `/readyz`（api/health） | Rust | 保留 Rust；多服务后可由 Go 做聚合只读镜像 | 被 compose / 探针直接调用，行为不能变 |
| `/api/v1/auth/register` `login` `refresh` `logout`（api/auth、auth/*） | Rust | **明确不迁** | Argon2id、令牌轮换、只存摘要；正确性与安全敏感 |
| `/api/v1/me`（api/me） | Rust | 保留 Rust；可新增 Go 只读镜像做灰度对照 | 无状态读，迁与不迁收益都小，先不折腾 |
| `/api/v1/chat/stream`（api/chat、upstream、gate、accounting） | Rust | **明确不迁** | SSE 首字 P99、turn 去重、并发闸门、记账在同一请求内闭环 |
| `/api/v1/rag/salt`（api/rag、rag） | Rust | 保留 Rust（Go 只读同一 `rag_salts`） | 盐同月不可变，跨月哈希一致性的前提 |
| `/api/v1/rag/referrals`（api/rag、rag） | Rust | 保留 Rust | 配额、限流、审计要在一个事务语义里 |
| 邀请码管理（现为 `bin/invite_admin`，无 HTTP） | Rust CLI | **新增** Go 管理面 HTTP（`/internal/admin/invites*`） | 典型低频管理面；先做只读+核销，逐步替代 CLI |
| 用户 / 配额 / 用量查询、运维接口（未实现） | — | **新增** Go 管理面 | 只读查询为主，性能不敏感 |
| 健康检查聚合（多服务后） | — | **新增** Go 边缘 | 聚合探针，无状态 |
| 网关前置：新增 per-IP 路由 / 灰度（未实现） | — | **新增** Go 可选；既有 `gate.rs` **不迁** | 现闸门是正确性核心；Go 只做新增的前置路由 |
| RAG 外部抓取 worker（`rag_fetch::HttpFetcher`） | Rust | **试点改 Go 独立进程**（可回退） | 纯外部 IO、改动频繁；见 §4 |
| RAG 检索 / 哈希 join / 两段式落库 / 索引 CLI（rag、rag_fetch） | Rust | **明确不迁** | 跨语言哈希契约与 `store_fetch_result` 事务不变量是金标准 |
| webhook / 通知（未实现） | — | **新增** Go | 外部 IO，失败重试独立 |
| bench / 语料清洗 / 规则试验 | tools（Python 可选） | 保持开发期工具 | 不进安装包、不进构建链 |

小结：**明确不迁 6 类**、**新增 Go 5 类**、**试点 1 个**、**开发期工具 1 类**。

---

## 3. Go 服务工程规范

### 3.1 目录（monorepo，与 Rust 平级）

```text
services/
  control-plane-go/            # 管理面（邀请码/查询/运维），按需新增
    cmd/control-plane/main.go
    internal/httpapi/          # handler、错误信封、中间件（request_id、body 限制）
    internal/config/           # env only，启动即校验
    internal/pg/               # 只读写，禁止 DDL / 迁移
    internal/contract/         # ErrorCode 常量、OpenAPI 生成物
    internal/observability/    # slog JSON、指标
    api/openapi.yaml
  rag-fetch-worker/            # 试点（§4）
    cmd/rag-fetch-worker/main.go
    internal/fetch/            # HTTP、重定向白名单、大小/类型上限、重试退避
    internal/canonical/        # canonical + sha256（对齐 docs/rag_vectors.json）
    internal/store/            # PG 两段式事务
    internal/config/
```

约定：`go.mod` 每服务独立；**禁止 `import "C"`、禁止 `CGO_ENABLED=1` 进产物**；`internal/` 不对外暴露。

### 3.2 错误码与响应信封（与 Rust 逐字段一致）

- 成功：`{"ok":true,"data":{...},"request_id":"req_..."}`
- 失败：`{"ok":false,"error":{"code":"UPPER_SNAKE","message":"...","request_id":"req_...","details":{...}}}`
- `code` 取值范围 = `crates/shared/src/error.rs` 的 `ErrorCode`（UNAUTHORIZED / TOKEN_EXPIRED / FORBIDDEN / INVITE_INVALID / NOT_FOUND / VALIDATION_ERROR / MESSAGE_TOO_LARGE / CONCURRENCY_LIMIT / RATE_LIMITED / QUOTA_EXCEEDED / REVIEW_REJECTED / UPSTREAM_TIMEOUT / UPSTREAM_ERROR / NOT_IMPLEMENTED / INTERNAL）。
- HTTP 状态映射与 Rust `ErrorCode::http_status()` **逐条一致**；`request_id` 取请求头 `x-request-id`（无则生成 `req_` + ULID），并回写同名响应头。
- 不把上游/底层原始错误原文透传给客户端；内部日志保留可定位信息，但按 ADR-19 只打哈希前 8 位。

### 3.3 配置

- 只走环境变量（12-factor），启动时校验，非法即**快速失败**。
- 与 Rust 共用的键**沿用同名**（如 `RAG_FETCH_*`、`DB_MAX_CONNECTIONS`），避免两套配置语义分叉。
- 密钥/令牌不进日志、不进错误信封。

### 3.4 数据库

- 驱动用 `pgx/v5`（`database/sql` 兼容层即可），`CGO_ENABLED=0`；连接池上限来自 `DB_MAX_CONNECTIONS`。
- **禁止**在 Go 服务里执行 DDL / 迁移；迁移唯一入口是 Rust 的 `crates/server/migrations`。
- 所有 SQL 参数化；设置语句超时与连接超时；读写 RAG 表时遵守「无 `user_id`、日志只打哈希前 8 位」的隐私红线。

### 3.5 契约测试

- OpenAPI schema 校验（Go 侧用 `kin-openapi` 或等价物）——响应必须同时满足 schema 与错误信封 golden。
- **跨语言向量守卫**：RAG worker 的 `canonical`/`hash` 必须与 `docs/rag_vectors.json`（当前 version=2）逐字节一致，并与 Rust/Dart 同一批 cases/rejects 对齐；任一语言改规则先改向量文件，否则红。
- PG 集成测试用**真实 PostgreSQL + 真实迁移**（CI 用 `pgvector/pgvector:pg16`），Go 只在其上读写。

### 3.6 静态门禁（每服务独立 CI job）

- `gofmt -l`（必须为空）、`go vet ./...`、`golangci-lint run`（至少：errcheck、govet、staticcheck、ineffassign、unused、gosec、bodyclose、sqlclosecheck、noctx、contextcheck、errorlint）。
- `govulncheck ./...`（阻断式）。
- `go test -race ./...`；`CGO_ENABLED=0 go build ./...`。
- CI job 名暂定 `ci / go`；稳定后再加入分支保护必需检查（改名=静默失效，改前先同步保护）。

### 3.7 运行与可观测

- `slog` 结构化 JSON 日志；请求日志含 `request_id`、路由、状态码、耗时。
- 最小指标：请求计数、错误码计数、P50/P99 耗时、DB 池占用、抓取成功/失败数。
- `/healthz`（存活）与 `/readyz`（就绪：DB 可写、依赖可达）；`SIGTERM` 优雅退出（在途请求用 context 超时收敛）。
- 入口限制：请求体上限、每 IP/每路由限流、并发上限（作为**新增**能力，不替换 Rust 闸门）。

### 3.8 可回退开关（硬要求）

- 每服务一个 `*_IMPL=rust|go` 环境变量，**默认 `rust`**；反代按开关切流，或直接不部署 Go 进程即回退。
- 回退必须**不需要改 schema、不需要数据修复**：Go 只使用与 Rust 一致的现有列；新增列必须可空且默认值安全。
- 发布单里写清「一条命令回退」的具体命令与验证点。

---

## 4. RAG 抓取 worker 试点接口契约草案

### 4.1 目标与范围

- 用 Go 独立进程替换 `crates/server/src/rag_fetch.rs` 中 **`HttpFetcher` 的外部抓取**：URL 校验、HTTP 拉取、DOI 重定向白名单、Content-Type / 大小上限、重试与退避、限速、抓取后清洗。
- **不改** Rust 的检索、哈希 join 与落库不变量：`canonical_identifier` / `hash_identifier` / `store_fetch_result` 仍是金标准。

### 4.2 交互方式：选「PG 表为契约」

| 方案 | 优点 | 代价 | 结论 |
|---|---|---|---|
| **PG 表契约**（Go 直接读写 `rag_*` 三表） | 不改 Rust 代码；复用 ADR-12「PG 当队列」；进程可独立部署/回退 | Go 必须完整实现 canonical/hash 与两段式事务 → 用跨语言向量 + PG 集成测试钉死 | **采用（试点）** |
| HTTP/JSON（Rust 暴露 claim/result 内部端点） | Go 不碰哈希/事务 | 要改 Rust 增加内部端点与鉴权；正文 base64 传输重 | 备选，试点后再评 |

### 4.3 数据契约（表不变量，Go 必须遵守）

| 表 | Go 权限 | 关键列 / 不变量 |
|---|---|---|
| `rag_salts` | 只读 | `(month, salt)`；同月不可变；worker 只读当月盐 |
| `rag_known_docs` | 读 + 更新正文 | `(month, identifier, source, fetched_at, doc_text)`；**无 hash 列**；更新按 `(month, identifier)` |
| `rag_candidates` | 读 + 更新状态 | `(month, hash, count, status, attempts, last_error, fetched_at, doc_id, claimed_at)`；`status ∈ pending/fetching/fetched/failed/skipped` |
| 隐私红线 | — | 三表**无 `user_id`**；日志只打哈希前 8 位；不落任何明文标识到日志 |

### 4.4 一轮抓取流程（与服务端状态机一致）

1. `month = UTC 当前月`；读取 `rag_salts` 当月盐（不存在则本轮跳过并告警，不自行造盐）。
2. **崩溃恢复**：`UPDATE rag_candidates SET status='pending', claimed_at=NULL WHERE month=$1 AND status='fetching' AND (claimed_at IS NULL OR claimed_at < now() - RAG_FETCH_STALE_AFTER_SECS)`。
3. **构建内存索引**：`SELECT identifier, source FROM rag_known_docs WHERE month=$1`，对每条算 `hash = sha256(salt ‖ "\n" ‖ canonical(identifier))`（Go 实现，向量守卫钉死）。
4. **抢占**：按 `count DESC, hash` 取 Top N；对每条 `UPDATE ... SET status='fetching', last_error=NULL, claimed_at=now() WHERE month=$1 AND hash=$2 AND status IN ('pending','failed')`；`rows_affected=1` 才继续。
5. **join 未命中**：置 `status='skipped', last_error='no_known_doc', claimed_at=NULL`。
6. **抓取**：带退避重试（`RAG_FETCH_RETRIES` / `BACKOFF_MS`）、限速（`MIN_INTERVAL_MS`）、DOI 白名单跟随重定向、`MAX_BODY_BYTES` 上限、Content-Type 白名单；清洗成纯文本（去标签/脚本、折叠空白、截断到 200k 字符）。
7. **两段式落库（同一事务）**：先 `UPDATE rag_known_docs SET fetched_at=now(), doc_text=$3 WHERE month=$1 AND identifier=$2`，`rows_affected` 必须为 1；再 `UPDATE rag_candidates SET status='fetched', attempts=$3, fetched_at=now(), doc_id=$4, claimed_at=NULL WHERE month=$1 AND hash=$2 AND status='fetching'`，`rows_affected` 必须为 1；任一为 0 立即回滚（`known_doc_missing` 或 `contended`）。
8. **失败**：`UPDATE rag_candidates SET status='failed', attempts=$3, last_error=<截断 200 字符>, claimed_at=NULL`。
9. 只打印计数摘要（processed / fetched / failed / skipped / contended），不打印哈希或标识。

### 4.5 配置（复用 Rust 同名 env，另加开关）

| 变量 | 默认 | 说明 |
|---|---|---|
| `RAG_FETCH_ENABLED` | false | 总开关；Go worker 也遵守 |
| `RAG_FETCH_TOP_N` | 100 | 每轮候选上限 |
| `RAG_FETCH_RETRIES` | 3 | 总尝试 = retries + 1 |
| `RAG_FETCH_BACKOFF_MS` | 200 | 指数退避基数 |
| `RAG_FETCH_TIMEOUT_SECS` | 10 | 单次 HTTP 超时 |
| `RAG_FETCH_MIN_INTERVAL_MS` | 1000 | 同轮抓取最小间隔（礼貌访问） |
| `RAG_FETCH_RETRY_FAILED` | false | 是否纳入上次 failed |
| `RAG_FETCH_STALE_AFTER_SECS` | 1800 | fetching 回收阈值 |
| `RAG_FETCH_MAX_BODY_BYTES` | 2097152 | 正文字节上限 |
| `RAG_FETCH_DOI_ALLOWED_HOSTS` | 见 Rust 默认 | DOI 重定向白名单 |
| `RAG_FETCH_WORKER_IMPL` | rust | **回退开关**：rust / go |
| `RAG_FETCH_WORKER_TICK_SECS` | 21600 | 调度间隔（6 小时） |
| `RAG_FETCH_WORKER_DRY_RUN` | false | 只记录不写库（灰度前置） |

### 4.6 必须通过的契约 / 集成测试

- **跨语言哈希**：`docs/rag_vectors.json` 全部 cases + rejects，Go 输出与 Rust 逐字节一致（新增 Go 守卫测试）。
- **PG 集成**（真实 PG + 真实迁移）：造盐 + 已知语料 + 候选，Go worker 对本地假 HTTP 服务跑：302 白名单内跟随 / 白名单外拒绝、404 永久失败、500 重试到耗尽、超限 body、错误 Content-Type、清洗后为空；断言 `status` / `attempts` / `doc_text` / `last_error` 与 Rust 语义一致。
- **幂等**：连跑两轮不重抓 `fetched`；`fetching` 超时恢复；两个 worker 并发只允许一个抢到（用 `status` 条件更新）。
- **回退**：`RAG_FETCH_WORKER_IMPL=rust` 时 Rust worker 行为完全不变；Go 不部署不影响主链路。

### 4.7 试点验收指标（决定是否扩大 Go 范围）

- 开发效率：同一需求改动的端到端时间（编码 + 编译 + 测试 + CI）Go vs Rust；目标**下降 ≥30%**。
- 运行：单条抓取延迟、失败率、常驻内存；不得引入新的运维事故。
- 若 2–3 个迭代未达标 → 按 ADR-20 退出条件**回退 Rust**。

### 4.8 回退动作（一条命令）

- 生产：设 `RAG_FETCH_WORKER_IMPL=rust` 并停止 Go worker 进程；Rust 内建 worker（`main.rs` 的 6 小时 ticker）恢复接管。
- 数据：无需迁移/修复——Go 只使用现有列；如新增列必须可空。
- 灰度：先 `RAG_FETCH_WORKER_DRY_RUN=1` 跑一轮，对比 Go 与 Rust 的候选/失败计数，再切真。

---

## 5. 落地检查单

- [x] ADR-20 状态定稿，边界澄清写入 ADR（2026-11，lead-server）
- [ ] 新建 `services/rag-fetch-worker/` 与 `services/control-plane-go/` 骨架
- [ ] 新增 `ci / go` job（gofmt / vet / golangci-lint / govulncheck / go test -race）
- [ ] Go 的 canonical/hash 接入 `docs/rag_vectors.json` 跨语言守卫
- [ ] PG 集成测试接入真实迁移（pgvector/pgvector:pg16）
- [ ] 回退开关 `RAG_FETCH_WORKER_IMPL` 与 DRY_RUN 演练
- [ ] 度量基线（t16）先于扩围；用数据决定是否扩大 Go
- [ ] `ci / go` 稳定后再加入分支保护必需检查

---

## 6. 与现有实现的对照（避免新人误读）

- `crates/server/src/rag_fetch.rs` 的 `canonical_identifier` / `hash_identifier` / `extract_text` / `store_fetch_result` 是**行为金标准**；Go 试点只允许「等价复刻」，不允许改语义。
- 状态机终态：`fetched` / `skipped` 不再重入；`failed` 仅在 `RAG_FETCH_RETRY_FAILED=true` 时重选；`fetching` 只有超时才回收。Go 必须完全同构。
- `rag_candidates.doc_id` 实际存的是**规范化 identifier**（列名历史遗留），不是外键；Go 不得据此做 FK 假设。

---

## §5 度量基线（t16，2026-11，Windows 16 vCPU / 15.2 GB / cargo 1.97.1）

> 引入新语言前先量化「迭代慢」到底慢在哪。以下为实测数据。

### 本地 Rust 迭代

| 操作 | 耗时 |
|---|---|
| 冷构建 `cargo build --workspace`（空 target） | **77.3s** |
| 空转（target 已热） | 0.7s |
| 改一行 → 编译：core / server / shared | 30.5s / 15.1s / 11.1s |
| 改一行 → 测试绿：core（测试执行仅 2.2s） | **37.4s** |
| `clippy --workspace` | 21.9s |

### CI 关键路径（main 绿跑 42592541191，整轮 212s）

| Job | 耗时 | 是否关键路径 |
|---|---|---|
| **Android APK** | **212s**（`assembleRelease` 111.6s + rust-android 28.4s + NDK/CMake/下载） | **是** |
| Flutter（analyze/test） | 109s | 是（并行） |
| PostgreSQL | 96s | 并行 |
| **Rust（fmt/clippy/test）** | **66s**（命中 rust-cache） | **否** |
| 其它（dll-e2e / e2e / 镜像 / 迁移 / cargo-deny / secret / docs） | 8–61s | 并行 |

### 结论（对 ADR-20 的影响）

1. **Rust 不是 CI 的关键路径**（66s，且被 Android 212s 盖过）；**Go 不会改善 CI 总时长**。
2. 本地 Rust 的痛点是**编译+链接**，不是语言抽象；先做 sccache / lld / workspace 拆分 / Gradle 预热，收益更高、成本更低。
3. Go 的收益因此**限定在「新增控制面与外部 IO 的开发速度」**，不能拿「CI 太慢」作为引入理由。
4. 若后续度量显示 Go 试点在开发速度上没有优势，按 ADR-20 的退出条件退回 Rust。
