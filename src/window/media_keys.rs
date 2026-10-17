//! MPRIS: media keys, the lock screen and playerctl drive the player and the queue.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::ApplicationWindow;
use gtk4 as gtk;
use gtk::Stack;

use super::app::App;
use super::PlayerMode;

/// Routes MPRIS commands to the player, the queue or the Shorts page, and keeps the
/// published status and navigation current.
pub(super) fn wire_mpris(app: &Rc<App>, window: &ApplicationWindow) {
    let App { engine, stack, queue, mpris, last_page, player_mode, .. } = &**app;
    let shorts_view = app.shorts_view.get().expect("shorts page built");
    // Route media-key, lockscreen and playerctl commands to the engine and the queue.
    {
        use crate::mpris::MprisCommand;
        let engine_c = engine.clone();
        let mpris_c = mpris.clone();
        let queue_c = queue.clone();
        let app_c = app.clone();
        let player_mode_c = player_mode.clone();
        let stack_c = stack.clone();
        let last_page_c = last_page.clone();
        let window_c = window.clone();
        let leave_full = move |stack: &Stack| {
            if player_mode_c.get() == PlayerMode::Full {
                stack.set_visible_child_name(&last_page_c.borrow());
            }
        };
        let shorts_c = shorts_view.clone();
        let stack_s = stack.clone();
        mpris.on_command(Rc::new(move |cmd| {
            if stack_s.visible_child_name().as_deref() == Some("shorts") {
                match cmd {
                    MprisCommand::Play => shorts_c.set_paused(false),
                    MprisCommand::Pause => shorts_c.set_paused(true),
                    MprisCommand::PlayPause => shorts_c.toggle_pause(),
                    MprisCommand::Next => shorts_c.step(1),
                    MprisCommand::Previous => shorts_c.step(-1),
                    MprisCommand::Raise => window_c.present(),
                    MprisCommand::Quit => window_c.close(),
                    MprisCommand::Stop => shorts_c.set_paused(true),
                }
                mpris_c.set_status(if shorts_c.is_paused() { "Paused" } else { "Playing" });
                return;
            }
            match cmd {
            MprisCommand::Play => {
                engine_c.set_paused(false);
                mpris_c.set_status("Playing");
            }
            MprisCommand::Pause => {
                engine_c.set_paused(true);
                mpris_c.set_status("Paused");
            }
            MprisCommand::PlayPause => {
                engine_c.toggle_pause();
                mpris_c.set_status(if engine_c.is_paused() { "Paused" } else { "Playing" });
            }
            MprisCommand::Stop => {
                leave_full(&stack_c);
                app_c.hide_player();
                mpris_c.set_status("Stopped");
            }
            MprisCommand::Next => {
                // Bind before matching: a RefMut in a match scrutinee lives for the whole match,
                // and the None arm reaches hide_player, which borrows the queue again to clear
                // it. Auto-advance binds the same way.
                let next = queue_c.borrow_mut().advance();
                match next {
                    Some(item) => app_c.play_in_mode(item),
                    None => {
                        leave_full(&stack_c);
                        app_c.hide_player();
                        mpris_c.set_status("Stopped");
                    }
                }
            }
            MprisCommand::Previous => engine_c.seek_absolute(0.0),
            MprisCommand::Raise => window_c.present(),
            MprisCommand::Quit => window_c.close(),
            }
        }));
    }
    // Periodic status/nav sync to MPRIS (the handle dedupes, so this only emits on change).
    {
        let engine_p = engine.clone();
        let mpris_p = mpris.clone();
        let queue_p = queue.clone();
        let player_mode_p = player_mode.clone();
        let last: Rc<Cell<(&'static str, bool, bool)>> = Rc::new(Cell::new(("Stopped", false, false)));
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let status = if player_mode_p.get() == PlayerMode::Hidden {
                "Stopped"
            } else if engine_p.is_paused() {
                "Paused"
            } else {
                "Playing"
            };
            let can_next = queue_p.borrow().items().len() > 1;
            let can_prev = player_mode_p.get() != PlayerMode::Hidden;
            let cur = (status, can_next, can_prev);
            if cur != last.get() {
                last.set(cur);
                mpris_p.set_status(status);
                mpris_p.set_nav(can_next, can_prev);
            }
            gtk::glib::ControlFlow::Continue
        });
    }
}
