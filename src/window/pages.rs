//! The browse pages: the recommended column, the results grid, the channel page and the
//! playlist page, with the card actions they share.
use std::rc::Rc;

use adw::prelude::*;
use adw::Toast;
use gtk4 as gtk;
use gtk::{Box as GtkBox, Orientation};

use crate::models::ResultItem;
use crate::playback::QueueAction;
use crate::widgets::{ChannelView, PlaylistView, QueueCard, ResultsView};

use super::app::App;
use super::PlayerMode;

/// Builds the recommended column, the results grid, the channel page and the playlist page.
pub(super) fn build_pages(app: &Rc<App>) {
    let App {
        rt,
        prefs_store,
        session,
        toasts,
        sidebar,
        stack,
        queue,
        results_box,
        header,
        last_page,
        results_url,
        channel_url,
        playlist_url,
        next_referer,
        hover_preview,
        ..
    } = &**app;
    // Recommended: a results grid for the watch page's right column, filled from the opened
    // video's `related` list (`start_watch_meta`). A click plays through the shared `activate`
    // path at the front of the queue, so auto-advance and PiP behave as for the main grid.
    // Built after `activate` and the queue closures exist, then added as the sidebar's default
    // "recommended" page.
    let recommended = Rc::new(ResultsView::new(
        rt.clone(),
        hover_preview.clone(),
        {
            let app = app.clone();
            let queue = queue.clone();
            move |item: ResultItem| {
                queue.borrow_mut().push_front(item.clone());
                app.activate(item);
            }
        },
        {
            let app = app.clone();
            move |id: String, name: String| app.open_channel(id, name)
        },
        {
            let app = app.clone();
            move |item: ResultItem, action: QueueAction| app.queue_item(item, action)
        },
        {
            let app = app.clone();
            move |id: String, name: String| app.open_playlist(id, name)
        },
    ));
    // Right-column "recommended" page: the queue card pinned at the top, the recommended grid
    // scrolling under it. The card hides when nothing is queued behind the playing video,
    // leaving the column to recommendations.
    let recommended_page = GtkBox::new(Orientation::Vertical, 0);
    let queue_card = QueueCard::new(queue.clone(), {
        let app = app.clone();
        Rc::new(move |item| app.play_in_mode(item))
    }, rt.clone());
    recommended_page.append(&queue_card.root);
    // Section heading for the grid below; without it the grid reads as a continuation of
    // the queue card above.
    let recommended_heading = gtk::Label::builder()
        .label("Recommended")
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_start(12)
        .margin_top(10)
        .margin_bottom(4)
        .build();
    recommended_page.append(&recommended_heading);
    // Mount `outer`. `ResultsView::new` already made `root` the child of `outer`, and
    // appending an already-parented widget is a no-op in GTK4, which leaves the grid detached
    // from the window.
    recommended_page.append(&recommended.outer);
    sidebar.add_named(&recommended_page, Some("recommended"));
    sidebar.set_visible_child_name("recommended");
    recommended.set_compact();
    let _ = app.recommended.set(recommended.clone());

    // Results grid. Created after the auth control so the click handler can revert to
    // anonymous (via `set_logged_in`) when a resolve hits an expired session.
    let results = Rc::new(ResultsView::new(
        rt.clone(),
        hover_preview.clone(),
        {
            let app = app.clone();
            let last_page = last_page.clone();
            let queue = queue.clone();
            let results_url = results_url.clone();
            let next_referer = next_referer.clone();
            move |item: ResultItem| {
                *last_page.borrow_mut() = "results".into();
                *next_referer.borrow_mut() = Some(results_url.borrow().clone());
                // Clicking a result plays it full-screen and makes it the front queue item, so
                // the rest of the queue plays after it.
                queue.borrow_mut().push_front(item.clone());
                app.activate(item);
            }
        },
        {
            let app = app.clone();
            move |id: String, name: String| app.open_channel(id, name)
        },
        {
            let app = app.clone();
            move |item: ResultItem, action: QueueAction| app.queue_item(item, action)
        },
        {
            let app = app.clone();
            move |id: String, name: String| app.open_playlist(id, name)
        },
    ));

    results_box.append(header);
    results_box.append(&results.outer);
    results.set_show_shelves(prefs_store.get().home_shorts);
    prefs_store.subscribe({
        let results = results.clone();
        move |p| results.set_show_shelves(p.home_shorts)
    });

    // Channel page: a self-contained ChannelView (header + Videos/Live tabs + sort chips
    // + grid + infinite scroll). Clicking a video plays it (returning here on back);
    // clicking a channel name navigates to that channel.
    let channel_view = ChannelView::new(
        rt.clone(),
        session.clone(),
        toasts.clone(),
        hover_preview.clone(),
        {
            let app = app.clone();
            let last_page = last_page.clone();
            let queue = queue.clone();
            let channel_url = channel_url.clone();
            let next_referer = next_referer.clone();
            Rc::new(move |item: ResultItem| {
                *last_page.borrow_mut() = "channel".into();
                *next_referer.borrow_mut() = Some(channel_url.borrow().clone());
                queue.borrow_mut().push_front(item.clone());
                app.activate(item);
            })
        },
        Rc::new({
            let app = app.clone();
            move |id: String, name: String| app.open_channel(id, name)
        }),
        Rc::new({
            let app = app.clone();
            move |item: ResultItem, action: QueueAction| app.queue_item(item, action)
        }),
        {
            let stack_w = stack.clone();
            Rc::new(move || stack_w.set_visible_child_name("results"))
        },
    );

    // Playlist page: the playlist's videos in order. Playing row N makes it the current queue
    // item and queues the rest of the playlist behind it, so on-end auto-advances through the
    // playlist.
    let playlist_view = PlaylistView::new(
        rt.clone(),
        session.clone(),
        toasts.clone(),
        {
            let app = app.clone();
            let last_page = last_page.clone();
            let queue = queue.clone();
            let playlist_url = playlist_url.clone();
            let next_referer = next_referer.clone();
            Rc::new(move |items: &[ResultItem], start: usize| {
                *last_page.borrow_mut() = "playlist".into();
                *next_referer.borrow_mut() = Some(playlist_url.borrow().clone());
                let Some(first) = queue.borrow_mut().play_from(items, start) else { return };
                app.activate(first);
            })
        },
        {
            let stack_w = stack.clone();
            Rc::new(move || stack_w.set_visible_child_name("results"))
        },
    );
    let _ = app.results.set(results);
    let _ = app.channel_view.set(channel_view);
    let _ = app.playlist_view.set(playlist_view);
}

impl App {
    pub(super) fn results(&self) -> &Rc<ResultsView> {
        self.results.get().expect("results grid built")
    }

    pub(super) fn channel_view(&self) -> &Rc<ChannelView> {
        self.channel_view.get().expect("channel page built")
    }

    pub(super) fn playlist_view(&self) -> &Rc<PlaylistView> {
        self.playlist_view.get().expect("playlist page built")
    }

    // ⋮-menu handler for the result/channel grids. If nothing is playing, the chosen video
    // spawns the corner PiP and starts immediately; if something's already playing, it just
    // joins the queue (Add to queue → end; Play next → right after the current).
    pub(super) fn queue_item(self: &Rc<Self>, item: ResultItem, action: QueueAction) {
        let App { queue, toasts, player_mode, .. } = &**self;
        if player_mode.get() == PlayerMode::Hidden {
            // Nothing playing → make it current and spawn the PiP.
            queue.borrow_mut().push_front(item.clone());
            self.play_mini(item);
            return;
        }
        match action {
            QueueAction::AddToQueue => {
                queue.borrow_mut().push_back(item);
                toasts.add_toast(Toast::new("Added to queue"));
            }
            QueueAction::PlayNext => {
                queue.borrow_mut().insert_after_current(item);
                toasts.add_toast(Toast::new("Playing next"));
            }
        }
    }
}
