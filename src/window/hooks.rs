//! Startup test hooks that open the Shorts tab, a video, a search or a playlist.
use std::rc::Rc;

use gtk4 as gtk;

use crate::hooks;
use crate::models::ResultItem;

use super::app::App;

/// `[<secs>:]<video_id>`; a video id holds no colon.
fn delayed_id(spec: &str, default_secs: u32) -> (u32, String) {
    match spec.split_once(':') {
        Some((secs, id)) => (secs.trim().parse().unwrap_or(default_secs), id.to_string()),
        None => (default_secs, spec.to_string()),
    }
}

fn schedule_autoplay(app: &Rc<App>, tag: &'static str, spec: &str, default_secs: u32) {
    let (delay, id) = delayed_id(spec, default_secs);
    let app = app.clone();
    tracing::info!("[test] {tag} {id} scheduled at +{delay}s");
    gtk::glib::timeout_add_seconds_local_once(delay, move || {
        tracing::info!("[test] {tag} activating {id}");
        app.activate(ResultItem::for_video_id(&id));
    });
}

/// The shorts, autoplay, autoplay-pip, autoplay2, autoplay3, search and playlist hooks.
pub(super) fn wire_startup_hooks(app: &Rc<App>) {
    let playlist_view = app.playlist_view().clone();
    {
        let app = app.clone();
        hooks::on("shorts", move |v| {
            let Ok(secs) = v.parse::<u32>() else { return };
            let app = app.clone();
            gtk::glib::timeout_add_seconds_local_once(secs, move || app.open_shorts(None));
        });
    }
    {
        let app = app.clone();
        hooks::on("autoplay", move |id| {
            let (app, id) = (app.clone(), id.to_string());
            gtk::glib::timeout_add_seconds_local_once(1, move || app.activate(ResultItem::for_video_id(&id)));
        });
    }
    {
        let app = app.clone();
        hooks::on("autoplay-pip", move |id| {
            let (app, id) = (app.clone(), id.to_string());
            gtk::glib::timeout_add_seconds_local_once(1, move || app.play_mini(ResultItem::for_video_id(&id)));
        });
    }
    {
        let app = app.clone();
        hooks::on("autoplay2", move |spec| schedule_autoplay(&app, "autoplay2", spec, 60));
    }
    {
        let app = app.clone();
        hooks::on("autoplay3", move |spec| schedule_autoplay(&app, "autoplay3", spec, 90));
    }
    {
        let app = app.clone();
        hooks::on("search", move |q| {
            let (app, q) = (app.clone(), q.to_string());
            gtk::glib::timeout_add_seconds_local_once(1, move || app.run_search(q));
        });
    }
    {
        let app = app.clone();
        hooks::on("playlist", move |pid| {
            let (app, pid) = (app.clone(), pid.to_string());
            gtk::glib::timeout_add_seconds_local_once(1, move || app.open_playlist(pid, String::new()));
            if let Some(after) = hooks::get("playlist-play").and_then(|v| v.parse().ok()) {
                let playlist_view = playlist_view.clone();
                gtk::glib::timeout_add_seconds_local_once(after, move || {
                    tracing::info!("[test] playlist play-all");
                    playlist_view.play_all();
                });
            }
        });
    }
}
