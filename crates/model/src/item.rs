//! Mail items.

use serde::{Deserialize, Serialize};

use crate::{DateTime, FolderId, ItemId};
use mailcraft_mime::Mailbox;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sensitivity {
    #[default]
    Normal,
    Personal,
    Private,
    Confidential,
}

/// Follow-up flag. Observed list glyphs: red flag (flagged), green check (complete).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlagState {
    #[default]
    None,
    Flagged {
        /// "Follow up", "For Your Information", "Reply", … (Custom dialog "Flag to").
        label: String,
        start: Option<DateTime>,
        due: Option<DateTime>,
        reminder: Option<DateTime>,
    },
    Complete {
        completed: Option<DateTime>,
    },
}

/// Last response sent for this message (drives the replied/forwarded row icon and the info bar).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseState {
    #[default]
    None,
    Replied(DateTime),
    RepliedAll(DateTime),
    Forwarded(DateTime),
}

/// Focused Inbox classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FocusClass {
    #[default]
    Focused,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BodyFormat {
    #[default]
    Html,
    PlainText,
    RichText,
}

impl BodyFormat {
    /// Label used in window titles: "Message (HTML)", "Message (Plain Text)", "Message (Rich Text)".
    pub fn title_label(self) -> &'static str {
        match self {
            BodyFormat::Html => "HTML",
            BodyFormat::PlainText => "Plain Text",
            BodyFormat::RichText => "Rich Text",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body {
    pub format: BodyFormat,
    pub text: Option<String>,
    /// Sanitising happens at display time (`mailcraft-html`); the stored HTML is the original.
    pub html: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailItem {
    pub id: ItemId,
    pub folder: FolderId,
    /// Message-ID without brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Option<Mailbox>,
    pub sender: Option<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub reply_to: Vec<Mailbox>,
    pub sent: Option<DateTime>,
    pub received: DateTime,
    /// Size in bytes (RFC 5322 source size).
    pub size: u64,
    pub read: bool,
    pub importance: Importance,
    pub sensitivity: Sensitivity,
    pub flag: FlagState,
    pub categories: Vec<String>,
    pub attachments: Vec<AttachmentMeta>,
    /// First ~255 chars of body text, whitespace collapsed (list preview line).
    pub preview: String,
    pub body: Body,
    pub is_draft: bool,
    pub response: ResponseState,
    pub focus: FocusClass,
    /// Conversation key (see `subject::conversation_topic` and threading in the engine).
    pub conversation: Option<u64>,
    /// "Ignore" conversation: new items go straight to Deleted Items.
    pub ignored: bool,
    /// IMAP UID / JMAP id when synced.
    pub remote_id: Option<String>,
    /// Blob key of the raw source in the store (`mailcraft-store`), when stored.
    pub blob: Option<String>,
    /// Original RFC 5322 source when kept (needed for Forward as Attachment, View Source, export).
    pub raw: Option<Vec<u8>>,
}

impl MailItem {
    /// Builds an item from a parsed message (subject, addresses, dates, importance, bodies, attachments
    /// metadata, preview, ids). `received` falls back to the Date header, then to `now`.
    pub fn from_message(msg: &mailcraft_mime::Message, raw: &[u8], now: DateTime) -> MailItem {
        let flatten = |list: Vec<mailcraft_mime::Address>| -> Vec<Mailbox> {
            list.iter().flat_map(mailcraft_mime::Address::mailboxes).collect()
        };
        let sent = msg.date();
        let received = received_date(&msg.headers).or(sent).unwrap_or(now);
        let importance = match msg.importance() {
            Some("high") => Importance::High,
            Some("low") => Importance::Low,
            _ => Importance::Normal,
        };
        let text = msg.text_body();
        let html = msg.html_body();
        let format = if html.is_some() { BodyFormat::Html } else { BodyFormat::PlainText };
        let preview = make_preview(text.as_deref(), html.as_deref());
        let attachments = msg
            .attachments()
            .into_iter()
            .map(|a| AttachmentMeta {
                filename: a.filename,
                mime_type: a.mime_type,
                size: u64::try_from(a.data.len()).unwrap_or(u64::MAX),
                content_id: a.content_id,
                inline: a.inline,
            })
            .collect();
        let header = |name: &str| msg.headers.text(name).map(|v| v.trim().to_string());
        let categories = header("Keywords")
            .map(|v| {
                let mut seen: Vec<String> = Vec::new();
                for c in v.split([',', ';']).map(str::trim).filter(|c| !c.is_empty()) {
                    if !seen.iter().any(|s| s.eq_ignore_ascii_case(c)) {
                        seen.push(c.to_string());
                    }
                }
                seen
            })
            .unwrap_or_default();
        let flag = match header("X-Message-Flag") {
            Some(label) if !label.is_empty() => {
                FlagState::Flagged { label, start: None, due: None, reminder: None }
            }
            _ => FlagState::None,
        };
        let is_draft = header("X-Unsent").is_some_and(|v| v == "1");
        MailItem {
            message_id: msg.message_id().map(|id| clean_id(&id)).filter(|id| !id.is_empty()),
            in_reply_to: msg
                .in_reply_to()
                .iter()
                .map(|id| clean_id(id))
                .filter(|id| !id.is_empty())
                .collect(),
            references: msg.references().iter().map(|id| clean_id(id)).filter(|id| !id.is_empty()).collect(),
            subject: msg.subject(),
            from: msg.from(),
            sender: msg.sender(),
            to: flatten(msg.to()),
            cc: flatten(msg.cc()),
            bcc: flatten(msg.bcc()),
            reply_to: flatten(msg.reply_to()),
            sent,
            received,
            size: u64::try_from(raw.len()).unwrap_or(u64::MAX),
            importance,
            sensitivity: header("Sensitivity").map(|v| parse_sensitivity(&v)).unwrap_or_default(),
            flag,
            categories,
            attachments,
            preview,
            body: Body { format, text, html },
            is_draft,
            raw: (!raw.is_empty()).then(|| raw.to_vec()),
            ..Default::default()
        }
    }
    pub fn has_attachments(&self) -> bool {
        self.attachments.iter().any(|a| !a.inline)
    }
    pub fn is_flagged(&self) -> bool {
        matches!(self.flag, FlagState::Flagged { .. })
    }
    /// Size in KB as shown in the list (rounded up, minimum 1).
    pub fn size_kb(&self) -> u64 {
        self.size.div_ceil(1024).max(1)
    }
    /// Plain text of the body: the text part, else the HTML with tags stripped. A blank text part does not
    /// hide the HTML (same choice as [`make_preview`]).
    pub fn body_text(&self) -> String {
        match (&self.body.text, &self.body.html) {
            (Some(t), _) if !t.trim().is_empty() => t.clone(),
            (_, Some(h)) => crate::text::strip_html(h),
            (Some(t), None) => t.clone(),
            (None, None) => String::new(),
        }
    }
    /// Recomputes `preview` from the body.
    pub fn refresh_preview(&mut self) {
        self.preview = make_preview(self.body.text.as_deref(), self.body.html.as_deref());
    }
    /// Size label as in the list/accessible name: "N KB" (rounded up, min 1).
    pub fn size_label(&self) -> String {
        format!("{} KB", self.size.div_ceil(1024).max(1))
    }
}

/// List preview: text body preferred, else crudely stripped HTML; whitespace collapsed; 255 characters.
pub fn make_preview(text: Option<&str>, html: Option<&str>) -> String {
    match (text, html) {
        (Some(t), _) if !t.trim().is_empty() => crate::text::preview_of(t, crate::text::PREVIEW_CHARS),
        (_, Some(h)) => crate::text::preview_of(&crate::text::strip_html(h), crate::text::PREVIEW_CHARS),
        _ => String::new(),
    }
}

/// Message-id without angle brackets or surrounding whitespace.
fn clean_id(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string()
}

/// Sensitivity header values (RFC 2156): "Personal", "Private", "Company-Confidential".
fn parse_sensitivity(v: &str) -> Sensitivity {
    let v = v.trim().to_ascii_lowercase();
    match v.as_str() {
        "personal" => Sensitivity::Personal,
        "private" => Sensitivity::Private,
        "company-confidential" | "confidential" => Sensitivity::Confidential,
        _ => Sensitivity::Normal,
    }
}

/// Arrival time from the newest (topmost) Received header: the date after its last ';'.
fn received_date(headers: &mailcraft_mime::Headers) -> Option<DateTime> {
    let first = headers.get_all("Received").next()?;
    let (_, date) = first.rsplit_once(';')?;
    DateTime::parse_rfc5322(date.trim()).ok()
}
