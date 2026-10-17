//! `mtwsola`: a GStreamer element around [`Wsola`] with a live-adjustable `tempo`.
//!
//! Output timestamps follow `pitch`'s contract, which the video restamp relies on: a
//! segment starting at `s` leaves as `s / tempo`, and output time then advances by the
//! frames produced. The division is f64 so live stream times stay exact.

use std::sync::Mutex;

use gstreamer as gst;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

use super::wsola::Wsola;

/// Called when a new tempo takes effect: (content ns, output ns, tempo) at the switch.
pub type SwitchHook = Box<dyn Fn(u64, u64, f64) + Send + Sync>;

/// The scale for a new segment's times.
pub type SegmentScale = Box<dyn Fn() -> f64 + Send + Sync>;

/// An input gap larger than this advances the output clock instead of being played through.
const GAP_NS: u64 = 40_000_000;

fn frames_to_ns(frames: u64, rate: u32) -> u64 {
    (u128::from(frames) * 1_000_000_000 / u128::from(rate.max(1))) as u64
}

mod imp {
    use super::*;

    #[derive(Default)]
    struct State {
        wsola: Option<Wsola>,
        channels: usize,
        rate: u32,
        seg_tempo: f64,
        /// The tempo being applied, and the content time of input frame 0 since the flush.
        cur_tempo: f64,
        in_origin: Option<u64>,
        in_gap_ns: u64,
        out_origin: Option<u64>,
        out_frames: u64,
        in_expected: Option<u64>,
        discont: bool,
    }

    pub struct Tempo {
        sinkpad: gst::Pad,
        srcpad: gst::Pad,
        state: Mutex<State>,
        tempo: Mutex<f32>,
        pub(super) hook: Mutex<Option<SwitchHook>>,
        pub(super) segment_scale: Mutex<Option<SegmentScale>>,
    }

    impl Tempo {
        fn tempo(&self) -> f64 {
            f64::from(*self.tempo.lock().unwrap())
        }

        fn sink_chain(&self, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
            let tempo = self.tempo();
            let out = {
                let mut st = self.state.lock().unwrap();
                let (channels, rate) = (st.channels, st.rate);
                let Some(_) = st.wsola.as_ref() else { return Err(gst::FlowError::NotNegotiated) };
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let samples: Vec<f32> =
                    map.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                drop(map);
                let frames = (samples.len() / channels) as u64;
                if buffer.flags().contains(gst::BufferFlags::DISCONT) {
                    st.discont = true;
                }
                if let Some(pts) = buffer.pts().map(|p| p.nseconds()) {
                    if st.out_origin.is_none() {
                        st.out_origin = Some((pts as f64 / st.seg_tempo) as u64);
                        st.in_origin = Some(pts);
                        st.out_frames = 0;
                    } else if let Some(exp) = st.in_expected.filter(|&e| pts > e + GAP_NS) {
                        let gap = pts - exp;
                        st.in_gap_ns += gap;
                        st.out_origin = st.out_origin.map(|o| o + (gap as f64 / tempo) as u64);
                        tracing::info!("[wsola] input gap {:.1}ms", gap as f64 / 1e6);
                    }
                    st.in_expected = Some(pts + frames_to_ns(frames, rate));
                }
                if tempo != st.cur_tempo {
                    st.cur_tempo = tempo;
                    let consumed = st.wsola.as_ref().map_or(0.0, |w| w.consumed()) as u64;
                    let c0 = st.in_origin.unwrap_or(0) + st.in_gap_ns + frames_to_ns(consumed, rate);
                    let o0 = st.out_origin.unwrap_or(0) + frames_to_ns(st.out_frames, rate);
                    if let Some(hook) = self.hook.lock().unwrap().as_ref() {
                        hook(c0, o0, tempo);
                    }
                }
                let w = st.wsola.as_mut().unwrap();
                w.push_interleaved(&samples);
                let mut rendered = Vec::new();
                w.process(tempo, &mut rendered);
                if rendered.is_empty() {
                    return Ok(gst::FlowSuccess::Ok);
                }
                let n = (rendered.len() / channels) as u64;
                let origin = st.out_origin.unwrap_or(0);
                let pts = origin + frames_to_ns(st.out_frames, rate);
                let end = origin + frames_to_ns(st.out_frames + n, rate);
                st.out_frames += n;
                let bytes: Vec<u8> = rendered.iter().flat_map(|s| s.to_le_bytes()).collect();
                let mut out = gst::Buffer::from_mut_slice(bytes);
                {
                    let b = out.get_mut().unwrap();
                    b.set_pts(gst::ClockTime::from_nseconds(pts));
                    b.set_duration(gst::ClockTime::from_nseconds(end - pts));
                    if std::mem::take(&mut st.discont) {
                        b.set_flags(gst::BufferFlags::DISCONT);
                    }
                }
                out
            };
            self.srcpad.push(out)
        }

        fn reset_timeline(st: &mut State) {
            if let Some(w) = st.wsola.as_mut() {
                w.flush();
            }
            st.out_origin = None;
            st.in_origin = None;
            st.in_gap_ns = 0;
            st.out_frames = 0;
            st.in_expected = None;
            st.discont = true;
        }

        fn sink_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
            match event.view() {
                gst::EventView::Caps(c) => {
                    let s = c.caps().structure(0);
                    let channels = s.and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2) as usize;
                    let rate = s.and_then(|s| s.get::<i32>("rate").ok()).unwrap_or(48_000) as u32;
                    let mut st = self.state.lock().unwrap();
                    if st.channels != channels || st.rate != rate || st.wsola.is_none() {
                        st.wsola = Some(Wsola::new(channels, rate));
                        st.channels = channels;
                        st.rate = rate;
                    }
                    drop(st);
                    self.srcpad.push_event(event)
                }
                gst::EventView::Segment(s) => {
                    let Ok(seg) = s.segment().clone().downcast::<gst::ClockTime>() else {
                        return self.srcpad.push_event(event);
                    };
                    let tempo = self.segment_scale.lock().unwrap().as_ref().map_or_else(|| self.tempo(), |f| f());
                    let mut st = self.state.lock().unwrap();
                    Self::reset_timeline(&mut st);
                    st.seg_tempo = tempo;
                    st.cur_tempo = tempo;
                    drop(st);
                    let mut seg = seg;
                    let scale = |t: Option<gst::ClockTime>| {
                        t.map(|t| gst::ClockTime::from_nseconds((t.nseconds() as f64 / tempo) as u64))
                    };
                    seg.set_start(scale(seg.start()));
                    seg.set_time(scale(seg.time()));
                    seg.set_position(scale(seg.position()));
                    if seg.stop().is_some() {
                        seg.set_stop(scale(seg.stop()));
                    }
                    let out = gst::event::Segment::builder(&seg).seqnum(event.seqnum()).build();
                    self.srcpad.push_event(out)
                }
                gst::EventView::FlushStop(_) => {
                    Self::reset_timeline(&mut self.state.lock().unwrap());
                    self.srcpad.push_event(event)
                }
                _ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tempo {
        const NAME: &'static str = "MtWsolaTempo";
        type Type = super::Tempo;
        type ParentType = gst::Element;

        fn with_class(klass: &Self::Class) -> Self {
            let sinkpad = gst::Pad::builder_from_template(&klass.pad_template("sink").unwrap())
                .chain_function(|_, parent, buffer| {
                    Tempo::catch_panic_pad_function(parent, || Err(gst::FlowError::Error), |t| t.sink_chain(buffer))
                })
                .event_function(|pad, parent, event| {
                    Tempo::catch_panic_pad_function(parent, || false, |t| t.sink_event(pad, event))
                })
                .flags(gst::PadFlags::PROXY_CAPS | gst::PadFlags::PROXY_ALLOCATION)
                .build();
            let srcpad = gst::Pad::builder_from_template(&klass.pad_template("src").unwrap())
                .flags(gst::PadFlags::PROXY_CAPS | gst::PadFlags::PROXY_ALLOCATION)
                .build();
            Self {
                sinkpad,
                srcpad,
                state: Mutex::new(State { seg_tempo: 1.0, cur_tempo: 1.0, ..Default::default() }),
                tempo: Mutex::new(1.0),
                hook: Mutex::new(None),
                segment_scale: Mutex::new(None),
            }
        }
    }

    impl ObjectImpl for Tempo {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: std::sync::OnceLock<Vec<glib::ParamSpec>> = std::sync::OnceLock::new();
            PROPS.get_or_init(|| {
                vec![glib::ParamSpecFloat::builder("tempo").minimum(0.1).maximum(4.0).default_value(1.0).build()]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "tempo" {
                *self.tempo.lock().unwrap() = value.get().unwrap_or(1.0);
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "tempo" => self.tempo.lock().unwrap().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_pad(&self.sinkpad).unwrap();
            obj.add_pad(&self.srcpad).unwrap();
        }
    }

    impl GstObjectImpl for Tempo {}

    impl ElementImpl for Tempo {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: std::sync::OnceLock<gst::subclass::ElementMetadata> = std::sync::OnceLock::new();
            Some(META.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "WSOLA tempo",
                    "Filter/Effect/Audio",
                    "Pitch-preserving playback rate (Chromium's WSOLA)",
                    "moderntube",
                )
            }))
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: std::sync::OnceLock<Vec<gst::PadTemplate>> = std::sync::OnceLock::new();
            TEMPLATES.get_or_init(|| {
                let caps = gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("layout", "interleaved")
                    .field("rate", gst::IntRange::new(1, i32::MAX))
                    .field("channels", gst::IntRange::new(1, 64))
                    .build();
                vec![
                    gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &caps).unwrap(),
                    gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &caps).unwrap(),
                ]
            })
        }

        fn change_state(&self, transition: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            if transition == gst::StateChange::PausedToReady {
                Self::reset_timeline(&mut self.state.lock().unwrap());
            }
            self.parent_change_state(transition)
        }
    }
}

glib::wrapper! {
    pub struct Tempo(ObjectSubclass<imp::Tempo>) @extends gst::Element, gst::Object;
}

impl Tempo {
    /// Runs `hook` each time a new tempo takes effect, with the switch point.
    pub fn set_switch_hook(&self, hook: SwitchHook) {
        *self.imp().hook.lock().unwrap() = Some(hook);
    }

    /// The scale a new segment gets, shared with the video restamp so both tracks land on
    /// one timeline. A tempo that differs from it takes effect through the switch hook.
    pub fn set_segment_scale(&self, scale: SegmentScale) {
        *self.imp().segment_scale.lock().unwrap() = Some(scale);
    }
}

/// Registers `mtwsola` with the process's GStreamer registry.
pub fn register() -> Result<(), glib::BoolError> {
    gst::Element::register(None, "mtwsola", gst::Rank::NONE, Tempo::static_type())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gstreamer_app as gst_app;

    const S: u64 = 1_000_000_000;

    /// Output segment start and first pts for 0.1 s of silence from `start` through
    /// `mtwsola` at `tempo`, with the timeline's segment scale fixed at `scale`.
    fn run(tempo: f32, scale: f64, start: u64) -> (u64, u64) {
        gst::init().unwrap();
        let _ = register();
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("layout", "interleaved")
            .field("rate", 48_000)
            .field("channels", 2)
            .build();
        let src = gst_app::AppSrc::builder().caps(&caps).format(gst::Format::Time).build();
        src.set_property("handle-segment-change", true);
        let el = gst::ElementFactory::make("mtwsola").property("tempo", tempo).build().unwrap();
        el.downcast_ref::<Tempo>().unwrap().set_segment_scale(Box::new(move || scale));
        let sink = gst_app::AppSink::builder().sync(false).build();
        let pipe = gst::Pipeline::new();
        pipe.add_many([src.upcast_ref(), &el, sink.upcast_ref()]).unwrap();
        gst::Element::link_many([src.upcast_ref(), &el, sink.upcast_ref()]).unwrap();
        pipe.set_state(gst::State::Playing).unwrap();
        let mut seg = gst::FormattedSegment::<gst::ClockTime>::new();
        seg.set_start(gst::ClockTime::from_nseconds(start));
        seg.set_time(gst::ClockTime::from_nseconds(start));
        for i in 0..10u64 {
            let mut b = gst::Buffer::from_mut_slice(vec![0u8; 480 * 2 * 4]);
            let bm = b.get_mut().unwrap();
            bm.set_pts(gst::ClockTime::from_nseconds(start + i * S / 100));
            bm.set_duration(gst::ClockTime::from_nseconds(S / 100));
            src.push_sample(&gst::Sample::builder().buffer(&b).caps(&caps).segment(&seg).build()).unwrap();
        }
        src.end_of_stream().unwrap();
        let sample = sink.pull_sample().unwrap();
        let seg = sample.segment().unwrap().downcast_ref::<gst::ClockTime>().unwrap().clone();
        let pts = sample.buffer().unwrap().pts().unwrap().nseconds();
        pipe.set_state(gst::State::Null).unwrap();
        (seg.start().unwrap().nseconds(), pts)
    }

    #[test]
    fn a_segment_follows_the_timeline_scale() {
        // Speed went to 2x before the first segment; video's segment is scaled by 1.
        let (seg_start, pts) = run(2.0, 1.0, 10 * S);
        assert_eq!(seg_start, 10 * S);
        assert_eq!(pts, 10 * S);
    }

    #[test]
    fn a_segment_after_a_2x_seek_is_scaled_to_output_time() {
        let (seg_start, pts) = run(2.0, 2.0, 30 * S);
        assert_eq!(seg_start, 15 * S);
        assert_eq!(pts, 15 * S);
    }
}
