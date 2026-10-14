//! Consent-gated desktop image downloads. No cookies, credentials or Referer; bounded memory and workers.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Context, Id, TextureHandle};
use mailcraft_net::{ConnectOptions, http::HttpClient};

use crate::body::{MAX_IMAGE_BYTES, MAX_IMAGE_CACHE, decode_image};

const MAX_DOWNLOADS: usize = 4;
const MAX_URL_BYTES: usize = 8192;
const MAX_TEXTURE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

enum Picture {
    Loading,
    Ready(Option<TextureHandle>),
}

struct Entry {
    src: String,
    picture: Picture,
    bytes: usize,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
}

impl State {
    fn reserve(&mut self, src: &str) -> bool {
        if self.entries.iter().filter(|e| matches!(e.picture, Picture::Loading)).count() >= MAX_DOWNLOADS {
            return false;
        }
        if self.entries.len() >= MAX_IMAGE_CACHE {
            let Some(oldest) = self.entries.iter().position(|e| !matches!(e.picture, Picture::Loading))
            else {
                return false;
            };
            self.entries.remove(oldest);
        }
        self.entries.push(Entry { src: src.into(), picture: Picture::Loading, bytes: 0 });
        true
    }

    fn finish(&mut self, src: &str, texture: Option<TextureHandle>) {
        let bytes = texture.as_ref().map_or(0, |t| t.byte_size());
        while self.entries.iter().map(|e| e.bytes).sum::<usize>().saturating_add(bytes) > MAX_CACHE_BYTES {
            let Some(oldest) = self.entries.iter().position(|e| !matches!(e.picture, Picture::Loading))
            else {
                break;
            };
            self.entries.remove(oldest);
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.src == src) {
            entry.picture = Picture::Ready(texture);
            entry.bytes = bytes;
        }
    }
}

#[derive(Clone, Default)]
struct Downloads(Arc<Mutex<State>>);

fn downloads(ctx: &Context) -> Downloads {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Downloads>(Id::new("mailcraft-remote-images")).clone())
}

/// Cached dimensions only: measuring the document must never trigger a network request.
pub(crate) fn cached_sizes(ctx: &Context) -> std::collections::BTreeMap<String, [usize; 2]> {
    let cache = downloads(ctx);
    let Ok(state) = cache.0.lock() else { return Default::default() };
    state
        .entries
        .iter()
        .filter_map(|entry| match &entry.picture {
            Picture::Ready(Some(texture)) => Some((entry.src.clone(), texture.size())),
            _ => None,
        })
        .collect()
}

/// Returns immediately. Disallowed sources are never requested or revealed from the shared cache.
pub(crate) fn image_texture(ctx: &Context, src: &str, allowed: bool) -> Option<TextureHandle> {
    let src = src.trim();
    if !allowed || src.len() > MAX_URL_BYTES {
        return None;
    }
    let cache = downloads(ctx);
    {
        let mut state = cache.0.lock().ok()?;
        if let Some(index) = state.entries.iter().position(|e| e.src == src) {
            let entry = state.entries.remove(index);
            let texture = match &entry.picture {
                Picture::Ready(t) => t.clone(),
                Picture::Loading => None,
            };
            state.entries.push(entry);
            return texture;
        }
        if !state.reserve(src) {
            return None;
        }
    }
    let worker_cache = cache.clone();
    let worker_ctx = ctx.clone();
    let worker_src = src.to_owned();
    let spawned = std::thread::Builder::new().name("mailcraft-image".into()).spawn(move || {
        let texture = fetch_picture(&worker_src).map(|image| {
            worker_ctx.load_texture("mailcraft-remote-image", image, egui::TextureOptions::LINEAR)
        });
        if let Ok(mut state) = worker_cache.0.lock() {
            state.finish(&worker_src, texture);
        }
        worker_ctx.request_repaint();
    });
    if spawned.is_err() {
        if let Ok(mut state) = cache.0.lock() {
            state.finish(src, None);
        }
        ctx.request_repaint();
    }
    None
}

fn fetch_picture(src: &str) -> Option<egui::ColorImage> {
    // HTML commonly uses scheme-relative image sources. Always choose HTTPS for these.
    let absolute;
    let src = if src.starts_with("//") {
        absolute = format!("https:{src}");
        absolute.as_str()
    } else {
        src
    };
    let mut client = HttpClient::new(ConnectOptions {
        connect_timeout: Duration::from_secs(10),
        io_timeout: Duration::from_secs(15),
        ..ConnectOptions::default()
    });
    client.max_body = MAX_IMAGE_BYTES;
    client.max_redirects = 3;
    let accept = if image::ImageFormat::WebP.reading_enabled() {
        "image/png,image/jpeg,image/gif,image/webp"
    } else {
        "image/png,image/jpeg,image/gif"
    };
    let response = client
        .get(src, &[("Accept", accept), ("Accept-Encoding", "identity")])
        .ok()?
        .error_for_status()
        .ok()?;
    let image = decode_image(&response.body)?;
    let bytes = image.size[0].checked_mul(image.size[1])?.checked_mul(4)?;
    (bytes <= MAX_TEXTURE_BYTES).then_some(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve(status: &str, body: Vec<u8>, extra: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/picture.png", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}\r\n", body.len());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.extend_from_slice(&byte);
            }
            socket.write_all(response.as_bytes()).unwrap();
            let _ = socket.write_all(&body);
            String::from_utf8(request).unwrap()
        });
        (url, server)
    }

    fn png() -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(6, 4, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn downloads_real_image_without_account_headers() {
        let (url, server) = serve("200 OK", png(), "Content-Type: image/png\r\n");
        assert_eq!(fetch_picture(&url).unwrap().size, [6, 4]);
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /picture.png http/1.1\r\n"));
        for header in ["cookie:", "authorization:", "referer:"] {
            assert!(!request.contains(header));
        }
        assert!(request.contains("accept-encoding: identity"));
    }

    #[test]
    fn async_download_repaints_and_reuses_its_texture() {
        let (url, server) = serve("200 OK", png(), "");
        let ctx = Context::default();
        let (tx, rx) = std::sync::mpsc::channel();
        ctx.set_request_repaint_callback(move |_| {
            let _ = tx.send(());
        });
        assert!(image_texture(&ctx, &url, true).is_none());
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let texture = image_texture(&ctx, &url, true).unwrap();
        assert_eq!(texture.size(), [6, 4]);
        assert_eq!(image_texture(&ctx, &url, true).unwrap().id(), texture.id());
        server.join().unwrap();
    }

    #[test]
    fn redirects_reach_images_but_oversized_responses_are_rejected() {
        let (target, target_server) = serve("200 OK", png(), "");
        let (url, redirect_server) = serve("302 Found", Vec::new(), &format!("Location: {target}\r\n"));
        assert_eq!(fetch_picture(&url).unwrap().size, [6, 4]);
        redirect_server.join().unwrap();
        target_server.join().unwrap();
        let (url, server) = serve("200 OK\r\nContent-Length: 999999999", Vec::new(), "");
        assert!(fetch_picture(&url).is_none());
        server.join().unwrap();
    }

    #[test]
    fn failures_and_non_images_stay_placeholders() {
        for (status, bytes) in [("404 Not Found", png()), ("200 OK", b"<html>oops</html>".to_vec())] {
            let (url, server) = serve(status, bytes, "");
            assert!(fetch_picture(&url).is_none());
            server.join().unwrap();
        }
        assert!(fetch_picture("file:///etc/passwd").is_none());
        assert!(fetch_picture("http://user:password@example.com/a.png").is_none());
    }

    #[test]
    fn blocked_pictures_do_not_start_downloads_or_reveal_cached_textures() {
        let ctx = Context::default();
        let url = "http://127.0.0.1:1/image.png";
        assert!(image_texture(&ctx, url, false).is_none());
        let cache = downloads(&ctx);
        assert!(cache.0.lock().unwrap().entries.is_empty());
        let texture = ctx.load_texture("test", decode_image(&png()).unwrap(), Default::default());
        {
            let mut state = cache.0.lock().unwrap();
            assert!(state.reserve(url));
            state.finish(url, Some(texture));
        }
        assert!(image_texture(&ctx, url, false).is_none());
        assert_eq!(image_texture(&ctx, url, true).unwrap().size(), [6, 4]);
    }

    #[test]
    fn concurrent_workers_and_cached_entries_are_bounded() {
        let mut state = State::default();
        for i in 0..MAX_DOWNLOADS {
            assert!(state.reserve(&format!("pending-{i}")));
        }
        assert!(!state.reserve("over-limit"));
        state.finish("pending-0", None);
        assert!(state.reserve("next"));
        state.finish("next", None);
        for i in 0..MAX_IMAGE_CACHE * 2 {
            let src = format!("failed-{i}");
            assert!(state.reserve(&src));
            state.finish(&src, None);
            assert!(state.entries.len() <= MAX_IMAGE_CACHE);
        }
        assert!(state.entries.iter().any(|e| e.src == "pending-1"));
    }
}
