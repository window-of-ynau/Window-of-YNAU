-- t141 / ADR-11（2026-11 修订）：邀请制注册。
--
-- 1.0 的身份方案**不碰校方身份接口**：校方域名有动态 JS 防护，云端无法校验校方会话，
-- 响应里也不返回学号（无法证明"令牌属于该学号"）。所以凭邀请码注册，校方登录态只用于端侧取数。
--
-- 隐私与安全约定：
--   * 邀请码**只存 SHA-256 哈希**，明文只在生成时打印一次；
--   * 核销记录里**不存明文 IP / UA**，只存哈希（能对"同一个人重复核销"做关联，但拿不到原文）；
--   * 失败原因对客户端**不区分**（存在/过期/撤销/用完都一个错误码），避免邀请码枚举。

-- ---------------------------------------------------------------------------
-- users.auth_method：账号来源（1.0 全是 invite；另外两个值只占位，不实现流程）
-- ---------------------------------------------------------------------------
ALTER TABLE users ADD COLUMN IF NOT EXISTS auth_method TEXT NOT NULL DEFAULT 'invite';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'users_auth_method_check'
          AND conrelid = 'users'::regclass
    ) THEN
        ALTER TABLE users
            ADD CONSTRAINT users_auth_method_check
            CHECK (auth_method IN ('invite', 'school_sso', 'oauth'));
    END IF;
END
$$;

COMMENT ON COLUMN users.auth_method IS
    't141：账号来源。invite=邀请码注册（1.0 方案）；school_sso / oauth 仅占位（ADR-11 后续版本）。';

-- ---------------------------------------------------------------------------
-- 邀请码
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invite_codes (
    -- 邀请码的 SHA-256（十六进制小写）；**不存明文**
    code_hash  TEXT        PRIMARY KEY,
    -- 批次：便于"按批次停发"（一次招生/一次活动一个批次）
    batch      TEXT        NOT NULL,
    -- 发放人（管理员标识；不是用户外键，避免把运维账号绑进业务表）
    issued_by  TEXT        NOT NULL,
    issued_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- NULL = 永不过期（默认建议给个期限，脚本默认 30 天）
    expires_at TIMESTAMPTZ,
    -- 一码多用：默认 1（一次性）
    max_uses   INT         NOT NULL DEFAULT 1 CHECK (max_uses >= 1),
    used_count INT         NOT NULL DEFAULT 0 CHECK (used_count >= 0),
    revoked_at TIMESTAMPTZ,
    note       TEXT,
    -- 用量永远不能超过上限（并发下也必须成立；原子核销靠这条 + WHERE 条件双保险）
    CONSTRAINT invite_codes_used_within_max CHECK (used_count <= max_uses)
);

COMMENT ON TABLE invite_codes IS
    't141：邀请码。只存 SHA-256 哈希；used_count 通过 "UPDATE ... WHERE used_count < max_uses RETURNING" 原子递增。';

CREATE INDEX IF NOT EXISTS ix_invite_codes_batch ON invite_codes (batch);
CREATE INDEX IF NOT EXISTS ix_invite_codes_expires ON invite_codes (expires_at);

-- ---------------------------------------------------------------------------
-- 核销记录（追溯谁用了哪个码；IP/UA 只存哈希）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invite_redemptions (
    id          BIGSERIAL PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 不做外键：邀请码可能按保留期清理，而核销记录要留作审计
    code_hash   TEXT        NOT NULL,
    redeemed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- sha256(客户端 IP) / sha256(User-Agent)：能关联"同一个人"，但拿不到原文
    ip_hash     TEXT,
    ua_hash     TEXT
);

COMMENT ON TABLE invite_redemptions IS
    't141：邀请码核销记录。ip_hash/ua_hash 是 SHA-256（不存明文），用于发现"一个码被同一台机器反复用"。';

CREATE INDEX IF NOT EXISTS ix_invite_redemptions_code ON invite_redemptions (code_hash);
CREATE INDEX IF NOT EXISTS ix_invite_redemptions_user ON invite_redemptions (user_id);
