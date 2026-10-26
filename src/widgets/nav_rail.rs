//! Collapsible left navigation rail (YouTube-style): an icon rail by default that
//! expands to labelled rows via the ☰ toggle. Replaces the old header Home/Subs
//! switcher. Expanded state is persisted (crate::prefs).
use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Image, Label, MenuButton, Orientation};

pub struct NavRail {
    pub root: GtkBox,
    home: Button,
    shorts: Button,
    subs: Button,
    playlists: Button,
    home_label: Label,
    shorts_label: Label,
    subs_label: Label,
    playlists_label: Label,
    settings: MenuButton,
    expanded: Rc<Cell<bool>>,
}

/// One nav entry: an icon + a label in a horizontal box, styled flat.
fn nav_item(icon: &str) -> (Button, Label) {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let img = Image::from_icon_name(icon);
    let label = Label::new(None);
    label.set_xalign(0.0);
    row.append(&img);
    row.append(&label);
    let btn = Button::builder().child(&row).build();
    btn.add_css_class("flat");
    // hexpand/halign are set per expand-state in `render`.
    (btn, label)
}

impl Default for NavRail {
    fn default() -> Self {
        Self::new()
    }
}

impl NavRail {
    pub fn new() -> Self {
        let root = GtkBox::new(Orientation::Vertical, 4);
        root.add_css_class("navigation-sidebar");
        root.set_margin_top(6);
        root.set_margin_start(4);
        root.set_margin_end(4);
        // Prevent the buttons' hexpand=true (below) from propagating up and
        // making the rail compete with `stage` for width in the shell hbox.
        root.set_hexpand(false);

        let toggle = Button::from_icon_name("open-menu-symbolic");
        toggle.add_css_class("flat");
        toggle.set_halign(Align::Start);
        toggle.set_tooltip_text(Some("Menu"));
        root.append(&toggle);

        let (home, home_label) = nav_item("go-home-symbolic");
        home.set_tooltip_text(Some("Home"));
        let (shorts, shorts_label) = nav_item("media-playback-start-symbolic");
        shorts.set_tooltip_text(Some("Shorts"));
        let (subs, subs_label) = nav_item("folder-videos-symbolic");
        subs.set_tooltip_text(Some("Subscriptions"));
        let (playlists, playlists_label) = nav_item("view-list-symbolic");
        playlists.set_tooltip_text(Some("Playlists"));
        root.append(&home);
        root.append(&shorts);
        root.append(&subs);
        root.append(&playlists);

        // The spacer pushes the Settings button to the bottom of the rail, reachable on every
        // page including the player.
        let spacer = GtkBox::new(Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        root.append(&spacer);

        let note = Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Leave a note (Ctrl+Shift+F)")
            .action_name("win.leave-note")
            .build();
        note.add_css_class("flat");
        note.set_halign(Align::Start);
        root.append(&note);

        let settings = MenuButton::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Settings")
            .build();
        settings.add_css_class("flat");
        settings.set_halign(Align::Start);
        root.append(&settings);

        let expanded = Rc::new(Cell::new(crate::prefs::load_nav_expanded()));
        let rail = Self {
            root, home, shorts, subs, playlists, home_label, shorts_label, subs_label, playlists_label, settings,
            expanded,
        };
        rail.render();

        // Toggle flips + persists + re-renders.
        {
            let expanded = rail.expanded.clone();
            let r = rail.clone_shallow();
            toggle.connect_clicked(move |_| {
                let next = !expanded.get();
                expanded.set(next);
                crate::prefs::save_nav_expanded(next);
                r.render();
            });
        }
        rail
    }

    /// Cheap handle clone for the toggle closure (shares the same widgets).
    fn clone_shallow(&self) -> Self {
        Self {
            root: self.root.clone(), home: self.home.clone(), shorts: self.shorts.clone(), subs: self.subs.clone(),
            playlists: self.playlists.clone(),
            home_label: self.home_label.clone(), shorts_label: self.shorts_label.clone(),
            subs_label: self.subs_label.clone(),
            playlists_label: self.playlists_label.clone(),
            settings: self.settings.clone(), expanded: self.expanded.clone(),
        }
    }

    /// Shows or hides the labels and sizes the rail for the current expand state.
    /// Collapsed: icon-sized buttons, left-aligned. Expanded: buttons fill the width with icon
    /// and label.
    fn render(&self) {
        let e = self.expanded.get();
        self.home_label.set_text(if e { "Home" } else { "" });
        self.shorts_label.set_text(if e { "Shorts" } else { "" });
        self.subs_label.set_text(if e { "Subscriptions" } else { "" });
        self.playlists_label.set_text(if e { "Playlists" } else { "" });
        self.home_label.set_visible(e);
        self.shorts_label.set_visible(e);
        self.subs_label.set_visible(e);
        self.playlists_label.set_visible(e);
        for b in [&self.home, &self.shorts, &self.subs, &self.playlists] {
            b.set_hexpand(e);
            // Collapsed: shrink to icon size and left-align (a clean icon column that lines
            // up with the ☰ toggle above). Expanded: fill the width for icon+label rows.
            b.set_halign(if e { Align::Fill } else { Align::Start });
        }
        // Expanded: fixed 200px. Collapsed: natural width (-1) so the rail hugs the icons
        // instead of reserving a wider fixed column with empty space to their right.
        self.root.set_size_request(if e { 200 } else { -1 }, -1);
    }

    pub fn connect_home<F: Fn() + 'static>(&self, f: F) {
        self.home.connect_clicked(move |_| f());
    }
    pub fn connect_shorts<F: Fn() + 'static>(&self, f: F) {
        self.shorts.connect_clicked(move |_| f());
    }
    pub fn connect_subscriptions<F: Fn() + 'static>(&self, f: F) {
        self.subs.connect_clicked(move |_| f());
    }
    pub fn connect_playlists<F: Fn() + 'static>(&self, f: F) {
        self.playlists.connect_clicked(move |_| f());
    }

    /// The Settings gear pinned to the bottom of the rail. Reachable on every page
    /// (including while a video plays); its popover is assigned by the caller.
    pub fn settings_button(&self) -> &MenuButton {
        &self.settings
    }
}
