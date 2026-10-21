//! The appointment and meeting window's fields (time zones, formatted notes, files, importance, rooms),
//! sending a new meeting from its window, Send a Calendar via E-mail's message body, copying a published
//! calendar, and Update Folder on a calendar kept on this computer only.

use serde_json::{Value, json};

use super::tests::{role, session};
use super::*;
use mailcraft_model::calendar::{Publication, Withdrawal};
use mailcraft_model::{FolderRole, RemoteRef};

fn cal(s: &Session) -> FolderId {
    role(s, FolderRole::Calendar)
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn outbox(s: &Session) -> FolderId {
    role(s, FolderRole::Outbox)
}

/// A session whose default account can send (its messages wait in the Outbox: nothing connects).
fn sending_session() -> Session {
    let mut mb = mailcraft_model::Mailbox::new_local("Local");
    let acc = mb.add_account(mailcraft_model::Account {
        kind: mailcraft_model::AccountKind::Imap,
        display_name: "me@example.com".into(),
        address: "me@example.com".into(),
        sender_name: "Me Example".into(),
        outgoing: Some(mailcraft_model::ServerSettings {
            host: "smtp.example.com".into(),
            port: 465,
            ..Default::default()
        }),
        ..Default::default()
    });
    mb.create_default_folders(acc);
    for a in &mut mb.accounts {
        a.is_default = a.id == acc;
    }
    let mut s = Session::new(mb);
    s.fixed_now = mailcraft_model::dates::local_datetime(2026, 10, 9, 10, 7, 0, 0);
    s.view.send_immediately = false;
    s.refresh_me();
    s
}

#[test]
fn window_fields_are_saved_and_listed() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({
            "subject": "Flight",
            "start": "2026-11-02T18:00",
            "end": "2026-11-03T06:00",
            "timeZone": "America/Toronto",
            "endTimeZone": "Europe/London",
            "importance": "high",
            "private": true,
            "bodyHtml": "<p>Gate <b>4</b></p><script>alert(1)</script>",
            "attachments": [{"name": "pass.pdf", "data_base64": "JVBERi0xLjQ="}],
        }),
    );
    let id = ItemId(r["id"].as_u64().unwrap());
    let a = s.mailbox.appointments[&id].clone();
    // 18:00 in Toronto (UTC-5 in November) and 06:00 in London (UTC+0).
    assert_eq!(a.start.unix, pim::parse_local("2026-11-02T23:00", 0).unwrap().unix);
    assert_eq!(a.end.unix, pim::parse_local("2026-11-03T06:00", 0).unwrap().unix);
    assert_eq!(a.zone(), Some("America/Toronto"));
    assert_eq!(a.end_tz.as_deref(), Some("Europe/London"));
    assert_eq!(a.importance, Importance::High);
    assert!(a.private);
    let html = a.body_html.clone().unwrap();
    assert!(html.contains("<b>4</b>") || html.contains("font-weight"), "{html}");
    assert!(!html.contains("script"), "{html}");
    assert_eq!(a.body, "Gate 4");
    assert_eq!(a.attachments.len(), 1);
    assert_eq!(a.attachments[0].filename, "pass.pdf");
    assert_eq!(a.attachments[0].mime_type, "application/pdf");
    assert_eq!(&*a.attachments[0].data, b"%PDF-1.4");
    assert_eq!(r["importance"], "high");
    assert_eq!(r["endTimeZone"], "Europe/London");
    assert_eq!(r["attachments"][0]["name"], "pass.pdf");
    assert_eq!(r["attachments"][0]["size"], 8);

    // Plain notes replace the formatted ones; an end zone equal to the start's is no separate zone; the
    // files stay unless the call names them.
    run(&mut s, "calendar.update", json!({"id": id.0, "body": "Gate 5", "endTimeZone": "America/Toronto"}));
    let a = s.mailbox.appointments[&id].clone();
    assert_eq!((a.body.as_str(), a.body_html.as_deref()), ("Gate 5", None));
    assert_eq!(a.end_tz, None);
    assert_eq!(a.attachments.len(), 1);
    run(&mut s, "calendar.update", json!({"id": id.0, "attachments": [], "importance": "low"}));
    let a = s.mailbox.appointments[&id].clone();
    assert!(a.attachments.is_empty());
    assert_eq!(a.importance, Importance::Low);

    // Hostile values are refused, never a panic.
    for bad in [
        json!({"id": id.0, "importance": "urgent"}),
        json!({"id": id.0, "endTimeZone": "Mars/Base"}),
        json!({"id": id.0, "attachments": "x"}),
        json!({"id": id.0, "attachments": [1]}),
        json!({"id": id.0, "attachments": [{"name": "x"}]}),
        json!({"id": id.0, "bodyHtml": 5}),
    ] {
        assert!(s.run("calendar.update", &bad).is_err(), "{bad}");
    }
}

#[test]
fn a_form_in_other_zones_shows_its_times_there() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"form": true, "subject": "Call", "start": "2026-11-02T09:00", "end": "2026-11-02T10:00",
               "timeZone": "Asia/Tokyo", "importance": "high", "bodyHtml": "<p><i>Agenda</i></p>"}),
    );
    assert_eq!(r["dialog"], "appointment");
    let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last().cloned() else { panic!("no form") };
    assert_eq!(p["zonesShown"], true);
    assert_eq!(p["timeZone"], "Asia/Tokyo");
    assert_eq!(p["start"], "2026-11-02T09:00", "the times read in the item's zone");
    assert_eq!(p["importance"], "high");
    assert!(p["bodyHtml"].as_str().unwrap().contains("Agenda"));
    assert!(s.mailbox.appointments.is_empty());
}

fn meeting(rooms: bool) -> Value {
    let mut p = json!({
        "subject": "Planning",
        "start": "2026-10-12T10:00",
        "end": "2026-10-12T11:00",
        "attendees": ["Pat Example <pat@example.net>"],
        "importance": "high",
        "bodyHtml": "<p>Bring the <b>numbers</b></p>",
        "attachments": [{"name": "agenda.txt", "data_base64": "YWdlbmRh"}],
    });
    if rooms {
        p["resources"] = json!(["Room 4 <room4@example.com>"]);
    }
    p
}

#[test]
fn a_new_meeting_sends_its_invitation_from_the_window() {
    let mut s = sending_session();
    let mut p = meeting(true);
    p["send"] = json!("now");
    let r = run(&mut s, "calendar.newMeeting", p);
    assert_eq!(r["sent"], true, "{r}");
    assert!(r.get("compose").is_none(), "no second compose: {r}");
    assert!(s.composes.is_empty());
    let id = ItemId(r["id"].as_u64().unwrap());
    let a = &s.mailbox.appointments[&id];
    let room = a.attendees.iter().find(|x| x.role == AttendeeRole::Resource).unwrap();
    assert_eq!(room.mailbox.addr, "room4@example.com");
    // The invitation waits in the Outbox: the room is invited as a resource, the file and the formatted
    // notes go with it, and it is marked important.
    let out = outbox(&s);
    let m = s.mailbox.mail.values().find(|m| m.folder == out).expect("the invitation is in the Outbox");
    assert_eq!(m.subject, "Planning");
    assert_eq!(m.importance, Importance::High);
    assert!(m.to.iter().any(|x| x.addr == "room4@example.com"));
    let names: Vec<&str> = m.attachments.iter().map(|x| x.filename.as_str()).collect();
    assert!(names.contains(&"agenda.txt"), "{names:?}");
    let ics =
        String::from_utf8(calendar_data(&s, m).expect("the invitation carries its calendar part")).unwrap();
    assert!(ics.contains("METHOD:REQUEST"), "{ics}");
    assert!(ics.contains("CUTYPE=RESOURCE"), "{ics}");
    assert!(ics.contains("PRIORITY:1"), "{ics}");
}

#[test]
fn a_meeting_that_cannot_be_sent_leaves_nothing_behind() {
    let mut s = sending_session();
    let mut p = meeting(false);
    p["send"] = json!("now");
    p["attendees"] = json!(["not an address"]);
    let e = s.run("calendar.newMeeting", &p).unwrap_err().to_string();
    assert!(e.contains("not sent"), "{e}");
    assert!(s.mailbox.appointments.is_empty());
    assert!(s.composes.is_empty());
    assert!(s.mailbox.mail.values().all(|m| m.folder != outbox(&s)));
    assert!(s.run("calendar.newMeeting", &json!({"start": "2026-10-12T10:00", "send": "none"})).is_err());
}

#[test]
fn discarding_the_invitation_removes_the_unsent_meeting() {
    let mut s = sending_session();
    // Discard.
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    assert!(s.mailbox.appointments.contains_key(&id));
    let c = &s.composes[&cid];
    assert_eq!(c.importance, Importance::High);
    assert!(c.attachments.iter().any(|x| x.filename == "agenda.txt"));
    assert!(c.attachments.iter().any(|x| x.filename == "invite.ics"));
    assert!(mailcraft_html::doc_to_text(&c.body).contains("numbers"));
    let d = run(&mut s, "compose.discard", json!({"compose": cid}));
    assert_eq!(d["meetingRemoved"], id.0);
    assert!(!s.mailbox.appointments.contains_key(&id));
    // Close without saving.
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    run(&mut s, "compose.close", json!({"compose": cid, "save": false}));
    assert!(!s.mailbox.appointments.contains_key(&id));
    // Sent: the meeting stays.
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    let sent = run(&mut s, "compose.send", json!({"compose": cid}));
    assert_eq!(sent["sent"], true, "{sent}");
    assert!(s.mailbox.appointments.contains_key(&id));
    // Send Update straight from the window.
    let u = run(&mut s, "calendar.sendUpdate", json!({"id": id.0, "send": "now"}));
    assert_eq!(u["sent"], true, "{u}");
    assert!(s.composes.is_empty());
}

#[test]
fn a_saved_invitation_keeps_its_meeting_when_closed() {
    // Regression: Save (or auto-save) cleared the invitation's dirty flag, so closing it afterwards took the
    // meeting off the calendar while the invitation stayed in Drafts.
    let mut s = sending_session();
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    run(&mut s, "compose.saveDraft", json!({"compose": cid}));
    let draft = s.composes[&cid].draft.unwrap();
    let c = run(&mut s, "compose.close", json!({"compose": cid}));
    assert_eq!(c["meetingRemoved"], Value::Null, "{c}");
    assert!(s.mailbox.appointments.contains_key(&id));
    assert!(s.mailbox.mail.contains_key(&draft));
    // Saved, then closed with Don't Save or Save: the draft and the meeting stay too.
    for save in [false, true] {
        let r = run(&mut s, "calendar.newMeeting", meeting(false));
        let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
        run(&mut s, "compose.saveDraft", json!({"compose": cid}));
        run(&mut s, "compose.close", json!({"compose": cid, "save": save}));
        assert!(s.mailbox.appointments.contains_key(&id), "save: {save}");
    }
    // Never saved and closed with Save: saved to Drafts, the meeting stays.
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    s.composes.get_mut(&cid).unwrap().dirty = false;
    let c = run(&mut s, "compose.close", json!({"compose": cid, "save": true}));
    assert!(c["draft"].is_u64(), "{c}");
    assert!(s.mailbox.appointments.contains_key(&id));
}

#[test]
fn discarding_the_invitation_after_an_update_went_out_keeps_the_meeting() {
    // Regression: the attendees had the meeting from Send Update, yet discarding the first invitation
    // still deleted it.
    let mut s = sending_session();
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    let u = run(&mut s, "calendar.sendUpdate", json!({"id": id.0, "send": "now"}));
    assert_eq!(u["sent"], true, "{u}");
    let d = run(&mut s, "compose.discard", json!({"compose": cid}));
    assert_eq!(d["meetingRemoved"], Value::Null, "{d}");
    assert!(s.mailbox.appointments.contains_key(&id));
    // An update opened but not sent leaves the first invitation in charge.
    let r = run(&mut s, "calendar.newMeeting", meeting(false));
    let (id, cid) = (ItemId(r["id"].as_u64().unwrap()), r["compose"].as_u64().unwrap());
    let u = run(&mut s, "calendar.sendUpdate", json!({"id": id.0}));
    run(&mut s, "compose.discard", json!({"compose": u["compose"]}));
    let d = run(&mut s, "compose.discard", json!({"compose": cid}));
    assert_eq!(d["meetingRemoved"], id.0);
}

#[test]
fn copying_a_published_calendar_does_not_publish_the_copy() {
    let mut s = session();
    let account = s.mailbox.default_account().unwrap().id;
    let c = cal(&s);
    let mut p = props::read(&s.mailbox, c);
    p.publication =
        Some(Publication { url: "https://dav.example.com/p/team.ics".into(), account, ..Default::default() });
    p.withdrawals = vec![Withdrawal { url: "https://dav.example.com/p/old.ics".into(), account }];
    props::write(&mut s.mailbox, c, &p);
    let id = create_appointment(&mut s, c, &json!({"subject": "Review", "start": "2026-10-12T10:00"}), false)
        .unwrap();
    if let Some(a) = s.mailbox.appointments.get_mut(&id) {
        a.remote = Some(RemoteRef { url: "https://dav.example.com/c/r.ics".into(), ..Default::default() });
    }
    let parent = role(&s, FolderRole::Inbox);
    let r = run(&mut s, "folder.copy", json!({"folder": c.0, "parent": parent.0}));
    let copy = FolderId(r["folder"].as_u64().unwrap());
    let cp = props::read(&s.mailbox, copy);
    assert!(cp.publication.is_none(), "the copy is not published to the original's address");
    assert!(cp.withdrawals.is_empty());
    let copied = s.mailbox.appointments.values().find(|a| a.folder == copy).unwrap();
    assert_eq!(copied.subject, "Review");
    assert!(copied.remote.is_none(), "the copy is not the server copy of the original");
    // The original keeps its publication.
    assert!(props::read(&s.mailbox, c).publication.is_some());
}

#[test]
fn update_folder_on_a_local_calendar_explains_itself() {
    let mut s = session();
    let c = cal(&s);
    let r = run(&mut s, "sendReceive.folder", json!({"folder": c.0}));
    assert_eq!(r["status"], "local folder");
    assert_eq!(r["server"], "none");
    let said = s.notices.iter().any(|n| match n {
        crate::Notice::Info(t) => t.contains("kept on this computer only") && t.contains("calendar"),
        _ => false,
    });
    assert!(said, "{:?}", s.notices);
}

fn share_body(s: &Session, r: &Value) -> String {
    let c = &s.composes[&r["compose"].as_u64().unwrap()];
    mailcraft_html::doc_to_text(&c.body)
}

#[test]
fn a_shared_calendar_shows_its_events_in_the_message() {
    let mut s = session();
    let c = cal(&s);
    for (subject, start, end) in [
        ("Standup", "2026-10-09T09:00", "2026-10-09T09:15"),
        ("Early gym", "2026-10-09T06:00", "2026-10-09T07:00"),
        ("Review", "2026-10-10T14:00", "2026-10-10T15:00"),
    ] {
        create_appointment(
            &mut s,
            c,
            &json!({"subject": subject, "start": start, "end": end, "body": format!("{subject} notes"),
                    "attachments": [{"name": "a.txt", "data_base64": "YQ=="}]}),
            false,
        )
        .unwrap();
    }
    // Daily schedule (the default): each day with its events and the free time between them.
    let r = run(&mut s, "calendar.share", json!({"range": "next7"}));
    assert_eq!(r["layout"], "dailySchedule");
    let body = share_body(&s, &r);
    for want in ["Calendar calendar", "Standup", "Early gym", "Review", "Free", "Friday, October 9, 2026"] {
        assert!(body.contains(want), "{want}: {body}");
    }
    // No files inside the attached calendar unless asked for.
    let ics =
        String::from_utf8(s.composes[&r["compose"].as_u64().unwrap()].attachments[0].data.clone()).unwrap();
    assert!(!ics.contains("ATTACH"), "{ics}");
    // List of events, working hours only, with the files.
    let r = run(
        &mut s,
        "calendar.share",
        json!({"range": "next7", "layout": "listOfEvents", "workingHoursOnly": true, "includeAttachments": true}),
    );
    let body = share_body(&s, &r);
    assert!(body.contains("Standup notes"), "{body}");
    assert!(!body.contains("Early gym"), "outside the working hours: {body}");
    assert!(body.contains("working hours"), "{body}");
    let ics =
        String::from_utf8(s.composes[&r["compose"].as_u64().unwrap()].attachments[0].data.clone()).unwrap();
    assert!(ics.contains("ATTACH;"), "{ics}");
    // Availability only: no subjects in the body either.
    let r = run(&mut s, "calendar.share", json!({"range": "next7", "detail": "availability"}));
    let body = share_body(&s, &r);
    assert!(!body.contains("Standup") && body.contains("Busy"), "{body}");
    assert!(s.run("calendar.share", &json!({"layout": "poster"})).is_err());
}

#[test]
fn save_attachment_picks_the_file_by_index_and_asks_for_a_path_in_its_own_window() {
    // Regression: of two files with one name the first was saved; without a path the mail Save Attachments
    // window opened (and saved the selected message's files instead).
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Trip", "start": "2026-11-02T09:00", "end": "2026-11-02T10:00",
               "attachments": [{"name": "notes.txt", "data_base64": "Zmlyc3Q="},
                               {"name": "notes.txt", "data_base64": "c2Vjb25k"}]}),
    );
    let id = r["id"].as_u64().unwrap();
    s.notices.clear();
    let r = run(&mut s, "calendar.saveAttachment", json!({"id": id, "index": 1, "name": "notes.txt"}));
    assert_eq!(r["dialog"], SAVE_ITEM_FILE);
    let params = s
        .notices
        .iter()
        .find_map(|n| match n {
            crate::Notice::OpenDialog(name, p) if name == SAVE_ITEM_FILE => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((params["id"].as_u64(), params["index"].as_u64()), (Some(id), Some(1)));
    assert_eq!((params["name"].as_str(), params["size"].as_u64()), (Some("notes.txt"), Some(6)));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!("mailcraft-save-item-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.txt");
    let at = path.to_string_lossy().to_string();
    let r = run(
        &mut s,
        "calendar.saveAttachment",
        json!({"id": id, "index": 1, "name": "notes.txt", "path": at}),
    );
    assert_eq!(r["index"], 1);
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
    // By name alone: the first so named.
    run(&mut s, "calendar.saveAttachment", json!({"id": id, "name": "notes.txt", "path": at}));
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    for bad in [
        json!({"id": id}),
        json!({"id": id, "index": 7}),
        json!({"id": id, "name": "missing.txt"}),
        // A stale index whose file has another name now, and no file of that name.
        json!({"id": id, "index": 0, "name": "other.txt"}),
    ] {
        assert!(s.run("calendar.saveAttachment", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
