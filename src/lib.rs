pub mod api_keys;
pub mod auth;
pub mod catchup;
pub mod diag;
pub mod emoji;
pub mod frame_stats;
pub mod hooks;
pub mod models;
pub mod mpris;
pub mod notes;
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

/// `$XDG_STATE_HOME/moderntube/moderntube.log`, or under `~/.local/state`.
pub fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("moderntube").join("moderntube.log"))
}

/// Milliseconds since the process first called this (main calls it at startup): the
/// shared clock for launch/open/seek latency logs on both playback engines.
pub fn uptime_ms() -> u128 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis()
}
