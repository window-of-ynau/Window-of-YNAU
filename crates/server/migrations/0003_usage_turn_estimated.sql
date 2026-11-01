-- 云农之窗 · 抽样记账的两个修正（评审 F1 / F2）
--
-- F1：口径原来是"每个 turn 一行"，但实际是**每个上游调用（每轮）一行**——
--     4 轮工具循环会写 4 行，且没有幂等键。这里补 turn_id 并加 (turn_id, round) 唯一索引，
--     写入改成 upsert：同一轮重放只更新那一行，不再重复计。
-- F2：表里没有 estimated，上游不给 usage 时记 0/0，无法区分"真的是 0"与"没有数据"。
--     补一个 boolean 标记，服务端拿不到 usage 时用端侧同口径估算并置 true。

-- ============ F1：turn_id + (turn_id, round) 唯一 ============

ALTER TABLE usage_samples ADD COLUMN IF NOT EXISTS turn_id text;

-- 历史行兜底：W2b 之前的写入没有 turn_id，用 req_id 顶上（req_id 是一次请求的唯一标识）
UPDATE usage_samples SET turn_id = req_id WHERE turn_id IS NULL;

ALTER TABLE usage_samples ALTER COLUMN turn_id SET NOT NULL;

-- 建唯一索引前先清掉历史重复（同一 req_id + round 只留最新一行）
DELETE FROM usage_samples older
    USING usage_samples newer
 WHERE older.id < newer.id
   AND older.turn_id = newer.turn_id
   AND older.round = newer.round;

CREATE UNIQUE INDEX IF NOT EXISTS ux_usage_turn_round ON usage_samples (turn_id, round);

-- ============ F2：estimated 标记 ============

ALTER TABLE usage_samples ADD COLUMN IF NOT EXISTS estimated boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN usage_samples.turn_id IS '一次用户提问（多轮工具循环共享）；与 round 一起构成幂等键';
COMMENT ON COLUMN usage_samples.estimated IS 'true = 上游没给 usage，这里的 token 数是服务端估算值';
