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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CmdError, Session};
use mailcraft_model::account::{AccountSync, CredentialKind};
use mailcraft_model::mailbox::MAX_FOLDER_NAME_CHARS;
use mailcraft_model::{
    Account, AccountId, AccountKind, Appointment, Contact, Folder, FolderId, ItemId, MailItem, Note, Task,
    ViewSettings,
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

/// A data file taken out of the mailbox.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataFileImage {
    pub format: String,
    pub version: u32,
    pub account: Account,
    pub folders: Vec<Folder>,
    pub mail: Vec<MailItem>,
    pub appointments: Vec<Appointment>,
    pub contacts: Vec<Contact>,
    pub tasks: Vec<Task>,
    pub notes: Vec<Note>,
    pub views: Vec<(FolderId, ViewSettings)>,
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

/// The data file of `account` as an image; message sources kept in the store's blobs are inlined, because
/// the blobs go once nothing in the mailbox refers to them.
pub fn image_of(s: &Session, account: AccountId) -> Result<DataFileImage, CmdError> {
    let Some(acct) = s.mailbox.account(account).cloned() else {
        return Err(CmdError::Params(format!("no such data file {}", account.0)));
    };
    let folders: Vec<Folder> = s.mailbox.folders.values().filter(|f| f.account == account).cloned().collect();
    let ids: BTreeSet<FolderId> = folders.iter().map(|f| f.id).collect();
    let mut mail = Vec::new();
    for m in s.mailbox.mail.values().filter(|m| ids.contains(&m.folder)) {
        let mut m = m.clone();
        if m.raw.is_none()
            && m.blob.is_some()
            && let Some(st) = &s.store
        {
            match st.raw_of(&m) {
                Ok(raw) => m.raw = raw,
                Err(e) => {
                    return Err(CmdError::Failed(format!(
                        "cannot read the source of \"{}\": {e}",
                        m.subject
                    )));
                }
            }
        }
        m.blob = None;
        mail.push(m);
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
    let fail = |e: std::io::Error| CmdError::Failed(format!("cannot write the closed data file: {e}"));
    std::fs::create_dir_all(dir).map_err(fail)?;
    let stem = file_stem(name);
    let mut path = dir.join(format!("{stem}.{EXTENSION}"));
    let mut k = 2u32;
    while path.exists() {
        if k > 10_000 {
            return Err(CmdError::Failed("too many closed data files with this name".into()));
        }
        path = dir.join(format!("{stem} ({k}).{EXTENSION}"));
        k = k.saturating_add(1);
    }
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    std::fs::write(&tmp, bytes).map_err(fail)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(e));
    }
    Ok(path)
}

/// Closes data file `account`: saves it as a closed data file, then removes it from the mailbox. Nothing is
/// removed when the save fails. Returns where it went (`None`: kept in memory).
pub fn detach(s: &mut Session, account: AccountId) -> Result<(Option<PathBuf>, usize), CmdError> {
    let image = image_of(s, account)?;
    let name = image.account.display_name.clone();
    let bytes = serde_json::to_vec(&image)
        .map_err(|e| CmdError::Failed(format!("cannot write the closed data file: {e}")))?;
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

/// Reads a closed data file from disk (size-capped).
pub fn read_file(path: &Path) -> Result<Vec<u8>, CmdError> {
    let fail = |e: std::io::Error| CmdError::Failed(format!("cannot read {}: {e}", path.display()));
    let len = std::fs::metadata(path).map_err(fail)?.len();
    if len > MAX_FILE_BYTES {
        return Err(CmdError::Failed(format!("{} is too large to open", path.display())));
    }
    std::fs::read(path).map_err(fail)
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
            continue;
        }
        let parent = parents.get(&f.id).copied().flatten().and_then(|p| map.get(&p).copied());
        let role = f.role.filter(|r| roles.insert(*r));
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
    for mut m in mail {
        let Some(folder) = map.get(&m.folder).copied() else { continue };
        if mail_map.contains_key(&m.id) {
            continue;
        }
        let new = fresh(s);
        mail_map.insert(m.id, new);
        m.id = new;
        m.folder = folder;
        m.remote_id = None;
        m.blob = None;
        new_mail.push(m);
    }
    for mut m in new_mail {
        for v in &mut m.votes {
            v.response = v.response.and_then(|r| mail_map.get(&r).copied());
        }
        s.mailbox.mail.insert(m.id, m);
    }
    for mut x in appointments {
        let Some(folder) = map.get(&x.folder).copied() else { continue };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.appointments.insert(x.id, x);
    }
    for mut x in contacts {
        let Some(folder) = map.get(&x.folder).copied() else { continue };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.contacts.insert(x.id, x);
    }
    for mut x in tasks {
        let Some(folder) = map.get(&x.folder).copied() else { continue };
        x.id = fresh(s);
        x.folder = folder;
        x.from_mail = x.from_mail.and_then(|m| mail_map.get(&m).copied());
        s.mailbox.tasks.insert(x.id, x);
    }
    for mut x in notes {
        let Some(folder) = map.get(&x.folder).copied() else { continue };
        x.id = fresh(s);
        x.folder = folder;
        s.mailbox.notes.insert(x.id, x);
    }
    // Default folders the file lacked, then names unique among siblings.
    s.mailbox.create_default_folders(id);
    dedupe_sibling_names(s, id);
    Ok(id)
}

/// Total size of a data file's messages (bytes, as the messages give it) and its item and folder counts.
pub fn usage(s: &Session, account: AccountId) -> (u64, usize, usize) {
    let ids: BTreeSet<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == account).map(|f| f.id).collect();
    let mut bytes = 0u64;
    let mut items = 0usize;
    for m in s.mailbox.mail.values().filter(|m| ids.contains(&m.folder)) {
        bytes = bytes.saturating_add(m.size);
        items = items.saturating_add(1);
    }
    let others = s.mailbox.appointments.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.contacts.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.tasks.values().filter(|x| ids.contains(&x.folder)).count()
        + s.mailbox.notes.values().filter(|x| ids.contains(&x.folder)).count();
    (bytes, items.saturating_add(others), ids.len())
}
