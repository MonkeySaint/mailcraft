//! The walk over a PST file with the `outlook-pst` crate (see the module documentation of [`super`]).

use std::collections::HashSet;
use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{AnsiAttachment, Attachment, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::message::{AnsiMessage, Message, UnicodeMessage};
use outlook_pst::messaging::store::{AnsiStore, EntryId, Store, UnicodeStore};
use outlook_pst::ndb::node_id::{NodeId, NodeIdType};
use outlook_pst::{AnsiPstFile, PstReader, UnicodePstFile};

use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{ItemAttachment, MAX_ITEM_ATTACHMENT_BYTES, MAX_ITEM_ATTACHMENTS};
use mailcraft_model::{FlagState, FolderId, FolderKind};

use super::pim::{self, Item, Names};
use super::{
    Control, Guarded, MAX_ATTACHMENT_BYTES, MAX_FOLDER_DEPTH, MAX_FOLDERS, Options, PstFolder, PstItem,
    Report, Sink, Source, Version, kind_for_class, malformed, password_error, role_for_name, sniff,
};
use crate::msg::props::{
    MV_FLAG, PT_BINARY, PT_BOOLEAN, PT_DOUBLE, PT_FLOAT, PT_I2, PT_I8, PT_LONG, PT_STRING8, PT_SYSTIME,
    PT_UNICODE, Props, Value,
};
use crate::msg::read::{RawAttachment, assemble};
use crate::msg::{MAX_ATTACHMENTS, MAX_EMBED_DEPTH, MAX_RECIPIENTS};
use crate::{FormatError, Result};

/// Message properties the mapping reads (named ones are added from the file's map).
const MAIL_TAGS: &[u16] = &[
    0x0017, 0x001A, 0x0036, 0x0037, 0x0039, 0x0042, 0x0060, 0x0061, 0x0064, 0x0065, 0x007D, 0x0C1A, 0x0C1E,
    0x0C1F, 0x0E06, 0x0E07, 0x0E1D, 0x1000, 0x1009, 0x1013, 0x1035, 0x1039, 0x1042, 0x3001, 0x3007, 0x3008,
    0x3FDE, 0x3FFD, 0x5D01, 0x5D02,
];
/// Most properties converted per object.
const MAX_PROPS: usize = 8_192;
/// Most values kept from a multi-valued property.
const MAX_MV_VALUES: usize = 1_024;
/// Most rows read from a contents or hierarchy table.
const MAX_ROWS: usize = 1_000_000;
/// The code page of 8-bit strings when an object names none.
const DEFAULT_CODEPAGE: u32 = 1252;

/// Where the bytes come from (kept so the file can be reopened after a reader failure).
#[derive(Clone)]
enum Data {
    File(PathBuf),
    Bytes(Arc<[u8]>),
}

impl Data {
    fn reader(&self, budget: u64, control: &Arc<Control>) -> io::Result<Box<dyn PstReader>> {
        Ok(match self {
            Data::File(p) => Box::new(Guarded::new(
                io::BufReader::new(std::fs::File::open(p)?),
                budget,
                Arc::clone(control),
            )),
            Data::Bytes(b) => {
                Box::new(Guarded::new(io::Cursor::new(Arc::clone(b)), budget, Arc::clone(control)))
            }
        })
    }
}

/// An open file of either format.
enum Pst {
    Unicode(Rc<UnicodeStore>),
    Ansi(Rc<AnsiStore>),
}

/// An open message of either format.
enum Msg {
    Unicode(Rc<UnicodeMessage>),
    Ansi(Rc<AnsiMessage>),
}

impl Pst {
    fn open(version: Version, reader: Box<dyn PstReader>) -> io::Result<Pst> {
        Ok(match version {
            Version::Unicode => {
                Pst::Unicode(UnicodeStore::read(Rc::new(UnicodePstFile::read_from(reader)?))?)
            }
            Version::Ansi => Pst::Ansi(AnsiStore::read(Rc::new(AnsiPstFile::read_from(reader)?))?),
        })
    }

    fn store(&self) -> Rc<dyn Store> {
        match self {
            Pst::Unicode(s) => Rc::clone(s) as Rc<dyn Store>,
            Pst::Ansi(s) => Rc::clone(s) as Rc<dyn Store>,
        }
    }

    fn message(&self, entry: &EntryId, wanted: &[u16]) -> io::Result<Msg> {
        Ok(match self {
            Pst::Unicode(s) => Msg::Unicode(UnicodeMessage::read(Rc::clone(s), entry, Some(wanted))?),
            Pst::Ansi(s) => Msg::Ansi(AnsiMessage::read(Rc::clone(s), entry, Some(wanted))?),
        })
    }
}

impl Msg {
    fn get(&self) -> &dyn Message {
        match self {
            Msg::Unicode(m) => m.as_ref(),
            Msg::Ansi(m) => m.as_ref(),
        }
    }

    fn attachment(&self, nid: NodeId, wanted: &[u16]) -> io::Result<Rc<dyn Attachment>> {
        Ok(match self {
            Msg::Unicode(m) => {
                UnicodeAttachment::read(Rc::clone(m), nid, Some(wanted))? as Rc<dyn Attachment>
            }
            Msg::Ansi(m) => AnsiAttachment::read(Rc::clone(m), nid, Some(wanted))? as Rc<dyn Attachment>,
        })
    }
}

fn text(e: impl std::fmt::Display) -> String {
    let s = e.to_string();
    crate::text::clip(&s, 300)
}

/// A folder's own properties and what it lists.
struct FolderInfo {
    name: String,
    class: String,
    count: u32,
    children: Vec<u32>,
    messages: Vec<u32>,
}

/// A folder waiting to be read.
/// (Its path is built from [`Walker::folders`] when needed: a cloned path per pending folder would let a
/// file whose folders list many children take far more memory than it is large.)
struct Pending {
    nid: u32,
    parent: Option<usize>,
    depth: usize,
}

struct Walker<'a> {
    data: Data,
    version: Version,
    control: Arc<Control>,
    options: Options,
    budget: u64,
    pst: Pst,
    names: Names,
    wanted: Vec<u16>,
    wastebasket: Option<u32>,
    sink: &'a mut dyn Sink,
    report: Report,
    items: usize,
    stopped: bool,
    /// Parent and name of each folder read (by index), for the paths in the report.
    folders: Vec<(Option<usize>, String)>,
    /// Mixed into the UIDs made for calendar items that carry none ([`Walker::fallback_uid`]).
    uid_seed: u64,
}

pub(super) fn read(
    source: Source,
    options: &Options,
    control: &Arc<Control>,
    sink: &mut dyn Sink,
) -> Result<Report> {
    let (data, size) = match source {
        Source::Path(p) => {
            let meta = std::fs::metadata(&p)?;
            if !meta.is_file() {
                return Err(FormatError::Io(format!("{} is not a file", p.display())));
            }
            (Data::File(p), meta.len())
        }
        Source::Bytes(b) => {
            let n = u64::try_from(b.len()).unwrap_or(u64::MAX);
            (Data::Bytes(Arc::from(b)), n)
        }
    };
    if size > super::MAX_PST_BYTES {
        return Err(FormatError::TooLarge {
            format: "pst",
            size: usize::try_from(size).unwrap_or(usize::MAX),
            limit: usize::try_from(super::MAX_PST_BYTES).unwrap_or(usize::MAX),
        });
    }
    let mut head = Vec::new();
    let mut r = data.reader(1024, control)?;
    (&mut r).take(600).read_to_end(&mut head)?;
    let version = match sniff(&head) {
        None => return Err(malformed("not a PST file")),
        Some(Err(e)) => return Err(e),
        Some(Ok(v)) => v,
    };
    control.file_bytes.store(size, Ordering::Relaxed);
    let budget = size.saturating_mul(options.read_factor.max(2)).saturating_add(64 << 20);

    let opened = catch_unwind(AssertUnwindSafe(|| -> std::result::Result<_, String> {
        let pst = Pst::open(version, data.reader(budget, control).map_err(text)?).map_err(text)?;
        let store = pst.store();
        let props = store.properties();
        if let Some(PropertyValue::Integer32(p)) = props.get(0x67FF)
            && *p != 0
        {
            return Err(String::from("password"));
        }
        let name = props.display_name().unwrap_or_default();
        let ipm = props.ipm_sub_tree_entry_id().map_err(text)?;
        let wastebasket = props.ipm_wastebasket_entry_id().ok().map(|e| u32::from(e.node_id()));
        let names = read_names(store.as_ref());
        Ok((pst, name, u32::from(ipm.node_id()), wastebasket, names))
    }));
    let (pst, name, ipm, wastebasket, names) = match opened {
        Ok(Ok(v)) => v,
        Ok(Err(e)) if e == "password" => return Err(password_error()),
        Ok(Err(_)) if control.cancelled() => {
            return Ok(Report { stopped: Some("cancelled".into()), ..Default::default() });
        }
        Ok(Err(e)) => return Err(malformed(format!("the file is damaged ({e})"))),
        Err(_) => return Err(malformed("the file is damaged (its message store could not be read)")),
    };
    let mut wanted: Vec<u16> = MAIL_TAGS.iter().chain(pim::TAGS).copied().collect();
    wanted.extend(names.wanted());
    wanted.sort_unstable();
    wanted.dedup();
    let mut w = Walker {
        data,
        version,
        control: Arc::clone(control),
        options: *options,
        budget,
        pst,
        names,
        wanted,
        wastebasket,
        sink,
        report: Report { name: crate::text::clip(name.trim(), 256), ..Default::default() },
        items: 0,
        stopped: false,
        folders: Vec::new(),
        uid_seed: uid_seed(options),
    };
    w.walk(ipm);
    // A table the cancel cut short reads as empty: whatever the walk saw, a stop asked for is a stop.
    w.must_stop();
    Ok(w.report)
}

/// The name-to-id map; empty when the file has none or it is damaged.
fn read_names(store: &dyn Store) -> Names {
    let Ok(map) = store.named_property_map() else { return Names::default() };
    let props = map.properties();
    let bin = |id: u16| match props.get(id) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        _ => Vec::new(),
    };
    Names::parse(&bin(0x0002), &bin(0x0003), &bin(0x0004))
}

/// A string property's text, in `cp` for 8-bit strings.
fn pv_string(v: &PropertyValue, cp: u32) -> Option<String> {
    match v {
        PropertyValue::String8(s) => {
            let b = s.buffer();
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            let (t, _) =
                crate::msg::encoding_for_codepage(cp).decode_without_bom_handling(b.get(..end).unwrap_or(b));
            Some(t.into_owned())
        }
        PropertyValue::Unicode(u) => {
            let b = u.buffer();
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            Some(String::from_utf16_lossy(b.get(..end).unwrap_or(b)))
        }
        _ => None,
    }
}

/// Converts crate property values to the `.msg` mapping's property map, and the object's code page.
fn to_props<'v>(values: impl Iterator<Item = (&'v u16, &'v PropertyValue)>, parent_cp: u32) -> (Props, u32) {
    let values: Vec<(u16, &PropertyValue)> = values.take(MAX_PROPS).map(|(k, v)| (*k, v)).collect();
    let int = |id: u16| {
        values.iter().find(|(k, _)| *k == id).and_then(|(_, v)| match v {
            PropertyValue::Integer32(i) => u32::try_from(*i).ok(),
            _ => None,
        })
    };
    let cp = int(0x3FFD).or_else(|| int(0x3FDE)).filter(|c| *c != 0).unwrap_or(parent_cp);
    let mut props = Props::default();
    for (id, v) in values {
        let (ty, value) = match v {
            PropertyValue::Integer16(x) => (PT_I2, Value::Int(i64::from(*x))),
            PropertyValue::Integer32(x) => (PT_LONG, Value::Int(i64::from(*x))),
            PropertyValue::Integer64(x) => (PT_I8, Value::Int(*x)),
            PropertyValue::Boolean(b) => (PT_BOOLEAN, Value::Bool(*b)),
            PropertyValue::Floating32(f) => (PT_FLOAT, Value::Float(f64::from(*f))),
            PropertyValue::Floating64(f) => (PT_DOUBLE, Value::Float(*f)),
            PropertyValue::Time(t) => match u64::try_from(*t) {
                Ok(t) => (PT_SYSTIME, Value::Time(t)),
                Err(_) => continue,
            },
            PropertyValue::String8(_) => {
                let mut s = pv_string(v, cp).unwrap_or_default();
                if id == 0x0037 {
                    s = strip_subject_prefix_marker(s);
                }
                (PT_STRING8, Value::Str(s))
            }
            PropertyValue::Unicode(_) => {
                let mut s = pv_string(v, cp).unwrap_or_default();
                if id == 0x0037 {
                    s = strip_subject_prefix_marker(s);
                }
                (PT_UNICODE, Value::Str(s))
            }
            PropertyValue::Binary(b) => (PT_BINARY, Value::Bin(b.buffer().to_vec())),
            PropertyValue::MultipleUnicode(list) => (
                PT_UNICODE | MV_FLAG,
                Value::MultiStr(
                    list.iter()
                        .take(MAX_MV_VALUES)
                        .map(|u| {
                            let b = u.buffer();
                            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                            String::from_utf16_lossy(b.get(..end).unwrap_or(b))
                        })
                        .collect(),
                ),
            ),
            PropertyValue::MultipleString8(list) => {
                let enc = crate::msg::encoding_for_codepage(cp);
                (
                    PT_STRING8 | MV_FLAG,
                    Value::MultiStr(
                        list.iter()
                            .take(MAX_MV_VALUES)
                            .map(|s| {
                                let b = s.buffer();
                                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                                enc.decode_without_bom_handling(b.get(..end).unwrap_or(b)).0.into_owned()
                            })
                            .collect(),
                    ),
                )
            }
            _ => continue,
        };
        props.map.insert((id, ty), value);
    }
    (props, cp)
}

/// PidTagSubject may start with SOH and the length of its prefix ("RE: ") ([MS-OXCMSG] §2.2.1.46).
fn strip_subject_prefix_marker(s: String) -> String {
    match s.strip_prefix('\u{1}') {
        Some(rest) => rest.chars().skip(1).collect(),
        None => s,
    }
}

/// Rows of a table as (row id, properties); multi-valued columns are left out.
fn table_rows(table: &dyn TableContext, cap: usize, cp: u32) -> Vec<(u32, Props)> {
    let context = table.context();
    let columns = context.columns();
    let mut out = Vec::new();
    for row in table.rows_matrix().take(cap) {
        let id = u32::from(row.id());
        let Ok(values) = row.columns(context) else { continue };
        let mut read = Vec::new();
        for (col, value) in columns.iter().zip(values.iter()) {
            let Some(value) = value else { continue };
            if u16::from(col.prop_type()) & MV_FLAG != 0 {
                continue;
            }
            if let Ok(v) = table.read_column(value, col.prop_type()) {
                read.push((col.prop_id(), v));
            }
        }
        let (props, _) = to_props(read.iter().map(|(k, v)| (k, v)), cp);
        out.push((id, props));
    }
    out
}

/// Ids of a table's rows whose node type is `kind`.
fn row_ids(table: Option<&Rc<dyn TableContext>>, kind: NodeIdType, cap: usize) -> Vec<u32> {
    let Some(table) = table else { return Vec::new() };
    table
        .rows_matrix()
        .take(cap)
        .map(|r| u32::from(r.id()))
        .filter(|id| NodeId::from(*id).id_type().is_ok_and(|t| t == kind))
        .collect()
}

impl Walker<'_> {
    /// Reopens the file after the reader failed (its lock and caches may be in a bad state).
    fn reopen(&mut self) -> bool {
        let left = self.budget.saturating_sub(self.control.bytes_read.load(Ordering::Relaxed));
        let opened = catch_unwind(AssertUnwindSafe(|| {
            self.data.reader(left, &self.control).and_then(|r| Pst::open(self.version, r))
        }));
        match opened {
            Ok(Ok(p)) => {
                self.pst = p;
                true
            }
            _ => false,
        }
    }

    /// Runs `f` with panics caught; a panic reopens the file.
    fn guarded<T>(
        &mut self,
        f: impl FnOnce(&Self) -> std::result::Result<T, String>,
    ) -> std::result::Result<T, String> {
        match catch_unwind(AssertUnwindSafe(|| f(self))) {
            Ok(r) => r,
            Err(_) => {
                if !self.reopen() {
                    self.stop("the file could not be read any further");
                }
                Err("damaged (it could not be read)".into())
            }
        }
    }

    fn stop(&mut self, why: &str) {
        if !self.stopped {
            self.stopped = true;
            self.report.stopped = Some(why.to_owned());
        }
    }

    /// Whether the walk must stop now.
    fn must_stop(&mut self) -> bool {
        if self.control.cancelled() {
            self.stop("cancelled");
        } else if self.control.bytes_read.load(Ordering::Relaxed) >= self.budget {
            self.stop("the file is damaged (reading it did not come to an end)");
        }
        self.stopped
    }

    fn walk(&mut self, ipm: u32) {
        let top = match self.guarded(|w| w.folder_info(ipm)) {
            Ok(info) => info.children,
            Err(e) => {
                if !self.must_stop() {
                    self.report.skip("", "", format!("the folder list could not be read: {e}"));
                }
                return;
            }
        };
        // Every folder id is queued once (a folder listed twice, or one that lists an ancestor, is read once),
        // and at most MAX_FOLDERS are ever queued, so the stack stays as small as the folder cap.
        let mut queued: HashSet<u32> = HashSet::from([ipm]);
        let mut stack: Vec<Pending> = Vec::new();
        let mut dropped = false;
        let mut told = false;
        queue(&mut stack, &mut queued, &mut dropped, top, None, 0);
        while let Some(p) = stack.pop() {
            if self.must_stop() {
                return;
            }
            if self.report.folders >= MAX_FOLDERS {
                let at = self.path_of(p.parent);
                self.report.skip(&at, "", format!("not read: a file holds at most {MAX_FOLDERS} folders"));
                self.stop("too many folders");
                return;
            }
            let info = match self.guarded(|w| w.folder_info(p.nid)) {
                Ok(i) => i,
                Err(e) => {
                    if self.must_stop() {
                        return;
                    }
                    let at = self.path_of(p.parent);
                    self.report.skip(&at, "", format!("a folder could not be read: {e}"));
                    continue;
                }
            };
            let name = clean_name(&info.name);
            let parent_path = self.path_of(p.parent);
            let path = if parent_path.is_empty() { name.clone() } else { format!("{parent_path}/{name}") };
            let kind = kind_for_class(&info.class);
            let role = if p.parent.is_none() {
                if Some(p.nid) == self.wastebasket && kind == FolderKind::Mail {
                    Some(mailcraft_model::FolderRole::DeletedItems)
                } else {
                    role_for_name(&name, kind)
                }
            } else {
                None
            };
            let index = self.report.folders;
            self.report.folders = self.report.folders.saturating_add(1);
            self.folders.push((p.parent, name.clone()));
            Control::bump(&self.control.folders_done, 1);
            Control::bump(&self.control.items_total, u64::from(info.count));
            let folder = PstFolder {
                index,
                parent: p.parent,
                name,
                class: info.class.clone(),
                kind,
                role,
                count: info.count,
            };
            if !self.sink.folder(folder) {
                self.stop("stopped");
                return;
            }
            for nid in info.messages {
                if self.must_stop() {
                    return;
                }
                if self.items >= self.options.max_items {
                    self.report.skip(
                        &path,
                        "",
                        format!("not read: an import takes at most {} items", self.options.max_items),
                    );
                    self.stop("too many items");
                    return;
                }
                self.items = self.items.saturating_add(1);
                Control::bump(&self.control.items_done, 1);
                let read = self.guarded(|w| w.item(nid, kind));
                match read {
                    Ok(ItemRead { item: Some(item), label, notes }) => {
                        for n in notes {
                            self.report.skip(&path, &label, n);
                        }
                        match &item {
                            PstItem::Mail(_) => self.report.mail = self.report.mail.saturating_add(1),
                            PstItem::Appointment(_) => {
                                self.report.appointments = self.report.appointments.saturating_add(1)
                            }
                            PstItem::Contact(_) => {
                                self.report.contacts = self.report.contacts.saturating_add(1)
                            }
                            PstItem::Task(_) => self.report.tasks = self.report.tasks.saturating_add(1),
                            PstItem::Note(_) => self.report.notes = self.report.notes.saturating_add(1),
                        }
                        if !self.sink.item(index, item) {
                            self.stop("stopped");
                            return;
                        }
                    }
                    Ok(ItemRead { item: None, label, notes }) => {
                        for n in notes {
                            self.report.skip(&path, &label, n);
                        }
                    }
                    Err(e) => {
                        if self.control.cancelled() {
                            self.stop("cancelled");
                            return;
                        }
                        self.report.skip(&path, "", format!("an item could not be read: {e}"));
                    }
                }
            }
            if p.depth.saturating_add(1) >= MAX_FOLDER_DEPTH {
                if !info.children.is_empty() {
                    self.report.skip(&path, "", "its subfolders nest too deep and were not read");
                }
                continue;
            }
            queue(
                &mut stack,
                &mut queued,
                &mut dropped,
                info.children,
                Some(index),
                p.depth.saturating_add(1),
            );
            if dropped && !told {
                // Said once; the folders queued before the cap are still read.
                told = true;
                self.report.skip(&path, "", format!("not read: a file holds at most {MAX_FOLDERS} folders"));
            }
        }
    }

    /// The path of folder `index` ("Inbox/Projects"); empty for `None`.
    fn path_of(&self, index: Option<usize>) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut at = index;
        // Parents always come before their children, so this ends; the depth bound is a second guard.
        while let Some(i) = at {
            let Some((parent, name)) = self.folders.get(i) else { break };
            parts.push(name);
            if parts.len() > MAX_FOLDER_DEPTH || parent.is_some_and(|p| p >= i) {
                break;
            }
            at = *parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// A UID for a calendar item that carries none of its own: unique to this import (the same file read
    /// twice gives other UIDs; the engine tells repeats apart by their content), never one of another file.
    fn fallback_uid(&self, nid: u32) -> String {
        format!("pst-{:016x}-{nid:08x}@mailcraft.invalid", self.uid_seed)
    }

    fn folder_info(&self, nid: u32) -> std::result::Result<FolderInfo, String> {
        let store = self.pst.store();
        let entry = store.properties().make_entry_id(NodeId::from(nid)).map_err(text)?;
        let folder = store.open_folder(&entry).map_err(text)?;
        let props = folder.properties();
        let name = props.display_name().unwrap_or_default();
        let class = props.get(0x3613).and_then(|v| pv_string(v, DEFAULT_CODEPAGE)).unwrap_or_default();
        let count = props.content_count().ok().and_then(|c| u32::try_from(c).ok()).unwrap_or(0);
        let children = row_ids(folder.hierarchy_table(), NodeIdType::NormalFolder, MAX_FOLDERS);
        let messages = row_ids(folder.contents_table(), NodeIdType::NormalMessage, MAX_ROWS);
        Ok(FolderInfo { name, class, count, children, messages })
    }

    fn item(&self, nid: u32, folder_kind: FolderKind) -> std::result::Result<ItemRead, String> {
        let store = self.pst.store();
        let entry = store.properties().make_entry_id(NodeId::from(nid)).map_err(text)?;
        let msg = self.pst.message(&entry, &self.wanted).map_err(text)?;
        let m = msg.get();
        let (props, cp) = to_props(m.properties().iter(), DEFAULT_CODEPAGE);
        let class = props.text(0x001A).unwrap_or_else(|| "IPM.Note".into());
        let lower = class.to_ascii_lowercase();
        let subject = props.string(0x0037).unwrap_or_default();
        let label = if subject.trim().is_empty() { class.clone() } else { subject.clone() };
        let mut notes = Vec::new();
        let recipients: Vec<Props> = m
            .recipient_table()
            .map(|t| table_rows(t.as_ref(), MAX_RECIPIENTS, cp).into_iter().map(|(_, p)| p).collect())
            .unwrap_or_default();
        let it = Item { props: &props, names: &self.names, offset_min: self.options.local_offset_min };
        let is = |prefix: &str| lower == prefix || lower.starts_with(&format!("{prefix}."));
        let item = if is("ipm.contact") {
            Some(PstItem::Contact(Box::new(pim::contact(&it))))
        } else if is("ipm.distlist") {
            notes.push("contact groups are not imported".into());
            None
        } else if is("ipm.appointment") {
            let organizer = organizer(&props);
            let uid = pim::meeting_uid(&it).unwrap_or_else(|| self.fallback_uid(nid));
            let (mut a, note) = pim::appointment(&it, organizer, &recipients, uid);
            notes.extend(note);
            a.attachments = self.item_attachments(&msg, cp, &mut notes);
            Some(PstItem::Appointment(Box::new(a)))
        } else if is("ipm.task") {
            Some(PstItem::Task(Box::new(pim::task(&it))))
        } else if is("ipm.stickynote") {
            Some(PstItem::Note(Box::new(pim::note(&it, self.options.now))))
        } else if is("ipm.activity") {
            notes.push("journal entries are not imported".into());
            None
        } else {
            let attachments = self.attachments(&msg, cp, 0, &mut notes);
            let mm = assemble(&props, self.names.keywords(), recipients, attachments);
            let (mut item, content) = mm.to_mail_item(FolderId::default(), self.options.now);
            if mm.message_id.is_none() {
                // A draft keeps the Message-ID its rebuilt source was given; that id is new on every read, so
                // it would make the same draft look new when an import is run again.
                item.message_id = None;
            }
            item.flag = flag(&it);
            if folder_kind != FolderKind::Mail {
                notes.push(format!("a {class} item in a {folder_kind:?} folder was imported as a message"));
            }
            Some(PstItem::Mail(Box::new((item, content))))
        };
        Ok(ItemRead { item, label, notes })
    }

    /// The attachments of a message, read up to the count and size caps.
    fn attachments(&self, msg: &Msg, cp: u32, depth: usize, notes: &mut Vec<String>) -> Vec<RawAttachment> {
        let Some(table) = msg.get().attachment_table() else { return Vec::new() };
        let mut out = Vec::new();
        let mut total: u64 = 0;
        for (row_id, row) in table_rows(table.as_ref(), MAX_ATTACHMENTS, cp) {
            let named = row
                .text(0x3707)
                .or_else(|| row.text(0x3704))
                .or_else(|| row.text(0x3001))
                .unwrap_or_else(|| "attachment".into());
            let size = row.int(0x0E20).and_then(|s| u64::try_from(s).ok()).unwrap_or(0);
            if size > MAX_ATTACHMENT_BYTES || total.saturating_add(size) > MAX_ATTACHMENT_BYTES {
                notes.push(format!("attachment \"{named}\" is too large to import ({size} bytes)"));
                continue;
            }
            let a = match msg.attachment(NodeId::from(row_id), &self.wanted) {
                Ok(a) => a,
                Err(e) => {
                    // outlook-pst 1.2 stops reading an attachment's properties at a PtypObject value, which
                    // is how every attached message is stored, so attached messages cannot be read yet.
                    if row.int(0x3705) == Some(5) {
                        notes.push(format!(
                            "attached message \"{named}\" was left out: the PST reader cannot open attached \
                             messages yet"
                        ));
                    } else {
                        notes.push(format!("attachment \"{named}\" could not be read: {}", text(e)));
                    }
                    continue;
                }
            };
            let (mut props, _) = to_props(a.properties().iter(), cp);
            let embedded = match a.data() {
                Some(AttachmentData::Binary(b)) => {
                    let n = u64::try_from(b.buffer().len()).unwrap_or(u64::MAX);
                    total = total.saturating_add(n);
                    if total > MAX_ATTACHMENT_BYTES {
                        notes.push(format!("attachment \"{named}\" is too large to import ({n} bytes)"));
                        continue;
                    }
                    if props.binary(0x3701).is_none() {
                        props.map.insert((0x3701, PT_BINARY), Value::Bin(b.buffer().to_vec()));
                    }
                    None
                }
                Some(AttachmentData::Message(inner)) => {
                    if depth >= MAX_EMBED_DEPTH {
                        notes.push("an attached message nests too deep and was left out".into());
                        continue;
                    }
                    props.map.remove(&(0x3701, PT_BINARY));
                    Some(self.embedded(inner.as_ref(), cp, notes))
                }
                None => {
                    notes.push(format!("attachment \"{named}\" is a link to a file, which is not imported"));
                    continue;
                }
            };
            out.push(RawAttachment { props, embedded });
        }
        out
    }

    /// An attached message (its own attachments cannot be opened through the crate's shared interface).
    fn embedded(&self, m: &dyn Message, cp: u32, notes: &mut Vec<String>) -> crate::msg::MsgMessage {
        let (props, cp) = to_props(m.properties().iter(), cp);
        let recipients = m
            .recipient_table()
            .map(|t| table_rows(t.as_ref(), MAX_RECIPIENTS, cp).into_iter().map(|(_, p)| p).collect());
        if m.attachment_table().is_some_and(|t| t.rows_matrix().next().is_some()) {
            notes.push("the attachments of an attached message were left out".into());
        }
        assemble(&props, self.names.keywords(), recipients.unwrap_or_default(), Vec::new())
    }

    /// File attachments of a calendar item.
    fn item_attachments(&self, msg: &Msg, cp: u32, notes: &mut Vec<String>) -> Vec<ItemAttachment> {
        let mut out = Vec::new();
        let mut total = 0usize;
        for a in self.attachments(msg, cp, MAX_EMBED_DEPTH, notes) {
            if a.embedded.is_some() {
                continue;
            }
            let Some(data) = a.props.binary(0x3701) else { continue };
            total = total.saturating_add(data.len());
            if out.len() >= MAX_ITEM_ATTACHMENTS
                || data.len() > MAX_ITEM_ATTACHMENT_BYTES
                || total > MAX_ITEM_ATTACHMENT_BYTES
            {
                notes.push("an attachment of a calendar item was too large and was left out".into());
                continue;
            }
            let filename = a
                .props
                .text(0x3707)
                .or_else(|| a.props.text(0x3704))
                .or_else(|| a.props.text(0x3001))
                .unwrap_or_else(|| "attachment".into());
            out.push(ItemAttachment {
                filename,
                mime_type: a.props.text(0x370E).unwrap_or_else(|| "application/octet-stream".into()),
                data: Arc::from(data),
            });
        }
        out
    }
}

/// What reading one item gave.
struct ItemRead {
    item: Option<PstItem>,
    /// Subject (or class) for the report.
    label: String,
    /// What was left out of it, or why it was skipped.
    notes: Vec<String>,
}

/// The organizer of a meeting: the sent-representing mailbox.
fn organizer(p: &Props) -> Option<Mailbox> {
    let addr = p.text(0x5D02).or_else(|| p.text(0x0065).filter(|e| e.contains('@')))?;
    let name = p.text(0x0042).filter(|n| !n.eq_ignore_ascii_case(&addr));
    Some(Mailbox::new(name.as_deref(), &addr))
}

/// A message's follow-up flag (PidTagFlagStatus, [MS-OXOFLAG]).
fn flag(it: &Item<'_>) -> FlagState {
    match it.props.int(0x1090) {
        Some(2) => {
            let label = it.named_text(pim::PSETID_COMMON, pim::LID_FLAG_REQUEST);
            let reminder = if it.named_bool(pim::PSETID_COMMON, pim::LID_REMINDER_SET).unwrap_or(false) {
                it.named_time(pim::PSETID_COMMON, pim::LID_REMINDER_SIGNAL_TIME)
                    .or_else(|| it.named_time(pim::PSETID_COMMON, pim::LID_REMINDER_TIME))
            } else {
                None
            };
            FlagState::Flagged {
                label: if label.trim().is_empty() {
                    "Follow up".to_owned()
                } else {
                    crate::text::clip(label.trim(), mailcraft_model::item::MAX_FLAG_LABEL_CHARS)
                },
                start: it.named_time(pim::PSETID_TASK, pim::LID_TASK_START),
                due: it.named_time(pim::PSETID_TASK, pim::LID_TASK_DUE),
                reminder,
            }
        }
        Some(1) => FlagState::Complete { completed: it.time(0x1091) },
        _ => FlagState::None,
    }
}

/// A folder name MailCraft accepts: no control characters or path separators, trimmed, capped.
/// Queues `children` (in file order) unless queued before; past [`MAX_FOLDERS`] queued folders the rest
/// are dropped and `dropped` is set.
fn queue(
    stack: &mut Vec<Pending>,
    queued: &mut HashSet<u32>,
    dropped: &mut bool,
    children: Vec<u32>,
    parent: Option<usize>,
    depth: usize,
) {
    let from = stack.len();
    for nid in children {
        if queued.contains(&nid) {
            continue;
        }
        // `queued` holds the IPM subtree too.
        if queued.len() > MAX_FOLDERS {
            *dropped = true;
            break;
        }
        queued.insert(nid);
        stack.push(Pending { nid, parent, depth });
    }
    // The stack pops from the end: reverse the new entries so they are read in file order.
    if let Some(new) = stack.get_mut(from..) {
        new.reverse();
    }
}

/// A seed unique to this read: the process's random hash keys, the time and a counter.
fn uid_seed(options: &Options) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::AtomicU64;
    static READS: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(options.now.unix);
    h.write_u64(READS.fetch_add(1, Ordering::Relaxed));
    // (The browser build reads no PST files; its clock is not read here.)
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.write_u32(std::process::id());
    h.finish()
}

fn clean_name(name: &str) -> String {
    let n: String =
        name.chars().map(|c| if c.is_control() || c == '/' || c == '\\' { ' ' } else { c }).collect();
    let n = crate::text::clip(n.trim(), 200);
    if n.is_empty() { "Folder".to_owned() } else { n }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_listed_many_times_are_queued_once_and_the_queue_is_capped() {
        let mut queued = HashSet::from([1u32]);
        let mut stack = Vec::new();
        let mut dropped = false;
        // A hierarchy table that repeats one id (and the IPM subtree's) a hundred thousand times.
        queue(&mut stack, &mut queued, &mut dropped, vec![7; 100_000], None, 0);
        queue(&mut stack, &mut queued, &mut dropped, vec![1, 7, 8, 7], Some(0), 1);
        assert_eq!(stack.iter().map(|p| p.nid).collect::<Vec<_>>(), [7, 8]);
        assert!(!dropped);
        // Folders that each list a hundred thousand new ids: the queue never holds more than the cap.
        for round in 0..5u32 {
            let ids: Vec<u32> = (0..100_000u32).map(|i| round * 100_000 + i + 10).collect();
            queue(&mut stack, &mut queued, &mut dropped, ids, Some(1), 2);
        }
        assert!(dropped);
        assert!(stack.len() <= MAX_FOLDERS, "{}", stack.len());
        // File order: the first child of a folder is read first (popped first).
        let mut stack = Vec::new();
        queue(&mut stack, &mut HashSet::new(), &mut false, vec![20, 21, 22], None, 0);
        assert_eq!(stack.pop().map(|p| p.nid), Some(20));
    }
}
