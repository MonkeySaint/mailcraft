//! In-memory mailbox: every account, folder and item the session knows. The engine mutates it through
//! commands; `mailcraft-store` persists it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    Account, AccountId, AccountKind, Appointment, Category, Contact, Folder, FolderId, FolderKind,
    FolderRole, ItemId, MailItem, MailTable, ModelError, Note, QuickStep, QuickStepAction, Result, Rule,
    RuleAction, Task, ViewSettings,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mailbox {
    pub accounts: Vec<Account>,
    pub folders: BTreeMap<FolderId, Folder>,
    /// Mail items, behind the [`MailTable`] read/write API.
    pub mail: MailTable,
    pub appointments: BTreeMap<ItemId, Appointment>,
    pub contacts: BTreeMap<ItemId, Contact>,
    pub tasks: BTreeMap<ItemId, Task>,
    pub notes: BTreeMap<ItemId, Note>,
    pub categories: Vec<Category>,
    pub rules: Vec<Rule>,
    pub quick_steps: Vec<QuickStep>,
    /// Per-folder view settings (missing = default).
    pub views: BTreeMap<FolderId, ViewSettings>,
    pub next_id: u64,
    /// Saved signatures (`signature.rs`).
    #[serde(default)]
    pub signatures: Vec<crate::signature::Signature>,
    /// Per-account default signature choices.
    #[serde(default)]
    pub signature_defaults: Vec<crate::signature::AccountSignatures>,
    /// Auto-Complete list: recently used recipients, most recent first.
    #[serde(default)]
    pub recent_recipients: Vec<crate::signature::RecentRecipient>,
    /// Words added to the spelling dictionary.
    #[serde(default)]
    pub custom_dictionary: Vec<String>,
    /// Junk Email Options: level, options and the safe/blocked lists (`junk.rs`).
    #[serde(default)]
    pub junk: crate::junk::JunkSettings,
}

/// Mail counts of one folder (the folder pane's badges and accessible names).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCounts {
    /// Mail items filed in the folder.
    pub total: usize,
    /// Those not read.
    pub unread: usize,
}

impl FolderCounts {
    pub(crate) fn add(&mut self, read: bool) {
        self.total = self.total.saturating_add(1);
        if !read {
            self.unread = self.unread.saturating_add(1);
        }
    }
}

/// Longest folder name accepted.
pub const MAX_FOLDER_NAME_CHARS: usize = 255;

/// Name given to a local account created with an empty display name.
pub const LOCAL_ACCOUNT_FALLBACK_NAME: &str = "Local Folders";

/// The local account name MailCraft 0.1.x gave new profiles; [`Mailbox::rename_legacy_local_account`] renames it.
pub const LEGACY_LOCAL_ACCOUNT_NAME: &str = "Outlook Data File";

/// Default Favorites, in order (`plan/reference/folders.md`).
pub const DEFAULT_FAVORITES: [FolderRole; 4] =
    [FolderRole::Inbox, FolderRole::SentItems, FolderRole::Drafts, FolderRole::DeletedItems];

impl Mailbox {
    /// Empty mailbox with one local account (a data file named `display_name`), all default
    /// folders, default categories and quick steps.
    /// Renames local accounts still called [`LEGACY_LOCAL_ACCOUNT_NAME`] (profiles made by MailCraft 0.1.x)
    /// to [`LOCAL_ACCOUNT_FALLBACK_NAME`] and drops the matching sender name. Returns how many changed.
    pub fn rename_legacy_local_account(&mut self) -> usize {
        let mut changed = 0;
        for a in self.accounts.iter_mut().filter(|a| a.kind == AccountKind::Local) {
            if a.display_name == LEGACY_LOCAL_ACCOUNT_NAME {
                a.display_name = LOCAL_ACCOUNT_FALLBACK_NAME.to_owned();
                changed += 1;
            }
            if a.sender_name == LEGACY_LOCAL_ACCOUNT_NAME {
                a.sender_name.clear();
            }
        }
        changed
    }

    /// Drops the "Create New" placeholder step older profiles stored among their quick steps (the gallery
    /// shows Create New itself now). Returns how many were dropped.
    pub fn drop_legacy_create_new_step(&mut self) -> usize {
        let before = self.quick_steps.len();
        self.quick_steps.retain(|q| !q.is_legacy_create_new());
        before.saturating_sub(self.quick_steps.len())
    }

    pub fn new_local(display_name: &str) -> Mailbox {
        let mut mb = Mailbox::default();
        let name = display_name.trim();
        let name = if name.is_empty() { LOCAL_ACCOUNT_FALLBACK_NAME } else { name };
        let account = mb.add_account(Account {
            kind: AccountKind::Local,
            display_name: name.to_string(),
            // A data file is not a person: no sender name to suggest anywhere.
            sender_name: String::new(),
            is_default: true,
            ..Default::default()
        });
        mb.create_default_folders(account);
        for (pos, role) in DEFAULT_FAVORITES.iter().enumerate() {
            if let Some(id) = mb.role_folder(account, *role)
                && let Some(f) = mb.folders.get_mut(&id)
            {
                f.favorite_pos = u32::try_from(pos).ok();
            }
        }
        if let Some(inbox) = mb.role_folder(account, FolderRole::Inbox)
            && let Some(f) = mb.folders.get_mut(&inbox)
        {
            f.expanded = true;
        }
        mb.categories = Category::defaults();
        mb.quick_steps = QuickStep::defaults();
        mb
    }
    /// Fresh id for folders and items (monotonic).
    ///
    /// Skips any value already used by a folder or item (e.g. after loading data whose `next_id` is stale).
    ///
    /// Never returns an id that is in use: once the counter is exhausted (only reachable with hostile or
    /// corrupted data) the lowest free id is handed out instead, so inserts can never overwrite each other.
    pub fn alloc_id(&mut self) -> u64 {
        while let Some(next) = self.next_id.checked_add(1) {
            self.next_id = next;
            if !self.id_in_use(next) {
                return next;
            }
        }
        self.lowest_free_id()
    }
    /// Smallest id ≥ 1 not used by any folder or item. The maps hold far fewer than `u64::MAX` entries, so a
    /// free id exists within the first `len + 1` candidates.
    fn lowest_free_id(&self) -> u64 {
        let used: std::collections::BTreeSet<u64> = self
            .folders
            .keys()
            .map(|f| f.0)
            .chain(self.mail.ids().map(|i| i.0))
            .chain(self.appointments.keys().map(|i| i.0))
            .chain(self.contacts.keys().map(|i| i.0))
            .chain(self.tasks.keys().map(|i| i.0))
            .chain(self.notes.keys().map(|i| i.0))
            .collect();
        let mut candidate: u64 = 1;
        for id in used.range(1..) {
            if *id != candidate {
                break;
            }
            candidate = candidate.saturating_add(1);
        }
        candidate
    }
    fn id_in_use(&self, id: u64) -> bool {
        self.folders.contains_key(&FolderId(id))
            || self.mail.contains(ItemId(id))
            || self.appointments.contains_key(&ItemId(id))
            || self.contacts.contains_key(&ItemId(id))
            || self.tasks.contains_key(&ItemId(id))
            || self.notes.contains_key(&ItemId(id))
    }
    pub fn add_account(&mut self, account: Account) -> AccountId {
        let next = self.accounts.iter().map(|a| a.id.0).max().map_or(Some(1), |m| m.checked_add(1));
        // At the limit (hostile data), take the lowest unused id instead of repeating the maximum.
        let id = next.unwrap_or_else(|| {
            let used: std::collections::BTreeSet<u32> = self.accounts.iter().map(|a| a.id.0).collect();
            (1..=u32::MAX).find(|i| !used.contains(i)).unwrap_or(0)
        });
        let mut account = account;
        account.id = AccountId(id);
        if self.accounts.is_empty() {
            account.is_default = true;
        } else if account.is_default {
            for a in &mut self.accounts {
                a.is_default = false;
            }
        }
        self.accounts.push(account);
        AccountId(id)
    }
    pub fn account(&self, id: AccountId) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }
    /// The default sending account (or the first account).
    pub fn default_account(&self) -> Option<&Account> {
        self.accounts.iter().find(|a| a.is_default).or_else(|| self.accounts.first())
    }
    /// Creates the default folders for an account; returns them in display order.
    ///
    /// Roles that already exist for the account are kept (their ids are returned), so the call is idempotent.
    pub fn create_default_folders(&mut self, account: AccountId) -> Vec<FolderId> {
        let mut out = Vec::with_capacity(FolderRole::DEFAULTS.len());
        for role in FolderRole::DEFAULTS {
            if let Some(existing) = self.role_folder(account, role) {
                out.push(existing);
                continue;
            }
            let id = FolderId(self.alloc_id());
            self.folders.insert(
                id,
                Folder {
                    id,
                    account,
                    parent: None,
                    name: role.default_name().to_string(),
                    role: Some(role),
                    kind: role.kind(),
                    ..Default::default()
                },
            );
            out.push(id);
        }
        out
    }
    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.get(&id)
    }
    /// The folder with `role` in `account`.
    pub fn role_folder(&self, account: AccountId, role: FolderRole) -> Option<FolderId> {
        self.folders.values().find(|f| f.account == account && f.role == Some(role)).map(|f| f.id)
    }
    /// Children of `parent` (`None` = account roots) for `account`, in display order.
    pub fn children(&self, account: AccountId, parent: Option<FolderId>) -> Vec<FolderId> {
        let mut kids: Vec<&Folder> =
            self.folders.values().filter(|f| f.account == account && f.parent == parent).collect();
        kids.sort_by_cached_key(|f| (f.sort_key(), f.id));
        kids.into_iter().map(|f| f.id).collect()
    }
    /// Favorites in order.
    pub fn favorites(&self) -> Vec<FolderId> {
        let mut favs: Vec<(u32, FolderId)> =
            self.folders.values().filter_map(|f| f.favorite_pos.map(|p| (p, f.id))).collect();
        favs.sort();
        favs.into_iter().map(|(_, id)| id).collect()
    }
    /// Adds a folder at the end of Favorites (no-op when already a favourite).
    pub fn add_favorite(&mut self, id: FolderId) -> Result<()> {
        let f = self.folders.get(&id).ok_or(ModelError::NoFolder(id))?;
        if f.favorite_pos.is_some() {
            return Ok(());
        }
        // Append to the current order and renumber, so gaps in stored positions cannot place it mid-list.
        let mut order = self.favorites();
        order.push(id);
        self.renumber_favorites(order);
        Ok(())
    }
    /// Removes a folder from Favorites (no-op when it is not one).
    pub fn remove_favorite(&mut self, id: FolderId) -> Result<()> {
        let f = self.folders.get_mut(&id).ok_or(ModelError::NoFolder(id))?;
        f.favorite_pos = None;
        self.renumber_favorites(self.favorites());
        Ok(())
    }
    /// Moves a favourite up (`delta` < 0) or down (`delta` > 0) by that many places, clamped to the list.
    pub fn move_favorite(&mut self, id: FolderId, delta: i32) -> Result<()> {
        let mut favs = self.favorites();
        let pos = favs.iter().position(|f| *f == id).ok_or(ModelError::NoFolder(id))?;
        let last = favs.len().saturating_sub(1);
        let target = if delta < 0 {
            pos.saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX))
        } else {
            pos.saturating_add(usize::try_from(delta).unwrap_or(usize::MAX)).min(last)
        };
        if target != pos && pos < favs.len() {
            let item = favs.remove(pos);
            favs.insert(target.min(favs.len()), item);
        }
        self.renumber_favorites(favs);
        Ok(())
    }
    fn renumber_favorites(&mut self, order: Vec<FolderId>) {
        for (i, id) in order.into_iter().enumerate() {
            if let Some(f) = self.folders.get_mut(&id) {
                f.favorite_pos = u32::try_from(i).ok();
            }
        }
    }
    /// True when `id` is `ancestor` or lies below it.
    pub fn is_in_subtree(&self, id: FolderId, ancestor: FolderId) -> bool {
        let mut cur = Some(id);
        // Bounded walk: a corrupted (cyclic) parent chain cannot loop forever.
        for _ in 0..=self.folders.len() {
            match cur {
                Some(c) if c == ancestor => return true,
                Some(c) => cur = self.folders.get(&c).and_then(|f| f.parent),
                None => return false,
            }
        }
        false
    }
    /// `id` and every folder below it (pre-order, display order among siblings).
    pub fn subtree(&self, id: FolderId) -> Vec<FolderId> {
        if !self.folders.contains_key(&id) {
            return Vec::new();
        }
        // One pass to index children (display order), then a DFS: linear-ish instead of rescanning every
        // folder for every node.
        let mut index: BTreeMap<FolderId, Vec<&Folder>> = BTreeMap::new();
        for f in self.folders.values() {
            if let Some(p) = f.parent
                && self.folders.get(&p).is_some_and(|pf| pf.account == f.account)
            {
                index.entry(p).or_default().push(f);
            }
        }
        for kids in index.values_mut() {
            kids.sort_by_cached_key(|f| (f.sort_key(), f.id));
        }
        let mut out = Vec::new();
        let mut stack = vec![id];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(f) = stack.pop() {
            if !visited.insert(f) {
                continue;
            }
            out.push(f);
            if let Some(kids) = index.get(&f) {
                stack.extend(kids.iter().rev().map(|k| k.id));
            }
        }
        out
    }
    /// Validates a folder name; returns it trimmed.
    fn valid_name(name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ModelError::Invalid("folder name is empty".into()));
        }
        if name.chars().count() > MAX_FOLDER_NAME_CHARS {
            return Err(ModelError::Invalid("folder name is too long".into()));
        }
        if name.contains('\\') {
            return Err(ModelError::Invalid("folder names cannot contain a backslash (\\)".into()));
        }
        if name.chars().any(char::is_control) {
            return Err(ModelError::Invalid("folder name contains control characters".into()));
        }
        Ok(name.to_string())
    }
    /// True when a child of `parent` other than `except` already has `name` (case-insensitive).
    fn sibling_name_taken(&self, parent: FolderId, name: &str, except: Option<FolderId>) -> bool {
        match self.folders.get(&parent) {
            Some(p) => self.name_taken_under(p.account, Some(parent), name, except),
            None => false,
        }
    }
    /// True when a folder of `account` directly under `parent` (`None` = the account's roots), other than
    /// `except`, already has `name` (case-insensitive).
    fn name_taken_under(
        &self,
        account: AccountId,
        parent: Option<FolderId>,
        name: &str,
        except: Option<FolderId>,
    ) -> bool {
        let lower = name.to_lowercase();
        self.folders.values().any(|f| {
            f.account == account
                && f.parent == parent
                && Some(f.id) != except
                && f.name.to_lowercase() == lower
        })
    }
    /// Creates a user folder. Errors: unknown parent, empty name, duplicate sibling name (case-insensitive).
    ///
    /// The new folder holds the same kind of items as its parent (a folder under Calendar is a calendar);
    /// under Search Folders it is a search folder. Search folders cannot have subfolders.
    pub fn create_folder(&mut self, parent: FolderId, name: &str) -> Result<FolderId> {
        self.create_folder_of_kind(parent, name, None)
    }

    /// Like [`Mailbox::create_folder`], with what the folder contains (Create New Folder › Folder contains):
    /// `None` inherits the parent's kind. Mail, calendar, contacts, tasks, notes and journal folders can sit
    /// under any of those; Search Folders only hold saved searches.
    pub fn create_folder_of_kind(
        &mut self,
        parent: FolderId,
        name: &str,
        kind: Option<FolderKind>,
    ) -> Result<FolderId> {
        let p = self.folders.get(&parent).ok_or(ModelError::NoFolder(parent))?;
        let kind = match (p.kind, kind) {
            (FolderKind::Search, _) => {
                return Err(ModelError::NotAllowed("search folders cannot contain folders".into()));
            }
            (FolderKind::SearchRoot, None | Some(FolderKind::Search)) => FolderKind::Search,
            (FolderKind::SearchRoot, Some(_)) => {
                return Err(ModelError::NotAllowed("Search Folders only hold saved searches".into()));
            }
            (_, Some(FolderKind::Search | FolderKind::SearchRoot)) => {
                return Err(ModelError::NotAllowed("make search folders with New Search Folder".into()));
            }
            (k, None) => k,
            (_, Some(k)) => k,
        };
        let account = p.account;
        let name = Self::valid_name(name)?;
        if self.sibling_name_taken(parent, &name, None) {
            return Err(ModelError::Invalid(format!("a folder named \"{name}\" already exists here")));
        }
        let id = FolderId(self.alloc_id());
        self.folders
            .insert(id, Folder { id, account, parent: Some(parent), name, kind, ..Default::default() });
        Ok(id)
    }
    /// Creates a user folder holding `kind` items directly under `parent`, or at the top of `account`'s folder
    /// list when `parent` is `None` (the account root). `kind` must be an item kind (mail, calendar, contacts,
    /// tasks, notes, journal); search folders and their container are made elsewhere. Errors as
    /// [`Mailbox::create_folder`], plus an unknown account, or a parent of another account or under Search
    /// Folders.
    pub fn create_folder_in(
        &mut self,
        account: AccountId,
        parent: Option<FolderId>,
        name: &str,
        kind: FolderKind,
    ) -> Result<FolderId> {
        if matches!(kind, FolderKind::Search | FolderKind::SearchRoot) {
            return Err(ModelError::NotAllowed("search folders are made with New Search Folder".into()));
        }
        if self.account(account).is_none() {
            return Err(ModelError::Invalid(format!("no such account {}", account.0)));
        }
        if let Some(pid) = parent {
            let p = self.folders.get(&pid).ok_or(ModelError::NoFolder(pid))?;
            if p.account != account {
                return Err(ModelError::Invalid("the parent folder belongs to another account".into()));
            }
            if matches!(p.kind, FolderKind::Search | FolderKind::SearchRoot) {
                return Err(ModelError::NotAllowed("search folders cannot contain other folders".into()));
            }
        }
        let name = Self::valid_name(name)?;
        if self.name_taken_under(account, parent, &name, None) {
            return Err(ModelError::Invalid(format!("a folder named \"{name}\" already exists here")));
        }
        let id = FolderId(self.alloc_id());
        self.folders.insert(id, Folder { id, account, parent, name, kind, ..Default::default() });
        Ok(id)
    }
    /// Rename (default folders: NotAllowed).
    pub fn rename_folder(&mut self, id: FolderId, name: &str) -> Result<()> {
        let f = self.folders.get(&id).ok_or(ModelError::NoFolder(id))?;
        if f.is_default() {
            return Err(ModelError::NotAllowed("default folders cannot be renamed".into()));
        }
        let (parent, account) = (f.parent, f.account);
        let name = Self::valid_name(name)?;
        if self.name_taken_under(account, parent, &name, Some(id)) {
            return Err(ModelError::Invalid(format!("a folder named \"{name}\" already exists here")));
        }
        if let Some(f) = self.folders.get_mut(&id) {
            f.name = name;
        }
        Ok(())
    }
    /// Move under a new parent (no cycles; default folders: NotAllowed).
    pub fn move_folder(&mut self, id: FolderId, new_parent: FolderId) -> Result<()> {
        let f = self.folders.get(&id).ok_or(ModelError::NoFolder(id))?;
        let p = self.folders.get(&new_parent).ok_or(ModelError::NoFolder(new_parent))?;
        if f.is_default() {
            return Err(ModelError::NotAllowed("default folders cannot be moved".into()));
        }
        if f.parent == Some(new_parent) {
            return Ok(());
        }
        if self.is_in_subtree(new_parent, id) {
            return Err(ModelError::NotAllowed(
                "a folder cannot be moved into itself or its subfolders".into(),
            ));
        }
        let search_target = matches!(p.kind, FolderKind::Search | FolderKind::SearchRoot);
        if search_target != (f.kind == FolderKind::Search) {
            return Err(ModelError::NotAllowed("search folders stay under Search Folders".into()));
        }
        if p.kind == FolderKind::Search {
            return Err(ModelError::NotAllowed("search folders cannot contain folders".into()));
        }
        let name = f.name.clone();
        let new_account = p.account;
        if self.sibling_name_taken(new_parent, &name, Some(id)) {
            return Err(ModelError::Invalid(format!("a folder named \"{name}\" already exists there")));
        }
        let moved = self.subtree(id);
        if let Some(f) = self.folders.get_mut(&id) {
            f.parent = Some(new_parent);
            f.manual_pos = None;
        }
        for fid in moved {
            if let Some(f) = self.folders.get_mut(&fid) {
                f.account = new_account;
            }
        }
        Ok(())
    }
    /// Deletes a user folder: it and its subtree move into Deleted Items (deleting inside Deleted Items removes
    /// permanently). Default folders: NotAllowed.
    ///
    /// Search folders are always removed outright (they hold no items). A name clash inside Deleted Items is
    /// resolved by numbering ("Projects (2)").
    pub fn delete_folder(&mut self, id: FolderId) -> Result<()> {
        let f = self.folders.get(&id).ok_or(ModelError::NoFolder(id))?;
        if f.is_default() {
            return Err(ModelError::NotAllowed("default folders cannot be deleted".into()));
        }
        let account = f.account;
        let is_search = f.kind == FolderKind::Search;
        let name = f.name.clone();
        let deleted = self.role_folder(account, FolderRole::DeletedItems);
        let permanent = is_search || deleted.is_none_or(|d| self.is_in_subtree(id, d));
        let subtree = self.subtree(id);
        if permanent {
            self.remove_folders_permanently(&subtree);
            return Ok(());
        }
        let Some(deleted) = deleted else {
            return Ok(());
        };
        let mut new_name = name.clone();
        let mut n: u32 = 2;
        while self.sibling_name_taken(deleted, &new_name, Some(id)) {
            // Keep the numbered name within the folder-name limit by shortening the base when needed.
            let suffix = format!(" ({n})");
            let keep = MAX_FOLDER_NAME_CHARS.saturating_sub(suffix.chars().count());
            let base: String = name.chars().take(keep).collect();
            new_name = format!("{}{suffix}", base.trim_end());
            n = n.saturating_add(1);
            if n == u32::MAX {
                break;
            }
        }
        if let Some(f) = self.folders.get_mut(&id) {
            f.parent = Some(deleted);
            f.name = new_name;
            f.manual_pos = None;
        }
        for fid in subtree {
            if let Some(f) = self.folders.get_mut(&fid) {
                f.favorite_pos = None;
            }
        }
        self.renumber_favorites(self.favorites());
        Ok(())
    }
    /// Removes folders and everything stored in them; rules that move or copy into them are disabled and
    /// quick steps that do are reset to ask for a folder.
    fn remove_folders_permanently(&mut self, ids: &[FolderId]) {
        let set: std::collections::BTreeSet<FolderId> = ids.iter().copied().collect();
        self.folders.retain(|id, _| !set.contains(id));
        self.mail.retain(|_, m| !set.contains(&m.folder));
        self.appointments.retain(|_, a| !set.contains(&a.folder));
        self.contacts.retain(|_, c| !set.contains(&c.folder));
        self.tasks.retain(|_, t| !set.contains(&t.folder));
        self.notes.retain(|_, n| !set.contains(&n.folder));
        self.views.retain(|id, _| !set.contains(id));
        let rules_target =
            |a: &RuleAction| matches!(a, RuleAction::MoveTo(f) | RuleAction::CopyTo(f) if set.contains(f));
        for rule in &mut self.rules {
            if rule.actions.iter().any(rules_target) {
                rule.enabled = false;
            }
        }
        // Quick steps that filed into a removed folder go back to "ask on first use".
        for step in &mut self.quick_steps {
            for action in &mut step.actions {
                if let QuickStepAction::MoveTo(target) | QuickStepAction::CopyTo(target) = action
                    && target.is_some_and(|f| set.contains(&f))
                {
                    *target = None;
                }
            }
        }
        self.renumber_favorites(self.favorites());
    }
    /// Inserts a mail item with a fresh id (keeps `item.folder`).
    pub fn add_mail(&mut self, item: MailItem) -> ItemId {
        let id = ItemId(self.alloc_id());
        let mut item = item;
        item.id = id;
        self.mail.insert(id, item);
        id
    }
    /// Mail items in `folder` (unsorted).
    pub fn items_in(&self, folder: FolderId) -> Vec<&MailItem> {
        self.mail.in_folder(folder).collect()
    }
    pub fn unread_count(&self, folder: FolderId) -> usize {
        self.mail.counts(folder).unread
    }
    pub fn total_count(&self, folder: FolderId) -> usize {
        self.mail.counts(folder).total
    }
    /// The mail counts of `folder` (one scan of all mail; [`Mailbox::folder_counts`] counts every folder in
    /// one scan).
    pub fn counts_of(&self, folder: FolderId) -> FolderCounts {
        self.mail.counts(folder)
    }
    /// The mail counts of every folder holding mail, in one scan of all mail. Folders without mail are
    /// missing (their counts are zero).
    pub fn folder_counts(&self) -> BTreeMap<FolderId, FolderCounts> {
        self.mail.all_counts()
    }
    /// Folder-pane badge text: "[8]" for a folder that counts every item (total-count roles, or Properties ›
    /// "Show total number of items") when non-zero, "778" unread for others, `None` when nothing to show.
    pub fn badge(&self, folder: FolderId) -> Option<String> {
        self.folders.get(&folder)?;
        self.badge_with(folder, self.counts_of(folder))
    }
    /// [`Mailbox::badge`] from counts the caller already has.
    pub fn badge_with(&self, folder: FolderId, counts: FolderCounts) -> Option<String> {
        let f = self.folders.get(&folder)?;
        if f.counts_total() {
            let n = counts.total;
            return (n > 0).then(|| format!("[{n}]"));
        }
        let n = counts.unread;
        (n > 0).then(|| n.to_string())
    }
    /// Accessible name of a folder row: "Inbox: 778 unread messages", "Drafts: 8 items", or just the name.
    pub fn folder_accessible_name(&self, folder: FolderId) -> String {
        if !self.folders.contains_key(&folder) {
            return String::new();
        }
        self.folder_accessible_name_with(folder, self.counts_of(folder))
    }
    /// [`Mailbox::folder_accessible_name`] from counts the caller already has.
    pub fn folder_accessible_name_with(&self, folder: FolderId, counts: FolderCounts) -> String {
        let Some(f) = self.folders.get(&folder) else {
            return String::new();
        };
        if f.counts_total() {
            match counts.total {
                0 => f.name.clone(),
                1 => format!("{}: 1 item", f.name),
                n => format!("{}: {n} items", f.name),
            }
        } else {
            match counts.unread {
                0 => f.name.clone(),
                1 => format!("{}: 1 unread message", f.name),
                n => format!("{}: {n} unread messages", f.name),
            }
        }
    }
    /// A rough estimate of the memory this mailbox takes (fixed sizes plus the text and bytes it holds),
    /// for byte caps such as the undo history's. One pass over every item; not exact, never panics.
    pub fn approx_bytes(&self) -> usize {
        let s = |t: &String| t.len();
        let list = |v: &[String]| v.iter().map(String::len).fold(0usize, usize::saturating_add);
        let mut total = std::mem::size_of::<Mailbox>();
        let mut add = |n: usize| total = total.saturating_add(n);
        for m in self.mail.items() {
            add(m.approx_bytes());
        }
        for a in self.appointments.values() {
            add(std::mem::size_of_val(a));
            add(s(&a.subject).saturating_add(s(&a.location)).saturating_add(s(&a.body)));
            add(a.body_html.as_ref().map_or(0, String::len));
            add(a.remote.as_ref().map_or(0, |r| r.raw.len()));
        }
        for c in self.contacts.values() {
            add(std::mem::size_of_val(c));
            add(s(&c.file_as).saturating_add(s(&c.notes)).saturating_add(list(&c.emails)));
            add(c.picture.as_ref().map_or(0, |p| p.data.len()));
            add(list(&c.certificates));
            add(c.remote.as_ref().map_or(0, |r| r.raw.len()));
        }
        for t in self.tasks.values() {
            add(std::mem::size_of_val(t));
            add(s(&t.subject).saturating_add(s(&t.body)));
        }
        for n in self.notes.values() {
            add(std::mem::size_of_val(n));
            add(s(&n.text));
        }
        add(self.folders.len().saturating_mul(std::mem::size_of::<crate::Folder>().saturating_add(64)));
        add(self.views.len().saturating_mul(std::mem::size_of::<ViewSettings>()));
        total
    }
    /// Moves items; returns the ids actually moved.
    ///
    /// Unknown ids and items already in `to` are skipped. Only mail folders accept mail items.
    pub fn move_items(&mut self, ids: &[ItemId], to: FolderId) -> Result<Vec<ItemId>> {
        let target = self.folders.get(&to).ok_or(ModelError::NoFolder(to))?;
        if target.kind != FolderKind::Mail {
            return Err(ModelError::NotAllowed(format!(
                "mail items cannot be moved into \"{}\"",
                target.name
            )));
        }
        let mut moved = Vec::new();
        for id in ids {
            if let Some(m) = self.mail.get_mut(*id)
                && m.folder != to
            {
                m.folder = to;
                moved.push(*id);
            }
        }
        Ok(moved)
    }
    /// Delete: move to Deleted Items; items already in Deleted Items are removed permanently. Returns removed
    /// item snapshots (for undo).
    ///
    /// Each snapshot is the item as it was before the call (including its original folder), for both moved
    /// and permanently removed items, so undo can restore by re-inserting the snapshots. Items in a folder
    /// below Deleted Items, or in an account without one, are removed permanently. Unknown ids are skipped.
    pub fn delete_items(&mut self, ids: &[ItemId]) -> Result<Vec<MailItem>> {
        let mut out = Vec::new();
        for id in ids {
            let Some(item) = self.mail.get(*id) else {
                continue;
            };
            let account = self.folders.get(&item.folder).map(|f| f.account);
            let deleted = account.and_then(|a| self.role_folder(a, FolderRole::DeletedItems));
            match deleted {
                Some(d) if !self.is_in_subtree(item.folder, d) => {
                    if let Some(m) = self.mail.get_mut(*id) {
                        out.push(m.clone());
                        m.folder = d;
                    }
                }
                _ => {
                    if let Some(m) = self.mail.remove(*id) {
                        out.push(m);
                    }
                }
            }
        }
        Ok(out)
    }
    /// Path for display / Move dialog: "Inbox\\Projects".
    pub fn folder_path(&self, id: FolderId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        for _ in 0..=self.folders.len() {
            let Some(c) = cur else { break };
            let Some(f) = self.folders.get(&c) else { break };
            parts.push(f.name.as_str());
            cur = f.parent;
        }
        parts.reverse();
        parts.join("\\")
    }
    /// Account id of a folder.
    pub fn folder_account(&self, id: FolderId) -> Option<AccountId> {
        self.folders.get(&id).map(|f| f.account)
    }
    /// Marks items read/unread; returns the ids whose state changed.
    pub fn set_read(&mut self, ids: &[ItemId], read: bool) -> Vec<ItemId> {
        let mut changed = Vec::new();
        for id in ids {
            if let Some(m) = self.mail.get_mut(*id)
                && m.read != read
            {
                m.read = read;
                changed.push(*id);
            }
        }
        changed
    }
    /// Checks structural invariants (unique ids, parents exist and share the account, no cycles, at most one
    /// folder per role per account, items live in existing folders). Returns the problems found.
    ///
    /// "Unique ids" means every folder and item is stored under its own id and no id is shared between
    /// folders and items of any kind (all of them come from [`Mailbox::alloc_id`]).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let mut check_id = |what: &str, key: u64, id: u64, problems: &mut Vec<String>| {
            if key != id {
                problems.push(format!("{what} {id} is stored under key {key}"));
            }
            if !seen_ids.insert(key) {
                problems.push(format!("id {key} is used more than once"));
            }
        };
        for (k, f) in &self.folders {
            check_id("folder", k.0, f.id.0, &mut problems);
        }
        for (k, m) in &self.mail {
            check_id("item", k.0, m.id.0, &mut problems);
        }
        let others = self
            .appointments
            .iter()
            .map(|(k, a)| (*k, a.id, a.folder))
            .chain(self.contacts.iter().map(|(k, c)| (*k, c.id, c.folder)))
            .chain(self.tasks.iter().map(|(k, t)| (*k, t.id, t.folder)))
            .chain(self.notes.iter().map(|(k, n)| (*k, n.id, n.folder)));
        for (k, id, folder) in others {
            check_id("item", k.0, id.0, &mut problems);
            if !self.folders.contains_key(&folder) {
                problems.push(format!("item {id:?} is in missing folder {folder:?}"));
            }
        }
        let mut role_counts: BTreeMap<(AccountId, FolderRole), usize> = BTreeMap::new();
        for f in self.folders.values() {
            if let Some(p) = f.parent {
                match self.folders.get(&p) {
                    None => problems.push(format!("folder {:?} has missing parent {:?}", f.id, p)),
                    Some(pf) if pf.account != f.account => {
                        problems.push(format!("folder {:?} is in a different account than its parent", f.id));
                    }
                    _ => {}
                }
                if self.is_in_subtree(p, f.id) {
                    problems.push(format!("folder {:?} is part of a cycle", f.id));
                }
            }
            if let Some(role) = f.role {
                let n = role_counts.entry((f.account, role)).or_insert(0usize);
                *n = n.saturating_add(1);
            }
        }
        for ((account, role), n) in role_counts {
            if n > 1 {
                problems.push(format!("role {role:?} appears {n} times in account {account:?}"));
            }
        }
        for m in self.mail.items() {
            if !self.folders.contains_key(&m.folder) {
                problems.push(format!("item {:?} is in missing folder {:?}", m.id, m.folder));
            }
        }
        problems.sort();
        problems.dedup();
        problems
    }
}
