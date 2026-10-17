//! "Ask" sidebar page: search a video's transcript and ask questions about it.
//! Every `[m:ss]` in a hit or an answer is a link that seeks there.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Button, Entry, Label, Orientation, PolicyType, ScrolledWindow};

use crate::catchup::fmt_ts;
use crate::transcript::{self, Line};
use crate::widgets::catchup_panel::linkify;

type SeekCb = Rc<RefCell<Option<Box<dyn Fn(i64)>>>>;
type AskCb = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;

/// Search hits shown at once.
const MAX_HITS: usize = 50;

pub struct AskPanel {
    pub root: gtk::Box,
    status: Label,
    /// Live only: transcribes the DVR window so there is something to search and ask.
    transcribe: Button,
    search: Entry,
    hits: gtk::Box,
    conversation: gtk::Box,
    scroll: ScrolledWindow,
    question: Entry,
    close: Button,
    lines: RefCell<Vec<Line>>,
    history: RefCell<Vec<(String, String)>>,
    /// The answer label waiting for a reply; `None` when no question is in flight.
    pending: RefCell<Option<Label>>,
    seek: SeekCb,
    ask: AskCb,
}

impl AskPanel {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(Orientation::Vertical, 8);
        root.set_width_request(360);
        root.add_css_class("ask-panel");
        root.set_margin_start(8);
        root.set_margin_end(8);
        root.set_margin_top(8);
        root.set_margin_bottom(8);

        let header = gtk::Box::new(Orientation::Horizontal, 6);
        let title = Label::new(Some("Ask about this video"));
        title.add_css_class("heading");
        title.set_halign(Align::Start);
        title.set_hexpand(true);
        let close = Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Close"));
        header.append(&title);
        header.append(&close);
        root.append(&header);

        let status = Label::new(None);
        status.add_css_class("dim-label");
        status.set_halign(Align::Start);
        status.set_wrap(true);
        status.set_xalign(0.0);
        root.append(&status);

        let transcribe = Button::with_label("Transcribe");
        transcribe.add_css_class("suggested-action");
        transcribe.set_halign(Align::Start);
        transcribe.set_visible(false);
        root.append(&transcribe);

        let search = Entry::builder().placeholder_text("Search the transcript").build();
        search.set_icon_from_icon_name(gtk::EntryIconPosition::Primary, Some("edit-find-symbolic"));
        root.append(&search);

        let body = gtk::Box::new(Orientation::Vertical, 10);
        let hits = gtk::Box::new(Orientation::Vertical, 4);
        let conversation = gtk::Box::new(Orientation::Vertical, 10);
        body.append(&hits);
        body.append(&conversation);
        let scroll = ScrolledWindow::new();
        scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&body));
        root.append(&scroll);

        let question = Entry::builder().placeholder_text("Ask a question, e.g. when did the reveal happen?").build();
        question.set_icon_from_icon_name(gtk::EntryIconPosition::Secondary, Some("mail-send-symbolic"));
        root.append(&question);

        let this = Rc::new(Self {
            root,
            status,
            transcribe,
            search,
            hits,
            conversation,
            scroll,
            question,
            close,
            lines: RefCell::new(Vec::new()),
            history: RefCell::new(Vec::new()),
            pending: RefCell::new(None),
            seek: Rc::new(RefCell::new(None)),
            ask: Rc::new(RefCell::new(None)),
        });

        let weak = Rc::downgrade(&this);
        this.search.connect_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.show_hits();
            }
        });
        let weak = Rc::downgrade(&this);
        this.question.connect_activate(move |_| {
            if let Some(this) = weak.upgrade() {
                this.submit();
            }
        });
        let weak = Rc::downgrade(&this);
        this.question.connect_icon_release(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.submit();
            }
        });
        this
    }

    pub fn connect_close(&self, f: impl Fn() + 'static) {
        self.close.connect_clicked(move |_| f());
    }

    /// A `[m:ss]` link was clicked → ms into the video.
    pub fn connect_seek(&self, f: impl Fn(i64) + 'static) {
        *self.seek.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_transcribe(&self, f: impl Fn() + 'static) {
        self.transcribe.connect_clicked(move |_| f());
    }

    /// Show the Transcribe button with `label`, or hide it with `None`.
    pub fn set_transcribe(&self, label: Option<&str>) {
        self.transcribe.set_visible(label.is_some());
        if let Some(l) = label {
            self.transcribe.set_label(l);
        }
    }

    pub fn set_transcribing(&self, busy: bool) {
        self.transcribe.set_sensitive(!busy);
    }

    /// Press Transcribe if it is offered; the headless test hook's entry point.
    pub fn click_transcribe(&self) -> bool {
        let offered = self.transcribe.is_visible() && self.transcribe.is_sensitive();
        if offered {
            self.transcribe.emit_clicked();
        }
        offered
    }

    /// A question was submitted; answer it with [`Self::show_answer`].
    pub fn connect_ask(&self, f: impl Fn(String) + 'static) {
        *self.ask.borrow_mut() = Some(Box::new(f));
    }

    /// Drop the previous video's transcript and conversation.
    pub fn reset(&self, status: &str) {
        self.lines.borrow_mut().clear();
        self.history.borrow_mut().clear();
        self.pending.borrow_mut().take();
        self.search.set_text("");
        clear(&self.hits);
        clear(&self.conversation);
        self.status.set_text(status);
        self.search.set_sensitive(false);
        self.question.set_sensitive(false);
    }

    pub fn set_transcript(&self, lines: Vec<Line>, source: &str) {
        let end = lines.last().map_or(0, |l| l.start_ms);
        self.status.set_text(&format!("{} lines up to {} · {source}", lines.len(), fmt_ts(end)));
        *self.lines.borrow_mut() = lines;
        self.search.set_sensitive(true);
        self.question.set_sensitive(true);
        self.question.grab_focus();
    }

    pub fn lines(&self) -> Vec<Line> {
        self.lines.borrow().clone()
    }

    pub fn history(&self) -> Vec<(String, String)> {
        self.history.borrow().clone()
    }

    /// The reply to the question in flight.
    pub fn show_answer(&self, question: &str, result: Result<String, String>) {
        let Some(label) = self.pending.borrow_mut().take() else { return };
        match result {
            Ok(answer) => {
                label.set_markup(&linkify(&answer));
                label.remove_css_class("dim-label");
                self.history.borrow_mut().push((question.to_string(), answer));
            }
            Err(e) => {
                label.set_text(&format!("Could not answer: {e}"));
                label.add_css_class("error");
            }
        }
        self.question.set_sensitive(true);
        self.question.grab_focus();
        self.scroll_to_end();
    }

    /// Ask `q` as if typed; the headless test hook's entry point.
    pub fn ask_now(&self, q: &str) {
        self.question.set_text(q);
        self.submit();
    }

    fn submit(&self) {
        let q = self.question.text().trim().to_string();
        if q.is_empty() || self.pending.borrow().is_some() || self.lines.borrow().is_empty() {
            return;
        }
        self.question.set_text("");
        self.question.set_sensitive(false);
        let asked = Label::new(Some(&q));
        asked.add_css_class("heading");
        asked.set_wrap(true);
        asked.set_xalign(0.0);
        asked.set_selectable(true);
        self.conversation.append(&asked);
        let answer = self.linked_label("Thinking…");
        answer.add_css_class("dim-label");
        self.conversation.append(&answer);
        *self.pending.borrow_mut() = Some(answer);
        self.scroll_to_end();
        if let Some(f) = self.ask.borrow().as_ref() {
            f(q);
        }
    }

    fn show_hits(&self) {
        clear(&self.hits);
        let query = self.search.text();
        let lines = self.lines.borrow();
        let found = transcript::search(&lines, &query);
        if query.trim().is_empty() {
            return;
        }
        let n = Label::new(Some(&match found.len() {
            0 => "No matches".to_string(),
            n if n > MAX_HITS => format!("{n} matches, first {MAX_HITS} shown"),
            n => format!("{n} matches"),
        }));
        n.add_css_class("dim-label");
        n.set_halign(Align::Start);
        self.hits.append(&n);
        for &i in found.iter().take(MAX_HITS) {
            let l = &lines[i];
            self.hits.append(&self.linked_label(&format!("[{}] {}", fmt_ts(l.start_ms), l.text)));
        }
    }

    fn linked_label(&self, text: &str) -> Label {
        let l = Label::new(None);
        l.set_markup(&linkify(text));
        l.set_wrap(true);
        l.set_xalign(0.0);
        l.set_selectable(true);
        let seek = self.seek.clone();
        l.connect_activate_link(move |_, href| {
            if let Some(ms) = href.strip_prefix("ts:").and_then(crate::catchup::parse_ts) {
                if let Some(f) = seek.borrow().as_ref() {
                    f(ms);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        l
    }

    fn scroll_to_end(&self) {
        let adj = self.scroll.vadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}
