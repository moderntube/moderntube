//! The results header's "Filters" button: a popover with the search response's filter
//! dialog, one column per group, as the web client's search filter dialog lays it out.
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, MenuButton, Orientation, Popover};
use innertube::SearchFilterGroup;

pub struct SearchFilters {
    /// The header button to place in a toolbar. Owns the popover.
    pub button: MenuButton,
    popover: Popover,
    columns: GtkBox,
    on_pick: Rc<dyn Fn(Option<String>)>,
}

impl SearchFilters {
    /// `on_pick(params)` searches the current query again with a picked option's `params`.
    pub fn new(on_pick: Rc<dyn Fn(Option<String>)>) -> Self {
        let columns = GtkBox::new(Orientation::Horizontal, 24);
        columns.set_margin_start(12);
        columns.set_margin_end(12);
        columns.set_margin_top(8);
        columns.set_margin_bottom(12);
        let popover = Popover::builder().child(&columns).build();
        let button = MenuButton::builder()
            .label("Filters")
            .tooltip_text("Search filters")
            .popover(&popover)
            .visible(false)
            .build();
        Self { button, popover, columns, on_pick }
    }

    /// Rebuilds the popover from a first search page's dialog; an empty dialog hides the button.
    /// `filtered` marks the button for a search that ran with filter params.
    pub fn set_groups(&self, groups: &[SearchFilterGroup], filtered: bool) {
        while let Some(child) = self.columns.first_child() {
            self.columns.remove(&child);
        }
        for group in groups {
            let column = GtkBox::new(Orientation::Vertical, 2);
            let title = Label::builder().label(&group.title).xalign(0.0).css_classes(["heading"]).build();
            title.set_margin_bottom(6);
            column.append(&title);
            for filter in &group.filters {
                let label = Label::builder().label(&filter.label).xalign(0.0).build();
                let option = Button::builder().child(&label).css_classes(["flat", "search-filter"]).build();
                if filter.selected {
                    option.add_css_class("selected");
                }
                option.set_tooltip_text(filter.tooltip.as_deref());
                if filter.clickable {
                    let (popover, on_pick, params) = (self.popover.clone(), self.on_pick.clone(), filter.params.clone());
                    option.connect_clicked(move |_| {
                        popover.popdown();
                        on_pick(params.clone());
                    });
                } else {
                    option.set_can_target(false);
                }
                option.set_sensitive(!filter.disabled);
                column.append(&option);
            }
            self.columns.append(&column);
        }
        self.button.set_visible(!groups.is_empty());
        if filtered {
            self.button.add_css_class("search-filters-active");
        } else {
            self.button.remove_css_class("search-filters-active");
        }
    }
}
