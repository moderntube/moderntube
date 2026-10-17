//! `PlaylistView`: a playlist page with a header (back, lead thumbnail, title, item count,
//! "Play all") over the numbered videos in playlist order.
//!
//! Rows are a `GtkListView` over a `GListStore<ResultItem>`, so only visible rows fetch
//! thumbnails. Activating row N hands `(items, N)` to `on_play`, which starts there and queues
//! the rest.
//!
//! `Session::playlist` returns the whole page with no continuation token, so a playlist longer
//! than one browse page shows its first page.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;
use gtk::gio;
use gtk::{
    Align, Box as GtkBox, Button, Label, ListItem, ListView, Orientation, Overlay, Picture,
    ScrolledWindow, SignalListItemFactory, SingleSelection, Spinner,
};

use innertube::{Error, PlaylistContents, Session};

use crate::models::ResultItem;
use crate::runtime::{AsyncRuntime, Generation};

use super::result_row::{fetch_bytes, ThumbCache};

/// Play-through callback: play `items[start]` and queue the rest of `items` behind it.
pub type OnPlay = Rc<dyn Fn(&[ResultItem], usize)>;

pub struct PlaylistView {
    pub root: GtkBox,
    title: Label,
    subtitle: Label,
    /// The lead video's thumbnail, shown beside the title once the contents arrive.
    art: Picture,
    play_all: Button,
    store: gio::ListStore,
    /// The ordered items on screen; the play-through source for a row activation.
    items: Rc<RefCell<Vec<ResultItem>>>,
    thumb_cache: ThumbCache,
    spinner: Spinner,
    empty: Label,
    generation: Generation,
    rt: AsyncRuntime,
    session: Rc<RefCell<Arc<Session>>>,
    toasts: ToastOverlay,
    on_play: OnPlay,
}

impl PlaylistView {
    /// Build the playlist page. `on_play(items, start)` plays `items[start]` and queues
    /// the rest; `on_back()` returns to the previous page.
    pub fn new(
        rt: AsyncRuntime,
        session: Rc<RefCell<Arc<Session>>>,
        toasts: ToastOverlay,
        on_play: OnPlay,
        on_back: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let store = gio::ListStore::new::<ResultItem>();
        let items: Rc<RefCell<Vec<ResultItem>>> = Rc::new(RefCell::new(Vec::new()));
        let thumb_cache: ThumbCache = Default::default();

        let factory = SignalListItemFactory::new();
        {
            let rt = rt.clone();
            let cache = thumb_cache.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<ListItem>().unwrap();
                let Some(item) = li.item().and_downcast::<ResultItem>() else { return };
                li.set_child(Some(&build_row(li.position(), &item, &rt, &cache)));
            });
        }

        let selection = SingleSelection::new(Some(store.clone()));
        let list = ListView::new(Some(selection), Some(factory));
        list.set_single_click_activate(true);
        {
            let items = items.clone();
            let on_play = on_play.clone();
            list.connect_activate(move |_, pos| {
                // Snapshot before the callback: it plays a video, and holding the borrow
                // across that would outlive the row click.
                let snapshot = items.borrow().clone();
                on_play(&snapshot, pos as usize);
            });
        }

        // Header: back, lead thumbnail, title + count, "Play all".
        let header = GtkBox::new(Orientation::Horizontal, 12);
        header.add_css_class("toolbar");
        let back = Button::from_icon_name("go-previous-symbolic");
        back.set_valign(Align::Center);
        back.connect_clicked(move |_| on_back());
        let art = Picture::builder().content_fit(gtk::ContentFit::Cover).build();
        art.set_size_request(160, 90);
        art.add_css_class("card");
        art.set_valign(Align::Center);
        art.set_visible(false);
        let title = Label::builder().xalign(0.0).css_classes(["title-2"]).build();
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let subtitle = Label::builder().xalign(0.0).css_classes(["dim-label", "caption"]).build();
        let heading = GtkBox::new(Orientation::Vertical, 2);
        heading.set_valign(Align::Center);
        heading.set_hexpand(true);
        heading.append(&title);
        heading.append(&subtitle);
        let play_all = Button::builder()
            .label("Play all")
            .css_classes(["suggested-action"])
            .valign(Align::Center)
            .sensitive(false)
            .build();
        header.append(&back);
        header.append(&art);
        header.append(&heading);
        header.append(&play_all);

        let scroller = ScrolledWindow::builder().hexpand(true).vexpand(true).child(&list).build();
        let spinner = Spinner::builder()
            .halign(Align::Center)
            .valign(Align::Center)
            .width_request(32)
            .height_request(32)
            .visible(false)
            .build();
        let empty = Label::builder()
            .label("This playlist is empty")
            .halign(Align::Center)
            .valign(Align::Center)
            .visible(false)
            .css_classes(["dim-label", "title-2"])
            .build();
        let body = Overlay::new();
        body.set_child(Some(&scroller));
        body.add_overlay(&spinner);
        body.add_overlay(&empty);

        let root = GtkBox::new(Orientation::Vertical, 6);
        root.append(&header);
        root.append(&body);

        let this = Rc::new(Self {
            root,
            title,
            subtitle,
            art,
            play_all: play_all.clone(),
            store,
            items,
            thumb_cache,
            spinner,
            empty,
            generation: Generation::default(),
            rt,
            session,
            toasts,
            on_play,
        });
        {
            let t = this.clone();
            play_all.connect_clicked(move |_| t.play_all());
        }
        this
    }

    /// Open `playlist_id`: clear the page, then fetch and list its videos. `name` is the
    /// title the card carried (the fetched title replaces it when the response has one).
    pub fn open(self: &Rc<Self>, playlist_id: String, name: String) {
        let token = self.generation.bump();
        self.title.set_text(if name.is_empty() { "Playlist" } else { &name });
        self.subtitle.set_text("");
        self.art.set_visible(false);
        self.art.set_paintable(gtk::gdk::Paintable::NONE);
        self.play_all.set_sensitive(false);
        self.store.remove_all();
        self.items.borrow_mut().clear();
        // Bounded to the playlist on screen, like the results grid's cache.
        self.thumb_cache.borrow_mut().clear();
        self.empty.set_text("This playlist is empty");
        self.empty.set_visible(false);
        self.spinner.set_visible(true);
        self.spinner.set_spinning(true);

        let current = self.session.borrow().clone();
        let this = self.clone();
        self.rt.spawn(
            async move { current.playlist(&playlist_id).await },
            move |res: Result<PlaylistContents, Error>| this.apply(token, res),
        );
    }

    /// Play the playlist from the top. No-op while it is empty (the button is insensitive
    /// then; the debug hook can still reach this).
    pub fn play_all(&self) {
        let snapshot = self.items.borrow().clone();
        if snapshot.is_empty() {
            return;
        }
        (self.on_play)(&snapshot, 0);
    }

    /// Render a fetched playlist, unless a newer `open` superseded this one.
    fn apply(self: &Rc<Self>, token: u64, res: Result<PlaylistContents, Error>) {
        if !self.generation.is_current(token) {
            return;
        }
        self.spinner.set_visible(false);
        self.spinner.set_spinning(false);
        let contents = match res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("[playlist] load failed: {e}");
                self.toasts.add_toast(Toast::new(&format!("Couldn't load playlist: {e}")));
                self.empty.set_text("Couldn't load this playlist");
                self.empty.set_visible(true);
                return;
            }
        };
        if let Some(t) = contents.title.as_deref().filter(|t| !t.is_empty()) {
            self.title.set_text(t);
        }
        let items: Vec<ResultItem> =
            contents.items.iter().filter_map(ResultItem::from_search_item).collect();
        // Marker that headless driver scenarios wait on before screenshotting.
        tracing::info!("[playlist] {} items", items.len());
        self.subtitle.set_text(&match items.len() {
            1 => "1 video".to_string(),
            n => format!("{n} videos"),
        });
        for it in &items {
            self.store.append(it);
        }
        self.play_all.set_sensitive(!items.is_empty());
        self.empty.set_visible(items.is_empty());
        // The contents response carries no playlist art, so the lead video's thumbnail is used.
        if let Some(first) = items.first() {
            // Keyed by video id, so the header and that video's row share one fetch. The header
            // Picture survives navigation, so its load is guarded; row Pictures are rebuilt per
            // bind.
            let (this, tok) = (self.clone(), token);
            paint_if(
                &self.art,
                first.thumbnail_url(),
                first.video_id(),
                &self.rt,
                &self.thumb_cache,
                move || this.generation.is_current(tok),
            );
            self.art.set_visible(true);
        }
        self.items.replace(items);
    }
}

/// One playlist row: position number, thumbnail with the duration or LIVE badge, then title
/// over the channel and view-count line. Rows are rebuilt per bind because the `ListView`
/// recycles them and the position number must follow.
fn build_row(index: u32, item: &ResultItem, rt: &AsyncRuntime, cache: &ThumbCache) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.set_margin_top(4);
    row.set_margin_bottom(4);
    row.set_margin_start(6);
    row.set_margin_end(6);

    let number = Label::builder()
        .label((index + 1).to_string())
        .width_chars(3)
        .xalign(1.0)
        .valign(Align::Center)
        .css_classes(["dim-label"])
        .build();
    row.append(&number);

    let thumb = Picture::builder().content_fit(gtk::ContentFit::Cover).build();
    thumb.set_size_request(160, 90);
    thumb.add_css_class("card");
    let badge = Label::builder()
        .halign(Align::End)
        .valign(Align::End)
        .margin_end(4)
        .margin_bottom(4)
        .visible(false)
        .build();
    if item.live() {
        badge.set_text("● LIVE");
        badge.add_css_class("live-badge");
        badge.set_visible(true);
    } else if let Some(d) = item.duration() {
        badge.set_text(&d);
        badge.add_css_class("duration-badge");
        badge.set_visible(true);
    }
    let thumb_overlay = Overlay::new();
    thumb_overlay.set_valign(Align::Center);
    thumb_overlay.set_child(Some(&thumb));
    thumb_overlay.add_overlay(&badge);
    row.append(&thumb_overlay);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_valign(Align::Center);
    text.set_hexpand(true);
    let title = Label::builder()
        .label(item.title())
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1) // let the row width, not the text, drive sizing
        .css_classes(["heading"])
        .build();
    let meta_text = [Some(item.channel()).filter(|c| !c.is_empty()), item.views(), item.published()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let meta = Label::builder()
        .label(&meta_text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .visible(!meta_text.is_empty())
        .css_classes(["dim-label", "caption"])
        .build();
    text.append(&title);
    text.append(&meta);
    row.append(&text);

    paint(&thumb, item.thumbnail_url(), item.video_id(), rt, cache);
    row
}

/// Cache-or-fetch `url` into `pic`, keyed by `key`. A row Picture is rebuilt per bind, so a
/// late paint lands on a detached widget and needs no guard. A Picture that outlives the load,
/// such as the page header's, uses [`paint_if`].
fn paint(pic: &Picture, url: Option<String>, key: String, rt: &AsyncRuntime, cache: &ThumbCache) {
    paint_if(pic, url, key, rt, cache, || true)
}

/// [`paint`] for a `Picture` that survives navigation: `still_current` is checked when the
/// bytes land, so a load started for one page cannot paint into the next.
fn paint_if(
    pic: &Picture,
    url: Option<String>,
    key: String,
    rt: &AsyncRuntime,
    cache: &ThumbCache,
    still_current: impl Fn() -> bool + 'static,
) {
    if let Some(texture) = cache.borrow().get(&key) {
        pic.set_paintable(Some(texture));
        return;
    }
    pic.set_paintable(gtk::gdk::Paintable::NONE);
    let Some(url) = url else { return };
    let pic = pic.clone();
    let cache = cache.clone();
    rt.spawn(
        async move { fetch_bytes(url).await },
        move |bytes: Option<Vec<u8>>| {
            let Some(bytes) = bytes else { return };
            let gbytes = gtk::glib::Bytes::from_owned(bytes);
            if let Ok(texture) = gtk::gdk::Texture::from_bytes(&gbytes) {
                // The bytes are keyed by video id, so they stay valid when the page has moved on.
                cache.borrow_mut().insert(key, texture.clone());
                if still_current() {
                    pic.set_paintable(Some(&texture));
                }
            }
        },
    );
}
