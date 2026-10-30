//! Microsoft Graph mail: mail folders (with child folders and the well-known folder names), messages read
//! through per-folder delta queries, a message's MIME source (`$value`), changes to read state, flags,
//! categories and importance, moves and deletes, messages created from MIME (drafts and uploads), sending
//! (`sendMail` with MIME, or a draft sent with `send`), and file attachments (small ones inline, large ones
//! through an upload session).

use base64::Engine;
use mailcraft_model::{DateTime, FolderRole};
use serde_json::{Value, json};

use super::{GraphClient, seg};
use crate::error::{NetError, Result};
use crate::http::percent_encode;

/// Most mail folders read from one mailbox.
pub const MAX_FOLDERS: usize = 5_000;
/// Deepest folder nesting followed.
pub const MAX_FOLDER_DEPTH: usize = 32;
/// Properties read for every message of a delta query (the message list's columns and the sync state).
pub const MESSAGE_SELECT: &str = "id,subject,from,sender,toRecipients,ccRecipients,bccRecipients,replyTo,\
receivedDateTime,sentDateTime,isRead,isDraft,flag,importance,categories,parentFolderId,internetMessageId,\
hasAttachments,bodyPreview,inferenceClassification,conversationId";
/// Largest request body Graph takes in one piece; bigger MIME content goes as a draft plus upload sessions.
pub const MAX_INLINE_REQUEST: usize = 3 * 1024 * 1024;
/// Bytes per upload-session request (a multiple of 320 KiB, as the service asks).
pub const UPLOAD_CHUNK: usize = 10 * 320 * 1024;
/// Most entries kept from a message's address and category lists.
const MAX_LIST: usize = 500;
/// Longest text kept from one property of a message.
const MAX_TEXT: usize = 4 * 1024;

/// Graph's well-known folder names and the MailCraft default folders they are.
pub const WELL_KNOWN: [(&str, FolderRole); 6] = [
    ("inbox", FolderRole::Inbox),
    ("drafts", FolderRole::Drafts),
    ("sentitems", FolderRole::SentItems),
    ("deleteditems", FolderRole::DeletedItems),
    ("junkemail", FolderRole::JunkEmail),
    ("archive", FolderRole::Archive),
];

/// The well-known folder name of a default folder.
pub fn well_known_name(role: FolderRole) -> Option<&'static str> {
    WELL_KNOWN.iter().find(|(_, r)| *r == role).map(|(n, _)| *n)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).map(|s| s.chars().take(MAX_TEXT).collect()).unwrap_or_default()
}

fn opt_text(v: &Value, key: &str) -> Option<String> {
    Some(text(v, key)).filter(|s| !s.is_empty())
}

/// A mail folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailFolder {
    pub id: String,
    pub display_name: String,
    pub parent_folder_id: Option<String>,
    pub child_folder_count: u64,
    pub total: u64,
    pub unread: u64,
    /// The folder is one of the well-known ones (set by [`GraphClient::mail_folders`]).
    pub role: Option<FolderRole>,
    /// Nesting below the top of the mailbox (0 = top level).
    pub depth: usize,
}

impl MailFolder {
    /// Reads a `mailFolder` resource; `None` without an id.
    pub fn from_json(v: &Value) -> Option<MailFolder> {
        let id = opt_text(v, "id")?;
        let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Some(MailFolder {
            id,
            display_name: text(v, "displayName"),
            parent_folder_id: opt_text(v, "parentFolderId"),
            child_folder_count: num("childFolderCount"),
            total: num("totalItemCount"),
            unread: num("unreadItemCount"),
            role: None,
            depth: 0,
        })
    }
}

/// A follow-up flag's state (`followupFlag.flagStatus`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlagStatus {
    #[default]
    NotFlagged,
    Flagged,
    Complete,
}

impl FlagStatus {
    pub fn name(self) -> &'static str {
        match self {
            FlagStatus::NotFlagged => "notFlagged",
            FlagStatus::Flagged => "flagged",
            FlagStatus::Complete => "complete",
        }
    }
    pub fn parse(s: &str) -> FlagStatus {
        match s.trim().to_ascii_lowercase().as_str() {
            "flagged" => FlagStatus::Flagged,
            "complete" => FlagStatus::Complete,
            _ => FlagStatus::NotFlagged,
        }
    }
}

/// An address of a message (`emailAddress {name, address}`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recipient {
    pub name: String,
    pub address: String,
}

fn recipient(v: &Value) -> Option<Recipient> {
    let e = v.get("emailAddress")?;
    let r = Recipient { name: text(e, "name"), address: text(e, "address") };
    (!r.address.is_empty() || !r.name.is_empty()).then_some(r)
}

fn recipients(v: &Value, key: &str) -> Vec<Recipient> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().take(MAX_LIST).filter_map(recipient).collect())
        .unwrap_or_default()
}

/// A message as a delta query or a GET reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphMessage {
    pub id: String,
    /// The delta query reports the message gone from the folder (`@removed`).
    pub removed: bool,
    pub folder: Option<String>,
    pub is_read: bool,
    pub is_draft: bool,
    pub flag: FlagStatus,
    /// `followupFlag.startDateTime.dateTime` as sent (UTC when the zone says so).
    pub flag_start: Option<String>,
    /// `followupFlag.dueDateTime.dateTime` as sent (UTC when the zone says so).
    pub flag_due: Option<String>,
    pub categories: Vec<String>,
    /// `low`, `normal` or `high`.
    pub importance: String,
    pub received: Option<String>,
    pub sent: Option<String>,
    pub subject: String,
    pub from: Option<Recipient>,
    pub sender: Option<Recipient>,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub reply_to: Vec<Recipient>,
    pub internet_message_id: Option<String>,
    pub has_attachments: bool,
    pub preview: String,
    /// Focused Inbox (`inferenceClassification`): `Some(true)` focused, `Some(false)` other.
    pub focused: Option<bool>,
    pub conversation_id: Option<String>,
}

impl GraphMessage {
    /// Reads a `message` resource (or a delta entry); `None` without an id.
    pub fn from_json(v: &Value) -> Option<GraphMessage> {
        let id = opt_text(v, "id")?;
        let removed = v.get("@removed").is_some();
        let flag = v.get("flag");
        let importance = text(v, "importance").to_ascii_lowercase();
        Some(GraphMessage {
            id,
            removed,
            folder: opt_text(v, "parentFolderId"),
            is_read: v.get("isRead").and_then(Value::as_bool).unwrap_or(false),
            is_draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
            flag: flag.map(|f| FlagStatus::parse(&text(f, "flagStatus"))).unwrap_or_default(),
            flag_start: flag
                .and_then(|f| f.get("startDateTime"))
                .map(|d| text(d, "dateTime"))
                .filter(|s| !s.is_empty()),
            flag_due: flag
                .and_then(|f| f.get("dueDateTime"))
                .map(|d| text(d, "dateTime"))
                .filter(|s| !s.is_empty()),
            categories: v
                .get("categories")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .take(MAX_LIST)
                        .filter_map(Value::as_str)
                        .map(|s| s.chars().take(255).collect::<String>())
                        .filter(|s| !s.trim().is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            importance: if importance.is_empty() { "normal".into() } else { importance },
            received: opt_text(v, "receivedDateTime"),
            sent: opt_text(v, "sentDateTime"),
            subject: text(v, "subject"),
            from: v.get("from").and_then(recipient),
            sender: v.get("sender").and_then(recipient),
            to: recipients(v, "toRecipients"),
            cc: recipients(v, "ccRecipients"),
            bcc: recipients(v, "bccRecipients"),
            reply_to: recipients(v, "replyTo"),
            internet_message_id: opt_text(v, "internetMessageId"),
            has_attachments: v.get("hasAttachments").and_then(Value::as_bool).unwrap_or(false),
            preview: text(v, "bodyPreview"),
            focused: v
                .get("inferenceClassification")
                .and_then(Value::as_str)
                .map(|c| !c.eq_ignore_ascii_case("other")),
            conversation_id: opt_text(v, "conversationId"),
        })
    }

    /// The received time (`receivedDateTime`), when the server gave a valid one.
    pub fn received_at(&self) -> Option<DateTime> {
        self.received.as_deref().and_then(|t| DateTime::parse_rfc5322(t).ok())
    }

    /// An RFC 5322 header block standing in for the message until its source is downloaded (headers-only
    /// downloads): From, Sender, To, Cc, Reply-To, Subject, Date, Message-ID and Importance from the
    /// properties, an empty body. Text is sent as UTF-8 (RFC 6532); CR and LF never reach a field.
    pub fn header_block(&self) -> Vec<u8> {
        fn clean(s: &str) -> String {
            s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_owned()
        }
        fn addr(r: &Recipient) -> String {
            let a = clean(&r.address);
            let n = clean(&r.name).replace(['"', '\\'], "");
            if n.is_empty() || n.eq_ignore_ascii_case(&a) {
                format!("<{a}>")
            } else {
                format!("\"{n}\" <{a}>")
            }
        }
        let mut out = String::new();
        let mut field = |name: &str, value: String| {
            if !value.is_empty() {
                out.push_str(name);
                out.push_str(": ");
                out.push_str(&value);
                out.push_str("\r\n");
            }
        };
        let list = |l: &[Recipient]| l.iter().map(addr).collect::<Vec<_>>().join(", ");
        field("From", self.from.as_ref().map(addr).unwrap_or_default());
        if self.sender.is_some() && self.sender != self.from {
            field("Sender", self.sender.as_ref().map(addr).unwrap_or_default());
        }
        field("To", list(&self.to));
        field("Cc", list(&self.cc));
        field("Reply-To", list(&self.reply_to));
        field("Subject", clean(&self.subject));
        let date = self
            .sent
            .as_deref()
            .or(self.received.as_deref())
            .and_then(|t| DateTime::parse_rfc5322(t).ok())
            .map(|d| d.to_rfc5322())
            .unwrap_or_default();
        field("Date", date);
        let mid = self
            .internet_message_id
            .as_deref()
            .map(|m| clean(m).trim_start_matches('<').trim_end_matches('>').to_owned())
            .filter(|m| !m.is_empty() && !m.contains(['<', '>', ' ']))
            .map(|m| format!("<{m}>"))
            .unwrap_or_default();
        field("Message-ID", mid);
        let imp = match self.importance.as_str() {
            "high" => "high",
            "low" => "low",
            _ => "",
        };
        field("Importance", imp.to_owned());
        field("MIME-Version", "1.0".into());
        field("Content-Type", "text/plain; charset=utf-8".into());
        out.push_str("\r\n");
        out.into_bytes()
    }
}

/// One page of a messages delta query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaPage {
    pub messages: Vec<GraphMessage>,
    /// More changes follow at this link.
    pub next: Option<String>,
    /// No more changes: continue from this link next time.
    pub delta: Option<String>,
}

/// Changes to a message's state, sent as one PATCH.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessagePatch {
    pub is_read: Option<bool>,
    /// The flag and, when flagged, its start and due day (`YYYY-MM-DDTHH:MM:SS` in UTC).
    pub flag: Option<(FlagStatus, Option<String>, Option<String>)>,
    /// A flagged message without dates here loses the start and due day it has on the service.
    pub clear_flag_dates: bool,
    pub categories: Option<Vec<String>>,
    pub importance: Option<String>,
}

impl MessagePatch {
    pub fn is_empty(&self) -> bool {
        self.is_read.is_none()
            && self.flag.is_none()
            && self.categories.is_none()
            && self.importance.is_none()
    }

    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(r) = self.is_read {
            m.insert("isRead".into(), json!(r));
        }
        if let Some((status, start, due)) = &self.flag {
            let mut f = serde_json::Map::new();
            f.insert("flagStatus".into(), json!(status.name()));
            // The service needs a start whenever a due date is set (and a due date with a start).
            if *status == FlagStatus::Flagged
                && let Some(d) = due.as_ref().or(start.as_ref())
            {
                let s = start.as_ref().unwrap_or(d);
                f.insert("startDateTime".into(), json!({"dateTime": s, "timeZone": "UTC"}));
                f.insert("dueDateTime".into(), json!({"dateTime": d, "timeZone": "UTC"}));
            } else if *status == FlagStatus::Flagged && self.clear_flag_dates {
                f.insert("startDateTime".into(), Value::Null);
                f.insert("dueDateTime".into(), Value::Null);
            }
            m.insert("flag".into(), Value::Object(f));
        }
        if let Some(c) = &self.categories {
            m.insert("categories".into(), json!(c));
        }
        if let Some(i) = &self.importance {
            m.insert("importance".into(), json!(i));
        }
        Value::Object(m)
    }
}

/// A file to attach to a draft (see [`GraphClient::add_attachment`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAttachment {
    pub name: String,
    pub content_type: String,
    pub content_id: Option<String>,
    pub inline: bool,
    pub data: Vec<u8>,
}

fn b64(data: &[u8]) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.encode(data).into_bytes()
}

impl GraphClient {
    /// The signed-in user (`/me`): display name, mail address, user principal name.
    pub fn me(&self) -> Result<Value> {
        self.get_json("/me?$select=displayName,mail,userPrincipalName")
    }

    /// A well-known folder (`inbox`, `sentitems`, …); `None` when the mailbox has none (404).
    pub fn well_known_folder(&self, name: &str) -> Result<Option<MailFolder>> {
        match self.get_json(&format!("/me/mailFolders/{}", seg(name))) {
            Ok(v) => Ok(MailFolder::from_json(&v).map(|mut f| {
                f.role = WELL_KNOWN.iter().find(|(n, _)| *n == name).map(|(_, r)| *r);
                f
            })),
            Err(NetError::Graph { status: 404, .. }) | Err(NetError::Http { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every mail folder of the mailbox, parents before children (the top level, then each folder's
    /// `childFolders`, breadth first), with the well-known folders marked. At most [`MAX_FOLDERS`] folders
    /// and [`MAX_FOLDER_DEPTH`] levels; a folder listed twice is kept once. [`GraphClient::mail_folder_tree`]
    /// also tells whether the caps left folders out.
    pub fn mail_folders(&self) -> Result<Vec<MailFolder>> {
        self.mail_folder_tree().map(|(list, _)| list)
    }

    /// [`GraphClient::mail_folders`], and whether the list is the whole tree (`false`: [`MAX_FOLDERS`] or
    /// [`MAX_FOLDER_DEPTH`] left folders out, so a folder missing from it may still be on the service). A
    /// cancelled client gets an error, never a part of the tree.
    pub fn mail_folder_tree(&self) -> Result<(Vec<MailFolder>, bool)> {
        let mut out: Vec<MailFolder> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let (top, mut whole) = self.get_all_checked("/me/mailFolders?$top=100", MAX_FOLDERS)?;
        let mut level: Vec<MailFolder> = top.iter().filter_map(MailFolder::from_json).collect();
        let mut depth = 0usize;
        while !level.is_empty() {
            if depth >= MAX_FOLDER_DEPTH {
                whole = false;
                break;
            }
            let mut next = Vec::new();
            for mut f in level {
                if !seen.insert(f.id.clone()) {
                    continue;
                }
                if out.len() >= MAX_FOLDERS {
                    whole = false;
                    continue;
                }
                f.depth = depth;
                if f.child_folder_count > 0 {
                    let path = format!("/me/mailFolders/{}/childFolders?$top=100", seg(&f.id));
                    let room = MAX_FOLDERS.saturating_sub(out.len());
                    let (kids, all) = self.get_all_checked(&path, room)?;
                    whole &= all;
                    next.extend(kids.iter().filter_map(MailFolder::from_json).map(|mut k| {
                        k.parent_folder_id = Some(f.id.clone());
                        k
                    }));
                }
                out.push(f);
            }
            level = next;
            depth = depth.saturating_add(1);
        }
        // The service's own Outbox is marked too, so a client keeping its own can leave it out.
        for (name, role) in WELL_KNOWN.into_iter().chain([("outbox", FolderRole::Outbox)]) {
            if let Some(w) = self.well_known_folder(name)?
                && let Some(f) = out.iter_mut().find(|f| f.id == w.id)
            {
                f.role = Some(role);
            }
        }
        if self.cancelled() {
            return Err(super::cancelled_error());
        }
        Ok((out, whole))
    }

    /// Creates a folder named `name` at the top level or below `parent`.
    pub fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<MailFolder> {
        let path = match parent {
            Some(p) => format!("/me/mailFolders/{}/childFolders", seg(p)),
            None => "/me/mailFolders".to_owned(),
        };
        let v = self.post_json(&path, &json!({"displayName": name}))?;
        let mut f = MailFolder::from_json(&v)
            .ok_or_else(|| NetError::Protocol("Microsoft Graph created a folder without an id".into()))?;
        f.parent_folder_id = parent.map(str::to_owned).or(f.parent_folder_id);
        Ok(f)
    }

    /// Renames a folder.
    pub fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        self.patch_json(&format!("/me/mailFolders/{}", seg(id)), &json!({"displayName": name})).map(|_| ())
    }

    /// Moves a folder below `dest` (a folder id or `msgfolderroot` for the top level); its id may change.
    pub fn move_folder(&self, id: &str, dest: &str) -> Result<MailFolder> {
        let v =
            self.post_json(&format!("/me/mailFolders/{}/move", seg(id)), &json!({"destinationId": dest}))?;
        MailFolder::from_json(&v)
            .ok_or_else(|| NetError::Protocol("Microsoft Graph moved a folder without naming it".into()))
    }

    /// Deletes a folder (with what it holds).
    pub fn delete_folder(&self, id: &str) -> Result<()> {
        self.delete(&format!("/me/mailFolders/{}", seg(id)))
    }

    /// One page of the messages delta query of `folder`: from `link` (a `nextLink` or a saved `deltaLink`),
    /// or a new query (only messages received on or after `received_after`, RFC 3339 UTC, when given).
    pub fn messages_delta(
        &self,
        folder: &str,
        link: Option<&str>,
        received_after: Option<&str>,
    ) -> Result<DeltaPage> {
        let path = match link {
            Some(l) => l.to_owned(),
            None => {
                let mut p =
                    format!("/me/mailFolders/{}/messages/delta?$select={MESSAGE_SELECT}", seg(folder));
                if let Some(after) = received_after {
                    p.push_str("&$filter=");
                    p.push_str(&percent_encode(&format!("receivedDateTime ge {after}")));
                }
                p
            }
        };
        let page = self.get_page(&path)?;
        Ok(DeltaPage {
            messages: page.value.iter().filter_map(GraphMessage::from_json).collect(),
            next: page.next_link,
            delta: page.delta_link,
        })
    }

    /// One message's properties.
    pub fn message(&self, id: &str) -> Result<GraphMessage> {
        let v = self.get_json(&format!("/me/messages/{}?$select={MESSAGE_SELECT}", seg(id)))?;
        GraphMessage::from_json(&v)
            .ok_or_else(|| NetError::Protocol("Microsoft Graph sent a message without an id".into()))
    }

    /// A message's MIME source (`$value`).
    pub fn message_mime(&self, id: &str) -> Result<Vec<u8>> {
        let raw = self.get_bytes(&format!("/me/messages/{}/$value", seg(id)))?;
        if raw.iter().all(u8::is_ascii_whitespace) {
            return Err(NetError::Protocol(format!("Microsoft Graph sent no content for message {id}")));
        }
        Ok(raw)
    }

    /// Changes a message's read state, flag, categories or importance.
    pub fn update_message(&self, id: &str, patch: &MessagePatch) -> Result<()> {
        if patch.is_empty() {
            return Ok(());
        }
        self.patch_json(&format!("/me/messages/{}", seg(id)), &patch.to_json()).map(|_| ())
    }

    /// Moves a message into folder `dest`; returns its id afterwards (the same with immutable ids).
    pub fn move_message(&self, id: &str, dest: &str) -> Result<String> {
        let v = self.post_json(&format!("/me/messages/{}/move", seg(id)), &json!({"destinationId": dest}))?;
        Ok(v.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(id).to_owned())
    }

    /// Deletes a message for good (`permanentDelete`; a service without it gets a plain DELETE, which files
    /// the message in Deleted Items or removes it there). A message already gone counts as deleted.
    pub fn delete_message(&self, id: &str) -> Result<()> {
        let gone = |e: &NetError| {
            matches!(e, NetError::Graph { status: 404, .. } | NetError::Http { status: 404, .. })
        };
        match self.post_json(&format!("/me/messages/{}/permanentDelete", seg(id)), &json!({})) {
            Ok(_) => Ok(()),
            Err(e) if gone(&e) => Ok(()),
            Err(NetError::Graph { status: 400 | 405 | 501, .. })
            | Err(NetError::Http { status: 400 | 405 | 501, .. }) => {
                match self.delete(&format!("/me/messages/{}", seg(id))) {
                    Err(e) if gone(&e) => Ok(()),
                    other => other,
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Creates a message from MIME content in `folder` (the Drafts folder when `None`). The service files it as
    /// a draft. At most [`MAX_INLINE_REQUEST`] bytes of base64; see [`GraphClient::create_large`].
    pub fn create_from_mime(&self, folder: Option<&str>, raw: &[u8]) -> Result<GraphMessage> {
        let path = match folder {
            Some(f) => format!("/me/mailFolders/{}/messages", seg(f)),
            None => "/me/messages".to_owned(),
        };
        let v = self.post_raw(&path, "text/plain", &b64(raw))?;
        GraphMessage::from_json(&v)
            .ok_or_else(|| NetError::Protocol("Microsoft Graph created a message without an id".into()))
    }

    /// Sends MIME content as it is (`sendMail`); the service files a copy in Sent Items.
    pub fn send_mime(&self, raw: &[u8]) -> Result<()> {
        self.post_raw("/me/sendMail", "text/plain", &b64(raw)).map(|_| ())
    }

    /// Sends a draft (`send`); the service files it in Sent Items.
    pub fn send_draft(&self, id: &str) -> Result<()> {
        self.post_json(&format!("/me/messages/{}/send", seg(id)), &json!({})).map(|_| ())
    }

    /// Attaches a file to a draft: in one request when small, else through an upload session in
    /// [`UPLOAD_CHUNK`] pieces.
    pub fn add_attachment(&self, message: &str, a: &FileAttachment) -> Result<()> {
        let size = a.data.len();
        let ctype =
            if a.content_type.trim().is_empty() { "application/octet-stream" } else { &a.content_type };
        if size.saturating_mul(4) / 3 < MAX_INLINE_REQUEST.saturating_sub(64 * 1024) {
            let mut body = json!({
                "@odata.type": "#microsoft.graph.fileAttachment",
                "name": a.name,
                "contentType": ctype,
                "isInline": a.inline,
                "contentBytes": String::from_utf8_lossy(&b64(&a.data)),
            });
            if let (Some(cid), Some(o)) = (&a.content_id, body.as_object_mut()) {
                o.insert("contentId".into(), json!(cid));
            }
            return self.post_json(&format!("/me/messages/{}/attachments", seg(message)), &body).map(|_| ());
        }
        let mut item = json!({"attachmentType": "file", "name": a.name, "size": size, "isInline": a.inline});
        if let (Some(cid), Some(o)) = (&a.content_id, item.as_object_mut()) {
            o.insert("contentId".into(), json!(cid));
        }
        let session = self.post_json(
            &format!("/me/messages/{}/attachments/createUploadSession", seg(message)),
            &json!({"AttachmentItem": item}),
        )?;
        let url = session
            .get("uploadUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                NetError::Protocol("Microsoft Graph opened an upload session without a URL".into())
            })?
            .to_owned();
        let mut start = 0usize;
        let mut rounds = 0usize;
        while start < size {
            rounds = rounds.saturating_add(1);
            if rounds > size / UPLOAD_CHUNK + 8 {
                return Err(NetError::Protocol("the upload session never finished".into()));
            }
            let end = start.saturating_add(UPLOAD_CHUNK).min(size);
            let chunk = a.data.get(start..end).unwrap_or_default();
            let v = self.put_range(&url, start, size, chunk)?;
            // The service names the next range it wants; otherwise the next piece follows this one.
            let wanted = v
                .get("nextExpectedRanges")
                .and_then(Value::as_array)
                .and_then(|r| r.first())
                .and_then(Value::as_str)
                .and_then(|r| r.split('-').next())
                .and_then(|n| n.trim().parse::<usize>().ok());
            start = match wanted {
                Some(n) if n > start && n <= size => n,
                _ => end,
            };
        }
        Ok(())
    }
}
