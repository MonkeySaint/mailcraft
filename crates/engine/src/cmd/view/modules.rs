//! Views of the People, Tasks and Notes modules (`plan/outlook/modules.md` › Current View, Arrangement,
//! Layout): the `view.*` commands act on these when the selected folder is a contacts, tasks or notes folder.
//!
//! **Views.** People: People, Business Card, Card, Phone, List. Tasks: Detailed, Simple List, To-Do List,
//! Prioritized, Active, Completed, Today, Next 7 Days, Overdue, Assigned, Server Tasks. Notes: Icon, Notes
//! List, Last 7 Days. Each folder remembers its view, its arrangement (People: Categories / Company /
//! Location in the Phone and List views; Tasks: Categories / Start Date / Due Date / Folder / Type /
//! Importance), the Tasks message preview, the reading pane position and the Notes icon arrangement.
//! Reverse Sort, Show in Groups and Use Tighter Spacing are the folder's [`ViewSettings`] fields as in Mail.
//!
//! **Storage.** Like the conversation state, the module view state lives in `ViewState::collapsed_groups`
//! (persisted with the profile) as reserved entries starting with a control character: one JSON
//! [`ModuleView`] and the folder's saved custom views. Group code skips them ([`super::is_group_label`]).
//!
//! **Rows.** [`rows`] is the list a module view shows (filtered, sorted and grouped); the UI draws it and the
//! Expand/Collapse commands read group labels from it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{dialog, settings};
use crate::cmd::notes::IconMode;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::dates::weekday_of_day;
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
        }
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

/// A custom view saved with Save Current View As a New View.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedView {
    pub name: String,
    pub state: ModuleView,
    /// Reverse Sort and Show in Groups at the time it was saved.
    pub reverse: bool,
    pub groups: bool,
}

impl Default for SavedView {
    fn default() -> Self {
        SavedView { name: String::new(), state: ModuleView::default(), reverse: false, groups: true }
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
    pub group: Option<String>,
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
        Some(mut v) => {
            // Stored text is not trusted: unknown views fall back to the default, numbers are clamped.
            v.view = canonical_view(kind, &v.view).unwrap_or(kind.default_view()).to_owned();
            v.arrange = v.arrange.as_deref().and_then(|a| canonical_arrange(kind, a)).map(str::to_owned);
            v.preview_lines = v.preview_lines.min(3);
            v
        }
        None => ModuleView::defaults(kind),
    }
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

/// Custom views saved for `folder`.
pub fn saved_views(s: &Session, folder: FolderId) -> Vec<SavedView> {
    s.view
        .collapsed_groups
        .get(&folder)
        .and_then(|list| list.iter().find_map(|e| e.strip_prefix(MOD_SAVED)))
        .and_then(|json| serde_json::from_str::<Vec<SavedView>>(json).ok())
        .unwrap_or_default()
        .into_iter()
        .take(MAX_SAVED_VIEWS)
        .collect()
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
        ModuleKind::Notes => false,
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

/// Every group label of the folder's view, in order.
pub fn groups(s: &Session, folder: FolderId) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in rows(s, folder).into_iter().filter_map(|r| r.group) {
        if !out.contains(&g) {
            out.push(g);
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
    })
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
        store(s, folder, kind, &st);
        clear_groups(s, folder);
        return Ok(json!({ "view": v, "module": kind.name() }));
    }
    let wanted = name.trim();
    let Some(saved) = saved_views(s, folder).into_iter().find(|v| v.name.eq_ignore_ascii_case(wanted)) else {
        return Err(CmdError::Params(format!("no {} view named \"{wanted}\"", kind.name())));
    };
    let mut st = saved.state.clone();
    st.view = canonical_view(kind, &st.view).unwrap_or(kind.default_view()).to_owned();
    store(s, folder, kind, &st);
    set_settings(s, folder, |v| {
        v.order = if saved.reverse { SortOrder::Ascending } else { SortOrder::Descending };
        v.show_in_groups = saved.groups;
    });
    clear_groups(s, folder);
    Ok(json!({ "view": st.view, "saved": saved.name, "module": kind.name() }))
}

/// `view.arrangeBy` in a module folder.
pub(crate) fn arrange_by(s: &mut Session, folder: FolderId, kind: ModuleKind, key: &str) -> CmdResult {
    let mut st = state(s, folder, kind);
    if !arrange_allowed(kind, &st.view) {
        return Err(CmdError::Disabled(format!("Arrange By is not available in the {} view", st.view)));
    }
    let a = canonical_arrange(kind, key)
        .ok_or_else(|| CmdError::Params(format!("unknown {} arrangement `{key}`", kind.name())))?;
    st.arrange = Some(a.to_owned());
    store(s, folder, kind, &st);
    clear_groups(s, folder);
    Ok(json!({ "arrangeBy": a, "label": arrange_label(kind, a), "module": kind.name() }))
}

/// `view.messagePreview` in a tasks folder (the task notes under each row).
pub(crate) fn message_preview(s: &mut Session, folder: FolderId, kind: ModuleKind, lines: u8) -> CmdResult {
    if kind != ModuleKind::Tasks {
        return Err(CmdError::Disabled("not available in this view".into()));
    }
    let mut st = state(s, folder, kind);
    st.preview_lines = lines.min(3);
    store(s, folder, kind, &st);
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

/// `view.manageViews` in a module folder: the module's views plus the folder's saved ones.
pub(crate) fn manage_views(s: &mut Session, folder: FolderId, kind: ModuleKind) -> CmdResult {
    let st = state(s, folder, kind);
    let mut views: Vec<String> = kind.views().iter().map(|v| (*v).to_owned()).collect();
    views.extend(saved_views(s, folder).into_iter().map(|v| v.name));
    Ok(dialog(
        s,
        "manageViews",
        json!({ "views": views, "current": st.view, "module": kind.name(), "folder": folder.0 }),
    ))
}

/// `view.saveViewAs` in a module folder: without `confirm` the Save View dialog opens; with
/// `{"name", "confirm": true}` the current view is saved under that name (replacing a saved view of the
/// same name).
pub(crate) fn save_view_as(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let name = p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    if canonical_view(kind, &name).is_some() {
        return Err(CmdError::Params(format!("a view named \"{name}\" already exists")));
    }
    if p.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(dialog(
            s,
            "saveModuleView",
            json!({ "folder": folder.0, "module": kind.name(), "name": name, "noun": kind.item_noun() }),
        ));
    }
    if name.is_empty() {
        return Err(CmdError::Params("the view needs a name".into()));
    }
    if name.chars().count() > MAX_VIEW_NAME || name.chars().any(char::is_control) {
        return Err(CmdError::Params(format!("view names are at most {MAX_VIEW_NAME} printable characters")));
    }
    let v = settings(s, folder);
    let entry = SavedView {
        name: name.clone(),
        state: state(s, folder, kind),
        reverse: v.order == SortOrder::Ascending,
        groups: v.show_in_groups,
    };
    let mut list = saved_views(s, folder);
    list.retain(|x| !x.name.eq_ignore_ascii_case(&name));
    if list.len() >= MAX_SAVED_VIEWS {
        return Err(CmdError::Failed(format!("a folder keeps at most {MAX_SAVED_VIEWS} saved views")));
    }
    list.push(entry);
    store_saved(s, folder, &list);
    Ok(json!({ "saved": name, "module": kind.name() }))
}

/// `view.viewSettings` in a module folder: the module's View Settings dialog.
pub(crate) fn view_settings(s: &mut Session, folder: FolderId, kind: ModuleKind) -> CmdResult {
    let st = state(s, folder, kind);
    let params = view_json(s, folder, kind, &st);
    Ok(dialog(s, "moduleViewSettings", params))
}

fn set_settings(s: &mut Session, folder: FolderId, f: impl FnOnce(&mut ViewSettings)) {
    f(s.mailbox.views.entry(folder).or_default());
    if s.mailbox.views.get(&folder).is_some_and(|v| *v == ViewSettings::default()) {
        s.mailbox.views.remove(&folder);
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
        assert!(matches!(s.run("view.resetView", &json!({})), Err(CmdError::Disabled(_))));
        run(&mut s, "view.manageViews", json!({}));
        let Some(Notice::OpenDialog(name, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(name, "manageViews");
        assert_eq!(p["views"].as_array().unwrap().len(), 11);
        assert_eq!(p["current"], "To-Do List");
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert!(s.enabled("view.resetView").is_none());
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(state(&s, tasks, ModuleKind::Tasks), ModuleView::defaults(ModuleKind::Tasks));
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
        // Save Current View As a New View, then apply it.
        assert_eq!(run(&mut s, "view.saveViewAs", json!({"name": "Mine"}))["dialog"], "saveModuleView");
        run(&mut s, "view.reverseSort", json!({}));
        assert_eq!(run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}))["saved"], "Mine");
        assert!(s.run("view.saveViewAs", &json!({"name": "Detailed", "confirm": true})).is_err());
        assert!(s.run("view.saveViewAs", &json!({"name": " ", "confirm": true})).is_err());
        run(&mut s, "view.resetView", json!({}));
        assert!(saved_views(&s, tasks).is_empty(), "Reset View drops the folder's saved views too");
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert_eq!(run(&mut s, "view.changeView", json!({"name": "mine"}))["view"], "To-Do List");
        run(&mut s, "view.manageViews", json!({}));
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["views"].as_array().unwrap().last().unwrap(), "Mine");
        assert_eq!(run(&mut s, "view.viewSettings", json!({}))["dialog"], "moduleViewSettings");
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
