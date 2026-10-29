//! Body store tests: both backends, what `store` keeps, the decoded-body cache, housekeeping, carrying
//! content out and back in, and hostile keys and files.

use super::*;
use mailcraft_model::BodyFormat;

const RAW: &[u8] =
    b"From: ann@example.org\r\nSubject: Hello\r\nContent-Type: text/plain\r\n\r\nHello there.\r\n";

fn text(t: &str) -> Body {
    Body { format: BodyFormat::PlainText, text: Some(t.into()), html: None }
}

fn backends() -> Vec<(BodyStore, Option<tempfile::TempDir>)> {
    let dir = tempfile::tempdir().unwrap();
    vec![(BodyStore::in_memory(), None), (BodyStore::on_disk(dir.path()), Some(dir))]
}

#[test]
fn store_keeps_a_record_only_for_a_body_the_source_does_not_give() {
    for (mut b, _dir) in backends() {
        let mut item = MailItem::default();
        // A source alone, and a source with the body it gives: no record.
        b.store(&mut item, &Content::source(RAW.to_vec())).unwrap();
        let key = item.blob.clone().unwrap();
        assert!(item.body_key.is_none());
        let parsed = Body::from_source(RAW).unwrap();
        let both = Content { source: Some(RAW.to_vec()), body: Some(parsed.clone()) };
        b.store(&mut item, &both).unwrap();
        assert_eq!((item.blob.as_deref(), item.body_key.as_deref()), (Some(key.as_str()), None));
        assert_eq!(*b.body_of(&item).unwrap().unwrap(), parsed);
        // A body changed here: a record, which wins over the source.
        let both = Content { source: Some(RAW.to_vec()), body: Some(text("Edited")) };
        b.store(&mut item, &both).unwrap();
        assert!(item.body_key.is_some());
        assert_eq!(b.body_of(&item).unwrap().unwrap().text.as_deref(), Some("Edited"));
        assert_eq!(b.source_of(&item).unwrap().as_deref(), Some(RAW), "the source is kept as it came");
        // A body alone; nothing at all for an empty body without a source.
        let mut typed = MailItem::default();
        b.store(&mut typed, &Content::body(text("Typed"))).unwrap();
        assert!(typed.blob.is_none() && typed.body_key.is_some());
        let mut empty = MailItem::default();
        b.store(&mut empty, &Content::body(Body::default())).unwrap();
        assert!(!BodyStore::has_content(&empty));
        assert!(b.body_of(&empty).unwrap().is_none());
        // The same content twice is stored once (content-addressed).
        assert_eq!(b.put_source(RAW).unwrap(), key);
        let mut again = MailItem::default();
        b.store_body(&mut again, &text("Typed")).unwrap();
        assert_eq!(again.body_key, typed.body_key);
        assert_eq!(again.preview, "Typed");
        assert_eq!(b.head_of(&item, 5).unwrap().as_deref(), Some(&RAW[..5]));
    }
}

#[test]
fn the_cache_is_capped_in_bytes_and_drops_the_least_recently_used() {
    for (mut b, _dir) in backends() {
        let mut items = Vec::new();
        for i in 0..3 {
            let mut m = MailItem::default();
            b.store(
                &mut m,
                &Content::source(format!("Subject: {i}\r\n\r\n{}\r\n", "x".repeat(2_000)).into_bytes()),
            )
            .unwrap();
            items.push(m);
        }
        b.clear_cache();
        // Room for two bodies of this size, not three.
        let one = b.body_of(&items[0]).unwrap().unwrap().approx_bytes();
        b.set_cache_cap(2 * (one + 200));
        b.clear_cache();
        b.body_of(&items[0]).unwrap();
        b.body_of(&items[1]).unwrap();
        b.body_of(&items[0]).unwrap(); // 0 is now the most recently used
        b.body_of(&items[2]).unwrap(); // 1 goes
        let st = b.cache_stats();
        assert_eq!(st.entries, 2, "{st:?}");
        assert!(st.bytes <= st.cap, "{st:?}");
        let misses = st.misses;
        b.body_of(&items[0]).unwrap();
        assert_eq!(b.cache_stats().misses, misses, "0 was kept");
        b.body_of(&items[1]).unwrap();
        assert_eq!(b.cache_stats().misses, misses + 1, "1 was dropped and read again");
        // A body larger than the whole cache is not kept with the others, and pushes none of them out.
        b.set_cache_cap(10);
        assert_eq!(b.cache_stats().entries, 0);
        assert!(b.body_of(&items[2]).unwrap().unwrap().plain_text().contains("xxx"));
        assert_eq!(b.cache_stats().entries, 0);
        // Regression: such a body was decoded again on every read (every frame of the reading pane). The last
        // one is kept on its own: read twice in a row, it is decoded once.
        let misses = b.cache_stats().misses;
        let x = b.body_of(&items[2]).unwrap().unwrap();
        let y = b.body_of(&items[2]).unwrap().unwrap();
        assert!(Arc::ptr_eq(&x, &y));
        assert_eq!(b.cache_stats().misses, misses, "no read of the store");
        // Another one takes its place.
        b.body_of(&items[1]).unwrap();
        b.body_of(&items[2]).unwrap();
        assert_eq!(b.cache_stats().misses, misses + 2);
        // The same body comes back as the same shared value while cached.
        b.set_cache_cap(DEFAULT_CACHE_BYTES);
        let x = b.body_of(&items[1]).unwrap().unwrap();
        let y = b.body_of(&items[1]).unwrap().unwrap();
        assert!(Arc::ptr_eq(&x, &y));
    }
}

#[test]
fn housekeeping_removes_only_what_nothing_refers_to() {
    for (mut b, dir) in backends() {
        let mut kept = MailItem::default();
        b.store(&mut kept, &Content { source: Some(RAW.to_vec()), body: Some(text("Mine")) }).unwrap();
        let mut gone = MailItem::default();
        b.store(&mut gone, &Content::source(b"Subject: Gone\r\n\r\nbye\r\n".to_vec())).unwrap();
        let live = keys_of([&kept]);
        assert_eq!(live.len(), 2, "its source and its body record");
        // Only the keys asked about, and of those only the ones not in use.
        let both: BTreeSet<String> = keys_of([&kept, &gone]);
        assert_eq!(b.remove_unreferenced(&both, &live).unwrap(), 1);
        assert!(!b.has_source(gone.blob.as_deref().unwrap()));
        assert!(b.has_source(kept.blob.as_deref().unwrap()) && b.has_body(kept.body_key.as_deref().unwrap()));
        assert!(b.body_of(&gone).is_err(), "a missing source is an error, not a panic");
        // A full collection with nothing live removes the rest.
        if let Some(d) = &dir {
            std::fs::write(d.path().join(crate::blob::BLOBS_DIR).join("stray.tmp"), b"half").unwrap();
        }
        let n = b.collect(&BTreeSet::new()).unwrap();
        assert!(n >= 2, "{n}");
        assert!(!b.has_source(kept.blob.as_deref().unwrap()));
        assert_eq!(b.memory_entries(), 0);
    }
}

#[test]
fn export_and_import_carry_content_and_drop_what_does_not_match() {
    let dir = tempfile::tempdir().unwrap();
    for (mut from, mut to) in [
        (BodyStore::in_memory(), BodyStore::on_disk(dir.path())),
        (BodyStore::on_disk(dir.path()), BodyStore::in_memory()),
    ] {
        let mut a = MailItem::default();
        from.store(&mut a, &Content { source: Some(RAW.to_vec()), body: Some(text("Edited")) }).unwrap();
        let mut c = from.export(&keys_of([&a]));
        assert_eq!((c.sources.len(), c.bodies.len()), (1, 1));
        // Hostile entries: a key that is not one, a path, bytes that do not match their key.
        c.sources.insert("../../etc/passwd".into(), b"x".to_vec());
        c.sources.insert(a.body_key.clone().unwrap(), b"not the body".to_vec());
        let mut other = MailItem::default();
        let mut scratch = BodyStore::in_memory();
        scratch.store(&mut other, &Content::source(b"Subject: Other\r\n\r\n\r\n".to_vec())).unwrap();
        c.sources.insert(other.blob.clone().unwrap(), b"Subject: Swapped\r\n\r\n\r\n".to_vec());
        c.bodies.insert("zz".into(), text("x"));
        assert_eq!(to.import(c).unwrap(), 2, "only the real source and body record");
        assert_eq!(to.source_of(&a).unwrap().as_deref(), Some(RAW));
        assert_eq!(to.body_of(&a).unwrap().unwrap().text.as_deref(), Some("Edited"));
        assert!(!to.has_source(other.blob.as_deref().unwrap()));
    }
}

#[test]
fn hostile_keys_and_damaged_files_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = BodyStore::on_disk(dir.path());
    for key in ["", "../../etc/passwd", "zz", "/abs", "a\u{0}b", &"f".repeat(10_000)] {
        assert!(b.source(key).is_err(), "{key:?}");
        assert!(b.body_record(key).is_err(), "{key:?}");
        assert!(!b.has_source(key) && !b.has_body(key));
        assert!(matches!(b.source_head(key, 10), Ok(None) | Err(_)));
        let item =
            MailItem { blob: Some(key.to_owned()), body_key: Some(key.to_owned()), ..Default::default() };
        assert!(b.body_of(&item).is_err());
    }
    // A source file changed on disk no longer matches its key: refused.
    let mut m = MailItem::default();
    b.store(&mut m, &Content { source: Some(RAW.to_vec()), body: Some(text("Edited")) }).unwrap();
    let key = BlobKey::parse(m.blob.as_deref().unwrap()).unwrap();
    let path = content_path(dir.path(), &key, SOURCE_EXT);
    std::fs::write(&path, b"tampered").unwrap();
    assert!(b.source_of(&m).is_err());
    // A body record that is not JSON is an error (and is not cached).
    b.clear_cache();
    let rec = BlobKey::parse(m.body_key.as_deref().unwrap()).unwrap();
    let path = content_path(dir.path(), &rec, BODY_EXT);
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::write(&path, vec![b'{'; usize::try_from(len).unwrap()]).unwrap();
    assert!(b.body_of(&m).is_err());
    // Sources past the size cap are refused before anything is written.
    let huge = vec![b'x'; MAX_BLOB_BYTES + 1];
    assert!(b.put_source(&huge).is_err());
    assert!(b.put_body(&text(&"y".repeat(MAX_BODY_RECORD_BYTES))).is_err(), "body records are capped too");
}
