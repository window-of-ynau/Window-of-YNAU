-- 云农之窗 · RAG 增量管线（ADR-19 决策 3）
--
-- 背景：上报的是 `sha256(月盐 ‖ 规范化标识)`，服务端**无法反推**明文标识。
-- 所以抓取管线需要另一份输入：服务端把**公开可抓**的语料预先索引，运行时按当月盐现算哈希，
-- 与上报哈希在**内存里** join，命中后才谈得上抓取。
--
-- ⚠️ 关键设计（captain 裁决，风险 1）：**这里不存 hash 列**。
-- 存 `(month, hash → identifier)` 等于在库里放一本"当月去匿名化字典"——任何有读库权限的人
-- 都能把当月任意一条上报哈希翻成明文，而"无 user_id / 只对内 / 定期清理"防的是误用、
-- 不防有人直接拿这本字典查。
--
-- 因此：静态数据里**不存在任何哈希→明文的映射**，只有 worker 运行期的瞬时映射
-- （按当月盐现算、join 完即丢）。代价是 join 要全表扫描——但已知语料是万级到十万级，
-- 当月盐算 10 万个 sha256 是毫秒级，完全可接受。
--
-- 隐私红线（务必遵守）：
--   1. 表里**不许有 user_id**（本文件没有，也请永远不要加）；
--   2. 任何 API 都不得暴露"标识 → 是否被上报过"的映射；
--   3. 只索引公开学术资源，绝不放用户输入的文本；
--   4. 按 `RAG_KNOWN_DOCS_RETENTION_MONTHS` 清理历史月份（索引 CLI 会顺手做）。

CREATE TABLE IF NOT EXISTS rag_known_docs (
    month      TEXT        NOT NULL,
    identifier TEXT        NOT NULL,
    source     TEXT        NOT NULL,
    fetched_at TIMESTAMPTZ,
    doc_text   TEXT,
    PRIMARY KEY (month, identifier)
);

COMMENT ON TABLE rag_known_docs IS
    '已知语料的月度索引（ADR-19 决策 3）。**不存 hash**：hash 只在 worker 运行期按当月盐现算。'
    '无 user_id；只对内使用；历史月份按月清理。';
COMMENT ON COLUMN rag_known_docs.identifier IS '规范化后的公开标识（如 2401.01234、10.1234/x）';
COMMENT ON COLUMN rag_known_docs.source     IS '抓取分派用的来源：arxiv / doi / web …';
COMMENT ON COLUMN rag_known_docs.doc_text   IS '抓取并清洗后的正文（NULL = 尚未抓取）';

CREATE INDEX IF NOT EXISTS ix_rag_known_docs_source
    ON rag_known_docs (source);

-- 抓取 worker 每次启动都要把本月语料读进内存算哈希：按 month 建索引
CREATE INDEX IF NOT EXISTS ix_rag_known_docs_month
    ON rag_known_docs (month);

-- ---------------------------------------------------------------------------
-- 候选表：补上抓取状态机需要的临时态与诊断列
-- ---------------------------------------------------------------------------
-- 为什么要有 `fetching`：抓取是网络 IO，不能在一个事务里挂着；
-- 所以用"抢占 + 落结果"两段式：pending → fetching（抢占成功）→ fetched/failed。
-- 多 worker 同时跑时，`UPDATE ... WHERE status='pending'` 保证只有一个抢到。
ALTER TABLE rag_candidates ADD COLUMN IF NOT EXISTS attempts   INT  NOT NULL DEFAULT 0;
ALTER TABLE rag_candidates ADD COLUMN IF NOT EXISTS last_error TEXT;

ALTER TABLE rag_candidates DROP CONSTRAINT IF EXISTS rag_candidates_status_check;
ALTER TABLE rag_candidates
    ADD CONSTRAINT rag_candidates_status_check
    CHECK (status IN ('pending', 'fetching', 'fetched', 'failed', 'skipped'));

-- 抓取 worker 的取数路径：本月 pending / failed 按热度
CREATE INDEX IF NOT EXISTS ix_rag_candidates_month_status_count
    ON rag_candidates (month, status, count DESC);
