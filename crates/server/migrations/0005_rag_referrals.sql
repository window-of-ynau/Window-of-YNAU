-- 云农之窗 · RAG 2.0 服务端地基（ADR-19 决策 2/3）
--
-- 三张表 + 一个运维视图，全部幂等（`IF NOT EXISTS` / `CREATE OR REPLACE` / 先 DROP 再 ADD 约束），
-- 所以：新库首次迁移、老库升级、迁移重入都走同一份文件。
--
-- 隐私边界（ADR-19）：这里**没有 user_id 列**，也不该有。
--   客户端上报的是"月度盐 + 词条"的匿名哈希；服务端只做聚合，
--   因此无法把某一行回溯到某个账号。上报审计里也只记条数，不记 hash 内容。
--
-- 为什么盐要落库：盐是"同月哈希可聚合"的前提。如果重启换盐，
--   同一个月内客户端算出来的哈希就对不上，聚合数据会永久碎片化。

-- ---------------------------------------------------------------------------
-- 月度盐：一行一个月，永不删除（历史月份的哈希要靠它才能核对）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS rag_salts (
    month      TEXT        PRIMARY KEY,
    salt       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE rag_salts IS
    'RAG 月度盐（ADR-19 决策 2）。盐不是秘密（下发给客户端），但必须由服务端统一下发，'
    '且同月内不可变，否则客户端算出的哈希无法聚合。';

-- ---------------------------------------------------------------------------
-- 聚合表：同一 (month, hash) 多行累加成一行
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS rag_referrals (
    month      TEXT        NOT NULL,
    hash       TEXT        NOT NULL,
    count      BIGINT      NOT NULL DEFAULT 0,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (month, hash)
);

COMMENT ON TABLE rag_referrals IS
    '匿名哈希的累计提及次数（ADR-19 决策 2/3）。无 user_id：服务端不保留"谁上传了哪条"。';

-- t81 / M2：**永不 DROP**。旧写法在 `psql -f`（非单事务）下重跑时，
-- DROP 先生效、ADD 再校验现有行失败 ⇒ 约束永久丢失（库里有 fetching 行就是这种局面）。
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'rag_referrals_count_check'
          AND conrelid = 'rag_referrals'::regclass
    ) THEN
        ALTER TABLE rag_referrals
            ADD CONSTRAINT rag_referrals_count_check CHECK (count >= 0);
    END IF;
END
$$;

-- 运维最常查"本月提到最多的前 N 条"：按 (month, count DESC) 建索引
CREATE INDEX IF NOT EXISTS ix_rag_referrals_month_count
    ON rag_referrals (month, count DESC);

-- ---------------------------------------------------------------------------
-- 候选表：抓取管线的状态机（本期只落 pending，抓取后续排期）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS rag_candidates (
    month      TEXT        NOT NULL,
    hash       TEXT        NOT NULL,
    count      BIGINT      NOT NULL DEFAULT 0,
    status     TEXT        NOT NULL DEFAULT 'pending',
    fetched_at TIMESTAMPTZ,
    doc_id     TEXT,
    PRIMARY KEY (month, hash)
);

COMMENT ON TABLE rag_candidates IS
    '抓取候选与结果（ADR-19 决策 3）。status: pending / fetched / failed / skipped。';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'rag_candidates_count_check'
          AND conrelid = 'rag_candidates'::regclass
    ) THEN
        ALTER TABLE rag_candidates
            ADD CONSTRAINT rag_candidates_count_check CHECK (count >= 0);
    END IF;
END
$$;

-- t81 / M2（最严重的一条）：**只在缺约束时 ADD，永不 DROP**。
--
-- 旧写法"先 DROP 再 ADD"在重跑时有两个坑：
--   1. `psql -f` 非单事务：DROP 已生效、ADD 因**已存在的 fetching 行**失败 ⇒ 约束永久丢失，
--      之后能插任意 `weird_status`，脏行还会让 0006 一起跑不过；
--   2. 即使单事务回滚，也把"扩状态值"这件事和"重入"耦在一起，判断依据不可靠。
--
-- 这里只保证 4 值版本存在；把 `fetching` 加进来是 0006 的职责（它同样只在需要时重建）。
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'rag_candidates_status_check'
          AND conrelid = 'rag_candidates'::regclass
    ) THEN
        ALTER TABLE rag_candidates
            ADD CONSTRAINT rag_candidates_status_check
            CHECK (status IN ('pending', 'fetched', 'failed', 'skipped'));
    END IF;
END
$$;

-- 抓取 worker 的取数路径：按 status + 热度
CREATE INDEX IF NOT EXISTS ix_rag_candidates_status_count
    ON rag_candidates (status, count DESC);

-- ---------------------------------------------------------------------------
-- 运维视图：每月 Top N（带名次），供 psql / 报表直接查
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW rag_top_referrals AS
SELECT
    month,
    hash,
    count,
    first_seen,
    last_seen,
    row_number() OVER (PARTITION BY month ORDER BY count DESC, hash) AS rank
FROM rag_referrals;

COMMENT ON VIEW rag_top_referrals IS
    '每月按提及次数排名的哈希（rank=1 是最高）。运维查询：SELECT * FROM rag_top_referrals WHERE month = ''2026-11'' AND rank <= 50;';
