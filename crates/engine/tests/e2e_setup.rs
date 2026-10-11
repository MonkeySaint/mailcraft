//! End to end, account setup and download policy: server lookup for the Add Account wizard (autoconfig and
//! DNS SRV against local fixtures), OAuth 2.0 device sign-in with XOAUTH2 IMAP/SMTP and token refresh, JMAP
//! push over the event source (and polling without it), the "download mail from the past N months" window,
//! and On Slow Connections Download Only Headers. Against the local test servers only (`mailcraft-testmail`,
//! 127.0.0.1, synthetic users); every wait has a timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_engine::sync::{PushMode, cmds};
use mailcraft_net::ConnectOptions;
use mailcraft_net::discover::{DiscoverOptions, ResolverChoice};
use mailcraft_testmail::{Config, DateTime, OAUTH_TEST_CLIENT, Srv, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";
const WAIT: Duration = Duration::from_secs(30);

fn servers(cfg: Config) -> TestServers {
    TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        ..cfg
    })
    .unwrap()
}

fn message(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {BOB}\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         {body}\r\n",
        subject.replace(' ', "-")
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Outlook Data File"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn id_of(v: &Value) -> AccountId {
    AccountId(u32::try_from(v["account"].as_u64().unwrap()).unwrap())
}

fn inbox(s: &Session, a: AccountId) -> FolderId {
    s.mailbox.role_folder(a, FolderRole::Inbox).unwrap()
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.values().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn poll_until(s: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        s.poll_sync();
        if done(s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {:?}", s.sync_status());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn add_imap(s: &mut Session, srv: &TestServers, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "imap",
        "address": ALICE,
        "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
        "outgoing": {"host": "127.0.0.1", "port": srv.smtps_port, "security": "tls", "extraRootPem": srv.cert_pem},
        "password": ALICE_PW,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    id_of(&run(s, "accounts.add", p))
}

#[test]
fn discover_finds_settings_from_autoconfig_and_dns() {
    let srv = servers(Config::default());
    let mut s = session();
    // Reserved example domains are never looked up on the network, even without test settings.
    let r = run(&mut s, "accounts.discover", json!({"address": "kim@example.com"}));
    assert_eq!((r["status"].as_str(), r["found"].as_bool()), (Some("done"), Some(false)), "{r}");

    s.sync.discovery = DiscoverOptions {
        resolver: ResolverChoice::Fixed(srv.dns_addr()),
        dns_timeout: Duration::from_secs(2),
        autoconfig_urls: srv.autoconfig_url_templates(),
        http: ConnectOptions::trusting(srv.cert_der.clone()),
        allow_reserved: true,
    };
    srv.set_autoconfig(
        "example.com",
        Some(
            "<clientConfig version='1.1'><emailProvider id='example.com'><displayName>Example</displayName>\
             <incomingServer type='imap'><hostname>imap.example.com</hostname><port>993</port>\
             <socketType>SSL</socketType><username>%EMAILADDRESS%</username>\
             <authentication>OAuth2</authentication></incomingServer>\
             <outgoingServer type='smtp'><hostname>smtp.example.com</hostname><port>465</port>\
             <socketType>SSL</socketType><username>%EMAILADDRESS%</username></outgoingServer>\
             </emailProvider></clientConfig>",
        ),
    );
    // Background lookup: running first, the result on a later call.
    let r = run(&mut s, "accounts.discover", json!({"address": "kim@example.com", "refresh": true}));
    let mut r = r;
    let end = Instant::now() + WAIT;
    while r["status"] == "running" {
        assert!(Instant::now() < end, "lookup timed out");
        std::thread::sleep(Duration::from_millis(20));
        r = run(&mut s, "accounts.discover", json!({"address": "kim@example.com"}));
    }
    assert_eq!(r["found"], true, "{r}");
    assert_eq!(r["sources"], json!(["autoconfig"]));
    assert_eq!(r["imap"]["host"], "imap.example.com");
    assert_eq!(r["imap"]["security"], "tls");
    assert_eq!(r["imap"]["oauthOnly"], true);
    assert_eq!(r["smtp"]["port"], 465);

    srv.set_srv(
        "_submissions._tcp.example.org",
        vec![Srv { priority: 0, weight: 0, port: 465, target: "mail.example.org".into() }],
    );
    srv.set_srv(
        "_imap._tcp.example.org",
        vec![Srv { priority: 0, weight: 0, port: 143, target: "mail.example.org".into() }],
    );
    let r = run(&mut s, "accounts.discover", json!({"address": "kim@example.org", "wait": true}));
    assert_eq!(r["sources"], json!(["dns"]), "{r}");
    assert_eq!((r["imap"]["port"].as_u64(), r["imap"]["security"].as_str()), (Some(143), Some("starttls")));
    assert_eq!(r["smtp"]["security"], "tls");
    assert_eq!(r["imap"]["verified"], true);
    assert!(r["pop3"].is_null());

    assert!(s.run("accounts.discover", &json!({"address": "nonsense"})).is_err());
}

#[test]
fn oauth_device_sign_in_then_mail_over_xoauth2_with_refresh() {
    let srv = servers(Config::default());
    let mut s = session();
    s.sync.oauth_http = Some(ConnectOptions::trusting(srv.cert_der.clone()));
    // An account added without a password, signed in with OAuth.
    let a = id_of(&run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtps_port, "security": "tls", "extraRootPem": srv.cert_pem},
        }),
    ));
    let (_, token, device) = srv.oauth_endpoints();
    let r = run(
        &mut s,
        "accounts.oauthSignIn",
        json!({"account": a.0, "provider": "custom", "clientId": OAUTH_TEST_CLIENT, "flow": "device",
               "tokenEndpoint": token, "deviceAuthorizationEndpoint": device, "scopes": ["mail"]}),
    );
    assert_eq!(r["flow"], "device", "{r}");
    let mut code = String::new();
    poll_until(&mut s, "the device code", |s| {
        let st = s.run("accounts.oauthStatus", &json!({"account": a.0})).unwrap();
        code = st["userCode"].as_str().unwrap_or_default().to_owned();
        !code.is_empty()
    });
    // The user enters the code at the provider's page.
    srv.oauth_approve(&code, ALICE).unwrap();
    poll_until(&mut s, "sign-in", |s| {
        s.run("accounts.oauthStatus", &json!({"account": a.0})).unwrap()["status"] == "done"
    });
    let acct = run(&mut s, "accounts.list", json!({}))["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == a.0)
        .cloned()
        .unwrap();
    assert_eq!(acct["credential"], "oauthToken");

    // Receive and send with XOAUTH2.
    srv.seed_message(ALICE, "INBOX", &message("Token mail", "Signed in with a token."), &[]).unwrap();
    sync(&mut s);
    let ib = inbox(&s, a);
    assert!(find(&s, ib, "Token mail").is_some(), "{:?}", s.sync_status());
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Sent with a token"}));
    run(&mut s, "compose.send", json!({"compose": c}));
    sync(&mut s);
    assert!(srv.submissions().iter().any(|m| String::from_utf8_lossy(&m.data).contains("Sent with a token")));

    // The server forgets every access token: the next sign-in fails, is renewed with the refresh token, and
    // the run carries on.
    srv.oauth_expire_access_tokens();
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "After refresh"}));
    run(&mut s, "compose.send", json!({"compose": c}));
    srv.seed_message(ALICE, "INBOX", &message("Still arriving", "Still here."), &[]).unwrap();
    sync(&mut s);
    assert!(
        srv.submissions().iter().any(|m| String::from_utf8_lossy(&m.data).contains("After refresh")),
        "{:?}",
        s.sync_status()
    );
    assert!(find(&s, ib, "Still arriving").is_some(), "{:?}", s.sync_status());
    assert!(s.sync_status().accounts[0].error.is_none(), "{:?}", s.sync_status());
    assert_eq!(srv.oauth_grants(), ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]);
    // The renewed token was stored: a new session would sign in with it.
    let stored = mailcraft_engine::accounts::account_secret(&s, a, false).unwrap();
    assert!(stored.starts_with("at-"));
}

#[test]
fn jmap_push_over_the_event_source_and_polling_without_it() {
    let srv = servers(Config::default());
    let mut s = session();
    let a = id_of(&run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    ));
    sync(&mut s);
    poll_until(&mut s, "JMAP push to start", |s| s.push_state(a).0 == PushMode::Idle);
    // New mail arrives without a send/receive.
    srv.seed_message(ALICE, "INBOX", &message("Pushed", "Over the event source."), &[]).unwrap();
    let ib = inbox(&s, a);
    poll_until(&mut s, "the pushed message", |s| find(s, ib, "Pushed").is_some());
    // Turning push off stops it.
    run(&mut s, "accounts.options", json!({"account": a.0, "push": false}));
    poll_until(&mut s, "push to stop", |s| s.push_state(a).0 == PushMode::Off);

    // A server without an event source: the account polls instead.
    let srv2 = servers(Config { jmap_push: false, ..Config::default() });
    let mut s2 = session();
    let b = id_of(&run(
        &mut s2,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv2.jmap_url(), "extraRootPem": srv2.cert_pem},
               "password": ALICE_PW}),
    ));
    sync(&mut s2);
    poll_until(&mut s2, "polling", |s| s.push_state(b).0 == PushMode::Polling);
}

#[test]
fn first_download_takes_the_past_months_only() {
    let srv = servers(Config::default());
    let now = DateTime::now().unix;
    let old = DateTime::from_unix(now - 200 * 86_400);
    srv.seed_message_at(ALICE, "INBOX", &message("Old news", "From last winter."), &[], old).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Fresh", "From today."), &[]).unwrap();

    // IMAP with 3 months: the old message stays on the server.
    let mut s = session();
    let a = add_imap(&mut s, &srv, json!({"downloadMonths": 3}));
    assert_eq!(run(&mut s, "accounts.options", json!({"account": a.0}))["downloadMonths"], 3);
    sync(&mut s);
    let ib = inbox(&s, a);
    assert!(find(&s, ib, "Fresh").is_some());
    assert!(find(&s, ib, "Old news").is_none());

    // JMAP with the default (12 months) takes both; with 3 months only the fresh one.
    let mut s = session();
    let j = id_of(&run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW, "downloadMonths": 3}),
    ));
    sync(&mut s);
    let ib = inbox(&s, j);
    assert!(find(&s, ib, "Fresh").is_some());
    assert!(find(&s, ib, "Old news").is_none());
    let mut s = session();
    let j = id_of(&run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    ));
    assert_eq!(run(&mut s, "accounts.options", json!({"account": j.0}))["downloadMonths"], 12);
    sync(&mut s);
    let ib = inbox(&s, j);
    assert!(find(&s, ib, "Fresh").is_some() && find(&s, ib, "Old news").is_some());
}

#[test]
fn slow_connections_download_only_headers() {
    let srv = servers(Config::default());
    let mut s = session();
    // Every measured link counts as slow here.
    s.sync.slow_link_threshold = Some(u64::MAX);
    let a = add_imap(&mut s, &srv, json!({"slowHeadersOnly": true}));
    let big = "A long line of text for the transfer rate measurement.\r\n".repeat(1_200);
    srv.seed_message(ALICE, "INBOX", &message("Big", &big), &[]).unwrap();
    sync(&mut s);
    let ib = inbox(&s, a);
    let first = find(&s, ib, "Big").unwrap();
    assert!(!cmds::is_header_only(&s, first), "no rate known yet: downloaded in full");
    assert!(mailcraft_engine::sync::link_speed(&s, a).is_some(), "the run measured the link");
    assert!(s.sync_status().accounts[0].slow_connection);

    srv.seed_message(ALICE, "INBOX", &message("On a slow link", "Body comes later."), &[]).unwrap();
    sync(&mut s);
    let head = find(&s, ib, "On a slow link").unwrap();
    assert!(cmds::is_header_only(&s, head), "slow link: headers only");

    // The preference off: full items again.
    run(&mut s, "sendReceive.preferences", json!({"account": a.0, "slowHeadersOnly": false}));
    srv.seed_message(ALICE, "INBOX", &message("Fast again", "Whole message."), &[]).unwrap();
    sync(&mut s);
    let full = find(&s, ib, "Fast again").unwrap();
    assert!(!cmds::is_header_only(&s, full));
}
