//! 流式事件装配器（ADR-16 的共用件）。
//!
//! 上游协议会违反我们自己对外承诺的两条不变量：
//! 1. **`Meta` 必首且唯一** —— 上游不会给我们发 meta；
//! 2. **`Done`/`Error` 必末且唯一** —— 但 Completions 的 `finish_reason` 帧给出真正原因后，
//!    还会再来一个 `[DONE]`，一条流里出现**两个** Done；而 `usage` 又常和结束原因同一个事件。
//!
//! 端侧网关（BYOK 直连）与服务端代理（套餐）**都**消费 `parse_events`，
//! 所以这个去重/合并逻辑必须只有一份实现，否则两边行为会分叉。
//!
//! ```text
//! parse_events(..) ──► ChunkAssembler::feed(..) ──► 契约合法的 Chunk 序列 ──► SSE / 回调
//!                                                  └─► usage() 供记账
//! ```

use crate::entitlement::Channel;
use crate::protocol::{Chunk, FinishReason, Usage};

/// 把原始事件流装配成契约合法的序列。
#[derive(Debug, Clone)]
pub struct ChunkAssembler {
    channel: Channel,
    request_id: String,
    model: String,
    meta_emitted: bool,
    /// 已经给出终止事件（Done 或 Error），之后的输入一律丢弃
    terminated: bool,
    /// 合并后的累计用量（按字段取 max，`estimated` 取或）
    usage: Usage,
}

impl ChunkAssembler {
    pub fn new(channel: Channel, request_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            channel,
            request_id: request_id.into(),
            model: model.into(),
            meta_emitted: false,
            terminated: false,
            usage: Usage::default(),
        }
    }

    /// 合并后的用量终值。记账/成本监控只应读这个值，不要逐事件累加。
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// 是否已经收到过终止事件。
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    fn merge_usage(&mut self, incoming: Usage) {
        // 上游累计值单调不减；Anthropic 的 input/output 分布在互不重叠的事件里
        self.usage.tokens_in = self.usage.tokens_in.max(incoming.tokens_in);
        self.usage.tokens_out = self.usage.tokens_out.max(incoming.tokens_out);
        self.usage.estimated |= incoming.estimated;
    }

    fn meta_chunk(&self) -> Chunk {
        Chunk::Meta {
            request_id: self.request_id.clone(),
            model: self.model.clone(),
            channel: self.channel,
        }
    }

    fn usage_chunk(&self) -> Option<Chunk> {
        let u = self.usage;
        (u.tokens_in > 0 || u.tokens_out > 0 || u.estimated).then_some(Chunk::Usage {
            tokens_in: u.tokens_in,
            tokens_out: u.tokens_out,
            estimated: u.estimated,
        })
    }

    /// 喂入一批原始事件，返回可以对外发送的事件。
    ///
    /// - `Meta` 由本装配器注入（上游的 Meta 会被丢弃）；
    /// - `Usage` 被缓冲，只在终止事件前输出一次；
    /// - 第一个终止事件（`Done`/`Error`）胜出，后续输入全部丢弃。
    pub fn feed(&mut self, incoming: Vec<Chunk>) -> Vec<Chunk> {
        let mut out: Vec<Chunk> = Vec::new();
        for chunk in incoming {
            if self.terminated {
                break;
            }
            match chunk {
                // 我们自己发 meta，上游的不透传
                Chunk::Meta { .. } => {}
                Chunk::Usage {
                    tokens_in,
                    tokens_out,
                    estimated,
                } => {
                    self.merge_usage(Usage {
                        tokens_in,
                        tokens_out,
                        estimated,
                    });
                }
                Chunk::Done { finish } => {
                    self.ensure_meta(&mut out);
                    if let Some(u) = self.usage_chunk() {
                        out.push(u);
                    }
                    out.push(Chunk::Done { finish });
                    self.terminated = true;
                }
                Chunk::Error { code, message } => {
                    self.ensure_meta(&mut out);
                    // 出错时不补 Done 也不发 usage：错误就是终止事件
                    out.push(Chunk::Error { code, message });
                    self.terminated = true;
                }
                Chunk::Ping => {
                    self.ensure_meta(&mut out);
                    out.push(Chunk::Ping);
                }
                other => {
                    self.ensure_meta(&mut out);
                    out.push(other);
                }
            }
        }
        out
    }

    /// 流结束（上游正常关闭）。若还没收到终止事件，补一个 `Done{Stop}`。
    ///
    /// 返回空 Vec 表示不需要再发任何东西（已经结束过了）。
    pub fn finish(&mut self) -> Vec<Chunk> {
        if self.terminated {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_meta(&mut out);
        if let Some(u) = self.usage_chunk() {
            out.push(u);
        }
        out.push(Chunk::Done {
            finish: FinishReason::Stop,
        });
        self.terminated = true;
        out
    }

    fn ensure_meta(&mut self, out: &mut Vec<Chunk>) {
        if !self.meta_emitted {
            out.push(self.meta_chunk());
            self.meta_emitted = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn asm() -> ChunkAssembler {
        ChunkAssembler::new(Channel::Plan, "req_1", "deepseek-chat")
    }

    fn delta(text: &str) -> Chunk {
        Chunk::Delta { text: text.into() }
    }

    fn usage(i: u32, o: u32) -> Chunk {
        Chunk::Usage {
            tokens_in: i,
            tokens_out: o,
            estimated: false,
        }
    }

    #[test]
    fn meta_is_injected_first_and_exactly_once() {
        let mut a = asm();
        let out = a.feed(vec![delta("你")]);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Chunk::Meta { .. }));
        assert_eq!(out[1], delta("你"));

        // 上游也发 Meta：必须被丢弃，不能出现第二个
        let out = a.feed(vec![Chunk::Meta {
            request_id: "upstream".into(),
            model: "x".into(),
            channel: Channel::Byok,
        }]);
        assert!(out.is_empty(), "上游 Meta 不应透传: {out:?}");
    }

    #[test]
    fn duplicate_done_is_deduped_and_first_finish_reason_wins() {
        let mut a = asm();
        // Completions 真实形态：finish 帧带真实原因，随后 [DONE] 再来一个
        let out = a.feed(vec![
            delta("好"),
            Chunk::Done {
                finish: FinishReason::ToolCalls,
            },
            Chunk::Done {
                finish: FinishReason::Stop,
            },
        ]);
        let dones: Vec<_> = out
            .iter()
            .filter(|c| matches!(c, Chunk::Done { .. }))
            .collect();
        assert_eq!(dones.len(), 1, "一条流只能有一个 Done: {out:?}");
        assert_eq!(
            *dones[0],
            Chunk::Done {
                finish: FinishReason::ToolCalls
            }
        );
        // 且它必须是最后一个事件
        assert!(matches!(out.last(), Some(Chunk::Done { .. })));
    }

    #[test]
    fn chunks_after_done_are_dropped() {
        let mut a = asm();
        let _ = a.feed(vec![Chunk::Done {
            finish: FinishReason::Stop,
        }]);
        assert!(a.feed(vec![delta("迟到")]).is_empty());
        assert!(a.is_terminated());
    }

    #[test]
    fn usage_is_merged_by_field_max_and_emitted_once() {
        let mut a = asm();
        // Anthropic 形态：input 在 message_start、output 在 message_delta
        let out = a.feed(vec![
            usage(1200, 0),
            usage(0, 15),
            Chunk::Done {
                finish: FinishReason::Stop,
            },
        ]);
        let usages: Vec<_> = out
            .iter()
            .filter(|c| matches!(c, Chunk::Usage { .. }))
            .collect();
        assert_eq!(usages.len(), 1, "usage 只输出一次: {out:?}");
        assert_eq!(a.usage().tokens_in, 1200);
        assert_eq!(a.usage().tokens_out, 15);
        // usage 必须在 Done 之前
        let uidx = out
            .iter()
            .position(|c| matches!(c, Chunk::Usage { .. }))
            .unwrap();
        let didx = out
            .iter()
            .position(|c| matches!(c, Chunk::Done { .. }))
            .unwrap();
        assert!(uidx < didx);
    }

    #[test]
    fn estimated_flag_is_sticky() {
        let mut a = asm();
        let _ = a.feed(vec![Chunk::Usage {
            tokens_in: 10,
            tokens_out: 0,
            estimated: true,
        }]);
        let _ = a.feed(vec![usage(10, 5)]);
        assert!(a.usage().estimated, "任一事件估算过，合并结果就是估算");
    }

    #[test]
    fn error_terminates_without_done() {
        let mut a = asm();
        let out = a.feed(vec![
            Chunk::Error {
                code: ErrorCode::UpstreamError,
                message: "boom".into(),
            },
            delta("不该出现"),
        ]);
        assert!(matches!(out.last(), Some(Chunk::Error { .. })));
        assert!(
            !out.iter().any(|c| matches!(c, Chunk::Done { .. })),
            "错误后不应再有 Done"
        );
        assert!(a.finish().is_empty(), "已经终止，finish 不应再发东西");
    }

    #[test]
    fn finish_synthesizes_done_and_meta_for_empty_stream() {
        let mut a = asm();
        let out = a.finish();
        assert!(
            matches!(out[0], Chunk::Meta { .. }),
            "空流也必须先有 meta: {out:?}"
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[1],
            Chunk::Done {
                finish: FinishReason::Stop
            }
        );
        assert!(a.finish().is_empty(), "power: 只能补一次");
    }
}
