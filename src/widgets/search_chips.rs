//! The chip row above the search results (All, Shorts, Videos, ...), as the web client's
//! search header shows it.
use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Orientation, PolicyType, ScrolledWindow, ToggleButton};
use innertube::SearchChip;

pub struct SearchChips {
    pub root: ScrolledWindow,
    row: GtkBox,
    chips: RefCell<Vec<SearchChip>>,
    on_pick: Rc<dyn Fn(String)>,
}

impl SearchChips {
    /// `on_pick(token)` loads the page behind a chip's continuation token.
    pub fn new(on_pick: Rc<dyn Fn(String)>) -> Self {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.set_margin_start(12);
        row.set_margin_end(12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        let root = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::External)
            .vscrollbar_policy(PolicyType::Never)
            .child(&row)
            .visible(false)
            .build();
        Self { root, row, chips: RefCell::default(), on_pick }
    }

    /// Shows the last chips again, after a chip's page failed to load.
    pub fn restore(&self) {
        let chips = self.chips.borrow().clone();
        self.set_chips(&chips);
    }

    /// Replaces the chips; an empty list hides the row.
    pub fn set_chips(&self, chips: &[SearchChip]) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        for chip in chips {
            let button = ToggleButton::builder().label(&chip.label).active(chip.selected).build();
            button.add_css_class("search-chip");
            match chip.token.clone() {
                Some(token) => {
                    let on_pick = self.on_pick.clone();
                    button.connect_clicked(move |b| {
                        b.set_sensitive(false);
                        on_pick(token.clone());
                    });
                }
                None => button.set_can_target(false),
            }
            self.row.append(&button);
        }
        self.root.set_visible(!chips.is_empty());
        *self.chips.borrow_mut() = chips.to_vec();
    }
}
