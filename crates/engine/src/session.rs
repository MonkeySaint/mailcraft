//! The session: mailbox + UI-independent state + undo + store.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::compose::ComposeState;
use crate::undo::{UndoEntry, UndoStack};
use crate::{CmdError, CmdResult, Registry};
use mailcraft_model::{
    DateTime, FolderId, FolderKind, FolderPaneState, FolderRole, ItemId, Mailbox, ReadingPanePos,
};
use mailcraft_store::Op;

/// Navigation modules (module rail order: Mail, Calendar, People, Tasks (To Do), Notes, Folders, Shortcuts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Module {
    #[default]
    Mail,
    Calendar,
    People,
    Tasks,
    Notes,
    Folders,
    Shortcuts,
}

/// Current selection in the mail list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub folder: FolderId,
    /// Selected items in list order; the focused one is `focus`.
    pub items: Vec<ItemId>,
    pub focus: Option<ItemId>,
}

/// UI-independent view state (persisted with the profile).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub module: Module,
    pub folder_pane: FolderPaneState,
    pub favorites_visible: bool,
    /// Global default; per-folder ViewSettings.reading_pane overrides.
    pub reading_pane: ReadingPanePos,
    /// Classic (false) or Simplified (true, default) ribbon.
    pub simplified_ribbon: bool,
    pub ribbon_collapsed: bool,
    pub status_bar: bool,
    /// Reading view (status bar button): hides pinned panes.
    pub reading_view: bool,
    /// Zoom for the reading pane, percent.
    pub zoom: u16,
    pub dark: bool,
    /// Focused Inbox: true = Focused tab, false = Other tab.
    pub focused_tab: bool,
    /// Active search query (empty = none) and its scope.
    pub search: String,
    pub search_scope: SearchScope,
    /// Collapsed date-group labels per folder.
    pub collapsed_groups: BTreeMap<FolderId, Vec<String>>,
    /// Main window maximized (Restore Down / Maximize).
    pub maximized: bool,
    /// Main window minimized.
    pub minimized: bool,
    /// Ribbon Display Options › Full-screen mode (ribbon and tabs hidden until clicked).
    pub ribbon_auto_hide: bool,
    /// Quick Access Toolbar command ids, in order.
    pub quick_access: Vec<String>,
    /// The last app opened from More Apps, pinned to the module rail above More Apps.
    pub pinned_app: Option<Module>,
}

/// Default Quick Access Toolbar: Send/Receive All Folders, Undo (`plan/outlook/layout.md`).
pub const DEFAULT_QUICK_ACCESS: [&str; 2] = ["sendReceive.all", "app.undo"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchScope {
    #[default]
    CurrentFolder,
    Subfolders,
    CurrentMailbox,
    AllMailboxes,
    AllOutlookItems,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            module: Module::Mail,
            folder_pane: FolderPaneState::Normal,
            favorites_visible: true,
            reading_pane: ReadingPanePos::Right,
            simplified_ribbon: true,
            ribbon_collapsed: false,
            status_bar: true,
            reading_view: false,
            zoom: 100,
            dark: false,
            focused_tab: true,
            search: String::new(),
            search_scope: SearchScope::CurrentFolder,
            collapsed_groups: BTreeMap::new(),
            maximized: true,
            minimized: false,
            ribbon_auto_hide: false,
            quick_access: DEFAULT_QUICK_ACCESS.iter().map(|s| (*s).to_owned()).collect(),
            pinned_app: None,
        }
    }
}

/// Things the UI should show after commands (alerts, info bars, dialogs requested by commands).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Notice {
    Info(String),
    Error(String),
    /// A command asks the UI to open a dialog by id with JSON params (e.g. "moveToFolder").
    OpenDialog(String, Value),
    /// A compose window was created/updated.
    Compose(u64),
    /// New Item Alert (rules).
    NewItemAlert(String, ItemId),
}

pub struct Session {
    pub mailbox: Mailbox,
    pub registry: Registry,
    pub selection: Selection,
    pub view: ViewState,
    pub undo: UndoStack,
    /// Open compose windows / inline responses by id.
    pub composes: BTreeMap<u64, ComposeState>,
    /// Notices queued for the UI (drained by the UI each frame).
    pub notices: Vec<Notice>,
    /// Clock override for tests and samples; `None` = system clock.
    pub fixed_now: Option<DateTime>,
    /// Local UTC offset in minutes used for display.
    pub tz_offset_min: i32,
    /// Persistent store (None = in-memory session).
    pub store: Option<mailcraft_store::Store>,
    /// Ops not yet written to the store.
    pub pending_ops: Vec<mailcraft_store::Op>,
    /// The user's own addresses (lower-case) for "me" rules and Reply All.
    pub me: Vec<String>,
    pub next_compose: u64,
    /// Items copied with Copy (Ctrl+C) in the message list, ready to be pasted into a folder.
    pub clipboard: Vec<ItemId>,
    /// Text copied from the reading pane.
    pub clipboard_text: Option<String>,
    /// Last folder shown in each module (restored when switching back).
    pub module_folders: Vec<(Module, FolderId)>,
    /// Journal size in bytes past which a write folds the journal into a fresh snapshot (0 = never). Undo,
    /// redo and view changes journal whole-mailbox `Replace` ops, so without this the journal would grow
    /// until the store refuses it.
    pub checkpoint_bytes: u64,
    /// Number of [`Session::journal`] calls so far (lets `run` tell whether a failed command journaled).
    journal_calls: u64,
}

/// Most ops kept in `pending_ops` while no store is attached; beyond that they collapse into one `Replace`.
const MAX_PENDING_OPS: usize = 2_000;

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("selection", &self.selection).field("view", &self.view).finish()
    }
}

impl Session {
    /// In-memory session over `mailbox` with the full registry; selects the first account's Inbox.
    pub fn new(mailbox: Mailbox) -> Session {
        let mut s = Session {
            mailbox,
            registry: crate::cmd::registry(),
            selection: Selection::default(),
            view: ViewState::default(),
            undo: UndoStack::default(),
            composes: BTreeMap::new(),
            notices: Vec::new(),
            fixed_now: None,
            tz_offset_min: 0,
            store: None,
            pending_ops: Vec::new(),
            me: Vec::new(),
            next_compose: 0,
            clipboard: Vec::new(),
            clipboard_text: None,
            module_folders: Vec::new(),
            checkpoint_bytes: mailcraft_store::Autosave::DEFAULT_CHECKPOINT_BYTES,
            journal_calls: 0,
        };
        s.refresh_me();
        s.select_default_folder();
        s
    }

    /// Opens (or creates) a persistent session in `dir`. A new data folder gets a local mailbox ("Outlook Data
    /// File") written as its first snapshot.
    pub fn open(dir: &std::path::Path) -> Result<Session, CmdError> {
        let mut store = mailcraft_store::Store::open(dir)
            .map_err(|e| CmdError::Failed(format!("cannot open the data folder {}: {e}", dir.display())))?;
        let mut mailbox = store
            .load()
            .map_err(|e| CmdError::Failed(format!("cannot read the data folder {}: {e}", dir.display())))?;
        if mailbox.accounts.is_empty() && mailbox.folders.is_empty() {
            mailbox = Mailbox::new_local(mailcraft_model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME);
            store.checkpoint(&mailbox).map_err(|e| {
                CmdError::Failed(format!("cannot write the data folder {}: {e}", dir.display()))
            })?;
        }
        let mut s = Session::new(mailbox);
        s.store = Some(store);
        Ok(s)
    }

    /// Recomputes [`Session::me`] from the accounts' addresses (lower-case, no duplicates).
    pub fn refresh_me(&mut self) {
        let mut me: Vec<String> = Vec::new();
        for a in &self.mailbox.accounts {
            let addr = a.address.trim().to_lowercase();
            if !addr.is_empty() && !me.contains(&addr) {
                me.push(addr);
            }
        }
        self.me = me;
    }

    /// Selects the default account's Inbox (or the first mail folder).
    pub fn select_default_folder(&mut self) {
        let mb = &self.mailbox;
        let inbox = mb.default_account().and_then(|a| mb.role_folder(a.id, FolderRole::Inbox));
        let any_inbox = || mb.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).map(|f| f.id);
        let any_mail = || mb.folders.values().find(|f| f.kind == FolderKind::Mail).map(|f| f.id);
        let any = || mb.folders.keys().next().copied();
        let folder = inbox.or_else(any_inbox).or_else(any_mail).or_else(any).unwrap_or_default();
        self.selection = Selection { folder, items: Vec::new(), focus: None };
    }

    /// Current time (fixed clock when set, else the system clock).
    pub fn now(&self) -> DateTime {
        self.fixed_now.unwrap_or_else(DateTime::now_utc)
    }

    /// Runs a command by id. Checks `enabled`, records undo for mutating commands, guards against panics
    /// (a panicking command returns `CmdError::Failed` and the mailbox is restored from the undo snapshot),
    /// flushes pending ops to the store.
    pub fn run(&mut self, id: &str, params: &Value) -> CmdResult {
        let Some(spec) = self.registry.get(id).cloned() else {
            return Err(CmdError::Unknown(id.to_owned()));
        };
        match catch_unwind(AssertUnwindSafe(|| (spec.enabled)(self))) {
            Ok(None) => {}
            Ok(Some(why)) => return Err(CmdError::Disabled(why.to_owned())),
            Err(payload) => {
                let msg = panic_text(payload.as_ref());
                log::error!("command `{id}` panicked in its enabled check: {msg}");
                return Err(CmdError::Failed(format!("command `{id}` panicked: {msg}")));
            }
        }
        // Undo/redo restore snapshots themselves (and journal a Replace); they must not record undo entries.
        let self_journaling = matches!(spec.id, "app.undo" | "app.redo");
        let snapshot = (spec.mutates && !self_journaling).then(|| self.mailbox.clone());
        let pending_before = self.pending_ops.len();
        let journal_calls_before = self.journal_calls;
        let result = match catch_unwind(AssertUnwindSafe(|| (spec.run)(self, params))) {
            Ok(r) => r,
            Err(payload) => {
                let msg = panic_text(payload.as_ref());
                log::error!("command `{id}` panicked: {msg}");
                Err(CmdError::Failed(format!("command `{id}` panicked: {msg}")))
            }
        };
        if let Some(before) = snapshot {
            if result.is_err() {
                self.mailbox = before;
                // Ops a failed command queued describe changes that were just rolled back.
                if self.journal_calls != journal_calls_before {
                    // It journaled: those ops may already be in the store (or a queued Replace may have
                    // superseded older ops), so record the rolled-back mailbox on top of them.
                    self.journal(vec![Op::Replace(Box::new(self.mailbox.clone()))]);
                } else if self.pending_ops.len() > pending_before {
                    self.pending_ops.truncate(pending_before);
                }
            } else {
                let ops = diff_ops(&before, &self.mailbox);
                if !ops.is_empty() {
                    self.undo.push(UndoEntry { label: spec.label.to_owned(), before: Box::new(before) });
                    self.journal(ops);
                }
            }
            self.refresh_me();
        }
        if self.pending_ops.len() > pending_before {
            // Ops a command queued directly (without a mailbox diff, e.g. view-only changes).
            self.flush_pending();
        }
        self.tidy_selection();
        result
    }

    /// Queues ops for the store and writes them when a store is attached (errors are logged; the command
    /// still succeeds and the ops stay queued for the next attempt).
    pub fn journal(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        self.journal_calls = self.journal_calls.wrapping_add(1);
        self.pending_ops.extend(ops);
        self.flush_pending();
    }

    /// Writes queued ops to the store (when attached). A queued `Replace` supersedes everything before it, so
    /// the queue stays small even without a store.
    pub fn flush_pending(&mut self) {
        if let Some(last_replace) = self.pending_ops.iter().rposition(|o| matches!(o, Op::Replace(_))) {
            self.pending_ops.drain(..last_replace);
        }
        if self.pending_ops.len() > MAX_PENDING_OPS {
            self.pending_ops = vec![Op::Replace(Box::new(self.mailbox.clone()))];
        }
        if let Some(store) = self.store.as_mut() {
            match store.append(&self.pending_ops) {
                Ok(()) => self.pending_ops.clear(),
                Err(e) => {
                    log::error!("could not write {} change(s) to the store: {e}", self.pending_ops.len())
                }
            }
            if self.pending_ops.is_empty()
                && self.checkpoint_bytes > 0
                && store.journal_bytes() >= self.checkpoint_bytes
            {
                // The journal now leads to `self.mailbox`; a snapshot of it supersedes the journal.
                if let Err(e) = store.checkpoint(&self.mailbox) {
                    log::error!("could not fold the journal into a snapshot: {e}");
                }
            }
        }
    }

    /// Undoes the last recorded command (restores its snapshot); returns its label.
    pub fn undo_last(&mut self) -> Option<String> {
        let entry = self.undo.undo.pop()?;
        let after = std::mem::replace(&mut self.mailbox, *entry.before);
        self.undo.push_redo(UndoEntry { label: entry.label.clone(), before: Box::new(after) });
        self.after_restore();
        Some(entry.label)
    }

    /// Redoes the last undone command; returns its label.
    pub fn redo_last(&mut self) -> Option<String> {
        let entry = self.undo.redo.pop()?;
        let before = std::mem::replace(&mut self.mailbox, *entry.before);
        self.undo.push_keep_redo(UndoEntry { label: entry.label.clone(), before: Box::new(before) });
        self.after_restore();
        Some(entry.label)
    }

    fn after_restore(&mut self) {
        self.journal(vec![Op::Replace(Box::new(self.mailbox.clone()))]);
        self.refresh_me();
        self.tidy_selection();
    }

    /// Drops selected items that no longer exist; reselects the default folder when the selected one is gone.
    pub fn tidy_selection(&mut self) {
        if !self.mailbox.folders.contains_key(&self.selection.folder) {
            self.select_default_folder();
            return;
        }
        let mb = &self.mailbox;
        // Selections hold mail in Mail and contacts/appointments/tasks/notes in the other modules.
        let exists = |id: &ItemId| {
            mb.mail.contains_key(id)
                || mb.contacts.contains_key(id)
                || mb.appointments.contains_key(id)
                || mb.tasks.contains_key(id)
                || mb.notes.contains_key(id)
        };
        self.selection.items.retain(|id| exists(id));
        if self.selection.focus.is_some_and(|f| !exists(&f)) {
            self.selection.focus = self.selection.items.first().copied();
        }
        let mail = &self.mailbox.mail;
        self.clipboard.retain(|id| mail.contains_key(id));
    }

    /// Item ids a command should act on: `params.ids` when present, else the selection (else the focus).
    pub fn target_ids(&self, params: &Value) -> Vec<ItemId> {
        if let Some(ids) = crate::cmd::util::opt_ids(params) {
            return ids;
        }
        if !self.selection.items.is_empty() {
            return self.selection.items.clone();
        }
        self.selection.focus.into_iter().collect()
    }

    /// The selected folder (`params.folder` overrides when present).
    pub fn target_folder(&self, params: &Value) -> FolderId {
        crate::cmd::util::opt_folder(params).unwrap_or(self.selection.folder)
    }

    /// Account of the selected folder (or the default account).
    pub fn current_account(&self) -> Option<mailcraft_model::AccountId> {
        self.mailbox
            .folder_account(self.selection.folder)
            .or_else(|| self.mailbox.default_account().map(|a| a.id))
    }

    /// Queues a notice for the UI.
    pub fn notify(&mut self, n: Notice) {
        self.notices.push(n);
    }

    /// Registers a compose state and returns its id (also queues `Notice::Compose`).
    pub fn open_compose(&mut self, mut c: ComposeState) -> u64 {
        self.next_compose = self.next_compose.saturating_add(1);
        c.id = self.next_compose;
        self.composes.insert(c.id, c);
        self.notices.push(Notice::Compose(self.next_compose));
        self.next_compose
    }

    /// Whether a command is currently enabled (`None` = enabled).
    pub fn enabled(&self, id: &str) -> Option<&'static str> {
        match self.registry.get(id) {
            Some(spec) => (spec.enabled)(self),
            None => Some("unknown command"),
        }
    }
}

/// Text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Store ops turning `before` into `after`: per-item and per-folder upserts/removals, or one `Replace` when
/// anything else changed (accounts, rules, categories, views, quick steps, calendar/contacts/tasks/notes).
pub(crate) fn diff_ops(before: &Mailbox, after: &Mailbox) -> Vec<Op> {
    let other_changed = before.accounts != after.accounts
        || before.categories != after.categories
        || before.rules != after.rules
        || before.quick_steps != after.quick_steps
        || before.views != after.views
        || before.appointments != after.appointments
        || before.contacts != after.contacts
        || before.tasks != after.tasks
        || before.notes != after.notes;
    if other_changed {
        return vec![Op::Replace(Box::new(after.clone()))];
    }
    let mut ops = Vec::new();
    for (id, f) in &after.folders {
        if before.folders.get(id) != Some(f) {
            ops.push(Op::PutFolder(Box::new(f.clone())));
        }
    }
    for (id, m) in &after.mail {
        if before.mail.get(id) != Some(m) {
            ops.push(Op::PutMail(Box::new(m.clone())));
        }
    }
    for id in before.mail.keys() {
        if !after.mail.contains_key(id) {
            ops.push(Op::RemoveMail(*id));
        }
    }
    for id in before.folders.keys() {
        if !after.folders.contains_key(id) {
            ops.push(Op::RemoveFolder(*id));
        }
    }
    ops
}
