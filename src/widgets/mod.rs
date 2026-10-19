pub mod an_webp;
pub mod ask_panel;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4::glib;

/// Shows `toggle` only when `label` is ellipsized.
///
/// `is_ellipsized()` reads the current pango layout, which says nothing before the label has
/// a width. The check waits for a non-zero allocated width and runs once.
pub(crate) fn reveal_when_truncated(label: &gtk::Label, toggle: &gtk::Widget) {
    toggle.set_visible(false);
    let toggle = toggle.clone();
    label.add_tick_callback(move |label, _| {
        if label.width() == 0 {
            return glib::ControlFlow::Continue;
        }
        toggle.set_visible(label.layout().is_ellipsized());
        glib::ControlFlow::Break
    });
}

/// Turns on `:shortcode` emoji completion for a composer entry.
///
/// GTK's `enable-emoji-completion` opens a popup after `:` and two characters, matched against
/// the CLDR names in the table shipped inside libgtk, with arrow-key navigation and Enter to
/// accept. The popup takes key events only while open, so Enter otherwise sends the entry.
///
/// CLDR names miss the names people type (`:sob`, `:heart`).
/// [`crate::emoji::install_shortcode_expansion`] substitutes a complete `:shortcode:` against
/// gemoji names when the closing colon is typed.
///
/// Unicode emoji only. Custom channel emotes are per-stream image catalogs keyed by `emojiId`,
/// which GTK's table does not hold.
pub(crate) fn enable_emoji_shortcodes(entry: &gtk::Entry) {
    entry.set_enable_emoji_completion(true);
    crate::emoji::install_shortcode_expansion(entry);
}

mod ambient_glow;
pub use ambient_glow::AmbientGlow;
pub mod captions;
pub mod catchup_panel;
pub mod catchup_strip;
pub mod channel_view;
pub mod comments;
pub mod hover_preview;
pub mod live_chat;
mod nav_rail;
pub mod player_controls;
pub mod player_view;
pub mod playlist_view;
pub mod queue_panel;
pub mod result_row;
pub mod results_view;
mod search_filters;
pub mod shorts_shelf;
mod shorts_view;
mod watch_info;
pub use shorts_view::ShortsView;
pub use ask_panel::AskPanel;
pub use captions::CaptionOverlay;
pub use catchup_panel::CatchupPanel;
pub use catchup_strip::CatchupStrip;
pub use channel_view::ChannelView;
pub use comments::CommentsView;
pub use hover_preview::HoverPreview;
pub use live_chat::{ChatMode, LiveChatView};
pub use nav_rail::NavRail;
pub use player_controls::PlayerControls;
pub use player_view::PlayerView;
pub use playlist_view::PlaylistView;
pub use queue_panel::{QueueCard, QueuePanel};
pub use results_view::ResultsView;
pub use search_filters::SearchFilters;
pub use watch_info::WatchInfo;
