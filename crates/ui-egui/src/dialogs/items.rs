//! Item windows shown as dialogs until the modules area gives them their own windows: appointment and
//! meeting, contact and contact group, task, note, plus Go to Date, Time Zone, Add Calendar, contact card /
//! chooser, Forward Contact and Choose Form. (Mail Merge and the Task Request window are the modules area's
//! own dialogs.)

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
        "note" | "newPost" => note(ctx, app, d),
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

fn appointment(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let meeting = d.name == "newMeeting";
    let id = d.u64("id").filter(|i| app.s.mailbox.appointments.contains_key(&ItemId(*i)));
    let existing = id.and_then(|i| app.s.mailbox.appointments.get(&ItemId(i))).cloned();
    let (sd, st) = match &existing {
        Some(a) => stamp(app, a.start.unix),
        None if !d.str("start").is_empty() => split_stamp(&d.str("start")),
        None => (common::today(app), "09:00".into()),
    };
    let (ed, et) = match &existing {
        Some(a) => stamp(app, a.end.unix),
        None if !d.str("end").is_empty() => split_stamp(&d.str("end")),
        None => (sd.clone(), "09:30".into()),
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
        f("showAs", "Show As:", combo(&SHOW_AS)),
        f("reminder", "Reminder (minutes, 0 = none):", Kind::Num { min: 0.0, max: 20_160.0 }),
        f("private", "Private", Kind::Check),
        f("body", "Notes:", Kind::Multi),
    ]);
    let title = if meeting {
        "Meeting"
    } else if existing.is_some() {
        "Appointment"
    } else {
        "New Appointment"
    };
    let mut form = Form::new(title, fields)
        .init("startDate", json!(sd))
        .init("startTime", json!(st))
        .init("endDate", json!(ed))
        .init("endTime", json!(et))
        .init("reminder", json!(d.u64("reminderMin").unwrap_or(15)))
        .init("to", json!(d.str("to")))
        .init("subject", json!(d.str("subject")))
        .ok(if meeting { "Send" } else { "Save & Close" })
        .width(520.0);
    if let Some(a) = &existing {
        form = form
            .init("subject", json!(a.subject))
            .init("location", json!(a.location))
            .init("allDay", json!(a.all_day))
            .init("reminder", json!(a.reminder_min.unwrap_or(0)))
            .init("private", json!(a.private))
            .init("body", json!(a.body));
    }
    let folder = d.u64("folder");
    run_form(ctx, app, d, form, move |app, v| {
        let mut p = json!({
            "subject": v.s("subject"),
            "location": v.s("location"),
            "start": format!("{}T{}", v.s("startDate"), if v.s("startTime").is_empty() { "00:00".into() } else { v.s("startTime") }),
            "end": format!("{}T{}", v.s("endDate"), if v.s("endTime").is_empty() { "00:00".into() } else { v.s("endTime") }),
            "allDay": v.b("allDay"),
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
                o.insert("attendees".into(), json!(common::split_list(&v.s("to"))));
                o.insert("optional".into(), json!(common::split_list(&v.s("optional"))));
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
    #[test]
    fn add_calendar_offers_only_supported_sources() {
        assert_eq!(super::add_calendar_source("file"), "file");
        assert_eq!(super::add_calendar_source("blank"), "blank");
        for other in ["", "internet", "addressBook", "roomList", "shared"] {
            assert_eq!(super::add_calendar_source(other), "internet");
        }
    }
}
