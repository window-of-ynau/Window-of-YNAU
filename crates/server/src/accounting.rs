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

/// 审计结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDecision {
    /// 我们放行了。
    Allow,
    /// 我们**按策略**拒绝了（鉴权失败、闸门拒绝、配额）。
    Deny,
    /// 放行了但**执行失败**（上游故障）。不是策略拒绝，别混进 deny：
    /// 否则"被拒绝率"这类指标会被上游抖动污染。
    Error,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }
}

/// 审计事件类型（`audit_logs.action`）。
pub mod audit_action {
    /// 一次对话请求（放行 / 闸门拒绝 / 上游失败）。
    pub const CHAT_STREAM: &str = "chat.stream";
    /// 鉴权中间件拒绝了请求（令牌无效 / 过期 / 已吊销 / 用户被停用）。
    pub const AUTHENTICATE: &str = "auth.authenticate";
}

/// 一条审计（对应 `audit_logs` 一行）。**不放正文**，只放动作与原因。
///
/// `user_id` 可空：鉴权失败时令牌本身就无效，我们不知道（也不该猜）是谁。
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub user_id: Option<Uuid>,
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
            user_id: Some(user_id),
            action: audit_action::CHAT_STREAM.to_owned(),
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
            user_id: Some(user_id),
            action: audit_action::CHAT_STREAM.to_owned(),
            target: Some(target.into()),
            decision: AuditDecision::Deny,
            reason: Some(reason.into()),
            req_id: req_id.into(),
        }
    }

    /// 鉴权失败：令牌无效/过期/已吊销，或用户被停用。
    ///
    /// **只记"确实带了令牌但被拒"的情况**（见 `api::auth_guard`）：完全没带令牌的请求
    /// 是海量且无信息量的，逐条落库等于给攻击者一个免费的写放大入口。
    pub fn auth_deny(
        user_id: Option<Uuid>,
        req_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            user_id,
            action: audit_action::AUTHENTICATE.to_owned(),
            target: None,
            decision: AuditDecision::Deny,
            reason: Some(reason.into()),
            req_id: req_id.into(),
        }
    }

    /// 上游失败（超时 / 非 2xx / 不是 SSE / 流中断）。
    ///
    /// `reason` 只放**机器可读的短码**，绝不放厂商原文（与 F3/F9 同一条边界）。
    pub fn upstream_error(
        user_id: Uuid,
        req_id: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            user_id: Some(user_id),
            action: audit_action::CHAT_STREAM.to_owned(),
            target: Some(target.into()),
            decision: AuditDecision::Error,
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
    /// 抽样盐（红队 M3）：客户端不可知，避免"挑一个不被抽中的 turn_id"规避监控
    salt: String,
}

impl UsageRecorder {
    pub fn new(db: Option<PgPool>, sample_percent: u8, salt: impl Into<String>) -> Self {
        Self {
            db,
            sample_percent: sample_percent.min(100),
            salt: salt.into(),
        }
    }

    pub fn sample_percent(&self) -> u8 {
        self.sample_percent
    }

    /// 这个 turn 是否需要抽样落库。确定性判定：同一 `(用户, turn)` 永远同一结果。
    ///
    /// 判定输入是 `salt || user_id || turn_id`（红队 M3）：
    /// - 只用客户端可控的 `turn_id` 时，客户端可以**预计算**哈希，挑一个不命中的 id
    ///   来规避成本监控（或挑命中的 id 刷写放大）；
    /// - 盐来自服务端（环境变量或启动随机），客户端拿不到，也就无法预测。
    ///
    /// 加盐只影响**新写入**的抽样判定，历史行保留原样、不回填。
    pub fn should_sample(&self, user_id: Uuid, turn_id: &str) -> bool {
        match self.sample_percent {
            0 => false,
            100 => true,
            percent => {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(self.salt.as_bytes());
                hasher.update(user_id.as_bytes());
                hasher.update(turn_id.as_bytes());
                let digest = hasher.finalize();
                let first = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
                (first % 100) < u32::from(percent)
            }
        }
    }

    /// 写一条抽样样本。返回 `Ok(false)` 表示"没有数据库，跳过"。
    ///
    /// 写入是 **upsert**：`(user_id, turn_id, round)` 唯一，同一用户同一轮重放只覆盖那一行
    /// （否则客户端重试一次就多算一次成本）。
    ///
    /// 唯一键**必须带 `user_id`**（评审 F11）：`turn_id` 是端侧生成的，两个用户撞同一个
    /// `turn_id` 并不罕见；只按 `(turn_id, round)` 唯一会让后一个用户改掉前一个用户的行
    /// （`user_id` 还留在前一个人身上，跨用户串账）。
    pub async fn write_sample(&self, sample: &UsageSample) -> Result<bool, sqlx::Error> {
        let Some(pool) = &self.db else {
            return Ok(false);
        };
        // 只在终止时才写：写的是装配器合并后的终值；上游没给 usage 时是估算值（estimated=true）
        sqlx::query(
            "INSERT INTO usage_samples \
             (turn_id, user_id, req_id, model, protocol, channel, round, tokens_in, tokens_out, estimated) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (user_id, turn_id, round) DO UPDATE SET \
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

/// 启动自检：库里有没有 `0003` 迁移加的那两列（评审 F12）。
///
/// 没有守卫时，漏跑迁移的表现是"抽样 INSERT 失败 → 只有 `warn` → 账目静默为零"，
/// 谁都不会发现。这里选择**启动即失败**：宁可起不来，也不要悄悄不记账。
///
/// 无数据库模式不调用它（本来就不记账）。
pub async fn verify_schema(pool: &PgPool) -> Result<(), String> {
    const REQUIRED: [&str; 2] = ["turn_id", "estimated"];

    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_schema = ANY (current_schemas(false)) \
           AND table_name = 'usage_samples'",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| format!("检查 usage_samples 表结构失败：{error}"))?;

    if columns.is_empty() {
        return Err(
            "数据库里没有 usage_samples 表：请先执行 crates/server/migrations/ 下的全部迁移\
             （psql \"$DATABASE_URL\" -f 每个 .sql，或 `sqlx migrate run`）"
                .into(),
        );
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|column| !columns.iter().any(|name| name == column))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "usage_samples 缺少列 {missing:?}：迁移没跑全（多半是漏了 0003_usage_turn_estimated.sql）。\n\
         请先执行：psql \"$DATABASE_URL\" -f crates/server/migrations/0003_usage_turn_estimated.sql\n\
         不修就启动的话，抽样记账会一直静默失败、成本统计永远是 0。"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SALT: &str = "test-salt-0123456789";

    #[test]
    fn sampling_percent_boundaries() {
        let user = Uuid::new_v4();
        let none = UsageRecorder::new(None, 0, TEST_SALT);
        for turn in ["a", "b", "c"] {
            assert!(!none.should_sample(user, turn), "0% 永远不抽样");
        }
        let all = UsageRecorder::new(None, 100, TEST_SALT);
        for turn in ["a", "b", "c"] {
            assert!(all.should_sample(user, turn), "100% 永远抽样");
        }
    }

    #[test]
    fn sampling_is_deterministic_per_turn() {
        let user = Uuid::new_v4();
        let recorder = UsageRecorder::new(None, 1, TEST_SALT);
        for turn in ["turn-1", "turn-2", "会话-中文"] {
            assert_eq!(
                recorder.should_sample(user, turn),
                recorder.should_sample(user, turn),
                "同一 turn 的结果必须稳定（重放不重复计数）"
            );
        }
    }

    /// 红队 M3：判定与**服务端盐**绑定——客户端不知道盐，就无法挑 turn_id 规避抽样。
    ///
    /// 用例：同一批 turn_id，换一个盐后命中集合必须发生变化（否则说明盐没参与哈希）。
    #[test]
    fn sampling_is_salted_so_clients_cannot_predict() {
        let user = Uuid::new_v4();
        let turns: Vec<String> = (0..500).map(|i| format!("turn-{i}")).collect();
        let a = UsageRecorder::new(None, 10, "salt-A-0123456789");
        let b = UsageRecorder::new(None, 10, "salt-B-0123456789");
        let hits_a: Vec<bool> = turns.iter().map(|t| a.should_sample(user, t)).collect();
        let hits_b: Vec<bool> = turns.iter().map(|t| b.should_sample(user, t)).collect();
        assert_ne!(hits_a, hits_b, "换盐后命中集合必须变化，否则盐没生效");

        // 同一用户同一 turn、同盐 → 稳定（重放不重复计数）
        assert_eq!(
            a.should_sample(user, "turn-1"),
            a.should_sample(user, "turn-1")
        );
        // 不同用户同一 turn → 独立判定（不是全站同一条分界线）
        let other = Uuid::new_v4();
        let hits_other: Vec<bool> = turns.iter().map(|t| a.should_sample(other, t)).collect();
        assert_ne!(hits_a, hits_other, "不同用户不应共享同一判定序列");
    }

    #[test]
    fn one_percent_sampling_hits_roughly_one_in_a_hundred() {
        let recorder = UsageRecorder::new(None, 1, TEST_SALT);
        let user = Uuid::new_v4();
        let hits = (0..10_000)
            .filter(|index| recorder.should_sample(user, &format!("turn-{index}")))
            .count();
        // 期望 100，给足抖动区间，避免 flaky
        assert!(
            (40..=200).contains(&hits),
            "10k 个 turn 按 1% 抽样，命中 {hits} 个，明显偏离"
        );
    }

    #[test]
    fn without_database_writes_are_skipped_not_errors() {
        let recorder = UsageRecorder::new(None, 100, TEST_SALT);
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

    /// W7：三类事件各自的 action / decision / reason 口径。
    #[test]
    fn audit_covers_auth_gate_and_upstream_events() {
        let user = Uuid::new_v4();

        // 鉴权失败：不知道是谁（令牌本身就无效），所以 user_id 为空
        let auth = AuditEntry::auth_deny(None, "req_auth", "token_expired");
        assert_eq!(auth.action, audit_action::AUTHENTICATE);
        assert_eq!(auth.decision, AuditDecision::Deny);
        assert_eq!(auth.reason.as_deref(), Some("token_expired"));
        assert!(auth.user_id.is_none(), "鉴权失败时不该编造 user_id");
        assert!(auth.target.is_none());

        // 上游失败：既不是放行也不是策略拒绝 → error
        let upstream = AuditEntry::upstream_error(user, "req_up", "turn-9", "upstream_status_500");
        assert_eq!(upstream.action, audit_action::CHAT_STREAM);
        assert_eq!(upstream.decision, AuditDecision::Error);
        assert_eq!(upstream.decision.as_str(), "error");
        assert_eq!(upstream.reason.as_deref(), Some("upstream_status_500"));
        assert_eq!(upstream.user_id, Some(user));

        // 闸门拒绝：策略拒绝 → deny
        let gate = AuditEntry::deny(user, "req_gate", "turn-9", "user_in_flight");
        assert_eq!(gate.decision, AuditDecision::Deny);
        assert_eq!(gate.reason.as_deref(), Some("user_in_flight"));
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
