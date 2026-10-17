//! SponsorBlock: the Settings toggles and the skip poll.
use std::cell::RefCell;
use std::rc::Rc;

use adw::Toast;
use gtk4 as gtk;

use super::app::App;

/// The SponsorBlock category toggles in Settings and the 400 ms segment-skip poll.
pub(super) fn wire_sponsorblock(app: &Rc<App>) {
    let App { rt, engine, player, toasts, current_video, current_is_live, settings_box, .. } = &**app;
    // On each VOD load, fetch skip segments for the enabled categories. A poll seeks past any
    // segment the playhead enters, and the segments are drawn on the seek bar. Live has no
    // fixed segments. Categories are set in Settings and persisted.
    let sponsor_cats = Rc::new(RefCell::new(crate::prefs::load_sponsor_categories()));
    {
        let sponsor_cats_cb = sponsor_cats.clone();
        crate::widgets::player_controls::build_sponsorblock_toggles(
            settings_box,
            &sponsor_cats.borrow(),
            move |new_cats| {
                *sponsor_cats_cb.borrow_mut() = new_cats.clone();
                crate::prefs::save_sponsor_categories(&new_cats); // applies to the next video load
            },
        );
    }
    {
        let engine = engine.clone();
        let current_video = current_video.clone();
        let current_is_live = current_is_live.clone();
        let rt = rt.clone();
        let toasts = toasts.clone();
        let player_sb = player.clone();
        let sponsor_cats = sponsor_cats.clone();
        let segments: Rc<RefCell<Vec<crate::sponsorblock::Segment>>> = Rc::new(RefCell::new(Vec::new()));
        let loaded_for: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            let vid = current_video.borrow().clone();
            // (Re)fetch when the loaded video changes (VOD only).
            if vid != *loaded_for.borrow() {
                loaded_for.replace(vid.clone());
                segments.borrow_mut().clear();
                player_sb.controls.set_sponsor_segments(Vec::new()); // clear the old bars
                if let (Some(id), false) = (vid.clone(), current_is_live.get()) {
                    let segs = segments.clone();
                    let player_done = player_sb.clone();
                    let cats = sponsor_cats.borrow().clone();
                    rt.spawn(
                        async move { crate::sponsorblock::fetch_segments(&id, &cats).await },
                        move |found: Vec<crate::sponsorblock::Segment>| {
                            *segs.borrow_mut() = found.clone();
                            player_done.controls.set_sponsor_segments(found); // paint the bars
                        },
                    );
                }
            }
            // Skip if the playhead is inside a segment (VOD only).
            if vid.is_some() && !current_is_live.get() {
                let pos = engine.time_pos();
                let target = segments
                    .borrow()
                    .iter()
                    .find(|s| pos >= s.start && pos < s.end - 0.5)
                    .map(|s| s.end);
                if let Some(end) = target {
                    engine.seek_absolute(end);
                    toasts.add_toast(Toast::new("Skipped segment"));
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
}
