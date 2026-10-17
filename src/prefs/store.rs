//! The typed [`Prefs`] snapshot and the observable [`PrefsStore`] around it.
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use innertube::Quality;

use super::*;

// ---- typed aggregate and change notification ----

/// Every non-secret user setting as one typed snapshot read from the individual pref
/// files. It excludes runtime data such as per-video resume positions. Mutate through
/// [`PrefsStore`] so the change is persisted and subscribers are notified.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub quality_cap: Option<Quality>,
    pub sponsor_categories: Vec<String>,
    pub pip_size: Option<(i32, i32)>,
    pub theme_mode: String,
    pub accent: String,
    pub nav_expanded: bool,
    pub preview_muted: bool,
    pub auto_return: bool,
    /// Opt in to a streaming preview where YouTube shows an animated image.
    pub sidebar_streaming_preview: bool,
    /// Hours before the join point that "Catch me up" summarizes (a 24/7 stream is otherwise
    /// unbounded). Default 4.0.
    pub catchup_cap_hours: f64,
    /// PO-token minter: "bgutil" (sidecar on :4416) or "native" (in-process BotGuard).
    pub pot_backend: String,
    /// Each click opens as a new visitor on its own watch page, which YouTube does not hold for an ad slot.
    pub direct_opens: bool,
    /// Report signed-in watches to YouTube, so they appear in the account's history with progress.
    pub report_watches: bool,
    /// Videos stream on the signed-in session, as a signed-in browser does.
    pub play_signed_in: bool,
    /// The home feed shows its Shorts shelves.
    pub home_shorts: bool,
}

impl Prefs {
    /// The defaults used when no file exists, matching the `load_*` fallbacks.
    pub fn defaults() -> Self {
        Prefs {
            quality_cap: None,
            sponsor_categories: crate::sponsorblock::default_categories(),
            pip_size: None,
            theme_mode: "system".to_string(),
            accent: crate::theme::DEFAULT_ACCENT.to_string(),
            nav_expanded: false,
            preview_muted: true,
            auto_return: true,
            sidebar_streaming_preview: false,
            catchup_cap_hours: DEFAULT_CATCHUP_CAP_HOURS,
            pot_backend: innertube::PotBackendKind::default().as_str().to_string(),
            direct_opens: false,
            report_watches: false,
            play_signed_in: false,
            home_shorts: true,
        }
    }

    pub(super) fn load_in(dir: &Path) -> Self {
        Prefs {
            quality_cap: load_quality_in(dir),
            sponsor_categories: load_sponsor_categories_in(dir)
                .unwrap_or_else(crate::sponsorblock::default_categories),
            pip_size: load_pip_size_in(dir),
            theme_mode: load_str_in(dir, "theme_mode").unwrap_or_else(|| "system".to_string()),
            accent: load_str_in(dir, "accent")
                .unwrap_or_else(|| crate::theme::DEFAULT_ACCENT.to_string()),
            nav_expanded: load_bool_in(dir, "nav_rail_expanded"),
            preview_muted: !load_bool_in(dir, "preview_unmuted"),
            auto_return: !load_bool_in(dir, "no_auto_return"),
            sidebar_streaming_preview: load_bool_in(dir, "sidebar_streaming_preview"),
            direct_opens: load_bool_in(dir, "direct_opens"),
            report_watches: load_bool_in(dir, "report_watches"),
            play_signed_in: load_bool_in(dir, "play_signed_in"),
            home_shorts: !load_bool_in(dir, "hide_home_shorts"),
            catchup_cap_hours: load_str_in(dir, "catchup_cap_hours")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|h| *h > 0.0)
                .unwrap_or(DEFAULT_CATCHUP_CAP_HOURS),
            pot_backend: load_str_in(dir, "pot_backend")
                .filter(|s| innertube::PotBackendKind::parse(s).is_some())
                .unwrap_or_else(|| innertube::PotBackendKind::default().as_str().to_string()),
        }
    }

    /// Load the full typed snapshot from disk (defaults where a file is missing).
    pub fn load() -> Self {
        config_dir().map(|d| Self::load_in(&d)).unwrap_or_else(Self::defaults)
    }
}

pub(super) type Subscriber = Box<dyn Fn(&Prefs)>;

/// Typed, observable preferences. Holds the current [`Prefs`] and a list of subscribers;
/// a `set_*` call updates the in-memory snapshot and notifies every subscriber. Persistence
/// is a subscriber too (see [`PrefsStore::persist_on_change`]), which keeps the store
/// disk-free and testable. Main thread only (`Rc`/`RefCell`); clones share the store.
#[derive(Clone)]
pub struct PrefsStore {
    pub(super) inner: Rc<RefCell<Prefs>>,
    subs: Rc<RefCell<Vec<Subscriber>>>,
}

/// Write every pref the `set_*` API can change into `dir`.
///
/// A hand-maintained list, split out of `persist_on_change` so the covered set is testable.
/// A field added to `Prefs` with a setter but missing here is not persisted.
pub(super) fn persist_all_in(dir: &Path, p: &Prefs) {
    if let Some(q) = p.quality_cap {
        let _ = save_quality_in(dir, q);
    }
    let _ = save_str_in(dir, "theme_mode", &p.theme_mode);
    let _ = save_str_in(dir, "accent", &p.accent);
    let _ = save_str_in(dir, "no_auto_return", if p.auto_return { "0" } else { "1" });
    let _ = save_str_in(dir, "catchup_cap_hours", &format!("{}", p.catchup_cap_hours));
    let _ = save_str_in(dir, "pot_backend", &p.pot_backend);
    let _ = save_str_in(dir, "direct_opens", if p.direct_opens { "1" } else { "0" });
    let _ = save_str_in(dir, "report_watches", if p.report_watches { "1" } else { "0" });
    let _ = save_str_in(dir, "play_signed_in", if p.play_signed_in { "1" } else { "0" });
    let _ = save_str_in(dir, "hide_home_shorts", if p.home_shorts { "0" } else { "1" });
    let _ = save_str_in(
        dir,
        "sidebar_streaming_preview",
        if p.sidebar_streaming_preview { "1" } else { "0" },
    );
}

impl PrefsStore {
    pub fn new(prefs: Prefs) -> Self {
        PrefsStore { inner: Rc::new(RefCell::new(prefs)), subs: Rc::new(RefCell::new(Vec::new())) }
    }

    /// Build a store from the on-disk snapshot and register the persistence subscriber, so
    /// `set_*` calls write through to disk. Tests use [`PrefsStore::new`] to stay disk-free.
    pub fn load() -> Self {
        let store = Self::new(Prefs::load());
        store.persist_on_change();
        store
    }

    /// Register a subscriber that persists the covered fields via the per-key `save_*`
    /// functions. Normally called once by `load()`.
    pub fn persist_on_change(&self) {
        // Each save is a tiny file write, so writing the whole covered set on every change
        // keeps disk equal to memory.
        self.subscribe(|p| {
            if let Some(d) = config_dir() {
                persist_all_in(&d, p);
            }
        });
    }

    /// A clone of the current snapshot.
    pub fn get(&self) -> Prefs {
        self.inner.borrow().clone()
    }

    /// Register a change subscriber. It is not called on registration; callers apply the
    /// current value themselves at startup.
    pub fn subscribe(&self, f: impl Fn(&Prefs) + 'static) {
        self.subs.borrow_mut().push(Box::new(f));
    }

    fn notify(&self) {
        // Clone the snapshot so subscribers can't deadlock on `inner`.
        let snap = self.inner.borrow().clone();
        for sub in self.subs.borrow().iter() {
            sub(&snap);
        }
    }

    /// Set theme mode and accent together and notify.
    pub fn set_appearance(&self, mode: &str, accent: &str) {
        {
            let mut p = self.inner.borrow_mut();
            p.theme_mode = mode.to_string();
            p.accent = accent.to_string();
        }
        self.notify();
    }

    /// Set the quality cap and notify.
    pub fn set_quality_cap(&self, q: Quality) {
        self.inner.borrow_mut().quality_cap = Some(q);
        self.notify();
    }

    /// Set auto-return-on-end and notify.
    pub fn set_auto_return(&self, on: bool) {
        self.inner.borrow_mut().auto_return = on;
        self.notify();
    }

    /// Set the watch-sidebar streaming preview and notify.
    pub fn set_sidebar_streaming_preview(&self, on: bool) {
        self.inner.borrow_mut().sidebar_streaming_preview = on;
        self.notify();
    }

    pub fn set_direct_opens(&self, on: bool) {
        self.inner.borrow_mut().direct_opens = on;
        self.notify();
    }

    pub fn set_report_watches(&self, on: bool) {
        self.inner.borrow_mut().report_watches = on;
        self.notify();
    }

    pub fn set_play_signed_in(&self, on: bool) {
        self.inner.borrow_mut().play_signed_in = on;
        self.notify();
    }

    pub fn set_home_shorts(&self, on: bool) {
        self.inner.borrow_mut().home_shorts = on;
        self.notify();
    }

    pub fn set_pot_backend(&self, kind: innertube::PotBackendKind) {
        self.inner.borrow_mut().pot_backend = kind.as_str().to_string();
        self.notify();
    }

    /// Hours before the join.
    pub fn set_catchup_cap_hours(&self, hours: f64) {
        self.inner.borrow_mut().catchup_cap_hours = hours;
        self.notify();
    }
}
