//! MailCraft data model (layer L1).
//!
//! Plain data plus pure operations. No I/O: persistence is `mailcraft-store`, networking is `mailcraft-net`.
//! Behaviour follows the black-box observations in `plan/outlook/` (folder order, count badges, date groups,
//! list time formats, subject prefixes).
//!
//! Interface rule for parallel work: the public types below are frozen for commit 2. Add fields only with
//! `#[serde(default)]` and keep `Default` derivable so other crates can build values with `..Default::default()`.

pub mod account;
pub mod calendar;
pub mod category;
pub mod contact;
pub mod dates;
pub mod folder;
pub mod ids;
pub mod item;
pub mod junk;
pub mod mailbox;
pub mod note;
pub mod quickstep;
pub mod rich;
pub mod rule;
pub mod signature;
pub mod subject;
pub mod task;
pub mod text;
pub mod thread;
pub mod view;

#[cfg(test)]
mod tests;

pub use account::{Account, AccountKind, Security, ServerSettings};
pub use calendar::{Appointment, Attendee, BusyStatus, Recurrence};
pub use category::{Category, CategoryColor};
pub use contact::Contact;
pub use dates::{DateGroup, ListTime};
pub use folder::{Folder, FolderKind, FolderRole};
pub use ids::{AccountId, FolderId, ItemId};
pub use item::{
    AttachmentMeta, Body, BodyFormat, FlagState, FocusClass, Importance, MailItem, ResponseState, Sensitivity,
};
pub use junk::{JunkLevel, JunkList, JunkSettings};
pub use mailbox::Mailbox;
pub use note::{Note, NoteColor};
pub use quickstep::{QuickStep, QuickStepAction};
pub use rich::{Block, Doc, Inline, ParaStyle, TextStyle};
pub use rule::{Rule, RuleAction, RuleCondition};
pub use signature::{AccountSignatures, RecentRecipient, Signature};
pub use task::{Task, TaskPriority, TaskStatus};
pub use thread::{Conversation, thread};
pub use view::{ArrangeBy, FolderPaneState, ListFilter, ReadingPanePos, SortOrder, ViewLayout, ViewSettings};

pub use mailcraft_mime::{DateTime, Mailbox as Address};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    #[error("no such folder {0:?}")]
    NoFolder(FolderId),
    #[error("no such item {0:?}")]
    NoItem(ItemId),
    #[error("no such account {0:?}")]
    NoAccount(AccountId),
    #[error("operation not allowed: {0}")]
    NotAllowed(String),
    #[error("invalid value: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, ModelError>;
