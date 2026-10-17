//! The live-edge ("● LIVE") re-join and its test hooks.
use std::rc::Rc;

use adw::Toast;
use gtk4 as gtk;
use gtk::glib;

use crate::playback::{player, PlaybackError, Prepared};

use super::app::App;
use super::playback_error_message;

impl App {
    // Live-edge ("● LIVE") button: return to the current head. The engine tries an in-session
    // return first; otherwise `prepare_live` joins a fresh stream at the head (the join asks
    // for player_time_ms 2^53 - 1, SABR-009). The old stream keeps playing while the new one
    // prepares, and the swap happens on ready.
    pub(super) fn go_live_edge(&self) {
        let App { rt, resolve_gen, engine, player, toasts, current_live, current_quality, .. } = self;
        let Some(id) = current_live.borrow().clone() else { return };
        if engine.live_go_to_head() {
            return;
        }
        let quality = current_quality.get(); // re-join at the current cap
        player.set_loading(true);
        let token = resolve_gen.bump();
        let generation = resolve_gen.clone();
        let engine2 = engine.clone();
        let player2 = player.clone();
        let toasts2 = toasts.clone();
        rt.spawn(
            async move { player::prepare_live(&id, quality, None, Default::default()).await },
            move |prepared: Result<Prepared, PlaybackError>| {
                if !generation.is_current(token) {
                    return; // superseded by a newer click
                }
                match prepared {
                    Ok(prepared) => {
                        engine2.load(prepared); // swap to the fresh edge stream
                        player2.controls.set_live(true);
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

/// The controls' live-edge button and the live-dvr-test and live-edge-test hooks.
pub(super) fn wire_live_edge(app: &Rc<App>) {
    let App { engine, player, .. } = &**app;
    {
        let app = app.clone();
        player.controls.connect_live_edge(move || app.go_live_edge());
    }
    let (dvr_engine, dvr_app) = (engine.clone(), app.clone());
    crate::hooks::on("live-dvr-test", move |plan| {
        let (engine, app) = (&dvr_engine, &dvr_app);
        for item in plan.split(',') {
            let Some((at, step)) = item.split_once(':') else { continue };
            let Ok(at) = at.trim().parse::<u32>() else { continue };
            let (engine, app, step) = (engine.clone(), app.clone(), step.trim().to_string());
            glib::timeout_add_seconds_local_once(at, move || {
                tracing::info!("[dvr-test] step {step} at pos={:.1}", engine.time_pos());
                if step == "live" {
                    app.go_live_edge();
                } else if let Some(x) = step.strip_suffix('x').and_then(|v| v.parse::<f64>().ok()) {
                    engine.set_speed(x);
                } else if let Ok(delta) = step.parse::<f64>() {
                    engine.seek_relative(delta);
                }
            });
        }
    });
    let app = app.clone();
    crate::hooks::on("live-edge-test", move |v| {
        let Ok(secs) = v.parse::<u32>() else { return };
        let app = app.clone();
        glib::timeout_add_seconds_local_once(secs, move || {
            tracing::info!("[live-edge-test] pressing LIVE");
            app.go_live_edge();
        });
    });
}
