use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::glib;

/// Transcribes a whole VOD's audio into the transcript cache, then shows it in the Ask panel.
pub(super) fn transcribe_vod(
    rt: &crate::runtime::AsyncRuntime,
    panel: &Rc<crate::widgets::AskPanel>,
    strip: &Rc<crate::widgets::CatchupStrip>,
    sidebar_column: &gtk::Box,
    id: String,
    duration_ms: i64,
) {
    panel.set_transcribing(true);
    strip.begin("Fetching audio…");
    sidebar_column.set_visible(true);
    let (ptx, prx) = async_channel::unbounded::<crate::catchup::CatchupPhase>();
    {
        let strip = strip.clone();
        glib::spawn_future_local(async move {
            while let Ok(phase) = prx.recv().await {
                strip.show_phase(&phase);
            }
        });
    }
    let (panel, strip, vid) = (panel.clone(), strip.clone(), id.clone());
    tracing::info!("[transcribe] {id}: VOD 0..{}", crate::catchup::fmt_ts(duration_ms));
    rt.spawn(
        async move { crate::catchup::transcribe_window(&vid, 0, Some(duration_ms), &ptx).await },
        move |res: Result<(Vec<crate::catchup::CatchupSegment>, i64), String>| {
            panel.set_transcribing(false);
            match res {
                Ok((segments, _)) => {
                    tracing::info!("[transcribe] {id}: {} segments", segments.len());
                    strip.finish();
                    show_vod_transcript(&panel, &id);
                }
                Err(e) => {
                    tracing::warn!("[transcribe] {id}: failed: {e}");
                    strip.show_error(&e);
                }
            }
        },
    );
}

/// A VOD with no captions: its speech-to-text transcript from the cache, or the Transcribe offer.
pub(super) fn show_vod_transcript(panel: &crate::widgets::AskPanel, id: &str) {
    let segments = crate::catchup::cached_transcript(id);
    if segments.is_empty() {
        panel.reset("This video has no captions. Transcribe its audio to search it and ask about it.");
        panel.set_transcribe(Some("Transcribe"));
    } else {
        panel.reset("");
        panel.set_transcript(crate::transcript::from_stt(&segments), "speech-to-text transcript");
        panel.set_transcribe(None);
    }
}

pub(super) fn show_live_transcript(panel: &crate::widgets::AskPanel, id: &str) {
    let segments = crate::catchup::cached_transcript(id);
    if segments.is_empty() {
        panel.reset("No transcript yet. Transcribe the stream to search it and ask about it.");
        panel.set_transcribe(Some("Transcribe"));
    } else {
        panel.reset("");
        panel.set_transcript(crate::transcript::from_stt(&segments), "live transcript");
        panel.set_transcribe(Some("Transcribe up to now"));
    }
}
