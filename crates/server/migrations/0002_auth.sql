-- 云农之窗 · 鉴权（W2 / ADR-11）
--
-- 两条铁律：
--   1. 密码只存 Argon2id 的 PHC 字符串，永不存明文、永不可逆；
--   2. 令牌只存 SHA-256 摘要：库被读走也换不出可用令牌。
--
-- 0001 已经有 users / devices / refresh_tokens，这里只补：
--   - users.password_hash 列（账号系统的密码哈希）
--   - access_tokens 表（可吊销的访问令牌；refresh_tokens 已在 0001）

ALTER TABLE users
    ADD COLUMN IF NOT EXISTS password_hash text;

CREATE TABLE IF NOT EXISTS access_tokens (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- 设备维度可空：W2 的注册/登录还不区分设备
    device_id   uuid REFERENCES devices (id) ON DELETE CASCADE,
    -- 只存 SHA-256 hex（64 字符），不存明文令牌
    token_hash  text NOT NULL,
    expires_at  timestamptz NOT NULL,
    revoked_at  timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- 鉴权查询的主索引，同时保证摘要不重复
CREATE UNIQUE INDEX IF NOT EXISTS ux_access_tokens_hash ON access_tokens (token_hash);

-- 按用户吊销（登出全部设备）与按用户查令牌
CREATE INDEX IF NOT EXISTS ix_access_tokens_user ON access_tokens (user_id, expires_at DESC);

-- 过期清理任务用：只看"未吊销"的行
CREATE INDEX IF NOT EXISTS ix_access_tokens_expiry
    ON access_tokens (expires_at) WHERE revoked_at IS NULL;
