//! The GStreamer substrate: segment sessions played through a pipeline into a paintable.
pub mod player;
pub mod preview;
pub mod substrate;
pub mod wsola;
pub mod wsola_element;

use std::sync::Arc;

use innertube::{
    MediaReader, Navigation, Playback, PlaybackOptions, Quality, SabrStream, SabrStreamOptions, SegmentVod,
    SegmentVodOptions,
};

pub use player::GstPlayer;
pub use substrate::decodes_av1;

use crate::playback::PlaybackError;

/// Where a GStreamer paintable goes once one exists; the widget registers one.
pub type GstPaintableSink = std::rc::Rc<dyn Fn(&gtk4::gdk::Paintable)>;

#[allow(clippy::large_enum_variant)]
pub enum Prepared {
    Vod(Arc<SegmentVod>),
    /// The stream and its two readers: it runs while a reader is alive.
    Live(SabrStream, MediaReader, MediaReader),
}

impl Prepared {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live(..))
    }

    pub fn storyboard(&self) -> Option<innertube::Storyboard> {
        match self {
            Self::Vod(vod) => vod.storyboard().cloned(),
            Self::Live(..) => None,
        }
    }
}

/// Open `video_id` as whichever kind it is. `start` is a VOD resume position in seconds.
pub async fn prepare(
    video_id: &str,
    quality: Quality,
    start: Option<f64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    let (session, solver, pot) = crate::playback::open_deps()?;
    let opts = PlaybackOptions {
        quality,
        vod_start_ms: start.map_or(0, |s| (s * 1000.0) as i64),
        navigation,
        av1: decodes_av1(),
        ..Default::default()
    };
    Ok(match Playback::open(&session, &solver, pot, video_id, &opts).await? {
        Playback::Vod(vod) => Prepared::Vod(Arc::new(vod)),
        Playback::Live(stream) => {
            let (v, a) = (stream.video(), stream.audio());
            Prepared::Live(stream, v, a)
        }
    })
}

pub async fn prepare_vod(
    video_id: &str,
    quality: Quality,
    start: Option<f64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    let (session, solver, pot) = crate::playback::open_deps()?;
    let opts = SegmentVodOptions {
        quality,
        start_ms: start.map_or(0, |s| (s * 1000.0) as i64),
        navigation,
        av1: decodes_av1(),
        ..Default::default()
    };
    let vod = SegmentVod::open(&session, &solver, pot, video_id, &opts).await?;
    Ok(Prepared::Vod(Arc::new(vod)))
}

/// Open a Short on the shared session. `preload` fetches one round and holds until
/// [`SegmentVod::activate`], as the web client's next-Short preload does.
pub async fn prepare_short(entry: innertube::ReelEntry, preload: bool) -> Result<Arc<SegmentVod>, PlaybackError> {
    let (session, solver, pot) = crate::playback::shared_deps()?;
    let opts = SegmentVodOptions {
        navigation: Navigation {
            player_params: entry.player_params.clone(),
            short: true,
            prefetched_player: entry.prefetched.clone(),
            ..Default::default()
        },
        av1: decodes_av1(),
        preload,
        viewport: Some(SHORTS_VIEWPORT),
        ..Default::default()
    };
    Ok(Arc::new(SegmentVod::open(&session, &solver, pot, &entry.video_id, &opts).await?))
}

/// The Shorts player's size in CSS pixels (REQ-015).
pub const SHORTS_VIEWPORT: (i32, i32) = (523, 932);

/// One SABR stream delivering the server's raw segments, as the web client's main player
/// runs it.
pub async fn prepare_live(
    video_id: &str,
    quality: Quality,
    start_ms: Option<i64>,
    navigation: Navigation,
) -> Result<Prepared, PlaybackError> {
    let (session, solver, pot) = crate::playback::open_deps()?;
    let opts = SabrStreamOptions {
        quality,
        start_ms,
        navigation,
        av1: decodes_av1(),
        ..Default::default()
    };
    let stream = SabrStream::open(&session, &solver, pot, video_id, &opts).await?;
    let (v, a) = (stream.video(), stream.audio());
    Ok(Prepared::Live(stream, v, a))
}
