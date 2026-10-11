//! Junk Email Options (`junkOptions`, Home › Delete › Block › Junk E-mail Options…): tabs Options, Safe
//! Senders, Safe Recipients, Blocked Senders and International (Blocked Top-Level Domains, Blocked
//! Encodings). The dialog edits a copy of the session's [`JunkSettings`]; OK and Apply send it with
//! `mail.junk {"action":"set"}`. Import from File… runs `mail.junk import` (undoable) and merges the result;
//! Export to File… applies the current list first, then runs `mail.junk export`.
//!
//! Wording is our own; only the feature names come from the observed client.

use egui::{Context, Key, ScrollArea, Ui};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, muted, primary, window, with_state};
use crate::{App, FileFilter};
use mailcraft_model::junk::{COMMON_ENCODINGS, COMMON_TLDS, normalize_entry};
use mailcraft_model::{JunkLevel, JunkList, JunkSettings};

/// Tabs, in order.
pub(crate) const TABS: [&str; 5] =
    ["Options", "Safe Senders", "Safe Recipients", "Blocked Senders", "International"];

/// Level choices: (level, name, our description).
const LEVELS: [(JunkLevel, &str, &str); 4] = [
    (
        JunkLevel::NoFiltering,
        "No Automatic Filtering",
        "Only mail from blocked senders and domains goes to Junk Email.",
    ),
    (JunkLevel::Low, "Low", "Catch only the most obvious junk."),
    (
        JunkLevel::High,
        "High",
        "Catch most junk. Some wanted mail may be caught too, so check Junk Email often.",
    ),
    (
        JunkLevel::SafeListsOnly,
        "Safe Lists Only",
        "Only mail from your Safe Senders and Safe Recipients (and Contacts, when trusted) stays in the Inbox.",
    ),
];

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    (d.name == "junkOptions").then(|| junk_options(ctx, app, d))
}

#[derive(Clone, Default)]
struct State {
    tab: usize,
    settings: JunkSettings,
    /// Text in the add/edit field of the list tabs (and the International tab's custom field).
    entry: String,
    selected: Option<usize>,
    /// Last message shown under the list (validation, import result).
    note: String,
    show_tlds: bool,
    show_encodings: bool,
}

/// The address list shown on tab `tab`.
fn tab_list(tab: usize) -> Option<JunkList> {
    match tab {
        1 => Some(JunkList::SafeSenders),
        2 => Some(JunkList::SafeRecipients),
        3 => Some(JunkList::BlockedSenders),
        _ => None,
    }
}

fn settings_json(st: &State) -> Value {
    json!({"action": "set", "settings": serde_json::to_value(&st.settings).unwrap_or_else(|_| json!({}))})
}

fn junk_options(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let tab = d.u64("tab").and_then(|t| usize::try_from(t).ok()).filter(|t| *t < TABS.len()).unwrap_or(0);
    let init = |app: &App| State { tab, settings: app.s.mailbox.junk.clone(), ..Default::default() };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Junk Email Options", 560.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in TABS.iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() && st.tab != i {
                        st.tab = i;
                        st.selected = None;
                        st.entry.clear();
                        st.note.clear();
                    }
                }
            });
            ui.separator();
            let mut entry_focused = false;
            match st.tab {
                0 => options_tab(ui, app, st),
                1..=3 => entry_focused = list_tab(ui, app, st),
                _ => entry_focused = international_tab(ui, app, st),
            }
            footer(ui, app, st, entry_focused)
        })
    })
}

/// OK / Cancel / Apply. Enter means OK unless a text field has focus (there it adds the entry).
fn footer(ui: &mut Ui, app: &mut App, st: &mut State, entry_focused: bool) -> Out {
    let enter = !entry_focused && common::enter_pressed(ui);
    let mut out = Out::Keep;
    common::footer(ui, |ui| {
        if btn(ui, "Apply") {
            app.run("mail.junk", settings_json(st));
            st.settings = app.s.mailbox.junk.clone();
        }
        if btn(ui, "Cancel") {
            out = Out::Close;
        }
        if primary(ui, app, true, "OK") || enter {
            out = if app.run("mail.junk", settings_json(st)).is_some() { Out::Close } else { Out::Keep };
        }
    });
    out
}

fn options_tab(ui: &mut Ui, app: &App, st: &mut State) {
    muted(ui, app, "Messages that look like junk can be moved to the Junk Email folder automatically.");
    ui.add_space(4.0);
    ui.label("Level of junk email protection:");
    for (level, name, text) in LEVELS {
        ui.radio_value(&mut st.settings.level, level, format!("{name}: {text}"));
    }
    ui.add_space(6.0);
    ui.checkbox(
        &mut st.settings.delete_suspected,
        "Permanently delete suspected junk email instead of moving it to the Junk Email folder",
    );
    ui.checkbox(
        &mut st.settings.disable_links,
        "Disable links and other functionality in junk and phishing messages",
    );
    ui.checkbox(
        &mut st.settings.warn_suspicious_domains,
        "Warn me about suspicious domain names in email addresses",
    );
}

/// A list tab; returns whether the entry field has focus.
fn list_tab(ui: &mut Ui, app: &mut App, st: &mut State) -> bool {
    let Some(list) = tab_list(st.tab) else { return false };
    match list {
        JunkList::SafeSenders => {
            muted(ui, app, "Mail from these addresses or domains is never treated as junk.");
            ui.checkbox(&mut st.settings.trust_contacts, "Also trust email from my Contacts");
            ui.checkbox(
                &mut st.settings.add_recipients_to_safe,
                "Automatically add people I email to the Safe Senders List",
            );
        }
        JunkList::SafeRecipients => {
            muted(
                ui,
                app,
                "Mail sent to these addresses or domains (groups, mailing lists) is never treated as junk.",
            );
        }
        _ => muted(ui, app, "Mail from these addresses or domains always goes to the Junk Email folder."),
    }
    ui.add_space(4.0);
    let mut focused = false;
    ui.horizontal_top(|ui| {
        entries_box(ui, app, st, list);
        ui.vertical(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.entry)
                    .hint_text("name@example.com or @example.com")
                    .desired_width(180.0),
            );
            focused = r.has_focus();
            let submitted = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if btn_en(ui, !st.entry.trim().is_empty(), "Add...") || (submitted && !st.entry.trim().is_empty())
            {
                add_entry(st, list);
                focused = true;
            }
            if btn_en(ui, st.selected.is_some() && !st.entry.trim().is_empty(), "Edit...") {
                edit_entry(st, list);
            }
            if btn_en(ui, st.selected.is_some(), "Remove")
                && let Some(i) = st.selected.take()
                && i < st.settings.list(list).len()
            {
                st.settings.list_mut(list).remove(i);
            }
            ui.add_space(8.0);
            if btn(ui, "Import from File...") {
                import(app, st, list);
            }
            if btn(ui, "Export to File...") {
                export(app, st, list);
            }
        });
    });
    if !st.note.is_empty() {
        muted(ui, app, &st.note.clone());
    }
    focused
}

/// The bordered, scrollable list of entries (click selects and copies into the field for editing).
fn entries_box(ui: &mut Ui, app: &App, st: &mut State, list: JunkList) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        ui.set_width(300.0);
        ui.set_min_height(220.0);
        ScrollArea::vertical().max_height(220.0).id_salt(("junk-list", list.key())).show(ui, |ui| {
            let entries = st.settings.list(list).clone();
            if entries.is_empty() {
                muted(ui, app, "(empty)");
            }
            for (i, e) in entries.iter().enumerate() {
                if ui.selectable_label(st.selected == Some(i), e).clicked() {
                    st.selected = Some(i);
                    st.entry = e.clone();
                }
            }
        });
    });
}

fn add_entry(st: &mut State, list: JunkList) {
    let text = st.entry.trim().to_owned();
    match st.settings.add(list, &text) {
        Ok(true) => {
            st.note.clear();
            st.entry.clear();
            st.selected = None;
        }
        Ok(false) => st.note = format!("{text} is already on the list."),
        Err(e) => st.note = e.to_string(),
    }
}

fn edit_entry(st: &mut State, list: JunkList) {
    let Some(i) = st.selected else { return };
    let Some(old) = st.settings.list(list).get(i).cloned() else { return };
    if normalize_entry(list, &st.entry).is_none() {
        st.note = format!("\"{}\" is not a valid address or domain.", st.entry.trim());
        return;
    }
    st.settings.remove(list, &old);
    if let Err(e) = st.settings.add(list, st.entry.trim()) {
        st.note = e.to_string();
        let _ = st.settings.add(list, &old);
        return;
    }
    st.note.clear();
    st.entry.clear();
    st.selected = None;
}

fn txt_filters() -> Vec<FileFilter> {
    vec![FileFilter::new("Text Files", &["txt"])]
}

fn import(app: &mut App, st: &mut State, list: JunkList) {
    let Some(path) = common::browse_open(app, "Import from File", &txt_filters(), "") else {
        if app.files.is_none() {
            st.note = "No file dialog is available here.".into();
        }
        return;
    };
    import_path(app, st, list, &path);
}

/// Imports `path` into `list` through the engine and merges the session's list into the dialog's copy.
fn import_path(app: &mut App, st: &mut State, list: JunkList, path: &str) {
    match app.run("mail.junk", json!({"action": "import", "list": list.key(), "path": path})) {
        Some(v) => {
            for e in app.s.mailbox.junk.list(list).clone() {
                let _ = st.settings.add(list, &e);
            }
            st.note =
                format!("Imported {} new entries.", v.get("added").and_then(Value::as_u64).unwrap_or(0));
        }
        None => st.note = "The file could not be imported.".into(),
    }
}

fn export(app: &mut App, st: &mut State, list: JunkList) {
    let Some(path) = common::browse_save(app, "Export to File", &txt_filters(), "junk-list.txt") else {
        if app.files.is_none() {
            st.note = "No file dialog is available here.".into();
        }
        return;
    };
    export_path(app, st, list, &path);
}

/// Applies the dialog's copy of `list`, then exports it to `path`.
fn export_path(app: &mut App, st: &mut State, list: JunkList, path: &str) {
    let entries = st.settings.list(list).clone();
    let mut set = serde_json::Map::new();
    set.insert(list.key().into(), json!(entries));
    app.run("mail.junk", json!({"action": "set", "settings": Value::Object(set)}));
    st.note = match app.run("mail.junk", json!({"action": "export", "list": list.key(), "path": path})) {
        Some(v) => format!("Exported {} entries.", v.get("exported").and_then(Value::as_u64).unwrap_or(0)),
        None => "The list could not be exported.".into(),
    };
}

/// International tab; returns whether its entry field has focus.
fn international_tab(ui: &mut Ui, app: &App, st: &mut State) -> bool {
    muted(
        ui,
        app,
        "Treat mail as junk when the sender's address ends in a blocked top-level domain, or when the message uses a blocked encoding.",
    );
    ui.add_space(4.0);
    let mut focused = false;
    let n_tld = st.settings.blocked_tlds.len();
    if ui.button(format!("Blocked Top-Level Domain List... ({n_tld} blocked)")).clicked() {
        st.show_tlds = !st.show_tlds;
    }
    if st.show_tlds {
        egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(
            ui,
            |ui| {
                ScrollArea::vertical().max_height(150.0).id_salt("junk-tlds").show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let mut all: Vec<String> = COMMON_TLDS.iter().map(|t| (*t).to_owned()).collect();
                        all.extend(st.settings.blocked_tlds.iter().cloned());
                        all.sort();
                        all.dedup();
                        for t in all {
                            let mut on = st.settings.blocked_tlds.contains(&t);
                            if ui.checkbox(&mut on, format!(".{}", t.to_uppercase())).changed() {
                                toggle(&mut st.settings, JunkList::BlockedTlds, &t, on);
                            }
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if btn(ui, "Select All") {
                        for t in COMMON_TLDS {
                            let _ = st.settings.add(JunkList::BlockedTlds, t);
                        }
                    }
                    if btn(ui, "Clear All") {
                        st.settings.blocked_tlds.clear();
                    }
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.entry)
                            .hint_text("other, e.g. xyz")
                            .desired_width(110.0),
                    );
                    focused |= r.has_focus();
                    let submitted = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if btn_en(ui, !st.entry.trim().is_empty(), "Add")
                        || (submitted && !st.entry.trim().is_empty())
                    {
                        add_entry(st, JunkList::BlockedTlds);
                    }
                });
            },
        );
    }
    ui.add_space(6.0);
    let n_enc = st.settings.blocked_encodings.len();
    if ui.button(format!("Blocked Encodings List... ({n_enc} blocked)")).clicked() {
        st.show_encodings = !st.show_encodings;
    }
    if st.show_encodings {
        egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(
            ui,
            |ui| {
                ScrollArea::vertical().max_height(170.0).id_salt("junk-encodings").show(ui, |ui| {
                    for (cs, label) in COMMON_ENCODINGS {
                        let mut on = st.settings.blocked_encodings.iter().any(|e| e == cs);
                        if ui.checkbox(&mut on, format!("{label}  [{cs}]")).changed() {
                            toggle(&mut st.settings, JunkList::BlockedEncodings, cs, on);
                        }
                    }
                    let custom: Vec<String> = st
                        .settings
                        .blocked_encodings
                        .iter()
                        .filter(|e| !COMMON_ENCODINGS.iter().any(|(c, _)| c == e))
                        .cloned()
                        .collect();
                    for cs in custom {
                        let mut on = true;
                        if ui.checkbox(&mut on, cs.as_str()).changed() {
                            toggle(&mut st.settings, JunkList::BlockedEncodings, &cs, on);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if btn(ui, "Clear All") {
                        st.settings.blocked_encodings.clear();
                    }
                });
            },
        );
    }
    if !st.note.is_empty() {
        muted(ui, app, &st.note.clone());
    }
    focused
}

fn toggle(s: &mut JunkSettings, list: JunkList, entry: &str, on: bool) {
    if on {
        let _ = s.add(list, entry);
    } else {
        s.remove(list, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;

    fn harness(app: App) -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::dialogs::dialogs(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        h
    }

    fn open(tab: usize) -> Harness<'static, App> {
        let mut app = App::new(sample_session());
        crate::dialogs::open(&mut app, "junkOptions", json!({"tab": tab}));
        harness(app)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let base = std::env::current_exe()
            .ok()
            .and_then(|e| e.ancestors().nth(3).map(|t| t.join("tmp")))
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
        let dir = base.join(format!("mc-junk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn options_tab_sets_level_and_flags_on_ok() {
        let mut h = open(0);
        h.get_by_label_contains("High:").click();
        h.run_steps(2);
        h.get_by_label_contains("Permanently delete").click();
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        let j = &h.state().s.mailbox.junk;
        assert_eq!(j.level, JunkLevel::High);
        assert!(j.delete_suspected);
        assert!(h.state().ui.dialogs.is_empty());
    }

    #[test]
    fn cancel_keeps_settings() {
        let mut h = open(0);
        h.get_by_label_contains("Safe Lists Only:").click();
        h.run_steps(2);
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert_eq!(h.state().s.mailbox.junk.level, JunkLevel::Low);
    }

    #[test]
    fn blocked_senders_add_remove_and_ok() {
        let mut h = open(3);
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run_steps(1);
        h.get_by_role(egui::accesskit::Role::TextInput).type_text("Spam@Example.NET");
        h.run_steps(2);
        h.get_by_label("Add...").click();
        h.run_steps(2);
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run_steps(1);
        h.get_by_role(egui::accesskit::Role::TextInput).type_text("not valid");
        h.run_steps(2);
        h.get_by_label("Add...").click();
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        assert_eq!(h.state().s.mailbox.junk.blocked_senders, vec!["spam@example.net"]);
    }

    #[test]
    fn international_tab_blocks_tld_and_encoding() {
        let mut h = open(4);
        h.get_by_label_contains("Blocked Top-Level Domain List").click();
        h.run_steps(2);
        h.get_by_label(".RU").click();
        h.run_steps(2);
        h.get_by_label_contains("Blocked Encodings List").click();
        h.run_steps(2);
        h.get_by_label_contains("[iso-8859-6]").click();
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        let j = &h.state().s.mailbox.junk;
        assert_eq!(j.blocked_tlds, vec!["ru"]);
        assert_eq!(j.blocked_encodings, vec!["iso-8859-6"]);
    }

    #[test]
    fn import_and_export_round_trip() {
        let dir = scratch("io");
        let src = dir.join("in.txt");
        std::fs::write(&src, "a@example.com\r\n@example.org\r\nbad entry\r\n").unwrap();
        let mut app = App::new(sample_session());
        let mut st = State { settings: app.s.mailbox.junk.clone(), ..Default::default() };
        import_path(&mut app, &mut st, JunkList::SafeSenders, &src.display().to_string());
        assert_eq!(st.settings.safe_senders, vec!["@example.org", "a@example.com"]);
        assert!(st.note.contains('2'), "{}", st.note);
        st.settings.add(JunkList::SafeSenders, "z@example.net").unwrap();
        let out = dir.join("out.txt");
        export_path(&mut app, &mut st, JunkList::SafeSenders, &out.display().to_string());
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text, "@example.org\r\na@example.com\r\nz@example.net\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_tab_renders_without_panicking() {
        for tab in 0..TABS.len() {
            let mut h = open(tab);
            h.run_steps(2);
            assert_eq!(h.state().ui.dialogs.len(), 1);
        }
    }
}
