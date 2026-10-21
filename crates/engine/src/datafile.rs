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
//! profile. A data file can be protected by a password: the file is then encrypted ([`seal`]: PBES2,
//! PBKDF2-HMAC-SHA256 and AES-256-CBC, RFC 8018), and the password is kept in the session's secret store,
//! never in the mailbox. The encryption covers the file only: while the data file is open its items live
//! unencrypted in the profile like any other data file's, and its password is remembered in the profile's
//! secret store (the OS keyring where there is one, the profile's secrets file otherwise). Closing it forgets
//! the password and folds the profile's journal into a fresh snapshot so the items leave both (the raw
//! message sources the profile stored for them stay in its `blobs` folder, shared by content with other
//! data files, so they are not deleted here); a password
//! typed to close it while none is remembered is checked against the file first (or typed twice when the
//! file is gone).

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
    let Some(mut acct) = s.mailbox.account(account).cloned() else {
        return Err(CmdError::Params(format!("no such data file {}", account.0)));
    };
    // Where the file sits on this computer is no part of it (it can be moved).
    acct.data_file = None;
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

// ---------------------------------------------------------------------------------------------------------
// Standalone data files

/// `format` of a data file protected by a password: the data file, encrypted ([`seal`]).
pub const SEALED_FORMAT: &str = "mailcraft-data-file-sealed";
/// PBKDF2 rounds a data file's password is stretched with (unit tests use fewer to stay fast; the file says
/// how many it used).
const SEAL_ROUNDS: u32 = if cfg!(test) { 1_000 } else { 200_000 };
/// Longest data file password (characters).
pub const MAX_PASSWORD_CHARS: usize = 256;

/// A password-protected data file on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SealedFile {
    format: String,
    version: u32,
    /// "pbkdf2-sha256" (RFC 8018).
    kdf: String,
    rounds: u32,
    /// Base64.
    salt: String,
    /// "aes-256-cbc".
    cipher: String,
    /// Base64.
    iv: String,
    /// Base64 of the encrypted data file.
    data: String,
}

/// Key of a standalone data file's password in the session's secret store.
pub fn secret_key(account: AccountId) -> String {
    format!("datafile:{}", account.0)
}

/// Whether `bytes` is a password-protected data file (only the start is looked at; [`unseal`] checks the
/// rest).
pub fn is_sealed(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(256)).unwrap_or_default();
    String::from_utf8_lossy(head).contains(SEALED_FORMAT)
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

/// The data file `bytes` encrypted under `password`.
pub fn seal(bytes: &[u8], password: &str) -> Result<Vec<u8>, CmdError> {
    use base64::Engine;
    check_password(password)?;
    let sealed = mailcraft_smime::seal::seal(bytes, password, SEAL_ROUNDS)
        .map_err(|e| CmdError::Failed(format!("cannot protect the data file: {e}")))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let file = SealedFile {
        format: SEALED_FORMAT.to_owned(),
        version: VERSION,
        kdf: "pbkdf2-sha256".to_owned(),
        rounds: sealed.rounds,
        salt: b64.encode(sealed.salt),
        cipher: "aes-256-cbc".to_owned(),
        iv: b64.encode(sealed.iv),
        data: b64.encode(&sealed.data),
    };
    serde_json::to_vec(&file).map_err(|e| CmdError::Failed(format!("cannot write the data file: {e}")))
}

/// The data file inside a password-protected one.
pub fn unseal(bytes: &[u8], password: &str) -> Result<Vec<u8>, CmdError> {
    use base64::Engine;
    let damaged = || CmdError::Failed("this protected data file is damaged".into());
    let file: SealedFile = serde_json::from_slice(bytes).map_err(|_| damaged())?;
    if file.format != SEALED_FORMAT || file.kdf != "pbkdf2-sha256" || file.cipher != "aes-256-cbc" {
        return Err(CmdError::Failed("this data file is protected in a way MailCraft cannot open".into()));
    }
    if file.version == 0 || file.version > VERSION {
        return Err(CmdError::Failed(format!(
            "this data file was written by a newer MailCraft (format {})",
            file.version
        )));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let bytes16 = |t: &str| -> Result<[u8; 16], CmdError> {
        let v = b64.decode(t.trim()).map_err(|_| damaged())?;
        <[u8; 16]>::try_from(v.as_slice()).map_err(|_| damaged())
    };
    let sealed = mailcraft_smime::seal::Sealed {
        rounds: file.rounds,
        salt: bytes16(&file.salt)?,
        iv: bytes16(&file.iv)?,
        data: b64.decode(file.data.trim()).map_err(|_| damaged())?,
    };
    match mailcraft_smime::seal::open(&sealed, password) {
        Ok(plain) => Ok(plain.to_vec()),
        Err(mailcraft_smime::SmimeError::Password) => {
            Err(CmdError::Params("the password is wrong, or the data file is damaged".into()))
        }
        Err(e) => Err(CmdError::Failed(format!("cannot open the protected data file: {e}"))),
    }
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

/// Writes `bytes` to `path` through a temporary file next to it and a rename, so a failed write leaves the
/// old file (or nothing) rather than half a file.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CmdError> {
    let fail = |e: std::io::Error| CmdError::Failed(format!("cannot write {}: {e}", path.display()));
    let Some(name) = path.file_name() else {
        return Err(CmdError::Params(format!("{} is not a file name", path.display())));
    };
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, bytes).map_err(fail)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(e));
    }
    Ok(())
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

/// Writes standalone data file `account` to its file now (encrypted when it has a password). Returns the
/// path.
pub fn save_linked(s: &Session, account: AccountId) -> Result<PathBuf, CmdError> {
    let Some(link) = s.mailbox.account(account).and_then(|a| a.data_file.clone()) else {
        return Err(CmdError::Params(
            "this data file is kept with the profile, not in a file of its own".into(),
        ));
    };
    let image = image_of(s, account)?;
    let bytes = serde_json::to_vec(&image)
        .map_err(|e| CmdError::Failed(format!("cannot write the data file: {e}")))?;
    let bytes = if link.protected {
        let Some(pw) = s.secrets.get(&secret_key(account)) else {
            return Err(CmdError::Params("the data file's password is needed to write it".into()));
        };
        seal(&bytes, &pw)?
    } else {
        bytes
    };
    let path = PathBuf::from(&link.path);
    write_file(&path, &bytes)?;
    Ok(path)
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

/// Opens the data file in `bytes` (read from `path`, decrypted with `password` when it is protected) and
/// keeps it linked to that file, so closing it writes it back there.
pub fn open_linked(
    s: &mut Session,
    path: &Path,
    bytes: &[u8],
    password: Option<&str>,
) -> Result<AccountId, CmdError> {
    let path = &absolute(path);
    if let Some(other) = linked_to(s, path) {
        let name = s.mailbox.account(other).map(|a| a.display_name.clone()).unwrap_or_default();
        return Err(CmdError::Params(format!("{} is already open as \"{name}\"", path.display())));
    }
    let protected = is_sealed(bytes);
    let plain = match (protected, password) {
        (true, Some(pw)) => unseal(bytes, pw)?,
        (true, None) => return Err(CmdError::Params("this data file is protected by a password".into())),
        (false, _) => bytes.to_vec(),
    };
    let image = parse(&plain)?;
    let id = attach(s, image)?;
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == id) {
        a.data_file =
            Some(mailcraft_model::account::DataFileLink { path: path.display().to_string(), protected });
    }
    if let (true, Some(pw)) = (protected, password)
        && let Err(e) = s.secrets.set(&secret_key(id), pw)
    {
        log::warn!("could not keep the data file password: {e}");
    }
    Ok(id)
}

/// Closes standalone data file `account`: writes it to its file, then takes it out of the mailbox (nothing is
/// removed when the write fails). Returns the file and the number of folders that left.
pub fn close_linked(s: &mut Session, account: AccountId) -> Result<(PathBuf, usize), CmdError> {
    let path = save_linked(s, account)?;
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
