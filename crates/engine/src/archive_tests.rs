//! AutoArchive tests: ageing per item type, per-folder properties, move vs delete, the mirrored archive tree,
//! expired mail, the Archive dialog's folder runs, undo and the schedule.

use super::*;
use mailcraft_model::{FlagState, TaskStatus};

const DAY: i64 = 86_400;

/// 2026-10-20 12:00 UTC.
fn now_unix() -> i64 {
    days_from_civil(2026, 10, 20).unwrap() * DAY + 12 * 3600
}

fn ago(days: i64) -> DateTime {
    DateTime::from_unix(now_unix() - days * DAY, 0)
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local(LOCAL_ACCOUNT_FALLBACK_NAME));
    s.fixed_now = Some(DateTime::from_unix(now_unix(), 0));
    s
}

fn role(s: &Session, r: FolderRole) -> FolderId {
    let acct = s.mailbox.accounts.first().unwrap().id;
    s.mailbox.role_folder(acct, r).unwrap()
}

fn mail(s: &mut Session, folder: FolderId, days_ago: i64) -> ItemId {
    s.mailbox.add_mail(MailItem {
        folder,
        subject: format!("{days_ago} days old"),
        received: ago(days_ago),
        ..Default::default()
    })
}

fn folder_of(s: &Session, id: ItemId) -> Option<String> {
    s.mailbox.mail.get(&id).map(|m| s.mailbox.folder_path(m.folder))
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn set(s: &mut Session, key: &str, v: Value) {
    s.settings.set(key, &v).unwrap();
}

#[test]
fn months_are_calendar_months_clamped_to_the_month() {
    let at = |y, m, d| DateTime::from_unix(days_from_civil(y, m, d).unwrap() * DAY + 3600, 0);
    let back = |t: i64| civil_from_days(t.div_euclid(DAY));
    assert_eq!(back(cutoff(at(2026, 8, 31), 0, 6, AgeUnit::Months)), (2026, 2, 28));
    assert_eq!(back(cutoff(at(2024, 8, 31), 0, 6, AgeUnit::Months)), (2024, 2, 29));
    assert_eq!(back(cutoff(at(2026, 1, 15), 0, 13, AgeUnit::Months)), (2024, 12, 15));
    assert_eq!(cutoff(at(2026, 1, 15), 0, 2, AgeUnit::Weeks), at(2026, 1, 1).unix);
    assert_eq!(cutoff(at(2026, 1, 15), 0, 3, AgeUnit::Days), at(2026, 1, 12).unix);
    // Local time: 00:30 on 1 March at UTC+2 is still 28 February in UTC.
    let t = DateTime::from_unix(days_from_civil(2026, 2, 28).unwrap() * DAY + 22 * 3600 + 1800, 0);
    let c = cutoff(t, 120, 1, AgeUnit::Months);
    assert_eq!(back(c + 120 * 60), (2026, 2, 1));
    // Hostile ages are capped, never overflow.
    assert!(cutoff(t, 0, u32::MAX, AgeUnit::Months) < t.unix);
}

#[test]
fn items_age_by_their_own_date() {
    let m = MailItem { received: ago(10), ..Default::default() };
    assert_eq!(mail_date(&m), Some(ago(10).unix));
    let no_received = MailItem { sent: Some(ago(3)), ..Default::default() };
    assert_eq!(mail_date(&no_received), Some(ago(3).unix));
    assert_eq!(mail_date(&MailItem::default()), None, "undated mail is never old");

    let a = Appointment { start: ago(40), end: ago(39), ..Default::default() };
    assert_eq!(appointment_date(&a), Some(ago(39).unix), "by the end");
    let series = Appointment {
        recurrence: Some(mailcraft_model::Recurrence { rrule: "FREQ=DAILY".into(), exdates: Vec::new() }),
        ..a.clone()
    };
    assert_eq!(appointment_date(&series), None, "recurring series stay");
    let exception = Appointment { recurrence_id: Some(ago(40)), ..a };
    assert_eq!(appointment_date(&exception), None);

    let open = Task { due: Some(ago(100)), ..Default::default() };
    assert_eq!(task_date(&open), None, "open tasks stay");
    let done = Task {
        status: TaskStatus::Completed,
        completed: Some(ago(50)),
        due: Some(ago(90)),
        ..Default::default()
    };
    assert_eq!(task_date(&done), Some(ago(50).unix), "by completion");
    let done_undated = Task { status: TaskStatus::Completed, due: Some(ago(90)), ..Default::default() };
    assert_eq!(task_date(&done_undated), Some(ago(90).unix));

    let n = Note { created: ago(300), modified: ago(20), ..Default::default() };
    assert_eq!(note_date(&n), Some(ago(20).unix), "by the last change");
}

#[test]
fn default_run_moves_old_items_into_a_mirrored_archives_tree() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let projects = s.mailbox.create_folder(inbox, "Projects").unwrap();
    let old_inbox = mail(&mut s, inbox, 200);
    let new_inbox = mail(&mut s, inbox, 20);
    let old_project = mail(&mut s, projects, 400);
    let calendar = role(&s, FolderRole::Calendar);
    let past = s.mailbox.appointments.len();
    let appt_id = ItemId(s.mailbox.alloc_id());
    s.mailbox.appointments.insert(
        appt_id,
        Appointment {
            id: appt_id,
            folder: calendar,
            subject: "Old".into(),
            start: ago(300),
            end: ago(300),
            ..Default::default()
        },
    );
    assert_eq!(s.mailbox.appointments.len(), past + 1);
    let tasks = role(&s, FolderRole::Tasks);
    let done = ItemId(s.mailbox.alloc_id());
    s.mailbox.tasks.insert(
        done,
        Task {
            id: done,
            folder: tasks,
            status: TaskStatus::Completed,
            completed: Some(ago(365)),
            ..Default::default()
        },
    );
    let open = ItemId(s.mailbox.alloc_id());
    s.mailbox.tasks.insert(open, Task { id: open, folder: tasks, due: Some(ago(365)), ..Default::default() });
    let notes = role(&s, FolderRole::Notes);
    let note = ItemId(s.mailbox.alloc_id());
    s.mailbox.notes.insert(
        note,
        Note { id: note, folder: notes, created: ago(500), modified: ago(500), ..Default::default() },
    );
    let contacts = role(&s, FolderRole::Contacts);
    let contact = ItemId(s.mailbox.alloc_id());
    s.mailbox
        .contacts
        .insert(contact, mailcraft_model::Contact { id: contact, folder: contacts, ..Default::default() });

    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(r["moved"], 5, "{r}");
    assert_eq!(r["rootPath"], "Archives");
    assert_eq!(folder_of(&s, old_inbox).as_deref(), Some("Archives\\Inbox"));
    assert_eq!(folder_of(&s, old_project).as_deref(), Some("Archives\\Inbox\\Projects"));
    assert_eq!(folder_of(&s, new_inbox).as_deref(), Some("Inbox"));
    let appt_folder = s.mailbox.appointments[&appt_id].folder;
    assert_eq!(s.mailbox.folder_path(appt_folder), "Archives\\Calendar");
    assert_eq!(s.mailbox.folder(appt_folder).unwrap().kind, FolderKind::Calendar, "mirrors keep the kind");
    assert_eq!(s.mailbox.folder_path(s.mailbox.tasks[&done].folder), "Archives\\Tasks");
    assert_eq!(s.mailbox.tasks[&open].folder, tasks, "open tasks stay");
    assert_eq!(s.mailbox.folder(s.mailbox.notes[&note].folder).unwrap().kind, FolderKind::Notes);
    assert_eq!(s.mailbox.contacts[&contact].folder, contacts, "contacts are never archived");
    // The archive is a user folder at the top of Local Folders, and is never archived itself.
    let root = find_root(&s, "").unwrap();
    let f = s.mailbox.folder(root).unwrap();
    assert_eq!((f.parent, f.role, f.name.as_str()), (None, None, ARCHIVES_NAME));
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(r["moved"], 0);
    assert_eq!(folder_of(&s, old_inbox).as_deref(), Some("Archives\\Inbox"));
    // A manual run is one undo step (the second run changed nothing, so it left none).
    s.run("app.undo", &json!({})).unwrap();
    assert_eq!(folder_of(&s, old_inbox).as_deref(), Some("Inbox"));
    assert_eq!(folder_of(&s, old_project).as_deref(), Some("Inbox\\Projects"));
}

#[test]
fn the_age_setting_and_action_change_what_happens() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let a = mail(&mut s, inbox, 100); // ~3 months
    set(&mut s, "advanced.autoArchive.olderThanMonths", json!(6));
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, a).as_deref(), Some("Inbox"), "younger than 6 months");
    set(&mut s, "advanced.autoArchive.olderThanMonths", json!(2));
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, a).as_deref(), Some("Archives\\Inbox"));
    // "Permanently delete old items".
    let b = mail(&mut s, inbox, 100);
    set(&mut s, "advanced.autoArchive.action", json!("delete"));
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!((r["deleted"].as_u64(), r["moved"].as_u64()), (Some(1), Some(0)));
    assert!(!s.mailbox.mail.contains_key(&b));
    assert!(s.mailbox.mail.contains_key(&a), "what was archived stays archived");
}

#[test]
fn archive_folder_setting_names_the_root() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let a = mail(&mut s, inbox, 400);
    set(&mut s, "advanced.autoArchive.folder", json!("Local Folders\\Old mail\\2026"));
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, a).as_deref(), Some("Old mail\\2026\\Inbox"));
    // An existing folder by path is used as it is.
    let b = mail(&mut s, inbox, 400);
    set(&mut s, "advanced.autoArchive.folder", json!("Archive"));
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, b).as_deref(), Some("Archive\\Inbox"));
}

#[test]
fn folder_properties_override_the_defaults() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let sent = role(&s, FolderRole::SentItems);
    let drafts = role(&s, FolderRole::Drafts);
    let junk = role(&s, FolderRole::JunkEmail);
    let kept = s.mailbox.create_folder(inbox, "Kept").unwrap();
    let in_never = mail(&mut s, sent, 900);
    let three_weeks = mail(&mut s, inbox, 21);
    let in_delete = mail(&mut s, drafts, 21);
    let in_target = mail(&mut s, junk, 21);
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": sent.0, "mode": "never"}));
    run(
        &mut s,
        "folder.autoArchiveSettings",
        json!({"folder": inbox.0, "mode": "custom", "age": 2, "unit": "weeks"}),
    );
    run(
        &mut s,
        "folder.autoArchiveSettings",
        json!({"folder": drafts.0, "mode": "custom", "age": 14, "unit": "days", "action": "delete"}),
    );
    run(
        &mut s,
        "folder.autoArchiveSettings",
        json!({"folder": junk.0, "mode": "custom", "age": 1, "unit": "weeks", "target": kept.0}),
    );
    assert_eq!(folder_props(&s.mailbox, sent).mode, ArchiveMode::Never);
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!((r["moved"].as_u64(), r["deleted"].as_u64()), (Some(2), Some(1)), "{r}");
    assert_eq!(folder_of(&s, in_never).as_deref(), Some("Sent Items"), "Do not archive");
    assert_eq!(folder_of(&s, three_weeks).as_deref(), Some("Archives\\Inbox"), "own age");
    assert!(!s.mailbox.mail.contains_key(&in_delete), "own action: delete");
    assert_eq!(folder_of(&s, in_target).as_deref(), Some("Inbox\\Kept"), "own folder, no mirroring");
    // The target folder is not archived itself (it sits under Inbox, whose age would catch it).
    assert_eq!(folder_of(&s, in_target).as_deref(), Some("Inbox\\Kept"));
    // Back to the defaults: nothing stored on the folder.
    run(
        &mut s,
        "folder.autoArchiveSettings",
        json!({"folder": inbox.0, "mode": "default", "age": 6, "unit": "months"}),
    );
    assert_eq!(s.mailbox.folder(inbox).unwrap().auto_archive, None);
    // "Apply these settings to all folders now" clears every folder's own settings.
    let r = run(&mut s, "options.autoArchive", json!({"applyToAll": true}));
    assert_eq!(r["folders"], 3);
    assert!(s.mailbox.folders.values().all(|f| f.auto_archive.is_none()));
}

#[test]
fn folder_settings_dialog_and_validation() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let calendar = role(&s, FolderRole::Calendar);
    let r = run(&mut s, "folder.autoArchiveSettings", json!({"folder": inbox.0}));
    assert_eq!(r["dialog"], "autoArchiveSettings");
    let Some(Notice::OpenDialog(_, p)) = s.notices.last().cloned() else { panic!("no dialog") };
    assert_eq!(p["props"]["mode"], "default");
    assert_eq!(p["props"]["age"], 6);
    assert_eq!(p["defaults"]["olderThanMonths"], 6);
    assert_eq!(p["archivable"], true);
    for bad in [
        json!({"mode": "sometimes"}),
        json!({"age": 0}),
        json!({"age": 5000}),
        json!({"age": "x"}),
        json!({"unit": "years"}),
        json!({"action": "shred"}),
        json!({"target": "No such folder"}),
        json!({"target": calendar.0}),
        json!({"target": inbox.0}),
    ] {
        let mut p = bad.clone();
        p["folder"] = json!(inbox.0);
        assert!(matches!(s.run("folder.autoArchiveSettings", &p), Err(CmdError::Params(_))), "{bad}");
    }
    assert_eq!(s.mailbox.folder(inbox).unwrap().auto_archive, None, "nothing saved by a refused change");
    let outbox = role(&s, FolderRole::Outbox);
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": outbox.0}));
    let Some(Notice::OpenDialog(_, p)) = s.notices.last().cloned() else { panic!("no dialog") };
    assert_eq!(p["archivable"], false);
}

#[test]
fn outbox_and_search_folders_are_left_alone() {
    let mut s = session();
    let outbox = role(&s, FolderRole::Outbox);
    let waiting = mail(&mut s, outbox, 900);
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, waiting).as_deref(), Some("Outbox"));
}

#[test]
fn expired_mail_is_deleted_when_asked() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let sent = role(&s, FolderRole::SentItems);
    let raw = |date: &str| format!("Expiry-Date: {date}\r\nSubject: Offer\r\n\r\nBody\r\n").into_bytes();
    let expiring = |s: &mut Session, folder: FolderId, date: &str| {
        s.mailbox.add_mail(MailItem { folder, received: ago(1), raw: Some(raw(date)), ..Default::default() })
    };
    let expired = expiring(&mut s, inbox, "Mon, 19 Oct 2026 10:00:00 +0000");
    let later = expiring(&mut s, inbox, "Sun, 1 Nov 2026 10:00:00 +0000");
    let in_never = expiring(&mut s, sent, "Mon, 19 Oct 2026 10:00:00 +0000");
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": sent.0, "mode": "never"}));
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(r["expired"], 0, "off by default");
    set(&mut s, "advanced.autoArchive.deleteExpired", json!(true));
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(r["expired"], 1);
    assert!(!s.mailbox.mail.contains_key(&expired));
    assert!(s.mailbox.mail.contains_key(&later));
    assert!(s.mailbox.mail.contains_key(&in_never), "folders that are not archived keep expired mail");
}

#[test]
fn do_not_autoarchive_items_stay_unless_included() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let keep = mail(&mut s, inbox, 400);
    s.selection.items = vec![keep];
    s.selection.focus = Some(keep);
    let r = run(&mut s, "mail.doNotAutoArchive", json!({"on": true}));
    assert_eq!(r["changed"], 1);
    assert!(s.mailbox.mail[&keep].no_auto_archive);
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, keep).as_deref(), Some("Inbox"));
    // The Archive dialog can include them.
    run(&mut s, "file.archive", json!({"folder": inbox.0, "before": "2026-10-01"}));
    assert_eq!(folder_of(&s, keep).as_deref(), Some("Inbox"));
    run(&mut s, "file.archive", json!({"folder": inbox.0, "before": "2026-10-01", "includeExcluded": true}));
    assert_eq!(folder_of(&s, keep).as_deref(), Some("Archives\\Inbox"));
    assert!(matches!(s.run("mail.doNotAutoArchive", &json!({"ids": [999_999]})), Err(CmdError::Disabled(_))));
}

#[test]
fn archive_dialog_runs_one_tree_with_one_date() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let sent = role(&s, FolderRole::SentItems);
    let sub = s.mailbox.create_folder(inbox, "Sub").unwrap();
    let a = mail(&mut s, inbox, 30);
    let b = mail(&mut s, sub, 30);
    let young = mail(&mut s, sub, 5);
    let other = mail(&mut s, sent, 900);
    let r = run(&mut s, "file.archive", json!({}));
    assert_eq!(r["dialog"], "archive");
    let Some(Notice::OpenDialog(_, p)) = s.notices.last().cloned() else { panic!("no dialog") };
    assert_eq!(p["before"], "2026-04-20", "six months back by default");
    assert_eq!(p["defaultTarget"], "Archives");
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": sub.0, "mode": "never"}));
    let r = run(&mut s, "file.archive", json!({"folder": inbox.0, "before": "2026-10-10"}));
    assert_eq!(r["moved"], 2, "{r}");
    assert_eq!(folder_of(&s, a).as_deref(), Some("Archives\\Inbox"));
    assert_eq!(
        folder_of(&s, b).as_deref(),
        Some("Archives\\Inbox\\Sub"),
        "a manual archive takes every subfolder"
    );
    assert_eq!(folder_of(&s, young).as_deref(), Some("Inbox\\Sub"));
    assert_eq!(folder_of(&s, other).as_deref(), Some("Sent Items"), "outside the tree");
    // Into a folder of choice.
    let target = s.mailbox.create_folder(sent, "Old").unwrap();
    run(&mut s, "file.archive", json!({"folder": sent.0, "before": "2026-10-10", "target": target.0}));
    assert_eq!(folder_of(&s, other).as_deref(), Some("Sent Items\\Old\\Sent Items"));
    for bad in [json!("yesterday"), json!("2026-13-01"), json!("2026-02-30"), json!(true)] {
        assert!(matches!(s.run("file.archive", &json!({"before": bad})), Err(CmdError::Params(_))), "{bad}");
    }
}

#[test]
fn a_run_stops_at_its_limit_and_the_rest_waits() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    for _ in 0..5 {
        mail(&mut s, inbox, 400);
    }
    let plan = Plan { limit: 3, ..Plan::all() };
    let r = archive(&mut s, &plan).unwrap();
    assert_eq!((r.moved, r.remaining), (3, true));
    let r = archive(&mut s, &plan).unwrap();
    assert_eq!((r.moved, r.remaining), (2, false));
}

#[test]
fn schedule_is_due_every_n_days_and_scheduled_runs_leave_no_undo_step() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let old = mail(&mut s, inbox, 400);
    assert!(!poll(&mut s).enabled);
    assert_eq!(schedule(&s).last_run, None, "off: the clock does not start");
    run(&mut s, "options.autoArchive", json!({"run": true, "everyDays": 14}));
    let sch = schedule(&s);
    assert_eq!(sch.last_run, Some(now_unix()), "turning it on starts the clock");
    assert!(!sch.due);
    assert_eq!(sch.next_run, Some(now_unix() + 14 * DAY));
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 13 * DAY, 0));
    assert!(!poll(&mut s).due);
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 14 * DAY, 0));
    assert!(poll(&mut s).due);
    let status = run(&mut s, "file.autoArchiveStatus", json!({}));
    assert_eq!(status["due"], true);
    let undo_before = s.undo.undo.len();
    let r = run_scheduled(&mut s).unwrap();
    assert_eq!(r["moved"], 1);
    assert_eq!(folder_of(&s, old).as_deref(), Some("Archives\\Inbox"));
    assert_eq!(s.undo.undo.len(), undo_before, "an automatic run records no undo step");
    let sch = schedule(&s);
    assert_eq!(sch.last_run, Some(now_unix() + 14 * DAY));
    assert!(!sch.due);
    // Not now: skips this run.
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 30 * DAY, 0));
    assert!(poll(&mut s).due);
    run(&mut s, "file.autoArchivePostpone", json!({}));
    assert!(!poll(&mut s).due);
    // Every 1 day.
    run(&mut s, "options.autoArchive", json!({"everyDays": 1}));
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 31 * DAY, 0));
    assert!(poll(&mut s).due);
    // A clock that went back restarts it.
    s.settings.set(LAST_RUN_KEY, &json!(now_unix() + 400 * DAY)).unwrap();
    assert!(!poll(&mut s).due);
    assert_eq!(schedule(&s).last_run, Some(now_unix() + 31 * DAY));
    // Off: never due.
    run(&mut s, "options.autoArchive", json!({"run": false}));
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 90 * DAY, 0));
    assert!(!poll(&mut s).due);
}

#[test]
fn scheduled_runs_queue_server_changes() {
    use mailcraft_model::ServerSettings;
    use mailcraft_model::account::RemoteOp;
    let mut s = session();
    let acct = s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        display_name: "user@example.com".into(),
        address: "user@example.com".into(),
        incoming: Some(ServerSettings { host: "imap.example.com".into(), port: 993, ..Default::default() }),
        ..Default::default()
    });
    s.mailbox.create_default_folders(acct);
    let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
    let id = s.mailbox.add_mail(MailItem {
        folder: inbox,
        received: ago(400),
        remote_id: Some("42".into()),
        body: mailcraft_model::item::Body { text: Some("Downloaded in full".into()), ..Default::default() },
        ..Default::default()
    });
    run(&mut s, "options.autoArchive", json!({"run": true}));
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 15 * DAY, 0));
    assert!(poll(&mut s).due);
    run_scheduled(&mut s).unwrap();
    assert_eq!(folder_of(&s, id).as_deref(), Some("Archives\\Inbox"));
    let pending = &s.mailbox.account(acct).unwrap().sync.pending;
    assert!(
        pending.iter().any(|p| matches!(&p.op, RemoteOp::Expunge { folder, uid: 42 } if folder == "INBOX")),
        "{pending:?}"
    );
}

/// An IMAP account with a synchronised Inbox (`INBOX`).
fn imap_inbox(s: &mut Session) -> (AccountId, FolderId) {
    use mailcraft_model::ServerSettings;
    let acct = s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        display_name: "user@example.com".into(),
        address: "user@example.com".into(),
        incoming: Some(ServerSettings { host: "imap.example.com".into(), port: 993, ..Default::default() }),
        ..Default::default()
    });
    s.mailbox.create_default_folders(acct);
    let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
    (acct, inbox)
}

/// Old server mail with UID `uid`, downloaded in full (`full`) or as headers only.
fn server_mail(s: &mut Session, folder: FolderId, uid: usize, full: bool) -> ItemId {
    let text = full.then(|| "Downloaded in full".to_owned());
    s.mailbox.add_mail(MailItem {
        folder,
        received: ago(400),
        remote_id: Some(uid.to_string()),
        body: mailcraft_model::item::Body { text, ..Default::default() },
        ..Default::default()
    })
}

fn expunges(s: &Session, acct: AccountId) -> usize {
    use mailcraft_model::account::RemoteOp;
    let pending = &s.mailbox.account(acct).unwrap().sync.pending;
    pending.iter().filter(|p| matches!(p.op, RemoteOp::Expunge { .. })).count()
}

#[test]
fn header_only_server_mail_is_not_moved_out_of_the_server() {
    // Regression: moving a header-only IMAP message into the local archive expunged the server copy, the only
    // place its body and attachments were.
    let mut s = session();
    let (acct, inbox) = imap_inbox(&mut s);
    let head = server_mail(&mut s, inbox, 7, false);
    s.sync.prefs.data.headers_only.insert(head.0);
    let bare = server_mail(&mut s, inbox, 8, false);
    let full = server_mail(&mut s, inbox, 9, true);
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!((r["moved"].as_u64(), r["headerOnly"].as_u64()), (Some(1), Some(2)), "{r}");
    assert_eq!(folder_of(&s, head).as_deref(), Some("Inbox"));
    assert_eq!(folder_of(&s, bare).as_deref(), Some("Inbox"), "no body at all counts as headers only");
    assert_eq!(folder_of(&s, full).as_deref(), Some("Archives\\Inbox"));
    assert_eq!(expunges(&s, acct), 1, "only the downloaded message leaves the server");
    let text = s
        .notices
        .iter()
        .rev()
        .find_map(|n| match n {
            Notice::Info(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("2 messages with only their headers"), "{text}");
    // Downloaded later, it goes on the next run.
    s.mailbox.mail.get_mut(&head).unwrap().body.text = Some("Now in full".into());
    s.sync.prefs.data.headers_only.remove(&head.0);
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, head).as_deref(), Some("Archives\\Inbox"));
    // A move to another folder of the same account keeps the server copy, so header-only mail may go.
    let keep = FolderId(s.mailbox.alloc_id());
    s.mailbox.folders.insert(
        keep,
        Folder {
            id: keep,
            account: acct,
            name: "Old".into(),
            kind: FolderKind::Mail,
            remote_name: Some("Old".into()),
            ..Default::default()
        },
    );
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": inbox.0, "mode": "custom", "target": keep.0}));
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(s.mailbox.mail[&bare].folder, keep);
    // Permanent delete was asked for: headers only or not, it goes.
    let gone = server_mail(&mut s, inbox, 10, false);
    run(&mut s, "folder.autoArchiveSettings", json!({"folder": inbox.0, "action": "delete", "target": null}));
    run(&mut s, "file.archive", json!({"all": true}));
    assert!(!s.mailbox.mail.contains_key(&gone));
}

#[test]
fn scheduled_steps_never_overflow_the_server_change_queue() {
    // Regression: a backlog larger than the server change queue (MAX_PENDING) dropped queued changes, the
    // user's own first, when steps ran back to back without send/receive in between.
    use mailcraft_model::account::{PendingOp, RemoteOp};
    let mut s = session();
    let (acct, inbox) = imap_inbox(&mut s);
    let backlog = crate::sync::MAX_PENDING + 1_000;
    let ids: Vec<ItemId> = (1..=backlog).map(|uid| server_mail(&mut s, inbox, uid, true)).collect();
    // The user's own change waits in the queue.
    let mine = RemoteOp::Expunge { folder: "Sent".into(), uid: 999_999 };
    let op = PendingOp { id: 0, op: mine.clone(), attempts: 0, not_before: 0, last_error: None };
    s.mailbox.accounts.iter_mut().find(|a| a.id == acct).unwrap().sync.pending.push(op);
    run(&mut s, "options.autoArchive", json!({"run": true}));
    s.fixed_now = Some(DateTime::from_unix(now_unix() + 15 * DAY, 0));
    assert!(poll(&mut s).due);
    let mut sent = 0usize;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps < 20, "the backlog goes through in a bounded number of steps");
        let r = run_scheduled(&mut s).unwrap();
        let pending = &s.mailbox.account(acct).unwrap().sync.pending;
        assert!(pending.len() <= QUEUE_ROOM, "a step fills the queue only to QUEUE_ROOM: {}", pending.len());
        assert_eq!(pending.first().map(|p| &p.op), Some(&mine), "the user's change is never dropped");
        if r["remaining"] != true {
            sent += expunges(&s, acct) - 1;
            break;
        }
        // Until send/receive works the queue off, a step does nothing.
        let again = run_scheduled(&mut s).unwrap();
        assert_eq!((again["moved"].as_u64(), again["remaining"].as_bool()), (Some(0), Some(true)), "{again}");
        // Send/receive works the queue off (the user's change still retrying).
        sent += expunges(&s, acct) - 1;
        s.mailbox.accounts.iter_mut().find(|a| a.id == acct).unwrap().sync.pending.truncate(1);
    }
    assert_eq!(sent, backlog, "every archived message's server copy is expunged once");
    assert!(ids.iter().all(|id| folder_of(&s, *id).as_deref() == Some("Archives\\Inbox")));
    assert!(!schedule(&s).due, "the run is recorded once the backlog is through");
}

#[test]
fn expired_mail_is_found_in_the_store_after_a_restart() {
    // Regression: the expiry date was read from the in-memory source only; mail loaded from the data file
    // keeps its source in a blob, so nothing expired after a restart.
    let dir = tempfile::tempdir().unwrap();
    let raw = b"Expiry-Date: Mon, 19 Oct 2026 10:00:00 +0000\r\nSubject: Offer\r\n\r\nBody\r\n".to_vec();
    let id = {
        let mut s = Session::open(dir.path()).unwrap();
        let inbox = role(&s, FolderRole::Inbox);
        let item = MailItem { folder: inbox, received: ago(1), raw: Some(raw), ..Default::default() };
        let id = s.mailbox.add_mail(item);
        let mb = s.mailbox.clone();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(mb))]);
        id
    };
    let mut s = Session::open(dir.path()).unwrap();
    s.fixed_now = Some(DateTime::from_unix(now_unix(), 0));
    let m = &s.mailbox.mail[&id];
    assert!(m.raw.is_none() && m.blob.is_some(), "the source is in a blob");
    set(&mut s, "advanced.autoArchive.deleteExpired", json!(true));
    let r = run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(r["expired"], 1, "{r}");
    assert!(!s.mailbox.mail.contains_key(&id));
}

#[test]
fn options_dialog_and_validation() {
    let mut s = session();
    let r = run(&mut s, "options.autoArchive", json!({}));
    assert_eq!(r["dialog"], "autoArchiveOptions");
    let Some(Notice::OpenDialog(_, p)) = s.notices.last().cloned() else { panic!("no dialog") };
    assert_eq!((p["everyDays"].as_u64(), p["olderThanMonths"].as_u64()), (Some(14), Some(6)));
    assert_eq!(p["action"], "move");
    // One bad value refuses the whole change.
    let bad = s.run("options.autoArchive", &json!({"prompt": true, "everyDays": 0}));
    assert!(bad.is_err());
    assert!(!s.settings.bool("advanced.autoArchive.prompt"));
    run(&mut s, "options.autoArchive", json!({"prompt": true, "action": "delete", "olderThanMonths": 3}));
    let d = defaults(&s);
    assert!(d.prompt);
    assert_eq!((d.action, d.older_than_months), (ArchiveAction::Delete, 3));
    let status = run(&mut s, "file.autoArchiveStatus", json!({}));
    assert_eq!(status["prompt"], true);
}

#[test]
fn flagged_mail_is_archived_like_any_other() {
    // Follow-up flags do not hold mail back (only "Do not AutoArchive" does).
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    let id = mail(&mut s, inbox, 400);
    s.mailbox.mail.get_mut(&id).unwrap().flag =
        FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
    run(&mut s, "file.archive", json!({"all": true}));
    assert_eq!(folder_of(&s, id).as_deref(), Some("Archives\\Inbox"));
}
