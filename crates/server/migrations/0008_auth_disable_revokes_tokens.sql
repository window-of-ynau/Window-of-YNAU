-- t134 / H1：禁用即吊销（数据库侧兜底）。
--
-- 应用侧已经有 `pg::disable_user`（同一事务里停用 + 吊销全部令牌），
-- 但运维/ADP（ADR-11 的"毕业即降级"）很可能是**直接写 SQL**：
--     UPDATE users SET disabled_at = now() WHERE id = ...;
-- 这种情况下如果只靠应用代码，那些已经签发出去的 access/refresh 会继续有效
-- （refresh 还能把账号无限续命）。所以在库上加一个触发器，让"禁用"这件事
-- **不管从哪条路进来**都同时吊销令牌。
--
-- 幂等：`CREATE OR REPLACE FUNCTION` + `DROP TRIGGER IF EXISTS` + `CREATE TRIGGER`。
-- 只响应 `disabled_at` 从 NULL 变为非 NULL 的那一次（重复禁用不会反复写令牌表）。

CREATE OR REPLACE FUNCTION auth_revoke_tokens_on_disable()
RETURNS trigger AS $$
BEGIN
    IF NEW.disabled_at IS NOT NULL AND OLD.disabled_at IS NULL THEN
        UPDATE access_tokens
           SET revoked_at = now()
         WHERE user_id = NEW.id AND revoked_at IS NULL;
        UPDATE refresh_tokens
           SET revoked_at = now()
         WHERE user_id = NEW.id AND revoked_at IS NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_auth_revoke_tokens_on_disable ON users;

CREATE TRIGGER trg_auth_revoke_tokens_on_disable
    AFTER UPDATE OF disabled_at ON users
    FOR EACH ROW
    EXECUTE FUNCTION auth_revoke_tokens_on_disable();

COMMENT ON FUNCTION auth_revoke_tokens_on_disable() IS
    'H1：users.disabled_at 被置为非空时，吊销该账号全部 access/refresh 令牌（禁用即吊销）。';
