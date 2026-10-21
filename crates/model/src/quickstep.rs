use serde::de::{self, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::{FolderId, Importance};

/// The message a New Message To, Forward or Forward as Attachment action makes: its recipients and the
/// editor's Show Options (Cc, Bcc, Subject, Flag, Importance, Text, send automatically).
///
/// Stored as a map; profiles made before the options existed stored only the To list, which still reads
/// (`["a@example.com"]` is a message to that address with no options).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MessageAction {
    pub to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    /// Subject of the message (empty = the usual one: blank for a new message, "FW: …" for a forward).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subject: String,
    /// Text put at the top of the message body.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Importance of the message (`None` = the usual Normal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<Importance>,
    /// Follow-up flag on the message: a Follow Up key ("today", "tomorrow", "thisWeek", "nextWeek",
    /// "noDate"), as the Flag action takes them (`None` = no flag).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    /// Send the message without opening it, after a one-minute delay in the Outbox.
    #[serde(rename = "autoSend", skip_serializing_if = "std::ops::Not::not")]
    pub auto_send: bool,
}

/// Most recipients one field of a [`MessageAction`] (or a New Meeting's attendee list) keeps.
pub const MAX_ACTION_RECIPIENTS: usize = 500;
/// Longest single recipient / attendee entry (chars): an RFC 5321 address (320) with room for a display name.
pub const MAX_ACTION_RECIPIENT_CHARS: usize = 512;
/// Longest subject (chars) of a [`MessageAction`].
pub const MAX_ACTION_SUBJECT: usize = 1_000;
/// Longest text (chars) of a [`MessageAction`].
pub const MAX_ACTION_TEXT: usize = 64 * 1024;

impl MessageAction {
    /// A message to these people with no options.
    pub fn to(to: Vec<String>) -> MessageAction {
        MessageAction { to, ..MessageAction::default() }
    }

    /// No recipient in any field yet (asked for on first use).
    pub fn has_no_recipients(&self) -> bool {
        self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty()
    }

    /// True when any option beyond the To list is set (the editor then shows the options).
    pub fn has_options(&self) -> bool {
        !self.cc.is_empty()
            || !self.bcc.is_empty()
            || !self.subject.trim().is_empty()
            || !self.text.trim().is_empty()
            || self.importance.is_some()
            || self.flag.is_some()
            || self.auto_send
    }

    /// Why these options cannot be stored (`None` = fine): sizes are capped, as the fields come from
    /// command parameters.
    pub fn problem(&self) -> Option<String> {
        for (name, list) in [("To", &self.to), ("Cc", &self.cc), ("Bcc", &self.bcc)] {
            if let Some(why) = recipients_problem(name, list) {
                return Some(why);
            }
        }
        if self.subject.chars().count() > MAX_ACTION_SUBJECT {
            return Some(format!("the subject is longer than {MAX_ACTION_SUBJECT} characters"));
        }
        if self.text.chars().count() > MAX_ACTION_TEXT {
            return Some(format!("the text is longer than {MAX_ACTION_TEXT} characters"));
        }
        None
    }
}

/// Why recipient list `list` (field `name`: "To", "attendee"...) cannot be stored (`None` = fine): at most
/// [`MAX_ACTION_RECIPIENTS`] entries of at most [`MAX_ACTION_RECIPIENT_CHARS`] characters each.
pub fn recipients_problem(name: &str, list: &[String]) -> Option<String> {
    if list.len() > MAX_ACTION_RECIPIENTS {
        return Some(format!("at most {MAX_ACTION_RECIPIENTS} {name} recipients"));
    }
    list.iter()
        .any(|r| r.chars().count() > MAX_ACTION_RECIPIENT_CHARS)
        .then(|| format!("a {name} recipient is longer than {MAX_ACTION_RECIPIENT_CHARS} characters"))
}

/// The stored forms of a [`MessageAction`]: the To list of older profiles, or the full map.
#[derive(Deserialize)]
#[serde(untagged)]
enum MessageActionRepr {
    List(Vec<String>),
    Full(MessageActionFields),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct MessageActionFields {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    text: String,
    importance: Option<Importance>,
    flag: Option<String>,
    #[serde(rename = "autoSend")]
    auto_send: bool,
}

impl<'de> Deserialize<'de> for MessageAction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match MessageActionRepr::deserialize(d)? {
            MessageActionRepr::List(to) => MessageAction::to(to),
            MessageActionRepr::Full(f) => MessageAction {
                to: f.to,
                cc: f.cc,
                bcc: f.bcc,
                subject: f.subject,
                text: f.text,
                importance: f.importance,
                flag: f.flag,
                auto_send: f.auto_send,
            },
        })
    }
}

/// Quick Steps actions (Home › Quick Steps). Defaults: Move to ?, To Manager, Team Email, Done, Reply & Delete
/// (the gallery adds a fixed Create New entry, which is not a stored step).
///
/// Serialised like a derived externally tagged enum (`"Delete"`, `{"MoveTo": 3}`); reading also accepts the
/// forms older profiles stored (`"NewMeeting"` without attendees, a bare To list for message actions).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
    /// A new message (to these people, with these options; no recipients = ask on first use).
    NewMessageTo(MessageAction),
    /// Forward the messages (no recipients = ask on first use).
    Forward(MessageAction),
    Reply,
    ReplyAll,
    /// A new meeting request to these attendees (empty = a meeting with no one invited yet: the step runs
    /// without first-use setup and the meeting form opens with no attendees).
    NewMeeting(Vec<String>),
    /// Forward the messages as attachments (no recipients = ask on first use).
    ForwardAsAttachment(MessageAction),
    /// Reply with a meeting request.
    ReplyWithMeeting,
    /// A task with the message's subject and text.
    CreateTask,
    /// An appointment with the message's subject and text (opened for its time).
    CreateAppointment,
    /// Ignore the conversation.
    Ignore,
    /// Always move the conversation to this folder (`None` = ask on first use).
    AlwaysMoveConversation(Option<FolderId>),
    /// Focused Inbox: move to Focused / to Other.
    MoveToFocused,
    MoveToOther,
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
                vec![QuickStepAction::Forward(MessageAction::default())],
                "Forwards the message to your manager.",
            ),
            step(
                3,
                "Team Email",
                vec![QuickStepAction::NewMessageTo(MessageAction::default())],
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
        ]
    }

    /// The gallery's fixed "Create New" entry. Profiles made by MailCraft 0.1.x stored it as a step of its
    /// own (no actions); [`crate::Mailbox::drop_legacy_create_new_step`] removes that copy.
    pub const CREATE_NEW: &'static str = "Create New";

    /// True for the stored "Create New" placeholder of an older profile.
    pub fn is_legacy_create_new(&self) -> bool {
        self.actions.is_empty() && self.name.trim().eq_ignore_ascii_case(Self::CREATE_NEW)
    }
    /// True when the step still needs its first-use setup (an unset target folder, or no actions at all).
    pub fn needs_setup(&self) -> bool {
        self.actions.is_empty() || self.needs_folder() || self.needs_recipients()
    }

    /// True when an action still asks for its folder on first use.
    pub fn needs_folder(&self) -> bool {
        self.actions.iter().any(|a| {
            matches!(
                a,
                QuickStepAction::MoveTo(None)
                    | QuickStepAction::CopyTo(None)
                    | QuickStepAction::AlwaysMoveConversation(None)
            )
        })
    }

    /// True when a message action still asks for its recipients on first use. A New Meeting without
    /// attendees does not: it opens an empty meeting form.
    pub fn needs_recipients(&self) -> bool {
        self.actions.iter().any(|a| a.message().is_some_and(MessageAction::has_no_recipients))
    }
}

impl QuickStepAction {
    /// The message options of a New Message To / Forward / Forward as Attachment action.
    pub fn message(&self) -> Option<&MessageAction> {
        match self {
            QuickStepAction::NewMessageTo(m)
            | QuickStepAction::Forward(m)
            | QuickStepAction::ForwardAsAttachment(m) => Some(m),
            _ => None,
        }
    }

    /// Mutable [`QuickStepAction::message`].
    pub fn message_mut(&mut self) -> Option<&mut MessageAction> {
        match self {
            QuickStepAction::NewMessageTo(m)
            | QuickStepAction::Forward(m)
            | QuickStepAction::ForwardAsAttachment(m) => Some(m),
            _ => None,
        }
    }

    /// The action of a variant without data (`"Delete"`), as stored.
    fn unit(name: &str) -> Option<QuickStepAction> {
        Some(match name {
            "Delete" => QuickStepAction::Delete,
            "PermanentlyDelete" => QuickStepAction::PermanentlyDelete,
            "MarkRead" => QuickStepAction::MarkRead,
            "MarkUnread" => QuickStepAction::MarkUnread,
            "ClearCategories" => QuickStepAction::ClearCategories,
            "ClearFlag" => QuickStepAction::ClearFlag,
            "MarkComplete" => QuickStepAction::MarkComplete,
            "Reply" => QuickStepAction::Reply,
            "ReplyAll" => QuickStepAction::ReplyAll,
            // Older profiles: a meeting step had no attendees.
            "NewMeeting" => QuickStepAction::NewMeeting(Vec::new()),
            "ReplyWithMeeting" => QuickStepAction::ReplyWithMeeting,
            "CreateTask" => QuickStepAction::CreateTask,
            "CreateAppointment" => QuickStepAction::CreateAppointment,
            "Ignore" => QuickStepAction::Ignore,
            "MoveToFocused" => QuickStepAction::MoveToFocused,
            "MoveToOther" => QuickStepAction::MoveToOther,
            _ => return None,
        })
    }
}

const ACTION_NAMES: &[&str] = &[
    "MoveTo",
    "CopyTo",
    "Delete",
    "PermanentlyDelete",
    "MarkRead",
    "MarkUnread",
    "SetImportance",
    "Categorize",
    "ClearCategories",
    "Flag",
    "ClearFlag",
    "MarkComplete",
    "NewMessageTo",
    "Forward",
    "Reply",
    "ReplyAll",
    "NewMeeting",
    "ForwardAsAttachment",
    "ReplyWithMeeting",
    "CreateTask",
    "CreateAppointment",
    "Ignore",
    "AlwaysMoveConversation",
    "MoveToFocused",
    "MoveToOther",
];

struct ActionVisitor;

impl<'de> Visitor<'de> for ActionVisitor {
    type Value = QuickStepAction;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a quick step action")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<QuickStepAction, E> {
        QuickStepAction::unit(v).ok_or_else(|| E::unknown_variant(v, ACTION_NAMES))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<QuickStepAction, A::Error> {
        let Some(name) = map.next_key::<String>()? else {
            return Err(de::Error::invalid_length(0, &"one action name"));
        };
        let action = match name.as_str() {
            "MoveTo" => QuickStepAction::MoveTo(map.next_value()?),
            "CopyTo" => QuickStepAction::CopyTo(map.next_value()?),
            "SetImportance" => QuickStepAction::SetImportance(map.next_value()?),
            "Categorize" => QuickStepAction::Categorize(map.next_value()?),
            "Flag" => QuickStepAction::Flag(map.next_value()?),
            "NewMessageTo" => QuickStepAction::NewMessageTo(map.next_value()?),
            "Forward" => QuickStepAction::Forward(map.next_value()?),
            "ForwardAsAttachment" => QuickStepAction::ForwardAsAttachment(map.next_value()?),
            "NewMeeting" => {
                let to: Option<Vec<String>> = map.next_value()?;
                QuickStepAction::NewMeeting(to.unwrap_or_default())
            }
            "AlwaysMoveConversation" => QuickStepAction::AlwaysMoveConversation(map.next_value()?),
            other => match QuickStepAction::unit(other) {
                Some(a) => {
                    map.next_value::<IgnoredAny>()?;
                    a
                }
                None => return Err(de::Error::unknown_variant(other, ACTION_NAMES)),
            },
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(de::Error::custom("one quick step action per entry"));
        }
        Ok(action)
    }
}

impl<'de> Deserialize<'de> for QuickStepAction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ActionVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_round_trip_and_older_forms_still_read() {
        let all = vec![
            QuickStepAction::MoveTo(Some(FolderId(3))),
            QuickStepAction::MoveTo(None),
            QuickStepAction::Delete,
            QuickStepAction::SetImportance(Importance::High),
            QuickStepAction::Flag("today".into()),
            QuickStepAction::NewMessageTo(MessageAction {
                to: vec!["team@example.com".into()],
                cc: vec!["lead@example.org".into()],
                subject: "Status".into(),
                text: "Hello".into(),
                importance: Some(Importance::Low),
                flag: Some("tomorrow".into()),
                auto_send: true,
                ..MessageAction::default()
            }),
            QuickStepAction::Forward(MessageAction::to(vec!["boss@example.com".into()])),
            QuickStepAction::NewMeeting(vec!["ann@example.net".into()]),
            QuickStepAction::ForwardAsAttachment(MessageAction::default()),
            QuickStepAction::MoveToOther,
        ];
        let json = serde_json::to_string(&all).unwrap();
        let back: Vec<QuickStepAction> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, all);

        // What MailCraft 0.1 stored: a bare To list and a unit New Meeting.
        let old = r#"[{"Forward":["a@example.com"]},"NewMeeting",{"NewMessageTo":[]},"MarkRead"]"#;
        let back: Vec<QuickStepAction> = serde_json::from_str(old).unwrap();
        assert_eq!(
            back,
            vec![
                QuickStepAction::Forward(MessageAction::to(vec!["a@example.com".into()])),
                QuickStepAction::NewMeeting(Vec::new()),
                QuickStepAction::NewMessageTo(MessageAction::default()),
                QuickStepAction::MarkRead,
            ]
        );
        // Junk is an error, not a panic.
        for bad in [r#""Nope""#, r#"{}"#, r#"{"MoveTo":1,"Delete":null}"#, r#"{"Forward":5}"#, "7"] {
            assert!(serde_json::from_str::<QuickStepAction>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn message_options_need_recipients_and_cap_sizes() {
        let step = |m: MessageAction| QuickStep {
            actions: vec![QuickStepAction::Forward(m)],
            ..QuickStep::default()
        };
        assert!(step(MessageAction::default()).needs_recipients());
        assert!(
            !step(MessageAction { cc: vec!["a@example.com".into()], ..Default::default() })
                .needs_recipients()
        );
        let big = MessageAction { text: "x".repeat(MAX_ACTION_TEXT + 1), ..Default::default() };
        assert!(big.problem().is_some());
        assert!(MessageAction::to(vec!["a@example.com".into()]).problem().is_none());
        assert!(!MessageAction::to(vec!["a@example.com".into()]).has_options());
        assert!(MessageAction { auto_send: true, ..Default::default() }.has_options());
        // Each entry is capped too (junk from the command channel).
        let long =
            MessageAction { bcc: vec!["x".repeat(MAX_ACTION_RECIPIENT_CHARS + 1)], ..Default::default() };
        assert!(long.problem().is_some_and(|w| w.contains("Bcc")));
        assert!(MessageAction::to(vec!["y".repeat(MAX_ACTION_RECIPIENT_CHARS)]).problem().is_none());
        let many = vec!["a@example.com".to_owned(); MAX_ACTION_RECIPIENTS + 1];
        assert!(recipients_problem("attendee", &many).is_some());
        // Regression: a meeting without attendees runs as it is (an empty meeting form), no setup needed.
        let meeting =
            QuickStep { actions: vec![QuickStepAction::NewMeeting(Vec::new())], ..QuickStep::default() };
        assert!(!meeting.needs_recipients());
        assert!(!meeting.needs_setup());
    }
}
