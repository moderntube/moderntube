use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::Toast;
use gtk4 as gtk;

use innertube::Session;
use crate::models::ResultItem;
use crate::runtime::{AsyncRuntime, Generation};
use crate::widgets::{CommentsView, ResultsView, WatchInfo};

use super::app::App;

/// Fetches a video's watch metadata and fills the `WatchInfo` block and the recommended grid
/// from `related`. A newer open (a changed generation token) drops the result.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_watch_meta(
    rt: AsyncRuntime,
    session: Arc<Session>,
    watch_info: Rc<WatchInfo>,
    recommended: Rc<ResultsView>,
    video_id: String,
    gen: Generation,
    token: u64,
    watch_details: Option<innertube::VideoDetails>,
) {
    // The open already carried the watch-next data (`get_watch`, IT-006); fill from it and skip
    // the fetch below.
    if let Some(d) = watch_details {
        fill_watch_meta(&watch_info, &recommended, &video_id, d);
        return;
    }
    let id_for_details = video_id.clone();
    rt.spawn(
        async move {
            // Prefer the watch page over the `next` POST: only the page carries the sidebar's
            // signed `an_webp` hover previews.
            let page = session
                .get_text(&format!("https://www.youtube.com/watch?v={video_id}"))
                .await
                .ok()
                .and_then(|html| innertube::video_details_from_watch_html(&html));
            match page {
                Some(d) => Ok(d),
                // Age-gated, consent-walled or unreadable pages fall back to the POST, which has
                // no animated previews.
                None => {
                    tracing::info!("[watch-meta] no ytInitialData on the page — falling back to next()");
                    session.next(&video_id).await
                }
            }
        },
        move |res: Result<innertube::VideoDetails, innertube::Error>| {
            if !gen.is_current(token) {
                return; // a newer video opened mid-fetch
            }
            let Ok(d) = res else { return }; // fetch failed → leave the block/grid as-is
            fill_watch_meta(&watch_info, &recommended, &id_for_details, d);
        },
    );
}

/// Render `d` into the watch-info block and the recommended grid. The fetch and the
/// `watch_details` fast path of `start_watch_meta` both land here.
fn fill_watch_meta(
    watch_info: &WatchInfo,
    recommended: &ResultsView,
    video_id: &str,
    d: innertube::VideoDetails,
) {
    watch_info.set_details(&d, video_id);
    // Count at each boundary so an empty grid can be traced in the log.
    let related: Vec<ResultItem> = d.related.iter().filter_map(ResultItem::from_search_item).collect();
    tracing::info!(
        "[watch] related: {} fetched → {} converted (dropped {})",
        d.related.len(),
        related.len(),
        d.related.len() - related.len(),
    );
    recommended.set_items(related);
}

/// Fetch the first page of comments for a VOD, render it, and store the next-page
/// continuation (for the load-more handler). Generation-guarded so a slow fetch for a
/// since-closed video is dropped instead of landing on the new one.
pub(super) fn start_comments(
    rt: AsyncRuntime,
    session: Arc<Session>,
    comments: Rc<CommentsView>,
    continuation: Rc<RefCell<Option<String>>>,
    video_id: String,
    gen: Generation,
    token: u64,
) {
    comments.set_video_id(&video_id); // target the post box at this video
    rt.spawn(
        async move { session.comments(&video_id).await },
        move |res: Result<innertube::Comments, innertube::Error>| {
            if !gen.is_current(token) {
                return;
            }
            let Ok(page) = res else { return }; // comments disabled / fetch failed → empty
            *continuation.borrow_mut() = page.continuation.clone();
            comments.set_comments(&page.items);
        },
    );
}

/// Wires the watch page's Subscribe, Like, Share, Save and channel actions.
pub(super) fn wire_watch_actions(app: &Rc<App>) {
    let App { rt, session, toasts, watch_info, .. } = &**app;
    // Connect `watch_info`'s callbacks to the backend. Subscribe flips its state only after
    // the call returns Ok; Like and Dislike flip first and revert on error.
    {
        let wi = watch_info.clone();
        let session = session.clone();
        let toasts = toasts.clone();
        let rt = rt.clone();
        watch_info.connect_subscribe(move || {
            if !session.borrow().is_authenticated() {
                toasts.add_toast(Toast::new("Sign in to subscribe"));
                return;
            }
            let Some(cid) = wi.channel_id() else { return };
            let was = wi.is_subscribed();
            let current = session.borrow().clone();
            let wi = wi.clone();
            let toasts = toasts.clone();
            rt.spawn(
                async move {
                    if was {
                        current.unsubscribe(&cid).await
                    } else {
                        current.subscribe(&cid).await
                    }
                },
                move |res| match res {
                    Ok(_) => wi.set_subscribed(!was),
                    Err(e) => toasts.add_toast(Toast::new(&format!("Subscribe failed: {e}"))),
                },
            );
        });
    }
    {
        let wi = watch_info.clone();
        let session = session.clone();
        let toasts = toasts.clone();
        let rt = rt.clone();
        watch_info.connect_like(move |new_liked| {
            // The widget flips optimistically before firing this; revert if we cannot proceed.
            if !session.borrow().is_authenticated() {
                wi.set_liked(!new_liked);
                toasts.add_toast(Toast::new("Sign in to like"));
                return;
            }
            let vid = wi.video_id();
            let current = session.borrow().clone();
            let wi = wi.clone();
            let toasts = toasts.clone();
            rt.spawn(
                async move {
                    if new_liked {
                        current.like(&vid).await
                    } else {
                        current.remove_like(&vid).await
                    }
                },
                move |res| {
                    if let Err(e) = res {
                        wi.set_liked(!new_liked); // revert the optimistic flip on failure
                        toasts.add_toast(Toast::new(&format!("Like failed: {e}")));
                    }
                },
            );
        });
    }
    {
        let wi = watch_info.clone();
        let session = session.clone();
        let toasts = toasts.clone();
        let rt = rt.clone();
        watch_info.connect_dislike(move |new_disliked| {
            // The widget flips optimistically before firing this; revert if we can't proceed.
            if !session.borrow().is_authenticated() {
                wi.set_disliked(!new_disliked);
                toasts.add_toast(Toast::new("Sign in to dislike"));
                return;
            }
            let vid = wi.video_id();
            let current = session.borrow().clone();
            let wi = wi.clone();
            let toasts = toasts.clone();
            rt.spawn(
                async move {
                    if new_disliked {
                        current.dislike(&vid).await
                    } else {
                        current.remove_like(&vid).await
                    }
                },
                move |res| {
                    if let Err(e) = res {
                        wi.set_disliked(!new_disliked);
                        toasts.add_toast(Toast::new(&format!("Dislike failed: {e}")));
                    }
                },
            );
        });
    }
    {
        let wi = watch_info.clone();
        let toasts = toasts.clone();
        watch_info.connect_share(move || {
            let vid = wi.video_id();
            if vid.is_empty() {
                return;
            }
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&format!("https://youtu.be/{vid}"));
            }
            toasts.add_toast(Toast::new("Link copied"));
        });
    }
    {
        let wi = watch_info.clone();
        let toasts = toasts.clone();
        let session = session.clone();
        let rt = rt.clone();
        // Save adds the video to Watch Later.
        watch_info.connect_save(move || {
            let vid = wi.video_id();
            if vid.is_empty() {
                return;
            }
            if !session.borrow().is_authenticated() {
                toasts.add_toast(Toast::new("Sign in to save to Watch Later"));
                return;
            }
            let current = session.borrow().clone();
            let toasts = toasts.clone();
            rt.spawn(
                async move { current.watch_later_add(&vid).await },
                move |res| match res {
                    Ok(_) => toasts.add_toast(Toast::new("Saved to Watch Later")),
                    Err(e) => toasts.add_toast(Toast::new(&format!("Couldn't save: {e}"))),
                },
            );
        });
    }
    {
        let wi = watch_info.clone();
        let app = app.clone();
        watch_info.connect_channel(move || {
            let Some(cid) = wi.channel_id() else { return };
            app.open_channel(cid, wi.channel_name());
        });
    }
}

/// Fetches the next comments page as the thread nears its end.
pub(super) fn wire_comment_paging(app: &Rc<App>) {
    let App { rt, session, toasts, comments, comments_gen, comments_continuation, .. } = &**app;
    // Infinite scroll for the comment thread: fetch the next page and append it,
    // guarded by `comments_gen` so a page in flight when the user opens a new video
    // is dropped instead of polluting the new thread.
    {
        let rt = rt.clone();
        let session = session.clone();
        let comments_cb = comments.clone();
        let toasts = toasts.clone();
        let comments_gen = comments_gen.clone();
        let comments_continuation = comments_continuation.clone();
        comments.connect_load_more(move || {
            let Some(token) = comments_continuation.borrow().clone() else { return };
            let gen_token = comments_gen.current();
            let current = session.borrow().clone();
            let generation = comments_gen.clone();
            let comments = comments_cb.clone();
            let toasts = toasts.clone();
            let comments_continuation = comments_continuation.clone();
            rt.spawn(
                async move { current.comments_page(&token).await },
                move |res: Result<innertube::Comments, innertube::Error>| {
                    if !generation.is_current(gen_token) {
                        return; // a new video opened mid-load
                    }
                    match res {
                        Ok(page) => {
                            *comments_continuation.borrow_mut() = page.continuation.clone();
                            comments.append_comments(&page.items);
                        }
                        Err(e) => toasts.add_toast(Toast::new(&format!("Couldn't load more comments: {e}"))),
                    }
                },
            );
        });
    }
}


/// The once-a-second watch report and the five-second resume-position save.
pub(super) fn wire_playhead_timers(app: &Rc<App>) {
    let App { rt, prefs_store, session, engine, current_video, .. } = &**app;
    // Watch reporting: the playing VOD's playhead, once a second, to a reporter on the
    // signed-in session. A change of video, a stop or the toggle going off ends the report.
    {
        let engine = engine.clone();
        let current_video = current_video.clone();
        let prefs_store = prefs_store.clone();
        let session = session.clone();
        let rt = rt.clone();
        let report: RefCell<Option<(String, innertube::WatchReport)>> = RefCell::new(None);
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let pos = engine.time_pos_checked();
            let want = current_video
                .borrow()
                .clone()
                .filter(|_| pos.is_some() && prefs_store.get().report_watches && session.borrow().is_authenticated());
            let mut report = report.borrow_mut();
            if report.as_ref().map(|(id, _)| id) != want.as_ref() {
                *report = want.map(|id| {
                    let (handle, task) = innertube::WatchReport::new(session.borrow().clone(), &id);
                    rt.spawn_bg(task);
                    (id, handle)
                });
            }
            if let (Some((_, handle)), Some(pos)) = (report.as_ref(), pos) {
                handle.update(pos, !engine.is_paused());
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    // Every 5 s, record the playhead of the playing VOD so a killed app still resumes.
    // `current_video` is None on live. A position near the end clears the entry.
    {
        let engine = engine.clone();
        let current_video = current_video.clone();
        let app = app.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(5), move || {
            if let Some(id) = current_video.borrow().clone() {
                let pos = engine.time_pos();
                let near_end = engine.duration().map(|d| pos > d - 10.0).unwrap_or(false);
                if near_end {
                    app.clear_position(&id);
                } else if pos > 3.0 {
                    app.record_position(&id, pos);
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
}
