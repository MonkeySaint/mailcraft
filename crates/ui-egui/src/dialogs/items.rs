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

        "closeAllItems" => close_item_windows(ctx, app),
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
        "chooseForm" => choose_form(ctx, app, d),
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

/// Item windows Close All Items closes (besides the read windows).
const ITEM_WINDOWS: [&str; 5] = ["note", "newPost", "contactCard", "chooseContact", "forwardContact"];

/// Close All Items (`app.closeAllItems` asks for it): closes the read windows and the item windows here. A
/// read window editing its message with unsaved edits stays open and asks whether to save them.
fn close_item_windows(ctx: &Context, app: &mut App) -> Out {
    let windows = std::mem::take(&mut app.ui.read_windows);
    for id in windows {
        if !crate::edit_message::may_close(ctx, app, id) {
            app.ui.read_windows.push(id);
        }
    }
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

/// Standard forms of Choose Form (engine kind, label).
const STANDARD_FORMS: [(&str, &str); 12] = [
    ("appointment", "Appointment"),
    ("meeting", "Meeting"),
    ("allDayEvent", "All Day Event"),
    ("recurringAppointment", "Recurring Appointment"),
    ("recurringMeeting", "Recurring Meeting"),
    ("recurringEvent", "Recurring Event"),
    ("post", "Post"),
    ("contact", "Contact"),
    ("task", "Task"),
    ("contactGroup", "Contact Group"),
    ("taskRequest", "Task Request"),
    ("email", "Message"),
];

#[derive(Clone, Default)]
struct FormsState {
    /// Look In: the Standard Forms Library, the Personal Forms Library (templates kept with the profile) or
    /// User Templates in File System (a folder).
    look: Look,
    /// The Personal Forms Library was listed.
    personal_listed: bool,
    /// The selected standard form (index) or template (path).
    standard: usize,
    template: Option<String>,
    dir: String,
    templates: Vec<(String, String)>,
    error: String,
    /// List the folder's templates on the next frame (on opening and after Browse picks a folder).
    pending: bool,
}

/// Lists the templates of `st.dir` (Show): the list, and a message when the folder cannot be read or has
/// none. A message from an earlier folder never stays under a new list.
fn list_templates(app: &mut App, st: &mut FormsState) {
    st.pending = false;
    st.error.clear();
    match app.run("mail.newItem", json!({"kind": "fromForm", "dir": st.dir.trim(), "list": true})) {
        Some(v) => {
            st.templates = templates_of(&v);
            if st.templates.is_empty() {
                st.error = "This folder has no .oft, .msg or .eml templates.".into();
            }
        }
        None => {
            st.templates.clear();
            st.error = super::folders::last_error(app);
        }
    }
    if st.template.as_ref().is_some_and(|t| !st.templates.iter().any(|(_, p)| p == t)) {
        st.template = None;
    }
}

/// Where Choose Form looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Look {
    #[default]
    Standard,
    Personal,
    User,
}

impl Look {
    fn label(self) -> &'static str {
        match self {
            Look::Standard => "Standard Forms Library",
            Look::Personal => "Personal Forms Library",
            Look::User => "User Templates in File System",
        }
    }
}

/// Lists the Personal Forms Library: the templates kept with the profile.
fn list_personal(app: &mut App, st: &mut FormsState) {
    st.personal_listed = true;
    st.error.clear();
    match app.run("mail.newItem", json!({"kind": "fromForm", "look": "personal", "list": true})) {
        Some(v) => {
            st.templates = templates_of(&v);
            if st.templates.is_empty() {
                st.error = "No personal forms yet: save a message as a template to keep it here.".into();
            }
        }
        None => {
            st.templates.clear();
            st.error = super::folders::last_error(app);
        }
    }
}

fn templates_of(v: &Value) -> Vec<(String, String)> {
    v.get("templates")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some((t.get("name")?.as_str()?.to_owned(), t.get("path")?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Choose Form: the standard forms, or the message templates (.oft, .msg, .eml) of a folder (User Templates
/// in File System; `mail.newItem {"kind": "fromForm"}` lists and opens them).
fn choose_form(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let start_dir = d.str("dir");
    let start_templates = d.get("templates").map(|_| templates_of(d.params)).unwrap_or_default();
    with_state(
        ctx,
        app,
        uid,
        |_: &App| FormsState {
            pending: start_templates.is_empty() && !start_dir.trim().is_empty(),
            dir: start_dir.clone(),
            templates: start_templates.clone(),
            ..Default::default()
        },
        |st, app| {
            window(ctx, app, d, "Choose Form", 460.0, |ui, app| {
                let before = st.look;
                ui.horizontal(|ui| {
                    ui.label("Look In:");
                    egui::ComboBox::from_id_salt(("forms-look", uid)).selected_text(st.look.label()).show_ui(
                        ui,
                        |ui| {
                            for look in [Look::Standard, Look::Personal, Look::User] {
                                ui.selectable_value(&mut st.look, look, look.label());
                            }
                        },
                    );
                });
                if st.look != before {
                    st.template = None;
                    st.error.clear();
                    st.templates.clear();
                    st.personal_listed = false;
                    st.pending = st.look == Look::User && !st.dir.trim().is_empty();
                }
                if st.look == Look::Personal && !st.personal_listed {
                    list_personal(app, st);
                }
                let mut open = false;
                if st.look != Look::Standard {
                    if st.look == Look::User {
                        ui.horizontal(|ui| {
                            let typed = ui.add(
                                egui::TextEdit::singleline(&mut st.dir)
                                    .desired_width(260.0)
                                    .hint_text("Folder"),
                            );
                            if typed.changed() {
                                // The message was about the folder named before.
                                st.error.clear();
                            }
                            if btn(ui, "Browse...")
                                && let Some(dir) = common::browse_folder(app, "Templates folder", &st.dir)
                            {
                                st.dir = dir;
                                st.templates.clear();
                                st.pending = true;
                            }
                            if btn(ui, "Show") || st.pending && !st.dir.trim().is_empty() {
                                list_templates(app, st);
                            }
                        });
                    }
                    egui::ScrollArea::vertical().id_salt(("forms-list", uid)).max_height(220.0).show(
                        ui,
                        |ui| {
                            for (name, path) in &st.templates {
                                let r =
                                    ui.selectable_label(st.template.as_deref() == Some(path.as_str()), name);
                                if r.clicked() {
                                    st.template = Some(path.clone());
                                }
                                if r.double_clicked() {
                                    st.template = Some(path.clone());
                                    open = true;
                                }
                            }
                        },
                    );
                } else {
                    egui::ScrollArea::vertical().id_salt(("forms-std", uid)).max_height(220.0).show(
                        ui,
                        |ui| {
                            for (i, (_, label)) in STANDARD_FORMS.iter().enumerate() {
                                let r = ui.selectable_label(st.standard == i, *label);
                                if r.clicked() {
                                    st.standard = i;
                                }
                                if r.double_clicked() {
                                    st.standard = i;
                                    open = true;
                                }
                            }
                        },
                    );
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let can = st.look == Look::Standard || st.template.is_some();
                let pressed = footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Some(false);
                    }
                    if common::primary(ui, app, can, "Open") {
                        return Some(true);
                    }
                    None
                });
                match (pressed, open) {
                    (Some(false), _) => Out::Close,
                    (Some(true), _) | (None, true) if can => {
                        let p = if st.look != Look::Standard {
                            json!({"kind": "fromForm", "path": st.template})
                        } else {
                            let kind = STANDARD_FORMS.get(st.standard).map_or("email", |(k, _)| *k);
                            json!({"kind": kind})
                        };
                        if app.run("mail.newItem", p).is_some() {
                            Out::Close
                        } else {
                            st.error = super::folders::last_error(app);
                            Out::Keep
                        }
                    }
                    _ => Out::Keep,
                }
            })
        },
    )
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
        let m = s.mailbox.mail.items().find(|m| m.subject == "Team lunch").expect("post saved");
        assert_eq!(m.folder, inbox);
        assert!(m.to.is_empty() && !m.is_draft);
        assert_eq!(m.body.text.as_deref(), Some("Friday at noon."));
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
        // Edit Message edits the open read window's message in place.
        assert_eq!(names(&h), ["newPost"]);
        assert_eq!(h.state().ui.read_windows, [id]);
        assert_eq!(h.state().ui.editing_messages, [id]);
        h.state_mut().run("app.closeAllItems", json!({}));
        h.run_steps(3);
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        // Nothing was changed in the edited message, so its window closes too.
        assert!(h.state().ui.read_windows.is_empty());
        assert!(h.state().ui.editing_messages.is_empty());
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

#[cfg(test)]
mod choose_form_tests {
    use super::{FormsState, list_personal, list_templates};
    use crate::App;
    use mailcraft_engine::sample::sample_session;

    fn scratch(name: &str) -> std::path::PathBuf {
        let base = std::env::current_exe()
            .ok()
            .and_then(|e| e.ancestors().nth(3).map(|t| t.join("tmp")))
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
        let dir = base.join(format!("mc-forms-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Regression: "no templates" stayed under a later, populated list.
    #[test]
    fn a_listed_folder_clears_the_earlier_message() {
        let empty = scratch("empty");
        let full = scratch("full");
        let eml = "From: a@example.com\r\nTo: b@example.org\r\nSubject: Weekly\r\n\r\nBody\r\n";
        std::fs::write(full.join("Weekly.eml"), eml).unwrap();
        let mut app = App::new(sample_session());
        let mut st = FormsState { dir: empty.display().to_string(), pending: true, ..Default::default() };
        list_templates(&mut app, &mut st);
        assert!(st.templates.is_empty() && !st.error.is_empty() && !st.pending);
        st.dir = full.display().to_string();
        list_templates(&mut app, &mut st);
        assert_eq!(st.templates.len(), 1);
        assert!(st.error.is_empty(), "{}", st.error);
        st.template = st.templates.first().map(|(_, p)| p.clone());
        st.dir = empty.join("missing").display().to_string();
        list_templates(&mut app, &mut st);
        assert!(st.templates.is_empty() && st.template.is_none() && !st.error.is_empty());
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&full);
    }

    /// Look In › Personal Forms Library lists the profile's templates; an in-memory profile has none to keep.
    #[test]
    fn the_personal_forms_library_lists_the_profile_templates() {
        let mut app = App::new(sample_session());
        let mut st = FormsState::default();
        list_personal(&mut app, &mut st);
        assert!(st.templates.is_empty() && st.error.contains("saved profile"), "{}", st.error);
        let dir = scratch("profile");
        let mut app = App::new(mailcraft_engine::Session::open(&dir).unwrap());
        let forms = dir.join("templates");
        std::fs::create_dir_all(&forms).unwrap();
        std::fs::write(forms.join("Status.eml"), "Subject: Status\r\n\r\nBody\r\n").unwrap();
        let mut st = FormsState::default();
        list_personal(&mut app, &mut st);
        assert_eq!(st.templates.len(), 1, "{}", st.error);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
