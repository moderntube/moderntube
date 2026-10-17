//! The post-seek rendezvous that starts audio and video together.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// Holds the play pipeline flushing after a seek until both tracks have their first
/// sample at the target, then ends the flush once so audio and video start together.
pub(super) struct SeekSync {
    pub(super) gen: AtomicU64,
    pub(super) pending: AtomicBool,
    pub(super) video: AtomicBool,
    pub(super) audio: AtomicBool,
    /// The seek generation whose parse-pipeline flush has completed. A sample pulled
    /// while this lags the seek generation came out of a pre-seek demuxer queue.
    pub(super) parse_gen: AtomicU64,
    pub(super) releasing: Mutex<()>,
    pub(super) srcs: [gst_app::AppSrc; 2],
    pub(super) armed_at: Mutex<std::time::Instant>,
    /// Live: each track's first PTS (video, audio), `u64::MAX` until it arrives. Both
    /// segments start at the later one, where both tracks have media, as MSE starts at
    /// the intersection of the buffered ranges. The audio sink clocks the pipeline and
    /// never starts on a first sample later than the segment start.
    pub(super) live_first: [AtomicU64; 2],
}

impl SeekSync {
    pub(super) const WAIT: std::time::Duration = std::time::Duration::from_secs(4);

    pub(super) fn wait(&self) -> std::time::Duration {
        Self::WAIT
    }

    pub(super) fn arm(&self, gen: u64) {
        self.gen.store(gen, Ordering::Relaxed);
        self.video.store(false, Ordering::Relaxed);
        self.audio.store(false, Ordering::Relaxed);
        *self.armed_at.lock().unwrap() = std::time::Instant::now();
        self.pending.store(true, Ordering::Relaxed);
    }

    /// Called by a pump holding its first post-seek sample; returns when it may push.
    pub(super) fn ready(&self, tag: &str, gen: u64, seek_gen: &AtomicU64, stop: &AtomicBool) {
        if !self.pending.load(Ordering::Relaxed) || self.gen.load(Ordering::Relaxed) != gen {
            return;
        }
        let flag = if tag == "audio" { &self.audio } else { &self.video };
        flag.store(true, Ordering::Relaxed);
        loop {
            if !self.pending.load(Ordering::Relaxed) || seek_gen.load(Ordering::Relaxed) != gen || stop.load(Ordering::Relaxed) {
                return;
            }
            let both = self.video.load(Ordering::Relaxed) && self.audio.load(Ordering::Relaxed);
            let waited = self.armed_at.lock().unwrap().elapsed();
            if both || waited >= self.wait() {
                self.release(gen, if both { "both tracks ready" } else { "timed out" });
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Blocks while a seek flush is pending, for any sample: nothing pushes into a
    /// flushing appsrc. Bounded by the rendezvous wait plus its fallback margin.
    pub(super) fn wait_released(&self, gen: u64, seek_gen: &AtomicU64, stop: &AtomicBool) {
        let t0 = std::time::Instant::now();
        while self.pending.load(Ordering::Relaxed)
            && seek_gen.load(Ordering::Relaxed) == gen
            && !stop.load(Ordering::Relaxed)
            && t0.elapsed() < self.wait() + std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(super) fn release(&self, gen: u64, why: &str) {
        // The flush ends before `pending` clears: waiters push as soon as it does.
        let _once = self.releasing.lock().unwrap();
        if self.pending.load(Ordering::Relaxed) && self.gen.load(Ordering::Relaxed) == gen {
            // FlushStop needs the streaming thread's lock, which an appsrc the seek never
            // flushed holds while it waits for data; the FlushStart wakes it first.
            for s in &self.srcs {
                let _ = s.send_event(gst::event::FlushStart::new());
                let _ = s.send_event(gst::event::FlushStop::new(true));
            }
            self.pending.store(false, Ordering::Relaxed);
            tracing::info!("[gst] seek flush released ({why}) uptime {}ms", crate::uptime_ms());
        }
    }
}
