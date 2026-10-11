//! Rich clipboard for the compose editor: the HTML flavour (`text/html`) of the system clipboard.
//!
//! egui only hands the editor plain text (`Event::Paste`). Where the platform offers HTML (native builds),
//! the app installs [`ClipboardHooks`] with [`set_clipboard_hooks`] (the desktop app uses the `arboard`
//! crate: `Clipboard::get().html()` / `set_html`), and the editor pastes the HTML through
//! `mailcraft_html::html_to_doc` (`format.paste {"html": …}`) instead of the text. Copy and Cut put the
//! selection on the clipboard as HTML with a plain-text alternative. Without hooks (web, tests) everything
//! stays plain text, as before.

use std::sync::RwLock;

/// Platform clipboard access beyond plain text.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClipboardHooks {
    /// The clipboard's HTML flavour, if any.
    pub read_html: Option<fn() -> Option<String>>,
    /// The clipboard's plain text (Paste Special › Unformatted Text from the dialog, where no paste event
    /// arrives).
    pub read_text: Option<fn() -> Option<String>>,
    /// Puts `html` with the plain-text alternative `text` on the clipboard; false when that failed.
    pub write_html: Option<fn(html: &str, text: &str) -> bool>,
}

static HOOKS: RwLock<ClipboardHooks> =
    RwLock::new(ClipboardHooks { read_html: None, read_text: None, write_html: None });

/// Most HTML read from the clipboard (bigger content is pasted as text).
pub const MAX_CLIPBOARD_HTML: usize = 8 * 1024 * 1024;

/// Installs the platform clipboard hooks (call once at start-up).
pub fn set_clipboard_hooks(h: ClipboardHooks) {
    if let Ok(mut g) = HOOKS.write() {
        *g = h;
    }
}

/// The installed hooks.
pub fn clipboard_hooks() -> ClipboardHooks {
    HOOKS.read().map(|g| *g).unwrap_or_default()
}

/// The clipboard's HTML, when a reader is installed and it holds some (capped).
pub fn clipboard_html() -> Option<String> {
    let html = (clipboard_hooks().read_html?)()?;
    (!html.trim().is_empty() && html.len() <= MAX_CLIPBOARD_HTML).then_some(html)
}

/// The clipboard's plain text, when a reader is installed.
pub fn clipboard_text() -> Option<String> {
    (clipboard_hooks().read_text?)()
}

/// Puts `html` (plain alternative `text`) on the clipboard; false without a writer or when it failed.
pub fn copy_html(html: &str, text: &str) -> bool {
    clipboard_hooks().write_html.is_some_and(|w| w(html, text))
}

/// Letters and digits of `s`, lower-cased (list markers, punctuation and spacing differ between the HTML
/// and text flavours of the same copy).
fn letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The HTML to paste for a paste event that carried `text`: the clipboard's HTML when it holds the same
/// text (so a stale or unrelated HTML flavour is never pasted), else `None`.
pub fn html_for_paste(text: &str) -> Option<String> {
    let html = clipboard_html()?;
    let (a, b) = (letters(&mailcraft_html::html_to_text(&html)), letters(text));
    let same = b.is_empty()
        || a == b
        || (!a.is_empty()
            && (a.contains(&b) || b.contains(&a))
            && a.len().abs_diff(b.len()) * 5 <= a.len().max(b.len()));
    same.then_some(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html() -> Option<String> {
        Some("<p>Hello <b>big</b>\nworld</p>".into())
    }

    #[test]
    fn html_is_pasted_only_when_it_matches_the_text() {
        set_clipboard_hooks(ClipboardHooks { read_html: Some(html), ..Default::default() });
        assert!(html_for_paste("Hello big world").is_some());
        assert!(html_for_paste("Hello  big\nworld").is_some());
        assert!(html_for_paste("something else").is_none());
        set_clipboard_hooks(ClipboardHooks::default());
        assert!(html_for_paste("Hello big world").is_none());
        assert!(!copy_html("<b>x</b>", "x"));
    }
}
