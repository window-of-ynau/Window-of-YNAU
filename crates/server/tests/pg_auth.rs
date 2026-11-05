//! PostgreSQL 集成测试：让 `auth::pg` 里的**真实 SQL** 真的跑一遍。
//!
//! 默认 `cargo test` **不会跑这里**：每条用例都带 `#[ignore]`，没装 PostgreSQL 的机器
//! 依然全绿。开启方式与环境约定见 [`common`]（`tests/common/mod.rs`）：
//! 有 `DATABASE_URL` 就必须跑通（缺库 / 缺 pgvector 都算失败），没有就优雅跳过。
//!
//! 这里只放 auth 的 SQL 用例；每条用例独占一个 schema，跑的是真实 `migrations/*.sql`。

mod common;

use sqlx::postgres::PgPool;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use common::{count, sha256_hex, TestDb};
use yunong_server::auth::{AuthService, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS};
use yunong_shared::ErrorCode;

const EMAIL: &str = "student@ynau.edu.cn";
const PASSWORD: &str = "correct-horse-battery";

/// 一张令牌表里的一行（只取断言要用的列）。
struct TokenRow {
    user_id: Uuid,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

async fn token_row(pool: &PgPool, table: &str, token_hash: &str) -> Option<TokenRow> {
    assert!(
        matches!(table, "access_tokens" | "refresh_tokens"),
        "只允许这两张令牌表：{table}"
    );
    let sql = format!("SELECT user_id, expires_at, revoked_at FROM {table} WHERE token_hash = $1");
    sqlx::query(&sql)
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|error| panic!("查 {table} 失败：{error}"))
        .map(|row| TokenRow {
            user_id: row.get("user_id"),
            expires_at: row.get("expires_at"),
            revoked_at: row.get("revoked_at"),
        })
}

// ============ 注册 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn register_writes_argon2id_hash_and_only_token_digests() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    assert_eq!(
        auth.backend_kind(),
        "postgres",
        "这条用例必须跑在 Pg 后端上，否则等于什么都没测"
    );

    // 大小写与空白都由服务层规范化：落库必须是 lower(email)，与唯一索引一致
    let session = auth
        .register("  Student@YNAU.edu.cn ", PASSWORD)
        .await
        .expect("注册应成功");
    assert_eq!(session.expires_in, ACCESS_TOKEN_TTL_SECS);

    let user = sqlx::query("SELECT id, email, password_hash FROM users")
        .fetch_one(&db.pool)
        .await
        .expect("应能从库里读出刚注册的用户");
    assert_eq!(user.get::<Uuid, _>("id"), session.user_id);
    assert_eq!(user.get::<String, _>("email"), EMAIL);
    let phc: String = user.get("password_hash");
    assert!(
        phc.starts_with("$argon2id$"),
        "密码只存 Argon2id PHC：{phc}"
    );
    assert!(!phc.contains(PASSWORD), "库里不能出现明文密码");

    // 访问令牌：只落 SHA-256 摘要，有效期 15 分钟
    let access = token_row(
        &db.pool,
        "access_tokens",
        &sha256_hex(&session.access_token),
    )
    .await
    .expect("访问令牌摘要应已落库");
    assert_eq!(access.user_id, session.user_id);
    assert!(access.revoked_at.is_none());
    let access_ttl = (access.expires_at - OffsetDateTime::now_utc()).whole_seconds();
    assert!(
        (ACCESS_TOKEN_TTL_SECS - 120..=ACCESS_TOKEN_TTL_SECS).contains(&access_ttl),
        "访问令牌有效期应约 {ACCESS_TOKEN_TTL_SECS}s，实际 {access_ttl}s"
    );

    // 刷新令牌：30 天
    let refresh = token_row(
        &db.pool,
        "refresh_tokens",
        &sha256_hex(&session.refresh_token),
    )
    .await
    .expect("刷新令牌摘要应已落库");
    assert!(refresh.revoked_at.is_none());
    let refresh_ttl = (refresh.expires_at - OffsetDateTime::now_utc()).whole_seconds();
    assert!(
        (REFRESH_TOKEN_TTL_SECS - 120..=REFRESH_TOKEN_TTL_SECS).contains(&refresh_ttl),
        "刷新令牌有效期应约 {REFRESH_TOKEN_TTL_SECS}s，实际 {refresh_ttl}s"
    );

    // 明文令牌一次都不许落库
    for (table, token) in [
        ("access_tokens", session.access_token.as_str()),
        ("refresh_tokens", session.refresh_token.as_str()),
    ] {
        let sql = format!("SELECT count(*) FROM {table} WHERE token_hash = $1");
        let hits: i64 = sqlx::query_scalar(&sql)
            .bind(token)
            .fetch_one(&db.pool)
            .await
            .expect("查明文令牌");
        assert_eq!(hits, 0, "{table} 里不许出现明文令牌");
    }

    db.finish().await;
}

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn duplicate_email_is_validation_error_not_internal() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    auth.register(EMAIL, PASSWORD)
        .await
        .expect("首次注册应成功");

    let error = auth
        .register(&EMAIL.to_ascii_uppercase(), PASSWORD)
        .await
        .expect_err("同邮箱第二次注册必须失败");
    // 唯一索引冲突要映射成 422（`pg::register` 的 is_unique_violation 分支），而不是 500
    assert_eq!(error.code, ErrorCode::ValidationError, "{}", error.message);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM users").await, 1);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        1,
        "失败路径不能再签发令牌"
    );

    db.finish().await;
}

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn concurrent_register_of_same_email_has_exactly_one_winner() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    let (first, second) = tokio::join!(
        auth.register(EMAIL, PASSWORD),
        auth.register(EMAIL, PASSWORD)
    );
    let winners = [first.is_ok(), second.is_ok()]
        .iter()
        .filter(|won| **won)
        .count();
    assert_eq!(
        winners, 1,
        "唯一索引只允许一个插入成功：{first:?} / {second:?}"
    );
    let loser = match first {
        Err(error) => error,
        Ok(_) => second.expect_err("必须有一个失败"),
    };
    assert_eq!(loser.code, ErrorCode::ValidationError, "{}", loser.message);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM users").await, 1);

    db.finish().await;
}

// ============ 登录 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn login_reads_user_row_and_reissues_a_fresh_pair() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    let registered = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");
    // 登录时邮箱同样规范化：大小写不影响命中
    let logged_in = auth
        .login(&EMAIL.to_ascii_uppercase(), PASSWORD)
        .await
        .expect("登录应成功");
    assert_eq!(logged_in.user_id, registered.user_id);
    assert_ne!(logged_in.access_token, registered.access_token);
    assert_ne!(logged_in.refresh_token, registered.refresh_token);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        2,
        "注册与登录各签发一对"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM refresh_tokens").await,
        2
    );

    // 密码错与账号不存在：同一个错误码、同一句话，且都不签发令牌
    let wrong_password = auth
        .login(EMAIL, "not-the-password")
        .await
        .expect_err("密码错必须失败");
    let unknown_user = auth
        .login("nobody@ynau.edu.cn", PASSWORD)
        .await
        .expect_err("账号不存在必须失败");
    assert_eq!(wrong_password.code, ErrorCode::Unauthorized);
    assert_eq!(unknown_user.code, ErrorCode::Unauthorized);
    assert_eq!(
        wrong_password.message, unknown_user.message,
        "不能泄露账号是否存在"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        2,
        "登录失败不能再签发令牌"
    );

    // `disabled_at IS NULL` 这条过滤必须真的生效
    let disabled = sqlx::query("UPDATE users SET disabled_at = now()")
        .execute(&db.pool)
        .await
        .expect("停用用户");
    assert_eq!(disabled.rows_affected(), 1, "应正好停用一个用户");
    let error = auth
        .login(EMAIL, PASSWORD)
        .await
        .expect_err("停用后不允许登录");
    assert_eq!(error.code, ErrorCode::Unauthorized);

    db.finish().await;
}

// ============ 访问令牌校验 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn authenticate_tells_expired_apart_from_revoked() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");

    let principal = auth
        .authenticate(&session.access_token)
        .await
        .expect("刚签发的访问令牌应可用");
    assert_eq!(principal.user_id, session.user_id);
    assert_eq!(principal.device_id, None, "W2 还没有设备维度");
    assert_eq!(principal.token_hash, sha256_hex(&session.access_token));

    // 库里没有、但格式合法的令牌 → 401
    let unknown = auth
        .authenticate(&"a".repeat(64))
        .await
        .expect_err("未知令牌必须失败");
    assert_eq!(unknown.code, ErrorCode::Unauthorized);

    // 过期 → TOKEN_EXPIRED：客户端据此走 refresh，而不是弹登录框
    let hash = sha256_hex(&session.access_token);
    let sql =
        "UPDATE access_tokens SET expires_at = now() - interval '1 second' WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&hash)
        .execute(&db.pool)
        .await
        .expect("把令牌改成过期");
    let expired = auth
        .authenticate(&session.access_token)
        .await
        .expect_err("过期令牌必须失败");
    assert_eq!(expired.code, ErrorCode::TokenExpired, "{}", expired.message);

    // 已吊销（且没过期）→ 401 而不是 TOKEN_EXPIRED：重试也好不了，得重新登录
    let sql = "UPDATE access_tokens \
               SET expires_at = now() + interval '15 minutes', revoked_at = now() \
               WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&hash)
        .execute(&db.pool)
        .await
        .expect("把令牌改成已吊销");
    let revoked = auth
        .authenticate(&session.access_token)
        .await
        .expect_err("已吊销令牌必须失败");
    assert_eq!(revoked.code, ErrorCode::Unauthorized, "{}", revoked.message);

    db.finish().await;
}

// ============ 刷新轮换 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn refresh_rotates_pair_and_revokes_the_old_refresh_token() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");

    let old_hash = sha256_hex(&session.refresh_token);
    let rotated = auth
        .refresh(&session.refresh_token)
        .await
        .expect("首次刷新应成功");
    assert_eq!(rotated.user_id, session.user_id);
    assert_ne!(rotated.refresh_token, session.refresh_token);
    assert_ne!(rotated.access_token, session.access_token);

    // 旧刷新令牌：当场吊销（轮换是"用掉即作废"，不是攒着等过期）
    let old = token_row(&db.pool, "refresh_tokens", &old_hash)
        .await
        .expect("旧刷新令牌应还在库里");
    assert!(old.revoked_at.is_some(), "旧刷新令牌必须当场作废");
    // 新的一对都已落库且未吊销
    for (table, token) in [
        ("access_tokens", rotated.access_token.as_str()),
        ("refresh_tokens", rotated.refresh_token.as_str()),
    ] {
        let row = token_row(&db.pool, table, &sha256_hex(token))
            .await
            .expect("新令牌应已落库");
        assert!(row.revoked_at.is_none(), "{table} 的新令牌不该被吊销");
    }
    // 新访问令牌立刻可用
    auth.authenticate(&rotated.access_token)
        .await
        .expect("刷新后新的访问令牌应可用");

    // 重放旧刷新令牌 → 401（轮换后不可复用）
    let replay = auth
        .refresh(&session.refresh_token)
        .await
        .expect_err("旧刷新令牌不能重放");
    assert_eq!(replay.code, ErrorCode::Unauthorized, "{}", replay.message);

    // 过期的刷新令牌 → 401
    let new_hash = sha256_hex(&rotated.refresh_token);
    let sql =
        "UPDATE refresh_tokens SET expires_at = now() - interval '1 second' WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&new_hash)
        .execute(&db.pool)
        .await
        .expect("把刷新令牌改成过期");
    let expired = auth
        .refresh(&rotated.refresh_token)
        .await
        .expect_err("过期刷新令牌必须失败");
    assert_eq!(expired.code, ErrorCode::Unauthorized, "{}", expired.message);

    // 库里根本没见过的 → 401
    let unknown = auth
        .refresh(&"b".repeat(64))
        .await
        .expect_err("未知刷新令牌必须失败");
    assert_eq!(unknown.code, ErrorCode::Unauthorized);

    db.finish().await;
}

// ============ 登出吊销 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn logout_revokes_current_access_token_and_all_refresh_tokens() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let first = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");
    let second = auth
        .login(EMAIL, PASSWORD)
        .await
        .expect("第二个会话应登录成功");

    let principal = auth
        .authenticate(&first.access_token)
        .await
        .expect("登出前访问令牌应可用");
    auth.logout(&principal).await.expect("登出应成功");

    // 当前访问令牌立刻失效（按 token_hash 精确吊销）
    let error = auth
        .authenticate(&first.access_token)
        .await
        .expect_err("登出后访问令牌必须失效");
    assert_eq!(error.code, ErrorCode::Unauthorized, "{}", error.message);
    // 另一个会话的访问令牌不受牵连
    auth.authenticate(&second.access_token)
        .await
        .expect("另一个会话的访问令牌不该被吊销");
    // 但刷新令牌是按 user_id 清空的：两个会话都得重新登录
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM refresh_tokens WHERE revoked_at IS NULL"
        )
        .await,
        0,
        "登出要把该账号所有未失效的刷新令牌都吊销"
    );
    let error = auth
        .refresh(&second.refresh_token)
        .await
        .expect_err("刷新令牌应已被一并吊销");
    assert_eq!(error.code, ErrorCode::Unauthorized, "{}", error.message);
    assert!(
        error.message.contains("失效"),
        "应是「已失效」而不是「无效」：{}",
        error.message
    );

    db.finish().await;
}

// ============ t134：H1 禁用即失效 ============

/// 用 SQL 直接停用（模拟运维/ADP）也必须让令牌立刻失效 —— 靠迁移 0008 的触发器兜底。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn disable_via_sql_revokes_tokens_and_blocks_refresh() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");
    assert!(auth.authenticate(&session.access_token).await.is_ok());

    // 运维路径：只改 users.disabled_at（不碰令牌表）
    let changed =
        sqlx::query("UPDATE users SET disabled_at = now() WHERE lower(email) = lower($1)")
            .bind(EMAIL)
            .execute(&db.pool)
            .await
            .expect("停用用户")
            .rows_affected();
    assert_eq!(changed, 1, "应正好停用一个用户");

    // **先验安全性质**：旧实现 refresh 不管 disabled_at ⇒ 这里会成功并无限续命（H1 正是这条）。
    // 断言顺序刻意放在吊销检查之前，让这条用例在旧实现下的失败点就是"续命成功"。
    assert!(
        auth.refresh(&session.refresh_token).await.is_err(),
        "停用账号不得再用刷新令牌续命（H1）"
    );
    let error = auth
        .refresh(&session.refresh_token)
        .await
        .expect_err("停用账号不得再用刷新令牌续命");
    assert_eq!(error.code, ErrorCode::Unauthorized, "必须是 401");

    // 访问令牌同样立刻失效（join 上 disabled_at IS NULL）
    let error = auth
        .authenticate(&session.access_token)
        .await
        .expect_err("停用账号的访问令牌必须失效");
    assert_eq!(error.code, ErrorCode::Unauthorized, "必须是 401");

    // 再验防御纵深：触发器必须把两类令牌都吊销（保留吊销时间，便于审计）
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM access_tokens WHERE revoked_at IS NULL"
        )
        .await,
        0,
        "停用后不该还有未吊销的访问令牌"
    );
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM refresh_tokens WHERE revoked_at IS NULL"
        )
        .await,
        0,
        "停用后不该还有未吊销的刷新令牌"
    );

    // 登录也拒绝（既有行为，回归保护）
    let error = auth
        .login(EMAIL, PASSWORD)
        .await
        .expect_err("停用账号不能登录");
    assert_eq!(error.code, ErrorCode::Unauthorized);

    db.finish().await;
}

/// 应用侧入口 `AuthService::disable_user`：幂等、返回是否真的改动、令牌一并吊销。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn disable_via_service_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");

    assert!(
        auth.disable_user(session.user_id)
            .await
            .expect("停用应成功"),
        "第一次停用应返回 true"
    );
    assert!(
        !auth
            .disable_user(session.user_id)
            .await
            .expect("重复停用不报错"),
        "重复停用应返回 false（幂等）"
    );

    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM refresh_tokens WHERE revoked_at IS NULL"
        )
        .await,
        0
    );
    assert_eq!(
        auth.refresh(&session.refresh_token)
            .await
            .expect_err("停用后刷新必须失败")
            .code,
        ErrorCode::Unauthorized
    );

    db.finish().await;
}

// ============ t134：H3 邮箱 +tag ============

/// `+tag` 规范化后是**同一个账号**：注册第二个必须撞唯一约束，登录用任意别名都行。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn email_plus_tag_maps_to_one_account() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    // 第一次用别名注册
    let session = auth
        .register("Student+lab@YNAU.edu.cn", PASSWORD)
        .await
        .expect("带 +tag 的合法地址必须能注册（只是按 Base 记账）");
    // 库里存的是规范化后的键
    assert_eq!(
        common::text_column(
            &db.pool,
            &format!("SELECT email FROM users WHERE id = '{}'", session.user_id)
        )
        .await
        .as_deref(),
        Some("student@ynau.edu.cn"),
        "落库的应是剥掉 +tag 且小写的规范化邮箱"
    );

    // 换一个别名（以及大小写）再注册 → 必须撞唯一约束（旧实现会成功）
    let error = auth
        .register("STUDENT+other@ynau.edu.cn", PASSWORD)
        .await
        .expect_err("同一收件箱的另一个别名不得注册第二个账号");
    assert_eq!(error.code, ErrorCode::ValidationError);

    // 登录：任意别名都落到同一个账号
    for alias in [
        "student@ynau.edu.cn",
        "Student+anything@ynau.edu.cn",
        "STUDENT@YNAU.EDU.CN",
    ] {
        let logged = auth
            .login(alias, PASSWORD)
            .await
            .unwrap_or_else(|error| panic!("{alias} 应能登录：{error:?}"));
        assert_eq!(logged.user_id, session.user_id, "{alias} 应落到同一账号");
    }

    db.finish().await;
}

// ============ t141：邀请制注册 ============

/// 一个可用的邀请码辅助：插库并返回明文。
async fn issue_invite(
    auth: &AuthService,
    code: &str,
    batch: &str,
    max_uses: i32,
    expires_at: Option<OffsetDateTime>,
) {
    auth.insert_invite(code, batch, "test", expires_at, max_uses, None)
        .await
        .expect("发放邀请码");
}

fn invite_use(code: &str) -> yunong_server::auth::InviteUse<'_> {
    yunong_server::auth::InviteUse {
        code,
        ip: Some("203.0.113.7"),
        user_agent: Some("ynau-test/1.0"),
    }
}

/// 注册必须带**可用**的邀请码；四种失败（不存在/过期/撤销/用完）返回同一个错误。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn invite_registration_requires_a_usable_code() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    // 不存在
    let unknown = auth
        .register_with_invite("a@ynau.edu.cn", PASSWORD, invite_use("ynau-unknown"))
        .await
        .expect_err("不存在的邀请码必须拒绝");
    assert_eq!(unknown.code, ErrorCode::Forbidden);

    // 过期
    issue_invite(
        &auth,
        "ynau-expired",
        "b1",
        1,
        Some(OffsetDateTime::now_utc() - time::Duration::seconds(1)),
    )
    .await;
    let expired = auth
        .register_with_invite("b@ynau.edu.cn", PASSWORD, invite_use("ynau-expired"))
        .await
        .expect_err("过期邀请码必须拒绝");

    // 撤销
    issue_invite(&auth, "ynau-revoked", "b1", 1, None).await;
    assert_eq!(auth.revoke_invite_batch("b1").await.expect("撤销"), 2);
    let revoked = auth
        .register_with_invite("c@ynau.edu.cn", PASSWORD, invite_use("ynau-revoked"))
        .await
        .expect_err("撤销后必须拒绝");

    // 用完
    issue_invite(&auth, "ynau-used", "b2", 1, None).await;
    auth.register_with_invite("d@ynau.edu.cn", PASSWORD, invite_use("ynau-used"))
        .await
        .expect("第一次应成功");
    let exhausted = auth
        .register_with_invite("e@ynau.edu.cn", PASSWORD, invite_use("ynau-used"))
        .await
        .expect_err("用完必须拒绝");

    // 四种失败的对外表现必须**完全一致**（码 + 文案），否则就是枚举预言机
    for error in [&expired, &revoked, &exhausted] {
        assert_eq!(error.code, unknown.code);
        assert_eq!(error.message, unknown.message);
    }
    assert_eq!(unknown.code, ErrorCode::Forbidden);

    // 失败路径不能建号
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM users").await,
        1,
        "只有成功的那一次建了号"
    );

    db.finish().await;
}

/// **并发安全**：同一个一次性邀请码被两个请求同时抢，只能成功一个。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn invite_is_single_use_under_concurrency() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    issue_invite(&auth, "ynau-race", "b1", 1, None).await;

    // 两个"用户"同时用同一张码注册（不同的邮箱，避免撞唯一约束干扰判断）
    let (first, second) = tokio::join!(
        auth.register_with_invite("race1@ynau.edu.cn", PASSWORD, invite_use("ynau-race")),
        auth.register_with_invite("race2@ynau.edu.cn", PASSWORD, invite_use("ynau-race")),
    );

    let successes = usize::from(first.is_ok()) + usize::from(second.is_ok());
    assert_eq!(successes, 1, "一次性邀请码必须只有一个请求成功");

    let failure = first.err().or_else(|| second.err()).expect("必有一个失败");
    assert_eq!(failure.code, ErrorCode::Forbidden);

    // 用量收敛到 1（不是 2）：靠 `UPDATE ... WHERE used_count < max_uses RETURNING`
    assert_eq!(
        auth.invite_batch_stats("b1").await.expect("用量"),
        (1, 1, 1)
    );
    assert_eq!(count(&db.pool, "SELECT count(*) FROM users").await, 1);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM invite_redemptions").await,
        1
    );

    db.finish().await;
}

/// 邮箱冲突时**不消耗**邀请码（事务回滚的证据）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn invite_is_not_burned_by_an_email_conflict() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    issue_invite(&auth, "ynau-first", "b1", 1, None).await;
    auth.register_with_invite(EMAIL, PASSWORD, invite_use("ynau-first"))
        .await
        .expect("第一次注册");

    issue_invite(&auth, "ynau-second", "b1", 1, None).await;
    let error = auth
        .register_with_invite(EMAIL, PASSWORD, invite_use("ynau-second"))
        .await
        .expect_err("重复邮箱必须 422");
    assert_eq!(error.code, ErrorCode::ValidationError);

    assert_eq!(
        auth.invite_batch_stats("b1").await.expect("用量"),
        (1, 2, 2),
        "冲突的这次不能把第二张码吃掉（used 只能是第一次的 1）"
    );

    db.finish().await;
}

/// 成功路径的落库形态：`auth_method='invite'`、核销记录里只有哈希、**没有明文 IP/UA**。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn invite_redemption_records_hashes_only() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    issue_invite(&auth, "ynau-hash", "b1", 1, None).await;
    auth.register_with_invite(EMAIL, PASSWORD, invite_use("ynau-hash"))
        .await
        .expect("注册应成功");

    assert_eq!(
        common::text_column(
            &db.pool,
            &format!("SELECT auth_method FROM users WHERE lower(email) = lower('{EMAIL}')")
        )
        .await
        .as_deref(),
        Some("invite"),
        "1.0 的账号来源就是 invite"
    );

    let row = sqlx::query("SELECT code_hash, ip_hash, ua_hash FROM invite_redemptions")
        .fetch_one(&db.pool)
        .await
        .expect("应有一条核销记录");
    let code_hash: String = row.get("code_hash");
    let ip_hash: Option<String> = row.get("ip_hash");
    let ua_hash: Option<String> = row.get("ua_hash");

    // 明文码只以哈希形式出现
    assert_eq!(code_hash, sha256_hex("ynau-hash"));
    assert!(!code_hash.contains("ynau-hash"));
    // IP / UA 也是哈希（64 hex），不是原文
    for (value, raw) in [
        (ip_hash.as_deref(), "203.0.113.7"),
        (ua_hash.as_deref(), "ynau-test/1.0"),
    ] {
        let value = value.expect("应记录哈希");
        assert_eq!(value.len(), 64, "必须是 sha256：{value}");
        assert!(!value.contains(raw), "不能存明文");
        assert_eq!(value, sha256_hex(raw));
    }

    // 库里不该有任何邀请码明文列
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'invite_codes' \
               AND column_name IN ('code', 'plain', 'code_plain')"
        )
        .await,
        0,
        "invite_codes 不允许有明文列"
    );

    db.finish().await;
}
