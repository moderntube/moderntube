//! `ChannelView`: a channel page. The header (banner, identity, tabs, sort chips) is the
//! first row of the reused [`ResultsView`], so it scrolls away with the grid as on the
//! web client. Tabs are Videos / Shorts / Live; Shorts render as one portrait grid.
//!
//! Sort chips carry a `browse` continuation token (`SearchResults.sort_chips`);
//! selecting one reloads the grid sorted via `channel_tab_continuation`. A
//! [`Generation`] counter drops stale loads when the user switches tab or sort fast.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;
use gtk::{Align, Box as GtkBox, Button, Label, Orientation, Overlay, Picture};

use innertube::{Channel, ChannelTab, Error, SearchItem, SearchResults, Session};

use crate::models::ResultItem;
use crate::playback::QueueAction;
use crate::runtime::{AsyncRuntime, Generation};

use super::hover_preview::HoverPreview;
use super::result_row::fetch_bytes;
use super::ResultsView;

/// Rendered and decoded size of the channel avatar; a Picture's size request only sets
/// its minimum, so the texture is decoded at this size.
const AVATAR_PX: i32 = 128;

/// How much of the description the collapsed form shows.
const DESC_COLLAPSED_CHARS: usize = 110;

/// Banner aspect: the web client crops the 2560x424 banner to about 6.2:1.
const BANNER_ASPECT: f64 = 6.2;

const TABS: [(ChannelTab, &str); 3] =
    [(ChannelTab::Videos, "Videos"), (ChannelTab::Shorts, "Shorts"), (ChannelTab::Live, "Live")];

struct State {
    channel_id: String,
    tab: ChannelTab,
    continuation: Option<String>,
}

pub struct ChannelView {
    pub root: GtkBox,
    results: Rc<ResultsView>,
    title: Label,
    verified: Label,
    meta: Label,
    banner: Picture,
    banner_frame: GtkBox,
    avatar: Picture,
    description: Label,
    desc_toggle: Button,
    desc_expanded: Cell<bool>,
    desc_full: RefCell<String>,
    link: Button,
    link_url: RefCell<Option<String>>,
    subscribe_btn: Button,
    subscribed: Cell<bool>,
    chip_row: GtkBox,
    tab_buttons: Vec<(ChannelTab, Button)>,
    state: RefCell<State>,
    generation: Generation,
    loading: Cell<bool>,
    rt: AsyncRuntime,
    session: Rc<RefCell<Arc<Session>>>,
    toasts: ToastOverlay,
}

impl ChannelView {
    /// `on_activate` plays a video, `on_channel` opens another channel, `on_back` leaves.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rt: AsyncRuntime,
        session: Rc<RefCell<Arc<Session>>>,
        toasts: ToastOverlay,
        preview: Rc<HoverPreview>,
        on_activate: Rc<dyn Fn(ResultItem)>,
        on_channel: Rc<dyn Fn(String, String)>,
        on_queue: Rc<dyn Fn(ResultItem, QueueAction)>,
        on_back: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let results = Rc::new(ResultsView::new(
            rt.clone(),
            preview,
            move |it| on_activate(it),
            move |id, name| on_channel(id, name),
            move |item, action| on_queue(item, action),
            // The channel tabs list videos only, so no playlist card reaches this.
            |_id, _name| {},
        ));

        let banner = Picture::builder().content_fit(gtk::ContentFit::Cover).hexpand(true).build();
        let banner_frame = GtkBox::builder().css_classes(["channel-banner"]).overflow(gtk::Overflow::Hidden).visible(false).build();
        banner_frame.append(&banner);

        let avatar = Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .width_request(AVATAR_PX)
            .height_request(AVATAR_PX)
            .halign(Align::Center)
            .valign(Align::Start)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["channel-page-avatar"])
            .build();

        let title = Label::builder().xalign(0.0).wrap(true).css_classes(["channel-name"]).build();
        let verified = Label::builder().label("\u{2714}").css_classes(["channel-verified"]).tooltip_text("Verified").visible(false).build();
        let name_row = GtkBox::new(Orientation::Horizontal, 8);
        name_row.append(&title);
        name_row.append(&verified);

        let meta = Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();

        let description = Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        let desc_toggle = Button::builder().label("…more").css_classes(["flat", "channel-inline-link"]).valign(Align::Start).build();
        let desc_row = GtkBox::new(Orientation::Horizontal, 4);
        desc_row.set_halign(Align::Start);
        desc_row.set_visible(false);
        desc_row.append(&description);
        desc_row.append(&desc_toggle);

        let link = Button::builder().css_classes(["flat", "channel-link"]).halign(Align::Start).visible(false).build();

        let subscribe_btn = Button::builder().label("Subscribe").css_classes(["channel-subscribe"]).halign(Align::Start).visible(false).build();

        let info = GtkBox::new(Orientation::Vertical, 6);
        info.set_valign(Align::Center);
        info.set_hexpand(true);
        info.append(&name_row);
        info.append(&meta);
        info.append(&desc_row);
        info.append(&link);
        info.append(&subscribe_btn);
        subscribe_btn.set_margin_top(6);

        let identity = GtkBox::new(Orientation::Horizontal, 24);
        identity.add_css_class("channel-identity");
        identity.append(&avatar);
        identity.append(&info);

        let tab_bar = GtkBox::new(Orientation::Horizontal, 4);
        tab_bar.add_css_class("channel-tabs");
        let tab_buttons: Vec<(ChannelTab, Button)> = TABS
            .iter()
            .map(|(tab, label)| {
                let b = Button::builder().label(*label).css_classes(["flat", "channel-tab"]).build();
                tab_bar.append(&b);
                (*tab, b)
            })
            .collect();

        let chip_row = GtkBox::new(Orientation::Horizontal, 8);
        chip_row.add_css_class("channel-chips");

        let header = GtkBox::new(Orientation::Vertical, 0);
        header.add_css_class("channel-header");
        header.append(&banner_frame);
        header.append(&identity);
        header.append(&tab_bar);
        header.append(&chip_row);
        // External horizontal policy gives the header a minimum width of 0, so the list may
        // measure it at any width during a page transition.
        let header_clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&header)
            .build();
        results.set_header(&header_clip);

        let back = Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .css_classes(["osd", "circular", "channel-back"])
            .halign(Align::Start)
            .valign(Align::Start)
            .build();
        back.connect_clicked(move |_| on_back());
        let stage = Overlay::new();
        stage.set_child(Some(&results.outer));
        stage.add_overlay(&back);

        let root = GtkBox::new(Orientation::Vertical, 0);
        root.append(&stage);
        stage.set_vexpand(true);

        // The banner keeps its aspect as the page resizes; its frame width is the viewport's.
        {
            let banner_frame = banner_frame.clone();
            results.root.hadjustment().connect_changed(move |a| {
                let w = a.page_size() - 32.0;
                if w > 0.0 {
                    banner_frame.set_size_request(-1, (w / BANNER_ASPECT).clamp(80.0, 220.0) as i32);
                }
            });
        }

        let this = Rc::new(Self {
            root,
            results,
            title,
            verified,
            meta,
            banner,
            banner_frame,
            avatar,
            description,
            desc_toggle: desc_toggle.clone(),
            desc_expanded: Cell::new(false),
            desc_full: RefCell::new(String::new()),
            link: link.clone(),
            link_url: RefCell::new(None),
            subscribe_btn: subscribe_btn.clone(),
            subscribed: Cell::new(false),
            chip_row,
            tab_buttons,
            state: RefCell::new(State {
                channel_id: String::new(),
                tab: ChannelTab::Videos,
                continuation: None,
            }),
            generation: Generation::default(),
            loading: Cell::new(false),
            rt,
            session,
            toasts,
        });

        {
            let t = this.clone();
            desc_toggle.connect_clicked(move |_| t.toggle_description());
        }
        let weak = Rc::downgrade(&this);
        crate::hooks::on("desc-toggle", move |spec| {
            for part in spec.split(',') {
                let Ok(secs) = part.trim().parse::<u32>() else { continue };
                let t = weak.clone();
                gtk::glib::timeout_add_seconds_local_once(secs, move || {
                    let Some(t) = t.upgrade() else { return };
                    tracing::info!("[test] description toggle");
                    t.toggle_description();
                });
            }
        });
        let weak = Rc::downgrade(&this);
        crate::hooks::on("channel-tab", move |v| {
            let Some((secs, name)) = v.split_once(':').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.to_string())))
            else {
                return;
            };
            let t = weak.clone();
            gtk::glib::timeout_add_seconds_local_once(secs, move || {
                let Some(t) = t.upgrade() else { return };
                if let Some((tab, _)) = TABS.iter().find(|(_, l)| *l == name) {
                    tracing::info!("[test] channel tab {name}");
                    t.load(*tab);
                }
            });
        });
        for (tab, b) in &this.tab_buttons {
            let (t, tab) = (this.clone(), *tab);
            b.connect_clicked(move |_| t.load(tab));
        }
        {
            let t = this.clone();
            subscribe_btn.connect_clicked(move |_| t.toggle_subscribe());
        }
        {
            let t = this.clone();
            link.connect_clicked(move |b| {
                if let Some(url) = t.link_url.borrow().clone() {
                    let win = b.root().and_downcast::<gtk::Window>();
                    gtk::UriLauncher::new(&url).launch(win.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
                }
            });
        }
        {
            let t = this.clone();
            this.results.connect_load_more(move || t.load_more());
        }

        this
    }

    /// Open `channel_id`: show the Videos tab immediately, then fill the header from
    /// `channel()` and reveal the tabs the channel has.
    pub fn open(self: &Rc<Self>, channel_id: String, name: String) {
        self.title.set_text(if name.is_empty() { "Channel" } else { &name });
        self.verified.set_visible(false);
        self.meta.set_text("");
        self.link.set_visible(false);
        self.subscribe_btn.set_visible(false);
        self.banner_frame.set_visible(false);
        self.banner.set_paintable(gtk::gdk::Paintable::NONE);
        self.avatar.set_paintable(gtk::gdk::Paintable::NONE);
        for (tab, b) in &self.tab_buttons {
            b.set_visible(*tab == ChannelTab::Videos);
        }
        self.set_description(None);
        self.state.borrow_mut().channel_id = channel_id.clone();
        self.load(ChannelTab::Videos);

        let current = self.session.borrow().clone();
        let this = self.clone();
        // A late probe for a channel the page has since left must not write its header
        // (a wrong subscribe state would unsubscribe from something else).
        let probed = channel_id.clone();
        self.rt.spawn(
            async move { current.channel(&channel_id).await },
            move |res: Result<Channel, Error>| {
                let Ok(ch) = res else { return };
                if this.state.borrow().channel_id != probed {
                    return;
                }
                this.fill_header(ch);
            },
        );
    }

    fn fill_header(self: &Rc<Self>, ch: Channel) {
        if let Some(name) = ch.name.as_deref() {
            self.title.set_text(name);
        }
        self.verified.set_visible(ch.verified);
        let meta: Vec<&str> =
            [ch.handle.as_deref(), ch.subscribers.as_deref(), ch.video_count.as_deref()].into_iter().flatten().collect();
        self.meta.set_text(&meta.join("  ·  "));
        for (tab, b) in &self.tab_buttons {
            let has = match tab {
                ChannelTab::Videos => true,
                ChannelTab::Shorts => ch.tabs.iter().any(|t| t == "Shorts"),
                ChannelTab::Live => ch.tabs.iter().any(|t| t == "Live" || t == "Streams"),
            };
            b.set_visible(has);
        }
        match ch.link {
            Some((label, url)) => {
                self.link.set_label(&format!("\u{1F517} {label}"));
                self.link.set_tooltip_text(Some(&url));
                self.link_url.replace(Some(url));
                self.link.set_visible(true);
            }
            None => {
                self.link_url.replace(None);
            }
        }
        self.set_subscribed(ch.subscribed);
        self.subscribe_btn.set_visible(true);
        self.set_description(ch.description.as_deref());
        if let Some(url) = ch.avatar_url {
            let avatar = self.avatar.clone();
            self.rt.spawn(async move { fetch_bytes(url).await }, move |bytes: Option<Vec<u8>>| {
                let Some(b) = bytes else { return };
                let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_owned(b));
                let Ok(scaled) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                    &stream,
                    AVATAR_PX,
                    AVATAR_PX,
                    true,
                    gtk::gio::Cancellable::NONE,
                ) else {
                    return;
                };
                avatar.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&scaled)));
            });
        }
        if let Some(url) = ch.banner_url {
            let (banner, frame) = (self.banner.clone(), self.banner_frame.clone());
            self.rt.spawn(async move { fetch_bytes(url).await }, move |bytes: Option<Vec<u8>>| {
                let Some(b) = bytes else { return };
                if let Ok(tex) = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(b)) {
                    banner.set_paintable(Some(&tex));
                    frame.set_visible(true);
                }
            });
        }
    }

    /// Show the channel description, or hide its row when there is none.
    fn set_description(&self, text: Option<&str>) {
        let text = text.map(str::trim).unwrap_or("");
        let Some(row) = self.description.parent() else { return };
        if text.is_empty() {
            row.set_visible(false);
            return;
        }
        *self.desc_full.borrow_mut() = text.to_string();
        self.desc_toggle.set_visible(Self::collapsed_text(text) != text);
        self.collapse_description();
        row.set_visible(true);
    }

    /// The first line of the description, cut to [`DESC_COLLAPSED_CHARS`] at a word boundary.
    fn collapsed_text(full: &str) -> String {
        let first = full.lines().next().unwrap_or("").trim();
        if first.chars().count() <= DESC_COLLAPSED_CHARS {
            return first.to_string();
        }
        let cut: String = first.chars().take(DESC_COLLAPSED_CHARS).collect();
        let cut = match cut.rfind(' ') {
            Some(i) if i > DESC_COLLAPSED_CHARS / 2 => &cut[..i],
            _ => &cut,
        };
        format!("{cut}\u{2026}")
    }

    fn collapse_description(&self) {
        self.desc_expanded.set(false);
        self.description.set_text(&Self::collapsed_text(&self.desc_full.borrow()));
        self.desc_toggle.set_label("…more");
    }

    fn toggle_description(&self) {
        tracing::info!(
            "[channel] description {} ({} chars shown)",
            if self.desc_expanded.get() { "collapsing" } else { "expanding" },
            self.description.text().chars().count()
        );
        if self.desc_expanded.get() {
            self.collapse_description();
        } else {
            self.desc_expanded.set(true);
            self.description.set_text(&self.desc_full.borrow());
            self.desc_toggle.set_label("less");
        }
    }

    /// Subscribe/unsubscribe toggle (optimistic; reverts on failure).
    fn toggle_subscribe(self: &Rc<Self>) {
        let id = self.state.borrow().channel_id.clone();
        if id.is_empty() {
            return;
        }
        let was = self.subscribed.get();
        self.set_subscribed(!was);
        let current = self.session.borrow().clone();
        let this = self.clone();
        self.rt.spawn(
            async move {
                if was {
                    current.unsubscribe(&id).await
                } else {
                    current.subscribe(&id).await
                }
            },
            move |res| {
                if let Err(e) = res {
                    this.set_subscribed(was);
                    this.toasts.add_toast(Toast::new(&format!("Subscribe failed: {e}")));
                }
            },
        );
    }

    fn set_subscribed(&self, sub: bool) {
        self.subscribed.set(sub);
        self.subscribe_btn.set_label(if sub { "Subscribed" } else { "Subscribe" });
        if sub {
            self.subscribe_btn.add_css_class("subscribed");
        } else {
            self.subscribe_btn.remove_css_class("subscribed");
        }
    }

    /// Load a tab fresh (reset grid + chips, bump generation).
    fn load(self: &Rc<Self>, tab: ChannelTab) {
        let token = self.generation.bump();
        {
            let mut s = self.state.borrow_mut();
            s.tab = tab;
            s.continuation = None;
        }
        self.set_active_tab(tab);
        self.results.set_items(std::iter::empty());
        self.results.set_loading(true);
        self.clear_chips();
        let id = self.state.borrow().channel_id.clone();
        let current = self.session.borrow().clone();
        let this = self.clone();
        self.rt.spawn(
            async move {
                match tab {
                    ChannelTab::Live => current.channel_live(&id).await,
                    ChannelTab::Shorts => current.channel_shorts(&id).await,
                    ChannelTab::Videos => current.channel_videos(&id).await,
                }
            },
            move |res: Result<SearchResults, Error>| this.apply(token, res, true),
        );
    }

    /// Reload the current tab sorted by a chip's continuation token.
    fn load_sort(self: &Rc<Self>, sort_token: String) {
        let token = self.generation.bump();
        self.state.borrow_mut().continuation = None;
        self.results.set_items(std::iter::empty());
        self.results.set_loading(true);
        let current = self.session.borrow().clone();
        let this = self.clone();
        self.rt.spawn(
            async move { current.channel_tab_continuation(&sort_token).await },
            move |res: Result<SearchResults, Error>| this.apply(token, res, true),
        );
    }

    /// Fetch the next page (continuation) and append.
    fn load_more(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        let Some(cont) = self.state.borrow().continuation.clone() else { return };
        self.loading.set(true);
        let token = self.generation.current();
        let current = self.session.borrow().clone();
        let this = self.clone();
        self.rt.spawn(
            async move { current.channel_tab_continuation(&cont).await },
            move |res: Result<SearchResults, Error>| this.apply(token, res, false),
        );
    }

    /// Shared result handler: `reload` replaces the grid + renders chips; otherwise appends.
    /// Drops the result if a newer load superseded this one (`generation`).
    fn apply(self: &Rc<Self>, token: u64, res: Result<SearchResults, Error>, reload: bool) {
        self.loading.set(false);
        if !self.generation.is_current(token) {
            return;
        }
        match res {
            Ok(found) => {
                self.state.borrow_mut().continuation = found.continuation.clone();
                let shorts_tab = self.state.borrow().tab == ChannelTab::Shorts;
                let (shorts, items): (Vec<&SearchItem>, Vec<&SearchItem>) =
                    found.items.iter().partition(|it| shorts_tab && matches!(it, SearchItem::Short { .. }));
                let shorts: Vec<ResultItem> = shorts.into_iter().filter_map(ResultItem::from_search_item).collect();
                let items: Vec<ResultItem> = items.into_iter().filter_map(ResultItem::from_search_item).collect();
                if reload {
                    self.results.set_items(items);
                    let chips: Vec<(String, String, bool)> = found
                        .sort_chips
                        .iter()
                        .map(|c| (c.label.clone(), c.token.clone(), c.selected))
                        .collect();
                    self.render_chips(chips);
                } else {
                    self.results.append_items(items);
                }
                if !shorts.is_empty() {
                    self.results.append_to_shelf(shorts);
                }
            }
            Err(e) => {
                self.results.set_loading(false);
                self.toasts.add_toast(Toast::new(&format!("Couldn't load channel: {e}")))
            }
        }
    }

    fn set_active_tab(&self, tab: ChannelTab) {
        for (t, b) in &self.tab_buttons {
            if *t == tab {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    }

    fn clear_chips(&self) {
        while let Some(c) = self.chip_row.first_child() {
            self.chip_row.remove(&c);
        }
        self.chip_row.set_visible(false);
    }

    fn render_chips(self: &Rc<Self>, chips: Vec<(String, String, bool)>) {
        self.clear_chips();
        self.chip_row.set_visible(!chips.is_empty());
        for (label, token, selected) in chips {
            let btn = Button::builder().label(&label).css_classes(["channel-chip"]).build();
            if selected {
                btn.add_css_class("selected");
            }
            let t = self.clone();
            btn.connect_clicked(move |_| t.load_sort(token.clone()));
            self.chip_row.append(&btn);
        }
    }
}
