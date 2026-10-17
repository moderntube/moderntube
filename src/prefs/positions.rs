//! Per-video resume positions, persisted most-recent first.
use std::path::Path;

use super::config_dir;

pub(super) const WATCH_POSITIONS_FILE: &str = "watch_positions";

/// Cap on stored per-video resume positions; bounds the file to the most recent N.
pub const WATCH_POSITIONS_MAX: usize = 200;

// A "video_id\tseconds" text file, one entry per line, most recent first, capped to
// `WATCH_POSITIONS_MAX`.

/// Move `id`'s resume position to the front of `list` (most-recent) with `secs`, then cap
/// the list to `max` newest entries. Pure so recency/bounding is unit-testable.
pub fn upsert_position(list: &mut Vec<(String, f64)>, id: &str, secs: f64, max: usize) {
    list.retain(|(v, _)| v != id);
    list.insert(0, (id.to_string(), secs));
    list.truncate(max);
}

/// Drop `id`'s stored resume position (e.g. it finished). Pure/testable.
pub fn remove_position(list: &mut Vec<(String, f64)>, id: &str) {
    list.retain(|(v, _)| v != id);
}

pub(super) fn load_watch_positions_in(dir: &Path) -> Vec<(String, f64)> {
    let Ok(s) = std::fs::read_to_string(dir.join(WATCH_POSITIONS_FILE)) else {
        return Vec::new();
    };
    s.lines()
        .filter_map(|line| {
            let (id, secs) = line.split_once('\t')?;
            let secs = secs.trim().parse::<f64>().ok().filter(|v| *v > 0.0 && v.is_finite())?;
            let id = id.trim();
            (!id.is_empty()).then(|| (id.to_string(), secs))
        })
        .take(WATCH_POSITIONS_MAX)
        .collect()
}

pub(super) fn save_watch_positions_in(dir: &Path, entries: &[(String, f64)]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body: String = entries
        .iter()
        .take(WATCH_POSITIONS_MAX)
        .map(|(id, secs)| format!("{id}\t{secs}\n"))
        .collect();
    std::fs::write(dir.join(WATCH_POSITIONS_FILE), body)
}

/// Load saved resume positions, most-recent first. Missing/unreadable → empty. The
/// no-resume hook gives a test run a deterministic start and leaves the user's file alone.
pub fn load_watch_positions() -> Vec<(String, f64)> {
    if crate::hooks::flag("no-resume") {
        return Vec::new();
    }
    config_dir().map(|d| load_watch_positions_in(&d)).unwrap_or_default()
}

/// Persist resume positions (best-effort; ignores I/O errors like the other prefs).
pub fn save_watch_positions(entries: &[(String, f64)]) {
    if crate::hooks::flag("no-resume") {
        return;
    }
    if let Some(dir) = config_dir() {
        let _ = save_watch_positions_in(&dir, entries);
    }
}
