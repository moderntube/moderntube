//! The play pipeline's per-track appsrc chain and the per-track parse pipelines.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::diagnostics::frame_gap_logger;
use super::zero_copy;
use super::stretch::Stretch;

pub(super) const PLAY_READAHEAD_NS: u64 = 3_000_000_000;

pub(super) struct Parse {
    pub(super) pipeline: gst::Pipeline,
    pub(super) src: gst::Element,
    pub(super) sink: gst_app::AppSink,
}

/// Parsed video the parse pipeline may hand the pump: every ladder codec, AV1 TU-aligned.
pub(super) const AV1_TU_CAPS: &str ="video/x-av1, alignment=(string)tu; video/x-vp9; video/x-h264; video/x-h265";

pub(super) fn build_play_track(
    play: &gst::Pipeline,
    name: &str,
    sink: gst::Element,
    stretch: Arc<Stretch>,
) -> Result<(gst_app::AppSrc, Arc<AtomicBool>), Box<dyn std::error::Error>> {
    let src = gst::ElementFactory::make("appsrc")
        .name(name)
        .property_from_str("format", "time")
        // Never blocks, and is bounded by media time. A pump parked in a blocking
        // `push_sample` never returns after a flushing seek, because downstream was flushed
        // out from under it. The pump throttles itself against the `full` flag instead.
        // Time, not bytes: a byte bound is a different readahead on every rung.
        .property("block", false)
        .property("max-bytes", 0u64)
        .property("max-time", PLAY_READAHEAD_NS)
        .property("is-live", false)
        .build()?
        .dynamic_cast::<gst_app::AppSrc>()
        .map_err(|_| "appsrc is not an AppSrc")?;
    let full = Arc::new(AtomicBool::new(false));
    {
        let f_enough = full.clone();
        let f_need = full.clone();
        src.set_callbacks(
            gst_app::AppSrcCallbacks::builder()
                .enough_data(move |_| f_enough.store(true, Ordering::Relaxed))
                .need_data(move |_, _| f_need.store(false, Ordering::Relaxed))
                .build(),
        );
    }
    src.set_property("handle-segment-change", true);
    let dec = gst::ElementFactory::make("decodebin3").build()?;
    let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
    let sysmem = gst::ElementFactory::make("capsfilter")
        .property("caps", gst::Caps::builder("video/x-raw").build())
        .build()?;
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 2_000_000_000u64)
        .build()?;
    let video = name.starts_with("video");
    // Speed is a live WSOLA tempo (pitch-preserving); a change needs no flush.
    let aconv = gst::ElementFactory::make("audioconvert").build()?;
    let tempo = gst::ElementFactory::make("mtwsola").name("tempo").build()?;
    let aconv2 = gst::ElementFactory::make("audioconvert").build()?;
    let aresample = gst::ElementFactory::make("audioresample").build()?;
    // Zero-copy: the decoder's VA surfaces reach the sink as DMABuf, no download or upload.
    let zero_copy = video && zero_copy();
    if zero_copy {
        play.add_many([src.upcast_ref::<gst::Element>(), &dec, &queue, &sink])?;
        gst::Element::link_many([&queue, &sink])?;
    } else if video {
        play.add_many([src.upcast_ref::<gst::Element>(), &dec, &videoconvert, &sysmem, &queue, &sink])?;
        gst::Element::link_many([&videoconvert, &sysmem, &queue, &sink])?;
    }
    if video {
        if let Some(pad) = sink.static_pad("sink") {
            let stretch = stretch.clone();
            pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                match &mut info.data {
                    Some(gst::PadProbeData::Buffer(buf)) => {
                        if let Some(pts) = buf.pts() {
                            stretch.note_frame(pts.nseconds());
                            let out = stretch.to_output(pts.nseconds());
                            let b = buf.make_mut();
                            if let Some(d) = b.duration() {
                                let end = stretch.to_output(pts.nseconds() + d.nseconds());
                                b.set_duration(gst::ClockTime::from_nseconds(end.saturating_sub(out)));
                            }
                            b.set_pts(gst::ClockTime::from_nseconds(out));
                        }
                    }
                    Some(gst::PadProbeData::Event(ev)) if ev.type_() == gst::EventType::Segment => {
                        // Scaled as the tempo stage scales the audio segment, so both share one timeline.
                        let scale = stretch.segment_scale();
                        if let gst::EventView::Segment(s) = ev.view() {
                            if let Ok(seg) = s.segment().clone().downcast::<gst::ClockTime>() {
                                let mut seg = seg;
                                let div = |t: Option<gst::ClockTime>| {
                                    t.map(|t| gst::ClockTime::from_nseconds((t.nseconds() as f64 / scale) as u64))
                                };
                                seg.set_start(div(seg.start()));
                                seg.set_time(div(seg.time()));
                                seg.set_position(div(seg.position()));
                                if seg.stop().is_some() {
                                    seg.set_stop(div(seg.stop()));
                                }
                                *ev = gst::event::Segment::builder(&seg).seqnum(ev.seqnum()).build();
                            }
                        }
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            });
        }
    } else {
        if let Some(t) = tempo.downcast_ref::<super::super::wsola_element::Tempo>() {
            let (on_switch, on_segment) = (stretch.clone(), stretch.clone());
            t.set_switch_hook(Box::new(move |c0, o0, rate| on_switch.change_at(c0, o0, rate)));
            t.set_segment_scale(Box::new(move || on_segment.segment_scale()));
        }
        let volume = gst::ElementFactory::make("volume").name("volume").build()?;
        // The tempo stage sits after the queue: a tempo change is heard as soon as the sink drains,
        // instead of after the queue's two seconds.
        play.add_many([src.upcast_ref::<gst::Element>(), &dec, &aconv, &aresample, &volume, &queue, &tempo, &aconv2, &sink])?;
        gst::Element::link_many([&aconv, &aresample, &volume, &queue, &tempo, &aconv2, &sink])?;
    }
    // What leaves the appsrc, with the negotiated caps. Tells "the pump pushed 1080p and the
    // decoder dropped it" from "the sink was still draining 480p".
    if video {
        let tag = name.to_string();
        if let Some(pad) = src.static_pad("src") {
            let seen = Arc::new(AtomicU64::new(0));
            let last = Arc::new(Mutex::new(String::new()));
            pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
                let n = seen.fetch_add(1, Ordering::Relaxed);
                let caps = pad
                    .current_caps()
                    .and_then(|c| {
                        c.structure(0).map(|s| {
                            format!(
                                "{}x{}",
                                s.get::<i32>("width").unwrap_or(0),
                                s.get::<i32>("height").unwrap_or(0)
                            )
                        })
                    })
                    .unwrap_or_else(|| "-".into());
                let mut last = last.lock().unwrap();
                if *last != caps {
                    tracing::info!("[gst] {tag}: APPSRC OUT #{n} caps {} -> {caps}", *last);
                    *last = caps;
                }
                gst::PadProbeReturn::Ok
            });
        }
    }
    // Link without the caps check: `Element::link` returns Noformat here even though avdec_vp9
    // can serve this caps shape. Negotiation completes when the pipeline reaches PLAYING.
    let src_pad = src.static_pad("src").ok_or("appsrc has no src pad")?;
    let dec_pad = dec.static_pad("sink").ok_or("decodebin has no sink pad")?;
    src_pad.link_full(&dec_pad, gst::PadLinkCheck::empty()).map_err(|_| "appsrc -> decodebin link failed")?;
    // decodebin's src pads are dynamic and feed the first element of the downstream chain;
    // the rest is linked statically above. Without zero-copy, videoconvert turns the decoder's
    // output into system-memory frames the paintable sink accepts.
    let queue_sink = if zero_copy {
        queue.static_pad("sink").ok_or("queue has no sink pad")?
    } else if video {
        videoconvert.static_pad("sink").ok_or("videoconvert has no sink pad")?
    } else {
        aconv.static_pad("sink").ok_or("audioconvert has no sink pad")?
    };
    // What the decoder emits (buffer count and caps changes). With the APPSRC OUT probe and
    // the sink frame counter it locates a stall: appsrc flowing and decode-out stalled is a
    // decoder wedge; decode-out flowing and the sink stalled is downstream.
    if video {
        if let Some(pad) = src.static_pad("src") {
            let gap = frame_gap_logger(format!("{name} appsrc-out"));
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                gap(info);
                gst::PadProbeReturn::Ok
            });
        }
        let vsink = queue_sink.clone();
        let dtag = name.to_string();
        let seen = Arc::new(AtomicU64::new(0));
        let last = Arc::new(Mutex::new(String::new()));
        let gap = frame_gap_logger(format!("{dtag} decode-out"));
        vsink.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            gap(info);
            let n = seen.fetch_add(1, Ordering::Relaxed);
            // Periodic count as well as caps changes: a stall here is otherwise invisible.
            if n.is_multiple_of(300) {
                tracing::info!("[gst] {dtag}: DECODE OUT count {n}");
            }
            let caps = pad
                .current_caps()
                .and_then(|c| {
                    c.structure(0).map(|s| {
                        format!(
                            "{}x{}",
                            s.get::<i32>("width").unwrap_or(0),
                            s.get::<i32>("height").unwrap_or(0)
                        )
                    })
                })
                .unwrap_or_else(|| "-".into());
            let mut last = last.lock().unwrap();
            if *last != caps {
                tracing::info!("[gst] {dtag}: DECODE OUT #{n} caps {} -> {caps}", *last);
                *last = caps;
            }
            gst::PadProbeReturn::Ok
        });
    }
    let tag = name.to_string();
    // Every decoder decodebin3 plugs: VA vs software, and any re-plug on renegotiation.
    if let Some(bin) = dec.downcast_ref::<gst::Bin>() {
        let tag = tag.clone();
        bin.connect_deep_element_added(move |_, _, element| {
            if let Some(f) = element.factory() {
                if f.klass().contains("Decoder") {
                    tracing::info!("[gst] {tag}: decoder {}", f.name());
                }
            }
        });
    }
    dec.connect_pad_added(move |_, pad| {
        tracing::info!("[gst] {tag}: decoder pad {}", pad.name());
        if queue_sink.is_linked() {
            return;
        }
        match pad.link_full(&queue_sink, gst::PadLinkCheck::empty()) {
            Ok(_) => tracing::info!("[gst] {tag}: decoder linked"),
            Err(e) => tracing::warn!("[gst] {tag}: decoder link failed: {e:?}"),
        }
    });
    Ok((src, full))
}

pub(super) fn build_parse(tag: &'static str, live: bool) -> Result<Parse, gst::StateChangeError> {
    let make = |f: &str| {
        gst::ElementFactory::make(f).build().map_err(|e| {
            tracing::warn!("[gst] {tag}: {f} missing: {e}");
            gst::StateChangeError
        })
    };
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("appsrc")
        .name(tag)
        .property_from_str("format", "bytes")
        .property_from_str("stream-type", if live { "stream" } else { "seekable" })
        .property("block", live)
        .property("max-bytes", 8u64 * 1024 * 1024)
        .build()
        .map_err(|_| gst::StateChangeError)?;
    let parsebin = make("parsebin")?;
    let sink = make("appsink")?.dynamic_cast::<gst_app::AppSink>().map_err(|_| gst::StateChangeError)?;
    sink.set_property("sync", false);
    // AV1 leaves here as whole temporal units: decodebin3 parses again, and re-parsing
    // frame-aligned AV1 (hidden frames carry no pts) mis-times the shown frames.
    if tag == "video" {
        sink.set_caps(Some(&AV1_TU_CAPS.parse::<gst::Caps>().expect("static caps")));
    }
    // The appsink queue is unbounded (`max-buffers=0` means unlimited). Clusters of 8 MB or
    // more from a saturated VOD fetch fill a bounded queue and backpressure matroskademux
    // into a failing pull, which kills the parse pipeline.
    sink.set_property("max-buffers", 0u32);
    sink.set_property("drop", false);
    pipeline.add_many([&src, &parsebin, sink.upcast_ref::<gst::Element>()]).map_err(|_| gst::StateChangeError)?;
    src.link(&parsebin).map_err(|_| gst::StateChangeError)?;
    let sink_pad = sink.static_pad("sink").ok_or(gst::StateChangeError)?;
    parsebin.connect_pad_added(move |_, pad| {
        // Compare caps, not link state: parsebin renegotiates in place across input chunks
        // (WebM chunks cut mid-element emit one src pad per format version). The first pad
        // stays linked but stops producing samples once a newer pad appears, so the sink
        // relinks onto the newest pad when its caps arrive. The bytes are contiguous, so no
        // resync is needed.
        let new_caps = pad.current_caps();
        let take = match (&new_caps, sink_pad.current_caps()) {
            (Some(nw), Some(cur)) => nw != &cur,
            (Some(_), None) => true,
            _ => false,
        };
        if sink_pad.is_linked() && !take {
            tracing::info!("[gst] {tag}: extra parsed pad ignored ({})", pad.name());
            return;
        }
        // Unlink the stale peer first: a pad cannot link to an already-linked peer.
        if sink_pad.is_linked() {
            if let Some(peer) = sink_pad.peer() {
                let _ = sink_pad.unlink(&peer);
            }
            tracing::info!("[gst] {tag}: re-linking parse pad {} -> appsink (caps moved)", pad.name());
        }
        match pad.link(&sink_pad) {
            Ok(_) => tracing::info!("[gst] {tag}: parsed pad linked ({})", pad.name()),
            Err(e) => tracing::warn!("[gst] {tag}: parsed pad link failed: {e:?}"),
        }
    });
    if let Some(bus) = pipeline.bus() {
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => tracing::warn!(
                    "[gst] {tag}: PARSE ERROR from {:?}: {} ({:?})",
                    e.src().map(|s| s.path_string()),
                    e.error(),
                    e.debug()
                ),
                gst::MessageView::Warning(w) => tracing::warn!("[gst] {tag}: parse warning from {:?}: {}", w.src().map(|s| s.path_string()), w.error()),
                gst::MessageView::Eos(_) => tracing::info!("[gst] {tag}: PARSE EOS"),
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
    }
    Ok(Parse { pipeline, src, sink })
}

/// Stop the parse pipeline behind `src` once its pump has ended.
pub(super) fn teardown_parse(parses: &Arc<Mutex<Vec<Parse>>>, src: &gst::Element, tag: &'static str, why: &str) {
    let mut n_parses = 0;
    {
        let mut ps = parses.lock().unwrap();
        ps.retain(|p| {
            let same = &p.src == src;
            if same {
                let _ = p.pipeline.set_state(gst::State::Null);
                n_parses += 1;
            }
            !same
        });
    }
    tracing::info!("[gst] {tag}: tore down parse ({why}): {n_parses} pipeline(s)");
}
