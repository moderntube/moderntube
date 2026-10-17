//! The stage: one video surface positioned over the player page's slot, in the corner
//! miniplayer, or fullscreen, plus the miniplayer's drag, scroll and resize gestures.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::Toast;
use gtk4 as gtk;
use gtk::glib;
use gtk::GestureDrag;

use crate::playback::{player, PlaybackError, Prepared};

use super::app::App;
use super::{first_scrolled_window, pip_quality, playback_error_message, PlayerMode};

/// Positions the video and hover-preview surfaces on the stage and installs the
/// miniplayer's drag, wheel and resize handling.
pub(super) fn wire_stage(app: &Rc<App>) {
    let App {
        stack,
        player,
        video_slot,
        player_mode,
        video_fullscreen,
        mini_offset,
        mini_size,
        hover_preview,
        hover_surface,
        stage,
        ..
    } = &**app;
    {
        let video_slot = video_slot.clone();
        let mode = player_mode.clone();
        let mini_offset = mini_offset.clone();
        let mini_size = mini_size.clone();
        let hover_surface = hover_surface.clone();
        let hover_preview_pos = hover_preview.clone();
        let video_fullscreen = video_fullscreen.clone();
        stage.connect_get_child_position(move |stage, child| {
            // The hover preview positions itself here from its card's current bounds, so it
            // follows the card through a window resize.
            if child == &hover_surface {
                return hover_preview_pos.child_position(stage.upcast_ref());
            }
            match mode.get() {
            // Fullscreen covers the stage; otherwise the surface covers the video slot.
            PlayerMode::Full if video_fullscreen.get() => {
                Some(gtk::gdk::Rectangle::new(0, 0, stage.width(), stage.height()))
            }
            PlayerMode::Full => {
                let b = video_slot.compute_bounds(stage)?;
                Some(gtk::gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                ))
            }
            // Bottom-right corner (16px inset) plus any drag offset, clamped on-screen.
            PlayerMode::Mini => {
                let (w, h) = mini_size.get();
                let m = 16;
                let (ox, oy) = mini_offset.get();
                let x = (stage.width() - w - m + ox).clamp(0, (stage.width() - w).max(0));
                let y = (stage.height() - h - m + oy).clamp(0, (stage.height() - h).max(0));
                Some(gtk::gdk::Rectangle::new(x, y, w, h))
            }
            PlayerMode::Hidden => None,
            }
        });
    }
    // Drag the corner miniplayer (mini mode only). The gesture lives on the stage overlay:
    // `player.root` is the widget being moved, so its own reference frame would shift each
    // frame and feed back into jitter. The claim waits for real movement so a stationary click
    // still reaches the corner buttons.
    {
        let drag = GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let mode = player_mode.clone();
        let offset = mini_offset.clone();
        let stage_d = stage.clone();
        let player_root = player.root.clone();
        let start = Rc::new(Cell::new((0i32, 0i32)));
        let active = Rc::new(Cell::new(false));
        let claimed = Rc::new(Cell::new(false));
        // Throttle the re-allocate to about 60 fps; a motion-event flood would queue one
        // relayout per event.
        let move_pending = Rc::new(Cell::new(false));
        {
            let mode = mode.clone();
            let offset = offset.clone();
            let start = start.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            let player_root = player_root.clone();
            let stage_d = stage_d.clone();
            drag.connect_drag_begin(move |_, sx, sy| {
                claimed.set(false);
                // Active only in mini mode, when the press began inside the corner player and
                // outside the top-left resize corner. The start point is in stage coordinates.
                let bounds = player_root.compute_bounds(&stage_d);
                let pt = gtk::graphene::Point::new(sx as f32, sy as f32);
                let on_player = mode.get() == PlayerMode::Mini
                    && bounds.map(|r| r.contains_point(&pt)).unwrap_or(false);
                let in_resize_corner = bounds
                    .map(|r| (sx as f32 - r.x()) < 22.0 && (sy as f32 - r.y()) < 22.0)
                    .unwrap_or(false);
                active.set(on_player && !in_resize_corner);
                if on_player {
                    start.set(offset.get());
                }
            });
        }
        {
            let offset = offset.clone();
            let start = start.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            let stage_d = stage_d.clone();
            let move_pending = move_pending.clone();
            drag.connect_drag_update(move |g, dx, dy| {
                if !active.get() {
                    return;
                }
                if !claimed.get() {
                    if dx.abs() + dy.abs() <= 4.0 {
                        return; // below threshold: leave the click sequence for the buttons
                    }
                    claimed.set(true);
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
                let (bx, by) = start.get();
                offset.set((bx + dx as i32, by + dy as i32)); // cheap; current for get_child_position
                if !move_pending.replace(true) {
                    let stage_t = stage_d.clone();
                    let pending = move_pending.clone();
                    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                        stage_t.queue_allocate();
                        pending.set(false);
                    });
                }
            });
        }
        {
            let active = active.clone();
            drag.connect_drag_end(move |_, _, _| active.set(false));
        }
        stage.add_controller(drag);
    }
    // A wheel over the corner miniplayer scrolls the page the user is browsing under it.
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let mode = player_mode.clone();
        let stack = stack.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            if mode.get() != PlayerMode::Mini {
                return glib::Propagation::Proceed;
            }
            let Some(sw) = stack.visible_child().and_then(|w| first_scrolled_window(&w)) else {
                return glib::Propagation::Proceed;
            };
            let adj = sw.vadjustment();
            let max = (adj.upper() - adj.page_size()).max(adj.lower());
            adj.set_value((adj.value() + dy * adj.step_increment()).clamp(adj.lower(), max));
            glib::Propagation::Stop
        });
        player.root.add_controller(scroll);
    }
    // Resize the corner miniplayer by dragging its top-left corner; the bottom-right stays
    // anchored. Keeps 16:9, clamps the width to 240 px minimum and 70% of the stage, and
    // persists the size.
    {
        let drag = GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let mode = player_mode.clone();
        let size = mini_size.clone();
        let stage_d = stage.clone();
        let player_root = player.root.clone();
        let base = Rc::new(Cell::new((0i32, 0i32)));
        let active = Rc::new(Cell::new(false));
        let claimed = Rc::new(Cell::new(false));
        // Throttle re-allocation to about 60 fps: each one resizes the video surface, and
        // per-event allocation starved the main thread.
        let resize_pending = Rc::new(Cell::new(false));
        {
            let mode = mode.clone();
            let size = size.clone();
            let base = base.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            let player_root = player_root.clone();
            let stage_d = stage_d.clone();
            drag.connect_drag_begin(move |_, sx, sy| {
                claimed.set(false);
                let in_corner = mode.get() == PlayerMode::Mini
                    && player_root
                        .compute_bounds(&stage_d)
                        .map(|r| (sx as f32 - r.x()) < 22.0 && (sy as f32 - r.y()) < 22.0)
                        .unwrap_or(false);
                active.set(in_corner);
                if in_corner {
                    base.set(size.get());
                }
            });
        }
        {
            let size = size.clone();
            let base = base.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            let stage_d = stage_d.clone();
            let resize_pending = resize_pending.clone();
            drag.connect_drag_update(move |g, dx, dy| {
                if !active.get() {
                    return;
                }
                if !claimed.get() {
                    if dx.abs() + dy.abs() <= 4.0 {
                        return;
                    }
                    claimed.set(true);
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
                // Dragging up-left (dx,dy negative) enlarges; width drives the 16:9 height.
                let (bw, _bh) = base.get();
                let max_w = ((stage_d.width() as f64) * 0.7) as i32;
                let new_w = (bw - dx as i32).clamp(240, max_w.max(240));
                let new_h = new_w * 9 / 16;
                size.set((new_w, new_h)); // cheap; always current for get_child_position
                // Coalesce motion events to about 60 fps.
                if !resize_pending.replace(true) {
                    let stage_t = stage_d.clone();
                    let pending = resize_pending.clone();
                    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                        stage_t.queue_allocate();
                        pending.set(false);
                    });
                }
            });
        }
        {
            let active = active.clone();
            let size = size.clone();
            let stage_d = stage_d.clone();
            drag.connect_drag_end(move |_, _, _| {
                if active.replace(false) {
                    stage_d.queue_allocate(); // final exact size
                    let (w, h) = size.get();
                    crate::prefs::save_pip_size(w, h); // persist the new size
                }
            });
        }
        stage.add_controller(drag);
    }
    // Resize affordance: show a diagonal-resize cursor over the PiP's top-left corner.
    {
        let motion = gtk::EventControllerMotion::new();
        let mode = player_mode.clone();
        let player_root = player.root.clone();
        motion.connect_motion(move |_, x, y| {
            let corner = mode.get() == PlayerMode::Mini && x < 22.0 && y < 22.0;
            player_root.set_cursor_from_name(if corner { Some("nw-resize") } else { None });
        });
        player.root.add_controller(motion);
    }
}

impl App {
    pub(super) fn show_full(&self) {
        let App { player, stack, stage, player_mode: mode, engine, current_quality, .. } = self;
        mode.set(PlayerMode::Full);
        player.set_mini(false);
        player.root.set_visible(true);
        stack.set_visible_child_name("player");
        stage.queue_allocate(); // re-run get-child-position for the new mode
        // Expanding restores the user's cap after the corner downshift.
        engine.pip_exit(current_quality.get());
    }

    pub(super) fn show_mini(&self) {
        let App {
            player, stack, stage, last_page, player_mode: mode, engine, current_quality, pip_reduced, ..
        } = self;
        mode.set(PlayerMode::Mini);
        player.set_mini(true);
        player.root.set_visible(true);
        stack.set_visible_child_name(&last_page.borrow());
        stage.queue_allocate();
        // Drop to the PiP cap unless play_mini already loaded the video at it.
        if !pip_reduced.get() {
            engine.pip_enter(pip_quality(current_quality.get()));
        }
    }

    pub(super) fn hide_player(&self) {
        let App { player, engine, player_mode: mode, queue, current_video, .. } = self;
        // Save the VOD's resume position before stopping. `current_video` is None on live.
        if let Some(id) = current_video.borrow().clone() {
            let pos = engine.time_pos();
            let near_end = engine.duration().map(|d| pos > d - 10.0).unwrap_or(false);
            if near_end {
                self.clear_position(&id);
            } else if pos > 3.0 {
                self.record_position(&id, pos);
            }
        }
        engine.stop();
        mode.set(PlayerMode::Hidden);
        player.root.set_visible(false);
        // Closing the player drops the queue so a leftover queue does not play behind the next
        // video. With an empty queue (auto-advance) this does nothing.
        queue.borrow_mut().clear();
    }
}

/// Hides the surface until a video loads and wires the corner miniplayer's expand and
/// close buttons.
pub(super) fn wire_mini_buttons(app: &Rc<App>) {
    let App { rt, resolve_gen, engine, player, toasts, current_video, current_quality, pip_reduced, .. } =
        &**app;
    player.root.set_visible(false); // nothing playing yet
    {
        let app_c = app.clone();
        let pip_reduced = pip_reduced.clone();
        let rt = rt.clone();
        let engine = engine.clone();
        let player_e = player.clone();
        let resolve_gen = resolve_gen.clone();
        let toasts = toasts.clone();
        let current_video = current_video.clone();
        let current_quality = current_quality.clone();
        player.connect_mini_expand(move || {
            app_c.show_full();
            // A video loaded at the reduced PiP cap reopens at the user's cap, resuming at the
            // current position. VOD only.
            if pip_reduced.replace(false) {
                if let Some(video_id) = current_video.borrow().clone() {
                    let q = current_quality.get();
                    player_e.set_loading(true);
                    let token = resolve_gen.bump();
                    let generation = resolve_gen.clone();
                    let engine2 = engine.clone();
                    let player2 = player_e.clone();
                    let toasts2 = toasts.clone();
                    rt.spawn(
                        async move { player::prepare_vod(&video_id, q, None, Default::default()).await },
                        move |prepared: Result<Prepared, PlaybackError>| {
                            if !generation.is_current(token) {
                                return;
                            }
                            match prepared {
                                Ok(prepared) => {
                                    let pos = engine2.time_pos();
                                    engine2.load_resume(prepared, pos);
                                    engine2.queue_render();
                                }
                                Err(e) => {
                                    player2.set_loading(false);
                                    toasts2.add_toast(Toast::new(&playback_error_message(&e)));
                                }
                            }
                        },
                    );
                }
            }
        });
    }
    {
        let app = app.clone();
        player.connect_mini_close(move || app.hide_player());
    }
}
