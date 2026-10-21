//! Send/Receive Groups (Send / Receive › Send/Receive Groups › Define Send/Receive Groups, Options ›
//! Advanced › Send/Receive): the groups with New, Edit, Copy, Remove and Rename, the settings of the selected
//! group (online: include in F9, schedule, run on exit; offline: include, schedule) and the group editor
//! (accounts, what each one sends and receives, which folders). The engine command
//! `sendReceive.defineGroups` keeps them (`mailcraft_engine::sync::groups`). The built-in All Accounts group
//! shows the Options › Advanced send/receive settings, which it uses.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{Dlg, Out, btn, btn_en, footer, heading, muted, window, with_state};
use crate::App;

/// What the name prompt is for.
#[derive(Clone, Debug, PartialEq)]
enum Prompt {
    New,
    Copy(String),
    Rename(String),
}

#[derive(Clone, Default)]
struct GroupsState {
    /// The engine's listing (`groups`, `accounts`).
    data: Value,
    selected: usize,
    prompt: Option<(Prompt, String)>,
    /// The group being edited (a copy of its JSON) and the account shown in the editor.
    editing: Option<(Value, Option<u64>)>,
    error: String,
}

fn list(app: &mut App) -> Value {
    app.s.run("sendReceive.defineGroups", &json!({"action": "list"})).unwrap_or(Value::Null)
}

fn groups(st: &GroupsState) -> Vec<Value> {
    st.data.get("groups").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn s_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn b_of(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Sets `k` on a JSON object (anything else is left as it is).
fn put(v: &mut Value, k: &str, x: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(k.to_owned(), x);
    }
}

fn n_of(v: &Value, k: &str) -> u32 {
    v.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

/// "Online and offline", "Online only", … for the list.
fn when_text(g: &Value) -> &'static str {
    let online = b_of(g, "include") || n_of(g, "everyMinutes") > 0 || b_of(g, "onExit");
    let offline = b_of(g, "offlineInclude") || n_of(g, "offlineEveryMinutes") > 0;
    match (online, offline) {
        (true, true) => "Online and offline",
        (true, false) => "Online only",
        (false, true) => "Offline only",
        (false, false) => "Only when chosen",
    }
}

/// Saves a user group (its JSON as the dialog holds it); errors land in the dialog.
fn save(app: &mut App, st: &mut GroupsState, g: &Value, old_name: Option<&str>) {
    let mut p = json!({"action": "save", "group": g});
    if let Some(o) = old_name {
        put(&mut p, "oldName", json!(o));
    }
    match app.s.run("sendReceive.defineGroups", &p) {
        Ok(_) => st.error.clear(),
        Err(e) => st.error = e.to_string(),
    }
    st.data = list(app);
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let initial = d.params.clone();
    with_state(
        ctx,
        app,
        uid,
        |_: &App| GroupsState { data: initial, ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Send/Receive Groups", 600.0, |ui, app| {
                if st.editing.is_some() {
                    return editor(ui, app, st, uid);
                }
                muted(
                    ui,
                    app,
                    "A group is a set of accounts and folders that send and receive together, with its own \
                     schedule.",
                );
                let gs = groups(st);
                st.selected = st.selected.min(gs.len().saturating_sub(1));
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(400.0);
                        egui::Grid::new(("srg", uid)).num_columns(2).striped(true).min_col_width(180.0).show(
                            ui,
                            |ui| {
                                ui.label(RichText::new("Group Name").strong());
                                ui.label(RichText::new("Send/Receive when").strong());
                                ui.end_row();
                                for (i, g) in gs.iter().enumerate() {
                                    if ui.selectable_label(st.selected == i, s_of(g, "name")).clicked() {
                                        st.selected = i;
                                        st.prompt = None;
                                    }
                                    ui.label(when_text(g));
                                    ui.end_row();
                                }
                            },
                        );
                    });
                    ui.vertical(|ui| {
                        let sel = gs.get(st.selected).cloned().unwrap_or(Value::Null);
                        let user = !b_of(&sel, "builtIn") && !sel.is_null();
                        let name = s_of(&sel, "name");
                        if btn(ui, "New...") {
                            st.prompt = Some((Prompt::New, String::new()));
                        }
                        if btn_en(ui, user, "Edit...") {
                            st.editing = Some((sel.clone(), None));
                        }
                        if btn_en(ui, user, "Copy...") {
                            st.prompt = Some((Prompt::Copy(name.clone()), format!("Copy of {name}")));
                        }
                        if btn_en(ui, user, "Remove") {
                            match app
                                .s
                                .run("sendReceive.defineGroups", &json!({"action": "remove", "name": name}))
                            {
                                Ok(_) => st.error.clear(),
                                Err(e) => st.error = e.to_string(),
                            }
                            st.data = list(app);
                        }
                        if btn_en(ui, user, "Rename") {
                            st.prompt = Some((Prompt::Rename(name.clone()), name.clone()));
                        }
                    });
                });
                if let Some((what, text)) = st.prompt.as_mut() {
                    ui.add_space(6.0);
                    let mut go = false;
                    let mut cancel = false;
                    ui.horizontal(|ui| {
                        ui.label("Group name:");
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(220.0));
                        go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        go |= btn_en(ui, !text.trim().is_empty(), "OK");
                        cancel = btn(ui, "Cancel");
                    });
                    if go {
                        let new = text.trim().to_owned();
                        let p = match what {
                            Prompt::New => json!({"action": "save", "group": {"name": new, "members": []}}),
                            Prompt::Copy(from) => json!({"action": "copy", "name": from, "newName": new}),
                            Prompt::Rename(from) => json!({"action": "rename", "name": from, "newName": new}),
                        };
                        let opened_editor = matches!(what, Prompt::New);
                        match app.s.run("sendReceive.defineGroups", &p) {
                            Ok(_) => {
                                st.error.clear();
                                st.prompt = None;
                                st.data = list(app);
                                if let Some(i) = groups(st).iter().position(|g| s_of(g, "name") == new) {
                                    st.selected = i;
                                    if opened_editor {
                                        st.editing = groups(st).get(i).cloned().map(|g| (g, None));
                                    }
                                }
                            }
                            Err(e) => st.error = e.to_string(),
                        }
                    } else if cancel {
                        st.prompt = None;
                    }
                }
                let gs = groups(st);
                if let Some(sel) = gs.get(st.selected).cloned() {
                    settings(ui, app, st, &sel, uid);
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}

/// The schedule settings of the selected group, applied as they change.
fn settings(ui: &mut egui::Ui, app: &mut App, st: &mut GroupsState, sel: &Value, uid: u64) {
    let name = s_of(sel, "name");
    let built_in = b_of(sel, "builtIn");
    heading(ui, app, &format!("Settings for group \"{name}\""));
    let mut g = sel.clone();
    let mut changed = false;
    ui.label(RichText::new("When MailCraft is online").strong());
    ui.add_enabled_ui(!built_in, |ui| {
        let mut include = b_of(&g, "include");
        if ui.checkbox(&mut include, "Include this group in send/receive (F9)").changed() {
            put(&mut g, "include", json!(include));
            changed = true;
        }
    });
    let mut minutes = n_of(&g, "everyMinutes");
    let mut scheduled = minutes > 0;
    ui.horizontal(|ui| {
        let c = ui.checkbox(&mut scheduled, "Schedule an automatic send/receive every");
        let v = ui.add_enabled(scheduled, egui::DragValue::new(&mut minutes).range(1..=1440));
        ui.label("minutes");
        if c.changed() {
            minutes = if scheduled { minutes.max(30) } else { 0 };
            changed = true;
        }
        changed |= v.changed();
    });
    put(&mut g, "everyMinutes", json!(if scheduled { minutes.max(1) } else { 0 }));
    let mut on_exit = b_of(&g, "onExit");
    if ui.checkbox(&mut on_exit, "Perform an automatic send/receive when exiting").changed() {
        put(&mut g, "onExit", json!(on_exit));
        changed = true;
    }
    ui.add_space(4.0);
    ui.label(RichText::new("When working offline").strong());
    ui.add_enabled_ui(!built_in, |ui| {
        let mut off_include = b_of(&g, "offlineInclude");
        if ui.checkbox(&mut off_include, "Include this group in send/receive (F9)").changed() {
            put(&mut g, "offlineInclude", json!(off_include));
            changed = true;
        }
        let mut off_minutes = n_of(&g, "offlineEveryMinutes");
        let mut off_sched = off_minutes > 0;
        ui.horizontal(|ui| {
            let c = ui.checkbox(&mut off_sched, "Schedule an automatic send/receive every");
            let v =
                ui.add_enabled(off_sched, egui::DragValue::new(&mut off_minutes).range(1..=1440).speed(1.0));
            ui.label("minutes");
            if c.changed() {
                off_minutes = if off_sched { off_minutes.max(30) } else { 0 };
                changed = true;
            }
            changed |= v.changed();
        });
        put(&mut g, "offlineEveryMinutes", json!(if off_sched { off_minutes.max(1) } else { 0 }));
    });
    if built_in {
        muted(ui, app, "All Accounts uses the send/receive options of File › Options › Advanced.");
    }
    let _ = uid;
    if !changed {
        return;
    }
    if built_in {
        let err = super::common::set_options(
            app,
            &[
                ("advanced.sendReceiveMinutes", g.get("everyMinutes").cloned().unwrap_or(Value::Null)),
                ("advanced.sendReceiveOnExit", g.get("onExit").cloned().unwrap_or(Value::Null)),
            ],
        );
        st.error = err.unwrap_or_default();
        st.data = list(app);
    } else {
        save(app, st, &g, Some(&name));
    }
}

/// The group editor: accounts on the left, what the chosen account does on the right.
fn editor(ui: &mut egui::Ui, app: &mut App, st: &mut GroupsState, uid: u64) -> Out {
    let accounts: Vec<Value> = st.data.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
    let Some((group, shown)) = st.editing.as_mut() else { return Out::Keep };
    let name = s_of(group, "name");
    heading(ui, app, &format!("Group \"{name}\""));
    if accounts.is_empty() {
        muted(ui, app, "There are no accounts with a mail server to add.");
    }
    if shown.is_none() {
        *shown = accounts.first().and_then(|a| a.get("id")).and_then(Value::as_u64);
    }
    let members = group.get("members").and_then(Value::as_array).cloned().unwrap_or_default();
    let member_of =
        |id: u64| members.iter().position(|m| m.get("account").and_then(Value::as_u64) == Some(id));
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(200.0);
            ui.label(RichText::new("Accounts").strong());
            for a in &accounts {
                let Some(id) = a.get("id").and_then(Value::as_u64) else { continue };
                let label =
                    format!("{}{}", if member_of(id).is_some() { "✓ " } else { "   " }, s_of(a, "name"));
                if ui.selectable_label(*shown == Some(id), label).clicked() {
                    *shown = Some(id);
                }
            }
        });
        ui.vertical(|ui| {
            let Some(id) = *shown else { return };
            let Some(acct) = accounts.iter().find(|a| a.get("id").and_then(Value::as_u64) == Some(id)) else {
                return;
            };
            let mut members = members.clone();
            let idx = member_of(id);
            let mut included = idx.is_some();
            let mut edited = false;
            if ui.checkbox(&mut included, "Include this account in the group").changed() {
                if included {
                    members.push(json!({"account": id, "send": true, "receive": true, "headersOnly": false, "folders": null}));
                } else if let Some(i) = idx {
                    members.remove(i);
                }
                edited = true;
            }
            if let Some(m) = member_of_list(&members, id).and_then(|i| members.get_mut(i)) {
                ui.add_space(4.0);
                for (key, label) in [
                    ("send", "Send mail items"),
                    ("receive", "Receive mail items"),
                    ("headersOnly", "Download headers only"),
                ] {
                    let mut on = b_of(m, key);
                    if ui.checkbox(&mut on, label).changed() {
                        put(m, key, json!(on));
                        edited = true;
                    }
                }
                ui.add_space(4.0);
                let all = m.get("folders").is_none_or(Value::is_null);
                if ui.radio(all, "Update every folder").clicked() && !all {
                    put(m, "folders", Value::Null);
                    edited = true;
                }
                if ui.radio(!all, "Update only the folders checked below").clicked() && all {
                    put(m, "folders", json!([]));
                    edited = true;
                }
                if !all {
                    let mut picked: Vec<u64> = m
                        .get("folders")
                        .and_then(Value::as_array)
                        .map(|l| l.iter().filter_map(Value::as_u64).collect())
                        .unwrap_or_default();
                    egui::ScrollArea::vertical().id_salt(("srgf", uid, id)).max_height(160.0).show(ui, |ui| {
                        for f in acct.get("folders").and_then(Value::as_array).into_iter().flatten() {
                            let Some(fid) = f.get("id").and_then(Value::as_u64) else { continue };
                            let mut on = picked.contains(&fid);
                            if ui.checkbox(&mut on, s_of(f, "path")).changed() {
                                if on {
                                    picked.push(fid);
                                } else {
                                    picked.retain(|x| *x != fid);
                                }
                                edited = true;
                            }
                        }
                    });
                    put(m, "folders", json!(picked));
                }
            }
            if edited {
                put(group, "members", json!(members));
            }
        });
    });
    if !st.error.is_empty() {
        ui.colored_label(app.theme.danger, &st.error);
    }
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            st.editing = None;
            st.error.clear();
            return Out::Keep;
        }
        if btn(ui, "OK") {
            let g = st.editing.as_ref().map(|(g, _)| g.clone()).unwrap_or(Value::Null);
            let name = s_of(&g, "name");
            save(app, st, &g, Some(&name));
            if st.error.is_empty() {
                st.editing = None;
            }
        }
        Out::Keep
    })
}

fn member_of_list(members: &[Value], id: u64) -> Option<usize> {
    members.iter().position(|m| m.get("account").and_then(Value::as_u64) == Some(id))
}
