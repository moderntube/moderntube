//! A feed's Shorts shelf: a heading, one row of portrait Shorts cards, and Show more.
use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, GestureClick, Image, Label, Orientation, Picture};

use crate::models::ResultItem;
use crate::runtime::AsyncRuntime;
use super::result_row::{fetch_bytes, ThumbCache};

/// Narrowest a Shorts card gets before the shelf drops a column (YouTube: 216 px at 1280).
const SHORT_CARD_MIN: i32 = 200;
const MAX_SHELF_COLUMNS: usize = 6;
const CARD_SPACING: i32 = 16;
/// The shelf's own horizontal padding (`.shorts-shelf`, 6 px each side).
const SHELF_PADDING: i32 = 12;

/// How many Shorts fit across `width` px.
pub fn shelf_columns(width: i32) -> usize {
    ((width / SHORT_CARD_MIN).max(2) as usize).min(MAX_SHELF_COLUMNS)
}

struct ShortCard {
    root: GtkBox,
    thumb: Picture,
    /// Caps the thumbnail at its 2:3 height whatever the image's own aspect.
    clamp: adw::Clamp,
    title: Label,
    views: Label,
    key: Rc<RefCell<String>>,
    bound: Rc<RefCell<Option<ResultItem>>>,
}

impl ShortCard {
    fn new(on_activate: Rc<dyn Fn(ResultItem)>) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 6);
        root.add_css_class("short-card");
        root.set_hexpand(true);
        let thumb = Picture::builder().content_fit(gtk::ContentFit::Cover).hexpand(true).build();
        thumb.add_css_class("card");
        let clamp = adw::Clamp::builder().orientation(Orientation::Vertical).child(&thumb).build();
        let title = Label::builder()
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(1)
            .css_classes(["heading"])
            .build();
        let views = Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(1)
            .css_classes(["dim-label", "caption"])
            .build();
        root.append(&clamp);
        root.append(&title);
        root.append(&views);
        let bound: Rc<RefCell<Option<ResultItem>>> = Rc::default();
        let click = GestureClick::new();
        {
            let bound = bound.clone();
            click.connect_released(move |_, _, _, _| {
                if let Some(item) = bound.borrow().clone() {
                    on_activate(item);
                }
            });
        }
        root.add_controller(click);
        root.set_cursor_from_name(Some("pointer"));
        Self { root, thumb, clamp, title, views, key: Rc::default(), bound }
    }

    fn bind(&self, item: &ResultItem, card_width: i32, rt: &AsyncRuntime, cache: &ThumbCache) {
        self.bound.replace(Some(item.clone()));
        let height = card_width * 3 / 2;
        self.thumb.set_size_request(-1, height);
        self.clamp.set_maximum_size(height);
        self.clamp.set_tightening_threshold(height);
        self.title.set_text(&item.title());
        self.views.set_text(item.views().as_deref().unwrap_or(""));
        let key = format!("short:{}", item.video_id());
        self.key.replace(key.clone());
        if let Some(texture) = cache.borrow().get(&key) {
            self.thumb.set_paintable(Some(texture));
            return;
        }
        self.thumb.set_paintable(gtk::gdk::Paintable::NONE);
        let Some(url) = item.avatar_url() else { return };
        let (thumb, cache, current) = (self.thumb.clone(), cache.clone(), self.key.clone());
        rt.spawn(async move { fetch_bytes(url).await }, move |bytes: Option<Vec<u8>>| {
            let Some(bytes) = bytes else { return };
            let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) else {
                return;
            };
            cache.borrow_mut().insert(key.clone(), texture.clone());
            if *current.borrow() == key {
                thumb.set_paintable(Some(&texture));
            }
        });
    }
}

/// A recycled shelf row; [`ShortsShelf::bind`] fills it for one `ShortsShelf` item.
pub struct ShortsShelf {
    pub root: GtkBox,
    header: GtkBox,
    title: Label,
    grid: GtkBox,
    more: Button,
    cards: RefCell<Vec<ShortCard>>,
    on_activate: Rc<dyn Fn(ResultItem)>,
    bound: Rc<RefCell<Option<ResultItem>>>,
    /// Re-lays the shelf after Show more / Show less changes its height.
    relayout: RefCell<Option<Rc<dyn Fn()>>>,
}

impl ShortsShelf {
    pub fn new(on_activate: Rc<dyn Fn(ResultItem)>) -> Rc<Self> {
        let root = GtkBox::new(Orientation::Vertical, 8);
        root.add_css_class("shorts-shelf");
        let header = GtkBox::new(Orientation::Horizontal, 8);
        let icon = Image::from_icon_name("media-playback-start-symbolic");
        icon.add_css_class("shorts-shelf-icon");
        let title = Label::builder().xalign(0.0).css_classes(["title-3"]).build();
        header.append(&icon);
        header.append(&title);
        let grid = GtkBox::new(Orientation::Vertical, 12);
        let more = Button::builder().halign(Align::Center).css_classes(["pill", "shorts-shelf-more"]).build();
        root.append(&header);
        root.append(&grid);
        root.append(&more);
        let shelf = Rc::new(Self {
            root,
            header,
            title,
            grid,
            more,
            cards: RefCell::default(),
            on_activate,
            bound: Rc::default(),
            relayout: RefCell::default(),
        });
        let weak = Rc::downgrade(&shelf);
        shelf.more.connect_clicked(move |_| {
            let Some(shelf) = weak.upgrade() else { return };
            if let Some(item) = shelf.bound.borrow().as_ref() {
                item.set_shelf_expanded(!item.shelf_expanded());
                tracing::info!("[shelf] {} expanded={}", item.title(), item.shelf_expanded());
            }
            let relayout = shelf.relayout.borrow().clone();
            if let Some(f) = relayout {
                f();
            }
        });
        shelf
    }

    pub fn bind(self: &Rc<Self>, item: &ResultItem, width: i32, rt: &AsyncRuntime, cache: &ThumbCache) {
        self.bound.replace(Some(item.clone()));
        self.title.set_text(&item.title());
        self.header.set_visible(!item.is_shelf_grid());
        if item.is_shelf_grid() {
            self.root.add_css_class("shorts-grid");
        } else {
            self.root.remove_css_class("shorts-grid");
        }
        let shorts = item.shelf_items();
        let cols = shelf_columns(width);
        let card_width = (width - SHELF_PADDING - CARD_SPACING * (cols as i32 - 1)) / cols as i32;
        let expanded = item.shelf_expanded();
        let shown = if expanded { shorts.len() } else { shorts.len().min(cols) };
        self.more.set_visible(shorts.len() > cols && !item.is_shelf_grid());
        self.more.set_label(if expanded { "Show less" } else { "Show more" });

        let mut cards = self.cards.borrow_mut();
        while cards.len() < shown {
            cards.push(ShortCard::new(self.on_activate.clone()));
        }
        while let Some(row) = self.grid.first_child() {
            let row = row.downcast::<GtkBox>().expect("shelf rows are boxes");
            while let Some(card) = row.first_child() {
                row.remove(&card);
            }
            self.grid.remove(&row);
        }
        for (start, chunk) in shorts[..shown].chunks(cols).enumerate().map(|(r, c)| (r * cols, c)) {
            let row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(CARD_SPACING).homogeneous(true).build();
            for slot in 0..cols {
                if let Some(short) = chunk.get(slot) {
                    let card = &cards[start + slot];
                    card.bind(short, card_width, rt, cache);
                    row.append(&card.root);
                } else {
                    row.append(&GtkBox::new(Orientation::Vertical, 0));
                }
            }
            self.grid.append(&row);
        }
    }

    pub fn set_relayout(&self, f: impl Fn() + 'static) {
        self.relayout.replace(Some(Rc::new(f)));
    }
}
