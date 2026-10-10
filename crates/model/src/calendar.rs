use serde::{Deserialize, Serialize};

use crate::{DateTime, FolderId, ItemId};
use mailcraft_mime::Mailbox;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusyStatus {
    Free,
    WorkingElsewhere,
    Tentative,
    #[default]
    Busy,
    OutOfOffice,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttendeeRole {
    #[default]
    Required,
    Optional,
    Resource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseStatus {
    #[default]
    None,
    Accepted,
    Tentative,
    Declined,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub mailbox: Mailbox,
    pub role: AttendeeRole,
    pub response: ResponseStatus,
}

/// Recurrence kept as an RFC 5545 RRULE value (e.g. "FREQ=WEEKLY;BYDAY=MO,WE") plus exception dates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recurrence {
    pub rrule: String,
    pub exdates: Vec<DateTime>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Appointment {
    pub id: ItemId,
    pub folder: FolderId,
    pub uid: String,
    pub subject: String,
    pub location: String,
    pub start: DateTime,
    pub end: DateTime,
    pub all_day: bool,
    pub show_as: BusyStatus,
    /// Minutes before start; `None` = no reminder.
    pub reminder_min: Option<u32>,
    pub organizer: Option<Mailbox>,
    pub attendees: Vec<Attendee>,
    pub recurrence: Option<Recurrence>,
    /// Set on an exception: this appointment replaces the occurrence of the series (same UID) that started
    /// at this time (RFC 5545 RECURRENCE-ID). The series lists that time in its EXDATEs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<DateTime>,
    pub categories: Vec<String>,
    pub private: bool,
    pub body: String,
    /// Meeting (has attendees) vs appointment.
    pub is_meeting: bool,
}
