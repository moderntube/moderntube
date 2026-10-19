use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::{ListBox, Popover, PositionType, ScrolledWindow, SearchEntry, SelectionMode};

use adw::Toast;

use innertube::Session;
use crate::models::ResultItem;
use crate::runtime::{AsyncRuntime, Generation};
use crate::widgets::SearchFilters;

use super::app::App;
use super::{FeedKind, PlayerMode, YOUTUBE_HOME};

/// Wires a search box's autocomplete: a debounced suggestions popover, Enter to search, row
/// click to fill and search, and focus-out dismissal. Each box has its own popover so the
/// results and player header boxes coexist.
pub(super) fn wire_search(
    search: &SearchEntry,
    run_search: Rc<dyn Fn(String)>,
    session: Rc<RefCell<Arc<Session>>>,
    rt: AsyncRuntime,
) {
    // The text last set by an autocomplete pick. `search_changed` skips suggestions while the
    // box holds exactly this: SearchEntry's debounced signal can fire more than once, so a
    // one-shot flag would not hold.
    let suggest_last_set = Rc::new(RefCell::new(String::new()));
    let suggest_pop =
        Popover::builder().autohide(false).has_arrow(false).position(PositionType::Bottom).build();
    suggest_pop.set_parent(search);
    suggest_pop.add_css_class("menu");
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::None);
    let scroller = ScrolledWindow::builder()
        .min_content_width(360)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    suggest_pop.set_child(Some(&scroller));

    // Generation for suggestion fetches. Enter and row picks bump it so a late response
    // cannot reopen the dismissed popover.
    let suggest_gen = Generation::default();
    // Enter → run the query and dismiss the suggestions (superseding any pending fetch).
    {
        let run_search = run_search.clone();
        let pop = suggest_pop.clone();
        let suggest_gen = suggest_gen.clone();
        search.connect_activate(move |entry| {
            suggest_gen.bump(); // supersede any in-flight suggestion
            pop.popdown();
            run_search(entry.text().to_string());
        });
    }
    // Row click → fill + search (suppress the resulting set_text from re-opening the popover).
    {
        let run_search = run_search.clone();
        let search_w = search.clone();
        let pop = suggest_pop.clone();
        let last_set = suggest_last_set.clone();
        let suggest_gen = suggest_gen.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(lbl) = row.child().and_downcast::<gtk::Label>() {
                let q = lbl.text().to_string();
                suggest_gen.bump(); // supersede any in-flight suggestion
                *last_set.borrow_mut() = q.clone(); // skip the set_text-induced fetch(es)
                search_w.set_text(&q);
                pop.popdown();
                run_search(q);
            }
        });
    }
    // Dismiss on focus-out (slight delay so a row click lands first).
    {
        let pop = suggest_pop.clone();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            let pop = pop.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                pop.popdown();
            });
        });
        search.add_controller(focus);
    }

    let last_set = suggest_last_set.clone();
    search.connect_search_changed(move |entry| {
        let q = entry.text().to_string();
        // Skip while the box holds the text set by an autocomplete pick.
        if q == *last_set.borrow() {
            suggest_pop.popdown();
            return;
        }
        if q.trim().is_empty() {
            suggest_pop.popdown();
            return;
        }
        let token = suggest_gen.bump();
        let gen = suggest_gen.clone();
        let list = list.clone();
        let pop = suggest_pop.clone();
        let current = session.borrow().clone();
        rt.spawn(
            async move { current.search_suggest(&q).await.unwrap_or_default() },
            move |sugs: Vec<String>| {
                if !gen.is_current(token) {
                    return;
                }
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                if sugs.is_empty() {
                    pop.popdown();
                    return;
                }
                for s in sugs.into_iter().take(10) {
                    let lbl = gtk::Label::builder()
                        .label(&s)
                        .xalign(0.0)
                        .margin_start(8)
                        .margin_end(8)
                        .margin_top(4)
                        .margin_bottom(4)
                        .build();
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&lbl));
                    list.append(&row);
                }
                pop.popup();
            },
        );
    });
}

impl App {
    // `run_search` runs a query for Enter, autocomplete rows and the `/` shortcut, and shows
    // the results page.
    pub(super) fn run_search(self: &Rc<Self>, query: String) {
        self.run_filtered_search(query, None);
    }

    /// Runs `query` with a search filter's `params` (IT-029), as the `sp` of the page URL.
    pub(super) fn run_filtered_search(self: &Rc<Self>, query: String, params: Option<String>) {
        let app = self;
        let App {
            rt, session, toasts, search_gen, search_continuation, search_query, loading_more, feed_kind,
            stack, last_page, player_mode, results_url, ..
        } = &**self;
        let results = self.results();
        if query.trim().is_empty() {
            return;
        }
        let mut url_params = vec![("search_query", query.as_str())];
        url_params.extend(params.as_deref().map(|p| ("sp", p)));
        if let Ok(url) = reqwest::Url::parse_with_params("https://www.youtube.com/results", url_params) {
            *results_url.borrow_mut() = url.into();
        }
        *search_query.borrow_mut() = query.clone();
        let filters = self.search_filters.get().cloned();
        let filtered = params.is_some();
        // Searching while watching full-screen drops the video to the corner PiP so the
        // results are not hidden behind it.
        if player_mode.get() == PlayerMode::Full {
            app.show_mini();
        }
        // A new search supersedes any in-flight load-more and resets pagination.
        let token = search_gen.bump();
        feed_kind.set(FeedKind::Search);
        *search_continuation.borrow_mut() = None;
        loading_more.set(false);
        stack.set_visible_child_name("results");
        *last_page.borrow_mut() = "results".into();
        let current = session.borrow().clone(); // snapshot the live Session (Arc, Send)
        let generation = search_gen.clone();
        let results = results.clone();
        let toasts = toasts.clone();
        let search_continuation = search_continuation.clone();
        // Show the spinner so a slow fetch does not read as nothing happening.
        results.set_empty_text(&format!("No results for “{query}”."));
        results.set_loading(true);
        tracing::info!("[search] start: {query:?} params={params:?}");
        let t0 = std::time::Instant::now();
        rt.spawn(
            async move { current.search(&query, params.as_deref()).await },
            move |res| {
                if !generation.is_current(token) {
                    return;
                }
                match res {
                    Ok(found) => {
                        *search_continuation.borrow_mut() = found.continuation.clone();
                        if let Some(filters) = &filters {
                            filters.set_groups(&found.filters, filtered);
                        }
                        let items: Vec<ResultItem> =
                            found.items.iter().filter_map(ResultItem::from_search_item).collect();
                        tracing::info!(
                            "[search] done in {:.1}s: {} items",
                            t0.elapsed().as_secs_f64(),
                            items.len()
                        );
                        results.set_items(items);
                    }
                    Err(e) => {
                        tracing::warn!("[search] error after {:.1}s: {e}", t0.elapsed().as_secs_f64());
                        results.set_loading(false);
                        toasts.add_toast(Toast::new(&format!("Search failed: {e}")));
                    }
                }
            },
        );
    }

    // Load a browse-based feed (Home / Subscriptions) into the results grid. Shares the
    // search generation + continuation so a feed switch supersedes in-flight pages and
    // load-more pages the right endpoint (via feed_kind).
    pub(super) fn load_feed(&self, kind: FeedKind) {
        let App {
            rt, session, toasts, search_gen, search_continuation, loading_more, feed_kind, results_url, ..
        } = self;
        let results = self.results();
        let token = search_gen.bump();
        feed_kind.set(kind);
        if let Some(filters) = self.search_filters.get() {
            filters.set_groups(&[], false);
        }
        *results_url.borrow_mut() = match kind {
            FeedKind::Subscriptions => "https://www.youtube.com/feed/subscriptions".into(),
            _ => YOUTUBE_HOME.into(),
        };
        *search_continuation.borrow_mut() = None;
        loading_more.set(false);
        let current = session.borrow().clone();
        let authed = current.is_authenticated();
        let generation = search_gen.clone();
        let results = results.clone();
        let toasts = toasts.clone();
        let search_continuation = search_continuation.clone();
        tracing::info!("[feed] load {kind:?} (authed={authed})");
        results.set_empty_text(match kind {
            FeedKind::Subscriptions if authed => {
                "No videos from your subscriptions yet.\nChannels you subscribe to show up here."
            }
            FeedKind::Subscriptions => "Sign in to see your subscriptions.",
            _ => "Nothing to show here yet.",
        });
        results.set_loading(true);
        rt.spawn(
            async move {
                match kind {
                    FeedKind::Subscriptions => current.subscriptions().await,
                    // Anonymous Home is the empty start-searching shell (IT-020); FEtrending
                    // answers 400.
                    _ => current.home().await,
                }
            },
            move |res| {
                if !generation.is_current(token) {
                    return;
                }
                match res {
                    Ok(found) => {
                        *search_continuation.borrow_mut() = found.continuation.clone();
                        let items: Vec<ResultItem> =
                            found.items.iter().filter_map(ResultItem::from_search_item).collect();
                        tracing::info!("[feed] {kind:?}: {} items (authed={authed})", items.len());
                        results.set_items(items);
                    }
                    Err(e) => {
                        tracing::warn!("[feed] {kind:?} error: {e}");
                        results.set_loading(false);
                        toasts.add_toast(Toast::new(&format!("Couldn't load feed: {e}")));
                    }
                }
            },
        );
    }
}

/// Autocomplete on both search boxes and the results grid's infinite scroll.
pub(super) fn wire_search_boxes(app: &Rc<App>, player_search: &SearchEntry) {
    let App { rt, session, toasts, search, search_gen, search_continuation, loading_more, feed_kind, .. } =
        &**app;
    let results = app.results();
    // Autocomplete on the results header and player header search boxes.
    let run_search: Rc<dyn Fn(String)> = {
        let app = app.clone();
        Rc::new(move |query: String| app.run_search(query))
    };
    wire_search(search, run_search.clone(), session.clone(), rt.clone());
    wire_search(player_search, run_search.clone(), session.clone(), rt.clone());

    let filters = Rc::new(SearchFilters::new({
        let app = Rc::downgrade(app);
        Rc::new(move |params: Option<String>| {
            if let Some(app) = app.upgrade() {
                let query = app.search_query.borrow().clone();
                app.run_filtered_search(query, params);
            }
        })
    }));
    app.header.pack_end(&filters.button);
    let _ = app.search_filters.set(filters);

    // Infinite scroll: near the bottom, fetch the next search page and append it.
    {
        let rt = rt.clone();
        let session = session.clone();
        let results_outer = results.clone();
        let toasts = toasts.clone();
        let search_gen = search_gen.clone();
        let search_continuation = search_continuation.clone();
        let loading_more = loading_more.clone();
        let results_cb = results.clone();
        let feed_kind = feed_kind.clone();
        results_outer.connect_load_more(move || {
            if loading_more.get() {
                return;
            }
            let Some(token) = search_continuation.borrow().clone() else { return };
            loading_more.set(true);
            let gen_token = search_gen.current();
            let current = session.borrow().clone(); // snapshot the live Session
            let generation = search_gen.clone();
            let results = results_cb.clone();
            let toasts = toasts.clone();
            let search_continuation = search_continuation.clone();
            let loading_more = loading_more.clone();
            let kind = feed_kind.get();
            rt.spawn(
                async move {
                    // Search pages a different endpoint than the browse-based feeds.
                    match kind {
                        FeedKind::Search => current.search_continuation(&token).await,
                        FeedKind::Home | FeedKind::Subscriptions => {
                            current.feed_continuation(&token).await
                        }
                    }
                },
                move |res| {
                    loading_more.set(false);
                    // A fresh search started while this page loaded; drop the page.
                    if !generation.is_current(gen_token) {
                        return;
                    }
                    match res {
                        Ok(found) => {
                            *search_continuation.borrow_mut() = found.continuation.clone();
                            let items: Vec<ResultItem> =
                                found.items.iter().filter_map(ResultItem::from_search_item).collect();
                            results.append_items(items);
                        }
                        Err(e) => toasts.add_toast(Toast::new(&format!("Couldn't load more: {e}"))),
                    }
                },
            );
        });
    }

}
