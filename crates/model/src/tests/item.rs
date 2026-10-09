use proptest::prelude::*;

use super::mail;
use crate::item::make_preview;
use crate::{AttachmentMeta, Body, BodyFormat, DateTime, FlagState, Importance, MailItem, Sensitivity};

const NOW: DateTime = DateTime { unix: 1_791_552_000, offset_min: 0 };

/// Parses with the mime crate (a parse failure fails the test rather than skipping it).
fn parse(raw: &str) -> MailItem {
    let msg = mailcraft_mime::parse(raw.as_bytes()).expect("well-formed test message parses");
    MailItem::from_message(&msg, raw.as_bytes(), NOW)
}

const PLAIN: &str = "Received: from mx.example.net by mail.example.net; Thu, 08 Oct 2026 08:31:00 +0000\r\n\
From: Alice Rivera <alice@example.com>\r\n\
To: Test User <test.user@example.net>, team: bob@example.org, carol@example.com;\r\n\
Cc: Bob Chen <bob.chen@example.org>\r\n\
Subject: URGENT: server room access\r\n\
Date: Thu, 08 Oct 2026 04:30:00 -0400\r\n\
Message-ID: <abc123@example.com>\r\n\
In-Reply-To: <parent@example.com>\r\n\
References: <root@example.com> <parent@example.com>\r\n\
Importance: High\r\n\
Sensitivity: Company-Confidential\r\n\
Keywords: Red category, Blue category, red category\r\n\
X-Message-Flag: Follow up\r\n\
\r\n\
The badge reader is down.\r\n\
Please   use the side door today.\r\n";

#[test]
fn from_plain_message() {
    let item = parse(PLAIN);
    assert_eq!(item.subject, "URGENT: server room access");
    assert_eq!(item.from.as_ref().map(|f| f.addr.as_str()), Some("alice@example.com"));
    assert_eq!(item.to.len(), 3, "group members are flattened");
    assert_eq!(item.cc.len(), 1);
    assert_eq!(item.message_id.as_deref(), Some("abc123@example.com"));
    assert_eq!(item.in_reply_to, ["parent@example.com"]);
    assert_eq!(item.references, ["root@example.com", "parent@example.com"]);
    assert_eq!(item.importance, Importance::High);
    assert_eq!(item.sensitivity, Sensitivity::Confidential);
    assert_eq!(item.categories, ["Red category", "Blue category"]);
    assert!(item.is_flagged());
    assert_eq!(item.sent.map(|d| d.unix), Some(1_791_448_200));
    // Received header wins over Date (one minute later).
    assert_eq!(item.received.unix, 1_791_448_260);
    assert_eq!(item.body.format, BodyFormat::PlainText);
    assert_eq!(item.preview, "The badge reader is down. Please use the side door today.");
    assert_eq!(item.size, PLAIN.len() as u64);
    assert!(!item.read);
    assert_eq!(item.raw.as_deref(), Some(PLAIN.as_bytes()));
}

#[test]
fn from_html_message_with_attachment() {
    let raw = "From: news@example.org\r\nSubject: Newsletter\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
--b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
<html><head><style>p{}</style></head><body><p>Hello&nbsp;<b>there</b></p></body></html>\r\n\
--b1\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--b1--\r\n";
    let item = parse(raw);
    assert_eq!(item.body.format, BodyFormat::Html);
    assert_eq!(item.preview, "Hello there");
    assert_eq!(item.received, NOW, "no Received or Date header: falls back to now");
    assert!(item.has_attachments());
    assert_eq!(item.attachments[0].filename, "report.pdf");
    assert_eq!(item.attachments[0].size, 5);
    assert_eq!(item.importance, Importance::Normal);
    assert_eq!(item.flag, FlagState::None);
}

#[test]
fn preview_rules() {
    assert_eq!(make_preview(Some("  a\n b "), Some("<p>html</p>")), "a b");
    assert_eq!(make_preview(Some("   "), Some("<p>html</p>")), "html");
    assert_eq!(make_preview(None, Some("<p>x &amp; y</p>")), "x & y");
    assert_eq!(make_preview(None, None), "");
    let long = "x".repeat(1000);
    assert_eq!(make_preview(Some(&long), None).chars().count(), 255);
}

#[test]
fn item_helpers() {
    let mut m = MailItem { size: 0, ..mail("x") };
    assert_eq!(m.size_kb(), 1);
    assert_eq!(m.size_label(), "1 KB");
    m.size = 1025;
    assert_eq!(m.size_label(), "2 KB");
    m.body = Body { html: Some("<b>Bold</b> text".into()), ..Default::default() };
    assert_eq!(crate::text::collapse_whitespace(&m.body_text()), "Bold text");
    m.refresh_preview();
    assert_eq!(m.preview, "Bold text");
    m.attachments = vec![AttachmentMeta { inline: true, ..Default::default() }];
    assert!(!m.has_attachments());
    assert_eq!(BodyFormat::RichText.title_label(), "Rich Text");
}

proptest! {
    #[test]
    fn hostile_messages_never_panic(raw in prop::collection::vec(any::<u8>(), 0..2000)) {
        if let Ok(msg) = mailcraft_mime::parse(&raw) {
            let item = MailItem::from_message(&msg, &raw, NOW);
            prop_assert!(item.preview.chars().count() <= 255);
            prop_assert_eq!(item.size, raw.len() as u64);
        }
    }

    #[test]
    fn hostile_header_text_never_panics(subject in "[ -~]{0,200}", kw in "[ -~]{0,100}", date in "[ -~]{0,60}") {
        let raw = format!("Subject: {subject}\r\nKeywords: {kw}\r\nReceived: x; {date}\r\nDate: {date}\r\n\r\nbody");
        if let Ok(msg) = mailcraft_mime::parse(raw.as_bytes()) {
            let item = MailItem::from_message(&msg, raw.as_bytes(), NOW);
            prop_assert!(item.categories.iter().all(|c| !c.is_empty()));
        }
    }
}

#[test]
fn body_text_falls_back_to_html_when_text_is_blank() {
    let m = MailItem {
        body: Body {
            text: Some(" \r\n ".into()),
            html: Some("<p>Real body</p>".into()),
            ..Default::default()
        },
        ..mail("x")
    };
    assert_eq!(crate::text::collapse_whitespace(&m.body_text()), "Real body");
    let only_blank = MailItem { body: Body { text: Some("  ".into()), ..Default::default() }, ..mail("x") };
    assert_eq!(only_blank.body_text(), "  ");
}
