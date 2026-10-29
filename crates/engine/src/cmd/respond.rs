//! `mail.reply`, `mail.replyAll`, `mail.forward`, `mail.forwardAsAttachment`, `mail.replyWithMeeting`
//! (Home › Respond; `plan/reference/reply-forward.md`).
//!
//! A response is a [`ComposeState`] with the "RE: "/"FW: " subject, the recipients and a body of two empty
//! paragraphs followed by the quoted header block and the original. Plain-text originals get the
//! "-----Original Message-----" separator and plain header lines; HTML (and rich text) originals get a
//! horizontal rule and bold labels. The original's format is kept. Responses open inline in the reading pane
//! unless `{"window": true}`. The source item's replied/forwarded state is set by the send command
//! (`ComposeState::source` + `kind`), not here.
//!
//! Options › Mail › Replies and forwards shape the body: the original text included (default), left out
//! (replies only), attached as an .eml item, indented, or prefixed line by line ("> " in plain text, a quote
//! bar in HTML); "Preface comments with" marks what the user types inside the original
//! ([`ComposeState::type_text`]). Save messages › "Save forwarded messages" and "save the reply in the same
//! folder" pick where the sent copy goes; "Ignore original message text" leaves the original out of
//! proofing.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::mail::{
    account_from, body_doc, item_account, item_source, my_addresses, need_items, need_one, open_dialog,
    primary_item, source_attachments,
};
use crate::cmd::compose::field_mailbox;
use crate::cmd::util::opt_bool;
use crate::compose::{ComposeAttachment, ComposeKind, ComposeState, InlineImage, SaveSentTo};
use crate::settings::OriginalText;
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_model::subject::{forward_subject, reply_subject};
use mailcraft_model::{
    Address, Block, BodyFormat, FolderKind, FolderRole, Importance, Inline, ItemId, MailItem, ParaStyle,
    TextStyle,
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

/// Forward: one message forwards with its text included; several selected messages go out as one new
/// message with them attached, as Forward as Attachment does.
fn forward(s: &mut Session, p: &Value) -> CmdResult {
    if super::mail::mail_targets(s, p).len() > 1 {
        return forward_as_attachment(s, p);
    }
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
    let c = crate::compose::with_default_signature(&s.mailbox, c);
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
fn source_inline_images(s: &Session, m: &MailItem) -> Vec<InlineImage> {
    // The kept source, else the one in the store (sources leave memory once stored).
    let Some(raw) = crate::cmd::compose::stored_raw(s, m) else {
        return Vec::new();
    };
    let Ok(msg) = mailcraft_mime::parse(&raw) else {
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

fn importance_label(i: Importance, l: &crate::intl::HeaderLabels) -> Option<&'static str> {
    match i {
        Importance::High => Some(l.high),
        Importance::Low => Some(l.low),
        Importance::Normal => None,
    }
}

/// Header lines of the quoted block as (label, value): From, Sent, To, Cc (when present), Subject and
/// Importance (when not normal). The words are English unless Options › Advanced › "Use English for message
/// headers on replies and forwards" is off ([`crate::intl::header_labels`]).
fn header_lines(s: &Session, m: &MailItem) -> Vec<(&'static str, String)> {
    let l = crate::intl::header_labels(&s.settings);
    let from = m.from.as_ref().or(m.sender.as_ref()).map(Address::to_display_string).unwrap_or_default();
    let sent = crate::intl::sent_date(m.sent.unwrap_or(m.received), s.tz_offset_min, l);
    let mut lines = vec![(l.from, from), (l.sent, sent), (l.to, join_addresses(&m.to))];
    if !m.cc.is_empty() {
        lines.push((l.cc, join_addresses(&m.cc)));
    }
    lines.push((l.subject, m.subject.clone()));
    if let Some(imp) = importance_label(m.importance, l) {
        lines.push((l.importance, imp.to_string()));
    }
    lines
}

/// Body of a response: two empty paragraphs (the caret goes in the first), then the quoted header block,
/// a blank line and the original ("Include original message text").
pub fn quoted_body(s: &Session, m: &MailItem) -> mailcraft_model::Doc {
    quoted_body_as(s, m, OriginalText::Include, "")
}

/// Left indent of an indented original ("Include and indent original message text"), px.
const INDENT_PX: f32 = 36.0;

/// Body of a response for one of the Options › Mail › Replies and forwards choices: two empty paragraphs,
/// then (when the original's text goes in) the header block, a blank line and the original: as it is, indented
/// (a tab before each plain-text line), or with `prefix` before each plain-text line (a quote bar in HTML).
/// The header block itself is never indented or prefixed.
pub fn quoted_body_as(s: &Session, m: &MailItem, mode: OriginalText, prefix: &str) -> mailcraft_model::Doc {
    let mut blocks = vec![Block::text_paragraph(""), Block::text_paragraph("")];
    if !mode.quotes() {
        return mailcraft_model::Doc { blocks, ..Default::default() };
    }
    let lines = header_lines(s, m);
    if m.body.format == BodyFormat::PlainText {
        blocks.push(Block::text_paragraph(crate::intl::header_labels(&s.settings).original));
        for (label, value) in lines {
            blocks.push(Block::text_paragraph(&format!("{label} {value}")));
        }
        blocks.push(Block::text_paragraph(""));
        let text = m.body.text.clone().unwrap_or_else(|| m.body_text());
        let lead = match mode {
            OriginalText::Indent => "\t",
            OriginalText::Prefix => prefix,
            _ => "",
        };
        if lead.is_empty() {
            blocks.extend(mailcraft_model::Doc::from_plain_text(&text).blocks);
        } else {
            let text = text.replace("\r\n", "\n");
            let text = text.strip_suffix('\n').unwrap_or(&text);
            for line in text.split('\n') {
                let quoted = format!("{lead}{line}");
                // An empty line gets the prefix without its trailing blank ("> " → ">").
                let quoted = if line.is_empty() { quoted.trim_end().to_owned() } else { quoted };
                blocks.push(Block::text_paragraph(&quoted));
            }
        }
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
        let original = body_doc(m).blocks;
        match mode {
            OriginalText::Indent => blocks.push(Block::Container {
                style: ParaStyle { indent_px: INDENT_PX, ..Default::default() },
                blocks: original,
            }),
            OriginalText::Prefix => blocks.push(Block::Quote { blocks: original }),
            _ => blocks.extend(original),
        }
    }
    mailcraft_model::Doc { blocks, ..Default::default() }
}

/// The name a prefaced comment starts with (Options › Mail › "Preface comments with"): the name set there
/// (or General › User name), else the replying account's sender or display name. `None` when the option is
/// off or no name is known.
fn preface_name(s: &Session, id: ItemId) -> Option<String> {
    let name = s.settings.preface_name()?;
    if !name.is_empty() {
        return Some(name);
    }
    let account =
        item_account(s, id).and_then(|a| s.mailbox.account(a)).or_else(|| s.mailbox.default_account())?;
    let name = if account.sender_name.trim().is_empty() {
        account.display_name.trim()
    } else {
        account.sender_name.trim()
    };
    (!name.is_empty()).then(|| name.to_owned())
}

/// The original message as an attached .eml item with `data` as its source.
fn eml_attachment(m: &MailItem, data: Vec<u8>) -> ComposeAttachment {
    ComposeAttachment { filename: eml_name(&m.subject), mime_type: "message/rfc822".into(), data }
}

/// The message's own source as received (kept in memory or in the store's blobs), so a forwarded copy
/// keeps every original header (Received, Authentication-Results, DKIM-Signature); rebuilt from the
/// stored fields only when no source is kept.
fn received_source(s: &Session, m: &MailItem) -> Vec<u8> {
    crate::cmd::compose::stored_raw(s, m).filter(|r| !r.is_empty()).unwrap_or_else(|| item_source(m))
}

/// Most messages one forward attaches, and their largest total size.
pub(crate) const MAX_FORWARD_ITEMS: usize = 100;
pub(crate) const MAX_FORWARD_BYTES: usize = 50 * 1024 * 1024;

/// Builds the compose state for a Reply / Reply All / Forward of item `id` (None when it does not exist),
/// following Options › Mail: what happens to the original text (include, omit, attach, indent, prefix),
/// prefaced comments, proofing of the original, where the sent copy of a forward or of a reply to a message
/// outside the Inbox is saved.
pub fn build_response(s: &Session, id: ItemId, kind: ComposeKind, inline: bool) -> Option<ComposeState> {
    // An S/MIME original is answered from its opened form (decrypted text, its own attachments).
    let shown = crate::smime::display_item(s, id)?;
    let m = shown.as_ref();
    let forward = kind == ComposeKind::Forward;
    let st = &s.settings;
    let mode = st.original_text(forward);
    let (to, cc) = response_recipients(s, m, kind);
    let subject = if forward { forward_subject(&m.subject) } else { reply_subject(&m.subject) };
    let attachments = match mode {
        // The opened form of an S/MIME message is what is attached; other mail goes as received.
        OriginalText::Attach => {
            let data = match &shown {
                std::borrow::Cow::Owned(_) => item_source(m),
                std::borrow::Cow::Borrowed(_) => received_source(s, m),
            };
            vec![eml_attachment(m, data)]
        }
        _ if forward => source_attachments(s, m),
        _ => Vec::new(),
    };
    let inline_images = if m.body.format == BodyFormat::PlainText || !mode.quotes() {
        Vec::new()
    } else {
        source_inline_images(s, m)
    };
    let save_sent = sent_copy_for(s, m, kind);
    Some(ComposeState {
        kind,
        source: Some(id),
        inline,
        to,
        cc,
        from: account_from(s, item_account(s, id)),
        subject,
        body: quoted_body_as(s, m, mode, &st.plain_prefix()),
        format: m.body.format,
        importance: Importance::Normal,
        attachments,
        inline_images,
        save_sent,
        preface: if mode.quotes() { preface_name(s, id) } else { None },
        proof_original: !st.bool("mail.compose.ignoreOriginal"),
        ..Default::default()
    })
}

/// Where the sent copy of a response goes (Options › Mail › Save messages): not saved for a forward when
/// "Save forwarded messages" is off; a reply to a message outside the Inbox goes to that message's folder
/// when "When replying to a message that is not in the Inbox, save the reply in the same folder" is on
/// (and copies are saved at all). Otherwise the default (Sent Items).
fn sent_copy_for(s: &Session, m: &MailItem, kind: ComposeKind) -> SaveSentTo {
    let st = &s.settings;
    match kind {
        ComposeKind::Forward if !st.bool("mail.save.forwarded") => SaveSentTo::DoNotSave,
        ComposeKind::Reply | ComposeKind::ReplyAll
            if st.bool("mail.save.replyInSameFolder") && st.bool("mail.save.sentCopies") =>
        {
            match s.mailbox.folder(m.folder) {
                Some(f)
                    if f.kind == FolderKind::Mail
                        && !matches!(
                            f.role,
                            Some(FolderRole::Inbox | FolderRole::Outbox | FolderRole::Drafts)
                        ) =>
                {
                    SaveSentTo::Folder(f.id)
                }
                _ => SaveSentTo::Default,
            }
        }
        _ => SaveSentTo::Default,
    }
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
    // Every attachment is a full copy of its message in memory: the number and the total size are capped.
    if ids.len() > MAX_FORWARD_ITEMS {
        return Err(CmdError::Failed(format!(
            "A forward can attach at most {MAX_FORWARD_ITEMS} messages ({} are selected).",
            ids.len()
        )));
    }
    let too_big = || {
        CmdError::Failed(format!(
            "The selected messages are too large to forward together (more than {} MB).",
            MAX_FORWARD_BYTES / (1024 * 1024)
        ))
    };
    let listed: u64 =
        ids.iter().filter_map(|id| s.mailbox.mail.get(*id)).map(|m| m.size).fold(0, u64::saturating_add);
    if usize::try_from(listed).map_or(true, |n| n > MAX_FORWARD_BYTES) {
        return Err(too_big());
    }
    let mut attachments = Vec::new();
    let mut subject = String::new();
    let mut total = 0usize;
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get(*id) {
            if subject.is_empty() {
                subject = forward_subject(&m.subject);
            }
            let data = received_source(s, m);
            total = total.saturating_add(data.len());
            if total > MAX_FORWARD_BYTES {
                return Err(too_big());
            }
            attachments.push(eml_attachment(m, data));
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
        save_sent: if s.settings.bool("mail.save.forwarded") {
            SaveSentTo::Default
        } else {
            SaveSentTo::DoNotSave
        },
        ..Default::default()
    };
    // Options › Mail › Signatures: the replies/forwards signature, as for a forward with the text included.
    let c = crate::compose::with_default_signature(&s.mailbox, c);
    let compose = s.open_compose(c);
    Ok(json!({"compose": compose, "attached": ids.len()}))
}

/// Reply with Meeting: asks for a new meeting request addressed to the sender and the other recipients
/// (without the user), with the message's subject and text.
fn reply_with_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let Some(id) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(m) = s.mailbox.mail.get(id) else {
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
    fn header_block_language_follows_the_english_headers_option() {
        let mut f = fx();
        f.s.settings.set("language.authoring", &json!("fr-FR")).unwrap();
        let text = |s: &mut Session| {
            let r = reply(s, &Value::Null).unwrap();
            s.composes[&r["compose"].as_u64().unwrap()].body.to_plain_text()
        };
        // On (default): English whatever the authoring language.
        assert!(text(&mut f.s).contains("\nFrom: Alice Rivera"));
        f.s.settings.set("advanced.englishHeaders", &json!(false)).unwrap();
        let fr = text(&mut f.s);
        assert!(fr.contains("-----Message original-----\nDe : Alice Rivera <alice@example.com>\n"), "{fr}");
        assert!(fr.contains("Envoyé : jeudi 8 octobre 2026 04:30\n"), "{fr}");
        assert!(fr.contains("Objet : URGENT: server room access\nImportance : Haute"), "{fr}");
        // The quoted original is still recognised (proofing skips it; new-text checks stop at it).
        let id = f.s.run("mail.reply", &json!({})).unwrap()["compose"].as_u64().unwrap();
        assert_eq!(f.s.composes[&id].quoted_start(), Some(2));
        assert_eq!(crate::settings::new_text_of_reply("Merci\nDe : Ann\nattached"), "Merci\n");
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
    fn forward_of_several_messages_attaches_them_all() {
        let mut f = fx();
        let r = forward(&mut f.s, &json!({"ids": [f.plain.0, f.html.0]})).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(r["attached"], 2);
        assert_eq!(c.attachments.len(), 2);
        assert_eq!(c.kind, ComposeKind::Forward);
        // One message keeps the inline forward.
        let r = forward(&mut f.s, &json!({"ids": [f.plain.0]})).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert!(c.attachments.iter().all(|a| a.mime_type != "message/rfc822"));
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

    /// Regression: Forward (or Report with an abuse address) on a whole folder copied every message into one
    /// compose in memory, without limit.
    #[test]
    fn forward_attaches_a_capped_number_and_size_of_messages() {
        let mut f = fx();
        let folder = f.s.mailbox.mail.get(f.plain).unwrap().folder;
        let mut ids = Vec::new();
        for i in 0..=MAX_FORWARD_ITEMS {
            ids.push(
                f.s.mailbox.add_mail(MailItem { folder, subject: format!("M{i}"), ..Default::default() }).0,
            );
        }
        let r = forward(&mut f.s, &json!({"ids": ids}));
        assert!(matches!(r, Err(CmdError::Failed(ref e)) if e.contains("at most")), "{r:?}");
        assert!(f.s.composes.is_empty());
        let ok = forward(&mut f.s, &json!({"ids": ids[..MAX_FORWARD_ITEMS].to_vec()})).unwrap();
        assert_eq!(ok["attached"], MAX_FORWARD_ITEMS);
        // Too large in total (as listed, or once the sources are read).
        let huge = f.s.mailbox.add_mail(MailItem { folder, size: 51 * 1024 * 1024, ..Default::default() });
        let r = forward_as_attachment(&mut f.s, &json!({"ids": [huge.0, f.plain.0]}));
        assert!(matches!(r, Err(CmdError::Failed(ref e)) if e.contains("too large")), "{r:?}");
        let big = vec![b'x'; MAX_FORWARD_BYTES / 2 + 1];
        let (b1, b2) = (
            f.s.mailbox.add_mail(MailItem { folder, raw: Some(big.clone()), ..Default::default() }),
            f.s.mailbox.add_mail(MailItem { folder, raw: Some(big), ..Default::default() }),
        );
        let r = forward_as_attachment(&mut f.s, &json!({"ids": [b1.0, b2.0]}));
        assert!(matches!(r, Err(CmdError::Failed(ref e)) if e.contains("too large")), "{r:?}");
    }

    #[test]
    fn forward_as_attachment_gets_the_reply_signature() {
        let mut f = fx();
        f.s.mailbox.add_signature("Short", mailcraft_model::Doc::from_plain_text("-- T")).unwrap();
        f.s.mailbox.set_signature_defaults(None, None, Some(Some("Short"))).unwrap();
        let r = forward_as_attachment(&mut f.s, &Value::Null).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.signature.as_ref().map(|s| s.name.as_str()), Some("Short"));
        assert!(matches!(c.body.blocks.get(1), Some(Block::Container { .. })), "{:?}", c.body.blocks);
        assert_eq!(c.selection.focus.path, vec![0], "the caret sits above the signature");
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
        let sent = &s.mailbox.mail.get(ItemId(r["item"].as_u64().unwrap())).unwrap();
        let to: Vec<(&str, Option<&str>)> =
            sent.to.iter().map(|a| (a.addr.as_str(), a.name.as_deref())).collect();
        assert_eq!(to, [("alice@example.com", Some("Rivera, Alice"))]);
        let cc: Vec<&str> = sent.cc.iter().map(|a| a.addr.as_str()).collect();
        assert_eq!(cc, ["bob@example.org", "carol@example.com"]);
        assert!(matches!(
            s.mailbox.mail.get(src).unwrap().response,
            mailcraft_model::ResponseState::RepliedAll(_)
        ));
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

    fn set(s: &mut Session, key: &str, value: Value) {
        s.run("options.set", &json!({"key": key, "value": value})).unwrap();
    }

    fn respond_to(f: &mut Fx, id: ItemId, kind: ComposeKind) -> ComposeState {
        f.s.selection.focus = Some(id);
        f.s.selection.items = vec![id];
        let r = respond(&mut f.s, &Value::Null, kind).unwrap();
        f.s.composes[&r["compose"].as_u64().unwrap()].clone()
    }

    const ORIGINAL: &str = "The badge reader is down. Please use the side door today.";

    #[test]
    fn reply_text_options_shape_the_body() {
        let mut f = fx();
        let plain = f.plain;
        // Prefix each line (the default prefix "> ", then a custom one).
        set(&mut f.s, "mail.reply.replyText", json!("prefix"));
        let c = respond_to(&mut f, plain, ComposeKind::Reply);
        let text = c.body.to_plain_text();
        assert!(text.contains("Subject: URGENT: server room access\n"), "header kept: {text}");
        assert!(text.ends_with(&format!("\n> {ORIGINAL}")), "{text}");
        set(&mut f.s, "mail.reply.plainPrefix", json!("| "));
        let c = respond_to(&mut f, plain, ComposeKind::Reply);
        assert!(c.body.to_plain_text().ends_with(&format!("\n| {ORIGINAL}")));
        // Include and indent.
        set(&mut f.s, "mail.reply.replyText", json!("indent"));
        let c = respond_to(&mut f, plain, ComposeKind::Reply);
        assert!(c.body.to_plain_text().ends_with(&format!("\n\t{ORIGINAL}")));
        // Do not include: an empty body; attach: the original as an .eml item.
        set(&mut f.s, "mail.reply.replyText", json!("none"));
        let c = respond_to(&mut f, plain, ComposeKind::Reply);
        assert_eq!(c.body.to_plain_text().trim(), "");
        assert!(c.attachments.is_empty());
        set(&mut f.s, "mail.reply.replyText", json!("attach"));
        let c = respond_to(&mut f, plain, ComposeKind::ReplyAll);
        assert_eq!(c.body.to_plain_text().trim(), "");
        assert_eq!(c.attachments.len(), 1);
        assert_eq!(c.attachments[0].filename, "URGENT_ server room access.eml");
        assert_eq!(c.attachments[0].mime_type, "message/rfc822");
        assert!(String::from_utf8_lossy(&c.attachments[0].data).contains(ORIGINAL));
        // Forwards follow their own option.
        let c = respond_to(&mut f, plain, ComposeKind::Forward);
        assert!(c.body.to_plain_text().ends_with(ORIGINAL) && c.attachments.is_empty());
    }

    #[test]
    fn forward_text_options_in_html() {
        let mut f = fx();
        let html = f.html;
        set(&mut f.s, "mail.reply.forwardText", json!("prefix"));
        let c = respond_to(&mut f, html, ComposeKind::Forward);
        assert!(matches!(c.body.blocks.last(), Some(Block::Quote { blocks }) if !blocks.is_empty()));
        assert!(mailcraft_html::doc_to_html(&c.body).contains("<blockquote"));
        set(&mut f.s, "mail.reply.forwardText", json!("indent"));
        let c = respond_to(&mut f, html, ComposeKind::Forward);
        assert!(matches!(
            c.body.blocks.last(),
            Some(Block::Container { style, .. }) if style.indent_px == INDENT_PX
        ));
        // The header block stays outside, so it is still recognised as the start of the original.
        assert_eq!(c.quoted_start(), Some(2));
        set(&mut f.s, "mail.reply.forwardText", json!("attach"));
        let c = respond_to(&mut f, html, ComposeKind::Forward);
        assert_eq!(c.attachments.len(), 1);
        assert!(c.inline_images.is_empty());
        // "Do not include" is not a forward choice.
        assert!(f.s.run("options.set", &json!({"key": "mail.reply.forwardText", "value": "none"})).is_err());
    }

    #[test]
    fn preface_and_proofing_options_reach_the_response() {
        let mut f = fx();
        let plain = f.plain;
        let c = respond_to(&mut f, plain, ComposeKind::Reply);
        assert_eq!((c.preface.clone(), c.proof_original), (None, false));
        set(&mut f.s, "mail.reply.prefaceComments", json!(true));
        // No name set anywhere: the account's sender name.
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).preface.as_deref(), Some("Test User"));
        set(&mut f.s, "general.userName", json!("T. User"));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).preface.as_deref(), Some("T. User"));
        set(&mut f.s, "mail.reply.prefaceName", json!("TU"));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Forward).preface.as_deref(), Some("TU"));
        // Nothing to preface when the original is not in the body.
        set(&mut f.s, "mail.reply.replyText", json!("attach"));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).preface, None);
        // Proofing the original.
        set(&mut f.s, "mail.compose.ignoreOriginal", json!(false));
        let mut c = respond_to(&mut f, plain, ComposeKind::Forward);
        assert!(c.proof_original);
        c.refresh_spelling(true, &mut |t: &str| {
            t.find("badge")
                .map(|i| crate::compose::TextIssue {
                    start: i,
                    end: i + 5,
                    kind: String::new(),
                    suggestions: vec![],
                    message: String::new(),
                })
                .into_iter()
                .collect()
        });
        assert_eq!(c.spell_issues.len(), 1, "the original is checked");
        c.proof_original = false;
        c.refresh_spelling(true, &mut |t: &str| {
            t.find("badge")
                .map(|i| crate::compose::TextIssue {
                    start: i,
                    end: i + 5,
                    kind: String::new(),
                    suggestions: vec![],
                    message: String::new(),
                })
                .into_iter()
                .collect()
        });
        assert!(c.spell_issues.is_empty());
    }

    #[test]
    fn where_sent_copies_of_responses_go() {
        let mut f = fx();
        let plain = f.plain;
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Forward).save_sent, SaveSentTo::Default);
        set(&mut f.s, "mail.save.forwarded", json!(false));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Forward).save_sent, SaveSentTo::DoNotSave);
        let r = forward_as_attachment(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.composes[&r["compose"].as_u64().unwrap()].save_sent, SaveSentTo::DoNotSave);
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).save_sent, SaveSentTo::Default);
        // A reply to a message outside the Inbox is saved next to it when asked to.
        set(&mut f.s, "mail.save.replyInSameFolder", json!(true));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).save_sent, SaveSentTo::Default, "Inbox");
        let inbox = f.s.mailbox.mail.get(plain).unwrap().folder;
        let projects = f.s.mailbox.create_folder(inbox, "Projects").unwrap();
        f.s.mailbox.mail.get_mut(plain).unwrap().folder = projects;
        assert_eq!(respond_to(&mut f, plain, ComposeKind::ReplyAll).save_sent, SaveSentTo::Folder(projects));
        // ...unless sent copies are not saved at all.
        set(&mut f.s, "mail.save.sentCopies", json!(false));
        assert_eq!(respond_to(&mut f, plain, ComposeKind::Reply).save_sent, SaveSentTo::DoNotSave);
    }
}
