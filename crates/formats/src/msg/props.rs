//! Property tags, types and the property stream layout ([MS-OXMSG] §2.4, [MS-OXPROPS] identifiers).

use std::collections::HashMap;

// Property types ([MS-OXCDATA] §2.11.1).
pub const PT_I2: u16 = 0x0002;
pub const PT_LONG: u16 = 0x0003;
pub const PT_FLOAT: u16 = 0x0004;
pub const PT_DOUBLE: u16 = 0x0005;
pub const PT_CURRENCY: u16 = 0x0006;
pub const PT_APPTIME: u16 = 0x0007;
pub const PT_ERROR: u16 = 0x000A;
pub const PT_BOOLEAN: u16 = 0x000B;
pub const PT_OBJECT: u16 = 0x000D;
pub const PT_I8: u16 = 0x0014;
pub const PT_STRING8: u16 = 0x001E;
pub const PT_UNICODE: u16 = 0x001F;
pub const PT_SYSTIME: u16 = 0x0040;
pub const PT_BINARY: u16 = 0x0102;
pub const MV_FLAG: u16 = 0x1000;

// Property identifiers.
pub const PID_IMPORTANCE: u16 = 0x0017;
pub const PID_MESSAGE_CLASS: u16 = 0x001A;
pub const PID_SENSITIVITY: u16 = 0x0036;
pub const PID_SUBJECT: u16 = 0x0037;
pub const PID_CLIENT_SUBMIT_TIME: u16 = 0x0039;
pub const PID_SENT_REPRESENTING_NAME: u16 = 0x0042;
pub const PID_SENT_REPRESENTING_ADDRTYPE: u16 = 0x0064;
pub const PID_SENT_REPRESENTING_EMAIL: u16 = 0x0065;
pub const PID_TRANSPORT_HEADERS: u16 = 0x007D;
pub const PID_RECIPIENT_TYPE: u16 = 0x0C15;
pub const PID_SENDER_NAME: u16 = 0x0C1A;
pub const PID_SENDER_ADDRTYPE: u16 = 0x0C1E;
pub const PID_SENDER_EMAIL: u16 = 0x0C1F;
pub const PID_DISPLAY_BCC: u16 = 0x0E02;
pub const PID_DISPLAY_CC: u16 = 0x0E03;
pub const PID_DISPLAY_TO: u16 = 0x0E04;
pub const PID_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
pub const PID_MESSAGE_FLAGS: u16 = 0x0E07;
pub const PID_NORMALIZED_SUBJECT: u16 = 0x0E1D;
pub const PID_ATTACH_SIZE: u16 = 0x0E20;
pub const PID_ATTACH_NUMBER: u16 = 0x0E21;
pub const PID_OBJECT_TYPE: u16 = 0x0FFE;
pub const PID_BODY: u16 = 0x1000;
pub const PID_RTF_COMPRESSED: u16 = 0x1009;
pub const PID_HTML: u16 = 0x1013;
pub const PID_INTERNET_MESSAGE_ID: u16 = 0x1035;
pub const PID_INTERNET_REFERENCES: u16 = 0x1039;
pub const PID_IN_REPLY_TO: u16 = 0x1042;
pub const PID_ROWID: u16 = 0x3000;
pub const PID_DISPLAY_NAME: u16 = 0x3001;
pub const PID_ADDRTYPE: u16 = 0x3002;
pub const PID_EMAIL_ADDRESS: u16 = 0x3003;
pub const PID_CREATION_TIME: u16 = 0x3007;
pub const PID_LAST_MODIFICATION_TIME: u16 = 0x3008;
pub const PID_STORE_SUPPORT_MASK: u16 = 0x340D;
pub const PID_ATTACH_DATA: u16 = 0x3701;
pub const PID_ATTACH_EXTENSION: u16 = 0x3703;
pub const PID_ATTACH_FILENAME: u16 = 0x3704;
pub const PID_ATTACH_METHOD: u16 = 0x3705;
pub const PID_ATTACH_LONG_FILENAME: u16 = 0x3707;
pub const PID_RENDERING_POSITION: u16 = 0x370B;
pub const PID_ATTACH_MIME_TAG: u16 = 0x370E;
pub const PID_ATTACH_CONTENT_ID: u16 = 0x3712;
pub const PID_ATTACH_FLAGS: u16 = 0x3714;
pub const PID_DISPLAY_TYPE: u16 = 0x3900;
pub const PID_SMTP_ADDRESS: u16 = 0x39FE;
pub const PID_INTERNET_CODEPAGE: u16 = 0x3FDE;
pub const PID_MESSAGE_CODEPAGE: u16 = 0x3FFD;
pub const PID_SENDER_SMTP: u16 = 0x5D01;
pub const PID_SENT_REPRESENTING_SMTP: u16 = 0x5D02;
pub const PID_ATTACHMENT_HIDDEN: u16 = 0x7FFE;

// Values.
pub const MSGFLAG_READ: u32 = 0x0001;
pub const MSGFLAG_UNSENT: u32 = 0x0008;
pub const MSGFLAG_HASATTACH: u32 = 0x0010;
pub const ATTACH_BY_VALUE: u32 = 1;
pub const ATTACH_EMBEDDED_MSG: u32 = 5;
pub const ATTACH_RENDERED_IN_BODY: u32 = 0x4;
pub const STORE_UNICODE_OK: u32 = 0x0004_0000;
pub const MAPI_MAILUSER: u32 = 6;
pub const MAPI_ATTACH: u32 = 7;
pub const PROPATTR_READABLE_WRITABLE: u32 = 0x0000_0006;
/// Reserved field of the PtypObject entry of an embedded message attachment ([MS-OXMSG] 2.4.2.2).
pub const EMBEDDED_MSG_RESERVED: u32 = 0x0000_0001;

// Storage and stream names.
pub const PROPERTIES_STREAM: &str = "__properties_version1.0";
pub const RECIP_PREFIX: &str = "__recip_version1.0_#";
pub const ATTACH_PREFIX: &str = "__attach_version1.0_#";
pub const NAMEID_STORAGE: &str = "__nameid_version1.0";
pub const SUBSTG_PREFIX: &str = "__substg1.0_";

/// Property stream header sizes.
pub const HEADER_TOP: usize = 32;
pub const HEADER_EMBEDDED: usize = 24;
pub const HEADER_SUB: usize = 8;

/// A property value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    /// FILETIME (100 ns ticks since 1601-01-01).
    Time(u64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
    MultiStr(Vec<String>),
}

/// The properties of one object, keyed by (id, type).
#[derive(Clone, Debug, Default)]
pub struct Props {
    pub map: HashMap<(u16, u16), Value>,
}

impl Props {
    pub fn get(&self, id: u16, ty: u16) -> Option<&Value> {
        self.map.get(&(id, ty))
    }

    /// String value (Unicode preferred over 8-bit).
    pub fn string(&self, id: u16) -> Option<String> {
        for ty in [PT_UNICODE, PT_STRING8] {
            if let Some(Value::Str(s)) = self.get(id, ty) {
                return Some(s.trim_end_matches('\0').to_owned());
            }
        }
        None
    }

    /// Non-empty trimmed string.
    pub fn text(&self, id: u16) -> Option<String> {
        self.string(id).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    }

    pub fn int(&self, id: u16) -> Option<i64> {
        for ty in [PT_LONG, PT_I2, PT_I8] {
            if let Some(Value::Int(v)) = self.get(id, ty) {
                return Some(*v);
            }
        }
        None
    }

    pub fn bool(&self, id: u16) -> Option<bool> {
        match self.get(id, PT_BOOLEAN) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn time(&self, id: u16) -> Option<u64> {
        match self.get(id, PT_SYSTIME) {
            Some(Value::Time(t)) if *t != 0 => Some(*t),
            _ => None,
        }
    }

    pub fn binary(&self, id: u16) -> Option<&[u8]> {
        match self.get(id, PT_BINARY) {
            Some(Value::Bin(b)) => Some(b),
            _ => None,
        }
    }
}

/// Seconds between 1601-01-01 and 1970-01-01.
const FILETIME_EPOCH_SECS: i64 = 11_644_473_600;

/// FILETIME → unix seconds.
pub fn filetime_to_unix(ft: u64) -> i64 {
    i64::try_from(ft / 10_000_000).unwrap_or(i64::MAX).saturating_sub(FILETIME_EPOCH_SECS)
}

/// Unix seconds → FILETIME (clamped to the representable range).
pub fn unix_to_filetime(unix: i64) -> u64 {
    let secs = unix.saturating_add(FILETIME_EPOCH_SECS).max(0);
    u64::try_from(secs).unwrap_or(0).saturating_mul(10_000_000)
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*d.get(at)?, *d.get(at + 1)?, *d.get(at + 2)?, *d.get(at + 3)?]))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    let lo = u32_at(d, at)?;
    let hi = u32_at(d, at + 4)?;
    Some(u64::from(hi) << 32 | u64::from(lo))
}

/// Fixed-length values from a property stream (entries after `header` bytes). Variable-length entries are
/// listed in the returned tag list; their data lives in separate streams.
pub fn parse_fixed(stream: &[u8], header: usize, props: &mut Props) -> Vec<(u16, u16)> {
    let mut variable = Vec::new();
    let mut at = header;
    while let (Some(ty), Some(id)) = (u16_at(stream, at), u16_at(stream, at + 2)) {
        let v = at + 8;
        let value = match ty {
            PT_I2 => u16_at(stream, v).map(|x| Value::Int(i64::from(x as i16))),
            PT_LONG | PT_ERROR => u32_at(stream, v).map(|x| Value::Int(i64::from(x as i32))),
            // One byte ([MS-OXCDATA] 2.11.1); the rest of the 8-byte value is reserved and ignored.
            PT_BOOLEAN => stream.get(v).map(|&x| Value::Bool(x != 0)),
            PT_I8 | PT_CURRENCY => u64_at(stream, v).map(|x| Value::Int(x as i64)),
            PT_SYSTIME => u64_at(stream, v).map(Value::Time),
            PT_DOUBLE | PT_APPTIME => u64_at(stream, v).map(|x| Value::Float(f64::from_bits(x))),
            PT_FLOAT => u32_at(stream, v).map(|x| Value::Float(f64::from(f32::from_bits(x)))),
            _ => {
                if variable.len() < 4096 {
                    variable.push((id, ty));
                }
                None
            }
        };
        if let Some(value) = value
            && props.map.len() < 8192
        {
            props.map.insert((id, ty), value);
        }
        at += 16;
    }
    variable
}

/// Encoded property stream: header bytes + 16-byte entries.
pub struct StreamWriter {
    pub bytes: Vec<u8>,
}

impl StreamWriter {
    pub fn new(header: Vec<u8>) -> Self {
        StreamWriter { bytes: header }
    }

    fn entry(&mut self, id: u16, ty: u16, value: [u8; 8]) {
        let tag = u32::from(id) << 16 | u32::from(ty);
        self.bytes.extend_from_slice(&tag.to_le_bytes());
        self.bytes.extend_from_slice(&PROPATTR_READABLE_WRITABLE.to_le_bytes());
        self.bytes.extend_from_slice(&value);
    }

    pub fn long(&mut self, id: u16, v: u32) {
        let mut b = [0u8; 8];
        for (slot, x) in b.iter_mut().zip(v.to_le_bytes()) {
            *slot = x;
        }
        self.entry(id, PT_LONG, b);
    }

    pub fn boolean(&mut self, id: u16, v: bool) {
        let mut b = [0u8; 8];
        if v && let Some(first) = b.first_mut() {
            *first = 1;
        }
        self.entry(id, PT_BOOLEAN, b);
    }

    pub fn time(&mut self, id: u16, ft: u64) {
        self.entry(id, PT_SYSTIME, ft.to_le_bytes());
    }

    /// Entry for an embedded object (PtypObject): Size 0xFFFFFFFF and the given Reserved value.
    pub fn object(&mut self, id: u16, reserved: u32) {
        let mut b = [0u8; 8];
        for (slot, x) in b.iter_mut().zip(u32::MAX.to_le_bytes().into_iter().chain(reserved.to_le_bytes())) {
            *slot = x;
        }
        self.entry(id, PT_OBJECT, b);
    }

    /// Entry for a variable-length value of `size` bytes (as defined for its type).
    pub fn variable(&mut self, id: u16, ty: u16, size: u32) {
        let mut b = [0u8; 8];
        for (slot, x) in b.iter_mut().zip(size.to_le_bytes()) {
            *slot = x;
        }
        self.entry(id, ty, b);
    }
}

/// Header of a top-level (32 bytes) or embedded (24 bytes) message property stream.
pub fn message_header(top: bool, recipients: u32, attachments: u32) -> Vec<u8> {
    let mut h = vec![0u8; 8];
    h.extend_from_slice(&recipients.to_le_bytes());
    h.extend_from_slice(&attachments.to_le_bytes());
    h.extend_from_slice(&recipients.to_le_bytes());
    h.extend_from_slice(&attachments.to_le_bytes());
    if top {
        h.extend_from_slice(&[0u8; 8]);
    }
    h
}

/// `__substg1.0_XXXXYYYY` stream name.
pub fn substg(id: u16, ty: u16) -> String {
    format!("{SUBSTG_PREFIX}{id:04X}{ty:04X}")
}

/// Parses a `__substg1.0_XXXXYYYY` name into (id, type).
pub fn parse_substg(name: &str) -> Option<(u16, u16)> {
    let hex = name.strip_prefix(SUBSTG_PREFIX)?;
    let hex = hex.get(..8)?;
    let id = u16::from_str_radix(hex.get(..4)?, 16).ok()?;
    let ty = u16::from_str_radix(hex.get(4..8)?, 16).ok()?;
    Some((id, ty))
}

/// UTF-16LE bytes of `s` (no terminator).
pub fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// UTF-16LE bytes → string (stops at a NUL terminator; unpaired surrogates replaced).
pub fn from_utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(units.get(..end).unwrap_or(&units))
}
