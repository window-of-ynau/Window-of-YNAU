//! 抽样记账与审计（ADR-03：按时间计费，token 只做**抽样成本监控**）。
//!
//! 两件事，都**绝不阻塞、绝不影响流**：
//!
//! - **1% 抽样**：`usage_samples` **每轮（每次上游调用）一行**（4 轮工具循环 = 4 次调用 = 4 行），
//!   `(turn_id, round)` 是幂等键、写入 upsert，重放不会重复计数。写的是装配器合并后的终值
//!   （[`ChunkAssembler::usage`](yunong_shared::ChunkAssembler)），逐事件写会显著高估。
//! - **审计**：`audit_logs` 记"放行"与"拒绝"两类动作（网关拒绝要能事后追查）。
//!
//! 两条铁律：写库失败只记 `warn`，不影响已经发给客户端的流；没有数据库（无 DB 模式）
//! 时静默跳过，本地骨架照样能跑。
//!
//! 抽样用 **turn_id 的 SHA-256 前缀**做确定性判定：同一个 turn 重放不会重复计数，
//! 也不需要随机数依赖。

use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;
use yunong_shared::{Channel, Protocol, Usage};

/// 一条抽样样本（对应 `usage_samples` 一行）。
///
/// **粒度是"一次上游调用"**，不是"一个 turn"：4 轮工具循环就是 4 次调用、4 行。
/// `(turn_id, round)` 是幂等键——同一轮重放只会更新那一行（见 `0003_*` 迁移与 `write_sample` 的 upsert）。
#[derive(Debug, Clone)]
pub struct UsageSample {
    /// 一次用户提问的 id（多轮工具循环共享）
    pub turn_id: String,
    pub user_id: Uuid,
    pub req_id: String,
    pub model: String,
    pub protocol: Protocol,
    pub channel: Channel,
    pub round: u8,
    /// 合并后的终值；`usage.estimated = true` 表示上游没给 usage、这里是服务端估算
    pub usage: Usage,
}

/// 审计动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDecision {
    Allow,
    Deny,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// 一条审计（对应 `audit_logs` 一行）。**不放正文**，只放动作与原因。
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub user_id: Uuid,
    pub action: String,
    pub target: Option<String>,
    pub decision: AuditDecision,
    pub reason: Option<String>,
    pub req_id: String,
}

impl AuditEntry {
    /// 网关放行。
    pub fn allow(user_id: Uuid, req_id: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            user_id,
            action: "chat.stream".to_owned(),
            target: Some(target.into()),
            decision: AuditDecision::Allow,
            reason: None,
            req_id: req_id.into(),
        }
    }

    /// 网关/限额拒绝。
    pub fn deny(
        user_id: Uuid,
        req_id: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            user_id,
            action: "chat.stream".to_owned(),
            target: Some(target.into()),
            decision: AuditDecision::Deny,
            reason: Some(reason.into()),
            req_id: req_id.into(),
        }
    }
}

/// 记账器：`AppState` 持有一份，所有请求共用。
#[derive(Debug, Clone)]
pub struct UsageRecorder {
    db: Option<PgPool>,
    sample_percent: u8,
}

impl UsageRecorder {
    pub fn new(db: Option<PgPool>, sample_percent: u8) -> Self {
        Self {
            db,
            sample_percent: sample_percent.min(100),
        }
    }

    pub fn sample_percent(&self) -> u8 {
        self.sample_percent
    }

    /// 这个 turn 是否需要抽样落库。确定性判定：同一 turn 永远同一结果。
    pub fn should_sample(&self, turn_id: &str) -> bool {
        match self.sample_percent {
            0 => false,
            100 => true,
            percent => {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(turn_id.as_bytes());
                let first = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
                (first % 100) < u32::from(percent)
            }
        }
    }

    /// 写一条抽样样本。返回 `Ok(false)` 表示"没有数据库，跳过"。
    ///
    /// 写入是 **upsert**：`(turn_id, round)` 唯一，同一轮重放只会覆盖那一行
    /// （否则客户端重试一次就多算一次成本）。
    pub async fn write_sample(&self, sample: &UsageSample) -> Result<bool, sqlx::Error> {
        let Some(pool) = &self.db else {
            return Ok(false);
        };
        // 只在终止时才写：写的是装配器合并后的终值；上游没给 usage 时是估算值（estimated=true）
        sqlx::query(
            "INSERT INTO usage_samples \
             (turn_id, user_id, req_id, model, protocol, channel, round, tokens_in, tokens_out, estimated) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (turn_id, round) DO UPDATE SET \
               req_id = EXCLUDED.req_id, \
               model = EXCLUDED.model, \
               protocol = EXCLUDED.protocol, \
               channel = EXCLUDED.channel, \
               tokens_in = EXCLUDED.tokens_in, \
               tokens_out = EXCLUDED.tokens_out, \
               estimated = EXCLUDED.estimated",
        )
        .bind(&sample.turn_id)
        .bind(sample.user_id)
        .bind(&sample.req_id)
        .bind(&sample.model)
        .bind(protocol_str(sample.protocol))
        .bind(sample.channel.as_str())
        .bind(i16::from(sample.round))
        .bind(sample.usage.tokens_in as i32)
        .bind(sample.usage.tokens_out as i32)
        .bind(sample.usage.estimated)
        .execute(pool)
        .await?;
        Ok(true)
    }

    /// 写一条审计。返回 `Ok(false)` 表示"没有数据库，跳过"。
    pub async fn write_audit(&self, entry: &AuditEntry) -> Result<bool, sqlx::Error> {
        let Some(pool) = &self.db else {
            return Ok(false);
        };
        sqlx::query(
            "INSERT INTO audit_logs (user_id, action, target, decision, reason, req_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(entry.user_id)
        .bind(&entry.action)
        .bind(entry.target.as_deref())
        .bind(entry.decision.as_str())
        .bind(entry.reason.as_deref())
        .bind(&entry.req_id)
        .execute(pool)
        .await?;
        Ok(true)
    }

    /// 抽样落库：**fire-and-forget**，失败只 warn。
    pub fn record_sample_detached(self: &Arc<Self>, sample: UsageSample) {
        let Some(_) = &self.db else {
            return; // 无数据库模式：连 task 都不用起
        };
        let recorder = Arc::clone(self);
        tokio::spawn(async move {
            match recorder.write_sample(&sample).await {
                Ok(_) => {}
                Err(error) => {
                    // 记账失败不能让用户看到错误，也不能影响成本监控之外的东西
                    tracing::warn!(%error, req_id = %sample.req_id, "写 usage_samples 失败（不影响对话流）");
                }
            }
        });
    }

    /// 审计落库：同样 fire-and-forget。
    pub fn record_audit_detached(self: &Arc<Self>, entry: AuditEntry) {
        let Some(_) = &self.db else {
            return;
        };
        let recorder = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = recorder.write_audit(&entry).await {
                tracing::warn!(%error, req_id = %entry.req_id, "写 audit_logs 失败（不影响对话流）");
            }
        });
    }
}

/// `usage_samples.protocol` 的取值：与 `Protocol` 的 serde 表示一致。
pub fn protocol_str(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => "openai_completions",
        Protocol::OpenaiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_percent_boundaries() {
        let none = UsageRecorder::new(None, 0);
        for turn in ["a", "b", "c"] {
            assert!(!none.should_sample(turn), "0% 永远不抽样");
        }
        let all = UsageRecorder::new(None, 100);
        for turn in ["a", "b", "c"] {
            assert!(all.should_sample(turn), "100% 永远抽样");
        }
    }

    #[test]
    fn sampling_is_deterministic_per_turn() {
        let recorder = UsageRecorder::new(None, 1);
        for turn in ["turn-1", "turn-2", "会话-中文"] {
            assert_eq!(
                recorder.should_sample(turn),
                recorder.should_sample(turn),
                "同一 turn 的结果必须稳定（重放不重复计数）"
            );
        }
    }

    #[test]
    fn one_percent_sampling_hits_roughly_one_in_a_hundred() {
        let recorder = UsageRecorder::new(None, 1);
        let hits = (0..10_000)
            .filter(|index| recorder.should_sample(&format!("turn-{index}")))
            .count();
        // 期望 100，给足抖动区间，避免 flaky
        assert!(
            (40..=200).contains(&hits),
            "10k 个 turn 按 1% 抽样，命中 {hits} 个，明显偏离"
        );
    }

    #[test]
    fn without_database_writes_are_skipped_not_errors() {
        let recorder = UsageRecorder::new(None, 100);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("建 runtime");
        runtime.block_on(async {
            let sample = UsageSample {
                turn_id: "turn-1".into(),
                user_id: Uuid::new_v4(),
                req_id: "req_1".into(),
                model: "m".into(),
                protocol: Protocol::OpenaiCompletions,
                channel: Channel::Plan,
                round: 1,
                usage: Usage::default(),
            };
            assert!(matches!(recorder.write_sample(&sample).await, Ok(false)));
            let entry = AuditEntry::allow(Uuid::new_v4(), "req_1", "turn-1");
            assert!(matches!(recorder.write_audit(&entry).await, Ok(false)));
        });
    }

    #[test]
    fn audit_entries_never_carry_content() {
        let user = Uuid::new_v4();
        let allow = AuditEntry::allow(user, "req_1", "turn-1");
        assert_eq!(allow.decision, AuditDecision::Allow);
        assert_eq!(allow.decision.as_str(), "allow");
        assert!(allow.reason.is_none());

        let deny = AuditEntry::deny(user, "req_2", "turn-2", "concurrency_limit");
        assert_eq!(deny.decision.as_str(), "deny");
        assert_eq!(deny.reason.as_deref(), Some("concurrency_limit"));
        assert_eq!(deny.action, "chat.stream");
    }

    #[test]
    fn protocol_strings_match_the_database_check_constraint() {
        // usage_samples.protocol 没有 CHECK，但取值必须和契约里的 snake_case 一致
        assert_eq!(
            protocol_str(Protocol::OpenaiCompletions),
            "openai_completions"
        );
        assert_eq!(protocol_str(Protocol::OpenaiResponses), "openai_responses");
        assert_eq!(
            protocol_str(Protocol::AnthropicMessages),
            "anthropic_messages"
        );
    }
}
