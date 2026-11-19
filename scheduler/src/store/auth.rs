//! Identity authority store (Issue #16): casters, roles, sessions, passwords.
//!
//! The scheduler is the single writer and the identity authority
//! (§4.1/§4.3, C02/C03/C10). This module owns Argon2id password hashing,
//! login verification (with blind-verify for unknown users), server-side
//! sessions (create/validate/rotate/revoke) and the CSRF token derivation
//! `HMAC(csrf_secret, "csrf")`.
//!
//! The portal never touches `casters`/`sessions` directly: it goes through the
//! `/internal/*` API built on top of these functions.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use sqlx::SqlitePool;

use crate::dto::{CasterDto, CreateSessionRequest, CreatedSessionDto, SessionValidationDto};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Session idle timeout in days (§4.1 / portal.md D3).
pub const SESSION_IDLE_DAYS: i64 = 7;
/// Absolute session lifetime in days (§4.1 / portal.md D3).
pub const SESSION_ABSOLUTE_DAYS: i64 = 30;

// ---------------------------------------------------------------------------
// Argon2id
// ---------------------------------------------------------------------------

/// Hashes a password with Argon2id and a per-password random salt, returning a
/// PHC string (`$argon2id$v=19$m=…$<salt>$<hash>`). The salt lives inside the
/// PHC string, so no extra column is needed.
pub fn hash_password(password: &str) -> ApiResult<String> {
    let salt = SaltString::encode_b64(&random_bytes(16))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("argon2 salt encode failed: {e}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("argon2 hash failed: {e}")))
}

/// Verifies `password` against a PHC hash. Any parse/verify failure is `false`
/// (fail-closed).
pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A constant Argon2id hash used to blind-verify unknown usernames, so an
/// unknown user and a wrong password take comparable time (no enumeration,
/// portal.md D2/§3.6).
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("zukka-blind-verify-dummy-not-a-real-password").unwrap_or_default()
    })
}

// ---------------------------------------------------------------------------
// Tokens / secrets
// ---------------------------------------------------------------------------

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    OsRng.fill_bytes(&mut buf);
    buf
}

fn base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A fresh 32-byte session token, base64url (portal.md D3).
pub fn generate_session_token() -> String {
    base64url(&random_bytes(32))
}

/// A fresh 32-byte CSRF secret, base64url.
pub fn generate_csrf_secret() -> String {
    base64url(&random_bytes(32))
}

/// `sha256(token)` in lowercase hex — the only form stored in `sessions`.
pub fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Derives the CSRF token from a session secret: `HMAC-SHA256(secret, "csrf")`,
/// base64url (portal.md D4). Deterministic for a fixed secret.
pub fn csrf_token(csrf_secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(csrf_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(b"csrf");
    base64url(&mac.finalize().into_bytes())
}

/// Generates a 24-character stream password from the Crockford base32 alphabet
/// (120 bits, no ambiguous characters), shown once at creation (portal.md §3.6).
pub fn generate_stream_password() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = random_bytes(15); // 120 bits → exactly 24 base32 chars
    let mut out = String::with_capacity(24);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in bytes {
        acc = (acc << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 0x1f) as usize] as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

fn fmt_ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Casters
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct CasterAuthRow {
    id: String,
    username: String,
    display_name: String,
    email: Option<String>,
    password_hash: String,
    is_active: i64,
    harbor_mount: Option<String>,
    stream_username: Option<String>,
    role_name: String,
    permissions: String,
}

impl CasterAuthRow {
    fn into_dto(self) -> CasterDto {
        let permissions: BTreeMap<String, bool> =
            serde_json::from_str(&self.permissions).unwrap_or_default();
        CasterDto {
            id: self.id,
            username: self.username,
            display_name: self.display_name,
            email: self.email,
            role: self.role_name,
            is_active: self.is_active != 0,
            harbor_mount: self.harbor_mount,
            stream_username: self.stream_username,
            permissions,
        }
    }
}

const CASTER_SELECT: &str = "SELECT c.id, c.username, c.display_name, c.email, c.password_hash, \
     c.is_active, c.harbor_mount, c.stream_username, \
     r.name AS role_name, r.permissions AS permissions \
     FROM casters c JOIN roles r ON r.id = c.role_id";

async fn fetch_caster_by_username(
    pool: &SqlitePool,
    username: &str,
) -> ApiResult<Option<CasterAuthRow>> {
    let sql = format!("{CASTER_SELECT} WHERE c.username = ?");
    let row = sqlx::query_as::<_, CasterAuthRow>(&sql)
        .bind(username)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

async fn fetch_caster_by_id(pool: &SqlitePool, id: &str) -> ApiResult<Option<CasterAuthRow>> {
    let sql = format!("{CASTER_SELECT} WHERE c.id = ?");
    let row = sqlx::query_as::<_, CasterAuthRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Fetches one caster (joined with role + permissions) or `404`.
pub async fn get_caster(pool: &SqlitePool, id: &str) -> ApiResult<CasterDto> {
    fetch_caster_by_id(pool, id)
        .await?
        .map(CasterAuthRow::into_dto)
        .ok_or_else(|| ApiError::not_found("caster", id))
}

/// Verifies `username`/`password`. Unknown user and wrong password both yield
/// `401` with comparable cost (blind verify); an inactive caster is `401` too
/// (fail-closed).
pub async fn verify_login(
    pool: &SqlitePool,
    username: &str,
    password: &str,
) -> ApiResult<CasterDto> {
    match fetch_caster_by_username(pool, username).await? {
        Some(row) => {
            let ok = verify_password(&row.password_hash, password);
            if ok && row.is_active != 0 {
                Ok(row.into_dto())
            } else {
                Err(ApiError::Unauthorized)
            }
        }
        None => {
            // Blind verify: identical cost to a real check, identical error.
            let _ = verify_password(dummy_hash(), password);
            Err(ApiError::Unauthorized)
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: String,
    caster_id: String,
    csrf_secret: String,
    revoked_at: Option<String>,
    idle_expires_at: String,
    absolute_expires_at: String,
}

/// Creates a session for an active caster. The raw token is returned **once**;
/// only `sha256(token)` is stored.
pub async fn create_session(
    pool: &SqlitePool,
    req: &CreateSessionRequest,
) -> ApiResult<CreatedSessionDto> {
    let is_active: Option<i64> = sqlx::query_scalar("SELECT is_active FROM casters WHERE id = ?")
        .bind(&req.caster_id)
        .fetch_optional(pool)
        .await?;
    match is_active {
        Some(1) => {}
        Some(_) => return Err(ApiError::Unauthorized),
        None => return Err(ApiError::not_found("caster", req.caster_id.clone())),
    }

    let token = generate_session_token();
    let token_hash = sha256_hex(&token);
    let csrf_secret = generate_csrf_secret();
    let csrf = csrf_token(&csrf_secret);
    let id = new_id();
    let now = Utc::now();
    let created = fmt_ts(now);
    let idle_expires_at = fmt_ts(now + Duration::days(SESSION_IDLE_DAYS));
    let absolute_expires_at = fmt_ts(now + Duration::days(SESSION_ABSOLUTE_DAYS));

    sqlx::query(
        "INSERT INTO sessions (id, caster_id, token_hash, csrf_secret, created_at, last_seen_at, \
         idle_expires_at, absolute_expires_at, ip_address, user_agent) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.caster_id)
    .bind(&token_hash)
    .bind(&csrf_secret)
    .bind(&created)
    .bind(&created)
    .bind(&idle_expires_at)
    .bind(&absolute_expires_at)
    .bind(req.ip_address.clone())
    .bind(req.user_agent.clone())
    .execute(pool)
    .await?;

    Ok(CreatedSessionDto {
        id,
        token,
        csrf_token: csrf,
        idle_expires_at,
        absolute_expires_at,
    })
}

/// Validates a raw session token. Returns `None` when the session is unknown,
/// revoked, idle/absolute-expired, or the caster is inactive.
pub async fn validate_session(
    pool: &SqlitePool,
    token: &str,
) -> ApiResult<Option<SessionValidationDto>> {
    let token_hash = sha256_hex(token);
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, caster_id, csrf_secret, revoked_at, idle_expires_at, absolute_expires_at \
         FROM sessions WHERE token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    if row.revoked_at.is_some() {
        return Ok(None);
    }
    let now = now_ts();
    // RFC3339 UTC ms strings are lexicographically ordered (§3.1/§3.5).
    if row.idle_expires_at <= now || row.absolute_expires_at <= now {
        return Ok(None);
    }
    let caster = match fetch_caster_by_id(pool, &row.caster_id).await? {
        Some(caster) if caster.is_active != 0 => caster.into_dto(),
        _ => return Ok(None),
    };

    Ok(Some(SessionValidationDto {
        session_id: row.id,
        caster,
        csrf_token: csrf_token(&row.csrf_secret),
        idle_expires_at: row.idle_expires_at,
        absolute_expires_at: row.absolute_expires_at,
    }))
}

/// Slides the idle window forward (portal batches this to ≤ 1×/min, §4.2).
/// The idle expiry never passes the absolute limit. Returns `false` for an
/// unknown or revoked session.
pub async fn touch_session(pool: &SqlitePool, id: &str) -> ApiResult<bool> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT absolute_expires_at FROM sessions WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((absolute,)) = row else {
        return Ok(false);
    };

    let now = Utc::now();
    let mut idle = now + Duration::days(SESSION_IDLE_DAYS);
    if let Some(absolute_dt) = parse_ts(&absolute) {
        if idle > absolute_dt {
            idle = absolute_dt;
        }
    }
    let res = sqlx::query(
        "UPDATE sessions SET last_seen_at = ?, idle_expires_at = ? \
         WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(fmt_ts(now))
    .bind(fmt_ts(idle))
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Revokes one session (logout). Returns `false` when nothing was revoked.
pub async fn revoke_session(pool: &SqlitePool, id: &str) -> ApiResult<bool> {
    let res = sqlx::query("UPDATE sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(now_ts())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Revokes every active session of a caster except `keep_session_id`
/// (rotation on password/role change). Returns the number revoked.
pub async fn revoke_other_sessions(
    pool: &SqlitePool,
    caster_id: &str,
    keep_session_id: Option<&str>,
) -> ApiResult<u64> {
    let ts = now_ts();
    let res = match keep_session_id {
        Some(keep) => {
            sqlx::query(
                "UPDATE sessions SET revoked_at = ? \
                 WHERE caster_id = ? AND id != ? AND revoked_at IS NULL",
            )
            .bind(&ts)
            .bind(caster_id)
            .bind(keep)
            .execute(pool)
            .await?
        }
        None => {
            sqlx::query(
                "UPDATE sessions SET revoked_at = ? WHERE caster_id = ? AND revoked_at IS NULL",
            )
            .bind(&ts)
            .bind(caster_id)
            .execute(pool)
            .await?
        }
    };
    Ok(res.rows_affected())
}

// ---------------------------------------------------------------------------
// Passwords (login vs. stream, D2)
// ---------------------------------------------------------------------------

/// Sets a new login password. Rejects (in both directions, D2) a password that
/// matches the caster's stream password with `422 field=password`. Revokes all
/// sessions except `keep_session_id` (rotation).
pub async fn change_password(
    pool: &SqlitePool,
    caster_id: &str,
    new_password: &str,
    keep_session_id: Option<&str>,
) -> ApiResult<()> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT password_hash, stream_password_hash FROM casters WHERE id = ?")
            .bind(caster_id)
            .fetch_optional(pool)
            .await?;
    let Some((_login_hash, stream_hash)) = row else {
        return Err(ApiError::not_found("caster", caster_id));
    };
    if new_password.is_empty() {
        return Err(ApiError::validation(
            "password",
            "password must not be empty",
        ));
    }
    if let Some(stream_hash) = stream_hash {
        if verify_password(&stream_hash, new_password) {
            return Err(ApiError::validation(
                "password",
                "login password must differ from the stream password",
            ));
        }
    }

    let hash = hash_password(new_password)?;
    let ts = now_ts();
    sqlx::query("UPDATE casters SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&hash)
        .bind(&ts)
        .bind(caster_id)
        .execute(pool)
        .await?;

    revoke_other_sessions(pool, caster_id, keep_session_id).await?;
    Ok(())
}

/// Sets the stream password. When `provided` is `None`, a fresh 24-char
/// password is generated and returned once (never stored in clear). A provided
/// password equal to the login password is rejected with `422` (D2, both
/// directions). A running Harbor connection is not touched (no kick/bann).
pub async fn change_stream_password(
    pool: &SqlitePool,
    caster_id: &str,
    provided: Option<&str>,
) -> ApiResult<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM casters WHERE id = ?")
        .bind(caster_id)
        .fetch_optional(pool)
        .await?;
    let Some((login_hash,)) = row else {
        return Err(ApiError::not_found("caster", caster_id));
    };

    let (plain, generated) = match provided {
        Some(password) => {
            if password.is_empty() {
                return Err(ApiError::validation(
                    "stream_password",
                    "stream password must not be empty",
                ));
            }
            if verify_password(&login_hash, password) {
                return Err(ApiError::validation(
                    "stream_password",
                    "stream password must differ from the login password",
                ));
            }
            (password.to_string(), false)
        }
        None => (generate_stream_password(), true),
    };

    let hash = hash_password(&plain)?;
    let ts = now_ts();
    sqlx::query("UPDATE casters SET stream_password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&hash)
        .bind(&ts)
        .bind(caster_id)
        .execute(pool)
        .await?;

    Ok(if generated { Some(plain) } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_argon2id_with_unique_salt() {
        let a = hash_password("correct horse").unwrap();
        let b = hash_password("correct horse").unwrap();
        assert!(a.starts_with("$argon2id$"), "phc: {a}");
        assert_ne!(a, b, "per-password salt must differ");
        assert!(verify_password(&a, "correct horse"));
        assert!(!verify_password(&a, "wrong"));
    }

    #[test]
    fn verify_rejects_garbage_hash() {
        assert!(!verify_password("not-a-phc-string", "x"));
    }

    #[test]
    fn csrf_token_is_deterministic() {
        let secret = "fixed-secret";
        assert_eq!(csrf_token(secret), csrf_token(secret));
        assert_ne!(csrf_token(secret), csrf_token("other-secret"));
    }

    #[test]
    fn stream_password_is_24_crockford_chars() {
        let pw = generate_stream_password();
        assert_eq!(pw.len(), 24, "pw: {pw}");
        assert!(
            pw.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "pw: {pw}"
        );
        assert!(!pw.contains(['I', 'L', 'O', 'U']), "ambiguous char in {pw}");
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn session_token_is_url_safe_and_32_bytes() {
        let token = generate_session_token();
        // 32 bytes → 43 base64url chars without padding.
        assert_eq!(token.len(), 43, "token: {token}");
        assert!(!token.contains(['+', '/', '=']));
    }
}
