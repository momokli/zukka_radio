//! End-to-end tests for the cockpit WebSocket bridge (Issue #148) against a
//! real `axum::serve` instance and a `tokio_tungstenite` client.
//!
//! Proofs:
//!   (a) auth: bad first-frame token → Close `4401`; first-frame service token
//!       → `hello`+`state`; `Authorization: Bearer` → ok; cookie without
//!       `control.read` → `4401`,
//!   (b) command roundtrip: `mixer.set` reaches the fake engine (`mixer.set
//!       live -6`) and is answered with `{id, ok:true}`,
//!   (c) meters: a 6-channel engine reading becomes a binary frame that decodes
//!       to the §6.2 layout,
//!   (d) backpressure: a slow reader causes broadcast lag (frames dropped) and
//!       never blocks — a later `ping` still gets a `pong`,
//!   (e) errors: `unknown_command`, `bad_gain`, `version_mismatch`,
//!   (f) throttle: a burst >100 commands/s yields `rate_limited`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use zukka_portal::api_client::{
    CasterDto, CasterUpdate, ControlState, CreatedCaster, CreatedSessionDto, FakeControlApi,
    FakeQueueApi, FakeTrackApi, IdentityApi, IdentityError, JobError, NewCaster, NewUploadJob,
    NewWaveform, SessionValidationDto, UploadJob, UploadJobApi, UploadJobPatch, Waveform,
    WaveformApi, WaveformApiError,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::control_ws::engine::FakeEngineClient;
use zukka_portal::control_ws::meter::MeterHub;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const SERVICE_TOKEN: &str = "svc-token";

// ---------------------------------------------------------------------------
// Fake identity
// ---------------------------------------------------------------------------

struct FakeIdentity {
    caster: CasterDto,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
}

impl FakeIdentity {
    fn new(role: &str) -> Self {
        Self {
            caster: CasterDto {
                id: "c-1".into(),
                username: "mira".into(),
                display_name: "Mira".into(),
                email: None,
                role: role.into(),
                is_active: true,
                harbor_mount: None,
                stream_username: None,
                permissions: zukka_portal::rbac::permissions_for_role(role),
            },
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Like [`FakeIdentity::new`] but revokes one permission (§6.10.1).
    fn with_denied(role: &str, denied: &str) -> Self {
        let mut identity = Self::new(role);
        identity
            .caster
            .permissions
            .insert(denied.to_string(), false);
        identity
    }

    fn seed_session(&self, token: &str) -> String {
        let session_id = format!("sess-{token}");
        self.sessions.lock().unwrap().insert(
            token.to_string(),
            SessionValidationDto {
                session_id: session_id.clone(),
                caster: self.caster.clone(),
                csrf_token: "csrf".into(),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        session_id
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, _u: &str, _p: &str) -> Result<CasterDto, IdentityError> {
        Ok(self.caster.clone())
    }
    async fn create_session(
        &self,
        _caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }
    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }
    async fn revoke_session(&self, _session_id: &str) -> Result<(), IdentityError> {
        Ok(())
    }
    async fn get_caster(&self, _caster_id: &str) -> Result<CasterDto, IdentityError> {
        Ok(self.caster.clone())
    }
    async fn change_password(
        &self,
        _c: &str,
        _p: &str,
        _k: Option<&str>,
    ) -> Result<(), IdentityError> {
        Ok(())
    }
    async fn change_stream_password(
        &self,
        _c: &str,
        _p: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
    async fn list_casters(&self) -> Result<Vec<CasterDto>, IdentityError> {
        Ok(Vec::new())
    }
    async fn create_caster(
        &self,
        _actor: Option<&str>,
        _req: &NewCaster,
    ) -> Result<CreatedCaster, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }
    async fn update_caster(
        &self,
        _actor: Option<&str>,
        _id: &str,
        _patch: &CasterUpdate,
    ) -> Result<CasterDto, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }
}

#[derive(Default)]
struct NoJobs;

#[async_trait]
impl UploadJobApi for NoJobs {
    async fn create_job(&self, _job: &NewUploadJob) -> Result<String, JobError> {
        Err(JobError::not_found())
    }
    async fn patch_job(&self, _id: &str, _p: &UploadJobPatch) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn get_job(&self, _id: &str) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn list_jobs(&self, _limit: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(vec![])
    }
}

#[derive(Default)]
struct NoWaveforms;

#[async_trait]
impl WaveformApi for NoWaveforms {
    async fn put_waveform(&self, _w: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        Err(WaveformApiError::Upstream("noop".into()))
    }
    async fn get_waveform(
        &self,
        _k: &str,
        _id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn config() -> Config {
    Config::new(
        "127.0.0.1:8081".into(),
        "https://portal.example.org".into(),
        "http://scheduler:8080".into(),
        SERVICE_TOKEN.into(),
        7,
        30,
    )
    .unwrap()
}

/// Spawns the real router on an ephemeral port; returns the `ws://` URL.
async fn serve(role: &str, engine: Arc<FakeEngineClient>, hub: Arc<MeterHub>) -> String {
    let identity = Arc::new(FakeIdentity::new(role));
    let state = AppState::with_clients(
        config(),
        identity,
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        Arc::new(FakeTrackApi::new()),
        Arc::new(FakeQueueApi::new()),
        Arc::new(FakeControlApi::new()),
    )
    .with_engine(engine)
    .with_meter_hub(hub);
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("ws://{addr}/api/v1/control/ws")
}

/// Serves the router for a prebuilt identity; returns the `ws://` URL.
async fn serve_state(
    identity: Arc<FakeIdentity>,
    engine: Arc<FakeEngineClient>,
    hub: Arc<MeterHub>,
) -> String {
    let state = AppState::with_clients(
        config(),
        identity.clone(),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        Arc::new(FakeTrackApi::new()),
        Arc::new(FakeQueueApi::new()),
        Arc::new(FakeControlApi::new()),
    )
    .with_engine(engine)
    .with_meter_hub(hub);
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("ws://{addr}/api/v1/control/ws")
}

async fn connect(url: &str) -> Ws {
    let (ws, _) = connect_async(url).await.unwrap();
    ws
}

async fn next_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("ws read timeout")
            .expect("ws closed")
            .expect("ws error");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// Reads text frames until one has the given `type` (skips binary/layout).
async fn read_until_type(ws: &mut Ws, kind: &str) -> serde_json::Value {
    loop {
        let value = next_json(ws).await;
        if value["type"] == kind {
            return value;
        }
    }
}

async fn read_close_code(ws: &mut Ws) -> u16 {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("close timeout")
            .expect("stream ended without close");
        match message {
            Ok(Message::Close(Some(frame))) => return u16::from(frame.code),
            Ok(_) => continue,
            Err(_) => panic!("error before close"),
        }
    }
}

async fn send_text(ws: &mut Ws, value: serde_json::Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn write_until_commands(engine: &FakeEngineClient, needle: &str) {
    for _ in 0..50 {
        if engine.commands().iter().any(|c| c == needle) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "engine never received {needle:?}; got {:?}",
        engine.commands()
    );
}

fn six_channel_meters() -> &'static str {
    r#"[
      {"id":"live","rms_l":-18.2,"rms_r":-18.0,"peak":-3.1,"limiter_active":false},
      {"id":"show","rms_l":null,"rms_r":null,"peak":null,"limiter_active":false},
      {"id":"queue","rms_l":-20.0,"rms_r":-20.0,"peak":-4.0,"limiter_active":false},
      {"id":"rotation","rms_l":-30.0,"rms_r":-30.0,"peak":-10.0,"limiter_active":false},
      {"id":"relay","rms_l":-40.0,"rms_r":-40.0,"peak":-20.0,"limiter_active":false},
      {"id":"fun","rms_l":-50.0,"rms_r":-50.0,"peak":-30.0,"limiter_active":false}
    ]"#
}

// ---------------------------------------------------------------------------
// (a) auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_first_frame_bad_token_closes_4401() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": "wrong"}),
    )
    .await;
    assert_eq!(read_close_code(&mut ws).await, 4401);
}

#[tokio::test]
async fn auth_first_frame_service_token_gets_hello_then_state() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;

    let hello = read_until_type(&mut ws, "hello").await;
    assert_eq!(hello["data"]["api_version"], 1);
    let state = read_until_type(&mut ws, "state").await;
    let channels = state["data"]["channels"].as_array().unwrap();
    assert_eq!(channels[0]["id"], "live");
    assert_eq!(state["data"]["schema_version"], 1);
}

#[tokio::test]
async fn auth_bearer_header_is_accepted() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {SERVICE_TOKEN}").parse().unwrap(),
    );
    let (mut ws, _) = connect_async(request).await.unwrap();
    let hello = read_until_type(&mut ws, "hello").await;
    assert_eq!(hello["type"], "hello");
}

#[tokio::test]
async fn auth_cookie_without_control_read_closes_4401() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    // A session whose role lacks `control.read` (§6.10.1) must be rejected.
    let identity = Arc::new(FakeIdentity::with_denied("caster", "control.read"));
    let token = "caster-token";
    identity.seed_session(token);
    let url = serve_state(identity.clone(), engine, hub).await;

    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "Cookie",
        format!("__Host-zukka_session={token}").parse().unwrap(),
    );
    let (mut ws, _) = connect_async(request).await.unwrap();
    assert_eq!(read_close_code(&mut ws).await, 4401);
}

// ---------------------------------------------------------------------------
// (b) command roundtrip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn command_roundtrip_reaches_engine() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine.clone(), hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    send_text(
        &mut ws,
        serde_json::json!({
            "id": "c-1", "api_version": 1, "cmd": "mixer.set",
            "args": {"channel": "live", "gain_db": -6.0}
        }),
    )
    .await;

    let reply = next_json(&mut ws).await;
    assert_eq!(reply["id"], "c-1");
    assert_eq!(reply["ok"], true);
    // The fader is coalesced and applied at 30 Hz by the tick.
    write_until_commands(&engine, "mixer.set live -6").await;
}

// ---------------------------------------------------------------------------
// (c) meters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binary_meter_frame_decodes() {
    let engine = Arc::new(FakeEngineClient::new());
    engine.set_meters(six_channel_meters());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    // Skip until the first binary frame arrives.
    let frame = loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("meter timeout")
            .expect("closed")
            .unwrap();
        if let Message::Binary(bytes) = message {
            break bytes;
        }
    };
    assert_eq!(frame.len(), 4 + 6 * 12);
    assert_eq!(frame[0], 1);
    assert_eq!(frame[1], 6);
    assert_eq!(&frame[2..4], &[0, 0]);
    // First channel (`live`) rms_l = -18.2.
    let rms_l = f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
    assert!((rms_l - (-18.2)).abs() < 1e-3);
    // Second channel (`show`) has no signal → f32::MIN.
    let show_rms_l = f32::from_le_bytes([frame[16], frame[17], frame[18], frame[19]]);
    assert_eq!(show_rms_l, f32::MIN);
}

// ---------------------------------------------------------------------------
// (d) backpressure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_reader_lags_but_is_never_blocked() {
    let engine = Arc::new(FakeEngineClient::new());
    engine.set_meters(six_channel_meters());
    // Capacity 1 forces the reader to lag.
    let hub = Arc::new(MeterHub::with_capacity(1));
    let url = serve("operator", engine, hub.clone()).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    // Stop reading and overrun the capacity-1 broadcast synchronously: the
    // single-threaded test runtime keeps the bridge's receiver from draining
    // while we publish, so the bridge must drop frames, never block.
    let snapshot = zukka_portal::control_ws::protocol::parse_meters(six_channel_meters())
        .expect("parse meters");
    for _ in 0..200 {
        hub.publish(snapshot.clone());
    }
    for _ in 0..100 {
        if hub.lagged() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        hub.lagged() > 0,
        "expected the hub to record dropped meter frames"
    );

    // The connection is still live: a `ping` is answered with `pong`.
    send_text(&mut ws, serde_json::json!({"id": "p", "cmd": "ping"})).await;
    loop {
        let value = next_json(&mut ws).await;
        if value["id"] == "p" {
            assert_eq!(value["ok"], true);
            assert_eq!(value["result"]["pong"], true);
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// (e) errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn command_errors_are_reported() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    // unknown_command
    send_text(
        &mut ws,
        serde_json::json!({"id": "e-1", "cmd": "does.not.exist"}),
    )
    .await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "unknown_command");

    // bad_gain
    send_text(
        &mut ws,
        serde_json::json!({
            "id": "e-2", "cmd": "mixer.set",
            "args": {"channel": "live", "gain_db": 99.0}
        }),
    )
    .await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], "bad_gain");

    // version_mismatch
    send_text(
        &mut ws,
        serde_json::json!({"id": "e-3", "api_version": 2, "cmd": "ping"}),
    )
    .await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], "version_mismatch");
}

// ---------------------------------------------------------------------------
// (f) throttle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn command_burst_yields_rate_limited() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    // 150 pings in a burst: the tail must be rate-limited.
    for i in 0..150 {
        send_text(
            &mut ws,
            serde_json::json!({"id": format!("b-{i}"), "cmd": "ping"}),
        )
        .await;
    }
    let mut saw_rate_limited = false;
    for _ in 0..150 {
        let value = next_json(&mut ws).await;
        if value["ok"] == false && value["error"]["code"] == "rate_limited" {
            saw_rate_limited = true;
            break;
        }
    }
    assert!(
        saw_rate_limited,
        "expected a rate_limited reply in the burst"
    );
}

// ---------------------------------------------------------------------------
// state.get round-trips the CockpitState DTO
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_get_returns_cockpit_state() {
    let engine = Arc::new(FakeEngineClient::new());
    let hub = Arc::new(MeterHub::new());
    let url = serve("operator", engine, hub).await;
    let mut ws = connect(&url).await;
    send_text(
        &mut ws,
        serde_json::json!({"type": "auth", "token": SERVICE_TOKEN}),
    )
    .await;
    read_until_type(&mut ws, "state").await;

    send_text(
        &mut ws,
        serde_json::json!({"id": "s-1", "cmd": "state.get"}),
    )
    .await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["id"], "s-1");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"]["schema_version"], 1);
    assert!(reply["result"]["channels"].is_array());
}

#[allow(dead_code)]
fn _assert_control_state_used(_: ControlState) {}
