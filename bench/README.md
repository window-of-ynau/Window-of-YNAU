# bench · 重构效果的裁判

一句话：**同一批真实问句，重构前后各跑一遍；只盯"变慢、变贵、变错"，不重做正确性测试。**

产品需求里写的是固定 50 条问句集、每次 PR 与基线对比、退化超 10% 阻断合并。本目录先落最小可跑集：12 条 smoke + 一套标准化的指标与判定口径。用例集可以长大，口径不要变。

## 目录

| 路径 | 作用 |
|---|---|
| `bench/cases/smoke.jsonl` | 12 条冒烟用例，一行一个 JSON |
| `bench/run.py` | 发请求、量指标、写结果、跟基线比（只用标准库） |
| `bench/baseline.json` | 基线快照；**当前是占位值，首次真实跑通后整体替换** |
| `bench/out/` | 运行时产物（时间戳 JSON），不要提交 |

## 怎么跑

前置：服务端已起、`/readyz` 正常、上游模型有 Key。基准会真实消耗套餐额度，**不要在生产环境跑**。

```bash
# 1. 起服务（或按 deploy/README.md 用 compose）
cargo run -p yunong-server

# 2. 跑冒烟，结果落到 bench/out/
python bench/run.py --cases bench/cases/smoke.jsonl

# 3. 只跑单条、或临时换个地址/超时
python bench/run.py --base-url http://127.0.0.1:8080 --timeout 30

# 4. 服务开了鉴权时补一个 token（本地免鉴权不用传）
python bench/run.py --token "$YUNONG_TOKEN"
```

参数与默认值：

| 参数 | 默认 | 说明 |
|---|---|---|
| `--base-url` | `http://127.0.0.1:8080` | 只填 origin，脚本自己拼 `/api/v1/chat/stream` |
| `--cases` | `bench/cases/smoke.jsonl` | JSONL 用例文件 |
| `--baseline` | `bench/baseline.json` | 基线；文件不存在则只写结果、退出 0 |
| `--out` | `bench/out/bench-<UTC>.json` | 结果 JSON |
| `--timeout` | `60` | 单条用例超时（秒） |
| `--token` | 无 | 可选 `Authorization: Bearer <token>` |

退出码：`0` 无退化（或无基线）；`1` 相对基线退化；`2` 参数/用例文件/基线文件不合法，或服务不可达（不会抛栈）。开跑前会先 `GET /healthz` 只判连通：连不上直接 `2`，不会让 12 条用例各自空等到超时；`/healthz` 返回 404/500 说明服务进程在，按可达处理，交给用例暴露问题。

## 用例分四类，外加一类失败降级

| 类别 | `tags` | 条数 | 考察点 |
|---|---|---|---|
| 课表 | `timetable` | 4 | 时间/周次/星期这类问法能触发 `get_courses`，不把整张课表塞进上下文 |
| 规章 | `regulation` | 2 | 问答改走 RAG，回答基于入库文档，不靠模型记忆编校规 |
| 知识 | `knowledge` | 2 | 公开知识检索命中；回答应带 `[K:doc#chunk]` 引用 |
| 多轮 | `multiturn` | 2 | "那周五呢""上面那些课"这类追问措辞下服务仍正常出流、不变慢 |
| 工具失败降级 | `tool_failure` | 2 | 数据拿不到/同步失败时有兜底话术，不崩、不瞎编课表 |

注意一个当前边界：`run.py` 每条用例只发**一次**请求、`messages` 只有当前这一问，**不带历史、`tools` 为空**（服务端从不执行工具，工具由端侧执行）。所以：

- 多轮用例验证的是"追问措辞下的服务端行为与延迟"，不是完整多轮链路；完整链路要端侧回放（客户端装历史后再发，同一 `turn_id`、`round+1`）。
- `expect.tool` 只作为端侧回放/人工核对的期望，`run.py` **不据它判失败**（请求里没有工具声明）。
- `expect.contains` 只做软提示：命中打 `expect: ok`，未命中打 `expect: MISS(...)`，**不进 `ok`、不进错误率**。这样模型措辞波动不会制造假红线；真要看回答质量，走下面的人工抽检。

## 指标定义

| 指标 | 定义 | 在结果里的字段 |
|---|---|---|
| TTFT | 从发出请求到收到第一个 `event: delta` 的毫秒数（`meta`/`tool_call`/`ping` 不算） | `cases[].ttft_ms`，汇总 `p50_ttft_ms`、`p95_ttft_ms` |
| 总耗时 | 从发出请求到 `done`（或 `error`/超时/连接结束）的毫秒数，含 TTFT | `cases[].total_ms`，汇总 `mean_total_ms` |
| token | `event: usage` 里的 `tokens_in` / `tokens_out`；上游没给就记 `null`，不估算 | `cases[].tokens_in` / `tokens_out` |
| 引用正确率 | 回答带 `[K:doc#chunk]` 引用、且被引 chunk 确实支撑结论的比例。目标 ≥ 80%，编造来源必须为 0。`run.py` 不自动算，按 `expect.contains` 抽查 + 人工核对原文 | 人工记录 |
| 单轮成本 | 每次请求的估算成本（分）：`tokens_in/1e6 × 输入价 + tokens_out/1e6 × 输出价`，价目在 `run.py` 顶部常量 | `summary.est_cost_per_case_cents` |
| 错误率 | `ok=false` 的用例占比。`ok=true` 要求收到 `done` 且全程无 `event: error` | `summary.error_rate` |

> 成本只用于趋势：套餐按**时间授权**、不做 token 记账，服务端只对 1% 请求做 token 抽样。`usage` 里的 token 也是抽样/估算口径，别当账单。

判失败（`exit 1`）的条件，满足其一即可：

1. `p95_ttft_ms` 比基线高 **> 10%**；
2. `mean_total_ms` 比基线高 **> 10%**；
3. `error_rate` 比基线高 **> 5 个百分点**（例：0% → 8.3%）。

## 怎么加用例

在 `bench/cases/smoke.jsonl` 追加一行，字段：

```json
{"id":"c13","tags":["timetable"],"question":"周五第 3 节在哪上","expect":{"tool":"get_courses","contains":["课"]}}
```

规矩：

1. `id` 唯一、稳定，不要复用已删除用例的编号；`question` 必须是学生真会问的话（自然口语），不要为了测试编造政策、老师、教室、时间。
2. `tags` 从 `timetable` / `regulation` / `knowledge` / `multiturn` / `tool_failure` 里选，一条一个主类，方便分类统计。
3. `expect.contains` 只放**无害的通用词**：`课`、`节`、`未找到`、`规定`、`图书馆`。不要写具体政策细节（如"提前 3 天""需辅导员签字"）——写进去就等于把未验证的业务规则固化成断言。
4. `expect.tool` 只写端侧真实注册的工具名；目前确定的是 `get_courses`。新增工具名要先去端侧工具注册表确认，别猜。
5. 跑一遍 `python scripts/check_docs.py bench`，确认文档没被写坏（可选但推荐）。

## 怎么更新 baseline

**只有在确认"这次变化是合理的"之后才更新。** 基线是裁判，不是记分牌；为了让 CI 变绿而抬高基线，等于把闸门拆了。

1. 服务健康时完整跑一遍 12 条，确认 `cases` 全部 `ok=true`、`error_rate` 为 0；
2. 打开 `bench/out/bench-<UTC>.json`，核对 `summary` 数字与那次日志一致；
3. 把 `summary` 整体替换进 `bench/baseline.json`，`version` +1，`_comment` 改成这次跑通的日期/commit（保留"占位"以外的新说明）；
4. 同一个 PR 里说明：为什么退化（或改善）是预期的；
5. 不要手改平滑、取整或删字段——`run.py` 按字段名读。

一次性替换的命令示例（PowerShell）：

```powershell
$r = Get-Content bench/out/bench-20260101T000000Z.json -Raw | ConvertFrom-Json
$r.summary | ConvertTo-Json
# 人工确认后，把上面的 summary 粘进 bench/baseline.json
```

## 已知限制

- 每条用例串行执行，避免触发服务端"单用户 1 个在途流"的 429；也不模拟多设备。
- TTFT 依赖本机到服务端、服务端到上游两段网络；CI 与本地数字不可直接混比，基线要按环境各存一份。
- 只有 `delta` 才算首字；只出 `tool_call` 的轮次 `ttft_ms` 为 `null`，不计入 p50/p95。
- `bench/out/` 建议加进 `.gitignore`（仓库维护者决定，本目录不代改）。
- 部署侧看 [deploy/README.md](../deploy/README.md)。
