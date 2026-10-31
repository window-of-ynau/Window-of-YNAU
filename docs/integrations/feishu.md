# 云农之窗 · 飞书通知接入（CI/CD 结果推到你自己的企业）

| 项 | 内容 |
|---|---|
| 目标 | GitHub Actions 跑完，把**结论 / 分支 / commit / 耗时 / 运行链接**推到你自己飞书企业的群里 |
| 形态 | 群机器人 + 自定义 webhook（不需要企业自建应用、不需要审批、不需要开放平台权限） |
| 依赖 | 仓库 secret `FEISHU_WEBHOOK`（可选再加 `FEISHU_SECRET` 做加签） |
| 边界 | **只发元数据**（状态/分支/哈希/耗时/链接）。CI 日志正文、失败堆栈、用户数据、`DATABASE_URL`、任何 Key 一律不发（[ADR-02](../04-ADR.md) 与 [02-系统设计文档 §10.2](../02-系统设计文档.md) 的"日志禁止内容"同样适用于外部 IM） |

> 飞书后台的具体按钮文案与位置会随版本变化，本文只写**路径层级**；细节**以官方文档为准**。

---

## 1. 建群 + 加自定义机器人（拿到 webhook）

### 1.1 建一个专门的通知群

不要用日常聊天群：
- CI 噪音会淹没正常讨论；
- 出问题时能整群静音/退订；
- webhook 泄露时的爆炸半径就只有这一群。

建议群名：`云农之窗 · CI`（或 `yunong-builds`）。

### 1.2 路径

```
群设置 → 群机器人 → 添加自定义机器人
```

- 添加后飞书会给你一个形如
  `https://open.feishu.cn/open-apis/bot/v2/hook/<一串 token>` 的 **webhook 地址**；
- 这个地址**本身就是凭据**（谁拿到谁就能往群里发消息），按 secret 对待；
- 同一页面通常可以：
  - 起个名字/头像（如"云农之窗 CI"）；
  - **安全设置**里选"签名校验"（推荐，见 §5）或"自定义关键词"；
  - 若你开了**自定义关键词**，消息文本里必须出现该关键词，否则会被拒 → 那就把关键词写进卡片标题（如 `云农之窗`）。

### 1.3 存进 GitHub Secrets

```bash
gh secret set FEISHU_WEBHOOK --body 'https://open.feishu.cn/open-apis/bot/v2/hook/xxxxxxxx'
gh secret set FEISHU_SECRET  --body '<签名校验里显示的那个 secret>'   # 没开加签就不要设
gh secret list
```

### 1.4 先手工验证一次（别等 CI 才发现）

```bash
curl -sS -X POST "$FEISHU_WEBHOOK" \
  -H 'Content-Type: application/json' \
  -d '{"msg_type":"text","content":{"text":"云农之窗 · webhook 连通性测试"}}'
```

**判成功的唯一标准是响应体 `code == 0`**。飞书在业务失败时通常仍回 HTTP 200，
所以 `curl -f` 或看退出码**都不够**：

```json
{"code":0,"msg":"success","data":{}}
```

---

## 2. 消息卡片 JSON（`msg_type: interactive`，最简可用）

结构三件套：**header（标题+颜色）→ elements[div+fields（两列数据）] → elements[action（按钮）]**。

```json
{
  "msg_type": "interactive",
  "card": {
    "config": { "wide_screen_mode": true },
    "header": {
      "template": "green",
      "title": { "tag": "plain_text", "content": "云农之窗 · CI 通过" }
    },
    "elements": [
      {
        "tag": "div",
        "fields": [
          { "is_short": true, "text": { "tag": "lark_md", "content": "**分支**\n`main`" } },
          { "is_short": true, "text": { "tag": "lark_md", "content": "**提交**\n`a1b2c3d`" } },
          { "is_short": true, "text": { "tag": "lark_md", "content": "**耗时**\n3m 42s" } },
          { "is_short": true, "text": { "tag": "lark_md", "content": "**触发**\n`push` · zhang" } }
        ]
      },
      { "tag": "hr" },
      {
        "tag": "action",
        "actions": [
          {
            "tag": "button",
            "text": { "tag": "plain_text", "content": "查看运行" },
            "url": "https://github.com/<org>/yunong/actions/runs/123456789",
            "type": "primary"
          }
        ]
      }
    ]
  }
}
```

要点：

| 元素 | 规则 |
|---|---|
| `header.template` | 颜色枚举，实操只用到三个：`green`（通过）/ `red`（失败）/ `grey`（取消、跳过） |
| `fields[].is_short` | `true` = 两列并排（一行两个）；`false` = 独占一行。**成对出现才是两列**，奇数个会留白 |
| `lark_md` | 支持 `**粗体**`、反引号、`<at id=ou_xxx></at>` @人（1.0 不需要 @人，刷屏） |
| `url` | 必须是绝对 https 地址；私有仓库的链接只有群内有权限的人点得开（外人点开是 404，不是泄露） |
| 卡片 2.0 | 若企业已迁到新版卡片（`schema: "2.0"` + `body.elements`），本文的 v1 结构仍兼容；**升级前以官方文档为准** |

---

## 3. `curl` 示例（本地调试用）

把 §2 的 JSON 存成 `ci-card.json`，然后：

```bash
curl -sS -X POST "$FEISHU_WEBHOOK" \
  -H 'Content-Type: application/json' \
  --data-binary @ci-card.json
```

排查三件套：

```bash
# ① 看清返回体（不要只看 curl 退出码）
curl -sS -o resp.json -w '%{http_code}\n' -X POST "$FEISHU_WEBHOOK" \
  -H 'Content-Type: application/json' --data-binary @ci-card.json
cat resp.json

# ② 校验自己拼的 JSON 是否合法（90% 的"发不出去"都是拼错了 JSON）
jq . ci-card.json > /dev/null && echo "JSON OK"
```

| 现象 | 先查什么 |
|---|---|
| `code` 非 0 | 读 `msg`：写错 URL / 机器人被移出群 / 关键词不匹配 / 触发频率上限 |
| HTTP 200 但群里没消息 | 是不是发错群了（同一个企业可能有多个同名群） |
| 只在某些运行失败 | 转义问题：commit message 里有 `"`、换行、反引号 → **一律用 `jq --arg` 拼，不要字符串拼接** |

---

## 4. GitHub Actions 里怎么发（`if: always()`）

### 4.1 放在哪：一个独立的 `notify` job

**不要**在每个 job 末尾各发一条（4 个 job = 4 条消息，会刷屏，而且你看不出整体结论）。
用 `needs` 聚合成**一条**消息：

```yaml
name: ci

on:
  push:
    branches: [main, dev]
  pull_request:

permissions:
  contents: read

jobs:
  # rust / flutter / docs / migrations 四个 job 的真实内容在 ci.yml 里，
  # 这里只保留 job id —— 它们决定 needs 与状态检查名。
  rust:
  flutter:
  docs:
  migrations:

  notify:
    name: notify-feishu
    needs: [rust, flutter, docs, migrations]
    if: always()                 # ← 关键：前面失败/跳过也照发
    runs-on: ubuntu-latest
    permissions:
      contents: read
      actions: read              # 读本次 run 的开始时间（算耗时）
    steps:
      - name: 推送飞书卡片
        continue-on-error: true  # 通知失败不该把流水线判红
        env:
          FEISHU_WEBHOOK: ${{ secrets.FEISHU_WEBHOOK }}
          FEISHU_SECRET:  ${{ secrets.FEISHU_SECRET }}
          GH_TOKEN:       ${{ secrets.GITHUB_TOKEN }}   # 只用来读 run 元数据
          R_RUST:    ${{ needs.rust.result }}
          R_FLUTTER: ${{ needs.flutter.result }}
          R_DOCS:    ${{ needs.docs.result }}
          R_MIG:     ${{ needs.migrations.result }}
        run: |
          set -euo pipefail
          echo "::add-mask::$FEISHU_WEBHOOK"     # 日志里出现也要打码
          [[ -n "${FEISHU_SECRET:-}" ]] && echo "::add-mask::$FEISHU_SECRET"

          # ---- 1) 结论：任一失败即失败；全 skipped/cancelled 视为未完成 ----
          conclusion=success
          for r in "$R_RUST" "$R_FLUTTER" "$R_DOCS" "$R_MIG"; do
            case "$r" in
              failure) conclusion=failure; break ;;
              cancelled) conclusion=cancelled ;;
              skipped)   [ "$conclusion" = success ] && conclusion=skipped ;;
            esac
          done
          case "$conclusion" in
            success)   title="云农之窗 · CI 通过"; template=green ;;
            cancelled) title="云农之窗 · 已取消"; template=grey ;;
            *)         title="云农之窗 · CI 失败"; template=red ;;
          esac

          # ---- 2) 元数据 ----
          branch="${GITHUB_REF_NAME}"
          sha_short="${GITHUB_SHA:0:7}"
          actor="${GITHUB_ACTOR}"
          event="${GITHUB_EVENT_NAME}"
          run_url="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
          jobs="rust=${R_RUST} flutter=${R_FLUTTER} docs=${R_DOCS} migrations=${R_MIG}"

          # ---- 3) 耗时：从本次 run 的 started_at 算（notify job 自己很短，不能用自己的时间）----
          started=$(gh api "repos/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}" \
                      --jq '.run_started_at' 2>/dev/null || echo "")
          duration="未知"
          if [[ -n "$started" ]]; then
            s=$(date -d "$started" +%s); now=$(date +%s); d=$(( now - s )); (( d < 0 )) && d=0
            duration="$(( d / 60 ))m $(( d % 60 ))s"
          fi

          # ---- 4) 用 jq 组装（绝不用字符串拼接，避免非法 JSON）----
          card=$(jq -n \
            --arg title "$title" --arg tpl "$template" \
            --arg branch "$branch" --arg sha "$sha_short" \
            --arg duration "$duration" --arg actor "$actor" \
            --arg event "$event" --arg jobs "$jobs" --arg url "$run_url" \
            '{
               msg_type: "interactive",
               card: {
                 config: { wide_screen_mode: true },
                 header: { template: $tpl, title: { tag: "plain_text", content: $title } },
                 elements: [
                   { tag: "div", fields: [
                       { is_short: true, text: { tag: "lark_md", content: ("**分支**\n`" + $branch + "`") } },
                       { is_short: true, text: { tag: "lark_md", content: ("**提交**\n`" + $sha + "`") } },
                       { is_short: true, text: { tag: "lark_md", content: ("**耗时**\n" + $duration) } },
                       { is_short: true, text: { tag: "lark_md", content: ("**触发**\n`" + $event + "` · " + $actor) } },
                       { is_short: false, text: { tag: "lark_md", content: ("**各作业**\n`" + $jobs + "`") } }
                     ] },
                   { tag: "hr" },
                   { tag: "action", actions: [
                       { tag: "button", type: "primary",
                         text: { tag: "plain_text", content: "查看运行" }, url: $url }
                     ] }
                 ]
               }
             }')

          # ---- 5) 可选：加签（开了"签名校验"才需要）----
          payload="$card"
          if [[ -n "${FEISHU_SECRET:-}" ]]; then
            ts=$(date +%s)
            sign=$(printf '%s\n%s' "$ts" "$FEISHU_SECRET" \
                   | openssl dgst -sha256 -hmac "$FEISHU_SECRET" -binary | base64)
            payload=$(jq -n --argjson c "$card" --arg ts "$ts" --arg sign "$sign" \
                        '$c + { timestamp: $ts, sign: $sign }')
          fi

          # ---- 6) 发送并校验 code == 0 ----
          resp=$(curl -sS -X POST "$FEISHU_WEBHOOK" \
                   -H 'Content-Type: application/json' --data-binary "$payload")
          echo "$resp"
          if [[ "$(jq -r '.code // -1' <<< "$resp")" != "0" ]]; then
            echo "::warning::飞书通知发送失败：$(jq -r '.msg // "unknown"' <<< "$resp")"
          fi
```

### 4.2 四个必须知道的坑

| 坑 | 说明 |
|---|---|
| `if: always()` 不等于"任何情况都执行" | 整个 workflow **被取消**时，job 可能根本不会启动。要严格覆盖"取消"场景，就用另一个 workflow 监听 `workflow_run` 事件来发通知（**行为细节以官方文档为准**）。被取消一次没消息，不算大事故；漏发失败消息才算。 |
| 不要用自己的 job 计时 | `notify` job 只有几秒，必须从 run 元数据取 `run_started_at`（见代码 3 段） |
| `continue-on-error: true` 必须加 | webhook 失效不该把 CI 判红，否则分支保护永远拦着 PR |
| 分支保护不需要它 | `notify` **不要**加进 [github.md](github.md) §2 的 `contexts`：通知挂了不该挡合并 |

### 4.3 顺便发到 PR 评论？（可选，1.0 不做）

`gh pr comment` 也能贴状态摘要，但那需要 `pull-requests: write` 权限（违反 [github.md §3](github.md) 的最小权限）。
1.0 用飞书群足够；等有人真的抱怨"PR 里看不到结论"再加。

---

## 5. 安全：webhook 泄露 = 谁都能往你群里刷消息

| 风险 | 后果 | 处置 |
|---|---|---|
| webhook 进了仓库/日志 | 陌生人可无限发消息：刷屏、发钓鱼链接、冒充 CI 结论诱导点链接 | 只放 `secrets`；`::add-mask::`；**绝不 `echo` 到日志**；`.env` 不进仓库 |
| fork PR 偷 secret | 攻击者在 PR 里改 workflow 把 webhook 打到自己的服务器 | 只用 `pull_request`（默认不给 secrets），**禁用 `pull_request_target`** |
| 群里任何人都能用这个机器人 | 内部风险：有人手动发假"CI 通过" | 机器人**只进专用群**（`云农之窗 · CI`），群里只放需要看构建的人；日常群不装 |
| 消息内容被转发出去 | 元数据里混进敏感值 | 卡片只放分支/哈希/耗时/链接；**不放** commit message 全文、失败日志片段、环境变量 |

**建议开启签名校验**（飞书机器人安全设置里）：

- 开加签后，请求需带 `timestamp`（秒）与 `sign`：
  `sign = base64(HMAC-SHA256(key = secret, msg = timestamp + "\n" + secret))`
  （算法来自飞书自定义机器人文档，**以官方文档为准确认**）；
- 代码见 §4.1 第 5 段；`FEISHU_SECRET` 只放 secret；
- 加签**不能**防"webhook 已泄露"（泄露者同样有 secret 的话就都能伪造），它能防的是"只拿到 webhook 但没拿到 secret"；
- 因此真正有效的第一道防线仍是：**webhook 不出现在任何可公开的地方** + 机器人只在专用群。

**轮换（泄露后的 5 步）**：

1. 在飞书里**删掉该机器人并重新添加**（旧 webhook 立即失效）——这是最干脆的失效手段；
2. `gh secret set FEISHU_WEBHOOK --body '<新地址>'`（`FEISHU_SECRET` 一并换）；
3. 重跑一次 workflow 验证（`gh workflow run ci.yml`）；
4. 清历史：webhook 若曾进过仓库，`git rm` **不够**，必须清历史（`git filter-repo`）并认为它已公开；
5. 记一笔：`docs/` 里不需要写"我们轮换过"，但要在群里说一声（有人可能在用旧地址做本地调试）。

---

## 6. 进阶（一句话，1.0 不做）

如果以后要看"哪个版本、什么时候、由谁发布"的看板，可以把发布记录写进**飞书多维表格**（tag、日期、commit、变更摘要、产物链接各一列），由 `release.yml` 追加一行；
但 1.0 用 `CHANGELOG.md` + GitHub Release 就够，别提前引入第二套数据源。

---

## 7. 落地检查单

```
[ ] 建了专用群（不是日常聊天群），只装这一个机器人
[ ] webhook 存进 FEISHU_WEBHOOK，未出现在任何文件/日志里
[ ] curl 直连验证过 code == 0
[ ] notify job 用 needs 聚合，一条消息；if: always()；continue-on-error: true
[ ] 卡片 = 标题 + 4 个两列字段 + 1 个按钮，颜色按结论切换
[ ] 耗时取自 run_started_at，不是 notify job 自己的时长
[ ] 内容只有 分支/短哈希/耗时/触发/链接 + 各作业结果（无日志正文、无密钥）
[ ] 开了签名校验（对应 FEISHU_SECRET），且轮换流程已写进团队约定
[ ] notify 没有加进 main 分支保护的状态检查
```

---

## 附：与 ADR 的对应

| 本文规则 | 依据 |
|---|---|
| 只发元数据、不发日志正文与正文片段 | [ADR-02](../04-ADR.md)（云端不落正文）＋[02-系统设计文档 §10.2](../02-系统设计文档.md)（日志禁止项） |
| 通知失败不阻塞合并 | [ADR-06](../04-ADR.md) 同款原则：辅助系统超时不能牺牲主流程可用性 |
| webhook 按凭据对待、可轮换 | [ADR-04](../04-ADR.md)（凭据最小暴露面）的同款推理，延伸到 CI 凭据 |
