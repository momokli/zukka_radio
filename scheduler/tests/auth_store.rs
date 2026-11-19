//! Store-level tests for the identity authority (Issue #16).
//!
//! These exercise `zukka_scheduler::store::auth` directly against a migrated
//! temporary SQLite database — no HTTP layer.

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_scheduler::dto::CreateSessionRequest;
use zukka_scheduler::store::auth;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

/// Inserts a caster with a real Argon2id login password and returns its id.
async fn caster(pool: &SqlitePool, username: &str, password: &str, active: bool) -> String {
    let id = new_id();
    let hash = auth::hash_password(password).expect("hash");
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind("Test Caster")
    .bind(&hash)
    .bind(ROLE_CASTER)
    .bind(if active { 1 } else { 0 })
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

#[tokio::test]
async fn verify_login_happy_and_failure_paths() {
    let (_dir, pool) = setup().await;
    let id = caster(&pool, "mira", "correct horse", true).await;

    let ok = auth::verify_login(&pool, "mira", "correct horse")
        .await
        .expect("valid login");
    assert_eq!(ok.id, id);
    assert_eq!(ok.username, "mira");
    assert_eq!(ok.role, "caster");
    assert!(ok.is_active);
    // permissions come from the seeded role JSON (§6.10.1)
    assert_eq!(ok.permissions.get("library.read"), Some(&true));
    assert_eq!(ok.permissions.get("settings.manage"), Some(&false));

    assert!(auth::verify_login(&pool, "mira", "wrong").await.is_err());
    assert!(
        auth::verify_login(&pool, "ghost", "whatever")
            .await
            .is_err()
    );

    // username lookup is NOCASE (casters.username COLLATE NOCASE)
    assert!(
        auth::verify_login(&pool, "MIRA", "correct horse")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn inactive_caster_is_401() {
    let (_dir, pool) = setup().await;
    caster(&pool, "off", "pw", false).await;
    assert!(
        auth::verify_login(&pool, "off", "pw").await.is_err(),
        "inactive caster must fail closed even with a correct password"
    );
}

#[tokio::test]
async fn session_lifecycle_create_validate_revoke() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "pw", true).await;

    let created = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: Some("127.0.0.1".into()),
            user_agent: Some("test".into()),
        },
    )
    .await
    .expect("create session");

    assert_eq!(created.id.len(), 36, "UUIDv7 session id");
    // only sha256(token) is stored, never the raw token
    let stored: Option<String> = sqlx::query_scalar("SELECT token_hash FROM sessions WHERE id = ?")
        .bind(&created.id)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some(auth::sha256_hex(&created.token).as_str())
    );
    assert_ne!(stored.as_deref(), Some(created.token.as_str()));

    let validation = auth::validate_session(&pool, &created.token)
        .await
        .expect("validate")
        .expect("valid session");
    assert_eq!(validation.session_id, created.id);
    assert_eq!(validation.caster.id, caster_id);
    assert_eq!(validation.csrf_token, created.csrf_token);

    // unknown token → None
    assert!(
        auth::validate_session(&pool, "not-a-token")
            .await
            .unwrap()
            .is_none()
    );

    // revoke → no longer valid
    assert!(auth::revoke_session(&pool, &created.id).await.unwrap());
    assert!(
        auth::validate_session(&pool, &created.token)
            .await
            .unwrap()
            .is_none()
    );
    // revoking twice is a no-op
    assert!(!auth::revoke_session(&pool, &created.id).await.unwrap());
}

#[tokio::test]
async fn expired_sessions_are_invalid() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "pw", true).await;
    let created = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();

    // push idle_expires_at into the past
    sqlx::query("UPDATE sessions SET idle_expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&created.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        auth::validate_session(&pool, &created.token)
            .await
            .unwrap()
            .is_none(),
        "idle-expired session must be invalid"
    );

    // both expired: idle < absolute (schema CHECK), both in the past
    sqlx::query(
        "UPDATE sessions SET idle_expires_at = '2000-01-01T00:00:00.000Z', \
         absolute_expires_at = '2001-01-01T00:00:00.000Z' WHERE id = ?",
    )
    .bind(&created.id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        auth::validate_session(&pool, &created.token)
            .await
            .unwrap()
            .is_none(),
        "absolute-expired session must be invalid"
    );
}

#[tokio::test]
async fn touch_session_slides_idle_window() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "pw", true).await;
    let created = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id,
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();

    // rewind idle_expires_at to a fixed past-ish value, then touch
    sqlx::query("UPDATE sessions SET idle_expires_at = '2026-11-19T00:00:00.000Z' WHERE id = ?")
        .bind(&created.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(auth::touch_session(&pool, &created.id).await.unwrap());
    let idle: String = sqlx::query_scalar("SELECT idle_expires_at FROM sessions WHERE id = ?")
        .bind(&created.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        idle.as_str() > "2026-11-19T00:00:00.000Z",
        "idle slid: {idle}"
    );
    // unknown session id → false
    assert!(!auth::touch_session(&pool, "nope").await.unwrap());
}

#[tokio::test]
async fn revoke_other_sessions_keeps_one() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "pw", true).await;
    let a = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();
    let b = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();
    let c = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id,
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();

    let revoked = auth::revoke_other_sessions(
        &pool,
        &auth::validate_session(&pool, &a.token)
            .await
            .unwrap()
            .unwrap()
            .caster
            .id,
        Some(&a.id),
    )
    .await
    .unwrap();
    assert_eq!(revoked, 2, "b and c revoked");
    assert!(
        auth::validate_session(&pool, &a.token)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        auth::validate_session(&pool, &b.token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth::validate_session(&pool, &c.token)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn password_change_rejects_stream_password_both_directions() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "login-pw", true).await;

    // set a stream password (provided)
    let generated = auth::change_stream_password(&pool, &caster_id, Some("stream-pw"))
        .await
        .unwrap();
    assert!(generated.is_none(), "provided password is not echoed back");

    // login password == stream password → 422 field=password
    let err = auth::change_password(&pool, &caster_id, "stream-pw", None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        zukka_scheduler::error::ApiError::Validation {
            field: "password",
            ..
        }
    ));

    // stream password == login password → 422 field=stream_password
    let err = auth::change_stream_password(&pool, &caster_id, Some("login-pw"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        zukka_scheduler::error::ApiError::Validation {
            field: "stream_password",
            ..
        }
    ));

    // a distinct login password is accepted
    auth::change_password(&pool, &caster_id, "new-login-pw", None)
        .await
        .unwrap();
    assert!(
        auth::verify_login(&pool, "mira", "new-login-pw")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn password_change_revokes_other_sessions() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "login-pw", true).await;
    let keep = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();
    let other = auth::create_session(
        &pool,
        &CreateSessionRequest {
            caster_id: caster_id.clone(),
            ip_address: None,
            user_agent: None,
        },
    )
    .await
    .unwrap();

    auth::change_password(&pool, &caster_id, "another-pw", Some(&keep.id))
        .await
        .unwrap();
    assert!(
        auth::validate_session(&pool, &keep.token)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        auth::validate_session(&pool, &other.token)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn generated_stream_password_is_24_chars_and_hashed() {
    let (_dir, pool) = setup().await;
    let caster_id = caster(&pool, "mira", "login-pw", true).await;
    let generated = auth::change_stream_password(&pool, &caster_id, None)
        .await
        .unwrap()
        .expect("generated password returned once");
    assert_eq!(generated.len(), 24);

    let stored: Option<String> =
        sqlx::query_scalar("SELECT stream_password_hash FROM casters WHERE id = ?")
            .bind(&caster_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let stored = stored.expect("stream hash stored");
    assert!(stored.starts_with("$argon2id$"), "hash only: {stored}");
    assert_ne!(stored, generated, "clear text is never stored");
    assert!(auth::verify_password(&stored, &generated));
}
