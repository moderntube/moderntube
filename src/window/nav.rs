//! Navigation: fullscreen, leaving the player for another page, and the nav rail.
use std::rc::Rc;

use adw::prelude::*;
use adw::Toast;

use crate::models::ResultItem;
use crate::widgets::NavRail;

use super::app::App;
use super::{FeedKind, PlayerMode};

impl App {
    // Fullscreen toggle; the window owns the state. Fullscreen hides the player header so the
    // video fills the screen, and the transport controls stay as an overlay. The controls
    // button toggles it and Esc exits.
    pub(super) fn set_fullscreen(&self, fs: bool) {
        let App { player, fs_state, video_fullscreen, stage, .. } = self;
        let window = self.window.get().expect("window built");
        let player_header = self.player_header.get().expect("player page built");
        let nav = self.nav_root.get().expect("nav rail built");
        fs_state.set(fs);
        if fs {
            window.fullscreen();
        } else {
            window.unfullscreen();
        }
        // The video covers the whole window; the nav rail and page stay behind it.
        video_fullscreen.set(fs);
        nav.set_visible(!fs);
        stage.queue_allocate();
        player_header.set_visible(!fs);
        // No rounded corners in fullscreen: the video runs edge to edge.
        if fs {
            player.root.remove_css_class("video-frame");
            player.root.add_css_class("fullscreen-player");
        } else {
            player.root.add_css_class("video-frame");
            player.root.remove_css_class("fullscreen-player");
        }
        player.controls.set_fullscreen_state(fs);
    }

    // Leaves the player for `page`: exits fullscreen and drops a full-screen player to the
    // corner PiP, so playback continues and the video stage does not cover the new page.
    pub(super) fn leave_player_to(&self, page: &str) {
        let App { fs_state, player_mode, stack, last_page, .. } = self;
        if fs_state.get() {
            self.set_fullscreen(false);
        }
        *last_page.borrow_mut() = page.to_string();
        if player_mode.get() == PlayerMode::Full {
            self.show_mini(); // keep playback in the corner; stack shows `page` (last_page)
        } else {
            stack.set_visible_child_name(page);
        }
    }

    // A channel click while full-screen drops to PiP and shows the channel page.
    pub(super) fn open_channel(&self, id: String, name: String) {
        *self.channel_url.borrow_mut() = format!("https://www.youtube.com/channel/{id}");
        self.channel_view().open(id, name);
        self.leave_player_to("channel");
    }

    // A playlist card opens the ordered playlist page, dropping a full-screen video to the
    // corner PiP on the way.
    pub(super) fn open_playlist(&self, id: String, name: String) {
        *self.playlist_url.borrow_mut() = format!("https://www.youtube.com/playlist?list={id}");
        self.playlist_view().open(id, name);
        self.leave_player_to("playlist");
    }
}

// Nav-rail Home and Subscriptions leave the player: they exit fullscreen, drop a full-screen
// video to the corner PiP so it keeps playing, and show the feed.
pub(super) fn wire_nav_rail(app: &Rc<App>, nav_rail: &NavRail) {
    let App {
        rt, session, toasts, player, stack, search_continuation, last_page, results_url, player_mode, fs_state,
        ..
    } = &**app;
    let results = app.results();
    {
        let go_feed: Rc<dyn Fn(FeedKind)> = {
            let app = app.clone();
            Rc::new(move |kind: FeedKind| {
                app.leave_player_to("results");
                app.load_feed(kind);
            })
        };
        {
            let go_feed = go_feed.clone();
            nav_rail.connect_home(move || go_feed(FeedKind::Home));
        }
        {
            let go_feed = go_feed.clone();
            nav_rail.connect_subscriptions(move || go_feed(FeedKind::Subscriptions));
        }
        {
            let app = app.clone();
            nav_rail.connect_shorts(move || {
                app.leave_player_to("shorts");
                app.open_shorts(None);
            });
        }
        // Nav-rail Playlists: the library view, with your playlists as cards; a card opens the
        // ordered playlist for play-through. Requires sign-in (`FElibrary`).
        {
            let fs_state = fs_state.clone();
            let app = app.clone();
            let player_mode = player_mode.clone();
            let stack = stack.clone();
            let last_page = last_page.clone();
            let rt = rt.clone();
            let session = session.clone();
            let results = results.clone();
            let toasts = toasts.clone();
            let search_continuation = search_continuation.clone();
            let results_url = results_url.clone();
            nav_rail.connect_playlists(move || {
                if fs_state.get() {
                    app.set_fullscreen(false);
                }
                *results_url.borrow_mut() = "https://www.youtube.com/feed/playlists".into();
                *last_page.borrow_mut() = "results".into();
                if player_mode.get() == PlayerMode::Full {
                    app.show_mini();
                } else {
                    stack.set_visible_child_name("results");
                }
                // The library is a set of playlist cards with no paging; clear the continuation.
                *search_continuation.borrow_mut() = None;
                let results = results.clone();
                let toasts = toasts.clone();
                let current = session.borrow().clone();
                results.set_empty_text("No playlists yet.");
                rt.spawn(
                    async move { current.list_playlists().await },
                    move |res| match res {
                        Ok(pls) => {
                            // Reached only when authed (anonymous → the Err(NotLoggedIn)
                            // arm below), so "sign in" would be wrong here.
                            if pls.is_empty() {
                                toasts.add_toast(Toast::new("You have no playlists yet"));
                            }
                            let items: Vec<ResultItem> = pls
                                .iter()
                                .map(|p| ResultItem::for_playlist(&p.id, &p.title, p.thumbnail.clone()))
                                .collect();
                            results.set_items(items);
                        }
                        // Anonymous → NotLoggedIn: show the actionable message here.
                        Err(innertube::Error::NotLoggedIn) => {
                            toasts.add_toast(Toast::new("Sign in to see your playlists"));
                        }
                        Err(e) => toasts.add_toast(Toast::new(&format!("Playlists failed: {e}"))),
                    },
                );
            });
        }
    }
    {
        let app = app.clone();
        let fs_state = fs_state.clone();
        player.controls.connect_fullscreen(move || app.set_fullscreen(!fs_state.get()));
    }
}
