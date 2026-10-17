//! The persistent player surface: the video widget in a `GtkOverlay` with the transport controls
//! in an auto-hiding `Revealer`. One instance serves every video.
//!
//! The miniplayer hosts this overlay in a top-level layer and repositions it between full and
//! corner without reparenting, so the GL context survives. In mini mode the transport hides and
//! corner controls (expand top-left, close top-right, play/pause bottom) fade in on hover.
use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Button, Overlay, Revealer, RevealerTransitionType};

use crate::playback::Player;
use crate::runtime::AsyncRuntime;
use crate::widgets::{CaptionOverlay, PlayerControls};

pub struct PlayerView {
    pub root: Overlay,
    pub controls: PlayerControls,
    pub captions: Rc<CaptionOverlay>,
    spinner: gtk::Spinner,
    /// "2×" badge, top-center, shown while hold-space fast-forward is engaged.
    speed_badge: Revealer,
    /// Full transport bar (hidden in mini mode).
    revealer: Revealer,
    /// Corner-mode controls (hover-revealed, crossfade): expand · close · play-pause.
    mini_revs: [Revealer; 3],
    mini_expand: Button,
    mini_close: Button,
    /// True while the surface is the corner miniplayer. The corner controls then replace the
    /// idle/motion reveal of the full transport.
    mini: Rc<Cell<bool>>,
    /// Generation for the corner-controls idle-hide (newest scheduled hide wins).
    mini_gen: Rc<Cell<u64>>,
}

/// Reveal the controls and schedule a hide 3s later. A generation counter ensures only
/// the most recent schedule actually hides (so we never remove already-fired sources).
fn reveal_then_hide(revealer: &Revealer, generation: &Rc<Cell<u64>>) {
    revealer.set_reveal_child(true);
    let g = generation.get().wrapping_add(1);
    generation.set(g);
    let revealer = revealer.clone();
    let generation = generation.clone();
    glib::timeout_add_seconds_local_once(3, move || {
        if generation.get() == g {
            revealer.set_reveal_child(false);
        }
    });
}

/// Crossfade the corner controls in, then schedule them to fade after 2s of no motion.
fn reveal_mini(revs: &[Revealer; 3], gen: &Rc<Cell<u64>>) {
    for r in revs {
        r.set_reveal_child(true);
    }
    let g = gen.get().wrapping_add(1);
    gen.set(g);
    let gen = gen.clone();
    let revs = revs.clone();
    glib::timeout_add_seconds_local_once(2, move || {
        if gen.get() == g {
            for r in &revs {
                r.set_reveal_child(false);
            }
        }
    });
}

fn play_icon(paused: bool) -> &'static str {
    if paused {
        "media-playback-start-symbolic"
    } else {
        "media-playback-pause-symbolic"
    }
}

impl PlayerView {
    pub fn new(engine: Rc<dyn Player>, rt: AsyncRuntime) -> Self {
        let area = engine.widget();
        // A click on the video moves keyboard focus off any text field (comment composer, search
        // box). The window's key controller ignores keys while a text widget has focus.
        area.set_focusable(true);
        {
            let click_focus = gtk::GestureClick::new();
            let area_f = area.clone();
            click_focus.connect_pressed(move |_, _, _, _| {
                area_f.grab_focus();
            });
            area.add_controller(click_focus);
        }
        let captions = CaptionOverlay::new(engine.clone(), rt.clone());
        let controls = PlayerControls::new(engine.clone(), rt);
        {
            let cc = controls.cc_button();
            cc.set_active(captions.is_enabled());
            let weak = Rc::downgrade(&captions);
            cc.connect_toggled(move |b| {
                if let Some(c) = weak.upgrade() {
                    c.set_enabled(b.is_active());
                }
            });
            let cc = cc.clone();
            captions.connect_enabled(move |on| cc.set_active(on));
        }

        let revealer = Revealer::new();
        revealer.set_transition_type(RevealerTransitionType::Crossfade);
        revealer.set_valign(gtk::Align::End);
        revealer.set_child(Some(&controls.root));
        revealer.set_reveal_child(true);

        // A centered spinner shown during the (multi-second) resolve, on top of the
        // controls so it reads as "loading this video". Dropped on the first frame.
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(48, 48);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);

        // "2×" speed badge (top-center), crossfaded in while hold-space 2× is active.
        let speed_badge = Revealer::new();
        speed_badge.set_transition_type(RevealerTransitionType::Crossfade);
        speed_badge.set_halign(Align::Center);
        speed_badge.set_valign(Align::Start);
        speed_badge.set_margin_top(12);
        let badge_label = gtk::Label::new(Some("2×  ▶▶"));
        badge_label.add_css_class("osd");
        badge_label.add_css_class("speed-badge");
        speed_badge.set_child(Some(&badge_label));
        speed_badge.set_reveal_child(false);

        // Corner controls (YouTube-miniplayer layout), each in a crossfade Revealer.
        let (exp_rev, mini_expand) = corner_button("view-fullscreen-symbolic", Align::Start, Align::Start);
        let (close_rev, mini_close) = corner_button("window-close-symbolic", Align::End, Align::Start);
        let (play_rev, mini_play) = corner_button("media-playback-pause-symbolic", Align::Center, Align::End);
        let mini_revs = [exp_rev, close_rev, play_rev];
        {
            let engine = engine.clone();
            let btn = mini_play.clone();
            mini_play.connect_clicked(move |_| {
                engine.toggle_pause();
                btn.set_icon_name(play_icon(engine.is_paused()));
            });
        }

        let root = Overlay::new();
        root.set_child(Some(&area));
        root.add_overlay(captions.widget());
        root.add_overlay(&revealer);
        // Scrub preview: floats above the transport and follows the hover x.
        root.add_overlay(controls.preview_widget());
        root.add_overlay(&spinner);
        root.add_overlay(&speed_badge);
        for r in &mini_revs {
            root.add_overlay(r);
        }
        // The overflow property clips the GL child to the rounded corners; CSS border-radius
        // alone does not.
        root.set_overflow(gtk::Overflow::Hidden);
        // Rounded video frame as on the web watch page. `App::set_fullscreen` removes the class
        // in fullscreen.
        root.add_css_class("video-frame");

        let generation = Rc::new(Cell::new(0u64));
        let mini = Rc::new(Cell::new(false));
        let mini_gen = Rc::new(Cell::new(0u64));

        // Clicks on the picture act like Space: a tap toggles pause, a hold plays at 2x
        // until release. The transport and corner controls are overlays above it.
        {
            let press = gtk::GestureClick::new();
            press.set_button(gtk::gdk::BUTTON_PRIMARY);
            let down = Rc::new(Cell::new(false));
            let held = Rc::new(Cell::new(false));
            let saved = Rc::new(Cell::new(1.0f64));
            let gen = Rc::new(Cell::new(0u64));
            {
                let (engine, down, held, saved, gen) =
                    (engine.clone(), down.clone(), held.clone(), saved.clone(), gen.clone());
                let (badge, area_f, mini) = (speed_badge.clone(), area.clone(), mini.clone());
                press.connect_pressed(move |_, _, _, _| {
                    if mini.get() {
                        return;
                    }
                    area_f.grab_focus();
                    down.set(true);
                    let g = gen.get().wrapping_add(1);
                    gen.set(g);
                    let (engine, down, held, saved, gen, badge) =
                        (engine.clone(), down.clone(), held.clone(), saved.clone(), gen.clone(), badge.clone());
                    glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                        if down.get() && gen.get() == g {
                            saved.set(engine.speed());
                            engine.set_speed(2.0);
                            if engine.is_paused() {
                                engine.toggle_pause();
                            }
                            held.set(true);
                            badge.set_reveal_child(true);
                        }
                    });
                });
            }
            let release = {
                let (engine, down, held, saved, gen, mini) =
                    (engine.clone(), down.clone(), held.clone(), saved.clone(), gen.clone(), mini.clone());
                let badge = speed_badge.clone();
                Rc::new(move |tap: bool| {
                    if !down.replace(false) {
                        return;
                    }
                    gen.set(gen.get().wrapping_add(1));
                    if held.replace(false) {
                        engine.set_speed(saved.get());
                        badge.set_reveal_child(false);
                    } else if tap && !mini.get() {
                        engine.toggle_pause();
                    }
                })
            };
            {
                let release = release.clone();
                press.connect_released(move |_, _, _, _| release(true));
            }
            press.connect_cancel(move |_, _| release(false));
            area.add_controller(press);
        }

        // Reveal + start the idle-hide timer each time the page is shown (area mapped),
        // unless we're the corner miniplayer.
        {
            let revealer = revealer.clone();
            let generation = generation.clone();
            let mini = mini.clone();
            area.connect_map(move |_| {
                if !mini.get() {
                    reveal_then_hide(&revealer, &generation);
                }
            });
        }

        // Pointer motion reveals controls: the full transport in normal mode, the corner
        // controls in mini mode. Dedup by position (hiding relayouts under a stationary
        // pointer, which GTK reports as motion at the same coords → flicker loop).
        let motion = gtk::EventControllerMotion::new();
        {
            let revealer = revealer.clone();
            let generation = generation.clone();
            let mini = mini.clone();
            let mini_gen = mini_gen.clone();
            let mini_revs = mini_revs.clone();
            let engine = engine.clone();
            let mini_play = mini_play.clone();
            let last = Rc::new(Cell::new((f64::NAN, f64::NAN)));
            motion.connect_motion(move |_, x, y| {
                if last.get() == (x, y) {
                    return;
                }
                last.set((x, y));
                if mini.get() {
                    mini_play.set_icon_name(play_icon(engine.is_paused()));
                    reveal_mini(&mini_revs, &mini_gen);
                } else {
                    reveal_then_hide(&revealer, &generation);
                }
            });
        }
        root.add_controller(motion);

        // Keep the corner play/pause icon in sync with the engine through the controls' poll, so
        // keyboard pause updates it without a re-hover.
        {
            let mini_play = mini_play.clone();
            controls.set_pause_mirror(move |paused| mini_play.set_icon_name(play_icon(paused)));
        }

        Self {
            root,
            controls,
            captions,
            spinner,
            speed_badge,
            revealer,
            mini_revs,
            mini_expand,
            mini_close,
            mini,
            mini_gen,
        }
    }

    /// Show/hide the centered loading spinner.
    pub fn set_loading(&self, loading: bool) {
        self.spinner.set_visible(loading);
        self.spinner.set_spinning(loading);
    }

    /// Show/hide the "2×" speed badge (hold-space fast-forward).
    pub fn set_speed_indicator(&self, on: bool) {
        self.speed_badge.set_reveal_child(on);
    }

    /// Switch chrome between the full transport (false) and the corner controls (true).
    pub fn set_mini(&self, mini: bool) {
        self.mini.set(mini);

        self.revealer.set_reveal_child(!mini);
        self.revealer.set_visible(!mini);
        // Corner controls start hidden (reveal on hover); bump the gen so any pending
        // reveal-timer can't flip them back on after we leave mini.
        self.mini_gen.set(self.mini_gen.get().wrapping_add(1));
        for r in &self.mini_revs {
            r.set_reveal_child(false);
        }
        if mini {
            self.root.add_css_class("miniplayer");
        } else {
            self.root.remove_css_class("miniplayer");
        }
    }

    /// The corner "expand to full" button.
    pub fn connect_mini_expand<F: Fn() + 'static>(&self, f: F) {
        self.mini_expand.connect_clicked(move |_| f());
    }

    /// The corner "close (stop)" button.
    pub fn connect_mini_close<F: Fn() + 'static>(&self, f: F) {
        self.mini_close.connect_clicked(move |_| f());
    }
}

/// A round OSD corner button wrapped in a crossfade `Revealer`, pinned to a corner/edge.
/// Returns the revealer (add as overlay + toggle) and the button (wire clicks).
fn corner_button(icon: &str, h: Align, v: Align) -> (Revealer, Button) {
    let b = Button::from_icon_name(icon);
    b.add_css_class("osd");
    b.add_css_class("circular");
    b.add_css_class("mini-ctl");
    let r = Revealer::new();
    r.set_transition_type(RevealerTransitionType::Crossfade);
    r.set_transition_duration(180);
    r.set_child(Some(&b));
    r.set_halign(h);
    r.set_valign(v);
    r.set_margin_start(8);
    r.set_margin_end(8);
    r.set_margin_top(8);
    r.set_margin_bottom(8);
    r.set_reveal_child(false);
    (r, b)
}
