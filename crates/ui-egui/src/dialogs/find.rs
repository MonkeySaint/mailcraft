//! Advanced Find, the Address Book and Check Names.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, btn, btn_en, child, folder_path, footer, muted, ok_cancel, primary, take_replies, window,
    with_state,
};
use crate::App;
use mailcraft_engine::cmd::search::{item_matches, token};
use mailcraft_model::{FolderId, ItemId};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "advancedFind" => advanced_find(ctx, app, d),
        "addressBook" => address_book(ctx, app, d),
        "checkNames" => check_names(ctx, app, d),
        _ => return None,
    })
}

const IN_FIELDS: [&str; 3] =
    ["subject field only", "subject field and message body", "frequently-used text fields"];
const TIMES: [(&str, &str); 9] = [
    ("anytime", ""),
    ("yesterday", "yesterday"),
    ("today", "today"),
    ("this week", "this week"),
    ("last week", "last week"),
    ("this month", "this month"),
    ("last month", "last month"),
    ("in the last 7 days", "last7"),
    ("older than…", "older"),
];

/// Look for: (label, `search.advancedFind` lookFor).
const LOOK_FOR: [(&str, &str); 6] = [
    ("Messages", "messages"),
    ("Any type of item", "any"),
    ("Appointments and meetings", "appointments"),
    ("Contacts", "contacts"),
    ("Tasks", "tasks"),
    ("Notes", "notes"),
];

#[derive(Clone, Default)]
struct FindState {
    tab: usize,
    /// Index into [`LOOK_FOR`].
    look_for: usize,
    folder: Option<u64>,
    subfolders: bool,
    words: String,
    in_field: usize,
    from: String,
    sent_to: String,
    time: usize,
    categories: String,
    read: usize,
    attachments: bool,
    importance: usize,
    older_days: u32,
    larger_kb: u32,
    results: Vec<u64>,
    searched: bool,
    selected: Option<u64>,
}

fn advanced_find(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| {
        let older = d.u64("olderDays").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let larger = d.u64("largerKb").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        FindState {
            folder: d.u64("folder").or(Some(a.s.selection.folder.0)),
            subfolders: older > 0 || larger > 0,
            older_days: older,
            larger_kb: larger,
            tab: if older > 0 || larger > 0 { 1 } else { 0 },
            time: if older > 0 { 8 } else { 0 },
            ..Default::default()
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "folder" {
                st.folder = v.as_u64().or(st.folder);
            }
        }
        if (st.older_days > 0 || st.larger_kb > 0) && !st.searched {
            run_find(app, st);
        }
        window(ctx, app, d, "Advanced Find", 680.0, |ui, app| {
            let mut out = Out::Keep;
            ui.horizontal(|ui| {
                ui.label("Look for:");
                let names: Vec<&str> = LOOK_FOR.iter().map(|l| l.0).collect();
                common::combo_idx(ui, ("aflook", uid), &mut st.look_for, &names, 190.0);
                ui.label("In:");
                ui.label(RichText::new(folder_path(app, st.folder)).strong());
                if btn(ui, "Browse...") {
                    out = Out::open(
                        "pickFolder",
                        child(uid, "folder", json!({"pick": "any", "selected": st.folder})),
                    );
                }
                ui.checkbox(&mut st.subfolders, "Include subfolders");
            });
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(500.0);
                    ui.horizontal(|ui| {
                        for (i, t) in ["Messages", "More Choices", "Advanced"].iter().enumerate() {
                            if ui.selectable_label(st.tab == i, *t).clicked() {
                                st.tab = i;
                            }
                        }
                    });
                    ui.separator();
                    egui::Grid::new(("af", uid, st.tab)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| match st.tab {
                        0 => {
                            ui.label("Search for the word(s):");
                            ui.add(egui::TextEdit::singleline(&mut st.words).desired_width(300.0));
                            ui.end_row();
                            ui.label("In:");
                            common::combo_idx(ui, ("afin", uid), &mut st.in_field, &IN_FIELDS, 300.0);
                            ui.end_row();
                            ui.label("From...");
                            ui.add(egui::TextEdit::singleline(&mut st.from).desired_width(300.0));
                            ui.end_row();
                            ui.label("Sent To...");
                            ui.add(egui::TextEdit::singleline(&mut st.sent_to).desired_width(300.0));
                            ui.end_row();
                            ui.label("Time: received");
                            let names: Vec<&str> = TIMES.iter().map(|t| t.0).collect();
                            ui.horizontal(|ui| {
                                common::combo_idx(ui, ("aftime", uid), &mut st.time, &names, 160.0);
                                if st.time == 8 {
                                    ui.add(egui::DragValue::new(&mut st.older_days).range(1..=3650).suffix(" days"));
                                }
                            });
                            ui.end_row();
                        }
                        1 => {
                            ui.label("Categories:");
                            ui.add(egui::TextEdit::singleline(&mut st.categories).hint_text("any category name").desired_width(260.0));
                            ui.end_row();
                            ui.label("Only items that are:");
                            common::combo_idx(ui, ("afread", uid), &mut st.read, &["read or unread", "unread", "read"], 160.0);
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut st.attachments, "Only items with one or more attachments");
                            ui.end_row();
                            ui.label("Whose importance is:");
                            common::combo_idx(ui, ("afimp", uid), &mut st.importance, &["any", "high", "normal", "low"], 160.0);
                            ui.end_row();
                            ui.label("Size (kilobytes): larger than");
                            ui.add(egui::DragValue::new(&mut st.larger_kb).range(0..=1_000_000));
                            ui.end_row();
                        }
                        _ => {
                            ui.label("Search text:");
                            ui.label(RichText::new(build_query(st)).monospace());
                            ui.end_row();
                            ui.label("");
                            muted(ui, app, "Combine terms such as from:, to:, subject:, body:, category:, isread:, hasattachments:, importance:, received:.");
                            ui.end_row();
                        }
                    });
                });
                ui.vertical(|ui| {
                    if primary(ui, app, true, "Find Now") {
                        run_find(app, st);
                    }
                    if btn(ui, "New Search") {
                        let folder = st.folder;
                        *st = FindState { folder, ..Default::default() };
                    }
                    if btn_en(ui, st.searched, "Show in List") {
                        if let Some(f) = st.folder {
                            app.run("nav.selectFolder", json!({"folder": f}));
                        }
                        app.run("search.query", json!({"text": build_query(st)}));
                    }
                });
            });
            if st.searched {
                ui.separator();
                ui.label(format!("{} item(s) found", st.results.len()));
                results_table(ui, app, uid, st);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
        })
    })
}

fn build_query(st: &FindState) -> String {
    let mut terms: Vec<String> = Vec::new();
    for w in st.words.split_whitespace() {
        terms.push(if st.in_field == 0 { token("subject", w) } else { w.to_owned() });
    }
    if !st.from.trim().is_empty() {
        terms.push(token("from", st.from.trim()));
    }
    if !st.sent_to.trim().is_empty() {
        terms.push(token("to", st.sent_to.trim()));
    }
    if let Some((_, v)) = TIMES.get(st.time)
        && !v.is_empty()
        && *v != "older"
        && *v != "last7"
    {
        terms.push(token("received", v));
    }
    if !st.categories.trim().is_empty() {
        terms.push(token("category", st.categories.trim()));
    }
    match st.read {
        1 => terms.push("isread:no".into()),
        2 => terms.push("isread:yes".into()),
        _ => {}
    }
    if st.attachments {
        terms.push("hasattachments:yes".into());
    }
    if let Some(i) = ["", "high", "normal", "low"].get(st.importance).filter(|i| !i.is_empty()) {
        terms.push(format!("importance:{i}"));
    }
    terms.join(" ")
}

fn run_find(app: &mut App, st: &mut FindState) {
    let query = build_query(st);
    // Other item kinds go through the engine's Advanced Find, which searches every kind.
    if let Some((_, kind)) = LOOK_FOR.get(st.look_for).filter(|l| l.1 != "messages") {
        let mut p = json!({"query": query, "lookFor": kind, "subfolders": st.subfolders, "limit": 5_000});
        if let (Some(f), Some(o)) = (st.folder, p.as_object_mut()) {
            o.insert("folder".into(), json!(f));
        }
        let found = app.run("search.advancedFind", p);
        st.results = found
            .as_ref()
            .and_then(|r| r.get("items"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|i| i.get("id").and_then(Value::as_u64)).collect())
            .unwrap_or_default();
        st.searched = true;
        return;
    }
    let folders: Vec<FolderId> = match st.folder {
        Some(f) if st.subfolders => app.s.mailbox.subtree(FolderId(f)),
        Some(f) => vec![FolderId(f)],
        None => app.s.mailbox.folders.keys().copied().collect(),
    };
    let now = app.s.now().unix;
    let days = |n: u32| i64::from(n) * 86_400;
    let mut found: Vec<(i64, u64)> = app
        .s
        .mailbox
        .mail
        .values()
        .filter(|m| folders.contains(&m.folder))
        .filter(|m| item_matches(&app.s, m, &query))
        .filter(|m| match TIMES.get(st.time).map(|t| t.1) {
            Some("older") => st.older_days == 0 || m.received.unix < now - days(st.older_days),
            Some("last7") => m.received.unix >= now - days(7),
            _ => true,
        })
        .filter(|m| st.larger_kb == 0 || m.size > u64::from(st.larger_kb) * 1024)
        .map(|m| (m.received.unix, m.id.0))
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    st.results = found.into_iter().map(|x| x.1).take(5_000).collect();
    st.searched = true;
}

fn results_table(ui: &mut egui::Ui, app: &mut App, uid: u64, st: &mut FindState) {
    egui::ScrollArea::vertical().id_salt(("afr", uid)).max_height(200.0).show(ui, |ui| {
        egui::Grid::new(("afrg", uid)).num_columns(4).striped(true).spacing([12.0, 2.0]).show(ui, |ui| {
            for h in ["From", "Subject", "Received", "In Folder"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for id in st.results.clone() {
                let Some(hit) = found_row(app, ItemId(id)) else { continue };
                let r = ui.selectable_label(st.selected == Some(id), hit.from);
                ui.label(hit.subject);
                ui.label(hit.when);
                ui.label(hit.folder);
                ui.end_row();
                if r.clicked() {
                    st.selected = Some(id);
                }
                if r.double_clicked() {
                    match hit.window {
                        None => {
                            app.run("mail.open", json!({"ids": [id]}));
                        }
                        Some(window) => super::open(app, window, json!({"id": id})),
                    }
                }
            }
        });
    });
}

/// One Advanced Find result: the From / Subject / Received / In Folder cells and the item window a
/// double-click opens (`None` = the message's read window).
struct FoundRow {
    from: String,
    subject: String,
    when: String,
    folder: String,
    window: Option<&'static str>,
}

fn found_row(app: &App, id: ItemId) -> Option<FoundRow> {
    let mb = &app.s.mailbox;
    let tz = app.s.tz_offset_min;
    let folder = |f: FolderId| mb.folder(f).map(|f| f.name.clone()).unwrap_or_default();
    let untitled =
        |t: &str| if t.trim().is_empty() { "(no subject)".to_owned() } else { t.trim().to_owned() };
    if let Some(m) = mb.mail.get(&id) {
        return Some(FoundRow {
            from: m.from.as_ref().map(|f| f.display().to_owned()).unwrap_or_default(),
            subject: untitled(&m.subject),
            when: mailcraft_model::ListTime::short_date_time(m.received, tz),
            folder: folder(m.folder),
            window: None,
        });
    }
    if let Some(a) = mb.appointments.get(&id) {
        let kind = if a.is_meeting { "Meeting" } else { "Appointment" };
        return Some(FoundRow {
            from: kind.to_owned(),
            subject: untitled(&a.subject),
            when: mailcraft_model::ListTime::short_date_time(a.start, tz),
            folder: folder(a.folder),
            window: Some("appointment"),
        });
    }
    if let Some(c) = mb.contacts.get(&id) {
        let name = c.full_name();
        return Some(FoundRow {
            from: "Contact".to_owned(),
            subject: if name.trim().is_empty() { c.file_as.clone() } else { name },
            when: String::new(),
            folder: folder(c.folder),
            window: Some("contact"),
        });
    }
    if let Some(t) = mb.tasks.get(&id) {
        return Some(FoundRow {
            from: "Task".to_owned(),
            subject: untitled(&t.subject),
            when: t.due.map(|d| mailcraft_model::ListTime::short_date_time(d, tz)).unwrap_or_default(),
            folder: folder(t.folder),
            window: Some("task"),
        });
    }
    let n = mb.notes.get(&id)?;
    Some(FoundRow {
        from: "Note".to_owned(),
        subject: untitled(n.text.lines().next().unwrap_or_default()),
        when: String::new(),
        folder: folder(n.folder),
        window: Some("note"),
    })
}

// ---------------------------------------------------------------------------------------------------------
// Address Book

#[derive(Clone, Default)]
struct BookState {
    search: String,
    selected: Option<String>,
    to: String,
    cc: String,
    bcc: String,
    loaded: bool,
}

/// "Name <address>" entries of every contact address, sorted by name.
fn entries(app: &App) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = app
        .s
        .mailbox
        .contacts
        .values()
        .flat_map(|c| {
            let name = c.full_name();
            let name = if name.trim().is_empty() { c.file_as.clone() } else { name };
            let company = c.company.clone();
            c.emails.iter().map(move |e| (name.clone(), e.clone(), company.clone())).collect::<Vec<_>>()
        })
        .collect();
    v.sort_by_key(|e| e.0.to_lowercase());
    v
}

fn entry_text(name: &str, addr: &str) -> String {
    if name.trim().is_empty() { addr.to_owned() } else { format!("{name} <{addr}>") }
}

fn append(field: &mut String, item: &str) {
    if field.split(';').any(|p| p.trim().eq_ignore_ascii_case(item)) {
        return;
    }
    if field.trim().is_empty() {
        *field = item.to_owned();
    } else {
        *field = format!("{}; {item}", field.trim_end().trim_end_matches(';'));
    }
}

fn address_book(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose").filter(|c| app.s.composes.contains_key(c));
    with_state(
        ctx,
        app,
        uid,
        |_: &App| BookState::default(),
        |st, app| {
            if !st.loaded {
                if let Some(c) = compose.and_then(|c| app.s.composes.get(&c)) {
                    st.to.clone_from(&c.to);
                    st.cc.clone_from(&c.cc);
                    st.bcc.clone_from(&c.bcc);
                }
                st.loaded = true;
            }
            window(ctx, app, d, "Address Book: Contacts", 660.0, |ui, app| {
                let mut out = Out::Keep;
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.add(egui::TextEdit::singleline(&mut st.search).desired_width(260.0));
                    ui.label("Address Book: Contacts");
                    if btn(ui, "Advanced Find") {
                        out = Out::open("advancedFind", json!({}));
                    }
                });
                let list = entries(app);
                let q = st.search.to_lowercase();
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(("ab", uid))
                            .max_height(220.0)
                            .min_scrolled_height(220.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Grid::new(("abg", uid))
                                    .num_columns(3)
                                    .striped(true)
                                    .spacing([16.0, 2.0])
                                    .show(ui, |ui| {
                                        for h in ["Name", "Email Address", "Company"] {
                                            ui.label(RichText::new(h).strong());
                                        }
                                        ui.end_row();
                                        if list.is_empty() {
                                            muted(ui, app, "No contacts with email addresses.");
                                            ui.end_row();
                                        }
                                        for (name, addr, company) in &list {
                                            if !q.is_empty()
                                                && !name.to_lowercase().contains(&q)
                                                && !addr.to_lowercase().contains(&q)
                                            {
                                                continue;
                                            }
                                            let text = entry_text(name, addr);
                                            let r = ui.selectable_label(
                                                st.selected.as_deref() == Some(text.as_str()),
                                                if name.is_empty() { addr } else { name },
                                            );
                                            ui.label(addr);
                                            ui.label(company);
                                            ui.end_row();
                                            if r.clicked() {
                                                st.selected = Some(text.clone());
                                            }
                                            if r.double_clicked() && compose.is_some() {
                                                append(&mut st.to, &text);
                                            }
                                        }
                                    });
                            });
                    });
                if compose.is_some() {
                    ui.label("Message Recipients");
                    for (label, field) in
                        [("To ->", &mut st.to), ("Cc ->", &mut st.cc), ("Bcc ->", &mut st.bcc)]
                    {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    st.selected.is_some(),
                                    egui::Button::new(label).min_size(egui::Vec2::new(60.0, 22.0)),
                                )
                                .clicked()
                                && let Some(s) = &st.selected
                            {
                                append(field, s);
                            }
                            ui.add(egui::TextEdit::singleline(field).desired_width(f32::INFINITY));
                        });
                    }
                }
                if !matches!(out, Out::Keep) {
                    return out;
                }
                match compose {
                    Some(c) => match ok_cancel(ui, app, true) {
                        Some(true) => {
                            if let Some(cs) = app.s.composes.get(&c).cloned() {
                                if cs.to != st.to {
                                    app.run("compose.setTo", json!({"compose": c, "value": st.to}));
                                }
                                if cs.cc != st.cc {
                                    app.run("compose.setCc", json!({"compose": c, "value": st.cc}));
                                }
                                if cs.bcc != st.bcc {
                                    if !cs.show_bcc {
                                        app.run("compose.showBcc", json!({"compose": c, "on": true}));
                                    }
                                    app.run("compose.setBcc", json!({"compose": c, "value": st.bcc}));
                                }
                            }
                            Out::Close
                        }
                        Some(false) => Out::Close,
                        None => Out::Keep,
                    },
                    None => footer(ui, |ui| {
                        if btn(ui, "Close") {
                            return Out::Close;
                        }
                        if btn_en(ui, st.selected.is_some(), "New Message") {
                            if let (Some(v), Some(to)) =
                                (app.run("mail.newEmail", json!({})), st.selected.clone())
                                && let Some(c) = v.get("compose").and_then(Value::as_u64)
                            {
                                app.run("compose.setTo", json!({"compose": c, "value": to}));
                            }
                            return Out::Close;
                        }
                        if btn(ui, "New Contact") {
                            app.run("people.new", json!({}));
                        }
                        Out::Keep
                    }),
                }
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Check Names

#[derive(Clone, Default)]
struct CheckState {
    picks: Vec<usize>,
}

fn check_names(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let unresolved = d.arr("unresolved");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| CheckState { picks: vec![0; unresolved.len()] },
        |st, app| {
            window(ctx, app, d, "Check Names", 460.0, |ui, app| {
                muted(ui, app, "MailCraft found more than one match, or none, for these names.");
                egui::ScrollArea::vertical().id_salt(("cn", uid)).max_height(300.0).show(ui, |ui| {
                    for (i, u) in unresolved.iter().enumerate() {
                        let name = u.get("name").and_then(Value::as_str).unwrap_or_default();
                        let cands: Vec<String> = u
                            .get("candidates")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                            .unwrap_or_default();
                        common::heading(ui, app, &format!("Select the address to use for \"{name}\":"));
                        let Some(pick) = st.picks.get_mut(i) else { continue };
                        if cands.is_empty() {
                            muted(ui, app, "No matches. Keep it as typed, or add the person as a contact.");
                        }
                        for (j, c) in cands.iter().enumerate() {
                            ui.radio_value(pick, j, c);
                        }
                        ui.radio_value(pick, cands.len(), "Keep as typed");
                    }
                });
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    if primary(ui, app, true, "OK") {
                        if let Some(c) = compose {
                            apply_names(app, c, &unresolved, &st.picks);
                        }
                        out = Out::Close;
                    }
                    if btn(ui, "New Contact") {
                        app.run("people.new", json!({}));
                    }
                });
                out
            })
        },
    )
}

fn apply_names(app: &mut App, compose: u64, unresolved: &[Value], picks: &[usize]) {
    let Some(cs) = app.s.composes.get(&compose).cloned() else { return };
    let mut fields = [cs.to.clone(), cs.cc.clone(), cs.bcc.clone()];
    for (u, pick) in unresolved.iter().zip(picks) {
        let name = u.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
        let Some(choice) =
            u.get("candidates").and_then(Value::as_array).and_then(|a| a.get(*pick)).and_then(Value::as_str)
        else {
            continue;
        };
        for f in &mut fields {
            // Split like the recipient fields do (a quoted "Last; First" name is one entry).
            let parts: Vec<String> = crate::compose::split_recipients(f)
                .into_iter()
                .map(|p| if p == name { choice.to_owned() } else { p.to_owned() })
                .collect();
            *f = parts.join("; ");
        }
    }
    let [to, cc, bcc] = fields;
    for (cmd, old, new) in
        [("compose.setTo", &cs.to, to), ("compose.setCc", &cs.cc, cc), ("compose.setBcc", &cs.bcc, bcc)]
    {
        if *old != new {
            app.run(cmd, json!({"compose": compose, "value": new}));
        }
    }
}
