//! Live-chat sidebar. One read-only `TextView` holds the whole chat, so wrapping text, inline
//! emoji images, role-colored names, author photos and membership badges mix through child
//! anchors. Incoming batches drip onto the view on a timer (DRIP_MS per message, faster when
//! backed up) so chat scrolls smoothly, as in the web client. Photos and emoji images load async
//! and are cached.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use gtk4 as gtk;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, Entry, Label, Orientation, Picture, PolicyType, ScrolledWindow,
    TextBuffer, TextView, WrapMode,
};

use innertube::{ChatBadge, ChatKind, ChatMessage, ChatRun};

use crate::runtime::AsyncRuntime;

const MAX_LINES: i32 = 220; // cap the rendered backlog (chat is unbounded)
const EMOJI_PX: i32 = 20;
const PHOTO_PX: i32 = 22;
const BADGE_PX: i32 = 16;
const DRIP_MS: u64 = 80; // ~12.5 msgs/sec target, faster when the queue backs up

type ImageCache = Rc<RefCell<HashMap<String, gdk::Texture>>>;
type SendCb = Rc<RefCell<Option<Rc<dyn Fn(String)>>>>;
type ModeCb = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// How the chat panel is presented. The header button cycles the modes; `App::place_chat`
/// reparents and positions the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatMode {
    /// Docked in the resizable side panel (default).
    Docked,
    /// Translucent panel pinned over the right edge of the video.
    Overlay,
    /// A draggable, translucent in-window panel floating over the video (move by its header).
    Floating,
}

impl ChatMode {
    /// The next mode in the cycle Docked → Overlay → Floating → Docked.
    pub fn next(self) -> Self {
        match self {
            ChatMode::Docked => ChatMode::Overlay,
            ChatMode::Overlay => ChatMode::Floating,
            ChatMode::Floating => ChatMode::Docked,
        }
    }
    fn icon(self) -> &'static str {
        match self {
            ChatMode::Docked => "sidebar-show-right-symbolic",
            ChatMode::Overlay => "video-display-symbolic",
            ChatMode::Floating => "window-restore-symbolic",
        }
    }
    fn tooltip(self) -> &'static str {
        match self {
            ChatMode::Docked => "Chat: docked (click to overlay)",
            ChatMode::Overlay => "Chat: overlay (click to float)",
            ChatMode::Floating => "Chat: floating (click to dock)",
        }
    }
}

pub struct LiveChatView {
    pub root: GtkBox,
    /// The header row (title + mode button). `window::player_page` attaches a drag gesture here in
    /// floating mode so the panel can be moved by its header.
    pub header: GtkBox,
    view: TextView,
    buffer: TextBuffer,
    /// The compose row (entry + send); hidden for replay chat (can't post to a VOD).
    compose: GtkBox,
    entry: Entry,
    send: Button,
    mode_btn: Button,
    send_cb: SendCb,
    mode_cb: ModeCb,
    rt: AsyncRuntime,
    photos: ImageCache,
    emojis: ImageCache,
    queue: Rc<RefCell<VecDeque<ChatMessage>>>,
    draining: Rc<Cell<bool>>,
    /// Message ids already rendered, so the optimistic echo and the poll's later copy show once.
    seen: RefCell<HashSet<String>>,
}

impl LiveChatView {
    pub fn new(rt: AsyncRuntime) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.set_width_request(360);
        root.add_css_class("background");

        // Header row: title and the mode button. It is also the drag handle in floating mode.
        let header = GtkBox::new(Orientation::Horizontal, 6);
        header.set_margin_top(6);
        header.set_margin_bottom(4);
        header.set_margin_start(12);
        header.set_margin_end(6);
        let title = Label::new(Some("Live chat"));
        title.add_css_class("heading");
        title.set_halign(Align::Start);
        title.set_hexpand(true);
        title.set_xalign(0.0);
        header.append(&title);
        let mode_btn = Button::from_icon_name(ChatMode::Docked.icon());
        mode_btn.add_css_class("flat");
        mode_btn.set_tooltip_text(Some(ChatMode::Docked.tooltip()));
        mode_btn.set_valign(Align::Center);
        header.append(&mode_btn);
        root.append(&header);

        let view = TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(WrapMode::WordChar);
        view.set_left_margin(8);
        view.set_right_margin(8);
        view.set_pixels_below_lines(4);
        let buffer = view.buffer();

        // Role/name color tags (YouTube-ish), all bold.
        let mk = |name: &str, color: &str| {
            buffer.create_tag(Some(name), &[("foreground", &color), ("weight", &700)]);
        };
        mk("name", "#9aa0a6"); // normal author: gray
        mk("owner", "#ffd24c"); // channel owner: gold
        mk("mod", "#5e84f1"); // moderator: blue
        mk("member", "#2ba640"); // member: green
        buffer.create_tag(Some("amount"), &[("foreground", &"#ffd24c"), ("weight", &700)]);

        let scroller = ScrolledWindow::new();
        scroller.set_policy(PolicyType::Never, PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&view));
        root.append(&scroller);

        let compose = GtkBox::new(Orientation::Horizontal, 6);
        compose.set_margin_top(6);
        compose.set_margin_bottom(8);
        compose.set_margin_start(8);
        compose.set_margin_end(8);
        let entry = Entry::new();
        entry.set_hexpand(true);
        entry.set_placeholder_text(Some("Chat…"));
        super::enable_emoji_shortcodes(&entry);
        let send = Button::from_icon_name("document-send-symbolic");
        send.set_tooltip_text(Some("Send"));
        compose.append(&entry);
        compose.append(&send);
        root.append(&compose);

        let send_cb: SendCb = Rc::new(RefCell::new(None));
        {
            let entry_c = entry.clone();
            let cb = send_cb.clone();
            let fire = move || {
                let text = entry_c.text().to_string().trim().to_string();
                if text.is_empty() {
                    return;
                }
                if let Some(f) = cb.borrow().clone() {
                    f(text);
                    entry_c.set_text("");
                }
            };
            let f1 = fire.clone();
            entry.connect_activate(move |_| f1());
            send.connect_clicked(move |_| fire());
        }

        let mode_cb: ModeCb = Rc::new(RefCell::new(None));
        {
            let cb = mode_cb.clone();
            mode_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().clone() {
                    f();
                }
            });
        }

        Self {
            root,
            header,
            view,
            buffer,
            compose,
            entry,
            send,
            mode_btn,
            send_cb,
            mode_cb,
            rt,
            photos: Rc::new(RefCell::new(HashMap::new())),
            emojis: Rc::new(RefCell::new(HashMap::new())),
            queue: Rc::new(RefCell::new(VecDeque::new())),
            draining: Rc::new(Cell::new(false)),
            seen: RefCell::new(HashSet::new()),
        }
    }

    /// Renders the user's own just-sent message at once. The poll's later copy is dropped by
    /// id.
    pub fn push_own(&self, msg: &ChatMessage) {
        self.append_row(msg);
    }

    /// Put an unsent message back in the compose box after a failed send, unless the user
    /// has already started typing something new.
    pub fn restore_unsent(&self, text: &str) {
        if self.entry.text().is_empty() {
            self.entry.set_text(text);
        }
    }

    /// Queue a freshly-polled batch; drip it onto the view on a timer (smooth scroll).
    pub fn enqueue(self: &Rc<Self>, msgs: Vec<ChatMessage>) {
        if msgs.is_empty() {
            return;
        }
        self.queue.borrow_mut().extend(msgs);
        if !self.draining.replace(true) {
            self.schedule_drip();
        }
    }

    fn schedule_drip(self: &Rc<Self>) {
        let this = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(DRIP_MS), move || {
            let n = {
                let q = this.queue.borrow();
                // Emit 1 at a time when calm; drain a backlog within ~1.3s (16 ticks).
                (q.len() as f64 / 16.0).ceil().max(1.0) as usize
            };
            for _ in 0..n {
                let Some(msg) = this.queue.borrow_mut().pop_front() else { break };
                this.append_row(&msg);
            }
            if this.queue.borrow().is_empty() {
                this.draining.set(false);
            } else {
                this.schedule_drip();
            }
        });
    }

    /// Render one message at the end of the buffer (photo, badges, colored name, body
    /// runs with inline emoji), then trim + autoscroll.
    fn append_row(&self, msg: &ChatMessage) {
        // Dedup by id (optimistic echo vs poll copy / re-sends). Cap the set so it can't
        // grow unbounded over a long session.
        if !msg.id.is_empty() {
            let mut seen = self.seen.borrow_mut();
            if !seen.insert(msg.id.clone()) {
                return;
            }
            if seen.len() > 4000 {
                seen.clear();
            }
        }
        let buf = &self.buffer;
        let insert_text = |s: &str| {
            let mut end = buf.end_iter();
            buf.insert(&mut end, s);
        };
        let insert_image = |url: &str, px: i32, cache: &ImageCache| {
            let mut end = buf.end_iter();
            let anchor = buf.create_child_anchor(&mut end);
            let pic = Picture::new();
            pic.set_size_request(px, px);
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_valign(Align::Center);
            self.view.add_child_at_anchor(&pic, &anchor);
            let p = pic.clone();
            load_image(&self.rt, cache, url.to_string(), move |t| p.set_paintable(Some(t)));
        };

        // Author photo: circular avatar, initials until the image loads or when there is none.
        {
            let mut end = buf.end_iter();
            let anchor = buf.create_child_anchor(&mut end);
            let avatar = adw::Avatar::new(PHOTO_PX, Some(&msg.author), true);
            avatar.set_valign(Align::Center);
            self.view.add_child_at_anchor(&avatar, &anchor);
            if let Some(url) = &msg.author_photo_url {
                let av = avatar.clone();
                load_image(&self.rt, &self.photos, url.clone(), move |t| {
                    av.set_custom_image(Some(t))
                });
            }
            insert_text(" ");
        }
        // Membership badge image (if any).
        for b in &msg.badges {
            if let ChatBadge::Member { image_url, .. } = b {
                insert_image(image_url, BADGE_PX, &self.emojis);
                insert_text(" ");
            }
        }
        // Author name, role-colored, with a trailing ✓ for verified.
        let tag = name_tag(&msg.badges);
        let verified = msg.badges.iter().any(|b| matches!(b, ChatBadge::Verified));
        let name = format!("{}{}", msg.author, if verified { " ✓" } else { "" });
        let start = buf.end_iter().offset();
        insert_text(&name);
        let s = buf.iter_at_offset(start);
        let e = buf.end_iter();
        buf.apply_tag_by_name(tag, &s, &e);

        // Super Chat amount badge.
        if let ChatKind::SuperChat { amount, .. } = &msg.kind {
            if !amount.is_empty() {
                let off = buf.end_iter().offset();
                insert_text(&format!("  {amount}"));
                let s = buf.iter_at_offset(off);
                let e = buf.end_iter();
                buf.apply_tag_by_name("amount", &s, &e);
            }
        }
        insert_text("  ");

        // Body runs (membership shows its header text).
        match &msg.kind {
            ChatKind::Membership { header } if !header.is_empty() => insert_text(header),
            _ => {
                for run in &msg.runs {
                    match run {
                        ChatRun::Text(t) | ChatRun::Link { text: t, .. } => insert_text(t),
                        ChatRun::Emoji { url, .. } => insert_image(url, EMOJI_PX, &self.emojis),
                    }
                }
            }
        }
        insert_text("\n");

        // Trim old lines.
        let lines = buf.line_count();
        if lines > MAX_LINES {
            let mut start = buf.start_iter();
            if let Some(mut to) = buf.iter_at_line(lines - MAX_LINES) {
                buf.delete(&mut start, &mut to);
            }
        }
        self.autoscroll();
    }

    fn autoscroll(&self) {
        let mark = self.buffer.create_mark(None, &self.buffer.end_iter(), false);
        let view = self.view.clone();
        let buffer = self.buffer.clone();
        glib::idle_add_local_once(move || {
            view.scroll_mark_onscreen(&mark);
            buffer.delete_mark(&mark);
        });
    }

    /// Drop all messages + the pending queue (on switching videos).
    pub fn clear(&self) {
        self.queue.borrow_mut().clear();
        self.seen.borrow_mut().clear();
        self.buffer.set_text("");
    }

    pub fn set_can_send(&self, can: bool) {
        self.entry.set_sensitive(can);
        self.send.set_sensitive(can);
        self.entry.set_placeholder_text(Some(if can { "Chat…" } else { "Sign in to chat" }));
    }

    /// Replay chat is read-only (you can't post to a past stream): hide the compose row
    /// entirely. Pass `false` for live chat to restore it.
    pub fn set_replay(&self, replay: bool) {
        self.compose.set_visible(!replay);
    }

    pub fn connect_send<F: Fn(String) + 'static>(&self, f: F) {
        *self.send_cb.borrow_mut() = Some(Rc::new(f));
    }

    /// The header's mode button was clicked (cycle to the next presentation mode).
    pub fn connect_mode_cycle<F: Fn() + 'static>(&self, f: F) {
        *self.mode_cb.borrow_mut() = Some(Rc::new(f));
    }

    /// Reflect the active mode on the button (icon + tooltip) and the panel chrome:
    /// floating/overlay modes get a translucent, rounded card style + a grab cursor on
    /// the header; docked is the plain side panel.
    pub fn apply_mode(&self, mode: ChatMode) {
        self.mode_btn.set_icon_name(mode.icon());
        self.mode_btn.set_tooltip_text(Some(mode.tooltip()));
        let floaty = matches!(mode, ChatMode::Overlay | ChatMode::Floating);
        if floaty {
            // Translucent card over the video; drop the opaque themed background.
            self.root.remove_css_class("background");
            self.root.add_css_class("chat-overlay");
        } else {
            self.root.remove_css_class("chat-overlay");
            self.root.add_css_class("background");
        }
        // Only the free-floating panel advertises a move cursor on its header.
        if matches!(mode, ChatMode::Floating) {
            self.header.set_cursor_from_name(Some("grab"));
        } else {
            self.header.set_cursor(None);
        }
    }
}

/// Which name color tag applies, by role precedence (owner > mod > member > normal).
fn name_tag(badges: &[ChatBadge]) -> &'static str {
    if badges.iter().any(|b| matches!(b, ChatBadge::Owner)) {
        "owner"
    } else if badges.iter().any(|b| matches!(b, ChatBadge::Moderator)) {
        "mod"
    } else if badges.iter().any(|b| matches!(b, ChatBadge::Member { .. })) {
        "member"
    } else {
        "name"
    }
}

/// Loads `url` through the texture cache and hands the texture to `apply`, which targets a
/// `Picture` or an `adw::Avatar`.
fn load_image<F: Fn(&gdk::Texture) + 'static>(rt: &AsyncRuntime, cache: &ImageCache, url: String, apply: F) {
    if let Some(tex) = cache.borrow().get(&url) {
        apply(tex);
        return;
    }
    let cache = cache.clone();
    rt.spawn(
        async move { fetch_bytes(&url).await.map(|b| (url, b)) },
        move |res: Option<(String, Vec<u8>)>| {
            if let Some((url, bytes)) = res {
                let gb = glib::Bytes::from_owned(bytes);
                if let Ok(tex) = gdk::Texture::from_bytes(&gb) {
                    cache.borrow_mut().insert(url, tex.clone());
                    apply(&tex);
                }
            }
        },
    );
}

async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    let resp = reqwest::get(url).await.ok()?;
    resp.bytes().await.ok().map(|b| b.to_vec())
}
