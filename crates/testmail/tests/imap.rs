//! IMAP server, spoken to in raw protocol.

mod common;

use std::time::{Duration, Instant};

use common::*;
use mailcraft_testmail::{Config, DateTime};

#[test]
fn greeting_capability_login() {
    let s = servers();
    let mut c = Client::connect(s.imap_port);
    let greet = c.line();
    assert!(greet.starts_with("* OK [CAPABILITY IMAP4rev1 IMAP4rev2"), "{greet}");
    let caps = c.ok("CAPABILITY");
    for cap in ["STARTTLS", "AUTH=PLAIN", "IDLE", "MOVE", "UIDPLUS", "SPECIAL-USE", "CONDSTORE", "LITERAL+"] {
        assert!(caps.contains(cap), "missing {cap}: {caps}");
    }
    let r = c.imap("SELECT INBOX");
    assert!(r.contains(" BAD "), "{r}");
    let r = c.imap(&format!("LOGIN {ALICE} wrong"));
    assert!(r.contains("NO [AUTHENTICATIONFAILED]"), "{r}");
    let r = c.ok(&format!("LOGIN \"{ALICE}\" \"alice-pw\""));
    assert!(r.contains("[CAPABILITY "), "{r}");
    let r = c.imap("LOGIN x y");
    assert!(r.contains(" BAD "), "{r}");
    let r = c.ok("LOGOUT");
    assert!(r.contains("* BYE"));
    assert_eq!(c.line(), "", "server closes after LOGOUT");
}

#[test]
fn starttls_and_implicit_tls() {
    let s = servers_with(Config { auth_requires_tls: true, ..Config::default() });
    let tls = s.client_tls_config().unwrap();
    assert!(s.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));

    let mut c = Client::connect(s.imap_port);
    let greet = c.line();
    assert!(greet.contains("LOGINDISABLED") && greet.contains("STARTTLS"), "{greet}");
    let r = c.imap(&format!("LOGIN {ALICE} alice-pw"));
    assert!(r.contains("NO [PRIVACYREQUIRED]"), "{r}");
    c.ok("STARTTLS");
    c.upgrade(tls.clone());
    let caps = c.ok("CAPABILITY");
    assert!(!caps.contains("STARTTLS") && caps.contains("AUTH=PLAIN"), "{caps}");
    c.ok(&format!("LOGIN {ALICE} alice-pw"));
    c.ok("SELECT INBOX");

    let mut c = Client::connect_tls(s.imaps_port, tls);
    assert!(c.line().starts_with("* OK"));
    c.ok(&format!("LOGIN {BOB} bob-pw"));
    c.ok("NOOP");
}

#[test]
fn authenticate_plain() {
    let s = servers();
    let mut c = Client::connect(s.imap_port);
    c.line();
    let resp = b64(format!("\0{ALICE}\0alice-pw").as_bytes());
    c.ok(&format!("AUTHENTICATE PLAIN {resp}"));

    let mut c = Client::connect(s.imap_port);
    c.line();
    c.send_str("t1 AUTHENTICATE PLAIN\r\n");
    assert!(c.line().starts_with("+"));
    c.send_str(&format!("{}\r\n", b64(format!("{BOB}\0{BOB}\0bob-pw").as_bytes())));
    let (_, tagged) = c.until_tagged("t1");
    assert!(tagged.starts_with("t1 OK"), "{tagged}");

    let mut c = Client::connect(s.imap_port);
    c.line();
    c.send_str("t1 AUTHENTICATE PLAIN\r\n");
    c.line();
    c.send_str("*\r\n");
    assert!(c.until_tagged("t1").1.starts_with("t1 BAD"));
    let r = c.imap(&format!("AUTHENTICATE PLAIN {}", b64(b"\0alice@example.com\0nope")));
    assert!(r.contains("NO [AUTHENTICATIONFAILED]"), "{r}");
    let r = c.imap("AUTHENTICATE CRAM-MD5");
    assert!(r.contains(" NO "), "{r}");
}

#[test]
fn list_special_use_and_status() {
    let s = servers();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    let r = c.ok("LIST \"\" \"*\"");
    assert!(r.contains("* LIST (\\HasNoChildren) \"/\" \"INBOX\""), "{r}");
    for (attr, name) in [
        ("\\Sent", "Sent"),
        ("\\Drafts", "Drafts"),
        ("\\Trash", "Trash"),
        ("\\Junk", "Junk"),
        ("\\Archive", "Archive"),
    ] {
        assert!(r.contains(&format!("(\\HasNoChildren {attr}) \"/\" \"{name}\"")), "{attr}: {r}");
    }
    let r = c.ok("LIST \"\" \"\"");
    assert!(r.contains("* LIST (\\Noselect) \"/\" \"\""), "{r}");
    c.ok("CREATE Projects/2026");
    let r = c.ok("LIST \"\" \"%\"");
    assert!(r.contains("(\\HasChildren) \"/\" \"Projects\"") && !r.contains("Projects/2026"), "{r}");
    let r = c.ok("LIST \"Projects/\" \"%\"");
    assert!(r.contains("\"Projects/2026\""), "{r}");
    let r = c.ok("LIST (SPECIAL-USE) \"\" \"*\"");
    assert!(!r.contains("\"INBOX\"") && r.contains("\\Sent"), "{r}");
    s.seed_message(ALICE, "INBOX", &sample("one", "x"), &[]).unwrap();
    let r = c.ok("LIST \"\" \"INBOX\" RETURN (SUBSCRIBED STATUS (MESSAGES UNSEEN))");
    assert!(r.contains("\\Subscribed"), "{r}");
    assert!(r.contains("* STATUS \"INBOX\" (MESSAGES 1 UNSEEN 1)"), "{r}");
    let r = c.ok("STATUS inbox (MESSAGES UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ SIZE)");
    assert!(r.contains("MESSAGES 1 UIDNEXT 2 UIDVALIDITY "), "{r}");
    let r = c.ok("LSUB \"\" \"*\"");
    assert!(r.contains("* LSUB"), "{r}");
    c.ok("UNSUBSCRIBE Junk");
    let r = c.ok("LSUB \"\" \"Junk\"");
    assert!(!r.contains("* LSUB"), "{r}");
    let r = c.imap("STATUS Nope (MESSAGES)");
    assert!(r.contains("NO [NONEXISTENT]"), "{r}");
    c.ok("NAMESPACE");
    c.ok("ID NIL");
}

#[test]
fn select_examine_create_delete_rename() {
    let s = servers();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    s.seed_message(ALICE, "INBOX", &sample("a", "1"), &["\\Seen"]).unwrap();
    s.seed_message(ALICE, "INBOX", &sample("b", "2"), &[]).unwrap();
    let r = c.ok("SELECT INBOX");
    assert!(r.contains("* 2 EXISTS"), "{r}");
    assert!(r.contains("* OK [UNSEEN 2]"), "{r}");
    assert!(r.contains("[UIDVALIDITY ") && r.contains("[UIDNEXT 3]") && r.contains("[HIGHESTMODSEQ "), "{r}");
    assert!(r.contains("[PERMANENTFLAGS (") && r.contains("[READ-WRITE]"), "{r}");
    let r = c.ok("EXAMINE INBOX");
    assert!(r.contains("[READ-ONLY]") && r.contains("[CLOSED]"), "{r}");
    let r = c.imap("UID STORE 1 +FLAGS (\\Flagged)");
    assert!(r.contains(" NO "), "read-only: {r}");

    c.ok("CREATE Work");
    let r = c.imap("CREATE Work");
    assert!(r.contains("NO [ALREADYEXISTS]"), "{r}");
    c.ok("RENAME Work Projects/Work");
    assert!(s.mailboxes(ALICE).unwrap().contains(&"Projects/Work".to_owned()));
    let r = c.imap("SELECT Work");
    assert!(r.contains("NO [NONEXISTENT]"), "{r}");
    c.ok("DELETE Projects/Work");
    let r = c.imap("DELETE INBOX");
    assert!(r.contains(" NO "), "{r}");
    c.ok("CREATE \"Saved Drafts\" (USE (\\Drafts))");
    let r = c.ok("LIST \"\" \"Saved*\"");
    assert!(r.contains("\\Drafts"), "{r}");
    assert!(c.imap("UNSELECT").contains(" BAD "), "a failed SELECT leaves no mailbox selected");
    c.ok("SELECT INBOX");
    c.ok("UNSELECT");
    let r = c.imap("UID FETCH 1 FLAGS");
    assert!(r.contains(" BAD "), "{r}");
}

#[test]
fn append_with_literals() {
    let s = servers();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    let msg = sample("appended", "Hello from APPEND\r\n");
    c.send_str(&format!(
        "a1 APPEND Drafts (\\Draft \\Seen) \"10-Oct-2026 09:30:00 -0400\" {{{}}}\r\n",
        msg.len()
    ));
    let cont = c.line();
    assert!(cont.starts_with("+ "), "{cont}");
    c.send(&msg);
    c.send_str("\r\n");
    let (_, tagged) = c.until_tagged("a1");
    assert!(tagged.starts_with("a1 OK [APPENDUID ") && tagged.contains(" 1]"), "{tagged}");
    // LITERAL+ (non-synchronising): no continuation request.
    c.send_str(&format!("a2 APPEND INBOX {{{}+}}\r\n", msg.len()));
    c.send(&msg);
    c.send_str("\r\n");
    let (_, tagged) = c.until_tagged("a2");
    assert!(tagged.starts_with("a2 OK [APPENDUID "), "{tagged}");
    let r = c.imap("APPEND Nowhere {1+}\r\nx");
    assert!(r.contains("NO [TRYCREATE]"), "{r}");

    let drafts = s.messages(ALICE, "Drafts").unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].raw, msg);
    assert_eq!(drafts[0].flags, vec!["\\Draft".to_owned(), "\\Seen".to_owned()]);
    assert_eq!(drafts[0].internal_date.to_imap(), "10-Oct-2026 09:30:00 -0400");
    assert_eq!(s.messages(ALICE, "INBOX").unwrap().len(), 1);
}

#[test]
fn fetch_items_and_seen() {
    let s = servers();
    let multi = b"From: \"Doe, Jane\" <jane@example.org>\r\nTo: alice@example.com\r\nCc: Team: a@example.net;\r\n\
Subject: Report\r\nDate: Fri, 09 Oct 2026 08:00:00 +0000\r\nMessage-ID: <r1@example.org>\r\n\
Content-Type: multipart/mixed; boundary=\"zz\"\r\n\r\n--zz\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee attached.\r\n\
--zz\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\nContent-Disposition: attachment; filename=\"r.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--zz--\r\n";
    let uid = s.seed_message(ALICE, "INBOX", multi, &[]).unwrap();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    c.ok("SELECT INBOX");
    let r = c.ok(&format!("UID FETCH {uid} (FLAGS INTERNALDATE RFC822.SIZE ENVELOPE)"));
    assert!(r.contains(&format!("* 1 FETCH (UID {uid} FLAGS () INTERNALDATE \"")), "{r}");
    assert!(r.contains(&format!("RFC822.SIZE {}", multi.len())), "{r}");
    assert!(r.contains("ENVELOPE (\"Fri, 09 Oct 2026 08:00:00 +0000\" \"Report\" ((\"Doe, Jane\" NIL \"jane\" \"example.org\"))"), "{r}");
    assert!(r.contains("((NIL NIL \"Team\" NIL)(NIL NIL \"a\" \"example.net\")(NIL NIL NIL NIL))"), "{r}");
    assert!(r.contains("\"<r1@example.org>\")"), "{r}");

    let r = c.ok("UID FETCH 1:* BODYSTRUCTURE");
    assert!(r.contains("BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 13 1 NIL NIL NIL NIL)"), "{r}");
    assert!(r.contains("(\"APPLICATION\" \"PDF\" (\"NAME\" \"r.pdf\") NIL NIL \"BASE64\" 8 NIL (\"ATTACHMENT\" (\"FILENAME\" \"r.pdf\")) NIL NIL) \"MIXED\""), "{r}");

    let r = c.ok(
        "UID FETCH 1 (BODY.PEEK[HEADER.FIELDS (Subject)] BODY.PEEK[1] BODY.PEEK[2.MIME] BODY.PEEK[]<0.4>)",
    );
    assert!(r.contains("BODY[HEADER.FIELDS (SUBJECT)] {19}\r\nSubject: Report\r\n\r\n"), "{r}");
    assert!(r.contains("BODY[1] {13}\r\nSee attached."), "{r}");
    assert!(r.contains("BODY[2.MIME] {"), "{r}");
    assert!(r.contains("BODY[]<0> {4}\r\nFrom"), "{r}");
    assert!(s.messages(ALICE, "INBOX").unwrap()[0].flags.is_empty(), "PEEK must not set \\Seen");

    let r = c.ok("UID FETCH 1 BODY[]");
    assert!(r.contains(&format!("BODY[] {{{}}}\r\n", multi.len())), "{r}");
    assert!(r.contains("FLAGS (\\Seen)"), "implicit \\Seen reported: {r}");
    assert_eq!(s.messages(ALICE, "INBOX").unwrap()[0].flags, vec!["\\Seen".to_owned()]);
    let r = c.ok("FETCH 1 (UID RFC822.HEADER)");
    assert!(r.contains("RFC822.HEADER {"), "{r}");
    let r = c.ok("FETCH 1 FAST");
    assert!(r.contains("FLAGS (\\Seen) INTERNALDATE"), "{r}");
    let r = c.imap("FETCH 1 (BOGUS)");
    assert!(r.contains(" BAD "), "{r}");
    let r = c.ok("UID FETCH 99 FLAGS");
    assert!(!r.contains("FETCH ("), "{r}");
}

#[test]
fn search_keys_and_esearch() {
    let s = servers();
    let old = DateTime::parse_imap("01-Sep-2026 12:00:00 +0000").unwrap();
    let new = DateTime::parse_imap("05-Oct-2026 12:00:00 +0000").unwrap();
    s.seed_message_at(ALICE, "INBOX", &sample("old news", "alpha"), &["\\Seen"], old).unwrap();
    s.seed_message_at(ALICE, "INBOX", &sample("fresh news", "beta"), &[], new).unwrap();
    s.seed_message_at(ALICE, "INBOX", &sample("flagged", "gamma"), &["\\Flagged", "\\Seen"], new).unwrap();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    c.ok("SELECT INBOX");
    assert!(c.ok("UID SEARCH ALL").contains("* SEARCH 1 2 3\r\n"));
    assert!(c.ok("UID SEARCH SINCE 1-Oct-2026").contains("* SEARCH 2 3\r\n"));
    assert!(c.ok("UID SEARCH BEFORE 1-Oct-2026").contains("* SEARCH 1\r\n"));
    assert!(c.ok("UID SEARCH UNSEEN").contains("* SEARCH 2\r\n"));
    assert!(c.ok("SEARCH FLAGGED").contains("* SEARCH 3\r\n"));
    assert!(c.ok("UID SEARCH SUBJECT news NOT SEEN").contains("* SEARCH 2\r\n"));
    assert!(c.ok("UID SEARCH OR BODY alpha BODY gamma").contains("* SEARCH 1 3\r\n"));
    assert!(c.ok("UID SEARCH FROM carol@EXAMPLE.org UID 2:*").contains("* SEARCH 2 3\r\n"));
    assert!(c.ok("UID SEARCH CHARSET UTF-8 TEXT beta").contains("* SEARCH 2\r\n"));
    assert!(c.ok("UID SEARCH SENTON 10-Oct-2026").contains("* SEARCH 1 2 3\r\n"));
    assert!(c.ok("UID SEARCH LARGER 100000").contains("* SEARCH\r\n"));
    let r = c.ok("UID SEARCH RETURN (MIN MAX COUNT ALL) SEEN");
    assert!(r.contains("UID MIN 1 MAX 3 COUNT 2 ALL 1,3"), "{r}");
    let r = c.ok("UID SEARCH MODSEQ 1");
    assert!(r.contains("* SEARCH 1 2 3 (MODSEQ "), "{r}");
    let r = c.imap("UID SEARCH CHARSET KOI8-R ALL");
    assert!(r.contains("NO [BADCHARSET"), "{r}");
    let r = c.imap("UID SEARCH FOO");
    assert!(r.contains(" BAD "), "{r}");
    c.ok("ENABLE IMAP4rev2");
    let r = c.ok("UID SEARCH UNSEEN");
    assert!(r.contains("* ESEARCH (TAG \"") && r.contains("UID ALL 2"), "{r}");
}

#[test]
fn store_flags_and_condstore() {
    let s = servers();
    for i in 0..3 {
        s.seed_message(ALICE, "INBOX", &sample(&format!("m{i}"), "x"), &[]).unwrap();
    }
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    let r = c.ok("ENABLE CONDSTORE");
    assert!(r.contains("* ENABLED CONDSTORE"), "{r}");
    c.ok("SELECT INBOX (CONDSTORE)");
    let r = c.ok("UID STORE 1:2 +FLAGS (\\Seen \\Flagged)");
    assert!(r.contains("* 1 FETCH (UID 1 FLAGS (\\Seen \\Flagged) MODSEQ ("), "{r}");
    assert!(r.contains("* 2 FETCH (UID 2 FLAGS"), "{r}");
    let r = c.ok("UID STORE 2 -FLAGS.SILENT (\\Flagged)");
    assert!(r.contains("* 2 FETCH (UID 2 MODSEQ ("), "silent+condstore reports MODSEQ: {r}");
    let r = c.ok("STORE 3 FLAGS ($Label1 \\Answered)");
    assert!(r.contains("* 3 FETCH (FLAGS ($Label1 \\Answered) MODSEQ ("), "{r}");
    let msgs = s.messages(ALICE, "INBOX").unwrap();
    assert_eq!(msgs[0].flags, vec!["\\Seen".to_owned(), "\\Flagged".to_owned()]);
    assert_eq!(msgs[1].flags, vec!["\\Seen".to_owned()]);
    // UNCHANGEDSINCE: message 1 changed after modseq 1.
    let r = c.ok("UID STORE 1,3 (UNCHANGEDSINCE 1) +FLAGS (\\Deleted)");
    assert!(r.contains("[MODIFIED 1,3]"), "{r}");
    let ms = msgs[0].modseq;
    let r = c.ok(&format!("UID STORE 1 (UNCHANGEDSINCE {ms}) +FLAGS (\\Deleted)"));
    assert!(!r.contains("MODIFIED"), "{r}");
    let highest_before = msgs.iter().map(|m| m.modseq).max().unwrap();
    let r = c.ok(&format!("UID FETCH 1:* (FLAGS) (CHANGEDSINCE {highest_before})"));
    assert!(r.contains("UID 1 FLAGS (\\Seen \\Flagged \\Deleted) MODSEQ ("), "{r}");
    assert!(!r.contains("UID 2 "), "{r}");
    let r = c.imap("UID STORE 1 +FLAGS (\\Bogus)");
    assert!(r.contains(" BAD "), "{r}");
}

#[test]
fn copy_move_expunge() {
    let s = servers();
    for i in 0..4 {
        s.seed_message(ALICE, "INBOX", &sample(&format!("m{i}"), "x"), &[]).unwrap();
    }
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    c.ok("SELECT INBOX");
    let r = c.ok("UID COPY 1:2 Archive");
    assert!(r.contains("[COPYUID ") && r.contains(" 1:2 1:2]"), "{r}");
    assert_eq!(s.messages(ALICE, "Archive").unwrap().len(), 2);
    let r = c.imap("UID COPY 1 Nowhere");
    assert!(r.contains("NO [TRYCREATE]"), "{r}");

    let r = c.ok("UID MOVE 2:3 Trash");
    assert!(r.contains("* OK [COPYUID ") && r.contains(" 2:3 1:2]"), "{r}");
    assert!(r.contains("* 3 EXPUNGE\r\n* 2 EXPUNGE\r\n"), "{r}");
    let inbox: Vec<u32> = s.messages(ALICE, "INBOX").unwrap().iter().map(|m| m.uid).collect();
    assert_eq!(inbox, vec![1, 4]);

    c.ok("UID STORE 1,4 +FLAGS.SILENT (\\Deleted)");
    let r = c.ok("UID EXPUNGE 4");
    assert!(r.contains("* 2 EXPUNGE"), "{r}");
    assert_eq!(s.messages(ALICE, "INBOX").unwrap().len(), 1);
    let r = c.ok("EXPUNGE");
    assert!(r.contains("* 1 EXPUNGE"), "{r}");
    let r = c.ok("STATUS Trash (MESSAGES UIDNEXT)");
    assert!(r.contains("MESSAGES 2 UIDNEXT 3"), "{r}");
    c.ok("SELECT Trash");
    c.ok("UID STORE 1 +FLAGS (\\Deleted)");
    c.ok("CLOSE");
    assert_eq!(s.messages(ALICE, "Trash").unwrap().len(), 1);
}

#[test]
fn idle_sees_other_sessions_and_api() {
    let s = servers();
    s.seed_message(ALICE, "INBOX", &sample("first", "x"), &[]).unwrap();
    let mut a = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    a.ok("SELECT INBOX");
    a.send_str("i1 IDLE\r\n");
    assert!(a.line().starts_with("+ "));

    let mut b = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    let msg = sample("second", "y");
    b.send_str(&format!("b1 APPEND INBOX {{{}+}}\r\n", msg.len()));
    b.send(&msg);
    b.send_str("\r\n");
    assert!(b.until_tagged("b1").1.starts_with("b1 OK"));
    let started = Instant::now();
    assert_eq!(a.line(), "* 2 EXISTS\r\n");
    assert!(started.elapsed() < Duration::from_secs(2));

    s.seed_message(ALICE, "INBOX", &sample("third", "z"), &[]).unwrap();
    assert_eq!(a.line(), "* 3 EXISTS\r\n");

    b.ok("SELECT INBOX");
    b.ok("UID STORE 1 +FLAGS (\\Seen)");
    assert_eq!(a.line(), "* 1 FETCH (UID 1 FLAGS (\\Seen))\r\n");
    b.ok("UID MOVE 2 Archive");
    assert_eq!(a.line(), "* 2 EXPUNGE\r\n");

    a.send_str("DONE\r\n");
    let (_, tagged) = a.until_tagged("i1");
    assert!(tagged.starts_with("i1 OK"), "{tagged}");
    let r = a.ok("UID SEARCH ALL");
    assert!(r.contains("* SEARCH 1 3\r\n"), "{r}");
}

#[test]
fn pending_expunge_is_reported_on_noop_not_fetch() {
    let s = servers();
    for i in 0..2 {
        s.seed_message(ALICE, "INBOX", &sample(&format!("m{i}"), "x"), &["\\Deleted"]).unwrap();
    }
    let mut a = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    a.ok("SELECT INBOX");
    let mut b = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    b.ok("SELECT INBOX");
    b.ok("UID EXPUNGE 1");
    let r = a.ok("FETCH 1:* FLAGS");
    assert!(!r.contains("EXPUNGE"), "no EXPUNGE during FETCH: {r}");
    assert!(r.contains("* 2 FETCH (FLAGS (\\Deleted))"), "{r}");
    let r = a.ok("NOOP");
    assert!(r.contains("* 1 EXPUNGE"), "{r}");
}

#[test]
fn junk_input_does_not_kill_the_server() {
    let s = servers();
    let mut c = Client::connect(s.imap_port);
    c.line();
    for junk in [
        "\r\n",
        "*\r\n",
        "a\r\n",
        "a FETCH\r\n",
        "a UID\r\n",
        "a UID FETCH 1:* (\r\n",
        "a SEARCH (((((\r\n",
        "a LOGIN \"unterminated\r\n",
        "a STORE 0:0 FLAGS ()\r\n",
        "a LIST (BOGUS) \"\" *\r\n",
        "a \u{1}\u{2}\r\n",
    ] {
        c.send_str(junk);
    }
    // A huge synchronising literal is refused without reading it.
    c.send_str("big APPEND INBOX {999999999999}\r\n");
    let r = c.imap("NOOP");
    assert!(r.contains("big BAD [TOOBIG]") || r.contains("big BAD"), "{r}");
    c.ok(&format!("LOGIN {ALICE} alice-pw"));
    c.ok("SELECT INBOX");
    for junk in [
        "a FETCH 1:* (BODY[1.2.3.4.MIME]<0.0>)",
        "a UID FETCH * BODY[9]",
        "a UID SEARCH UID $",
        "a MOVE 1 \"\"",
    ] {
        let r = c.imap(junk.trim_start_matches("a "));
        assert!(r.contains(" OK ") || r.contains(" NO ") || r.contains(" BAD "), "{r}");
    }
    // Server still serves new clients.
    let mut d = Client::imap_login(s.imap_port, BOB, "bob-pw");
    d.ok("NOOP");
}

#[test]
fn shutdown_on_drop() {
    let s = servers();
    let port = s.imap_port;
    let mut c = Client::imap_login(port, ALICE, "alice-pw");
    c.ok("SELECT INBOX");
    c.send_str("i IDLE\r\n");
    c.line();
    let started = Instant::now();
    drop(s);
    assert!(started.elapsed() < Duration::from_secs(5), "drop joined promptly");
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "listener closed");
    assert_eq!(c.line(), "", "connection closed");
}

#[test]
fn mailbox_names_use_modified_utf7_unless_imap4rev2_is_enabled() {
    // RFC 3501 section 5.1.3: IMAP4rev1 sessions exchange mailbox names in modified UTF-7.
    let s = servers();
    s.create_mailbox(ALICE, "Café", None).unwrap();
    let mut c = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    let r = c.ok("LIST \"\" \"Caf*\"");
    assert!(r.contains("\"Caf&AOk-\""), "{r}");
    assert!(!r.contains("Café"), "{r}");
    let r = c.ok("STATUS Caf&AOk- (MESSAGES)");
    assert!(r.contains("* STATUS \"Caf&AOk-\" (MESSAGES 0)"), "{r}");
    c.ok("SELECT Caf&AOk-");
    c.ok("CREATE Entw&APw-rfe");
    assert!(s.mailboxes(ALICE).unwrap().contains(&"Entwürfe".to_owned()));
    c.ok("RENAME Entw&APw-rfe \"Tom &- Jerry\"");
    assert!(s.mailboxes(ALICE).unwrap().contains(&"Tom & Jerry".to_owned()));
    s.seed_message(ALICE, "INBOX", &sample("a", "1"), &[]).unwrap();
    c.ok("SELECT INBOX");
    c.ok("UID COPY 1 Caf&AOk-");
    assert_eq!(s.messages(ALICE, "Café").unwrap().len(), 1);
    // After ENABLE IMAP4rev2 names are UTF-8 (RFC 9051 section 5.1).
    let mut d = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    d.ok("ENABLE IMAP4rev2");
    let r = d.ok("LIST \"\" \"Caf*\"");
    assert!(r.contains("Café"), "{r}");
    assert!(!r.contains("&AOk-"), "{r}");
}
