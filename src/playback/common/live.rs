//! The live SABR stream a player is playing, and the reads both substrates make of it.
use std::cell::RefCell;

use innertube::{SabrStream, StreamStats};

#[derive(Default)]
pub struct LiveSession {
    stream: RefCell<Option<SabrStream>>,
}

impl LiveSession {
    pub fn set(&self, stream: Option<SabrStream>) {
        *self.stream.borrow_mut() = stream;
    }

    pub fn is_live(&self) -> bool {
        self.stream.borrow().is_some()
    }

    pub fn with<R>(&self, f: impl FnOnce(&SabrStream) -> R) -> Option<R> {
        self.stream.borrow().as_ref().map(f)
    }

    pub fn stats(&self) -> Option<StreamStats> {
        self.with(|s| s.stats())
    }

    /// The live playhead in stream-absolute ms (the SABR `player_time` coordinate).
    pub fn player_time_ms(&self) -> Option<i64> {
        self.stats().map(|s| s.player_time_ms)
    }

    /// The live head in stream-absolute ms, once the server has reported it.
    pub fn head_ms(&self) -> Option<i64> {
        self.stats().and_then(|s| s.live_head_ms)
    }

    pub fn behind_head_s(&self) -> Option<f64> {
        let st = self.stats()?;
        Some((st.live_head_ms? - st.player_time_ms) as f64 / 1000.0)
    }

    /// How far behind the head the server places a joining player, seconds.
    pub fn placement_s(&self) -> f64 {
        let Some(st) = self.stats() else { return 0.0 };
        st.live_head_ms.zip(st.live_max_seekable_ms).map_or(0.0, |(h, m)| (h - m).max(0) as f64 / 1000.0)
    }

    /// Whether playback sits at the live head: no further behind it than the server's own
    /// placement (`head - max_seekable`) plus a slack.
    pub fn at_head(&self) -> Option<bool> {
        const EDGE_SLACK_S: f64 = 5.0;
        Some(self.behind_head_s()? <= self.placement_s() + EDGE_SLACK_S)
    }

    /// Change the live video rung in session. `false` when nothing is live.
    pub fn set_quality(&self, height: i32) -> bool {
        self.with(|s| s.set_quality(height)).is_some()
    }

    /// Live "Auto": release the manual rung pick so the server drives quality again. Picking
    /// the top rung instead would arm a pick and stop the server from adapting.
    pub fn unpin_quality(&self) -> bool {
        self.with(|s| s.unpin_quality()).is_some()
    }

    pub fn set_playback_rate(&self, rate: f32) {
        self.with(|s| s.set_playback_rate(rate));
    }

    pub fn set_playhead_ms(&self, ms: i64) {
        self.with(|s| s.set_playhead_ms(ms));
    }
}

/// Ease an above-1x rate back to 1x once a live playhead is within `settle` of the head.
/// Live content arrives at 1x, so a faster consumer pins the head and stalls.
pub fn settle_edge_speed(player: &dyn super::super::player::Player, requested: f64) {
    const EDGE_SETTLE_BEHIND_S: f64 = 6.0;
    let live = player.live();
    let settle_s = EDGE_SETTLE_BEHIND_S.max(live.placement_s() + 2.0);
    if (requested - 1.0).abs() < 0.05 || !live.is_live() {
        return;
    }
    let Some(behind) = live.behind_head_s() else { return };
    if behind <= settle_s && !player.is_paused() {
        tracing::info!(
            "[live-edge-clamp] behind_head {behind:.1}s <= {settle_s:.1}s with \
speed {requested:.2}x: easing to 1.0x (live cannot sustain >1x at the head)"
        );
        player.set_speed(1.0);
    }
}
