# deploy · 部署与上线

这套文件负责"把服务端跑起来"：镜像、单机 compose、环境变量模板，以及生产环境上 HTTPS 反代的姿势。不负责构建 Flutter 客户端，也不负责 CI。

相关：[bench/README.md](../bench/README.md)（上线前后跑同一批问句，看延迟/成本/错误率有没有退化）、[docs/03-接口文档.md](../docs/03-接口文档.md)（接口与 SSE 契约）。

## 里面有什么

| 文件 | 用途 |
|---|---|
| `Dockerfile.server` | 多阶段构建：Rust 编译 → Debian slim 运行，非 root，只装 CA 证书 |
| `docker-compose.yml` | `db`（pgvector/pg16，不对外暴露）+ `server`（8080，等 db healthy） |
| `.env.example` | 环境变量模板；复制成 `.env` 后填写，`.env` 永远不进 Git |

## 前置

- Docker Engine + Compose v2（`docker compose version` 能跑）。
- 仓库根有 `Cargo.toml`、`Cargo.lock` 和 `crates/{core,server,shared}`；`Cargo.lock` 要提交（镜像按 lock 构建）。
- 镜像构建**不连数据库**：sqlx 用运行期查询，所以构建机上没有 DB 也能 build。

## 本地起服务

```bash
# 1. 准备变量（别把 .env 提交）
cp deploy/.env.example deploy/.env
# 编辑 deploy/.env：POSTGRES_PASSWORD 与 DATABASE_URL 里的密码要一致

# 2. 构建并启动（首次会编译 Rust，慢一点）
#    --env-file 显式指定，避免"在哪个目录执行 compose"导致读不到变量
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d --build

# 3. 看状态
docker compose --env-file deploy/.env -f deploy/docker-compose.yml ps
docker compose --env-file deploy/.env -f deploy/docker-compose.yml logs -f server
```

此时 `server` 只绑在宿主机 `127.0.0.1:8080`。真机联调要让手机连过来，把 `deploy/.env` 里的 `SERVER_BIND=0.0.0.0` 打开，或者在 compose 里把端口改成 `"0.0.0.0:8080:8080"`——**仅限内网，别直接开在公网**。

## 跑数据库迁移

迁移文件在 `crates/server/migrations/`，按文件名顺序执行，每个文件一个事务：

```bash
# 在仓库根执行；db 没有映射到宿主机端口，所以用 compose exec 送进去
for f in crates/server/migrations/*.sql; do
  docker compose --env-file deploy/.env -f deploy/docker-compose.yml exec -T db \
    psql -U yunong -d yunong -v ON_ERROR_STOP=1 -1 -f - < "$f"
done
```

注意：

- 顺序执行，别打乱；`CREATE EXTENSION vector` 需要超级用户，compose 里的 `POSTGRES_USER`（yunong）是超级用户，够用。
- 重跑前确认 SQL 幂等（`IF NOT EXISTS` / 迁移记录表）；非幂等的迁移重复执行会报错，别用 `ON_ERROR_STOP=0` 硬吞。
- 如果宿主机已经装了 `psql`，也可以直接连——但默认没发布 5432，需要临时在 compose 里映射或用 `docker compose exec`。映射之后就是常规写法：

```bash
for f in crates/server/migrations/*.sql; do
  psql "postgres://yunong@127.0.0.1:5432/yunong" -v ON_ERROR_STOP=1 -1 -f "$f"
done
```

- 服务端若自带启动迁移，以它为准；当前按设计由部署方执行（服务端只管运行期查询）。

## 冒烟验证

```bash
curl -sS http://127.0.0.1:8080/healthz
curl -sS http://127.0.0.1:8080/readyz        # DB 可用 + 上游配置存在才算就绪

# SSE 流式接口（-N 关掉 curl 自己的缓冲；带鉴权的环境补 -H 'Authorization: Bearer ...'）
curl -N -X POST http://127.0.0.1:8080/api/v1/chat/stream \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"s_demo","turn_id":"t_demo","round":1,"model":"deepseek-chat","protocol":"openai_completions","messages":[{"role":"user","content":"下周三下午有课吗"}],"tools":[],"max_output_tokens":512,"stream":true,"estimated_input_tokens":16}'
```

期望看到 `event: meta` → `delta` → `usage` → `done`；异常是 `event: error` 且带 `code`。想看量化指标直接跑 `python bench/run.py`。

## 生产反向代理 + TLS

原则：公网只开 443，8080 只绑回环；**反代必须关闭响应缓冲**，否则 SSE 会被攒成一大块，TTFT 直接报废；`proxy_read_timeout` 要大于最长生成时间（心跳每 15s，正常不会先断）。

Caddy（自动申请/续期证书，5 行）：

```caddyfile
api.yunong.example.com {
	encode zstd gzip
	reverse_proxy 127.0.0.1:8080 {
		flush_interval -1
	}
}
```

`flush_interval -1` 就是"每个 chunk 立即下发"，等价于 Nginx 的 `proxy_buffering off`。`encode` 对 SSE 会自动跳过，不用担心。

Nginx（证书用 certbot 签，这里省略申请步骤；核心 5 行在 `location` 里）：

```nginx
server {
    listen 443 ssl http2;
    server_name api.yunong.example.com;
    ssl_certificate     /etc/letsencrypt/live/api.yunong.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/api.yunong.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Connection "";      # SSE 长连接
        proxy_buffering off;                 # 必须关，否则流式变批量
        proxy_read_timeout 3600s;
    }
}
```

另外建议：`client_max_body_size` 按单轮输入上限给（`MAX_INPUT_TOKENS` 不是字节数，别直接相等）；限流不要加在 `/healthz` `/readyz` 上；反代层不要把 `X-Request-Id` 吃掉，排查问题全靠它。

## "套餐按时间授权"对部署的影响

计费模型是**按时间授权、不做 token 记账**：授权记录里只有到期时间（接口上的 `entitlement_expires_at`），token 只用 1% 抽样做成本监控，不参与计费。因此：

1. **不需要任何"额度清零"定时任务**：没有"每月剩余额度"这种状态，也就没有月末批量重置、对账、补发。过期与否在请求时比较时间即可。
2. **不需要额度计数器/Redis 记账**：扩容瓶颈是 `CONCURRENCY_LIMIT`（同时在途流）和上游厂商限流，不是扣额度。注意：如果并发闸门放在进程内存里，多副本会放大实际并发，扩副本前先确认；单机 compose 默认 1 副本，不受影响。
3. **时钟就是授权边界**：宿主机必须开 NTP（chrony/systemd-timesyncd），容器跟宿主机走。机器时间漂移会让授权提前失效或延后生效，也会让 JWT 过期判断异常。所有时间用 UTC 比较，不依赖容器时区。
4. **备份重点变了**：授权/订单/用户表比抽样用量表重要得多；抽样成本数据丢了只影响成本监控，不影响用户是否还能用。1% 样本永远不要拿来当账单或退款依据。
5. **续费/升级 = 改到期时间**：不用迁移用量、不用补账；过期请求返回 `402 QUOTA_EXCEEDED`，客户端引导续费即可。
6. **`MAX_INPUT_TOKENS` 是保护阈值**，用于"先拒后转"避免超额请求打到上游，不是额度。
7. **防的是"预算失控"，不是"额度超发"**：时间授权没有实时扣减，一个用户在到期前理论上可以持续消耗；生产上靠上游厂商的预算告警 + 反代/服务端的速率与并发限制兜底，不要指望"额度用完自动停"（这条已被 ADR-03 列为风险项）。

## 安全与排错

- 密钥：只有 `deploy/.env` 保存真实值，已在 `.gitignore` 里（没有的话请补上）；镜像里只有二进制和 CA 证书，不 COPY 任何 `.env`。
- 运行身份：镜像用固定 uid/gid `10001:10001` 非 root 运行；挂载日志/数据卷时注意宿主机目录权限。
- 日志：`RUST_LOG=info` 足够；生产别开 debug，避免把 prompt/正文带进日志（会话正文按设计不落库）。
- `/readyz` 一直失败：先看 `docker compose ps` 里 db 是否 healthy，再核对 `DATABASE_URL`，最后确认迁移跑过、上游配置存在。
- SSE 卡住/一次性吐完：99% 是反代缓冲没关，查 `proxy_buffering` / `flush_interval`。
- `429 CONCURRENCY_LIMIT`：同一用户已有在途流；客户端要保证同一 `turn_id` 串行，别把多轮请求并发出去。
- `401 UNAUTHORIZED`：接口需要 Bearer token；本地跑 bench 时用 `--token` 传。
- 构建慢：仓库根已有 `.dockerignore`（忽略 `target/`、`app/`、`.git/`、`.worktrees/`、`deploy/.env` 等），本机构建上下文从 ~10GB 降到 1MB 量级；**新增 Dockerfile `COPY` 目录时记得同步它**，被 ignore 掉的目录会直接构建失败。
- 镜像"能构建"不等于"能跑"：空壳二进制（Dockerfile 依赖缓存层 mtime 问题）会让容器秒退。CI 的 `docker` job 与发布流水线都会跑 `scripts/docker-smoke.sh`（真起容器 + 探 `/healthz`），本机也可以直接跑：

```bash
bash scripts/docker-smoke.sh yunong-server:ci   # 或任意本地镜像 tag
```

## 发布流程（版本标签与产物）

发布由 `.github/workflows/release.yml` 负责，**推 `v*` 标签触发**（不需要手动点按钮）：

```bash
# 1. 确认 main 上 CI 全绿（rust / flutter / docs / migrations / pg / docker 六个 job）
gh run list --branch main --limit 3

# 2. 版本号对齐：tag vX.Y.Z 必须等于 Cargo.toml 的 [workspace.package].version
#    （workflow 会校验，不一致直接红）app/pubspec.yaml 的版本前缀建议同步，不一致只 warning
grep -A2 '\[workspace.package\]' Cargo.toml
grep '^version:' app/pubspec.yaml

# 3. 打标签并推送；标签必须指向 main 上的提交（workflow 用 merge-base 校验）
git checkout main && git pull --ff-only
git tag -a v0.1.0 -m "云农之窗 v0.1.0"
git push origin v0.1.0
```

| 产物 | 位置 | 说明 |
|---|---|---|
| 服务端镜像 | `ghcr.io/<owner>/yunong-server:vX.Y.Z` 与 `:latest` | 多阶段构建、非 root、只含二进制 + CA 证书；**冒烟通过后才推送** |
| Android APK | Actions artifact `yunong-apk-vX.Y.Z` | 4 个签名 secret 齐备才出 release 包，否则降级 debug 并在日志里 `::warning::` |
| APK 校验和 | 同一 artifact 里的 `SHA256SUMS` | 下载后 `sha256sum -c SHA256SUMS` 验证 |

**镜像先冒烟后推送**：构建只 load 到 runner 本地 → `scripts/docker-smoke.sh` 起容器探 `/healthz`、`/readyz`、检查二进制 ≥2MB 与 uid=10001 → 全过才 `docker push`。坏镜像不会进 GHCR，也不会顶掉 `:latest`。

**干跑**（验证发布流水线本身，不推送 GHCR）：

```bash
gh workflow run release --ref <分支名> -f dry_run=true
gh run watch
```

**回滚**：`:latest` 只是便利标签，回滚请显式部署上一个 tag 的镜像——在 compose 里给 `server` 写死 `image: ghcr.io/<owner>/yunong-server:v0.0.9` 再 `up -d`，不要依赖 `latest`。

## CI 工作流的关键约束（改 `.github/**` 之前先读）

| 约束 | 为什么 |
|---|---|
| `ci.yml` 各 job 的 `name:` = 分支保护的必需检查名 | 改名等于静默取消保护。当前必需：`Rust（fmt / clippy / test）`、`Flutter（analyze / test）`、`文档（表格 / 相对链接）`、`迁移脚本（PG16 + pgvector）`、`PostgreSQL（auth 真实 SQL 集成测试）`。改名前先 `gh api repos/<owner>/<repo>/branches/main/protection` 看现状，改完同步更新 required checks |
| 迁移按文件名 glob **全量**跑（0001 → 最后一个） | 只跑部分文件会漏后加的表/索引；`migrations` 与 `pg` 两个 job 都必须全量 |
| `persist-credentials: false` 保持 | 不把 token 写进 `.git/config`；需要凭据的步骤（GHCR 登录）单独用 `GITHUB_TOKEN` |
| `release.yml` 是唯一提权 workflow（`packages: write`） | 不要在 `ci.yml` 里加 `packages: write` / `contents: write` |
| 镜像冒烟只有一份实现：`scripts/docker-smoke.sh` | CI 的 docker job 与 release 的发布前检查共用，改断言只改一处 |
| `docker` / `security` job 暂不在必需检查里 | 先观察耗时与稳定性；稳定后再用 `gh api` 一次性加入 required checks（加之前它们只报警、不阻塞合入） |
| PG 用例不并行加锁（不要 `--test-threads=1`） | 每条用例用独立 schema（`yunong_pg_<uuid>`）隔离，并行是安全的；单线程只会白白变慢 |

## 依赖安全与许可证（cargo-deny）

CI 的 `security` job（`依赖安全与许可证（cargo-deny）`）在每次 PR / push main 时跑 [cargo-deny](https://embarkstudios.github.io/cargo-deny/)，配置在仓库根 `deny.toml`，检查四类东西：

| 检查 | 内容 | 当前姿态 |
|---|---|---|
| `advisories` | RustSec 漏洞库（含 yanked、unmaintained） | 漏洞一律 error，**阶段 2 起阻断合入**（2026-11 依赖升级后基线已清零） |
| `licenses` | 许可证白名单（AGPL-3.0 + 商业双许可 → 只允许宽松/弱著佐权项） | 白名单外一律 error（阻断） |
| `bans` | 禁用依赖：直接依赖 `openssl-sys` / `native-tls`（服务端统一 rustls）；重复版本与通配版本只告警 | 命中即 error（阻断） |
| `sources` | 依赖来源：只允许 crates.io 官方索引，禁 git 依赖与未知 registry | 命中即 error（阻断） |

**为什么选 cargo-deny 而不是 cargo-audit**：cargo-audit 只查 RustSec 漏洞；cargo-deny 一次把漏洞、许可证、禁用依赖、来源都查了——本项目是双许可，许可证闸门与"服务端只用 rustls"这类依赖政策和漏洞同等重要。

### 当前状态：已阻断（阶段 2）

- **阶段 1（已完成，留作记录）**：门禁刚接入时先以 `continue-on-error: true` 跑了一轮，只记录基线。那次基线是 **7 条漏洞**：`rustls 0.23.45` ×4（RUSTSEC-2026-0346/0347/0348/0349）、`rustls-webpki 0.103.15`（-0382）、`tokio 1.54.0`（-0384，且已被 yank）、`webpki-roots 1.0.9`（-0381），全部有修复版本。
- **阶段 2（当前）**：升级 `rustls 0.23.46` / `tokio 1.54.1` / `rustls-webpki 0.103.16` / `webpki-roots 1.0.11` 后 `cargo deny check` 变成 `advisories ok, bans ok, licenses ok, sources ok`；`ci.yml` 已去掉 `continue-on-error`，**命中即红**。
- **还剩一步**：先观察 3~5 个 PR 的稳定性与耗时，再用 `gh api` 把 `依赖安全与许可证（cargo-deny）` 加进分支保护必需检查（与 `docker` job 同一节奏）。在此之前它变红会「看得见但不阻塞合入」。

> 注意：cargo-deny ≥ 0.18 **移除了 advisories 的 `vulnerability` 级别开关**（漏洞一律按 error 报），所以"先 warn 后 deny"只能在 CI 层用 `continue-on-error` 表达；在 `deny.toml` 里写 `vulnerability = "warn"` 会让配置解析直接报错。

### 命中时怎么看、怎么处理

cargo-deny 的人类可读输出直接给足四要素，例如：

```text
error[vulnerability]: Potential segfault in the time crate
  ┌─ ...
Crate:     time
Version:   0.1.45
ID:        RUSTSEC-2020-0071
Solution:  Upgrade to >=0.2.23     # 没有修复版本时这里会写明无可用修复
```

处理路径：
1. **能升级** → 单独提依赖升级 PR（不要顺手改 `Cargo.lock` 混进功能 PR，它会牵动所有 CI job）；
2. **确认不可利用 / 不影响** → 写进 `deny.toml` 的 `[advisories].ignore`，**必须写原因**，并定期复审（升级依赖后要回来删掉已失效的 ignore）；
3. **许可证拿不准** → 先报 captain，**不要自行往 `[licenses].allow` 里加**（AGPL + 商业双许可的兼容性判断不能由 CI 维护者单方面放宽）。

### 本地复现（需要网络，离线环境跑不了）

```bash
cargo install cargo-deny --locked        # 一次性安装
cargo deny --all-features check          # 四类全查（默认读仓库根 deny.toml）
cargo deny --all-features check advisories   # 只查漏洞
cargo deny --all-features check --format json  # 机器可读
```

**为什么离线跑不了**：cargo-deny 要 `git clone` RustSec advisory-db（默认拉到本地缓存目录），并通过 `cargo metadata` 拉取 crates.io 元数据/sources；内网、沙箱或断网环境会直接失败。这种情况下**以 CI 的 `security` job 结果为准**——这也是把它放 CI 而不是只放本地的原因。


