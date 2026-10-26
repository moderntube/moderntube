//! The Shorts player: one portrait video at a time, laid out as YouTube's desktop Shorts page
//! (card with the title and channel over it, an action column beside it, the comments sheet to
//! its right, navigation at the window edge). The next Short is preloaded as the web client
//! does it: one SABR round on its own session, continued on the swipe (SABR-035).
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;
use gtk::{
    glib, Align, AspectFrame, Box as GtkBox, Button, Image, Label, Orientation, Overlay, Picture, ProgressBar, Revealer,
    Spinner, Stack, StackTransitionType, ToggleButton,
};
use innertube::{ReelEntry, SegmentVod};

use crate::playback::{Player, Prepared};
use crate::runtime::AsyncRuntime;

/// How long a Short takes to slide in; swipes during it are ignored.
const SLIDE_MS: u32 = 260;
/// Vertical drag distance that counts as a swipe.
const DRAG_SWIPE_PX: f64 = 60.0;

pub struct ShortsView {
    pub root: Overlay,
    inner: Rc<Inner>,
}

struct Ui {
    slides: Stack,
    still: Picture,
    spinner: Spinner,
    top_bar: Revealer,
    pause_button: Button,
    mute_button: Button,
    center_icon: Image,
    center: Revealer,
    title: Label,
    channel: Label,
    status: Label,
    progress: ProgressBar,
    like_button: ToggleButton,
    like_count: Label,
    comments_button: ToggleButton,
    comments_count: Label,
    comments_panel: Revealer,
    comments_title_count: Label,
}

struct Inner {
    rt: AsyncRuntime,
    toasts: ToastOverlay,
    /// The account session: the Shorts feed is personal, so its calls go out signed in.
    session: Rc<RefCell<Arc<innertube::Session>>>,
    player: Rc<dyn Player>,
    ui: Ui,
    /// Shorts shown so far; `pos` indexes the one on screen.
    history: RefCell<Vec<ReelEntry>>,
    pos: Cell<usize>,
    /// Shorts the sequence has delivered and the user has not reached.
    queue: RefCell<Vec<ReelEntry>>,
    token: RefCell<Option<String>>,
    sequence_busy: Cell<bool>,
    preloaded: RefCell<Option<(String, Arc<SegmentVod>)>>,
    /// Bumped on every change of Short; a stale async result checks it and drops itself.
    generation: Cell<u64>,
    /// Direction of the slide the next first frame finishes: 1 up, -1 down, 0 none.
    slide_dir: Cell<i32>,
    swipe_locked_until: Cell<Option<Instant>>,
    scroll_acc: Cell<f64>,
    /// The Short on screen has drawn its first frame.
    frame_shown: Cell<bool>,
    hovered: Cell<bool>,
    volume_before_mute: Cell<f64>,
    comments: Rc<super::CommentsView>,
    /// The Short on screen's comments panel token, and the next page's.
    comments_token: RefCell<Option<String>>,
    comments_next: RefCell<Option<String>>,
    /// The generation whose comments are loaded.
    comments_gen: Cell<u64>,
    /// The "add watched videos to your history" setting.
    report_watches: Rc<dyn Fn() -> bool>,
    /// Reports the Short on screen to the account; dropping it sends the final report.
    report: RefCell<Option<innertube::WatchReport>>,
}

fn circle_button(icon: &str, tip: &str, class: &str) -> Button {
    let b = Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.add_css_class(class);
    b.set_focus_on_click(false);
    b
}

/// An action-column entry: a round button with a caption under it.
fn action(button: &impl IsA<gtk::Widget>, caption: &Label) -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 4);
    b.add_css_class("shorts-action");
    b.append(button);
    caption.add_css_class("shorts-action-caption");
    b.append(caption);
    b
}

impl ShortsView {
    pub fn new(
        rt: AsyncRuntime,
        session: Rc<RefCell<Arc<innertube::Session>>>,
        toasts: ToastOverlay,
        report_watches: Rc<dyn Fn() -> bool>,
    ) -> Self {
        let comments = super::CommentsView::new(rt.clone(), session.clone(), toasts.clone());
        let player = crate::playback::player::build();

        // The card: the video, a still for slide transitions, and the overlays.
        let still = Picture::new();
        still.set_content_fit(gtk::ContentFit::Contain);
        let slides = Stack::new();
        slides.set_transition_duration(SLIDE_MS);
        slides.add_named(&player.widget(), Some("live"));
        slides.add_named(&still, Some("still"));
        let card = Overlay::new();
        card.add_css_class("shorts-card");
        card.set_overflow(gtk::Overflow::Hidden);
        card.set_child(Some(&slides));

        let spinner = Spinner::new();
        spinner.set_size_request(48, 48);
        spinner.set_halign(Align::Center);
        spinner.set_valign(Align::Center);
        card.add_overlay(&spinner);

        let pause_button = circle_button("media-playback-pause-symbolic", "Pause (Space)", "shorts-ctl");
        let mute_button = circle_button("audio-volume-high-symbolic", "Mute", "shorts-ctl");
        let top = GtkBox::new(Orientation::Horizontal, 8);
        top.set_margin_top(12);
        top.set_margin_start(12);
        top.append(&pause_button);
        top.append(&mute_button);
        let top_bar = Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(150)
            .child(&top)
            .valign(Align::Start)
            .halign(Align::Start)
            .build();
        card.add_overlay(&top_bar);

        let center_icon = Image::from_icon_name("media-playback-pause-symbolic");
        center_icon.set_pixel_size(36);
        center_icon.add_css_class("shorts-center-icon");
        let center = Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(180)
            .child(&center_icon)
            .halign(Align::Center)
            .valign(Align::Center)
            .can_target(false)
            .build();
        card.add_overlay(&center);

        let channel = Label::builder().xalign(0.0).build();
        channel.add_css_class("shorts-channel");
        let title = Label::builder().xalign(0.0).wrap(true).lines(2).ellipsize(gtk::pango::EllipsizeMode::End).build();
        title.add_css_class("shorts-title");
        let status = Label::builder().xalign(0.0).wrap(true).build();
        status.add_css_class("shorts-status");
        let meta = GtkBox::new(Orientation::Vertical, 6);
        meta.add_css_class("shorts-meta");
        meta.set_valign(Align::End);
        meta.set_can_target(false);
        meta.append(&channel);
        meta.append(&title);
        meta.append(&status);
        card.add_overlay(&meta);

        let progress = ProgressBar::new();
        progress.add_css_class("shorts-progress");
        progress.set_valign(Align::End);
        progress.set_can_target(false);
        card.add_overlay(&progress);

        let frame = AspectFrame::new(0.5, 0.5, 9.0 / 16.0, false);
        frame.set_vexpand(true);
        frame.set_child(Some(&card));

        // Action column, bottom-aligned beside the card.
        let like_button = ToggleButton::builder().icon_name("emote-love-symbolic").tooltip_text("Like").build();
        like_button.add_css_class("shorts-round");
        like_button.set_focus_on_click(false);
        let like_count = Label::new(Some("Like"));
        let comments_button =
            ToggleButton::builder().icon_name("chat-message-new-symbolic").tooltip_text("Comments").build();
        comments_button.add_css_class("shorts-round");
        comments_button.set_focus_on_click(false);
        let comments_count = Label::new(Some("Comments"));
        let share_button = circle_button("send-to-symbolic", "Copy link", "shorts-round");
        let actions = GtkBox::new(Orientation::Vertical, 14);
        actions.set_valign(Align::End);
        actions.set_margin_bottom(8);
        actions.append(&action(&like_button, &like_count));
        actions.append(&action(&comments_button, &comments_count));
        actions.append(&action(&share_button, &Label::new(Some("Share"))));

        // Comments sheet.
        let sheet = GtkBox::new(Orientation::Vertical, 0);
        sheet.add_css_class("shorts-comments");
        sheet.set_size_request(420, -1);
        let header = GtkBox::new(Orientation::Horizontal, 8);
        header.add_css_class("shorts-comments-header");
        let heading = Label::new(Some("Comments"));
        heading.add_css_class("title-3");
        let comments_title_count = Label::new(None);
        comments_title_count.add_css_class("dim-label");
        let close = circle_button("window-close-symbolic", "Close", "flat");
        close.set_hexpand(true);
        close.set_halign(Align::End);
        header.append(&heading);
        header.append(&comments_title_count);
        header.append(&close);
        sheet.append(&header);
        sheet.append(&gtk::Separator::new(Orientation::Horizontal));
        comments.hide_heading();
        comments.set_shorts();
        comments.root.add_css_class("shorts-comments-body");
        let comments_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&comments.root)
            .build();
        sheet.append(&comments_scroller);
        // Absorbs the wheel scrolls the list leaves over, so they do not reach the page's swipe.
        let absorb = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        absorb.connect_scroll(|_, _, _| glib::Propagation::Stop);
        sheet.add_controller(absorb);
        let comments_panel = Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .transition_duration(220)
            .child(&sheet)
            .build();

        let row = GtkBox::new(Orientation::Horizontal, 16);
        row.set_halign(Align::Center);
        row.set_margin_top(12);
        row.set_margin_bottom(12);
        row.append(&frame);
        row.append(&actions);
        row.append(&comments_panel);

        let up = circle_button("go-up-symbolic", "Previous Short (Up)", "shorts-nav");
        let down = circle_button("go-down-symbolic", "Next Short (Down)", "shorts-nav");
        let nav = GtkBox::new(Orientation::Vertical, 16);
        nav.set_halign(Align::End);
        nav.set_valign(Align::Center);
        nav.set_margin_end(24);
        nav.append(&up);
        nav.append(&down);

        let ambient = super::AmbientGlow::new();
        if let Ok(picture) = player.widget().downcast::<Picture>() {
            ambient.set_source(picture.paintable().as_ref());
            let glow = ambient.downgrade();
            picture.connect_paintable_notify(move |p| {
                if let Some(g) = glow.upgrade() {
                    g.set_source(p.paintable().as_ref());
                }
            });
        }
        let root = Overlay::new();
        root.set_child(Some(&ambient));
        root.add_overlay(&row);
        root.add_overlay(&nav);
        root.set_focusable(true);
        // The card fills the page height at 9:16. A Picture's natural size is the stream's own
        // resolution, so a low rung (360x640) would otherwise shrink the card to that width.
        {
            let (frame, panel) = (frame.clone(), comments_panel.clone());
            let last = Cell::new((0, 0, false));
            root.add_tick_callback(move |root, _| {
                let state = (root.width(), root.height(), panel.reveals_child());
                if state != last.get() && state.1 > 0 {
                    last.set(state);
                    let (w, h, open) = state;
                    let reserved = 300 + if open { 436 } else { 0 };
                    let by_height = (h - 24) * 9 / 16;
                    frame.set_size_request(by_height.min(w - reserved).max(0), -1);
                }
                glib::ControlFlow::Continue
            });
        }
        if crate::diag::frame_stats() {
            crate::frame_stats::attach(&root, "shorts");
        }

        let ui = Ui {
            slides,
            still,
            spinner,
            top_bar,
            pause_button: pause_button.clone(),
            mute_button: mute_button.clone(),
            center_icon,
            center,
            title,
            channel,
            status,
            progress,
            like_button: like_button.clone(),
            like_count,
            comments_button: comments_button.clone(),
            comments_count,
            comments_panel,
            comments_title_count,
        };
        let inner = Rc::new(Inner {
            rt,
            toasts,
            session,
            player,
            ui,
            history: RefCell::new(Vec::new()),
            pos: Cell::new(0),
            queue: RefCell::new(Vec::new()),
            token: RefCell::new(None),
            sequence_busy: Cell::new(false),
            preloaded: RefCell::new(None),
            generation: Cell::new(0),
            slide_dir: Cell::new(0),
            swipe_locked_until: Cell::new(None),
            scroll_acc: Cell::new(0.0),
            frame_shown: Cell::new(false),
            hovered: Cell::new(false),
            volume_before_mute: Cell::new(100.0),
            comments,
            comments_token: RefCell::new(None),
            comments_next: RefCell::new(None),
            comments_gen: Cell::new(0),
            report_watches,
            report: RefCell::new(None),
        });

        let weak = || Rc::downgrade(&inner);
        {
            let w = weak();
            up.connect_clicked(move |_| {
                if let Some(i) = w.upgrade() {
                    i.swipe(-1);
                }
            });
            let w = weak();
            down.connect_clicked(move |_| {
                if let Some(i) = w.upgrade() {
                    i.swipe(1);
                }
            });
            let w = weak();
            pause_button.connect_clicked(move |_| {
                if let Some(i) = w.upgrade() {
                    i.toggle_pause();
                }
            });
            let w = weak();
            mute_button.connect_clicked(move |_| {
                if let Some(i) = w.upgrade() {
                    i.toggle_mute();
                }
            });
            let w = weak();
            share_button.connect_clicked(move |b| {
                let Some(i) = w.upgrade() else { return };
                let Some(id) = i.current().map(|e| e.video_id) else { return };
                b.clipboard().set_text(&format!("https://youtube.com/shorts/{id}"));
                i.toasts.add_toast(Toast::new("Link copied"));
            });
            let w = weak();
            like_button.connect_clicked(move |b| {
                if let Some(i) = w.upgrade() {
                    i.like(b.is_active());
                }
            });
            let w = weak();
            comments_button.connect_toggled(move |b| {
                let Some(i) = w.upgrade() else { return };
                i.ui.comments_panel.set_reveal_child(b.is_active());
                if b.is_active() {
                    i.load_comments();
                }
            });
            let cb = comments_button.clone();
            close.connect_clicked(move |_| cb.set_active(false));
            // The comment list grows to its full height; this scroller pages it near the bottom.
            let w = weak();
            let pending = Rc::new(Cell::new(false));
            comments_scroller.vadjustment().connect_value_changed(move |a| {
                if a.upper() - (a.value() + a.page_size()) < 600.0 && !pending.replace(true) {
                    if let Some(i) = w.upgrade() {
                        i.comments.request_load_more();
                    }
                    let pending = pending.clone();
                    glib::timeout_add_local_once(Duration::from_millis(400), move || pending.set(false));
                }
            });
            let w = weak();
            inner.comments.connect_load_more(move || {
                if let Some(i) = w.upgrade() {
                    i.load_more_comments();
                }
            });
        }
        // Tap the video to pause; drag it up or down to swipe.
        {
            let tap = gtk::GestureClick::new();
            let w = weak();
            tap.connect_released(move |g, n, _, _| {
                if n == 1 && g.current_button() == gtk::gdk::BUTTON_PRIMARY {
                    if let Some(i) = w.upgrade() {
                        i.toggle_pause();
                    }
                }
            });
            card.add_controller(tap);
            let drag = gtk::GestureDrag::new();
            let w = weak();
            drag.connect_drag_end(move |g, _, dy| {
                if dy.abs() >= DRAG_SWIPE_PX {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(i) = w.upgrade() {
                        i.swipe(if dy < 0.0 { 1 } else { -1 });
                    }
                }
            });
            card.add_controller(drag);
            let motion = gtk::EventControllerMotion::new();
            let w = weak();
            motion.connect_enter(move |_, _, _| {
                if let Some(i) = w.upgrade() {
                    i.hovered.set(true);
                    i.sync_controls();
                }
            });
            let w = weak();
            motion.connect_leave(move |_| {
                if let Some(i) = w.upgrade() {
                    i.hovered.set(false);
                    i.sync_controls();
                }
            });
            card.add_controller(motion);
        }
        {
            let keys = gtk::EventControllerKey::new();
            let w = weak();
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(i) = w.upgrade() else { return glib::Propagation::Proceed };
                match key {
                    gtk::gdk::Key::Down | gtk::gdk::Key::j => i.swipe(1),
                    gtk::gdk::Key::Up | gtk::gdk::Key::k => i.swipe(-1),
                    gtk::gdk::Key::m => i.toggle_mute(),
                    gtk::gdk::Key::c => i.ui.comments_button.set_active(!i.ui.comments_button.is_active()),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            root.add_controller(keys);
        }
        {
            // A wheel notch is 1.0; a touchpad sends many small deltas, summed to one swipe.
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            let w = weak();
            scroll.connect_scroll(move |_, _, dy| {
                let Some(i) = w.upgrade() else { return glib::Propagation::Proceed };
                let acc = i.scroll_acc.get() + dy;
                if acc.abs() >= 1.0 {
                    i.scroll_acc.set(0.0);
                    i.swipe(if acc > 0.0 { 1 } else { -1 });
                } else {
                    i.scroll_acc.set(acc);
                }
                glib::Propagation::Stop
            });
            row.add_controller(scroll);
        }
        {
            let w = weak();
            inner.player.set_on_failed(Rc::new(move |reason| {
                if let Some(i) = w.upgrade() {
                    i.toasts.add_toast(Toast::new(&crate::playback::stopped_message(&reason)));
                }
            }));
            let w = weak();
            inner.player.set_on_ended(Rc::new(move || {
                if let Some(i) = w.upgrade() {
                    i.player.seek_absolute(0.0);
                    i.player.set_paused(false);
                }
            }));
            let w = weak();
            inner.player.set_on_loaded(Rc::new(move || {
                if let Some(i) = w.upgrade() {
                    i.first_frame();
                }
            }));
        }
        {
            let w = weak();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let Some(i) = w.upgrade() else { return glib::ControlFlow::Break };
                i.tick();
                glib::ControlFlow::Continue
            });
            let w = weak();
            glib::timeout_add_local(Duration::from_secs(1), move || {
                let Some(i) = w.upgrade() else { return glib::ControlFlow::Break };
                if let (Some(r), Some(pos)) = (i.report.borrow().as_ref(), i.player.time_pos_checked()) {
                    r.update(pos, !i.player.is_paused());
                }
                glib::ControlFlow::Continue
            });
        }
        Self { root, inner }
    }

    /// The Shorts tab: the server picks the first Short.
    pub fn open_seedless(&self) {
        let i = self.inner.clone();
        i.reset();
        i.ui.spinner.start();
        let gen = i.generation.get();
        let session = i.feed_session();
        i.rt.clone().spawn(
            async move {
                session.reel_seedless().await.map_err(|e| e.to_string())?.ok_or_else(|| "no Short".to_string())
            },
            move |res| {
                if i.generation.get() != gen {
                    return;
                }
                match res {
                    Ok(start) => {
                        i.token.replace(start.sequence);
                        i.history.replace(vec![start.first]);
                        i.show(0);
                    }
                    Err(e) => {
                        i.ui.spinner.stop();
                        i.ui.status.set_text(&format!("Shorts unavailable: {e}"));
                    }
                }
            },
        );
    }

    /// A Shorts card was clicked.
    pub fn open_seed(&self, entry: ReelEntry) {
        let i = self.inner.clone();
        i.reset();
        i.token.replace(entry.sequence_params.clone());
        i.history.replace(vec![entry]);
        i.show(0);
    }

    pub fn stop(&self) {
        self.inner.reset();
    }

    pub fn grab_focus(&self) {
        self.root.grab_focus();
    }

    pub fn set_paused(&self, paused: bool) {
        self.inner.player.set_paused(paused);
        self.inner.sync_controls();
    }

    pub fn toggle_pause(&self) {
        self.inner.toggle_pause();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.player.is_paused()
    }

    /// The next (`1`) or previous (`-1`) Short.
    pub fn step(&self, delta: i32) {
        self.inner.swipe(delta);
    }

    pub fn current_video_id(&self) -> Option<String> {
        self.inner.current().map(|e| e.video_id)
    }
}

impl Inner {
    fn feed_session(&self) -> Arc<innertube::Session> {
        self.session.borrow().clone()
    }

    fn current(&self) -> Option<ReelEntry> {
        self.history.borrow().get(self.pos.get()).cloned()
    }

    /// `entry` as the playback session can open it: a prefetched player response belongs to
    /// the feed's session, so a different playback session asks `/player` itself.
    fn playable(&self, mut entry: ReelEntry) -> ReelEntry {
        let same = crate::playback::shared_deps().is_ok_and(|(s, _, _)| s.id() == self.feed_session().id());
        if !same {
            entry.prefetched = None;
        }
        entry
    }

    fn reset(&self) {
        self.report.replace(None);
        self.generation.set(self.generation.get() + 1);
        self.player.stop();
        if let Some((_, vod)) = self.preloaded.take() {
            vod.stop();
        }
        self.history.borrow_mut().clear();
        self.queue.borrow_mut().clear();
        self.token.replace(None);
        self.pos.set(0);
        self.slide_dir.set(0);
        self.ui.still.set_paintable(gtk::gdk::Paintable::NONE);
        self.ui.slides.set_visible_child_full("still", StackTransitionType::None);
        self.ui.title.set_text("");
        self.ui.channel.set_text("");
        self.ui.status.set_text("");
        self.ui.progress.set_fraction(0.0);
    }

    fn toggle_pause(&self) {
        self.player.toggle_pause();
        let icon = if self.player.is_paused() { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" };
        self.ui.center_icon.set_icon_name(Some(icon));
        self.ui.center.set_reveal_child(true);
        let center = self.ui.center.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || center.set_reveal_child(false));
        self.sync_controls();
    }

    fn toggle_mute(&self) {
        let v = self.player.volume();
        if v > 0.0 {
            self.volume_before_mute.set(v);
            self.player.set_volume(0.0);
        } else {
            self.player.set_volume(self.volume_before_mute.get().max(1.0));
        }
        self.sync_controls();
    }

    /// Hover or pause shows the controls; the icons follow the player's state.
    fn sync_controls(&self) {
        let paused = self.player.is_paused();
        self.ui.top_bar.set_reveal_child(self.hovered.get() || paused);
        self.ui
            .pause_button
            .set_icon_name(if paused { "media-playback-start-symbolic" } else { "media-playback-pause-symbolic" });
        let muted = self.player.volume() <= 0.0;
        self.ui
            .mute_button
            .set_icon_name(if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" });
    }

    fn tick(&self) {
        if let (Some(pos), Some(dur)) = (self.player.time_pos_checked(), self.player.duration()) {
            if dur > 0.0 {
                self.ui.progress.set_fraction((pos / dur).clamp(0.0, 1.0));
            }
        }
    }

    /// A swipe from the user: one at a time, never while a slide is still running.
    fn swipe(self: &Rc<Self>, delta: i32) {
        let now = Instant::now();
        if self.swipe_locked_until.get().is_some_and(|t| now < t) {
            return;
        }
        self.swipe_locked_until.set(Some(now + Duration::from_millis(u64::from(SLIDE_MS) + 120)));
        self.step(delta);
    }

    fn step(self: &Rc<Self>, delta: i32) {
        let len = self.history.borrow().len();
        if len == 0 {
            return;
        }
        let target = self.pos.get() as i64 + i64::from(delta);
        if target < 0 {
            return;
        }
        let target = target as usize;
        if target >= len {
            let next = {
                let mut q = self.queue.borrow_mut();
                (!q.is_empty()).then(|| q.remove(0))
            };
            match next {
                Some(e) => self.history.borrow_mut().push(e),
                None => {
                    self.ui.status.set_text("Loading more Shorts…");
                    self.fetch_sequence();
                    return;
                }
            }
        }
        self.freeze_frame();
        self.slide_dir.set(delta.signum());
        self.show(target);
    }

    fn current_frame(&self) -> Option<gtk::gdk::Paintable> {
        self.player.widget().downcast::<Picture>().ok().and_then(|p| p.paintable()).map(|p| p.current_image())
    }

    /// Cover the video with its current frame, so the next Short can slide in over it.
    fn freeze_frame(&self) {
        let frame = self.current_frame();
        self.ui.still.set_paintable(frame.as_ref());
        self.ui.slides.set_visible_child_full("still", StackTransitionType::None);
    }

    fn first_frame(self: &Rc<Self>) {
        tracing::info!("[shorts] first frame #{} uptime {}ms", self.pos.get(), crate::uptime_ms());
        self.frame_shown.set(true);
        self.ui.spinner.stop();
        let transition = match self.slide_dir.replace(0) {
            1 => StackTransitionType::SlideUp,
            -1 => StackTransitionType::SlideDown,
            _ => StackTransitionType::Crossfade,
        };
        self.ui.slides.set_visible_child_full("live", transition);
        self.sync_controls();
        self.preload_next();
    }

    /// Put `history[idx]` on screen: the preloaded session when it is that Short, else a cold open.
    fn show(self: &Rc<Self>, idx: usize) {
        let Some(entry) = self.history.borrow().get(idx).cloned() else { return };
        self.generation.set(self.generation.get() + 1);
        let gen = self.generation.get();
        self.pos.set(idx);
        self.frame_shown.set(false);
        let session = self.feed_session();
        let report = ((self.report_watches)() && session.is_authenticated()).then(|| {
            let (handle, task) = innertube::WatchReport::new_short(session, &entry.video_id);
            self.rt.spawn_bg(task);
            handle
        });
        self.report.replace(report);
        self.comments.clear();
        self.comments.set_video_id(&entry.video_id);
        self.comments_token.replace(None);
        self.comments_next.replace(None);
        self.ui.comments_count.set_text("Comments");
        self.ui.comments_title_count.set_text("");
        self.ui.like_count.set_text("Like");
        self.ui.like_button.set_active(false);
        self.ui.like_button.remove_css_class("liked");
        self.ui.title.set_text("");
        self.ui.channel.set_text("");
        self.ui.status.set_text("");
        self.ui.progress.set_fraction(0.0);
        tracing::info!("[shorts] show #{idx} {} uptime {}ms", entry.video_id, crate::uptime_ms());
        let pre = self.preloaded.take();
        match pre {
            Some((id, vod)) if id == entry.video_id => {
                tracing::info!("[shorts] {id}: continuing the preloaded session");
                vod.activate();
                self.player.load(Prepared::Vod(vod));
            }
            other => {
                if let Some((_, vod)) = other {
                    vod.stop();
                }
                self.ui.spinner.start();
                self.player.stop();
                let me = self.clone();
                let e = self.playable(entry.clone());
                self.rt.spawn(crate::playback::prepare_short(e, false), move |res| {
                    if me.generation.get() != gen {
                        if let Ok(vod) = res {
                            vod.stop();
                        }
                        return;
                    }
                    match res {
                        Ok(vod) => me.player.load(Prepared::Vod(vod)),
                        Err(err) => {
                            me.ui.spinner.stop();
                            me.ui.status.set_text(&format!("Could not play this Short: {err}"));
                        }
                    }
                });
            }
        }
        let me = self.clone();
        let e = entry.clone();
        let session = self.feed_session();
        self.rt.spawn(async move { session.reel_item_watch(&e).await.ok() }, move |overlay| {
            if me.generation.get() != gen {
                return;
            }
            let Some(o) = overlay else { return };
            me.ui.title.set_text(o.title.as_deref().unwrap_or(""));
            me.ui.channel.set_text(o.channel.as_deref().unwrap_or(""));
            if let Some(n) = &o.likes {
                me.ui.like_count.set_text(n);
            }
            me.ui.like_button.set_active(o.liked);
            if o.liked {
                me.ui.like_button.add_css_class("liked");
            }
            if let Some(n) = &o.comment_count {
                me.ui.comments_count.set_text(n);
                me.ui.comments_title_count.set_text(n);
            }
            me.comments_token.replace(o.comments_token);
            if me.ui.comments_button.is_active() {
                me.load_comments();
            }
        });
        if self.queue.borrow().len() < 3 {
            self.fetch_sequence();
        }
    }

    fn like(self: &Rc<Self>, on: bool) {
        let Some(id) = self.current().map(|e| e.video_id) else { return };
        let session = self.feed_session();
        if !session.is_authenticated() {
            self.ui.like_button.set_active(false);
            self.toasts.add_toast(Toast::new("Sign in to like Shorts"));
            return;
        }
        if on {
            self.ui.like_button.add_css_class("liked");
        } else {
            self.ui.like_button.remove_css_class("liked");
        }
        let toasts = self.toasts.clone();
        self.rt.spawn(
            async move { if on { session.like(&id).await } else { session.remove_like(&id).await } },
            move |res| {
                if let Err(e) = res {
                    toasts.add_toast(Toast::new(&format!("Like failed: {e}")));
                }
            },
        );
    }

    /// The Short on screen's first comments page, once per Short.
    fn load_comments(self: &Rc<Self>) {
        let gen = self.generation.get();
        if self.comments_gen.get() == gen {
            return;
        }
        let Some(token) = self.comments_token.borrow().clone() else { return };
        self.comments_gen.set(gen);
        let me = self.clone();
        let session = self.feed_session();
        self.rt.spawn(async move { session.reel_comments_page(&token).await }, move |res| {
            if me.generation.get() != gen {
                return;
            }
            match res {
                Ok(page) => {
                    me.comments_next.replace(page.continuation.clone());
                    me.comments.set_comments(&page.items);
                }
                Err(e) => tracing::warn!("[shorts] comments failed: {e}"),
            }
        });
    }

    fn load_more_comments(self: &Rc<Self>) {
        let Some(token) = self.comments_next.take() else { return };
        let gen = self.generation.get();
        let me = self.clone();
        let session = self.feed_session();
        self.rt.spawn(async move { session.reel_comments_page(&token).await }, move |res| {
            if me.generation.get() != gen {
                return;
            }
            if let Ok(page) = res {
                me.comments_next.replace(page.continuation.clone());
                me.comments.append_comments(&page.items);
            }
        });
    }

    fn fetch_sequence(self: &Rc<Self>) {
        if self.sequence_busy.get() {
            return;
        }
        let Some(token) = self.token.borrow().clone() else { return };
        let Some(current) = self.current().map(|e| e.video_id) else { return };
        self.sequence_busy.set(true);
        let me = self.clone();
        let session = self.feed_session();
        self.rt.spawn(
            async move {
                let sts = innertube::session_signature_timestamp(&session).await;
                session.reel_watch_sequence(&token, &current, sts).await.map_err(|e| e.to_string())
            },
            move |res| {
                me.sequence_busy.set(false);
                match res {
                    Ok(seq) => {
                        let ads = seq.entries.iter().filter(|e| e.is_ad).count();
                        tracing::info!("[shorts] sequence: +{} entries, {ads} ads dropped", seq.entries.len() - ads);
                        me.queue.borrow_mut().extend(seq.entries.into_iter().filter(|e| !e.is_ad));
                        me.token.replace(seq.continuation);
                        if me.ui.status.text() == "Loading more Shorts…" {
                            me.ui.status.set_text("");
                        }
                        if me.frame_shown.get() {
                            me.preload_next();
                        }
                    }
                    Err(e) => tracing::warn!("[shorts] sequence failed: {e}"),
                }
            },
        );
    }

    /// Once the Short on screen has its first frame, start the next one: one round, then hold.
    fn preload_next(self: &Rc<Self>) {
        let next = {
            let pos = self.pos.get();
            let hist = self.history.borrow();
            hist.get(pos + 1).cloned().or_else(|| self.queue.borrow().first().cloned())
        };
        let Some(next) = next else { return };
        if self.preloaded.borrow().as_ref().is_some_and(|(id, _)| *id == next.video_id) {
            return;
        }
        let gen = self.generation.get();
        let me = self.clone();
        let id = next.video_id.clone();
        tracing::info!("[shorts] preload {id} uptime {}ms", crate::uptime_ms());
        let next = self.playable(next);
        self.rt.spawn(crate::playback::prepare_short(next, true), move |res| match res {
            Ok(vod) if me.generation.get() == gen => {
                if let Some((_, old)) = me.preloaded.replace(Some((id, vod))) {
                    old.stop();
                }
            }
            Ok(vod) => vod.stop(),
            Err(e) => tracing::warn!("[shorts] preload {id} failed: {e}"),
        });
    }
}
