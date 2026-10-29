//! Offline window tests: which bodies a pass takes off, what it never touches, the settings that drive it,
//! and opening a message whose body is gone (online and offline).

use super::*;
use crate::model::{Account, DateTime, FolderId, FolderRole, Mailbox, ServerSettings};
use mailcraft_store::Content;

const NOW: i64 = 1_791_000_000;

struct Fx {
    s: Session,
    acct: AccountId,
    inbox: FolderId,
}

fn fx() -> Fx {
    let mut mb = Mailbox::new_local("Local Folders");
    let acct = mb.add_account(Account {
        kind: AccountKind::Imap,
        display_name: "Work".into(),
        address: "me@example.com".into(),
        incoming: Some(ServerSettings { host: "127.0.0.1".into(), port: 9, ..Default::default() }),
        ..Default::default()
    });
    mb.create_default_folders(acct);
    let inbox = mb.role_folder(acct, FolderRole::Inbox).unwrap();
    mb.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
    let mut s = Session::new(mb);
    s.fixed_now = Some(DateTime::from_unix(NOW, 0));
    s.view.work_offline = true;
    Fx { s, acct, inbox }
}

fn raw(uid: u32) -> Vec<u8> {
    format!("From: ann@example.org\r\nSubject: Message {uid}\r\nMessage-ID: <m{uid}@example.org>\r\n\r\nBody {uid}\r\n")
        .into_bytes()
}

/// A downloaded server message received `days` ago, with UID `uid`.
fn server_mail(f: &mut Fx, uid: u32, days: i64) -> ItemId {
    let item = MailItem {
        folder: f.inbox,
        subject: format!("Message {uid}"),
        preview: format!("Body {uid}"),
        remote_id: Some(uid.to_string()),
        received: DateTime::from_unix(NOW - days * DAY, 0),
        ..Default::default()
    };
    f.s.add_message(item, &Content::source(raw(uid))).unwrap()
}

fn options(f: &mut Fx, p: Value) -> Value {
    let mut p = p;
    p["account"] = json!(f.acct.0);
    f.s.run("accounts.options", &p).unwrap()
}

#[test]
fn window_stops_labels_keys_and_days() {
    assert_eq!(OfflineWindow::ALL.len(), 11);
    assert_eq!(OfflineWindow::default(), OfflineWindow::OneYear);
    let labels: Vec<&str> = OfflineWindow::ALL.iter().map(|w| w.label()).collect();
    assert_eq!(
        labels,
        [
            "3 days", "1 week", "2 weeks", "1 month", "3 months", "6 months", "1 year", "2 years", "3 years",
            "5 years", "All"
        ]
    );
    let mut last = 0;
    for (i, w) in OfflineWindow::ALL.into_iter().enumerate() {
        assert_eq!(w.index(), i);
        assert_eq!(OfflineWindow::from_index(i), Some(w));
        assert_eq!(OfflineWindow::parse(w.key()), Some(w));
        assert_eq!(OfflineWindow::parse(w.label()), Some(w));
        assert_eq!(OfflineWindow::parse(&w.label().to_uppercase()), Some(w));
        if let Some(d) = w.days() {
            assert!(d > last, "{w:?} is longer than the stop before it");
            last = d;
        }
    }
    assert_eq!(OfflineWindow::All.days(), None);
    assert_eq!(OfflineWindow::OneYear.days(), Some(365));
    assert_eq!(OfflineWindow::from_index(11), None);
    for junk in ["", "13 months", "-1", "all of it", "\u{0}"] {
        assert_eq!(OfflineWindow::parse(junk), None, "{junk:?}");
    }
}

#[test]
fn a_pass_takes_old_bodies_off_and_keeps_the_headers() {
    let mut f = fx();
    let old = server_mail(&mut f, 1, 400);
    let new = server_mail(&mut f, 2, 30);
    let key = f.s.mailbox.mail.get(old).unwrap().blob.clone().unwrap();
    let steps = f.s.undo.undo.len();
    f.s.pending_ops.clear();
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.removed, r.more), (1, 1, false), "the default window is one year");
    let m = f.s.mailbox.mail.get(old).unwrap();
    assert!(m.body_evicted && m.blob.is_none() && m.body_key.is_none());
    assert_eq!((m.subject.as_str(), m.preview.as_str()), ("Message 1", "Body 1"), "headers and preview stay");
    assert!(!f.s.bodies.has_source(&key), "its source is gone from the body store");
    assert!(f.s.body(old).unwrap().is_none());
    assert!(is_evicted(&f.s, old) && !is_evicted(&f.s, new));
    assert!(f.s.mailbox.mail.get(new).unwrap().blob.is_some());
    assert_eq!(f.s.undo.undo.len(), steps, "no undo step");
    assert!(!f.s.pending_ops.is_empty(), "the change is journaled");
    assert!(f.s.mailbox.account(f.acct).unwrap().sync.pending.is_empty(), "nothing changes on the server");
    // A second pass finds nothing more.
    assert_eq!(pass(&mut f.s, f.acct), PassReport::default());
}

#[test]
fn what_a_pass_never_takes_off() {
    let mut f = fx();
    let selected = server_mail(&mut f, 1, 400);
    let edited = server_mail(&mut f, 2, 400);
    let draft = server_mail(&mut f, 3, 400);
    let no_uid = server_mail(&mut f, 4, 400);
    let gone = server_mail(&mut f, 5, 400);
    f.s.edit_body(edited, |b| b.text = Some("Changed here".into())).unwrap();
    f.s.mailbox.mail.get_mut(draft).unwrap().is_draft = true;
    f.s.mailbox.mail.get_mut(no_uid).unwrap().remote_id = None;
    f.s.selection.items = vec![selected];
    // A local folder of the same account (not on the server).
    let local = f.s.mailbox.create_folder(f.inbox, "Kept here").unwrap();
    let here = server_mail(&mut f, 6, 400);
    f.s.mailbox.mail.get_mut(here).unwrap().folder = local;
    let r = pass(&mut f.s, f.acct);
    assert_eq!(r.evicted, 1, "{r:?}");
    assert!(is_evicted(&f.s, gone));
    for (id, why) in [
        (selected, "selected"),
        (edited, "edited"),
        (draft, "draft"),
        (no_uid, "no server id"),
        (here, "local"),
    ] {
        assert!(!is_evicted(&f.s, id), "{why}");
        assert!(f.s.body(id).unwrap().is_some(), "{why} keeps its body");
    }
    assert_eq!(f.s.body(edited).unwrap().unwrap().text.as_deref(), Some("Changed here"));
}

#[test]
fn pop3_jmap_and_local_accounts_keep_everything() {
    let mut f = fx();
    let id = server_mail(&mut f, 1, 4000);
    for kind in [AccountKind::Jmap, AccountKind::Local] {
        f.s.mailbox.accounts.iter_mut().find(|a| a.id == f.acct).unwrap().kind = kind;
        assert!(!applies(&f.s, f.acct), "{kind:?}");
        assert_eq!(pass(&mut f.s, f.acct).evicted, 0);
        assert!(f.s.run("accounts.applyOfflineWindow", &json!({"account": f.acct.0})).is_err());
    }
    assert!(has_window_account(&f.s).is_some(), "no IMAP account left");
    // POP3 (an IMAP-kind account whose options say POP3).
    f.s.mailbox.accounts.iter_mut().find(|a| a.id == f.acct).unwrap().kind = AccountKind::Imap;
    crate::accounts::load_prefs(&mut f.s);
    crate::accounts::prefs(&mut f.s).accounts.entry(f.acct.0).or_default().pop3 = true;
    assert!(crate::accounts::is_pop3(&f.s, f.acct));
    assert!(!applies(&f.s, f.acct));
    assert_eq!(pass(&mut f.s, f.acct).evicted, 0);
    assert!(!is_evicted(&f.s, id));
}

#[test]
fn the_settings_drive_the_window_and_apply_at_once() {
    let mut f = fx();
    let recent = server_mail(&mut f, 1, 2);
    let month = server_mail(&mut f, 2, 20);
    let years = server_mail(&mut f, 3, 800);
    let o = options(&mut f, json!({}));
    assert_eq!((o["keepOffline"].as_bool(), o["offlineWindow"].as_str()), (Some(true), Some("1y")), "{o}");
    assert_eq!(o["offlineWindowLabel"], "1 year");
    assert_eq!(o["offlineWindowApplies"], true);
    // All: nothing goes, even years-old mail.
    let r = options(&mut f, json!({"offlineWindow": "all"}));
    assert_eq!(r["evicted"], 0, "{r}");
    // Two weeks (by label or by stop number): the month-old and the years-old go.
    let r = options(&mut f, json!({"offlineWindow": 2}));
    assert_eq!(r["evicted"], 2, "{r}");
    assert!(is_evicted(&f.s, month) && is_evicted(&f.s, years) && !is_evicted(&f.s, recent));
    assert_eq!(crate::accounts::account_options(&f.s, f.acct).offline_window, OfflineWindow::TwoWeeks);
    // Off: no bodies are kept at all.
    let r = options(&mut f, json!({"keepOffline": false}));
    assert_eq!(r["evicted"], 1, "{r}");
    assert!(is_evicted(&f.s, recent));
    assert!(placeholder(&f.s, recent).unwrap().contains("keeps no mail on this computer"));
    // Hostile values are refused and change nothing.
    for bad in [
        json!({"offlineWindow": "forever"}),
        json!({"offlineWindow": 11}),
        json!({"offlineWindow": -1}),
        json!({"keepOffline": "yes"}),
    ] {
        let mut p = bad.clone();
        p["account"] = json!(f.acct.0);
        assert!(f.s.run("accounts.options", &p).is_err(), "{bad}");
    }
    assert!(!crate::accounts::account_options(&f.s, f.acct).keep_offline);
    // The settings survive a reload of the preferences.
    let all = f.s.run("accounts.applyOfflineWindow", &json!({})).unwrap();
    assert_eq!(all["accounts"][0]["keepOffline"], false, "{all}");
}

#[test]
fn opening_downloads_the_body_again_when_online() {
    let mut f = fx();
    let id = server_mail(&mut f, 7, 400);
    pass(&mut f.s, f.acct);
    assert!(is_evicted(&f.s, id));
    // Working offline: nothing starts, and the reading pane says why.
    assert!(!opened(&mut f.s, id));
    let text = placeholder(&f.s, id).unwrap();
    assert!(text.contains("older than the 1 year") && text.contains("working offline"), "{text}");
    // Online: one download starts (opening it again does not start another).
    // Online: the download is asked for once (the test account has no server, so the run itself fails, and
    // the bar says so instead of "Downloading" for ever).
    f.s.view.work_offline = false;
    opened(&mut f.s, id);
    assert!(f.s.sync.offline.fetching.contains(&id));
    assert!(
        f.s.sync.fetch.get(&f.acct).is_some_and(|l| l.contains(&id)),
        "queued for the account's next run"
    );
    assert!(!opened(&mut f.s, id), "once");
    let text = placeholder(&f.s, id).unwrap();
    assert!(text.contains("Downloading") || text.contains("could not be downloaded"), "{text}");
    // The whole message arrives: the body is back and the item is an ordinary one again.
    f.s.view.work_offline = true;
    let msg = mailcraft_net::sync::NewMessage {
        uid: 7,
        raw: raw(7),
        flags: vec![],
        internal_date: None,
        size: None,
        headers_only: false,
    };
    assert!(f.s.queue_imap_results(
        f.acct,
        crate::sync::SyntheticImap::Bodies { folder: "INBOX".into(), msgs: vec![msg] }
    ));
    for _ in 0..20 {
        f.s.poll_sync();
        if !is_evicted(&f.s, id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!is_evicted(&f.s, id));
    assert!(!f.s.mailbox.mail.get(id).unwrap().body_evicted);
    assert!(f.s.body_text(f.s.mailbox.mail.get(id).unwrap()).contains("Body 7"));
    assert_eq!(placeholder(&f.s, id), None);
    // Regression: the download was remembered for the rest of the session, so a message that lost its body
    // again was never downloaded again and the bar said "Downloading" for ever.
    assert!(!f.s.sync.offline.fetching.contains(&id), "the download is over");
    pass(&mut f.s, f.acct);
    assert!(is_evicted(&f.s, id));
    f.s.view.work_offline = false;
    assert!(!f.s.sync.offline.fetching.contains(&id));
    opened(&mut f.s, id);
    assert!(f.s.sync.offline.fetching.contains(&id), "asked for again");
}

/// Regression: a message open in a read window that is not zoomed lost its body to a pass.
#[test]
fn a_message_open_in_a_read_window_keeps_its_body() {
    let mut f = fx();
    let open = server_mail(&mut f, 1, 400);
    let other = server_mail(&mut f, 2, 400);
    f.s.open_messages.insert(open);
    assert_eq!(pass(&mut f.s, f.acct).evicted, 1);
    assert!(!is_evicted(&f.s, open) && is_evicted(&f.s, other));
    f.s.open_messages.clear();
    assert_eq!(pass(&mut f.s, f.acct).evicted, 1, "closed, it goes with the next pass");
    assert!(is_evicted(&f.s, open));
}

#[test]
fn content_the_undo_history_refers_to_stays_until_the_step_goes() {
    let dir = tempfile::tempdir().unwrap();
    let mut f = fx();
    let mut s = Session::open(dir.path()).unwrap();
    // The fixture's mailbox and options on a profile with a body store on disk.
    s.mailbox = std::mem::take(&mut f.s.mailbox);
    s.fixed_now = f.s.fixed_now;
    s.view.work_offline = true;
    f.s = s;
    let id = server_mail(&mut f, 1, 400);
    let key = f.s.mailbox.mail.get(id).unwrap().blob.clone().unwrap();
    assert!(f.s.bodies.has_source(&key));
    f.s.selection.items = vec![id];
    f.s.run("mail.markRead", &json!({})).unwrap();
    f.s.selection.items.clear();
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.removed), (1, 0), "the undo step still refers to the source");
    assert!(f.s.bodies.has_source(&key));
    // Undo brings the item back as it was, source and all.
    f.s.run("app.undo", &json!({})).unwrap();
    assert!(f.s.body(id).unwrap().is_some());
}

/// Regression: a pass removed the sources it took off even when the journal refused its change (a full
/// disk); killed before a later save, the profile named sources that were gone. They now stay until the
/// change is saved, and a later pass removes them.
#[test]
fn content_stays_until_the_store_holds_the_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut f = fx();
    // The journal's size cap makes the write fail (a legacy profile).
    let mut s = Session::open_with(dir.path(), mailcraft_store::StoreKind::Legacy).unwrap();
    s.mailbox = std::mem::take(&mut f.s.mailbox);
    s.fixed_now = f.s.fixed_now;
    s.view.work_offline = true;
    f.s = s;
    let id = server_mail(&mut f, 1, 400);
    let key = f.s.mailbox.mail.get(id).unwrap().blob.clone().unwrap();
    f.s.flush_pending();
    let store = f.s.store.as_mut().unwrap();
    store.set_journal_cap(store.journal_bytes());
    store.set_snapshot_cap(1);
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.removed), (1, 0));
    assert!(is_evicted(&f.s, id));
    assert!(!f.s.pending_ops.is_empty(), "the change is still queued");
    assert!(f.s.bodies.has_source(&key), "so the source the store still names stays");
    // Room again: once the change is saved, the next pass removes it.
    let store = f.s.store.as_mut().unwrap();
    store.set_journal_cap(u64::MAX);
    store.set_snapshot_cap(u64::MAX);
    f.s.flush_pending();
    assert!(f.s.pending_ops.is_empty());
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.removed), (0, 1));
    assert!(!f.s.bodies.has_source(&key));
}

#[test]
fn passes_run_after_a_good_send_receive_at_most_hourly() {
    let mut f = fx();
    let a = server_mail(&mut f, 1, 400);
    tick(&mut f.s);
    assert!(!is_evicted(&f.s, a), "no send/receive yet");
    note_run_done(&mut f.s, f.acct);
    tick(&mut f.s);
    assert!(is_evicted(&f.s, a));
    let b = server_mail(&mut f, 2, 400);
    note_run_done(&mut f.s, f.acct);
    tick(&mut f.s);
    assert!(!is_evicted(&f.s, b), "the last pass was less than an hour ago");
    f.s.fixed_now = Some(DateTime::from_unix(NOW + PASS_INTERVAL_SECS, 0));
    tick(&mut f.s);
    assert!(is_evicted(&f.s, b));
}

#[test]
fn a_pass_is_bounded_and_the_next_one_goes_on() {
    let mut f = fx();
    let n = MAX_EVICT_PER_PASS + 7;
    let mut ids = Vec::with_capacity(n);
    for uid in 0..n {
        let item = MailItem {
            folder: f.inbox,
            remote_id: Some(uid.to_string()),
            received: DateTime::from_unix(NOW - 400 * DAY - i64::try_from(uid).unwrap(), 0),
            ..Default::default()
        };
        ids.push(
            f.s.add_message(item, &Content::source(format!("Subject: {uid}\r\n\r\nx\r\n").into_bytes()))
                .unwrap(),
        );
    }
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.more), (MAX_EVICT_PER_PASS, true));
    // Oldest first: the last added (received earliest) went.
    assert!(is_evicted(&f.s, *ids.last().unwrap()));
    assert!(!is_evicted(&f.s, ids[0]));
    let r = pass(&mut f.s, f.acct);
    assert_eq!((r.evicted, r.more), (7, false));
    assert!(ids.iter().all(|id| is_evicted(&f.s, *id)));
}

/// Regression: a message whose body the window took off still has its server id; moving it to a local
/// folder (or another account) expunged the only full copy and kept a bodyless item. Such moves, copies and
/// forwards are refused until it is downloaded again; moves within its account still work.
#[test]
fn a_message_without_its_body_never_leaves_its_server() {
    let mut f = fx();
    let old = server_mail(&mut f, 1, 400);
    let kept = server_mail(&mut f, 2, 30);
    pass(&mut f.s, f.acct);
    assert!(is_evicted(&f.s, old));
    let local_acct = f.s.mailbox.accounts.iter().find(|a| a.kind == AccountKind::Local).unwrap().id;
    let local = f.s.mailbox.role_folder(local_acct, FolderRole::Inbox).unwrap();
    let acct = f.acct;
    let pending = |s: &Session| s.mailbox.account(acct).unwrap().sync.pending.len();
    let r = f.s.run("mail.move", &json!({"ids": [old.0, kept.0], "folder": local.0}));
    let Err(CmdError::Failed(why)) = r else { panic!("{r:?}") };
    assert!(why.contains("only its header"), "{why}");
    let m = f.s.mailbox.mail.get(old).unwrap();
    assert_eq!((m.folder, m.remote_id.as_deref()), (f.inbox, Some("1")), "the move is rolled back");
    assert_eq!(f.s.mailbox.mail.get(kept).unwrap().folder, f.inbox, "the whole command is");
    assert_eq!(pending(&f.s), 0, "nothing is expunged");
    // Copies and forwards would write a message without its body.
    let r = f.s.run("mail.copy", &json!({"ids": [old.0], "folder": local.0}));
    assert!(matches!(r, Err(CmdError::Failed(_))), "{r:?}");
    let r = f.s.run("mail.forwardAsAttachment", &json!({"ids": [old.0]}));
    assert!(matches!(r, Err(CmdError::Failed(_))), "{r:?}");
    assert_eq!(f.s.mailbox.mail.in_folder(local).count(), 0);
    // A folder moved off the server takes its messages along: refused too.
    let work = f.s.mailbox.create_folder(f.inbox, "Work").unwrap();
    f.s.mailbox.folders.get_mut(&work).unwrap().remote_name = Some("INBOX/Work".into());
    f.s.mailbox.mail.update(old, |m| m.folder = work);
    f.s.selection.folder = work;
    let r = f.s.run("folder.move", &json!({"folder": work.0, "parent": local.0}));
    assert!(matches!(r, Err(CmdError::Failed(_))), "{r:?}");
    assert_eq!(f.s.mailbox.folder(work).unwrap().parent, Some(f.inbox));
    f.s.mailbox.mail.update(old, |m| m.folder = f.inbox);
    // Within the account the server keeps the message: allowed.
    let old_folder = f.s.mailbox.create_folder(f.inbox, "Old").unwrap();
    f.s.mailbox.folders.get_mut(&old_folder).unwrap().remote_name = Some("INBOX/Old".into());
    f.s.run("mail.move", &json!({"ids": [old.0], "folder": old_folder.0})).unwrap();
    assert!(matches!(
        f.s.mailbox.account(acct).unwrap().sync.pending.last().map(|p| &p.op),
        Some(mailcraft_model::account::RemoteOp::Move { .. })
    ));
    // Once downloaded again, it moves out like any other message.
    f.s.set_source(old, Some(&raw(1))).unwrap();
    f.s.mailbox.mail.update(old, |m| m.remote_id = Some("1".into()));
    let before = pending(&f.s);
    f.s.run("mail.move", &json!({"ids": [old.0], "folder": local.0})).unwrap();
    assert_eq!(f.s.mailbox.mail.get(old).unwrap().folder, local);
    assert!(pending(&f.s) > before);
    assert!(f.s.body(old).unwrap().is_some_and(|b| b.plain_text().contains("Body 1")));
}
