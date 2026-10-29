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

/// Header carrying the reminder time of a flag set for the recipients (RFC 5322 date; MailCraft's own).
pub const RECIPIENT_REMINDER_HEADER: &str = "X-MailCraft-Flag-Reminder";

/// Longest flag label kept from a received message.
pub const MAX_FLAG_LABEL_CHARS: usize = 255;

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

/// A message body: its text and HTML parts. Bodies are not part of [`MailItem`] (storage stage S5): they live
/// in the store's body store and are read on demand (the engine's `Session::body`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body {
    pub format: BodyFormat,
    pub text: Option<String>,
    /// Sanitising happens at display time (`mailcraft-html`); the stored HTML is the original.
    pub html: Option<String>,
}

impl Body {
    /// The body of a parsed message: its text and HTML parts (HTML format when it has an HTML part).
    pub fn from_message(msg: &mailcraft_mime::Message) -> Body {
        let text = msg.text_body();
        let html = msg.html_body();
        let format = if html.is_some() { BodyFormat::Html } else { BodyFormat::PlainText };
        Body { format, text, html }
    }
    /// The body a message source gives (see [`Body::from_message`]); `None` when the source does not parse.
    pub fn from_source(raw: &[u8]) -> Option<Body> {
        mailcraft_mime::parse(raw).ok().map(|m| Body::from_message(&m))
    }
    /// Neither a text nor an HTML part.
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.html.is_none()
    }
    /// Plain text of the body: the text part, else the HTML with tags stripped. A blank text part does not
    /// hide the HTML (same choice as [`make_preview`]).
    pub fn plain_text(&self) -> String {
        match (&self.text, &self.html) {
            (Some(t), _) if !t.trim().is_empty() => t.clone(),
            (_, Some(h)) => crate::text::strip_html(h),
            (Some(t), None) => t.clone(),
            (None, None) => String::new(),
        }
    }
    /// Like [`Body::plain_text`], but reading at most `max_bytes` of the body (cut at a character
    /// boundary): bounded work for something drawn every frame, however large the message is.
    pub fn text_prefix(&self, max_bytes: usize) -> String {
        let cut = |s: &str| crate::text::truncate_at_char(s, max_bytes).to_owned();
        match (&self.text, &self.html) {
            (Some(t), _) if !t.trim().is_empty() => cut(t),
            (_, Some(h)) => crate::text::strip_html(crate::text::truncate_at_char(h, max_bytes)),
            (Some(t), None) => cut(t),
            (None, None) => String::new(),
        }
    }
    /// The list preview line of this body ([`make_preview`]).
    pub fn preview(&self) -> String {
        make_preview(self.text.as_deref(), self.html.as_deref())
    }
    /// A rough estimate of the memory this body takes (for byte caps; not exact).
    pub fn approx_bytes(&self) -> usize {
        let opt = |t: &Option<String>| t.as_ref().map_or(0, |t| t.len().saturating_add(16));
        std::mem::size_of::<Body>().saturating_add(opt(&self.text)).saturating_add(opt(&self.html))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub inline: bool,
}

/// A voting response recorded on the message it answers (the sent original), so the tally keeps it after
/// the response itself is deleted for good.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteRecord {
    /// Responder's address (empty when the response named none).
    pub from: String,
    /// The voting choice, as the message offers it.
    pub choice: String,
    /// When the response was sent (else received).
    pub when: DateTime,
    /// The response message, while it exists.
    #[serde(default)]
    pub response: Option<ItemId>,
}

/// Most voting responses recorded on one message (one per responder).
pub const MAX_VOTE_RECORDS: usize = 2_000;

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
    pub is_draft: bool,
    pub response: ResponseState,
    pub focus: FocusClass,
    /// Conversation key (see `subject::conversation_topic` and threading in the engine).
    pub conversation: Option<u64>,
    /// "Ignore" conversation: new items go straight to Deleted Items.
    pub ignored: bool,
    /// IMAP UID / JMAP id when synced.
    pub remote_id: Option<String>,
    /// Key of the RFC 5322 source in the store's body store (`mailcraft-store`), when stored. The source is
    /// read on demand (View Source, Forward as Attachment, export); the decoded body comes from it unless
    /// [`MailItem::body_key`] is set.
    pub blob: Option<String>,
    /// Key of a body record in the store's body store when the body is not the one the source gives: an item
    /// without a source (a local draft, a message made here) or one whose body was changed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_key: Option<String>,
    /// Expiry date of the message (`Expiry-Date` or `Expires`, RFC 4021), read from its source when the source
    /// was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<DateTime>,
    /// The account's offline window took this message's body off this computer (its header stays): it is
    /// downloaded again when the message is opened while the account is online.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_evicted: bool,
    /// Pinned to the top of its folder's list (Quick Action Pin / Unpin, `mail.pin`). Local only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Voting responses to this message recorded as they arrived (latest per responder).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub votes: Vec<VoteRecord>,
    /// Properties › "Do not AutoArchive this item": AutoArchive leaves it where it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_auto_archive: bool,
    /// Always Move Conversation: later messages of this item's conversation go to this folder on arrival.
    /// Local only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_move_to: Option<FolderId>,
    /// Properties › Contacts: people the message is linked to (names or addresses as typed). Local only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<String>,
}

impl MailItem {
    /// A rough estimate of the memory this item takes: its fixed size plus the text it holds (addresses, ids,
    /// preview; bodies and sources live in the body store). For byte caps; not exact.
    pub fn approx_bytes(&self) -> usize {
        // Each heap allocation costs a little more than its length.
        const PER_ALLOC: usize = 16;
        let s = |t: &str| if t.is_empty() { 0 } else { t.len().saturating_add(PER_ALLOC) };
        let opt = |t: &Option<String>| t.as_deref().map_or(0, s);
        let addr = |a: &Mailbox| {
            std::mem::size_of::<Mailbox>().saturating_add(s(&a.addr)).saturating_add(opt(&a.name))
        };
        let addrs = |v: &[Mailbox]| v.iter().map(addr).fold(0usize, usize::saturating_add);
        let strs = |v: &[String]| {
            v.iter()
                .map(|t| s(t).saturating_add(std::mem::size_of::<String>()))
                .fold(0usize, usize::saturating_add)
        };
        let parts = [
            std::mem::size_of::<MailItem>(),
            opt(&self.message_id),
            strs(&self.in_reply_to),
            strs(&self.references),
            s(&self.subject),
            self.from.as_ref().map_or(0, addr),
            self.sender.as_ref().map_or(0, addr),
            addrs(&self.to),
            addrs(&self.cc),
            addrs(&self.bcc),
            addrs(&self.reply_to),
            strs(&self.categories),
            self.attachments
                .iter()
                .map(|a| {
                    std::mem::size_of::<AttachmentMeta>()
                        .saturating_add(s(&a.filename))
                        .saturating_add(s(&a.mime_type))
                        .saturating_add(opt(&a.content_id))
                })
                .fold(0usize, usize::saturating_add),
            s(&self.preview),
            opt(&self.remote_id),
            opt(&self.blob),
            opt(&self.body_key),
            self.votes.len().saturating_mul(std::mem::size_of::<VoteRecord>().saturating_add(2 * PER_ALLOC)),
            strs(&self.contacts),
        ];
        parts.into_iter().fold(0usize, usize::saturating_add)
    }

    /// Builds an item from a parsed message (subject, addresses, dates, importance, attachments metadata,
    /// preview, ids, expiry) whose source is `raw` (its size). The body is [`Body::from_message`]; neither it
    /// nor the source is kept on the item (the caller stores them). `received` falls back to the Date header,
    /// then to `now`.
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
        let preview = Body::from_message(msg).preview();
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
        // A flag the sender set for its recipients: the label, the RFC 2156 reply-by date as the due date,
        // and the reminder MailCraft sends with it. A reminder time before the message arrived is the
        // sender's way to pop a window the moment it lands, so it is dropped (the flag stays).
        let date_header = |name: &str| msg.headers.get(name).and_then(|v| DateTime::parse_rfc5322(v).ok());
        let reminder = date_header(RECIPIENT_REMINDER_HEADER).filter(|r| r.unix >= received.unix);
        let flag = match header("X-Message-Flag") {
            Some(label) if !label.is_empty() => FlagState::Flagged {
                label: label.chars().filter(|c| !c.is_control()).take(MAX_FLAG_LABEL_CHARS).collect(),
                start: None,
                due: date_header("Reply-By"),
                reminder,
            },
            _ => FlagState::None,
        };
        let is_draft = header("X-Unsent").is_some_and(|v| v == "1");
        let expires = date_header("Expiry-Date").or_else(|| date_header("Expires"));
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
            is_draft,
            expires,
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
    /// Size label as in the list/accessible name: "N KB" (rounded up, min 1).
    pub fn size_label(&self) -> String {
        format!("{} KB", self.size.div_ceil(1024).max(1))
    }
    /// Takes from `old` (the same message before it was downloaded again, e.g. a header-only item getting
    /// its body) everything that is not read from the message source: id, folder, read, flag, categories,
    /// response, draft, server id, arrival time, Focused, Ignore, conversation, pin, recorded votes and linked
    /// contacts.
    pub fn keep_local_state(&mut self, old: &MailItem) {
        // Every field is named, so a new one has to be sorted into "local" or "from the source" here.
        let MailItem {
            id,
            folder,
            message_id: _,
            in_reply_to: _,
            references: _,
            subject: _,
            from: _,
            sender: _,
            to: _,
            cc: _,
            bcc: _,
            reply_to: _,
            sent: _,
            received,
            size: _,
            read,
            importance: _,
            sensitivity: _,
            flag,
            categories,
            attachments: _,
            preview: _,
            is_draft,
            response,
            focus,
            conversation,
            ignored,
            remote_id,
            blob: _,
            body_key: _,
            expires: _,
            body_evicted: _,
            pinned,
            votes,
            no_auto_archive,
            always_move_to,
            contacts,
        } = old;
        self.id = *id;
        self.folder = *folder;
        self.received = *received;
        self.read = *read;
        self.flag = flag.clone();
        self.categories = categories.clone();
        self.is_draft = *is_draft;
        self.response = response.clone();
        self.focus = *focus;
        self.conversation = *conversation;
        self.ignored = *ignored;
        self.remote_id = remote_id.clone();
        self.contacts = contacts.clone();
        self.pinned = *pinned;
        self.votes = votes.clone();
        self.no_auto_archive = *no_auto_archive;
        self.always_move_to = *always_move_to;
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
