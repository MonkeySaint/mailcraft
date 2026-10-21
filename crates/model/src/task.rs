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

/// Where a task request stands.
///
/// On the copy kept by the person who assigned the task it follows the assignee's answers (task responses
/// and status reports); on a task accepted from a task request it is [`TaskRequestState::Accepted`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskRequestState {
    /// Not a task request.
    #[default]
    None,
    /// Sent; no answer yet.
    Waiting,
    Accepted,
    Declined,
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
    /// iCalendar UID shared by the requester's copy and the assignee's task (RFC 5546 iTIP); empty for
    /// tasks that never took part in a task request.
    #[serde(default)]
    pub uid: String,
    /// Where the task request stands (see [`TaskRequestState`]).
    #[serde(default)]
    pub request_state: TaskRequestState,
    /// When the last task response or status report about this task was sent or applied.
    #[serde(default)]
    pub last_report: Option<DateTime>,
    /// When the task was last created or changed in MailCraft (the Modified column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime>,
}

impl Task {
    /// True when the task is complete.
    pub fn is_complete(&self) -> bool {
        self.status == TaskStatus::Completed
    }

    /// True for a task I accepted from someone else's task request.
    pub fn is_requested(&self) -> bool {
        self.requested_by.is_some()
    }

    /// True for my copy of a task I assigned to other people.
    pub fn is_assigned_by_me(&self) -> bool {
        self.requested_by.is_none() && !self.assigned_to.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_tasks_load_without_request_fields() {
        let json = r#"{"id":1,"folder":2,"subject":"Old","start":null,"due":null,"status":"NotStarted",
            "priority":"Normal","percent_complete":0,"reminder":null,"body":"","categories":[],
            "completed":null,"from_mail":null}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.request_state, TaskRequestState::None);
        assert!(t.uid.is_empty() && !t.is_requested() && !t.is_assigned_by_me());
        let mine = Task { assigned_to: vec!["pat@example.org".into()], ..t.clone() };
        assert!(mine.is_assigned_by_me());
        let got = Task { requested_by: Some("boss@example.com".into()), ..mine };
        assert!(got.is_requested() && !got.is_assigned_by_me());
    }
}
