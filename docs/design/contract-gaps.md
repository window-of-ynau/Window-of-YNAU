# 契约缺口登记（`crates/shared` 待改）

> 来源：`agent/protocols` 在实现三协议适配时，发现**内部契约无法表达某些上游语义**。
> 这是并行开发最有价值的产出之一：写实现的人才会知道契约缺什么。
>
> 处理原则：**先登记、再改契约、最后改实现**。改 `crates/shared` 属于单写者变更，必须在这里留档，
> 否则三个 agent 各自"就地变通"会让契约名存实亡。

| # | 缺口 | 影响 | 建议改法 | 优先级 |
|---|---|---|---|---|
| C1 | `ChatMessage` 没有 `tool_calls` 字段，assistant 的工具调用只能塞进 `content` | 模型"边说边调工具"无法表达；多工具并行调用无法表达 | `ChatMessage` 增加 `tool_calls: Vec<ToolCallRef>`（`id`/`name`/`arguments`），`content` 回归纯文本 | **P0** — ✅ W2b 已解决（[ADR-16](../04-ADR.md)） |
| C2 | `ChatRequest` 没有 `temperature` | 文档 §9 写了但契约里没有；三协议实现一律不发，行为不可调 | 增加 `temperature: Option<f32>`，缺省不发 | P1 |
| C3 | 工具结果没有 `ok`/`is_error` | Anthropic `tool_result.is_error` 丢失；模型看不出"工具失败了" | `ChatMessage` 增加 `tool_ok: Option<bool>` | P1 |
| C4 | Completions 的 `tool` 消息与 Responses 的 `function_call_output` **没有 name 位置** | 工具名只能靠 `tool_call_id` 反查；上游若丢 id 就对不上 | 内部保留 `name`（已有字段），编码时按协议能力尽力携带，并在日志里记录丢失 | P2（协议限制，不可消除） |
| C5 | Anthropic `tool_use.input` 必须是 JSON 对象 | 上游给的是字符串参数时非法 JSON 只能降级 `{}` | 契约层保证 `arguments` 永远是**合法 JSON 值**；端侧执行前校验 schema | P1 |
| C6 | usage 与结束原因经常在**同一个事件**里（Anthropic `message_delta`、Responses `response.completed`） | `parse_event` 一次只能返回一个 `Chunk`，二选一必丢数据 | 改为 `parse_events(...) -> Vec<Chunk>`，允许一事件多 chunk | **P0** — ✅ W2b 已解决（[ADR-16](../04-ADR.md)） |
| C7 | 无状态解析：工具参数分片（`arguments.delta`）无法跨事件聚合 | 长参数的工具调用会被截断 | 由 `gateway` 持有聚合状态，`parse_events` 保持纯函数并在注释声明 | P1 — **定址已由 C9 解决**（分片能归到正确的调用），跨事件聚合仍需 W3 |
| C8 | `Meta`/`Ping` 不由适配层产出 | 若客户端依赖 `Meta` 判断通道，缺失会出错 | 明确由 gateway 合成；适配层只产出 `Delta/ToolCall/Usage/Done/Error` | ✅ W2b 已解决（`ChunkAssembler` 统一注入） |
| C9 | `Chunk::ToolCall` 没有序号：并行调用时参数分片无法归位（A 的参数会拼到 B 上） | 并行工具调用只能整体放弃或瞎拼 | `Chunk::ToolCall` 增 `index: Option<u32>`（`None` 不序列化，wire 兼容）；三适配器分别取 `tool_calls[].index` / `content_block_start.index` / `output_index` | **P0** — ✅ W2b 已解决（[ADR-17](../04-ADR.md)） |

## 已解决（W2b）

| # | 落地位置 | 验收方式 |
|---|---|---|
| C1 | `crates/shared/src/protocol.rs`（`ToolCallRef` + `ChatMessage::tool_calls` + `ChatRequest::validate` 三条校验） | 三协议各一条"文本 + 工具同轮"fixture；`shared` 单测覆盖并行调用与三条校验 |
| C6 | `crates/core/src/protocols`（`parse_events -> Vec<Chunk>`，`done_last` 保证 `Done` 必末） | 三协议各一条"usage + 结束原因同事件"fixture；`[DONE]` / 空事件 / 上游错误行为统一 |
| C8 | `crates/shared/src/stream.rs`（`ChunkAssembler::feed/finish` 注入 `Meta`、合成 `Ping` 语义） | 装配器单测 + `docs/03` §9.4 写入契约 |
| C9 | `crates/shared/src/protocol.rs`（`Chunk::ToolCall.index: Option<u32>`，`None` 不序列化）+ `crates/core/src/protocols`（三适配器各取自己的序号字段） | 三协议并行调用 fixture；`index` 缺失时行为与旧 wire 一致（兼容性测试） |

顺带定下的口径（C6 配套，不改类型）：`Usage.tokens_in/tokens_out = 0` 表示"本事件**未携带**
该字段"，同一 turn 的多个 `Usage` 按字段取 **max** 合并，任一 `estimated=true` 则结果也是 true。

## 仍未解决（W3 之前不做）

| # | 为什么留着 | 触发条件 |
|---|---|---|
| C2 | `temperature` 只在"换模型要保行为一致"时才需要；1.0 让端侧按模型给默认值即可 | 出现"同一固定超参必须在三协议一致"的需求 |
| C3 | 端侧能把工具失败写进 `tool_result` 的文本里，模型大多能读懂；加字段要动 6 处编码 | 模型开始依赖结构化 `is_error`（评测里能测出来） |
| C4 | 协议本身没有位置，属于不可消除的限制 | 只做"尽力携带 + 日志" |
| C5 | 契约层已保证 `arguments` 是合法 JSON；上游给字符串时降级 `{}` 并记日志 | 上游开始返回字符串化参数 |
| C7 | 跨事件聚合要引入有状态解析器，`parse_events` 的纯函数约定会破 | W3 做"长参数工具调用"时一并设计 |

## 改契约时的检查单

1. 改 `crates/shared` → 同步 `docs/03-接口文档.md` §9 与 `docs/02-系统设计文档.md` 的协议表；
2. 三协议适配器各补一条 fixture；
3. `cargo test --workspace` 与 `bench` 的 smoke 用例都要过；
4. 在 ADR 里记一条（若改变了"谁能表达什么"的边界）。
