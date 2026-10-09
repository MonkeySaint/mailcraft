//! `mail.reply`, `mail.replyAll`, `mail.forward`, `mail.forwardAsAttachment`, `mail.replyWithMeeting`
//! (Home › Respond; `plan/outlook/reply-forward.md`).
//!
//! A response is a [`ComposeState`] with the "RE: "/"FW: " subject, the recipients and a body of two empty
//! paragraphs followed by the quoted header block and the original. Plain-text originals get the
//! "-----Original Message-----" separator and plain header lines; HTML (and rich text) originals get a
//! horizontal rule and bold labels. The original's format is kept. Responses open inline in the reading pane
//! unless `{"window": true}`. The source item's replied/forwarded state is set by the send command
//! (`ComposeState::source` + `kind`), not here.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::mail::{
    account_from, body_doc, item_account, item_source, my_addresses, need_items, need_one, open_dialog,
    primary_item, source_attachments,
};
use crate::cmd::compose::field_mailbox;
use crate::cmd::util::opt_bool;
use crate::compose::{ComposeAttachment, ComposeKind, ComposeState, InlineImage};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_model::subject::{forward_subject, reply_subject};
use mailcraft_model::{
    Address, Block, BodyFormat, Importance, Inline, ItemId, ListTime, MailItem, ParaStyle, TextStyle,
};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "mail.reply",
            label: "Reply",
            location: "Home › Respond",
            shortcut: "Mod+R",
            params: r#"{"window"?:bool,"ids"?:[number]}"#,
            mutates: false,
            enabled: need_one,
            run: reply,
        },
        CommandSpec {
            id: "mail.replyAll",
            label: "Reply All",
            location: "Home › Respond",
            shortcut: "Mod+Shift+R",
            params: r#"{"window"?:bool,"ids"?:[number]}"#,
            mutates: false,
            enabled: need_one,
            run: reply_all,
        },
        CommandSpec {
            id: "mail.forward",
            label: "Forward",
            location: "Home › Respond",
            shortcut: "Mod+F",
            params: r#"{"window"?:bool,"ids"?:[number]}"#,
            mutates: false,
            enabled: need_one,
            run: forward,
        },
        CommandSpec {
            id: "mail.forwardAsAttachment",
            label: "Forward as Attachment",
            location: "Home › Respond › More",
            shortcut: "",
            params: r#"{"ids"?:[number]}"#,
            mutates: false,
            enabled: need_items,
            run: forward_as_attachment,
        },
        CommandSpec {
            id: "mail.replyWithMeeting",
            label: "Reply with Meeting",
            location: "Home › Respond",
            shortcut: "",
            params: r#"{"ids"?:[number]}"#,
            mutates: false,
            enabled: need_one,
            run: reply_with_meeting,
        },
    ]
}

fn reply(s: &mut Session, p: &Value) -> CmdResult {
    respond(s, p, ComposeKind::Reply)
}

fn reply_all(s: &mut Session, p: &Value) -> CmdResult {
    respond(s, p, ComposeKind::ReplyAll)
}

fn forward(s: &mut Session, p: &Value) -> CmdResult {
    respond(s, p, ComposeKind::Forward)
}

fn respond(s: &mut Session, p: &Value, kind: ComposeKind) -> CmdResult {
    let Some(id) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let inline = !opt_bool(p, "window").unwrap_or(false);
    let Some(c) = build_response(s, id, kind, inline) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let compose = s.open_compose(c);
    Ok(json!({"compose": compose, "inline": inline}))
}

/// "Name <addr>; Name <addr>" as shown in the quoted header block.
fn join_addresses<'a>(list: impl IntoIterator<Item = &'a Address>) -> String {
    list.into_iter().map(Address::to_display_string).collect::<Vec<_>>().join("; ")
}

/// "Name <addr>; Name <addr>" as written into a recipient field (names that would split the entry quoted).
fn join_field<'a>(list: impl IntoIterator<Item = &'a Address>) -> String {
    list.into_iter().map(field_mailbox).collect::<Vec<_>>().join("; ")
}

/// Who a reply goes to: the Reply-To addresses when present, else the sender.
fn reply_targets(m: &MailItem) -> Vec<Address> {
    if !m.reply_to.is_empty() {
        return m.reply_to.clone();
    }
    m.from.clone().or_else(|| m.sender.clone()).into_iter().collect()
}

/// (To, Cc) addresses for a response of `kind`. Reply: the sender. Reply All: the sender plus the original
/// To, and the original Cc, without the user's own addresses and with duplicates collapsed (the sender is
/// kept even when it is the user, so a reply-all to one's own message goes back to oneself once).
/// Forward: empty.
fn response_lists(s: &Session, m: &MailItem, kind: ComposeKind) -> (Vec<Address>, Vec<Address>) {
    match kind {
        ComposeKind::Reply => (reply_targets(m), Vec::new()),
        ComposeKind::ReplyAll => {
            let me = my_addresses(s);
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut to: Vec<Address> = Vec::new();
            for a in reply_targets(m) {
                if seen.insert(a.addr.trim().to_lowercase()) {
                    to.push(a);
                }
            }
            let mut take = |list: &[Address], out: &mut Vec<Address>| {
                for a in list {
                    let key = a.addr.trim().to_lowercase();
                    if key.is_empty() || me.contains(&key) || !seen.insert(key) {
                        continue;
                    }
                    out.push(a.clone());
                }
            };
            take(&m.to, &mut to);
            let mut cc: Vec<Address> = Vec::new();
            take(&m.cc, &mut cc);
            (to, cc)
        }
        _ => (Vec::new(), Vec::new()),
    }
}

/// (To, Cc) recipient-field text for a response of `kind` (see [`response_lists`]).
pub(crate) fn response_recipients(s: &Session, m: &MailItem, kind: ComposeKind) -> (String, String) {
    let (to, cc) = response_lists(s, m, kind);
    (join_field(&to), join_field(&cc))
}

/// Images embedded in an HTML original (parts referenced by `cid:` from its body), so the quoted original
/// keeps showing them and the sent response carries them. Empty when no source is kept.
fn source_inline_images(m: &MailItem) -> Vec<InlineImage> {
    let Some(raw) = m.raw.as_deref() else {
        return Vec::new();
    };
    let Ok(msg) = mailcraft_mime::parse(raw) else {
        return Vec::new();
    };
    msg.attachments()
        .into_iter()
        .filter(|a| a.inline)
        .filter_map(|a| {
            let content_id = a.content_id.filter(|c| !c.trim().is_empty())?;
            Some(InlineImage { content_id, filename: a.filename, mime_type: a.mime_type, data: a.data })
        })
        .collect()
}

fn importance_label(i: Importance) -> Option<&'static str> {
    match i {
        Importance::High => Some("High"),
        Importance::Low => Some("Low"),
        Importance::Normal => None,
    }
}

/// Header lines of the quoted block as (label, value): From, Sent, To, Cc (when present), Subject and
/// Importance (when not normal).
fn header_lines(s: &Session, m: &MailItem) -> Vec<(&'static str, String)> {
    let from = m.from.as_ref().or(m.sender.as_ref()).map(Address::to_display_string).unwrap_or_default();
    let sent = ListTime::long(m.sent.unwrap_or(m.received), s.tz_offset_min);
    let mut lines = vec![("From:", from), ("Sent:", sent), ("To:", join_addresses(&m.to))];
    if !m.cc.is_empty() {
        lines.push(("Cc:", join_addresses(&m.cc)));
    }
    lines.push(("Subject:", m.subject.clone()));
    if let Some(imp) = importance_label(m.importance) {
        lines.push(("Importance:", imp.to_string()));
    }
    lines
}

/// Body of a response: two empty paragraphs (the caret goes in the first), then the quoted header block,
/// a blank line and the original.
pub fn quoted_body(s: &Session, m: &MailItem) -> mailcraft_model::Doc {
    let mut blocks = vec![Block::text_paragraph(""), Block::text_paragraph("")];
    let lines = header_lines(s, m);
    if m.body.format == BodyFormat::PlainText {
        blocks.push(Block::text_paragraph("-----Original Message-----"));
        for (label, value) in lines {
            blocks.push(Block::text_paragraph(&format!("{label} {value}")));
        }
        blocks.push(Block::text_paragraph(""));
        let text = m.body.text.clone().unwrap_or_else(|| m.body_text());
        blocks.extend(mailcraft_model::Doc::from_plain_text(&text).blocks);
    } else {
        blocks.push(Block::Rule);
        for (label, value) in lines {
            let bold = TextStyle { bold: Some(true), ..Default::default() };
            blocks.push(Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![
                    Inline::Text { text: label.to_string(), style: bold },
                    Inline::Text { text: format!(" {value}"), style: TextStyle::default() },
                ],
            });
        }
        blocks.push(Block::text_paragraph(""));
        blocks.extend(body_doc(m).blocks);
    }
    mailcraft_model::Doc { blocks, ..Default::default() }
}

/// Builds the compose state for a Reply / Reply All / Forward of item `id` (None when it does not exist).
pub fn build_response(s: &Session, id: ItemId, kind: ComposeKind, inline: bool) -> Option<ComposeState> {
    let m = s.mailbox.mail.get(&id)?;
    let (to, cc) = response_recipients(s, m, kind);
    let subject =
        if kind == ComposeKind::Forward { forward_subject(&m.subject) } else { reply_subject(&m.subject) };
    let attachments = if kind == ComposeKind::Forward { source_attachments(m) } else { Vec::new() };
    let inline_images =
        if m.body.format == BodyFormat::PlainText { Vec::new() } else { source_inline_images(m) };
    Some(ComposeState {
        kind,
        source: Some(id),
        inline,
        to,
        cc,
        from: account_from(s, item_account(s, id)),
        subject,
        body: quoted_body(s, m),
        format: m.body.format,
        importance: Importance::Normal,
        attachments,
        inline_images,
        ..Default::default()
    })
}

/// File name for a message attached as .eml: its subject with characters unsafe in file names replaced.
fn eml_name(subject: &str) -> String {
    let clean: String = subject
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    let base = if clean.is_empty() { "message" } else { clean };
    format!("{base}.eml")
}

/// Forward as Attachment: a new message window with the selected messages attached as .eml items; the
/// subject is "FW: " + the first message's subject.
fn forward_as_attachment(s: &mut Session, p: &Value) -> CmdResult {
    let ids = super::mail::mail_targets(s, p);
    let Some(first) = ids.first().copied() else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let mut attachments = Vec::new();
    let mut subject = String::new();
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get(id) {
            if subject.is_empty() {
                subject = forward_subject(&m.subject);
            }
            attachments.push(ComposeAttachment {
                filename: eml_name(&m.subject),
                mime_type: "message/rfc822".into(),
                data: item_source(m),
            });
        }
    }
    let c = ComposeState {
        kind: ComposeKind::Forward,
        source: Some(first),
        inline: false,
        from: account_from(s, item_account(s, first)),
        subject,
        format: BodyFormat::Html,
        attachments,
        ..Default::default()
    };
    let compose = s.open_compose(c);
    Ok(json!({"compose": compose, "attached": ids.len()}))
}

/// Reply with Meeting: asks for a new meeting request addressed to the sender and the other recipients
/// (without the user), with the message's subject and text.
fn reply_with_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let Some(id) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(m) = s.mailbox.mail.get(&id) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let (to, cc) = response_lists(s, m, ComposeKind::ReplyAll);
    let attendees: Vec<String> = to.iter().chain(&cc).map(field_mailbox).collect();
    let params = json!({
        "source": id.0,
        "subject": m.subject,
        "attendees": attendees,
        "body": m.body_text(),
    });
    open_dialog(s, "newMeeting", params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Body, DateTime, FolderRole, Mailbox};

    fn addr(name: &str, a: &str) -> Address {
        Address::new(Some(name), a)
    }

    /// Thursday, October 8, 2026 4:30 AM UTC.
    const SENT: i64 = 20734 * 86_400 + 4 * 3600 + 30 * 60;

    struct Fx {
        s: Session,
        plain: ItemId,
        html: ItemId,
    }

    fn fx() -> Fx {
        let mut mb = Mailbox::new_local("Test");
        if let Some(acc) = mb.accounts.first_mut() {
            acc.address = "test.user@example.net".into();
            acc.sender_name = "Test User".into();
        }
        let account = mb.default_account().map(|a| a.id).unwrap();
        let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
        let t = DateTime { unix: SENT, offset_min: 0 };
        let plain = mb.add_mail(MailItem {
            folder: inbox,
            subject: "URGENT: server room access".into(),
            from: Some(addr("Alice Rivera", "alice@example.com")),
            to: vec![addr("Test User", "test.user@example.net")],
            cc: vec![addr("Bob Chen", "bob.chen@example.org"), addr("Carol Okafor", "carol@example.com")],
            sent: Some(t),
            received: t,
            importance: Importance::High,
            body: Body {
                format: BodyFormat::PlainText,
                text: Some("The badge reader is down. Please use the side door today.".into()),
                html: None,
            },
            ..Default::default()
        });
        let html = mb.add_mail(MailItem {
            folder: inbox,
            subject: "Re: Newsletter".into(),
            from: Some(addr("News Desk", "news@example.org")),
            to: vec![addr("Test User", "test.user@example.net"), addr("Dana", "dana@example.com")],
            sent: Some(t),
            received: t,
            body: Body {
                format: BodyFormat::Html,
                text: None,
                html: Some("<h1>Weekly</h1><p>Hello <b>there</b></p>".into()),
            },
            ..Default::default()
        });
        let mut s = Session::new(mb);
        s.selection.folder = inbox;
        s.selection.items = vec![plain];
        s.selection.focus = Some(plain);
        Fx { s, plain, html }
    }

    fn para_text(b: &Block) -> String {
        match b {
            Block::Paragraph { inlines, .. } => inlines
                .iter()
                .map(|i| match i {
                    Inline::Text { text, .. } => text.clone(),
                    _ => String::new(),
                })
                .collect(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn registered() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "mail.reply",
                "mail.replyAll",
                "mail.forward",
                "mail.forwardAsAttachment",
                "mail.replyWithMeeting"
            ]
        );
    }

    #[test]
    fn disabled_without_selection() {
        let mut f = fx();
        f.s.selection.items.clear();
        f.s.selection.focus = None;
        assert!(need_one(&f.s).is_some());
        assert!(matches!(reply(&mut f.s, &Value::Null), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn reply_plain_text_layout() {
        let mut f = fx();
        let r = reply(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["inline"], true);
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.kind, ComposeKind::Reply);
        assert_eq!(c.source, Some(f.plain));
        assert!(c.inline);
        assert_eq!(c.subject, "RE: URGENT: server room access");
        assert_eq!(c.to, "Alice Rivera <alice@example.com>");
        assert_eq!(c.cc, "");
        assert_eq!(c.from, "Test User <test.user@example.net>");
        assert_eq!(c.format, BodyFormat::PlainText);
        assert_eq!(c.title(), "RE: URGENT: server room access - Message (Plain Text)");
        let text = c.body.to_plain_text();
        let expected = "\n\n-----Original Message-----\n\
From: Alice Rivera <alice@example.com>\n\
Sent: Thursday, October 8, 2026 4:30 AM\n\
To: Test User <test.user@example.net>\n\
Cc: Bob Chen <bob.chen@example.org>; Carol Okafor <carol@example.com>\n\
Subject: URGENT: server room access\n\
Importance: High\n\
\n\
The badge reader is down. Please use the side door today.";
        assert_eq!(text, expected);
    }

    #[test]
    fn window_param_pops_out() {
        let mut f = fx();
        let r = reply(&mut f.s, &json!({"window": true})).unwrap();
        assert_eq!(r["inline"], false);
        assert!(!f.s.composes[&r["compose"].as_u64().unwrap()].inline);
    }

    #[test]
    fn reply_all_collapses_me_and_duplicates() {
        let mut f = fx();
        let r = reply_all(&mut f.s, &Value::Null).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.kind, ComposeKind::ReplyAll);
        assert_eq!(c.to, "Alice Rivera <alice@example.com>");
        assert_eq!(c.cc, "Bob Chen <bob.chen@example.org>; Carol Okafor <carol@example.com>");

        // Sender = me, me in To and Cc: To = me once, Cc empty.
        let me = addr("Test User", "test.user@example.net");
        let m = MailItem {
            from: Some(me.clone()),
            to: vec![me.clone()],
            cc: vec![addr("TEST USER", "Test.User@example.net")],
            ..Default::default()
        };
        let (to, cc) = response_recipients(&f.s, &m, ComposeKind::ReplyAll);
        assert_eq!(to, "Test User <test.user@example.net>");
        assert_eq!(cc, "");

        // Duplicates between To and Cc are dropped from Cc.
        let m = MailItem {
            from: Some(addr("A", "a@example.com")),
            to: vec![addr("B", "b@example.com"), addr("A again", "A@example.com")],
            cc: vec![addr("B", "b@example.com"), addr("C", "c@example.com")],
            ..Default::default()
        };
        let (to, cc) = response_recipients(&f.s, &m, ComposeKind::ReplyAll);
        assert_eq!(to, "A <a@example.com>; B <b@example.com>");
        assert_eq!(cc, "C <c@example.com>");
    }

    #[test]
    fn reply_uses_reply_to_when_present() {
        let f = fx();
        let m = MailItem {
            from: Some(addr("A", "a@example.com")),
            reply_to: vec![addr("List", "list@example.org")],
            ..Default::default()
        };
        assert_eq!(response_recipients(&f.s, &m, ComposeKind::Reply).0, "List <list@example.org>");
    }

    #[test]
    fn forward_html_layout() {
        let mut f = fx();
        f.s.selection.focus = Some(f.html);
        let r = forward(&mut f.s, &Value::Null).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.kind, ComposeKind::Forward);
        assert_eq!(c.subject, "FW: Newsletter");
        assert_eq!((c.to.as_str(), c.cc.as_str()), ("", ""));
        assert_eq!(c.format, BodyFormat::Html);
        let b = &c.body.blocks;
        assert_eq!(para_text(&b[0]), "");
        assert_eq!(para_text(&b[1]), "");
        assert_eq!(b[2], Block::Rule);
        assert_eq!(para_text(&b[3]), "From: News Desk <news@example.org>");
        match &b[3] {
            Block::Paragraph { inlines, .. } => match &inlines[0] {
                Inline::Text { style, .. } => assert_eq!(style.bold, Some(true)),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(para_text(&b[4]), "Sent: Thursday, October 8, 2026 4:30 AM");
        assert_eq!(para_text(&b[5]), "To: Test User <test.user@example.net>; Dana <dana@example.com>");
        assert_eq!(para_text(&b[6]), "Subject: Re: Newsletter");
        assert_eq!(para_text(&b[7]), "");
        // The original HTML follows (heading kept).
        assert!(b[8..].iter().any(|blk| matches!(blk, Block::Heading { .. })));
        assert!(c.body.to_plain_text().contains("Hello there"));
    }

    #[test]
    fn forward_includes_attachments_from_source() {
        let mut f = fx();
        let raw = mailcraft_mime::MessageBuilder {
            from: Some(addr("Alice", "alice@example.com")),
            subject: "Files".into(),
            text: Some("See attached.".into()),
            date: Some(DateTime { unix: SENT, offset_min: 0 }),
            attachments: vec![mailcraft_mime::AttachmentSpec {
                filename: "plan.txt".into(),
                mime_type: "text/plain".into(),
                data: b"step one".to_vec(),
                content_id: None,
            }],
            ..Default::default()
        }
        .build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, DateTime::default());
        item.folder = f.s.selection.folder;
        let id = f.s.mailbox.add_mail(item);
        let r = forward(&mut f.s, &json!({"ids": [id.0]})).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.attachments.len(), 1);
        assert_eq!(c.attachments[0].filename, "plan.txt");
        assert_eq!(c.attachments[0].data, b"step one");
        // Replies do not carry attachments.
        let r = reply(&mut f.s, &json!({"ids": [id.0]})).unwrap();
        assert!(f.s.composes[&r["compose"].as_u64().unwrap()].attachments.is_empty());
    }

    #[test]
    fn forward_as_attachment_attaches_eml() {
        let mut f = fx();
        f.s.selection.items = vec![f.plain, f.html];
        let r = forward_as_attachment(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["attached"], 2);
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert!(!c.inline);
        assert_eq!(c.subject, "FW: URGENT: server room access");
        assert_eq!(c.attachments.len(), 2);
        assert_eq!(c.attachments[0].filename, "URGENT_ server room access.eml");
        assert_eq!(c.attachments[0].mime_type, "message/rfc822");
        let parsed = mailcraft_mime::parse(&c.attachments[0].data).unwrap();
        assert_eq!(parsed.subject(), "URGENT: server room access");
        assert_eq!(eml_name("  "), "message.eml");
    }

    #[test]
    fn reply_with_meeting_opens_meeting_dialog() {
        let mut f = fx();
        let r = reply_with_meeting(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["dialog"], "newMeeting");
        match f.s.notices.last() {
            Some(crate::Notice::OpenDialog(name, params)) => {
                assert_eq!(name, "newMeeting");
                assert_eq!(params["subject"], "URGENT: server room access");
                let att: Vec<&str> =
                    params["attendees"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
                assert_eq!(
                    att,
                    [
                        "Alice Rivera <alice@example.com>",
                        "Bob Chen <bob.chen@example.org>",
                        "Carol Okafor <carol@example.com>"
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// Session with a sending account (compose tests' fixture) and one Inbox message from `from`.
    fn sending_fx(from: Address, cc: Vec<Address>) -> (Session, ItemId) {
        let mut s = crate::cmd::compose::tests::session();
        let inbox = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        let id = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Badges".into(),
            from: Some(from),
            to: vec![addr("Me Example", "me@example.com")],
            cc,
            message_id: Some("badges@example.com".into()),
            body: Body { format: BodyFormat::PlainText, text: Some("Hi".into()), html: None },
            ..Default::default()
        });
        s.selection.folder = inbox;
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        (s, id)
    }

    #[test]
    fn reply_to_name_with_comma_or_semicolon_can_be_sent() {
        // Directory-style names ("Last, First") are common; the reply must still send to that address.
        let (mut s, src) = sending_fx(
            addr("Rivera, Alice", "alice@example.com"),
            vec![addr("Chen; Bob", "bob@example.org"), addr("Carol", "carol@example.com")],
        );
        let r = s.run("mail.replyAll", &Value::Null).unwrap();
        let cid = r["compose"].as_u64().unwrap();
        assert_eq!(s.composes[&cid].to, "Rivera, Alice <alice@example.com>");
        let r = s.run("compose.send", &json!({"compose": cid})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        let sent = &s.mailbox.mail[&ItemId(r["item"].as_u64().unwrap())];
        let to: Vec<(&str, Option<&str>)> =
            sent.to.iter().map(|a| (a.addr.as_str(), a.name.as_deref())).collect();
        assert_eq!(to, [("alice@example.com", Some("Rivera, Alice"))]);
        let cc: Vec<&str> = sent.cc.iter().map(|a| a.addr.as_str()).collect();
        assert_eq!(cc, ["bob@example.org", "carol@example.com"]);
        assert!(matches!(s.mailbox.mail[&src].response, mailcraft_model::ResponseState::RepliedAll(_)));
    }

    #[test]
    fn reply_with_meeting_keeps_names_with_separators_whole() {
        let (mut s, _) = sending_fx(addr("Chen; Bob", "bob@example.org"), Vec::new());
        reply_with_meeting(&mut s, &Value::Null).unwrap();
        match s.notices.last() {
            Some(crate::Notice::OpenDialog(_, params)) => {
                let att: Vec<&str> =
                    params["attendees"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
                assert_eq!(att.len(), 1, "{att:?}");
                let parsed = crate::cmd::compose::parse_recipients(att[0]).unwrap();
                assert_eq!(parsed.len(), 1);
                assert_eq!(parsed[0].mailboxes()[0].addr, "bob@example.org");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn html_response_keeps_inline_images() {
        let mut f = fx();
        let raw = mailcraft_mime::MessageBuilder {
            from: Some(addr("News Desk", "news@example.org")),
            subject: "Logo".into(),
            html: Some("<p>Our logo: <img src=\"cid:logo@x\" alt=\"Logo\"></p>".into()),
            text: Some("Our logo".into()),
            date: Some(DateTime { unix: SENT, offset_min: 0 }),
            attachments: vec![
                mailcraft_mime::AttachmentSpec {
                    filename: "logo.png".into(),
                    mime_type: "image/png".into(),
                    data: b"PNGDATA".to_vec(),
                    content_id: Some("logo@x".into()),
                },
                mailcraft_mime::AttachmentSpec {
                    filename: "report.pdf".into(),
                    mime_type: "application/pdf".into(),
                    data: b"PDF".to_vec(),
                    content_id: None,
                },
            ],
            ..Default::default()
        }
        .build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, DateTime::default());
        item.folder = f.s.selection.folder;
        let id = f.s.mailbox.add_mail(item);
        for (run, kind) in [(reply as fn(&mut Session, &Value) -> CmdResult, "reply"), (forward, "forward")] {
            let r = run(&mut f.s, &json!({"ids": [id.0]})).unwrap();
            let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
            assert_eq!(c.inline_images.len(), 1, "{kind}");
            assert_eq!(c.inline_images[0].content_id, "logo@x");
            assert_eq!(c.inline_images[0].data, b"PNGDATA");
            assert!(c.images().iter().any(|(src, _)| src == "cid:logo@x"), "{kind}");
            // The inline image is not also listed as a file attachment.
            let names: Vec<&str> = c.attachments.iter().map(|a| a.filename.as_str()).collect();
            let expected: &[&str] = if kind == "forward" { &["report.pdf"] } else { &[] };
            assert_eq!(names, expected, "{kind}");
            // The built message carries the image part for the cid reference.
            let b = crate::cmd::compose::build_message(&f.s, c, None, false);
            assert!(b.attachments.iter().any(|a| a.content_id.as_deref() == Some("logo@x")), "{kind}");
        }
    }

    #[test]
    fn session_run_reply() {
        let mut f = fx();
        let r = f.s.run("mail.reply", &Value::Null).unwrap();
        assert!(r["compose"].as_u64().is_some());
        assert!(f.s.notices.iter().any(|n| matches!(n, crate::Notice::Compose(_))));
    }
}
