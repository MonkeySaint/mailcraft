//! Building outgoing messages.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::addr::{format_address_list, format_mailbox};
use crate::header::fold;
use crate::params::format_param;
use crate::{Address, DateTime, Mailbox};

/// Per-process sequence number mixed into generated Message-IDs and boundaries.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentSpec {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
    /// Set for inline images referenced as `cid:` from the HTML body.
    pub content_id: Option<String>,
}

/// Builder for RFC 5322 messages. Structure chosen automatically: text only → text/plain; text + html →
/// multipart/alternative; inline cid parts → multipart/related; attachments → multipart/mixed. Text parts use
/// UTF-8 with quoted-printable (or 7bit when pure ASCII with short lines). CRLF line endings, headers folded at 78.
#[derive(Clone, Debug, Default)]
pub struct MessageBuilder {
    pub from: Option<Mailbox>,
    pub sender: Option<Mailbox>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub date: Option<DateTime>,
    /// Without angle brackets; generated from `date` + random-ish token + from-domain when `None`.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    /// "high" | "low"; `None` = normal (writes Importance and X-Priority).
    pub importance: Option<String>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentSpec>,
    /// Extra raw headers (name, value), e.g. Disposition-Notification-To, Return-Receipt-To.
    pub extra_headers: Vec<(String, String)>,
    /// When false, Bcc is omitted from the output (sending); true keeps it (drafts / Sent Items copy).
    pub keep_bcc: bool,
    /// Charset of the text parts (a label such as `iso-8859-1` or `windows-1252`); `None` = UTF-8. A text the
    /// charset cannot represent goes out in UTF-8 instead (see [`MessageBuilder::text_charset`]).
    pub charset: Option<String>,
}

/// 64-bit FNV-1a: small, deterministic across runs and platforms (unlike `DefaultHasher`).
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        // Field separator so ("ab","c") and ("a","bc") differ.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
    }
}

/// One MIME entity being serialised.
struct Entity {
    headers: Vec<(String, String)>,
    body: Body,
}

enum Body {
    /// Already transfer-encoded content. `raw` marks 7bit/8bit content that could contain a boundary.
    Encoded {
        data: Vec<u8>,
        raw: bool,
    },
    Multipart {
        subtype: &'static str,
        extra: Vec<(String, String)>,
        children: Vec<Entity>,
    },
}

impl Entity {
    /// Serialises the entity, choosing boundaries that do not occur in raw child content.
    fn write(&self, out: &mut Vec<u8>, salt: u64, counter: &mut u32) {
        match &self.body {
            Body::Encoded { data, .. } => {
                write_headers(out, &self.headers);
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(data);
            }
            Body::Multipart { subtype, extra, children } => {
                let mut rendered: Vec<Vec<u8>> = Vec::with_capacity(children.len());
                for c in children {
                    let mut buf = Vec::new();
                    c.write(&mut buf, salt, counter);
                    rendered.push(buf);
                }
                let boundary = loop {
                    *counter = counter.wrapping_add(1);
                    let b = format!("----=_MC_{salt:016x}_{:04}", *counter);
                    let delim = format!("--{b}");
                    let collides = children
                        .iter()
                        .zip(&rendered)
                        .any(|(c, r)| c.has_raw() && contains(r, delim.as_bytes()));
                    if !collides || *counter > 10_000 {
                        break b;
                    }
                };
                let mut ct = format!("multipart/{subtype}; boundary=\"{boundary}\"");
                for (k, v) in extra {
                    ct.push_str("; ");
                    ct.push_str(&format_param(k, v));
                }
                let mut headers = self.headers.clone();
                headers.push(("Content-Type".into(), ct));
                write_headers(out, &headers);
                out.extend_from_slice(b"\r\n");
                for r in &rendered {
                    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                    out.extend_from_slice(r);
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            }
        }
    }

    fn has_raw(&self) -> bool {
        match &self.body {
            Body::Encoded { raw, .. } => *raw,
            Body::Multipart { children, .. } => children.iter().any(Entity::has_raw),
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

fn write_headers(out: &mut Vec<u8>, headers: &[(String, String)]) {
    for (k, v) in headers {
        out.extend_from_slice(fold(k, v).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
}

/// Converts bare LF (and bare CR) to CRLF.
fn to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 32);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                out.push_str("\r\n");
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => out.push_str("\r\n"),
            _ => out.push(c),
        }
    }
    out
}

/// True when `text` can go out as 7bit without risk: ASCII, no control characters other than line breaks and
/// tabs, lines ≤ 78 chars, no trailing whitespace, no bare CR, no mbox-sensitive `From ` line starts.
fn fits_7bit(text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    for line in text.split('\n') {
        if line.contains('\r') || line.len() > 78 || line.ends_with([' ', '\t']) || line.starts_with("From ")
        {
            return false;
        }
        if line.bytes().any(|b| b < 32 && b != b'\t' || b == 127) {
            return false;
        }
    }
    true
}

/// A text part in `charset` (a label [`crate::charset::encode`] accepts; UTF-8 when it cannot represent
/// `text`). An HTML part in another charset than UTF-8 has its own charset declaration rewritten to match
/// ([`retarget_meta_charset`]).
fn text_entity(text: &str, subtype: &str, charset: Option<&str>) -> Entity {
    let encoded = charset.and_then(|c| {
        let label = charset_label(c);
        let text = if subtype == "html" && label != "utf-8" {
            std::borrow::Cow::Owned(retarget_meta_charset(text, &label))
        } else {
            std::borrow::Cow::Borrowed(text)
        };
        crate::charset::encode(&text, c).map(|b| (label, text, b))
    });
    let (label, text, bytes) = encoded
        .unwrap_or_else(|| ("utf-8".to_owned(), std::borrow::Cow::Borrowed(text), text.as_bytes().to_vec()));
    let (data, cte, raw) = if fits_7bit(&text) {
        (to_crlf(&text).into_bytes(), "7bit", true)
    } else {
        (crate::transfer::encode_qp(&bytes).into_bytes(), "quoted-printable", false)
    };
    Entity {
        headers: vec![
            ("Content-Type".into(), format!("text/{subtype}; charset=\"{label}\"")),
            ("Content-Transfer-Encoding".into(), cte.into()),
        ],
        body: Body::Encoded { data, raw },
    }
}

/// `html` with the charset it declares for itself set to `label`: the value after `charset=` in every
/// `<meta>` tag before `<body` (`<meta charset="utf-8">` and the `content` of
/// `<meta http-equiv="Content-Type" content="text/html; charset=utf-8">`), so a saved copy or a reader that
/// honours the markup decodes the bytes in the charset the Content-Type names. `label` is a token
/// ([`charset_label`]).
fn retarget_meta_charset(html: &str, label: &str) -> String {
    // ASCII lower-casing keeps every byte offset, so offsets found in `lower` are char boundaries in `html`
    // (they all sit on ASCII bytes).
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let head_end = lower.find("<body").unwrap_or(lower.len());
    let mut out = String::with_capacity(html.len().saturating_add(16));
    let (mut copied, mut at) = (0, 0);
    while let Some(rel) = lower.get(at..head_end).and_then(|h| h.find("<meta")) {
        let tag = at.saturating_add(rel);
        let tag_end =
            lower.get(tag..).and_then(|t| t.find('>')).map_or(lower.len(), |e| tag.saturating_add(e));
        if let Some(c) = lower.get(tag..tag_end).and_then(|t| t.find("charset=")) {
            let mut start = tag.saturating_add(c).saturating_add("charset=".len());
            if bytes.get(start).is_some_and(|b| matches!(b, b'"' | b'\'')) {
                start = start.saturating_add(1);
            }
            let mut end = start;
            while end < tag_end
                && bytes.get(end).is_some_and(|b| {
                    !matches!(b, b'"' | b'\'' | b';' | b'/' | b'>') && !b.is_ascii_whitespace()
                })
            {
                end = end.saturating_add(1);
            }
            out.push_str(html.get(copied..start).unwrap_or_default());
            out.push_str(label);
            copied = end;
        }
        at = tag_end.max(tag.saturating_add(1));
        if at >= head_end {
            break;
        }
    }
    out.push_str(html.get(copied..).unwrap_or_default());
    out
}

/// The charset parameter written for `label`: lower-cased, only token characters.
fn charset_label(label: &str) -> String {
    let l: String = crate::charset::normalize_label(label)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        .take(40)
        .collect();
    if l.is_empty() { "utf-8".to_owned() } else { l }
}

fn clean_line(s: &str) -> String {
    s.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect()
}

fn attachment_entity(a: &AttachmentSpec, inline: bool) -> Entity {
    let parsed = crate::ContentType::parse(&a.mime_type);
    // An invalid type (parsed as the text/plain default), or a multipart type on what is a single base64 leaf,
    // goes out as an opaque file; otherwise the caller's parameters (method=, charset=, ...) are kept.
    let invalid = a.mime_type.trim().is_empty()
        || (parsed.mime_type == "text/plain" && !a.mime_type.to_ascii_lowercase().contains("text/plain"))
        || parsed.is_multipart();
    let (mime, params) = if invalid {
        ("application/octet-stream".to_owned(), Vec::new())
    } else {
        (parsed.mime_type, parsed.params)
    };
    let name: String = a.filename.chars().filter(|c| !c.is_control()).collect();
    let mut ct = mime.clone();
    for (k, v) in params.iter().filter(|(k, _)| k != "name") {
        ct.push_str("; ");
        ct.push_str(&format_param(k, v));
    }
    if !name.is_empty() {
        ct.push_str("; ");
        if crate::encoded::needs_encoding(&name) && !name.contains("=?") {
            // Widely understood legacy form for the Content-Type name; the RFC 2231 form goes in the disposition.
            ct.push_str(&format!("name=\"{}\"", crate::encoded::encode_run_limited(&name, 64)));
        } else {
            ct.push_str(&format_param("name", &name));
        }
    }
    let mut disp = if inline { "inline".to_owned() } else { "attachment".to_owned() };
    if !name.is_empty() {
        disp.push_str("; ");
        disp.push_str(&format_param("filename", &name));
    }
    let mut headers = vec![("Content-Type".to_owned(), ct), ("Content-Disposition".to_owned(), disp)];
    if let Some(cid) = &a.content_id {
        let cid: String = cid.chars().filter(|c| !c.is_whitespace() && *c != '<' && *c != '>').collect();
        if !cid.is_empty() {
            headers.push(("Content-ID".into(), format!("<{cid}>")));
        }
    }
    let (data, raw) = if mime == "message/rfc822" || mime == "message/global" {
        // RFC 2046 §5.2.1: message/rfc822 must not be base64-encoded.
        let cte = if a.data.is_ascii() { "7bit" } else { "8bit" };
        headers.push(("Content-Transfer-Encoding".into(), cte.into()));
        (a.data.clone(), true)
    } else {
        headers.push(("Content-Transfer-Encoding".into(), "base64".into()));
        (crate::transfer::encode_base64(&a.data).into_bytes(), false)
    };
    Entity { headers, body: Body::Encoded { data, raw } }
}

fn domain_of(addr: &str) -> String {
    let d = addr.rsplit_once('@').map(|(_, d)| d).unwrap_or_default();
    let d: String = d.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-').collect();
    let d = d.trim_matches('.');
    if d.is_empty() { "mailcraft.invalid".to_owned() } else { d.to_owned() }
}

fn clean_id(id: &str) -> String {
    id.chars().filter(|c| !c.is_whitespace() && *c != '<' && *c != '>' && !c.is_control()).collect()
}

fn id_list(ids: &[String]) -> String {
    ids.iter()
        .map(|i| clean_id(i))
        .filter(|i| !i.is_empty())
        .map(|i| format!("<{i}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Header names the builder writes itself (or must never write from `extra_headers`, like Bcc when sending);
/// extra headers with these names are ignored.
const RESERVED: [&str; 17] = [
    "content-type",
    "content-transfer-encoding",
    "mime-version",
    "content-disposition",
    "from",
    "sender",
    "reply-to",
    "to",
    "cc",
    "bcc",
    "subject",
    "date",
    "message-id",
    "in-reply-to",
    "references",
    "importance",
    "x-priority",
];

impl MessageBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Message-ID that [`MessageBuilder::build`] would generate for this builder with sequence number `seq`:
    /// `<hex date>.<hash of date, subject, from>.<seq>@<from domain>`.
    pub fn generate_message_id(&self, date: DateTime, seq: u64) -> String {
        let mut h = Fnv::new();
        h.write(&date.unix.to_be_bytes());
        h.write(self.subject.as_bytes());
        h.write(self.from.as_ref().map(|m| m.addr.as_str()).unwrap_or_default().as_bytes());
        h.write(&seq.to_be_bytes());
        let domain = domain_of(self.from.as_ref().map(|m| m.addr.as_str()).unwrap_or_default());
        format!("{:x}.{:016x}.{seq}@{domain}", date.unix.max(0), h.0)
    }

    /// The charset the text parts go out in: [`MessageBuilder::charset`] when it can represent both the text
    /// and the HTML body, else UTF-8.
    pub fn text_charset(&self) -> String {
        match self.charset.as_deref() {
            Some(c)
                if [&self.text, &self.html]
                    .into_iter()
                    .flatten()
                    .all(|t| crate::charset::encode(t, c).is_some()) =>
            {
                charset_label(c)
            }
            _ => "utf-8".to_owned(),
        }
    }

    /// Serialises to RFC 5322 bytes.
    pub fn build(&self) -> Vec<u8> {
        let date = self.date.unwrap_or_else(DateTime::now_utc);
        let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(f) = &self.from {
            headers.push(("From".into(), format_mailbox(f, true)));
        }
        if let Some(s) = &self.sender {
            headers.push(("Sender".into(), format_mailbox(s, true)));
        }
        if !self.reply_to.is_empty() {
            headers.push(("Reply-To".into(), format_address_list(&self.reply_to, true)));
        }
        if !self.to.is_empty() {
            headers.push(("To".into(), format_address_list(&self.to, true)));
        }
        if !self.cc.is_empty() {
            headers.push(("Cc".into(), format_address_list(&self.cc, true)));
        }
        if self.keep_bcc && !self.bcc.is_empty() {
            headers.push(("Bcc".into(), format_address_list(&self.bcc, true)));
        }
        headers.push(("Subject".into(), crate::encoded::encode_words(&self.subject)));
        headers.push(("Date".into(), date.to_rfc5322()));
        let id = self.message_id.as_deref().map(clean_id).filter(|i| !i.is_empty());
        let id = id.unwrap_or_else(|| self.generate_message_id(date, seq));
        headers.push(("Message-ID".into(), format!("<{id}>")));
        let irt = id_list(&self.in_reply_to);
        if !irt.is_empty() {
            headers.push(("In-Reply-To".into(), irt));
        }
        let refs = id_list(&self.references);
        if !refs.is_empty() {
            headers.push(("References".into(), refs));
        }
        match self.importance.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("high") => {
                headers.push(("Importance".into(), "high".into()));
                headers.push(("X-Priority".into(), "1 (Highest)".into()));
            }
            Some("low") => {
                headers.push(("Importance".into(), "low".into()));
                headers.push(("X-Priority".into(), "5 (Lowest)".into()));
            }
            _ => {}
        }
        for (k, v) in &self.extra_headers {
            let k = k.trim();
            let valid = !k.is_empty() && k.chars().all(|c| c.is_ascii_graphic() && c != ':');
            if valid && !RESERVED.contains(&k.to_ascii_lowercase().as_str()) {
                headers.push((k.to_owned(), clean_line(v)));
            }
        }
        headers.push(("MIME-Version".into(), "1.0".into()));

        let mut root = self.body_entity();
        let mut all = headers;
        all.append(&mut root.headers);
        root.headers = all;
        let mut salt = Fnv::new();
        salt.write(&date.unix.to_be_bytes());
        salt.write(id.as_bytes());
        let mut out = Vec::new();
        let mut counter = 0u32;
        root.write(&mut out, salt.0, &mut counter);
        out
    }

    /// The MIME tree for the body and attachments (structure as documented on [`MessageBuilder`]).
    fn body_entity(&self) -> Entity {
        let has_html = self.html.is_some();
        let (inline, regular): (Vec<&AttachmentSpec>, Vec<&AttachmentSpec>) = self
            .attachments
            .iter()
            .partition(|a| has_html && a.content_id.as_deref().is_some_and(|c| !c.trim().is_empty()));
        let cs = self.charset.as_deref();
        let html_entity = |h: &str| {
            let e = text_entity(h, "html", cs);
            if inline.is_empty() {
                e
            } else {
                let mut children = vec![e];
                children.extend(inline.iter().map(|a| attachment_entity(a, true)));
                Entity {
                    headers: Vec::new(),
                    body: Body::Multipart {
                        subtype: "related",
                        extra: vec![("type".into(), "text/html".into())],
                        children,
                    },
                }
            }
        };
        let body = match (&self.text, &self.html) {
            (text, None) => text_entity(text.as_deref().unwrap_or_default(), "plain", cs),
            (None, Some(h)) => html_entity(h),
            (Some(t), Some(h)) => Entity {
                headers: Vec::new(),
                body: Body::Multipart {
                    subtype: "alternative",
                    extra: Vec::new(),
                    children: vec![text_entity(t, "plain", cs), html_entity(h)],
                },
            },
        };
        if regular.is_empty() {
            return body;
        }
        let mut children = vec![body];
        children.extend(regular.iter().map(|a| attachment_entity(a, false)));
        Entity {
            headers: Vec::new(),
            body: Body::Multipart { subtype: "mixed", extra: Vec::new(), children },
        }
    }
}
