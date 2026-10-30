//! A small web server for discovery and sign-in tests (HTTP and HTTPS on 127.0.0.1):
//!
//! - autoconfig documents set with [`crate::TestServers::set_autoconfig`], at
//!   `/autoconfig/<domain>/mail/config-v1.1.xml` (standing in for `https://autoconfig.<domain>/…`) and
//!   `/<domain>/.well-known/autoconfig/mail/config-v1.1.xml`;
//! - tenants' OpenID configurations set with [`crate::TestServers::set_tenant`], at
//!   `/tenant/<domain>/v2.0/.well-known/openid-configuration` (standing in for the Microsoft identity
//!   platform's), 400 `invalid_tenant` for other domains;
//! - the OAuth 2.0 authorization server of [`crate::oauth`]: `POST /oauth/device` (device authorization),
//!   `POST /oauth/token`, `GET /oauth/authorize` (answers with a redirect at once) and `GET /oauth/verify`.
//!
//! - the WebDAV / CardDAV / CalDAV server of [`crate::dav`] under `/dav/` and the `.well-known` DAV locations.
//!
//! - the fake Microsoft Graph mail service of [`crate::graph`] under `/graph/v1.0/` (and its upload sessions
//!   under `/graph-upload/`).
//! - Exchange Web Services and Autodiscover ([`crate::ews`]) at `/EWS/Exchange.asmx` and
//!   `…/autodiscover/autodiscover.xml`, on kept-alive connections.
//!
//! Every other request path is logged ([`crate::TestServers::web_requests`]), one request per connection.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::conn::Conn;
use crate::jmap::{Request, read_request, respond};
use crate::server::Ctx;

/// Documents and the request log.
#[derive(Debug, Default)]
pub struct WebState {
    /// Documents by (domain, served at the `autoconfig.<domain>` location too).
    pub autoconfig: BTreeMap<String, (String, bool)>,
    /// Tenant ids by domain.
    pub tenants: BTreeMap<String, String>,
    pub requests: Vec<String>,
}

pub type SharedWeb = Arc<Mutex<WebState>>;

/// Most requests kept in the log.
const MAX_LOG: usize = 1000;

pub fn serve(mut conn: Conn, ctx: &Ctx) {
    // Graph and Exchange Web Services take whole messages (base64) in one request.
    let max = crate::dav::MAX_RESOURCE.max(ctx.config.max_message_size.saturating_mul(2)) + 64 * 1024;
    if let Ok(Some(req)) = read_request(&mut conn, max) {
        let path = req.path.split('?').next().unwrap_or_default();
        if crate::ews::handles(path) {
            // Exchange Web Services and Autodiscover keep the connection (NTLM signs in a connection).
            crate::ews::serve(conn, ctx, req);
            return;
        }
        let _ = if crate::graph::handles(path) {
            crate::graph::handle(&mut conn, ctx, &req)
        } else if crate::dav::handles(path) {
            crate::dav::handle(&mut conn, ctx, &req)
        } else {
            handle(&mut conn, ctx, &req)
        };
    }
    conn.close();
}

fn json(conn: &mut Conn, status: u16, v: &Value) -> io::Result<()> {
    let line = match status {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        _ => "500 Internal Server Error",
    };
    let body = v.to_string();
    let out = format!(
        "HTTP/1.1 {line}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(out.as_bytes())
}

fn handle(conn: &mut Conn, ctx: &Ctx, req: &Request) -> io::Result<()> {
    {
        let mut w = ctx.web.lock().unwrap_or_else(|e| e.into_inner());
        if w.requests.len() < MAX_LOG {
            w.requests.push(format!("{} {}", req.method, req.path));
        }
    }
    let (path, query) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));
    let scheme = if conn.is_tls() { "https" } else { "http" };
    let base = format!("{scheme}://{}", req.header("host").unwrap_or("127.0.0.1"));
    match (req.method.as_str(), path) {
        ("POST", "/oauth/device") => {
            let form = crate::oauth::parse_form(&String::from_utf8_lossy(&req.body));
            let (st, v) = ctx.shared.lock().oauth.device_authorization(&form, &base);
            json(conn, st, &v)
        }
        ("POST", "/oauth/token") => {
            let form = crate::oauth::parse_form(&String::from_utf8_lossy(&req.body));
            let (st, v) = ctx.shared.lock().oauth.token_endpoint(&form);
            json(conn, st, &v)
        }
        ("GET", "/oauth/authorize") => {
            let q = crate::oauth::parse_form(query);
            let res = {
                let mut store = ctx.shared.lock();
                let users: Vec<String> =
                    q.get("login_hint").filter(|u| store.user(u).is_some()).cloned().into_iter().collect();
                store.oauth.authorize(&q, |u| users.iter().any(|x| x == u))
            };
            match res {
                Ok(loc) => conn.write_all(
                    format!("HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                ),
                Err(e) => respond(conn, "400 Bad Request", "text/plain", e.as_bytes()),
            }
        }
        ("GET", "/oauth/verify") => respond(
            conn,
            "200 OK",
            "text/plain",
            b"Test sign-in page: tests approve codes through TestServers.",
        ),
        ("GET", p) if p.starts_with("/tenant/") => {
            let domain = p
                .strip_prefix("/tenant/")
                .and_then(|r| r.strip_suffix("/v2.0/.well-known/openid-configuration"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let tenant = ctx.web.lock().unwrap_or_else(|e| e.into_inner()).tenants.get(&domain).cloned();
            match tenant {
                Some(t) => json(
                    conn,
                    200,
                    &serde_json::json!({
                        "issuer": format!("{base}/{t}/v2.0"),
                        "authorization_endpoint": format!("{base}/{t}/oauth2/v2.0/authorize"),
                        "token_endpoint": format!("{base}/{t}/oauth2/v2.0/token"),
                        "response_types_supported": ["code"],
                    }),
                ),
                None => json(
                    conn,
                    400,
                    &serde_json::json!({"error": "invalid_tenant", "error_description": "unknown tenant"}),
                ),
            }
        }
        ("GET", p) => {
            let sub = p.strip_prefix("/autoconfig/").and_then(|r| r.strip_suffix("/mail/config-v1.1.xml"));
            let well_known = p
                .strip_prefix('/')
                .and_then(|r| r.strip_suffix("/.well-known/autoconfig/mail/config-v1.1.xml"));
            let doc = {
                let w = ctx.web.lock().unwrap_or_else(|e| e.into_inner());
                match (sub, well_known) {
                    (Some(d), _) => w.autoconfig.get(&d.to_ascii_lowercase()).filter(|(_, both)| *both),
                    (None, Some(d)) => w.autoconfig.get(&d.to_ascii_lowercase()),
                    (None, None) => None,
                }
                .map(|(x, _)| x.clone())
            };
            match doc {
                Some(xml) => respond(conn, "200 OK", "application/xml", xml.as_bytes()),
                None => respond(conn, "404 Not Found", "text/plain", b"not found"),
            }
        }
        _ => respond(conn, "404 Not Found", "text/plain", b"not found"),
    }
}
