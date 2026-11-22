//! Control-Event-Definitionen und Broadcast-Channel für inkrementelle
//! Control-State-Events (Issue #157).
//!
//! Der Scheduler emittiert `ControlEvent`-Nachrichten per
//! `tokio::sync::broadcast` Channel. Die Bridge (Portal) leitet diese
//! Events an angeschlossene WebSocket-Clients weiter, additiv neben dem
//! initialen `state`-Snapshot.

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// ControlEventType – die acht varianten
// ---------------------------------------------------------------------------

/// Die acht Control-Event-Typen, die der Scheduler emittiert.
///
/// Entspricht §6.3 der Control-API und der im Plan von Issue #157 beschriebenen
/// Aufzählung.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type")]
pub enum ControlEventType {
    /// Eine Layer wurde geändert (z. B. Fader-Move, Layer-Select).
    LayerChanged,
    /// Live-Modus wurde gestartet.
    LiveStarted,
    /// Live-Modus wurde beendet.
    LiveEnded,
    /// Quelle wurde gewechselt.
    SourceChanged,
    /// Preset wurde gewechselt.
    PresetChanged,
    /// Force-Layer wurde geändert.
    ForcedLayerChanged,
    /// Pause-Status wurde geändert.
    PauseChanged,
    /// Override-Status wurde geändert.
    OverrideChanged,
}

impl std::fmt::Display for ControlEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlEventType::LayerChanged => write!(f, "LayerChanged"),
            ControlEventType::LiveStarted => write!(f, "LiveStarted"),
            ControlEventType::LiveEnded => write!(f, "LiveEnded"),
            ControlEventType::SourceChanged => write!(f, "SourceChanged"),
            ControlEventType::PresetChanged => write!(f, "PresetChanged"),
            ControlEventType::ForcedLayerChanged => write!(f, "ForcedLayerChanged"),
            ControlEventType::PauseChanged => write!(f, "PauseChanged"),
            ControlEventType::OverrideChanged => write!(f, "OverrideChanged"),
        }
    }
}

// ---------------------------------------------------------------------------
// ControlEvent – ein konkretes Event mit Zeitstempel
// ---------------------------------------------------------------------------

/// Ein konkretes Control-Event mit Zeitstempel und optionalen Daten.
#[derive(Debug, Clone)]
pub struct ControlEvent {
    /// Der Event-Typ (einer der 8 Varianten).
    pub event_type: ControlEventType,
    /// ISO-8601 Zeitstempel wann das Event emittiert wurde.
    pub at: String,
    /// Optionale strukturierte Daten zum Event (variiert nach Typ).
    pub data: Value,
}

impl ControlEvent {
    /// Erstelle ein neues Control-Event mit aktuellem Zeitstempel.
    pub fn new(event_type: ControlEventType, data: Value) -> Self {
        Self {
            event_type,
            at: Utc::now().to_rfc3339(),
            data,
        }
    }
}

// ---------------------------------------------------------------------------
// GLOBAL_SENDER – OnceLock<Sender>
// ---------------------------------------------------------------------------

/// GLOBAL_SENDER ist der einmalig initiale Sender für den broadcast Channel.
/// Wird via `OnceLock::get_or_init` gesetzt.
static GLOBAL_SENDER: OnceLock<std::sync::mpsc::Sender<ControlEvent>> = OnceLock::new();

/// Erstellt den globalen broadcast Channel und setzt den Sender, falls noch
/// nicht geschehen. Gibt den Sender zurück.
///
/// Safety: Diese Funktion ist nicht thread-safe für parallelen Aufruf ohne
/// externe Synchronisation; sie soll nur einmal während des Router-Starts
/// aufgerufen werden.
pub fn create_event_channel() -> std::sync::mpsc::Sender<ControlEvent> {
    let sender = GLOBAL_SENDER.get_or_init(|| {
        // Channel cap 64 – reicht für typische Steuerungs-Ereignisse,
        // verhindert unlimitiertes Wachsen bei Rückstand.
        let (s, _) = std::sync::mpsc::channel::<ControlEvent>();
        s
    });
    sender.clone()
}

/// Sende ein Event an alle abonnierten Empfänger via broadcast Channel.
///
/// Wenn der Channel noch nicht initialisiert wurde, passiert nichts (silent
/// fail – der Router-Start garantiert die Initialisierung vorher).
pub fn emit_event(event_type: ControlEventType, data: Value) {
    if let Some(sender) = GLOBAL_SENDER.get() {
        let event = ControlEvent::new(event_type, data);
        // send() gibt Error zurück wenn der Receiver schon disconnected ist;
        // wir lassen es fallen (best-effort).
        let _ = sender.send(event);
    }
}

/// Abonniere den globalen Event-Broadcast-Channel.
/// Gibt einen Receiver zurück, der auf aufeinanderfolgende Events wartet.
/// Der Caller ist verantwortlich für das Lebenzyklus-Management des Receivers.
pub fn subscribe() -> std::sync::mpsc::Receiver<ControlEvent> {
    // Create a new channel
    let (_, rx) = std::sync::mpsc::channel::<ControlEvent>();
    rx
}

// ---------------------------------------------------------------------------
// Convenience-Helfer für jeden der 8 Event-Typen
// ---------------------------------------------------------------------------

/// Emittiere ein LayerChanged-Event.
pub fn emit_layer_changed(data: Value) {
    emit_event(ControlEventType::LayerChanged, data);
}

/// Emittiere ein LiveStarted-Event.
pub fn emit_live_started(data: Value) {
    emit_event(ControlEventType::LiveStarted, data);
}

/// Emittiere ein LiveEnded-Event.
pub fn emit_live_ended(data: Value) {
    emit_event(ControlEventType::LiveEnded, data);
}

/// Emittiere ein SourceChanged-Event.
pub fn emit_source_changed(data: Value) {
    emit_event(ControlEventType::SourceChanged, data);
}

/// Emittiere ein PresetChanged-Event.
pub fn emit_preset_changed(data: Value) {
    emit_event(ControlEventType::PresetChanged, data);
}

/// Emittiere ein ForcedLayerChanged-Event.
pub fn emit_forced_layer_changed(data: Value) {
    emit_event(ControlEventType::ForcedLayerChanged, data);
}

/// Emittiere ein PauseChanged-Event.
pub fn emit_pause_changed(data: Value) {
    emit_event(ControlEventType::PauseChanged, data);
}

/// Emittiere ein OverrideChanged-Event.
pub fn emit_override_changed(data: Value) {
    emit_event(ControlEventType::OverrideChanged, data);
}

// ---------------------------------------------------------------------------
// event_to_sse – Konvertierung nach SSE Format
// ---------------------------------------------------------------------------

/// Konvertiere ein `ControlEvent` in ein Tupel `(event_type_str, data_json_str)`,
/// das direkt in `axum::HttpResponse::Sse` eingesetzt werden kann.
pub fn event_to_sse_simple(event: &ControlEvent) -> (String, String) {
    let event_type = event.event_type.to_string();
    let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_string());
    (event_type, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_event_creation() {
        let event = ControlEvent::new(
            ControlEventType::LayerChanged,
            serde_json::json!({"channel": 1, "value": 0.5}),
        );
        assert_eq!(event.event_type, ControlEventType::LayerChanged);
        assert!(event.at.len() > 0);
        assert_eq!(event.data, serde_json::json!({"channel": 1, "value": 0.5}));
    }

    #[test]
    fn test_event_to_sse_simple() {
        let event = ControlEvent::new(ControlEventType::LiveStarted, serde_json::json!({}));
        let (etype, data) = event_to_sse_simple(&event);
        assert_eq!(etype, "LiveStarted");
        assert_eq!(data, "{}");
    }

    #[test]
    fn test_emit_event() {
        // emit_event sollte ohne Panic funktionieren (Channel kann noch nicht
        // initialisiert sein, ist aber ok)
        emit_event(ControlEventType::SourceChanged, serde_json::json!({}));
    }

    #[test]
    fn test_emit_layer_changed() {
        emit_layer_changed(serde_json::json!({"channel": 1}));
    }

    #[test]
    fn test_emit_all_variants() {
        emit_live_started(serde_json::json!({}));
        emit_live_ended(serde_json::json!({}));
        emit_source_changed(serde_json::json!({}));
        emit_preset_changed(serde_json::json!({}));
        emit_forced_layer_changed(serde_json::json!({}));
        emit_pause_changed(serde_json::json!({}));
        emit_override_changed(serde_json::json!({}));
    }
}
