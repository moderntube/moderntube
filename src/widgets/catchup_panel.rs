//! "Catch me up" result panel: a sidebar page showing the summary with clickable `[h:mm:ss]`
//! anchors (DVR seek) and a collapsible transcript.
//!
//! It is a page of the window's sidebar `Stack`, in the slot live chat uses. The sidebar
//! swaps to it once a summary is ready, and the close button returns to chat. A running
//! catch-up shows in [`super::CatchupStrip`], which leaves chat on screen.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Button, Expander, Label, Orientation, PolicyType, ScrolledWindow};

use crate::catchup::{fmt_ts, parse_ts, CatchupResult};

/// The panel's `[h:mm:ss]`-click handler (→ stream-absolute ms).
type SeekCb = Rc<RefCell<Option<Box<dyn Fn(i64)>>>>;

pub struct CatchupPanel {
    pub root: gtk::Box,
    title: Label,
    /// Result content (summary and transcript), rebuilt per run.
    content: gtk::Box,
    /// `[h:mm:ss]` click → stream-absolute ms.
    seek: SeekCb,
    close: Button,
}

/// Escape `text` for Pango and linkify every `[h:mm:ss]` occurrence as
/// `<a href="ts:...">` (the panel's activate-link handler parses it back).
pub(crate) fn linkify(text: &str) -> String {
    let escaped = glib::markup_escape_text(text);
    let mut out = String::with_capacity(escaped.len() + 64);
    let mut rest = escaped.as_str();
    while let Some(open) = rest.find('[') {
        let (before, after_open) = rest.split_at(open);
        out.push_str(before);
        match after_open[1..].find(']') {
            Some(close) => {
                let stamp = &after_open[1..1 + close];
                if parse_ts(stamp).is_some() {
                    out.push_str(&format!("<a href=\"ts:{stamp}\">[{stamp}]</a>"));
                } else {
                    out.push('[');
                    out.push_str(stamp);
                    out.push(']');
                }
                rest = &after_open[close + 2..];
            }
            None => {
                out.push_str(after_open);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

impl CatchupPanel {
    pub fn new() -> Self {
        let root = gtk::Box::new(Orientation::Vertical, 8);
        root.set_width_request(360);
        root.add_css_class("catchup-panel");
        root.set_margin_start(8);
        root.set_margin_end(8);
        root.set_margin_top(8);
        root.set_margin_bottom(8);

        let header = gtk::Box::new(Orientation::Horizontal, 6);
        let title = Label::new(Some("Catch me up"));
        title.add_css_class("heading");
        title.set_halign(Align::Start);
        title.set_hexpand(true);
        let close = Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Back to chat"));
        header.append(&title);
        header.append(&close);
        root.append(&header);

        let content = gtk::Box::new(Orientation::Vertical, 10);
        let scroll = ScrolledWindow::new();
        scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&content));
        root.append(&scroll);

        Self { root, title, content, seek: Rc::new(RefCell::new(None)), close }
    }

    pub fn connect_close(&self, f: impl Fn() + 'static) {
        self.close.connect_clicked(move |_| f());
    }

    /// `[h:mm:ss]` anchor clicked → stream-absolute ms.
    pub fn connect_seek(&self, f: impl Fn(i64) + 'static) {
        *self.seek.borrow_mut() = Some(Box::new(f));
    }

    /// Drop the previous run's summary. Called when a new run starts so a stale
    /// summary can't be read as the one being generated.
    pub fn reset(&self) {
        self.title.set_text("Catch me up");
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
    }

    /// A label whose `[h:mm:ss]` anchors dispatch to the panel's seek callback.
    fn linked_label(&self, text: &str) -> Label {
        let l = Label::new(None);
        l.set_use_markup(true);
        l.set_markup(&linkify(text));
        l.set_wrap(true);
        l.set_xalign(0.0);
        l.set_selectable(true);
        let seek = self.seek.clone();
        l.connect_activate_link(move |_, href| {
            if let Some(stamp) = href.strip_prefix("ts:") {
                if let Some(ms) = crate::catchup::parse_ts(stamp) {
                    if let Some(f) = seek.borrow().as_ref() {
                        f(ms);
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        l
    }

    pub fn show_result(&self, res: &CatchupResult) {
        self.title.set_text(&format!(
            "Catch me up  ·  {} → {}",
            fmt_ts(res.from_ms),
            fmt_ts(res.to_ms)
        ));
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        self.content.append(&self.linked_label(&res.summary));

        if !res.segments.is_empty() {
            let exp = Expander::new(Some("Transcript"));
            let mut body = String::new();
            for s in &res.segments {
                body.push_str(&format!("[{}] {}\n", fmt_ts(s.start_ms), s.text));
            }
            let t = self.linked_label(body.trim_end());
            t.add_css_class("dim-label");
            exp.set_child(Some(&t));
            self.content.append(&exp);
        }
        if res.chat_count > 0 {
            let n = Label::new(Some(&format!("{} chat messages folded in", res.chat_count)));
            n.add_css_class("dim-label");
            n.set_halign(Align::Start);
            self.content.append(&n);
        }
    }
}

impl Default for CatchupPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkify_anchors_timestamps_and_escapes_the_rest() {
        let s = linkify("- [1:02:05] A&B <talk>\nplain [not a ts] tail");
        assert!(s.contains("<a href=\"ts:1:02:05\">[1:02:05]</a>"), "stamp becomes a link: {s}");
        assert!(s.contains("A&amp;B &lt;talk&gt;"), "markup escaped: {s}");
        assert!(s.contains("[not a ts]"), "non-timestamp brackets left plain: {s}");
    }
}
