//! View dialogs: Advanced View Settings (with Sort, Group By, Filter, Other Settings, Columns, Conditional
//! Formatting and Format Columns), Conversation Settings, Zoom, Manage All Views, Save View As, Apply View,
//! Quick Actions.
//!
//! The mail view state they edit lives in the engine (`mailcraft_engine::cmd::view::mail`): each dialog reads
//! it when it opens and applies its changes with the `view.*` commands on OK.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, muted, ok_cancel, primary, window, with_state};
use crate::App;
use mailcraft_engine::cmd::view::mail::{self as mailview, CustomFilter, FormatRule, RuleFont};
use mailcraft_engine::cmd::view::modules::ModuleKind;
use mailcraft_engine::cmd::view::table;
use mailcraft_engine::queries::view_settings;
use mailcraft_model::FolderId;
use mailcraft_model::view::{ArrangeBy, ReadingPanePos, SortOrder, ViewLayout};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "advancedViewSettings" => advanced(ctx, app, d),
        "viewSort" => sort(ctx, app, d),
        "viewGroupBy" => group_by(ctx, app, d),
        "viewFilter" => filter(ctx, app, d),
        "viewOtherSettings" => other(ctx, app, d),
        "viewFormat" if d.str("kind") == "columns" => format_columns(ctx, app, d),
        "viewFormat" => rules(ctx, app, d),
        "showColumns" => columns(ctx, app, d),
        "conversationSettings" => conversation(ctx, app, d),
        "zoom" => zoom(ctx, app, d),
        "manageViews" if d.bool("mail") => manage_mail_views(ctx, app, d),
        "manageViews" => manage_views(ctx, app, d),
        "saveViewAs" => save_view_as(ctx, app, d),
        "applyView" => apply_view(ctx, app, d),
        "quickActions" => quick_actions(ctx, app, d),
        _ => return None,
    })
}

fn folder_of(app: &App, d: &Dlg<'_>) -> FolderId {
    d.u64("folder").map_or(app.s.selection.folder, FolderId)
}

fn layout_name(l: ViewLayout) -> &'static str {
    match l {
        ViewLayout::Compact => "Compact",
        ViewLayout::Single => "Single",
        ViewLayout::Preview => "Preview",
    }
}

const ARRANGE_KEYS: [&str; 13] = [
    "date",
    "from",
    "to",
    "categories",
    "flagStatus",
    "flagStart",
    "flagDue",
    "size",
    "subject",
    "type",
    "attachments",
    "account",
    "importance",
];

/// Advanced View Settings (`plan/reference/dialogs.md` › view-settings): a button per part with what it is set
/// to. Opened from Manage Views › Modify (`savedView`), OK stores the folder's view into that saved view.
fn advanced(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let folder = app.s.selection.folder;
    let v = view_settings(&app.s, folder);
    let mail = mailview::mail_list_folder(&app.s, folder);
    let view_name =
        if mail { mailview::current_view_name(&app.s, folder) } else { layout_name(v.layout).to_owned() };
    let title = format!("Advanced View Settings: {view_name}");
    let saved_view = d.str("savedView");
    window(ctx, app, d, &title, 520.0, |ui, app| {
        let mut out = Out::Keep;
        let order = match v.order {
            SortOrder::Descending => "Descending",
            SortOrder::Ascending => "Ascending",
        };
        let (cols, filter, rules, formats) = if mail {
            let cols: Vec<String> = mailview::columns(&app.s, folder).into_iter().map(|c| c.label).collect();
            let filter = mailview::custom_filter(&app.s, folder).unwrap_or_default().describe();
            let rules = mailview::rules(&app.s, folder);
            let on = rules.iter().filter(|r| r.on).count();
            let n = mailview::mail_view(&app.s, folder).formats.len();
            (
                cols.join(", "),
                filter,
                format!("{on} of {} rules on", rules.len()),
                if n == 0 { "Default formats".to_owned() } else { format!("{n} columns formatted") },
            )
        } else {
            let fixed = "Not available in this view".to_owned();
            (fixed.clone(), fixed.clone(), fixed.clone(), fixed)
        };
        let rows: [(&str, &str, &str, String, bool); 7] = [
            ("Columns...", "showColumns", "", cols, mail),
            (
                "Group By...",
                "viewGroupBy",
                "",
                if v.show_in_groups { format!("{} (automatic)", v.arrange_by.label()) } else { "Off".into() },
                true,
            ),
            ("Sort...", "viewSort", "", format!("{} ({order})", v.arrange_by.label()), true),
            ("Filter...", "viewFilter", "", filter, mail),
            (
                "Other Settings...",
                "viewOtherSettings",
                "",
                "Fonts and other Table View settings".into(),
                true,
            ),
            ("Conditional Formatting...", "viewFormat", "rules", rules, mail),
            ("Format Columns...", "viewFormat", "columns", formats, mail),
        ];
        egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(8.0).show(
            ui,
            |ui| {
                ui.label(RichText::new("Description").strong());
                egui::Grid::new(("avs", d.uid)).num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    for (label, dlg, kind, desc, enabled) in &rows {
                        let b = egui::Button::new(*label).min_size(egui::vec2(170.0, 24.0));
                        if ui.add_enabled(*enabled, b).clicked() {
                            let mut params = json!({"folder": folder.0});
                            if !kind.is_empty()
                                && let Some(o) = params.as_object_mut()
                            {
                                o.insert("kind".into(), json!(kind));
                            }
                            out = Out::open(dlg, params);
                        }
                        ui.add(egui::Label::new(RichText::new(desc).color(app.theme.text_muted)).truncate());
                        ui.end_row();
                    }
                });
            },
        );
        if btn(ui, "Reset Current View") {
            app.run("view.resetView", json!({}));
        }
        if !matches!(out, Out::Keep) {
            return out;
        }
        match ok_cancel(ui, app, true) {
            Some(true) => {
                if !saved_view.is_empty() {
                    app.run("view.manageViews", json!({"op": "update", "name": saved_view}));
                }
                Out::Close
            }
            Some(false) => Out::Close,
            None => Out::Keep,
        }
    })
}

#[derive(Clone, Default)]
struct SortState {
    key: usize,
    desc: bool,
}

fn sort(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    if let Some((_, kind, _)) = table::table_here(&app.s) {
        return module_sort(ctx, app, d, kind);
    }
    let uid = d.uid;
    let folder = folder_of(app, d);
    let init = |a: &App| {
        let v = view_settings(&a.s, folder);
        SortState {
            key: ArrangeBy::ALL.iter().position(|x| *x == v.arrange_by).unwrap_or(0),
            desc: v.order == SortOrder::Descending,
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Sort", 380.0, |ui, app| {
            ui.label("Sort items by");
            let labels: Vec<&str> = ArrangeBy::ALL.iter().map(|a| a.label()).collect();
            ui.horizontal(|ui| {
                common::combo_idx(ui, ("sortk", uid), &mut st.key, &labels, 180.0);
                ui.vertical(|ui| {
                    ui.radio_value(&mut st.desc, false, "Ascending");
                    ui.radio_value(&mut st.desc, true, "Descending");
                });
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let v = view_settings(&app.s, folder);
                    let key = ARRANGE_KEYS.get(st.key).copied().unwrap_or("date");
                    if ArrangeBy::ALL.get(st.key) != Some(&v.arrange_by) {
                        app.run("view.arrangeBy", json!({"key": key}));
                    }
                    let now = view_settings(&app.s, folder);
                    if (now.order == SortOrder::Descending) != st.desc {
                        app.run("view.sortOrder", json!({"dir": if st.desc { "desc" } else { "asc" }}));
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct OnState {
    on: bool,
}

fn group_by(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = folder_of(app, d);
    with_state(
        ctx,
        app,
        uid,
        |a: &App| OnState { on: view_settings(&a.s, folder).show_in_groups },
        |st, app| {
            window(ctx, app, d, "Group By", 380.0, |ui, app| {
                ui.checkbox(&mut st.on, "Automatically group according to arrangement");
                let v = view_settings(&app.s, folder);
                muted(ui, app, &format!("Items are arranged by {}.", v.arrange_by.label()));
                match ok_cancel(ui, app, true) {
                    Some(true) => {
                        if view_settings(&app.s, folder).show_in_groups != st.on {
                            app.run("view.showInGroups", json!({}));
                        }
                        Out::Close
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct FilterState {
    f: CustomFilter,
    days: String,
}

const READ_CHOICES: [(&str, &str); 3] = [("", "Any"), ("unread", "Unread"), ("read", "Read")];
const IMPORTANCE_CHOICES: [(&str, &str); 4] =
    [("", "Any"), ("high", "High"), ("normal", "Normal"), ("low", "Low")];

/// View Settings › Filter: the conditions an item must meet to show in this view (`view.customFilter`).
fn filter(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    if let Some((_, kind, _)) = table::table_here(&app.s) {
        return module_filter(ctx, app, d, kind);
    }
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let init = |a: &App| {
        let f = mailview::custom_filter(&a.s, folder).unwrap_or_default();
        let days = if f.days == 0 { String::new() } else { f.days.to_string() };
        FilterState { f, days }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Filter", 460.0, |ui, app| {
            common::heading(ui, app, "Messages");
            common::field(ui, "Search for the word(s):", &mut st.f.words, 240.0);
            ui.horizontal(|ui| {
                ui.label("In:");
                ui.radio_value(&mut st.f.in_body, false, "subject field only");
                ui.radio_value(&mut st.f.in_body, true, "subject field and message body");
            });
            common::field(ui, "From:", &mut st.f.from, 280.0);
            common::field(ui, "Sent To:", &mut st.f.sent_to, 270.0);
            ui.horizontal(|ui| {
                ui.label("Received in the last");
                ui.add(egui::TextEdit::singleline(&mut st.days).desired_width(50.0));
                ui.label("days (empty: any time)");
            });
            common::heading(ui, app, "More Choices");
            ui.horizontal(|ui| {
                ui.label("Only items that are:");
                let mut idx = READ_CHOICES.iter().position(|(k, _)| *k == st.f.read).unwrap_or(0);
                let labels: Vec<&str> = READ_CHOICES.iter().map(|x| x.1).collect();
                if common::combo_idx(ui, ("vf-read", uid), &mut idx, &labels, 120.0) {
                    st.f.read = READ_CHOICES.get(idx).map_or("", |x| x.0).to_owned();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Whose importance is:");
                let mut idx = IMPORTANCE_CHOICES.iter().position(|(k, _)| *k == st.f.importance).unwrap_or(0);
                let labels: Vec<&str> = IMPORTANCE_CHOICES.iter().map(|x| x.1).collect();
                if common::combo_idx(ui, ("vf-imp", uid), &mut idx, &labels, 120.0) {
                    st.f.importance = IMPORTANCE_CHOICES.get(idx).map_or("", |x| x.0).to_owned();
                }
            });
            ui.checkbox(&mut st.f.has_attachments, "Only items with one or more attachments");
            ui.checkbox(&mut st.f.flagged, "Only items that are flagged");
            common::field(ui, "Category:", &mut st.f.category, 200.0);
            let days_ok = st.days.trim().is_empty() || st.days.trim().parse::<u32>().is_ok();
            if !days_ok {
                ui.colored_label(app.theme.danger, "Enter a number of days.");
            }
            let mut clear = false;
            let r = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Some(false);
                }
                if primary(ui, app, days_ok, "OK") {
                    return Some(true);
                }
                if btn(ui, "Clear All") {
                    clear = true;
                }
                None
            });
            if clear {
                *st = FilterState::default();
            }
            match r {
                Some(true) => {
                    let mut f = st.f.clone();
                    f.days = st.days.trim().parse().unwrap_or(0);
                    let v = if f.is_empty() {
                        Value::Null
                    } else {
                        serde_json::to_value(&f).unwrap_or(Value::Null)
                    };
                    app.run("view.customFilter", json!({"filter": v}));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct OtherState {
    layout: usize,
    preview: usize,
    pane: usize,
    tighter: bool,
    conversations: bool,
}

fn other(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = folder_of(app, d);
    let init = |a: &App| {
        let v = view_settings(&a.s, folder);
        OtherState {
            layout: match v.layout {
                ViewLayout::Compact => 0,
                ViewLayout::Single => 1,
                ViewLayout::Preview => 2,
            },
            preview: usize::from(v.preview_lines),
            pane: match v.reading_pane {
                ReadingPanePos::Right => 0,
                ReadingPanePos::Bottom => 1,
                ReadingPanePos::Off => 2,
            },
            tighter: v.tighter_spacing,
            conversations: v.conversations,
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Other Settings", 420.0, |ui, app| {
            common::heading(ui, app, "Message list");
            ui.horizontal(|ui| {
                ui.label("Layout:");
                common::combo_idx(ui, ("ol", uid), &mut st.layout, &["Compact", "Single", "Preview"], 140.0);
            });
            ui.horizontal(|ui| {
                ui.label("Message Preview:");
                common::combo_idx(
                    ui,
                    ("op", uid),
                    &mut st.preview,
                    &["Off", "1 line", "2 lines", "3 lines"],
                    140.0,
                );
            });
            ui.checkbox(&mut st.tighter, "Use tighter spacing");
            ui.checkbox(&mut st.conversations, "Show as Conversations");
            common::heading(ui, app, "Reading Pane");
            for (i, l) in ["Right", "Bottom", "Off"].iter().enumerate() {
                ui.radio_value(&mut st.pane, i, *l);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let v = view_settings(&app.s, folder);
                    let layout =
                        ["compact", "single", "preview"].get(st.layout).copied().unwrap_or("compact");
                    if layout_name(v.layout).to_lowercase() != layout {
                        app.run("view.layout", json!({"layout": layout}));
                    }
                    if usize::from(v.preview_lines) != st.preview {
                        app.run("view.messagePreview", json!({"lines": st.preview}));
                    }
                    if v.tighter_spacing != st.tighter {
                        app.run("view.tighterSpacing", json!({}));
                    }
                    if v.conversations != st.conversations {
                        app.run("view.conversations", json!({}));
                    }
                    let pos = ["right", "bottom", "off"].get(st.pane).copied().unwrap_or("right");
                    app.run("view.readingPane", json!({"position": pos}));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Colours offered for Conditional Formatting fonts: (label, "#rrggbb"; empty = the list's own).
const RULE_COLORS: [(&str, &str); 9] = [
    ("Automatic", ""),
    ("Red", "#c42b1c"),
    ("Orange", "#ca5010"),
    ("Green", "#107c10"),
    ("Blue", "#0f6cbd"),
    ("Purple", "#8764b8"),
    ("Teal", "#038387"),
    ("Gray", "#8a8886"),
    ("Brown", "#8e562e"),
];

#[derive(Clone, Default)]
struct RulesState {
    rules: Vec<FormatRule>,
    pick: Option<usize>,
}

fn rule_condition_text(r: &FormatRule) -> String {
    match r.kind.as_str() {
        "unread" => "Unread messages".into(),
        "overdue" => "The follow-up due date has passed".into(),
        "expired" => "The message's expiry date has passed".into(),
        _ => r.condition.describe(),
    }
}

/// Conditional Formatting: rules that change the font of matching rows, applied top to bottom
/// (`view.conditionalFormatting`). The built-in rules can be turned off but not removed.
fn rules(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    if let Some((_, kind, _)) = table::table_here(&app.s) {
        return module_rules(ctx, app, d, kind);
    }
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let init = |a: &App| RulesState { rules: mailview::rules(&a.s, folder), pick: Some(0) };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Conditional Formatting", 520.0, |ui, app| {
            ui.label("Rules for this view:");
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ui.set_min_height(120.0);
                    ui.set_width(ui.available_width());
                    for (i, r) in st.rules.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut r.on, "");
                            if ui.selectable_label(st.pick == Some(i), r.name.as_str()).clicked() {
                                st.pick = Some(i);
                            }
                        });
                    }
                },
            );
            let n = st.rules.len();
            let custom = st.pick.and_then(|i| st.rules.get(i)).is_some_and(|r| r.kind == "custom");
            ui.horizontal(|ui| {
                if btn_en(ui, n < mailview::MAX_RULES, "Add") {
                    st.rules.push(FormatRule {
                        name: "Untitled".into(),
                        on: true,
                        kind: "custom".into(),
                        condition: CustomFilter::default(),
                        font: RuleFont { bold: true, ..Default::default() },
                    });
                    st.pick = Some(st.rules.len().saturating_sub(1));
                }
                if btn_en(ui, custom, "Delete")
                    && let Some(i) = st.pick
                    && i < st.rules.len()
                {
                    st.rules.remove(i);
                    st.pick = None;
                }
                if btn_en(ui, st.pick.is_some_and(|i| i > 0), "Move Up")
                    && let Some(i) = st.pick
                    && i > 0
                    && i < n
                {
                    st.rules.swap(i, i - 1);
                    st.pick = Some(i - 1);
                }
                if btn_en(ui, st.pick.is_some_and(|i| i + 1 < n), "Move Down")
                    && let Some(i) = st.pick
                    && i + 1 < n
                {
                    st.rules.swap(i, i + 1);
                    st.pick = Some(i + 1);
                }
            });
            if let Some(r) = st.pick.and_then(|i| st.rules.get_mut(i)) {
                common::heading(ui, app, "Properties of selected rule");
                ui.add_enabled_ui(r.kind == "custom", |ui| {
                    common::field(ui, "Name:", &mut r.name, 260.0);
                });
                ui.horizontal(|ui| {
                    ui.label("Font:");
                    ui.checkbox(&mut r.font.bold, "Bold");
                    ui.checkbox(&mut r.font.italic, "Italic");
                    ui.checkbox(&mut r.font.underline, "Underline");
                    ui.checkbox(&mut r.font.strike, "Strikethrough");
                });
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut idx = RULE_COLORS
                        .iter()
                        .position(|(_, h)| h.eq_ignore_ascii_case(&r.font.color))
                        .unwrap_or(0);
                    let labels: Vec<&str> = RULE_COLORS.iter().map(|x| x.0).collect();
                    if common::combo_idx(ui, ("cf-color", uid), &mut idx, &labels, 140.0) {
                        r.font.color = RULE_COLORS.get(idx).map_or("", |x| x.1).to_owned();
                    }
                });
                if r.kind == "custom" {
                    ui.label("Condition (the row matches every part that is filled in):");
                    common::field(ui, "Words in the subject:", &mut r.condition.words, 220.0);
                    common::field(ui, "From:", &mut r.condition.from, 280.0);
                    common::field(ui, "Sent To:", &mut r.condition.sent_to, 270.0);
                    ui.horizontal(|ui| {
                        ui.label("Importance:");
                        let mut idx = IMPORTANCE_CHOICES
                            .iter()
                            .position(|(k, _)| *k == r.condition.importance)
                            .unwrap_or(0);
                        let labels: Vec<&str> = IMPORTANCE_CHOICES.iter().map(|x| x.1).collect();
                        if common::combo_idx(ui, ("cf-imp", uid), &mut idx, &labels, 120.0) {
                            r.condition.importance =
                                IMPORTANCE_CHOICES.get(idx).map_or("", |x| x.0).to_owned();
                        }
                    });
                    ui.checkbox(&mut r.condition.has_attachments, "With attachments");
                } else {
                    muted(ui, app, &format!("Condition: {}", rule_condition_text(r)));
                }
            }
            let mut reset = false;
            let res = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Some(false);
                }
                if primary(ui, app, true, "OK") {
                    return Some(true);
                }
                if btn(ui, "Reset") {
                    reset = true;
                }
                None
            });
            if reset {
                st.rules = mailview::default_rules();
                st.pick = Some(0);
            }
            match res {
                Some(true) => {
                    let rules = serde_json::to_value(&st.rules).unwrap_or(Value::Null);
                    if app.run("view.conditionalFormatting", json!({"rules": rules})).is_some() {
                        Out::Close
                    } else {
                        Out::Keep
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// One column in the Format Columns dialog.
#[derive(Clone, Default)]
struct ColFormat {
    id: String,
    label: String,
    default_label: String,
    format: String,
    formats: Vec<(String, String)>,
    align: String,
    start: (String, String, String),
}

#[derive(Clone, Default)]
struct FormatState {
    cols: Vec<ColFormat>,
    pick: usize,
}

/// Format Columns: a column's label, display format and alignment (`view.formatColumns`).
fn format_columns(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let init = |a: &App| {
        if table::table_here(&a.s).is_some() {
            // Module tables: label and alignment only (one display format).
            return FormatState {
                cols: table::shown(&a.s, folder)
                    .into_iter()
                    .map(|c| ColFormat {
                        id: c.col.id.to_owned(),
                        default_label: c.col.label.to_owned(),
                        formats: vec![("text".to_owned(), "Text".to_owned())],
                        start: (c.label.clone(), "text".to_owned(), c.align.to_owned()),
                        label: c.label,
                        format: "text".to_owned(),
                        align: c.align.to_owned(),
                    })
                    .collect(),
                pick: 0,
            };
        }
        FormatState {
            cols: mailview::columns(&a.s, folder)
                .into_iter()
                .map(|c| {
                    let def = mailview::column(c.id);
                    ColFormat {
                        id: c.id.to_owned(),
                        default_label: def.map_or_else(String::new, |x| x.label.to_owned()),
                        formats: c
                            .kind
                            .formats()
                            .iter()
                            .map(|(i, l)| ((*i).to_owned(), (*l).to_owned()))
                            .collect(),
                        start: (c.label.clone(), c.format.to_owned(), c.align.to_owned()),
                        label: c.label,
                        format: c.format.to_owned(),
                        align: c.align.to_owned(),
                    }
                })
                .collect(),
            pick: 0,
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Format Columns", 460.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    ui.label("Available columns:");
                    for (i, c) in st.cols.iter().enumerate() {
                        if ui.selectable_label(st.pick == i, c.label.as_str()).clicked() {
                            st.pick = i;
                        }
                    }
                });
                ui.vertical(|ui| {
                    if let Some(c) = st.cols.get_mut(st.pick) {
                        ui.label(
                            RichText::new(format!("Field: {}", c.default_label)).color(app.theme.text_muted),
                        );
                        common::field(ui, "Label:", &mut c.label, 180.0);
                        ui.horizontal(|ui| {
                            ui.label("Format:");
                            let labels: Vec<&str> = c.formats.iter().map(|x| x.1.as_str()).collect();
                            let mut idx = c.formats.iter().position(|x| x.0 == c.format).unwrap_or(0);
                            if common::combo_idx(ui, ("fc-format", uid, st.pick), &mut idx, &labels, 170.0) {
                                c.format = c.formats.get(idx).map(|x| x.0.clone()).unwrap_or_default();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Alignment:");
                            for (v, l) in [("left", "Left"), ("center", "Center"), ("right", "Right")] {
                                if ui.radio(c.align == v, l).clicked() {
                                    c.align = v.to_owned();
                                }
                            }
                        });
                    }
                });
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    for c in &st.cols {
                        if (c.label.clone(), c.format.clone(), c.align.clone()) == c.start {
                            continue;
                        }
                        let label =
                            if c.label.trim().is_empty() { c.default_label.clone() } else { c.label.clone() };
                        app.run(
                            "view.formatColumns",
                            json!({"column": c.id, "label": label, "format": c.format, "align": c.align}),
                        );
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct ColState {
    /// Shown column ids in order.
    shown: Vec<String>,
    /// Width per shown column (0 = automatic).
    widths: Vec<u16>,
    start_widths: Vec<(String, u16)>,
    pick_avail: Option<String>,
    pick_shown: Option<usize>,
}

fn col_label(id: &str) -> &'static str {
    mailview::column(id).map_or("", |c| c.label)
}

/// Show Columns (Add Columns, View Settings › Columns): which columns the column layout shows, in what order
/// and how wide (`view.columns`). Module tables have their own columns ([`module_columns`]).
fn columns(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    if let Some((_, kind, _)) = table::table_here(&app.s) {
        return module_columns(ctx, app, d, kind);
    }
    if !mailview::mail_list_folder(&app.s, folder) {
        return window(ctx, app, d, "Show Columns", 360.0, |ui, app| {
            muted(ui, app, "This view has no columns.");
            ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close)
        });
    }
    let init = |a: &App| {
        let cols = mailview::columns(&a.s, folder);
        let start_widths: Vec<(String, u16)> =
            cols.iter().map(|c| (c.id.to_owned(), c.width.unwrap_or(0))).collect();
        ColState {
            shown: cols.iter().map(|c| c.id.to_owned()).collect(),
            widths: cols.iter().map(|c| c.width.unwrap_or(0)).collect(),
            start_widths,
            ..Default::default()
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Show Columns", 560.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.label("Available columns:");
                    for c in mailview::COLUMNS.iter().filter(|c| !st.shown.iter().any(|s| s == c.id)) {
                        if ui.selectable_label(st.pick_avail.as_deref() == Some(c.id), c.label).clicked() {
                            st.pick_avail = Some(c.id.to_owned());
                        }
                    }
                });
                ui.vertical(|ui| {
                    if btn_en(ui, st.pick_avail.is_some() && st.shown.len() < mailview::MAX_COLUMNS, "Add ->")
                        && let Some(c) = st.pick_avail.take()
                    {
                        st.shown.push(c);
                        st.widths.push(0);
                    }
                    if btn_en(ui, st.pick_shown.is_some() && st.shown.len() > 1, "<- Remove")
                        && let Some(i) = st.pick_shown.take()
                        && i < st.shown.len()
                    {
                        st.shown.remove(i);
                        if i < st.widths.len() {
                            st.widths.remove(i);
                        }
                    }
                });
                ui.vertical(|ui| {
                    ui.label("Show these columns in this order:");
                    for (i, c) in st.shown.iter().enumerate() {
                        if ui.selectable_label(st.pick_shown == Some(i), col_label(c)).clicked() {
                            st.pick_shown = Some(i);
                        }
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i > 0), "Move Up")
                            && let Some(i) = st.pick_shown
                            && i > 0
                            && i < st.shown.len()
                        {
                            st.shown.swap(i, i - 1);
                            if i < st.widths.len() {
                                st.widths.swap(i, i - 1);
                            }
                            st.pick_shown = Some(i - 1);
                        }
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i + 1 < st.shown.len()), "Move Down")
                            && let Some(i) = st.pick_shown
                            && i + 1 < st.shown.len()
                        {
                            st.shown.swap(i, i + 1);
                            if i + 1 < st.widths.len() {
                                st.widths.swap(i, i + 1);
                            }
                            st.pick_shown = Some(i + 1);
                        }
                    });
                    if let Some(i) = st.pick_shown
                        && let Some(w) = st.widths.get_mut(i)
                    {
                        ui.horizontal(|ui| {
                            ui.label("Width:");
                            ui.add(egui::DragValue::new(w).range(0..=mailview::MAX_WIDTH).suffix(" px"));
                            ui.label(RichText::new("(0: automatic)").color(app.theme.text_muted));
                        });
                    }
                });
            });
            match ok_cancel(ui, app, !st.shown.is_empty()) {
                Some(true) => {
                    let mut widths = serde_json::Map::new();
                    for (id, w) in st.shown.iter().zip(st.widths.iter()) {
                        let before = st.start_widths.iter().find(|(k, _)| k == id).map_or(0, |(_, w)| *w);
                        if *w != before {
                            widths.insert(id.clone(), if *w == 0 { Value::Null } else { json!(w) });
                        }
                    }
                    let ok =
                        app.run("view.columns", json!({"columns": st.shown, "widths": widths})).is_some();
                    if ok { Out::Close } else { Out::Keep }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

const CONV: [(&str, &str); 4] = [
    ("fromOtherFolders", "Show Messages from Other Folders"),
    ("sendersAboveSubject", "Show Senders Above the Subject"),
    ("alwaysExpand", "Always Expand Selected Conversation"),
    ("classicIndent", "Use Classic Indented View"),
];

#[derive(Clone, Default)]
struct ConvState {
    on: Vec<bool>,
    conversations: bool,
}

fn conversation(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = folder_of(app, d);
    // The folder's settings as the engine keeps them (`view.conversationSettings`).
    let current = |a: &App| -> Vec<bool> {
        let cs = mailcraft_engine::queries::conversation_settings(&a.s, folder);
        CONV.iter().map(|(k, _)| cs.get(k).unwrap_or(false)).collect()
    };
    let init =
        |a: &App| ConvState { on: current(a), conversations: view_settings(&a.s, folder).conversations };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Conversation Settings", 380.0, |ui, app| {
            ui.checkbox(&mut st.conversations, "Show as Conversations");
            ui.add_enabled_ui(st.conversations, |ui| {
                for ((_, label), on) in CONV.iter().zip(st.on.iter_mut()) {
                    ui.checkbox(on, *label);
                }
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let was_on = view_settings(&app.s, folder).conversations;
                    // Settings change only while the view is on (the engine refuses them otherwise).
                    if st.conversations && !was_on {
                        app.run("view.conversations", json!({"on": true}));
                    }
                    if st.conversations || was_on {
                        let now = current(app);
                        for (((k, _), on), cur) in CONV.iter().zip(st.on.iter()).zip(now.iter()) {
                            if on != cur {
                                app.run("view.conversationSettings", json!({"setting": k, "on": on}));
                            }
                        }
                    }
                    if !st.conversations && was_on {
                        app.run("view.conversations", json!({"on": false}));
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct ZoomState {
    pct: u32,
}

fn zoom(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |a: &App| ZoomState {
        pct: match compose.and_then(|c| a.s.composes.get(&c)) {
            Some(c) => u32::from(c.zoom_pct()),
            None => {
                d.u64("pct").and_then(|v| u32::try_from(v).ok()).unwrap_or_else(|| u32::from(a.s.view.zoom))
            }
        },
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Zoom", 300.0, |ui, app| {
            ui.label("Zoom to");
            for p in [200u32, 150, 100, 75, 50] {
                ui.radio_value(&mut st.pct, p, format!("{p}%"));
            }
            ui.horizontal(|ui| {
                ui.label("Percent:");
                ui.add(egui::DragValue::new(&mut st.pct).range(10..=500).suffix("%"));
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    match compose {
                        Some(c) => app.run("format.zoom", json!({"compose": c, "pct": st.pct})),
                        None => app.run("view.zoom", json!({"pct": st.pct})),
                    };
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// What the Manage All Views dialog is asking for below its list.
#[derive(Clone, Default, PartialEq)]
enum ViewPrompt {
    #[default]
    None,
    New,
    Copy,
    Rename,
}

#[derive(Clone, Default)]
struct MailViewsState {
    selected: String,
    this_folder: bool,
    prompt: ViewPrompt,
    name: String,
    /// "folder" or "all".
    scope: String,
    base: usize,
}

/// Manage All Views in a mail folder (`plan/reference/dialogs.md` › manage-views): the built-in views and the
/// saved ones with where they can be used; New, Copy, Modify, Rename, Reset, Delete and Apply View
/// (`view.manageViews`).
fn manage_mail_views(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let current = mailview::current_view_name(&app.s, folder);
    with_state(
        ctx,
        app,
        uid,
        |_: &App| MailViewsState { selected: current.clone(), scope: "all".into(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Manage All Views", 520.0, |ui, app| {
                let saved = mailview::saved_views(&app.s);
                let mut out = Out::Keep;
                let built_in = mailview::BUILT_IN.iter().any(|b| b.eq_ignore_ascii_case(&st.selected));
                let is_saved = saved.iter().any(|v| v.name.eq_ignore_ascii_case(&st.selected));
                ui.horizontal_top(|ui| {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_width(320.0);
                            ui.set_min_height(200.0);
                            egui::Grid::new(("mmv", uid)).num_columns(2).show(ui, |ui| {
                                ui.label(RichText::new("View Name").strong());
                                ui.label(RichText::new("Can Be Used On").strong());
                                ui.end_row();
                                if !st.this_folder {
                                    for v in mailview::BUILT_IN {
                                        if ui
                                            .selectable_label(st.selected.eq_ignore_ascii_case(v), v)
                                            .clicked()
                                        {
                                            st.selected = v.to_owned();
                                        }
                                        ui.label(mailview::ALL_FOLDERS);
                                        ui.end_row();
                                    }
                                }
                                for v in &saved {
                                    if st.this_folder && v.folder != Some(folder.0) {
                                        continue;
                                    }
                                    if ui
                                        .selectable_label(st.selected.eq_ignore_ascii_case(&v.name), &v.name)
                                        .clicked()
                                    {
                                        st.selected = v.name.clone();
                                    }
                                    let used = match v.folder {
                                        None => mailview::ALL_FOLDERS.to_owned(),
                                        Some(f) => app.s.mailbox.folder(FolderId(f)).map_or_else(
                                            || "(deleted folder)".to_owned(),
                                            |x| x.name.clone(),
                                        ),
                                    };
                                    ui.label(used);
                                    ui.end_row();
                                }
                            });
                        });
                    ui.vertical(|ui| {
                        if btn(ui, "New...") {
                            st.prompt = ViewPrompt::New;
                            st.name = String::new();
                        }
                        if btn_en(ui, built_in || is_saved, "Copy...") {
                            st.prompt = ViewPrompt::Copy;
                            st.name = format!("Copy of {}", st.selected);
                        }
                        if btn_en(ui, built_in || is_saved, "Modify...") {
                            // Show the view, then edit it; OK in View Settings stores a saved view.
                            app.run("view.manageViews", json!({"op": "apply", "name": st.selected}));
                            let mut p = json!({"folder": folder.0});
                            if is_saved && let Some(o) = p.as_object_mut() {
                                o.insert("savedView".into(), json!(st.selected));
                            }
                            out = Out::open("advancedViewSettings", p);
                        }
                        if btn_en(ui, is_saved, "Rename...") {
                            st.prompt = ViewPrompt::Rename;
                            st.name = st.selected.clone();
                        }
                        if btn_en(ui, built_in, "Reset") {
                            app.run("view.manageViews", json!({"op": "reset", "name": st.selected}));
                        }
                        if btn_en(ui, is_saved, "Delete")
                            && app
                                .run("view.manageViews", json!({"op": "delete", "name": st.selected}))
                                .is_some()
                        {
                            st.selected = mailview::current_view_name(&app.s, folder);
                        }
                    });
                });
                ui.checkbox(&mut st.this_folder, "Only show views created for this folder");
                if st.prompt != ViewPrompt::None {
                    let title = match st.prompt {
                        ViewPrompt::New => "Create a New View",
                        ViewPrompt::Copy => "Copy View",
                        _ => "Rename View",
                    };
                    common::heading(ui, app, title);
                    common::field(ui, "Name of new view:", &mut st.name, 220.0);
                    if st.prompt == ViewPrompt::New {
                        ui.horizontal(|ui| {
                            ui.label("Type of view:");
                            common::combo_idx(
                                ui,
                                ("mmv-base", uid),
                                &mut st.base,
                                &mailview::BUILT_IN,
                                120.0,
                            );
                        });
                    }
                    if st.prompt != ViewPrompt::Rename {
                        ui.label("Can be used on:");
                        ui.radio_value(&mut st.scope, "folder".to_owned(), "This folder");
                        ui.radio_value(&mut st.scope, "all".to_owned(), mailview::ALL_FOLDERS);
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, !st.name.trim().is_empty(), "OK") {
                            let p = match st.prompt {
                                ViewPrompt::New => json!({
                                    "op": "new",
                                    "name": st.name.trim(),
                                    "base": mailview::BUILT_IN.get(st.base).copied().unwrap_or("Compact"),
                                    "scope": st.scope,
                                }),
                                ViewPrompt::Copy => json!({
                                    "op": "copy", "name": st.selected, "newName": st.name.trim(), "scope": st.scope,
                                }),
                                _ => json!({"op": "rename", "name": st.selected, "newName": st.name.trim()}),
                            };
                            if app.run("view.manageViews", p).is_some() {
                                st.selected = st.name.trim().to_owned();
                                st.prompt = ViewPrompt::None;
                            }
                        }
                        if btn(ui, "Cancel") {
                            st.prompt = ViewPrompt::None;
                        }
                    });
                }
                if !matches!(out, Out::Keep) {
                    return out;
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if primary(ui, app, built_in || is_saved, "Apply View") {
                        app.run("view.manageViews", json!({"op": "apply", "name": st.selected}));
                        return Out::Close;
                    }
                    Out::Keep
                })
            })
        },
    )
}

/// Manage All Views in a contacts, tasks or notes folder: the module's built-in views and the saved ones
/// with where they can be used; New, Copy, Modify, Rename, Reset, Delete and Apply View
/// (`view.manageViews` with `op`).
fn manage_views(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use mailcraft_engine::cmd::view::modules;
    let uid = d.uid;
    let folder = folder_of(app, d);
    let Some(kind) = modules::module_kind(&app.s, folder) else {
        return window(ctx, app, d, "Manage All Views", 360.0, |ui, app| {
            muted(ui, app, "This folder has no views to manage.");
            ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close)
        });
    };
    let built_in: &'static [&'static str] = kind.views();
    let current = modules::current_view_name(&app.s, folder);
    let all_label = modules::scope_label(kind, true);
    with_state(
        ctx,
        app,
        uid,
        |_: &App| MailViewsState { selected: current.clone(), scope: "folder".into(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Manage All Views", 520.0, |ui, app| {
                let saved = modules::saved_views(&app.s, folder);
                let is_built = built_in.iter().any(|b| b.eq_ignore_ascii_case(&st.selected));
                let is_saved = saved.iter().any(|v| v.name.eq_ignore_ascii_case(&st.selected));
                ui.horizontal_top(|ui| {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_width(320.0);
                            ui.set_min_height(200.0);
                            egui::ScrollArea::vertical().id_salt(("mmv-scroll", uid)).max_height(260.0).show(
                                ui,
                                |ui| {
                                    egui::Grid::new(("mmv", uid)).num_columns(2).show(ui, |ui| {
                                        ui.label(RichText::new("View Name").strong());
                                        ui.label(RichText::new("Can Be Used On").strong());
                                        ui.end_row();
                                        if !st.this_folder {
                                            for v in built_in {
                                                let sel = st.selected.eq_ignore_ascii_case(v);
                                                if ui.selectable_label(sel, *v).clicked() {
                                                    st.selected = (*v).to_owned();
                                                }
                                                ui.label(all_label.as_str());
                                                ui.end_row();
                                            }
                                        }
                                        for v in &saved {
                                            if st.this_folder && v.everywhere {
                                                continue;
                                            }
                                            let sel = st.selected.eq_ignore_ascii_case(&v.name);
                                            if ui.selectable_label(sel, &v.name).clicked() {
                                                st.selected = v.name.clone();
                                            }
                                            ui.label(modules::scope_label(kind, v.everywhere));
                                            ui.end_row();
                                        }
                                    });
                                },
                            );
                        });
                    ui.vertical(|ui| {
                        if btn(ui, "New...") {
                            st.prompt = ViewPrompt::New;
                            st.name = String::new();
                            st.scope = "folder".into();
                        }
                        if btn_en(ui, is_built || is_saved, "Copy...") {
                            st.prompt = ViewPrompt::Copy;
                            st.name = format!("Copy of {}", st.selected);
                            st.scope = "folder".into();
                        }
                        if btn_en(ui, is_built || is_saved, "Modify...") {
                            // Show the view, then edit it; OK in View Settings stores a saved view.
                            app.run("view.manageViews", json!({"op": "apply", "name": st.selected}));
                            let p = if is_saved { json!({"savedView": st.selected}) } else { json!({}) };
                            app.run("view.viewSettings", p);
                        }
                        if btn_en(ui, is_saved, "Rename...") {
                            st.prompt = ViewPrompt::Rename;
                            st.name = st.selected.clone();
                        }
                        if btn_en(ui, is_built, "Reset") {
                            app.run("view.manageViews", json!({"op": "reset", "name": st.selected}));
                        }
                        if btn_en(ui, is_saved, "Delete")
                            && app
                                .run("view.manageViews", json!({"op": "delete", "name": st.selected}))
                                .is_some()
                        {
                            st.selected = modules::current_view_name(&app.s, folder);
                        }
                    });
                });
                if let Some(rows) = modules::view_description(&app.s, folder, &st.selected) {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .inner_margin(6.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Description").strong());
                            for (label, text) in rows {
                                let line =
                                    RichText::new(format!("{label}: {text}")).color(app.theme.text_muted);
                                ui.add(egui::Label::new(line).truncate());
                            }
                        });
                }
                ui.checkbox(&mut st.this_folder, "Only show views created for this folder");
                if st.prompt != ViewPrompt::None {
                    let title = match st.prompt {
                        ViewPrompt::New => "Create a New View",
                        ViewPrompt::Copy => "Copy View",
                        _ => "Rename View",
                    };
                    common::heading(ui, app, title);
                    common::field(ui, "Name of new view:", &mut st.name, 220.0);
                    if st.prompt == ViewPrompt::New {
                        ui.horizontal(|ui| {
                            ui.label("Type of view:");
                            common::combo_idx(ui, ("mmv-base", uid), &mut st.base, built_in, 140.0);
                        });
                    }
                    if st.prompt != ViewPrompt::Rename {
                        ui.label("Can be used on:");
                        ui.radio_value(&mut st.scope, "folder".to_owned(), "This folder");
                        ui.radio_value(&mut st.scope, "all".to_owned(), all_label.as_str());
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, !st.name.trim().is_empty(), "OK") {
                            let p = match st.prompt {
                                ViewPrompt::New => json!({
                                    "op": "new",
                                    "name": st.name.trim(),
                                    "base": built_in.get(st.base).copied().unwrap_or(kind.default_view()),
                                    "scope": st.scope,
                                }),
                                ViewPrompt::Copy => json!({
                                    "op": "copy", "name": st.selected, "newName": st.name.trim(), "scope": st.scope,
                                }),
                                _ => json!({"op": "rename", "name": st.selected, "newName": st.name.trim()}),
                            };
                            if app.run("view.manageViews", p).is_some() {
                                st.selected = st.name.trim().to_owned();
                                st.prompt = ViewPrompt::None;
                            }
                        }
                        if btn(ui, "Cancel") {
                            st.prompt = ViewPrompt::None;
                        }
                    });
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if primary(ui, app, is_built || is_saved, "Apply View") {
                        app.run("view.manageViews", json!({"op": "apply", "name": st.selected}));
                        return Out::Close;
                    }
                    // New, Copy, Rename, Reset and Delete take effect at once: OK closes and keeps them
                    // (hidden while a name is asked, whose own OK confirms it).
                    if st.prompt == ViewPrompt::None && btn(ui, "OK") {
                        return Out::Close;
                    }
                    Out::Keep
                })
            })
        },
    )
}

#[derive(Clone, Default)]
struct NameState {
    name: String,
    /// "folder" or "all".
    scope: String,
}

/// Save Current View As a New View: a name and where the view can be used (`view.saveViewAs`).
fn save_view_as(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let start = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| NameState { name: start.clone(), scope: "all".into() },
        |st, app| {
            window(ctx, app, d, "Save Current View As a New View", 400.0, |ui, app| {
                common::field(ui, "Name of new view:", &mut st.name, 220.0);
                ui.label("Can be used on:");
                ui.radio_value(&mut st.scope, "folder".to_owned(), "This folder");
                ui.radio_value(&mut st.scope, "all".to_owned(), mailview::ALL_FOLDERS);
                match ok_cancel(ui, app, !st.name.trim().is_empty()) {
                    Some(true) => {
                        let p = json!({"name": st.name.trim(), "scope": st.scope, "confirm": true});
                        // A refused name (a built-in view's) keeps the dialog open; the error shows as a toast.
                        if app.run("view.saveViewAs", p).is_some() { Out::Close } else { Out::Keep }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct ApplyState {
    picked: Vec<u64>,
    subfolders: bool,
}

fn apply_view(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let src = folder_of(app, d);
    with_state(
        ctx,
        app,
        uid,
        |_: &App| ApplyState::default(),
        |st, app| {
            window(ctx, app, d, "Apply View", 420.0, |ui, app| {
                ui.label(format!(
                    "Apply the view of \"{}\" to these folders:",
                    app.s.mailbox.folder_path(src)
                ));
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(("av", uid))
                            .max_height(240.0)
                            .min_scrolled_height(240.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut folders: Vec<(String, u64)> = app
                                    .s
                                    .mailbox
                                    .folders
                                    .values()
                                    .filter(|f| f.kind == mailcraft_model::FolderKind::Mail && f.id != src)
                                    .map(|f| (app.s.mailbox.folder_path(f.id), f.id.0))
                                    .collect();
                                folders.sort();
                                for (path, id) in folders {
                                    let mut on = st.picked.contains(&id);
                                    if ui.checkbox(&mut on, path).changed() {
                                        if on {
                                            st.picked.push(id);
                                        } else {
                                            st.picked.retain(|x| *x != id);
                                        }
                                    }
                                }
                            });
                    });
                ui.checkbox(&mut st.subfolders, "Apply view to subfolders");
                match ok_cancel(ui, app, !st.picked.is_empty()) {
                    Some(true) => {
                        app.run(
                            "view.applyViewTo",
                            json!({"folders": st.picked, "subfolders": st.subfolders}),
                        );
                        Out::Close
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct QaState {
    first: usize,
    second: usize,
}

/// Quick Actions: the two buttons a hovered message row shows (`view.quickActions`).
fn quick_actions(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let idx = |id: &str| mailview::QUICK_ACTIONS.iter().position(|(k, _)| *k == id).unwrap_or(0);
    let init = |a: &App| {
        let [first, second] = mailview::quick_actions(&a.s);
        QaState { first: idx(first), second: idx(second) }
    };
    let labels: Vec<&str> = mailview::QUICK_ACTIONS.iter().map(|x| x.1).collect();
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Quick Actions", 380.0, |ui, app| {
            muted(ui, app, "Choose the buttons shown on a message when you point at it in the list.");
            ui.horizontal(|ui| {
                ui.label("First button (right edge):");
                common::combo_idx(ui, ("qa1", uid), &mut st.first, &labels, 180.0);
            });
            ui.horizontal(|ui| {
                ui.label("Second button:");
                common::combo_idx(ui, ("qa2", uid), &mut st.second, &labels, 180.0);
            });
            let mut reset = false;
            let r = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Some(false);
                }
                if primary(ui, app, true, "OK") {
                    return Some(true);
                }
                if btn(ui, "Reset") {
                    reset = true;
                }
                None
            });
            if reset {
                let [a, b] = mailview::DEFAULT_QUICK_ACTIONS;
                st.first = idx(a);
                st.second = idx(b);
            }
            match r {
                Some(true) => {
                    let id = |i: usize| mailview::QUICK_ACTIONS.get(i).map_or("none", |x| x.0);
                    app.run("view.quickActions", json!({"actions": [id(st.first), id(st.second)]}));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use crate::App;
    use mailcraft_engine::cmd::view::mail as mailview;
    use mailcraft_engine::sample::sample_session;

    fn harness() -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::dialogs::dialogs(&ctx, app);
            },
            App::new(sample_session()),
        );
        h.run_steps(2);
        h
    }

    fn open(h: &mut Harness<'static, App>, id: &str, p: serde_json::Value) {
        h.state_mut().run(id, p).unwrap();
        h.run_steps(3);
    }

    #[test]
    fn show_columns_adds_a_column() {
        let mut h = harness();
        open(&mut h, "view.addColumns", json!({}));
        h.get_by_label("Cc").click();
        h.run();
        h.get_by_label("Add ->").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        let s = &h.state().s;
        let ids: Vec<&str> = mailview::columns(s, s.selection.folder).iter().map(|c| c.id).collect();
        assert_eq!(ids.last(), Some(&"cc"));
        assert!(h.query_by_label("Show Columns").is_none(), "closed");
    }

    #[test]
    fn save_view_as_for_this_folder() {
        let mut h = harness();
        open(&mut h, "view.saveViewAs", json!({"name": "Team"}));
        h.get_by_label("This folder").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        let s = &h.state().s;
        let saved = mailview::saved_views(s);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved.first().and_then(|v| v.folder), Some(s.selection.folder.0));
        assert_eq!(mailview::current_view_name(s, s.selection.folder), "Team");
    }

    #[test]
    fn manage_views_copy_and_delete() {
        let mut h = harness();
        open(&mut h, "view.manageViews", json!({}));
        assert!(h.query_by_label("Manage All Views").is_some());
        h.get_by_label("Copy...").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        let names: Vec<String> = mailview::saved_views(&h.state().s).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["Copy of Compact"]);
        // The window shrinks once the name prompt closes: let it settle before clicking.
        h.run_steps(6);
        h.get_by_label("Delete").click();
        h.run_steps(2);
        assert!(mailview::saved_views(&h.state().s).is_empty());
        h.get_by_label("Close").click();
        h.run();
        assert!(h.query_by_label("Manage All Views").is_none());
    }

    #[test]
    fn module_manage_views_copy_rename_and_delete() {
        use mailcraft_engine::cmd::view::modules;
        let mut h = harness();
        let notes = h
            .state()
            .s
            .mailbox
            .folders
            .values()
            .find(|f| f.kind == mailcraft_model::FolderKind::Notes)
            .unwrap()
            .id;
        h.state_mut().run("nav.selectFolder", json!({"folder": notes.0})).unwrap();
        open(&mut h, "view.manageViews", json!({}));
        assert!(h.query_by_label("Manage All Views").is_some());
        assert!(h.query_by_label("New...").is_some(), "New is offered");
        h.get_by_label("Copy...").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        let names = |h: &Harness<'static, App>| -> Vec<String> {
            modules::saved_views(&h.state().s, notes).into_iter().map(|v| v.name).collect()
        };
        assert_eq!(names(&h), ["Copy of Icon"]);
        h.run_steps(6);
        h.get_by_label("Rename...").click();
        h.run();
        assert!(h.query_by_label("Rename View").is_some());
        h.get_by_label("Cancel").click();
        h.run_steps(6);
        h.get_by_label("Delete").click();
        h.run_steps(2);
        assert!(names(&h).is_empty());
        h.get_by_label("Close").click();
        h.run();
        assert!(h.query_by_label("Manage All Views").is_none());
    }

    #[test]
    fn filter_and_rules_dialogs_apply() {
        let mut h = harness();
        open(&mut h, "view.customFilter", json!({}));
        h.get_by_label("Only items that are flagged").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        let folder = h.state().s.selection.folder;
        assert!(mailview::custom_filter(&h.state().s, folder).is_some_and(|f| f.flagged));
        open(&mut h, "view.conditionalFormatting", json!({}));
        h.get_by_label("Add").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        assert_eq!(mailview::rules(&h.state().s, folder).len(), mailview::default_rules().len() + 1);
    }

    #[test]
    fn quick_actions_dialog_keeps_the_choice() {
        let mut h = harness();
        h.state_mut().run("view.quickActions", json!({"actions": ["archive", "pin"]})).unwrap();
        open(&mut h, "view.quickActions", json!({}));
        assert!(h.query_by_label("Quick Actions").is_some());
        h.get_by_label("Reset").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(2);
        assert_eq!(mailview::quick_actions(&h.state().s), mailview::DEFAULT_QUICK_ACTIONS);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Module tables (People › Phone and List, the Tasks views): Show Columns, Sort, Filter and Conditional
// Formatting act on the engine's per-view table state (`mailcraft_engine::cmd::view::table`).

/// Show Columns for a module table: the module's columns, shown ones in order (`view.columns`).
fn module_columns(ctx: &Context, app: &mut App, d: &Dlg<'_>, kind: ModuleKind) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let init = |a: &App| ColState {
        shown: table::columns(&a.s, folder).iter().map(|c| c.id.to_owned()).collect(),
        ..Default::default()
    };
    let label = move |id: &str| table::column(kind, id).map_or("", |c| c.label);
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Show Columns", 560.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.label("Available columns:");
                    egui::ScrollArea::vertical().id_salt(("mcols-avail", uid)).max_height(300.0).show(
                        ui,
                        |ui| {
                            for c in
                                table::available(kind).iter().filter(|c| !st.shown.iter().any(|s| s == c.id))
                            {
                                if ui
                                    .selectable_label(st.pick_avail.as_deref() == Some(c.id), c.label)
                                    .clicked()
                                {
                                    st.pick_avail = Some(c.id.to_owned());
                                }
                            }
                        },
                    );
                });
                ui.vertical(|ui| {
                    if btn_en(ui, st.pick_avail.is_some() && st.shown.len() < table::MAX_COLUMNS, "Add ->")
                        && let Some(c) = st.pick_avail.take()
                    {
                        st.shown.push(c);
                        st.pick_shown = Some(st.shown.len().saturating_sub(1));
                    }
                    if btn_en(ui, st.pick_shown.is_some() && st.shown.len() > 1, "<- Remove")
                        && let Some(i) = st.pick_shown.take()
                        && i < st.shown.len()
                    {
                        st.shown.remove(i);
                    }
                    if btn(ui, "Reset") {
                        let view = mailcraft_engine::cmd::view::modules::state(&app.s, folder, kind).view;
                        st.shown =
                            table::default_columns(kind, &view).iter().map(|c| (*c).to_owned()).collect();
                        st.pick_shown = None;
                    }
                });
                ui.vertical(|ui| {
                    ui.label("Show these columns in this order:");
                    for (i, c) in st.shown.iter().enumerate() {
                        if ui.selectable_label(st.pick_shown == Some(i), label(c)).clicked() {
                            st.pick_shown = Some(i);
                        }
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i > 0), "Move Up")
                            && let Some(i) = st.pick_shown
                            && i > 0
                            && i < st.shown.len()
                        {
                            st.shown.swap(i, i - 1);
                            st.pick_shown = Some(i - 1);
                        }
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i + 1 < st.shown.len()), "Move Down")
                            && let Some(i) = st.pick_shown
                            && i + 1 < st.shown.len()
                        {
                            st.shown.swap(i, i + 1);
                            st.pick_shown = Some(i + 1);
                        }
                    });
                });
            });
            match ok_cancel(ui, app, !st.shown.is_empty()) {
                Some(true) => {
                    let ok = app.run("view.columns", json!({"columns": st.shown})).is_some();
                    if ok { Out::Close } else { Out::Keep }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct ModuleSortState {
    /// Index into the choices (0 = the view's own order).
    key: usize,
    desc: bool,
}

/// Sort for a module table: a column (or the view's own order) and the direction (`view.sortBy`).
fn module_sort(ctx: &Context, app: &mut App, d: &Dlg<'_>, kind: ModuleKind) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let cols = table::available(kind);
    let init = |a: &App| match table::sort(&a.s, folder) {
        Some(f) => ModuleSortState {
            key: cols.iter().position(|c| c.id == f.column).map_or(0, |i| i + 1),
            desc: f.descending,
        },
        None => ModuleSortState::default(),
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Sort", 380.0, |ui, app| {
            ui.label("Sort items by");
            let mut labels: Vec<&str> = vec!["(the view's own order)"];
            labels.extend(cols.iter().map(|c| c.label));
            ui.horizontal(|ui| {
                common::combo_idx(ui, ("msortk", uid), &mut st.key, &labels, 200.0);
                ui.add_enabled_ui(st.key > 0, |ui| {
                    ui.vertical(|ui| {
                        ui.radio_value(&mut st.desc, false, "Ascending");
                        ui.radio_value(&mut st.desc, true, "Descending");
                    });
                });
            });
            muted(ui, app, "Items sort inside their groups; Reverse Sort turns the whole list around.");
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let field = st.key.checked_sub(1).and_then(|i| cols.get(i)).map(|c| c.id);
                    let dir = if st.desc { "desc" } else { "asc" };
                    app.run("view.sortBy", json!({"field": field, "dir": dir}));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Filter for a module table: words (in the subject or name, optionally the notes), priority (tasks),
/// flagged, a category and, for tasks, Modified in the last N days (`view.customFilter`).
fn module_filter(ctx: &Context, app: &mut App, d: &Dlg<'_>, kind: ModuleKind) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let tasks = kind == ModuleKind::Tasks;
    let init = |a: &App| {
        let f = table::filter(&a.s, folder).unwrap_or_default();
        let days = if f.days == 0 { String::new() } else { f.days.to_string() };
        FilterState { f, days }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Filter", 460.0, |ui, app| {
            common::heading(ui, app, if tasks { "Tasks" } else { "Contacts" });
            common::field(ui, "Search for the word(s):", &mut st.f.words, 240.0);
            ui.horizontal(|ui| {
                ui.label("In:");
                let (a, b) = if tasks {
                    ("subject field only", "subject field and notes")
                } else {
                    ("name fields only", "name fields and notes")
                };
                ui.radio_value(&mut st.f.in_body, false, a);
                ui.radio_value(&mut st.f.in_body, true, b);
            });
            common::heading(ui, app, "More Choices");
            if tasks {
                ui.horizontal(|ui| {
                    ui.label("Whose priority is:");
                    let mut idx =
                        IMPORTANCE_CHOICES.iter().position(|(k, _)| *k == st.f.importance).unwrap_or(0);
                    let labels: Vec<&str> = IMPORTANCE_CHOICES.iter().map(|x| x.1).collect();
                    if common::combo_idx(ui, ("mf-imp", uid), &mut idx, &labels, 120.0) {
                        st.f.importance = IMPORTANCE_CHOICES.get(idx).map_or("", |x| x.0).to_owned();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Modified in the last");
                    ui.add(egui::TextEdit::singleline(&mut st.days).desired_width(50.0));
                    ui.label("days (empty: any time)");
                });
            }
            let flagged = if tasks { "Only tasks with a start or due date" } else { "Only flagged contacts" };
            ui.checkbox(&mut st.f.flagged, flagged);
            common::field(ui, "Category:", &mut st.f.category, 200.0);
            let days_ok = !tasks || st.days.trim().is_empty() || st.days.trim().parse::<u32>().is_ok();
            if !days_ok {
                ui.colored_label(app.theme.danger, "Enter a number of days.");
            }
            let mut clear = false;
            let r = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Some(false);
                }
                if primary(ui, app, days_ok, "OK") {
                    return Some(true);
                }
                if btn(ui, "Clear All") {
                    clear = true;
                }
                None
            });
            if clear {
                *st = FilterState::default();
            }
            match r {
                Some(true) => {
                    let mut f = CustomFilter {
                        words: st.f.words.clone(),
                        in_body: st.f.in_body,
                        importance: if tasks { st.f.importance.clone() } else { String::new() },
                        flagged: st.f.flagged,
                        category: st.f.category.clone(),
                        ..Default::default()
                    };
                    f.days = if tasks { st.days.trim().parse().unwrap_or(0) } else { 0 };
                    let v = if f.is_empty() {
                        Value::Null
                    } else {
                        serde_json::to_value(&f).unwrap_or(Value::Null)
                    };
                    app.run("view.customFilter", json!({"filter": v}));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

fn module_rule_text(kind: ModuleKind, r: &FormatRule) -> String {
    match (kind, r.kind.as_str()) {
        (ModuleKind::People, "overdue") => "The follow-up due date has passed".into(),
        (_, "overdue") => "The due date has passed and the task is not complete".into(),
        (ModuleKind::People, "completed") => "The follow-up flag is complete".into(),
        (_, "completed") => "The task is complete".into(),
        _ => table::describe_filter(kind, &r.condition),
    }
}

/// Conditional Formatting for a module table: the built-in Overdue and Completed rules (on or off, their
/// font) and custom rules with a condition (`view.conditionalFormatting`).
fn module_rules(ctx: &Context, app: &mut App, d: &Dlg<'_>, kind: ModuleKind) -> Out {
    let uid = d.uid;
    let folder = app.s.selection.folder;
    let tasks = kind == ModuleKind::Tasks;
    let init = |a: &App| RulesState { rules: table::rules(&a.s, folder), pick: Some(0) };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Conditional Formatting", 520.0, |ui, app| {
            ui.label("Rules for this view:");
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ui.set_min_height(100.0);
                    ui.set_width(ui.available_width());
                    for (i, r) in st.rules.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut r.on, "");
                            if ui.selectable_label(st.pick == Some(i), r.name.as_str()).clicked() {
                                st.pick = Some(i);
                            }
                        });
                    }
                },
            );
            let n = st.rules.len();
            let custom = st.pick.and_then(|i| st.rules.get(i)).is_some_and(|r| r.kind == "custom");
            ui.horizontal(|ui| {
                if btn_en(ui, n < mailview::MAX_RULES, "Add") {
                    st.rules.push(FormatRule {
                        name: "Untitled".into(),
                        on: true,
                        kind: "custom".into(),
                        condition: CustomFilter::default(),
                        font: RuleFont { bold: true, ..Default::default() },
                    });
                    st.pick = Some(st.rules.len().saturating_sub(1));
                }
                if btn_en(ui, custom, "Delete")
                    && let Some(i) = st.pick
                    && i < st.rules.len()
                {
                    st.rules.remove(i);
                    st.pick = None;
                }
                if btn_en(ui, st.pick.is_some_and(|i| i > 0), "Move Up")
                    && let Some(i) = st.pick
                    && i > 0
                    && i < n
                {
                    st.rules.swap(i, i - 1);
                    st.pick = Some(i - 1);
                }
                if btn_en(ui, st.pick.is_some_and(|i| i + 1 < n), "Move Down")
                    && let Some(i) = st.pick
                    && i + 1 < n
                {
                    st.rules.swap(i, i + 1);
                    st.pick = Some(i + 1);
                }
            });
            if let Some(r) = st.pick.and_then(|i| st.rules.get_mut(i)) {
                common::heading(ui, app, "Properties of selected rule");
                ui.add_enabled_ui(r.kind == "custom", |ui| {
                    common::field(ui, "Name:", &mut r.name, 260.0);
                });
                ui.horizontal(|ui| {
                    ui.label("Font:");
                    ui.checkbox(&mut r.font.bold, "Bold");
                    ui.checkbox(&mut r.font.italic, "Italic");
                    ui.checkbox(&mut r.font.underline, "Underline");
                    ui.checkbox(&mut r.font.strike, "Strikethrough");
                });
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut idx = RULE_COLORS
                        .iter()
                        .position(|(_, h)| h.eq_ignore_ascii_case(&r.font.color))
                        .unwrap_or(0);
                    let labels: Vec<&str> = RULE_COLORS.iter().map(|x| x.0).collect();
                    if common::combo_idx(ui, ("mcf-color", uid), &mut idx, &labels, 140.0) {
                        r.font.color = RULE_COLORS.get(idx).map_or("", |x| x.1).to_owned();
                    }
                });
                if r.kind == "custom" {
                    ui.label("Condition (the row matches every part that is filled in):");
                    let words = if tasks { "Words in the subject:" } else { "Words in the name:" };
                    common::field(ui, words, &mut r.condition.words, 220.0);
                    if tasks {
                        ui.horizontal(|ui| {
                            ui.label("Priority:");
                            let mut idx = IMPORTANCE_CHOICES
                                .iter()
                                .position(|(k, _)| *k == r.condition.importance)
                                .unwrap_or(0);
                            let labels: Vec<&str> = IMPORTANCE_CHOICES.iter().map(|x| x.1).collect();
                            if common::combo_idx(ui, ("mcf-imp", uid), &mut idx, &labels, 120.0) {
                                r.condition.importance =
                                    IMPORTANCE_CHOICES.get(idx).map_or("", |x| x.0).to_owned();
                            }
                        });
                    }
                    common::field(ui, "Category:", &mut r.condition.category, 200.0);
                    ui.checkbox(
                        &mut r.condition.flagged,
                        if tasks { "With a start or due date" } else { "Flagged" },
                    );
                } else {
                    muted(ui, app, &format!("Condition: {}", module_rule_text(kind, r)));
                    if r.font.color.is_empty() && tasks {
                        muted(ui, app, "Automatic colour: the one set in Options › Tasks.");
                    }
                }
            }
            let mut reset = false;
            let res = footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Some(false);
                }
                if primary(ui, app, true, "OK") {
                    return Some(true);
                }
                if btn(ui, "Reset") {
                    reset = true;
                }
                None
            });
            if reset {
                st.rules = table::default_rules(kind);
                st.pick = Some(0);
            }
            match res {
                Some(true) => {
                    let rules = serde_json::to_value(&st.rules).unwrap_or(Value::Null);
                    if app.run("view.conditionalFormatting", json!({"rules": rules})).is_some() {
                        Out::Close
                    } else {
                        Out::Keep
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}
