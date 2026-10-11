//! End-to-end flows over several commands on the sample mailbox, plus the session core (undo/redo, panic
//! guard, store journaling).

// Test-only crate: helper functions outside `#[test]` may fail loudly.
#![allow(clippy::expect_used)]

use mailcraft_engine::queries::{list_rows, visible_item_ids};
use mailcraft_engine::sample::sample_session;
use mailcraft_engine::{CmdError, CmdResult, CommandSpec, Notice, Registry, Session, always};
use mailcraft_model::{FlagState, FolderId, FolderRole, ItemId, Mailbox};
use serde_json::{Value, json};

fn role(s: &Session, r: FolderRole) -> FolderId {
    let account = s.current_account().expect("account");
    s.mailbox.role_folder(account, r).expect("role folder")
}

fn find(s: &Session, subject: &str) -> ItemId {
    s.mailbox.mail.values().find(|m| m.subject == subject).map(|m| m.id).expect("sample message")
}

fn select(s: &mut Session, id: ItemId) {
    s.run("nav.selectItem", &json!({"id": id.0})).expect("select");
}

#[test]
fn reply_then_send_without_account_fails_with_notice() {
    let mut s = sample_session();
    let id = find(&s, "Budget questions");
    select(&mut s, id);
    s.run("mail.reply", &json!({})).expect("reply");
    let (cid, c) = s.composes.iter().next_back().map(|(k, v)| (*k, v.clone())).expect("compose opened");
    assert_eq!(c.subject, "RE: Budget questions");
    assert!(c.to.contains("alice@example.com"), "reply goes to the sender: {}", c.to);
    let outbox = role(&s, FolderRole::Outbox);
    let sent = role(&s, FolderRole::SentItems);
    let sent_before = s.mailbox.total_count(sent);
    s.notices.clear();
    let r = s.run("compose.send", &json!({"compose": cid, "id": cid}));
    let notice = s.notices.iter().any(|n| match n {
        Notice::Error(t) | Notice::Info(t) => t.contains("account"),
        Notice::OpenDialog(name, p) => name.contains("ccount") || p.to_string().contains("account"),
        _ => false,
    });
    let err_mentions_account =
        matches!(&r, Err(CmdError::Failed(m) | CmdError::Disabled(m)) if m.contains("account"));
    assert!(notice || err_mentions_account, "expected the no-account notice, got {r:?} / {:?}", s.notices);
    assert_eq!(s.mailbox.total_count(outbox), 0, "nothing goes to the Outbox");
    assert_eq!(s.mailbox.total_count(sent), sent_before);
}

#[test]
fn delete_then_undo_restores_the_message() {
    let mut s = sample_session();
    let inbox = s.selection.folder;
    let id = find(&s, "Lunch on Friday?");
    select(&mut s, id);
    s.run("mail.delete", &json!({})).expect("delete");
    assert_eq!(s.mailbox.mail[&id].folder, role(&s, FolderRole::DeletedItems));
    assert_eq!(s.undo.undo_label(), Some("Delete"));
    s.run("app.undo", &json!({})).expect("undo");
    assert_eq!(s.mailbox.mail[&id].folder, inbox);
    s.run("app.redo", &json!({})).expect("redo");
    assert_eq!(s.mailbox.mail[&id].folder, role(&s, FolderRole::DeletedItems));
}

#[test]
fn move_flag_categorize_and_undo_each() {
    let mut s = sample_session();
    let projects = s.mailbox.folders.values().find(|f| f.name == "Projects").map(|f| f.id).expect("Projects");
    let id = find(&s, "Analytics summary");
    select(&mut s, id);

    s.run("mail.flag", &json!({})).expect("flag");
    assert!(matches!(s.mailbox.mail[&id].flag, FlagState::Flagged { .. }));
    s.run("mail.markComplete", &json!({})).expect("complete");
    assert!(matches!(s.mailbox.mail[&id].flag, FlagState::Complete { .. }));
    s.run("mail.clearFlag", &json!({})).expect("clear flag");
    assert_eq!(s.mailbox.mail[&id].flag, FlagState::None);

    s.run("mail.categorize", &json!({"name": "Red category"})).expect("categorize");
    assert!(s.mailbox.mail[&id].categories.iter().any(|c| c == "Red category"));
    let row = list_rows(&s)
        .into_iter()
        .find_map(|r| match r {
            mailcraft_engine::queries::ListRow::Item(i) if i.id == id => Some(i),
            _ => None,
        })
        .expect("row");
    assert!(!row.categories.is_empty());

    s.run("mail.move", &json!({"folder": projects.0})).expect("move");
    assert_eq!(s.mailbox.mail[&id].folder, projects);
    assert!(!visible_item_ids(&s).contains(&id), "moved out of the Inbox view");

    // Undo walks back move, categorize, clear flag, complete, flag.
    s.run("app.undo", &json!({})).expect("undo move");
    assert_ne!(s.mailbox.mail[&id].folder, projects);
    s.run("app.undo", &json!({})).expect("undo categorize");
    assert!(!s.mailbox.mail[&id].categories.iter().any(|c| c == "Red category"));
}

#[test]
fn new_command_clears_redo_and_undo_is_bounded() {
    let mut s = sample_session();
    let id = find(&s, "Analytics summary");
    select(&mut s, id);
    s.run("mail.flag", &json!({})).expect("flag");
    s.run("app.undo", &json!({})).expect("undo");
    assert!(s.undo.can_redo());
    s.run("mail.categorize", &json!({"name": "Blue category"})).expect("categorize");
    assert!(!s.undo.can_redo());
    for i in 0..120 {
        let name = if i % 2 == 0 { "Green category" } else { "Purple category" };
        s.run("mail.categorize", &json!({"name": name})).expect("categorize");
    }
    assert!(s.undo.undo.len() <= 100);
}

fn mutate_then_panic(s: &mut Session, _: &Value) -> CmdResult {
    s.mailbox.mail.clear();
    s.pending_ops.push(mailcraft_store::Op::Replace(Box::default()));
    let v: Vec<u8> = Vec::new();
    // A deliberate out-of-bounds access: the guard must catch it and restore the mailbox.
    #[allow(clippy::indexing_slicing)]
    let x = v[3];
    Ok(json!(x))
}

fn mutate_then_fail(s: &mut Session, _: &Value) -> CmdResult {
    s.mailbox.mail.clear();
    s.pending_ops.push(mailcraft_store::Op::Replace(Box::default()));
    Err(CmdError::Failed("nope".into()))
}

#[test]
fn panic_guard_and_errors_restore_the_mailbox() {
    let mut s = sample_session();
    let mut specs = s.registry.specs.clone();
    for (id, run) in [
        ("test.panic", mutate_then_panic as fn(&mut Session, &Value) -> CmdResult),
        ("test.fail", mutate_then_fail),
    ] {
        specs.push(CommandSpec {
            id,
            label: "Test",
            location: "Test",
            shortcut: "",
            params: "",
            mutates: true,
            enabled: always,
            run,
        });
    }
    s.registry = Registry::new(specs);
    let before: Mailbox = s.mailbox.clone();
    let pending = s.pending_ops.len();
    match s.run("test.panic", &json!({})) {
        Err(CmdError::Failed(m)) => assert!(m.contains("panicked"), "{m}"),
        other => panic!("expected a guarded panic, got {other:?}"),
    }
    assert_eq!(s.mailbox, before);
    assert_eq!(s.pending_ops.len(), pending);
    assert!(!s.undo.can_undo());
    assert!(matches!(s.run("test.fail", &json!({})), Err(CmdError::Failed(_))));
    assert_eq!(s.mailbox, before);
    assert_eq!(s.pending_ops.len(), pending);
}

#[test]
fn reopen_renames_the_legacy_local_account() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut s = Session::open(dir.path()).expect("open new store");
        s.mailbox.accounts[0].display_name = mailcraft_model::mailbox::LEGACY_LOCAL_ACCOUNT_NAME.to_owned();
        s.mailbox.accounts[0].sender_name = mailcraft_model::mailbox::LEGACY_LOCAL_ACCOUNT_NAME.to_owned();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
    }
    let s = Session::open(dir.path()).expect("reopen");
    assert_eq!(s.mailbox.accounts[0].display_name, "Local Folders");
    assert_eq!(s.mailbox.accounts[0].sender_name, "");
}

#[test]
fn store_journal_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let count;
    {
        let mut s = Session::open(dir.path()).expect("open new store");
        assert_eq!(s.mailbox.accounts.len(), 1);
        assert_eq!(s.mailbox.accounts[0].display_name, "Local Folders");
        assert_eq!(s.mailbox.folder(s.selection.folder).and_then(|f| f.role), Some(FolderRole::Inbox));
        // Load the sample into the store, then change it through commands.
        let sample = sample_session();
        s.mailbox = sample.mailbox.clone();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        s.select_default_folder();
        let id = find(&s, "Analytics summary");
        s.run("nav.selectItem", &json!({"id": id.0})).expect("select");
        s.run("app.copy", &json!({})).expect("copy");
        s.run("app.paste", &json!({})).expect("paste");
        s.run("mail.flag", &json!({})).expect("flag");
        assert!(s.pending_ops.is_empty(), "ops were written to the store");
        count = s.mailbox.mail.len();
        // Undo journals a Replace.
        s.run("app.undo", &json!({})).expect("undo flag");
        assert!(s.pending_ops.is_empty());
    }
    let s = Session::open(dir.path()).expect("reopen");
    assert_eq!(s.mailbox.mail.len(), count);
    let copies = s.mailbox.mail.values().filter(|m| m.subject == "Analytics summary").count();
    assert_eq!(copies, 2);
    assert!(s.mailbox.mail.values().filter(|m| m.subject == "Analytics summary").all(|m| !m.is_flagged()));
    assert_eq!(s.me, vec!["jordan.lee@example.net".to_owned()]);
}

#[test]
fn module_rail_and_selection_flow() {
    let mut s = sample_session();
    let unread: Vec<ItemId> =
        visible_item_ids(&s).into_iter().filter(|id| !s.mailbox.mail[id].read).collect();
    let first = unread[0];
    select(&mut s, first);
    s.run("nav.calendar", &json!({})).expect("calendar");
    assert!(s.mailbox.mail[&first].read, "leaving the message marks it read");
    s.run("nav.mail", &json!({})).expect("mail");
    assert_eq!(s.mailbox.folder(s.selection.folder).and_then(|f| f.role), Some(FolderRole::Inbox));
    assert_eq!(s.registry.by_shortcut("Mod+2").map(|c| c.id), Some("nav.calendar"));
    assert_eq!(s.registry.by_shortcut("mod+z").map(|c| c.id), Some("app.undo"));
}

/// Regression (honest-parity audit, commit 10): New Appointment/Contact/Task from the Mail module used to pass the
/// selected mail folder, which the item dialogs reject; only a post belongs in the mail folder.
#[test]
fn new_items_from_mail_do_not_pass_the_mail_folder() {
    let mut s = sample_session();
    for kind in ["appointment", "meeting", "contact", "task", "contactGroup"] {
        s.notices.clear();
        s.run("mail.newItem", &json!({"kind": kind})).unwrap();
        let params = s.notices.iter().find_map(|n| match n {
            Notice::OpenDialog(_, p) => Some(p.clone()),
            _ => None,
        });
        let params = params.expect("dialog opened");
        assert!(params.get("folder").is_none(), "{kind}: {params}");
    }
    s.notices.clear();
    s.run("mail.newItem", &json!({"kind": "post"})).unwrap();
    assert!(s.notices.iter().any(|n| matches!(n, Notice::OpenDialog(_, p) if p.get("folder").is_some())));
}
