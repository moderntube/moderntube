//! Embedded WebKitGTK login window: sign into YouTube and capture the cookie header.
use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::gio;
use gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{LoadEvent, NetworkSession, WebView};

/// Spoof desktop Firefox so Google doesn't serve the embedded-webview block.
const SPOOF_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";
const LOGIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F";

/// Opens a modal login window over `parent`. Once a SAPISID cookie appears, `on_captured`
/// gets the `Cookie:` header and the window closes, before the YouTube homepage finishes
/// rendering. The session is ephemeral, so each login starts fresh.
pub fn open_login<F: Fn(String) + 'static>(parent: &impl IsA<gtk::Window>, on_captured: F) {
    let session = NetworkSession::new_ephemeral();
    let webview = WebView::builder().network_session(&session).build();
    webview.set_vexpand(true);
    webview.set_hexpand(true);
    let settings = webkit6::prelude::WebViewExt::settings(&webview).unwrap_or_default();
    settings.set_user_agent(Some(SPOOF_UA));
    webview.set_settings(&settings);
    webview.load_uri(LOGIN_URL);

    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Sign in to YouTube")
        .default_width(1100)
        .default_height(820)
        .child(&webview)
        .build();

    let done = Rc::new(Cell::new(false));
    let on_captured = Rc::new(on_captured);
    let window_weak = window.downgrade();
    webview.connect_load_changed(move |wv, event| {
        if done.get() || !matches!(event, LoadEvent::Committed | LoadEvent::Finished) {
            return;
        }
        if !wv.uri().unwrap_or_default().contains("youtube.com") {
            return;
        }
        let Some(manager) = wv.network_session().and_then(|s| s.cookie_manager()) else {
            return;
        };
        let done = done.clone();
        let on_captured = on_captured.clone();
        let window_weak = window_weak.clone();
        manager.cookies("https://www.youtube.com", gio::Cancellable::NONE, move |res| {
            let Ok(mut cookies) = res else { return };
            let mut pairs = Vec::with_capacity(cookies.len());
            let mut has_sapisid = false;
            for c in cookies.iter_mut() {
                let name = c.name().unwrap_or_default();
                let value = c.value().unwrap_or_default();
                if name == "SAPISID" || name == "__Secure-3PAPISID" {
                    has_sapisid = true;
                }
                pairs.push(format!("{name}={value}"));
            }
            if !has_sapisid || done.replace(true) {
                return;
            }
            on_captured(pairs.join("; "));
            // Close from an idle callback: destroying the WebView inside its own
            // cookie-manager callback is a re-entrant teardown that crashes the process.
            let window_weak = window_weak.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(window) = window_weak.upgrade() {
                    window.close();
                }
            });
        });
    });

    window.present();
}
