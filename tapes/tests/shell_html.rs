//! HTTP-level tests for the server-rendered tape shell (Issue #26, M3).
//!
//! Coverage maps to the acceptance criteria: OG/Twitter meta (AK 18), real HTML
//! 404 (AK 19), XSS safety (plan R1/R2/R3), asset delivery (plan T26-09/11) and
//! the API regression that keeps `/api/v1/*` a JSON envelope.

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
const VERSION: &str = env!("CARGO_PKG_VERSION");

const XSS_TITLE: &str = "My <script>alert(1)</script> Title";
const XSS_CASTER: &str = "<img src=x onerror=alert(1)>";
const XSS_SHOW: &str = "\" onmouseover=\"alert(1)";
const XSS_DESCRIPTION: &str = "desc </script><script>alert(2)</script>";

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = app.clone().oneshot(request).await.expect("response");
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
    send(app, request(Method::GET, uri)).await
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Extracts the JSON island payload from the shell HTML.
fn tape_state(body: &str) -> String {
    let marker = "id=\"tape-state\">";
    let start = body.find(marker).expect("tape-state island") + marker.len();
    let rest = &body[start..];
    let end = rest.find("</script>").expect("closing script tag");
    rest[..end].to_string()
}

async fn seed(pool: &SqlitePool) {
    for (id, username, display) in [
        ("c-alpha", "alpha", "Alpha"),
        ("c-beta", "beta", "Beta"),
        ("c-xss", "xss", XSS_CASTER),
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

    for (id, slug, name) in [
        ("s-deep", "deep-cuts", "Deep Cuts"),
        ("s-xss", "xss-show", XSS_SHOW),
    ] {
        sqlx::query(
            "INSERT INTO shows (id, slug, name, description, created_by, created_at, updated_at) \
             VALUES (?, ?, ?, '', 'c-alpha', ?, ?)",
        )
        .bind(id)
        .bind(slug)
        .bind(name)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert show");
    }

    struct Row<'a> {
        slug: &'a str,
        caster: &'a str,
        show: Option<&'a str>,
        title: Option<&'a str>,
        description: Option<&'a str>,
        status: &'a str,
        visible: i64,
        duration: Option<i64>,
    }

    let rows = [
        Row {
            slug: "aaa111",
            caster: "c-alpha",
            show: Some("s-deep"),
            title: Some("My Title"),
            description: None,
            status: "ready",
            visible: 1,
            duration: Some(7_194_000),
        },
        Row {
            slug: "bbb222",
            caster: "c-alpha",
            show: Some("s-deep"),
            title: None,
            description: None,
            status: "ready",
            visible: 1,
            duration: Some(3_600_000),
        },
        Row {
            slug: "ccc333",
            caster: "c-alpha",
            show: None,
            title: None,
            description: None,
            status: "ready",
            visible: 1,
            duration: None,
        },
        Row {
            slug: "hidden1",
            caster: "c-alpha",
            show: None,
            title: Some("Hidden"),
            description: None,
            status: "ready",
            visible: 0,
            duration: Some(3_600_000),
        },
        Row {
            slug: "pending1",
            caster: "c-alpha",
            show: None,
            title: Some("Pending"),
            description: None,
            status: "pending",
            visible: 1,
            duration: None,
        },
        Row {
            slug: "deleted1",
            caster: "c-alpha",
            show: None,
            title: Some("Deleted"),
            description: None,
            status: "deleted",
            visible: 1,
            duration: Some(3_600_000),
        },
        Row {
            slug: "xss1",
            caster: "c-xss",
            show: Some("s-xss"),
            title: Some(XSS_TITLE),
            description: Some(XSS_DESCRIPTION),
            status: "ready",
            visible: 1,
            duration: Some(3_600_000),
        },
    ];

    for row in rows {
        sqlx::query(
            "INSERT INTO tapes (id, slug, caster_id, show_id, title, description, started_at, \
             ended_at, duration_ms, source, format, mime, size_bytes, status, visible, \
             published_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, '2026-11-23T10:00:00.000Z', '2026-11-23T12:00:00.000Z', \
             ?, 'live', 'mp3', 'audio/mpeg', 1024, ?, ?, ?, ?, ?)",
        )
        .bind(row.slug)
        .bind(row.slug)
        .bind(row.caster)
        .bind(row.show)
        .bind(row.title)
        .bind(row.description)
        .bind(row.duration)
        .bind(row.status)
        .bind(row.visible)
        .bind(if row.visible == 1 && row.status == "ready" {
            Some(TS)
        } else {
            None
        })
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert tape");
    }

    sqlx::query(
        "INSERT INTO waveforms (subject_kind, subject_id, scheme, algorithm, ms_per_bar, bars, \
         data, created_at) VALUES ('tape', 'aaa111', 1, 'ffmpeg-rms-500ms/1', 500, 14388, ?, ?)",
    )
    .bind(vec![0u8; 4])
    .bind(TS)
    .execute(pool)
    .await
    .expect("waveform");
}

async fn setup() -> (TempDir, Router) {
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
        pool,
        BASE_URL.to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    (dir, app)
}

#[tokio::test]
async fn index_renders_shell_with_assets_and_noscript() {
    let (_dir, app) = setup().await;
    let (status, headers, body) = get(&app, "/").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&headers, "cache-control"), "public, max-age=60");
    assert!(header(&headers, "content-security-policy").contains("script-src 'self'"));
    assert!(!header(&headers, "content-security-policy").contains("unsafe-inline"));

    assert!(body.contains("<meta charset=\"utf-8\""));
    assert!(body.contains("<noscript>"));
    assert!(body.contains("/assets/app.mjs?v="));
    assert!(body.contains(&format!("/assets/app.css?v={VERSION}")));
    assert!(body.contains(&format!("/assets/app.mjs?v={VERSION}")));
    assert!(!body.contains("<!--META-->"));
    assert!(!body.contains("<!--TAPE_STATE-->"));
    // The feed index carries `null`, so the island exists but holds no tape.
    assert_eq!(tape_state(&body), "null");
}

#[tokio::test]
async fn tape_page_has_meta_island_and_cache() {
    let (_dir, app) = setup().await;
    let (status, headers, body) = get(&app, "/t/aaa111").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&headers, "cache-control"), "public, max-age=60");

    let expected_title = "Alpha \u{2013} 23.11.2026 11:00 \u{2013} 13:00";
    assert!(body.contains(&format!(
        "<title>{expected_title} // radio zukka tapes</title>"
    )));
    assert!(body.contains(&format!(
        "<meta property=\"og:title\" content=\"{expected_title}\" />"
    )));
    assert!(
        body.contains(
            "<meta property=\"og:description\" content=\"Deep Cuts \u{00b7} 1:59:54\" />"
        )
    );
    assert!(body.contains("<meta property=\"og:url\" content=\"https://tapes.test/t/aaa111\" />"));
    assert!(body.contains(
        "<meta property=\"og:audio\" content=\"https://tapes.test/media/aaa111.mp3\" />"
    ));
    assert!(body.contains("<meta property=\"og:audio:type\" content=\"audio/mpeg\" />"));
    assert!(body.contains("<meta name=\"twitter:card\" content=\"summary_large_image\" />"));
    assert!(body.contains("<link rel=\"canonical\" href=\"https://tapes.test/t/aaa111\" />"));

    let island: Value = serde_json::from_str(&tape_state(&body)).expect("island json");
    assert_eq!(island["slug"], "aaa111");
    assert_eq!(island["audio_url"], "https://tapes.test/media/aaa111.mp3");
    assert_eq!(island["window_label"], "23.11.2026 11:00 \u{2013} 13:00");
}

#[tokio::test]
async fn meta_degrades_without_show_and_duration() {
    let (_dir, app) = setup().await;

    let (_s, _h, with_show) = get(&app, "/t/bbb222").await;
    assert!(
        with_show.contains(
            "<meta property=\"og:description\" content=\"Deep Cuts \u{00b7} 1:00:00\" />"
        )
    );

    let (_s, _h, without) = get(&app, "/t/ccc333").await;
    assert!(
        !without.contains("og:description"),
        "no trailing separator when show and duration are absent"
    );
    assert!(without.contains(
        "<title>Alpha \u{2013} 23.11.2026 11:00 \u{2013} 13:00 // radio zukka tapes</title>"
    ));
}

#[tokio::test]
async fn xss_is_escaped_everywhere() {
    let (_dir, app) = setup().await;
    let (status, _h, body) = get(&app, "/t/xss1").await;
    assert_eq!(status, StatusCode::OK);

    // Only the JSON island and the app module are executable scripts.
    assert_eq!(
        body.matches("<script").count(),
        2,
        "no injected <script> may survive"
    );
    assert!(!body.contains("<script>alert"));
    assert!(!body.contains("<img src=x onerror"));

    // Values are present, but entity-escaped.
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(body.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(body.contains("&quot; onmouseover=&quot;alert(1)"));

    // The JSON island stays parseable and carries the exact original title.
    let island: Value = serde_json::from_str(&tape_state(&body)).expect("island parseable");
    assert_eq!(island["title"], XSS_TITLE);
    assert_eq!(island["description"], XSS_DESCRIPTION);
    assert!(!tape_state(&body).contains("</script"));
}

#[tokio::test]
async fn unknown_and_hidden_share_one_html_404() {
    let (_dir, app) = setup().await;

    let (status, headers, unknown) = get(&app, "/t/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    // No slug is reflected.
    assert!(!unknown.contains("does-not-exist"));

    for path in [
        "/t/hidden1",
        "/t/pending1",
        "/t/deleted1",
        "/some/unknown/path",
    ] {
        let (status, _h, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, unknown, "{path} must be byte-identical");
    }
}

#[tokio::test]
async fn encoded_script_slug_is_not_reflected() {
    let (_dir, app) = setup().await;
    let (status, _h, body) = get(&app, "/t/%3Cscript%3Ealert(1)%3C%2Fscript%3E").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("<script>alert"));
    assert!(!body.contains("alert(1)"));
    assert!(!body.contains("<script"));
}

#[tokio::test]
async fn assets_are_served_immutable_with_types() {
    let (_dir, app) = setup().await;

    for (uri, content_type) in [
        ("/assets/app.css?v=0.1.0", "text/css; charset=utf-8"),
        ("/assets/app.mjs?v=0.1.0", "text/javascript; charset=utf-8"),
        ("/assets/peaks.mjs", "text/javascript; charset=utf-8"),
        ("/assets/player.mjs", "text/javascript; charset=utf-8"),
        ("/assets/fonts/VT323.woff2", "font/woff2"),
        ("/assets/fonts/SpaceMono-Regular.woff2", "font/woff2"),
        ("/assets/fonts/OFL.txt", "text/plain; charset=utf-8"),
    ] {
        let (status, headers, _body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header(&headers, "content-type"), content_type, "{uri}");
        assert_eq!(
            header(&headers, "cache-control"),
            "public, max-age=31536000, immutable",
            "{uri}"
        );
        assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    }

    let (status, _h, _b) = get(&app, "/assets/nope.css").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_unknown_path_stays_json() {
    let (_dir, app) = setup().await;
    let (status, headers, body) = get(&app, "/api/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(header(&headers, "content-type").contains("application/json"));
    let value: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
}

#[test]
fn static_files_have_no_external_urls() {
    for (name, content) in [
        ("index.html", include_str!("../assets/index.html")),
        ("app.css", include_str!("../assets/app.css")),
        ("app.mjs", include_str!("../assets/app.mjs")),
        ("peaks.mjs", include_str!("../assets/peaks.mjs")),
        ("player.mjs", include_str!("../assets/player.mjs")),
    ] {
        assert!(
            !content.contains("http://") && !content.contains("https://"),
            "{name} must not reference an external URL"
        );
    }
}

#[test]
fn template_markers_are_unique_except_version() {
    let template = include_str!("../assets/index.html");
    assert_eq!(template.matches("<!--META-->").count(), 1);
    assert_eq!(template.matches("<!--NOSCRIPT-->").count(), 1);
    assert_eq!(template.matches("<!--TAPE_STATE-->").count(), 1);
    // The version marker appears once per versioned asset URL.
    assert_eq!(template.matches("<!--APP_VERSION-->").count(), 2);
    // No inline script or style.
    assert!(!template.contains("style=\""), "no inline style attribute");
    assert!(!template.contains("onclick="), "no inline event handlers");
}
