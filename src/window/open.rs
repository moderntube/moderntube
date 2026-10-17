use std::rc::Rc;

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;

use innertube::{Navigation, PotError, Quality, StreamError, StreamErrorKind};
use crate::models::ResultItem;
use crate::playback::{failure_class, player, stopped_message, PlaybackError, Prepared};
use crate::runtime::AsyncRuntime;
use crate::widgets::{ChatMode, QueuePanel};

use super::app::App;
use super::chat::{start_live_chat, start_replay_chat};
use super::watch::{start_comments, start_watch_meta};
use super::{pip_quality, watch_url, PlayerMode, LIVE_QUALITY_LADDER, YOUTUBE_HOME};

pub(super) fn on_result_activated(
    app: &Rc<App>,
    resume_at: Option<f64>,
    quality_override: Option<Quality>,
    referer: String,
    item: ResultItem,
) {
    let recommended = app.recommended.get().expect("recommended grid built");
    let App {
        rt,
        session,
        engine,
        stack,
        player,
        resolve_gen,
        toasts,
        current_video,
        current_quality,
        current_live,
        chat,
        chat_gen,
        chat_toggle,
        comments,
        comments_gen,
        comments_continuation,
        watch_info,
        sidebar,
        chat_mode,
        current_is_live,
        player_mode,
        ..
    } = &**app;
    // Instant feedback: stop the old video, blank the controls, show a spinner and switch to
    // the player page. The open below takes seconds (n-sig solve, PO token, SABR open). The
    // spinner clears on the first frame (engine on-loaded hook).
    engine.stop();
    player.controls.reset();
    player.set_loading(true);
    stack.set_visible_child_name("player");

    let video_id = item.video_id();
    let token = resolve_gen.bump();
    let app = app.clone();
    let rt = rt.clone();
    let engine = engine.clone();
    let stack = stack.clone();
    let player = player.clone();
    let generation = resolve_gen.clone();
    let toasts = toasts.clone();
    let current_video = current_video.clone();
    let current_quality = current_quality.clone();
    let current_live = current_live.clone();
    // Live-chat: a fresh generation token (new video → new chat) + a Session snapshot
    // and an id/rt for the poll loop, all consumed by the live branch of the load cb.
    let chat = chat.clone();
    let chat_toggle = chat_toggle.clone();
    let chat_token = chat_gen.bump();
    let chat_gen = chat_gen.clone();
    let chat_session = session.borrow().clone();
    let rt_for_chat = rt.clone();
    let vid_for_chat = video_id.clone();
    // Comments (VOD only): a fresh generation token (new video → new thread) + a Session
    // snapshot, an id, and an rt for the first-page fetch in the VOD branch below.
    let comments = comments.clone();
    let comments_continuation = comments_continuation.clone();
    let comments_token = comments_gen.bump();
    let comments_gen = comments_gen.clone();
    let comments_session = session.borrow().clone();
    let rt_for_comments = rt.clone();
    let vid_for_comments = video_id.clone();
    // Watch metadata and recommended: share the comments generation and token, so a superseded
    // click drops the in-flight fetch too. Own Session snapshot, rt and id for the fetch.
    let watch_info = watch_info.clone();
    let recommended = recommended.clone();
    let meta_session = session.borrow().clone();
    let rt_for_meta = rt.clone();
    let vid_for_meta = video_id.clone();
    let sidebar = sidebar.clone();
    let chat_mode = chat_mode.clone();
    let current_is_live = current_is_live.clone();
    let player_mode = player_mode.clone();
    // Apply the current quality cap (saved preference, carried across videos) to VOD and live.
    // A `quality_override` (a video started directly in the corner PiP) wins.
    let quality = quality_override.unwrap_or_else(|| current_quality.get());
    tracing::info!("[quality] effective={quality:?} (pip_override={quality_override:?})");

    let engine2 = engine.clone();
    let stack2 = stack.clone();
    let player2 = player.clone();
    let player_mode2 = player_mode.clone();
    let toasts2 = toasts.clone();
    let generation2 = generation.clone();
    let rt_auth = rt.clone();
    let vid = video_id.clone();
    let vid_for_ctx = video_id.clone();
    // Tear the outgoing session down before the new one warms up: a second concurrent session
    // gets no-media rounds (SABR-056). Only when the video changes; a same-video re-open (PiP
    // expand, quality cap change) keeps playing while the replacement prepares.
    if current_video.borrow().as_deref() != Some(video_id.as_str()) {
        engine.cancel_stream();
    }
    let nav = Navigation { referer: Some(referer), ..Default::default() };
    let fut = async move { player::prepare(&vid, quality, resume_at.filter(|p| *p > 0.0), nav).await };
    rt.spawn(fut, move |prepared: Result<Prepared, PlaybackError>| {
        // A newer click superseded this one and owns the UI now.
        if !generation2.is_current(token) {
            return;
        }
        match prepared {
            Ok(prepared) => {
                let is_live = prepared.is_live();
                // Seek-preview storyboard (VOD only; None for live → hover preview disabled).
                let storyboard = prepared.storyboard();
                // Resume at the saved position for a returning VOD; live and first-time plays
                // start at the beginning or the edge.
                match (is_live, resume_at) {
                    (false, Some(pos)) if pos > 0.0 => engine2.load_resume(prepared, pos),
                    _ => engine2.load(prepared),
                }
                player2.controls.set_live(is_live);
                player2.controls.set_storyboard(storyboard); // VOD seek-preview thumbs (None for live)
                if is_live {
                    *current_video.borrow_mut() = None; // VOD-context id; live uses current_live
                    *current_live.borrow_mut() = Some(vid_for_ctx.clone()); // for the LIVE button + quality switch
                    current_quality.set(quality);
                    // Quality menu for live: a fixed rung ladder. The engine caps downshift-only
                    // (highest available at or below the cap); live ABR serves what fits and the
                    // cap is the user's ceiling.
                    player2.controls.set_qualities(LIVE_QUALITY_LADDER, &quality);
                    current_is_live.set(true);
                    if let Some(spec) = crate::hooks::get("live-switch-test") {
                        for part in spec.split(',') {
                            let Some((at, h)) = part.split_once(':') else { continue };
                            let (Ok(at), Ok(h)) = (at.trim().parse::<u32>(), h.trim().parse::<i32>()) else { continue };
                            let eng = engine2.clone();
                            gtk::glib::timeout_add_seconds_local_once(at, move || {
                                tracing::info!("[test] live switch → {h}p");
                                eng.set_live_quality(h);
                            });
                        }
                    }
                    // Live: show the info block (title, channel, subs, description) below the
                    // video as for a VOD, without comments; the right column belongs to chat.
                    // `start_watch_meta` below fills it.
                    watch_info.root.set_visible(true);
                    watch_info.clear();
                    comments.root.set_visible(false);
                    // Live chat: reveal the toggle, place the panel per the current
                    // mode (docked/overlay/floating), gate composing, start the poll.
                    chat.clear();
                    chat_toggle.set_tooltip_text(Some("Live chat"));
                    chat_toggle.set_visible(true);
                    chat_toggle.set_active(true);
                    app.place_chat(chat_mode.get()); // positions chat + sets visibility
                    chat.set_replay(false); // live: show the compose row
                    chat.set_can_send(chat_session.is_authenticated());
                    start_live_chat(rt_for_chat, chat_session, chat, vid_for_chat, chat_gen, chat_token);
                    // Populate the info block for live too (title/channel/description);
                    // `recommended` is set but not shown (the right column is chat).
                    start_watch_meta(
                        rt_for_meta, meta_session, watch_info, recommended,
                        vid_for_meta, comments_gen, comments_token,
                        engine2.watch_details(),
                    );
                } else {
                    *current_video.borrow_mut() = Some(vid_for_ctx.clone());
                    *current_live.borrow_mut() = None;
                    current_is_live.set(false);
                    // If chat is floating/overlaid over the video, hide it for a VOD.
                    if chat_mode.get() != ChatMode::Docked {
                        chat.root.set_visible(false);
                    }
                    // Left column: reveal the metadata block and the comment thread; both scroll
                    // under the pinned video. The sidebar hosts the recommended grid.
                    watch_info.root.set_visible(true);
                    comments.root.set_visible(true);
                    // Blank the previous video's metadata now. `start_watch_meta` below lands
                    // asynchronously, and the old title, channel and description would stay
                    // visible until it completes.
                    watch_info.clear();
                    comments.clear();
                    *comments_continuation.borrow_mut() = None;
                    // Right column shows recommended for a VOD; the toggle shows or hides it
                    // (replay chat, if any, switches it to "chat"). This is the only place that
                    // returns the stack to "recommended" after a live stream set it to "chat".
                    sidebar.set_visible_child_name("recommended");
                    chat_toggle.set_tooltip_text(Some("Recommended"));
                    chat_toggle.set_visible(true);
                    chat_toggle.set_active(true);
                    sidebar.set_visible(true);
                    // Fetch watch metadata + related for the left block / recommended
                    // grid (generation-guarded on the comments token so a superseded
                    // click drops it), then the first comments page.
                    start_watch_meta(
                        rt_for_meta, meta_session, watch_info, recommended,
                        vid_for_meta, comments_gen.clone(),
                        comments_token, engine2.watch_details(),
                    );
                    start_comments(
                        rt_for_comments, comments_session, comments, comments_continuation,
                        vid_for_comments, comments_gen, comments_token,
                    );
                    // If this VOD was streamed live, show its replay chat synced to the player
                    // clock (switches the sidebar from recommended to chat once it resolves;
                    // no-ops for a plain VOD).
                    start_replay_chat(
                        rt_for_chat, chat_session, chat, engine2.clone(),
                        sidebar.clone(),
                        chat_toggle.clone(), toasts2.clone(),
                        vid_for_chat, chat_gen, chat_token,
                    );
                    current_quality.set(quality);
                    // Prefer the backend's switchable rungs (WebM/VP9 itags the
                    // in-session switch can actually reach) over every height in
                    // the player response; fall back to the latter if unavailable.
                    let menu_heights = engine2.vod_heights().unwrap_or_default();
                    player2.controls.set_qualities(&menu_heights, &quality);
                }
                engine2.queue_render();
            }
            Err(e) => {
                player2.set_loading(false);
                player2.root.set_visible(false);
                player_mode2.set(PlayerMode::Hidden);
                stack2.set_visible_child_name("results");
                if let PlaybackError::Stream(StreamError::Core(core)) = &e {
                    if handle_auth_expiry(core, &rt_auth, &app, &toasts2) {
                        return;
                    }
                }
                toasts2.add_toast(Toast::new(&playback_error_message(&e)));
            }
        }
    });
}

/// On an auth-expiry error (`NotLoggedIn`), clear the stored cookie, revert to an
/// anonymous session, flip the header back to "Sign in", and toast. Returns true if it
/// handled an expiry (so the caller can skip its normal error message).
fn handle_auth_expiry(
    err: &innertube::Error,
    rt: &AsyncRuntime,
    app: &App,
    toasts: &ToastOverlay,
) -> bool {
    if !matches!(err, innertube::Error::NotLoggedIn) {
        return false;
    }
    rt.spawn(async { crate::auth::store::clear().await }, |_| {});
    if super::auth::swap_session(app, None) {
        app.set_logged_in(false, None);
        toasts.add_toast(Toast::new("Session expired — please sign in again"));
    }
    true
}

pub(super) fn playback_error_message(e: &PlaybackError) -> String {
    tracing::warn!("[playback] open failed: {e:?}");
    let PlaybackError::Stream(stream) = e else {
        return format!("Playback failed: {e}");
    };
    if let Some(playability) = stream.playability() {
        return playability.reason().unwrap_or("This video can't be played").to_string();
    }
    let bgutil = crate::playback::pot_kind() == innertube::PotBackendKind::BgUtil;
    match stream {
        StreamError::Pot(PotError::Http(_)) if bgutil && stream.kind() == StreamErrorKind::Network => {
            "The bgutil PO-token server on 127.0.0.1:4416 is unreachable (Settings, PO-token minter)".to_string()
        }
        _ => format!("Playback failed: {}", failure_class(stream)),
    }
}

impl App {
    /// Opens the Shorts page on a Shorts card, or on `None` for the Shorts tab (the server
    /// picks the first Short).
    pub(super) fn open_shorts(&self, seed: Option<innertube::ReelEntry>) {
        let App { engine, stack, .. } = self;
        let shorts_view = self.shorts_view.get().expect("shorts page built");
        engine.set_paused(true);
        stack.set_visible_child_name("shorts");
        match seed {
            Some(e) => shorts_view.open_seed(e),
            None => shorts_view.open_seedless(),
        }
        shorts_view.grab_focus();
    }

    // `activate(item)` opens a video on the player page; the search grid and the channel
    // grid share it.
    pub(super) fn activate(self: &Rc<Self>, item: ResultItem) {
        let app = self;
        let App {
            engine,
            current_video,
            mpris,
            watch_positions,
            pip_quality_override,
            pip_reduced,
            video_history,
            suppress_history,
            history_back,
            next_referer,
            ..
        } = &**self;
        tracing::info!("[activate] {} uptime {}ms", item.video_id(), crate::uptime_ms());
        if let Some(reel) = item.reel() {
            app.open_shorts(Some(reel));
            return;
        }
        let referer = next_referer
            .take()
            .or_else(|| current_video.borrow().as_deref().map(watch_url))
            .unwrap_or_else(|| YOUTUBE_HOME.into());
        // A reduced cap set by play_mini loads this video at PiP resolution; a normal activate
        // (None) uses the user's cap. Remembered so expand can restore it.
        let quality_override = pip_quality_override.take();
        pip_reduced.set(quality_override.is_some());
        // History-back replays (the history_back button below) set this just before
        // calling `activate` again so this pass doesn't re-push the video it's leaving.
        // Read-and-clear now so it applies to exactly this one call.
        let is_history_replay = suppress_history.take();
        // Save the outgoing VOD's position so returning to it resumes. Read before
        // on_result_activated swaps current_video. Skip the first few seconds and the last
        // ~10 s (effectively finished), clearing any stale entry in the finished case.
        if let Some(old) = current_video.borrow().clone() {
            let pos = engine.time_pos();
            let near_end = engine.duration().map(|d| pos > d - 10.0).unwrap_or(false);
            if pos > 3.0 && !near_end {
                app.record_position(&old, pos);
            } else if near_end {
                app.clear_position(&old);
            }
            // Push the video we're navigating away from, unless this is the history-back
            // button's own replay, or we're "navigating" to the same video again.
            if !is_history_replay && old != item.video_id() {
                video_history.borrow_mut().push(old);
            }
        }
        history_back.set_sensitive(!video_history.borrow().is_empty());
        let want = item.video_id();
        let resume_at =
            watch_positions.borrow().iter().find(|(id, _)| id == &want).map(|(_, p)| *p);
        // Show the full player surface (also leaves mini mode if we were in it).
        app.show_full();
        // Publish now-playing metadata for the lockscreen and media keys.
        mpris.set_track(&item.title(), &item.channel(), item.thumbnail_url().as_deref(), None);
        mpris.set_status("Playing");
        on_result_activated(app, resume_at, quality_override, referer, item);
    }

    // Play a video directly in the corner PiP: load it (which briefly targets the full
    // page), then drop to mini. Both steps are synchronous, so only the mini state renders.
    // Used when queueing while browsing (nothing full-screen).
    pub(super) fn play_mini(self: &Rc<Self>, item: ResultItem) {
        let App { pip_quality_override, current_quality, .. } = &**self;
        // Load at PiP resolution: set the override the next activate consumes.
        pip_quality_override.set(Some(pip_quality(current_quality.get())));
        self.activate(item);
        self.show_mini();
    }

    // Play the next item in the current mode (full stays full, the corner PiP stays in the
    // corner); used for queue auto-advance.
    pub(super) fn play_in_mode(self: &Rc<Self>, item: ResultItem) {
        if self.player_mode.get() == PlayerMode::Mini {
            self.play_mini(item);
        } else {
            self.activate(item);
        }
    }
}

/// The header queue panel and the end-of-video auto-advance.
pub(super) fn wire_queue(app: &Rc<App>) {
    let App { prefs_store, engine, stack, queue, header, last_page, player_mode, .. } = &**app;
    // Header queue panel: the card menus fill the queue and this button shows and manages it.
    // Selecting a row plays it in the corner PiP and leaves it in the queue.
    let queue_panel = QueuePanel::new(queue.clone(), {
        let app = app.clone();
        Rc::new(move |item| app.play_mini(item))
    });
    header.pack_end(&queue_panel.button);

    engine.set_on_failed({
        let toasts = app.toasts.clone();
        Rc::new(move |reason: std::sync::Arc<StreamError>| {
            toasts.add_toast(Toast::new(&stopped_message(&reason)));
        })
    });

    // Auto-advance: a finished video leaves the queue and the next plays in the same mode.
    // With an empty queue, leave the player, returning to the page we came from when
    // full-screen.
    engine.set_on_ended({
        let queue = queue.clone();
        let app = app.clone();
        let stack = stack.clone();
        let last_page = last_page.clone();
        let player_mode = player_mode.clone();
        let prefs_store = prefs_store.clone();
        Rc::new(move || {
            let next = queue.borrow_mut().advance();
            match next {
                Some(item) => app.play_in_mode(item),
                None => {
                    // Return to results when the queue drains, unless the auto-return
                    // preference is off, which leaves the player up.
                    if prefs_store.get().auto_return {
                        if player_mode.get() == PlayerMode::Full {
                            stack.set_visible_child_name(&last_page.borrow());
                        }
                        app.hide_player();
                    }
                }
            }
        })
    });
}
