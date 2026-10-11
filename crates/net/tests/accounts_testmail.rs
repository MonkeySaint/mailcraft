//! Account setup against the in-process servers from `mailcraft-testmail` (127.0.0.1 only): discovery from
//! autoconfig documents and DNS SRV records (fake responder), OAuth 2.0 device sign-in with XOAUTH2 /
//! OAUTHBEARER logins to IMAP and SMTP and token refresh, JMAP push, and the IMAP first-download window.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_net::autoconfig::Protocol;
use mailcraft_net::discover::{DiscoverOptions, ResolverChoice, discover};
use mailcraft_net::http::HttpClient;
use mailcraft_net::imap::ImapClient;
use mailcraft_net::jmap::{JmapAuth, JmapClient, StateChange};
use mailcraft_net::oauth::{self, OAuthProvider};
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings, months_before};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security};
use mailcraft_testmail::{Config, DateTime, OAUTH_TEST_CLIENT, Srv, TestServers};

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
        allow_reserved: true,
    }
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
