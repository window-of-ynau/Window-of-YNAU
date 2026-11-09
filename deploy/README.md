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
- **`-1`（单事务）不能去掉**：迁移里有 `DROP CONSTRAINT → ADD CONSTRAINT` 这类顺序敏感的 DDL。
- **`-1` 只能加在 `-f`（整份迁移文件）上**：`psql -1 -c "CREATE DATABASE ..."` 会报 `cannot run inside a transaction block`——`CREATE DATABASE` 不能在事务块里跑。CI 的迁移 job 里，前缀只带 `-v ON_ERROR_STOP=1`，`-1` 只加在文件级 `psql -f`（t81/M2 的真实教训，别统一加）。
  非单事务时，ADD 失败而 DROP 已提交，等于**把约束永久删掉**（RAG 的 `rag_candidates_status_check`
  就踩过这个坑：库里存在 `status='fetching'` 的行时重跑会让约束丢失）。
  加 `-1` 后每份迁移要么全生效、要么全回滚。
- 重跑前确认 SQL 幂等（`IF NOT EXISTS` / 迁移记录表）；非幂等的迁移重复执行会报错，别用 `ON_ERROR_STOP=0` 硬吞。
- 如果宿主机已经装了 `psql`，也可以直接连——但默认没发布 5432，需要临时在 compose 里映射或用 `docker compose exec`。映射之后就是常规写法：

```bash
for f in crates/server/migrations/*.sql; do
  psql "postgres://yunong@127.0.0.1:5432/yunong" -v ON_ERROR_STOP=1 -1 -f "$f"
done
```

- 服务端若自带启动迁移，以它为准；当前按设计由部署方执行（服务端只管运行期查询）。

## RAG 抓取 worker 的运维开关

抓取默认**关闭**（`RAG_FETCH_ENABLED=false`）：CI 与离线环境都不允许联网抓取。

| 变量 | 默认 | 说明 |
|---|---|---|
| `RAG_FETCH_ENABLED` | `false` | 打开后每 6 小时跑一轮（只处理 `pending`） |
| `RAG_FETCH_TOP_N` | `100` | 每轮按 `count` 取前 N 条 |
| `RAG_FETCH_RETRIES` | `3` | 单条重试次数（总计 `retries+1` 次） |
| `RAG_FETCH_MIN_INTERVAL_MS` | `1000` | 两次抓取之间的最小间隔（对 arXiv 礼貌访问） |
| `RAG_FETCH_TIMEOUT_SECS` | `10` | 单次 HTTP 超时 |
| `RAG_FETCH_STALE_AFTER_SECS` | `1800` | `fetching` 占位多久算"上个 worker 崩了"，可被下一轮回收 |
| `RAG_FETCH_MAX_BODY_BYTES` | `2097152` | 正文字节上限；超过判失败（不落库） |
| `RAG_FETCH_DOI_ALLOWED_HOSTS` | 见 `crates/server/src/config.rs` 的 `DEFAULT_DOI_HOSTS` | DOI 抓取允许跟随的重定向域名白名单 |
| `RAG_FETCH_RETRY_FAILED` | `false` | **重试失败项**：设为 `true` 后本轮把 `status='failed'` 的候选也纳入 |
| `RAG_REPORT_PER_MIN` | `30` | `/api/v1/rag/referrals` 每账号每分钟上限（0 = 关闭） |

**怎么重试失败项**：把 `RAG_FETCH_RETRY_FAILED=true` 写进 `deploy/.env` 并重启 `server`，
下一轮（或手动触发一轮）会把本月 `failed` 的候选重新抓一遍；**确认恢复后记得改回 `false`**，
否则失败项会被无限重试（每 6 小时一轮）。

**怎么看待 DOI 重定向**：`doi.org` 是 302 跳到出版商，不跟随则 DOI 源完全不可用。
抓取器只在 `RAG_FETCH_DOI_ALLOWED_HOSTS` 白名单内跟随（最多 5 跳、只允许 https、拒绝私网地址）。
这与上游模型 API 的"完全不跟随重定向"不冲突——那是我们自己配置的地址，这是抓外网公开资源，风险模型不同。

**不上白名单的源**：`application/pdf` 等二进制内容直接判失败不落库（我们不解析 PDF）。
正文按"去标签 + 解实体 + 折叠空白"粗清洗后写入 `rag_known_docs.doc_text`（上限 20 万字符），
正式清洗管线（去导航/分节/引用解析）另排期。

## 冒烟验证

```bash
curl -sS http://127.0.0.1:8080/healthz
curl -sS http://127.0.0.1:8080/readyz        # DB 可用 + 上游配置存在才算就绪

# SSE 流式接口（-N 关掉 curl 自己的缓冲；带鉴权的环境补 -H 'Authorization: Bearer ...'）
curl -N -X POST http://127.0.0.1:8080/api/v1/chat/stream \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"s_demo","turn_id":"t_demo","round":1,"model":"deepseek-flash","protocol":"openai_completions","messages":[{"role":"user","content":"下周三下午有课吗"}],"tools":[],"max_output_tokens":512,"stream":true,"estimated_input_tokens":16}'
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
# 注意：冒烟会断言 org.opencontainers.image.revision 是 40 位 commit，
# 本地构建镜像时要传 --build-arg GIT_SHA=$(git rev-parse HEAD)
```

## 发布流程（版本标签与产物）

发布由 `.github/workflows/release.yml` 负责，**推 `v*` 标签触发**（不需要手动点按钮）：

```bash
# 1. 确认 main 上 CI 全绿（rust / flutter / docs / migrations / pg / docker 六个 job）
gh run list --branch main --limit 3

# 2. 版本号三处对齐：git tag vX.Y.Z == Cargo.toml 的 [workspace.package].version
#    == app/pubspec.yaml 的 version 前缀。release workflow 的 version-check job 会硬校验，
#    不一致直接红（安装包版本由 Cargo.toml 推导、APK 由 pubspec 推导，所以卡这里等于卡住四个产物）
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
| **Windows 安装包** | `WindowOfYNAU-vX.Y.Z-setup.exe`（Actions artifact + Release asset） | Inno Setup 6 构建；安装 `yunong.exe` + `yunong_core.dll` + `flutter_windows.dll` + `data\`（DLL 必须与 exe 同目录） |

### Windows 安装包（Inno Setup）

选 **Inno Setup 6** 而不是 WiX：单文件 `.iss` + 一条 `ISCC` 命令就能进 CI（`choco install innosetup -y`），
中文界面、开始菜单/可选桌面快捷方式、卸载程序都是声明式配置；而"卸载时保留用户数据并提示位置"
需要写一小段 Pascal 回调，Inno 的 `CurUninstallStepChanged` 三行就够了，WiX 那边要额外自定义 Action。
对本项目的桌面端规模，WiX 的 `.wxs/.wixproj` + Toolset 版本矩阵不划算。

```bash
# 本机打安装包（先构建 exe + DLL，再过护栏 → 白名单暂存 → ISCC）
pwsh scripts/build-windows-installer.ps1 -Version 0.9.1
# 产物：dist/WindowOfYNAU-0.9.1-setup.exe + dist/SHA256SUMS
```

**⚠️ 不要直接对 `app/build/windows/x64/runner/Release/` 打整包**——这是 2026-11 真实发生过的事故：
在 Release 目录里跑过一次 App 之后，WebView2 会在 exe 同级生成 `yunong.exe.WebView2/EBWebView/`，
里面有**本机保存的 Cookie 与 Login Data**（校方会话、登录凭据）。t154 之前的 `.iss` 用
`{SourceDir}\*` 通配打包，把这些用户数据一起打进了发布安装包。

现在打包脚本是**白名单模式**，顺序是：

1. `scripts/build-windows-app.ps1` 构建 `yunong.exe` + `yunong_core.dll` + Flutter 产物；
2. **护栏 A**：扫 Release 目录，出现 `*.WebView2` / `EBWebView` / `*.db` / `secrets` / `*.pdb` / `.env` / `*.jks` 直接失败并列出路径；
3. **白名单暂存** `build/installer-staging/`：只复制 `yunong.exe`、`yunong_core.dll`、`flutter_windows.dll`、`flutter_inappwebview_windows_plugin.dll`、`WebView2Loader.dll`、`native_assets.json`（可选）与 `data\`（排除 db/secrets/.env/pdb）；
4. ISCC 只对暂存目录编译（`.iss` 里另带 `Excludes` 作第二道保险）；
5. **护栏 C**：安装包 ≥ 40 MB 直接失败（异常增大通常意味着混进了用户数据/调试符号）；
6. **护栏 D**：比对 `setup.exe` 与 `yunong.exe` 的图标 SHA256，确认安装向导用的是应用图标而不是 Inno 默认图标。

发布流水线里还有 **护栏 B**：静默安装到临时目录后复查安装目录里没有 `.WebView2` / `*.db` / `secrets`，
再断言卸载后 `%LOCALAPPDATA%\yunong` 仍保留。

**前置**：本机需要 **Inno Setup 6**（`winget install JRSoftware.InnoSetup`）。脚本会先查注册表里的
`InstallLocation` 定位 `ISCC.exe`，再回退到常见安装路径——不再依赖硬编码目录（本机它就装在
`D:\Program Files\Inno Setup 6`）。找不到时脚本会给出安装提示。

- **未签名**：暂无代码签名证书。用户首次运行会看到 SmartScreen"Windows 已保护你的电脑"，
  处理方式：点 **更多信息 → 仍要运行**（发布说明里也要写这句）。拿到证书后把 `signtool` 加进
  `scripts/build-windows-installer.ps1` 的步骤 5 之前即可，`.iss` 不用改。
- **用户数据不随卸载删除**：安装目录删干净，但 `%LOCALAPPDATA%\yunong`（SQLite 课表库、记忆/凭据的
  DPAPI 密文、设置）保留；卸载完成页会弹出该路径。要彻底清除只能手动删目录——这是有意的，
  卸载不该毁掉用户数据。
- CI 里除了构建，还会做一次**从零安装验证**：静默装到临时目录 → 断言 `yunong.exe`、`yunong_core.dll`、
  `flutter_windows.dll`、`data\` 都在 → 静默卸载 → 断言 `%LOCALAPPDATA%\yunong` 未被删。

### Android 签名 APK（配好 4 个 Secrets 即可用）

CI 的 `android` job 有两种模式，日志里会明确打印走了哪条：

- 有 `KEYSTORE_BASE64` → `using release keystore (KEYSTORE_BASE64 已配置, alias=…)`，产出正式签名 APK；
- 没有 → `fallback to debug signing (secrets missing)` + `::warning::`，只产出 debug APK（可在测试机侧载，**不可发布**）。

护栏：如果配了 Secrets 但 APK 仍由 debug keystore 签名（`apksigner --print-certs` 里出现 `CN=Android Debug`），
job **直接失败**——防止"以为签了其实没签"。想不打包 tag 也验证这条链路：

```bash
gh workflow run release --ref main -f dry_run=true -f build_android=true
```

**owner 要做四步**（一次性）：

```bash
# 1. 生成 upload keystore（口令自己定；有效期 10000 天 ≈ 27 年）
keytool -genkeypair -v -keystore upload-keystore.jks -keyalg RSA -keysize 2048 \
  -validity 10000 -alias upload -storetype JKS

# 2. base64 编码（Linux/macOS；Windows 用 pwsh 的 [Convert]::ToBase64String 或 certutil -encode）
base64 -w0 upload-keystore.jks > keystore.b64

# 3. 写入 4 个 GitHub Secrets
gh secret set KEYSTORE_BASE64   --body "$(cat keystore.b64)"
gh secret set KEYSTORE_PASSWORD --body '<store password>'
gh secret set KEY_ALIAS         --body 'upload'
gh secret set KEY_PASSWORD      --body '<key password>'

# 4. 立刻删掉本地副本
rm -f keystore.b64 upload-keystore.jks
```

**验证方式**：跑 `gh workflow run release --ref main -f dry_run=true -f build_android=true`，
日志里应看到 `using release keystore` 与 `OK：APK 由 release keystore 签名（非 Android Debug）`；
本地可用 `apksigner verify --print-certs app-release.apk` 看证书 DN。

**keystore 绝不能提交进仓库**（`.gitignore` 已覆盖 `*.jks` / `*.keystore` / `app/android/key.properties`）。
**丢失 keystore 的后果**：已发布到应用商店/分发的 APK 无法再更新——包名不变时 Android 要求新包用**同一签名**
（否则用户必须先卸载、数据全丢）。所以 keystore 与两个口令要离线备份（密码管理器 + 加密备份各一份）。

### 干跑发布流水线

```bash
# 不推 GHCR、不打 Release，只验证四个 job 能跑通（含 Windows 安装包的装/卸闭环）
gh workflow run release --ref main -f dry_run=true
gh run watch <run-id>
```

**镜像先冒烟后推送**：构建只 load 到 runner 本地 → `scripts/docker-smoke.sh` 起容器探 `/healthz`、`/readyz`、检查二进制 ≥2MB 与 uid=10001 → 全过才 `docker push`。坏镜像不会进 GHCR，也不会顶掉 `:latest`。

**镜像自带你需要的元数据（OCI labels）**：`org.opencontainers.image.revision` = 构建时的 commit（CI / release 传 `--build-arg GIT_SHA=${{ github.sha }}`），另有 `created` / `version` / `source` / `title` / `licenses`。排查线上"这容器跑的是哪个提交"时：

```bash
docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' ghcr.io/<owner>/yunong-server:vX.Y.Z
# 输出 40 位 commit sha；scripts/docker-smoke.sh 也会断言它非空且长度 40
```

**干跑**（验证发布流水线本身，不推送 GHCR）：

```bash
gh workflow run release --ref <分支名> -f dry_run=true
gh run watch
```

**回滚**：`:latest` 只是便利标签，回滚请显式部署上一个 tag 的镜像——在 compose 里给 `server` 写死 `image: ghcr.io/<owner>/yunong-server:v0.0.9` 再 `up -d`，不要依赖 `latest`。

## CI 工作流的关键约束（改 `.github/**` 之前先读）

| 约束 | 为什么 |
|---|---|
| `ci.yml` 各 job 的 `name:` = 分支保护的必需检查名 | 改名等于静默取消保护。当前必需（9 个）：`Rust（fmt / clippy / test）`、`Flutter（analyze / test）`、`文档（表格 / 相对链接）`、`迁移脚本（PG16 + pgvector）`、`PostgreSQL（auth 真实 SQL 集成测试）`、`服务端镜像（build + /healthz 冒烟）`、`端到端对话闭环（mock 上游）`、`密钥扫描（secret scanning）`、`依赖安全与许可证（cargo-deny）`。改名前先 `gh api repos/<owner>/<repo>/branches/main/protection` 看现状，改完同步更新 required checks |
| 迁移按文件名 glob **全量**跑（0001 → 最后一个） | 只跑部分文件会漏后加的表/索引；`migrations` 与 `pg` 两个 job 都必须全量 |
| `persist-credentials: false` 保持 | 不把 token 写进 `.git/config`；需要凭据的步骤（GHCR 登录）单独用 `GITHUB_TOKEN` |
| `release.yml` 是唯一提权 workflow（`packages: write`） | 不要在 `ci.yml` 里加 `packages: write` / `contents: write` |
| 镜像冒烟只有一份实现：`scripts/docker-smoke.sh` | CI 的 docker job 与 release 的发布前检查共用，改断言只改一处 |
| `security`（cargo-deny）job 已是必需检查 | 连续多次绿后（t69）加入；依赖升级 / 新增依赖后若红，先看 `deny.toml` 白名单与 `cargo deny check advisories` 输出 |
| PG 用例不并行加锁（不要 `--test-threads=1`） | 每条用例用独立 schema（`yunong_pg_<uuid>`）隔离，并行是安全的；单线程只会白白变慢 |

## 端到端闭环（无凭据）

CI 的 `e2e` job（`端到端对话闭环（mock 上游）`）验证的是"**用户能完成一次对话**"，而不是"各组件单测过了"：

```
mock 上游(假 SSE) ← 服务端容器 ← 脚本客户端（注册 → 登录 → /api/v1/chat/stream）
```

**不需要任何真实账号或 Key**：mock 上游不校验 Key；用户在测试里现注册现登录。

- 编排脚本：`scripts/e2e-chat.py`（docker + python3 stdlib，无需 psql——迁移和断言都走 pgvector 镜像里的 psql）；
- mock 上游：`scripts/mock-upstream/mock_upstream.py`，按请求体里的 `model` 选形态：
  `mock-ok`（正常流）、`mock-500`、`mock-html`（非 SSE）、`mock-hang`（挂起）、`mock-stream-error`（流内错误帧）；
- 覆盖的断言：正常流契约（`meta` 首 / `usage` / `done` 末且唯一）、上游 500 → 502 **不泄露原文**、
  非 SSE → 502、流内错误帧 → `event: error`（终止事件）且洗掉厂商原文、首事件超时 → 504、
  100% 抽样时 `usage_samples` 落 1 行且 token 数与 mock 一致、上游失败写 `audit_logs(decision='error')`。

本地复现（需要 Docker，Windows/macOS/Linux 都行；容器通过 `host.docker.internal` 访问本机 mock）：

> mock 必须监听 `0.0.0.0`：Linux runner 上容器里的 `host.docker.internal` 是 docker 网关 IP，
> 只绑 `127.0.0.1` 时 CI 连不上（本机 Docker Desktop 会做转发，所以只在 CI 暴露过）。脚本已经这么传参。

```bash
docker build -f deploy/Dockerfile.server \
  --build-arg GIT_SHA=$(git rev-parse HEAD) \
  -t yunong-server:ci .
python3 scripts/e2e-chat.py --image yunong-server:ci
# 失败时保留容器排查：python3 scripts/e2e-chat.py --image yunong-server:ci --keep
```

**当前节奏**：该 job 带 `continue-on-error: true`，先观察 3~5 个 PR，稳定后删掉这行转阻断，
最后再与其他非必需 job 一起用 `gh api` 加进分支保护。

### 两个"假上游"的分工（别合并，也别互相替代）

| 工具 | 用途 | 谁用 |
|---|---|---|
| `crates/core/examples/fake_sse.rs` | **端侧 BYOK 手动联调**（Dart/Flutter 发真实请求，看逐字输出） | 开发者本机 |
| `scripts/mock-upstream/mock_upstream.py` | **CI 服务端 E2E**（6 种场景、容器网络、psql 断言） | CI |

`fake_sse` 只有 ok/429/html 三种形态、没有"流内错误帧"和"挂起"，覆盖不了 `event: error`
与超时路径；CI 闭环用 `scripts/mock-upstream/` 这份。两者是**不同层面的测试替身**，
不是重复实现——断言逻辑只写在 `scripts/e2e-chat.py` 一处。

## 密钥泄露防护（secret scanning）

**事故留档（2026-11）**：owner 的真实 API Key 曾被写进测试代码并推到 GitHub。处置：立即吊销并换新 Key → 脱敏（示例值统一改成 `sk-test-…`）→ 加下面这道闸。影响面：该 Key 在泄露窗口内可被任意人使用，需按"已泄露"看待（吊销是唯一正确的收尾），不是改代码就能当没发生。

**现在的双保险**（规则只有一份：`scripts/secret-scan.py`）：

| 时机 | 触发 | 行为 |
|---|---|---|
| 提交前 | `.git/hooks/pre-commit` → `scripts/secret-scan.py --staged` | 命中即**拒绝提交** |
| PR / push main | CI `密钥扫描（secret scanning）` job → `scripts/secret-scan.py --all` | 命中即红（当前阶段 1 `continue-on-error`，观察 3~5 个 PR 后转阻断） |

**每台开发机装一次钩子**：

```bash
bash scripts/install-hooks.sh                 # Linux / macOS / Git Bash
powershell -File scripts/install-hooks.ps1    # Windows PowerShell
```

规则：`sk-` + ≥20 位（**`sk-test-` 前缀放行**）、`AKIA`+16 位、`gh[pousr]_`+≥20 位、
`-----BEGIN … PRIVATE KEY-----`、带 ≥16 位密码的 DSN。误报用 `ALLOW_SECRET=1 git commit …`
放行一次（会打 `::warning::`），长期误报请改写示例而不是常开这个开关。

**fail-open**：钩子发现 `scripts/secret-scan.py` 不存在（切到旧分支、脚本还没合入）时只警告并放行，不会把提交卡死——这条是踩过的坑，别改成硬失败。

**为什么不用 gitleaks**：gitleaks 规则更全，但要额外引一个 action / 二进制；本项目当前
只需要"拦住我们实际踩过的那几类"（尤其 `sk-` 前缀），轻量正则 + 一份脚本就能同时服务
CI 与 pre-commit，且**离线可跑、规则可读**。等规则明显不够用时再换。

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


