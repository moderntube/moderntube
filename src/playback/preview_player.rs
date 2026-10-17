//! The hover preview. Callers go through [`PreviewPlayer`] only.
use std::rc::Rc;

use gtk4 as gtk;

use super::gst::preview::{GstPreparedPreview, GstPreviewEngine};
use super::PlaybackError;

pub type PreviewPrepared = GstPreparedPreview;

pub struct PreviewPlayer(Rc<GstPreviewEngine>);

impl Default for PreviewPlayer {
    fn default() -> Self {
        Self(GstPreviewEngine::new())
    }
}

impl PreviewPlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The widget the preview draws into.
    pub fn widget(&self) -> gtk::Widget {
        self.0.widget()
    }

    pub fn connect_playing<F: Fn(&str) + 'static>(&self, f: F) {
        self.0.connect_playing(f)
    }

    pub fn set_muted(&self, muted: bool) {
        self.0.set_muted(muted)
    }

    pub fn time_pos(&self) -> Option<f64> {
        self.0.time_pos()
    }

    pub fn start(&self, video_id: &str, prepared: PreviewPrepared) {
        self.0.start(video_id, prepared)
    }

    pub fn stop(&self) {
        self.0.stop()
    }

    /// Previews `video_id` as the kind its player response says, whatever the card's live flag.
    pub async fn prepare(video_id: &str, nav: &innertube::Navigation) -> Result<PreviewPrepared, PlaybackError> {
        GstPreviewEngine::prepare(video_id, nav).await
    }
}
