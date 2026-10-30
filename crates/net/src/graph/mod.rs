//! Microsoft Graph REST API v1.0 client (the public API documented at learn.microsoft.com, "Microsoft Graph
//! REST API v1.0 reference") over the crate's blocking HTTP/1.1 client.
//!
//! [`GraphClient`] is the transport every Graph feature shares: Bearer authentication with a delegated OAuth
//! access token, JSON requests and answers, collections paged through `@odata.nextLink`, delta queries that end
//! with an `@odata.deltaLink`, and throttling (HTTP 429, and 503/504 for requests that may be repeated) waited
//! out as the server's `Retry-After` asks, within bounds. The mail resources (mail folders, messages, delta,
//! MIME content, sending, attachments with upload sessions) are in [`mail`]; contacts, calendars and To Do
//! tasks (delta rounds, meeting responses, the mailbox time zone) in [`pim`].
//!
//! **Hostile input.** Every answer is size-capped ([`GraphClient::max_json`], [`GraphClient::max_content`]),
//! collections are read up to a caller-given number of entries, and a `nextLink` or `deltaLink` that leaves the
//! API's origin (scheme, host, port) is refused, so the access token never goes to another server. Malformed
//! JSON is an error, never a panic.
//!
//! **Ids.** Every request asks for immutable ids (`Prefer: IdType="ImmutableId"`), so a message keeps its id
//! when it moves between folders; a server that ignores that is still handled (a move answers with the
//! message's new id).

pub mod mail;
pub mod pim;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpResponse, Url};

/// The Microsoft Graph v1.0 endpoint of the global cloud.
pub const DEFAULT_BASE: &str = "https://graph.microsoft.com/v1.0";
/// Resource prefix of Graph's delegated permission scopes.
pub const SCOPE_PREFIX: &str = "https://graph.microsoft.com/";
/// Delegated permissions the mail part needs: read and change the user's mail, send as the user, and a
/// refresh token (`offline_access`). Calendars, contacts and tasks add their own ([`scopes`]).
pub const MAIL_SCOPES: [&str; 3] =
    ["https://graph.microsoft.com/Mail.ReadWrite", "https://graph.microsoft.com/Mail.Send", "offline_access"];

/// Largest JSON answer read (a page of a collection, a message's properties).
pub const MAX_JSON: usize = 16 * 1024 * 1024;
/// Largest MIME content or file read (a message's `$value`).
pub const MAX_CONTENT: usize = 160 * 1024 * 1024;
/// Most times one request is repeated after throttling.
pub const MAX_RETRIES: u32 = 5;
/// Longest single wait a `Retry-After` may ask for (a longer one is cut to this).
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
/// Longest error text kept from an answer.
const MAX_ERROR_CHARS: usize = 300;

/// The delegated scopes to sign in with: [`MAIL_SCOPES`] and then `extra` (bare names such as
/// `Calendars.ReadWrite` get the Graph resource prefix), without duplicates, in that order.
pub fn scopes(extra: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in MAIL_SCOPES.iter().copied().chain(extra.iter().copied()) {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        let full = if s.contains('/') || s == "offline_access" || s == "openid" || s == "profile" {
            s.to_owned()
        } else {
            format!("{SCOPE_PREFIX}{s}")
        };
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&full)) {
            out.push(full);
        }
    }
    out
}

/// True when `scope` is a Graph permission (`https://graph.microsoft.com/…`).
pub fn is_graph_scope(scope: &str) -> bool {
    scope.trim().to_ascii_lowercase().starts_with(SCOPE_PREFIX)
}

/// One page of a collection: its entries and where it continues.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub value: Vec<Value>,
    /// The next page (`@odata.nextLink`).
    pub next_link: Option<String>,
    /// A delta query is done: the link for the changes after this one (`@odata.deltaLink`).
    pub delta_link: Option<String>,
}

impl Page {
    /// Reads a collection page. An answer that is not an object with a `value` array is a protocol error.
    pub fn from_json(v: &Value) -> Result<Page> {
        let value =
            v.get("value").and_then(Value::as_array).cloned().ok_or_else(|| {
                NetError::Protocol("Microsoft Graph sent a collection without `value`".into())
            })?;
        let link = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned).filter(|s| !s.is_empty());
        Ok(Page { value, next_link: link("@odata.nextLink"), delta_link: link("@odata.deltaLink") })
    }
}

/// How long to wait before repeating a throttled request: `Retry-After` in seconds (an HTTP date or junk
/// counts as absent), else an exponential backoff from one second; never more than [`MAX_RETRY_WAIT`].
pub fn retry_wait(retry_after: Option<&str>, attempt: u32) -> Duration {
    let asked = retry_after.and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
    let backoff = Duration::from_secs(1u64 << attempt.min(6));
    asked.unwrap_or(backoff).min(MAX_RETRY_WAIT)
}

/// The Graph error of a failed answer: `{"error": {"code", "message"}}` when the body has one, else the status.
pub fn error_of(resp: &HttpResponse) -> NetError {
    let parsed: Option<Value> = serde_json::from_slice(&resp.body).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));
    let text = |k: &str| {
        err.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .map(|s| s.chars().take(MAX_ERROR_CHARS).collect::<String>())
            .unwrap_or_default()
    };
    let (code, message) = (text("code"), text("message"));
    if resp.status == 401 {
        let why = if message.is_empty() { "the access token was refused".to_owned() } else { message };
        return NetError::Auth(format!("Microsoft Graph: {why}"));
    }
    if code.is_empty() && message.is_empty() {
        return NetError::Http { status: resp.status, body: resp.text_snippet() };
    }
    NetError::Graph { status: resp.status, code, message }
}

/// A body sent with a request.
#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    None,
    Json(&'a Value),
    /// Raw bytes of this content type (`text/plain` for base64 MIME).
    Raw(&'a str, &'a [u8]),
}

/// A Microsoft Graph client for one signed-in user.
#[derive(Clone)]
pub struct GraphClient {
    base: Url,
    base_text: String,
    token: String,
    http: HttpClient,
    /// Largest JSON answer read.
    pub max_json: usize,
    /// Largest MIME content or file read.
    pub max_content: usize,
    /// Page size asked of collections (`Prefer: odata.maxpagesize`); 0 leaves it to the server.
    pub page_size: u32,
    /// Most retries of one throttled request.
    pub max_retries: u32,
    /// Further `Prefer` preferences sent with every request (`outlook.timezone="UTC"`).
    pub prefer: Vec<String>,
    cancel: Option<Arc<AtomicBool>>,
    throttled: Arc<std::sync::atomic::AtomicU32>,
}

impl std::fmt::Debug for GraphClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphClient").field("base", &self.base_text).field("token", &"..").finish()
    }
}

impl GraphClient {
    /// A client for the API at `base` (`https://graph.microsoft.com/v1.0`, or a test server's) signing in
    /// with the access token `token`.
    pub fn new(base: &str, token: &str, http: HttpClient) -> Result<GraphClient> {
        let trimmed = base.trim().trim_end_matches('/');
        let url = Url::parse(trimmed)?;
        if token.trim().is_empty() {
            return Err(NetError::Auth("Microsoft Graph needs a sign-in (an OAuth access token)".into()));
        }
        Ok(GraphClient {
            base_text: trimmed.to_owned(),
            base: url,
            token: token.trim().to_owned(),
            http,
            max_json: MAX_JSON,
            max_content: MAX_CONTENT,
            page_size: 100,
            max_retries: MAX_RETRIES,
            prefer: Vec::new(),
            cancel: None,
            throttled: Arc::default(),
        })
    }

    /// Stops waits for throttling (and further retries) as soon as `cancel` is raised.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> GraphClient {
        self.cancel = Some(cancel);
        self
    }

    /// Uses a renewed access token from now on.
    pub fn set_token(&mut self, token: &str) {
        token.trim().clone_into(&mut self.token);
    }

    /// The API base (without a trailing `/`).
    pub fn base(&self) -> &str {
        &self.base_text
    }

    /// How many answers asked this client to slow down (throttling) so far.
    pub fn throttled(&self) -> u32 {
        self.throttled.load(Ordering::SeqCst)
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
    }

    /// The absolute URL for `path` (`/me/mailFolders`, relative to the base) or for a link the server sent
    /// (`@odata.nextLink`), which must stay on the API's origin.
    pub fn url(&self, path_or_link: &str) -> Result<String> {
        let p = path_or_link.trim();
        let lower = p.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            let u = Url::parse(p)?;
            if !u.same_origin(&self.base) {
                return Err(NetError::Protocol(format!(
                    "Microsoft Graph sent a link to another server ({}); it is not followed",
                    u.origin()
                )));
            }
            return Ok(u.to_string());
        }
        if !p.starts_with('/') {
            return Err(NetError::Invalid(format!("Graph path `{p}` must start with `/`")));
        }
        Ok(format!("{}{p}", self.base_text))
    }

    /// Sleeps `d` in small steps; false when cancelled meanwhile.
    fn pause(&self, d: Duration) -> bool {
        let end = Instant::now().checked_add(d).unwrap_or_else(Instant::now);
        loop {
            if self.cancelled() {
                return false;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }

    /// Sends one request to `url` (absolute, already checked) and returns the final answer, any status. A
    /// throttled request (429; 503 and 504 too when `repeatable`) is repeated after the wait the server asks
    /// for, at most [`GraphClient::max_retries`] times.
    fn exchange(
        &self,
        method: &str,
        url: &str,
        body: Body<'_>,
        authorized: bool,
        repeatable: bool,
        max_body: usize,
        extra: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let auth = format!("Bearer {}", self.token);
        let pages = format!("odata.maxpagesize={}", self.page_size);
        let mut prefer = String::from("IdType=\"ImmutableId\"");
        if self.page_size > 0 {
            prefer.push_str(", ");
            prefer.push_str(&pages);
        }
        for p in self.prefer.iter().map(|p| p.trim()).filter(|p| !p.is_empty() && !p.contains(['\r', '\n'])) {
            prefer.push_str(", ");
            prefer.push_str(p);
        }
        let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json"), ("Prefer", &prefer)];
        if authorized {
            headers.push(("Authorization", &auth));
        }
        headers.extend_from_slice(extra);
        let data: Option<Vec<u8>>;
        match body {
            Body::None => data = None,
            Body::Json(v) => {
                data = Some(serde_json::to_vec(v).map_err(|e| NetError::Invalid(e.to_string()))?);
                headers.push(("Content-Type", "application/json"));
            }
            Body::Raw(ctype, b) => {
                data = Some(b.to_vec());
                headers.push(("Content-Type", ctype));
            }
        }
        let mut http = self.http.clone();
        http.max_body = max_body;
        let mut attempt = 0u32;
        loop {
            let resp = http.request(method, url, &headers, data.as_deref())?;
            let throttled = resp.status == 429 || (repeatable && matches!(resp.status, 503 | 504));
            if !throttled {
                return Ok(resp);
            }
            self.throttled.fetch_add(1, Ordering::SeqCst);
            if attempt >= self.max_retries {
                return Err(error_of(&resp));
            }
            let wait = retry_wait(resp.header("retry-after"), attempt);
            log::info!("graph: throttled ({}), waiting {} ms", resp.status, wait.as_millis());
            if !self.pause(wait) {
                return Err(NetError::Timeout("cancelled while Microsoft Graph asked to wait".into()));
            }
            attempt = attempt.saturating_add(1);
        }
    }

    /// A request answered with JSON (or nothing: 204 gives `Value::Null`).
    pub fn call(&self, method: &str, path_or_link: &str, body: Body<'_>) -> Result<Value> {
        let url = self.url(path_or_link)?;
        let repeatable = method != "POST";
        let resp = self.exchange(method, &url, body, true, repeatable, self.max_json, &[])?;
        if !resp.is_success() {
            return Err(error_of(&resp));
        }
        if resp.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&resp.body)
            .map_err(|e| NetError::Protocol(format!("Microsoft Graph sent invalid JSON: {e}")))
    }

    /// GET answered with JSON.
    pub fn get_json(&self, path_or_link: &str) -> Result<Value> {
        self.call("GET", path_or_link, Body::None)
    }

    /// GET of one page of a collection.
    pub fn get_page(&self, path_or_link: &str) -> Result<Page> {
        Page::from_json(&self.get_json(path_or_link)?)
    }

    /// Every entry of a collection, following `@odata.nextLink`, up to `max` entries (the rest is left). A
    /// cancelled client gets an error, never a part of the collection passed off as all of it.
    pub fn get_all(&self, path: &str, max: usize) -> Result<Vec<Value>> {
        self.get_all_checked(path, max).map(|(v, _)| v)
    }

    /// [`GraphClient::get_all`], and whether that was the whole collection (`false`: `max` entries were
    /// reached with more left on the service).
    pub fn get_all_checked(&self, path: &str, max: usize) -> Result<(Vec<Value>, bool)> {
        let mut out = Vec::new();
        let mut next = Some(path.to_owned());
        let mut pages = 0usize;
        let mut whole = true;
        while let Some(link) = next.take() {
            if self.cancelled() {
                return Err(cancelled_error());
            }
            if out.len() >= max {
                whole = false;
                break;
            }
            pages = pages.saturating_add(1);
            if pages > max.saturating_add(1) {
                return Err(NetError::Protocol("Microsoft Graph kept sending empty pages".into()));
            }
            let page = self.get_page(&link)?;
            let room = max.saturating_sub(out.len());
            if page.value.len() > room {
                whole = false;
            }
            out.extend(page.value.into_iter().take(room));
            next = page.next_link;
        }
        Ok((out, whole))
    }

    /// POST of JSON answered with JSON (or nothing).
    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("POST", path, Body::Json(body))
    }

    /// PATCH of JSON answered with JSON (or nothing).
    pub fn patch_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("PATCH", path, Body::Json(body))
    }

    /// DELETE.
    pub fn delete(&self, path: &str) -> Result<()> {
        self.call("DELETE", path, Body::None).map(|_| ())
    }

    /// GET of raw content (a message's MIME source), at most [`GraphClient::max_content`] bytes.
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let url = self.url(path)?;
        let resp = self.exchange("GET", &url, Body::None, true, true, self.max_content, &[])?;
        if !resp.is_success() {
            return Err(error_of(&resp));
        }
        Ok(resp.body)
    }

    /// POST of raw bytes of `ctype` answered with JSON (or nothing).
    pub fn post_raw(&self, path: &str, ctype: &str, body: &[u8]) -> Result<Value> {
        self.call("POST", path, Body::Raw(ctype, body))
    }

    /// PUT of a byte range to an upload session's URL (pre-authorized: no access token goes with it; the URL
    /// may be on another host of the service, but must be https unless the client allows plain text). Returns
    /// the answer's JSON (`nextExpectedRanges` while more is expected; the attachment at the end).
    pub fn put_range(&self, upload_url: &str, start: usize, total: usize, chunk: &[u8]) -> Result<Value> {
        let u = Url::parse(upload_url)?;
        if !u.https && !self.http.options.allow_plaintext_auth {
            return Err(NetError::Insecure(format!("upload session at {}", u.origin())));
        }
        let end = start.saturating_add(chunk.len()).saturating_sub(1);
        let range = format!("bytes {start}-{end}/{total}");
        let len = chunk.len().to_string();
        let extra = [("Content-Range", range.as_str()), ("X-Content-Length", len.as_str())];
        let resp = self.exchange(
            "PUT",
            &u.to_string(),
            Body::Raw("application/octet-stream", chunk),
            false,
            true,
            self.max_json,
            &extra,
        )?;
        if !resp.is_success() {
            return Err(error_of(&resp));
        }
        if resp.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&resp.body)
            .map_err(|e| NetError::Protocol(format!("the upload session sent invalid JSON: {e}")))
    }
}

/// The error of a request stopped because the run was cancelled.
pub(crate) fn cancelled_error() -> NetError {
    NetError::Timeout("cancelled".into())
}

/// Percent-encodes a Graph id for a path segment (ids may hold `/`, `+` and `=`).
pub fn seg(id: &str) -> String {
    crate::http::percent_encode(id)
}

#[cfg(test)]
mod tests;
