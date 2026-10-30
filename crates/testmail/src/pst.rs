//! Synthetic PST files for importer tests, written from the public [MS-PST] specification.
//!
//! MailCraft only reads PST files; this builder exists so tests have fixtures without real mail. It writes a
//! Unicode PST (format version 23) with every structure a reader walks:
//! - the header (§2.2.2.6) with both CRCs (§5.3) and the root (§2.2.2.5) pointing at the two B-trees;
//! - the node and block B-trees (§2.2.2.7.7) as leaf pages, with one intermediate level when the entries do
//!   not fit one page, each page with its trailer, signature (§5.5) and CRC;
//! - data blocks with trailers, optionally with permutative encoding (§5.1), and data trees (XBLOCK,
//!   §2.2.2.8.3.2.1) for data larger than one block; sub-node trees (SLBLOCK, §2.2.2.8.3.3.1);
//! - heaps on nodes (§2.3.1), B-trees on heaps (§2.3.2), property contexts (§2.3.3) and table contexts
//!   (§2.3.4), property values over 3580 bytes in sub-nodes;
//! - the message store, the name-to-id map (§2.4.7), the root folder, the IPM subtree and any folders below
//!   it with their hierarchy, contents and associated contents tables, and messages with recipient and
//!   attachment tables, attachments and embedded messages (§2.4.4–2.4.6).
//!
//! Allocation maps are left out and marked invalid in the root (`fAMapValid` = 0), which a reader that only
//! reads never needs. Every address in the tests that use it is at example.com, example.org or example.net.

/// Property types ([MS-OXCDATA] §2.11.1).
pub mod types {
    pub const I16: u16 = 0x0002;
    pub const I32: u16 = 0x0003;
    pub const DOUBLE: u16 = 0x0005;
    pub const BOOL: u16 = 0x000B;
    pub const OBJECT: u16 = 0x000D;
    pub const I64: u16 = 0x0014;
    pub const STRING8: u16 = 0x001E;
    pub const UNICODE: u16 = 0x001F;
    pub const TIME: u16 = 0x0040;
    pub const BINARY: u16 = 0x0102;
    pub const MV_UNICODE: u16 = 0x101F;
}

/// PS_PUBLIC_STRINGS {00020329-0000-0000-C000-000000000046} in its on-disk byte order.
pub const PS_PUBLIC_STRINGS: [u8; 16] =
    [0x29, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Appointment {00062002-0000-0000-C000-000000000046}.
pub const PSETID_APPOINTMENT: [u8; 16] =
    [0x02, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Task {00062003-0000-0000-C000-000000000046}.
pub const PSETID_TASK: [u8; 16] =
    [0x03, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Address {00062004-0000-0000-C000-000000000046}.
pub const PSETID_ADDRESS: [u8; 16] =
    [0x04, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Common {00062008-0000-0000-C000-000000000046}.
pub const PSETID_COMMON: [u8; 16] =
    [0x08, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Note {0006200E-0000-0000-C000-000000000046}.
pub const PSETID_NOTE: [u8; 16] =
    [0x0E, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// A property value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    I16(i16),
    I32(i32),
    Bool(bool),
    I64(i64),
    Double(f64),
    /// Unix seconds (written as a FILETIME).
    Time(i64),
    Unicode(String),
    /// 8-bit string bytes (no terminator).
    String8(Vec<u8>),
    Binary(Vec<u8>),
    MvUnicode(Vec<String>),
    /// PtypObject: the sub-node holding the object, and its size (an embedded message attachment).
    Object(u32, u32),
}

impl Value {
    fn prop_type(&self) -> u16 {
        match self {
            Value::I16(_) => types::I16,
            Value::I32(_) => types::I32,
            Value::Bool(_) => types::BOOL,
            Value::I64(_) => types::I64,
            Value::Double(_) => types::DOUBLE,
            Value::Time(_) => types::TIME,
            Value::Unicode(_) => types::UNICODE,
            Value::String8(_) => types::STRING8,
            Value::Binary(_) => types::BINARY,
            Value::MvUnicode(_) => types::MV_UNICODE,
            Value::Object(..) => types::OBJECT,
        }
    }
}

/// Which property a value is for: a tag below 0x8000, or a named property ([MS-OXPROPS] §1.3.2) the builder
/// maps to an id of 0x8000 or more in the name-to-id map.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Tag(u16),
    Lid([u8; 16], u32),
    Name([u8; 16], String),
}

/// Properties of one object.
pub type Props = Vec<(Key, Value)>;

/// One attachment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachmentDef {
    pub props: Props,
    /// An embedded message (attach method 5); its PidTagAttachDataObject is written by the builder.
    pub embedded: Option<Box<ItemDef>>,
}

/// A message (mail, contact, appointment, task, note, ...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemDef {
    pub props: Props,
    /// Rows of the recipient table.
    pub recipients: Vec<Props>,
    pub attachments: Vec<AttachmentDef>,
}

impl ItemDef {
    /// A message of class `class` (PidTagMessageClass).
    pub fn new(class: &str) -> ItemDef {
        ItemDef { props: vec![(Key::Tag(0x001A), Value::Unicode(class.to_owned()))], ..Default::default() }
    }

    /// Adds a property.
    pub fn with(mut self, key: Key, value: Value) -> ItemDef {
        self.props.push((key, value));
        self
    }

    /// Adds a tagged property.
    pub fn tag(self, id: u16, value: Value) -> ItemDef {
        self.with(Key::Tag(id), value)
    }

    /// Adds a recipient: `kind` 1 = To, 2 = Cc, 3 = Bcc.
    pub fn recipient(mut self, kind: i32, name: &str, addr: &str) -> ItemDef {
        self.recipients.push(vec![
            (Key::Tag(0x0C15), Value::I32(kind)),
            (Key::Tag(0x3001), Value::Unicode(name.to_owned())),
            (Key::Tag(0x3002), Value::Unicode("SMTP".to_owned())),
            (Key::Tag(0x3003), Value::Unicode(addr.to_owned())),
            (Key::Tag(0x39FE), Value::Unicode(addr.to_owned())),
        ]);
        self
    }

    /// Adds a file attachment by value.
    pub fn attach(mut self, filename: &str, mime: &str, data: &[u8]) -> ItemDef {
        let size = i32::try_from(data.len()).unwrap_or(i32::MAX);
        self.attachments.push(AttachmentDef {
            props: vec![
                (Key::Tag(0x3705), Value::I32(1)),
                (Key::Tag(0x3707), Value::Unicode(filename.to_owned())),
                (Key::Tag(0x3704), Value::Unicode(filename.to_owned())),
                (Key::Tag(0x370E), Value::Unicode(mime.to_owned())),
                (Key::Tag(0x0E20), Value::I32(size)),
                (Key::Tag(0x3701), Value::Binary(data.to_vec())),
            ],
            embedded: None,
        });
        self
    }

    /// Adds an embedded message attachment.
    pub fn attach_message(mut self, display: &str, inner: ItemDef) -> ItemDef {
        self.attachments.push(AttachmentDef {
            props: vec![
                (Key::Tag(0x3705), Value::I32(5)),
                (Key::Tag(0x3001), Value::Unicode(display.to_owned())),
            ],
            embedded: Some(Box::new(inner)),
        });
        self
    }
}

/// A folder below the IPM subtree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderDef {
    pub name: String,
    /// PidTagContainerClass ("IPF.Note", "IPF.Appointment", "IPF.Contact", "IPF.Task", "IPF.StickyNote").
    pub class: String,
    /// Index of the parent in [`PstBuilder::folders`]; `None` = directly below the IPM subtree.
    pub parent: Option<usize>,
    pub items: Vec<ItemDef>,
}

/// Block encoding (header `bCryptMethod`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Crypt {
    #[default]
    None,
    /// NDB_CRYPT_PERMUTE ([MS-PST] §5.1), what most PST files use.
    Permute,
}

/// Builds a PST file.
#[derive(Clone, Debug, Default)]
pub struct PstBuilder {
    /// The store's display name.
    pub name: String,
    pub crypt: Crypt,
    pub folders: Vec<FolderDef>,
    /// Index of the folder that is the store's Deleted Items (PidTagIpmWastebasketEntryId).
    pub wastebasket: Option<usize>,
    /// Extra store properties (PidTagPstPassword 0x67FF marks a password-protected file).
    pub store_props: Props,
}

/// Largest data block payload (Unicode).
const BLOCK_MAX: usize = 8176;
/// Largest heap allocation; bigger property values go to sub-nodes.
const HEAP_ITEM_MAX: usize = 3580;
/// Page size.
const PAGE: usize = 512;
/// Where blocks and pages start (after the header, density list and the first allocation map slot).
const DATA_START: usize = 0x4600;

const NID_MESSAGE_STORE: u32 = 0x21;
const NID_NAME_TO_ID_MAP: u32 = 0x61;
const NID_ROOT_FOLDER: u32 = 0x122;
const NID_RECIPIENT_TABLE: u32 = 0x692;
const NID_ATTACHMENT_TABLE: u32 = 0x671;

const NT_NORMAL_FOLDER: u32 = 0x02;
const NT_NORMAL_MESSAGE: u32 = 0x04;
const NT_ATTACHMENT: u32 = 0x05;
const NT_HIERARCHY_TABLE: u32 = 0x0D;
const NT_CONTENTS_TABLE: u32 = 0x0E;
const NT_ASSOC_CONTENTS_TABLE: u32 = 0x0F;
const NT_LTP: u32 = 0x1F;

/// The store record key every entry id carries (synthetic).
const RECORD_KEY: [u8; 16] = *b"MailCraft-test-k";

fn nid(kind: u32, index: u32) -> u32 {
    (index << 5) | kind
}

/// CRC of [MS-PST] §5.3: the reflected CRC-32 polynomial, started from 0, without a final complement.
pub fn crc(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

/// ComputeSig of [MS-PST] §5.5.
fn signature(ib: u64, bid: u64) -> u16 {
    let v = ib ^ bid;
    ((v >> 16) as u16) ^ (v as u16)
}

/// mpbbR, the encoding table of NDB_CRYPT_PERMUTE ([MS-PST] §5.1).
const PERMUTE_R: [u8; 256] = [
    65, 54, 19, 98, 168, 33, 110, 187, 244, 22, 204, 4, 127, 100, 232, 93, 30, 242, 203, 42, 116, 197, 94,
    53, 210, 149, 71, 158, 150, 45, 154, 136, 76, 125, 132, 63, 219, 172, 49, 182, 72, 95, 246, 196, 216, 57,
    139, 231, 35, 59, 56, 142, 200, 193, 223, 37, 177, 32, 165, 70, 96, 78, 156, 251, 170, 211, 86, 81, 69,
    124, 85, 0, 7, 201, 43, 157, 133, 155, 9, 160, 143, 173, 179, 15, 99, 171, 137, 75, 215, 167, 21, 90,
    113, 102, 66, 191, 38, 74, 107, 152, 250, 234, 119, 83, 178, 112, 5, 44, 253, 89, 58, 134, 126, 206, 6,
    235, 130, 120, 87, 199, 141, 67, 175, 180, 28, 212, 91, 205, 226, 233, 39, 79, 195, 8, 114, 128, 207,
    176, 239, 245, 40, 109, 190, 48, 77, 52, 146, 213, 14, 60, 34, 50, 229, 228, 249, 159, 194, 209, 10, 129,
    18, 225, 238, 145, 131, 118, 227, 151, 230, 97, 138, 23, 121, 164, 183, 220, 144, 122, 92, 140, 2, 166,
    202, 105, 222, 80, 26, 17, 147, 185, 82, 135, 88, 252, 237, 29, 55, 73, 27, 106, 224, 41, 51, 153, 189,
    108, 217, 148, 243, 64, 84, 111, 240, 198, 115, 184, 214, 62, 101, 24, 68, 31, 221, 103, 16, 241, 12, 25,
    236, 174, 3, 161, 20, 123, 169, 11, 255, 248, 163, 192, 162, 1, 247, 46, 188, 36, 104, 117, 13, 254, 186,
    47, 181, 208, 218, 61,
];

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn filetime(unix: i64) -> i64 {
    unix.saturating_add(11_644_473_600).saturating_mul(10_000_000)
}
fn len16(n: usize) -> Result<u16, String> {
    u16::try_from(n).map_err(|_| format!("{n} does not fit 16 bits"))
}
fn len32(n: usize) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("{n} does not fit 32 bits"))
}

/// A heap on a node, in one block.
struct Heap {
    allocs: Vec<Vec<u8>>,
}

impl Heap {
    fn new() -> Heap {
        Heap { allocs: Vec::new() }
    }

    /// Adds an allocation; its HID.
    fn alloc(&mut self, bytes: Vec<u8>) -> Result<u32, String> {
        if bytes.len() > HEAP_ITEM_MAX {
            return Err(format!("heap allocation of {} bytes", bytes.len()));
        }
        if bytes.is_empty() {
            // An empty value is HID 0; a zero-size allocation would be a free slot.
            return Ok(0);
        }
        self.allocs.push(bytes);
        Ok(len32(self.allocs.len())? << 5)
    }

    fn build(&self, client_sig: u8, user_root: u32) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        put_u16(&mut out, 0); // ibHnpm, patched below
        out.push(0xEC);
        out.push(client_sig);
        put_u32(&mut out, user_root);
        put_u32(&mut out, 0);
        let mut offsets = Vec::with_capacity(self.allocs.len() + 1);
        for a in &self.allocs {
            offsets.push(len16(out.len())?);
            out.extend_from_slice(a);
        }
        offsets.push(len16(out.len())?);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let ib = len16(out.len())?;
        if let Some(slot) = out.get_mut(0..2) {
            slot.copy_from_slice(&ib.to_le_bytes());
        }
        put_u16(&mut out, len16(self.allocs.len())?);
        put_u16(&mut out, 0);
        for o in offsets {
            put_u16(&mut out, o);
        }
        if out.len() > BLOCK_MAX {
            return Err(format!("heap of {} bytes does not fit one block", out.len()));
        }
        Ok(out)
    }

    /// A B-tree on this heap with sorted `(key, value)` records; the HID of its header.
    fn bth(&mut self, key_size: u8, value_size: u8, records: &[Vec<u8>]) -> Result<u32, String> {
        let root = if records.is_empty() { 0 } else { self.alloc(records.concat())? };
        let mut header = vec![0xB5, key_size, value_size, 0];
        put_u32(&mut header, root);
        self.alloc(header)
    }
}

/// A node's data and the sub-nodes it owns.
#[derive(Default)]
struct NodeData {
    data: Vec<u8>,
    subs: Vec<(u32, NodeData)>,
}

/// Allocates sub-node ids for the values of one node.
struct SubIds {
    next: u32,
}

impl SubIds {
    fn take(&mut self, kind: u32) -> u32 {
        self.next = self.next.saturating_add(1);
        nid(kind, self.next)
    }
}

/// Resolves named properties to ids from 0x8000 on.
#[derive(Default)]
struct Names {
    list: Vec<([u8; 16], Option<u32>, Option<String>)>,
}

impl Names {
    fn id(&mut self, key: &Key) -> Result<u16, String> {
        let (guid, lid, name) = match key {
            Key::Tag(id) => return Ok(*id),
            Key::Lid(g, l) => (*g, Some(*l), None),
            Key::Name(g, n) => (*g, None, Some(n.clone())),
        };
        let pos = match self.list.iter().position(|(g, l, n)| *g == guid && *l == lid && *n == name) {
            Some(p) => p,
            None => {
                self.list.push((guid, lid, name));
                self.list.len().saturating_sub(1)
            }
        };
        0x8000u16.checked_add(len16(pos)?).ok_or_else(|| "too many named properties".to_owned())
    }
}

/// Property context bytes for `props`, moving values over [`HEAP_ITEM_MAX`] to sub-nodes.
fn property_context(
    props: &[(u16, Value)],
    subs: &mut Vec<(u32, NodeData)>,
    ids: &mut SubIds,
) -> Result<Vec<u8>, String> {
    let mut heap = Heap::new();
    let mut sorted: Vec<&(u16, Value)> = props.iter().collect();
    sorted.sort_by_key(|(id, _)| *id);
    let mut records = Vec::new();
    for (id, value) in sorted {
        let hnid = match value {
            Value::I16(v) => u32::from(*v as u16),
            Value::I32(v) => *v as u32,
            Value::Bool(v) => u32::from(*v),
            other => {
                let bytes = value_bytes(other)?;
                if bytes.len() > HEAP_ITEM_MAX {
                    let sub = ids.take(NT_LTP);
                    subs.push((sub, NodeData { data: bytes, subs: Vec::new() }));
                    sub
                } else {
                    heap.alloc(bytes)?
                }
            }
        };
        let mut r = Vec::new();
        put_u16(&mut r, *id);
        put_u16(&mut r, value.prop_type());
        put_u32(&mut r, hnid);
        records.push(r);
    }
    let root = heap.bth(2, 6, &records)?;
    heap.build(0xBC, root)
}

/// The stored bytes of a variable-size or 8-byte value.
fn value_bytes(v: &Value) -> Result<Vec<u8>, String> {
    Ok(match v {
        Value::I16(x) => x.to_le_bytes().to_vec(),
        Value::I32(x) => x.to_le_bytes().to_vec(),
        Value::Bool(x) => vec![u8::from(*x)],
        Value::I64(x) => x.to_le_bytes().to_vec(),
        Value::Double(x) => x.to_le_bytes().to_vec(),
        Value::Time(x) => filetime(*x).to_le_bytes().to_vec(),
        Value::Unicode(s) => utf16(s),
        Value::String8(b) | Value::Binary(b) => b.clone(),
        Value::Object(node, size) => {
            let mut out = node.to_le_bytes().to_vec();
            out.extend_from_slice(&size.to_le_bytes());
            out
        }
        Value::MvUnicode(list) => {
            let mut out = Vec::new();
            put_u32(&mut out, len32(list.len())?);
            let mut offset = 4usize.saturating_add(list.len().saturating_mul(4));
            let mut data = Vec::new();
            for s in list {
                put_u32(&mut out, len32(offset)?);
                let b = utf16(s);
                offset = offset.saturating_add(b.len());
                data.extend_from_slice(&b);
            }
            out.extend_from_slice(&data);
            out
        }
    })
}

/// A table column: (property id, type).
type Column = (u16, u16);

/// Table context bytes: `rows` are (row id, values by column).
fn table_context(
    columns: &[Column],
    rows: &[(u32, Vec<Option<Value>>)],
    subs: &mut Vec<(u32, NodeData)>,
    ids: &mut SubIds,
) -> Result<Vec<u8>, String> {
    let size = |ty: u16| match ty {
        types::I64 | types::TIME | types::DOUBLE => 8usize,
        types::I16 => 2,
        types::BOOL => 1,
        _ => 4,
    };
    // Layout: row id, row version, then 8-, 4-, 2- and 1-byte columns.
    let mut layout: Vec<(usize, Column, usize, usize)> = Vec::new(); // (column index, column, offset, size)
    let mut offset = 8usize;
    for want in [8usize, 4, 2, 1] {
        for (i, c) in columns.iter().enumerate() {
            if size(c.1) == want {
                layout.push((i, *c, offset, want));
                offset = offset.saturating_add(want);
            }
        }
        if want == 4 {
            // end of the 4-byte section is recorded below
        }
    }
    let end4 = 8 + layout.iter().filter(|l| l.3 >= 4).map(|l| l.3).sum::<usize>();
    let end2 = end4 + layout.iter().filter(|l| l.3 == 2).map(|l| l.3).sum::<usize>();
    let end1 = end2 + layout.iter().filter(|l| l.3 == 1).map(|l| l.3).sum::<usize>();
    let n_cols = columns.len().saturating_add(2);
    let bm = end1.saturating_add(n_cols.div_ceil(8));
    // Existence bits: row id 0, row version 1, columns from 2 in their given order.
    let mut heap = Heap::new();
    let mut matrix = Vec::new();
    let mut index_records = Vec::new();
    let mut sorted_rows: Vec<&(u32, Vec<Option<Value>>)> = rows.iter().collect();
    sorted_rows.sort_by_key(|r| r.0);
    for (row_index, (row_id, values)) in rows.iter().enumerate() {
        let mut row = vec![0u8; bm];
        if let Some(slot) = row.get_mut(0..4) {
            slot.copy_from_slice(&row_id.to_le_bytes());
        }
        let mut ceb = vec![0u8; n_cols.div_ceil(8)];
        let set_bit = |bit: usize, ceb: &mut Vec<u8>| {
            if let Some(b) = ceb.get_mut(bit / 8) {
                *b |= 0x80 >> (bit % 8);
            }
        };
        set_bit(0, &mut ceb);
        set_bit(1, &mut ceb);
        for (i, (_, ty), off, sz) in &layout {
            let Some(Some(v)) = values.get(*i) else { continue };
            if v.prop_type() != *ty {
                return Err(format!("column {i} holds a value of another type"));
            }
            let bytes = match v {
                Value::I16(_)
                | Value::I32(_)
                | Value::Bool(_)
                | Value::I64(_)
                | Value::Double(_)
                | Value::Time(_) => value_bytes(v)?,
                other => {
                    let b = value_bytes(other)?;
                    let hnid = if b.len() > HEAP_ITEM_MAX {
                        let sub = ids.take(NT_LTP);
                        subs.push((sub, NodeData { data: b, subs: Vec::new() }));
                        sub
                    } else {
                        heap.alloc(b)?
                    };
                    hnid.to_le_bytes().to_vec()
                }
            };
            if let Some(slot) = row.get_mut(*off..off.saturating_add(*sz)) {
                slot.copy_from_slice(bytes.get(..*sz).unwrap_or(&[]));
            }
            set_bit(i.saturating_add(2), &mut ceb);
        }
        if let Some(slot) = row.get_mut(end1..bm) {
            slot.copy_from_slice(&ceb);
        }
        matrix.extend_from_slice(&row);
        let _ = row_index;
    }
    for (row_index, (row_id, _)) in rows.iter().enumerate() {
        let mut r = Vec::new();
        put_u32(&mut r, *row_id);
        put_u32(&mut r, len32(row_index)?);
        index_records.push((*row_id, r));
    }
    index_records.sort_by_key(|(id, _)| *id);
    let index_records: Vec<Vec<u8>> = index_records.into_iter().map(|(_, r)| r).collect();
    let row_index = heap.bth(4, 4, &index_records)?;
    let rows_hnid = if matrix.is_empty() {
        0
    } else if matrix.len() > HEAP_ITEM_MAX {
        let sub = ids.take(NT_LTP);
        subs.push((sub, NodeData { data: matrix, subs: Vec::new() }));
        sub
    } else {
        heap.alloc(matrix)?
    };
    let mut info = vec![0x7C, u8::try_from(n_cols).map_err(|_| "too many columns".to_owned())?];
    for v in [end4, end2, end1, bm] {
        put_u16(&mut info, len16(v)?);
    }
    put_u32(&mut info, row_index);
    put_u32(&mut info, rows_hnid);
    put_u32(&mut info, 0);
    let mut descs: Vec<(u32, u16, u8, u8)> = vec![(0x67F2_0003, 0, 4, 0), (0x67F3_0003, 4, 4, 1)];
    for (i, (id, ty), off, sz) in &layout {
        descs.push((
            (u32::from(*id) << 16) | u32::from(*ty),
            len16(*off)?,
            u8::try_from(*sz).unwrap_or(4),
            u8::try_from(i.saturating_add(2)).map_err(|_| "too many columns".to_owned())?,
        ));
    }
    descs.sort_by_key(|d| d.0);
    for (tag, ib, cb, bit) in descs {
        put_u32(&mut info, tag);
        put_u16(&mut info, ib);
        info.push(cb);
        info.push(bit);
    }
    let root = heap.alloc(info)?;
    heap.build(0x7C, root)
}

/// One NBT row.
struct NodeEntry {
    nid: u32,
    data: u64,
    sub: u64,
    parent: u32,
}

/// The file being written.
struct Writer {
    out: Vec<u8>,
    blocks: Vec<(u64, u64, u16)>,
    nodes: Vec<NodeEntry>,
    next_bid: u64,
    next_page: u64,
    crypt: Crypt,
}

impl Writer {
    fn bid(&mut self, internal: bool) -> u64 {
        let b = self.next_bid;
        self.next_bid = self.next_bid.saturating_add(4);
        if internal { b | 2 } else { b }
    }

    fn align(&mut self, to: usize) {
        while !self.out.len().is_multiple_of(to) {
            self.out.push(0);
        }
    }

    /// Writes one block; its BID.
    fn block(&mut self, payload: &[u8], internal: bool) -> Result<u64, String> {
        let bid = self.bid(internal);
        let mut stored = payload.to_vec();
        if !internal && self.crypt == Crypt::Permute {
            for b in &mut stored {
                *b = PERMUTE_R.get(usize::from(*b)).copied().unwrap_or(*b);
            }
        }
        self.align(64);
        let ib = self.out.len() as u64;
        self.out.extend_from_slice(&stored);
        let total = stored.len().saturating_add(16).div_ceil(64).saturating_mul(64);
        while self.out.len() < (ib as usize).saturating_add(total).saturating_sub(16) {
            self.out.push(0);
        }
        let cb = len16(stored.len())?;
        put_u16(&mut self.out, cb);
        put_u16(&mut self.out, signature(ib, bid));
        put_u32(&mut self.out, crc(&stored));
        put_u64(&mut self.out, bid);
        self.blocks.push((bid, ib, cb));
        Ok(bid)
    }

    /// Writes data of any size: one block, or a data tree.
    fn data(&mut self, data: &[u8]) -> Result<u64, String> {
        if data.is_empty() {
            return Err("empty node data".into());
        }
        if data.len() <= BLOCK_MAX {
            return self.block(data, false);
        }
        let mut bids = Vec::new();
        for chunk in data.chunks(BLOCK_MAX) {
            bids.push(self.block(chunk, false)?);
        }
        if bids.len() > (BLOCK_MAX - 8) / 8 {
            return Err("data too large for one XBLOCK".into());
        }
        let mut x = vec![0x01, 0x01];
        put_u16(&mut x, len16(bids.len())?);
        put_u32(&mut x, len32(data.len())?);
        for b in bids {
            put_u64(&mut x, b);
        }
        self.block(&x, true)
    }

    /// Writes a node's sub-nodes; the SLBLOCK's BID (0 when there are none and `force` is false).
    fn subnodes(&mut self, subs: &[(u32, NodeData)], force: bool) -> Result<u64, String> {
        if subs.is_empty() && !force {
            return Ok(0);
        }
        let mut entries = Vec::new();
        for (sub_nid, node) in subs {
            let data = self.data(&node.data)?;
            let sub = self.subnodes(&node.subs, false)?;
            entries.push((*sub_nid, data, sub));
        }
        entries.sort_by_key(|e| e.0);
        if entries.len() > (BLOCK_MAX - 8) / 24 {
            return Err("too many sub-nodes".into());
        }
        let mut sl = vec![0x02, 0x00];
        put_u16(&mut sl, len16(entries.len())?);
        put_u32(&mut sl, 0);
        for (n, d, s) in entries {
            put_u64(&mut sl, u64::from(n));
            put_u64(&mut sl, d);
            put_u64(&mut sl, s);
        }
        self.block(&sl, true)
    }

    fn node(&mut self, nid: u32, parent: u32, node: &NodeData, force_sub: bool) -> Result<(), String> {
        let data = self.data(&node.data)?;
        let sub = self.subnodes(&node.subs, force_sub)?;
        self.nodes.push(NodeEntry { nid, data, sub, parent });
        Ok(())
    }

    fn page(
        &mut self,
        entries: &[u8],
        count: usize,
        max: usize,
        entry_size: usize,
        level: u8,
        ptype: u8,
    ) -> Result<(u64, u64), String> {
        self.align(PAGE);
        let ib = self.out.len() as u64;
        let bid = self.next_page;
        self.next_page = self.next_page.saturating_add(4);
        let mut page = entries.to_vec();
        page.resize(488, 0);
        page.push(u8::try_from(count).map_err(|_| "page overflow".to_owned())?);
        page.push(u8::try_from(max).map_err(|_| "page overflow".to_owned())?);
        page.push(u8::try_from(entry_size).map_err(|_| "page overflow".to_owned())?);
        page.push(level);
        put_u32(&mut page, 0);
        let c = crc(&page);
        page.push(ptype);
        page.push(ptype);
        put_u16(&mut page, signature(ib, bid));
        put_u32(&mut page, c);
        put_u64(&mut page, bid);
        self.out.extend_from_slice(&page);
        Ok((bid, ib))
    }

    /// Writes a B-tree of `(key, entry bytes)`; the root page's BREF.
    fn btree(
        &mut self,
        mut leaf: Vec<(u64, Vec<u8>)>,
        entry_size: usize,
        ptype: u8,
    ) -> Result<(u64, u64), String> {
        leaf.sort_by_key(|e| e.0);
        // Pages of one level: (first key, entries); the level above points at each page.
        let mut level: u8 = 0;
        let mut entries = leaf;
        let mut size = entry_size;
        loop {
            let max = 488 / size;
            if entries.len() <= max {
                let bytes: Vec<u8> = entries.iter().flat_map(|e| e.1.clone()).collect();
                return self.page(&bytes, entries.len(), max, size, level, ptype);
            }
            if level >= 7 {
                return Err("B-tree too deep".into());
            }
            let mut parents = Vec::new();
            for chunk in entries.chunks(max) {
                let bytes: Vec<u8> = chunk.iter().flat_map(|e| e.1.clone()).collect();
                let key = chunk.first().map(|e| e.0).unwrap_or(0);
                let (bid, ib) = self.page(&bytes, chunk.len(), max, size, level, ptype)?;
                let mut e = Vec::new();
                put_u64(&mut e, key);
                put_u64(&mut e, bid);
                put_u64(&mut e, ib);
                parents.push((key, e));
            }
            entries = parents;
            size = 24;
            level = level.saturating_add(1);
        }
    }
}

impl PstBuilder {
    /// An empty store named `name`.
    pub fn new(name: &str) -> PstBuilder {
        PstBuilder { name: name.to_owned(), ..Default::default() }
    }

    /// Adds a folder; its index.
    pub fn folder(&mut self, parent: Option<usize>, name: &str, class: &str) -> usize {
        self.folders.push(FolderDef {
            name: name.to_owned(),
            class: class.to_owned(),
            parent,
            items: Vec::new(),
        });
        self.folders.len().saturating_sub(1)
    }

    /// Adds an item to folder `folder` (ignored for an unknown folder).
    pub fn item(&mut self, folder: usize, item: ItemDef) {
        if let Some(f) = self.folders.get_mut(folder) {
            f.items.push(item);
        }
    }

    /// The PST file.
    pub fn build(&self) -> Result<Vec<u8>, String> {
        let mut names = Names::default();
        let mut w = Writer {
            out: vec![0u8; DATA_START],
            blocks: Vec::new(),
            nodes: Vec::new(),
            next_bid: 4,
            next_page: 0x1_0000,
            crypt: self.crypt,
        };
        // Folder ids: root 0x122, IPM subtree, then the folders in order.
        let ipm = nid(NT_NORMAL_FOLDER, 0x400);
        let folder_nid = |i: usize| nid(NT_NORMAL_FOLDER, 0x401u32.saturating_add(i as u32));
        let mut next_message: u32 = 0x10000;

        // Messages first, so the contents tables know their ids.
        let mut contents: Vec<Vec<u32>> = vec![Vec::new(); self.folders.len()];
        for (fi, f) in self.folders.iter().enumerate() {
            for item in &f.items {
                next_message = next_message.saturating_add(1);
                let m = nid(NT_NORMAL_MESSAGE, next_message);
                let node = message_node(item, &mut names, 0)?;
                w.node(m, folder_nid(fi), &node, true)?;
                if let Some(c) = contents.get_mut(fi) {
                    c.push(m);
                }
            }
        }

        // Folders.
        let write_folder = |w: &mut Writer,
                            id: u32,
                            parent: u32,
                            name: &str,
                            class: &str,
                            children: &[u32],
                            items: &[u32]|
         -> Result<(), String> {
            let mut subs = Vec::new();
            let mut ids = SubIds { next: 0 };
            let props = vec![
                (0x3001u16, Value::Unicode(name.to_owned())),
                (0x3602, Value::I32(i32::try_from(items.len()).unwrap_or(i32::MAX))),
                (0x3603, Value::I32(0)),
                (0x360A, Value::Bool(!children.is_empty())),
                (0x3613, Value::Unicode(class.to_owned())),
            ];
            let pc = property_context(&props, &mut subs, &mut ids)?;
            w.node(id, parent, &NodeData { data: pc, subs }, false)?;
            let cols: Vec<Column> = vec![(0x3001, types::UNICODE)];
            let mut subs = Vec::new();
            let rows: Vec<(u32, Vec<Option<Value>>)> = children.iter().map(|c| (*c, vec![None])).collect();
            let tc = table_context(&cols, &rows, &mut subs, &mut ids)?;
            w.node((id & !0x1F) | NT_HIERARCHY_TABLE, parent, &NodeData { data: tc, subs }, false)?;
            let cols: Vec<Column> = vec![(0x0037, types::UNICODE)];
            let mut subs = Vec::new();
            let rows: Vec<(u32, Vec<Option<Value>>)> = items.iter().map(|c| (*c, vec![None])).collect();
            let tc = table_context(&cols, &rows, &mut subs, &mut ids)?;
            w.node((id & !0x1F) | NT_CONTENTS_TABLE, parent, &NodeData { data: tc, subs }, false)?;
            let mut subs = Vec::new();
            let tc = table_context(&cols, &[], &mut subs, &mut ids)?;
            w.node((id & !0x1F) | NT_ASSOC_CONTENTS_TABLE, parent, &NodeData { data: tc, subs }, false)?;
            Ok(())
        };
        let top: Vec<u32> = self
            .folders
            .iter()
            .enumerate()
            .filter(|(_, f)| f.parent.is_none())
            .map(|(i, _)| folder_nid(i))
            .collect();
        write_folder(&mut w, NID_ROOT_FOLDER, NID_ROOT_FOLDER, "", "", &[ipm], &[])?;
        write_folder(&mut w, ipm, NID_ROOT_FOLDER, "Top of Personal Folders", "", &top, &[])?;
        for (i, f) in self.folders.iter().enumerate() {
            let children: Vec<u32> = self
                .folders
                .iter()
                .enumerate()
                .filter(|(_, c)| c.parent == Some(i))
                .map(|(j, _)| folder_nid(j))
                .collect();
            let parent = match f.parent {
                Some(p) if p < self.folders.len() => folder_nid(p),
                _ => ipm,
            };
            let items = contents.get(i).cloned().unwrap_or_default();
            write_folder(&mut w, folder_nid(i), parent, &f.name, &f.class, &children, &items)?;
        }

        // Message store.
        let entry_id = |n: u32| {
            let mut e = Vec::new();
            put_u32(&mut e, 0);
            e.extend_from_slice(&RECORD_KEY);
            put_u32(&mut e, n);
            e
        };
        let mut store: Vec<(u16, Value)> = vec![
            (0x0FF9, Value::Binary(RECORD_KEY.to_vec())),
            (0x3001, Value::Unicode(self.name.clone())),
            (0x35E0, Value::Binary(entry_id(ipm))),
            (0x35E7, Value::Binary(entry_id(nid(0x03, 0x401)))),
        ];
        if let Some(wb) = self.wastebasket {
            store.push((0x35E3, Value::Binary(entry_id(folder_nid(wb)))));
        }
        for (k, v) in &self.store_props {
            let id = names.id(k)?;
            store.retain(|(i, _)| *i != id);
            store.push((id, v.clone()));
        }
        let mut subs = Vec::new();
        let mut ids = SubIds { next: 0 };
        let pc = property_context(&store, &mut subs, &mut ids)?;
        w.node(NID_MESSAGE_STORE, 0, &NodeData { data: pc, subs }, false)?;

        // Name-to-id map.
        let mut guids: Vec<[u8; 16]> = Vec::new();
        let mut entries = Vec::new();
        let mut strings = Vec::new();
        for (i, (guid, lid, name)) in names.list.iter().enumerate() {
            let guid_index: u16 = if *guid == PS_PUBLIC_STRINGS {
                2
            } else {
                let p = match guids.iter().position(|g| g == guid) {
                    Some(p) => p,
                    None => {
                        guids.push(*guid);
                        guids.len().saturating_sub(1)
                    }
                };
                len16(p)?.saturating_add(3)
            };
            let (property, kind) = match (lid, name) {
                (Some(l), _) => (*l, 0u16),
                (None, Some(n)) => {
                    let off = len32(strings.len())?;
                    let b = utf16(n);
                    put_u32(&mut strings, len32(b.len())?);
                    strings.extend_from_slice(&b);
                    while strings.len() % 4 != 0 {
                        strings.push(0);
                    }
                    (off, 1u16)
                }
                (None, None) => (0, 0),
            };
            put_u32(&mut entries, property);
            put_u16(&mut entries, (guid_index << 1) | kind);
            put_u16(&mut entries, len16(i)?);
        }
        let map = vec![
            (0x0001u16, Value::I32(251)),
            (0x0002, Value::Binary(guids.concat())),
            (0x0003, Value::Binary(entries)),
            (0x0004, Value::Binary(strings)),
        ];
        let mut subs = Vec::new();
        let pc = property_context(&map, &mut subs, &mut ids)?;
        w.node(NID_NAME_TO_ID_MAP, 0, &NodeData { data: pc, subs }, false)?;

        // B-trees.
        let nbt: Vec<(u64, Vec<u8>)> = w
            .nodes
            .iter()
            .map(|n| {
                let mut e = Vec::new();
                put_u64(&mut e, u64::from(n.nid));
                put_u64(&mut e, n.data);
                put_u64(&mut e, n.sub);
                put_u32(&mut e, n.parent);
                put_u32(&mut e, 0);
                (u64::from(n.nid), e)
            })
            .collect();
        let bbt: Vec<(u64, Vec<u8>)> = w
            .blocks
            .iter()
            .map(|(bid, ib, cb)| {
                let mut e = Vec::new();
                put_u64(&mut e, *bid);
                put_u64(&mut e, *ib);
                put_u16(&mut e, *cb);
                put_u16(&mut e, 2);
                put_u32(&mut e, 0);
                (*bid, e)
            })
            .collect();
        let nbt_ref = w.btree(nbt, 32, 0x81)?;
        let bbt_ref = w.btree(bbt, 24, 0x80)?;
        w.align(PAGE);
        let eof = w.out.len() as u64;

        // Header.
        let mut h = Vec::new();
        put_u32(&mut h, 0x4E44_4221); // "!BDN"
        put_u32(&mut h, 0); // dwCRCPartial
        put_u16(&mut h, 0x4D53); // "SM"
        put_u16(&mut h, 23);
        put_u16(&mut h, 19);
        h.push(0x01);
        h.push(0x01);
        put_u32(&mut h, 0);
        put_u32(&mut h, 0);
        put_u64(&mut h, 0);
        put_u64(&mut h, w.next_page);
        put_u32(&mut h, 1);
        for i in 0..32u32 {
            put_u32(&mut h, if i == NT_NORMAL_MESSAGE { next_message.saturating_add(1) } else { 0x1000 });
        }
        put_u64(&mut h, 0);
        // ROOT
        put_u32(&mut h, 0);
        put_u64(&mut h, eof);
        put_u64(&mut h, 0x4400);
        put_u64(&mut h, 0);
        put_u64(&mut h, 0);
        put_u64(&mut h, nbt_ref.0);
        put_u64(&mut h, nbt_ref.1);
        put_u64(&mut h, bbt_ref.0);
        put_u64(&mut h, bbt_ref.1);
        h.push(0); // fAMapValid: allocation maps not written
        h.push(0);
        put_u16(&mut h, 0);
        put_u32(&mut h, 0); // dwAlign
        h.extend_from_slice(&[0xFF; 128]); // rgbFM
        h.extend_from_slice(&[0xFF; 128]); // rgbFP
        h.push(0x80);
        h.push(match self.crypt {
            Crypt::None => 0,
            Crypt::Permute => 1,
        });
        put_u16(&mut h, 0);
        put_u64(&mut h, w.next_bid);
        let partial = crc(h.get(8..8 + 471).unwrap_or(&[]));
        let full = crc(h.get(8..8 + 516).unwrap_or(&[]));
        put_u32(&mut h, full);
        h.extend_from_slice(&[0u8; 36]);
        if let Some(slot) = h.get_mut(4..8) {
            slot.copy_from_slice(&partial.to_le_bytes());
        }
        if let Some(slot) = w.out.get_mut(0..h.len()) {
            slot.copy_from_slice(&h);
        }
        Ok(w.out)
    }
}

/// The node of a message: its property context, recipient and attachment tables, attachments.
fn message_node(item: &ItemDef, names: &mut Names, depth: usize) -> Result<NodeData, String> {
    if depth > 4 {
        return Err("embedded messages nest too deep".into());
    }
    let mut subs = Vec::new();
    let mut ids = SubIds { next: 0x100 };
    let mut props = Vec::new();
    for (k, v) in &item.props {
        props.push((names.id(k)?, v.clone()));
    }
    let pc = property_context(&props, &mut subs, &mut ids)?;

    // Recipient table.
    let rcols: Vec<Column> = vec![
        (0x0C15, types::I32),
        (0x3001, types::UNICODE),
        (0x3002, types::UNICODE),
        (0x3003, types::UNICODE),
        (0x39FE, types::UNICODE),
    ];
    let mut rows = Vec::new();
    for (i, r) in item.recipients.iter().enumerate() {
        let mut values = vec![None; rcols.len()];
        for (k, v) in r {
            let id = names.id(k)?;
            if let Some(pos) = rcols.iter().position(|c| c.0 == id)
                && let Some(slot) = values.get_mut(pos)
            {
                *slot = Some(v.clone());
            }
        }
        rows.push((len32(i)?, values));
    }
    let mut tsubs = Vec::new();
    let rt = table_context(&rcols, &rows, &mut tsubs, &mut ids)?;
    subs.extend(tsubs);
    subs.push((NID_RECIPIENT_TABLE, NodeData { data: rt, subs: Vec::new() }));

    // Attachments.
    if !item.attachments.is_empty() {
        let acols: Vec<Column> = vec![
            (0x0E20, types::I32),
            (0x3001, types::UNICODE),
            (0x3704, types::UNICODE),
            (0x3705, types::I32),
        ];
        let mut rows = Vec::new();
        for (i, a) in item.attachments.iter().enumerate() {
            let anid = nid(NT_ATTACHMENT, 0x20u32.saturating_add(len32(i)?));
            let mut asubs = Vec::new();
            let mut aids = SubIds { next: 0x200 };
            let mut aprops = Vec::new();
            for (k, v) in &a.props {
                aprops.push((names.id(k)?, v.clone()));
            }
            if let Some(inner) = &a.embedded {
                let mnid = aids.take(NT_NORMAL_MESSAGE);
                let node = message_node(inner, names, depth.saturating_add(1))?;
                asubs.push((mnid, node));
                // PidTagAttachDataObject: the embedded message's sub-node and its size.
                aprops.retain(|(i, _)| *i != 0x3701);
                aprops.push((0x3701, Value::Object(mnid, 0)));
            }
            let mut values = vec![None; acols.len()];
            for (id, v) in &aprops {
                if let Some(pos) = acols.iter().position(|c| c.0 == *id && c.1 == v.prop_type())
                    && let Some(slot) = values.get_mut(pos)
                {
                    *slot = Some(v.clone());
                }
            }
            let mut pc_subs = Vec::new();
            let apc = property_context(&aprops, &mut pc_subs, &mut aids)?;
            asubs.extend(pc_subs);
            subs.push((anid, NodeData { data: apc, subs: asubs }));
            rows.push((anid, values));
        }
        let mut tsubs = Vec::new();
        let at = table_context(&acols, &rows, &mut tsubs, &mut ids)?;
        subs.extend(tsubs);
        subs.push((NID_ATTACHMENT_TABLE, NodeData { data: at, subs: Vec::new() }));
    }
    Ok(NodeData { data: pc, subs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_spec_check_value_form() {
        // The [MS-PST] CRC starts from 0 with no final complement: CRC("") = 0, and it differs from the
        // usual CRC-32 of "123456789" (0xCBF43926) by the missing pre/post inversion.
        assert_eq!(crc(b""), 0);
        assert_ne!(crc(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn builds_a_file_with_a_valid_header() {
        let mut b = PstBuilder::new("Test store");
        let inbox = b.folder(None, "Inbox", "IPF.Note");
        b.item(inbox, ItemDef::new("IPM.Note").tag(0x0037, Value::Unicode("Hello".into())));
        let file = b.build().unwrap();
        assert_eq!(file.get(0..4), Some(b"!BDN".as_slice()));
        assert_eq!(file.len() % PAGE, 0);
    }
}
