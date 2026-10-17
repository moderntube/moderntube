//! `:shortcode:` → emoji for the chat and comment composers.
//!
//! Two layers cover what people type:
//!
//! * GTK's native completion (`GtkEntry:enable-emoji-completion`, wired in
//!   [`crate::widgets::enable_emoji_shortcodes`]) gives the popup, arrow-key navigation and
//!   Enter-to-accept for a partial shortcode, from the table shipped inside libgtk.
//! * This module substitutes a complete `:shortcode:` when its closing colon is typed, using
//!   the gemoji (GitHub) names.
//!
//! GTK's completion matches CLDR names only. 😭 is `loudly crying face`, so
//! `:loudly_crying_face` completes but `:sob` does not: `sob` is only a keyword, and keywords
//! feed the emoji chooser's search. `:sob:` and `:heart:` are gemoji names. Resolving GTK's
//! keywords instead picks the wrong emoji (`heart` gives 🥰 by file order, 💏🏻 by shortest
//! name), so the `emojis` crate supplies the gemoji mapping.

use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::Cell;
use std::rc::Rc;

/// Longest shortcode accepted, in bytes. Gemoji names are well under it.
const MAX_SHORTCODE_LEN: usize = 40;

/// Resolves a bare shortcode (no surrounding colons, e.g. `sob`) to its emoji.
///
/// Case-insensitive: people type `:SOB:` and gemoji names are lowercase.
pub fn lookup(shortcode: &str) -> Option<&'static str> {
    if shortcode.is_empty() || shortcode.len() > MAX_SHORTCODE_LEN {
        return None;
    }
    let lower = shortcode.to_ascii_lowercase();
    emojis::get_by_shortcode(&lower).map(|e| e.as_str())
}

/// If `text` ends with a complete `:shortcode:`, return the byte offset where that run
/// starts plus the emoji it resolves to.
///
/// Only the end of the text is considered: substitution fires on the keystroke that closes
/// the shortcode.
fn trailing_shortcode(text: &str) -> Option<(usize, &'static str)> {
    let rest = text.strip_suffix(':')?;
    // The nearest earlier colon opens the shortcode.
    let open = rest.rfind(':')?;
    let name = &rest[open + 1..];
    // A shortcode has no whitespace, so `12:30 - 4:` is not read as `30 - 4`.
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return None;
    }
    lookup(name).map(|emoji| (open, emoji))
}

/// Substitutes a complete `:shortcode:` in a composer [`gtk::Entry`] when the closing colon
/// is typed.
///
/// The completion popup handles partial input. This handles names typed straight through,
/// such as `:heart:`, which GTK's table lacks. Fires only when the cursor is at the end of
/// the text.
pub fn install_shortcode_expansion(entry: &gtk::Entry) {
    // Guards re-entry: `set_text` emits `changed` again.
    let busy = Rc::new(Cell::new(false));
    entry.connect_changed(move |entry| {
        if busy.get() {
            return;
        }
        let text = entry.text();
        let text = text.as_str();
        // Only when the cursor is at the end; otherwise the user is editing earlier text.
        if entry.position() as usize != text.chars().count() {
            return;
        }
        let Some((start, emoji)) = trailing_shortcode(text) else { return };
        let mut next = String::with_capacity(start + emoji.len());
        next.push_str(&text[..start]);
        next.push_str(emoji);
        busy.set(true);
        entry.set_text(&next);
        entry.set_position(-1); // caret to the end, after the inserted emoji
        busy.set(false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemoji_names_gtk_lacks_resolve() {
        // GTK's completion matches CLDR names, so it finds `:loudly_crying_face:` but never
        // `:sob:`, and has no `heart` name at all.
        assert_eq!(lookup("sob"), Some("😭"));
        assert_eq!(lookup("heart"), Some("❤️"));
        // Uppercase also resolves.
        assert_eq!(lookup("SOB"), Some("😭"));
    }

    #[test]
    fn only_a_complete_trailing_shortcode_is_replaced() {
        assert_eq!(trailing_shortcode("hello :sob:"), Some((6, "😭")));
        assert_eq!(trailing_shortcode(":heart:"), Some((0, "❤️")));
        // Still being typed: left to the popup.
        assert_eq!(trailing_shortcode("hello :sob"), None);
        // Unknown name: leave the user's text alone rather than eating the colons.
        assert_eq!(trailing_shortcode("hello :nope:"), None);
    }

    #[test]
    fn ordinary_colon_text_is_never_replaced() {
        assert_eq!(trailing_shortcode("meet at 12:30 -- 4:"), None);
        assert_eq!(trailing_shortcode("ratio 16:9:"), None);
        assert_eq!(trailing_shortcode("::"), None);
        assert_eq!(trailing_shortcode(":"), None);
    }

    #[test]
    fn a_second_shortcode_after_an_earlier_one_still_fires() {
        // The nearest opening colon binds, not the first in the line.
        let text = "😭 and now :fire:";
        let (start, emoji) = trailing_shortcode(text).expect("second shortcode resolves");
        assert_eq!(emoji, "🔥");
        assert_eq!(&text[start..], ":fire:");
    }
}
