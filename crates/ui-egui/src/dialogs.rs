//! Dialogs opened via `app.ui.dialogs` (pushed by the shell from `Notice::OpenDialog` and by UI code), incl.
//! the Options dialog — backstage/dialogs area.
//!
//! Every dialog id the engine emits has a renderer here (see [`KNOWN`]); unknown ids get a generic
//! "not available yet" box. Dialogs are egui modals stacked in `app.ui.dialogs` order (topmost last); each
//! gets a unique `_dlg` id injected into its params, which keys its egui-side state. Buttons act only through
//! `app.run(id, params)`.
//!
//! Helpers for other areas: [`open`] (push a dialog, routing `backstage` / `readWindow` / `exit`),
//! [`confirm`] (Yes/No box that runs a command), [`take_notices`] (moves `Notice::OpenDialog` from the
//! session into `app.ui.dialogs`; [`dialogs`] calls it every frame).

#[cfg(test)]
mod account_tests;
mod accounts;
mod alerts;
mod archive;
mod categories;
mod common;
mod compose_dlg;
pub(crate) mod files;
mod find;
mod folders;
mod general;
mod items;
mod junk;
mod mailopts;
mod options;
mod print;
mod quicksteps;
mod rules;
mod security;
mod views;

#[cfg(test)]
mod tests;

use egui::{Context, Id};
use serde_json::{Value, json};

use crate::{App, Surface};
use common::{Dlg, Out};
pub(crate) use common::{browse_folder, browse_open, browse_save};

pub fn dialogs(ctx: &egui::Context, app: &mut App) {
    take_notices(app);
    assign_uids(ctx, app);
    // Requests the modules draw themselves stay queued for them (next pass), so no generic window flashes.
    let (deferred, list): (Vec<_>, Vec<_>) = app
        .ui
        .dialogs
        .clone()
        .into_iter()
        .partition(|(name, params)| crate::modules::takes_dialog(app, name, params));
    if !deferred.is_empty() {
        ctx.request_repaint();
    }
    let n = list.len();
    let mut closed: Vec<u64> = Vec::new();
    let mut opened: Vec<(String, Value)> = Vec::new();
    for (i, (name, params)) in list.iter().enumerate() {
        let uid = uid_of(params);
        let d = Dlg { name, params, uid, top: i + 1 == n };
        match show(ctx, app, &d) {
            Out::Keep => {}
            Out::Close => closed.push(uid),
            Out::CloseOpen(v) => {
                closed.push(uid);
                opened.extend(v);
            }
            Out::Open(v) => opened.extend(v),
        }
    }
    if !closed.is_empty() {
        app.ui.dialogs.retain(|(_, p)| !closed.contains(&uid_of(p)));
        ctx.request_repaint();
    }
    for (name, params) in opened {
        open(app, &name, params);
        ctx.request_repaint();
    }
}

/// Opens dialog `name`. `backstage` opens the File view, `readWindow` a read window, `exit` closes the app
/// window; a dialog already open with the same id and params is not opened twice.
pub fn open(app: &mut App, name: &str, params: Value) {
    match name {
        "backstage" => {
            let page = params.get("page").and_then(Value::as_str).unwrap_or("info").to_owned();
            let surface =
                params.get("compose").and_then(Value::as_u64).map_or(Surface::Main, Surface::Compose);
            app.ui.backstage = Some((surface, page));
        }
        "mainWindow" => {
            // Open in New Window: another main window on the folder (or, past the limit, the folder here).
            if let Some(f) = params.get("folder").and_then(Value::as_u64)
                && crate::explorer::open(app, mailcraft_model::FolderId(f)).is_none()
            {
                app.run("nav.selectFolder", json!({"folder": f}));
            }
        }
        "readWindow" => {
            if let Some(id) = params.get("id").and_then(Value::as_u64)
                && !app.ui.read_windows.contains(&id)
            {
                app.ui.read_windows.push(id);
            }
        }
        _ => {
            let dup = app.ui.dialogs.iter().any(|(n, p)| n == name && strip_uid(p) == strip_uid(&params));
            if !dup {
                app.ui.dialogs.push((name.to_owned(), params));
            }
        }
    }
}

/// The AutoArchive scheduler: checks when AutoArchive is due and runs it (or asks first) while the app is
/// idle. The shell calls it every frame.
pub fn auto_archive_tick(ctx: &egui::Context, app: &mut App) {
    archive::tick(ctx, app);
}

/// Opens a Yes/No confirmation; Yes runs `command` with `params`.
pub fn confirm(app: &mut App, title: &str, text: &str, command: &str, params: Value) {
    open(app, "confirm", json!({"title": title, "text": text, "command": command, "params": params}));
}

/// Moves `Notice::OpenDialog` notices from the session into `app.ui.dialogs` (other notices stay queued).
pub fn take_notices(app: &mut App) {
    if !app.s.notices.iter().any(|n| matches!(n, mailcraft_engine::Notice::OpenDialog(..))) {
        return;
    }
    let all = std::mem::take(&mut app.s.notices);
    for n in all {
        match n {
            mailcraft_engine::Notice::OpenDialog(name, params) => open(app, &name, params),
            other => app.s.notices.push(other),
        }
    }
}

/// A UI-only setting kept by the dialogs (Options values the engine does not hold yet, Reading Pane options
/// such as `readingPane.markWhenViewed`, junk lists, signatures…), for other areas to read.
pub fn ui_pref(ctx: &egui::Context, key: &str) -> Option<Value> {
    common::pref(ctx, key)
}

/// Gives form controls (text fields, check boxes, radio buttons, buttons) inside `ui` a visible border; the
/// shell theme draws idle widgets frameless. Used by dialogs and by item windows of other areas.
pub fn form_visuals(ui: &mut egui::Ui, theme: &crate::theme::Theme) {
    common::dialog_visuals(ui, theme);
}

/// Address (or name for local data files) shown for an account in dialogs and the backstage.
pub fn account_label(a: &mailcraft_model::Account) -> String {
    common::account_title(a)
}

/// File › Options › Search › "Highlight search terms in the results" (the profile setting `search.highlight`).
pub fn search_highlight_on(s: &mailcraft_engine::Session) -> bool {
    s.settings.bool("search.highlight")
}

/// Dialog ids with a dedicated renderer (everything the engine emits, plus the UI's own).
pub const KNOWN: &[&str] = &[
    // folders / pickers
    "moveToFolder",
    "copyToFolder",
    "archiveFolder",
    "alwaysMoveConversation",
    "selectFolder",
    "moveFolder",
    "copyFolder",
    "goToFolder",
    "createFolder",
    "renameFolder",
    "folderProperties",
    "newSearchFolder",
    "recoverDeletedItems",
    "autoArchiveSettings",
    "autoArchiveOptions",
    "archive",
    "autoArchivePrompt",
    "mainWindow",
    "confirm",
    "dataFileProperties",
    "openDataFile",
    "pickFolder",
    // categories / flags
    "colorCategories",
    "newCategory",
    "renameCategory",
    "setQuickClick",
    "customFlag",
    "addReminder",
    // rules
    "createRule",
    "rulesAndAlerts",
    "rulesWizard",
    "runRulesNow",
    "ruleCreated",
    "renameRule",
    "rulesOptions",
    "rulePrompt",
    "copyRule",
    "newItemAlerts",
    "ruleSound",
    // options
    "options",
    // general
    "about",
    "help",
    "whatsNew",
    "feedback",
    "contactSupport",
    "accountSettings",
    "addAccount",
    "testAccount",
    "confirmRemoveAccount",
    "mailboxCleanup",
    "importExport",
    "saveAs",
    "saveAttachments",
    "revealPath",
    "print",
    "pageSetup",
    "advancedFind",
    "addressBook",
    "checkNames",
    "readingPaneOptions",
    "junkOptions",
    "messageOptions",
    "viewSource",
    "saveChanges",
    "exit",
    "quickAccess",
    "ribbonDisplayOptions",
    "manageQuickSteps",
    "editQuickStep",
    "quickStepFirstUse",
    "sendReceiveProgress",
    "searchBox",
    "searchTools",
    "indexingStatus",
    "reminders",
    "stationery",
    "signatures",
    "notAvailable",
    "message",
    // S/MIME
    "digitalIds",
    "messageSecurity",
    "encryptionProblem",
    // views
    "advancedViewSettings",
    "conversationSettings",
    "zoom",
    "manageViews",
    "saveViewAs",
    "showColumns",
    "applyView",
    "quickActions",
    "viewSort",
    "viewGroupBy",
    "viewFilter",
    "viewOtherSettings",
    "viewFormat",
    // compose
    "font",
    "colors",
    "bordersAndShading",
    "setNumberingValue",
    "defineNewBullet",
    "defineNewNumberFormat",
    "defineNewMultilevelList",
    "defineNewListStyle",
    "paragraph",
    "saveStyleSet",
    "applyStyles",
    "pasteSpecial",
    "find",
    "replace",
    "goTo",
    "insertPicture",
    "icons",
    "insertChart",
    "screenshot",
    "drawTable",
    "quickTables",
    "insertTable",
    "drawTextBox",
    "insertLink",
    "bookmark",
    "symbol",
    "dateTime",
    "quickParts",
    "insertBusinessCard",
    "insertCalendar",
    "insertObject",
    "attachFile",
    "attachItem",
    "browseThemes",
    "saveTheme",
    "customizeTheme",
    "themes",
    "pageColor",
    "equation",
    "smartArt",
    "language",
    "thesaurus",
    "translate",
    "wordCount",
    "accessibility",
    "readAloud",
    "searchRefine",
    // items
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
    "closeAllItems",
    "chooseForm",
    "forwardContact",
    "goToDate",
    "timeZone",
    "addCalendar",
    "contactCard",
    "chooseContact",
];

fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    if d.name == "reminders" {
        // The Reminders window is the modules area's own window (it also pops up when reminders come due).
        crate::modules::open_reminders(ctx, app);
        return Out::Close;
    }
    if let Some(out) = folders::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = archive::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = categories::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = quicksteps::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = alerts::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = rules::show(ctx, app, d) {
        return out;
    }
    if d.name == "options" {
        return options::show(ctx, app, d);
    }
    if let Some(out) = accounts::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = general::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = print::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = files::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = find::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = junk::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = mailopts::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = views::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = compose_dlg::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = security::show(ctx, app, d) {
        return out;
    }
    if let Some(out) = items::show(ctx, app, d) {
        return out;
    }
    general::not_available(ctx, app, d)
}

// ---------------------------------------------------------------------------------------------------------

const UID_KEY: &str = "_dlg";

fn uid_of(p: &Value) -> u64 {
    p.get(UID_KEY).and_then(Value::as_u64).unwrap_or(0)
}

fn strip_uid(p: &Value) -> Value {
    let mut v = p.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove(UID_KEY);
    }
    v
}

fn assign_uids(ctx: &Context, app: &mut App) {
    let counter = Id::new("mc-dialog-next-uid");
    for (_, p) in &mut app.ui.dialogs {
        if uid_of(p) != 0 {
            continue;
        }
        if !p.is_object() {
            let old = std::mem::take(p);
            *p = if old.is_null() { json!({}) } else { json!({ "value": old }) };
        }
        let uid = ctx.data_mut(|d| {
            let n = d.get_temp_mut_or_insert_with(counter, || 0u64);
            *n = n.saturating_add(1);
            *n
        });
        if let Some(o) = p.as_object_mut() {
            o.insert(UID_KEY.into(), json!(uid));
        }
    }
}
