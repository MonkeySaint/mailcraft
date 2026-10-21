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
    /// Send / Receive › Work Offline: no server connections; Outbox messages wait.
    pub work_offline: bool,
    /// Options › Advanced › Send/Receive: automatic send/receive every N minutes (0 = off).
    pub auto_send_receive_minutes: u32,
    /// Options › Advanced › "Send immediately when connected".
    pub send_immediately: bool,
}

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
    /// Data files closed in a session without a store, ready to be opened again (`datafile.rs`).
    pub closed_data_files: Vec<crate::datafile::ClosedDataFile>,
    /// Set by a command whose effect reaches outside the mailbox where an undo snapshot cannot put it back
    /// (closing or opening a data file): the undo history is cleared instead of recording a step, since any
    /// older snapshot would bring back or drop that data file too.
    pub(crate) undo_barrier: bool,
    /// Quick Print may go straight to the system's default printer. The desktop app turns it on; tests and
    /// tools leave it off, so nothing reaches a real printer by accident.
    pub direct_print: bool,
    /// Profile folder holding `settings.json` (`None` = in-memory session: nothing is saved).
    settings_dir: Option<std::path::PathBuf>,
    /// Bytes last read from or written to `settings.json` (identical states are not rewritten).
    settings_saved: Option<Vec<u8>>,
    /// The command whose enabled check is running names its items in its params (`id` / `ids`), see
    /// [`Session::params_name_items`].
    params_name_items: bool,
}

/// Longest wait for "Perform an automatic send/receive when exiting" (seconds).
pub const EXIT_SYNC_SECS: u64 = 30;

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
            run_depth: 0,
            automatic_depth: 0,
            exit_sync_done: false,
            secrets: Box::new(crate::accounts::MemorySecretStore::default()),
            sync: crate::sync::SyncState::default(),
            search: crate::search_index::SearchState::default(),
            smime: crate::smime::SmimeSlot::default(),
            settings: Settings::default(),
            closed_data_files: Vec::new(),
            undo_barrier: false,
            direct_print: false,
            settings_dir: None,
            settings_saved: None,
            params_name_items: false,
        };
        s.refresh_me();
        s.select_default_folder();
        s
    }

    /// Opens (or creates) a persistent session in `dir`. A new data folder gets a local mailbox ("Local
    /// Folders") written as its first snapshot.
    pub fn open(dir: &std::path::Path) -> Result<Session, CmdError> {
        let mut store = mailcraft_store::Store::open(dir)
            .map_err(|e| CmdError::Failed(format!("cannot open the data folder {}: {e}", dir.display())))?;
        let mut mailbox = store
            .load()
            .map_err(|e| CmdError::Failed(format!("cannot read the data folder {}: {e}", dir.display())))?;
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
        let mut s = Session::new(mailbox);
        s.store = Some(store);
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
        crate::smime::load(&mut s);
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
                self.notify(Notice::Error(format!(
                    "Your settings could not be read ({e}); MailCraft started with the default settings."
                )));
            }
        }
        self.apply_settings();
        self.select_start_folder();
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
                }
            }
            "search.includeDeleted" => self.search.include_deleted = st.bool(key),
            "calendar.timeZone" => self.tz_offset_min = st.time_zone_offset_min(),
            _ => {}
        }
    }

    /// Copies session state that mirrors a setting back into the settings (commands such as Work Offline,
    /// Change Time Zone or the dark-mode toggle change the session directly).
    fn sync_settings_from_session(&mut self) {
        let scope = SCOPE_NAMES
            .iter()
            .find(|(sc, _)| *sc == self.view.search_scope)
            .map_or("currentFolder", |(_, n)| *n);
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
                if c.format == mailcraft_model::BodyFormat::Html {
                    c.format = st.compose_format();
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
        if self.exit_sync_done || !self.settings.bool("advanced.sendReceiveOnExit") || self.view.work_offline
        {
            return;
        }
        let accounts = crate::sync::group_accounts(self);
        if accounts.is_empty() {
            return;
        }
        self.exit_sync_done = true;
        let plan = crate::sync::Plan { user: false, ..crate::sync::Plan::all() };
        match self.send_receive(&accounts, plan) {
            Ok(_) => {
                if !self.wait_for_sync(std::time::Duration::from_secs(EXIT_SYNC_SECS)) {
                    log::warn!("send/receive on exit did not finish in {EXIT_SYNC_SECS} s");
                    self.cancel_sync();
                }
            }
            Err(e) => log::warn!("send/receive on exit: {e}"),
        }
    }

    /// Options › Advanced › "Empty Deleted Items folders when exiting": empties every Deleted Items folder
    /// (items and subfolders) and journals the change before the exit save.
    fn empty_deleted_on_exit(&mut self) {
        if !self.settings.bool("advanced.emptyDeletedOnExit") {
            return;
        }
        let before = self.mailbox.clone();
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
        let ops = diff_ops(&before, &self.mailbox);
        if !ops.is_empty() {
            self.undo.push(UndoEntry { label: "Empty Deleted Items".into(), before: Box::new(before) });
            self.journal(ops);
            self.tidy_selection();
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
    /// (a panicking command returns `CmdError::Failed` and the mailbox is restored from the undo snapshot),
    /// flushes pending ops to the store.
    pub fn run(&mut self, id: &str, params: &Value) -> CmdResult {
        let Some(spec) = self.registry.get(id).cloned() else {
            return Err(CmdError::Unknown(id.to_owned()));
        };
        let named = std::mem::replace(&mut self.params_name_items, names_items(params));
        let enabled = catch_unwind(AssertUnwindSafe(|| (spec.enabled)(self)));
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
        // Undo/redo restore snapshots themselves (and journal a Replace); they must not record undo entries.
        let self_journaling = matches!(spec.id, "app.undo" | "app.redo");
        let snapshot = (spec.mutates && !self_journaling).then(|| self.mailbox.clone());
        let pending_before = self.pending_ops.len();
        let journal_calls_before = self.journal_calls;
        let counted = u32::from(snapshot.is_some());
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
                // Local changes to synchronised folders become queued server changes (adjusting the
                // mailbox, so this comes before the diff that journals it).
                if self.run_depth == 0 {
                    crate::sync::queue_local_changes(self, &before);
                }
                let ops = diff_ops(&before, &self.mailbox);
                if self.undo_barrier {
                    self.undo.clear();
                }
                if !ops.is_empty() {
                    if self.automatic_depth == 0 && !self.undo_barrier {
                        self.undo.push(UndoEntry { label: spec.label.to_owned(), before: Box::new(before) });
                    }
                    self.journal(ops);
                }
            }
            self.refresh_me();
        }
        if self.run_depth == 0 {
            self.undo_barrier = false;
        }
        if self.pending_ops.len() > pending_before {
            // Ops a command queued directly (without a mailbox diff, e.g. view-only changes, or Open marking
            // the message read): the search index must hear of them like journaled ones.
            if let Some(direct) = self.pending_ops.get(pending_before..) {
                self.search.note_ops(direct);
            }
            self.flush_pending();
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

    /// Queues ops for the store and writes them when a store is attached (errors are logged; the command
    /// still succeeds and the ops stay queued for the next attempt).
    pub fn journal(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        self.journal_calls = self.journal_calls.wrapping_add(1);
        self.search.note_ops(&ops);
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
        crate::sync::after_restore(self, &after);
        self.undo.push_redo(UndoEntry { label: entry.label.clone(), before: Box::new(after) });
        self.after_restore();
        Some(entry.label)
    }

    /// Redoes the last undone command; returns its label.
    pub fn redo_last(&mut self) -> Option<String> {
        let entry = self.undo.redo.pop()?;
        let before = std::mem::replace(&mut self.mailbox, *entry.before);
        crate::sync::after_restore(self, &before);
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
pub(crate) const SCOPE_NAMES: [(SearchScope, &str); 5] = [
    (SearchScope::CurrentFolder, "currentFolder"),
    (SearchScope::Subfolders, "subfolders"),
    (SearchScope::CurrentMailbox, "currentMailbox"),
    (SearchScope::AllMailboxes, "allMailboxes"),
    (SearchScope::AllItems, "allItems"),
];

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
/// anything else changed (accounts and their sync queues, rules, categories, views, quick steps,
/// calendar/contacts/tasks/notes, signatures, Auto-Complete, the dictionary).
pub(crate) fn diff_ops(before: &Mailbox, after: &Mailbox) -> Vec<Op> {
    // Exhaustive on purpose: a new Mailbox field fails to compile here until it is journaled.
    let Mailbox {
        accounts,
        folders: _,
        mail: _,
        appointments,
        contacts,
        tasks,
        notes,
        categories,
        rules,
        quick_steps,
        views,
        next_id: _,
        signatures,
        signature_defaults,
        recent_recipients,
        custom_dictionary,
        junk,
    } = after;
    let other_changed = before.accounts != *accounts
        || before.categories != *categories
        || before.rules != *rules
        || before.quick_steps != *quick_steps
        || before.views != *views
        || before.appointments != *appointments
        || before.contacts != *contacts
        || before.tasks != *tasks
        || before.notes != *notes
        || before.signatures != *signatures
        || before.signature_defaults != *signature_defaults
        || before.recent_recipients != *recent_recipients
        || before.custom_dictionary != *custom_dictionary
        || before.junk != *junk;
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
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::Replace(_)]));
        let mut after = mb.clone();
        after.recent_recipients.push(Default::default());
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::Replace(_)]));
        let mut after = mb.clone();
        after.signatures.push(Default::default());
        assert!(matches!(diff_ops(&mb, &after).as_slice(), [Op::Replace(_)]));
        assert!(diff_ops(&mb, &mb.clone()).is_empty());
    }

    #[test]
    fn automatic_runs_record_no_undo_step() {
        let mut s = crate::sample::sample_session();
        let id = s.mailbox.mail.values().find(|m| !m.read).map(|m| m.id).unwrap();
        s.selection.folder = s.mailbox.mail[&id].folder;
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let undo_before = s.undo.undo.len();
        let ops_before = s.journal_calls;
        s.run_automatic("mail.markRead", &serde_json::json!({"ids": [id.0]})).unwrap();
        assert!(s.mailbox.mail[&id].read);
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
            assert!(s.mailbox.mail[&msg].pinned);
            assert_eq!(s.mailbox.notes[&note].position(), Some((12.0, 34.0)));
            assert!(s.view.collapsed_groups.values().flatten().all(|e| !e.starts_with('\u{1}')));
        }
        // Saved with the mailbox, and gone from settings.json: unpinning sticks.
        let mut s = Session::open(dir.path()).unwrap();
        assert!(s.mailbox.mail[&msg].pinned);
        s.run("mail.pin", &serde_json::json!({"ids": [msg.0]})).unwrap();
        s.run("notes.setPosition", &serde_json::json!({"id": note.0, "position": null})).unwrap();
        drop(s);
        let s = Session::open(dir.path()).unwrap();
        assert!(!s.mailbox.mail[&msg].pinned);
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
            s.load_settings(dir.path());
            assert!(s.mailbox.mail[&msg].pinned, "pinned in memory");
            assert!(!s.pending_ops.is_empty());
            assert!(s.view.collapsed_groups[&inbox].iter().any(|e| e.starts_with("\u{1}mail.pinned=")));
            assert!(settings().contains("mail.pinned="), "{}", settings());
            msg
        };
        // Next start (the store writes again): moved for good.
        let s = Session::open(dir.path()).unwrap();
        assert!(s.mailbox.mail[&msg].pinned);
        assert!(!settings().contains("mail.pinned="));
    }
}
