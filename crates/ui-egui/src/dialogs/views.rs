//! View dialogs: Advanced View Settings (with Sort, Group By, Filter, Other Settings, Columns and Conditional
//! Formatting), Conversation Settings, Zoom, Manage All Views, Save View As, Apply View, Quick Actions.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, muted, ok_cancel, primary, window, with_state};
use crate::App;
use mailcraft_engine::queries::view_settings;
use mailcraft_model::FolderId;
use mailcraft_model::view::{ArrangeBy, ListFilter, ReadingPanePos, SortOrder, ViewLayout};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "advancedViewSettings" => advanced(ctx, app, d),
        "viewSort" => sort(ctx, app, d),
        "viewGroupBy" => group_by(ctx, app, d),
        "viewFilter" => filter(ctx, app, d),
        "viewOtherSettings" => other(ctx, app, d),
        "viewFormat" => format(ctx, app, d),
        "showColumns" => columns(ctx, app, d),
        "conversationSettings" => conversation(ctx, app, d),
        "zoom" => zoom(ctx, app, d),
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

fn filter_label(f: ListFilter) -> &'static str {
    match f {
        ListFilter::All => "Off",
        ListFilter::Unread => "Unread",
        ListFilter::Flagged => "Flagged",
        ListFilter::Mentioned => "Mentioned",
    }
}

fn advanced(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let folder = folder_of(app, d);
    let v = view_settings(&app.s, folder);
    let title = format!("Advanced View Settings: {}", layout_name(v.layout));
    let p = json!({"folder": folder.0});
    window(ctx, app, d, &title, 520.0, |ui, app| {
        let mut out = Out::Keep;
        let cols =
            common::pref(ctx, &format!("columns.{}", folder.0)).and_then(|v| v.as_array().map(Vec::len));
        let order = match v.order {
            SortOrder::Descending => "Descending",
            SortOrder::Ascending => "Ascending",
        };
        let rows: [(&str, &str, String); 7] = [
            ("Columns...", "showColumns", match cols {
                Some(n) => format!("{n} columns"),
                None => "Importance, Reminder, Icon, Flag Status, Attachment, From, Subject, Received, Size, Categories".into(),
            }),
            ("Group By...", "viewGroupBy", if v.show_in_groups { format!("{} (automatic)", v.arrange_by.label()) } else { "Off".into() }),
            ("Sort...", "viewSort", format!("{} ({order})", v.arrange_by.label())),
            ("Filter...", "viewFilter", filter_label(v.filter).into()),
            ("Other Settings...", "viewOtherSettings", "Fonts and other Table View settings".into()),
            ("Conditional Formatting...", "viewFormat", "User defined fonts on each message".into()),
            ("Format Columns...", "viewFormat", "Specify the display formats for each field".into()),
        ];
        egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(8.0).show(
            ui,
            |ui| {
                ui.label(RichText::new("Description").strong());
                egui::Grid::new(("avs", d.uid)).num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    for (label, dlg, desc) in &rows {
                        if ui.add(egui::Button::new(*label).min_size(egui::vec2(170.0, 24.0))).clicked() {
                            let mut params = p.clone();
                            if *label == "Format Columns..."
                                && let Some(o) = params.as_object_mut()
                            {
                                o.insert("kind".into(), json!("columns"));
                            }
                            out = Out::open(dlg, params);
                        }
                        ui.label(RichText::new(desc).color(app.theme.text_muted));
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
        ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close)
    })
}

#[derive(Clone, Default)]
struct SortState {
    key: usize,
    desc: bool,
}

fn sort(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
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

const FILTERS: [(&str, &str); 7] = [
    ("all", "All items (no filter)"),
    ("unread", "Unread"),
    ("hasAttachments", "Has attachments"),
    ("flagged", "Flagged"),
    ("mentioned", "Mentioned"),
    ("important", "Important"),
    ("toOrCcMe", "Sent to me (To or Cc)"),
];

#[derive(Clone, Default)]
struct FilterState {
    pick: usize,
}

fn filter(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = folder_of(app, d);
    let init = |a: &App| {
        let key = match view_settings(&a.s, folder).filter {
            ListFilter::All => "all",
            ListFilter::Unread => "unread",
            ListFilter::Flagged => "flagged",
            ListFilter::Mentioned => "mentioned",
        };
        FilterState { pick: FILTERS.iter().position(|f| f.0 == key).unwrap_or(0) }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Filter", 360.0, |ui, app| {
            ui.label("Show only:");
            for (i, (_, l)) in FILTERS.iter().enumerate() {
                ui.radio_value(&mut st.pick, i, *l);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let kind = FILTERS.get(st.pick).map_or("all", |f| f.0);
                    app.run("view.filter", json!({"kind": kind}));
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

const FORMAT_RULES: [&str; 5] =
    ["Unread messages", "Unread group messages", "Expired email", "Overdue email", "Messages from me"];

#[derive(Clone, Default)]
struct FormatState {
    on: Vec<bool>,
}

fn format(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let columns = d.str("kind") == "columns";
    let init = |_: &App| FormatState {
        on: FORMAT_RULES
            .iter()
            .map(|r| common::pref(ctx, &format!("format.{r}")).and_then(|v| v.as_bool()).unwrap_or(true))
            .collect(),
    };
    with_state(ctx, app, uid, init, |st, app| {
        let title = if columns { "Format Columns" } else { "Conditional Formatting" };
        window(ctx, app, d, title, 400.0, |ui, app| {
            if columns {
                muted(
                    ui,
                    app,
                    "Columns use their default formats: dates as short date and time, sizes in KB.",
                );
                return ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close);
            }
            ui.label("Rules for this view:");
            for (r, on) in FORMAT_RULES.iter().zip(st.on.iter_mut()) {
                ui.checkbox(on, *r);
            }
            muted(ui, app, "Unread messages show in bold with an accent bar.");
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    for (r, on) in FORMAT_RULES.iter().zip(st.on.iter()) {
                        common::set_pref(ctx, &format!("format.{r}"), json!(on));
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

const ALL_COLUMNS: [&str; 16] = [
    "Importance",
    "Reminder",
    "Icon",
    "Flag Status",
    "Attachment",
    "From",
    "Subject",
    "Received",
    "Size",
    "Categories",
    "To",
    "Cc",
    "Sent",
    "Mention",
    "Sensitivity",
    "In Folder",
];

#[derive(Clone, Default)]
struct ColState {
    shown: Vec<String>,
    pick_avail: Option<String>,
    pick_shown: Option<usize>,
}

fn columns(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = folder_of(app, d);
    let key = format!("columns.{}", folder.0);
    let init = |_: &App| ColState {
        shown: common::pref(ctx, &key)
            .and_then(|v| v.as_array().cloned())
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_else(|| ALL_COLUMNS.iter().take(10).map(|s| (*s).to_owned()).collect()),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Show Columns", 520.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    ui.label("Available columns:");
                    for c in ALL_COLUMNS.iter().filter(|c| !st.shown.iter().any(|s| s == *c)) {
                        if ui.selectable_label(st.pick_avail.as_deref() == Some(*c), *c).clicked() {
                            st.pick_avail = Some((*c).to_owned());
                        }
                    }
                });
                ui.vertical(|ui| {
                    if btn_en(ui, st.pick_avail.is_some(), "Add ->")
                        && let Some(c) = st.pick_avail.take()
                    {
                        st.shown.push(c);
                    }
                    if btn_en(ui, st.pick_shown.is_some(), "<- Remove")
                        && let Some(i) = st.pick_shown.take()
                        && i < st.shown.len()
                    {
                        st.shown.remove(i);
                    }
                });
                ui.vertical(|ui| {
                    ui.label("Show these columns in this order:");
                    for (i, c) in st.shown.iter().enumerate() {
                        if ui.selectable_label(st.pick_shown == Some(i), c).clicked() {
                            st.pick_shown = Some(i);
                        }
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i > 0), "Move Up")
                            && let Some(i) = st.pick_shown
                        {
                            st.shown.swap(i, i - 1);
                            st.pick_shown = Some(i - 1);
                        }
                        if btn_en(ui, st.pick_shown.is_some_and(|i| i + 1 < st.shown.len()), "Move Down")
                            && let Some(i) = st.pick_shown
                        {
                            st.shown.swap(i, i + 1);
                            st.pick_shown = Some(i + 1);
                        }
                    });
                });
            });
            match ok_cancel(ui, app, !st.shown.is_empty()) {
                Some(true) => {
                    common::set_pref(ctx, &key, json!(st.shown));
                    Out::Close
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

#[derive(Clone, Default)]
struct ViewsState {
    selected: String,
    this_folder: bool,
}

fn manage_views(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let mut views: Vec<String> = d.arr("views").iter().filter_map(Value::as_str).map(str::to_owned).collect();
    if views.is_empty() {
        views = vec!["Compact".into(), "Single".into(), "Preview".into()];
    }
    let saved: Vec<String> = common::pref(ctx, "views.saved")
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.iter().filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_owned)).collect())
        .unwrap_or_default();
    let current = d.str("current");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| ViewsState { selected: current.clone(), this_folder: false },
        |st, app| {
            window(ctx, app, d, "Manage All Views", 500.0, |ui, app| {
                let mut out = Out::Keep;
                ui.horizontal_top(|ui| {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_width(300.0);
                            ui.set_min_height(200.0);
                            egui::Grid::new(("mv", uid)).num_columns(2).show(ui, |ui| {
                                ui.label(RichText::new("View Name").strong());
                                ui.label(RichText::new("Can Be Used On").strong());
                                ui.end_row();
                                for v in views.iter().chain(saved.iter()) {
                                    if ui.selectable_label(st.selected.eq_ignore_ascii_case(v), v).clicked() {
                                        st.selected = v.clone();
                                    }
                                    ui.label("All Mail and Post folders");
                                    ui.end_row();
                                }
                            });
                        });
                    ui.vertical(|ui| {
                        ui.add_enabled(false, egui::Button::new("New...").min_size(egui::vec2(90.0, 24.0)));
                        if btn(ui, "Copy...") {
                            out =
                                Out::open("saveViewAs", json!({"name": format!("Copy of {}", st.selected)}));
                        }
                        if btn(ui, "Modify...") {
                            app.run("view.viewSettings", json!({}));
                        }
                        if btn(ui, "Reset") {
                            app.run("view.resetView", json!({}));
                        }
                    });
                });
                ui.checkbox(&mut st.this_folder, "Only show views created for this folder");
                if !matches!(out, Out::Keep) {
                    return out;
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if primary(ui, app, !st.selected.is_empty(), "Apply View") {
                        if views.iter().any(|v| v.eq_ignore_ascii_case(&st.selected)) {
                            app.run("view.changeView", json!({"name": st.selected.to_lowercase()}));
                        }
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
    error: String,
}

fn save_view_as(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let start = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| NameState { name: start.clone(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Save Current View As a New View", 380.0, |ui, app| {
                common::field(ui, "Name of new view:", &mut st.name, 220.0);
                ui.label("Can be used on: All Mail and Post folders");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel(ui, app, !st.name.trim().is_empty()) {
                    Some(true) => {
                        let name = st.name.trim();
                        if ["compact", "single", "preview"].contains(&name.to_lowercase().as_str()) {
                            st.error = format!("A view named \"{name}\" already exists.");
                            return Out::Keep;
                        }
                        let settings = d.get("settings").cloned().unwrap_or_else(|| {
                            serde_json::to_value(view_settings(&app.s, app.s.selection.folder))
                                .unwrap_or(Value::Null)
                        });
                        let mut prefs = common::prefs(ctx);
                        let list = prefs.values.entry("views.saved").or_insert_with(|| json!([]));
                        if let Some(a) = list.as_array_mut() {
                            a.retain(|x| x.get("name").and_then(Value::as_str) != Some(name));
                            a.push(json!({"name": name, "settings": settings}));
                        }
                        common::set_prefs(ctx, prefs);
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

const QUICK_ACTIONS: [&str; 8] = [
    "None",
    "Delete",
    "Archive",
    "Flag / Unflag",
    "Mark as Read / Unread",
    "Move",
    "Pin / Unpin",
    "Categorize",
];

#[derive(Clone, Default)]
struct QaState {
    first: String,
    second: String,
}

fn quick_actions(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let s = |k: &str, def: &str| {
        common::pref(ctx, k).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| def.to_owned())
    };
    let init = |_: &App| QaState {
        first: s("quickActions.1", "Flag / Unflag"),
        second: s("quickActions.2", "Delete"),
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Quick Actions", 360.0, |ui, app| {
            muted(ui, app, "Choose the buttons shown on a message when you point at it in the list.");
            ui.horizontal(|ui| {
                ui.label("First action:");
                common::combo(ui, ("qa1", uid), &mut st.first, &QUICK_ACTIONS, 180.0);
            });
            ui.horizontal(|ui| {
                ui.label("Second action:");
                common::combo(ui, ("qa2", uid), &mut st.second, &QUICK_ACTIONS, 180.0);
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    common::set_pref(ctx, "quickActions.1", json!(st.first));
                    common::set_pref(ctx, "quickActions.2", json!(st.second));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}
