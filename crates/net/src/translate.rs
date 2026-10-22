//! A client for a LibreTranslate-compatible translation server (`POST /translate`, `POST /detect`), the
//! open-source machine translation service the user can run on their own computer or network.
//!
//! Nothing here runs unless the user entered a server: the app never picks one. Plain `http://` is only
//! accepted for a server on this computer (`localhost`, `127.0.0.1`, `::1`); anything else must be
//! `https://`, so message text and the API key are not sent across a network in the clear. A plain `http://`
//! name is checked again on connecting: every address it resolves to must be a loopback address
//! ([`ConnectOptions::loopback_only`]), so no DNS answer can send the request to another computer.

use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{NetError, Result};
use crate::http::{HttpClient, Url};
use crate::transport::ConnectOptions;

/// Longest text sent in one request (bytes).
pub const MAX_TEXT_BYTES: usize = 512 * 1024;
/// Largest response accepted (bytes).
const MAX_RESPONSE: usize = 8 * 1024 * 1024;
/// Longest API key accepted (characters).
pub const MAX_KEY_CHARS: usize = 512;

/// A translation server.
#[derive(Clone, Debug)]
pub struct LibreTranslate {
    base: Url,
    api_key: Option<String>,
    http: HttpClient,
}

/// True for a host name or address on this computer: a loopback IP address (127.0.0.0/8, `::1`), or
/// `localhost` and names under it (RFC 6761 section 6.3). A name that merely starts with `127.` is not one.
fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost" || h.ends_with(".localhost")
}

impl LibreTranslate {
    /// A client for the server at `url` (its base address, for example `http://localhost:5000`), with an
    /// optional API key. `options` sets the connection timeouts and trusted certificates.
    pub fn new(url: &str, api_key: Option<&str>, options: ConnectOptions) -> Result<LibreTranslate> {
        let mut base = Url::parse(url)?;
        if !base.https && !is_loopback(&base.host) {
            return Err(NetError::Insecure(format!(
                "{}: use https:// for a translation server that is not on this computer",
                base.origin()
            )));
        }
        // The base is a directory: `…/lt` and `…/lt/` both mean `…/lt/translate`.
        let mut path = base.path.split('?').next().unwrap_or("/").to_owned();
        if !path.ends_with('/') {
            path.push('/');
        }
        base.path = path;
        let key = api_key.map(str::trim).filter(|k| !k.is_empty());
        if let Some(k) = key
            && (k.chars().count() > MAX_KEY_CHARS || k.chars().any(char::is_control))
        {
            return Err(NetError::Invalid("the API key is not valid".into()));
        }
        let mut options = options;
        // Plain http: the name must still be this computer once resolved (checked on every connection).
        options.loopback_only |= !base.https;
        let mut http = HttpClient::new(options);
        http.max_body = MAX_RESPONSE;
        http.max_redirects = 0;
        Ok(LibreTranslate { base, api_key: key.map(str::to_owned), http })
    }

    /// Default timeouts for a translation: a server that does not answer in time is given up on.
    pub fn default_options() -> ConnectOptions {
        ConnectOptions {
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(60),
            ..ConnectOptions::default()
        }
    }

    /// The server's address as entered (origin and path).
    pub fn server(&self) -> String {
        self.base.to_string()
    }

    fn post(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        if let (Some(k), Some(o)) = (&self.api_key, body.as_object_mut()) {
            o.insert("api_key".into(), json!(k));
        }
        let url = self.base.join(endpoint)?;
        let resp = self.http.post_json(&url.to_string(), &[("Accept", "application/json")], &body)?;
        if !resp.is_success() {
            // LibreTranslate reports `{"error": "…"}` with 400, 403 (bad key), 429 (slow down) or 500.
            let why = resp
                .json()
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(|e| e.chars().take(300).collect()))
                .unwrap_or_else(|| resp.text_snippet());
            return Err(match resp.status {
                401 | 403 => NetError::Auth(format!("the translation server refused the request: {why}")),
                status => NetError::Http { status, body: why },
            });
        }
        resp.json()
    }

    /// Translates `text` into `target` (a language code such as `fr`), the source language detected by the
    /// server. Returns the translation and, when the server reports it, the detected source language.
    pub fn translate(&self, text: &str, target: &str) -> Result<(String, Option<String>)> {
        check_text(text)?;
        let target = check_lang(target)?;
        let v = self
            .post("translate", json!({ "q": text, "source": "auto", "target": target, "format": "text" }))?;
        let out = v
            .get("translatedText")
            .and_then(Value::as_str)
            .ok_or_else(|| NetError::Protocol("the translation server sent no translatedText".into()))?;
        let detected = v
            .get("detectedLanguage")
            .and_then(|d| d.get("language"))
            .and_then(Value::as_str)
            .and_then(|l| check_lang(l).ok());
        Ok((out.to_owned(), detected))
    }

    /// The language of `text` as the server detects it (most confident first), if it answers with one.
    pub fn detect(&self, text: &str) -> Result<Option<String>> {
        check_text(text)?;
        let v = self.post("detect", json!({ "q": text }))?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(|d| d.get("language"))
            .and_then(Value::as_str)
            .and_then(|l| check_lang(l).ok()))
    }
}

fn check_text(text: &str) -> Result<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(NetError::Invalid("the text is too long to translate at once".into()));
    }
    Ok(())
}

/// A language code (`fr`, `pt-BR`, `zh-Hans`), lower-cased primary subtag; refuses anything else.
fn check_lang(tag: &str) -> Result<String> {
    let t = tag.trim();
    let mut parts = t.split(['-', '_']);
    let primary = parts.next().unwrap_or_default();
    let ok_primary = (2..=3).contains(&primary.len()) && primary.chars().all(|c| c.is_ascii_alphabetic());
    let ok_rest = parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
    if !ok_primary || !ok_rest || t.len() > 35 {
        return Err(NetError::Invalid(format!(
            "`{}` is not a language code",
            t.chars().take(40).collect::<String>()
        )));
    }
    Ok(t.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::http::testserver::{json as reply, serve};

    #[test]
    fn translates_through_a_local_server() {
        let (url, t, seen) = serve(2, |req| match req.path.as_str() {
            "/lt/translate" => reply(
                200,
                r#"{"translatedText":"Hello","detectedLanguage":{"confidence":92,"language":"fr"}}"#,
            ),
            _ => reply(200, r#"[{"confidence":90.0,"language":"de"}]"#),
        });
        let lt = LibreTranslate::new(&format!("{url}/lt"), Some("k-123"), LibreTranslate::default_options())
            .unwrap();
        let (out, from) = lt.translate("Bonjour", "en").unwrap();
        assert_eq!((out.as_str(), from.as_deref()), ("Hello", Some("fr")));
        let req = seen.recv().unwrap();
        assert_eq!(req.method, "POST");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["q"], "Bonjour");
        assert_eq!(body["target"], "en");
        assert_eq!(body["source"], "auto");
        assert_eq!(body["api_key"], "k-123");
        assert_eq!(lt.detect("Guten Tag").unwrap().as_deref(), Some("de"));
        assert_eq!(seen.recv().unwrap().path, "/lt/detect");
        t.join().unwrap();
    }

    #[test]
    fn server_errors_and_bad_answers() {
        let (url, t, _) = serve(3, |req| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            match body["q"].as_str() {
                Some("key") => reply(403, r#"{"error":"Invalid API key"}"#),
                Some("busy") => reply(429, r#"{"error":"Slowdown"}"#),
                _ => reply(200, r#"{"nothing":true}"#),
            }
        });
        let lt = LibreTranslate::new(&url, None, LibreTranslate::default_options()).unwrap();
        let e = lt.translate("key", "en").unwrap_err();
        assert!(matches!(e, NetError::Auth(_)) && e.to_string().contains("Invalid API key"), "{e}");
        assert!(matches!(lt.translate("busy", "en"), Err(NetError::Http { status: 429, .. })));
        assert!(matches!(lt.translate("x", "en"), Err(NetError::Protocol(_))));
        t.join().unwrap();
        // Refused before anything is sent.
        assert!(lt.translate("x", "not a tag!").is_err());
        assert!(lt.translate(&"x".repeat(MAX_TEXT_BYTES + 1), "en").is_err());
    }

    /// Regression: a plain-http name under `.localhost` was trusted on its name alone, but it goes through
    /// the system resolver, and where that asks DNS the answer can be another computer. The resolved
    /// addresses are checked now: anything but loopback is refused before a byte is sent.
    #[test]
    fn plain_http_connects_only_to_loopback_addresses() {
        let mut o = LibreTranslate::default_options();
        o.loopback_only = true;
        let far = crate::transport::Conn::connect("192.0.2.1", 9, false, &o);
        assert!(matches!(far, Err(NetError::Insecure(_))), "{:?}", far.err());
        let mut http = HttpClient::new(o.clone());
        http.max_redirects = 0;
        let r = http.post_json("http://192.0.2.1:9/translate", &[], &json!({"api_key": "k"}));
        assert!(matches!(r, Err(NetError::Insecure(_))), "{:?}", r.err());
        // A loopback server is reached as before.
        let (url, t, _) = serve(1, |_| reply(200, r#"{"translatedText":"Hi"}"#));
        let lt = LibreTranslate::new(&url, Some("k"), o).unwrap();
        assert_eq!(lt.translate("Salut", "en").unwrap().0, "Hi");
        t.join().unwrap();
    }

    #[test]
    fn only_https_leaves_this_computer() {
        let o = LibreTranslate::default_options;
        assert!(matches!(
            LibreTranslate::new("http://translate.example.org", None, o()),
            Err(NetError::Insecure(_))
        ));
        assert!(LibreTranslate::new("https://translate.example.org/api", None, o()).is_ok());
        assert!(LibreTranslate::new("http://localhost:5000", None, o()).is_ok());
        assert!(LibreTranslate::new("http://127.0.0.1:5000", None, o()).is_ok());
        assert!(LibreTranslate::new("http://[::1]:5000", None, o()).is_ok());
        assert!(LibreTranslate::new("http://127.8.9.10:5000", None, o()).is_ok());
        assert!(LibreTranslate::new("http://lt.localhost:5000", None, o()).is_ok());
        // Regression: names that only look like a loopback address are other computers.
        for url in
            ["http://127.attacker.example", "http://127.0.0.1.example.net", "http://localhost.example.com"]
        {
            assert!(matches!(LibreTranslate::new(url, None, o()), Err(NetError::Insecure(_))), "{url}");
        }
        assert!(LibreTranslate::new("ftp://example.org", None, o()).is_err());
        assert!(LibreTranslate::new("http://localhost", Some("bad\nkey"), o()).is_err());
        let lt = LibreTranslate::new("https://translate.example.org/api", Some("  "), o()).unwrap();
        assert!(lt.api_key.is_none());
        assert_eq!(lt.server(), "https://translate.example.org/api/");
        assert_eq!(check_lang("pt-BR").unwrap(), "pt-BR");
        // Plain http is held to loopback addresses when connecting; https is not.
        assert!(
            LibreTranslate::new("http://lt.localhost:5000", None, o()).unwrap().http.options.loopback_only
        );
        assert!(
            !LibreTranslate::new("https://lt.example.org", None, o()).unwrap().http.options.loopback_only
        );
        assert!(check_lang("e").is_err() && check_lang("en-").is_err());
    }
}
