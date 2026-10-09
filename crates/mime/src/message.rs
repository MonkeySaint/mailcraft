//! MIME tree and the parsed message.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Address, DateTime, Headers, Mailbox};

/// Parsed `Content-Type` (type/subtype lower-cased; parameter names lower-cased; RFC 2231 continuations and
/// charsets already joined/decoded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentType {
    /// e.g. "text/plain", "multipart/alternative".
    pub mime_type: String,
    pub params: Vec<(String, String)>,
}

impl Default for ContentType {
    fn default() -> Self {
        Self { mime_type: "text/plain".into(), params: vec![("charset".into(), "us-ascii".into())] }
    }
}

impl ContentType {
    /// Parses a Content-Type value. An invalid or missing type becomes `text/plain` (RFC 2045 §5.2), keeping any
    /// parameters that were given.
    pub fn parse(value: &str) -> Self {
        let (main, params) = crate::params::parse_header_params(value);
        let main = main.to_ascii_lowercase();
        let valid = match main.split_once('/') {
            Some((t, s)) => {
                !t.is_empty()
                    && !s.is_empty()
                    && t.chars().all(crate::params::is_token_char)
                    && s.chars().all(crate::params::is_token_char)
            }
            None => false,
        };
        let mime_type = if valid { main } else { "text/plain".to_owned() };
        Self { mime_type, params }
    }
    /// `"text"` for `"text/plain"`.
    pub fn main_type(&self) -> &str {
        self.mime_type.split('/').next().unwrap_or_default()
    }
    /// `"plain"` for `"text/plain"`.
    pub fn subtype(&self) -> &str {
        self.mime_type.split_once('/').map(|(_, s)| s).unwrap_or_default()
    }
    pub fn is_text(&self) -> bool {
        self.mime_type.starts_with("text/")
    }
    /// `message/rfc822` or `message/global` (an attached message).
    pub fn is_message(&self) -> bool {
        self.mime_type == "message/rfc822" || self.mime_type == "message/global"
    }
    /// Formats as a header value (`type/subtype; name=value; ...`).
    pub fn to_header_value(&self) -> String {
        let mut s = self.mime_type.clone();
        for (k, v) in &self.params {
            s.push_str("; ");
            s.push_str(&crate::params::format_param(k, v));
        }
        s
    }
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn is_multipart(&self) -> bool {
        self.mime_type.starts_with("multipart/")
    }
    pub fn charset(&self) -> &str {
        self.param("charset").unwrap_or("us-ascii")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    #[default]
    None,
    Inline,
    Attachment,
}

impl Disposition {
    /// Parses a Content-Disposition value into the disposition and its parameters. Unknown dispositions are
    /// treated as `attachment` (RFC 2183 §2.8); an empty value is `None`.
    pub fn parse(value: &str) -> (Disposition, Vec<(String, String)>) {
        let (main, params) = crate::params::parse_header_params(value);
        let d = match main.to_ascii_lowercase().as_str() {
            "" => Disposition::None,
            "inline" => Disposition::Inline,
            _ => Disposition::Attachment,
        };
        (d, params)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartBody {
    /// Leaf body with the transfer encoding already removed (still in its charset).
    Leaf(Vec<u8>),
    Multipart(Vec<Part>),
    /// `message/rfc822` (forwarded as attachment).
    Message(Box<Message>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub headers: Headers,
    pub content_type: ContentType,
    pub disposition: Disposition,
    /// Decoded filename from Content-Disposition `filename` or Content-Type `name`.
    pub filename: Option<String>,
    /// Content-ID without angle brackets.
    pub content_id: Option<String>,
    pub body: PartBody,
}

/// A file-like part for the attachment well.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub content_id: Option<String>,
    /// True when referenced inline (e.g. `cid:` image in the HTML body).
    pub inline: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub headers: Headers,
    pub root: Part,
}

/// Parses an RFC 5322 message (CRLF or LF). Never panics; caps per crate constants.
///
/// Only an empty (or all-whitespace) input is an error; anything else yields a best-effort tree: a missing
/// header block gives a header-less text/plain message, a multipart without a usable boundary keeps its body as a
/// single text part, a missing final boundary ends the last part at the end of the data, and nesting or part
/// counts beyond [`crate::MAX_DEPTH`] / [`crate::MAX_PARTS`] are kept as undecoded leaves.
pub fn parse(bytes: &[u8]) -> crate::Result<Message> {
    if bytes.len() > crate::MAX_MESSAGE_BYTES {
        return Err(crate::MimeError::TooLarge(bytes.len()));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(crate::MimeError::Malformed("empty message".into()));
    }
    Ok(parse_with_budget(bytes, NESTED_DECODE_BUDGET))
}

/// Most bytes held at once in transfer-decoded copies of nested `message/rfc822` bodies (each nesting level that
/// is base64 / quoted-printable encoded needs its own decoded copy while its children are parsed). Past this, a
/// nested message stays an undecoded-structure leaf (still offered as an attachment).
const NESTED_DECODE_BUDGET: usize = crate::MAX_MESSAGE_BYTES;

/// Parse-wide limits.
struct Ctx {
    /// Parts created so far (capped by [`crate::MAX_PARTS`]).
    count: usize,
    /// Bytes still available for decoded nested-message copies on the current nesting path.
    budget: usize,
}

fn parse_with_budget(bytes: &[u8], budget: usize) -> Message {
    let mut ctx = Ctx { count: 0, budget };
    parse_message(bytes, 0, &mut ctx)
}

fn parse_message(bytes: &[u8], depth: usize, ctx: &mut Ctx) -> Message {
    let root = parse_part(bytes, depth, ctx, &ContentType::default());
    Message { headers: root.headers.clone(), root }
}

fn digest_default() -> ContentType {
    ContentType { mime_type: "message/rfc822".into(), params: Vec::new() }
}

fn parse_part(bytes: &[u8], depth: usize, ctx: &mut Ctx, default_ct: &ContentType) -> Part {
    ctx.count = ctx.count.saturating_add(1);
    let (headers, off) = Headers::parse(bytes);
    let raw_body = bytes.get(off..).unwrap_or_default();
    let content_type = match headers.get("Content-Type") {
        Some(v) if !v.trim().is_empty() => ContentType::parse(v),
        _ => default_ct.clone(),
    };
    let (disposition, dparams) =
        headers.get("Content-Disposition").map(Disposition::parse).unwrap_or_default();
    let filename = dparams
        .iter()
        .find(|(k, _)| k == "filename")
        .map(|(_, v)| v.clone())
        .or_else(|| content_type.param("name").map(str::to_owned))
        .map(|f| f.trim().to_owned())
        .filter(|f| !f.is_empty());
    let content_id = headers.get("Content-ID").map(strip_angle).filter(|c| !c.is_empty());
    let cte = headers.get("Content-Transfer-Encoding").unwrap_or_default().to_owned();
    let can_nest = depth < crate::MAX_DEPTH && ctx.count < crate::MAX_PARTS;
    let body = if content_type.is_multipart() && can_nest {
        let child_default = if content_type.mime_type == "multipart/digest" {
            digest_default()
        } else {
            ContentType::default()
        };
        let boundary = content_type.param("boundary").unwrap_or_default();
        match split_multipart(raw_body, boundary) {
            Some(slices) => {
                let mut children = Vec::with_capacity(slices.len());
                for slice in slices {
                    if ctx.count >= crate::MAX_PARTS {
                        break;
                    }
                    children.push(parse_part(slice, depth + 1, ctx, &child_default));
                }
                PartBody::Multipart(children)
            }
            None => {
                // No usable boundary: keep the body as one plain-text child so it can still be read.
                ctx.count = ctx.count.saturating_add(1);
                PartBody::Multipart(vec![Part {
                    headers: Headers::default(),
                    content_type: ContentType::default(),
                    disposition: Disposition::None,
                    filename: None,
                    content_id: None,
                    body: PartBody::Leaf(crate::transfer::decode(raw_body, &cte)),
                }])
            }
        }
    } else if content_type.is_message() && can_nest {
        let decoded = crate::transfer::decode_cow(raw_body, &cte);
        // Only an actual decoded copy counts against the budget; identity encodings are parsed in place.
        let cost = match &decoded {
            std::borrow::Cow::Borrowed(_) => 0,
            std::borrow::Cow::Owned(v) => v.len(),
        };
        if cost <= ctx.budget {
            ctx.budget -= cost;
            let inner = parse_message(&decoded, depth + 1, ctx);
            ctx.budget += cost;
            PartBody::Message(Box::new(inner))
        } else {
            PartBody::Leaf(decoded.into_owned())
        }
    } else {
        PartBody::Leaf(crate::transfer::decode(raw_body, &cte))
    };
    Part { headers, content_type, disposition, filename, content_id, body }
}

/// Removes surrounding whitespace, angle brackets and comments-free padding from an id (`<a@b>` → `a@b`).
fn strip_angle(v: &str) -> String {
    let t = v.trim();
    let t = match (t.find('<'), t.rfind('>')) {
        (Some(a), Some(b)) if a < b => t.get(a + 1..b).unwrap_or(t),
        _ => t.trim_start_matches('<').trim_end_matches('>'),
    };
    t.split_whitespace().collect()
}

/// Splits a multipart body at `--boundary` delimiter lines. Returns `None` when the boundary is empty or no
/// delimiter is found. The preamble and epilogue are discarded; a missing close delimiter ends the last part at
/// the end of the body. The line break before each delimiter belongs to the delimiter (RFC 2046 §5.1.1).
pub fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Option<Vec<&'a [u8]>> {
    if boundary.is_empty() || boundary.len() > 200 {
        return None;
    }
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut found = false;
    let mut line_start = 0usize;
    while line_start <= body.len() {
        let rest = body.get(line_start..).unwrap_or_default();
        let line_end = rest.iter().position(|&b| b == b'\n').map_or(body.len(), |p| line_start + p);
        let line = body.get(line_start..line_end).unwrap_or_default();
        if let Some(after) = line.strip_prefix(delim) {
            let (close, after) = match after.strip_prefix(b"--") {
                Some(a) => (true, a),
                None => (false, after),
            };
            if after.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                found = true;
                if let Some(s) = cur_start.take() {
                    let mut end = line_start;
                    if end > s && body.get(end - 1) == Some(&b'\n') {
                        end -= 1;
                    }
                    if end > s && body.get(end - 1) == Some(&b'\r') {
                        end -= 1;
                    }
                    parts.push(body.get(s..end.max(s)).unwrap_or_default());
                    if parts.len() >= crate::MAX_PARTS {
                        return Some(parts);
                    }
                }
                if close {
                    return Some(parts);
                }
                cur_start = Some((line_end + 1).min(body.len()));
            }
        }
        if line_end >= body.len() {
            break;
        }
        line_start = line_end + 1;
    }
    if let Some(s) = cur_start {
        parts.push(body.get(s..).unwrap_or_default());
    }
    if found { Some(parts) } else { None }
}

/// Extracts `<id>` tokens from a Message-ID / In-Reply-To / References value (angle brackets removed). Values
/// without brackets fall back to whitespace-separated words.
pub fn parse_msg_ids(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(a) = rest.find('<') {
        let after = rest.get(a + 1..).unwrap_or_default();
        let Some(b) = after.find('>') else { break };
        let id: String = after.get(..b).unwrap_or_default().split_whitespace().collect();
        if !id.is_empty() && out.len() < 10_000 {
            out.push(id);
        }
        rest = after.get(b + 1..).unwrap_or_default();
    }
    if out.is_empty() {
        out = value
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(|w| w.trim_matches(|c| c == '<' || c == '>'))
            .filter(|w| !w.is_empty() && !w.starts_with('('))
            .take(10_000)
            .map(str::to_owned)
            .collect();
    }
    out
}

/// Charset declared in an HTML `<meta>` within the first 2 KB, if any.
fn sniff_html_charset(bytes: &[u8]) -> Option<String> {
    let head = bytes.get(..bytes.len().min(2048)).unwrap_or_default();
    let lower: Vec<u8> = head.to_ascii_lowercase();
    let pos = lower.windows(8).position(|w| w == b"charset=")?;
    let rest = lower.get(pos + 8..)?;
    let rest = rest.strip_prefix(b"\"").or_else(|| rest.strip_prefix(b"'")).unwrap_or(rest);
    let end = rest
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')))
        .unwrap_or(rest.len());
    let cs = std::str::from_utf8(rest.get(..end)?).ok()?;
    if cs.is_empty() { None } else { Some(cs.to_owned()) }
}

/// Decodes `format=flowed` text (RFC 3676 §4): soft line breaks (a trailing space) are joined with the next
/// line of the same quote depth, space-stuffing is removed, and the `-- ` signature separator stays a hard break.
/// Input uses LF line endings.
pub fn unflow(text: &str, delsp: bool) -> String {
    let mut out = String::with_capacity(text.len());
    // Pending paragraph: (quote depth, accumulated text).
    let mut pending: Option<(usize, String)> = None;
    let finish = |out: &mut String, depth: usize, content: &str| {
        for _ in 0..depth {
            out.push('>');
        }
        if depth > 0 && !content.is_empty() {
            out.push(' ');
        }
        out.push_str(content);
        out.push('\n');
    };
    let ends_with_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    for line in body.split('\n') {
        let depth = line.chars().take_while(|&c| c == '>').count();
        let mut content = line.get(depth..).unwrap_or_default();
        if let Some(stripped) = content.strip_prefix(' ') {
            content = stripped;
        }
        let soft = content.ends_with(' ') && content != "-- ";
        // A change of quote depth ends the paragraph even after a soft break.
        if let Some((d, acc)) = pending.take() {
            if d == depth {
                let mut acc = acc;
                acc.push_str(content);
                pending = Some((d, acc));
            } else {
                finish(&mut out, d, &acc);
                pending = Some((depth, content.to_owned()));
            }
        } else {
            pending = Some((depth, content.to_owned()));
        }
        if soft {
            if delsp && let Some((_, acc)) = pending.as_mut() {
                acc.pop();
            }
        } else if let Some((d, acc)) = pending.take() {
            finish(&mut out, d, &acc);
        }
    }
    if let Some((d, acc)) = pending.take() {
        finish(&mut out, d, &acc);
    }
    if !ends_with_newline {
        out.pop();
    }
    out
}

fn crlf_to_lf(s: String) -> String {
    if s.contains("\r\n") { s.replace("\r\n", "\n") } else { s }
}

/// Common extensions for generated attachment names.
fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/tiff" => "tif",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "htm",
        "text/calendar" => "ics",
        "text/csv" => "csv",
        "text/vcard" | "text/x-vcard" => "vcf",
        "application/zip" => "zip",
        "message/rfc822" | "message/global" => "eml",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        _ => "dat",
    }
}

/// Last path component with control characters removed and surrounding whitespace trimmed.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let t = clean.trim().trim_end_matches('.').trim();
    if t == "." || t == ".." { String::new() } else { t.to_owned() }
}

impl Part {
    pub fn mime_type(&self) -> &str {
        &self.content_type.mime_type
    }
    /// Leaf data (transfer encoding removed), `None` for containers.
    pub fn data(&self) -> Option<&[u8]> {
        match &self.body {
            PartBody::Leaf(d) => Some(d),
            _ => None,
        }
    }
    /// Leaf data decoded with the part's charset (HTML `<meta charset>` sniffed when the header has none),
    /// CRLF normalised to LF. `None` for containers.
    pub fn text(&self) -> Option<String> {
        let data = self.data()?;
        let charset = match self.content_type.param("charset") {
            Some(c) if !c.trim().is_empty() => c.to_owned(),
            _ if self.content_type.mime_type == "text/html" => {
                sniff_html_charset(data).unwrap_or_else(|| "utf-8".into())
            }
            _ => "us-ascii".to_owned(),
        };
        let text = crlf_to_lf(crate::charset::decode(data, &charset));
        let flowed = self.content_type.mime_type == "text/plain"
            && self.content_type.param("format").is_some_and(|f| f.eq_ignore_ascii_case("flowed"));
        if flowed {
            let delsp = self.content_type.param("delsp").is_some_and(|d| d.eq_ignore_ascii_case("yes"));
            return Some(unflow(&text, delsp));
        }
        Some(text)
    }
    /// True when this leaf is meant as a file rather than body text.
    pub fn is_attachment(&self) -> bool {
        self.disposition == Disposition::Attachment
            || matches!(self.body, PartBody::Message(_))
            || (self.filename.is_some() && !self.content_type.is_text())
    }
    /// Serialises this part (headers as stored, folded; bodies re-encoded per their Content-Transfer-Encoding).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.headers.to_bytes());
        out.extend_from_slice(b"\r\n");
        match &self.body {
            PartBody::Leaf(data) => self.write_encoded(data, out),
            PartBody::Multipart(children) => {
                if let Some(encoded) = self.unsplit_body() {
                    out.extend_from_slice(&encoded);
                    return;
                }
                match self.content_type.param("boundary").filter(|b| !b.is_empty()) {
                    Some(b) => {
                        for c in children {
                            out.extend_from_slice(format!("--{b}\r\n").as_bytes());
                            c.write(out);
                            out.extend_from_slice(b"\r\n");
                        }
                        out.extend_from_slice(format!("--{b}--\r\n").as_bytes());
                    }
                    None => {
                        // Synthetic child of a boundary-less multipart: write its (decoded) body back with this
                        // part's own transfer encoding, as it was read.
                        let mut data = Vec::new();
                        for c in children {
                            if let PartBody::Leaf(d) = &c.body {
                                data.extend_from_slice(d);
                            }
                        }
                        self.write_encoded(&data, out);
                    }
                }
            }
            PartBody::Message(m) => {
                // A message/rfc822 part is normally 7bit/8bit, but some senders base64- or QP-encode it; re-apply
                // the declared encoding so the header stays truthful.
                if self.declared_cte().is_empty() {
                    m.root.write(out);
                } else {
                    let mut inner = Vec::new();
                    m.root.write(&mut inner);
                    self.write_encoded(&inner, out);
                }
            }
        }
    }

    /// For a multipart that [`parse`] could not split (no usable boundary, or the boundary never occurs), the
    /// encoded body to write back unframed: the single synthetic child without headers, re-encoded with this
    /// part's transfer encoding, provided that text still contains no delimiter line (so it reads back the same).
    fn unsplit_body(&self) -> Option<Vec<u8>> {
        let PartBody::Multipart(children) = &self.body else { return None };
        let [only] = children.as_slice() else { return None };
        if !only.headers.is_empty() {
            return None;
        }
        let PartBody::Leaf(data) = &only.body else { return None };
        let cte = self.headers.get("Content-Transfer-Encoding").unwrap_or_default();
        let encoded = crate::transfer::encode(data, cte, "");
        match self.content_type.param("boundary") {
            Some(b) if split_multipart(&encoded, b).is_some() => None,
            _ => Some(encoded),
        }
    }

    /// The canonical Content-Transfer-Encoding this part declares (see [`crate::transfer::canonical_encoding`]);
    /// empty for identity encodings.
    fn declared_cte(&self) -> &'static str {
        crate::transfer::canonical_encoding(self.headers.get("Content-Transfer-Encoding").unwrap_or_default())
    }

    fn write_encoded(&self, data: &[u8], out: &mut Vec<u8>) {
        let cte = self.headers.get("Content-Transfer-Encoding").unwrap_or_default();
        let name = self.filename.as_deref().map(sanitize_filename).unwrap_or_default();
        out.extend_from_slice(&crate::transfer::encode(data, cte, &name));
    }
}

/// Collects mailboxes from every instance of an address header.
fn address_header(headers: &Headers, name: &str) -> Vec<Address> {
    headers.get_all(name).flat_map(crate::addr::parse_address_list).collect()
}

/// Body choice: the best `text/plain` (html = false) or `text/html` (html = true) part.
fn find_body(part: &Part, html: bool) -> Option<&Part> {
    match &part.body {
        PartBody::Leaf(_) => {
            let want = if html { "text/html" } else { "text/plain" };
            (part.content_type.mime_type == want && part.disposition != Disposition::Attachment)
                .then_some(part)
        }
        PartBody::Multipart(children) => match part.content_type.subtype() {
            // The last alternative is the richest the sender offers (RFC 2046 §5.1.4).
            "alternative" => children.iter().rev().find_map(|c| find_body(c, html)),
            "related" => {
                let start = part.content_type.param("start").map(strip_angle);
                let root = start
                    .and_then(|s| children.iter().find(|c| c.content_id.as_deref() == Some(s.as_str())))
                    .or_else(|| children.first())?;
                find_body(root, html)
            }
            _ => children.iter().find_map(|c| find_body(c, html)),
        },
        PartBody::Message(_) => None,
    }
}

/// Content-IDs referenced as `cid:` URLs in an HTML body (RFC 2392), percent-decoded and lower-cased. One pass
/// over the HTML, so a large body with many parts stays cheap.
fn cid_references(html: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while let Some(rel) =
        bytes.get(i..).and_then(|r| r.windows(4).position(|w| w.eq_ignore_ascii_case(b"cid:")))
    {
        let start = i + rel + 4;
        let len = bytes
            .get(start..)
            .unwrap_or_default()
            .iter()
            .position(|b| {
                b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b')' | b'(' | b'&')
            })
            .unwrap_or(bytes.len().saturating_sub(start));
        let raw = bytes.get(start..start + len).unwrap_or_default();
        if !raw.is_empty() && out.len() < crate::MAX_PARTS * 4 {
            let id = String::from_utf8_lossy(&crate::params::percent_decode(&String::from_utf8_lossy(raw)))
                .to_lowercase();
            out.insert(id);
        }
        i = start + len;
    }
    out
}

fn walk_parts<'a>(part: &'a Part, out: &mut Vec<&'a Part>) {
    out.push(part);
    match &part.body {
        PartBody::Leaf(_) => {}
        PartBody::Multipart(children) => {
            for c in children {
                walk_parts(c, out);
            }
        }
        PartBody::Message(m) => walk_parts(&m.root, out),
    }
}

impl Message {
    /// Decoded Subject, `""` when missing.
    pub fn subject(&self) -> String {
        self.headers.text("Subject").unwrap_or_default()
    }
    pub fn from(&self) -> Option<Mailbox> {
        address_header(&self.headers, "From").iter().flat_map(Address::mailboxes).next()
    }
    pub fn sender(&self) -> Option<Mailbox> {
        address_header(&self.headers, "Sender").iter().flat_map(Address::mailboxes).next()
    }
    pub fn to(&self) -> Vec<Address> {
        address_header(&self.headers, "To")
    }
    pub fn cc(&self) -> Vec<Address> {
        address_header(&self.headers, "Cc")
    }
    pub fn bcc(&self) -> Vec<Address> {
        address_header(&self.headers, "Bcc")
    }
    pub fn reply_to(&self) -> Vec<Address> {
        address_header(&self.headers, "Reply-To")
    }
    pub fn date(&self) -> Option<DateTime> {
        self.headers.get_all("Date").find_map(|v| DateTime::parse_rfc5322(v).ok())
    }
    /// Message-ID without angle brackets.
    pub fn message_id(&self) -> Option<String> {
        self.headers.get("Message-ID").and_then(|v| parse_msg_ids(v).into_iter().next())
    }
    /// In-Reply-To ids without angle brackets.
    pub fn in_reply_to(&self) -> Vec<String> {
        self.headers.get_all("In-Reply-To").flat_map(parse_msg_ids).collect()
    }
    /// References ids without angle brackets, oldest first.
    pub fn references(&self) -> Vec<String> {
        self.headers.get_all("References").flat_map(parse_msg_ids).collect()
    }
    /// "high" / "low" from Importance, X-Priority (1-2 high, 4-5 low) or Priority; `None` = normal.
    pub fn importance(&self) -> Option<&'static str> {
        if let Some(v) = self.headers.get("Importance") {
            match v.trim().to_ascii_lowercase().as_str() {
                "high" => return Some("high"),
                "low" => return Some("low"),
                "normal" => return None,
                _ => {}
            }
        }
        if let Some(v) = self.headers.get("X-Priority") {
            match v.trim().chars().next() {
                Some('1' | '2') => return Some("high"),
                Some('4' | '5') => return Some("low"),
                Some('3') => return None,
                _ => {}
            }
        }
        for name in ["Priority", "X-MSMail-Priority"] {
            if let Some(v) = self.headers.get(name) {
                match v.trim().to_ascii_lowercase().as_str() {
                    "urgent" | "high" => return Some("high"),
                    "non-urgent" | "low" => return Some("low"),
                    _ => {}
                }
            }
        }
        None
    }
    /// Best plain-text body (text/plain preferred in multipart/alternative), charset-decoded.
    pub fn text_body(&self) -> Option<String> {
        find_body(&self.root, false).and_then(Part::text)
    }
    /// Best HTML body, charset-decoded.
    pub fn html_body(&self) -> Option<String> {
        find_body(&self.root, true).and_then(Part::text)
    }
    /// Attachments and inline files (everything that is not the chosen text/html body).
    pub fn attachments(&self) -> Vec<Attachment> {
        let text = find_body(&self.root, false);
        let html_part = find_body(&self.root, true);
        let cids = html_part.and_then(Part::text).map(|h| cid_references(&h)).unwrap_or_default();
        let mut out = Vec::new();
        let mut n = 0usize;
        self.collect_attachments(&self.root, text, html_part, &cids, &mut n, &mut out);
        out
    }

    fn collect_attachments(
        &self,
        part: &Part,
        text: Option<&Part>,
        html_part: Option<&Part>,
        cids: &HashSet<String>,
        n: &mut usize,
        out: &mut Vec<Attachment>,
    ) {
        let chosen = |p: &Part| {
            text.is_some_and(|t| std::ptr::eq(t, p)) || html_part.is_some_and(|h| std::ptr::eq(h, p))
        };
        match &part.body {
            PartBody::Multipart(children) => {
                for c in children {
                    self.collect_attachments(c, text, html_part, cids, n, out);
                }
            }
            PartBody::Leaf(data) => {
                if chosen(part) {
                    return;
                }
                let mime = part.content_type.mime_type.as_str();
                let body_fragment = (mime == "text/plain" || mime == "text/html")
                    && part.filename.is_none()
                    && part.disposition != Disposition::Attachment;
                if body_fragment {
                    return;
                }
                *n += 1;
                out.push(Attachment {
                    filename: Self::attachment_name(part, *n, None),
                    mime_type: mime.to_owned(),
                    content_id: part.content_id.clone(),
                    inline: part.content_id.as_ref().is_some_and(|c| cids.contains(&c.to_lowercase())),
                    data: data.clone(),
                });
            }
            PartBody::Message(m) => {
                *n += 1;
                out.push(Attachment {
                    filename: Self::attachment_name(part, *n, Some(&m.subject())),
                    mime_type: part.content_type.mime_type.clone(),
                    content_id: part.content_id.clone(),
                    inline: false,
                    data: m.to_bytes(),
                });
            }
        }
    }

    fn attachment_name(part: &Part, n: usize, subject: Option<&str>) -> String {
        if let Some(f) = part.filename.as_deref().map(sanitize_filename).filter(|f| !f.is_empty()) {
            return f;
        }
        let ext = extension_for(&part.content_type.mime_type);
        if let Some(s) = subject {
            let s = sanitize_filename(&s.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"));
            let s: String = s.chars().take(120).collect();
            if !s.is_empty() {
                return format!("{s}.{ext}");
            }
            return format!("message.{ext}");
        }
        if part.content_type.main_type() == "image" {
            format!("image{n:03}.{ext}")
        } else {
            format!("attachment{n:03}.{ext}")
        }
    }

    /// Iterates all parts depth-first.
    pub fn parts(&self) -> Vec<&Part> {
        let mut out = Vec::new();
        walk_parts(&self.root, &mut out);
        out
    }
    /// Serialises the message (used for `.eml` attachments of nested messages and for export). Header values are
    /// written as stored (folded at 78); leaf bodies are re-encoded with their declared transfer encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.root.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn content_type_parse() {
        let ct = ContentType::parse("Text/HTML; Charset=\"UTF-8\"");
        assert_eq!(ct.mime_type, "text/html");
        assert_eq!(ct.charset(), "UTF-8");
        assert!(ct.is_text());
        assert_eq!(ct.main_type(), "text");
        assert_eq!(ct.subtype(), "html");
        let ct = ContentType::parse("garbage; charset=iso-8859-1");
        assert_eq!(ct.mime_type, "text/plain");
        assert_eq!(ct.charset(), "iso-8859-1");
        let ct = ContentType::parse("");
        assert_eq!(ct.mime_type, "text/plain");
        assert_eq!(ct.charset(), "us-ascii");
        let ct = ContentType::parse("multipart/mixed; boundary=\"--=_x\"");
        assert!(ct.is_multipart());
        assert_eq!(ct.param("BOUNDARY"), Some("--=_x"));
        assert!(ContentType::parse("message/rfc822").is_message());
        assert_eq!(ContentType::parse("a/b; x=\"y z\"").to_header_value(), "a/b; x=\"y z\"");
    }

    #[test]
    fn disposition_parse() {
        assert_eq!(Disposition::parse("inline").0, Disposition::Inline);
        assert_eq!(Disposition::parse("ATTACHMENT; filename=a").0, Disposition::Attachment);
        assert_eq!(Disposition::parse("form-data").0, Disposition::Attachment);
        assert_eq!(Disposition::parse("").0, Disposition::None);
    }

    #[test]
    fn splitter() {
        let body = b"preamble\r\n--b\r\none\r\n--b\r\ntwo\r\n--b--\r\nepilogue";
        let parts = split_multipart(body, "b").unwrap_or_default();
        assert_eq!(parts, vec![&b"one"[..], &b"two"[..]]);
        let body = b"--b\none\n--b\ntwo";
        assert_eq!(split_multipart(body, "b").unwrap_or_default(), vec![&b"one"[..], &b"two"[..]]);
        assert_eq!(split_multipart(b"--bc\nx", "b"), None);
        assert_eq!(split_multipart(b"--b  \nx\n--b--  \n", "b").unwrap_or_default(), vec![&b"x"[..]]);
        assert_eq!(split_multipart(b"no delimiters", "b"), None);
        assert_eq!(split_multipart(b"--b\n", ""), None);
        assert_eq!(split_multipart(b"--b\r\n\r\n--b--", "b").unwrap_or_default(), vec![&b""[..]]);
        assert_eq!(split_multipart(b"--b", "b").unwrap_or_default(), vec![&b""[..]]);
    }

    #[test]
    fn msg_ids() {
        assert_eq!(parse_msg_ids("<a@b>"), vec!["a@b"]);
        assert_eq!(parse_msg_ids(" <a@b> (comment) <c@d>\t<e@f>"), vec!["a@b", "c@d", "e@f"]);
        assert_eq!(parse_msg_ids("bare@id"), vec!["bare@id"]);
        assert_eq!(parse_msg_ids("<a @ b>"), vec!["a@b"]);
        assert!(parse_msg_ids("").is_empty());
        assert_eq!(parse_msg_ids("<unterminated"), vec!["unterminated"]);
    }

    #[test]
    fn filenames() {
        assert_eq!(sanitize_filename("C:\\Users\\x\\report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename(".."), "");
        assert_eq!(sanitize_filename("a\u{0}b.txt "), "ab.txt");
    }

    #[test]
    fn html_charset_sniff() {
        assert_eq!(sniff_html_charset(b"<meta charset=\"iso-8859-1\">").as_deref(), Some("iso-8859-1"));
        assert_eq!(
            sniff_html_charset(b"<meta http-equiv=Content-Type content='text/html; charset=windows-1252'>")
                .as_deref(),
            Some("windows-1252")
        );
        assert_eq!(sniff_html_charset(b"<p>none</p>"), None);
    }

    #[test]
    fn format_flowed() {
        assert_eq!(unflow("Hello \nworld\n", false), "Hello world\n");
        assert_eq!(unflow("Hello \nworld", true), "Helloworld");
        assert_eq!(unflow(" From me\n", false), "From me\n");
        assert_eq!(unflow("> quoted \n> more\nplain\n", false), "> quoted more\nplain\n");
        assert_eq!(unflow("para \n>quote\n", false), "para \n> quote\n");
        assert_eq!(unflow("text\n-- \nsig\n", false), "text\n-- \nsig\n");
        assert_eq!(unflow("", false), "");
        let m = parse(b"Content-Type: text/plain; format=flowed; charset=utf-8\n\nOne \ntwo\n").ok();
        assert_eq!(m.and_then(|m| m.text_body()).as_deref(), Some("One two\n"));
    }

    #[test]
    fn nested_message_decode_budget() {
        // Five levels of quoted-printable message/rfc822 (decoding copies each level).
        let mut src = String::from("Subject: bottom\n\nx");
        for _ in 0..5 {
            src =
                format!("Content-Type: message/rfc822\nContent-Transfer-Encoding: quoted-printable\n\n{src}");
        }
        let depth = |m: &Message| {
            let mut d = 0;
            let mut cur = &m.root;
            while let PartBody::Message(inner) = &cur.body {
                d += 1;
                cur = &inner.root;
            }
            d
        };
        let full = parse_with_budget(src.as_bytes(), usize::MAX);
        assert_eq!(depth(&full), 5);
        // A small budget stops decoding nested messages; the rest stays an (attachable) leaf.
        // Level 1 decodes 314 bytes, level 2 another 240: only the first fits in 400.
        let capped = parse_with_budget(src.as_bytes(), 400);
        assert_eq!(depth(&capped), 1);
        assert_eq!(capped.attachments().len(), 1);
        // Identity transfer encodings are parsed in place and cost nothing.
        let plain = src.replace("Content-Transfer-Encoding: quoted-printable\n", "");
        assert_eq!(depth(&parse_with_budget(plain.as_bytes(), 0)), 5);
    }

    #[test]
    fn empty_and_too_large() {
        assert!(parse(b"").is_err());
        assert!(parse(b" \r\n ").is_err());
        assert!(parse(b"x").is_ok());
    }

    proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..800)) {
            if let Ok(m) = parse(&data) {
                let _ = (m.subject(), m.text_body(), m.html_body(), m.attachments(), m.parts().len(), m.to_bytes());
                let _ = (m.from(), m.to(), m.date(), m.message_id(), m.references(), m.importance());
            }
        }

        #[test]
        fn unflow_never_panics(s in "[a-z >\\n-]{0,200}", delsp in any::<bool>()) {
            let _ = unflow(&s, delsp);
        }
    }
}
