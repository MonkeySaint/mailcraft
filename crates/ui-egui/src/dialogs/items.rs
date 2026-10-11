//! Item windows shown as dialogs until the modules area gives them their own windows: appointment, event and
//! meeting (with the New Items presets: All Day Event, Recurring Appointment / Meeting / Event, Meeting with
//! All), contact and contact group, task, note, post (Post in This Folder), Edit Message, plus Go to Date,
//! Time Zone, Add Calendar, contact card / chooser, Forward Contact and Choose Form. (Mail Merge and the Task
//! Request window are the modules area's own dialogs.) `closeAllItems` (Close All Items) closes the read
//! windows and these item windows.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, window, with_state};
use super::compose_dlg::{Form, Kind, Vals, cmd, combo, f, run_form};
use crate::App;
use mailcraft_model::{ItemId, NoteColor, TaskPriority, TaskStatus};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "appointment" | "newAppointment" | "newMeeting" => appointment(ctx, app, d),
        "contact" | "newContact" => contact(ctx, app, d),
        "contactGroup" | "newContactGroup" => contact_group(ctx, app, d),
        "task" | "newTask" => task(ctx, app, d),
        "note" => note(ctx, app, d),
        "newPost" => post(ctx, app, d),
        "editMessage" => edit_message(ctx, app, d),
        "closeAllItems" => close_item_windows(app),
        "goToDate" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Go To Date",
                vec![
                    f("date", "Date (YYYY-MM-DD):", Kind::Text),
                    f("mode", "Show in:", combo(&["day", "workWeek", "week", "month"])),
                ],
            )
            .init("date", json!(if d.str("date").is_empty() { common::today(app) } else { d.str("date") }))
            .init("mode", json!(if d.str("mode").is_empty() { "day".to_owned() } else { d.str("mode") })),
            |app, v| {
                cmd(app, "calendar.goToDate", json!({"date": v.s("date"), "mode": v.s("mode")})).map(|_| None)
            },
        ),
        "timeZone" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Time Zone",
                vec![
                    f("zone", "Time zone (IANA name, e.g. America/Toronto):", Kind::Text),
                    f("offset", "Or a fixed offset from UTC (minutes):", Kind::Num { min: -840.0, max: 840.0 }),
                    f(
                        "n",
                        "",
                        Kind::Note(
                            "A named zone follows daylight saving time. A fixed offset is used when no zone is \
                             given, for example -300 for UTC-05:00."
                                .into(),
                        ),
                    ),
                ],
            )
            .init("zone", json!(mailcraft_engine::cmd::calendar::views::time_zones(&app.s).zone))
            .init(
                "offset",
                json!(d.get("offsetMin").and_then(Value::as_i64).unwrap_or(i64::from(app.s.tz_offset_min))),
            ),
            |app, v| {
                let zone = v.s("zone");
                let params = if zone.trim().is_empty() {
                    json!({"offsetMin": v.n("offset") as i64})
                } else {
                    json!({"zone": zone.trim()})
                };
                cmd(app, "calendar.changeTimeZone", params).map(|_| None)
            },
        ),
        "addCalendar" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Add Calendar",
                vec![
                    f("source", "From:", combo(&["internet", "file", "blank"])),
                    f("name", "Name:", Kind::Text),
                    f("url", "Internet calendar URL:", Kind::Text),
                    f("path", "iCalendar file (.ics):", Kind::Text),
                    f(
                        "n",
                        "",
                        Kind::Note(
                            "An Internet calendar is read-only and is downloaded in the background.".into(),
                        ),
                    ),
                ],
            )
            .init("source", json!(add_calendar_source(&d.str("source"))))
            .width(480.0),
            |app, v| {
                // Never fetch on the UI thread: the calendar's workers download the feed.
                let params = json!({"source": v.s("source"), "name": v.s("name"), "url": v.s("url"), "path": v.s("path"), "fetch": false});
                cmd(app, "calendar.addCalendar", params).map(|_| None)
            },
        ),
        "contactCard" => contact_card(ctx, app, d),
        "chooseContact" => choose_contact(ctx, app, d),
        "forwardContact" => forward_contact(ctx, app, d),
        "chooseForm" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Choose Form",
                vec![f(
                    "kind",
                    "Standard forms:",
                    combo(&[
                        "appointment",
                        "meeting",
                        "allDayEvent",
                        "recurringAppointment",
                        "recurringMeeting",
                        "recurringEvent",
                        "post",
                        "contact",
                        "task",
                        "contactGroup",
                        "taskRequest",
                        "email",
                    ]),
                )],
            )
            .ok("Open"),
            |app, v| cmd(app, "mail.newItem", json!({"kind": v.s("kind")})).map(|_| None),
        ),
        _ => return None,
    })
}

/// The Add Calendar source offered first: the requested one when MailCraft supports it, else Internet.
fn add_calendar_source(requested: &str) -> &'static str {
    match requested {
        "file" => "file",
        "blank" => "blank",
        _ => "internet",
    }
}

/// Unix seconds → "YYYY-MM-DDTHH:MM".
fn stamp(app: &App, unix: i64) -> (String, String) {
    common::date_parts(app, unix)
}

fn split_stamp(s: &str) -> (String, String) {
    match s.split_once('T').or_else(|| s.split_once(' ')) {
        Some((d, t)) => (d.to_owned(), t.chars().take(5).collect()),
        None => (s.to_owned(), String::new()),
    }
}

const SHOW_AS: [&str; 5] = ["busy", "free", "tentative", "workingElsewhere", "outOfOffice"];

/// Recurrence patterns the New Items forms offer ("none" = a single occurrence).
const RECURRENCE: [&str; 6] = ["none", "daily", "weekdays", "weekly", "monthly", "yearly"];

/// RFC 5545 RRULE for a recurrence pattern ending after `count` occurrences (0 = no end date); `None` for a
/// single occurrence or an unknown pattern. Weekly, monthly and yearly repeat on the start's day.
fn rrule(pattern: &str, count: u64) -> Option<String> {
    let base = match pattern {
        "daily" => "FREQ=DAILY",
        "weekdays" => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
        "weekly" => "FREQ=WEEKLY",
        "monthly" => "FREQ=MONTHLY",
        "yearly" => "FREQ=YEARLY",
        _ => return None,
    };
    Some(if count > 0 { format!("{base};COUNT={}", count.min(MAX_OCCURRENCES)) } else { base.to_owned() })
}

/// Most occurrences the "End after" box takes.
const MAX_OCCURRENCES: u64 = 999;

/// People typed into a To / Required / Optional box: entries separated by ';', or by ',' after an address
/// (so "Rivera, Alice <alice@example.com>" stays whole); quoted names are kept intact.
fn split_people(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut angle) = (false, false);
    for ch in text.chars() {
        match ch {
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            _ => {}
        }
        let split = !quoted && !angle && (ch == ';' || (ch == ',' && cur.contains('@')));
        if split {
            push_person(&mut out, &cur);
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    push_person(&mut out, &cur);
    out
}

/// Adds one entry of a people list, unless it is only spaces and stray commas.
fn push_person(out: &mut Vec<String>, entry: &str) {
    let t = entry.trim().trim_matches(',').trim();
    if !t.is_empty() {
        out.push(t.to_owned());
    }
}

/// "HH:MM" plus `minutes`, kept within the day; `None` when `t` is not a time.
fn later(t: &str, minutes: u32) -> Option<String> {
    let (h, m) = t.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().get(..2).unwrap_or(m.trim()).parse().ok()?);
    if h > 23 || m > 59 {
        return None;
    }
    let total = (h * 60 + m).saturating_add(minutes).min(23 * 60 + 59);
    Some(format!("{:02}:{:02}", total / 60, total % 60))
}

/// A people list from the dialog params: `key` as text, else the `list` array joined with "; ".
fn people_param(d: &Dlg<'_>, key: &str, list: &str) -> String {
    let text = d.str(key);
    if !text.is_empty() {
        return text;
    }
    d.arr(list).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")
}

fn appointment(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let meeting = d.name == "newMeeting";
    let id = d.u64("id").filter(|i| app.s.mailbox.appointments.contains_key(&ItemId(*i)));
    let existing = id.and_then(|i| app.s.mailbox.appointments.get(&ItemId(i))).cloned();
    // New Items presets: All Day Event / Recurring Event start as all-day items, the Recurring ones weekly.
    let all_day = existing.as_ref().map_or(d.bool("allDay"), |a| a.all_day);
    let recurrence = match d.str("recurrence").as_str() {
        r if RECURRENCE.contains(&r) => r.to_owned(),
        _ => "none".to_owned(),
    };
    let (sd, st) = match &existing {
        Some(a) => stamp(app, a.start.unix),
        None if !d.str("start").is_empty() => split_stamp(&d.str("start")),
        None => (common::today(app), "09:00".into()),
    };
    let (ed, et) = match &existing {
        // An all-day item ends at midnight after its last day; the form shows that last day.
        Some(a) if a.all_day => stamp(app, a.end.unix.saturating_sub(1).max(a.start.unix)),
        Some(a) => stamp(app, a.end.unix),
        None if !d.str("end").is_empty() => split_stamp(&d.str("end")),
        // A start without an end: half an hour from the start.
        None => (sd.clone(), later(&st, 30).unwrap_or_else(|| "09:30".into())),
    };
    let mut fields = Vec::new();
    if meeting {
        fields.push(f("to", "Required:", Kind::Text));
        fields.push(f("optional", "Optional:", Kind::Text));
    }
    fields.extend([
        f("subject", "Title:", Kind::Text),
        f("location", "Location:", Kind::Text),
        f("startDate", "Start date:", Kind::Text),
        f("startTime", "Start time:", Kind::Text),
        f("endDate", "End date:", Kind::Text),
        f("endTime", "End time:", Kind::Text),
        f("allDay", "All day event", Kind::Check),
    ]);
    if existing.is_none() {
        fields.push(f("recurrence", "Recurrence:", combo(&RECURRENCE)));
        fields.push(f(
            "occurrences",
            "End after (occurrences, 0 = no end date):",
            Kind::Num { min: 0.0, max: MAX_OCCURRENCES as f64 },
        ));
    }
    fields.extend([
        f("showAs", "Show As:", combo(&SHOW_AS)),
        f("reminder", "Reminder (minutes, 0 = none):", Kind::Num { min: 0.0, max: 20_160.0 }),
        f("private", "Private", Kind::Check),
        f("body", "Notes:", Kind::Multi),
    ]);
    let title = match (meeting, existing.is_some(), all_day, recurrence != "none") {
        (true, _, _, true) => "Recurring Meeting",
        (true, _, _, false) => "Meeting",
        (false, true, _, _) => "Appointment",
        (false, false, true, true) => "Recurring Event",
        (false, false, true, false) => "Event",
        (false, false, false, true) => "Recurring Appointment",
        (false, false, false, false) => "New Appointment",
    };
    let mut form = Form::new(title, fields)
        .init("startDate", json!(sd))
        .init("startTime", json!(if all_day && existing.is_none() { String::new() } else { st }))
        .init("endDate", json!(ed))
        .init("endTime", json!(if all_day && existing.is_none() { String::new() } else { et }))
        .init("allDay", json!(all_day))
        .init("recurrence", json!(recurrence))
        .init("occurrences", json!(0))
        // All-day events show as free, like a birthday or a holiday; appointments as busy.
        .init("showAs", json!(if all_day { "free" } else { "busy" }))
        .init("reminder", json!(d.u64("reminderMin").unwrap_or(if all_day { 0 } else { 15 })))
        .init("to", json!(people_param(d, "to", "attendees")))
        .init("optional", json!(people_param(d, "optional", "optional")))
        .init("subject", json!(d.str("subject")))
        .init("location", json!(d.str("location")))
        .init("body", json!(d.str("body")))
        .ok(if meeting { "Send" } else { "Save & Close" })
        .width(520.0);
    if let Some(a) = &existing {
        form = form
            .init("subject", json!(a.subject))
            .init("location", json!(a.location))
            .init("reminder", json!(a.reminder_min.unwrap_or(0)))
            .init("private", json!(a.private))
            .init("body", json!(a.body));
    }
    let folder = d.u64("folder");
    run_form(ctx, app, d, form, move |app, v| {
        let all_day = v.b("allDay");
        let time = |key: &str, default: &str| {
            let t = v.s(key);
            if all_day || t.is_empty() { default.to_owned() } else { t }
        };
        let start_date = v.s("startDate");
        let end_date = if v.s("endDate").is_empty() { start_date.clone() } else { v.s("endDate") };
        let mut p = json!({
            "subject": v.s("subject"),
            "location": v.s("location"),
            "start": format!("{start_date}T{}", time("startTime", "00:00")),
            // All day: the end date is the last day of the event (inclusive).
            "end": format!("{end_date}T{}", time("endTime", if all_day { "23:59" } else { "00:00" })),
            "allDay": all_day,
            "showAs": if v.s("showAs").is_empty() { "busy".to_owned() } else { v.s("showAs") },
            "reminderMin": if v.n("reminder") > 0.0 { json!(v.n("reminder") as u64) } else { Value::Null },
            "private": v.b("private"),
            "body": v.s("body"),
        });
        if let Some(o) = p.as_object_mut() {
            if let Some(fid) = folder {
                o.insert("folder".into(), json!(fid));
            }
            if meeting {
                o.insert("attendees".into(), json!(split_people(&v.s("to"))));
                o.insert("optional".into(), json!(split_people(&v.s("optional"))));
            }
            if id.is_none()
                && let Some(rule) = rrule(&v.s("recurrence"), v.n("occurrences").max(0.0) as u64)
            {
                o.insert("recurrence".into(), json!(rule));
            }
            if let Some(i) = id {
                o.remove("folder");
                o.insert("id".into(), json!(i));
            }
        }
        let id_cmd = if id.is_some() {
            "calendar.update"
        } else if meeting {
            "calendar.newMeeting"
        } else {
            "calendar.newAppointment"
        };
        cmd(app, id_cmd, p).map(|_| None)
    })
}

/// Post in This Folder: a subject and text saved as a post in the mail folder (`mail.newItem` kind post).
fn post(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let folder = d.u64("folder");
    let path = d.str("path");
    let mut fields = Vec::new();
    if !path.is_empty() {
        fields.push(f("n", "", Kind::Note(format!("Posted to: {path}"))));
    }
    fields.extend([f("subject", "Subject:", Kind::Text), f("body", "Message:", Kind::Multi)]);
    let form = Form::new("New Post", fields).init("subject", json!(d.str("subject"))).ok("Post").width(520.0);
    run_form(ctx, app, d, form, move |app, v| {
        let text = v.0.get("body").and_then(Value::as_str).unwrap_or_default().to_owned();
        if v.s("subject").is_empty() && text.trim().is_empty() {
            return Err("Enter a subject or a message.".into());
        }
        let mut p = json!({"kind": "post", "subject": v.s("subject"), "text": text});
        if let (Some(fid), Some(o)) = (folder, p.as_object_mut()) {
            o.insert("folder".into(), json!(fid));
        }
        cmd(app, "mail.newItem", p).map(|_| None)
    })
}

/// Edit Message: the subject and text of a received message, saved back to the stored item.
fn edit_message(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let Some(id) = d.u64("id").filter(|i| app.s.mailbox.mail.contains_key(&ItemId(*i))) else {
        return Out::Close;
    };
    let (subject, text) = (d.str("subject"), d.str("text"));
    let mut fields = vec![f("subject", "Subject:", Kind::Text), f("body", "Message:", Kind::Multi)];
    if d.bool("html") {
        fields.push(f(
            "n",
            "",
            Kind::Note("Saving changed text replaces the message's formatting with plain paragraphs.".into()),
        ));
    }
    let form = Form::new("Edit Message", fields)
        .init("subject", json!(subject))
        .init("body", json!(text))
        .ok("Save")
        .width(560.0);
    run_form(ctx, app, d, form, move |app, v| {
        let body = v.0.get("body").and_then(Value::as_str).unwrap_or_default().to_owned();
        let mut p = json!({"ids": [id]});
        if let Some(o) = p.as_object_mut() {
            if v.s("subject") != subject.trim() {
                o.insert("subject".into(), json!(v.s("subject")));
            }
            // Untouched text keeps the message's own body (and its formatting).
            if body.trim() != text.trim() {
                o.insert("text".into(), json!(body));
            }
            if o.len() == 1 {
                return Ok(None);
            }
        }
        cmd(app, "mail.editMessage", p).map(|_| None)
    })
}

/// Item windows Close All Items closes (besides the read windows).
const ITEM_WINDOWS: [&str; 15] = [
    "appointment",
    "newAppointment",
    "newMeeting",
    "contact",
    "newContact",
    "contactGroup",
    "newContactGroup",
    "task",
    "newTask",
    "note",
    "newPost",
    "editMessage",
    "contactCard",
    "chooseContact",
    "forwardContact",
];

/// Close All Items (`app.closeAllItems` asks for it): closes the read windows and the item windows here.
fn close_item_windows(app: &mut App) -> Out {
    app.ui.read_windows.clear();
    app.ui.dialogs.retain(|(name, _)| !ITEM_WINDOWS.contains(&name.as_str()));
    Out::Close
}

fn contact(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let id = d.u64("id").filter(|i| app.s.mailbox.contacts.contains_key(&ItemId(*i)));
    let existing = id.and_then(|i| app.s.mailbox.contacts.get(&ItemId(i))).cloned();
    let mut form = Form::new(
        if existing.is_some() { "Contact" } else { "New Contact" },
        vec![
            f("fullName", "Full Name:", Kind::Text),
            f("company", "Company:", Kind::Text),
            f("jobTitle", "Job title:", Kind::Text),
            f("fileAs", "File as:", Kind::Text),
            f("email", "Email:", Kind::Text),
            f("email2", "Email 2:", Kind::Text),
            f("business", "Business phone:", Kind::Text),
            f("mobile", "Mobile phone:", Kind::Text),
            f("webPage", "Web page address:", Kind::Text),
            f("birthday", "Birthday (YYYY-MM-DD):", Kind::Text),
            f("notes", "Notes:", Kind::Multi),
        ],
    )
    .ok("Save & Close")
    .width(500.0);
    if let Some(c) = &existing {
        let phone = |k: &str| {
            c.phones
                .iter()
                .find(|(kind, _)| kind.eq_ignore_ascii_case(k))
                .map(|(_, n)| n.clone())
                .unwrap_or_default()
        };
        form = form
            .init("fullName", json!(c.full_name()))
            .init("company", json!(c.company))
            .init("jobTitle", json!(c.job_title))
            .init("fileAs", json!(c.file_as))
            .init("email", json!(c.emails.first().cloned().unwrap_or_default()))
            .init("email2", json!(c.emails.get(1).cloned().unwrap_or_default()))
            .init("business", json!(phone("Business")))
            .init("mobile", json!(phone("Mobile")))
            .init("webPage", json!(c.web_page))
            .init("birthday", json!(c.birthday.clone().unwrap_or_default()))
            .init("notes", json!(c.notes));
    }
    let folder = d.u64("folder");
    run_form(ctx, app, d, form, move |app, v: &Vals<'_>| {
        if v.s("fullName").is_empty() && v.s("email").is_empty() && v.s("company").is_empty() {
            return Err("Enter a name, an email address or a company.".into());
        }
        let mut phones = serde_json::Map::new();
        if !v.s("business").is_empty() {
            phones.insert("Business".into(), json!(v.s("business")));
        }
        if !v.s("mobile").is_empty() {
            phones.insert("Mobile".into(), json!(v.s("mobile")));
        }
        let emails: Vec<String> =
            [v.s("email"), v.s("email2")].into_iter().filter(|e| !e.is_empty()).collect();
        let mut p = json!({
            "fullName": v.s("fullName"), "company": v.s("company"), "jobTitle": v.s("jobTitle"),
            "emails": emails, "webPage": v.s("webPage"), "notes": v.s("notes"),
        });
        if let Some(o) = p.as_object_mut() {
            if !v.s("fileAs").is_empty() {
                o.insert("fileAs".into(), json!(v.s("fileAs")));
            }
            if !phones.is_empty() {
                o.insert("phones".into(), Value::Object(phones));
            }
            if !v.s("birthday").is_empty() {
                o.insert("birthday".into(), json!(v.s("birthday")));
            }
            match id {
                Some(i) => {
                    o.insert("id".into(), json!(i));
                }
                None => {
                    if let Some(fid) = folder {
                        o.insert("folder".into(), json!(fid));
                    }
                }
            }
        }
        let out = cmd(app, if id.is_some() { "people.update" } else { "people.new" }, p)?;
        // Options › People › Check for duplicates: nothing was saved (the contact window offers Add / Update).
        if let Some(d) = out.get("duplicate").filter(|_| out.get("id").is_none()) {
            let name = d.get("name").and_then(Value::as_str).unwrap_or("");
            return Err(format!(
                "A contact with this name or email address already exists in this folder ({name}). Open it to \
                 update it, or change the name."
            ));
        }
        Ok(None)
    })
}

fn contact_group(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let folder = d.u64("folder");
    let form = Form::new(
        "Contact Group",
        vec![
            f("name", "Name:", Kind::Text),
            f("members", "Members (separate with ;):", Kind::Multi),
            f("notes", "Notes:", Kind::Multi),
        ],
    )
    .ok("Save & Close")
    .width(480.0);
    run_form(ctx, app, d, form, move |app, v| {
        if v.s("name").is_empty() {
            return Err("Enter a name for the group.".into());
        }
        let mut p = json!({"name": v.s("name"), "members": common::split_list(&v.s("members")), "notes": v.s("notes")});
        if let (Some(fid), Some(o)) = (folder, p.as_object_mut()) {
            o.insert("folder".into(), json!(fid));
        }
        cmd(app, "people.newGroup", p).map(|_| None)
    })
}

const STATUS: [&str; 5] = ["notStarted", "inProgress", "completed", "waitingOnSomeoneElse", "deferred"];

fn task(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let request = d.name.contains("askRequest");
    let id = d.u64("id").filter(|i| app.s.mailbox.tasks.contains_key(&ItemId(*i)));
    let existing = id.and_then(|i| app.s.mailbox.tasks.get(&ItemId(i))).cloned();
    let mut fields = Vec::new();
    if request {
        fields.push(f("to", "To:", Kind::Text));
    }
    fields.extend([
        f("subject", "Subject:", Kind::Text),
        f("start", "Start date (YYYY-MM-DD):", Kind::Text),
        f("due", "Due date (YYYY-MM-DD):", Kind::Text),
        f("status", "Status:", combo(&STATUS)),
        f("priority", "Priority:", combo(&["normal", "low", "high"])),
        f("percent", "% Complete:", Kind::Num { min: 0.0, max: 100.0 }),
        f("reminder", "Reminder (YYYY-MM-DD HH:MM):", Kind::Text),
        f("body", "Notes:", Kind::Multi),
    ]);
    if request {
        fields.push(f(
            "n",
            "",
            Kind::Note("The task is saved to your Tasks; sending task requests needs a mail server.".into()),
        ));
    }
    let mut form = Form::new(if request { "Task Request" } else { "Task" }, fields)
        .ok("Save & Close")
        .width(500.0)
        .init("to", json!(d.str("to")));
    if let Some(t) = &existing {
        let date = |o: Option<mailcraft_model::DateTime>| {
            o.map(|x| common::date_parts(app, x.unix).0).unwrap_or_default()
        };
        form = form
            .init("subject", json!(t.subject))
            .init("start", json!(date(t.start)))
            .init("due", json!(date(t.due)))
            .init(
                "status",
                json!(match t.status {
                    TaskStatus::NotStarted => "notStarted",
                    TaskStatus::InProgress => "inProgress",
                    TaskStatus::Completed => "completed",
                    TaskStatus::WaitingOnSomeoneElse => "waitingOnSomeoneElse",
                    TaskStatus::Deferred => "deferred",
                }),
            )
            .init(
                "priority",
                json!(match t.priority {
                    TaskPriority::Low => "low",
                    TaskPriority::Normal => "normal",
                    TaskPriority::High => "high",
                }),
            )
            .init("percent", json!(t.percent_complete))
            .init("body", json!(t.body));
    }
    let folder = d.u64("folder");
    run_form(ctx, app, d, form, move |app, v| {
        if v.s("subject").is_empty() {
            return Err("Enter a subject.".into());
        }
        let mut p = json!({
            "subject": v.s("subject"),
            "status": if v.s("status").is_empty() { "notStarted".to_owned() } else { v.s("status") },
            "priority": if v.s("priority").is_empty() { "normal".to_owned() } else { v.s("priority") },
            "percent": v.n("percent") as u64,
            "body": v.s("body"),
        });
        if let Some(o) = p.as_object_mut() {
            for k in ["start", "due"] {
                if !v.s(k).is_empty() {
                    o.insert(k.into(), json!(v.s(k)));
                }
            }
            if !v.s("reminder").is_empty() {
                o.insert("reminder".into(), json!(v.s("reminder").replacen(' ', "T", 1)));
            }
            match id {
                Some(i) => {
                    o.insert("id".into(), json!(i));
                }
                None => {
                    if let Some(fid) = folder {
                        o.insert("folder".into(), json!(fid));
                    }
                }
            }
        }
        cmd(app, if id.is_some() { "tasks.update" } else { "tasks.new" }, p).map(|_| None)
    })
}

#[derive(Clone, Default)]
struct NoteState {
    text: String,
    color: String,
    loaded: bool,
}

const NOTE_COLORS: [&str; 5] = ["yellow", "blue", "green", "pink", "white"];

fn note(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let id = d.u64("id");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| NoteState::default(),
        |st, app| {
            if !st.loaded {
                if let Some(n) = id.and_then(|i| app.s.mailbox.notes.get(&ItemId(i))) {
                    st.text.clone_from(&n.text);
                    st.color = match n.color {
                        NoteColor::Blue => "blue",
                        NoteColor::Green => "green",
                        NoteColor::Pink => "pink",
                        NoteColor::Yellow => "yellow",
                        NoteColor::White => "white",
                    }
                    .into();
                } else {
                    st.color = "yellow".into();
                }
                st.loaded = true;
            }
            let title = if d.name == "newPost" { "Post" } else { "Note" };
            window(ctx, app, d, title, 360.0, |ui, app| {
                ui.add(egui::TextEdit::multiline(&mut st.text).desired_rows(8).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    common::combo(ui, ("nc", uid), &mut st.color, &NOTE_COLORS, 100.0);
                });
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    if super::common::primary(ui, app, true, "Save & Close") {
                        match id {
                            Some(i) => {
                                app.run("notes.update", json!({"id": i, "text": st.text, "color": st.color}))
                            }
                            None => app.run("notes.new", json!({"text": st.text, "color": st.color})),
                        };
                        return Out::Close;
                    }
                    Out::Keep
                })
            })
        },
    )
}

fn contact_card(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let id = d.u64("id");
    let c = id.and_then(|i| app.s.mailbox.contacts.get(&ItemId(i))).cloned();
    window(ctx, app, d, "Contact Card", 380.0, |ui, app| {
        let Some(c) = c else {
            ui.label("The contact no longer exists.");
            return footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep });
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            app.icons.paint(ui, r, "person", app.theme.accent);
            ui.vertical(|ui| {
                ui.label(RichText::new(c.full_name()).strong().size(16.0));
                if !c.job_title.is_empty() || !c.company.is_empty() {
                    muted(ui, app, &format!("{} {}", c.job_title, c.company));
                }
            });
        });
        for e in &c.emails {
            ui.label(format!("Email: {e}"));
        }
        for (k, n) in &c.phones {
            ui.label(format!("{k}: {n}"));
        }
        footer(ui, |ui| {
            if btn(ui, "Close") {
                return Out::Close;
            }
            if btn(ui, "Edit") {
                return Out::close_open("contact", json!({"id": c.id.0}));
            }
            if let Some(e) = c.emails.first()
                && btn(ui, "Email")
            {
                if let Some(v) = app.run("mail.newEmail", json!({}))
                    && let Some(cid) = v.get("compose").and_then(Value::as_u64)
                {
                    app.run(
                        "compose.setTo",
                        json!({"compose": cid, "value": format!("{} <{e}>", c.full_name())}),
                    );
                }
                return Out::Close;
            }
            Out::Keep
        })
    })
}

fn choose_contact(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let ids = d.ids();
    window(ctx, app, d, "Choose a Contact", 380.0, |ui, app| {
        ui.label(format!("More than one contact matches \"{}\":", d.str("text")));
        let mut out = Out::Keep;
        for i in &ids {
            if let Some(c) = app.s.mailbox.contacts.get(&ItemId(*i))
                && ui
                    .button(format!("{}  {}", c.full_name(), c.emails.first().cloned().unwrap_or_default()))
                    .clicked()
            {
                out = Out::close_open("contactCard", json!({"id": i}));
            }
        }
        out.or(footer(ui, |ui| if btn(ui, "Cancel") { Out::Close } else { Out::Keep }))
    })
}

fn forward_contact(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let ids = d.ids();
    window(ctx, app, d, "Forward Contact", 400.0, |ui, app| {
        let as_card = d.str("as") == "businessCard";
        ui.label(format!(
            "Forward {} contact(s) {}.",
            ids.len(),
            if as_card { "as business cards" } else { "as contact details" }
        ));
        footer(ui, |ui| {
            if btn(ui, "Cancel") {
                return Out::Close;
            }
            if super::common::primary(ui, app, !ids.is_empty(), "Create Message") {
                let text: Vec<String> = ids
                    .iter()
                    .filter_map(|i| app.s.mailbox.contacts.get(&ItemId(*i)))
                    .map(|c| {
                        let mut lines = vec![c.full_name()];
                        if !c.company.is_empty() {
                            lines.push(c.company.clone());
                        }
                        lines.extend(c.emails.iter().cloned());
                        lines.extend(c.phones.iter().map(|(k, n)| format!("{k}: {n}")));
                        lines.join("\n")
                    })
                    .collect();
                if let Some(v) = app.run("mail.newEmail", json!({}))
                    && let Some(cid) = v.get("compose").and_then(Value::as_u64)
                {
                    app.run("compose.insertText", json!({"compose": cid, "text": text.join("\n\n")}));
                }
                return Out::Close;
            }
            Out::Keep
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::App;
    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;
    use mailcraft_model::ItemId;
    use serde_json::json;

    fn harness(app: App) -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::dialogs::dialogs(&ctx, app);
            },
            app,
        );
        h.run_steps(3);
        h
    }

    fn names(h: &Harness<'_, App>) -> Vec<String> {
        h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Types `text` into the `n`th text box of the open form.
    fn type_into(h: &mut Harness<'_, App>, n: usize, text: &str) {
        h.query_all_by_role(Role::TextInput).nth(n).unwrap().focus();
        h.run_steps(3);
        h.query_all_by_role(Role::TextInput).nth(n).unwrap().type_text(text);
        h.run_steps(3);
    }

    #[test]
    fn all_day_event_preset_saves_a_whole_day() {
        let mut app = App::new(sample_session());
        let before: Vec<ItemId> = app.s.mailbox.appointments.keys().copied().collect();
        app.run("mail.newItem", json!({"kind": "allDayEvent", "start": "2026-10-14T00:00"})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["newAppointment"]);
        type_into(&mut h, 0, "Holiday");
        h.get_by_label("Save & Close").click();
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        let s = &h.state().s;
        let a = s.mailbox.appointments.values().find(|a| !before.contains(&a.id)).expect("saved");
        assert_eq!(a.subject, "Holiday");
        assert!(a.all_day);
        assert_eq!(a.end.unix - a.start.unix, 86_400, "one whole day");
        assert!(a.recurrence.is_none());
    }

    #[test]
    fn recurring_meeting_preset_saves_a_weekly_series_with_attendees() {
        let mut app = App::new(sample_session());
        let before: Vec<ItemId> = app.s.mailbox.appointments.keys().copied().collect();
        app.run("mail.newItem", json!({"kind": "recurringMeeting", "start": "2026-10-14T10:00"})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["newMeeting"]);
        type_into(&mut h, 0, "Rivera, Alice <alice@example.com>; bob@example.org");
        type_into(&mut h, 2, "Standup");
        h.get_by_label("Send").click();
        h.run_steps(3);
        let s = &h.state().s;
        let a = s.mailbox.appointments.values().find(|a| !before.contains(&a.id)).expect("saved");
        assert_eq!(a.subject, "Standup");
        assert!(a.recurrence.as_ref().is_some_and(|r| r.rrule.starts_with("FREQ=WEEKLY")));
        let who: Vec<&str> = a.attendees.iter().map(|x| x.mailbox.addr.as_str()).collect();
        assert!(who.contains(&"alice@example.com") && who.contains(&"bob@example.org"), "{who:?}");
    }

    #[test]
    fn post_in_this_folder_saves_a_post_not_a_note() {
        let mut app = App::new(sample_session());
        let inbox = app.s.selection.folder;
        let notes = app.s.mailbox.notes.len();
        let mail = app.s.mailbox.mail.len();
        app.run("mail.newItem", json!({"kind": "post"})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["newPost"]);
        type_into(&mut h, 0, "Team lunch");
        h.get_by_role(Role::MultilineTextInput).focus();
        h.run_steps(3);
        h.get_by_role(Role::MultilineTextInput).type_text("Friday at noon.");
        h.run_steps(3);
        h.get_by_label("Post").click();
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        let s = &h.state().s;
        assert_eq!(s.mailbox.notes.len(), notes);
        assert_eq!(s.mailbox.mail.len(), mail + 1);
        let m = s.mailbox.mail.values().find(|m| m.subject == "Team lunch").expect("post saved");
        assert_eq!(m.folder, inbox);
        assert!(m.to.is_empty() && !m.is_draft);
        assert_eq!(m.body.text.as_deref(), Some("Friday at noon."));
    }

    #[test]
    fn edit_message_saves_the_new_subject() {
        let mut app = App::new(sample_session());
        let folder = app.s.selection.folder;
        let id = app.s.mailbox.items_in(folder).first().map(|m| m.id).unwrap();
        let body = app.s.mailbox.mail[&id].body.clone();
        app.run("nav.selectItem", json!({"id": id.0})).unwrap();
        app.run("mail.editMessage", json!({})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["editMessage"]);
        type_into(&mut h, 0, " (edited)");
        h.get_by_label("Save").click();
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        let m = &h.state().s.mailbox.mail[&id];
        assert!(m.subject.contains("(edited)"), "{}", m.subject);
        // The text was not touched: the body (and its formatting) stays as it was.
        assert_eq!(m.body, body);
    }

    #[test]
    fn close_all_items_closes_read_and_item_windows() {
        let mut app = App::new(sample_session());
        let folder = app.s.selection.folder;
        let id = app.s.mailbox.items_in(folder).first().map(|m| m.id.0).unwrap();
        crate::dialogs::open(&mut app, "readWindow", json!({"id": id}));
        app.run("mail.newItem", json!({"kind": "post"})).unwrap();
        app.run("mail.newItem", json!({"kind": "contact"})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["newPost", "newContact"]);
        assert_eq!(h.state().ui.read_windows, [id]);
        h.state_mut().run("app.closeAllItems", json!({}));
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        assert!(h.state().ui.read_windows.is_empty());
    }
    #[test]
    fn recurrence_patterns_become_rrules() {
        assert_eq!(super::rrule("none", 5), None);
        assert_eq!(super::rrule("bogus", 0), None);
        assert_eq!(super::rrule("weekly", 0).as_deref(), Some("FREQ=WEEKLY"));
        assert_eq!(
            super::rrule("weekdays", 10).as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;COUNT=10")
        );
        assert_eq!(super::rrule("yearly", 1_000_000).as_deref(), Some("FREQ=YEARLY;COUNT=999"));
        // The calendar takes every rule the form makes.
        let mut s = mailcraft_engine::sample::sample_session();
        for p in super::RECURRENCE.iter().skip(1) {
            let r = super::rrule(p, 3).unwrap();
            let params = serde_json::json!({"subject": p, "start": "2026-10-12T09:00", "recurrence": r});
            assert!(s.run("calendar.newAppointment", &params).is_ok(), "{r}");
        }
    }

    #[test]
    fn people_lists_split_between_entries_only() {
        assert_eq!(
            super::split_people("Rivera, Alice <alice@example.com>; bob@example.org, carol@example.net"),
            vec!["Rivera, Alice <alice@example.com>", "bob@example.org", "carol@example.net"]
        );
        assert_eq!(
            super::split_people("\"Lee; Dana\" <dana@example.org>, Eve <eve@example.com>;;"),
            vec!["\"Lee; Dana\" <dana@example.org>", "Eve <eve@example.com>"]
        );
        assert!(super::split_people(" ; , ").is_empty());
        assert_eq!(super::later("10:00", 30).as_deref(), Some("10:30"));
        assert_eq!(super::later("23:45", 30).as_deref(), Some("23:59"));
        assert_eq!(super::later("noon", 30), None);
        assert_eq!(super::later("25:00", 30), None);
        assert_eq!(
            super::split_people("\"unterminated <x@example.com"),
            vec!["\"unterminated <x@example.com"]
        );
    }

    #[test]
    fn add_calendar_offers_only_supported_sources() {
        assert_eq!(super::add_calendar_source("file"), "file");
        assert_eq!(super::add_calendar_source("blank"), "blank");
        for other in ["", "internet", "addressBook", "roomList", "shared"] {
            assert_eq!(super::add_calendar_source(other), "internet");
        }
    }
}
