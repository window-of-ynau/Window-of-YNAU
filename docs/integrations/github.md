# 云农之窗 · GitHub 组织化落地（自有 Org + 邀队友）

| 项 | 内容 |
|---|---|
| 适用场景 | 仓库在**你自己的 GitHub Organization** 下，1.0 阶段会陆续邀请 2~5 位队友 |
| 前置 | 已读过 [04-ADR.md](../04-ADR.md)（14 条决策是本文的边界来源） |
| 目标 | 队友能提交 PR，但**没人能把半成品直接推上 main**；CI 是合并的唯一闸门；密钥永不进仓库 |
| 不做 | 不写 `.github/` 下的任何文件（工程侧并行负责），本文只给**可复制的配置内容 + 命令** |

> 本文出现的 `<org>` / `yunong` 一律替换成你的组织名与仓库名；`gh` 命令里的 `<org>` 是小写。
> UI 入口的具体文案可能随 GitHub 改版变化，**能小写落地的部分用 `gh api`，只能用 UI 的地方以官方文档为准**。

---

## 0. 一次性准备（5 分钟）

```bash
gh auth status                      # 必须已登录
gh auth refresh -h github.com -s admin:org,repo,read:org   # 建 team / 设分支保护需要 admin:org
gh config set git_protocol ssh      # 可选，但队友用 SSH 更省事
```

先确认三件事，否则后面白做：

| 检查 | 命令 | 期望 |
|---|---|---|
| 组织存在且你是 owner | `gh api /orgs/<org> --jq '.login, .plan.name'` | 输出组织名与计划 |
| **私有仓的分支保护需要付费计划** | 同上，看 `.plan.name` | `free` 且仓库私有 → 分支保护不可用，见 §2.3 |
| 仓库还没建 | `gh repo view <org>/yunong` | 报错则走 §1.3 创建 |

---

## 1. Organization 与 Team

### 1.1 两个 Team 就够，别建更多

| Team | 谁在里面 | 仓库权限 | 为什么是这一档 |
|---|---|---|---|
| `maintainers` | 你 + 1~2 位核心（合并、发版、改 CI） | **Maintain**（仓库级） | 能改仓库设置、管分支、管理 issue/PR，但**不是** Admin：改不了危险开关（删仓库、转所有权） |
| `contributors` | 所有队友 | **Write** | 能开 PR、能推自己的分支、能跑 CI；**推不进 main**（有 §2 的保护） |
| 只读旁观者（如学弟/评审） | 单独邀请 | **Read** | 不要为了"能让 TA 试试"给 Write |

**组织角色 vs 仓库角色**（最容易搞混的一点）：

| 层 | 取值 | 说明 |
|---|---|---|
| Organization role | `Owner` / `Member` | **Owner 能改组织计费与安全设置**。队友一律 `Member`，`Owner` 只留你一个 |
| Repository role | `Read` / `Triage` / `Write` / `Maintain` / `Admin` | 这才是"能不能推代码"的开关 |
| Team 的仓库权限 | `pull` / `triage` / `push` / `maintain` / `admin` | 与上面一一对应，**通过 Team 授予，不要逐个邀请**（人走了只踢 Team） |

### 1.2 建 Team + 授权（全部可脚本化）

```bash
# 1) 建两个 closed team（closed = 组织外看不见成员名单）
gh api -X POST /orgs/<org>/teams -f name=maintainers  -f privacy=closed --jq '.slug'
gh api -X POST /orgs/<org>/teams -f name=contributors -f privacy=closed --jq '.slug'

# 2) 授予仓库权限（注意 permission 的取值是 pull/triage/push/maintain/admin）
gh api -X PUT /orgs/<org>/teams/maintainers/repos/<org>/yunong  -f permission=maintain
gh api -X PUT /orgs/<org>/teams/contributors/repos/<org>/yunong -f permission=push

# 3) 邀请成员（Organization role = direct_member，不要给 admin）
gh api -X PUT /orgs/<org>/memberships/<github-username> -f role=direct_member

# 4) 把人放进 Team（不是放进仓库）
gh api -X PUT /orgs/<org>/teams/contributors/memberships/<github-username>
```

> 邀请后对方要在邮件/GitHub 通知里**接受邀请**才生效，否则 `gh api .../memberships` 显示 `state: pending`。

### 1.3 创建仓库（如果还没建）

```bash
# 从本地已有目录推上去（yunong 根目录下执行）
gh repo create <org>/yunong --private --source=. --remote=origin --push

# 或者先建空仓再推
gh repo create <org>/yunong --private \
  --description "云农之窗 · 校园助手（Rust core + Flutter UI + axum 服务端）"
```

初始设置：

```bash
gh repo edit <org>/yunong \
  --enable-issues --enable-wiki=false \
  --delete-branch-on-merge \
  --enable-squash-merge --enable-merge-commit=false --enable-rebase-merge=false
```

- `--delete-branch-on-merge`：PR 合完自动删源分支，队友不用记着清理；
- 只留 **Squash**：与 §2 的 `Require linear history` 配套，main 的历史永远是干净的直线（一个 PR 一个 commit）；
- `--enable-wiki=false`：知识写进 `docs/`，别分裂成两处。

---

## 2. main 分支保护（**必须做的一步**）

### 2.1 要拦住什么

| 规则 | 拦住的真实事故 |
|---|---|
| Require a pull request before merging | 队友 `git push origin main` 直接推上去 |
| Require **1** approval | 自己给自己合（一个人"写完就合"，CI 绿了也没人看） |
| Require status checks：`ci / rust`、`ci / flutter`、`ci / docs`、`ci / migrations` | 编译不过/文档链接失效/migration 破坏 ADR-08 的改动被合入 |
| Require branches to be up to date | 两个 PR 各自绿、合到一起就红（经典的"半绿合并"） |
| Require **linear history** | merge commit 把 main 搅成网状，`git bisect` 失效 |
| Do **not** allow force push / deletion | `git push -f` 覆盖别人的提交；误删 main |
| Do not allow bypassing（enforce_admins） | "我是管理员所以我直接推"——这条不勾，前面六条等于没设 |

### 2.2 落地：`protection.json` + 一条 `gh api`

把下面内容存成 **`protection.json`**（放在仓库外或临时目录，不要提交进仓库）：

```json
{
  "required_status_checks": {
    "strict": true,
    "contexts": ["ci / rust", "ci / flutter", "ci / docs", "ci / migrations"]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "required_approving_review_count": 1,
    "require_last_push_approval": false
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_conversation_resolution": true,
  "block_creations": false,
  "lock_branch": false
}
```

```bash
gh api -X PUT repos/<org>/yunong/branches/main/protection \
  -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  --input protection.json
```

验证（这三条命令以后每次改保护都重跑）：

```bash
gh api repos/<org>/yunong/branches/main/protection \
  --jq '{linear: .required_linear_history.enabled,
         admins: .enforce_admins.enabled,
         reviews: .required_pull_request_reviews.required_approving_review_count,
         force_push: .allow_force_pushes.enabled,
         checks: .required_status_checks.contexts}'

# 保护是否真的生效：用一个应该被拒的推送测（期望被拒）
git push origin HEAD:main
```

> 若 `PUT` 报 `enforce_admins` 相关错误，单独把它打开一次：
> `gh api -X POST repos/<org>/yunong/branches/main/protection/enforce_admins`
> （不同 API 版本这里是 `PUT` 还是 `POST` **以官方文档为准**；`--jq '.enforce_admins.enabled'` 复验即可。）

### 2.3 两个会让人卡住的坑

1. **私有仓库 + 免费组织**：分支保护（含"必须 PR"）在部分免费计划下对私有仓不可用。
   二选一：① 仓库设为 public（与 [ADR-14](../04-ADR.md) 的 AGPL-3.0 双许可一致，本来就打算开源）；② 升级组织计划。
   判断方法：直接跑 §2.2 的 `PUT`，403 就是计划不够（**具体可用性以官方文档/你的计费页为准**）。
2. **状态检查名必须逐字一致**。GitHub 的 check 名是 `<workflow 的 name> / <job 的 id>`，不是文件名。
   job 用 matrix 时会带后缀（如 `ci / rust (ubuntu-latest)`），那样就**永远 pending**。
   查真实名字（先让 CI 在任意分支跑绿一次）：

   ```bash
   SHA=$(git rev-parse HEAD)
   gh api "repos/<org>/yunong/commits/$SHA/check-runs" --jq '.check_runs[].name'
   ```

   把输出的字符串**原样**抄进 `contexts`。约定见 §3.2：`ci.yml` 的 `name: ci`，job id 用 `rust` / `flutter` / `docs` / `migrations`。

3. **bench 先别进门禁**。[02-系统设计文档 §10.3](../02-系统设计文档.md) 规定 TTFT 退化 >10% 阻断合并，但性能检查天生有抖动；
   1.0 先让 `bench.yml` 只跑 nightly 观察两周，数字稳定后再加进 `contexts`（否则一个抖动就卡死所有 PR）。

---

## 3. Actions 权限最小化

### 3.1 先把默认值压到只读

```bash
# 默认 GITHUB_TOKEN 只读（这是最重要的一行配置）
gh api -X PUT repos/<org>/yunong/actions/permissions/workflow \
  --input - <<'JSON'
{ "default_workflow_permissions": "read", "can_approve_pull_request_reviews": false }
JSON

# 组织级也设一遍，新仓库才会继承
gh api -X PUT /orgs/<org>/actions/permissions/workflow \
  --input - <<'JSON'
{ "default_workflow_permissions": "read", "can_approve_pull_request_reviews": false }
JSON
```

为什么必须只读：PR 里的 workflow 也能拿到 token。默认读写 = 任何人开个 PR 改一行 yml，就能用你的 token 推代码、删 release。

### 3.2 按 workflow 提权的写法

`ci.yml`——**故意不加 `permissions` 之外的任何权限**（下面省略 `steps`，只保留决定状态检查名的那几个字段）：

```yaml
name: ci                      # ← 状态检查名的前半段

on:
  pull_request:
  push:
    branches: [main, dev]

permissions:
  contents: read              # 顶层最小集，所有 job 默认继承，子 job 不再提权

concurrency:                  # 同一分支的新提交取消旧运行，省额度
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  rust:        # cargo fmt --check / clippy -D warnings / test / audit
  flutter:     # flutter analyze + test，构建 app 但不发布
  docs:        # 链接检查、shared → Schema 与 docs 一致性、仓库内禁止 .env
  migrations:  # sqlx migrate --dry-run；出现 CREATE TABLE messages 即失败（ADR-08）
```

> job id 就是状态检查名的后半段（`ci / rust` …），**不要把 job id 起成 `build-rust-2`**，改名＝分支保护失效。

`release.yml`——**全仓唯一提权的 workflow**：

```yaml
name: release

on:
  push:
    tags: ['v*.*.*']

permissions:
  contents: read            # 默认仍然只读

jobs:
  image:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write        # 只为了推 GHCR
      id-token: write        # 需要构建来源证明时
    steps:
      - uses: actions/checkout@<pin-到-SHA>
```

### 3.3 四条硬规则

| 规则 | 原因 |
|---|---|
| **禁用 `pull_request_target`**（需要写权限的任务尤其） | 该事件在基础分支上下文跑，且默认能拿 secrets → fork PR 直接偷密钥 |
| fork 的 PR **不给 secrets** | 普通 `pull_request` 本来就不给，别再手工 `env` 传进去 |
| 第三方 action **pin 到 commit SHA**（不是 `@v4`） | tag 可以被移动，`@v4` 等于信任一个可变引用 |
| `actions/checkout` 加 `persist-credentials: false` | 否则 token 被写进 `.git/config`，后续步骤可读到 |

---

## 4. Secrets 清单与"绝不提交 .env"

### 4.1 清单

| Secret | 用途 | 谁会读 | 怎么产生 |
|---|---|---|---|
| `KEYSTORE_BASE64` | Android 签名 keystore 的 base64 | 仅 `release.yml` | 见下方命令 |
| `KEYSTORE_PASSWORD` | keystore 口令 | 仅 `release.yml` | 你生成 keystore 时设的 |
| `KEY_ALIAS` | 签名证书别名 | 仅 `release.yml` | `keytool -list` 可见 |
| `KEY_PASSWORD` | 别名口令 | 仅 `release.yml` | 同上 |
| `FEISHU_WEBHOOK` | CI 结果推飞书（可选） | `notify` job | 见 [feishu.md](feishu.md) |
| `FEISHU_SECRET` | 飞书加签密钥（可选） | `notify` job | 同上 |

**不要放进 Secrets 的东西**：

- `GITHUB_TOKEN`：内置，推 GHCR 用它就够，**不要为此建 PAT**（PAT 是长期凭据，泄露面更大）；
- 任何模型厂商 Key（`DEEPSEEK_API_KEY` 等）：CI 不需要调模型（Benchmark 用录制 fixture）；真需要时走 environment secret 并只给 nightly；
- `SEMESTER_START`、`DATABASE_URL`（本地测试库）：不是秘密，走配置/默认值。

生成 keystore 的 base64：

```powershell
# Windows / PowerShell
[Convert]::ToBase64String([IO.File]::ReadAllBytes('release.jks')) | Set-Content keystore.b64 -NoNewline
gh secret set KEYSTORE_BASE64 < keystore.b64
Remove-Item keystore.b64, release.jks -Force   # 本地留一份到密码管理器后立刻删
```

```bash
# 或 bash
base64 -w0 release.jks | gh secret set KEYSTORE_BASE64
```

一次性设置其余：

```bash
gh secret set KEYSTORE_PASSWORD --body '<口令>'
gh secret set KEY_ALIAS         --body 'yunong'
gh secret set KEY_PASSWORD      --body '<口令>'
gh secret list                  # 复验：只显示名字与更新时间，不显示值
```

> 更稳的做法：把 release 相关 secret 放到 **Environment**（如 `production`）而不是仓库级，并给该 environment 配 required reviewers
> ——这样"推 tag"这一步也必须人工批准才能拿到签名密钥。

### 4.2 绝不提交 `.env`

`.gitignore` 必须有（这一条比什么都重要）：

```gitignore
.env
.env.*
!.env.example        # 只允许提交没有真实值的样例
*.jks
*.keystore
keystore.b64
```

三道保险：

1. **Push protection**（服务端拦截）：仓库 Settings 里开启 Secret scanning / Push protection；
   查状态：`gh api repos/<org>/yunong --jq '.security_and_analysis'`（字段在组织仓库才有，**可用性以官方文档为准**）。
2. **本地 pre-commit 拦截**（可选）：
   ```bash
   git config core.hooksPath .githooks
   # .githooks/pre-commit：命中 ^\.env 或 ^*.jks$ 就 exit 1
   ```
3. **CI 兜底**：`docs` job 里加一句"仓库内不得出现 `.env`（`.env.example` 除外）"的检查，把 `git ls-files | grep -E '(^|/)\.env$'` 作为失败条件。

**万一已经提交了**：先撤销/轮换密钥（key 一旦进过远端就等于公开），再清历史（`git filter-repo`）；
只 `git rm` 而不清历史**等于没做**——历史里还能 `git show` 出来。

---

## 5. GHCR 镜像命名与可见性

### 5.1 命名（一次定死，别改）

| 镜像 | 标签 | 说明 |
|---|---|---|
| `ghcr.io/<org>/yunong-server` | `1.0.0`、`sha-a1b2c3d`、`latest`（仅正式版） | 服务端单二进制（[02-系统设计文档 §11](../02-系统设计文档.md)：`scratch`/`distroless`） |

规则：

- **路径必须全小写**。组织名有大写时 `docker build -t` 会直接报 `repository name must be lowercase` → 在 workflow 里先转小写：
  `IMAGE=$(echo "ghcr.io/${{ github.repository_owner }}/yunong-server" | tr '[:upper:]' '[:lower:]')`；
- PR 构建**只在需要时推** `sha-<7位>`，不要推 `latest`；`latest` 只由正式 tag 产生（否则线上拉到的可能是任意 commit）；
- 只构 `linux/amd64`（1.0 单实例，见 [ADR-12](../04-ADR.md)），多架构等有实机需求再说。

### 5.2 可见性

- 包**默认继承仓库可见性**：仓库 private → 镜像是 private；
- 私有镜像在服务器上 `docker pull` 要登录，且 token 要有 `read:packages`：
  ```bash
  echo "$GHCR_PAT" | docker login ghcr.io -u <github-username> --password-stdin
  ```
  这是**部署机上唯一需要长期保存的凭据**，用 fine-grained PAT 且只给 `read:packages`；
- 要公开（开源、减少部署摩擦）：
  ```bash
  gh api -X PATCH /orgs/<org>/packages/container/yunong-server -f visibility=public
  ```
  公开前确认镜像里没有构建期注入的东西（比如 `.cargo/credentials`、`*.env`）——**公开以后历史层也能被拉下来**。

---

## 6. 发布流程（tag → release → 变更日志）

### 6.1 顺序（照做）

```bash
# 0) 前提：main 全绿，且 tag 必须打在 main 上
git switch main && git pull --ff-only
git merge-base --is-ancestor HEAD origin/main || echo "HEAD 不在 main 上，停"

# 1) 改版本号 + 写 CHANGELOG（见 §6.2），走 PR 合入
# 2) 打附注 tag（不要轻量 tag，附注 tag 才有作者/日期/说明）
git tag -a v1.0.0 -m "yunong 1.0.0：MVP + 生产级重构"
git push origin v1.0.0        # 触发 release.yml

# 3) 看流水线
gh run list --workflow=release.yml --limit 3
gh run watch

# 4) 建 Release：先 draft，人肉核对产物与文案后再发布
gh release create v1.0.0 --verify-tag --draft \
  --title "v1.0.0 · 首个正式版" \
  --notes-file docs/CHANGELOG-1.0.0.md

gh release view v1.0.0                                   # 核对
gh release edit v1.0.0 --draft=false                     # 发布
gh release upload v1.0.0 dist/*.aab dist/*.msix          # 需要时补附件
```

### 6.2 变更日志：让人看得懂"我该不该升"

`CHANGELOG.md` 用固定四段，**只写用户能感知的变化**：

```markdown
## [1.0.0] - 2026-11-15

### 新增
- 课表同步（WebView 抓取 + 端侧解析），失败时明确提示"学校页面可能已更新"
### 变更
- 对话改为流式输出（首字延迟从 ~8s 降到 ~1s）
### 修复
- 并发提问会互相覆盖历史消息
### 破坏性
- 旧 JSON 数据首次启动自动迁移到 SQLite，迁移前自动备份
```

三条纪律：

1. **PR 标题就是变更日志条目**的原文来源（squash 合并后一条 PR 一个 commit），别指望事后补写；
2. Release 正文里写清 **AGPL-3.0 源码可得性**（[ADR-14](../04-ADR.md)：开源约束服务端分发），并给出对应 tag 的源码链接；
3. `--generate-notes` 可以辅助，但**必须人工改一遍**——它会把 `chore: bump deps` 也列进去。

### 6.3 release.yml 里该有的两个守卫

- **tag 必须来自 main**：`git merge-base --is-ancestor $GITHUB_SHA origin/main` 不成立就失败；
- **可复现**：`cargo build --locked`、`flutter pub get --enforce-lockfile`，版本号从 tag 推导（`${GITHUB_REF_NAME#v}`），不要在 yml 里再写一遍版本号。

---

## 7. 邀请协作者之后：CODEOWNERS 必须跟着换

仓库还是"单人仓库"时，CODEOWNERS 里通常写的是**你自己的用户名**。一旦邀了队友：

```gitignore
# .github/CODEOWNERS（交给工程侧提交，本次不动仓库）
*                          @<org>/maintainers
/core/                     @<org>/maintainers
/server/migrations/        @<org>/maintainers
/docs/                     @<org>/maintainers
/.github/workflows/        @<org>/maintainers
```

四条提醒（每一条都真的会静默失效）：

1. **`@<org>/<team>` 里的 team 必须对该仓库有 Write 及以上权限**，否则这一行不生效（不报错，只是没人被请求评审）；
2. 只在分支保护里勾了 **Require review from Code Owners** 时，CODEOWNERS 才是强制的（§2.2 的 `require_code_owner_reviews: true`）；
3. 换组织名 / 仓库转移 / 改用户名后，`@旧名字` 会**静默失效** → 转移前后各跑一次 `gh api repos/<org>/yunong/codeowners/errors` 查错（该接口存在性以官方文档为准；没有就在 PR 里看是否出现 requested reviewers）；
4. 别把 CODEOWNERS 写成 `* @你个人`——你一休假，所有 PR 卡死。

---

## 8. 一页检查单

```
[ ] 组织 Owner 只有 1 人；队友全是 Member
[ ] Team：maintainers(maintain) / contributors(push)，成员从 Team 授予而非逐个邀请
[ ] 仓库：squash-only + auto-delete-branch + wiki 关
[ ] main 保护：PR + 1 approval + 4 个状态检查 + up-to-date + linear + 禁 force push + enforce_admins
[ ] 状态检查名与 ci.yml 的 job id 逐字对齐，且已用 check-runs 接口复验过
[ ] workflow 默认权限 = read；只有 release.yml 提 privileges（packages: write）
[ ] 禁用 pull_request_target；第三方 action pin 到 SHA
[ ] Secrets 只有 6 个以内，全部 gh secret list 可见；GITHUB_TOKEN 未另建 PAT
[ ] .gitignore 覆盖 .env / *.jks；push protection 已开
[ ] GHCR 镜像名全小写；latest 只由正式 tag 产生
[ ] 发布：附注 tag → draft release → 人肉核对 → 发布；CHANGELOG 四段式
[ ] CODEOWNERS 已从个人换成 @<org>/maintainers
```

---

## 附：与 ADR 的对应

| 本文规则 | 依据 |
|---|---|
| CI 检查 migrations（禁止建 `messages` 表） | [ADR-08](../04-ADR.md) 越界防范："云端迁移脚本里根本不建这些表" |
| docs 检查契约与实现一致 | [ADR-13](../04-ADR.md)：`shared` 是唯一契约源，OpenAPI/文档由它生成 |
| 日志/产物不得含正文与凭据 | [ADR-02](../04-ADR.md)、[02-系统设计文档 §10.2](../02-系统设计文档.md) |
| AGPL 源码可得性写进 Release | [ADR-14](../04-ADR.md) |
| 单实例部署、只构 amd64 | [ADR-12](../04-ADR.md)（闸门是进程内的，扩容前必须先改实现） |
