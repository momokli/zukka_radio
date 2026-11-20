//! Pure dedupe-key and payload builders for announcement events (Issue #35).
//!
//! An announcement is created from one of seven triggers
//! ([`AnnouncementEvent`]). Automatic emitters must be **idempotent**, so each
//! event that maps to a stable resource derives a `dedupe_key`
//! (`announcements.dedupe_key` is `UNIQUE`, nullable). Events without a stable
//! key return `None` (several `NULL` rows are allowed).
//!
//! The payload builders are pure: they never touch the database. They only
//! select the documented keys from a [`PayloadContext`] so the render engine
//! (#34) can substitute them later (during #37 delivery).

use serde_json::{Value, json};

use crate::dto::AnnouncementEvent;

/// Inputs available when deriving a stable `dedupe_key`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DedupeContext<'a> {
    pub broadcast_id: Option<&'a str>,
    pub tape_id: Option<&'a str>,
    /// Monotonic instant/instance key for event-like triggers
    /// (`fallback_entered`, `error`).
    pub epoch: Option<&'a str>,
    /// Error scope (e.g. `live`, `tape`, `scheduler`).
    pub scope: Option<&'a str>,
}

/// Derives the `dedupe_key` per the §2 schema; `None` = not deduplicable.
pub fn dedupe_key(event: AnnouncementEvent, ctx: &DedupeContext<'_>) -> Option<String> {
    match event {
        AnnouncementEvent::LiveOn => ctx.broadcast_id.map(|id| format!("live_on:{id}")),
        AnnouncementEvent::LiveOff => ctx.broadcast_id.map(|id| format!("live_off:{id}")),
        AnnouncementEvent::ShowReminder => ctx.broadcast_id.map(|id| format!("show_reminder:{id}")),
        // Architecture §3.6: `dedupe_key = tape:<id>`.
        AnnouncementEvent::RecordingPublished => ctx.tape_id.map(|id| format!("tape:{id}")),
        AnnouncementEvent::FallbackEntered => ctx.epoch.map(|epoch| format!("fallback:{epoch}")),
        AnnouncementEvent::Error => match (ctx.scope, ctx.epoch) {
            (Some(scope), Some(epoch)) => Some(format!("error:{scope}:{epoch}")),
            _ => None,
        },
        // Manual posts are deliberately repeatable.
        AnnouncementEvent::Manual => None,
    }
}

/// Fields the payload builders may project into an announcement payload.
#[derive(Debug, Clone, Default)]
pub struct PayloadContext {
    pub broadcast_id: Option<String>,
    pub show_id: Option<String>,
    pub show_name: Option<String>,
    pub caster_id: Option<String>,
    pub caster_name: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub tape_id: Option<String>,
    pub tape_title: Option<String>,
    pub scope: Option<String>,
    pub epoch: Option<String>,
    pub message: Option<String>,
}

/// Keys selected per event (documented placeholders in `rendered` §7).
fn keys_for(event: AnnouncementEvent) -> &'static [&'static str] {
    match event {
        AnnouncementEvent::LiveOn => &[
            "broadcast_id",
            "show_id",
            "show_name",
            "caster_id",
            "caster_name",
            "start_time",
        ],
        AnnouncementEvent::LiveOff => &[
            "broadcast_id",
            "show_id",
            "show_name",
            "caster_id",
            "caster_name",
            "start_time",
            "end_time",
        ],
        AnnouncementEvent::ShowReminder => &["broadcast_id", "show_id", "show_name", "start_time"],
        AnnouncementEvent::RecordingPublished => &["tape_id", "tape_title", "show_id", "caster_id"],
        AnnouncementEvent::FallbackEntered | AnnouncementEvent::Error => {
            &["scope", "epoch", "message"]
        }
        AnnouncementEvent::Manual => &["message"],
    }
}

fn field<'a>(ctx: &'a PayloadContext, key: &str) -> Option<&'a str> {
    match key {
        "broadcast_id" => ctx.broadcast_id.as_deref(),
        "show_id" => ctx.show_id.as_deref(),
        "show_name" => ctx.show_name.as_deref(),
        "caster_id" => ctx.caster_id.as_deref(),
        "caster_name" => ctx.caster_name.as_deref(),
        "start_time" => ctx.start_time.as_deref(),
        "end_time" => ctx.end_time.as_deref(),
        "tape_id" => ctx.tape_id.as_deref(),
        "tape_title" => ctx.tape_title.as_deref(),
        "scope" => ctx.scope.as_deref(),
        "epoch" => ctx.epoch.as_deref(),
        "message" => ctx.message.as_deref(),
        _ => None,
    }
}

/// Builds the announcement payload object, including only present keys.
pub fn build_payload(event: AnnouncementEvent, ctx: &PayloadContext) -> Value {
    let mut map = serde_json::Map::new();
    for key in keys_for(event) {
        if let Some(value) = field(ctx, key) {
            map.insert((*key).to_string(), json!(value));
        }
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DedupeContext<'static> {
        DedupeContext {
            broadcast_id: Some("b1"),
            tape_id: Some("t1"),
            epoch: Some("42"),
            scope: Some("live"),
        }
    }

    #[test]
    fn dedupe_keys_per_event() {
        let c = ctx();
        assert_eq!(
            dedupe_key(AnnouncementEvent::LiveOn, &c).as_deref(),
            Some("live_on:b1")
        );
        assert_eq!(
            dedupe_key(AnnouncementEvent::LiveOff, &c).as_deref(),
            Some("live_off:b1")
        );
        assert_eq!(
            dedupe_key(AnnouncementEvent::ShowReminder, &c).as_deref(),
            Some("show_reminder:b1")
        );
        assert_eq!(
            dedupe_key(AnnouncementEvent::RecordingPublished, &c).as_deref(),
            Some("tape:t1")
        );
        assert_eq!(
            dedupe_key(AnnouncementEvent::FallbackEntered, &c).as_deref(),
            Some("fallback:42")
        );
        assert_eq!(
            dedupe_key(AnnouncementEvent::Error, &c).as_deref(),
            Some("error:live:42")
        );
        assert_eq!(dedupe_key(AnnouncementEvent::Manual, &c), None);
    }

    #[test]
    fn missing_context_yields_none() {
        let empty = DedupeContext::default();
        assert_eq!(dedupe_key(AnnouncementEvent::LiveOn, &empty), None);
        assert_eq!(
            dedupe_key(AnnouncementEvent::RecordingPublished, &empty),
            None
        );
        assert_eq!(dedupe_key(AnnouncementEvent::Error, &empty), None);
    }

    #[test]
    fn error_requires_both_scope_and_epoch() {
        let scope_only = DedupeContext {
            scope: Some("live"),
            ..Default::default()
        };
        assert_eq!(dedupe_key(AnnouncementEvent::Error, &scope_only), None);
    }

    #[test]
    fn live_on_payload_keys() {
        let p = PayloadContext {
            broadcast_id: Some("b1".into()),
            show_name: Some("Morgen".into()),
            caster_name: Some("Momo".into()),
            start_time: Some("2026-11-18T20:00:00+01:00".into()),
            ..Default::default()
        };
        let value = build_payload(AnnouncementEvent::LiveOn, &p);
        assert_eq!(value["broadcast_id"], "b1");
        assert_eq!(value["show_name"], "Morgen");
        assert_eq!(value["caster_name"], "Momo");
        assert_eq!(value["start_time"], "2026-11-18T20:00:00+01:00");
        // Absent keys are omitted, not null.
        assert!(value.get("end_time").is_none());
    }

    #[test]
    fn manual_payload_only_message() {
        let p = PayloadContext {
            message: Some("hi".into()),
            show_name: Some("ignored".into()),
            ..Default::default()
        };
        let value = build_payload(AnnouncementEvent::Manual, &p);
        assert_eq!(value["message"], "hi");
        assert!(value.get("show_name").is_none());
    }

    #[test]
    fn payload_is_object_even_when_empty() {
        let value = build_payload(AnnouncementEvent::LiveOn, &PayloadContext::default());
        assert!(value.as_object().unwrap().is_empty());
    }
}
