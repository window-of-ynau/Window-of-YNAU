-- 云农之窗 · 审计日志的覆盖面补全（W7）
--
-- 背景：`audit_logs` 之前只记了"闸门放行 / 闸门拒绝"两类，缺三类关键事件：
--   1. 鉴权失败（令牌无效 / 过期 / 已吊销）——安全事件，必须能事后追查；
--   2. 上游失败（超时 / 非 2xx / 不是 SSE / 流中断）——排障与上游 SLA 的依据；
--   3. 闸门拒绝已有，但要能按原因聚合（靠 reason + 下面的索引）。
--
-- decision 语义：
--   allow = 我们放行了；
--   deny  = 我们**按策略**拒绝了（鉴权失败、闸门拒绝、配额）；
--   error = 我们放行了但**执行失败**（上游故障）。既不是放行也不是策略拒绝，
--           混进 deny 会让"被拒绝率"这类指标失真，所以单独一个值。

ALTER TABLE audit_logs DROP CONSTRAINT IF EXISTS audit_logs_decision_check;
ALTER TABLE audit_logs
    ADD CONSTRAINT audit_logs_decision_check CHECK (decision IN ('allow', 'deny', 'error'));

-- 鉴权失败时我们不知道用户是谁（令牌无效），user_id 留空是正常的；
-- 但 action/reason 要能聚合，所以给 (action, created_at) 建索引。
CREATE INDEX IF NOT EXISTS ix_audit_action_time ON audit_logs (action, created_at DESC);

-- 排障时最常查的是"最近有哪些被拒/失败的请求"：部分索引只覆盖非 allow 的行，
-- 体积远小于全表索引。
CREATE INDEX IF NOT EXISTS ix_audit_not_allow_time
    ON audit_logs (created_at DESC)
    WHERE decision <> 'allow';

COMMENT ON COLUMN audit_logs.action IS '事件类型：chat.stream（对话）/ auth.authenticate（鉴权）';
COMMENT ON COLUMN audit_logs.decision IS 'allow=放行；deny=按策略拒绝；error=放行后执行失败（上游故障）';
COMMENT ON COLUMN audit_logs.reason IS '机器可读的短码（如 turn_in_flight / token_expired / upstream_timeout），不含上游原文';
