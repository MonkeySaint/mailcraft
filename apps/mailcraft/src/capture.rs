//! Screen capture for Insert › Screenshot, installed into the UI as [`mailcraft_ui_egui::compose::set_capture`].
//!
//! - Linux and the BSDs: on X11, the open windows come from the window manager (`_NET_CLIENT_LIST_STACKING`)
//!   and are read with `GetImage` (x11rb, pure Rust); the whole screen likewise. Without a compositing
//!   manager X keeps no contents for the covered parts of a window (`GetImage` would return whatever is on
//!   top, other windows the person did not choose to send), so the window is first brought to the front and
//!   a window that is still covered is refused. On Wayland windows cannot be listed by other programs, so the
//!   menu says so, and Screen Clipping asks the XDG desktop portal (`org.freedesktop.portal.Screenshot`,
//!   interactive), which shows its own picker above every window (the compose window is not minimised) and
//!   is given up after [`PORTAL_TIMEOUT`] if it never answers. X11 without a usable visual also falls back
//!   to the portal.
//! - Windows and macOS: xcap (Apache-2.0) lists and captures windows and monitors.
//! - MailCraft's own windows are listed too (the UI leaves out only the window the compose is in).
//!
//! Nothing here may panic; every failure becomes the reason the UI shows.

use mailcraft_ui_egui::compose::{CaptureWindow, Captured, ScreenCapture};

/// The platform's capture backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeCapture;

/// Installs the capture backend into the UI.
pub fn install() {
    mailcraft_ui_egui::compose::set_capture(Some(std::sync::Arc::new(NativeCapture)));
}

/// RGBA picture from an `image` buffer.
fn from_image(img: image::RgbaImage, clipped: bool) -> Captured {
    Captured { width: img.width(), height: img.height(), rgba: img.into_raw(), clipped }
}

// ---------------------------------------------------------------------------------------------------------
// Windows, macOS

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl ScreenCapture for NativeCapture {
    fn windows(&self) -> Result<Vec<CaptureWindow>, String> {
        let all = xcap::Window::all().map_err(|e| format!("The open windows cannot be listed ({e})."))?;
        Ok(all
            .iter()
            .filter(|w| !w.is_minimized().unwrap_or(true))
            .filter_map(|w| Some(CaptureWindow { id: u64::from(w.id().ok()?), title: w.title().ok()? }))
            .collect())
    }

    fn capture_window(&self, id: u64) -> Result<Captured, String> {
        let all = xcap::Window::all().map_err(|e| format!("The open windows cannot be listed ({e})."))?;
        let w = all
            .into_iter()
            .find(|w| w.id().ok().map(u64::from) == Some(id))
            .ok_or_else(|| "That window has closed.".to_owned())?;
        w.capture_image()
            .map(|img| from_image(img, false))
            .map_err(|e| format!("The window could not be captured ({e})."))
    }

    fn capture_screen(&self) -> Result<Captured, String> {
        let monitors = xcap::Monitor::all().map_err(|e| format!("The screens cannot be listed ({e})."))?;
        let primary = monitors.iter().position(|m| m.is_primary().unwrap_or(false)).unwrap_or(0);
        let m = monitors.get(primary).ok_or_else(|| "No screen was found.".to_owned())?;
        m.capture_image()
            .map(|img| from_image(img, false))
            .map_err(|e| format!("The screen could not be captured ({e})."))
    }
}

// ---------------------------------------------------------------------------------------------------------
// Linux and the BSDs

#[cfg(all(unix, not(target_os = "macos")))]
impl ScreenCapture for NativeCapture {
    fn windows(&self) -> Result<Vec<CaptureWindow>, String> {
        if wayland() {
            return Err(
                "On Wayland other programs' windows cannot be listed. Use Screen Clipping to capture \
part of the screen."
                    .to_owned(),
            );
        }
        x11::windows()
    }

    fn capture_window(&self, id: u64) -> Result<Captured, String> {
        x11::capture_window(id)
    }

    fn capture_screen(&self) -> Result<Captured, String> {
        if !wayland()
            && let Ok(shot) = x11::capture_root()
        {
            return Ok(shot);
        }
        within(PORTAL_TIMEOUT, || portal::screenshot(true))
    }

    fn clip_needs_window_hidden(&self) -> bool {
        // The portal's picker is above every window, and on Wayland a minimised window cannot be restored by
        // the program.
        !wayland()
    }
}

/// How long the desktop portal may take to answer (the person picks the region meanwhile).
#[cfg(all(unix, not(target_os = "macos")))]
pub const PORTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// Runs `f` on its own thread and waits at most `limit` for it. A call that does not finish in time is left
/// to end on its own (its answer is dropped), and the reason is returned.
#[cfg(all(unix, not(target_os = "macos")))]
fn within<T: Send + 'static>(
    limit: std::time::Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("mailcraft-portal".into())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .unwrap_or_else(|_| Err("The system screenshot tool failed.".to_owned()));
            let _ = tx.send(r);
        })
        .map_err(|e| format!("The system screenshot tool could not be asked ({e})."))?;
    match rx.recv_timeout(limit) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("The system screenshot tool did not answer.".to_owned())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("The system screenshot tool failed.".to_owned())
        }
    }
}

/// A Wayland session (X11 window listing would only see X programs).
#[cfg(all(unix, not(target_os = "macos")))]
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use mailcraft_ui_egui::compose::{CaptureWindow, Captured};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, ImageFormat, ImageOrder, MapState, Window,
        WindowClass,
    };
    use x11rb::rust_connection::RustConnection;

    /// Largest side of a capture (px), so a bogus geometry cannot ask for gigabytes.
    const MAX_SIDE: u16 = 16_384;
    /// Wait after bringing a window to the front, so it can redraw what was covered.
    const RAISE_WAIT: std::time::Duration = std::time::Duration::from_millis(350);
    /// Most top-level windows looked at when checking whether the captured one is covered.
    const MAX_STACK: usize = 4096;
    /// Deepest window nesting followed up to the top-level window.
    const MAX_DEPTH: usize = 32;

    /// A rectangle on the screen: x, y, width, height (px).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Rect {
        pub x: i32,
        pub y: i32,
        pub w: i32,
        pub h: i32,
    }

    impl Rect {
        /// Whether the two rectangles share any pixel.
        pub fn overlaps(&self, o: &Rect) -> bool {
            self.w > 0
                && self.h > 0
                && o.w > 0
                && o.h > 0
                && self.x < o.x.saturating_add(o.w)
                && o.x < self.x.saturating_add(self.w)
                && self.y < o.y.saturating_add(o.h)
                && o.y < self.y.saturating_add(self.h)
        }
    }

    /// Whether `target` is covered by any of the windows `above` it.
    pub fn covered(target: &Rect, above: &[Rect]) -> bool {
        above.iter().any(|r| r.overlaps(target))
    }

    /// Whether a compositing manager runs on `screen` (then X keeps every window's own contents, covered or
    /// not, and `GetImage` returns them).
    fn composited(conn: &RustConnection, screen: usize) -> bool {
        atom(conn, &format!("_NET_WM_CM_S{screen}"))
            .and_then(|a| conn.get_selection_owner(a).ok()?.reply().ok())
            .is_some_and(|r| r.owner != x11rb::NONE)
    }

    /// Asks the window manager to bring `win` to the front (as a pager would).
    fn activate(conn: &RustConnection, root: Window, win: Window) {
        let Some(active) = atom(conn, "_NET_ACTIVE_WINDOW") else { return };
        let event = ClientMessageEvent::new(32, win, active, [2u32, 0, 0, 0, 0]);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        if conn.send_event(false, root, mask, event).is_ok() {
            let _ = conn.flush();
        }
    }

    /// The child of `root` that holds `win` (its window-manager frame, or itself).
    fn top_level(conn: &RustConnection, root: Window, win: Window) -> Option<Window> {
        let mut cur = win;
        for _ in 0..MAX_DEPTH {
            let parent = conn.query_tree(cur).ok()?.reply().ok()?.parent;
            if parent == root || parent == x11rb::NONE {
                return Some(cur);
            }
            cur = parent;
        }
        None
    }

    /// Whether any viewable window stacked above `win` overlaps it.
    fn is_covered(conn: &RustConnection, root: Window, win: Window) -> Result<bool, String> {
        let lost = || "That window has closed.".to_owned();
        let top = top_level(conn, root, win).ok_or_else(lost)?;
        let geo = conn.get_geometry(win).ok().and_then(|c| c.reply().ok()).ok_or_else(lost)?;
        let at =
            conn.translate_coordinates(win, root, 0, 0).ok().and_then(|c| c.reply().ok()).ok_or_else(lost)?;
        let target =
            Rect { x: at.dst_x.into(), y: at.dst_y.into(), w: geo.width.into(), h: geo.height.into() };
        let stack = conn.query_tree(root).ok().and_then(|c| c.reply().ok()).ok_or_else(lost)?.children;
        let Some(pos) = stack.iter().position(|w| *w == top) else {
            return Err(lost());
        };
        // Bottom to top: the windows after the target's are above it. Ask for all, then read the answers.
        let above: Vec<Window> = stack.iter().skip(pos + 1).copied().take(MAX_STACK).collect();
        let asked: Vec<_> =
            above.iter().map(|w| (conn.get_window_attributes(*w).ok(), conn.get_geometry(*w).ok())).collect();
        let mut rects = Vec::new();
        for (attrs, geo) in asked {
            let (Some(attrs), Some(geo)) =
                (attrs.and_then(|c| c.reply().ok()), geo.and_then(|c| c.reply().ok()))
            else {
                continue;
            };
            if attrs.map_state != MapState::VIEWABLE || attrs.class == WindowClass::INPUT_ONLY {
                continue;
            }
            let border = i32::from(geo.border_width).saturating_mul(2);
            rects.push(Rect {
                x: geo.x.into(),
                y: geo.y.into(),
                w: i32::from(geo.width).saturating_add(border),
                h: i32::from(geo.height).saturating_add(border),
            });
        }
        Ok(covered(&target, &rects))
    }

    fn connect() -> Result<(RustConnection, usize), String> {
        x11rb::connect(None).map_err(|e| format!("The X display cannot be reached ({e})."))
    }

    fn atom(conn: &RustConnection, name: &str) -> Option<u32> {
        conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok().map(|r| r.atom)
    }

    fn prop32(conn: &RustConnection, win: Window, prop: u32, ty: u32) -> Vec<u32> {
        conn.get_property(false, win, prop, ty, 0, 4096)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    fn title(conn: &RustConnection, win: Window, net_name: Option<u32>, utf8: Option<u32>) -> String {
        if let (Some(n), Some(u)) = (net_name, utf8)
            && let Some(r) = conn.get_property(false, win, n, u, 0, 1024).ok().and_then(|c| c.reply().ok())
            && !r.value.is_empty()
        {
            return String::from_utf8_lossy(&r.value).into_owned();
        }
        conn.get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.value.iter().map(|b| char::from(*b)).collect())
            .unwrap_or_default()
    }

    pub fn windows() -> Result<Vec<CaptureWindow>, String> {
        let (conn, screen) = connect()?;
        let root = conn.setup().roots.get(screen).ok_or("The X screen was not found.")?.root;
        let list_atom =
            atom(&conn, "_NET_CLIENT_LIST_STACKING").ok_or("The window manager does not list windows.")?;
        let net_name = atom(&conn, "_NET_WM_NAME");
        let utf8 = atom(&conn, "UTF8_STRING");
        let state = atom(&conn, "_NET_WM_STATE");
        let hidden = atom(&conn, "_NET_WM_STATE_HIDDEN");
        let mut out = Vec::new();
        // Stacking order is bottom to top: list the top-most first.
        for win in prop32(&conn, root, list_atom, u32::from(AtomEnum::WINDOW)).into_iter().rev() {
            if let (Some(st), Some(h)) = (state, hidden)
                && prop32(&conn, win, st, u32::from(AtomEnum::ATOM)).contains(&h)
            {
                continue;
            }
            let t = title(&conn, win, net_name, utf8);
            if !t.trim().is_empty() {
                out.push(CaptureWindow { id: u64::from(win), title: t });
            }
        }
        Ok(out)
    }

    /// Reads `w` × `h` pixels of drawable `win` as RGBA (32-bit ZPixmap, the common TrueColor layout).
    fn grab(conn: &RustConnection, win: Window, w: u16, h: u16) -> Result<Captured, String> {
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err("The window has no visible area.".into());
        }
        let setup = conn.setup();
        let reply = conn
            .get_image(ImageFormat::Z_PIXMAP, win, 0, 0, w, h, u32::MAX)
            .map_err(|e| format!("The capture failed ({e})."))?
            .reply()
            .map_err(|e| format!("The capture failed ({e})."))?;
        let bpp = setup.pixmap_formats.iter().find(|f| f.depth == reply.depth).map(|f| f.bits_per_pixel);
        if bpp != Some(32) || reply.depth < 24 {
            return Err("This display's pixel format cannot be captured.".into());
        }
        let little = setup.image_byte_order == ImageOrder::LSB_FIRST;
        let n = usize::from(w) * usize::from(h);
        let mut rgba = Vec::with_capacity(n * 4);
        for &[p0, p1, p2, p3] in reply.data.as_chunks::<4>().0.iter().take(n) {
            let (r, g, b) = if little { (p2, p1, p0) } else { (p1, p2, p3) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
        if rgba.len() != n * 4 {
            return Err("The capture came back short.".into());
        }
        Ok(Captured { width: u32::from(w), height: u32::from(h), rgba, clipped: false })
    }

    pub fn capture_window(id: u64) -> Result<Captured, String> {
        let (conn, screen) = connect()?;
        let win = u32::try_from(id).map_err(|_| "That window has closed.".to_owned())?;
        let root = conn.setup().roots.get(screen).ok_or("The X screen was not found.")?.root;
        let attrs = conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .ok_or_else(|| "That window has closed.".to_owned())?;
        if attrs.map_state != MapState::VIEWABLE {
            return Err("That window is minimised or hidden. Show it, then try again.".into());
        }
        if !composited(&conn, screen) {
            // Without a compositor the covered parts would show the windows on top: bring it to the front.
            activate(&conn, root, win);
            std::thread::sleep(RAISE_WAIT);
            if is_covered(&conn, root, win)? {
                return Err("Part of that window is covered by another window. Bring it to the front, then \
try again."
                    .into());
            }
        }
        let geo = conn
            .get_geometry(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .ok_or_else(|| "That window has closed.".to_owned())?;
        grab(&conn, win, geo.width, geo.height)
    }

    pub fn capture_root() -> Result<Captured, String> {
        let (conn, screen) = connect()?;
        let s = conn.setup().roots.get(screen).ok_or("The X screen was not found.")?;
        let (root, w, h) = (s.root, s.width_in_pixels, s.height_in_pixels);
        grab(&conn, root, w, h)
    }
}

/// The XDG desktop portal's Screenshot (works on Wayland and with sandboxes such as Flatpak).
#[cfg(all(unix, not(target_os = "macos")))]
mod portal {
    use std::collections::HashMap;

    use mailcraft_ui_egui::compose::Captured;
    use zbus::zvariant::{OwnedValue, Value};

    /// Largest screenshot file read back (bytes).
    const MAX_FILE: u64 = 256 * 1024 * 1024;

    fn fail(e: impl std::fmt::Display) -> String {
        format!("The system screenshot tool is not available ({e}).")
    }

    /// Asks the portal for a screenshot; `interactive` lets the person choose what to capture.
    pub fn screenshot(interactive: bool) -> Result<Captured, String> {
        let conn = zbus::blocking::Connection::session().map_err(fail)?;
        let token = format!(
            "mailcraft{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let sender = conn
            .unique_name()
            .map(|n| n.as_str().trim_start_matches(':').replace('.', "_"))
            .ok_or_else(|| fail("no bus name"))?;
        let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        let request = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            request_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .map_err(fail)?;
        // Subscribe before asking, so the answer cannot be missed.
        let mut answers = request.receive_signal("Response").map_err(fail)?;
        let portal = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .map_err(fail)?;
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("interactive", Value::from(interactive));
        let _handle: zbus::zvariant::OwnedObjectPath =
            portal.call("Screenshot", &("", options)).map_err(fail)?;
        let msg = answers.next().ok_or_else(|| fail("no answer"))?;
        let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(fail)?;
        if code != 0 {
            return Err("The screenshot was cancelled.".into());
        }
        let uri: String = results
            .get("uri")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .ok_or_else(|| fail("no picture"))?;
        let path = file_path(&uri).ok_or_else(|| fail("the picture is not a local file"))?;
        let too_big = std::fs::metadata(&path).map(|m| m.len() > MAX_FILE).unwrap_or(true);
        if too_big {
            return Err("The screenshot file is missing or too large.".into());
        }
        let data = std::fs::read(&path).map_err(|e| format!("The screenshot cannot be read ({e})."))?;
        let img =
            image::load_from_memory(&data).map_err(|e| format!("The screenshot cannot be read ({e})."))?;
        Ok(super::from_image(img.to_rgba8(), interactive))
    }

    /// Local path of a `file://` URI (percent-decoded).
    pub fn file_path(uri: &str) -> Option<std::path::PathBuf> {
        let rest = uri.strip_prefix("file://")?;
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        if !rest.starts_with('/') {
            return None;
        }
        let bytes = rest.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            if b == b'%'
                && let Some(hex) = rest.get(i + 1..i + 3)
                && let Ok(v) = u8::from_str_radix(hex, 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
            out.push(b);
            i += 1;
        }
        Some(std::path::PathBuf::from(String::from_utf8(out).ok()?))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn file_uris_decode() {
            assert_eq!(
                super::file_path("file:///home/u/Pictures/Screenshot%20from%202026.png").unwrap(),
                std::path::PathBuf::from("/home/u/Pictures/Screenshot from 2026.png")
            );
            assert_eq!(
                super::file_path("file://localhost/tmp/a.png").unwrap(),
                std::path::PathBuf::from("/tmp/a.png")
            );
            assert!(super::file_path("https://example.com/a.png").is_none());
            assert!(super::file_path("file://relative").is_none());
            assert_eq!(
                super::file_path("file:///a%2").unwrap(),
                std::path::PathBuf::from("/a%2"),
                "a cut escape stays text"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_sets_the_backend() {
        super::install();
        assert!(mailcraft_ui_egui::compose::capture_backend().is_some());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_portal_call_that_never_answers_is_given_up() {
        // Regression: the portal wait had no limit, so a portal that never answered kept Screenshot busy for
        // good.
        let (_keep, never) = std::sync::mpsc::channel::<()>();
        let start = std::time::Instant::now();
        let r: Result<(), String> = super::within(std::time::Duration::from_millis(50), move || {
            let _ = never.recv_timeout(std::time::Duration::from_secs(5));
            Ok(())
        });
        assert_eq!(r, Err("The system screenshot tool did not answer.".to_owned()));
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
        assert_eq!(super::within(std::time::Duration::from_secs(5), || Ok(7)), Ok(7));
        assert!(
            super::within(std::time::Duration::from_secs(5), || -> Result<(), String> { panic!("x") })
                .is_err()
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_window_under_another_counts_as_covered() {
        use super::x11::{Rect, covered};
        let target = Rect { x: 100, y: 100, w: 200, h: 100 };
        assert!(!covered(&target, &[]));
        assert!(!covered(&target, &[Rect { x: 300, y: 100, w: 50, h: 50 }]), "touching edges do not overlap");
        assert!(covered(
            &target,
            &[Rect { x: 0, y: 0, w: 50, h: 50 }, Rect { x: 290, y: 190, w: 20, h: 20 }]
        ));
        assert!(!covered(&target, &[Rect { x: 150, y: 150, w: 0, h: 10 }]), "an empty window covers nothing");
        assert!(!covered(&target, &[Rect { x: i32::MAX, y: i32::MAX, w: i32::MAX, h: i32::MAX }]));
    }
}
