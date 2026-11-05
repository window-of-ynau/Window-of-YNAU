//! PostgreSQL 后端：所有 SQL 都在这里。
//!
//! 表结构见 `migrations/0001_init.sql` 与 `migrations/0002_auth.sql`；
//! 令牌**只落 SHA-256 摘要**，明文只在响应里出现一次。

use sqlx::{PgConnection, PgPool, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{
    error::AuthError, token, AuthSession, Principal, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS,
};

/// 刚签发的一对令牌（明文只留在内存里，落库的是摘要）。
struct IssuedPair {
    access_token: String,
    refresh_token: String,
}

/// 注册：邮箱唯一索引冲突 → 422，而不是 500。
pub(crate) async fn register(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<AuthSession, AuthError> {
    let inserted =
        sqlx::query("INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id")
            .bind(email)
            .bind(password_hash)
            .fetch_one(pool)
            .await;

    let user_id: Uuid = match inserted {
        Ok(row) => row.try_get("id").map_err(row_error)?,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(AuthError::validation("该邮箱已注册"));
        }
        Err(error) => return Err(db_error(error)),
    };

    issue_session(pool, user_id, None).await
}

/// 发放一个邀请码（只落哈希）。脚本与运维复用这一条 SQL。
pub(crate) async fn insert_invite(
    pool: &PgPool,
    code_hash: &str,
    batch: &str,
    issued_by: &str,
    expires_at: Option<OffsetDateTime>,
    max_uses: i32,
    note: Option<&str>,
) -> Result<(), AuthError> {
    sqlx::query(
        "INSERT INTO invite_codes (code_hash, batch, issued_by, expires_at, max_uses, note) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(code_hash)
    .bind(batch)
    .bind(issued_by)
    .bind(expires_at)
    .bind(max_uses)
    .bind(note)
    .execute(pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 按批次撤销（返回本次真正撤销的张数；已撤销的不重复计数）。
pub(crate) async fn revoke_invite_batch(pool: &PgPool, batch: &str) -> Result<u64, AuthError> {
    let affected = sqlx::query(
        "UPDATE invite_codes SET revoked_at = now() \
         WHERE batch = $1 AND revoked_at IS NULL",
    )
    .bind(batch)
    .execute(pool)
    .await
    .map_err(db_error)?
    .rows_affected();
    Ok(affected)
}

/// 某批次的用量：`(已用, 总容量, 张数)`。
pub(crate) async fn invite_batch_stats(
    pool: &PgPool,
    batch: &str,
) -> Result<(i64, i64, i64), AuthError> {
    let row = sqlx::query(
        "SELECT coalesce(sum(used_count), 0)::bigint AS used, \
                coalesce(sum(max_uses), 0)::bigint AS capacity, \
                count(*)::bigint AS codes \
         FROM invite_codes WHERE batch = $1",
    )
    .bind(batch)
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    let used = row.try_get("used").map_err(row_error)?;
    let capacity = row.try_get("capacity").map_err(row_error)?;
    let codes = row.try_get("codes").map_err(row_error)?;
    Ok((used, capacity, codes))
}

/// 一次邀请码核销的上下文（哈希已在 service 层算好，明文不进这一层）。
pub(crate) struct InviteUse<'a> {
    pub code_hash: &'a str,
    pub ip_hash: Option<&'a str>,
    pub ua_hash: Option<&'a str>,
}

/// 带邀请码的注册：**核销与建号在同一个事务里**（t141）。
///
/// 原子性体现在三处：
/// 1. 核销用一条 `UPDATE ... WHERE used_count < max_uses RETURNING` 完成
///    "存在 / 未撤销 / 未过期 / 未用完"四道检查与自增——不是"先查后写"；
/// 2. 邮箱撞唯一约束时**整个事务回滚**，邀请码不会被白吃（否则攻击者用已注册邮箱就能烧掉别人的码）；
/// 3. 令牌与核销记录同事务提交，不会出现"账号建了但没记账"或"记账了但没建号"。
pub(crate) async fn register_with_invite(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
    invite: InviteUse<'_>,
) -> Result<AuthSession, AuthError> {
    let mut tx = pool.begin().await.map_err(db_error)?;

    let batch: Option<String> = sqlx::query_scalar(
        "UPDATE invite_codes SET used_count = used_count + 1 \
         WHERE code_hash = $1 \
           AND revoked_at IS NULL \
           AND (expires_at IS NULL OR expires_at > now()) \
           AND used_count < max_uses \
         RETURNING batch",
    )
    .bind(invite.code_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;

    let Some(batch) = batch else {
        // 不存在 / 已撤销 / 已过期 / 用量已满 —— 对客户端**合并成同一个错误**（防枚举）
        tx.rollback().await.ok();
        return Err(AuthError::invite_invalid());
    };

    let inserted = sqlx::query(
        "INSERT INTO users (email, password_hash, auth_method) VALUES ($1, $2, 'invite') \
         RETURNING id",
    )
    .bind(email)
    .bind(password_hash)
    .fetch_one(&mut *tx)
    .await;

    let user_id: Uuid = match inserted {
        Ok(row) => row.try_get("id").map_err(row_error)?,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            tx.rollback().await.ok();
            return Err(AuthError::validation("该邮箱已注册"));
        }
        Err(error) => return Err(db_error(error)),
    };

    sqlx::query(
        "INSERT INTO invite_redemptions (user_id, code_hash, ip_hash, ua_hash) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(invite.code_hash)
    .bind(invite.ip_hash)
    .bind(invite.ua_hash)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;

    let issued = insert_pair(&mut tx, user_id, None).await?;
    tx.commit().await.map_err(db_error)?;

    tracing::info!(
        batch = %batch,
        user_id = %user_id,
        "邀请码注册成功"
    );

    Ok(AuthSession {
        user_id,
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        expires_in: ACCESS_TOKEN_TTL_SECS,
    })
}

/// 登录第一步：只查"这个邮箱的哈希串"，**不做** Argon2 校验。
///
/// 为什么拆开：Argon2 是同步的 CPU/内存密集计算，直接在 async fn 里做会占住 runtime
/// worker（实测并发登录时 `/healthz` 被拖到 632ms）。现在由 `AuthService::login`
/// 把校验交给 `spawn_blocking`，这里只负责一次查询。
///
/// 返回 `None` 表示"邮箱不存在或已禁用"；"存在但没有密码"（如第三方账号）返回 `Some(.., None)`。
/// 两种情况由调用方统一回 401 且文案一致，不泄露账号是否存在。
pub(crate) async fn password_hash(
    pool: &PgPool,
    email: &str,
) -> Result<Option<(Uuid, Option<String>)>, AuthError> {
    let found = sqlx::query(
        "SELECT id, password_hash FROM users WHERE lower(email) = lower($1) AND disabled_at IS NULL",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;

    let Some(row) = found else {
        return Ok(None);
    };

    let user_id: Uuid = row.try_get("id").map_err(row_error)?;
    let stored: Option<String> = row.try_get("password_hash").map_err(row_error)?;
    Ok(Some((user_id, stored)))
}

/// 校验访问令牌。
pub(crate) async fn authenticate(
    pool: &PgPool,
    access_token: &str,
) -> Result<Principal, AuthError> {
    let token_hash = token::hash(access_token);
    // H1（t134）：令牌有效 ≠ 账号可用。禁用（毕业/封禁）后必须立刻失去访问权，
    // 所以这里的 join 带上 `disabled_at IS NULL`——旧实现只在 login 查了它。
    let found = sqlx::query(
        "SELECT t.user_id, t.device_id, t.expires_at, t.revoked_at \
         FROM access_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = $1 AND u.disabled_at IS NULL",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;

    let Some(row) = found else {
        return Err(AuthError::unauthorized_with("访问令牌无效"));
    };

    let revoked_at: Option<OffsetDateTime> = row.try_get("revoked_at").map_err(row_error)?;
    if revoked_at.is_some() {
        return Err(AuthError::unauthorized_with("访问令牌已吊销"));
    }
    let expires_at: OffsetDateTime = row.try_get("expires_at").map_err(row_error)?;
    if expires_at <= OffsetDateTime::now_utc() {
        return Err(AuthError::token_expired());
    }

    Ok(Principal {
        user_id: row.try_get("user_id").map_err(row_error)?,
        device_id: row.try_get("device_id").map_err(row_error)?,
        token_hash,
    })
}

/// 轮换刷新令牌：一个事务里"锁定旧令牌 → 吊销 → 发新的一对"。
pub(crate) async fn refresh(pool: &PgPool, refresh_token: &str) -> Result<AuthSession, AuthError> {
    let token_hash = token::hash(refresh_token);
    let mut tx = pool.begin().await.map_err(db_error)?;

    // H1（t134）：禁用的账号**不能续命**。旧实现只校验令牌本身，
    // 于是 30 天有效的 refresh 可以把已禁用账号无限续下去（ADR-11 的"毕业即降级"失效）。
    let found = sqlx::query(
        "SELECT t.user_id, t.device_id, t.expires_at, t.revoked_at \
         FROM refresh_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = $1 AND u.disabled_at IS NULL FOR UPDATE OF t",
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;

    let Some(row) = found else {
        return Err(AuthError::unauthorized_with("刷新令牌无效"));
    };

    let revoked_at: Option<OffsetDateTime> = row.try_get("revoked_at").map_err(row_error)?;
    if revoked_at.is_some() {
        return Err(AuthError::unauthorized_with("刷新令牌已失效，请重新登录"));
    }
    let expires_at: OffsetDateTime = row.try_get("expires_at").map_err(row_error)?;
    if expires_at <= OffsetDateTime::now_utc() {
        return Err(AuthError::unauthorized_with("刷新令牌已过期，请重新登录"));
    }

    let user_id: Uuid = row.try_get("user_id").map_err(row_error)?;
    let device_id: Option<Uuid> = row.try_get("device_id").map_err(row_error)?;

    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() WHERE token_hash = $1")
        .bind(&token_hash)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;

    let issued = insert_pair(&mut tx, user_id, device_id).await?;
    tx.commit().await.map_err(db_error)?;

    Ok(AuthSession {
        user_id,
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        expires_in: ACCESS_TOKEN_TTL_SECS,
    })
}

/// 停用一个账号（ADR-11 的"毕业即降级"/封禁入口）。
///
/// **禁用即吊销**：同一个事务里把该账号所有未失效的 access/refresh 令牌都吊销。
/// 只在库里改 `disabled_at` 也能达到同样效果（迁移 0008 有触发器兜底），
/// 但应用侧有这条路径可以显式表达意图、并且返回值能告诉调用方"是不是真的改了"。
pub(crate) async fn disable_user(pool: &PgPool, user_id: Uuid) -> Result<bool, AuthError> {
    let mut tx = pool.begin().await.map_err(db_error)?;

    let changed =
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1 AND disabled_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();

    revoke_all_tokens(&mut tx, user_id).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(changed == 1)
}

/// 吊销一个账号的**全部**令牌（access + refresh）。
async fn revoke_all_tokens(conn: &mut PgConnection, user_id: Uuid) -> Result<(), AuthError> {
    sqlx::query(
        "UPDATE access_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 登出：吊销当前访问令牌 + 该账号全部未失效的刷新令牌。
pub(crate) async fn logout(pool: &PgPool, principal: &Principal) -> Result<(), AuthError> {
    let mut tx = pool.begin().await.map_err(db_error)?;

    sqlx::query(
        "UPDATE access_tokens SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&principal.token_hash)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(principal.user_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;

    tx.commit().await.map_err(db_error)?;
    Ok(())
}

/// 发一对新令牌。
pub(crate) async fn issue_session(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Option<Uuid>,
) -> Result<AuthSession, AuthError> {
    let mut conn = pool.acquire().await.map_err(db_error)?;
    let issued = insert_pair(&mut conn, user_id, device_id).await?;
    Ok(AuthSession {
        user_id,
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        expires_in: ACCESS_TOKEN_TTL_SECS,
    })
}

/// 往两张令牌表各插一行（只插摘要）。
async fn insert_pair(
    conn: &mut PgConnection,
    user_id: Uuid,
    device_id: Option<Uuid>,
) -> Result<IssuedPair, AuthError> {
    let access_token = token::generate();
    let refresh_token = token::generate();
    let now = OffsetDateTime::now_utc();

    sqlx::query(
        "INSERT INTO access_tokens (user_id, device_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(device_id)
    .bind(token::hash(&access_token))
    .bind(now + Duration::seconds(ACCESS_TOKEN_TTL_SECS))
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;

    sqlx::query(
        "INSERT INTO refresh_tokens (user_id, device_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(device_id)
    .bind(token::hash(&refresh_token))
    .bind(now + Duration::seconds(REFRESH_TOKEN_TTL_SECS))
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;

    Ok(IssuedPair {
        access_token,
        refresh_token,
    })
}

/// 查询/写入失败：记日志，对外只给一句笼统的话。
fn db_error(error: sqlx::Error) -> AuthError {
    tracing::error!(%error, "鉴权数据库操作失败");
    AuthError::internal("鉴权数据库操作失败")
}

fn row_error(error: sqlx::Error) -> AuthError {
    tracing::error!(%error, "鉴权数据字段读取失败");
    AuthError::internal("鉴权数据字段读取失败")
}
