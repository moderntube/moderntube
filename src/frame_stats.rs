//! [`crate::diag::frame_stats`]: every 2 s, the frames painted, the median and worst interval
//! between them, and the mean main-thread frame time (layout, snapshot, render submission).
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk4 as gtk;
use gtk::prelude::*;

#[derive(Default)]
struct Window {
    since: Option<Instant>,
    last_paint: Option<Instant>,
    intervals_ms: Vec<f64>,
    paint_ms: Vec<f64>,
    paint_start: Option<Instant>,
}

pub fn attach(widget: &impl IsA<gtk::Widget>, tag: &'static str) {
    let state = Rc::new(RefCell::new(Window::default()));
    widget.connect_realize(move |w| {
        let Some(clock) = w.frame_clock() else { return };
        let s = state.clone();
        clock.connect_before_paint(move |_| s.borrow_mut().paint_start = Some(Instant::now()));
        let s = state.clone();
        clock.connect_after_paint(move |_| {
            let now = Instant::now();
            let mut st = s.borrow_mut();
            if let Some(start) = st.paint_start.take() {
                st.paint_ms.push(now.duration_since(start).as_secs_f64() * 1000.0);
            }
            if let Some(prev) = st.last_paint.replace(now) {
                st.intervals_ms.push(now.duration_since(prev).as_secs_f64() * 1000.0);
            }
            let since = *st.since.get_or_insert(now);
            if now.duration_since(since).as_secs_f64() >= 2.0 && !st.intervals_ms.is_empty() {
                let mut iv = std::mem::take(&mut st.intervals_ms);
                iv.sort_by(|a, b| a.total_cmp(b));
                let paint = std::mem::take(&mut st.paint_ms);
                let mean_paint = paint.iter().sum::<f64>() / paint.len().max(1) as f64;
                tracing::info!(
                    "[frames {tag}] fps={:.1} p50={:.1}ms p95={:.1}ms max={:.1}ms paint={:.2}ms",
                    iv.len() as f64 / now.duration_since(since).as_secs_f64(),
                    iv[iv.len() / 2],
                    iv[(iv.len() * 95 / 100).min(iv.len() - 1)],
                    iv[iv.len() - 1],
                    mean_paint,
                );
                st.since = Some(now);
            }
        });
    });
}
