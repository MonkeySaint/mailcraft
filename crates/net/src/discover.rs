//! Finding an account's servers from its address, before falling back to guessing:
//!
//! 0. **Consumer domains** of the big providers (`gmail.com`, `outlook.com`, `hotmail.co.uk` …): their public
//!    settings from [`crate::providers`], with no lookup at all.
//! 1. **Autoconfig**: the provider's `clientConfig` document over HTTPS on the mail domain
//!    (`https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=…`, then
//!    `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`), see [`crate::autoconfig`].
//! 2. **DNS SRV** (RFC 6186, with the implicit-TLS records of RFC 8314 section 5.1 and JMAP's RFC 8620
//!    section 2.2): `_imaps._tcp`, `_imap._tcp`, `_pop3s._tcp`, `_pop3._tcp`, `_submissions._tcp`,
//!    `_submission._tcp` and `_jmap._tcp` under the domain, asked of the system resolver.
//! 3. **DNS MX**: when the domain's preferred mail exchangers belong to a well-known provider (Microsoft 365,
//!    Outlook.com, Google Workspace; see [`crate::providers`]), that provider's servers. Only the domain is
//!    asked about, of the same resolver; the address is not sent anywhere.
//! 4. **Microsoft 365 tenant**: the tenant's public OpenID configuration, asked by domain only (never the
//!    address) at `login.microsoftonline.com`, when Microsoft 365 was recognised (for the tenant id, which
//!    single-tenant app registrations need for OAuth), or when nothing else found anything and the MX records
//!    were looked up and name a mail exchanger MailCraft does not know (a tenant whose mail passes through a
//!    filtering service first has an MX record of that service). Without an MX answer (no resolver, as on
//!    Windows; a failed lookup; no MX records) or with a null MX, the domain is not sent to Microsoft.
//!
//! 5. **Exchange Autodiscover** ([MS-OXDSCLI]), when nothing above found an incoming server: an unsigned
//!    request at the domain's Autodiscover locations ([`crate::ews::autodiscover`]). Exchange servers answer
//!    only signed-in requests, so this learns that an Exchange server answers ([`EwsFound::needs_sign_in`]);
//!    the account setup finishes the lookup with the password.
//!
//! Servers found through DNS SRV outside the address's domain are marked unverified (RFC 6186 section 6):
//! the account wizard shows them for confirmation like any other suggestion. A known provider's servers are
//! its own hosts, whatever pointed at them, and count as verified, except when the provider is only known from
//! the tenant: every domain verified in a Microsoft Entra tenant has one, wherever its mail is (an on-premises
//! Exchange server, another host), so those servers are a probable match to be confirmed (unverified).
//! Nothing here sends credentials.
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
use crate::http::{HttpClient, Url, percent_encode};
use crate::providers::{self, KnownProvider, ProviderId};
use crate::transport::ConnectOptions;

/// The autoconfig locations tried, in order. `{domain}` is the address's domain, `{address}` the
/// percent-encoded address.
pub const AUTOCONFIG_URLS: [&str; 2] = [
    "https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={address}",
    "https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml",
];

/// Where a Microsoft 365 tenant's public OpenID configuration is read; `{domain}` is the mail domain (only the
/// domain is sent, never the address).
pub const TENANT_URL: &str =
    "https://login.microsoftonline.com/{domain}/v2.0/.well-known/openid-configuration";

/// Largest OpenID configuration document read.
const MAX_OPENID_DOCUMENT: usize = 64 * 1024;

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
    /// HTTP options for the autoconfig and tenant requests.
    pub http: ConnectOptions,
    /// Microsoft 365 tenant lookup URL template (see [`TENANT_URL`]); `None` = no tenant lookup.
    pub tenant_url: Option<String>,
    /// Look up reserved test domains too (only for tests against local servers).
    pub allow_reserved: bool,
    /// Exchange Autodiscover location templates (see [`crate::ews::autodiscover::AUTODISCOVER_URLS`]);
    /// empty = no Autodiscover.
    pub autodiscover_urls: Vec<String>,
    /// The plain-HTTP Autodiscover redirect probe (see [`crate::ews::autodiscover::REDIRECT_PROBE_URL`]).
    pub autodiscover_probe: Option<String>,
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
            tenant_url: Some(TENANT_URL.to_owned()),
            allow_reserved: false,
            autodiscover_urls: crate::ews::autodiscover::AUTODISCOVER_URLS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            autodiscover_probe: Some(crate::ews::autodiscover::REDIRECT_PROBE_URL.to_owned()),
        }
    }
}

impl DiscoverOptions {
    /// No network lookups at all (discovery always finds nothing).
    pub fn offline() -> DiscoverOptions {
        DiscoverOptions {
            resolver: ResolverChoice::Off,
            autoconfig_urls: Vec::new(),
            tenant_url: None,
            autodiscover_urls: Vec::new(),
            autodiscover_probe: None,
            ..Default::default()
        }
    }

    /// The Autodiscover settings of these options (with `resolver` for the SRV record).
    pub fn autodiscover(&self) -> crate::ews::autodiscover::AutodiscoverOptions {
        crate::ews::autodiscover::AutodiscoverOptions {
            urls: self.autodiscover_urls.clone(),
            redirect_probe: self.autodiscover_probe.clone(),
            resolver: match &self.resolver {
                ResolverChoice::System => Resolver::system(),
                ResolverChoice::Fixed(a) => Some(Resolver::new(*a)),
                ResolverChoice::Off => None,
            },
            http: self.http.clone(),
            confirmed_hosts: Vec::new(),
        }
    }

    /// True when discovery makes no network request at all with these options.
    pub fn is_offline(&self) -> bool {
        self.resolver == ResolverChoice::Off
            && self.autoconfig_urls.is_empty()
            && self.tenant_url.is_none()
            && self.autodiscover_urls.is_empty()
            && self.autodiscover_probe.is_none()
    }
}

/// A well-known provider recognised for the address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderMatch {
    pub provider: &'static KnownProvider,
    /// How it was recognised: `domain` (a consumer domain), `mx` (the domain's MX records), `servers` (the
    /// servers another source found) or `tenant` (the domain has a Microsoft 365 tenant but its MX records
    /// name another exchanger: a probable match, its servers unverified).
    pub by: &'static str,
}

/// An Exchange server that answered Autodiscover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EwsFound {
    /// The EWS URL, when the answer named one (Autodiscover answered without a sign-in).
    pub url: Option<String>,
    /// The Autodiscover location that answered.
    pub autodiscover: String,
    /// The server asked for a sign-in: the account setup looks again with the password.
    pub needs_sign_in: bool,
    /// The location is in the address's domain.
    pub verified: bool,
}

/// What discovery found. Each protocol has at most one server (the preferred one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub address: String,
    pub domain: String,
    /// Where the settings came from, in order: "provider" (a consumer domain), "autoconfig", "dns" (SRV),
    /// "mx", "tenant".
    pub sources: Vec<String>,
    pub display_name: String,
    pub imap: Option<ServerConfig>,
    pub pop3: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    /// JMAP session URL server (`host` is the URL).
    pub jmap: Option<ServerConfig>,
    /// An Exchange server (EWS) found by Autodiscover.
    pub ews: Option<EwsFound>,
    /// The well-known provider serving the address, if recognised.
    pub provider: Option<ProviderMatch>,
    /// The domain's mail exchangers (MX), most preferred first, when they were looked up.
    pub mx: Vec<String>,
    /// The Microsoft 365 tenant id (a GUID) of the domain, when it was looked up and found.
    pub tenant: Option<String>,
    /// What was tried and why it did not help (for diagnostics).
    pub notes: Vec<String>,
}

impl Discovery {
    pub fn found_anything(&self) -> bool {
        self.imap.is_some()
            || self.pop3.is_some()
            || self.smtp.is_some()
            || self.jmap.is_some()
            || self.ews.is_some()
    }

    /// An incoming server and an outgoing one are known.
    fn complete(&self) -> bool {
        (self.imap.is_some() || self.pop3.is_some()) && self.smtp.is_some()
    }

    /// Fills the servers still missing from a known provider's; true when it filled any.
    fn fill_from(&mut self, p: &KnownProvider) -> bool {
        // A known provider's servers are not mixed into settings that already name another incoming server.
        // Only a missing outgoing server is added, and only to the provider's own incoming servers.
        if self.imap.is_some() || self.pop3.is_some() || self.jmap.is_some() {
            let own = [&self.imap, &self.pop3].into_iter().flatten().any(|s| {
                s.host.eq_ignore_ascii_case(p.imap.host) || s.host.eq_ignore_ascii_case(p.pop3.host)
            });
            if self.smtp.is_none() && own {
                self.smtp = Some(p.smtp_for(&self.address));
                return true;
            }
            return false;
        }
        self.imap = Some(p.imap_for(&self.address));
        self.pop3 = Some(p.pop3_for(&self.address));
        if self.smtp.is_none() {
            self.smtp = Some(p.smtp_for(&self.address));
        }
        if self.display_name.is_empty() {
            self.display_name = p.name.to_owned();
        }
        true
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
    srv_records(address, domain, resolver, notes).0
}

/// [`lookup_srv`], and whether the resolver kept answering.
fn srv_records(
    address: &str,
    domain: &str,
    resolver: &Resolver,
    notes: &mut Vec<String>,
) -> (Discovery, bool) {
    let mut d = Discovery::default();
    let mut answering = true;
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
                answering = false;
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
    (d, answering)
}

/// What the MX lookup showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MxOutcome {
    /// Not looked up, the lookup failed, or the domain has no MX records.
    NoAnswer,
    /// A null MX (RFC 7505): the domain accepts no mail.
    NullMx,
    /// Mail exchangers of no provider MailCraft knows.
    Unknown,
    /// A known provider's mail exchangers.
    Known,
}

/// The domain's MX records: the provider they belong to fills the servers still missing.
fn mx_step(d: &mut Discovery, resolver: &Resolver, notes: &mut Vec<String>) -> MxOutcome {
    let domain = d.domain.clone();
    let records = match resolver.lookup_mx(&domain) {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("MX {domain}: {e}"));
            return MxOutcome::NoAnswer;
        }
    };
    d.mx = records.iter().filter(|r| !r.is_null()).take(16).map(|r| r.exchange.clone()).collect();
    if d.mx.is_empty() && records.iter().any(crate::dns::MxRecord::is_null) {
        notes.push(format!("{domain} accepts no mail (null MX record)"));
        return MxOutcome::NullMx;
    }
    let Some(p) = providers::by_mx_records(&records) else {
        let Some(first) = d.mx.first() else { return MxOutcome::NoAnswer };
        notes.push(format!("MX {domain}: {first} is not a provider MailCraft knows"));
        return MxOutcome::Unknown;
    };
    if d.fill_from(p) {
        d.sources.push("mx".into());
        d.provider = Some(ProviderMatch { provider: p, by: "mx" });
        notes.push(format!("the MX records of {domain} point at {}", p.name));
    } else {
        notes.push(format!(
            "the MX records of {domain} point at {}; the servers found first are kept",
            p.name
        ));
    }
    MxOutcome::Known
}

/// True for a GUID in its usual 8-4-4-4-12 hex form.
pub fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes()
            .enumerate()
            .all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// Reads the tenant id from an OpenID configuration fetched from `url`: the first path segment of its
/// `issuer`, which must be a GUID on the same https host.
pub fn parse_tenant(body: &[u8], url: &str) -> Result<String> {
    if body.len() > MAX_OPENID_DOCUMENT {
        return Err(NetError::TooLarge("OpenID configuration".into()));
    }
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| NetError::Protocol(format!("OpenID configuration: {e}")))?;
    let issuer = v
        .get("issuer")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| NetError::Protocol("OpenID configuration without an issuer".into()))?;
    let iu = Url::parse(issuer)?;
    let lu = Url::parse(url)?;
    if !iu.https || !iu.host.eq_ignore_ascii_case(&lu.host) {
        return Err(NetError::Protocol(format!("unexpected issuer `{issuer}`")));
    }
    let tenant = iu.path.trim_start_matches('/').split('/').next().unwrap_or_default();
    if !is_guid(tenant) {
        return Err(NetError::Protocol(format!("the issuer `{issuer}` names no tenant id")));
    }
    Ok(tenant.to_ascii_lowercase())
}

/// The Microsoft 365 tenant id of `domain` from its public OpenID configuration ([`DiscoverOptions::tenant_url`];
/// only the domain is sent). `None` when the domain belongs to no tenant or the lookup fails (noted).
pub fn fetch_tenant(domain: &str, o: &DiscoverOptions, notes: &mut Vec<String>) -> Option<String> {
    let template = o.tenant_url.as_deref()?;
    let url = template.replace("{domain}", domain);
    if !url.to_ascii_lowercase().starts_with("https://") {
        notes.push(format!("{url}: skipped (the tenant is only looked up over HTTPS)"));
        return None;
    }
    let mut http = HttpClient::new(o.http.clone());
    http.max_body = MAX_OPENID_DOCUMENT;
    match http.get(&url, &[("Accept", "application/json")]) {
        Ok(r) if r.is_success() => match parse_tenant(&r.body, &url) {
            Ok(t) => Some(t),
            Err(e) => {
                notes.push(format!("{url}: {e}"));
                None
            }
        },
        Ok(r) => {
            notes.push(format!("{url}: HTTP {} ({domain} is not a Microsoft 365 domain)", r.status));
            None
        }
        Err(e) => {
            notes.push(format!("{url}: {e}"));
            None
        }
    }
}

/// Discovers the servers for `address`: a consumer domain's provider without any lookup; otherwise autoconfig
/// first, then DNS SRV, then DNS MX for whatever is still missing, and the Microsoft 365 tenant (see the
/// module documentation). Only an unusable address is an error; failed lookups are recorded in
/// [`Discovery::notes`].
pub fn discover(address: &str, o: &DiscoverOptions) -> Result<Discovery> {
    let address = address.trim().to_owned();
    let domain = address_domain(&address)?;
    let mut d = Discovery { address: address.clone(), domain: domain.clone(), ..Default::default() };
    if is_reserved_domain(&domain) && !o.allow_reserved {
        d.notes.push(format!("{domain} is reserved for examples and tests; not looked up"));
        return Ok(d);
    }
    if let Some(p) = providers::by_domain(&domain) {
        d.fill_from(p);
        d.sources.push("provider".into());
        d.provider = Some(ProviderMatch { provider: p, by: "domain" });
        d.notes.push(format!("{domain} is served by {}; nothing was looked up", p.name));
        return Ok(d);
    }
    let mut notes = Vec::new();
    let mut mx = MxOutcome::NoAnswer;
    if let Some(c) = fetch_autoconfig(&address, &domain, o, &mut notes) {
        d.sources.push("autoconfig".into());
        d.display_name = c.display_name.clone();
        d.imap = c.incoming_of(Protocol::Imap).cloned();
        d.pop3 = c.incoming_of(Protocol::Pop3).cloned();
        d.smtp = c.outgoing.first().cloned();
    }
    let resolver = match &o.resolver {
        ResolverChoice::System => Resolver::system(),
        ResolverChoice::Fixed(a) => Some(Resolver::new(*a)),
        ResolverChoice::Off => None,
    };
    match resolver {
        None => notes.push("no DNS resolver configured; SRV and MX records not looked up".into()),
        Some(_) if d.complete() => {}
        Some(mut r) => {
            r.timeout = o.dns_timeout;
            let (srv, answering) = srv_records(&address, &domain, &r, &mut notes);
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
            if answering && !d.complete() {
                mx = mx_step(&mut d, &r, &mut notes);
            }
        }
    }
    if d.provider.is_none()
        && let Some(p) =
            [&d.imap, &d.pop3, &d.smtp].into_iter().flatten().find_map(|s| providers::by_host(&s.host))
    {
        d.provider = Some(ProviderMatch { provider: p, by: "servers" });
    }
    // Exchange Autodiscover, when no incoming server is known yet and no well-known provider serves the domain.
    if d.provider.is_none()
        && d.imap.is_none()
        && d.pop3.is_none()
        && d.jmap.is_none()
        && !o.autodiscover_urls.is_empty()
    {
        match crate::ews::autodiscover::autodiscover(&address, None, &o.autodiscover()) {
            Ok(crate::ews::autodiscover::Probe::Found(f)) => {
                // Both the Autodiscover location and the EWS server it names must be in the domain.
                let verified = [&f.from, &f.ews_url]
                    .iter()
                    .all(|x| Url::parse(x).is_ok_and(|u| in_domain(&u.host, &domain)));
                d.ews = Some(EwsFound {
                    url: Some(f.ews_url),
                    autodiscover: f.from,
                    needs_sign_in: false,
                    verified,
                });
                d.sources.push("autodiscover".into());
            }
            Ok(crate::ews::autodiscover::Probe::NeedsSignIn { url }) => {
                let verified = Url::parse(&url).is_ok_and(|u| in_domain(&u.host, &domain));
                d.ews = Some(EwsFound { url: None, autodiscover: url, needs_sign_in: true, verified });
                d.sources.push("autodiscover".into());
            }
            // Only a signed-in lookup ends so.
            Ok(crate::ews::autodiscover::Probe::Unconfirmed { url, .. }) => {
                notes.push(format!("autodiscover: {url}"))
            }
            Ok(crate::ews::autodiscover::Probe::Nothing { notes: n }) => {
                notes.extend(n.into_iter().take(6));
            }
            Err(e) => notes.push(format!("autodiscover: {e}")),
        }
    }
    let microsoft = d.provider.is_some_and(|m| m.provider.id == ProviderId::Microsoft365);
    // Only the domain of a known Microsoft 365 organization, or one whose mail goes to an exchanger nobody
    // recognised (possibly a filtering service in front of Microsoft 365), is sent to Microsoft.
    let fallback = d.provider.is_none() && !d.found_anything() && mx == MxOutcome::Unknown;
    if (microsoft || fallback)
        && let Some(t) = fetch_tenant(&domain, o, &mut notes)
    {
        d.tenant = Some(t);
        if fallback && d.fill_from(&providers::MICROSOFT_365) {
            // Every domain verified in a Microsoft Entra tenant has one, wherever its mail is: a probable
            // match only, to be confirmed.
            for s in [&mut d.imap, &mut d.pop3, &mut d.smtp].into_iter().flatten() {
                s.verified = false;
            }
            d.sources.push("tenant".into());
            d.provider = Some(ProviderMatch { provider: &providers::MICROSOFT_365, by: "tenant" });
            notes.push(format!(
                "{domain} has a Microsoft 365 tenant; its mail may be elsewhere (confirm these servers)"
            ));
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
    fn consumer_domains_are_known_without_lookups() {
        let d = discover("kim@Outlook.com", &DiscoverOptions::offline()).unwrap();
        assert_eq!(d.sources, ["provider"]);
        let m = d.provider.unwrap();
        assert_eq!((m.provider.key, m.by), ("outlook.com", "domain"));
        assert_eq!(d.imap.as_ref().unwrap().host, "outlook.office365.com");
        assert_eq!(d.smtp.as_ref().unwrap().host, "smtp-mail.outlook.com");
        assert_eq!(d.pop3.as_ref().unwrap().port, 995);
        assert!(d.imap.unwrap().oauth_only());
        let d = discover("kim@gmail.com", &DiscoverOptions::offline()).unwrap();
        assert_eq!(d.provider.unwrap().provider.key, "google");
        assert_eq!(d.display_name, "Google");
        assert!(DiscoverOptions::offline().is_offline() && !DiscoverOptions::default().is_offline());
    }

    #[test]
    fn tenant_ids_come_from_the_issuer_on_the_same_host() {
        let url = "https://login.example.net/example.com/v2.0/.well-known/openid-configuration";
        let doc = |issuer: &str| format!(r#"{{"issuer": "{issuer}", "token_endpoint": "x"}}"#);
        let guid = "0F0E0D0C-1111-2222-3333-444455556666";
        let t = parse_tenant(doc(&format!("https://login.example.net/{guid}/v2.0")).as_bytes(), url).unwrap();
        assert_eq!(t, guid.to_ascii_lowercase());
        for bad in [
            format!("https://elsewhere.example.org/{guid}/v2.0"),
            format!("http://login.example.net/{guid}/v2.0"),
            "https://login.example.net/{tenantid}/v2.0".to_owned(),
            "https://login.example.net/".to_owned(),
            "not a url".to_owned(),
        ] {
            assert!(parse_tenant(doc(&bad).as_bytes(), url).is_err(), "{bad}");
        }
        assert!(parse_tenant(b"{}", url).is_err());
        assert!(parse_tenant(b"\xff junk", url).is_err());
        assert!(parse_tenant(&vec![b' '; MAX_OPENID_DOCUMENT + 1], url).is_err());
        assert!(is_guid("00000000-0000-0000-0000-000000000000"));
        assert!(
            !is_guid("00000000-0000-0000-0000-00000000000g")
                && !is_guid("0000000000000000000000000000000000-0")
        );
        let mut notes = Vec::new();
        let o = DiscoverOptions {
            tenant_url: Some("http://127.0.0.1:9/{domain}".into()),
            ..DiscoverOptions::offline()
        };
        assert!(fetch_tenant("example.com", &o, &mut notes).is_none());
        assert!(notes[0].contains("only looked up over HTTPS"));
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
