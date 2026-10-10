use std::io::{Cursor, Write};

use mailcraft_mime::Mailbox;
use mailcraft_model::{BodyFormat, DateTime, FolderId, Importance, MailItem, Sensitivity};
use proptest::prelude::*;

use crate::msg::{self, AttachmentContent, MsgAttachment, MsgMessage, rtf, rtfhtml};

const SPEC_RTF: &[u8] = &[
    0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03,
    0x00, 0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65,
    0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0,
];

#[test]
fn rtf_decompresses_reference_example() {
    assert_eq!(rtf::INITIAL_DICTIONARY.len(), 207);
    let out = rtf::decompress(SPEC_RTF).unwrap();
    assert_eq!(out, b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    assert_eq!(rtf::crc32(&SPEC_RTF[16..]), 0xa7c7_c5f1);
}

#[test]
fn rtf_compress_round_trip_and_uncompressed_form() {
    let text = b"{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 \\deff0{\\fonttbl{\\f0 Arial;}}hello hello hello hello world}".repeat(30);
    let c = rtf::compress(&text);
    assert!(c.len() < text.len());
    assert_eq!(rtf::decompress(&c).unwrap(), text);
    assert_eq!(rtf::decompress(&rtf::compress(b"")).unwrap(), b"");
    let mut mela = Vec::new();
    mela.extend_from_slice(&15u32.to_le_bytes());
    mela.extend_from_slice(&3u32.to_le_bytes());
    mela.extend_from_slice(b"MELA");
    mela.extend_from_slice(&0u32.to_le_bytes());
    mela.extend_from_slice(b"abc");
    assert_eq!(rtf::decompress(&mela).unwrap(), b"abc");
    assert!(rtf::decompress(b"short").is_err());
}

const ENCAPSULATED: &str = "{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 \\deff0{\\fonttbl{\\f0\\fswiss Arial;}}\r\n\
{\\*\\htmltag19 <html>}{\\*\\htmltag34 <head>}{\\*\\htmltag50 <body>}\\htmlrtf {\\htmlrtf0 Hello \\'e9 world\\par\r\n\
{\\*\\htmltag84 <b>}\\htmlrtf {\\b \\htmlrtf0 bold\\htmlrtf }\\htmlrtf0 {\\*\\htmltag92 </b>}\r\n\
\\htmlrtf\\par\\htmlrtf0 \\u8364?{\\*\\mhtmltag8 <x>}{\\*\\htmltag58 </body>}{\\*\\htmltag27 </html>}}}";

#[test]
fn rtf_html_deencapsulation_and_text() {
    let html = rtfhtml::extract_html(ENCAPSULATED.as_bytes()).unwrap();
    assert_eq!(html, "<html><head><body>Hello \u{e9} world\r\n<b>bold</b>\u{20ac}</body></html>");
    assert!(rtfhtml::extract_html(b"{\\rtf1 plain}").is_none());
    let text = rtfhtml::extract_text(
        b"{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}{\\*\\generator x;}First\\par Second\\tab x\\'e9 \\{y\\}}",
    );
    assert_eq!(text, "First\nSecond\tx\u{e9} {y}");
}

fn sample() -> MsgMessage {
    let inner = MsgMessage {
        subject: "Inner note".into(),
        from: Some(Mailbox::new(Some("Cy"), "cy@example.net")),
        to: vec![Mailbox::new(None, "ann@example.com")],
        body_text: Some("inner body".into()),
        sent: Some(DateTime::from_unix(1_790_000_000, 0)),
        // The writer stores a creation time (falling back to the send time).
        created: Some(DateTime::from_unix(1_790_000_000, 0)),
        ..Default::default()
    };
    MsgMessage {
        subject: "RE: Quarterly plan".into(),
        from: Some(Mailbox::new(Some("Ann Lee"), "ann@example.com")),
        sender: Some(Mailbox::new(Some("Assistant"), "pa@example.com")),
        to: vec![Mailbox::new(Some("Bob"), "bob@example.org"), Mailbox::new(None, "dee@example.org")],
        cc: vec![Mailbox::new(Some("Cy"), "cy@example.net")],
        bcc: vec![Mailbox::new(None, "audit@example.com")],
        sent: Some(DateTime::from_unix(1_791_000_000, 0)),
        received: Some(DateTime::from_unix(1_791_000_100, 0)),
        created: Some(DateTime::from_unix(1_791_000_100, 0)),
        message_id: Some("q2@example.com".into()),
        in_reply_to: vec!["q1@example.com".into()],
        references: vec!["q0@example.com".into(), "q1@example.com".into()],
        importance: Importance::High,
        sensitivity: Sensitivity::Confidential,
        read: true,
        unsent: false,
        categories: vec!["Blue category".into(), "Follow".into()],
        body_text: Some("Hello Bob \u{2014} caf\u{e9}".into()),
        body_html: Some("<p>Hello <b>Bob</b> <img src=\"cid:logo@x\"></p>".into()),
        body_rtf: None,
        body_format: BodyFormat::Html,
        transport_headers: None,
        attachments: vec![
            MsgAttachment {
                filename: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                content_id: None,
                inline: false,
                content: AttachmentContent::Data(b"%PDF-1.4 fake".to_vec()),
            },
            MsgAttachment {
                filename: "logo.png".into(),
                mime_type: "image/png".into(),
                content_id: Some("logo@x".into()),
                inline: true,
                content: AttachmentContent::Data(vec![0x89, b'P', b'N', b'G', 0, 1, 2]),
            },
            MsgAttachment {
                filename: "Inner note.msg".into(),
                mime_type: "message/rfc822".into(),
                content_id: None,
                inline: false,
                content: AttachmentContent::Message(Box::new(inner)),
            },
        ],
        ..Default::default()
    }
}

#[test]
fn msg_write_read_round_trip() {
    let m = sample();
    let bytes = msg::write(&m).unwrap();
    assert!(bytes.starts_with(&msg::CFB_SIGNATURE));
    assert_eq!(crate::Format::sniff(&bytes), Some(crate::Format::Msg));
    let back = msg::read(&bytes).unwrap();
    assert_eq!(back, m);
}

#[test]
fn msg_to_mail_item_and_back() {
    let m = sample();
    let item = m.to_mail_item(FolderId(3), DateTime::default());
    assert_eq!(item.subject, "RE: Quarterly plan");
    assert_eq!(item.folder, FolderId(3));
    assert_eq!(item.from, m.from);
    assert_eq!(item.cc, m.cc);
    assert_eq!(item.bcc, m.bcc);
    assert_eq!(item.message_id.as_deref(), Some("q2@example.com"));
    assert_eq!(item.importance, Importance::High);
    assert_eq!(item.sensitivity, Sensitivity::Confidential);
    assert_eq!(item.received.unix, 1_791_000_100);
    assert!(item.read);
    assert_eq!(item.categories, m.categories);
    assert_eq!(item.body.html, m.body_html);
    let names: Vec<&str> = item.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert!(names.contains(&"report.pdf") && names.contains(&"Inner note.eml"));
    assert!(item.attachments.iter().any(|a| a.inline && a.content_id.as_deref() == Some("logo@x")));
    // Back to .msg via the kept source: attachment content and the embedded message survive.
    let again = msg::read(&msg::write_item(&item).unwrap()).unwrap();
    assert_eq!(again.subject, m.subject);
    assert_eq!(again.to, m.to);
    assert_eq!(again.categories, m.categories);
    let pdf = again.attachments.iter().find(|a| a.filename == "report.pdf").unwrap();
    assert_eq!(pdf.content, AttachmentContent::Data(b"%PDF-1.4 fake".to_vec()));
    let embedded = again.attachments.iter().find_map(|a| match &a.content {
        AttachmentContent::Message(sub) => Some(sub),
        AttachmentContent::Data(_) => None,
    });
    assert_eq!(embedded.map(|s| s.subject.as_str()), Some("Inner note"));
    // read_item convenience.
    let it = msg::read_item(&msg::write(&m).unwrap(), FolderId(1), DateTime::default()).unwrap();
    assert_eq!(it.subject, m.subject);
}

#[test]
fn mail_item_without_source_writes() {
    let item = MailItem {
        subject: "Draft".into(),
        to: vec![Mailbox::new(None, "x@example.com")],
        is_draft: true,
        body: mailcraft_model::Body {
            text: Some("draft text".into()),
            format: BodyFormat::PlainText,
            html: None,
        },
        ..Default::default()
    };
    let back = msg::read(&msg::write_item(&item).unwrap()).unwrap();
    assert!(back.unsent);
    assert_eq!(back.body_text.as_deref(), Some("draft text"));
    assert_eq!(back.to, item.to);
}

/// Builds a .msg by hand: 8-bit strings in code page 1252, an RTF-only body, transport headers.
fn handmade() -> Vec<u8> {
    let mut cf = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut props = vec![0u8; 32];
    let mut entry = |tag: u32, value: u64| {
        props.extend_from_slice(&tag.to_le_bytes());
        props.extend_from_slice(&6u32.to_le_bytes());
        props.extend_from_slice(&value.to_le_bytes());
    };
    entry(0x3FFD_0003, 1252); // message code page
    entry(0x0E07_0003, 0x8); // unsent
    entry(0x0017_0003, 0); // low importance
    entry(0x0039_0040, (1_791_000_000u64 + 11_644_473_600) * 10_000_000);
    let streams: Vec<(&str, Vec<u8>)> = vec![
        ("/__substg1.0_0037001E", b"Caf\xe9 menu".to_vec()),
        ("/__substg1.0_001A001E", b"IPM.Note".to_vec()),
        ("/__substg1.0_0C1A001E", b"Zo\xeb".to_vec()),
        ("/__substg1.0_0C1F001E", b"/O=EXAMPLE/OU=X/CN=ZOE".to_vec()),
        ("/__substg1.0_0C1E001E", b"EX".to_vec()),
        ("/__substg1.0_5D01001E", b"zoe@example.com".to_vec()),
        (
            "/__substg1.0_007D001E",
            b"From: Zoe <zoe@example.com>\r\nTo: a@example.com\r\nMessage-ID: <hdr@example.com>\r\n".to_vec(),
        ),
        (
            "/__substg1.0_10090102",
            rtf::compress(b"{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}Plain \\b rich\\b0  text\\par next}"),
        ),
    ];
    {
        let mut s = cf.create_stream("/__properties_version1.0").unwrap();
        s.write_all(&props).unwrap();
    }
    for (path, data) in streams {
        let mut s = cf.create_stream(path).unwrap();
        s.write_all(&data).unwrap();
    }
    cf.create_storage("/__recip_version1.0_#00000000").unwrap();
    {
        let mut rp = vec![0u8; 8];
        rp.extend_from_slice(&0x0C15_0003u32.to_le_bytes());
        rp.extend_from_slice(&6u32.to_le_bytes());
        rp.extend_from_slice(&2u64.to_le_bytes());
        let mut s = cf.create_stream("/__recip_version1.0_#00000000/__properties_version1.0").unwrap();
        s.write_all(&rp).unwrap();
        let mut s = cf.create_stream("/__recip_version1.0_#00000000/__substg1.0_3001001E").unwrap();
        s.write_all(b"Ren\xe9").unwrap();
        let mut s = cf.create_stream("/__recip_version1.0_#00000000/__substg1.0_3003001E").unwrap();
        s.write_all(b"rene@example.org").unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

#[test]
fn reads_8bit_strings_rtf_body_and_header_fallback() {
    let m = msg::read(&handmade()).unwrap();
    assert_eq!(m.subject, "Caf\u{e9} menu");
    assert_eq!(m.from, Some(Mailbox::new(Some("Zo\u{eb}"), "zoe@example.com")));
    assert_eq!(m.cc, vec![Mailbox::new(Some("Ren\u{e9}"), "rene@example.org")]);
    assert_eq!(m.to, vec![Mailbox::new(None, "a@example.com")]);
    assert_eq!(m.message_id.as_deref(), Some("hdr@example.com"));
    assert!(m.unsent && !m.read);
    assert_eq!(m.importance, Importance::Low);
    assert_eq!(m.sent.map(|t| t.unix), Some(1_791_000_000));
    assert_eq!(m.body_format, BodyFormat::RichText);
    assert_eq!(m.body_text.as_deref(), Some("Plain rich text\nnext"));
    assert!(m.body_rtf.is_some());
    let item = m.to_mail_item(FolderId(1), DateTime::default());
    assert_eq!(item.body.format, BodyFormat::RichText);
    assert!(item.is_draft);
}

#[test]
fn rejects_non_msg() {
    assert!(msg::read(b"not a compound file").is_err());
    let mut bad = msg::CFB_SIGNATURE.to_vec();
    bad.extend_from_slice(&[0u8; 600]);
    assert!(msg::read(&bad).is_err());
    // A valid compound file that is not an Outlook item.
    let mut cf = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    cf.create_stream("/other").unwrap().write_all(b"x").unwrap();
    cf.flush().unwrap();
    assert!(msg::read(&cf.into_inner().into_inner()).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn msg_never_panics_on_junk(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = msg::read(&data);
        let mut with_sig = msg::CFB_SIGNATURE.to_vec();
        with_sig.extend_from_slice(&data);
        let _ = msg::read(&with_sig);
    }

    #[test]
    fn msg_never_panics_on_damaged_files(edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..40)) {
        let mut bytes = msg::write(&sample()).unwrap();
        for (i, b) in edits {
            let at = i.index(bytes.len());
            bytes[at] = b;
        }
        let _ = msg::read(&bytes);
        let mut hand = handmade();
        let len = hand.len();
        hand.truncate(len / 2);
        let _ = msg::read(&hand);
    }

    #[test]
    fn rtf_never_panics(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = rtf::decompress(&data);
        let _ = rtfhtml::extract_html(&data);
        let _ = rtfhtml::extract_text(&data);
        let mut head = b"{\\rtf1\\fromhtml1 ".to_vec();
        head.extend_from_slice(&data);
        let _ = rtfhtml::extract_html(&head);
    }

    #[test]
    fn rtf_compression_round_trips(data in proptest::collection::vec(prop_oneof![Just(b'a'), Just(b'{'), Just(b'\\'), any::<u8>()], 0..3000)) {
        prop_assert_eq!(rtf::decompress(&rtf::compress(&data)).unwrap(), data);
    }

    #[test]
    fn msg_text_round_trips(subject in "[ -~\u{e9}\u{4e2d}\u{1F600}]{0,60}", body in "[ -~\n\u{e9}]{0,200}", cats in proptest::collection::vec("[a-zA-Z ]{1,10}", 0..3)) {
        let m = MsgMessage {
            subject: subject.clone(),
            body_text: Some(body.clone()),
            categories: cats.iter().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect(),
            to: vec![Mailbox::new(Some(&subject), "x@example.com")],
            ..Default::default()
        };
        let back = msg::read(&msg::write(&m).unwrap()).unwrap();
        prop_assert_eq!(back.subject, m.subject);
        // An empty body is not stored ([MS-OXMSG] 2.4.2.2: string streams hold at least one character).
        prop_assert_eq!(back.body_text, m.body_text.filter(|b| !b.is_empty()));
        prop_assert_eq!(back.categories, m.categories);
    }
}
