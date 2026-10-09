use serde::{Deserialize, Serialize};

use crate::{DateTime, FolderId, ItemId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    #[default]
    NotStarted,
    InProgress,
    Completed,
    WaitingOnSomeoneElse,
    Deferred,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: ItemId,
    pub folder: FolderId,
    pub subject: String,
    pub start: Option<DateTime>,
    pub due: Option<DateTime>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    /// 0..=100
    pub percent_complete: u8,
    pub reminder: Option<DateTime>,
    pub body: String,
    pub categories: Vec<String>,
    pub completed: Option<DateTime>,
    /// Flagged mail shows up in the To-Do list; this links back to it.
    pub from_mail: Option<ItemId>,
}
