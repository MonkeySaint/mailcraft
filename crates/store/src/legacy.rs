//! Reading message content that files written before storage stage S5 carry inline.
//!
//! Until S5 every serialised [`MailItem`] held its decoded body (`"body":{"format":…}`) and, until its source
//! moved to a blob, the source as a JSON number array (`"raw":[…]`). Snapshots, journal lines, the
//! recoverable-items file and the browser build's saved state written then still do. The item type no longer
//! has those fields, so a plain parse drops them; this module reads them in a second, narrow parse (only when
//! the bytes show the old shape) and moves them into a [`BodyStore`]. Nothing here is written any more.

use std::collections::BTreeMap;

use mailcraft_model::{Body, ItemId, MailItem, Mailbox};
use serde::Deserialize;

use crate::Result;
use crate::bodies::{BodyStore, Content};

/// The bytes only a pre-S5 mail item writes: its inline body.
const BODY_MARK: &[u8] = b"\"body\":{\"format\":";
/// A pre-S5 item still holding its source.
const RAW_MARK: &[u8] = b"\"raw\":[";

/// Whether `json` holds pre-S5 mail content (an inline body or source).
pub fn has_inline(json: &[u8]) -> bool {
    contains(json, BODY_MARK) || contains(json, RAW_MARK)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    let Some(&first) = needle.first() else { return true };
    let mut rest = hay;
    while let Some(pos) = rest.iter().position(|&b| b == first) {
        let tail = rest.get(pos..).unwrap_or_default();
        if tail.starts_with(needle) {
            return true;
        }
        rest = tail.get(1..).unwrap_or_default();
    }
    false
}

/// The content one pre-S5 item carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Inline {
    pub body: Option<Body>,
    pub raw: Option<Vec<u8>>,
}

impl Inline {
    pub fn is_empty(&self) -> bool {
        self.body.as_ref().is_none_or(Body::is_empty) && self.raw.as_ref().is_none_or(Vec::is_empty)
    }
}

/// [`Inline`] plus the item's id (a journal line or a recoverable entry).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct InlineItem {
    id: ItemId,
    body: Option<Body>,
    raw: Option<Vec<u8>>,
}

impl From<InlineItem> for (ItemId, Inline) {
    fn from(i: InlineItem) -> Self {
        (i.id, Inline { body: i.body, raw: i.raw })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MailOnly {
    mail: BTreeMap<ItemId, Inline>,
}

#[derive(Deserialize)]
struct Envelope {
    mailbox: MailOnly,
}

/// Inline content of the mail items of a serialised [`Mailbox`] (`{"mail":{…},…}`).
pub fn mailbox_inline(json: &[u8]) -> Result<BTreeMap<ItemId, Inline>> {
    let m: MailOnly =
        serde_json::from_slice(json).map_err(|e| crate::StoreError::Corrupt(format!("snapshot: {e}")))?;
    Ok(drop_empty(m.mail))
}

/// Inline content of a snapshot envelope (`{"mailcraft_snapshot":1,…,"mailbox":{…}}`, written before
/// storage stage S5), or of a bare mailbox.
pub fn snapshot_inline(json: &[u8]) -> Result<BTreeMap<ItemId, Inline>> {
    if json.starts_with(b"{\"mailcraft_snapshot\"") {
        let e: Envelope =
            serde_json::from_slice(json).map_err(|e| crate::StoreError::Corrupt(format!("snapshot: {e}")))?;
        Ok(drop_empty(e.mailbox.mail))
    } else {
        mailbox_inline(json)
    }
}

/// Inline content of any envelope that keeps the mailbox under `"mailbox"` (the browser build's saved
/// state: `{"format":…,"mailbox":{…}}`).
pub fn envelope_inline(json: &[u8]) -> Result<BTreeMap<ItemId, Inline>> {
    let e: Envelope = serde_json::from_slice(json)
        .map_err(|e| crate::StoreError::Corrupt(format!("saved mailbox: {e}")))?;
    Ok(drop_empty(e.mailbox.mail))
}

fn drop_empty(map: BTreeMap<ItemId, Inline>) -> BTreeMap<ItemId, Inline> {
    map.into_iter().filter(|(_, c)| !c.is_empty()).collect()
}

/// Inline content of one journal line holding a `PutMail` or a `Replace` op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineOp {
    Mail(ItemId, Inline),
    Mailbox(BTreeMap<ItemId, Inline>),
}

#[derive(Deserialize)]
enum OpShape {
    PutMail(InlineItem),
    Replace(MailOnly),
}

/// The inline content of a pre-S5 journal line, if it holds any.
pub fn journal_line_inline(line: &[u8]) -> Option<InlineOp> {
    if !has_inline(line) {
        return None;
    }
    match serde_json::from_slice::<OpShape>(line) {
        Ok(OpShape::PutMail(i)) => {
            let (id, c) = i.into();
            (!c.is_empty()).then_some(InlineOp::Mail(id, c))
        }
        Ok(OpShape::Replace(m)) => Some(InlineOp::Mailbox(drop_empty(m.mail))),
        Err(e) => {
            log::warn!("could not read the message content of an old journal line: {e}");
            None
        }
    }
}

/// [`mailcraft_model::DeletedItem`]'s shape (adjacently tagged).
#[derive(Deserialize)]
#[serde(tag = "kind", content = "item", rename_all = "camelCase")]
enum DeletedShape {
    Mail(InlineItem),
    Appointment(serde::de::IgnoredAny),
    Contact(serde::de::IgnoredAny),
    Task(serde::de::IgnoredAny),
    Note(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct EntryShape {
    item: DeletedShape,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BinShape {
    items: Vec<EntryShape>,
}

/// Inline content of the mail entries of a pre-S5 recoverable-items file, in entry order: (entry index,
/// item id, content).
pub fn recoverable_inline(json: &[u8]) -> Result<Vec<(usize, ItemId, Inline)>> {
    let bin: BinShape = serde_json::from_slice(json)
        .map_err(|e| crate::StoreError::Corrupt(format!("recoverable items: {e}")))?;
    Ok(bin
        .items
        .into_iter()
        .enumerate()
        .filter_map(|(i, e)| match e.item {
            DeletedShape::Mail(m) => {
                let (id, c) = m.into();
                (!c.is_empty()).then_some((i, id, c))
            }
            _ => None,
        })
        .collect())
}

/// Moves `inline` into `bodies` for `item`: the source (kept as the item's `blob`, its expiry date read from
/// it) and the body when it is not the one the source gives (a record under `body_key`). A body only checks
/// against a source the item has in `bodies` already when `inline` carries none.
pub fn store_inline(item: &mut MailItem, inline: Inline, bodies: &mut BodyStore) -> Result<()> {
    let source = match inline.raw.filter(|r| !r.is_empty()) {
        Some(raw) => Some(raw),
        None => match &item.blob {
            Some(_) if inline.body.is_some() => bodies.source_of(item).ok().flatten(),
            _ => None,
        },
    };
    let had_blob = item.blob.clone();
    if let Some(raw) = &source
        && item.expires.is_none()
    {
        item.expires = expiry_in(raw);
    }
    let content = Content { source, body: inline.body };
    bodies.store(item, &content)?;
    if item.blob.is_none() {
        // A source the store already had (and could not read back here) stays referenced.
        item.blob = had_blob;
    }
    Ok(())
}

/// Applies the inline content collected while loading to the items of `mb` that are still there; returns
/// how many items had some.
pub fn apply(mb: &mut Mailbox, pending: BTreeMap<ItemId, Inline>, bodies: &mut BodyStore) -> Result<usize> {
    let mut n = 0usize;
    for (id, inline) in pending {
        let Some(mut item) = mb.mail.get(id).cloned() else { continue };
        store_inline(&mut item, inline, bodies)?;
        mb.mail.insert(id, item);
        n = n.saturating_add(1);
    }
    Ok(n)
}

/// Fills in the expiry date of every item of `mb` with a source and none recorded, reading the start of its
/// source (items loaded from files written before S5, which kept no expiry date). Returns how many got one.
pub fn fill_expiry(mb: &mut Mailbox, bodies: &BodyStore) -> usize {
    let todo: Vec<ItemId> =
        mb.mail.items().filter(|m| m.blob.is_some() && m.expires.is_none()).map(|m| m.id).collect();
    let mut n = 0usize;
    for id in todo {
        let Some(item) = mb.mail.get(id) else { continue };
        let date = match bodies.head_of(item, HEADER_SCAN) {
            Ok(Some(head)) => expiry_in(&head),
            Ok(None) => None,
            Err(e) => {
                log::warn!("cannot read the source of message {}: {e}", id.0);
                None
            }
        };
        if let Some(d) = date {
            mb.mail.update(id, |m| m.expires = Some(d));
            n = n.saturating_add(1);
        }
    }
    n
}

/// Bytes of a source scanned for its header section.
pub const HEADER_SCAN: usize = 64 * 1024;

/// The expiry date (`Expiry-Date` or `Expires`, RFC 4021) of the message whose source is (or starts with)
/// `raw`. Only the header section is read.
pub fn expiry_in(raw: &[u8]) -> Option<mailcraft_model::DateTime> {
    let head = raw.get(..raw.len().min(HEADER_SCAN))?;
    let end = head
        .iter()
        .enumerate()
        .find_map(|(i, &b)| {
            let next = head.get(i.saturating_add(1)..).unwrap_or_default();
            (b == b'\n' && (next.starts_with(b"\n") || next.starts_with(b"\r\n"))).then_some(i)
        })
        .unwrap_or(head.len());
    let text = String::from_utf8_lossy(head.get(..end).unwrap_or(head));
    let mut value: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = value.as_mut() {
            if line.starts_with([' ', '\t']) {
                v.push(' ');
                v.push_str(line.trim());
                continue;
            }
            break;
        }
        if let Some((name, rest)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("expiry-date") || name.eq_ignore_ascii_case("expires") {
                value = Some(rest.trim().to_owned());
            }
        }
    }
    mailcraft_model::DateTime::parse_rfc5322(value?.trim()).ok()
}
