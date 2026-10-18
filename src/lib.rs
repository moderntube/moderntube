pub mod api_keys;
pub mod auth;
pub mod catchup;
pub mod diag;
pub mod emoji;
pub mod frame_stats;
pub mod hooks;
pub mod models;
pub mod mpris;
pub mod playback;
pub mod prefs;
pub mod riva;
pub mod runtime;
pub mod settings;
pub mod sponsorblock;
pub mod theme;
pub mod transcript;
pub mod widgets;
pub mod window;

/// Milliseconds since the process first called this (main calls it at startup): the
/// shared clock for launch/open/seek latency logs on both playback engines.
pub fn uptime_ms() -> u128 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis()
}
