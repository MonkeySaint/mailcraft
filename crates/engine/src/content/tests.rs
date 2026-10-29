//! Session content access: the cached body reads, search reading bodies from the body store, a profile
//! written before bodies moved out of the items, and data files carrying content.

use super::*;
use crate::model::{FolderRole, Mailbox};
use crate::search_index;
use mailcraft_search::{DocKind, Filter};
use serde_json::json;

fn inbox(s: &Session) -> crate::model::FolderId {
    let a = s.mailbox.default_account().map(|a| a.id).unwrap();
    s.mailbox.role_folder(a, FolderRole::Inbox).unwrap()
}

fn found(s: &Session, word: &str) -> Vec<u64> {
    let filter = Filter { folders: None, kinds: vec![DocKind::Mail] };
    search_index::hits(s, word, &filter).into_iter().map(|h| h.id).collect()
}

#[test]
fn bodies_are_read_through_the_cache() {
    let s = crate::sample::sample_session();
    let m = s.mailbox.mail.items().find(|m| m.blob.is_some()).unwrap();
    s.bodies.clear_cache();
    let a = s.body(m.id).unwrap().unwrap();
    let st = s.bodies.cache_stats();
    let b = s.body(m.id).unwrap().unwrap();
    assert!(Arc::ptr_eq(&a, &b), "the second read is the cached value");
    assert_eq!(s.bodies.cache_stats().hits, st.hits + 1);
    assert_eq!(s.body_text(m), a.plain_text());
    assert!(s.body(ItemId(u64::MAX)).unwrap().is_none(), "an unknown item has no body");
    assert!(s.source(ItemId(u64::MAX)).unwrap().is_none());
    // A small cap keeps few bodies, and every read still works.
    s.bodies.set_cache_cap(4_096);
    for m in s.mailbox.mail.items() {
        s.body(m.id).unwrap();
    }
    assert!(s.bodies.cache_stats().bytes <= 4_096);
}

#[test]
fn search_reads_bodies_from_the_body_store() {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    let raw = format!(
        "From: ann@example.org\r\nSubject: Notes\r\n\r\n{} zebrafinch at the end\r\n",
        "filler words here ".repeat(200)
    );
    let item = MailItem {
        folder: inbox(&s),
        subject: "Notes".into(),
        preview: "filler".into(),
        ..Default::default()
    };
    let id = s.add_message(item, &Content::source(raw.into_bytes())).unwrap();
    s.search.invalidate();
    assert_eq!(found(&s, "zebrafinch"), [id.0], "a word past the preview, in the stored source");
    // An edited body (a body record) is what search reads afterwards.
    s.edit_body(id, |b| b.text = Some("Now about kestrels.".into())).unwrap();
    s.search.invalidate(); // a direct edit, outside a command (commands journal theirs)
    assert!(found(&s, "zebrafinch").is_empty());
    assert_eq!(found(&s, "kestrels"), [id.0]);
    // A body that is not on this computer: the headers are still searchable.
    s.set_source(id, None).unwrap();
    s.search.invalidate();
    assert!(found(&s, "kestrels").is_empty());
    assert_eq!(found(&s, "Notes"), [id.0]);
}

/// A profile written before bodies and sources moved out of the items opens with them, and the next
/// checkpoint writes the new form.
#[test]
fn a_profile_with_inline_bodies_opens_with_its_messages() {
    let dir = tempfile::tempdir().unwrap();
    let raw = "From: ann@example.org\r\nSubject: Old source\r\n\r\nKept in the snapshot\r\n";
    let mut mb = Mailbox::new_local("Local Folders");
    let folder = mb.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).map(|f| f.id).unwrap();
    let with_source = mb.add_mail(MailItem { folder, subject: "Old source".into(), ..Default::default() });
    let typed = mb.add_mail(MailItem { folder, subject: "Typed".into(), ..Default::default() });
    let mut v = serde_json::to_value(&mb).unwrap();
    v["mail"][with_source.0.to_string()]["raw"] = json!(raw.as_bytes());
    v["mail"][typed.0.to_string()]["body"] =
        json!({"format": "PlainText", "text": "Written here", "html": null});
    let snapshot = format!("{{\"mailcraft_snapshot\":1,\"generation\":1,\"mailbox\":{v}}}");
    std::fs::write(dir.path().join(mailcraft_store::journal::SNAPSHOT_FILE), snapshot).unwrap();
    {
        let s = Session::open(dir.path()).unwrap();
        assert_eq!(s.source(with_source).unwrap().as_deref(), Some(raw.as_bytes()));
        assert!(s.body_text(s.mailbox.mail.get(with_source).unwrap()).contains("Kept in the snapshot"));
        assert_eq!(s.body(typed).unwrap().unwrap().text.as_deref(), Some("Written here"));
    }
    let written = std::fs::read_to_string(dir.path().join(mailcraft_store::journal::SNAPSHOT_FILE)).unwrap();
    assert!(written.contains("\"bodies\":2"), "rewritten in the new form");
    assert!(!mailcraft_store::legacy::has_inline(written.as_bytes()), "no content inline any more");
    let s = Session::open(dir.path()).unwrap();
    assert_eq!(s.body(typed).unwrap().unwrap().text.as_deref(), Some("Written here"), "and after a restart");
}

/// A data file carries its messages' sources and the bodies changed here, out of the profile and back.
#[test]
fn data_files_carry_sources_and_edited_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::open(dir.path()).unwrap();
    let r = s.run("file.newDataFile", &json!({"name": "Kept"})).unwrap();
    let acct = crate::model::AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let folder = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    let raw = b"From: ann@example.org\r\nSubject: Carried\r\n\r\nThe source\r\n".to_vec();
    let a = s
        .add_message(
            MailItem { folder, subject: "Carried".into(), ..Default::default() },
            &Content::source(raw.clone()),
        )
        .unwrap();
    let b = s
        .add_message(
            MailItem { folder, subject: "Note".into(), ..Default::default() },
            &Content::source(raw.clone()),
        )
        .unwrap();
    s.edit_body(b, |body| body.text = Some("Changed here".into())).unwrap();
    let typed = MailItem { folder, subject: "Typed".into(), ..Default::default() };
    let c = s
        .add_message(
            typed,
            &Content::body(mailcraft_model::Body { text: Some("Only here".into()), ..Default::default() }),
        )
        .unwrap();
    let _ = (a, c);
    let r = s.run("file.closeDataFile", &json!({"store": acct.0, "confirm": true})).unwrap();
    assert!(r["path"].as_str().is_some());
    assert!(s.mailbox.mail.items().all(|m| m.subject != "Typed"));
    let r = s.run("file.openDataFile", &json!({"name": "Kept"})).unwrap();
    let back = crate::model::AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let folder = s.mailbox.role_folder(back, FolderRole::Inbox).unwrap();
    let by_subject = |s: &Session, subject: &str| {
        s.mailbox.items_in(folder).into_iter().find(|m| m.subject == subject).map(|m| m.id).unwrap()
    };
    let a = by_subject(&s, "Carried");
    let b = by_subject(&s, "Note");
    let c = by_subject(&s, "Typed");
    assert_eq!(s.source(a).unwrap().as_deref(), Some(&raw[..]));
    assert_eq!(s.body(b).unwrap().unwrap().text.as_deref(), Some("Changed here"));
    assert_eq!(s.source(b).unwrap().as_deref(), Some(&raw[..]), "the edited message keeps its source");
    assert_eq!(s.body(c).unwrap().unwrap().text.as_deref(), Some("Only here"));
}

/// The `.body` records in a profile's body store.
fn body_records(dir: &std::path::Path) -> usize {
    fn walk(p: &std::path::Path, n: &mut usize) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, n);
            } else if path.extension().is_some_and(|x| x == mailcraft_store::bodies::BODY_EXT) {
                *n += 1;
            }
        }
    }
    let mut n = 0;
    walk(dir, &mut n);
    n
}

/// Regression: a disk profile never collected content nothing referred to any more (each body saved again
/// left its old record forever). Opening the profile now removes it.
#[test]
fn opening_a_profile_collects_content_nothing_refers_to() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut s = Session::open(dir.path()).unwrap();
        let folder = inbox(&s);
        let id = s
            .add_message(
                MailItem { folder, subject: "Draft".into(), ..Default::default() },
                &Content::body(mailcraft_model::Body { text: Some("v0".into()), ..Default::default() }),
            )
            .unwrap();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        for i in 1..=5 {
            s.run("mail.editMessage", &json!({"ids": [id.0], "text": format!("version {i}")})).unwrap();
        }
        assert_eq!(body_records(dir.path()), 6, "every version is stored while the undo history needs it");
        id
    };
    let s = Session::open(dir.path()).unwrap();
    assert_eq!(body_records(dir.path()), 1, "only the latest version is left");
    assert_eq!(s.body(id).unwrap().unwrap().text.as_deref(), Some("version 5"));
}

/// Regression: purging unused content read the bin from disk only, so entries a failed save left in memory
/// lost their content.
#[test]
fn purging_keeps_the_content_of_the_bin_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::open(dir.path()).unwrap();
    let folder = inbox(&s);
    let raw = b"From: ann@example.org\r\nSubject: Binned\r\n\r\nStill needed\r\n".to_vec();
    let id = s
        .add_message(
            MailItem { folder, subject: "Binned".into(), ..Default::default() },
            &Content::source(raw),
        )
        .unwrap();
    let item = s.mailbox.mail.remove(id).unwrap();
    let key = item.blob.clone().unwrap();
    // In the bin in memory only (as when saving the bin failed).
    s.recover.bin.add(crate::model::DeletedItem::Mail(Box::new(item)), "Inbox", 0);
    s.purge_unused_blobs();
    assert!(s.bodies.has_source(&key), "the bin in memory still refers to it");
    s.recover.bin.items.clear();
    s.purge_unused_blobs();
    assert!(!s.bodies.has_source(&key));
}
