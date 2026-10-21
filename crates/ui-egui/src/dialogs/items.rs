//! Item windows shown as dialogs: note, post (Post in This Folder), Edit Message, plus Go to Date, Time Zone,
//! Add Calendar, contact card / chooser, Forward Contact and Choose Form. Appointments, meetings, contacts,
//! contact groups and tasks (New Items included) open in the modules area's own item windows, as do Mail
//! Merge and the Task Request window. `closeAllItems` (Close All Items) closes the read windows and these
//! item windows.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, window, with_state};
use super::compose_dlg::{Form, Kind, cmd, combo, f, run_form};
use crate::App;
use mailcraft_model::{ItemId, NoteColor};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
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
const ITEM_WINDOWS: [&str; 6] =
    ["note", "newPost", "editMessage", "contactCard", "chooseContact", "forwardContact"];

/// Close All Items (`app.closeAllItems` asks for it): closes the read windows and the item windows here.
fn close_item_windows(app: &mut App) -> Out {
    app.ui.read_windows.clear();
    app.ui.dialogs.retain(|(name, _)| !ITEM_WINDOWS.contains(&name.as_str()));
    Out::Close
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
        app.run("nav.selectItem", json!({"id": id})).unwrap();
        app.run("mail.editMessage", json!({})).unwrap();
        let mut h = harness(app);
        assert_eq!(names(&h), ["newPost", "editMessage"]);
        assert_eq!(h.state().ui.read_windows, [id]);
        h.state_mut().run("app.closeAllItems", json!({}));
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        assert!(h.state().ui.read_windows.is_empty());
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
