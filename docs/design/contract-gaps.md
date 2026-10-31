# 契约缺口登记（`crates/shared` 待改）

> 来源：`agent/protocols` 在实现三协议适配时，发现**内部契约无法表达某些上游语义**。
> 这是并行开发最有价值的产出之一：写实现的人才会知道契约缺什么。
>
> 处理原则：**先登记、再改契约、最后改实现**。改 `crates/shared` 属于单写者变更，必须在这里留档，
> 否则三个 agent 各自"就地变通"会让契约名存实亡。

| # | 缺口 | 影响 | 建议改法 | 优先级 |
|---|---|---|---|---|
| C1 | `ChatMessage` 没有 `tool_calls` 字段，assistant 的工具调用只能塞进 `content` | 模型"边说边调工具"无法表达；多工具并行调用无法表达 | `ChatMessage` 增加 `tool_calls: Vec<ToolCallRef>`（`id`/`name`/`arguments`），`content` 回归纯文本 | **P0** — W2b 已解决（[ADR-16](../04-ADR.md)） |
| C2 | `ChatRequest` 没有 `temperature` | 文档 §9 写了但契约里没有；三协议实现一律不发，行为不可调 | 增加 `temperature: Option<f32>`，缺省不发 | P1 |
| C3 | 工具结果没有 `ok`/`is_error` | Anthropic `tool_result.is_error` 丢失；模型看不出"工具失败了" | `ChatMessage` 增加 `tool_ok: Option<bool>` | P1 |
| C4 | Completions 的 `tool` 消息与 Responses 的 `function_call_output` **没有 name 位置** | 工具名只能靠 `tool_call_id` 反查；上游若丢 id 就对不上 | 内部保留 `name`（已有字段），编码时按协议能力尽力携带，并在日志里记录丢失 | P2（协议限制，不可消除） |
| C5 | Anthropic `tool_use.input` 必须是 JSON 对象 | 上游给的是字符串参数时非法 JSON 只能降级 `{}` | 契约层保证 `arguments` 永远是**合法 JSON 值**；端侧执行前校验 schema | P1 |
| C6 | usage 与结束原因经常在**同一个事件**里（Anthropic `message_delta`、Responses `response.completed`） | `parse_event` 一次只能返回一个 `Chunk`，二选一必丢数据 | 改为 `parse_events(...) -> Vec<Chunk>`，允许一事件多 chunk | **P0** — W2b 已解决（[ADR-16](../04-ADR.md)） |
| C7 | 无状态解析：工具参数分片（`arguments.delta`）无法跨事件聚合 | 长参数的工具调用会被截断 | 由 `gateway` 持有聚合状态，`parse_events` 保持纯函数并在注释声明 | P1 |
| C8 | `Meta`/`Ping` 不由适配层产出 | 若客户端依赖 `Meta` 判断通道，缺失会出错 | 明确由 gateway 合成；适配层只产出 `Delta/ToolCall/Usage/Done/Error` | P2（已按此实现，仅需写清） |

## 已解决（W2b）

| # | 落地位置 | 验收方式 |
|---|---|---|
| C1 | `crates/shared/src/protocol.rs`（`ToolCallRef` + `ChatMessage::tool_calls` + `ChatRequest::validate` 三条校验） | 三协议各一条"文本 + 工具同轮"fixture；`shared` 单测覆盖并行调用与三条校验 |
| C6 | `crates/core/src/protocols`（`parse_events -> Vec<Chunk>`，`done_last` 保证 `Done` 必末） | 三协议各一条"usage + 结束原因同事件"fixture；`[DONE]` / 空事件 / 上游错误行为统一 |

顺带定下的口径（C6 配套，不改类型）：`Usage.tokens_in/tokens_out = 0` 表示"本事件**未携带**
该字段"，同一 turn 的多个 `Usage` 按字段取 **max** 合并，任一 `estimated=true` 则结果也是 true。

## 改契约时的检查单

1. 改 `crates/shared` → 同步 `docs/03-接口文档.md` §9 与 `docs/02-系统设计文档.md` 的协议表；
2. 三协议适配器各补一条 fixture；
3. `cargo test --workspace` 与 `bench` 的 smoke 用例都要过；
4. 在 ADR 里记一条（若改变了"谁能表达什么"的边界）。
