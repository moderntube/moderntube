//! The pump threads: parsed samples from a parse pipeline into the play pipeline.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer_app as gst_app;

use super::pipeline::{teardown_parse, Parse};
use super::seek_sync::SeekSync;

pub(super) const SEEK_SLACK_NS: u64 = 6_000_000_000;
pub(super) const RESYNC_GIVE_UP: u64 = 40_000;

/// A VOD feed restarts at the fragment holding the target, so its first new-generation
/// sample is the random access point before it; a live track joins on a keyframe near it.
pub(super) fn resync_accepts(live: bool, key: bool, pts: Option<u64>, target: u64) -> bool {
    !live || (key && pts.is_some_and(|p| p + SEEK_SLACK_NS >= target && p <= target + SEEK_SLACK_NS))
}

pub(super) fn is_delta(sample: &gst::Sample) -> bool {
    sample.buffer().is_some_and(|b| b.flags().contains(gst::BufferFlags::DELTA_UNIT))
}

/// MSE's "need random access point" flag: after a parser reset a track
/// drops coded frames until a random access point.
pub(super) struct RapGate(bool);

impl RapGate {
    pub(super) fn admit(&mut self, delta: bool) -> bool {
        if self.0 && delta {
            return false;
        }
        self.0 = false;
        true
    }
}

#[derive(Default)]
pub(super) struct PumpState {
    pub(super) resync: AtomicBool,
    pub(super) target_ns: AtomicU64,
    pub(super) last_pts_ns: AtomicU64,
    pub(super) seg_start_ns: AtomicU64,
    pub(super) seg_rate_milli: AtomicU64,
    pub(super) need_segment: AtomicBool,
    /// Live: a sample at or behind what this track already pushed is an overlapping
    /// append and is dropped, as a `SourceBuffer` absorbs one.
    pub(super) live: AtomicBool,
    pub(super) done: AtomicBool,
    pub(super) phase: AtomicU64,
    pub(super) pulled: AtomicU64,
    pub(super) pushed: AtomicU64,
    pub(super) dropped_stale: AtomicU64,
    pub(super) since_ms: AtomicU64,
}

impl PumpState {
    pub(super) fn enter(&self, phase: u64, epoch: std::time::Instant) {
        self.phase.store(phase, Ordering::Relaxed);
        self.since_ms.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    pub(super) fn report(&self, epoch: std::time::Instant) -> String {
        let now = epoch.elapsed().as_millis() as u64;
        let held = now.saturating_sub(self.since_ms.load(Ordering::Relaxed));
        let phase = match self.phase.load(Ordering::Relaxed) {
            1 => "wait",
            2 => "push",
            _ => "pull",
        };
        format!(
            "in {phase} {held}ms ahead={:.2}s pulled={} pushed={} stale={}",
            self.last_pts_ns.load(Ordering::Relaxed) as f64 / 1e9,
            self.pulled.load(Ordering::Relaxed),
            self.pushed.load(Ordering::Relaxed),
            self.dropped_stale.load(Ordering::Relaxed),
        )
    }
}

pub(super) type PumpEntry = (&'static str, Arc<PumpState>, gst_app::AppSrc, gst::Element);
pub(super) type PumpRegistry = Arc<Mutex<Vec<PumpEntry>>>;

#[allow(clippy::too_many_arguments)]
pub(super) fn pump(
    from: gst_app::AppSink,
    to: gst_app::AppSrc,
    stop: Arc<AtomicBool>,
    seek_gen: Arc<AtomicU64>,
    state: Arc<PumpState>,
    full: Arc<AtomicBool>,
    epoch: std::time::Instant,
    push_lock: Arc<Mutex<()>>,
    parses: Arc<Mutex<Vec<Parse>>>,
    src_elem: gst::Element,
    tag: &'static str,
    seek_sync: Arc<SeekSync>,
) {
    std::thread::spawn(move || {
        let mut n: u64 = 0;
        let mut first_after_resync = false;
        let mut rap = RapGate(true);
        let mut rap_dropped: u64 = 0;
        let mut live_high: Option<u64> = None;
        let mut live_behind = false;
        let mut live_dropped: u64 = 0;
        // Samples a resync has logged skipping; the first few after each seek tell why.
        let mut skip_logged = 0u32;
        let (mut ts_prev, mut ts_step, mut ts_caps): (Option<u64>, Option<u64>, Option<String>) = (None, None, None);
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            state.enter(0, epoch);
            let sample = match from.pull_sample() {
                Ok(s) => s,
                // The play appsrc outlives this parse pipeline, so a pump end leaves it open;
                // teardown NULLs the play pipeline.
                Err(e) => {
                    let eos = from.is_eos();
                    tracing::info!("[gst] {tag}: pump end ({e:?}) after {n} samples, eos={eos}");
                    if eos && !stop.load(Ordering::Relaxed) {
                        let r = to.end_of_stream();
                        tracing::info!("[gst] {tag}: end of track -> play pipeline ({r:?})");
                    }
                    teardown_parse(&parses, &src_elem, tag, "pump end");
                    break;
                }
            };
            // The sample belongs to the seek generation current when it came out of the
            // parser; a pull can block across a whole seek and return its first sample.
            let gen_before = seek_gen.load(Ordering::Relaxed);
            let parse_flushed = seek_sync.parse_gen.load(Ordering::Relaxed) == gen_before;
            state.pulled.fetch_add(1, Ordering::Relaxed);
            if state.live.load(Ordering::Relaxed) && state.resync.load(Ordering::Relaxed) {
                live_high = None;
                live_behind = false;
            }
            if state.live.load(Ordering::Relaxed) {
                let at = sample.buffer().and_then(|b| b.dts().or(b.pts())).map(|t| t.nseconds());
                // Untimed samples belong to the timed one before them.
                if let Some(t) = at {
                    live_behind = live_high.is_some_and(|h| t <= h);
                    if !live_behind {
                        live_high = Some(t);
                    }
                }
                if live_behind {
                    live_dropped += 1;
                    if state.resync.load(Ordering::Relaxed) && skip_logged < 4 {
                        skip_logged += 1;
                        tracing::info!("[gst] {tag}: resync skips an overlapping sample at {:.3}s", at.unwrap_or(0) as f64 / 1e9);
                    }
                    if live_dropped.is_power_of_two() {
                        tracing::info!("[gst] {tag}: {live_dropped} overlapping live samples dropped (at {:.3}s)", at.unwrap_or(0) as f64 / 1e9);
                    }
                    continue;
                }
            }
            if !parse_flushed {
                state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                if skip_logged < 4 {
                    skip_logged += 1;
                    let pts = sample.buffer().and_then(|b| b.pts()).map_or(0, |p| p.nseconds());
                    tracing::info!("[gst] {tag}: resync skips a pre-flush sample at {:.3}s", pts as f64 / 1e9);
                }
                continue;
            }
            if seek_gen.load(Ordering::Relaxed) != gen_before {
                state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if state.resync.load(Ordering::Relaxed) {
                let target = state.target_ns.load(Ordering::Relaxed);
                let pts = sample.buffer().and_then(|b| b.pts()).map(|p| p.nseconds());
                let decodable = tag == "audio" || !is_delta(&sample);
                if !resync_accepts(state.live.load(Ordering::Relaxed), decodable, pts, target) {
                    let dropped = state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                    if skip_logged < 4 {
                        skip_logged += 1;
                        tracing::info!(
                            "[gst] {tag}: resync skips pts={:.3}s key={decodable} target={:.3}s",
                            pts.unwrap_or(0) as f64 / 1e9,
                            target as f64 / 1e9
                        );
                    }
                    if dropped < RESYNC_GIVE_UP {
                        continue;
                    }
                    tracing::warn!("[gst] {tag}: resync window gave up after {dropped} samples");
                }
                // Under the push lock the seek also holds: a sample from before a seek that
                // lands now must not consume the resync that seek just armed.
                {
                    let _g = push_lock.lock().unwrap_or_else(|e| e.into_inner());
                    if seek_gen.load(Ordering::Relaxed) != gen_before {
                        state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    state.resync.store(false, Ordering::Relaxed);
                }
                first_after_resync = true;
                skip_logged = 0;
                rap = RapGate(true);
                tracing::info!("[gst] {tag}: resynced at {:.3}s", pts.unwrap_or(0) as f64 / 1e9);
            }
            if !rap.admit(tag == "video" && is_delta(&sample)) {
                let dropped = rap_dropped;
                rap_dropped += 1;
                if dropped < 4 {
                    let pts = sample.buffer().and_then(|b| b.pts()).map_or(0, |p| p.nseconds());
                    tracing::info!("[gst] {tag}: needs a random access point, drops a delta at {:.3}s", pts as f64 / 1e9);
                }
                continue;
            }
            if rap_dropped > 0 {
                tracing::info!("[gst] {tag}: random access point after {rap_dropped} dropped deltas");
                rap_dropped = 0;
            }
            let fresh = state.need_segment.swap(false, Ordering::Relaxed);
            // Segments stay at rate 1: speed is the tempo stage plus the video restamp.
            let want_rate = 1000;
            let seg = {
                let (at, base) = if fresh {
                    // Every track's segment starts at the seek target (the sinks clip the
                    // pre-roll); a live join has none and takes the later first sample.
                    let at = {
                        match state.target_ns.load(Ordering::Relaxed) {
                            t if t == u64::MAX => {
                                let pts = sample.buffer().and_then(|b| b.pts()).map_or(0, |p| p.nseconds());
                                let (mine, other) = if tag == "audio" { (1, 0) } else { (0, 1) };
                                seek_sync.live_first[mine].store(pts, Ordering::Relaxed);
                                let t0 = std::time::Instant::now();
                                while seek_sync.live_first[other].load(Ordering::Relaxed) == u64::MAX
                                    && t0.elapsed() < std::time::Duration::from_secs(3)
                                    && !stop.load(Ordering::Relaxed)
                                {
                                    std::thread::sleep(std::time::Duration::from_millis(2));
                                }
                                let at = match seek_sync.live_first[other].load(Ordering::Relaxed) {
                                    u64::MAX => pts,
                                    o => o.max(pts),
                                };
                                state.target_ns.store(at, Ordering::Relaxed);
                                at
                            }
                            t => t,
                        }
                    };
                    let base = 0;
                    state.seg_start_ns.store(at, Ordering::Relaxed);
                    state.seg_rate_milli.store(want_rate, Ordering::Relaxed);
                    (at, base)
                } else {
                    (state.seg_start_ns.load(Ordering::Relaxed), 0)
                };
                let mut g = gst::Segment::new();
                g.set_format(gst::Format::Time);
                g.set_rate(state.seg_rate_milli.load(Ordering::Relaxed).max(1) as f64 / 1000.0);
                let t = gst::GenericFormattedValue::Time(Some(gst::ClockTime::from_nseconds(at)));
                g.set_start(t);
                g.set_time(t);
                g.set_position(t);
                g.set_base(gst::GenericFormattedValue::Time(Some(gst::ClockTime::from_nseconds(base))));
                if fresh {
                    tracing::info!("[gst] {tag}: segment start {:.3}s base {:.3}s", at as f64 / 1e9, base as f64 / 1e9);
                }
                Some(g)
            };
            let sample = restamp(&sample, seg, first_after_resync || fresh);
            if let Some(pts) = sample.buffer().and_then(|b| b.pts()) {
                state.last_pts_ns.store(pts.nseconds(), Ordering::Relaxed);
                if tag == "audio" {
                    let pts = pts.nseconds();
                    let caps = sample.caps().map(|c| c.to_string());
                    if ts_caps.is_some() && caps != ts_caps {
                        tracing::info!("[gst-ts] audio caps change at {:.3}s: {}", pts as f64 / 1e9, caps.as_deref().unwrap_or("none"));
                    }
                    if let (Some(prev), Some(step)) = (ts_prev, ts_step) {
                        let delta = pts as i64 - prev as i64;
                        if (delta - step as i64).abs() > 1_000_000 {
                            tracing::info!(
                                "[gst-ts] audio pts step {:.2}ms (frame {:.2}ms) at {:.3}s",
                                delta as f64 / 1e6,
                                step as f64 / 1e6,
                                pts as f64 / 1e9
                            );
                        }
                    }
                    if let Some(prev) = ts_prev.filter(|&p| pts > p) {
                        ts_step = Some(pts - prev);
                    }
                    ts_prev = Some(pts);
                    ts_caps = caps;
                }
            }
            if n < 3 {
                let pts = sample
                    .buffer()
                    .and_then(|b| b.pts())
                    .map(|p| format!("{:.3}s", p.seconds_f64()))
                    .unwrap_or_else(|| "none".into());
                let caps = sample.caps().map(|c| c.to_string()).unwrap_or_else(|| "none".into());
                let has_seg = sample.segment().is_some();
                tracing::info!("[gst] {tag}: pumped #{n} pts={pts} caps=[{caps}] seg={has_seg}");
            }
            // Wait for room, re-checking `stop` and the seek generation every step so a seek
            // mid-wait is honoured. With `block=false` appsrc's `max-time` is advisory (it
            // emits enough-data/need-data and keeps accepting), so the pump gates on the `full`
            // flag those signals drive. SABR audio buffers carry no duration, so
            // `current-level-time` is meaningless for audio while the flag tracks the queue.
            if std::mem::take(&mut first_after_resync) {
                seek_sync.ready(tag, gen_before, &seek_gen, &stop);
            }
            seek_sync.wait_released(gen_before, &seek_gen, &stop);
            state.enter(1, epoch);
            while !stop.load(Ordering::Relaxed)
                && seek_gen.load(Ordering::Relaxed) == gen_before
                && full.load(Ordering::Relaxed)
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if seek_gen.load(Ordering::Relaxed) != gen_before {
                state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
            state.enter(2, epoch);
            let guard = push_lock.lock().unwrap_or_else(|e| e.into_inner());
            if seek_gen.load(Ordering::Relaxed) != gen_before {
                drop(guard);
                state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let mut result = to.push_sample(&sample);
            drop(guard);
            // A flush in progress is transient: retry the same sample until it ends.
            let t0 = std::time::Instant::now();
            while result == Err(gst::FlowError::Flushing)
                && t0.elapsed() < std::time::Duration::from_secs(5)
                && seek_gen.load(Ordering::Relaxed) == gen_before
                && !stop.load(Ordering::Relaxed)
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
                let _g = push_lock.lock().unwrap_or_else(|e| e.into_inner());
                result = to.push_sample(&sample);
            }
            if seek_gen.load(Ordering::Relaxed) != gen_before {
                state.dropped_stale.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if let Err(e) = result {
                tracing::warn!("[gst] {tag}: push failed after {n} samples: {e:?}");
                break;
            }
            state.pushed.fetch_add(1, Ordering::Relaxed);
            n += 1;
        }
        // Handover and error exits never end the shared appsrc (see the pull_sample match
        // above). EOS belongs to a track that finished, which already propagated it.
        // Teardown (`stop()`) sets the play pipeline to NULL instead.
        state.done.store(true, Ordering::Relaxed);
    });
}

/// `keep_discont` false clears DISCONT: each segment piece gets a fresh parse, which flags
/// its first buffer, and the audio sink re-aligns on every flag, leaving a sliver of silence.
pub(super) fn restamp(sample: &gst::Sample, segment: Option<gst::Segment>, keep_discont: bool) -> gst::Sample {
    let Some(buf) = sample.buffer() else { return sample.clone() };
    let caps = sample.caps().map(|c| c.to_owned());
    let mut copy = buf.copy_deep().expect("buffer copy");
    if !keep_discont {
        copy.make_mut().unset_flags(gst::BufferFlags::DISCONT);
    }
    let mut out = gst::Sample::builder().buffer(&copy);
    if let Some(caps) = caps.as_ref() {
        out = out.caps(caps);
    }
    if let Some(seg) = segment.as_ref() {
        out = out.segment(seg);
    }
    out.build()
}

#[cfg(test)]
mod resync_tests {
    use super::*;

    #[test]
    fn delta_frames_after_a_reset_are_dropped_until_a_keyframe() {
        let mut gate = RapGate(true);
        assert!(!gate.admit(true));
        assert!(!gate.admit(true));
        assert!(gate.admit(false));
        assert!(gate.admit(true));
    }

}
