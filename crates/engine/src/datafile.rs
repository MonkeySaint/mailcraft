//! Closing and reopening data files (Remove on a data file's context menu, File › Open & Export › Open
//! Data File).
//!
//! Every data file of a profile lives in the profile's one store, so closing a data file cannot simply leave
//! a file behind. Instead [`detach`] writes the data file (its account, folders, items and folder views, with
//! every message's source inlined) to a closed data file, `closed-data-files/<name>.mcdata` in the profile
//! folder, and only then takes it out of the mailbox. A session without a store (the sample, tests) keeps the
//! closed data file in memory ([`Session::closed_data_files`]) instead. [`attach`] reads one back with fresh
//! folder and item ids, so a closed data file can also be opened in another profile.
//!
//! A closed data file is untrusted input when it is opened: its size is capped, folder parents that point
//! outside the file, form a cycle or nest too deep are cut, folder names are made valid and unique among
//! siblings, duplicate default-folder roles are dropped, and items whose folder is not in the file are
//! skipped. It always opens as a local data file, even when it was a server account's offline copy.
//!
//! Closing and opening a data file cannot be undone with Undo: the closed data file lives outside the
//! mailbox, so both clear the undo history instead of recording a step.
//!
//! **Standalone data files.** New Data File with a path ([`create_linked`]) writes the new data file to
//! that file at once, and Open Data File from a file outside the profile ([`open_linked`]) reads one; such a
//! data file stays linked to its file (`Account::data_file`). While it is open its items live in the profile
//! like any other data file; Close writes it back to its file ([`close_linked`]) and Data File Properties ›
//! Save writes it there on demand ([`save_linked`]), so the file can be copied, moved and opened in another
//! profile. A data file can be protected by a password: the file is then encrypted ([`seal`]: a key derived
//! with PBKDF2-HMAC-SHA256, RFC 8018, AES-256-CBC and an HMAC-SHA256 tag over the header and the
//! ciphertext, see `mailcraft_smime::seal`; files of the earlier unauthenticated format still open), and the
//! password is kept in the session's secret store, never in the mailbox. The encryption covers the file
//! only: while the data file is open its items live unencrypted in the profile like any other data file's,
//! and its password is remembered in the profile's secret store (the OS keyring where there is one, the
//! profile's secrets file otherwise). Closing it forgets the password, drops its items from the
//! recoverable-items bin, folds the profile's journal into a fresh snapshot so the items leave both, and
//! then deletes the raw message sources the profile stored for them in its `blobs` folder (a source another
//! open data file or a recoverable item shares by content stays); a password typed to close it while none
//! is remembered is checked against the file first (or typed twice when the file is gone).
//!
//! Files are written durably: through a temporary file created exclusively (it never follows a link
//! planted at its name) with a random name next to the target, synced to disk, renamed over the target, and
//! the folder synced after the rename. Reading refuses anything that is not a regular file and stops at
//! [`MAX_FILE_BYTES`]. What [`attach`] had to repair is counted ([`Repairs`]) and reported; a file of its
//! own that needed repairs is first copied to `<file>.bak`, so the first write-back never loses the
//! original. Exit writes a data file of its own back only when it changed, and a file that something else
//! wrote since it was read (another program, or another profile with the same file open) is never
//! overwritten: the data file goes to a copy next to it instead ([`save_linked_as`]).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{CmdError, Session};
use mailcraft_model::account::{AccountSync, CredentialKind};
use mailcraft_model::mailbox::MAX_FOLDER_NAME_CHARS;
use mailcraft_model::{
    Account, AccountId, AccountKind, Appointment, Body, Contact, Folder, FolderId, ItemId, MailItem, Note,
    Task, ViewSettings,
};

/// `format` field of a closed data file.
pub const FORMAT: &str = "mailcraft-data-file";
/// Current version of the format.
pub const VERSION: u32 = 1;
/// File extension of closed data files.
pub const EXTENSION: &str = "mcdata";
/// Folder in the profile folder that holds closed data files.
pub const CLOSED_DIR: &str = "closed-data-files";
/// Largest closed data file read back.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// Most folders and items read from one closed data file.
const MAX_FOLDERS: usize = 100_000;
const MAX_ITEMS: usize = 5_000_000;
/// Most closed data files kept in memory by a session without a store.
const MAX_IN_MEMORY: usize = 64;

/// A mail item of a data file, with its content inline (a data file is self-contained; in the profile the
/// content lives in the body store). The JSON is the item's own fields plus `body` and `raw`, as every data
/// file has had: `body` is always written (MailCraft before storage stage S5 needs it), `raw` is the source
/// as a number array, or null.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DataMail {
    #[serde(flatten)]
    pub item: MailItem,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub raw: Option<Vec<u8>>,
}

impl From<MailItem> for DataMail {
    /// An item with no content of its own.
    fn from(item: MailItem) -> DataMail {
        DataMail { item, body: Body::default(), raw: None }
    }
}

impl std::ops::Deref for DataMail {
    type Target = MailItem;
    fn deref(&self) -> &MailItem {
        &self.item
    }
}

/// A data file taken out of the mailbox.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataFileImage {
    pub format: String,
    pub version: u32,
    pub account: Account,
    #[serde(deserialize_with = "capped_folders")]
    pub folders: Vec<Folder>,
    #[serde(deserialize_with = "capped_items")]
    pub mail: Vec<DataMail>,
    #[serde(deserialize_with = "capped_items")]
    pub appointments: Vec<Appointment>,
    #[serde(deserialize_with = "capped_items")]
    pub contacts: Vec<Contact>,
    #[serde(deserialize_with = "capped_items")]
    pub tasks: Vec<Task>,
    #[serde(deserialize_with = "capped_items")]
    pub notes: Vec<Note>,
    #[serde(deserialize_with = "capped_folders")]
    pub views: Vec<(FolderId, ViewSettings)>,
}

/// A list read from a data file that stops with an error past `cap` entries, so a hostile file cannot make
/// the reader build millions of entries before the counts are checked.
fn capped_vec<'de, D, T>(d: D, cap: usize) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Capped<T> {
        cap: usize,
        kind: std::marker::PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Capped<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "a list of at most {} entries", self.cap)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1024));
            while let Some(x) = seq.next_element()? {
                if out.len() >= self.cap {
                    return Err(serde::de::Error::custom(format!("more than {} entries", self.cap)));
                }
                out.push(x);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Capped { cap, kind: std::marker::PhantomData })
}

fn capped_folders<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    capped_vec(d, MAX_FOLDERS)
}

fn capped_items<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    capped_vec(d, MAX_ITEMS)
}

/// What [`attach`] had to repair in a data file: nothing is lost silently, the counts are reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Repairs {
    /// Items (of any kind) left out because their folder is not in the file.
    pub items_without_folder: usize,
    /// Messages left out because another message in the file has the same id.
    pub duplicate_items: usize,
    /// Folder entries left out because another folder in the file has the same id.
    pub duplicate_folders: usize,
    /// Folders moved to the top level (parent outside the file, a parent loop or nesting too deep).
    pub folders_moved_to_top: usize,
    /// Default-folder roles dropped because another folder already had them.
    pub roles_dropped: usize,
}

impl Repairs {
    /// Items that did not come back (the rest of the repairs lose nothing).
    pub fn items_lost(&self) -> usize {
        self.items_without_folder.saturating_add(self.duplicate_items)
    }

    /// Whether anything was repaired.
    pub fn any(&self) -> bool {
        *self != Repairs::default()
    }

    /// The repairs in a sentence ("2 items whose folder is not in the file were left out; …").
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        let n = |k: usize, one: &str, many: &str| if k == 1 { one.to_owned() } else { format!("{k} {many}") };
        if self.items_without_folder > 0 {
            parts.push(format!(
                "{} left out",
                n(
                    self.items_without_folder,
                    "1 item whose folder is not in the file was",
                    "items whose folder is not in the file were"
                )
            ));
        }
        if self.duplicate_items > 0 {
            parts.push(format!(
                "{} left out",
                n(self.duplicate_items, "1 message listed twice was", "messages listed twice were")
            ));
        }
        if self.duplicate_folders > 0 {
            parts.push(format!(
                "{} merged",
                n(self.duplicate_folders, "1 folder listed twice was", "folders listed twice were")
            ));
        }
        if self.folders_moved_to_top > 0 {
            parts.push(format!(
                "{} moved to the top level",
                n(
                    self.folders_moved_to_top,
                    "1 folder with a broken parent was",
                    "folders with a broken parent were"
                )
            ));
        }
        if self.roles_dropped > 0 {
            parts.push(format!(
                "{} made an ordinary folder",
                n(
                    self.roles_dropped,
                    "1 second copy of a default folder was",
                    "second copies of default folders were"
                )
            ));
        }
        parts.join("; ")
    }
}

/// A closed data file kept in memory (sessions without a store).
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedDataFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A closed data file that can be opened again: its name and, when it is on disk, its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedEntry {
    pub name: String,
    pub path: Option<PathBuf>,
    pub bytes: u64,
}

/// The data file of `account` as an image; message sources and bodies kept in the body store are inlined,
/// because the store's content goes once nothing in the mailbox refers to it.
pub fn image_of(s: &Session, account: AccountId) -> Result<DataFileImage, CmdError> {
    let Some(mut acct) = s.mailbox.account(account).cloned() else {
        return Err(CmdError::Params(format!("no such data file {}", account.0)));
    };
    // Where the file sits on this computer is no part of it (it can be moved).
    acct.data_file = None;
    let folders: Vec<Folder> = s.mailbox.folders.values().filter(|f| f.account == account).cloned().collect();
    let ids: BTreeSet<FolderId> = folders.iter().map(|f| f.id).collect();
    let mut mail = Vec::new();
    for m in s.mailbox.mail.items().filter(|m| ids.contains(&m.folder)) {
        let unreadable =
            |e: CmdError| CmdError::Failed(format!("cannot read the content of \"{}\": {e}", m.subject));
        let raw = s.source_of(m).map_err(unreadable)?;
        let body = s.body_of(m).map_err(unreadable)?.map(|b| (*b).clone()).unwrap_or_default();
        let mut item = m.clone();
        // Keys name content in this profile's store only.
        item.blob = None;
        item.body_key = None;
        mail.push(DataMail { item, body, raw });
    }
    Ok(DataFileImage {
        format: FORMAT.to_owned(),
        version: VERSION,
        account: acct,
        appointments: s.mailbox.appointments.values().filter(|x| ids.contains(&x.folder)).cloned().collect(),
        contacts: s.mailbox.contacts.values().filter(|x| ids.contains(&x.folder)).cloned().collect(),
        tasks: s.mailbox.tasks.values().filter(|x| ids.contains(&x.folder)).cloned().collect(),
        notes: s.mailbox.notes.values().filter(|x| ids.contains(&x.folder)).cloned().collect(),
        views: s
            .mailbox
            .views
            .iter()
            .filter(|(f, _)| ids.contains(f))
            .map(|(f, v)| (*f, v.clone()))
            .collect(),
        folders,
        mail,
    })
}

/// File name for a closed data file called `name`: characters unsafe in file names replaced.
fn file_stem(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim().trim_matches('.').trim();
    if clean.is_empty() { "Data File".to_owned() } else { clean.to_owned() }
}

/// Where closed data files of this session go (`None`: no store, they stay in memory).
pub fn closed_dir(s: &Session) -> Option<PathBuf> {
    s.store.as_ref().map(|st| st.dir.join(CLOSED_DIR))
}

/// Writes `bytes` to a new file in `dir` named after `name` (numbered when taken), through a temporary file
/// and a rename so a failed write leaves nothing half written.
fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, CmdError> {
    write_numbered(dir, name, bytes)
        .map_err(|e| CmdError::Failed(format!("cannot write the closed data file: {e}")))
}

/// [`write_new`] with the error as it came.
fn write_numbered(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stem = file_stem(name);
    let mut path = dir.join(format!("{stem}.{EXTENSION}"));
    let mut k = 2u32;
    while std::fs::symlink_metadata(&path).is_ok() {
        if k > 10_000 {
            return Err(std::io::Error::other("too many data files with this name"));
        }
        path = dir.join(format!("{stem} ({k}).{EXTENSION}"));
        k = k.saturating_add(1);
    }
    write_durable(&path, |f| f.write_all(bytes))?;
    Ok(path)
}

/// A temporary file next to `path`, created exclusively under a random name: an existing file or a link
/// planted at that name is never opened, followed or truncated (another name is tried instead).
fn create_temp(path: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let Some(name) = path.file_name() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file name"));
    };
    let mut last = None;
    for attempt in 0u32..16 {
        let token = mailcraft_smime::random_secret()
            .map(|t| t.chars().take(16).collect::<String>())
            .unwrap_or_else(|_| {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                format!("{}-{nanos}-{attempt}", std::process::id())
            });
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(".{token}.tmp"));
        let tmp = path.with_file_name(tmp_name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary file name")))
}

/// Writes `path` durably: `write` fills a temporary file next to it ([`create_temp`]), which is synced to
/// disk and renamed over `path`; the folder is synced after the rename. A failure leaves the old file (or
/// nothing) and no temporary file. Only once this returns `Ok` is the new content safe from a power loss.
fn write_durable(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let (tmp, mut file) = create_temp(path)?;
    let res = (|| {
        write(&mut file)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    mailcraft_store::journal::sync_dir(dir);
    Ok(())
}

/// A data file larger than `cap` ([`MAX_FILE_BYTES`], the most [`read_file`] reads back) is never written:
/// it could not be opened again (issue #25).
fn check_size(bytes: &[u8], cap: u64) -> Result<(), CmdError> {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > cap {
        return Err(CmdError::Failed(format!(
            "the data file would take {len} bytes, more than the {cap} bytes MailCraft can open again; move \
             some of its mail to another data file first"
        )));
    }
    Ok(())
}

/// Closes data file `account`: saves it as a closed data file, then removes it from the mailbox. Nothing is
/// removed when the save fails. Returns where it went (`None`: kept in memory).
pub fn detach(s: &mut Session, account: AccountId) -> Result<(Option<PathBuf>, usize), CmdError> {
    let image = image_of(s, account)?;
    let name = image.account.display_name.clone();
    let bytes = serde_json::to_vec(&image)
        .map_err(|e| CmdError::Failed(format!("cannot write the closed data file: {e}")))?;
    check_size(&bytes, MAX_FILE_BYTES)?;
    let path = match closed_dir(s) {
        Some(dir) => Some(write_new(&dir, &name, &bytes)?),
        None => {
            // Nothing is thrown away to make room: at the limit, Remove is refused.
            if s.closed_data_files.len() >= MAX_IN_MEMORY {
                return Err(CmdError::Failed(format!(
                    "{MAX_IN_MEMORY} closed data files are already waiting to be opened again; open one first"
                )));
            }
            s.closed_data_files.push(ClosedDataFile { name, bytes });
            None
        }
    };
    crate::sync::forget_account(s, account);
    let folders = crate::accounts::remove_account_data(s, account);
    Ok((path, folders))
}

/// Closed data files that can be opened again: those kept in memory, then those in the profile's
/// closed-data-files folder (by name).
pub fn closed_entries(s: &Session) -> Vec<ClosedEntry> {
    let mut out: Vec<ClosedEntry> = s
        .closed_data_files
        .iter()
        .map(|c| ClosedEntry {
            name: c.name.clone(),
            path: None,
            bytes: u64::try_from(c.bytes.len()).unwrap_or(u64::MAX),
        })
        .collect();
    if let Some(dir) = closed_dir(s)
        && let Ok(rd) = std::fs::read_dir(&dir)
    {
        let mut found: Vec<ClosedEntry> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(EXTENSION))
            .take(1_000)
            .map(|p| {
                let name = p.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
                let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                ClosedEntry { name, path: Some(p), bytes }
            })
            .collect();
        found.sort_by_key(|a| a.name.to_lowercase());
        out.extend(found);
    }
    out
}

/// Reads a data file from disk: only a regular file (a link to one is fine; a device, a pipe, a socket or a
/// folder is refused before it is opened, so nothing blocks or reads without end), and never more than
/// [`MAX_FILE_BYTES`], even when the file grows while it is read.
pub fn read_file(path: &Path) -> Result<Vec<u8>, CmdError> {
    read_capped(path, MAX_FILE_BYTES)
}

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, CmdError> {
    use std::io::Read;
    let fail = |e: std::io::Error| CmdError::Failed(format!("cannot read {}: {e}", path.display()));
    let not_file = || CmdError::Params(format!("{} is not a regular file", path.display()));
    let too_large = || CmdError::Failed(format!("{} is too large to open", path.display()));
    let meta = std::fs::metadata(path).map_err(fail)?;
    if !meta.is_file() {
        return Err(not_file());
    }
    if meta.len() > cap {
        return Err(too_large());
    }
    let file = std::fs::File::open(path).map_err(fail)?;
    // Swapped for something else between the check and the open: refused all the same.
    if !file.metadata().map_err(fail)?.is_file() {
        return Err(not_file());
    }
    let mut buf = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(cap.saturating_add(1)).read_to_end(&mut buf).map_err(fail)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > cap {
        return Err(too_large());
    }
    Ok(buf)
}

/// Parses a closed data file.
pub fn parse(bytes: &[u8]) -> Result<DataFileImage, CmdError> {
    let image: DataFileImage = serde_json::from_slice(bytes)
        .map_err(|e| CmdError::Failed(format!("this is not a MailCraft data file ({e})")))?;
    if image.format != FORMAT {
        return Err(CmdError::Failed("this is not a MailCraft data file".into()));
    }
    if image.version == 0 || image.version > VERSION {
        return Err(CmdError::Failed(format!(
            "this data file was written by a newer MailCraft (format {})",
            image.version
        )));
    }
    let items = image
        .mail
        .len()
        .saturating_add(image.appointments.len())
        .saturating_add(image.contacts.len())
        .saturating_add(image.tasks.len())
        .saturating_add(image.notes.len());
    if image.folders.len() > MAX_FOLDERS || items > MAX_ITEMS {
        return Err(CmdError::Failed("this data file holds too many folders or items".into()));
    }
    Ok(image)
}

/// Deepest folder nesting kept when a data file is opened; deeper folders move to the top level.
const MAX_DEPTH: usize = 64;

/// Folder parents of a data file repaired in one pass: a parent outside the file, a parent loop or nesting
/// deeper than [`MAX_DEPTH`] makes the folder a top-level folder. Returns each folder's parent (old ids).
fn repaired_parents(folders: &[Folder]) -> BTreeMap<FolderId, Option<FolderId>> {
    let mut parent_of: BTreeMap<FolderId, Option<FolderId>> = BTreeMap::new();
    for f in folders {
        parent_of.entry(f.id).or_insert(f.parent);
    }
    let mut out: BTreeMap<FolderId, Option<FolderId>> = BTreeMap::new();
    let mut depth: BTreeMap<FolderId, usize> = BTreeMap::new();
    for f in folders {
        if depth.contains_key(&f.id) {
            continue;
        }
        // Walk up until a folder already placed, a top-level folder or a loop.
        let mut path: Vec<FolderId> = Vec::new();
        let mut on_path: BTreeSet<FolderId> = BTreeSet::new();
        let mut cur = f.id;
        let above: Option<(FolderId, usize)> = loop {
            if let Some(d) = depth.get(&cur).copied() {
                break Some((cur, d));
            }
            if !on_path.insert(cur) {
                // A loop: the folder that closes it becomes a top-level folder.
                break None;
            }
            path.push(cur);
            match parent_of.get(&cur).copied().flatten() {
                Some(p) if parent_of.contains_key(&p) => cur = p,
                _ => break None,
            }
        };
        let mut parent = above.map(|(p, _)| p);
        let mut d = above.map_or(0, |(_, d)| d.saturating_add(1));
        for id in path.iter().rev() {
            if d >= MAX_DEPTH {
                parent = None;
                d = 0;
            }
            out.insert(*id, parent);
            depth.insert(*id, d);
            parent = Some(*id);
            d = d.saturating_add(1);
        }
    }
    out
}

/// A folder name from a data file made valid: control characters dropped, backslashes (the folder path
/// separator) replaced, trimmed and capped.
fn clean_folder_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '\\' { '_' } else { c })
        .take(MAX_FOLDER_NAME_CHARS)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() { "Folder".to_owned() } else { clean.to_owned() }
}

/// Numbers folders of `account` whose name a sibling already has (" (2)"); default folders go first, so
/// they keep theirs.
fn dedupe_sibling_names(s: &mut Session, account: AccountId) {
    let mut order: Vec<(bool, FolderId)> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == account)
        .map(|f| (f.role.is_none(), f.id))
        .collect();
    order.sort();
    let mut taken: BTreeSet<(Option<FolderId>, String)> = BTreeSet::new();
    // Next number to try per (parent, name), so many folders of one name stay linear.
    let mut next: BTreeMap<(Option<FolderId>, String), u32> = BTreeMap::new();
    for (_, id) in order {
        let Some(f) = s.mailbox.folders.get(&id) else { continue };
        let parent = f.parent;
        let base = f.name.clone();
        let mut name = base.clone();
        let counter = next.entry((parent, base.to_lowercase())).or_insert(2);
        let mut k = *counter;
        let limit = k.saturating_add(1_000);
        while taken.contains(&(parent, name.to_lowercase())) && k < limit {
            let suffix = format!(" ({k})");
            let keep = MAX_FOLDER_NAME_CHARS.saturating_sub(suffix.chars().count());
            let stem: String = base.chars().take(keep).collect();
            name = format!("{}{suffix}", stem.trim_end());
            k = k.saturating_add(1);
        }
        *counter = k;
        taken.insert((parent, name.to_lowercase()));
        if name != base
            && let Some(f) = s.mailbox.folders.get_mut(&id)
        {
            f.name = name;
        }
    }
}

/// Adds the data file in `image` to the mailbox with fresh ids; returns its new account id. Its name gets a
/// number when another data file already has it; it never becomes the default account.
///
/// It always opens as a local data file: a server account's mail in it is a copy that no longer belongs to
/// the server (server settings, server folder states and queued server changes are dropped, so opening it
/// never sends anything to a server). Ids inside it (AutoArchive folders, the message a task was made from,
/// voting responses) follow the new ids or are cleared.
pub fn attach(s: &mut Session, image: DataFileImage) -> Result<AccountId, CmdError> {
    attach_report(s, image).map(|(id, _)| id)
}

/// [`attach`], also returning what had to be repaired.
pub fn attach_report(s: &mut Session, image: DataFileImage) -> Result<(AccountId, Repairs), CmdError> {
    let mut fixed = Repairs::default();
    if s.mailbox.accounts.len() >= crate::cmd::file::MAX_ACCOUNTS {
        return Err(CmdError::Failed(format!("at most {} data files", crate::cmd::file::MAX_ACCOUNTS)));
    }
    let DataFileImage { account, folders, mail, appointments, contacts, tasks, notes, views, .. } = image;
    let base: String = account.display_name.chars().filter(|c| !c.is_control()).take(255).collect();
    let base = if base.trim().is_empty() { "Data File".to_owned() } else { base.trim().to_owned() };
    let taken =
        |s: &Session, n: &str| s.mailbox.accounts.iter().any(|a| a.display_name.eq_ignore_ascii_case(n));
    let mut name = base.clone();
    let mut k = 2u32;
    while taken(s, &name) && k < 10_000 {
        name = format!("{base} ({k})");
        k = k.saturating_add(1);
    }
    let id = s.mailbox.add_account(Account {
        display_name: name,
        is_default: false,
        kind: AccountKind::Local,
        incoming: None,
        outgoing: None,
        credential: CredentialKind::default(),
        server_saves_sent: false,
        sync: AccountSync::default(),
        // The caller links the data file to the file it came from, if it stays one.
        data_file: None,
        ..account
    });

    // Folders: fresh ids; parents outside the file, in a loop or too deep are cut; one folder per default
    // role.
    let mut map: BTreeMap<FolderId, FolderId> = BTreeMap::new();
    for f in &folders {
        if let std::collections::btree_map::Entry::Vacant(e) = map.entry(f.id) {
            e.insert(FolderId(s.mailbox.alloc_id()));
        }
    }
    let parents = repaired_parents(&folders);
    let mut roles = BTreeSet::new();
    let mut placed = BTreeSet::new();
    for f in folders {
        let Some(new) = map.get(&f.id).copied() else { continue };
        if !placed.insert(new) {
            fixed.duplicate_folders = fixed.duplicate_folders.saturating_add(1);
            continue;
        }
        let parent = parents.get(&f.id).copied().flatten().and_then(|p| map.get(&p).copied());
        if f.parent.is_some() && parent.is_none() {
            fixed.folders_moved_to_top = fixed.folders_moved_to_top.saturating_add(1);
        }
        let role = f.role.filter(|r| roles.insert(*r));
        if f.role.is_some() && role.is_none() {
            fixed.roles_dropped = fixed.roles_dropped.saturating_add(1);
        }
        let name = clean_folder_name(&f.name);
        let auto_archive = f.auto_archive.map(|mut a| {
            a.target = a.target.and_then(|t| map.get(&t).copied());
            a
        });
        s.mailbox.folders.insert(
            new,
            Folder {
                id: new,
                account: id,
                parent,
                name,
                role,
                favorite_pos: None,
                remote_name: None,
                auto_archive,
                ..f
            },
        );
    }
    for (f, v) in views {
        if let Some(new) = map.get(&f) {
            s.mailbox.views.insert(*new, v);
        }
    }
    let fresh = |s: &mut Session| -> ItemId { ItemId(s.mailbox.alloc_id()) };
    // Mail first, so links to messages (voting responses, tasks made from mail) can follow it.
    let mut mail_map: BTreeMap<ItemId, ItemId> = BTreeMap::new();
    let mut new_mail = Vec::with_capacity(mail.len());
    let mut orphan = 0usize;
    for DataMail { item: mut m, body, raw } in mail {
        let Some(folder) = map.get(&m.folder).copied() else {
            orphan = orphan.saturating_add(1);
            continue;
        };
        if mail_map.contains_key(&m.id) {
            fixed.duplicate_items = fixed.duplicate_items.saturating_add(1);
            continue;
        }
        let new = fresh(s);
        mail_map.insert(m.id, new);
        m.id = new;
        m.folder = folder;
        m.remote_id = None;
        m.blob = None;
        m.body_key = None;
        m.body_evicted = false;
        // The content goes to this profile's body store (the source; a body record only for a body the
        // source does not give).
        let content = mailcraft_store::Content {
            source: raw.filter(|r| !r.is_empty()),
            body: (!body.is_empty()).then_some(body),
        };
        s.store_content(&mut m, &content)?;
        new_mail.push(m);
    }
    for mut m in new_mail {
        for v in &mut m.votes {
            v.response = v.response.and_then(|r| mail_map.get(&r).copied());
        }
        s.mailbox.mail.insert(m.id, m);
    }
    for mut x in appointments {
        let Some(folder) = map.get(&x.folder).copied() else {
            orphan = orphan.saturating_add(1);
            continue;
        };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.appointments.insert(x.id, x);
    }
    for mut x in contacts {
        let Some(folder) = map.get(&x.folder).copied() else {
            orphan = orphan.saturating_add(1);
            continue;
        };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.contacts.insert(x.id, x);
    }
    for mut x in tasks {
        let Some(folder) = map.get(&x.folder).copied() else {
            orphan = orphan.saturating_add(1);
            continue;
        };
        x.id = fresh(s);
        x.folder = folder;
        x.from_mail = x.from_mail.and_then(|m| mail_map.get(&m).copied());
        s.mailbox.tasks.insert(x.id, x);
    }
    for mut x in notes {
        let Some(folder) = map.get(&x.folder).copied() else {
            orphan = orphan.saturating_add(1);
            continue;
        };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.notes.insert(x.id, x);
    }
    // Default folders the file lacked, then names unique among siblings.
    s.mailbox.create_default_folders(id);
    dedupe_sibling_names(s, id);
    fixed.items_without_folder = orphan;
    Ok((id, fixed))
}

// ---------------------------------------------------------------------------------------------------------
// Standalone data files

/// `format` of a data file protected by a password: the data file, encrypted ([`seal`]).
pub const SEALED_FORMAT: &str = "mailcraft-data-file-sealed";
/// Version of the protected format written now. Version 2 adds the HMAC-SHA256 tag over header and
/// ciphertext and the password check value; version 1 files (no tag) still open, and are written as
/// version 2 the next time they are saved.
pub const SEALED_VERSION: u32 = 2;
/// PBKDF2-HMAC-SHA256 rounds a new protected data file's password is stretched with (current OWASP
/// guidance). The file says how many it used, so older files keep their own count.
pub const PASSWORD_ROUNDS: u32 = 600_000;
/// What this build writes (unit tests use fewer rounds to stay fast).
const SEAL_ROUNDS: u32 = if cfg!(test) { 1_000 } else { PASSWORD_ROUNDS };
/// Longest data file password (characters).
pub const MAX_PASSWORD_CHARS: usize = 256;
/// Key derivation of the protected format (RFC 8018).
const KDF: &str = "pbkdf2-sha256";
/// Cipher of the protected format.
const CIPHER: &str = "aes-256-cbc";
/// Authentication of version 2.
const AUTH: &str = "hmac-sha256";

/// What the tag of a protected data file of format `version` is bound to: the format, that version and its
/// algorithms (the round count, salt and IV are added by `mailcraft_smime::seal`). A file is checked against
/// the version it declares, so files of an earlier version keep opening once a later one is written.
fn seal_context(version: u32) -> String {
    format!("{SEALED_FORMAT};version={version};kdf={KDF};cipher={CIPHER};auth={AUTH}")
}

/// A password-protected data file as it is written. `format` comes first: [`is_sealed`] looks for it.
#[derive(Serialize)]
struct SealedOut<'a> {
    format: &'a str,
    version: u32,
    kdf: &'a str,
    rounds: u32,
    /// Base64.
    salt: String,
    cipher: &'a str,
    /// Base64.
    iv: String,
    auth: &'a str,
    /// Base64 of the password check value.
    check: String,
    /// Base64 of the HMAC-SHA256 tag.
    tag: String,
    /// Base64 of the encrypted data file.
    data: String,
}

/// A password-protected data file as it is read: text fields borrow the file's bytes where they can, so the
/// (large) data is not copied before it is decoded.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SealedIn<'a> {
    #[serde(borrow)]
    format: Cow<'a, str>,
    version: u32,
    #[serde(borrow)]
    kdf: Cow<'a, str>,
    rounds: u32,
    #[serde(borrow)]
    salt: Cow<'a, str>,
    #[serde(borrow)]
    cipher: Cow<'a, str>,
    #[serde(borrow)]
    iv: Cow<'a, str>,
    #[serde(borrow)]
    auth: Cow<'a, str>,
    #[serde(borrow)]
    check: Cow<'a, str>,
    #[serde(borrow)]
    tag: Cow<'a, str>,
    #[serde(borrow)]
    data: Cow<'a, str>,
}

/// Key of a standalone data file's password in the session's secret store.
pub fn secret_key(account: AccountId) -> String {
    format!("datafile:{}", account.0)
}

/// Whether `bytes` is a password-protected data file: its JSON starts with a `format` field whose value is
/// exactly [`SEALED_FORMAT`] (only the start is looked at; [`unseal`] checks the rest). A plain data file
/// whose name or contents merely contain that text is not one.
pub fn is_sealed(bytes: &[u8]) -> bool {
    let mut rest = bytes.get(..bytes.len().min(256)).unwrap_or_default();
    let quoted = format!("\"{SEALED_FORMAT}\"");
    let mut eat = |token: &[u8]| -> bool {
        match rest.trim_ascii_start().strip_prefix(token) {
            Some(after) => {
                rest = after;
                true
            }
            None => false,
        }
    };
    eat(b"{") && eat(b"\"format\"") && eat(b":") && eat(quoted.as_bytes()) && (eat(b",") || eat(b"}"))
}

/// A password that can protect a data file: 1 to [`MAX_PASSWORD_CHARS`] characters, no control characters.
pub fn check_password(password: &str) -> Result<(), CmdError> {
    let n = password.chars().count();
    if n == 0 || n > MAX_PASSWORD_CHARS || password.chars().any(char::is_control) {
        return Err(CmdError::Params(format!(
            "a data file password is 1 to {MAX_PASSWORD_CHARS} characters without control characters"
        )));
    }
    Ok(())
}

/// The data file `bytes` encrypted under `password` (the current protected format, [`SEALED_VERSION`]).
pub fn seal(bytes: &[u8], password: &str) -> Result<Vec<u8>, CmdError> {
    use base64::Engine;
    check_password(password)?;
    let fail = |e: String| CmdError::Failed(format!("cannot protect the data file: {e}"));
    let sealed =
        mailcraft_smime::seal::seal(bytes, password, SEAL_ROUNDS, seal_context(SEALED_VERSION).as_bytes())
            .map_err(|e| fail(e.to_string()))?;
    let Some(auth) = &sealed.auth else {
        return Err(fail("no authentication tag".into()));
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let file = SealedOut {
        format: SEALED_FORMAT,
        version: SEALED_VERSION,
        kdf: KDF,
        rounds: sealed.rounds,
        salt: b64.encode(sealed.salt),
        cipher: CIPHER,
        iv: b64.encode(sealed.iv),
        auth: AUTH,
        check: b64.encode(auth.check),
        tag: b64.encode(auth.mac),
        data: b64.encode(&sealed.data),
    };
    serde_json::to_vec(&file).map_err(|e| CmdError::Failed(format!("cannot write the data file: {e}")))
}

fn damaged() -> CmdError {
    CmdError::Failed("this protected data file is damaged".into())
}

/// `t` decoded from base64 into exactly `N` bytes.
fn fixed_bytes<const N: usize>(t: &str) -> Result<[u8; N], CmdError> {
    use base64::Engine;
    let v = base64::engine::general_purpose::STANDARD.decode(t.trim()).map_err(|_| damaged())?;
    <[u8; N]>::try_from(v.as_slice()).map_err(|_| damaged())
}

/// The header of a protected data file checked and decoded (the data left empty), with the file itself.
fn sealed_parts(bytes: &[u8]) -> Result<(SealedIn<'_>, mailcraft_smime::seal::Sealed), CmdError> {
    let file: SealedIn<'_> = serde_json::from_slice(bytes).map_err(|_| damaged())?;
    if file.format != SEALED_FORMAT {
        return Err(CmdError::Failed("this data file is not protected by a password".into()));
    }
    if file.version == 0 || file.version > SEALED_VERSION {
        return Err(CmdError::Failed(format!(
            "this data file was written by a newer MailCraft (format {})",
            file.version
        )));
    }
    if file.kdf != KDF || file.cipher != CIPHER || (file.version >= 2 && file.auth != AUTH) {
        return Err(CmdError::Failed("this data file is protected in a way MailCraft cannot open".into()));
    }
    let auth = if file.version >= 2 {
        Some(mailcraft_smime::seal::Auth { check: fixed_bytes(&file.check)?, mac: fixed_bytes(&file.tag)? })
    } else {
        None
    };
    let sealed = mailcraft_smime::seal::Sealed {
        rounds: file.rounds,
        salt: fixed_bytes(&file.salt)?,
        iv: fixed_bytes(&file.iv)?,
        data: Vec::new(),
        auth,
    };
    Ok((file, sealed))
}

/// An error of `mailcraft_smime::seal` as the user sees it (`authenticated`: the file has a tag, so a wrong
/// password and a damaged file are told apart).
fn seal_error(e: mailcraft_smime::SmimeError, authenticated: bool) -> CmdError {
    match e {
        mailcraft_smime::SmimeError::Password if authenticated => {
            CmdError::Params("the password is wrong".into())
        }
        mailcraft_smime::SmimeError::Password => {
            CmdError::Params("the password is wrong, or the data file is damaged".into())
        }
        mailcraft_smime::SmimeError::Malformed(_) => CmdError::Failed(
            "this protected data file is damaged, or was changed without the password: it was not opened"
                .into(),
        ),
        e => CmdError::Failed(format!("cannot open the protected data file: {e}")),
    }
}

/// The data file inside a password-protected one. The data is decoded into one buffer and decrypted in
/// place; that buffer is what comes back (wiped when it is dropped).
pub fn unseal(bytes: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>, CmdError> {
    use base64::Engine;
    let (file, mut sealed) = sealed_parts(bytes)?;
    sealed.data =
        base64::engine::general_purpose::STANDARD.decode(file.data.trim()).map_err(|_| damaged())?;
    // Checked against the version the file declares, not the one written now.
    let context = seal_context(file.version);
    drop(file);
    let authenticated = sealed.auth.is_some();
    mailcraft_smime::seal::open(sealed, password, context.as_bytes())
        .map_err(|e| seal_error(e, authenticated))
}

/// Whether `password` opens the protected data file `bytes`, without decrypting it: the current format
/// compares the password check value, a version 1 file decrypts only its first two blocks.
pub fn check_sealed_password(bytes: &[u8], password: &str) -> Result<(), CmdError> {
    use base64::Engine;
    let (file, mut sealed) = sealed_parts(bytes)?;
    if sealed.auth.is_none() {
        // 64 base64 characters are 48 bytes: the blocks that hold the marker.
        let head: String = file.data.trim().chars().take(64).collect();
        sealed.data = base64::engine::general_purpose::STANDARD.decode(head).map_err(|_| damaged())?;
    }
    let authenticated = sealed.auth.is_some();
    mailcraft_smime::seal::check_password(&sealed, password).map_err(|e| seal_error(e, authenticated))
}

/// `path` made absolute against the current folder (as it is now), so a data file linked to a relative
/// path is written back to the same file even when MailCraft later runs from elsewhere.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with the data file extension added when it has none.
pub fn with_extension(path: &Path) -> PathBuf {
    if path.extension().is_some() { path.to_path_buf() } else { path.with_extension(EXTENSION) }
}

/// Writes `bytes` to `path` durably ([`write_durable`]): a failed write leaves the old file (or nothing)
/// rather than half a file, and once it returns the file survives a power loss.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CmdError> {
    if path.file_name().is_none() {
        return Err(CmdError::Params(format!("{} is not a file name", path.display())));
    }
    write_durable(path, |f| f.write_all(bytes))
        .map_err(|e| CmdError::Failed(format!("cannot write {}: {e}", path.display())))
}

/// Copies the data file at `path` to a backup next to it before it is first written back:
/// `<file>.bak`, or `<file>.2.bak`… when that is taken (an older backup is never replaced). Returns the
/// backup's path.
fn backup_copy(path: &Path) -> Result<PathBuf, CmdError> {
    let fail = |e: std::io::Error| {
        CmdError::Failed(format!("cannot keep a copy of {} before repairing it: {e}", path.display()))
    };
    let Some(name) = path.file_name() else {
        return Err(CmdError::Params(format!("{} is not a file name", path.display())));
    };
    for k in 1u32..=100 {
        let mut bak = name.to_os_string();
        if k == 1 {
            bak.push(".bak");
        } else {
            bak.push(format!(".{k}.bak"));
        }
        let bak = path.with_file_name(bak);
        if std::fs::symlink_metadata(&bak).is_ok() {
            continue;
        }
        write_durable(&bak, |out| {
            use std::io::Read;
            let src = std::fs::File::open(path)?;
            if !src.metadata()?.is_file() {
                return Err(std::io::Error::other("not a regular file"));
            }
            std::io::copy(&mut src.take(MAX_FILE_BYTES), out).map(|_| ())
        })
        .map_err(fail)?;
        return Ok(bak);
    }
    Err(CmdError::Failed(format!("too many backups of {} already", path.display())))
}

/// The same file as `a` and `b` (resolved when both exist; compared as written otherwise).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The open data file kept in the file at `path`, if any.
pub fn linked_to(s: &Session, path: &Path) -> Option<AccountId> {
    s.mailbox
        .accounts
        .iter()
        .find(|a| a.data_file.as_ref().is_some_and(|l| same_file(Path::new(&l.path), path)))
        .map(|a| a.id)
}

/// What this session last read from or wrote to a standalone data file's file: the file's size and change
/// time on disk then, and a digest of the data file as it stood (`None` when not known, e.g. after a
/// restart). A file whose size or change time moved since was written by something else; an unchanged data
/// file is not written again on exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    disk: Option<(u64, Option<std::time::SystemTime>)>,
    content: Option<u64>,
}

fn disk_stamp(path: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

/// A `Write` that only feeds a hasher, so a digest of serialized data needs no buffer.
struct HashWriter(std::hash::DefaultHasher);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::hash::Hasher;
        self.0.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl HashWriter {
    fn put<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CmdError> {
        serde_json::to_writer(&mut *self, value)
            .map_err(|e| CmdError::Failed(format!("cannot read the data file: {e}")))?;
        // A separator, so neighbouring values cannot run into each other.
        self.bytes(b"\n");
        Ok(())
    }

    fn bytes(&mut self, b: &[u8]) {
        use std::hash::Hasher;
        self.0.write_u64(u64::try_from(b.len()).unwrap_or(u64::MAX));
        self.0.write(b);
    }
}

/// A digest of the data file of `account` as it stands: what [`image_of`] would put in the file, read one
/// item at a time (message sources are hashed as they are read from the store, one at a time), so no copy
/// of the whole data file is built. Two states of the data file with the same contents have the same
/// digest, wherever their message sources are kept.
fn content_digest(s: &Session, account: AccountId) -> Result<u64, CmdError> {
    use std::hash::Hasher;
    let Some(acct) = s.mailbox.account(account) else {
        return Err(CmdError::Params(format!("no such data file {}", account.0)));
    };
    let mut h = HashWriter(std::hash::DefaultHasher::new());
    h.put(&(FORMAT, VERSION))?;
    // Where the file sits is no part of it (as in `image_of`).
    let mut a = acct.clone();
    a.data_file = None;
    h.put(&a)?;
    drop(a);
    let ids: BTreeSet<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == account).map(|f| f.id).collect();
    for f in s.mailbox.folders.values().filter(|f| f.account == account) {
        h.put(f)?;
    }
    for m in s.mailbox.mail.items().filter(|m| ids.contains(&m.folder)) {
        // The item with its content keys: content is keyed by its bytes (storage stage S5), so the keys stand
        // for the source and the body without reading either.
        h.put(m)?;
    }
    h.bytes(b"appointments");
    for x in s.mailbox.appointments.values().filter(|x| ids.contains(&x.folder)) {
        h.put(x)?;
    }
    h.bytes(b"contacts");
    for x in s.mailbox.contacts.values().filter(|x| ids.contains(&x.folder)) {
        h.put(x)?;
    }
    h.bytes(b"tasks");
    for x in s.mailbox.tasks.values().filter(|x| ids.contains(&x.folder)) {
        h.put(x)?;
    }
    h.bytes(b"notes");
    for x in s.mailbox.notes.values().filter(|x| ids.contains(&x.folder)) {
        h.put(x)?;
    }
    h.bytes(b"views");
    for (f, v) in s.mailbox.views.iter().filter(|(f, _)| ids.contains(f)) {
        h.put(&(f, v))?;
    }
    Ok(h.0.finish())
}

/// The data file of `account` serialized as it would be written (before any encryption).
fn image_bytes(s: &Session, account: AccountId) -> Result<Zeroizing<Vec<u8>>, CmdError> {
    let image = image_of(s, account)?;
    serde_json::to_vec(&image)
        .map(Zeroizing::new)
        .map_err(|e| CmdError::Failed(format!("cannot write the data file: {e}")))
}

/// Notes the files of the standalone data files open in this profile as they are on disk now (at start-up),
/// so a file another program or MailCraft instance writes afterwards is not overwritten.
pub(crate) fn note_linked_files(s: &mut Session) {
    let linked: Vec<(AccountId, PathBuf)> = s
        .mailbox
        .accounts
        .iter()
        .filter_map(|a| a.data_file.as_ref().map(|l| (a.id, PathBuf::from(&l.path))))
        .collect();
    for (id, path) in linked {
        s.linked_seen.insert(id, Seen { disk: disk_stamp(&path), content: None });
    }
}

/// Writes standalone data file `account` to its file now (encrypted when it has a password). Returns the
/// path written.
pub fn save_linked(s: &mut Session, account: AccountId) -> Result<PathBuf, CmdError> {
    save_linked_as(s, account, false).map(|(path, _)| path)
}

/// [`save_linked`]; with `only_if_changed`, a data file unchanged since it was last read or written (and
/// whose file is still there) is not written again. Returns the path and whether it was written.
///
/// A file that changed on disk since this session last read or wrote it (another program, or another
/// MailCraft profile with the same file open, wrote it) is never overwritten. When this data file has no
/// changes of its own since then, nothing is written: the file already holds everything, and newer. When it
/// has, it goes to a new file next to it instead (`<name> (copy N).mcdata`), the data file stays linked to
/// that copy, and a notice says so. Nothing is lost on either side. The new link is in the mailbox only:
/// the caller saves the mailbox afterwards (Exit writes these files before it checkpoints the store).
pub(crate) fn save_linked_as(
    s: &mut Session,
    account: AccountId,
    only_if_changed: bool,
) -> Result<(PathBuf, bool), CmdError> {
    let Some(link) = s.mailbox.account(account).and_then(|a| a.data_file.clone()) else {
        return Err(CmdError::Params(
            "this data file is kept with the profile, not in a file of its own".into(),
        ));
    };
    let path = PathBuf::from(&link.path);
    // The digest is taken without building the file; the file is built only when it is written.
    let content = content_digest(s, account)?;
    let seen = s.linked_seen.get(&account).cloned();
    let on_disk = disk_stamp(&path);
    let changed_elsewhere = match (&seen, &on_disk) {
        (Some(seen), Some(now)) => seen.disk.as_ref() != Some(now),
        _ => false,
    };
    let unchanged_here = seen.as_ref().is_some_and(|x| x.content == Some(content));
    if unchanged_here && on_disk.is_some() && (only_if_changed || changed_elsewhere) {
        // Nothing of this side to keep: a newer file written elsewhere stays as it is, and no stale copy of
        // it is made.
        if changed_elsewhere && !only_if_changed {
            s.notify(crate::Notice::Info(format!(
                "{} was changed by another program since MailCraft opened it. This data file has no changes \
                 of its own since then, so the file was left as it is.",
                path.display()
            )));
        }
        return Ok((path, false));
    }
    let plain = image_bytes(s, account)?;
    let sealed;
    let bytes: &[u8] = if link.protected {
        let Some(pw) = s.secrets.get(&secret_key(account)) else {
            return Err(CmdError::Params("the data file's password is needed to write it".into()));
        };
        sealed = seal(&plain, &pw)?;
        &sealed
    } else {
        &plain
    };
    check_size(bytes, MAX_FILE_BYTES)?;
    let written = if changed_elsewhere {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let copy = write_numbered(dir, &format!("{stem} (copy)"), bytes)
            .map_err(|e| CmdError::Failed(format!("cannot write a copy of {}: {e}", path.display())))?;
        let name = s.mailbox.account(account).map(|a| a.display_name.clone()).unwrap_or_default();
        if let Some(l) =
            s.mailbox.accounts.iter_mut().find(|a| a.id == account).and_then(|a| a.data_file.as_mut())
        {
            l.path = copy.display().to_string();
        }
        s.notify(crate::Notice::Info(format!(
            "{} was changed by another program since MailCraft opened it, so it was left as it is. The data \
             file \"{name}\" was saved to {} instead.",
            path.display(),
            copy.display()
        )));
        copy
    } else {
        write_file(&path, bytes)?;
        path
    };
    s.linked_seen.insert(account, Seen { disk: disk_stamp(&written), content: Some(content) });
    Ok((written, true))
}

/// Creates a standalone data file called `name` in the file at `path` (with its default folders, and
/// encrypted under `password` when one is given) and writes it there at once. Nothing is added when the
/// file cannot be written.
pub fn create_linked(
    s: &mut Session,
    name: &str,
    path: &Path,
    password: Option<&str>,
) -> Result<AccountId, CmdError> {
    if let Some(pw) = password {
        check_password(pw)?;
    }
    let path = &absolute(path);
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Local,
        display_name: name.to_owned(),
        is_default: false,
        data_file: Some(mailcraft_model::account::DataFileLink {
            path: path.display().to_string(),
            protected: password.is_some(),
        }),
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    let stored = match password {
        Some(pw) => s.secrets.set(&secret_key(id), pw).map_err(CmdError::Failed),
        None => Ok(()),
    };
    let written = stored.and_then(|()| save_linked(s, id).map(|_| ()));
    if let Err(e) = written {
        let _ = s.secrets.remove(&secret_key(id));
        crate::accounts::remove_account_data(s, id);
        return Err(e);
    }
    Ok(id)
}

/// A standalone data file just opened: its account, what had to be repaired, and the copy of the file as
/// it was when anything was ([`backup_copy`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub account: AccountId,
    pub repairs: Repairs,
    pub backup: Option<PathBuf>,
}

/// Opens the data file in `bytes` (read from `path`, decrypted with `password` when it is protected) and
/// keeps it linked to that file, so closing it writes it back there. A password for a file that is not
/// protected is refused (the caller expected protection the file does not have). When the file needed
/// repairs, a copy of it as it was is kept first; nothing opens when that copy cannot be made.
pub fn open_linked(
    s: &mut Session,
    path: &Path,
    bytes: Vec<u8>,
    password: Option<&str>,
) -> Result<Opened, CmdError> {
    let path = &absolute(path);
    if let Some(other) = linked_to(s, path) {
        let name = s.mailbox.account(other).map(|a| a.display_name.clone()).unwrap_or_default();
        return Err(CmdError::Params(format!("{} is already open as \"{name}\"", path.display())));
    }
    // The file as it was read (just now, by the caller).
    let seen_disk = disk_stamp(path);
    let protected = is_sealed(&bytes);
    let image = match (protected, password) {
        (true, Some(pw)) => {
            let plain = unseal(&bytes, pw)?;
            // Only one full copy of the contents at a time.
            drop(bytes);
            parse(&plain)?
        }
        (true, None) => return Err(CmdError::Params("this data file is protected by a password".into())),
        (false, Some(_)) => {
            return Err(CmdError::Params(format!(
                "{} is not protected by a password, so it cannot be opened or created with one: open it \
                 without a password, or choose another file name",
                path.display()
            )));
        }
        (false, None) => {
            let image = parse(&bytes)?;
            // Only one full copy of the contents at a time.
            drop(bytes);
            image
        }
    };
    let (id, repairs) = attach_report(s, image)?;
    let backup = if repairs.any() {
        match backup_copy(path) {
            Ok(b) => Some(b),
            Err(e) => {
                crate::accounts::remove_account_data(s, id);
                return Err(e);
            }
        }
    } else {
        None
    };
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == id) {
        a.data_file =
            Some(mailcraft_model::account::DataFileLink { path: path.display().to_string(), protected });
    }
    if let (true, Some(pw)) = (protected, password)
        && let Err(e) = s.secrets.set(&secret_key(id), pw)
    {
        log::warn!("could not keep the data file password: {e}");
    }
    // As read: written back on exit only once it changes, and never over a file something else wrote since.
    let content = content_digest(s, id).ok();
    s.linked_seen.insert(id, Seen { disk: seen_disk, content });
    Ok(Opened { account: id, repairs, backup })
}

/// Closes standalone data file `account`: writes it to its file, then takes it out of the mailbox (nothing is
/// removed when the write fails). Returns the file and the number of folders that left.
pub fn close_linked(s: &mut Session, account: AccountId) -> Result<(PathBuf, usize), CmdError> {
    let path = save_linked(s, account)?;
    s.linked_seen.remove(&account);
    let _ = s.secrets.remove(&secret_key(account));
    crate::sync::forget_account(s, account);
    let folders = crate::accounts::remove_account_data(s, account);
    Ok((path, folders))
}

/// Total size of a data file's messages (bytes, as the messages give it) and its item and folder counts.
pub fn usage(s: &Session, account: AccountId) -> (u64, usize, usize) {
    let ids: BTreeSet<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == account).map(|f| f.id).collect();
    let mut bytes = 0u64;
    let mut items = 0usize;
    for m in s.mailbox.mail.items().filter(|m| ids.contains(&m.folder)) {
        bytes = bytes.saturating_add(m.size);
        items = items.saturating_add(1);
    }
    let others = s.mailbox.appointments.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.contacts.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.tasks.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.notes.values().filter(|x| ids.contains(&x.folder)).count();
    (bytes, items.saturating_add(others), ids.len())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn plain_file(name: &str) -> Vec<u8> {
        let image = DataFileImage {
            format: FORMAT.to_owned(),
            version: VERSION,
            account: Account { display_name: name.to_owned(), ..Default::default() },
            ..Default::default()
        };
        serde_json::to_vec(&image).unwrap()
    }

    /// Issue #25: the writer refuses what the reader would refuse, so a data file never becomes one that
    /// cannot be opened again.
    #[test]
    fn data_file_write_and_read_limits_agree() {
        let bytes = plain_file("x");
        let len = u64::try_from(bytes.len()).unwrap();
        assert!(check_size(&bytes, len).is_ok());
        assert!(matches!(check_size(&bytes, len - 1), Err(CmdError::Failed(e)) if e.contains("open again")));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcdata");
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_capped(&path, len).is_ok());
        assert!(read_capped(&path, len - 1).is_err());
    }

    /// Regression: `is_sealed` looked for the format name anywhere in the first 256 bytes, so a plain data
    /// file whose name contained it was taken for a protected one and could never be opened.
    #[test]
    fn only_the_format_field_makes_a_file_protected() {
        let named = plain_file("mailcraft-data-file-sealed notes");
        assert!(String::from_utf8_lossy(&named).contains(SEALED_FORMAT));
        assert!(!is_sealed(&named), "a plain data file named after the format");
        assert_eq!(parse(&named).unwrap().account.display_name, "mailcraft-data-file-sealed notes");
        let sealed = seal(&plain_file("x"), "pw-1").unwrap();
        assert!(is_sealed(&sealed));
        assert!(is_sealed(b" {\n \"format\" : \"mailcraft-data-file-sealed\" , \"version\":2}"));
        assert!(is_sealed(br#"{"format":"mailcraft-data-file-sealed"}"#));
        for not in [
            &br#"{"format":"mailcraft-data-file-sealed-x"}"#[..],
            br#"{"version":2,"format":"mailcraft-data-file-sealed"}"#,
            br#"{"format":"mailcraft-data-file","note":"mailcraft-data-file-sealed"}"#,
            br#"["format","mailcraft-data-file-sealed"]"#,
            b"",
            b"{",
        ] {
            assert!(!is_sealed(not), "{}", String::from_utf8_lossy(not));
        }
    }

    #[test]
    fn new_protected_files_use_600000_rounds_and_the_authenticated_format() {
        const { assert!(PASSWORD_ROUNDS >= 600_000) };
        const { assert!(PASSWORD_ROUNDS <= mailcraft_smime::seal::MAX_ROUNDS, "files written now must open") };
        let bytes = seal(&plain_file("x"), "pw-1").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["version"], SEALED_VERSION);
        assert_eq!(v["auth"], AUTH);
        assert!(v["tag"].as_str().is_some_and(|t| !t.is_empty()));
        assert!(v["check"].as_str().is_some_and(|t| !t.is_empty()));
        assert_eq!(parse(&unseal(&bytes, "pw-1").unwrap()).unwrap().account.display_name, "x");
    }

    /// Regression: the protected format had no MAC; a changed byte of the data or the header opened (or
    /// failed only as "wrong password"). Now it is refused as damaged, and a wrong password is said so.
    #[test]
    fn protected_files_are_authenticated() {
        use base64::Engine;
        let bytes = seal(&plain_file("Private"), "pw-1").unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(unseal(&bytes, "pw-2"), Err(CmdError::Params(e)) if e == "the password is wrong"));
        // A flipped bit in the first ciphertext block (bare CBC would flip the next plain block).
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut data = b64.decode(v["data"].as_str().unwrap()).unwrap();
        data[0] ^= 4;
        v["data"] = serde_json::Value::String(b64.encode(&data));
        let altered = serde_json::to_vec(&v).unwrap();
        assert!(matches!(unseal(&altered, "pw-1"), Err(CmdError::Failed(e)) if e.contains("damaged")));
        // The password still checks out against the header, without decrypting anything.
        check_sealed_password(&altered, "pw-1").unwrap();
        assert!(matches!(check_sealed_password(&altered, "pw-2"), Err(CmdError::Params(_))));
        // Neither a missing tag nor a newer format opens.
        let mut untagged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        untagged["tag"] = serde_json::Value::String(String::new());
        assert!(unseal(&serde_json::to_vec(&untagged).unwrap(), "pw-1").is_err());
        let mut newer: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        newer["version"] = serde_json::json!(3);
        assert!(unseal(&serde_json::to_vec(&newer).unwrap(), "pw-1").is_err());
    }

    /// A file of the earlier, unauthenticated format (version 1) as it was written.
    pub(crate) fn v1_file(plain: &[u8], password: &str) -> Vec<u8> {
        use base64::Engine;
        let s = mailcraft_smime::seal::seal_v1(plain, password, 1_000).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        // In the field order the earlier MailCraft wrote (`format` first).
        format!(
            r#"{{"format":"{SEALED_FORMAT}","version":1,"kdf":"{KDF}","rounds":{},"salt":"{}","cipher":"{CIPHER}","iv":"{}","data":"{}"}}"#,
            s.rounds,
            b64.encode(s.salt),
            b64.encode(s.iv),
            b64.encode(&s.data)
        )
        .into_bytes()
    }

    #[test]
    fn version_1_protected_files_still_open() {
        let old = v1_file(&plain_file("Old archive"), "old-pw");
        assert!(is_sealed(&old));
        assert_eq!(parse(&unseal(&old, "old-pw").unwrap()).unwrap().account.display_name, "Old archive");
        assert!(matches!(unseal(&old, "nope"), Err(CmdError::Params(_))));
        check_sealed_password(&old, "old-pw").unwrap();
        assert!(check_sealed_password(&old, "nope").is_err());
    }

    /// Regression: `read_file` checked the length and then read without a cap, so `/dev/zero` (length 0)
    /// or a link to it was read until memory ran out, and a pipe blocked for good.
    #[cfg(unix)]
    #[test]
    fn reading_refuses_devices_links_to_them_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(read_file(Path::new("/dev/zero")), Err(CmdError::Params(_))));
        let link = dir.path().join("zero.mcdata");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        assert!(matches!(read_file(&link), Err(CmdError::Params(_))));
        assert!(matches!(read_file(dir.path()), Err(CmdError::Params(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        // A link to a regular file is fine; past the cap is refused.
        let real = dir.path().join("real.mcdata");
        std::fs::write(&real, b"0123456789").unwrap();
        let to_real = dir.path().join("to-real.mcdata");
        std::os::unix::fs::symlink(&real, &to_real).unwrap();
        assert_eq!(read_file(&to_real).unwrap(), b"0123456789");
        assert!(read_capped(&real, 9).is_err());
        assert_eq!(read_capped(&real, 10).unwrap().len(), 10);
        assert!(read_file(&dir.path().join("missing.mcdata")).is_err());
    }

    /// Regression: the temporary file was `<name>.tmp`, opened with create + truncate, so a link planted
    /// there made a save overwrite the link's target.
    #[cfg(unix)]
    #[test]
    fn writing_never_follows_a_planted_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let target = dir.path().join("Shared.mcdata");
        std::os::unix::fs::symlink(&victim, dir.path().join("Shared.mcdata.tmp")).unwrap();
        write_file(&target, b"new contents").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert_eq!(std::fs::read(&target).unwrap(), b"new contents");
        assert!(!std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        // Each temporary file is new and exclusive; none is left behind.
        let (a, _fa) = create_temp(&target).unwrap();
        let (b, _fb) = create_temp(&target).unwrap();
        assert_ne!(a, b);
        std::fs::remove_file(a).unwrap();
        std::fs::remove_file(b).unwrap();
        write_file(&target, b"again").unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["Shared.mcdata", "Shared.mcdata.tmp", "victim.txt"]);
        // A failed write leaves the old file and no temporary file.
        let gone = dir.path().join("no-such-folder").join("x.mcdata");
        assert!(write_file(&gone, b"x").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"again");
    }

    /// Closed data files of the profile go through the same durable, exclusive write.
    #[test]
    fn closed_data_files_are_written_whole_and_numbered() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_new(dir.path(), "Archive", b"one").unwrap();
        let b = write_new(dir.path(), "Archive", b"two").unwrap();
        assert_eq!(a.file_name().unwrap(), "Archive.mcdata");
        assert_eq!(b.file_name().unwrap(), "Archive (2).mcdata");
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary file is left");
    }

    /// A hostile file cannot make the reader build more than the cap of entries before the counts are
    /// checked: the list stops with an error at the cap.
    #[test]
    fn lists_past_the_cap_stop_while_reading() {
        let view = serde_json::to_string(&ViewSettings::default()).unwrap();
        let mut text = String::from(r#"{"format":"mailcraft-data-file","version":1,"views":["#);
        for i in 0..=MAX_FOLDERS {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!("[1,{view}]"));
        }
        text.push_str("]}");
        let e = parse(text.as_bytes()).unwrap_err();
        assert!(e.to_string().contains(&format!("more than {MAX_FOLDERS} entries")), "{e}");
        let ok =
            format!(r#"{{"format":"mailcraft-data-file","version":1,"views":[[1,{view}]],"folders":[]}}"#);
        assert_eq!(parse(ok.as_bytes()).unwrap().views.len(), 1);
    }

    #[test]
    fn repairs_describe_themselves() {
        assert!(!Repairs::default().any());
        let r = Repairs { items_without_folder: 2, duplicate_items: 1, ..Default::default() };
        assert!(r.any());
        assert_eq!(r.items_lost(), 3);
        assert_eq!(
            r.describe(),
            "2 items whose folder is not in the file were left out; 1 message listed twice was left out"
        );
    }

    /// Regression: the tag was checked against the context of the version written now, so once the format
    /// moves on, every file of an earlier authenticated version would be refused as damaged. The context is
    /// the declared version's, and version 2's is fixed for good.
    #[test]
    fn the_tag_is_checked_against_the_version_the_file_declares() {
        use base64::Engine;
        assert_eq!(
            seal_context(2),
            "mailcraft-data-file-sealed;version=2;kdf=pbkdf2-sha256;cipher=aes-256-cbc;auth=hmac-sha256"
        );
        let b64 = base64::engine::general_purpose::STANDARD;
        let write = |declared: u32, tagged: u32| -> Vec<u8> {
            let sealed = mailcraft_smime::seal::seal(
                &plain_file("Kept"),
                "pw-1",
                1_000,
                seal_context(tagged).as_bytes(),
            )
            .unwrap();
            let auth = sealed.auth.unwrap();
            serde_json::to_vec(&SealedOut {
                format: SEALED_FORMAT,
                version: declared,
                kdf: KDF,
                rounds: sealed.rounds,
                salt: b64.encode(sealed.salt),
                cipher: CIPHER,
                iv: b64.encode(sealed.iv),
                auth: AUTH,
                check: b64.encode(auth.check),
                tag: b64.encode(auth.mac),
                data: b64.encode(&sealed.data),
            })
            .unwrap()
        };
        let v2 = write(2, 2);
        assert_eq!(parse(&unseal(&v2, "pw-1").unwrap()).unwrap().account.display_name, "Kept");
        // A tag made for another version than the file declares does not check out.
        let e = unseal(&write(2, 7), "pw-1").unwrap_err();
        assert!(matches!(&e, CmdError::Failed(t) if t.contains("damaged")), "{e}");
    }

    /// Regression: the unchanged-content check built a full copy of the data file (every message source read
    /// back from the store, then all of it serialized). The digest is now streamed: it is the same wherever a
    /// message source is kept, and tells different contents apart.
    #[test]
    fn the_content_digest_follows_the_contents_not_where_sources_are_kept() {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(profile.path()).unwrap();
        let file = dir.path().join("Notes.mcdata");
        let id = create_linked(&mut s, "Notes", &file, None).unwrap();
        let inbox = s.mailbox.role_folder(id, mailcraft_model::FolderRole::Inbox).unwrap();
        let mut item = MailItem { folder: inbox, subject: "A".into(), ..Default::default() };
        s.store_source(&mut item, b"Subject: A\r\n\r\nbody one\r\n").unwrap();
        let m = s.mailbox.add_mail(item);
        let first = content_digest(&s, id).unwrap();
        assert_eq!(content_digest(&s, id).unwrap(), first, "the same every time");
        // The source is in the store's blobs; a checkpoint changes nothing.
        s.store.as_mut().unwrap().checkpoint(&s.mailbox).unwrap();
        assert!(s.mailbox.mail.get(m).unwrap().blob.is_some());
        assert_eq!(content_digest(&s, id).unwrap(), first);
        // What Exit compares is that digest: written once with this message, then left alone.
        save_linked(&mut s, id).unwrap();
        let written = std::fs::read(&file).unwrap();
        assert!(!save_linked_as(&mut s, id, true).unwrap().1, "unchanged, not written again");
        assert_eq!(std::fs::read(&file).unwrap(), written);
        // Another source, another digest; the item itself counts as well.
        let mut item = s.mailbox.mail.get(m).cloned().unwrap();
        s.store_source(&mut item, b"Subject: A\r\n\r\nbody two\r\n").unwrap();
        s.mailbox.mail.insert(m, item);
        let other = content_digest(&s, id).unwrap();
        assert_ne!(other, first);
        s.mailbox.mail.get_mut(m).unwrap().subject = "B".into();
        assert_ne!(content_digest(&s, id).unwrap(), other);
        assert!(content_digest(&s, AccountId(9_999)).is_err());
    }
}
