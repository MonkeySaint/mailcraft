//! Control channel: newline-delimited JSON over a local TCP socket (127.0.0.1, port from
//! MAILCRAFT_CONTROL_PORT) so tests and agents can drive the running app: run commands, query state, press
//! keys, request screenshots, resize the window (keys area). Protocol: `docs/control-protocol.md`.
//!
//! A background thread accepts connections; each connection thread reads request lines and hands them to the
//! UI thread through an mpsc channel. The shell calls [`Control::poll`] once per frame; it answers every
//! queued request against the live [`App`] (all changes go through [`App::run`]) and the connection thread
//! writes the response line back.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use mailcraft_engine::queries;

use crate::{App, Surface};

/// Longest request line accepted (bytes); longer lines get an error response.
pub const MAX_LINE: usize = 1 << 20;
/// How long a connection waits for the UI thread to answer one request.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// One request handed to the UI thread, with the channel its response line goes back on.
struct Pending {
    request: Value,
    reply: Sender<Value>,
}

/// A screenshot asked for and not yet delivered by the integration.
struct ScreenshotJob {
    id: Value,
    path: Option<PathBuf>,
    reply: Sender<Value>,
}

/// Pending control requests drained by the UI each frame.
#[derive(Debug, Default)]
pub struct Control {
    inner: Option<Inner>,
}

struct Inner {
    rx: Receiver<Pending>,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// The egui context, shared with the connection threads so they can wake the UI.
    ctx: Arc<Mutex<Option<egui::Context>>>,
    screenshots: Vec<ScreenshotJob>,
    /// Screenshots the integration has read back (sent from the screenshot callback).
    shots_tx: Sender<Arc<egui::ColorImage>>,
    shots_rx: Receiver<Arc<egui::ColorImage>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("addr", &self.addr)
            .field("screenshots", &self.screenshots.len())
            .finish()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so the listener thread notices and exits.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

impl Control {
    /// Listens on 127.0.0.1:`port` (0 = any free port; see [`Control::port`]). `None` when the socket
    /// cannot be bound (the app runs without a control channel).
    pub fn start(port: u16) -> Option<Control> {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                log::error!("control channel: cannot listen on 127.0.0.1:{port}: {e}");
                return None;
            }
        };
        let addr = listener.local_addr().ok()?;
        let (tx, rx) = mpsc::channel::<Pending>();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
        let (stop2, ctx2) = (Arc::clone(&stop), Arc::clone(&ctx));
        let spawned = std::thread::Builder::new()
            .name("mailcraft-control".into())
            .spawn(move || accept_loop(listener, tx, stop2, ctx2));
        if let Err(e) = spawned {
            log::error!("control channel: cannot start the listener thread: {e}");
            return None;
        }
        log::info!("control channel listening on {addr}");
        let (shots_tx, shots_rx) = mpsc::channel();
        Some(Control {
            inner: Some(Inner { rx, addr, stop, ctx, screenshots: Vec::new(), shots_tx, shots_rx }),
        })
    }

    /// Starts on the port in `MAILCRAFT_CONTROL_PORT`, if set and valid.
    pub fn from_env() -> Option<Control> {
        let port = std::env::var("MAILCRAFT_CONTROL_PORT").ok()?.trim().parse::<u16>().ok()?;
        Control::start(port)
    }

    /// The bound port (0 when not listening).
    pub fn port(&self) -> u16 {
        self.inner.as_ref().map_or(0, |i| i.addr.port())
    }

    /// Answers every queued request. Call once per frame on the UI thread (before drawing, so state changes
    /// show in the same frame).
    pub fn poll(&mut self, ctx: &egui::Context, app: &mut App) {
        let Some(inner) = self.inner.as_mut() else { return };
        if let Ok(mut slot) = inner.ctx.lock()
            && slot.is_none()
        {
            *slot = Some(ctx.clone());
        }
        deliver_screenshots(ctx, inner);
        while let Ok(p) = inner.rx.try_recv() {
            let id = p.request.get("id").cloned().unwrap_or(Value::Null);
            if p.request.get("cmd").and_then(Value::as_str) == Some("screenshot") {
                let path = p.request.get("path").and_then(Value::as_str).map(PathBuf::from);
                let (tx, wake) = (inner.shots_tx.clone(), ctx.clone());
                ctx.request_screenshot(move |image| {
                    let _ = tx.send(image);
                    wake.request_repaint();
                });
                ctx.request_repaint();
                inner.screenshots.push(ScreenshotJob { id, path, reply: p.reply });
                continue;
            }
            let response = match handle_request(ctx, app, &p.request) {
                Ok(result) => json!({"id": id, "ok": true, "result": result}),
                Err(error) => json!({"id": id, "ok": false, "error": error}),
            };
            let _ = p.reply.send(response);
            ctx.request_repaint();
        }
    }
}

/// Shell hook: starts the control channel on `app.ui.control_port` the first time it is called (once; a
/// failed bind is not retried) and answers queued requests. Call once per frame on the main window's context.
pub fn frame(ctx: &egui::Context, app: &mut App) {
    let Some(port) = app.ui.control_port else { return };
    let id = egui::Id::unique("mailcraft.control");
    let slot = ctx.data_mut(|d| d.get_temp::<SharedControl>(id));
    let slot = match slot {
        Some(s) => s,
        None => {
            let s = SharedControl(Arc::new(Mutex::new(Control::start(port))));
            ctx.data_mut(|d| d.insert_temp(id, s.clone()));
            s
        }
    };
    if let Ok(mut guard) = slot.0.lock()
        && let Some(control) = guard.as_mut()
    {
        control.poll(ctx, app);
    }
}

/// The running control channel kept in egui memory by [`frame`] (`None` = could not listen).
#[derive(Clone)]
struct SharedControl(Arc<Mutex<Option<Control>>>);

fn accept_loop(
    listener: TcpListener,
    tx: Sender<Pending>,
    stop: Arc<AtomicBool>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let (tx, ctx, stop) = (tx.clone(), Arc::clone(&ctx), Arc::clone(&stop));
        let spawned = std::thread::Builder::new()
            .name("mailcraft-control-conn".into())
            .spawn(move || serve(stream, tx, ctx, stop));
        if let Err(e) = spawned {
            log::error!("control channel: cannot start a connection thread: {e}");
        }
    }
}

/// Reads request lines from one connection and writes one response line per request.
fn serve(
    stream: TcpStream,
    tx: Sender<Pending>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    stop: Arc<AtomicBool>,
) {
    let Ok(read_half) = stream.try_clone() else { return };
    let mut writer = stream;
    let mut reader = BufReader::new(read_half);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = match read_line_capped(&mut reader, &mut line) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 || stop.load(Ordering::SeqCst) {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let response = if line.len() > MAX_LINE {
            json!({"id": null, "ok": false, "error": format!("request longer than {MAX_LINE} bytes")})
        } else {
            match serde_json::from_str::<Value>(text) {
                Ok(request) if request.is_object() => forward(&tx, &ctx, request),
                Ok(_) => json!({"id": null, "ok": false, "error": "request must be a JSON object"}),
                Err(e) => json!({"id": null, "ok": false, "error": format!("invalid JSON: {e}")}),
            }
        };
        let mut out = response.to_string();
        out.push('\n');
        if writer.write_all(out.as_bytes()).and_then(|()| writer.flush()).is_err() {
            break;
        }
    }
}

/// Reads one line (without the newline), keeping at most `MAX_LINE + 1` bytes; the rest of an over-long line
/// is skipped. Returns the number of bytes consumed (0 = end of stream).
fn read_line_capped(reader: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut consumed = 0usize;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(consumed);
        }
        let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (buf.get(..i).unwrap_or(&[]), Some(i.saturating_add(1))),
            None => (buf, None),
        };
        let room = MAX_LINE.saturating_add(1).saturating_sub(out.len());
        out.extend(chunk.iter().take(room));
        let used = done.unwrap_or(buf.len());
        reader.consume(used);
        consumed = consumed.saturating_add(used);
        if done.is_some() {
            return Ok(consumed);
        }
    }
}

/// Hands a request to the UI thread and waits for its response.
fn forward(tx: &Sender<Pending>, ctx: &Arc<Mutex<Option<egui::Context>>>, request: Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(Pending { request, reply: reply_tx }).is_err() {
        return json!({"id": id, "ok": false, "error": "the app is shutting down"});
    }
    if let Ok(slot) = ctx.lock()
        && let Some(c) = slot.as_ref()
    {
        c.request_repaint();
    }
    match reply_rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(v) => v,
        Err(_) => json!({"id": id, "ok": false, "error": "timed out waiting for the UI"}),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Requests

/// Answers one request (everything but `screenshot`).
pub fn handle_request(ctx: &egui::Context, app: &mut App, req: &Value) -> Result<Value, String> {
    let cmd = req.get("cmd").and_then(Value::as_str).ok_or("`cmd` (string) required")?;
    match cmd {
        "ping" => Ok(json!({"pong": true})),
        "run" => {
            let command = req.get("command").and_then(Value::as_str).ok_or("`command` (string) required")?;
            let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
            let before = app.ui.toast.clone();
            match app.run(command, params) {
                Some(v) => Ok(v),
                None => {
                    let msg =
                        app.ui.toast.as_ref().filter(|t| Some(*t) != before.as_ref()).map(|(m, _)| m.clone());
                    Err(msg.unwrap_or_else(|| format!("`{command}` failed")))
                }
            }
        }
        "query" => {
            let what = req.get("what").and_then(Value::as_str).ok_or("`what` (string) required")?;
            query(app, what, req)
        }
        "key" => {
            let keys = req.get("keys").and_then(Value::as_str).ok_or("`keys` (string) required")?;
            let surface = parse_surface(req.get("surface"))?;
            Ok(crate::keys::press(ctx, app, surface, keys))
        }
        "resize" => {
            let w = req.get("w").and_then(Value::as_f64).ok_or("`w` (number) required")?;
            let h = req.get("h").and_then(Value::as_f64).ok_or("`h` (number) required")?;
            if !(w.is_finite() && h.is_finite()) || w < 1.0 || h < 1.0 || w > 16384.0 || h > 16384.0 {
                return Err("`w` and `h` must be between 1 and 16384".into());
            }
            let size = egui::vec2(w as f32, h as f32);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.request_repaint();
            Ok(json!({"w": w, "h": h}))
        }
        other => Err(format!("unknown cmd `{other}` (ping, run, query, key, resize, screenshot)")),
    }
}

/// `"main"` / absent, `{"compose": id}`, `{"read": id}`.
fn parse_surface(v: Option<&Value>) -> Result<Surface, String> {
    match v {
        None | Some(Value::Null) => Ok(Surface::Main),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("main") => Ok(Surface::Main),
        Some(Value::Object(o)) => {
            if let Some(id) = o.get("compose").and_then(Value::as_u64) {
                Ok(Surface::Compose(id))
            } else if let Some(id) = o.get("read").and_then(Value::as_u64) {
                Ok(Surface::Read(id))
            } else {
                Err("`surface` must be \"main\", {\"compose\": id} or {\"read\": id}".into())
            }
        }
        Some(_) => Err("`surface` must be \"main\", {\"compose\": id} or {\"read\": id}".into()),
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn query(app: &App, what: &str, req: &Value) -> Result<Value, String> {
    let s = &app.s;
    match what {
        "list_rows" => to_value(&queries::list_rows(s)),
        "folder_rows" => to_value(&queries::folder_rows(s)),
        "status" => {
            let (items, unread) = queries::status_counts(s);
            Ok(json!({
                "items": items,
                "unread": unread,
                "module": format!("{:?}", s.view.module),
                "folder": s.selection.folder.0,
                "folderName": s.mailbox.folder(s.selection.folder).map(|f| f.name.clone()),
                "view": to_value(&s.view)?,
                "undo": s.undo.undo.len(),
                "redo": s.undo.redo.len(),
            }))
        }
        "selection" => to_value(&s.selection),
        "composes" => {
            let list: Vec<Value> = s
                .composes
                .values()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "title": c.title(),
                        "kind": to_value(&c.kind).unwrap_or(Value::Null),
                        "inline": c.inline,
                        "to": c.to,
                        "cc": c.cc,
                        "bcc": c.bcc,
                        "subject": c.subject,
                        "dirty": c.dirty,
                        "source": c.source.map(|i| i.0),
                        "draft": c.draft.map(|i| i.0),
                        "attachments": c.attachments.len(),
                    })
                })
                .collect();
            Ok(Value::Array(list))
        }
        "compose" => {
            let id = req.get("compose").and_then(Value::as_u64).ok_or("`compose` (number) required")?;
            let c = s.composes.get(&id).ok_or_else(|| format!("no compose window {id}"))?;
            to_value(c)
        }
        "reading_header" => {
            let item = req
                .get("item")
                .and_then(Value::as_u64)
                .or_else(|| s.selection.focus.map(|i| i.0))
                .ok_or("`item` (number) required (nothing is focused)")?;
            to_value(&queries::reading_header(s, mailcraft_model::ItemId(item)))
        }
        "ui" => Ok(json!({
            "ribbonTab": to_value(&app.ui.ribbon_tab)?,
            "backstage": app.ui.backstage.as_ref().map(|(s, p)| json!({"surface": format!("{s:?}"), "page": p})),
            "dialogs": app.ui.dialogs.iter().map(|(d, p)| json!({"id": d, "params": p})).collect::<Vec<_>>(),
            "readWindows": app.ui.read_windows,
            "searchActive": app.ui.search_active,
            "searchText": app.ui.search_text,
            "singlePaneReading": app.ui.single_pane_reading,
            "toast": app.ui.toast.as_ref().map(|(t, _)| t.clone()),
            "keytips": app.ui.keytips,
            "calendarView": app.ui.calendar_view,
        })),
        "commands" => Ok(Value::Array(
            s.registry
                .specs
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "location": c.location,
                        "shortcut": c.shortcut,
                        "params": c.params,
                        "enabled": (c.enabled)(s).is_none(),
                    })
                })
                .collect(),
        )),
        other => Err(format!(
            "unknown query `{other}` (list_rows, folder_rows, status, selection, composes, compose, reading_header, ui, commands)"
        )),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Screenshots

fn deliver_screenshots(ctx: &egui::Context, inner: &mut Inner) {
    // Drain every time, so a late picture for requests already answered is not handed to the next one.
    let image = inner.shots_rx.try_iter().last();
    if inner.screenshots.is_empty() {
        return;
    }
    let Some(image) = image else {
        ctx.request_repaint();
        return;
    };
    for job in inner.screenshots.drain(..) {
        let [w, h] = image.size;
        let result = match &job.path {
            Some(path) => match write_png(path, &image) {
                Ok(()) => json!({"id": job.id, "ok": true, "result": {"path": path, "w": w, "h": h}}),
                Err(e) => {
                    json!({"id": job.id, "ok": false, "error": format!("cannot write {}: {e}", path.display())})
                }
            },
            None => json!({"id": job.id, "ok": true, "result": {"w": w, "h": h}}),
        };
        let _ = job.reply.send(result);
    }
}

/// Writes an RGBA image as a PNG (stored, uncompressed deflate blocks; no extra dependencies).
fn write_png(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    let mut raw = Vec::with_capacity(h.saturating_mul(w.saturating_mul(4).saturating_add(1)));
    for y in 0..h {
        raw.push(0u8); // filter: none
        for x in 0..w {
            let px = image.pixels.get(y.saturating_mul(w).saturating_add(x)).copied().unwrap_or_default();
            let [r, g, b, a] = px.to_srgba_unmultiplied();
            raw.extend_from_slice(&[r, g, b, a]);
        }
    }
    std::fs::write(path, encode_png(w as u32, h as u32, &raw))
}

/// PNG bytes for 8-bit RGBA scanlines (each prefixed with its filter byte).
pub fn encode_png(w: u32, h: u32, scanlines: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    // zlib stream with stored blocks.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = scanlines.chunks(65_535).collect();
    if blocks.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let last = blocks.len().saturating_sub(1);
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i == last));
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(scanlines).to_be_bytes());
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(out.get(start..).unwrap_or(&[]));
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}
