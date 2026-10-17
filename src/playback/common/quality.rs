//! The quality cap a video opens at, carried across videos.
use innertube::Quality;

fn max_height_hook() -> Option<i32> {
    crate::hooks::get("max-height").and_then(|s| s.trim().parse().ok())
}

/// The launch quality cap, highest precedence first: the max-height hook, the saved
/// preference, then `Best`.
fn resolve_initial_quality(hook_height: Option<i32>, saved: Option<Quality>) -> Quality {
    if let Some(h) = hook_height {
        return Quality::Height(h);
    }
    saved.unwrap_or(Quality::Best)
}

/// The quality cap applied at launch and carried across videos: hook override, then saved
/// preference, then `Best`. The cap only limits downward (`Quality::Height` keeps the ABR
/// ladder at or below it), is persisted via [`crate::prefs`], and applies to VOD and live.
pub fn initial_quality() -> Quality {
    resolve_initial_quality(max_height_hook(), crate::prefs::load_quality())
}
