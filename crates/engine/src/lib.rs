//! MailCraft engine (layer L4): the session and its commands.
//!
//! **Everything is a command.** Each user-visible action is a [`CommandSpec`] with a stable id (`mail.reply`,
//! `folder.new`, `view.readingPane`), a label, its UI location, a default shortcut, a params description and a
//! `run` function. Ribbon, menus, shortcuts, the CLI, the control channel and the MCP server all go through
//! [`Session::run`], which validates, records undo, journals changes to the store and never panics (panic
//! guard).
//!
//! Interface (frozen for commit 3; add, don't rename): [`CommandSpec`], [`Registry`], [`CmdError`],
//! [`CmdResult`], [`Session`] (see session.rs), [`queries`] (read-only views the UI renders), [`catalog`].

pub mod accounts;
pub mod archive;
pub mod catalog;
pub mod cmd;
pub mod compose;
pub mod intl;
pub mod junk;
pub mod new_mail;
pub mod queries;
pub mod reminders;
pub mod rules_exec;
pub mod sample;
pub mod search_index;
mod session;
pub mod settings;
pub mod smime;
pub mod sync;
pub mod troubleshoot;
pub mod undo;

use serde_json::Value;

pub use accounts::{FileSecretStore, MemorySecretStore, SecretStore};
pub use mailcraft_model as model;
pub use session::{DEFAULT_QUICK_ACCESS, Module, Notice, SearchScope, Selection, Session, ViewState};
pub use sync::{Connection, Plan, SyncStatus, TestReport};

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum CmdError {
    #[error("unknown command `{0}`")]
    Unknown(String),
    #[error("bad parameters: {0}")]
    Params(String),
    #[error("not available: {0}")]
    Disabled(String),
    #[error("{0}")]
    Failed(String),
}

impl From<mailcraft_model::ModelError> for CmdError {
    fn from(e: mailcraft_model::ModelError) -> Self {
        CmdError::Failed(e.to_string())
    }
}

pub type CmdResult = Result<Value, CmdError>;

/// A registered command.
#[derive(Clone)]
pub struct CommandSpec {
    /// Stable id: `<area>.<camelCase>` (areas: file, mail, folder, view, compose, format, insert, search, nav,
    /// calendar, people, tasks, notes, rules, quicksteps, categories, sendReceive, options, help, app).
    pub id: &'static str,
    pub label: &'static str,
    /// Where it lives in the UI: `Home › Respond`, `View › Layout`, `Context › Message`, `Compose › Message`…
    pub location: &'static str,
    /// Default shortcut in portable form (`Mod+R`, `Mod+Shift+M`, `F9`, `Delete`). `Mod` = Ctrl (⌘ on macOS).
    pub shortcut: &'static str,
    /// Parameter description for agents (`{"folder": number, "ids": [number]}`), `""` when none.
    pub params: &'static str,
    pub mutates: bool,
    /// `None` = enabled; `Some(reason)` = disabled.
    pub enabled: fn(&Session) -> Option<&'static str>,
    pub run: fn(&mut Session, &Value) -> CmdResult,
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSpec").field("id", &self.id).finish()
    }
}

/// Always-enabled predicate.
pub fn always(_: &Session) -> Option<&'static str> {
    None
}

/// All commands, by id.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub specs: Vec<CommandSpec>,
}

impl Registry {
    pub fn new(mut specs: Vec<CommandSpec>) -> Self {
        specs.sort_by_key(|s| s.id);
        specs.dedup_by_key(|s| s.id);
        Self { specs }
    }
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.specs.binary_search_by_key(&id, |s| s.id).ok().and_then(|i| self.specs.get(i))
    }
    pub fn len(&self) -> usize {
        self.specs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
    /// Command bound to a portable shortcut (`Mod+R`).
    pub fn by_shortcut(&self, shortcut: &str) -> Option<&CommandSpec> {
        self.specs.iter().find(|s| {
            !s.shortcut.is_empty() && s.shortcut.split(" / ").any(|k| k.eq_ignore_ascii_case(shortcut))
        })
    }
}
