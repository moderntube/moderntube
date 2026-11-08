//! Top-level window: a `GtkStack` with a results page and a persistent player page. One
//! app-lifetime `Player` serves every video: a click prepares the video, loads it on that
//! player and switches the stack; back stops playback and switches back. Search and the
//! click-to-open cross the glib/tokio bridge; a `Generation` per action drops superseded
//! results.
use std::rc::Rc;

use adw::prelude::*;
use adw::ApplicationWindow;
use gtk4 as gtk;
use gtk::{Box as GtkBox, Orientation};

use innertube::Quality;
use crate::runtime::AsyncRuntime;

mod app;
mod appearance;
mod auth;
mod catchup;
mod chat;
mod feedback;
mod hooks;
mod live;
mod media_keys;
mod nav;
mod open;
mod pages;
mod player_page;
mod preferences;
mod probes;
mod quality;
mod search;
mod settings;
mod shortcuts;
mod sponsor;
mod stage;
mod transcript;
mod watch;

use app::App;
use open::playback_error_message;
use player_page::PlayerPage;

// `LIVE_QUALITY_LADDER` (below) lists the live quality ceilings the menu offers, 2160p to
// 144p. Live has no per-video height list. The engine caps downshift-only and resolves a
// pick the stream lacks to the nearest available rung; `set_qualities` prepends "Auto".
/// The YouTube home URL, the default `referer`.
const YOUTUBE_HOME: &str = "https://www.youtube.com/";

fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// The first mapped `ScrolledWindow` in `w`'s subtree with room to scroll, depth-first.
fn first_scrolled_window(w: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
        let adj = sw.vadjustment();
        if sw.is_mapped() && adj.upper() > adj.page_size() {
            return Some(sw.clone());
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(sw) = first_scrolled_window(&c) {
            return Some(sw);
        }
        child = c.next_sibling();
    }
    None
}

const LIVE_QUALITY_LADDER: &[i32] = &[2160, 1440, 1080, 720, 480, 360, 240, 144];

/// Which feed the results grid shows. It decides which endpoint the search-pagination
/// continuation token pages: search or the browse-based feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedKind {
    Search,
    Home,
    Subscriptions,
}

/// Where the single video surface is shown. It is never reparented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayerMode {
    /// No video loaded; surface hidden.
    Hidden,
    /// Full player page; surface positioned over the page's video slot.
    Full,
    /// Corner miniplayer; surface shrunk to the bottom-right while browsing.
    Mini,
}

pub fn build(application: &adw::Application, rt: AsyncRuntime) -> ApplicationWindow {
    let session = match crate::auth::build_session(None) {
        Ok(s) => s,
        Err(e) => return startup_error_window(application, &e),
    };
    let app = App::new(rt, session);
    let App {
        rt,
        prefs_store,
        session,
        toasts,
        stack,
        results_box,
        settings_box,
        hover_preview,
        stage,
        ..
    } = &*app;
    auth::wire_sign_in(&app);
    // A clicked preview hands its watch position off through the shared recorder, so opening
    // the video resumes where the preview was.
    hover_preview.set_position_recorder({
        let app = app.clone();
        Rc::new(move |id: &str, secs: f64| app.record_position(id, secs))
    });
    stage::wire_stage(&app);
    stage::wire_mini_buttons(&app);

    let shorts_view = Rc::new(crate::widgets::ShortsView::new(rt.clone(), session.clone(), toasts.clone(), {
        let prefs_store = prefs_store.clone();
        Rc::new(move || prefs_store.get().report_watches)
    }));
    let _ = app.shorts_view.set(shorts_view.clone());
    open::wire_queue(&app);
    pages::build_pages(&app);
    let (channel_view, playlist_view) = (app.channel_view(), app.playlist_view());

    watch::wire_watch_actions(&app);

    let page = player_page::build_player_page(&app);
    let _ = app.player_header.set(page.player_header.clone());
    player_page::wire_sidebar(&app, &page);
    watch::wire_comment_paging(&app);
    player_page::wire_history_buttons(&app, &page);
    let PlayerPage { player_box, player_search, .. } = &page;

    stack.add_named(results_box, Some("results"));
    stack.add_named(&channel_view.root, Some("channel"));
    stack.add_named(&playlist_view.root, Some("playlist"));
    stack.add_named(player_box, Some("player"));
    let shorts_header = adw::HeaderBar::new();
    let shorts_search = gtk::SearchEntry::builder().placeholder_text("Search YouTube").width_request(360).build();
    shorts_header.set_title_widget(Some(&shorts_search));
    let shorts_page = GtkBox::new(Orientation::Vertical, 0);
    shorts_page.append(&shorts_header);
    shorts_view.root.set_vexpand(true);
    shorts_page.append(&shorts_view.root);
    stack.add_named(&shorts_page, Some("shorts"));
    stack.set_visible_child_name("results");
    {
        let shorts_view = shorts_view.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() != Some("shorts") {
                shorts_view.stop();
            }
        });
    }

    // Left nav rail, created here so it can be placed in the shell.
    let nav_rail = crate::widgets::NavRail::new();
    let _ = app.nav_root.set(nav_rail.root.clone());
    settings::attach_settings_popover(&nav_rail, settings_box);
    let shell = GtkBox::new(Orientation::Horizontal, 0);
    shell.append(&nav_rail.root);
    shell.append(stage);
    stage.set_hexpand(true);
    stage.set_vexpand(true);
    toasts.set_child(Some(&shell));

    search::wire_search_boxes(&app, &[player_search, &shorts_search]);
    // The nav rail's Home and Subscriptions are wired in `nav::wire_nav_rail` once the window
    // exists, so they can leave the player page.
    // The initial home load runs after the stored session resolves (`auth::wire_session`): a
    // locked keyring would otherwise leave an anonymous fetch racing the unlock prompt.

    let window = ApplicationWindow::builder()
        .application(application)
        .title("moderntube")
        .default_width(1100)
        .default_height(700)
        .content(toasts)
        .build();

    let _ = app.window.set(window.clone());

    preferences::wire_preferences(application, &window, app.rt.clone());
    app.header.pack_end(&preferences::primary_menu());
    auth::wire_session(&app, &window);
    probes::wire_probes(&app, &window);
    nav::wire_nav_rail(&app, &nav_rail);
    live::wire_live_edge(&app);
    catchup::wire_catchup(&app, &page);
    chat::wire_chat_send(&app);
    quality::wire_quality(&app);
    shortcuts::wire_shortcuts(&app, &window);
    feedback::wire_feedback(&app, &window);
    sponsor::wire_sponsorblock(&app);
    hooks::wire_startup_hooks(&app);
    media_keys::wire_mpris(&app, &window);
    watch::wire_playhead_timers(&app);

    window
}

/// With no HTTP client there is nothing to browse, so the window says why and stays open.
fn startup_error_window(application: &adw::Application, e: &innertube::Error) -> ApplicationWindow {
    tracing::warn!("[startup] could not build a session: {e}");
    let page = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("Could not start a session")
        .description(e.to_string())
        .build();
    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&adw::HeaderBar::new());
    page.set_vexpand(true);
    content.append(&page);
    ApplicationWindow::builder()
        .application(application)
        .title("moderntube")
        .default_width(1100)
        .default_height(700)
        .content(&content)
        .build()
}

/// The reduced quality cap for the corner PiP: 480p, or the user's cap when that is lower.
fn pip_quality(user_cap: Quality) -> Quality {
    match user_cap {
        Quality::Best => Quality::Height(480),
        Quality::Height(h) => Quality::Height(h.min(480)),
    }
}

/// A sidebar dragged narrower than this closes; the toggle reopens it at its last width.
const SIDEBAR_COLLAPSE_PX: i32 = 120;
/// The narrowest width a sidebar reopens at.
const SIDEBAR_REOPEN_PX: i32 = 280;
