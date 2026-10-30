use super::mail::*;
use super::*;
use crate::http::testserver::{json as reply, serve};
use crate::transport::ConnectOptions;
use mailcraft_model::FolderRole;
use serde_json::json;

fn client(base: &str) -> GraphClient {
    let opts = ConnectOptions { allow_plaintext_auth: true, ..ConnectOptions::default() };
    GraphClient::new(&format!("{base}/v1.0"), "tok", HttpClient::new(opts)).unwrap()
}

#[test]
fn scopes_are_prefixed_and_deduplicated() {
    let s = scopes(&["Calendars.ReadWrite", "https://graph.microsoft.com/Mail.Send", "offline_access", " "]);
    assert_eq!(
        s,
        vec![
            "https://graph.microsoft.com/Mail.ReadWrite",
            "https://graph.microsoft.com/Mail.Send",
            "offline_access",
            "https://graph.microsoft.com/Calendars.ReadWrite",
        ]
    );
    assert!(is_graph_scope("https://graph.microsoft.com/Mail.Read"));
    assert!(!is_graph_scope("https://outlook.office.com/IMAP.AccessAsUser.All"));
}

#[test]
fn retry_waits_follow_the_server_within_bounds() {
    assert_eq!(retry_wait(Some("3"), 0), Duration::from_secs(3));
    assert_eq!(retry_wait(Some("99999999"), 0), MAX_RETRY_WAIT);
    assert_eq!(retry_wait(Some("Wed, 21 Oct 2026 07:28:00 GMT"), 2), Duration::from_secs(4));
    assert_eq!(retry_wait(None, 40), Duration::from_secs(64));
}

/// Regression: a cancel between pages returned the pages read so far as the whole collection (and a folder
/// tree cut short then deleted the folders it left out).
#[test]
fn a_cancelled_listing_is_an_error_and_a_capped_one_says_so() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let (base, t, _rx) = serve(1, move |req| {
        flag.store(true, Ordering::SeqCst);
        let next = format!(
            r#"{{"value": [{{"id": "a"}}], "@odata.nextLink": "http://{}/v1.0/me/mailFolders?skip=1"}}"#,
            req.header("host").unwrap()
        );
        reply(200, &next)
    });
    let c = client(&base).with_cancel(Arc::clone(&cancel));
    assert!(c.get_all("/me/mailFolders", 10).is_err(), "cancelled: no part passed off as all");
    t.join().unwrap();

    let (base, t, _rx) = serve(2, |_| reply(200, r#"{"value": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}"#));
    let c = client(&base);
    let (some, whole) = c.get_all_checked("/me/mailFolders", 2).unwrap();
    assert_eq!((some.len(), whole), (2, false));
    let (all, whole) = c.get_all_checked("/me/mailFolders", 10).unwrap();
    assert_eq!((all.len(), whole), (3, true));
    t.join().unwrap();
}

#[test]
fn paging_follows_next_links_and_sends_the_token() {
    let (base, t, rx) = serve(2, |req| {
        if req.path.contains("skip=1") {
            reply(200, r#"{"value": [{"id": "b"}]}"#)
        } else {
            let next = format!(
                r#"{{"value": [{{"id": "a"}}], "@odata.nextLink": "http://{}/v1.0/me/mailFolders?skip=1"}}"#,
                req.header("host").unwrap()
            );
            reply(200, &next)
        }
    });
    let c = client(&base);
    let all = c.get_all("/me/mailFolders", 10).unwrap();
    assert_eq!(all.len(), 2);
    let first = rx.recv().unwrap();
    assert_eq!(first.header("authorization"), Some("Bearer tok"));
    assert!(first.header("prefer").unwrap().contains("IdType=\"ImmutableId\""));
    t.join().unwrap();
}

#[test]
fn a_link_to_another_server_is_never_followed() {
    let (base, t, rx) = serve(1, |_| {
        reply(200, r#"{"value": [{"id": "a"}], "@odata.nextLink": "https://attacker.example.net/steal"}"#)
    });
    let c = client(&base);
    let err = c.get_all("/me/mailFolders", 10).unwrap_err();
    assert!(matches!(err, NetError::Protocol(_)), "{err:?}");
    assert_eq!(rx.iter().count(), 1, "nothing was sent to the other server");
    t.join().unwrap();
    assert!(c.url("http://127.0.0.1:1/v1.0/me").is_err(), "another port is another origin");
    assert!(c.url("me").is_err());
}

#[test]
fn throttling_is_waited_out_then_retried() {
    let (base, t, rx) = serve(3, |req| {
        if req.path.ends_with("again=2") || req.method == "PATCH" {
            reply(200, r#"{"id": "x"}"#)
        } else {
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    });
    let mut c = client(&base);
    c.max_retries = 1;
    let err = c.get_json("/me/messages?again=1").unwrap_err();
    assert!(matches!(err, NetError::Http { status: 429, .. }), "{err:?}");
    assert_eq!(c.throttled(), 2);
    assert_eq!(c.get_json("/me/messages?again=2").unwrap()["id"], "x");
    assert_eq!(rx.iter().count(), 3);
    t.join().unwrap();
}

#[test]
fn a_cancelled_client_stops_waiting() {
    let (base, t, _rx) =
        serve(1, |_| b"HTTP/1.1 503 Busy\r\nRetry-After: 100\r\nContent-Length: 0\r\n\r\n".to_vec());
    let cancel = Arc::new(AtomicBool::new(true));
    let c = client(&base).with_cancel(cancel);
    let started = Instant::now();
    assert!(matches!(c.get_json("/me"), Err(NetError::Timeout(_))));
    assert!(started.elapsed() < Duration::from_secs(5));
    t.join().unwrap();
}

#[test]
fn errors_carry_the_service_code() {
    let (base, t, _rx) = serve(3, |req| {
        if req.path.contains("gone") {
            reply(410, r#"{"error": {"code": "SyncStateNotFound", "message": "start over"}}"#)
        } else if req.path.contains("auth") {
            reply(401, r#"{"error": {"code": "InvalidAuthenticationToken", "message": "expired"}}"#)
        } else {
            reply(500, "not json at all")
        }
    });
    let c = client(&base);
    match c.get_json("/gone").unwrap_err() {
        NetError::Graph { status, code, .. } => {
            assert_eq!((status, code.as_str()), (410, "SyncStateNotFound"))
        }
        e => panic!("{e:?}"),
    }
    assert!(matches!(c.get_json("/auth"), Err(NetError::Auth(_))));
    assert!(matches!(c.post_json("/x", &json!({})), Err(NetError::Http { status: 500, .. })));
    t.join().unwrap();
}

#[test]
fn hostile_answers_are_errors_not_panics() {
    let bodies = [
        "[1, 2, 3]",
        r#"{"value": {"id": 1}}"#,
        r#"{"value": [1, "x", null, {"id": 5}, {"id": ""}], "@odata.deltaLink": 7}"#,
        "{\"value\": [",
        "\u{0}\u{1}",
    ];
    let n = bodies.len();
    let (base, t, _rx) = serve(n, move |req| {
        let i: usize = req.path.rsplit('/').next().unwrap().parse().unwrap();
        reply(200, bodies[i])
    });
    let c = client(&base);
    assert!(c.get_page("/0").is_err());
    assert!(c.get_page("/1").is_err());
    let d = c.messages_delta("f", Some("/2"), None).unwrap();
    assert!(d.messages.is_empty() && d.delta.is_none(), "{d:?}");
    assert!(c.get_page("/3").is_err());
    assert!(c.get_page("/4").is_err());
    t.join().unwrap();
}

#[test]
fn oversized_answers_are_refused() {
    let (base, t, _rx) =
        serve(1, |_| reply(200, &format!(r#"{{"value": [], "pad": "{}"}}"#, "x".repeat(4096))));
    let mut c = client(&base);
    c.max_json = 1024;
    assert!(c.get_page("/me/mailFolders").is_err());
    t.join().unwrap();
}

#[test]
fn messages_parse_and_stand_in_as_headers() {
    let v = json!({
        "id": "m1", "subject": "Line\r\nBcc: injected", "isRead": true, "isDraft": false,
        "importance": "High", "categories": ["Blue category", 7, ""],
        "flag": {"flagStatus": "flagged", "dueDateTime": {"dateTime": "2026-10-09T00:00:00.0000000", "timeZone": "UTC"}},
        "from": {"emailAddress": {"name": "Bob \"B\"", "address": "bob@example.org"}},
        "toRecipients": [{"emailAddress": {"address": "alice@example.com"}}, {"nope": 1}],
        "receivedDateTime": "2026-10-05T10:00:00Z", "internetMessageId": "<abc@example.org>",
        "inferenceClassification": "other", "parentFolderId": "F1"
    });
    let m = GraphMessage::from_json(&v).unwrap();
    assert_eq!((m.flag, m.importance.as_str(), m.focused), (FlagStatus::Flagged, "high", Some(false)));
    assert_eq!(m.categories, vec!["Blue category".to_owned()]);
    assert_eq!(m.to.len(), 1);
    let h = String::from_utf8(m.header_block()).unwrap();
    assert!(h.contains("Subject: Line  Bcc: injected\r\n"), "{h}");
    assert!(!h.contains("\nBcc:"), "no header injection: {h}");
    assert!(h.contains("From: \"Bob B\" <bob@example.org>\r\n"), "{h}");
    assert!(h.contains("Message-ID: <abc@example.org>\r\n") && h.contains("Importance: high\r\n"), "{h}");
    assert!(h.contains("Date: Mon, 05 Oct 2026 10:00:00 +0000\r\n"), "{h}");
    assert!(h.ends_with("\r\n\r\n"));
    assert!(m.received_at().is_some());
    let gone = GraphMessage::from_json(&json!({"id": "m2", "@removed": {"reason": "deleted"}})).unwrap();
    assert!(gone.removed);
    assert!(GraphMessage::from_json(&json!({"subject": "no id"})).is_none());
}

#[test]
fn patches_name_only_what_changed() {
    let p = MessagePatch { is_read: Some(true), ..Default::default() };
    assert_eq!(p.to_json(), json!({"isRead": true}));
    let due = Some("2026-10-09T00:00:00".to_owned());
    let p = MessagePatch { flag: Some((FlagStatus::Flagged, None, due)), ..Default::default() };
    let v = p.to_json();
    assert_eq!(v["flag"]["flagStatus"], "flagged");
    assert_eq!(v["flag"]["startDateTime"]["dateTime"], "2026-10-09T00:00:00", "a due date needs a start");
    assert!(MessagePatch::default().is_empty());
    assert_eq!(well_known_name(FolderRole::SentItems), Some("sentitems"));
}

#[test]
fn large_attachments_go_through_an_upload_session() {
    let data = vec![7u8; UPLOAD_CHUNK + 1000];
    let (base, t, rx) = serve(3, |req| {
        let host = req.header("host").unwrap().to_owned();
        if req.path.ends_with("createUploadSession") {
            reply(201, &format!(r#"{{"uploadUrl": "http://{host}/upload/s1"}}"#))
        } else if req.header("content-range").is_some_and(|r| r.starts_with("bytes 0-")) {
            reply(200, r#"{"nextExpectedRanges": ["3276800-"]}"#)
        } else {
            reply(201, "")
        }
    });
    let c = client(&base);
    let a =
        FileAttachment { name: "big.bin".into(), content_type: String::new(), data, ..Default::default() };
    c.add_attachment("m1", &a).unwrap();
    let seen: Vec<_> = rx.iter().collect();
    assert_eq!(seen.len(), 3);
    assert!(seen[1].header("authorization").is_none(), "the upload URL is pre-authorized");
    assert_eq!(
        seen[1].header("content-range"),
        Some(format!("bytes 0-3276799/{}", UPLOAD_CHUNK + 1000)).as_deref()
    );
    assert_eq!(seen[2].body.len(), 1000);
    t.join().unwrap();
}

#[test]
fn delta_rounds_read_to_the_delta_link_and_send_extra_preferences() {
    let (base, t, rx) = serve(3, |req| {
        let host = req.header("host").unwrap().to_owned();
        if req.path.contains("page=2") {
            reply(
                200,
                &format!(
                    r#"{{"value": [{{"id": "c", "@removed": {{"reason": "deleted"}}}}, {{"x": 1}}], "@odata.deltaLink": "http://{host}/v1.0/me/contactFolders/f/contacts/delta?$deltatoken=7"}}"#
                ),
            )
        } else if req.path.contains("elsewhere") {
            reply(200, r#"{"value": [], "@odata.deltaLink": "https://other.example.com/v1.0/delta"}"#)
        } else {
            reply(
                200,
                &format!(
                    r#"{{"value": [{{"id": "a", "givenName": "Pat"}}, {{"id": "b"}}], "@odata.nextLink": "http://{host}/v1.0/me/contactFolders/f/contacts/delta?page=2"}}"#
                ),
            )
        }
    });
    let mut c = client(&base);
    c.prefer.push("outlook.timezone=\"UTC\"".into());
    c.prefer.push("bad\r\nInjected: 1".into());
    let r = pim::delta_round(&c, "/me/contactFolders/f/contacts/delta", 100).unwrap();
    assert_eq!(r.changed.len(), 2);
    assert_eq!(r.removed, vec!["c".to_owned()]);
    assert!(r.delta_link.ends_with("$deltatoken=7"), "{}", r.delta_link);
    // A delta link to another server is refused, never followed with the token.
    assert!(pim::delta_round(&c, "/me/elsewhere/delta", 100).is_err());
    t.join().unwrap();
    let first = rx.recv().unwrap();
    let prefer = first.header("prefer").unwrap_or_default().to_owned();
    assert!(prefer.contains("outlook.timezone=\"UTC\""), "{prefer}");
    assert!(!prefer.contains("Injected"), "{prefer}");

    // More changes than asked for: an error, not a partial round.
    let (base, t, _rx) = serve(1, |_| {
        reply(
            200,
            r#"{"value": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "@odata.deltaLink": "http://127.0.0.1/x"}"#,
        )
    });
    assert!(pim::delta_round(&client(&base), "/me/todo/lists/l/tasks/delta", 2).is_err());
    t.join().unwrap();
}
