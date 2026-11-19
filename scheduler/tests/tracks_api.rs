//! Store + HTTP + visibility tests for tracks (Issue #19: US-1, US-2).
//!
//! Store tests run `zukka_scheduler::store::tracks` directly against a migrated
//! temporary SQLite database; API tests go through `zukka_scheduler::api::router`
//! with `tower::ServiceExt::oneshot`. The viewer context is the `X-Caster-Id`
//! header (§4.1).
//!
//! The **§1 visibility matrix** is the core acceptance: a foreign private track
//! is invisible in list/search *and* via a direct URL (`404`, never `403`).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::dto::{
    CreateTrackRequest, ImportTrackRequest, UpdateTrackRequest, Visibility,
};
use zukka_scheduler::store::tracks;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";
const CASTER_B: &str = "0192bbbb-0001-7000-8000-00000000000b";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn caster(pool: &SqlitePool, id: &str) {
    let username = format!("user_{}", id.replace('-', ""));
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    viewer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(viewer) = viewer {
        builder = builder.header("x-caster-id", viewer);
    }
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn create_req(title: &str, visibility: Option<&str>, caster: Option<&str>) -> CreateTrackRequest {
    CreateTrackRequest {
        music_api_id: format!("music-{title}"),
        title: title.to_string(),
        storage_key: format!("key/{title}"),
        duration_ms: 1000,
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        isrc: None,
        artwork_url: None,
        visibility: visibility.map(str::to_string),
        caster_id: caster.map(str::to_string),
        metadata: None,
    }
}

// ---------------------------------------------------------------------------
// US-1: store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_create_get_update_delete() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;

    let created = tracks::create(&pool, &create_req("Song", None, Some(CASTER_A)))
        .await
        .expect("create");
    assert_eq!(created.title, "Song");
    assert_eq!(created.artist, "Artist");
    assert_eq!(created.visibility, Visibility::Shared, "default shared");
    assert_eq!(created.caster_id.as_deref(), Some(CASTER_A));
    assert_eq!(created.id.len(), 36);

    let fetched = tracks::get(&pool, &created.id, Some(CASTER_A))
        .await
        .expect("get");
    assert_eq!(fetched.id, created.id);

    let updated = tracks::update(
        &pool,
        &created.id,
        &UpdateTrackRequest {
            title: Some("Renamed".into()),
            artist: None,
            album: None,
            artwork_url: None,
            visibility: Some("private".into()),
            caster_id: None,
        },
        Some(CASTER_A),
    )
    .await
    .expect("update");
    assert_eq!(updated.title, "Renamed");
    assert_eq!(updated.visibility, Visibility::Private);

    tracks::delete(&pool, &created.id, Some(CASTER_A))
        .await
        .expect("delete");
    assert!(
        tracks::get(&pool, &created.id, Some(CASTER_A))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn store_validation_and_duplicate() {
    let (_dir, pool) = setup().await;

    let mut req = create_req("Song", None, None);
    req.title = "   ".into();
    let err = tracks::create(&pool, &req).await.unwrap_err();
    assert!(
        matches!(err, zukka_scheduler::error::ApiError::Validation { field, .. } if field == "title")
    );

    let mut req = create_req("Song", None, None);
    req.duration_ms = -1;
    let err = tracks::create(&pool, &req).await.unwrap_err();
    assert!(
        matches!(err, zukka_scheduler::error::ApiError::Validation { field, .. } if field == "duration_ms")
    );

    tracks::create(&pool, &create_req("Dup", None, None))
        .await
        .expect("first");
    let err = tracks::create(&pool, &create_req("Dup", None, None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, zukka_scheduler::error::ApiError::Conflict { code, .. } if code == "duplicate_track")
    );
}

#[tokio::test]
async fn store_import_and_duplicate() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let req = ImportTrackRequest {
        music_api_id: "m-1".into(),
        title: "Imported".into(),
        storage_key: "key/1".into(),
        duration_ms: 500,
        artist: Some("A".into()),
        album: None,
        isrc: None,
        artwork_url: None,
        visibility: Some("private".into()),
        caster_id: Some(CASTER_A.into()),
    };
    let track = tracks::import(&pool, &req).await.expect("import");
    assert_eq!(track.visibility, Visibility::Private);

    let err = tracks::import(&pool, &req).await.unwrap_err();
    assert!(
        matches!(err, zukka_scheduler::error::ApiError::Conflict { code, .. } if code == "duplicate_track")
    );
}

// ---------------------------------------------------------------------------
// US-2: HTTP API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_create_list_patch_delete_roundtrip() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/tracks",
        None,
        Some(json!({
            "music_api_id": "m-1",
            "title": "Song",
            "storage_key": "key/1",
            "duration_ms": 1000,
            "visibility": "shared",
            "caster_id": CASTER_A,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["visibility"], "shared");

    let (status, list) = send(&app, Method::GET, "/api/v1/tracks", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, one) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["title"], "Song");

    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{id}"),
        Some(CASTER_A),
        Some(json!({ "title": "Renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["title"], "Renamed");

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/tracks/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_create_field_error_is_422() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/tracks",
        None,
        Some(json!({
            "music_api_id": "m-1",
            "title": "",
            "storage_key": "key/1",
            "duration_ms": 0,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "title");
}

#[tokio::test]
async fn api_import_and_duplicate() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    let payload = json!({
        "music_api_id": "m-imp",
        "title": "Imported",
        "storage_key": "key/i",
        "duration_ms": 10,
    });
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/tracks/import",
        None,
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/tracks/import",
        None,
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_track");
}

// ---------------------------------------------------------------------------
// §1 visibility matrix (core acceptance)
// ---------------------------------------------------------------------------

/// Seeds: shared track + A-private + B-private. Returns `(shared_id, a_priv, b_priv)`.
async fn seed_visibility(pool: &SqlitePool) -> (String, String, String) {
    caster(pool, CASTER_A).await;
    caster(pool, CASTER_B).await;
    let shared = tracks::create(pool, &create_req("Shared", None, Some(CASTER_A)))
        .await
        .unwrap();
    let a_priv = tracks::create(
        pool,
        &create_req("A Secret", Some("private"), Some(CASTER_A)),
    )
    .await
    .unwrap();
    let b_priv = tracks::create(
        pool,
        &create_req("B Secret", Some("private"), Some(CASTER_B)),
    )
    .await
    .unwrap();
    (shared.id, a_priv.id, b_priv.id)
}

#[tokio::test]
async fn visibility_matrix() {
    let (_dir, pool) = setup().await;
    let (shared, a_priv, b_priv) = seed_visibility(&pool).await;
    let app = app(pool);

    // list as A → shared + A-private; B-private absent.
    let (status, list) = send(&app, Method::GET, "/api/v1/tracks", Some(CASTER_A), None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&shared));
    assert!(ids.contains(&a_priv));
    assert!(!ids.contains(&b_priv), "B-private must be hidden from A");

    // list with no header → only shared.
    let (_status, list) = send(&app, Method::GET, "/api/v1/tracks", None, None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        1,
        "only shared without viewer"
    );
    assert_eq!(list[0]["id"], shared);

    // search q=<B-private title> as A → no hit.
    let (status, hits) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?q=B%20Secret",
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        hits.as_array().unwrap().is_empty(),
        "foreign private not searchable"
    );

    // get(B-private) as A → 404.
    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{b_priv}"),
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");

    // get(B-private) as B → 200.
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{b_priv}"),
        Some(CASTER_B),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // get(A-private) with no header → 404.
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{a_priv}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Filter visibility=private as A → only A-private.
    let (status, list) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?visibility=private",
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![a_priv.as_str()]);
}

#[tokio::test]
async fn owner_private_toggle_hides_from_others() {
    let (_dir, pool) = setup().await;
    let (_shared, a_priv, _b_priv) = seed_visibility(&pool).await;
    let app = app(pool);

    // A toggles its own shared→private track; afterwards B cannot see it.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{a_priv}"),
        Some(CASTER_A),
        Some(json!({ "visibility": "shared" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{a_priv}"),
        Some(CASTER_B),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "shared is visible to B");

    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{a_priv}"),
        Some(CASTER_A),
        Some(json!({ "visibility": "private" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks/{a_priv}"),
        Some(CASTER_B),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private hidden again");
}

#[tokio::test]
async fn mutation_of_foreign_private_is_404() {
    let (_dir, pool) = setup().await;
    let (_shared, _a_priv, b_priv) = seed_visibility(&pool).await;
    let app = app(pool);

    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{b_priv}"),
        Some(CASTER_A),
        Some(json!({ "title": "hijack" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/tracks/{b_priv}"),
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shared_track_update_is_owner_only() {
    let (_dir, pool) = setup().await;
    let (shared, _a_priv, _b_priv) = seed_visibility(&pool).await;
    let app = app(pool.clone());

    // B sees the shared track but must not PATCH it: 404, no change.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{shared}"),
        Some(CASTER_B),
        Some(json!({ "title": "hijacked", "visibility": "private" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let unchanged = tracks::get(&pool, &shared, Some(CASTER_A))
        .await
        .expect("track still exists");
    assert_eq!(unchanged.title, "Shared", "title untouched");
    assert_eq!(
        unchanged.visibility,
        Visibility::Shared,
        "visibility untouched"
    );

    // The owner may still PATCH its own shared track.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{shared}"),
        Some(CASTER_A),
        Some(json!({ "title": "Owner Renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Owner Renamed");

    // Request without a viewer cannot match the owner scope → 404.
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{shared}"),
        None,
        Some(json!({ "title": "anon" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Uebersicht der verbleibenden Endpunkt-Codes + Query-Filter (US-2)
// ---------------------------------------------------------------------------

/// `POST /tracks` with an already used `music_api_id` → `409 duplicate_track`
/// (the store path is covered above; this exercises the HTTP envelope).
#[tokio::test]
async fn api_create_duplicate_music_api_id_is_409() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    let payload = json!({
        "music_api_id": "dup-1",
        "title": "Song",
        "storage_key": "key/1",
        "duration_ms": 10,
    });
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/tracks",
        None,
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = send(&app, Method::POST, "/api/v1/tracks", None, Some(payload)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_track");
}

/// `PATCH`/`DELETE` on an id nobody owns → `404` (never `403`).
#[tokio::test]
async fn api_patch_and_delete_unknown_is_404() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let ghost = "0192cccc-0001-7000-8000-00000000000f";

    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/tracks/{ghost}"),
        Some(CASTER_A),
        Some(json!({ "title": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/tracks/{ghost}"),
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `q` searches `title|artist|album|isrc` case-insensitively.
#[tokio::test]
async fn search_matches_artist_album_isrc_case_insensitive() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let mut a = create_req("Alpha", None, None);
    a.artist = Some("Nina Simone".into());
    a.album = Some("Pastel Blues".into());
    a.isrc = Some("USABC1234567".into());
    tracks::create(&pool, &a).await.expect("alpha");

    let mut b = create_req("Beta", None, None);
    b.artist = Some("Miles Davis".into());
    b.album = Some("Kind of Blue".into());
    b.isrc = Some("USDEF7654321".into());
    tracks::create(&pool, &b).await.expect("beta");

    // Each needle matches exactly one track, regardless of case / field.
    for q in [
        "Alpha",
        "nina",
        "PASTEL",
        "usabc",
        "Kind%20of%20Blue",
        "MILES",
    ] {
        let (status, hits) = send(
            &app,
            Method::GET,
            &format!("/api/v1/tracks?q={q}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hits.as_array().unwrap().len(), 1, "q={q}: {hits}");
    }

    // A needle that matches nothing yields an empty list.
    let (status, hits) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?q=zzzznomatch",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(hits.as_array().unwrap().is_empty());
}

/// `owner_caster_id` / `visibility` filters stay inside the §1 scope, and
/// `limit`/`offset` page the result.
#[tokio::test]
async fn list_owner_visibility_filter_and_paging() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    caster(&pool, CASTER_B).await;

    for i in 0..3 {
        let mut req = create_req(&format!("A Shared {i}"), None, Some(CASTER_A));
        req.music_api_id = format!("a-shared-{i}");
        tracks::create(&pool, &req).await.unwrap();
    }
    let mut a_priv = create_req("A Secret", Some("private"), Some(CASTER_A));
    a_priv.music_api_id = "a-priv".into();
    tracks::create(&pool, &a_priv).await.unwrap();

    let mut b_shared = create_req("B Shared", None, Some(CASTER_B));
    b_shared.music_api_id = "b-shared".into();
    tracks::create(&pool, &b_shared).await.unwrap();

    let app = app(pool);
    let count = |v: &Value| v.as_array().unwrap().len();

    // owner_caster_id=A as A → 3 shared + 1 private = 4.
    let (status, hits) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks?owner_caster_id={CASTER_A}"),
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&hits), 4, "{hits}");

    // owner_caster_id=A as B → only the 3 shared (A-private stays hidden).
    let (status, hits) = send(
        &app,
        Method::GET,
        &format!("/api/v1/tracks?owner_caster_id={CASTER_A}"),
        Some(CASTER_B),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&hits), 3, "foreign private excluded: {hits}");

    // visibility=shared as A → 3 A-shared + 1 B-shared = 4 (A-private out).
    let (status, hits) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?visibility=shared",
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&hits), 4, "{hits}");

    // paging: limit=2 → 2 rows; a far offset → none.
    let (_, hits) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?limit=2",
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(count(&hits), 2, "{hits}");
    let (_, hits) = send(
        &app,
        Method::GET,
        "/api/v1/tracks?offset=100",
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(count(&hits), 0, "{hits}");
}
