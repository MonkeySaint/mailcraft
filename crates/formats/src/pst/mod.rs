//! PST files ([MS-PST]): import, and opening one as a data file. Read only.
//!
//! The node database, B-trees, blocks (with both block encodings), heaps, property and table contexts and
//! the messaging objects are read with Microsoft's MIT-licensed `outlook-pst` crate (the [MS-PST]
//! reference reader). This module walks what it returns and maps it to MailCraft items:
//! - the folder tree below the IPM subtree, each folder's kind from its container class;
//! - mail: the properties go through the same mapping as `.msg` items ([`crate::msg`]), so headers, plain,
//!   HTML and compressed-RTF bodies, recipients, attachments (by value and embedded messages), read state,
//!   importance, sensitivity, categories and dates come across, and an RFC 5322 source is built for the
//!   store; follow-up flags from PidTagFlagStatus;
//! - contacts ([MS-OXOCNTC]), appointments with their recurrence pattern ([MS-OXOCAL] §2.2.1.44, see
//!   [`pim`]), tasks ([MS-OXOTASK]) and notes ([MS-OXONOTE]).
//!
//! Writing PST files is not supported (the crate reads only); export goes to MailCraft data files, EML,
//! MBOX, iCalendar and vCard.
//!
//! **Hostile files.** A PST file is untrusted input. Everything the crate does runs under
//! `std::panic::catch_unwind`, per folder and per item: a panic skips that folder or item, reopens the file
//! (the crate's lock and caches may be left in a bad state) and goes on. Reads go through [`Guarded`],
//! which stops at a read budget (a damaged B-tree that loops reads forever otherwise) and on cancel. Folders
//! are capped in number and depth (a folder that lists an ancestor as its child is cut), items in number,
//! attachments in count and size (the attachment table's size column is checked before an attachment is
//! read), and every item's rebuilt source is capped like a downloaded message. Password-protected files are
//! refused with a clear error (the password is only a check made by the program that wrote the file, not
//! encryption, so honouring it is up to the reader).
//!
//! The walk reports progress and takes a cancel request through a shared [`Control`], and hands what it
//! reads to a [`Sink`] one folder or item at a time, so the caller decides how much to hold.

mod pim;
mod walk;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mailcraft_model::{Appointment, Contact, DateTime, FolderKind, FolderRole, MailItem, Note, Task};

pub use pim::recurrence_rule;

use crate::{FormatError, Result};

/// `dwMagic` of a PST header ("!BDN").
pub const MAGIC: [u8; 4] = *b"!BDN";
/// Largest PST file opened (Unicode PST files reach 50 GB with default settings).
pub const MAX_PST_BYTES: u64 = 128 << 30;
/// Most folders read from one file.
pub const MAX_FOLDERS: usize = 100_000;
/// Deepest folder nesting followed.
pub const MAX_FOLDER_DEPTH: usize = 64;
/// Most items read from one file.
pub const MAX_ITEMS: usize = 5_000_000;
/// Most attachment bytes read per item (larger ones are skipped and reported); the rebuilt source then
/// stays below the size of the largest message MailCraft parses.
pub const MAX_ATTACHMENT_BYTES: u64 = 40 << 20;
/// Most entries kept in [`Report::skipped`] (the count goes on).
pub const MAX_SKIPPED_LISTED: usize = 1_000;

/// What a PST file is, from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    /// 32-bit file (format version 14/15).
    Ansi,
    /// 64-bit file (format version 23).
    Unicode,
}

/// The header's format: `None` when `head` is not a PST file; an error for a PST format MailCraft cannot
/// read (4K-page files are written by server caches, not by the desktop program).
pub fn sniff(head: &[u8]) -> Option<std::result::Result<Version, FormatError>> {
    if head.get(0..4) != Some(MAGIC.as_slice()) {
        return None;
    }
    if head.get(8..10) != Some(b"SM".as_slice()) {
        // wMagicClient "SM" for PST ("SO" is an offline cache file).
        return Some(Err(unsupported(
            "this is not a PST data file (an offline cache file cannot be imported)",
        )));
    }
    let ver = head.get(10..12).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(u16::from_le_bytes);
    Some(match ver {
        Some(14 | 15) => Ok(Version::Ansi),
        Some(23) => Ok(Version::Unicode),
        Some(v) => Err(unsupported(format!("PST format version {v} is not supported"))),
        None => Err(malformed("the header is cut short")),
    })
}

fn unsupported(msg: impl Into<String>) -> FormatError {
    FormatError::Unsupported { format: "pst", msg: msg.into() }
}

fn malformed(msg: impl Into<String>) -> FormatError {
    FormatError::malformed("pst", msg)
}

/// The error for a password-protected file.
pub fn password_error() -> FormatError {
    unsupported(
        "this PST file is protected by a password; remove the password in the program that created it, \
         then import it again",
    )
}

/// Where the file comes from.
#[derive(Clone, Debug)]
pub enum Source {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// Progress and cancel, shared between the walk and whoever shows it.
#[derive(Debug, Default)]
pub struct Control {
    cancel: AtomicBool,
    /// Items read so far (imported or skipped).
    pub items_done: AtomicU64,
    /// Items the folders announce (their content counts); grows as folders are read.
    pub items_total: AtomicU64,
    /// Folders read so far.
    pub folders_done: AtomicU64,
    /// Bytes read from the file so far.
    pub bytes_read: AtomicU64,
    /// Size of the file.
    pub file_bytes: AtomicU64,
}

impl Control {
    /// Asks the walk to stop after the item it is reading.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// Whether a stop was asked for.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn bump(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }
}

/// Options of one read.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// "Now", for items without dates.
    pub now: DateTime,
    /// Local UTC offset (minutes) given to appointment and task times.
    pub local_offset_min: i32,
    /// Most items read (the rest is reported as not read).
    pub max_items: usize,
    /// Most bytes read from the file, in multiples of its size (the B-trees are read many times).
    pub read_factor: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options { now: DateTime::from_unix(0, 0), local_offset_min: 0, max_items: MAX_ITEMS, read_factor: 24 }
    }
}

/// A folder of the file.
#[derive(Clone, Debug, PartialEq)]
pub struct PstFolder {
    /// Position among the folders handed to the sink (0, 1, 2, …).
    pub index: usize,
    /// The parent's index; `None` for a folder directly below the top of the file.
    pub parent: Option<usize>,
    pub name: String,
    /// PidTagContainerClass ("IPF.Note", …), empty when the folder has none.
    pub class: String,
    pub kind: FolderKind,
    /// The default folder this one is (by the store's entry ids and the usual names), for folders directly
    /// below the top.
    pub role: Option<FolderRole>,
    /// Items the folder says it holds.
    pub count: u32,
}

/// An item of the file. Folder fields are left at their default; the caller files the item.
#[derive(Clone, Debug, PartialEq)]
pub enum PstItem {
    /// A message with the RFC 5322 source built for it and its body.
    Mail(Box<(MailItem, mailcraft_store::Content)>),
    Appointment(Box<Appointment>),
    Contact(Box<Contact>),
    Task(Box<Task>),
    Note(Box<Note>),
}

/// Receives what the walk reads. Returning false stops the walk.
pub trait Sink {
    fn folder(&mut self, folder: PstFolder) -> bool;
    fn item(&mut self, folder: usize, item: PstItem) -> bool;
}

/// Something that was not imported, or imported with less than the file had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// Folder path inside the file ("Inbox/Projects").
    pub folder: String,
    /// The item (subject or message class), empty for a whole folder.
    pub item: String,
    pub reason: String,
}

/// What one read did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The file's display name.
    pub name: String,
    pub folders: usize,
    pub mail: usize,
    pub appointments: usize,
    pub contacts: usize,
    pub tasks: usize,
    pub notes: usize,
    /// The first [`MAX_SKIPPED_LISTED`] things skipped or imported in part.
    pub skipped: Vec<Skipped>,
    /// How many were skipped or imported in part in all.
    pub skipped_count: usize,
    /// The read stopped early: on cancel, at the item cap, or because the sink asked.
    pub stopped: Option<String>,
}

impl Report {
    /// Items imported.
    pub fn imported(&self) -> usize {
        self.mail
            .saturating_add(self.appointments)
            .saturating_add(self.contacts)
            .saturating_add(self.tasks)
            .saturating_add(self.notes)
    }

    fn skip(&mut self, folder: &str, item: &str, reason: impl Into<String>) {
        self.skipped_count = self.skipped_count.saturating_add(1);
        if self.skipped.len() < MAX_SKIPPED_LISTED {
            self.skipped.push(Skipped {
                folder: folder.to_owned(),
                item: crate::text::clip(item, 200),
                reason: reason.into(),
            });
        }
    }
}

/// Reads a PST file, handing its folders and items to `sink`. Errors only when the file cannot be read at
/// all (not a PST file, protected by a password, the header or the store damaged); damage further in is
/// skipped and listed in the report.
pub fn read(
    source: Source,
    options: &Options,
    control: &std::sync::Arc<Control>,
    sink: &mut dyn Sink,
) -> Result<Report> {
    match walk::read(source, options, control, sink) {
        // A stop asked for before the file was even open is a stop, not a damaged file.
        Err(_) if control.cancelled() => {
            Ok(Report { stopped: Some("cancelled".into()), ..Default::default() })
        }
        r => r,
    }
}

/// A sink that keeps everything (tests, small files).
#[derive(Debug, Default)]
pub struct Collect {
    pub folders: Vec<PstFolder>,
    pub items: Vec<(usize, PstItem)>,
}

impl Sink for Collect {
    fn folder(&mut self, folder: PstFolder) -> bool {
        self.folders.push(folder);
        true
    }
    fn item(&mut self, folder: usize, item: PstItem) -> bool {
        self.items.push((folder, item));
        true
    }
}

/// The folder kind for a container class.
pub fn kind_for_class(class: &str) -> FolderKind {
    let c = class.to_ascii_lowercase();
    if c.starts_with("ipf.appointment") {
        FolderKind::Calendar
    } else if c.starts_with("ipf.contact") {
        FolderKind::Contacts
    } else if c.starts_with("ipf.task") {
        FolderKind::Tasks
    } else if c.starts_with("ipf.stickynote") {
        FolderKind::Notes
    } else if c.starts_with("ipf.journal") {
        FolderKind::Journal
    } else {
        FolderKind::Mail
    }
}

/// The default folder a folder directly below the top of a file is, by its usual English name.
pub fn role_for_name(name: &str, kind: FolderKind) -> Option<FolderRole> {
    let n = name.trim().to_ascii_lowercase();
    let role = match n.as_str() {
        "inbox" => FolderRole::Inbox,
        "drafts" => FolderRole::Drafts,
        "sent items" | "sent" => FolderRole::SentItems,
        "deleted items" | "trash" => FolderRole::DeletedItems,
        "archive" => FolderRole::Archive,
        "junk email" | "junk e-mail" | "junk" => FolderRole::JunkEmail,
        "outbox" => FolderRole::Outbox,
        "conversation history" => FolderRole::ConversationHistory,
        "rss feeds" | "rss subscriptions" => FolderRole::RssFeeds,
        "sync issues" => FolderRole::SyncIssues,
        "calendar" => FolderRole::Calendar,
        "contacts" => FolderRole::Contacts,
        "journal" => FolderRole::Journal,
        "notes" => FolderRole::Notes,
        "tasks" => FolderRole::Tasks,
        _ => return None,
    };
    (role.kind() == kind).then_some(role)
}

/// Reads through to the file, stopping at a byte budget and on cancel.
pub(crate) struct Guarded<R> {
    inner: R,
    budget: u64,
    control: std::sync::Arc<Control>,
}

impl<R> Guarded<R> {
    pub(crate) fn new(inner: R, budget: u64, control: std::sync::Arc<Control>) -> Self {
        Guarded { inner, budget, control }
    }
}

impl<R: std::io::Read> std::io::Read for Guarded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.control.cancelled() {
            return Err(std::io::Error::other("the import was cancelled"));
        }
        if self.budget == 0 {
            return Err(std::io::Error::other("read budget exhausted (damaged B-tree?)"));
        }
        let want = usize::try_from(self.budget).unwrap_or(usize::MAX).min(buf.len());
        let n = self.inner.read(buf.get_mut(..want).unwrap_or(&mut []))?;
        let n64 = u64::try_from(n).unwrap_or(u64::MAX);
        self.budget = self.budget.saturating_sub(n64);
        Control::bump(&self.control.bytes_read, n64);
        Ok(n)
    }
}

impl<R: std::io::Seek> std::io::Seek for Guarded<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
