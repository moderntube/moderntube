use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;
use gtk::{Stack, ToggleButton};

use innertube::{ChatReplay, ChatSendResult, LiveChat, Session};
use crate::playback::Player;
use crate::runtime::{AsyncRuntime, Generation};
use crate::widgets::LiveChatView;

use super::app::App;

/// Kick off live-chat: fetch the first page (resolves the chat continuation), render
/// it, then self-schedule polling. `gen`/`token` stop a stale loop once a new video
/// opens; a live-edge re-join keeps the same chat and does not bump `chat_gen`.
pub(super) fn start_live_chat(
    rt: AsyncRuntime,
    session: Arc<Session>,
    chat: Rc<LiveChatView>,
    video_id: String,
    gen: Generation,
    token: u64,
) {
    let (rt2, s2, chat2, gen2) = (rt.clone(), session.clone(), chat.clone(), gen.clone());
    rt.spawn(
        async move { session.live_chat(&video_id).await },
        move |res: Result<LiveChat, innertube::Error>| {
            if !gen2.is_current(token) {
                return;
            }
            let Ok(page) = res else { return }; // not live / chat disabled → empty
            let LiveChat { messages, continuation, poll_ms } = page;
            chat2.enqueue(messages); // dripped onto the view by the widget
            if let Some(cont) = continuation {
                schedule_chat_poll(rt2, s2, chat2, cont, gen2, token, poll_ms);
            }
        },
    );
}

/// Poll the next chat page after the server-suggested delay, append new messages, and
/// re-arm until the generation token goes stale (new video) or chat ends.
fn schedule_chat_poll(
    rt: AsyncRuntime,
    session: Arc<Session>,
    chat: Rc<LiveChatView>,
    cont: String,
    gen: Generation,
    token: u64,
    poll_ms: Option<i64>,
) {
    let delay = poll_ms.unwrap_or(2000).clamp(800, 10_000) as u64;
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        if !gen.is_current(token) {
            return;
        }
        let (rt2, s2, chat2, gen2) = (rt.clone(), session.clone(), chat.clone(), gen.clone());
        let cont_retry = cont.clone();
        rt.spawn(
            async move { session.live_chat_page(&cont).await },
            move |res: Result<LiveChat, innertube::Error>| {
                if !gen2.is_current(token) {
                    return;
                }
                // A transient poll error or a missing continuation must not stop chat. Retry
                // the same continuation on error and reuse it when the server returns no new
                // token. The generation token still ends the loop on a video switch.
                match res {
                    Ok(LiveChat { messages, continuation, poll_ms }) => {
                        tracing::debug!(
                            target: "moderntube::diag::chat",
                            "[chat] poll: +{} msgs (next in {}ms)",
                            messages.len(),
                            poll_ms.unwrap_or(2000)
                        );
                        chat2.enqueue(messages);
                        let next = continuation.unwrap_or(cont_retry);
                        schedule_chat_poll(rt2, s2, chat2, next, gen2, token, poll_ms);
                    }
                    Err(e) => {
                        tracing::warn!("[chat] poll error (retrying in 2s): {e}");
                        schedule_chat_poll(rt2, s2, chat2, cont_retry, gen2, token, Some(2000));
                    }
                }
            },
        );
    });
}

/// Chat replay for a VOD that was streamed live: probes `live_chat_replay` and, when replay
/// chat exists, shows the chat panel and polls it synced to the player clock. Otherwise it
/// leaves the sidebar as it is.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_replay_chat(
    rt: AsyncRuntime,
    session: Arc<Session>,
    chat: Rc<LiveChatView>,
    engine: Rc<dyn Player>,
    sidebar: Stack,
    chat_toggle: ToggleButton,
    toasts: ToastOverlay,
    video_id: String,
    gen: Generation,
    token: u64,
) {
    let (rt2, s2, chat2, eng2, gen2) = (rt.clone(), session.clone(), chat.clone(), engine.clone(), gen.clone());
    rt.spawn(
        async move { session.live_chat_replay(&video_id).await },
        move |res: Result<ChatReplay, innertube::Error>| {
            if !gen2.is_current(token) {
                return;
            }
            let lc = match res {
                Ok(ChatReplay::Available(lc)) => lc,
                // Was live but replay is off or not retained: show YouTube's notice.
                Ok(ChatReplay::Unavailable(msg)) => {
                    toasts.add_toast(Toast::new(
                        msg.as_deref().unwrap_or("Live chat replay isn't available for this video"),
                    ));
                    return;
                }
                // Ordinary VOD that was never live: no notice.
                Ok(ChatReplay::NotLive) => return,
                // A failure looks like a plain VOD in the UI, so it is logged.
                Err(e) => {
                    tracing::warn!("[chat] replay lookup failed: {e}");
                    return;
                }
            };
            // Replay available: switch the sidebar to the (read-only) replay chat.
            chat2.clear();
            chat2.set_replay(true); // hide the compose row; a VOD replay takes no posts
            chat_toggle.set_tooltip_text(Some("Chat replay"));
            chat_toggle.set_visible(true);
            chat_toggle.set_active(true);
            sidebar.set_visible_child_name("chat");
            sidebar.set_visible(true);
            let pos_ms = (eng2.time_pos() * 1000.0) as i64;
            let LiveChat { messages, continuation, .. } = lc;
            let due: Vec<_> = messages.into_iter().filter(|m| m.video_offset_ms.is_none_or(|o| o <= pos_ms)).collect();
            chat2.enqueue(due);
            if let Some(cont) = continuation {
                schedule_replay_poll(rt2, s2, chat2, eng2, cont, gen2, token);
            }
        },
    );
}

/// Poll the next replay batch ~1s later at the current player offset, reveal only the
/// messages due by now (the widget dedups overlap by id), and re-arm until the generation
/// token goes stale (new video).
fn schedule_replay_poll(
    rt: AsyncRuntime,
    session: Arc<Session>,
    chat: Rc<LiveChatView>,
    engine: Rc<dyn Player>,
    cont: String,
    gen: Generation,
    token: u64,
) {
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
        if !gen.is_current(token) {
            return;
        }
        let pos_ms = (engine.time_pos() * 1000.0) as i64;
        let (rt2, s2, chat2, eng2, gen2, keep) =
            (rt.clone(), session.clone(), chat.clone(), engine.clone(), gen.clone(), cont.clone());
        rt.spawn(
            async move { session.live_chat_replay_page(&cont, pos_ms).await },
            move |res: Result<LiveChat, innertube::Error>| {
                if !gen2.is_current(token) {
                    return;
                }
                let next = match res {
                    Ok(LiveChat { messages, continuation, .. }) => {
                        let due: Vec<_> =
                            messages.into_iter().filter(|m| m.video_offset_ms.is_none_or(|o| o <= pos_ms)).collect();
                        chat2.enqueue(due);
                        continuation.unwrap_or(keep)
                    }
                    // Keep the old continuation and retry on the next tick. Logged, since a
                    // stalled replay looks like a window with no messages.
                    Err(e) => {
                        tracing::warn!("[chat] replay poll failed, retrying with the same continuation: {e}");
                        keep
                    }
                };
                schedule_replay_poll(rt2, s2, chat2, eng2, next, gen2, token);
            },
        );
    });
}


/// Posts a typed chat message to the live stream.
pub(super) fn wire_chat_send(app: &Rc<App>) {
    let App { rt, session, toasts, chat, current_live, .. } = &**app;

    // Posts a chat message (requires sign-in). A confirmed send is echoed at once and deduped
    // against the poll; rejections and errors are toasted.
    {
        let rt = rt.clone();
        let session = session.clone();
        let toasts = toasts.clone();
        let current_live = current_live.clone();
        let chat_send = chat.clone();
        chat.connect_send(move |text: String| {
            let Some(id) = current_live.borrow().clone() else { return };
            let sess = session.borrow().clone();
            let toasts2 = toasts.clone();
            let chat2 = chat_send.clone();
            let unsent = text.clone();
            tracing::info!("[chat] send ({} chars) → {id}", unsent.chars().count());
            rt.spawn(
                async move { sess.live_chat_send(&id, &text).await.map_err(|e| e.to_string()) },
                move |res: Result<ChatSendResult, String>| match res {
                    // A confirmed send is echoed. A rejection (slow mode, timeout, duplicate)
                    // is toasted and the typed message is restored for a retry.
                    Ok(ChatSendResult::Sent(msg)) => {
                        tracing::info!("[chat] send OK (echo={})", msg.is_some());
                        if let Some(m) = msg {
                            chat2.push_own(&m); // optimistic echo (deduped vs the poll)
                        }
                    }
                    Ok(ChatSendResult::Rejected(reason)) => {
                        tracing::info!("[chat] send REJECTED: {reason}");
                        toasts2.add_toast(Toast::new(&format!("Not sent: {reason}")));
                        chat2.restore_unsent(&unsent);
                    }
                    Err(e) => {
                        tracing::warn!("[chat] send ERROR: {e}");
                        toasts2.add_toast(Toast::new(&format!("Couldn't send: {e}")));
                        chat2.restore_unsent(&unsent);
                    }
                },
            );
        });
    }
}
