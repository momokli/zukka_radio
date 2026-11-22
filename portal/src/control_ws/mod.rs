//! Control WebSocket bridge (Issue #148) — `GET /api/v1/control/ws`.
//!
//! The cockpit holds one WebSocket to this bridge instead of polling. The
//! bridge authenticates in-band (portal session cookie **or** service token,
//! §2.2), pushes `hello`+`state`, samples meters at 30 Hz (§6.2), dispatches
//! commands to the Liquidsoap command server (§6.1), throttles faders (§6.5)
//! and keeps the connection alive with protocol pings (§2.2).
//!
//! The route lives **outside** the `protected` layer: a WS upgrade must not be
//! answered with a `303 /login`; failures close the socket with `4401`.

pub mod engine;
pub mod meter;
pub mod protocol;
pub mod throttle;

use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast::error::RecvError;

use crate::app::AppState;
use crate::control_ws::protocol::{
    Action, AuthFrame, CommandFrame, WsError, WsErrorCode, canonical_order, encode_meter_frame,
    meters_json, resolve,
};
use crate::control_ws::throttle::{FaderCoalescer, RateLimiter};

/// Close code for missing/invalid auth (§2.2).
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
/// Close code for a missed protocol pong (§2.2).
pub const CLOSE_PING_TIMEOUT: u16 = 4408;

/// Server-reported name in the `hello` event.
const SERVER_NAME: &str = "zukka-portal";
/// Protocol ping period (§2.2).
const PING_PERIOD: Duration = Duration::from_secs(15);
/// Pong deadline after a protocol ping (§2.2).
const PONG_TIMEOUT: Duration = Duration::from_secs(30);
/// Meter sampling/apply cadence (§6.2/§6.5).
const TICK_PERIOD: Duration = Duration::from_millis(33);
/// How long to wait for the first-frame auth before closing.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// `GET /api/v1/control/ws` — upgrade handler (auth happens in-band).
pub async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let bearer = parse_bearer(&headers);
    let cookie = crate::auth::session::parse_cookie(
        headers.get(header::COOKIE).and_then(|v| v.to_str().ok()),
        crate::auth::session::COOKIE_NAME,
    );
    ws.on_upgrade(move |socket| handle_socket(socket, state, PreAuth { bearer, cookie }))
}

/// Auth material lifted from the upgrade request.
#[derive(Debug, Clone, Default)]
struct PreAuth {
    bearer: Option<String>,
    cookie: Option<String>,
}

/// Resolved authenticated principal.
#[derive(Debug, Clone)]
struct Session {
    /// `X-Caster-Id` viewer (`None` for a service token = full access).
    viewer: Option<String>,
    /// Session id echoed in `hello` (`"service"` for a service token).
    id: String,
}

fn parse_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn close_frame(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: Utf8Bytes::from(reason.to_string()),
    }
}

async fn handle_socket(mut socket: WebSocket, state: AppState, pre: PreAuth) {
    let session = match authenticate(&mut socket, &state, pre).await {
        Ok(session) => session,
        Err(close) => {
            let _ = socket.send(Message::Close(Some(close))).await;
            return;
        }
    };

    // `hello` is always the first event after successful auth (§6.3).
    let hello = event_json(
        "hello",
        serde_json::json!({
            "api_version": protocol::API_VERSION,
            "server": SERVER_NAME,
            "session_id": session.id,
        }),
    );
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    // Directly followed by the full `state` snapshot (§6.3).
    match state.control.cockpit_state(session.viewer.as_deref()).await {
        Ok(cockpit) => {
            let data = serde_json::to_value(&cockpit).unwrap_or(serde_json::Value::Null);
            if socket
                .send(Message::Text(event_json("state", data).into()))
                .await
                .is_err()
            {
                return;
            }
        }
        Err(err) => {
            let data = serde_json::json!({
                "code": "engine_unavailable",
                "message": err.to_string(),
            });
            if socket
                .send(Message::Text(event_json("error", data).into()))
                .await
                .is_err()
            {
                return;
            }
        }
    }

    run_connection(socket, state, session).await;
}

/// Resolves the principal in contract order: bearer → cookie → first frame.
async fn authenticate(
    socket: &mut WebSocket,
    state: &AppState,
    pre: PreAuth,
) -> Result<Session, CloseFrame> {
    let service_token = &state.config.service_token;

    if let Some(token) = &pre.bearer {
        if constant_time_eq(token, service_token) {
            return Ok(Session {
                viewer: None,
                id: "service".to_string(),
            });
        }
    }

    if let Some(token) = &pre.cookie {
        match state.identity.validate_session(token).await {
            Ok(Some(validation)) => {
                if validation
                    .caster
                    .permissions
                    .get("control.read")
                    .copied()
                    .unwrap_or(false)
                {
                    return Ok(Session {
                        viewer: Some(validation.caster.id),
                        id: validation.session_id,
                    });
                }
                return Err(close_frame(CLOSE_UNAUTHORIZED, "forbidden"));
            }
            Ok(None) => {}
            Err(err) => tracing::debug!("control ws session validation failed: {err}"),
        }
    }

    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            if let Ok(frame) = serde_json::from_str::<AuthFrame>(text.as_str()) {
                if frame.kind == "auth" {
                    if let Some(token) = &frame.token {
                        if constant_time_eq(token, service_token) {
                            return Ok(Session {
                                viewer: None,
                                id: "service".to_string(),
                            });
                        }
                    }
                }
            }
            Err(close_frame(CLOSE_UNAUTHORIZED, "unauthorized"))
        }
        _ => Err(close_frame(CLOSE_UNAUTHORIZED, "unauthorized")),
    }
}

async fn run_connection(socket: WebSocket, state: AppState, session: Session) {
    let (mut sender, mut receiver) = socket.split();
    let mut meter_rx = state.meter_hub.subscribe();

    let mut binary = true;
    let mut layout: Vec<String> = Vec::new();
    let mut limiter = RateLimiter::new(Instant::now());
    let mut coalescer = FaderCoalescer::new(Instant::now());
    let mut tick = tokio::time::interval(TICK_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_tick = tokio::time::interval(PING_PERIOD);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut outstanding_ping: Option<Instant> = None;

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let now = Instant::now();
                        if !handle_text(
                            &mut sender,
                            &state,
                            &session,
                            text.as_str(),
                            &mut limiter,
                            &mut coalescer,
                            &mut binary,
                            now,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sender.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        outstanding_ping = None;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            snapshot = meter_rx.recv() => {
                match snapshot {
                    Ok(meters) => {
                        let order = canonical_order(meters.iter().map(|m| m.id.clone()));
                        if order != layout {
                            let data = serde_json::json!({ "channel_ids": order });
                            if sender
                                .send(Message::Text(event_json("meters.layout", data).into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            layout = order;
                        }
                        if binary {
                            let frame = encode_meter_frame(&layout, &meters);
                            if sender.send(Message::Binary(Bytes::from(frame))).await.is_err() {
                                break;
                            }
                        } else {
                            let data = meters_json(&layout, &meters);
                            if sender
                                .send(Message::Text(event_json("meters", data).into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Backpressure: the slow consumer drops frames, the
                        // audio path never blocks (§6.2).
                        state.meter_hub.note_lag(n);
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                for command in coalescer.take(Instant::now()) {
                    let _ = state.engine.command(&command).await;
                }
            }
            _ = ping_tick.tick() => {
                let now = Instant::now();
                if let Some(sent) = outstanding_ping {
                    if now.duration_since(sent) > PONG_TIMEOUT {
                        let _ = sender
                            .send(Message::Close(Some(close_frame(
                                CLOSE_PING_TIMEOUT,
                                "ping timeout",
                            ))))
                            .await;
                        break;
                    }
                }
                outstanding_ping = Some(now);
                if sender
                    .send(Message::Ping(Bytes::from_static(b"z")))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Handles one text frame; returns `false` when the connection should close.
#[allow(clippy::too_many_arguments)]
async fn handle_text<S>(
    sender: &mut S,
    state: &AppState,
    session: &Session,
    text: &str,
    limiter: &mut RateLimiter,
    coalescer: &mut FaderCoalescer,
    binary: &mut bool,
    now: Instant,
) -> bool
where
    S: SinkExt<Message> + Unpin,
{
    let frame: CommandFrame = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(_) => {
            let err = WsError::new(WsErrorCode::InvalidArgs, "malformed command frame");
            return send(sender, reply_err(None, &err)).await;
        }
    };
    let id = frame.id.clone();

    if !limiter.allow(now) {
        let err = WsError::new(WsErrorCode::RateLimited, "too many commands");
        return send(sender, reply_err(id.as_deref(), &err)).await;
    }

    match resolve(&frame) {
        Ok(Action::Ping) => {
            let result = serde_json::json!({ "pong": true, "t": now_iso() });
            send(sender, reply_ok(id.as_deref(), result)).await
        }
        Ok(Action::State) => match state.control.cockpit_state(session.viewer.as_deref()).await {
            Ok(cockpit) => {
                let result = serde_json::to_value(&cockpit).unwrap_or(serde_json::Value::Null);
                send(sender, reply_ok(id.as_deref(), result)).await
            }
            Err(err) => {
                let err = WsError::new(WsErrorCode::EngineUnavailable, err.to_string());
                send(sender, reply_err(id.as_deref(), &err)).await
            }
        },
        Ok(Action::MetersSubscribe { binary: enabled }) => {
            *binary = enabled;
            let result = serde_json::json!({ "binary": enabled });
            send(sender, reply_ok(id.as_deref(), result)).await
        }
        Ok(Action::Engine(command)) => {
            if let Some(key) = fader_key(&command) {
                coalescer.submit(key, command);
                let result = serde_json::json!({ "applied": true });
                send(sender, reply_ok(id.as_deref(), result)).await
            } else {
                match state.engine.command(&command).await {
                    Ok(_) => {
                        let result = serde_json::json!({ "applied": true });
                        send(sender, reply_ok(id.as_deref(), result)).await
                    }
                    Err(err) => {
                        let error = WsError::new(err.code(), err.message());
                        send(sender, reply_err(id.as_deref(), &error)).await
                    }
                }
            }
        }
        Err(err) => send(sender, reply_err(id.as_deref(), &err)).await,
    }
}

/// The coalescing key for fader commands (§6.5); `None` for non-faders.
fn fader_key(command: &str) -> Option<String> {
    let mut parts = command.split_whitespace();
    match parts.next()? {
        "mixer.set" | "mixer.trim" => {
            let channel = parts.next().unwrap_or("");
            let kind = command_kind(command);
            Some(format!("{kind} {channel}"))
        }
        "mixer.master" => Some("mixer.master".to_string()),
        _ => None,
    }
}

fn command_kind(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

async fn send<S>(sender: &mut S, message: Message) -> bool
where
    S: SinkExt<Message> + Unpin,
{
    sender.send(message).await.is_ok()
}

fn id_value(id: Option<&str>) -> serde_json::Value {
    match id {
        Some(id) => serde_json::Value::String(id.to_string()),
        None => serde_json::Value::Null,
    }
}

/// Builds a success reply (`{id, ok:true, result}`).
fn reply_ok(id: Option<&str>, result: serde_json::Value) -> Message {
    let body = serde_json::json!({ "id": id_value(id), "ok": true, "result": result });
    Message::Text(body.to_string().into())
}

/// Builds an error reply (`{id, ok:false, error}`).
fn reply_err(id: Option<&str>, err: &WsError) -> Message {
    let body = serde_json::json!({ "id": id_value(id), "ok": false, "error": err.to_json() });
    Message::Text(body.to_string().into())
}

/// Builds a JSON event (`{type, at, data}`).
fn event_json(kind: &str, data: serde_json::Value) -> String {
    serde_json::json!({ "type": kind, "at": now_iso(), "data": data }).to_string()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_key_classifies_commands() {
        assert_eq!(
            fader_key("mixer.set live -6").as_deref(),
            Some("mixer.set live")
        );
        assert_eq!(
            fader_key("mixer.trim show -3").as_deref(),
            Some("mixer.trim show")
        );
        assert_eq!(
            fader_key("mixer.master -1").as_deref(),
            Some("mixer.master")
        );
        assert_eq!(fader_key("mixer.mute live true"), None);
        assert_eq!(fader_key("live.set /live/slot-01"), None);
    }

    #[test]
    fn bearer_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(parse_bearer(&headers).as_deref(), Some("abc"));
    }

    #[test]
    fn replies_carry_the_id() {
        let ok = reply_ok(Some("c-1"), serde_json::json!({"applied": true}));
        let Message::Text(text) = ok else {
            panic!("text")
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(value["id"], "c-1");
        assert_eq!(value["ok"], true);

        let err = reply_err(None, &WsError::new(WsErrorCode::BadGain, "nope"));
        let Message::Text(text) = err else {
            panic!("text")
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(value["id"], serde_json::Value::Null);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "bad_gain");
    }

    #[test]
    fn events_have_type_and_at() {
        let event = event_json("hello", serde_json::json!({"api_version": 1}));
        let value: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(value["type"], "hello");
        assert!(value["at"].is_string());
    }
}
