//! Account setup against the in-process servers from `mailcraft-testmail` (127.0.0.1 only): discovery from
//! autoconfig documents, DNS SRV and MX records (fake responder) and tenant OpenID configurations (standing in
//! for Microsoft's), OAuth 2.0 device sign-in with XOAUTH2 /
//! OAUTHBEARER logins to IMAP and SMTP and token refresh, JMAP push, and the IMAP first-download window.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_net::autoconfig::Protocol;
use mailcraft_net::discover::{DiscoverOptions, ResolverChoice, discover};
use mailcraft_net::http::HttpClient;
use mailcraft_net::imap::ImapClient;
use mailcraft_net::jmap::{JmapAuth, JmapClient, StateChange};
use mailcraft_net::oauth::{self, OAuthProvider};
use mailcraft_net::providers::ProviderId;
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings, months_before};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security};
use mailcraft_testmail::{Config, DateTime, Mx, OAUTH_TEST_CLIENT, Srv, TestServers};

const USER: &str = "alice@example.com";
const PASS: &str = "alice-pw";

fn servers(cfg: Config) -> TestServers {
    TestServers::start(Config { users: vec![(USER.into(), PASS.into())], add_trace_headers: false, ..cfg })
        .unwrap()
}

fn opts(s: &TestServers) -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(20), ..ConnectOptions::trusting(s.cert_der.clone()) }
}

fn discover_opts(s: &TestServers) -> DiscoverOptions {
    DiscoverOptions {
        resolver: ResolverChoice::Fixed(s.dns_addr()),
        dns_timeout: Duration::from_secs(2),
        autoconfig_urls: s.autoconfig_url_templates(),
        http: opts(s),
        tenant_url: Some(s.tenant_url_template()),
        allow_reserved: true,
    }
}

const TENANT: &str = "6f1a5c2e-0b7d-4c3a-9e8f-1d2c3b4a5e6f";

fn mx(preference: u16, exchange: &str) -> Mx {
    Mx { preference, exchange: exchange.into() }
}

fn tenant_requests(s: &TestServers) -> Vec<String> {
    s.web_requests().into_iter().filter(|r| r.contains("/tenant/")).collect()
}

#[test]
fn discovery_recognises_microsoft_365_google_and_consumer_domains() {
    let s = servers(Config::default());

    // 1. MX at Microsoft 365 (the owner's case: no autoconfig, no SRV; imap.<domain> does not exist).
    s.set_mx("m365.example.net", vec![mx(0, "m365-example-net.mail.protection.outlook.com")]);
    s.set_tenant("m365.example.net", Some(TENANT));
    let d = discover("kim@m365.example.net", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["mx"], "{:?}", d.notes);
    let m = d.provider.unwrap();
    assert_eq!((m.provider.id, m.by), (ProviderId::Microsoft365, "mx"));
    assert_eq!(d.mx, ["m365-example-net.mail.protection.outlook.com"]);
    assert_eq!(d.tenant.as_deref(), Some(TENANT));
    let imap = d.imap.unwrap();
    assert_eq!((imap.host.as_str(), imap.port, imap.security), ("outlook.office365.com", 993, Security::Tls));
    assert_eq!(imap.username, "kim@m365.example.net");
    assert!(imap.verified && imap.oauth_only());
    let pop = d.pop3.unwrap();
    assert_eq!((pop.host.as_str(), pop.port, pop.security), ("outlook.office365.com", 995, Security::Tls));
    let smtp = d.smtp.unwrap();
    assert_eq!(
        (smtp.host.as_str(), smtp.port, smtp.security),
        ("smtp.office365.com", 587, Security::StartTls)
    );
    // The tenant is asked about by domain only: the address never leaves for it.
    let t = tenant_requests(&s);
    assert_eq!(t, ["GET /tenant/m365.example.net/v2.0/.well-known/openid-configuration"]);

    // 2. Google Workspace (the most preferred MX decides); no tenant lookup for it.
    s.set_mx("workspace.example.org", vec![mx(10, "alt1.aspmx.l.google.com"), mx(1, "aspmx.l.google.com")]);
    let d = discover("lee@workspace.example.org", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["mx"]);
    assert_eq!(d.provider.unwrap().provider.id, ProviderId::Google);
    assert_eq!(d.mx, ["aspmx.l.google.com", "alt1.aspmx.l.google.com"]);
    assert_eq!(d.imap.unwrap().host, "imap.gmail.com");
    let smtp = d.smtp.unwrap();
    assert_eq!((smtp.host.as_str(), smtp.port, smtp.security), ("smtp.gmail.com", 465, Security::Tls));
    assert!(smtp.offers_oauth() && !smtp.oauth_only());
    assert!(d.tenant.is_none());
    assert_eq!(tenant_requests(&s).len(), 1);

    // 3. An unknown MX (a filtering service in front of Microsoft 365, or mail hosted elsewhere by an
    //    organization that also has a tenant): Microsoft 365 is only a probable match, its servers unverified.
    s.set_mx("filtered.example.com", vec![mx(5, "inbound.filter.example.net")]);
    s.set_tenant("filtered.example.com", Some(TENANT));
    let d = discover("kim@filtered.example.com", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["tenant"], "{:?}", d.notes);
    assert_eq!(d.provider.unwrap().by, "tenant");
    assert_eq!(d.tenant.as_deref(), Some(TENANT));
    let (imap, smtp) = (d.imap.unwrap(), d.smtp.unwrap());
    assert_eq!((imap.host.as_str(), smtp.host.as_str()), ("outlook.office365.com", "smtp.office365.com"));
    assert!(!imap.verified && !smtp.verified && !d.pop3.unwrap().verified, "to be confirmed");
    assert!(
        d.notes.iter().any(|n| n.contains("inbound.filter.example.net is not a provider")),
        "{:?}",
        d.notes
    );
    assert!(d.notes.iter().any(|n| n.contains("its mail may be elsewhere")), "{:?}", d.notes);

    // 4. Unknown MX and no tenant: nothing found (the wizard guesses), the reasons noted.
    s.set_mx("plain.example.com", vec![mx(10, "mx.plain.example.com")]);
    let d = discover("kim@plain.example.com", &discover_opts(&s)).unwrap();
    assert!(!d.found_anything() && d.provider.is_none());
    assert!(d.notes.iter().any(|n| n.contains("not a Microsoft 365 domain")), "{:?}", d.notes);

    // 5. SRV records come first: the MX is not needed then.
    s.set_srv(
        "_imaps._tcp.both.example.org",
        vec![Srv { priority: 0, weight: 0, port: 993, target: "imap.both.example.org".into() }],
    );
    s.set_srv(
        "_submissions._tcp.both.example.org",
        vec![Srv { priority: 0, weight: 0, port: 465, target: "smtp.both.example.org".into() }],
    );
    s.set_mx("both.example.org", vec![mx(0, "aspmx.l.google.com")]);
    let d = discover("kim@both.example.org", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["dns"]);
    assert!(d.provider.is_none() && d.mx.is_empty());
    assert_eq!(d.imap.unwrap().host, "imap.both.example.org");

    // 6. SRV for submission only, MX at Google: Google's incoming servers, the domain's own SMTP kept.
    s.set_srv(
        "_submission._tcp.half.example.org",
        vec![Srv { priority: 0, weight: 0, port: 587, target: "relay.half.example.org".into() }],
    );
    s.set_mx("half.example.org", vec![mx(0, "smtp.google.com")]);
    let d = discover("kim@half.example.org", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["dns", "mx"]);
    assert_eq!(d.imap.unwrap().host, "imap.gmail.com");
    assert_eq!(d.smtp.unwrap().host, "relay.half.example.org");

    // 7. A null MX: the domain takes no mail, even with a tenant (not asked about at all).
    s.set_mx("nomail.example.net", vec![mx(0, "")]);
    s.set_tenant("nomail.example.net", Some(TENANT));
    let before = tenant_requests(&s).len();
    let d = discover("kim@nomail.example.net", &discover_opts(&s)).unwrap();
    assert!(d.notes.iter().any(|n| n.contains("accepts no mail")), "{:?}", d.notes);
    assert!(!d.found_anything() && d.provider.is_none() && d.tenant.is_none());
    assert_eq!(tenant_requests(&s).len(), before);

    // 7b. No MX answer (no resolver, as on Windows): the domain is not sent to Microsoft either.
    s.set_tenant("nodns.example.org", Some(TENANT));
    let o = DiscoverOptions { resolver: ResolverChoice::Off, ..discover_opts(&s) };
    let d = discover("kim@nodns.example.org", &o).unwrap();
    assert!(!d.found_anything() && d.provider.is_none() && d.tenant.is_none());
    assert_eq!(tenant_requests(&s).len(), before);

    // 8. Consumer domains: their settings at once, no request of any kind.
    let before = s.web_requests().len();
    for (address, id, smtp_host) in [
        ("kim@outlook.com", ProviderId::OutlookCom, "smtp-mail.outlook.com"),
        ("kim@hotmail.co.uk", ProviderId::OutlookCom, "smtp-mail.outlook.com"),
        ("kim@gmail.com", ProviderId::Google, "smtp.gmail.com"),
    ] {
        let d = discover(address, &discover_opts(&s)).unwrap();
        assert_eq!(d.sources, ["provider"]);
        assert_eq!((d.provider.unwrap().provider.id, d.provider.unwrap().by), (id, "domain"));
        assert_eq!(d.smtp.unwrap().host, smtp_host);
        assert!(d.mx.is_empty());
    }
    assert_eq!(s.web_requests().len(), before);

    // 9. An autoconfig document naming Microsoft's servers: recognised from the hosts (and the tenant read).
    s.set_autoconfig(
        "ac.example.com",
        Some(
            "<clientConfig version='1.1'><emailProvider id='ac.example.com'>\
             <incomingServer type='imap'><hostname>outlook.office365.com</hostname><port>993</port>\
             <socketType>SSL</socketType><username>%EMAILADDRESS%</username>\
             <authentication>OAuth2</authentication></incomingServer>\
             <outgoingServer type='smtp'><hostname>smtp.office365.com</hostname><port>587</port>\
             <socketType>STARTTLS</socketType><username>%EMAILADDRESS%</username></outgoingServer>\
             </emailProvider></clientConfig>",
        ),
    );
    s.set_tenant("ac.example.com", Some(TENANT));
    let d = discover("kim@ac.example.com", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["autoconfig"]);
    assert_eq!(d.provider.unwrap().by, "servers");
    assert_eq!(d.tenant.as_deref(), Some(TENANT));
}

const AUTOCONFIG: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.com">
  <domain>example.com</domain><displayName>Example Mail</displayName>
  <incomingServer type="imap"><hostname>imap.example.com</hostname><port>993</port>
    <socketType>SSL</socketType><username>%EMAILADDRESS%</username>
    <authentication>password-cleartext</authentication></incomingServer>
  <outgoingServer type="smtp"><hostname>smtp.example.com</hostname><port>587</port>
    <socketType>STARTTLS</socketType><username>%EMAILLOCALPART%</username>
    <authentication>password-cleartext</authentication></outgoingServer>
</emailProvider></clientConfig>"#;

#[test]
fn discovery_prefers_autoconfig_then_dns_srv() {
    let s = servers(Config::default());
    // 1. Autoconfig on the domain wins (and DNS is not needed).
    s.set_autoconfig("example.com", Some(AUTOCONFIG));
    let d = discover("kim+mail@example.com", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["autoconfig"]);
    assert_eq!(d.display_name, "Example Mail");
    let imap = d.imap.unwrap();
    assert_eq!((imap.host.as_str(), imap.port, imap.security), ("imap.example.com", 993, Security::Tls));
    assert_eq!(imap.username, "kim+mail@example.com");
    let smtp = d.smtp.unwrap();
    assert_eq!((smtp.port, smtp.security, smtp.username.as_str()), (587, Security::StartTls, "kim+mail"));
    let reqs = s.web_requests();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert!(
        reqs[0]
            .contains("/autoconfig/example.com/mail/config-v1.1.xml?emailaddress=kim%2Bmail%40example.com")
    );

    // 2. The well-known location is tried next.
    s.set_autoconfig("example.com", None);
    s.set_autoconfig_well_known("example.org", &AUTOCONFIG.replace("example.com", "example.org"));
    let d = discover("kim@example.org", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["autoconfig"]);
    assert_eq!(d.imap.unwrap().host, "imap.example.org");
    assert!(s.web_requests().iter().any(|r| r.contains("/example.org/.well-known/autoconfig/")));

    // 3. No document: SRV records (implicit TLS preferred, "." = not offered, outside targets flagged).
    s.set_srv(
        "_imaps._tcp.example.net",
        vec![
            Srv { priority: 20, weight: 0, port: 993, target: "backup.example.net".into() },
            Srv { priority: 10, weight: 5, port: 993, target: "imap.example.net".into() },
        ],
    );
    s.set_srv(
        "_imap._tcp.example.net",
        vec![Srv { priority: 0, weight: 0, port: 143, target: "x.example.net".into() }],
    );
    s.set_srv(
        "_pop3s._tcp.example.net",
        vec![Srv { priority: 0, weight: 0, port: 0, target: String::new() }],
    );
    s.set_srv(
        "_submission._tcp.example.net",
        vec![Srv { priority: 0, weight: 0, port: 587, target: "smtp.provider.example".into() }],
    );
    s.set_srv(
        "_jmap._tcp.example.net",
        vec![Srv { priority: 0, weight: 0, port: 8443, target: "jmap.example.net".into() }],
    );
    let d = discover("kim@example.net", &discover_opts(&s)).unwrap();
    assert_eq!(d.sources, ["dns"], "{:?}", d.notes);
    let imap = d.imap.unwrap();
    assert_eq!((imap.host.as_str(), imap.port, imap.security), ("imap.example.net", 993, Security::Tls));
    assert!(imap.verified);
    assert!(d.pop3.is_none(), "a \".\" target means no POP3");
    let smtp = d.smtp.unwrap();
    assert_eq!((smtp.host.as_str(), smtp.security), ("smtp.provider.example", Security::StartTls));
    assert!(!smtp.verified, "outside the domain: to be confirmed");
    assert!(d.notes.iter().any(|n| n.contains("outside example.net")));
    let jmap = d.jmap.unwrap();
    assert_eq!(jmap.protocol, Protocol::Jmap);
    assert_eq!(jmap.host, "https://jmap.example.net:8443/.well-known/jmap");

    // 4. Nothing anywhere: nothing found, quickly.
    let started = Instant::now();
    let d = discover("kim@nothing.example", &discover_opts(&s)).unwrap();
    assert!(!d.found_anything());
    assert!(started.elapsed() < Duration::from_secs(10));

    // 5. A truncated answer needs TCP, which the fake responder lacks: DNS stops with a note.
    s.set_srv_truncated("_imaps._tcp.trunc.example");
    let d = discover("kim@trunc.example", &discover_opts(&s)).unwrap();
    assert!(d.imap.is_none());
    assert!(d.notes.iter().any(|n| n.contains("_imaps._tcp.trunc.example")), "{:?}", d.notes);
}

fn provider(s: &TestServers) -> OAuthProvider {
    let (auth, token, device) = s.oauth_endpoints();
    OAuthProvider {
        id: "custom".into(),
        name: "Test".into(),
        authorization_endpoint: auth,
        token_endpoint: token,
        device_authorization_endpoint: Some(device),
        client_id: OAUTH_TEST_CLIENT.into(),
        scopes: vec!["mail".into()],
        ..Default::default()
    }
}

#[test]
fn oauth_device_sign_in_xoauth2_logins_and_refresh() {
    let s = servers(Config::default());
    let http = HttpClient::new(opts(&s));
    let p = provider(&s);
    let da = oauth::start_device_flow(&http, &p).unwrap();
    assert_eq!(oauth::poll_device_token(&http, &p, &da.device_code).unwrap(), oauth::DevicePoll::Pending);
    assert_eq!(s.oauth_pending_codes(), std::slice::from_ref(&da.user_code));
    assert!(s.oauth_approve(&da.user_code, "nobody@example.com").is_err());
    s.oauth_approve(&da.user_code, USER).unwrap();
    let tokens = oauth::wait_for_device_token(&http, &p, &da, |_| true).unwrap();
    assert!(tokens.refresh_token.is_some() && tokens.expires_at.is_some());

    // XOAUTH2 (IMAP picks it first) and OAUTHBEARER, over both servers.
    let creds = Credentials::oauth2(USER, tokens.access_token.clone());
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&creds, "").unwrap();
    assert!(c.is_authenticated());
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&creds, "OAUTHBEARER").unwrap();
    let smtp = |creds: &Credentials, mech: &str| {
        let mut st = SmtpSettings::new("127.0.0.1", s.smtps_port, Security::Tls);
        st.options = opts(&s);
        st.credentials = Some(creds.clone());
        st.auth_mechanism = mech.into();
        SmtpClient::connect(&st)
    };
    assert!(smtp(&creds, "").unwrap().is_authenticated());
    assert!(smtp(&creds, "OAUTHBEARER").unwrap().is_authenticated());
    // A token for another user, and a password where a token is expected, are refused.
    let other = Credentials::oauth2("bob@example.org", tokens.access_token.clone());
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    assert!(matches!(c.login_with(&other, ""), Err(NetError::Auth(_))));

    // Expired tokens fail everywhere until refreshed.
    s.oauth_expire_access_tokens();
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    assert!(matches!(c.login_with(&creds, ""), Err(NetError::Auth(_))));
    assert!(matches!(smtp(&creds, ""), Err(NetError::Auth(_))));
    let fresh = oauth::refresh(&http, &p, tokens.refresh_token.as_deref().unwrap()).unwrap();
    assert_ne!(fresh.access_token, tokens.access_token);
    let creds = Credentials::oauth2(USER, fresh.access_token.clone());
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&creds, "").unwrap();
    assert!(smtp(&creds, "").unwrap().is_authenticated());
    // The old refresh token was rotated away.
    assert!(matches!(
        oauth::refresh(&http, &p, tokens.refresh_token.as_deref().unwrap()),
        Err(NetError::OAuth { .. })
    ));
    // JMAP takes the token as a Bearer token.
    let j = JmapClient::connect(
        &s.jmap_url(),
        JmapAuth::Bearer(fresh.access_token.clone()),
        HttpClient::new(opts(&s)),
    );
    assert!(j.is_ok());
    assert_eq!(s.oauth_grants(), ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]);
    // An unknown client is refused.
    let mut bad = p.clone();
    bad.client_id = "someone-else".into();
    assert!(matches!(oauth::start_device_flow(&http, &bad), Err(NetError::OAuth { .. })));
}

#[test]
fn oauth_browser_flow_with_pkce() {
    let s = servers(Config::default());
    let http = HttpClient::new(opts(&s));
    let mut p = provider(&s);
    // The "browser" talks to the authorization page over plain HTTP so it can follow the redirect to the
    // loopback listener; the token request stays on HTTPS.
    p.authorization_endpoint = format!("http://127.0.0.1:{}/oauth/authorize", s.web_port);
    let redirect = oauth::LoopbackRedirect::bind().unwrap();
    let pkce = oauth::Pkce::generate().unwrap();
    let state = oauth::random_state().unwrap();
    let url = oauth::authorization_url(&p, &redirect.redirect_uri(), &pkce, &state, Some(USER)).unwrap();
    let browser = std::thread::spawn(move || {
        let _ = HttpClient::default().get(&url, &[]);
    });
    let target = redirect.wait_for(Duration::from_secs(10), &state).unwrap();
    browser.join().unwrap();
    let code = oauth::parse_redirect(&target, &state).unwrap();
    let wrong = oauth::Pkce::generate().unwrap();
    assert!(oauth::exchange_code(&http, &p, &code, &redirect.redirect_uri(), &wrong).is_err());
    // The failed attempt used the code up.
    assert!(oauth::exchange_code(&http, &p, &code, &redirect.redirect_uri(), &pkce).is_err());

    let state = oauth::random_state().unwrap();
    let url = oauth::authorization_url(&p, &redirect.redirect_uri(), &pkce, &state, Some(USER)).unwrap();
    let browser = std::thread::spawn(move || {
        let _ = HttpClient::default().get(&url, &[]);
    });
    let target = redirect.wait_for(Duration::from_secs(10), &state).unwrap();
    browser.join().unwrap();
    let code = oauth::parse_redirect(&target, &state).unwrap();
    let t = oauth::exchange_code(&http, &p, &code, &redirect.redirect_uri(), &pkce).unwrap();
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&Credentials::oauth2(USER, t.access_token), "").unwrap();
}

#[test]
fn jmap_push_reports_new_mail() {
    let s = servers(Config::default());
    let c = JmapClient::connect(
        &s.jmap_url(),
        JmapAuth::Basic { user: USER.into(), password: PASS.into() },
        HttpClient::new(opts(&s)),
    )
    .unwrap();
    let mut events = c.open_events(&["Email", "Mailbox"], 300).unwrap();
    let first = loop_event(&mut events);
    let sc = StateChange::parse(&first).unwrap();
    let st0 = sc.state_of("A", "Email").unwrap().to_owned();
    s.seed_message(USER, "INBOX", b"Subject: pushed\r\n\r\nhi\r\n", &[]).unwrap();
    let next = StateChange::parse(&loop_event(&mut events)).unwrap();
    assert_ne!(next.state_of("A", "Email").unwrap(), st0);

    // A server without push: no event source.
    let s2 = servers(Config { jmap_push: false, ..Config::default() });
    let c2 = JmapClient::connect(
        &s2.jmap_url(),
        JmapAuth::Basic { user: USER.into(), password: PASS.into() },
        HttpClient::new(opts(&s2)),
    )
    .unwrap();
    assert!(c2.event_source_url(&["Email"], 60).is_none());
    assert!(matches!(c2.open_events(&["Email"], 60), Err(NetError::Unsupported(_))));
}

fn loop_event(events: &mut mailcraft_net::eventsource::EventStream) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(e) = events.next_event(Duration::from_millis(200)).unwrap()
            && e.event == "state"
        {
            return e.data;
        }
    }
    panic!("no state event");
}

#[test]
fn imap_first_download_window() {
    let s = servers(Config::default());
    let now = DateTime::now().unix;
    let old = DateTime::from_unix(now - 400 * 86_400);
    s.seed_message_at(USER, "INBOX", b"Subject: old\r\n\r\nold\r\n", &[], old).unwrap();
    s.seed_message(USER, "INBOX", b"Subject: new\r\n\r\nnew\r\n", &[]).unwrap();
    let mut set =
        ImapSettings::new("127.0.0.1", s.imaps_port, Security::Tls, Credentials::password(USER, PASS));
    set.options = opts(&s);
    set.first_sync_since = Some(months_before(now, 12));
    let mut acc = ImapAccount::new(set.clone());
    let d = acc.sync_folder("INBOX", &FolderState::default()).unwrap();
    assert_eq!(d.new.len(), 1, "only the last 12 months");
    assert!(String::from_utf8_lossy(&d.new[0].raw).contains("Subject: new"));
    // Later syncs see new mail as usual.
    s.seed_message_at(USER, "INBOX", b"Subject: late\r\n\r\nlate\r\n", &[], old).unwrap();
    let d2 = acc.sync_folder("INBOX", &d.state).unwrap();
    assert_eq!(d2.new.len(), 1);
    // Without a window everything comes.
    set.first_sync_since = None;
    let d = ImapAccount::new(set).sync_folder("INBOX", &FolderState::default()).unwrap();
    assert_eq!(d.new.len(), 3);
}
