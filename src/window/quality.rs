//! The quality menu, and the test hooks that drive quality, speed, seeks, skips and pauses.
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;

use innertube::Quality;

use super::app::App;

/// The quality menu's handler and the quality-test, speed-test, seek-test, skip-test and
/// pause-test hooks.
pub(super) fn wire_quality(app: &Rc<App>) {
    let App { engine, player, current_quality, current_live, .. } = &**app;
    // Persist the chosen cap and apply it in session: live re-joins the running stream at the
    // new rung, VOD pins or unpins a rung. The engine caps downshift-only, and the cap applies
    // to the next play too.
    {
        let engine = engine.clone();
        let player = player.clone();
        let current_live = current_live.clone();
        let current_quality = current_quality.clone();
        let player_cb = player.clone();
        let engine_heal = engine.clone();
        let quality_selected: Rc<dyn Fn(Quality)> = Rc::new(move |q: Quality| {
            // A quality-test pin at the saved cap would be a no-op, so the hook makes it switch.
            if crate::hooks::flag("quality-test") && current_quality.get() == q {
                tracing::info!("[quality] test hook: resetting session cap to Best so the pin switches");
                current_quality.set(Quality::Best);
            }
            if current_quality.get() == q {
                tracing::info!("[quality] select {q:?}: no-op, already at this cap");
                return;
            }
            current_quality.set(q);
            crate::prefs::save_quality(q); // persist across launches
            if current_live.borrow().is_some() {
                // Live changes rung in session without recreating the stream: `set_live_quality`
                // re-joins the running SabrStream at the new rung with a brief rebuffer.
                // Auto unpins: a manual pick collapses the preferred video formats to the one
                // itag (REQ-018), which stops the server adapting. A pick above the opened
                // ladder resolves to its top.
                let applied = match q {
                    Quality::Best => engine.unpin_live_quality(),
                    Quality::Height(h) => engine.set_live_quality(h),
                };
                if !applied {
                    // No live stream: clear the spinner.
                    player_cb.set_loading(false);
                }
                // No spinner: the re-join is a brief rebuffer on the same session.
            } else if engine.set_vod_quality(q) {
                player_cb.set_loading(false);
            } else {
                player_cb.set_loading(false); // idle: the cap applies to the next play
            }
        });
        {
            let q = quality_selected.clone();
            player.controls.connect_quality_selected(move |sel| q(sel));
        }
        // Each pin goes through the quality menu's own closure.
        crate::hooks::on("quality-test", move |spec| {
            for ev in spec.split(',') {
                if let Some((secs, target)) = ev.split_once(':') {
                    let q = if target == "auto" {
                        Quality::Best
                    } else {
                        target.parse().map(Quality::Height).unwrap_or(Quality::Best)
                    };
                    let secs: u32 = secs.parse().unwrap_or(20);
                    let f = quality_selected.clone();
                    glib::timeout_add_seconds_local_once(secs, move || {
                        tracing::info!("[quality-test] select {q:?}");
                        f(q)
                    });
                }
            }
        });
        let e = engine_heal.clone();
        crate::hooks::on("speed-test", move |spec| {
            for ev in spec.split(',') {
                if let Some((secs, rate)) = ev.split_once(':') {
                    let rate: f64 = rate.parse().unwrap_or(1.0);
                    let secs: u32 = secs.parse().unwrap_or(30);
                    let e = e.clone();
                    glib::timeout_add_seconds_local_once(secs, move || {
                        tracing::info!("[speed-test] set speed {rate}");
                        e.set_speed(rate);
                    });
                }
            }
        });
        let e = engine_heal.clone();
        crate::hooks::on("seek-test", move |spec| {
            for (secs, target) in spec.split(',').filter_map(|ev| ev.split_once(':')) {
                let secs: u32 = secs.parse().unwrap_or(35);
                let target: f64 = target.parse().unwrap_or(0.0);
                let engine2 = e.clone();
                glib::timeout_add_seconds_local_once(secs, move || {
                    tracing::info!("[seek-test] seeking to {target}s uptime {}ms", crate::uptime_ms());
                    engine2.seek_absolute(target);
                });
            }
        });
        let e = engine_heal.clone();
        crate::hooks::on("skip-test", move |spec| {
            for ev in spec.split(',') {
                let f: Vec<&str> = ev.split(':').collect();
                let [secs, count, every, delta] = f[..] else { continue };
                let (Ok(secs), Ok(count), Ok(every), Ok(delta)) =
                    (secs.parse::<u64>(), count.parse::<u64>(), every.parse::<u64>(), delta.parse::<f64>())
                else {
                    continue;
                };
                for i in 0..count {
                    let e = e.clone();
                    let at = std::time::Duration::from_millis(secs * 1000 + i * every);
                    glib::timeout_add_local_once(at, move || {
                        tracing::info!("[skip-test] {delta:+}s uptime {}ms", crate::uptime_ms());
                        e.seek_relative(delta);
                    });
                }
            }
        });
        crate::hooks::on("pause-test", move |spec| {
            for ev in spec.split(',') {
                let Some((secs, hold)) = ev.split_once(':') else { continue };
                let (Ok(secs), Ok(hold)) = (secs.trim().parse::<u32>(), hold.trim().parse::<u32>())
                else {
                    continue;
                };
                let e_pause = engine_heal.clone();
                let e_resume = engine_heal.clone();
                glib::timeout_add_seconds_local_once(secs, move || {
                    tracing::info!("[pause-test] pause at t={secs}s (avsync={:.4})", e_pause.avsync());
                    e_pause.set_paused(true);
                    glib::timeout_add_seconds_local_once(hold, move || {
                        tracing::info!("[pause-test] resume after {hold}s");
                        e_resume.set_paused(false);
                    });
                });
            }
        });
    }
}
