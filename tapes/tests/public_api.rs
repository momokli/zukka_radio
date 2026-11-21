//! HTTP-level integration tests for the public Tapes API (Issue #24).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! [`zukka_tapes::api::router`]; the database is a temporary SQLite file
//! seeded from the scheduler schema (no network). The application pool is
//! opened read-only, so a write attempt must fail with `SQLITE_READONLY`.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const BASE_URL: &str = "https://tapes.test";
const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-01T00:00:00.000Z";

fn build_get(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

async fn get(app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    send(app, build_get(uri, &[])).await
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, _headers, body) = get(app, uri).await;
    let value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (status, value)
}

struct Seed {
    slug: &'static str,
    caster: &'static str,
    show: Option<&'static str>,
    title: Option<&'static str>,
    started: &'static str,
    ended: &'static str,
    status: &'static str,
    visible: i64,
    duration_ms: Option<i64>,
    format: Option<&'static str>,
    published: Option<&'static str>,
}

fn seeds() -> Vec<Seed> {
    vec![
        // ready + visible, with title, show and waveform (detail fixture).
        Seed {
            slug: "aaa111",
            caster: "c-alpha",
            show: Some("s-deep"),
            title: Some("My Title"),
            started: "2026-11-23T10:00:00.000Z",
            ended: "2026-11-23T12:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(7_194_000),
            format: Some("mp3"),
            published: Some("2026-11-23T12:04:11.000Z"),
        },
        // ready + visible, no title but show → display_title = "Deep Cuts".
        Seed {
            slug: "bbb222",
            caster: "c-alpha",
            show: Some("s-deep"),
            title: None,
            started: "2026-11-22T10:00:00.000Z",
            ended: "2026-11-22T11:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: Some("mp3"),
            published: Some("2026-11-22T11:00:00.000Z"),
        },
        // ready + visible, neither title nor show → display_title = "Alpha".
        Seed {
            slug: "ccc333",
            caster: "c-alpha",
            show: None,
            title: None,
            started: "2026-11-21T10:00:00.000Z",
            ended: "2026-11-21T11:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: None,
            published: Some("2026-11-21T11:00:00.000Z"),
        },
        // ready + visible, other caster (Beta).
        Seed {
            slug: "ddd444",
            caster: "c-beta",
            show: None,
            title: Some("Beta Show"),
            started: "2026-11-20T10:00:00.000Z",
            ended: "2026-11-20T11:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: Some("flac"),
            published: Some("2026-11-20T11:00:00.000Z"),
        },
        // Never public: hidden, pending, deleted.
        Seed {
            slug: "hidden1",
            caster: "c-alpha",
            show: None,
            title: Some("Hidden"),
            started: "2026-11-19T10:00:00.000Z",
            ended: "2026-11-19T11:00:00.000Z",
            status: "ready",
            visible: 0,
            duration_ms: Some(3_600_000),
            format: Some("mp3"),
            published: None,
        },
        Seed {
            slug: "pending1",
            caster: "c-alpha",
            show: None,
            title: Some("Pending"),
            started: "2026-11-18T10:00:00.000Z",
            ended: "2026-11-18T11:00:00.000Z",
            status: "pending",
            visible: 1,
            duration_ms: None,
            format: None,
            published: None,
        },
        Seed {
            slug: "deleted1",
            caster: "c-alpha",
            show: None,
            title: Some("Deleted"),
            started: "2026-11-17T10:00:00.000Z",
            ended: "2026-11-17T11:00:00.000Z",
            status: "deleted",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: Some("mp3"),
            published: None,
        },
        // DST fall-back boundary tapes (2026-10-25).
        Seed {
            slug: "dst-earlier",
            caster: "c-beta",
            show: None,
            title: Some("DST earlier"),
            started: "2026-10-25T00:30:00.000Z",
            ended: "2026-10-25T01:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(1_800_000),
            format: Some("mp3"),
            published: Some("2026-10-25T01:00:00.000Z"),
        },
        Seed {
            slug: "dst-later",
            caster: "c-beta",
            show: None,
            title: Some("DST later"),
            started: "2026-10-25T01:30:00.000Z",
            ended: "2026-10-25T02:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(1_800_000),
            format: Some("mp3"),
            published: Some("2026-10-25T02:00:00.000Z"),
        },
        // Berlin-November boundary fixtures.
        Seed {
            slug: "nov-first",
            caster: "c-alpha",
            show: None,
            title: Some("November first"),
            started: "2026-11-01T12:00:00.000Z",
            ended: "2026-11-01T13:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: Some("mp3"),
            published: Some("2026-11-01T13:00:00.000Z"),
        },
        Seed {
            slug: "nov-second",
            caster: "c-alpha",
            show: None,
            title: Some("November second"),
            started: "2026-11-02T12:00:00.000Z",
            ended: "2026-11-02T13:00:00.000Z",
            status: "ready",
            visible: 1,
            duration_ms: Some(3_600_000),
            format: Some("mp3"),
            published: Some("2026-11-02T13:00:00.000Z"),
        },
    ]
}

async fn seed(pool: &SqlitePool) {
    for (id, username, display) in [
        ("c-alpha", "alpha", "Alpha"),
        ("c-beta", "beta", "Beta"),
        ("c-gamma", "gamma", "Gamma"),
    ] {
        sqlx::query(
            "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
             created_at, updated_at) VALUES (?, ?, ?, 'x', ?, 1, ?, ?)",
        )
        .bind(id)
        .bind(username)
        .bind(display)
        .bind(ROLE_CASTER)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert caster");
    }

    sqlx::query(
        "INSERT INTO shows (id, slug, name, description, created_by, created_at, updated_at) \
         VALUES ('s-deep', 'deep-cuts', 'Deep Cuts', '', 'c-alpha', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert show");

    for seed in seeds() {
        sqlx::query(
            "INSERT INTO tapes (id, slug, caster_id, show_id, title, started_at, ended_at, \
             duration_ms, source, format, mime, size_bytes, status, visible, published_at, \
             storage_key, etag, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'live', ?, ?, 1024, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(seed.slug)
        .bind(seed.slug)
        .bind(seed.caster)
        .bind(seed.show)
        .bind(seed.title)
        .bind(seed.started)
        .bind(seed.ended)
        .bind(seed.duration_ms)
        .bind(seed.format)
        .bind(seed.format.map(|_| "audio/mpeg".to_string()))
        .bind(seed.status)
        .bind(seed.visible)
        .bind(seed.published)
        .bind(format!("recordings/{}.mp3", seed.slug))
        .bind(format!("\"{}\"", seed.slug))
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert tape");
    }

    // A waveform for `aaa111` only.
    sqlx::query(
        "INSERT INTO waveforms (subject_kind, subject_id, scheme, algorithm, ms_per_bar, bars, \
         data, created_at) VALUES ('tape', 'aaa111', 1, 'ffmpeg-rms-500ms/1', 500, 14388, ?, ?)",
    )
    .bind(vec![0u8; 4])
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert waveform");

    // A caster (Gamma) whose only tape is hidden → must not appear.
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, title, started_at, ended_at, source, status, \
         visible, created_at, updated_at) \
         VALUES ('hidden2', 'hidden2', 'c-gamma', 'Gamma hidden', \
         '2026-11-05T10:00:00.000Z', '2026-11-05T11:00:00.000Z', 'live', 'ready', 0, ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert hidden gamma tape");
}

/// Builds the app over a fresh temp DB; returns the router plus the read-only
/// pool (for the read-only assertions).
async fn setup() -> (TempDir, Router, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");

    let writer = zukka_tapes::fixtures::create_db(&path)
        .await
        .expect("create db");
    seed(&writer).await;
    writer.close().await;

    let pool = zukka_tapes::db::connect_read_only(&path)
        .await
        .expect("read-only pool");
    let app = zukka_tapes::api::router(
        pool.clone(),
        BASE_URL.to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    (dir, app, pool)
}

#[tokio::test]
async fn guard_body_is_identical_for_hidden_and_unknown() {
    let (_dir, app, _pool) = setup().await;

    let (_s, _h, unknown) = get(&app, "/api/v1/tapes/does-not-exist").await;
    for hidden in ["hidden1", "pending1", "deleted1"] {
        let (status, _h, body) = get(&app, &format!("/api/v1/tapes/{hidden}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{hidden} must be 404");
        assert_eq!(body, unknown, "{hidden} body must equal unknown-slug body");
    }
    let value: Value = serde_json::from_str(&unknown).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(value["error"]["message"], "tape not found");
}

#[tokio::test]
async fn detail_json_matches_spec_and_hides_internal_fields() {
    let (_dir, app, _pool) = setup().await;
    let (status, value) = get_json(&app, "/api/v1/tapes/aaa111").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(value["slug"], "aaa111");
    assert_eq!(value["caster"]["id"], "c-alpha");
    assert_eq!(value["caster"]["name"], "Alpha");
    assert!(value["caster"]["accent"].is_null());
    assert_eq!(value["show"]["id"], "s-deep");
    assert_eq!(value["show"]["name"], "Deep Cuts");
    assert_eq!(value["title"], "My Title");
    assert_eq!(value["display_title"], "My Title");
    assert_eq!(value["tz"], "Europe/Berlin");
    assert_eq!(value["started_at"], "2026-11-23T10:00:00Z");
    assert_eq!(value["ended_at"], "2026-11-23T12:00:00Z");
    assert_eq!(value["start_local"], "2026-11-23T11:00:00+01:00");
    assert_eq!(value["end_local"], "2026-11-23T13:00:00+01:00");
    assert_eq!(value["window_label"], "23.11.2026 11:00 \u{2013} 13:00");
    assert_eq!(value["duration_ms"], 7_194_000);
    assert_eq!(value["source"], "live");
    assert_eq!(value["format"], "mp3");
    assert_eq!(value["mime"], "audio/mpeg");
    assert_eq!(value["size_bytes"], 1024);
    assert_eq!(value["audio_url"], "https://tapes.test/media/aaa111.mp3");
    assert_eq!(
        value["download_url"],
        "https://tapes.test/media/aaa111.mp3?download=1"
    );
    assert_eq!(
        value["waveform_url"],
        "https://tapes.test/api/v1/tapes/aaa111/waveform"
    );
    assert_eq!(value["share_url"], "https://tapes.test/t/aaa111");
    assert_eq!(value["peaks"]["scheme"], 1);
    assert_eq!(value["peaks"]["ms_per_bar"], 500);
    assert_eq!(value["peaks"]["bars"], 14388);
    assert_eq!(value["published_at"], "2026-11-23T12:04:11Z");
    assert_eq!(value["updated_at"], "2026-11-01T00:00:00Z");

    let object = value.as_object().expect("object");
    for forbidden in ["status", "visible", "storage_key", "object_key", "etag"] {
        assert!(
            !object.contains_key(forbidden),
            "field {forbidden} must never be public"
        );
    }
}

#[tokio::test]
async fn peaks_is_null_without_waveform() {
    let (_dir, app, _pool) = setup().await;
    let (status, value) = get_json(&app, "/api/v1/tapes/bbb222").await;
    assert_eq!(status, StatusCode::OK);
    assert!(value["peaks"].is_null());
}

#[tokio::test]
async fn display_title_fallback_chain() {
    let (_dir, app, _pool) = setup().await;
    for (slug, expected) in [
        ("aaa111", "My Title"),
        ("bbb222", "Deep Cuts"),
        ("ccc333", "Alpha"),
    ] {
        let (status, value) = get_json(&app, &format!("/api/v1/tapes/{slug}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["display_title"], expected, "display_title for {slug}");
    }
}

#[tokio::test]
async fn dst_offsets_are_explicit() {
    let (_dir, app, _pool) = setup().await;
    let (_s, earlier) = get_json(&app, "/api/v1/tapes/dst-earlier").await;
    // 2026-10-25T00:30:00Z is still CEST (+02:00) → 02:30 Berlin.
    assert_eq!(earlier["start_local"], "2026-10-25T02:30:00+02:00");
    let (_s, later) = get_json(&app, "/api/v1/tapes/dst-later").await;
    // 2026-10-25T01:30:00Z is CET (+01:00) → 02:30 Berlin.
    assert_eq!(later["start_local"], "2026-10-25T02:30:00+01:00");
}

#[tokio::test]
async fn feed_paginates_without_overlap_and_ends_with_null() {
    let (_dir, app, _pool) = setup().await;

    let (status, first) = get_json(&app, "/api/v1/tapes?limit=2&sort=oldest").await;
    assert_eq!(status, StatusCode::OK);
    let first_slugs: Vec<String> = first["tapes"]
        .as_array()
        .expect("array")
        .iter()
        .map(|t| t["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(first_slugs.len(), 2);
    assert_eq!(first["count"], 2);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("next_cursor")
        .to_string();

    let (status, second) = get_json(
        &app,
        &format!("/api/v1/tapes?limit=2&sort=oldest&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second_slugs: Vec<String> = second["tapes"]
        .as_array()
        .expect("array")
        .iter()
        .map(|t| t["slug"].as_str().unwrap().to_string())
        .collect();
    for slug in &second_slugs {
        assert!(!first_slugs.contains(slug), "no overlap: {slug}");
    }

    // A full page exhausts the feed → next_cursor is null.
    let (status, all) = get_json(&app, "/api/v1/tapes?limit=200").await;
    assert_eq!(status, StatusCode::OK);
    assert!(all["next_cursor"].is_null());
    assert!(all["server_time"].is_string());
}

#[tokio::test]
async fn feed_caster_filter_single_csv_and_repeat() {
    let (_dir, app, _pool) = setup().await;

    fn slugs(app_value: &Value) -> Vec<String> {
        app_value["tapes"]
            .as_array()
            .expect("array")
            .iter()
            .map(|t| t["slug"].as_str().unwrap().to_string())
            .collect()
    }

    // Single caster excludes the other one.
    let (status, only_alpha) = get_json(&app, "/api/v1/tapes?caster=c-alpha&limit=200").await;
    assert_eq!(status, StatusCode::OK);
    let alpha = slugs(&only_alpha);
    assert!(alpha.contains(&"aaa111".to_string()));
    assert!(!alpha.contains(&"ddd444".to_string()), "Beta excluded");

    // CSV and repeated forms both select the union.
    for query in ["caster=c-alpha,c-beta", "caster=c-alpha&caster=c-beta"] {
        let (status, value) = get_json(&app, &format!("/api/v1/tapes?{query}&limit=200")).await;
        assert_eq!(status, StatusCode::OK);
        let found = slugs(&value);
        assert!(found.contains(&"aaa111".to_string()), "{query}");
        assert!(found.contains(&"ddd444".to_string()), "{query}");
        assert!(!found.contains(&"hidden1".to_string()), "{query}");
    }
}

#[tokio::test]
async fn feed_from_to_uses_berlin_days() {
    let (_dir, app, _pool) = setup().await;
    let (status, value) = get_json(
        &app,
        "/api/v1/tapes?from=2026-11-01&to=2026-11-01&limit=200",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let slugs: Vec<&str> = value["tapes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"nov-first"));
    assert!(!slugs.contains(&"nov-second"), "to is exclusive");
}

#[tokio::test]
async fn bad_requests_return_envelope() {
    let (_dir, app, _pool) = setup().await;

    for uri in [
        "/api/v1/tapes?limit=0",
        "/api/v1/tapes?limit=201",
        "/api/v1/tapes?limit=abc",
        "/api/v1/tapes?from=2026-12-01&to=2026-11-01",
        "/api/v1/tapes?cursor=@@@not-base64@@@",
        "/api/v1/tapes?sort=sideways",
    ] {
        let (status, _h, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        let value: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["error"]["code"], "bad_request", "{uri}");
    }
}

#[tokio::test]
async fn unknown_route_is_enveloped_404() {
    let (_dir, app, _pool) = setup().await;
    let (status, _h, body) = get(&app, "/api/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let value: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
}

#[tokio::test]
async fn method_not_allowed_and_cors() {
    let (_dir, app, _pool) = setup().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/tapes")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(&app, request).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let allow = headers
        .get("allow")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(allow.contains("GET"), "Allow header: {allow}");
    assert!(allow.contains("HEAD"), "Allow header: {allow}");
    assert!(allow.contains("OPTIONS"), "Allow header: {allow}");

    // OPTIONS preflight.
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/tapes")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(&app, request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
    assert_eq!(
        headers.get("access-control-allow-methods").unwrap(),
        "GET, HEAD, OPTIONS"
    );

    // Every 200 carries ACAO.
    let (_s, headers, _body) = get(&app, "/api/v1/casters").await;
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
}

#[tokio::test]
async fn etag_304_and_cache_control() {
    let (_dir, app, _pool) = setup().await;

    let (status, headers, _body) = get(&app, "/api/v1/tapes/aaa111").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "public, max-age=300");
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();

    let (status, headers, body) = send(
        &app,
        build_get("/api/v1/tapes/aaa111", &[("if-none-match", &etag)]),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(headers.get("etag").unwrap().to_str().unwrap(), etag);
    assert_eq!(headers.get("cache-control").unwrap(), "public, max-age=300");

    // Feed also carries an ETag.
    let (_s, feed_headers, _b) = get(&app, "/api/v1/tapes").await;
    assert!(feed_headers.get("etag").is_some());
    assert_eq!(
        feed_headers.get("cache-control").unwrap(),
        "public, max-age=60"
    );
}

#[tokio::test]
async fn casters_aggregate_excludes_hidden_only() {
    let (_dir, app, _pool) = setup().await;
    let (status, value) = get_json(&app, "/api/v1/casters").await;
    assert_eq!(status, StatusCode::OK);

    let casters = value.as_array().expect("array");
    let names: Vec<&str> = casters
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alpha", "Beta"], "sorted by display_name");
    assert!(!names.contains(&"Gamma"), "hidden-only caster absent");

    let alpha = &casters[0];
    assert!(alpha["accent"].is_null());
    // Alpha: aaa111, bbb222, ccc333, nov-first, nov-second.
    assert_eq!(alpha["tape_count"], 5);
    assert_eq!(alpha["last_tape_at"], "2026-11-23T10:00:00Z");
    let beta = &casters[1];
    assert_eq!(beta["tape_count"], 3);
}

#[tokio::test]
async fn pool_is_truly_read_only() {
    let (_dir, _app, pool) = setup().await;
    let result = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, status, visible, \
         created_at, updated_at) \
         VALUES ('x', 'x', 'c-alpha', '2026-11-01T00:00:00.000Z', '2026-11-01T01:00:00.000Z', \
         'live', 'ready', 1, '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .execute(&pool)
    .await;
    assert!(
        result.is_err(),
        "write through the read-only pool must fail"
    );
    let message = format!("{}", result.unwrap_err());
    assert!(
        message.to_lowercase().contains("readonly") || message.contains("read-only"),
        "unexpected error: {message}"
    );
}
