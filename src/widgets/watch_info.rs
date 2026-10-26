//! `WatchInfo`: the metadata block below the video on the watch page. It holds the title, a
//! channel row (avatar, name, subs, Subscribe), an action row (Like/Dislike/Share/Save) and an
//! expandable description. `set_details` fills it from a [`innertube::VideoDetails`]; the
//! `connect_*` methods let the caller (window::watch) attach backend calls, so the widget
//! never touches `Session`.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::{Align, Box as GtkBox, Button, Label, Orientation, Revealer, RevealerTransitionType};

use innertube::VideoDetails;

use crate::runtime::AsyncRuntime;

use super::result_row::fetch_bytes;

const AVATAR_PX: i32 = 40;
/// Lines shown before the description is truncated with "…more".
const COLLAPSED_LINES: i32 = 3;

type ClickCb = Rc<RefCell<Option<Box<dyn Fn()>>>>;
type LikeCb = Rc<RefCell<Option<Box<dyn Fn(bool)>>>>;

/// Abbreviate a raw count ("19,214,590", "2,071 views") to a compact form ("19M", "2K").
fn compact_num(n: u64) -> String {
    let (v, suf) = if n >= 1_000_000_000 {
        (n as f64 / 1e9, "B")
    } else if n >= 1_000_000 {
        (n as f64 / 1e6, "M")
    } else if n >= 1_000 {
        (n as f64 / 1e3, "K")
    } else {
        return n.to_string();
    };
    // One decimal, but drop a trailing ".0" (19.0M → 19M).
    if ((v * 10.0).round() as u64).is_multiple_of(10) {
        format!("{}{}", v as u64, suf)
    } else {
        format!("{v:.1}{suf}")
    }
}

/// Parse the digit run out of a raw count string → the number, if any.
fn parse_count(raw: Option<&str>) -> Option<u64> {
    let digits: String = raw?.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The description box's first line, "1.4M views  3d ago", from the raw view count and date.
fn description_meta(views: Option<&str>, published: Option<&str>) -> Option<String> {
    let views = parse_count(views).map(|n| format!("{} views", compact_num(n)));
    let parts: Vec<String> = [views, published.map(str::to_string)].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join("  "))
}

/// The Like button's label: the compact count, plus one when `liked`. Without a count it is
/// `👍` when liked and `👍 Like` otherwise; YouTube delivers the count on a separate, delayed
/// endpoint.
fn like_label(raw: Option<&str>, liked: bool) -> String {
    match parse_count(raw) {
        Some(n) => format!("\u{1F44D} {}", compact_num(n + liked as u64)),
        None => {
            if liked {
                "\u{1F44D}".to_string()
            } else {
                "\u{1F44D} Like".to_string()
            }
        }
    }
}

pub struct WatchInfo {
    pub root: GtkBox,
    rt: AsyncRuntime,

    title: Label,
    avatar: adw::Avatar,
    channel_name: Label,
    subs_label: Label,
    channel_click_area: GtkBox,
    subscribe_btn: Button,

    like_btn: Button,
    liked: Rc<Cell<bool>>,
    like_count: Rc<RefCell<Option<String>>>,
    dislike_btn: Button,
    disliked: Rc<Cell<bool>>,
    /// The Subscribe button's state. Only `set_subscribed` changes it, after the caller has the
    /// backend outcome; `is_subscribed` tells window::watch's handler which direction to call.
    subscribed: Rc<Cell<bool>>,

    description_revealer: Revealer,
    description_label: Label,
    description_meta: Label,
    description_toggle: Button,
    description_expanded: Rc<Cell<bool>>,

    /// The uploader's channel id, for the channel-click callback target. Shared with the
    /// channel-row click gesture registered in `new` (which reads it at click time).
    channel_id: Rc<RefCell<Option<String>>>,
    /// The video currently bound. Guards the async avatar fetch against a fast video switch
    /// landing a stale image (as `ResultRow`'s `key` does).
    video_id: Rc<RefCell<String>>,

    on_subscribe: ClickCb,
    on_like: LikeCb,
    on_dislike: LikeCb,
    on_share: ClickCb,
    on_save: ClickCb,
    on_channel: ClickCb,
}

impl WatchInfo {
    pub fn new(rt: AsyncRuntime) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 10);
        root.add_css_class("watch-info");
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let title = Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["title-2"])
            .build();
        root.append(&title);

        // Channel row: avatar + [name, subs] (clickable → connect_channel), Subscribe
        // pushed to the end.
        let channel_row = GtkBox::new(Orientation::Horizontal, 10);

        let avatar = adw::Avatar::new(AVATAR_PX, None, true);
        avatar.set_valign(Align::Center);

        let channel_name = Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let subs_label = Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .visible(false)
            .build();
        let name_col = GtkBox::new(Orientation::Vertical, 2);
        name_col.append(&channel_name);
        name_col.append(&subs_label);

        let channel_click_area = GtkBox::new(Orientation::Horizontal, 10);
        channel_click_area.append(&avatar);
        channel_click_area.append(&name_col);
        channel_click_area.set_hexpand(true);

        let subscribe_btn = Button::with_label("Subscribe");
        subscribe_btn.add_css_class("pill");
        subscribe_btn.add_css_class("suggested-action");
        subscribe_btn.set_valign(Align::Center);

        channel_row.append(&channel_click_area);
        channel_row.append(&subscribe_btn);
        root.append(&channel_row);

        // Action row: a segmented Like|Dislike pill with the compact count on Like, then Share
        // and Save pills.
        let action_row = GtkBox::new(Orientation::Horizontal, 8);
        action_row.set_halign(Align::Start);

        let like_btn = Button::with_label(&like_label(None, false));
        like_btn.add_css_class("flat");
        let dislike_btn = Button::with_label("\u{1F44E}");
        dislike_btn.set_tooltip_text(Some("Dislike"));
        dislike_btn.add_css_class("flat");
        let segmented = GtkBox::new(Orientation::Horizontal, 0);
        segmented.add_css_class("linked");
        segmented.add_css_class("watch-pill-group");
        segmented.set_valign(Align::Center);
        segmented.append(&like_btn);
        segmented.append(&dislike_btn);
        action_row.append(&segmented);

        let pill = |icon: &str, label: &str, tip: &str| {
            let content = adw::ButtonContent::builder().icon_name(icon).label(label).build();
            Button::builder().child(&content).tooltip_text(tip).css_classes(["watch-pill"]).build()
        };
        let share_btn = pill("emblem-shared-symbolic", "Share", "Copy link");
        let save_btn = pill("bookmark-new-symbolic", "Save", "Save to Watch Later");
        for b in [&share_btn, &save_btn] {
            b.set_valign(Align::Center);
            action_row.append(b);
        }
        root.append(&action_row);

        // Description: collapsed to a few lines by default, "…more" expands the full
        // text; the whole block (label + toggle) is hidden when there's no description.
        // WordChar: one unbroken token (a long URL) would otherwise set the column's width.
        let description_label = Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(COLLAPSED_LINES)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();
        let description_toggle = Button::with_label("…more");
        description_toggle.add_css_class("flat");
        description_toggle.set_halign(Align::Start);
        let description_meta = Label::builder().xalign(0.0).css_classes(["heading"]).visible(false).build();
        let description_block = GtkBox::new(Orientation::Vertical, 4);
        description_block.add_css_class("description-box");
        description_block.append(&description_meta);
        description_block.append(&description_label);
        description_block.append(&description_toggle);
        let description_revealer = Revealer::builder()
            .transition_type(RevealerTransitionType::SlideDown)
            .reveal_child(false)
            .child(&description_block)
            .build();
        root.append(&description_revealer);

        let this = Self {
            root,
            rt,
            title,
            avatar,
            channel_name,
            subs_label,
            channel_click_area,
            subscribe_btn: subscribe_btn.clone(),
            like_btn: like_btn.clone(),
            liked: Rc::new(Cell::new(false)),
            like_count: Rc::new(RefCell::new(None)),
            dislike_btn: dislike_btn.clone(),
            disliked: Rc::new(Cell::new(false)),
            subscribed: Rc::new(Cell::new(false)),
            description_revealer,
            description_label,
            description_meta,
            description_toggle: description_toggle.clone(),
            description_expanded: Rc::new(Cell::new(false)),
            channel_id: Rc::new(RefCell::new(None)),
            video_id: Rc::new(RefCell::new(String::new())),
            on_subscribe: Rc::new(RefCell::new(None)),
            on_like: Rc::new(RefCell::new(None)),
            on_dislike: Rc::new(RefCell::new(None)),
            on_share: Rc::new(RefCell::new(None)),
            on_save: Rc::new(RefCell::new(None)),
            on_channel: Rc::new(RefCell::new(None)),
        };

        subscribe_btn.connect_clicked({
            let cb = this.on_subscribe.clone();
            move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            }
        });

        // Like and Dislike are plain Buttons with manually tracked state. A click computes the
        // new state, updates the visuals the way `set_liked` does, and fires the callback once.
        // A ToggleButton's "toggled" would also fire when `set_liked` reverts programmatically,
        // looping back into the caller's handler.
        like_btn.connect_clicked({
            let this_liked = this.liked.clone();
            let disliked = this.disliked.clone();
            let like_btn = like_btn.clone();
            let dislike_btn = dislike_btn.clone();
            let count = this.like_count.clone();
            let cb = this.on_like.clone();
            move |_| {
                let new = !this_liked.get();
                this_liked.set(new);
                if new {
                    disliked.set(false);
                    dislike_btn.set_css_classes(&["flat"]);
                }
                apply_like_visual(&like_btn, new, count.borrow().as_deref());
                if let Some(f) = cb.borrow().as_ref() {
                    f(new);
                }
            }
        });

        dislike_btn.connect_clicked({
            let disliked = this.disliked.clone();
            let liked = this.liked.clone();
            let dislike_btn = dislike_btn.clone();
            let like_btn = like_btn.clone();
            let count = this.like_count.clone();
            let cb = this.on_dislike.clone();
            move |_| {
                let new = !disliked.get();
                disliked.set(new);
                // Same swap-don't-stack rule as `apply_like_visual`.
                dislike_btn.set_css_classes(if new { &["suggested-action"] } else { &["flat"] });
                if new {
                    liked.set(false);
                    apply_like_visual(&like_btn, false, count.borrow().as_deref());
                }
                if let Some(f) = cb.borrow().as_ref() {
                    f(new);
                }
            }
        });

        share_btn.connect_clicked({
            let cb = this.on_share.clone();
            move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            }
        });
        save_btn.connect_clicked({
            let cb = this.on_save.clone();
            move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            }
        });

        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed({
            let channel_id = this.channel_id.clone();
            let cb = this.on_channel.clone();
            move |g, _, _, _| {
                if channel_id.borrow().is_some() {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(f) = cb.borrow().as_ref() {
                        f();
                    }
                }
            }
        });
        this.channel_click_area.add_controller(gesture);

        description_toggle.connect_clicked({
            let expanded = this.description_expanded.clone();
            let label = this.description_label.clone();
            let btn = this.description_toggle.clone();
            move |_| {
                let now = !expanded.get();
                expanded.set(now);
                apply_description_state(&label, &btn, now);
            }
        });

        this
    }

    /// Fills every field from `d`. A missing field hides its row or label.
    pub fn set_details(&self, d: &VideoDetails, video_id: &str) {
        *self.video_id.borrow_mut() = video_id.to_string();
        *self.channel_id.borrow_mut() = d.channel_id.clone();
        self.channel_click_area
            .set_cursor_from_name(d.channel_id.as_deref().map(|_| "pointer"));

        self.title.set_text(d.title.as_deref().unwrap_or(""));
        self.title.set_visible(d.title.is_some());

        self.channel_name.set_text(d.channel.as_deref().unwrap_or(""));
        self.channel_name.set_visible(d.channel.is_some());

        match &d.subscriber_count {
            Some(s) if !s.is_empty() => {
                self.subs_label.set_text(s);
                self.subs_label.set_visible(true);
            }
            _ => self.subs_label.set_visible(false),
        }

        self.set_subscribed(d.subscribed);

        // Avatar: clear at once so the previous channel's avatar does not show while the new
        // one loads, then fetch. The video_id guard stops a fast video switch from painting a
        // stale avatar.
        self.avatar.set_custom_image(gtk::gdk::Paintable::NONE);
        if let Some(url) = d.channel_avatar.clone() {
            let key = video_id.to_string();
            let current = self.video_id.clone();
            let avatar = self.avatar.clone();
            self.rt.spawn(
                async move { fetch_bytes(url).await },
                move |bytes: Option<Vec<u8>>| {
                    let Some(bytes) = bytes else { return };
                    let gbytes = gtk::glib::Bytes::from_owned(bytes);
                    if let Ok(texture) = gtk::gdk::Texture::from_bytes(&gbytes) {
                        if *current.borrow() == key {
                            avatar.set_custom_image(Some(&texture));
                        }
                    }
                },
            );
        }

        *self.like_count.borrow_mut() = d.like_count.clone();
        self.liked.set(false);
        self.disliked.set(false);
        self.dislike_btn.set_css_classes(&["flat"]);
        apply_like_visual(&self.like_btn, false, d.like_count.as_deref());

        let meta = description_meta(d.view_count.as_deref(), d.published.as_deref());
        self.description_meta.set_text(meta.as_deref().unwrap_or(""));
        self.description_meta.set_visible(meta.is_some());
        match &d.description {
            Some(text) if !text.is_empty() => {
                self.description_label.set_text(text);
                self.description_expanded.set(false);
                apply_description_state(&self.description_label, &self.description_toggle, false);
                // Reveal "…more" only if the collapsed label is truncated.
                crate::widgets::reveal_when_truncated(
                    &self.description_label,
                    self.description_toggle.upcast_ref(),
                );
                self.description_revealer.set_reveal_child(true);
            }
            _ => {
                self.description_label.set_text("");
                self.description_toggle.set_visible(false);
                self.description_revealer.set_reveal_child(meta.is_some());
            }
        }
    }

    /// Shows the subscribe state after a toggle, or reverts it on failure. Does not call the
    /// backend.
    pub fn set_subscribed(&self, on: bool) {
        self.subscribed.set(on);
        self.subscribe_btn.set_label(if on { "Subscribed" } else { "Subscribe" });
        if on {
            self.subscribe_btn.remove_css_class("suggested-action");
        } else {
            self.subscribe_btn.add_css_class("suggested-action");
        }
    }

    /// The video id bound by the last `set_details` call (empty if none bound yet, or
    /// after `clear()`). Read by window::watch's action handlers to know which video to act on.
    pub fn video_id(&self) -> String {
        self.video_id.borrow().clone()
    }

    /// The title shown, empty before `set_details` and after `clear()`.
    pub fn title(&self) -> String {
        self.title.text().to_string()
    }

    /// The uploader's channel id bound by the last `set_details` call, if known.
    pub fn channel_id(&self) -> Option<String> {
        self.channel_id.borrow().clone()
    }

    /// The channel display name currently shown (for opening the channel page with a
    /// non-empty title before its own fetch resolves).
    pub fn channel_name(&self) -> String {
        self.channel_name.text().to_string()
    }

    /// Current Subscribe-button state, as last set by `set_details`/`set_subscribed`.
    pub fn is_subscribed(&self) -> bool {
        self.subscribed.get()
    }

    /// Blanks every field to its state before `set_details`. Call it on a video switch, before
    /// the fetch lands, so the previous video's metadata does not stay on screen.
    pub fn clear(&self) {
        *self.video_id.borrow_mut() = String::new();
        *self.channel_id.borrow_mut() = None;
        self.channel_click_area.set_cursor_from_name(None);

        self.title.set_text("");
        self.title.set_visible(false);

        self.channel_name.set_text("");
        self.channel_name.set_visible(false);

        self.subs_label.set_visible(false);
        self.avatar.set_custom_image(gtk::gdk::Paintable::NONE);

        self.set_subscribed(false);

        *self.like_count.borrow_mut() = None;
        self.liked.set(false);
        self.disliked.set(false);
        self.dislike_btn.set_css_classes(&["flat"]);
        apply_like_visual(&self.like_btn, false, None);

        self.description_label.set_text("");
        self.description_expanded.set(false);
        apply_description_state(&self.description_label, &self.description_toggle, false);
        self.description_revealer.set_reveal_child(false);
    }

    /// Reflect a like outcome (see `set_subscribed`). Does not fire `connect_like`.
    pub fn set_liked(&self, on: bool) {
        self.liked.set(on);
        if on {
            self.disliked.set(false);
            self.dislike_btn.set_css_classes(&["flat"]);
        }
        apply_like_visual(&self.like_btn, on, self.like_count.borrow().as_deref());
    }

    pub fn connect_subscribe<F: Fn() + 'static>(&self, f: F) {
        *self.on_subscribe.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_like<F: Fn(bool) + 'static>(&self, f: F) {
        *self.on_like.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_dislike<F: Fn(bool) + 'static>(&self, f: F) {
        *self.on_dislike.borrow_mut() = Some(Box::new(f));
    }

    /// Show the dislike state without firing the callback, to revert a failed write.
    pub fn set_disliked(&self, on: bool) {
        self.disliked.set(on);
        self.dislike_btn.set_css_classes(if on { &["suggested-action"] } else { &["flat"] });
    }

    pub fn connect_share<F: Fn() + 'static>(&self, f: F) {
        *self.on_share.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_save<F: Fn() + 'static>(&self, f: F) {
        *self.on_save.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_channel<F: Fn() + 'static>(&self, f: F) {
        *self.on_channel.borrow_mut() = Some(Box::new(f));
    }
}

/// Update the Like button's label + highlight for `on`, keeping the "no count yet"
/// wording rule (`like_label`) regardless of toggle state.
fn apply_like_visual(btn: &Button, on: bool, count: Option<&str>) {
    btn.set_label(&like_label(count, on));
    // Swap the classes instead of stacking them. In the Adwaita stylesheet `button.flat`
    // (`background: transparent`) comes after `button.suggested-action`
    // (`background-color: …`) at equal specificity, so with both set `.flat` wins and the
    // accent never shows.
    btn.set_css_classes(if on { &["suggested-action"] } else { &["flat"] });
}

/// Swap the description label's truncation + toggle button wording between collapsed
/// (a few lines, ellipsized, "…more") and expanded (full text, "Show less").
fn apply_description_state(label: &Label, toggle: &Button, expanded: bool) {
    if expanded {
        label.set_lines(-1);
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        toggle.set_label("Show less");
    } else {
        label.set_lines(COLLAPSED_LINES);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        toggle.set_label("…more");
    }
}
