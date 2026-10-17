//! The player page: a header, a pinned 16:9 video slot above the watch info and comments,
//! and the sidebar column beside them.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::HeaderBar;
use gtk4 as gtk;
use gtk::glib;
use gtk::{AspectFrame, Box as GtkBox, Button, GestureDrag, Orientation, Paned, PolicyType, ScrolledWindow, SearchEntry};

use crate::models::ResultItem;
use crate::widgets::{CatchupStrip, ChatMode};

use super::app::App;
use super::{SIDEBAR_COLLAPSE_PX, SIDEBAR_REOPEN_PX};

/// The player page's widgets that later wiring needs.
pub(super) struct PlayerPage {
    pub player_box: GtkBox,
    pub player_header: HeaderBar,
    pub player_search: SearchEntry,
    pub back: Button,
    pub content: Paned,
    /// The sidebar's last comfortable width: a collapse and a reopen return to it.
    pub sidebar_width: Rc<Cell<i32>>,
    /// A drag-collapsed sidebar stays mapped at 0 px, so its divider can be dragged back out.
    pub sidebar_collapsed: Rc<Cell<bool>>,
    /// Set while the toggle is moved to match a drag, so its handlers leave the panel alone.
    pub sidebar_syncing: Rc<Cell<bool>>,
    pub catchup_strip: Rc<CatchupStrip>,
    pub sidebar_column: GtkBox,
}

/// Lays out the player page: header, the 16:9 video slot over the watch info and comments,
/// and the sidebar column beside them.
pub(super) fn build_player_page(app: &Rc<App>) -> PlayerPage {
    let App { player, comments, watch_info, chat_toggle, sidebar, history_back, video_slot, .. } = &**app;
    // The header carries its own search box, since a widget has one parent and the results
    // header owns the other. Both call `run_search`.
    let player_box = GtkBox::new(Orientation::Vertical, 0);
    let player_header = HeaderBar::new();
    let back = Button::from_icon_name("go-previous-symbolic");
    let player_search = SearchEntry::builder().placeholder_text("Search YouTube").width_request(360).build();
    player_header.set_title_widget(Some(&player_search));
    player_header.pack_start(&back);
    // `history_back` is created in `App::new` so `activate` can update its sensitivity. Its icon
    // differs from `back`, which drops to the corner PiP.
    player_header.pack_start(history_back);
    player_header.pack_end(chat_toggle);
    player_box.append(&player_header);
    let content = Paned::new(Orientation::Horizontal);
    let sidebar_width = Rc::new(Cell::new(0i32));
    let sidebar_collapsed = Rc::new(Cell::new(false));
    let sidebar_syncing = Rc::new(Cell::new(false));
    player.root.set_hexpand(true);
    // Left column: a pinned 16:9 video slot above a scroller holding the watch metadata and
    // comments, so the GL overlay's target never scrolls. `obey_child=false` uses our ratio
    // instead of the empty slot's.
    let video_frame = AspectFrame::builder()
        .ratio(16.0 / 9.0)
        .obey_child(false)
        .hexpand(true)
        .vexpand(false)
        .build();
    video_frame.set_child(Some(video_slot));
    let below = GtkBox::new(Orientation::Vertical, 0);
    below.append(&watch_info.root);
    below.append(&comments.root);
    let below_scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&below)
        .build();
    let left = GtkBox::new(Orientation::Vertical, 0);
    left.append(&video_frame);
    left.append(&below_scroller);
    // The comment thread sits inside `below_scroller`, which owns the scrolling, so the outer
    // scroller nearing the bottom drives comment paging.
    {
        let comments = comments.clone();
        let pending = Rc::new(Cell::new(false));
        let adj = below_scroller.vadjustment();
        adj.connect_value_changed(move |a| {
            if a.upper() - (a.value() + a.page_size()) < 600.0 && !pending.get() {
                pending.set(true);
                comments.request_load_more();
                let pending2 = pending.clone();
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    pending2.set(false)
                });
            }
        });
    }
    content.set_start_child(Some(&left));
    // The right column is the catch-up strip above the sidebar stack, so a running catch-up
    // leaves chat visible. The result panel takes the stack once a summary is ready.
    let catchup_strip = Rc::new(crate::widgets::CatchupStrip::new());
    let sidebar_column = GtkBox::new(Orientation::Vertical, 0);
    sidebar_column.append(&catchup_strip.root);
    sidebar.set_vexpand(true); // the stack, not the strip, takes the column's height
    sidebar_column.append(sidebar);
    // The column is shown when the stack or the strip is. The stack's `visible` notify covers
    // existing `sidebar.set_visible` calls; the strip sets the column directly, so a failed
    // run stays readable with the sidebar toggled off.
    {
        let column = sidebar_column.clone();
        let strip = catchup_strip.clone();
        sidebar.connect_visible_notify(move |s| {
            column.set_visible(s.is_visible() || strip.root.is_visible());
        });
    }
    content.set_end_child(Some(&sidebar_column));
    content.set_resize_start_child(true); // video takes new space; sidebar keeps its width
    content.set_resize_end_child(false);
    // The sidebar may be dragged narrower than its content: past SIDEBAR_COLLAPSE_PX it closes.
    content.set_shrink_end_child(true);
    sidebar.set_visible(false);
    player_box.append(&content);
    PlayerPage {
        player_box,
        player_header,
        player_search,
        back,
        content,
        sidebar_width,
        sidebar_collapsed,
        sidebar_syncing,
        catchup_strip,
        sidebar_column,
    }
}

/// The sidebar's drag-collapse and reopen, the chat mode button, the floating chat drag
/// and the sidebar toggle.
pub(super) fn wire_sidebar(app: &Rc<App>, page: &PlayerPage) {
    let App { chat, chat_toggle, chat_mode, current_is_live, sidebar, player, stage, .. } = &**app;
    let PlayerPage {
        content, sidebar_width, sidebar_collapsed, sidebar_syncing, catchup_strip, sidebar_column, ..
    } = page;
    // Dragging the divider resizes `video_slot`, but `stage` does not re-allocate on its own,
    // so the video overlay lags behind the column. Queue a stage allocation as the divider
    // moves, throttled to about 60 fps.
    {
        let stage_p = stage.clone();
        let pending = Rc::new(Cell::new(false));
        let (toggle, last_width, settle) = (chat_toggle.clone(), sidebar_width.clone(), Rc::new(Cell::new(0u64)));
        let (collapsed, syncing) = (sidebar_collapsed.clone(), sidebar_syncing.clone());
        content.connect_position_notify(move |paned| {
            let width = paned.width() - paned.position();
            if width >= SIDEBAR_REOPEN_PX {
                last_width.set(width);
            }
            if collapsed.get() {
                if width >= SIDEBAR_COLLAPSE_PX {
                    tracing::info!("[sidebar] reopened by drag ({width} px)");
                    collapsed.set(false);
                    paned.set_wide_handle(false);
                    syncing.set(true);
                    toggle.set_active(true);
                    syncing.set(false);
                }
            } else if width < SIDEBAR_COLLAPSE_PX && toggle.is_active() && paned.width() > 0 {
                // Collapse once the divider has rested near the right edge.
                let g = settle.get().wrapping_add(1);
                settle.set(g);
                let (paned, toggle, settle, collapsed, syncing) =
                    (paned.clone(), toggle.clone(), settle.clone(), collapsed.clone(), syncing.clone());
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                    if settle.get() == g && !collapsed.get() && paned.width() - paned.position() < SIDEBAR_COLLAPSE_PX {
                        tracing::info!("[sidebar] collapsed by drag ({} px)", paned.width() - paned.position());
                        collapsed.set(true);
                        paned.set_wide_handle(true);
                        paned.set_position(paned.width());
                        syncing.set(true);
                        toggle.set_active(false);
                        syncing.set(false);
                    }
                });
            }
            if pending.replace(true) {
                return;
            }
            let stage_t = stage_p.clone();
            let pending = pending.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                stage_t.queue_allocate();
                pending.set(false);
            });
        });
    }
    // Reopening restores the sidebar's last width: a hidden or collapsed end child has none.
    {
        let (content, sidebar_width) = (content.clone(), sidebar_width.clone());
        let (collapsed, syncing) = (sidebar_collapsed.clone(), sidebar_syncing.clone());
        chat_toggle.connect_toggled(move |t| {
            if syncing.get() {
                return;
            }
            collapsed.set(false);
            content.set_wide_handle(false);
            if !t.is_active() {
                return;
            }
            let (content, width) = (content.clone(), sidebar_width.get().max(SIDEBAR_REOPEN_PX));
            gtk::glib::idle_add_local_once(move || {
                let open = content.width() - content.position();
                if open < SIDEBAR_REOPEN_PX && content.width() > width {
                    content.set_position(content.width() - width);
                }
                tracing::info!("[sidebar] reopened: {open} px -> {} px", content.width() - content.position());
            });
        });
    }
    // Mode button (in the chat header) cycles docked → overlay → floating.
    {
        let app = app.clone();
        let chat_mode = chat_mode.clone();
        chat.connect_mode_cycle(move || app.place_chat(chat_mode.get().next()));
    }
    {
        let app = app.clone();
        crate::hooks::on("chat-mode", move |m| {
            let mode = match m.trim().to_ascii_lowercase().as_str() {
                "overlay" => Some(ChatMode::Overlay),
                "floating" => Some(ChatMode::Floating),
                "docked" => Some(ChatMode::Docked),
                other => {
                    tracing::info!("[chat-mode] ignoring chat-mode {other:?}");
                    None
                }
            };
            if let Some(mode) = mode {
                let app = app.clone();
                glib::timeout_add_seconds_local_once(6, move || {
                    tracing::info!("[chat-mode] placing chat: {mode:?}");
                    app.place_chat(mode);
                });
            }
        });
    }

    // Drag the floating chat card by its header. The gesture lives on the stable overlay
    // (`player.root`): on the moving card its own reference frame would shift each frame and
    // feed back into jitter. Drags outside the header are denied; moves go through overlay
    // margins.
    {
        let drag = GestureDrag::new();
        let chat_d = chat.clone();
        let overlay = player.root.clone();
        let chat_mode = chat_mode.clone();
        let base = Rc::new(Cell::new((0i32, 0i32)));
        let active = Rc::new(Cell::new(false));
        // Claim only once movement starts, so a stationary click on the header still reaches
        // the mode button.
        let claimed = Rc::new(Cell::new(false));
        {
            let chat_d = chat_d.clone();
            let base = base.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            let chat_mode = chat_mode.clone();
            let overlay = overlay.clone();
            drag.connect_drag_begin(move |g, sx, sy| {
                // Only when floating and the press began on the card header, in the overlay's
                // coordinate space.
                let on_header = chat_mode.get() == ChatMode::Floating
                    && chat_d
                        .header
                        .compute_bounds(&overlay)
                        .map(|r| r.contains_point(&gtk::graphene::Point::new(sx as f32, sy as f32)))
                        .unwrap_or(false);
                active.set(on_header);
                claimed.set(false);
                if on_header {
                    base.set((chat_d.root.margin_start(), chat_d.root.margin_top()));
                    // The claim waits for movement in drag_update so a click passes through.
                } else {
                    g.set_state(gtk::EventSequenceState::Denied);
                }
            });
        }
        {
            let chat_d = chat_d.clone();
            let active = active.clone();
            let claimed = claimed.clone();
            drag.connect_drag_update(move |g, ox, oy| {
                if !active.get() {
                    return;
                }
                if !claimed.get() {
                    if ox.abs() + oy.abs() <= 4.0 {
                        return; // below threshold: leave the click for the header's button
                    }
                    claimed.set(true);
                    chat_d.header.set_cursor_from_name(Some("grabbing"));
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
                let (bx, by) = base.get();
                let nx = (bx + ox as i32).max(0);
                let ny = (by + oy as i32).max(0);
                let max_x = (overlay.width() - chat_d.root.width()).max(0);
                let max_y = (overlay.height() - chat_d.root.height()).max(0);
                chat_d.root.set_margin_start(nx.min(max_x));
                chat_d.root.set_margin_top(ny.min(max_y));
            });
        }
        {
            let chat_d = chat.clone();
            let active = active.clone();
            drag.connect_drag_end(move |_, _, _| {
                active.set(false);
                if claimed.replace(false) {
                    chat_d.header.set_cursor_from_name(Some("grab"));
                }
            });
        }
        // Bubble phase so the controls' own widgets (seek bar, buttons) handle first.
        drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
        player.root.add_controller(drag);
    }

    // The sidebar toggle shows or hides the active surface: the chat card when live and
    // undocked, otherwise the side panel.
    {
        let sidebar = sidebar.clone();
        let chat = chat.clone();
        let chat_mode = chat_mode.clone();
        let current_is_live = current_is_live.clone();
        let column = sidebar_column.clone();
        let strip = catchup_strip.clone();
        let syncing = sidebar_syncing.clone();
        chat_toggle.connect_toggled(move |t| {
            if syncing.get() {
                return;
            }
            let on = t.is_active();
            if current_is_live.get() && chat_mode.get() != ChatMode::Docked {
                chat.root.set_visible(on);
            } else {
                sidebar.set_visible(on);
                // Set the wrapper column here too. The stack's `notify::visible` fires only on a
                // change, and the catch-up strip sets the column directly, so the column can be
                // hidden while the stack is already visible.
                column.set_visible(on || strip.root.is_visible());
            }
        });
    }
}

/// The player header's back and previous-video buttons.
pub(super) fn wire_history_buttons(app: &Rc<App>, page: &PlayerPage) {
    let App { video_history, suppress_history, history_back, .. } = &**app;
    let back = &page.back;
    {
        // Back drops the video to the corner miniplayer, which keeps playing. The corner × ends
        // playback.
        let app = app.clone();
        back.connect_clicked(move |_| app.show_mini());
    }
    {
        // Pops the history stack and replays that id through `activate`. `suppress_history`
        // keeps the replay from pushing the video it leaves; `activate` then updates the
        // button's sensitivity.
        let video_history = video_history.clone();
        let suppress_history = suppress_history.clone();
        let app = app.clone();
        history_back.connect_clicked(move |_| {
            let Some(id) = video_history.borrow_mut().pop() else { return };
            suppress_history.set(true);
            app.activate(ResultItem::for_video_id(&id));
        });
    }
}
