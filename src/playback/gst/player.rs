//! The main player on the GStreamer substrate: segment sessions for VOD, raw live segments
//! for live, both played through a [`GstSubstrate`] into a `gtk::Picture`.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use innertube::{MediaReader, Quality, SegmentVod, StoryboardThumb};

use super::substrate::GstSubstrate;
use super::Prepared;
use crate::playback::common::events::PlayerEvents;
use crate::playback::common::live::{settle_edge_speed, LiveSession};
use crate::playback::player::Player;

pub struct GstPlayer {
    me: std::rc::Weak<GstPlayer>,
    picture: gtk::Picture,
    gst: RefCell<Option<GstSubstrate>>,
    live: LiveSession,
    /// A live stream runs while a reader of it is alive.
    live_readers: RefCell<Option<(MediaReader, MediaReader)>>,
    events: PlayerEvents,
    volume: Cell<f64>,
    requested_speed: Cell<f64>,
    /// The stream's `SABR_SEEK` count the substrate last followed.
    sabr_seeks_seen: Cell<Option<u64>>,
    /// The last DVR seek target (stream ms) the substrate has already flushed to.
    dvr_target_ms: Cell<Option<i64>>,
}

impl GstPlayer {
    /// A server `SABR_SEEK` this far before a DVR seek's target lands in the segment the
    /// substrate already flushed to: the server names the segment start.
    const DVR_CONFIRM_SPAN_MS: i64 = 10_000;

    pub fn build() -> Rc<dyn Player> {
        let picture = gtk::Picture::new();
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        let muted = crate::hooks::flag("mute");
        let speed = crate::hooks::get("speed")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|r| *r > 0.0)
            .map_or(1.0, |r| r.clamp(0.25, 4.0));
        let p = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            picture,
            gst: RefCell::new(None),
            live: LiveSession::default(),
            live_readers: RefCell::new(None),
            events: PlayerEvents::default(),
            volume: Cell::new(if muted { 0.0 } else { 100.0 }),
            requested_speed: Cell::new(speed),
            sabr_seeks_seen: Cell::new(None),
            dvr_target_ms: Cell::new(None),
        });
        p.start_playhead_reporter();
        p.start_soak_tick();
        p
    }

    /// Put `paintable` on the picture; its invalidation drives the per-frame paints.
    fn attach(picture: &gtk::Picture, paintable: &gtk::gdk::Paintable) {
        picture.set_paintable(Some(paintable));
        let pic = picture.clone();
        paintable.connect_invalidate_contents(move |_| pic.queue_draw());
    }

    /// A fresh substrate wired to the picture and the first-frame and end callbacks.
    fn new_gst(self: &Rc<Self>) -> Option<GstSubstrate> {
        if let Some(old) = self.gst.borrow_mut().take() {
            old.stop();
        }
        let gst = match GstSubstrate::new() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("[engine] GStreamer pipeline failed to build: {e}");
                return None;
            }
        };
        Self::attach(&self.picture, gst.paintable());
        let picture = self.picture.clone();
        gst.set_on_repaintable(Rc::new(move |p| Self::attach(&picture, p)));
        let weak = Rc::downgrade(self);
        let flag = gst.first_frame_handle();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            if flag.load(Ordering::Relaxed) {
                p.events.loaded();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        let (ended, stopped) = gst.end_handles();
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            if stopped.load(Ordering::Relaxed) {
                return glib::ControlFlow::Break;
            }
            if ended.swap(false, Ordering::Relaxed) {
                // A session that gave up feeds EOS too; only its failure reason tells them apart.
                if let Some(reason) = p.segment_vod().and_then(|v| v.failure()) {
                    p.events.failed(reason);
                    return glib::ControlFlow::Break;
                }
                // A looping Short seeks back to 0 and reaches the end again.
                p.events.ended();
            }
            glib::ControlFlow::Continue
        });
        Some(gst)
    }

    fn segment_vod(&self) -> Option<Arc<SegmentVod>> {
        self.gst.borrow().as_ref().and_then(|g| g.segment_vod())
    }

    fn load_inner(self: &Rc<Self>, prepared: Prepared, resume: Option<f64>) {
        self.stop();
        self.sabr_seeks_seen.set(None);
        let gst = match prepared {
            Prepared::Vod(vod) => {
                vod.set_playback_rate(self.requested_speed.get() as f32);
                let Some(gst) = self.new_gst() else { return };
                let start_ms = resume.map_or(0, |s| (s * 1000.0) as i64);
                if let Err(e) = gst.start_segvod(vod, start_ms) {
                    tracing::warn!("[engine] GStreamer segment VOD failed to start: {e}");
                    return;
                }
                tracing::info!("[engine] VOD playing from a segment session through the GStreamer substrate");
                gst
            }
            Prepared::Live(stream, v, a) => {
                let (video, audio) = (stream.segment_track(true), stream.segment_track(false));
                stream.set_playback_rate(self.requested_speed.get() as f32);
                self.live.set(Some(stream));
                *self.live_readers.borrow_mut() = Some((v, a));
                let Some(gst) = self.new_gst() else { return };
                if let Err(e) = gst.start_live(Box::new(video), Box::new(audio)) {
                    tracing::warn!("[engine] GStreamer live pipeline failed to start: {e}");
                    return;
                }
                tracing::info!("[engine] live playing through the GStreamer substrate");
                gst
            }
        };
        gst.set_volume(self.volume.get());
        if (self.requested_speed.get() - 1.0).abs() > f64::EPSILON {
            gst.set_speed(self.requested_speed.get());
        }
        *self.gst.borrow_mut() = Some(gst);
    }

    /// Keep the session's read-ahead gate on the real playhead, and follow the live
    /// controllers. Every 500 ms for the app's life.
    fn start_playhead_reporter(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            let Some(pos) = p.time_pos_checked() else { return glib::ControlFlow::Continue };
            if let Some(vod) = p.segment_vod() {
                vod.set_playhead_ms((pos * 1000.0) as i64);
                return glib::ControlFlow::Continue;
            }
            if p.live.is_live() {
                p.live.set_playhead_ms((pos * 1000.0) as i64);
                p.maybe_follow_sabr_seek();
                settle_edge_speed(&*p, p.requested_speed.get());
            }
            glib::ControlFlow::Continue
        });
    }

    /// One line a second in the soak format the verdict tools read ([`crate::diag::soak`]).
    fn start_soak_tick(self: &Rc<Self>) {
        if !crate::diag::soak() {
            return;
        }
        let weak = Rc::downgrade(self);
        let t0 = std::time::Instant::now();
        glib::timeout_add_seconds_local(1, move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            let g = p.gst.borrow();
            let h = g.as_ref().and_then(|g| g.video_height()).unwrap_or(0);
            let buffering = g.as_ref().is_some_and(|g| g.is_buffering());
            drop(g);
            let pos = p.time_pos_checked().unwrap_or(-1.0);
            let behind = p.live.behind_head_s().map(|b| format!(" behind_head={b:.1}")).unwrap_or_default();
            tracing::info!(
                "[soak +{}s] served={h}p pos={pos:.1} speed={:.2}{behind} cache=- apts=- hwdec=- drops=- ddrops=- avsync={:.3}{}",
                t0.elapsed().as_secs(),
                p.speed(),
                p.avsync(),
                if buffering { " REBUFFER" } else { "" },
            );
            glib::ControlFlow::Continue
        });
    }

    /// Follows a server `SABR_SEEK` by seeking to the target plus 0.1 s (SABR-011). Seeks
    /// before the first frame belong to the join.
    fn maybe_follow_sabr_seek(&self) {
        let Some((count, target)) = self.live.stats().map(|st| (st.sabr_seeks, st.sabr_seek_ms)) else {
            self.sabr_seeks_seen.set(None);
            return;
        };
        let started = self.gst.borrow().as_ref().is_some_and(|g| g.first_frame_handle().load(Ordering::Relaxed));
        match self.sabr_seeks_seen.get() {
            _ if !started => self.sabr_seeks_seen.set(Some(count)),
            None => self.sabr_seeks_seen.set(Some(count)),
            Some(seen) if count > seen => {
                self.sabr_seeks_seen.set(Some(count));
                if let (Some(ms), Some(t)) = (target, self.dvr_target_ms.take()) {
                    if ms <= t && t - ms < Self::DVR_CONFIRM_SPAN_MS {
                        tracing::info!("[live-reseek] SABR_SEEK #{count} to {ms}ms confirms the DVR seek to {t}ms");
                        return;
                    }
                }
                if let (Some(ms), Some(g)) = (target, self.gst.borrow().as_ref()) {
                    tracing::info!("[live-reseek] SABR_SEEK #{count} to {ms}ms: seeking the substrate");
                    g.live_seek(ms as f64 / 1000.0 + 0.1);
                }
            }
            Some(_) => {}
        }
    }

    /// A seek inside the live DVR window, stream-absolute seconds; a target at the latest
    /// seekable time returns to the head.
    fn live_dvr_seek(&self, g: &GstSubstrate, secs: f64) {
        let Some(st) = self.live.stats() else { return };
        let (Some(lo), Some(hi)) = (st.live_min_seekable_ms, st.live_max_seekable_ms.or(st.live_head_ms)) else {
            return;
        };
        let ms = ((secs * 1000.0) as i64).clamp(lo, hi.max(lo));
        if ms >= hi - 1000 {
            tracing::info!("[live] seek to the end of the DVR window: back to the head");
            self.dvr_target_ms.set(None);
            self.live.with(|s| s.request_reseek_live());
            return;
        }
        tracing::info!("[live] DVR seek to {ms} ms ({:.1}s behind the head)", (hi - ms) as f64 / 1000.0);
        self.dvr_target_ms.set(Some(ms));
        self.live.with(|s| s.seek(ms));
        g.live_seek(ms as f64 / 1000.0);
    }

    /// The substrate callbacks hold a weak owner; `load` arrives through `&dyn Player`.
    fn rc(&self) -> Rc<Self> {
        self.me.upgrade().expect("a GstPlayer is always owned by an Rc")
    }
}

impl Player for GstPlayer {
    fn widget(&self) -> gtk::Widget {
        self.picture.clone().upcast()
    }

    fn load(&self, prepared: Prepared) {
        self.rc().load_inner(prepared, None);
    }

    fn load_resume(&self, prepared: Prepared, resume_secs: f64) {
        self.rc().load_inner(prepared, Some(resume_secs));
    }

    fn stop(&self) {
        if let Some(old) = self.gst.borrow_mut().take() {
            old.stop();
        }
        self.live.set(None);
        *self.live_readers.borrow_mut() = None;
        self.dvr_target_ms.set(None);
    }

    fn cancel_stream(&self) {
        if let Some(vod) = self.segment_vod() {
            tracing::info!("[engine] cancel_stream: halting the outgoing SABR session");
            vod.stop();
        }
    }

    fn toggle_pause(&self) {
        if let Some(g) = self.gst.borrow().as_ref() {
            g.set_paused(!g.is_paused());
        }
    }

    fn set_paused(&self, paused: bool) {
        if let Some(g) = self.gst.borrow().as_ref() {
            g.set_paused(paused);
        }
    }

    fn is_paused(&self) -> bool {
        self.gst.borrow().as_ref().is_some_and(|g| g.is_paused())
    }

    fn time_pos_checked(&self) -> Option<f64> {
        self.gst.borrow().as_ref().and_then(|g| g.position_secs())
    }

    fn avsync(&self) -> f64 {
        self.gst.borrow().as_ref().map_or(f64::NAN, |g| g.avsync())
    }

    fn duration(&self) -> Option<f64> {
        self.gst.borrow().as_ref().and_then(|g| g.duration_secs())
    }

    fn seek_absolute(&self, secs: f64) {
        let g = self.gst.borrow();
        let Some(g) = g.as_ref() else { return };
        if g.is_live() {
            self.live_dvr_seek(g, secs);
        } else {
            g.seek_absolute(secs);
        }
    }

    fn seek_relative(&self, delta: f64) {
        let g = self.gst.borrow();
        let Some(g) = g.as_ref() else { return };
        if g.is_live() {
            self.live_dvr_seek(g, g.position_secs().unwrap_or(0.0) + delta);
            return;
        }
        let end = g.duration_secs().unwrap_or(f64::MAX);
        g.seek_absolute((g.position_secs().unwrap_or(0.0) + delta).clamp(0.0, end));
    }

    fn volume(&self) -> f64 {
        self.volume.get()
    }

    fn set_volume(&self, v: f64) {
        let v = v.clamp(0.0, 130.0);
        self.volume.set(v);
        if let Some(g) = self.gst.borrow().as_ref() {
            g.set_volume(v);
        }
    }

    fn speed(&self) -> f64 {
        self.gst.borrow().as_ref().map_or(self.requested_speed.get(), |g| g.speed())
    }

    fn set_speed(&self, s: f64) {
        let clamped = s.clamp(0.1, 4.0);
        let was = self.requested_speed.replace(clamped);
        if let Some(g) = self.gst.borrow().as_ref() {
            g.set_speed(clamped);
        }
        if (clamped - was).abs() > f64::EPSILON {
            tracing::info!("[speed] {was:.2}x -> {clamped:.2}x at pos={:.3}", self.time_pos_checked().unwrap_or(f64::NAN));
        }
        // The server paces delivery for the advertised rate (`ClientAbrState.playback_rate`).
        self.live.set_playback_rate(clamped as f32);
        if let Some(vod) = self.segment_vod() {
            vod.set_playback_rate(clamped as f32);
        }
    }

    fn events(&self) -> &PlayerEvents {
        &self.events
    }

    fn live(&self) -> &LiveSession {
        &self.live
    }

    fn live_window(&self) -> Option<(f64, f64)> {
        self.gst.borrow().as_ref()?;
        let st = self.live.stats()?;
        Some((st.live_min_seekable_ms? as f64 / 1000.0, st.live_head_ms? as f64 / 1000.0))
    }

    fn live_thumbnail(&self, secs: f64) -> Option<StoryboardThumb> {
        self.gst.borrow().as_ref()?;
        self.live.with(|s| s.live_thumbnail_at((secs * 1000.0) as i64)).flatten()
    }

    fn live_go_to_head(&self) -> bool {
        if self.gst.borrow().is_none() || !self.live.is_live() {
            return false;
        }
        tracing::info!("[live] back to the live head");
        self.live.with(|s| s.request_reseek_live());
        true
    }

    fn vod_heights(&self) -> Option<Vec<i32>> {
        let mut h = self.segment_vod()?.heights();
        h.dedup();
        Some(h)
    }

    fn set_vod_quality(&self, q: Quality) -> bool {
        let Some(vod) = self.segment_vod() else { return false };
        match q {
            Quality::Height(h) => {
                let itag = vod.pin_height(h);
                tracing::info!("[quality] segment VOD pinned to {h}p (itag {itag:?})");
            }
            Quality::Best => {
                vod.unpin();
                tracing::info!("[quality] segment VOD back to server-driven");
            }
        }
        true
    }

    fn caption_session(&self) -> Option<Arc<SegmentVod>> {
        self.segment_vod()
    }

    fn watch_details(&self) -> Option<innertube::VideoDetails> {
        if let Some(vod) = self.segment_vod() {
            return vod.watch_details().cloned();
        }
        self.live.with(|s| s.watch_details().cloned()).flatten()
    }
}
