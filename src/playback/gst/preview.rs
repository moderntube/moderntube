//! Hover previews on the GStreamer substrate: one small, muted, low-res pipeline per
//! hovered card, drawn into its own `gtk::Picture`.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use innertube::{MediaReader, NSigSolver, Playback, PlaybackOptions, SabrStream, SegmentVod, Session};

use super::substrate::{decodes_av1, GstSubstrate};
use crate::playback::{PlaybackError, PREVIEW_QUALITY};

/// Off-thread preparation result for a GStreamer preview.
#[allow(clippy::large_enum_variant)]
pub enum GstPreparedPreview {
    Vod(Arc<SegmentVod>),
    /// The stream and its two readers: it runs while a reader is alive.
    Live(SabrStream, MediaReader, MediaReader),
}

type PlayingCb = Box<dyn Fn(&str)>;

pub struct GstPreviewEngine {
    picture: gtk::Picture,
    gst: Rc<RefCell<Option<GstSubstrate>>>,
    /// A live preview's stream and readers; it runs while a reader is alive.
    live: RefCell<Option<(SabrStream, MediaReader, MediaReader)>>,
    current_id: Rc<RefCell<Option<String>>>,
    muted: Cell<bool>,
    on_playing: Rc<RefCell<Option<PlayingCb>>>,
    first_frame_watch: RefCell<Option<glib::SourceId>>,
}

impl GstPreviewEngine {
    pub fn new() -> Rc<Self> {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        let engine = Rc::new(Self {
            picture,
            gst: Rc::new(RefCell::new(None)),
            live: RefCell::new(None),
            current_id: Rc::new(RefCell::new(None)),
            muted: Cell::new(true),
            on_playing: Rc::new(RefCell::new(None)),
            first_frame_watch: RefCell::new(None),
        });
        let eng = engine.clone();
        engine.picture.connect_unmap(move |_| eng.stop());
        engine
    }

    /// The widget the preview draws into.
    pub fn widget(&self) -> gtk::Widget {
        self.picture.clone().upcast()
    }

    pub fn connect_playing<F: Fn(&str) + 'static>(&self, f: F) {
        *self.on_playing.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(g) = self.gst.borrow().as_ref() {
            g.set_volume(if muted { 0.0 } else { 100.0 });
        }
    }

    pub fn time_pos(&self) -> Option<f64> {
        self.gst.borrow().as_ref().and_then(|g| g.position_secs())
    }

    pub async fn prepare(
        video_id: &str,
        nav: &innertube::Navigation,
    ) -> Result<GstPreparedPreview, PlaybackError> {
        let (session, solver, pot) = Self::deps()?;
        let opts = PlaybackOptions {
            quality: PREVIEW_QUALITY,
            av1: decodes_av1(),
            navigation: nav.clone(),
            ..Default::default()
        };
        Ok(match Playback::open(&session, &solver, pot, video_id, &opts).await? {
            Playback::Vod(vod) => GstPreparedPreview::Vod(Arc::new(vod)),
            Playback::Live(stream) => {
                let (v, a) = (stream.video(), stream.audio());
                GstPreparedPreview::Live(stream, v, a)
            }
        })
    }

    /// Start `video_id`'s preview, replacing whatever preview was running.
    pub fn start(&self, video_id: &str, prepared: GstPreparedPreview) {
        if self.current_id.borrow().as_deref() == Some(video_id) {
            tracing::info!("[preview] start {video_id}: already loaded, no-op");
            return;
        }
        tracing::info!(
            "[preview] start {video_id} (gst {}) — replacing {}",
            if matches!(prepared, GstPreparedPreview::Live(..)) { "live" } else { "vod" },
            self.current_id.borrow().as_deref().unwrap_or("<nothing>"),
        );
        self.stop();
        let gst = match GstSubstrate::new() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("[preview] {video_id}: GStreamer pipeline failed to build: {e}");
                return;
            }
        };
        let attach: super::GstPaintableSink = {
            let picture = self.picture.clone();
            Rc::new(move |p: &gtk::gdk::Paintable| {
                picture.set_paintable(Some(p));
                let picture = picture.clone();
                p.connect_invalidate_contents(move |_| picture.queue_draw());
            })
        };
        attach(gst.paintable());
        gst.set_on_repaintable(attach);
        let started = match prepared {
            GstPreparedPreview::Vod(vod) => {
                self.report_playhead(video_id, vod.clone());
                gst.start_segvod(vod, 0)
            }
            GstPreparedPreview::Live(stream, rv, ra) => {
                let (v, a) = (stream.segment_track(true), stream.segment_track(false));
                *self.live.borrow_mut() = Some((stream, rv, ra));
                gst.start_live(Box::new(v), Box::new(a))
            }
        };
        if let Err(e) = started {
            tracing::warn!("[preview] {video_id}: GStreamer pipeline failed to start: {e}");
            return;
        }
        gst.set_volume(if self.muted.get() { 0.0 } else { 100.0 });
        *self.current_id.borrow_mut() = Some(video_id.to_string());
        let flag = gst.first_frame_handle();
        *self.gst.borrow_mut() = Some(gst);

        let (current, on_playing, id) =
            (self.current_id.clone(), self.on_playing.clone(), video_id.to_string());
        let watch = glib::timeout_add_local(Duration::from_millis(50), move || {
            if current.borrow().as_deref() != Some(id.as_str()) {
                return glib::ControlFlow::Break;
            }
            if !flag.load(Ordering::Relaxed) {
                return glib::ControlFlow::Continue;
            }
            tracing::info!("[preview] {id}: preview playing t={}ms", crate::uptime_ms());
            if let Some(cb) = on_playing.borrow().as_ref() {
                cb(&id);
            }
            glib::ControlFlow::Break
        });
        *self.first_frame_watch.borrow_mut() = Some(watch);
    }

    /// Stop the current preview and release its pipeline and stream.
    pub fn stop(&self) {
        if let Some(id) = self.current_id.borrow_mut().take() {
            tracing::info!("[preview] stop {id}");
        }
        if let Some(w) = self.first_frame_watch.borrow_mut().take() {
            // Already removed if it returned `Break`; removing twice panics.
            if glib::MainContext::default().find_source_by_id(&w).is_some() {
                w.remove();
            }
        }
        if let Some(g) = self.gst.borrow_mut().take() {
            g.stop();
        }
        *self.live.borrow_mut() = None;
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
    }

    /// Keep `vod`'s read-ahead gate on this preview's playhead while `video_id` is current.
    fn report_playhead(&self, video_id: &str, vod: Arc<SegmentVod>) {
        let (gst, current, id) = (self.gst.clone(), self.current_id.clone(), video_id.to_string());
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if current.borrow().as_deref() != Some(id.as_str()) {
                return glib::ControlFlow::Break;
            }
            if let Some(pos) = gst.borrow().as_ref().and_then(|g| g.position_secs()) {
                vod.set_playhead_ms((pos * 1000.0) as i64);
            }
            glib::ControlFlow::Continue
        });
    }

    fn deps() -> Result<(Session, NSigSolver, crate::playback::common::Pot), PlaybackError> {
        crate::playback::shared_deps()
    }
}
