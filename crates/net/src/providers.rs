//! Well-known mail providers whose IMAP, POP3 and SMTP settings are public and the same for every mailbox:
//! Microsoft 365 (Exchange Online), Outlook.com (Microsoft's consumer mail) and Google (Gmail and Google
//! Workspace). Their own documentation lists the servers; none of them publishes an autoconfig document or
//! SRV records for the domains it hosts, so without this table account setup would guess `imap.<domain>`.
//!
//! A provider is recognised
//! - from the address's domain for the big consumer domains ([`by_domain`]; no lookup at all),
//! - from the domain's MX records ([`by_mx`]; a DNS question about the domain, the address is not sent
//!   anywhere): `*.mail.protection.outlook.com` and `*.mx.microsoft` are Microsoft 365,
//!   `*.olc.protection.outlook.com` is Outlook.com, `*.google.com` / `*.googlemail.com` (for example
//!   `aspmx.l.google.com`, `smtp.google.com`) are Google,
//! - from server host names another source found ([`by_host`]),
//! - from the Microsoft 365 tenant's public OpenID configuration, looked up by domain
//!   ([`crate::discover::fetch_tenant`]).
//!
//! The servers of a known provider are used as found (they are the provider's own hosts, whatever pointed at
//! them), so they are marked verified. Each provider names the OAuth provider of `data/oauth-providers.json`
//! it signs in with and whether passwords still work.

use mailcraft_model::Security;

use crate::autoconfig::{Protocol, ServerConfig};
use crate::discover::in_domain;

/// Which well-known provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// Microsoft 365 / Exchange Online (work or school accounts on the organization's own domain).
    Microsoft365,
    /// Outlook.com, Hotmail, Live and MSN addresses (Microsoft personal accounts).
    OutlookCom,
    /// Gmail and Google Workspace.
    Google,
}

/// One server of a known provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownServer {
    pub host: &'static str,
    pub port: u16,
    pub security: Security,
}

/// A well-known provider's public settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownProvider {
    pub id: ProviderId,
    /// Stable key: `microsoft365`, `outlook.com`, `google`.
    pub key: &'static str,
    pub name: &'static str,
    /// The OAuth provider (`id` in `data/oauth-providers.json`) the accounts sign in with.
    pub oauth_provider: &'static str,
    /// Passwords are refused for IMAP, POP3 and SMTP (basic authentication is off): OAuth only.
    pub oauth_only: bool,
    /// Microsoft Graph can also reach these mailboxes. Hook for a Graph account kind (graph-mail, issue #4):
    /// when one exists, account setup offers it for these providers.
    pub graph: bool,
    pub imap: KnownServer,
    pub pop3: KnownServer,
    pub smtp: KnownServer,
    /// Hints for the settings page (what the mailbox's administrator or owner may have to switch on).
    pub notes: &'static [&'static str],
}

const fn server(host: &'static str, port: u16, security: Security) -> KnownServer {
    KnownServer { host, port, security }
}

/// Microsoft 365 (public documentation: "POP, IMAP, and SMTP settings" for Microsoft 365 / Exchange Online).
pub const MICROSOFT_365: KnownProvider = KnownProvider {
    id: ProviderId::Microsoft365,
    key: "microsoft365",
    name: "Microsoft 365",
    oauth_provider: "microsoft",
    oauth_only: true,
    graph: true,
    imap: server("outlook.office365.com", 993, Security::Tls),
    pop3: server("outlook.office365.com", 995, Security::Tls),
    smtp: server("smtp.office365.com", 587, Security::StartTls),
    notes: &[
        "Microsoft 365 does not accept passwords for IMAP, POP and SMTP (basic authentication is off): sign in \
         with OAuth 2.0.",
        "Your organization may need to turn on IMAP (or POP) and Authenticated SMTP for your mailbox.",
        "An administrator of your organization may need to consent to the permissions of the app you \
         registered (IMAP.AccessAsUser.All, POP.AccessAsUser.All, SMTP.Send, offline_access).",
    ],
};

/// Outlook.com (public documentation: "POP, IMAP, and SMTP settings for Outlook.com").
pub const OUTLOOK_COM: KnownProvider = KnownProvider {
    id: ProviderId::OutlookCom,
    key: "outlook.com",
    name: "Outlook.com",
    oauth_provider: "microsoft",
    oauth_only: true,
    graph: true,
    imap: server("outlook.office365.com", 993, Security::Tls),
    pop3: server("outlook.office365.com", 995, Security::Tls),
    smtp: server("smtp-mail.outlook.com", 587, Security::StartTls),
    notes: &[
        "Outlook.com does not accept passwords for IMAP, POP and SMTP: sign in with OAuth 2.0.",
        "POP must be turned on in the Outlook.com settings (Mail, Forwarding and IMAP) before it can be used.",
    ],
};

/// Google (public documentation: "Read Gmail messages on other email clients using IMAP").
pub const GOOGLE: KnownProvider = KnownProvider {
    id: ProviderId::Google,
    key: "google",
    name: "Google",
    oauth_provider: "google",
    oauth_only: false,
    graph: false,
    imap: server("imap.gmail.com", 993, Security::Tls),
    pop3: server("pop.gmail.com", 995, Security::Tls),
    smtp: server("smtp.gmail.com", 465, Security::Tls),
    notes: &[
        "Google accounts sign in with OAuth 2.0, or with an app password when 2-Step Verification is on; the \
         account password itself is refused.",
        "Google Workspace: your administrator may need to allow IMAP or POP access for your organization.",
    ],
};

/// Every known provider.
pub const ALL: [&KnownProvider; 3] = [&MICROSOFT_365, &OUTLOOK_COM, &GOOGLE];

/// Microsoft's consumer mail domains (Outlook.com). Others are recognised by their MX.
const OUTLOOK_DOMAINS: &[&str] = &[
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "passport.com",
    "windowslive.com",
    "outlook.fr",
    "outlook.de",
    "outlook.es",
    "outlook.it",
    "outlook.jp",
    "outlook.com.au",
    "outlook.com.br",
    "hotmail.co.uk",
    "hotmail.fr",
    "hotmail.de",
    "hotmail.it",
    "hotmail.es",
    "hotmail.be",
    "hotmail.nl",
    "hotmail.se",
    "hotmail.co.jp",
    "hotmail.com.ar",
    "hotmail.com.au",
    "hotmail.com.br",
    "live.ca",
    "live.co.uk",
    "live.fr",
    "live.de",
    "live.it",
    "live.nl",
    "live.be",
    "live.se",
    "live.no",
    "live.dk",
    "live.at",
    "live.ie",
    "live.com.au",
    "live.com.mx",
    "live.jp",
];

/// Google's consumer mail domains (Gmail).
const GMAIL_DOMAINS: &[&str] = &["gmail.com", "googlemail.com"];

fn clean(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The provider of a big consumer mail domain (`gmail.com`, `outlook.com`, `hotmail.co.uk` …): known without
/// any lookup.
pub fn by_domain(domain: &str) -> Option<&'static KnownProvider> {
    let d = clean(domain);
    if OUTLOOK_DOMAINS.contains(&d.as_str()) {
        Some(&OUTLOOK_COM)
    } else if GMAIL_DOMAINS.contains(&d.as_str()) {
        Some(&GOOGLE)
    } else {
        None
    }
}

/// The provider whose mail exchanger `exchange` is (an MX record's host).
pub fn by_mx(exchange: &str) -> Option<&'static KnownProvider> {
    let h = clean(exchange);
    let under = |zone: &str| h.len() > zone.len() + 1 && in_domain(&h, zone);
    if under("mail.protection.outlook.com") || under("mx.microsoft") {
        Some(&MICROSOFT_365)
    } else if under("olc.protection.outlook.com") {
        Some(&OUTLOOK_COM)
    } else if under("google.com") || under("googlemail.com") {
        Some(&GOOGLE)
    } else {
        None
    }
}

/// The provider of the domain's preferred mail exchangers (`records` sorted by preference; the ones with the
/// lowest preference decide). `None` when they belong to no known provider.
pub fn by_mx_records(records: &[crate::dns::MxRecord]) -> Option<&'static KnownProvider> {
    let best = records.iter().filter(|r| !r.is_null()).map(|r| r.preference).min()?;
    records.iter().filter(|r| r.preference == best && !r.is_null()).find_map(|r| by_mx(&r.exchange))
}

/// The provider running a server host (`outlook.office365.com` is shared by Microsoft 365 and Outlook.com and
/// answers as Microsoft 365).
pub fn by_host(host: &str) -> Option<&'static KnownProvider> {
    let h = clean(host);
    ALL.into_iter().find(|p| [p.imap.host, p.pop3.host, p.smtp.host].contains(&h.as_str()))
}

impl KnownProvider {
    fn config(&self, protocol: Protocol, s: KnownServer, address: &str) -> ServerConfig {
        let mut auth = vec!["OAuth2".to_owned()];
        if !self.oauth_only {
            auth.push("password-cleartext".to_owned());
        }
        ServerConfig {
            protocol,
            host: s.host.to_owned(),
            port: s.port,
            security: s.security,
            username: address.trim().to_owned(),
            auth,
            verified: true,
        }
    }

    /// The IMAP server for `address`.
    pub fn imap_for(&self, address: &str) -> ServerConfig {
        self.config(Protocol::Imap, self.imap, address)
    }

    /// The POP3 server for `address`.
    pub fn pop3_for(&self, address: &str) -> ServerConfig {
        self.config(Protocol::Pop3, self.pop3, address)
    }

    /// The SMTP submission server for `address`.
    pub fn smtp_for(&self, address: &str) -> ServerConfig {
        self.config(Protocol::Smtp, self.smtp, address)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::dns::MxRecord;

    #[test]
    fn consumer_domains_need_no_lookup() {
        for d in ["outlook.com", "Hotmail.com", "live.co.uk", "msn.com", "hotmail.fr."] {
            assert_eq!(by_domain(d).unwrap().id, ProviderId::OutlookCom, "{d}");
        }
        for d in ["gmail.com", "GOOGLEMAIL.COM"] {
            assert_eq!(by_domain(d).unwrap().id, ProviderId::Google, "{d}");
        }
        for d in ["example.com", "outlook.example.com", "mygmail.com", "live.example.net", ""] {
            assert!(by_domain(d).is_none(), "{d}");
        }
    }

    #[test]
    fn mail_exchangers() {
        for (mx, id) in [
            ("example-com.mail.protection.outlook.com", ProviderId::Microsoft365),
            ("EXAMPLE-ORG.mail.protection.outlook.com.", ProviderId::Microsoft365),
            ("example-net.o-v1.mx.microsoft", ProviderId::Microsoft365),
            ("hotmail-com.olc.protection.outlook.com", ProviderId::OutlookCom),
            ("aspmx.l.google.com", ProviderId::Google),
            ("alt1.aspmx.l.google.com", ProviderId::Google),
            ("smtp.google.com", ProviderId::Google),
            ("aspmx2.googlemail.com", ProviderId::Google),
        ] {
            assert_eq!(by_mx(mx).map(|p| p.id), Some(id), "{mx}");
        }
        for mx in [
            "mail.protection.outlook.com",
            "google.com",
            "mx.example.com",
            "google.com.example.net",
            "evil-google.com",
            "outlook.com",
            "mail.protection.outlook.com.example.org",
            "",
        ] {
            assert!(by_mx(mx).is_none(), "{mx}");
        }
        let mx = |p: u16, e: &str| MxRecord { preference: p, exchange: e.into() };
        // The preferred exchangers decide; a backup MX at a known provider does not.
        assert!(by_mx_records(&[mx(10, "mx.example.com"), mx(20, "aspmx.l.google.com")]).is_none());
        assert_eq!(
            by_mx_records(&[mx(20, "mx.example.com"), mx(10, "x.mail.protection.outlook.com")]).unwrap().id,
            ProviderId::Microsoft365
        );
        assert!(by_mx_records(&[mx(0, "")]).is_none(), "null MX");
        assert!(by_mx_records(&[]).is_none());
    }

    #[test]
    fn hosts_and_server_settings() {
        assert_eq!(by_host("Outlook.Office365.com").unwrap().id, ProviderId::Microsoft365);
        assert_eq!(by_host("smtp-mail.outlook.com").unwrap().id, ProviderId::OutlookCom);
        assert_eq!(by_host("imap.gmail.com").unwrap().id, ProviderId::Google);
        assert!(by_host("imap.example.com").is_none());
        let i = MICROSOFT_365.imap_for(" kim@example.com ");
        assert_eq!((i.host.as_str(), i.port, i.security), ("outlook.office365.com", 993, Security::Tls));
        assert_eq!(i.username, "kim@example.com");
        assert!(i.oauth_only() && i.verified);
        let s = MICROSOFT_365.smtp_for("kim@example.com");
        assert_eq!((s.host.as_str(), s.port, s.security), ("smtp.office365.com", 587, Security::StartTls));
        let p = MICROSOFT_365.pop3_for("kim@example.com");
        assert_eq!((p.port, p.protocol), (995, Protocol::Pop3));
        let g = GOOGLE.smtp_for("kim@example.com");
        assert_eq!((g.host.as_str(), g.port, g.security), ("smtp.gmail.com", 465, Security::Tls));
        assert!(g.offers_oauth() && !g.oauth_only());
        for p in ALL {
            assert!(crate::oauth::builtin_providers().iter().any(|o| o.id == p.oauth_provider), "{}", p.key);
            assert!(!p.notes.is_empty());
        }
    }
}
