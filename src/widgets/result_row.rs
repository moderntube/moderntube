//! One search result card: a 16:9 thumbnail (async-loaded) above the title + channel.
//! A `gtk::Box` subclass with a `bind` method, so the GridView factory stays free of
//! widget-tree walking or unsafe data stashing.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, EventControllerMotion, Label, MenuButton, Orientation, Overlay,
    Picture, Popover,
};

use crate::models::ResultItem;
use crate::playback::QueueAction;
use crate::runtime::AsyncRuntime;

/// Shared thumbnail cache (video id → texture). A recycled GridView card repaints a seen
/// thumbnail at once instead of clearing and re-fetching, and redundant fetches are skipped.
pub type ThumbCache = Rc<RefCell<HashMap<String, gtk::gdk::Texture>>>;

mod imp {
    use super::*;

    pub struct ResultRow {
        pub thumb: Picture,
        /// The overlay over the thumbnail: the badge, and the hover preview's video surface,
        /// which fills the thumbnail in place.
        pub thumb_overlay: Overlay,
        /// Duration ("12:34") or "● LIVE", overlaid on the thumbnail's bottom-right.
        pub badge: Label,
        pub title: Label,
        /// The uploader's round avatar beside the text, as on the web client's cards.
        pub avatar: gtk::Image,
        pub avatar_key: RefCell<String>,
        pub channel: Label,
        /// "1.2M views · 2 days ago".
        pub meta: Label,
        /// The video id this card shows. Guards the async thumbnail paint against the card
        /// being recycled to another item mid-fetch.
        pub key: RefCell<String>,
        /// Sidebar layout: thumbnail left, text right, no avatar.
        pub compact: std::cell::Cell<bool>,
        /// The uploader's channel id for the currently-bound item (for click-through).
        pub channel_id: RefCell<Option<String>>,
        /// ⋮ overlay button (Play next / Add to queue), top-right of the thumbnail.
        pub menu: MenuButton,
        /// The item bound to this recycled card; what the ⋮ menu queues.
        pub bound: RefCell<Option<ResultItem>>,
    }

    impl Default for ResultRow {
        fn default() -> Self {
            Self {
                // Picture (not Image) scales the texture to fill; Cover crops to 16:9.
                thumb: Picture::builder().content_fit(gtk::ContentFit::Cover).build(),
                thumb_overlay: Overlay::new(),
                badge: Label::builder()
                    .halign(Align::End)
                    .valign(Align::End)
                    .margin_end(6)
                    .margin_bottom(6)
                    .css_classes(["duration-badge"])
                    .visible(false)
                    .build(),
                title: Label::builder()
                    .xalign(0.0)
                    .wrap(true)
                    .lines(2)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(1) // let the card width, not the text, drive sizing
                    .css_classes(["heading"])
                    .build(),
                avatar: gtk::Image::builder()
                    .pixel_size(36)
                    .overflow(gtk::Overflow::Hidden)
                    .valign(Align::Start)
                    .margin_top(2)
                    .css_classes(["card-avatar"])
                    .visible(false)
                    .build(),
                avatar_key: RefCell::new(String::new()),
                channel: Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(1)
                    .css_classes(["dim-label", "caption"])
                    .build(),
                meta: Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(1)
                    .css_classes(["dim-label", "caption"])
                    .build(),
                key: RefCell::new(String::new()),
                compact: std::cell::Cell::new(false),
                channel_id: RefCell::new(None),
                menu: MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .valign(Align::Center)
                    .tooltip_text("Add to queue")
                    .css_classes(["queue-menu", "flat"])
                    .build(),
                bound: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ResultRow {
        const NAME: &'static str = "MtResultRow";
        type Type = super::ResultRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ResultRow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(Orientation::Vertical);
            obj.set_spacing(6);
            obj.set_margin_top(6);
            obj.set_margin_bottom(6);
            obj.set_margin_start(6);
            obj.set_margin_end(6);
            // 16:9 thumbnail filling the card width; rounded corners via CSS class.
            self.thumb.set_size_request(320, 180);
            self.thumb.set_hexpand(true);
            self.thumb.add_css_class("card");
            // Duration / LIVE badge overlaid on the thumbnail.
            self.thumb_overlay.set_child(Some(&self.thumb));
            self.thumb_overlay.add_overlay(&self.badge);
            let text = gtk::Box::new(Orientation::Vertical, 2);
            text.append(&self.title);
            text.append(&self.channel);
            // Bottom row: meta line + the ⋮ queue menu pushed to the free bottom-right
            // (off the thumbnail, which is the video itself).
            let meta_row = gtk::Box::new(Orientation::Horizontal, 4);
            self.meta.set_hexpand(true);
            meta_row.append(&self.meta);
            meta_row.append(&self.menu);
            text.append(&meta_row);
            text.set_hexpand(true);
            let info = gtk::Box::new(Orientation::Horizontal, 12);
            info.append(&self.avatar);
            info.append(&text);
            obj.append(&self.thumb_overlay);
            obj.append(&info);
        }
    }
    impl WidgetImpl for ResultRow {}
    impl BoxImpl for ResultRow {}
}

glib::wrapper! {
    pub struct ResultRow(ObjectSubclass<imp::ResultRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for ResultRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ResultRow {
    /// Fires `f(channel_id, channel_name)` when the channel label or avatar is clicked. The click
    /// is claimed so the card's own activate does not also fire. Wired once per recycled card;
    /// it reads the bound item at click time.
    pub fn connect_channel_clicked<F: Fn(String, String) + 'static>(&self, f: F) {
        let f = Rc::new(f);
        for target in [self.imp().channel.upcast_ref::<gtk::Widget>(), self.imp().avatar.upcast_ref()] {
            let gesture = gtk::GestureClick::new();
            let (obj, f) = (self.clone(), f.clone());
            gesture.connect_pressed(move |g, _, _, _| {
                let imp = obj.imp();
                if let Some(id) = imp.channel_id.borrow().clone() {
                    g.set_state(gtk::EventSequenceState::Claimed); // don't bubble → play
                    f(id, imp.channel.text().to_string());
                }
            });
            target.add_controller(gesture);
        }
    }

    /// Wire the ⋮ menu's actions. `f(item, action)` fires with the card's currently-bound
    /// item when "Play next" / "Add to queue" is chosen. Wired once per recycled card (in
    /// the factory setup); reads the bound item at click time.
    pub fn connect_queue<F: Fn(ResultItem, QueueAction) + 'static>(&self, f: F) {
        let f = Rc::new(f);
        let popover = Popover::builder().css_classes(["menu"]).build();
        let list = GtkBox::new(Orientation::Vertical, 0);
        for (label, action) in
            [("Play next", QueueAction::PlayNext), ("Add to queue", QueueAction::AddToQueue)]
        {
            let btn = Button::builder().label(label).has_frame(false).build();
            let obj = self.clone();
            let f = f.clone();
            let popover = popover.clone();
            btn.connect_clicked(move |_| {
                popover.popdown();
                if let Some(item) = obj.imp().bound.borrow().clone() {
                    f(item, action);
                }
            });
            list.append(&btn);
        }
        popover.set_child(Some(&list));
        self.imp().menu.set_popover(Some(&popover));
    }

    /// Switches to the sidebar layout; call once, before the first bind.
    pub fn set_compact(&self) {
        let imp = self.imp();
        imp.compact.set(true);
        self.set_orientation(Orientation::Horizontal);
        self.set_spacing(8);
        imp.thumb.set_hexpand(false);
        imp.thumb_overlay.set_valign(Align::Start);
        imp.avatar.set_visible(false);
    }

    fn thumb_width(&self) -> i32 {
        if self.imp().compact.get() { 168 } else { 320 }
    }

    /// The item this card shows.
    pub fn item(&self) -> Option<ResultItem> {
        self.imp().bound.borrow().clone()
    }

    /// The thumbnail widget (the overlay wrapping the 16:9 `Picture` + badge). The shared
    /// hover preview floats over this rectangle in the top-level stage overlay; the caller
    /// uses its bounds to position the preview surface.
    pub fn thumbnail_widget(&self) -> gtk::Widget {
        self.imp().thumb_overlay.clone().upcast()
    }

    /// Wires hover-preview triggers on this recycled card. `on_hover(item)` fires after a steady
    /// `debounce` hover, so a passing pointer starts no stream, and reads the item bound at fire
    /// time. `on_leave(video_id)` fires at once on pointer-leave with the id bound then.
    /// `on_enter(video_id)` fires on every pointer-enter; the preview uses it to cancel its
    /// pending teardown, because the preview surface is a stage-overlay sibling and crossing onto
    /// it leaves the card.
    pub fn connect_hover_preview<F, G, H>(
        &self,
        debounce: Duration,
        on_hover: F,
        on_leave: G,
        on_enter: H,
    ) where
        F: Fn(ResultItem) + 'static,
        G: Fn(String) + 'static,
        H: Fn(String) + 'static,
    {
        let on_hover = Rc::new(on_hover);
        let motion = EventControllerMotion::new();
        let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let obj = self.clone();
        let timer_enter = timer.clone();
        motion.connect_enter(move |_, _, _| {
            on_enter(obj.imp().key.borrow().clone());
            // Cancel any still-PENDING debounce from a prior enter (safe: it hasn't fired,
            // so the source is still live). A fired timer already cleared itself below.
            if let Some(id) = timer_enter.borrow_mut().take() {
                id.remove();
            }
            let obj = obj.clone();
            let on_hover = on_hover.clone();
            let timer_fire = timer_enter.clone();
            let source = glib::timeout_add_local_once(debounce, move || {
                // The one-shot source removes itself when it fires. Drop the stored handle so a
                // later leave or enter never calls `.remove()` on a dead source, which panics.
                timer_fire.borrow_mut().take();
                if let Some(item) = obj.imp().bound.borrow().clone() {
                    on_hover(item);
                }
            });
            *timer_enter.borrow_mut() = Some(source);
        });
        let obj = self.clone();
        motion.connect_leave(move |_| {
            // Only removes a still-pending debounce; a fired one already cleared itself.
            if let Some(id) = timer.borrow_mut().take() {
                id.remove();
            }
            on_leave(obj.imp().key.borrow().clone());
        });
        self.add_controller(motion);
    }

    pub fn bind(&self, item: &ResultItem, rt: &AsyncRuntime, cache: &ThumbCache) {
        self.imp().bound.replace(Some(item.clone()));
        let imp = self.imp();

        // Channel entity card: round avatar, name and subscriber count, with no duration, queue
        // menu or meta. The thumbnail Picture doubles as the avatar (`channel-avatar` class) and
        // the card opens the channel page (routed in results_view). Recycled cards reset these.
        if item.is_channel() {
            imp.channel_id.replace(item.channel_id());
            imp.title.set_text(&item.title());
            imp.channel.set_text(item.subscribers().as_deref().unwrap_or(""));
            imp.channel.set_visible(item.subscribers().is_some());
            imp.channel.set_cursor_from_name(None);
            imp.meta.set_visible(false);
            imp.badge.set_visible(false);
            imp.menu.set_visible(false);
            imp.thumb.set_size_request(160, 160);
            imp.thumb.remove_css_class("card");
            imp.thumb.add_css_class("channel-avatar");
            imp.avatar.set_visible(false);
            let key = format!("ch:{}", item.channel_id().unwrap_or_default());
            imp.key.replace(key.clone());
            paint_thumb(self, &imp.thumb, item.avatar_url(), key, rt, cache);
            return;
        }
        // Playlist card: title and a 'Playlist' badge; the card opens the ordered playlist view.
        // A playlist search result has no per-video thumbnail, so the badge labels the
        // placeholder.
        if item.is_playlist() {
            imp.title.set_text(&item.title());
            imp.channel.set_visible(false);
            imp.meta.set_visible(false);
            imp.menu.set_visible(false);
            imp.badge.set_text("☰ Playlist");
            imp.badge.remove_css_class("live-badge");
            imp.badge.add_css_class("duration-badge");
            imp.badge.set_visible(true);
            imp.thumb.set_size_request(self.thumb_width(), self.thumb_width() * 9 / 16);
            imp.thumb.remove_css_class("channel-avatar");
            imp.thumb.add_css_class("card");
            imp.avatar.set_visible(false);
            let key = format!("pl:{}", item.playlist_id());
            imp.key.replace(key.clone());
            // The lead video's thumbnail (None → placeholder).
            paint_thumb(self, &imp.thumb, item.thumbnail_url(), key, rt, cache);
            return;
        }
        // A video card recycled from a channel/playlist card must restore the video look.
        imp.menu.set_visible(true);
        imp.thumb.set_size_request(self.thumb_width(), self.thumb_width() * 9 / 16);
        imp.thumb.remove_css_class("channel-avatar");
        imp.thumb.add_css_class("card");

        let key = item.video_id();
        imp.key.replace(key.clone());
        imp.title.set_text(&item.title());
        imp.channel.set_text(&item.channel());
        imp.channel.set_visible(!item.channel().is_empty());
        // A clickable (pointer cursor) channel name only when we know its id.
        let chid = item.channel_id();
        imp.channel.set_cursor_from_name(chid.as_deref().map(|_| "pointer"));
        imp.channel_id.replace(chid.clone());
        imp.avatar.set_cursor_from_name(chid.as_deref().map(|_| "pointer"));
        paint_avatar(self, item.channel_avatar(), rt, cache);

        // Meta line: "views · published" (whichever are present).
        let meta = [item.views(), item.published()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        imp.meta.set_text(&meta);
        imp.meta.set_visible(!meta.is_empty());

        // Duration / LIVE badge.
        if item.live() {
            imp.badge.set_text("● LIVE");
            imp.badge.remove_css_class("duration-badge");
            imp.badge.add_css_class("live-badge");
            imp.badge.set_visible(true);
        } else if let Some(d) = item.duration() {
            imp.badge.set_text(&d);
            imp.badge.remove_css_class("live-badge");
            imp.badge.add_css_class("duration-badge");
            imp.badge.set_visible(true);
        } else {
            imp.badge.set_visible(false);
        }

        paint_thumb(self, &imp.thumb, item.thumbnail_url(), key, rt, cache);
    }
}

/// Cache-or-fetch `url` into `thumb`, keyed by `key`; the async paint only lands if `row` still
/// shows `key` (the GridView may recycle it mid-fetch). A `None` url clears the picture. Used by
/// the video thumbnail and the channel avatar.
fn paint_thumb(
    row: &ResultRow,
    thumb: &Picture,
    url: Option<String>,
    key: String,
    rt: &AsyncRuntime,
    cache: &ThumbCache,
) {
    if let Some(texture) = cache.borrow().get(&key) {
        thumb.set_paintable(Some(texture));
        return;
    }
    thumb.set_paintable(gtk::gdk::Paintable::NONE);
    let Some(url) = url else { return };
    let thumb = thumb.clone();
    let cache = cache.clone();
    let obj = row.clone();
    rt.spawn(
        async move { fetch_bytes(url).await },
        move |bytes: Option<Vec<u8>>| {
            if let Some(bytes) = bytes {
                let gbytes = glib::Bytes::from_owned(bytes);
                if let Ok(texture) = gtk::gdk::Texture::from_bytes(&gbytes) {
                    cache.borrow_mut().insert(key.clone(), texture.clone());
                    if *obj.imp().key.borrow() == key {
                        thumb.set_paintable(Some(&texture));
                    }
                }
            }
        },
    );
}

/// Cache-or-fetch the card's channel avatar; hidden when the item has none.
fn paint_avatar(row: &ResultRow, url: Option<String>, rt: &AsyncRuntime, cache: &ThumbCache) {
    let imp = row.imp();
    imp.avatar.set_visible(url.is_some() && !imp.compact.get());
    let Some(url) = url else { return };
    let key = format!("av:{url}");
    imp.avatar_key.replace(key.clone());
    if let Some(texture) = cache.borrow().get(&key) {
        imp.avatar.set_paintable(Some(texture));
        return;
    }
    imp.avatar.clear();
    let (cache, obj) = (cache.clone(), row.clone());
    rt.spawn(async move { fetch_bytes(url).await }, move |bytes: Option<Vec<u8>>| {
        let Some(bytes) = bytes else { return };
        let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) else { return };
        cache.borrow_mut().insert(key.clone(), texture.clone());
        if *obj.imp().avatar_key.borrow() == key {
            obj.imp().avatar.set_paintable(Some(&texture));
        }
    });
}

pub(crate) async fn fetch_bytes(url: String) -> Option<Vec<u8>> {
    let resp = reqwest::get(&url).await.ok()?;
    Some(resp.bytes().await.ok()?.to_vec())
}
