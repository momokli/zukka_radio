//! 30 Hz meter hub (Issue #148, §6.2).
//!
//! One sampler task per [`AppState`](crate::app::AppState) polls
//! `EngineClient::meters` at 30 Hz and fans the snapshot out over a
//! `tokio::sync::broadcast` channel. A slow subscriber never blocks the audio
//! path: broadcast lag drops the missed frames (see the connection loop).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::control_ws::engine::EngineClient;
use crate::control_ws::protocol::{ChannelMeter, parse_meters};

/// Broadcast capacity: enough headroom for a burst without unbounded buffering.
pub const DEFAULT_CAPACITY: usize = 16;

/// Sampling period (30 Hz).
pub const SAMPLE_PERIOD: Duration = Duration::from_millis(33);

/// Fans engine meter snapshots out to every connected cockpit client.
pub struct MeterHub {
    tx: broadcast::Sender<Vec<ChannelMeter>>,
    started: AtomicBool,
    lagged: AtomicU64,
}

impl Default for MeterHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MeterHub {
    /// A hub with the default broadcast capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// A hub with a custom broadcast capacity (small values force lag in tests).
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            started: AtomicBool::new(false),
            lagged: AtomicU64::new(0),
        }
    }

    /// Subscribes to meter snapshots.
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<ChannelMeter>> {
        self.tx.subscribe()
    }

    /// Publishes one snapshot (the sampler uses this; tests drive it directly).
    pub fn publish(&self, meters: Vec<ChannelMeter>) {
        // Ignore send errors: no subscribers is normal.
        let _ = self.tx.send(meters);
    }

    /// Records `n` dropped frames from a lagged subscriber (§6.2 backpressure).
    pub fn note_lag(&self, n: u64) {
        self.lagged.fetch_add(n, Ordering::Relaxed);
    }

    /// Total dropped frames observed across subscribers (test/metric hook).
    pub fn lagged(&self) -> u64 {
        self.lagged.load(Ordering::Relaxed)
    }

    /// Starts the 30 Hz sampler exactly once. Skipped while the engine is
    /// disabled, so a hermetic portal never talks to Liquidsoap.
    pub fn start(self: &Arc<Self>, engine: Arc<dyn EngineClient>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SAMPLE_PERIOD);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if !engine.enabled() {
                    continue;
                }
                match engine.meters().await {
                    Ok(body) => match parse_meters(&body) {
                        Ok(meters) => hub.publish(meters),
                        Err(err) => tracing::debug!("meter parse failed: {err}"),
                    },
                    Err(err) => tracing::debug!("meter read failed: {}", err.message()),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_ws::engine::FakeEngineClient;

    fn meter(id: &str) -> ChannelMeter {
        ChannelMeter {
            id: id.into(),
            rms_l: Some(-1.0),
            rms_r: Some(-1.0),
            peak: Some(-1.0),
        }
    }

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let hub = MeterHub::new();
        let mut rx = hub.subscribe();
        hub.publish(vec![meter("live")]);
        let received = rx.recv().await.unwrap();
        assert_eq!(received[0].id, "live");
    }

    #[tokio::test]
    async fn slow_subscriber_lags_instead_of_blocking() {
        let hub = MeterHub::with_capacity(1);
        let mut rx = hub.subscribe();
        // Fill + overrun the single-slot channel.
        hub.publish(vec![meter("a")]);
        hub.publish(vec![meter("b")]);
        hub.publish(vec![meter("c")]);
        // Draining yields the lag signal; the producer never blocked.
        let err = loop {
            match rx.recv().await {
                Ok(_) => continue,
                Err(err) => break err,
            }
        };
        assert!(matches!(err, broadcast::error::RecvError::Lagged(_)));
    }

    #[tokio::test]
    async fn start_is_idempotent_and_hermetic() {
        let hub = Arc::new(MeterHub::new());
        let engine = Arc::new(FakeEngineClient::disabled());
        hub.start(engine.clone());
        hub.start(engine);
        assert!(hub.started.load(Ordering::SeqCst));
    }
}
