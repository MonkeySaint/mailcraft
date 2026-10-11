//! System clipboard HTML for the compose editor (`mailcraft_ui_egui::compose::set_clipboard_hooks`): egui
//! only exchanges plain text, so rich paste and copy go through `arboard`. One clipboard handle is kept for
//! the life of the process (on X11 the copied data is served by it), created on first use. Every failure
//! (no display server, clipboard busy, no HTML flavour) is just "nothing": the editor falls back to text.

use std::sync::Mutex;

use mailcraft_ui_egui::compose::{ClipboardHooks, set_clipboard_hooks};

static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

/// Runs `f` with the shared clipboard handle, opening it on first use.
fn with_clipboard<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
    let mut guard = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        match arboard::Clipboard::new() {
            Ok(c) => *guard = Some(c),
            Err(e) => {
                log::debug!("system clipboard unavailable: {e}");
                return None;
            }
        }
    }
    guard.as_mut().and_then(f)
}

fn read_html() -> Option<String> {
    with_clipboard(|c| c.get().html().ok())
}

fn read_text() -> Option<String> {
    with_clipboard(|c| c.get_text().ok())
}

fn write_html(html: &str, text: &str) -> bool {
    with_clipboard(|c| c.set().html(html, Some(text)).ok()).is_some()
}

/// Installs the hooks (call once at start-up, before the window opens).
pub fn install() {
    set_clipboard_hooks(ClipboardHooks {
        read_html: Some(read_html),
        read_text: Some(read_text),
        write_html: Some(write_html),
    });
}
