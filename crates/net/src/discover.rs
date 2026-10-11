//! Finding an account's servers from its address, before falling back to guessing:
//!
//! 1. **Autoconfig**: the provider's `clientConfig` document over HTTPS on the mail domain
//!    (`https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=…`, then
//!    `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`), see [`crate::autoconfig`].
//! 2. **DNS SRV** (RFC 6186, with the implicit-TLS records of RFC 8314 section 5.1 and JMAP's RFC 8620
//!    section 2.2): `_imaps._tcp`, `_imap._tcp`, `_pop3s._tcp`, `_pop3._tcp`, `_submissions._tcp`,
//!    `_submission._tcp` and `_jmap._tcp` under the domain, asked of the system resolver.
//!
//! Servers found through DNS outside the address's domain are marked unverified (RFC 6186 section 6): the
//! account wizard shows them for confirmation like any other suggestion. Nothing here sends credentials.
//!
//! Domains reserved for documentation and testing (RFC 2606, RFC 6761: `example.com/.net/.org`, `.example`,
//! `.test`, `.invalid`, `.localhost`) are never looked up unless [`DiscoverOptions::allow_reserved`] is set
//! (tests point the lookups at local servers).

use std::net::SocketAddr;
use std::time::Duration;

use mailcraft_model::Security;

use crate::autoconfig::{self, AutoConfig, Protocol, ServerConfig};
use crate::dns::{Resolver, SrvRecord};
use crate::error::{NetError, Result};
use crate::http::{HttpClient, percent_encode};
use crate::transport::ConnectOptions;

/// The autoconfig locations tried, in order. `{domain}` is the address's domain, `{address}` the
/// percent-encoded address.
pub const AUTOCONFIG_URLS: [&str; 2] = [
    "https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={address}",
    "https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml",
];

/// Which DNS server to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolverChoice {
    /// The first name server of `/etc/resolv.conf`; no DNS lookups where there is none.
    System,
    /// This server (tests: a local fake responder).
    Fixed(SocketAddr),
    /// No DNS lookups.
    Off,
}

/// How discovery runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverOptions {
    pub resolver: ResolverChoice,
    pub dns_timeout: Duration,
    /// Autoconfig URL templates (see [`AUTOCONFIG_URLS`]); empty = no autoconfig.
    pub autoconfig_urls: Vec<String>,
    /// HTTP options for the autoconfig requests.
    pub http: ConnectOptions,
    /// Look up reserved test domains too (only for tests against local servers).
    pub allow_reserved: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        DiscoverOptions {
            resolver: ResolverChoice::System,
            dns_timeout: Duration::from_secs(3),
            autoconfig_urls: AUTOCONFIG_URLS.iter().map(|s| (*s).to_owned()).collect(),
            http: ConnectOptions {
                connect_timeout: Duration::from_secs(8),
                io_timeout: Duration::from_secs(10),
                ..ConnectOptions::default()
            },
            allow_reserved: false,
        }
    }
}

impl DiscoverOptions {
    /// No network lookups at all (discovery always finds nothing).
    pub fn offline() -> DiscoverOptions {
        DiscoverOptions { resolver: ResolverChoice::Off, autoconfig_urls: Vec::new(), ..Default::default() }
    }
}

/// What discovery found. Each protocol has at most one server (the preferred one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub address: String,
    pub domain: String,
    /// Where the settings came from, in order: "autoconfig", "dns".
    pub sources: Vec<String>,
    pub display_name: String,
    pub imap: Option<ServerConfig>,
    pub pop3: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    /// JMAP session URL server (`host` is the URL).
    pub jmap: Option<ServerConfig>,
    /// What was tried and why it did not help (for diagnostics).
    pub notes: Vec<String>,
}

impl Discovery {
    pub fn found_anything(&self) -> bool {
        self.imap.is_some() || self.pop3.is_some() || self.smtp.is_some() || self.jmap.is_some()
    }
}

/// True for domains reserved for documentation and testing (RFC 2606, RFC 6761).
pub fn is_reserved_domain(domain: &str) -> bool {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let in_zone = |z: &str| d == z || d.ends_with(&format!(".{z}"));
    ["example.com", "example.net", "example.org", "example", "test", "invalid", "localhost"]
        .iter()
        .any(|z| in_zone(z))
}

/// The domain of an address, checked to be an ASCII host name (internationalized domains are not looked up).
pub fn address_domain(address: &str) -> Result<String> {
    let a = address.trim();
    let (local, domain) = a
        .rsplit_once('@')
        .filter(|(l, d)| !l.is_empty() && !d.is_empty())
        .ok_or_else(|| NetError::Invalid(format!("not an email address: `{a}`")))?;
    if local.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(NetError::Invalid(format!("not an email address: `{a}`")));
    }
    let domain = domain.to_ascii_lowercase();
    if !autoconfig::valid_host(&domain) || (!domain.contains('.') && domain != "localhost") {
        return Err(NetError::Invalid(format!("`{domain}` is not a domain name that can be looked up")));
    }
    Ok(domain)
}

/// True when `host` is `domain` or inside it.
pub fn in_domain(host: &str, domain: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    h == d || h.ends_with(&format!(".{d}"))
}

fn expand_url(template: &str, domain: &str, address: &str) -> String {
    template.replace("{domain}", domain).replace("{address}", &percent_encode(address))
}

/// Fetches and reads the first autoconfig document that works.
pub fn fetch_autoconfig(
    address: &str,
    domain: &str,
    o: &DiscoverOptions,
    notes: &mut Vec<String>,
) -> Option<AutoConfig> {
    let mut http = HttpClient::new(o.http.clone());
    http.max_body = autoconfig::MAX_DOCUMENT;
    for t in &o.autoconfig_urls {
        let url = expand_url(t, domain, address);
        if !url.to_ascii_lowercase().starts_with("https://") {
            notes.push(format!("{url}: skipped (autoconfig is only read over HTTPS)"));
            continue;
        }
        match http.get(&url, &[("Accept", "application/xml, text/xml")]) {
            Ok(r) if r.is_success() => match autoconfig::parse_autoconfig(&r.body, address) {
                Ok(c) => return Some(c),
                Err(e) => notes.push(format!("{url}: {e}")),
            },
            Ok(r) => notes.push(format!("{url}: HTTP {}", r.status)),
            Err(e) => notes.push(format!("{url}: {e}")),
        }
    }
    None
}

fn first_usable(records: &[SrvRecord]) -> Option<&SrvRecord> {
    // A single "." target: the service is not offered (RFC 2782); otherwise the preferred real target.
    records.iter().find(|r| !r.is_unavailable() && r.port != 0 && autoconfig::valid_host(&r.target))
}

/// SRV records of the mail services under `domain`. Stops early when the resolver does not answer.
pub fn lookup_srv(address: &str, domain: &str, resolver: &Resolver, notes: &mut Vec<String>) -> Discovery {
    let mut d = Discovery::default();
    // (service, protocol, security), preferred first within a protocol.
    let services: [(&str, Protocol, Security); 7] = [
        ("_imaps", Protocol::Imap, Security::Tls),
        ("_imap", Protocol::Imap, Security::StartTls),
        ("_pop3s", Protocol::Pop3, Security::Tls),
        ("_pop3", Protocol::Pop3, Security::StartTls),
        ("_submissions", Protocol::Smtp, Security::Tls),
        ("_submission", Protocol::Smtp, Security::StartTls),
        ("_jmap", Protocol::Jmap, Security::Tls),
    ];
    for (svc, protocol, security) in services {
        let slot = match protocol {
            Protocol::Imap => &mut d.imap,
            Protocol::Pop3 => &mut d.pop3,
            Protocol::Smtp => &mut d.smtp,
            Protocol::Jmap => &mut d.jmap,
        };
        if slot.is_some() {
            continue;
        }
        let name = format!("{svc}._tcp.{domain}");
        let records = match resolver.lookup_srv(&name) {
            Ok(r) => r,
            Err(e @ (NetError::Timeout(_) | NetError::Connect(_) | NetError::Io(_))) => {
                notes.push(format!("{name}: {e}; DNS lookups stopped"));
                break;
            }
            Err(e) => {
                notes.push(format!("{name}: {e}"));
                continue;
            }
        };
        let Some(r) = first_usable(&records) else { continue };
        let verified = in_domain(&r.target, domain);
        if !verified {
            notes.push(format!("{name} points outside {domain} (to {}): confirm it before use", r.target));
        }
        let host = if protocol == Protocol::Jmap {
            if r.port == 443 {
                format!("https://{}/.well-known/jmap", r.target)
            } else {
                format!("https://{}:{}/.well-known/jmap", r.target, r.port)
            }
        } else {
            r.target.clone()
        };
        *slot = Some(ServerConfig {
            protocol,
            host,
            port: r.port,
            security,
            username: address.to_owned(),
            auth: Vec::new(),
            verified,
        });
    }
    d
}

/// Discovers the servers for `address`: autoconfig first, then DNS SRV for whatever is still missing. Only
/// an unusable address is an error; failed lookups are recorded in [`Discovery::notes`].
pub fn discover(address: &str, o: &DiscoverOptions) -> Result<Discovery> {
    let address = address.trim().to_owned();
    let domain = address_domain(&address)?;
    let mut d = Discovery { address: address.clone(), domain: domain.clone(), ..Default::default() };
    if is_reserved_domain(&domain) && !o.allow_reserved {
        d.notes.push(format!("{domain} is reserved for examples and tests; not looked up"));
        return Ok(d);
    }
    let mut notes = Vec::new();
    if let Some(c) = fetch_autoconfig(&address, &domain, o, &mut notes) {
        d.sources.push("autoconfig".into());
        d.display_name = c.display_name.clone();
        d.imap = c.incoming_of(Protocol::Imap).cloned();
        d.pop3 = c.incoming_of(Protocol::Pop3).cloned();
        d.smtp = c.outgoing.first().cloned();
    }
    let complete = (d.imap.is_some() || d.pop3.is_some()) && d.smtp.is_some();
    let resolver = match &o.resolver {
        ResolverChoice::System => Resolver::system(),
        ResolverChoice::Fixed(a) => Some(Resolver::new(*a)),
        ResolverChoice::Off => None,
    };
    match resolver {
        None => notes.push("no DNS resolver configured; SRV records not looked up".into()),
        Some(_) if complete => {}
        Some(mut r) => {
            r.timeout = o.dns_timeout;
            let srv = lookup_srv(&address, &domain, &r, &mut notes);
            let mut used = false;
            for (slot, found) in [
                (&mut d.imap, srv.imap),
                (&mut d.pop3, srv.pop3),
                (&mut d.smtp, srv.smtp),
                (&mut d.jmap, srv.jmap),
            ] {
                if slot.is_none() && found.is_some() {
                    *slot = found;
                    used = true;
                }
            }
            if used {
                d.sources.push("dns".into());
            }
        }
    }
    d.notes = notes;
    Ok(d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn domains() {
        for d in ["example.com", "mail.example.org", "x.test", "localhost", "a.invalid", "EXAMPLE.NET."] {
            assert!(is_reserved_domain(d), "{d}");
        }
        for d in ["example.co", "notexample.com", "fastmail.com"] {
            assert!(!is_reserved_domain(d), "{d}");
        }
        assert_eq!(address_domain(" kim@Mail.Example.COM ").unwrap(), "mail.example.com");
        for bad in ["", "kim", "@example.com", "kim@", "kim@bad domain", "kim@exämple.com", "kim@nodot"] {
            assert!(address_domain(bad).is_err(), "{bad}");
        }
        assert!(in_domain("imap.example.com", "example.com"));
        assert!(in_domain("Example.com.", "example.com"));
        assert!(!in_domain("example.com.evil.net", "example.com"));
        assert!(!in_domain("badexample.com", "example.com"));
        assert_eq!(
            expand_url(AUTOCONFIG_URLS[0], "example.com", "a+b@example.com"),
            "https://autoconfig.example.com/mail/config-v1.1.xml?emailaddress=a%2Bb%40example.com"
        );
    }

    #[test]
    fn reserved_domains_and_offline_options_do_no_lookups() {
        let d = discover("kim@example.com", &DiscoverOptions::default()).unwrap();
        assert!(!d.found_anything());
        assert!(d.notes[0].contains("reserved"));
        let o = DiscoverOptions { allow_reserved: true, ..DiscoverOptions::offline() };
        let d = discover("kim@example.com", &o).unwrap();
        assert!(!d.found_anything() && d.sources.is_empty());
        assert!(discover("not an address", &o).is_err());
    }

    #[test]
    fn plain_http_autoconfig_is_never_fetched() {
        let o = DiscoverOptions {
            allow_reserved: true,
            autoconfig_urls: vec!["http://127.0.0.1:9/{domain}".into()],
            ..DiscoverOptions::offline()
        };
        let d = discover("kim@example.com", &o).unwrap();
        assert!(d.notes.iter().any(|n| n.contains("only read over HTTPS")));
    }
}
