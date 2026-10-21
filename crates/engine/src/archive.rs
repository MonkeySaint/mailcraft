//! AutoArchive: moving or deleting old items (File › Options › Advanced › AutoArchive Settings, a folder's
//! Properties › AutoArchive, Folder › AutoArchive Settings, File › Info › Tools › Clean Up Old Items).
//!
//! **Which folders.** Every mail, calendar, task and note folder except Outbox, search folders, subscribed
//! (Internet or file) calendars and the archive folders themselves. Each folder archives by its own
//! properties ([`FolderArchive`], kept on the folder): with the default settings, not at all, or with its own
//! age and action (move to a folder of its choice, or delete permanently).
//!
//! **How old.** Items are aged by the date that says when they were last current:
//! - mail: the received date (the sent date when there is none);
//! - appointments: the end; recurring series and their exceptions are never archived;
//! - tasks: completed tasks only, by the completion date (the due date when none was recorded);
//! - notes: the last change; contacts (and journal entries, which MailCraft does not keep) are never
//!   archived.
//!
//! An item is old when that date lies before now minus the age (calendar months in local time, or weeks or
//! days). Mail marked "Do not AutoArchive this item" ([`MailItem::no_auto_archive`]) stays unless a manual
//! archive includes such items.
//!
//! **Where to.** Moved items go to the archive folder — `Archives` at the top of the Local Folders data file,
//! unless Options name another folder (`advanced.autoArchive.folder`) — under a copy of the source path
//! (`Inbox\Projects` → `Archives\Inbox\Projects`; missing folders are created with the source's kind). A
//! folder with its own "Move old items to" folder sends them straight there. "Delete expired items" removes
//! mail whose expiry date (RFC 4021 `Expiry-Date` / `Expires`) has passed, whatever its age.
//!
//! **When.** "Run AutoArchive every N days" makes it due N days after the last run ([`LAST_RUN_KEY`], saved
//! with the profile; the clock starts when the option is first seen on). The desktop app calls [`poll`] when
//! idle and runs [`run_scheduled`] (an automatic action: no undo step) or, with "Prompt before AutoArchive
//! runs", asks first. One run handles at most [`MAX_ITEMS_PER_RUN`] items; the rest stays due and goes on the
//! next check. Running it by hand (`file.archive`) records an undo step like any command.
//!
//! **Server mail.** Moving or deleting mail of a synchronised (IMAP) folder queues a server change. A run
//! stops taking such mail once the account has [`QUEUE_ROOM`] changes waiting, so it never pushes the user's
//! own queued changes out of the bounded queue; the rest waits until send/receive has caught up. Mail
//! downloaded as headers only is not moved out of its server folder (the local copy would have no body to
//! download afterwards); it is archived once downloaded in full.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cmd::util::{opt_bool, opt_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::calendar::CalendarSource;
use mailcraft_model::dates::{civil_from_days, days_from_civil, days_in_month};
use mailcraft_model::folder::{AgeUnit, ArchiveAction, ArchiveMode, FolderArchive, MAX_ARCHIVE_AGE};
use mailcraft_model::mailbox::{LOCAL_ACCOUNT_FALLBACK_NAME, MAX_FOLDER_NAME_CHARS};
use mailcraft_model::{
    Account, AccountId, AccountKind, Appointment, DateTime, Folder, FolderId, FolderKind, FolderRole, ItemId,
    MailItem, Mailbox, Note, Task,
};

/// Name of the archive folder created at the top of Local Folders.
pub const ARCHIVES_NAME: &str = "Archives";
/// Setting holding the last AutoArchive run (unix seconds, 0 = never).
pub const LAST_RUN_KEY: &str = "state.autoArchive.lastRun";
/// Most items one run moves or deletes (the rest waits for the next run).
pub const MAX_ITEMS_PER_RUN: usize = 20_000;
/// A run takes no more mail of a synchronised account once that many server changes wait in its queue.
pub const QUEUE_ROOM: usize = 10_000;
// Leave the queue room for the user's own changes.
const _: () = assert!(QUEUE_ROOM < crate::sync::MAX_PENDING / 2);
/// Deepest folder path mirrored into the archive.
const MAX_DEPTH: usize = 64;
const DAY: i64 = 86_400;

// ---------------------------------------------------------------------------------------------------------
// Ageing

/// The date mail is aged by: received, else sent (`None` when it has neither).
pub fn mail_date(m: &MailItem) -> Option<i64> {
    let t = if m.received.unix > 0 { m.received.unix } else { m.sent.map_or(0, |d| d.unix) };
    (t > 0).then_some(t)
}

/// The date an appointment is aged by: its end. Recurring series and exceptions are never archived.
pub fn appointment_date(a: &Appointment) -> Option<i64> {
    if a.recurrence.is_some() || a.recurrence_id.is_some() {
        return None;
    }
    Some(a.end.unix.max(a.start.unix))
}

/// The date a task is aged by: completed tasks only, by completion (else due) date.
pub fn task_date(t: &Task) -> Option<i64> {
    if !t.is_complete() {
        return None;
    }
    t.completed.or(t.due).map(|d| d.unix)
}

/// The date a note is aged by: its last change.
pub fn note_date(n: &Note) -> Option<i64> {
    Some(n.modified.unix.max(n.created.unix))
}

/// Unix time `age` `unit`s before `now` (months are calendar months in local time, the day clamped to the
/// month's length: 31 August − 6 months = 28 or 29 February).
pub fn cutoff(now: DateTime, tz_offset_min: i32, age: u32, unit: AgeUnit) -> i64 {
    let age = i64::from(age.min(MAX_ARCHIVE_AGE));
    match unit {
        AgeUnit::Days => now.unix.saturating_sub(age.saturating_mul(DAY)),
        AgeUnit::Weeks => now.unix.saturating_sub(age.saturating_mul(7 * DAY)),
        AgeUnit::Months => {
            let off = i64::from(tz_offset_min).saturating_mul(60);
            let local = now.unix.saturating_add(off);
            let (y, m, d) = civil_from_days(local.div_euclid(DAY));
            let total =
                y.saturating_mul(12).saturating_add(i64::from(m)).saturating_sub(1).saturating_sub(age);
            let year = total.div_euclid(12);
            let month = u8::try_from(total.rem_euclid(12).saturating_add(1)).unwrap_or(1);
            let day = d.min(days_in_month(year, month));
            match days_from_civil(year, month, day) {
                Some(days) => {
                    days.saturating_mul(DAY).saturating_add(local.rem_euclid(DAY)).saturating_sub(off)
                }
                None => now.unix.saturating_sub(age.saturating_mul(30 * DAY)),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Settings

/// The global AutoArchive settings (File › Options › Advanced › AutoArchive Settings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Defaults {
    pub run: bool,
    pub every_days: u32,
    pub prompt: bool,
    pub delete_expired: bool,
    pub older_than_months: u32,
    pub action: ArchiveAction,
    /// Archive folder path; empty = `Archives` in Local Folders.
    pub folder: String,
}

pub fn defaults(s: &Session) -> Defaults {
    let st = &s.settings;
    Defaults {
        run: st.bool("advanced.autoArchive.run"),
        every_days: u32::try_from(st.int("advanced.autoArchive.everyDays")).unwrap_or(14).max(1),
        prompt: st.bool("advanced.autoArchive.prompt"),
        delete_expired: st.bool("advanced.autoArchive.deleteExpired"),
        older_than_months: u32::try_from(st.int("advanced.autoArchive.olderThanMonths")).unwrap_or(6).max(1),
        action: if st.text("advanced.autoArchive.action") == "delete" {
            ArchiveAction::Delete
        } else {
            ArchiveAction::Move
        },
        folder: st.text("advanced.autoArchive.folder"),
    }
}

/// A folder's AutoArchive properties (the defaults when it has none of its own).
pub fn folder_props(mb: &Mailbox, folder: FolderId) -> FolderArchive {
    mb.folder(folder).and_then(|f| f.auto_archive).unwrap_or_default()
}

/// Whether AutoArchive looks at items of this folder at all.
pub fn archivable(mb: &Mailbox, f: &Folder) -> bool {
    let kind_ok =
        matches!(f.kind, FolderKind::Mail | FolderKind::Calendar | FolderKind::Tasks | FolderKind::Notes);
    let subscribed = f.kind == FolderKind::Calendar
        && crate::cmd::calendar::props::read(mb, f.id).source != CalendarSource::Local;
    kind_ok && f.role != Some(FolderRole::Outbox) && !subscribed
}

/// What one run does in one folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Items aged before this (unix seconds) are old.
    pub cutoff: i64,
    pub action: ArchiveAction,
    /// Folder old items go to (`None` = the archive folder, mirroring the path).
    pub target: Option<FolderId>,
}

/// The rule AutoArchive applies to `f` by its properties (`None` = not archived).
pub fn rule_for(s: &Session, d: &Defaults, f: &Folder) -> Option<Rule> {
    let p = f.auto_archive.unwrap_or_default();
    let now = s.now();
    match p.mode {
        ArchiveMode::Never => None,
        ArchiveMode::Default => Some(Rule {
            cutoff: cutoff(now, s.tz_offset_min, d.older_than_months, AgeUnit::Months),
            action: d.action,
            target: None,
        }),
        ArchiveMode::Custom => Some(Rule {
            cutoff: cutoff(now, s.tz_offset_min, p.age.max(1), p.unit),
            action: p.action,
            target: p.target.filter(|_| p.action == ArchiveAction::Move),
        }),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Archive folders

/// The Local Folders data file (the local account named so, else the first local account).
pub fn local_account(mb: &Mailbox) -> Option<AccountId> {
    let locals = || mb.accounts.iter().filter(|a| a.kind == AccountKind::Local);
    locals().find(|a| a.display_name == LOCAL_ACCOUNT_FALLBACK_NAME).or_else(|| locals().next()).map(|a| a.id)
}

fn ensure_local_account(mb: &mut Mailbox) -> AccountId {
    if let Some(a) = local_account(mb) {
        return a;
    }
    let id = mb.add_account(Account {
        kind: AccountKind::Local,
        display_name: LOCAL_ACCOUNT_FALLBACK_NAME.to_owned(),
        ..Default::default()
    });
    mb.create_default_folders(id);
    id
}

fn child_named(
    mb: &Mailbox,
    account: AccountId,
    parent: Option<FolderId>,
    name: &str,
    kind: Option<FolderKind>,
) -> Option<FolderId> {
    let lower = name.to_lowercase();
    mb.folders
        .values()
        .find(|f| {
            f.account == account
                && f.parent == parent
                && f.name.to_lowercase() == lower
                && kind.is_none_or(|k| f.kind == k)
        })
        .map(|f| f.id)
}

/// The child of `parent` called `name` holding `kind` items, created when missing (as `name (2)`… when a
/// folder of another kind already has the name).
fn child_or_create(
    mb: &mut Mailbox,
    account: AccountId,
    parent: Option<FolderId>,
    name: &str,
    kind: FolderKind,
) -> Result<FolderId, CmdError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_FOLDER_NAME_CHARS {
        return Err(CmdError::Params(format!("not a folder name: {name:?}")));
    }
    if let Some(id) = child_named(mb, account, parent, name, Some(kind)) {
        return Ok(id);
    }
    let mut chosen = name.to_owned();
    let mut n: u32 = 2;
    while child_named(mb, account, parent, &chosen, None).is_some() {
        if n > 99 {
            return Err(CmdError::Failed(format!("no free folder name for {name:?}")));
        }
        let suffix = format!(" ({n})");
        let stem: String = name.chars().take(MAX_FOLDER_NAME_CHARS.saturating_sub(suffix.len())).collect();
        chosen = format!("{}{suffix}", stem.trim_end());
        n = n.saturating_add(1);
    }
    let id = FolderId(mb.alloc_id());
    mb.folders.insert(id, Folder { id, account, parent, name: chosen, kind, ..Default::default() });
    Ok(id)
}

/// The archive folder named by `text` (empty = `Archives` in Local Folders), if it exists.
pub fn find_root(s: &Session, text: &str) -> Option<FolderId> {
    let text = text.trim().trim_matches('\\');
    if text.is_empty() {
        let account = local_account(&s.mailbox)?;
        return child_named(&s.mailbox, account, None, ARCHIVES_NAME, None);
    }
    crate::cmd::folder::resolve_folder(s, &json!(text)).filter(|f| {
        s.mailbox.folder(*f).is_some_and(|f| !matches!(f.kind, FolderKind::Search | FolderKind::SearchRoot))
    })
}

/// The archive folder named by `text`, created in Local Folders (with any missing parents) when missing.
pub fn ensure_root(s: &mut Session, text: &str) -> Result<FolderId, CmdError> {
    if let Some(root) = find_root(s, text) {
        return Ok(root);
    }
    let account = ensure_local_account(&mut s.mailbox);
    let account_name = s.mailbox.account(account).map(|a| a.display_name.to_lowercase()).unwrap_or_default();
    let mut parts: Vec<&str> = text.split('\\').map(str::trim).filter(|p| !p.is_empty()).collect();
    // "Local Folders\Old mail" names the data file first.
    if parts.len() > 1 && parts.first().is_some_and(|p| p.to_lowercase() == account_name) {
        parts.remove(0);
    }
    if parts.is_empty() {
        parts.push(ARCHIVES_NAME);
    }
    parts.truncate(MAX_DEPTH);
    let mut parent = None;
    for part in parts {
        parent = Some(child_or_create(&mut s.mailbox, account, parent, part, FolderKind::Mail)?);
    }
    parent.ok_or_else(|| CmdError::Failed("no archive folder".into()))
}

/// The copy of `source`'s path under `root` (created as needed): `Inbox\Projects` → `root\Inbox\Projects`.
pub fn mirror(mb: &mut Mailbox, root: FolderId, source: FolderId) -> Result<FolderId, CmdError> {
    let account =
        mb.folder_account(root).ok_or_else(|| CmdError::Failed("the archive folder is gone".into()))?;
    let mut chain: Vec<(String, FolderKind)> = Vec::new();
    let mut cur = Some(source);
    while let Some(c) = cur {
        if chain.len() >= MAX_DEPTH {
            break;
        }
        let Some(f) = mb.folder(c) else { break };
        chain.push((f.name.clone(), f.kind));
        cur = f.parent;
    }
    chain.reverse();
    let mut parent = root;
    for (name, kind) in chain {
        parent = child_or_create(mb, account, Some(parent), &name, kind)?;
    }
    Ok(parent)
}

// ---------------------------------------------------------------------------------------------------------
// Running

/// What one run covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Archive this folder and its subfolders with one cutoff (the Archive dialog); `None` = every folder by
    /// its own AutoArchive properties.
    pub tree: Option<(FolderId, i64)>,
    /// Also archive mail marked "Do not AutoArchive this item".
    pub include_excluded: bool,
    /// Archive folder for this run (`None` = the one in Options).
    pub root: Option<FolderId>,
    /// Most items handled.
    pub limit: usize,
}

impl Plan {
    /// AutoArchive over every folder by its properties.
    pub fn all() -> Plan {
        Plan { tree: None, include_excluded: false, root: None, limit: MAX_ITEMS_PER_RUN }
    }
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Items moved to archive folders.
    pub moved: usize,
    /// Old items deleted permanently.
    pub deleted: usize,
    /// Expired mail deleted.
    pub expired: usize,
    /// Old server mail left in place because only its headers are downloaded.
    pub header_only: usize,
    /// Folders looked at.
    pub folders: usize,
    /// More old items wait (the run hit its limit, or a server change queue is full).
    pub remaining: bool,
    /// The archive folder used (when items were moved there).
    pub root: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mail,
    Appointment,
    Task,
    Note,
}

#[derive(Clone, Copy, Debug)]
enum Job {
    Expired(ItemId),
    Old(Kind, ItemId, FolderId, Rule),
}

impl Report {
    /// Items the run moved or deleted.
    pub fn done(&self) -> usize {
        self.moved.saturating_add(self.deleted).saturating_add(self.expired)
    }
}

fn set_folder(mb: &mut Mailbox, kind: Kind, id: ItemId, to: FolderId) -> bool {
    let slot = match kind {
        Kind::Mail => mb.mail.get_mut(&id).map(|x| &mut x.folder),
        Kind::Appointment => mb.appointments.get_mut(&id).map(|x| &mut x.folder),
        Kind::Task => mb.tasks.get_mut(&id).map(|x| &mut x.folder),
        Kind::Note => mb.notes.get_mut(&id).map(|x| &mut x.folder),
    };
    match slot {
        Some(f) if *f != to => {
            *f = to;
            true
        }
        _ => false,
    }
}

fn remove(mb: &mut Mailbox, kind: Kind, id: ItemId) -> bool {
    match kind {
        Kind::Mail => mb.mail.remove(&id).is_some(),
        Kind::Appointment => mb.appointments.remove(&id).is_some(),
        Kind::Task => mb.tasks.remove(&id).is_some(),
        Kind::Note => mb.notes.remove(&id).is_some(),
    }
}

/// Moves or deletes old items as `plan` says (see the module docs). Changes the mailbox only; the command
/// around it records undo and journals.
pub fn archive(s: &mut Session, plan: &Plan) -> Result<Report, CmdError> {
    let d = defaults(s);
    let now = s.now().unix;
    let configured = plan.root.or_else(|| find_root(s, &d.folder));
    // Folders items are moved into are never archived themselves (nor is the default Archives folder after
    // Options name another one).
    let mut sinks: Vec<FolderId> = configured.into_iter().chain(find_root(s, "")).collect();
    sinks.extend(s.mailbox.folders.values().filter_map(|f| {
        let p = f.auto_archive?;
        (p.mode == ArchiveMode::Custom && p.action == ArchiveAction::Move).then_some(p.target).flatten()
    }));
    let candidates: Vec<FolderId> = match plan.tree {
        Some((f, _)) => s.mailbox.subtree(f),
        None => s.mailbox.folders.keys().copied().collect(),
    };
    let mut rules: BTreeMap<FolderId, Rule> = BTreeMap::new();
    for id in candidates {
        let Some(f) = s.mailbox.folder(id) else { continue };
        if !archivable(&s.mailbox, f) || sinks.iter().any(|t| s.mailbox.is_in_subtree(id, *t)) {
            continue;
        }
        let rule = match plan.tree {
            Some((_, before)) => Some(Rule { cutoff: before, action: ArchiveAction::Move, target: None }),
            None => rule_for(s, &d, f),
        };
        if let Some(r) = rule {
            rules.insert(id, r);
        }
    }
    let mut report = Report { folders: rules.len(), ..Default::default() };
    let delete_expired = plan.tree.is_none() && d.delete_expired;
    let ss: &Session = s;
    let mb = &ss.mailbox;
    let is_mail_folder = |f: FolderId| mb.folder(f).is_some_and(|f| f.kind == FolderKind::Mail);
    // Each job with the account whose server change queue it feeds.
    let mut jobs: Vec<(Job, Option<AccountId>)> = Vec::new();
    for m in mb.mail.values() {
        let Some(r) = rules.get(&m.folder) else { continue };
        let queue = crate::sync::queue_account(mb, m);
        if delete_expired
            && is_mail_folder(m.folder)
            && crate::cmd::view::mail::stored_expiry(ss, m).is_some_and(|e| e.unix <= now)
        {
            jobs.push((Job::Expired(m.id), queue));
            continue;
        }
        if (!m.no_auto_archive || plan.include_excluded) && mail_date(m).is_some_and(|t| t < r.cutoff) {
            // A move out of the server folder expunges the server copy; one never downloaded in full would
            // lose its body (a move within the same account keeps it on the server).
            if r.action == ArchiveAction::Move
                && let Some(src) = queue
                && crate::sync::cmds::is_header_only(ss, m.id)
                && r.target.and_then(|t| crate::sync::synced_account(mb, t)) != Some(src)
            {
                report.header_only = report.header_only.saturating_add(1);
                continue;
            }
            jobs.push((Job::Old(Kind::Mail, m.id, m.folder, *r), queue));
        }
    }
    let mut old = |kind: Kind, id: ItemId, folder: FolderId, date: Option<i64>| {
        if let Some(r) = rules.get(&folder)
            && date.is_some_and(|t| t < r.cutoff)
        {
            jobs.push((Job::Old(kind, id, folder, *r), None));
        }
    };
    for a in mb.appointments.values() {
        old(Kind::Appointment, a.id, a.folder, appointment_date(a));
    }
    for t in mb.tasks.values() {
        old(Kind::Task, t.id, t.folder, task_date(t));
    }
    for n in mb.notes.values() {
        old(Kind::Note, n.id, n.folder, note_date(n));
    }
    // The run's limit, and the room left in each server change queue.
    let mut room: BTreeMap<AccountId, usize> = BTreeMap::new();
    let mut taken: Vec<Job> = Vec::new();
    for (job, queue) in jobs {
        if taken.len() >= plan.limit {
            report.remaining = true;
            break;
        }
        if let Some(a) = queue {
            let left = room
                .entry(a)
                .or_insert_with(|| QUEUE_ROOM.saturating_sub(crate::sync::pending_len(&s.mailbox, a)));
            if *left == 0 {
                report.remaining = true;
                continue;
            }
            *left = left.saturating_sub(1);
        }
        taken.push(job);
    }
    let mut root = configured;
    let mut dest: BTreeMap<FolderId, FolderId> = BTreeMap::new();
    for job in taken {
        match job {
            Job::Expired(id) => {
                if remove(&mut s.mailbox, Kind::Mail, id) {
                    report.expired = report.expired.saturating_add(1);
                }
            }
            Job::Old(kind, id, _, r) if r.action == ArchiveAction::Delete => {
                if remove(&mut s.mailbox, kind, id) {
                    report.deleted = report.deleted.saturating_add(1);
                }
            }
            Job::Old(kind, id, folder, r) => {
                let to = match dest.get(&folder) {
                    Some(to) => *to,
                    None => {
                        let to = destination(s, &mut root, &d.folder, folder, r.target)?;
                        dest.insert(folder, to);
                        to
                    }
                };
                if set_folder(&mut s.mailbox, kind, id, to) {
                    report.moved = report.moved.saturating_add(1);
                }
            }
        }
    }
    if report.moved > 0 {
        report.root = root.map(|r| r.0);
    }
    Ok(report)
}

/// Where old items of `source` go: its own target folder when it holds the same kind of items, else the
/// mirrored path under the archive folder (created on first use).
fn destination(
    s: &mut Session,
    root: &mut Option<FolderId>,
    root_text: &str,
    source: FolderId,
    target: Option<FolderId>,
) -> Result<FolderId, CmdError> {
    let kind = s.mailbox.folder(source).map(|f| f.kind);
    if let Some(t) = target
        && t != source
        && s.mailbox.folder(t).map(|f| f.kind) == kind
    {
        return Ok(t);
    }
    let r = match *root {
        Some(r) if s.mailbox.folders.contains_key(&r) => r,
        _ => ensure_root(s, root_text)?,
    };
    *root = Some(r);
    mirror(&mut s.mailbox, r, source)
}

// ---------------------------------------------------------------------------------------------------------
// Scheduling

/// When AutoArchive runs next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// "Run AutoArchive every N days" is on.
    pub enabled: bool,
    pub every_days: u32,
    /// "Prompt before AutoArchive runs".
    pub prompt: bool,
    /// Last run (unix seconds), `None` = never.
    pub last_run: Option<i64>,
    /// Next run (unix seconds), `None` until a first run is recorded.
    pub next_run: Option<i64>,
    /// A run is due now.
    pub due: bool,
}

pub fn schedule(s: &Session) -> Schedule {
    let d = defaults(s);
    let last_run = Some(s.settings.int(LAST_RUN_KEY)).filter(|t| *t > 0);
    let next_run = last_run.map(|t| t.saturating_add(i64::from(d.every_days).saturating_mul(DAY)));
    let now = s.now().unix;
    Schedule {
        enabled: d.run,
        every_days: d.every_days,
        prompt: d.prompt,
        last_run,
        next_run,
        due: d.run && next_run.is_some_and(|n| n <= now),
    }
}

/// Records a run (or a skipped one) now and saves the profile settings.
pub fn record_run(s: &mut Session) {
    let now = s.now().unix.max(1);
    if let Err(e) = s.settings.set(LAST_RUN_KEY, &json!(now)) {
        log::error!("AutoArchive: cannot record the run: {e}");
    }
    if let Err(e) = s.save_settings() {
        log::error!("{e}");
    }
}

/// The schedule as the scheduler sees it: the first look at a turned-on schedule starts its clock, and a last
/// run in the future (the clock went back) restarts it.
pub fn poll(s: &mut Session) -> Schedule {
    let sch = schedule(s);
    if sch.enabled && sch.last_run.is_none_or(|t| t > s.now().unix) {
        record_run(s);
        return schedule(s);
    }
    sch
}

/// Runs AutoArchive as an automatic action (no undo step), queueing the server changes it makes (items
/// leaving synchronised folders) and journaling them.
pub fn run_scheduled(s: &mut Session) -> CmdResult {
    let before = s.mailbox.clone();
    let out = s.run_automatic("file.archive", &json!({ "all": true }))?;
    let mid = s.mailbox.clone();
    crate::sync::queue_local_changes(s, &before);
    let ops = crate::session::diff_ops(&mid, &s.mailbox);
    s.journal(ops);
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------
// Commands

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    mutates: bool,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates, enabled: always, run }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "file.archive",
            "Clean Up Old Items...",
            "File › Info › Tools; Mailbox Cleanup › AutoArchive",
            r#"{} (dialog) | {"all":true} (AutoArchive every folder now) | {"folder"?:string,"before":"YYYY-MM-DD"|number,"includeExcluded"?:bool,"target"?:string}"#,
            true,
            run_archive,
        ),
        spec("file.autoArchiveStatus", "AutoArchive Status", "AutoArchive scheduler", "", false, run_status),
        spec("file.autoArchivePostpone", "Not Now", "AutoArchive prompt", "", false, run_postpone),
        spec(
            "options.autoArchive",
            "AutoArchive Settings...",
            "File › Options › Advanced › AutoArchive",
            r#"{} (dialog) | {"run"?:bool,"everyDays"?:number,"prompt"?:bool,"deleteExpired"?:bool,"olderThanMonths"?:number,"action"?:"move"|"delete","folder"?:string} | {"applyToAll":true}"#,
            true,
            run_options,
        ),
        spec(
            "mail.doNotAutoArchive",
            "Do not AutoArchive this item",
            "Message › Properties",
            r#"{"ids"?:[number],"on"?:bool}"#,
            true,
            run_exclude,
        ),
    ]
}

fn report_json(s: &Session, r: &Report) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    if let (Some(o), Some(root)) = (v.as_object_mut(), r.root) {
        o.insert("rootPath".into(), json!(s.mailbox.folder_path(FolderId(root))));
    }
    v
}

fn summary(r: &Report) -> String {
    let n = |k: usize, one: &str| format!("{k} {one}{}", if k == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if r.moved > 0 {
        parts.push(format!("moved {} to the archive", n(r.moved, "item")));
    }
    if r.deleted > 0 {
        parts.push(format!("deleted {}", n(r.deleted, "old item")));
    }
    if r.expired > 0 {
        parts.push(format!("deleted {}", n(r.expired, "expired message")));
    }
    let mut text = if parts.is_empty() {
        "AutoArchive found no old items.".to_owned()
    } else {
        format!("AutoArchive {}.", parts.join(", "))
    };
    if r.header_only > 0 {
        let what = if r.header_only == 1 {
            "1 message with only its headers was".to_owned()
        } else {
            format!("{} messages with only their headers were", r.header_only)
        };
        text.push_str(&format!(" {what} left in place until downloaded in full."));
    }
    if r.remaining {
        text.push_str(" More old items remain.");
    }
    text
}

/// Local midnight of "YYYY-MM-DD", or a unix time given as a number.
fn parse_before(s: &Session, v: &Value) -> Result<i64, CmdError> {
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    let text = v.as_str().map(str::trim).unwrap_or_default();
    let mut parts = text.splitn(3, '-').map(|p| p.trim().parse::<i64>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (parts.next(), parts.next(), parts.next()) else {
        return Err(CmdError::Params(format!("`before` must be a date (YYYY-MM-DD), not {v}")));
    };
    let days = u8::try_from(m)
        .ok()
        .zip(u8::try_from(d).ok())
        .and_then(|(m, d)| days_from_civil(y, m, d))
        .ok_or_else(|| CmdError::Params(format!("not a date: {text}")))?;
    Ok(days.saturating_mul(DAY).saturating_sub(i64::from(s.tz_offset_min).saturating_mul(60)))
}

fn date_text(s: &Session, unix: i64) -> String {
    let local = unix.saturating_add(i64::from(s.tz_offset_min).saturating_mul(60));
    let (y, m, d) = civil_from_days(local.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

fn root_json(s: &Session, root: Option<FolderId>) -> (Value, String) {
    match root {
        Some(r) => (json!(r.0), s.mailbox.folder_path(r)),
        None => (Value::Null, String::new()),
    }
}

fn run_archive(s: &mut Session, p: &Value) -> CmdResult {
    let d = defaults(s);
    if opt_bool(p, "all").unwrap_or(false) {
        let report = archive(s, &Plan::all())?;
        if !report.remaining {
            record_run(s);
        }
        crate::cmd::folder::prune_selection(s);
        s.notify(Notice::Info(summary(&report)));
        return Ok(report_json(s, &report));
    }
    let folder = crate::cmd::folder::current_folder(s, p)?;
    let target = crate::cmd::folder::param_folder(s, p, "target")?;
    let Some(before) = p.get("before").filter(|v| !v.is_null()) else {
        let default_before = cutoff(s.now(), s.tz_offset_min, d.older_than_months, AgeUnit::Months);
        let (root, root_path) = root_json(s, target.or_else(|| find_root(s, &d.folder)));
        let default_target =
            if d.folder.trim().is_empty() { ARCHIVES_NAME.to_owned() } else { d.folder.clone() };
        let params = json!({
            "folder": folder.0,
            "path": s.mailbox.folder_path(folder),
            "before": date_text(s, default_before),
            "includeExcluded": false,
            "target": root,
            "targetPath": root_path,
            "defaultTarget": default_target,
        });
        return Ok(crate::cmd::folder::dialog(s, "archive", params));
    };
    let before = parse_before(s, before)?;
    if let Some(t) = target
        && s.mailbox.folder(t).is_some_and(|f| matches!(f.kind, FolderKind::Search | FolderKind::SearchRoot))
    {
        return Err(CmdError::Params("items cannot be archived into a search folder".into()));
    }
    let plan = Plan {
        tree: Some((folder, before)),
        include_excluded: opt_bool(p, "includeExcluded").unwrap_or(false),
        root: target,
        limit: MAX_ITEMS_PER_RUN,
    };
    let report = archive(s, &plan)?;
    crate::cmd::folder::prune_selection(s);
    s.notify(Notice::Info(summary(&report)));
    Ok(report_json(s, &report))
}

fn status_json(s: &Session) -> Value {
    let mut v = serde_json::to_value(schedule(s)).unwrap_or_else(|_| json!({}));
    let d = defaults(s);
    if let Some(o) = v.as_object_mut() {
        let (root, path) = root_json(s, find_root(s, &d.folder));
        o.insert("root".into(), root);
        o.insert("rootPath".into(), json!(path));
    }
    v
}

fn run_status(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(status_json(s))
}

fn run_postpone(s: &mut Session, _p: &Value) -> CmdResult {
    record_run(s);
    Ok(status_json(s))
}

/// Keys `options.autoArchive` accepts: (param, setting).
const OPTION_KEYS: [(&str, &str); 7] = [
    ("run", "advanced.autoArchive.run"),
    ("everyDays", "advanced.autoArchive.everyDays"),
    ("prompt", "advanced.autoArchive.prompt"),
    ("deleteExpired", "advanced.autoArchive.deleteExpired"),
    ("olderThanMonths", "advanced.autoArchive.olderThanMonths"),
    ("action", "advanced.autoArchive.action"),
    ("folder", "advanced.autoArchive.folder"),
];

fn run_options(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "applyToAll").unwrap_or(false) {
        // "Apply these settings to all folders now": every folder goes back to the default settings.
        let mut n = 0usize;
        for f in s.mailbox.folders.values_mut() {
            if f.auto_archive.take().is_some() {
                n = n.saturating_add(1);
            }
        }
        return Ok(json!({ "folders": n }));
    }
    let given: Vec<(&str, &Value)> = OPTION_KEYS
        .iter()
        .filter_map(|(param, key)| p.get(*param).filter(|v| !v.is_null()).map(|v| (*key, v)))
        .collect();
    if given.is_empty() {
        let d = defaults(s);
        let mut params = serde_json::to_value(&d).unwrap_or_else(|_| json!({}));
        if let Some(o) = params.as_object_mut() {
            let (root, path) = root_json(s, find_root(s, &d.folder));
            o.insert("root".into(), root);
            o.insert("rootPath".into(), json!(path));
            o.insert("lastRun".into(), json!(schedule(s).last_run));
        }
        return Ok(crate::cmd::folder::dialog(s, "autoArchiveOptions", params));
    }
    // Validate everything before changing anything.
    let mut values = Vec::new();
    for (key, v) in given {
        let def =
            crate::settings::def(key).ok_or_else(|| CmdError::Failed(format!("unknown setting {key}")))?;
        values.push((key, crate::settings::normalize(def, v)?));
    }
    for (key, v) in &values {
        s.settings.set(key, v)?;
    }
    // Turning the schedule on starts its clock.
    poll(s);
    Ok(status_json(s))
}

fn run_exclude(s: &mut Session, p: &Value) -> CmdResult {
    let ids = crate::cmd::util::mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let on = opt_bool(p, "on").unwrap_or(true);
    let mut n = 0usize;
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id)
            && m.no_auto_archive != on
        {
            m.no_auto_archive = on;
            n = n.saturating_add(1);
        }
    }
    Ok(json!({ "changed": n, "on": on }))
}

/// `folder.autoArchiveSettings`: with no settings in `p`, opens the folder's AutoArchive dialog; otherwise
/// saves them on the folder (`mode` default | never | custom, `age`, `unit` days | weeks | months, `action`
/// move | delete, `target` folder or null).
pub fn folder_settings(s: &mut Session, p: &Value) -> CmdResult {
    let folder = crate::cmd::folder::current_folder(s, p)?;
    let given = ["mode", "age", "unit", "action", "target"].iter().any(|k| p.get(*k).is_some());
    let mut props = folder_props(&s.mailbox, folder);
    if !given {
        let d = defaults(s);
        let (target, target_path) = root_json(s, props.target.filter(|t| s.mailbox.folders.contains_key(t)));
        let (root, root_path) = root_json(s, find_root(s, &d.folder));
        let f = s.mailbox.folder(folder);
        let params = json!({
            "folder": folder.0,
            "name": f.map(|f| f.name.clone()).unwrap_or_default(),
            "path": s.mailbox.folder_path(folder),
            "archivable": f.is_some_and(|f| archivable(&s.mailbox, f)),
            "props": props_json(&props),
            "target": target,
            "targetPath": target_path,
            "defaults": serde_json::to_value(&d).unwrap_or(Value::Null),
            "root": root,
            "rootPath": root_path,
        });
        return Ok(crate::cmd::folder::dialog(s, "autoArchiveSettings", params));
    }
    if let Some(mode) = opt_str(p, "mode") {
        props.mode = match mode.trim().to_ascii_lowercase().as_str() {
            "default" => ArchiveMode::Default,
            "never" => ArchiveMode::Never,
            "custom" => ArchiveMode::Custom,
            other => {
                return Err(CmdError::Params(format!(
                    "`mode` must be default, never or custom, not {other}"
                )));
            }
        };
    }
    if let Some(v) = p.get("age").filter(|v| !v.is_null()) {
        props.age = v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (1..=MAX_ARCHIVE_AGE).contains(n))
            .ok_or_else(|| CmdError::Params(format!("`age` must be 1 to {MAX_ARCHIVE_AGE}, not {v}")))?;
    }
    if let Some(unit) = opt_str(p, "unit") {
        props.unit = match unit.trim().to_ascii_lowercase().as_str() {
            "days" | "day" => AgeUnit::Days,
            "weeks" | "week" => AgeUnit::Weeks,
            "months" | "month" => AgeUnit::Months,
            other => {
                return Err(CmdError::Params(format!("`unit` must be days, weeks or months, not {other}")));
            }
        };
    }
    if let Some(action) = opt_str(p, "action") {
        props.action = match action.trim().to_ascii_lowercase().as_str() {
            "move" => ArchiveAction::Move,
            "delete" => ArchiveAction::Delete,
            other => return Err(CmdError::Params(format!("`action` must be move or delete, not {other}"))),
        };
    }
    if p.get("target").is_some() {
        props.target = crate::cmd::folder::param_folder(s, p, "target")?;
        if let Some(t) = props.target {
            let kind = s.mailbox.folder(folder).map(|f| f.kind);
            if t == folder {
                return Err(CmdError::Params("old items cannot be moved into the folder itself".into()));
            }
            if s.mailbox.folder(t).map(|f| f.kind) != kind {
                return Err(CmdError::Params("the archive folder must hold the same kind of items".into()));
            }
        }
    }
    let stored = (props != FolderArchive::default()).then_some(props);
    if let Some(f) = s.mailbox.folders.get_mut(&folder) {
        f.auto_archive = stored;
    }
    Ok(json!({ "folder": folder.0, "props": props_json(&props) }))
}

fn props_json(p: &FolderArchive) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
