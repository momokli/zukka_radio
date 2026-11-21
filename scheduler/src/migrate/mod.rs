//! Legacy data migration (Issue #46 "Datenmigration", M7).
//!
//! `zukka-migrate` reads a documented JSON export of the legacy AzuraCast /
//! Funkwhale data and imports **casters, tracks and tapes** into the existing
//! `zukka.db`. Default mode is `--dry-run`: validate and count, never write.
//! `--apply` backs up the database, imports in a single transaction and is
//! idempotent (a second run writes zero rows).
//!
//! Semantics (see `docs/data-migration.md`):
//!
//! - **Casters** get the seeded `caster` role, a locked Argon2id password hash
//!   (password reset required) and `is_active` from the legacy `aktiv` flag.
//! - **Tracks** key on the stringified legacy file id (`music_api_id`); `isrc`
//!   is stored only when non-empty; `visibility = 'shared'`.
//! - **Tapes** key on a deterministic 8-char Crockford-Base32 slug; `source`
//!   is `live` when a broadcast id is present, else `upload`; `status='ready'`,
//!   `visible=0`.
//! - **Orphans/duplicates** (tape without a resolvable caster, track with an
//!   unknown caster, duplicated natural keys) abort *before* any write with a
//!   complete list and exit code 1.
//!
//! Exit codes: `0` ok, `1` validation/orphan error, `2` I/O or database error.

pub mod apply;
pub mod export;
pub mod map;
pub mod report;
pub mod slug;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::migrate::apply::{ApplyError, WritePlan};
use crate::migrate::export::LegacyExport;
use crate::migrate::map::{CasterRow, TapeRow, TrackRow, map_caster, map_tape, map_track};
use crate::migrate::report::{EntityReport, Issue, Report};

/// Migration configuration derived from the CLI.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the legacy JSON export.
    pub legacy: PathBuf,
    /// Path to the target `zukka.db`.
    pub db: PathBuf,
    /// `true` = `--apply`, `false` = `--dry-run`.
    pub apply: bool,
}

/// Result of a migration run.
#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    pub applied: bool,
    pub backup: Option<PathBuf>,
}

/// Error type carrying the CLI exit code.
#[derive(Debug)]
pub enum MigrateError {
    /// Validation failure (bad export/enum/field) → exit 1.
    Validation(String),
    /// Orphans/duplicates/invariant violation → exit 1, no writes.
    Failed(Vec<Issue>),
    /// Filesystem error → exit 2.
    Io(anyhow::Error),
    /// Database error → exit 2.
    Db(anyhow::Error),
}

impl MigrateError {
    /// Process exit code for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            MigrateError::Validation(_) | MigrateError::Failed(_) => 1,
            MigrateError::Io(_) | MigrateError::Db(_) => 2,
        }
    }
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Validation(msg) => write!(f, "validation error: {msg}"),
            MigrateError::Failed(issues) => {
                writeln!(f, "migration aborted: {} problem(s)", issues.len())?;
                for issue in issues {
                    writeln!(f, "  - {} {}: {}", issue.entity, issue.key, issue.reason)?;
                }
                Ok(())
            }
            MigrateError::Io(err) => write!(f, "i/o error: {err:#}"),
            MigrateError::Db(err) => write!(f, "database error: {err:#}"),
        }
    }
}

impl std::error::Error for MigrateError {}

// ---------------------------------------------------------------------------
// Existing-row snapshots (for skip/update classification)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ExistingCaster {
    id: String,
    display_name: String,
    email: Option<String>,
    is_active: i64,
    role_id: String,
}

#[derive(Debug, Clone)]
struct ExistingTrack {
    id: String,
    isrc: Option<String>,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    storage_key: String,
    visibility: String,
    caster_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ExistingTape {
    id: String,
    caster_id: String,
    title: Option<String>,
    started_at: String,
    ended_at: String,
    duration_ms: Option<i64>,
    source: String,
    music_api_id: Option<String>,
    storage_key: Option<String>,
    format: Option<String>,
    size_bytes: Option<i64>,
}

#[derive(Debug, Default)]
struct Existing {
    /// username (lowercase) → row
    casters: HashMap<String, ExistingCaster>,
    /// music_api_id → row
    tracks: HashMap<String, ExistingTrack>,
    /// slug → row
    tapes: HashMap<String, ExistingTape>,
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

/// Runs the migration according to `cfg`.
pub async fn run(cfg: Config) -> Result<Outcome, MigrateError> {
    let raw = std::fs::read_to_string(&cfg.legacy).map_err(|e| {
        MigrateError::Io(anyhow::anyhow!(
            "cannot read legacy export {}: {e}",
            cfg.legacy.display()
        ))
    })?;
    let export: LegacyExport = serde_json::from_str(&raw)
        .map_err(|e| MigrateError::Validation(format!("invalid legacy export JSON: {e}")))?;
    export.validate().map_err(MigrateError::Validation)?;

    let pool = open_db(&cfg.db, !cfg.apply)
        .await
        .map_err(MigrateError::Db)?;
    let existing = load_existing(&pool).await.map_err(MigrateError::Db)?;

    let mut report = Report::default();
    let mut issues: Vec<Issue> = Vec::new();
    let writes = plan(&export, &existing, &mut report, &mut issues);

    report.issues = issues;
    if !report.invariant_ok() {
        return Err(MigrateError::Validation(
            "counting invariant violated (read != written + skipped + failed)".to_string(),
        ));
    }
    if report.has_issues() {
        return Err(MigrateError::Failed(report.issues));
    }

    if !cfg.apply {
        return Ok(Outcome {
            report,
            applied: false,
            backup: None,
        });
    }

    let backup = apply::backup(&pool, &cfg.db).await.map_err(map_apply_err)?;
    match apply::apply(&pool, &writes).await {
        Ok(()) => {}
        Err(err) => return Err(map_apply_err(err)),
    }
    Ok(Outcome {
        report,
        applied: true,
        backup: Some(backup),
    })
}

fn map_apply_err(err: ApplyError) -> MigrateError {
    match err {
        ApplyError::ForeignKeyCheck(n) => MigrateError::Validation(format!(
            "foreign_key_check reported {n} row(s) after import; rolled back"
        )),
        ApplyError::Db(err) => MigrateError::Db(err),
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

fn plan(
    export: &LegacyExport,
    existing: &Existing,
    report: &mut Report,
    issues: &mut Vec<Issue>,
) -> WritePlan {
    let ts = crate::store::now_ts();
    let mut writes = WritePlan::default();

    // username (lowercase) → id, seeded from existing rows.
    let mut caster_map: HashMap<String, String> = existing
        .casters
        .iter()
        .map(|(k, v)| (k.clone(), v.id.clone()))
        .collect();

    // ---- casters ----
    let mut seen_usernames: HashSet<String> = HashSet::new();
    for legacy in &export.casters {
        report.casters.read += 1;
        let username = map::normalize_username(&legacy.username);
        if username.is_empty() {
            report.casters.failed += 1;
            issues.push(Issue::new(
                "caster",
                "<empty>",
                "username must not be empty",
            ));
            continue;
        }
        if !seen_usernames.insert(username.clone()) {
            report.casters.failed += 1;
            issues.push(Issue::new(
                "caster",
                username,
                "duplicate username in export",
            ));
            continue;
        }
        let id = caster_map
            .get(&username)
            .cloned()
            .unwrap_or_else(crate::uuid::new_id);
        let row = map_caster(id.clone(), legacy, &ts);
        caster_map.insert(username.clone(), id);
        classify_caster(row, existing, &mut writes, &mut report.casters);
    }

    // ---- tracks ----
    let mut seen_music_api_ids: HashSet<String> = HashSet::new();
    let mut seen_isrcs: HashSet<String> = HashSet::new();
    for legacy in &export.tracks {
        report.tracks.read += 1;
        let music_api_id = legacy.file_id.to_string();
        if !seen_music_api_ids.insert(music_api_id.clone()) {
            report.tracks.failed += 1;
            issues.push(Issue::new(
                "track",
                music_api_id,
                "duplicate music_api_id in export",
            ));
            continue;
        }
        let caster_ref = legacy
            .caster
            .as_deref()
            .map(map::normalize_username)
            .filter(|c| !c.is_empty());
        let caster_id = match &caster_ref {
            Some(name) => match caster_map.get(name) {
                Some(id) => Some(id.clone()),
                None => {
                    report.tracks.failed += 1;
                    issues.push(Issue::new(
                        "track",
                        music_api_id,
                        format!("unknown caster '{name}'"),
                    ));
                    continue;
                }
            },
            None => None,
        };
        let id = existing
            .tracks
            .get(&music_api_id)
            .map(|e| e.id.clone())
            .unwrap_or_else(crate::uuid::new_id);
        match map_track(id, legacy, caster_id, &ts) {
            Ok(row) => {
                if let Some(isrc) = &row.isrc
                    && !seen_isrcs.insert(isrc.clone())
                {
                    report.tracks.failed += 1;
                    issues.push(Issue::new(
                        "track",
                        music_api_id,
                        format!("duplicate isrc '{isrc}' in export"),
                    ));
                    continue;
                }
                classify_track(row, existing, &mut writes, &mut report.tracks);
            }
            Err(reason) => {
                report.tracks.failed += 1;
                issues.push(Issue::new("track", music_api_id, reason));
            }
        }
    }

    // ---- tapes ----
    let mut seen_slugs: HashSet<String> = HashSet::new();
    let mut seen_tape_music_api_ids: HashSet<String> = HashSet::new();
    for legacy in &export.tapes {
        report.tapes.read += 1;
        let key = legacy.natural_key().to_string();
        let slug = slug::slug_from_key(&key);
        if !seen_slugs.insert(slug.clone()) {
            report.tapes.failed += 1;
            issues.push(Issue::new(
                "tape",
                key,
                format!("duplicate slug '{slug}' in export"),
            ));
            continue;
        }
        let caster_name = map::normalize_username(&legacy.caster);
        let caster_id = match caster_map.get(&caster_name) {
            Some(id) => id.clone(),
            None => {
                report.tapes.failed += 1;
                issues.push(Issue::new(
                    "tape",
                    key,
                    format!("unknown caster '{caster_name}' (orphan tape)"),
                ));
                continue;
            }
        };
        let id = existing
            .tapes
            .get(&slug)
            .map(|e| e.id.clone())
            .unwrap_or_else(crate::uuid::new_id);
        match map_tape(id, legacy, caster_id, slug, &ts) {
            Ok(row) => {
                if let Some(api_id) = &row.music_api_id
                    && !seen_tape_music_api_ids.insert(api_id.clone())
                {
                    report.tapes.failed += 1;
                    issues.push(Issue::new(
                        "tape",
                        key,
                        format!("duplicate music_api_id '{api_id}' in export"),
                    ));
                    continue;
                }
                classify_tape(row, existing, &mut writes, &mut report.tapes);
            }
            Err(reason) => {
                report.tapes.failed += 1;
                issues.push(Issue::new("tape", key, reason));
            }
        }
    }

    writes
}

fn classify_caster(
    row: CasterRow,
    existing: &Existing,
    writes: &mut WritePlan,
    counter: &mut EntityReport,
) {
    match existing.casters.get(&row.username) {
        Some(e)
            if e.display_name == row.display_name
                && e.email == row.email
                && e.is_active == row.is_active
                && e.role_id == row.role_id =>
        {
            counter.skipped += 1;
        }
        Some(_) => {
            counter.written += 1;
            writes.caster_updates.push(row);
        }
        None => {
            counter.written += 1;
            writes.caster_inserts.push(row);
        }
    }
}

fn classify_track(
    row: TrackRow,
    existing: &Existing,
    writes: &mut WritePlan,
    counter: &mut EntityReport,
) {
    match existing.tracks.get(&row.music_api_id) {
        Some(e)
            if e.isrc == row.isrc
                && e.title == row.title
                && e.artist == row.artist
                && e.album == row.album
                && e.duration_ms == row.duration_ms
                && e.storage_key == row.storage_key
                && e.visibility == row.visibility
                && e.caster_id == row.caster_id =>
        {
            counter.skipped += 1;
        }
        Some(_) => {
            counter.written += 1;
            writes.track_updates.push(row);
        }
        None => {
            counter.written += 1;
            writes.track_inserts.push(row);
        }
    }
}

fn classify_tape(
    row: TapeRow,
    existing: &Existing,
    writes: &mut WritePlan,
    counter: &mut EntityReport,
) {
    match existing.tapes.get(&row.slug) {
        Some(e)
            if e.caster_id == row.caster_id
                && e.title == row.title
                && e.started_at == row.started_at
                && e.ended_at == row.ended_at
                && e.duration_ms == row.duration_ms
                && e.source == row.source
                && e.music_api_id == row.music_api_id
                && e.storage_key == row.storage_key
                && e.format == row.format
                && e.size_bytes == row.size_bytes =>
        {
            counter.skipped += 1;
        }
        Some(_) => {
            counter.written += 1;
            writes.tape_updates.push(row);
        }
        None => {
            counter.written += 1;
            writes.tape_inserts.push(row);
        }
    }
}

// ---------------------------------------------------------------------------
// Database helpers
// ---------------------------------------------------------------------------

async fn open_db(path: &Path, read_only: bool) -> anyhow::Result<SqlitePool> {
    if !path.exists() {
        anyhow::bail!("database {} does not exist", path.display());
    }
    let mut options = SqliteConnectOptions::new()
        .filename(path)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    if read_only {
        options = options.read_only(true);
    } else {
        options = options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
    }
    let max = if read_only { 5 } else { 1 };
    let pool = SqlitePoolOptions::new()
        .max_connections(max)
        .connect_with(options)
        .await?;
    Ok(pool)
}

async fn load_existing(pool: &SqlitePool) -> anyhow::Result<Existing> {
    let mut existing = Existing::default();

    let casters = sqlx::query_as::<_, (String, String, String, Option<String>, i64, String)>(
        "SELECT id, username, display_name, email, is_active, role_id FROM casters",
    )
    .fetch_all(pool)
    .await?;
    for (id, username, display_name, email, is_active, role_id) in casters {
        existing.casters.insert(
            username.to_lowercase(),
            ExistingCaster {
                id,
                display_name,
                email,
                is_active,
                role_id,
            },
        );
    }

    let tracks = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            String,
            String,
            Option<String>,
        ),
    >(
        "SELECT id, music_api_id, isrc, title, artist, album, duration_ms, storage_key, \
         visibility, caster_id FROM tracks",
    )
    .fetch_all(pool)
    .await?;
    for (
        id,
        music_api_id,
        isrc,
        title,
        artist,
        album,
        duration_ms,
        storage_key,
        visibility,
        caster_id,
    ) in tracks
    {
        existing.tracks.insert(
            music_api_id,
            ExistingTrack {
                id,
                isrc,
                title,
                artist,
                album,
                duration_ms,
                storage_key,
                visibility,
                caster_id,
            },
        );
    }

    let tapes = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            Option<i64>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ),
    >(
        "SELECT id, slug, caster_id, title, started_at, ended_at, duration_ms, source, \
         music_api_id, storage_key, format, size_bytes FROM tapes",
    )
    .fetch_all(pool)
    .await?;
    for (
        id,
        slug,
        caster_id,
        title,
        started_at,
        ended_at,
        duration_ms,
        source,
        music_api_id,
        storage_key,
        format,
        size_bytes,
    ) in tapes
    {
        existing.tapes.insert(
            slug,
            ExistingTape {
                id,
                caster_id,
                title,
                started_at,
                ended_at,
                duration_ms,
                source,
                music_api_id,
                storage_key,
                format,
                size_bytes,
            },
        );
    }

    Ok(existing)
}
