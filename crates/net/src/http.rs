//! A small blocking HTTP/1.1 client (RFC 9110/9112) for JMAP and OAuth: one request per connection,
//! Content-Length and chunked bodies, redirects, size caps. TLS via the shared transport.

use crate::error::{NetError, Result};
use crate::transport::{Conn, ConnectOptions};

const MAX_HEADER_LINE: usize = 64 * 1024;
const MAX_HEADERS: usize = 200;
/// Most interim (1xx) responses skipped before the final one.
const MAX_INTERIM: usize = 10;

/// An `http` or `https` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    /// Host without brackets for IPv6.
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    /// Parses an absolute `http(s)://host[:port][/path][?query]` URL. User info is rejected.
    pub fn parse(s: &str) -> Result<Url> {
        let s = s.trim();
        let bad = |why: &str| NetError::Invalid(format!("URL `{s}`: {why}"));
        let (https, rest) = if let Some(r) = strip_prefix_ci(s, "https://") {
            (true, r)
        } else if let Some(r) = strip_prefix_ci(s, "http://") {
            (false, r)
        } else {
            return Err(bad("only http and https URLs are supported"));
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = rest.get(..auth_end).unwrap_or_default();
        let mut path = rest.get(auth_end..).unwrap_or_default().to_owned();
        if path.is_empty() || path.starts_with('?') {
            path.insert(0, '/');
        }
        if path.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(bad("spaces or control characters in the path"));
        }
        if authority.contains('@') {
            return Err(bad("credentials in URLs are not supported"));
        }
        let default_port = if https { 443 } else { 80 };
        let (host, port) = if let Some(r) = authority.strip_prefix('[') {
            let close = r.find(']').ok_or_else(|| bad("unterminated IPv6 address"))?;
            let host = r.get(..close).unwrap_or_default();
            let after = r.get(close + 1..).unwrap_or_default();
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad("bad port"))?,
                None if after.is_empty() => default_port,
                None => return Err(bad("junk after IPv6 address")),
            };
            (host.to_owned(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_owned(), p.parse().map_err(|_| bad("bad port"))?),
                None => (authority.to_owned(), default_port),
            }
        };
        if host.is_empty() || host.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(bad("missing or invalid host"));
        }
        Ok(Url { https, host, port, path })
    }

    /// Resolves `reference` (absolute URL, `//host/…`, `/path`, or relative path) against this URL.
    pub fn join(&self, reference: &str) -> Result<Url> {
        let r = reference.trim();
        if strip_prefix_ci(r, "http://").is_some() || strip_prefix_ci(r, "https://").is_some() {
            return Url::parse(r);
        }
        if let Some(rest) = r.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        let path = if r.starts_with('/') {
            r.to_owned()
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = base.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            format!("{dir}/{r}")
        };
        Url::parse(&format!("{}{}", self.origin(), path))
    }

    /// `scheme://host[:port]`.
    pub fn origin(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{}", self.port)
        }
    }

    /// Whether `other` has the same scheme, host and port (RFC 6454 origin).
    pub fn same_origin(&self, other: &Url) -> bool {
        self.https == other.https && self.port == other.port && self.host.eq_ignore_ascii_case(&other.host)
    }

    fn host_header(&self) -> String {
        self.origin().split_once("://").map(|(_, h)| h.to_owned()).unwrap_or_default()
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| s.get(prefix.len()..).unwrap_or_default())
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes (and `+` as space when `plus` is set); invalid escapes are kept.
pub fn percent_decode(s: &str, plus: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        match c {
            b'%' => {
                let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    None => out.push(c),
                }
            }
            b'+' if plus => out.push(b' '),
            _ => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `application/x-www-form-urlencoded` body.
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The URL that produced this response (after redirects).
    pub url: String,
}

impl HttpResponse {
    /// First header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
    /// Parses the body as JSON.
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body)
            .map_err(|e| NetError::Protocol(format!("invalid JSON from {}: {e}", self.url)))
    }
    /// The body as text (lossy), truncated for error messages.
    pub fn text_snippet(&self) -> String {
        let t = String::from_utf8_lossy(&self.body);
        t.chars().take(500).collect()
    }
    /// Errors with [`NetError::Http`] unless the status is 2xx.
    pub fn error_for_status(self) -> Result<HttpResponse> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(NetError::Http { status: self.status, body: self.text_snippet() })
        }
    }
}

/// Blocking HTTP client.
#[derive(Clone, Debug)]
pub struct HttpClient {
    pub options: ConnectOptions,
    /// Largest accepted response body.
    pub max_body: usize,
    pub max_redirects: u8,
    pub user_agent: String,
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient::new(ConnectOptions::default())
    }
}

impl HttpClient {
    pub fn new(options: ConnectOptions) -> Self {
        HttpClient {
            options,
            max_body: 64 * 1024 * 1024,
            max_redirects: 5,
            user_agent: format!("MailCraft/{}", env!("CARGO_PKG_VERSION")),
        }
    }

    /// GET.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.request("GET", url, headers, None)
    }

    /// POST a JSON value.
    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse> {
        let data = serde_json::to_vec(body).map_err(|e| NetError::Invalid(e.to_string()))?;
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        h.push(("Content-Type", "application/json"));
        self.request("POST", url, &h, Some(&data))
    }

    /// POST a form.
    pub fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        pairs: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let data = form_encode(pairs);
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        h.push(("Content-Type", "application/x-www-form-urlencoded"));
        h.push(("Accept", "application/json"));
        self.request("POST", url, &h, Some(data.as_bytes()))
    }

    /// Sends a request, following redirects (303 turns into GET; credentials are dropped when the
    /// redirect leaves the original origin: scheme, host and port).
    pub fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let mut url = Url::parse(url)?;
        let first = url.clone();
        let mut method = method.to_owned();
        let mut body = body.map(<[u8]>::to_vec);
        let mut redirects = 0u8;
        loop {
            let same_origin = url.same_origin(&first);
            let hdrs: Vec<(&str, &str)> = headers
                .iter()
                .copied()
                .filter(|(k, _)| same_origin || !k.eq_ignore_ascii_case("authorization"))
                .collect();
            let resp = self.once(&method, &url, &hdrs, body.as_deref())?;
            let is_redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308);
            let Some(loc) = resp.header("location").filter(|_| is_redirect).map(str::to_owned) else {
                return Ok(resp);
            };
            if redirects >= self.max_redirects {
                return Err(NetError::Protocol(format!("too many redirects at {url}")));
            }
            redirects += 1;
            let next = url.join(&loc)?;
            if url.https && !next.https {
                return Err(NetError::Protocol(format!("refusing a redirect from HTTPS to {next}")));
            }
            if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == "POST") {
                method = "GET".into();
                body = None;
            }
            if body.is_some() && !next.same_origin(&first) {
                // 307/308 replay the body. Token and API request bodies carry secrets (client_secret,
                // refresh_token, code_verifier), so they never follow a redirect to another origin.
                return Err(NetError::Protocol(format!(
                    "refusing to resend the request body to another origin ({})",
                    next.origin()
                )));
            }
            url = next;
        }
    }

    fn once(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        if !method.bytes().all(|b| b.is_ascii_uppercase()) || method.is_empty() {
            return Err(NetError::Invalid(format!("HTTP method `{method}`")));
        }
        if !url.https
            && !self.options.allow_plaintext_auth
            && headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        {
            return Err(NetError::Insecure(format!("Authorization header to {}", url.origin())));
        }
        let mut req = format!("{method} {} HTTP/1.1\r\nHost: {}\r\n", url.path, url.host_header());
        let has = |n: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
        if !has("user-agent") {
            req.push_str(&format!("User-Agent: {}\r\n", self.user_agent));
        }
        if !has("accept") {
            req.push_str("Accept: */*\r\n");
        }
        req.push_str("Connection: close\r\n");
        for (k, v) in headers {
            let bad = |s: &str| s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            if bad(k) || bad(v) || k.is_empty() || k.contains(':') {
                return Err(NetError::Invalid(format!("HTTP header `{k}`")));
            }
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        } else if method == "POST" || method == "PUT" {
            req.push_str("Content-Length: 0\r\n");
        }
        req.push_str("\r\n");
        let mut data = req.into_bytes();
        if let Some(b) = body {
            data.extend_from_slice(b);
        }
        let mut conn = Conn::connect(&url.host, url.port, url.https, &self.options)?;
        conn.write_all(&data)?;
        let resp = self.read_response(&mut conn, method == "HEAD", url);
        conn.shutdown();
        resp
    }

    fn read_response(&self, conn: &mut Conn, head: bool, url: &Url) -> Result<HttpResponse> {
        let mut interim = 0usize;
        loop {
            let line = conn.read_line(MAX_HEADER_LINE)?;
            let line = String::from_utf8_lossy(&line).into_owned();
            let mut parts = line.splitn(3, ' ');
            let version = parts.next().unwrap_or_default();
            if !version.starts_with("HTTP/1.") {
                return Err(NetError::Protocol(format!("not an HTTP/1.x response: `{line}`")));
            }
            let status: u16 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .filter(|s| (100..600).contains(s))
                .ok_or_else(|| NetError::Protocol(format!("bad status line `{line}`")))?;
            let mut headers = Vec::new();
            loop {
                let h = conn.read_line(MAX_HEADER_LINE)?;
                if h.is_empty() {
                    break;
                }
                if headers.len() >= MAX_HEADERS {
                    return Err(NetError::TooLarge("too many HTTP headers".into()));
                }
                let h = String::from_utf8_lossy(&h).into_owned();
                if let Some((k, v)) = h.split_once(':') {
                    headers.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            }
            if (100..200).contains(&status) {
                interim += 1;
                if interim > MAX_INTERIM {
                    return Err(NetError::Protocol("too many interim (1xx) HTTP responses".into()));
                }
                continue;
            }
            let get =
                |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
            let body = if head || status == 204 || status == 304 {
                Vec::new()
            } else if get("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
                self.read_chunked(conn)?
            } else if let Some(len) = get("content-length") {
                let n: usize = len
                    .trim()
                    .parse()
                    .map_err(|_| NetError::Protocol(format!("bad Content-Length `{len}`")))?;
                if n > self.max_body {
                    return Err(NetError::TooLarge(format!("HTTP body of {n} bytes")));
                }
                conn.read_exact(n)?
            } else {
                conn.read_to_end(self.max_body)?
            };
            return Ok(HttpResponse { status, headers, body, url: url.to_string() });
        }
    }

    fn read_chunked(&self, conn: &mut Conn) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let line = conn.read_line(MAX_HEADER_LINE)?;
            let line = String::from_utf8_lossy(&line);
            let hex = line.split(';').next().unwrap_or_default().trim();
            // RFC 9112 section 7.1: chunk-size = 1*HEXDIG (from_str_radix alone would accept a sign).
            let n = Some(hex)
                .filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|h| usize::from_str_radix(h, 16).ok())
                .ok_or_else(|| NetError::Protocol(format!("bad chunk size `{hex}`")))?;
            if n == 0 {
                // Trailers.
                for _ in 0..MAX_HEADERS {
                    if conn.read_line(MAX_HEADER_LINE)?.is_empty() {
                        return Ok(body);
                    }
                }
                return Err(NetError::TooLarge("too many trailers".into()));
            }
            if body.len().saturating_add(n) > self.max_body {
                return Err(NetError::TooLarge("chunked HTTP body".into()));
            }
            body.extend_from_slice(&conn.read_exact(n)?);
            let crlf = conn.read_line(2)?;
            if !crlf.is_empty() {
                return Err(NetError::Protocol("missing CRLF after chunk".into()));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testserver {
    //! A scripted one-shot HTTP server on 127.0.0.1 for tests.
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    /// A request as the server saw it.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Seen {
        pub fn header(&self, n: &str) -> Option<&str> {
            self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str())
        }
    }

    /// Serves one raw response per request with `respond(request) -> raw response bytes`, for `count`
    /// requests. Returns the base URL, the thread, and a channel of seen requests.
    pub fn serve<F>(count: usize, respond: F) -> (String, JoinHandle<()>, mpsc::Receiver<Seen>)
    where
        F: Fn(&Seen) -> Vec<u8> + Send + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || {
            for _ in 0..count {
                let (s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut it = line.split_whitespace();
                let method = it.next().unwrap_or_default().to_owned();
                let path = it.next().unwrap_or_default().to_owned();
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    headers.push((k.trim().to_owned(), v.trim().to_owned()));
                }
                let len: usize = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.parse().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let seen = Seen { method, path, headers, body };
                let resp = respond(&seen);
                let _ = tx.send(seen);
                let mut s = s;
                s.write_all(&resp).unwrap();
            }
        });
        (format!("http://127.0.0.1:{port}"), t, rx)
    }

    pub fn json(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::*;
    use super::*;

    #[test]
    fn url_parsing() {
        let u = Url::parse("https://example.com").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (true, "example.com", 443, "/"));
        let u = Url::parse("HTTP://127.0.0.1:8080/a/b?x=1#frag").unwrap();
        assert_eq!((u.https, u.port, u.path.as_str()), (false, 8080, "/a/b?x=1"));
        let u = Url::parse("http://[::1]:99?q").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("::1", 99, "/?q"));
        assert_eq!(u.to_string(), "http://[::1]:99/?q");
        assert!(Url::parse("ftp://x").is_err());
        assert!(Url::parse("http://user:pw@x/").is_err());
        assert!(Url::parse("http://x:bad/").is_err());
        assert!(Url::parse("http:///path").is_err());
        assert!(Url::parse("http://x/a b").is_err());
        let base = Url::parse("https://example.com/a/b/c?z").unwrap();
        assert_eq!(base.join("/root").unwrap().to_string(), "https://example.com/root");
        assert_eq!(base.join("rel?q").unwrap().to_string(), "https://example.com/a/b/rel?q");
        assert_eq!(base.join("//other.org/x").unwrap().to_string(), "https://other.org/x");
        assert_eq!(base.join("http://h:81/").unwrap().to_string(), "http://h:81/");
    }

    #[test]
    fn encoding() {
        assert_eq!(percent_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(percent_decode("a%20b+c%zz%", true), "a b c%zz%");
        assert_eq!(
            form_encode(&[("grant_type", "refresh_token"), ("x", "1 2")]),
            "grant_type=refresh_token&x=1%202"
        );
    }

    #[test]
    fn content_length_and_chunked() {
        let (base, t, rx) = serve(2, |s| {
            if s.path == "/chunked" {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nX-T: 1\r\n\r\n".to_vec()
            } else {
                json(201, "{\"ok\":true}")
            }
        });
        let c = plaintext_ok();
        let r = c
            .post_json(&format!("{base}/api"), &[("Authorization", "Bearer t")], &serde_json::json!({"a": 1}))
            .unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.json().unwrap()["ok"], true);
        let seen = rx.recv().unwrap();
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.header("authorization"), Some("Bearer t"));
        assert_eq!(seen.body, br#"{"a":1}"#);
        let r = c.get(&format!("{base}/chunked"), &[]).unwrap();
        assert_eq!(r.body, b"hello world");
        t.join().unwrap();
    }

    #[test]
    fn redirects_and_errors() {
        let (base, t, rx) = serve(3, |s| match s.path.as_str() {
            "/start" => b"HTTP/1.1 303 See Other\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n".to_vec(),
            "/next" => b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope".to_vec(),
            _ => b"HTTP/1.1 200 OK\r\n\r\nuntil-close".to_vec(),
        });
        let c = HttpClient::default();
        let r = c.request("POST", &format!("{base}/start"), &[], Some(b"x")).unwrap();
        assert_eq!(r.status, 404);
        assert!(r.url.ends_with("/next"));
        assert_eq!(r.clone().error_for_status(), Err(NetError::Http { status: 404, body: "nope".into() }));
        let _ = rx.recv().unwrap();
        assert_eq!(rx.recv().unwrap().method, "GET");
        let r = c.get(&format!("{base}/eof"), &[]).unwrap();
        assert_eq!(r.body, b"until-close");
        t.join().unwrap();
        assert!(c.get(&format!("{base}/x"), &[("Bad\r\nHeader", "v")]).is_err());
    }

    fn plaintext_ok() -> HttpClient {
        HttpClient::new(ConnectOptions { allow_plaintext_auth: true, ..ConnectOptions::default() })
    }

    #[test]
    fn authorization_needs_tls_unless_allowed() {
        // Port 1 is never contacted: the request is refused before connecting.
        let e =
            HttpClient::default().get("http://127.0.0.1:1/", &[("Authorization", "Bearer t")]).unwrap_err();
        assert!(matches!(e, NetError::Insecure(_)), "{e:?}");
        let e = plaintext_ok().get("http://127.0.0.1:1/", &[("Authorization", "Bearer t")]).unwrap_err();
        assert!(!matches!(e, NetError::Insecure(_)), "{e:?}");
    }

    #[test]
    fn authorization_dropped_when_a_redirect_changes_origin() {
        let (other, t2, rx2) = serve(1, |_| json(200, "{}"));
        let target = format!("{other}/landing");
        let (base, t1, _rx1) = serve(1, move |_| {
            format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n").into_bytes()
        });
        let r = plaintext_ok().get(&format!("{base}/start"), &[("Authorization", "Bearer secret")]).unwrap();
        assert_eq!(r.status, 200);
        let seen = rx2.recv().unwrap();
        assert_eq!(seen.path, "/landing");
        assert_eq!(seen.header("authorization"), None, "same host, different port: another origin");
        t1.join().unwrap();
        t2.join().unwrap();
    }

    #[test]
    fn request_body_is_not_resent_to_another_origin() {
        // A 307/308 keeps the method and body; a token request's body holds secrets (client_secret,
        // refresh_token, code_verifier), so it must not follow the redirect to another origin.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let target = format!("http://127.0.0.1:{dead}/steal");
        let (base, t, _rx) = serve(1, move |_| {
            format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n")
                .into_bytes()
        });
        let e = HttpClient::default()
            .post_form(&format!("{base}/token"), &[], &[("client_secret", "s3cret")])
            .unwrap_err();
        assert!(matches!(e, NetError::Protocol(_)), "{e:?}");
        t.join().unwrap();
        // Same origin: the body follows.
        let (base, t, rx) = serve(2, |s| {
            if s.path == "/a" {
                b"HTTP/1.1 308 Permanent Redirect\r\nLocation: /b\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                json(200, "{}")
            }
        });
        let r = HttpClient::default().request("POST", &format!("{base}/a"), &[], Some(b"payload")).unwrap();
        assert_eq!(r.status, 200);
        let _ = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert_eq!((second.method.as_str(), second.body.as_slice()), ("POST", &b"payload"[..]));
        t.join().unwrap();
    }

    #[test]
    fn endless_interim_responses_and_bad_chunks_are_errors() {
        let (base, t, _rx) = serve(2, |s| {
            if s.path == "/interim" {
                let mut r = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(100);
                r.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                r
            } else {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+5\r\nhello\r\n0\r\n\r\n".to_vec()
            }
        });
        let c = HttpClient::default();
        assert!(c.get(&format!("{base}/interim"), &[]).is_err());
        assert!(matches!(c.get(&format!("{base}/chunk"), &[]), Err(NetError::Protocol(_))));
        t.join().unwrap();
    }

    #[test]
    fn body_cap() {
        let (base, t, _rx) = serve(1, |_| b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n".to_vec());
        let c = HttpClient { max_body: 1000, ..HttpClient::default() };
        assert!(matches!(c.get(&base, &[]), Err(NetError::TooLarge(_))));
        t.join().unwrap();
    }
}
