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
    /// IANA time zone the times were entered in ("America/Toronto"); `start`/`end` stay UTC instants, and
    /// recurrences expand in this zone's wall-clock time (DST-correct). `None` = the start's fixed offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl Appointment {
    /// The IANA zone the appointment's times were entered in, when it names one (trimmed, non-empty).
    pub fn zone(&self) -> Option<&str> {
        self.tz.as_deref().map(str::trim).filter(|z| !z.is_empty())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Calendar views (`plan/outlook/modules.md` › Calendar › View tab)
// ---------------------------------------------------------------------------------------------------------

/// Calendar arrangement (Day, Work Week, Week, Month, Schedule View).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarMode {
    Day,
    WorkWeek,
    Week,
    #[default]
    Month,
    Schedule,
}

impl CalendarMode {
    pub const ALL: [CalendarMode; 5] = [
        CalendarMode::Day,
        CalendarMode::WorkWeek,
        CalendarMode::Week,
        CalendarMode::Month,
        CalendarMode::Schedule,
    ];

    pub fn parse(s: &str) -> Option<CalendarMode> {
        match s {
            "day" => Some(CalendarMode::Day),
            "workWeek" => Some(CalendarMode::WorkWeek),
            "week" => Some(CalendarMode::Week),
            "month" => Some(CalendarMode::Month),
            "schedule" => Some(CalendarMode::Schedule),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            CalendarMode::Day => "day",
            CalendarMode::WorkWeek => "workWeek",
            CalendarMode::Week => "week",
            CalendarMode::Month => "month",
            CalendarMode::Schedule => "schedule",
        }
    }
    /// Ribbon label.
    pub fn label(self) -> &'static str {
        match self {
            CalendarMode::Day => "Day",
            CalendarMode::WorkWeek => "Work Week",
            CalendarMode::Week => "Week",
            CalendarMode::Month => "Month",
            CalendarMode::Schedule => "Schedule View",
        }
    }
}

/// Daily Task List display under day/week views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DailyTaskList {
    #[default]
    Normal,
    Minimized,
    Off,
}

/// View › Change View: the calendar grid, the grid with item text (Preview), or a table of all items (List)
/// or of current and future items (Active).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarCurrentView {
    #[default]
    Calendar,
    Preview,
    List,
    Active,
}

impl CalendarCurrentView {
    pub const ALL: [CalendarCurrentView; 4] = [
        CalendarCurrentView::Calendar,
        CalendarCurrentView::Preview,
        CalendarCurrentView::List,
        CalendarCurrentView::Active,
    ];

    /// Menu label (also accepted by [`CalendarCurrentView::parse`]).
    pub fn label(self) -> &'static str {
        match self {
            CalendarCurrentView::Calendar => "Calendar",
            CalendarCurrentView::Preview => "Preview",
            CalendarCurrentView::List => "List",
            CalendarCurrentView::Active => "Active",
        }
    }
    /// Case-insensitive label.
    pub fn parse(s: &str) -> Option<CalendarCurrentView> {
        Self::ALL.into_iter().find(|v| v.label().eq_ignore_ascii_case(s.trim()))
    }
    /// List and Active show a table instead of the grid.
    pub fn is_table(self) -> bool {
        matches!(self, CalendarCurrentView::List | CalendarCurrentView::Active)
    }
}

/// Columns of the List / Active table views (also their sort keys).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarColumn {
    Subject,
    Location,
    #[default]
    Start,
    End,
    Recurrence,
    Categories,
    Calendar,
}

impl CalendarColumn {
    pub const ALL: [CalendarColumn; 7] = [
        CalendarColumn::Subject,
        CalendarColumn::Location,
        CalendarColumn::Start,
        CalendarColumn::End,
        CalendarColumn::Recurrence,
        CalendarColumn::Categories,
        CalendarColumn::Calendar,
    ];
    pub fn label(self) -> &'static str {
        match self {
            CalendarColumn::Subject => "Subject",
            CalendarColumn::Location => "Location",
            CalendarColumn::Start => "Start",
            CalendarColumn::End => "End",
            CalendarColumn::Recurrence => "Recurrence Pattern",
            CalendarColumn::Categories => "Categories",
            CalendarColumn::Calendar => "Calendar",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            CalendarColumn::Subject => "subject",
            CalendarColumn::Location => "location",
            CalendarColumn::Start => "start",
            CalendarColumn::End => "end",
            CalendarColumn::Recurrence => "recurrence",
            CalendarColumn::Categories => "categories",
            CalendarColumn::Calendar => "calendar",
        }
    }
    /// By name or label, case-insensitive ("start", "Recurrence Pattern").
    pub fn parse(s: &str) -> Option<CalendarColumn> {
        let s = s.trim();
        Self::ALL.into_iter().find(|c| c.name().eq_ignore_ascii_case(s) || c.label().eq_ignore_ascii_case(s))
    }
}

/// The calendar view the UI shows. Held by the UI, passed to and returned from the view commands, and saved
/// per calendar folder ([`CalendarProps::view`], without the date).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarViewState {
    pub mode: CalendarMode,
    /// Selected date "YYYY-MM-DD" (empty = today).
    pub date: String,
    /// Days shown in Day arrangement (1, or 7 for Next 7 Days).
    pub days: u8,
    /// Time Scale slot size in minutes.
    pub time_scale: u8,
    /// Show only working hours in day/week views.
    pub working_hours: bool,
    /// Overlay (true) vs side-by-side when several calendars are shown.
    pub overlay: bool,
    pub daily_task_list: DailyTaskList,
    /// View › Change View.
    pub current: CalendarCurrentView,
    /// Sort column of the List / Active views.
    pub sort: CalendarColumn,
    pub ascending: bool,
    /// View › Layout › Reading Pane (Off by default in Calendar).
    pub reading_pane: crate::ReadingPanePos,
    /// View › Layout › Use Tighter Spacing.
    pub tighter_spacing: bool,
}

impl Default for CalendarViewState {
    fn default() -> Self {
        Self {
            mode: CalendarMode::Month,
            date: String::new(),
            days: 1,
            time_scale: 30,
            working_hours: false,
            overlay: false,
            daily_task_list: DailyTaskList::Normal,
            current: CalendarCurrentView::Calendar,
            sort: CalendarColumn::Start,
            ascending: true,
            reading_pane: crate::ReadingPanePos::Off,
            tighter_spacing: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Calendar folder properties
// ---------------------------------------------------------------------------------------------------------

/// Where a calendar folder's items come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarSource {
    /// Items made here (or imported); editable.
    #[default]
    Local,
    /// Add Calendar › From Internet: an iCalendar subscription, read-only, refreshed periodically.
    Internet,
    /// Add Calendar › From File: a copy of an .ics file's events (editable).
    File,
}

/// An internet calendar subscription (Add Calendar › From Internet).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Subscription {
    /// The http(s) URL fetched (`webcal://` is stored as `http://`, `webcals://` as `https://`).
    pub url: String,
    /// Minutes between refreshes (the publisher's REFRESH-INTERVAL / X-PUBLISHED-TTL when it gives one).
    pub refresh_min: u32,
    /// Unix seconds of the last fetch attempt.
    pub last_fetch: Option<i64>,
    /// Unix seconds of the last successful update.
    pub last_update: Option<i64>,
    /// Error of the last attempt, if it failed.
    pub last_error: Option<String>,
    /// HTTP validators for conditional requests (RFC 9110 §13).
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Time zones the calendar shows (Options › Calendar › Time zones).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarTimeZones {
    /// IANA id of the primary zone ("" = the session's fixed UTC offset).
    pub zone: String,
    pub label: String,
    pub show_second: bool,
    /// IANA id of the second zone shown in day and week views.
    pub second_zone: String,
    pub second_label: String,
}

/// Per-calendar-folder settings: colour, source (and subscription), saved view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarProps {
    /// View › Color name (`None` = Automatic).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub source: CalendarSource,
    /// Items cannot be changed (internet subscriptions).
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription: Option<Subscription>,
    /// The folder's saved view (date fields cleared).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<CalendarViewState>,
    /// Rises with every saved view change (the UI adopts a saved view newer than the last it saw).
    #[serde(skip_serializing_if = "is_zero")]
    pub view_seq: u64,
    /// The calendar's time zones (kept on the default calendar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zones: Option<CalendarTimeZones>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl CalendarProps {
    /// True when nothing differs from the defaults (nothing to store).
    pub fn is_default(&self) -> bool {
        *self == CalendarProps::default()
    }
}
