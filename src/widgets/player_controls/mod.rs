//! The transport bar overlaid on the video: play/pause, a seek scale + time labels
//! (VOD), and a volume control. A poll timer mirrors the player's state into the widgets.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, DrawingArea, EventControllerMotion, Label, MenuButton, Orientation, Overlay,
    Popover, Revealer, RevealerTransitionType, Scale,
};

use innertube::{Quality, Storyboard};

use crate::playback::Player;
use crate::runtime::AsyncRuntime;
use crate::sponsorblock::Segment;

mod sponsor_bar;
mod storyboard_preview;

pub use sponsor_bar::build_sponsorblock_toggles;

/// Handler invoked when the user picks a quality from the menu.
type QualityCb = Rc<RefCell<Option<Rc<dyn Fn(Quality)>>>>;

/// Catch-me-up window modes: which missed span to summarize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchupMode {
    /// The capped window ending at the point the viewer joined.
    BeforeJoin,
    /// The gap the viewer skipped: from the pause point to the current live edge.
    WhilePaused,
}

/// Handler invoked when a catch-up window mode is picked.
type CatchupCb = Rc<RefCell<Option<Rc<dyn Fn(CatchupMode)>>>>;

/// Display label for a quality: `Best -> "Auto"`, `Height(720) -> "720p"`.
fn quality_label(q: &Quality) -> String {
    match q {
        Quality::Best => "Auto".to_string(),
        Quality::Height(h) => format!("{h}p"),
    }
}

/// `83.0 -> "1:23"`, `3725.0 -> "1:02:05"`.
pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

pub struct PlayerControls {
    pub root: GtkBox,
    scale: Scale,
    pos_label: Label,
    dur_label: Label,
    time_sep: Label,
    live_btn: Button,
    edge: Button,
    /// "Catch me up" trigger (live only).
    catchup: MenuButton,
    /// "Ask about this video": opens the transcript search + Q&A page.
    ask: Button,
    catchup_cb: CatchupCb,
    fullscreen: Button,
    quality: MenuButton,
    quality_menu: GtkBox,
    /// Handler invoked when the user picks a quality (set by `window::quality`).
    quality_cb: QualityCb,
    /// Current video's seek-preview storyboard (None for live / no storyboard).
    storyboard: Rc<RefCell<Option<Storyboard>>>,
    /// External play/pause button mirror (the corner miniplayer's), updated by the poll.
    pause_mirror: PauseMirror,
    /// SponsorBlock segments drawn as colored bars on the seek bar, and the drawing
    /// surface that renders them (duration is tracked by the poll via `sponsor_dur`).
    sponsor_segs: Rc<RefCell<Vec<Segment>>>,
    seg_draw: DrawingArea,
    /// The scrub preview: an overlay child the PlayerView mounts above the transport,
    /// margin-positioned to follow the hover x. A Popover moved per motion event ghosts on
    /// Wayland.
    preview: GtkBox,
    cc: gtk::ToggleButton,
    mute: Button,
}

/// A registered callback that mirrors the engine's pause state to an external button.
type PauseMirror = Rc<RefCell<Option<Rc<dyn Fn(bool)>>>>;

impl PlayerControls {
    pub fn new(engine: Rc<dyn Player>, rt: AsyncRuntime) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        let row = GtkBox::new(Orientation::Horizontal, 2);
        root.add_css_class("player-bar");

        let play = Button::from_icon_name("media-playback-pause-symbolic");
        let pos_label = Label::new(Some("0:00"));
        let dur_label = Label::new(Some("0:00"));
        let time_sep = Label::new(Some("/"));
        for l in [&pos_label, &time_sep, &dur_label] {
            l.add_css_class("time");
        }
        time_sep.add_css_class("dim");
        dur_label.add_css_class("dim");
        let scale = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 1.0);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        let volume = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
        volume.set_width_request(76);
        volume.set_value(100.0);
        volume.set_draw_value(false);
        volume.add_css_class("volume-slider");
        let mute = Button::from_icon_name("audio-volume-high-symbolic");
        mute.set_tooltip_text(Some("Mute (m)"));
        mute.add_css_class("mute");
        let volume_reveal = Revealer::builder()
            .transition_type(RevealerTransitionType::SlideRight)
            .transition_duration(160)
            .child(&volume)
            .build();
        let volume_group = GtkBox::new(Orientation::Horizontal, 0);
        volume_group.append(&mute);
        volume_group.append(&volume_reveal);
        {
            let hover = EventControllerMotion::new();
            let r = volume_reveal.clone();
            hover.connect_enter(move |_, _, _| r.set_reveal_child(true));
            let r = volume_reveal.clone();
            hover.connect_leave(move |_| r.set_reveal_child(false));
            volume_group.add_controller(hover);
        }
        let live_badge = Label::new(None);
        live_badge.set_markup(LIVE_AT_HEAD);
        let live_btn = Button::builder().child(&live_badge).has_frame(false).tooltip_text("Go to live").build();
        live_btn.add_css_class("live-pill");
        live_btn.set_visible(false);
        let edge = Button::from_icon_name("media-skip-forward-symbolic");
        edge.set_tooltip_text(Some("Go to live"));
        edge.set_visible(false);
        // "Catch me up" (live only): a MenuButton offering the two window modes; the popover
        // buttons dispatch through `catchup_cb` (set via `connect_catchup`).
        let catchup = MenuButton::builder()
            .icon_name("document-open-recent-symbolic")
            .tooltip_text("Catch me up — summarize what you missed")
            .build();
        catchup.set_visible(false);
        let catchup_cb: CatchupCb = Rc::new(RefCell::new(None));
        {
            let menu = GtkBox::new(Orientation::Vertical, 0);
            // 'Before joining' summarizes the stream history before the viewer tuned in; 'while
            // paused' summarizes the gap from the pause to the live edge.
            let since_join = Button::with_label("What I missed before joining");
            since_join.add_css_class("flat");
            let since_pause = Button::with_label("While I was paused");
            since_pause.add_css_class("flat");
            menu.append(&since_join);
            menu.append(&since_pause);
            let pop = Popover::new();
            pop.set_child(Some(&menu));
            catchup.set_popover(Some(&pop));
            let cb = catchup_cb.clone();
            let p = pop.clone();
            since_join.connect_clicked(move |_| {
                p.popdown();
                if let Some(f) = cb.borrow().as_ref().cloned() {
                    f(CatchupMode::BeforeJoin);
                }
            });
            let cb = catchup_cb.clone();
            since_pause.connect_clicked(move |_| {
                pop.popdown();
                if let Some(f) = cb.borrow().as_ref().cloned() {
                    f(CatchupMode::WhilePaused);
                }
            });
        }
        let ask = Button::from_icon_name("edit-find-symbolic");
        ask.set_tooltip_text(Some("Ask about this video — search the transcript, ask questions"));
        let fullscreen = Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.set_tooltip_text(Some("Fullscreen"));
        let cc = gtk::ToggleButton::builder()
            .icon_name("media-view-subtitles-symbolic")
            .tooltip_text("Captions (c)")
            .build();

        // Quality menu (VOD only): a MenuButton labelled with the current quality,
        // opening a popover of options rebuilt per video via `set_qualities`.
        let quality = MenuButton::builder().label("Auto").tooltip_text("Quality").build();
        quality.add_css_class("quality-chip");
        let quality_menu = GtkBox::new(Orientation::Vertical, 0);
        let popover = Popover::new();
        popover.set_child(Some(&quality_menu));
        quality.set_popover(Some(&popover));
        quality.set_visible(false); // shown once a VOD reports its heights

        // SponsorBlock timeline: a pass-through DrawingArea overlaid on the seek bar that
        // paints colored bars for each segment (green sponsor, yellow self-promo, …).
        let sponsor_segs: Rc<RefCell<Vec<Segment>>> = Rc::new(RefCell::new(Vec::new()));
        let sponsor_dur = Rc::new(Cell::new(0.0f64));
        let seg_draw = sponsor_bar::build_sponsor_timeline(&sponsor_segs, &sponsor_dur);
        let seek_overlay = Overlay::new();
        seek_overlay.set_hexpand(true);
        seek_overlay.add_css_class("player-seek");
        seek_overlay.set_child(Some(&scale));
        seek_overlay.add_overlay(&seg_draw);

        // The seek bar spans the full width on its own row, above the buttons.
        root.append(&seek_overlay);
        root.append(&row);
        row.append(&play);
        row.append(&volume_group);
        row.append(&live_btn);
        let times = GtkBox::new(Orientation::Horizontal, 4);
        times.add_css_class("times");
        times.append(&pos_label);
        times.append(&time_sep);
        times.append(&dur_label);
        row.append(&times);
        row.append(&edge);
        let spacer = GtkBox::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        row.append(&spacer);
        // Settings gear: playback speed.
        let gear = MenuButton::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Settings")
            .build();
        let gear_menu = GtkBox::new(Orientation::Vertical, 2);
        gear_menu.set_margin_top(4);
        gear_menu.set_margin_bottom(4);
        gear_menu.set_margin_start(4);
        gear_menu.set_margin_end(4);
        let gear_pop = Popover::new();
        gear_pop.set_child(Some(&gear_menu));
        gear.set_popover(Some(&gear_pop));
        let speed_header = Label::builder().label("Playback speed").css_classes(["dim-label", "caption"]).xalign(0.0).build();
        gear_menu.append(&speed_header);
        for (label, val) in [
            ("0.25×", 0.25), ("0.5×", 0.5), ("0.75×", 0.75), ("Normal", 1.0),
            ("1.25×", 1.25), ("1.5×", 1.5), ("1.75×", 1.75), ("2×", 2.0),
        ] {
            let b = Button::builder().label(label).has_frame(false).build();
            if let Some(c) = b.child().and_downcast::<Label>() {
                c.set_xalign(0.0);
            }
            let engine = engine.clone();
            let gear_pop = gear_pop.clone();
            b.connect_clicked(move |_| {
                engine.set_speed(val);
                gear_pop.popdown();
            });
            gear_menu.append(&b);
        }

        row.append(&catchup);
        row.append(&ask);
        row.append(&cc);
        row.append(&quality);
        row.append(&gear);
        row.append(&fullscreen);

        let quality_cb: QualityCb = Rc::new(RefCell::new(None));

        // Time of the last user-driven scale change. The poll leaves the thumb alone for 400 ms
        // after it. A timestamp is used because a seeking flag stays set when a drag-scrub
        // cancels the click gesture.
        let last_seek = Rc::new(Cell::new(None::<std::time::Instant>));

        // Play/pause.
        {
            let engine = engine.clone();
            let play_btn = play.clone();
            play.connect_clicked(move |_| {
                engine.toggle_pause();
                play_btn.set_icon_name(pause_icon(engine.is_paused()));
            });
        }

        // Seek: drag/scroll/click updates the engine live (absolute seconds = scale
        // value) and stamps `last_seek` so the poll timer doesn't fight the user.
        {
            let engine = engine.clone();
            let last_seek = last_seek.clone();
            // A drag reports every step; only the value it settles on is sought, so one drag
            // is one seek (each seek flushes the pipeline).
            let gen = Rc::new(Cell::new(0u64));
            scale.connect_change_value(move |_, _, value| {
                last_seek.set(Some(std::time::Instant::now()));
                let g = gen.get().wrapping_add(1);
                gen.set(g);
                let (engine, gen) = (engine.clone(), gen.clone());
                glib::timeout_add_local_once(Duration::from_millis(SEEK_SETTLE_MS), move || {
                    if gen.get() == g {
                        engine.seek_absolute(value);
                    }
                });
                glib::Propagation::Proceed
            });
        }

        // Volume.
        {
            let engine = engine.clone();
            let mute_icon = mute.clone();
            volume.connect_value_changed(move |s| {
                engine.set_volume(s.value());
                mute_icon.set_icon_name(volume_icon(s.value()));
            });
        }
        {
            let volume = volume.clone();
            let before = Rc::new(Cell::new(100.0f64));
            mute.connect_clicked(move |_| {
                if volume.value() > 0.0 {
                    before.set(volume.value());
                    volume.set_value(0.0);
                } else {
                    volume.set_value(before.get().max(5.0));
                }
            });
        }


        // Poll timer: mirror the player → widgets (skip the scale while the user is seeking).
        let storyboard: Rc<RefCell<Option<Storyboard>>> = Rc::new(RefCell::new(None));
        let dur = Rc::new(Cell::new(0.0f64));
        // Optional external play/pause button to keep in sync (the corner miniplayer).
        let pause_mirror: PauseMirror = Rc::new(RefCell::new(None));
        {
            let engine = engine.clone();
            let scale = scale.clone();
            let pos_label = pos_label.clone();
            let dur_label = dur_label.clone();
            let play_btn = play.clone();
            let last_seek = last_seek.clone();
            let dur_cell = dur.clone();
            let pause_mirror = pause_mirror.clone();
            let sponsor_dur_p = sponsor_dur.clone();
            let seg_draw_p = seg_draw.clone();
            let live_badge = live_badge.clone();
            let edge = edge.clone();
            let volume_p = volume.clone();
            glib::timeout_add_local(Duration::from_millis(250), move || {
                let pos = engine.time_pos();
                if (pos * 4.0) as i64 % 4 == 0 && crate::diag::avsync() {
                    tracing::info!("[avsync] pos={pos:.1} avsync={:.4}", engine.avsync());
                }
                pos_label.set_text(&fmt_time(pos));
                if let Some(d) = engine.duration() {
                    dur_cell.set(d); // mirrored for the hover handler
                    if (scale.adjustment().upper() - d).abs() > 0.5 {
                        scale.set_range(0.0, d);
                        // Duration changed (new video parsed) → repaint SponsorBlock bars.
                        sponsor_dur_p.set(d);
                        seg_draw_p.queue_draw();
                    }
                    dur_label.set_text(&fmt_time(d));
                    // Follow the playhead unless the user moved the thumb in the last 400ms.
                    let user_seeking =
                        last_seek.get().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
                    if !user_seeking {
                        scale.set_value(pos);
                    }
                } else if let Some((lo, head)) = engine.live_window() {
                    scale.set_visible(true);
                    edge.set_visible(false);
                    let adj = scale.adjustment();
                    if (adj.lower() - lo).abs() > 0.5 || (adj.upper() - head).abs() > 0.5 {
                        scale.set_range(lo, head);
                    }
                    let user_seeking =
                        last_seek.get().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
                    if !user_seeking {
                        scale.set_value(pos.clamp(lo, head));
                    }
                    let at_head = engine.live_at_head().unwrap_or(true);
                    pos_label.set_visible(!at_head);
                    pos_label.set_text(&format!("−{}", fmt_time((head - pos).max(0.0))));
                    live_badge.set_markup(if at_head { LIVE_AT_HEAD } else { LIVE_BEHIND });
                }
                let v = engine.volume();
                if (volume_p.value() - v).abs() > 0.5 {
                    volume_p.set_value(v);
                }
                let paused = engine.is_paused();
                play_btn.set_icon_name(pause_icon(paused));
                // Mirror the pause state to the corner miniplayer's play/pause button, so
                // keyboard pause updates it too.
                if let Some(m) = pause_mirror.borrow().as_ref() {
                    m(paused);
                }
                glib::ControlFlow::Continue
            });
        }

        // Hover preview: a thumbnail of the hovered position and its timestamp above the
        // scrubber. A sprite-sheet is fetched and decoded once and cached for the session.
        // The preview is an overlay child positioned with margins to follow the hover x (moving
        // an xdg_popup per motion event ghosts on Wayland). PlayerView mounts it via
        // `preview_widget`.
        let preview = storyboard_preview::build_scrub_preview(&engine, rt, &scale, &storyboard, &dur);

        Self {
            root, scale, pos_label, dur_label, time_sep, live_btn, edge, catchup, ask, catchup_cb, fullscreen, quality, quality_menu, quality_cb,
            storyboard, pause_mirror, sponsor_segs, seg_draw, preview, cc, mute,
        }
    }

    /// The captions toggle; the PlayerView binds it to its caption overlay.
    pub fn cc_button(&self) -> &gtk::ToggleButton {
        &self.cc
    }

    /// Mute, or restore the volume from before the mute.
    pub fn toggle_mute(&self) {
        self.mute.emit_clicked();
    }

    /// The scrub-preview overlay child; the PlayerView adds it to its overlay so it floats above
    /// the transport.
    pub fn preview_widget(&self) -> &GtkBox {
        &self.preview
    }

    /// Runs `f(mode)` when a "Catch me up" window mode is picked.
    pub fn connect_catchup<F: Fn(CatchupMode) + 'static>(&self, f: F) {
        *self.catchup_cb.borrow_mut() = Some(Rc::new(f));
    }

    /// Run `f` when "Ask about this video" is clicked.
    pub fn connect_ask<F: Fn() + 'static>(&self, f: F) {
        self.ask.connect_clicked(move |_| f());
    }

    pub fn click_ask(&self) {
        self.ask.emit_clicked();
    }

    /// Registers an external play/pause button (the corner miniplayer's) to mirror the engine's
    /// pause state through the poll. `f(paused)`.
    pub fn set_pause_mirror<F: Fn(bool) + 'static>(&self, f: F) {
        *self.pause_mirror.borrow_mut() = Some(Rc::new(f));
    }

    /// Set (or clear, for live) the current video's seek-preview storyboard.
    pub fn set_storyboard(&self, sb: Option<Storyboard>) {
        *self.storyboard.borrow_mut() = sb;
    }

    /// Set the SponsorBlock segments to paint on the seek bar (duration is tracked by the
    /// poll). Pass an empty vec to clear (e.g. on a new video before its segments load).
    pub fn set_sponsor_segments(&self, segments: Vec<Segment>) {
        *self.sponsor_segs.borrow_mut() = segments;
        self.seg_draw.queue_draw();
    }

    /// Store the handler invoked when the user picks a quality from the menu.
    pub fn connect_quality_selected<F: Fn(Quality) + 'static>(&self, f: F) {
        *self.quality_cb.borrow_mut() = Some(Rc::new(f));
    }

    /// Rebuild the quality menu for the current VOD from its available heights, with
    /// `current` marked on the button. Empty heights → hide the menu (e.g. live).
    pub fn set_qualities(&self, heights: &[i32], current: &Quality) {
        while let Some(child) = self.quality_menu.first_child() {
            self.quality_menu.remove(&child);
        }
        if heights.is_empty() {
            self.quality.set_visible(false);
            return;
        }
        self.quality.set_visible(true);
        self.quality.set_label(&quality_label(current));
        let mut options = vec![Quality::Best];
        options.extend(heights.iter().map(|h| Quality::Height(*h)));
        for q in options {
            let btn = Button::builder().label(quality_label(&q)).has_frame(false).build();
            let cb = self.quality_cb.clone();
            let menu_btn = self.quality.clone();
            let popover = self.quality.popover();
            btn.connect_clicked(move |_| {
                menu_btn.set_label(&quality_label(&q));
                if let Some(p) = &popover {
                    p.popdown();
                }
                if let Some(cb) = cb.borrow().as_ref() {
                    cb(q);
                }
            });
            self.quality_menu.append(&btn);
        }
    }

    /// Run `f` when the fullscreen button is clicked (the window owns the actual
    /// fullscreen state, so `window::nav` supplies the toggle).
    pub fn connect_fullscreen<F: Fn() + 'static>(&self, f: F) {
        self.fullscreen.connect_clicked(move |_| f());
    }

    /// Runs `f` when the live-edge button is clicked. The handler re-joins the stream at the
    /// current live head.
    pub fn connect_live_edge<F: Fn() + 'static>(&self, f: F) {
        let f = Rc::new(f);
        let g = f.clone();
        self.edge.connect_clicked(move |_| f());
        self.live_btn.connect_clicked(move |_| g());
    }

    /// Reflect the current fullscreen state on the button (icon + tooltip).
    pub fn set_fullscreen_state(&self, fs: bool) {
        self.fullscreen.set_icon_name(if fs {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
        self.fullscreen.set_tooltip_text(Some(if fs { "Leave fullscreen" } else { "Fullscreen" }));
    }

    /// Clears the previous video's position and duration before a new video loads.
    pub fn reset(&self) {
        self.scale.set_range(0.0, 1.0);
        self.scale.set_value(0.0);
        self.pos_label.set_text("0:00");
        self.dur_label.set_text("0:00");
        self.live_btn.set_visible(false);
        self.edge.set_visible(false);
        self.catchup.set_visible(false);
    }

    /// Configure the bar for live vs VOD: live hides the scrub bar + duration and shows
    /// the LIVE badge + edge button.
    pub fn set_live(&self, live: bool) {
        self.scale.set_visible(!live);
        self.dur_label.set_visible(!live);
        self.time_sep.set_visible(!live);
        self.pos_label.set_visible(!live);
        self.live_btn.set_visible(live);
        self.edge.set_visible(live);
        self.catchup.set_visible(live);
        if live {
            // Quality selection is VOD-only; never show its menu on a live stream.
            self.quality.set_visible(false);
        }
    }
}

/// How long the seek bar must rest before a drag's value is sought.
const SEEK_SETTLE_MS: u64 = 150;

const LIVE_AT_HEAD: &str = "<span foreground='#ff4e45'>●</span> LIVE";
const LIVE_BEHIND: &str = "<span foreground='#9a9a9a'>●</span> LIVE";

fn volume_icon(v: f64) -> &'static str {
    match v {
        v if v <= 0.0 => "audio-volume-muted-symbolic",
        v if v < 34.0 => "audio-volume-low-symbolic",
        v if v < 67.0 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

fn pause_icon(paused: bool) -> &'static str {
    if paused {
        "media-playback-start-symbolic"
    } else {
        "media-playback-pause-symbolic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_minutes_and_hours() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(83.0), "1:23");
        assert_eq!(fmt_time(3725.0), "1:02:05");
        assert_eq!(fmt_time(-5.0), "0:00");
    }
}
