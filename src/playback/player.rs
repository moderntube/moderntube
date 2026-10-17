//! The one interface the app plays video through; nothing outside `gst/` names the substrate.
use std::rc::Rc;
use std::sync::Arc;

use gtk4 as gtk;
use innertube::{Navigation, Quality, SegmentVod, StoryboardThumb, VideoDetails};

use super::common::events::PlayerEvents;
use super::common::live::LiveSession;
use super::PlaybackError;

/// The byte sources for one video, opened off the main thread and handed to [`Player::load`].
pub use super::gst::Prepared;

/// Open a video as a VOD or a live stream, whichever it is. `start` is a VOD resume
/// position in seconds.
pub async fn prepare(
    video_id: &str,
    quality: Quality,
    start: Option<f64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    super::gst::prepare(video_id, quality, start, navigation).await
}

/// Open a VOD. `start` is the resume position in seconds.
pub async fn prepare_vod(
    video_id: &str,
    quality: Quality,
    start: Option<f64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    super::gst::prepare_vod(video_id, quality, start, navigation).await
}

/// Open a live stream: at the edge, or at `start_ms` in the DVR.
pub async fn prepare_live(
    video_id: &str,
    quality: Quality,
    start_ms: Option<i64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    super::gst::prepare_live(video_id, quality, start_ms, navigation).await
}

pub fn build() -> Rc<dyn Player> {
    super::gst::GstPlayer::build()
}

pub trait Player {
    /// The widget the video draws into; the player owns it.
    fn widget(&self) -> gtk::Widget;
    /// Ask for a repaint after a load or a track swap.
    fn queue_render(&self) {}

    fn load(&self, prepared: Prepared);
    /// Like [`Player::load`], starting at `resume_secs`.
    fn load_resume(&self, prepared: Prepared, resume_secs: f64);
    fn stop(&self);
    /// Halt the outgoing SABR session; a second concurrent session gets empty rounds.
    fn cancel_stream(&self);

    fn toggle_pause(&self);
    fn set_paused(&self, paused: bool);
    fn is_paused(&self) -> bool;
    fn time_pos(&self) -> f64 {
        self.time_pos_checked().unwrap_or(0.0)
    }
    /// The playhead, or `None` when the substrate cannot answer. Anything that
    /// repositions the stream uses this: a 0.0 fallback would re-anchor at the start.
    fn time_pos_checked(&self) -> Option<f64>;
    /// Whether a seek is outstanding. Diagnostic only.
    fn seeking(&self) -> Option<bool> {
        None
    }
    fn avsync(&self) -> f64;
    fn duration(&self) -> Option<f64>;
    fn seek_absolute(&self, secs: f64);
    fn seek_relative(&self, delta: f64);
    fn volume(&self) -> f64;
    fn set_volume(&self, v: f64);
    fn speed(&self) -> f64;
    fn set_speed(&self, s: f64);

    fn events(&self) -> &PlayerEvents;
    fn live(&self) -> &LiveSession;

    fn is_live(&self) -> bool {
        self.live().is_live()
    }
    /// The live DVR window, stream-absolute seconds `(earliest, head)`.
    fn live_window(&self) -> Option<(f64, f64)> {
        None
    }
    fn live_thumbnail(&self, _secs: f64) -> Option<StoryboardThumb> {
        None
    }
    /// `false`: the caller re-joins the edge with a fresh open.
    fn live_go_to_head(&self) -> bool {
        false
    }

    /// Heights the playing VOD can be pinned to.
    fn vod_heights(&self) -> Option<Vec<i32>>;
    /// Apply a quality pick to the playing VOD in session (`Best` returns the rung to the
    /// server). `false` when no VOD plays.
    fn set_vod_quality(&self, q: Quality) -> bool;
    /// The corner PiP opened at `pip_cap`; a player may drop to it while small.
    fn pip_enter(&self, _pip_cap: Quality) {}
    /// Back to full size: restore `user_cap`.
    fn pip_exit(&self, _user_cap: Quality) {}
    /// The segment session captions are read from.
    fn caption_session(&self) -> Option<Arc<SegmentVod>> {
        None
    }
    /// Watch-page metadata the open already carried (the watch-next half of `get_watch`,
    /// IT-006). The caller skips a separate `next()` call when it is present.
    fn watch_details(&self) -> Option<VideoDetails> {
        None
    }

    fn set_on_loaded(&self, f: Rc<dyn Fn()>) {
        *self.events().on_loaded.borrow_mut() = Some(f);
    }
    fn set_on_ended(&self, f: Rc<dyn Fn()>) {
        *self.events().on_ended.borrow_mut() = Some(f);
    }
    fn set_on_failed(&self, f: crate::playback::common::events::FailedFn) {
        *self.events().on_failed.borrow_mut() = Some(f);
    }

    fn live_player_time_ms(&self) -> Option<i64> {
        self.live().player_time_ms()
    }
    fn live_head_ms(&self) -> Option<i64> {
        self.live().head_ms()
    }
    fn live_at_head(&self) -> Option<bool> {
        self.live().at_head()
    }
    fn set_live_quality(&self, height: i32) -> bool {
        self.live().set_quality(height)
    }
    fn unpin_live_quality(&self) -> bool {
        self.live().unpin_quality()
    }
}
