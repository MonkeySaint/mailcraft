//! Rich clipboard for the compose editor: the HTML flavour (`text/html`) of the system clipboard.
//!
//! egui only hands the editor plain text (`Event::Paste`). Where the platform offers HTML (native builds),
//! the app installs [`ClipboardHooks`] with [`set_clipboard_hooks`] (the desktop app uses the `arboard`
//! crate: `Clipboard::get().html()` / `set_html`), and the editor pastes the HTML through
//! `mailcraft_html::html_to_doc` (`format.paste {"html": …}`) instead of the text. Copy and Cut put the
//! selection on the clipboard as HTML with a plain-text alternative. Without hooks (web, tests) everything
//! stays plain text, as before.
//!
//! Pictures: a platform that can read a picture from the clipboard installs a reader with
//! [`set_clipboard_image_reader`] (encoded PNG / JPEG / GIF / BMP / WebP bytes). Ctrl+V with only a picture
//! on the clipboard then pastes it (`format.paste {"image": …}`), and Paste Special offers it. HTML that
//! carries its pictures (`data:` URIs, as browsers and office programs copy them) is pasted with its
//! pictures either way.

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

#[cfg(not(test))]
static HOOKS: RwLock<ClipboardHooks> =
    RwLock::new(ClipboardHooks { read_html: None, read_text: None, write_html: None });

/// Reads the clipboard's picture as encoded image bytes, when it holds one.
pub type ImageReader = fn() -> Option<Vec<u8>>;

static IMAGE_READER: RwLock<Option<ImageReader>> = RwLock::new(None);

/// Biggest clipboard picture pasted.
pub const MAX_CLIPBOARD_IMAGE: usize = 25 * 1024 * 1024;

/// Installs (or removes) the platform's clipboard picture reader.
pub fn set_clipboard_image_reader(reader: Option<ImageReader>) {
    if let Ok(mut g) = IMAGE_READER.write() {
        *g = reader;
    }
}

/// The clipboard's picture (encoded bytes), when a reader is installed and it holds one within the size cap.
pub fn clipboard_image() -> Option<Vec<u8>> {
    let reader = IMAGE_READER.read().ok().and_then(|g| *g)?;
    reader().filter(|d| !d.is_empty() && d.len() <= MAX_CLIPBOARD_IMAGE)
}

/// Most HTML read from the clipboard (bigger content is pasted as text).
pub const MAX_CLIPBOARD_HTML: usize = 8 * 1024 * 1024;

/// Installs the platform clipboard hooks (call once at start-up).
#[cfg(not(test))]
pub fn set_clipboard_hooks(h: ClipboardHooks) {
    if let Ok(mut g) = HOOKS.write() {
        *g = h;
    }
}

/// The installed hooks.
#[cfg(not(test))]
pub fn clipboard_hooks() -> ClipboardHooks {
    HOOKS.read().map(|g| *g).unwrap_or_default()
}

// Unit tests run in parallel threads, each with its own UI: their hooks are per thread so one test's fake
// clipboard never reaches another's paste.
#[cfg(test)]
thread_local! {
    static TEST_HOOKS: std::cell::Cell<ClipboardHooks> = const { std::cell::Cell::new(ClipboardHooks {
        read_html: None,
        read_text: None,
        write_html: None,
    }) };
}

/// Installs the platform clipboard hooks (call once at start-up).
#[cfg(test)]
pub fn set_clipboard_hooks(h: ClipboardHooks) {
    TEST_HOOKS.with(|c| c.set(h));
}

/// The installed hooks.
#[cfg(test)]
pub fn clipboard_hooks() -> ClipboardHooks {
    TEST_HOOKS.with(std::cell::Cell::get)
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

/// How often the system clipboard is looked at while MailCraft runs (seconds).
pub const WATCH_SECS: f64 = 1.5;

/// What the clipboard watch saw last.
#[derive(Clone, Debug, Default)]
struct Watch {
    last_check: Option<f64>,
    last_text: Option<String>,
    /// The last text collected from another program (taken back out when that program clears the clipboard).
    collected: Option<String>,
}

/// Whether `text` looks like a password or a one-time code (a password manager's copy): one short token
/// with no spaces that is either 4 to 10 numerals or mixes at least three of lower case, upper case,
/// numerals and symbols without a word-like run of lower-case letters. Addresses (`://`, `@`) are not secrets. Such copies are never collected: password
/// managers mark them as concealed on the clipboard, a mark the plain-text reader cannot see.
pub fn looks_secret(text: &str) -> bool {
    let t = text.trim();
    let n = t.chars().count();
    if !(4..=128).contains(&n) || t.chars().any(char::is_whitespace) || t.contains("://") || t.contains('@') {
        return false;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return n <= 10;
    }
    let classes = [
        t.chars().any(char::is_lowercase),
        t.chars().any(char::is_uppercase),
        t.chars().any(|c| c.is_ascii_digit()),
        t.chars().any(|c| !c.is_alphanumeric()),
    ];
    // Written words ("Q3-report") have runs of lower-case letters; generated secrets rarely do.
    let mut run = 0usize;
    let mut longest = 0usize;
    for c in t.chars() {
        run = if c.is_lowercase() { run.saturating_add(1) } else { 0 };
        longest = longest.max(run);
    }
    n >= 8 && longest < 5 && classes.iter().filter(|b| **b).count() >= 3
}

/// Whether the watch reads the system clipboard at all: a reader is installed, "Collect text copied in
/// other programs" is on, and a Clipboard pane is open unless "Collect copies while the Clipboard pane is
/// closed" is on (off by default).
pub fn watching(app: &crate::App) -> bool {
    let pane_open = app.s.composes.values().any(|c| c.editor.clipboard_pane);
    clipboard_hooks().read_text.is_some()
        && app.s.settings.bool("state.clipboard.collectOther")
        && (pane_open || app.s.settings.bool("state.clipboard.collectHidden"))
}

/// The Clipboard pane collects what is copied in other programs while MailCraft runs: every
/// [`WATCH_SECS`] the system clipboard's text is compared with what was there before, and a new copy that
/// MailCraft did not make itself goes to `format.clipboardCollect`. What was on the clipboard when the watch
/// started is not collected, and neither is anything that [`looks_secret`]. The clipboard is read only while
/// [`watching`]. When the other program clears the clipboard, the copy it had put there leaves the history
/// again.
pub fn clipboard_watch(ctx: &egui::Context, app: &mut crate::App) {
    let id = egui::Id::new("mailcraft-clipboard-watch");
    if !watching(app) {
        // Start afresh next time: what was copied meanwhile is not collected when the pane opens.
        ctx.data_mut(|d| d.remove_temp::<Watch>(id));
        return;
    }
    let now = ctx.input(|i| i.time);
    let mut w: Watch = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(WATCH_SECS));
    if w.last_check.is_some_and(|t| now - t < WATCH_SECS) {
        return;
    }
    w.last_check = Some(now);
    let text = clipboard_text().unwrap_or_default();
    let first = w.last_text.is_none();
    let changed = w.last_text.as_deref() != Some(text.as_str());
    if changed {
        w.last_text = Some(text.clone());
    }
    // Cleared by the program that put it there: take that copy back out.
    let forget = if changed && text.trim().is_empty() { w.collected.take() } else { None };
    if changed && !text.trim().is_empty() {
        w.collected = None;
    }
    // MailCraft's own copies (Copy in a message, the reading pane) are in the history already.
    let own = app.s.clipboard_text.as_deref().is_some_and(|t| t.trim() == text.trim());
    let collect = !(first || !changed || own || text.trim().is_empty() || looks_secret(&text));
    if collect {
        w.collected = Some(text.clone());
    }
    ctx.data_mut(|d| d.insert_temp(id, w));
    if let Some(gone) = forget {
        if app.run("format.clipboardCollect", serde_json::json!({"text": gone, "forget": true})).is_some() {
            ctx.request_repaint();
        }
        return;
    }
    if !collect {
        return;
    }
    let mut p = serde_json::json!({"text": text, "source": "other"});
    if let (Some(html), Some(o)) = (html_for_paste(&text), p.as_object_mut()) {
        o.insert("html".into(), serde_json::json!(html));
    }
    if app.run("format.clipboardCollect", p).is_some() {
        ctx.request_repaint();
    }
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

    fn png() -> Option<Vec<u8>> {
        Some(b"\x89PNG\r\n\x1a\n....".to_vec())
    }

    fn huge() -> Option<Vec<u8>> {
        Some(vec![0u8; MAX_CLIPBOARD_IMAGE + 1])
    }

    #[test]
    fn picture_reader_is_optional_and_capped() {
        assert!(clipboard_image().is_none());
        set_clipboard_image_reader(Some(png));
        assert_eq!(clipboard_image().map(|d| d.len()), Some(12));
        set_clipboard_image_reader(Some(huge));
        assert!(clipboard_image().is_none());
        set_clipboard_image_reader(None);
        assert!(clipboard_image().is_none());
    }
}
