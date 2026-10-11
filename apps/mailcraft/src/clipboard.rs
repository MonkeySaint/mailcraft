//! System clipboard HTML for the compose editor (`mailcraft_ui_egui::compose::set_clipboard_hooks`): egui
//! only exchanges plain text, so rich paste and copy go through `arboard`. One clipboard handle is kept for
//! the life of the process (on X11 the copied data is served by it), created on first use. Every failure
//! (no display server, clipboard busy, no HTML flavour) is just "nothing": the editor falls back to text.
//!
//! Pictures: arboard hands over a clipboard picture as raw RGBA pixels; [`read_image`] encodes them as PNG for
//! the editor's picture paste (`mailcraft_ui_egui::compose::set_clipboard_image_reader`). Pictures bigger
//! than [`MAX_PICTURE_SIDE`] a side, or whose pixel data does not match their size, are not pasted.

use std::sync::Mutex;

use mailcraft_ui_egui::compose::{ClipboardHooks, set_clipboard_hooks, set_clipboard_image_reader};

/// Longest side of a clipboard picture that is pasted (pixels).
pub const MAX_PICTURE_SIDE: usize = 16_384;
/// Most pixels of a clipboard picture that is pasted.
pub const MAX_PICTURE_PIXELS: usize = 64 * 1024 * 1024;

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

/// The clipboard's picture as PNG bytes, when it holds one.
fn read_image() -> Option<Vec<u8>> {
    let img = with_clipboard(|c| c.get_image().ok())?;
    encode_png(img.width, img.height, &img.bytes)
}

/// Encodes `width` × `height` RGBA pixels (row-major, 4 bytes each) as PNG; `None` for an empty or oversized
/// picture, pixel data of the wrong length, or an encoder failure.
pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_PICTURE_SIDE || height > MAX_PICTURE_SIDE {
        return None;
    }
    let pixels = width.checked_mul(height).filter(|n| *n <= MAX_PICTURE_PIXELS)?;
    if pixels.checked_mul(4)? != rgba.len() {
        return None;
    }
    let img =
        image::RgbaImage::from_raw(u32::try_from(width).ok()?, u32::try_from(height).ok()?, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Installs the hooks (call once at start-up, before the window opens).
pub fn install() {
    set_clipboard_hooks(ClipboardHooks {
        read_html: Some(read_html),
        read_text: Some(read_text),
        write_html: Some(write_html),
    });
    set_clipboard_image_reader(Some(read_image));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_pixels_become_png() {
        let rgba: Vec<u8> = (0..2 * 3).flat_map(|i| [i as u8 * 40, 0, 255, 255]).collect();
        let png = encode_png(2, 3, &rgba).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let back = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!((back.width(), back.height()), (2, 3));
        assert_eq!(back.into_raw(), rgba);
    }

    #[test]
    fn bad_clipboard_pixels_are_refused() {
        assert!(encode_png(0, 3, &[]).is_none());
        assert!(encode_png(2, 2, &[0; 15]).is_none(), "short pixel data");
        assert!(encode_png(2, 2, &[0; 17]).is_none(), "long pixel data");
        assert!(encode_png(MAX_PICTURE_SIDE + 1, 1, &[0; 4]).is_none());
        assert!(encode_png(usize::MAX, usize::MAX, &[]).is_none());
    }
}
