//! The animated-WebP hover preview.
//!
//! In the watch sidebar the reference client opens no player session on hover. It fetches one
//! `i.ytimg.com/an_webp/<id>/mqdefault_6s.webp` (a silent loop of about 6 s, 130-150 KB) and
//! animates it over the thumbnail (IT-027).
//!
//! The URL is signed (`du`/`sqp`/`rs`) and cannot be built from a video id, so it comes from
//! the response (`ResultItem::moving_thumbnail`). YouTube sends it only on surfaces that
//! preview with an image, and its presence selects that path.
//!
//! libavcodec's WebP decoder does not handle animation chunks, so frames are decoded with the
//! pure-Rust `image` crate (`webp` feature only, no system linkage).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

/// One decoded frame: RGBA8 pixels and how long it is shown.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub delay_ms: u32,
}

/// Shortest time a frame is shown. A zero delay is legal in the format, and a zero-length
/// timer on the GTK main loop spins and starves everything else.
pub const MIN_FRAME_MS: u32 = 20;

/// Decode an animated WebP into its frames.
///
/// A still WebP decodes to a single frame, which animates correctly (one frame, forever)
/// and needs no special case.
pub fn decode(bytes: &[u8]) -> Result<Vec<Frame>, image::ImageError> {
    use image::AnimationDecoder;
    let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))?;
    let mut out = Vec::new();
    for frame in decoder.into_frames() {
        let frame = frame?;
        let (num, den) = frame.delay().numer_denom_ms();
        let buffer = frame.into_buffer();
        out.push(Frame {
            width: buffer.width(),
            height: buffer.height(),
            delay_ms: (num / den.max(1)).max(MIN_FRAME_MS),
            rgba: buffer.into_raw(),
        });
    }
    Ok(out)
}

/// Which frame is showing `elapsed_ms` into a loop, and how long until the next one.
///
/// Returns `None` for an empty animation. The loop wraps, so this is defined for any
/// elapsed time; a caller that drifts (a stalled main loop, a long decode) resynchronises
/// to the right frame instead of accumulating the drift.
pub fn frame_at(delays_ms: &[u32], elapsed_ms: u64) -> Option<(usize, u32)> {
    if delays_ms.is_empty() {
        return None;
    }
    let total: u64 = delays_ms.iter().map(|d| (*d).max(MIN_FRAME_MS) as u64).sum();
    let mut at = elapsed_ms % total;
    for (i, d) in delays_ms.iter().enumerate() {
        let d = (*d).max(MIN_FRAME_MS) as u64;
        if at < d {
            return Some((i, (d - at) as u32));
        }
        at -= d;
    }
    // Unreachable: `total` sums the same clamped values. Returns the last frame instead of
    // panicking.
    Some((delays_ms.len() - 1, MIN_FRAME_MS))
}

/// A `gtk::Picture` that plays a decoded animation on the main loop.
pub struct AnimatedPreview {
    pub picture: gtk::Picture,
    frames: Rc<Vec<gdk::Texture>>,
    delays: Rc<Vec<u32>>,
    timer: Rc<RefCell<Option<glib::SourceId>>>,
}

impl AnimatedPreview {
    pub fn new() -> Self {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .hexpand(true)
            .vexpand(true)
            .build();
        picture.set_visible(false);
        Self {
            picture,
            frames: Rc::new(Vec::new()),
            delays: Rc::new(Vec::new()),
            timer: Rc::new(RefCell::new(None)),
        }
    }

    /// Show `frames`, looping. Replaces whatever was playing.
    pub fn play(&mut self, frames: Vec<Frame>) {
        self.stop();
        if frames.is_empty() {
            return;
        }
        self.delays = Rc::new(frames.iter().map(|f| f.delay_ms).collect());
        self.frames = Rc::new(
            frames
                .into_iter()
                .map(|f| {
                    let bytes = glib::Bytes::from_owned(f.rgba);
                    gdk::MemoryTexture::new(
                        f.width as i32,
                        f.height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &bytes,
                        f.width as usize * 4,
                    )
                    .upcast()
                })
                .collect(),
        );
        self.picture.set_paintable(self.frames.first());
        self.picture.set_visible(true);
        self.schedule(0);
    }

    /// Arm the timer for the frame after `elapsed_ms`.
    fn schedule(&self, elapsed_ms: u64) {
        rearm(
            self.frames.clone(),
            self.delays.clone(),
            self.timer.clone(),
            self.picture.clone(),
            elapsed_ms,
        );
    }

    /// Stop animating and hide the picture.
    pub fn stop(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        self.picture.set_visible(false);
        self.picture.set_paintable(gdk::Paintable::NONE);
    }
}

/// One animation tick: shows the frame at `elapsed_ms` and re-arms the timer for the next.
fn rearm(
    frames: Rc<Vec<gdk::Texture>>,
    delays: Rc<Vec<u32>>,
    timer: Rc<RefCell<Option<glib::SourceId>>>,
    picture: gtk::Picture,
    elapsed_ms: u64,
) {
    let Some((idx, next_in)) = frame_at(&delays, elapsed_ms) else { return };
    picture.set_paintable(frames.get(idx));
    let (f2, d2, t2, p2) = (frames.clone(), delays.clone(), timer.clone(), picture.clone());
    let src = glib::timeout_add_local_once(Duration::from_millis(next_in as u64), move || {
        t2.borrow_mut().take();
        rearm(f2, d2, t2.clone(), p2, elapsed_ms + next_in as u64);
    });
    *timer.borrow_mut() = Some(src);
}

impl Default for AnimatedPreview {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_webp_frame_follows_elapsed_time_and_wraps_at_the_end_of_the_loop() {
        // 20 frames of 125 ms, a 2.5 s loop.
        let delays = vec![125u32; 20];

        assert_eq!(frame_at(&delays, 0), Some((0, 125)));
        assert_eq!(frame_at(&delays, 124), Some((0, 1)));
        assert_eq!(frame_at(&delays, 125), Some((1, 125)));
        assert_eq!(frame_at(&delays, 2_499), Some((19, 1)));
        // Wraps rather than running off the end.
        assert_eq!(frame_at(&delays, 2_500), Some((0, 125)));
        assert_eq!(frame_at(&delays, 2_625), Some((1, 125)));
    }

    #[test]
    fn an_webp_zero_delay_frames_never_schedule_a_zero_length_timer() {
        // A 0 ms delay is legal in the format; unclamped it arms a zero-length timeout.
        let delays = vec![0, 0, 0];
        for elapsed in [0u64, 1, 19, 20, 41, 1_000] {
            let (_, next) = frame_at(&delays, elapsed).expect("a frame");
            assert!(next >= 1, "elapsed {elapsed} produced a {next}ms timer");
        }
    }
}
