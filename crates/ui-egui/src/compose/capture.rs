//! Insert › Screenshot: the open windows and Screen Clipping, through a platform capture backend the desktop
//! app installs with [`set_capture`] (the engine cannot capture; the web build and tests have no backend, so
//! Screenshot falls back to inserting a saved picture).
//!
//! - The Screenshot menu lists [`window_items`] (cached for a second, so an open menu does not enumerate the
//!   windows every frame). A backend that cannot list windows says why; the menu then shows that reason as
//!   one disabled-looking entry that opens the saved-picture dialog with it.
//! - A capture runs on a worker thread (a system screenshot dialog can block for as long as the person takes)
//!   and is picked up by [`poll`] on a later frame; one that has not come back after [`CAPTURE_TIMEOUT`] is
//!   given up (the saved-picture dialog opens instead, so Screenshot never stays stuck). Screen Clipping
//!   first minimises the compose window so it is not in the shot, and restores it when the capture is back,
//!   unless the backend says the system's own tool shows its picker above every window
//!   ([`ScreenCapture::clip_needs_window_hidden`]; the desktop portal on Wayland, where a minimised window
//!   cannot be brought back by the program).
//! - The menu lists every window but the one the compose is in (MailCraft's main window and other windows
//!   are listed, as other programs' are).
//! - A window capture, or a clip the system already cut out ([`Captured::clipped`]), is inserted as a PNG
//!   via `insert.screenshot`; a whole-screen capture opens the Screen Clipping dialog to drag the region.
//! - Failures go to `insert.screenshot {"unavailable": reason}`, which offers a saved picture instead.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use egui::ViewportCommand;
use serde_json::{Value, json};

use crate::App;

/// A window the backend can capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureWindow {
    /// Backend's id for [`ScreenCapture::capture_window`].
    pub id: u64,
    /// Window title (what the menu shows).
    pub title: String,
}

/// A captured picture: RGBA rows, `width` × `height`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Captured {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The system's own screenshot tool already let the person pick the region (no Screen Clipping dialog).
    pub clipped: bool,
}

impl Captured {
    /// Whether the pixel buffer matches the size (backends must never hand over a short buffer, but a wrong
    /// one must not crash the encoder).
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && u64::from(self.width).saturating_mul(u64::from(self.height)).saturating_mul(4)
                == self.rgba.len() as u64
    }
}

/// Platform screen capture. Implementations must not panic; every method may block (it runs off the UI
/// thread except [`ScreenCapture::windows`]).
pub trait ScreenCapture: Send + Sync {
    /// Windows that can be captured, top-most first; `Err` says why windows cannot be listed here.
    fn windows(&self) -> Result<Vec<CaptureWindow>, String>;
    /// Captures window `id`.
    fn capture_window(&self, id: u64) -> Result<Captured, String>;
    /// Screen Clipping: the screen to clip from, or (with [`Captured::clipped`]) the region the system's
    /// screenshot tool let the person choose.
    fn capture_screen(&self) -> Result<Captured, String>;
    /// Whether Screen Clipping must first move the compose window out of the way (minimise it). False when
    /// the system's screenshot tool shows its own picker above every window, as the desktop portal does.
    fn clip_needs_window_hidden(&self) -> bool {
        true
    }
}

static BACKEND: RwLock<Option<Arc<dyn ScreenCapture>>> = RwLock::new(None);

/// Installs (or with `None` removes) the capture backend.
pub fn set_capture(backend: Option<Arc<dyn ScreenCapture>>) {
    *BACKEND.write().unwrap_or_else(PoisonError::into_inner) = backend;
    *WINDOWS.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// The installed capture backend.
pub fn capture_backend() -> Option<Arc<dyn ScreenCapture>> {
    BACKEND.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Reason given when no backend is installed.
pub const NO_BACKEND: &str = "This version of MailCraft cannot capture the screen.";
/// Seconds the window list stays cached.
const WINDOW_CACHE: Duration = Duration::from_secs(1);
/// Most windows listed in the menu.
pub const MAX_WINDOWS: usize = 30;
/// Wait after minimising the compose window before a Screen Clipping capture (the window animates away).
const MINIMIZE_WAIT: Duration = Duration::from_millis(450);
/// Largest picture accepted from a backend (pixels).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
/// A capture not back after this long is given up (a system screenshot tool that never answers).
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(180);
/// Reason shown when a capture is given up.
pub const CAPTURE_TIMED_OUT: &str = "The screen capture did not finish.";

type WindowCache = Option<(Instant, Result<Vec<CaptureWindow>, String>)>;
static WINDOWS: Mutex<WindowCache> = Mutex::new(None);

/// The Screenshot menu's windows (or why there are none), without the window titled `own` (the window the
/// compose is in).
pub fn window_items(own: Option<&str>) -> Result<Vec<CaptureWindow>, String> {
    all_windows().map(|mut list| {
        if let Some(own) = own {
            list.retain(|w| w.title.trim() != own.trim());
        }
        list.truncate(MAX_WINDOWS);
        list
    })
}

fn all_windows() -> Result<Vec<CaptureWindow>, String> {
    let Some(backend) = capture_backend() else {
        return Err(NO_BACKEND.to_owned());
    };
    let mut cache = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, list)) = cache.as_ref()
        && at.elapsed() < WINDOW_CACHE
    {
        return list.clone();
    }
    let list = backend.windows().map(|mut w| {
        w.retain(|w| !w.title.trim().is_empty());
        // One more than shown, so the list stays full after the compose's own window is left out.
        w.truncate(MAX_WINDOWS + 1);
        w
    });
    *cache = Some((Instant::now(), list.clone()));
    list
}

/// A capture in flight for compose `cid`.
struct Job {
    cid: u64,
    window: Option<String>,
    clip: bool,
    minimized: bool,
    started: Instant,
    rx: Receiver<Result<Captured, String>>,
}

type Jobs = Arc<Mutex<Vec<Job>>>;

/// The captures in flight in this egui context (each window system context, and each test harness, has its
/// own).
fn jobs(ctx: &egui::Context) -> Jobs {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(egui::Id::new("mailcraft-capture-jobs"), Jobs::default).clone()
    })
}
/// Whole-screen captures waiting in the Screen Clipping dialog, by token.
static CLIPS: Mutex<Vec<(u64, Arc<Captured>)>> = Mutex::new(Vec::new());
/// Most screen captures kept for open Screen Clipping dialogs.
const MAX_CLIPS: usize = 4;

/// Whether compose `cid` has a capture running.
pub fn busy(ctx: &egui::Context, cid: u64) -> bool {
    jobs(ctx).lock().unwrap_or_else(PoisonError::into_inner).iter().any(|j| j.cid == cid)
}

/// Whether the capture running for compose `cid` minimised its window (`None` when none runs).
#[cfg(test)]
pub(crate) fn minimized_for(ctx: &egui::Context, cid: u64) -> Option<bool> {
    jobs(ctx)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|j| j.cid == cid)
        .map(|j| j.minimized)
}

/// The title of the window compose `cid` is in (left out of the Screenshot menu).
pub(crate) fn own_window_title(app: &App, cid: u64) -> Option<String> {
    let c = app.s.composes.get(&cid)?;
    Some(if c.inline { MAIN_WINDOW_TITLE.to_owned() } else { c.title() })
}

/// The main window's title (set by the desktop app).
pub const MAIN_WINDOW_TITLE: &str = "MailCraft";

/// The viewport showing compose `cid` (its own window, or the main window for an inline response).
fn viewport_of(app: &App, cid: u64) -> egui::ViewportId {
    if app.s.composes.get(&cid).is_some_and(|c| c.inline) {
        egui::ViewportId::ROOT
    } else {
        super::viewport_id(cid)
    }
}

/// `insert.screenshot` from the ribbon: with a window id or `clip`, starts a capture (true = handled). The
/// saved-picture item (no window, no clip) and calls that already carry a picture fall through.
pub(crate) fn ribbon_screenshot(ctx: &egui::Context, app: &mut App, cid: u64, params: &Value) -> bool {
    if params.get("data_base64").is_some() || params.get("path").is_some() {
        return false;
    }
    let clip = params.get("clip").and_then(Value::as_bool).unwrap_or(false);
    let window_id =
        params.get("windowId").and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()));
    let window = params.get("window").and_then(Value::as_str).map(str::to_owned);
    if !clip && window_id.is_none() {
        // A window entry without an id is the "cannot list windows" entry: say why.
        if let Some(reason) = window.filter(|_| params.get("windowId").is_some()) {
            app.run("insert.screenshot", json!({"compose": cid, "unavailable": reason}));
            return true;
        }
        return false;
    }
    let Some(backend) = capture_backend() else {
        app.run("insert.screenshot", json!({"compose": cid, "clip": clip, "unavailable": NO_BACKEND}));
        return true;
    };
    if busy(ctx, cid) {
        return true;
    }
    let (tx, rx) = channel();
    let minimized = clip && backend.clip_needs_window_hidden();
    if minimized {
        ctx.send_viewport_cmd_to(viewport_of(app, cid), ViewportCommand::Minimized(true));
    }
    let spawned = std::thread::Builder::new().name("mailcraft-capture".into()).spawn(move || {
        if minimized {
            std::thread::sleep(MINIMIZE_WAIT);
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match window_id {
            Some(id) if !clip => backend.capture_window(id),
            _ => backend.capture_screen(),
        }));
        let result = run.unwrap_or_else(|_| Err("The screen capture failed.".to_owned()));
        let _ = tx.send(result);
    });
    if spawned.is_err() {
        if minimized {
            ctx.send_viewport_cmd_to(viewport_of(app, cid), ViewportCommand::Minimized(false));
        }
        app.run(
            "insert.screenshot",
            json!({"compose": cid, "clip": clip, "unavailable": "The screen capture could not start."}),
        );
        return true;
    }
    let job = Job { cid, window, clip, minimized, started: Instant::now(), rx };
    jobs(ctx).lock().unwrap_or_else(PoisonError::into_inner).push(job);
    ctx.request_repaint_after(Duration::from_millis(100));
    true
}

/// Picks up finished captures (call every frame).
pub(crate) fn poll(ctx: &egui::Context, app: &mut App) {
    poll_at(ctx, app, Instant::now());
}

/// [`poll`] at time `now` (captures started more than [`CAPTURE_TIMEOUT`] before are given up; the worker
/// thread's late answer, if any, is dropped).
pub(crate) fn poll_at(ctx: &egui::Context, app: &mut App, now: Instant) {
    let mut done: Vec<(Job, Result<Captured, String>)> = Vec::new();
    {
        let all = jobs(ctx);
        let mut jobs = all.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.is_empty() {
            return;
        }
        let mut i = 0;
        while i < jobs.len() {
            let r = jobs.get(i).map(|j| j.rx.try_recv());
            match r {
                Some(Ok(result)) => {
                    let job = jobs.remove(i);
                    done.push((job, result));
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    let job = jobs.remove(i);
                    done.push((job, Err("The screen capture failed.".to_owned())));
                }
                _ if jobs
                    .get(i)
                    .is_some_and(|j| now.saturating_duration_since(j.started) >= CAPTURE_TIMEOUT) =>
                {
                    let job = jobs.remove(i);
                    done.push((job, Err(CAPTURE_TIMED_OUT.to_owned())));
                }
                _ => i += 1,
            }
        }
        if !jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    for (job, result) in done {
        let vp = viewport_of(app, job.cid);
        if job.minimized {
            ctx.send_viewport_cmd_to(vp, ViewportCommand::Minimized(false));
        }
        // A window capture may have brought the captured window to the front: the compose comes back.
        ctx.send_viewport_cmd_to(vp, ViewportCommand::Focus);
        if !app.s.composes.contains_key(&job.cid) {
            continue;
        }
        finish(app, &job, result);
    }
}

fn finish(app: &mut App, job: &Job, result: Result<Captured, String>) {
    let cid = job.cid;
    let shot = match result {
        Ok(shot) if shot.is_valid() && u64::from(shot.width) * u64::from(shot.height) <= MAX_PIXELS => shot,
        Ok(_) => {
            app.run(
                "insert.screenshot",
                json!({"compose": cid, "clip": job.clip, "unavailable": "The capture came back empty."}),
            );
            return;
        }
        Err(reason) => {
            app.run("insert.screenshot", json!({"compose": cid, "clip": job.clip, "unavailable": reason}));
            return;
        }
    };
    if job.clip && !shot.clipped {
        let token = keep_clip(shot);
        crate::dialogs::open(app, "screenClip", json!({"compose": cid, "clip": token}));
        return;
    }
    insert_capture(app, cid, &shot, job.window.as_deref());
}

/// Encodes `shot` and inserts it into compose `cid` (`window` = what it shows, for the alt text).
pub(crate) fn insert_capture(app: &mut App, cid: u64, shot: &Captured, window: Option<&str>) -> bool {
    let Some(png) = encode_png(shot) else {
        app.run(
            "insert.screenshot",
            json!({"compose": cid, "unavailable": "The capture could not be encoded."}),
        );
        return false;
    };
    let data = base64::engine::general_purpose::STANDARD.encode(png);
    app.run(
        "insert.screenshot",
        json!({"compose": cid, "name": "screenshot.png", "data_base64": data, "window": window}),
    )
    .is_some()
}

fn keep_clip(shot: Captured) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut clips = CLIPS.lock().unwrap_or_else(PoisonError::into_inner);
    clips.push((token, Arc::new(shot)));
    let extra = clips.len().saturating_sub(MAX_CLIPS);
    clips.drain(..extra);
    token
}

/// The screen capture held for Screen Clipping dialog `token`.
pub(crate) fn clip_image(token: u64) -> Option<Arc<Captured>> {
    CLIPS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(t, _)| *t == token)
        .map(|(_, c)| c.clone())
}

/// Drops the capture of a closed Screen Clipping dialog.
pub(crate) fn drop_clip(token: u64) {
    CLIPS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(t, _)| *t != token);
}

/// PNG bytes of a capture.
pub fn encode_png(shot: &Captured) -> Option<Vec<u8>> {
    if !shot.is_valid() {
        return None;
    }
    let img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// The part of `shot` inside the pixel rectangle `x, y, w, h` (clamped to the picture); `None` when nothing
/// of it is inside.
pub fn crop(shot: &Captured, x: u32, y: u32, w: u32, h: u32) -> Option<Captured> {
    if !shot.is_valid() || x >= shot.width || y >= shot.height {
        return None;
    }
    let w = w.min(shot.width - x);
    let h = h.min(shot.height - y);
    if w == 0 || h == 0 {
        return None;
    }
    let stride = usize::try_from(shot.width).ok()?.checked_mul(4)?;
    let (x, y, w, h) = (
        usize::try_from(x).ok()?,
        usize::try_from(y).ok()?,
        usize::try_from(w).ok()?,
        usize::try_from(h).ok()?,
    );
    let mut rgba = Vec::with_capacity(w.checked_mul(h)?.checked_mul(4)?);
    for row in y..y + h {
        let start = row.checked_mul(stride)?.checked_add(x.checked_mul(4)?)?;
        rgba.extend_from_slice(shot.rgba.get(start..start.checked_add(w * 4)?)?);
    }
    Some(Captured { width: u32::try_from(w).ok()?, height: u32::try_from(h).ok()?, rgba, clipped: true })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fake backend: windows (one blank, one titled like a new compose window, MailCraft's main window), a
    /// 4×3 screen. With [`PORTAL`] set it acts like the desktop portal (no need to minimise for clipping).
    pub(crate) struct Fake;

    pub(crate) static PORTAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// A backend whose captures only come back when [`Stuck::release`] is called (a portal that never
    /// answers).
    pub(crate) struct Stuck(pub Mutex<Option<std::sync::mpsc::Receiver<()>>>);

    impl ScreenCapture for Stuck {
        fn windows(&self) -> Result<Vec<CaptureWindow>, String> {
            Ok(vec![CaptureWindow { id: 1, title: "Slow".into() }])
        }
        fn capture_window(&self, _: u64) -> Result<Captured, String> {
            let rx = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(rx) = rx {
                let _ = rx.recv();
            }
            Ok(picture(1, 1))
        }
        fn capture_screen(&self) -> Result<Captured, String> {
            self.capture_window(1)
        }
    }

    pub(crate) fn picture(w: u32, h: u32) -> Captured {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[x as u8 * 10, y as u8 * 10, 0, 255]);
            }
        }
        Captured { width: w, height: h, rgba, clipped: false }
    }

    impl ScreenCapture for Fake {
        fn windows(&self) -> Result<Vec<CaptureWindow>, String> {
            Ok(vec![
                CaptureWindow { id: 7, title: "Notes - Editor".into() },
                CaptureWindow { id: 9, title: " ".into() },
                CaptureWindow { id: 8, title: "Calculator".into() },
                CaptureWindow { id: 10, title: "Untitled - Message (HTML)".into() },
                CaptureWindow { id: 11, title: MAIN_WINDOW_TITLE.into() },
            ])
        }
        fn capture_window(&self, id: u64) -> Result<Captured, String> {
            if id == 7 { Ok(picture(2, 2)) } else { Err("That window has closed.".into()) }
        }
        fn capture_screen(&self) -> Result<Captured, String> {
            Ok(picture(4, 3))
        }
        fn clip_needs_window_hidden(&self) -> bool {
            !PORTAL.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[test]
    fn crop_clamps_and_rejects_outside() {
        let shot = picture(4, 3);
        let c = crop(&shot, 1, 1, 10, 10).unwrap();
        assert_eq!((c.width, c.height), (3, 2));
        assert_eq!(&c.rgba[..4], &[10, 10, 0, 255]);
        assert!(crop(&shot, 4, 0, 1, 1).is_none());
        assert!(crop(&shot, 0, 0, 0, 3).is_none());
        let bad = Captured { width: 4, height: 3, rgba: vec![0; 5], clipped: false };
        assert!(crop(&bad, 0, 0, 1, 1).is_none(), "a short buffer is refused, not indexed");
        assert!(encode_png(&bad).is_none());
    }

    #[test]
    fn png_round_trips() {
        let png = encode_png(&picture(3, 2)).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let back = image::load_from_memory(&png).unwrap();
        assert_eq!((back.width(), back.height()), (3, 2));
    }
}
