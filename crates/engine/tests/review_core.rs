//! Session core and list/folder query checks against the observed Outlook behaviour
//! (`plan/outlook/message-list.md`, `reading-pane.md`, `folders.md`).

#![allow(clippy::expect_used)]

use mailcraft_engine::queries::{FolderRowKind, ListRow, folder_rows, list_rows, view_item_ids};
use mailcraft_engine::sample::sample_session;
use mailcraft_engine::{Module, Session};
use mailcraft_model::{Address, ArrangeBy, FolderRole, ItemId, ListFilter, SortOrder};
use serde_json::json;

fn find(s: &Session, subject: &str) -> ItemId {
    s.mailbox.mail.values().find(|m| m.subject == subject).map(|m| m.id).expect("sample message")
}

fn groups(rows: &[ListRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            ListRow::Group { label, .. } => Some(label.clone()),
            ListRow::Item(_) => None,
        })
        .collect()
}

fn item_rows(rows: &[ListRow]) -> Vec<mailcraft_engine::queries::ItemRow> {
    rows.iter()
        .filter_map(|r| match r {
            ListRow::Item(i) => Some(i.clone()),
            ListRow::Group { .. } => None,
        })
        .collect()
}

fn arrange(s: &mut Session, key: ArrangeBy, order: SortOrder) {
    let folder = s.selection.folder;
    let mut vs = mailcraft_engine::queries::view_settings(s, folder);
    vs.arrange_by = key;
    vs.order = order;
    vs.focused_inbox = false;
    vs.filter = ListFilter::All;
    s.mailbox.views.insert(folder, vs);
}

fn assert_unique(g: &[String]) {
    let mut u = g.to_vec();
    u.sort();
    u.dedup();
    assert_eq!(u.len(), g.len(), "a group header appears more than once: {g:?}");
}

#[test]
fn from_groups_are_not_split_by_name_case() {
    let mut s = sample_session();
    // Three Inbox mails from "Alice Rivera"; give the middle one a lower-case display name.
    let mut alice: Vec<ItemId> = s
        .mailbox
        .mail
        .values()
        .filter(|m| m.folder == s.selection.folder)
        .filter(|m| m.from.as_ref().is_some_and(|f| f.addr == "alice@example.com"))
        .map(|m| m.id)
        .collect();
    alice.sort_by_key(|id| s.mailbox.mail[id].received.unix);
    assert!(alice.len() >= 2, "sample has several mails from Alice");
    let mid = alice[alice.len() / 2];
    if let Some(m) = s.mailbox.mail.get_mut(&mid) {
        m.from = Some(Address { name: Some("alice rivera".into()), addr: "alice@example.com".into() });
    }
    arrange(&mut s, ArrangeBy::From, SortOrder::Ascending);
    let rows = list_rows(&s);
    let g = groups(&rows);
    assert_unique(&g);
    let alice_group = rows.iter().find_map(|r| match r {
        ListRow::Group { label, count, .. } if label.eq_ignore_ascii_case("alice rivera") => Some(*count),
        _ => None,
    });
    assert_eq!(alice_group, Some(alice.len()), "all of Alice's mail in one group: {g:?}");
}

#[test]
fn subject_groups_are_not_split_by_case_or_spacing() {
    let mut s = sample_session();
    let inbox = s.selection.folder;
    let b = s
        .mailbox
        .mail
        .values()
        .find(|m| m.folder == inbox && m.subject == "RE: Project kickoff")
        .map(|m| m.id)
        .expect("Bob's reply in the Inbox");
    // Same conversation topic as "Project kickoff", different case/spacing.
    if let Some(m) = s.mailbox.mail.get_mut(&b) {
        m.subject = "RE: project  KICKOFF".into();
    }
    arrange(&mut s, ArrangeBy::Subject, SortOrder::Ascending);
    let g = groups(&list_rows(&s));
    assert_unique(&g);
    let kick: Vec<&String> = g.iter().filter(|l| l.to_lowercase().contains("kickoff")).collect();
    assert_eq!(kick.len(), 1, "{g:?}");
}

#[test]
fn group_items_match_the_header_count() {
    let mut s = sample_session();
    for key in ArrangeBy::ALL {
        arrange(&mut s, key, SortOrder::Descending);
        for r in list_rows(&s) {
            if let ListRow::Group { label, count, .. } = r {
                assert_eq!(
                    mailcraft_engine::queries::group_items(&s, &label).len(),
                    count,
                    "{key:?} {label}"
                );
            }
        }
    }
}

#[test]
fn collapse_this_group_and_collapse_all_work_for_every_arrangement() {
    for key in ArrangeBy::ALL {
        let mut s = sample_session();
        arrange(&mut s, key, SortOrder::Descending);
        let first = view_item_ids(&s).first().copied().expect("items");
        s.selection.items = vec![first];
        s.selection.focus = Some(first);
        s.run("view.collapseGroup", &json!({})).expect("collapse group");
        let rows = list_rows(&s);
        assert!(
            matches!(rows.first(), Some(ListRow::Group { collapsed: true, .. })),
            "{key:?}: the focused item's group is collapsed: {:?}",
            rows.first()
        );
        assert!(!item_rows(&rows).iter().any(|i| i.id == first), "{key:?}: its rows are hidden");
        s.run("view.collapseAll", &json!({})).expect("collapse all");
        let rows = list_rows(&s);
        assert!(
            item_rows(&rows).is_empty(),
            "{key:?}: Collapse All Groups hides every row: {:?}",
            groups(&rows)
        );
        s.run("view.expandAll", &json!({})).expect("expand all");
        assert_eq!(item_rows(&list_rows(&s)).len(), view_item_ids(&s).len());
    }
}

#[test]
fn flagged_row_accessible_name_matches_observed_text() {
    // Observed: "…, Size 8 KB, Start Date Fri 10/9/2026, Due Date Fri 10/9/2026, Flag Status Red Flag, Preview …, "
    let s = sample_session();
    let id = find(&s, "Quarterly report draft");
    let row = item_rows(&list_rows(&s)).into_iter().find(|i| i.id == id).expect("row");
    assert!(
        row.accessible
            .contains("Start Date Fri 10/9/2026, Due Date Mon 10/12/2026, Flag Status Red Flag, Preview "),
        "{}",
        row.accessible
    );
    assert!(row.accessible.ends_with(", "), "the observed name ends with \", \": {:?}", row.accessible);
}

#[test]
fn folder_pane_default_order_and_badges() {
    let s = sample_session();
    let rows = folder_rows(&s);
    let favs: Vec<&str> =
        rows.iter().filter(|r| r.kind == FolderRowKind::Favorite).map(|r| r.name.as_str()).collect();
    assert_eq!(favs, ["Inbox", "Sent Items", "Drafts", "Deleted Items"]);
    let root = rows.iter().position(|r| r.kind == FolderRowKind::AccountRoot).expect("root");
    let top: Vec<&str> =
        rows.iter().skip(root + 1).filter(|r| r.depth == 1).map(|r| r.name.as_str()).collect();
    let expected = [
        "Inbox",
        "Drafts",
        "Sent Items",
        "Deleted Items",
        "Archive",
        "Conversation History",
        "Junk Email",
        "Outbox",
        "RSS Feeds",
    ];
    let observed: Vec<&str> = top.iter().copied().filter(|n| expected.contains(n)).collect();
    assert_eq!(observed, expected, "default folder order: {top:?}");
    if let Some(pos) = top.iter().position(|n| *n == "Search Folders") {
        assert_eq!(pos, top.len() - 1, "Search Folders come after the mail folders: {top:?}");
    }
    let mb = &s.mailbox;
    for r in &rows {
        let Some(id) = r.id else { continue };
        let f = mb.folder(id).expect("folder");
        let total = mb.total_count(id);
        let unread = mb.mail.values().filter(|m| m.folder == id && !m.read).count();
        match f.role {
            Some(FolderRole::Drafts | FolderRole::JunkEmail) => {
                let want = if total == 0 { String::new() } else { format!("[{total}]") };
                assert_eq!(r.badge, want, "{}", r.name);
                let acc = if total == 0 { r.name.clone() } else { format!("{}: {total} items", r.name) };
                assert_eq!(r.accessible, acc);
            }
            _ if f.kind == mailcraft_model::FolderKind::Mail => {
                let want = if unread == 0 { String::new() } else { unread.to_string() };
                assert_eq!(r.badge, want, "{}", r.name);
                let acc = match unread {
                    0 => r.name.clone(),
                    1 => format!("{}: 1 unread message", r.name),
                    n => format!("{}: {n} unread messages", r.name),
                };
                assert_eq!(r.accessible, acc);
                assert!(!r.badge_total || r.badge.is_empty(), "{}", r.name);
            }
            _ => {}
        }
    }
}

#[test]
fn non_mail_modules_hide_favorites() {
    let mut s = sample_session();
    s.view.module = Module::Calendar;
    let rows = folder_rows(&s);
    assert!(
        rows.iter().all(|r| r.kind != FolderRowKind::FavoritesHeader && r.kind != FolderRowKind::Favorite)
    );
}

#[test]
fn undo_redo_round_trip_restores_exact_mailboxes() {
    let mut s = sample_session();
    let start = s.mailbox.clone();
    let id = find(&s, "Analytics summary");
    s.run("nav.selectItem", &json!({"id": id.0})).expect("select");
    let after_select = s.mailbox.clone();
    s.run("mail.flag", &json!({})).expect("flag");
    let flagged = s.mailbox.clone();
    s.run("mail.categorize", &json!({"name": "Red category"})).expect("cat");
    let categorized = s.mailbox.clone();
    s.run("app.undo", &json!({})).expect("undo");
    assert_eq!(s.mailbox, flagged);
    s.run("app.undo", &json!({})).expect("undo");
    assert_eq!(s.mailbox, after_select);
    s.run("app.redo", &json!({})).expect("redo");
    assert_eq!(s.mailbox, flagged);
    s.run("app.redo", &json!({})).expect("redo");
    assert_eq!(s.mailbox, categorized);
    assert!(s.run("app.redo", &json!({})).is_err() || s.mailbox == categorized);
    let _ = start;
}

#[test]
fn failed_command_records_no_undo_step() {
    let mut s = sample_session();
    let depth = s.undo.undo.len();
    let _ = s.run("mail.move", &json!({"folder": u64::MAX, "ids": [u64::MAX]}));
    assert_eq!(s.undo.undo.len(), depth);
}

#[test]
fn every_mailbox_change_is_journaled() {
    // With no store attached, every successful change (including read marking by commands that are not undo
    // steps, such as Open) must leave ops queued so the store sees it; the diff must not miss fields.
    let ids: Vec<&'static str> = sample_session().registry.specs.iter().map(|c| c.id).collect();
    let mut missed = Vec::new();
    for id in ids {
        if matches!(id, "app.undo" | "app.redo") {
            continue;
        }
        let mut s = sample_session();
        let first = view_item_ids(&s).first().copied();
        if let Some(f) = first {
            s.selection.items = vec![f];
            s.selection.focus = Some(f);
        }
        s.pending_ops.clear();
        let before = s.mailbox.clone();
        if s.run(id, &json!({})).is_ok() && s.mailbox != before {
            let mut replay = before.clone();
            for op in &s.pending_ops {
                op.clone().apply(&mut replay);
            }
            let mut a = replay.clone();
            let mut b = s.mailbox.clone();
            a.next_id = 0;
            b.next_id = 0;
            if a != b {
                missed.push(id);
            }
        }
    }
    assert!(missed.is_empty(), "changes not reproduced by the journal: {missed:?}");
}

fn journal_then_fail(s: &mut Session, _: &serde_json::Value) -> mailcraft_engine::CmdResult {
    let ids: Vec<ItemId> = s.mailbox.mail.keys().copied().collect();
    for id in &ids {
        s.mailbox.mail.remove(id);
    }
    s.journal(ids.into_iter().map(mailcraft_store::Op::RemoveMail).collect());
    Err(mailcraft_engine::CmdError::Failed("nope".into()))
}

#[test]
fn a_failed_command_that_journaled_leaves_the_store_matching_the_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let count;
    {
        let mut s = Session::open(dir.path()).expect("open");
        s.mailbox = sample_session().mailbox;
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        count = s.mailbox.mail.len();
        let mut specs = s.registry.specs.clone();
        specs.push(mailcraft_engine::CommandSpec {
            id: "test.journalThenFail",
            label: "Test",
            location: "Test",
            shortcut: "",
            params: "",
            mutates: true,
            enabled: mailcraft_engine::always,
            run: journal_then_fail,
        });
        s.registry = mailcraft_engine::Registry::new(specs);
        assert!(s.run("test.journalThenFail", &json!({})).is_err());
        assert_eq!(s.mailbox.mail.len(), count, "rolled back in memory");
    }
    let s = Session::open(dir.path()).expect("reopen");
    assert_eq!(s.mailbox.mail.len(), count, "the store matches the rolled-back mailbox");
}

fn replace_then_fail(s: &mut Session, _: &serde_json::Value) -> mailcraft_engine::CmdResult {
    s.mailbox.mail.clear();
    s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
    Err(mailcraft_engine::CmdError::Failed("nope".into()))
}

#[test]
fn a_failed_command_that_journaled_without_a_store_queues_the_rollback() {
    let mut s = sample_session();
    for _ in 0..3 {
        s.journal(vec![mailcraft_store::Op::PutMail(Box::new(
            s.mailbox.mail.values().next().cloned().expect("mail"),
        ))]);
    }
    s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
    let base = s.mailbox.clone();
    let mut specs = s.registry.specs.clone();
    specs.push(mailcraft_engine::CommandSpec {
        id: "test.journalThenFail",
        label: "Test",
        location: "Test",
        shortcut: "",
        params: "",
        mutates: true,
        enabled: mailcraft_engine::always,
        run: replace_then_fail,
    });
    s.registry = mailcraft_engine::Registry::new(specs);
    assert!(s.run("test.journalThenFail", &json!({})).is_err());
    let mut replay = mailcraft_model::Mailbox::default();
    for op in s.pending_ops.clone() {
        op.apply(&mut replay);
    }
    assert_eq!(replay.mail.len(), base.mail.len(), "queued ops replay to the rolled-back mailbox");
}

#[test]
fn a_growing_journal_is_folded_into_a_snapshot() {
    // Every view change and every undo journals a whole-mailbox Replace; without checkpoints the journal
    // grows until the store refuses to append (or to open).
    let dir = tempfile::tempdir().expect("tempdir");
    let flagged;
    {
        let mut s = Session::open(dir.path()).expect("open");
        s.checkpoint_bytes = 1;
        s.mailbox = sample_session().mailbox;
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        s.select_default_folder();
        let id = find(&s, "Analytics summary");
        s.run("nav.selectItem", &json!({"id": id.0})).expect("select");
        s.run("mail.flag", &json!({})).expect("flag");
        let bytes = s.store.as_ref().map_or(u64::MAX, |st| st.journal_bytes());
        assert!(bytes < 1_000, "the journal was folded into a snapshot ({bytes} bytes left)");
        assert!(s.pending_ops.is_empty());
        flagged = s.mailbox.mail[&id].is_flagged();
    }
    let s = Session::open(dir.path()).expect("reopen");
    let id = find(&s, "Analytics summary");
    assert_eq!(s.mailbox.mail[&id].is_flagged(), flagged);
    assert!(flagged);
}
