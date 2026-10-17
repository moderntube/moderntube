use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::gio;
use gtk::{Box as GtkBox, Button, Orientation, SelectionMode};

/// Appearance settings content: a theme dropdown (System/Light/Dark/OLED) and an accent
/// (preset swatches plus a custom color picker). Changes go through the [`PrefsStore`], whose
/// subscribers persist them and re-apply the theme. Returns the content box for the Settings
/// popover (`window::settings`).
pub(super) fn build_appearance_content(prefs: crate::prefs::PrefsStore) -> GtkBox {
    use gtk::{ColorDialog, DropDown, FlowBox};
    let col = GtkBox::new(Orientation::Vertical, 10);

    // Current state (start from the store's snapshot).
    let snap = prefs.get();
    let mode0 = crate::theme::ThemeMode::from_str(&snap.theme_mode);
    let accent0 = snap.accent;
    let cur_accent = Rc::new(RefCell::new(accent0));
    let cur_mode = Rc::new(Cell::new(mode0));

    // Theme dropdown.
    col.append(&gtk::Label::builder().label("Theme").xalign(0.0).build());
    let modes = ["System", "Light", "Dark", "Dark · OLED"];
    let dd = DropDown::from_strings(&modes);
    dd.set_selected(match mode0 {
        crate::theme::ThemeMode::System => 0,
        crate::theme::ThemeMode::Light => 1,
        crate::theme::ThemeMode::Dark => 2,
        crate::theme::ThemeMode::Oled => 3,
    });
    col.append(&dd);

    // Accent swatches.
    col.append(&gtk::Label::builder().label("Accent").xalign(0.0).margin_top(6).build());
    let presets = ["#cc0000", "#3b82f6", "#22a55b", "#8b5cf6", "#f59e0b", "#ec4899", "#14b8a6"];
    // GTK4 has no per-widget CssProvider API (StyleContext::add_provider was removed in
    // GTK4; only the display-wide `style_context_add_provider_for_display` remains), so
    // each preset gets a stable `.swatch-N` class in one shared, display-wide provider.
    // `.swatch-selected` draws a ring on the active swatch so the current accent is visible.
    let swatch_css: String = presets
        .iter()
        .enumerate()
        .map(|(i, hex)| format!(".swatch-{i} {{ background: {hex}; }}"))
        .chain(std::iter::once(
            ".swatch-selected { box-shadow: inset 0 0 0 2px rgba(255,255,255,0.92), \
             0 0 0 2px rgba(0,0,0,0.55); }"
                .to_string(),
        ))
        .collect();
    let swatch_provider = gtk::CssProvider::new();
    swatch_provider.load_from_string(&swatch_css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &swatch_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let flow = FlowBox::builder()
        .max_children_per_line(7)
        .selection_mode(SelectionMode::None)
        .build();
    let apply = {
        let prefs = prefs.clone();
        let cur_accent = cur_accent.clone();
        let cur_mode = cur_mode.clone();
        Rc::new(move || {
            let m = cur_mode.get();
            let a = cur_accent.borrow().clone();
            // The store's subscribers persist and restyle.
            prefs.set_appearance(m.as_str(), &a);
        })
    };
    // Build the swatches, then a `select` helper that moves the ring to one (or none, when
    // a custom color that isn't a preset is chosen).
    let swatches: Rc<Vec<Button>> = Rc::new(
        (0..presets.len())
            .map(|i| {
                let sw = Button::new();
                sw.set_size_request(28, 28);
                sw.add_css_class("circular");
                sw.add_css_class(&format!("swatch-{i}"));
                flow.append(&sw);
                sw
            })
            .collect(),
    );
    let select: Rc<dyn Fn(Option<usize>)> = {
        let swatches = swatches.clone();
        Rc::new(move |sel: Option<usize>| {
            for (i, sw) in swatches.iter().enumerate() {
                if Some(i) == sel {
                    sw.add_css_class("swatch-selected");
                } else {
                    sw.remove_css_class("swatch-selected");
                }
            }
        })
    };
    for (i, hex) in presets.into_iter().enumerate() {
        let apply = apply.clone();
        let cur_accent = cur_accent.clone();
        let select = select.clone();
        swatches[i].connect_clicked(move |_| {
            *cur_accent.borrow_mut() = hex.to_string();
            apply();
            select(Some(i));
        });
    }
    // Ring the swatch matching the currently-saved accent (none if it's a custom color).
    let cur = cur_accent.borrow().clone();
    select(presets.iter().position(|&h| h == cur.as_str()));
    col.append(&flow);

    let custom = Button::with_label("Custom…");
    {
        let apply = apply.clone();
        let cur_accent = cur_accent.clone();
        let select = select.clone();
        custom.connect_clicked(move |btn| {
            let dialog = ColorDialog::new();
            let apply = apply.clone();
            let cur_accent = cur_accent.clone();
            let select = select.clone();
            let parent = btn.root().and_downcast::<gtk::Window>();
            dialog.choose_rgba(parent.as_ref(), None, gio::Cancellable::NONE, move |res| {
                if let Ok(rgba) = res {
                    let hex = format!(
                        "#{:02x}{:02x}{:02x}",
                        (rgba.red() * 255.0) as u8,
                        (rgba.green() * 255.0) as u8,
                        (rgba.blue() * 255.0) as u8
                    );
                    *cur_accent.borrow_mut() = crate::theme::normalize_accent(&hex);
                    apply();
                    select(None); // a custom color is not a preset: clear the ring
                }
            });
        });
    }
    col.append(&custom);

    // Dropdown change → apply.
    {
        let apply = apply.clone();
        let cur_mode = cur_mode.clone();
        dd.connect_selected_notify(move |dd| {
            cur_mode.set(match dd.selected() {
                1 => crate::theme::ThemeMode::Light,
                2 => crate::theme::ThemeMode::Dark,
                3 => crate::theme::ThemeMode::Oled,
                _ => crate::theme::ThemeMode::System,
            });
            apply();
        });
    }

    col
}
