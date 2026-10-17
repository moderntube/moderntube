//! Catch-up progress strip: a slim status line above the sidebar stack, shown while a
//! "catch me up" run works.
//!
//! The strip is the running surface and [`super::CatchupPanel`] the result surface. Chat stays
//! visible and scrollable under the strip, and the strip collapses to the progress bar alone.
//! It shows the fetch percentage, transcription chunk counts, the rate-limit wait, an
//! indeterminate pulse, elapsed time, and the failure message with its Retry button.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Label, Orientation, ProgressBar};

use crate::catchup::{fmt_ts, CatchupPhase};

/// Maps a phase to the strip's status line and the bar fraction. `None` means the phase has
/// no measurable progress, so the bar pulses. A static full bar would read as a hang.
fn describe(phase: &CatchupPhase) -> (String, Option<f64>) {
    match *phase {
        CatchupPhase::Fetching(done_ms, total_ms) if total_ms > 0 => {
            let f = (done_ms as f64 / total_ms as f64).clamp(0.0, 1.0);
            (format!("Fetching audio… {:.0}%", f * 100.0), Some(f))
        }
        // The window end is not known yet (edge-bounded first chunk): report the buffered
        // seconds instead of a percent.
        CatchupPhase::Fetching(done_ms, _) => {
            (format!("Fetching audio… {}s", done_ms / 1000), None)
        }
        CatchupPhase::Transcribing(done, total) => {
            let f = if total > 0 { (done as f64 / total as f64).clamp(0.0, 1.0) } else { 0.0 };
            (format!("Transcribing… {done}/{total}"), Some(f))
        }
        CatchupPhase::RateLimited(secs) => {
            (format!("Rate-limited — retrying in {secs}s (progress saved)"), None)
        }
        CatchupPhase::Chat => ("Folding in chat…".to_string(), None),
        CatchupPhase::Summarizing => ("Summarizing…".to_string(), None),
    }
}

pub struct CatchupStrip {
    pub root: GtkBox,
    /// The expanded form's one-line row; hidden when collapsed, leaving only `bar`.
    row: GtkBox,
    status: Label,
    elapsed: Label,
    bar: ProgressBar,
    /// Shown only while collapsed: brings the status row back.
    expand: Button,
    /// Shown only in the error state; re-runs the same catch-up. Transcribed chunks are
    /// cached, so a retry re-transcribes nothing.
    retry: Button,
    collapsed: Rc<Cell<bool>>,
    /// True while the current phase has no measurable progress → the ticker pulses the
    /// bar. Shared with the ticker closure.
    indeterminate: Rc<Cell<bool>>,
    /// Drives the pulse and the elapsed clock for one run.
    tick: RefCell<Option<glib::SourceId>>,
}

impl CatchupStrip {
    pub fn new() -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("catchup-strip");
        root.set_visible(false);

        let row = GtkBox::new(Orientation::Horizontal, 6);
        row.set_margin_start(8);
        row.set_margin_end(4);
        row.set_margin_top(2);
        row.set_margin_bottom(2);

        // Ellipsized so a long phase line shrinks instead of widening the sidebar column.
        let status = Label::new(None);
        status.set_halign(Align::Start);
        status.set_hexpand(true);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let elapsed = Label::new(None);
        elapsed.add_css_class("dim-label");
        elapsed.add_css_class("numeric");

        let retry = Button::with_label("Retry");
        retry.add_css_class("flat");
        retry.set_visible(false);

        let collapse = Button::from_icon_name("pan-up-symbolic");
        collapse.add_css_class("flat");
        collapse.set_tooltip_text(Some("Collapse — the catch-up keeps running"));

        row.append(&status);
        row.append(&elapsed);
        row.append(&retry);
        row.append(&collapse);
        root.append(&row);

        let bar = ProgressBar::new();
        bar.set_hexpand(true);
        bar.set_valign(Align::Center);
        let expand = Button::from_icon_name("pan-down-symbolic");
        expand.add_css_class("flat");
        expand.set_tooltip_text(Some("Show progress"));
        expand.set_visible(false);
        let bar_row = GtkBox::new(Orientation::Horizontal, 4);
        bar_row.append(&bar);
        bar_row.append(&expand);
        root.append(&bar_row);

        let collapsed = Rc::new(Cell::new(false));
        {
            let row = row.clone();
            let bar = bar.clone();
            let expand = expand.clone();
            let collapsed = collapsed.clone();
            collapse.connect_clicked(move |_| {
                collapsed.set(true);
                row.set_visible(false);
                expand.set_visible(true);
                bar.set_tooltip_text(Some("Catch-up running — click to expand"));
            });
        }
        {
            let row = row.clone();
            let bar = bar.clone();
            let collapsed = collapsed.clone();
            expand.connect_clicked(move |b| {
                collapsed.set(false);
                row.set_visible(true);
                b.set_visible(false);
                bar.set_tooltip_text(None);
            });
        }
        Self {
            root,
            row,
            status,
            elapsed,
            bar,
            expand,
            retry,
            collapsed,
            indeterminate: Rc::new(Cell::new(true)),
            tick: RefCell::new(None),
        }
    }

    /// The error-state "Retry" button was clicked → re-run the last catch-up.
    pub fn connect_retry(&self, f: impl Fn() + 'static) {
        self.retry.connect_clicked(move |_| f());
    }

    /// A run started: show the strip expanded, drop any previous error, and start the
    /// pulse + elapsed ticker.
    pub fn begin(&self, what: &str) {
        self.stop_tick();
        self.root.remove_css_class("error");
        self.root.set_visible(true);
        self.expand();
        self.retry.set_visible(false);
        self.bar.set_visible(true);
        self.elapsed.set_text("0:00");
        self.set_status(what, None);

        let bar = self.bar.clone();
        let elapsed = self.elapsed.clone();
        let indeterminate = self.indeterminate.clone();
        let t0 = Instant::now();
        // 400ms: fast enough that the pulse reads as motion, slow enough to stay cheap.
        let id = glib::timeout_add_local(Duration::from_millis(400), move || {
            elapsed.set_text(&fmt_ts(t0.elapsed().as_millis() as i64));
            if indeterminate.get() {
                bar.pulse();
            }
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(id);
    }

    /// Report the running phase (label + bar).
    pub fn show_phase(&self, phase: &CatchupPhase) {
        let (text, fraction) = describe(phase);
        self.set_status(&text, fraction);
    }

    /// The run failed. The strip expands, since it is the only surface that shows the failure.
    pub fn show_error(&self, msg: &str) {
        self.stop_tick();
        self.root.add_css_class("error");
        self.root.set_visible(true);
        self.expand();
        self.indeterminate.set(false);
        // The bar stays up, full and tinted by `.error`, so the collapsed strip keeps a
        // clickable 3px line. Hiding it would leave no height and no way to reach Retry.
        self.bar.set_visible(true);
        self.bar.set_fraction(1.0);
        self.elapsed.set_text("");
        self.status.set_text(&format!("Catch-up failed: {msg}"));
        self.status.set_tooltip_text(Some(msg));
        self.retry.set_visible(true);
    }

    /// The run produced a summary. The panel takes the sidebar, so the strip hides.
    pub fn finish(&self) {
        self.stop_tick();
        self.root.remove_css_class("error");
        self.root.set_visible(false);
        self.retry.set_visible(false);
    }

    fn set_status(&self, text: &str, fraction: Option<f64>) {
        self.status.set_text(text);
        // One ellipsized line: the tooltip keeps the untruncated text reachable.
        self.status.set_tooltip_text(Some(text));
        match fraction {
            Some(f) => {
                self.indeterminate.set(false);
                self.bar.set_fraction(f);
            }
            None => {
                self.indeterminate.set(true);
                self.bar.pulse();
            }
        }
    }

    fn expand(&self) {
        self.collapsed.set(false);
        self.row.set_visible(true);
        self.expand.set_visible(false);
        self.bar.set_tooltip_text(None);
    }

    fn stop_tick(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }
}

impl Default for CatchupStrip {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_fraction_never_leaves_zero_to_one() {
        for phase in [
            CatchupPhase::Fetching(30_000, 10_000),
            CatchupPhase::Fetching(0, 10_000),
            CatchupPhase::Transcribing(0, 0),
            CatchupPhase::Transcribing(5, 3),
        ] {
            if let (_, Some(f)) = describe(&phase) {
                assert!((0.0..=1.0).contains(&f), "{phase:?} gave {f}");
            }
        }
    }
}
