//! The segment-session feed threads that push track bytes into a parse pipeline.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer_app as gst_app;

use innertube::SegmentSource;

pub(super) const MP4_MSE_CAPS: &str = "video/quicktime, variant=(string)mse-bytestream";

/// Caps a track's parse appsrc declares, from its first piece. qtdemux resets its box
/// parser on a flush only in its MSE variant; otherwise a seek inside an mdat derails it.
pub(super) fn container_caps(first: &[u8]) -> Option<gst::Caps> {
    (first.get(4..8) == Some(b"ftyp".as_slice())).then(|| MP4_MSE_CAPS.parse().expect("static caps"))
}

pub(super) fn spawn_segment_feed(
    src: gst_app::AppSrc,
    mut track: Box<dyn SegmentSource>,
    reposition: Arc<Mutex<Option<i64>>>,
    push: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    tag: &'static str,
) {
    std::thread::spawn(move || {
        let mut total: u64 = 0;
        let mut total_pieces: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Some(ms) = reposition.lock().unwrap().take() {
                track.reposition(ms);
            }
            let Some(piece) = track.next_piece() else {
                if reposition.lock().unwrap().is_some() {
                    continue;
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                tracing::info!("[gst] {tag}: segment feed reached the end at {} ms", track.cursor_ms());
                let _ = src.end_of_stream();
                break;
            };
            let _held = push.lock().unwrap();
            if reposition.lock().unwrap().is_some() {
                continue;
            }
            if total == 0 {
                tracing::info!("[gst] {tag}: first segment itag {} at {} ms uptime {}ms", piece.itag, piece.start_ms, crate::uptime_ms());
                if let Some(caps) = container_caps(&piece.bytes) {
                    src.set_caps(Some(&caps));
                }
            }
            total += piece.bytes.len() as u64;
            // The dump-feed hook writes the bytes this track feeds its parser, one file per
            // piece in feed order.
            if let Some(dir) = crate::hooks::get("dump-feed") {
                let n = total_pieces;
                let _ = std::fs::write(format!("{dir}/{tag}-{n:05}-{}-{}.bin", piece.itag, piece.start_ms), &piece.bytes);
            }
            total_pieces += 1;
            if let Err(e) = src.push_buffer(gst::Buffer::from_slice(piece.bytes)) {
                if e == gst::FlowError::Flushing || reposition.lock().unwrap().is_some() {
                    continue;
                }
                break;
            }
        }
        running.store(false, Ordering::Relaxed);
        tracing::info!("[gst] {tag}: segment feed ended after {total}B");
    });
}
