-- 云农之窗 · 初始 schema（ADR-02 / ADR-08 / ADR-11）
--
-- 三条铁律：
--   1. 这里**没有** messages / memory / blobs / 课表等表 —— 会话正文与隐私数据只在端上。
--   2. 这里**没有** 学校凭据、也没有用户的上游 API Key —— 凭据不出端。
--   3. usage_samples 是 1% 抽样，用于成本监控，**不是账单**。

CREATE EXTENSION IF NOT EXISTS vector;

-- ============ 账号与设备 ============

CREATE TABLE IF NOT EXISTS users (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email       text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_users_email_lower ON users (lower(email));

CREATE TABLE IF NOT EXISTS devices (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    platform    text NOT NULL CHECK (platform IN ('android', 'windows', 'web')),
    name        text,
    pubkey      text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz
);
CREATE INDEX IF NOT EXISTS ix_devices_user ON devices (user_id);

CREATE TABLE IF NOT EXISTS refresh_tokens (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id  uuid REFERENCES devices (id) ON DELETE CASCADE,
    token_hash text NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_refresh_user ON refresh_tokens (user_id, expires_at DESC);

-- ============ 授权（按时间，不按 token） ============

CREATE TABLE IF NOT EXISTS plans (
    code        text PRIMARY KEY,
    name        text    NOT NULL,
    valid_days  integer NOT NULL CHECK (valid_days > 0),
    price_cents integer NOT NULL CHECK (price_cents >= 0),
    active      boolean NOT NULL DEFAULT true
);

CREATE TABLE IF NOT EXISTS entitlements (
    id         bigserial PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    plan_code  text NOT NULL REFERENCES plans (code),
    starts_at  timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    source     text NOT NULL CHECK (source IN ('redeem', 'admin', 'payment')),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_entitlements_user ON entitlements (user_id, expires_at DESC);

CREATE TABLE IF NOT EXISTS redeem_codes (
    code        text PRIMARY KEY,
    plan_code   text NOT NULL REFERENCES plans (code),
    valid_days  integer NOT NULL CHECK (valid_days > 0),
    used_by     uuid REFERENCES users (id),
    used_at     timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- ============ 知识库（唯一可以放内容的表） ============

CREATE TABLE IF NOT EXISTS knowledge_docs (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    title      text NOT NULL,
    source_url text,
    license    text,
    status     text NOT NULL DEFAULT 'pending'
               CHECK (status IN ('pending', 'approved', 'rejected')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_knowledge_docs_status ON knowledge_docs (status, created_at DESC);

CREATE TABLE IF NOT EXISTS knowledge_chunks (
    id        bigserial PRIMARY KEY,
    doc_id    uuid NOT NULL REFERENCES knowledge_docs (id) ON DELETE CASCADE,
    ordinal   integer NOT NULL,
    content   text NOT NULL,
    tsv       tsvector,
    embedding vector(1024)
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_chunks_doc_ordinal ON knowledge_chunks (doc_id, ordinal);
CREATE INDEX IF NOT EXISTS ix_chunks_tsv ON knowledge_chunks USING GIN (tsv);
CREATE INDEX IF NOT EXISTS ix_chunks_vec ON knowledge_chunks
    USING hnsw (embedding vector_cosine_ops);

-- ============ 抽样用量与审计 ============

CREATE TABLE IF NOT EXISTS usage_samples (
    id         bigserial PRIMARY KEY,
    user_id    uuid REFERENCES users (id) ON DELETE SET NULL,
    req_id     text NOT NULL,
    model      text NOT NULL,
    protocol   text NOT NULL,
    channel    text NOT NULL CHECK (channel IN ('byok', 'plan')),
    round      smallint NOT NULL DEFAULT 1,
    tokens_in  integer NOT NULL DEFAULT 0,
    tokens_out integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_usage_user_time ON usage_samples (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS ix_usage_time ON usage_samples (created_at DESC);

CREATE TABLE IF NOT EXISTS audit_logs (
    id         bigserial PRIMARY KEY,
    user_id    uuid REFERENCES users (id) ON DELETE SET NULL,
    action     text NOT NULL,
    target     text,
    decision   text NOT NULL CHECK (decision IN ('allow', 'deny')),
    reason     text,
    req_id     text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_audit_time ON audit_logs (created_at DESC);

-- ============ 初始套餐（示例，上线前替换） ============

INSERT INTO plans (code, name, valid_days, price_cents)
VALUES
    ('plan_30d', '30 天不限量', 30, 990),
    ('plan_90d', '90 天不限量', 90, 2490)
ON CONFLICT (code) DO NOTHING;
