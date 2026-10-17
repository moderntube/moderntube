//! Shared hover preview: one small, muted, low-res video surface that floats over the hovered
//! card's thumbnail. One instance serves every `ResultsView` (the grid and the watch sidebar),
//! so at most one preview plays at a time and a new one tears down the old.
//!
//! The surface is an overlay child of the window's `stage`, added once and positioned by the
//! stage's `get-child-position` handler ([`HoverPreview::child_position`]). Teardown stops the
//! feed and hides the surface.
//!
//! The surface is a sibling of the card, so GTK sends the card a `leave` as soon as the surface
//! appears under the pointer. Teardown therefore waits [`HIDE_GRACE`], and a crossing into the
//! surface or back into the card ([`HoverPreview::keep_alive`]) cancels it. Leaving the surface
//! schedules it again. This also keeps the mute button reachable.
//!
//! The `playback.hover_previews` setting turns previews off (default: on).
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, EventControllerMotion, GestureClick, Orientation, Overlay};

use crate::models::ResultItem;
use crate::playback::{PreviewPlayer, PreviewPrepared};
use crate::runtime::AsyncRuntime;

/// Grace before a leave actually tears the preview down. Long enough to cover GTK's
/// synthesized leave→enter pair when the surface mounts under the pointer (delivered
/// within one frame cycle), short enough that leaving the card feels immediate.
const HIDE_GRACE: Duration = Duration::from_millis(150);

/// Speaker icons for the muted / unmuted preview toggle.
const ICON_MUTED: &str = "audio-volume-muted-symbolic";
const ICON_UNMUTED: &str = "audio-volume-high-symbolic";

/// The activation callback + item for the currently-shown preview, so a click on the
/// preview opens that video (YouTube behavior). Each `ResultsView` supplies its own
/// wrapper (they route navigation slightly differently), stored per-`show`.
type ClickTarget = (ResultItem, Rc<dyn Fn(ResultItem)>);

/// The window's shared watch-position recorder (`App::record_position` in `window::app`).
type PositionRecorder = Rc<dyn Fn(&str, f64)>;

pub struct HoverPreview {
    engine: Rc<PreviewPlayer>,
    /// The shared preview surface (video plus the mute toggle at top-right). Added once to the
    /// stage overlay and never reparented; the stage positions it over the hovered card.
    root: Overlay,
    rt: AsyncRuntime,
    /// Video id of the currently-showing (or in-flight) preview. Guards a stale async
    /// prepare result and a duplicate/overlapping hover.
    current: Rc<RefCell<Option<String>>>,
    /// What a click on the preview surface should open (set per `show`).
    on_click: Rc<RefCell<Option<ClickTarget>>>,
    /// Pending grace-window teardown (see the module docs). One-shot: the fire closure
    /// clears this handle itself, so cancel paths only ever `.remove()` a live source.
    hide_timer: Rc<RefCell<Option<glib::SourceId>>>,
    /// The in-flight prepare for the current hover. Teardown aborts it, which cancels its
    /// network I/O; dropping only the result would let the fetch run to completion.
    prepare: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>>,
    /// Reports the watch position when a previewed video is clicked open, so the VOD resume
    /// path picks it up. Set once by `window::build`.
    on_position: Rc<RefCell<Option<PositionRecorder>>>,
    /// The thumbnail the surface floats over. Weak, so a destroyed card (feed reload,
    /// navigation) is not kept alive. Read by the stage overlay's `get-child-position`
    /// handler on every allocation, so the surface follows its card through a resize.
    tracked: Rc<RefCell<Option<glib::WeakRef<gtk::Widget>>>>,
    /// The animated-WebP layer, used instead of a stream where YouTube uses one.
    animated: Rc<RefCell<crate::widgets::an_webp::AnimatedPreview>>,
    /// The mute/unmute toggle over the preview, hidden on paths that have no audio.
    mute_btn: Button,
    /// Black fill over the shared surface, up from the moment a hover fires until that hover's
    /// first frame. Without it the previous card's frame shows until the new stream opens.
    cover: GtkBox,
    /// Last rectangle handed to the overlay, so `child_position` logs only on change.
    /// GTK asks for it on every allocation.
    #[allow(clippy::type_complexity)]
    last_rect: Cell<Option<((i32, i32, i32, i32), Option<(i32, i32)>)>>,
    /// Reads the window's current results/channel/playlist page URL, for a preview's
    /// `player()` `referer`. Set once by `window::app` via `set_referer_source`.
    referer_source: RefCell<Option<Rc<dyn Fn() -> String>>>,
}

/// Minimum preview watch time (seconds) handed off as a resume point. A shorter graze
/// does not mark the video as partly watched.
const RESUME_HANDOFF_MIN_SECS: f64 = 5.0;

/// Immediate teardown: stop the preview, hide the surface, clear the active id
/// and any pending grace timer. Free function so the timer closure can run it without
/// `&self` (it fires after `new()` returned).
// The handles are the state a teardown clears. A struct holding them would replace the
// argument list.
#[allow(clippy::too_many_arguments)]
fn teardown_now(
    engine: &PreviewPlayer,
    root: &Overlay,
    current: &RefCell<Option<String>>,
    on_click: &RefCell<Option<ClickTarget>>,
    hide_timer: &RefCell<Option<glib::SourceId>>,
    prepare: &RefCell<Option<tokio::task::JoinHandle<()>>>,
    tracked: &RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    animated: &RefCell<crate::widgets::an_webp::AnimatedPreview>,
    why: &str,
) {
    // `why` names the path that cleared `current`: a grace hide, an explicit stop, or a new
    // hover superseding the old one.
    if let Some(id) = current.borrow().as_deref() {
        tracing::info!("[preview] teardown {id} (why={why})");
    }
    if let Some(id) = hide_timer.borrow_mut().take() {
        id.remove();
    }
    // Aborting the prepare cancels its network I/O.
    if let Some(task) = prepare.borrow_mut().take() {
        task.abort();
    }
    engine.stop();
    animated.borrow().stop();
    root.set_visible(false);
    current.borrow_mut().take();
    on_click.borrow_mut().take();
    // Stop following the card: a hidden surface must not keep answering
    // `get-child-position` with a rectangle, and the weak ref must not outlive the hover.
    tracked.borrow_mut().take();
}

/// Fetch and decode one animated WebP.
///
/// A plain GET: the URL is signed, and `i.ytimg.com` serves it without the watch page's
/// headers.
async fn fetch_an_webp(url: &str) -> Result<Vec<crate::widgets::an_webp::Frame>, String> {
    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        // Not every video has an animated preview; a 404 lands here and the card keeps its
        // thumbnail.
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    crate::widgets::an_webp::decode(&bytes).map_err(|e| e.to_string())
}

/// (Re)arms the grace-window teardown. The one-shot source clears its stored handle before
/// running, since removing a fired source panics with "Source ID not found".
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn schedule_hide(
    engine: &Rc<PreviewPlayer>,
    root: &Overlay,
    current: &Rc<RefCell<Option<String>>>,
    on_click: &Rc<RefCell<Option<ClickTarget>>>,
    hide_timer: &Rc<RefCell<Option<glib::SourceId>>>,
    prepare: &Rc<RefCell<Option<tokio::task::JoinHandle<()>>>>,
    tracked: &Rc<RefCell<Option<glib::WeakRef<gtk::Widget>>>>,
    animated: &Rc<RefCell<crate::widgets::an_webp::AnimatedPreview>>,
) {
    if let Some(id) = hide_timer.borrow_mut().take() {
        id.remove();
    }
    let (engine, root) = (engine.clone(), root.clone());
    let (current, on_click) = (current.clone(), on_click.clone());
    let timer = hide_timer.clone();
    let prepare = prepare.clone();
    let tracked = tracked.clone();
    let animated = animated.clone();
    let source = glib::timeout_add_local_once(HIDE_GRACE, move || {
        timer.borrow_mut().take();
        teardown_now(&engine, &root, &current, &on_click, &timer, &prepare, &tracked, &animated,
                     "grace-hide");
    });
    *hide_timer.borrow_mut() = Some(source);
}

impl HoverPreview {
    /// `muted` is the saved preview-mute preference.
    pub fn new(rt: AsyncRuntime, muted: bool) -> Rc<Self> {
        let engine = Rc::new(PreviewPlayer::new());
        let area = engine.widget();

        // Mute/un-mute toggle over the video (top-right), persisted. Previews default to
        // muted; the icon reflects state.
        let muted = Rc::new(Cell::new(muted));
        engine.set_muted(muted.get());
        let mute_btn = Button::builder()
            .icon_name(if muted.get() { ICON_MUTED } else { ICON_UNMUTED })
            .halign(Align::End)
            .valign(Align::Start)
            .margin_top(6)
            .margin_end(6)
            .css_classes(["osd", "circular"])
            .tooltip_text("Mute / unmute preview")
            .build();
        {
            let engine = engine.clone();
            let muted = muted.clone();
            mute_btn.connect_clicked(move |b| {
                let now = !muted.get();
                muted.set(now);
                engine.set_muted(now);
                crate::prefs::save_preview_muted(now);
                b.set_icon_name(if now { ICON_MUTED } else { ICON_UNMUTED });
            });
        }

        let current: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let on_click: Rc<RefCell<Option<ClickTarget>>> = Rc::new(RefCell::new(None));
        let hide_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let on_position: Rc<RefCell<Option<PositionRecorder>>> = Rc::new(RefCell::new(None));
        let prepare: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> = Rc::new(RefCell::new(None));
        let tracked: Rc<RefCell<Option<glib::WeakRef<gtk::Widget>>>> = Rc::new(RefCell::new(None));

        // The animated-WebP layer sits over the video area. Only one of the two is visible: a
        // card with a moving thumbnail animates the image and opens no session.
        let animated = Rc::new(RefCell::new(crate::widgets::an_webp::AnimatedPreview::new()));
        let animated_picture = animated.borrow().picture.clone();

        // Covers the video surface from the moment a hover fires until that hover's first frame.
        // The shared surface keeps its last frame when hidden, so without the cover the
        // previous card's frame shows over the new card until the new stream opens. Black
        // matches what the surface shows on the first hover of a session.
        let cover = GtkBox::new(Orientation::Vertical, 0);
        cover.add_css_class("preview-cover");
        cover.set_visible(false);
        // Clicks pass through to the video area's open-the-video gesture.
        cover.set_can_target(false);
        animated_picture.set_can_target(false);

        let root = Overlay::new();
        root.set_child(Some(&area));
        root.add_overlay(&animated_picture);
        root.add_overlay(&cover);
        root.add_overlay(&mute_btn);
        // Fill, not Start: the stage hands this surface an explicit rectangle from
        // `child_position`, and an aligned child is allocated at its natural size inside it,
        // which put the surface a card-width off the hovered card.
        root.set_halign(Align::Fill);
        root.set_valign(Align::Fill);
        root.set_visible(false);

        // Crossing into the surface cancels a pending grace teardown: the card fires `leave`
        // as soon as the surface mounts under the pointer, since the surface is a
        // stage-overlay sibling of the card. Crossing out schedules the teardown again, so
        // leaving the card through the preview still hides it.
        {
            let motion = EventControllerMotion::new();
            let t = hide_timer.clone();
            motion.connect_enter(move |_, _, _| {
                if let Some(id) = t.borrow_mut().take() {
                    id.remove();
                }
            });
            let (eng, root_l) = (engine.clone(), root.clone());
            let (cur, clk, t) = (current.clone(), on_click.clone(), hide_timer.clone());
            let prep = prepare.clone();
            let trk = tracked.clone();
            let anim = animated.clone();
            motion.connect_leave(move |_| {
                if cur.borrow().is_some() {
                    schedule_hide(&eng, &root_l, &cur, &clk, &t, &prep, &trk, &anim);
                }
            });
            root.add_controller(motion);
        }

        // The surface is an overlay child of the stage, and the ScrolledWindow holding the grid
        // is in a different branch, so a wheel over a playing preview would not reach the feed.
        // Forward the delta to the tracked card's scroller, found by walking up from the card,
        // so every `ResultsView` works. No teardown here: `ResultsView` already hides the preview
        // from the adjustment's `value_changed` (`hide_on_scroll`).
        {
            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            let tracked_s = tracked.clone();
            scroll.connect_scroll(move |_, _dx, dy| {
                let Some(card) = tracked_s.borrow().as_ref().and_then(|w| w.upgrade()) else {
                    return glib::Propagation::Proceed;
                };
                let mut node = card.parent();
                while let Some(w) = node {
                    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
                        let adj = sw.vadjustment();
                        // `step_increment` is GTK's wheel-notch unit, so a forwarded scroll
                        // matches scrolls elsewhere in the feed.
                        let max = (adj.upper() - adj.page_size()).max(adj.lower());
                        adj.set_value((adj.value() + dy * adj.step_increment()).clamp(adj.lower(), max));
                        return glib::Propagation::Stop;
                    }
                    node = w.parent();
                }
                glib::Propagation::Proceed
            });
            root.add_controller(scroll);
        }

        // Clicking the video area opens the previewed video. The gesture is on `area`, so the
        // mute button, a sibling overlay, consumes its own clicks. The surface is outside the
        // GridView's widget tree, so the click never reaches the card's activate and is
        // forwarded here.
        {
            let click = GestureClick::new();
            let on_click = on_click.clone();
            let current = current.clone();
            let engine_c = engine.clone();
            let root_c = root.clone();
            let on_position_c = on_position.clone();
            click.connect_released(move |g, _, _, _| {
                let target = on_click.borrow_mut().take();
                tracing::info!("[preview] click released, target={}", target.as_ref().map_or("none".into(), |(i, _)| i.video_id()));
                if let Some((item, act)) = target {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    // Hand the position off as the resume point so the click continues where
                    // the preview was. VOD only: live positions are raw stream PTS.
                    if !item.live() {
                        if let (Some(rec), Some(pos)) =
                            (on_position_c.borrow().clone(), engine_c.time_pos())
                        {
                            if pos >= RESUME_HANDOFF_MIN_SECS {
                                rec(&item.video_id(), pos);
                            }
                        }
                    }
                    // Stop + hide the preview surface before navigating.
                    engine_c.stop();
                    root_c.set_visible(false);
                    current.borrow_mut().take();
                    act(item);
                }
            });
            area.add_controller(click);
        }

        let this = Rc::new(Self {
            engine,
            root,
            rt,
            current,
            on_click,
            hide_timer,
            prepare,
            on_position,
            tracked,
            animated,
            mute_btn,
            cover,
            last_rect: Cell::new(None),
            referer_source: RefCell::new(None),
        });

        // Drop the cover when this hover's media opens: that is the first moment the surface
        // holds a frame of the right video.
        {
            let cover = this.cover.clone();
            let current = this.current.clone();
            this.engine.connect_playing(move |id| {
                // Guarded on the id so a late callback from a superseded hover does not
                // uncover the surface.
                if current.borrow().as_deref() == Some(id) {
                    cover.set_visible(false);
                }
            });
        }
        this
    }

    /// The preview surface widget. `window::app` adds this exactly once as an overlay child
    /// of the stable `stage` overlay; it must never be added anywhere else.
    pub fn surface(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The hover-previews setting, read as each card is built.
    pub fn enabled(&self) -> bool {
        crate::settings::read(|s| s.playback.hover_previews)
    }

    /// Wire the window's shared position recorder (`record_position`) so a clicked
    /// preview hands its watch position off as the video's resume point.
    pub fn set_position_recorder(&self, rec: PositionRecorder) {
        *self.on_position.borrow_mut() = Some(rec);
    }

    /// Wire the window's current-page-URL reader, for a preview's `player()` `referer`.
    pub fn set_referer_source(&self, f: Rc<dyn Fn() -> String>) {
        *self.referer_source.borrow_mut() = Some(f);
    }

    /// If `video_id`'s preview is playing (VOD, watched past the handoff threshold), record
    /// its position as the video's resume point. Called by the grid's activate path, so a
    /// click on the card around the preview resumes like a click on the preview surface.
    pub fn handoff_position(&self, video_id: &str, is_live: bool) {
        if is_live || self.current.borrow().as_deref() != Some(video_id) {
            return;
        }
        if let (Some(rec), Some(pos)) = (self.on_position.borrow().clone(), self.engine.time_pos()) {
            if pos >= RESUME_HANDOFF_MIN_SECS {
                rec(video_id, pos);
            }
        }
    }

    /// Show a preview of `item`, floated over `thumbnail`'s rectangle (in the stable stage
    /// overlay's coordinate space). `on_activate` opens the video if the preview is
    /// clicked. No-ops if `item` is already the one showing (a re-hover landing before the
    /// previous debounce's teardown).
    pub fn show(&self, thumbnail: &gtk::Widget, item: ResultItem, on_activate: Rc<dyn Fn(ResultItem)>) {
        let id = item.video_id();
        if self.current.borrow().as_deref() == Some(id.as_str()) {
            return;
        }
        tracing::info!(
            "[preview] hover {id} (live={}) — fetching t={}ms",
            item.live(),
            crate::uptime_ms()
        );
        // Tear down whatever was showing, then reposition + reveal the shared surface.
        self.teardown("superseded-by-new-hover");
        self.current.borrow_mut().replace(id.clone());
        *self.on_click.borrow_mut() = Some((item.clone(), on_activate));
        *self.tracked.borrow_mut() = Some(thumbnail.downgrade());
        // Nudge the overlay to re-run `get-child-position` now that there is a card to
        // follow; from here GTK re-runs it on every allocation.
        if let Some(container) = self.root.parent() {
            container.queue_allocate();
        }
        // Up before the surface is revealed, since the framebuffer still holds the previous
        // card's last frame. `connect_playing` drops it for a stream; the animated path drops
        // it once frames play.
        self.cover.set_visible(true);
        self.root.set_visible(true);

        // A card whose response carried an `an_webp` URL previews with one image GET and no
        // player session (IT-027). The sidebar-streaming pref opts back into a real stream.
        if let Some(url) = item.moving_thumbnail() {
            if !crate::prefs::load_sidebar_streaming_preview() {
                tracing::info!("[preview] {id}: animated thumbnail (no session opened)");
                // An animated thumbnail opens no session and has no audio, so the mute toggle
                // is hidden. The log reads the widget back.
                self.mute_btn.set_visible(false);
                tracing::info!(
                    "[preview] {id}: audio control visible={}",
                    self.mute_btn.is_visible()
                );
                let (animated, current, root, cover) = (
                    self.animated.clone(),
                    self.current.clone(),
                    self.root.clone(),
                    self.cover.clone(),
                );
                let vid = id.clone();
                let handle = self.rt.spawn_abortable(
                    async move { (vid, fetch_an_webp(&url).await) },
                    move |(vid, frames)| {
                        // A hover that has already been superseded must not paint.
                        if current.borrow().as_deref() != Some(vid.as_str()) {
                            return;
                        }
                        match frames {
                            Ok(frames) => {
                                // This path has no pipeline to report a first frame, so
                                // it uncovers the surface itself.
                                animated.borrow_mut().play(frames);
                                cover.set_visible(false);
                            }
                            Err(e) => {
                                // Fall back to the card's thumbnail; opening a stream here
                                // would cost the session this path avoids.
                                tracing::warn!("[preview] {vid}: animated thumbnail failed — {e}");
                                root.set_visible(false);
                                current.borrow_mut().take();
                            }
                        }
                    },
                );
                *self.prepare.borrow_mut() = Some(handle);
                return;
            }
        }

        // The streaming path carries an audio track, so the control is shown again; the shared
        // surface may have been hidden by the animated path.
        self.mute_btn.set_visible(true);
        tracing::info!("[preview] {id}: audio control visible={}", self.mute_btn.is_visible());

        let engine = self.engine.clone();
        let current = self.current.clone();
        let root = self.root.clone();
        let on_click = self.on_click.clone();
        let vid = id.clone();
        let nav = innertube::Navigation {
            player_params: item.preview_player_params(),
            referer: self.referer_source.borrow().as_ref().map(|f| f()),
            preview: true,
            ..Default::default()
        };
        let handle = self.rt.spawn_abortable(
            async move {
                let prepared = PreviewPlayer::prepare(&vid, &nav).await;
                (vid, prepared)
            },
            move |(vid, prepared): (String, Result<PreviewPrepared, _>)| {
                // Only start if this hover is still active; a fast pass-over may have left or
                // moved to another card during the fetch.
                if current.borrow().as_deref() != Some(vid.as_str()) {
                    tracing::info!(
                        "[preview] {vid}: prepare landed but the hover moved on (now {}) — dropped",
                        current.borrow().as_deref().unwrap_or("<nothing>"),
                    );
                    return;
                }
                match prepared {
                    Ok(p) => engine.start(&vid, p),
                    Err(e) => {
                        // Stop and hide the surface so the card falls back to its thumbnail.
                        // Clearing `current` alone would leave the previous video visible and
                        // replaying on every re-hover.
                        tracing::warn!("[preview] {vid}: prepare failed, tearing down — {e}");
                        engine.stop();
                        root.set_visible(false);
                        current.borrow_mut().take();
                        on_click.borrow_mut().take();
                    }
                }
            },
        );
        // Held so teardown can abort it mid-fetch (the previous hover's task was
        // already aborted by the `teardown()` above).
        *self.prepare.borrow_mut() = Some(handle);
    }

    /// Where the surface belongs now: the tracked thumbnail's rectangle in `stage`'s
    /// coordinate space, or `None` when nothing is previewed.
    ///
    /// Called from the stage overlay's `get-child-position` handler on every allocation, so
    /// the surface follows its card through a window resize with no cached rectangle to go
    /// stale.
    pub fn child_position(&self, stage: &gtk::Widget) -> Option<gtk::gdk::Rectangle> {
        let thumbnail = self.tracked.borrow().as_ref()?.upgrade()?;
        let b = thumbnail.compute_bounds(stage)?;
        // Log the rectangle when it changes, so the surface-versus-card offset can be measured.
        // Allocation runs often, so only changes are logged.
        let rect = (b.x() as i32, b.y() as i32, b.width() as i32, b.height() as i32);
        // Keyed on the rectangle and the offset from it, so a settled offset is logged and not
        // only the transient.
        let actual = self.root.compute_bounds(stage);
        let delta = actual.map(|a| (a.x() as i32 - rect.0, a.y() as i32 - rect.1));
        let key = (rect, delta);
        if self.last_rect.get() != Some(key) {
            self.last_rect.set(Some(key));
            // Also report where the surface actually is in the same coordinate space; dx and
            // dy are its offset from the rect it was handed. On the allocation that moves the
            // surface the two differ by one frame, so read the last line of a settled hover.
            match actual {
                Some(a) => tracing::info!(
                    "[preview] surface rect x={} y={} w={} h={} actual_x={} actual_y={} \
                     actual_w={} actual_h={} dx={} dy={}",
                    rect.0,
                    rect.1,
                    rect.2,
                    rect.3,
                    a.x() as i32,
                    a.y() as i32,
                    a.width() as i32,
                    a.height() as i32,
                    a.x() as i32 - rect.0,
                    a.y() as i32 - rect.1
                ),
                None => tracing::info!(
                    "[preview] surface rect x={} y={} w={} h={} actual=unmapped",
                    rect.0, rect.1, rect.2, rect.3
                ),
            }
        }
        Some(gtk::gdk::Rectangle::new(rect.0, rect.1, rect.2, rect.3))
    }

    /// Schedule a teardown of the preview if `video_id` is the one showing, after the grace
    /// window (see the module docs). A crossing into the surface or back into the card
    /// cancels it. A leave for a card whose debounce never elapsed, or one already
    /// superseded, does nothing.
    pub fn hide(&self, video_id: &str) {
        if self.current.borrow().as_deref() == Some(video_id) {
            schedule_hide(
                &self.engine,
                &self.root,
                &self.current,
                &self.on_click,
                &self.hide_timer,
                &self.prepare,
                &self.tracked,
                &self.animated,
            );
        }
    }

    /// Cancel a pending grace teardown if `video_id` is the preview showing. The pointer
    /// re-entered the card the preview belongs to, for example from the surface onto the
    /// title bar.
    pub fn keep_alive(&self, video_id: &str) {
        if self.current.borrow().as_deref() == Some(video_id) {
            if let Some(id) = self.hide_timer.borrow_mut().take() {
                id.remove();
            }
        }
    }

    /// Hide any active preview (used by the scroll handler so it doesn't float over the
    /// wrong card while the grid scrolls). Cheap no-op when nothing is showing.
    pub fn hide_on_scroll(&self) {
        if self.current.borrow().is_some() {
            self.teardown("scroll");
        }
    }

    /// Hide any active preview when the page holding its card is swapped out.
    pub fn hide_on_page_change(&self) {
        if self.current.borrow().is_some() {
            self.teardown("page-change");
        }
    }

    /// Unconditionally tear down any active preview. Call before navigation (the grid's
    /// activate handler).
    pub fn stop(&self) {
        self.teardown("explicit-stop");
    }

    /// Stop the preview, hide the surface, and clear the active id.
    fn teardown(&self, why: &str) {
        teardown_now(
            &self.engine,
            &self.root,
            &self.current,
            &self.on_click,
            &self.hide_timer,
            &self.prepare,
            &self.tracked,
            &self.animated,
            why,
        );
    }
}
