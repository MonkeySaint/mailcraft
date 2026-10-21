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

/// Whether `term` is one of the query's terms (quoted values with spaces count as one term).
fn token_on(search: &str, term: &str) -> bool {
    mailcraft_engine::cmd::search::terms(search).iter().any(|t| t.eq_ignore_ascii_case(term))
}

fn current_view(app: &App) -> mailcraft_model::ViewSettings {
    mailcraft_engine::queries::view_settings(&app.s, app.s.selection.folder)
}

/// The selected folder's module view (contacts, tasks and notes folders).
fn module_here(
    app: &App,
) -> Option<(
    mailcraft_engine::cmd::view::modules::ModuleKind,
    mailcraft_engine::cmd::view::modules::ModuleView,
)> {
    mailcraft_engine::cmd::view::modules::current(&app.s, app.s.selection.folder)
}

/// The module item the main window has selected (People, Tasks, Notes).
fn module_item(app: &App, surface: Surface) -> Option<mailcraft_model::ItemId> {
    if surface != Surface::Main
        || !matches!(app.s.view.module, Module::People | Module::Tasks | Module::Notes)
    {
        return None;
    }
    app.ui.module_selection.map(mailcraft_model::ItemId)
}

/// Tags state of the selected contact or task (`pim:private`, `pim:high`, `pim:low`, `pim:flagged`).
fn pim_on(app: &App, surface: Surface, arg: &str) -> bool {
    let Some(id) = module_item(app, surface) else { return false };
    let mb = &app.s.mailbox;
    if let Some(t) = mb.tasks.get(&id) {
        return match arg {
            "private" => t.private,
            "high" => t.priority == mailcraft_model::TaskPriority::High,
            "low" => t.priority == mailcraft_model::TaskPriority::Low,
            "flagged" => !t.is_complete() && (t.due.is_some() || t.start.is_some()),
            _ => false,
        };
    }
    if let Some(c) = mb.contacts.get(&id) {
        return match arg {
            "private" => c.private,
            "flagged" => matches!(c.flag, mailcraft_model::FlagState::Flagged { .. }),
            _ => false,
        };
    }
    false
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
        // Follow Up › Today … No Date: checked for the entry the item's flag came from.
        "followup" => focused_mail(app, surface)
            .and_then(|m| mailcraft_engine::cmd::mail::follow_up_kind(&app.s, &m.flag))
            .is_some_and(|k| k == arg),
        // Send / Receive › Work Offline, as the engine keeps it.
        "offline" => v.work_offline,
        // Folder › Show All Folders A to Z, as the engine keeps it.
        "allaz" => mailcraft_engine::cmd::folder::show_all_az_on(&app.s),
        "favorite" => {
            app.s.mailbox.folders.get(&app.s.selection.folder).is_some_and(|f| f.favorite_pos.is_some())
        }
        "conversations" => current_view(app).conversations,
        // Conversation Settings of the selected folder, as the engine keeps them.
        "conv" => mailcraft_engine::queries::conversation_settings(&app.s, app.s.selection.folder)
            .get(arg)
            .unwrap_or(false),
        "focused" => current_view(app).focused_inbox,
        "reverse" => current_view(app).order == mailcraft_model::SortOrder::Ascending,
        "tighter" => current_view(app).tighter_spacing,
        "groups" => current_view(app).show_in_groups,
        // A tasks folder keeps its own message preview (the task notes); mail folders their view settings.
        "preview" => arg.parse::<u8>().is_ok_and(|n| match module_here(app) {
            Some((_, st)) => st.preview_lines == n,
            None => current_view(app).preview_lines == n,
        }),
        "pim" => pim_on(app, surface, arg),
        // The arrangement of a contacts or tasks folder (the module's Arrange By menu).
        "marr" => module_here(app).is_some_and(|(k, st)| st.arrangement(k) == Some(arg)),
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
        "searchDeleted" => app.s.search.include_deleted,
        "todo" => {
            let (cal, people, tasks) = app.ui.todo_bar_parts;
            match arg {
                "calendar" => cal,
                "people" => people,
                "tasks" => tasks,
                "off" => !(cal || people || tasks),
                _ => false,
            }
        }
        // Send / Receive › Download Preferences as the engine shows them (the current account's, else the
        // defaults for new accounts): the mode (`dl:<mode>`) and On Slow Connections Download Only Headers.
        "dl" => mailcraft_engine::sync::cmds::shown_preferences(&app.s).0 == arg,
        "slowHeaders" => mailcraft_engine::sync::cmds::shown_preferences(&app.s).1,
        "ignored" => focused_mail(app, surface).is_some_and(|m| m.ignored),
        "cat" => focused_mail(app, surface)
            .is_some_and(|m| m.categories.iter().any(|c| c.eq_ignore_ascii_case(arg))),
        "cal" => {
            app.ui.calendar_view.eq_ignore_ascii_case(arg)
                || (app.ui.calendar_view.is_empty() && arg == "month")
        }
        "calview" => calview_on(app, arg),
        "notesIcon" => notes_icon_on(app, arg),
        _ => compose(app, surface).is_some_and(|c| compose_on(c, name, arg)),
    }
}

fn compose_on(c: &mailcraft_engine::compose::ComposeState, name: &str, arg: &str) -> bool {
    // Character toggles are on when every selected character has the format (the typing style at a caret);
    // heading text counts as bold unless it was made plain.
    let all = |f: fn(&mailcraft_model::TextStyle) -> Option<bool>| c.selection_all(|s, _| f(s) == Some(true));
    match name {
        "bold" => c.selection_all(|s, heading| s.bold == Some(true) || (heading && s.bold != Some(false))),
        "italic" => all(|s| s.italic),
        "underline" => all(|s| s.underline),
        "strike" => all(|s| s.strike),
        "sub" => all(|s| s.subscript),
        "sup" => all(|s| s.superscript),
        "link" => c.link_at_caret().is_some(),
        "table" => c.cell_at_caret().is_some(),
        "signature" => c.signature.as_ref().is_some_and(|s| s.name.eq_ignore_ascii_case(arg)),
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
        "sign" => c.sign,
        "delay" => c.delay_until.is_some(),
        "replyto" => !c.reply_to.trim().is_empty(),
        "painter" => c.painter.is_some(),
        "pen" => c.highlight_pen.is_some(),
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
    let font = |s: &mailcraft_model::TextStyle| {
        s.font_family
            .clone()
            .or_else(|| c.body.base.font_family.clone())
            .unwrap_or_else(|| mailcraft_fonts::BODY_FAMILY.to_owned())
    };
    let size = |s: &mailcraft_model::TextStyle| {
        let pt = s.size_pt.or(c.body.base.size_pt).unwrap_or(mailcraft_fonts::BODY_SIZE_PT);
        if (pt.fract()).abs() < 0.01 { format!("{pt:.0}") } else { format!("{pt:.1}") }
    };
    match spec {
        Some("value:font") => selection_value(c, font),
        Some("value:size") => selection_value(c, size),
        _ => String::new(),
    }
}

/// What a font / size box shows: the value of the first selected char, or blank when the selection mixes
/// values (like word processors); the caret's typing style when nothing is selected.
fn selection_value(
    c: &mailcraft_engine::compose::ComposeState,
    value: impl Fn(&mailcraft_model::TextStyle) -> String,
) -> String {
    if c.selection.is_collapsed() {
        return value(&c.style_at_caret());
    }
    let first: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let uniform = c.selection_all(|s, _| {
        let v = value(s);
        let mut f = first.borrow_mut();
        match f.as_ref() {
            Some(prev) => prev.eq_ignore_ascii_case(&v),
            None => {
                *f = Some(v);
                true
            }
        }
    });
    match first.into_inner() {
        Some(v) if uniform => v,
        Some(_) => String::new(),
        // Only spaces selected: the caret's style.
        None => value(&c.style_at_caret()),
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
        S::AllItems => "allItems",
    }
}

/// Name of the Styles gallery entry matching the paragraph at the caret ("Normal", "Heading 1"…).
fn current_style(c: &mailcraft_engine::compose::ComposeState) -> String {
    let path = &c.selection.focus.path;
    let mut blocks = &c.body.blocks;
    let mut block = None;
    let mut rest: &[usize] = path;
    // Walk the caret's block path (lists add [item, block], tables [row, cell, block]).
    for _ in 0..64 {
        let Some((&first, tail)) = rest.split_first() else { break };
        let Some(b) = blocks.get(first) else { break };
        block = Some(b);
        match (b, tail) {
            (
                mailcraft_model::Block::Quote { blocks: inner }
                | mailcraft_model::Block::Container { blocks: inner, .. },
                t,
            ) if !t.is_empty() => {
                blocks = inner;
                rest = t;
            }
            (mailcraft_model::Block::List { items, .. }, [item, t @ ..]) if !t.is_empty() => {
                match items.get(*item) {
                    Some(inner) => {
                        blocks = inner;
                        rest = t;
                    }
                    None => break,
                }
            }
            (mailcraft_model::Block::Table { rows, .. }, [row, cell, t @ ..]) if !t.is_empty() => {
                match rows.get(*row).and_then(|r| r.get(*cell)) {
                    Some(cell) => {
                        blocks = &cell.blocks;
                        rest = t;
                    }
                    None => break,
                }
            }
            _ => break,
        }
    }
    match block {
        Some(mailcraft_model::Block::Heading { level, .. }) => format!("Heading {level}"),
        Some(mailcraft_model::Block::Paragraph { .. }) => "Normal".to_owned(),
        _ => String::new(),
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
pub const SOURCES: [&str; 40] = [
    "themeFontNames",
    "fontNames",
    "styleSets",
    "blocks:quickParts",
    "blocks:autoText",
    "blocks:textBoxes",
    "blocks:quickTables",
    "blocks:equations",
    "screenWindows",
    "recentSymbols",
    "tableGrid",
    "savedThemes",
    "customColors",
    "customFonts",
    "recentSearches",
    "searchCategories",
    "searchStores",
    "signatures",
    "recentFiles",
    "quicksteps",
    "categories",
    "accounts",
    "arrange",
    "styles",
    "views:mail",
    "views:people",
    "views:tasks",
    "views:notes",
    "themeColors",
    "themeShades:bg1",
    "themeShades:tx1",
    "themeShades:bg2",
    "themeShades:tx2",
    "themeShades:accent1",
    "themeShades:accent2",
    "themeShades:accent3",
    "themeShades:accent4",
    "themeShades:accent5",
    "themeShades:accent6",
    "highlightPens",
];

/// Swatch bytes of a colour.
fn swatch(c: mailcraft_geom::Color) -> Option<[u8; 3]> {
    Some([c.r, c.g, c.b])
}

/// Theme colour items of the Font Color / Shading menus, from the compose window's active theme (the default
/// theme elsewhere): the base colour of each column (`key` = None) or the five shades of column `key`.
/// Template variables: `$theme` (column key) and `$tint` (percent, positive lighter).
fn theme_items(app: &App, surface: Surface, key: Option<&str>) -> Vec<DynItem> {
    use mailcraft_engine::cmd::format::{THEME_COLUMNS, theme_color};
    let fallback;
    let c = match compose(app, surface) {
        Some(c) => c,
        None => {
            fallback = mailcraft_engine::compose::ComposeState::default();
            &fallback
        }
    };
    let item = |col: &str, name: &str, tint: i8| {
        let label = match tint.signum() {
            1 => format!("{name}, Lighter {tint}%"),
            -1 => format!("{name}, Darker {}%", tint.unsigned_abs()),
            _ => name.to_owned(),
        };
        DynItem {
            label,
            icon: "",
            swatch: theme_color(c, col, i64::from(tint)).and_then(swatch),
            vars: vec![("$theme", json!(col)), ("$tint", json!(tint))],
            on: false,
        }
    };
    match key {
        None => THEME_COLUMNS.iter().map(|(k, _, name, _)| item(k, name, 0)).collect(),
        Some(want) => THEME_COLUMNS
            .iter()
            .filter(|(k, ..)| k.eq_ignore_ascii_case(want))
            .flat_map(|(k, _, name, tints)| tints.iter().map(|t| item(k, name, *t)).collect::<Vec<_>>())
            .collect(),
    }
}

/// Text Highlight Color pens; with High-contrast only on, the pens that leave text hard to read are hidden.
fn highlight_pens(app: &App, surface: Surface) -> Vec<DynItem> {
    use mailcraft_engine::cmd::format::{HIGHLIGHT_COLORS, is_high_contrast};
    let hc = compose(app, surface).is_some_and(|c| c.editor.high_contrast_only);
    HIGHLIGHT_COLORS
        .iter()
        .filter_map(|(name, hex)| {
            let col = mailcraft_geom::Color::parse_hex(hex)?;
            if hc && !is_high_contrast(col) {
                return None;
            }
            Some(DynItem {
                label: (*name).to_owned(),
                icon: "",
                swatch: swatch(col),
                vars: vec![("$color", json!(hex))],
                on: false,
            })
        })
        .collect()
}

/// Styles gallery: the built-in styles plus the styles created in this message (engine order), the caret's
/// style checked; outside a compose window the built-in list.
fn style_items(app: &App, surface: Surface) -> Vec<DynItem> {
    let item = |name: &str, on: bool| DynItem {
        label: name.to_owned(),
        icon: "font-color",
        swatch: None,
        vars: vec![("$name", json!(name))],
        on,
    };
    let Some(c) = compose(app, surface) else {
        return STYLES.iter().map(|s| item(s, false)).collect();
    };
    let current = current_style(c);
    let mut out: Vec<DynItem> = STYLES.iter().map(|s| item(s, current.eq_ignore_ascii_case(s))).collect();
    for v in mailcraft_engine::cmd::format::styles_list(c) {
        let Some(name) = v.get("name").and_then(Value::as_str) else { continue };
        let on = v.get("current").and_then(Value::as_bool).unwrap_or(false);
        match out.iter_mut().find(|d| d.label.eq_ignore_ascii_case(name)) {
            Some(d) => d.on = d.on || on,
            None if v.get("builtin").and_then(Value::as_bool) == Some(false) => out.push(item(name, on)),
            None => {}
        }
    }
    out
}

/// The caret's font in a compose window ("" elsewhere).
fn caret_font(app: &App, surface: Surface) -> String {
    compose(app, surface).and_then(|c| c.style_at_caret().font_family).unwrap_or_default()
}

/// Font list › Theme Fonts: the theme's heading and body fonts ("Aptos Display (Headings)").
fn theme_font_items(app: &App, surface: Surface) -> Vec<DynItem> {
    let fallback;
    let c = match compose(app, surface) {
        Some(c) => c,
        None => {
            fallback = mailcraft_engine::compose::ComposeState::default();
            &fallback
        }
    };
    let theme = c.theme_resolved();
    let current = caret_font(app, surface);
    [(theme.heading_font, "Headings"), (theme.body_font, "Body")]
        .into_iter()
        .map(|(family, role)| DynItem {
            label: format!("{family} ({role})"),
            icon: "",
            swatch: None,
            on: family.eq_ignore_ascii_case(&current),
            vars: vec![("$name", json!(family))],
        })
        .collect()
}

/// Font list › All Fonts: known stand-ins and installed fonts (checked: the caret's font).
fn font_name_items(app: &App, surface: Surface) -> Vec<DynItem> {
    let current = caret_font(app, surface);
    crate::compose::font_names()
        .into_iter()
        .map(|name| DynItem {
            on: name.eq_ignore_ascii_case(&current),
            vars: vec![("$name", json!(name))],
            label: name,
            icon: "",
            swatch: None,
        })
        .collect()
}

/// Change Styles › Style Set: the sets saved with Save as a New Style Set.
fn saved_style_set_items(app: &App) -> Vec<DynItem> {
    mailcraft_engine::cmd::format::saved_style_sets(&app.s)
        .into_iter()
        .map(|set| DynItem {
            vars: vec![("$name", json!(set.name))],
            label: set.name,
            icon: "view",
            swatch: None,
            on: false,
        })
        .collect()
}

/// Change View: the saved views usable in the selected mail folder (checked: the one it shows).
fn mail_view_items(app: &App) -> Vec<DynItem> {
    use mailcraft_engine::cmd::view::mail::{current_view_name, views_for};
    let folder = app.s.selection.folder;
    let current = current_view_name(&app.s, folder);
    views_for(&app.s, folder)
        .into_iter()
        .map(|v| DynItem {
            on: v.name.eq_ignore_ascii_case(&current),
            label: v.name.clone(),
            icon: "view",
            swatch: None,
            vars: vec![("$name", json!(v.name))],
        })
        .collect()
}

/// The master category list by name (the Categorize and Categorized menus list it alphabetically).
fn sorted_categories(app: &App) -> Vec<&mailcraft_model::Category> {
    let mut v: Vec<&mailcraft_model::Category> = app.s.mailbox.categories.iter().collect();
    v.sort_by_cached_key(|c| c.name.to_lowercase());
    v
}

/// Table ▾ grid: columns × rows (Insert › Table, `plan/reference/ribbon-compose.md`: 1x1 … 7x6).
pub const TABLE_GRID: (usize, usize) = (7, 6);

/// Building-block gallery entries saved in `gallery` (Quick Parts, AutoText, Text Box, Quick Tables,
/// Equation menus).
fn block_items(app: &App, surface: Surface, gallery: &str) -> Vec<DynItem> {
    let Some(c) = compose(app, surface) else {
        return Vec::new();
    };
    mailcraft_engine::cmd::insert::gallery_entries(&app.s, c.id, gallery)
        .into_iter()
        .map(|b| DynItem {
            label: b.name.chars().take(60).collect(),
            icon: "quick-steps",
            swatch: None,
            vars: vec![("$name", json!(b.name))],
            on: false,
        })
        .collect()
}

/// Screenshot ▾: the windows the capture backend can see; when it cannot list them, one entry carrying the
/// reason (`$id` null).
fn window_items(app: &App, surface: Surface) -> Vec<DynItem> {
    let own = compose(app, surface).and_then(|c| crate::compose::own_window_title(app, c.id));
    match crate::compose::screenshot_windows(own.as_deref()) {
        Ok(list) => list
            .into_iter()
            .map(|w| DynItem {
                label: w.title.chars().take(70).collect(),
                icon: "picture",
                swatch: None,
                vars: vec![("$id", json!(w.id)), ("$name", json!(w.title))],
                on: false,
            })
            .collect(),
        Err(reason) => vec![DynItem {
            label: "Window capture is not available".into(),
            icon: "",
            swatch: None,
            vars: vec![("$id", Value::Null), ("$name", json!(reason))],
            on: false,
        }],
    }
}

/// Saved theme parts from the profile: `colors`, `fonts` or `themes`.
fn saved_theme_items(app: &App, part: &str) -> Vec<DynItem> {
    let saved = mailcraft_engine::cmd::compose::saved_themes(&app.s);
    let item = |name: &str, swatch: Option<[u8; 3]>| DynItem {
        label: name.to_owned(),
        icon: if swatch.is_some() { "" } else { "view" },
        swatch,
        vars: vec![("$name", json!(name))],
        on: false,
    };
    match part {
        "colors" => saved
            .colors
            .iter()
            .map(|c| {
                item(&c.name, c.colors.get(mailcraft_engine::compose::ACCENT1).copied().and_then(swatch))
            })
            .collect(),
        "fonts" => saved.fonts.iter().map(|c| item(&c.name, None)).collect(),
        _ => saved.themes.iter().map(|t| item(&t.name, None)).collect(),
    }
}

/// Change View / Current View gallery of a module: its built-in views, then the views saved for the selected
/// folder (or for every folder of the module); checked: the view the folder shows.
fn module_view_items(app: &App, kind: mailcraft_engine::cmd::view::modules::ModuleKind) -> Vec<DynItem> {
    use mailcraft_engine::cmd::view::modules;
    let folder = app.s.selection.folder;
    let icon = match kind {
        modules::ModuleKind::People => "people",
        modules::ModuleKind::Tasks => "tasks",
        modules::ModuleKind::Notes => "notes",
    };
    let choices = if modules::module_kind(&app.s, folder) == Some(kind) {
        modules::view_choices(&app.s, folder)
    } else {
        // Not in a folder of this module: its built-in views, the default one checked.
        kind.views().iter().map(|v| ((*v).to_owned(), false, *v == kind.default_view())).collect()
    };
    choices
        .into_iter()
        .map(|(name, saved, on)| DynItem {
            label: name.clone(),
            icon: if saved { "view" } else { icon },
            swatch: None,
            vars: vec![("$name", json!(name))],
            on,
        })
        .collect()
}

/// The Calendar's view as the engine keeps it for the current calendar (the default view before any change).
fn calendar_view_state(app: &App) -> mailcraft_model::calendar::CalendarViewState {
    mailcraft_engine::cmd::calendar::views::saved_view(&app.s).map(|(v, _)| v).unwrap_or_default()
}

/// `calview:<part>`: the Calendar view's toggles and choices (`overlay`, `workingHours`, `tighter`,
/// `timeScale.<minutes>`, `rp.<right|bottom|off>`, `dailyTaskList.<normal|minimized|off>`).
fn calview_on(app: &App, arg: &str) -> bool {
    use mailcraft_model::calendar::DailyTaskList;
    let v = calendar_view_state(app);
    let (part, value) = arg.split_once('.').unwrap_or((arg, ""));
    match part {
        "overlay" => v.overlay,
        "workingHours" => v.working_hours,
        "tighter" => v.tighter_spacing,
        "timeScale" => value.parse::<u8>().is_ok_and(|m| m == v.time_scale),
        "rp" => matches!(
            (value, v.reading_pane),
            ("right", ReadingPanePos::Right)
                | ("bottom", ReadingPanePos::Bottom)
                | ("off", ReadingPanePos::Off)
        ),
        "dailyTaskList" => matches!(
            (value, v.daily_task_list),
            ("normal", DailyTaskList::Normal)
                | ("minimized", DailyTaskList::Minimized)
                | ("off", DailyTaskList::Off)
        ),
        _ => false,
    }
}

/// `notesIcon:<large|small|list>`: the Notes folder shows its Icon view with that arrangement.
fn notes_icon_on(app: &App, arg: &str) -> bool {
    use mailcraft_engine::cmd::notes::IconMode;
    use mailcraft_engine::cmd::view::modules;
    let folder = app.s.selection.folder;
    let Some((modules::ModuleKind::Notes, st)) = modules::current(&app.s, folder) else { return false };
    st.view == "Icon"
        && matches!(
            (arg, st.icon_mode),
            ("large", IconMode::Large) | ("small", IconMode::Small) | ("list", IconMode::List)
        )
}

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
        // Search › Recent Searches: the last searches, newest first (runs `search.recent {"index"}`).
        "recentSearches" => {
            if app.s.search.recent.is_empty() {
                return vec![DynItem {
                    label: "No Recent Searches".into(),
                    icon: "",
                    swatch: None,
                    vars: vec![("$id", Value::Null)],
                    on: false,
                }];
            }
            app.s
                .search
                .recent
                .iter()
                .enumerate()
                .map(|(i, q)| DynItem {
                    label: q.chars().take(60).collect(),
                    icon: "search",
                    swatch: None,
                    vars: vec![("$id", json!(i))],
                    on: q == &app.s.view.search,
                })
                .collect()
        }
        // The stored steps, then the gallery's fixed Create New entry (`quicksteps.run {"id": "new"}`).
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
            .chain(std::iter::once(DynItem {
                label: mailcraft_model::QuickStep::CREATE_NEW.to_owned(),
                icon: "plus",
                swatch: None,
                vars: vec![("$id", json!("new")), ("$name", json!(mailcraft_model::QuickStep::CREATE_NEW))],
                on: false,
            }))
            .collect(),
        "categories" => {
            let item = focused_mail(app, surface);
            let pim: Option<&Vec<String>> = module_item(app, surface)
                .and_then(|id| mailcraft_engine::cmd::calendar::pim::categories(&app.s.mailbox, id));
            sorted_categories(app)
                .into_iter()
                .map(|c| {
                    let rgb = c.color.rgb();
                    DynItem {
                        label: c.name.clone(),
                        icon: "category-tag",
                        swatch: Some([rgb.r, rgb.g, rgb.b]),
                        vars: vec![("$name", json!(c.name))],
                        on: match pim {
                            Some(cats) => cats.iter().any(|x| x.eq_ignore_ascii_case(&c.name)),
                            None => item.is_some_and(|m| {
                                m.categories.iter().any(|x| x.eq_ignore_ascii_case(&c.name))
                            }),
                        },
                    }
                })
                .collect()
        }
        // Search › Categorized: checked while the query holds that category's `category:` term.
        "searchCategories" => sorted_categories(app)
            .into_iter()
            .map(|c| {
                let rgb = c.color.rgb();
                let term = mailcraft_engine::cmd::search::token("category", &c.name);
                DynItem {
                    label: c.name.clone(),
                    icon: "category-tag",
                    swatch: Some([rgb.r, rgb.g, rgb.b]),
                    vars: vec![("$name", json!(c.name))],
                    on: token_on(&app.s.view.search, &term),
                }
            })
            .collect(),
        // Search Tools › Locations to Search: checked = All Mailboxes / All Items search the store.
        "searchStores" => mailcraft_engine::cmd::search::locations(&app.s)
            .into_iter()
            .map(|(key, label, on)| DynItem {
                label,
                icon: "account",
                swatch: None,
                vars: vec![("$name", json!(key))],
                on,
            })
            .collect(),
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
        // Arrangement gallery / Arrange By menu of a contacts or tasks folder: the module's arrangements.
        "arrange" if module_here(app).is_some_and(|(k, _)| !k.arrangements().is_empty()) => {
            let Some((kind, st)) = module_here(app) else { return Vec::new() };
            let cur = st.arrangement(kind).map(str::to_owned);
            kind.arrangements()
                .iter()
                .map(|(key, label)| DynItem {
                    label: (*label).to_owned(),
                    icon: "sort",
                    swatch: None,
                    vars: vec![("$key", json!(key))],
                    on: cur.as_deref() == Some(*key),
                })
                .collect()
        }
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
        "styles" => style_items(app, surface),
        "themeFontNames" => theme_font_items(app, surface),
        "fontNames" => font_name_items(app, surface),
        "styleSets" => saved_style_set_items(app),
        _ if source.starts_with("blocks:") => {
            block_items(app, surface, source.strip_prefix("blocks:").unwrap_or(""))
        }
        "screenWindows" => window_items(app, surface),
        "recentSymbols" => crate::compose::recent_symbols().into_iter().map(|s| plain(&s, "")).collect(),
        "savedThemes" => saved_theme_items(app, "themes"),
        "customColors" => saved_theme_items(app, "colors"),
        "customFonts" => saved_theme_items(app, "fonts"),
        // Drawn as a grid by the menu (see `Resolved::Grid`); as plain items: one per size.
        "tableGrid" => {
            let (cols, rows) = TABLE_GRID;
            (1..=rows)
                .flat_map(|r| (1..=cols).map(move |c| (c, r)))
                .map(|(c, r)| DynItem {
                    label: format!("{c}x{r} Table"),
                    icon: "table",
                    swatch: None,
                    vars: vec![("$cols", json!(c)), ("$rows", json!(r))],
                    on: false,
                })
                .collect()
        }
        "views:mail" => mail_view_items(app),
        "themeColors" => theme_items(app, surface, None),
        "highlightPens" => highlight_pens(app, surface),
        _ if source.starts_with("themeShades:") => {
            theme_items(app, surface, source.strip_prefix("themeShades:"))
        }
        // Signature ▾: the saved signatures (checked: the one in this message).
        "signatures" => {
            let current = compose(app, surface).and_then(|c| c.signature.as_ref()).map(|s| s.name.clone());
            app.s
                .mailbox
                .signatures
                .iter()
                .map(|g| DynItem {
                    on: current.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&g.name)),
                    ..plain(&g.name, "signature")
                })
                .collect()
        }
        // Attach File ▾: files attached recently, newest first.
        "recentFiles" => crate::compose::recent_files()
            .into_iter()
            .map(|p| DynItem {
                label: crate::compose::recent_label(&p),
                icon: "attach",
                swatch: None,
                vars: vec![("$path", json!(p))],
                on: false,
            })
            .collect(),
        "views:people" => module_view_items(app, mailcraft_engine::cmd::view::modules::ModuleKind::People),
        "views:tasks" => module_view_items(app, mailcraft_engine::cmd::view::modules::ModuleKind::Tasks),
        "views:notes" => module_view_items(app, mailcraft_engine::cmd::view::modules::ModuleKind::Notes),
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
    // Calendar view commands that do not apply to the arrangement shown (Time Scale in Month view).
    if surface == Surface::Main
        && app.s.view.module == Module::Calendar
        && let Some(why) = mailcraft_engine::cmd::calendar::view_enabled(&calendar_view_state(app), cmd)
    {
        return Some(why);
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
    // Show as Conversations first asks whether it changes for this folder or all mailboxes
    // (`plan/reference/reply-forward.md` › Conversation view); the answer runs the command with a scope.
    if surface == Surface::Main && act.cmd == "view.conversations" && act.params.get("scope").is_none() {
        if app.s.enabled(&act.cmd).is_none() {
            crate::list::ask_conversation_scope(ctx, app);
        }
        return;
    }
    let params = with_context(app, surface, &act.cmd, act.params.clone());
    // A few compose commands open the compose window's own UI (file picker, Signatures, Spelling dialog).
    if let Surface::Compose(cid) = surface
        && crate::compose::ribbon_command(ctx, app, cid, &act.cmd, &params)
    {
        return;
    }
    // Calendar commands (and View-tab commands while the Calendar shows) go through the modules area so the
    // calendar's view state goes in and comes back in the same frame.
    let calendar_view = act.cmd.starts_with("calendar.")
        || (act.cmd.starts_with("view.") && app.s.view.module == mailcraft_engine::Module::Calendar);
    let result = if surface == Surface::Main && calendar_view {
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
    // Formatting from the ribbon leaves the caret in the body, so typing goes on where it was (a command
    // that opened a dialog leaves the focus to the dialog).
    if let Surface::Compose(cid) = surface
        && result.get("dialog").is_none()
        && matches!(act.cmd.split('.').next(), Some("format" | "insert" | "review"))
    {
        crate::compose::refocus_body(ctx, cid);
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
            ItemKind::Dyn if it.target == "tableGrid" => {
                let (cols, rows) = TABLE_GRID;
                out.push(Resolved::Grid {
                    cols,
                    rows,
                    disabled: disabled(app, surface, &it.cmd.id),
                    cmd: it.cmd.clone(),
                });
            }
            ItemKind::Dyn => {
                // Recent Searches with nothing in it shows its placeholder greyed out.
                let empty = it.target == "recentSearches" && app.s.search.recent.is_empty();
                for d in dyn_items(app, surface, &it.target) {
                    let action = action(app, &it.cmd, d.on, None, &d.vars);
                    let disabled = disabled(app, surface, &it.cmd.id).or_else(|| {
                        if it.target == "quicksteps" {
                            quick_step_blocked(app, surface, &d.vars)
                        } else {
                            None
                        }
                    });
                    out.push(Resolved::Cmd {
                        label: d.label,
                        icon: d.icon.to_owned(),
                        swatch: d.swatch,
                        key: String::new(),
                        on: Some(d.on),
                        disabled: if empty { Some("no recent searches") } else { disabled },
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
                    disabled: disabled(app, surface, &it.cmd.id).or_else(|| {
                        it.en.as_deref().filter(|s| !is_on(app, surface, mem, s)).map(|_| "not available now")
                    }),
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

/// Why the quick step named by `vars` (`$id`) cannot run on this surface's items: a read window acts on its
/// one message, the main window on the selection (see `mailcraft_engine::cmd::quicksteps::blocked_for`).
pub(crate) fn quick_step_blocked(
    app: &App,
    surface: Surface,
    vars: &[(&'static str, Value)],
) -> Option<&'static str> {
    let id = vars.iter().find(|(k, _)| *k == "$id").and_then(|(_, v)| v.as_u64())?;
    let step = app.s.mailbox.quick_steps.iter().find(|q| q.id == id)?;
    let count = match surface {
        Surface::Read(_) => 1,
        _ => app.s.selection.items.len().max(usize::from(app.s.selection.focus.is_some())),
    };
    mailcraft_engine::cmd::quicksteps::blocked_for(step, count, false)
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
    /// A size picker (Table ▾): `cols` × `rows` cells; picking one runs `cmd` with `$cols` / `$rows`.
    Grid {
        cols: usize,
        rows: usize,
        disabled: Option<&'static str>,
        cmd: CmdRef,
    },
    Sep,
}

#[cfg(test)]
mod compose_state_tests {
    use super::*;
    use mailcraft_engine::compose::{ComposeKind, ComposeState, DocPos};
    use mailcraft_engine::{Session, model::Mailbox};

    fn app_with(body: mailcraft_model::Doc) -> (App, u64) {
        let mut mb = Mailbox::new_local("Local");
        mb.add_signature("Formal", mailcraft_model::Doc::from_plain_text("Regards")).unwrap();
        let mut s = Session::new(mb);
        let cid = s.open_compose(ComposeState { kind: ComposeKind::New, body, ..ComposeState::default() });
        (App::new(s), cid)
    }

    fn select(app: &mut App, cid: u64, a: (Vec<usize>, usize), b: (Vec<usize>, usize)) {
        if let Some(c) = app.s.composes.get_mut(&cid) {
            c.set_selection(DocPos::new(a.0, a.1), DocPos::new(b.0, b.1));
        }
    }

    #[test]
    fn character_toggles_follow_the_whole_selection() {
        use mailcraft_model::{Block, Inline, TextStyle};
        let bold = TextStyle { bold: Some(true), ..TextStyle::default() };
        let body = mailcraft_model::Doc {
            blocks: vec![
                Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![
                        Inline::Text { text: "ab".into(), style: bold.clone() },
                        Inline::Text { text: "cd".into(), style: TextStyle::default() },
                    ],
                },
                Block::Heading {
                    level: 1,
                    style: Default::default(),
                    inlines: vec![Inline::Text { text: "Title".into(), style: TextStyle::default() }],
                },
            ],
            ..Default::default()
        };
        let (mut app, cid) = app_with(body);
        let s = Surface::Compose(cid);
        let mem = Mem::default();
        select(&mut app, cid, (vec![0], 0), (vec![0], 2));
        assert!(is_on(&app, s, &mem, "bold"), "all selected chars are bold");
        select(&mut app, cid, (vec![0], 0), (vec![0], 4));
        assert!(!is_on(&app, s, &mem, "bold"), "mixed selection");
        assert!(!is_on(&app, s, &mem, "italic"));
        select(&mut app, cid, (vec![1], 1), (vec![1], 1));
        assert!(is_on(&app, s, &mem, "bold"), "heading text counts as bold");
        let styles = dyn_items(&app, s, "styles");
        assert!(styles.iter().any(|d| d.label == "Heading 1" && d.on));
        select(&mut app, cid, (vec![0], 1), (vec![0], 1));
        assert!(dyn_items(&app, s, "styles").iter().any(|d| d.label == "Normal" && d.on));
    }

    #[test]
    fn font_boxes_go_blank_on_a_mixed_selection() {
        use mailcraft_model::{Block, Inline, TextStyle};
        let big =
            TextStyle { size_pt: Some(20.0), font_family: Some("Carlito".into()), ..TextStyle::default() };
        let body = mailcraft_model::Doc {
            blocks: vec![Block::Paragraph {
                style: Default::default(),
                inlines: vec![
                    Inline::Text { text: "ab".into(), style: big.clone() },
                    Inline::Text { text: "cd".into(), style: TextStyle::default() },
                ],
            }],
            ..Default::default()
        };
        let (mut app, cid) = app_with(body);
        let s = Surface::Compose(cid);
        // Backward selection over the big chars: the first selected char decides, not the char before the caret.
        select(&mut app, cid, (vec![0], 2), (vec![0], 0));
        assert_eq!(combo_value(&app, s, Some("value:size")), "20");
        assert_eq!(combo_value(&app, s, Some("value:font")), "Carlito");
        select(&mut app, cid, (vec![0], 1), (vec![0], 3));
        assert_eq!(combo_value(&app, s, Some("value:size")), "");
        assert_eq!(combo_value(&app, s, Some("value:font")), "");
        select(&mut app, cid, (vec![0], 4), (vec![0], 4));
        assert_eq!(combo_value(&app, s, Some("value:size")), "12");
    }

    #[test]
    fn signature_and_recent_file_sources() {
        let (mut app, cid) = app_with(mailcraft_model::Doc::from_plain_text("x"));
        let s = Surface::Compose(cid);
        let sigs = dyn_items(&app, s, "signatures");
        assert_eq!(sigs.len(), 1);
        assert!(!sigs[0].on);
        app.run("compose.signature", json!({"compose": cid, "name": "Formal"}));
        assert!(dyn_items(&app, s, "signatures")[0].on);
        assert!(is_on(&app, s, &Mem::default(), "signature:Formal"));
        let path = format!("/home/user/Documents/mailcraft-state-test-{}/notes.txt", std::process::id());
        crate::compose::remember_file(&path);
        let recent = dyn_items(&app, s, "recentFiles");
        let item = recent.iter().find(|d| d.label == "notes.txt").unwrap();
        assert_eq!(item.vars, vec![("$path", json!(path))]);
    }
}
