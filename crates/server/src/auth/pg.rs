//! PostgreSQL 后端：所有 SQL 都在这里。
//!
//! 表结构见 `migrations/0001_init.sql` 与 `migrations/0002_auth.sql`；
//! 令牌**只落 SHA-256 摘要**，明文只在响应里出现一次。

use sqlx::{PgConnection, PgPool, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{
    error::AuthError, password, token, AuthSession, Principal, ACCESS_TOKEN_TTL_SECS,
    REFRESH_TOKEN_TTL_SECS,
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

/// 登录：查用户 → Argon2id 校验 → 发令牌。
///
/// 邮箱不存在时也做一次哈希，让"存在"与"不存在"的耗时接近；
/// 两种情况返回**完全相同**的 401 文案，不泄露账号是否存在。
pub(crate) async fn login(
    pool: &PgPool,
    email: &str,
    password: &str,
) -> Result<AuthSession, AuthError> {
    let found = sqlx::query(
        "SELECT id, password_hash FROM users WHERE lower(email) = lower($1) AND disabled_at IS NULL",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;

    let Some(row) = found else {
        let _ = password::hash(password);
        return Err(AuthError::unauthorized_with("邮箱或密码不正确"));
    };

    let user_id: Uuid = row.try_get("id").map_err(row_error)?;
    let stored: Option<String> = row.try_get("password_hash").map_err(row_error)?;
    let matched = stored
        .as_deref()
        .is_some_and(|phc| password::verify(password, phc));
    if !matched {
        return Err(AuthError::unauthorized_with("邮箱或密码不正确"));
    }

    issue_session(pool, user_id, None).await
}

/// 校验访问令牌。
pub(crate) async fn authenticate(
    pool: &PgPool,
    access_token: &str,
) -> Result<Principal, AuthError> {
    let token_hash = token::hash(access_token);
    let found = sqlx::query(
        "SELECT user_id, device_id, expires_at, revoked_at \
         FROM access_tokens WHERE token_hash = $1",
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

    let found = sqlx::query(
        "SELECT user_id, device_id, expires_at, revoked_at \
         FROM refresh_tokens WHERE token_hash = $1 FOR UPDATE",
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
async fn issue_session(
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
