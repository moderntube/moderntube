//! Scrollable results grid: a `GtkListView` of rows over a flat `GListStore<ResultItem>`.
//! A row is either one line of `ResultRow` cards or a full-width Shorts shelf, and the
//! rows re-form when the width changes the column count. Fires `on_activate` when a card
//! is clicked.
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk::gio;
use gtk::glib::{self, BoxedAnyObject};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{
    Align, Label, ListItem, ListView, NoSelection, Orientation, Overlay, ScrolledWindow,
    SignalListItemFactory, Spinner,
};

use crate::models::ResultItem;
use crate::playback::QueueAction;
use crate::runtime::AsyncRuntime;
use super::hover_preview::HoverPreview;
use super::result_row::{ResultRow, ThumbCache};
use super::shorts_shelf::ShortsShelf;

/// Debounce before a steady hover starts a preview stream; a pointer passing over a card
/// stays under it.
const HOVER_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(600);

/// A repeat activate of the same video inside this window is the same click: the preview
/// surface and the card under it both receive it, about 180 ms apart.
const ACTIVATE_DEDUPE: Duration = Duration::from_millis(500);

/// A card's minimum width: the 320 px thumbnail plus the card's 6 px margins.
const CARD_MIN_WIDTH: i32 = 332;
const MAX_COLUMNS: usize = 4;

fn columns_for(width: i32) -> usize {
    ((width / CARD_MIN_WIDTH).max(1) as usize).min(MAX_COLUMNS)
}

/// One displayed row, as indexes into the flat item store.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowSpec {
    /// The page header set with [`ResultsView::set_header`]; it scrolls with the rows.
    Header,
    Cards(Vec<u32>),
    Shelf(u32),
}

/// Lays `is_shelf` (one entry per stored item) into rows of `cols` cards. A shelf waits
/// for the row in progress to fill, as the web client's rich grid places sections.
fn layout_rows(is_shelf: &[bool], cols: usize, show_shelves: bool) -> Vec<RowSpec> {
    let mut rows = Vec::new();
    let mut cards: Vec<u32> = Vec::new();
    let mut shelves: Vec<u32> = Vec::new();
    let flush = |cards: &mut Vec<u32>, shelves: &mut Vec<u32>, rows: &mut Vec<RowSpec>| {
        if !cards.is_empty() {
            rows.push(RowSpec::Cards(std::mem::take(cards)));
        }
        rows.extend(shelves.drain(..).filter(|_| show_shelves).map(RowSpec::Shelf));
    };
    for (i, &shelf) in is_shelf.iter().enumerate() {
        let i = i as u32;
        if shelf {
            shelves.push(i);
            if cards.is_empty() {
                flush(&mut cards, &mut shelves, &mut rows);
            }
            continue;
        }
        cards.push(i);
        if cards.len() == cols {
            flush(&mut cards, &mut shelves, &mut rows);
        }
    }
    flush(&mut cards, &mut shelves, &mut rows);
    rows
}

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct FeedRow {
        pub cards: gtk::Box,
        pub slots: RefCell<Vec<ResultRow>>,
        pub shelf: RefCell<Option<Rc<ShortsShelf>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FeedRow {
        const NAME: &'static str = "MtFeedRow";
        type Type = super::FeedRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for FeedRow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(Orientation::Vertical);
            self.cards.set_homogeneous(true);
            obj.append(&self.cards);
        }
    }
    impl WidgetImpl for FeedRow {}
    impl BoxImpl for FeedRow {}
}

glib::wrapper! {
    pub struct FeedRow(ObjectSubclass<row_imp::FeedRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

/// What the store, the rows and the signal handlers share.
struct Grid {
    store: gio::ListStore,
    rows: gio::ListStore,
    specs: RefCell<Vec<RowSpec>>,
    width: Cell<i32>,
    show_shelves: Cell<bool>,
    /// One column of thumbnail-left cards (the watch-page sidebar).
    compact: Cell<bool>,
    header: RefCell<Option<gtk::Widget>>,
}

impl Grid {
    fn columns(&self) -> usize {
        if self.compact.get() {
            return 1;
        }
        match self.width.get() {
            0 => 3,
            w => columns_for(w),
        }
    }

    /// Re-forms the rows, replacing only those from the first one that changed.
    fn relayout(&self) {
        let is_shelf: Vec<bool> = (0..self.store.n_items())
            .map(|i| self.store.item(i).and_downcast::<ResultItem>().is_some_and(|it| it.is_shorts_shelf()))
            .collect();
        let mut new = layout_rows(&is_shelf, self.columns(), self.show_shelves.get());
        // The header has a minimum width, so it joins once the viewport has one.
        if self.header.borrow().is_some() && self.width.get() > 0 {
            new.insert(0, RowSpec::Header);
        }
        let mut specs = self.specs.borrow_mut();
        let same = specs.iter().zip(&new).take_while(|(a, b)| a == b).count();
        if same == specs.len() && same == new.len() {
            return;
        }
        let added: Vec<BoxedAnyObject> = new[same..].iter().cloned().map(BoxedAnyObject::new).collect();
        self.rows.splice(same as u32, (specs.len() - same) as u32, &added);
        *specs = new;
    }

    /// Re-binds the row showing store item `idx` as a shelf.
    fn rebind_shelf(&self, idx: u32) {
        let specs = self.specs.borrow();
        if let Some(pos) = specs.iter().position(|s| *s == RowSpec::Shelf(idx)) {
            self.rows.splice(pos as u32, 1, &[BoxedAnyObject::new(specs[pos].clone())]);
        }
    }

    fn item(&self, i: u32) -> Option<ResultItem> {
        self.store.item(i).and_downcast::<ResultItem>()
    }
}

pub struct ResultsView {
    pub root: ScrolledWindow,
    /// `root` plus the loading spinner and the empty-state label: the widget callers mount.
    pub outer: Overlay,
    spinner: Spinner,
    /// Shown when a fetch succeeded and returned nothing.
    empty: Label,
    grid: Rc<Grid>,
    thumb_cache: ThumbCache,
    /// The app-wide hover preview. Replacing the results stops it: the old card goes away
    /// with no `leave` event.
    preview: Rc<HoverPreview>,
    /// Keys already in the store; feeds repeat videos across continuation pages.
    seen: RefCell<HashSet<String>>,
}

impl ResultsView {
    /// `on_activate(item)` plays a card; `on_channel(id, name)` opens a channel. `preview`
    /// is shared by every `ResultsView`, so one preview plays at a time.
    pub fn new(
        rt: AsyncRuntime,
        preview: Rc<HoverPreview>,
        on_activate: impl Fn(ResultItem) + 'static,
        on_channel: impl Fn(String, String) + 'static,
        on_queue: impl Fn(ResultItem, QueueAction) + 'static,
        on_playlist: impl Fn(String, String) + 'static,
    ) -> Self {
        let grid = Rc::new(Grid {
            store: gio::ListStore::new::<ResultItem>(),
            rows: gio::ListStore::new::<BoxedAnyObject>(),
            specs: RefCell::default(),
            width: Cell::new(0),
            show_shelves: Cell::new(true),
            compact: Cell::new(false),
            header: RefCell::default(),
        });
        let thumb_cache: ThumbCache = Default::default();

        // A second SABR session for the same video is refused (GATE-003), so a repeat activate
        // inside ACTIVATE_DEDUPE collapses into the first.
        let on_activate: Rc<dyn Fn(ResultItem)> = {
            let open = on_activate;
            let last: RefCell<Option<(String, Instant)>> = RefCell::new(None);
            Rc::new(move |item: ResultItem| {
                let (id, now) = (item.video_id(), Instant::now());
                let duplicate = last
                    .borrow()
                    .as_ref()
                    .is_some_and(|(prev, at)| *prev == id && now - *at < ACTIVATE_DEDUPE);
                if duplicate {
                    return;
                }
                *last.borrow_mut() = Some((id, now));
                open(item);
            })
        };
        let on_channel: Rc<dyn Fn(String, String)> = Rc::new(on_channel);
        let on_playlist: Rc<dyn Fn(String, String)> = Rc::new(on_playlist);
        let on_queue: Rc<dyn Fn(ResultItem, QueueAction)> = Rc::new(on_queue);

        // A video card resumes from its hover preview's position when that preview is playing.
        let open_card: Rc<dyn Fn(ResultItem)> = {
            let (preview, on_activate, on_channel) = (preview.clone(), on_activate.clone(), on_channel.clone());
            Rc::new(move |item: ResultItem| {
                preview.stop();
                if item.is_channel() {
                    if let Some(id) = item.channel_id() {
                        on_channel(id, item.title());
                    }
                } else if item.is_playlist() {
                    on_playlist(item.playlist_id(), item.title());
                } else {
                    preview.handoff_position(&item.video_id(), item.live());
                    preview.stop();
                    on_activate(item);
                }
            })
        };

        let make_card: Rc<dyn Fn() -> ResultRow> = {
            let preview = preview.clone();
            let on_activate = on_activate.clone();
            let grid = grid.clone();
            Rc::new(move || {
                let row = ResultRow::default();
                row.add_css_class("feed-card");
                if grid.compact.get() {
                    row.set_compact();
                }
                let oc = on_channel.clone();
                row.connect_channel_clicked(move |id, name| oc(id, name));
                let oq = on_queue.clone();
                row.connect_queue(move |item, action| oq(item, action));
                let click = gtk::GestureClick::new();
                {
                    let (row, open_card) = (row.clone(), open_card.clone());
                    click.connect_released(move |_, _, _, _| {
                        if let Some(item) = row.item() {
                            open_card(item);
                        }
                    });
                }
                row.add_controller(click);
                if preview.enabled() {
                    let (p_show, p_hide, p_keep) =
                        (preview.clone(), preview.clone(), preview.clone());
                    let anchor = row.clone();
                    let on_act = on_activate.clone();
                    row.connect_hover_preview(
                        HOVER_PREVIEW_DEBOUNCE,
                        // A channel or playlist card's id is a channel/playlist id; a stream for it hangs.
                        move |item| {
                            if item.is_channel() || item.is_playlist() {
                                return;
                            }
                            p_show.show(&anchor.thumbnail_widget(), item, on_act.clone());
                        },
                        move |id| p_hide.hide(&id),
                        move |id| p_keep.keep_alive(&id),
                    );
                }
                row
            })
        };

        // The hover-test hook, read when the grid is built, also previews channel and playlist
        // cards, whose prepare fails.
        let hover_test: Rc<Vec<(u32, u32)>> = Rc::new(
            crate::hooks::get("hover-test")
                .map(|spec| {
                    spec.split(',')
                        .filter_map(|part| {
                            let (secs, idx) = part.split_once(':')?;
                            Some((secs.trim().parse().ok()?, idx.trim().parse().ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let hover_fired: Rc<RefCell<HashSet<u32>>> = Rc::default();
        let fire_hover_test = {
            let (preview, on_activate) = (preview.clone(), on_activate.clone());
            move |idx: u32, row: &ResultRow, result: &ResultItem| {
                for &(secs, at) in hover_test.iter() {
                    if at != idx || !hover_fired.borrow_mut().insert(idx) {
                        continue;
                    }
                    let (p, row, result) = (preview.clone(), row.clone(), result.clone());
                    let on_act = on_activate.clone();
                    let activate_at = crate::hooks::get("hover-activate").and_then(|v| v.parse().ok());
                    glib::timeout_add_seconds_local_once(secs, move || {
                        tracing::info!(
                            "[hover-test] previewing card {idx}: {} (live={})",
                            result.video_id(),
                            result.live()
                        );
                        p.show(&row.thumbnail_widget(), result.clone(), on_act.clone());
                        if let Some(after) = activate_at {
                            let (p2, on_act2) = (p.clone(), on_act.clone());
                            glib::timeout_add_seconds_local_once(after, move || {
                                tracing::info!("[hover-test] activating {} from the preview", result.video_id());
                                p2.handoff_position(&result.video_id(), result.live());
                                p2.stop();
                                on_act2(result);
                            });
                        }
                    });
                }
            }
        };

        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let item = obj.downcast_ref::<ListItem>().unwrap();
            item.set_activatable(false);
            item.set_child(Some(&glib::Object::new::<FeedRow>()));
        });
        {
            let (grid, cache) = (grid.clone(), thumb_cache.clone());
            factory.connect_bind(move |_, obj| {
                let list_item = obj.downcast_ref::<ListItem>().unwrap();
                let spec = list_item.item().and_downcast::<BoxedAnyObject>().unwrap();
                let spec = spec.borrow::<RowSpec>().clone();
                let row = list_item.child().and_downcast::<FeedRow>().unwrap();
                let imp = row.imp();
                let header = grid.header.borrow().clone();
                if let Some(h) = header.as_ref().filter(|h| h.parent().as_ref() == Some(row.upcast_ref())) {
                    if spec != RowSpec::Header {
                        row.remove(h);
                    }
                }
                match spec {
                    RowSpec::Header => {
                        imp.cards.set_visible(false);
                        if let Some(shelf) = imp.shelf.borrow().as_ref() {
                            shelf.root.set_visible(false);
                        }
                        if let Some(h) = header {
                            if h.parent().as_ref() != Some(row.upcast_ref()) {
                                if let Some(old) = h.parent().and_downcast::<gtk::Box>() {
                                    old.remove(&h);
                                }
                                row.append(&h);
                            }
                        }
                    }
                    RowSpec::Cards(indexes) => {
                        if let Some(shelf) = imp.shelf.borrow().as_ref() {
                            shelf.root.set_visible(false);
                        }
                        imp.cards.set_visible(true);
                        let cols = grid.columns();
                        let mut slots = imp.slots.borrow_mut();
                        while slots.len() < cols {
                            let card = make_card();
                            imp.cards.append(&card);
                            slots.push(card);
                        }
                        for (slot, card) in slots.iter().enumerate() {
                            card.set_visible(slot < cols);
                            let idx = indexes.get(slot).copied();
                            match idx.and_then(|i| grid.item(i)).zip(idx) {
                                Some((result, idx)) => {
                                    card.bind(&result, &rt, &cache);
                                    card.set_opacity(1.0);
                                    card.set_can_target(true);
                                    fire_hover_test(idx, card, &result);
                                }
                                // An unfilled slot of the last row keeps its width.
                                None => {
                                    card.set_opacity(0.0);
                                    card.set_can_target(false);
                                }
                            }
                        }
                    }
                    RowSpec::Shelf(idx) => {
                        imp.cards.set_visible(false);
                        let Some(result) = grid.item(idx) else { return };
                        let shelf = imp
                            .shelf
                            .borrow_mut()
                            .get_or_insert_with(|| {
                                let shelf = ShortsShelf::new(on_activate.clone());
                                row.append(&shelf.root);
                                shelf
                            })
                            .clone();
                        shelf.root.set_visible(true);
                        let width = grid.width.get();
                        shelf.bind(&result, width, &rt, &cache);
                        let (weak, rt, cache) = (Rc::downgrade(&shelf), rt.clone(), cache.clone());
                        shelf.set_relayout(move || {
                            if let Some(shelf) = weak.upgrade() {
                                shelf.bind(&result, width, &rt, &cache);
                            }
                        });
                    }
                }
            });
        }

        let list = ListView::new(Some(NoSelection::new(Some(grid.rows.clone()))), Some(factory));
        list.add_css_class("feed-list");
        list.set_overflow(gtk::Overflow::Hidden);

        let root = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overflow(gtk::Overflow::Hidden)
            .child(&list)
            .build();
        // Scrolling hides the preview so it doesn't hover over the wrong card mid-scroll.
        {
            let preview = preview.clone();
            root.vadjustment().connect_value_changed(move |_| preview.hide_on_scroll());
        }
        // The horizontal page size is the viewport width.
        {
            let grid = grid.clone();
            root.hadjustment().connect_changed(move |a| {
                let width = a.page_size() as i32;
                if width <= 0 || width == grid.width.get() {
                    return;
                }
                let before_width = grid.width.get();
                let before = (grid.columns(), crate::widgets::shorts_shelf::shelf_columns(grid.width.get()));
                grid.width.set(width);
                let after = (grid.columns(), crate::widgets::shorts_shelf::shelf_columns(width));
                if before != after || before_width == 0 {
                    grid.specs.borrow_mut().clear();
                    grid.rows.remove_all();
                    grid.relayout();
                }
            });
        }
        let spinner = Spinner::builder()
            .halign(Align::Center)
            .valign(Align::Center)
            .width_request(32)
            .height_request(32)
            .visible(false)
            .build();
        let empty = Label::builder()
            .label("Nothing to show here")
            .halign(Align::Center)
            .valign(Align::Center)
            .justify(gtk::Justification::Center)
            .wrap(true)
            .visible(false)
            .build();
        empty.add_css_class("dim-label");
        empty.add_css_class("title-2");
        let outer = Overlay::new();
        outer.set_child(Some(&root));
        outer.add_overlay(&spinner);
        outer.add_overlay(&empty);
        Self {
            root,
            outer,
            spinner,
            empty,
            grid,
            thumb_cache,
            preview,
            seen: RefCell::new(HashSet::new()),
        }
    }

    /// Shows the loading spinner while a fetch runs; `set_items` clears it.
    pub fn set_loading(&self, loading: bool) {
        self.spinner.set_visible(loading);
        self.spinner.set_spinning(loading);
        self.sync_empty();
    }

    /// The message shown when a fetch returns nothing.
    pub fn set_empty_text(&self, text: &str) {
        self.empty.set_text(text);
    }

    /// One column of thumbnail-left cards, as the web client's watch-page sidebar.
    pub fn set_compact(&self) {
        self.grid.compact.set(true);
        self.grid.specs.borrow_mut().clear();
        self.grid.rows.remove_all();
        self.grid.relayout();
    }

    /// A widget shown above the rows that scrolls away with them, as a channel page header.
    pub fn set_header(&self, header: &impl IsA<gtk::Widget>) {
        self.grid.header.replace(Some(header.clone().upcast()));
        self.grid.relayout();
    }

    /// Adds Shorts to the last shelf in the results, or appends them as a new shelf.
    pub fn append_to_shelf(&self, shorts: Vec<ResultItem>) {
        let store = &self.grid.store;
        let last = (0..store.n_items()).rev().find(|&i| self.grid.item(i).is_some_and(|it| it.is_shorts_shelf()));
        match last.and_then(|i| self.grid.item(i).map(|it| (i, it))) {
            Some((idx, shelf)) => {
                shelf.extend_shelf(shorts);
                self.grid.rebind_shelf(idx);
            }
            None => self.append_items([ResultItem::shorts_grid(shorts)]),
        }
    }

    /// Shows or hides the feed's Shorts shelves; the items stay, so this takes effect at once.
    pub fn set_show_shelves(&self, on: bool) {
        if self.grid.show_shelves.replace(on) != on {
            self.grid.relayout();
            self.sync_empty();
        }
    }

    /// No rows and no fetch in flight: the fetch returned nothing.
    fn sync_empty(&self) {
        let show = self.grid.store.n_items() == 0 && !self.spinner.is_visible();
        if show && !self.empty.is_visible() {
            tracing::info!("[results] empty state shown");
        }
        self.empty.set_visible(show);
    }

    /// Replaces the results (a new search or feed); the thumbnail cache and the dedup set
    /// start over with them.
    pub fn set_items(&self, items: impl IntoIterator<Item = ResultItem>) {
        self.preview.stop();
        self.thumb_cache.borrow_mut().clear();
        self.grid.store.remove_all();
        self.grid.specs.borrow_mut().clear();
        self.grid.rows.remove_all();
        self.seen.borrow_mut().clear();
        self.append_items(items);
        // Last: the empty state appears once the spinner drops, so the items must be in first.
        self.set_loading(false);
    }

    /// Append more results (a continuation page), skipping any video already shown.
    pub fn append_items(&self, items: impl IntoIterator<Item = ResultItem>) {
        let mut seen = self.seen.borrow_mut();
        for it in items {
            // A channel card has an empty video id; it keys on its channel id.
            let key = if it.is_channel() {
                format!("ch:{}", it.channel_id().unwrap_or_default())
            } else if it.is_playlist() {
                format!("pl:{}", it.playlist_id())
            } else if it.is_shorts_shelf() {
                format!("shelf:{}", it.video_id())
            } else {
                it.video_id()
            };
            if seen.insert(key) {
                self.grid.store.append(&it);
            }
        }
        drop(seen);
        self.grid.relayout();
        self.sync_empty();
    }

    /// Fires `f` within one viewport of the bottom, or while the content is shorter than
    /// the viewport. It fires repeatedly; the caller dedups.
    pub fn connect_load_more<F: Fn() + 'static>(&self, f: F) {
        let f = std::rc::Rc::new(f);
        let adj = self.root.vadjustment();
        // Scrolled near the bottom.
        {
            let f = f.clone();
            adj.connect_value_changed(move |a| {
                if a.upper() > a.page_size() && a.value() + a.page_size() >= a.upper() - a.page_size() {
                    f();
                }
            });
        }
        // Content shorter than the viewport; `changed` fires after a relayout.
        {
            let f = f.clone();
            adj.connect_changed(move |a| {
                if a.page_size() > 0.0 && a.upper() <= a.page_size() {
                    f();
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_video_is_placed_once_in_order_and_rows_never_exceed_the_column_count() {
        let layouts: [&[bool]; 4] = [
            &[false; 7],
            &[false, false, true, false, false, false, false],
            &[true, false, false],
            &[false, true, true, false, false, true],
        ];
        for is_shelf in layouts {
            for cols in 1..=4 {
                for show_shelves in [true, false] {
                    let rows = layout_rows(is_shelf, cols, show_shelves);
                    let mut cards = vec![];
                    let mut shelves = vec![];
                    for row in &rows {
                        match row {
                            RowSpec::Cards(c) => {
                                assert!(c.len() <= cols);
                                cards.extend(c);
                            }
                            RowSpec::Shelf(i) => shelves.push(*i),
                            RowSpec::Header => unreachable!(),
                        }
                    }
                    let want = |shelf: bool| {
                        (0..is_shelf.len() as u32).filter(|&i| is_shelf[i as usize] == shelf).collect::<Vec<_>>()
                    };
                    assert_eq!(cards.into_iter().copied().collect::<Vec<_>>(), want(false));
                    assert_eq!(shelves, if show_shelves { want(true) } else { vec![] });
                }
            }
        }
    }
}
