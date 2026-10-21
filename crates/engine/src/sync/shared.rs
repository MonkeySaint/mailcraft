//! Other users' and shared IMAP mailboxes (RFC 2342 namespaces) opened as stores of their own.
//!
//! A full send/receive of an IMAP account lists the mailboxes its login may open beside its own (the
//! "other users" and "shared" namespaces) and keeps the list on the account
//! (`AccountSync::shared_roots`). Opening one ([`open`]) adds a store: an account record marked
//! [`SharedStore`] that signs in with the owner's login, lists only the folders below the mailbox's root
//! (its Inbox is the root or the root's `INBOX`), cannot send, and shows in the folder pane and in Search ›
//! Locations to Search like any other mailbox. Closing it ([`close`]) removes the store and its local copy;
//! the mail on the server is not touched.

use mailcraft_model::account::SharedRootInfo;
use mailcraft_model::{Account, AccountId, AccountKind, FolderRole, SharedStore};

use crate::{CmdError, Session};

/// Most stores opened from one account.
pub const MAX_STORES: usize = 50;

#[cfg(not(target_arch = "wasm32"))]
fn kind_of(k: mailcraft_net::sync::SharedKind) -> mailcraft_model::account::SharedKind {
    match k {
        mailcraft_net::sync::SharedKind::OtherUser => mailcraft_model::account::SharedKind::OtherUser,
        mailcraft_net::sync::SharedKind::Shared => mailcraft_model::account::SharedKind::Shared,
    }
}

/// Records the shared mailboxes a run of `account` listed.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn apply_roots(s: &mut Session, account: AccountId, roots: Vec<mailcraft_net::sync::SharedRoot>) {
    let list: Vec<SharedRootInfo> = roots
        .into_iter()
        .map(|r| SharedRootInfo {
            root: r.root,
            name: r.name.chars().filter(|c| !c.is_control()).take(200).collect(),
            delimiter: r.delimiter,
            kind: kind_of(r.kind),
        })
        .collect();
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account)
        && a.sync.shared_roots != list
    {
        a.sync.shared_roots = list;
    }
}

/// The stores opened from `owner`.
pub fn stores_of(s: &Session, owner: AccountId) -> Vec<&Account> {
    s.mailbox.accounts.iter().filter(|a| a.shared.as_ref().is_some_and(|x| x.owner == owner)).collect()
}

/// The shared mailbox of `owner` named `name` (its name or full server name).
fn find_root(s: &Session, owner: AccountId, name: &str) -> Option<SharedRootInfo> {
    let a = s.mailbox.account(owner)?;
    let t = name.trim();
    a.sync
        .shared_roots
        .iter()
        .find(|r| r.root == t)
        .or_else(|| a.sync.shared_roots.iter().find(|r| r.name.eq_ignore_ascii_case(t)))
        .cloned()
}

/// Opens the shared mailbox `name` of `owner` as a store. Returns the store's account id (the existing one
/// when it is open already).
pub fn open(s: &mut Session, owner: AccountId, name: &str) -> Result<AccountId, CmdError> {
    let Some(o) = s.mailbox.account(owner).cloned() else {
        return Err(CmdError::Params("no such account".into()));
    };
    if o.kind != AccountKind::Imap || o.shared.is_some() || crate::accounts::is_pop3(s, owner) {
        return Err(CmdError::Disabled("Shared mailboxes need an IMAP account.".into()));
    }
    let Some(root) = find_root(s, owner, name) else {
        return Err(CmdError::Params(format!(
            "{} offers no shared mailbox named \"{}\" (Send/Receive lists them)",
            o.display_name,
            name.trim()
        )));
    };
    if let Some(a) =
        stores_of(s, owner).into_iter().find(|a| a.shared.as_ref().is_some_and(|x| x.root == root.root))
    {
        return Ok(a.id);
    }
    if stores_of(s, owner).len() >= MAX_STORES {
        return Err(CmdError::Failed(format!("at most {MAX_STORES} shared mailboxes per account")));
    }
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        display_name: root.name.clone(),
        address: String::new(),
        incoming: o.incoming.clone(),
        credential: o.credential,
        shared: Some(SharedStore {
            owner,
            root: root.root.clone(),
            delimiter: root.delimiter,
            kind: root.kind,
        }),
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    if let Some(inbox) = s.mailbox.role_folder(id, FolderRole::Inbox)
        && let Some(f) = s.mailbox.folders.get_mut(&inbox)
    {
        f.expanded = true;
    }
    Ok(id)
}

/// Closes a store opened from a shared mailbox: its local copy goes, the server's mail stays.
pub fn close(s: &mut Session, store: AccountId) -> Result<usize, CmdError> {
    if s.mailbox.account(store).is_none_or(|a| a.shared.is_none()) {
        return Err(CmdError::Params("not a shared mailbox".into()));
    }
    super::forget_account(s, store);
    let folders = crate::accounts::remove_account_data(s, store);
    crate::accounts::forget_options(s, store);
    Ok(folders)
}
