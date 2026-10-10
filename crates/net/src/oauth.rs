//! OAuth 2.0 for mail login: authorization-code flow with PKCE (RFC 6749 §4.1, RFC 7636) through a loopback
//! redirect (RFC 8252 §7.3), the device authorization grant (RFC 8628), and token refresh (RFC 6749 §6).
//!
//! Providers are data ([`OAuthProvider`], loadable from JSON); no client ids or secrets live in code.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpResponse, Url, percent_decode, percent_encode};

/// An OAuth provider's endpoints and this application's registration with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthProvider {
    /// Stable key ("example").
    pub id: String,
    pub name: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
    pub client_id: String,
    /// Only for providers that issue a (non-confidential) secret to installed apps.
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    /// Mail domains this provider serves (for picking a provider from an address).
    pub domains: Vec<String>,
    /// Extra authorization-request parameters (`access_type=offline`, `prompt=consent`, …).
    pub extra_auth_params: Vec<(String, String)>,
}

/// Parses a JSON array of providers.
pub fn providers_from_json(s: &str) -> Result<Vec<OAuthProvider>> {
    serde_json::from_str(s).map_err(|e| NetError::Invalid(format!("OAuth provider list: {e}")))
}

/// The provider serving `address`'s domain.
pub fn provider_for_address<'a>(providers: &'a [OAuthProvider], address: &str) -> Option<&'a OAuthProvider> {
    let domain = address.rsplit_once('@').map(|(_, d)| d).unwrap_or(address);
    providers.iter().find(|p| p.domains.iter().any(|d| d.eq_ignore_ascii_case(domain)))
}

/// Tokens from a token endpoint.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub token_type: String,
    /// Unix seconds.
    pub expires_at: Option<u64>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl TokenSet {
    fn from_json(v: &Value, now: u64) -> Result<TokenSet> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let access_token = s("access_token")
            .ok_or_else(|| NetError::Protocol("token response without access_token".into()))?;
        let expires_in =
            v.get("expires_in").and_then(|e| e.as_u64().or_else(|| e.as_str().and_then(|s| s.parse().ok())));
        Ok(TokenSet {
            access_token,
            token_type: s("token_type").unwrap_or_else(|| "Bearer".into()),
            expires_at: expires_in.map(|e| now.saturating_add(e)),
            refresh_token: s("refresh_token"),
            scope: s("scope"),
            id_token: s("id_token"),
        })
    }

    /// Whether the access token expires within `skew` seconds of `now` (unknown expiry counts as valid).
    pub fn is_expired(&self, now: u64, skew: u64) -> bool {
        self.expires_at.is_some_and(|t| now.saturating_add(skew) >= t)
    }
}

/// Current Unix time in seconds.
pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut v)
        .map_err(|_| NetError::Io("the system random generator failed".into()))?;
    Ok(v)
}

/// A random `state` value (base64url, 128 bits).
pub fn random_state() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(16)?))
}

/// PKCE code verifier and S256 challenge (RFC 7636).
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce").field("challenge", &self.challenge).finish_non_exhaustive()
    }
}

impl Pkce {
    /// A fresh verifier from 32 random bytes (43 characters).
    pub fn generate() -> Result<Pkce> {
        Ok(Pkce::from_verifier(&URL_SAFE_NO_PAD.encode(random_bytes(32)?)))
    }
    /// The S256 challenge for a given verifier.
    pub fn from_verifier(verifier: &str) -> Pkce {
        let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
        Pkce { verifier: verifier.to_owned(), challenge: URL_SAFE_NO_PAD.encode(digest.as_ref()) }
    }
    pub fn method(&self) -> &'static str {
        "S256"
    }
}

/// Builds the authorization request URL.
pub fn authorization_url(
    p: &OAuthProvider,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
    login_hint: Option<&str>,
) -> Result<String> {
    let base = Url::parse(&p.authorization_endpoint)?;
    let mut q: Vec<(String, String)> = vec![
        ("response_type".into(), "code".into()),
        ("client_id".into(), p.client_id.clone()),
        ("redirect_uri".into(), redirect_uri.into()),
        ("scope".into(), p.scopes.join(" ")),
        ("state".into(), state.into()),
        ("code_challenge".into(), pkce.challenge.clone()),
        ("code_challenge_method".into(), pkce.method().into()),
    ];
    if let Some(h) = login_hint {
        q.push(("login_hint".into(), h.into()));
    }
    q.extend(p.extra_auth_params.iter().cloned());
    let query = q
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let sep = if base.path.contains('?') { '&' } else { '?' };
    Ok(format!("{base}{sep}{query}"))
}

/// Query parameters of a URL or bare query string.
pub fn query_params(url_or_query: &str) -> Vec<(String, String)> {
    let q = url_or_query.split_once('?').map(|(_, q)| q).unwrap_or(url_or_query);
    let q = q.split('#').next().unwrap_or_default();
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k, true), percent_decode(v, true))
        })
        .collect()
}

/// Extracts the authorization code from the redirect, checking `state`.
pub fn parse_redirect(url_or_query: &str, expected_state: &str) -> Result<String> {
    let params = query_params(url_or_query);
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    if let Some(error) = get("error") {
        return Err(NetError::OAuth { error, description: get("error_description").unwrap_or_default() });
    }
    if get("state").as_deref() != Some(expected_state) {
        return Err(NetError::OAuth {
            error: "invalid_state".into(),
            description: "the redirect's state does not match".into(),
        });
    }
    get("code")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| NetError::Protocol("redirect without an authorization code".into()))
}

fn token_error(resp: &HttpResponse) -> NetError {
    match resp.json() {
        Ok(v) if v.get("error").is_some() => NetError::OAuth {
            error: v.get("error").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
            description: v.get("error_description").and_then(Value::as_str).unwrap_or_default().to_owned(),
        },
        _ => NetError::Http { status: resp.status, body: resp.text_snippet() },
    }
}

/// Codes, verifiers, refresh tokens and client secrets go only to an `https://` endpoint (RFC 6749 section
/// 3.2), unless the client explicitly allows plaintext.
fn require_https(http: &HttpClient, endpoint: &str) -> Result<()> {
    if Url::parse(endpoint)?.https || http.options.allow_plaintext_auth {
        Ok(())
    } else {
        Err(NetError::Insecure(format!("OAuth endpoint {endpoint} is not https")))
    }
}

fn token_request<'a>(
    http: &HttpClient,
    p: &'a OAuthProvider,
    mut form: Vec<(&'a str, &'a str)>,
) -> Result<TokenSet> {
    require_https(http, &p.token_endpoint)?;
    form.push(("client_id", &p.client_id));
    if let Some(s) = &p.client_secret {
        form.push(("client_secret", s));
    }
    let resp = http.post_form(&p.token_endpoint, &[], &form)?;
    if !resp.is_success() {
        return Err(token_error(&resp));
    }
    let v = resp.json()?;
    if v.get("error").is_some() {
        return Err(token_error(&resp));
    }
    TokenSet::from_json(&v, now_unix())
}

/// Exchanges an authorization code (RFC 6749 §4.1.3 with the PKCE verifier).
pub fn exchange_code(
    http: &HttpClient,
    p: &OAuthProvider,
    code: &str,
    redirect_uri: &str,
    pkce: &Pkce,
) -> Result<TokenSet> {
    token_request(
        http,
        p,
        vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", &pkce.verifier),
        ],
    )
}

/// Refreshes an access token. The old refresh token is kept when the server does not rotate it.
pub fn refresh(http: &HttpClient, p: &OAuthProvider, refresh_token: &str) -> Result<TokenSet> {
    let mut t =
        token_request(http, p, vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token)])?;
    if t.refresh_token.is_none() {
        t.refresh_token = Some(refresh_token.to_owned());
    }
    Ok(t)
}

/// A pending device authorization (RFC 8628 §3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub device_code: String,
    /// Show this to the user.
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    /// Seconds between polls.
    pub interval: u64,
}

/// Starts the device flow.
pub fn start_device_flow(http: &HttpClient, p: &OAuthProvider) -> Result<DeviceAuthorization> {
    let endpoint = p
        .device_authorization_endpoint
        .as_deref()
        .ok_or_else(|| NetError::Unsupported(format!("{} has no device authorization endpoint", p.name)))?;
    require_https(http, endpoint)?;
    let scope = p.scopes.join(" ");
    let mut form = vec![("client_id", p.client_id.as_str()), ("scope", scope.as_str())];
    if let Some(s) = &p.client_secret {
        form.push(("client_secret", s));
    }
    let resp = http.post_form(endpoint, &[], &form)?;
    if !resp.is_success() {
        return Err(token_error(&resp));
    }
    let v = resp.json()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(DeviceAuthorization {
        device_code: s("device_code")
            .ok_or_else(|| NetError::Protocol("device response without device_code".into()))?,
        user_code: s("user_code")
            .ok_or_else(|| NetError::Protocol("device response without user_code".into()))?,
        // Some providers use the draft name `verification_url`.
        verification_uri: s("verification_uri")
            .or_else(|| s("verification_url"))
            .ok_or_else(|| NetError::Protocol("device response without verification_uri".into()))?,
        verification_uri_complete: s("verification_uri_complete"),
        expires_in: v.get("expires_in").and_then(Value::as_u64).unwrap_or(600),
        interval: v.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1),
    })
}

/// Outcome of one device-token poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not finished yet.
    Pending,
    /// Poll less often (add 5 seconds to the interval).
    SlowDown,
    Done(TokenSet),
}

/// Polls the token endpoint once (RFC 8628 §3.4/3.5). Denial and expiry are [`NetError::OAuth`].
pub fn poll_device_token(http: &HttpClient, p: &OAuthProvider, device_code: &str) -> Result<DevicePoll> {
    match token_request(
        http,
        p,
        vec![("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("device_code", device_code)],
    ) {
        Ok(t) => Ok(DevicePoll::Done(t)),
        Err(NetError::OAuth { error, .. }) if error == "authorization_pending" => Ok(DevicePoll::Pending),
        Err(NetError::OAuth { error, .. }) if error == "slow_down" => Ok(DevicePoll::SlowDown),
        Err(e) => Err(e),
    }
}

/// Polls until the user finishes, the code expires, or `sleep` returns false (cancelled). `sleep` is
/// given each wait so callers can sleep, or check a cancel flag.
pub fn wait_for_device_token(
    http: &HttpClient,
    p: &OAuthProvider,
    da: &DeviceAuthorization,
    mut sleep: impl FnMut(Duration) -> bool,
) -> Result<TokenSet> {
    let deadline = crate::transport::deadline_after(Duration::from_secs(da.expires_in));
    let mut interval = da.interval;
    loop {
        if !sleep(Duration::from_secs(interval)) {
            return Err(NetError::OAuth {
                error: "cancelled".into(),
                description: "sign-in was cancelled".into(),
            });
        }
        if Instant::now() > deadline {
            return Err(NetError::OAuth {
                error: "expired_token".into(),
                description: "the device code expired".into(),
            });
        }
        match poll_device_token(http, p, &da.device_code)? {
            DevicePoll::Done(t) => return Ok(t),
            DevicePoll::Pending => {}
            DevicePoll::SlowDown => interval = interval.saturating_add(5),
        }
    }
}

/// A one-shot loopback HTTP listener on 127.0.0.1 receiving the authorization redirect (RFC 8252 §7.3).
#[derive(Debug)]
pub struct LoopbackRedirect {
    listener: TcpListener,
    port: u16,
}

impl LoopbackRedirect {
    /// Binds an ephemeral port on 127.0.0.1.
    pub fn bind() -> Result<LoopbackRedirect> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(NetError::from)?;
        let port = listener.local_addr().map_err(NetError::from)?.port();
        Ok(LoopbackRedirect { listener, port })
    }

    /// The `redirect_uri` to register in the authorization request.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Waits for the browser's redirect and returns its request target (path and query), answering with a
    /// short page telling the user to return to the app.
    pub fn wait(&self, timeout: Duration) -> Result<String> {
        self.listener.set_nonblocking(true).map_err(NetError::from)?;
        let deadline = crate::transport::deadline_after(timeout);
        loop {
            match self.listener.accept() {
                Ok((sock, _)) => {
                    // Any local process can connect here: a connection that fails or sends junk is
                    // dropped and the wait goes on; the request line is read under a size cap.
                    let Some(line) = read_request_line(&sock) else { continue };
                    let target = line.split_whitespace().nth(1).unwrap_or_default().to_owned();
                    // Skip a favicon or other stray request.
                    if !target.contains("code=") && !target.contains("error=") {
                        let mut s = sock;
                        let _ = s.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        continue;
                    }
                    let page = "<!doctype html><meta charset=utf-8><title>MailCraft</title><p>Sign-in finished. You can close this tab and return to MailCraft.</p>";
                    let mut s = sock;
                    let _ = s.write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len())
                            .as_bytes(),
                    );
                    return Ok(target);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(NetError::Timeout("waiting for the browser sign-in".into()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Longest request line accepted on the loopback redirect listener.
const MAX_REDIRECT_LINE: u64 = 16 * 1024;

/// Reads the HTTP request line from a loopback connection: `None` on a socket error, an over-long or
/// non-UTF-8 line.
fn read_request_line(sock: &std::net::TcpStream) -> Option<String> {
    sock.set_nonblocking(false).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut reader = BufReader::new(sock.try_clone().ok()?).take(MAX_REDIRECT_LINE);
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    if line.last() != Some(&b'\n') {
        return None;
    }
    String::from_utf8(line).ok()
}

/// XOAUTH2 initial client response (raw, before base64).
pub fn xoauth2_string(user: &str, access_token: &str) -> Vec<u8> {
    crate::sasl::xoauth2(user, access_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testserver::{json, serve};

    fn provider(base: &str) -> OAuthProvider {
        OAuthProvider {
            id: "test".into(),
            name: "Test".into(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            device_authorization_endpoint: Some(format!("{base}/device")),
            client_id: "client-1".into(),
            client_secret: None,
            scopes: vec!["mail".into(), "offline_access".into()],
            domains: vec!["example.com".into()],
            extra_auth_params: vec![("prompt".into(), "consent".into())],
        }
    }

    fn plaintext_ok() -> HttpClient {
        HttpClient::new(crate::ConnectOptions {
            allow_plaintext_auth: true,
            ..crate::ConnectOptions::default()
        })
    }

    #[test]
    fn pkce_rfc7636_vector() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let g = Pkce::generate().unwrap();
        assert_eq!(g.verifier.len(), 43);
        assert_ne!(g.verifier, Pkce::generate().unwrap().verifier);
        assert!(!format!("{g:?}").contains(&g.verifier));
    }

    #[test]
    fn auth_url_and_redirect() {
        let p = provider("https://login.example.com");
        let pkce = Pkce::from_verifier("v");
        let url =
            authorization_url(&p, "http://127.0.0.1:5555/", &pkce, "st8", Some("a@example.com")).unwrap();
        assert!(url.starts_with("https://login.example.com/authorize?response_type=code&client_id=client-1"));
        let params = query_params(&url);
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("redirect_uri"), Some("http://127.0.0.1:5555/"));
        assert_eq!(get("scope"), Some("mail offline_access"));
        assert_eq!(get("code_challenge_method"), Some("S256"));
        assert_eq!(get("login_hint"), Some("a@example.com"));
        assert_eq!(get("prompt"), Some("consent"));
        assert_eq!(parse_redirect("/?code=abc%2F1&state=st8", "st8").unwrap(), "abc/1");
        assert!(matches!(parse_redirect("/?code=abc&state=other", "st8"), Err(NetError::OAuth { .. })));
        let e =
            parse_redirect("/?error=access_denied&error_description=no+way&state=st8", "st8").unwrap_err();
        assert_eq!(e, NetError::OAuth { error: "access_denied".into(), description: "no way".into() });
        assert!(parse_redirect("/?state=st8", "st8").is_err());
    }

    #[test]
    fn providers_json() {
        let list = providers_from_json(
            r#"[{"id":"x","name":"X","authorization_endpoint":"https://x/a","token_endpoint":"https://x/t","client_id":"c","scopes":["s"],"domains":["x.example"]}]"#,
        )
        .unwrap();
        assert_eq!(list[0].client_secret, None);
        assert_eq!(provider_for_address(&list, "me@X.example").map(|p| p.id.as_str()), Some("x"));
        assert!(provider_for_address(&list, "me@other.example").is_none());
        assert!(providers_from_json("{").is_err());
    }

    #[test]
    fn code_exchange_and_refresh() {
        let (base, t, rx) = serve(3, |s| {
            let body = String::from_utf8_lossy(&s.body).into_owned();
            if body.contains("grant_type=authorization_code") {
                json(
                    200,
                    r#"{"access_token":"AT1","token_type":"Bearer","expires_in":3600,"refresh_token":"RT1","scope":"mail"}"#,
                )
            } else if body.contains("refresh_token=RT1") {
                json(200, r#"{"access_token":"AT2","token_type":"Bearer","expires_in":"60"}"#)
            } else {
                json(400, r#"{"error":"invalid_grant","error_description":"expired"}"#)
            }
        });
        let p = provider(&base);
        // Codes, verifiers and refresh tokens never go to an http:// token endpoint unless allowed.
        let e = refresh(&HttpClient::default(), &p, "RT1").unwrap_err();
        assert!(matches!(e, NetError::Insecure(_)), "{e:?}");
        let http = plaintext_ok();
        let pkce = Pkce::from_verifier("verifier-xyz");
        let t1 = exchange_code(&http, &p, "CODE", "http://127.0.0.1:1/", &pkce).unwrap();
        assert_eq!(t1.access_token, "AT1");
        assert_eq!(t1.refresh_token.as_deref(), Some("RT1"));
        assert!(!t1.is_expired(now_unix(), 60));
        assert!(t1.is_expired(now_unix() + 3600, 0));
        let seen = rx.recv().unwrap();
        let form = query_params(&String::from_utf8_lossy(&seen.body));
        assert!(form.contains(&("code_verifier".into(), "verifier-xyz".into())));
        assert!(form.contains(&("client_id".into(), "client-1".into())));
        assert_eq!(seen.header("content-type"), Some("application/x-www-form-urlencoded"));
        let t2 = refresh(&http, &p, "RT1").unwrap();
        assert_eq!(t2.access_token, "AT2");
        assert_eq!(t2.refresh_token.as_deref(), Some("RT1"));
        let e = refresh(&http, &p, "OLD").unwrap_err();
        assert_eq!(e, NetError::OAuth { error: "invalid_grant".into(), description: "expired".into() });
        assert!(!format!("{t2:?}").contains("AT2"));
        t.join().unwrap();
    }

    #[test]
    fn device_flow() {
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pc = polls.clone();
        let (base, t, _rx) = serve(4, move |s| {
            if s.path == "/device" {
                return json(
                    200,
                    r#"{"device_code":"DC","user_code":"WDJB-MJHT","verification_uri":"https://example.com/device","expires_in":1800,"interval":1}"#,
                );
            }
            match pc.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => json(400, r#"{"error":"authorization_pending"}"#),
                1 => json(400, r#"{"error":"slow_down"}"#),
                _ => json(200, r#"{"access_token":"DEV","token_type":"Bearer"}"#),
            }
        });
        let p = provider(&base);
        assert!(matches!(start_device_flow(&HttpClient::default(), &p), Err(NetError::Insecure(_))));
        let http = plaintext_ok();
        let da = start_device_flow(&http, &p).unwrap();
        assert_eq!(da.user_code, "WDJB-MJHT");
        let mut waits = Vec::new();
        let tok = wait_for_device_token(&http, &p, &da, |d| {
            waits.push(d.as_secs());
            true
        })
        .unwrap();
        assert_eq!(tok.access_token, "DEV");
        assert_eq!(waits, vec![1, 1, 6]);
        t.join().unwrap();
        let mut no_device = provider(&base);
        no_device.device_authorization_endpoint = None;
        assert!(matches!(start_device_flow(&http, &no_device), Err(NetError::Unsupported(_))));
        let cancelled = wait_for_device_token(&http, &p, &da, |_| false).unwrap_err();
        assert!(matches!(cancelled, NetError::OAuth { ref error, .. } if error == "cancelled"));
    }

    #[test]
    fn loopback_redirect() {
        let lb = LoopbackRedirect::bind().unwrap();
        let uri = lb.redirect_uri();
        let t = std::thread::spawn(move || {
            let c = HttpClient::default();
            let _ = c.get(&format!("{uri}favicon.ico"), &[]);
            c.get(&format!("{uri}?code=XYZ&state=S"), &[]).unwrap()
        });
        let target = lb.wait(Duration::from_secs(10)).unwrap();
        assert_eq!(parse_redirect(&target, "S").unwrap(), "XYZ");
        let page = t.join().unwrap();
        assert_eq!(page.status, 200);
        let lb = LoopbackRedirect::bind().unwrap();
        assert!(matches!(lb.wait(Duration::from_millis(100)), Err(NetError::Timeout(_))));
    }

    #[test]
    fn loopback_redirect_survives_stray_connections() {
        // Anything on the machine can connect to the loopback port: junk (non-UTF-8, an endless line) must
        // not abort the sign-in or buffer without bound.
        let lb = LoopbackRedirect::bind().unwrap();
        let uri = lb.redirect_uri();
        let port = uri.trim_start_matches("http://127.0.0.1:").trim_end_matches('/').parse::<u16>().unwrap();
        let t = std::thread::spawn(move || {
            use std::io::Write as _;
            let mut junk = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            junk.write_all(b"\xff\xfe\xfd\r\n").unwrap();
            drop(junk);
            let mut long = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let _ = long.write_all(&vec![b'a'; 200_000]);
            drop(long);
            HttpClient::default().get(&format!("{uri}?code=XYZ&state=S"), &[]).unwrap()
        });
        let target = lb.wait(Duration::from_secs(20)).unwrap();
        assert_eq!(parse_redirect(&target, "S").unwrap(), "XYZ");
        assert_eq!(t.join().unwrap().status, 200);
    }
}
