//! The window's shared state: the widgets and cells its callbacks share.
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::{ApplicationWindow, HeaderBar, ToastOverlay};
use gtk4 as gtk;
use gtk::gio;
use gtk::{
    Align, Box as GtkBox, Button, MenuButton, Orientation, Overlay, SearchEntry, Stack,
    StackTransitionType, ToggleButton,
};

use innertube::{Quality, Session};
use crate::playback::{initial_quality, player, Player, Queue};
use crate::runtime::{AsyncRuntime, Generation};
use crate::widgets::{
    ChannelView, ChatMode, CommentsView, HoverPreview, LiveChatView, PlayerView, PlaylistView, ResultsView,
    SearchFilters, ShortsView, WatchInfo,
};

use super::{FeedKind, PlayerMode, YOUTUBE_HOME};

pub(super) struct App {
    pub rt: AsyncRuntime,
    pub prefs_store: crate::prefs::PrefsStore,
    pub session: Rc<RefCell<Arc<Session>>>,
    pub search_gen: Generation,
    pub resolve_gen: Generation,
    pub chat_gen: Generation,
    pub comments_gen: Generation,
    pub comments_continuation: Rc<RefCell<Option<String>>>,
    pub chat_mode: Rc<Cell<ChatMode>>,
    pub current_is_live: Rc<Cell<bool>>,
    pub engine: Rc<dyn Player>,
    pub player: Rc<PlayerView>,
    pub chat: Rc<LiveChatView>,
    pub toasts: ToastOverlay,
    pub comments: Rc<CommentsView>,
    pub watch_info: Rc<WatchInfo>,
    pub chat_toggle: ToggleButton,
    pub sidebar: Stack,
    pub stack: Stack,
    pub current_video: Rc<RefCell<Option<String>>>,
    pub video_history: Rc<RefCell<Vec<String>>>,
    pub suppress_history: Rc<Cell<bool>>,
    pub history_back: Button,
    pub watch_positions: Rc<RefCell<Vec<(String, f64)>>>,
    pub queue: Rc<RefCell<Queue>>,
    pub mpris: Rc<crate::mpris::Mpris>,
    pub current_quality: Rc<Cell<Quality>>,
    pub current_live: Rc<RefCell<Option<String>>>,
    pub search_continuation: Rc<RefCell<Option<String>>>,
    /// The query on the results page, which a picked search filter runs again.
    pub search_query: Rc<RefCell<String>>,
    pub search_filters: OnceCell<Rc<SearchFilters>>,
    pub loading_more: Rc<Cell<bool>>,
    pub feed_kind: Rc<Cell<FeedKind>>,
    pub results_box: GtkBox,
    pub header: HeaderBar,
    pub search: SearchEntry,
    pub sign_in: Button,
    pub account: MenuButton,
    pub settings_box: GtkBox,
    pub last_page: Rc<RefCell<String>>,
    pub results_url: Rc<RefCell<String>>,
    pub channel_url: Rc<RefCell<String>>,
    pub playlist_url: Rc<RefCell<String>>,
    pub next_referer: Rc<RefCell<Option<String>>>,
    pub video_slot: GtkBox,
    pub player_mode: Rc<Cell<PlayerMode>>,
    pub video_fullscreen: Rc<Cell<bool>>,
    pub mini_offset: Rc<Cell<(i32, i32)>>,
    pub mini_size: Rc<Cell<(i32, i32)>>,
    pub pip_quality_override: Rc<Cell<Option<Quality>>>,
    pub pip_reduced: Rc<Cell<bool>>,
    pub hover_preview: Rc<HoverPreview>,
    pub hover_surface: gtk::Widget,
    pub stage: Overlay,
    /// The recommended grid, built once `activate` exists; set before a card can be clicked.
    pub recommended: OnceCell<Rc<ResultsView>>,
    /// The top-level window, built after the pages; the login window's transient parent.
    pub window: OnceCell<ApplicationWindow>,
    pub shorts_view: OnceCell<Rc<ShortsView>>,
    pub results: OnceCell<Rc<ResultsView>>,
    pub channel_view: OnceCell<Rc<ChannelView>>,
    pub playlist_view: OnceCell<Rc<PlaylistView>>,
    pub fs_state: Rc<Cell<bool>>,
    pub player_header: OnceCell<HeaderBar>,
    pub nav_root: OnceCell<GtkBox>,
}

impl App {
    /// Builds the shared state and the widgets the pages are assembled from.
    pub(super) fn new(rt: AsyncRuntime, session: Arc<Session>) -> Rc<Self> {
        let theme = Rc::new(crate::theme::Theme::install());
        {
            let mode = crate::theme::ThemeMode::from_str(
                &crate::prefs::load_theme_mode().unwrap_or_else(|| "system".into()),
            );
            let accent = crate::prefs::load_accent()
                .unwrap_or_else(|| crate::theme::DEFAULT_ACCENT.to_string());
            theme.apply(mode, &accent);
        }
        // Typed, observable preferences. `load()` reads the on-disk snapshot and registers the
        // persistence subscriber; the theme subscriber below re-applies styling when appearance
        // prefs change. The startup apply above covers first paint.
        let prefs_store = crate::prefs::PrefsStore::load();
        {
            let p = prefs_store.get();
            crate::playback::common::set_direct_opens(p.direct_opens);
            crate::playback::set_pot_backend(innertube::PotBackendKind::parse(&p.pot_backend).unwrap_or_default());
        }
        prefs_store.subscribe({
            let theme = theme.clone();
            move |p| theme.apply(crate::theme::ThemeMode::from_str(&p.theme_mode), &p.accent)
        });
        // Swappable so login/logout takes effect immediately. Read `session.borrow().clone()`
        // at each call-site to snapshot the current (anon or authenticated) Session.
        let session: Rc<RefCell<Arc<Session>>> = Rc::new(RefCell::new(session));
        let search_gen = Generation::default();
        let resolve_gen = Generation::default();
        // Generation for the live-chat poll loop. Bumped when a new video opens; a live-edge
        // re-join keeps the same chat.
        let chat_gen = Generation::default();
        // Comments pagination: a generation (bumped per VOD open, so a stale page can't append
        // to a newer video) + the next-page continuation token.
        let comments_gen = Generation::default();
        let comments_continuation: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        // Chat presentation mode (docked, overlay, floating) and whether the loaded video is
        // live. Together they decide what the sidebar toggle shows and where chat lives.
        let chat_mode: Rc<Cell<ChatMode>> = Rc::new(Cell::new(ChatMode::Docked));
        let current_is_live: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        let engine = player::build();
        let player = Rc::new(PlayerView::new(engine.clone(), rt.clone()));
        let chat = Rc::new(crate::widgets::LiveChatView::new(rt.clone()));
        let toasts = ToastOverlay::new();
        let comments = crate::widgets::CommentsView::new(rt.clone(), session.clone(), toasts.clone());
        // Watch-page metadata block shown below the video in the left column. Built early so the
        // play path can capture it.
        let watch_info = Rc::new(WatchInfo::new(rt.clone()));
        // A clickable comment timestamp seeks the player (VOD and was-live replay).
        {
            let engine = engine.clone();
            comments.set_on_seek(Rc::new(move |secs: f64| engine.seek_absolute(secs)));
        }
        // Sidebar toggle, created early so closures can capture it and packed into the player
        // header. Shows or hides the sidebar: live chat for live streams, recommended videos for
        // VODs. Visible once a video is loaded.
        let chat_toggle = ToggleButton::new();
        chat_toggle.set_icon_name("sidebar-show-right-symbolic");
        chat_toggle.set_tooltip_text(Some("Toggle sidebar"));
        chat_toggle.set_visible(false);
        // Right-column stack: "recommended" (VOD default, related videos) or "chat" (live and
        // was-live VOD replay). Created early so closures can capture it; packed into the Paned
        // further down. The "recommended" page is added once `recommended` is built.
        let sidebar = Stack::new();
        sidebar.add_named(&chat.root, Some("chat"));

        let stack = Stack::new();
        stack.set_transition_type(StackTransitionType::Crossfade);

        // Drop the loading spinner as soon as the new video's first frame is loaded.
        {
            let player = player.clone();
            engine.set_on_loaded(Rc::new(move || {
                tracing::info!("[first-frame] uptime {}ms", crate::uptime_ms());
                player.set_loading(false)
            }));
        }

        // The playing video id (None on live) and the quality cap it plays at. The cap is seeded
        // from the saved preference and carries across videos, VOD and live.
        let current_video: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        // "Previous video" history: ids pushed by `activate` when it swaps to a different video,
        // popped by the history-back button in the player header. `back` is separate and drops to
        // the corner PiP. `suppress_history` stops the button's replay `activate` from re-pushing
        // the video it leaves.
        let video_history: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let suppress_history: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let history_back = Button::from_icon_name("media-skip-backward-symbolic");
        history_back.set_tooltip_text(Some("Previous video"));
        history_back.set_sensitive(false);
        // Per-video resume positions: saved during VOD playback and on navigate or stop,
        // persisted via `prefs` and loaded at launch, most recent first. `record_position`
        // upserts and persists; `clear_position` drops one (finished video).
        let watch_positions: Rc<RefCell<Vec<(String, f64)>>> =
            Rc::new(RefCell::new(crate::prefs::load_watch_positions()));
        // Watch queue shared by the result-card menus, the header queue panel and the
        // auto-advance hook.
        let queue = Rc::new(RefCell::new(Queue::new()));
        // MPRIS: media-key and lockscreen control over D-Bus. Created early so `activate` can
        // push now-playing metadata; commands are wired near the end.
        let mpris = Rc::new(crate::mpris::Mpris::new(&rt));
        let current_quality: Rc<Cell<Quality>> = Rc::new(Cell::new(initial_quality()));
        // The playing live video id (None for VOD or idle), for the LIVE button to re-join at
        // the head.
        let current_live: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        // Results pagination: the next-page continuation token (None → no more pages), an
        // in-flight guard so scrolling near the bottom can't fire duplicate load-mores, and
        // which feed is showing (so load-more pages the right endpoint).
        let search_continuation: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let loading_more = Rc::new(Cell::new(false));
        let feed_kind: Rc<Cell<FeedKind>> = Rc::new(Cell::new(FeedKind::Home));

        let results_box = GtkBox::new(Orientation::Vertical, 0);
        let header = HeaderBar::new();
        let search = SearchEntry::builder().placeholder_text("Search YouTube").width_request(360).build();
        header.set_title_widget(Some(&search));

        // Auth controls: "Sign in" when anonymous, an account menu with "Sign out" when signed in.
        // They live in the Settings popover on the nav rail, which is reachable on every page.
        let sign_in = Button::with_label("Sign in");
        let logout_menu = gio::Menu::new();
        logout_menu.append(Some("Sign out"), Some("win.logout"));
        let account = MenuButton::builder().label("Account").menu_model(&logout_menu).build();
        account.set_visible(false);

        // Settings popover content. Account, appearance and behavior rows are built here and
        // SponsorBlock is appended later. Assigned to the nav rail's Settings button once the
        // rail exists.
        let settings_box =
            super::settings::build_settings_box(&prefs_store, &player, &session, &sign_in, &account);

        // The page the player should return to on "back" (results or a channel page).
        let last_page: Rc<RefCell<String>> = Rc::new(RefCell::new("results".into()));
        // Each browse page's youtube.com URL: the `referer` of a video opened from it.
        let results_url: Rc<RefCell<String>> = Rc::new(RefCell::new(YOUTUBE_HOME.into()));
        let channel_url: Rc<RefCell<String>> = Rc::new(RefCell::new(YOUTUBE_HOME.into()));
        let playlist_url: Rc<RefCell<String>> = Rc::new(RefCell::new(YOUTUBE_HOME.into()));
        // Set by a browse page's click; any other open comes from the current watch page.
        let next_referer: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        // Miniplayer: one video surface, repositioned and never reparented. `player.root` sits in
        // the top-level `stage` overlay, positioned by `get-child-position`: Full covers the
        // player page's `video_slot` (so the docked sidebar Paned stays), Mini is a bottom-right
        // corner box over the current page. `video_slot` is an empty placeholder reserving the
        // video's space in the Paned. The GL context and hwdec frames survive every transition
        // because only the allocation changes.
        let video_slot = GtkBox::new(Orientation::Vertical, 0);
        video_slot.set_hexpand(true);
        // Pinned at the top of the left column, outside the scroller, and held to 16:9 by the
        // AspectFrame below, so the GL surface (which tracks this slot's allocation) keeps a
        // fixed shape while the metadata and comments scroll under it.
        video_slot.set_vexpand(false);
        let player_mode = Rc::new(Cell::new(PlayerMode::Hidden));
        // Fullscreen: the full-size player covers the whole stage instead of its slot.
        let video_fullscreen = Rc::new(Cell::new(false));
        // Drag offset of the corner miniplayer from its default bottom-right position.
        let mini_offset = Rc::new(Cell::new((0i32, 0i32)));
        // Corner-miniplayer size, resizable and persisted; defaults to 360x202 (16:9).
        let mini_size = Rc::new(Cell::new(crate::prefs::load_pip_size().unwrap_or((360, 202))));
        // A video started directly in the corner PiP loads at a reduced resolution.
        // `pip_quality_override` carries that cap to the next `activate`; `pip_reduced` remembers
        // it so expanding to full re-prepares at the user's cap. Toggling an already-playing
        // video between full and mini does not reload. PiP cap = min(480p, user cap).
        let pip_quality_override: Rc<Cell<Option<Quality>>> = Rc::new(Cell::new(None));
        let pip_reduced = Rc::new(Cell::new(false));

        // One hover preview shared by every `ResultsView`, so at most one plays at a time.
        // Created before `stage` so its surface is a stable overlay child of the stage.
        let hover_preview = HoverPreview::new(rt.clone(), prefs_store.get().preview_muted);
        {
            let hp = Rc::downgrade(&hover_preview);
            sidebar.connect_visible_child_notify(move |_| {
                if let Some(hp) = hp.upgrade() {
                    hp.hide_on_page_change();
                }
            });
        }
        hover_preview.set_referer_source({
            let (last_page, results_url, channel_url, playlist_url) =
                (last_page.clone(), results_url.clone(), channel_url.clone(), playlist_url.clone());
            Rc::new(move || match last_page.borrow().as_str() {
                "channel" => channel_url.borrow().clone(),
                "playlist" => playlist_url.borrow().clone(),
                _ => results_url.borrow().clone(),
            })
        });
        let hover_surface = hover_preview.surface();

        let stage = Overlay::new();
        stage.set_child(Some(&stack));
        stage.add_overlay(&player.root);
        // The preview surface floats over the hovered card, positioned through
        // `get-child-position`. Add it once and never reparent it.
        stage.add_overlay(&hover_surface);
        Rc::new(App {
            rt,
            prefs_store,
            session,
            search_gen,
            resolve_gen,
            chat_gen,
            comments_gen,
            comments_continuation,
            chat_mode,
            current_is_live,
            engine,
            player,
            chat,
            toasts,
            comments,
            watch_info,
            chat_toggle,
            sidebar,
            stack,
            current_video,
            video_history,
            suppress_history,
            history_back,
            watch_positions,
            queue,
            mpris,
            current_quality,
            current_live,
            search_continuation,
            search_query: Rc::default(),
            search_filters: OnceCell::new(),
            loading_more,
            feed_kind,
            results_box,
            header,
            search,
            sign_in,
            account,
            settings_box,
            last_page,
            results_url,
            channel_url,
            playlist_url,
            next_referer,
            video_slot,
            player_mode,
            video_fullscreen,
            mini_offset,
            mini_size,
            pip_quality_override,
            pip_reduced,
            hover_preview,
            hover_surface,
            stage,
            recommended: OnceCell::new(),
            window: OnceCell::new(),
            shorts_view: OnceCell::new(),
            results: OnceCell::new(),
            channel_view: OnceCell::new(),
            playlist_view: OnceCell::new(),
            fs_state: Rc::new(Cell::new(false)),
            player_header: OnceCell::new(),
            nav_root: OnceCell::new(),
        })
    }

    /// The single authority on where the live-chat panel lives and whether it or the sidebar
    /// shows: docked in the side panel, or over the video as a strip or a floating card.
    pub(super) fn place_chat(&self, mode: ChatMode) {
        let (sidebar, overlay, chat, chat_toggle, chat_mode) =
            (&self.sidebar, &self.player.root, &self.chat, &self.chat_toggle, &self.chat_mode);
        let prev = chat_mode.get();
        if prev != mode {
            match prev {
                ChatMode::Docked => sidebar.remove(&chat.root),
                _ => overlay.remove_overlay(&chat.root),
            }
            match mode {
                ChatMode::Docked => {
                    chat.root.set_halign(Align::Fill);
                    chat.root.set_valign(Align::Fill);
                    chat.root.set_margin_start(0);
                    chat.root.set_margin_top(0);
                    chat.root.set_margin_end(0);
                    chat.root.set_margin_bottom(0);
                    chat.root.set_width_request(360);
                    chat.root.set_height_request(-1);
                    sidebar.add_named(&chat.root, Some("chat"));
                    sidebar.set_visible_child_name("chat");
                }
                ChatMode::Overlay => {
                    // Full-height translucent strip pinned to the right edge.
                    chat.root.set_halign(Align::End);
                    chat.root.set_valign(Align::Fill);
                    chat.root.set_margin_start(0);
                    chat.root.set_margin_top(0);
                    chat.root.set_margin_end(0);
                    chat.root.set_margin_bottom(0);
                    chat.root.set_width_request(360);
                    chat.root.set_height_request(-1);
                    overlay.add_overlay(&chat.root);
                }
                ChatMode::Floating => {
                    // Draggable card near the top-left (moved by its header).
                    chat.root.set_halign(Align::Start);
                    chat.root.set_valign(Align::Start);
                    chat.root.set_margin_start(24);
                    chat.root.set_margin_top(24);
                    chat.root.set_width_request(360);
                    chat.root.set_height_request(440);
                    overlay.add_overlay(&chat.root);
                }
            }
            chat_mode.set(mode);
        }
        chat.apply_mode(mode);
        let on = chat_toggle.is_active();
        match mode {
            ChatMode::Docked => {
                chat.root.set_visible(true);
                sidebar.set_visible_child_name("chat"); // place_chat is live-only
                sidebar.set_visible(on);
            }
            _ => {
                chat.root.set_visible(on);
                sidebar.set_visible(false);
            }
        }
    }

    /// Upserts and persists a video's resume position, in seconds.
    pub(super) fn record_position(&self, id: &str, secs: f64) {
        {
            let mut list = self.watch_positions.borrow_mut();
            crate::prefs::upsert_position(&mut list, id, secs, crate::prefs::WATCH_POSITIONS_MAX);
        }
        crate::prefs::save_watch_positions(&self.watch_positions.borrow());
    }

    pub(super) fn clear_position(&self, id: &str) {
        {
            let mut list = self.watch_positions.borrow_mut();
            crate::prefs::remove_position(&mut list, id);
        }
        crate::prefs::save_watch_positions(&self.watch_positions.borrow());
    }

    // Flip header state (and the account label) between logged-out and logged-in.
    pub(super) fn set_logged_in(&self, logged_in: bool, label: Option<String>) {
        let playback = self.prefs_store.get().play_signed_in.then(|| self.session.borrow().clone());
        crate::playback::set_playback_account(playback.as_deref());
        self.sign_in.set_visible(!logged_in);
        self.account.set_visible(logged_in);
        self.chat.set_can_send(logged_in); // only authed users can post to chat
        if let Some(l) = label {
            self.account.set_label(&l);
        }
    }
}
