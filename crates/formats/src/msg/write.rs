//! `.msg` writer (Unicode, minimal valid structure per [MS-OXMSG] §2.1–2.4).

use std::io::{Cursor, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use mailcraft_mime::Mailbox;
use mailcraft_model::{Importance, Sensitivity};

use super::props::*;
use super::{AttachmentContent, MAX_EMBED_DEPTH, MsgMessage};
use crate::{FormatError, Result};

type Cf = cfb::CompoundFile<Cursor<Vec<u8>>>;

/// Property id assigned to the `Keywords` named property (first named property slot).
const KEYWORDS_ID: u16 = 0x8000;

fn io_err(e: std::io::Error) -> FormatError {
    FormatError::Io(e.to_string())
}

/// Serialises a message as `.msg` bytes.
pub fn write(m: &MsgMessage) -> Result<Vec<u8>> {
    match catch_unwind(AssertUnwindSafe(|| write_inner(m))) {
        Ok(r) => r,
        Err(_) => Err(FormatError::Io("compound file writer failed".into())),
    }
}

fn write_inner(m: &MsgMessage) -> Result<Vec<u8>> {
    // Version 3 (512-byte sectors): what Outlook items use and every compound file reader accepts.
    let mut cf: Cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new())).map_err(io_err)?;
    write_nameid(&mut cf)?;
    write_message(&mut cf, "", m, true, 0)?;
    cf.flush().map_err(io_err)?;
    Ok(cf.into_inner().into_inner())
}

fn put_stream(cf: &mut Cf, path: &str, bytes: &[u8]) -> Result<()> {
    let mut s = cf.create_stream(path).map_err(io_err)?;
    s.write_all(bytes).map_err(io_err)?;
    s.flush().map_err(io_err)?;
    Ok(())
}

/// Named-property mapping: one string-named property, `Keywords` in PS_PUBLIC_STRINGS (GUID index 2), mapped
/// to 0x8000. The GUID stream stays empty because PS_PUBLIC_STRINGS has a predefined index.
fn write_nameid(cf: &mut Cf) -> Result<()> {
    let base = format!("/{NAMEID_STORAGE}");
    cf.create_storage(&base).map_err(io_err)?;
    let name = utf16le("Keywords");
    let mut strings = Vec::new();
    strings.extend_from_slice(&u32::try_from(name.len()).unwrap_or(0).to_le_bytes());
    strings.extend_from_slice(&name);
    while strings.len() % 4 != 0 {
        strings.push(0);
    }
    let idx_kind: u16 = (2 << 1) | 1;
    let mut entry = Vec::new();
    entry.extend_from_slice(&0u32.to_le_bytes()); // string offset
    entry.extend_from_slice(&idx_kind.to_le_bytes());
    entry.extend_from_slice(&0u16.to_le_bytes()); // property index → 0x8000
    put_stream(cf, &format!("{base}/{}", substg(0x0002, PT_BINARY)), &[])?;
    put_stream(cf, &format!("{base}/{}", substg(0x0003, PT_BINARY)), &entry)?;
    put_stream(cf, &format!("{base}/{}", substg(0x0004, PT_BINARY)), &strings)?;
    // Hash bucket for the string name: CRC-32 of the name XOR the index/kind value, modulo 0x1F.
    let crc = super::rtf::crc32(&name);
    let bucket = 0x1000 + ((crc ^ u32::from(idx_kind)) % 0x1F);
    let mut hash_entry = Vec::new();
    hash_entry.extend_from_slice(&crc.to_le_bytes());
    hash_entry.extend_from_slice(&idx_kind.to_le_bytes());
    hash_entry.extend_from_slice(&0u16.to_le_bytes());
    let bucket_id = u16::try_from(bucket).unwrap_or(0x1000);
    put_stream(cf, &format!("{base}/{}", substg(bucket_id, PT_BINARY)), &hash_entry)?;
    Ok(())
}

/// Property set of one object under construction.
struct Obj {
    stream: StreamWriter,
    streams: Vec<(String, Vec<u8>)>,
}

impl Obj {
    fn new(header: Vec<u8>) -> Self {
        Obj { stream: StreamWriter::new(header), streams: Vec::new() }
    }

    /// A PtypString property. Empty strings are left out: a value stream must hold at least one character
    /// ([MS-OXMSG] 2.4.2.2; Outlook refuses files with zero-length string streams).
    fn string(&mut self, id: u16, s: &str) {
        if s.is_empty() {
            return;
        }
        let bytes = utf16le(s);
        let size = u32::try_from(bytes.len().saturating_add(2)).unwrap_or(u32::MAX);
        self.stream.variable(id, PT_UNICODE, size);
        self.streams.push((substg(id, PT_UNICODE), bytes));
    }

    fn opt_string(&mut self, id: u16, s: Option<&str>) {
        if let Some(s) = s {
            self.string(id, s);
        }
    }

    fn binary(&mut self, id: u16, b: Vec<u8>) {
        self.stream.variable(id, PT_BINARY, u32::try_from(b.len()).unwrap_or(u32::MAX));
        self.streams.push((substg(id, PT_BINARY), b));
    }

    fn multi_string(&mut self, id: u16, values: &[String]) {
        let ty = PT_UNICODE | MV_FLAG;
        let mut lengths = Vec::new();
        for (i, v) in values.iter().enumerate() {
            let mut bytes = utf16le(v);
            bytes.extend_from_slice(&[0, 0]);
            lengths.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes());
            self.streams.push((format!("{}-{i:08X}", substg(id, ty)), bytes));
        }
        self.stream.variable(id, ty, u32::try_from(lengths.len()).unwrap_or(0));
        self.streams.push((substg(id, ty), lengths));
    }

    fn flush(self, cf: &mut Cf, base: &str) -> Result<()> {
        put_stream(cf, &format!("{base}/{PROPERTIES_STREAM}"), &self.stream.bytes)?;
        for (name, bytes) in &self.streams {
            put_stream(cf, &format!("{base}/{name}"), bytes)?;
        }
        Ok(())
    }
}

fn display_list(v: &[Mailbox]) -> String {
    v.iter().map(|m| m.display().to_owned()).collect::<Vec<_>>().join("; ")
}

fn sender_props(o: &mut Obj, m: &Mailbox, name_id: u16, email_id: u16, type_id: u16, smtp_id: u16) {
    o.string(name_id, m.display());
    if !m.addr.is_empty() {
        o.string(email_id, &m.addr);
        o.string(type_id, "SMTP");
        o.string(smtp_id, &m.addr);
    }
}

fn write_message(cf: &mut Cf, base: &str, m: &MsgMessage, top: bool, depth: usize) -> Result<()> {
    let recipients: Vec<(u32, &Mailbox)> =
        m.to.iter()
            .map(|r| (1, r))
            .chain(m.cc.iter().map(|r| (2, r)))
            .chain(m.bcc.iter().map(|r| (3, r)))
            .collect();
    let attachments: Vec<_> = m
        .attachments
        .iter()
        .filter(|a| depth < MAX_EMBED_DEPTH || matches!(a.content, AttachmentContent::Data(_)))
        .collect();
    let n_recip = u32::try_from(recipients.len()).unwrap_or(u32::MAX);
    let n_attach = u32::try_from(attachments.len()).unwrap_or(u32::MAX);
    let mut o = Obj::new(message_header(top, n_recip, n_attach));

    let class = if m.message_class.trim().is_empty() { "IPM.Note" } else { m.message_class.trim() };
    o.string(PID_MESSAGE_CLASS, class);
    o.string(PID_SUBJECT, &m.subject);
    o.string(PID_NORMALIZED_SUBJECT, mailcraft_model::subject::strip_prefixes(&m.subject));
    let author = m.from.as_ref().or(m.sender.as_ref());
    if let Some(a) = author {
        sender_props(
            &mut o,
            a,
            PID_SENT_REPRESENTING_NAME,
            PID_SENT_REPRESENTING_EMAIL,
            PID_SENT_REPRESENTING_ADDRTYPE,
            PID_SENT_REPRESENTING_SMTP,
        );
    }
    if let Some(s) = m.sender.as_ref().or(author) {
        sender_props(&mut o, s, PID_SENDER_NAME, PID_SENDER_EMAIL, PID_SENDER_ADDRTYPE, PID_SENDER_SMTP);
    }
    o.string(PID_DISPLAY_TO, &display_list(&m.to));
    o.string(PID_DISPLAY_CC, &display_list(&m.cc));
    o.string(PID_DISPLAY_BCC, &display_list(&m.bcc));
    if let Some(t) = m.sent {
        o.stream.time(PID_CLIENT_SUBMIT_TIME, unix_to_filetime(t.unix));
    }
    if let Some(t) = m.received {
        o.stream.time(PID_MESSAGE_DELIVERY_TIME, unix_to_filetime(t.unix));
    }
    if let Some(t) = m.created.or(m.received).or(m.sent) {
        o.stream.time(PID_CREATION_TIME, unix_to_filetime(t.unix));
        o.stream.time(PID_LAST_MODIFICATION_TIME, unix_to_filetime(t.unix));
    }
    o.opt_string(PID_INTERNET_MESSAGE_ID, m.message_id.as_ref().map(|id| format!("<{id}>")).as_deref());
    if !m.in_reply_to.is_empty() {
        let v = m.in_reply_to.iter().map(|id| format!("<{id}>")).collect::<Vec<_>>().join(" ");
        o.string(PID_IN_REPLY_TO, &v);
    }
    if !m.references.is_empty() {
        let v = m.references.iter().map(|id| format!("<{id}>")).collect::<Vec<_>>().join(" ");
        o.string(PID_INTERNET_REFERENCES, &v);
    }
    o.stream.long(
        PID_IMPORTANCE,
        match m.importance {
            Importance::Low => 0,
            Importance::Normal => 1,
            Importance::High => 2,
        },
    );
    o.stream.long(
        PID_SENSITIVITY,
        match m.sensitivity {
            Sensitivity::Normal => 0,
            Sensitivity::Personal => 1,
            Sensitivity::Private => 2,
            Sensitivity::Confidential => 3,
        },
    );
    let mut flags = 0;
    if m.read {
        flags |= MSGFLAG_READ;
    }
    if m.unsent {
        flags |= MSGFLAG_UNSENT;
    }
    if attachments.iter().any(|a| !a.inline) {
        flags |= MSGFLAG_HASATTACH;
    }
    o.stream.long(PID_MESSAGE_FLAGS, flags);
    if top {
        o.stream.long(PID_STORE_SUPPORT_MASK, STORE_UNICODE_OK);
    }
    if let Some(t) = &m.body_text {
        o.string(PID_BODY, t);
    }
    if let Some(h) = &m.body_html {
        o.binary(PID_HTML, h.as_bytes().to_vec());
        o.stream.long(PID_INTERNET_CODEPAGE, 65001);
    }
    if let Some(rtf) = &m.body_rtf
        && m.body_html.is_none()
    {
        o.binary(PID_RTF_COMPRESSED, super::rtf::compress(rtf));
    }
    o.opt_string(PID_TRANSPORT_HEADERS, m.transport_headers.as_deref());
    if !m.categories.is_empty() {
        o.multi_string(KEYWORDS_ID, &m.categories);
    }
    o.flush(cf, base)?;

    for (i, (kind, r)) in recipients.iter().enumerate() {
        let path = format!("{base}/{RECIP_PREFIX}{i:08X}");
        cf.create_storage(&path).map_err(io_err)?;
        let mut ro = Obj::new(vec![0u8; HEADER_SUB]);
        ro.stream.long(PID_RECIPIENT_TYPE, *kind);
        ro.stream.long(PID_ROWID, u32::try_from(i).unwrap_or(0));
        ro.stream.long(PID_OBJECT_TYPE, MAPI_MAILUSER);
        ro.stream.long(PID_DISPLAY_TYPE, 0);
        ro.string(PID_DISPLAY_NAME, r.display());
        if !r.addr.is_empty() {
            ro.string(PID_EMAIL_ADDRESS, &r.addr);
            ro.string(PID_ADDRTYPE, "SMTP");
            ro.string(PID_SMTP_ADDRESS, &r.addr);
        }
        ro.flush(cf, &path)?;
    }

    for (i, a) in attachments.iter().enumerate() {
        let path = format!("{base}/{ATTACH_PREFIX}{i:08X}");
        cf.create_storage(&path).map_err(io_err)?;
        let mut ao = Obj::new(vec![0u8; HEADER_SUB]);
        let n = u32::try_from(i).unwrap_or(0);
        ao.stream.long(PID_ATTACH_NUMBER, n);
        ao.stream.long(PID_OBJECT_TYPE, MAPI_ATTACH);
        ao.stream.long(PID_RENDERING_POSITION, u32::MAX);
        ao.string(PID_DISPLAY_NAME, &a.filename);
        ao.string(PID_ATTACH_LONG_FILENAME, &a.filename);
        ao.string(PID_ATTACH_FILENAME, &a.filename);
        if let Some((_, ext)) = a.filename.rsplit_once('.').filter(|(_, e)| !e.is_empty() && e.len() <= 16) {
            ao.string(PID_ATTACH_EXTENSION, &format!(".{ext}"));
        }
        ao.opt_string(PID_ATTACH_MIME_TAG, Some(a.mime_type.as_str()));
        ao.opt_string(PID_ATTACH_CONTENT_ID, a.content_id.as_deref());
        if a.inline {
            ao.stream.boolean(PID_ATTACHMENT_HIDDEN, true);
            ao.stream.long(PID_ATTACH_FLAGS, ATTACH_RENDERED_IN_BODY);
        }
        match &a.content {
            AttachmentContent::Data(d) => {
                ao.stream.long(PID_ATTACH_METHOD, ATTACH_BY_VALUE);
                ao.stream.long(PID_ATTACH_SIZE, u32::try_from(d.len()).unwrap_or(u32::MAX));
                ao.binary(PID_ATTACH_DATA, d.clone());
                ao.flush(cf, &path)?;
            }
            AttachmentContent::Message(sub) => {
                ao.stream.long(PID_ATTACH_METHOD, ATTACH_EMBEDDED_MSG);
                // [MS-OXMSG] 2.4.2.2: Size 0xFFFFFFFF and Reserved 0x01 for an embedded message object.
                ao.stream.object(PID_ATTACH_DATA, EMBEDDED_MSG_RESERVED);
                ao.flush(cf, &path)?;
                let sub_path = format!("{path}/{}", substg(PID_ATTACH_DATA, PT_OBJECT));
                cf.create_storage(&sub_path).map_err(io_err)?;
                write_message(cf, &sub_path, sub, false, depth + 1)?;
            }
        }
    }
    Ok(())
}
