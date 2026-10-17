//! Diagnostic probes: the main-loop stall heartbeat and the FPS counter.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::ApplicationWindow;
use gtk4 as gtk;

use super::app::App;

/// [`crate::diag::heartbeat`] logs main-loop stalls; [`crate::diag::fps`] logs frame rate
/// and, with the autoscroll hook, scrolls the results grid while it measures.
pub(super) fn wire_probes(app: &Rc<App>, window: &ApplicationWindow) {
    let results = app.results();
    // A stall over 80 ms starves the video render and audio.
    if crate::diag::heartbeat() {
        let last = Rc::new(Cell::new(std::time::Instant::now()));
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let now = std::time::Instant::now();
            let gap = now.duration_since(last.replace(now)).as_millis();
            if gap > 80 {
                tracing::info!("[loop] main-loop stall {gap}ms");
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    if crate::diag::fps() {
        let autoscroll = crate::hooks::flag("autoscroll");
        let results_sw = results.root.clone();
        let frames = Rc::new(Cell::new(0u32));
        let last_log = Rc::new(Cell::new(0i64));
        let last_frame = Rc::new(Cell::new(0i64));
        let max_gap = Rc::new(Cell::new(0i64));
        let dir = Rc::new(Cell::new(1.0f64));
        window.add_tick_callback(move |_w, clock| {
            let t = clock.frame_time(); // microseconds
            let lf = last_frame.replace(t);
            if lf != 0 {
                let gap = t - lf;
                if gap > max_gap.get() {
                    max_gap.set(gap);
                }
            }
            frames.set(frames.get() + 1);
            if last_log.get() == 0 {
                last_log.set(t);
            }
            if t - last_log.get() >= 1_000_000 {
                let secs = (t - last_log.get()) as f64 / 1e6;
                tracing::info!(
                    "[fps] {:.0} fps | worst frame gap {:.1}ms | autoscroll={autoscroll}",
                    frames.get() as f64 / secs,
                    max_gap.get() as f64 / 1000.0
                );
                frames.set(0);
                last_log.set(t);
                max_gap.set(0);
            }
            if autoscroll {
                let adj = results_sw.vadjustment();
                // Oscillate over the first 2000 px of loaded rows; the bottom triggers
                // pagination.
                let upper = (adj.upper() - adj.page_size()).min(2000.0);
                if upper > 1.0 {
                    let mut v = adj.value() + dir.get() * 22.0;
                    if v >= upper {
                        v = upper;
                        dir.set(-1.0);
                    } else if v <= 0.0 {
                        v = 0.0;
                        dir.set(1.0);
                    }
                    adj.set_value(v);
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
}
