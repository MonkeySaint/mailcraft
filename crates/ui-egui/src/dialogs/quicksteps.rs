//! Quick Steps dialogs (plan/reference/dialogs.md "Manage Quick Steps" 558×400, "Edit Quick Step" 510×570, and the
//! first-use setup of steps such as "Move to: ?"):
//! - `manageQuickSteps`: the list with ▲ ▼, New ▾ (templates), the Description pane (actions, shortcut key,
//!   tooltip), Edit / Duplicate / Delete, Reset to Defaults, OK / Cancel.
//! - `editQuickStep`: name, the action list (add, remove, reorder; per-action options: folder, recipients,
//!   category, flag, importance), shortcut key Ctrl+Shift+1 … 9, tooltip text; Finish / Cancel. Opened with
//!   `{"step": {...}}` to edit, or `{"template", "name", "actions"}` for a new step.
//! - `quickStepFirstUse`: "First Time Setup": name, the missing folder / recipients, Options... (the full
//!   editor), Save.
//!
//! Every change goes through `quicksteps.*` commands (each undoable).

use egui::{Context, RichText, Ui};
use serde_json::{Value, json};

use crate::App;
use crate::dialogs::common::{
    self, Dlg, Out, btn, btn_en, child, folder_path, footer, muted, primary, take_replies, window, with_state,
};
use crate::dialogs::folders::last_error;
use mailcraft_model::{FolderId, Importance, QuickStep, QuickStepAction};

pub(super) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "manageQuickSteps" => manage(ctx, app, d),
        "editQuickStep" => edit(ctx, app, d),
        "quickStepFirstUse" => first_use(ctx, app, d),
        _ => return None,
    })
}

/// New Quick Step templates (key, menu label).
pub(super) const TEMPLATES: [(&str, &str); 7] = [
    ("moveToFolder", "Move to Folder..."),
    ("categorizeMove", "Categorize & Move..."),
    ("flagMove", "Flag & Move..."),
    ("newEmailTo", "New E-mail To..."),
    ("forwardTo", "Forward To..."),
    ("newMeeting", "New Meeting..."),
    ("custom", "Custom..."),
];

/// Action kinds offered in the editor, in [`QuickStepAction`] order.
const KINDS: [&str; 25] = [
    "Move to folder",
    "Copy to folder",
    "Delete message",
    "Permanently delete message",
    "Mark as read",
    "Mark as unread",
    "Set importance",
    "Categorize message",
    "Clear categories",
    "Flag message",
    "Clear flags on message",
    "Mark complete",
    "New message",
    "Forward",
    "Reply",
    "Reply All",
    "New meeting",
    "Forward as attachment",
    "Reply with meeting",
    "Create a task with the message text",
    "Create an appointment with the message text",
    "Ignore conversation",
    "Always move conversation",
    "Move to Focused",
    "Move to Other",
];

/// Flag choices: the Follow Up keys the engine turns into dates, with their labels.
const FLAGS: [(&str, &str); 5] = [
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("thisWeek", "This Week"),
    ("nextWeek", "Next Week"),
    ("noDate", "No Date"),
];

const IMPORTANCE: [&str; 3] = ["Low", "Normal", "High"];

const KEYS: [&str; 10] = [
    "None",
    "Ctrl+Shift+1",
    "Ctrl+Shift+2",
    "Ctrl+Shift+3",
    "Ctrl+Shift+4",
    "Ctrl+Shift+5",
    "Ctrl+Shift+6",
    "Ctrl+Shift+7",
    "Ctrl+Shift+8",
    "Ctrl+Shift+9",
];

/// Text of an action in the Description pane and the setup dialog.
pub(super) fn action_text(app: &App, a: &QuickStepAction) -> String {
    let folder = |f: &Option<FolderId>| {
        f.and_then(|f| app.s.mailbox.folder(f)).map_or("(choose on first use)".to_owned(), |f| f.name.clone())
    };
    let people =
        |to: &[String]| if to.is_empty() { "(choose on first use)".to_owned() } else { to.join("; ") };
    match a {
        QuickStepAction::MoveTo(f) => format!("Move to folder: {}", folder(f)),
        QuickStepAction::CopyTo(f) => format!("Copy to folder: {}", folder(f)),
        QuickStepAction::Delete => "Delete message".into(),
        QuickStepAction::PermanentlyDelete => "Permanently delete message".into(),
        QuickStepAction::MarkRead => "Mark as read".into(),
        QuickStepAction::MarkUnread => "Mark as unread".into(),
        QuickStepAction::SetImportance(i) => format!("Set importance: {}", imp_label(*i)),
        QuickStepAction::Categorize(c) => {
            format!("Categorize message: {}", if c.trim().is_empty() { "(none chosen)" } else { c.trim() })
        }
        QuickStepAction::ClearCategories => "Clear categories".into(),
        QuickStepAction::Flag(k) => {
            let label = FLAGS.iter().find(|(key, _)| key == k).map_or(k.as_str(), |(_, l)| l);
            format!("Flag message: {label}")
        }
        QuickStepAction::ClearFlag => "Clear flags on message".into(),
        QuickStepAction::MarkComplete => "Mark complete".into(),
        QuickStepAction::NewMessageTo(to) => format!("New message to: {}", people(to)),
        QuickStepAction::Forward(to) => format!("Forward to: {}", people(to)),
        QuickStepAction::Reply => "Reply".into(),
        QuickStepAction::ReplyAll => "Reply All".into(),
        QuickStepAction::NewMeeting => "New meeting".into(),
        QuickStepAction::ForwardAsAttachment(to) => format!("Forward as attachment to: {}", people(to)),
        QuickStepAction::ReplyWithMeeting => "Reply with meeting".into(),
        QuickStepAction::CreateTask => "Create a task with the message text".into(),
        QuickStepAction::CreateAppointment => "Create an appointment with the message text".into(),
        QuickStepAction::Ignore => "Ignore conversation".into(),
        QuickStepAction::AlwaysMoveConversation(f) => format!("Always move conversation to: {}", folder(f)),
        QuickStepAction::MoveToFocused => "Move to Focused".into(),
        QuickStepAction::MoveToOther => "Move to Other".into(),
    }
}

fn imp_label(i: Importance) -> &'static str {
    match i {
        Importance::Low => "Low",
        Importance::Normal => "Normal",
        Importance::High => "High",
    }
}

// ---------------------------------------------------------------------------------------------------------
// Manage Quick Steps

#[derive(Clone, Default)]
struct ManageState {
    selected: Option<u64>,
}

fn manage(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| ManageState { selected: a.s.mailbox.quick_steps.first().map(|q| q.id) };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Manage Quick Steps", 580.0, |ui, app| {
            let steps = app.s.mailbox.quick_steps.clone();
            if st.selected.is_none_or(|id| !steps.iter().any(|q| q.id == id)) {
                st.selected = steps.first().map(|q| q.id);
            }
            let pos = st.selected.and_then(|id| steps.iter().position(|q| q.id == id));
            let sel = pos.and_then(|i| steps.get(i)).cloned();
            let mut out = Out::Keep;
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    ui.label(RichText::new("Quick step").strong());
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                        ui,
                        |ui| {
                            ui.set_min_height(220.0);
                            ui.set_min_width(220.0);
                            for q in &steps {
                                let r = ui.selectable_label(st.selected == Some(q.id), &q.name);
                                if r.clicked() {
                                    st.selected = Some(q.id);
                                }
                                if r.double_clicked() {
                                    app.run("quicksteps.edit", json!({"id": q.id}));
                                }
                            }
                        },
                    );
                    ui.horizontal(|ui| {
                        let up = pos.is_some_and(|i| i > 0);
                        let down = pos.is_some_and(|i| i + 1 < steps.len());
                        if ui.add_enabled(up, egui::Button::new("▲")).on_hover_text("Move Up in List").clicked()
                            && let Some(q) = &sel
                        {
                            app.run("quicksteps.moveUp", json!({"id": q.id}));
                        }
                        if ui.add_enabled(down, egui::Button::new("▼")).on_hover_text("Move Down in List").clicked()
                            && let Some(q) = &sel
                        {
                            app.run("quicksteps.moveDown", json!({"id": q.id}));
                        }
                        ui.menu_button("New", |ui| {
                            for (key, label) in TEMPLATES {
                                if ui.button(label).clicked() {
                                    app.run("quicksteps.new", json!({"template": key}));
                                    ui.close();
                                }
                            }
                        });
                    });
                });
                ui.vertical(|ui| {
                    ui.label(RichText::new("Description").strong());
                    if let Some(q) = &sel {
                        ui.label(RichText::new(&q.name).strong());
                        ui.label("Actions");
                        if q.actions.is_empty() {
                            muted(ui, app, "  (no actions yet)");
                        }
                        for a in &q.actions {
                            ui.label(format!("  {}", action_text(app, a)));
                        }
                        ui.label("Shortcut key");
                        ui.label(format!("  {}", if q.shortcut.is_empty() { "None" } else { &q.shortcut }));
                        ui.label("Tooltip");
                        ui.label(format!("  {}", if q.tooltip.is_empty() { "(none)" } else { &q.tooltip }));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if btn_en(ui, sel.is_some(), "Edit")
                            && let Some(q) = &sel
                        {
                            app.run("quicksteps.edit", json!({"id": q.id}));
                        }
                        if btn_en(ui, sel.is_some(), "Duplicate")
                            && let Some(q) = &sel
                            && let Some(v) = app.run("quicksteps.duplicate", json!({"id": q.id}))
                        {
                            st.selected = v.get("id").and_then(Value::as_u64);
                        }
                        if btn_en(ui, sel.is_some(), "Delete")
                            && let Some(q) = &sel
                        {
                            out = Out::open(
                                "confirm",
                                json!({"title": "Manage Quick Steps", "text": format!("Delete the quick step \"{}\"?", q.name), "command": "quicksteps.delete", "params": {"id": q.id}}),
                            );
                        }
                    });
                });
            });
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, true, "OK") {
                    return Out::Close;
                }
                if btn(ui, "Reset to Defaults") {
                    return Out::open(
                        "confirm",
                        json!({"title": "Manage Quick Steps", "text": "Put back the default quick steps? Quick steps you made are removed.", "command": "quicksteps.reset", "params": {}}),
                    );
                }
                Out::Keep
            })
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Edit Quick Step

/// One action row of the editor.
#[derive(Clone, Debug, Default, PartialEq)]
struct Act {
    kind: usize,
    folder: Option<u64>,
    /// Category, or recipients ("a; b").
    text: String,
    /// Importance / flag choice.
    choice: usize,
}

impl Act {
    fn from(a: &QuickStepAction) -> Act {
        let k = |kind: usize| Act { kind, ..Default::default() };
        match a {
            QuickStepAction::MoveTo(f) => Act { kind: 0, folder: f.map(|f| f.0), ..Default::default() },
            QuickStepAction::CopyTo(f) => Act { kind: 1, folder: f.map(|f| f.0), ..Default::default() },
            QuickStepAction::Delete => k(2),
            QuickStepAction::PermanentlyDelete => k(3),
            QuickStepAction::MarkRead => k(4),
            QuickStepAction::MarkUnread => k(5),
            QuickStepAction::SetImportance(i) => Act {
                kind: 6,
                choice: match i {
                    Importance::Low => 0,
                    Importance::Normal => 1,
                    Importance::High => 2,
                },
                ..Default::default()
            },
            QuickStepAction::Categorize(c) => Act { kind: 7, text: c.clone(), ..Default::default() },
            QuickStepAction::ClearCategories => k(8),
            QuickStepAction::Flag(f) => Act {
                kind: 9,
                choice: FLAGS.iter().position(|(key, _)| key == f).unwrap_or(0),
                ..Default::default()
            },
            QuickStepAction::ClearFlag => k(10),
            QuickStepAction::MarkComplete => k(11),
            QuickStepAction::NewMessageTo(to) => Act { kind: 12, text: to.join("; "), ..Default::default() },
            QuickStepAction::Forward(to) => Act { kind: 13, text: to.join("; "), ..Default::default() },
            QuickStepAction::Reply => k(14),
            QuickStepAction::ReplyAll => k(15),
            QuickStepAction::NewMeeting => k(16),
            QuickStepAction::ForwardAsAttachment(to) => {
                Act { kind: 17, text: to.join("; "), ..Default::default() }
            }
            QuickStepAction::ReplyWithMeeting => k(18),
            QuickStepAction::CreateTask => k(19),
            QuickStepAction::CreateAppointment => k(20),
            QuickStepAction::Ignore => k(21),
            QuickStepAction::AlwaysMoveConversation(f) => {
                Act { kind: 22, folder: f.map(|f| f.0), ..Default::default() }
            }
            QuickStepAction::MoveToFocused => k(23),
            QuickStepAction::MoveToOther => k(24),
        }
    }

    fn build(&self) -> QuickStepAction {
        match self.kind {
            0 => QuickStepAction::MoveTo(self.folder.map(FolderId)),
            1 => QuickStepAction::CopyTo(self.folder.map(FolderId)),
            2 => QuickStepAction::Delete,
            3 => QuickStepAction::PermanentlyDelete,
            4 => QuickStepAction::MarkRead,
            5 => QuickStepAction::MarkUnread,
            6 => QuickStepAction::SetImportance(match self.choice {
                0 => Importance::Low,
                2 => Importance::High,
                _ => Importance::Normal,
            }),
            7 => QuickStepAction::Categorize(self.text.trim().to_owned()),
            8 => QuickStepAction::ClearCategories,
            9 => QuickStepAction::Flag(FLAGS.get(self.choice).map_or("today", |(k, _)| k).to_owned()),
            10 => QuickStepAction::ClearFlag,
            11 => QuickStepAction::MarkComplete,
            12 => QuickStepAction::NewMessageTo(common::split_list(&self.text)),
            13 => QuickStepAction::Forward(common::split_list(&self.text)),
            14 => QuickStepAction::Reply,
            15 => QuickStepAction::ReplyAll,
            17 => QuickStepAction::ForwardAsAttachment(common::split_list(&self.text)),
            18 => QuickStepAction::ReplyWithMeeting,
            19 => QuickStepAction::CreateTask,
            20 => QuickStepAction::CreateAppointment,
            21 => QuickStepAction::Ignore,
            22 => QuickStepAction::AlwaysMoveConversation(self.folder.map(FolderId)),
            23 => QuickStepAction::MoveToFocused,
            24 => QuickStepAction::MoveToOther,
            _ => QuickStepAction::NewMeeting,
        }
    }
}

#[derive(Clone, Default)]
struct EditState {
    id: Option<u64>,
    name: String,
    acts: Vec<Act>,
    shortcut: String,
    tooltip: String,
    error: String,
}

fn parse_actions(v: Option<&Value>) -> Vec<QuickStepAction> {
    v.cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn edit(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| {
        let step = d.get("step");
        let id = step
            .and_then(|s| s.get("id"))
            .and_then(Value::as_u64)
            .filter(|id| a.s.mailbox.quick_steps.iter().any(|q| q.id == *id));
        let get = |k: &str| {
            step.and_then(|s| s.get(k))
                .or_else(|| d.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let actions = parse_actions(step.and_then(|s| s.get("actions")).or_else(|| d.get("actions")));
        let shortcut = get("shortcut");
        EditState {
            id,
            name: get("name"),
            acts: if actions.is_empty() {
                vec![Act { kind: 0, ..Default::default() }]
            } else {
                actions.iter().map(Act::from).collect()
            },
            shortcut: if shortcut.is_empty() { "None".into() } else { shortcut },
            tooltip: get("tooltip"),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if let Some(i) = k.strip_prefix("folder:").and_then(|i| i.parse::<usize>().ok())
                && let Some(a) = st.acts.get_mut(i)
            {
                a.folder = v.as_u64();
            }
        }
        window(ctx, app, d, "Edit Quick Step", 540.0, |ui, app| {
            common::field(ui, "Name:", &mut st.name, 380.0);
            ui.add_space(4.0);
            ui.label("Add actions below that run, in order, when this quick step is clicked.");
            ui.label(RichText::new("Actions").strong());
            let out = action_rows(ui, app, uid, st);
            if btn(ui, "Add Action") {
                st.acts.push(Act { kind: 4, ..Default::default() });
            }
            common::heading(ui, app, "Optional");
            ui.horizontal(|ui| {
                ui.label("Shortcut key:");
                common::combo(ui, ("qs-key", uid), &mut st.shortcut, &KEYS, 140.0);
            });
            common::field(ui, "Tooltip text:", &mut st.tooltip, 340.0);
            muted(ui, app, "The tooltip shows when the mouse rests on the quick step.");
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, !st.name.trim().is_empty() && !st.acts.is_empty(), "Finish") {
                    if save(app, st) {
                        return Out::Close;
                    }
                    st.error = last_error(app);
                }
                Out::Keep
            })
        })
    })
}

/// The editable action rows; returns a picker to open.
fn action_rows(ui: &mut Ui, app: &mut App, uid: u64, st: &mut EditState) -> Out {
    let mut out = Out::Keep;
    let mut remove = None;
    let mut swap: Option<(usize, usize)> = None;
    let cats = crate::dialogs::categories::category_names(app);
    let n = st.acts.len();
    for (i, a) in st.acts.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            common::combo_idx(ui, ("qs-kind", uid, i), &mut a.kind, &KINDS, 180.0);
            match a.kind {
                0 | 1 | 22 => {
                    let p = folder_path(app, a.folder);
                    ui.label(if p.is_empty() { "Choose folder on first use".to_owned() } else { p });
                    if ui.button("Folder...").clicked() {
                        out = Out::open(
                            "pickFolder",
                            child(uid, &format!("folder:{i}"), json!({"selected": a.folder})),
                        );
                    }
                }
                6 => {
                    common::combo_idx(ui, ("qs-imp", uid, i), &mut a.choice, &IMPORTANCE, 100.0);
                }
                7 => {
                    let c: Vec<&str> = cats.iter().map(String::as_str).collect();
                    common::combo(ui, ("qs-cat", uid, i), &mut a.text, &c, 160.0);
                }
                9 => {
                    let labels: Vec<&str> = FLAGS.iter().map(|(_, l)| *l).collect();
                    common::combo_idx(ui, ("qs-flag", uid, i), &mut a.choice, &labels, 120.0);
                }
                12 | 13 | 17 => {
                    ui.add(
                        egui::TextEdit::singleline(&mut a.text)
                            .hint_text("To (empty: ask on first use)")
                            .desired_width(170.0),
                    );
                }
                _ => {}
            }
            if ui.add_enabled(i > 0, egui::Button::new("▲")).on_hover_text("Move action up").clicked() {
                swap = Some((i, i.saturating_sub(1)));
            }
            if ui.add_enabled(i + 1 < n, egui::Button::new("▼")).on_hover_text("Move action down").clicked()
            {
                swap = Some((i, i + 1));
            }
            if ui.button("✕").on_hover_text("Delete action").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some((a, b)) = swap
        && a < st.acts.len()
        && b < st.acts.len()
    {
        st.acts.swap(a, b);
    }
    if let Some(i) = remove
        && i < st.acts.len()
    {
        st.acts.remove(i);
    }
    out
}

/// Runs quicksteps.edit / quicksteps.new with the editor's values; true on success.
fn save(app: &mut App, st: &EditState) -> bool {
    let actions: Vec<QuickStepAction> = st.acts.iter().map(Act::build).collect();
    let shortcut = if st.shortcut == "None" { String::new() } else { st.shortcut.clone() };
    let mut p = json!({"name": st.name.trim(), "actions": actions, "shortcut": shortcut, "tooltip": st.tooltip.trim()});
    let cmd = match st.id {
        Some(id) => {
            if let Some(o) = p.as_object_mut() {
                o.insert("id".into(), json!(id));
            }
            "quicksteps.edit"
        }
        None => {
            if let Some(o) = p.as_object_mut() {
                o.insert("template".into(), json!("custom"));
            }
            "quicksteps.new"
        }
    };
    app.run(cmd, p).is_some()
}

// ---------------------------------------------------------------------------------------------------------
// First Time Setup

#[derive(Clone, Default)]
struct FirstUse {
    folder: Option<u64>,
    to: String,
    name: String,
    /// The user typed a name (a picked folder no longer renames the step).
    named: bool,
}

/// Name a step gets from its folder when the user leaves the placeholder name ("Move to: ?").
fn placeholder(name: &str) -> bool {
    name.contains('?')
}

/// What First Time Setup asks for: (a folder, recipients), as the engine's first-use check decides.
fn first_use_needs(actions: &[QuickStepAction]) -> (bool, bool) {
    let step = QuickStep { actions: actions.to_vec(), ..QuickStep::default() };
    (step.needs_folder(), step.needs_recipients())
}

#[cfg(test)]
#[test]
fn first_use_asks_for_what_the_engine_needs() {
    let needs = |a: QuickStepAction| first_use_needs(&[a]);
    assert_eq!(needs(QuickStepAction::ForwardAsAttachment(Vec::new())), (false, true));
    assert_eq!(needs(QuickStepAction::ForwardAsAttachment(vec!["a@example.com".into()])), (false, false));
    assert_eq!(needs(QuickStepAction::AlwaysMoveConversation(None)), (true, false));
    assert_eq!(needs(QuickStepAction::MoveTo(None)), (true, false));
    assert_eq!(needs(QuickStepAction::Forward(Vec::new())), (false, true));
    assert_eq!(needs(QuickStepAction::MarkRead), (false, false));
}

fn first_use(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let step = d.get("step").cloned().unwrap_or(Value::Null);
    let actions = parse_actions(step.get("actions"));
    let (needs_folder, needs_to) = first_use_needs(&actions);
    let init = |_: &App| FirstUse {
        name: step.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "folder" {
                st.folder = v.as_u64();
                if !st.named
                    && placeholder(&st.name)
                    && let Some(f) = st.folder.and_then(|f| app.s.mailbox.folder(FolderId(f)))
                {
                    st.name = f.name.clone();
                }
            }
        }
        window(ctx, app, d, "First Time Setup", 440.0, |ui, app| {
            ui.label("Set up this quick step before its first use.");
            ui.horizontal(|ui| {
                ui.label("Name:");
                if ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(320.0)).changed() {
                    st.named = true;
                }
            });
            common::heading(ui, app, "Actions");
            let mut out = Out::Keep;
            for a in &actions {
                ui.label(action_text(app, a));
            }
            if needs_folder {
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    let p = folder_path(app, st.folder);
                    ui.label(
                        RichText::new(if p.is_empty() { "Choose folder".to_owned() } else { p }).strong(),
                    );
                    if btn(ui, "Choose Folder...") {
                        out = Out::open("pickFolder", child(uid, "folder", json!({"selected": st.folder})));
                    }
                });
            }
            if needs_to {
                common::field(ui, "To:", &mut st.to, 320.0);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            let ready = !st.name.trim().is_empty()
                && (!needs_folder || st.folder.is_some())
                && (!needs_to || !common::split_list(&st.to).is_empty());
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, ready, "Save") {
                    let mut p = json!({"id": step.get("id"), "name": st.name.trim()});
                    if let Some(o) = p.as_object_mut() {
                        if let Some(f) = st.folder {
                            o.insert("folder".into(), json!(f));
                        }
                        if needs_to {
                            o.insert("to".into(), json!(common::split_list(&st.to)));
                        }
                        // The items the step was run on (a read window's message, a group's items).
                        for key in ["ids", "group"] {
                            if let Some(v) = d.get(key) {
                                o.insert(key.into(), v.clone());
                            }
                        }
                    }
                    app.run("quicksteps.run", p);
                    return Out::Close;
                }
                if btn(ui, "Options...") {
                    return Out::close_open("editQuickStep", json!({"step": step}));
                }
                Out::Keep
            })
        })
    })
}
