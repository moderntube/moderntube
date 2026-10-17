//! Signing in and out, and restoring the stored session at startup.
use std::rc::Rc;

use adw::prelude::*;
use adw::{ApplicationWindow, Toast};
use gtk4 as gtk;
use gtk::gio;

use super::app::App;
use super::FeedKind;

/// The Sign out action and the startup restore of the stored session, followed by the
/// first home-feed load.
pub(super) fn wire_session(app: &Rc<App>, window: &ApplicationWindow) {
    let App { rt, toasts, feed_kind, .. } = &**app;
    // Sign out: clear the stored cookie and revert to an anonymous session.
    let logout = gio::SimpleAction::new("logout", None);
    {
        let rt = rt.clone();
        let toasts = toasts.clone();
        let app = app.clone();
        logout.connect_activate(move |_, _| {
            rt.spawn(async { crate::auth::store::clear().await }, |_| {});
            if swap_session(&app, None) {
                app.set_logged_in(false, None);
                toasts.add_toast(Toast::new("Signed out"));
            }
        });
    }
    window.add_action(&logout);

    // Restore the stored session, then do the initial home load. `store::load()` waits on the
    // keyring: a locked keyring shows an unlock prompt, and a fetch before it resolves would run
    // anonymously. A dismissed prompt or no stored cookie resolves to None and the load
    // proceeds anonymously.
    {
        let app = app.clone();
        let feed_kind = feed_kind.clone();
        rt.spawn(
            async { crate::auth::store::load().await },
            move |cookie: Option<String>| {
                if let Some(header) = cookie.filter(|h| crate::auth::cookie_is_usable(h)) {
                    if swap_session(&app, Some(&header)) {
                        app.set_logged_in(true, Some("Account".into()));
                    }
                }
                // Only load if the user hasn't already navigated somewhere themselves
                // (e.g. searched while the keyring prompt was up).
                if feed_kind.get() == FeedKind::Home {
                    app.load_feed(FeedKind::Home);
                }
            },
        );
    }
}

/// The Sign in button: opens the login window and adopts the cookie it returns.
pub(super) fn wire_sign_in(app: &Rc<App>) {
    app.sign_in.connect_clicked({
        let app = app.clone();
        move |_| {
            let Some(parent) = app.window.get().cloned() else { return };
            let app = app.clone();
            crate::auth::login::open_login(&parent, move |header: String| {
                let header_for_store = header.clone();
                app.rt.spawn(async move { crate::auth::store::save(&header_for_store).await }, |_| {});
                if swap_session(&app, Some(&header)) {
                    app.set_logged_in(true, Some("Account".into()));
                    app.toasts.add_toast(Toast::new("Signed in"));
                }
            });
        }
    });
}

/// Replace the app's session. On failure the current one stays and a toast says why.
pub(super) fn swap_session(app: &App, cookie: Option<&str>) -> bool {
    match crate::auth::build_session(cookie) {
        Ok(s) => {
            *app.session.borrow_mut() = s;
            true
        }
        Err(e) => {
            tracing::warn!("[auth] could not build a session: {e}");
            app.toasts.add_toast(Toast::new(&format!("Could not start a session: {e}")));
            false
        }
    }
}
