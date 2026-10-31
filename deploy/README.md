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
- 构建慢：建议在仓库根加 `.dockerignore`（忽略 `target/`、`app/build/`、`.git/`、`bench/out/` 等），本目录不代建。
