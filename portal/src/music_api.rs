//! Music API client (Issue #18, US-3; portal.md §3.5; Issue #134).
//!
//! The Music API is the file authority: the portal never stores audio. It is a
//! content-addressed object store with **no `/api/v1` prefix** — the routes
//! verified against `music-api` are:
//!
//! - `PUT /objects/{sha256}` — binary body (streamed), optional headers
//!   `X-ISRC`, `X-Name`, `X-Original-Path`; idempotent (`{"stored":false,
//!   "present":true}` when the object already exists).
//! - `HEAD /objects/{sha256}` — `200` present / `404` absent.
//! - `POST /work` — `{"kind":"analyze"|"download","ref":…}`.
//!
//! There is no `POST /tracks`, no `PUT /tracks/{id}/asset` and no
//! `by-hash`/search route. Every call goes through the [`MusicApi`] trait so a
//! divergent contract touches exactly this file, and so tests run without a
//! network (see [`FakeMusicApi`]).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

/// One `PUT /objects/{sha256}` payload (master or derivate).
#[derive(Debug, Clone, Copy)]
pub struct ObjectUpload<'a> {
    /// Lowercase-hex sha256 of the bytes (the object key).
    pub sha256: &'a str,
    /// Local file to stream as the request body.
    pub path: &'a Path,
    /// `Content-Type` of the file.
    pub content_type: &'a str,
    /// Optional `X-ISRC` header.
    pub isrc: Option<&'a str>,
    /// Optional `X-Name` header (title).
    pub name: Option<&'a str>,
    /// Optional `X-Original-Path` header (original filename).
    pub original_path: Option<&'a str>,
}

/// Result of a [`MusicApi::put_object`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The object was newly stored (`{"stored":true,…}`).
    Stored,
    /// The object already existed (idempotent hit).
    Present,
}

/// Music API failure.
#[derive(Debug, thiserror::Error)]
pub enum MusicApiError {
    /// The object does not exist (HTTP `404`).
    #[error("object not found")]
    NotFound,
    /// Transport, parse, or unexpected-status failure.
    #[error("music api error: {0}")]
    Upstream(String),
    /// Local I/O failure while reading an asset.
    #[error("io error: {0}")]
    Io(String),
}

/// Music API operations the upload pipeline needs (portal.md §3.5).
#[async_trait]
pub trait MusicApi: Send + Sync {
    /// Dedupe lookup: `HEAD /objects/{sha256}` (`200` ⇒ `true`, `404` ⇒ `false`).
    async fn has_object(&self, sha256: &str) -> Result<bool, MusicApiError>;

    /// Uploads one object (`PUT /objects/{sha256}`), streaming from disk.
    async fn put_object(&self, upload: &ObjectUpload<'_>) -> Result<StoreOutcome, MusicApiError>;

    /// Enqueues Music-API work (`POST /work`).
    async fn enqueue_work(&self, kind: &str, reference: &str) -> Result<(), MusicApiError>;
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// HTTP client for the Music API, authenticated with a bearer token.
pub struct HttpMusicApi {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl HttpMusicApi {
    /// Builds a client against `base_url` (e.g. `http://music-api:8082`).
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
        })
    }

    /// No `/api/v1` prefix: the real Music API serves routes at the root (E1).
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn error(status: reqwest::StatusCode) -> MusicApiError {
        if status == reqwest::StatusCode::NOT_FOUND {
            MusicApiError::NotFound
        } else {
            MusicApiError::Upstream(format!("music api returned {status}"))
        }
    }
}

#[derive(Debug, Deserialize)]
struct PutResponse {
    #[serde(default)]
    stored: bool,
}

#[async_trait]
impl MusicApi for HttpMusicApi {
    async fn has_object(&self, sha256: &str) -> Result<bool, MusicApiError> {
        let response = self
            .client
            .head(self.url(&format!("/objects/{sha256}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        match response.status() {
            reqwest::StatusCode::OK => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            other => Err(Self::error(other)),
        }
    }

    async fn put_object(&self, upload: &ObjectUpload<'_>) -> Result<StoreOutcome, MusicApiError> {
        // Streams from disk (never buffers the master in RAM, D6).
        let file = tokio::fs::File::open(upload.path)
            .await
            .map_err(|e| MusicApiError::Io(e.to_string()))?;
        let mut request = self
            .client
            .put(self.url(&format!("/objects/{}", upload.sha256)))
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, upload.content_type)
            .body(reqwest::Body::from(file));
        if let Some(isrc) = upload.isrc {
            request = request.header("x-isrc", isrc);
        }
        if let Some(name) = upload.name {
            request = request.header("x-name", name);
        }
        if let Some(original_path) = upload.original_path {
            request = request.header("x-original-path", original_path);
        }
        let response = request
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error(response.status()));
        }
        let body: PutResponse = response
            .json()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        Ok(if body.stored {
            StoreOutcome::Stored
        } else {
            StoreOutcome::Present
        })
    }

    async fn enqueue_work(&self, kind: &str, reference: &str) -> Result<(), MusicApiError> {
        let response = self
            .client
            .post(self.url("/work"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "kind": kind, "ref": reference }))
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error(response.status()))
        }
    }
}

// ---------------------------------------------------------------------------
// Fake (tests, no network)
// ---------------------------------------------------------------------------

/// One recorded `put_object` call.
#[derive(Debug, Clone)]
pub struct PutRecord {
    pub sha256: String,
    pub path: String,
    pub isrc: Option<String>,
    pub name: Option<String>,
    pub original_path: Option<String>,
}

#[derive(Default)]
struct FakeInner {
    /// Present object keys (dedupe set).
    objects: HashSet<String>,
    /// Every `put_object` call, in order.
    puts: Vec<PutRecord>,
    /// Number of calls that actually stored a new object.
    stored: usize,
    /// `(kind, ref)` for every `enqueue_work` call.
    work: Vec<(String, String)>,
    /// Every `has_object` lookup key.
    heads: Vec<String>,
}

/// In-memory [`MusicApi`] for tests. Dedupe mirrors the real
/// `HEAD /objects/{sha256}` contract; seed objects with [`seed_object`].
///
/// [`seed_object`]: FakeMusicApi::seed_object
#[derive(Default)]
pub struct FakeMusicApi {
    inner: Mutex<FakeInner>,
}

impl FakeMusicApi {
    /// A fresh, empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `sha256` as an already-present object (a `HEAD` hit).
    pub fn seed_object(&self, sha256: &str) {
        self.inner
            .lock()
            .unwrap()
            .objects
            .insert(sha256.to_string());
    }

    /// Number of `put_object` calls so far.
    pub fn put_count(&self) -> usize {
        self.inner.lock().unwrap().puts.len()
    }

    /// The local paths recorded, in call order.
    pub fn put_paths(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .puts
            .iter()
            .map(|p| p.path.clone())
            .collect()
    }

    /// The `X-ISRC` value recorded per `put_object`, in call order.
    pub fn put_isrcs(&self) -> Vec<Option<String>> {
        self.inner
            .lock()
            .unwrap()
            .puts
            .iter()
            .map(|p| p.isrc.clone())
            .collect()
    }

    /// Every recorded `put_object` call (sha256, path and headers), in order.
    pub fn put_records(&self) -> Vec<PutRecord> {
        self.inner.lock().unwrap().puts.clone()
    }

    /// Number of `put_object` calls that stored a *new* object.
    pub fn stored_count(&self) -> usize {
        self.inner.lock().unwrap().stored
    }

    /// `(kind, ref)` for every `enqueue_work` call, in order.
    pub fn work_items(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().work.clone()
    }

    /// Every `has_object` lookup key, in call order.
    pub fn head_requests(&self) -> Vec<String> {
        self.inner.lock().unwrap().heads.clone()
    }
}

#[async_trait]
impl MusicApi for FakeMusicApi {
    async fn has_object(&self, sha256: &str) -> Result<bool, MusicApiError> {
        let mut inner = self.inner.lock().unwrap();
        inner.heads.push(sha256.to_string());
        Ok(inner.objects.contains(sha256))
    }

    async fn put_object(&self, upload: &ObjectUpload<'_>) -> Result<StoreOutcome, MusicApiError> {
        let mut inner = self.inner.lock().unwrap();
        inner.puts.push(PutRecord {
            sha256: upload.sha256.to_string(),
            path: upload.path.to_string_lossy().into_owned(),
            isrc: upload.isrc.map(str::to_string),
            name: upload.name.map(str::to_string),
            original_path: upload.original_path.map(str::to_string),
        });
        if inner.objects.insert(upload.sha256.to_string()) {
            inner.stored += 1;
            Ok(StoreOutcome::Stored)
        } else {
            Ok(StoreOutcome::Present)
        }
    }

    async fn enqueue_work(&self, kind: &str, reference: &str) -> Result<(), MusicApiError> {
        self.inner
            .lock()
            .unwrap()
            .work
            .push((kind.to_string(), reference.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_put_and_head() {
        let api = FakeMusicApi::new();
        assert!(!api.has_object("abc").await.unwrap());

        let outcome = api
            .put_object(&ObjectUpload {
                sha256: "abc",
                path: Path::new("/tmp/a.flac"),
                content_type: "audio/flac",
                isrc: Some("DEZZ00000001"),
                name: Some("Song"),
                original_path: Some("a.flac"),
            })
            .await
            .unwrap();
        assert_eq!(outcome, StoreOutcome::Stored);
        assert_eq!(api.stored_count(), 1);

        // Idempotent: a second PUT of the same key is `Present`.
        let again = api
            .put_object(&ObjectUpload {
                sha256: "abc",
                path: Path::new("/tmp/a.flac"),
                content_type: "audio/flac",
                isrc: None,
                name: None,
                original_path: None,
            })
            .await
            .unwrap();
        assert_eq!(again, StoreOutcome::Present);
        assert_eq!(api.stored_count(), 1);

        // `has_object` now sees it and recorded both lookups.
        assert!(api.has_object("abc").await.unwrap());
        assert_eq!(
            api.head_requests(),
            vec!["abc".to_string(), "abc".to_string()]
        );
    }

    #[tokio::test]
    async fn fake_records_put_paths() {
        let api = FakeMusicApi::new();
        api.put_object(&ObjectUpload {
            sha256: "h1",
            path: Path::new("/tmp/a.flac"),
            content_type: "audio/flac",
            isrc: Some("US-1"),
            name: None,
            original_path: None,
        })
        .await
        .unwrap();
        api.put_object(&ObjectUpload {
            sha256: "h2",
            path: Path::new("/tmp/a.mp3"),
            content_type: "audio/mpeg",
            isrc: Some("US-1"),
            name: None,
            original_path: None,
        })
        .await
        .unwrap();
        assert_eq!(api.put_count(), 2);
        assert_eq!(
            api.put_paths(),
            vec!["/tmp/a.flac".to_string(), "/tmp/a.mp3".to_string()]
        );
        assert_eq!(
            api.put_isrcs(),
            vec![Some("US-1".to_string()), Some("US-1".to_string())]
        );
    }

    #[tokio::test]
    async fn fake_records_work() {
        let api = FakeMusicApi::new();
        api.enqueue_work("analyze", "abc").await.unwrap();
        assert_eq!(
            api.work_items(),
            vec![("analyze".to_string(), "abc".to_string())]
        );
    }
}
