//! Captions over the video: the playing segment session's timedtext cues, drawn as a
//! bottom-centred label. Toggled by the CC button or the `c` key; styled from
//! [`CaptionPrefs`].
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use innertube::{Caption, CaptionTrack, SegmentVod};

use crate::playback::Player;
use crate::prefs::{self, CaptionPrefs};
use crate::runtime::AsyncRuntime;

type EnabledCb = Box<dyn Fn(bool)>;
type LoadedCb = Box<dyn Fn(&CaptionPrefs)>;
type Edit = Box<dyn Fn(&mut CaptionPrefs)>;

pub struct CaptionOverlay {
    label: gtk::Label,
    prefs: RefCell<CaptionPrefs>,
    cues: RefCell<Vec<Caption>>,
    /// Identity of the session the cues belong to (its `Arc` address), 0 for none.
    session_key: Cell<usize>,
    /// Bumped per load; a fetch that returns under an older value is dropped.
    load_gen: Cell<u64>,
    on_enabled: RefCell<Vec<EnabledCb>>,
    on_loaded: RefCell<Vec<LoadedCb>>,
    /// The saved settings have been read from disk.
    disk_read: Cell<bool>,
    early_edits: RefCell<Vec<Edit>>,
    engine: Rc<dyn Player>,
    rt: AsyncRuntime,
}

/// The track for `lang`: human before auto-generated, an exact code before a prefix
/// match; else the first human track; else the first track.
pub fn pick_track(tracks: &[CaptionTrack], lang: &str) -> Option<usize> {
    let lang = lang.to_ascii_lowercase();
    let code = |t: &CaptionTrack| t.language_code.to_ascii_lowercase();
    let rank = |t: &CaptionTrack| -> Option<u8> {
        let c = code(t);
        let inexact = if c == lang {
            0
        } else if c.split('-').next() == lang.split('-').next() {
            1
        } else {
            return None;
        };
        Some(2 * u8::from(t.auto) + inexact)
    };
    tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| rank(t).map(|r| (r, i)))
        .min()
        .map(|(_, i)| i)
        .or_else(|| tracks.iter().position(|t| !t.auto))
        .or_else(|| (!tracks.is_empty()).then_some(0))
}

/// The text of the newest cue active at `t_ms`: a timedtext cue runs on past the start
/// of the next one, which replaces it on screen.
pub fn active_text(cues: &[Caption], t_ms: i64) -> String {
    cues.iter()
        .filter(|c| c.start_ms <= t_ms && t_ms < c.start_ms + c.dur_ms && !c.text.trim().is_empty())
        .max_by_key(|c| c.start_ms)
        .map(|c| c.text.trim().to_string())
        .unwrap_or_default()
}

impl CaptionOverlay {
    pub fn new(engine: Rc<dyn Player>, rt: AsyncRuntime) -> Rc<Self> {
        let label = gtk::Label::new(None);
        label.set_halign(gtk::Align::Center);
        label.set_valign(gtk::Align::End);
        label.set_margin_bottom(72);
        label.set_margin_start(24);
        label.set_margin_end(24);
        label.set_justify(gtk::Justification::Center);
        label.set_wrap(true);
        label.set_can_target(false);
        label.set_visible(false);
        let this = Rc::new(Self {
            label,
            prefs: RefCell::new(CaptionPrefs::default()),
            cues: RefCell::new(Vec::new()),
            session_key: Cell::new(0),
            load_gen: Cell::new(0),
            on_enabled: RefCell::new(Vec::new()),
            on_loaded: RefCell::new(Vec::new()),
            disk_read: Cell::new(false),
            early_edits: RefCell::new(Vec::new()),
            engine,
            rt,
        });
        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            let Ok(p) = gtk::gio::spawn_blocking(prefs::load_caption_prefs).await else { return };
            if let Some(this) = weak.upgrade() {
                this.loaded(p);
            }
        });
        let weak = Rc::downgrade(&this);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            this.tick();
            glib::ControlFlow::Continue
        });
        this
    }

    pub fn widget(&self) -> &gtk::Label {
        &self.label
    }

    pub fn prefs(&self) -> CaptionPrefs {
        self.prefs.borrow().clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.prefs.borrow().enabled
    }

    /// Run `f` once the saved caption settings are read from disk.
    pub fn connect_loaded<F: Fn(&CaptionPrefs) + 'static>(&self, f: F) {
        self.on_loaded.borrow_mut().push(Box::new(f));
    }

    fn loaded(self: &Rc<Self>, mut p: CaptionPrefs) {
        let early = std::mem::take(&mut *self.early_edits.borrow_mut());
        for f in &early {
            f(&mut p);
        }
        self.disk_read.set(true);
        if !early.is_empty() {
            prefs::save_caption_prefs(&p);
        }
        let was_enabled = self.is_enabled();
        self.prefs.replace(p.clone());
        for f in self.on_loaded.borrow().iter() {
            f(&p);
        }
        if p.enabled != was_enabled {
            for f in self.on_enabled.borrow().iter() {
                f(p.enabled);
            }
            if p.enabled {
                self.reload();
            }
        }
    }

    /// Run `f` with the new state whenever captions are switched on or off.
    pub fn connect_enabled<F: Fn(bool) + 'static>(&self, f: F) {
        self.on_enabled.borrow_mut().push(Box::new(f));
    }

    pub fn toggle(self: &Rc<Self>) {
        let on = !self.is_enabled();
        self.set_enabled(on);
    }

    pub fn set_enabled(self: &Rc<Self>, on: bool) {
        if self.is_enabled() == on {
            return;
        }
        self.update(move |p| p.enabled = on);
        for f in self.on_enabled.borrow().iter() {
            f(on);
        }
        if on {
            self.reload();
        }
    }

    pub fn set_lang(self: &Rc<Self>, lang: &str) {
        let lang = lang.to_string();
        self.update(move |p| p.lang = lang.clone());
        self.reload();
    }

    pub fn set_scale(&self, scale: f64) {
        self.update(move |p| p.scale = scale);
    }

    pub fn set_bg_opacity(&self, bg: f64) {
        self.update(move |p| p.bg_opacity = bg);
    }

    /// Before the saved settings are read, an edit applies in memory and is replayed over
    /// them when they arrive; saving then would write defaults over the other fields.
    fn update(&self, f: impl Fn(&mut CaptionPrefs) + 'static) {
        let mut p = self.prefs.borrow_mut();
        f(&mut p);
        if self.disk_read.get() {
            prefs::save_caption_prefs(&p);
        } else {
            self.early_edits.borrow_mut().push(Box::new(f));
        }
    }

    fn current_session(&self) -> Option<Arc<SegmentVod>> {
        self.engine.caption_session()
    }

    /// Forget the cues and fetch them again for the playing session.
    fn reload(self: &Rc<Self>) {
        self.session_key.set(usize::MAX);
    }

    fn load(self: &Rc<Self>, vod: &SegmentVod) {
        let captions = vod.captions().clone();
        let Some(index) = pick_track(captions.tracks(), &self.prefs.borrow().lang) else {
            tracing::info!("[captions] no tracks");
            return;
        };
        let track = &captions.tracks()[index];
        tracing::info!("[captions] loading {} ({}{})", track.name, track.language_code, if track.auto { ", auto" } else { "" });
        let gen = self.load_gen.get() + 1;
        self.load_gen.set(gen);
        let weak = Rc::downgrade(self);
        self.rt.spawn(
            async move { captions.fetch(index).await },
            move |res| {
                let Some(this) = weak.upgrade() else { return };
                if this.load_gen.get() != gen {
                    return;
                }
                match res {
                    Ok(cues) => {
                        tracing::info!("[captions] {} cues", cues.len());
                        *this.cues.borrow_mut() = cues;
                    }
                    Err(e) => tracing::warn!("[captions] fetch failed: {e}"),
                }
            },
        );
    }

    fn tick(self: &Rc<Self>) {
        let vod = self.current_session();
        let key = vod.as_ref().map_or(0, |v| Arc::as_ptr(v) as usize);
        if key != self.session_key.get() {
            self.session_key.set(key);
            self.load_gen.set(self.load_gen.get() + 1);
            self.cues.borrow_mut().clear();
            if let (true, Some(vod)) = (self.is_enabled(), vod.as_ref()) {
                self.load(vod);
            }
        }
        let text = if self.is_enabled() {
            let t_ms = self.engine.time_pos_checked().map_or(-1, |t| (t * 1000.0) as i64);
            active_text(&self.cues.borrow(), t_ms)
        } else {
            String::new()
        };
        if text.is_empty() {
            self.label.set_visible(false);
            return;
        }
        let p = self.prefs.borrow();
        let parent_h = self.label.parent().map_or(360, |w| w.height()).max(120);
        let px = (parent_h as f64 * 0.04 * p.scale).round().max(10.0);
        let alpha = (p.bg_opacity.clamp(0.0, 1.0) * 100.0).round().max(1.0);
        self.label.set_markup(&format!(
            "<span font=\"{px}px\" foreground=\"#ffffff\" background=\"#000000\" bgalpha=\"{alpha}%\"> {} </span>",
            glib::markup_escape_text(&text).replace('\n', " \n ")
        ));
        self.label.set_visible(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(code: &str, auto: bool) -> CaptionTrack {
        CaptionTrack { language_code: code.into(), name: code.into(), auto, base_url: String::new() }
    }

    #[test]
    fn picks_a_human_track_over_an_auto_one_in_the_same_language() {
        let tracks = [track("en", true), track("de", false), track("en-GB", false)];
        assert_eq!(pick_track(&tracks, "en"), Some(2));
    }

    #[test]
    fn shows_only_the_newest_active_cue() {
        let cue = |s, d, t: &str| Caption { start_ms: s, dur_ms: d, text: t.into() };
        let cues = [cue(0, 5000, "a"), cue(1000, 5000, "b"), cue(2000, 5000, "c"), cue(9000, 100, "d")];
        assert_eq!(active_text(&cues, 2500), "c");
        assert_eq!(active_text(&cues, 8000), "");
    }
}
