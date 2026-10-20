//! Downloads the remote pictures an HTML message refers to (`<img src="https://…">`), for the reading pane's
//! "Download pictures".
//!
//! Every fetch is one plain GET: no cookies, no `Referer`, no credentials (user info in a URL is refused),
//! a generic `User-Agent`. Redirects are followed by hand up to [`MAX_REDIRECTS`] (never from HTTPS down to
//! HTTP), the whole fetch must finish within [`ImageFetcher::timeout`], and bodies over
//! [`ImageFetcher::max_bytes`] are refused. The answer must be a picture: an `image/*` (or untyped binary)
//! content type, or bytes that start like a known picture format. Loopback hosts (`localhost`, `127.0.0.0/8`,
//! `::1`) are refused unless [`ImageFetcher::allow_loopback`] is set, so a message cannot poke services on the
//! reader's own machine.
//!
//! Protocol-relative sources (`//host/x.png`) are fetched over HTTPS; `http://` is allowed.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::error::{NetError, Result};
use crate::http::{HttpClient, Url};
use crate::transport::ConnectOptions;

/// Largest picture downloaded (bytes).
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Most redirects followed for one picture.
pub const MAX_REDIRECTS: u8 = 5;
/// Longest one picture may take, redirects included.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest a picture's `src` may be (characters).
pub const MAX_URL_LEN: usize = 8 * 1024;

/// The absolute URL to fetch for a picture's `src`, or `None` when it is not a remote picture: `http(s)://…`
/// as is, `//host/…` over HTTPS. Surrounding whitespace is dropped and inner spaces are percent-encoded;
/// fragments, user info, control characters and over-long sources are refused.
pub fn image_url(src: &str) -> Option<Url> {
    let s = src.trim();
    if s.is_empty() || s.len() > MAX_URL_LEN || s.chars().any(char::is_control) {
        return None;
    }
    let s = s.replace(' ', "%20");
    let lower: String = s.chars().take(8).collect::<String>().to_ascii_lowercase();
    let abs = if lower.starts_with("http://") || lower.starts_with("https://") {
        s
    } else if s.starts_with("//") {
        format!("https:{s}")
    } else {
        return None;
    };
    Url::parse(&abs).ok()
}

/// Whether `host` names this machine: `localhost` (and `*.localhost`), a loopback address, or the
/// unspecified address.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    let lower = h.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_loopback() || a.is_unspecified(),
        Ok(IpAddr::V6(a)) => {
            a.is_loopback() || a.is_unspecified() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

/// The picture format `data` starts like, as a media type.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    let starts = |p: &[u8]| data.starts_with(p);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if starts(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else if starts(b"BM") && data.len() > 14 {
        Some("image/bmp")
    } else {
        None
    }
}

/// Whether a `Content-Type` may carry a picture (`image/*`, untyped binary, or none at all).
fn picture_type(ct: Option<&str>) -> bool {
    let Some(ct) = ct else { return true };
    let mime = ct.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    mime.is_empty()
        || mime.starts_with("image/")
        || mime == "application/octet-stream"
        || mime == "binary/octet-stream"
}

/// A downloaded picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedImage {
    /// The URL that answered (after redirects).
    pub url: String,
    /// The answer's `Content-Type`, if any.
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// Downloads remote pictures (blocking; run it off the UI thread).
#[derive(Clone, Debug)]
pub struct ImageFetcher {
    pub client: HttpClient,
    /// Largest picture accepted (bytes).
    pub max_bytes: usize,
    /// Longest one fetch may take, redirects included.
    pub timeout: Duration,
    pub max_redirects: u8,
    /// Allow pictures from this machine (tests with a local server).
    pub allow_loopback: bool,
}

impl Default for ImageFetcher {
    fn default() -> Self {
        let options = ConnectOptions {
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(15),
            ..ConnectOptions::default()
        };
        let mut client = HttpClient::new(options);
        client.max_body = MAX_IMAGE_BYTES;
        client.max_redirects = 0;
        client.user_agent = "Mozilla/5.0 (compatible; MailCraft)".into();
        ImageFetcher {
            client,
            max_bytes: MAX_IMAGE_BYTES,
            timeout: FETCH_TIMEOUT,
            max_redirects: MAX_REDIRECTS,
            allow_loopback: false,
        }
    }
}

impl ImageFetcher {
    /// Downloads the picture a body refers to as `src` (see [`image_url`]).
    pub fn fetch(&self, src: &str) -> Result<FetchedImage> {
        let mut url = image_url(src).ok_or_else(|| NetError::Invalid("not a remote picture URL".into()))?;
        let deadline = Instant::now().checked_add(self.timeout).unwrap_or_else(Instant::now);
        let client = HttpClient { max_body: self.max_bytes.min(self.client.max_body), ..self.client.clone() };
        let headers = [("Accept", "image/png,image/jpeg,image/gif,image/*;q=0.8,*/*;q=0.5")];
        let mut redirects = 0u8;
        loop {
            if !self.allow_loopback && is_loopback_host(&url.host) {
                return Err(NetError::Invalid(format!(
                    "refusing a picture from this machine ({})",
                    url.origin()
                )));
            }
            let resp = client.get_once(&url, &headers, deadline)?;
            if matches!(resp.status, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = resp.header("location")
            {
                if redirects >= self.max_redirects {
                    return Err(NetError::Protocol(format!("too many redirects at {url}")));
                }
                redirects += 1;
                let next = url.join(loc)?;
                if url.https && !next.https {
                    return Err(NetError::Protocol(format!("refusing a redirect from HTTPS to {next}")));
                }
                url = next;
                continue;
            }
            let resp = resp.error_for_status()?;
            if resp.body.is_empty() {
                return Err(NetError::Protocol(format!("empty picture from {url}")));
            }
            if resp.body.len() > self.max_bytes {
                return Err(NetError::TooLarge(format!("picture of {} bytes", resp.body.len())));
            }
            let content_type = resp.header("content-type").map(str::to_owned);
            if !picture_type(content_type.as_deref()) && sniff_image(&resp.body).is_none() {
                return Err(NetError::Protocol(format!(
                    "{url} is not a picture ({})",
                    content_type.as_deref().unwrap_or_default()
                )));
            }
            return Ok(FetchedImage { url: resp.url, content_type, data: resp.body });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testserver::serve;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRfake";

    fn local() -> ImageFetcher {
        ImageFetcher { allow_loopback: true, ..ImageFetcher::default() }
    }

    fn png_response(ct: &str, body: &[u8]) -> Vec<u8> {
        let mut r =
            format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n", body.len())
                .into_bytes();
        r.extend_from_slice(body);
        r
    }

    #[test]
    fn urls() {
        assert_eq!(
            image_url(" https://cdn.example.com/a.png ").unwrap().to_string(),
            "https://cdn.example.com/a.png"
        );
        assert_eq!(image_url("HTTP://example.org/x?y=1#f").unwrap().to_string(), "http://example.org/x?y=1");
        assert_eq!(image_url("//example.net/p.gif").unwrap().to_string(), "https://example.net/p.gif");
        assert_eq!(image_url("https://example.com/a b.png").unwrap().path, "/a%20b.png");
        for bad in [
            "",
            "cid:logo@x",
            "data:image/png;base64,AAAA",
            "/relative.png",
            "relative.png",
            "javascript:alert(1)",
            "ftp://example.com/x.png",
            "https://user:pw@example.com/x.png",
            "https://example.com/a\nb.png",
            "https:///x.png",
        ] {
            assert!(image_url(bad).is_none(), "{bad:?}");
        }
        assert!(image_url(&format!("https://example.com/{}", "a".repeat(MAX_URL_LEN))).is_none());
    }

    #[test]
    fn loopback_hosts() {
        for h in
            ["localhost", "LOCALHOST.", "a.localhost", "127.0.0.1", "127.9.9.9", "::1", "[::1]", "0.0.0.0"]
        {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in ["example.com", "10.0.0.1", "192.0.2.1", "localhost.example.com", "2001:db8::1"] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_image(PNG), Some("image/png"));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<html>"), None);
        assert_eq!(sniff_image(b""), None);
        assert!(picture_type(Some("image/png; charset=binary")));
        assert!(picture_type(None));
        assert!(!picture_type(Some("text/html")));
    }

    #[test]
    fn fetches_a_picture_without_cookies_or_referrer() {
        let (base, t, rx) = serve(1, |_| png_response("image/png", PNG));
        let got = local().fetch(&format!("{base}/logo.png")).unwrap();
        assert_eq!(got.data, PNG);
        assert_eq!(got.content_type.as_deref(), Some("image/png"));
        let seen = rx.recv().unwrap();
        assert_eq!((seen.method.as_str(), seen.path.as_str()), ("GET", "/logo.png"));
        for h in ["cookie", "referer", "authorization", "origin"] {
            assert_eq!(seen.header(h), None, "{h}");
        }
        t.join().unwrap();
    }

    #[test]
    fn loopback_is_refused_by_default() {
        // Port 1 is never contacted: the host is refused before connecting.
        let e = ImageFetcher::default().fetch("http://127.0.0.1:1/x.png").unwrap_err();
        assert!(matches!(e, NetError::Invalid(_)), "{e:?}");
        assert!(ImageFetcher::default().fetch("http://localhost:1/x.png").is_err());
    }

    #[test]
    fn redirects_are_followed_up_to_the_limit() {
        let (base, t, _rx) = serve(2, |s| {
            if s.path == "/a" {
                b"HTTP/1.1 302 Found\r\nLocation: /b.png\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                png_response("application/octet-stream", PNG)
            }
        });
        let got = local().fetch(&format!("{base}/a")).unwrap();
        assert!(got.url.ends_with("/b.png"), "{}", got.url);
        t.join().unwrap();
        // A redirect loop stops.
        let (base, t, _rx) = serve(usize::from(MAX_REDIRECTS) + 1, |_| {
            b"HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n".to_vec()
        });
        assert!(matches!(local().fetch(&format!("{base}/again")), Err(NetError::Protocol(_))));
        t.join().unwrap();
    }

    #[test]
    fn non_pictures_errors_and_oversized_bodies_are_refused() {
        let (base, t, _rx) = serve(4, |s| match s.path.as_str() {
            "/html" => png_response("text/html", b"<html>tracking</html>"),
            "/sniffed" => png_response("text/plain", PNG),
            "/missing" => b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope".to_vec(),
            _ => b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 999999999\r\n\r\n".to_vec(),
        });
        let f = local();
        assert!(matches!(f.fetch(&format!("{base}/html")), Err(NetError::Protocol(_))));
        assert_eq!(f.fetch(&format!("{base}/sniffed")).unwrap().data, PNG);
        assert!(matches!(f.fetch(&format!("{base}/missing")), Err(NetError::Http { status: 404, .. })));
        assert!(matches!(f.fetch(&format!("{base}/huge")), Err(NetError::TooLarge(_))));
        t.join().unwrap();
    }

    #[test]
    fn a_slow_server_hits_the_deadline() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            use std::io::Write;
            // Headers, then a body that never arrives.
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\n\r\n");
            std::thread::sleep(Duration::from_millis(1500));
        });
        let f = ImageFetcher { timeout: Duration::from_millis(300), ..local() };
        let started = Instant::now();
        let e = f.fetch(&format!("http://127.0.0.1:{port}/slow.png")).unwrap_err();
        assert!(matches!(e, NetError::Timeout(_)), "{e:?}");
        assert!(started.elapsed() < Duration::from_millis(1400));
        t.join().unwrap();
    }

    #[test]
    fn body_until_close_is_read_whole() {
        let mut body = PNG.to_vec();
        body.extend((0..200_000u32).map(|i| (i % 251) as u8));
        let expect = body.clone();
        let (base, t, _rx) = serve(1, move |_| {
            let mut r = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\r\n".to_vec();
            r.extend_from_slice(&body);
            r
        });
        assert_eq!(local().fetch(&format!("{base}/eof.png")).unwrap().data, expect);
        t.join().unwrap();
    }
}
