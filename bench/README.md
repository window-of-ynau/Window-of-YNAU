# bench · 评测门禁（issue #56 / W11）

一句话：**同一批 50 条问句，每次 PR 与基线对比；TTFT / 总耗时 / 成本 / 命中率任一退化 > 10% 就阻断合并。**

## 现在是真的门禁（v2，t162 起）

| 项 | v1（之前） | v2（现在） |
|---|---|---|
| 用例 | 12 条 smoke | **50 条**：课表 15 / 规章 10 / 知识 15 / 多轮 10 |
| 指标 | 只有 TTFT / 总耗时 / 错误率 | **五项**：首字延迟、总耗时、tokens（in/out）、估计成本、**要点命中** |
| 基线 | 自述"不是实测" | **CI 实采**（`environment` 字段记录采集环境与时间） |
| 生效方式 | 没配 `vars.BENCH_BASE_URL` 就跳过（等于从不跑） | **PR 默认执行**：自建 PG + mock 上游 + 现场编译的服务端 |
| 判定 | 只有 p95 TTFT / mean 总耗时 | TTFT P50/P95、总耗时 P50/P95、单条成本（+10%）、命中率（−10%）、错误率（+5pp） |

## 诚实边界（先读这一段）

评测跑的是 `bench/mock_upstream.py` **回放固定答案**的假上游：

- 它能证明：流式转发、usage 记账、token 统计、成本折算、错误处理、**延迟/成本的相对退化**；
- 它**不能**证明：模型回答质量。所以"要点命中率"在这里衡量的是
  **"上游给的答案有没有被完整、按序送到客户端"**，不是"模型答得对不对"；
- 换真实上游跑（`--base-url` 指到生产/staging）时，同一套指标与门禁照用，只是基线要重新采。

## 目录

| 路径 | 作用 |
|---|---|
| `bench/cases/full.jsonl` | **50 条门禁用例**（一行一个 JSON；含 question / expect / answer） |
| `bench/cases/smoke.jsonl` | 12 条冒烟用例（本地快速验证用） |
| `bench/mock_upstream.py` | 回放标准答案的 mock 上游（`openai_completions` SSE，只用标准库） |
| `bench/run.py` | 发请求、量五项指标、写结果、生成 Markdown 报告、跟基线比（只用标准库） |
| `bench/pricing.json` | 成本价目（元/百万 token） |
| `bench/baseline.json` | 基线快照（CI 实采；`environment` 写清采集环境） |
| `bench/results/` | 运行时产物（JSON + 报告），已在 .gitignore 里 |
| `bench/strengthen_expect.py` | 维护脚本：把 `expect.contains` 收敛成"只存在于答案里的事实词" |

> 为什么期望词必须是"答案里有、问题里没有"的事实词：早期版本用"周三""课"这类词，
> 连"回显问题"的兜底答案都能命中 ⇒ 命中率永远 100%，指标形同虚设（实测踩过）。

## 本地跑（不需要任何 Key）

```bash
# 1) 起 mock 上游（回放 50 条标准答案）
python bench/mock_upstream.py --port 18089 --cases bench/cases/full.jsonl &

# 2) 起服务端（三个协议都指到 mock；评测不需要邀请码）
DATABASE_URL=postgres://… \
UPSTREAM_COMPLETIONS_URL=http://127.0.0.1:18089 \
UPSTREAM_RESPONSES_URL=http://127.0.0.1:18089 \
UPSTREAM_MESSAGES_URL=http://127.0.0.1:18089 \
UPSTREAM_API_KEY=sk-bench-mock AUTH_INVITE_REQUIRED=false \
YUNONG_BIND=127.0.0.1:8080 ./target/debug/yunong-server &

# 3) 注册拿 token（评测账号）
TOKEN=$(curl -fsS -X POST http://127.0.0.1:8080/api/v1/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"bench@ynau.edu.cn","password":"bench-Passw0rd!"}' | python -c 'import json,sys;print(json.load(sys.stdin)["data"]["access_token"])')

# 4) 跑评测（对比基线 + 出报告）
python bench/run.py --cases bench/cases/full.jsonl \
  --base-url http://127.0.0.1:8080 --token "$TOKEN" \
  --baseline bench/baseline.json \
  --out bench/results/local.json --report bench/results/local.md \
  --env-note "local dev"
```

退出码：`0` 通过（或基线还是 bootstrap）／`1` 相对基线退化／`2` 参数、文件或服务不可达。

## 五项指标与判定口径

| 指标 | 定义 | 退化方向 | 阻断阈值 |
|---|---|---|---|
| 首字延迟 TTFT | 请求发出 → 第一个 `delta` 帧 | 变大 | P50 或 P95 > +10% |
| 总耗时 | 请求发出 → 流结束 | 变大 | P50 或 P95 > +10% |
| tokens | 上游 `usage` 帧的 in/out（没有就是 null，不伪造） | 变大 | 单条成本 > +10% |
| 估计成本 | 按 `bench/pricing.json` 折算成"分" | 变大 | 同上一行 |
| 要点命中 | 回答是否包含 `expect.contains` 的全部词 | **变小** | 相对下跌 > 10%（如 100%→<90%） |
| 错误率 | 失败用例占比 | 变大 | > 基线 + 5pp |

阈值写死在 `bench/run.py` 的 `REGRESSION_TOLERANCE` 与 `compare_with_baseline()` 里，
**不要靠在 baseline.json 里改数字来放宽**（基线只放"量到的值"）。

### 为什么还有"绝对噪声下限"

mock 上游的延迟是**个位数毫秒**（CI 实测 p50 2.6ms / p95 3.2ms）。这个量级上 +10% 只有 0.3ms，
一次 CI 的调度抖动就能超过 ⇒ 门禁会变成随机器心情红绿。
所以判定是「**相对 >10% 且绝对增量超过下限**」才阻断：延迟 5ms、成本 0.01 分
（`NOISE_FLOOR_MS` / `NOISE_FLOOR_CENTS`）。真实上游是几百毫秒量级，5ms 的噪声下限在那里
永远不生效，不会掩盖真退化；命中率与错误率不受此影响。

## 首次采集 / 更新基线

基线必须是**实测**，且标注采集环境：

```bash
python bench/run.py --cases bench/cases/full.jsonl --base-url … --token … \
  --update-baseline bench/baseline.json --env-note "GitHub Actions ubuntu-latest"
```

写出后**删掉 `bootstrap` 字段**（有它时门禁只记录不阻断，用于首次采集窗口）。
换机器/换上游/换价目都算"环境变更"，需要重新采基线并在 PR 里说明原因。

## CI 里的耗时

`bench.yml` 从"默认跳过"改成**默认执行**（PR / 每日 / 手动）：

| 步骤 | 约耗时 |
|---|---|
| checkout + 装 Python / Rust 工具链 | ~20s |
| `cargo build -p yunong-server --bin yunong-server`（有 rust-cache） | 1.5–2.5 min |
| 应用迁移（PG 服务） | ~5s |
| 起 mock 上游 + 起服务端 + 注册 | ~5s |
| 50 条评测 + 基线对比 + 报告 | ~2–5s |
| **合计** | **约 2.5–4 min** |

## p95 在 mock 场景**不阻断**（2026-11 决定），以及恢复它的条件

| 指标 | mock 场景（现在） | 接真实上游后 |
|---|---|---|
| p50 延迟（TTFT / 总耗时） | **阻断**（> 基线 +10%，带 10ms 噪声下限） | 阻断 |
| **p95 延迟** | **只观测**（写进 JSON 与报告，> 基线 5 倍标 ⚠️） | **恢复为阻断**（+10%） |
| 命中率 | **阻断**（零容忍，mock 回放固定答案） | 阻断 |
| 错误率 | **阻断**（零容忍） | 阻断 |
| 单条成本 | **阻断**（计算得出，不是测量） | 阻断 |

**为什么 mock 场景不阻断 p95**：mock 上游的响应是 1–2ms，而 CI runner 的偶发抖动是 10–40ms
——**抖动比被测信号大一个数量级**，这个指标在该环境下没有测量能力，卡它只会持续误报。
实测证据：某轮 9 条用例同时出现 10–40ms 停顿、重测全部回到 1.4–1.6ms，而中位数一直在 1.5ms
（管线没退化），p95 却被判 +84%。这不是阈值问题，是测量能力问题；所以我们**没有降阈值**。

**恢复 p95 阻断的条件**（`--p95-gate` 的语义，默认 `auto`）：

1. **自动**：基线 `p95_ttft_ms ≥ 100ms`（真实上游是几百毫秒量级，抖动占比小）⇒ 自动阻断；
2. **手动**：`--p95-gate on` 强制阻断、`--p95-gate off` 强制只观测；
3. 接真实上游时请**重新采基线**（`--update-baseline`，`environment` 写明真实端点），
   并在 PR 里说明"p95 已恢复为阻断项"。

**ENV_UNSTABLE**：一次 run 里触发尖刺重测的用例 **≥5 条且 ≥10%** ⇒ 报告标 `ENV_UNSTABLE`，
视为整轮环境抖动，只按 p50 / 命中率 / 错误率 / 成本判定（p95 本来就不阻断，这一标记是
给"为什么这轮数字不可信"的说明）。**不再**因为重测条数多就直接判失败（早期规则已退役：
它会把 captain 实测的 9 条那次直接判红）。

## 门禁误报的三种典型形状（看到红色先对照这张表，不要先"重跑一次试试"）

| 形状 | 典型数字 | 含义 | 处置 |
|---|---|---|---|
| **① p50 正常、p95 爆、命中率 100%** | `p50=1.8ms` 而 `p95=42.4ms`；基线 3.2ms | **环境瞬停**（runner 抢占 / PG checkpoint / mock 的 GC 停顿）：少数请求被拖慢，整轮没变慢 | p95 在 mock 场景**不阻断**（只看 p50/命中率/错误率/成本，本轮应 PASS）；报告里有"最慢用例"与"重测记录"；触发重测 ≥5 条会标 `ENV_UNSTABLE` |
| **② p50 与 p95 同时上抬** | `p50 13.2ms`（基线 2.6）、`p95 33.3ms` | **真变慢**：所有请求一起变慢 | **阻断，去查代码**；这种情况门禁不会做重测（"整轮变慢不做尖刺重测"） |
| **③ 命中率下降 / 错误率上升** | `hit_rate < 90%` 或 `error_rate > 0` | **mock 回放或服务端逻辑问题**（答案没送到、流被截断、上游报错） | **阻断，零容忍**，不放宽、不重测 |

历史上两次典型误报都属形状 ①：run `42337511062`（`p95 21.1ms`，同一 commit 重跑即绿）
与 run `42367996754`（`p95 42.4ms`，rerun 后 `p95 1.9ms`；经查 `crates/` 与 `bench/` 在两个
commit 之间完全一致，且三段 socket 都关了 Nagle，**与 delayed-ACK 无关**）。

## 为什么这样测（改之前先读：这几条是踩坑换来的）

1. **必须先预热（`--warmup 3`）**：第一个请求要建连、走 DNS、冷缓存，它天然慢。
   不预热的话这条毛刺会直接进 P95 —— 那测的是"冷启动"，不是"稳态退化"。
2. **每条用例重复 3 次取中位数（`--repeats 3`）**，P50/P95 在"每条的中位数"上算。
   实测教训（run 42337511062）：mock 场景 TTFT 只有 2–3ms，一条 20ms 的调度毛刺就能把
   **P95 从 3.2ms 拉到 21.1ms（+559%）**，而同一轮的 **P50 反而更好**（2.1ms vs 2.6ms）。
   中位数把"偶发一次"过滤掉，只留"稳定变慢"。
3. **噪声下限 10ms（`NOISE_FLOOR_MS`）**：个位数毫秒的基线上，+10% 只有 0.3ms，
   任何 CI 抖动都会误杀。而真实上游是几百毫秒量级，10ms ≈ 2–5%，不会掩盖真退化。
4. **报告里带"最慢 3 条用例"**：抖动时先看它们，能把"环境毛刺"和"某条用例真的变慢"分开，
   不用再靠猜。
5. **mock 必须用 chunked 分帧，不许"回完就关连接"**：SSE 没有 `Content-Length`，只有 chunked
   或"关连接"两种合法终止方式。若回完 `[DONE]` 直接关连接，服务端的连接池会以为连接可复用，
   下一次复用时才发现在对端已关闭，只能重建连接 —— 实测表现为**每隔一条用例冒一个 20–100ms**
   的尖刺（run 42338912066：t02 31.8ms / t06 101.6ms，命中率却完全正常）。
   改了 chunked 之后同一台机器连跑三次，P95 稳定在 25.8 / 26.3 / 26.3ms。
6. **每条重复 5 次，并把每次的原始值写进结果（D1）**：只有中位数时无法区分"5 次里 1 次慢"和
   "5 次都慢" —— run `42367996754` 就只能靠事后重跑才查清。原始值是诊断证据，不是冗余数据。
7. **孤立尖刺重测一次，但必须留痕（D2）**：触发条件是「该用例中位数 > 本轮中位数的 5 倍」
   **且**「本轮 P50 仍在基线 +10% 内」；重测的「首次 / 重测 / 采用」三项都进报告，
   不允许悄悄取小的；**整轮变慢不重测**；一次 run 里有 **≥3 条**触发重测 ⇒ 直接判环境异常（请重跑）。
8. **不要为了提高 RTF 把这些删掉**：删 `--repeats` 省下的是几十秒，换来的是随机红绿的假门禁
   —— 一个会误报的门禁会训练所有人"红了先重跑"，最后真退化也被当成抖动放过。

> 本地跑（Windows/开发机）会**因为环境差异被判退化**，这是预期：基线是在
> GitHub Actions ubuntu-latest 上采的。本地只用来看 `summary` 与报告，
> 不要拿本地数字去比 CI 基线，也不要因此改 `bench/baseline.json`。

## 已知限制

1. mock 回放的是固定答案 ⇒ 只测管线，不测模型质量（见上面"诚实边界"）；
2. 延迟绝对值与 runner 噪声强相关：所以基线**必须**在 CI runner 上采，别拿本地数字当门禁；
3. `T2` 类并发/长上下文场景不在本集内（由 `chat_stream` 的集成用例覆盖行为，不覆盖延迟曲线）。

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
