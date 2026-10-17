//! The Preferences window over [`crate::settings`], opened by `app.preferences`.
use adw::prelude::*;
use gtk4 as gtk;
use gtk::gio;

use crate::settings::{self, Endpoint};

/// Registers `app.preferences` (Ctrl+,) to present the window over `parent`.
pub(super) fn wire_preferences(application: &adw::Application, parent: &adw::ApplicationWindow) {
    let action = gio::SimpleAction::new("preferences", None);
    let parent = parent.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(parent) = parent.upgrade() {
            build(&parent).present();
        }
    });
    application.add_action(&action);
    application.set_accels_for_action("app.preferences", &["<Control>comma"]);
}

/// The app menu for a header bar.
pub(super) fn primary_menu() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("app.preferences"));
    gtk::MenuButton::builder().icon_name("open-menu-symbolic").tooltip_text("Main menu").menu_model(&menu).build()
}

fn build(parent: &adw::ApplicationWindow) -> adw::PreferencesWindow {
    let s = settings::get();
    let page = adw::PreferencesPage::new();

    let playback = adw::PreferencesGroup::builder().title("Playback").build();
    let hover = adw::SwitchRow::builder()
        .title("Video previews on hover")
        .subtitle("Applies to cards shown after the change")
        .active(s.playback.hover_previews)
        .build();
    hover.connect_active_notify(|r| {
        let on = r.is_active();
        save(r, |s| s.playback.hover_previews = on);
    });
    playback.add(&hover);
    let av1 = adw::SwitchRow::builder()
        .title("AV1 video")
        .subtitle("Used when a hardware AV1 decoder is installed. Applies to the next video.")
        .active(s.playback.av1)
        .build();
    av1.connect_active_notify(|r| {
        let on = r.is_active();
        save(r, |s| s.playback.av1 = on);
    });
    playback.add(&av1);
    page.add(&playback);

    let catchup = adw::PreferencesGroup::builder()
        .title("Catch me up")
        .description("An OpenAI-compatible endpoint writes summaries and answers Ask. Off until a base URL is set.")
        .build();
    add_endpoint_rows(&catchup, &s.catchup.summarizer, |s| &mut s.catchup.summarizer);
    let fallback = adw::ExpanderRow::builder()
        .title("Fallback endpoint")
        .subtitle("Tried when every base URL above fails")
        .build();
    for row in endpoint_rows(&s.catchup.fallback, |s| &mut s.catchup.fallback) {
        fallback.add_row(&row);
    }
    catchup.add(&fallback);
    let stt = adw::ActionRow::builder()
        .title("Transcription providers")
        .subtitle(format!(
            "{} configured. Edit [[catchup.stt]] in {}",
            s.catchup.stt.len(),
            settings::path().map(|p| p.display().to_string()).unwrap_or_else(|| "settings.toml".into())
        ))
        .subtitle_selectable(true)
        .build();
    catchup.add(&stt);
    page.add(&catchup);

    let win = adw::PreferencesWindow::builder().transient_for(parent).modal(true).search_enabled(false).build();
    win.add(&page);
    win
}

/// Saves a change; a failure is toasted on the window that holds `row`.
fn save(row: &impl IsA<gtk::Widget>, f: impl FnOnce(&mut settings::Settings)) {
    if settings::update(f).is_err() {
        if let Some(win) = row.root().and_downcast::<adw::PreferencesWindow>() {
            win.add_toast(adw::Toast::new("Setting not saved: check settings.toml"));
        }
    }
}

fn add_endpoint_rows(group: &adw::PreferencesGroup, e: &Endpoint, field: fn(&mut settings::Settings) -> &mut Endpoint) {
    for row in endpoint_rows(e, field) {
        group.add(&row);
    }
}

/// Model, base URL and key-variable rows; each saves on Enter or the apply button.
fn endpoint_rows(e: &Endpoint, field: fn(&mut settings::Settings) -> &mut Endpoint) -> [adw::EntryRow; 3] {
    let row = |title: &str, text: &str, set: fn(&mut Endpoint, String)| {
        let r = adw::EntryRow::builder().title(title).text(text).show_apply_button(true).build();
        r.connect_apply(move |r| {
            let v = r.text().trim().to_string();
            save(r, |s| set(field(s), v));
        });
        r
    };
    [
        row("Model", &e.model, |e, v| e.model = v),
        row("API base URLs (comma-separated)", &e.api_base, |e, v| e.api_base = v),
        row("API key environment variable", &e.api_key_env, |e, v| e.api_key_env = v),
    ]
}
