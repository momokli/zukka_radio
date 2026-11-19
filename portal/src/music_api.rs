//! Music API client (Issue #18, US-3; portal.md §3.5).
//!
//! The Music API is the file authority: the portal never stores audio. Every
//! call goes through the [`MusicApi`] trait so a divergent contract (open
//! question Q1) touches exactly this file, and so tests run without a network
//! (see [`FakeMusicApi`]).

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A track as returned by the Music API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackDto {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub stream_url: Option<String>,
}

/// Metadata for a new track (`POST /tracks`).
#[derive(Debug, Clone)]
pub struct NewTrack {
    pub title: String,
    pub artist: Option<String>,
    pub sha256: Option<String>,
}

/// Music API failure.
#[derive(Debug, thiserror::Error)]
pub enum MusicApiError {
    /// The track does not exist (HTTP `404`).
    #[error("track not found")]
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
    /// Creates track metadata and returns the created track.
    async fn create_track(&self, track: &NewTrack) -> Result<TrackDto, MusicApiError>;

    /// Uploads the binary asset (master or derivate) for `track_id`.
    async fn upload_asset(
        &self,
        track_id: &str,
        path: &Path,
        content_type: &str,
    ) -> Result<(), MusicApiError>;

    /// Fetches a track by id.
    async fn get_track(&self, track_id: &str) -> Result<TrackDto, MusicApiError>;

    /// Searches tracks (`GET /tracks?q=&limit=&offset=`).
    async fn search(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackDto>, MusicApiError>;

    /// Hash-based dedupe lookup (`GET /tracks/by-hash/{sha256}`); `None` on
    /// `404`.
    async fn find_by_hash(&self, sha256: &str) -> Result<Option<TrackDto>, MusicApiError>;

    /// Deletes a track.
    async fn delete(&self, track_id: &str) -> Result<(), MusicApiError>;
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

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base_url)
    }

    fn error(status: reqwest::StatusCode) -> MusicApiError {
        if status == reqwest::StatusCode::NOT_FOUND {
            MusicApiError::NotFound
        } else {
            MusicApiError::Upstream(format!("music api returned {status}"))
        }
    }
}

#[async_trait]
impl MusicApi for HttpMusicApi {
    async fn create_track(&self, track: &NewTrack) -> Result<TrackDto, MusicApiError> {
        let response = self
            .client
            .post(self.url("/tracks"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "title": track.title,
                "artist": track.artist,
                "sha256": track.sha256,
            }))
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error(response.status()));
        }
        response
            .json()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))
    }

    async fn upload_asset(
        &self,
        track_id: &str,
        path: &Path,
        content_type: &str,
    ) -> Result<(), MusicApiError> {
        // Streams from disk (never buffers the master in RAM, D6).
        let part = reqwest::multipart::Part::file(path)
            .await
            .map_err(|e| MusicApiError::Io(e.to_string()))?
            .file_name(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "asset".to_string()),
            )
            .mime_str(content_type)
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .client
            .put(self.url(&format!("/tracks/{track_id}/asset")))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error(response.status()))
        }
    }

    async fn get_track(&self, track_id: &str) -> Result<TrackDto, MusicApiError> {
        let response = self
            .client
            .get(self.url(&format!("/tracks/{track_id}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error(response.status()));
        }
        response
            .json()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))
    }

    async fn search(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackDto>, MusicApiError> {
        let response = self
            .client
            .get(self.url("/tracks"))
            .bearer_auth(&self.token)
            .query(&[
                ("q", query.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ])
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error(response.status()));
        }
        response
            .json()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))
    }

    async fn find_by_hash(&self, sha256: &str) -> Result<Option<TrackDto>, MusicApiError> {
        let response = self
            .client
            .get(self.url(&format!("/tracks/by-hash/{sha256}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::error(response.status()));
        }
        let track = response
            .json()
            .await
            .map_err(|e| MusicApiError::Upstream(e.to_string()))?;
        Ok(Some(track))
    }

    async fn delete(&self, track_id: &str) -> Result<(), MusicApiError> {
        let response = self
            .client
            .delete(self.url(&format!("/tracks/{track_id}")))
            .bearer_auth(&self.token)
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

#[derive(Default)]
struct FakeInner {
    tracks: Vec<TrackDto>,
    /// `(track_id, asset_path)` for every `upload_asset` call.
    assets: Vec<(String, String)>,
    counter: u64,
}

/// In-memory [`MusicApi`] for tests. Dedupe by `sha256` mirrors the real
/// `GET /tracks/by-hash/{sha256}` contract.
#[derive(Default)]
pub struct FakeMusicApi {
    inner: Mutex<FakeInner>,
}

impl FakeMusicApi {
    /// A fresh, empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of `create_track` calls so far (dedupe assertions).
    pub fn created_count(&self) -> usize {
        self.inner.lock().unwrap().tracks.len()
    }

    /// Number of `upload_asset` calls so far.
    pub fn asset_count(&self) -> usize {
        self.inner.lock().unwrap().assets.len()
    }

    /// The asset paths recorded, in call order.
    pub fn asset_paths(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .assets
            .iter()
            .map(|(_, path)| path.clone())
            .collect()
    }

    /// The sha256 recorded for `track_id`, if any.
    pub fn sha256_of(&self, track_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .and_then(|t| t.sha256.clone())
    }
}

#[async_trait]
impl MusicApi for FakeMusicApi {
    async fn create_track(&self, track: &NewTrack) -> Result<TrackDto, MusicApiError> {
        let mut inner = self.inner.lock().unwrap();
        inner.counter += 1;
        let dto = TrackDto {
            id: format!("track-{}", inner.counter),
            title: track.title.clone(),
            artist: track.artist.clone(),
            duration_ms: None,
            sha256: track.sha256.clone(),
            stream_url: None,
        };
        inner.tracks.push(dto.clone());
        Ok(dto)
    }

    async fn upload_asset(
        &self,
        track_id: &str,
        path: &Path,
        _content_type: &str,
    ) -> Result<(), MusicApiError> {
        self.inner
            .lock()
            .unwrap()
            .assets
            .push((track_id.to_string(), path.to_string_lossy().into_owned()));
        Ok(())
    }

    async fn get_track(&self, track_id: &str) -> Result<TrackDto, MusicApiError> {
        self.inner
            .lock()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .cloned()
            .ok_or(MusicApiError::NotFound)
    }

    async fn search(
        &self,
        query: &str,
        _limit: i64,
        _offset: i64,
    ) -> Result<Vec<TrackDto>, MusicApiError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .tracks
            .iter()
            .filter(|t| t.title.contains(query))
            .cloned()
            .collect())
    }

    async fn find_by_hash(&self, sha256: &str) -> Result<Option<TrackDto>, MusicApiError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.sha256.as_deref() == Some(sha256))
            .cloned())
    }

    async fn delete(&self, track_id: &str) -> Result<(), MusicApiError> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.tracks.len();
        inner.tracks.retain(|t| t.id != track_id);
        if inner.tracks.len() == before {
            return Err(MusicApiError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_create_and_find_by_hash() {
        let api = FakeMusicApi::new();
        let created = api
            .create_track(&NewTrack {
                title: "Song".into(),
                artist: Some("Artist".into()),
                sha256: Some("abc".into()),
            })
            .await
            .expect("create");
        assert_eq!(created.id, "track-1");
        assert_eq!(api.created_count(), 1);

        let hit = api.find_by_hash("abc").await.unwrap();
        assert_eq!(hit.map(|t| t.id), Some("track-1".to_string()));
        assert!(api.find_by_hash("nope").await.unwrap().is_none());
        assert_eq!(api.sha256_of("track-1").as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn fake_records_asset_uploads() {
        let api = FakeMusicApi::new();
        api.upload_asset("track-1", Path::new("/tmp/a.flac"), "audio/flac")
            .await
            .unwrap();
        api.upload_asset("track-1", Path::new("/tmp/a.mp3"), "audio/mpeg")
            .await
            .unwrap();
        assert_eq!(api.asset_count(), 2);
        assert_eq!(
            api.asset_paths(),
            vec!["/tmp/a.flac".to_string(), "/tmp/a.mp3".to_string()]
        );
    }

    #[tokio::test]
    async fn fake_delete_missing_is_not_found() {
        let api = FakeMusicApi::new();
        assert!(matches!(
            api.delete("ghost").await,
            Err(MusicApiError::NotFound)
        ));
    }
}
