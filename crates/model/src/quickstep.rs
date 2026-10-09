use serde::{Deserialize, Serialize};

use crate::{FolderId, Importance};

/// Quick Steps actions (Home › Quick Steps). Defaults: Move to ?, To Manager, Team Email, Done, Reply & Delete,
/// Create New.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuickStepAction {
    /// `None` = ask for the folder on first use.
    MoveTo(Option<FolderId>),
    CopyTo(Option<FolderId>),
    Delete,
    PermanentlyDelete,
    MarkRead,
    MarkUnread,
    SetImportance(Importance),
    Categorize(String),
    ClearCategories,
    Flag(String),
    ClearFlag,
    MarkComplete,
    NewMessageTo(Vec<String>),
    Forward(Vec<String>),
    Reply,
    ReplyAll,
    NewMeeting,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickStep {
    pub id: u64,
    pub name: String,
    pub actions: Vec<QuickStepAction>,
    /// "Ctrl+Shift+1" … "Ctrl+Shift+9" or empty.
    pub shortcut: String,
    pub tooltip: String,
}

impl QuickStep {
    pub fn defaults() -> Vec<QuickStep> {
        let step = |id: u64, name: &str, actions: Vec<QuickStepAction>, tooltip: &str| QuickStep {
            id,
            name: name.to_string(),
            actions,
            shortcut: String::new(),
            tooltip: tooltip.to_string(),
        };
        vec![
            step(
                1,
                "Move to: ?",
                vec![QuickStepAction::MoveTo(None), QuickStepAction::MarkRead],
                "Moves the selected message to a folder you pick the first time, and marks it as read.",
            ),
            step(
                2,
                "To Manager",
                vec![QuickStepAction::Forward(Vec::new())],
                "Forwards the message to your manager.",
            ),
            step(
                3,
                "Team Email",
                vec![QuickStepAction::NewMessageTo(Vec::new())],
                "Starts a new message to your team.",
            ),
            step(
                4,
                "Done",
                vec![QuickStepAction::MarkComplete, QuickStepAction::MarkRead, QuickStepAction::MoveTo(None)],
                "Marks the message complete and read, then files it in a folder.",
            ),
            step(
                5,
                "Reply & Delete",
                vec![QuickStepAction::Reply, QuickStepAction::Delete],
                "Opens a reply and moves the original to Deleted Items.",
            ),
            step(6, "Create New", Vec::new(), "Sets up a new quick step."),
        ]
    }
    /// True when the step still needs its first-use setup (an unset target folder, or no actions at all).
    pub fn needs_setup(&self) -> bool {
        self.actions.is_empty()
            || self.actions.iter().any(|a| {
                matches!(a, QuickStepAction::MoveTo(None) | QuickStepAction::CopyTo(None))
                    || matches!(a, QuickStepAction::Forward(to) | QuickStepAction::NewMessageTo(to) if to.is_empty())
            })
    }
}
