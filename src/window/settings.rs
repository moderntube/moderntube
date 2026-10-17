//! The Settings popover content and its attachment to the nav rail.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::glib;
use gtk::{Box as GtkBox, Button, MenuButton, Orientation, Popover};

use innertube::Session;
use crate::widgets::PlayerView;

use super::appearance::build_appearance_content;

/// The Settings content: Account, Appearance, Captions and Behavior rows.
pub(super) fn build_settings_box(
    prefs_store: &crate::prefs::PrefsStore,
    player: &PlayerView,
    session: &Rc<RefCell<Arc<Session>>>,
    sign_in: &Button,
    account: &MenuButton,
) -> GtkBox {
    let settings_box = GtkBox::new(Orientation::Vertical, 10);
    settings_box.set_margin_top(12);
    settings_box.set_margin_bottom(12);
    settings_box.set_margin_start(12);
    settings_box.set_margin_end(12);
    settings_box.set_width_request(260);
    settings_box.append(&gtk::Label::builder().label("Account").xalign(0.0).css_classes(["heading"]).build());
    settings_box.append(sign_in);
    settings_box.append(account);
    settings_box.append(&gtk::Separator::new(Orientation::Horizontal));
    settings_box
        .append(&gtk::Label::builder().label("Appearance").xalign(0.0).css_classes(["heading"]).build());
    settings_box.append(&build_appearance_content(prefs_store.clone()));
    settings_box.append(&gtk::Separator::new(Orientation::Horizontal));
    settings_box
        .append(&gtk::Label::builder().label("Captions").xalign(0.0).css_classes(["heading"]).build());
    {
        let captions = player.captions.clone();
        let cur = captions.prefs();
        let row = |label: &str, w: &gtk::Widget| {
            let row = GtkBox::new(Orientation::Horizontal, 8);
            row.append(&gtk::Label::builder().label(label).xalign(0.0).wrap(true).hexpand(true).build());
            w.set_valign(gtk::Align::Center);
            row.append(w);
            row
        };
        let lang = gtk::Entry::builder()
            .text(&cur.lang)
            .width_chars(6)
            .tooltip_text("Language code, e.g. en, es, pt-BR")
            .build();
        {
            let captions = captions.clone();
            lang.connect_activate(move |e| captions.set_lang(e.text().trim()));
        }
        settings_box.append(&row("Language", lang.upcast_ref()));
        let size = gtk::SpinButton::with_range(50.0, 250.0, 10.0);
        size.set_value((cur.scale * 100.0).round());
        {
            let captions = captions.clone();
            size.connect_value_changed(move |s| captions.set_scale(s.value() / 100.0));
        }
        settings_box.append(&row("Text size (%)", size.upcast_ref()));
        let bg = gtk::SpinButton::with_range(0.0, 100.0, 5.0);
        bg.set_value((cur.bg_opacity * 100.0).round());
        {
            let captions = captions.clone();
            bg.connect_value_changed(move |s| captions.set_bg_opacity(s.value() / 100.0));
        }
        settings_box.append(&row("Background opacity (%)", bg.upcast_ref()));
        captions.connect_loaded(move |p| {
            lang.set_text(&p.lang);
            size.set_value((p.scale * 100.0).round());
            bg.set_value((p.bg_opacity * 100.0).round());
        });
    }
    settings_box.append(&gtk::Separator::new(Orientation::Horizontal));
    settings_box
        .append(&gtk::Label::builder().label("Behavior").xalign(0.0).css_classes(["heading"]).build());
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Return to results when the queue ends")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().auto_return);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            sw.connect_active_notify(move |sw| prefs_store.set_auto_return(sw.is_active()));
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    // Sidebar hover preview. Off shows the animated image the reference client shows there;
    // on opens a streaming preview, which costs a player session per hover.
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Stream sidebar hover previews (instead of an animated thumbnail)")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().sidebar_streaming_preview);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            sw.connect_active_notify(move |sw| {
                prefs_store.set_sidebar_streaming_preview(sw.is_active())
            });
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Prevent ad loads (WIP)")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().direct_opens);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            sw.connect_active_notify(move |sw| {
                prefs_store.set_direct_opens(sw.is_active());
                crate::playback::common::set_direct_opens(sw.is_active());
            });
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Play videos signed in")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().play_signed_in);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            let session = session.clone();
            sw.connect_active_notify(move |sw| {
                prefs_store.set_play_signed_in(sw.is_active());
                let account = sw.is_active().then(|| session.borrow().clone());
                crate::playback::set_playback_account(account.as_deref());
            });
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Show Shorts on Home")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().home_shorts);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            sw.connect_active_notify(move |sw| prefs_store.set_home_shorts(sw.is_active()));
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Add watched videos to your YouTube history (signed in)")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let sw = gtk::Switch::new();
        sw.set_active(prefs_store.get().report_watches);
        sw.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            sw.connect_active_notify(move |sw| prefs_store.set_report_watches(sw.is_active()));
        }
        row.append(&sw);
        settings_box.append(&row);
    }
    {
        use innertube::PotBackendKind;
        const KINDS: [PotBackendKind; 2] = [PotBackendKind::BgUtil, PotBackendKind::Native];
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("PO-token minter (applies to the next video)")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let dd = gtk::DropDown::from_strings(&["bgutil sidecar (:4416)", "Built-in (no sidecar)"]);
        let current = PotBackendKind::parse(&prefs_store.get().pot_backend).unwrap_or_default();
        dd.set_selected(KINDS.iter().position(|k| *k == current).unwrap_or(0) as u32);
        dd.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            dd.connect_selected_notify(move |dd| {
                let kind = KINDS.get(dd.selected() as usize).copied().unwrap_or_default();
                prefs_store.set_pot_backend(kind);
                crate::playback::set_pot_backend(kind);
            });
        }
        row.append(&dd);
        settings_box.append(&row);
    }
    // Catch-up cap: how many hours before the join Catch me up summarizes.
    {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label("Catch-me-up limit (hours before join)")
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        // 0.5–12h in half-hour steps.
        let spin = gtk::SpinButton::with_range(0.5, 12.0, 0.5);
        spin.set_digits(1);
        spin.set_value(prefs_store.get().catchup_cap_hours);
        spin.set_valign(gtk::Align::Center);
        {
            let prefs_store = prefs_store.clone();
            spin.connect_value_changed(move |s| prefs_store.set_catchup_cap_hours(s.value()));
        }
        row.append(&spin);
        settings_box.append(&row);
    }
    settings_box.append(&Button::builder().label("Preferences…").action_name("app.preferences").build());
    settings_box.append(&gtk::Separator::new(Orientation::Horizontal));
    settings_box
}

/// Hangs the Settings content off the nav rail's gear in a bounded, scrolled popover.
pub(super) fn attach_settings_popover(nav_rail: &crate::widgets::NavRail, settings_box: &GtkBox) {
    let settings_popover = Popover::new();
    // A popover taller than the room above the gear closes as soon as it opens.
    let settings_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(settings_box)
        .build();
    settings_popover.set_child(Some(&settings_scroll));
    nav_rail.settings_button().set_popover(Some(&settings_popover));
    settings_popover.connect_visible_notify(|p| {
        // Wall-clock ms separate a close inside the click handler from a later one.
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        tracing::info!(
            "[settings] popover {} t={ms} parent_mapped={} child={}",
            if p.is_visible() { "shown" } else { "hidden" },
            p.parent().map(|w| w.is_mapped()).unwrap_or(false),
            p.child().is_some(),
        );
    });
    // A popover needs a seat to grab, which a headless compositor lacks, so the hook shows
    // the same content in a plain window.
    crate::hooks::on("open-settings", move |_| {
        if settings_popover.child().is_none() {
            return;
        }
        settings_popover.set_child(None::<&gtk::Widget>);
        let win = gtk::Window::builder()
            .title("Settings")
            .default_width(520)
            .default_height(640)
            .child(&settings_scroll)
            .build();
        glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            win.present();
        });
    });
}
