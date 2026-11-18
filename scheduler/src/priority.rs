//! Pure priority-layer selection (§2.4 / liquidsoap §D6).
//!
//! This module has **no** database dependency: the active layer is an
//! invariant of the priority chain and must be unit-testable in isolation.
//! Runtime availability (live connected, show/queue content, relay up) is
//! supplied by the caller via a closure; `sources.config` never affects the
//! selection.

use crate::dto::{SourceDto, SourceLayer};

/// Highest-priority `enabled` + available layer, `None` when none matches.
///
/// Tie-break on equal `priority`: the lexicographically **smaller** `name`
/// wins, making the choice deterministic and reproducible.
pub fn select_active(
    sources: &[SourceDto],
    is_available: impl Fn(SourceLayer) -> bool,
) -> Option<&SourceDto> {
    sources
        .iter()
        .filter(|source| source.enabled)
        .filter(|source| is_available(source.name))
        .max_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.name.as_str().cmp(a.name.as_str()))
        })
}

/// Convenience wrapper for callers that only need the layer name.
pub fn active_layer(
    sources: &[SourceDto],
    is_available: impl Fn(SourceLayer) -> bool,
) -> Option<SourceLayer> {
    select_active(sources, is_available).map(|source| source.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: SourceLayer, priority: i64, enabled: bool) -> SourceDto {
        SourceDto {
            id: format!("id-{}", name.as_str()),
            name,
            kind: name,
            priority,
            enabled,
            config: serde_json::json!({}),
            created_at: "2026-11-18T00:00:00.000Z".to_string(),
            updated_at: "2026-11-18T00:00:00.000Z".to_string(),
        }
    }

    /// The seeded chain: live 100 / show 80 / queue 60 / rotation 40 /
    /// relay 20 / fun 1, all enabled.
    fn seed() -> Vec<SourceDto> {
        vec![
            source(SourceLayer::Live, 100, true),
            source(SourceLayer::Show, 80, true),
            source(SourceLayer::Queue, 60, true),
            source(SourceLayer::Rotation, 40, true),
            source(SourceLayer::Relay, 20, true),
            source(SourceLayer::Fun, 1, true),
        ]
    }

    #[test]
    fn highest_available_wins() {
        let sources = seed();
        let picked = select_active(&sources, |_| true).expect("a layer");
        assert_eq!(picked.name, SourceLayer::Live);
    }

    #[test]
    fn unavailable_top_falls_through() {
        let sources = seed();
        let layer = active_layer(&sources, |layer| layer != SourceLayer::Live);
        assert_eq!(layer, Some(SourceLayer::Show));
    }

    #[test]
    fn disabled_top_is_skipped() {
        let mut sources = seed();
        sources[0].enabled = false; // live disabled
        let layer = active_layer(&sources, |_| true);
        assert_eq!(layer, Some(SourceLayer::Show));
    }

    #[test]
    fn all_disabled_yields_none() {
        let mut sources = seed();
        for source in &mut sources {
            source.enabled = false;
        }
        assert_eq!(active_layer(&sources, |_| true), None);
    }

    #[test]
    fn all_enabled_but_unavailable_yields_none() {
        let sources = seed();
        assert_eq!(active_layer(&sources, |_| false), None);
    }

    #[test]
    fn empty_slice_yields_none() {
        let sources: Vec<SourceDto> = Vec::new();
        assert!(select_active(&sources, |_| true).is_none());
    }

    #[test]
    fn rotation_and_fun_remain_as_last_resort() {
        let sources = seed();
        // Only rotation and fun are available.
        let layer = active_layer(&sources, |layer| {
            matches!(layer, SourceLayer::Rotation | SourceLayer::Fun)
        });
        assert_eq!(layer, Some(SourceLayer::Rotation));
    }

    #[test]
    fn tie_break_prefers_name_ascending() {
        let sources = vec![
            source(SourceLayer::Relay, 50, true),
            source(SourceLayer::Show, 50, true),
        ];
        let layer = active_layer(&sources, |_| true);
        // 'relay' < 'show' lexicographically.
        assert_eq!(layer, Some(SourceLayer::Relay));
    }

    #[test]
    fn fun_disabled_with_everything_else_gone_yields_none() {
        let mut sources = seed();
        sources[5].enabled = false; // fun disabled
        let layer = active_layer(&sources, |layer| layer == SourceLayer::Fun);
        assert_eq!(layer, None);
    }

    #[test]
    fn toggling_enabled_changes_selection() {
        let mut sources = seed();
        assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Live));
        sources[0].enabled = false; // disable live
        assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Show));
    }
}
