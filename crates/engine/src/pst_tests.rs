#![cfg(not(target_arch = "wasm32"))]
//! PST import through the commands, against synthetic files (`mailcraft_testmail::pst`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::sample::sample_session;
use mailcraft_model::FolderKind;
use mailcraft_testmail::pst::{
    ItemDef, Key, PS_PUBLIC_STRINGS, PSETID_APPOINTMENT, PSETID_NOTE, PSETID_TASK, PstBuilder, Value as P,
};

const T0: i64 = 1_772_442_000;

fn u(s: &str) -> P {
    P::Unicode(s.to_owned())
}

fn mail(subject: &str, id: &str) -> ItemDef {
    ItemDef::new("IPM.Note")
        .tag(0x0037, u(subject))
        .tag(0x0042, u("Ada Example"))
        .tag(0x5D02, u("ada@example.com"))
        .tag(0x0E06, P::Time(T0))
        .tag(0x1035, u(id))
        .tag(0x1000, u(&format!("Text of {subject}.")))
        .recipient(1, "Bo Example", "bo@example.org")
}

fn fixture() -> PstBuilder {
    let mut b = PstBuilder::new("Old mail");
    let inbox = b.folder(None, "Inbox", "IPF.Note");
    let projects = b.folder(Some(inbox), "Projects", "IPF.Note");
    let travel = b.folder(None, "Travel", "IPF.Note");
    let cal = b.folder(None, "Calendar", "IPF.Appointment");
    let contacts = b.folder(None, "Contacts", "IPF.Contact");
    let tasks = b.folder(None, "Tasks", "IPF.Task");
    let notes = b.folder(None, "Notes", "IPF.StickyNote");
    b.item(
        inbox,
        mail("Imported one", "<pst-1@example.com>")
            .tag(0x0E07, P::I32(1))
            .with(Key::Name(PS_PUBLIC_STRINGS, "Keywords".into()), P::MvUnicode(vec!["Green".into()]))
            .attach("note.txt", "text/plain", b"attached text"),
    );
    b.item(inbox, mail("Imported two", "<pst-2@example.com>"));
    b.item(projects, mail("Project plan", "<pst-3@example.com>"));
    b.item(travel, mail("Tickets", "<pst-4@example.com>"));
    b.item(
        cal,
        ItemDef::new("IPM.Appointment")
            .tag(0x0037, u("Dentist"))
            .with(Key::Lid(PSETID_APPOINTMENT, 0x820D), P::Time(T0 + 86_400))
            .with(Key::Lid(PSETID_APPOINTMENT, 0x820E), P::Time(T0 + 86_400 + 1800)),
    );
    b.item(contacts, ItemDef::new("IPM.Contact").tag(0x3A06, u("Cy")).tag(0x3A11, u("Example")));
    b.item(
        tasks,
        ItemDef::new("IPM.Task").tag(0x0037, u("File taxes")).with(Key::Lid(PSETID_TASK, 0x8101), P::I32(2)),
    );
    b.item(
        notes,
        ItemDef::new("IPM.StickyNote")
            .tag(0x1000, u("Call back"))
            .with(Key::Lid(PSETID_NOTE, 0x8B00), P::I32(0)),
    );
    b
}

fn write(dir: &Path, name: &str, b: &PstBuilder) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b.build().unwrap()).unwrap();
    path
}

fn folder_named(s: &Session, account: AccountId, parent: Option<FolderId>, name: &str) -> Option<FolderId> {
    s.mailbox
        .folders
        .values()
        .find(|f| f.account == account && f.parent == parent && f.name == name)
        .map(|f| f.id)
}

fn subjects(s: &Session, folder: FolderId) -> Vec<String> {
    let mut v: Vec<String> = s.mailbox.items_in(folder).iter().map(|m| m.subject.clone()).collect();
    v.sort();
    v
}

fn finish(s: &mut Session) -> Value {
    for _ in 0..10_000 {
        s.poll_sync();
        if !running(s) {
            return status(s).unwrap();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the import did not finish");
}

#[test]
fn import_merges_default_folders_and_files_every_kind_of_item() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "old.pst", &fixture());
    let mut s = sample_session();
    let account = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let before = s.mailbox.items_in(inbox).len();
    let r = s
        .run(
            "file.openExport",
            &json!({"action": "import", "path": path.to_str().unwrap(), "folder": inbox.0, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["running"], false);
    assert_eq!(r["imported"], 8, "{r}");
    assert_eq!(r["counts"]["mail"], 4);
    assert_eq!(r["counts"]["appointments"], 1);
    assert_eq!(r["counts"]["contacts"], 1);
    assert_eq!(r["counts"]["tasks"], 1);
    assert_eq!(r["counts"]["notes"], 1);
    assert_eq!(r["skippedCount"], 0, "{r}");
    // The PST Inbox went into the account's Inbox; its subfolder and the other mail folder were made.
    assert_eq!(s.mailbox.items_in(inbox).len(), before + 2);
    let projects = folder_named(&s, account, Some(inbox), "Projects").unwrap();
    // The sample Inbox has a Projects folder already: the PST one merges into it.
    assert!(subjects(&s, projects).contains(&"Project plan".to_owned()));
    let travel = folder_named(&s, account, None, "Travel").unwrap();
    assert_eq!(s.mailbox.folder(travel).unwrap().kind, FolderKind::Mail);
    assert_eq!(subjects(&s, travel), ["Tickets"]);
    let one = s.mailbox.items_in(inbox).into_iter().find(|m| m.subject == "Imported one").cloned().unwrap();
    assert!(one.read);
    assert_eq!(one.categories, ["Green"]);
    assert_eq!(one.attachments.len(), 1);
    let body = s.body(one.id).unwrap().unwrap();
    assert!(body.text.as_deref().unwrap().contains("Text of Imported one."));
    let raw = s.source(one.id).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Subject: Imported one"));
    // PIM items went to the account's default folders.
    let cal = s.mailbox.role_folder(account, FolderRole::Calendar).unwrap();
    assert!(
        s.mailbox
            .appointments
            .values()
            .any(|a| a.folder == cal && a.subject == "Dentist" && a.start.unix == T0 + 86_400)
    );
    let contacts = s.mailbox.role_folder(account, FolderRole::Contacts).unwrap();
    assert!(s.mailbox.contacts.values().any(|c| c.folder == contacts && c.first_name == "Cy"));
    let tasks = s.mailbox.role_folder(account, FolderRole::Tasks).unwrap();
    assert!(
        s.mailbox.tasks.values().any(|t| t.folder == tasks && t.subject == "File taxes" && t.is_complete())
    );
    let notes = s.mailbox.role_folder(account, FolderRole::Notes).unwrap();
    assert!(s.mailbox.notes.values().any(|n| n.folder == notes && n.text == "Call back"));
    let text = summary(&r);
    assert!(text.contains("4 messages") && text.contains("old.pst"), "{text}");

    // Running it again leaves the items already there out; with duplicates allowed they come in again.
    // Regression: only mail was checked, so a second run filed every calendar item, contact, task and note
    // again.
    let pim = |s: &Session| {
        (s.mailbox.appointments.len(), s.mailbox.contacts.len(), s.mailbox.tasks.len(), s.mailbox.notes.len())
    };
    let pim_before = pim(&s);
    let r = s
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["imported"], 0, "{r}");
    assert_eq!(r["duplicates"], 8, "{r}");
    assert_eq!(s.mailbox.items_in(inbox).len(), before + 2);
    assert_eq!(pim(&s), pim_before);
    let r = s
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0, "duplicates": "allow", "wait": true}),
        )
        .unwrap();
    assert_eq!(r["counts"]["mail"], 4);
    assert_eq!(r["imported"], 8, "{r}");
    assert_eq!(s.mailbox.items_in(inbox).len(), before + 4);
}

/// Regression: drafts and other mail without a Message-ID, and calendar items, contacts, tasks and notes,
/// came in again on every run of the same file; items that only look alike stay apart.
#[test]
fn running_an_import_again_adds_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = PstBuilder::new("Again");
    let drafts = b.folder(None, "Drafts", "IPF.Note");
    let cal = b.folder(None, "Calendar", "IPF.Appointment");
    let contacts = b.folder(None, "Contacts", "IPF.Contact");
    let draft = |text: &str| {
        ItemDef::new("IPM.Note")
            .tag(0x0037, u("Unfinished"))
            .tag(0x0E07, P::I32(8)) // mfUnsent
            .tag(0x1000, u(text))
    };
    b.item(drafts, draft("First draft text."));
    b.item(drafts, draft("Second draft, other text."));
    let appt = |subject: &str, start: i64| {
        ItemDef::new("IPM.Appointment")
            .tag(0x0037, u(subject))
            .with(Key::Lid(PSETID_APPOINTMENT, 0x820D), P::Time(start))
            .with(Key::Lid(PSETID_APPOINTMENT, 0x820E), P::Time(start + 1800))
    };
    b.item(cal, appt("Standup", T0));
    b.item(cal, appt("Standup", T0 + 86_400));
    b.item(contacts, ItemDef::new("IPM.Contact").tag(0x3A06, u("Di")).tag(0x3A11, u("Example")));
    b.item(contacts, ItemDef::new("IPM.Contact").tag(0x3A06, u("Ed")).tag(0x3A11, u("Example")));
    let path = write(dir.path(), "again.pst", &b);
    let mut s = sample_session();
    let account = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let run = |s: &mut Session| {
        s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0, "wait": true}),
        )
        .unwrap()
    };
    let first = run(&mut s);
    assert_eq!(first["imported"], 6, "{first}");
    assert_eq!(first["duplicates"], 0, "look-alikes are not repeats: {first}");
    let drafts = s.mailbox.role_folder(account, FolderRole::Drafts).unwrap();
    let in_drafts = s.mailbox.items_in(drafts).len();
    let counts = (s.mailbox.appointments.len(), s.mailbox.contacts.len());
    let again = run(&mut s);
    assert_eq!(again["imported"], 0, "{again}");
    assert_eq!(again["duplicates"], 6, "{again}");
    assert_eq!(s.mailbox.items_in(drafts).len(), in_drafts);
    assert_eq!((s.mailbox.appointments.len(), s.mailbox.contacts.len()), counts);
}

/// An IMAP account with a synchronised Inbox (`INBOX`).
fn imap_account(s: &mut Session) -> (AccountId, FolderId) {
    use mailcraft_model::{Account, AccountKind, ServerSettings};
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

fn server_ops(s: &Session, acct: AccountId) -> (usize, usize) {
    use mailcraft_model::account::RemoteOp;
    let pending = &s.mailbox.account(acct).unwrap().sync.pending;
    let appends = pending.iter().filter(|p| matches!(p.op, RemoteOp::Append { .. })).count();
    let creates = pending.iter().filter(|p| matches!(p.op, RemoteOp::CreateFolder { .. })).count();
    (appends, creates)
}

/// Regression: an import in the background (the wizard's way) put mail and folders into an IMAP account's
/// synchronised folders without queueing them for the server; the same import with `wait` did.
#[test]
fn a_background_import_into_a_server_account_is_queued_for_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "old.pst", &fixture());
    let mut background = sample_session();
    let (acct, inbox) = imap_account(&mut background);
    let r = background
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0}),
        )
        .unwrap();
    assert_eq!(r["started"], true);
    let st = finish(&mut background);
    assert_eq!(st["counts"]["mail"], 4, "{st}");
    let (appends, creates) = server_ops(&background, acct);
    // Two into INBOX, one into the new Inbox/Projects, one into the new Travel.
    assert_eq!(appends, 4);
    assert!(creates >= 2, "{creates}");

    let mut waited = sample_session();
    let (acct, inbox) = imap_account(&mut waited);
    waited
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0, "wait": true}),
        )
        .unwrap();
    assert_eq!(server_ops(&waited, acct), (appends, creates), "both ways queue the same");
}

/// A stand-in reader process: a shell script.
#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A job around a reader process started from `exe`.
#[cfg(unix)]
fn process_job(exe: &Path, pst: &Path) -> Job {
    let control = std::sync::Arc::new(mailcraft_formats::pst::Control::default());
    let child = std::sync::Arc::new(std::sync::Mutex::new(None));
    let rx = spawn_process(exe, pst, mailcraft_formats::pst::Options::default(), &child, &control).unwrap();
    Job {
        id: 1,
        path: pst.to_path_buf(),
        target: Target::Account(AccountId(1)),
        account: AccountId(1),
        new_data_file: false,
        skip_duplicates: true,
        rx,
        control,
        child,
        progress: Progress::default(),
        folders: Vec::new(),
        imported: Counts::default(),
        duplicates: 0,
        seen: Default::default(),
        notes: Vec::new(),
        notes_count: 0,
    }
}

/// Regression: Stop (or the end of `timeoutSecs`) kills the reader process, and its reading thread took the
/// killed process for one that died on damaged data ("the PST reader stopped on damaged data (signal: 9)",
/// or "sent something unreadable" when the kill cut a line), so a stop was reported as an error.
#[cfg(unix)]
#[test]
fn stopping_a_reader_process_is_a_stop_not_damage() {
    let dir = tempfile::tempdir().unwrap();
    let pst = write(dir.path(), "x.pst", &fixture());
    let progress =
        r#"{"t":"progress","items_done":3,"items_total":9,"folders":2,"bytes_read":10,"file_bytes":20}"#;
    for (name, body) in [
        ("idle.sh", format!("echo '{progress}'\nexec sleep 60")),
        // Killed in the middle of a line.
        ("midline.sh", format!("echo '{progress}'\nprintf '{{\"t\":\"fol'\nexec sleep 60")),
    ] {
        let exe = script(dir.path(), name, &body);
        let job = process_job(&exe, &pst);
        match job.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Progress(p) => assert_eq!(p.folders, 2),
            other => panic!("{other:?}"),
        }
        cancel_job(&job);
        match job.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Done(Ok(r)) => {
                assert_eq!(r.stopped.as_deref(), Some("cancelled"), "{name}");
                assert_eq!(r.folders, 2);
            }
            other => panic!("{name}: {other:?}"),
        }
        let st = json!({"path": "x.pst", "stopped": "cancelled", "counts": {"mail": 3}});
        assert!(summary(&st).starts_with("Import of x.pst stopped."), "{}", summary(&st));
    }
}

/// Regression: after a line that was too large or unreadable the reading thread waited for the reader
/// process without stopping it; the process, still writing, blocked on the full pipe, and the thread and the
/// process were left behind until the app ended.
#[cfg(unix)]
#[test]
fn a_reader_process_that_sends_a_bad_line_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let pst = write(dir.path(), "x.pst", &fixture());
    let exe = script(dir.path(), "bad.sh", "echo 'not a result line'\nexec sleep 60");
    let job = process_job(&exe, &pst);
    let started = std::time::Instant::now();
    match job.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Event::Done(Err(e)) => assert!(e.contains("unreadable"), "{e}"),
        other => panic!("{other:?}"),
    }
    // The thread ends (its sender goes) long before the process would have ended by itself.
    assert!(matches!(
        job.rx.recv_timeout(Duration::from_secs(30)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(started.elapsed() < Duration::from_secs(45), "{:?}", started.elapsed());
    assert!(job.child.lock().unwrap().is_none(), "the process was taken and waited for");
}

/// The progress window opens again from the command (the status bar and Open & Export use it after Hide),
/// and on its own at the end when something was left out.
#[test]
fn the_progress_window_can_be_shown_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = fixture();
    b.item(0, ItemDef::new("IPM.DistList").tag(0x0037, u("A contact group")));
    let path = write(dir.path(), "old.pst", &b);
    let mut s = sample_session();
    let opened = |s: &Session| {
        s.notices
            .iter()
            .filter(|n| matches!(n, crate::Notice::OpenDialog(d, _) if d == "pstProgress"))
            .count()
    };
    s.run("file.openExport", &json!({"action": "importPst", "path": path.to_str().unwrap()})).unwrap();
    assert_eq!(opened(&s), 1);
    s.notices.clear();
    let st = s.run("file.importProgress", &json!({})).unwrap();
    assert_eq!(opened(&s), 0, "a plain status query opens nothing");
    let job = st["job"].clone();
    s.run("file.importProgress", &json!({"show": true})).unwrap();
    assert!(
        s.notices
            .iter()
            .any(|n| matches!(n, crate::Notice::OpenDialog(d, p) if d == "pstProgress" && p["job"] == job))
    );
    s.notices.clear();
    let st = finish(&mut s);
    assert_eq!(st["skippedCount"], 1, "{st}");
    assert_eq!(opened(&s), 1, "the report opens when something was left out");
    s.notices.clear();
    // After the end the report stays available.
    let last = s.run("file.importProgress", &json!({"show": true})).unwrap();
    assert_eq!(last["running"], false);
    assert_eq!(opened(&s), 1);
}

#[test]
fn import_beneath_a_folder_keeps_the_tree_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "old.pst", &fixture());
    let mut s = sample_session();
    let account = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let r = s
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "folder": inbox.0, "into": "beneath", "wait": true}),
        )
        .unwrap();
    assert_eq!(r["imported"], 8, "{r}");
    let pst_inbox = folder_named(&s, account, Some(inbox), "Inbox").unwrap();
    assert_eq!(subjects(&s, pst_inbox), ["Imported one", "Imported two"]);
    let projects = folder_named(&s, account, Some(pst_inbox), "Projects").unwrap();
    assert_eq!(subjects(&s, projects), ["Project plan"]);
    let cal = folder_named(&s, account, Some(inbox), "Calendar").unwrap();
    assert_eq!(s.mailbox.folder(cal).unwrap().kind, FolderKind::Calendar);
    assert!(s.mailbox.appointments.values().any(|a| a.folder == cal));
    assert!(
        s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "into": "sideways"})
        )
        .is_err()
    );
    assert!(
        s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "duplicates": "maybe"})
        )
        .is_err()
    );
}

#[test]
fn open_data_file_with_a_pst_file_makes_a_data_file_that_closes_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "Archive 2019.pst", &fixture());
    let mut s = sample_session();
    let accounts = s.mailbox.accounts.len();
    let r = s.run("file.openDataFile", &json!({"path": path.to_str().unwrap()})).unwrap();
    assert_eq!(r["started"], true);
    let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    assert_eq!(r["name"], "Archive 2019");
    assert_eq!(s.mailbox.accounts.len(), accounts + 1);
    // While it fills, the data file cannot be removed, and a second import waits.
    assert!(matches!(
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})),
        Err(CmdError::Disabled(_))
    ));
    assert!(matches!(
        s.run("file.openExport", &json!({"action": "importPst", "path": path.to_str().unwrap()})),
        Err(CmdError::Disabled(_))
    ));
    let progress = s.run("file.importProgress", &json!({})).unwrap();
    assert_eq!(progress["running"], true);
    assert_eq!(progress["newDataFile"], true);
    let st = finish(&mut s);
    assert_eq!(st["imported"], 8, "{st}");
    let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
    assert_eq!(subjects(&s, inbox), ["Imported one", "Imported two"]);
    assert!(folder_named(&s, id, None, "Travel").is_some());
    assert!(
        s.notices.iter().any(|n| matches!(n, crate::Notice::Info(t) if t.contains("from Archive 2019.pst")))
    );
    s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
    assert_eq!(s.mailbox.accounts.len(), accounts);
    let after = s.run("file.importProgress", &json!({})).unwrap();
    assert_eq!(after["running"], false);
}

#[test]
fn progress_and_cancel_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = fixture();
    for i in 0..300 {
        b.item(0, mail(&format!("Bulk {i}"), &format!("<bulk-{i}@example.com>")));
    }
    let path = write(dir.path(), "big.pst", &b);
    let mut s = sample_session();
    assert_eq!(s.run("file.importProgress", &json!({})).unwrap(), json!({"running": false}));
    let r =
        s.run("file.openExport", &json!({"action": "importPst", "path": path.to_str().unwrap()})).unwrap();
    assert_eq!(r["started"], true);
    let r = s.run("file.importProgress", &json!({"cancel": true})).unwrap();
    assert_eq!(r["stopping"], true);
    let st = finish(&mut s);
    // Stopped early (or, on a fast machine, finished just before the stop): never more than the file holds.
    let n = st["imported"].as_u64().unwrap();
    assert!(n <= 308, "{st}");
    if n < 308 {
        assert_eq!(st["stopped"], "cancelled", "{st}");
        assert!(summary(&st).contains("stopped"));
    }
    // Run again: every item (mail, calendar item, contact, task, note) comes in once only.
    let rerun = |s: &mut Session| {
        s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": path.to_str().unwrap(), "wait": true}),
        )
        .unwrap()
    };
    let r = rerun(&mut s);
    assert_eq!(r["duplicates"].as_u64(), Some(n), "{r}");
    assert_eq!(n + r["imported"].as_u64().unwrap(), 308, "{r}");
    let r = rerun(&mut s);
    assert_eq!(r["imported"], 0, "{r}");
    assert_eq!(r["duplicates"], 308, "{r}");
}

#[test]
fn damaged_protected_and_foreign_files_never_crash_and_say_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = sample_session();
    let mail_before = s.mailbox.mail.len();
    // Not a PST file at all: refused before anything starts.
    let other = dir.path().join("notes.pst");
    std::fs::write(&other, b"hello").unwrap();
    let e = s
        .run("file.openExport", &json!({"action": "importPst", "path": other.to_str().unwrap()}))
        .unwrap_err();
    assert!(e.to_string().contains("not a PST file"), "{e}");
    assert!(
        s.run("file.openExport", &json!({"action": "importPst", "path": dir.path().to_str().unwrap()}))
            .is_err()
    );
    // Password-protected: the import ends with a clear error and changes nothing.
    let mut b = fixture();
    b.store_props.push((Key::Tag(0x67FF), P::I32(0x0BAD_F00D)));
    let locked = write(dir.path(), "locked.pst", &b);
    let r = s
        .run(
            "file.openExport",
            &json!({"action": "importPst", "path": locked.to_str().unwrap(), "wait": true}),
        )
        .unwrap();
    assert!(r["error"].as_str().unwrap().contains("password"), "{r}");
    assert_eq!(s.mailbox.mail.len(), mail_before);
    // Truncated and corrupted copies: each import ends (with or without an error), nothing panics.
    let good = fixture().build().unwrap();
    for (i, cut) in [100usize, 600, 0x4500, good.len() / 2, good.len() - 700].into_iter().enumerate() {
        let p = dir.path().join(format!("cut{i}.pst"));
        std::fs::write(&p, &good[..cut]).unwrap();
        let r = s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": p.to_str().unwrap(), "wait": true}),
        );
        if let Ok(r) = r {
            assert_eq!(r["running"], false);
        }
    }
    let mut seed = 0x1234_5678_9abc_def1u64;
    for i in 0..40 {
        let mut f = good.clone();
        for _ in 0..8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = 0x4600 + (seed as usize) % (f.len() - 0x4600);
            f[at] = (seed >> 40) as u8;
        }
        let p = dir.path().join(format!("bad{i}.pst"));
        std::fs::write(&p, &f).unwrap();
        let r = s.run(
            "file.openExport",
            &json!({"action": "importPst", "path": p.to_str().unwrap(), "wait": true, "duplicates": "allow"}),
        );
        if let Ok(r) = r {
            assert_eq!(r["running"], false);
        }
    }
    assert!(!running(&s));
}

#[test]
fn reader_process_lines_round_trip() {
    let item = mailcraft_formats::pst::PstItem::Mail(Box::new((
        MailItem { subject: "Wire".into(), ..Default::default() },
        mailcraft_store::Content { source: Some(b"Subject: Wire\r\n\r\nbody".to_vec()), body: None },
    )));
    let line = serde_json::to_string(&Wire::of_item(3, item.clone())).unwrap();
    let back: Wire = serde_json::from_str(&line).unwrap();
    match back.into_event().unwrap() {
        Event::Item(3, got) => assert_eq!(got, item),
        other => panic!("{other:?}"),
    }
    let report = Report { name: "x".into(), mail: 2, skipped_count: 1, ..Default::default() };
    let line = serde_json::to_string(&Wire::Done { report: WireReport::from(&report) }).unwrap();
    match serde_json::from_str::<Wire>(&line).unwrap().into_event().unwrap() {
        Event::Done(Ok(r)) => assert_eq!(r, report),
        other => panic!("{other:?}"),
    }
    // A damaged line is an error, not a panic.
    assert!(serde_json::from_str::<Wire>(r#"{"t":"mail","folder":1}"#).is_err());
    let bad = Wire::Mail { folder: 0, item: Box::default(), source: Some("***".into()), body: None };
    assert!(bad.into_event().is_err());
    assert_eq!(worker_main(&[]), 2);
}

/// A reader process that dies (here: one that exits at once with an error, as a reader that ran out of
/// memory on a damaged file would) ends the import with a report; one that cannot start is an error.
#[cfg(unix)]
#[test]
fn a_reader_process_that_dies_ends_the_import_with_a_report() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "x.pst", &fixture());
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let options = mailcraft_formats::pst::Options::default();
    let control = std::sync::Arc::new(mailcraft_formats::pst::Control::default());
    let rx = spawn_process(Path::new("/bin/false"), &path, options, &slot, &control).unwrap();
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Event::Done(Err(e)) => assert!(e.contains("stopped"), "{e}"),
        other => panic!("{other:?}"),
    }
    let missing = dir.path().join("no-such-program");
    assert!(spawn_process(&missing, &path, options, &slot, &control).is_err());
    // A reader that writes something that is not a result line.
    let rx = spawn_process(Path::new("/bin/echo"), &path, options, &slot, &control).unwrap();
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Event::Done(Err(e)) => assert!(e.contains("unreadable"), "{e}"),
        other => panic!("{other:?}"),
    }
}
