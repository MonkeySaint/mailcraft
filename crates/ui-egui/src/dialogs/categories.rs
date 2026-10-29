//! Color Categories (master list with checkboxes; New / Rename / Delete / Color / Shortcut Key), New and
//! Rename Category, Set Quick Click, and the Custom flag dialog (Flag to, Start/Due date, Reminder; for a
//! message being written also Flag for Me / Flag for Recipients).

use egui::{Align, Color32, Context, Layout, RichText, Sense, Ui, Vec2};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, btn, btn_en, child, footer, muted, ok_cancel, send_reply, take_replies, window,
    with_state,
};
use super::folders::last_error;
use crate::App;
use mailcraft_model::{CategoryColor, FlagState, ItemId};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "colorCategories" => color_categories(ctx, app, d),
        "newCategory" | "renameCategory" => edit_category(ctx, app, d),
        "setQuickClick" => quick_click(ctx, app, d),
        "customFlag" | "addReminder" => custom_flag(ctx, app, d),
        _ => return None,
    })
}

const SHORTCUTS: [&str; 12] = [
    "(None)", "Ctrl+F2", "Ctrl+F3", "Ctrl+F4", "Ctrl+F5", "Ctrl+F6", "Ctrl+F7", "Ctrl+F8", "Ctrl+F9",
    "Ctrl+F10", "Ctrl+F11", "Ctrl+F12",
];

pub(crate) fn swatch_color(c: CategoryColor) -> Color32 {
    let rgb = c.rgb();
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

fn swatch(ui: &mut egui::Ui, app: &App, c: CategoryColor) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(16.0, 14.0), Sense::hover());
    if c == CategoryColor::None {
        ui.painter().rect_stroke(
            r,
            2.0,
            egui::Stroke::new(1.0, app.theme.text_muted),
            egui::StrokeKind::Inside,
        );
    } else {
        ui.painter().rect_filled(r, 2.0, swatch_color(c));
    }
}

fn color_combo(
    ui: &mut egui::Ui,
    app: &App,
    id: impl std::hash::Hash + std::fmt::Debug,
    cur: CategoryColor,
) -> Option<CategoryColor> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id).selected_text(cur.name()).width(130.0).show_ui(ui, |ui| {
        for c in CategoryColor::ALL {
            ui.horizontal(|ui| {
                swatch(ui, app, c);
                if ui.selectable_label(c == cur, c.name()).clicked() {
                    picked = Some(c);
                }
            });
        }
    });
    picked
}

#[derive(Clone, Default)]
struct CatState {
    /// Desired checkbox state per category (lower-case name → on).
    checks: Vec<(String, bool)>,
    /// Initial state per category for the targets: Some(true) all have it, Some(false) none, None mixed.
    initial: Vec<(String, Option<bool>)>,
    selected: Option<String>,
}

/// Categories of the target items: messages, or (with `area` people / tasks / notes / calendar) the
/// module's items.
fn item_categories<'a>(app: &'a App, area: &str, ids: &[u64]) -> Vec<&'a Vec<String>> {
    use mailcraft_engine::cmd::calendar::pim;
    ids.iter()
        .filter_map(|i| match area {
            "" | "mail" => app.s.mailbox.mail.get(ItemId(*i)).map(|m| &m.categories),
            _ => pim::categories(&app.s.mailbox, ItemId(*i)),
        })
        .collect()
}

fn initial_marks(app: &App, area: &str, ids: &[u64]) -> Vec<(String, Option<bool>)> {
    let items = item_categories(app, area, ids);
    app.s
        .mailbox
        .categories
        .iter()
        .map(|c| {
            let n = items.iter().filter(|m| m.iter().any(|x| x.eq_ignore_ascii_case(&c.name))).count();
            let mark = if items.is_empty() || n == 0 {
                Some(false)
            } else if n == items.len() {
                Some(true)
            } else {
                None
            };
            (c.name.to_lowercase(), mark)
        })
        .collect()
}

/// The master category list sorted by name (the Color Categories dialog lists it alphabetically).
fn sorted_categories(app: &App) -> Vec<mailcraft_model::Category> {
    let mut cats = app.s.mailbox.categories.clone();
    cats.sort_by_key(|c| c.name.to_lowercase());
    cats
}

fn color_categories(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let ids = d.ids();
    let area = d.str("area");
    let init = |app: &App| {
        let initial = initial_marks(app, &area, &ids);
        let checks = initial.iter().map(|(n, m)| (n.clone(), m.unwrap_or(false) || m.is_none())).collect();
        CatState { checks, initial, selected: sorted_categories(app).first().map(|c| c.name.clone()) }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if let (true, Some(name)) = (k == "created", v.as_str()) {
                st.checks.push((name.to_lowercase(), !ids.is_empty()));
                st.selected = Some(name.to_owned());
            }
            if let (true, Some(name)) = (k == "renamed", v.as_str()) {
                st.selected = Some(name.to_owned());
            }
        }
        window(ctx, app, d, "Color Categories", 500.0, |ui, app| {
            muted(ui, app, "Tick categories to assign them to the selected items. Select a name to edit it.");
            let mut out = Out::Keep;
            ui.horizontal_top(|ui| {
                let list_w = 290.0;
                ui.vertical(|ui| {
                    ui.set_width(list_w);
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
                        // Column headings (Name, Shortcut key) over the alphabetical list, as observed.
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.label(RichText::new("Name").color(app.theme.text_muted).small());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(4.0);
                                ui.label(RichText::new("Shortcut key").color(app.theme.text_muted).small());
                            });
                        });
                        egui::ScrollArea::vertical().id_salt(("cats", uid)).max_height(222.0).min_scrolled_height(222.0).auto_shrink([false, false]).show(ui, |ui| {
                            let cats = sorted_categories(app);
                            for c in &cats {
                                let key = c.name.to_lowercase();
                                ui.horizontal(|ui| {
                                    let mut on = st.checks.iter().find(|(n, _)| *n == key).is_some_and(|(_, v)| *v);
                                    if ui.add_enabled(!ids.is_empty(), egui::Checkbox::without_text(&mut on)).changed() {
                                        match st.checks.iter_mut().find(|(n, _)| *n == key) {
                                            Some(e) => e.1 = on,
                                            None => st.checks.push((key.clone(), on)),
                                        }
                                    }
                                    swatch(ui, app, c.color);
                                    let sel = st.selected.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&c.name));
                                    if ui.selectable_label(sel, &c.name).clicked() {
                                        st.selected = Some(c.name.clone());
                                    }
                                    if !c.shortcut.is_empty() {
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            ui.add_space(4.0);
                                            ui.label(RichText::new(&c.shortcut).color(app.theme.text_muted));
                                        });
                                    }
                                });
                            }
                        });
                    });
                });
                ui.vertical(|ui| {
                    if btn(ui, "New...") {
                        out = Out::open("newCategory", child(uid, "created", json!({})));
                    }
                    let sel = st.selected.clone().and_then(|n| {
                        app.s.mailbox.categories.iter().find(|c| c.name.eq_ignore_ascii_case(&n)).cloned()
                    });
                    if btn_en(ui, sel.is_some(), "Rename") && let Some(c) = &sel {
                        out = Out::open("renameCategory", child(uid, "renamed", json!({"name": c.name})));
                    }
                    if btn_en(ui, sel.is_some(), "Delete") && let Some(c) = &sel {
                        out = Out::open(
                            "confirm",
                            json!({
                                "title": "Delete Category",
                                "text": format!("Delete the \"{}\" category? It is removed from the list but kept on items that already have it.", c.name),
                                "command": "categories.delete",
                                "params": {"name": c.name},
                            }),
                        );
                    }
                    ui.add_space(12.0);
                    ui.label("Color:");
                    if let Some(c) = &sel && let Some(col) = color_combo(ui, app, ("catcolor", uid), c.color) {
                        app.run("categories.color", json!({"name": c.name, "color": col.name()}));
                    }
                    ui.add_space(6.0);
                    ui.label("Shortcut Key:");
                    if let Some(c) = &sel {
                        let mut cur = if c.shortcut.is_empty() { SHORTCUTS[0].to_owned() } else { c.shortcut.clone() };
                        if common::combo(ui, ("catkey", uid), &mut cur, &SHORTCUTS, 120.0) {
                            let v = if cur == SHORTCUTS[0] { String::new() } else { cur };
                            app.run("categories.shortcut", json!({"name": c.name, "shortcut": v}));
                        }
                    }
                });
            });
            if !matches!(out, Out::Keep) {
                return out;
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    apply_checks(app, &area, &ids, st);
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Runs `mail.categorize` (a toggle) so each category ends up on or off on every target; module items
/// (`area`) take `<area>.categorize` with `on`.
fn apply_checks(app: &mut App, area: &str, ids: &[u64], st: &CatState) {
    if ids.is_empty() {
        return;
    }
    let cats: Vec<String> = app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
    if !matches!(area, "" | "mail") {
        let cmd = format!("{area}.categorize");
        for name in cats {
            let key = name.to_lowercase();
            let want = st.checks.iter().find(|(n, _)| *n == key).is_some_and(|(_, v)| *v);
            // A category made in this dialog was on no target (`None` below means mixed).
            let was = st.initial.iter().find(|(n, _)| *n == key).map_or(Some(false), |(_, m)| *m);
            // Untouched mixed rows stay mixed; rows already as wanted are left alone.
            if (was.is_none() && want) || was == Some(want) {
                continue;
            }
            app.run(&cmd, json!({"name": name, "on": want, "ids": ids}));
        }
        return;
    }
    let now = initial_marks(app, area, ids);
    for name in cats {
        let key = name.to_lowercase();
        let want = st.checks.iter().find(|(n, _)| *n == key).is_some_and(|(_, v)| *v);
        let was = st.initial.iter().find(|(n, _)| *n == key).map(|(_, m)| *m);
        let cur = now.iter().find(|(n, _)| *n == key).and_then(|(_, m)| *m);
        // Untouched mixed rows stay mixed.
        if was == Some(None) && want {
            continue;
        }
        match (cur, want) {
            (Some(true), true) | (Some(false), false) => {}
            (Some(false), true) | (Some(true), false) => {
                app.run("mail.categorize", json!({"name": name, "ids": ids}));
            }
            (None, true) => {
                app.run("mail.categorize", json!({"name": name, "ids": ids}));
            }
            (None, false) => {
                // Mixed → the toggle first adds everywhere, then removes everywhere.
                app.run("mail.categorize", json!({"name": name, "ids": ids}));
                app.run("mail.categorize", json!({"name": name, "ids": ids}));
            }
        }
    }
}

#[derive(Clone, Default)]
struct EditCat {
    name: String,
    color: usize,
    shortcut: String,
    error: String,
    focused: bool,
}

fn edit_category(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let rename = d.name == "renameCategory";
    let old = d.str("name");
    let init = |app: &App| EditCat {
        name: old.clone(),
        color: if rename { 0 } else { 1 + (app.s.mailbox.categories.len() % (CategoryColor::ALL.len() - 1)) },
        shortcut: SHORTCUTS[0].to_owned(),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        let title = if rename { "Rename Category" } else { "Add New Category" };
        window(ctx, app, d, title, 340.0, |ui, app| {
            ui.horizontal(|ui| {
                ui.label("Name:");
                let r = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(240.0));
                if !st.focused {
                    r.request_focus();
                    st.focused = true;
                }
            });
            if !rename {
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let cur = CategoryColor::ALL.get(st.color).copied().unwrap_or_default();
                    swatch(ui, app, cur);
                    if let Some(c) = color_combo(ui, app, ("newcolor", uid), cur) {
                        st.color = CategoryColor::ALL.iter().position(|x| *x == c).unwrap_or(0);
                    }
                    ui.label("Shortcut Key:");
                    common::combo(ui, ("newkey", uid), &mut st.shortcut, &SHORTCUTS, 100.0);
                });
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, !st.name.trim().is_empty()) {
                Some(true) => {
                    let name = st.name.trim().to_owned();
                    let res = if rename {
                        if name == old {
                            return Out::Close;
                        }
                        app.run("categories.rename", json!({"name": old, "newName": name}))
                    } else {
                        let color = CategoryColor::ALL.get(st.color).copied().unwrap_or_default().name();
                        let shortcut =
                            if st.shortcut == SHORTCUTS[0] { String::new() } else { st.shortcut.clone() };
                        app.run("categories.new", json!({"name": name, "color": color, "shortcut": shortcut}))
                    };
                    if res.is_some() {
                        if let Some((to, key)) = d.reply_to() {
                            send_reply(ctx, to, &key, json!(name));
                        }
                        Out::Close
                    } else {
                        st.error = last_error(app);
                        Out::Keep
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

const FLAG_KINDS: [(&str, &str); 6] = [
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("thisWeek", "This Week"),
    ("nextWeek", "Next Week"),
    ("noDate", "No Date"),
    ("complete", "Mark Complete"),
];

#[derive(Clone, Default)]
struct QuickState {
    pick: String,
}

fn quick_click(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let for_flag = d.str("for") == "flag";
    let names: Vec<String> = if for_flag {
        FLAG_KINDS.iter().map(|(_, l)| (*l).to_owned()).collect()
    } else {
        app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect()
    };
    // The current Quick Click flag / category comes from the profile settings (the commands store it).
    let saved = if for_flag {
        let kind = app.s.settings.text("tasks.quickClickFlag");
        FLAG_KINDS.iter().find(|(k, _)| *k == kind).map(|(_, l)| (*l).to_owned())
    } else {
        mailcraft_engine::cmd::categories::quick_click_category(&app.s)
    };
    let init =
        |_: &App| QuickState { pick: saved.clone().or_else(|| names.first().cloned()).unwrap_or_default() };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Set Quick Click", 340.0, |ui, app| {
            ui.label(if for_flag {
                "Choose the flag a single click in the flag column sets:"
            } else {
                "Choose the category a single click in the category column sets:"
            });
            let choices: Vec<&str> = names.iter().map(String::as_str).collect();
            common::combo(ui, ("qc", uid), &mut st.pick, &choices, 260.0);
            match ok_cancel(ui, app, !st.pick.is_empty()) {
                Some(true) => {
                    let res = if for_flag {
                        let kind =
                            FLAG_KINDS.iter().find(|(_, l)| *l == st.pick).map_or("today", |(k, _)| *k);
                        app.run("mail.setQuickClickFlag", json!({"kind": kind}))
                    } else {
                        app.run("categories.setQuickClick", json!({"name": st.pick}))
                    };
                    if res.is_none() {
                        return Out::Keep;
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

const FLAG_TO: [&str; 10] = [
    "Call",
    "Do not Forward",
    "Follow up",
    "For Your Information",
    "Forward",
    "No Response Necessary",
    "Read",
    "Reply",
    "Reply to All",
    "Review",
];

#[derive(Clone, Default)]
struct FlagDlg {
    /// Flag for Me (a message being written can be flagged for its recipients only).
    flag_me: bool,
    label: String,
    start: String,
    due: String,
    reminder: bool,
    rdate: String,
    rtime: String,
    /// Flag for Recipients (messages being written only).
    rec_on: bool,
    rec_label: String,
    rec_due: String,
    rec_reminder: bool,
    rec_rdate: String,
    rec_rtime: String,
    error: String,
}

/// The "Flag to" box with its menu of standard labels.
fn flag_to(ui: &mut Ui, id: (&'static str, u64), label: &mut String) {
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(label).desired_width(180.0));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Flag to"));
        egui::ComboBox::from_id_salt(id).selected_text("").width(24.0).show_ui(ui, |ui| {
            for f in FLAG_TO {
                if ui.selectable_label(label == f, f).clicked() {
                    *label = f.to_owned();
                }
            }
        });
    });
}

fn custom_flag(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let ids = d.ids();
    let init = |app: &App| {
        let mut st = FlagDlg {
            flag_me: true,
            label: "Follow up".into(),
            reminder: d.bool("reminder") || d.name == "addReminder",
            rtime: "16:00".into(),
            rec_label: "Follow up".into(),
            rec_rtime: "16:00".into(),
            ..Default::default()
        };
        let flag = match compose {
            Some(c) => app.s.composes.get(&c).map(|c| c.flag.clone()),
            None => ids.first().and_then(|i| app.s.mailbox.mail.get(ItemId(*i))).map(|m| m.flag.clone()),
        };
        if let Some(FlagState::Flagged { label, start, due, reminder }) = flag {
            st.label = label;
            st.start = start.map(|t| common::date_parts(app, t.unix).0).unwrap_or_default();
            st.due = due.map(|t| common::date_parts(app, t.unix).0).unwrap_or_default();
            if let Some(r) = reminder {
                st.reminder = true;
                let (dd, tt) = common::date_parts(app, r.unix);
                st.rdate = dd;
                st.rtime = tt;
            }
        } else if let Some(c) = compose.and_then(|c| app.s.composes.get(&c))
            && c.recipient_flag.is_some()
        {
            // Only the recipients are flagged.
            st.flag_me = false;
        }
        if let Some(r) = compose.and_then(|c| app.s.composes.get(&c)).and_then(|c| c.recipient_flag.clone()) {
            st.rec_on = true;
            st.rec_label = r.label;
            st.rec_due = r.due.map(|t| common::date_parts(app, t.unix).0).unwrap_or_default();
            if let Some(at) = r.reminder {
                st.rec_reminder = true;
                let (dd, tt) = common::date_parts(app, at.unix);
                st.rec_rdate = dd;
                st.rec_rtime = tt;
            }
        }
        if st.rdate.is_empty() {
            st.rdate = common::today(app);
        }
        if st.rec_rdate.is_empty() {
            st.rec_rdate = common::today(app);
        }
        st
    };
    // Clear Flag is offered only when there is a flag to clear.
    let flagged = match compose {
        Some(c) => {
            app.s.composes.get(&c).is_some_and(|c| c.flag != FlagState::None || c.recipient_flag.is_some())
        }
        None => {
            ids.iter().any(|i| app.s.mailbox.mail.get(ItemId(*i)).is_some_and(|m| m.flag != FlagState::None))
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Custom", if compose.is_some() { 440.0 } else { 400.0 }, |ui, app| {
            muted(ui, app, "A flag adds the item to your to-do list until you mark it complete.");
            if compose.is_some() {
                ui.checkbox(&mut st.flag_me, "Flag for Me");
            }
            ui.add_enabled_ui(st.flag_me, |ui| {
                egui::Grid::new(("flag", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Flag to");
                    flag_to(ui, ("flagto", uid), &mut st.label);
                    ui.end_row();
                    ui.label("Start date");
                    common::date_box(ui, app, ("fstart", uid), &mut st.start, "None", "Start date");
                    ui.end_row();
                    ui.label("Due date");
                    common::date_box(ui, app, ("fdue", uid), &mut st.due, "None", "Due date");
                    ui.end_row();
                    ui.checkbox(&mut st.reminder, "Reminder");
                    ui.add_enabled_ui(st.reminder, |ui| {
                        ui.horizontal(|ui| {
                            common::date_box(ui, app, ("frdate", uid), &mut st.rdate, "", "Reminder date");
                            common::time_box(ui, ("frtime", uid), &mut st.rtime, "Reminder time");
                        });
                    });
                    ui.end_row();
                });
            });
            if compose.is_some() {
                ui.separator();
                ui.checkbox(&mut st.rec_on, "Flag for Recipients");
                ui.add_enabled_ui(st.rec_on, |ui| {
                    egui::Grid::new(("flagrec", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Flag to");
                        flag_to(ui, ("recflagto", uid), &mut st.rec_label);
                        ui.end_row();
                        ui.label("Due date");
                        common::date_box(
                            ui,
                            app,
                            ("rdue", uid),
                            &mut st.rec_due,
                            "None",
                            "Recipients' due date",
                        );
                        ui.end_row();
                        ui.checkbox(&mut st.rec_reminder, "Reminder");
                        ui.add_enabled_ui(st.rec_reminder, |ui| {
                            ui.horizontal(|ui| {
                                common::date_box(
                                    ui,
                                    app,
                                    ("rrdate", uid),
                                    &mut st.rec_rdate,
                                    "",
                                    "Recipients' reminder date",
                                );
                                common::time_box(
                                    ui,
                                    ("rrtime", uid),
                                    &mut st.rec_rtime,
                                    "Recipients' reminder time",
                                );
                            });
                        });
                        ui.end_row();
                    });
                });
                muted(ui, app, "Recipients' copies arrive flagged with this label, due date and reminder.");
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let mut out = Out::Keep;
            let ok = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    out = Out::Close;
                }
                let ok = common::primary(ui, app, true, "OK");
                if common::btn_en(ui, flagged, "Clear Flag") {
                    match compose {
                        Some(c) => app.run("compose.followUp", json!({"compose": c, "kind": "clear"})),
                        None => app.run("mail.clearFlag", json!({"ids": ids})),
                    };
                    out = Out::Close;
                }
                ok
            });
            if ok && matches!(out, Out::Keep) {
                out = apply_flag(app, st, compose, &ids);
            }
            out
        })
    })
}

/// "YYYY-MM-DD" (or empty / "none") → a date, or the message to show.
fn flag_day(app: &App, s: &str) -> Result<Option<i64>, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    common::parse_date(app, s, "")
        .map(Some)
        .ok_or_else(|| format!("\u{201C}{s}\u{201D} is not a date (YYYY-MM-DD)."))
}

fn apply_flag(app: &mut App, st: &mut FlagDlg, compose: Option<u64>, ids: &[u64]) -> Out {
    let (start, due) = match (flag_day(app, &st.start), flag_day(app, &st.due)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            st.error = e;
            return Out::Keep;
        }
    };
    if let (Some(a), Some(b)) = (start, due)
        && a > b
    {
        st.error = "The start date is after the due date.".into();
        return Out::Keep;
    }
    let reminder = if st.reminder {
        match common::parse_date(app, &st.rdate, &st.rtime) {
            Some(t) => Some(t),
            None => {
                st.error = "The reminder date or time is not valid.".into();
                return Out::Keep;
            }
        }
    } else {
        None
    };
    let res = match compose {
        Some(c) => {
            let rec_due = match flag_day(app, &st.rec_due) {
                Ok(d) => d,
                Err(e) => {
                    st.error = e;
                    return Out::Keep;
                }
            };
            let rec_reminder = if st.rec_on && st.rec_reminder {
                match common::parse_date(app, &st.rec_rdate, &st.rec_rtime) {
                    Some(t) => Some(t),
                    None => {
                        st.error = "The recipients' reminder date or time is not valid.".into();
                        return Out::Keep;
                    }
                }
            } else {
                None
            };
            let recipients = if st.rec_on {
                json!({"label": st.rec_label.trim(), "due": rec_due, "reminder": rec_reminder})
            } else {
                Value::Null
            };
            app.run(
                "compose.followUp",
                json!({"compose": c, "kind": "custom", "flagMe": st.flag_me, "label": st.label.trim(),
                       "start": start, "due": due, "reminder": reminder, "recipients": recipients}),
            )
        }
        None => {
            let mut p = json!({"ids": ids, "label": st.label.trim()});
            if let Some(o) = p.as_object_mut() {
                if let Some(s) = start {
                    o.insert("start".into(), json!(s));
                }
                if let Some(s) = due {
                    o.insert("due".into(), json!(s));
                }
                if let Some(r) = reminder {
                    o.insert("reminder".into(), json!(r));
                }
            }
            app.run("mail.flagCustom", p)
        }
    };
    if res.is_some() {
        Out::Close
    } else {
        st.error = last_error(app);
        Out::Keep
    }
}

/// Category colour as a [`Value`] name (used by rules).
pub(crate) fn category_names(app: &App) -> Vec<String> {
    app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect()
}
