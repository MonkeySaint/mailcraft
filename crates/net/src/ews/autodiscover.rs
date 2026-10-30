//! Autodiscover for Exchange servers ([MS-OXDSCLI], the plain-old-XML protocol): finds the EWS URL of an
//! address. The locations tried, in order (section 3.1.5):
//!
//! 1. `https://<domain>/autodiscover/autodiscover.xml`, then `https://autodiscover.<domain>/…`, by POST of
//!    the request document;
//! 2. an unauthenticated GET of `http://autodiscover.<domain>/autodiscover/autodiscover.xml`, whose redirect
//!    to an HTTPS location is followed (the plain-HTTP answer itself is never trusted with credentials);
//! 3. the DNS SRV record `_autodiscover._tcp.<domain>`.
//!
//! Answers may redirect to another address (`redirectAddr`: the lookup starts over for it) or to another
//! URL (`redirectUrl`, HTTPS only), at most [`MAX_REDIRECTS`] times.
//!
//! The password goes only to hosts in the address's own domain (the domain or a subdomain of it) and to hosts
//! the user confirmed ([`AutodiscoverOptions::confirmed_hosts`]). The redirect probe, SRV records and
//! redirects are not authenticated, so a location they name in another domain is only asked without
//! credentials; when it answers, the lookup ends with [`Probe::Unconfirmed`] naming the host for the user to
//! confirm. An answer whose EWS URL is outside the domain is treated the same way. Without credentials a server that asks
//! for a sign-in (HTTP 401) is reported as such ([`Probe::NeedsSignIn`]): Exchange servers answer only signed-in
//! requests, so the account wizard can offer an Exchange account and finish the lookup once it has the
//! password.

use crate::autoconfig::parse_xml_with;
use crate::dns::Resolver;
use crate::error::{NetError, Result};
use crate::http::{HttpClient, Url, percent_encode};
use crate::transport::ConnectOptions;

use super::{Signer, esc};

/// The POST locations tried first (`{domain}` is the address's domain).
pub const AUTODISCOVER_URLS: [&str; 2] = [
    "https://{domain}/autodiscover/autodiscover.xml",
    "https://autodiscover.{domain}/autodiscover/autodiscover.xml",
];
/// The plain-HTTP location whose redirect is followed.
pub const REDIRECT_PROBE_URL: &str = "http://autodiscover.{domain}/autodiscover/autodiscover.xml";
/// Most redirects (address or URL) followed.
pub const MAX_REDIRECTS: usize = 10;
/// Largest answer read.
const MAX_ANSWER: usize = 1024 * 1024;

const REQUEST_NS: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006";
const RESPONSE_SCHEMA: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

/// Where and how autodiscover looks.
#[derive(Clone, Debug)]
pub struct AutodiscoverOptions {
    /// POST location templates (see [`AUTODISCOVER_URLS`]).
    pub urls: Vec<String>,
    /// The plain-HTTP redirect probe template (`None`: not tried).
    pub redirect_probe: Option<String>,
    /// For the SRV record (`None`: not looked up).
    pub resolver: Option<Resolver>,
    pub http: ConnectOptions,
    /// Hosts outside the address's domain the user agreed to sign in to (their subdomains too).
    pub confirmed_hosts: Vec<String>,
}

impl Default for AutodiscoverOptions {
    fn default() -> Self {
        AutodiscoverOptions {
            urls: AUTODISCOVER_URLS.iter().map(|s| (*s).to_owned()).collect(),
            redirect_probe: Some(REDIRECT_PROBE_URL.to_owned()),
            resolver: Resolver::system(),
            http: ConnectOptions {
                connect_timeout: std::time::Duration::from_secs(8),
                io_timeout: std::time::Duration::from_secs(15),
                ..ConnectOptions::default()
            },
            confirmed_hosts: Vec::new(),
        }
    }
}

/// The settings an answer gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The address the settings are for (after `redirectAddr`).
    pub address: String,
    pub ews_url: String,
    pub display_name: String,
    /// The autodiscover location that answered.
    pub from: String,
}

/// What a lookup found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    Found(Found),
    /// A server answered at `url` but wants a sign-in first.
    NeedsSignIn {
        url: String,
    },
    /// Signed in, nothing answered in the address's domain, but a host outside it (found through the redirect
    /// probe, SRV, a redirect or as the EWS server) did: the password is sent there only once the user
    /// confirmed `host` (see [`AutodiscoverOptions::confirmed_hosts`]).
    Unconfirmed {
        url: String,
        host: String,
    },
    /// Nothing answered; `notes` say what was tried.
    Nothing {
        notes: Vec<String>,
    },
}

/// One answer document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Settings { ews_url: String, display_name: String },
    RedirectAddr(String),
    RedirectUrl(String),
    Error(String),
}

/// The request document for `address`.
pub fn request_body(address: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Autodiscover xmlns="{REQUEST_NS}"><Request><EMailAddress>{}</EMailAddress><AcceptableResponseSchema>{RESPONSE_SCHEMA}</AcceptableResponseSchema></Request></Autodiscover>"#,
        esc(address)
    )
}

/// Reads an answer document. The EWS URL of the external (`EXPR`) protocol section wins over the internal
/// (`EXCH`) one, then any other `EwsUrl`.
pub fn parse_answer(body: &[u8]) -> Result<Answer> {
    let text = String::from_utf8_lossy(body);
    let root = parse_xml_with(&text, MAX_ANSWER, 20_000)?;
    let resp = root
        .child("Response")
        .ok_or_else(|| NetError::Protocol("the autodiscover answer has no Response".into()))?;
    if let Some(e) = resp.child("Error") {
        let code = e.child_text("ErrorCode").unwrap_or_default();
        let msg = e.child_text("Message").unwrap_or_default();
        return Ok(Answer::Error(format!("{code} {msg}").trim().chars().take(300).collect()));
    }
    let account = resp
        .child("Account")
        .ok_or_else(|| NetError::Protocol("the autodiscover answer has no Account".into()))?;
    match account.child_text("Action").unwrap_or("settings").to_ascii_lowercase().as_str() {
        "redirectaddr" => {
            let a = account.child_text("RedirectAddr").unwrap_or_default();
            return Ok(Answer::RedirectAddr(a.to_owned()));
        }
        "redirecturl" => {
            let u = account.child_text("RedirectUrl").unwrap_or_default();
            return Ok(Answer::RedirectUrl(u.to_owned()));
        }
        _ => {}
    }
    let url_of = |kind: &str| {
        account
            .all("Protocol")
            .filter(|p| p.child_text("Type").is_some_and(|t| t.eq_ignore_ascii_case(kind)))
            .find_map(|p| p.child_text("EwsUrl").filter(|u| !u.is_empty()).map(str::to_owned))
    };
    let ews = url_of("EXPR")
        .or_else(|| url_of("EXCH"))
        .or_else(|| {
            account
                .all("Protocol")
                .find_map(|p| p.child_text("EwsUrl").filter(|u| !u.is_empty()).map(str::to_owned))
        })
        .ok_or_else(|| NetError::Protocol("the autodiscover answer names no EWS URL".into()))?;
    let u = Url::parse(&ews)?;
    if !u.https {
        return Err(NetError::Protocol("the autodiscover answer names a plain-HTTP EWS URL".into()));
    }
    let display_name = resp
        .child("User")
        .and_then(|u| u.child_text("DisplayName"))
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    Ok(Answer::Settings { ews_url: ews, display_name })
}

fn domain_of(address: &str) -> Result<String> {
    let (_, d) = address
        .trim()
        .rsplit_once('@')
        .ok_or_else(|| NetError::Invalid(format!("`{address}` is not an email address")))?;
    let d = d.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() || !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return Err(NetError::Invalid(format!("`{address}` has no usable domain")));
    }
    Ok(d)
}

fn expand(template: &str, domain: &str, address: &str) -> String {
    template.replace("{domain}", domain).replace("{address}", &percent_encode(address))
}

/// The host of `url` when credentials may go there (in `anchor`, the address's domain, or a confirmed host),
/// else `Err` with the host.
fn trusted_host(url: &str, anchor: &str, confirmed: &[String]) -> std::result::Result<String, String> {
    let host = Url::parse(url).map(|u| u.host.trim_end_matches('.').to_ascii_lowercase()).unwrap_or_default();
    let ok = !host.is_empty()
        && (crate::discover::in_domain(&host, anchor)
            || confirmed
                .iter()
                .map(|c| c.trim().trim_end_matches('.'))
                .any(|c| !c.is_empty() && crate::discover::in_domain(&host, c)));
    if ok { Ok(host) } else { Err(host) }
}

/// Finds the EWS URL of `address`, signed in with `signer` (`None`: only probes whether a server answers).
pub fn autodiscover(address: &str, signer: Option<&Signer>, o: &AutodiscoverOptions) -> Result<Probe> {
    let http = HttpClient::new(o.http.clone());
    let mut address = address.trim().to_owned();
    // Credentials go only to this domain (of the address first given) and to confirmed hosts.
    let anchor = domain_of(&address)?;
    let mut notes: Vec<String> = Vec::new();
    let mut needs_sign_in: Option<String> = None;
    let mut unconfirmed: Option<(String, String)> = None;
    let mut refused = false;
    let mut hops = 0usize;
    'addresses: loop {
        let domain = domain_of(&address)?;
        let mut queue: Vec<String> = o.urls.iter().map(|t| expand(t, &domain, &address)).collect();
        let mut probed_redirect = false;
        let mut looked_up_srv = false;
        let mut i = 0usize;
        loop {
            if i >= queue.len() {
                // The plain-HTTP redirect probe, then SRV, add more locations.
                if !probed_redirect {
                    probed_redirect = true;
                    if let Some(t) = &o.redirect_probe {
                        let probe = expand(t, &domain, &address);
                        // One request: the redirect itself is the answer (not followed, so a client that
                        // errs on a redirect limit of 0 never hides it).
                        let deadline = std::time::Instant::now()
                            .checked_add(o.http.io_timeout.saturating_add(o.http.connect_timeout))
                            .unwrap_or_else(std::time::Instant::now);
                        match Url::parse(&probe).and_then(|u| http.get_once(&u, &[], deadline)) {
                            Ok(r) if matches!(r.status, 301 | 302 | 307 | 308) => {
                                if let Some(loc) = r.header("location")
                                    && Url::parse(loc).is_ok_and(|u| u.https)
                                {
                                    queue.push(loc.to_owned());
                                }
                            }
                            Ok(r) => notes.push(format!("{probe}: HTTP {} (no redirect)", r.status)),
                            Err(e) => notes.push(format!("{probe}: {e}")),
                        }
                    }
                    continue;
                }
                if !looked_up_srv {
                    looked_up_srv = true;
                    if let Some(r) = &o.resolver {
                        let name = format!("_autodiscover._tcp.{domain}");
                        match r.lookup_srv(&name) {
                            Ok(list) => {
                                for s in list.iter().filter(|s| !s.target.is_empty()).take(4) {
                                    let host = s.target.trim_end_matches('.');
                                    queue.push(if s.port == 443 {
                                        format!("https://{host}/autodiscover/autodiscover.xml")
                                    } else {
                                        format!("https://{host}:{}/autodiscover/autodiscover.xml", s.port)
                                    });
                                }
                            }
                            Err(e) => notes.push(format!("{name}: {e}")),
                        }
                    }
                    continue;
                }
                break 'addresses;
            }
            let url = queue.get(i).cloned().unwrap_or_default();
            i += 1;
            let body = request_body(&address);
            // A location outside the domain is asked without credentials only.
            let outside = trusted_host(&url, &anchor, &o.confirmed_hosts).err();
            let signed = signer.filter(|_| outside.is_none());
            let resp = match signed {
                Some(sg) => {
                    sg.reset();
                    sg.post(&http, &url, "text/xml; charset=utf-8", body.as_bytes(), &[])
                }
                None => http.request(
                    "POST",
                    &url,
                    &[("Content-Type", "text/xml; charset=utf-8")],
                    Some(body.as_bytes()),
                ),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    notes.push(format!("{url}: {e}"));
                    continue;
                }
            };
            if let (Some(_), Some(host)) = (signer, &outside)
                && (resp.status == 401 || resp.is_success())
            {
                // Something answers there: the user decides whether the password may go to it.
                notes.push(format!("{url}: {host} is outside {anchor}; not signed in there"));
                if unconfirmed.is_none() && !host.is_empty() {
                    unconfirmed = Some((url.clone(), host.clone()));
                }
                continue;
            }
            if resp.status == 401 {
                if signed.is_some() {
                    refused = true;
                    notes.push(format!("{url}: the sign-in was refused"));
                } else if needs_sign_in.is_none() {
                    needs_sign_in = Some(url.clone());
                }
                continue;
            }
            if !resp.is_success() {
                notes.push(format!("{url}: HTTP {}", resp.status));
                continue;
            }
            match parse_answer(&resp.body) {
                Ok(Answer::Settings { ews_url, display_name }) => {
                    if signer.is_some()
                        && let Err(host) = trusted_host(&ews_url, &anchor, &o.confirmed_hosts)
                    {
                        // Every later run signs in there: the user confirms it first.
                        notes.push(format!("{url}: the EWS server {host} is outside {anchor}"));
                        if unconfirmed.is_none() && !host.is_empty() {
                            unconfirmed = Some((ews_url, host));
                        }
                        continue;
                    }
                    return Ok(Probe::Found(Found { address, ews_url, display_name, from: url }));
                }
                Ok(Answer::RedirectAddr(a)) => {
                    hops += 1;
                    if hops > MAX_REDIRECTS || domain_of(&a).is_err() || a.eq_ignore_ascii_case(&address) {
                        notes.push(format!("{url}: redirect to an unusable address"));
                        continue;
                    }
                    address = a;
                    continue 'addresses;
                }
                Ok(Answer::RedirectUrl(u)) => {
                    hops += 1;
                    if hops <= MAX_REDIRECTS && Url::parse(&u).is_ok_and(|x| x.https) {
                        queue.insert(i, u);
                    } else {
                        notes.push(format!("{url}: redirect to an unusable URL"));
                    }
                }
                Ok(Answer::Error(e)) => notes.push(format!("{url}: {e}")),
                Err(e) => notes.push(format!("{url}: {e}")),
            }
        }
    }
    if let Some(url) = needs_sign_in {
        return Ok(Probe::NeedsSignIn { url });
    }
    if refused {
        return Err(NetError::Auth("the autodiscover server refused the sign-in".into()));
    }
    if let Some((url, host)) = unconfirmed {
        return Ok(Probe::Unconfirmed { url, host });
    }
    Ok(Probe::Nothing { notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse() {
        let doc = br#"<?xml version="1.0"?><Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006"><Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a"><User><DisplayName>Pat Example</DisplayName></User><Account><AccountType>email</AccountType><Action>settings</Action><Protocol><Type>EXCH</Type><EwsUrl>https://internal.example.com/EWS/Exchange.asmx</EwsUrl></Protocol><Protocol><Type>EXPR</Type><EwsUrl>https://mail.example.com/EWS/Exchange.asmx</EwsUrl></Protocol></Account></Response></Autodiscover>"#;
        assert_eq!(
            parse_answer(doc).unwrap(),
            Answer::Settings {
                ews_url: "https://mail.example.com/EWS/Exchange.asmx".into(),
                display_name: "Pat Example".into()
            }
        );
        let redirect = br#"<Autodiscover><Response><Account><Action>redirectAddr</Action><RedirectAddr>pat@example.org</RedirectAddr></Account></Response></Autodiscover>"#;
        assert_eq!(parse_answer(redirect).unwrap(), Answer::RedirectAddr("pat@example.org".into()));
        let err = br#"<Autodiscover><Response><Error><ErrorCode>500</ErrorCode><Message>bad address</Message></Error></Response></Autodiscover>"#;
        assert_eq!(parse_answer(err).unwrap(), Answer::Error("500 bad address".into()));
        let plain = br#"<Autodiscover><Response><Account><Protocol><Type>EXPR</Type><EwsUrl>http://mail.example.com/EWS/Exchange.asmx</EwsUrl></Protocol></Account></Response></Autodiscover>"#;
        assert!(parse_answer(plain).is_err(), "plain-HTTP EWS URLs are refused");
        assert!(parse_answer(b"<x>").is_err());
        assert!(parse_answer(b"<Autodiscover/>").is_err());
        assert!(request_body("a&b@example.com").contains("a&amp;b@example.com"));
        assert_eq!(domain_of("pat@Example.COM.").unwrap(), "example.com");
        assert!(domain_of("nope").is_err() && domain_of("x@exa mple.com").is_err());
    }

    #[test]
    fn credentials_go_only_to_the_domain_and_confirmed_hosts() {
        let none: Vec<String> = Vec::new();
        let t = |u: &str, c: &[String]| trusted_host(u, "example.com", c);
        assert_eq!(t("https://example.com/autodiscover/autodiscover.xml", &none).unwrap(), "example.com");
        assert!(t("https://autodiscover.EXAMPLE.com./x", &none).is_ok());
        assert!(t("https://mail.eu.example.com:444/x", &none).is_ok());
        assert_eq!(t("https://evil.example/x", &none).unwrap_err(), "evil.example");
        assert!(t("https://notexample.com/x", &none).is_err(), "a suffix that is not a subdomain");
        assert!(t("https://example.com.evil.example/x", &none).is_err());
        assert!(t("not a url", &none).is_err());
        let ok = vec!["hosted.example.net".to_owned()];
        assert!(t("https://hosted.example.net/x", &ok).is_ok());
        assert!(t("https://ad.hosted.example.net/x", &ok).is_ok());
        assert!(t("https://example.net/x", &ok).is_err());
        assert!(t("https://evil.example/x", &[String::new(), " ".into()]).is_err());
    }
}
