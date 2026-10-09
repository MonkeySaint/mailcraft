//! Calendar commands (`calendar.*`), plus the shared PIM helpers ([`pim`]) that the People, Tasks and Notes
//! modules use for item CRUD (new / update / delete / copy / move / categorize).
//!
//! **View state.** The session has no calendar fields (its interface is frozen), so the arrangement
//! commands are pure functions of a [`CalendarViewState`] that the UI holds (commit 4): the caller passes
//! the current state as `{"view": {...}}` (omitted = the default Month view on today) and the command
//! returns the new state as `{"view": {...}}`. Toggles (`calendar.overlay`, `calendar.workingHours`) flip
//! the passed state unless `{"on": bool}` is given. `calendar.color` returns the chosen colour for the
//! calendar folder; the UI keeps it per folder. [`view_enabled`] answers the view-dependent enable rules
//! (Time Scale, Working Hours and Daily Task List are unavailable in Month view).
//!
//! **Items.** Appointments live in calendar folders (`Mailbox.appointments`). `calendar.newAppointment`
//! creates one when field params are given (defaults: starts at the next half hour, lasts 30 minutes,
//! reminder 15 minutes before) and otherwise asks the UI to open a blank appointment window.
//! `calendar.newMeeting` creates a meeting (an appointment with attendees) and opens a compose with the
//! invitation (plain-text summary plus an RFC 5545 `text/calendar` attachment).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use self::pim::Kind;
use crate::cmd::util::{opt_str, opt_u64};
use crate::compose::{ComposeAttachment, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Module, Session, always};
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::{
    Address, Appointment, Attendee, BusyStatus, DateTime, Doc, FolderId, FolderKind, ItemId, ListTime,
    Recurrence,
};

/// Shared helpers for PIM items (appointments, contacts, tasks, notes).
pub mod pim {
    use serde_json::{Value, json};

    use crate::cmd::util::{opt_bool, opt_folder, opt_u64, req_str};
    use crate::{CmdError, CmdResult, Notice, Session};
    use mailcraft_model::{
        Address, Category, DateTime, FolderId, FolderKind, FolderRole, ItemId, Mailbox, dates,
    };

    pub const DAY_SECS: i64 = 86_400;
    /// Longest text field accepted from params (bytes).
    pub const MAX_TEXT: usize = 1 << 20;
    /// Most entries accepted in a list param (categories, attendees, members).
    pub const MAX_LIST: usize = 10_000;
    /// Latest instant accepted from params (year 9999).
    const MAX_UNIX: i64 = 253_402_300_799;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        Appointment,
        Contact,
        Task,
        Note,
    }

    impl Kind {
        pub fn role(self) -> FolderRole {
            match self {
                Kind::Appointment => FolderRole::Calendar,
                Kind::Contact => FolderRole::Contacts,
                Kind::Task => FolderRole::Tasks,
                Kind::Note => FolderRole::Notes,
            }
        }
        pub fn folder_kind(self) -> FolderKind {
            match self {
                Kind::Appointment => FolderKind::Calendar,
                Kind::Contact => FolderKind::Contacts,
                Kind::Task => FolderKind::Tasks,
                Kind::Note => FolderKind::Notes,
            }
        }
        /// Lower-case noun for messages.
        pub fn noun(self) -> &'static str {
            match self {
                Kind::Appointment => "appointment",
                Kind::Contact => "contact",
                Kind::Task => "task",
                Kind::Note => "note",
            }
        }
    }

    /// Which PIM map holds `id`.
    pub fn kind_of(mb: &Mailbox, id: ItemId) -> Option<Kind> {
        if mb.appointments.contains_key(&id) {
            Some(Kind::Appointment)
        } else if mb.contacts.contains_key(&id) {
            Some(Kind::Contact)
        } else if mb.tasks.contains_key(&id) {
            Some(Kind::Task)
        } else if mb.notes.contains_key(&id) {
            Some(Kind::Note)
        } else {
            None
        }
    }

    /// Folder of a PIM item.
    pub fn folder_of(mb: &Mailbox, id: ItemId) -> Option<FolderId> {
        match kind_of(mb, id)? {
            Kind::Appointment => mb.appointments.get(&id).map(|a| a.folder),
            Kind::Contact => mb.contacts.get(&id).map(|c| c.folder),
            Kind::Task => mb.tasks.get(&id).map(|t| t.folder),
            Kind::Note => mb.notes.get(&id).map(|n| n.folder),
        }
    }

    fn set_folder(mb: &mut Mailbox, id: ItemId, folder: FolderId) {
        match kind_of(mb, id) {
            Some(Kind::Appointment) => {
                if let Some(a) = mb.appointments.get_mut(&id) {
                    a.folder = folder;
                }
            }
            Some(Kind::Contact) => {
                if let Some(c) = mb.contacts.get_mut(&id) {
                    c.folder = folder;
                }
            }
            Some(Kind::Task) => {
                if let Some(t) = mb.tasks.get_mut(&id) {
                    t.folder = folder;
                }
            }
            Some(Kind::Note) => {
                if let Some(n) = mb.notes.get_mut(&id) {
                    n.folder = folder;
                }
            }
            None => {}
        }
    }

    fn remove(mb: &mut Mailbox, id: ItemId) {
        mb.appointments.remove(&id);
        mb.contacts.remove(&id);
        mb.tasks.remove(&id);
        mb.notes.remove(&id);
    }

    pub fn categories(mb: &Mailbox, id: ItemId) -> Option<&Vec<String>> {
        match kind_of(mb, id)? {
            Kind::Appointment => mb.appointments.get(&id).map(|a| &a.categories),
            Kind::Contact => mb.contacts.get(&id).map(|c| &c.categories),
            Kind::Task => mb.tasks.get(&id).map(|t| &t.categories),
            Kind::Note => mb.notes.get(&id).map(|n| &n.categories),
        }
    }

    pub fn categories_mut(mb: &mut Mailbox, id: ItemId) -> Option<&mut Vec<String>> {
        match kind_of(mb, id)? {
            Kind::Appointment => mb.appointments.get_mut(&id).map(|a| &mut a.categories),
            Kind::Contact => mb.contacts.get_mut(&id).map(|c| &mut c.categories),
            Kind::Task => mb.tasks.get_mut(&id).map(|t| &mut t.categories),
            Kind::Note => mb.notes.get_mut(&id).map(|n| &mut n.categories),
        }
    }

    /// True when `folder` exists and holds items of `kind`.
    pub fn is_kind_folder(s: &Session, folder: FolderId, kind: Kind) -> bool {
        s.mailbox.folder(folder).is_some_and(|f| f.kind == kind.folder_kind())
    }

    /// The default folder for `kind`: the current account's role folder, else any account's, else any folder
    /// of that kind.
    pub fn default_folder(s: &Session, kind: Kind) -> Option<FolderId> {
        if let Some(account) = s.current_account()
            && let Some(f) = s.mailbox.role_folder(account, kind.role())
        {
            return Some(f);
        }
        s.mailbox
            .folders
            .values()
            .find(|f| f.role == Some(kind.role()))
            .or_else(|| s.mailbox.folders.values().find(|f| f.kind == kind.folder_kind()))
            .map(|f| f.id)
    }

    /// The folder of `kind` the user is in: the selected folder when it holds `kind`, else the default.
    pub fn current_folder(s: &Session, kind: Kind) -> Option<FolderId> {
        if is_kind_folder(s, s.selection.folder, kind) {
            return Some(s.selection.folder);
        }
        default_folder(s, kind)
    }

    /// Folder new items of `kind` go to: `params.folder` (must hold `kind`), else [`current_folder`].
    pub fn new_item_folder(s: &Session, p: &Value, kind: Kind) -> Result<FolderId, CmdError> {
        if let Some(f) = opt_folder(p) {
            return if is_kind_folder(s, f, kind) {
                Ok(f)
            } else {
                Err(CmdError::Params(format!("folder {} does not hold {} items", f.0, kind.noun())))
            };
        }
        current_folder(s, kind)
            .ok_or_else(|| CmdError::Failed(format!("there is no folder for {} items", kind.noun())))
    }

    /// Items of `kind` a command acts on: `params.id`, else `params.ids`, else the selection (else focus).
    pub fn targets(s: &Session, p: &Value, kind: Kind) -> Vec<ItemId> {
        let ids = match opt_u64(p, "id") {
            Some(id) => vec![ItemId(id)],
            None => s.target_ids(p),
        };
        let mut seen = std::collections::BTreeSet::new();
        ids.into_iter().filter(|i| kind_of(&s.mailbox, *i) == Some(kind) && seen.insert(*i)).collect()
    }

    /// Selected items of `kind` (selection, else focus).
    pub fn selected(s: &Session, kind: Kind) -> Vec<ItemId> {
        targets(s, &Value::Null, kind)
    }

    /// Like [`targets`] but errors when nothing applies.
    pub fn require_targets(s: &Session, p: &Value, kind: Kind) -> Result<Vec<ItemId>, CmdError> {
        let ids = targets(s, p, kind);
        if ids.is_empty() {
            Err(CmdError::Params(format!("no {} selected (pass `id` or `ids`)", kind.noun())))
        } else {
            Ok(ids)
        }
    }

    /// Asks the UI to open a dialog/window and returns `{"dialog": name}`.
    pub fn dialog(s: &mut Session, name: &str, params: Value) -> CmdResult {
        s.notify(Notice::OpenDialog(name.to_owned(), params));
        Ok(json!({ "dialog": name }))
    }

    /// Delete: items move to their account's Deleted Items; items already there (or without one) are
    /// removed permanently. Returns the ids handled.
    pub fn delete(s: &mut Session, ids: &[ItemId]) -> Vec<ItemId> {
        let mut done = Vec::new();
        for &id in ids {
            let Some(folder) = folder_of(&s.mailbox, id) else {
                continue;
            };
            let deleted = s
                .mailbox
                .folder_account(folder)
                .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems));
            match deleted {
                Some(d) if !s.mailbox.is_in_subtree(folder, d) => set_folder(&mut s.mailbox, id, d),
                _ => remove(&mut s.mailbox, id),
            }
            done.push(id);
        }
        done
    }

    fn check_dest(s: &Session, folder: FolderId, kind: Kind) -> Result<(), CmdError> {
        if is_kind_folder(s, folder, kind) {
            Ok(())
        } else {
            Err(CmdError::Params(format!("folder {} cannot hold {} items", folder.0, kind.noun())))
        }
    }

    /// Copies items into `folder` (fresh ids); returns the new ids.
    pub fn copy_to(
        s: &mut Session,
        ids: &[ItemId],
        folder: FolderId,
        kind: Kind,
    ) -> Result<Vec<ItemId>, CmdError> {
        check_dest(s, folder, kind)?;
        let mut out = Vec::new();
        let stamp = s.now().unix;
        for &id in ids {
            if kind_of(&s.mailbox, id) != Some(kind) {
                continue;
            }
            let new = ItemId(s.mailbox.alloc_id());
            let mb = &mut s.mailbox;
            match kind {
                Kind::Appointment => {
                    if let Some(mut a) = mb.appointments.get(&id).cloned() {
                        a.id = new;
                        a.folder = folder;
                        a.uid = super::new_uid(new, stamp);
                        mb.appointments.insert(new, a);
                    }
                }
                Kind::Contact => {
                    if let Some(mut c) = mb.contacts.get(&id).cloned() {
                        c.id = new;
                        c.folder = folder;
                        mb.contacts.insert(new, c);
                    }
                }
                Kind::Task => {
                    if let Some(mut t) = mb.tasks.get(&id).cloned() {
                        t.id = new;
                        t.folder = folder;
                        t.from_mail = None;
                        mb.tasks.insert(new, t);
                    }
                }
                Kind::Note => {
                    if let Some(mut n) = mb.notes.get(&id).cloned() {
                        n.id = new;
                        n.folder = folder;
                        mb.notes.insert(new, n);
                    }
                }
            }
            out.push(new);
        }
        Ok(out)
    }

    /// Moves items into `folder`; returns the ids that moved.
    pub fn move_to(
        s: &mut Session,
        ids: &[ItemId],
        folder: FolderId,
        kind: Kind,
    ) -> Result<Vec<ItemId>, CmdError> {
        check_dest(s, folder, kind)?;
        let mut out = Vec::new();
        for &id in ids {
            if kind_of(&s.mailbox, id) == Some(kind) && folder_of(&s.mailbox, id) != Some(folder) {
                set_folder(&mut s.mailbox, id, folder);
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Generic `<area>.delete`.
    pub fn run_delete(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
        let ids = require_targets(s, p, kind)?;
        let done = delete(s, &ids);
        Ok(json!({ "deleted": ids_json(&done) }))
    }

    /// Generic `<area>.copy {folder}`.
    pub fn run_copy(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
        let folder = opt_folder(p).ok_or_else(|| CmdError::Params("`folder` (number) required".into()))?;
        let ids = require_targets(s, p, kind)?;
        let out = copy_to(s, &ids, folder, kind)?;
        Ok(json!({ "copies": ids_json(&out) }))
    }

    /// Generic `<area>.move {folder}`.
    pub fn run_move(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
        let folder = opt_folder(p).ok_or_else(|| CmdError::Params("`folder` (number) required".into()))?;
        let ids = require_targets(s, p, kind)?;
        let out = move_to(s, &ids, folder, kind)?;
        Ok(json!({ "moved": ids_json(&out) }))
    }

    /// Generic `<area>.categorize {name, on?}` / `{clear: true}`. Without `on`, toggles: removes the
    /// category when every target has it, else adds it to all.
    pub fn run_categorize(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
        let ids = require_targets(s, p, kind)?;
        if opt_bool(p, "clear") == Some(true) {
            for id in &ids {
                if let Some(c) = categories_mut(&mut s.mailbox, *id) {
                    c.clear();
                }
            }
            return Ok(json!({ "ids": ids_json(&ids), "cleared": true }));
        }
        let name = req_str(p, "name")?;
        let cat = Category::find(&s.mailbox.categories, name)
            .map(|c| c.name.clone())
            .ok_or_else(|| CmdError::Params(format!("no category named \"{name}\"")))?;
        let has = |mb: &Mailbox, id: ItemId| {
            categories(mb, id).is_some_and(|c| c.iter().any(|x| x.trim().eq_ignore_ascii_case(cat.trim())))
        };
        let all = ids.iter().all(|id| has(&s.mailbox, *id));
        let on = opt_bool(p, "on").unwrap_or(!all);
        for id in &ids {
            let present = has(&s.mailbox, *id);
            if let Some(c) = categories_mut(&mut s.mailbox, *id) {
                if on && !present {
                    c.push(cat.clone());
                } else if !on {
                    c.retain(|x| !x.trim().eq_ignore_ascii_case(cat.trim()));
                }
            }
        }
        Ok(json!({ "ids": ids_json(&ids), "category": cat, "on": on }))
    }

    pub fn ids_json(ids: &[ItemId]) -> Value {
        Value::Array(ids.iter().map(|i| json!(i.0)).collect())
    }

    /// True when any of `keys` is present in `p`.
    pub fn has_any(p: &Value, keys: &[&str]) -> bool {
        keys.iter().any(|k| p.get(k).is_some())
    }

    /// Optional text param (string; `null` = empty). Errors on other types and oversize text.
    pub fn text(p: &Value, key: &str) -> Result<Option<String>, CmdError> {
        match p.get(key) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(String::new())),
            Some(Value::String(t)) if t.len() <= MAX_TEXT => Ok(Some(t.clone())),
            Some(Value::String(_)) => Err(CmdError::Params(format!("`{key}` is too long"))),
            Some(_) => Err(CmdError::Params(format!("`{key}` must be a string"))),
        }
    }

    /// Optional list param: an array of strings or one string split on `;`. `null` = empty list.
    pub fn list(p: &Value, key: &str) -> Result<Option<Vec<String>>, CmdError> {
        let out: Vec<String> = match p.get(key) {
            None => return Ok(None),
            Some(Value::Null) => Vec::new(),
            Some(Value::String(t)) => {
                t.split(';').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
            }
            Some(Value::Array(a)) => {
                let mut v = Vec::new();
                for x in a {
                    match x {
                        Value::String(t) if !t.trim().is_empty() => v.push(t.trim().to_owned()),
                        Value::String(_) => {}
                        _ => return Err(CmdError::Params(format!("`{key}` must hold strings"))),
                    }
                }
                v
            }
            Some(_) => return Err(CmdError::Params(format!("`{key}` must be a list of strings"))),
        };
        if out.len() > MAX_LIST || out.iter().any(|x| x.len() > MAX_TEXT) {
            return Err(CmdError::Params(format!("`{key}` is too long")));
        }
        Ok(Some(out))
    }

    /// Parses address strings ("Name <a@b>", "a@b"; several per string separated by `,`) into mailboxes.
    pub fn parse_addresses(items: &[String]) -> Vec<Address> {
        let mut out: Vec<Address> = Vec::new();
        for item in items {
            for a in mailcraft_mime::parse_address_list(item) {
                for m in a.mailboxes() {
                    if !m.addr.is_empty() && !out.iter().any(|o| o.same_address(&m)) && out.len() < MAX_LIST {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    /// Local civil date/time text → instant: "YYYY-MM-DD", "YYYY-MM-DDTHH:MM[:SS]" (or a space for `T`).
    pub fn parse_local(text: &str, tz: i32) -> Option<DateTime> {
        let t = text.trim();
        let (date, time) = match t.split_once(['T', ' ']) {
            Some((d, tm)) => (d, Some(tm.trim())),
            None => (t, None),
        };
        let mut dp = date.split('-');
        let y: i64 = dp.next()?.trim().parse().ok()?;
        let m: u8 = dp.next()?.trim().parse().ok()?;
        let d: u8 = dp.next()?.trim().parse().ok()?;
        if dp.next().is_some() || !(1..=9999).contains(&y) {
            return None;
        }
        let (h, mi, sec) = match time {
            None | Some("") => (0, 0, 0),
            Some(tm) => {
                let mut tp = tm.split(':');
                let h: u8 = tp.next()?.trim().parse().ok()?;
                let mi: u8 = tp.next()?.trim().parse().ok()?;
                let sec: u8 = match tp.next() {
                    Some(x) => x.trim().parse().ok()?,
                    None => 0,
                };
                if tp.next().is_some() {
                    return None;
                }
                (h, mi, sec)
            }
        };
        dates::local_datetime(y, m, d, h, mi, sec, tz)
    }

    /// Optional date param: absent → `None`; `null` → `Some(None)` (clear); a local date/time string or a
    /// unix timestamp → `Some(Some(t))`.
    pub fn date(p: &Value, key: &str, tz: i32) -> Result<Option<Option<DateTime>>, CmdError> {
        let bad = || {
            CmdError::Params(format!("`{key}`: expected \"YYYY-MM-DD\", \"YYYY-MM-DDTHH:MM\" or a unix time"))
        };
        match p.get(key) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(None)),
            Some(Value::String(t)) => parse_local(t, tz).map(|d| Some(Some(d))).ok_or_else(bad),
            Some(Value::Number(n)) => n
                .as_i64()
                .filter(|u| (-MAX_UNIX..=MAX_UNIX).contains(u))
                .map(|u| Some(Some(DateTime::from_unix(u, tz))))
                .ok_or_else(bad),
            Some(_) => Err(bad()),
        }
    }

    /// "YYYY-MM-DD" of `t` in local time.
    pub fn date_string(t: DateTime, tz: i32) -> String {
        let c = t.civil(tz);
        format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
    }

    /// "YYYY-MM-DDTHH:MM" of `t` in local time.
    pub fn datetime_string(t: DateTime, tz: i32) -> String {
        let c = t.civil(tz);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute)
    }

    /// Local midnight starting the day of `t`.
    pub fn local_midnight(t: DateTime, tz: i32) -> DateTime {
        let day = t.day_number(tz);
        let unix = day.saturating_mul(DAY_SECS).saturating_sub(i64::from(tz).saturating_mul(60));
        DateTime::from_unix(unix, tz)
    }

    /// `t` plus `secs` (saturating).
    pub fn add_secs(t: DateTime, secs: i64) -> DateTime {
        DateTime::from_unix(t.unix.saturating_add(secs), t.offset_min)
    }

    /// Today's local date as "YYYY-MM-DD".
    pub fn today(s: &Session) -> String {
        date_string(s.now(), s.tz_offset_min)
    }

    /// Optional bool param that must be a bool when present.
    pub fn flag(p: &Value, key: &str) -> Result<Option<bool>, CmdError> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(CmdError::Params(format!("`{key}` must be true or false"))),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// View state
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

/// Allowed Time Scale slot sizes (minutes).
pub const TIME_SCALES: [u8; 6] = [60, 30, 15, 10, 6, 5];

/// Calendar colours offered by View › Color.
pub const CALENDAR_COLORS: [&str; 10] =
    ["Automatic", "Blue", "Green", "Orange", "Navy Blue", "Yellow", "Teal", "Magenta", "Brown", "Dark Red"];

/// The calendar view the UI shows. Held by the UI; passed to and returned from view commands.
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
        }
    }
}

/// View-dependent enable rules for the UI (`None` = enabled).
pub fn view_enabled(view: &CalendarViewState, id: &str) -> Option<&'static str> {
    match id {
        "calendar.timeScale" | "calendar.workingHours" | "calendar.dailyTaskList"
            if view.mode == CalendarMode::Month =>
        {
            Some("not available in Month view")
        }
        _ => None,
    }
}

/// Reads `params.view` (or the default) and normalises it (date defaults to today; out-of-range values
/// reset).
fn view_param(s: &Session, p: &Value) -> Result<CalendarViewState, CmdError> {
    let mut v = match p.get("view") {
        None | Some(Value::Null) => CalendarViewState::default(),
        Some(x) => serde_json::from_value::<CalendarViewState>(x.clone())
            .map_err(|e| CmdError::Params(format!("`view`: {e}")))?,
    };
    if pim::parse_local(&v.date, s.tz_offset_min).is_none() {
        v.date = pim::today(s);
    }
    if !(1..=14).contains(&v.days) {
        v.days = 1;
    }
    if !TIME_SCALES.contains(&v.time_scale) {
        v.time_scale = 30;
    }
    Ok(v)
}

fn view_result(s: &mut Session, v: &CalendarViewState) -> CmdResult {
    s.view.module = Module::Calendar;
    Ok(json!({ "view": v }))
}

fn check_view(v: &CalendarViewState, id: &str) -> Result<(), CmdError> {
    match view_enabled(v, id) {
        Some(why) => Err(CmdError::Disabled(why.to_owned())),
        None => Ok(()),
    }
}

fn run_arrange(s: &mut Session, p: &Value) -> CmdResult {
    let mode = opt_str(p, "mode")
        .and_then(CalendarMode::parse)
        .ok_or_else(|| CmdError::Params("`mode` must be day|workWeek|week|month|schedule".into()))?;
    let mut v = view_param(s, p)?;
    v.mode = mode;
    v.days = 1;
    view_result(s, &v)
}

fn run_today(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    v.date = pim::today(s);
    view_result(s, &v)
}

fn run_next7(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    v.date = pim::today(s);
    v.mode = CalendarMode::Day;
    v.days = 7;
    view_result(s, &v)
}

fn run_go_to_date(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    let mode = match opt_str(p, "mode") {
        Some(m) => Some(
            CalendarMode::parse(m)
                .filter(|m| *m != CalendarMode::Schedule)
                .ok_or_else(|| CmdError::Params("`mode` must be day|workWeek|week|month".into()))?,
        ),
        None => None,
    };
    let Some(date) = opt_str(p, "date") else {
        let params = json!({ "date": v.date, "mode": mode.unwrap_or(v.mode).name() });
        return pim::dialog(s, "goToDate", params);
    };
    let t = pim::parse_local(date, s.tz_offset_min)
        .ok_or_else(|| CmdError::Params("`date` must be YYYY-MM-DD".into()))?;
    v.date = pim::date_string(t, s.tz_offset_min);
    if let Some(m) = mode {
        v.mode = m;
        v.days = 1;
    }
    view_result(s, &v)
}

fn run_time_scale(s: &mut Session, p: &Value) -> CmdResult {
    let minutes = opt_u64(p, "minutes")
        .and_then(|m| u8::try_from(m).ok())
        .filter(|m| TIME_SCALES.contains(m))
        .ok_or_else(|| CmdError::Params("`minutes` must be 60, 30, 15, 10, 6 or 5".into()))?;
    let mut v = view_param(s, p)?;
    check_view(&v, "calendar.timeScale")?;
    v.time_scale = minutes;
    view_result(s, &v)
}

fn run_working_hours(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    check_view(&v, "calendar.workingHours")?;
    v.working_hours = pim::flag(p, "on")?.unwrap_or(!v.working_hours);
    view_result(s, &v)
}

fn run_overlay(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    v.overlay = pim::flag(p, "on")?.unwrap_or(!v.overlay);
    view_result(s, &v)
}

fn run_daily_task_list(s: &mut Session, p: &Value) -> CmdResult {
    let mode = match opt_str(p, "mode") {
        Some("normal") => DailyTaskList::Normal,
        Some("minimized") => DailyTaskList::Minimized,
        Some("off") => DailyTaskList::Off,
        _ => return Err(CmdError::Params("`mode` must be normal|minimized|off".into())),
    };
    let mut v = view_param(s, p)?;
    check_view(&v, "calendar.dailyTaskList")?;
    v.daily_task_list = mode;
    view_result(s, &v)
}

/// Number of calendar folders in the mailbox.
fn calendar_count(s: &Session) -> usize {
    s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Calendar).count()
}

fn overlay_enabled(s: &Session) -> Option<&'static str> {
    if calendar_count(s) < 2 { Some("only one calendar") } else { None }
}

/// Calendar a command targets: `params.calendar` (name), `params.folder`, the selected calendar, else the
/// default calendar.
fn target_calendar(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    if let Some(name) = opt_str(p, "calendar") {
        let name = name.trim();
        return s
            .mailbox
            .folders
            .values()
            .find(|f| f.kind == FolderKind::Calendar && f.name.eq_ignore_ascii_case(name))
            .map(|f| f.id)
            .ok_or_else(|| CmdError::Params(format!("no calendar named \"{name}\"")));
    }
    pim::new_item_folder(s, p, Kind::Appointment)
}

fn run_color(s: &mut Session, p: &Value) -> CmdResult {
    let color = opt_str(p, "color")
        .and_then(|c| CALENDAR_COLORS.iter().find(|x| x.eq_ignore_ascii_case(c.trim())))
        .ok_or_else(|| CmdError::Params(format!("`color` must be one of {}", CALENDAR_COLORS.join(", "))))?;
    let folder = target_calendar(s, p)?;
    s.view.module = Module::Calendar;
    Ok(json!({ "calendar": folder.0, "color": color }))
}

fn run_change_time_zone(s: &mut Session, p: &Value) -> CmdResult {
    match p.get("offsetMin") {
        None | Some(Value::Null) => {
            let params = json!({ "offsetMin": s.tz_offset_min });
            pim::dialog(s, "timeZone", params)
        }
        Some(v) => {
            let off = v
                .as_i64()
                .filter(|o| (-14 * 60..=14 * 60).contains(o))
                .and_then(|o| i32::try_from(o).ok())
                .ok_or_else(|| CmdError::Params("`offsetMin` must be between -840 and 840".into()))?;
            s.tz_offset_min = off;
            Ok(json!({ "offsetMin": off }))
        }
    }
}

fn run_add_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let Some(source) = opt_str(p, "source") else {
        return pim::dialog(s, "addCalendar", json!({}));
    };
    if !matches!(source, "addressBook" | "roomList" | "internet" | "shared") {
        return Err(CmdError::Params("`source` must be addressBook|roomList|internet|shared".into()));
    }
    let root = pim::default_folder(s, Kind::Appointment)
        .ok_or_else(|| CmdError::Failed("there is no calendar folder".into()))?;
    let (name, remote) = if source == "internet" {
        let Some(url) = opt_str(p, "url").map(str::trim).filter(|u| !u.is_empty()) else {
            return pim::dialog(s, "addCalendar", json!({ "source": source }));
        };
        let lower = url.to_ascii_lowercase();
        let rest = ["https://", "http://", "webcal://", "webcals://"]
            .iter()
            .find_map(|scheme| lower.strip_prefix(scheme).map(|_| url.get(scheme.len()..).unwrap_or("")))
            .ok_or_else(|| CmdError::Params("`url` must be an http(s) or webcal address".into()))?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_owned();
        if host.is_empty() {
            return Err(CmdError::Params("`url` has no host".into()));
        }
        let name = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or(host, str::to_owned);
        (name, format!("internet:{url}"))
    } else {
        let address = opt_str(p, "address").map(str::trim).filter(|a| !a.is_empty());
        let name = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty());
        let Some(name) = name.or(address) else {
            return pim::dialog(s, "addCalendar", json!({ "source": source }));
        };
        (name.to_owned(), format!("{source}:{}", address.unwrap_or(name)))
    };
    let id = s.mailbox.create_folder(root, &name)?;
    if let Some(f) = s.mailbox.folders.get_mut(&id) {
        f.remote_name = Some(remote);
    }
    Ok(json!({ "folder": id.0, "name": name }))
}

fn run_share(s: &mut Session, p: &Value) -> CmdResult {
    let folder = target_calendar(s, p)?;
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let tz = s.tz_offset_min;
    let mut appts: Vec<&Appointment> =
        s.mailbox.appointments.values().filter(|a| a.folder == folder).collect();
    appts.sort_by_key(|a| (a.start.unix, a.id));
    let ics = ics_calendar(Some("PUBLISH"), &appts, tz, s.now());
    let events = appts.len();
    let calendars: Vec<Value> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Calendar)
        .map(|f| json!({ "id": f.id.0, "name": f.name }))
        .collect();
    let compose = ComposeState {
        subject: name.clone(),
        attachments: vec![ComposeAttachment {
            filename: format!("{}.ics", file_safe(&name)),
            mime_type: "text/calendar".into(),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    let id = s.open_compose(compose);
    Ok(json!({ "compose": id, "calendar": folder.0, "events": events, "calendars": calendars }))
}

fn file_safe(name: &str) -> String {
    let n: String =
        name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    if n.trim().is_empty() { "calendar".into() } else { n.trim().to_owned() }
}

// ---------------------------------------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------------------------------------

/// Default appointment length (minutes).
pub const DEFAULT_DURATION_MIN: i64 = 30;
/// Default reminder (minutes before start).
pub const DEFAULT_REMINDER_MIN: u32 = 15;
/// Longest appointment accepted (seconds; ten years).
const MAX_SPAN_SECS: i64 = 10 * 366 * pim::DAY_SECS;

const APPT_FIELDS: [&str; 15] = [
    "subject",
    "location",
    "body",
    "start",
    "end",
    "durationMin",
    "allDay",
    "showAs",
    "reminderMin",
    "private",
    "categories",
    "attendees",
    "optional",
    "resources",
    "recurrence",
];

/// Start of the next half-hour slot after `now` (local time).
pub fn next_half_hour(now: DateTime, tz: i32) -> DateTime {
    let shift = i64::from(tz).saturating_mul(60);
    let local = now.unix.saturating_add(shift);
    let slot = local.div_euclid(1800).saturating_add(1).saturating_mul(1800);
    DateTime::from_unix(slot.saturating_sub(shift), tz)
}

/// UID for a new appointment.
pub(crate) fn new_uid(id: ItemId, stamp: i64) -> String {
    format!("mailcraft-{}-{}@localhost", id.0, stamp)
}

/// A new appointment with the defaults (next half hour, 30 minutes, Busy, reminder 15).
pub fn default_appointment(s: &Session, folder: FolderId) -> Appointment {
    let start = next_half_hour(s.now(), s.tz_offset_min);
    Appointment {
        folder,
        start,
        end: pim::add_secs(start, DEFAULT_DURATION_MIN.saturating_mul(60)),
        show_as: BusyStatus::Busy,
        reminder_min: Some(DEFAULT_REMINDER_MIN),
        ..Default::default()
    }
}

fn parse_show_as(v: &str) -> Option<BusyStatus> {
    match v {
        "free" => Some(BusyStatus::Free),
        "workingElsewhere" => Some(BusyStatus::WorkingElsewhere),
        "tentative" => Some(BusyStatus::Tentative),
        "busy" => Some(BusyStatus::Busy),
        "outOfOffice" => Some(BusyStatus::OutOfOffice),
        _ => None,
    }
}

fn show_as_name(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "free",
        BusyStatus::WorkingElsewhere => "workingElsewhere",
        BusyStatus::Tentative => "tentative",
        BusyStatus::Busy => "busy",
        BusyStatus::OutOfOffice => "outOfOffice",
    }
}

fn attendees_from(list: &[String], role: AttendeeRole) -> Vec<Attendee> {
    pim::parse_addresses(list)
        .into_iter()
        .map(|mailbox| Attendee { mailbox, role, response: ResponseStatus::None })
        .collect()
}

/// The organizer address for new meetings: the current account.
fn organizer(s: &Session) -> Option<Address> {
    let account = s.current_account().and_then(|a| s.mailbox.account(a))?;
    if account.address.trim().is_empty() {
        return None;
    }
    let name = if account.sender_name.trim().is_empty() { None } else { Some(account.sender_name.trim()) };
    Some(Address::new(name, account.address.trim()))
}

/// Applies appointment field params to `a` (shared by new and update).
pub fn apply_appointment_fields(s: &Session, a: &mut Appointment, p: &Value) -> Result<(), CmdError> {
    let tz = s.tz_offset_min;
    if let Some(v) = pim::text(p, "subject")? {
        a.subject = v;
    }
    if let Some(v) = pim::text(p, "location")? {
        a.location = v;
    }
    if let Some(v) = pim::text(p, "body")? {
        a.body = v;
    }
    let old_len = a.end.unix.saturating_sub(a.start.unix).max(0);
    let all_day = pim::flag(p, "allDay")?;
    if let Some(d) = all_day {
        a.all_day = d;
    }
    let start = pim::date(p, "start", tz)?;
    match start {
        Some(Some(t)) => a.start = t,
        Some(None) => return Err(CmdError::Params("`start` cannot be cleared".into())),
        None => {}
    }
    let end = pim::date(p, "end", tz)?;
    let duration = match p.get("durationMin") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_i64()
                .filter(|m| (0..=MAX_SPAN_SECS / 60).contains(m))
                .ok_or_else(|| CmdError::Params("`durationMin` must be a non-negative number".into()))?,
        ),
    };
    match (end, duration) {
        (Some(Some(t)), _) => a.end = t,
        (Some(None), _) => return Err(CmdError::Params("`end` cannot be cleared".into())),
        (None, Some(m)) => a.end = pim::add_secs(a.start, m.saturating_mul(60)),
        (None, None) => a.end = pim::add_secs(a.start, old_len),
    }
    if a.all_day {
        a.start = pim::local_midnight(a.start, tz);
        let end_mid = pim::local_midnight(a.end, tz);
        let end_day = if end_mid.unix < a.end.unix { pim::add_secs(end_mid, pim::DAY_SECS) } else { end_mid };
        a.end = if end_day.unix <= a.start.unix { pim::add_secs(a.start, pim::DAY_SECS) } else { end_day };
    }
    if a.end.unix < a.start.unix {
        return Err(CmdError::Params("the end time is before the start time".into()));
    }
    if a.end.unix.saturating_sub(a.start.unix) > MAX_SPAN_SECS {
        return Err(CmdError::Params("the appointment is too long".into()));
    }
    match p.get("showAs") {
        None | Some(Value::Null) => {
            if all_day == Some(true) {
                a.show_as = BusyStatus::Free;
            }
        }
        Some(v) => {
            a.show_as = v.as_str().and_then(parse_show_as).ok_or_else(|| {
                CmdError::Params("`showAs` must be free|workingElsewhere|tentative|busy|outOfOffice".into())
            })?;
        }
    }
    match p.get("reminderMin") {
        None => {}
        Some(Value::Null) | Some(Value::Bool(false)) => a.reminder_min = None,
        Some(v) => {
            a.reminder_min = Some(
                v.as_u64()
                    .filter(|m| *m <= 60 * 24 * 7 * 52)
                    .and_then(|m| u32::try_from(m).ok())
                    .ok_or_else(|| CmdError::Params("`reminderMin` must be minutes or null".into()))?,
            );
        }
    }
    if let Some(v) = pim::flag(p, "private")? {
        a.private = v;
    }
    if let Some(v) = pim::list(p, "categories")? {
        a.categories = v;
    }
    let mut touched = false;
    let mut attendees: Vec<Attendee> = a.attendees.clone();
    for (key, role) in [
        ("attendees", AttendeeRole::Required),
        ("optional", AttendeeRole::Optional),
        ("resources", AttendeeRole::Resource),
    ] {
        if let Some(list) = pim::list(p, key)? {
            touched = true;
            attendees.retain(|x| x.role != role);
            for att in attendees_from(&list, role) {
                if !attendees.iter().any(|x| x.mailbox.same_address(&att.mailbox)) {
                    attendees.push(att);
                }
            }
        }
    }
    if touched {
        a.attendees = attendees;
        if !a.attendees.is_empty() {
            a.is_meeting = true;
        }
    }
    match p.get("recurrence") {
        None => {}
        Some(Value::Null) => a.recurrence = None,
        Some(Value::String(r)) if r.trim().is_empty() => a.recurrence = None,
        Some(Value::String(r)) => {
            let r = r.trim().trim_start_matches("RRULE:");
            if r.len() > 1000 || !r.to_ascii_uppercase().contains("FREQ=") {
                return Err(CmdError::Params(
                    "`recurrence` must be an RRULE value such as FREQ=WEEKLY".into(),
                ));
            }
            a.recurrence = Some(Recurrence { rrule: r.to_owned(), exdates: Vec::new() });
        }
        Some(_) => return Err(CmdError::Params("`recurrence` must be a string".into())),
    }
    Ok(())
}

/// Summary of an appointment for results.
pub fn appointment_json(a: &Appointment, tz: i32) -> Value {
    json!({
        "id": a.id.0,
        "folder": a.folder.0,
        "subject": a.subject,
        "location": a.location,
        "start": pim::datetime_string(a.start, tz),
        "end": pim::datetime_string(a.end, tz),
        "allDay": a.all_day,
        "showAs": show_as_name(a.show_as),
        "reminderMin": a.reminder_min,
        "meeting": a.is_meeting,
        "attendees": a.attendees.iter().map(|x| x.mailbox.to_display_string()).collect::<Vec<_>>(),
        "categories": a.categories,
    })
}

/// Creates an appointment in `folder` from field params (on top of the defaults). Returns its id.
pub fn create_appointment(
    s: &mut Session,
    folder: FolderId,
    p: &Value,
    meeting: bool,
) -> Result<ItemId, CmdError> {
    let mut a = default_appointment(s, folder);
    apply_appointment_fields(s, &mut a, p)?;
    if meeting {
        a.is_meeting = true;
    }
    if a.is_meeting && a.organizer.is_none() {
        a.organizer = organizer(s);
    }
    let id = ItemId(s.mailbox.alloc_id());
    a.id = id;
    a.uid = new_uid(id, s.now().unix);
    s.mailbox.appointments.insert(id, a);
    Ok(id)
}

fn run_new_appointment(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Appointment)?;
    if !pim::has_any(p, &APPT_FIELDS) {
        let d = default_appointment(s, folder);
        let tz = s.tz_offset_min;
        let params = json!({
            "folder": folder.0,
            "start": pim::datetime_string(d.start, tz),
            "end": pim::datetime_string(d.end, tz),
            "reminderMin": d.reminder_min,
            "focus": "subject",
        });
        return pim::dialog(s, "appointment", params);
    }
    let id = create_appointment(s, folder, p, false)?;
    let tz = s.tz_offset_min;
    Ok(s.mailbox.appointments.get(&id).map_or_else(|| json!({ "id": id.0 }), |a| appointment_json(a, tz)))
}

/// Creates a meeting and opens a compose with the invitation. Returns `{"id", "compose", ...}`.
pub fn create_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Appointment)?;
    let id = create_appointment(s, folder, p, true)?;
    let compose =
        meeting_compose(s, id).ok_or_else(|| CmdError::Failed("the meeting could not be created".into()))?;
    let tz = s.tz_offset_min;
    let mut out =
        s.mailbox.appointments.get(&id).map_or_else(|| json!({ "id": id.0 }), |a| appointment_json(a, tz));
    if let Some(o) = out.as_object_mut() {
        o.insert("compose".into(), json!(compose));
    }
    Ok(out)
}

fn run_new_meeting(s: &mut Session, p: &Value) -> CmdResult {
    create_meeting(s, p)
}

/// Opens a compose carrying the meeting invitation for appointment `id`.
pub fn meeting_compose(s: &mut Session, id: ItemId) -> Option<u64> {
    let a = s.mailbox.appointments.get(&id)?.clone();
    let tz = s.tz_offset_min;
    let join = |roles: &[AttendeeRole]| {
        a.attendees
            .iter()
            .filter(|x| roles.contains(&x.role))
            .map(|x| x.mailbox.to_display_string())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let to = join(&[AttendeeRole::Required, AttendeeRole::Resource]);
    let cc = join(&[AttendeeRole::Optional]);
    let mut text = format!("When: {}\n", when_text(&a, tz));
    if !a.location.trim().is_empty() {
        text.push_str(&format!("Where: {}\n", a.location.trim()));
    }
    if !a.body.trim().is_empty() {
        text.push('\n');
        text.push_str(&a.body);
    }
    let ics = ics_calendar(Some("REQUEST"), &[&a], tz, s.now());
    let compose = ComposeState {
        to,
        cc,
        from: a.organizer.as_ref().map(Address::to_display_string).unwrap_or_default(),
        subject: a.subject.clone(),
        body: Doc::from_plain_text(&text),
        attachments: vec![ComposeAttachment {
            filename: "invite.ics".into(),
            mime_type: "text/calendar; method=REQUEST".into(),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    Some(s.open_compose(compose))
}

/// "Friday, October 9, 2026 10:00 AM-10:30 AM" (all-day and multi-day forms too).
fn when_text(a: &Appointment, tz: i32) -> String {
    let same_day = a.start.day_number(tz) == a.end.day_number(tz);
    if a.all_day {
        let last = pim::add_secs(a.end, -1);
        if a.end.unix.saturating_sub(a.start.unix) <= pim::DAY_SECS {
            ListTime::long_date(a.start, tz)
        } else {
            format!("{} - {}", ListTime::long_date(a.start, tz), ListTime::long_date(last, tz))
        }
    } else if same_day {
        format!(
            "{} {}-{}",
            ListTime::long_date(a.start, tz),
            ListTime::clock(a.start, tz),
            ListTime::clock(a.end, tz)
        )
    } else {
        format!(
            "{} {} - {} {}",
            ListTime::long_date(a.start, tz),
            ListTime::clock(a.start, tz),
            ListTime::long_date(a.end, tz),
            ListTime::clock(a.end, tz)
        )
    }
}

fn run_update(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Appointment)?;
    let mut updated = Vec::new();
    for id in &ids {
        let Some(mut a) = s.mailbox.appointments.get(id).cloned() else { continue };
        apply_appointment_fields(s, &mut a, p)?;
        if a.is_meeting && a.organizer.is_none() {
            a.organizer = organizer(s);
        }
        updated.push(a);
    }
    let tz = s.tz_offset_min;
    let out: Vec<Value> = updated.iter().map(|a| appointment_json(a, tz)).collect();
    for a in updated {
        s.mailbox.appointments.insert(a.id, a);
    }
    Ok(json!({ "updated": out }))
}

fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_delete(s, p, Kind::Appointment)
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_copy(s, p, Kind::Appointment)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_move(s, p, Kind::Appointment)
}
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_categorize(s, p, Kind::Appointment)
}

// ---------------------------------------------------------------------------------------------------------
// iCalendar (RFC 5545) writer
// ---------------------------------------------------------------------------------------------------------

/// Escapes a TEXT value (RFC 5545 §3.3.11).
pub fn ics_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Parameter value: quoted when it holds `:`, `;` or `,` (DQUOTE itself is dropped).
fn ics_param(s: &str) -> String {
    let v: String = s.chars().filter(|c| *c != '"' && !c.is_control()).collect();
    if v.contains([':', ';', ',']) { format!("\"{v}\"") } else { v }
}

/// Folds a content line at 75 octets (continuations start with a space), CRLF-terminated.
fn ics_fold(line: &str, out: &mut String) {
    let mut width = 0usize;
    for c in line.chars() {
        let w = c.len_utf8();
        if width.saturating_add(w) > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width = width.saturating_add(w);
    }
    out.push_str("\r\n");
}

fn ics_utc(t: DateTime) -> String {
    let c = t.civil(0);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

fn ics_date(t: DateTime, tz: i32) -> String {
    let c = t.civil(tz);
    format!("{:04}{:02}{:02}", c.year, c.month, c.day)
}

fn vevent_lines(a: &Appointment, tz: i32, stamp: DateTime, lines: &mut Vec<String>) {
    lines.push("BEGIN:VEVENT".into());
    lines.push(format!("UID:{}", ics_escape(&a.uid)));
    lines.push(format!("DTSTAMP:{}", ics_utc(stamp)));
    if a.all_day {
        lines.push(format!("DTSTART;VALUE=DATE:{}", ics_date(a.start, tz)));
        lines.push(format!("DTEND;VALUE=DATE:{}", ics_date(a.end, tz)));
    } else {
        lines.push(format!("DTSTART:{}", ics_utc(a.start)));
        lines.push(format!("DTEND:{}", ics_utc(a.end)));
    }
    lines.push(format!("SUMMARY:{}", ics_escape(&a.subject)));
    if !a.location.is_empty() {
        lines.push(format!("LOCATION:{}", ics_escape(&a.location)));
    }
    if !a.body.is_empty() {
        lines.push(format!("DESCRIPTION:{}", ics_escape(&a.body)));
    }
    if let Some(r) = &a.recurrence {
        lines.push(format!("RRULE:{}", r.rrule.chars().filter(|c| !c.is_control()).collect::<String>()));
    }
    if let Some(o) = &a.organizer {
        let cn = o.name.as_deref().map(|n| format!(";CN={}", ics_param(n))).unwrap_or_default();
        lines.push(format!("ORGANIZER{cn}:mailto:{}", ics_param(&o.addr)));
    }
    for at in &a.attendees {
        let role = match at.role {
            AttendeeRole::Required => "ROLE=REQ-PARTICIPANT",
            AttendeeRole::Optional => "ROLE=OPT-PARTICIPANT",
            AttendeeRole::Resource => "CUTYPE=RESOURCE;ROLE=NON-PARTICIPANT",
        };
        let partstat = match at.response {
            ResponseStatus::None => "NEEDS-ACTION",
            ResponseStatus::Accepted => "ACCEPTED",
            ResponseStatus::Tentative => "TENTATIVE",
            ResponseStatus::Declined => "DECLINED",
        };
        let cn = at.mailbox.name.as_deref().map(|n| format!(";CN={}", ics_param(n))).unwrap_or_default();
        lines.push(format!(
            "ATTENDEE;{role};PARTSTAT={partstat};RSVP=TRUE{cn}:mailto:{}",
            ics_param(&at.mailbox.addr)
        ));
    }
    if !a.categories.is_empty() {
        let cats: Vec<String> = a.categories.iter().map(|c| ics_escape(c)).collect();
        lines.push(format!("CATEGORIES:{}", cats.join(",")));
    }
    if a.private {
        lines.push("CLASS:PRIVATE".into());
    }
    lines.push(format!("TRANSP:{}", if a.show_as == BusyStatus::Free { "TRANSPARENT" } else { "OPAQUE" }));
    if let Some(m) = a.reminder_min {
        lines.push("BEGIN:VALARM".into());
        lines.push("ACTION:DISPLAY".into());
        lines.push(format!("TRIGGER:-PT{m}M"));
        lines.push(format!("DESCRIPTION:{}", ics_escape(&a.subject)));
        lines.push("END:VALARM".into());
    }
    lines.push("END:VEVENT".into());
}

/// A VCALENDAR object holding `appts` (`method` e.g. "REQUEST", "PUBLISH").
pub fn ics_calendar(method: Option<&str>, appts: &[&Appointment], tz: i32, stamp: DateTime) -> String {
    let mut lines =
        vec!["BEGIN:VCALENDAR".to_owned(), "VERSION:2.0".into(), "PRODID:-//MailCraft//MailCraft//EN".into()];
    if let Some(m) = method {
        lines.push(format!("METHOD:{m}"));
    }
    for a in appts {
        vevent_lines(a, tz, stamp, &mut lines);
    }
    lines.push("END:VCALENDAR".into());
    let mut out = String::new();
    for l in &lines {
        ics_fold(l, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------------------

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, location, shortcut, params, mutates, run| CommandSpec {
        id,
        label,
        location,
        shortcut,
        params,
        mutates,
        enabled: always,
        run,
    };
    vec![
        spec(
            "calendar.addCalendar",
            "Add Calendar",
            "Home › Manage Calendars; Folder › Share",
            "",
            r#"{"source": "addressBook|roomList|internet|shared", "name"?: string, "address"?: string, "url"?: string}"#,
            true,
            run_add_calendar,
        ),
        spec(
            "calendar.arrange",
            "Calendar Arrangement",
            "Home › Arrange; View › Arrangement",
            "",
            r#"{"mode": "day|workWeek|week|month|schedule", "view"?: CalendarViewState}"#,
            false,
            run_arrange,
        ),
        spec(
            "calendar.changeTimeZone",
            "Change Time Zone",
            "View › Time Scale",
            "",
            r#"{"offsetMin"?: number}"#,
            false,
            run_change_time_zone,
        ),
        spec(
            "calendar.color",
            "Calendar Color",
            "View › Color",
            "",
            r#"{"color": "Automatic|Blue|Green|Orange|Navy Blue|Yellow|Teal|Magenta|Brown|Dark Red", "calendar"?: string, "folder"?: number}"#,
            false,
            run_color,
        ),
        spec(
            "calendar.dailyTaskList",
            "Daily Task List",
            "View › Layout",
            "",
            r#"{"mode": "normal|minimized|off", "view"?: CalendarViewState}"#,
            false,
            run_daily_task_list,
        ),
        spec(
            "calendar.goToDate",
            "Go to Date",
            "Home › Go To",
            "Mod+G",
            r#"{"date"?: "YYYY-MM-DD", "mode"?: "day|workWeek|week|month", "view"?: CalendarViewState}"#,
            false,
            run_go_to_date,
        ),
        spec(
            "calendar.next7Days",
            "Next 7 Days",
            "Home › Go To",
            "",
            r#"{"view"?: CalendarViewState}"#,
            false,
            run_next7,
        ),
        CommandSpec {
            id: "calendar.overlay",
            label: "Overlay",
            location: "View › Arrangement",
            shortcut: "",
            params: r#"{"on"?: bool, "view"?: CalendarViewState}"#,
            mutates: false,
            enabled: overlay_enabled,
            run: run_overlay,
        },
        spec(
            "calendar.share",
            "Share Calendar",
            "Home › Manage Calendars; Folder › Share",
            "",
            r#"{"calendar"?: "<calendar name>", "folder"?: number}"#,
            false,
            run_share,
        ),
        spec(
            "calendar.timeScale",
            "Time Scale",
            "View › Arrangement",
            "",
            r#"{"minutes": 60|30|15|10|6|5, "view"?: CalendarViewState}"#,
            false,
            run_time_scale,
        ),
        spec(
            "calendar.today",
            "Today",
            "Home › Go To",
            "Mod+T",
            r#"{"view"?: CalendarViewState}"#,
            false,
            run_today,
        ),
        spec(
            "calendar.workingHours",
            "Working Hours",
            "View › Arrangement",
            "",
            r#"{"on"?: bool, "view"?: CalendarViewState}"#,
            false,
            run_working_hours,
        ),
        spec(
            "calendar.newAppointment",
            "New Appointment",
            "Calendar › Home › New",
            "",
            r#"{"subject"?, "location"?, "body"?, "start"?: "YYYY-MM-DDTHH:MM", "end"?, "durationMin"?, "allDay"?: bool, "showAs"?: "free|workingElsewhere|tentative|busy|outOfOffice", "reminderMin"?: number|null, "private"?: bool, "categories"?: [string], "recurrence"?: "RRULE", "folder"?: number} (no fields = open a blank appointment window)"#,
            true,
            run_new_appointment,
        ),
        spec(
            "calendar.newMeeting",
            "New Meeting",
            "Calendar › Home › New",
            "",
            r#"{"attendees"?: [string], "optional"?: [string], "resources"?: [string], ...appointment fields}"#,
            true,
            run_new_meeting,
        ),
        spec(
            "calendar.update",
            "Edit Appointment",
            "Calendar › Item",
            "",
            r#"{"id": number | "ids": [number], ...appointment fields}"#,
            true,
            run_update,
        ),
        spec(
            "calendar.delete",
            "Delete",
            "Calendar › Item",
            "",
            r#"{"id"?: number, "ids"?: [number]}"#,
            true,
            run_delete,
        ),
        spec(
            "calendar.copy",
            "Copy to Folder",
            "Calendar › Item",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            run_copy,
        ),
        spec(
            "calendar.move",
            "Move to Folder",
            "Calendar › Item",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            run_move,
        ),
        spec(
            "calendar.categorize",
            "Categorize",
            "Calendar › Item",
            "",
            r#"{"name": string, "on"?: bool, "clear"?: bool, "id"?: number, "ids"?: [number]}"#,
            true,
            run_categorize,
        ),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mailcraft_model::{FolderRole, Mailbox};

    /// Session over a fresh local mailbox, clock fixed at 2026-10-09 10:07 UTC (a Friday).
    pub(crate) fn session() -> Session {
        let mut s = Session::new(Mailbox::new_local("Test"));
        s.fixed_now = mailcraft_model::dates::local_datetime(2026, 10, 9, 10, 7, 0, 0);
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.address = "me@example.com".into();
            a.sender_name = "Me Myself".into();
        }
        s
    }

    pub(crate) fn role(s: &Session, r: FolderRole) -> FolderId {
        let a = s.mailbox.default_account().unwrap().id;
        s.mailbox.role_folder(a, r).unwrap()
    }

    fn cal(s: &Session) -> FolderId {
        role(s, FolderRole::Calendar)
    }

    #[test]
    fn registers_every_id() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        for id in [
            "calendar.addCalendar",
            "calendar.arrange",
            "calendar.changeTimeZone",
            "calendar.color",
            "calendar.dailyTaskList",
            "calendar.goToDate",
            "calendar.next7Days",
            "calendar.overlay",
            "calendar.share",
            "calendar.timeScale",
            "calendar.today",
            "calendar.workingHours",
        ] {
            assert!(ids.contains(&id), "{id}");
        }
        let s = session();
        assert!(s.registry.get("calendar.today").is_some());
    }

    #[test]
    fn next_half_hour_rounds_up() {
        let now = mailcraft_model::dates::local_datetime(2026, 10, 9, 10, 7, 0, 0).unwrap();
        assert_eq!(pim::datetime_string(next_half_hour(now, 0), 0), "2026-10-09T10:30");
        let exact = mailcraft_model::dates::local_datetime(2026, 10, 9, 10, 30, 0, 0).unwrap();
        assert_eq!(pim::datetime_string(next_half_hour(exact, 0), 0), "2026-10-09T11:00");
        // Local offset -04:00: 10:07 UTC is 06:07 local → 06:30 local.
        assert_eq!(pim::datetime_string(next_half_hour(now, -240), -240), "2026-10-09T06:30");
    }

    #[test]
    fn new_appointment_without_fields_opens_window_with_defaults() {
        let mut s = session();
        let r = s.run("calendar.newAppointment", &json!({})).unwrap();
        assert_eq!(r["dialog"], "appointment");
        assert!(s.mailbox.appointments.is_empty());
        match s.notices.last() {
            Some(crate::Notice::OpenDialog(name, p)) => {
                assert_eq!(name, "appointment");
                assert_eq!(p["start"], "2026-10-09T10:30");
                assert_eq!(p["end"], "2026-10-09T11:00");
                assert_eq!(p["reminderMin"], 15);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn new_appointment_with_fields_creates_with_defaults() {
        let mut s = session();
        let r =
            s.run("calendar.newAppointment", &json!({"subject": "Dentist", "location": "Main St"})).unwrap();
        let id = ItemId(r["id"].as_u64().unwrap());
        let a = &s.mailbox.appointments[&id];
        assert_eq!(a.folder, cal(&s));
        assert_eq!(a.subject, "Dentist");
        assert_eq!(a.reminder_min, Some(15));
        assert_eq!(a.end.unix - a.start.unix, 1800);
        assert_eq!(r["start"], "2026-10-09T10:30");
        assert!(!a.is_meeting);
        assert!(!a.uid.is_empty());
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn new_appointment_explicit_times_and_validation() {
        let mut s = session();
        let r = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "Review", "start": "2026-10-12T14:00", "durationMin": 90, "reminderMin": null, "showAs": "tentative"}),
            )
            .unwrap();
        assert_eq!(r["end"], "2026-10-12T15:30");
        assert_eq!(r["reminderMin"], Value::Null);
        assert_eq!(r["showAs"], "tentative");
        let bad = s
            .run("calendar.newAppointment", &json!({"start": "2026-10-12T14:00", "end": "2026-10-12T13:00"}));
        assert!(matches!(bad, Err(CmdError::Params(_))));
        let bad = s.run("calendar.newAppointment", &json!({"start": "2026-13-40"}));
        assert!(matches!(bad, Err(CmdError::Params(_))));
        let bad = s.run("calendar.newAppointment", &json!({"subject": "x", "showAs": "busyish"}));
        assert!(matches!(bad, Err(CmdError::Params(_))));
    }

    #[test]
    fn all_day_event_spans_whole_days() {
        let mut s = session();
        let r = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "Holiday", "allDay": true, "start": "2026-10-12"}),
            )
            .unwrap();
        assert_eq!(r["start"], "2026-10-12T00:00");
        assert_eq!(r["end"], "2026-10-13T00:00");
        assert_eq!(r["showAs"], "free");
    }

    #[test]
    fn new_meeting_creates_appointment_and_invitation() {
        let mut s = session();
        let r = s
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Plan", "attendees": ["Ann Lee <ann@example.com>", "bob@example.org"], "optional": "cy@example.net", "location": "Room 4"}),
            )
            .unwrap();
        let id = ItemId(r["id"].as_u64().unwrap());
        let a = &s.mailbox.appointments[&id];
        assert!(a.is_meeting);
        assert_eq!(a.attendees.len(), 3);
        assert_eq!(a.organizer.as_ref().unwrap().addr, "me@example.com");
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "Plan");
        assert_eq!(c.to, "Ann Lee <ann@example.com>; bob@example.org");
        assert_eq!(c.cc, "cy@example.net");
        let text = c.body.to_plain_text();
        assert!(text.contains("Friday, October 9, 2026"), "{text}");
        assert!(text.contains("Room 4"));
        let ics = String::from_utf8(c.attachments[0].data.clone()).unwrap();
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.contains("METHOD:REQUEST\r\n"));
        assert!(ics.contains("DTSTART:20261009T103000Z\r\n"));
        let unfolded = ics.replace("\r\n ", "");
        assert!(unfolded.contains("mailto:ann@example.com"), "{unfolded}");
        assert!(ics.contains("ROLE=OPT-PARTICIPANT"));
        assert!(s.notices.iter().any(|n| matches!(n, crate::Notice::Compose(_))));
    }

    #[test]
    fn update_keeps_duration_when_start_moves() {
        let mut s = session();
        let r = s.run("calendar.newAppointment", &json!({"subject": "A", "durationMin": 60})).unwrap();
        let id = r["id"].as_u64().unwrap();
        let r = s
            .run("calendar.update", &json!({"id": id, "start": "2026-10-20T09:00", "subject": "B"}))
            .unwrap();
        assert_eq!(r["updated"][0]["end"], "2026-10-20T10:00");
        assert_eq!(s.mailbox.appointments[&ItemId(id)].subject, "B");
        assert!(matches!(s.run("calendar.update", &json!({"id": 999_999})), Err(CmdError::Params(_))));
    }

    #[test]
    fn delete_moves_to_deleted_items_then_removes() {
        let mut s = session();
        let id = s.run("calendar.newAppointment", &json!({"subject": "A"})).unwrap()["id"].as_u64().unwrap();
        s.run("calendar.delete", &json!({"id": id})).unwrap();
        assert_eq!(s.mailbox.appointments[&ItemId(id)].folder, role(&s, FolderRole::DeletedItems));
        s.run("calendar.delete", &json!({"ids": [id]})).unwrap();
        assert!(!s.mailbox.appointments.contains_key(&ItemId(id)));
    }

    #[test]
    fn copy_move_and_categorize() {
        let mut s = session();
        let root = cal(&s);
        let other = s.mailbox.create_folder(root, "Team").unwrap();
        let id = s.run("calendar.newAppointment", &json!({"subject": "A"})).unwrap()["id"].as_u64().unwrap();
        let r = s.run("calendar.copy", &json!({"id": id, "folder": other.0})).unwrap();
        let copy = ItemId(r["copies"][0].as_u64().unwrap());
        assert_eq!(s.mailbox.appointments[&copy].folder, other);
        assert_ne!(s.mailbox.appointments[&copy].uid, s.mailbox.appointments[&ItemId(id)].uid);
        s.run("calendar.move", &json!({"id": id, "folder": other.0})).unwrap();
        assert_eq!(s.mailbox.appointments[&ItemId(id)].folder, other);
        let inbox = role(&s, FolderRole::Inbox);
        assert!(matches!(
            s.run("calendar.move", &json!({"id": id, "folder": inbox.0})),
            Err(CmdError::Params(_))
        ));
        s.run("calendar.categorize", &json!({"id": id, "name": "red category"})).unwrap();
        assert_eq!(s.mailbox.appointments[&ItemId(id)].categories, vec!["Red category".to_string()]);
        s.run("calendar.categorize", &json!({"id": id, "name": "Red category"})).unwrap();
        assert!(s.mailbox.appointments[&ItemId(id)].categories.is_empty());
        assert!(matches!(
            s.run("calendar.categorize", &json!({"id": id, "name": "Nope"})),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn arrange_sets_mode_and_module() {
        let mut s = session();
        let r = s.run("calendar.arrange", &json!({"mode": "workWeek"})).unwrap();
        assert_eq!(r["view"]["mode"], "workWeek");
        assert_eq!(r["view"]["date"], "2026-10-09");
        assert_eq!(s.view.module, Module::Calendar);
        assert!(matches!(s.run("calendar.arrange", &json!({"mode": "year"})), Err(CmdError::Params(_))));
    }

    #[test]
    fn today_and_next_seven_days() {
        let mut s = session();
        let r = s.run("calendar.today", &json!({"view": {"mode": "week", "date": "2026-01-01"}})).unwrap();
        assert_eq!(r["view"]["date"], "2026-10-09");
        assert_eq!(r["view"]["mode"], "week");
        let r = s.run("calendar.next7Days", &json!({})).unwrap();
        assert_eq!(r["view"]["mode"], "day");
        assert_eq!(r["view"]["days"], 7);
    }

    #[test]
    fn go_to_date_dialog_or_jump() {
        let mut s = session();
        let r = s.run("calendar.goToDate", &json!({})).unwrap();
        assert_eq!(r["dialog"], "goToDate");
        let r = s.run("calendar.goToDate", &json!({"date": "2027-02-14", "mode": "day"})).unwrap();
        assert_eq!(r["view"]["date"], "2027-02-14");
        assert_eq!(r["view"]["mode"], "day");
        assert!(matches!(
            s.run("calendar.goToDate", &json!({"date": "2027-02-30"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn time_scale_working_hours_and_task_list_disabled_in_month() {
        let mut s = session();
        let month = json!({"mode": "month"});
        assert!(matches!(
            s.run("calendar.timeScale", &json!({"minutes": 15, "view": month})),
            Err(CmdError::Disabled(_))
        ));
        assert!(matches!(
            s.run("calendar.workingHours", &json!({"view": month})),
            Err(CmdError::Disabled(_))
        ));
        assert!(matches!(
            s.run("calendar.dailyTaskList", &json!({"mode": "off", "view": month})),
            Err(CmdError::Disabled(_))
        ));
        let week = json!({"mode": "week"});
        let r = s.run("calendar.timeScale", &json!({"minutes": 15, "view": week})).unwrap();
        assert_eq!(r["view"]["timeScale"], 15);
        assert!(matches!(
            s.run("calendar.timeScale", &json!({"minutes": 20, "view": week})),
            Err(CmdError::Params(_))
        ));
        let r = s.run("calendar.workingHours", &json!({"view": week})).unwrap();
        assert_eq!(r["view"]["workingHours"], true);
        let r = s.run("calendar.workingHours", &json!({"view": r["view"].clone()})).unwrap();
        assert_eq!(r["view"]["workingHours"], false);
        let r = s.run("calendar.dailyTaskList", &json!({"mode": "minimized", "view": week})).unwrap();
        assert_eq!(r["view"]["dailyTaskList"], "minimized");
    }

    #[test]
    fn overlay_needs_two_calendars() {
        let mut s = session();
        assert!(matches!(s.run("calendar.overlay", &json!({})), Err(CmdError::Disabled(_))));
        let root = cal(&s);
        s.mailbox.create_folder(root, "Team").unwrap();
        let r = s.run("calendar.overlay", &json!({})).unwrap();
        assert_eq!(r["view"]["overlay"], true);
    }

    #[test]
    fn color_validates_and_targets_calendar() {
        let mut s = session();
        let r = s.run("calendar.color", &json!({"color": "navy blue"})).unwrap();
        assert_eq!(r["color"], "Navy Blue");
        assert_eq!(r["calendar"], cal(&s).0);
        assert!(matches!(s.run("calendar.color", &json!({"color": "Purple"})), Err(CmdError::Params(_))));
        assert!(matches!(
            s.run("calendar.color", &json!({"color": "Blue", "calendar": "Nope"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn change_time_zone() {
        let mut s = session();
        assert_eq!(s.run("calendar.changeTimeZone", &json!({})).unwrap()["dialog"], "timeZone");
        s.run("calendar.changeTimeZone", &json!({"offsetMin": -300})).unwrap();
        assert_eq!(s.tz_offset_min, -300);
        assert!(matches!(
            s.run("calendar.changeTimeZone", &json!({"offsetMin": 5000})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn add_calendar_from_internet_and_shared() {
        let mut s = session();
        assert_eq!(s.run("calendar.addCalendar", &json!({})).unwrap()["dialog"], "addCalendar");
        assert_eq!(
            s.run("calendar.addCalendar", &json!({"source": "internet"})).unwrap()["dialog"],
            "addCalendar"
        );
        let r = s
            .run(
                "calendar.addCalendar",
                &json!({"source": "internet", "url": "webcal://cal.example.com/holidays.ics"}),
            )
            .unwrap();
        let f = FolderId(r["folder"].as_u64().unwrap());
        let folder = s.mailbox.folder(f).unwrap();
        assert_eq!(folder.name, "cal.example.com");
        assert_eq!(folder.kind, FolderKind::Calendar);
        assert_eq!(folder.parent, Some(cal(&s)));
        let r = s.run(
            "calendar.addCalendar",
            &json!({"source": "shared", "address": "ann@example.com", "name": "Ann"}),
        );
        assert!(r.is_ok());
        assert!(matches!(s.run("calendar.addCalendar", &json!({"source": "fax"})), Err(CmdError::Params(_))));
        assert!(matches!(
            s.run("calendar.addCalendar", &json!({"source": "internet", "url": "ftp://x"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn share_sends_calendar_as_icalendar() {
        let mut s = session();
        s.run("calendar.newAppointment", &json!({"subject": "Standup, daily", "start": "2026-10-12T09:00"}))
            .unwrap();
        let r = s.run("calendar.share", &json!({"calendar": "calendar"})).unwrap();
        assert_eq!(r["events"], 1);
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.attachments[0].filename, "Calendar.ics");
        let ics = String::from_utf8(c.attachments[0].data.clone()).unwrap();
        assert!(ics.contains("METHOD:PUBLISH"));
        assert!(ics.contains("SUMMARY:Standup\\, daily"));
    }

    #[test]
    fn ics_lines_fold_at_75_octets() {
        let mut out = String::new();
        let long = "DESCRIPTION:".to_owned() + &"é".repeat(100);
        ics_fold(&long, &mut out);
        for line in out.split("\r\n") {
            assert!(line.len() <= 75, "{}", line.len());
        }
        assert_eq!(out.replace("\r\n ", ""), long + "\r\n");
        assert_eq!(ics_escape("a;b,c\\d\ne"), "a\\;b\\,c\\\\d\\ne");
    }

    #[test]
    fn parse_local_forms() {
        assert!(pim::parse_local("2026-10-09", 0).is_some());
        assert!(pim::parse_local("2026-10-09T08:30", 0).is_some());
        assert!(pim::parse_local("2026-10-09 08:30:15", 0).is_some());
        assert!(pim::parse_local("2026-10-09T25:00", 0).is_none());
        assert!(pim::parse_local("garbage", 0).is_none());
        assert!(pim::parse_local("", 0).is_none());
    }
}
