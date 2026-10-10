//! Outlook item files (`.msg`): [MS-OXMSG] objects stored in a [MS-CFB] compound file.
//!
//! Reading extracts the message class, subject, sender, recipients (To/Cc/Bcc from the recipient storages),
//! dates, internet ids, importance/sensitivity/flags, categories (the `Keywords` named property), the bodies
//! (PidTagBody, PidTagHtml, and PidTagRtfCompressed decompressed per [MS-OXRTFCP], with HTML de-encapsulated per
//! [MS-OXRTFEX] when no HTML body is present) and attachments (by value, and embedded messages recursively).
//!
//! Writing produces a minimal valid file for Save As: a Unicode property stream, recipient and attachment
//! storages (embedded messages as nested message storages), the named-property mapping storage, Unicode string
//! properties and the STORE_UNICODE_OK support mask.

mod props;
pub mod read;
pub mod rtf;
pub mod rtfhtml;
pub mod write;

use mailcraft_mime::{Address, AttachmentSpec, Mailbox, MessageBuilder};
use mailcraft_model::{BodyFormat, DateTime, FolderId, Importance, MailItem, Sensitivity};

pub use read::read;
pub use write::write;

/// Compound File Binary signature.
pub const CFB_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Largest `.msg` accepted.
pub const MAX_MSG_BYTES: usize = mailcraft_mime::MAX_MESSAGE_BYTES;
/// Most recipients read per message.
pub const MAX_RECIPIENTS: usize = 10_000;
/// Most attachments read per message.
pub const MAX_ATTACHMENTS: usize = 2_000;
/// Deepest embedded-message nesting followed.
pub const MAX_EMBED_DEPTH: usize = 8;

/// An attachment's content.
#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentContent {
    Data(Vec<u8>),
    /// An embedded message (attach method 5).
    Message(Box<MsgMessage>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MsgAttachment {
    pub filename: String,
    pub mime_type: String,
    pub content_id: Option<String>,
    /// Shown in the body (cid-referenced / hidden) rather than in the attachment list.
    pub inline: bool,
    pub content: AttachmentContent,
}

/// A message read from or written to a `.msg` file.
#[derive(Clone, Debug, PartialEq)]
pub struct MsgMessage {
    /// "IPM.Note", "IPM.Schedule.Meeting.Request", …
    pub message_class: String,
    pub subject: String,
    /// The author (sent-representing); falls back to the sender.
    pub from: Option<Mailbox>,
    /// The sending mailbox when different from `from`.
    pub sender: Option<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub sent: Option<DateTime>,
    pub received: Option<DateTime>,
    pub created: Option<DateTime>,
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub importance: Importance,
    pub sensitivity: Sensitivity,
    pub read: bool,
    /// Unsent (draft).
    pub unsent: bool,
    pub categories: Vec<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// Decompressed RTF body, when the file had one.
    pub body_rtf: Option<Vec<u8>>,
    pub body_format: BodyFormat,
    /// RFC 5322 header block of a received message (PidTagTransportMessageHeaders).
    pub transport_headers: Option<String>,
    pub attachments: Vec<MsgAttachment>,
}

impl Default for MsgMessage {
    fn default() -> Self {
        MsgMessage {
            message_class: "IPM.Note".into(),
            subject: String::new(),
            from: None,
            sender: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            sent: None,
            received: None,
            created: None,
            message_id: None,
            in_reply_to: Vec::new(),
            references: Vec::new(),
            importance: Importance::Normal,
            sensitivity: Sensitivity::Normal,
            read: false,
            unsent: false,
            categories: Vec::new(),
            body_text: None,
            body_html: None,
            body_rtf: None,
            body_format: BodyFormat::PlainText,
            transport_headers: None,
            attachments: Vec::new(),
        }
    }
}

/// encoding_rs encoding for a Windows code page number (Windows-1252 when unknown).
pub(crate) fn encoding_for_codepage(cp: u32) -> &'static encoding_rs::Encoding {
    use encoding_rs as e;
    match cp {
        65001 => e::UTF_8,
        1200 => e::UTF_16LE,
        1201 => e::UTF_16BE,
        874 => e::WINDOWS_874,
        932 => e::SHIFT_JIS,
        936 | 54936 => e::GB18030,
        949 => e::EUC_KR,
        950 => e::BIG5,
        1250 => e::WINDOWS_1250,
        1251 => e::WINDOWS_1251,
        1253 => e::WINDOWS_1253,
        1254 => e::WINDOWS_1254,
        1255 => e::WINDOWS_1255,
        1256 => e::WINDOWS_1256,
        1257 => e::WINDOWS_1257,
        1258 => e::WINDOWS_1258,
        866 => e::IBM866,
        20866 => e::KOI8_R,
        21866 => e::KOI8_U,
        28592 => e::ISO_8859_2,
        28593 => e::ISO_8859_3,
        28594 => e::ISO_8859_4,
        28595 => e::ISO_8859_5,
        28596 => e::ISO_8859_6,
        28597 => e::ISO_8859_7,
        28598 => e::ISO_8859_8,
        28603 => e::ISO_8859_13,
        28605 => e::ISO_8859_15,
        50220..=50222 => e::ISO_2022_JP,
        51932 => e::EUC_JP,
        51936 => e::GBK,
        51949 => e::EUC_KR,
        10000 => e::MACINTOSH,
        _ => e::WINDOWS_1252,
    }
}

fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = crate::text::clip(s.trim(), 120);
    if s.is_empty() { "message".to_owned() } else { s }
}

impl MsgMessage {
    /// RFC 5322 bytes for the message (attachments and embedded messages included).
    pub fn to_eml(&self) -> Vec<u8> {
        self.to_eml_depth(0)
    }

    fn to_eml_depth(&self, depth: usize) -> Vec<u8> {
        let addrs =
            |v: &[Mailbox]| v.iter().filter(|m| !m.addr.is_empty()).cloned().map(Address::Mailbox).collect();
        let mut attachments = Vec::new();
        for a in &self.attachments {
            match &a.content {
                AttachmentContent::Data(d) => attachments.push(AttachmentSpec {
                    filename: a.filename.clone(),
                    mime_type: if a.mime_type.is_empty() {
                        "application/octet-stream".into()
                    } else {
                        a.mime_type.clone()
                    },
                    data: d.clone(),
                    content_id: if a.inline { a.content_id.clone() } else { None },
                }),
                AttachmentContent::Message(m) if depth < MAX_EMBED_DEPTH => {
                    attachments.push(AttachmentSpec {
                        filename: format!("{}.eml", sanitize_filename(&m.subject)),
                        mime_type: "message/rfc822".into(),
                        data: m.to_eml_depth(depth + 1),
                        content_id: None,
                    })
                }
                AttachmentContent::Message(_) => {}
            }
        }
        let mut extra = Vec::new();
        if !self.categories.is_empty() {
            extra.push(("Keywords".to_owned(), self.categories.join(", ")));
        }
        match self.sensitivity {
            Sensitivity::Normal => {}
            Sensitivity::Personal => extra.push(("Sensitivity".into(), "Personal".into())),
            Sensitivity::Private => extra.push(("Sensitivity".into(), "Private".into())),
            Sensitivity::Confidential => extra.push(("Sensitivity".into(), "Company-Confidential".into())),
        }
        if self.unsent {
            extra.push(("X-Unsent".into(), "1".into()));
        }
        let sender = match (&self.sender, &self.from) {
            (Some(s), Some(f)) if !s.same_address(f) && !s.addr.is_empty() => Some(s.clone()),
            _ => None,
        };
        let builder = MessageBuilder {
            from: self.from.clone().or_else(|| self.sender.clone()).filter(|m| !m.addr.is_empty()),
            sender,
            to: addrs(&self.to),
            cc: addrs(&self.cc),
            bcc: addrs(&self.bcc),
            subject: self.subject.clone(),
            date: self.sent.or(self.received).or(self.created),
            message_id: self.message_id.clone(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            importance: match self.importance {
                Importance::High => Some("high".into()),
                Importance::Low => Some("low".into()),
                Importance::Normal => None,
            },
            text: self.body_text.clone(),
            html: self.body_html.clone(),
            attachments,
            extra_headers: extra,
            keep_bcc: true,
            ..Default::default()
        };
        builder.build()
    }

    /// A mailbox item for the message (its source is the [`MsgMessage::to_eml`] rendering).
    pub fn to_mail_item(&self, folder: FolderId, now: DateTime) -> MailItem {
        let raw = self.to_eml();
        let mut item = mailcraft_store::import::item_from_raw(&raw, folder, now);
        item.received = self.received.or(self.sent).or(self.created).unwrap_or(now);
        item.sent = self.sent;
        item.read = self.read;
        item.is_draft = self.unsent;
        item.sensitivity = self.sensitivity;
        item.importance = self.importance;
        if !self.categories.is_empty() {
            item.categories = self.categories.clone();
        }
        if self.message_id.is_none() && !self.unsent {
            // The builder invented an id; do not present it as the original.
            item.message_id = None;
        }
        item.body.format = self.body_format;
        item
    }

    /// Builds a message from a mailbox item; attachment content comes from the item's source when kept.
    pub fn from_mail_item(item: &MailItem) -> MsgMessage {
        Self::from_mail_item_depth(item, 0)
    }

    fn from_mail_item_depth(item: &MailItem, depth: usize) -> MsgMessage {
        let mut attachments = Vec::new();
        if let Some(raw) = &item.raw
            && let Ok(msg) = mailcraft_mime::parse(raw)
        {
            for a in msg.attachments().into_iter().take(MAX_ATTACHMENTS) {
                let content = if a.mime_type.eq_ignore_ascii_case("message/rfc822") && depth < MAX_EMBED_DEPTH
                {
                    match mailcraft_mime::parse(&a.data) {
                        Ok(inner) => {
                            let sub = MailItem::from_message(&inner, &a.data, item.received);
                            AttachmentContent::Message(Box::new(Self::from_mail_item_depth(&sub, depth + 1)))
                        }
                        Err(_) => AttachmentContent::Data(a.data),
                    }
                } else {
                    AttachmentContent::Data(a.data)
                };
                attachments.push(MsgAttachment {
                    filename: a.filename,
                    mime_type: a.mime_type,
                    content_id: a.content_id,
                    inline: a.inline,
                    content,
                });
            }
        }
        MsgMessage {
            message_class: "IPM.Note".into(),
            subject: item.subject.clone(),
            from: item.from.clone(),
            sender: item.sender.clone(),
            to: item.to.clone(),
            cc: item.cc.clone(),
            bcc: item.bcc.clone(),
            sent: item.sent,
            received: (!item.is_draft).then_some(item.received),
            created: Some(item.received),
            message_id: item.message_id.clone(),
            in_reply_to: item.in_reply_to.clone(),
            references: item.references.clone(),
            importance: item.importance,
            sensitivity: item.sensitivity,
            read: item.read,
            unsent: item.is_draft,
            categories: item.categories.clone(),
            body_text: item.body.text.clone(),
            body_html: item.body.html.clone(),
            body_rtf: None,
            body_format: item.body.format,
            transport_headers: None,
            attachments,
        }
    }
}

/// Reads a `.msg` file into a mailbox item.
pub fn read_item(data: &[u8], folder: FolderId, now: DateTime) -> crate::Result<MailItem> {
    Ok(read(data)?.to_mail_item(folder, now))
}

/// Writes a mailbox item as `.msg` bytes.
pub fn write_item(item: &MailItem) -> crate::Result<Vec<u8>> {
    write(&MsgMessage::from_mail_item(item))
}
