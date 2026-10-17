//! SponsorBlock in the player: the seek-bar timeline and the category toggles for Settings.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{Box as GtkBox, CheckButton, DrawingArea, Label};

use crate::sponsorblock::Segment;

/// Builds SponsorBlock category toggles into `container`, for the Settings popover. `enabled`
/// is the initial set; `on_change(new_set)` fires on each toggle so the caller can persist it.
/// Categories follow `sponsorblock::ALL_CATEGORIES`.
pub fn build_sponsorblock_toggles<F: Fn(Vec<String>) + 'static>(
    container: &GtkBox,
    enabled: &[String],
    on_change: F,
) {
    let header = Label::builder()
        .label("SponsorBlock — auto-skip")
        .css_classes(["dim-label", "caption"])
        .xalign(0.0)
        .margin_top(8)
        .build();
    container.append(&header);
    let state = Rc::new(RefCell::new(enabled.to_vec()));
    let on_change = Rc::new(on_change);
    for (id, label) in crate::sponsorblock::ALL_CATEGORIES {
        let check = CheckButton::with_label(label);
        check.set_active(enabled.iter().any(|c| c == id));
        let state = state.clone();
        let on_change = on_change.clone();
        let id = id.to_string();
        check.connect_toggled(move |c| {
            {
                let mut s = state.borrow_mut();
                s.retain(|x| x != &id);
                if c.is_active() {
                    s.push(id.clone());
                }
            }
            on_change(state.borrow().clone());
        });
        container.append(&check);
    }
}

/// The SponsorBlock timeline: colored bars for each segment, drawn over the seek bar.
pub(super) fn build_sponsor_timeline(sponsor_segs: &Rc<RefCell<Vec<Segment>>>, sponsor_dur: &Rc<Cell<f64>>) -> DrawingArea {
    let seg_draw = DrawingArea::new();
    seg_draw.set_can_target(false); // clicks pass through to the scale beneath
    seg_draw.set_hexpand(true);
    {
        let segs = sponsor_segs.clone();
        let segdur = sponsor_dur.clone();
        seg_draw.set_draw_func(move |_, cr, w, h| {
            let d = segdur.get();
            if d <= 0.0 {
                return;
            }
            // Inset ~ the slider handle's half-width so bars line up with the trough.
            let inset = 9.0_f64;
            let track_w = (w as f64 - 2.0 * inset).max(0.0);
            let bar_h = 4.0;
            let y = (h as f64 - bar_h) / 2.0;
            for s in segs.borrow().iter() {
                let x0 = inset + (s.start / d).clamp(0.0, 1.0) * track_w;
                let x1 = inset + (s.end / d).clamp(0.0, 1.0) * track_w;
                let (r, g, b) = crate::sponsorblock::category_color(&s.category);
                cr.set_source_rgba(r, g, b, 0.85);
                cr.rectangle(x0, y, (x1 - x0).max(1.0), bar_h);
                let _ = cr.fill();
            }
        });
    }
    seg_draw
}
