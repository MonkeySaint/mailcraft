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

/// Task recurrence (the task window's Recurrence dialog).
///
/// Either the due dates follow an RFC 5545 RRULE anchored at `start` (the first occurrence's due date), or
/// — "Regenerate new task" — each new task falls due `regenerate_days` after the previous one was completed.
/// Completing an occurrence keeps a completed copy and moves the task to its next occurrence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskRecurrence {
    /// RRULE value without the `RRULE:` name (e.g. `FREQ=WEEKLY;BYDAY=MO`); empty when regenerating.
    pub rrule: String,
    /// Due date of the first occurrence (DTSTART of the rule).
    pub start: Option<DateTime>,
    /// Regenerate: days between completing one task and the next one's due date (0 = follow `rrule`).
    pub regenerate_days: u32,
    /// Occurrences completed so far.
    pub completed_count: u32,
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
    /// Recurring task.
    #[serde(default)]
    pub recurrence: Option<TaskRecurrence>,
    /// Private (Tags › Private).
    #[serde(default)]
    pub private: bool,
    /// People this task was assigned to with a task request ("Name <address>"); shown in the Assigned view.
    #[serde(default)]
    pub assigned_to: Vec<String>,
    /// Who assigned this task to me (the task request's organizer), when it came from a task request.
    #[serde(default)]
    pub requested_by: Option<String>,
}

impl Task {
    /// True when the task is complete.
    pub fn is_complete(&self) -> bool {
        self.status == TaskStatus::Completed
    }
}
