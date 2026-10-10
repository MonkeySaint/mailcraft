//! Live state for ribbon definitions: toggle predicates (`on=`), dynamic menu/gallery items (`dyn=`),
//! parameter templates (`$on`, `$value`, `$name`, …), per-surface parameter context, enablement and running
//! commands through [`App::run`].

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::defs::{CmdRef, Item, ItemKind};
use crate::{App, Surface};
use mailcraft_engine::Module;
use mailcraft_model::{
    ArrangeBy, BodyFormat, FolderPaneState, Importance, ListFilter, ReadingPanePos, ViewLayout,
};

/// Ribbon-local UI state kept in egui temp memory (never in the session).
#[derive(Clone, Debug, Default)]
pub struct Mem {
    /// Booleans for toggles whose state the engine does not keep (`on=mem:<key>`).
    pub flags: BTreeMap<String, bool>,
    /// Text typed into edit boxes, by control key.
    pub text: BTreeMap<String, String>,
    /// Horizontal scroll offset of the Classic ribbon, per surface key.
    pub scroll: BTreeMap<String, f32>,
    /// A menu to open on the next frame (set by keytips), as "<tab>/<control key>".
    pub open_request: Option<String>,
}

pub fn mem_id(surface: Surface) -> egui::Id {
    egui::Id::new(("mailcraft-ribbon-mem", super::surface_key(surface)))
}

pub fn load_mem(ctx: &egui::Context, surface: Surface) -> Mem {
    ctx.data(|d| d.get_temp::<Mem>(mem_id(surface))).unwrap_or_default()
}

pub fn store_mem(ctx: &egui::Context, surface: Surface, mem: Mem) {
    ctx.data_mut(|d| d.insert_temp(mem_id(surface), mem));
}

/// `mem:<key>` or `mem:<key>=1` (default on). Keys `family.member` form radio groups: once one member was
/// chosen, the others' defaults no longer apply.
fn mem_flag(mem: &Mem, spec: &str) -> bool {
    let (key, default) = match spec.split_once('=') {
        Some((k, v)) => (k, v == "1"),
        None => (spec, false),
    };
    if let Some(v) = mem.flags.get(key) {
        return *v;
    }
    if let Some((family, _)) = key.split_once('.')
        && mem.flags.contains_key(&format!("{family}.__set"))
    {
        return false;
    }
    default
}

/// Updates a `mem:` flag after its command ran: plain flags flip, radio members become the chosen one.
pub fn flip_mem(mem: &mut Mem, spec: &str) {
    let now = mem_flag(mem, spec);
    let key = spec.split_once('=').map_or(spec, |(k, _)| k);
    if let Some((family, _)) = key.split_once('.') {
        let prefix = format!("{family}.");
        mem.flags.retain(|k, _| !k.starts_with(&prefix));
        mem.flags.insert(format!("{family}.__set"), true);
        mem.flags.insert(key.to_owned(), true);
        return;
    }
    mem.flags.insert(key.to_owned(), !now);
}

fn token_on(search: &str, term: &str) -> bool {
    search.split_whitespace().any(|t| t.eq_ignore_ascii_case(term))
}

fn current_view(app: &App) -> mailcraft_model::ViewSettings {
    mailcraft_engine::queries::view_settings(&app.s, app.s.selection.folder)
}

fn focused_mail(app: &App, surface: Surface) -> Option<&mailcraft_model::MailItem> {
    let id = match surface {
        Surface::Read(id) => mailcraft_model::ItemId(id),
        _ => app.s.selection.focus.or_else(|| app.s.selection.items.first().copied())?,
    };
    app.s.mailbox.mail.get(&id)
}

fn compose(app: &App, surface: Surface) -> Option<&mailcraft_engine::compose::ComposeState> {
    match surface {
        Surface::Compose(id) => app.s.composes.get(&id),
        _ => None,
    }
}

/// Whether a state predicate is on. Unknown predicates are off.
pub fn is_on(app: &App, surface: Surface, mem: &Mem, spec: &str) -> bool {
    let (name, arg) = spec.split_once(':').unwrap_or((spec, ""));
    let v = &app.s.view;
    match name {
        "mem" => mem_flag(mem, arg),
        "rp" => {
            let pos = current_view(app).reading_pane;
            matches!(
                (arg, pos),
                ("right", ReadingPanePos::Right)
                    | ("bottom", ReadingPanePos::Bottom)
                    | ("off", ReadingPanePos::Off)
            )
        }
        "fp" => matches!(
            (arg, v.folder_pane),
            ("normal", FolderPaneState::Normal)
                | ("minimized", FolderPaneState::Minimized)
                | ("off", FolderPaneState::Off)
        ),
        "favorites" => v.favorites_visible,
        "favorite" => {
            app.s.mailbox.folders.get(&app.s.selection.folder).is_some_and(|f| f.favorite_pos.is_some())
        }
        "conversations" => current_view(app).conversations,
        "focused" => current_view(app).focused_inbox,
        "reverse" => current_view(app).order == mailcraft_model::SortOrder::Ascending,
        "tighter" => current_view(app).tighter_spacing,
        "groups" => current_view(app).show_in_groups,
        "preview" => arg.parse::<u8>().is_ok_and(|n| current_view(app).preview_lines == n),
        "layout" => matches!(
            (arg, current_view(app).layout),
            ("compact", ViewLayout::Compact)
                | ("single", ViewLayout::Single)
                | ("preview", ViewLayout::Preview)
        ),
        "arrange" => arrange_key(current_view(app).arrange_by) == arg,
        "filter" => matches!(
            (arg, current_view(app).filter),
            ("unread", ListFilter::Unread)
                | ("flagged", ListFilter::Flagged)
                | ("mentioned", ListFilter::Mentioned)
        ),
        "tok" => token_on(&v.search, arg),
        "scope" => scope_name(v.search_scope) == arg,
        "ignored" => focused_mail(app, surface).is_some_and(|m| m.ignored),
        "cat" => focused_mail(app, surface)
            .is_some_and(|m| m.categories.iter().any(|c| c.eq_ignore_ascii_case(arg))),
        "cal" => {
            app.ui.calendar_view.eq_ignore_ascii_case(arg)
                || (app.ui.calendar_view.is_empty() && arg == "month")
        }
        _ => compose(app, surface).is_some_and(|c| compose_on(c, name, arg)),
    }
}

fn compose_on(c: &mailcraft_engine::compose::ComposeState, name: &str, arg: &str) -> bool {
    let st = c.style_at_caret();
    let on = |b: Option<bool>| b.unwrap_or(false);
    match name {
        "bold" => on(st.bold),
        "italic" => on(st.italic),
        "underline" => on(st.underline),
        "strike" => on(st.strike),
        "sub" => on(st.subscript),
        "sup" => on(st.superscript),
        "align" => {
            use mailcraft_model::rich::Align;
            matches!(
                (arg, c.para_at_caret().align),
                ("left", Align::Start)
                    | ("center", Align::Center)
                    | ("right", Align::End)
                    | ("justify", Align::Justify)
            )
        }
        "bullets" => {
            matches!(c.list_at_caret(), Some((_, mailcraft_model::rich::ListKind::Bullet)))
        }
        "numbering" => {
            matches!(c.list_at_caret(), Some((_, mailcraft_model::rich::ListKind::Numbered { .. })))
        }
        "imp" => matches!((arg, c.importance), ("high", Importance::High) | ("low", Importance::Low)),
        "fmt" => matches!(
            (arg, c.format),
            ("html", BodyFormat::Html) | ("plain", BodyFormat::PlainText) | ("rich", BodyFormat::RichText)
        ),
        "bcc" => c.show_bcc,
        "from" => c.show_from,
        "rr" => c.request_read_receipt,
        "dr" => c.request_delivery_receipt,
        "encrypt" => c.encrypt,
        "delay" => c.delay_until.is_some(),
        "replyto" => !c.reply_to.trim().is_empty(),
        "painter" => c.painter.is_some(),
        "marks" => c.editor.show_marks,
        "gridlines" => c.editor.show_gridlines,
        "selobj" => c.editor.select_objects,
        "selpane" => c.editor.selection_pane,
        "hc" => c.editor.high_contrast_only,
        "lightcanvas" => c.light_canvas,
        "vote" => c.voting.eq_ignore_ascii_case(arg),
        "save" => {
            use mailcraft_engine::compose::SaveSentTo;
            matches!((arg, &c.save_sent), ("default", SaveSentTo::Default) | ("none", SaveSentTo::DoNotSave))
        }
        _ => false,
    }
}

/// Current value shown by a combo (`on=value:font|size`).
pub fn combo_value(app: &App, surface: Surface, spec: Option<&str>) -> String {
    let Some(c) = compose(app, surface) else {
        return String::new();
    };
    let st = c.style_at_caret();
    match spec {
        Some("value:font") => st
            .font_family
            .clone()
            .or_else(|| c.body.base.font_family.clone())
            .unwrap_or_else(|| mailcraft_fonts::BODY_FAMILY.to_owned()),
        Some("value:size") => {
            let pt = st.size_pt.or(c.body.base.size_pt).unwrap_or(mailcraft_fonts::BODY_SIZE_PT);
            if (pt.fract()).abs() < 0.01 { format!("{pt:.0}") } else { format!("{pt:.1}") }
        }
        _ => String::new(),
    }
}

pub fn arrange_key(a: ArrangeBy) -> &'static str {
    match a {
        ArrangeBy::Date => "date",
        ArrangeBy::From => "from",
        ArrangeBy::To => "to",
        ArrangeBy::Categories => "categories",
        ArrangeBy::FlagStatus => "flagStatus",
        ArrangeBy::FlagStartDate => "flagStart",
        ArrangeBy::FlagDueDate => "flagDue",
        ArrangeBy::Size => "size",
        ArrangeBy::Subject => "subject",
        ArrangeBy::Type => "type",
        ArrangeBy::Attachments => "attachments",
        ArrangeBy::Account => "account",
        ArrangeBy::Importance => "importance",
    }
}

fn scope_name(sc: mailcraft_engine::SearchScope) -> &'static str {
    use mailcraft_engine::SearchScope as S;
    match sc {
        S::AllMailboxes => "allMailboxes",
        S::CurrentMailbox => "currentMailbox",
        S::CurrentFolder => "currentFolder",
        S::Subfolders => "subfolders",
        S::AllOutlookItems => "allItems",
    }
}

/// Built-in paragraph/character styles offered by the Styles gallery.
pub const STYLES: [&str; 16] = [
    "Normal",
    "No Spacing",
    "Heading 1",
    "Heading 2",
    "Title",
    "Subtitle",
    "Subtle Emphasis",
    "Emphasis",
    "Intense Emphasis",
    "Strong",
    "Quote",
    "Intense Quote",
    "Subtle Reference",
    "Intense Reference",
    "Book Title",
    "List Paragraph",
];

/// One expanded dynamic item.
#[derive(Clone, Debug, PartialEq)]
pub struct DynItem {
    pub label: String,
    pub icon: &'static str,
    pub swatch: Option<[u8; 3]>,
    /// Template variables (`$id`, `$name`, `$key`).
    pub vars: Vec<(&'static str, Value)>,
    /// Checked state.
    pub on: bool,
}

/// Names of the dynamic sources (for tests).
#[cfg(test)]
pub const SOURCES: [&str; 8] = [
    "quicksteps",
    "categories",
    "accounts",
    "arrange",
    "styles",
    "views:people",
    "views:tasks",
    "views:notes",
];

/// Items of a dynamic source.
pub fn dyn_items(app: &App, surface: Surface, source: &str) -> Vec<DynItem> {
    let plain = |label: &str, icon: &'static str| DynItem {
        label: label.to_owned(),
        icon,
        swatch: None,
        vars: vec![("$name", json!(label))],
        on: false,
    };
    match source {
        "quicksteps" => app
            .s
            .mailbox
            .quick_steps
            .iter()
            .map(|q| DynItem {
                label: q.name.clone(),
                icon: "quick-steps",
                swatch: None,
                vars: vec![("$id", json!(q.id)), ("$name", json!(q.name))],
                on: false,
            })
            .collect(),
        "categories" => {
            let item = focused_mail(app, surface);
            app.s
                .mailbox
                .categories
                .iter()
                .map(|c| {
                    let rgb = c.color.rgb();
                    DynItem {
                        label: c.name.clone(),
                        icon: "category-tag",
                        swatch: Some([rgb.r, rgb.g, rgb.b]),
                        vars: vec![("$name", json!(c.name))],
                        on: item
                            .is_some_and(|m| m.categories.iter().any(|x| x.eq_ignore_ascii_case(&c.name))),
                    }
                })
                .collect()
        }
        "accounts" => app
            .s
            .mailbox
            .accounts
            .iter()
            .map(|a| {
                let name =
                    if a.address.trim().is_empty() { a.display_name.clone() } else { a.address.clone() };
                let label =
                    if a.display_name.trim().is_empty() { name.clone() } else { a.display_name.clone() };
                DynItem {
                    label,
                    icon: "account",
                    swatch: None,
                    vars: vec![("$name", json!(name))],
                    on: false,
                }
            })
            .collect(),
        "arrange" => {
            let cur = current_view(app).arrange_by;
            ArrangeBy::ALL
                .iter()
                .map(|a| DynItem {
                    label: a.label().to_owned(),
                    icon: "sort",
                    swatch: None,
                    vars: vec![("$key", json!(arrange_key(*a)))],
                    on: *a == cur,
                })
                .collect()
        }
        "styles" => STYLES.iter().map(|s| plain(s, "font-color")).collect(),
        "views:people" => {
            ["People", "Business Card", "Card", "Phone", "List"].iter().map(|s| plain(s, "people")).collect()
        }
        "views:tasks" => [
            "Detailed",
            "Simple List",
            "To-Do List",
            "Prioritized",
            "Active",
            "Completed",
            "Today",
            "Next 7 Days",
            "Overdue",
            "Assigned",
        ]
        .iter()
        .map(|s| plain(s, "tasks"))
        .collect(),
        "views:notes" => ["Icon", "Notes List", "Last 7 Days"].iter().map(|s| plain(s, "notes")).collect(),
        _ => Vec::new(),
    }
}

/// A resolved, runnable action.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub cmd: String,
    pub params: Value,
    /// `mem:` flag to flip after a successful run.
    pub flip: Option<String>,
}

/// Fills `$…` placeholders in a params template.
pub fn fill(
    template: &str,
    on: bool,
    value: Option<&str>,
    vars: &[(&'static str, Value)],
    app: &App,
) -> Value {
    let mut v: Value = if template.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(template).unwrap_or_else(|_| Value::Object(Map::new()))
    };
    replace(&mut v, &|s: &str| -> Option<Value> {
        match s {
            "$on" => Some(Value::Bool(on)),
            "$!on" => Some(Value::Bool(!on)),
            "$folder" => Some(json!(app.s.selection.folder.0.to_string())),
            "$value" => {
                let text = value.unwrap_or_default().trim();
                Some(match text.parse::<f64>() {
                    Ok(n) if n.is_finite() => json!(n),
                    _ => json!(text),
                })
            }
            _ => vars.iter().find(|(k, _)| *k == s).map(|(_, v)| v.clone()),
        }
    });
    v
}

fn replace(v: &mut Value, f: &dyn Fn(&str) -> Option<Value>) {
    match v {
        Value::String(s) => {
            if let Some(n) = f(s) {
                *v = n;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| replace(x, f)),
        Value::Object(o) => o.values_mut().for_each(|x| replace(x, f)),
        _ => {}
    }
}

/// The action of a command reference (toggle-aware).
pub fn action(
    app: &App,
    cmd: &CmdRef,
    on: bool,
    value: Option<&str>,
    vars: &[(&'static str, Value)],
) -> Action {
    Action { cmd: cmd.id.clone(), params: fill(&cmd.params, on, value, vars, app), flip: None }
}

/// Adds the surface context to params: `{"compose": id}` in compose windows, `{"ids": [item]}` / `{"id":
/// item}` in read windows, the selected module item in Calendar/People/Tasks/Notes.
pub fn with_context(app: &App, surface: Surface, cmd: &str, params: Value) -> Value {
    let mut obj = match params {
        Value::Object(o) => o,
        Value::Null => Map::new(),
        other => return other,
    };
    let spec_params = app.s.registry.get(cmd).map(|s| s.params).unwrap_or_default();
    let takes = |k: &str| spec_params.contains(&format!("\"{k}\""));
    let area = cmd.split('.').next().unwrap_or_default();
    match surface {
        Surface::Compose(id) => {
            if matches!(area, "compose" | "format" | "insert" | "review") || takes("compose") {
                obj.entry("compose").or_insert(json!(id));
            }
        }
        Surface::Read(item) => {
            if takes("ids") {
                obj.entry("ids").or_insert(json!([item]));
            } else if takes("id") && area == "review" {
                obj.entry("id").or_insert(json!(item));
            }
        }
        Surface::Main => {
            let module_area = match app.s.view.module {
                Module::Calendar => "calendar",
                Module::People => "people",
                Module::Tasks => "tasks",
                Module::Notes => "notes",
                _ => "",
            };
            if let Some(sel) = app.ui.module_selection
                && !module_area.is_empty()
                && area == module_area
            {
                if takes("ids") {
                    obj.entry("ids").or_insert(json!([sel]));
                } else if takes("id") {
                    obj.entry("id").or_insert(json!(sel));
                }
            }
        }
    }
    Value::Object(obj)
}

/// Why a command is disabled on this surface (`None` = enabled).
pub fn disabled(app: &App, surface: Surface, cmd: &str) -> Option<&'static str> {
    if cmd.is_empty() {
        return None;
    }
    let Some(spec) = app.s.registry.get(cmd) else {
        return Some("not available");
    };
    // In a read window the command targets the window's item, not the main selection.
    if let Surface::Read(item) = surface
        && (spec.params.contains("\"ids\"") || spec.params.contains("\"id\""))
    {
        return if app.s.mailbox.mail.contains_key(&mailcraft_model::ItemId(item)) {
            None
        } else {
            Some("the message is no longer available")
        };
    }
    app.s.enabled(cmd)
}

/// Runs an action on a surface; applies UI-side effects of a few results. Never panics.
pub fn run(ctx: &egui::Context, app: &mut App, surface: Surface, mem: &mut Mem, act: &Action) {
    if act.cmd.is_empty() {
        return;
    }
    // Read-window commands without an ids/id parameter act on the main selection, and the engine checks
    // enablement against the main selection even when ids are given: select the window's item first.
    if let Surface::Read(item) = surface {
        let takes_ids = app
            .s
            .registry
            .get(&act.cmd)
            .is_some_and(|s| s.params.contains("\"ids\"") || s.params.contains("\"id\""));
        let selected = app.s.selection.items.iter().any(|i| i.0 == item);
        let blocked = app.s.enabled(&act.cmd).is_some();
        let exists = app.s.mailbox.mail.contains_key(&mailcraft_model::ItemId(item));
        if (!takes_ids || blocked) && !selected && exists && act.cmd.starts_with("mail.") {
            let _ = app.run("nav.selectItem", json!({"id": item}));
        }
    }
    let params = with_context(app, surface, &act.cmd, act.params.clone());
    // Calendar commands go through the modules area so the calendar's view state goes in and comes back.
    let result = if surface == Surface::Main && act.cmd.starts_with("calendar.") {
        crate::modules::calendar_command(ctx, app, &act.cmd, params)
    } else {
        app.run(&act.cmd, params)
    };
    let Some(result) = result else {
        return;
    };
    if let Some(flag) = &act.flip {
        flip_mem(mem, flag);
    }
    match act.cmd.as_str() {
        "search.close" => {
            app.ui.search_active = false;
            app.ui.search_text.clear();
        }
        "calendar.arrange" | "calendar.today" | "calendar.next7Days" | "calendar.goToDate" => {
            if let Some(mode) = result.pointer("/view/mode").and_then(Value::as_str) {
                app.ui.calendar_view = mode.to_owned();
            }
        }
        _ => {}
    }
}

/// Expands a menu's entries, resolving dynamic sources into concrete command items.
pub fn expand_items(app: &App, surface: Surface, mem: &Mem, items: &[Item]) -> Vec<Resolved> {
    let mut out = Vec::new();
    for it in items {
        match it.kind {
            ItemKind::Sep => out.push(Resolved::Sep),
            ItemKind::Sub => out.push(Resolved::Sub {
                label: it.label.clone(),
                icon: it.icon.clone(),
                menu: it.target.clone(),
                key: it.key.clone(),
            }),
            ItemKind::Dyn => {
                for d in dyn_items(app, surface, &it.target) {
                    let action = action(app, &it.cmd, d.on, None, &d.vars);
                    out.push(Resolved::Cmd {
                        label: d.label,
                        icon: d.icon.to_owned(),
                        swatch: d.swatch,
                        key: String::new(),
                        on: Some(d.on),
                        disabled: disabled(app, surface, &it.cmd.id),
                        action,
                    });
                }
            }
            ItemKind::Cmd => {
                let on = it.on.as_deref().map(|s| is_on(app, surface, mem, s));
                let mut action = action(app, &it.cmd, on.unwrap_or(false), None, &[]);
                action.flip = it.on.as_deref().and_then(|s| s.strip_prefix("mem:")).map(str::to_owned);
                out.push(Resolved::Cmd {
                    label: it.label.clone(),
                    icon: it.icon.clone(),
                    swatch: it.swatch,
                    key: it.key.clone(),
                    on,
                    disabled: disabled(app, surface, &it.cmd.id),
                    action,
                });
            }
        }
    }
    // No leading, trailing or doubled separators (dynamic sources may be empty).
    let mut cleaned: Vec<Resolved> = Vec::with_capacity(out.len());
    for r in out {
        if matches!(r, Resolved::Sep) && matches!(cleaned.last(), None | Some(Resolved::Sep)) {
            continue;
        }
        cleaned.push(r);
    }
    if matches!(cleaned.last(), Some(Resolved::Sep)) {
        cleaned.pop();
    }
    cleaned
}

/// A menu entry ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    Cmd {
        label: String,
        icon: String,
        swatch: Option<[u8; 3]>,
        key: String,
        /// `Some` for checkable entries.
        on: Option<bool>,
        disabled: Option<&'static str>,
        action: Action,
    },
    Sub {
        label: String,
        icon: String,
        menu: String,
        key: String,
    },
    Sep,
}
