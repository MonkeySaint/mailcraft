//! Turning raw RFC 5322 sources into mailbox items, and the Maildir / MBOX / .eml importers.

use std::fs::{self, File};
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};

use mailcraft_model::{DateTime, FlagState, FolderId, ItemId, MailItem, Mailbox, ResponseState};

use crate::journal::read_capped;
use crate::mbox::{MAX_MESSAGE_BYTES, MboxReader};
use crate::{Result, StoreError, time};

/// Most entries scanned in one Maildir subdirectory.
pub const MAX_MAILDIR_ENTRIES: usize = 1_000_000;
/// Most message source (bytes) one import holds in memory; the rest of the input is left unread.
pub const MAX_IMPORT_BYTES: u64 = 2 << 30;
/// Flag label used for Maildir `F` (the default follow-up flag).
pub const FOLLOW_UP_LABEL: &str = "Follow up";

/// Builds an item from raw bytes. When the MIME parser rejects the message the item is still created from a
/// minimal header scan so no message is ever lost on import.
pub fn item_from_raw(raw: &[u8], folder: FolderId, now: DateTime) -> MailItem {
    let mut item = match mailcraft_mime::parse(raw) {
        Ok(msg) => MailItem::from_message(&msg, raw, now),
        Err(e) => {
            log::debug!("import: MIME parse failed ({e}); using minimal header scan");
            fallback_item(raw, now)
        }
    };
    if item.subject.is_empty() && item.message_id.is_none() {
        // A stub or failed parse: fill the two fields every list needs from our own scan.
        let fb = fallback_item(raw, now);
        item.subject = fb.subject;
        item.message_id = fb.message_id;
    }
    item.folder = folder;
    if item.size == 0 {
        item.size = u64::try_from(raw.len()).unwrap_or(u64::MAX);
    }
    if item.raw.is_none() {
        item.raw = Some(raw.to_vec());
    }
    item
}

/// Unfolded `(name, value)` header fields of `raw` (lossy UTF-8; stops at the first empty line).
pub fn scan_headers(raw: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in raw.split(|&b| b == b'\n').take(10_000) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(line);
        if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            if let Some(last) = out.last_mut()
                && last.1.len() < 64 * 1024
            {
                last.1.push(' ');
                last.1.push_str(text.trim());
            }
            continue;
        }
        if let Some((name, value)) = text.split_once(':') {
            let name = name.trim();
            if !name.is_empty() && !name.contains(' ') {
                out.push((name.to_owned(), value.trim().to_owned()));
            }
        }
    }
    out
}

fn fallback_item(raw: &[u8], now: DateTime) -> MailItem {
    let headers = scan_headers(raw);
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
    let message_id = get("Message-ID")
        .map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_owned())
        .filter(|v| !v.is_empty());
    MailItem {
        subject: get("Subject").unwrap_or_default().chars().take(1024).collect(),
        message_id,
        received: now,
        size: u64::try_from(raw.len()).unwrap_or(u64::MAX),
        ..Default::default()
    }
}

/// Inserts `item` under a fresh id that does not collide with an existing one.
pub fn insert_item(mb: &mut Mailbox, mut item: MailItem) -> Result<ItemId> {
    for _ in 0..1024 {
        if mb.next_id == u64::MAX {
            return Err(StoreError::Corrupt("mailbox id space exhausted".into()));
        }
        let id = ItemId(mb.alloc_id());
        if !mb.mail.contains(id) {
            item.id = id;
            mb.mail.insert(id, item);
            return Ok(id);
        }
    }
    // next_id lags far behind existing ids (a damaged snapshot): jump past them.
    let max = mb.mail.ids().next_back().map_or(0, |k| k.0);
    mb.next_id = max;
    if mb.next_id == u64::MAX {
        return Err(StoreError::Corrupt("mailbox id space exhausted".into()));
    }
    let id = ItemId(mb.alloc_id());
    item.id = id;
    mb.mail.insert(id, item);
    Ok(id)
}

/// Inserts every item or none: on failure the mailbox (items and `next_id`) is left as it was, so a caller
/// that gets an error never holds unjournaled half-imports.
pub fn insert_all(mb: &mut Mailbox, items: Vec<MailItem>) -> Result<Vec<ItemId>> {
    let saved_next = mb.next_id;
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        match insert_item(mb, item) {
            Ok(id) => ids.push(id),
            Err(e) => {
                for id in &ids {
                    mb.mail.remove(*id);
                }
                mb.next_id = saved_next;
                return Err(e);
            }
        }
    }
    Ok(ids)
}

/// Flags from a Maildir file name's `:2,` (or `!2,`) info suffix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaildirFlags {
    pub seen: bool,
    pub flagged: bool,
    pub replied: bool,
    pub passed: bool,
    pub draft: bool,
    pub trashed: bool,
}

impl MaildirFlags {
    pub fn parse(name: &str) -> MaildirFlags {
        let mut f = MaildirFlags::default();
        let info = name.rfind(":2,").or_else(|| name.rfind("!2,")).and_then(|i| name.get(i + 3..));
        for c in info.unwrap_or("").chars().take(64) {
            match c {
                'S' => f.seen = true,
                'F' => f.flagged = true,
                'R' => f.replied = true,
                'P' => f.passed = true,
                'D' => f.draft = true,
                'T' => f.trashed = true,
                _ => {}
            }
        }
        f
    }

    /// Applies the flags to an item (`received` stands in for the unknown response time).
    pub fn apply(&self, item: &mut MailItem) {
        item.read = self.seen;
        item.is_draft = self.draft;
        if self.flagged {
            item.flag = FlagState::Flagged {
                label: FOLLOW_UP_LABEL.to_owned(),
                start: None,
                due: None,
                reminder: None,
            };
        }
        if self.replied {
            item.response = ResponseState::Replied(item.received);
        } else if self.passed {
            item.response = ResponseState::Forwarded(item.received);
        }
    }
}

/// Delivery time from a Maildir unique name (`<secs>.<rest>`), when plausible.
pub fn maildir_delivery_time(name: &str) -> Option<DateTime> {
    let secs = name.split('.').next()?;
    if secs.is_empty() || secs.len() > 12 || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let unix: i64 = secs.parse().ok()?;
    (unix > 0 && unix <= time::MAX_UNIX).then_some(DateTime { unix, offset_min: 0 })
}

fn mtime(path: &Path) -> Option<DateTime> {
    let m = fs::metadata(path).ok()?.modified().ok()?;
    let secs = m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(DateTime { unix: i64::try_from(secs).ok()?.min(time::MAX_UNIX), offset_min: 0 })
}

struct MaildirEntry {
    path: PathBuf,
    name: String,
    in_new: bool,
    delivered: Option<DateTime>,
}

fn list_maildir(dir: &Path) -> Result<Vec<MaildirEntry>> {
    let mut out = Vec::new();
    let mut found_sub = false;
    for (sub, in_new) in [("new", true), ("cur", false)] {
        let subdir = dir.join(sub);
        let rd = match fs::read_dir(&subdir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        found_sub = true;
        for entry in rd.take(MAX_MAILDIR_ENTRIES) {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let delivered = maildir_delivery_time(&name).or_else(|| mtime(&path));
            out.push(MaildirEntry { path, name, in_new, delivered });
        }
    }
    if !found_sub {
        return Err(StoreError::Io(std::io::Error::new(
            ErrorKind::NotFound,
            format!("{} is not a Maildir (no cur/ or new/)", dir.display()),
        )));
    }
    out.sort_by(|a, b| (a.delivered, &a.name).cmp(&(b.delivered, &b.name)));
    Ok(out)
}

/// See [`crate::import_maildir`].
pub fn import_maildir(mb: &mut Mailbox, dir: &Path, folder: FolderId) -> Result<Vec<ItemId>> {
    import_maildir_capped(mb, dir, folder, MAX_IMPORT_BYTES)
}

/// [`import_maildir`] holding at most `max_bytes` of message source.
pub fn import_maildir_capped(
    mb: &mut Mailbox,
    dir: &Path,
    folder: FolderId,
    max_bytes: u64,
) -> Result<Vec<ItemId>> {
    let now = time::now();
    let mut items = Vec::new();
    let mut held = 0u64;
    for entry in list_maildir(dir)? {
        let flags = if entry.in_new { MaildirFlags::default() } else { MaildirFlags::parse(&entry.name) };
        if flags.trashed {
            log::debug!("maildir: skipping trashed {}", entry.name);
            continue;
        }
        let raw = match read_capped(&entry.path, cap_u64()) {
            Ok(Some(raw)) => raw,
            Ok(None) => continue, // removed while scanning
            Err(e) => {
                // One unreadable or oversized message must not abort the whole import.
                log::warn!("maildir: skipping {}: {e}", entry.name);
                continue;
            }
        };
        if !take_budget(&mut held, raw.len(), max_bytes) {
            log::warn!("maildir: stopped after {} messages ({held} bytes, the import cap)", items.len());
            break;
        }
        let mut item = item_from_raw(&raw, folder, now);
        if let Some(t) = entry.delivered {
            item.received = t;
        }
        flags.apply(&mut item);
        items.push(item);
    }
    insert_all(mb, items)
}

/// Adds `len` to `held` unless that would pass `max`.
fn take_budget(held: &mut u64, len: usize, max: u64) -> bool {
    let next = held.saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
    if next > max {
        return false;
    }
    *held = next;
    true
}

fn cap_u64() -> u64 {
    u64::try_from(MAX_MESSAGE_BYTES).unwrap_or(u64::MAX)
}

/// See [`crate::import_mbox`].
pub fn import_mbox(mb: &mut Mailbox, file: &Path, folder: FolderId) -> Result<Vec<ItemId>> {
    import_mbox_capped(mb, file, folder, MAX_IMPORT_BYTES)
}

/// [`import_mbox`] holding at most `max_bytes` of message source.
pub fn import_mbox_capped(
    mb: &mut Mailbox,
    file: &Path,
    folder: FolderId,
    max_bytes: u64,
) -> Result<Vec<ItemId>> {
    let now = time::now();
    let mut held = 0u64;
    let mut reader = MboxReader::new(BufReader::new(File::open(file)?));
    let mut items = Vec::new();
    let mut skipped = 0usize;
    // Nothing is inserted until the whole file has been read, so a read error leaves `mb` untouched.
    while let Some(msg) = reader.next_message()? {
        if msg.oversized {
            skipped += 1;
            continue;
        }
        if !take_budget(&mut held, msg.data.len(), max_bytes) {
            log::warn!("mbox: stopped after {} messages ({held} bytes, the import cap)", items.len());
            break;
        }
        let (_, delivered) = msg.envelope();
        let mut item = item_from_raw(&msg.data, folder, now);
        if let Some(t) = delivered {
            item.received = t;
        }
        // MBOX carries no read state of its own; honour the common Status / X-Status headers.
        apply_status_headers(&msg.data, &mut item);
        items.push(item);
    }
    if skipped > 0 {
        log::warn!("mbox: skipped {skipped} message(s) larger than {MAX_MESSAGE_BYTES} bytes");
    }
    if reader.hit_message_limit() {
        log::warn!("mbox: stopped after {} messages", crate::mbox::MAX_MESSAGES);
    }
    insert_all(mb, items)
}

/// `Status: RO` (R = read) and `X-Status: AF` (A = answered, F = flagged, D = deleted ignored, T = draft).
pub fn apply_status_headers(raw: &[u8], item: &mut MailItem) {
    for (name, value) in scan_headers(raw) {
        if name.eq_ignore_ascii_case("Status") {
            if value.contains('R') {
                item.read = true;
            }
        } else if name.eq_ignore_ascii_case("X-Status") {
            let flags = MaildirFlags {
                seen: item.read,
                flagged: value.contains('F'),
                replied: value.contains('A'),
                draft: value.contains('T'),
                ..Default::default()
            };
            flags.apply(item);
        }
    }
}

/// Reads one `.eml` file (capped) and builds its item.
pub fn read_eml(path: &Path, folder: FolderId) -> Result<MailItem> {
    let raw = read_capped(path, cap_u64())?.ok_or_else(|| {
        StoreError::Io(std::io::Error::new(ErrorKind::NotFound, path.display().to_string()))
    })?;
    // `received` falls back to `now` inside the item builder; use a sentinel no header date can produce to
    // tell "no Date / Received header" apart and take the file's modification time instead.
    let no_date = DateTime { unix: i64::MIN, offset_min: 0 };
    let mut item = item_from_raw(&raw, folder, no_date);
    if item.received == no_date {
        item.received = mtime(path).unwrap_or_else(time::now);
    }
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maildir_flag_parsing() {
        let f = MaildirFlags::parse("1791433800.M1P2.host:2,FRS");
        assert!(f.seen && f.flagged && f.replied && !f.draft && !f.trashed);
        assert_eq!(
            MaildirFlags::parse("x!2,DT"),
            MaildirFlags { draft: true, trashed: true, ..Default::default() }
        );
        assert_eq!(MaildirFlags::parse("noinfo"), MaildirFlags::default());
        assert_eq!(MaildirFlags::parse("a:2,abc"), MaildirFlags::default());
        assert_eq!(MaildirFlags::parse("a:2,"), MaildirFlags::default());
    }

    #[test]
    fn delivery_time_from_name() {
        assert_eq!(maildir_delivery_time("1791433800.M1.host").map(|d| d.unix), Some(1_791_433_800));
        assert_eq!(maildir_delivery_time("abc.host"), None);
        assert_eq!(maildir_delivery_time("99999999999999999999.x"), None);
        assert_eq!(maildir_delivery_time("0.x"), None);
    }

    #[test]
    fn header_scan_unfolds() {
        let h = scan_headers(b"Subject: hello\r\n  world\r\nMessage-ID: <a@b>\r\n\r\nSubject: body\r\n");
        assert_eq!(h, vec![("Subject".into(), "hello world".into()), ("Message-ID".into(), "<a@b>".into())]);
        let item = fallback_item(b"Subject: x\nMessage-Id: <id@h>\n\nbody", DateTime::default());
        assert_eq!(item.subject, "x");
        assert_eq!(item.message_id.as_deref(), Some("id@h"));
    }

    #[test]
    fn insert_never_overwrites() {
        let mut mb = Mailbox::default();
        mb.mail.insert(ItemId(1), MailItem { subject: "keep".into(), ..Default::default() });
        mb.mail.insert(ItemId(5000), MailItem { subject: "keep2".into(), ..Default::default() });
        let id = insert_item(&mut mb, MailItem::default()).unwrap_or_default();
        assert_ne!(id, ItemId(1));
        assert_eq!(mb.mail.get(ItemId(1)).map(|m| m.subject.as_str()), Some("keep"));
        mb.next_id = u64::MAX;
        assert!(insert_item(&mut mb, MailItem::default()).is_err());
    }

    #[test]
    fn insert_all_is_all_or_nothing() {
        let mut mb = Mailbox { next_id: u64::MAX - 2, ..Default::default() };
        let before = mb.clone();
        let items = vec![MailItem::default(), MailItem::default(), MailItem::default()];
        assert!(insert_all(&mut mb, items).is_err());
        assert_eq!(mb, before);
        let ids = insert_all(&mut mb, vec![MailItem::default()]).unwrap_or_default();
        assert_eq!(ids, vec![ItemId(u64::MAX - 1)]);
    }

    #[test]
    fn status_headers() {
        let mut item = MailItem::default();
        apply_status_headers(b"Status: RO\nX-Status: AF\n\nbody", &mut item);
        assert!(item.read && item.is_flagged());
        assert!(matches!(item.response, ResponseState::Replied(_)));
    }

    proptest::proptest! {
        #[test]
        fn hostile_raw_never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..1024)) {
            let item = item_from_raw(&data, FolderId(3), DateTime::default());
            proptest::prop_assert_eq!(item.folder, FolderId(3));
            proptest::prop_assert_eq!(item.raw.as_deref(), Some(data.as_slice()));
            let _ = MaildirFlags::parse(&String::from_utf8_lossy(&data));
            let _ = maildir_delivery_time(&String::from_utf8_lossy(&data));
        }
    }
}
