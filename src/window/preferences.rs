//! The Preferences window over [`crate::settings`] and [`crate::api_keys`], opened by
//! `app.preferences`.
use adw::prelude::*;
use gtk4 as gtk;
use gtk::gio;

use crate::api_keys;
use crate::runtime::AsyncRuntime;
use crate::settings::{self, Endpoint};

/// Registers `app.preferences` (Ctrl+,) to present the window over `parent`.
pub(super) fn wire_preferences(application: &adw::Application, parent: &adw::ApplicationWindow, rt: AsyncRuntime) {
    let action = gio::SimpleAction::new("preferences", None);
    let parent = parent.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(parent) = parent.upgrade() {
            build(&parent, &rt).present();
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

fn build(parent: &adw::ApplicationWindow, rt: &AsyncRuntime) -> adw::PreferencesWindow {
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
        .description("An OpenAI-compatible endpoint writes summaries and answers Ask. Off until a base URL is set. API keys are kept in the system keyring.")
        .build();
    for row in endpoint_rows(&s.catchup.summarizer, |s| &mut s.catchup.summarizer) {
        catchup.add(&row);
    }
    catchup.add(&key_row(rt, "API key", api_keys::SUMMARIZER.to_string()));
    let fallback = adw::ExpanderRow::builder()
        .title("Fallback endpoint")
        .subtitle("Tried when every base URL above fails")
        .build();
    for row in endpoint_rows(&s.catchup.fallback, |s| &mut s.catchup.fallback) {
        fallback.add_row(&row);
    }
    fallback.add_row(&key_row(rt, "API key", api_keys::FALLBACK.to_string()));
    catchup.add(&fallback);
    let stt = adw::ExpanderRow::builder()
        .title("Transcription providers")
        .subtitle(format!(
            "{} configured. Edit [[catchup.stt]] in {}",
            s.catchup.stt.len(),
            settings::path().map(|p| p.display().to_string()).unwrap_or_else(|| "settings.toml".into())
        ))
        .build();
    for p in &s.catchup.stt {
        stt.add_row(&key_row(rt, &format!("{} API key", p.name), api_keys::stt_slot(&p.name)));
    }
    catchup.add(&stt);
    page.add(&catchup);

    let win = adw::PreferencesWindow::builder().transient_for(parent).modal(true).search_enabled(false).build();
    win.add(&page);
    win
}

fn toast(row: &impl IsA<gtk::Widget>, msg: &str) {
    if let Some(win) = row.root().and_downcast::<adw::PreferencesWindow>() {
        win.add_toast(adw::Toast::new(msg));
    }
}

/// Saves a change; a failure is toasted on the window that holds `row`.
fn save(row: &impl IsA<gtk::Widget>, f: impl FnOnce(&mut settings::Settings)) {
    if settings::update(f).is_err() {
        toast(row, "Setting not saved: check settings.toml");
    }
}

/// Model and base URL rows; each saves on Enter or the apply button.
fn endpoint_rows(e: &Endpoint, field: fn(&mut settings::Settings) -> &mut Endpoint) -> [adw::EntryRow; 2] {
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
    ]
}

/// The keyring key for `slot`. It fills in once the keyring answers; applying an empty
/// field deletes the stored key.
fn key_row(rt: &AsyncRuntime, title: &str, slot: String) -> adw::PasswordEntryRow {
    let r = adw::PasswordEntryRow::builder().title(title).show_apply_button(true).build();
    let weak = r.downgrade();
    let s = slot.clone();
    rt.spawn(async move { api_keys::load(&s).await }, move |key| {
        if let (Some(r), Some(k)) = (weak.upgrade(), key) {
            r.set_text(&k);
        }
    });
    let rt = rt.clone();
    r.connect_apply(move |r| {
        let (key, slot, weak) = (r.text().trim().to_string(), slot.clone(), r.downgrade());
        rt.spawn(async move { api_keys::save(&slot, &key).await }, move |res| {
            if let (Err(e), Some(r)) = (res, weak.upgrade()) {
                toast(&r, &format!("API key not saved: {e}"));
            }
        });
    });
    r
}
