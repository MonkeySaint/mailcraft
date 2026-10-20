//! Remote (http/https) pictures in message bodies, downloaded once the reader allows them ("Download
//! pictures", or Trust Center › "Don't download pictures automatically" turned off). Nothing is fetched
//! otherwise: a remote picture can be a tracking pixel.
//!
//! - [`request`] queues a picture; at most [`MAX_CONCURRENT`] background threads download
//!   ([`mailcraft_net::image_fetch::ImageFetcher`]: plain GET, no cookies or referrer, redirects, a deadline, a
//!   [`MAX_IMAGE_BYTES`] cap) and decode (one at a time, with the body's decode limits). The UI thread never
//!   waits: [`pump`], called by the body every frame, turns finished pictures into textures, and each finished
//!   picture requests a repaint.
//! - Downloaded bytes are kept for the session per URL in an LRU cache of [`MAX_CACHE_BYTES`], so going back
//!   to a message does not download again; textures are kept within [`MAX_TEXTURE_PIXELS`] (least recently
//!   drawn dropped first) and decoded again from the cached bytes when needed. Pictures wider or taller than
//!   [`MAX_TEXTURE_SIDE`] are scaled down for display; layout still uses their intrinsic size.
//! - A picture that failed is not retried until [`request`] is called with `retry` (the body does that when a
//!   message is shown again).
//! - The browser build has no sockets or threads: everything here is a no-op there and remote pictures keep
//!   their placeholder.

/// Most pictures downloading at once.
pub const MAX_CONCURRENT: usize = 4;
/// Most remote pictures loaded for one message (the first ones in document order).
pub const MAX_PER_MESSAGE: usize = 200;
/// Largest picture downloaded (bytes).
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Downloaded bytes kept for the session (all pictures together).
pub const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Longest texture side; bigger pictures are scaled down for display.
pub const MAX_TEXTURE_SIDE: usize = 2560;
/// Pixels kept as textures (all remote pictures together).
pub const MAX_TEXTURE_PIXELS: usize = 40_000_000;

/// Where a remote picture stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Queued or downloading.
    Loading,
    /// Downloaded and decoded; holds the intrinsic size in pixels.
    Ready([usize; 2]),
    /// Could not be downloaded or decoded.
    Failed,
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(target_arch = "wasm32")]
mod web {
    use super::Status;

    /// The cache key of a picture source (its trimmed text in the browser build).
    pub fn key(src: &str) -> Option<String> {
        let s = src.trim();
        (!s.is_empty()).then(|| s.to_owned())
    }
    /// No-op: the browser build does not download pictures.
    pub fn pump(_ctx: &egui::Context) {}
    /// No-op: the browser build does not download pictures.
    pub fn request(_ctx: &egui::Context, _url: &str, _retry: bool) {}
    /// Always `None` in the browser build.
    pub fn texture(_ctx: &egui::Context, _url: &str) -> Option<egui::TextureHandle> {
        None
    }
    /// Always `None` in the browser build.
    pub fn status(_ctx: &egui::Context, _url: &str) -> Option<Status> {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex, MutexGuard};

    use mailcraft_net::image_fetch::{ImageFetcher, image_url};

    use super::*;

    /// Most picture states remembered (finished ones are forgotten first).
    const MAX_STATES: usize = 4096;
    /// Most remote textures kept.
    const MAX_TEXTURES: usize = 256;

    /// Decoding runs one picture at a time, so several big pictures never hold decode buffers together.
    static DECODE: Mutex<()> = Mutex::new(());

    /// The cache key of a picture source: the URL that is fetched (`//host/x` → `https://host/x`), or `None`
    /// when `src` is not a usable remote URL.
    pub fn key(src: &str) -> Option<String> {
        image_url(src).map(|u| u.to_string())
    }

    enum Job {
        Fetch(String),
        Decode(String, Arc<[u8]>),
    }

    struct Done {
        url: String,
        /// Display image and intrinsic size; `None` when the picture failed.
        image: Option<(egui::ColorImage, [usize; 2])>,
    }

    struct Tex {
        handle: egui::TextureHandle,
        pixels: usize,
        /// Pass the texture was last drawn in.
        used: u64,
    }

    /// Downloaded bytes by URL, least recently used dropped first beyond `cap` bytes.
    pub(super) struct ByteCache {
        map: HashMap<String, (Arc<[u8]>, u64)>,
        total: usize,
        tick: u64,
        cap: usize,
    }

    impl ByteCache {
        pub(super) fn new(cap: usize) -> Self {
            ByteCache { map: HashMap::new(), total: 0, tick: 0, cap }
        }

        pub(super) fn get(&mut self, url: &str) -> Option<Arc<[u8]>> {
            self.tick = self.tick.wrapping_add(1);
            let tick = self.tick;
            self.map.get_mut(url).map(|(b, t)| {
                *t = tick;
                Arc::clone(b)
            })
        }

        pub(super) fn put(&mut self, url: &str, data: Arc<[u8]>) {
            if let Some((old, _)) = self.map.remove(url) {
                self.total = self.total.saturating_sub(old.len());
            }
            if data.len() > self.cap {
                return;
            }
            self.tick = self.tick.wrapping_add(1);
            self.total = self.total.saturating_add(data.len());
            self.map.insert(url.to_owned(), (data, self.tick));
            while self.total > self.cap {
                let Some(oldest) = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some((b, _)) = self.map.remove(&oldest) {
                    self.total = self.total.saturating_sub(b.len());
                }
            }
        }

        #[cfg(test)]
        pub(super) fn total(&self) -> usize {
            self.total
        }
    }

    struct Shared {
        status: HashMap<String, Status>,
        queue: VecDeque<Job>,
        /// Background threads running.
        workers: usize,
        bytes: ByteCache,
        done: Vec<Done>,
        textures: HashMap<String, Tex>,
        /// Network fetches started (for tests and diagnostics).
        fetches: usize,
        allow_loopback: bool,
    }

    impl Default for Shared {
        fn default() -> Self {
            Shared {
                status: HashMap::new(),
                queue: VecDeque::new(),
                workers: 0,
                bytes: ByteCache::new(MAX_CACHE_BYTES),
                done: Vec::new(),
                textures: HashMap::new(),
                fetches: 0,
                allow_loopback: false,
            }
        }
    }

    #[derive(Clone, Default)]
    struct Loader(Arc<Mutex<Shared>>);

    fn loader(ctx: &egui::Context) -> Arc<Mutex<Shared>> {
        let id = egui::Id::new("mailcraft.remote-images");
        ctx.data_mut(|d| Arc::clone(&d.get_temp_mut_or_default::<Loader>(id).0))
    }

    fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Allows pictures from this machine (tests with a local server).
    #[cfg(test)]
    pub(crate) fn allow_loopback(ctx: &egui::Context) {
        lock(&loader(ctx)).allow_loopback = true;
    }

    /// Network fetches started so far (tests).
    #[cfg(test)]
    pub(crate) fn fetch_count(ctx: &egui::Context) -> usize {
        lock(&loader(ctx)).fetches
    }

    /// Drops every texture (tests: as if they were evicted).
    #[cfg(test)]
    pub(crate) fn drop_textures(ctx: &egui::Context) {
        lock(&loader(ctx)).textures.clear();
    }

    /// Background threads running (tests).
    #[cfg(test)]
    pub(crate) fn worker_count(ctx: &egui::Context) -> usize {
        lock(&loader(ctx)).workers
    }

    /// Queues the picture at `url` (a [`key`]) unless it is loading, drawn, or failed (`retry` tries a failed
    /// one again). Only call this when the reader allows remote pictures.
    pub fn request(ctx: &egui::Context, url: &str, retry: bool) {
        let shared = loader(ctx);
        let mut s = lock(&shared);
        match s.status.get(url) {
            Some(Status::Loading) => return,
            Some(Status::Ready(_)) if s.textures.contains_key(url) => return,
            Some(Status::Failed) if !retry => return,
            _ => {}
        }
        if s.status.len() >= MAX_STATES {
            let Shared { status, textures, .. } = &mut *s;
            status.retain(|k, v| *v == Status::Loading || textures.contains_key(k));
            if status.len() >= MAX_STATES {
                return;
            }
        }
        let job = match s.bytes.get(url) {
            Some(b) => Job::Decode(url.to_owned(), b),
            None => Job::Fetch(url.to_owned()),
        };
        s.status.insert(url.to_owned(), Status::Loading);
        s.queue.push_back(job);
        if s.workers < MAX_CONCURRENT {
            s.workers += 1;
            let allow_loopback = s.allow_loopback;
            drop(s);
            let (shared2, ctx2) = (Arc::clone(&shared), ctx.clone());
            let spawned = std::thread::Builder::new()
                .name("mailcraft-picture".into())
                .spawn(move || worker(&shared2, &ctx2, allow_loopback));
            if let Err(e) = spawned {
                log::warn!("cannot start a picture download thread: {e}");
                let mut s = lock(&shared);
                s.workers = s.workers.saturating_sub(1);
            }
        }
    }

    /// The texture of a downloaded picture, if it is ready. A picture whose texture was dropped is queued
    /// again (decoded from the cached bytes when they are still there).
    pub fn texture(ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
        let pass = ctx.cumulative_pass_nr();
        let shared = loader(ctx);
        let evicted = {
            let mut s = lock(&shared);
            if let Some(t) = s.textures.get_mut(url) {
                t.used = pass;
                return Some(t.handle.clone());
            }
            matches!(s.status.get(url), Some(Status::Ready(_)))
        };
        if evicted {
            request(ctx, url, false);
        }
        None
    }

    /// Where the picture at `url` stands; `None` when it was never requested.
    pub fn status(ctx: &egui::Context, url: &str) -> Option<Status> {
        lock(&loader(ctx)).status.get(url).copied()
    }

    /// Turns finished downloads into textures (UI thread; cheap when nothing finished). Records each
    /// picture's intrinsic size for layout.
    pub fn pump(ctx: &egui::Context) {
        let shared = loader(ctx);
        let done = {
            let mut s = lock(&shared);
            if s.done.is_empty() {
                return;
            }
            std::mem::take(&mut s.done)
        };
        let pass = ctx.cumulative_pass_nr();
        let mut sizes = Vec::new();
        let mut s = lock(&shared);
        for d in done {
            match d.image {
                Some((img, size)) => {
                    let pixels = img.pixels.len();
                    let name = format!("mailcraft-remote-image-{:016x}", crate::body::source_key(&d.url));
                    let handle = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
                    s.textures.insert(d.url.clone(), Tex { handle, pixels, used: pass });
                    s.status.insert(d.url.clone(), Status::Ready(size));
                    sizes.push((d.url, size));
                }
                None => {
                    s.status.insert(d.url, Status::Failed);
                }
            }
        }
        evict(&mut s, pass);
        drop(s);
        for (url, size) in sizes {
            crate::body::record_image_size(ctx, &url, size);
        }
        ctx.request_repaint();
    }

    /// Drops the least recently drawn textures beyond the caps (ones drawn this pass last).
    fn evict(s: &mut Shared, pass: u64) {
        let mut total: usize = s.textures.values().map(|t| t.pixels).sum();
        while total > MAX_TEXTURE_PIXELS || s.textures.len() > MAX_TEXTURES {
            let Some(victim) =
                s.textures.iter().min_by_key(|(_, t)| (t.used >= pass, t.used)).map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(t) = s.textures.remove(&victim) {
                total = total.saturating_sub(t.pixels);
            }
        }
    }

    fn worker(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, allow_loopback: bool) {
        let fetcher = ImageFetcher { allow_loopback, ..ImageFetcher::default() };
        loop {
            let job = {
                let mut s = lock(shared);
                match s.queue.pop_front() {
                    Some(j) => {
                        if matches!(j, Job::Fetch(_)) {
                            s.fetches = s.fetches.saturating_add(1);
                        }
                        j
                    }
                    None => {
                        s.workers = s.workers.saturating_sub(1);
                        return;
                    }
                }
            };
            let url = match &job {
                Job::Fetch(u) | Job::Decode(u, _) => u.clone(),
            };
            // A panic in one job must not stop the loader (the slot would never be freed).
            let image =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(shared, &fetcher, job)))
                    .ok()
                    .flatten();
            lock(shared).done.push(Done { url, image });
            ctx.request_repaint();
        }
    }

    fn run_job(
        shared: &Arc<Mutex<Shared>>,
        fetcher: &ImageFetcher,
        job: Job,
    ) -> Option<(egui::ColorImage, [usize; 2])> {
        let data: Arc<[u8]> = match job {
            Job::Decode(_, b) => b,
            Job::Fetch(url) => match fetcher.fetch(&url) {
                Ok(f) => {
                    let b: Arc<[u8]> = f.data.into();
                    lock(shared).bytes.put(&url, Arc::clone(&b));
                    b
                }
                Err(e) => {
                    log::debug!("picture {url}: {e}");
                    return None;
                }
            },
        };
        let _one_at_a_time = DECODE.lock().unwrap_or_else(|e| e.into_inner());
        let img = crate::body::decode_image(&data)?;
        let size = img.size;
        Some((shrink(img), size))
    }

    /// Scales a picture down so neither side exceeds [`MAX_TEXTURE_SIDE`] (aspect kept).
    pub(super) fn shrink(img: egui::ColorImage) -> egui::ColorImage {
        let [w, h] = img.size;
        let long = w.max(h);
        if long <= MAX_TEXTURE_SIDE || w == 0 || h == 0 {
            return img;
        }
        let scale = MAX_TEXTURE_SIDE as f64 / long as f64;
        let nw = ((w as f64 * scale).round() as u32).clamp(1, MAX_TEXTURE_SIDE as u32);
        let nh = ((h as f64 * scale).round() as u32).clamp(1, MAX_TEXTURE_SIDE as u32);
        let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else { return img };
        let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let Some(buf) = image::RgbaImage::from_raw(w32, h32, raw) else { return img };
        let small = image::imageops::resize(&buf, nw, nh, image::imageops::FilterType::Triangle);
        egui::ColorImage::from_rgba_premultiplied([nw as usize, nh as usize], small.as_raw())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::native::{ByteCache, shrink};
    use super::*;
    use crate::body::{self, BodyOptions, BodyView};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img =
            image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 9) as u8, (y * 9) as u8, 120, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A local picture server: serves `body` for every path (404 for `/missing.png`), counts requests and
    /// records the request lines and headers. Runs until the test process ends.
    fn picture_server(body: Vec<u8>) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (h2, s2) = (Arc::clone(&hits), Arc::clone(&seen));
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                h2.fetch_add(1, Ordering::SeqCst);
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                let missing = head.starts_with("GET /missing.png");
                s2.lock().unwrap().push(head);
                let resp = if missing {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                } else {
                    let mut r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(&body);
                    r
                };
                let _ = s.write_all(&resp);
            }
        });
        (base, hits, seen)
    }

    /// Paints the doc once; returns the image boxes laid out and whether a texture was drawn.
    fn frame(
        ctx: &egui::Context,
        view: &mut BodyView,
        doc: &mailcraft_model::Doc,
        allow: bool,
    ) -> (Vec<egui::Rect>, bool) {
        let opts = BodyOptions { allow_remote: allow, ..BodyOptions::default() };
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            body::show_doc_with(ui, view, doc, &opts);
        });
        out.textures_delta.clear();
        let textured = out.shapes.iter().any(|s| match &s.shape {
            egui::Shape::Mesh(m) => m.texture_id != egui::TextureId::default(),
            _ => false,
        });
        let rects = view
            .layout
            .as_ref()
            .map(|l| {
                l.display
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        mailcraft_layout::DisplayItem::Image { rect, .. } => Some(egui::Rect::from_min_size(
                            egui::pos2(rect.x, rect.y),
                            egui::vec2(rect.w, rect.h),
                        )),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        (rects, textured)
    }

    /// Runs frames until `done` holds (at most a few seconds).
    fn frames_until(
        ctx: &egui::Context,
        view: &mut BodyView,
        doc: &mailcraft_model::Doc,
        done: impl Fn(&[egui::Rect], bool) -> bool,
    ) -> (Vec<egui::Rect>, bool) {
        let start = Instant::now();
        loop {
            let (rects, textured) = frame(ctx, view, doc, true);
            if done(&rects, textured) || start.elapsed() > Duration::from_secs(10) {
                return (rects, textured);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn nothing_is_fetched_until_pictures_are_allowed_then_the_picture_is_drawn() {
        let (base, hits, seen) = picture_server(png(30, 20));
        let url = format!("{base}/logo.png");
        let doc = mailcraft_html::html_to_doc(&format!("<p>Hi <img src=\"{url}\" alt=\"Logo\"></p>"));
        let ctx = egui::Context::default();
        native::allow_loopback(&ctx);
        let mut view = BodyView::default();

        // Blocked: several frames, no request, the placeholder box.
        for _ in 0..5 {
            let (rects, textured) = frame(&ctx, &mut view, &doc, false);
            assert!(!textured);
            assert_eq!(rects.len(), 1);
            assert!(rects[0].width() < 20.0, "placeholder size until downloaded: {rects:?}");
        }
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing fetched while pictures are blocked");
        assert_eq!(status(&ctx, &url), None);
        assert!(texture(&ctx, &url).is_none());

        // Allowed: downloaded in the background, then drawn at its intrinsic size.
        let (rects, textured) =
            frames_until(&ctx, &mut view, &doc, |r, t| t && r.first().is_some_and(|r| r.width() > 20.0));
        assert!(textured, "the downloaded picture is drawn");
        assert_eq!(status(&ctx, &url), Some(Status::Ready([30, 20])));
        assert_eq!((rects[0].width(), rects[0].height()), (30.0, 20.0));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let head = seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(head.starts_with("get /logo.png http/1.1"), "{head}");
        for h in ["cookie:", "referer:", "authorization:"] {
            assert!(!head.contains(h), "{h} sent: {head}");
        }

        // Another view of the same message (switching back) uses the session cache: no new download, even
        // after the texture is dropped.
        native::drop_textures(&ctx);
        let mut again = BodyView::default();
        let (_, textured) = frames_until(&ctx, &mut again, &doc, |_, t| t);
        assert!(textured);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "served from the cache");
        assert_eq!(native::fetch_count(&ctx), 1);
    }

    #[test]
    fn sizes_follow_attributes_and_fit_the_pane() {
        let (base, _hits, _seen) = picture_server(png(1000, 500));
        let wide = format!("{base}/wide.png");
        let doc = mailcraft_html::html_to_doc(&format!(
            "<p><img src=\"{wide}\"></p><p><img src=\"{base}/w.png\" width=\"100\"></p><p><img src=\"{base}/missing.png\" alt=\"Gone\"></p>"
        ));
        let ctx = egui::Context::default();
        native::allow_loopback(&ctx);
        let mut view = BodyView::default();
        let (rects, _) = frames_until(&ctx, &mut view, &doc, |r, _| {
            r.len() == 3 && r[0].width() > 20.0 && r[1].height() < 99.0
        });
        // Intrinsic 1000 × 500, scaled down to the pane width with its aspect kept.
        assert!(rects[0].width() <= view.width + 0.5 && rects[0].width() > 100.0, "{rects:?}");
        assert!((rects[0].width() / rects[0].height() - 2.0).abs() < 0.01, "{rects:?}");
        // Width given: height follows the aspect ratio.
        assert_eq!((rects[1].width(), rects[1].height()), (100.0, 50.0));
        // A failed picture keeps its placeholder.
        let missing = format!("{base}/missing.png");
        let start = Instant::now();
        while status(&ctx, &missing) != Some(Status::Failed) && start.elapsed() < Duration::from_secs(10) {
            frame(&ctx, &mut view, &doc, true);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(status(&ctx, &missing), Some(Status::Failed));
    }

    #[test]
    fn per_message_and_concurrency_caps() {
        // A server that accepts and never answers keeps downloads in flight.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let mut html = String::new();
        for i in 0..(MAX_PER_MESSAGE + 50) {
            html.push_str(&format!("<img src=\"{base}/p{i}.png\">"));
        }
        let doc = mailcraft_html::html_to_doc(&format!("<p>{html}</p>"));
        let ctx = egui::Context::default();
        native::allow_loopback(&ctx);
        let mut view = BodyView::default();
        frame(&ctx, &mut view, &doc, true);
        assert!(native::worker_count(&ctx) <= MAX_CONCURRENT);
        assert_eq!(status(&ctx, &format!("{base}/p0.png")), Some(Status::Loading));
        assert_eq!(status(&ctx, &format!("{base}/p{}.png", MAX_PER_MESSAGE - 1)), Some(Status::Loading));
        assert_eq!(status(&ctx, &format!("{base}/p{}.png", MAX_PER_MESSAGE)), None, "beyond the cap");
        frame(&ctx, &mut view, &doc, true);
        assert_eq!(status(&ctx, &format!("{base}/p{}.png", MAX_PER_MESSAGE + 10)), None);
        drop(l);
    }

    #[test]
    fn keys_and_byte_cache() {
        assert_eq!(key("//example.com/a.png").as_deref(), Some("https://example.com/a.png"));
        assert_eq!(key(" http://example.org/x ").as_deref(), Some("http://example.org/x"));
        assert_eq!(key("cid:x@y"), None);
        let mut c = ByteCache::new(100);
        c.put("a", vec![0u8; 40].into());
        c.put("b", vec![0u8; 40].into());
        assert!(c.get("a").is_some(), "touch a, so b is the oldest");
        c.put("c", vec![0u8; 40].into());
        assert!(c.get("b").is_none(), "least recently used dropped");
        assert!(c.get("a").is_some() && c.get("c").is_some());
        assert_eq!(c.total(), 80);
        c.put("huge", vec![0u8; 101].into());
        assert!(c.get("huge").is_none(), "bigger than the cache: not kept");
        c.put("a", vec![0u8; 10].into());
        assert_eq!(c.total(), 50, "replacing an entry updates the total");
    }

    #[test]
    fn big_pictures_are_scaled_down_for_display() {
        let img = egui::ColorImage::new(
            [MAX_TEXTURE_SIDE * 2, 10],
            vec![egui::Color32::RED; MAX_TEXTURE_SIDE * 20],
        );
        let small = shrink(img);
        assert_eq!(small.size, [MAX_TEXTURE_SIDE, 5]);
        let img = egui::ColorImage::new([4, 3], vec![egui::Color32::RED; 12]);
        assert_eq!(shrink(img).size, [4, 3]);
    }
}
