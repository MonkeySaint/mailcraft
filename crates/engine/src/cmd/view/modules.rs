//! Views of the People, Tasks and Notes modules (`plan/reference/modules.md` › Current View, Arrangement,
//! Layout): the `view.*` commands act on these when the selected folder is a contacts, tasks or notes folder.
//!
//! **Views.** People: People, Business Card, Card, Phone, List. Tasks: Detailed, Simple List, To-Do List,
//! Prioritized, Active, Completed, Today, Next 7 Days, Overdue, Assigned, Server Tasks. Notes: Icon, Notes
//! List, Last 7 Days. Each folder remembers its view, its arrangement (People: Categories / Company /
//! Location in the Phone and List views; Tasks: Categories / Start Date / Due Date / Folder / Type /
//! Importance), the Tasks message preview, the reading pane position and the Notes icon arrangement.
//! Reverse Sort, Show in Groups and Use Tighter Spacing are the folder's [`ViewSettings`] fields as in Mail.
//! The table views (People › Phone and List, every Tasks view) also keep their columns, sort field, filter
//! and Conditional Formatting ([`super::table`]).
//!
//! **Saved views.** Save Current View As a New View and Manage All Views › New / Copy keep a view for this
//! folder or for every folder of the module ("Can Be Used On"); Change View lists them after the built-in
//! views. While a saved view shows, the folder's state names it (`ModuleView::name`) and its table settings
//! sit under their own key, so they never overwrite the built-in view's.
//!
//! **Storage.** Like the conversation state, the module view state lives in `ViewState::collapsed_groups`
//! (persisted with the profile) as reserved entries starting with a control character: one JSON
//! [`ModuleView`] and the folder's saved custom views. Group code skips them ([`super::is_group_label`]).
//! Views kept for every folder of a module live in the profile's `state.uiPrefs`
//! (`view.moduleViews.<module>`).
//!
//! **Rows.** [`rows`] is the list a module view shows (filtered, sorted and grouped); the UI draws it and the
//! Expand/Collapse commands read group labels from it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{dialog, settings};
use crate::cmd::notes::IconMode;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::dates::weekday_of_day;
use mailcraft_model::view::{
    ExpandDefault, GROUP_SEP, ItemFilter, ItemRule, MAX_GROUP_LEVELS, MAX_SORT_KEYS, TableOther,
    sanitize_rules,
};
use mailcraft_model::{FolderId, FolderKind, ItemId, ReadingPanePos, SortOrder, ViewSettings};

const MOD_STATE: &str = "\u{1}mod.state=";
const MOD_SAVED: &str = "\u{1}mod.saved=";
/// Most custom views saved per folder.
pub const MAX_SAVED_VIEWS: usize = 50;
/// Longest custom view name (characters).
pub const MAX_VIEW_NAME: usize = 64;

pub const PEOPLE_VIEWS: [&str; 5] = ["People", "Business Card", "Card", "Phone", "List"];
pub const TASK_VIEWS: [&str; 11] = [
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
    "Server Tasks",
];
pub const NOTE_VIEWS: [&str; 3] = ["Icon", "Notes List", "Last 7 Days"];

/// (key, label) arrangements of the People Phone and List views.
pub const PEOPLE_ARRANGE: [(&str, &str); 3] =
    [("categories", "Categories"), ("company", "Company"), ("location", "Location")];
/// (key, label) arrangements of the Tasks views.
pub const TASK_ARRANGE: [(&str, &str); 6] = [
    ("categories", "Categories"),
    ("startDate", "Start Date"),
    ("dueDate", "Due Date"),
    ("folder", "Folder"),
    ("type", "Type"),
    ("importance", "Importance"),
];

/// A module whose folders have module views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModuleKind {
    People,
    Tasks,
    Notes,
}

impl ModuleKind {
    /// The module of a folder kind (Contacts, Tasks, Notes).
    pub fn of(kind: FolderKind) -> Option<ModuleKind> {
        match kind {
            FolderKind::Contacts => Some(ModuleKind::People),
            FolderKind::Tasks => Some(ModuleKind::Tasks),
            FolderKind::Notes => Some(ModuleKind::Notes),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ModuleKind::People => "people",
            ModuleKind::Tasks => "tasks",
            ModuleKind::Notes => "notes",
        }
    }

    /// Built-in views, in gallery order.
    pub fn views(self) -> &'static [&'static str] {
        match self {
            ModuleKind::People => &PEOPLE_VIEWS,
            ModuleKind::Tasks => &TASK_VIEWS,
            ModuleKind::Notes => &NOTE_VIEWS,
        }
    }

    pub fn default_view(self) -> &'static str {
        match self {
            ModuleKind::People => "People",
            ModuleKind::Tasks => "To-Do List",
            ModuleKind::Notes => "Icon",
        }
    }

    /// (key, label) arrangements offered by the module (none for Notes).
    pub fn arrangements(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ModuleKind::People => &PEOPLE_ARRANGE,
            ModuleKind::Tasks => &TASK_ARRANGE,
            ModuleKind::Notes => &[],
        }
    }

    /// "Contact", "Task", "Note" (dialog texts: "Apply Current View to Other Contact Folders").
    pub fn item_noun(self) -> &'static str {
        match self {
            ModuleKind::People => "Contact",
            ModuleKind::Tasks => "Task",
            ModuleKind::Notes => "Note",
        }
    }

    pub fn folder_kind(self) -> FolderKind {
        match self {
            ModuleKind::People => FolderKind::Contacts,
            ModuleKind::Tasks => FolderKind::Tasks,
            ModuleKind::Notes => FolderKind::Notes,
        }
    }
}

/// View state of one module folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleView {
    /// Current view (a built-in view name).
    pub view: String,
    /// Arrangement key chosen with Arrange By (`None` = the view's own arrangement).
    pub arrange: Option<String>,
    /// Tasks › Message Preview: 0 (Off) … 3 lines of the task notes under each row.
    pub preview_lines: u8,
    pub reading_pane: ReadingPanePos,
    /// Notes Icon view: Large Icons, Small Icons or List.
    pub icon_mode: IconMode,
    /// Notes Icon view: icons snapped to the grid (Line Up Icons); cleared when the user drags an icon.
    pub lined_up: bool,
    /// What View Settings changed per built-in table view (columns, sort, filter, Conditional Formatting;
    /// see [`super::table`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, super::table::ViewCustom>,
    /// The saved view in effect (empty: the built-in view `view`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Notes List / Last 7 Days: the sort column (a [`NOTE_COLUMNS`] key; empty = the view's own order,
    /// newest first). Reverse Sort reverses it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sort: String,
    /// Notes List / Last 7 Days: the columns shown, in order (empty = [`NOTE_DEFAULT_COLUMNS`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// Notes View Settings › Filter.
    pub filter: ItemFilter,
    /// Notes View Settings › Conditional Formatting.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ItemRule>,
    /// Notes View Settings › Other Settings: AutoPreview off (no note text under the rows of the table views).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_preview: bool,
    /// Notes View Settings › Other Settings: no grid lines between the rows of the table views.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_grid: bool,
    /// Notes List / Last 7 Days: Sort › Then by, the keys after `sort` ([`NOTE_COLUMNS`] keys, each with its
    /// own direction).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub then_sort: Vec<super::table::SortField>,
    /// Notes List / Last 7 Days: Group By fields, outermost first ([`NOTE_COLUMNS`] keys).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<super::table::SortField>,
    /// Notes tables: Group By › Expand/collapse defaults (People and Tasks keep theirs per view in `custom`).
    #[serde(skip_serializing_if = "ExpandDefault::is_default")]
    pub expand: ExpandDefault,
    /// Notes tables: Format Columns (column key → label and alignment).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, super::mail::ColumnFormat>,
    /// Notes tables: Other Settings fonts (grid lines and AutoPreview are `no_grid` and `no_preview`).
    #[serde(skip_serializing_if = "TableOther::is_default")]
    pub other: TableOther,
}

/// (key, label) columns of the Notes List and Last 7 Days tables (Show Columns, header sort).
pub const NOTE_COLUMNS: [(&str, &str); 5] = [
    ("icon", "Icon"),
    ("subject", "Subject"),
    ("created", "Created"),
    ("modified", "Modified"),
    ("categories", "Categories"),
];
/// Columns of a Notes table nobody changed.
pub const NOTE_DEFAULT_COLUMNS: [&str; 4] = ["icon", "subject", "created", "categories"];

/// Column key for `key` (a key or a label, any case).
pub fn note_column(key: &str) -> Option<&'static str> {
    let want = squash(key);
    NOTE_COLUMNS.iter().find(|(k, l)| squash(k) == want || squash(l) == want).map(|(k, _)| *k)
}

/// Label of a Notes column key.
pub fn note_column_label(key: &str) -> &'static str {
    NOTE_COLUMNS.iter().find(|(k, _)| *k == key).map_or("", |(_, l)| *l)
}

/// The Notes table columns of `st` (its own, else the defaults).
pub fn note_columns(st: &ModuleView) -> Vec<&'static str> {
    let own: Vec<&'static str> = st.columns.iter().filter_map(|c| note_column(c)).collect();
    if own.is_empty() { NOTE_DEFAULT_COLUMNS.to_vec() } else { own }
}

/// Whether the Notes table sorted by `key` lists newest first before Reverse Sort (the view's own order and
/// the date columns); text and colour columns list in ascending order.
pub fn note_sort_descending(key: &str) -> bool {
    matches!(key, "" | "created" | "modified")
}

impl Default for ModuleView {
    fn default() -> Self {
        ModuleView::defaults(ModuleKind::People)
    }
}

impl ModuleView {
    /// A folder's view before any change.
    pub fn defaults(kind: ModuleKind) -> ModuleView {
        ModuleView {
            view: kind.default_view().to_owned(),
            arrange: None,
            preview_lines: 0,
            // Observed: the Notes reading pane is Off, People and Tasks show it on the right.
            reading_pane: if kind == ModuleKind::Notes { ReadingPanePos::Off } else { ReadingPanePos::Right },
            icon_mode: IconMode::Large,
            lined_up: true,
            custom: BTreeMap::new(),
            name: String::new(),
            sort: String::new(),
            columns: Vec::new(),
            filter: ItemFilter::default(),
            rules: Vec::new(),
            no_preview: false,
            no_grid: false,
            then_sort: Vec::new(),
            group_by: Vec::new(),
            expand: ExpandDefault::AllExpanded,
            formats: BTreeMap::new(),
            other: TableOther::default(),
        }
    }

    /// Stored or given values cleaned up: unknown views fall back to the default, numbers are clamped,
    /// text is capped, unknown columns and table settings are dropped.
    pub fn sanitized(mut self, kind: ModuleKind) -> ModuleView {
        self.view = canonical_view(kind, &self.view).unwrap_or(kind.default_view()).to_owned();
        self.arrange = self.arrange.as_deref().and_then(|a| canonical_arrange(kind, a)).map(str::to_owned);
        self.preview_lines = self.preview_lines.min(3);
        self.custom = super::table::sanitize_custom(kind, std::mem::take(&mut self.custom));
        self.name = self.name.trim().chars().filter(|c| !c.is_control()).take(MAX_VIEW_NAME).collect();
        if kind == ModuleKind::Notes {
            self.sort = note_column(&self.sort).unwrap_or("").to_owned();
            let mut seen = std::collections::BTreeSet::new();
            self.columns = self
                .columns
                .iter()
                .filter_map(|c| note_column(c))
                .filter(|c| seen.insert(*c))
                .map(str::to_owned)
                .collect();
            if self.columns.iter().map(String::as_str).eq(NOTE_DEFAULT_COLUMNS) {
                self.columns.clear();
            }
            let note_field = |f: super::table::SortField| {
                note_column(&f.column)
                    .map(|c| super::table::SortField { column: c.to_owned(), descending: f.descending })
            };
            let first = if self.sort.is_empty() { "created".to_owned() } else { self.sort.clone() };
            let mut used: Vec<String> = vec![first];
            self.then_sort = std::mem::take(&mut self.then_sort)
                .into_iter()
                .filter_map(note_field)
                .filter(|f| {
                    let fresh = !used.contains(&f.column);
                    used.push(f.column.clone());
                    fresh
                })
                .take(MAX_SORT_KEYS.saturating_sub(1))
                .collect();
            let mut grouped: Vec<String> = Vec::new();
            self.group_by = std::mem::take(&mut self.group_by)
                .into_iter()
                .filter_map(note_field)
                .filter(|f| {
                    let fresh = !grouped.contains(&f.column);
                    grouped.push(f.column.clone());
                    fresh
                })
                .take(MAX_GROUP_LEVELS)
                .collect();
            self.formats = std::mem::take(&mut self.formats)
                .into_iter()
                .filter_map(|(k, f)| {
                    let key = note_column(&k)?;
                    let label: String =
                        f.label.trim().chars().filter(|c| !c.is_control()).take(MAX_VIEW_NAME).collect();
                    let f = super::mail::ColumnFormat {
                        label: if label == note_column_label(key) { String::new() } else { label },
                        format: String::new(),
                        align: if matches!(f.align.as_str(), "left" | "center" | "right") {
                            f.align
                        } else {
                            String::new()
                        },
                    };
                    (f != super::mail::ColumnFormat::default()).then(|| (key.to_owned(), f))
                })
                .collect();
            let o = std::mem::take(&mut self.other).sanitized();
            self.other =
                TableOther { row_font: o.row_font, column_font: o.column_font, ..TableOther::default() };
        } else {
            self.sort.clear();
            self.columns.clear();
            self.then_sort.clear();
            self.group_by.clear();
            self.expand = ExpandDefault::AllExpanded;
            self.formats.clear();
            self.other = TableOther::default();
        }
        self.filter = std::mem::take(&mut self.filter).sanitized();
        self.rules = sanitize_rules(std::mem::take(&mut self.rules));
        self
    }

    /// The arrangement in effect: the one chosen, else the view's own (List groups by company, To-Do List by
    /// due date, Prioritized by importance).
    pub fn arrangement(&self, kind: ModuleKind) -> Option<&str> {
        if let Some(a) = self.arrange.as_deref() {
            return Some(a);
        }
        match (kind, self.view.as_str()) {
            (ModuleKind::People, "List") => Some("company"),
            (ModuleKind::Tasks, "To-Do List") => Some("dueDate"),
            (ModuleKind::Tasks, "Prioritized") => Some("importance"),
            _ => None,
        }
    }
}

/// A custom view saved with Save Current View As a New View (or New / Copy in Manage All Views).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedView {
    pub name: String,
    pub state: ModuleView,
    /// Reverse Sort and Show in Groups at the time it was saved.
    pub reverse: bool,
    pub groups: bool,
    /// Kept for every folder of the module ("Can be used on: All … folders") rather than this folder only.
    #[serde(skip)]
    pub everywhere: bool,
}

impl Default for SavedView {
    fn default() -> Self {
        SavedView {
            name: String::new(),
            state: ModuleView::default(),
            reverse: false,
            groups: true,
            everywhere: false,
        }
    }
}

/// What a module view row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowKind {
    Task,
    /// A flagged message (To-Do List).
    Mail,
    /// A contact (People views; a flagged contact in the To-Do List).
    Contact,
    Note,
}

/// One row of a module view: an item and its group header label when the view is shown in groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRow {
    pub id: ItemId,
    pub kind: RowKind,
    /// The group the row is collapsed by: its group labels joined with [`GROUP_SEP`] (one label unless Group
    /// By nests several fields).
    pub group: Option<String>,
    /// The row's group headers, outermost first (empty when the view is not shown in groups).
    pub levels: Vec<String>,
}

impl ViewRow {
    /// A row outside any group.
    pub fn plain(id: ItemId, kind: RowKind) -> ViewRow {
        ViewRow { id, kind, group: None, levels: Vec::new() }
    }

    /// A row under the nested groups `levels` (outermost first; none = no group).
    pub fn grouped(id: ItemId, kind: RowKind, levels: Vec<String>) -> ViewRow {
        let group = (!levels.is_empty()).then(|| levels.join(GROUP_SEP));
        ViewRow { id, kind, group, levels }
    }

    /// The collapse key of the row's group at depth `depth` (0 = outermost): its labels up to that level.
    pub fn group_key(&self, depth: usize) -> Option<String> {
        let parts = self.levels.get(..=depth)?;
        Some(parts.join(GROUP_SEP))
    }
}

/// Whether a row with the group headers `levels` is hidden because it or one of its outer groups is
/// collapsed (`collapsed` holds collapse keys, [`ViewRow::group_key`]).
pub fn hidden_by(levels: &[String], collapsed: &dyn Fn(&str) -> bool) -> bool {
    (0..levels.len()).any(|d| levels.get(..=d).is_some_and(|p| collapsed(&p.join(GROUP_SEP))))
}

// ---------------------------------------------------------------------------------------------------------
// State

/// The module of `folder` (contacts, tasks and notes folders).
pub fn module_kind(s: &Session, folder: FolderId) -> Option<ModuleKind> {
    s.mailbox.folder(folder).and_then(|f| ModuleKind::of(f.kind))
}

/// The module and view state of `folder` (defaults when nothing is stored); `None` outside module folders.
pub fn current(s: &Session, folder: FolderId) -> Option<(ModuleKind, ModuleView)> {
    let kind = module_kind(s, folder)?;
    Some((kind, state(s, folder, kind)))
}

/// View state of a module folder (its defaults when nothing valid is stored).
pub fn state(s: &Session, folder: FolderId, kind: ModuleKind) -> ModuleView {
    let stored = s
        .view
        .collapsed_groups
        .get(&folder)
        .and_then(|list| list.iter().find_map(|e| e.strip_prefix(MOD_STATE)))
        .and_then(|json| serde_json::from_str::<ModuleView>(json).ok());
    match stored {
        Some(v) => {
            // Stored text is not trusted: unknown views fall back to the default, numbers are clamped.
            let mut v = v.sanitized(kind);
            // A saved view deleted or renamed from another folder leaves no trace here: the folder goes
            // back to its built-in view and loses that view's table settings.
            let names: Vec<String> =
                saved_views(s, folder).into_iter().map(|sv| sv.name.to_lowercase()).collect();
            if !v.name.is_empty() && !names.contains(&v.name.to_lowercase()) {
                v.name.clear();
            }
            v.custom.retain(|k, _| k.strip_prefix('@').is_none_or(|n| names.contains(&n.to_lowercase())));
            v
        }
        None => ModuleView::defaults(kind),
    }
}

/// While a saved view is shown, a change made to the view (columns, sort, filter, rules, formats,
/// arrangement, preview, Reverse Sort, Show in Groups) is kept in that saved view too, so showing it
/// again later does not undo the change.
pub(crate) fn sync_saved(s: &mut Session, folder: FolderId) -> Result<(), CmdError> {
    let Some((kind, st)) = current(s, folder) else { return Ok(()) };
    if st.name.is_empty() {
        return Ok(());
    }
    let Some(old) = find_saved(s, folder, &st.name) else { return Ok(()) };
    let v = snapshot(s, folder, kind, &old.name, old.everywhere);
    if v != old { put_saved(s, folder, kind, v) } else { Ok(()) }
}

/// Stores the view state of a module folder (the entry is dropped when it equals the defaults, so Reset
/// View's enabled state stays observable).
pub(crate) fn store(s: &mut Session, folder: FolderId, kind: ModuleKind, st: &ModuleView) {
    let list = s.view.collapsed_groups.entry(folder).or_default();
    list.retain(|e| !e.starts_with(MOD_STATE));
    if *st != ModuleView::defaults(kind)
        && let Ok(json) = serde_json::to_string(st)
    {
        list.push(format!("{MOD_STATE}{json}"));
    }
    if list.is_empty() {
        s.view.collapsed_groups.remove(&folder);
    }
}

/// Key of a module's views kept for all its folders (in the profile's `state.uiPrefs`).
fn module_key(kind: ModuleKind) -> String {
    format!("view.moduleViews.{}", kind.name())
}

fn clean_saved(kind: ModuleKind, list: Vec<SavedView>, everywhere: bool) -> Vec<SavedView> {
    let mut seen: Vec<String> = Vec::new();
    list.into_iter()
        .filter_map(|mut v| {
            v.name = v.name.trim().chars().filter(|c| !c.is_control()).take(MAX_VIEW_NAME).collect();
            v.state = std::mem::take(&mut v.state).sanitized(kind);
            v.state.name.clear();
            v.everywhere = everywhere;
            let key = v.name.to_lowercase();
            let ok = !v.name.is_empty() && canonical_view(kind, &v.name).is_none() && !seen.contains(&key);
            seen.push(key);
            ok.then_some(v)
        })
        .take(MAX_SAVED_VIEWS)
        .collect()
}

/// Custom views saved for `folder` only.
fn folder_saved(s: &Session, folder: FolderId, kind: ModuleKind) -> Vec<SavedView> {
    let list = s
        .view
        .collapsed_groups
        .get(&folder)
        .and_then(|list| list.iter().find_map(|e| e.strip_prefix(MOD_SAVED)))
        .and_then(|json| serde_json::from_str::<Vec<SavedView>>(json).ok())
        .unwrap_or_default();
    clean_saved(kind, list, false)
}

/// Custom views kept for every folder of the module.
fn module_saved(s: &Session, kind: ModuleKind) -> Vec<SavedView> {
    let list = super::mail::ui_pref(s, &module_key(kind))
        .and_then(|v| serde_json::from_value::<Vec<SavedView>>(v).ok())
        .unwrap_or_default();
    clean_saved(kind, list, true)
}

/// Custom views usable in `folder`: the folder's own, then the module's views for all folders (a folder
/// view hides a module view of the same name).
pub fn saved_views(s: &Session, folder: FolderId) -> Vec<SavedView> {
    let Some(kind) = module_kind(s, folder) else { return Vec::new() };
    let mut out = folder_saved(s, folder, kind);
    for v in module_saved(s, kind) {
        if !out.iter().any(|x| x.name.eq_ignore_ascii_case(&v.name)) {
            out.push(v);
        }
    }
    out
}

fn store_saved(s: &mut Session, folder: FolderId, views: &[SavedView]) {
    let list = s.view.collapsed_groups.entry(folder).or_default();
    list.retain(|e| !e.starts_with(MOD_SAVED));
    if !views.is_empty()
        && let Ok(json) = serde_json::to_string(views)
    {
        list.push(format!("{MOD_SAVED}{json}"));
    }
    if list.is_empty() {
        s.view.collapsed_groups.remove(&folder);
    }
}

/// "All Note folders" (Manage Views › Can Be Used On).
pub fn all_folders_text(kind: ModuleKind) -> String {
    format!("All {} folders", kind.item_noun())
}

fn store_module_saved(s: &mut Session, kind: ModuleKind, views: &[SavedView]) -> Result<(), CmdError> {
    let v = if views.is_empty() { None } else { serde_json::to_value(views).ok() };
    super::mail::set_ui_pref(s, &module_key(kind), v)
}

/// Stores `view` as a saved view (replacing one of the same name in the same scope).
fn put_saved(s: &mut Session, folder: FolderId, kind: ModuleKind, view: SavedView) -> Result<(), CmdError> {
    let mut list = if view.everywhere { module_saved(s, kind) } else { folder_saved(s, folder, kind) };
    list.retain(|x| !x.name.eq_ignore_ascii_case(&view.name));
    if list.len() >= MAX_SAVED_VIEWS {
        return Err(CmdError::Failed(format!("at most {MAX_SAVED_VIEWS} saved views are kept")));
    }
    let everywhere = view.everywhere;
    list.push(view);
    if everywhere {
        store_module_saved(s, kind, &list)
    } else {
        store_saved(s, folder, &list);
        Ok(())
    }
}

/// Removes the saved view `name` usable in `folder` (the folder's own first). False when there is none.
fn remove_saved(s: &mut Session, folder: FolderId, kind: ModuleKind, name: &str) -> Result<bool, CmdError> {
    let mut list = folder_saved(s, folder, kind);
    if list.iter().any(|v| v.name.eq_ignore_ascii_case(name)) {
        list.retain(|v| !v.name.eq_ignore_ascii_case(name));
        store_saved(s, folder, &list);
        return Ok(true);
    }
    let mut list = module_saved(s, kind);
    if list.iter().any(|v| v.name.eq_ignore_ascii_case(name)) {
        list.retain(|v| !v.name.eq_ignore_ascii_case(name));
        store_module_saved(s, kind, &list)?;
        return Ok(true);
    }
    Ok(false)
}

/// The saved view `name` usable in `folder`.
fn find_saved(s: &Session, folder: FolderId, name: &str) -> Option<SavedView> {
    let want = name.trim();
    saved_views(s, folder).into_iter().find(|v| v.name.eq_ignore_ascii_case(want))
}

/// The name of the view the folder shows: its saved view while one is in effect, else the built-in view.
pub fn current_view_name(s: &Session, folder: FolderId) -> String {
    let Some((_, st)) = current(s, folder) else { return String::new() };
    if !st.name.is_empty() && find_saved(s, folder, &st.name).is_some() {
        return st.name;
    }
    st.view
}

/// Change View choices of the folder: (name, saved, shown now), the built-in views first.
pub fn view_choices(s: &Session, folder: FolderId) -> Vec<(String, bool, bool)> {
    let Some(kind) = module_kind(s, folder) else { return Vec::new() };
    let current = current_view_name(s, folder);
    kind.views()
        .iter()
        .map(|v| ((*v).to_owned(), false))
        .chain(saved_views(s, folder).into_iter().map(|v| (v.name, true)))
        .map(|(name, saved)| {
            let on = name.eq_ignore_ascii_case(&current);
            (name, saved, on)
        })
        .collect()
}

/// Manage All Views' Description of the view `name` usable in `folder` (a built-in view with this folder's
/// changes to it, or a saved view): (label, text) rows. `None` when there is no such view.
pub fn view_description(s: &Session, folder: FolderId, name: &str) -> Option<Vec<(&'static str, String)>> {
    let kind = module_kind(s, folder)?;
    let (st, reverse, groups, used_on) = if let Some(v) = canonical_view(kind, name) {
        let here = state(s, folder, kind);
        let mut st = ModuleView::defaults(kind);
        st.view = v.to_owned();
        if let Some(c) = here.custom.get(v) {
            st.custom.insert(v.to_owned(), c.clone());
        }
        if here.name.is_empty() && here.view == v {
            st.arrange = here.arrange;
        }
        let set = settings(s, folder);
        (st, set.order == SortOrder::Ascending, set.show_in_groups, scope_label(kind, true))
    } else {
        let v = find_saved(s, folder, name)?;
        let mut st = v.state;
        st.name.clear();
        (st, v.reverse, v.groups, scope_label(kind, v.everywhere))
    };
    let sum = super::table::summary(folder, kind, &st);
    let text = |k: &str| sum.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut rows = vec![("Type of view", st.view.clone())];
    if sum.get("table").and_then(Value::as_bool) == Some(true) {
        rows.push(("Columns", text("columns")));
    }
    let arranged = if arrange_allowed(kind, &st.view) || grouped_view(kind, &st.view) {
        st.arrangement(kind).map(|a| arrange_label(kind, a)).filter(|l| !l.is_empty())
    } else {
        None
    };
    let group = match arranged {
        Some(l) if groups => format!("{l}{}", if reverse { " (reversed)" } else { "" }),
        Some(l) => format!("Off (arranged by {l})"),
        None => "None".to_owned(),
    };
    rows.push(("Group By", group));
    if sum.get("table").and_then(Value::as_bool) == Some(true) {
        rows.push(("Sort", text("sort")));
        rows.push(("Filter", text("filter")));
        if kind != ModuleKind::Notes {
            rows.push(("Conditional Formatting", text("rules")));
        }
        rows.push(("Format Columns", text("formats")));
    }
    rows.push(("Can be used on", used_on));
    Some(rows)
}

/// A new view name: printable, not too long, not a built-in view's name.
fn check_name(kind: ModuleKind, name: &str) -> Result<String, CmdError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CmdError::Params("the view needs a name".into()));
    }
    if name.chars().count() > MAX_VIEW_NAME || name.chars().any(char::is_control) {
        return Err(CmdError::Params(format!("view names are at most {MAX_VIEW_NAME} printable characters")));
    }
    if canonical_view(kind, name).is_some() {
        return Err(CmdError::Params(format!("a view named \"{name}\" already exists")));
    }
    Ok(name.to_owned())
}

/// True when the folder's module view differs from its defaults (Reset View is enabled).
pub fn customized(s: &Session, folder: FolderId) -> bool {
    s.view.collapsed_groups.get(&folder).is_some_and(|list| list.iter().any(|e| e.starts_with(MOD_STATE)))
}

/// Built-in view named `name` (case and spacing insensitive: "to-do list", "To-Do List", "todoList").
pub fn canonical_view(kind: ModuleKind, name: &str) -> Option<&'static str> {
    let want = squash(name);
    kind.views().iter().copied().find(|v| squash(v) == want)
}

/// Arrangement key for `key` (a key or a label: "dueDate", "Due Date", "flagDue").
pub fn canonical_arrange(kind: ModuleKind, key: &str) -> Option<&'static str> {
    let want = squash(key);
    let alias = match (kind, want.as_str()) {
        (ModuleKind::Tasks, "flagdue" | "flagduedate") => Some("dueDate"),
        (ModuleKind::Tasks, "flagstart" | "flagstartdate") => Some("startDate"),
        (ModuleKind::Tasks, "priority") => Some("importance"),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    kind.arrangements().iter().find(|(k, l)| squash(k) == want || squash(l) == want).map(|(k, _)| *k)
}

/// Label of an arrangement key ("Due Date").
pub fn arrange_label(kind: ModuleKind, key: &str) -> &'static str {
    kind.arrangements().iter().find(|(k, _)| *k == key).map_or("", |(_, l)| *l)
}

fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether Arrange By applies in `view` (People: only the Phone and List table views).
pub fn arrange_allowed(kind: ModuleKind, view: &str) -> bool {
    match kind {
        ModuleKind::People => matches!(view, "Phone" | "List"),
        ModuleKind::Tasks => true,
        // Notes List and Last 7 Days sort by a column (a header click); the Icon view has no arrangement.
        ModuleKind::Notes => view != "Icon",
    }
}

/// Whether the view is a table with groups (Expand/Collapse, Add Columns apply).
pub fn grouped_view(kind: ModuleKind, view: &str) -> bool {
    match kind {
        ModuleKind::People => matches!(view, "Phone" | "List"),
        ModuleKind::Tasks => true,
        ModuleKind::Notes => false,
    }
}

/// Reverse Sort is on (the folder's sort order is ascending: the module views' natural order reversed).
pub fn reversed(s: &Session, folder: FolderId) -> bool {
    settings(s, folder).order == SortOrder::Ascending
}

/// Show in Groups is on for the folder.
pub fn show_groups(s: &Session, folder: FolderId) -> bool {
    settings(s, folder).show_in_groups
}

/// Date group of a local day number: (rank, label). `past` labels days before today ("Overdue" for due
/// dates, "Earlier" for start dates). Weeks start on Sunday.
pub fn date_group(day: Option<i64>, today: i64, past: &'static str) -> (u8, &'static str) {
    let week_start = today.saturating_sub(i64::from(weekday_of_day(today).0));
    match day {
        None => (6, "No Date"),
        Some(d) if d < today => (0, past),
        Some(d) if d == today => (1, "Today"),
        Some(d) if d == today.saturating_add(1) => (2, "Tomorrow"),
        Some(d) if d <= week_start.saturating_add(6) => (3, "This Week"),
        Some(d) if d <= week_start.saturating_add(13) => (4, "Next Week"),
        Some(_) => (5, "Later"),
    }
}

/// Rows of a module folder's current view (see the module docs).
pub fn rows(s: &Session, folder: FolderId) -> Vec<ViewRow> {
    match module_kind(s, folder) {
        Some(ModuleKind::People) => crate::cmd::people::view_rows(s, folder, ""),
        Some(ModuleKind::Tasks) => crate::cmd::tasks::view_rows(s, folder),
        Some(ModuleKind::Notes) => crate::cmd::notes::view_rows(s, folder),
        None => Vec::new(),
    }
}

/// Group label of item `id` in the folder's view.
pub fn group_of(s: &Session, folder: FolderId, id: ItemId) -> Option<String> {
    rows(s, folder).into_iter().find(|r| r.id == id).and_then(|r| r.group)
}

/// Every group label of the folder's view, in order (with nested groups, each outer group's collapse key
/// before the groups inside it).
pub fn groups(s: &Session, folder: FolderId) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in rows(s, folder) {
        // Nested groups (Group By on several fields): every outer group has a header of its own.
        for d in 0..r.levels.len() {
            if let Some(key) = r.group_key(d)
                && seen.insert(key.clone())
            {
                out.push(key);
            }
        }
    }
    out
}

/// Summary of a folder's module view for results and dialogs.
pub fn view_json(s: &Session, folder: FolderId, kind: ModuleKind, st: &ModuleView) -> Value {
    let v = settings(s, folder);
    json!({
        "folder": folder.0,
        "module": kind.name(),
        "view": st.view,
        "views": kind.views(),
        "saved": saved_views(s, folder).iter().map(|v| v.name.clone()).collect::<Vec<_>>(),
        "name": current_view_name(s, folder),
        "arrange": st.arrangement(kind),
        "arrangements": kind.arrangements().iter().map(|(k, l)| json!({"key": k, "label": l})).collect::<Vec<_>>(),
        "arrangeAllowed": arrange_allowed(kind, &st.view),
        "reverse": v.order == SortOrder::Ascending,
        "groups": v.show_in_groups,
        "previewLines": st.preview_lines,
        "readingPane": pane_name(st.reading_pane),
        "tighterSpacing": v.tighter_spacing,
        "iconMode": st.icon_mode,
        "linedUp": st.lined_up,
        "current": current_name(st),
        "sort": st.sort,
        "columns": if kind == ModuleKind::Notes { note_columns(st) } else { Vec::new() },
        "allColumns": if kind == ModuleKind::Notes {
            NOTE_COLUMNS.iter().map(|(k, l)| json!({"key": k, "label": l})).collect::<Vec<_>>()
        } else {
            Vec::new()
        },
        "filter": st.filter,
        "filterText": st.filter.describe(),
        "rules": st.rules,
        "autoPreview": !st.no_preview,
        "gridLines": !st.no_grid,
        "sortBy": if kind == ModuleKind::Notes && st.view != "Icon" {
            super::table::fields_json(&note_sort_keys(st, v.order == SortOrder::Ascending))
        } else {
            json!([])
        },
        "groupBy": super::table::fields_json(&st.group_by),
        "expandGroups": st.expand.key(),
        "formats": st.formats,
        "rowFont": st.other.row_font,
        "columnFont": st.other.column_font,
    })
}

/// The name Change View checks: the saved view shown, else the built-in view.
pub fn current_name(st: &ModuleView) -> &str {
    if st.name.is_empty() { &st.view } else { &st.name }
}

/// Change View items of a module folder: (name, checked) for the built-in views, then the saved views
/// usable in it.
pub fn change_view_items(s: &Session, folder: FolderId) -> Vec<(String, bool)> {
    let Some((kind, st)) = current(s, folder) else { return Vec::new() };
    let cur = current_name(&st).to_owned();
    kind.views()
        .iter()
        .map(|v| (*v).to_owned())
        .chain(saved_views(s, folder).into_iter().map(|v| v.name))
        .map(|n| {
            let on = n.eq_ignore_ascii_case(&cur);
            (n, on)
        })
        .collect()
}

pub(crate) fn pane_name(p: ReadingPanePos) -> &'static str {
    match p {
        ReadingPanePos::Right => "right",
        ReadingPanePos::Bottom => "bottom",
        ReadingPanePos::Off => "off",
    }
}

// ---------------------------------------------------------------------------------------------------------
// Commands (called by the `view.*` commands when the selected folder is a module folder)

/// `view.changeView`: a built-in view or a saved custom view.
pub(crate) fn change_view(s: &mut Session, folder: FolderId, kind: ModuleKind, name: &str) -> CmdResult {
    let mut st = state(s, folder, kind);
    if let Some(v) = canonical_view(kind, name) {
        if st.view != v {
            // Each view arranges itself until Arrange By picks something else.
            st.arrange = None;
        }
        st.view = v.to_owned();
        st.name.clear();
        store(s, folder, kind, &st);
        open_groups(s, folder);
        return Ok(json!({ "view": v, "module": kind.name() }));
    }
    let wanted = name.trim();
    let Some(saved) = find_saved(s, folder, wanted) else {
        let clipped: String = wanted.chars().take(MAX_VIEW_NAME).collect();
        return Err(CmdError::Params(format!("no {} view named \"{clipped}\"", kind.name())));
    };
    apply_saved(s, folder, kind, &saved);
    let view = state(s, folder, kind).view;
    Ok(json!({ "view": view, "saved": saved.name, "module": kind.name() }))
}

fn apply_saved(s: &mut Session, folder: FolderId, kind: ModuleKind, saved: &SavedView) {
    // The folder keeps its changes to the built-in views; the saved view's table settings go under its own
    // key ("@name").
    let sv = &saved.state;
    let mut st = state(s, folder, kind);
    st.view = canonical_view(kind, &sv.view).unwrap_or(kind.default_view()).to_owned();
    st.arrange = sv.arrange.clone();
    st.preview_lines = sv.preview_lines;
    st.reading_pane = sv.reading_pane;
    st.icon_mode = sv.icon_mode;
    st.lined_up = sv.lined_up;
    st.sort = sv.sort.clone();
    st.columns = sv.columns.clone();
    st.filter = sv.filter.clone();
    st.rules = sv.rules.clone();
    st.no_preview = sv.no_preview;
    st.no_grid = sv.no_grid;
    st.then_sort = sv.then_sort.clone();
    st.group_by = sv.group_by.clone();
    st.expand = sv.expand;
    st.formats = sv.formats.clone();
    st.other = sv.other.clone();
    st.name = saved.name.clone();
    let key = format!("@{}", saved.name);
    match sv.custom.get(&super::table::active_key(sv)).or_else(|| sv.custom.get(&sv.view)) {
        Some(c) if !c.is_empty() => {
            st.custom.insert(key, c.clone());
        }
        _ => {
            st.custom.remove(&key);
        }
    }
    store(s, folder, kind, &st);
    set_settings(s, folder, |v| {
        v.order = if saved.reverse { SortOrder::Ascending } else { SortOrder::Descending };
        v.show_in_groups = saved.groups;
    });
    open_groups(s, folder);
}

/// `view.arrangeBy` in a module folder.
pub(crate) fn arrange_by(s: &mut Session, folder: FolderId, kind: ModuleKind, key: &str) -> CmdResult {
    let mut st = state(s, folder, kind);
    if kind == ModuleKind::Notes {
        return note_sort(s, folder, st, key);
    }
    if !arrange_allowed(kind, &st.view) {
        return Err(CmdError::Disabled(format!("Arrange By is not available in the {} view", st.view)));
    }
    let a = canonical_arrange(kind, key)
        .ok_or_else(|| CmdError::Params(format!("unknown {} arrangement `{key}`", kind.name())))?;
    st.arrange = Some(a.to_owned());
    store(s, folder, kind, &st);
    sync_saved(s, folder)?;
    clear_groups(s, folder);
    Ok(json!({ "arrangeBy": a, "label": arrange_label(kind, a), "module": kind.name() }))
}

/// Notes List / Last 7 Days: sorts by column `key` (a header click). Choosing the sort column again reverses
/// the order (Reverse Sort); a new column starts in its natural order (text A to Z, dates newest first).
fn note_sort(s: &mut Session, folder: FolderId, mut st: ModuleView, key: &str) -> CmdResult {
    if st.view == "Icon" {
        return Err(CmdError::Disabled("the Icon view is not sorted by a column".into()));
    }
    let col = note_column(key).ok_or_else(|| {
        CmdError::Params(format!(
            "`key` must be a Notes column ({})",
            NOTE_COLUMNS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let current = if st.sort.is_empty() { "created" } else { st.sort.as_str() };
    let reverse = if current == col { !reversed(s, folder) } else { false };
    st.sort = col.to_owned();
    store(s, folder, ModuleKind::Notes, &st);
    set_settings(s, folder, |v| v.order = if reverse { SortOrder::Ascending } else { SortOrder::Descending });
    sync_saved(s, folder)?;
    Ok(json!({
        "sort": col,
        "label": note_column_label(col),
        "reverse": reverse,
        "descending": note_sort_descending(col) != reverse,
        "module": "notes",
    }))
}

/// `view.messagePreview` in a tasks folder (the task notes under each row).
pub(crate) fn message_preview(s: &mut Session, folder: FolderId, kind: ModuleKind, lines: u8) -> CmdResult {
    if kind != ModuleKind::Tasks {
        return Err(CmdError::Disabled("not available in this view".into()));
    }
    let mut st = state(s, folder, kind);
    st.preview_lines = lines.min(3);
    store(s, folder, kind, &st);
    sync_saved(s, folder)?;
    Ok(json!({ "lines": st.preview_lines, "module": kind.name() }))
}

/// `view.readingPane` in a module folder: the module's own position, mirrored into the folder's view
/// settings (which the ribbon's Reading Pane menu shows).
pub(crate) fn reading_pane(
    s: &mut Session,
    folder: FolderId,
    kind: ModuleKind,
    pos: ReadingPanePos,
) -> CmdResult {
    let mut st = state(s, folder, kind);
    st.reading_pane = pos;
    store(s, folder, kind, &st);
    set_settings(s, folder, |v| v.reading_pane = pos);
    sync_saved(s, folder)?;
    Ok(json!({ "position": pane_name(pos), "module": kind.name() }))
}

/// Notes icon arrangement (`notes.iconView`, `notes.lineUpIcons`): switches the folder to the Icon view.
pub(crate) fn set_icons(s: &mut Session, folder: FolderId, mode: IconMode, lined_up: bool) -> ModuleView {
    let kind = ModuleKind::Notes;
    let mut st = state(s, folder, kind);
    st.view = "Icon".to_owned();
    st.icon_mode = mode;
    st.lined_up = lined_up;
    store(s, folder, kind, &st);
    st
}

/// `view.applyViewTo` in a module folder: the view settings and module view of `src` go to every target
/// folder of the same kind. Without targets the Apply View dialog opens.
pub(crate) fn apply_view_to(
    s: &mut Session,
    src: FolderId,
    kind: ModuleKind,
    targets: Option<std::collections::BTreeSet<FolderId>>,
) -> CmdResult {
    let Some(targets) = targets else {
        let folders: Vec<Value> = s
            .mailbox
            .folders
            .values()
            .filter(|f| f.kind == kind.folder_kind() && f.id != src)
            .map(|f| json!({ "id": f.id.0, "path": s.mailbox.folder_path(f.id) }))
            .collect();
        return Ok(dialog(
            s,
            "applyModuleView",
            json!({ "folder": src.0, "module": kind.name(), "noun": kind.item_noun(), "folders": folders }),
        ));
    };
    let view = settings(s, src);
    let st = state(s, src, kind);
    let mut applied = 0usize;
    for id in targets {
        if id == src || module_kind(s, id) != Some(kind) {
            continue;
        }
        if view == ViewSettings::default() {
            s.mailbox.views.remove(&id);
        } else {
            s.mailbox.views.insert(id, view.clone());
        }
        store(s, id, kind, &st);
        applied = applied.saturating_add(1);
    }
    Ok(json!({ "applied": applied, "module": kind.name() }))
}

fn scope_everywhere(p: &Value) -> Result<bool, CmdError> {
    match p.get("scope").and_then(Value::as_str) {
        None | Some("folder") => Ok(false),
        Some("all") => Ok(true),
        Some(_) => Err(CmdError::Params("`scope` must be folder or all".into())),
    }
}

fn name_param<'a>(p: &'a Value, key: &str) -> Result<&'a str, CmdError> {
    p.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| CmdError::Params(format!("`{key}` (a view name) required")))
}

/// The current view of the folder as a saved view named `name`.
fn snapshot(s: &Session, folder: FolderId, kind: ModuleKind, name: &str, everywhere: bool) -> SavedView {
    let v = settings(s, folder);
    let mut st = state(s, folder, kind);
    // Only the shown view's table settings go with it.
    let c = super::table::custom(&st);
    st.custom.clear();
    if !c.is_empty() {
        st.custom.insert(st.view.clone(), c);
    }
    st.name.clear();
    SavedView {
        name: name.to_owned(),
        state: st,
        reverse: v.order == SortOrder::Ascending,
        groups: v.show_in_groups,
        everywhere,
    }
}

/// "Can Be Used On" text of a module view.
pub fn scope_label(kind: ModuleKind, everywhere: bool) -> String {
    if everywhere { format!("All {} folders", kind.item_noun()) } else { "This folder".to_owned() }
}

fn exists_error(name: &str) -> CmdError {
    CmdError::Params(format!("a view named \"{name}\" already exists"))
}

/// `view.manageViews` in a module folder. Without `op` the Manage All Views dialog opens; `op` is one of
/// `apply` (`name`), `new` (`name`, `base`: a built-in view, `scope`), `copy` (`name`, `newName`,
/// `scope`), `rename` (`name`, `newName`), `delete` (`name`), `update` (`name`: the saved view takes the
/// current view) and `reset` (`name`: a built-in view loses its View Settings changes).
pub(crate) fn manage_views(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let Some(op) = p.get("op").and_then(Value::as_str) else {
        let current = current_view_name(s, folder);
        let saved = saved_views(s, folder);
        let mut views: Vec<String> = kind.views().iter().map(|v| (*v).to_owned()).collect();
        views.extend(saved.iter().map(|v| v.name.clone()));
        let saved: Vec<Value> = saved
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "base": v.state.view,
                    "scope": if v.everywhere { "all" } else { "folder" },
                    "usedOn": scope_label(kind, v.everywhere),
                })
            })
            .collect();
        return Ok(dialog(
            s,
            "manageViews",
            json!({
                "views": views,
                "builtIn": kind.views(),
                "saved": saved,
                "current": current,
                "allFolders": scope_label(kind, true),
                "module": kind.name(),
                "noun": kind.item_noun(),
                "folder": folder.0,
            }),
        ));
    };
    match op {
        "apply" => change_view(s, folder, kind, name_param(p, "name")?),
        "new" => {
            let name = check_name(kind, name_param(p, "name")?)?;
            if find_saved(s, folder, &name).is_some() {
                return Err(exists_error(&name));
            }
            let base = match p.get("base").and_then(Value::as_str) {
                None => kind.default_view(),
                Some(b) => canonical_view(kind, b)
                    .ok_or_else(|| CmdError::Params(format!("`base` must be a {} view", kind.name())))?,
            };
            let everywhere = scope_everywhere(p)?;
            let mut st = ModuleView::defaults(kind);
            st.view = base.to_owned();
            let v = SavedView { name: name.clone(), state: st, reverse: false, groups: true, everywhere };
            put_saved(s, folder, kind, v)?;
            Ok(json!({ "created": name, "base": base, "module": kind.name() }))
        }
        "copy" => {
            let from = name_param(p, "name")?;
            let name = check_name(kind, name_param(p, "newName")?)?;
            if find_saved(s, folder, &name).is_some() {
                return Err(exists_error(&name));
            }
            let everywhere = scope_everywhere(p)?;
            let copy = if let Some(b) = canonical_view(kind, from) {
                // A built-in view with this folder's View Settings changes for it.
                let mut st = ModuleView::defaults(kind);
                st.view = b.to_owned();
                if let Some(c) = state(s, folder, kind).custom.get(b) {
                    st.custom.insert(b.to_owned(), c.clone());
                }
                SavedView { name: name.clone(), state: st, reverse: false, groups: true, everywhere }
            } else {
                let mut v = find_saved(s, folder, from)
                    .ok_or_else(|| CmdError::Params(format!("no {} view named \"{from}\"", kind.name())))?;
                v.name = name.clone();
                v.everywhere = everywhere;
                v
            };
            put_saved(s, folder, kind, copy)?;
            Ok(json!({ "copied": name, "module": kind.name() }))
        }
        "rename" => {
            let from = name_param(p, "name")?;
            let name = check_name(kind, name_param(p, "newName")?)?;
            let mut v = find_saved(s, folder, from)
                .ok_or_else(|| CmdError::Params("only saved views can be renamed".into()))?;
            if !v.name.eq_ignore_ascii_case(&name) && find_saved(s, folder, &name).is_some() {
                return Err(exists_error(&name));
            }
            // Read before the old name goes, or the state would already have dropped its settings.
            let mut st = state(s, folder, kind);
            remove_saved(s, folder, kind, &v.name)?;
            let old = std::mem::replace(&mut v.name, name.clone());
            put_saved(s, folder, kind, v)?;
            if let Some(c) = st.custom.remove(&format!("@{old}")) {
                st.custom.insert(format!("@{name}"), c);
            }
            if st.name.eq_ignore_ascii_case(&old) {
                st.name = name.clone();
            }
            store(s, folder, kind, &st);
            Ok(json!({ "renamed": old, "name": name, "module": kind.name() }))
        }
        "delete" => {
            let name = name_param(p, "name")?;
            if !remove_saved(s, folder, kind, name)? {
                return Err(CmdError::Params("only saved views can be deleted".into()));
            }
            let mut st = state(s, folder, kind);
            st.custom.retain(|k, _| !k.strip_prefix('@').is_some_and(|n| n.eq_ignore_ascii_case(name)));
            if st.name.eq_ignore_ascii_case(name) {
                st.name.clear();
            }
            store(s, folder, kind, &st);
            Ok(json!({ "deleted": name, "module": kind.name() }))
        }
        "update" => {
            let name = name_param(p, "name")?;
            let old = find_saved(s, folder, name)
                .ok_or_else(|| CmdError::Params(format!("no saved view named \"{name}\"")))?;
            let v = snapshot(s, folder, kind, &old.name, old.everywhere);
            put_saved(s, folder, kind, v)?;
            let mut st = state(s, folder, kind);
            st.name = old.name.clone();
            store(s, folder, kind, &st);
            Ok(json!({ "updated": old.name, "module": kind.name() }))
        }
        "reset" => {
            let name = name_param(p, "name")?;
            let v = canonical_view(kind, name)
                .ok_or_else(|| CmdError::Params("only built-in views can be reset".into()))?;
            let mut st = state(s, folder, kind);
            st.custom.remove(v);
            if st.view == v && st.name.is_empty() {
                st.arrange = None;
                // A Notes view's table settings belong to the folder's view: they go back too.
                if kind == ModuleKind::Notes {
                    let d = ModuleView::defaults(kind);
                    st.sort = d.sort;
                    st.columns = d.columns;
                    st.filter = d.filter;
                    st.rules = d.rules;
                    st.no_preview = d.no_preview;
                    st.no_grid = d.no_grid;
                    st.then_sort = d.then_sort;
                    st.group_by = d.group_by;
                    st.expand = d.expand;
                    st.formats = d.formats;
                    st.other = d.other;
                }
            }
            store(s, folder, kind, &st);
            clear_groups(s, folder);
            Ok(json!({ "reset": v, "module": kind.name() }))
        }
        other => {
            let clipped: String = other.chars().take(20).collect();
            Err(CmdError::Params(format!(
                "`op` must be apply, new, copy, rename, delete, update or reset, not `{clipped}`"
            )))
        }
    }
}

/// `view.saveViewAs` in a module folder: without `confirm` the Save View dialog opens; with
/// `{"name", "confirm": true}` the current view is saved under that name (replacing a saved view of the
/// same name) for this folder, or with `"scope": "all"` for every folder of the module.
pub(crate) fn save_view_as(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let name = p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    if canonical_view(kind, &name).is_some() {
        return Err(exists_error(&name));
    }
    if p.get("confirm").and_then(Value::as_bool) != Some(true) {
        let clipped: String = name.chars().take(MAX_VIEW_NAME).collect();
        return Ok(dialog(
            s,
            "saveModuleView",
            json!({
                "folder": folder.0,
                "module": kind.name(),
                "name": clipped,
                "noun": kind.item_noun(),
                "allFolders": scope_label(kind, true),
            }),
        ));
    }
    let name = check_name(kind, &name)?;
    let everywhere = scope_everywhere(p)?;
    // Same name, same scope: the saved view is replaced. Same name in the other scope: one of the two
    // would hide the other in this folder, so the name is refused.
    if let Some(other) = find_saved(s, folder, &name)
        && other.everywhere != everywhere
    {
        let place =
            if other.everywhere { scope_label(kind, true).to_lowercase() } else { "this folder".to_owned() };
        return Err(CmdError::Params(format!("a view named \"{}\" is already kept for {place}", other.name)));
    }
    let v = snapshot(s, folder, kind, &name, everywhere);
    put_saved(s, folder, kind, v)?;
    let mut st = state(s, folder, kind);
    st.name = name.clone();
    store(s, folder, kind, &st);
    Ok(json!({ "saved": name, "module": kind.name(), "scope": if everywhere { "all" } else { "folder" } }))
}

/// `view.viewSettings` in a module folder: the module's View Settings dialog.
pub(crate) fn view_settings(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let mut st = state(s, folder, kind);
    if !SETTINGS_KEYS.iter().any(|k| p.get(*k).is_some()) {
        let mut params = view_json(s, folder, kind, &st);
        if let Some(o) = params.as_object_mut() {
            o.insert("table".into(), super::table::summary(folder, kind, &st));
            if let Some(name) = p.get("savedView").and_then(Value::as_str) {
                o.insert("savedView".into(), json!(name));
            }
        }
        return Ok(dialog(s, "moduleViewSettings", params));
    }
    if kind != ModuleKind::Notes {
        if let Some(key) = NOTE_ONLY_KEYS.iter().find(|k| p.get(**k).is_some()) {
            return Err(CmdError::Params(format!(
                "`{key}` applies to notes folders; contact and task views take view.columns, view.sortBy, \
                 view.customFilter, view.conditionalFormatting and view.formatColumns"
            )));
        }
        super::table::apply_settings(s, folder, kind, p)?;
        let st = state(s, folder, kind);
        let mut out = view_json(s, folder, kind, &st);
        if let Some(o) = out.as_object_mut() {
            o.insert("table".into(), super::table::summary(folder, kind, &st));
        }
        return Ok(out);
    }
    let reverse_before = reversed(s, folder);
    let mut reverse = reverse_before;
    let expand_before = st.expand;
    let groups_before = st.group_by.clone();
    if let Some(v) = p.get("sortBy") {
        let list = note_fields(v, MAX_SORT_KEYS, "sortBy")?;
        let mut keys = list.into_iter();
        match keys.next() {
            Some(first) => {
                // The first key is the view's sort column; Reverse Sort turns it the way asked for.
                reverse = note_sort_descending(&first.column) != first.descending;
                st.sort = first.column;
            }
            None => {
                st.sort.clear();
                reverse = false;
            }
        }
        st.then_sort = keys.collect();
    }
    if let Some(v) = p.get("groupBy") {
        st.group_by = note_fields(v, MAX_GROUP_LEVELS, "groupBy")?;
    }
    if let Some(v) = p.get("expandGroups") {
        st.expand = v.as_str().and_then(ExpandDefault::parse).ok_or_else(|| {
            CmdError::Params("`expandGroups` must be expanded, collapsed or lastViewed".into())
        })?;
    }
    for (key, slot) in [("rowFont", &mut st.other.row_font), ("columnFont", &mut st.other.column_font)] {
        if let Some(v) = p.get(key) {
            let n = v.as_u64().ok_or_else(|| CmdError::Params(format!("`{key}` must be a font size")))?;
            *slot = mailcraft_model::view::clamp_font(u8::try_from(n).unwrap_or(u8::MAX));
        }
    }
    if let Some(v) = p.get("formats") {
        st.formats = if v.is_null() {
            BTreeMap::new()
        } else {
            serde_json::from_value::<BTreeMap<String, super::mail::ColumnFormat>>(v.clone())
                .map_err(|e| CmdError::Params(format!("`formats`: {e}")))?
        };
        if st.formats.keys().any(|k| note_column(k).is_none()) {
            return Err(CmdError::Params("`formats` names a column the Notes tables do not have".into()));
        }
    }
    if let Some(v) = p.get("sort") {
        st.sort = match v {
            Value::Null => String::new(),
            Value::String(k) if k.trim().is_empty() => String::new(),
            Value::String(k) => note_column(k)
                .ok_or_else(|| CmdError::Params(format!("unknown Notes column `{}`", clip_key(k))))?
                .to_owned(),
            _ => return Err(CmdError::Params("`sort` must be a column key".into())),
        };
    }
    if let Some(v) = p.get("columns") {
        let list = v.as_array().ok_or_else(|| CmdError::Params("`columns` must be a list".into()))?;
        let mut cols: Vec<String> = Vec::new();
        for c in list.iter().take(NOTE_COLUMNS.len().saturating_mul(4)) {
            let key = c.as_str().and_then(note_column).ok_or_else(|| {
                CmdError::Params(format!("unknown Notes column {}", clip_key(&c.to_string())))
            })?;
            if !cols.iter().any(|x| x == key) {
                cols.push(key.to_owned());
            }
        }
        if !cols.is_empty() && !cols.iter().any(|c| c == "subject") {
            return Err(CmdError::Params("the Subject column cannot be hidden".into()));
        }
        st.columns = cols;
    }
    if let Some(v) = p.get("filter") {
        st.filter = if v.is_null() {
            ItemFilter::default()
        } else {
            serde_json::from_value::<ItemFilter>(v.clone())
                .map_err(|e| CmdError::Params(format!("`filter`: {e}")))?
                .sanitized()
        };
    }
    if let Some(v) = p.get("rules") {
        let rules: Vec<ItemRule> = if v.is_null() {
            Vec::new()
        } else {
            serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("`rules`: {e}")))?
        };
        st.rules = sanitize_rules(rules);
    }
    if let Some(v) = p.get("autoPreview") {
        st.no_preview =
            !v.as_bool().ok_or_else(|| CmdError::Params("`autoPreview` must be true or false".into()))?;
    }
    if let Some(v) = p.get("gridLines") {
        st.no_grid =
            !v.as_bool().ok_or_else(|| CmdError::Params("`gridLines` must be true or false".into()))?;
    }
    let mut st = st.sanitized(kind);
    // `sanitized` keeps a saved view's name only by its text; the stored state was already checked.
    st.name = state(s, folder, kind).name;
    let regroup = st.group_by != groups_before || st.expand != expand_before;
    store(s, folder, kind, &st);
    if reverse != reverse_before {
        set_settings(s, folder, |v| {
            v.order = if reverse { SortOrder::Ascending } else { SortOrder::Descending }
        });
    }
    if regroup {
        open_groups(s, folder);
    }
    // While a saved view shows, the change is kept in that view too.
    sync_saved(s, folder)?;
    Ok(view_json(s, folder, kind, &st))
}

/// A list of Notes sort or group fields (`[{"field", "dir"}]` or plain keys of [`NOTE_COLUMNS`]): at most
/// `max`, none twice.
fn note_fields(v: &Value, max: usize, what: &str) -> Result<Vec<super::table::SortField>, CmdError> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    let list = v.as_array().ok_or_else(|| CmdError::Params(format!("`{what}` must be a list of fields")))?;
    if list.len() > max {
        return Err(CmdError::Params(format!("`{what}` takes at most {max} fields")));
    }
    let mut out: Vec<super::table::SortField> = Vec::new();
    for f in list {
        let (id, dir) = super::table::field_entry(f, what)?;
        let key = note_column(id)
            .ok_or_else(|| CmdError::Params(format!("unknown Notes column `{}`", clip_key(id))))?;
        let descending = match dir {
            None => note_sort_descending(key),
            Some("asc") => false,
            Some("desc") => true,
            Some(other) => {
                return Err(CmdError::Params(format!(
                    "`dir` must be asc or desc, not `{}`",
                    clip_key(other)
                )));
            }
        };
        if out.iter().any(|x| x.column == key) {
            return Err(CmdError::Params(format!("`{what}` names {} twice", note_column_label(key))));
        }
        out.push(super::table::SortField { column: key.to_owned(), descending });
    }
    Ok(out)
}

/// `view.formatColumns` in a Notes table: without `column` the Format Columns dialog opens; with it the
/// column's `label` and `align` change (empty strings back to the defaults).
pub(crate) fn note_format_columns(s: &mut Session, folder: FolderId, p: &Value) -> CmdResult {
    let kind = ModuleKind::Notes;
    let mut st = state(s, folder, kind);
    let Some(id) = p.get("column").and_then(Value::as_str) else {
        let list: Vec<Value> = note_shown(&st)
            .into_iter()
            .map(|(k, label, align)| {
                json!({
                    "id": k,
                    "label": label,
                    "defaultLabel": note_column_label(k),
                    "format": "text",
                    "formats": [{"id": "text", "label": "Text"}],
                    "align": align,
                    "custom": st.formats.contains_key(k),
                })
            })
            .collect();
        return Ok(dialog(
            s,
            "viewFormat",
            json!({ "folder": folder.0, "kind": "columns", "columns": list, "module": kind.name() }),
        ));
    };
    let key = note_column(id)
        .ok_or_else(|| CmdError::Params(format!("unknown Notes column `{}`", clip_key(id))))?;
    let mut f = st.formats.get(key).cloned().unwrap_or_default();
    if let Some(label) = p.get("label").and_then(Value::as_str) {
        if label.trim().chars().count() > MAX_VIEW_NAME {
            return Err(CmdError::Params(format!("column labels are at most {MAX_VIEW_NAME} characters")));
        }
        f.label = label.trim().to_owned();
    }
    if let Some(align) = p.get("align").and_then(Value::as_str) {
        if !matches!(align, "left" | "center" | "right" | "") {
            return Err(CmdError::Params(format!(
                "`align` must be left, center or right, not `{}`",
                clip_key(align)
            )));
        }
        f.align = align.to_owned();
    }
    st.formats.insert(key.to_owned(), f);
    let name = st.name.clone();
    let mut st = st.sanitized(kind);
    st.name = name;
    store(s, folder, kind, &st);
    sync_saved(s, folder)?;
    let now = note_shown(&st).into_iter().find(|(k, _, _)| *k == key);
    Ok(json!({
        "column": key,
        "label": now.as_ref().map_or(note_column_label(key).to_owned(), |c| c.1.clone()),
        "align": now.as_ref().map_or("left", |c| c.2),
    }))
}

/// Keys of `view.viewSettings` that change a view's advanced settings (without them the dialog opens).
pub const SETTINGS_KEYS: [&str; 12] = [
    "sort",
    "columns",
    "filter",
    "rules",
    "autoPreview",
    "gridLines",
    "sortBy",
    "groupBy",
    "expandGroups",
    "rowFont",
    "columnFont",
    "formats",
];

/// Keys only the Notes views take (People and Tasks views set these with their own commands).
const NOTE_ONLY_KEYS: [&str; 5] = ["sort", "columns", "filter", "rules", "formats"];

/// Notes List / Last 7 Days: the sort keys in effect, `(column, descending)`: the sort column (Created, newest
/// first, unless one was picked; Reverse Sort turns it around), then the Then by keys.
pub fn note_sort_keys(st: &ModuleView, reversed: bool) -> Vec<super::table::SortField> {
    let first = if st.sort.is_empty() { "created" } else { st.sort.as_str() };
    let mut out = vec![super::table::SortField {
        column: first.to_owned(),
        descending: note_sort_descending(&st.sort) != reversed,
    }];
    out.extend(st.then_sort.iter().cloned());
    out
}

/// Whether a Notes table view shows Group By groups (fields set, Show in Groups on).
pub fn notes_grouped(s: &Session, folder: FolderId, st: &ModuleView) -> bool {
    st.view != "Icon" && !st.group_by.is_empty() && show_groups(s, folder)
}

/// Label of a Notes column in a group header ("Color" for the icon column, whose value is the colour).
pub fn note_group_label(key: &str) -> &'static str {
    if key == "icon" { "Color" } else { note_column_label(key) }
}

/// The Notes table columns as drawn: (key, label, alignment) with Format Columns applied.
pub fn note_shown(st: &ModuleView) -> Vec<(&'static str, String, &'static str)> {
    note_columns(st)
        .into_iter()
        .map(|k| {
            let f = st.formats.get(k);
            let label = f
                .map(|f| f.label.clone())
                .filter(|l| !l.trim().is_empty())
                .unwrap_or_else(|| note_column_label(k).to_owned());
            let align = match f.map(|f| f.align.as_str()) {
                Some("center") => "center",
                Some("right") => "right",
                Some("left") => "left",
                _ if k == "icon" => "center",
                _ => "left",
            };
            (k, label, align)
        })
        .collect()
}

fn clip_key(k: &str) -> String {
    k.chars().take(40).collect()
}

fn set_settings(s: &mut Session, folder: FolderId, f: impl FnOnce(&mut ViewSettings)) {
    f(s.mailbox.views.entry(folder).or_default());
    if s.mailbox.views.get(&folder).is_some_and(|v| *v == ViewSettings::default()) {
        s.mailbox.views.remove(&folder);
    }
}

/// Whether Reset View has something to put back in the folder's current view: View Settings changes to it
/// (columns, sort keys, groups, filter, rules, formats, Other Settings, an arrangement picked with Arrange
/// By), Reverse Sort, Show in Groups or tighter spacing, collapsed groups, or Notes icons dragged off the
/// grid. Which view is shown, the reading pane and the message preview are not part of it.
pub fn view_customized(s: &Session, folder: FolderId) -> bool {
    let Some((kind, st)) = current(s, folder) else { return false };
    let collapsed =
        s.view.collapsed_groups.get(&folder).is_some_and(|g| g.iter().any(|e| super::is_group_label(e)));
    let dragged = kind == ModuleKind::Notes && !crate::cmd::notes::positions(s, folder).is_empty();
    if !st.name.is_empty() && find_saved(s, folder, &st.name).is_some() {
        // A saved view keeps every change made while it shows (`sync_saved`): only collapsed groups, tighter
        // spacing and dragged Notes icons differ from it.
        return collapsed || dragged || settings(s, folder).tighter_spacing;
    }
    let mut base = ModuleView::defaults(kind);
    base.view = st.view.clone();
    let own = !super::table::custom(&st).is_empty()
        || st.arrange.is_some()
        || st.sort != base.sort
        || st.then_sort != base.then_sort
        || st.group_by != base.group_by
        || st.expand != base.expand
        || st.columns != base.columns
        || st.formats != base.formats
        || st.filter != base.filter
        || st.rules != base.rules
        || st.no_preview != base.no_preview
        || st.no_grid != base.no_grid
        || st.other != base.other;
    let v = settings(s, folder);
    let d = ViewSettings::default();
    let arranged = v.order != d.order || v.show_in_groups != d.show_in_groups || v.tighter_spacing;
    own || arranged || collapsed || dragged
}

/// Reset View in a module folder: the current view loses its View Settings changes (see [`view_customized`])
/// and the folder stays on it; the view chosen, the reading pane, the message preview, the Notes icon
/// arrangement and the custom views saved for the folder stay. While a saved view shows, the folder goes back
/// to that view as it was saved (with its groups opened as it says); the saved view itself is never changed.
pub(crate) fn reset_current(s: &mut Session, folder: FolderId, kind: ModuleKind) -> Result<(), CmdError> {
    let mut st = state(s, folder, kind);
    if !st.name.is_empty()
        && let Some(saved) = find_saved(s, folder, &st.name)
    {
        if kind == ModuleKind::Notes {
            crate::cmd::notes::clear_positions(s, folder);
        }
        clear_groups(s, folder);
        set_settings(s, folder, |v| v.tighter_spacing = false);
        apply_saved(s, folder, kind, &saved);
        return Ok(());
    }
    st.custom.remove(&super::table::active_key(&st));
    st.arrange = None;
    let d = ModuleView::defaults(kind);
    st.sort = d.sort;
    st.then_sort = d.then_sort;
    st.group_by = d.group_by;
    st.expand = d.expand;
    st.columns = d.columns;
    st.formats = d.formats;
    st.filter = d.filter;
    st.rules = d.rules;
    st.no_preview = d.no_preview;
    st.no_grid = d.no_grid;
    st.other = d.other;
    store(s, folder, kind, &st);
    set_settings(s, folder, |v| {
        let d = ViewSettings::default();
        v.order = d.order;
        v.show_in_groups = d.show_in_groups;
        v.tighter_spacing = d.tighter_spacing;
    });
    if kind == ModuleKind::Notes {
        crate::cmd::notes::clear_positions(s, folder);
    }
    clear_groups(s, folder);
    sync_saved(s, folder)
}

/// Reset View over a mail folder's view state: the mail view, collapsed groups and the other reserved
/// entries go, but custom views saved for the folder stay (they are views to choose from, not the current
/// state).
pub(crate) fn reset_keeping_saved(s: &mut Session, folder: FolderId) {
    if let Some(list) = s.view.collapsed_groups.get_mut(&folder) {
        list.retain(|e| e.starts_with(MOD_SAVED));
        if list.is_empty() {
            s.view.collapsed_groups.remove(&folder);
        }
    }
}

/// Opens a view's groups as its Expand/collapse default says: all expanded (collapsed groups forgotten), all
/// collapsed, or as they were left.
pub(crate) fn open_groups(s: &mut Session, folder: FolderId) {
    let Some((kind, st)) = current(s, folder) else { return };
    let expand = if kind == ModuleKind::Notes { st.expand } else { super::table::custom(&st).expand };
    match expand {
        ExpandDefault::AsLastViewed => {}
        ExpandDefault::AllExpanded => clear_groups(s, folder),
        ExpandDefault::AllCollapsed => {
            clear_groups(s, folder);
            let labels = groups(s, folder);
            if !labels.is_empty() {
                s.view.collapsed_groups.entry(folder).or_default().extend(labels);
            }
        }
    }
}

/// Group labels change meaning with the view or arrangement: drops collapsed groups (keeps reserved state).
pub(crate) fn clear_groups(s: &mut Session, folder: FolderId) {
    if let Some(list) = s.view.collapsed_groups.get_mut(&folder) {
        list.retain(|e| !super::is_group_label(e));
        if list.is_empty() {
            s.view.collapsed_groups.remove(&folder);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use crate::{Notice, Selection};
    use mailcraft_model::{FolderRole, ListFilter};

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn select_folder(s: &mut Session, r: FolderRole) -> FolderId {
        let f = role(s, r);
        s.selection = Selection { folder: f, items: vec![], focus: None };
        f
    }

    fn select_item(s: &mut Session, id: u64) {
        s.selection.items = vec![ItemId(id)];
        s.selection.focus = Some(ItemId(id));
    }

    fn task(s: &mut Session, p: Value) -> u64 {
        run(s, "tasks.new", p)["id"].as_u64().unwrap()
    }

    fn ids(rows: &[ViewRow]) -> Vec<u64> {
        rows.iter().map(|r| r.id.0).collect()
    }

    fn groups_of(rows: &[ViewRow]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in rows.iter().filter_map(|r| r.group.clone()) {
            if !out.contains(&g) {
                out.push(g);
            }
        }
        out
    }

    #[test]
    fn change_view_in_every_module() {
        let mut s = session();
        let contacts = select_folder(&mut s, FolderRole::Contacts);
        assert_eq!(state(&s, contacts, ModuleKind::People).view, "People");
        for v in PEOPLE_VIEWS {
            assert_eq!(run(&mut s, "view.changeView", json!({"name": v}))["view"], v);
            assert_eq!(state(&s, contacts, ModuleKind::People).view, v);
        }
        // Manage Views applies lower-cased names.
        run(&mut s, "view.changeView", json!({"name": "business card"}));
        assert_eq!(state(&s, contacts, ModuleKind::People).view, "Business Card");
        assert!(matches!(s.run("view.changeView", &json!({"name": "Detailed"})), Err(CmdError::Params(_))));

        let tasks = select_folder(&mut s, FolderRole::Tasks);
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, "To-Do List");
        for v in TASK_VIEWS {
            run(&mut s, "view.changeView", json!({"name": v}));
            assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, v);
        }
        let notes = select_folder(&mut s, FolderRole::Notes);
        for v in NOTE_VIEWS {
            run(&mut s, "view.changeView", json!({"name": v}));
            assert_eq!(state(&s, notes, ModuleKind::Notes).view, v);
        }
        // Each folder keeps its own view.
        assert_eq!(state(&s, contacts, ModuleKind::People).view, "Business Card");
        // Mail folders keep the mail layouts.
        select_folder(&mut s, FolderRole::Inbox);
        run(&mut s, "view.changeView", json!({"name": "Single"}));
        assert!(s.run("view.changeView", &json!({"name": "Phone"})).is_err());
    }

    #[test]
    fn manage_views_lists_module_views_and_reset_restores_defaults() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        // Tasks offer Reset View on an unchanged view too (as observed); it changes nothing then.
        assert!(s.enabled("view.resetView").is_none());
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(state(&s, tasks, ModuleKind::Tasks), ModuleView::defaults(ModuleKind::Tasks));
        let contacts = select_folder(&mut s, FolderRole::Contacts);
        assert!(matches!(s.run("view.resetView", &json!({})), Err(CmdError::Disabled(_))), "{contacts:?}");
        select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.manageViews", json!({}));
        let Some(Notice::OpenDialog(name, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(name, "manageViews");
        assert_eq!(p["views"].as_array().unwrap().len(), 11);
        assert_eq!(p["current"], "To-Do List");
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert!(s.enabled("view.resetView").is_none());
        run(&mut s, "view.resetView", json!({}));
        let mut detailed = ModuleView::defaults(ModuleKind::Tasks);
        detailed.view = "Detailed".into();
        assert_eq!(state(&s, tasks, ModuleKind::Tasks), detailed, "Reset View stays on the view");
    }

    /// Regression: Reset View in a tasks folder used to wipe the whole module state (the folder jumped back to
    /// the To-Do List and lost its preview and reading pane). It now resets only the current view's
    /// customisations, as Manage All Views › Reset does, and stays on that view.
    #[test]
    fn reset_view_in_tasks_resets_only_the_current_view() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        run(&mut s, "view.columns", json!({"columns": ["subject", "dueDate"]}));
        run(&mut s, "view.sortBy", json!({"field": "subject", "dir": "desc"}));
        run(&mut s, "view.customFilter", json!({"filter": {"words": "x"}}));
        run(&mut s, "view.arrangeBy", json!({"key": "importance"}));
        run(&mut s, "view.messagePreview", json!({"lines": 2}));
        run(&mut s, "view.readingPane", json!({"position": "bottom"}));
        run(&mut s, "view.reverseSort", json!({}));
        // Another view's changes are not touched.
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.columns", json!({"columns": ["subject"]}));
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        let r = run(&mut s, "view.resetView", json!({}));
        assert_eq!(r["view"], "Simple List");
        let st = state(&s, tasks, ModuleKind::Tasks);
        assert_eq!(st.view, "Simple List", "the folder stays on its view");
        assert_eq!(st.arrange, None);
        assert!(!st.custom.contains_key("Simple List"), "columns, sort and filter go");
        assert_eq!(st.custom.get("Detailed").map(|c| c.columns.len()), Some(1), "other views keep theirs");
        assert_eq!(st.preview_lines, 2, "the message preview stays");
        assert_eq!(st.reading_pane, ReadingPanePos::Bottom, "the reading pane stays");
        assert!(!reversed(&s, tasks), "Reverse Sort goes");
        assert_eq!(super::super::table::columns(&s, tasks).len(), 5);
    }

    #[test]
    fn reset_view_keeps_saved_custom_views() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.saveViewAs", json!({"name": "My tasks", "confirm": true}));
        assert_eq!(saved_views(&s, tasks).len(), 1);
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, "Simple List");
        let saved = saved_views(&s, tasks);
        assert_eq!(saved.len(), 1, "Reset View must not delete saved views");
        assert_eq!(saved[0].name, "My tasks");
    }

    #[test]
    fn reset_view_on_a_saved_view_goes_back_to_it_and_never_rewrites_it() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        run(&mut s, "view.columns", json!({"columns": ["subject", "dueDate"]}));
        run(&mut s, "view.sortBy", json!({"field": "subject", "dir": "desc"}));
        run(&mut s, "view.customFilter", json!({"filter": {"words": "x"}}));
        run(&mut s, "view.reverseSort", json!({}));
        for everywhere in [false, true] {
            let name = if everywhere { "Everywhere" } else { "Mine" };
            let scope = if everywhere { "all" } else { "folder" };
            run(&mut s, "view.saveViewAs", json!({"name": name, "scope": scope, "confirm": true}));
            let before = find_saved(&s, tasks, name).unwrap();
            run(&mut s, "view.changeView", json!({"name": "Detailed"}));
            run(&mut s, "view.changeView", json!({"name": name}));
            assert_eq!(state(&s, tasks, ModuleKind::Tasks).name, name);
            // Nothing differs from the saved view: Reset View has nothing to put back.
            assert!(!view_customized(&s, tasks), "{name}");
            run(&mut s, "view.resetView", json!({}));
            let after = find_saved(&s, tasks, name).unwrap();
            assert_eq!(after, before, "Reset View must not write defaults into the saved view {name}");
            let st = state(&s, tasks, ModuleKind::Tasks);
            assert_eq!(st.name, name, "the folder stays on the saved view");
            assert_eq!(super::super::table::columns(&s, tasks).len(), 2, "with its columns");
            assert!(reversed(&s, tasks), "and its Reverse Sort");
        }
    }

    #[test]
    fn task_views_filter_and_group() {
        let mut s = session(); // Friday 2026-10-09
        let folder = select_folder(&mut s, FolderRole::Tasks);
        let overdue = task(&mut s, json!({"subject": "Overdue", "due": "2026-10-05"}));
        let today = task(&mut s, json!({"subject": "Today", "due": "2026-10-09", "priority": "high"}));
        let soon = task(&mut s, json!({"subject": "Soon", "due": "2026-10-12"}));
        let later =
            task(&mut s, json!({"subject": "Later", "due": "2026-12-01", "categories": ["Red category"]}));
        let undated = task(&mut s, json!({"subject": "Undated"}));
        let done = task(&mut s, json!({"subject": "Done", "due": "2026-10-01", "complete": true}));
        s.mailbox.tasks.get_mut(&ItemId(later)).unwrap().assigned_to = vec!["Bo <bo@example.com>".into()];
        let view = |s: &mut Session, name: &str| {
            run(s, "view.changeView", json!({"name": name}));
            rows(s, folder)
        };
        // To-Do List: open tasks grouped by due date.
        let r = view(&mut s, "To-Do List");
        assert_eq!(ids(&r), vec![overdue, today, soon, later, undated]);
        assert_eq!(groups_of(&r), vec!["Overdue", "Today", "Next Week", "Later", "No Date"]);
        // Simple List: everything, by due date, no groups.
        let r = view(&mut s, "Simple List");
        assert_eq!(ids(&r), vec![done, overdue, today, soon, later, undated]);
        assert!(r.iter().all(|r| r.group.is_none()));
        assert_eq!(ids(&view(&mut s, "Active")), vec![overdue, today, soon, later, undated]);
        assert_eq!(ids(&view(&mut s, "Completed")), vec![done]);
        assert_eq!(ids(&view(&mut s, "Today")), vec![overdue, today]);
        assert_eq!(ids(&view(&mut s, "Next 7 Days")), vec![today, soon]);
        assert_eq!(ids(&view(&mut s, "Overdue")), vec![overdue]);
        assert_eq!(ids(&view(&mut s, "Assigned")), vec![later]);
        assert!(view(&mut s, "Server Tasks").is_empty(), "a local folder has no server tasks");
        let r = view(&mut s, "Prioritized");
        assert_eq!(groups_of(&r), vec!["High", "Normal"]);
        assert_eq!(r.first().map(|r| r.id.0), Some(today));
        // Arrange By, Show in Groups, Reverse Sort.
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert_eq!(run(&mut s, "view.arrangeBy", json!({"key": "Categories"}))["arrangeBy"], "categories");
        assert_eq!(groups_of(&rows(&s, folder)), vec!["Red category", "(none)"]);
        run(&mut s, "view.arrangeBy", json!({"key": "flagStart"}));
        assert_eq!(state(&s, folder, ModuleKind::Tasks).arrange.as_deref(), Some("startDate"));
        assert!(s.run("view.arrangeBy", &json!({"key": "size"})).is_err());
        run(&mut s, "view.arrangeBy", json!({"key": "importance"}));
        run(&mut s, "view.showInGroups", json!({}));
        assert!(rows(&s, folder).iter().all(|r| r.group.is_none()));
        run(&mut s, "view.showInGroups", json!({}));
        let before = ids(&rows(&s, folder));
        assert_eq!(run(&mut s, "view.reverseSort", json!({}))["order"], "asc");
        let mut after = ids(&rows(&s, folder));
        after.reverse();
        assert_eq!(after, before);
    }

    #[test]
    fn tasks_arranged_by_categories_list_an_item_under_each_category() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Tasks);
        let both = task(&mut s, json!({"subject": "Both", "categories": ["Red category", "Blue category"]}));
        let red = task(&mut s, json!({"subject": "Red", "categories": ["Red category"]}));
        let none = task(&mut s, json!({"subject": "None"}));
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.arrangeBy", json!({"key": "categories"}));
        let r = rows(&s, folder);
        let pairs: Vec<(u64, Option<&str>)> = r.iter().map(|x| (x.id.0, x.group.as_deref())).collect();
        assert_eq!(
            pairs,
            vec![
                (both, Some("Blue category")),
                (both, Some("Red category")),
                (red, Some("Red category")),
                (none, Some("(none)")),
            ]
        );
        // Collapsing one header (named by its label) leaves the item's other group open.
        select_item(&mut s, both);
        run(&mut s, "view.collapseGroup", json!({"label": "Red category"}));
        let collapsed = &s.view.collapsed_groups[&folder];
        assert!(collapsed.iter().any(|g| g == "Red category"));
        assert!(!collapsed.iter().any(|g| g == "Blue category"));
    }

    #[test]
    fn expand_collapse_task_groups() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Tasks);
        let a = task(&mut s, json!({"subject": "A", "due": "2026-10-09"}));
        task(&mut s, json!({"subject": "B"}));
        assert!(matches!(s.run("view.collapseGroup", &json!({})), Err(CmdError::Disabled(_))));
        select_item(&mut s, a);
        assert_eq!(run(&mut s, "view.collapseGroup", json!({}))["group"], "Today");
        assert_eq!(
            s.view.collapsed_groups[&folder].iter().filter(|e| super::super::is_group_label(e)).count(),
            1
        );
        assert_eq!(run(&mut s, "view.collapseAll", json!({}))["collapsed"], 2);
        run(&mut s, "view.expandAll", json!({}));
        assert!(
            s.view
                .collapsed_groups
                .get(&folder)
                .is_none_or(|l| l.iter().all(|e| !super::super::is_group_label(e)))
        );
        // The module view survives group changes.
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.collapseAll", json!({}));
        run(&mut s, "view.expandAll", json!({}));
        assert_eq!(state(&s, folder, ModuleKind::Tasks).view, "Detailed");
    }

    #[test]
    fn people_arrangement_only_in_table_views() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Contacts);
        for (name, company) in [("Ann Lee", "Example Co"), ("Bo Ek", ""), ("Cy Day", "Example Co")] {
            run(&mut s, "people.new", json!({"fullName": name, "company": company}));
        }
        assert!(matches!(s.run("view.arrangeBy", &json!({"key": "company"})), Err(CmdError::Disabled(_))));
        assert!(s.enabled("view.expandAll").is_some(), "no groups in the People view");
        assert!(rows(&s, folder).iter().all(|r| r.group.is_none()));
        run(&mut s, "view.changeView", json!({"name": "List"}));
        // List groups by company by default.
        assert_eq!(groups_of(&rows(&s, folder)), vec!["Example Co", "(none)"]);
        assert!(s.enabled("view.expandAll").is_none());
        assert!(s.enabled("view.messagePreview").is_some(), "no message preview for contacts");
        assert_eq!(run(&mut s, "view.arrangeBy", json!({"key": "Location"}))["arrangeBy"], "location");
        assert_eq!(groups_of(&rows(&s, folder)), vec!["(none)"]);
        run(&mut s, "view.changeView", json!({"name": "Phone"}));
        assert!(rows(&s, folder).iter().all(|r| r.group.is_none()), "Phone is not grouped by default");
        assert_eq!(rows(&s, folder).len(), 3);
    }

    #[test]
    fn preview_reading_pane_and_spacing_per_module() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        assert_eq!(run(&mut s, "view.messagePreview", json!({"lines": 2}))["lines"], 2);
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).preview_lines, 2);
        let notes = select_folder(&mut s, FolderRole::Notes);
        assert_eq!(state(&s, notes, ModuleKind::Notes).reading_pane, ReadingPanePos::Off);
        assert!(matches!(s.run("view.messagePreview", &json!({"lines": 1})), Err(CmdError::Disabled(_))));
        run(&mut s, "view.readingPane", json!({"position": "bottom"}));
        assert_eq!(state(&s, notes, ModuleKind::Notes).reading_pane, ReadingPanePos::Bottom);
        assert_eq!(settings(&s, notes).reading_pane, ReadingPanePos::Bottom);
        assert_eq!(s.view.reading_pane, ReadingPanePos::Right, "the Mail default does not change");
        assert_eq!(run(&mut s, "view.tighterSpacing", json!({}))["on"], true);
        assert!(settings(&s, tasks).tighter_spacing);
        // A filter left on a mail folder does not leak into module folders.
        assert_eq!(settings(&s, notes).filter, ListFilter::All);
    }

    #[test]
    fn apply_view_and_saved_views() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let other = s.mailbox.create_folder(tasks, "Home").unwrap();
        run(&mut s, "view.changeView", json!({"name": "Overdue"}));
        run(&mut s, "view.applyViewTo", json!({}));
        let Some(Notice::OpenDialog(name, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(name, "applyModuleView");
        assert_eq!(p["folders"][0]["id"], other.0);
        let inbox = role(&s, FolderRole::Inbox);
        let r = run(&mut s, "view.applyViewTo", json!({"folders": [other.0, inbox.0]}));
        assert_eq!(r["applied"], 1, "only folders of the same kind take the view");
        assert_eq!(state(&s, other, ModuleKind::Tasks).view, "Overdue");
        // Save Current View As a New View (Tasks wait for a change first), then apply it.
        run(&mut s, "view.reverseSort", json!({}));
        assert_eq!(run(&mut s, "view.saveViewAs", json!({"name": "Mine"}))["dialog"], "saveModuleView");
        assert_eq!(run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}))["saved"], "Mine");
        assert!(s.run("view.saveViewAs", &json!({"name": "Detailed", "confirm": true})).is_err());
        assert!(s.run("view.saveViewAs", &json!({"name": " ", "confirm": true})).is_err());
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(saved_views(&s, tasks).len(), 1, "Reset View keeps the folder's saved views");
        run(&mut s, "view.reverseSort", json!({}));
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        // Reset View left the folder on Overdue, so that is the view saved as "Mine".
        assert_eq!(run(&mut s, "view.changeView", json!({"name": "mine"}))["view"], "Overdue");
        run(&mut s, "view.manageViews", json!({}));
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["views"].as_array().unwrap().last().unwrap(), "Mine");
        assert_eq!(run(&mut s, "view.viewSettings", json!({}))["dialog"], "moduleViewSettings");
    }

    #[test]
    fn manage_views_new_copy_rename_delete_and_scopes() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let other = s.mailbox.create_folder(tasks, "Home").unwrap();
        let mv = |s: &mut Session, p: Value| s.run("view.manageViews", &p);
        // New: a view of a built-in type, for this folder or every task folder.
        mv(&mut s, json!({"op": "new", "name": "Errands", "base": "detailed"})).unwrap();
        mv(&mut s, json!({"op": "new", "name": "Shared", "base": "Overdue", "scope": "all"})).unwrap();
        assert!(mv(&mut s, json!({"op": "new", "name": "errands"})).is_err(), "names are unique");
        assert!(mv(&mut s, json!({"op": "new", "name": "Completed"})).is_err(), "built-in names are taken");
        assert!(mv(&mut s, json!({"op": "new", "name": "X", "base": "Phone"})).is_err());
        assert!(mv(&mut s, json!({"op": "new", "name": "X", "scope": "everywhere"})).is_err());
        let names =
            |s: &Session, f: FolderId| saved_views(s, f).into_iter().map(|v| v.name).collect::<Vec<_>>();
        assert_eq!(names(&s, tasks), ["Errands", "Shared"]);
        assert_eq!(names(&s, other), ["Shared"], "only the all-folders view is usable elsewhere");
        // The dialog shows where each view can be used.
        mv(&mut s, json!({})).unwrap();
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["saved"][0]["usedOn"], "This folder");
        assert_eq!(p["saved"][1]["usedOn"], "All Task folders");
        // Apply, then the Change View choices check it.
        mv(&mut s, json!({"op": "apply", "name": "Errands"})).unwrap();
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, "Detailed");
        assert_eq!(current_view_name(&s, tasks), "Errands");
        let on: Vec<String> = view_choices(&s, tasks).into_iter().filter(|c| c.2).map(|c| c.0).collect();
        assert_eq!(on, ["Errands"]);
        // Copy (of a built-in view with this folder's column changes, or of a saved view), Rename, Delete.
        s.run("view.changeView", &json!({"name": "Simple List"})).unwrap();
        s.run("view.columns", &json!({"columns": ["subject", "priority"]})).unwrap();
        mv(&mut s, json!({"op": "copy", "name": "Simple List", "newName": "Lean"})).unwrap();
        mv(&mut s, json!({"op": "copy", "name": "Shared", "newName": "Mine too", "scope": "folder"}))
            .unwrap();
        mv(&mut s, json!({"op": "apply", "name": "Lean"})).unwrap();
        assert_eq!(crate::cmd::view::table::columns(&s, tasks).len(), 2);
        mv(&mut s, json!({"op": "rename", "name": "Lean", "newName": "Lean list"})).unwrap();
        assert_eq!(current_view_name(&s, tasks), "Lean list", "the shown view follows its new name");
        assert!(mv(&mut s, json!({"op": "rename", "name": "Detailed", "newName": "Z"})).is_err());
        assert!(mv(&mut s, json!({"op": "rename", "name": "Lean list", "newName": "Errands"})).is_err());
        mv(&mut s, json!({"op": "delete", "name": "Lean list"})).unwrap();
        assert_eq!(current_view_name(&s, tasks), "Simple List");
        assert!(mv(&mut s, json!({"op": "delete", "name": "Overdue"})).is_err(), "built-in views stay");
        mv(&mut s, json!({"op": "delete", "name": "Shared"})).unwrap();
        assert!(names(&s, other).is_empty());
        // Update stores the current view into a saved one; Reset clears a built-in view's changes.
        s.run("view.arrangeBy", &json!({"key": "folder"})).unwrap();
        mv(&mut s, json!({"op": "update", "name": "Errands"})).unwrap();
        let errands = saved_views(&s, tasks).into_iter().find(|v| v.name == "Errands").unwrap();
        assert_eq!(errands.state.arrange.as_deref(), Some("folder"));
        mv(&mut s, json!({"op": "reset", "name": "Simple List"})).unwrap();
        assert_eq!(crate::cmd::view::table::columns(&s, tasks).len(), 5);
        assert!(mv(&mut s, json!({"op": "reset", "name": "Errands"})).is_err());
        assert!(mv(&mut s, json!({"op": "fly"})).is_err());
        assert!(mv(&mut s, json!({"op": "apply"})).is_err());
        // Save As with "all": usable in every task folder.
        s.run("view.saveViewAs", &json!({"name": "Everywhere", "scope": "all", "confirm": true})).unwrap();
        assert_eq!(names(&s, other), ["Everywhere"]);
        assert_eq!(current_view_name(&s, tasks), "Everywhere");
    }

    #[test]
    fn a_saved_view_keeps_its_own_columns_apart_from_the_built_in_view() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let cols =
            |s: &Session| crate::cmd::view::table::columns(s, tasks).iter().map(|c| c.id).collect::<Vec<_>>();
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.columns", json!({"columns": ["subject", "status"]}));
        run(&mut s, "view.saveViewAs", json!({"name": "Two", "confirm": true}));
        // Changing the saved view's columns while it shows leaves the built-in Detailed view alone.
        run(&mut s, "view.columns", json!({"columns": ["subject"]}));
        assert_eq!(cols(&s), ["subject"]);
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert_eq!(cols(&s), ["subject", "status"]);
        // Showing the saved view again keeps the change made while it was shown.
        run(&mut s, "view.changeView", json!({"name": "Two"}));
        assert_eq!(cols(&s), ["subject"]);
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, "Detailed");
    }

    #[test]
    fn changes_made_while_a_saved_view_shows_are_kept_in_it() {
        // Regression: sort, filter, arrangement, Reverse Sort and Show in Groups changed while a saved view
        // was shown went back to the stored definition the next time the view was picked.
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let other = s.mailbox.create_folder(tasks, "Home").unwrap();
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        run(&mut s, "view.saveViewAs", json!({"name": "Shared", "scope": "all", "confirm": true}));
        run(&mut s, "view.sortBy", json!({"field": "subject"}));
        run(&mut s, "view.customFilter", json!({"filter": {"words": "budget"}}));
        run(&mut s, "view.arrangeBy", json!({"key": "folder"}));
        run(&mut s, "view.reverseSort", json!({}));
        run(&mut s, "view.showInGroups", json!({}));
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert!(crate::cmd::view::table::filter(&s, tasks).is_none(), "the built-in view has no filter");
        run(&mut s, "view.changeView", json!({"name": "Shared"}));
        let sort = crate::cmd::view::table::sort(&s, tasks);
        assert_eq!(sort.map(|f| f.column), Some("subject".to_owned()));
        assert!(crate::cmd::view::table::filter(&s, tasks).is_some_and(|f| f.words == "budget"));
        let st = state(&s, tasks, ModuleKind::Tasks);
        assert_eq!(st.arrange.as_deref(), Some("folder"));
        assert_eq!(settings(&s, tasks).order, SortOrder::Ascending);
        assert!(!settings(&s, tasks).show_in_groups);
        // A view kept for all folders brings the change to the other folders too.
        s.selection.folder = other;
        run(&mut s, "view.changeView", json!({"name": "Shared"}));
        assert!(crate::cmd::view::table::filter(&s, other).is_some_and(|f| f.words == "budget"));
        assert_eq!(state(&s, other, ModuleKind::Tasks).arrange.as_deref(), Some("folder"));
    }

    #[test]
    fn a_view_deleted_in_another_folder_leaves_no_orphan_settings() {
        // Regression: deleting or renaming an all-folders view from folder A left folder B reading and
        // writing the gone view's "@name" settings while its ribbon showed the built-in view.
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let other = s.mailbox.create_folder(tasks, "Home").unwrap();
        run(
            &mut s,
            "view.manageViews",
            json!({"op": "new", "name": "Shared", "base": "Simple List", "scope": "all"}),
        );
        s.selection.folder = other;
        run(&mut s, "view.changeView", json!({"name": "Shared"}));
        run(&mut s, "view.columns", json!({"columns": ["subject"]}));
        assert!(state(&s, other, ModuleKind::Tasks).custom.contains_key("@Shared"));
        s.selection.folder = tasks;
        run(&mut s, "view.manageViews", json!({"op": "delete", "name": "Shared"}));
        s.selection.folder = other;
        let st = state(&s, other, ModuleKind::Tasks);
        assert!(st.name.is_empty());
        assert!(!st.custom.keys().any(|k| k.starts_with('@')));
        assert_eq!(current_view_name(&s, other), "Simple List");
        // The built-in view's own changes show and Reset on it works.
        run(&mut s, "view.columns", json!({"columns": ["subject", "status"]}));
        assert_eq!(crate::cmd::view::table::columns(&s, other).len(), 2);
        run(&mut s, "view.manageViews", json!({"op": "reset", "name": "Simple List"}));
        assert_eq!(crate::cmd::view::table::columns(&s, other).len(), 5);
        // Renaming from another folder: the folder showing it falls back the same way.
        run(
            &mut s,
            "view.manageViews",
            json!({"op": "new", "name": "Wide", "base": "Detailed", "scope": "all"}),
        );
        run(&mut s, "view.changeView", json!({"name": "Wide"}));
        s.selection.folder = tasks;
        run(&mut s, "view.manageViews", json!({"op": "rename", "name": "Wide", "newName": "Wider"}));
        assert_eq!(current_view_name(&s, other), "Detailed");
        // Renaming in the folder that shows it keeps its settings under the new name.
        run(&mut s, "view.changeView", json!({"name": "Wider"}));
        run(&mut s, "view.columns", json!({"columns": ["subject"]}));
        run(&mut s, "view.manageViews", json!({"op": "rename", "name": "Wider", "newName": "Widest"}));
        assert_eq!(current_view_name(&s, tasks), "Widest");
        assert_eq!(crate::cmd::view::table::columns(&s, tasks).len(), 1);
    }

    #[test]
    fn manage_views_describes_built_in_and_saved_views() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        let get = |rows: &[(&str, String)], k: &str| rows.iter().find(|r| r.0 == k).map(|r| r.1.clone());
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        run(&mut s, "view.columns", json!({"columns": ["subject", "priority"]}));
        run(&mut s, "view.customFilter", json!({"filter": {"words": "report"}}));
        let rows = view_description(&s, tasks, "simple list").unwrap();
        assert_eq!(get(&rows, "Type of view").as_deref(), Some("Simple List"));
        assert_eq!(get(&rows, "Columns").as_deref(), Some("Subject, Priority"));
        assert!(get(&rows, "Filter").is_some_and(|f| f.contains("report")), "{rows:?}");
        assert_eq!(get(&rows, "Can be used on").as_deref(), Some("All Task folders"));
        // A built-in view this folder did not change has its own columns.
        let rows = view_description(&s, tasks, "Detailed").unwrap();
        assert_ne!(get(&rows, "Columns").as_deref(), Some("Subject, Priority"));
        assert_eq!(get(&rows, "Filter").as_deref(), Some("Off"));
        // A saved view: its base view, its own settings and where it can be used.
        run(&mut s, "view.saveViewAs", json!({"name": "Lean", "confirm": true}));
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        let rows = view_description(&s, tasks, "Lean").unwrap();
        assert_eq!(get(&rows, "Type of view").as_deref(), Some("Simple List"));
        assert_eq!(get(&rows, "Columns").as_deref(), Some("Subject, Priority"));
        assert_eq!(get(&rows, "Can be used on").as_deref(), Some("This folder"));
        assert!(view_description(&s, tasks, "Nope").is_none());
        // Views that are not tables: no column rows.
        let notes = select_folder(&mut s, FolderRole::Notes);
        let rows = view_description(&s, notes, "Icon").unwrap();
        assert!(get(&rows, "Columns").is_none());
        assert!(view_description(&s, role(&s, FolderRole::Inbox), "Compact").is_none());
    }

    #[test]
    fn save_view_as_refuses_a_name_kept_in_the_other_scope() {
        // Regression: saving for all folders under the name of a folder-only view hid the new view here.
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.reverseSort", json!({}));
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        let e = s.run("view.saveViewAs", &json!({"name": "mine", "scope": "all", "confirm": true}));
        assert!(e.is_err());
        run(&mut s, "view.manageViews", json!({"op": "new", "name": "Shared", "scope": "all"}));
        assert!(s.run("view.saveViewAs", &json!({"name": "Shared", "confirm": true})).is_err());
        // The same scope replaces the saved view.
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        let mine = saved_views(&s, tasks).into_iter().find(|v| v.name == "Mine").unwrap();
        assert_eq!(mine.state.view, "Detailed");
        assert_eq!(saved_views(&s, tasks).len(), 2);
    }

    #[test]
    fn arrange_by_is_greyed_where_a_view_has_no_arrangement() {
        let mut s = session();
        select_folder(&mut s, FolderRole::Contacts);
        assert!(s.enabled("view.arrangeBy").is_some(), "the People view has no arrangement");
        run(&mut s, "view.changeView", json!({"name": "Card"}));
        assert!(s.enabled("view.arrangeBy").is_some());
        run(&mut s, "view.changeView", json!({"name": "Phone"}));
        assert!(s.enabled("view.arrangeBy").is_none());
        select_folder(&mut s, FolderRole::Notes);
        assert!(s.enabled("view.arrangeBy").is_some());
        select_folder(&mut s, FolderRole::Tasks);
        assert!(s.enabled("view.arrangeBy").is_none());
        select_folder(&mut s, FolderRole::Inbox);
        assert!(s.enabled("view.arrangeBy").is_none());
    }

    #[test]
    fn save_view_as_waits_for_a_change_in_tasks() {
        let mut s = session();
        select_folder(&mut s, FolderRole::Tasks);
        assert!(s.enabled("view.saveViewAs").is_some(), "an unchanged Tasks view has nothing to save");
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert!(s.enabled("view.saveViewAs").is_none());
        // People and Notes offer it at once, as observed.
        select_folder(&mut s, FolderRole::Contacts);
        assert!(s.enabled("view.saveViewAs").is_none());
    }

    #[test]
    fn sort_by_in_a_mail_folder_sets_the_arrangement() {
        let mut s = session();
        let inbox = select_folder(&mut s, FolderRole::Inbox);
        let r = run(&mut s, "view.sortBy", json!({"field": "subject", "dir": "asc"}));
        assert_eq!(r["arrangeBy"], "subject");
        assert_eq!(settings(&s, inbox).order, SortOrder::Ascending);
        assert!(s.run("view.sortBy", &json!({"field": "mention"})).is_err(), "Mention does not sort");
        assert!(s.run("view.sortBy", &json!({})).is_err());
    }

    #[test]
    fn notes_views_and_icons() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Notes);
        let old = run(&mut s, "notes.new", json!({"text": "old"}))["id"].as_u64().unwrap();
        s.mailbox.notes.get_mut(&ItemId(old)).unwrap().modified.unix -= 30 * 86_400;
        s.mailbox.notes.get_mut(&ItemId(old)).unwrap().created.unix -= 30 * 86_400;
        let new = run(&mut s, "notes.new", json!({"text": "new"}))["id"].as_u64().unwrap();
        assert_eq!(ids(&rows(&s, folder)), vec![old, new], "Icon: oldest first");
        run(&mut s, "view.changeView", json!({"name": "Notes List"}));
        assert_eq!(ids(&rows(&s, folder)), vec![new, old]);
        run(&mut s, "view.changeView", json!({"name": "Last 7 Days"}));
        assert_eq!(ids(&rows(&s, folder)), vec![new]);
        // Icon arrangement from the ribbon (no view param) is stored and switches back to the Icon view.
        assert_eq!(run(&mut s, "notes.iconView", json!({"mode": "small"}))["view"]["iconMode"], "small");
        let st = state(&s, folder, ModuleKind::Notes);
        assert_eq!((st.view.as_str(), st.icon_mode), ("Icon", IconMode::Small));
        run(
            &mut s,
            "notes.iconView",
            json!({"mode": "large", "view": {"iconMode": "large", "linedUp": false}}),
        );
        assert!(!state(&s, folder, ModuleKind::Notes).lined_up);
        run(&mut s, "notes.lineUpIcons", json!({}));
        assert!(state(&s, folder, ModuleKind::Notes).lined_up);
    }

    #[test]
    fn conversation_and_module_state_coexist() {
        // Regression: Arrange By used to drop every reserved entry of the folder, Conversation Settings
        // dropped other reserved entries.
        let mut s = session();
        let inbox = select_folder(&mut s, FolderRole::Inbox);
        run(&mut s, "view.conversations", json!({"on": true}));
        run(&mut s, "view.conversationSettings", json!({"setting": "alwaysExpand", "on": true}));
        run(&mut s, "view.arrangeBy", json!({"key": "from"}));
        assert!(
            crate::cmd::view::conversation_state(&s, inbox).settings.get("alwaysExpand").unwrap_or(false)
        );
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        let list = s.view.collapsed_groups.entry(tasks).or_default();
        list.push("\u{1}conv.set:alwaysExpand=1".into());
        crate::cmd::view::modules::store(
            &mut s,
            tasks,
            ModuleKind::Tasks,
            &ModuleView { view: "Active".into(), ..ModuleView::defaults(ModuleKind::Tasks) },
        );
        assert!(s.view.collapsed_groups[&tasks].iter().any(|e| e.starts_with("\u{1}conv.")));
        assert_eq!(state(&s, tasks, ModuleKind::Tasks).view, "Active");
    }

    #[test]
    fn hostile_stored_state_falls_back() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        s.view.collapsed_groups.insert(
            tasks,
            vec![
                "\u{1}mod.state={\"view\":\"Bogus\",\"arrange\":\"size\",\"previewLines\":200}".into(),
                "\u{1}mod.saved=not json".into(),
            ],
        );
        let st = state(&s, tasks, ModuleKind::Tasks);
        assert_eq!((st.view.as_str(), st.arrange, st.preview_lines), ("To-Do List", None, 3));
        assert!(saved_views(&s, tasks).is_empty());
        assert!(rows(&s, tasks).is_empty());
    }

    fn note(s: &mut Session, text: &str, days_ago: i64, cats: &[&str]) -> u64 {
        let id = run(s, "notes.new", json!({"text": text, "categories": cats}))["id"].as_u64().unwrap();
        let n = s.mailbox.notes.get_mut(&ItemId(id)).unwrap();
        n.created.unix -= days_ago * 86_400;
        n.modified.unix -= days_ago * 86_400;
        id
    }

    #[test]
    fn notes_table_sorts_by_header_and_filters() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Notes);
        let b = note(&mut s, "Banana bread\nflour", 3, &["Red category"]);
        let a = note(&mut s, "apple pie", 1, &[]);
        let c = note(&mut s, "Cherry jam", 2, &["Blue category"]);
        run(&mut s, "view.changeView", json!({"name": "Notes List"}));
        assert_eq!(ids(&rows(&s, folder)), vec![a, c, b], "newest first");
        // Header clicks: Subject sorts A to Z, again Z to A; another column starts in its own order.
        let r = run(&mut s, "view.arrangeBy", json!({"key": "subject"}));
        assert_eq!((r["sort"].as_str(), r["reverse"].as_bool()), (Some("subject"), Some(false)));
        assert_eq!(ids(&rows(&s, folder)), vec![a, b, c]);
        assert_eq!(run(&mut s, "view.arrangeBy", json!({"key": "Subject"}))["reverse"], true);
        assert_eq!(ids(&rows(&s, folder)), vec![c, b, a]);
        run(&mut s, "view.arrangeBy", json!({"key": "modified"}));
        assert_eq!(ids(&rows(&s, folder)), vec![a, c, b]);
        run(&mut s, "view.arrangeBy", json!({"key": "categories"}));
        assert_eq!(ids(&rows(&s, folder)), vec![a, c, b], "no category, Blue, Red");
        assert!(matches!(s.run("view.arrangeBy", &json!({"key": "size"})), Err(CmdError::Params(_))));
        // View Settings › Filter applies to every Notes view.
        let r = run(&mut s, "view.viewSettings", json!({"filter": {"words": "BREAD"}}));
        assert_eq!(r["filterText"], "\"BREAD\" in the subject");
        assert_eq!(ids(&rows(&s, folder)), vec![b]);
        run(&mut s, "view.viewSettings", json!({"filter": {"words": "flour", "inBody": true}}));
        assert_eq!(ids(&rows(&s, folder)), vec![b]);
        run(&mut s, "view.viewSettings", json!({"filter": {"category": "blue category"}}));
        assert_eq!(ids(&rows(&s, folder)), vec![c]);
        run(&mut s, "view.viewSettings", json!({"filter": {"days": 2}}));
        assert_eq!(ids(&rows(&s, folder)).len(), 2);
        run(&mut s, "view.viewSettings", json!({"filter": null}));
        assert_eq!(ids(&rows(&s, folder)).len(), 3);
        // The Icon view is not sorted by a column.
        run(&mut s, "view.changeView", json!({"name": "Icon"}));
        assert!(matches!(s.run("view.arrangeBy", &json!({"key": "subject"})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn notes_advanced_view_settings() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Notes);
        let a = note(&mut s, "Budget\nQ3 numbers", 0, &[]);
        run(&mut s, "view.changeView", json!({"name": "Notes List"}));
        let r = run(
            &mut s,
            "view.viewSettings",
            json!({
                "columns": ["subject", "modified", "Icon", "subject"],
                "sort": "Created",
                "autoPreview": false,
                "gridLines": false,
                "rules": [
                    {"name": "Money", "on": true, "filter": {"words": "budget"}, "bold": true, "color": "#C42B1C"},
                    {"name": "", "on": true, "filter": {"words": "x"}},
                ],
            }),
        );
        assert_eq!(r["columns"], json!(["subject", "modified", "icon"]));
        assert_eq!(r["sort"], "created");
        assert_eq!((r["autoPreview"].as_bool(), r["gridLines"].as_bool()), (Some(false), Some(false)));
        let st = state(&s, folder, ModuleKind::Notes);
        assert_eq!(st.rules.len(), 1, "unnamed rules are dropped");
        let n = s.mailbox.notes[&ItemId(a)].clone();
        let style = crate::cmd::notes::row_style(&s, &st, &n);
        assert!(style.bold);
        assert_eq!(style.color.as_deref(), Some("#c42b1c"));
        assert!(s.enabled("view.resetView").is_none(), "the settings customise the view");
        // Bad values are refused; the defaults read back as the defaults.
        assert!(s.run("view.viewSettings", &json!({"columns": ["created"]})).is_err(), "Subject stays");
        assert!(s.run("view.viewSettings", &json!({"columns": ["nope"]})).is_err());
        assert!(s.run("view.viewSettings", &json!({"sort": 5})).is_err());
        assert!(s.run("view.viewSettings", &json!({"filter": "words"})).is_err());
        assert!(s.run("view.viewSettings", &json!({"gridLines": "yes"})).is_err());
        let r = run(
            &mut s,
            "view.viewSettings",
            json!({"columns": ["icon", "subject", "created", "categories"]}),
        );
        assert_eq!(r["columns"], json!(NOTE_DEFAULT_COLUMNS));
        assert!(state(&s, folder, ModuleKind::Notes).columns.is_empty());
        // Tasks folders take the filter and rules but no Notes columns.
        select_folder(&mut s, FolderRole::Tasks);
        assert!(s.run("view.viewSettings", &json!({"columns": ["subject"]})).is_err());
        assert_eq!(run(&mut s, "view.viewSettings", json!({}))["dialog"], "moduleViewSettings");
    }

    #[test]
    fn saved_module_views_in_change_view_and_manage_views() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Notes);
        let names = |s: &Session| -> Vec<(String, bool)> { change_view_items(s, folder) };
        assert_eq!(names(&s).iter().filter(|(_, on)| *on).count(), 1);
        assert_eq!(names(&s)[0], ("Icon".to_owned(), true));
        run(&mut s, "view.changeView", json!({"name": "Notes List"}));
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        // The saved view shows in Change View and is the one checked.
        assert_eq!(names(&s).last(), Some(&("Mine".to_owned(), true)));
        run(&mut s, "view.changeView", json!({"name": "Icon"}));
        assert_eq!(names(&s).last(), Some(&("Mine".to_owned(), false)));
        // Manage Views: New, Copy, Rename, Modify (update), Delete, Reset.
        run(&mut s, "view.manageViews", json!({"op": "new", "name": "Week", "base": "last 7 days"}));
        run(
            &mut s,
            "view.manageViews",
            json!({"op": "copy", "name": "Week", "newName": "Week 2", "scope": "all"}),
        );
        let all: Vec<(String, bool)> =
            saved_views(&s, folder).iter().map(|v| (v.name.clone(), v.everywhere)).collect();
        assert_eq!(
            all,
            vec![("Mine".to_owned(), false), ("Week".to_owned(), false), ("Week 2".to_owned(), true)]
        );
        run(&mut s, "view.manageViews", json!({"op": "apply", "name": "week"}));
        assert_eq!(state(&s, folder, ModuleKind::Notes).view, "Last 7 Days");
        run(&mut s, "view.manageViews", json!({"op": "rename", "name": "Week", "newName": "Fortnight"}));
        assert_eq!(
            current_name(&state(&s, folder, ModuleKind::Notes)),
            "Fortnight",
            "the shown view keeps up"
        );
        run(&mut s, "view.viewSettings", json!({"filter": {"words": "x"}}));
        run(&mut s, "view.manageViews", json!({"op": "update", "name": "Fortnight"}));
        let f = saved_views(&s, folder).into_iter().find(|v| v.name == "Fortnight").unwrap();
        assert_eq!(f.state.filter.words, "x");
        assert!(s.run("view.manageViews", &json!({"op": "rename", "name": "Icon", "newName": "X"})).is_err());
        assert!(
            s.run("view.manageViews", &json!({"op": "rename", "name": "Mine", "newName": "fortnight"}))
                .is_err()
        );
        assert!(s.run("view.manageViews", &json!({"op": "delete", "name": "Icon"})).is_err());
        assert!(s.run("view.manageViews", &json!({"op": "new", "name": "Notes List"})).is_err());
        assert!(s.run("view.manageViews", &json!({"op": "new", "name": "Z", "base": "Bogus"})).is_err());
        assert!(
            s.run("view.manageViews", &json!({"op": "copy", "name": "Mine", "newName": "Y", "scope": "x"}))
                .is_err()
        );
        assert!(s.run("view.manageViews", &json!({"op": "dance"})).is_err());
        let r = run(&mut s, "view.manageViews", json!({"op": "delete", "name": "Fortnight"}));
        assert_eq!(r["deleted"], "Fortnight");
        assert_eq!(current_name(&state(&s, folder, ModuleKind::Notes)), "Last 7 Days");
        assert_eq!(state(&s, folder, ModuleKind::Notes).filter.words, "x");
        run(&mut s, "view.manageViews", json!({"op": "reset", "name": "Last 7 Days"}));
        let st = state(&s, folder, ModuleKind::Notes);
        assert_eq!(st.view, "Last 7 Days");
        assert!(st.filter.words.is_empty(), "Reset takes the shown view back to its settings");
        assert_eq!(saved_views(&s, folder).len(), 2, "Reset in Manage Views keeps the saved views");
        // Views for every Notes folder show in the other notes folders too; this folder's own do not.
        let other = s.mailbox.create_folder(folder, "Ideas").unwrap();
        s.selection.folder = other;
        let there: Vec<String> = saved_views(&s, other).into_iter().map(|v| v.name).collect();
        assert_eq!(there, vec!["Week 2".to_owned()]);
        let r = run(&mut s, "view.manageViews", json!({}));
        assert_eq!(r["dialog"], "manageViews");
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        let rows = p["saved"].as_array().unwrap();
        assert_eq!(rows.last().unwrap()["usedOn"], "All Note folders");
        // Save View As for every folder.
        run(&mut s, "view.saveViewAs", json!({"name": "Shared", "scope": "all", "confirm": true}));
        s.selection.folder = folder;
        assert!(saved_views(&s, folder).iter().any(|v| v.name == "Shared" && v.everywhere));
    }

    /// Regression: a change in a folder whose own view hid a shared view of the same name rewrote the shared
    /// list without it, and saving an own view over a shared name removed it from every folder.
    #[test]
    fn own_and_shared_views_with_one_name_both_survive() {
        let mut s = session();
        let b = select_folder(&mut s, FolderRole::Notes);
        let c = s.mailbox.create_folder(b, "Ideas").unwrap();
        let d = s.mailbox.create_folder(b, "Recipes").unwrap();
        let scopes = |s: &Session, f: FolderId| -> Vec<(String, bool)> {
            saved_views(s, f).into_iter().map(|v| (v.name, v.everywhere)).collect()
        };
        // B keeps its own "X"; C saves "X" for every Notes folder (C cannot see B's).
        run(&mut s, "view.saveViewAs", json!({"name": "X", "confirm": true}));
        s.selection.folder = c;
        run(&mut s, "view.saveViewAs", json!({"name": "X", "scope": "all", "confirm": true}));
        assert_eq!(scopes(&s, d), vec![("X".to_owned(), true)]);
        // Every kind of change in B leaves the shared "X" alone.
        s.selection.folder = b;
        assert_eq!(scopes(&s, b), vec![("X".to_owned(), false)], "B's own wins in B");
        run(&mut s, "view.saveViewAs", json!({"name": "Y", "confirm": true}));
        run(&mut s, "view.manageViews", json!({"op": "new", "name": "N"}));
        run(&mut s, "view.manageViews", json!({"op": "copy", "name": "N", "newName": "N2"}));
        run(&mut s, "view.manageViews", json!({"op": "rename", "name": "N2", "newName": "N3"}));
        run(&mut s, "view.manageViews", json!({"op": "update", "name": "X"}));
        run(&mut s, "view.manageViews", json!({"op": "delete", "name": "N"}));
        assert_eq!(scopes(&s, d), vec![("X".to_owned(), true)], "D still has the shared X");
        // Deleting B's own "X" shows the shared one again; it is not deleted with it.
        run(&mut s, "view.manageViews", json!({"op": "delete", "name": "X"}));
        assert!(scopes(&s, b).contains(&("X".to_owned(), true)));
        assert_eq!(scopes(&s, d), vec![("X".to_owned(), true)]);
        // A name kept for every folder is not also saved for this folder alone (one would hide the other
        // here): the request is refused and the shared view stays for every folder.
        run(&mut s, "view.manageViews", json!({"op": "new", "name": "Week 2", "scope": "all"}));
        let own = json!({"name": "Week 2", "scope": "folder", "confirm": true});
        assert!(s.run("view.saveViewAs", &own).is_err());
        assert!(scopes(&s, b).contains(&("Week 2".to_owned(), true)));
        assert!(scopes(&s, d).contains(&("Week 2".to_owned(), true)));
        // Saving it again for every folder replaces it there.
        run(&mut s, "view.saveViewAs", json!({"name": "Week 2", "scope": "all", "confirm": true}));
        assert_eq!(scopes(&s, b).iter().filter(|(n, _)| n == "Week 2").count(), 1);
        assert!(scopes(&s, d).contains(&("Week 2".to_owned(), true)));
    }

    #[test]
    fn hostile_saved_module_views_are_cleaned() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Notes);
        s.view.collapsed_groups.insert(
            folder,
            vec![format!(
                "\u{1}mod.saved={}",
                json!([
                    {"name": "Icon", "state": {}},
                    {"name": "\u{7}", "state": {}},
                    {"name": "Ok", "state": {"view": "Nope", "sort": "zzz", "columns": ["x", "subject"], "rules": [{"name": "r", "color": "red"}]}},
                    {"name": "ok", "state": {}},
                ])
            )],
        );
        let v = saved_views(&s, folder);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].state.view, "Icon");
        assert_eq!(v[0].state.sort, "");
        assert_eq!(v[0].state.columns, vec!["subject".to_owned()]);
        assert_eq!(v[0].state.rules[0].color, "");
    }

    #[test]
    fn date_groups() {
        let today = 20_735; // Friday 2026-10-09
        assert_eq!(date_group(None, today, "Overdue"), (6, "No Date"));
        assert_eq!(date_group(Some(today - 1), today, "Overdue"), (0, "Overdue"));
        assert_eq!(date_group(Some(today), today, "Overdue").1, "Today");
        assert_eq!(date_group(Some(today + 1), today, "Overdue").1, "Tomorrow");
        assert_eq!(date_group(Some(today + 3), today, "Overdue").1, "Next Week");
        assert_eq!(date_group(Some(today + 30), today, "Earlier").1, "Later");
        assert_eq!(date_group(Some(i64::MIN), i64::MAX, "Earlier").1, "Earlier");
    }
}
