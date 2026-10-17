//! The two surfaces that render the shared watch queue, both rebuilt from the `Queue` change hook
//! and both built with [`row`]:
//!
//! * [`QueuePanel`]: the browse-page header `MenuButton`, whose popover lists the queue while you
//!   pick more videos to add.
//! * [`QueueCard`]: the watch page's card above the recommended grid in the right column. It is
//!   hidden unless something is queued behind the playing video.
//!
//! Both offer reorder (▲▼), remove (×), and click-a-row to play it now.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, Image, Label, ListBox, MenuButton, Orientation, Picture, Popover,
    PolicyType, ScrolledWindow, SelectionMode,
};

use crate::models::ResultItem;
use crate::playback::Queue;
use crate::runtime::AsyncRuntime;
use super::result_row::{fetch_bytes, ThumbCache};

/// Queue-card thumbnail size: 16:9, small enough to leave the title most of a ~360 px sidebar
/// column.
const THUMB: (i32, i32) = (96, 54);

pub struct QueuePanel {
    /// The header button to place in a toolbar. Owns the popover.
    pub button: MenuButton,
}

impl QueuePanel {
    /// `on_play(item)` plays a video chosen from the panel (it's already been removed from
    /// the queue). The panel subscribes to `queue`'s change hook and rebuilds itself.
    pub fn new(queue: Rc<RefCell<Queue>>, on_play: Rc<dyn Fn(ResultItem)>) -> Self {
        let count = Label::builder().css_classes(["queue-count"]).build();
        let header_box = GtkBox::new(Orientation::Horizontal, 4);
        header_box.append(&Image::from_icon_name("view-list-symbolic"));
        header_box.append(&count);
        let button = MenuButton::builder().tooltip_text("Queue").build();
        button.set_child(Some(&header_box));

        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::None);
        let scroller = ScrolledWindow::builder()
            .min_content_width(340)
            .min_content_height(120)
            .max_content_height(480)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        let popover = Popover::builder().child(&scroller).build();
        button.set_popover(Some(&popover));

        let rebuild: crate::playback::queue::OnChange = {
            let list = list.clone();
            let count = count.clone();
            let queue = queue.clone();
            let on_play = on_play.clone();
            let popover = popover.clone();
            Rc::new(move |items: &[ResultItem]| {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                count.set_text(&items.len().to_string());
                count.set_visible(!items.is_empty());
                if items.is_empty() {
                    let empty = Label::builder()
                        .label("Queue is empty")
                        .css_classes(["dim-label"])
                        .margin_top(16)
                        .margin_bottom(16)
                        .build();
                    list.append(&empty);
                    return;
                }
                for (i, item) in items.iter().enumerate() {
                    list.append(&row(i, item, &queue, &on_play, Some(&popover), items.len(), None));
                }
            })
        };

        rebuild(&queue.borrow().items());
        queue.borrow_mut().add_on_change(rebuild);
        Self { button }
    }
}

/// The watch page's "Queue" card: the same list as the header popover, always-on in the
/// right column above the recommended grid.
pub struct QueueCard {
    pub root: GtkBox,
}

impl QueueCard {
    /// `on_play(item)` plays a row (it stays in the queue, moved to the front). `rt` loads
    /// the row thumbnails; `queue`'s change hook drives every rebuild.
    pub fn new(queue: Rc<RefCell<Queue>>, on_play: Rc<dyn Fn(ResultItem)>, rt: AsyncRuntime) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("card");
        root.add_css_class("queue-card");
        root.set_margin_start(8);
        root.set_margin_end(8);
        root.set_margin_top(8);
        root.set_visible(false);

        let head = GtkBox::new(Orientation::Horizontal, 6);
        head.set_margin_start(10);
        head.set_margin_end(6);
        head.set_margin_top(8);
        head.set_margin_bottom(4);
        let title = Label::builder().label("Queue").css_classes(["heading"]).xalign(0.0).build();
        let count = Label::builder().css_classes(["dim-label", "caption"]).xalign(0.0).build();
        count.set_valign(Align::Baseline);
        let clear = Button::builder()
            .label("Clear")
            .css_classes(["flat"])
            .halign(Align::End)
            .hexpand(true)
            .tooltip_text("Empty the queue")
            .build();
        head.append(&title);
        head.append(&count);
        head.append(&clear);
        root.append(&head);

        // The card paints the surface; the stylesheet makes the list and its scroller
        // transparent so their own view background doesn't sit as a square block inside
        // the card's rounded corners.
        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::None);
        // Bounded so a long queue can't push the recommended grid off the column; the
        // card scrolls internally past that height.
        let scroller = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .max_content_height(300)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        scroller.set_margin_bottom(6);
        root.append(&scroller);

        {
            let queue = queue.clone();
            clear.connect_clicked(move |_| queue.borrow_mut().clear());
        }

        // Thumbnails are cached across rebuilds: the list is rebuilt from scratch on every
        // queue mutation (reorder, remove, advance), so an uncached card would re-fetch
        // every visible thumbnail on each ▲ press.
        let thumbs: ThumbCache = Default::default();
        let rebuild: crate::playback::queue::OnChange = {
            let root = root.clone();
            let list = list.clone();
            let count = count.clone();
            let queue = queue.clone();
            let on_play = on_play.clone();
            Rc::new(move |items: &[ResultItem]| {
                // Index 0 is the video already playing on this page, so a 1-item queue has
                // nothing up next and the card would be pure noise beside the video.
                root.set_visible(items.len() > 1);
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                if items.len() < 2 {
                    return;
                }
                count.set_text(&format!("· {} videos", items.len()));
                for (i, item) in items.iter().enumerate() {
                    list.append(&row(
                        i,
                        item,
                        &queue,
                        &on_play,
                        None,
                        items.len(),
                        Some((&rt, &thumbs)),
                    ));
                }
            })
        };

        rebuild(&queue.borrow().items());
        queue.borrow_mut().add_on_change(rebuild);
        Self { root }
    }
}

/// One queue row: clickable title/channel (play now) + ▲ ▼ × controls.
///
/// `popover` is the host to dismiss on play (the header panel; `None` for the always-open
/// card). `thumbs` adds a leading thumbnail, loaded through the given cache. The header
/// popover passes `None` and stays text-only.
fn row(
    i: usize,
    item: &ResultItem,
    queue: &Rc<RefCell<Queue>>,
    on_play: &Rc<dyn Fn(ResultItem)>,
    popover: Option<&Popover>,
    len: usize,
    thumbs: Option<(&AsyncRuntime, &ThumbCache)>,
) -> GtkBox {
    let row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(6).build();
    // The front item is the one currently playing; mark it.
    if i == 0 {
        row.add_css_class("queue-current");
        let dot = gtk::Image::from_icon_name("media-playback-start-symbolic");
        dot.add_css_class("accent");
        dot.set_valign(gtk::Align::Center);
        row.append(&dot);
    }
    if let Some((rt, cache)) = thumbs {
        row.append(&thumbnail(item, rt, cache));
    }

    let text = GtkBox::new(Orientation::Vertical, 0);
    text.set_hexpand(true);
    let title = Label::builder()
        .label(item.title())
        .xalign(0.0)
        .ellipsize(EllipsizeMode::End)
        .max_width_chars(30)
        .css_classes(["heading"])
        .build();
    let chan = Label::builder()
        .label(item.channel())
        .xalign(0.0)
        .ellipsize(EllipsizeMode::End)
        .max_width_chars(30)
        .css_classes(["dim-label", "caption"])
        .build();
    text.append(&title);
    text.append(&chan);
    let play = Button::builder().child(&text).has_frame(false).hexpand(true).build();
    play.set_tooltip_text(Some("Play now"));
    {
        let queue = queue.clone();
        let on_play = on_play.clone();
        let popover = popover.cloned();
        play.connect_clicked(move |_| {
            if let Some(p) = &popover {
                p.popdown();
            }
            // Move it to the front (it stays in the queue) and play it. Drop the borrow
            // before on_play (it may, in turn, touch shared state).
            let chosen = queue.borrow_mut().move_to_front(i);
            if let Some(item) = chosen {
                on_play(item);
            }
        });
    }
    row.append(&play);

    // Reorder and remove apply to upcoming items only. The front item is the one playing,
    // so it has no controls and the item below it cannot move up into its slot.
    if i >= 1 {
        let up = icon_button("go-up-symbolic", "Move up", i > 1);
        let down = icon_button("go-down-symbolic", "Move down", i + 1 < len);
        let rm = icon_button("window-close-symbolic", "Remove", true);
        {
            let queue = queue.clone();
            up.connect_clicked(move |_| queue.borrow_mut().move_up(i));
        }
        {
            let queue = queue.clone();
            down.connect_clicked(move |_| queue.borrow_mut().move_down(i));
        }
        {
            let queue = queue.clone();
            rm.connect_clicked(move |_| queue.borrow_mut().remove(i));
        }
        let controls = GtkBox::new(Orientation::Horizontal, 0);
        // Compacted by the stylesheet inside the card, where the column is narrow enough
        // that three default-width icon buttons cost the title a third of its space.
        controls.add_css_class("queue-ctl");
        controls.set_valign(gtk::Align::Center);
        controls.append(&up);
        controls.append(&down);
        controls.append(&rm);
        row.append(&controls);
    }
    row
}

/// A row thumbnail: painted from `cache` when present, else fetched.
///
/// The image is decoded at display size. A `GtkPicture`'s natural width is its paintable's
/// intrinsic width and `set_size_request` only raises the minimum, so a 320x180 texture would
/// claim most of a ~360 px column.
///
/// The fetch writes into the `Picture` created here. A rebuild drops that row, so a late fetch
/// paints an unparented widget and cannot reach the row that replaced it.
fn thumbnail(item: &ResultItem, rt: &AsyncRuntime, cache: &ThumbCache) -> Picture {
    let pic = Picture::new();
    pic.set_size_request(THUMB.0, THUMB.1);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_valign(Align::Center);
    pic.set_halign(Align::Start);
    pic.add_css_class("queue-thumb");
    let key = item.video_id();
    if let Some(texture) = cache.borrow().get(&key) {
        pic.set_paintable(Some(texture));
        return pic;
    }
    let Some(url) = item.thumbnail_url() else { return pic };
    let cache = cache.clone();
    let target = pic.clone();
    rt.spawn(
        async move { fetch_bytes(url).await },
        move |bytes: Option<Vec<u8>>| {
            let Some(bytes) = bytes else { return };
            let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_owned(bytes));
            let scaled = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                &stream,
                THUMB.0,
                THUMB.1,
                true, // fit inside the box; the source is already 16:9
                gtk::gio::Cancellable::NONE,
            );
            let Ok(scaled) = scaled else { return };
            let texture = gtk::gdk::Texture::for_pixbuf(&scaled);
            cache.borrow_mut().insert(key, texture.clone());
            target.set_paintable(Some(&texture));
        },
    );
    pic
}

fn icon_button(icon: &str, tip: &str, sensitive: bool) -> Button {
    let b = Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.set_sensitive(sensitive);
    b
}
