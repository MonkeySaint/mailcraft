//! The session: mailbox + UI-independent state + undo + store.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::compose::{ComposeKind, ComposeState, SaveSentTo};
use crate::settings::{self, Settings, SettingsFile};
use crate::undo::{UndoEntry, UndoStack};
use crate::{CmdError, CmdResult, Registry};
use mailcraft_model::{
    DateTime, FolderId, FolderKind, FolderPaneState, FolderRole, ItemId, Mailbox, ReadingPanePos,
};
use mailcraft_store::{Op, StoreError};

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
    /// Send / Receive › Work Offline: no server connections; Outbox messages wait.
    pub work_offline: bool,
    /// Options › Advanced › Send/Receive: automatic send/receive every N minutes (0 = off).
    pub auto_send_receive_minutes: u32,
    /// Options › Advanced › "Send immediately when connected".
    pub send_immediately: bool,
    /// View › Layout › To-Do Bar: the parts shown (`view.todoBar`).
    pub todo_bar: TodoBar,
    /// Folders appointments, contacts, tasks and notes were last moved to, newest first (the Move menus
    /// offer them for a one-click move).
    pub recent_move_folders: Vec<FolderId>,
    /// People › Search People: recent searches, newest first.
    pub recent_people_searches: Vec<String>,
    /// Status bar › Unseen Items in View: per folder, the item-id high-water mark of the visit before the
    /// current one and of the current one (items above the first arrived since the folder was last opened).
    pub folder_visits: BTreeMap<FolderId, (u64, u64)>,
}

/// The parts of the To-Do Bar shown at the right of the main window (none = the bar is off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TodoBar {
    pub calendar: bool,
    pub people: bool,
    pub tasks: bool,
}

impl TodoBar {
    /// Whether any part shows.
    pub fn shown(self) -> bool {
        self.calendar || self.people || self.tasks
    }
}

impl ViewState {
    /// Whether the To-Do Bar is on screen: a part is pinned (View › To-Do Bar) and Reading View, which hides
    /// every pinned pane, is off. Normal View shows it again as it was.
    pub fn todo_bar_visible(&self) -> bool {
        self.todo_bar.shown() && !self.reading_view
    }
}

/// Most recent Search People texts kept.
pub const MAX_RECENT_PEOPLE_SEARCHES: usize = 10;

/// Most folders [`ViewState::recent_move_folders`] keeps.
pub const MAX_RECENT_MOVES: usize = 10;

/// Records `folder` as the newest Move target of a calendar, contact, task or note item.
pub(crate) fn note_recent_move(s: &mut Session, folder: FolderId) {
    let list = &mut s.view.recent_move_folders;
    list.retain(|f| *f != folder);
    list.insert(0, folder);
    list.truncate(MAX_RECENT_MOVES);
}

/// Recent Move targets of `kind` other than the current folder, newest first.
pub fn recent_move_folders(s: &Session, kind: FolderKind) -> Vec<FolderId> {
    s.view
        .recent_move_folders
        .iter()
        .copied()
        .filter(|f| *f != s.selection.folder && s.mailbox.folder(*f).is_some_and(|x| x.kind == kind))
        .collect()
}

/// Most folders whose visits [`ViewState::folder_visits`] remembers.
pub const MAX_FOLDER_VISITS: usize = 2_000;

/// Default Quick Access Toolbar: Send/Receive All Folders, Undo (`plan/reference/layout.md`).
pub const DEFAULT_QUICK_ACCESS: [&str; 2] = ["sendReceive.all", "app.undo"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchScope {
    #[default]
    CurrentFolder,
    Subfolders,
    CurrentMailbox,
    AllMailboxes,
    AllItems,
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
            work_offline: false,
            auto_send_receive_minutes: crate::sync::DEFAULT_AUTO_MINUTES,
            send_immediately: true,
            todo_bar: TodoBar::default(),
            recent_move_folders: Vec::new(),
            recent_people_searches: Vec::new(),
            folder_visits: BTreeMap::new(),
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
    /// Message bodies and sources (storage stage S5): on disk in the profile folder, in memory without one.
    /// Items refer to their content by key; read it with [`Session::body`] and [`Session::source`]
    /// (`content.rs`).
    pub bodies: mailcraft_store::BodyStore,
    /// Ops not yet written to the store.
    pub pending_ops: Vec<mailcraft_store::Op>,
    /// The user's own addresses (lower-case) for "me" rules and Reply All.
    pub me: Vec<String>,
    pub next_compose: u64,
    /// Items copied with Copy (Ctrl+C) in the message list, ready to be pasted into a folder.
    pub clipboard: Vec<ItemId>,
    /// Text copied from the reading pane.
    pub clipboard_text: Option<String>,
    /// The Clipboard pane's history (newest last): copies made in messages, the reading pane and, while
    /// MailCraft runs, other programs. Every open message's pane shows it.
    pub clip_history: Vec<crate::compose::ClipEntry>,
    /// Last folder shown in each module (restored when switching back).
    pub module_folders: Vec<(Module, FolderId)>,
    /// Journal size in bytes past which a write folds the journal into a fresh snapshot (0 = never).
    /// Changes to accounts, rules, categories and views still journal whole-mailbox `Replace` ops, so
    /// without this the journal would grow until the store refuses it.
    pub checkpoint_bytes: u64,
    /// Number of [`Session::journal`] calls so far (lets `run` tell whether a failed command journaled).
    journal_calls: u64,
    /// Goes up whenever the mailbox may have changed ([`Session::mailbox_revision`], `counts.rs`).
    pub(crate) revision: u64,
    /// Folder counts memoised per [`Session::mailbox_revision`] (`counts.rs`).
    pub(crate) counts: crate::counts::CountsMemo,
    /// What the session told the user about writes the store refused.
    store_guard: StoreGuard,
    /// Mutating commands running (one may run others); server changes are queued by the outermost one.
    run_depth: u32,
    /// Automatic actions running ([`Session::run_automatic`]): their changes record no undo step.
    automatic_depth: u32,
    /// The exit send/receive has run: the window close that follows File › Exit runs `file.exit` again (the
    /// final save), which must not start a second send/receive and a second wait.
    exit_sync_done: bool,
    /// Account passwords and tokens (never in the mailbox or its journal; see `accounts.rs`).
    pub secrets: Box<dyn crate::accounts::SecretStore>,
    /// Send/receive workers, queued results and the status bar state (see `sync.rs`).
    pub sync: crate::sync::SyncState,
    /// Search index, recent searches and search toggles (see `search_index.rs`).
    pub search: crate::search_index::SearchState,
    /// Profile settings: File › Options and the option dialogs (see `settings.rs`). Saved with the view state
    /// to `settings.json` in the profile folder after every command that changes them.
    pub settings: Settings,
    /// S/MIME: digital IDs, other people's certificates, trusted certificates and opened messages (see
    /// `smime.rs`).
    pub smime: crate::smime::SmimeSlot,
    /// Items deleted for good, kept for Recover Deleted Items (`recover.rs`).
    pub recover: crate::recover::RecoverState,
    /// Data files closed in a session without a store, ready to be opened again (`datafile.rs`).
    pub closed_data_files: Vec<crate::datafile::ClosedDataFile>,
    /// Set by a command whose effect reaches outside the mailbox where an undo snapshot cannot put it back
    /// (closing or opening a data file): the undo history is cleared instead of recording a step, since any
    /// older snapshot would bring back or drop that data file too.
    pub(crate) undo_barrier: bool,
    /// Set by a command that took data out of the profile which must not linger in its journal (closing a
    /// password-protected data file): once the command's changes are written, the journal is folded into a
    /// fresh snapshot.
    pub(crate) checkpoint_after: bool,
    /// Set with `checkpoint_after` by closing a protected data file: once the snapshot is written, the
    /// message sources nothing refers to any more are deleted from the store's blobs.
    pub(crate) purge_blobs_after: bool,
    /// What was last read from or written to each standalone data file's file (`datafile::Seen`), so a file
    /// something else wrote meanwhile is not overwritten and an unchanged one is not rewritten on exit.
    pub(crate) linked_seen: BTreeMap<mailcraft_model::AccountId, crate::datafile::Seen>,
    /// Quick Print may go straight to the system's default printer. The desktop app turns it on; tests and
    /// tools leave it off, so nothing reaches a real printer by accident.
    pub direct_print: bool,
    /// The machine translator an embedding app installed (opt-in: none by default, so no text leaves the
    /// computer), see [`crate::translate`].
    pub translator: Option<std::sync::Arc<dyn crate::translate::Translator>>,
    /// Translations of received messages shown in place of their bodies (`review.translate`).
    pub translations: BTreeMap<ItemId, crate::translate::Translation>,
    /// Zoom of read windows zoomed on their own (message → percent; the others use `view.zoom`).
    pub read_zoom: BTreeMap<ItemId, u16>,
    /// Messages open in read windows (kept up to date by the front end): the offline window keeps their
    /// bodies.
    pub open_messages: std::collections::BTreeSet<ItemId>,
    /// Messages Options › Language › Always translate already tried to translate this session.
    pub translate_tried: std::collections::BTreeSet<ItemId>,
    /// Connection settings for the translation server (`None` = its defaults; tests trust a local server).
    #[cfg(not(target_arch = "wasm32"))]
    pub translator_options: Option<mailcraft_net::ConnectOptions>,
    /// Translations (and the Options translation test) run on a worker thread and are applied when
    /// [`Session::poll_sync`] sees them finish, so a slow server never holds up the caller. The desktop app
    /// turns it on; tools and tests leave it off and get the result from the command itself.
    pub background_translate: bool,
    /// Translations running on worker threads (see [`crate::translate`]).
    pub(crate) translate_jobs: crate::translate::Jobs,
    /// Pages View in Browser wrote this session (deleted with their message and when the session ends).
    pub browser_pages: crate::browser::BrowserPages,
    /// Profile folder holding `settings.json` (`None` = in-memory session: nothing is saved).
    settings_dir: Option<std::path::PathBuf>,
    /// Bytes last read from or written to `settings.json` (identical states are not rewritten).
    settings_saved: Option<Vec<u8>>,
    /// The command whose enabled check is running names its items in its params (`id` / `ids`), see
    /// [`Session::params_name_items`].
    params_name_items: bool,
    /// While an enabled check runs for items other than the selection (the items a command's params name, or
    /// a read window's message, [`Session::enabled_for`]): those items. [`Session::target_ids`] answers
    /// with them, so "is something selected?" checks look at the items the command will act on.
    check_scope: std::sync::Mutex<Option<Vec<ItemId>>>,
    /// Body tests of the message list's filters, remembered per item and content ([`Session::body_test`]).
    pub(crate) body_tests: std::sync::Mutex<crate::content::BodyTests>,
}

/// Clears [`Session::check_scope`] when dropped (also when an enabled check panics).
struct ScopeGuard<'a>(&'a std::sync::Mutex<Option<Vec<ItemId>>>);

impl Drop for ScopeGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Longest wait for "Perform an automatic send/receive when exiting" (seconds).
pub const EXIT_SYNC_SECS: u64 = 30;

/// Most ops kept in `pending_ops` while no store is attached; beyond that they collapse into one `Replace`.
const MAX_PENDING_OPS: usize = 2_000;

/// Told once when the store refuses a snapshot because the mailbox has grown past what it can read back.
pub const SNAPSHOT_TOO_LARGE: &str = "Your mailbox has grown too large for MailCraft to save it as one file \
    (the limit is 1 GB). Your changes are still being saved as a list of changes, but that list has a limit \
    too: delete or archive mail you no longer need, and empty Deleted Items, to make room.";

/// Told once when the store can take neither the latest changes nor a snapshot holding them.
pub const UNSAVED_TOO_LARGE: &str = "Your latest changes could not be saved: your mailbox has grown past the \
    largest size MailCraft can store (1 GB). Delete or archive mail you no longer need, and empty Deleted \
    Items; MailCraft saves the changes as soon as there is room.";

/// Bookkeeping for the store's guard rails (issue #25).
#[derive(Debug, Default)]
struct StoreGuard {
    /// The user was told the store refused a write for size; cleared once a write of that kind succeeds.
    snapshot_refused: bool,
    journal_refused: bool,
    /// After a refused snapshot, the journal size at which folding it is tried again (a snapshot of a
    /// mailbox that is too large is not attempted on every write).
    checkpoint_retry_at: u64,
    /// While the journal and the fallback snapshot both refuse ([`StoreGuard::journal_refused`]): the
    /// mailbox's estimated size ([`Mailbox::approx_bytes`]) when the snapshot was last refused, and the
    /// flushes since. The snapshot (a clone of the mailbox and a write up to the cap) is tried again only once
    /// the mailbox shrank or after [`FALLBACK_RETRY_FLUSHES`] flushes, not on every command.
    fallback_refused_bytes: usize,
    fallback_skips: u32,
}

/// Flushes after which a refused fallback snapshot is tried again even if the mailbox did not shrink.
const FALLBACK_RETRY_FLUSHES: u32 = 256;

impl StoreGuard {
    /// Whether to try the fallback snapshot now (see [`StoreGuard::fallback_refused_bytes`]).
    fn fallback_due(&mut self, mailbox: &Mailbox) -> bool {
        if !self.journal_refused {
            return true;
        }
        self.fallback_skips = self.fallback_skips.saturating_add(1);
        if self.fallback_skips >= FALLBACK_RETRY_FLUSHES {
            return true;
        }
        // Shrunk by at least 1/256 since the last refusal (deleting one message rarely makes room).
        let floor = self.fallback_refused_bytes.saturating_sub(self.fallback_refused_bytes / 256);
        mailbox.approx_bytes() < floor
    }
}

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
            bodies: mailcraft_store::BodyStore::in_memory(),
            pending_ops: Vec::new(),
            me: Vec::new(),
            next_compose: 0,
            clipboard: Vec::new(),
            clipboard_text: None,
            clip_history: Vec::new(),
            module_folders: Vec::new(),
            checkpoint_bytes: mailcraft_store::Autosave::DEFAULT_CHECKPOINT_BYTES,
            journal_calls: 0,
            revision: 0,
            counts: crate::counts::CountsMemo::default(),
            store_guard: StoreGuard::default(),
            run_depth: 0,
            automatic_depth: 0,
            exit_sync_done: false,
            secrets: Box::new(crate::accounts::MemorySecretStore::default()),
            sync: crate::sync::SyncState::default(),
            search: crate::search_index::SearchState::default(),
            smime: crate::smime::SmimeSlot::default(),
            settings: Settings::default(),
            recover: crate::recover::RecoverState::default(),
            closed_data_files: Vec::new(),
            undo_barrier: false,
            checkpoint_after: false,
            purge_blobs_after: false,
            linked_seen: BTreeMap::new(),
            direct_print: false,
            translator: None,
            translations: BTreeMap::new(),
            translate_tried: std::collections::BTreeSet::new(),
            read_zoom: BTreeMap::new(),
            open_messages: std::collections::BTreeSet::new(),
            #[cfg(not(target_arch = "wasm32"))]
            translator_options: None,
            background_translate: false,
            translate_jobs: crate::translate::Jobs::default(),
            browser_pages: crate::browser::BrowserPages::default(),
            settings_dir: None,
            settings_saved: None,
            params_name_items: false,
            check_scope: std::sync::Mutex::new(None),
            body_tests: std::sync::Mutex::new(crate::content::BodyTests::default()),
        };
        s.refresh_me();
        s.select_default_folder();
        s
    }

    /// Opens (or creates) a persistent session in `dir`. A new data folder gets a local mailbox ("Local
    /// Folders") written as its first snapshot, in the storage `MAILCRAFT_STORE` names (legacy unless it says
    /// `sqlite`; see [`Session::open_with`]).
    pub fn open(dir: &std::path::Path) -> Result<Session, CmdError> {
        Session::open_with(dir, mailcraft_store::StoreKind::from_env())
    }

    /// [`Session::open`] with the storage a new profile gets. An existing profile keeps the storage it has;
    /// asking for SQLite on a legacy profile says so in a notice (moving a profile over comes with storage
    /// stage S9).
    pub fn open_with(
        dir: &std::path::Path,
        new_kind: mailcraft_store::StoreKind,
    ) -> Result<Session, CmdError> {
        let mut store = mailcraft_store::Store::open_with(dir, new_kind)
            .map_err(|e| CmdError::Failed(format!("cannot open the data folder {}: {e}", dir.display())))?;
        let (mut mailbox, report) = store
            .load_report()
            .map_err(|e| CmdError::Failed(format!("cannot read the data folder {}: {e}", dir.display())))?;
        if report.legacy_content {
            // Written before storage stage S5: the content it carried inline is in the body store now; a
            // fresh snapshot drops it from the files (and keeps the next open from reading it again).
            if let Err(e) = store.checkpoint(&mailbox) {
                log::error!("could not write the snapshot without inline message content: {e}");
            }
        }
        // Junk lists from disk may be unsorted, duplicated or over the cap (older or hand-edited files);
        // adding to a list relies on it being sorted.
        mailbox.junk.sanitize();
        mailbox.rename_legacy_local_account();
        mailbox.drop_legacy_create_new_step();
        if mailbox.accounts.is_empty() && mailbox.folders.is_empty() {
            mailbox = Mailbox::new_local(mailcraft_model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME);
            store.checkpoint(&mailbox).map_err(|e| {
                CmdError::Failed(format!("cannot write the data folder {}: {e}", dir.display()))
            })?;
        }
        let kind = store.kind();
        let mut s = Session::new(mailbox);
        s.bodies = store.body_store();
        s.store = Some(store);
        log::info!("profile {}: {} storage", dir.display(), kind.name());
        if kind == mailcraft_store::StoreKind::Legacy && new_kind == mailcraft_store::StoreKind::Sqlite {
            s.notify(Notice::Info(
                "This profile keeps its current storage: SQLite storage is only used for new profiles so far."
                    .into(),
            ));
        }
        // View in Browser pages a crashed session left behind (the profile's lock is held now, so no other
        // session is using them).
        let swept = crate::browser::sweep_leftovers(&crate::browser::profile_base(dir));
        if swept > 0 {
            log::info!("View in Browser: removed {swept} page folder(s) left by an earlier session");
        }
        // Secrets live next to the data file, outside the mailbox snapshot and journal.
        match crate::accounts::FileSecretStore::open(dir) {
            Ok(st) => s.secrets = Box::new(st),
            Err(e) => {
                log::error!("account secrets unavailable: {e}");
                s.notify(Notice::Error(format!(
                    "Saved account passwords could not be read ({e}). Enter them again in Account Settings."
                )));
            }
        }
        s.load_settings(dir);
        crate::recover::load(&mut s);
        // Content nothing refers to any more: earlier versions of drafts saved again and of edited bodies,
        // rolled-back imports, content whose undo steps went with the last session. The history is empty and
        // the bin is loaded, so everything still in use is known.
        s.collect_content();
        crate::smime::load(&mut s);
        crate::datafile::note_linked_files(&mut s);
        // A named calendar time zone (Calendar › Time Zones) sets the display offset for today's DST rules.
        crate::cmd::calendar::views::refresh_time_zone(&mut s);
        Ok(s)
    }

    // -----------------------------------------------------------------------------------------------------
    // Settings

    /// Loads `settings.json` from the profile folder `dir` (options, view state, recent searches), applies
    /// them, opens the start-up folder and saves later changes there. A missing file means defaults; an
    /// unreadable one is kept aside as `settings.json.bad` and defaults are used.
    pub fn load_settings(&mut self, dir: &std::path::Path) {
        self.settings_dir = Some(dir.to_path_buf());
        match SettingsFile::read(dir) {
            Ok(None) => {}
            Ok(Some(f)) => {
                let (loaded, dropped) = Settings::from_json(&f.settings);
                for d in dropped {
                    log::warn!("settings.json: ignored {d}");
                }
                self.settings = loaded;
                if let Ok(view) = serde_json::from_value::<ViewState>(f.view) {
                    self.view = self.sanitize_view(view);
                }
                self.search.recent = f
                    .recent_searches
                    .into_iter()
                    .filter(|q| !q.trim().is_empty() && q.chars().count() <= 512)
                    .take(crate::search_index::MAX_RECENT)
                    .collect();
            }
            Err(e) => {
                log::error!("settings.json unreadable: {e}");
                let bad = dir.join(format!("{}.bad", settings::FILE_NAME));
                let _ = std::fs::rename(dir.join(settings::FILE_NAME), bad);
                // The translation server address is lost with the settings: so is the key kept for it.
                if let Err(e) = self.secrets.remove(crate::translate::KEY_SECRET) {
                    log::error!("the translation server's API key could not be removed: {e}");
                }
                self.notify(Notice::Error(format!(
                    "Your settings could not be read ({e}); MailCraft started with the default settings."
                )));
            }
        }
        self.apply_settings();
        self.select_start_folder();
        crate::cmd::nav::note_visit(self, self.selection.folder);
        crate::cmd::view::opened(self, self.selection.folder);
        self.settings_saved = self.settings_bytes();
        if self.migrate_view_state() {
            // The moved entries leave settings.json right away, so an older copy never brings them back.
            if let Err(e) = self.save_settings() {
                log::error!("{e}");
            }
        }
    }

    /// Moves what MailCraft 0.1 kept in the view state onto the items: note icon positions
    /// (`Note::x` / `Note::y`) and pinned messages (`MailItem::pinned`). The mailbox change is journaled
    /// (no undo step). Returns whether the old entries are gone from the view state, i.e. it had some and the
    /// change reached the store. When the store write fails the entries stay (in memory, so settings.json
    /// keeps them too) and the next start moves them again; moving is idempotent.
    pub fn migrate_view_state(&mut self) -> bool {
        let legacy = |e: &String| e.starts_with("\u{1}notes.pos=") || e.starts_with("\u{1}mail.pinned=");
        if !self.view.collapsed_groups.values().any(|list| list.iter().any(legacy)) {
            return false;
        }
        let entries = self.view.collapsed_groups.clone();
        let notes = crate::cmd::notes::migrate_view_positions(self);
        let pins = crate::cmd::view::mail::migrate_view_pins(self);
        if notes > 0 || pins > 0 {
            log::info!("moved {notes} note position(s) and {pins} pin(s) out of the view state");
            self.journal(vec![Op::Replace(Box::new(self.mailbox.clone()))]);
            if self.store.is_some() && !self.pending_ops.is_empty() {
                log::warn!("the moved note positions and pins are not saved yet; keeping the old entries");
                self.view.collapsed_groups = entries;
                return false;
            }
        }
        true
    }

    /// View state as loaded from disk, made safe: zoom in range, known Quick Access commands only, no active
    /// search, not minimised, online (Work Offline is per session), Mail module (the start-up folder decides
    /// where the window opens).
    fn sanitize_view(&self, mut v: ViewState) -> ViewState {
        if v.zoom != 0 && !(10..=500).contains(&v.zoom) {
            v.zoom = 100;
        }
        v.quick_access.retain(|id| self.registry.get(id).is_some());
        v.quick_access.truncate(64);
        v.quick_access.dedup();
        v.collapsed_groups.retain(|f, labels| self.mailbox.folders.contains_key(f) && labels.len() <= 64);
        v.folder_visits.retain(|f, _| self.mailbox.folders.contains_key(f));
        v.recent_move_folders.retain(|f| self.mailbox.folders.contains_key(f));
        v.recent_move_folders.truncate(MAX_RECENT_MOVES);
        v.recent_people_searches.retain(|q| !q.trim().is_empty() && q.chars().count() <= 256);
        v.recent_people_searches.truncate(MAX_RECENT_PEOPLE_SEARCHES);
        while v.folder_visits.len() > MAX_FOLDER_VISITS {
            v.folder_visits.pop_first();
        }
        v.search.clear();
        v.minimized = false;
        v.work_offline = false;
        v.module = Module::Mail;
        if v.auto_send_receive_minutes > 1440 {
            v.auto_send_receive_minutes = crate::sync::DEFAULT_AUTO_MINUTES;
        }
        v
    }

    /// Copies settings that the session holds elsewhere (view state, search, time zone) into place.
    pub fn apply_settings(&mut self) {
        for key in MIRRORED {
            self.apply_setting(key);
        }
    }

    /// Applies one setting the session also holds elsewhere (no-op for the others).
    pub fn apply_setting(&mut self, key: &str) {
        let st = &self.settings;
        match key {
            "advanced.sendImmediately" => self.view.send_immediately = st.bool(key),
            "advanced.sendReceiveMinutes" => {
                self.view.auto_send_receive_minutes =
                    u32::try_from(st.int(key)).unwrap_or(crate::sync::DEFAULT_AUTO_MINUTES);
            }
            "general.theme" => match st.text(key).as_str() {
                "dark" => self.view.dark = true,
                "light" => self.view.dark = false,
                _ => {}
            },
            "search.scope" => {
                let name = st.text(key);
                if let Some((sc, _)) = SCOPE_NAMES.iter().find(|(_, n)| *n == name) {
                    self.view.search_scope = *sc;
                } else if name == INBOX_MAILBOX_SCOPE {
                    self.view.search_scope = self.inbox_rule_scope(self.selection.folder);
                }
            }
            "search.includeDeleted" => self.search.include_deleted = st.bool(key),
            "calendar.timeZone" => self.tz_offset_min = st.time_zone_offset_min(),
            crate::recover::DAYS_KEY => crate::recover::days_changed(self),
            _ => {}
        }
    }

    /// Copies session state that mirrors a setting back into the settings (commands such as Work Offline,
    /// Change Time Zone or the dark-mode toggle change the session directly).
    fn sync_settings_from_session(&mut self) {
        let mut scope = SCOPE_NAMES
            .iter()
            .find(|(sc, _)| *sc == self.view.search_scope)
            .map_or("currentFolder", |(_, n)| *n);
        // "Current folder, current mailbox when searching from the Inbox" picks one of the two by folder: the
        // pick is not a new choice.
        if self.settings.text("search.scope") == INBOX_MAILBOX_SCOPE
            && matches!(self.view.search_scope, SearchScope::CurrentFolder | SearchScope::CurrentMailbox)
        {
            scope = INBOX_MAILBOX_SCOPE;
        }
        let pairs = [
            ("advanced.sendImmediately", json!(self.view.send_immediately)),
            ("advanced.sendReceiveMinutes", json!(self.view.auto_send_receive_minutes)),
            ("search.scope", json!(scope)),
            ("search.includeDeleted", json!(self.search.include_deleted)),
            ("calendar.timeZone", json!(self.tz_offset_min)),
        ];
        for (k, v) in pairs {
            if self.settings.get(k).as_ref() != Some(&v) {
                // Out-of-range session values (none today) simply stay unsaved.
                let _ = self.settings.set(k, &v);
            }
        }
        let theme = self.settings.text("general.theme");
        if theme != "system" && (theme == "dark") != self.view.dark {
            let _ = self.settings.set("general.theme", &json!(if self.view.dark { "dark" } else { "light" }));
        }
    }

    /// Opens Options › Advanced › "Start MailCraft in this folder" (a folder path, or a folder name), else
    /// keeps the default Inbox.
    pub fn select_start_folder(&mut self) {
        let want = self.settings.text("advanced.startFolder");
        let want = want.trim().trim_matches('\\');
        if want.is_empty() {
            return;
        }
        let mb = &self.mailbox;
        let by_path = mb.folders.keys().copied().find(|f| mb.folder_path(*f).eq_ignore_ascii_case(want));
        let by_name = || mb.folders.values().find(|f| f.name.eq_ignore_ascii_case(want)).map(|f| f.id);
        if let Some(folder) = by_path.or_else(by_name) {
            if let Some(kind) = mb.folder(folder).map(|f| f.kind) {
                self.view.module = crate::cmd::nav::module_for_kind(kind);
            }
            self.selection = Selection { folder, items: Vec::new(), focus: None };
        }
    }

    /// Serialised settings file for the current state (`None` for in-memory sessions).
    fn settings_bytes(&self) -> Option<Vec<u8>> {
        self.settings_dir.as_ref()?;
        let mut view = self.view.clone();
        view.search.clear();
        view.minimized = false;
        let view = serde_json::to_value(&view).unwrap_or(Value::Null);
        SettingsFile::new(&self.settings, view, &self.search.recent).to_bytes().ok()
    }

    /// Writes `settings.json` when the settings or the persisted view state changed since the last write.
    pub fn save_settings(&mut self) -> Result<(), CmdError> {
        let Some(dir) = self.settings_dir.clone() else { return Ok(()) };
        let Some(bytes) = self.settings_bytes() else { return Ok(()) };
        if self.settings_saved.as_ref() == Some(&bytes) {
            return Ok(());
        }
        SettingsFile::write_bytes(&dir, &bytes).map_err(CmdError::Failed)?;
        self.settings_saved = Some(bytes);
        Ok(())
    }

    /// Changes a setting (validated) and saves; view-backed keys go through `options.set` rules.
    pub fn set_option(&mut self, key: &str, value: &Value) -> Result<Value, CmdError> {
        crate::cmd::options::set_option(self, key, value)
    }

    /// Current value of a setting, `None` for unknown keys.
    pub fn option(&self, key: &str) -> Option<Value> {
        crate::cmd::options::option_value(self, key)
    }

    /// Fills in a new compose state from the settings: Bcc / From boxes, and for new messages the format, font,
    /// importance, sensitivity, receipts, Sent Items copy and proofing language; replies and forwards open in
    /// their own window when the option says so.
    fn apply_compose_defaults(&self, c: &mut ComposeState) {
        let st = &self.settings;
        c.show_bcc |= st.bool("mail.compose.showBcc");
        c.show_from |= st.bool("mail.compose.showFrom");
        if c.kind == ComposeKind::Draft {
            return;
        }
        c.request_delivery_receipt |= st.bool("mail.tracking.requestDelivery");
        c.request_read_receipt |= st.bool("mail.tracking.requestRead");
        // Trust Center › Email Security: Sign / Encrypt by default; replies to encrypted mail stay encrypted.
        crate::smime::compose_defaults(self, c);
        if c.save_sent == SaveSentTo::Default && !st.bool("mail.save.sentCopies") {
            c.save_sent = SaveSentTo::DoNotSave;
        }
        match c.kind {
            ComposeKind::Reply | ComposeKind::ReplyAll | ComposeKind::Forward => {
                if c.inline && st.bool("mail.reply.newWindow") {
                    c.inline = false;
                }
            }
            ComposeKind::New if c.source.is_none() && c.draft.is_none() => {
                if c.format == mailcraft_model::BodyFormat::Html && !c.format_explicit {
                    c.format = st.compose_format();
                }
                // Personal Stationery: a new HTML message without a template or stationery of its own.
                let unstyled =
                    c.theme == crate::compose::ThemeState::default() && c.body.background.is_none();
                if c.format == mailcraft_model::BodyFormat::Html
                    && unstyled
                    && !c.dirty
                    && let Some(stationery) = crate::compose::stationery(&st.text("mail.compose.stationery"))
                {
                    c.apply_stationery(stationery);
                }
                if c.importance == mailcraft_model::Importance::Normal {
                    c.importance = st.default_importance();
                }
                if c.sensitivity == mailcraft_model::Sensitivity::Normal {
                    c.sensitivity = st.default_sensitivity();
                }
                let family = st.text("mail.compose.fontFamily");
                if c.body.base.font_family.is_none() && !family.trim().is_empty() {
                    c.body.base.font_family = Some(family.trim().to_owned());
                }
                let size = st.int("mail.compose.fontSizePt");
                if c.body.base.size_pt.is_none() && size > 0 {
                    c.body.base.size_pt = Some(size as f32);
                }
                let lang = st.text("language.authoring");
                if c.language.is_none() && lang != "en-US" && !lang.is_empty() {
                    c.language = Some(lang);
                }
                // Change Styles › Set as Default.
                if c.style_set.is_none() && c.paragraph_spacing_preset.is_none() {
                    crate::cmd::format::apply_default_style_set(self, c);
                }
            }
            _ => {}
        }
    }

    /// Options › Mail › "Warn me when I send a message that may be missing an attachment": a message that
    /// mentions an attachment but has none asks first (the confirm dialog sends with
    /// `"attachmentReminder": false`). `None` = send as usual.
    fn attachment_reminder(&mut self, params: &Value) -> Option<CmdResult> {
        if !self.settings.bool("mail.send.attachmentReminder")
            || params.get("attachmentReminder").and_then(Value::as_bool) == Some(false)
        {
            return None;
        }
        let cid = crate::cmd::compose::compose_id(self, params).ok()?;
        let c = self.composes.get(&cid)?;
        if !c.attachments.is_empty() {
            return None;
        }
        let body = c.body.to_plain_text();
        let mentions = match c.kind {
            ComposeKind::Reply | ComposeKind::ReplyAll | ComposeKind::Forward => {
                settings::mentions_attachment(settings::new_text_of_reply(&body))
            }
            ComposeKind::New | ComposeKind::Draft => {
                settings::mentions_attachment(&c.subject) || settings::mentions_attachment(&body)
            }
        };
        if !mentions {
            return None;
        }
        self.notify(Notice::OpenDialog(
            "confirm".into(),
            json!({
                "title": "Attachment Reminder",
                "text": "You may have forgotten to attach a file. Send anyway?",
                "command": "compose.send",
                "params": {"compose": cid, "attachmentReminder": false},
            }),
        ));
        Some(Ok(json!({"sent": false, "compose": cid, "dialog": "confirm", "reason": "attachmentReminder"})))
    }

    /// Options › Advanced › Send and receive › "Perform an automatic send/receive when exiting": one
    /// send/receive of the All Accounts group (Outboxes sent, folders updated) before the exit save, waited for
    /// at most [`EXIT_SYNC_SECS`] (then the workers are asked to stop). Nothing happens offline or without
    /// server accounts, and it runs once per session however often `file.exit` runs.
    fn send_receive_on_exit(&mut self) {
        if self.exit_sync_done || self.view.work_offline {
            return;
        }
        crate::accounts::load_prefs(self);
        let all = self.settings.bool("advanced.sendReceiveOnExit");
        let accounts = if all { crate::sync::group_accounts(self) } else { Vec::new() };
        // User-defined groups set to "Perform an automatic send/receive when exiting".
        let groups = crate::sync::groups::exit_groups(self);
        if accounts.is_empty() && groups.is_empty() {
            return;
        }
        self.exit_sync_done = true;
        let mut started = false;
        if !accounts.is_empty() {
            let plan = crate::sync::Plan { user: false, ..crate::sync::Plan::all() };
            match self.send_receive(&accounts, plan) {
                Ok(_) => started = true,
                Err(e) => log::warn!("send/receive on exit: {e}"),
            }
        }
        for g in groups {
            match crate::sync::groups::run(self, &g, false, false) {
                Ok(_) => started = true,
                Err(e) => log::warn!("send/receive of group {} on exit: {e}", g.name),
            }
        }
        if started && !self.wait_for_sync(std::time::Duration::from_secs(EXIT_SYNC_SECS)) {
            log::warn!("send/receive on exit did not finish in {EXIT_SYNC_SECS} s");
            self.cancel_sync();
        }
    }

    /// Options › Advanced › "Empty Deleted Items folders when exiting": empties every Deleted Items folder
    /// (items and subfolders) and journals the change before the exit save.
    fn empty_deleted_on_exit(&mut self) {
        if !self.settings.bool("advanced.emptyDeletedOnExit") {
            return;
        }
        let recording = self.begin_changes();
        let deleted: Vec<FolderId> = self
            .mailbox
            .folders
            .values()
            .filter(|f| f.role == Some(FolderRole::DeletedItems))
            .map(|f| f.id)
            .collect();
        for d in deleted {
            let items: Vec<ItemId> = self.mailbox.items_in(d).iter().map(|m| m.id).collect();
            if let Err(e) = self.mailbox.delete_items(&items) {
                log::error!("emptying Deleted Items on exit: {e}");
            }
            let subs: Vec<FolderId> =
                self.mailbox.folders.values().filter(|f| f.parent == Some(d)).map(|f| f.id).collect();
            for f in subs {
                if let Err(e) = self.mailbox.delete_folder(f) {
                    log::error!("emptying Deleted Items on exit: {e}");
                }
            }
        }
        let mut changes = self.end_changes(recording);
        let ops = changes.ops(&self.mailbox);
        if !ops.is_empty() {
            crate::recover::capture(self, &changes);
            self.record_undo("Empty Deleted Items", changes);
            self.journal(ops);
            self.tidy_selection();
        }
    }

    /// Drops change records left open on the mail table when nothing that could close them is running (no
    /// command, no undo; called as the outermost command starts and as send/receive results are applied). A
    /// record is only left open by a panic between opening and closing it with no enclosing record to fold it
    /// into (inside undo's adjustments, or queueing server changes); kept, it would copy every item written
    /// for the rest of the session.
    pub(crate) fn drop_stray_records(&mut self) {
        if self.run_depth > 0 || !self.mailbox.mail.is_recording() {
            return;
        }
        let n = self.mailbox.mail.abandon_all();
        log::error!("dropped {n} change record(s) left open by an earlier failure");
    }

    /// Records a fresh undo step from a command's changes. A change set whose mail record was lost cannot be
    /// undone: the history is cleared instead (an older step would not fit the mailbox any more).
    pub(crate) fn record_undo(&mut self, label: &str, changes: crate::changes::ChangeSet) {
        match UndoEntry::from_changes(label, changes) {
            Some(entry) => self.undo.push(entry),
            None => self.undo.clear(),
        }
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
    /// (a panicking command returns `CmdError::Failed` and the mailbox is put back from its change record),
    /// flushes pending ops to the store.
    pub fn run(&mut self, id: &str, params: &Value) -> CmdResult {
        let Some(spec) = self.registry.get(id).cloned() else {
            return Err(CmdError::Unknown(id.to_owned()));
        };
        self.drop_stray_records();
        let named = std::mem::replace(&mut self.params_name_items, names_items(params));
        let scope = self.named_items(params);
        let enabled = catch_unwind(AssertUnwindSafe(|| match scope {
            Some(items) => self.enabled_for(id, &items),
            None => (spec.enabled)(self),
        }));
        self.params_name_items = named;
        match enabled {
            Ok(None) => {}
            Ok(Some(why)) => return Err(CmdError::Disabled(why.to_owned())),
            Err(payload) => {
                let msg = panic_text(payload.as_ref());
                log::error!("command `{id}` panicked in its enabled check: {msg}");
                return Err(CmdError::Failed(format!("command `{id}` panicked: {msg}")));
            }
        }
        // Options that act around commands (see `settings.rs`).
        if id == "compose.send"
            && let Some(r) = self.attachment_reminder(params)
        {
            return r;
        }
        if id == "file.exit" {
            self.send_receive_on_exit();
            self.empty_deleted_on_exit();
        }
        let read_on_delete = (id == "mail.delete" && self.settings.bool("mail.other.markReadOnDelete"))
            .then(|| self.target_ids(params));
        // Undo/redo restore their steps and journal them themselves; they must not record undo entries.
        let self_journaling = matches!(spec.id, "app.undo" | "app.redo");
        // What the command changes is recorded (`changes.rs`): no copy of the whole mailbox.
        let recording = (spec.mutates && !self_journaling).then(|| self.begin_changes());
        let pending_before = self.pending_ops.len();
        let journal_calls_before = self.journal_calls;
        let counted = u32::from(recording.is_some());
        self.run_depth = self.run_depth.saturating_add(counted);
        let result = catch_unwind(AssertUnwindSafe(|| (spec.run)(self, params)));
        self.run_depth = self.run_depth.saturating_sub(counted);
        let result = match result {
            Ok(r) => r,
            Err(payload) => {
                let msg = panic_text(payload.as_ref());
                log::error!("command `{id}` panicked: {msg}");
                Err(CmdError::Failed(format!("command `{id}` panicked: {msg}")))
            }
        };
        // A change that takes header-only messages off their server would expunge the only full copy: it is
        // refused (rolled back below) and their download starts, so the same command works once they arrive.
        let mut stranded = Vec::new();
        let result = match (&result, &recording) {
            (Ok(_), Some(r)) if self.run_depth == 0 => {
                stranded = crate::sync::header_only_leaving(self, r);
                if stranded.is_empty() { result } else { Err(CmdError::Failed(String::new())) }
            }
            _ => result,
        };
        if let (Some(ids), Ok(_)) = (read_on_delete, &result) {
            // Options › Mail › "Mark messages as read when deleted" (part of the delete's undo step).
            self.mailbox.set_read(&ids, true);
        }
        if let Ok(v) = &result {
            // Showing Bcc / From in one message keeps them shown in the next ones.
            for (cmd, field, key) in [
                ("compose.showBcc", "showBcc", "mail.compose.showBcc"),
                ("compose.showFrom", "showFrom", "mail.compose.showFrom"),
            ] {
                if id == cmd
                    && let Some(on) = v.get(field).and_then(Value::as_bool)
                {
                    let _ = self.settings.set(key, &json!(on));
                }
            }
        }
        if let Some(recording) = recording {
            // The command may have changed the mailbox (or it is put back below): memoised views are stale.
            self.mailbox_changed();
            if result.is_err() {
                let back = self.rollback_changes(recording);
                // Ops a failed command queued describe changes that were just rolled back.
                if back.mail.is_none() {
                    // Nothing could be rolled back; the whole mailbox was journaled as it is.
                } else if self.journal_calls != journal_calls_before {
                    // It journaled: those ops may already be in the store (or a queued Replace may have
                    // superseded older ops), and it may have changed an item again after journaling it, so
                    // record the whole rolled-back mailbox on top of them (rare: a command that journals and
                    // then fails).
                    self.journal(vec![Op::Replace(Box::new(self.mailbox.clone()))]);
                } else if self.pending_ops.len() > pending_before {
                    self.pending_ops.truncate(pending_before);
                }
            } else {
                let mut changes = self.end_changes(recording);
                // Local changes to synchronised folders become queued server changes (adjusting the
                // mailbox, so this comes before the ops that journal it).
                if self.run_depth == 0 {
                    crate::sync::queue_local_changes(self, &mut changes);
                }
                let ops = changes.ops(&self.mailbox);
                // Items this command deleted for good stay recoverable (not a draft replaced by its sent copy,
                // nor the items of a data file being closed or of a removed account: `capture` skips those).
                if !ops.is_empty()
                    && self.run_depth == 0
                    && !self.undo_barrier
                    && !spec.id.starts_with("compose.")
                {
                    crate::recover::capture(self, &changes);
                }
                if self.undo_barrier {
                    self.undo.clear();
                }
                if !ops.is_empty() {
                    if self.automatic_depth == 0 && !self.undo_barrier {
                        self.record_undo(spec.label, changes);
                    }
                    if self.journal_calls == journal_calls_before {
                        self.drop_superseded(pending_before, &ops);
                    }
                    self.journal(ops);
                }
            }
            self.refresh_me();
        }
        if self.run_depth == 0 {
            self.undo_barrier = false;
        }
        let result = if stranded.is_empty() {
            result
        } else {
            Err(crate::sync::cmds::not_downloaded(self, &stranded, "moved off the server"))
        };
        if self.pending_ops.len() > pending_before {
            // Ops a command queued directly (without a mailbox diff, e.g. view-only changes, or Open marking
            // the message read): the search index must hear of them like journaled ones.
            if let Some(direct) = self.pending_ops.get(pending_before..) {
                self.search.note_ops(direct);
            }
            self.flush_pending();
        }
        if self.run_depth == 0 {
            let purge = std::mem::take(&mut self.purge_blobs_after);
            if std::mem::take(&mut self.checkpoint_after) && self.checkpoint_now() && purge {
                self.purge_unused_blobs();
            }
        }
        self.tidy_selection();
        self.sync_settings_from_session();
        if let Err(e) = self.save_settings() {
            log::error!("{e}");
        }
        result
    }

    /// Runs a command as an automatic action of the session (new mail being processed: automatic meeting
    /// responses, tracking updates), not as something the user did: like [`Session::run`], but the change
    /// records no undo step, and the server changes it makes are left to the caller, which queues them with
    /// its own (it compares the mailbox from before the whole batch).
    pub fn run_automatic(&mut self, id: &str, params: &Value) -> CmdResult {
        self.automatic_depth = self.automatic_depth.saturating_add(1);
        self.run_depth = self.run_depth.saturating_add(1);
        let result = self.run(id, params);
        self.run_depth = self.run_depth.saturating_sub(1);
        self.automatic_depth = self.automatic_depth.saturating_sub(1);
        result
    }

    /// Drops the per-item ops a command queued directly (from `from` on, none written yet) for items its
    /// change record journals anyway: `ops` come after them and hold each item's final state, so the item is
    /// not written twice (a flag change on a message wrote the message twice before).
    fn drop_superseded(&mut self, from: usize, ops: &[Op]) {
        if self.pending_ops.len() <= from {
            return;
        }
        let covered: std::collections::BTreeSet<ItemId> = ops
            .iter()
            .filter_map(|o| match o {
                Op::PutMail(m) => Some(m.id),
                Op::RemoveMail(id) => Some(*id),
                _ => None,
            })
            .collect();
        if covered.is_empty() {
            return;
        }
        let queued = self.pending_ops.split_off(from);
        self.pending_ops.extend(queued.into_iter().filter(|o| match o {
            Op::PutMail(m) => !covered.contains(&m.id),
            Op::RemoveMail(id) => !covered.contains(id),
            _ => true,
        }));
    }

    /// Queues ops for the store and writes them when a store is attached (errors are logged; the command
    /// still succeeds and the ops stay queued for the next attempt).
    pub fn journal(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        self.journal_calls = self.journal_calls.wrapping_add(1);
        self.mailbox_changed();
        self.search.note_ops(&ops);
        self.pending_ops.extend(ops);
        self.flush_pending();
    }

    /// Writes queued ops to the store (when attached). A queued `Replace` supersedes everything before it, so
    /// the queue stays small even without a store.
    ///
    /// Guard rails (issue #25): when the journal refuses the batch for its size, the same state is saved as a
    /// snapshot instead; a snapshot is never written past what the store reads back, and when it is refused
    /// the journal is kept as it is and the user is told once (an error notice).
    pub fn flush_pending(&mut self) {
        if self.pending_ops.is_empty() {
            return;
        }
        // Queued ops describe changes to the mailbox (some commands queue them directly).
        self.mailbox_changed();
        if let Some(last_replace) = self.pending_ops.iter().rposition(|o| matches!(o, Op::Replace(_))) {
            self.pending_ops.drain(..last_replace);
        }
        if self.pending_ops.len() > MAX_PENDING_OPS {
            self.pending_ops = vec![Op::Replace(Box::new(self.mailbox.clone()))];
        }
        let Some(store) = self.store.as_mut() else { return };
        let guard = &mut self.store_guard;
        let mut notices = Vec::new();
        match store.append(&self.pending_ops) {
            Ok(()) => {
                self.pending_ops.clear();
                guard.journal_refused = false;
            }
            Err(e) => {
                log::error!("could not write {} change(s) to the store: {e}", self.pending_ops.len());
                if matches!(e, StoreError::TooLarge(_)) && guard.fallback_due(&self.mailbox) {
                    // The mailbox already holds every queued change: a snapshot saves the same state.
                    match store.checkpoint(&self.mailbox) {
                        Ok(()) => {
                            self.pending_ops.clear();
                            guard.snapshot_refused = false;
                            guard.journal_refused = false;
                            guard.checkpoint_retry_at = 0;
                        }
                        Err(e2) => {
                            log::error!("could not save the changes as a snapshot either: {e2}");
                            guard.fallback_refused_bytes = self.mailbox.approx_bytes();
                            guard.fallback_skips = 0;
                            if !std::mem::replace(&mut guard.journal_refused, true) {
                                notices.push(Notice::Error(UNSAVED_TOO_LARGE.to_owned()));
                            }
                        }
                    }
                }
            }
        }
        if self.pending_ops.is_empty()
            && self.checkpoint_bytes > 0
            && store.journal_bytes() >= self.checkpoint_bytes.max(guard.checkpoint_retry_at)
        {
            // The journal now leads to `self.mailbox`; a snapshot of it supersedes the journal.
            match store.checkpoint(&self.mailbox) {
                Ok(()) => {
                    guard.snapshot_refused = false;
                    guard.checkpoint_retry_at = 0;
                }
                Err(e) => {
                    log::error!("could not fold the journal into a snapshot: {e}");
                    if matches!(e, StoreError::TooLarge(_)) {
                        // Try again once the journal has grown by another checkpoint's worth (the mailbox
                        // may have shrunk by then), not on every write.
                        guard.checkpoint_retry_at =
                            store.journal_bytes().saturating_add(self.checkpoint_bytes.max(1));
                        if !std::mem::replace(&mut guard.snapshot_refused, true) {
                            notices.push(Notice::Error(SNAPSHOT_TOO_LARGE.to_owned()));
                        }
                    }
                }
            }
        }
        self.notices.extend(notices);
    }

    /// Folds the journal into a fresh snapshot of the mailbox now (when a store is attached and every queued
    /// change is written), so items that left the mailbox leave the journal and snapshot too. Message sources
    /// (content-addressed files next to the snapshot) are not touched.
    /// Returns whether the snapshot was written (`false` without a store too).
    pub(crate) fn checkpoint_now(&mut self) -> bool {
        if !self.pending_ops.is_empty() {
            self.flush_pending();
        }
        let Some(store) = self.store.as_mut() else { return false };
        if !self.pending_ops.is_empty() {
            return false;
        }
        match store.checkpoint(&self.mailbox) {
            Ok(()) => {
                self.store_guard.snapshot_refused = false;
                self.store_guard.checkpoint_retry_at = 0;
                true
            }
            Err(e) => {
                log::error!("could not fold the journal into a snapshot: {e}");
                if matches!(e, StoreError::TooLarge(_))
                    && !std::mem::replace(&mut self.store_guard.snapshot_refused, true)
                {
                    self.notify(Notice::Error(SNAPSHOT_TOO_LARGE.to_owned()));
                }
                false
            }
        }
    }

    /// Deletes the message content in the store's blobs that nothing refers to any more (the mailbox, which
    /// was just checkpointed, the recoverable-items bin in memory and on disk, and the undo history), e.g.
    /// that of a protected data file that was closed. Content another item shares stays.
    pub(crate) fn purge_unused_blobs(&mut self) {
        // The bin in memory too: entries a failed save left out of the file still need their content.
        let also = self.content_keys_in_use();
        if let Some(store) = self.store.as_mut() {
            self.bodies.clear_cache();
            match store.gc_blobs(&self.mailbox, &also) {
                Ok(n) => log::info!("removed {n} message sources nothing refers to any more"),
                Err(e) => {
                    log::error!("could not remove unused message sources: {e}");
                    self.notify(Notice::Error(format!(
                        "The message sources of the closed data file could not all be removed from this \
                         profile ({e})."
                    )));
                }
            }
        }
    }

    /// Removes the profile's stored content that nothing in this session refers to (see
    /// [`Session::content_keys_in_use`]; the bin on disk is read too). Not while changes are still queued for
    /// the store: what it holds may still name content memory no longer does.
    pub(crate) fn collect_content(&mut self) {
        if !self.pending_ops.is_empty() {
            return;
        }
        let live = self.content_keys_in_use();
        let Some(store) = self.store.as_mut() else { return };
        match store.gc_blobs(&self.mailbox, &live) {
            Ok(0) => {}
            Ok(n) => {
                self.bodies.clear_cache();
                log::info!("removed {n} stored message content files nothing refers to any more");
            }
            Err(e) => log::warn!("could not remove unused message content: {e}"),
        }
    }

    /// Undoes the last recorded command (puts back what it changed); returns its label. `Disabled` when there
    /// is nothing to undo; `Failed` when putting the step back went wrong (the mailbox and the history are
    /// left as they were).
    pub fn undo_last(&mut self) -> Result<String, CmdError> {
        let entry = self.undo.undo.pop().ok_or_else(|| CmdError::Disabled("nothing to undo".into()))?;
        let label = entry.label.clone();
        match self.restore_step(entry) {
            Ok(live) => match UndoEntry::from_changes(&label, live) {
                Some(redo) => self.undo.push_redo(redo),
                None => self.undo.redo.clear(),
            },
            Err(entry) => {
                self.undo.undo.push(*entry);
                return Err(CmdError::Failed(format!("\"{label}\" could not be undone")));
            }
        }
        Ok(label)
    }

    /// Redoes the last undone command; returns its label (errors as [`Session::undo_last`]).
    pub fn redo_last(&mut self) -> Result<String, CmdError> {
        let entry = self.undo.redo.pop().ok_or_else(|| CmdError::Disabled("nothing to redo".into()))?;
        let label = entry.label.clone();
        match self.restore_step(entry) {
            Ok(live) => match UndoEntry::from_changes(&label, live) {
                Some(undo) => self.undo.push_keep_redo(undo),
                None => self.undo.undo.clear(),
            },
            Err(entry) => {
                self.undo.redo.push(*entry);
                return Err(CmdError::Failed(format!("\"{label}\" could not be redone")));
            }
        }
        Ok(label)
    }

    /// Puts an undo or redo step back: everything but the mail as the step kept it, and the mail items it
    /// kept, then what happened meanwhile (folders holding later mail, `sync::after_restore`). Journals the
    /// change and returns what was there before, as the step that reverts this one.
    ///
    /// A panic while adjusting for what happened meanwhile puts the mailbox back as it was before the
    /// restore and hands the step back (`Err`) for the history to keep; nothing is journaled.
    fn restore_step(&mut self, entry: UndoEntry) -> Result<crate::changes::ChangeSet, Box<UndoEntry>> {
        #[cfg(any(test, debug_assertions))]
        let full = self.mailbox.clone();
        let UndoEntry { label, small, mail, .. } = entry;
        let step_ids: std::collections::BTreeSet<ItemId> = mail.ids().collect();
        let live_small = Box::new(self.mailbox.replace_all_but_mail(*small));
        // Ids never go back: items and folders made after the step (a draft saved, mail that arrived) keep
        // theirs, and an id the step frees is not handed out again before a redo brings its item back.
        self.mailbox.next_id = self.mailbox.next_id.max(live_small.next_id);
        let live_mail = self.mailbox.mail.restore(mail);
        // What the step put back, in case it has to be taken out again.
        let step_small = self.mailbox.without_mail();
        let mut changes = crate::changes::ChangeSet::new(live_small, Some(live_mail));
        #[cfg(any(test, debug_assertions))]
        changes.set_full(full);
        let token = self.mailbox.mail.begin();
        let adjusted = catch_unwind(AssertUnwindSafe(|| {
            #[cfg(test)]
            test_hooks::maybe_panic_in_restore(&mut self.mailbox.mail);
            self.keep_filled_folders(&changes.small);
            crate::sync::after_restore(self, &mut changes);
        }));
        if let Err(payload) = adjusted {
            log::error!(
                "undo/redo of \"{label}\" panicked: {}; the step is kept",
                panic_text(payload.as_ref())
            );
            // The adjustments go first (records they left open are folded in), then the restore itself.
            let _ = self.mailbox.mail.rollback(token);
            let crate::changes::ChangeSet { small: live_small, mail: live_mail, .. } = changes;
            let next_id = self.mailbox.next_id;
            let _ = self.mailbox.replace_all_but_mail(*live_small);
            self.mailbox.next_id = self.mailbox.next_id.max(next_id);
            let put_back = live_mail.map(|m| self.mailbox.mail.restore(m)).unwrap_or_default();
            let mail = put_back.into_iter().filter(|(id, _)| step_ids.contains(id)).collect();
            self.mailbox_changed();
            self.tidy_selection();
            return Err(Box::new(UndoEntry::new(label, Box::new(step_small), mail)));
        }
        let more = self.mailbox.mail.commit(token);
        changes.absorb(more);
        let ops = changes.ops(&self.mailbox);
        self.journal(ops);
        self.refresh_me();
        self.tidy_selection();
        Ok(changes)
    }

    /// After an undo or redo put everything but the mail back: a folder the step takes away (it created the
    /// folder) that still holds mail the step did not put there (filed by AutoArchive, a rule or
    /// synchronisation afterwards) stays, with its parents, so that mail is never dropped. `live` is
    /// everything but the mail as it was before the restore.
    ///
    /// Not when the folder's account is gone too (undo of Add Account, redo of Remove Account): that mail goes
    /// with the account, as the step took it, instead of staying in folders no account owns. Runs inside the
    /// restore's change record, which keeps what it removes.
    fn keep_filled_folders(&mut self, live: &Mailbox) {
        let gone: std::collections::BTreeSet<FolderId> =
            live.folders.keys().filter(|f| !self.mailbox.folders.contains_key(f)).copied().collect();
        if gone.is_empty() {
            return;
        }
        let mut holding: std::collections::BTreeSet<FolderId> = std::collections::BTreeSet::new();
        for m in self.mailbox.mail.items() {
            if gone.contains(&m.folder) {
                holding.insert(m.folder);
            }
        }
        let mut orphaned: std::collections::BTreeSet<FolderId> = std::collections::BTreeSet::new();
        for fid in holding {
            if !live.folders.get(&fid).is_some_and(|f| self.mailbox.account(f.account).is_some()) {
                orphaned.insert(fid);
                continue;
            }
            let mut cur = Some(fid);
            let mut hops = 0usize;
            while let Some(f) = cur {
                hops = hops.saturating_add(1);
                if self.mailbox.folders.contains_key(&f) || hops > live.folders.len() {
                    break;
                }
                let Some(folder) = live.folders.get(&f) else { break };
                if self.mailbox.account(folder.account).is_none() {
                    break;
                }
                log::info!("undo keeps folder {} ({}): it holds mail filed after the step", f.0, folder.name);
                cur = folder.parent;
                self.mailbox.folders.insert(f, folder.clone());
            }
        }
        if !orphaned.is_empty() {
            log::info!("undo removes the mail of {} folders of an account it takes away", orphaned.len());
            self.mailbox.mail.retain(|_, m| !orphaned.contains(&m.folder));
        }
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
            mb.mail.contains(*id)
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
        self.clipboard.retain(|id| mail.contains(*id));
        // Translations of messages that are gone (deleted, or replaced by a sync) are dropped.
        self.translations.retain(|id, _| mail.contains(*id));
        // So are their View in Browser pages.
        self.browser_pages.prune(|id| mail.contains(id));
    }

    /// Item ids a command should act on: `params.ids` when present, else the selection (else the focus).
    pub fn target_ids(&self, params: &Value) -> Vec<ItemId> {
        if let Some(ids) = crate::cmd::util::opt_ids(params) {
            return ids;
        }
        if let Some(ids) = self.check_scope() {
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
        self.apply_compose_defaults(&mut c);
        // Named places in a reopened draft or a new message become bookmarks. A reply or forward leaves the
        // ones in the quoted original where they are (they belong to that message, not to the user).
        if matches!(c.kind, crate::compose::ComposeKind::New | crate::compose::ComposeKind::Draft) {
            c.adopt_anchors();
        }
        self.next_compose = self.next_compose.saturating_add(1);
        c.id = self.next_compose;
        self.composes.insert(c.id, c);
        self.notices.push(Notice::Compose(self.next_compose));
        self.next_compose
    }

    /// Whether a command is currently enabled (`None` = enabled).
    /// True while the enabled check of a command whose params name its items (a non-null `id`, or a
    /// non-empty `ids`) runs: a check that only asks "is something selected?" passes then, so scripts (CLI,
    /// MCP) can act on items by id with nothing selected, while the ribbon still greys the command out.
    pub fn params_name_items(&self) -> bool {
        self.params_name_items
    }

    pub fn enabled(&self, id: &str) -> Option<&'static str> {
        match self.registry.get(id) {
            Some(spec) => (spec.enabled)(self),
            None => Some("unknown command"),
        }
    }

    /// Whether command `id` is enabled for `items` instead of the selection (a read window asks for its
    /// own message): while the check runs, [`Session::target_ids`] and the "focused item" checks see
    /// `items`. Nothing of the session changes.
    pub fn enabled_for(&self, id: &str, items: &[ItemId]) -> Option<&'static str> {
        let Some(spec) = self.registry.get(id) else { return Some("unknown command") };
        *self.check_scope.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(items.to_vec());
        let _guard = ScopeGuard(&self.check_scope);
        (spec.enabled)(self)
    }

    /// The items an enabled check is scoped to ([`Session::enabled_for`]), if one is running.
    pub fn check_scope(&self) -> Option<Vec<ItemId>> {
        self.check_scope.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// The items command params name (`ids`, else `id`) when every one of them is an item of the mailbox
    /// (a message, contact, appointment, task or note); `None` otherwise (ids of other things, or none).
    fn named_items(&self, params: &Value) -> Option<Vec<ItemId>> {
        let ids = match crate::cmd::util::opt_ids(params) {
            Some(ids) => ids,
            None => vec![ItemId(params.get("id").and_then(Value::as_u64)?)],
        };
        let mb = &self.mailbox;
        let exists = |id: &ItemId| {
            mb.mail.contains(*id)
                || mb.contacts.contains_key(id)
                || mb.appointments.contains_key(id)
                || mb.tasks.contains_key(id)
                || mb.notes.contains_key(id)
        };
        (!ids.is_empty() && ids.len() <= 10_000 && ids.iter().all(exists)).then_some(ids)
    }
}

/// Settings the session also holds elsewhere ([`Session::apply_setting`]).
const MIRRORED: [&str; 6] = [
    "advanced.sendImmediately",
    "advanced.sendReceiveMinutes",
    "general.theme",
    "search.scope",
    "search.includeDeleted",
    "calendar.timeZone",
];

/// Search scopes by their option names (Options › Search › "Include results only from").
/// Options › Search › "Current folder, current mailbox when searching from the Inbox".
pub(crate) const INBOX_MAILBOX_SCOPE: &str = "folderInboxMailbox";

impl Session {
    /// The search scope [`INBOX_MAILBOX_SCOPE`] gives in `folder`: the mailbox from an Inbox, else the folder.
    pub(crate) fn inbox_rule_scope(&self, folder: FolderId) -> SearchScope {
        if self.mailbox.folder(folder).is_some_and(|f| f.role == Some(FolderRole::Inbox)) {
            SearchScope::CurrentMailbox
        } else {
            SearchScope::CurrentFolder
        }
    }
}

pub(crate) const SCOPE_NAMES: [(SearchScope, &str); 5] = [
    (SearchScope::CurrentFolder, "currentFolder"),
    (SearchScope::Subfolders, "subfolders"),
    (SearchScope::CurrentMailbox, "currentMailbox"),
    (SearchScope::AllMailboxes, "allMailboxes"),
    (SearchScope::AllItems, "allItems"),
];

/// Tests: makes the next undo or redo panic while it adjusts for what happened after its step.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;

    thread_local! {
        static PANIC_IN_RESTORE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn panic_in_next_restore() {
        PANIC_IN_RESTORE.with(|c| c.set(true));
    }

    /// Panics when asked to, after opening a change record it never closes and changing an item under it
    /// (as code that panics half-way would).
    pub(crate) fn maybe_panic_in_restore(mail: &mut mailcraft_model::MailTable) {
        if PANIC_IN_RESTORE.with(|c| c.replace(false)) {
            let _ = mail.begin();
            if let Some(id) = mail.ids().next() {
                mail.update(id, |m| m.subject.push_str(" (half done)"));
            }
            panic!("injected panic while undo adjusts the mailbox");
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

/// Store ops turning `before` into `after`: the configuration parts and account sync states that changed
/// ([`small_ops`]), then per-folder and per-item upserts and removals; or one `Replace` when the calendar,
/// contacts, tasks or notes changed (until storage stage S12).
///
/// Compares every item, so it is O(mailbox). Commands, undo and sync use change records instead
/// (`changes.rs`, which derives the same ops); this remains for the debug-build check that they agree and for
/// tests (release builds leave it out).
#[cfg(any(test, debug_assertions))]
pub(crate) fn diff_ops(before: &Mailbox, after: &Mailbox) -> Vec<Op> {
    let Some(mut ops) = small_ops(before, after) else {
        return vec![Op::Replace(Box::new(after.clone()))];
    };
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
    for id in before.mail.ids() {
        if !after.mail.contains(id) {
            ops.push(Op::RemoveMail(id));
        }
    }
    for id in before.folders.keys() {
        if !after.folders.contains_key(id) {
            ops.push(Op::RemoveFolder(*id));
        }
    }
    ops
}

/// The ops for everything but the folders and the mail (storage stage S4): one `PutConfig` with the
/// configuration parts that changed (accounts compared and carried without their sync state), then one
/// `PutSync` per account whose sync state changed, in list order. `None` when that cannot be said in those
/// ops and the whole mailbox is journaled instead: the calendar, contacts, tasks or notes changed
/// ([`pim_changed`]), or an account id appears twice.
pub(crate) fn small_ops(before: &Mailbox, after: &Mailbox) -> Option<Vec<Op>> {
    if pim_changed(before, after) || !unique_accounts(&before.accounts) || !unique_accounts(&after.accounts) {
        return None;
    }
    let mut ops = Vec::new();
    if let Some(state) = mailcraft_store::SmallState::changed(before, after) {
        ops.push(Op::PutConfig(Box::new(state)));
    }
    let was: BTreeMap<mailcraft_model::AccountId, &mailcraft_model::account::AccountSync> =
        before.accounts.iter().map(|a| (a.id, &a.sync)).collect();
    let none = mailcraft_model::account::AccountSync::default();
    for a in &after.accounts {
        // An account new to the list starts with no sync state (`SmallState::apply`).
        if **was.get(&a.id).unwrap_or(&&none) != a.sync {
            ops.push(Op::PutSync(a.id, Box::new(a.sync.clone())));
        }
    }
    Some(ops)
}

/// Whether no account id appears twice.
fn unique_accounts(accounts: &[mailcraft_model::Account]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    accounts.iter().all(|a| seen.insert(a.id))
}

/// Whether the calendar, contacts, tasks or notes differ (the store journals that as a whole-mailbox
/// `Replace` until storage stage S12).
pub(crate) fn pim_changed(before: &Mailbox, after: &Mailbox) -> bool {
    // Exhaustive on purpose: a new Mailbox field fails to compile here until it is journaled.
    let Mailbox {
        // Journaled by `small_ops` (PutConfig, PutSync).
        accounts: _,
        categories: _,
        rules: _,
        quick_steps: _,
        views: _,
        signatures: _,
        signature_defaults: _,
        recent_recipients: _,
        custom_dictionary: _,
        junk: _,
        // Journaled per entry.
        folders: _,
        mail: _,
        next_id: _,
        appointments,
        contacts,
        tasks,
        notes,
    } = after;
    before.appointments != *appointments
        || before.contacts != *contacts
        || before.tasks != *tasks
        || before.notes != *notes
}

/// Whether command params name the items to act on ([`Session::params_name_items`]).
fn names_items(params: &Value) -> bool {
    params.get("id").is_some_and(|v| !v.is_null())
        || params.get("ids").and_then(Value::as_array).is_some_and(|a| !a.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_journals_signatures_and_auto_complete() {
        let mb = Mailbox::new_local("Local");
        let mut after = mb.clone();
        after.custom_dictionary.push("mailcraft".into());
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::PutConfig(_)]));
        let mut after = mb.clone();
        after.recent_recipients.push(Default::default());
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::PutConfig(_)]));
        let mut after = mb.clone();
        after.signatures.push(Default::default());
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::PutConfig(_)]));
        assert!(diff_ops(&mb, &mb.clone()).is_empty());
    }

    #[test]
    fn automatic_runs_record_no_undo_step() {
        let mut s = crate::sample::sample_session();
        let id = s.mailbox.mail.items().find(|m| !m.read).map(|m| m.id).unwrap();
        s.selection.folder = s.mailbox.mail.get(id).unwrap().folder;
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let undo_before = s.undo.undo.len();
        let ops_before = s.journal_calls;
        s.run_automatic("mail.markRead", &serde_json::json!({"ids": [id.0]})).unwrap();
        assert!(s.mailbox.mail.get(id).unwrap().read);
        assert_eq!(s.undo.undo.len(), undo_before, "not undoable");
        assert!(s.journal_calls > ops_before, "still journaled");
        assert_eq!((s.automatic_depth, s.run_depth), (0, 0));
        // The same command run by the user is one undo step.
        s.run("mail.markUnread", &serde_json::json!({"ids": [id.0]})).unwrap();
        assert_eq!(s.undo.undo.len(), undo_before + 1);
    }

    #[test]
    fn old_view_state_pins_and_note_positions_move_onto_the_items_when_opened() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let (msg, note) = {
            let mut s = Session::open(dir.path()).unwrap();
            let inbox = s.selection.folder;
            let msg = s.mailbox.add_mail(mailcraft_model::MailItem {
                folder: inbox,
                subject: "keep".into(),
                ..Default::default()
            });
            let notes = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Notes)).unwrap().id;
            let note =
                s.run("notes.new", &serde_json::json!({"text": "A", "folder": notes.0})).unwrap()["id"]
                    .as_u64()
                    .unwrap();
            s.journal(vec![Op::Replace(Box::new(s.mailbox.clone()))]);
            // What MailCraft 0.1 left in the view state.
            s.view.collapsed_groups.insert(inbox, vec![format!("\u{1}mail.pinned=[{}]", msg.0)]);
            s.view.collapsed_groups.insert(notes, vec![format!("\u{1}notes.pos={{\"{note}\":[12.0,34.0]}}")]);
            s.save_settings().unwrap();
            (msg, ItemId(note))
        };
        {
            let s = Session::open(dir.path()).unwrap();
            assert!(s.mailbox.mail.get(msg).unwrap().pinned);
            assert_eq!(s.mailbox.notes[&note].position(), Some((12.0, 34.0)));
            assert!(s.view.collapsed_groups.values().flatten().all(|e| !e.starts_with('\u{1}')));
        }
        // Saved with the mailbox, and gone from settings.json: unpinning sticks.
        let mut s = Session::open(dir.path()).unwrap();
        assert!(s.mailbox.mail.get(msg).unwrap().pinned);
        s.run("mail.pin", &serde_json::json!({"ids": [msg.0]})).unwrap();
        s.run("notes.setPosition", &serde_json::json!({"id": note.0, "position": null})).unwrap();
        drop(s);
        let s = Session::open(dir.path()).unwrap();
        assert!(!s.mailbox.mail.get(msg).unwrap().pinned);
        assert_eq!(s.mailbox.notes[&note].position(), None);
    }

    #[test]
    fn old_view_state_entries_stay_until_the_moved_state_is_stored() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let settings = || std::fs::read_to_string(dir.path().join(settings::FILE_NAME)).unwrap();
        let msg = {
            let mut s = Session::open(dir.path()).unwrap();
            let inbox = s.selection.folder;
            let msg = s.mailbox.add_mail(mailcraft_model::MailItem { folder: inbox, ..Default::default() });
            s.journal(vec![Op::Replace(Box::new(s.mailbox.clone()))]);
            s.view.collapsed_groups.insert(inbox, vec![format!("\u{1}mail.pinned=[{}]", msg.0)]);
            s.save_settings().unwrap();
            // The store cannot take the change (disk full, I/O error): settings.json keeps the old pins.
            s.store.as_mut().unwrap().set_journal_cap(1);
            s.store.as_mut().unwrap().set_snapshot_cap(1);
            s.load_settings(dir.path());
            assert!(s.mailbox.mail.get(msg).unwrap().pinned, "pinned in memory");
            assert!(!s.pending_ops.is_empty());
            assert!(s.view.collapsed_groups[&inbox].iter().any(|e| e.starts_with("\u{1}mail.pinned=")));
            assert!(settings().contains("mail.pinned="), "{}", settings());
            msg
        };
        // Next start (the store writes again): moved for good.
        let s = Session::open(dir.path()).unwrap();
        assert!(s.mailbox.mail.get(msg).unwrap().pinned);
        assert!(!settings().contains("mail.pinned="));
    }
}
