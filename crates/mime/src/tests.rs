//! Whole-message fixtures (parse) and builder round trips.

use proptest::prelude::*;

use crate::{Address, AttachmentSpec, DateTime, Disposition, Mailbox, MessageBuilder, PartBody, parse};

fn must(bytes: &[u8]) -> crate::Message {
    match parse(bytes) {
        Ok(m) => m,
        Err(e) => panic!("parse failed: {e}"),
    }
}

fn mb(name: Option<&str>, addr: &str) -> Address {
    Address::Mailbox(Mailbox::new(name, addr))
}

const PLAIN: &str = "Return-Path: <alice@example.com>\n\
Received: from mx.example.com by mail.example.org; Thu, 8 Oct 2026 08:30:01 +0000\n\
From: Alice Example <alice@example.com>\n\
To: Bob <bob@example.org>, carol@example.org\n\
Cc: \"Dave, the Builder\" <dave@example.net>\n\
Subject: Lunch\n\x20\x20tomorrow?\n\
Date: Thu, 08 Oct 2026 04:30:00 -0400\n\
Message-ID: <abc123@example.com>\n\
In-Reply-To: <prev@example.org>\n\
References: <root@example.org>\n\x20<prev@example.org>\n\
X-Priority: 2\n\
\n\
Hi Bob,\n\
\n\
Lunch at noon?\n";

#[test]
fn plain_message() {
    let m = must(PLAIN.as_bytes());
    assert_eq!(m.subject(), "Lunch  tomorrow?");
    assert_eq!(m.from(), Some(Mailbox::new(Some("Alice Example"), "alice@example.com")));
    assert_eq!(m.to(), vec![mb(Some("Bob"), "bob@example.org"), mb(None, "carol@example.org")]);
    assert_eq!(m.cc(), vec![mb(Some("Dave, the Builder"), "dave@example.net")]);
    assert!(m.bcc().is_empty());
    assert_eq!(m.date(), DateTime::from_civil(2026, 10, 8, 4, 30, 0, -240));
    assert_eq!(m.message_id().as_deref(), Some("abc123@example.com"));
    assert_eq!(m.in_reply_to(), vec!["prev@example.org"]);
    assert_eq!(m.references(), vec!["root@example.org", "prev@example.org"]);
    assert_eq!(m.importance(), Some("high"));
    assert_eq!(m.text_body().as_deref(), Some("Hi Bob,\n\nLunch at noon?\n"));
    assert_eq!(m.html_body(), None);
    assert!(m.attachments().is_empty());
    assert_eq!(m.parts().len(), 1);
    assert_eq!(m.root.content_type.mime_type, "text/plain");
}

const NEWSLETTER: &str = "From: \"News Desk\" <news@letters.example>\r\n\
To: undisclosed-recipients:;\r\n\
Subject: =?windows-1252?Q?Weekly_=93Digest=94_=96_issue_12?=\r\n\
Date: Mon, 5 Oct 2026 10:00:00 GMT\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=windows-1252\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
Importance: Low\r\n\
\r\n\
<html><body><h1>Caf=E9 news</h1><p>A long paragraph that goes past the s=\r\n\
oft line break limit.</p><img src=3D\"https://letters.example/p.gif\"></body></html>\r\n";

#[test]
fn html_newsletter() {
    let m = must(NEWSLETTER.as_bytes());
    assert_eq!(m.subject(), "Weekly \u{201c}Digest\u{201d} \u{2013} issue 12");
    assert_eq!(m.to(), vec![Address::Group { name: "undisclosed-recipients".into(), members: vec![] }]);
    assert_eq!(m.importance(), Some("low"));
    assert_eq!(m.date().map(|d| d.offset_min), Some(0));
    let html = m.html_body().unwrap_or_default();
    assert!(html.contains("<h1>Café news</h1>"), "{html}");
    assert!(html.contains("past the soft line"), "{html}");
    assert!(html.contains("src=\"https://letters.example/p.gif\""));
    assert!(html.ends_with("</html>\n"));
    assert_eq!(m.text_body(), None);
    assert!(m.attachments().is_empty());
}

const ALTERNATIVE: &str = "From: a@example.com\n\
To: b@example.com\n\
Subject: alt\n\
Content-Type: multipart/alternative; boundary=\"XYZ\"\n\
\n\
This is a multi-part message in MIME format.\n\
--XYZ\n\
Content-Type: text/plain; charset=utf-8\n\
Content-Transfer-Encoding: 8bit\n\
\n\
Plain café\n\
--XYZ\n\
Content-Type: text/html; charset=utf-8\n\
Content-Transfer-Encoding: base64\n\
\n\
PHA+SFRNTCBjYWbDqTwvcD4=\n\
--XYZ--\n\
epilogue text\n";

#[test]
fn multipart_alternative() {
    let m = must(ALTERNATIVE.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("Plain café"));
    assert_eq!(m.html_body().as_deref(), Some("<p>HTML café</p>"));
    assert!(m.attachments().is_empty());
    assert_eq!(m.parts().len(), 3);
    match &m.root.body {
        PartBody::Multipart(c) => assert_eq!(c.len(), 2),
        other => panic!("{other:?}"),
    }
}

const MIXED: &str = "From: =?utf-8?B?SsO8cmdlbg==?= <j@example.de>\r\n\
To: x@example.com\r\n\
Subject: Report\r\n\
Content-Type: multipart/mixed;\r\n\tboundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=inner\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain\r\n\
\r\n\
See attached.\r\n\
--inner\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>See <b>attached</b>.</p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"ignored.pdf\"\r\n\
Content-Disposition: attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--outer\r\n\
Content-Type: text/plain; name=notes.txt\r\n\
Content-Disposition: attachment\r\n\
\r\n\
some notes\r\n\
--outer--\r\n";

#[test]
fn mixed_with_attachments() {
    let m = must(MIXED.as_bytes());
    assert_eq!(m.from().and_then(|f| f.name).as_deref(), Some("Jürgen"));
    assert_eq!(m.text_body().as_deref(), Some("See attached."));
    assert_eq!(m.html_body().as_deref(), Some("<p>See <b>attached</b>.</p>"));
    let atts = m.attachments();
    assert_eq!(atts.len(), 2, "{atts:?}");
    let pdf = atts.first().cloned().unwrap_or_else(|| panic!("no pdf"));
    assert_eq!(pdf.filename, "résumé.pdf");
    assert_eq!(pdf.mime_type, "application/pdf");
    assert_eq!(pdf.data, b"%PDF-1.4\n");
    assert!(!pdf.inline);
    let notes = atts.get(1).cloned().unwrap_or_else(|| panic!("no notes"));
    assert_eq!(notes.filename, "notes.txt");
    assert_eq!(notes.data, b"some notes");
    assert_eq!(m.parts().len(), 6);
}

const RELATED: &str = "From: a@example.com\n\
Subject: inline image\n\
Content-Type: multipart/related; boundary=rel; type=\"text/html\"\n\
\n\
--rel\n\
Content-Type: text/html; charset=us-ascii\n\
\n\
<p><img src=\"cid:Logo@Example\"></p>\n\
--rel\n\
Content-Type: image/png\n\
Content-ID: <logo@example>\n\
Content-Disposition: inline\n\
Content-Transfer-Encoding: base64\n\
\n\
iVBORw0KGgo=\n\
--rel\n\
Content-Type: image/gif\n\
Content-ID: <unused@example>\n\
Content-Transfer-Encoding: base64\n\
\n\
R0lGODlh\n\
--rel--\n";

#[test]
fn related_with_cid_images() {
    let m = must(RELATED.as_bytes());
    assert_eq!(m.html_body().as_deref(), Some("<p><img src=\"cid:Logo@Example\"></p>"));
    assert_eq!(m.text_body(), None);
    let atts = m.attachments();
    assert_eq!(atts.len(), 2);
    let logo = atts.first().cloned().unwrap_or_else(|| panic!("logo"));
    assert_eq!(logo.content_id.as_deref(), Some("logo@example"));
    assert!(logo.inline);
    assert_eq!(logo.filename, "image001.png");
    assert_eq!(logo.data, b"\x89PNG\r\n\x1a\n");
    let gif = atts.get(1).cloned().unwrap_or_else(|| panic!("gif"));
    assert!(!gif.inline);
    assert_eq!(gif.filename, "image002.gif");
}

const RELATED_START: &str = "Content-Type: multipart/related; boundary=r; start=\"<root@x>\"\n\
\n\
--r\n\
Content-Type: image/png\n\
Content-ID: <img@x>\n\
\n\
png\n\
--r\n\
Content-Type: multipart/alternative; boundary=a\n\
Content-ID: <root@x>\n\
\n\
--a\n\
Content-Type: text/plain\n\
\n\
text\n\
--a\n\
Content-Type: text/html\n\
\n\
<img src=cid:img@x>\n\
--a--\n\
--r--\n";

#[test]
fn related_with_start_parameter() {
    let m = must(RELATED_START.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("text"));
    assert_eq!(m.html_body().as_deref(), Some("<img src=cid:img@x>"));
    let atts = m.attachments();
    assert_eq!(atts.len(), 1);
    assert!(atts.first().is_some_and(|a| a.inline));
}

const FORWARDED: &str = "From: fw@example.com\n\
Subject: Fwd: original\n\
Content-Type: multipart/mixed; boundary=\"b1\"\n\
\n\
--b1\n\
Content-Type: text/plain\n\
\n\
See below.\n\
--b1\n\
Content-Type: message/rfc822\n\
Content-Disposition: attachment\n\
\n\
From: orig@example.com\n\
Subject: =?utf-8?q?Original_=E2=9C=93?=\n\
Content-Type: multipart/alternative; boundary=\"b2\"\n\
\n\
--b2\n\
Content-Type: text/plain\n\
\n\
inner text\n\
--b2\n\
Content-Type: text/html\n\
\n\
<p>inner</p>\n\
--b2--\n\
--b1--\n";

#[test]
fn nested_rfc822() {
    let m = must(FORWARDED.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("See below."));
    assert_eq!(m.html_body(), None);
    let atts = m.attachments();
    assert_eq!(atts.len(), 1);
    let eml = atts.first().cloned().unwrap_or_else(|| panic!("eml"));
    assert_eq!(eml.filename, "Original ✓.eml");
    assert_eq!(eml.mime_type, "message/rfc822");
    let inner = must(&eml.data);
    assert_eq!(inner.subject(), "Original ✓");
    assert_eq!(inner.text_body().as_deref(), Some("inner text"));
    assert_eq!(inner.html_body().as_deref(), Some("<p>inner</p>"));
    // Nested parts are visited too: root, text, message, inner alternative, inner text, inner html.
    assert_eq!(m.parts().len(), 6);
    let nested = m.parts().into_iter().find_map(|p| match &p.body {
        PartBody::Message(msg) => Some(msg.from()),
        _ => None,
    });
    assert_eq!(nested, Some(Some(Mailbox::new(None, "orig@example.com"))));
}

#[test]
fn digest_defaults_to_messages() {
    let src = "Content-Type: multipart/digest; boundary=d\n\n--d\n\nSubject: one\n\nbody one\n--d\n\nSubject: two\n\nbody two\n--d--\n";
    let m = must(src.as_bytes());
    let atts = m.attachments();
    assert_eq!(atts.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), vec!["one.eml", "two.eml"]);
}

#[test]
fn encoded_word_subjects_in_several_charsets() {
    let cases = [
        ("=?ISO-8859-1?Q?Caf=E9?=", "Café"),
        ("=?iso-8859-15?q?=A4uro?=", "€uro"),
        ("=?UTF-8?B?8J+Ygw==?=", "😃"),
        ("=?Shift_JIS?B?k/qWe4zq?=", "日本語"),
        ("=?GB2312?B?1tDOxA==?=", "中文"),
        ("=?KOI8-R?Q?=F0=D2=C9=D7=C5=D4?=", "Привет"),
        ("=?windows-1252?Q?=93quoted=94?=", "\u{201c}quoted\u{201d}"),
        ("=?ISO-2022-JP?B?GyRCRnxLXDhsGyhC?=", "日本語"),
        ("=?big5?B?pKSk5Q==?=", "中文"),
        ("=?EUC-KR?B?x9Gxub7u?=", "한국어"),
        ("Re: =?utf-8?Q?caf=C3=A9?= =?utf-8?Q?_ok?=", "Re: café ok"),
        ("=?unknown-charset?Q?plain?=", "plain"),
    ];
    for (raw, want) in cases {
        let src = format!("Subject: {raw}\n\nx");
        assert_eq!(must(src.as_bytes()).subject(), want, "{raw}");
    }
}

#[test]
fn broken_boundaries() {
    // Missing final boundary: last part runs to the end.
    let src = "Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nfirst\n--b\nContent-Type: text/plain; name=a.txt\nContent-Disposition: attachment\n\ntail without close";
    let m = must(src.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("first"));
    assert_eq!(m.attachments().first().map(|a| a.data.clone()), Some(b"tail without close".to_vec()));

    // Declared boundary never appears: body kept as text.
    let src = "Content-Type: multipart/mixed; boundary=nope\n\nJust some text\n";
    let m = must(src.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("Just some text\n"));

    // No boundary parameter at all.
    let src = "Content-Type: multipart/alternative\n\nstill readable";
    assert_eq!(must(src.as_bytes()).text_body().as_deref(), Some("still readable"));

    // Boundary that is a prefix of another line does not split.
    let src = "Content-Type: multipart/mixed; boundary=ab\n\n--ab\n\none\n--abc\nstill one\n--ab--\n";
    assert_eq!(must(src.as_bytes()).text_body().as_deref(), Some("one\n--abc\nstill one"));

    // Part without headers and a part with junk before headers.
    let src = "Content-Type: multipart/mixed; boundary=q\n\n--q\nno headers here\n--q--";
    assert_eq!(must(src.as_bytes()).text_body().as_deref(), Some("no headers here"));
}

#[test]
fn garbage_bytes() {
    let m = must(b"\x00\xff\xfe garbage \x80\x81\n\n\x00");
    assert!(m.headers.is_empty());
    assert!(m.text_body().is_some());
    let m = must(b"Subject: \xff\xfe\n\n\xc3\x28");
    assert_eq!(m.subject(), "ÿþ");
    assert_eq!(m.text_body().as_deref(), Some("Ã("));
    let m = must(b"Content-Type: text/plain; charset=utf-8\n\n\xc3\x28");
    assert_eq!(m.text_body().as_deref(), Some("\u{FFFD}("));
    assert!(parse(&vec![b'a'; crate::MAX_MESSAGE_BYTES + 1]).is_err());
}

#[test]
fn depth_and_part_caps() {
    // Nest multiparts beyond MAX_DEPTH.
    let mut src = String::new();
    for i in 0..(crate::MAX_DEPTH + 10) {
        src.push_str(&format!("Content-Type: multipart/mixed; boundary=b{i}\n\n--b{i}\n"));
    }
    src.push_str("Content-Type: text/plain\n\ndeep\n");
    let m = must(src.as_bytes());
    let depth = {
        fn d(p: &crate::Part) -> usize {
            match &p.body {
                PartBody::Multipart(c) => 1 + c.iter().map(d).max().unwrap_or(0),
                _ => 0,
            }
        }
        d(&m.root)
    };
    assert!(depth <= crate::MAX_DEPTH);

    // Many parts.
    let mut src = String::from("Content-Type: multipart/mixed; boundary=z\n\n");
    for _ in 0..(crate::MAX_PARTS + 500) {
        src.push_str("--z\nContent-Type: application/octet-stream\n\nx\n");
    }
    let m = must(src.as_bytes());
    assert!(m.parts().len() <= crate::MAX_PARTS + 1);

    // Deeply nested messages.
    let mut src = String::new();
    for _ in 0..100 {
        src.push_str("Content-Type: message/rfc822\n\n");
    }
    src.push_str("Subject: bottom\n\nx");
    let m = must(src.as_bytes());
    let _ = m.attachments();
    assert!(m.parts().len() <= crate::MAX_DEPTH + 2);
}

#[test]
fn importance_variants() {
    for (h, want) in [
        ("Importance: high", Some("high")),
        ("Importance: Normal\nX-Priority: 1", None),
        ("X-Priority: 5 (Lowest)", Some("low")),
        ("X-Priority: 3", None),
        ("Priority: urgent", Some("high")),
        ("Priority: non-urgent", Some("low")),
        ("X-MSMail-Priority: High", Some("high")),
        ("X-Priority: junk", None),
    ] {
        let src = format!("{h}\n\nbody");
        assert_eq!(must(src.as_bytes()).importance(), want, "{h}");
    }
}

#[test]
fn message_to_bytes_round_trip() {
    for src in [PLAIN, NEWSLETTER, ALTERNATIVE, MIXED, RELATED, FORWARDED] {
        let m = must(src.as_bytes());
        let again = must(&m.to_bytes());
        assert_eq!(again.subject(), m.subject());
        assert_eq!(again.text_body(), m.text_body());
        assert_eq!(again.html_body(), m.html_body());
        assert_eq!(again.attachments(), m.attachments());
        assert_eq!(again.headers, m.headers);
    }
}

fn builder() -> MessageBuilder {
    MessageBuilder {
        from: Some(Mailbox::new(Some("Sam Example"), "sam@example.ca")),
        to: vec![mb(Some("Bob"), "bob@example.org")],
        subject: "Status update".into(),
        date: DateTime::from_civil(2026, 10, 9, 9, 15, 0, -240),
        ..Default::default()
    }
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn build_text_only() {
    let mut b = builder();
    b.text = Some("Hello\nworld".into());
    let raw = b.build();
    let s = text_of(&raw);
    assert!(s.contains("MIME-Version: 1.0\r\n"));
    assert!(s.contains("Content-Type: text/plain; charset=\"utf-8\"\r\n"));
    assert!(s.contains("Content-Transfer-Encoding: 7bit\r\n"));
    assert!(s.contains("Date: Fri, 09 Oct 2026 09:15:00 -0400\r\n"));
    assert!(s.contains("\r\n\r\nHello\r\nworld"));
    assert!(!s.contains("\n\n"), "bare LF in {s}");
    let m = must(&raw);
    assert_eq!(m.text_body().as_deref(), Some("Hello\nworld"));
    assert_eq!(m.subject(), "Status update");
    let id = m.message_id().unwrap_or_default();
    assert!(id.ends_with("@example.ca"), "{id}");
}

#[test]
fn build_structures() {
    let mut b = builder();
    b.text = Some("t".into());
    b.html = Some("<p>h</p>".into());
    let m = must(&b.build());
    assert_eq!(m.root.content_type.mime_type, "multipart/alternative");

    b.attachments.push(AttachmentSpec {
        filename: "logo.png".into(),
        mime_type: "image/png".into(),
        data: vec![1, 2, 3],
        content_id: Some("logo@x".into()),
    });
    b.html = Some("<img src=\"cid:logo@x\">".into());
    let m = must(&b.build());
    assert_eq!(m.root.content_type.mime_type, "multipart/alternative");
    let types: Vec<&str> = m.parts().iter().map(|p| p.content_type.mime_type.as_str()).collect();
    assert_eq!(
        types,
        vec!["multipart/alternative", "text/plain", "multipart/related", "text/html", "image/png"]
    );
    let atts = m.attachments();
    assert_eq!(atts.len(), 1);
    assert!(atts.first().is_some_and(|a| a.inline && a.content_id.as_deref() == Some("logo@x")));

    b.attachments.push(AttachmentSpec {
        filename: "a.bin".into(),
        mime_type: "application/octet-stream".into(),
        data: vec![0; 10],
        content_id: None,
    });
    let m = must(&b.build());
    assert_eq!(m.root.content_type.mime_type, "multipart/mixed");
    assert_eq!(m.attachments().len(), 2);
    let disp: Vec<Disposition> =
        m.parts().iter().filter(|p| p.filename.is_some()).map(|p| p.disposition.clone()).collect();
    assert_eq!(disp, vec![Disposition::Inline, Disposition::Attachment]);

    // Inline cid parts without HTML become regular attachments.
    let mut b = builder();
    b.text = Some("t".into());
    b.attachments.push(AttachmentSpec {
        filename: "i.png".into(),
        mime_type: "image/png".into(),
        data: vec![9],
        content_id: Some("c".into()),
    });
    let m = must(&b.build());
    assert_eq!(m.root.content_type.mime_type, "multipart/mixed");

    // HTML only.
    let mut b = builder();
    b.html = Some("<b>x</b>".into());
    let m = must(&b.build());
    assert_eq!(m.root.content_type.mime_type, "text/html");
    assert_eq!(m.text_body(), None);
}

#[test]
fn build_headers() {
    let mut b = builder();
    b.subject = "Réunion: ordre du jour très important pour la semaine prochaine, merci de lire".into();
    b.cc = vec![mb(Some("Doe, Jane"), "jane@example.org"), mb(Some("Zoë"), "zoe@example.org")];
    b.bcc = vec![mb(None, "hidden@example.org")];
    b.reply_to = vec![mb(None, "replies@example.ca")];
    b.sender = Some(Mailbox::new(Some("Assistant"), "assist@example.ca"));
    b.in_reply_to = vec!["<parent@example.org>".into()];
    b.references = vec!["root@example.org".into(), "parent@example.org".into()];
    b.importance = Some("High".into());
    b.message_id = Some("<fixed@example.ca>".into());
    b.extra_headers = vec![
        ("Disposition-Notification-To".into(), "sam@example.ca".into()),
        ("X-Evil".into(), "a\r\nBcc: victim@example.com".into()),
        ("Content-Type".into(), "text/evil".into()),
        ("Bad Name".into(), "x".into()),
    ];
    let raw = b.build();
    let s = text_of(&raw);
    for line in s.split("\r\n") {
        assert!(line.len() <= 78, "long line: {line}");
        assert!(line.is_ascii(), "non-ascii: {line}");
    }
    assert!(!s.contains("hidden@example.org"));
    assert!(s.contains("X-Priority: 1 (Highest)"));
    assert!(!s.contains("text/evil"));
    assert!(!s.contains("Bad Name"));
    let m = must(&raw);
    assert_eq!(m.subject(), b.subject);
    assert_eq!(m.cc(), b.cc);
    assert!(m.bcc().is_empty());
    assert_eq!(m.reply_to(), b.reply_to);
    assert_eq!(m.sender(), b.sender);
    assert_eq!(m.in_reply_to(), vec!["parent@example.org"]);
    assert_eq!(m.references(), vec!["root@example.org", "parent@example.org"]);
    assert_eq!(m.importance(), Some("high"));
    assert_eq!(m.message_id().as_deref(), Some("fixed@example.ca"));
    assert_eq!(m.headers.get("Disposition-Notification-To"), Some("sam@example.ca"));
    assert_eq!(m.headers.get("X-Evil"), Some("a  Bcc: victim@example.com"));
    assert!(m.headers.get("Bcc").is_none());

    b.keep_bcc = true;
    b.importance = Some("low".into());
    let m = must(&b.build());
    assert_eq!(m.bcc(), b.bcc);
    assert_eq!(m.importance(), Some("low"));
}

#[test]
fn message_ids_are_unique_and_deterministic_in_shape() {
    let b = builder();
    let a = must(&b.build()).message_id().unwrap_or_default();
    let c = must(&b.build()).message_id().unwrap_or_default();
    assert_ne!(a, c);
    let d = b.date.unwrap_or_default();
    assert_eq!(b.generate_message_id(d, 7), b.generate_message_id(d, 7));
    assert_ne!(b.generate_message_id(d, 7), b.generate_message_id(d, 8));
    let mut anon = builder();
    anon.from = None;
    assert!(anon.generate_message_id(d, 1).ends_with("@mailcraft.invalid"));
}

#[test]
fn build_boundary_collision_avoided() {
    // A 7bit text that happens to contain lines looking like our boundaries.
    let mut b = builder();
    let mut text = String::new();
    for k in 0..5 {
        text.push_str(&format!("------=_MC_0000000000000000_{k:04}\n"));
    }
    b.text = Some(text.clone());
    b.html = Some("<p>x</p>".into());
    b.attachments.push(AttachmentSpec {
        filename: "fwd.eml".into(),
        mime_type: "message/rfc822".into(),
        data: b"Subject: inner\r\n\r\nbody\r\n".to_vec(),
        content_id: None,
    });
    let m = must(&b.build());
    assert_eq!(m.text_body().as_deref(), Some(text.as_str()));
    let atts = m.attachments();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts.first().map(|a| a.filename.as_str()), Some("fwd.eml"));
    let inner = must(&atts.first().map(|a| a.data.clone()).unwrap_or_default());
    assert_eq!(inner.subject(), "inner");
}

#[test]
fn build_long_and_unicode_text_uses_qp() {
    let mut b = builder();
    let text = format!("{}\nnaïve ✓\nFrom here\ntrailing \n", "x".repeat(200));
    b.text = Some(text.clone());
    let raw = b.build();
    let s = text_of(&raw);
    assert!(s.contains("Content-Transfer-Encoding: quoted-printable"));
    assert!(s.split("\r\n").all(|l| l.len() <= 78));
    assert_eq!(must(&raw).text_body().as_deref(), Some(text.as_str()));
}

#[test]
fn build_attachment_names() {
    let mut b = builder();
    b.text = Some("x".into());
    let long = format!("{}.pdf", "very long file name ".repeat(5));
    for name in [
        "plain.txt",
        "with space.doc",
        "naïve résumé.pdf",
        "日本語のファイル名がとても長い場合のテスト.xlsx",
        long.as_str(),
        "quote\"d.txt",
    ] {
        b.attachments.push(AttachmentSpec {
            filename: name.into(),
            mime_type: "application/octet-stream".into(),
            data: name.as_bytes().to_vec(),
            content_id: None,
        });
    }
    let raw = b.build();
    let s = text_of(&raw);
    assert!(s.split("\r\n").all(|l| l.len() <= 78), "{s}");
    let m = must(&raw);
    let names: Vec<String> = m.attachments().into_iter().map(|a| a.filename).collect();
    assert_eq!(
        names,
        vec![
            "plain.txt",
            "with space.doc",
            "naïve résumé.pdf",
            "日本語のファイル名がとても長い場合のテスト.xlsx",
            long.as_str(),
            "quote\"d.txt"
        ]
    );
}

#[test]
fn build_without_from_or_date() {
    let b = MessageBuilder::new();
    let raw = b.build();
    let m = must(&raw);
    assert!(m.from().is_none());
    assert!(m.date().is_some());
    assert_eq!(m.text_body().as_deref(), Some(""));
    assert_eq!(m.subject(), "");
}

fn name_strat() -> impl Strategy<Value = Option<String>> {
    proptest::option::of(
        "[A-Za-z0-9 ,.'\"()é✓日]{1,20}"
            .prop_map(|s| s.trim().to_owned())
            .prop_filter("non-empty", |s| !s.is_empty()),
    )
}

fn mailbox_strat() -> impl Strategy<Value = Mailbox> {
    (name_strat(), "[a-z0-9]{1,8}@[a-z]{1,8}\\.(com|org|ca)")
        .prop_map(|(n, a)| Mailbox::new(n.as_deref(), &a))
}

fn addr_list_strat() -> impl Strategy<Value = Vec<Address>> {
    proptest::collection::vec(mailbox_strat().prop_map(Address::Mailbox), 0..4)
}

fn attachment_strat() -> impl Strategy<Value = AttachmentSpec> {
    (
        "[A-Za-z0-9 éü日_-]{1,30}\\.(png|pdf|txt|bin)",
        prop_oneof![
            Just("image/png"),
            Just("application/pdf"),
            Just("text/plain"),
            Just("application/octet-stream")
        ],
        proptest::collection::vec(any::<u8>(), 0..300),
        proptest::option::of("[a-z0-9]{1,10}@[a-z]{1,6}"),
    )
        .prop_map(|(filename, mime, data, content_id)| AttachmentSpec {
            filename: filename.trim().to_owned(),
            mime_type: mime.to_owned(),
            data,
            content_id,
        })
}

fn body_text_strat() -> impl Strategy<Value = String> {
    // LF line endings only (CRLF normalises to LF on parse).
    "[A-Za-z0-9 .,;:!?=\\-_\t\né✓日\u{1F600}]{0,300}"
}

prop_compose! {
    fn builder_strat()(
        from in proptest::option::of(mailbox_strat()),
        to in addr_list_strat(),
        cc in addr_list_strat(),
        bcc in addr_list_strat(),
        subject in "[A-Za-z0-9 :,.!?=_éü✓日\u{1F600}-]{0,120}",
        unix in 0i64..4_000_000_000,
        off in -720i32..840,
        ids in proptest::collection::vec("[a-z0-9.]{1,10}@[a-z]{1,8}\\.com", 0..3),
        importance in prop_oneof![Just(None), Just(Some("high".to_owned())), Just(Some("low".to_owned()))],
        text in proptest::option::of(body_text_strat()),
        html in proptest::option::of(body_text_strat().prop_map(|s| format!("<p>{s}</p>"))),
        attachments in proptest::collection::vec(attachment_strat(), 0..4),
        keep_bcc in any::<bool>(),
    ) -> MessageBuilder {
        MessageBuilder {
            from,
            to,
            cc,
            bcc,
            subject: subject.trim().to_owned(),
            date: Some(DateTime::from_unix(unix, off)),
            references: ids.clone(),
            in_reply_to: ids.last().cloned().into_iter().collect(),
            importance,
            text,
            html,
            attachments,
            keep_bcc,
            ..Default::default()
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn build_parse_round_trip(b in builder_strat()) {
        let raw = b.build();
        for line in String::from_utf8_lossy(&raw).split("\r\n") {
            prop_assert!(line.is_ascii(), "non-ASCII line {line:?}");
            prop_assert!(line.len() <= 998);
        }
        let m = parse(&raw).map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(m.subject(), b.subject.clone());
        prop_assert_eq!(m.from(), b.from.clone());
        prop_assert_eq!(m.to(), b.to.clone());
        prop_assert_eq!(m.cc(), b.cc.clone());
        prop_assert_eq!(m.bcc(), if b.keep_bcc { b.bcc.clone() } else { Vec::new() });
        prop_assert_eq!(m.date(), b.date);
        prop_assert_eq!(m.references(), b.references.clone());
        prop_assert_eq!(m.in_reply_to(), b.in_reply_to.clone());
        prop_assert_eq!(m.importance(), b.importance.as_deref());
        let want_text = match (&b.text, &b.html) {
            (None, None) => Some(String::new()),
            (t, _) => t.clone(),
        };
        prop_assert_eq!(m.text_body(), want_text);
        prop_assert_eq!(m.html_body(), b.html.clone());
        let atts = m.attachments();
        prop_assert_eq!(atts.len(), b.attachments.len());
        // Inline (cid + html) attachments come first inside the related part, then the regular ones.
        let has_html = b.html.is_some();
        let (inline, regular): (Vec<&AttachmentSpec>, Vec<&AttachmentSpec>) =
            b.attachments.iter().partition(|a| has_html && a.content_id.is_some());
        for (got, want) in atts.iter().zip(inline.iter().chain(regular.iter())) {
            prop_assert_eq!(&got.filename, &want.filename);
            prop_assert_eq!(&got.mime_type, &want.mime_type);
            prop_assert_eq!(&got.data, &want.data);
            prop_assert_eq!(&got.content_id, &want.content_id);
        }
        // Serialising the parsed tree and parsing again keeps the bodies.
        let again = parse(&m.to_bytes()).map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(again.text_body(), m.text_body());
        prop_assert_eq!(again.attachments(), m.attachments());
    }

    #[test]
    fn parse_never_panics_on_mutated_messages(
        which in 0usize..6,
        cuts in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..20),
        truncate in any::<prop::sample::Index>(),
    ) {
        let src = [PLAIN, NEWSLETTER, ALTERNATIVE, MIXED, RELATED, FORWARDED];
        let mut bytes = src.get(which).copied().unwrap_or(PLAIN).as_bytes().to_vec();
        for (idx, b) in cuts {
            let i = idx.index(bytes.len());
            if let Some(slot) = bytes.get_mut(i) {
                *slot = b;
            }
        }
        let t = truncate.index(bytes.len() + 1);
        bytes.truncate(t);
        if let Ok(m) = parse(&bytes) {
            let _ = (m.subject(), m.text_body(), m.html_body(), m.attachments(), m.to_bytes());
            let _ = (m.from(), m.to(), m.cc(), m.date(), m.message_id(), m.in_reply_to(), m.importance());
        }
    }
}

// ---- Review regressions -------------------------------------------------------------------------------------

#[test]
fn review_from_civil_extreme_offset_does_not_panic() {
    assert!(DateTime::from_civil(2026, 1, 1, 0, 0, 0, i32::MIN).is_none());
    assert!(DateTime::from_civil(2026, 1, 1, 0, 0, 0, i32::MAX).is_none());
}

#[test]
fn review_days_from_civil_extreme_years_do_not_panic() {
    for y in [i64::MIN, i64::MIN + 1, i64::MAX, i64::MAX - 1, -1_000_000_000_000_000, 1_000_000_000_000_000] {
        let _ = crate::date::days_from_civil(y, 1, 1);
        let _ = crate::date::days_from_civil(y, 12, 31);
    }
}

#[test]
fn review_fold_never_emits_whitespace_only_lines() {
    for words in 0..40 {
        for spaces in [1usize, 2, 5, 40, 120] {
            let value = format!("{}{}", "word ".repeat(words), " ".repeat(spaces));
            let f = crate::header::fold("Subject", &value);
            for line in f.split("\r\n").skip(1) {
                assert!(!line.trim().is_empty(), "whitespace-only line in {f:?}");
            }
        }
    }
    let mut b = builder();
    b.subject = format!("{}{}", "word ".repeat(15), " ".repeat(40));
    let raw = text_of(&b.build());
    let head = raw.split("\r\n\r\n").next().unwrap_or_default();
    for line in head.split("\r\n") {
        assert!(!line.trim().is_empty(), "whitespace-only header line in {head:?}");
    }
}

#[test]
fn review_literal_encoded_word_filenames_round_trip() {
    let v = "=?utf-8?q?x?=.txt";
    let s = format!("attachment; {}", crate::params::format_param("filename", v));
    let (_, ps) = crate::params::parse_header_params(&s);
    assert_eq!(ps.iter().find(|(k, _)| k == "filename").map(|(_, v)| v.as_str()), Some(v));
    let mut b = builder();
    b.text = Some("x".into());
    b.attachments.push(AttachmentSpec {
        filename: v.into(),
        mime_type: "application/pdf".into(),
        data: vec![1],
        content_id: None,
    });
    let m = must(&b.build());
    assert_eq!(m.attachments().first().map(|a| a.filename.clone()).as_deref(), Some(v));
}

#[test]
fn review_numeric_zone_wins_over_alphabetic() {
    let d = DateTime::parse_rfc5322("Thu, 08 Oct 2026 04:30:00 -0000 EST").ok();
    assert_eq!(d.map(|d| d.offset_min), Some(0));
    let d = DateTime::parse_rfc5322("Thu, 08 Oct 2026 04:30:00 +0200 X").ok();
    assert_eq!(d.map(|d| d.offset_min), Some(120));
}

#[test]
fn review_to_bytes_respects_cte_of_message_and_boundaryless_parts() {
    let src = "Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nouter\n--b\nContent-Type: message/rfc822\nContent-Transfer-Encoding: base64\n\nU3ViamVjdDogaW5uZXIKCmJvZHkK\n--b--\n";
    let m = must(src.as_bytes());
    assert_eq!(m.attachments().first().map(|a| must(&a.data).subject()).as_deref(), Some("inner"));
    let again = must(&m.to_bytes());
    assert_eq!(again.attachments().first().map(|a| must(&a.data).subject()).as_deref(), Some("inner"));

    let src = "Content-Type: multipart/mixed\nContent-Transfer-Encoding: base64\n\naGVsbG8gd29ybGQ=\n";
    let m = must(src.as_bytes());
    assert_eq!(m.text_body().as_deref(), Some("hello world"));
    let again = must(&m.to_bytes());
    assert_eq!(again.text_body().as_deref(), Some("hello world"));
}

#[test]
fn review_attachment_type_parameters_kept() {
    let mut b = builder();
    b.text = Some("x".into());
    b.attachments.push(AttachmentSpec {
        filename: "invite.ics".into(),
        mime_type: "text/calendar; method=REQUEST; charset=utf-8".into(),
        data: b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".to_vec(),
        content_id: None,
    });
    let m = must(&b.build());
    let part = m.parts().into_iter().find(|p| p.content_type.mime_type == "text/calendar").cloned();
    let part = part.unwrap_or_else(|| panic!("no calendar part"));
    assert_eq!(part.content_type.param("method"), Some("REQUEST"));
    assert_eq!(part.content_type.param("charset"), Some("utf-8"));
    assert_eq!(part.filename.as_deref(), Some("invite.ics"));
}

#[test]
fn review_extra_headers_cannot_duplicate_core_fields() {
    let mut b = builder();
    b.extra_headers = vec![
        ("Date".into(), "Mon, 1 Jan 2001 00:00:00 +0000".into()),
        ("From".into(), "evil@example.com".into()),
        ("Bcc".into(), "leak@example.com".into()),
        ("Message-ID".into(), "<dup@x>".into()),
        ("X-Mailer".into(), "MailCraft".into()),
    ];
    let m = must(&b.build());
    assert_eq!(m.headers.get_all("Date").count(), 1);
    assert_eq!(m.headers.get_all("From").count(), 1);
    assert_eq!(m.headers.get_all("Message-ID").count(), 1);
    assert!(m.headers.get("Bcc").is_none());
    assert_eq!(m.headers.get("X-Mailer"), Some("MailCraft"));
}

#[test]
fn review_bare_local_part_round_trips() {
    let l = vec![mb(None, "postmaster")];
    let s = crate::format_address_list(&l, true);
    assert_eq!(crate::parse_address_list(&s), l);
}

#[test]
fn review_encoded_word_scan_is_linear() {
    // Many `=?` candidates whose closing `?=` is far away (or absent) used to rescan to the end for each one.
    let s = format!("{}?=", "=?a?b?x y".repeat(45_000));
    let t = std::time::Instant::now();
    assert_eq!(crate::encoded::decode_words(&s), s);
    let s = "=?a?q?x".repeat(60_000);
    assert_eq!(crate::encoded::decode_words(&s), s);
    let s = format!("{} \r?=", "=?utf-8?q?ab".repeat(40_000));
    assert_eq!(crate::encoded::decode_words(&s), s);
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t.elapsed());
}

#[test]
fn review_to_bytes_reapplies_aliased_transfer_encodings() {
    let data = b"\x00\x01\x02\xff binary =3D\r\n\x80 tail".to_vec();
    let b64 = crate::transfer::encode_base64(&data);
    let qp = crate::transfer::encode_qp(&data);
    for (cte, body) in [
        ("\"Base64\"", b64.clone()),
        ("b", b64.clone()),
        ("qp", qp.clone()),
        ("Quoted_Printable", qp.clone()),
        ("x-uuencode", "begin 644 a.bin\n7``$\"_R!B:6YA<GD@/3-$#0J`('1A:6P`\n`\nend\n".to_owned()),
    ] {
        let src = format!(
            "Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nhi\n--b\n\
             Content-Type: application/octet-stream; name=a.bin\nContent-Transfer-Encoding: {cte}\n\n{body}\n--b--\n"
        );
        let m = must(src.as_bytes());
        let first = m.attachments().first().map(|a| a.data.clone());
        assert_eq!(first.as_deref(), Some(&data[..]), "{cte}");
        let again = must(&m.to_bytes());
        assert_eq!(again.attachments().first().map(|a| a.data.clone()).as_deref(), Some(&data[..]), "{cte}");
    }
}

#[test]
fn review_boundary_never_found_round_trips() {
    // The declared boundary never occurs, so the body is kept as one synthetic child. Writing it back must not
    // frame it with delimiters (its header-like text would then be parsed as part headers).
    let src = "Content-Type: multipart/mixed; boundary=nope\n\nSubject: not a header\n\nhello\n";
    let m = must(src.as_bytes());
    let want = Some("Subject: not a header\n\nhello\n".to_owned());
    assert_eq!(m.text_body(), want);
    let again = must(&m.to_bytes());
    assert_eq!(again.text_body().map(|t| t.replace("\r\n", "\n")), want);
}

#[test]
fn review_builder_never_emits_multipart_leaf() {
    // A multipart/* type on a base64 leaf would make the parser look for a boundary and lose the attachment.
    let mut b = builder();
    b.text = Some("x".into());
    b.attachments.push(AttachmentSpec {
        filename: "parts.bin".into(),
        mime_type: "multipart/mixed; boundary=zz".into(),
        data: vec![1, 2, 3],
        content_id: None,
    });
    let m = must(&b.build());
    let atts = m.attachments();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts.first().map(|a| a.data.clone()), Some(vec![1, 2, 3]));
    assert_eq!(atts.first().map(|a| a.filename.clone()).as_deref(), Some("parts.bin"));
}

#[test]
fn review_cid_references_match_whole_ids() {
    let src = "Content-Type: multipart/related; boundary=r\n\n--r\nContent-Type: text/html\n\n\
               <img src=\"cid:abc\"><img src='CID:logo%40Example'>\n\
               --r\nContent-Type: image/png\nContent-ID: <a>\n\nx\n\
               --r\nContent-Type: image/png\nContent-ID: <abc>\n\nx\n\
               --r\nContent-Type: image/png\nContent-ID: <logo@example>\n\nx\n--r--\n";
    let m = must(src.as_bytes());
    let inline: Vec<(Option<String>, bool)> =
        m.attachments().into_iter().map(|a| (a.content_id, a.inline)).collect();
    assert_eq!(
        inline,
        vec![(Some("a".into()), false), (Some("abc".into()), true), (Some("logo@example".into()), true)]
    );
}
