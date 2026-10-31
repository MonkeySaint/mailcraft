//! AutoArchive dialogs and the scheduler (engine `mailcraft_engine::archive`):
//!
//! - `autoArchiveSettings`: a folder's AutoArchive settings (Folder › AutoArchive Settings); the same form is
//!   the AutoArchive tab of folder Properties ([`FolderForm`]). Saved with `folder.autoArchiveSettings`.
//! - `autoArchiveOptions`: File › Options › Advanced › AutoArchive Settings... — the schedule, prompt,
//!   expired items and the default folder settings; "Apply these settings to all folders now". Saved with
//!   `options.autoArchive`.
//! - `archive`: File › Info › Tools › Clean Up Old Items... — archive every folder by its settings, or one
//!   folder and its subfolders older than a date (`file.archive`).
//! - `autoArchivePrompt`: "Prompt before AutoArchive runs".
//!
//! [`tick`] runs every frame: a few seconds after start-up and then once a minute, while nothing is going on
//! (no dialog, no File view, no mouse button or key held, no send/receive running), it asks the engine
//! whether AutoArchive is due and runs it (or asks first). A run handles a bounded number of items, so a
//! large backlog goes in steps, one per frame, instead of freezing the window. Steps stop taking server mail
//! while an account's change queue is full; a step that could do nothing waits [`BLOCKED_SECS`] for
//! send/receive to catch up.

use egui::{Context, Id};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, Pick, btn, child, folder_path, folder_tree, footer, muted, ok_cancel, primary,
    take_replies, window, with_state,
};
use crate::App;
use mailcraft_engine::archive;
use mailcraft_model::FolderId;
use mailcraft_model::folder::{AgeUnit, ArchiveAction, ArchiveMode};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "autoArchiveSettings" => folder_dialog(ctx, app, d),
        "autoArchiveOptions" => options_dialog(ctx, app, d),
        "archive" => archive_dialog(ctx, app, d),
        "autoArchivePrompt" => prompt(ctx, app, d),
        _ => return None,
    })
}

const UNITS: [&str; 3] = ["Days", "Weeks", "Months"];

/// "Archive items older than 6 months to Archives" for the default settings.
fn defaults_text(app: &App) -> String {
    let d = archive::defaults(&app.s);
    let months = format!("{} month{}", d.older_than_months, if d.older_than_months == 1 { "" } else { "s" });
    match d.action {
        ArchiveAction::Delete => format!("Default: permanently delete items older than {months}."),
        ArchiveAction::Move => {
            let to = if d.folder.trim().is_empty() { archive::ARCHIVES_NAME.to_owned() } else { d.folder };
            format!("Default: move items older than {months} to {to}.")
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Folder settings

/// A folder's AutoArchive settings as edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FolderForm {
    /// 0 = do not archive, 1 = default settings, 2 = these settings.
    pub mode: usize,
    pub age: u32,
    /// Index into [`UNITS`].
    pub unit: usize,
    pub delete: bool,
    /// "Move old items to" folder (`None` = the archive folder).
    pub target: Option<u64>,
}

impl FolderForm {
    /// The folder's saved settings.
    pub(crate) fn load(app: &App, folder: u64) -> FolderForm {
        let p = archive::folder_props(&app.s.mailbox, FolderId(folder));
        FolderForm {
            mode: match p.mode {
                ArchiveMode::Never => 0,
                ArchiveMode::Default => 1,
                ArchiveMode::Custom => 2,
            },
            age: p.age.max(1),
            unit: match p.unit {
                AgeUnit::Days => 0,
                AgeUnit::Weeks => 1,
                AgeUnit::Months => 2,
            },
            delete: p.action == ArchiveAction::Delete,
            target: p.target.map(|t| t.0).filter(|t| app.s.mailbox.folders.contains_key(&FolderId(*t))),
        }
    }

    /// `folder.autoArchiveSettings` params that save this form.
    pub(crate) fn params(&self, folder: u64) -> Value {
        let mode = ["never", "default", "custom"].get(self.mode).copied().unwrap_or("default");
        let unit = ["days", "weeks", "months"].get(self.unit).copied().unwrap_or("months");
        json!({
            "folder": folder,
            "mode": mode,
            "age": self.age,
            "unit": unit,
            "action": if self.delete { "delete" } else { "move" },
            "target": self.target,
        })
    }

    /// Saves the form; the error text when the engine refuses it.
    pub(crate) fn save(&self, app: &mut App, folder: u64) -> Result<(), String> {
        match app.run("folder.autoArchiveSettings", self.params(folder)) {
            Some(_) => Ok(()),
            None => Err(super::folders::last_error(app)),
        }
    }
}

/// The form's controls. Browse opens a folder picker that replies to dialog `uid` under `reply_key`.
pub(crate) fn folder_form(
    ui: &mut egui::Ui,
    app: &App,
    form: &mut FolderForm,
    folder: u64,
    uid: u64,
    reply_key: &str,
) -> Out {
    let mb = &app.s.mailbox;
    let Some(f) = mb.folder(FolderId(folder)) else {
        muted(ui, app, "The folder no longer exists.");
        return Out::Keep;
    };
    if !archive::archivable(mb, f) {
        muted(ui, app, "Items in this folder are not archived.");
        return Out::Keep;
    }
    let pick = if f.kind == mailcraft_model::FolderKind::Mail { "mail" } else { "any" };
    ui.radio_value(&mut form.mode, 0, "Do not archive items in this folder");
    ui.radio_value(&mut form.mode, 1, "Archive items in this folder using the default settings");
    muted(ui, app, &defaults_text(app));
    ui.radio_value(&mut form.mode, 2, "Archive this folder using these settings:");
    let mut out = Out::Keep;
    ui.add_enabled_ui(form.mode == 2, |ui| {
        ui.indent(("aa-custom", uid), |ui| {
            ui.horizontal(|ui| {
                ui.label("Clean out items older than");
                ui.add(
                    egui::DragValue::new(&mut form.age).range(1..=mailcraft_model::folder::MAX_ARCHIVE_AGE),
                );
                common::combo_idx(ui, ("aa-unit", uid), &mut form.unit, &UNITS, 90.0);
            });
            ui.radio_value(&mut form.delete, false, "Move old items to:");
            ui.horizontal(|ui| {
                let path = folder_path(app, form.target);
                let shown = if path.is_empty() { "The archive folder (default)".to_owned() } else { path };
                ui.label(shown);
                if btn(ui, "Browse...") {
                    out = Out::open(
                        "pickFolder",
                        child(uid, reply_key, json!({"pick": pick, "title": "Move old items to"})),
                    );
                }
                if form.target.is_some() && btn(ui, "Default") {
                    form.target = None;
                }
            });
            ui.radio_value(&mut form.delete, true, "Permanently delete old items");
        });
    });
    out
}

#[derive(Clone, Default)]
struct FolderState {
    form: FolderForm,
    error: String,
}

fn folder_dialog(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = d.u64("folder").unwrap_or(app.s.selection.folder.0);
    let init = |a: &App| FolderState { form: FolderForm::load(a, folder), error: String::new() };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "target" {
                st.form.target = v.as_u64();
            }
        }
        let title = format!("{} Properties", d.str("name"));
        let title = if d.str("name").is_empty() { "AutoArchive".to_owned() } else { title };
        window(ctx, app, d, &title, 440.0, |ui, app| {
            common::heading(ui, app, "AutoArchive");
            let out = folder_form(ui, app, &mut st.form, folder, uid, "target");
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => match st.form.save(app, folder) {
                    Ok(()) => Out::Close,
                    Err(e) => {
                        st.error = e;
                        Out::Keep
                    }
                },
                Some(false) => Out::Close,
                None => out,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Global options

#[derive(Clone, Default)]
struct OptionsState {
    run: bool,
    every: u32,
    prompt: bool,
    delete_expired: bool,
    months: u32,
    delete: bool,
    folder: String,
    note: String,
    error: String,
}

fn options_dialog(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| {
        let o = archive::defaults(&a.s);
        OptionsState {
            run: o.run,
            every: o.every_days,
            prompt: o.prompt,
            delete_expired: o.delete_expired,
            months: o.older_than_months,
            delete: o.action == ArchiveAction::Delete,
            folder: o.folder,
            ..Default::default()
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "folder" {
                st.folder = folder_path(app, v.as_u64());
            }
        }
        window(ctx, app, d, "AutoArchive", 480.0, |ui, app| {
            let mut out = Out::Keep;
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.run, "Run AutoArchive every");
                ui.add_enabled(st.run, egui::DragValue::new(&mut st.every).range(1..=60));
                ui.label("days");
            });
            ui.add_enabled_ui(st.run, |ui| {
                ui.indent(("aa-run", uid), |ui| {
                    ui.checkbox(&mut st.prompt, "Prompt before AutoArchive runs");
                    common::heading(ui, app, "During AutoArchive:");
                    ui.checkbox(&mut st.delete_expired, "Delete expired items (email folders only)");
                });
            });
            common::heading(ui, app, "Default folder settings for archiving");
            ui.horizontal(|ui| {
                ui.label("Clean out items older than");
                ui.add(egui::DragValue::new(&mut st.months).range(1..=60));
                ui.label("months");
            });
            ui.radio_value(&mut st.delete, false, "Move old items to:");
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !st.delete,
                    egui::TextEdit::singleline(&mut st.folder)
                        .hint_text(archive::ARCHIVES_NAME)
                        .desired_width(260.0),
                );
                if ui.add_enabled(!st.delete, egui::Button::new("Browse...")).clicked() {
                    out = Out::open("pickFolder", child(uid, "folder", json!({"pick": "parent"})));
                }
            });
            ui.radio_value(&mut st.delete, true, "Permanently delete old items");
            ui.add_space(4.0);
            if btn(ui, "Apply these settings to all folders now") {
                st.note = match app.run("options.autoArchive", json!({"applyToAll": true})) {
                    Some(v) => {
                        let n = v.get("folders").and_then(Value::as_u64).unwrap_or(0);
                        format!("{n} folder{} now use the default settings.", if n == 1 { "" } else { "s" })
                    }
                    None => super::folders::last_error(app),
                };
            }
            if !st.note.is_empty() {
                muted(ui, app, &st.note);
            }
            muted(ui, app, "To change a folder's own settings, use its Properties › AutoArchive.");
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let params = json!({
                        "run": st.run,
                        "everyDays": st.every,
                        "prompt": st.prompt,
                        "deleteExpired": st.delete_expired,
                        "olderThanMonths": st.months,
                        "action": if st.delete { "delete" } else { "move" },
                        "folder": st.folder.trim(),
                    });
                    if app.run("options.autoArchive", params).is_some() {
                        Out::Close
                    } else {
                        st.error = super::folders::last_error(app);
                        Out::Keep
                    }
                }
                Some(false) => Out::Close,
                None => out,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Archive (Clean Up Old Items)

#[derive(Clone, Default)]
struct ArchiveState {
    all: bool,
    folder: Option<u64>,
    before: String,
    include: bool,
    target: Option<u64>,
    error: String,
}

fn archive_dialog(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |_: &App| ArchiveState {
        all: false,
        folder: d.u64("folder"),
        before: d.str("before"),
        include: d.bool("includeExcluded"),
        target: d.u64("target"),
        error: String::new(),
    };
    let default_target = d.str("defaultTarget");
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "target" {
                st.target = v.as_u64();
            }
        }
        window(ctx, app, d, "Archive", 440.0, |ui, app| {
            let mut out = Out::Keep;
            ui.radio_value(&mut st.all, true, "Archive all folders according to their AutoArchive settings");
            ui.radio_value(&mut st.all, false, "Archive this folder and all subfolders:");
            ui.add_enabled_ui(!st.all, |ui| {
                folder_tree(ui, app, &mut st.folder, Pick::Any, ("aa-tree", uid), 200.0);
                ui.horizontal(|ui| {
                    ui.label("Archive items older than:");
                    ui.add(
                        egui::TextEdit::singleline(&mut st.before)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(110.0),
                    );
                });
                ui.checkbox(&mut st.include, "Include items with \"Do not AutoArchive\" checked");
                ui.horizontal(|ui| {
                    ui.label("Archive folder:");
                    let path = folder_path(app, st.target);
                    ui.label(if path.is_empty() { default_target.clone() } else { path });
                    if btn(ui, "Browse...") {
                        out = Out::open("pickFolder", child(uid, "target", json!({"pick": "parent"})));
                    }
                });
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let ok_enabled = st.all || (st.folder.is_some() && !st.before.trim().is_empty());
            match ok_cancel(ui, app, ok_enabled) {
                Some(true) => {
                    let params = if st.all {
                        json!({"all": true})
                    } else {
                        json!({
                            "folder": st.folder,
                            "before": st.before.trim(),
                            "includeExcluded": st.include,
                            "target": st.target,
                        })
                    };
                    if app.run("file.archive", params).is_some() {
                        Out::Close
                    } else {
                        st.error = super::folders::last_error(app);
                        Out::Keep
                    }
                }
                Some(false) => Out::Close,
                None => out,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Prompt and scheduler

fn prompt(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let mut approved = false;
    let out = window(ctx, app, d, "AutoArchive", 400.0, |ui, app| {
        ui.label("AutoArchive is due. Archive old items now?");
        muted(ui, app, &defaults_text(app));
        footer(ui, |ui| {
            if btn(ui, "No") {
                return Out::Close;
            }
            if primary(ui, app, true, "Yes") {
                approve(ctx);
                approved = true;
                return Out::Close;
            }
            Out::Keep
        })
    });
    // No, Esc and the title-bar X all postpone: otherwise the next check would ask again a minute later.
    if matches!(out, Out::Close) && !approved {
        app.run("file.autoArchivePostpone", json!({}));
    }
    out
}

/// Seconds after start-up before the first check.
pub(crate) const STARTUP_DELAY_SECS: f64 = 10.0;
/// Seconds between checks.
pub(crate) const CHECK_SECS: f64 = 60.0;
/// Seconds before trying again when a step could do nothing (send/receive has not yet caught up with the
/// server changes earlier steps queued).
pub(crate) const BLOCKED_SECS: f64 = 900.0;

/// Scheduler state (egui memory).
#[derive(Clone, Debug, Default)]
pub(crate) struct Tick {
    /// egui time of the next check.
    pub next: Option<f64>,
    /// The prompt was answered Yes: run without asking until this run is done.
    pub approved: bool,
}

fn tick_id() -> Id {
    Id::unique("mc-auto-archive-tick")
}

pub(crate) fn load_tick(ctx: &Context) -> Tick {
    ctx.data_mut(|d| d.get_temp::<Tick>(tick_id())).unwrap_or_default()
}

pub(crate) fn store_tick(ctx: &Context, t: Tick) {
    ctx.data_mut(|d| d.insert_temp(tick_id(), t));
}

/// The prompt's Yes: run at the next frame.
fn approve(ctx: &Context) {
    let mut t = load_tick(ctx);
    t.approved = true;
    t.next = Some(ctx.input(|i| i.time));
    store_tick(ctx, t);
    ctx.request_repaint();
}

fn idle(ctx: &Context, app: &App) -> bool {
    app.ui.dialogs.is_empty()
        && app.ui.backstage.is_none()
        && !app.s.sync_busy()
        && !ctx.input(|i| i.pointer.any_down() || !i.keys_down.is_empty())
}

/// Checks the AutoArchive schedule when it is time (see the module docs). Called every frame.
pub(crate) fn tick(ctx: &Context, app: &mut App) {
    let mut t = load_tick(ctx);
    let now = ctx.input(|i| i.time);
    let next = *t.next.get_or_insert(now + STARTUP_DELAY_SECS);
    if now >= next {
        t.next = Some(now + CHECK_SECS);
        if idle(ctx, app) {
            check(ctx, app, &mut t, now);
        } else {
            // Busy: look again shortly.
            t.next = Some(now + 2.0);
        }
    }
    if app.s.settings.bool("advanced.autoArchive.run")
        && let Some(n) = t.next
    {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((n - now).clamp(0.1, CHECK_SECS)));
    }
    store_tick(ctx, t);
}

fn check(ctx: &Context, app: &mut App, t: &mut Tick, now: f64) {
    let sch = archive::poll(&mut app.s);
    if !sch.due {
        t.approved = false;
        return;
    }
    if sch.prompt && !t.approved {
        super::open(app, "autoArchivePrompt", json!({}));
        ctx.request_repaint();
        return;
    }
    match archive::run_scheduled(&mut app.s) {
        Ok(v) if v.get("remaining").and_then(Value::as_bool) == Some(true) => {
            // More to do. A step that did something goes on at the next idle frame: it only took server mail
            // while the queues had room, and the idle check waits for a running send/receive. A step that
            // could do nothing waits for send/receive to work the queues off.
            let done = ["moved", "deleted", "expired"]
                .iter()
                .filter_map(|k| v.get(*k).and_then(Value::as_u64))
                .fold(0u64, u64::saturating_add);
            t.next = Some(if done > 0 { now } else { now + BLOCKED_SECS });
            ctx.request_repaint();
        }
        Ok(_) => t.approved = false,
        Err(e) => {
            // Not again every minute: wait for the next scheduled run.
            archive::record_run(&mut app.s);
            t.approved = false;
            app.toast(ctx, format!("AutoArchive did not run: {e}"), true);
        }
    }
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
