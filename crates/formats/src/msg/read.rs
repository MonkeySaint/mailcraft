//! `.msg` reader.

use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};

use mailcraft_mime::Mailbox;
use mailcraft_model::{BodyFormat, DateTime, Importance, Sensitivity};

use super::props::*;
use super::{
    AttachmentContent, MAX_ATTACHMENTS, MAX_EMBED_DEPTH, MAX_MSG_BYTES, MAX_RECIPIENTS, MsgAttachment,
    MsgMessage,
};
use crate::{FormatError, Result, check_size};

type Cf<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;

/// Largest single stream read.
const MAX_STREAM_BYTES: u64 = MAX_MSG_BYTES as u64;
/// Code page of 8-bit strings when the item names none.
const DEFAULT_CODEPAGE: u32 = 1252;
/// Most values read from one multi-valued property.
const MAX_MV_VALUES: usize = 1024;

/// PS_PUBLIC_STRINGS {00020329-0000-0000-C000-000000000046} in its on-disk byte order.
const PS_PUBLIC_STRINGS: [u8; 16] =
    [0x29, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

fn err(msg: impl Into<String>) -> FormatError {
    FormatError::malformed("msg", msg)
}

/// Parses `.msg` bytes.
pub fn read(data: &[u8]) -> Result<MsgMessage> {
    check_size("msg", data.len(), MAX_MSG_BYTES)?;
    if !data.starts_with(&super::CFB_SIGNATURE) {
        return Err(err("not a compound file"));
    }
    // The container library is trusted to return errors on damage; the guard keeps a defect in it from
    // taking the client down.
    match catch_unwind(AssertUnwindSafe(|| read_inner(data))) {
        Ok(r) => r,
        Err(_) => Err(err("damaged compound file")),
    }
}

fn read_inner(data: &[u8]) -> Result<MsgMessage> {
    let cf = cfb::CompoundFile::open(Cursor::new(data)).map_err(|e| err(e.to_string()))?;
    if !cf.is_stream(format!("/{PROPERTIES_STREAM}")) {
        return Err(err("no property stream (not an Outlook item)"));
    }
    // Directory entries may share sector chains; cap the total bytes read at a few times the file size.
    let budget = u64::try_from(data.len()).unwrap_or(u64::MAX).saturating_mul(4).max(1 << 20);
    let mut src = Src { cf, budget };
    let keywords = keywords_id(&mut src);
    Ok(read_object(&mut src, "", HEADER_TOP, 0, keywords, DEFAULT_CODEPAGE))
}

/// The open container plus the remaining read budget.
struct Src<'a> {
    cf: Cf<'a>,
    budget: u64,
}

fn read_stream(src: &mut Src<'_>, path: &str) -> Option<Vec<u8>> {
    if src.budget == 0 {
        return None;
    }
    let s = src.cf.open_stream(path).ok()?;
    let mut out = Vec::new();
    s.take(MAX_STREAM_BYTES.min(src.budget)).read_to_end(&mut out).ok()?;
    src.budget = src.budget.saturating_sub(u64::try_from(out.len()).unwrap_or(u64::MAX));
    Some(out)
}

/// Children of a storage: (name, is_stream).
fn children(src: &Src<'_>, base: &str) -> Vec<(String, bool)> {
    let path = if base.is_empty() { "/".to_owned() } else { base.to_owned() };
    match src.cf.read_storage(&path) {
        Ok(entries) => entries.take(100_000).map(|e| (e.name().to_owned(), e.is_stream())).collect(),
        Err(_) => Vec::new(),
    }
}

/// Property id of the `Keywords` named property (categories), from the named-property mapping.
fn keywords_id(cf: &mut Src<'_>) -> Option<u16> {
    let base = format!("/{NAMEID_STORAGE}");
    let entries = read_stream(cf, &format!("{base}/{}", substg(0x0003, PT_BINARY)))?;
    let strings = read_stream(cf, &format!("{base}/{}", substg(0x0004, PT_BINARY))).unwrap_or_default();
    let guids = read_stream(cf, &format!("{base}/{}", substg(0x0002, PT_BINARY))).unwrap_or_default();
    for e in entries.as_chunks::<8>().0.iter().take(65_536) {
        let off = u32::from_le_bytes([*e.first()?, *e.get(1)?, *e.get(2)?, *e.get(3)?]);
        let idx_kind = u16::from_le_bytes([*e.get(4)?, *e.get(5)?]);
        let prop_index = u16::from_le_bytes([*e.get(6)?, *e.get(7)?]);
        if idx_kind & 1 == 0 {
            continue;
        }
        let guid_index = usize::from(idx_kind >> 1);
        let public = guid_index == 2
            || (guid_index >= 3
                && guids.get((guid_index - 3) * 16..(guid_index - 2) * 16)
                    == Some(PS_PUBLIC_STRINGS.as_slice()));
        if !public {
            continue;
        }
        // Offsets come from the file: checked arithmetic (usize is 32 bits on wasm32).
        let Some(start) = usize::try_from(off).ok().and_then(|o| o.checked_add(4)) else { continue };
        let Some(len) = strings
            .get(start - 4..start)
            .and_then(|b| Some(u32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?])))
        else {
            continue;
        };
        let len = usize::try_from(len).unwrap_or(0).min(1024);
        let name = start
            .checked_add(len)
            .and_then(|end| strings.get(start..end))
            .map(from_utf16le)
            .unwrap_or_default();
        if name == "Keywords" {
            return 0x8000u16.checked_add(prop_index);
        }
    }
    None
}

/// Properties of the object at `base`, and the code page of its 8-bit strings (its own PidTagMessageCodepage
/// or PidTagInternetCodepage, else `parent_cp`: recipients and attachments use their message's code page).
fn read_props(cf: &mut Src<'_>, base: &str, header: usize, parent_cp: u32) -> (Props, u32) {
    let mut props = Props::default();
    let stream = read_stream(cf, &format!("{base}/{PROPERTIES_STREAM}")).unwrap_or_default();
    parse_fixed(&stream, header, &mut props);
    let cp = props
        .int(PID_MESSAGE_CODEPAGE)
        .or_else(|| props.int(PID_INTERNET_CODEPAGE))
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(parent_cp);
    let enc = super::encoding_for_codepage(cp);
    let mut multi: Vec<((u16, u16), u32, String)> = Vec::new();
    for (name, is_stream) in children(cf, base) {
        if !is_stream {
            continue;
        }
        let Some((id, ty)) = parse_substg(&name) else { continue };
        let index = name.get(SUBSTG_PREFIX.len() + 8..).and_then(|s| s.strip_prefix('-'));
        let path = format!("{base}/{name}");
        if ty & MV_FLAG != 0 {
            if let Some(i) = index.and_then(|s| u32::from_str_radix(s, 16).ok())
                && multi.len() < MAX_MV_VALUES * 4
                && let Some(bytes) = read_stream(cf, &path)
            {
                let s = match ty & !MV_FLAG {
                    PT_UNICODE => from_utf16le(&bytes),
                    PT_STRING8 => decode_8bit(&bytes, enc),
                    _ => continue,
                };
                multi.push(((id, ty), i, s));
            }
            continue;
        }
        if index.is_some() || props.map.len() >= 8192 {
            continue;
        }
        let Some(bytes) = read_stream(cf, &path) else { continue };
        let value = match ty {
            PT_UNICODE => Value::Str(from_utf16le(&bytes)),
            PT_STRING8 => Value::Str(decode_8bit(&bytes, enc)),
            PT_BINARY => Value::Bin(bytes),
            _ => continue,
        };
        props.map.insert((id, ty), value);
    }
    multi.sort_by_key(|(k, i, _)| (*k, *i));
    for (key, _, s) in multi {
        if let Value::MultiStr(v) = props.map.entry(key).or_insert_with(|| Value::MultiStr(Vec::new()))
            && v.len() < MAX_MV_VALUES
        {
            v.push(s.trim_end_matches('\0').to_owned());
        }
    }
    (props, cp)
}

fn decode_8bit(bytes: &[u8], enc: &'static encoding_rs::Encoding) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let (t, _) = enc.decode_without_bom_handling(bytes.get(..end).unwrap_or(bytes));
    t.into_owned()
}

fn time(props: &Props, id: u16) -> Option<DateTime> {
    props.time(id).map(|ft| DateTime::from_unix(filetime_to_unix(ft), 0))
}

/// A mailbox from name / SMTP / email / address type properties.
fn mailbox(
    name: Option<String>,
    smtp: Option<String>,
    email: Option<String>,
    addrtype: Option<String>,
) -> Option<Mailbox> {
    let is_smtp = addrtype.as_deref().is_none_or(|t| t.eq_ignore_ascii_case("SMTP"));
    let addr = smtp
        .filter(|s| s.contains('@'))
        .or_else(|| email.clone().filter(|e| is_smtp && e.contains('@')))
        .or(email);
    match (name, addr) {
        (n, Some(a)) => {
            let n =
                n.filter(|n| !n.eq_ignore_ascii_case(&a) && !n.trim_matches('\'').eq_ignore_ascii_case(&a));
            Some(Mailbox::new(n.as_deref(), &a))
        }
        (Some(n), None) => Some(Mailbox { name: Some(n), addr: String::new() }),
        (None, None) => None,
    }
}

fn clean_id(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').trim().to_owned()
}

fn read_object(
    cf: &mut Src<'_>,
    base: &str,
    header: usize,
    depth: usize,
    keywords: Option<u16>,
    parent_cp: u32,
) -> MsgMessage {
    let (p, cp) = read_props(cf, base, header, parent_cp);
    let kids = children(cf, base);
    let mut recips: Vec<&String> =
        kids.iter().filter(|(n, s)| !s && n.starts_with(RECIP_PREFIX)).map(|(n, _)| n).collect();
    let mut attachs: Vec<&String> =
        kids.iter().filter(|(n, s)| !s && n.starts_with(ATTACH_PREFIX)).map(|(n, _)| n).collect();
    recips.sort();
    attachs.sort();

    let mut m = MsgMessage {
        message_class: p.text(PID_MESSAGE_CLASS).unwrap_or_else(|| "IPM.Note".into()),
        subject: p.string(PID_SUBJECT).unwrap_or_default(),
        ..Default::default()
    };
    m.sender = mailbox(
        p.text(PID_SENDER_NAME),
        p.text(PID_SENDER_SMTP),
        p.text(PID_SENDER_EMAIL),
        p.text(PID_SENDER_ADDRTYPE),
    );
    m.from = mailbox(
        p.text(PID_SENT_REPRESENTING_NAME),
        p.text(PID_SENT_REPRESENTING_SMTP),
        p.text(PID_SENT_REPRESENTING_EMAIL),
        p.text(PID_SENT_REPRESENTING_ADDRTYPE),
    )
    .or_else(|| m.sender.clone());
    if m.sender.as_ref().zip(m.from.as_ref()).is_some_and(|(s, f)| s.same_address(f)) {
        m.sender = None;
    }
    m.sent = time(&p, PID_CLIENT_SUBMIT_TIME);
    m.received = time(&p, PID_MESSAGE_DELIVERY_TIME);
    m.created = time(&p, PID_CREATION_TIME).or_else(|| time(&p, PID_LAST_MODIFICATION_TIME));
    m.message_id = p.text(PID_INTERNET_MESSAGE_ID).map(|s| clean_id(&s)).filter(|s| !s.is_empty());
    m.in_reply_to =
        p.text(PID_IN_REPLY_TO).map(|s| mailcraft_mime::message::parse_msg_ids(&s)).unwrap_or_default();
    m.references = p
        .text(PID_INTERNET_REFERENCES)
        .map(|s| mailcraft_mime::message::parse_msg_ids(&s))
        .unwrap_or_default();
    m.in_reply_to = m.in_reply_to.iter().map(|s| clean_id(s)).filter(|s| !s.is_empty()).collect();
    m.references = m.references.iter().map(|s| clean_id(s)).filter(|s| !s.is_empty()).collect();
    m.importance = match p.int(PID_IMPORTANCE) {
        Some(0) => Importance::Low,
        Some(2) => Importance::High,
        _ => Importance::Normal,
    };
    m.sensitivity = match p.int(PID_SENSITIVITY) {
        Some(1) => Sensitivity::Personal,
        Some(2) => Sensitivity::Private,
        Some(3) => Sensitivity::Confidential,
        _ => Sensitivity::Normal,
    };
    let flags = p.int(PID_MESSAGE_FLAGS).and_then(|f| u32::try_from(f & 0xFFFF_FFFF).ok()).unwrap_or(0);
    m.read = flags & MSGFLAG_READ != 0;
    m.unsent = flags & MSGFLAG_UNSENT != 0;
    if let Some(Value::MultiStr(cats)) =
        keywords.and_then(|k| p.get(k, PT_UNICODE | MV_FLAG).or_else(|| p.get(k, PT_STRING8 | MV_FLAG)))
    {
        m.categories = cats.iter().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect();
    }
    m.transport_headers = p.string(PID_TRANSPORT_HEADERS).filter(|h| !h.trim().is_empty());

    // Bodies.
    m.body_text = p.string(PID_BODY);
    let html_cp = p.int(PID_INTERNET_CODEPAGE).and_then(|c| u32::try_from(c).ok()).unwrap_or(65001);
    m.body_html = match p.binary(PID_HTML) {
        Some(b) => Some(match std::str::from_utf8(b) {
            Ok(s) => s.trim_end_matches('\0').to_owned(),
            Err(_) => decode_8bit(b, super::encoding_for_codepage(html_cp)),
        }),
        None => p.string(PID_HTML),
    };
    m.body_rtf = p.binary(PID_RTF_COMPRESSED).and_then(|b| super::rtf::decompress(b).ok());
    m.body_format = if m.body_html.is_some() { BodyFormat::Html } else { BodyFormat::PlainText };
    if let Some(rtf) = &m.body_rtf {
        if super::rtfhtml::is_html_encapsulated(rtf) {
            if m.body_html.is_none() {
                m.body_html = super::rtfhtml::extract_html(rtf);
                m.body_format = BodyFormat::Html;
            }
        } else if m.body_html.is_none() {
            m.body_format = BodyFormat::RichText;
            if m.body_text.as_deref().is_none_or(|t| t.trim().is_empty()) {
                m.body_text = Some(super::rtfhtml::extract_text(rtf));
            }
        }
    }

    // Recipients.
    for r in recips.into_iter().take(MAX_RECIPIENTS) {
        let (rp, _) = read_props(cf, &format!("{base}/{r}"), HEADER_SUB, cp);
        let Some(mb) = mailbox(
            rp.text(PID_DISPLAY_NAME),
            rp.text(PID_SMTP_ADDRESS),
            rp.text(PID_EMAIL_ADDRESS),
            rp.text(PID_ADDRTYPE),
        ) else {
            continue;
        };
        match rp.int(PID_RECIPIENT_TYPE).map(|t| t & 0xF) {
            Some(2) => m.cc.push(mb),
            Some(3) => m.bcc.push(mb),
            _ => m.to.push(mb),
        }
    }

    // Header fallback for what the properties lack.
    if let Some(h) = &m.transport_headers {
        let mut raw = h.clone().into_bytes();
        raw.extend_from_slice(b"\r\n\r\n");
        if let Ok(msg) = mailcraft_mime::parse(&raw) {
            if m.from.as_ref().is_none_or(|f| f.addr.is_empty()) {
                m.from = msg.from().or(m.from.take());
            }
            let flat = |v: Vec<mailcraft_mime::Address>| {
                v.iter().flat_map(mailcraft_mime::Address::mailboxes).collect::<Vec<_>>()
            };
            if m.to.is_empty() {
                m.to = flat(msg.to());
            }
            if m.cc.is_empty() {
                m.cc = flat(msg.cc());
            }
            if m.message_id.is_none() {
                m.message_id = msg.message_id().map(|s| clean_id(&s)).filter(|s| !s.is_empty());
            }
            if m.sent.is_none() {
                m.sent = msg.date();
            }
        }
    }

    // Attachments.
    for a in attachs.into_iter().take(MAX_ATTACHMENTS) {
        let path = format!("{base}/{a}");
        let (ap, _) = read_props(cf, &path, HEADER_SUB, cp);
        let method = ap.int(PID_ATTACH_METHOD).unwrap_or(1);
        let filename = ap
            .text(PID_ATTACH_LONG_FILENAME)
            .or_else(|| ap.text(PID_ATTACH_FILENAME))
            .or_else(|| ap.text(PID_DISPLAY_NAME))
            .unwrap_or_default();
        let content_id = ap.text(PID_ATTACH_CONTENT_ID).map(|c| clean_id(&c)).filter(|c| !c.is_empty());
        let embedded_path = format!("{path}/{}", substg(PID_ATTACH_DATA, PT_OBJECT));
        let content = if method == i64::from(ATTACH_EMBEDDED_MSG) && cf.cf.is_storage(&embedded_path) {
            if depth >= MAX_EMBED_DEPTH {
                continue;
            }
            AttachmentContent::Message(Box::new(read_object(
                cf,
                &embedded_path,
                HEADER_EMBEDDED,
                depth + 1,
                keywords,
                cp,
            )))
        } else if let Some(d) = ap.binary(PID_ATTACH_DATA) {
            AttachmentContent::Data(d.to_vec())
        } else {
            continue;
        };
        let mime_type = ap.text(PID_ATTACH_MIME_TAG).unwrap_or_else(|| match &content {
            AttachmentContent::Message(_) => "message/rfc822".into(),
            AttachmentContent::Data(_) => "application/octet-stream".into(),
        });
        let filename = match (&content, filename.is_empty()) {
            (AttachmentContent::Message(sub), true) => {
                format!("{}.msg", super::sanitize_filename(&sub.subject))
            }
            (_, true) => "attachment".to_owned(),
            _ => filename,
        };
        let hidden = ap.bool(PID_ATTACHMENT_HIDDEN).unwrap_or(false);
        let rendered = ap.int(PID_ATTACH_FLAGS).is_some_and(|f| f & i64::from(ATTACH_RENDERED_IN_BODY) != 0);
        let referenced = content_id
            .as_deref()
            .is_some_and(|cid| m.body_html.as_deref().is_some_and(|h| h.contains(&format!("cid:{cid}"))));
        let inline = content_id.is_some() && (hidden || rendered || referenced);
        m.attachments.push(MsgAttachment { filename, mime_type, content_id, inline, content });
    }
    m
}
