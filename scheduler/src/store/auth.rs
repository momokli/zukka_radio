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

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use sqlx::SqlitePool;

use crate::dto::{CasterDto, CreateSessionRequest, CreatedSessionDto, SessionValidationDto};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
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
// Harbor ingest auth (Issue #33)
// ---------------------------------------------------------------------------

/// Successful Harbor credential check.
#[derive(Debug, Clone)]
pub struct HarborAuth {
    pub caster_id: String,
    pub display_name: String,
    pub mount: String,
}

/// Verifies Harbor ingest credentials (§4.2, Issue #33).
///
/// Resolves `mount` → caster via `casters.harbor_mount`, then checks the
/// presented `username` against `casters.stream_username` (NOCASE) and
/// `password` against `casters.stream_password_hash`. Fail-closed: an unknown
/// mount/username, inactive caster, missing stream credentials or a wrong
/// password all produce `401 unauthorized` — deliberately indistinguishable.
///
/// Exactly **one** Argon2 verification runs per attempt: matched credentials
/// verify the real hash, everything else verifies the blind-verify dummy hash,
/// so unknown and known-but-wrong attempts cost the same (§4.1, < 200 ms).
/// No extra DB roundtrip happens after the single lookup.
pub async fn authenticate_harbor(
    pool: &SqlitePool,
    mount: &str,
    username: &str,
    password: &str,
) -> ApiResult<HarborAuth> {
    #[derive(Debug, sqlx::FromRow)]
    struct HarborRow {
        id: String,
        display_name: String,
        stream_username: Option<String>,
        stream_password_hash: Option<String>,
        is_active: i64,
    }

    let row = sqlx::query_as::<_, HarborRow>(
        "SELECT id, display_name, stream_username, stream_password_hash, is_active \
         FROM casters WHERE harbor_mount = ?",
    )
    .bind(mount)
    .fetch_optional(pool)
    .await?;

    // Decide the single hash to verify *before* running Argon2.
    let candidate = match row {
        Some(row)
            if row.is_active != 0
                && row
                    .stream_username
                    .as_deref()
                    .is_some_and(|u| u.eq_ignore_ascii_case(username))
                && row.stream_password_hash.is_some() =>
        {
            Some(row)
        }
        _ => None,
    };

    let hash = candidate
        .as_ref()
        .and_then(|row| row.stream_password_hash.as_deref())
        .unwrap_or_else(|| dummy_hash());

    // Exactly one Argon2 verification: verify the real hash only when the
    // candidate matched; otherwise verify the blind-verify dummy once.
    if !verify_password(hash, password) {
        return Err(ApiError::Unauthorized);
    }
    match candidate {
        Some(row) => Ok(HarborAuth {
            caster_id: row.id,
            display_name: row.display_name,
            mount: mount.to_string(),
        }),
        None => Err(ApiError::Unauthorized),
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

// ---------------------------------------------------------------------------
// Caster management (Issue #101)
// ---------------------------------------------------------------------------

/// Env var holding the harbor slot pool size (same value Compose uses).
pub const HARBOR_SLOTS_ENV: &str = "ZUKKA_HARBOR_SLOTS";
/// Env var holding the bootstrap admin username (Issue #101).
pub const BOOTSTRAP_USER_ENV: &str = "ZUKKA_BOOTSTRAP_ADMIN_USER";
/// Env var holding the bootstrap admin password (Issue #101).
pub const BOOTSTRAP_PASSWORD_ENV: &str = "ZUKKA_BOOTSTRAP_ADMIN_PASSWORD";

/// Values required to create a caster (§4.2 identity writes).
#[derive(Debug, Clone)]
pub struct NewCaster<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub password: &'a str,
    pub role: &'a str,
    pub harbor_mount: Option<&'a str>,
    pub email: Option<&'a str>,
    pub stream_username: Option<&'a str>,
}

/// Result of a successful create: the caster plus the one-time stream password.
#[derive(Debug, Clone)]
pub struct CreatedCaster {
    pub caster: CasterDto,
    pub stream_password: Option<String>,
}

/// Mutable caster fields (`PATCH /internal/casters/{id}`).
#[derive(Debug, Clone, Default)]
pub struct CasterPatch<'a> {
    pub display_name: Option<&'a str>,
    pub email: Option<&'a str>,
    pub role: Option<&'a str>,
    pub is_active: Option<bool>,
}

/// Outcome of the idempotent bootstrap check (Issue #101).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// A bootstrap admin was created (`mount` is its harbor mount).
    Created { username: String, mount: String },
    /// No `ZUKKA_BOOTSTRAP_ADMIN_*` env pair is configured.
    SkippedNoEnv,
    /// The `casters` table already has at least one row.
    SkippedNotEmpty,
}

struct PreparedCaster {
    username: String,
    display_name: String,
    email: Option<String>,
    password_hash: String,
    role_id: String,
    role: String,
    stream_username: String,
    stream_password_hash: String,
    harbor_mount: String,
}

fn validate_username(raw: &str) -> ApiResult<String> {
    let value = raw.trim().to_lowercase();
    let len = value.chars().count();
    if !(3..=32).contains(&len) {
        return Err(ApiError::validation(
            "username",
            "username must be 3-32 characters",
        ));
    }
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !first_ok || !rest_ok {
        return Err(ApiError::validation(
            "username",
            "username may only contain a-z, 0-9, '_' and '-', starting with a letter or digit",
        ));
    }
    Ok(value)
}

fn validate_display_name(raw: &str) -> ApiResult<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ApiError::validation(
            "display_name",
            "display_name must not be empty",
        ));
    }
    if value.chars().count() > 100 {
        return Err(ApiError::validation(
            "display_name",
            "display_name must be at most 100 characters",
        ));
    }
    Ok(value.to_string())
}

fn validate_password(raw: &str) -> ApiResult<()> {
    if raw.is_empty() {
        return Err(ApiError::validation(
            "password",
            "password must not be empty",
        ));
    }
    if raw.chars().count() < 8 {
        return Err(ApiError::validation(
            "password",
            "password must be at least 8 characters",
        ));
    }
    Ok(())
}

fn normalize_email(raw: Option<&str>) -> ApiResult<Option<String>> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) if !value.contains('@') => {
            Err(ApiError::validation("email", "email must contain '@'"))
        }
        Some(value) => Ok(Some(value.to_string())),
    }
}

async fn resolve_role(pool: &SqlitePool, raw: &str) -> ApiResult<(String, String)> {
    let name = raw.trim().to_lowercase();
    if !matches!(name.as_str(), "admin" | "operator" | "caster") {
        return Err(ApiError::validation(
            "role",
            "role must be one of admin, operator, caster",
        ));
    }
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM roles WHERE name = ?")
        .bind(&name)
        .fetch_optional(pool)
        .await?;
    id.map(|id| (id, name))
        .ok_or_else(|| ApiError::validation("role", "unknown role"))
}

fn harbor_slot_limit() -> u32 {
    std::env::var(HARBOR_SLOTS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|slots| *slots > 0)
        .unwrap_or(32)
        .min(99)
}

fn slot_mount(slot: u32) -> String {
    format!("/live/slot-{slot:02}")
}

fn parse_slot_mount(mount: &str) -> Option<u32> {
    let rest = mount.strip_prefix("/live/slot-")?;
    if rest.len() != 2 || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

async fn occupied_mounts(pool: &SqlitePool) -> ApiResult<HashSet<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT harbor_mount FROM casters WHERE harbor_mount IS NOT NULL")
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(mount,)| mount).collect())
}

fn resolve_mount(provided: Option<&str>, occupied: &HashSet<String>) -> ApiResult<String> {
    let slots = harbor_slot_limit();
    match provided.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let slot = parse_slot_mount(raw).filter(|slot| (1..=slots).contains(slot));
            let Some(slot) = slot else {
                return Err(ApiError::validation(
                    "harbor_mount",
                    format!("harbor_mount must match /live/slot-NN with slot 1..={slots}"),
                ));
            };
            let mount = slot_mount(slot);
            if occupied.contains(&mount) {
                return Err(ApiError::conflict(
                    "duplicate_harbor_mount",
                    "this harbor mount is already taken",
                ));
            }
            Ok(mount)
        }
        None => {
            for slot in 1..=slots {
                let mount = slot_mount(slot);
                if !occupied.contains(&mount) {
                    return Ok(mount);
                }
            }
            Err(ApiError::conflict(
                "harbor_pool_exhausted",
                "no free harbor mount slot left",
            ))
        }
    }
}

/// Maps a failed caster write onto the precise `409`/`422` codes.
fn map_caster_write_error(err: sqlx::Error) -> ApiError {
    if let Some(message) = unique_violation(&err) {
        if message.contains("casters.username") {
            return ApiError::conflict(
                "duplicate_username",
                "a caster with this username already exists",
            );
        }
        if message.contains("casters.harbor_mount") {
            return ApiError::conflict(
                "duplicate_harbor_mount",
                "this harbor mount is already taken",
            );
        }
        if message.contains("casters.email") {
            return ApiError::conflict(
                "duplicate_email",
                "a caster with this email already exists",
            );
        }
        if message.contains("casters.stream_username") {
            return ApiError::conflict(
                "duplicate_stream_username",
                "this stream username is already taken",
            );
        }
    }
    if is_foreign_key_violation(&err) {
        return ApiError::validation("role", "unknown role");
    }
    err.into()
}

async fn insert_caster_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    prepared: &PreparedCaster,
    actor: Option<&str>,
    extra_details: Option<serde_json::Value>,
) -> ApiResult<String> {
    let id = new_id();
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, email, password_hash, role_id, \
         stream_username, stream_password_hash, harbor_mount, is_active, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&prepared.username)
    .bind(&prepared.display_name)
    .bind(&prepared.email)
    .bind(&prepared.password_hash)
    .bind(&prepared.role_id)
    .bind(&prepared.stream_username)
    .bind(&prepared.stream_password_hash)
    .bind(&prepared.harbor_mount)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(map_caster_write_error)?;

    let mut details = serde_json::json!({
        "username": prepared.username,
        "role": prepared.role,
        "harbor_mount": prepared.harbor_mount,
    });
    if let Some(extra) = extra_details {
        if let (Some(object), serde_json::Value::Object(extra)) = (details.as_object_mut(), extra) {
            object.extend(extra);
        }
    }
    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, actor_caster_id, caster_id, details, \
         created_at) VALUES (?, ?, 'caster_created', ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(&ts)
    .bind(actor)
    .bind(&id)
    .bind(details.to_string())
    .bind(&ts)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// Creates a caster with a validated username, role and harbor slot, and a
/// freshly generated stream password (returned exactly once).
pub async fn create_caster(
    pool: &SqlitePool,
    req: &NewCaster<'_>,
    actor: Option<&str>,
) -> ApiResult<CreatedCaster> {
    let username = validate_username(req.username)?;
    let display_name = validate_display_name(req.display_name)?;
    validate_password(req.password)?;
    let email = normalize_email(req.email)?;
    let (role_id, role) = resolve_role(pool, req.role).await?;

    // Pre-check for a precise `duplicate_username` code: the insert also hits
    // `casters.stream_username` (default = username), which would otherwise
    // mask the username conflict.
    let existing: Option<String> =
        sqlx::query_scalar("SELECT username FROM casters WHERE username = ?")
            .bind(&username)
            .fetch_optional(pool)
            .await?;
    if existing.is_some() {
        return Err(ApiError::conflict(
            "duplicate_username",
            "a caster with this username already exists",
        ));
    }

    let occupied = occupied_mounts(pool).await?;
    let harbor_mount = resolve_mount(req.harbor_mount, &occupied)?;

    let stream_username = req
        .stream_username
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
        .unwrap_or_else(|| username.clone());
    let stream_password = generate_stream_password();

    let prepared = PreparedCaster {
        username,
        display_name,
        email,
        password_hash: hash_password(req.password)?,
        role_id,
        role,
        stream_username,
        stream_password_hash: hash_password(&stream_password)?,
        harbor_mount,
    };

    let mut tx = pool.begin().await?;
    let id = insert_caster_tx(&mut tx, &prepared, actor, None).await?;
    tx.commit().await?;

    Ok(CreatedCaster {
        caster: get_caster(pool, &id).await?,
        stream_password: Some(stream_password),
    })
}

/// Lists all casters ordered by username (case-insensitive).
pub async fn list_casters(pool: &SqlitePool) -> ApiResult<Vec<CasterDto>> {
    let sql = format!("{CASTER_SELECT} ORDER BY c.username COLLATE NOCASE");
    let rows = sqlx::query_as::<_, CasterAuthRow>(&sql)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(CasterAuthRow::into_dto).collect())
}

/// Number of casters (bootstrap idempotence).
pub async fn count_casters(pool: &SqlitePool) -> ApiResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM casters")
        .fetch_one(pool)
        .await?)
}

/// Updates the mutable caster fields. A role change or deactivation revokes
/// all of that caster's sessions. Username and mount never change here.
pub async fn update_caster(
    pool: &SqlitePool,
    id: &str,
    patch: &CasterPatch<'_>,
    actor: Option<&str>,
) -> ApiResult<CasterDto> {
    #[derive(Debug, sqlx::FromRow)]
    struct Existing {
        display_name: String,
        email: Option<String>,
        is_active: i64,
        role_id: String,
        role_name: String,
    }

    let existing = sqlx::query_as::<_, Existing>(
        "SELECT c.display_name, c.email, c.is_active, c.role_id, r.name AS role_name \
         FROM casters c JOIN roles r ON r.id = c.role_id WHERE c.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(existing) = existing else {
        return Err(ApiError::not_found("caster", id));
    };

    let display_name = match patch.display_name {
        Some(raw) => validate_display_name(raw)?,
        None => existing.display_name.clone(),
    };
    let email = match patch.email {
        Some(raw) => normalize_email(Some(raw))?,
        None => existing.email.clone(),
    };
    let (role_id, role_name) = match patch.role {
        Some(raw) => resolve_role(pool, raw).await?,
        None => (existing.role_id.clone(), existing.role_name.clone()),
    };
    let role_changed = role_name != existing.role_name;
    let is_active = patch.is_active.unwrap_or(existing.is_active != 0);
    let ts = now_ts();

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE casters SET display_name = ?, email = ?, role_id = ?, is_active = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&display_name)
    .bind(&email)
    .bind(&role_id)
    .bind(i64::from(is_active))
    .bind(&ts)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_caster_write_error)?;

    let event = if role_changed {
        "role_changed"
    } else {
        "caster_updated"
    };
    let details = serde_json::json!({ "role": role_name, "is_active": is_active });
    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, actor_caster_id, caster_id, details, \
         created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(&ts)
    .bind(event)
    .bind(actor)
    .bind(id)
    .bind(details.to_string())
    .bind(&ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if role_changed || !is_active {
        revoke_other_sessions(pool, id, None).await?;
    }

    get_caster(pool, id).await
}

/// Idempotent bootstrap: creates the first admin from
/// `ZUKKA_BOOTSTRAP_ADMIN_USER`/`ZUKKA_BOOTSTRAP_ADMIN_PASSWORD` when the
/// `casters` table is empty. The password is never logged.
pub async fn bootstrap_admin(pool: &SqlitePool) -> ApiResult<BootstrapOutcome> {
    let username = std::env::var(BOOTSTRAP_USER_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let password = std::env::var(BOOTSTRAP_PASSWORD_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    let (Some(username), Some(password)) = (username, password) else {
        return Ok(BootstrapOutcome::SkippedNoEnv);
    };

    if count_casters(pool).await? > 0 {
        return Ok(BootstrapOutcome::SkippedNotEmpty);
    }

    let username = validate_username(&username)?;
    validate_password(&password)?;
    let display_name = if username == "admin" {
        "Administrator".to_string()
    } else {
        username.clone()
    };
    let occupied = occupied_mounts(pool).await?;
    let harbor_mount = resolve_mount(None, &occupied)?;
    let stream_password = generate_stream_password();

    let prepared = PreparedCaster {
        username: username.clone(),
        display_name,
        email: None,
        password_hash: hash_password(&password)?,
        role_id: "0192f0a1-0001-7000-8000-000000000001".to_string(),
        role: "admin".to_string(),
        stream_username: username.clone(),
        stream_password_hash: hash_password(&stream_password)?,
        harbor_mount: harbor_mount.clone(),
    };

    let mut tx = pool.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM casters")
        .fetch_one(&mut *tx)
        .await?;
    if count > 0 {
        tx.commit().await?;
        return Ok(BootstrapOutcome::SkippedNotEmpty);
    }
    insert_caster_tx(
        &mut tx,
        &prepared,
        None,
        Some(serde_json::json!({ "actor": "bootstrap" })),
    )
    .await?;
    tx.commit().await?;

    Ok(BootstrapOutcome::Created {
        username,
        mount: harbor_mount,
    })
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
