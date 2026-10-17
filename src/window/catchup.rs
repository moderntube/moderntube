//! "Catch me up", Ask and Transcribe: the live catch-up summary, the transcript Q&A panel
//! and the DVR seeks their anchors trigger.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::Toast;
use gtk4 as gtk;
use gtk::glib;

use crate::playback::{player, PlaybackError, Prepared};

use super::app::App;
use super::player_page::PlayerPage;
use super::transcript::{show_live_transcript, show_vod_transcript, transcribe_vod};
use super::playback_error_message;

/// Builds the catch-up and Ask sidebar pages and wires their triggers, anchors and hooks.
pub(super) fn wire_catchup(app: &Rc<App>, page: &PlayerPage) {
    let App {
        rt, prefs_store, resolve_gen, engine, player, toasts, sidebar, current_video, current_quality,
        current_live, ..
    } = &**app;
    let PlayerPage { catchup_strip, sidebar_column, .. } = page;
    // "Catch me up": summarize the missed DVR span.
    // Panel lives as a sidebar page; the trigger is the controls' MenuButton (live
    // only). Window bounds come from a 1s poll that records the join playhead (first
    // stream-absolute position after an open) and the last pause playhead.
    let catchup_panel = Rc::new(crate::widgets::CatchupPanel::new());
    sidebar.add_named(&catchup_panel.root, Some("catchup"));
    let ask_panel = crate::widgets::AskPanel::new();
    sidebar.add_named(&ask_panel.root, Some("ask"));
    // The live stream whose transcript the Ask panel holds.
    let ask_loaded_live: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));    {
        let sidebar = sidebar.clone();
        let current_live = current_live.clone();
        ask_panel.connect_close(move || {
            let live = current_live.borrow().is_some() && sidebar.child_by_name("chat").is_some();
            let back = if live { "chat" } else { "recommended" };
            sidebar.set_visible_child_name(back);
        });
    }
    {
        let rt = rt.clone();
        let engine = engine.clone();
        let weak = Rc::downgrade(&ask_panel);
        ask_panel.connect_ask(move |question| {
            let Some(panel) = weak.upgrade() else { return };
            let title = engine.watch_details().and_then(|d| d.title).unwrap_or_default();
            let cfg = crate::settings::read(|s| crate::catchup::SummarizerConfig::from_settings(&s.catchup));
            let (lines, history) = (panel.lines(), panel.history());
            let q = question.clone();
            let weak = Rc::downgrade(&panel);
            rt.spawn(
                async move { crate::transcript::ask(title, lines, history, q, cfg).await },
                move |res: Result<String, String>| {
                    match &res {
                        Ok(a) => tracing::info!("[ask] answered ({} chars)", a.len()),
                        Err(e) => tracing::warn!("[ask] failed: {e}"),
                    }
                    if let Some(panel) = weak.upgrade() {
                        panel.show_answer(&question, res);
                    }
                },
            );
        });
    }
    {
        let rt = rt.clone();
        let engine = engine.clone();
        let sidebar = sidebar.clone();
        let panel = ask_panel.clone();
        let loaded_for: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        // The question is asked once the transcript is in.
        let (ask_player, ask_panel_t) = (player.clone(), panel.clone());
        crate::hooks::on("ask-test", move |v| {
            let Some((secs, q)) = v.split_once(':').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.to_string())))
            else {
                return;
            };
            let player = ask_player.clone();
            let panel = ask_panel_t.clone();
            glib::timeout_add_seconds_local_once(secs, move || {
                tracing::info!("[ask-test] opening Ask");
                player.controls.click_ask();
                let mut pressed = false;
                glib::timeout_add_seconds_local(1, move || {
                    if panel.lines().is_empty() {
                        if !pressed && panel.click_transcribe() {
                            tracing::info!("[ask-test] pressed Transcribe");
                            pressed = true;
                        }
                        return glib::ControlFlow::Continue;
                    }
                    tracing::info!("[ask-test] asking: {q}");
                    panel.ask_now(&q);
                    glib::ControlFlow::Break
                });
            });
        });
        let current_live = current_live.clone();
        let current_video = current_video.clone();
        let loaded_live = ask_loaded_live.clone();
        let overlay = player.captions.clone();
        player.controls.connect_ask(move || {
            sidebar.set_visible_child_name("ask");
            if let Some(id) = current_live.borrow().clone() {
                loaded_for.set(0);
                if loaded_live.replace(Some(id.clone())).as_deref() != Some(id.as_str()) || panel.lines().is_empty() {
                    show_live_transcript(&panel, &id);
                }
                return;
            }
            loaded_live.replace(None);
            panel.set_transcribe(None);
            let Some(vod) = engine.caption_session() else {
                panel.reset("No video is playing.");
                loaded_for.set(0);
                return;
            };
            let key = Arc::as_ptr(&vod) as usize;
            if loaded_for.replace(key) == key {
                return;
            }
            let captions = vod.captions().clone();
            let lang = overlay.prefs().lang;
            let Some(index) = crate::widgets::captions::pick_track(captions.tracks(), &lang) else {
                match current_video.borrow().as_deref() {
                    Some(id) => show_vod_transcript(&panel, id),
                    None => panel.reset("This video has no captions, so there is no transcript yet."),
                }
                return;
            };
            let track = captions.tracks()[index].clone();
            let source = format!("{} captions{}", track.name, if track.auto { " (auto-generated)" } else { "" });
            panel.reset("Loading the transcript…");
            let panel = panel.clone();
            let loaded_for = loaded_for.clone();
            rt.spawn(async move { captions.fetch(index).await }, move |res| {
                if loaded_for.get() != key {
                    return;
                }
                match res {
                    Ok(cues) => panel.set_transcript(crate::transcript::from_captions(&cues), &source),
                    Err(e) => {
                        loaded_for.set(0);
                        panel.reset(&format!("Could not load captions: {e}"));
                    }
                }
            });
        });
    }
    let catchup_join: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
    let catchup_pause: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
    {
        let engine = engine.clone();
        let current_live = current_live.clone();
        let join = catchup_join.clone();
        let pause = catchup_pause.clone();
        let last_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let was_paused = Rc::new(Cell::new(false));
        glib::timeout_add_seconds_local(1, move || {
            let id = current_live.borrow().clone();
            if *last_id.borrow() != id {
                *last_id.borrow_mut() = id.clone();
                join.set(None);
                pause.set(None);
                was_paused.set(false);
            }
            if id.is_some() {
                if let Some(pt) = engine.live_player_time_ms() {
                    // Rounds before the first anchor report the live-edge request value
                    // 2^53 - 1 (SABR-009), which is not a stream position. Only an anchored
                    // playhead may seed the catch-up window bounds.
                    let plausible = pt > 0 && pt < 9_000_000_000_000;
                    if plausible && join.get().is_none() {
                        join.set(Some(pt));
                    }
                    let p = engine.is_paused();
                    if plausible && p && !was_paused.get() {
                        pause.set(Some(pt));
                    }
                    was_paused.set(p);
                }
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let sidebar = sidebar.clone();
        catchup_panel.connect_close(move || {
            sidebar.set_visible_child_name("chat");
        });
    }
    // [h:mm:ss] anchor → DVR seek: a fresh live open joined at that offset (the live
    // pipe can't rewind; same swap-on-ready flow as the "● LIVE" edge button).
    {
        let rt = rt.clone();
        let engine = engine.clone();
        let player_l = player.clone();
        let resolve_gen = resolve_gen.clone();
        let toasts = toasts.clone();
        let current_live = current_live.clone();
        let current_quality = current_quality.clone();
        let engine_dvr = engine.clone();
        let dvr_open: Rc<dyn Fn(i64)> = Rc::new(move |ms| {
            let Some(id) = current_live.borrow().clone() else { return };
            let quality = current_quality.get();
            player_l.set_loading(true);
            let token = resolve_gen.bump();
            let generation = resolve_gen.clone();
            let engine2 = engine.clone();
            let player2 = player_l.clone();
            let toasts2 = toasts.clone();
            rt.spawn(
                async move { player::prepare_live(&id, quality, Some(ms), Default::default()).await },
                move |prepared: Result<Prepared, PlaybackError>| {
                    if !generation.is_current(token) {
                        return;
                    }
                    match prepared {
                        Ok(prepared) => {
                            engine2.load(prepared);
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
        });
        {
            let open = dvr_open.clone();
            catchup_panel.connect_seek(move |ms| open(ms));
        }
        {
            let engine = engine_dvr.clone();
            ask_panel.connect_seek(move |ms| engine.seek_absolute(ms as f64 / 1000.0));
        }
        // The reopen is the one a catch-up anchor click does.
        crate::hooks::on("dvr-test", move |v| {
            let Some((secs, behind)) =
                v.split_once(':').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<i64>().ok()?)))
            else {
                return;
            };
            let (engine, open) = (engine_dvr.clone(), dvr_open.clone());
            glib::timeout_add_seconds_local_once(secs, move || match engine.live_head_ms() {
                Some(head) => {
                    tracing::info!("[dvr-test] opening {behind}s behind head {head}");
                    open(head - behind * 1000);
                }
                None => tracing::info!("[dvr-test] no live head known"),
            });
        });
    }
    // Transcribe (live): the capped DVR window up to the live edge, into the shared cache.
    {
        let rt = rt.clone();
        let engine = engine.clone();
        let toasts = toasts.clone();
        let current_live = current_live.clone();
        let join = catchup_join.clone();
        let strip = catchup_strip.clone();
        let sidebar_column = sidebar_column.clone();
        let prefs_store = prefs_store.clone();
        let loaded_live = ask_loaded_live.clone();
        let weak = Rc::downgrade(&ask_panel);
        let current_video = current_video.clone();
        ask_panel.connect_transcribe(move || {
            let Some(panel) = weak.upgrade() else { return };
            let Some(id) = current_live.borrow().clone() else {
                let (Some(id), Some(secs)) = (current_video.borrow().clone(), engine.duration()) else {
                    toasts.add_toast(Toast::new("The video length is not known yet — try in a few seconds"));
                    return;
                };
                transcribe_vod(&rt, &panel, &strip, &sidebar_column, id, (secs * 1000.0) as i64);
                return;
            };
            let Some(edge) = engine.live_head_ms().or(join.get()) else {
                toasts.add_toast(Toast::new("The stream position is not known yet — try in a few seconds"));
                return;
            };
            let cap_hours = cap_hours_override().unwrap_or_else(|| prefs_store.get().catchup_cap_hours);
            let from_ms = (edge - (cap_hours * 3_600_000.0) as i64).max(0);
            panel.set_transcribing(true);
            strip.begin("Fetching audio…");
            sidebar_column.set_visible(true);
            let (ptx, prx) = async_channel::unbounded::<crate::catchup::CatchupPhase>();
            {
                let strip = strip.clone();
                glib::spawn_future_local(async move {
                    while let Ok(phase) = prx.recv().await {
                        strip.show_phase(&phase);
                    }
                });
            }
            let strip = strip.clone();
            let loaded_live = loaded_live.clone();
            let vid = id.clone();
            tracing::info!("[transcribe] {id}: {}..{}", crate::catchup::fmt_ts(from_ms), crate::catchup::fmt_ts(edge));
            rt.spawn(
                async move { crate::catchup::transcribe_window(&vid, from_ms, Some(edge), &ptx).await },
                move |res| {
                    panel.set_transcribing(false);
                    match res {
                        Ok((segments, to_ms)) => {
                            tracing::info!("[transcribe] {id}: {} segments to {}", segments.len(), crate::catchup::fmt_ts(to_ms));
                            strip.finish();
                            if loaded_live.borrow().as_deref() == Some(id.as_str()) {
                                show_live_transcript(&panel, &id);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[transcribe] {id}: failed: {e}");
                            strip.show_error(&e);
                        }
                    }
                },
            );
        });
    }
    {
        let rt = rt.clone();
        let toasts = toasts.clone();
        let current_live = current_live.clone();
        let sidebar = sidebar.clone();
        let sidebar_column = sidebar_column.clone();
        let panel = catchup_panel.clone();
        let strip = catchup_strip.clone();
        let retry_strip = catchup_strip.clone();
        let join = catchup_join.clone();
        let pause = catchup_pause.clone();
        let catchup_prefs = prefs_store.clone();
        // Remember the last-run mode so the strip's Retry button re-runs the same catch-up (the
        // cache means no re-fetch and no re-transcribe).
        let last_mode =
            Rc::new(std::cell::Cell::new(crate::widgets::player_controls::CatchupMode::BeforeJoin));
        let last_mode_t = last_mode.clone();
        let trigger: Rc<dyn Fn(crate::widgets::player_controls::CatchupMode)> =
            Rc::new(move |mode| {
            last_mode_t.set(mode);
            let Some(id) = current_live.borrow().clone() else { return };
            use crate::widgets::player_controls::CatchupMode;
            // A 24/7 stream is otherwise unbounded before the join.
            let cap_hours = cap_hours_override().unwrap_or_else(|| catchup_prefs.get().catchup_cap_hours);
            let cap_ms: i64 = (cap_hours * 3_600_000.0) as i64;
            // Compute the [from, to] window per mode:
            // - Joined  → what happened BEFORE I tuned in: [join - cap, join].
            // - Paused  → the gap I skipped: [pause position, current live edge].
            let (from_ms, to_ms) = match mode {
                CatchupMode::BeforeJoin => match join.get() {
                    Some(j) => ((j - cap_ms).max(0), Some(j)),
                    None => {
                        toasts.add_toast(Toast::new(
                            "Catch-up window unknown yet — try in a few seconds",
                        ));
                        return;
                    }
                },
                CatchupMode::WhilePaused => match pause.get().or(join.get()) {
                    Some(p) => (p, None), // None = resolve to the current live edge
                    None => {
                        toasts.add_toast(Toast::new(
                            "Catch-up window unknown yet — try in a few seconds",
                        ));
                        return;
                    }
                },
            };
            // The run shows in the strip and the sidebar stack is left alone, so chat (or
            // whatever page is up) stays on screen. `sidebar_column` is shown directly because
            // the stack may be toggled off and the strip must still appear.
            panel.reset();
            if sidebar.visible_child_name().as_deref() == Some("catchup")
                && sidebar.child_by_name("chat").is_some()
            {
                // A previous run's summary is up; it isn't this run's result.
                sidebar.set_visible_child_name("chat");
            }
            strip.begin("Fetching audio…");
            sidebar_column.set_visible(true);
            let (ptx, prx) = async_channel::unbounded::<crate::catchup::CatchupPhase>();
            {
                let strip = strip.clone();
                glib::spawn_future_local(async move {
                    while let Ok(phase) = prx.recv().await {
                        strip.show_phase(&phase);
                    }
                });
            }
            let panel2 = panel.clone();
            let strip2 = strip.clone();
            let sidebar2 = sidebar.clone();
            let summarizer = crate::settings::read(|s| crate::catchup::SummarizerConfig::from_settings(&s.catchup));
            rt.spawn(
                async move { crate::catchup::run_catchup(id, from_ms, to_ms, ptx, summarizer).await },
                move |res: Result<crate::catchup::CatchupResult, String>| match res {
                    // Ready: the result panel takes the sidebar.
                    Ok(r) => {
                        strip2.finish();
                        panel2.show_result(&r);
                        sidebar2.set_visible(true);
                        sidebar2.set_visible_child_name("catchup");
                    }
                    // A failure stays in the strip, expanded and with Retry, and does not take
                    // over the sidebar.
                    Err(e) => strip2.show_error(&e),
                },
            );
        });
        {
            let t = trigger.clone();
            player.controls.connect_catchup(move |mode| t(mode));
        }
        {
            let t = trigger.clone();
            retry_strip.connect_retry(move || t(last_mode.get()));
        }
        // The hook fires the closure the controls' menu dispatches.
        crate::hooks::on("catchup-test", move |spec| {
            if let Some((secs, mode)) = spec.split_once(':') {
                let mode = if mode == "paused" {
                    crate::widgets::player_controls::CatchupMode::WhilePaused
                } else {
                    crate::widgets::player_controls::CatchupMode::BeforeJoin
                };
                let secs: u32 = secs.parse().unwrap_or(20);
                let t = trigger.clone();
                glib::timeout_add_seconds_local_once(secs, move || t(mode));
            }
        });
    }
}

fn cap_hours_override() -> Option<f64> {
    crate::hooks::get("catchup-cap-hours").and_then(|s| s.parse::<f64>().ok()).filter(|h| *h > 0.0)
}
