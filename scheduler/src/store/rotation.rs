//! Rotation M3U store (Issue #14).
//!
//! Renders the shared track library as an M3U playlist and keeps it up to date
//! on the `rotation` volume so Liquidsoap's
//! `playlist("/rotation/rotation.m3u", reload_mode="watch")` picks up changes
//! without a restart (§3c, liquidsoap.md D8).
//!
//! The projection is deterministic: every `tracks` row with
//! `visibility = 'shared'` and a non-empty `music_api_id`, ordered by `id`.
//! Randomization is Liquidsoap's job, not the scheduler's — the file must stay
//! stable unless the underlying content actually changes.

use std::io::Write;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::error::{ApiError, ApiResult};
use crate::store::playback::{music_api_base, playback_uri};

/// Target path of the rotation file.
///
/// Defaults to the production mount `/rotation/rotation.m3u`; overridable via
/// `ROTATION_M3U_PATH` (integration tests point it at a temp dir).
pub fn path() -> PathBuf {
    std::env::var("ROTATION_M3U_PATH")
        .unwrap_or_else(|_| "/rotation/rotation.m3u".to_string())
        .into()
}

/// Replaces any `\r`/`\n` (including `\r\n`) run with a single space so the
/// line-based M3U cannot be broken by newlines in metadata. Other characters
/// (including `,` and `#`) pass through unchanged.
fn sanitize_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_separator = false;
    for ch in value.chars() {
        if ch == '\r' || ch == '\n' {
            if !pending_separator {
                out.push(' ');
                pending_separator = true;
            }
        } else {
            out.push(ch);
            pending_separator = false;
        }
    }
    out
}

/// Renders the current rotation as an M3U string.
///
/// An empty rotation yields exactly `#EXTM3U\n` (valid, plays nothing).
pub async fn render(pool: &SqlitePool) -> ApiResult<String> {
    #[derive(Debug, sqlx::FromRow)]
    struct Row {
        music_api_id: String,
        isrc: Option<String>,
        title: String,
        artist: String,
        duration_ms: i64,
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT music_api_id, isrc, title, artist, duration_ms FROM tracks \
         WHERE visibility = 'shared' \
           AND (COALESCE(isrc, '') <> '' OR music_api_id <> '') ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let base = music_api_base();
    let mut out = String::from("#EXTM3U\n");
    for row in rows {
        let secs = row.duration_ms / 1000;
        let artist = sanitize_line(&row.artist);
        let title = sanitize_line(&row.title);
        if artist.is_empty() {
            out.push_str(&format!("#EXTINF:{secs},{title}\n"));
        } else {
            out.push_str(&format!("#EXTINF:{secs},{artist} - {title}\n"));
        }
        out.push_str(&format!(
            "{}\n",
            playback_uri(&base, row.isrc.as_deref(), &row.music_api_id)
        ));
    }
    Ok(out)
}

/// Writes `bytes` to `path` atomically: temp file in the same directory, fsync,
/// then `rename` (atomic on the same filesystem). Parents are created first.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)?;

    let stem = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rotation.m3u".to_string());
    let tmp = parent.join(format!(".{stem}.{}.tmp", std::process::id()));

    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Renders and atomically writes the rotation file to [`path`].
pub async fn sync(pool: &SqlitePool) -> ApiResult<()> {
    let body = render(pool).await?;
    write_atomic(&path(), body.as_bytes())
        .map_err(|err| ApiError::Internal(anyhow::Error::new(err)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_and_replaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("rotation.m3u");

        write_atomic(&target, b"#EXTM3U\n").expect("first write");
        assert_eq!(
            std::fs::read(&target).expect("read first"),
            b"#EXTM3U\n".to_vec()
        );

        write_atomic(&target, b"#EXTM3U\n#EXTINF:1X\n").expect("second write");
        assert_eq!(
            std::fs::read(&target).expect("read second"),
            b"#EXTM3U\n#EXTINF:1X\n".to_vec()
        );

        // No leftover temp files in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "unexpected temp files: {leftovers:?}");
    }

    #[tokio::test]
    async fn render_skips_rows_without_playable_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::connect(&dir.path().join("rot.db"))
            .await
            .expect("connect + migrate");
        let ts = "2026-11-18T12:00:00.000Z";

        for (id, music_api_id, title, artist, duration_ms) in [
            ("t1", "m1", "Kept", "Artist", 215_000_i64),
            ("t2", "", "Skipped", "Artist", 1_000_i64),
        ] {
            sqlx::query(
                "INSERT INTO tracks (id, music_api_id, title, artist, duration_ms, storage_key, visibility, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, 's3/key', 'shared', ?, ?)",
            )
            .bind(id)
            .bind(music_api_id)
            .bind(title)
            .bind(artist)
            .bind(duration_ms)
            .bind(ts)
            .bind(ts)
            .execute(&pool)
            .await
            .expect("insert track");
        }

        let m3u = render(&pool).await.expect("render");
        assert!(m3u.contains("215,Artist - Kept"));
        assert!(!m3u.contains("Skipped"), "unplayable row leaked: {m3u}");
        assert!(!m3u.contains("/objects/\n"));
    }

    #[test]
    fn sanitize_line_collapses_newlines() {
        assert_eq!(sanitize_line("a\nb"), "a b");
        assert_eq!(sanitize_line("a\r\nb"), "a b");
        assert_eq!(sanitize_line("a\rb"), "a b");
        assert_eq!(sanitize_line("plain"), "plain");
    }
}
