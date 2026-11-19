//! Portal-side visibility guard (Issue #19 §1, defense-in-depth).
//!
//! The scheduler is the authority (Query-Filter + Detail-404, see
//! `scheduler/src/store/tracks.rs`); this mirrors the same rule so a track that
//! slips through is still hidden from rendering:
//!
//! ```text
//! visible(track, viewer) := track.visibility = 'shared' OR track.caster_id = viewer
//! ```

use crate::api_client::Track;
use crate::auth::AuthContext;

/// `true` when `track` may be shown to the authenticated `ctx`.
pub fn visible_to(track: &Track, ctx: &AuthContext) -> bool {
    visible_to_caster(track, Some(ctx.caster_id.as_str()))
}

/// `true` when `track` is visible to `viewer` (`None` = anonymous → shared only).
pub fn visible_to_caster(track: &Track, viewer: Option<&str>) -> bool {
    track.visibility == "shared" || (viewer.is_some() && track.caster_id.as_deref() == viewer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_client::Track;

    fn track(visibility: &str, caster: Option<&str>) -> Track {
        Track {
            id: "t".into(),
            music_api_id: "m".into(),
            title: "Song".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 1,
            storage_key: "k".into(),
            artwork_url: None,
            visibility: visibility.into(),
            caster_id: caster.map(str::to_string),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn shared_is_visible_to_everyone() {
        let t = track("shared", Some("a"));
        assert!(visible_to_caster(&t, None));
        assert!(visible_to_caster(&t, Some("a")));
        assert!(visible_to_caster(&t, Some("b")));
    }

    #[test]
    fn private_is_visible_to_owner_only() {
        let t = track("private", Some("a"));
        assert!(visible_to_caster(&t, Some("a")));
        assert!(!visible_to_caster(&t, Some("b")));
        assert!(!visible_to_caster(&t, None));
    }
}
