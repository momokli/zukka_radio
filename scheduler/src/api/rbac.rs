//! RBAC helper for the scheduler API (Issue #38).
//!
//! `require_permission` resolves the viewer from the canonical `X-Caster-Id`
//! header (set by the portal, never client input, §4.1) and asserts a single
//! permission from the §6.10.1 matrix. Least privilege is the rule: a missing
//! header, an unknown/inactive caster is `401`; a viewer whose role does not
//! carry the permission (`false`, missing key or broken JSON) is `403`.

use axum::http::HeaderMap;
use sqlx::SqlitePool;

use crate::api::viewer_caster_id;
use crate::error::{ApiError, ApiResult};

/// The authenticated viewer context resolved from `X-Caster-Id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub caster_id: String,
    pub display_name: String,
}

/// Resolves the viewer and asserts `permission` (§6.10.1).
///
/// * `X-Caster-Id` absent/blank, or caster unknown/`is_active = 0` → `401`.
/// * Permission not granted (incl. missing key / malformed JSON) → `403`.
pub async fn require_permission(
    pool: &SqlitePool,
    headers: &HeaderMap,
    permission: &str,
) -> ApiResult<Viewer> {
    let Some(caster_id) = viewer_caster_id(headers) else {
        return Err(ApiError::Unauthorized);
    };

    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT c.id, c.display_name, r.permissions, c.is_active \
         FROM casters c JOIN roles r ON r.id = c.role_id WHERE c.id = ?",
    )
    .bind(&caster_id)
    .fetch_optional(pool)
    .await?;

    let Some((id, display_name, permissions, is_active)) = row else {
        return Err(ApiError::Unauthorized);
    };
    if is_active == 0 {
        return Err(ApiError::Unauthorized);
    }

    // Least privilege: anything other than a literal `true` denies (a missing
    // key or unparseable JSON is a `403`, never a `500`).
    let granted = serde_json::from_str::<serde_json::Value>(&permissions)
        .ok()
        .and_then(|value| value.get(permission).and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    if !granted {
        return Err(ApiError::Forbidden);
    }

    Ok(Viewer {
        caster_id: id,
        display_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
    const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";
    const TS: &str = "2026-11-18T12:00:00.000Z";

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::connect(&dir.path().join("zukka.db"))
            .await
            .expect("connect + migrate");
        (dir, pool)
    }

    async fn insert_caster(pool: &SqlitePool, id: &str, role: &str, active: i64) {
        sqlx::query(
            "INSERT INTO casters \
             (id, username, display_name, password_hash, role_id, is_active, created_at, updated_at) \
             VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("user_{}", id.replace('-', "")))
        .bind(role)
        .bind(active)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert caster");
    }

    fn headers(caster: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(caster) = caster {
            headers.insert("x-caster-id", HeaderValue::from_str(caster).unwrap());
        }
        headers
    }

    #[tokio::test]
    async fn missing_header_is_401() {
        let (_dir, pool) = pool().await;
        insert_caster(&pool, CASTER_A, ROLE_CASTER, 1).await;
        let err = require_permission(&pool, &headers(None), "announcement.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized), "{err:?}");
    }

    #[tokio::test]
    async fn unknown_caster_is_401() {
        let (_dir, pool) = pool().await;
        let err = require_permission(
            &pool,
            &headers(Some("0192ffff-0000-7000-8000-000000000000")),
            "announcement.write",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized), "{err:?}");
    }

    #[tokio::test]
    async fn inactive_caster_is_401() {
        let (_dir, pool) = pool().await;
        insert_caster(&pool, CASTER_A, ROLE_CASTER, 0).await;
        let err = require_permission(&pool, &headers(Some(CASTER_A)), "announcement.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized), "{err:?}");
    }

    #[tokio::test]
    async fn caster_role_grants_announcement_write() {
        let (_dir, pool) = pool().await;
        insert_caster(&pool, CASTER_A, ROLE_CASTER, 1).await;
        let viewer = require_permission(&pool, &headers(Some(CASTER_A)), "announcement.write")
            .await
            .expect("granted");
        assert_eq!(viewer.caster_id, CASTER_A);
        assert_eq!(viewer.display_name, "Test Caster");
    }

    #[tokio::test]
    async fn missing_permission_key_is_403() {
        let (_dir, pool) = pool().await;
        // Empty permission map → least privilege.
        sqlx::query("UPDATE roles SET permissions = '{}' WHERE id = ?")
            .bind(ROLE_CASTER)
            .execute(&pool)
            .await
            .unwrap();
        insert_caster(&pool, CASTER_A, ROLE_CASTER, 1).await;
        let err = require_permission(&pool, &headers(Some(CASTER_A)), "announcement.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }

    #[tokio::test]
    async fn permission_false_is_403() {
        let (_dir, pool) = pool().await;
        // Explicit `false` (e.g. an operator-style role) → least privilege.
        sqlx::query("UPDATE roles SET permissions = '{\"announcement.write\":false}' WHERE id = ?")
            .bind(ROLE_CASTER)
            .execute(&pool)
            .await
            .unwrap();
        insert_caster(&pool, CASTER_A, ROLE_CASTER, 1).await;
        let err = require_permission(&pool, &headers(Some(CASTER_A)), "announcement.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }
}
