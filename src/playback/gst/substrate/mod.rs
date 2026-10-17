use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::GstPaintableSink;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk4 as gtk;

use innertube::{SegmentSource, SegmentVod};

mod diagnostics;
mod feed;
mod pipeline;
mod pump;
mod seek_sync;
mod stretch;

use diagnostics::{frame_gap_logger, log_segments, sink_pos};
#[cfg(test)]
use feed::container_caps;
use feed::spawn_segment_feed;
use pipeline::{build_parse, build_play_track, Parse};
use pump::{pump, PumpRegistry, PumpState};
use seek_sync::SeekSync;
use stretch::Stretch;

/// Decoded frames go to the sink as DMABuf when VA decodes every ladder codec, since the sink
/// takes few software-decoder formats.
fn zero_copy() -> bool {
    static ZC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ZC.get_or_init(|| {
        gst::init().is_ok() && ["vavp9dec", "vah264dec"].iter().all(|f| gst::ElementFactory::find(f).is_some())
    })
}

/// The AV1 setting is on and a hardware AV1 decoder is installed, so an av01 ladder plays.
pub fn decodes_av1() -> bool {
    static AV1: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    crate::settings::read(|s| s.playback.av1)
        && *AV1.get_or_init(|| gst::init().is_ok() && gst::ElementFactory::find("vaav1dec").is_some())
}

pub struct GstStats {
    pub frames: AtomicU64,
    pub pipeline_errors: AtomicU64,
    pub caps: Mutex<Option<String>>,
}

impl Default for GstStats {
    fn default() -> Self {
        Self { frames: AtomicU64::new(0), pipeline_errors: AtomicU64::new(0), caps: Mutex::new(None) }
    }
}

pub struct GstSubstrate {
    play: gst::Pipeline,
    paintable: gtk::gdk::Paintable,
    video_in: gst_app::AppSrc,
    audio_in: gst_app::AppSrc,
    parses: Arc<Mutex<Vec<Parse>>>,
    stop: Arc<AtomicBool>,
    stats: Arc<GstStats>,
    last_pos_ns: Arc<AtomicU64>,
    duration_secs: Arc<Mutex<Option<f64>>>,
    first_frame: Arc<AtomicBool>,
    /// Both tracks reached their end and the pipeline posted EOS.
    ended: Arc<AtomicBool>,
    /// The play pipeline's bus watch lives as long as this guard.
    bus_watch: Mutex<Option<gst::bus::BusWatchGuard>>,
    seek_gen: Arc<AtomicU64>,
    pumps: PumpRegistry,
    want_paused: Arc<AtomicBool>,
    seek_sync: Arc<SeekSync>,
    buffering: Arc<AtomicBool>,
    rate: Arc<AtomicU64>,
    stretch: Arc<Stretch>,
    video_full: Arc<AtomicBool>,
    audio_full: Arc<AtomicBool>,
    video_push: Arc<Mutex<()>>,
    audio_push: Arc<Mutex<()>>,
    /// Re-attach hook for paintable replacement. gtk4paintablesink swaps its paintable on
    /// renegotiation (new caps); the widget follows it or the display freezes on the stale
    /// one while frame counters keep advancing. Fired from notify::paintable. Main thread
    /// only (`Rc`).
    repaint_cb: Rc<RefCell<Option<GstPaintableSink>>>,
    epoch: std::time::Instant,
    vsink: gst::Element,
    asink: gst::Element,
    live: AtomicBool,
    segvod: Mutex<Option<SegVodFeeds>>,
}

/// One segment-session track feed, reachable from a seek.
struct SegFeed {
    tag: &'static str,
    /// Cleared when the feed thread exits: after the end of the video, or on stop.
    running: Arc<AtomicBool>,
    /// The next position the feed reads from, taken by the feed before its next read.
    reposition: Arc<Mutex<Option<i64>>>,
    interrupt: Arc<AtomicBool>,
    /// Held by the feed while it pushes, and by a seek across the parse flush.
    push: Arc<Mutex<()>>,
    parse_src: gst::Element,
}

struct SegVodFeeds {
    /// The VOD session behind the feeds; `None` on live.
    vod: Option<Arc<SegmentVod>>,
    feeds: Vec<SegFeed>,
}

impl GstSubstrate {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        gst::init()?;
        super::wsola_element::register()?;
        let play = gst::Pipeline::new();
        let stats: Arc<GstStats> = Default::default();
        let paint_sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|e| format!("gtk4paintablesink missing: {e}"))?;
        let paintable: gtk::gdk::Paintable = paint_sink.property("paintable");
        let repaint_cb: Rc<RefCell<Option<GstPaintableSink>>> = Rc::new(RefCell::new(None));
        {
            let repaint_slot = repaint_cb.clone();
            // _local: the closure holds an Rc (not Send) and runs on the main thread.
            paint_sink.connect_notify_local(Some("paintable"), move |sink, _| {
                let p: gtk::gdk::Paintable = sink.property("paintable");
                if let Some(cb) = repaint_slot.borrow().as_ref() {
                    tracing::info!("[gst] paintable replaced, re-attaching");
                    cb(&p);
                }
            });
        }
        let first_frame = Arc::new(AtomicBool::new(false));
        if let Some(pad) = paint_sink.static_pad("sink") {
            let stats = stats.clone();
            let first = first_frame.clone();
            let gap = frame_gap_logger("video sink-in".into());
            pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                gap(info);
                first.store(true, Ordering::Relaxed);
                stats.frames.fetch_add(1, Ordering::Relaxed);
                if let Some(caps) = pad.current_caps() {
                    if let Some(s) = caps.structure(0) {
                        if let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) {
                            *stats.caps.lock().unwrap() = Some(format!("{w}x{h}"));
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }
        log_segments("video", &paint_sink);
        let vsink = paint_sink.clone();
        let stretch: Arc<Stretch> = Default::default();
        let (video_in, video_full) = build_play_track(&play, "video-in", paint_sink, stretch.clone())?;
        // The audio-device hook routes test audio to a null sink through pulsesink, the
        // element autoaudiosink picks for interactive use.
        let audio_sink = match crate::hooks::get("audio-device")
            .and_then(|d| d.strip_prefix("pipewire/").map(str::to_owned))
        {
            Some(sink) => {
                let el = gst::ElementFactory::make("pulsesink").property("device", sink.as_str()).build()?;
                tracing::info!(
                    "[gst] audio sink -> pulsesink device {sink} buffer-time {}us latency-time {}us",
                    el.property::<i64>("buffer-time"),
                    el.property::<i64>("latency-time")
                );
                el
            }
            None => gst::ElementFactory::make("autoaudiosink").build()?,
        };
        log_segments("audio", &audio_sink);
        let asink = audio_sink.clone();
        let (audio_in, audio_full) = build_play_track(&play, "audio-in", audio_sink, stretch.clone())?;
        Ok(Self {
            play,
            paintable,
            video_in: video_in.clone(),
            audio_in: audio_in.clone(),
            parses: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
            stats,
            last_pos_ns: Arc::new(AtomicU64::new(0)),
            duration_secs: Arc::new(Mutex::new(None)),
            first_frame,
            ended: Arc::new(AtomicBool::new(false)),
            bus_watch: Mutex::new(None),
            seek_gen: Arc::new(AtomicU64::new(0)),
            pumps: Arc::new(Mutex::new(Vec::new())),
            want_paused: Arc::new(AtomicBool::new(false)),
            buffering: Arc::new(AtomicBool::new(false)),
            seek_sync: Arc::new(SeekSync {
                gen: AtomicU64::new(0),
                pending: AtomicBool::new(false),
                video: AtomicBool::new(false),
                audio: AtomicBool::new(false),
                parse_gen: AtomicU64::new(0),
                releasing: Mutex::new(()),
                srcs: [video_in.clone(), audio_in.clone()],
                armed_at: Mutex::new(std::time::Instant::now()),
                live_first: [AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX)],
            }),
            rate: Arc::new(AtomicU64::new(1000)),
            stretch,
            video_full,
            audio_full,
            video_push: Arc::new(Mutex::new(())),
            audio_push: Arc::new(Mutex::new(())),
            repaint_cb,
            epoch: std::time::Instant::now(),
            vsink,
            asink,
            live: AtomicBool::new(false),
            segvod: Mutex::new(None),
        })
    }

    /// Video minus audio rendered position, in seconds (NaN until both sinks report).
    pub fn avsync(&self) -> f64 {
        match (sink_pos(&self.vsink), sink_pos(&self.asink)) {
            (Some(v), Some(a)) => v - a,
            _ => f64::NAN,
        }
    }

    /// Log when each sink first renders at the seek target: `[gst-seek]` rows score
    /// seek latency and whether audio and video resume together.
    fn trace_seek(&self, target: f64) {
        let (vsink, asink, stats) = (self.vsink.clone(), self.asink.clone(), self.stats.clone());
        let frames0 = stats.frames.load(Ordering::Relaxed);
        let t0 = std::time::Instant::now();
        let (mut v_at, mut a_at): (Option<u128>, Option<u128>) = (None, None);
        let landed = move |p: Option<f64>| p.is_some_and(|p| (p - target).abs() < 6.0);
        gst::glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            let ms = t0.elapsed().as_millis();
            let (v, a) = (sink_pos(&vsink), sink_pos(&asink));
            if v_at.is_none()
                && stats.frames.load(Ordering::Relaxed) > frames0
                && landed(v)
                && v.unwrap_or(0.0) > target + 0.05
            {
                v_at = Some(ms);
                tracing::info!("[gst-seek] target={target:.2}s video renders +{ms}ms at {:.3}s", v.unwrap_or(-1.0));
            }
            // Audio counts as playing once its rendered position moves past where it landed.
            if a_at.is_none() && landed(a) && a.unwrap_or(0.0) > target + 0.05 {
                a_at = Some(ms);
                tracing::info!("[gst-seek] target={target:.2}s audio plays +{ms}ms at {:.3}s", a.unwrap_or(-1.0));
            }
            if let (Some(va), Some(aa)) = (v_at, a_at) {
                let av = match (v, a) {
                    (Some(v), Some(a)) => format!("{:.0}ms", (v - a) * 1000.0),
                    _ => "?".into(),
                };
                tracing::info!("[gst-seek] target={target:.2}s DONE playing +{}ms (video +{va}ms, audio +{aa}ms, gap {}ms) av={av}", va.max(aa), va.abs_diff(aa));
                return gst::glib::ControlFlow::Break;
            }
            if ms > 30_000 {
                tracing::info!("[gst-seek] target={target:.2}s TIMEOUT video={v_at:?} audio={a_at:?} v={v:?} a={a:?}");
                return gst::glib::ControlFlow::Break;
            }
            gst::glib::ControlFlow::Continue
        });
    }

    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        &self.paintable
    }

    /// Register the closure that re-attaches the widget when the sink replaces its paintable
    /// (see `repaint_cb`).
    pub fn set_on_repaintable(&self, f: GstPaintableSink) {
        *self.repaint_cb.borrow_mut() = Some(f);
    }

    pub fn first_frame_handle(&self) -> Arc<AtomicBool> {
        self.first_frame.clone()
    }

    /// `(ended, stopped)`: set when playback reaches the end, and when this substrate stops.
    pub fn end_handles(&self) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        (self.ended.clone(), self.stop.clone())
    }

    pub fn duration_secs(&self) -> Option<f64> {
        *self.duration_secs.lock().unwrap()
    }

    pub fn set_paused(&self, paused: bool) {
        self.want_paused.store(paused, Ordering::Relaxed);
        if !paused && self.buffering.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.play.set_state(if paused { gst::State::Paused } else { gst::State::Playing });
    }

    /// Pause for buffering when either track is about to run dry, and resume once both
    /// hold enough: the clock stops with the pipeline, so audio and video stay together.
    fn watch_buffering(&self) {
        const LOW_NS: i64 = 150_000_000;
        const RESUME_NS: i64 = 1_500_000_000;
        tracing::info!("[gst] buffering pause below {}ms", LOW_NS / 1_000_000);
        let (play, pumps, sync, stop) = (self.play.clone(), self.pumps.clone(), self.seek_sync.clone(), self.stop.clone());
        let (buffering, paused, duration) = (self.buffering.clone(), self.want_paused.clone(), self.duration_secs.clone());
        let mut since: Option<std::time::Instant> = None;
        gst::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if stop.load(Ordering::Relaxed) {
                return gst::glib::ControlFlow::Break;
            }
            if sync.pending.load(Ordering::Relaxed) {
                return gst::glib::ControlFlow::Continue;
            }
            let Some(pos) = play.query_position::<gst::ClockTime>().map(|p| p.nseconds() as i64) else {
                return gst::glib::ControlFlow::Continue;
            };
            let near_end = duration.lock().unwrap().is_some_and(|d| pos as f64 / 1e9 > d - 3.0);
            // Only live pumps that have pushed say anything about what is queued.
            let ahead = |tag: &str| {
                pumps
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(t, st, _, _)| {
                        *t == tag
                            && !st.done.load(Ordering::Relaxed)
                            && st.last_pts_ns.load(Ordering::Relaxed) > 0
                    })
                    .map(|(_, st, _, _)| st.last_pts_ns.load(Ordering::Relaxed) as i64 - pos)
                    .max()
            };
            let least = match (ahead("video"), ahead("audio")) {
                (Some(v), Some(a)) => Some(v.min(a)),
                _ => None,
            };
            let stuck = since.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(10));
            if !buffering.load(Ordering::Relaxed) {
                let Some(least) = least else {
                    return gst::glib::ControlFlow::Continue;
                };
                if least < LOW_NS && !near_end && !paused.load(Ordering::Relaxed) {
                    buffering.store(true, Ordering::Relaxed);
                    since = Some(std::time::Instant::now());
                    let _ = play.set_state(gst::State::Paused);
                    tracing::info!("[gst] buffering at {:.2}s (least ahead {}ms)", pos as f64 / 1e9, least / 1_000_000);
                }
            } else if least.is_some_and(|l| l >= RESUME_NS) || near_end || stuck {
                buffering.store(false, Ordering::Relaxed);
                if !paused.load(Ordering::Relaxed) {
                    let _ = play.set_state(gst::State::Playing);
                }
                let ms = since.take().map_or(0, |t| t.elapsed().as_millis());
                tracing::info!("[gst] buffering done after {ms}ms");
            }
            gst::glib::ControlFlow::Continue
        });
    }

    pub fn is_paused(&self) -> bool {
        self.want_paused.load(Ordering::Relaxed)
    }

    /// Height of the last decoded picture.
    pub fn video_height(&self) -> Option<i32> {
        self.stats.caps.lock().unwrap().as_deref()?.split('x').nth(1)?.parse().ok()
    }

    pub fn is_buffering(&self) -> bool {
        self.buffering.load(Ordering::Relaxed)
    }

    /// Volume on a 0-130 scale (100 = unity), applied to the audio chain.
    pub fn set_volume(&self, v: f64) {
        if let Some(vol) = self.play.by_name("volume") {
            vol.set_property("volume", (v / 100.0).clamp(0.0, 1.3));
        }
    }

    pub fn speed(&self) -> f64 {
        self.rate.load(Ordering::Relaxed) as f64 / 1000.0
    }

    pub fn position_secs(&self) -> Option<f64> {
        // A flushing pipeline still answers with its pre-seek position.
        if self.seek_sync.pending.load(Ordering::Relaxed) {
            return Some(self.last_pos_ns.load(Ordering::Relaxed) as f64 / 1e9);
        }
        if let Some(p) = self.play.query_position::<gst::ClockTime>() {
            let content = self.stretch.to_content(p.nseconds());
            self.last_pos_ns.store(content, Ordering::Relaxed);
            return Some(content as f64 / 1e9);
        }
        match self.last_pos_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(ns as f64 / 1e9),
        }
    }

    /// Play a live stream from its raw segments. Both tracks start at the later of their
    /// first samples; a server reposition moves the cursors (see [`Self::live_seek`]).
    pub fn start_live(&self, video: Box<dyn SegmentSource>, audio: Box<dyn SegmentSource>) -> Result<(), gst::StateChangeError> {
        self.live.store(true, Ordering::Relaxed);
        self.start_feeds(None, [video, audio], None)
    }

    /// Play a VOD from its segment session: each track's segments, in time order and led
    /// by their rung's init, go into that track's parse pipeline.
    pub fn start_segvod(&self, vod: Arc<SegmentVod>, start_ms: i64) -> Result<(), gst::StateChangeError> {
        *self.duration_secs.lock().unwrap() = Some(vod.duration_ms() as f64 / 1000.0);
        vod.set_playhead_ms(start_ms);
        let tracks: [Box<dyn SegmentSource>; 2] = [Box::new(vod.track(true, start_ms)), Box::new(vod.track(false, start_ms))];
        self.start_feeds(Some(vod), tracks, Some(start_ms))
    }

    /// `start_ms` is the VOD open position; `None` anchors a live timeline at its first samples.
    fn start_feeds(&self, vod: Option<Arc<SegmentVod>>, tracks: [Box<dyn SegmentSource>; 2], start_ms: Option<i64>) -> Result<(), gst::StateChangeError> {
        self.watch_bus();
        let mut feeds = Vec::new();
        for (tag, track) in ["video", "audio"].into_iter().zip(tracks) {
            feeds.push(self.start_track(tag, track, start_ms, start_ms.is_some_and(|ms| ms > 0))?);
        }
        *self.segvod.lock().unwrap() = Some(SegVodFeeds { vod, feeds });
        let _ = self.play.set_state(gst::State::Playing);
        self.log_stats();
        self.watch_buffering();
        Ok(())
    }

    /// One track's parse pipeline, segment feed and pump into the play pipeline. `resync`:
    /// the pump joins a pending seek, reporting its first sample at `start_ms` to it.
    fn start_track(
        &self,
        tag: &'static str,
        track: Box<dyn SegmentSource>,
        start_ms: Option<i64>,
        resync: bool,
    ) -> Result<SegFeed, gst::StateChangeError> {
        let parse = build_parse(tag, true)?;
        let appsrc = parse.src.clone().dynamic_cast::<gst_app::AppSrc>().map_err(|_| gst::StateChangeError)?;
        let _ = parse.pipeline.set_state(gst::State::Playing);
        let feed = SegFeed {
            tag,
            reposition: Arc::new(Mutex::new(None)),
            interrupt: track.interrupt_handle(),
            push: Arc::new(Mutex::new(())),
            parse_src: parse.src.clone(),
            running: Arc::new(AtomicBool::new(true)),
        };
        spawn_segment_feed(appsrc, track, feed.reposition.clone(), feed.push.clone(), self.stop.clone(), feed.running.clone(), tag);
        let (dst, lock, flow) = match tag {
            "audio" => (self.audio_in.clone(), self.audio_push.clone(), self.audio_full.clone()),
            _ => (self.video_in.clone(), self.video_push.clone(), self.video_full.clone()),
        };
        let state: Arc<PumpState> = Default::default();
        match start_ms {
            Some(ms) => {
                state.target_ns.store(ms.max(0) as u64 * 1_000_000, Ordering::Relaxed);
                state.resync.store(resync, Ordering::Relaxed);
            }
            None => {
                state.target_ns.store(u64::MAX, Ordering::Relaxed);
                state.live.store(true, Ordering::Relaxed);
            }
        }
        state.need_segment.store(true, Ordering::Relaxed);
        self.pumps.lock().unwrap().push((tag, state.clone(), dst.clone(), parse.src.clone()));
        pump(parse.sink.clone(), dst, self.stop.clone(), self.seek_gen.clone(), state, flow, self.epoch, lock, self.parses.clone(), parse.src.clone(), tag, self.seek_sync.clone());
        self.parses.lock().unwrap().push(parse);
        Ok(feed)
    }

    /// A seek in a segment session: flush both pipelines and move each feed's cursor.
    fn seek_segvod(&self, target_ns: u64) -> bool {
        let mut guard = self.segvod.lock().unwrap();
        let Some(seg) = guard.as_mut() else { return false };
        let secs = target_ns as f64 / 1e9;
        let video_guard = self.video_push.lock().unwrap_or_else(|e| e.into_inner());
        let audio_guard = self.audio_push.lock().unwrap_or_else(|e| e.into_inner());
        let gen = self.seek_gen.fetch_add(1, Ordering::Relaxed) + 1;
        self.seek_sync.arm(gen);
        {
            let (sync, seek_gen) = (self.seek_sync.clone(), self.seek_gen.clone());
            gst::glib::timeout_add_local_once(self.seek_sync.wait() + std::time::Duration::from_secs(1), move || {
                if seek_gen.load(Ordering::Relaxed) == gen {
                    sync.release(gen, "no track arrived");
                }
            });
        }
        self.last_pos_ns.store(target_ns, Ordering::Relaxed);
        self.stretch.reset(target_ns, self.speed());
        tracing::info!("[gst] reposition to {secs:.2}s (gen {gen})");
        self.trace_seek(secs);
        self.pumps.lock().unwrap().retain(|(_, st, _, _)| !st.done.load(Ordering::Relaxed));
        for (_, st, src, _) in self.pumps.lock().unwrap().iter() {
            st.target_ns.store(target_ns, Ordering::Relaxed);
            st.resync.store(true, Ordering::Relaxed);
            st.need_segment.store(true, Ordering::Relaxed);
            let _ = src.send_event(gst::event::FlushStart::new());
        }
        self.video_full.store(false, Ordering::Relaxed);
        self.audio_full.store(false, Ordering::Relaxed);
        drop((video_guard, audio_guard));
        let ms = (target_ns / 1_000_000) as i64;
        if let Some(vod) = &seg.vod {
            vod.set_playhead_ms(ms);
        } else {
            for slot in &self.seek_sync.live_first {
                slot.store(u64::MAX, Ordering::Relaxed);
            }
        }
        for f in &seg.feeds {
            *f.reposition.lock().unwrap() = Some(ms);
            f.interrupt.store(true, Ordering::Relaxed);
        }
        // FlushStart first: it unblocks a feed parked in a full appsrc, which then
        // releases its push lock.
        // The generation is published while every feed is held, so the first sample a feed
        // pushes after the flush (a live track's keyframe) is never judged stale.
        for f in &seg.feeds {
            let _ = f.parse_src.send_event(gst::event::FlushStart::new());
        }
        let held: Vec<_> = seg.feeds.iter().map(|f| f.push.lock().unwrap()).collect();
        for f in &seg.feeds {
            let _ = f.parse_src.send_event(gst::event::FlushStop::new(true));
        }
        self.seek_sync.parse_gen.store(gen, Ordering::Relaxed);
        drop(held);
        // A track that played to the end has sent EOS and let its chain go; a seek back
        // reopens it from the target, as MSE reopens a source buffer after endOfStream.
        if let Some(vod) = seg.vod.clone() {
            for feed in seg.feeds.iter_mut().filter(|f| !f.running.load(Ordering::Relaxed)) {
                let tag = feed.tag;
                let dst = if tag == "audio" { &self.audio_in } else { &self.video_in };
                let _ = dst.send_event(gst::event::FlushStart::new());
                match self.start_track(tag, Box::new(vod.track(tag == "video", ms)), Some(ms), true) {
                    Ok(f) => {
                        tracing::info!("[gst] {tag}: track reopened at {ms} ms after its end");
                        *feed = f;
                    }
                    Err(e) => tracing::warn!("[gst] {tag}: could not reopen the track: {e:?}"),
                }
            }
        }
        true
    }

    /// The segment session playing, if any.
    pub fn segment_vod(&self) -> Option<Arc<SegmentVod>> {
        self.segvod.lock().unwrap().as_ref().and_then(|s| s.vod.clone())
    }

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }

    pub fn seek_absolute(&self, secs: f64) {
        if self.is_live() {
            return;
        }
        let target = gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9) as u64);
        self.seek_segvod(target.nseconds());
    }

    /// Live reposition to a stream-absolute time, as the web player seeks its media
    /// element on a server `SABR_SEEK`: both cursors move and both pipelines flush.
    pub fn live_seek(&self, secs: f64) {
        if self.is_live() {
            self.seek_segvod((secs.max(0.0) * 1e9) as u64);
        }
    }

    pub fn set_speed(&self, rate: f64) {
        let rate = rate.clamp(0.1, 4.0);
        let was = self.rate.load(Ordering::Relaxed) as f64 / 1000.0;
        if (rate - was).abs() < f64::EPSILON {
            return;
        }
        self.rate.store((rate * 1000.0).round() as u64, Ordering::Relaxed);
        tracing::info!("[gst] speed {was:.2}x -> {rate:.2}x uptime {}ms", crate::uptime_ms());
        self.stretch.request();
        if let Some(tempo) = self.play.by_name("tempo") {
            tempo.set_property("tempo", rate as f32);
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(seg) = self.segvod.lock().unwrap().take() {
            if let Some(vod) = seg.vod {
                vod.stop();
            }
        }
        for p in self.parses.lock().unwrap().drain(..) {
            let _ = p.pipeline.set_state(gst::State::Null);
        }
        let _ = self.play.set_state(gst::State::Null);
    }

    fn watch_bus(&self) {
        let errors = self.stats.clone();
        let ended = self.ended.clone();
        let Some(bus) = self.play.bus() else { return };
        let watch = bus.add_watch_local(move |_, msg| {
            match msg.view() {
                gst::MessageView::Eos(_) => {
                    tracing::info!("[gst] end of stream uptime {}ms", crate::uptime_ms());
                    ended.store(true, Ordering::Relaxed);
                }
                gst::MessageView::Error(e) => {
                    errors.pipeline_errors.fetch_add(1, Ordering::Relaxed);
                    tracing::error!("[gst] ERROR from {:?}: {} ({:?})", msg.src().map(|s| s.path_string()), e.error(), e.debug());
                }
                gst::MessageView::Warning(w) => tracing::warn!("[gst] WARN from {:?}: {}", msg.src().map(|s| s.path_string()), w.error()),
                gst::MessageView::StateChanged(s) if s.src().is_some_and(|o| o.type_().name() == "GstPipeline") => {
                    tracing::info!("[gst] STATE {:?} -> {:?} pending {:?}", s.old(), s.current(), s.pending());
                }
                gst::MessageView::AsyncDone(_) => tracing::info!("[gst] ASYNC_DONE uptime {}ms", crate::uptime_ms()),
                gst::MessageView::AsyncStart(_) => tracing::info!("[gst] ASYNC_START"),
                gst::MessageView::Qos(q) => tracing::info!(
                    "[gst] QOS from {:?}: live={} running_time={} stream_time={} timestamp={} duration={}",
                    msg.src().map(|s| s.path_string()),
                    q.live(),
                    q.running_time().map(|t| format!("{t}")).unwrap_or_else(|| "-".into()),
                    q.stream_time().map(|t| format!("{t}")).unwrap_or_else(|| "-".into()),
                    q.timestamp().map(|t| format!("{t}")).unwrap_or_else(|| "-".into()),
                    q.duration().map(|t| format!("{t}")).unwrap_or_else(|| "-".into()),
                ),
                _ => {}
            }
            gst::glib::ControlFlow::Continue
        });
        *self.bus_watch.lock().unwrap() = watch.ok();
    }

    fn log_stats(&self) {
        let last_pos_tick = self.last_pos_ns.clone();
        let sync = self.seek_sync.clone();
        let stats = self.stats.clone();
        let stop = self.stop.clone();
        let play = self.play.clone();
        let pumps = self.pumps.clone();
        let epoch = self.epoch;
        let start = std::time::Instant::now();
        let mut last = 0u64;
        let (vsink, asink) = (self.vsink.clone(), self.asink.clone());
        // Per-second `[gst-picture]` row read by verdict-picture.py: sink frames delivered,
        // pipeline position and caps.
        gst::glib::timeout_add_seconds_local(1, move || {
            if stop.load(Ordering::Relaxed) {
                return gst::glib::ControlFlow::Break;
            }
            let frames = stats.frames.load(Ordering::Relaxed);
            // The feed threads bound themselves against this, so refresh it here too and
            // not only when a caller happens to ask for the position.
            if let Some(p) = play.query_position::<gst::ClockTime>().filter(|_| !sync.pending.load(Ordering::Relaxed)) {
                last_pos_tick.store(p.nseconds(), Ordering::Relaxed);
            }
            let pos = play.query_position::<gst::ClockTime>().map(|p| p.seconds_f64()).unwrap_or(-1.0);
            let (v, a) = (sink_pos(&vsink).unwrap_or(-1.0), sink_pos(&asink).unwrap_or(-1.0));
            let drops = vsink.property::<gst::Structure>("stats").get::<u64>("dropped").unwrap_or(0);
            tracing::info!(
                "[gst-picture] t={}s frames={frames} (+{}) pos={pos:.2}s caps={} v={v:.3} a={a:.3} av={:.0}ms drops={drops}",
                start.elapsed().as_secs(),
                frames - last,
                stats.caps.lock().unwrap().clone().unwrap_or_else(|| "-".into()),
                (v - a) * 1000.0,
            );
            last = frames;
            gst::glib::ControlFlow::Continue
        });
        let stats = self.stats.clone();
        let stop = self.stop.clone();
        let play = self.play.clone();
        gst::glib::timeout_add_seconds_local(5, move || {
            if stop.load(Ordering::Relaxed) {
                return gst::glib::ControlFlow::Break;
            }
            let frames = stats.frames.load(Ordering::Relaxed);
            let pos = play.query_position::<gst::ClockTime>().map(|p| p.seconds_f64()).unwrap_or(-1.0);
            tracing::warn!(
                "[gst +{}s] frames={frames} pos={pos:.2}s errors={} caps={}",
                start.elapsed().as_secs(),
                stats.pipeline_errors.load(Ordering::Relaxed),
                stats.caps.lock().unwrap().clone().unwrap_or_else(|| "-".into()),
            );
            for (tag, st, src, parse_src) in pumps.lock().unwrap().iter() {
                let level = src.property::<u64>("current-level-time") as f64 / 1e9;
                let bytes = src.property::<u64>("current-level-bytes");
                let pq = parse_src.property::<u64>("current-level-bytes");
                tracing::info!("[gst   pump] {tag}: {} level={level:.2}s/{bytes}B parseq={pq}B", st.report(epoch));
            }
            gst::glib::ControlFlow::Continue
        });
    }
}

#[cfg(test)]
mod flush_tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/frag-h264.mp4");

    fn boxes(b: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 8 <= b.len() {
            let size = u32::from_be_bytes(b[i..i + 4].try_into().unwrap()) as usize;
            out.push(&b[i..i + size]);
            i += size;
        }
        out
    }

    /// Feeds init + one fragment + half of the next, flushes the parse chain the way a seek
    /// does, then feeds init + a whole fragment. Returns the samples parsed after the flush.
    fn flush_mid_mdat() -> usize {
        gst::init().unwrap();
        let b = boxes(FIXTURE);
        let init = [b[0], b[1]].concat();
        let (frag1, frag2, frag3) = ([b[2], b[3]].concat(), [b[4], b[5]].concat(), [b[6], b[7]].concat());
        let parse = build_parse("video", true).unwrap();
        let src = parse.src.clone().dynamic_cast::<gst_app::AppSrc>().unwrap();
        src.set_caps(container_caps(&init).as_ref());
        parse.pipeline.set_state(gst::State::Playing).unwrap();
        let push = |bytes: &[u8]| src.push_buffer(gst::Buffer::from_slice(bytes.to_vec())).unwrap();
        push(&init);
        push(&frag1);
        push(&frag2[..frag2.len() / 2]);
        for _ in 0..30 {
            parse.sink.try_pull_sample(gst::ClockTime::from_seconds(2)).expect("fragment 1 parses");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        src.send_event(gst::event::FlushStart::new());
        src.send_event(gst::event::FlushStop::new(true));
        push(&init);
        push(&frag3);
        let mut after = 0;
        while parse.sink.try_pull_sample(gst::ClockTime::from_mseconds(500)).is_some() {
            after += 1;
        }
        parse.pipeline.set_state(gst::State::Null).unwrap();
        after
    }

    #[test]
    fn the_parser_recovers_after_a_flush_inside_an_mdat() {
        assert_eq!(flush_mid_mdat(), 30);
    }

    fn waiting_appsrc() -> (gst::Pipeline, gst_app::AppSrc) {
        let pipeline = gst::Pipeline::new();
        let src = gst::ElementFactory::make("appsrc").build().unwrap();
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();
        pipeline.add_many([&src, &sink]).unwrap();
        src.link(&sink).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        (pipeline, src.dynamic_cast::<gst_app::AppSrc>().unwrap())
    }

    fn seek_sync(srcs: [gst_app::AppSrc; 2]) -> SeekSync {
        SeekSync {
            gen: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            video: AtomicBool::new(false),
            audio: AtomicBool::new(false),
            parse_gen: AtomicU64::new(0),
            releasing: Mutex::new(()),
            srcs,
            armed_at: Mutex::new(std::time::Instant::now()),
            live_first: [AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX)],
        }
    }

    #[test]
    fn a_release_completes_on_an_appsrc_the_seek_never_flushed() {
        gst::init().unwrap();
        let ((pa, a), (pb, b)) = (waiting_appsrc(), waiting_appsrc());
        let sync = Arc::new(seek_sync([a, b]));
        sync.arm(1);
        let (tx, rx) = std::sync::mpsc::channel();
        let s = sync.clone();
        std::thread::spawn(move || {
            s.release(1, "test");
            let _ = tx.send(());
        });
        let done = rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok();
        assert!(done, "release() blocked on an appsrc waiting for data");
        assert!(!sync.pending.load(Ordering::Relaxed));
        let _ = (pa.set_state(gst::State::Null), pb.set_state(gst::State::Null));
    }

    #[test]
    fn a_release_for_an_older_seek_leaves_the_newer_flush_pending() {
        gst::init().unwrap();
        let ((pa, a), (pb, b)) = (waiting_appsrc(), waiting_appsrc());
        let sync = seek_sync([a, b]);
        sync.arm(2);
        sync.release(1, "stale");
        assert!(sync.pending.load(Ordering::Relaxed), "a stale release must not end seek 2's flush");
        let _ = (pa.set_state(gst::State::Null), pb.set_state(gst::State::Null));
    }
}
