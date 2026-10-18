//! App-wide theming: one CssProvider driven by an accent colour and an OLED flag, plus
//! libadwaita colour-scheme selection. The component styles share the stylesheet, so they
//! reload with the accent.
use gtk4 as gtk;

pub const DEFAULT_ACCENT: &str = "#cc0000";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
    Oled,
}

impl ThemeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Oled => "oled",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "oled" => Self::Oled,
            _ => Self::System,
        }
    }
    fn color_scheme(&self) -> adw::ColorScheme {
        match self {
            Self::Light => adw::ColorScheme::ForceLight,
            Self::Dark | Self::Oled => adw::ColorScheme::ForceDark,
            Self::System => adw::ColorScheme::Default,
        }
    }
    fn is_oled(&self) -> bool {
        matches!(self, Self::Oled)
    }
}

/// Validate a `#rrggbb` (case-insensitive, leading `#` optional) → lowercased `#rrggbb`,
/// or DEFAULT_ACCENT if malformed.
pub fn normalize_accent(input: &str) -> String {
    let h = input.trim().trim_start_matches('#');
    if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("#{}", h.to_ascii_lowercase())
    } else {
        DEFAULT_ACCENT.to_string()
    }
}

/// The full app stylesheet: accent named-color overrides, optional OLED background
/// overrides, and the component styles (chat overlay, badges, miniplayer, etc.).
pub fn build_css(accent: &str, oled: bool) -> String {
    let accent = normalize_accent(accent);
    let mut css = format!(
        "@define-color accent_bg_color {accent}; \
         @define-color accent_color {accent}; \
         @define-color accent_fg_color #ffffff; "
    );
    if oled {
        css.push_str(
            "@define-color window_bg_color #000000; \
             @define-color view_bg_color #000000; \
             @define-color headerbar_bg_color #000000; \
             @define-color card_bg_color #0a0a0a; ",
        );
    }
    css.push_str(
        ".chat-overlay { background-color: rgba(16,16,16,0.80); border-radius: 12px; border: 1px solid rgba(255,255,255,0.10); } \
         .chat-overlay text { background-color: transparent; color: #f1f1f1; } \
         .chat-overlay scrolledwindow, .chat-overlay textview { background-color: transparent; } \
         .duration-badge { background-color: rgba(0,0,0,0.80); color: #fff; font-size: 0.78em; font-weight: bold; padding: 1px 4px; border-radius: 4px; } \
         .live-badge { background-color: #cc0000; color: #fff; font-size: 0.78em; font-weight: bold; padding: 1px 5px; border-radius: 4px; } \
         .miniplayer { border-radius: 12px; border: 1px solid rgba(255,255,255,0.14); box-shadow: 0 8px 28px rgba(0,0,0,0.55); background-color: #000; } \
         .mini-ctl { background-color: rgba(0,0,0,0.62); color: #fff; min-width: 30px; min-height: 30px; padding: 4px; border-radius: 999px; } \
         .mini-ctl:hover { background-color: rgba(70,70,70,0.92); } \
         .queue-count { font-size: 0.72em; font-weight: bold; padding-right: 2px; } \
         .search-filter { padding: 4px 8px; font-weight: normal; } \
         .search-filter.selected { font-weight: bold; color: @accent_color; } \
         .search-filters-active > button { color: @accent_color; } \
         .queue-current { background-color: rgba(255,255,255,0.07); border-radius: 6px; } \
         .queue-current .heading { color: @accent_color; } \
         .queue-card { border-radius: 12px; } \
         .queue-card list, .queue-card scrolledwindow { background: transparent; } \
         .queue-card row { padding: 2px 4px; background: transparent; } \
         .queue-card .queue-thumb { border-radius: 6px; } \
         .queue-card button.flat { min-height: 22px; padding: 0 8px; } \
         .queue-card .queue-ctl button { min-width: 24px; min-height: 24px; padding: 0; } \
         .comment-like { padding: 0 4px; min-height: 0; } \
         .comment-like.liked { color: @accent_color; font-weight: bold; } \
         .channel-avatar { border-radius: 999px; margin: 8px 0; } \
         .speed-badge { background-color: rgba(0,0,0,0.78); color: #fff; font-size: 0.95em; font-weight: bold; padding: 4px 12px; border-radius: 999px; }\n\
         .scrub-preview { background-color: rgba(0,0,0,0.82); border-radius: 10px; padding: 4px; border: none; }\n\
         .scrub-preview picture { border-radius: 8px; }\n\
         .video-frame { border-radius: 12px; }\n\
         .fullscreen-player { background-color: black; }\n\
         .preview-cover { background-color: #000; }\n\
         .catchup-strip { background-color: alpha(@accent_bg_color, 0.12); }\n\
         .catchup-strip.error { background-color: alpha(#e01b24, 0.20); }\n\
         .catchup-strip.error progressbar progress { background-color: #e01b24; }\n\
         .catchup-strip label { font-size: 0.86em; }\n\
         .catchup-strip button { min-height: 20px; min-width: 20px; padding: 0 4px; }\n\
         .catchup-strip progressbar trough, .catchup-strip progressbar progress { min-height: 3px; }\n\
         .player-bar { background-image: linear-gradient(to bottom, alpha(black,0), alpha(black,0.45) 40%, alpha(black,0.78)); padding: 36px 12px 4px 12px; border-radius: 0 0 12px 12px; color: #fff; }\n\
         .player-bar button, .player-bar menubutton > button { background: none; box-shadow: none; border: none; color: #fff; min-width: 36px; min-height: 36px; padding: 0; border-radius: 999px; transition: background-color 120ms ease-out; }\n\
         .player-bar button:hover, .player-bar menubutton > button:hover { background-color: alpha(white,0.14); }\n\
         .player-bar button:active, .player-bar button:checked { background-color: alpha(white,0.22); }\n\
         .player-bar button:checked { color: @accent_color; }\n\
         .player-bar .times { margin: 0 8px; }\n\
         .player-bar .time { font-size: 0.88em; font-weight: 500; font-feature-settings: \"tnum\"; text-shadow: 0 1px 2px alpha(black,0.6); }\n\
         .player-bar .time.dim { color: alpha(white,0.62); }\n\
         .player-bar .live-pill { min-height: 26px; padding: 0 10px; margin: 0 4px; font-size: 0.78em; font-weight: 800; letter-spacing: 0.06em; background-color: alpha(white,0.10); }\n\
         .player-bar .quality-chip > button { padding: 0 10px; font-size: 0.8em; font-weight: 700; }\n\
         .player-bar .volume-slider { margin: 0 6px 0 2px; }\n\
         .player-bar .volume-slider trough { min-height: 3px; background-color: alpha(white,0.25); border-radius: 999px; }\n\
         .player-bar .volume-slider highlight { background-color: #fff; border-radius: 999px; }\n\
         .player-bar .volume-slider slider { min-width: 11px; min-height: 11px; margin: -4px; background: #fff; border: none; box-shadow: none; }\n\
         .player-seek { margin: 0 2px 2px 2px; }\n\
         .player-seek scale { padding: 6px 0; }\n\
         .player-seek scale trough { min-height: 3px; background-color: alpha(white,0.24); border-radius: 999px; transition: min-height 120ms ease-out; }\n\
         .player-seek:hover scale trough { min-height: 5px; }\n\
         .player-seek scale highlight { background-image: linear-gradient(to right, shade(@accent_color,0.85), @accent_color); border-radius: 999px; }\n\
         .player-seek scale slider { min-width: 0; min-height: 0; margin: 0; background: @accent_color; border: none; box-shadow: none; transition: all 120ms ease-out; }\n\
         .player-seek:hover scale slider { min-width: 14px; min-height: 14px; margin: -5px; box-shadow: 0 0 0 5px alpha(@accent_color,0.28); }",
    );
    css.push_str(SHORTS_CSS);
    css
}

/// The Shorts page, after YouTube's desktop Shorts layout (12px card, 48px action buttons,
/// 56px edge navigation, a bordered comments sheet).
const SHORTS_CSS: &str = "
.shorts-card { background-color: #000; border-radius: 12px; box-shadow: 0 14px 44px alpha(black,0.45); }
.shorts-meta { background-image: linear-gradient(to bottom, alpha(black,0), alpha(black,0.55) 45%, alpha(black,0.82)); padding: 48px 16px 18px 16px; }
.shorts-channel { color: #fff; font-weight: 600; font-size: 0.95em; text-shadow: 0 1px 2px alpha(black,0.6); }
.shorts-title { color: #fff; font-size: 1.0em; text-shadow: 0 1px 2px alpha(black,0.6); }
.shorts-status { color: alpha(white,0.75); font-size: 0.85em; }
.shorts-ctl { background-color: alpha(black,0.45); color: #fff; min-width: 44px; min-height: 44px; padding: 0; border-radius: 999px; border: none; box-shadow: none; transition: background-color 120ms ease-out; }
.shorts-ctl:hover { background-color: alpha(black,0.65); }
.shorts-center-icon { background-color: alpha(black,0.55); color: #fff; padding: 22px; border-radius: 999px; }
.shorts-progress { min-height: 3px; }
.shorts-progress trough { min-height: 3px; background-color: alpha(white,0.25); border-radius: 0; }
.shorts-progress progress { min-height: 3px; background-color: @accent_color; border-radius: 0; }
.shorts-round { min-width: 48px; min-height: 48px; padding: 0; border-radius: 999px; background-color: alpha(currentColor,0.10); border: none; box-shadow: none; transition: background-color 120ms ease-out, color 120ms ease-out; }
.shorts-round:hover { background-color: alpha(currentColor,0.18); }
.shorts-round:checked { background-color: alpha(currentColor,0.18); }
.shorts-round.liked { color: @accent_color; }
.shorts-action-caption { font-size: 0.8em; font-weight: 500; }
.shorts-nav { min-width: 56px; min-height: 56px; padding: 0; border-radius: 999px; background-color: alpha(currentColor,0.08); border: none; box-shadow: none; }
.shorts-nav:hover { background-color: alpha(currentColor,0.16); }
.shorts-comments { background-color: @card_bg_color; border: 1px solid alpha(currentColor,0.12); border-radius: 12px; }
.shorts-comments-header { padding: 10px 8px 10px 16px; }
.shorts-comments-body { background: transparent; padding-top: 8px; }
.shorts-comments scrolledwindow { background: transparent; border-radius: 0 0 12px 12px; }
.feed-list, .feed-list > row { background: transparent; padding: 0; }
.feed-list > row:hover, .feed-list > row:selected { background: transparent; }
.card-avatar { border-radius: 999px; }
.channel-header { padding: 12px 16px 4px 16px; }
.channel-banner { border-radius: 16px; margin-bottom: 18px; }
.channel-page-avatar { border-radius: 999px; }
.channel-identity { margin: 0 4px 14px 4px; }
.channel-name { font-size: 2.1em; font-weight: 800; letter-spacing: -0.02em; }
.channel-verified { font-size: 0.9em; color: alpha(currentColor,0.65); background-color: alpha(currentColor,0.12); border-radius: 999px; padding: 1px 6px; }
.channel-inline-link { padding: 0 4px; min-height: 0; font-weight: 600; }
.channel-link { padding: 2px 6px; min-height: 0; color: #3ea6ff; }
.channel-subscribe { background-color: #f1f1f1; color: #0f0f0f; font-weight: 600; border-radius: 999px; padding: 6px 18px; box-shadow: none; }
.channel-subscribe:hover { background-color: #d9d9d9; }
.channel-subscribe.subscribed { background-color: alpha(currentColor,0.12); color: inherit; }
.channel-tabs { border-bottom: 1px solid alpha(currentColor,0.14); margin: 0 4px; }
.channel-tab { border-radius: 0; padding: 10px 14px; font-weight: 600; color: alpha(currentColor,0.65); border-bottom: 2px solid transparent; }
.channel-tab.active { color: inherit; border-bottom-color: currentColor; }
.channel-chips { margin: 14px 4px 6px 4px; }
.channel-chip { border-radius: 8px; padding: 4px 12px; min-height: 28px; font-weight: 600; background-color: alpha(currentColor,0.10); box-shadow: none; }
.channel-chip.selected { background-color: #f1f1f1; color: #0f0f0f; }
.channel-back { margin: 18px 0 0 22px; }
.shorts-grid { border: none; margin: 0; padding-top: 6px; }
.watch-pill, box.watch-pill-group > button { background-color: alpha(currentColor,0.10); border-radius: 999px; padding: 4px 14px; min-height: 28px; box-shadow: none; }
.watch-pill:hover, box.watch-pill-group > button:hover { background-color: alpha(currentColor,0.18); }
box.watch-pill-group > button:first-child { border-radius: 999px 0 0 999px; border-right: 1px solid alpha(currentColor,0.15); }
box.watch-pill-group > button:last-child { border-radius: 0 999px 999px 0; }
.description-box { background-color: alpha(currentColor,0.08); border-radius: 12px; padding: 10px 12px; }
.comment-replies-toggle { color: #3ea6ff; font-weight: 600; padding-left: 4px; padding-right: 8px; }
.feed-card { border-radius: 10px; transition: background-color 120ms ease-out; }
.feed-card:hover { background-color: alpha(@accent_bg_color,0.25); }
.shorts-shelf { padding: 16px 6px 20px 6px; border-top: 1px solid alpha(currentColor,0.10); border-bottom: 1px solid alpha(currentColor,0.10); margin: 8px 0; }
.shorts-shelf-icon { color: @accent_color; -gtk-icon-size: 22px; }
.short-card .card { border-radius: 10px; }
.shorts-shelf-more { min-width: 340px; }
";

/// Owns the app-wide provider so the accent/OLED CSS can be rebuilt live.
pub struct Theme {
    provider: gtk::CssProvider,
}

impl Theme {
    /// Create the provider and register it at APPLICATION priority (so `@define-color`
    /// overrides adw's named colors). Call once at startup, then `apply`.
    pub fn install() -> Self {
        let provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        Self { provider }
    }

    /// Set the libadwaita color scheme and reload the accent/OLED stylesheet.
    pub fn apply(&self, mode: ThemeMode, accent: &str) {
        adw::StyleManager::default().set_color_scheme(mode.color_scheme());
        self.provider
            .load_from_string(&build_css(accent, mode.is_oled()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trips_and_defaults() {
        for m in ["system", "light", "dark", "oled"] {
            assert_eq!(ThemeMode::from_str(m).as_str(), m);
        }
        assert_eq!(ThemeMode::from_str("garbage").as_str(), "system");
    }

    #[test]
    fn accent_is_normalized_or_defaulted() {
        assert_eq!(normalize_accent("#3B82F6"), "#3b82f6");
        assert_eq!(normalize_accent("cc0000"), "#cc0000");
        assert_eq!(normalize_accent("not-a-color"), DEFAULT_ACCENT);
        assert_eq!(normalize_accent("#12345"), DEFAULT_ACCENT); // wrong length
    }
}
