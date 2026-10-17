//! VOD comments sidebar: a scrollable list of comment rows (circular avatar, author
//! name + creator/verified badge, wrapped text, like/reply/time meta). Fetching is
//! driven by `window::watch` (`Session::comments`/`comments_page`); near the bottom it fires
//! `connect_load_more` for the next page.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::{Toast, ToastOverlay};
use gtk4 as gtk;
use gtk::gdk;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Button, Entry, Label, Orientation, PolicyType, ScrolledWindow};

use innertube::{Comment, Comments, Error, Session};

use crate::runtime::AsyncRuntime;

type ImageCache = Rc<RefCell<HashMap<String, gdk::Texture>>>;
type LoadMoreCb = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
/// Player-seek callback for a clickable comment timestamp.
type SeekCb = RefCell<Option<Rc<dyn Fn(f64)>>>;

/// Optimistically bump a like count for display. A plain integer increments by one (with
/// thousands separators). An abbreviated count (1.2K, 3M) is left unchanged and the button
/// highlight shows the like. Empty becomes "1".
fn bump_like_count(s: &str) -> String {
    if s.is_empty() {
        return "1".to_string();
    }
    if s.chars().any(|c| matches!(c, 'K' | 'M' | 'B' | 'k' | 'm' | 'b')) {
        return s.to_string();
    }
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.parse::<u64>() {
        Ok(n) => group_thousands(n + 1),
        Err(_) => s.to_string(),
    }
}

/// `1234 -> "1,234"`.
fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in b.iter().enumerate() {
        if i > 0 && (b.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch as char);
    }
    out
}

pub struct CommentsView {
    pub root: GtkBox,
    list: GtkBox,
    rt: AsyncRuntime,
    session: Rc<RefCell<Arc<Session>>>,
    toasts: ToastOverlay,
    video_id: RefCell<String>,
    post_box: GtkBox,
    avatars: ImageCache,
    load_more: LoadMoreCb,
    /// Signed-in identity, shown as the author of optimistic comments. Fetched once from
    /// `account_info` after the first authed video.
    account_name: Rc<RefCell<Option<String>>>,
    account_avatar: Rc<RefCell<Option<String>>>,
    account_fetched: Rc<Cell<bool>>,
    /// Seeks the player to `secs` seconds; set by `window::app` for clickable timestamps.
    seek: SeekCb,
    /// Reply threads page through `browse`, as a Short's comments do.
    shorts: Cell<bool>,
}

/// Linkify `mm:ss` / `h:mm:ss` timestamp tokens in comment text as Pango `<a href="t:SECONDS">`
/// anchors and escape the rest. A token links only when whole: start, space, `(` or `[` on the
/// left; end, space, `)`, `]` or punctuation on the right. A number-colon-number run inside a
/// URL or a ratio is left alone. Returns `(markup, any_link)`.
fn linkify_timestamps(text: &str) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut any = false;
    let mut i = 0;
    let is_boundary_left = |c: Option<&char>| matches!(c, None | Some(' ' | '\n' | '\t' | '(' | '[' | '\u{2013}' | '\u{2014}'));
    while i < chars.len() {
        // Try to match a timestamp starting at i, only at a left boundary.
        if is_boundary_left(if i == 0 { None } else { chars.get(i - 1) }) {
            if let Some((secs, len)) = match_timestamp(&chars[i..]) {
                let end = chars.get(i + len);
                let boundary_right = matches!(end, None | Some(' ' | '\n' | '\t' | ')' | ']' | '.' | ',' | '!' | '?' | ':'));
                if boundary_right {
                    let label: String = chars[i..i + len].iter().collect();
                    out.push_str(&format!("<a href=\"t:{secs}\">{}</a>", glib::markup_escape_text(&label)));
                    any = true;
                    i += len;
                    continue;
                }
            }
        }
        out.push_str(&glib::markup_escape_text(&chars[i].to_string()));
        i += 1;
    }
    (out, any)
}

/// Match a `mm:ss` or `h:mm:ss` timestamp at the slice start. Returns `(seconds,
/// char_len)`. Segments: 1-2 digits each; seconds/minutes 0-59 except the leading one.
fn match_timestamp(s: &[char]) -> Option<(i64, usize)> {
    let mut nums: Vec<i64> = Vec::new();
    let mut idx = 0;
    loop {
        let start = idx;
        while idx < s.len() && s[idx].is_ascii_digit() && idx - start < 2 {
            idx += 1;
        }
        if idx == start {
            return None; // no digits where a segment was expected
        }
        let seg: i64 = s[start..idx].iter().collect::<String>().parse().ok()?;
        nums.push(seg);
        if idx < s.len() && s[idx] == ':' && nums.len() < 3 {
            idx += 1; // consume the colon, expect another segment
        } else {
            break;
        }
    }
    let total = match nums.as_slice() {
        [m, sec] if *sec < 60 => m * 60 + sec,
        [h, m, sec] if *m < 60 && *sec < 60 => h * 3600 + m * 60 + sec,
        _ => return None,
    };
    Some((total, idx))
}

impl CommentsView {
    /// A Short's comments: reply threads load through `browse`.
    pub fn set_shorts(&self) {
        self.shorts.set(true);
    }

    /// Hide the built-in "Comments" heading, for a host that draws its own.
    pub fn hide_heading(&self) {
        if let Some(title) = self.root.first_child() {
            title.set_visible(false);
        }
    }

    pub fn new(rt: AsyncRuntime, session: Rc<RefCell<Arc<Session>>>, toasts: ToastOverlay) -> Rc<Self> {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.set_width_request(360);
        root.add_css_class("background");

        let title = Label::new(Some("Comments"));
        title.add_css_class("heading");
        title.set_halign(Align::Start);
        title.set_margin_top(8);
        title.set_margin_bottom(6);
        title.set_margin_start(12);
        root.append(&title);

        // Post box: an entry + Comment button (hidden when not signed in).
        let post_box = GtkBox::new(Orientation::Horizontal, 6);
        post_box.set_margin_start(12);
        post_box.set_margin_end(12);
        post_box.set_margin_bottom(6);
        let post_entry = Entry::builder().placeholder_text("Add a comment…").hexpand(true).build();
        super::enable_emoji_shortcodes(&post_entry);
        let post_btn = Button::with_label("Comment");
        post_box.append(&post_entry);
        post_box.append(&post_btn);
        root.append(&post_box);

        let list = GtkBox::new(Orientation::Vertical, 12);
        list.set_margin_start(10);
        list.set_margin_end(10);
        list.set_margin_top(4);
        list.set_margin_bottom(8);
        let scroller = ScrolledWindow::new();
        // The outer left-column scroller owns scrolling on the watch page (the video is pinned
        // above), so this one never scrolls and propagates its natural height.
        scroller.set_policy(PolicyType::Never, PolicyType::Never);
        scroller.set_vexpand(false);
        // The outer scroller calls `request_load_more` for pagination. The inner adjustment
        // below only matters when the widget is standalone.
        scroller.set_propagate_natural_height(true);
        scroller.set_child(Some(&list));
        root.append(&scroller);

        let load_more: LoadMoreCb = Rc::new(RefCell::new(None));
        // Infinite scroll: fire load_more when within ~600px of the bottom (guarded so a
        // burst of scroll events fires once per page).
        {
            let load_more = load_more.clone();
            let pending = Rc::new(Cell::new(false));
            let adj = scroller.vadjustment();
            adj.connect_value_changed(move |a| {
                if a.upper() - (a.value() + a.page_size()) < 600.0 && !pending.get() {
                    if let Some(f) = load_more.borrow().clone() {
                        pending.set(true);
                        f();
                        // Re-arm shortly; the page append moves `upper` so the next check passes.
                        let pending2 = pending.clone();
                        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                            pending2.set(false)
                        });
                    }
                }
            });
        }

        let this = Rc::new(Self {
            root,
            list,
            rt,
            session,
            toasts,
            video_id: RefCell::new(String::new()),
            post_box,
            avatars: Rc::new(RefCell::new(HashMap::new())),
            load_more,
            account_name: Rc::new(RefCell::new(None)),
            account_avatar: Rc::new(RefCell::new(None)),
            account_fetched: Rc::new(Cell::new(false)),
            seek: RefCell::new(None),
            shorts: Cell::new(false),
        });

        {
            let t = this.clone();
            let entry = post_entry.clone();
            post_btn.connect_clicked(move |_| t.post_comment(&entry));
        }
        {
            let t = this.clone();
            post_entry.connect_activate(move |entry| t.post_comment(entry));
        }

        this
    }

    /// Register the player-seek callback for clickable comment timestamps.
    pub fn set_on_seek(&self, f: Rc<dyn Fn(f64)>) {
        *self.seek.borrow_mut() = Some(f);
    }

    /// Set the video the post box targets, and show/hide it by auth state.
    pub fn set_video_id(&self, id: &str) {
        *self.video_id.borrow_mut() = id.to_string();
        let authed = self.session.borrow().is_authenticated();
        self.post_box.set_visible(authed);
        // Fetch the signed-in identity once, so an optimistic comment shows the real name and
        // avatar instead of "You".
        if authed && !self.account_fetched.replace(true) {
            let sess = self.session.borrow().clone();
            let name = self.account_name.clone();
            let avatar = self.account_avatar.clone();
            self.rt.spawn(
                async move { sess.account_info().await },
                move |res: Result<innertube::AccountInfo, innertube::Error>| {
                    if let Ok(info) = res {
                        tracing::info!(
                            "[account] name={:?} handle={:?} avatar={}",
                            info.name, info.handle, info.avatar_url.is_some()
                        );
                        *name.borrow_mut() = info.name;
                        *avatar.borrow_mut() = info.avatar_url;
                    }
                },
            );
        }
    }

    /// Post a top-level comment from the entry's text.
    fn post_comment(self: &Rc<Self>, entry: &Entry) {
        let text = entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let vid = self.video_id.borrow().clone();
        if vid.is_empty() {
            return;
        }
        let current = self.session.borrow().clone();
        let this = self.clone();
        let entry = entry.clone();
        let posted_text = text.clone();
        entry.set_sensitive(false);
        self.rt.spawn(
            async move { current.comment(&vid, &text).await },
            move |res| {
                entry.set_sensitive(true);
                match res {
                    Ok(_) => {
                        entry.set_text("");
                        // Show the new comment at the top at once: the create response does
                        // not reliably echo it, and a refetch of the "top" page would not list
                        // it. Uses the signed-in identity, else "You".
                        let mine = Comment {
                            id: String::new(),
                            author: this.account_name.borrow().clone().unwrap_or_else(|| "You".to_string()),
                            channel_id: None,
                            author_thumbnail_url: this.account_avatar.borrow().clone(),
                            text: posted_text,
                            published: Some("now".to_string()),
                            like_count: Some("0".to_string()),
                            reply_count: None,
                            is_creator: false,
                            is_verified: false,
                            replies_token: None,
                            like_token: None,
                            unlike_token: None,
                            reply_params: None,
                            reply_level: 0,
                        };
                        let row = this.build_row(&mine);
                        this.list.prepend(&row);
                        this.toasts.add_toast(Toast::new("Comment posted"));
                    }
                    // Keep the typed text on failure.
                    Err(e) => this.toasts.add_toast(Toast::new(&format!("Couldn't post: {e}"))),
                }
            },
        );
    }

    /// Replace the list with the first page.
    pub fn set_comments(self: &Rc<Self>, comments: &[Comment]) {
        self.clear();
        self.append_comments(comments);
    }

    /// Append a further page.
    pub fn append_comments(self: &Rc<Self>, comments: &[Comment]) {
        for c in comments {
            let row = self.build_row(c);
            self.list.append(&row);
        }
    }

    pub fn clear(&self) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
    }

    /// Build one comment widget (avatar + text + meta), including a "View replies"
    /// button + (lazily filled) indented replies sub-box when the comment has replies.
    fn build_row(self: &Rc<Self>, c: &Comment) -> GtkBox {
        let row = GtkBox::new(Orientation::Horizontal, 10);
        row.set_halign(Align::Fill);
        // A reply to a reply sits under the reply it answers.
        if c.reply_level >= 2 {
            row.set_margin_start(34);
        }

        let avatar = adw::Avatar::new(if c.reply_level > 0 { 24 } else { 36 }, Some(&c.author), true);
        avatar.set_valign(Align::Start);
        if let Some(url) = &c.author_thumbnail_url {
            let av = avatar.clone();
            load_avatar(&self.rt, &self.avatars, url.clone(), move |t| av.set_custom_image(Some(t)));
        }
        row.append(&avatar);

        let col = GtkBox::new(Orientation::Vertical, 2);
        col.set_hexpand(true);

        // Header: author (+ badge) + relative time.
        let badge = if c.is_creator {
            "  <span foreground='#3ea6ff'>creator</span>"
        } else if c.is_verified {
            "  ✓"
        } else {
            ""
        };
        let when = c.published.as_deref().unwrap_or("");
        let header = Label::new(None);
        header.set_markup(&format!(
            "<b>{}</b>{}  <span foreground='#888' size='small'>{}</span>",
            glib::markup_escape_text(&c.author),
            badge,
            glib::markup_escape_text(when),
        ));
        header.set_halign(Align::Start);
        header.set_xalign(0.0);
        header.set_wrap(true);
        header.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        col.append(&header);

        let body = Label::new(None);
        body.set_halign(Align::Start);
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.set_selectable(true);
        // Clamp a long comment to a few lines so one giant comment doesn't fill the whole
        // pane; a "Read more" toggle (only shown when it actually overflows) expands it.
        const COMMENT_LINES: i32 = 8;
        body.set_lines(COMMENT_LINES);
        body.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // Render `mm:ss` / `h:mm:ss` in the comment as clickable seek links.
        let (markup, has_ts) = linkify_timestamps(&c.text);
        if has_ts {
            body.set_use_markup(true);
            body.set_markup(&markup);
            let seek = self.seek.borrow().clone();
            body.connect_activate_link(move |_, href| {
                if let Some(secs) = href.strip_prefix("t:").and_then(|s| s.parse::<f64>().ok()) {
                    if let Some(f) = &seek {
                        f(secs);
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        } else {
            body.set_text(&c.text);
        }
        col.append(&body);
        let more_btn = Button::with_label("Read more");
        more_btn.add_css_class("flat");
        more_btn.add_css_class("comment-more");
        more_btn.set_halign(Align::Start);
        more_btn.set_visible(false);
        col.append(&more_btn);
        {
            let expanded = Cell::new(false);
            let body = body.clone();
            more_btn.connect_clicked(move |b| {
                let now = !expanded.get();
                expanded.set(now);
                body.set_lines(if now { -1 } else { COMMENT_LINES });
                body.set_ellipsize(if now {
                    gtk::pango::EllipsizeMode::None
                } else {
                    gtk::pango::EllipsizeMode::End
                });
                b.set_label(if now { "Show less" } else { "Read more" });
            });
        }
        crate::widgets::reveal_when_truncated(&body, more_btn.upcast_ref());

        // Meta row: the like count is the like button (👍 N), incremented optimistically and
        // reverted on failure. The "N replies" toggle below carries the reply count.
        let meta_row = GtkBox::new(Orientation::Horizontal, 12);
        meta_row.set_halign(Align::Start);
        meta_row.set_margin_top(2);

        let count = c.like_count.clone().unwrap_or_default();
        let like_label = format!("\u{1F44D} {count}");
        if let Some(token) = c.like_token.clone() {
            // Likeable: a flat button showing the count; a click likes, a second click un-likes.
            let like_btn = Button::builder().label(&like_label).css_classes(["flat", "comment-like"]).build();
            let liked = Rc::new(Cell::new(false));
            let unlike = c.unlike_token.clone();
            let base = count.clone();
            let this = self.clone();
            let show = move |btn: &Button, on: bool| {
                let n = if on { bump_like_count(&base) } else { base.clone() };
                btn.set_label(&format!("\u{1F44D} {n}"));
                if on {
                    btn.add_css_class("liked");
                } else {
                    btn.remove_css_class("liked");
                }
            };
            let show = Rc::new(show);
            like_btn.connect_clicked(move |btn| {
                let now = !liked.get();
                let action = if now { Some(token.clone()) } else { unlike.clone() };
                let Some(action) = action else { return };
                liked.set(now);
                show(btn, now);
                let current = this.session.borrow().clone();
                let (this2, btn2, liked2, show2) = (this.clone(), btn.clone(), liked.clone(), show.clone());
                this.rt.spawn(
                    async move { current.perform_comment_action(&action).await },
                    move |res| {
                        tracing::info!("[comment] liked={now} accepted={}", res.is_ok());
                        if let Err(e) = res {
                            liked2.set(!now);
                            show2(&btn2, !now);
                            this2.toasts.add_toast(Toast::new(&format!("Couldn't update the like: {e}")));
                        }
                    },
                );
            });
            meta_row.append(&like_btn);
        } else if !count.is_empty() {
            // Not likeable (anonymous / already liked) → display-only.
            let l = Label::new(Some(&like_label));
            l.add_css_class("dim-label");
            l.add_css_class("caption");
            meta_row.append(&l);
        }
        // Reply: a toggle reveals an inline composer when the comment accepts replies and the
        // viewer is signed in. Posting calls create_comment_reply.
        let reply_ui: Option<(Button, GtkBox)> =
            if c.reply_params.is_some() && self.session.borrow().is_authenticated() {
                let reply_btn = Button::with_label("Reply");
                reply_btn.add_css_class("flat");
                reply_btn.add_css_class("comment-reply-btn");
                reply_btn.set_halign(Align::Start);
                meta_row.append(&reply_btn);
                Some((reply_btn, GtkBox::new(Orientation::Horizontal, 6)))
            } else {
                None
            };

        if meta_row.first_child().is_some() {
            col.append(&meta_row);
        }

        // The reply composer box (hidden until "Reply" is clicked). Its own row so it
        // sits under the meta line; emoji-autocomplete-enabled like the top composer.
        if let (Some((reply_btn, composer)), Some(params)) = (reply_ui, c.reply_params.clone()) {
            composer.set_margin_start(24);
            composer.set_margin_top(4);
            composer.set_visible(false);
            let entry = Entry::builder().placeholder_text("Add a reply…").hexpand(true).build();
            super::enable_emoji_shortcodes(&entry);
            let post = Button::with_label("Reply");
            composer.append(&entry);
            composer.append(&post);
            col.append(&composer);

            {
                let composer = composer.clone();
                let entry = entry.clone();
                reply_btn.connect_clicked(move |_| {
                    let vis = !composer.is_visible();
                    composer.set_visible(vis);
                    if vis {
                        entry.grab_focus();
                    }
                });
            }
            let submit = {
                let this = self.clone();
                let entry = entry.clone();
                let composer = composer.clone();
                let post = post.clone();
                move || {
                    let text = entry.text().to_string();
                    if text.trim().is_empty() {
                        return;
                    }
                    entry.set_sensitive(false);
                    post.set_sensitive(false);
                    let current = this.session.borrow().clone();
                    let params = params.clone();
                    let author = this.account_name.borrow().clone().unwrap_or_else(|| "You".into());
                    let (this2, entry2, post2, composer2) =
                        (this.clone(), entry.clone(), post.clone(), composer.clone());
                    this.rt.spawn(
                        async move { current.create_comment_reply(&params, &text).await },
                        move |res: Result<serde_json::Value, Error>| {
                            entry2.set_sensitive(true);
                            post2.set_sensitive(true);
                            match res {
                                Ok(_) => {
                                    entry2.set_text("");
                                    composer2.set_visible(false);
                                    this2.toasts.add_toast(Toast::new(&format!(
                                        "Replied as {author}"
                                    )));
                                }
                                Err(e) => this2
                                    .toasts
                                    .add_toast(Toast::new(&format!("Couldn't reply: {e}"))),
                            }
                        },
                    );
                }
            };
            {
                let submit = submit.clone();
                post.connect_clicked(move |_| submit());
            }
            entry.connect_activate(move |_| submit());
        }

        // View replies: a lazily-loaded, indented sub-box under a top-level comment. A reply's
        // own replies arrive inline in its thread page, in order.
        if let Some(token) = c.replies_token.clone().filter(|_| c.reply_level == 0) {
            let count = c.reply_count.clone().filter(|s| !s.is_empty());
            let label = move |open: bool| {
                let arrow = if open { '\u{25B4}' } else { '\u{25BE}' };
                match count.as_deref() {
                    Some("1") => format!("{arrow} 1 reply"),
                    Some(n) => format!("{arrow} {n} replies"),
                    None => format!("{arrow} Replies"),
                }
            };
            let view_btn = Button::with_label(&label(false));
            view_btn.add_css_class("flat");
            view_btn.add_css_class("comment-replies-toggle");
            view_btn.set_halign(Align::Start);
            let replies_box = GtkBox::new(Orientation::Vertical, 12);
            replies_box.set_margin_start(24);
            replies_box.set_margin_top(6);
            replies_box.set_visible(false);
            col.append(&view_btn);
            col.append(&replies_box);

            let loaded = Rc::new(Cell::new(false));
            let this = self.clone();
            let rbox = replies_box.clone();
            view_btn.connect_clicked(move |btn| {
                if loaded.get() {
                    let vis = !rbox.is_visible();
                    rbox.set_visible(vis);
                    btn.set_label(&label(vis));
                    return;
                }
                loaded.set(true);
                rbox.set_visible(true);
                btn.set_label(&label(true));
                this.load_replies(token.clone(), rbox.clone());
            });
        }

        row.append(&col);
        row
    }

    /// Appends one page of a reply thread to `rbox`, ending in "Show more replies" while
    /// the thread has more pages.
    fn load_replies(self: &Rc<Self>, token: String, rbox: GtkBox) {
        let spinner = gtk::Spinner::builder().spinning(true).halign(Align::Start).build();
        rbox.append(&spinner);
        let current = self.session.borrow().clone();
        let shorts = self.shorts.get();
        let this = self.clone();
        self.rt.spawn(
            async move {
                if shorts {
                    current.reel_comments_page(&token).await
                } else {
                    current.comments_page(&token).await
                }
            },
            move |res: Result<Comments, Error>| {
                rbox.remove(&spinner);
                match res {
                    Ok(page) => {
                        let nested = page.items.iter().filter(|c| c.reply_level >= 2).count();
                        tracing::info!("[comments] reply page: {} replies, {nested} nested", page.items.len());
                        for reply in &page.items {
                            rbox.append(&this.build_row(reply));
                        }
                        if let Some(next) = page.continuation {
                            let more = Button::with_label("Show more replies");
                            more.add_css_class("flat");
                            more.add_css_class("comment-replies-toggle");
                            more.set_halign(Align::Start);
                            let (this, rbox2) = (this.clone(), rbox.clone());
                            more.connect_clicked(move |b| {
                                rbox2.remove(b);
                                this.load_replies(next.clone(), rbox2.clone());
                            });
                            rbox.append(&more);
                        }
                    }
                    Err(e) => this.toasts.add_toast(Toast::new(&format!("Couldn't load replies: {e}"))),
                }
            },
        );
    }

    pub fn connect_load_more<F: Fn() + 'static>(&self, f: F) {
        *self.load_more.borrow_mut() = Some(Rc::new(f));
    }

    /// Fire the registered load-more callback (fetch the next page). Used when the widget
    /// is embedded in an outer scroller (the watch page) that owns scrolling, so the inner
    /// near-bottom trigger never fires. The callback is generation-guarded and no-ops when
    /// there's no continuation, so an over-eager call is safe.
    pub fn request_load_more(&self) {
        if let Some(f) = self.load_more.borrow().clone() {
            f();
        }
    }
}

/// Load `url` (cached) into an `adw::Avatar` custom image.
fn load_avatar<F: Fn(&gdk::Texture) + 'static>(rt: &AsyncRuntime, cache: &ImageCache, url: String, apply: F) {
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

#[cfg(test)]
mod tests {
    use super::{bump_like_count, linkify_timestamps, match_timestamp};

    #[test]
    fn match_timestamp_parses_mm_ss_and_h_mm_ss() {
        assert_eq!(match_timestamp(&"0:18".chars().collect::<Vec<_>>()), Some((18, 4)));
        assert_eq!(match_timestamp(&"1:05".chars().collect::<Vec<_>>()), Some((65, 4)));
        assert_eq!(match_timestamp(&"1:02:05".chars().collect::<Vec<_>>()), Some((3725, 7)));
        // Trailing text after a valid stamp: match only the stamp.
        assert_eq!(match_timestamp(&"2:30 min".chars().collect::<Vec<_>>()), Some((150, 4)));
        // Invalid: seconds >= 60, or not a timestamp.
        assert_eq!(match_timestamp(&"1:75".chars().collect::<Vec<_>>()), None);
        assert_eq!(match_timestamp(&"hello".chars().collect::<Vec<_>>()), None);
    }

    #[test]
    fn linkify_wraps_whole_timestamps_and_escapes_the_rest() {
        let (m, any) = linkify_timestamps("intro at 0:18 & the drop 1:02:05!");
        assert!(any);
        assert!(m.contains("<a href=\"t:18\">0:18</a>"), "mm:ss linked: {m}");
        assert!(m.contains("<a href=\"t:3725\">1:02:05</a>"), "h:mm:ss linked: {m}");
        assert!(m.contains("&amp;"), "ampersand escaped: {m}");
        // No timestamp → no markup, no link.
        let (_, any2) = linkify_timestamps("just plain text no stamps");
        assert!(!any2);
    }

    #[test]
    fn linkify_ignores_ratios_and_urls() {
        // A ratio like "16:9" parses as m:ss and is linked when whole. A colon-number run
        // inside a longer token has no left boundary and is skipped.
        let (m, _) = linkify_timestamps("aspect16:9 ratio");
        assert!(!m.contains("<a "), "no boundary before 16:9 in 'aspect16:9' → not linked: {m}");
    }

    #[test]
    fn like_count_bumps_plain_integers() {
        assert_eq!(bump_like_count("5"), "6");
        assert_eq!(bump_like_count("999"), "1,000");
        assert_eq!(bump_like_count("1,234"), "1,235");
        assert_eq!(bump_like_count(""), "1");
    }
}
