//! The window's keyboard shortcuts.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::ApplicationWindow;
use gtk4 as gtk;

use super::app::App;
use super::PlayerMode;

/// Space (tap to pause, hold for 2×), "/" to search, arrows, Escape, c and m.
pub(super) fn wire_shortcuts(app: &Rc<App>, window: &ApplicationWindow) {
    let App { engine, player, stack, search, player_mode, .. } = &**app;
    let shorts_view = app.shorts_view.get().expect("shorts page built");
    // Capture phase so space and arrows act on playback before a focused widget (e.g. the back
    // button) consumes them.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    // Space is tap or hold: a tap toggles pause, holding engages 2× until release. The
    // press and release handlers share this state.
    let space_down = Rc::new(Cell::new(false));
    let space_2x = Rc::new(Cell::new(false));
    let space_saved_speed = Rc::new(Cell::new(1.0f64));
    let space_gen = Rc::new(Cell::new(0u64));
    // The press went to the Shorts player, so the release does too.
    let space_on_shorts = Rc::new(Cell::new(false));
    let typing_in_field: Rc<dyn Fn() -> bool> = {
        let window_f = window.clone();
        Rc::new(move || {
            gtk::prelude::GtkWindowExt::focus(&window_f).is_some_and(|f| {
                f.is::<gtk::Text>() || f.is::<gtk::Editable>() || f.is::<gtk::Entry>()
            })
        })
    };
    {
        let engine = engine.clone();
        let stack_w = stack.clone();
        let app_f = app.clone();
        let fs_state = app.fs_state.clone();
        let player_mode = player_mode.clone();
        let space_down = space_down.clone();
        let space_2x = space_2x.clone();
        let space_saved_speed = space_saved_speed.clone();
        let space_gen = space_gen.clone();
        let space_on_shorts = space_on_shorts.clone();
        let typing = typing_in_field.clone();
        let search_key = search.clone();
        let player_k = player.clone();
        let app_k = app.clone();
        let app_e = app.clone();
        let shorts_k = shorts_view.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            // Don't steal keys while a text field is focused (chat compose, search).
            if typing() {
                return gtk::glib::Propagation::Proceed;
            }
            use gtk::gdk::Key;
            // `/` focuses search from anywhere. A fullscreen player drops to the corner PiP first
            // so it does not cover the results. The focus grab is deferred to idle so this
            // keystroke does not reach the search entry.
            if key == Key::slash {
                if fs_state.get() {
                    app_f.set_fullscreen(false);
                }
                if player_mode.get() == PlayerMode::Full {
                    app_k.show_mini();
                }
                stack_w.set_visible_child_name("results");
                let search_key = search_key.clone();
                gtk::glib::idle_add_local_once(move || {
                    search_key.grab_focus();
                });
                return gtk::glib::Propagation::Stop;
            }
            // Space: the pause decision waits for release. The first press starts a hold timer;
            // if still held after 180 ms, engage 2× without pausing. `space_down` guards against
            // key auto-repeat.
            if key == Key::space {
                let on_shorts = stack_w.visible_child_name().as_deref() == Some("shorts");
                if !on_shorts && player_mode.get() == PlayerMode::Hidden {
                    return gtk::glib::Propagation::Proceed;
                }
                if space_down.replace(true) {
                    return gtk::glib::Propagation::Stop; // auto-repeat
                }
                space_on_shorts.set(on_shorts);
                let g = space_gen.get().wrapping_add(1);
                space_gen.set(g);
                let engine = engine.clone();
                let space_down = space_down.clone();
                let space_2x = space_2x.clone();
                let space_saved_speed = space_saved_speed.clone();
                let space_gen = space_gen.clone();
                let player_k = player_k.clone();
                let shorts_k = shorts_k.clone();
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                    if !(space_down.get() && space_gen.get() == g) {
                        return;
                    }
                    if on_shorts {
                        shorts_k.hold_2x(true);
                        space_2x.set(true);
                    } else {
                        space_saved_speed.set(engine.speed());
                        engine.set_speed(2.0);
                        // Holding while paused resumes at 2×; release restores the speed and
                        // leaves playback running.
                        if engine.is_paused() {
                            engine.toggle_pause();
                        }
                        space_2x.set(true);
                        player_k.set_speed_indicator(true);
                    }
                });
                return gtk::glib::Propagation::Stop;
            }
            // The rest (seek / volume / fullscreen-escape) are player-page only.
            if stack_w.visible_child_name().as_deref() != Some("player") {
                return gtk::glib::Propagation::Proceed;
            }
            match key {
                // Right on live jumps to the live head, like the LIVE button.
                Key::Right if engine.is_live() => app_e.go_live_edge(),
                Key::Left => engine.seek_relative(-5.0),
                Key::Right => engine.seek_relative(5.0),
                Key::Up => engine.set_volume(engine.volume() + 5.0),
                Key::Down => engine.set_volume(engine.volume() - 5.0),
                Key::Escape if fs_state.get() => app_f.set_fullscreen(false),
                Key::c | Key::C => player_k.captions.toggle(),
                Key::m | Key::M => player_k.controls.toggle_mute(),
                _ => return gtk::glib::Propagation::Proceed,
            }
            gtk::glib::Propagation::Stop
        });
    }
    {
        let engine = engine.clone();
        let player_k = player.clone();
        let shorts_k = shorts_view.clone();
        keys.connect_key_released(move |_, key, _, _| {
            if key != gtk::gdk::Key::space || !space_down.replace(false) {
                return;
            }
            space_gen.set(space_gen.get().wrapping_add(1)); // cancel a pending hold-timer
            let held = space_2x.replace(false);
            if space_on_shorts.get() {
                if held {
                    shorts_k.hold_2x(false);
                } else {
                    shorts_k.toggle_pause();
                }
            } else if held {
                engine.set_speed(space_saved_speed.get()); // was a hold → restore speed
                player_k.set_speed_indicator(false);
            } else {
                engine.toggle_pause(); // was a tap → pause/resume
            }
        });
    }
    window.add_controller(keys);
}
