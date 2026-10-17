//! Diagnostic probes on the play pipeline: segments, frame gaps, sink positions.
use std::sync::Mutex;

use gstreamer as gst;
use gstreamer::prelude::*;

/// Log every segment and flush reaching a sink: the timeline it renders against.
pub(super) fn log_segments(tag: &'static str, sink: &gst::Element) {
    let Some(pad) = sink.static_pad("sink") else { return };
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH, move |_, info| {
        if let Some(gst::PadProbeData::Event(ev)) = &info.data {
            match ev.view() {
                gst::EventView::Segment(s) => {
                    if let Ok(seg) = s.segment().clone().downcast::<gst::ClockTime>() {
                        tracing::info!(
                            "[gst-sink] {tag}: segment start={:?} time={:?} base={:?} uptime {}ms",
                            seg.start(),
                            seg.time(),
                            seg.base(),
                            crate::uptime_ms()
                        );
                    }
                }
                gst::EventView::FlushStop(_) => tracing::info!("[gst-sink] {tag}: flush-stop uptime {}ms", crate::uptime_ms()),
                _ => {}
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// Logs a wall-clock gap or a pts step over 100 ms between consecutive buffers at a probe.
pub(super) fn frame_gap_logger(tag: String) -> impl Fn(&gst::PadProbeInfo) + Send + Sync + 'static {
    let last = Mutex::new(None::<(std::time::Instant, f64)>);
    let strict = crate::diag::gap_strict();
    move |info| {
        let Some(buf) = info.buffer() else { return };
        let now = std::time::Instant::now();
        let pts = buf.pts().map_or(-1.0, |p| p.seconds_f64());
        let prev = last.lock().unwrap().replace((now, pts));
        if let Some((at, prev_pts)) = prev {
            let ms = now.duration_since(at).as_millis();
            let step = (pts - prev_pts) * 1000.0;
            let odd = if strict { !(30.0..=37.0).contains(&step) } else { step > 100.0 };
            if ms > 100 || odd {
                let dts = buf.dts().map_or(-1.0, |p| p.seconds_f64());
                let dur = buf.duration().map_or(-1.0, |d| d.mseconds() as f64);
                tracing::info!(
                    "[gst-gap] {tag}: {ms}ms wall, pts {prev_pts:.3}s -> {pts:.3}s ({step:.0}ms) dts {dts:.3} dur {dur}ms flags {:?} uptime {}ms",
                    buf.flags(),
                    crate::uptime_ms()
                );
            }
        }
    }
}

/// Stream time in seconds each sink last rendered: the audio and video positions a viewer sees.
pub(super) fn sink_pos(sink: &gst::Element) -> Option<f64> {
    sink.query_position::<gst::ClockTime>().map(|p| p.seconds_f64())
}
