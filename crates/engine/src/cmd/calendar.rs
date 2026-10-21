//! Calendar commands (`calendar.*`), plus the shared PIM helpers ([`pim`]) that the People, Tasks and Notes
//! modules use for item CRUD (new / update / delete / copy / move / categorize).
//!
//! **View state.** The session has no calendar fields (its interface is frozen), so the arrangement
//! commands are pure functions of a [`CalendarViewState`] that the UI holds (commit 4): the caller passes
//! the current state as `{"view": {...}}` (omitted = the current calendar's saved view, else the default
//! Month view on today) and the command returns the new state as `{"view": {...}}`. Toggles
//! (`calendar.overlay`, `calendar.workingHours`) flip the passed state unless `{"on": bool}` is given. Each
//! new state is also saved on the calendar folder ([`views`]). [`view_enabled`] answers the view-dependent
//! enable rules (Time Scale, Working Hours and Daily Task List are unavailable in Month view). The View
//! tab's generic commands (Change View, Arrange By, Reading Pane, To-Do Bar, View Settings, Reset View) are
//! answered for the Calendar module by [`module_view`].
//!
//! **Calendars.** Per-calendar properties (colour, internet subscription, saved view, time zones) live in
//! [`props`]; `calendar.color` sets the colour (Automatic = a palette colour per calendar,
//! [`calendar_color`]). Add Calendar subscribes to internet calendars ([`subscribe`]; read-only, refreshed
//! by `calendar.refreshCalendar`), copies an .ics file into a new calendar, or makes a blank one.
//!
//! **Items.** Appointments live in calendar folders (`Mailbox.appointments`). `calendar.newAppointment`
//! creates one when field params are given (defaults from File › Options › Calendar: the next half hour,
//! the default duration, the default reminder) and otherwise (or with `form: true`) asks the UI for the
//! appointment window, prefilled. `calendar.newMeeting` asks for the meeting window prefilled with the given
//! fields (People › Meeting and Tasks › Meeting too) and saves nothing until it is sent: with a `start` (the
//! window's Send) it creates the meeting and opens a compose with the invitation (plain-text summary plus
//! an RFC 5545 `text/calendar` attachment). `calendar.autoRespond` applies the automatic accept / decline
//! options to meeting messages. `calendar.moveItem` and
//! `calendar.resize` are the day/week views' drag and edge-drag. An appointment may name an IANA time zone
//! ([`tz`]); its times are then entered in that zone and its recurrences expand in that zone's wall-clock
//! time across daylight-saving changes.

pub mod props;
pub mod subscribe;
pub mod tz;
pub mod views;

pub use views::{module_view, saved_view, second_zone, third_zone, work_hours, work_week};

use serde::Serialize;
use serde_json::{Value, json};

use self::pim::Kind;
use crate::cmd::util::{opt_str, opt_u64};
use crate::compose::{ComposeAttachment, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Module, Session, always};
pub use mailcraft_formats::ics::itip::ItipMessage;
use mailcraft_formats::ics::itip::{self, Method};
use mailcraft_formats::ics::{ImportOptions, rrule};
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::{
    Address, Appointment, Attendee, BusyStatus, DateTime, Doc, FolderId, FolderKind, FolderRole, ItemId,
    ListTime, MailItem, Recurrence,
};

/// The reminders scanner (`crate::reminders`).
pub use crate::reminders;

/// Shared helpers for PIM items (appointments, contacts, tasks, notes).
pub mod pim {
    use serde_json::{Value, json};

    use crate::cmd::util::{opt_bool, opt_folder, opt_str, opt_u64};
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

    /// Categories of a PIM item, or of a message (the To-Do List shows flagged messages beside tasks).
    pub fn categories(mb: &Mailbox, id: ItemId) -> Option<&Vec<String>> {
        let Some(kind) = kind_of(mb, id) else { return mb.mail.get(&id).map(|m| &m.categories) };
        match kind {
            Kind::Appointment => mb.appointments.get(&id).map(|a| &a.categories),
            Kind::Contact => mb.contacts.get(&id).map(|c| &c.categories),
            Kind::Task => mb.tasks.get(&id).map(|t| &t.categories),
            Kind::Note => mb.notes.get(&id).map(|n| &n.categories),
        }
    }

    pub fn categories_mut(mb: &mut Mailbox, id: ItemId) -> Option<&mut Vec<String>> {
        let Some(kind) = kind_of(mb, id) else { return mb.mail.get_mut(&id).map(|m| &mut m.categories) };
        match kind {
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
            let uid = super::new_uid(s, new, stamp);
            let mb = &mut s.mailbox;
            match kind {
                Kind::Appointment => {
                    if let Some(mut a) = mb.appointments.get(&id).cloned() {
                        a.id = new;
                        a.folder = folder;
                        a.uid = uid.clone();
                        // A copied exception stands alone (it has a new UID, so no series).
                        a.recurrence_id = None;
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
        crate::session::note_recent_move(s, folder);
        Ok(json!({ "moved": ids_json(&out) }))
    }

    /// Generic `<area>.categorize {name?, on?}` / `{clear: true}` (no name = the Quick Click category).
    /// Without `on`, toggles: removes the category when every target has it, else adds it to all.
    pub fn run_categorize(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
        let ids = require_targets(s, p, kind)?;
        categorize_ids(s, p, &ids)
    }

    /// [`run_categorize`] on the items `ids` (PIM items and messages).
    pub fn categorize_ids(s: &mut Session, p: &Value, ids: &[ItemId]) -> CmdResult {
        let ids = ids.to_vec();
        if opt_bool(p, "clear") == Some(true) {
            for id in &ids {
                if let Some(c) = categories_mut(&mut s.mailbox, *id) {
                    c.clear();
                }
            }
            return Ok(json!({ "ids": ids_json(&ids), "cleared": true }));
        }
        // Without a name: the Quick Click category.
        let name = match opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None => crate::cmd::categories::quick_click_category(s)
                .ok_or_else(|| CmdError::Params("`name` required (there are no categories)".into()))?,
        };
        let name = name.as_str();
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

pub use mailcraft_model::calendar::{
    CalendarColumn, CalendarCurrentView, CalendarMode, CalendarProps, CalendarSource, CalendarTimeZones,
    CalendarViewState, DailyTaskList, Subscription,
};

/// Daily Task List: unfinished tasks per local day in `[from_day, to_day)` (days since 1970-01-01 in offset
/// `tz`), each on its due day (else its start day); overdue tasks show on `today`. Sorted by day, then due
/// time and subject; at most `max`.
pub fn tasks_for_days(
    mb: &mailcraft_model::Mailbox,
    tz: i32,
    today: i64,
    from_day: i64,
    to_day: i64,
    max: usize,
) -> Vec<(i64, ItemId)> {
    let mut out: Vec<(i64, i64, String, ItemId)> = mb
        .tasks
        .values()
        .filter(|t| t.status != mailcraft_model::TaskStatus::Completed && t.completed.is_none())
        .filter(|t| mb.folder(t.folder).is_some_and(|f| f.role != Some(FolderRole::DeletedItems)))
        .filter_map(|t| {
            let when = t.due.or(t.start)?;
            let day = when.day_number(tz).max(today);
            (from_day..to_day).contains(&day).then(|| (day, when.unix, t.subject.to_lowercase(), t.id))
        })
        .collect();
    out.sort();
    out.into_iter().take(max).map(|(d, _, _, id)| (d, id)).collect()
}

/// Allowed Time Scale slot sizes (minutes).
pub const TIME_SCALES: [u8; 6] = [60, 30, 15, 10, 6, 5];

/// Calendar colours offered by View › Color.
pub const CALENDAR_COLORS: [&str; 10] =
    ["Automatic", "Blue", "Green", "Orange", "Navy Blue", "Yellow", "Teal", "Magenta", "Brown", "Dark Red"];

/// RGB of the named colours of [`CALENDAR_COLORS`] (Automatic picks one of them per calendar).
pub const CALENDAR_RGB: [(&str, [u8; 3]); 9] = [
    ("Blue", [0x3b, 0x78, 0xc2]),
    ("Green", [0x4f, 0x9a, 0x4a]),
    ("Orange", [0xe0, 0x88, 0x3a]),
    ("Navy Blue", [0x2b, 0x3f, 0x7a]),
    ("Yellow", [0xe6, 0xc3, 0x39]),
    ("Teal", [0x2a, 0x8c, 0x8c]),
    ("Magenta", [0xb4, 0x4b, 0x9a]),
    ("Brown", [0x8a, 0x5a, 0x3a]),
    ("Dark Red", [0x9a, 0x2a, 0x2a]),
];

/// The colour a calendar is drawn in: its View › Color, else (Automatic) the palette entry for its position
/// among the calendar folders (the first calendar blue, the next green, …).
pub fn calendar_color(mb: &mailcraft_model::Mailbox, folder: FolderId) -> (&'static str, [u8; 3]) {
    let chosen = props::read(mb, folder).color;
    if let Some(c) =
        chosen.as_deref().and_then(|c| CALENDAR_RGB.iter().find(|(n, _)| n.eq_ignore_ascii_case(c)))
    {
        return *c;
    }
    let pos = mb.folders.values().filter(|f| f.kind == FolderKind::Calendar).position(|f| f.id == folder);
    let fallback = ("Blue", [0x3b, 0x78, 0xc2]);
    pos.and_then(|i| CALENDAR_RGB.get(i % CALENDAR_RGB.len())).copied().unwrap_or(fallback)
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
/// reset). Also re-reads the display offset of a named time zone (it moves with daylight saving).
fn view_param(s: &mut Session, p: &Value) -> Result<CalendarViewState, CmdError> {
    views::refresh_time_zone(s);
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
    v.sanitize_table();
    Ok(v)
}

/// Shows the Calendar module with view `v`, saves it on the current calendar and returns `{"view": v}`.
fn view_result(s: &mut Session, v: &CalendarViewState) -> CmdResult {
    s.view.module = Module::Calendar;
    views::save_view(s, v);
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
    // An arrangement shows the calendar grid (leaving the List / Active tables).
    if v.current.is_table() {
        v.current = CalendarCurrentView::Calendar;
    }
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
    let stored = (*color != "Automatic").then(|| (*color).to_owned());
    props::update(&mut s.mailbox, folder, |x| x.color = stored);
    s.view.module = Module::Calendar;
    let (name, rgb) = calendar_color(&s.mailbox, folder);
    Ok(json!({
        "calendar": folder.0,
        "color": color,
        "shown": name,
        "rgb": format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]),
    }))
}

/// Calendar Properties (the calendar tab of Folder › Properties): `color` (as View › Color), and for an
/// internet calendar its address (`url`, http(s) or webcal) and `refreshMin` (minutes between updates,
/// kept within the allowed range); for any other calendar `readOnly` (its items cannot be changed until it is
/// turned off again; an internet calendar is always read-only). `updateNow` downloads an internet calendar
/// again (like Update Folder). A new address forgets what the old one sent and is downloaded on the next
/// update. Returns the calendar's properties as Folder › Properties shows them.
fn run_set_properties(s: &mut Session, p: &Value) -> CmdResult {
    let folder = target_calendar(s, p)?;
    let mut x = props::read(&s.mailbox, folder);
    if let Some(c) = opt_str(p, "color") {
        let color = CALENDAR_COLORS.iter().find(|x| x.eq_ignore_ascii_case(c.trim())).ok_or_else(|| {
            CmdError::Params(format!("`color` must be one of {}", CALENDAR_COLORS.join(", ")))
        })?;
        x.color = (*color != "Automatic").then(|| (*color).to_owned());
    }
    let internet = x.source == mailcraft_model::calendar::CalendarSource::Internet;
    if let Some(url) = pim::text(p, "url")? {
        let sub = x
            .subscription
            .as_mut()
            .filter(|_| internet)
            .ok_or_else(|| CmdError::Params("only an internet calendar has an address".into()))?;
        let url = props::normalize_url(&url).ok_or_else(|| {
            CmdError::Params("the address must start with http://, https:// or webcal://".into())
        })?;
        if url != sub.url {
            *sub = Subscription { url, refresh_min: sub.refresh_min, ..Default::default() };
        }
    }
    if let Some(m) = opt_u64(p, "refreshMin") {
        let sub =
            x.subscription.as_mut().filter(|_| internet).ok_or_else(|| {
                CmdError::Params("only an internet calendar is updated on a schedule".into())
            })?;
        let m = u32::try_from(m).unwrap_or(u32::MAX);
        sub.refresh_min = m.clamp(props::MIN_REFRESH_MIN, props::MAX_REFRESH_MIN);
    }
    if let Some(ro) = pim::flag(p, "readOnly")? {
        if internet && !ro {
            return Err(CmdError::Params("an internet calendar is always read-only".into()));
        }
        x.read_only = ro || internet;
    }
    props::write(&mut s.mailbox, folder, &x);
    let mut out = calendar_properties_json(&s.mailbox, folder);
    if pim::flag(p, "updateNow")? == Some(true) {
        if !internet {
            return Err(CmdError::Params("only an internet calendar can be updated".into()));
        }
        let r = s.run("sendReceive.folder", &json!({ "folder": folder.0 }))?;
        if let Some(o) = out.as_object_mut() {
            o.insert("update".into(), r.get("status").cloned().unwrap_or(Value::Null));
        }
    }
    Ok(out)
}

/// A calendar's properties as Folder › Properties shows them: colour, source, read-only, and for an
/// internet calendar its address, update interval and last update.
pub fn calendar_properties_json(mb: &mailcraft_model::Mailbox, folder: FolderId) -> Value {
    let x = props::read(mb, folder);
    let source = match x.source {
        mailcraft_model::calendar::CalendarSource::Local => "local",
        mailcraft_model::calendar::CalendarSource::Internet => "internet",
        mailcraft_model::calendar::CalendarSource::File => "file",
    };
    let sub = x.subscription.as_ref();
    json!({
        "folder": folder.0,
        "color": x.color.clone().unwrap_or_else(|| "Automatic".into()),
        "source": source,
        "readOnly": x.read_only,
        "url": sub.map(|x| x.url.clone()),
        "refreshMin": sub.map(|x| x.refresh_min),
        "lastUpdate": sub.and_then(|x| x.last_update),
        "lastError": sub.and_then(|x| x.last_error.clone()),
    })
}

/// Change Time Zone: `{zone}` (IANA id) or `{offsetMin}` sets the calendar's zone; `label`, `secondZone`,
/// `secondLabel`, `showSecond` set the labels and the second zone shown in day and week views. No params =
/// open the dialog.
fn run_change_time_zone(s: &mut Session, p: &Value) -> CmdResult {
    let keys = [
        "offsetMin",
        "zone",
        "label",
        "secondZone",
        "secondLabel",
        "showSecond",
        "thirdZone",
        "thirdLabel",
        "showThird",
        "swap",
    ];
    if !pim::has_any(p, &keys) {
        let z = views::time_zones(s);
        let now = s.now().unix;
        let zones: Vec<Value> =
            tz::ZONES.iter().map(|id| json!({ "id": id, "label": tz::label(id, now) })).collect();
        let params = json!({ "offsetMin": s.tz_offset_min, "timeZones": z, "zones": zones });
        return pim::dialog(s, "timeZone", params);
    }
    let mut z = views::time_zones(s);
    let text = |key: &str| -> Result<Option<String>, CmdError> {
        match pim::text(p, key)? {
            Some(t) if t.chars().count() > 64 => Err(CmdError::Params(format!("`{key}` is too long"))),
            Some(t) => Ok(Some(t.trim().chars().filter(|c| !c.is_control()).collect())),
            None => Ok(None),
        }
    };
    let zone_param = |key: &str| -> Result<Option<String>, CmdError> {
        match text(key)? {
            None => Ok(None),
            Some(t) if t.is_empty() => Ok(Some(String::new())),
            Some(t) => tz::canonical(&t)
                .map(Some)
                .ok_or_else(|| CmdError::Params(format!("`{key}`: unknown time zone `{t}`"))),
        }
    };
    if let Some(zone) = zone_param("zone")? {
        z.zone = zone;
    }
    if let Some(l) = text("label")? {
        z.label = l;
    }
    if let Some(zone) = zone_param("secondZone")? {
        z.second_zone = zone;
    }
    if let Some(l) = text("secondLabel")? {
        z.second_label = l;
    }
    if let Some(on) = pim::flag(p, "showSecond")? {
        z.show_second = on;
    }
    if let Some(zone) = zone_param("thirdZone")? {
        z.third_zone = zone;
    }
    if let Some(l) = text("thirdLabel")? {
        z.third_label = l;
    }
    if let Some(on) = pim::flag(p, "showThird")? {
        z.show_third = on;
    }
    if pim::flag(p, "swap")? == Some(true) {
        if !tz::known(&z.zone) || !tz::known(&z.second_zone) {
            return Err(CmdError::Params("swapping needs two named time zones".into()));
        }
        std::mem::swap(&mut z.zone, &mut z.second_zone);
        std::mem::swap(&mut z.label, &mut z.second_label);
    }
    if z.show_second && !tz::known(&z.second_zone) {
        return Err(CmdError::Params("choose the second time zone (`secondZone`)".into()));
    }
    if z.show_third && !tz::known(&z.third_zone) {
        return Err(CmdError::Params("choose the third time zone (`thirdZone`)".into()));
    }
    if z.show_third && !z.show_second {
        return Err(CmdError::Params("a third time zone is shown with a second one (`showSecond`)".into()));
    }
    let now = s.now().unix;
    let offset = match p.get("offsetMin") {
        None | Some(Value::Null) => tz::offset_at(&z.zone, now),
        Some(v) => {
            let off = v
                .as_i64()
                .filter(|o| (-14 * 60..=14 * 60).contains(o))
                .and_then(|o| i32::try_from(o).ok())
                .ok_or_else(|| CmdError::Params("`offsetMin` must be between -840 and 840".into()))?;
            // A bare offset replaces a named zone that disagrees with it.
            if tz::offset_at(&z.zone, now) != Some(off) {
                z.zone.clear();
            }
            Some(off)
        }
    };
    if let Some(off) = offset {
        s.tz_offset_min = off;
    }
    let root = pim::default_folder(s, Kind::Appointment);
    if let Some(f) = root {
        let keep = (z != CalendarTimeZones::default()).then(|| z.clone());
        props::update(&mut s.mailbox, f, |x| x.time_zones = keep);
    }
    // Mirror into File › Options › Calendar › Time zones (numeric offsets there).
    views::set_option(s, "calendar.timeZone", json!(s.tz_offset_min));
    views::set_option(s, "calendar.timeZoneLabel", json!(z.label));
    if let Some(off) = tz::offset_at(&z.second_zone, now) {
        views::set_option(s, "calendar.secondTimeZoneOffset", json!(off));
    }
    views::set_option(s, "calendar.secondTimeZone", json!(z.show_second));
    views::set_option(s, "calendar.secondTimeZoneLabel", json!(z.second_label));
    Ok(json!({ "offsetMin": s.tz_offset_min, "timeZones": z }))
}

/// Add Calendar. `source`:
/// - `internet` `{url, name?, refreshMin?, data?, fetch?}`: subscribes to an iCalendar URL (read-only
///   calendar, refreshed periodically). Fetches at once unless `fetch: false` or the caller passes the
///   fetched `data`.
/// - `file` `{path | data, name?}`: a new calendar holding a copy of an .ics file's events.
/// - `blank` `{name, parent?}`: Create New Blank Calendar.
/// - `addressBook`, `roomList`, `shared`: other people's and rooms' calendars live on an Exchange server,
///   which MailCraft does not connect to; refused with the reason.
fn run_add_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let Some(source) = opt_str(p, "source") else {
        return pim::dialog(s, "addCalendar", json!({}));
    };
    match source {
        "internet" => add_internet_calendar(s, p),
        "file" => add_file_calendar(s, p),
        "blank" => add_blank_calendar(s, p),
        "addressBook" | "roomList" | "shared" => Err(CmdError::Disabled(
            "opening other people's, rooms' and shared calendars needs a Microsoft Exchange account, which \
             MailCraft does not support; subscribe to an internet calendar (From Internet) instead"
                .into(),
        )),
        _ => Err(CmdError::Params("`source` must be internet|file|blank|addressBook|roomList|shared".into())),
    }
}

/// Optional `data` param: iCalendar text up to the iCalendar size limit.
fn data_text(p: &Value) -> Result<Option<String>, CmdError> {
    match p.get("data") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) if t.len() <= mailcraft_formats::ics::MAX_ICS_BYTES => Ok(Some(t.clone())),
        Some(Value::String(_)) => Err(CmdError::Params("`data` is too large for a calendar".into())),
        Some(_) => Err(CmdError::Params("`data` must be iCalendar text".into())),
    }
}

/// Optional `name` param, trimmed and non-empty.
fn opt_name(p: &Value) -> Result<Option<String>, CmdError> {
    Ok(pim::text(p, "name")?.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()))
}

/// Creates calendar folder `name` under `parent` (`name (2)`, `name (3)`… when taken).
fn create_calendar(s: &mut Session, parent: FolderId, name: &str) -> Result<FolderId, CmdError> {
    let base: String = name.chars().filter(|c| !c.is_control()).take(200).collect();
    let base = if base.trim().is_empty() { "Calendar".to_owned() } else { base.trim().to_owned() };
    let mut last = None;
    for n in 1..=50u32 {
        let candidate = if n == 1 { base.clone() } else { format!("{base} ({n})") };
        match s.mailbox.create_folder_of_kind(parent, &candidate, Some(FolderKind::Calendar)) {
            Ok(id) => return Ok(id),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| CmdError::Failed("could not create the calendar".into()), CmdError::from))
}

/// Where Add Calendar puts new calendars: `params.parent` (any folder that can hold folders, like Create New
/// Folder's "Select where to place the folder"), else the default calendar.
fn calendar_parent(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    if let Some(f) = opt_u64(p, "parent").map(FolderId) {
        return match s.mailbox.folder(f).map(|x| x.kind) {
            None => Err(CmdError::Params(format!("no such folder {}", f.0))),
            Some(FolderKind::Search | FolderKind::SearchRoot) => {
                Err(CmdError::Params("a calendar cannot go under Search Folders".into()))
            }
            Some(_) => Ok(f),
        };
    }
    pim::default_folder(s, Kind::Appointment)
        .ok_or_else(|| CmdError::Failed("there is no calendar folder".into()))
}

fn add_internet_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let Some(raw) = pim::text(p, "url")?.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) else {
        return pim::dialog(s, "addCalendar", json!({ "source": "internet" }));
    };
    if raw.len() > 4096 {
        return Err(CmdError::Params("`url` is too long".into()));
    }
    let url = props::normalize_url(&raw)
        .ok_or_else(|| CmdError::Params("`url` must be an http(s) or webcal address".into()))?;
    let refresh = match p.get("refreshMin") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|m| u32::try_from(m).ok())
                .filter(|m| (props::MIN_REFRESH_MIN..=props::MAX_REFRESH_MIN).contains(m))
                .ok_or_else(|| {
                    CmdError::Params(format!(
                        "`refreshMin` must be {} to {} minutes",
                        props::MIN_REFRESH_MIN,
                        props::MAX_REFRESH_MIN
                    ))
                })?,
        ),
    };
    let parent = calendar_parent(s, p)?;
    // The fetched calendar (passed in, or fetched now) names the calendar when no name is given.
    let outcome: Option<Result<subscribe::Fetched, String>> = match data_text(p)? {
        Some(d) if !d.is_empty() => {
            Some(Ok(subscribe::Fetched::Data { body: d.into_bytes(), etag: None, last_modified: None }))
        }
        _ if pim::flag(p, "fetch")? == Some(false) => None,
        _ => Some(subscribe::fetch(&subscribe::Due {
            folder: FolderId(0),
            url: url.clone(),
            etag: None,
            last_modified: None,
        })),
    };
    let published = match &outcome {
        Some(Ok(subscribe::Fetched::Data { body, .. })) => subscribe::calendar_meta(body).0,
        _ => None,
    };
    let name = opt_name(p)?.or(published).unwrap_or_else(|| props::url_host(&url));
    let folder = create_calendar(s, parent, &name)?;
    let sub = Subscription {
        url: url.clone(),
        refresh_min: refresh.unwrap_or(props::DEFAULT_REFRESH_MIN),
        ..Default::default()
    };
    props::update(&mut s.mailbox, folder, |x| {
        x.source = CalendarSource::Internet;
        x.read_only = true;
        x.subscription = Some(sub);
    });
    let mut out =
        json!({ "folder": folder.0, "name": s.mailbox.folder(folder).map(|f| f.name.clone()), "url": url });
    if let Some(o) = outcome {
        let r = subscribe::apply(s, folder, o);
        // An explicit refresh interval wins over the publisher's.
        if let Some(m) = refresh {
            props::update(&mut s.mailbox, folder, |x| {
                if let Some(sub) = x.subscription.as_mut() {
                    sub.refresh_min = m;
                }
            });
        }
        if let (Some(o), Some(r)) = (out.as_object_mut(), r.as_object()) {
            for k in ["events", "notModified", "error"] {
                if let Some(v) = r.get(k) {
                    o.insert(k.into(), v.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Largest .ics file read by From File.
const MAX_FILE_BYTES: u64 = mailcraft_formats::ics::MAX_ICS_BYTES as u64;

fn add_file_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let (data, stem) = match (data_text(p)?, pim::text(p, "path")?) {
        (Some(d), _) if !d.is_empty() => (d.into_bytes(), None),
        (_, Some(path)) if !path.trim().is_empty() => {
            let path = std::path::PathBuf::from(path.trim());
            let meta = std::fs::metadata(&path)
                .map_err(|e| CmdError::Failed(format!("cannot open {}: {e}", path.display())))?;
            if !meta.is_file() {
                return Err(CmdError::Params(format!("{} is not a file", path.display())));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(CmdError::Params(format!("{} is too large for a calendar", path.display())));
            }
            let data = std::fs::read(&path)
                .map_err(|e| CmdError::Failed(format!("cannot read {}: {e}", path.display())))?;
            (data, path.file_stem().map(|x| x.to_string_lossy().into_owned()))
        }
        _ => return pim::dialog(s, "addCalendar", json!({ "source": "file" })),
    };
    // Refuse non-calendars before creating anything.
    mailcraft_formats::ics::read(&data, ImportOptions { local_offset_min: s.tz_offset_min })
        .map_err(|e| CmdError::Params(format!("not an iCalendar file: {e}")))?;
    let parent = calendar_parent(s, p)?;
    let name = opt_name(p)?
        .or_else(|| subscribe::calendar_meta(&data).0)
        .or(stem)
        .unwrap_or_else(|| "Calendar".into());
    let folder = create_calendar(s, parent, &name)?;
    props::update(&mut s.mailbox, folder, |x| x.source = CalendarSource::File);
    let events = subscribe::replace_items(s, folder, &data, true)?;
    Ok(
        json!({ "folder": folder.0, "name": s.mailbox.folder(folder).map(|f| f.name.clone()), "events": events }),
    )
}

fn add_blank_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let Some(name) = opt_name(p)? else {
        return pim::dialog(s, "addCalendar", json!({ "source": "blank" }));
    };
    let parent = calendar_parent(s, p)?;
    let folder = create_calendar(s, parent, &name)?;
    Ok(json!({ "folder": folder.0, "name": s.mailbox.folder(folder).map(|f| f.name.clone()) }))
}

/// Refresh internet calendars. `{folder}` one, `{all: true}` every subscription, `{due: true}` those whose
/// refresh interval has passed; default the selected calendar when it is a subscription, else all. A worker
/// that fetched already passes the outcome for one `folder`: `data` (iCalendar text, with `etag` and
/// `lastModified`), `notModified: true` or `error`.
fn run_refresh_calendar(s: &mut Session, p: &Value) -> CmdResult {
    let folder = opt_u64(p, "folder").map(FolderId);
    let given: Option<Result<subscribe::Fetched, String>> = if let Some(d) = data_text(p)? {
        let etag = pim::text(p, "etag")?.filter(|e| !e.is_empty());
        let last_modified = pim::text(p, "lastModified")?.filter(|e| !e.is_empty());
        Some(Ok(subscribe::Fetched::Data { body: d.into_bytes(), etag, last_modified }))
    } else if pim::flag(p, "notModified")? == Some(true) {
        Some(Ok(subscribe::Fetched::NotModified))
    } else {
        pim::text(p, "error")?.map(Err)
    };
    if let Some(outcome) = given {
        let f =
            folder.ok_or_else(|| CmdError::Params("`folder` is required with a fetched result".into()))?;
        subscribe::subscription(s, f)
            .ok_or_else(|| CmdError::Params(format!("folder {} is not an internet calendar", f.0)))?;
        return Ok(json!({ "refreshed": [subscribe::apply(s, f, outcome)] }));
    }
    let targets: Vec<subscribe::Due> = if let Some(f) = folder {
        vec![
            subscribe::subscription(s, f)
                .ok_or_else(|| CmdError::Params(format!("folder {} is not an internet calendar", f.0)))?,
        ]
    } else if pim::flag(p, "due")? == Some(true) {
        subscribe::subscriptions(s, false)
    } else {
        let current = pim::current_folder(s, Kind::Appointment).and_then(|f| subscribe::subscription(s, f));
        match (current, pim::flag(p, "all")?) {
            (Some(c), None | Some(false)) => vec![c],
            _ => subscribe::subscriptions(s, true),
        }
    };
    let mut out = Vec::new();
    for d in targets.into_iter().take(200) {
        let outcome = subscribe::fetch(&d);
        out.push(subscribe::apply(s, d.folder, outcome));
    }
    Ok(json!({ "refreshed": out }))
}

/// Share Calendar › E-mail Calendar: a message with the calendar as an iCalendar attachment. `range`: `all`
/// (default), `today`, `tomorrow`, `next7`, `next30` or `custom` (`from` to `to`, whole days, "YYYY-MM-DD");
/// `detail`: `full` (default), `limited` (subject and time) or `availability` (busy/free blocks only).
/// Private items are shared as their free/busy time only unless `includePrivate` is true (then like any
/// other item). Reminders are left out.
fn run_share(s: &mut Session, p: &Value) -> CmdResult {
    let folder = target_calendar(s, p)?;
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let clock = tz::Clock::of(s);
    let today = clock.midnight(s.now());
    let day_after = |t: DateTime, days: i64| {
        clock.from_local(clock.day(t).saturating_add(days).saturating_mul(pim::DAY_SECS))
    };
    let range = opt_str(p, "range").unwrap_or("all");
    let window = match range {
        "all" => None,
        "today" => Some((today, day_after(today, 1))),
        "tomorrow" => Some((day_after(today, 1), day_after(today, 2))),
        "next7" => Some((today, day_after(today, 7))),
        "next30" => Some((today, day_after(today, 30))),
        "custom" => {
            let day = |key: &str| -> Result<DateTime, CmdError> {
                clock.date_param(p, key)?.flatten().map(|t| clock.midnight(t)).ok_or_else(|| {
                    CmdError::Params(format!("`{key}` (YYYY-MM-DD) is required for a custom range"))
                })
            };
            let (from, to) = (day("from")?, day("to")?);
            if to.unix < from.unix {
                return Err(CmdError::Params("the end date is before the start date".into()));
            }
            if to.unix.saturating_sub(from.unix) > MAX_SPAN_SECS {
                return Err(CmdError::Params("the date range is too long".into()));
            }
            Some((from, day_after(to, 1)))
        }
        r => {
            return Err(CmdError::Params(format!(
                "`range` must be all|today|tomorrow|next7|next30|custom, not `{r}`"
            )));
        }
    };
    let detail = opt_str(p, "detail").unwrap_or("full");
    if !["full", "limited", "availability"].contains(&detail) {
        return Err(CmdError::Params(format!("`detail` must be full|limited|availability, not `{detail}`")));
    }
    let include_private = pim::flag(p, "includePrivate")?.unwrap_or(false);
    let mut appts: Vec<Appointment> = s
        .mailbox
        .appointments
        .values()
        .filter(|a| a.folder == folder)
        .filter(|a| match window {
            None => true,
            Some((from, to)) => !occurrences(a, from, to, 1).is_empty(),
        })
        .cloned()
        .collect();
    appts.sort_by_key(|a| (a.start.unix, a.id));
    for a in &mut appts {
        let hidden = a.private && !include_private;
        if detail != "full" || hidden {
            a.body.clear();
            a.attendees.clear();
            a.organizer = None;
            a.categories.clear();
        }
        if detail == "availability" || hidden {
            a.subject = show_as_label(a.show_as).to_owned();
            a.location.clear();
        }
        // The copy is not the user's item: no alarms go out with it.
        a.reminder_min = None;
    }
    let refs: Vec<&Appointment> = appts.iter().collect();
    let ics = ics_calendar(Some("PUBLISH"), &refs, s.now());
    let events = appts.len();
    let calendars: Vec<Value> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Calendar)
        .map(|f| json!({ "id": f.id.0, "name": f.name }))
        .collect();
    let mut compose = ComposeState {
        subject: name.clone(),
        attachments: vec![ComposeAttachment {
            filename: format!("{}.ics", file_safe(&name)),
            mime_type: "text/calendar".into(),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    // Options › Mail › Signatures: the new-messages signature.
    crate::compose::insert_default_signature(&s.mailbox, &mut compose);
    let id = s.open_compose(compose);
    Ok(json!({
        "compose": id,
        "calendar": folder.0,
        "events": events,
        "calendars": calendars,
        "range": range,
        "detail": detail,
        "from": window.map(|(f, _)| clock.datetime_string(f)),
        "to": window.map(|(_, t)| clock.datetime_string(t)),
    }))
}

/// "Busy", "Free"… for availability-only sharing.
fn show_as_label(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "Free",
        BusyStatus::WorkingElsewhere => "Working Elsewhere",
        BusyStatus::Tentative => "Tentative",
        BusyStatus::Busy => "Busy",
        BusyStatus::OutOfOffice => "Out of Office",
    }
}

fn file_safe(name: &str) -> String {
    let n: String =
        name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    if n.trim().is_empty() { "calendar".into() } else { n.trim().to_owned() }
}

// ---------------------------------------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------------------------------------

/// Default appointment length out of the box (minutes; File › Options › Calendar changes it,
/// [`default_duration_min`]).
pub const DEFAULT_DURATION_MIN: i64 = 30;
/// Default reminder out of the box (minutes before start; [`default_reminder_min`]).
pub const DEFAULT_REMINDER_MIN: u32 = 15;
/// Longest appointment accepted (seconds; ten years).
const MAX_SPAN_SECS: i64 = 10 * 366 * pim::DAY_SECS;

const APPT_FIELDS: [&str; 16] = [
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
    "timeZone",
];

/// Start of the next half-hour slot after `now` (local time).
pub fn next_half_hour(now: DateTime, tz: i32) -> DateTime {
    let shift = i64::from(tz).saturating_mul(60);
    let local = now.unix.saturating_add(shift);
    let slot = local.div_euclid(1800).saturating_add(1).saturating_mul(1800);
    DateTime::from_unix(slot.saturating_sub(shift), tz)
}

/// UID for a new appointment: the item id and time plus a hash of the user's addresses, so meetings from
/// different people (whose mailboxes number items alike) never share a UID (RFC 5545 §3.8.4.7).
pub(crate) fn new_uid(s: &Session, id: ItemId, stamp: i64) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for a in &s.mailbox.accounts {
        feed(a.address.trim().to_ascii_lowercase().as_bytes());
        feed(&[0]);
    }
    feed(&id.0.to_be_bytes());
    feed(&stamp.to_be_bytes());
    format!("mailcraft-{}-{}-{:016x}@localhost", id.0, stamp, h)
}

/// Length of a new appointment or meeting in minutes: File › Options › Calendar › Default duration, less the
/// "Shorten appointments and meetings" minutes when that is on (never below 5 minutes).
pub fn default_duration_min(s: &Session) -> i64 {
    let full = i64::from(s.settings.default_duration_minutes()).clamp(1, 24 * 60);
    if !s.settings.bool("calendar.shorten") {
        return full;
    }
    let by = if full < 60 {
        s.settings.int("calendar.shortenUnderHour")
    } else {
        s.settings.int("calendar.shortenHourOrLonger")
    };
    let short = full.saturating_sub(by.clamp(0, 60));
    if short >= 5 { short } else { full }
}

/// Reminder of a new timed appointment or meeting (minutes before the start): File › Options › Calendar ›
/// Default reminders, `None` when that is off.
pub fn default_reminder_min(s: &Session) -> Option<u32> {
    s.settings.default_reminder_minutes().map(|m| m.min(60 * 24 * 7 * 52))
}

/// A new appointment with the defaults: the next half hour, the default duration and reminder from
/// File › Options › Calendar (30 minutes and 15 minutes before out of the box), Busy.
pub fn default_appointment(s: &Session, folder: FolderId) -> Appointment {
    let start = next_half_hour(s.now(), s.tz_offset_min);
    Appointment {
        folder,
        start,
        end: pim::add_secs(start, default_duration_min(s).saturating_mul(60)),
        show_as: BusyStatus::Busy,
        reminder_min: default_reminder_min(s),
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
    let cal = tz::Clock::of(s);
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
    match p.get("timeZone") {
        None => {}
        Some(Value::Null) => a.tz = None,
        Some(Value::String(z)) if z.trim().is_empty() => a.tz = None,
        Some(Value::String(z)) => {
            a.tz = Some(
                tz::canonical(z)
                    .ok_or_else(|| CmdError::Params(format!("`timeZone`: unknown time zone `{z}`")))?,
            );
        }
        Some(_) => return Err(CmdError::Params("`timeZone` must be an IANA time zone name".into())),
    }
    // Wall-clock times are read in the zone named by this call's `timeZone` (timed appointments), else on the
    // calendar's clock (its named zone, daylight-saving aware, else the session's offset), the clock the
    // calendar views and the appointment window show times on.
    let explicit = match p.get("timeZone") {
        Some(Value::String(z)) if !z.trim().is_empty() && !a.all_day => a.zone().map(str::to_owned),
        _ => None,
    };
    let clock = explicit.map_or_else(|| tz::Clock::of(s), |z| tz::Clock::named(&z, tz));
    let start = clock.date_param(p, "start")?;
    match start {
        Some(Some(t)) => a.start = t,
        Some(None) => return Err(CmdError::Params("`start` cannot be cleared".into())),
        None => {}
    }
    let end = clock.date_param(p, "end")?;
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
        // Whole days on the calendar's clock (a day next to a daylight-saving change is 23 or 25 hours).
        let (start, end) = whole_days(&cal, a.start, a.end);
        a.start = start;
        a.end = end;
    }
    if a.end.unix < a.start.unix {
        return Err(CmdError::Params("the end time is before the start time".into()));
    }
    // A zoned appointment's times carry its zone's offset (the same instants).
    if !a.all_day
        && let Some(z) = a.zone().map(str::to_owned)
    {
        a.start = tz::in_zone(&z, a.start).unwrap_or(a.start);
        a.end = tz::in_zone(&z, a.end).unwrap_or(a.end);
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
    let rule = match p.get("recurrence") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(r)) if r.trim().is_empty() => Some(None),
        Some(Value::String(r)) => {
            let r = r.trim().trim_start_matches("RRULE:").trim();
            if r.len() > 1000 {
                return Err(CmdError::Params("`recurrence` is too long".into()));
            }
            rrule::Rrule::parse(r).map_err(|e| {
                CmdError::Params(format!("`recurrence` must be an RRULE value such as FREQ=WEEKLY ({e})"))
            })?;
            Some(Some(r.to_owned()))
        }
        Some(v @ Value::Object(_)) => {
            let form: recurrence::RecurrenceForm = serde_json::from_value(v.clone())
                .map_err(|e| CmdError::Params(format!("`recurrence`: {e}")))?;
            Some(Some(form.to_rrule(a.all_day, tz).map_err(CmdError::Params)?))
        }
        Some(_) => return Err(CmdError::Params("`recurrence` must be an RRULE string or a pattern".into())),
    };
    match rule {
        None => {}
        Some(None) => a.recurrence = None,
        // The same rule keeps its exceptions (deleted / edited occurrences).
        Some(Some(r)) => {
            match a.recurrence.as_mut() {
                Some(old) if old.rrule.eq_ignore_ascii_case(&r) => {}
                _ => a.recurrence = Some(Recurrence { rrule: r, exdates: Vec::new() }),
            }
            sync_series_start(a);
        }
    }
    Ok(())
}

/// Moves a series' start (and end) forward to the first day its pattern produces, so the series starts on
/// one of its occurrences: RFC 5545 §3.8.5.3 wants DTSTART to be the first instance, and an unsynchronised
/// DTSTART (a "every Monday" series saved on a Wednesday) would add a stray occurrence. Leaves the series
/// alone when the pattern produces nothing before its end. Returns true when it moved.
pub fn sync_series_start(a: &mut Appointment) -> bool {
    let Some(rec) = &a.recurrence else { return false };
    let Ok(rule) = rrule::Rrule::parse(&rec.rrule) else { return false };
    let off = i64::from(a.start.offset_min).saturating_mul(60);
    let local = a.start.unix.saturating_add(off);
    let day = local.div_euclid(pim::DAY_SECS);
    let Some(first) = rule.first_day_from(day) else { return false };
    let shift = first.saturating_sub(day).saturating_mul(pim::DAY_SECS);
    if shift <= 0 {
        return false;
    }
    let (new_local, new_unix) = (local.saturating_add(shift), a.start.unix.saturating_add(shift));
    let past_end = match rule.until {
        Some(rrule::Until::Date(d)) => new_local.div_euclid(pim::DAY_SECS) > d.div_euclid(pim::DAY_SECS),
        Some(rrule::Until::Local(l)) => new_local > l,
        Some(rrule::Until::Utc(u)) => new_unix > u,
        None => false,
    };
    if past_end {
        return false;
    }
    a.start = DateTime::from_unix(new_unix, a.start.offset_min);
    a.end = DateTime::from_unix(a.end.unix.saturating_add(shift), a.end.offset_min);
    true
}

/// Midnight starting the day of `start` to the midnight ending the day of `end` (at least one day), on
/// `clock`.
fn whole_days(clock: &tz::Clock, start: DateTime, end: DateTime) -> (DateTime, DateTime) {
    let first = clock.day(start);
    let next_midnight = |day: i64| clock.from_local(day.saturating_add(1).saturating_mul(pim::DAY_SECS));
    let s = clock.midnight(start);
    let end_mid = clock.midnight(end);
    let e = if end_mid.unix < end.unix { next_midnight(clock.day(end)) } else { end_mid };
    (s, if e.unix <= s.unix { next_midnight(first) } else { e })
}

/// Summary of an appointment for results, its times on the calendar's clock.
pub fn appointment_summary(s: &Session, a: &Appointment) -> Value {
    appointment_json_on(a, &tz::Clock::of(s))
}

/// Summary of an appointment for results, its times at fixed offset `tz`.
pub fn appointment_json(a: &Appointment, tz: i32) -> Value {
    appointment_json_on(a, &tz::Clock::fixed(tz))
}

fn appointment_json_on(a: &Appointment, clock: &tz::Clock) -> Value {
    let tz = clock.offset_at(a.start.unix);
    json!({
        "id": a.id.0,
        "folder": a.folder.0,
        "subject": a.subject,
        "location": a.location,
        "start": clock.datetime_string(a.start),
        "end": clock.datetime_string(a.end),
        "allDay": a.all_day,
        "showAs": show_as_name(a.show_as),
        "reminderMin": a.reminder_min,
        "meeting": a.is_meeting,
        "attendees": a.attendees.iter().map(|x| x.mailbox.to_display_string()).collect::<Vec<_>>(),
        "categories": a.categories,
        "recurrence": a.recurrence.as_ref().map(|r| r.rrule.clone()),
        "recurrenceText": describe_recurrence(a, tz),
        "organizer": a.organizer.as_ref().map(Address::to_display_string),
        "tracking": tracking_json(a),
        "timeZone": a.zone(),
    })
}

/// Creates an appointment in `folder` from field params (on top of the defaults). Returns its id.
pub fn create_appointment(
    s: &mut Session,
    folder: FolderId,
    p: &Value,
    meeting: bool,
) -> Result<ItemId, CmdError> {
    let mut a = draft_appointment(s, folder, p, meeting)?;
    let id = ItemId(s.mailbox.alloc_id());
    a.id = id;
    a.uid = new_uid(s, id, s.now().unix);
    s.mailbox.appointments.insert(id, a);
    Ok(id)
}

/// The appointment `create_appointment` would store (validated, not stored, no id or UID yet).
fn draft_appointment(
    s: &Session,
    folder: FolderId,
    p: &Value,
    meeting: bool,
) -> Result<Appointment, CmdError> {
    let mut a = default_appointment(s, folder);
    // A timed appointment is entered in the calendar's time zone (when it names one), so a series keeps its
    // wall-clock time across daylight-saving changes; `timeZone` overrides it.
    a.tz = tz::Clock::of(s).zone_id().map(str::to_owned);
    apply_appointment_fields(s, &mut a, p)?;
    if a.all_day {
        a.tz = None;
        // An all-day event's default reminder is the evening before (18 hours ahead), as the window offers.
        if p.get("reminderMin").is_none() && a.reminder_min.is_some() {
            a.reminder_min = Some(ALL_DAY_REMINDER_MIN);
        }
    }
    if meeting {
        a.is_meeting = true;
    }
    if a.is_meeting && a.organizer.is_none() {
        a.organizer = organizer(s);
    }
    Ok(a)
}

/// Default reminder of an all-day event (minutes before its midnight start).
pub const ALL_DAY_REMINDER_MIN: u32 = 18 * 60;

/// `form`: `Some(true)` = always open the window, `Some(false)` = never; else `default`.
fn wants_form(p: &Value, default: bool) -> Result<bool, CmdError> {
    Ok(pim::flag(p, "form")?.unwrap_or(default))
}

/// Asks the UI for an appointment or meeting window prefilled from field params, without saving anything
/// (cancelling the window leaves nothing behind). The draft is validated first, so bad fields are refused
/// here. Dialog `appointment` (a meeting: `newMeeting`, the dialog the meeting form answers to) with
/// `folder`, `start`/`end` on the calendar's clock (all-day: the end falls on the last day), `reminderMin`,
/// `subject`,
/// `location`, `body`, `allDay`, `showAs`, `private`, `categories`, `recurrence` (RRULE), `recurring`
/// (open the Recurrence dialog), `meeting`, `attendees` / `optional` / `resources` (lists) and `to` (the
/// required attendees as one string).
fn open_item_form(s: &mut Session, folder: FolderId, p: &Value, meeting: bool) -> CmdResult {
    let a = draft_appointment(s, folder, p, meeting)?;
    let clock = tz::Clock::of(s);
    let (start, end) = if a.all_day {
        // The window shows an all-day event's inclusive last day.
        let last = pim::add_secs(a.end, -1).unix.max(a.start.unix);
        (clock.datetime_string(a.start), clock.datetime_string(DateTime::from_unix(last, a.end.offset_min)))
    } else {
        (clock.datetime_string(a.start), clock.datetime_string(a.end))
    };
    let names = |role: AttendeeRole| -> Vec<String> {
        a.attendees.iter().filter(|x| x.role == role).map(|x| x.mailbox.to_display_string()).collect()
    };
    let required = names(AttendeeRole::Required);
    let params = json!({
        "folder": folder.0,
        "start": start,
        "end": end,
        "allDay": a.all_day,
        "reminderMin": a.reminder_min,
        "showAs": show_as_name(a.show_as),
        "private": a.private,
        "categories": a.categories,
        "subject": a.subject,
        "location": a.location,
        "body": a.body,
        "recurrence": a.recurrence.as_ref().map(|r| r.rrule.clone()),
        "recurring": pim::flag(p, "recurring")?.unwrap_or(false),
        "meeting": a.is_meeting,
        "to": required.join("; "),
        "attendees": required,
        "optional": names(AttendeeRole::Optional),
        "resources": names(AttendeeRole::Resource),
        "timeZone": if a.all_day { None } else { a.tz.clone() },
        "focus": if a.subject.trim().is_empty() { "subject" } else { "body" },
    });
    pim::dialog(s, if a.is_meeting { MEETING_FORM } else { APPOINTMENT_FORM }, params)
}

/// Dialog id of the appointment window.
pub const APPOINTMENT_FORM: &str = "appointment";
/// Dialog id of the meeting window (a new meeting request, not yet saved).
pub const MEETING_FORM: &str = "newMeeting";

/// `calendar.newAppointment`: with field params creates the appointment; without (or with `form: true`)
/// opens the appointment window prefilled with them.
fn run_new_appointment(s: &mut Session, p: &Value) -> CmdResult {
    let folder = new_appointment_folder(s, p)?;
    if wants_form(p, !pim::has_any(p, &APPT_FIELDS))? {
        return open_item_form(s, folder, p, false);
    }
    let id = create_appointment(s, folder, p, false)?;
    Ok(s.mailbox.appointments.get(&id).map_or_else(|| json!({ "id": id.0 }), |a| appointment_summary(s, a)))
}

/// New Meeting: a meeting request starts as a form. With a `start` (a fully specified request) or
/// `form: false` the meeting is created and its invitation opens in a compose (`{"id", "compose", ...}`);
/// otherwise the meeting window opens prefilled with the given fields (`{"dialog": "newMeeting"}`) and
/// nothing is saved until it is sent. People › Meeting and Tasks › Meeting come through here too.
pub fn create_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let folder = new_appointment_folder(s, p)?;
    let explicit = p.get("start").is_some_and(|v| !v.is_null());
    if wants_form(p, !explicit)? {
        return open_item_form(s, folder, p, true);
    }
    let id = create_appointment(s, folder, p, true)?;
    let compose =
        meeting_compose(s, id).ok_or_else(|| CmdError::Failed("the meeting could not be created".into()))?;
    let mut out =
        s.mailbox.appointments.get(&id).map_or_else(|| json!({ "id": id.0 }), |a| appointment_summary(s, a));
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
    meeting_compose_seq(s, id, 0)
}

/// Opens a compose carrying the invitation (iTIP REQUEST, written by `mailcraft-formats`) for appointment
/// `id` with SEQUENCE `sequence` (0 for a new meeting, higher for updates).
pub fn meeting_compose_seq(s: &mut Session, id: ItemId, sequence: u32) -> Option<u64> {
    let a = s.mailbox.appointments.get(&id)?.clone();
    // Times as the organizer's calendar shows them (the offset in force at the start).
    let tz = tz::Clock::of(s).offset_at(a.start.unix);
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
    let when = describe_recurrence(&a, tz).unwrap_or_else(|| when_text(&a, tz));
    let mut text = format!("When: {when}\n");
    if !a.location.trim().is_empty() {
        text.push_str(&format!("Where: {}\n", a.location.trim()));
    }
    if !a.body.trim().is_empty() {
        text.push('\n');
        text.push_str(&a.body);
    }
    let mut ics = itip::request(&a, sequence, s.now());
    // Options › Calendar › "Allow attendees to propose new times for meetings" off: the request says so
    // with the X-MICROSOFT-DISALLOW-COUNTER property of [MS-OXCICAL].
    if !s.settings.bool("calendar.allowProposeNewTime") {
        ics = ics.replace("END:VEVENT\r\n", "X-MICROSOFT-DISALLOW-COUNTER:TRUE\r\nEND:VEVENT\r\n");
    }
    let mut compose = ComposeState {
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
    // Options › Mail › Signatures: the new-messages signature, below the When/Where text.
    crate::compose::insert_default_signature(&s.mailbox, &mut compose);
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
    check_items_writable(s, &ids)?;
    let mut updated = Vec::new();
    for id in &ids {
        let Some(mut a) = s.mailbox.appointments.get(id).cloned() else { continue };
        apply_appointment_fields(s, &mut a, p)?;
        if a.is_meeting && a.organizer.is_none() {
            a.organizer = organizer(s);
        }
        updated.push(a);
    }
    let out: Vec<Value> = updated.iter().map(|a| appointment_summary(s, a)).collect();
    for a in updated {
        s.mailbox.appointments.insert(a.id, a);
    }
    Ok(json!({ "updated": out }))
}

/// Deleting a recurring series deletes its exceptions (edited occurrences, same UID) with it.
fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    let mut ids = pim::require_targets(s, p, Kind::Appointment)?;
    check_items_writable(s, &ids)?;
    let mut extra = Vec::new();
    for id in &ids {
        if let Some(a) = s.mailbox.appointments.get(id).filter(|a| a.recurrence.is_some()) {
            extra.extend(exceptions_of(s, a));
        }
    }
    for x in extra {
        if !ids.contains(&x) {
            ids.push(x);
        }
    }
    let done = pim::delete(s, &ids);
    Ok(json!({ "deleted": pim::ids_json(&done) }))
}

/// The exceptions of series `a`: appointments in its folder with its UID and a RECURRENCE-ID.
fn exceptions_of(s: &Session, a: &Appointment) -> Vec<ItemId> {
    if a.uid.trim().is_empty() {
        return Vec::new();
    }
    s.mailbox
        .appointments
        .values()
        .filter(|x| {
            x.id != a.id
                && x.folder == a.folder
                && x.recurrence_id.is_some()
                && x.uid.eq_ignore_ascii_case(&a.uid)
        })
        .map(|x| x.id)
        .collect()
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(f) = opt_u64(p, "folder").map(FolderId) {
        check_writable(s, f)?;
    }
    pim::run_copy(s, p, Kind::Appointment)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(f) = opt_u64(p, "folder").map(FolderId) {
        check_writable(s, f)?;
    }
    let ids = pim::targets(s, p, Kind::Appointment);
    check_items_writable(s, &ids)?;
    pim::run_move(s, p, Kind::Appointment)
}
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::targets(s, p, Kind::Appointment);
    check_items_writable(s, &ids)?;
    pim::run_categorize(s, p, Kind::Appointment)
}

/// Refuses changes to the items of a read-only calendar (an internet subscription, or a calendar marked
/// read-only in Calendar Properties).
fn check_writable(s: &Session, folder: FolderId) -> Result<(), CmdError> {
    if props::is_read_only(&s.mailbox, folder) {
        let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
        let why = if props::read(&s.mailbox, folder).source
            == mailcraft_model::calendar::CalendarSource::Internet
        {
            "is a subscribed internet calendar"
        } else {
            "is read-only (Calendar Properties)"
        };
        return Err(CmdError::Disabled(format!("\"{name}\" {why}; its items cannot be changed")));
    }
    Ok(())
}

fn check_items_writable(s: &Session, ids: &[ItemId]) -> Result<(), CmdError> {
    for id in ids {
        if let Some(a) = s.mailbox.appointments.get(id) {
            check_writable(s, a.folder)?;
        }
    }
    Ok(())
}

/// Refuses what a scheduling message would change on a read-only calendar: the user's copy of the meeting
/// `uid` (with its exceptions, and the one for occurrence `rid`) and, when there is no copy yet and `placing`,
/// the calendar a new copy from `mail` would go to.
fn check_meeting_writable(
    s: &Session,
    mail: ItemId,
    uid: &str,
    rid: Option<DateTime>,
    placing: bool,
) -> Result<(), CmdError> {
    let existing = find_by_uid(s, uid);
    let mut ids: Vec<ItemId> = Vec::new();
    if let Some(id) = existing {
        ids.push(id);
        if let Some(a) = s.mailbox.appointments.get(&id) {
            ids.extend(exceptions_of(s, a));
        }
    }
    if let Some(x) = rid.and_then(|rid| find_instance(s, uid, rid)) {
        ids.push(x);
    }
    check_items_writable(s, &ids)?;
    if existing.is_none() && placing {
        check_writable(s, meeting_folder(s, mail)?)?;
    }
    Ok(())
}

/// Folder for a new appointment: `params.folder` (a writable calendar), else the calendar in use, else the
/// default calendar when the one in use is read-only.
fn new_appointment_folder(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    let folder = pim::new_item_folder(s, p, Kind::Appointment)?;
    if opt_u64(p, "folder").is_none()
        && props::is_read_only(&s.mailbox, folder)
        && let Some(d) = pim::default_folder(s, Kind::Appointment)
    {
        check_writable(s, d)?;
        return Ok(d);
    }
    check_writable(s, folder)?;
    Ok(folder)
}

/// Day/week view drag (`calendar.moveItem`): the appointment starts at `start`, keeping its length.
fn run_move_item(s: &mut Session, p: &Value) -> CmdResult {
    drag_item(s, p, false)
}

/// Day/week view edge drag (`calendar.resize`): a new `start` and/or `end`.
fn run_resize(s: &mut Session, p: &Value) -> CmdResult {
    drag_item(s, p, true)
}

fn drag_item(s: &mut Session, p: &Value, resize: bool) -> CmdResult {
    let id = pim::require_targets(s, p, Kind::Appointment)?
        .into_iter()
        .next()
        .ok_or_else(|| CmdError::Params("no appointment".into()))?;
    let a =
        s.mailbox.appointments.get(&id).cloned().ok_or_else(|| CmdError::Params("no appointment".into()))?;
    check_writable(s, a.folder)?;
    let dest = match opt_u64(p, "folder").map(FolderId) {
        Some(f) if f != a.folder => {
            if !pim::is_kind_folder(s, f, Kind::Appointment) {
                return Err(CmdError::Params(format!("folder {} is not a calendar", f.0)));
            }
            check_writable(s, f)?;
            Some(f)
        }
        _ => None,
    };
    // One occurrence of a series becomes an exception; the series itself is edited in its window.
    let (target, series) = if a.recurrence.is_some() {
        if p.get("occurrence").is_none() {
            return Err(CmdError::Params(
                "pass `occurrence` to move one occurrence of a series (open the series to move them all)"
                    .into(),
            ));
        }
        if dest.is_some() {
            return Err(CmdError::Params("an occurrence stays in its series' calendar".into()));
        }
        let (start, _) = occurrence_param(s, &a, p)?;
        let ex = split_occurrence(s, id, start).ok_or_else(|| {
            CmdError::Failed("the occurrence could not be separated from its series".into())
        })?;
        (ex, Some(id))
    } else {
        (id, None)
    };
    let mut x = s
        .mailbox
        .appointments
        .get(&target)
        .cloned()
        .ok_or_else(|| CmdError::Params("no appointment".into()))?;
    // Wall-clock input is read on the calendar's clock (the views pass unix times).
    let cal = tz::Clock::of(s);
    let zone = if x.all_day { None } else { x.zone().map(str::to_owned) };
    let start = cal.date_param(p, "start")?.flatten();
    let end = cal.date_param(p, "end")?.flatten();
    let len = x.end.unix.saturating_sub(x.start.unix).max(0);
    let (mut new_start, mut new_end) = if resize {
        if start.is_none() && end.is_none() {
            return Err(CmdError::Params("`start` or `end` is required".into()));
        }
        (start.unwrap_or(x.start), end.unwrap_or(x.end))
    } else {
        let st = start.ok_or_else(|| CmdError::Params("`start` is required".into()))?;
        if x.all_day {
            // An all-day item moves by whole days, keeping its number of days.
            let days = cal.day(x.end).saturating_sub(cal.day(x.start)).max(1);
            let first = cal.day(st);
            (cal.midnight(st), cal.from_local(first.saturating_add(days).saturating_mul(pim::DAY_SECS)))
        } else {
            (st, pim::add_secs(st, len))
        }
    };
    if x.all_day {
        (new_start, new_end) = whole_days(&cal, new_start, new_end);
    }
    if new_end.unix <= new_start.unix && (resize || len > 0) {
        return Err(CmdError::Params("the end must be after the start".into()));
    }
    if new_end.unix.saturating_sub(new_start.unix) > MAX_SPAN_SECS {
        return Err(CmdError::Params("the appointment is too long".into()));
    }
    if let Some(z) = &zone {
        new_start = tz::in_zone(z, new_start).unwrap_or(new_start);
        new_end = tz::in_zone(z, new_end).unwrap_or(new_end);
    }
    x.start = new_start;
    x.end = new_end;
    if let Some(f) = dest {
        x.folder = f;
        for ex in exceptions_of(s, &a) {
            if let Some(e) = s.mailbox.appointments.get_mut(&ex) {
                e.folder = f;
            }
        }
    }
    let meeting = x.is_meeting && !x.attendees.is_empty();
    s.mailbox.appointments.insert(target, x);
    let mut out = s
        .mailbox
        .appointments
        .get(&target)
        .map_or_else(|| json!({ "id": target.0 }), |x| appointment_json_on(x, &cal));
    if let Some(o) = out.as_object_mut() {
        if let Some(sid) = series {
            o.insert("series".into(), json!(sid.0));
        }
        // The organizer is asked whether to send the attendees an update (`calendar.sendUpdate`).
        o.insert("sendUpdate".into(), json!(meeting));
    }
    Ok(out)
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

/// A VCALENDAR object holding `appts` (`method` e.g. "REQUEST", "PUBLISH"), written by `mailcraft-formats`
/// (recurrence exceptions as EXDATE, exceptions with RECURRENCE-ID, reminders as VALARM).
pub fn ics_calendar(method: Option<&str>, appts: &[&Appointment], stamp: DateTime) -> String {
    let method = method.map(Method::parse);
    mailcraft_formats::ics::write(appts, &[], method.as_ref(), stamp)
}

// ---------------------------------------------------------------------------------------------------------
// Occurrences and recurrence
// ---------------------------------------------------------------------------------------------------------

/// Occurrences (start, end) of `a` overlapping `[from, to)`, expanded by `mailcraft-formats` (RRULE with
/// EXDATEs, in the appointment's own UTC offset). At most `max` (capped by the formats limit).
pub fn occurrences(a: &Appointment, from: DateTime, to: DateTime, max: usize) -> Vec<(DateTime, DateTime)> {
    let max = max.min(rrule::MAX_OCCURRENCES);
    // A recurring timed appointment in a named zone repeats that zone's wall-clock time (DST-aware).
    if !a.all_day
        && a.recurrence.is_some()
        && let Some(z) = a.zone().and_then(tz::zone)
    {
        return tz::expand_zoned(a, z, from, to, max);
    }
    mailcraft_formats::ics::expand_appointment(a, from, to, max)
}

/// The Appointment Recurrence dialog's model and its RRULE mapping (RFC 5545 §3.3.10).
pub mod recurrence {
    use serde::{Deserialize, Serialize};

    use super::pim;
    use mailcraft_formats::ics::rrule::{self, Freq, Rrule, Until};
    use mailcraft_model::{DateTime, dates};

    /// Recurrence pattern (left column of the dialog).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Pattern {
        Daily,
        #[default]
        Weekly,
        Monthly,
        Yearly,
    }

    /// Range of recurrence: No end date, End after N occurrences, End by a date.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum RangeEnd {
        #[default]
        NoEnd,
        After,
        By,
    }

    /// Weekday names, Monday = 0 (the formats crate's numbering).
    pub const WEEKDAYS: [&str; 7] =
        ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    /// Ordinals for "The second Tuesday": 1..=4 and -1 = last.
    pub const ORDINALS: [(i8, &str); 5] =
        [(1, "first"), (2, "second"), (3, "third"), (4, "fourth"), (-1, "last")];
    /// Longest interval accepted.
    pub const MAX_INTERVAL: u32 = 999;
    /// Most occurrences accepted for "End after".
    pub const MAX_COUNT: u32 = 999;

    /// The dialog's fields.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    pub struct RecurrenceForm {
        pub pattern: Pattern,
        /// Every N days / weeks / months / years.
        pub interval: u32,
        /// Daily: "Every weekday" instead of every N days.
        pub every_weekday: bool,
        /// Weekly: the days (Monday = 0 … Sunday = 6).
        pub weekdays: Vec<u8>,
        /// Monthly / yearly: "The second Tuesday" (true) or "Day 12" (false).
        pub by_weekday: bool,
        /// Day of the month, 1..=31.
        pub day: u8,
        /// 1..=4, or -1 for "last".
        pub ordinal: i8,
        /// Monday = 0.
        pub weekday: u8,
        /// Yearly: month 1..=12.
        pub month: u8,
        pub end: RangeEnd,
        /// End after N occurrences.
        pub count: u32,
        /// End by "YYYY-MM-DD" (local date, inclusive).
        pub until: String,
    }

    impl Default for RecurrenceForm {
        fn default() -> Self {
            RecurrenceForm {
                pattern: Pattern::Weekly,
                interval: 1,
                every_weekday: false,
                weekdays: vec![0],
                by_weekday: false,
                day: 1,
                ordinal: 1,
                weekday: 0,
                month: 1,
                end: RangeEnd::NoEnd,
                count: 10,
                until: String::new(),
            }
        }
    }

    /// (local day number, Monday-based weekday, year, month, day of month) of `t`.
    fn local_parts(t: DateTime, tz: i32) -> (i64, u8, i64, u8, u8) {
        let day = t.day_number(tz);
        let (y, m, d) = dates::civil_from_days(day);
        (day, rrule::weekday(day), y, m, d)
    }

    /// "The fourth/last Friday" ordinal of a date (5th weeks count as last).
    fn ordinal_of(y: i64, m: u8, d: u8) -> i8 {
        let n = (d.saturating_sub(1) / 7).saturating_add(1);
        if d.saturating_add(7) > dates::days_in_month(y, m) { -1 } else { i8::try_from(n).unwrap_or(1) }
    }

    impl RecurrenceForm {
        /// The dialog's defaults for an appointment starting at `start`: weekly on its weekday, ending after
        /// 10 occurrences when switched to "End after", and by three months later when switched to "End by".
        pub fn for_start(start: DateTime, tz: i32) -> RecurrenceForm {
            let (day, wd, y, m, d) = local_parts(start, tz);
            let (ey, em, ed) = dates::civil_from_days(day.saturating_add(91));
            RecurrenceForm {
                pattern: Pattern::Weekly,
                weekdays: vec![wd],
                day: d,
                ordinal: ordinal_of(y, m, d),
                weekday: wd,
                month: m,
                until: format!("{ey:04}-{em:02}-{ed:02}"),
                ..RecurrenceForm::default()
            }
        }

        /// Reads an RRULE back into the dialog (fields the dialog cannot show keep the defaults of
        /// [`RecurrenceForm::for_start`]).
        pub fn from_rrule(text: &str, start: DateTime, tz: i32) -> RecurrenceForm {
            let mut f = RecurrenceForm::for_start(start, tz);
            let Ok(r) = Rrule::parse(text) else { return f };
            f.interval = r.interval.clamp(1, MAX_INTERVAL);
            let weekdays_only = {
                let mut d: Vec<u8> = r.by_day.iter().filter(|(o, _)| *o == 0).map(|(_, w)| *w).collect();
                d.sort_unstable();
                d == [0, 1, 2, 3, 4] && r.by_day.len() == 5
            };
            match r.freq {
                Freq::Daily => {
                    f.pattern = Pattern::Daily;
                    f.every_weekday = weekdays_only;
                }
                Freq::Weekly if weekdays_only && r.interval == 1 => {
                    f.pattern = Pattern::Daily;
                    f.every_weekday = true;
                }
                Freq::Weekly => {
                    f.pattern = Pattern::Weekly;
                    let mut d: Vec<u8> = r.by_day.iter().map(|(_, w)| *w).filter(|w| *w < 7).collect();
                    d.sort_unstable();
                    d.dedup();
                    if !d.is_empty() {
                        f.weekdays = d;
                    }
                }
                Freq::Monthly | Freq::Yearly => {
                    f.pattern = if r.freq == Freq::Monthly { Pattern::Monthly } else { Pattern::Yearly };
                    if let Some(m) = r.by_month.first() {
                        f.month = *m;
                    }
                    if let Some(d) = r.by_month_day.first() {
                        f.by_weekday = false;
                        f.day = u8::try_from(*d).ok().filter(|d| (1..=31).contains(d)).unwrap_or(f.day);
                    } else if let Some((o, w)) = r.by_day.first() {
                        let ord = if *o != 0 { *o } else { r.by_set_pos.first().copied().unwrap_or(1) };
                        f.by_weekday = true;
                        f.weekday = (*w).min(6);
                        f.ordinal = if ord < 0 { -1 } else { i8::try_from(ord.clamp(1, 4)).unwrap_or(1) };
                    }
                }
            }
            if let Some(c) = r.count {
                f.end = RangeEnd::After;
                f.count = c.clamp(1, MAX_COUNT);
            } else if let Some(u) = r.until {
                f.end = RangeEnd::By;
                let day = match u {
                    Until::Date(d) | Until::Local(d) => d.div_euclid(rrule_day()),
                    Until::Utc(u) => {
                        u.saturating_add(i64::from(tz).saturating_mul(60)).div_euclid(rrule_day())
                    }
                };
                let (y, m, d) = dates::civil_from_days(day);
                f.until = format!("{y:04}-{m:02}-{d:02}");
            }
            f
        }

        /// The RRULE value for this form. `all_day` and `tz` decide how "End by" is written (a DATE for
        /// all-day events, else the end of that local day in UTC).
        pub fn to_rrule(&self, all_day: bool, tz: i32) -> Result<String, String> {
            let interval = self.interval.clamp(1, MAX_INTERVAL);
            let mut r = Rrule { interval, ..Rrule::default() };
            match self.pattern {
                Pattern::Daily if self.every_weekday => {
                    r.freq = Freq::Weekly;
                    r.interval = 1;
                    r.by_day = (0..5).map(|d| (0, d)).collect();
                }
                Pattern::Daily => r.freq = Freq::Daily,
                Pattern::Weekly => {
                    r.freq = Freq::Weekly;
                    let mut d: Vec<u8> = self.weekdays.iter().copied().filter(|w| *w < 7).collect();
                    d.sort_unstable();
                    d.dedup();
                    if d.is_empty() {
                        return Err("Choose at least one day of the week.".into());
                    }
                    r.by_day = d.into_iter().map(|w| (0, w)).collect();
                }
                Pattern::Monthly | Pattern::Yearly => {
                    r.freq = if self.pattern == Pattern::Monthly { Freq::Monthly } else { Freq::Yearly };
                    if self.pattern == Pattern::Yearly {
                        if !(1..=12).contains(&self.month) {
                            return Err("Choose a month.".into());
                        }
                        r.by_month = vec![self.month];
                    }
                    if self.by_weekday {
                        if self.weekday > 6 || !(self.ordinal == -1 || (1..=4).contains(&self.ordinal)) {
                            return Err("Choose which weekday of the month.".into());
                        }
                        r.by_day = vec![(i32::from(self.ordinal), self.weekday)];
                    } else {
                        if !(1..=31).contains(&self.day) {
                            return Err("The day of the month must be between 1 and 31.".into());
                        }
                        r.by_month_day = vec![i32::from(self.day)];
                    }
                }
            }
            match self.end {
                RangeEnd::NoEnd => {}
                RangeEnd::After => {
                    if !(1..=MAX_COUNT).contains(&self.count) {
                        return Err(format!("The number of occurrences must be between 1 and {MAX_COUNT}."));
                    }
                    r.count = Some(self.count);
                }
                RangeEnd::By => {
                    let t = pim::parse_local(&self.until, 0).ok_or("Enter the end date as YYYY-MM-DD.")?;
                    let day = t.unix.div_euclid(rrule_day());
                    r.until = Some(if all_day {
                        Until::Date(day.saturating_mul(rrule_day()))
                    } else {
                        let next_local = day.saturating_add(1).saturating_mul(rrule_day());
                        Until::Utc(
                            next_local.saturating_sub(i64::from(tz).saturating_mul(60)).saturating_sub(1),
                        )
                    });
                }
            }
            Ok(r.to_value())
        }
    }

    fn rrule_day() -> i64 {
        86_400
    }

    fn weekday_list(days: &[String]) -> String {
        match days {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }

    fn ordinal_name(o: i32) -> &'static str {
        let o = if o < 0 { -1 } else { o };
        ORDINALS.iter().find(|(n, _)| i32::from(*n) == o).map(|(_, s)| *s).unwrap_or("first")
    }

    /// "every Monday and Wednesday", "every 2 days", "day 12 of every month", "the last Friday of every 3
    /// months", "every October 9" — the pattern phrase of [`describe`].
    pub fn pattern_text(text: &str) -> Option<String> {
        let r = Rrule::parse(text).ok()?;
        let every = |unit: &str, units: &str| {
            if r.interval <= 1 { format!("every {unit}") } else { format!("every {} {units}", r.interval) }
        };
        let names: Vec<String> = r
            .by_day
            .iter()
            .filter_map(|(_, w)| WEEKDAYS.get(usize::from(*w)).map(|s| (*s).to_owned()))
            .collect();
        let months = super::recurrence_month_names();
        Some(match r.freq {
            Freq::Daily => every("day", "days"),
            Freq::Weekly => {
                let mut sorted: Vec<u8> = r.by_day.iter().map(|(_, w)| *w).collect();
                sorted.sort_unstable();
                if sorted == [0, 1, 2, 3, 4] && r.interval <= 1 {
                    "every weekday".to_owned()
                } else if names.is_empty() {
                    every("week", "weeks")
                } else if r.interval <= 1 {
                    format!("every {}", weekday_list(&names))
                } else {
                    format!("every {} weeks on {}", r.interval, weekday_list(&names))
                }
            }
            Freq::Monthly => {
                let period = if r.interval <= 1 {
                    "every month".to_owned()
                } else {
                    format!("every {} months", r.interval)
                };
                if let Some(d) = r.by_month_day.first() {
                    format!("day {d} of {period}")
                } else if let Some((o, w)) = r.by_day.first() {
                    let o = if *o != 0 { *o } else { r.by_set_pos.first().copied().unwrap_or(1) };
                    let day = WEEKDAYS.get(usize::from(*w)).copied().unwrap_or("Monday");
                    format!("the {} {day} of {period}", ordinal_name(o))
                } else {
                    period
                }
            }
            Freq::Yearly => {
                let month = r
                    .by_month
                    .first()
                    .and_then(|m| months.get(usize::from(*m).saturating_sub(1)))
                    .copied()
                    .unwrap_or("");
                if let Some(d) = r.by_month_day.first() {
                    if r.interval <= 1 {
                        format!("every {month} {d}")
                    } else {
                        format!("every {} years on {month} {d}", r.interval)
                    }
                } else if let Some((o, w)) = r.by_day.first() {
                    let o = if *o != 0 { *o } else { r.by_set_pos.first().copied().unwrap_or(1) };
                    let day = WEEKDAYS.get(usize::from(*w)).copied().unwrap_or("Monday");
                    let period = if r.interval <= 1 {
                        "every year".to_owned()
                    } else {
                        format!("every {} years", r.interval)
                    };
                    format!("the {} {day} of {month} {period}", ordinal_name(o))
                } else {
                    every("year", "years")
                }
            }
        })
    }
}

/// Month names (shared with [`recurrence::pattern_text`]).
pub(crate) fn recurrence_month_names() -> [&'static str; 12] {
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
}

/// "Occurs every Monday effective 10/12/2026 until 12/14/2026 from 9:00 AM to 9:30 AM." for a recurring
/// appointment; `None` when it does not recur.
pub fn describe_recurrence(a: &Appointment, tz: i32) -> Option<String> {
    let rec = a.recurrence.as_ref()?;
    let pattern = recurrence::pattern_text(&rec.rrule)?;
    let rule = rrule::Rrule::parse(&rec.rrule).ok()?;
    let short = |t: DateTime| {
        let c = t.civil(tz);
        format!("{}/{}/{}", c.month, c.day, c.year)
    };
    let mut text = format!("Occurs {pattern} effective {}", short(a.start));
    if let Some(c) = rule.count {
        text.push_str(&format!(" for {c} occurrence{}", if c == 1 { "" } else { "s" }));
    } else if let Some(u) = rule.until {
        let unix = match u {
            rrule::Until::Utc(u) => u,
            rrule::Until::Date(d) | rrule::Until::Local(d) => {
                d.saturating_sub(i64::from(tz).saturating_mul(60))
            }
        };
        text.push_str(&format!(" until {}", short(DateTime::from_unix(unix, tz))));
    }
    if !a.all_day {
        text.push_str(&format!(" from {} to {}", ListTime::clock(a.start, tz), ListTime::clock(a.end, tz)));
    }
    text.push('.');
    Some(text)
}

/// Parses an `occurrence` param (local "YYYY-MM-DDTHH:MM" or unix seconds) and checks that the series has an
/// occurrence starting then. Returns (start, end).
fn occurrence_param(s: &Session, a: &Appointment, p: &Value) -> Result<(DateTime, DateTime), CmdError> {
    if a.recurrence.is_none() {
        return Err(CmdError::Params("the appointment does not recur".into()));
    }
    let t = match tz::Clock::of(s).date_param(p, "occurrence")? {
        Some(Some(t)) => t,
        _ => return Err(CmdError::Params("`occurrence` (the occurrence's start) is required".into())),
    };
    // Long events of a frequent series overlap: look at every occurrence running then.
    occurrences(a, t, pim::add_secs(t, 1), rrule::MAX_OCCURRENCES)
        .into_iter()
        .find(|(st, _)| st.unix == t.unix)
        .ok_or_else(|| CmdError::Params("the series has no occurrence starting then".into()))
}

fn series(s: &Session, p: &Value) -> Result<Appointment, CmdError> {
    let id = pim::require_targets(s, p, Kind::Appointment)?
        .into_iter()
        .next()
        .ok_or_else(|| CmdError::Params("no appointment".into()))?;
    s.mailbox.appointments.get(&id).cloned().ok_or_else(|| CmdError::Params("no appointment".into()))
}

fn run_occurrences(s: &mut Session, p: &Value) -> CmdResult {
    let a = series(s, p)?;
    let cal = tz::Clock::of(s);
    let tz = cal.offset_at(a.start.unix);
    let from = cal.date_param(p, "from")?.flatten().unwrap_or(a.start);
    let to = cal.date_param(p, "to")?.flatten().unwrap_or_else(|| pim::add_secs(from, 366 * pim::DAY_SECS));
    if to.unix < from.unix {
        return Err(CmdError::Params("`to` is before `from`".into()));
    }
    let max = opt_u64(p, "max").map_or(50, |m| usize::try_from(m).unwrap_or(50)).clamp(1, 1000);
    let list: Vec<Value> = occurrences(&a, from, to, max)
        .into_iter()
        .map(|(st, en)| json!({ "start": cal.datetime_string(st), "end": cal.datetime_string(en) }))
        .collect();
    Ok(json!({ "id": a.id.0, "occurrences": list, "text": describe_recurrence(&a, tz) }))
}

/// Edits one occurrence: the series gets an exception date and the occurrence becomes its own appointment.
fn run_edit_occurrence(s: &mut Session, p: &Value) -> CmdResult {
    let mut a = series(s, p)?;
    check_writable(s, a.folder)?;
    if p.get("recurrence").is_some() {
        return Err(CmdError::Params("an occurrence cannot have its own recurrence; edit the series".into()));
    }
    let (start, end) = occurrence_param(s, &a, p)?;
    let mut ex = a.clone();
    ex.recurrence = None;
    ex.start = start;
    ex.end = end;
    apply_appointment_fields(s, &mut ex, p)?;
    if let Some(r) = a.recurrence.as_mut() {
        if r.exdates.len() >= pim::MAX_LIST {
            return Err(CmdError::Failed("the series has too many exceptions".into()));
        }
        r.exdates.push(start);
    }
    let id = ItemId(s.mailbox.alloc_id());
    ex.id = id;
    // An exception keeps the series' UID and names the occurrence it replaces (RFC 5545 RECURRENCE-ID).
    if ex.uid.trim().is_empty() {
        ex.uid = new_uid(s, a.id, s.now().unix);
        a.uid = ex.uid.clone();
    }
    ex.recurrence_id = Some(start);
    let series_id = a.id;
    s.mailbox.appointments.insert(series_id, a);
    s.mailbox.appointments.insert(id, ex);
    let mut out =
        s.mailbox.appointments.get(&id).map_or_else(|| json!({ "id": id.0 }), |x| appointment_summary(s, x));
    if let Some(o) = out.as_object_mut() {
        o.insert("series".into(), json!(series_id.0));
    }
    Ok(out)
}

/// Makes the occurrence of series `series` that starts at `rid` an exception (a copy with RECURRENCE-ID, the
/// series' UID and an EXDATE on the series). `None` when the series has no occurrence then.
fn split_occurrence(s: &mut Session, series: ItemId, rid: DateTime) -> Option<ItemId> {
    let a = s.mailbox.appointments.get(&series)?.clone();
    a.recurrence.as_ref()?;
    let (start, end) = occurrences(&a, rid, pim::add_secs(rid, 1), rrule::MAX_OCCURRENCES)
        .into_iter()
        .find(|(st, _)| st.unix == rid.unix)?;
    let id = ItemId(s.mailbox.alloc_id());
    let mut ex = a.clone();
    ex.id = id;
    ex.recurrence = None;
    ex.recurrence_id = Some(start);
    ex.start = start;
    ex.end = end;
    if let Some(r) = s.mailbox.appointments.get_mut(&series).and_then(|x| x.recurrence.as_mut()) {
        if r.exdates.len() >= pim::MAX_LIST {
            return None;
        }
        r.exdates.push(start);
    }
    s.mailbox.appointments.insert(id, ex);
    Some(id)
}

/// Deletes one occurrence (adds an exception date to the series).
fn run_delete_occurrence(s: &mut Session, p: &Value) -> CmdResult {
    let mut a = series(s, p)?;
    check_writable(s, a.folder)?;
    let (start, _) = occurrence_param(s, &a, p)?;
    if let Some(r) = a.recurrence.as_mut() {
        if r.exdates.len() >= pim::MAX_LIST {
            return Err(CmdError::Failed("the series has too many exceptions".into()));
        }
        r.exdates.push(start);
    }
    let id = a.id;
    s.mailbox.appointments.insert(id, a);
    Ok(json!({ "series": id.0, "deleted": tz::Clock::of(s).datetime_string(start) }))
}

// ---------------------------------------------------------------------------------------------------------
// Free/busy (Scheduling Assistant, conflicts)
// ---------------------------------------------------------------------------------------------------------

/// A busy block from the user's own calendars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusySlot {
    pub id: u64,
    pub start: i64,
    pub end: i64,
    /// "busy", "tentative", "outOfOffice", "workingElsewhere".
    pub show_as: String,
    pub subject: String,
    pub private: bool,
}

/// Most busy blocks returned by one query.
pub const MAX_BUSY: usize = 2_000;

/// Non-free time on the user's calendars overlapping `[from, to)`, sorted by start. `exclude` leaves out one
/// appointment (the one being edited), `exclude_uid` every appointment with that UID (a meeting being
/// answered).
pub fn free_busy(
    s: &Session,
    from: DateTime,
    to: DateTime,
    exclude: Option<ItemId>,
    exclude_uid: Option<&str>,
) -> Vec<BusySlot> {
    let mb = &s.mailbox;
    let mut out = Vec::new();
    for a in mb.appointments.values() {
        if a.show_as == BusyStatus::Free
            || Some(a.id) == exclude
            || exclude_uid.is_some_and(|u| !u.is_empty() && a.uid.eq_ignore_ascii_case(u))
            || !mb.folder(a.folder).is_some_and(|f| f.kind == FolderKind::Calendar)
        {
            continue;
        }
        for (st, en) in occurrences(a, from, to, MAX_BUSY) {
            out.push(BusySlot {
                id: a.id.0,
                start: st.unix,
                end: en.unix,
                show_as: show_as_name(a.show_as).to_owned(),
                subject: a.subject.clone(),
                private: a.private,
            });
            if out.len() >= MAX_BUSY {
                break;
            }
        }
    }
    out.sort_by_key(|b| (b.start, b.end, b.id));
    out.truncate(MAX_BUSY);
    out
}

fn run_free_busy(s: &mut Session, p: &Value) -> CmdResult {
    let cal = tz::Clock::of(s);
    let from = cal.date_param(p, "start")?.flatten().unwrap_or_else(|| cal.midnight(s.now()));
    let to = cal.date_param(p, "end")?.flatten().unwrap_or_else(|| pim::add_secs(from, pim::DAY_SECS));
    if to.unix < from.unix || to.unix.saturating_sub(from.unix) > MAX_SPAN_SECS {
        return Err(CmdError::Params("`end` must follow `start` within ten years".into()));
    }
    let exclude = opt_u64(p, "exclude").map(ItemId);
    let busy = free_busy(s, from, to, exclude, None);
    Ok(json!({ "busy": busy }))
}

// ---------------------------------------------------------------------------------------------------------
// Meeting requests and responses (iTIP, RFC 5546)
// ---------------------------------------------------------------------------------------------------------

/// SEQUENCE for updates and cancellations: minutes since 2020-01-01, so it grows without being stored.
fn update_sequence(now: DateTime) -> u32 {
    u32::try_from(now.unix.saturating_sub(1_577_836_800).max(0) / 60).unwrap_or(u32::MAX)
}

fn is_calendar_type(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    m.starts_with("text/calendar") || m.starts_with("application/ics")
}

fn is_ics_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().ends_with(".ics")
}

/// The iCalendar data of a message: its `text/calendar` part (else an attached `.ics` file). `None` when the
/// message carries none or its source is not available.
pub fn calendar_data(s: &Session, m: &MailItem) -> Option<Vec<u8>> {
    if !m.attachments.iter().any(|a| is_calendar_type(&a.mime_type) || is_ics_name(&a.filename)) {
        return None;
    }
    let raw = match &m.raw {
        Some(r) => r.clone(),
        None => s.store.as_ref().and_then(|st| st.raw_of(m).ok().flatten())?,
    };
    let msg = mailcraft_mime::parse(&raw).ok()?;
    let parts = msg.parts();
    let pick = parts
        .iter()
        .find(|p| is_calendar_type(p.mime_type()))
        .or_else(|| parts.iter().find(|p| p.filename.as_deref().is_some_and(is_ics_name)))?;
    pick.data().map(<[u8]>::to_vec)
}

/// The scheduling message carried by mail item `mail` (METHOD and first VEVENT).
pub fn read_itip(s: &Session, mail: ItemId) -> Option<ItipMessage> {
    let m = s.mailbox.mail.get(&mail)?;
    let data = calendar_data(s, m)?;
    itip::read(&data, ImportOptions { local_offset_min: s.tz_offset_min }).ok()
}

fn response_name(r: ResponseStatus) -> &'static str {
    match r {
        ResponseStatus::None => "none",
        ResponseStatus::Accepted => "accepted",
        ResponseStatus::Tentative => "tentative",
        ResponseStatus::Declined => "declined",
    }
}

fn is_me(s: &Session, addr: &str) -> bool {
    let a = addr.trim().to_ascii_lowercase();
    !a.is_empty()
        && (s.me.contains(&a)
            || s.mailbox.accounts.iter().any(|acc| acc.address.trim().eq_ignore_ascii_case(&a)))
}

/// The user's address for answering `event`: an attendee that is one of the user's addresses, else the
/// account that received `mail`, else the default account.
fn my_address(s: &Session, mail: Option<ItemId>, event: &Appointment) -> Option<Address> {
    if let Some(a) = event.attendees.iter().find(|x| is_me(s, &x.mailbox.addr)) {
        return Some(a.mailbox.clone());
    }
    let account = mail
        .and_then(|m| s.mailbox.mail.get(&m))
        .and_then(|m| s.mailbox.folder_account(m.folder))
        .and_then(|a| s.mailbox.account(a))
        .or_else(|| s.mailbox.default_account())?;
    if account.address.trim().is_empty() {
        return None;
    }
    let name = if account.sender_name.trim().is_empty() { None } else { Some(account.sender_name.trim()) };
    Some(Address::new(name, account.address.trim()))
}

/// The user's calendar copy of the meeting with `uid` (calendar folders only).
pub fn find_by_uid(s: &Session, uid: &str) -> Option<ItemId> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    let mb = &s.mailbox;
    let mut hits: Vec<&Appointment> = mb
        .appointments
        .values()
        .filter(|a| {
            a.uid.trim().eq_ignore_ascii_case(uid)
                && mb.folder(a.folder).is_some_and(|f| f.kind == FolderKind::Calendar)
        })
        .collect();
    // The series (recurring) first, then single meetings, then exceptions; oldest first.
    hits.sort_by_key(|a| (a.recurrence.is_none(), a.recurrence_id.is_some(), a.id));
    hits.first().map(|a| a.id)
}

/// What the reading pane shows for a meeting message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingMessage {
    pub mail: u64,
    /// "REQUEST", "REPLY", "CANCEL", "COUNTER", "PUBLISH", …
    pub method: String,
    pub uid: String,
    pub subject: String,
    pub location: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// "Friday, October 9, 2026 10:00 AM-10:30 AM".
    pub when: String,
    pub organizer: String,
    /// "Occurs every Monday …" for a recurring meeting.
    pub recurrence: Option<String>,
    /// The meeting in the user's calendar.
    pub appointment: Option<u64>,
    /// The user's response recorded on that copy ("accepted", "tentative", "declined", "none").
    pub my_response: Option<String>,
    pub i_am_organizer: bool,
    /// REPLY / COUNTER: (attendee, response) as sent.
    pub replies: Vec<(String, String)>,
    /// Other appointments on the user's calendar at that time.
    pub conflicts: Vec<String>,
    /// Accept / Tentative / Decline / Propose New Time apply.
    pub can_respond: bool,
    /// The info-bar line.
    pub info: String,
}

/// Summary of the scheduling message `msg` (carried by `mail`) against the user's calendar.
pub fn meeting_message_from(s: &Session, mail: ItemId, msg: &ItipMessage) -> MeetingMessage {
    let tz = s.tz_offset_min;
    let ev = &msg.event.appointment;
    // For one occurrence: its exception, else the series while it still has that occurrence.
    let mine = match msg.event.recurrence_id {
        Some(rid) => find_instance(s, &ev.uid, rid).or_else(|| {
            find_by_uid(s, &ev.uid).filter(|id| {
                s.mailbox.appointments.get(id).is_some_and(|a| {
                    a.recurrence.as_ref().is_some_and(|r| !r.exdates.iter().any(|x| x.unix == rid.unix))
                })
            })
        }),
        None => find_by_uid(s, &ev.uid),
    }
    .and_then(|id| s.mailbox.appointments.get(&id));
    let me = my_address(s, Some(mail), ev);
    let my_response = mine.map(|a| {
        let r = me
            .as_ref()
            .and_then(|m| a.attendees.iter().find(|x| x.mailbox.same_address(m)))
            .map_or(ResponseStatus::None, |x| x.response);
        response_name(r).to_owned()
    });
    let i_am_organizer = ev.organizer.as_ref().is_some_and(|o| is_me(s, &o.addr));
    let replies: Vec<(String, String)> = match msg.method {
        Method::Reply | Method::Counter => ev
            .attendees
            .iter()
            .map(|x| (x.mailbox.display().to_owned(), response_name(x.response).to_owned()))
            .collect(),
        _ => Vec::new(),
    };
    let conflicts: Vec<String> = if msg.method == Method::Request {
        free_busy(s, ev.start, ev.end.max(pim::add_secs(ev.start, 1)), None, Some(&ev.uid))
            .into_iter()
            .map(|b| if b.subject.trim().is_empty() { "(No subject)".to_owned() } else { b.subject })
            .collect()
    } else {
        Vec::new()
    };
    let who = |name: &str| if name.trim().is_empty() { "An attendee".to_owned() } else { name.to_owned() };
    let info = match &msg.method {
        Method::Request | Method::Add => match my_response.as_deref() {
            Some("accepted") => "You accepted this meeting.".to_owned(),
            Some("tentative") => "You tentatively accepted this meeting.".to_owned(),
            Some("declined") => "You declined this meeting.".to_owned(),
            _ if !conflicts.is_empty() => {
                format!("Conflicts with another appointment on your Calendar: {}.", conflicts.join("; "))
            }
            Some(_) => "Please respond. This meeting is already on your Calendar.".to_owned(),
            None => "Please respond.".to_owned(),
        },
        Method::Reply => match replies.first() {
            Some((name, r)) => match r.as_str() {
                "accepted" => format!("{} has accepted this meeting.", who(name)),
                "tentative" => format!("{} has tentatively accepted this meeting.", who(name)),
                "declined" => format!("{} has declined this meeting.", who(name)),
                _ => format!("{} has not responded yet.", who(name)),
            },
            None => "Meeting response.".to_owned(),
        },
        Method::Cancel => {
            if mine.is_some() {
                "This meeting has been canceled. Remove it from your Calendar.".to_owned()
            } else {
                "This meeting has been canceled and is not on your Calendar.".to_owned()
            }
        }
        Method::Counter => {
            let name = replies.first().map(|(n, _)| who(n)).unwrap_or_else(|| who(""));
            format!("{name} proposed a new time: {}.", when_text(ev, tz))
        }
        Method::Publish => "This is a calendar item. Add it to your Calendar.".to_owned(),
        other => format!("Calendar message ({}).", other.as_str()),
    };
    MeetingMessage {
        mail: mail.0,
        method: msg.method.as_str().to_owned(),
        uid: ev.uid.clone(),
        subject: ev.subject.clone(),
        location: ev.location.clone(),
        start: ev.start.unix,
        end: ev.end.unix,
        all_day: ev.all_day,
        when: when_text(ev, tz),
        organizer: ev.organizer.as_ref().map(Address::to_display_string).unwrap_or_default(),
        recurrence: describe_recurrence(ev, tz),
        appointment: mine.map(|a| a.id.0),
        my_response,
        i_am_organizer,
        replies,
        conflicts,
        can_respond: matches!(msg.method, Method::Request | Method::Add) && !i_am_organizer,
        info,
    }
}

/// [`meeting_message_from`] for mail item `mail` (reads and parses its calendar part).
pub fn meeting_message(s: &Session, mail: ItemId) -> Option<MeetingMessage> {
    let msg = read_itip(s, mail)?;
    Some(meeting_message_from(s, mail, &msg))
}

fn req_meeting_mail(s: &Session, p: &Value) -> Result<(ItemId, ItipMessage), CmdError> {
    let id = opt_u64(p, "id")
        .map(ItemId)
        .or(s.selection.focus)
        .ok_or_else(|| CmdError::Params("`id` (the meeting message) is required".into()))?;
    if !s.mailbox.mail.contains_key(&id) {
        return Err(CmdError::Params(format!("no message {}", id.0)));
    }
    let msg =
        read_itip(s, id).ok_or_else(|| CmdError::Params("the message has no meeting information".into()))?;
    Ok((id, msg))
}

fn run_meeting_info(s: &mut Session, p: &Value) -> CmdResult {
    let (id, msg) = req_meeting_mail(s, p)?;
    serde_json::to_value(meeting_message_from(s, id, &msg)).map_err(|e| CmdError::Failed(e.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendMode {
    Now,
    Edit,
    None,
}

fn send_mode(p: &Value, default: SendMode) -> Result<SendMode, CmdError> {
    match opt_str(p, "send") {
        None => Ok(default),
        Some("now") => Ok(SendMode::Now),
        Some("edit") => Ok(SendMode::Edit),
        Some("none") => Ok(SendMode::None),
        Some(_) => Err(CmdError::Params("`send` must be now|edit|none".into())),
    }
}

/// Opens a compose with a `text/calendar` part; sends it straight away for [`SendMode::Now`]. Returns
/// (compose id, sent).
fn itip_compose(
    s: &mut Session,
    to: String,
    subject: String,
    body: &str,
    method: &str,
    ics: String,
    mode: SendMode,
) -> Result<(Option<u64>, bool), CmdError> {
    if mode == SendMode::None {
        return Ok((None, false));
    }
    let compose = ComposeState {
        to,
        subject,
        body: Doc::from_plain_text(body),
        attachments: vec![ComposeAttachment {
            filename: if method == "CANCEL" { "cancel.ics".into() } else { "invite.ics".into() },
            mime_type: format!("text/calendar; method={method}"),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    let cid = s.open_compose(compose);
    if mode == SendMode::Now {
        let r = s.run("compose.send", &json!({ "compose": cid }))?;
        return Ok((Some(cid), r.get("sent").and_then(Value::as_bool).unwrap_or(false)));
    }
    Ok((Some(cid), false))
}

fn parse_response(p: &Value) -> Result<ResponseStatus, CmdError> {
    match opt_str(p, "response") {
        Some("accept" | "accepted") => Ok(ResponseStatus::Accepted),
        Some("tentative") => Ok(ResponseStatus::Tentative),
        Some("decline" | "declined") => Ok(ResponseStatus::Declined),
        _ => Err(CmdError::Params("`response` must be accept|tentative|decline".into())),
    }
}

/// Records `response` as `me`'s answer on `a` (adding `me` as a required attendee when missing).
fn set_my_response(a: &mut Appointment, me: &Address, response: ResponseStatus) {
    let room = a.attendees.len() < pim::MAX_LIST;
    match a.attendees.iter_mut().find(|x| x.mailbox.same_address(me)) {
        Some(x) => x.response = response,
        None if room => {
            a.attendees.push(Attendee { mailbox: me.clone(), role: AttendeeRole::Required, response });
        }
        None => {}
    }
    a.is_meeting = true;
    if response == ResponseStatus::Tentative {
        a.show_as = BusyStatus::Tentative;
    } else if a.show_as == BusyStatus::Tentative {
        a.show_as = BusyStatus::Busy;
    }
}

/// Keeps what the user set on their copy `old` (folder, reminder, categories, private) on the update `a`.
fn keep_local(a: &mut Appointment, old: &Appointment) {
    a.id = old.id;
    a.folder = old.folder;
    a.reminder_min = old.reminder_min;
    a.categories = old.categories.clone();
    a.private = old.private || a.private;
}

/// The calendar a new meeting from `mail` goes to: its account's Calendar, else the default one.
fn meeting_folder(s: &Session, mail: ItemId) -> Result<FolderId, CmdError> {
    let account = s.mailbox.mail.get(&mail).and_then(|m| s.mailbox.folder_account(m.folder));
    account
        .and_then(|acc| s.mailbox.role_folder(acc, FolderRole::Calendar))
        .or_else(|| pim::default_folder(s, Kind::Appointment))
        .ok_or_else(|| CmdError::Failed("there is no calendar folder".into()))
}

/// The user's calendar exception for the occurrence of `uid` that started at `rid` (calendar folders only).
pub fn find_instance(s: &Session, uid: &str, rid: DateTime) -> Option<ItemId> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    let mb = &s.mailbox;
    mb.appointments
        .values()
        .filter(|a| {
            a.recurrence_id.is_some_and(|r| r.unix == rid.unix)
                && a.uid.trim().eq_ignore_ascii_case(uid)
                && mb.folder(a.folder).is_some_and(|f| f.kind == FolderKind::Calendar)
        })
        .map(|a| a.id)
        .min()
}

/// Stores the exception `a` (its `recurrence_id` set) on the user's calendar, replacing an earlier copy of
/// the same instance; `series` (the user's copy of the series, if any) loses that occurrence and lends the
/// exception its folder and reminder. Returns the exception's id.
fn store_instance(
    s: &mut Session,
    mail: ItemId,
    mut a: Appointment,
    rid: DateTime,
    series: Option<ItemId>,
) -> Result<ItemId, CmdError> {
    a.recurrence = None;
    a.recurrence_id = Some(rid);
    let series_copy = series.and_then(|id| s.mailbox.appointments.get(&id)).cloned();
    if let Some(sid) = series
        && let Some(r) = s.mailbox.appointments.get_mut(&sid).and_then(|x| x.recurrence.as_mut())
        && !r.exdates.iter().any(|x| x.unix == rid.unix)
        && r.exdates.len() < pim::MAX_LIST
    {
        r.exdates.push(rid);
    }
    let existing = find_instance(s, &a.uid, rid).and_then(|id| s.mailbox.appointments.get(&id)).cloned();
    let id = match (existing, series_copy) {
        (Some(old), _) => {
            keep_local(&mut a, &old);
            old.id
        }
        (None, Some(series)) => {
            keep_local(&mut a, &series);
            ItemId(s.mailbox.alloc_id())
        }
        (None, None) => {
            a.folder = meeting_folder(s, mail)?;
            a.reminder_min = a.reminder_min.or_else(|| default_reminder_min(s));
            ItemId(s.mailbox.alloc_id())
        }
    };
    a.id = id;
    s.mailbox.appointments.insert(id, a);
    Ok(id)
}

/// Puts the user's answer to `msg` on the calendar: accepted/tentative meetings are added or updated (local
/// reminder and categories kept), a declined one is removed. A message about one occurrence of a series
/// (RECURRENCE-ID) changes only that occurrence; overridden occurrences sent with a series become its
/// exceptions. Returns (calendar item, the copy answered, the address answered from).
fn apply_response(
    s: &mut Session,
    mail: ItemId,
    msg: &ItipMessage,
    response: ResponseStatus,
) -> Result<(Option<ItemId>, Appointment, Address), CmdError> {
    let mut a = msg.event.appointment.clone();
    let me = my_address(s, Some(mail), &a)
        .ok_or_else(|| CmdError::Failed("there is no account address to answer from".into()))?;
    set_my_response(&mut a, &me, response);
    let sender = s.mailbox.mail.get(&mail).and_then(|m| m.from.clone());
    if a.organizer.is_none() {
        a.organizer = sender.clone();
    }
    if a.uid.trim().is_empty() {
        let seed = ItemId(s.mailbox.alloc_id());
        a.uid = new_uid(s, seed, s.now().unix);
    }
    // Nothing changes when the calendar holding the meeting (or receiving it) is read-only.
    check_meeting_writable(s, mail, &a.uid, msg.event.recurrence_id, response != ResponseStatus::Declined)?;
    let existing = find_by_uid(s, &a.uid);
    let series = existing.filter(|id| s.mailbox.appointments.get(id).is_some_and(|x| x.recurrence.is_some()));
    if let Some(rid) = msg.event.recurrence_id {
        // One occurrence: the series stays; this occurrence becomes (or stops being) an exception.
        let item = if response == ResponseStatus::Declined {
            if let Some(sid) = series
                && let Some(r) = s.mailbox.appointments.get_mut(&sid).and_then(|x| x.recurrence.as_mut())
                && !r.exdates.iter().any(|x| x.unix == rid.unix)
                && r.exdates.len() < pim::MAX_LIST
            {
                r.exdates.push(rid);
            }
            if let Some(old) = find_instance(s, &a.uid, rid) {
                pim::delete(s, &[old]);
            }
            None
        } else {
            Some(store_instance(s, mail, a.clone(), rid, series)?)
        };
        mark_read(s, mail);
        return Ok((item, a, me));
    }
    if response == ResponseStatus::Declined {
        if let Some(id) = existing {
            let mut gone = vec![id];
            if let Some(x) = s.mailbox.appointments.get(&id) {
                gone.extend(exceptions_of(s, x));
            }
            pim::delete(s, &gone);
        }
        mark_read(s, mail);
        return Ok((None, a, me));
    }
    let old = existing.and_then(|id| s.mailbox.appointments.get(&id)).cloned();
    let id = match old {
        Some(old) => {
            keep_local(&mut a, &old);
            old.id
        }
        None => {
            a.folder = meeting_folder(s, mail)?;
            a.reminder_min = a.reminder_min.or_else(|| default_reminder_min(s));
            ItemId(s.mailbox.alloc_id())
        }
    };
    a.id = id;
    a.recurrence_id = None;
    s.mailbox.appointments.insert(id, a.clone());
    if a.recurrence.is_some() {
        for o in &msg.overrides {
            let Some(rid) = o.recurrence_id else { continue };
            let mut x = o.appointment.clone();
            set_my_response(&mut x, &me, response);
            if x.organizer.is_none() {
                x.organizer = a.organizer.clone();
            }
            x.uid = a.uid.clone();
            store_instance(s, mail, x, rid, Some(id))?;
        }
    }
    mark_read(s, mail);
    Ok((Some(id), a, me))
}

/// Puts the meeting request `msg` (carried by `mail`) on the user's calendar as Tentative until it is
/// answered (new mail with Options › Mail › Tracking › "Automatically process meeting requests and
/// responses…" on): the copy goes to the account's calendar with the default reminder (Options ›
/// Calendar), the user's response on it stays "none", so Accept / Tentative / Decline still apply and then
/// update it (Decline removes it). Requests for a meeting already on the calendar, about one occurrence,
/// organized by the user or without a UID are left alone, and the message stays unread. Returns the
/// calendar item placed.
pub fn place_tentative(s: &mut Session, mail: ItemId, msg: &ItipMessage) -> Result<Option<ItemId>, CmdError> {
    let uid = msg.event.appointment.uid.trim();
    if !matches!(msg.method, Method::Request | Method::Add)
        || msg.event.recurrence_id.is_some()
        || uid.is_empty()
        || find_by_uid(s, uid).is_some()
        || !meeting_message_from(s, mail, msg).can_respond
    {
        return Ok(None);
    }
    let was_read = s.mailbox.mail.get(&mail).is_some_and(|m| m.read);
    let (item, ..) = apply_response(s, mail, msg, ResponseStatus::None)?;
    if let Some(m) = s.mailbox.mail.get_mut(&mail) {
        m.read = was_read;
    }
    if let Some(id) = item {
        let mut ids = vec![id];
        if let Some(a) = s.mailbox.appointments.get(&id) {
            ids.extend(exceptions_of(s, a));
        }
        for i in ids {
            if let Some(a) = s.mailbox.appointments.get_mut(&i) {
                a.show_as = BusyStatus::Tentative;
            }
        }
    }
    Ok(item)
}

fn mark_read(s: &mut Session, mail: ItemId) {
    if let Some(m) = s.mailbox.mail.get_mut(&mail) {
        m.read = true;
    }
}

fn response_subject(r: ResponseStatus, subject: &str) -> String {
    let prefix = match r {
        ResponseStatus::Accepted => "Accepted",
        ResponseStatus::Tentative => "Tentative",
        ResponseStatus::Declined => "Declined",
        ResponseStatus::None => "Response",
    };
    format!("{prefix}: {subject}")
}

/// `calendar.respond {id, response, send?, comment?}`: Accept / Tentative / Decline a meeting request.
fn run_respond(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, msg) = req_meeting_mail(s, p)?;
    if !matches!(msg.method, Method::Request | Method::Add) {
        return Err(CmdError::Params("only meeting requests can be answered".into()));
    }
    let response = parse_response(p)?;
    let mode = send_mode(p, SendMode::Now)?;
    let comment = pim::text(p, "comment")?.unwrap_or_default();
    let (item, answered, me) = apply_response(s, mail, &msg, response)?;
    let organizer = answered.organizer.clone();
    let mut compose = None;
    let mut sent = false;
    if let Some(org) = organizer.filter(|_| mode != SendMode::None) {
        let ics = itip::reply(&answered, &me, response, msg.event.sequence, s.now());
        let subject = response_subject(response, &answered.subject);
        (compose, sent) = itip_compose(s, org.to_display_string(), subject, &comment, "REPLY", ics, mode)?;
    }
    Ok(json!({
        "appointment": item.map(|i| i.0),
        "response": response_name(response),
        "compose": compose,
        "sent": sent,
    }))
}

/// `calendar.proposeNewTime {id, start, end, response?: tentative|decline, send?, comment?}`: answers the
/// request (Tentative by default) and sends the organizer a COUNTER with the proposed time.
fn run_propose_new_time(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, msg) = req_meeting_mail(s, p)?;
    if !matches!(msg.method, Method::Request | Method::Add) {
        return Err(CmdError::Params("only meeting requests can be answered".into()));
    }
    let response = match opt_str(p, "response") {
        // Options › Calendar › "Use this response when proposing a new meeting time".
        None => match s.settings.text("calendar.proposeResponse").as_str() {
            "accept" => ResponseStatus::Accepted,
            "decline" => ResponseStatus::Declined,
            _ => ResponseStatus::Tentative,
        },
        Some(_) => match parse_response(p)? {
            ResponseStatus::Accepted => {
                return Err(CmdError::Params("propose a new time with tentative or decline".into()));
            }
            r => r,
        },
    };
    let cal = tz::Clock::of(s);
    let start = cal
        .date_param(p, "start")?
        .flatten()
        .ok_or_else(|| CmdError::Params("`start` is required".into()))?;
    let end =
        cal.date_param(p, "end")?.flatten().ok_or_else(|| CmdError::Params("`end` is required".into()))?;
    let tz = cal.offset_at(start.unix);
    if end.unix < start.unix || end.unix.saturating_sub(start.unix) > MAX_SPAN_SECS {
        return Err(CmdError::Params("the proposed end is before the start".into()));
    }
    let mode = send_mode(p, SendMode::Edit)?;
    let comment = pim::text(p, "comment")?.unwrap_or_default();
    let (item, answered, me) = apply_response(s, mail, &msg, response)?;
    let mut proposal = answered.clone();
    proposal.start = DateTime::from_unix(start.unix, answered.start.offset_min);
    proposal.end = DateTime::from_unix(end.unix, answered.start.offset_min);
    proposal.reminder_min = None;
    proposal.attendees.retain(|x| x.mailbox.same_address(&me));
    let mut compose = None;
    let mut sent = false;
    if let Some(org) = answered.organizer.clone().filter(|_| mode != SendMode::None) {
        let ics = mailcraft_formats::ics::write(&[&proposal], &[], Some(&Method::Counter), s.now());
        let body = if comment.trim().is_empty() {
            format!("Proposed new time: {}", when_text(&proposal, tz))
        } else {
            format!("Proposed new time: {}\n\n{comment}", when_text(&proposal, tz))
        };
        let subject = format!("New Time Proposed: {}", answered.subject);
        (compose, sent) = itip_compose(s, org.to_display_string(), subject, &body, "COUNTER", ics, mode)?;
    }
    Ok(json!({
        "appointment": item.map(|i| i.0),
        "response": response_name(response),
        "proposed": { "start": cal.datetime_string(start), "end": cal.datetime_string(end) },
        "compose": compose,
        "sent": sent,
    }))
}

/// `calendar.processReply {id}`: records an attendee's REPLY on the organizer's meeting (tracking).
fn run_process_reply(s: &mut Session, p: &Value) -> CmdResult {
    let (_, msg) = req_meeting_mail(s, p)?;
    if msg.method != Method::Reply {
        return Err(CmdError::Params("the message is not a meeting response".into()));
    }
    // A reply about one occurrence goes on its exception when there is one.
    let uid = &msg.event.appointment.uid;
    let id = msg
        .event
        .recurrence_id
        .and_then(|rid| find_instance(s, uid, rid))
        .or_else(|| find_by_uid(s, uid))
        .ok_or_else(|| CmdError::Params("the meeting is not on your Calendar".into()))?;
    let mut a =
        s.mailbox.appointments.get(&id).cloned().ok_or_else(|| CmdError::Failed("no meeting".into()))?;
    check_writable(s, a.folder)?;
    let n = itip::apply_reply(&mut a, &msg);
    let tracking = tracking_json(&a);
    s.mailbox.appointments.insert(id, a);
    Ok(json!({ "appointment": id.0, "updated": n, "tracking": tracking }))
}

/// `calendar.removeFromCalendar {id}`: applies a CANCEL (the meeting, or the one canceled occurrence).
fn run_remove_canceled(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, msg) = req_meeting_mail(s, p)?;
    if msg.method != Method::Cancel {
        return Err(CmdError::Params("the message is not a meeting cancellation".into()));
    }
    let uid = msg.event.appointment.uid.clone();
    let id =
        find_by_uid(s, &uid).ok_or_else(|| CmdError::Params("the meeting is not on your Calendar".into()))?;
    check_meeting_writable(s, mail, &uid, msg.event.recurrence_id, false)?;
    if let Some(occ) = msg.event.recurrence_id {
        // One occurrence: the series loses it, and an exception for it goes.
        let mut series = false;
        if let Some(r) = s.mailbox.appointments.get_mut(&id).and_then(|a| a.recurrence.as_mut()) {
            series = true;
            if r.exdates.len() < pim::MAX_LIST && !r.exdates.iter().any(|x| x.unix == occ.unix) {
                r.exdates.push(occ);
            }
        }
        let instance = find_instance(s, &uid, occ);
        let removed = instance.map(|x| pim::delete(s, &[x])).unwrap_or_default();
        if series || !removed.is_empty() {
            return Ok(json!({
                "appointment": id.0,
                "occurrence": tz::Clock::of(s).datetime_string(occ),
                "removed": pim::ids_json(&removed),
            }));
        }
    }
    let mut gone = vec![id];
    if let Some(a) = s.mailbox.appointments.get(&id) {
        gone.extend(exceptions_of(s, a));
    }
    let removed = pim::delete(s, &gone);
    Ok(json!({ "removed": pim::ids_json(&removed) }))
}

// ---------------------------------------------------------------------------------------------------------
// Automatic accept or decline (File › Options › Calendar › Auto Accept/Decline)
// ---------------------------------------------------------------------------------------------------------

/// Most messages one automatic pass looks at.
pub const MAX_AUTO_SCAN: usize = 50;

/// True when one of the automatic accept / decline options is on.
pub fn auto_respond_on(s: &Session) -> bool {
    ["calendar.autoAccept", "calendar.declineConflicts", "calendar.declineRecurring"]
        .iter()
        .any(|k| s.settings.bool(k))
}

/// Unread messages in mail folders (not Sent Items, Drafts, Outbox, Deleted Items or Junk) that carry
/// calendar data: what an automatic pass would look at, newest first, at most [`MAX_AUTO_SCAN`]. Empty
/// when the options are off. Cheap (no message is parsed).
pub fn auto_respond_candidates(s: &Session) -> Vec<ItemId> {
    if !auto_respond_on(s) {
        return Vec::new();
    }
    let skip = [
        FolderRole::SentItems,
        FolderRole::Drafts,
        FolderRole::Outbox,
        FolderRole::DeletedItems,
        FolderRole::JunkEmail,
    ];
    let mut hits: Vec<(i64, ItemId)> = s
        .mailbox
        .mail
        .values()
        .filter(|m| !m.read)
        .filter(|m| m.attachments.iter().any(|a| is_calendar_type(&a.mime_type) || is_ics_name(&a.filename)))
        .filter(|m| {
            s.mailbox
                .folder(m.folder)
                .is_some_and(|f| f.kind == FolderKind::Mail && !f.role.is_some_and(|r| skip.contains(&r)))
        })
        .map(|m| (m.received.unix, m.id))
        .collect();
    hits.sort_by_key(|(t, id)| (std::cmp::Reverse(*t), std::cmp::Reverse(*id)));
    hits.into_iter().take(MAX_AUTO_SCAN).map(|(_, id)| id).collect()
}

/// What the automatic options do with a scheduling message (`None` = leave it to the user).
fn auto_decision(s: &Session, mail: ItemId, msg: &ItipMessage) -> Option<&'static str> {
    // A meeting on (or bound for) a read-only calendar is left to the user.
    let placing = matches!(msg.method, Method::Request | Method::Add);
    let uid = msg.event.appointment.uid.trim();
    if !uid.is_empty() && check_meeting_writable(s, mail, uid, msg.event.recurrence_id, placing).is_err() {
        return None;
    }
    let info = meeting_message_from(s, mail, msg);
    match msg.method {
        Method::Request | Method::Add => {
            if !info.can_respond {
                return None;
            }
            // Already answered (accepted, tentative or declined) on the user's calendar.
            if info.my_response.as_deref().is_some_and(|r| r != "none") {
                return None;
            }
            let recurring = msg.event.appointment.recurrence.is_some();
            if (s.settings.bool("calendar.declineRecurring") && recurring)
                || (s.settings.bool("calendar.declineConflicts") && !info.conflicts.is_empty())
            {
                Some("decline")
            } else if s.settings.bool("calendar.autoAccept") {
                Some("accept")
            } else {
                None
            }
        }
        Method::Cancel if s.settings.bool("calendar.autoAccept") && info.appointment.is_some() => {
            Some("remove")
        }
        _ => None,
    }
}

/// `calendar.autoRespond {ids?}`: applies File › Options › Calendar › Automatic accept or decline to meeting
/// messages (`ids`, else [`auto_respond_candidates`]): requests are accepted (or declined when they are
/// recurring / conflict with the calendar and those options are on) and the response is sent; with
/// automatic accept on, canceled meetings are removed from the calendar. Messages already answered, from the
/// user, or not about meetings are left alone. Returns `{"handled": [{"id", "action", ...}]}`.
fn run_auto_respond(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> = match p.get("ids") {
        None | Some(Value::Null) => auto_respond_candidates(s),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_u64).map(ItemId).take(MAX_AUTO_SCAN).collect(),
        Some(_) => return Err(CmdError::Params("`ids` must be a list of message ids".into())),
    };
    let mut handled = Vec::new();
    if !auto_respond_on(s) {
        return Ok(json!({ "handled": handled }));
    }
    for mail in ids {
        if !s.mailbox.mail.contains_key(&mail) {
            continue;
        }
        let Some(msg) = read_itip(s, mail) else { continue };
        let Some(action) = auto_decision(s, mail, &msg) else { continue };
        let out = match action {
            "remove" => {
                let r = run_remove_canceled(s, &json!({ "id": mail.0 }));
                mark_read(s, mail);
                r
            }
            response => run_respond(s, &json!({ "id": mail.0, "response": response, "send": "now" })),
        };
        match out {
            Ok(v) => handled.push(json!({ "id": mail.0, "action": action, "result": v })),
            Err(e) => handled.push(json!({ "id": mail.0, "action": action, "error": e.to_string() })),
        }
    }
    Ok(json!({ "handled": handled }))
}

/// `calendar.acceptProposal {id}`: the organizer takes a COUNTER's time and sends the update.
fn run_accept_proposal(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, msg) = req_meeting_mail(s, p)?;
    if msg.method != Method::Counter {
        return Err(CmdError::Params("the message does not propose a new time".into()));
    }
    let uid = msg.event.appointment.uid.clone();
    let found =
        find_by_uid(s, &uid).ok_or_else(|| CmdError::Params("the meeting is not on your Calendar".into()))?;
    check_meeting_writable(s, mail, &uid, msg.event.recurrence_id, false)?;
    // A proposal for one occurrence moves only that occurrence (its exception, made here if need be).
    let id = match msg.event.recurrence_id {
        Some(rid) => match find_instance(s, &uid, rid) {
            Some(x) => x,
            None => split_occurrence(s, found, rid)
                .ok_or_else(|| CmdError::Params("the series has no occurrence at that time".into()))?,
        },
        None => found,
    };
    let ev = &msg.event.appointment;
    if let Some(a) = s.mailbox.appointments.get_mut(&id) {
        a.start = ev.start;
        a.end = if ev.end.unix < ev.start.unix { ev.start } else { ev.end };
        a.all_day = ev.all_day;
    }
    let compose = meeting_compose_seq(s, id, update_sequence(s.now()));
    Ok(json!({ "appointment": id.0, "compose": compose }))
}

fn tracking_json(a: &Appointment) -> Value {
    Value::Array(
        a.attendees
            .iter()
            .map(|x| {
                json!({
                    "name": x.mailbox.name.clone().unwrap_or_default(),
                    "address": x.mailbox.addr,
                    "role": match x.role {
                        AttendeeRole::Required => "required",
                        AttendeeRole::Optional => "optional",
                        AttendeeRole::Resource => "resource",
                    },
                    "response": response_name(x.response),
                })
            })
            .collect(),
    )
}

/// `calendar.tracking {id}`: attendee responses of a meeting.
fn run_tracking(s: &mut Session, p: &Value) -> CmdResult {
    let a = series(s, p)?;
    let count = |r: ResponseStatus| a.attendees.iter().filter(|x| x.response == r).count();
    Ok(json!({
        "id": a.id.0,
        "organizer": a.organizer.as_ref().map(Address::to_display_string),
        "attendees": tracking_json(&a),
        "accepted": count(ResponseStatus::Accepted),
        "tentative": count(ResponseStatus::Tentative),
        "declined": count(ResponseStatus::Declined),
        "none": count(ResponseStatus::None),
    }))
}

/// `calendar.sendUpdate {id}`: opens the updated invitation for a meeting the user organizes.
fn run_send_update(s: &mut Session, p: &Value) -> CmdResult {
    let a = series(s, p)?;
    if a.attendees.is_empty() {
        return Err(CmdError::Params("the appointment has no attendees".into()));
    }
    let compose = meeting_compose_seq(s, a.id, update_sequence(s.now()))
        .ok_or_else(|| CmdError::Failed("the update could not be created".into()))?;
    Ok(json!({ "id": a.id.0, "compose": compose }))
}

/// `calendar.cancelMeeting {id, send?, comment?}`: the organizer cancels a meeting: a CANCEL goes to the
/// attendees and the meeting leaves the calendar.
fn run_cancel_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let a = series(s, p)?;
    if a.attendees.is_empty() {
        return Err(CmdError::Params("the appointment has no attendees".into()));
    }
    check_writable(s, a.folder)?;
    let mode = send_mode(p, SendMode::Edit)?;
    let comment = pim::text(p, "comment")?.unwrap_or_default();
    let to = a.attendees.iter().map(|x| x.mailbox.to_display_string()).collect::<Vec<_>>().join("; ");
    let ics = itip::cancel(&a, update_sequence(s.now()), s.now());
    let (compose, sent) =
        itip_compose(s, to, format!("Canceled: {}", a.subject), &comment, "CANCEL", ics, mode)?;
    let removed = pim::delete(s, &[a.id]);
    Ok(json!({ "removed": pim::ids_json(&removed), "compose": compose, "sent": sent }))
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
            r#"{"source": "internet|file|blank|addressBook|roomList|shared", "name"?: string, "url"?: string (internet), "refreshMin"?: number, "fetch"?: bool, "data"?: "iCalendar text", "path"?: string (file), "parent"?: number} (addressBook/roomList/shared need Exchange: refused)"#,
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
            r#"{"zone"?: "IANA id", "offsetMin"?: number, "label"?: string, "secondZone"?: "IANA id", "secondLabel"?: string, "showSecond"?: bool, "thirdZone"?: "IANA id", "thirdLabel"?: string, "showThird"?: bool, "swap"?: bool} (no params = open the dialog)"#,
            true,
            run_change_time_zone,
        ),
        spec(
            "calendar.color",
            "Calendar Color",
            "View › Color",
            "",
            r#"{"color": "Automatic|Blue|Green|Orange|Navy Blue|Yellow|Teal|Magenta|Brown|Dark Red", "calendar"?: string, "folder"?: number}"#,
            true,
            run_color,
        ),
        spec(
            "calendar.setProperties",
            "Calendar Properties",
            "Folder › Properties (calendars)",
            "",
            r#"{"folder"?: number, "calendar"?: string, "color"?: "Automatic|Blue|…", "url"?: string (internet calendars), "refreshMin"?: number (internet calendars), "readOnly"?: bool (other calendars), "updateNow"?: bool (internet calendars)}"#,
            true,
            run_set_properties,
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
            r#"{"calendar"?: "<calendar name>", "folder"?: number, "range"?: "all|today|tomorrow|next7|next30|custom", "from"?: "YYYY-MM-DD", "to"?: "YYYY-MM-DD" (custom), "detail"?: "full|limited|availability", "includePrivate"?: bool}"#,
            false,
            run_share,
        ),
        spec(
            "calendar.refreshCalendar",
            "Refresh Internet Calendar",
            "Send / Receive › Update Folder (internet calendars)",
            "",
            r#"{"folder"?: number, "all"?: bool, "due"?: bool, "data"?: "fetched iCalendar text", "etag"?: string, "lastModified"?: string, "notModified"?: bool, "error"?: string}"#,
            true,
            run_refresh_calendar,
        ),
        spec(
            "calendar.moveItem",
            "Move Appointment",
            "Calendar › Day/Week view (drag)",
            "",
            r#"{"id": number, "start": "YYYY-MM-DDTHH:MM" | unix, "occurrence"?: "YYYY-MM-DDTHH:MM" | unix (one occurrence of a series), "folder"?: number}"#,
            true,
            run_move_item,
        ),
        spec(
            "calendar.resize",
            "Change Appointment Length",
            "Calendar › Day/Week view (drag an edge)",
            "",
            r#"{"id": number, "start"?: "YYYY-MM-DDTHH:MM" | unix, "end"?: "YYYY-MM-DDTHH:MM" | unix, "occurrence"?: "YYYY-MM-DDTHH:MM" | unix}"#,
            true,
            run_resize,
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
            "calendar.autoRespond",
            "Automatic Accept or Decline",
            "File › Options › Calendar › Automatic accept or decline",
            "",
            r#"{"ids"?: [number] (meeting messages; default: unread ones)}"#,
            true,
            run_auto_respond,
        ),
        spec(
            "calendar.newAppointment",
            "New Appointment",
            "Calendar › Home › New",
            "",
            r#"{"subject"?, "location"?, "body"?, "start"?: "YYYY-MM-DDTHH:MM", "end"?, "durationMin"?, "timeZone"?: "IANA id", "allDay"?: bool, "showAs"?: "free|workingElsewhere|tentative|busy|outOfOffice", "reminderMin"?: number|null, "private"?: bool, "categories"?: [string], "recurrence"?: "RRULE", "folder"?: number, "form"?: bool, "recurring"?: bool} (no fields or form: true = open the appointment window prefilled; recurring: with the Recurrence dialog)"#,
            true,
            run_new_appointment,
        ),
        spec(
            "calendar.newMeeting",
            "New Meeting",
            "Calendar › Home › New",
            "",
            r#"{"attendees"?: [string], "optional"?: [string], "resources"?: [string], ...appointment fields, "form"?: bool, "recurring"?: bool} (no start = open the meeting window prefilled; nothing is saved until it is sent)"#,
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
        // Recurrence
        spec(
            "calendar.occurrences",
            "Show Occurrences",
            "Calendar › Item › Recurrence",
            "",
            r#"{"id": number, "from"?: "YYYY-MM-DDTHH:MM", "to"?: "YYYY-MM-DDTHH:MM", "max"?: number}"#,
            false,
            run_occurrences,
        ),
        spec(
            "calendar.editOccurrence",
            "Open This Occurrence",
            "Calendar › Item › Recurrence",
            "",
            r#"{"id": number, "occurrence": "YYYY-MM-DDTHH:MM" | unix, ...appointment fields except recurrence}"#,
            true,
            run_edit_occurrence,
        ),
        spec(
            "calendar.deleteOccurrence",
            "Delete Occurrence",
            "Calendar › Item › Delete",
            "",
            r#"{"id": number, "occurrence": "YYYY-MM-DDTHH:MM" | unix}"#,
            true,
            run_delete_occurrence,
        ),
        spec(
            "calendar.freeBusy",
            "Scheduling Assistant",
            "Meeting › Show",
            "",
            r#"{"start"?: "YYYY-MM-DDTHH:MM", "end"?: "YYYY-MM-DDTHH:MM", "exclude"?: number}"#,
            false,
            run_free_busy,
        ),
        // Meetings (iTIP)
        spec(
            "calendar.meetingInfo",
            "Meeting Information",
            "Reading Pane › Meeting",
            "",
            r#"{"id": number (meeting message)}"#,
            false,
            run_meeting_info,
        ),
        spec(
            "calendar.respond",
            "Respond to Meeting",
            "Reading Pane › Meeting › Accept / Tentative / Decline",
            "",
            r#"{"id": number, "response": "accept|tentative|decline", "send"?: "now|edit|none", "comment"?: string}"#,
            true,
            run_respond,
        ),
        spec(
            "calendar.proposeNewTime",
            "Propose New Time",
            "Reading Pane › Meeting › Propose New Time",
            "",
            r#"{"id": number, "start": "YYYY-MM-DDTHH:MM", "end": "YYYY-MM-DDTHH:MM", "response"?: "tentative|decline", "send"?: "now|edit|none", "comment"?: string}"#,
            true,
            run_propose_new_time,
        ),
        spec(
            "calendar.processReply",
            "Record Meeting Response",
            "Reading Pane › Meeting",
            "",
            r#"{"id": number (meeting response message)}"#,
            true,
            run_process_reply,
        ),
        spec(
            "calendar.removeFromCalendar",
            "Remove from Calendar",
            "Reading Pane › Meeting",
            "",
            r#"{"id": number (meeting cancellation message)}"#,
            true,
            run_remove_canceled,
        ),
        spec(
            "calendar.acceptProposal",
            "Accept Proposal",
            "Reading Pane › Meeting",
            "",
            r#"{"id": number (new time proposal message)}"#,
            true,
            run_accept_proposal,
        ),
        spec("calendar.tracking", "Tracking", "Meeting › Show", "", r#"{"id": number}"#, false, run_tracking),
        spec(
            "calendar.sendUpdate",
            "Send Update",
            "Meeting › Actions",
            "",
            r#"{"id": number}"#,
            false,
            run_send_update,
        ),
        spec(
            "calendar.cancelMeeting",
            "Cancel Meeting",
            "Meeting › Actions",
            "",
            r#"{"id": number, "send"?: "now|edit|none", "comment"?: string}"#,
            true,
            run_cancel_meeting,
        ),
        // Reminders (View › Window › Reminders Window)
        spec(
            "calendar.reminders",
            "Reminders",
            "View › Window › Reminders Window",
            "",
            r#"{"state"?: ReminderState}"#,
            false,
            reminders::run_list,
        ),
        spec(
            "calendar.dismissReminder",
            "Dismiss",
            "Reminders Window",
            "",
            r#"{"keys"?: [string], "all"?: bool, "state"?: ReminderState}"#,
            true,
            reminders::run_dismiss,
        ),
        spec(
            "calendar.snoozeReminder",
            "Snooze",
            "Reminders Window",
            "",
            r#"{"keys"?: [string], "all"?: bool, "minutes"?: number, "state"?: ReminderState}"#,
            true,
            reminders::run_snooze,
        ),
        spec(
            "calendar.openReminderItem",
            "Open Item",
            "Reminders Window",
            "",
            r#"{"key": string}"#,
            false,
            reminders::run_open,
        ),
    ]
}

#[cfg(test)]
mod depth_tests;
#[cfg(test)]
mod settings_tests;

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
                &json!({"subject": "Plan", "attendees": ["Ann Lee <ann@example.com>", "bob@example.org"], "optional": "cy@example.net", "location": "Room 4", "start": "2026-10-09T10:30"}),
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
                &json!({"source": "internet", "url": "webcal://cal.example.com/holidays.ics", "fetch": false}),
            )
            .unwrap();
        let f = FolderId(r["folder"].as_u64().unwrap());
        let folder = s.mailbox.folder(f).unwrap();
        assert_eq!(folder.name, "cal.example.com");
        assert_eq!(folder.kind, FolderKind::Calendar);
        assert_eq!(folder.parent, Some(cal(&s)));
        let sub = props::read(&s.mailbox, f).subscription.unwrap();
        assert_eq!(sub.url, "http://cal.example.com/holidays.ics");
        // Other people's, rooms' and shared calendars need Exchange.
        for source in ["shared", "addressBook", "roomList"] {
            let r = s.run(
                "calendar.addCalendar",
                &json!({"source": source, "address": "ann@example.com", "name": "Ann"}),
            );
            assert!(matches!(r, Err(CmdError::Disabled(_))), "{source}");
        }
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
    fn meeting_and_share_messages_get_the_new_message_signature() {
        let mut s = session();
        s.mailbox.add_signature("New", Doc::from_plain_text("-- New sig")).unwrap();
        s.mailbox.add_signature("Re", Doc::from_plain_text("-- Re sig")).unwrap();
        s.mailbox.set_signature_defaults(None, Some(Some("New")), Some(Some("Re"))).unwrap();
        let r = s
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Plan", "attendees": ["ann@example.com"], "form": false}),
            )
            .unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.signature.as_ref().map(|g| g.name.as_str()), Some("New"));
        let text = c.body.to_plain_text();
        let when = text.find("When:").unwrap();
        let sig = text.find("-- New sig").unwrap();
        assert!(when < sig, "the signature follows the meeting details: {text}");
        assert_eq!(text.matches("-- New sig").count(), 1);
        assert_eq!(c.attachments.len(), 1, "the invitation stays attached");

        let r = s.run("calendar.share", &json!({"calendar": "calendar"})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.signature.as_ref().map(|g| g.name.as_str()), Some("New"));
    }

    #[test]
    fn meeting_without_a_default_signature_is_unchanged() {
        let mut s = session();
        s.mailbox.add_signature("Unused", Doc::from_plain_text("-- unused")).unwrap();
        let r = s
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Plan", "attendees": ["ann@example.com"], "form": false}),
            )
            .unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert!(c.signature.is_none());
        assert!(!c.body.to_plain_text().contains("unused"));
    }

    #[test]
    fn ics_escape_escapes_text() {
        assert_eq!(ics_escape("a;b,c\\d\ne"), "a\\;b\\,c\\\\d\\ne");
    }

    // -----------------------------------------------------------------------------------------------------
    // Recurrence
    // -----------------------------------------------------------------------------------------------------

    use recurrence::{Pattern, RangeEnd, RecurrenceForm};

    fn t(text: &str) -> DateTime {
        pim::parse_local(text, 0).unwrap()
    }

    #[test]
    fn recurrence_form_writes_and_reads_rrules() {
        let start = t("2026-10-12T09:00"); // a Monday
        let base = RecurrenceForm::for_start(start, 0);
        assert_eq!(base.pattern, Pattern::Weekly);
        assert_eq!(base.weekdays, vec![0]);
        assert_eq!(base.day, 12);
        assert_eq!(base.ordinal, 2);
        assert_eq!(base.month, 10);
        let cases: Vec<(RecurrenceForm, &str)> = vec![
            (
                RecurrenceForm {
                    weekdays: vec![2, 0],
                    interval: 2,
                    end: RangeEnd::After,
                    count: 5,
                    ..base.clone()
                },
                "FREQ=WEEKLY;INTERVAL=2;COUNT=5;BYDAY=MO,WE",
            ),
            (
                RecurrenceForm { pattern: Pattern::Daily, every_weekday: true, ..base.clone() },
                "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
            ),
            (
                RecurrenceForm { pattern: Pattern::Daily, interval: 3, ..base.clone() },
                "FREQ=DAILY;INTERVAL=3",
            ),
            (RecurrenceForm { pattern: Pattern::Monthly, ..base.clone() }, "FREQ=MONTHLY;BYMONTHDAY=12"),
            (
                RecurrenceForm {
                    pattern: Pattern::Monthly,
                    by_weekday: true,
                    ordinal: -1,
                    weekday: 4,
                    ..base.clone()
                },
                "FREQ=MONTHLY;BYDAY=-1FR",
            ),
            (
                RecurrenceForm { pattern: Pattern::Yearly, ..base.clone() },
                "FREQ=YEARLY;BYMONTHDAY=12;BYMONTH=10",
            ),
            (
                RecurrenceForm {
                    pattern: Pattern::Yearly,
                    by_weekday: true,
                    ordinal: 2,
                    weekday: 1,
                    month: 3,
                    ..base.clone()
                },
                "FREQ=YEARLY;BYDAY=2TU;BYMONTH=3",
            ),
            (
                RecurrenceForm { end: RangeEnd::By, until: "2026-12-14".into(), ..base.clone() },
                "FREQ=WEEKLY;UNTIL=20261214T235959Z;BYDAY=MO",
            ),
        ];
        for (form, want) in cases {
            let rule = form.to_rrule(false, 0).unwrap();
            assert_eq!(rule, want);
            let back = RecurrenceForm::from_rrule(&rule, start, 0);
            assert_eq!(back.to_rrule(false, 0).unwrap(), want, "{back:?}");
        }
        // All-day events end on a DATE; a UTC offset moves the timed UNTIL to the local day's end.
        let by = RecurrenceForm { end: RangeEnd::By, until: "2026-12-14".into(), ..base.clone() };
        assert_eq!(by.to_rrule(true, 0).unwrap(), "FREQ=WEEKLY;UNTIL=20261214;BYDAY=MO");
        assert_eq!(by.to_rrule(false, -300).unwrap(), "FREQ=WEEKLY;UNTIL=20261215T045959Z;BYDAY=MO");
        assert_eq!(
            RecurrenceForm::from_rrule("FREQ=WEEKLY;UNTIL=20261215T045959Z;BYDAY=MO", start, -300).until,
            "2026-12-14"
        );
        // Bad forms are refused.
        assert!(RecurrenceForm { weekdays: vec![], ..base.clone() }.to_rrule(false, 0).is_err());
        assert!(
            RecurrenceForm { pattern: Pattern::Monthly, day: 32, ..base.clone() }.to_rrule(false, 0).is_err()
        );
        assert!(
            RecurrenceForm { end: RangeEnd::After, count: 0, ..base.clone() }.to_rrule(false, 0).is_err()
        );
        assert!(
            RecurrenceForm { end: RangeEnd::By, until: "soon".into(), ..base.clone() }
                .to_rrule(false, 0)
                .is_err()
        );
        // Junk rules fall back to the defaults.
        assert_eq!(RecurrenceForm::from_rrule("FREQ=NEVER", start, 0), base);
    }

    #[test]
    fn recurrence_text() {
        let mut s = session();
        let r = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "1:1", "start": "2026-10-12T09:00", "durationMin": 30,
                        "recurrence": {"pattern": "weekly", "weekdays": [0, 2], "end": "by", "until": "2026-12-14"}}),
            )
            .unwrap();
        assert_eq!(
            r["recurrenceText"],
            "Occurs every Monday and Wednesday effective 10/12/2026 until 12/14/2026 from 9:00 AM to 9:30 AM."
        );
        for (rule, want) in [
            ("FREQ=DAILY;INTERVAL=2", "every 2 days"),
            ("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", "every weekday"),
            ("FREQ=MONTHLY;BYMONTHDAY=12", "day 12 of every month"),
            ("FREQ=MONTHLY;INTERVAL=3;BYDAY=-1FR", "the last Friday of every 3 months"),
            ("FREQ=YEARLY;BYMONTH=10;BYMONTHDAY=9", "every October 9"),
            ("FREQ=YEARLY;BYMONTH=3;BYDAY=2TU", "the second Tuesday of March every year"),
        ] {
            assert_eq!(recurrence::pattern_text(rule).as_deref(), Some(want), "{rule}");
        }
        assert!(recurrence::pattern_text("junk").is_none());
    }

    #[test]
    fn recurrence_param_validates_and_keeps_exceptions() {
        let mut s = session();
        let id = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "S", "start": "2026-10-12T09:00", "recurrence": "FREQ=DAILY;COUNT=5"}),
            )
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        assert!(matches!(
            s.run("calendar.update", &json!({"id": id, "recurrence": "FREQ=HOURLY"})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("calendar.update", &json!({"id": id, "recurrence": {"pattern": "weekly", "weekdays": []}})),
            Err(CmdError::Params(_))
        ));
        s.run("calendar.deleteOccurrence", &json!({"id": id, "occurrence": "2026-10-13T09:00"})).unwrap();
        // Saving the same rule again (the editor does) keeps the deleted occurrence deleted.
        s.run("calendar.update", &json!({"id": id, "subject": "S2", "recurrence": "FREQ=DAILY;COUNT=5"}))
            .unwrap();
        let r = s.run("calendar.occurrences", &json!({"id": id})).unwrap();
        let starts: Vec<&str> =
            r["occurrences"].as_array().unwrap().iter().map(|o| o["start"].as_str().unwrap()).collect();
        assert_eq!(starts, ["2026-10-12T09:00", "2026-10-14T09:00", "2026-10-15T09:00", "2026-10-16T09:00"]);
        // A new rule starts over.
        s.run("calendar.update", &json!({"id": id, "recurrence": "FREQ=DAILY;COUNT=2"})).unwrap();
        assert!(s.mailbox.appointments[&ItemId(id)].recurrence.as_ref().unwrap().exdates.is_empty());
    }

    #[test]
    fn edit_and_delete_one_occurrence() {
        let mut s = session();
        let id = s
            .run("calendar.newAppointment", &json!({"subject": "Standup", "start": "2026-10-12T09:00", "durationMin": 15, "recurrence": "FREQ=WEEKLY;BYDAY=MO,WE,FR"}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let r = s
            .run("calendar.editOccurrence", &json!({"id": id, "occurrence": "2026-10-14T09:00", "start": "2026-10-14T11:00", "location": "Café"}))
            .unwrap();
        assert_eq!(r["series"], id);
        assert_eq!(r["start"], "2026-10-14T11:00");
        assert_eq!(r["end"], "2026-10-14T11:15");
        assert_eq!(r["recurrence"], Value::Null);
        let ex = ItemId(r["id"].as_u64().unwrap());
        assert_eq!(s.mailbox.appointments[&ex].location, "Café");
        let week = json!({"id": id, "from": "2026-10-12", "to": "2026-10-19"});
        let r = s.run("calendar.occurrences", &week).unwrap();
        assert_eq!(r["occurrences"].as_array().unwrap().len(), 2, "{r}");
        s.run("calendar.deleteOccurrence", &json!({"id": id, "occurrence": "2026-10-16T09:00"})).unwrap();
        let r = s.run("calendar.occurrences", &week).unwrap();
        assert_eq!(r["occurrences"].as_array().unwrap().len(), 1);
        // Not an occurrence / not recurring / recurrence on an occurrence.
        for bad in [
            json!({"id": id, "occurrence": "2026-10-13T09:00"}),
            json!({"id": id}),
            json!({"id": ex.0, "occurrence": "2026-10-14T11:00"}),
        ] {
            assert!(matches!(s.run("calendar.deleteOccurrence", &bad), Err(CmdError::Params(_))), "{bad}");
        }
        assert!(matches!(
            s.run(
                "calendar.editOccurrence",
                &json!({"id": id, "occurrence": "2026-10-19T09:00", "recurrence": "FREQ=DAILY"})
            ),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn series_starts_on_its_first_occurrence() {
        let mut s = session();
        // Saved on a Wednesday with an every-Monday pattern: the series starts the next Monday.
        let r = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "1:1", "start": "2026-10-14T09:00", "durationMin": 30,
                        "recurrence": {"pattern": "weekly", "weekdays": [0], "end": "after", "count": 3}}),
            )
            .unwrap();
        assert_eq!(r["start"], "2026-10-19T09:00");
        assert_eq!(r["end"], "2026-10-19T09:30");
        let id = ItemId(r["id"].as_u64().unwrap());
        let got = s.run("calendar.occurrences", &json!({"id": id.0})).unwrap();
        let list: Vec<&str> =
            got["occurrences"].as_array().unwrap().iter().map(|o| o["start"].as_str().unwrap()).collect();
        assert_eq!(list, ["2026-10-19T09:00", "2026-10-26T09:00", "2026-11-02T09:00"]);
        // Day 31 of every month from the 12th: first on the 31st, then months that have one.
        let r = s
            .run("calendar.newAppointment", &json!({"subject": "Close books", "start": "2026-10-12T16:00", "recurrence": "FREQ=MONTHLY;BYMONTHDAY=31"}))
            .unwrap();
        assert_eq!(r["start"], "2026-10-31T16:00");
        let got = s.run("calendar.occurrences", &json!({"id": r["id"], "max": 3})).unwrap();
        let list: Vec<&str> =
            got["occurrences"].as_array().unwrap().iter().map(|o| o["start"].as_str().unwrap()).collect();
        assert_eq!(list, ["2026-10-31T16:00", "2026-12-31T16:00", "2027-01-31T16:00"]);
        // A start already on the pattern stays; a pattern ending before its first day leaves the start alone.
        let r = s
            .run(
                "calendar.newAppointment",
                &json!({"subject": "Fri", "start": "2026-10-16T09:00", "recurrence": "FREQ=WEEKLY;BYDAY=FR"}),
            )
            .unwrap();
        assert_eq!(r["start"], "2026-10-16T09:00");
        let r = s
            .run("calendar.newAppointment", &json!({"subject": "Never", "start": "2026-10-14T09:00", "recurrence": "FREQ=WEEKLY;BYDAY=MO;UNTIL=20261016T000000Z"}))
            .unwrap();
        assert_eq!(r["start"], "2026-10-14T09:00");
    }

    #[test]
    fn long_occurrences_that_overlap_can_be_deleted() {
        let mut s = session();
        let id = s
            .run("calendar.newAppointment", &json!({"subject": "Shift", "start": "2026-10-12T09:00", "durationMin": 6 * 24 * 60, "recurrence": "FREQ=DAILY;COUNT=10"}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        // Five earlier occurrences are still running when the 10/17 one starts.
        s.run("calendar.deleteOccurrence", &json!({"id": id, "occurrence": "2026-10-17T09:00"})).unwrap();
        let r = s
            .run("calendar.occurrences", &json!({"id": id, "from": "2026-10-01", "to": "2026-11-01"}))
            .unwrap();
        assert_eq!(r["occurrences"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn exceptions_keep_the_series_uid_and_go_with_it() {
        let mut s = session();
        let id = s
            .run("calendar.newAppointment", &json!({"subject": "Standup", "start": "2026-10-12T09:00", "durationMin": 15, "recurrence": "FREQ=DAILY;COUNT=5"}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let r = s
            .run(
                "calendar.editOccurrence",
                &json!({"id": id, "occurrence": "2026-10-13T09:00", "start": "2026-10-13T10:00"}),
            )
            .unwrap();
        let ex = ItemId(r["id"].as_u64().unwrap());
        let series = &s.mailbox.appointments[&ItemId(id)];
        let x = &s.mailbox.appointments[&ex];
        assert_eq!(x.uid, series.uid);
        assert_eq!(x.recurrence_id.map(|t| pim::datetime_string(t, 0)).as_deref(), Some("2026-10-13T09:00"));
        s.run("calendar.deleteOccurrence", &json!({"id": id, "occurrence": "2026-10-14T09:00"})).unwrap();
        // Shared as iCalendar, the deleted occurrence stays deleted and the moved one stays moved.
        let r = s.run("calendar.share", &json!({"calendar": "calendar"})).unwrap();
        let data = s.composes[&r["compose"].as_u64().unwrap()].attachments[0].data.clone();
        let cal = mailcraft_formats::ics::read(&data, ImportOptions::default()).unwrap();
        let master = cal.events.iter().find(|e| e.appointment.recurrence.is_some()).unwrap();
        let back = mailcraft_formats::ics::expand_appointment(
            &master.appointment,
            t("2026-10-01T00:00"),
            t("2026-11-01T00:00"),
            20,
        );
        let back: Vec<String> = back.iter().map(|(st, _)| pim::datetime_string(*st, 0)).collect();
        assert_eq!(back, ["2026-10-12T09:00", "2026-10-15T09:00", "2026-10-16T09:00"]);
        let moved = cal.events.iter().find(|e| e.recurrence_id.is_some()).unwrap();
        assert_eq!(pim::datetime_string(moved.appointment.start, 0), "2026-10-13T10:00");
        // Deleting the series deletes its exception too.
        s.run("calendar.delete", &json!({"id": id})).unwrap();
        let cal_folder = role(&s, FolderRole::Calendar);
        assert_ne!(s.mailbox.appointments[&ex].folder, cal_folder);
        // A copy of an exception stands alone.
        let copy = s.run("calendar.copy", &json!({"id": ex.0, "folder": cal_folder.0})).unwrap();
        let _ = copy;
        assert!(
            s.mailbox
                .appointments
                .values()
                .filter(|a| a.folder == cal_folder)
                .all(|a| a.recurrence_id.is_none())
        );
    }

    #[test]
    fn free_busy_lists_own_busy_time() {
        let mut s = session();
        s.run(
            "calendar.newAppointment",
            &json!({"subject": "A", "start": "2026-10-12T09:00", "durationMin": 60}),
        )
        .unwrap();
        s.run(
            "calendar.newAppointment",
            &json!({"subject": "Free", "start": "2026-10-12T10:00", "showAs": "free"}),
        )
        .unwrap();
        s.run("calendar.newAppointment", &json!({"subject": "Daily", "start": "2026-10-01T13:00", "recurrence": "FREQ=DAILY", "showAs": "tentative"})).unwrap();
        let r = s.run("calendar.freeBusy", &json!({"start": "2026-10-12", "end": "2026-10-13"})).unwrap();
        let busy = r["busy"].as_array().unwrap();
        assert_eq!(busy.len(), 2, "{r}");
        assert_eq!(busy[0]["subject"], "A");
        assert_eq!(busy[1]["showAs"], "tentative");
        assert!(matches!(
            s.run("calendar.freeBusy", &json!({"start": "2026-10-13", "end": "2026-10-12"})),
            Err(CmdError::Params(_))
        ));
    }

    // -----------------------------------------------------------------------------------------------------
    // Meeting requests (iTIP)
    // -----------------------------------------------------------------------------------------------------

    /// A session for `addr` (same fixed clock).
    pub(crate) fn person(addr: &str, name: &str) -> Session {
        let mut s = session();
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.address = addr.into();
            a.sender_name = name.into();
        }
        s.refresh_me();
        s
    }

    /// Delivers compose `cid` of `from` into `to`'s Inbox (built like a sent message).
    pub(crate) fn deliver(from: &Session, cid: u64, to: &mut Session) -> ItemId {
        let c = &from.composes[&cid];
        let mut b = mailcraft_mime::MessageBuilder::new();
        let acc = from.mailbox.default_account().unwrap();
        b.from = Some(Address::new(Some(&acc.sender_name), &acc.address));
        b.to = mailcraft_mime::parse_address_list(&c.to);
        b.subject = c.subject.clone();
        b.text = Some(c.body.to_plain_text());
        b.attachments = c
            .attachments
            .iter()
            .map(|a| mailcraft_mime::AttachmentSpec {
                filename: a.filename.clone(),
                mime_type: a.mime_type.clone(),
                data: a.data.clone(),
                content_id: None,
            })
            .collect();
        let raw = b.build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, to.now());
        item.folder = role(to, FolderRole::Inbox);
        to.mailbox.add_mail(item)
    }

    pub(crate) fn invite(org: &mut Session) -> (ItemId, u64) {
        let r = org
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Design review", "start": "2026-10-13T15:00", "durationMin": 60, "location": "Room 4",
                        "attendees": ["Ann Lee <ann@example.com>", "bob@example.org"]}),
            )
            .unwrap();
        (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap())
    }

    #[test]
    fn invitation_is_an_itip_request_from_formats() {
        let mut org = session();
        let (_, cid) = invite(&mut org);
        let att = &org.composes[&cid].attachments[0];
        assert_eq!(att.mime_type, "text/calendar; method=REQUEST");
        let msg = itip::read(&att.data, ImportOptions::default()).unwrap();
        assert_eq!(msg.method, Method::Request);
        assert_eq!(msg.event.appointment.subject, "Design review");
        assert_eq!(msg.event.appointment.attendees.len(), 2);
        assert_eq!(msg.event.appointment.organizer.as_ref().unwrap().addr, "me@example.com");
        let ics = String::from_utf8(att.data.clone()).unwrap().replace("\r\n ", "");
        assert!(ics.contains("RSVP=TRUE"), "{ics}");
    }

    #[test]
    fn accept_adds_to_calendar_and_replies_to_the_organizer() {
        let mut org = session();
        let (meeting, cid) = invite(&mut org);
        let mut ann = person("ann@example.com", "Ann Lee");
        // Ann has something else at that time.
        ann.run("calendar.newAppointment", &json!({"subject": "Dentist", "start": "2026-10-13T15:30"}))
            .unwrap();
        let mail = deliver(&org, cid, &mut ann);
        let info = ann.run("calendar.meetingInfo", &json!({"id": mail.0})).unwrap();
        assert_eq!(info["method"], "REQUEST");
        assert_eq!(info["canRespond"], true);
        assert_eq!(info["appointment"], Value::Null);
        assert_eq!(info["conflicts"], json!(["Dentist"]));
        assert!(info["info"].as_str().unwrap().starts_with("Conflicts with another appointment"), "{info}");
        assert_eq!(info["when"], "Tuesday, October 13, 2026 3:00 PM-4:00 PM");

        let r = ann
            .run(
                "calendar.respond",
                &json!({"id": mail.0, "response": "accept", "send": "edit", "comment": "See you"}),
            )
            .unwrap();
        let appt = ItemId(r["appointment"].as_u64().unwrap());
        let a = &ann.mailbox.appointments[&appt];
        assert_eq!(a.subject, "Design review");
        assert_eq!(a.folder, role(&ann, FolderRole::Calendar));
        assert!(a.is_meeting);
        assert_eq!(a.reminder_min, Some(DEFAULT_REMINDER_MIN));
        let me = a.attendees.iter().find(|x| x.mailbox.addr == "ann@example.com").unwrap();
        assert_eq!(me.response, ResponseStatus::Accepted);
        let info = ann.run("calendar.meetingInfo", &json!({"id": mail.0})).unwrap();
        assert_eq!(info["myResponse"], "accepted");
        assert_eq!(info["info"], "You accepted this meeting.");
        let rc = r["compose"].as_u64().unwrap();
        let c = &ann.composes[&rc];
        assert_eq!(c.subject, "Accepted: Design review");
        assert!(c.to.contains("me@example.com"));
        assert_eq!(c.attachments[0].mime_type, "text/calendar; method=REPLY");

        // The organizer records the response.
        let reply = deliver(&ann, rc, &mut org);
        let info = org.run("calendar.meetingInfo", &json!({"id": reply.0})).unwrap();
        assert_eq!(info["method"], "REPLY");
        assert_eq!(info["info"], "Ann Lee has accepted this meeting.");
        assert_eq!(info["canRespond"], false);
        let r = org.run("calendar.processReply", &json!({"id": reply.0})).unwrap();
        assert_eq!(r["updated"], 1);
        let t = org.run("calendar.tracking", &json!({"id": meeting.0})).unwrap();
        assert_eq!(t["accepted"], 1);
        assert_eq!(t["none"], 1);
        assert_eq!(t["attendees"][0]["response"], "accepted");
        // Answering twice updates the same calendar item; declining removes it.
        let r = ann
            .run("calendar.respond", &json!({"id": mail.0, "response": "tentative", "send": "none"}))
            .unwrap();
        assert_eq!(r["appointment"].as_u64(), Some(appt.0));
        assert_eq!(r["compose"], Value::Null);
        assert_eq!(ann.mailbox.appointments[&appt].show_as, BusyStatus::Tentative);
        ann.run("calendar.respond", &json!({"id": mail.0, "response": "decline", "send": "none"})).unwrap();
        assert_ne!(ann.mailbox.appointments[&appt].folder, role(&ann, FolderRole::Calendar));
        assert!(ann.mailbox.validate().is_empty());
    }

    #[test]
    fn propose_new_time_and_accept_the_proposal() {
        let mut org = session();
        let (meeting, cid) = invite(&mut org);
        let mut bob = person("bob@example.org", "Bob");
        let mail = deliver(&org, cid, &mut bob);
        let r = bob
            .run(
                "calendar.proposeNewTime",
                &json!({"id": mail.0, "start": "2026-10-14T10:00", "end": "2026-10-14T11:00"}),
            )
            .unwrap();
        assert_eq!(r["response"], "tentative");
        let pc = r["compose"].as_u64().unwrap();
        assert_eq!(bob.composes[&pc].subject, "New Time Proposed: Design review");
        assert_eq!(bob.composes[&pc].attachments[0].mime_type, "text/calendar; method=COUNTER");
        let counter = deliver(&bob, pc, &mut org);
        let info = org.run("calendar.meetingInfo", &json!({"id": counter.0})).unwrap();
        assert_eq!(info["method"], "COUNTER");
        assert!(
            info["info"]
                .as_str()
                .unwrap()
                .contains("proposed a new time: Wednesday, October 14, 2026 10:00 AM"),
            "{info}"
        );
        let r = org.run("calendar.acceptProposal", &json!({"id": counter.0})).unwrap();
        assert!(r["compose"].as_u64().is_some());
        let a = &org.mailbox.appointments[&meeting];
        assert_eq!(pim::datetime_string(a.start, 0), "2026-10-14T10:00");
        assert_eq!(pim::datetime_string(a.end, 0), "2026-10-14T11:00");
        assert!(matches!(
            bob.run(
                "calendar.proposeNewTime",
                &json!({"id": mail.0, "start": "2026-10-14T10:00", "end": "2026-10-14T09:00"})
            ),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            bob.run("calendar.proposeNewTime", &json!({"id": mail.0, "start": "2026-10-14T10:00", "end": "2026-10-14T11:00", "response": "accept"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn cancellation_removes_the_meeting() {
        let mut org = session();
        let (meeting, cid) = invite(&mut org);
        let mut ann = person("ann@example.com", "Ann Lee");
        let mail = deliver(&org, cid, &mut ann);
        let appt = ann
            .run("calendar.respond", &json!({"id": mail.0, "response": "accept", "send": "none"}))
            .unwrap()["appointment"]
            .as_u64()
            .unwrap();
        let r = org.run("calendar.cancelMeeting", &json!({"id": meeting.0, "comment": "Sorry"})).unwrap();
        let cc = r["compose"].as_u64().unwrap();
        assert_eq!(org.composes[&cc].subject, "Canceled: Design review");
        assert!(
            org.composes[&cc].to.contains("ann@example.com")
                && org.composes[&cc].to.contains("bob@example.org")
        );
        assert_ne!(org.mailbox.appointments[&meeting].folder, role(&org, FolderRole::Calendar));
        let cancel = deliver(&org, cc, &mut ann);
        let info = ann.run("calendar.meetingInfo", &json!({"id": cancel.0})).unwrap();
        assert_eq!(info["method"], "CANCEL");
        assert_eq!(info["info"], "This meeting has been canceled. Remove it from your Calendar.");
        ann.run("calendar.removeFromCalendar", &json!({"id": cancel.0})).unwrap();
        assert_ne!(ann.mailbox.appointments[&ItemId(appt)].folder, role(&ann, FolderRole::Calendar));
        let info = ann.run("calendar.meetingInfo", &json!({"id": cancel.0})).unwrap();
        assert_eq!(info["info"], "This meeting has been canceled and is not on your Calendar.");
        assert!(matches!(
            ann.run("calendar.removeFromCalendar", &json!({"id": cancel.0})),
            Err(CmdError::Params(_))
        ));
        // A cancellation is not a request.
        assert!(matches!(
            ann.run("calendar.respond", &json!({"id": cancel.0, "response": "accept"})),
            Err(CmdError::Params(_))
        ));
    }

    /// Delivers a scheduling message written by "Org" into `to`'s Inbox.
    fn deliver_ics(to: &mut Session, method: &str, ics: &str) -> ItemId {
        let mut b = mailcraft_mime::MessageBuilder::new();
        b.from = Some(Address::new(Some("Org"), "org@example.com"));
        b.to = mailcraft_mime::parse_address_list("ann@example.com");
        b.subject = "Weekly sync".into();
        b.text = Some("See invite".into());
        b.attachments = vec![mailcraft_mime::AttachmentSpec {
            filename: "invite.ics".into(),
            mime_type: format!("text/calendar; method={method}"),
            data: ics.replace('\n', "\r\n").into_bytes(),
            content_id: None,
        }];
        let raw = b.build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, to.now());
        item.folder = role(to, FolderRole::Inbox);
        to.mailbox.add_mail(item)
    }

    const SERIES: &str = "BEGIN:VEVENT\nUID:sync-1@example.com\nSEQUENCE:0\nDTSTART:20261012T150000Z\n\
DTEND:20261012T153000Z\nSUMMARY:Weekly sync\nRRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=6\n\
ORGANIZER;CN=Org:mailto:org@example.com\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ann@example.com\n\
END:VEVENT\n";

    fn instance(method: &str, rid: &str, start: &str, end: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\nVERSION:2.0\nMETHOD:{method}\nBEGIN:VEVENT\nUID:sync-1@example.com\nSEQUENCE:1\n\
RECURRENCE-ID:{rid}\nDTSTART:{start}\nDTEND:{end}\nSUMMARY:Weekly sync (moved)\n\
ORGANIZER;CN=Org:mailto:org@example.com\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:ann@example.com\n\
END:VEVENT\nEND:VCALENDAR\n"
        )
    }

    fn starts(s: &mut Session, id: ItemId) -> Vec<String> {
        let r = s
            .run("calendar.occurrences", &json!({"id": id.0, "from": "2026-10-01", "to": "2026-12-01"}))
            .unwrap();
        r["occurrences"].as_array().unwrap().iter().map(|o| o["start"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn answering_one_occurrence_keeps_the_series() {
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver_ics(
            &mut ann,
            "REQUEST",
            &format!("BEGIN:VCALENDAR\nVERSION:2.0\nMETHOD:REQUEST\n{SERIES}END:VCALENDAR\n"),
        );
        let series = ItemId(
            ann.run("calendar.respond", &json!({"id": req.0, "response": "accept", "send": "none"})).unwrap()
                ["appointment"]
                .as_u64()
                .unwrap(),
        );
        assert_eq!(starts(&mut ann, series).len(), 6);
        // The organizer moves the 10/19 occurrence to 17:00; Ann accepts the update.
        let upd = deliver_ics(
            &mut ann,
            "REQUEST",
            &instance("REQUEST", "20261019T150000Z", "20261019T170000Z", "20261019T173000Z"),
        );
        let info = ann.run("calendar.meetingInfo", &json!({"id": upd.0})).unwrap();
        assert_eq!(info["appointment"], series.0, "the occurrence is still on the series: {info}");
        let r =
            ann.run("calendar.respond", &json!({"id": upd.0, "response": "accept", "send": "none"})).unwrap();
        let ex = ItemId(r["appointment"].as_u64().unwrap());
        assert_ne!(ex, series);
        let a = &ann.mailbox.appointments[&series];
        assert!(a.recurrence.is_some(), "the series survives");
        assert_eq!(a.subject, "Weekly sync");
        let x = &ann.mailbox.appointments[&ex];
        assert_eq!(x.uid, "sync-1@example.com");
        assert_eq!(x.recurrence_id.map(|t| pim::datetime_string(t, 0)).as_deref(), Some("2026-10-19T15:00"));
        assert_eq!(pim::datetime_string(x.start, 0), "2026-10-19T17:00");
        assert_eq!(x.reminder_min, a.reminder_min);
        assert!(!starts(&mut ann, series).contains(&"2026-10-19T15:00".to_owned()));
        assert_eq!(starts(&mut ann, series).len(), 5);
        // A second move of the same occurrence updates that exception, not a new one.
        let upd2 = deliver_ics(
            &mut ann,
            "REQUEST",
            &instance("REQUEST", "20261019T150000Z", "20261019T180000Z", "20261019T183000Z"),
        );
        let r = ann
            .run("calendar.respond", &json!({"id": upd2.0, "response": "tentative", "send": "none"}))
            .unwrap();
        assert_eq!(r["appointment"].as_u64(), Some(ex.0));
        assert_eq!(pim::datetime_string(ann.mailbox.appointments[&ex].start, 0), "2026-10-19T18:00");
        let copies = ann
            .mailbox
            .appointments
            .values()
            .filter(|x| x.uid == "sync-1@example.com" && x.folder == role(&ann, FolderRole::Calendar))
            .count();
        assert_eq!(copies, 2);
        // Declining another occurrence takes only that one off the calendar.
        let other = deliver_ics(
            &mut ann,
            "REQUEST",
            &instance("REQUEST", "20261026T150000Z", "20261026T150000Z", "20261026T153000Z"),
        );
        let r = ann
            .run("calendar.respond", &json!({"id": other.0, "response": "decline", "send": "none"}))
            .unwrap();
        assert_eq!(r["appointment"], Value::Null);
        assert!(ann.mailbox.appointments[&series].recurrence.is_some());
        assert_eq!(starts(&mut ann, series).len(), 4);
        // Canceling one occurrence removes its exception; canceling the meeting removes everything.
        let c1 = deliver_ics(
            &mut ann,
            "CANCEL",
            &instance("CANCEL", "20261019T150000Z", "20261019T180000Z", "20261019T183000Z"),
        );
        ann.run("calendar.removeFromCalendar", &json!({"id": c1.0})).unwrap();
        assert_ne!(ann.mailbox.appointments[&ex].folder, role(&ann, FolderRole::Calendar));
        assert_eq!(ann.mailbox.appointments[&series].folder, role(&ann, FolderRole::Calendar));
        let info = ann.run("calendar.meetingInfo", &json!({"id": c1.0})).unwrap();
        assert_eq!(info["appointment"], Value::Null, "{info}");
        assert!(ann.mailbox.validate().is_empty());
    }

    #[test]
    fn organizer_updates_one_occurrence_end_to_end() {
        let mut org = session();
        let r = org
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Weekly", "start": "2026-10-12T15:00", "durationMin": 30,
                        "attendees": ["Ann Lee <ann@example.com>"], "recurrence": "FREQ=WEEKLY;BYDAY=MO;COUNT=4"}),
            )
            .unwrap();
        let (series, cid) = (r["id"].as_u64().unwrap(), r["compose"].as_u64().unwrap());
        let mut ann = person("ann@example.com", "Ann Lee");
        let mail = deliver(&org, cid, &mut ann);
        let ann_series = ItemId(
            ann.run("calendar.respond", &json!({"id": mail.0, "response": "accept", "send": "none"}))
                .unwrap()["appointment"]
                .as_u64()
                .unwrap(),
        );
        // The organizer moves one occurrence and sends the update for it.
        let ex = org
            .run(
                "calendar.editOccurrence",
                &json!({"id": series, "occurrence": "2026-10-19T15:00", "start": "2026-10-20T15:00"}),
            )
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let up = org.run("calendar.sendUpdate", &json!({"id": ex})).unwrap()["compose"].as_u64().unwrap();
        let ics =
            String::from_utf8(org.composes[&up].attachments[0].data.clone()).unwrap().replace("\r\n ", "");
        assert!(ics.contains("RECURRENCE-ID:20261019T150000Z"), "{ics}");
        assert!(!ics.contains("RRULE"), "{ics}");
        let upd = deliver(&org, up, &mut ann);
        ann.run("calendar.respond", &json!({"id": upd.0, "response": "accept", "send": "none"})).unwrap();
        assert!(ann.mailbox.appointments[&ann_series].recurrence.is_some(), "Ann keeps the series");
        let got = starts(&mut ann, ann_series);
        assert_eq!(got, ["2026-10-12T15:00", "2026-10-26T15:00", "2026-11-02T15:00"]);
        let moved: Vec<&Appointment> =
            ann.mailbox.appointments.values().filter(|a| a.recurrence_id.is_some()).collect();
        assert_eq!(moved.len(), 1);
        assert_eq!(pim::datetime_string(moved[0].start, 0), "2026-10-20T15:00");
    }

    #[test]
    fn proposal_for_one_occurrence_moves_only_that_occurrence() {
        let mut org = session();
        let r = org
            .run(
                "calendar.newMeeting",
                &json!({"subject": "Weekly", "start": "2026-10-12T15:00", "durationMin": 30,
                        "attendees": ["ann@example.com"], "recurrence": "FREQ=WEEKLY;BYDAY=MO;COUNT=4"}),
            )
            .unwrap();
        let series = ItemId(r["id"].as_u64().unwrap());
        let uid = org.mailbox.appointments[&series].uid.clone();
        let counter = format!(
            "BEGIN:VCALENDAR\nVERSION:2.0\nMETHOD:COUNTER\nBEGIN:VEVENT\nUID:{uid}\nRECURRENCE-ID:20261019T150000Z\n\
DTSTART:20261020T150000Z\nDTEND:20261020T153000Z\nSUMMARY:Weekly\nORGANIZER:mailto:me@example.com\n\
ATTENDEE;PARTSTAT=TENTATIVE:mailto:ann@example.com\nEND:VEVENT\nEND:VCALENDAR\n"
        );
        let mail = deliver_ics(&mut org, "COUNTER", &counter);
        let r = org.run("calendar.acceptProposal", &json!({"id": mail.0})).unwrap();
        let ex = ItemId(r["appointment"].as_u64().unwrap());
        assert_ne!(ex, series);
        let a = &org.mailbox.appointments[&series];
        assert_eq!(pim::datetime_string(a.start, 0), "2026-10-12T15:00", "the series keeps its time");
        assert_eq!(starts(&mut org, series), ["2026-10-12T15:00", "2026-10-26T15:00", "2026-11-02T15:00"]);
        assert_eq!(pim::datetime_string(org.mailbox.appointments[&ex].start, 0), "2026-10-20T15:00");
        let cid = r["compose"].as_u64().unwrap();
        let ics =
            String::from_utf8(org.composes[&cid].attachments[0].data.clone()).unwrap().replace("\r\n ", "");
        assert!(ics.contains("RECURRENCE-ID:20261019T150000Z"), "{ics}");
    }

    #[test]
    fn series_with_overrides_and_whole_cancel() {
        let mut ann = person("ann@example.com", "Ann Lee");
        let moved = "BEGIN:VEVENT\nUID:sync-1@example.com\nRECURRENCE-ID:20261026T150000Z\nDTSTART:20261027T090000Z\n\
DTEND:20261027T093000Z\nSUMMARY:Weekly sync (Tuesday)\nORGANIZER;CN=Org:mailto:org@example.com\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:ann@example.com\nEND:VEVENT\n";
        // The override comes first; the series is still what gets answered.
        let req = deliver_ics(
            &mut ann,
            "REQUEST",
            &format!("BEGIN:VCALENDAR\nVERSION:2.0\nMETHOD:REQUEST\n{moved}{SERIES}END:VCALENDAR\n"),
        );
        let series = ItemId(
            ann.run("calendar.respond", &json!({"id": req.0, "response": "accept", "send": "none"})).unwrap()
                ["appointment"]
                .as_u64()
                .unwrap(),
        );
        assert!(ann.mailbox.appointments[&series].recurrence.is_some());
        let got = starts(&mut ann, series);
        assert_eq!(got.len(), 5);
        assert!(!got.contains(&"2026-10-26T15:00".to_owned()));
        let cal = role(&ann, FolderRole::Calendar);
        let ex: Vec<&Appointment> = ann
            .mailbox
            .appointments
            .values()
            .filter(|x| x.recurrence_id.is_some() && x.folder == cal)
            .collect();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].subject, "Weekly sync (Tuesday)");
        let me = ex[0].attendees.iter().find(|x| x.mailbox.addr == "ann@example.com").unwrap();
        assert_eq!(me.response, ResponseStatus::Accepted);
        let cancel = deliver_ics(
            &mut ann,
            "CANCEL",
            &format!("BEGIN:VCALENDAR\nVERSION:2.0\nMETHOD:CANCEL\n{SERIES}END:VCALENDAR\n"),
        );
        ann.run("calendar.removeFromCalendar", &json!({"id": cancel.0})).unwrap();
        assert!(ann.mailbox.appointments.values().all(|x| x.folder != cal), "series and exception both go");
    }

    #[test]
    fn plain_mail_has_no_meeting() {
        let mut s = session();
        let inbox = role(&s, FolderRole::Inbox);
        let id = s.mailbox.add_mail(MailItem { folder: inbox, subject: "Hi".into(), ..Default::default() });
        assert!(meeting_message(&s, id).is_none());
        assert!(matches!(s.run("calendar.meetingInfo", &json!({"id": id.0})), Err(CmdError::Params(_))));
        assert!(matches!(s.run("calendar.meetingInfo", &json!({"id": 999_999})), Err(CmdError::Params(_))));
        assert!(matches!(
            s.run("calendar.respond", &json!({"id": id.0, "response": "maybe"})),
            Err(CmdError::Params(_))
        ));
        // A meeting without attendees has nothing to cancel or update.
        let a =
            s.run("calendar.newAppointment", &json!({"subject": "Solo"})).unwrap()["id"].as_u64().unwrap();
        assert!(matches!(s.run("calendar.cancelMeeting", &json!({"id": a})), Err(CmdError::Params(_))));
        assert!(matches!(s.run("calendar.sendUpdate", &json!({"id": a})), Err(CmdError::Params(_))));
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
