//! Persisted, non-secret user preferences. Each pref is a small plain-text file under
//! `~/.config/moderntube/` (honours `XDG_CONFIG_HOME`). Secrets stay in the keyring via
//! `auth::store`.
//!
//! Two layers:
//! - Free functions (`load_*`/`save_*`): the per-key storage primitives; each feature owns
//!   its file.
//! - [`Prefs`] and [`PrefsStore`]: a typed aggregate of every setting plus change
//!   notification, so distant widgets react to a change (e.g. theme). The store persists
//!   through the same free functions.

use innertube::Quality;
use std::path::{Path, PathBuf};

mod positions;
mod store;

pub use positions::{
    load_watch_positions, remove_position, save_watch_positions, upsert_position, WATCH_POSITIONS_MAX,
};
pub use store::{Prefs, PrefsStore};
#[cfg(test)]
use positions::{load_watch_positions_in, save_watch_positions_in, WATCH_POSITIONS_FILE};
#[cfg(test)]
use store::persist_all_in;

const QUALITY_FILE: &str = "quality";
const SPONSORBLOCK_FILE: &str = "sponsorblock";
const PIP_SIZE_FILE: &str = "pip_size";

/// `~/.config/moderntube` (honours `XDG_CONFIG_HOME`). `None` if neither
/// `XDG_CONFIG_HOME` nor `HOME` is set.
pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("moderntube"))
}

/// Read a single-key text file, trimmed. Missing/unreadable → None.
fn load_str_in(dir: &Path, file: &str) -> Option<String> {
    let s = std::fs::read_to_string(dir.join(file)).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn save_str_in(dir: &Path, file: &str, value: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(file), value)
}

/// A bool key: file contains "1"/"0" (or "true"/"false"); missing → false.
fn load_bool_in(dir: &Path, file: &str) -> bool {
    matches!(load_str_in(dir, file).as_deref(), Some("1") | Some("true"))
}

/// Serialize a cap: `Best -> "best"`, `Height(720) -> "720"`.
fn quality_to_str(q: Quality) -> String {
    match q {
        Quality::Best => "best".to_string(),
        Quality::Height(h) => h.to_string(),
    }
}

/// Parse a saved cap. `"best"` (any case) → `Best`; a positive integer → `Height`;
/// anything else (empty, garbage, non-positive) → `None` so the caller can default.
fn quality_from_str(s: &str) -> Option<Quality> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("best") {
        return Some(Quality::Best);
    }
    s.parse::<i32>().ok().filter(|h| *h > 0).map(Quality::Height)
}

fn load_quality_in(dir: &Path) -> Option<Quality> {
    let s = std::fs::read_to_string(dir.join(QUALITY_FILE)).ok()?;
    quality_from_str(&s)
}

fn save_quality_in(dir: &Path, q: Quality) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(QUALITY_FILE), quality_to_str(q))
}

/// Load the saved quality cap, if any. Missing / invalid file → `None`.
pub fn load_quality() -> Option<Quality> {
    load_quality_in(config_dir()?.as_path())
}

/// Persist the quality cap (best-effort; ignores I/O errors like `auth::store`).
pub fn save_quality(q: Quality) {
    if let Some(dir) = config_dir() {
        let _ = save_quality_in(&dir, q);
    }
}

fn load_sponsor_categories_in(dir: &Path) -> Option<Vec<String>> {
    let s = std::fs::read_to_string(dir.join(SPONSORBLOCK_FILE)).ok()?;
    // Comma-separated category ids; an empty file means "skip nothing" (valid choice).
    Some(s.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect())
}

fn save_sponsor_categories_in(dir: &Path, cats: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(SPONSORBLOCK_FILE), cats.join(","))
}

/// Load the enabled SponsorBlock categories. Missing file → the default set (sponsors
/// only); an empty (but present) file → empty (the user disabled everything).
pub fn load_sponsor_categories() -> Vec<String> {
    config_dir()
        .and_then(|d| load_sponsor_categories_in(&d))
        .unwrap_or_else(crate::sponsorblock::default_categories)
}

/// Persist the enabled SponsorBlock categories (best-effort).
pub fn save_sponsor_categories(cats: &[String]) {
    if let Some(dir) = config_dir() {
        let _ = save_sponsor_categories_in(&dir, cats);
    }
}

fn load_pip_size_in(dir: &Path) -> Option<(i32, i32)> {
    let s = std::fs::read_to_string(dir.join(PIP_SIZE_FILE)).ok()?;
    let (w, h) = s.trim().split_once(',')?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn save_pip_size_in(dir: &Path, w: i32, h: i32) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(PIP_SIZE_FILE), format!("{w},{h}"))
}

/// Load the saved corner-PiP size `(w, h)`, if any.
pub fn load_pip_size() -> Option<(i32, i32)> {
    load_pip_size_in(config_dir()?.as_path())
}

/// Persist the corner-PiP size (best-effort).
pub fn save_pip_size(w: i32, h: i32) {
    if let Some(dir) = config_dir() {
        let _ = save_pip_size_in(&dir, w, h);
    }
}

/// Saved theme mode string ("light"/"dark"/"system"/"oled"), if any.
pub fn load_theme_mode() -> Option<String> {
    load_str_in(config_dir()?.as_path(), "theme_mode")
}

/// Saved accent color hex ("#rrggbb"), if any.
pub fn load_accent() -> Option<String> {
    load_str_in(config_dir()?.as_path(), "accent")
}

/// Whether the nav rail is expanded (default false = collapsed).
pub fn load_nav_expanded() -> bool {
    config_dir().map(|d| load_bool_in(&d, "nav_rail_expanded")).unwrap_or(false)
}
pub fn save_nav_expanded(expanded: bool) {
    if let Some(d) = config_dir() {
        let _ = save_str_in(&d, "nav_rail_expanded", if expanded { "1" } else { "0" });
    }
}

/// Stored as `preview_unmuted`, so a missing file reads as muted ([`Prefs::preview_muted`]).
pub fn save_preview_muted(muted: bool) {
    if let Some(d) = config_dir() {
        let _ = save_str_in(&d, "preview_unmuted", if muted { "0" } else { "1" });
    }
}

/// Whether a watch-sidebar hover opens a streaming preview instead of the animated WebP the
/// reference client uses there. Default false; a missing pref reads as the reference
/// behaviour.
pub fn load_sidebar_streaming_preview() -> bool {
    config_dir().map(|d| load_bool_in(&d, "sidebar_streaming_preview")).unwrap_or(false)
}

/// Caption display settings, stored as `key=value` lines in `captions`.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionPrefs {
    pub enabled: bool,
    /// Preferred track language code; a prefix match (`en` → `en-GB`) counts.
    pub lang: String,
    /// Text size relative to the default (1.0 = 4% of the video height).
    pub scale: f64,
    /// Background box opacity, 0.0–1.0.
    pub bg_opacity: f64,
}

impl Default for CaptionPrefs {
    fn default() -> Self {
        Self { enabled: false, lang: "en".into(), scale: 1.0, bg_opacity: 0.75 }
    }
}

pub fn load_caption_prefs() -> CaptionPrefs {
    let mut p = CaptionPrefs::default();
    let Some(s) = config_dir().and_then(|d| load_str_in(&d, "captions")) else { return p };
    for line in s.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "enabled" => p.enabled = v.trim() == "1",
            "lang" if !v.trim().is_empty() => p.lang = v.trim().to_string(),
            "scale" => p.scale = v.trim().parse().unwrap_or(p.scale),
            "bg" => p.bg_opacity = v.trim().parse().unwrap_or(p.bg_opacity),
            _ => {}
        }
    }
    p
}

pub fn save_caption_prefs(p: &CaptionPrefs) {
    if let Some(d) = config_dir() {
        let body = format!(
            "enabled={}\nlang={}\nscale={}\nbg={}\n",
            u8::from(p.enabled),
            p.lang,
            p.scale,
            p.bg_opacity
        );
        let _ = save_str_in(&d, "captions", &body);
    }
}

/// Default catch-up cap: summarize up to 4h before the join point.
pub const DEFAULT_CATCHUP_CAP_HOURS: f64 = 4.0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A clean, uniquely-named temp dir (parallel-safe: one per test name).
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moderntube-prefs-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn empty_sponsor_file_means_none_enabled() {
        let d = tmp("sb-empty");
        save_sponsor_categories_in(&d, &[]).unwrap();
        // A present, empty file is an explicit empty set (user disabled everything),
        assert_eq!(load_sponsor_categories_in(&d), Some(Vec::new()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn round_trips_watch_positions() {
        let d = tmp("watch");
        let entries = vec![("abc".to_string(), 12.5), ("def".to_string(), 90.0)];
        save_watch_positions_in(&d, &entries).unwrap();
        assert_eq!(load_watch_positions_in(&d), entries);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn watch_positions_skips_garbage_lines() {
        let d = tmp("watch-garbage");
        std::fs::create_dir_all(&d).unwrap();
        // Valid, then a no-tab line, a non-numeric secs, a zero, and a NaN → only the valid one.
        std::fs::write(
            d.join(WATCH_POSITIONS_FILE),
            "vid1\t42.0\nnotab\nvid2\tabc\nvid3\t0\nvid4\tNaN\n",
        )
        .unwrap();
        assert_eq!(load_watch_positions_in(&d), vec![("vid1".to_string(), 42.0)]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn upsert_moves_to_front_and_caps() {
        let mut list = vec![("a".to_string(), 1.0), ("b".to_string(), 2.0)];
        // Re-recording "b" moves it to the front with the new value (no duplicate).
        upsert_position(&mut list, "b", 5.0, 10);
        assert_eq!(list, vec![("b".to_string(), 5.0), ("a".to_string(), 1.0)]);
        // Cap keeps the newest `max`.
        upsert_position(&mut list, "c", 3.0, 2);
        assert_eq!(list, vec![("c".to_string(), 3.0), ("b".to_string(), 5.0)]);
    }

    #[test]
    fn prefs_saved_by_the_individual_savers_load_back() {
        let d = tmp("prefs-full");
        save_sponsor_categories_in(&d, &["sponsor".into(), "intro".into()]).unwrap();
        save_pip_size_in(&d, 480, 270).unwrap();
        save_str_in(&d, "nav_rail_expanded", "1").unwrap();
        save_str_in(&d, "preview_unmuted", "1").unwrap();

        let p = Prefs::load_in(&d);
        assert_eq!(p.sponsor_categories, vec!["sponsor".to_string(), "intro".to_string()]);
        assert_eq!(p.pip_size, Some((480, 270)));
        assert!(p.nav_expanded);
        assert!(!p.preview_muted);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_pref_the_store_persists_survives_a_restart() {
        // `persist_all_in` is a hand-maintained list: a pref missing from it changes in
        // memory and is lost on the next launch.
        let d = tmp("persist-all");
        let mut p = Prefs::defaults();
        p.quality_cap = Some(Quality::Height(720));
        p.theme_mode = "oled".into();
        p.accent = "#3b82f6".into();
        p.auto_return = !p.auto_return;
        p.catchup_cap_hours += 1.5;
        p.pot_backend = "bgutil".into();
        p.direct_opens = !p.direct_opens;
        p.report_watches = !p.report_watches;
        p.play_signed_in = !p.play_signed_in;
        p.home_shorts = !p.home_shorts;
        p.sidebar_streaming_preview = !p.sidebar_streaming_preview;
        assert_ne!(p, Prefs::defaults());

        persist_all_in(&d, &p);
        assert_eq!(Prefs::load_in(&d), p);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_saved_quality_cap_survives_a_restart() {
        let d = tmp("quality");
        for q in [Quality::Best, Quality::Height(720), Quality::Height(2160)] {
            save_quality_in(&d, q).unwrap();
            assert_eq!(load_quality_in(&d), Some(q));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_corrupt_quality_file_is_not_a_cap() {
        let d = tmp("quality-corrupt");
        std::fs::create_dir_all(&d).unwrap();
        for bad in ["", "not-a-quality", "0", "-5"] {
            std::fs::write(d.join(QUALITY_FILE), bad).unwrap();
            assert_eq!(load_quality_in(&d), None, "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unknown_pot_backend_loads_as_a_valid_one() {
        let d = tmp("pot-backend");
        save_str_in(&d, "pot_backend", "jsdom").unwrap();
        let backend = Prefs::load_in(&d).pot_backend;
        assert!(innertube::PotBackendKind::parse(&backend).is_some(), "{backend}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_subscriber_sees_the_new_value_after_a_change() {
        let store = PrefsStore::new(Prefs::defaults());
        let seen: Vec<Rc<Cell<Option<bool>>>> = (0..2).map(|_| Rc::new(Cell::new(None))).collect();
        for cell in &seen {
            let cell = cell.clone();
            store.subscribe(move |p| cell.set(Some(p.auto_return)));
        }
        let flipped = !store.get().auto_return;
        store.set_auto_return(flipped);
        assert_eq!(store.get().auto_return, flipped);
        assert!(seen.iter().all(|c| c.get() == Some(flipped)));
    }
}
