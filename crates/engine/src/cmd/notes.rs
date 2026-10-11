//! Notes commands (`notes.*`).
//!
//! Notes live in notes folders (`Mailbox.notes`); the first line of a note is its title. `notes.new` creates
//! the note straight away (like a paper sticky note) and asks the UI to open it for editing.
//!
//! **View state.** The notes folder's module view (`cmd::view::modules`) keeps the Icon view arrangement
//! (Large Icons, Small Icons, List) and whether the icons are lined up: `notes.iconView` and
//! `notes.lineUpIcons` update it (switching the folder to the Icon view) and return it as
//! `{"view": NotesViewState}`. A `view` param overrides the stored state as the starting point.
//! `notes.lineUpIcons` also returns the grid order of the notes in the current folder.
//!
//! **Icon positions.** Icons dragged off the grid keep their place: `notes.setPosition` stores the note's
//! offset from the grid's top-left corner (points) with the folder's view state, which the profile saves,
//! and [`positions`] reads them back. Line Up Icons drops them (the icons snap back to the grid), and so
//! does Reset View.
//!
//! `notes.forward` sends the note's text in a new message. [`view_rows`] is the list of the Notes views
//! (Icon, Notes List, Last 7 Days).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::calendar::{pim, pim::Kind};
use crate::cmd::util::opt_str;
use crate::cmd::view::modules::{self, ModuleKind, RowKind, ViewRow};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::{FolderId, ItemId, Note, NoteColor};

/// Icon view arrangement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IconMode {
    #[default]
    Large,
    Small,
    List,
}

/// The Notes Icon view state the UI holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotesViewState {
    pub icon_mode: IconMode,
    /// Icons snapped to the grid (set by Line Up Icons; cleared when the UI lets the user drag icons).
    pub lined_up: bool,
}

/// The starting view state: the `view` param, else the current notes folder's stored state.
fn view_param(s: &Session, p: &Value) -> Result<NotesViewState, CmdError> {
    match p.get("view") {
        None | Some(Value::Null) => Ok(stored_view(s)),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("`view`: {e}"))),
    }
}

/// Icon arrangement stored for the current notes folder.
pub fn stored_view(s: &Session) -> NotesViewState {
    pim::current_folder(s, Kind::Note)
        .map(|f| {
            let st = modules::state(s, f, ModuleKind::Notes);
            NotesViewState { icon_mode: st.icon_mode, lined_up: st.lined_up }
        })
        .unwrap_or_default()
}

/// Stores `v` for the current notes folder (which switches to the Icon view).
fn store_view(s: &mut Session, v: &NotesViewState) {
    if let Some(f) = pim::current_folder(s, Kind::Note) {
        modules::set_icons(s, f, v.icon_mode, v.lined_up);
    }
}

/// Reserved entry of the folder's view state holding the icon positions (JSON after the prefix). The
/// control character keeps it apart from group labels (`cmd::view::is_group_label`).
const NOTE_POS: &str = "\u{1}notes.pos=";
/// Most icon positions kept per folder.
pub const MAX_POSITIONS: usize = 2_000;
/// Largest offset accepted (points).
pub const MAX_OFFSET: f32 = 100_000.0;

fn valid_offset(v: f32) -> bool {
    v.is_finite() && (0.0..=MAX_OFFSET).contains(&v)
}

/// Icon positions of the notes of `folder` that were dragged off the grid: note id → (x, y) offset from the
/// grid's top-left corner. Positions of notes no longer in the folder are left out.
pub fn positions(s: &Session, folder: FolderId) -> BTreeMap<u64, (f32, f32)> {
    s.view
        .collapsed_groups
        .get(&folder)
        .and_then(|list| list.iter().find_map(|e| e.strip_prefix(NOTE_POS)))
        .and_then(|json| serde_json::from_str::<BTreeMap<u64, (f32, f32)>>(json).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, (x, y))| {
            valid_offset(*x)
                && valid_offset(*y)
                && s.mailbox.notes.get(&ItemId(*id)).is_some_and(|n| n.folder == folder)
        })
        .take(MAX_POSITIONS)
        .collect()
}

fn store_positions(s: &mut Session, folder: FolderId, map: &BTreeMap<u64, (f32, f32)>) {
    let list = s.view.collapsed_groups.entry(folder).or_default();
    list.retain(|e| !e.starts_with(NOTE_POS));
    if !map.is_empty()
        && let Ok(json) = serde_json::to_string(map)
    {
        list.push(format!("{NOTE_POS}{json}"));
    }
    if list.is_empty() {
        s.view.collapsed_groups.remove(&folder);
    }
}

fn offset_param(p: &Value, key: &str) -> Result<f32, CmdError> {
    p.get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .filter(|v| valid_offset(*v))
        .ok_or_else(|| CmdError::Params(format!("`{key}` must be a number from 0 to {MAX_OFFSET}")))
}

/// `notes.setPosition {id, x, y}` keeps a dragged icon where it was dropped; `{id, position: null}` puts it
/// back on the grid. Dragging means the icons are no longer lined up.
fn run_set_position(s: &mut Session, p: &Value) -> CmdResult {
    let id = pim::require_targets(s, p, Kind::Note)?
        .first()
        .copied()
        .ok_or_else(|| CmdError::Params("no note given".into()))?;
    let folder =
        s.mailbox.notes.get(&id).map(|n| n.folder).ok_or_else(|| CmdError::Params("no note".into()))?;
    let mut map = positions(s, folder);
    if p.get("position").is_some_and(Value::is_null) {
        map.remove(&id.0);
    } else {
        let (x, y) = (offset_param(p, "x")?, offset_param(p, "y")?);
        if !map.contains_key(&id.0) && map.len() >= MAX_POSITIONS {
            return Err(CmdError::Params(format!("at most {MAX_POSITIONS} icons keep a position")));
        }
        map.insert(id.0, (x, y));
        let st = modules::state(s, folder, ModuleKind::Notes);
        if st.lined_up {
            modules::set_icons(s, folder, st.icon_mode, false);
        }
    }
    store_positions(s, folder, &map);
    Ok(json!({ "id": id.0, "folder": folder.0, "positions": map.len() }))
}

fn parse_color(c: &str) -> Option<NoteColor> {
    match c {
        "blue" => Some(NoteColor::Blue),
        "green" => Some(NoteColor::Green),
        "pink" => Some(NoteColor::Pink),
        "yellow" => Some(NoteColor::Yellow),
        "white" => Some(NoteColor::White),
        _ => None,
    }
}

fn color_name(c: NoteColor) -> &'static str {
    match c {
        NoteColor::Blue => "blue",
        NoteColor::Green => "green",
        NoteColor::Pink => "pink",
        NoteColor::Yellow => "yellow",
        NoteColor::White => "white",
    }
}

/// Title of a note: its first non-empty line.
pub fn title(n: &Note) -> String {
    n.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_owned()
}

pub fn note_json(n: &Note, tz: i32) -> Value {
    json!({
        "id": n.id.0,
        "folder": n.folder.0,
        "title": title(n),
        "text": n.text,
        "color": color_name(n.color),
        "created": pim::datetime_string(n.created, tz),
        "modified": pim::datetime_string(n.modified, tz),
        "categories": n.categories,
    })
}

fn apply_note_fields(n: &mut Note, p: &Value) -> Result<bool, CmdError> {
    let mut changed = false;
    if let Some(t) = pim::text(p, "text")? {
        n.text = t;
        changed = true;
    }
    if let Some(c) = p.get("color").filter(|v| !v.is_null()) {
        n.color = c
            .as_str()
            .and_then(parse_color)
            .ok_or_else(|| CmdError::Params("`color` must be blue|green|pink|yellow|white".into()))?;
        changed = true;
    }
    if let Some(v) = pim::list(p, "categories")? {
        n.categories = v;
        changed = true;
    }
    Ok(changed)
}

fn run_new(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Note)?;
    let now = s.now();
    let mut n = Note { folder, created: now, modified: now, ..Default::default() };
    apply_note_fields(&mut n, p)?;
    let id = ItemId(s.mailbox.alloc_id());
    n.id = id;
    let mut out = note_json(&n, s.tz_offset_min);
    s.mailbox.notes.insert(id, n);
    pim::dialog(s, "note", json!({ "id": id.0 }))?;
    if let Some(o) = out.as_object_mut() {
        o.insert("dialog".into(), json!("note"));
    }
    Ok(out)
}

fn run_update(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Note)?;
    let now = s.now();
    let tz = s.tz_offset_min;
    let mut updated = Vec::new();
    for id in &ids {
        let Some(mut n) = s.mailbox.notes.get(id).cloned() else { continue };
        if apply_note_fields(&mut n, p)? {
            n.modified = now;
        }
        updated.push(n);
    }
    let out: Vec<Value> = updated.iter().map(|n| note_json(n, tz)).collect();
    for n in updated {
        s.mailbox.notes.insert(n.id, n);
    }
    Ok(json!({ "updated": out }))
}

fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_delete(s, p, Kind::Note)
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("folder").is_none_or(Value::is_null) {
        return crate::cmd::tasks::move_dialog(s, p, Kind::Note, true);
    }
    pim::run_copy(s, p, Kind::Note)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("folder").is_none_or(Value::is_null) {
        return crate::cmd::tasks::move_dialog(s, p, Kind::Note, false);
    }
    pim::run_move(s, p, Kind::Note)
}
fn run_all_categories(s: &mut Session, p: &Value) -> CmdResult {
    crate::cmd::tasks::categories_dialog(s, p, Kind::Note)
}

/// Forward: a new message with the note's title as the subject and its text as the body (one message for
/// all the selected notes).
fn run_forward(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Note)?;
    let notes: Vec<Note> = ids.iter().filter_map(|i| s.mailbox.notes.get(i)).cloned().collect();
    let subject = match notes.as_slice() {
        [one] => title(one),
        [first, rest @ ..] => format!("{} and {} more", title(first), rest.len()),
        [] => String::new(),
    };
    let body = notes.iter().map(|n| n.text.trim_end().to_owned()).collect::<Vec<_>>().join("\n\n");
    let mut compose = crate::compose::ComposeState {
        subject: mailcraft_model::subject::forward_subject(&subject),
        body: mailcraft_model::Doc::from_plain_text(&body),
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut compose);
    let cid = s.open_compose(compose);
    Ok(json!({ "compose": cid, "notes": notes.len() }))
}

fn note_selected(s: &Session) -> Option<&'static str> {
    if pim::selected(s, Kind::Note).is_empty() { Some("select a note first") } else { None }
}

/// Rows of the selected Notes view of `folder`: Icon in grid order (oldest first), Notes List newest
/// first, Last 7 Days the notes changed in the last seven days, newest first. Reverse Sort reverses.
pub fn view_rows(s: &Session, folder: FolderId) -> Vec<ViewRow> {
    let st = modules::state(s, folder, ModuleKind::Notes);
    let since = s.now().unix.saturating_sub(7 * pim::DAY_SECS);
    let mut v: Vec<(i64, ItemId)> = s
        .mailbox
        .notes
        .values()
        .filter(|n| n.folder == folder)
        .filter(|n| st.view != "Last 7 Days" || n.modified.unix >= since)
        .map(|n| (n.created.unix, n.id))
        .collect();
    v.sort();
    if st.view != "Icon" {
        v.reverse();
    }
    let mut out: Vec<ViewRow> =
        v.into_iter().map(|(_, id)| ViewRow { id, kind: RowKind::Note, group: None }).collect();
    if modules::reversed(s, folder) {
        out.reverse();
    }
    out
}
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_categorize(s, p, Kind::Note)
}

fn run_icon_view(s: &mut Session, p: &Value) -> CmdResult {
    let mode = match opt_str(p, "mode") {
        Some("large") => IconMode::Large,
        Some("small") => IconMode::Small,
        Some("list") => IconMode::List,
        _ => return Err(CmdError::Params("`mode` must be large|small|list".into())),
    };
    let mut v = view_param(s, p)?;
    v.icon_mode = mode;
    store_view(s, &v);
    Ok(json!({ "view": v }))
}

/// Notes of the current notes folder in grid order (oldest first, then id).
pub fn grid_order(s: &Session) -> Vec<ItemId> {
    let Some(folder) = pim::current_folder(s, Kind::Note) else {
        return Vec::new();
    };
    let mut v: Vec<(i64, ItemId)> =
        s.mailbox.notes.values().filter(|n| n.folder == folder).map(|n| (n.created.unix, n.id)).collect();
    v.sort();
    v.into_iter().map(|(_, id)| id).collect()
}

fn notes_present(s: &Session) -> Option<&'static str> {
    if grid_order(s).is_empty() { Some("no notes to line up") } else { None }
}

fn run_line_up(s: &mut Session, p: &Value) -> CmdResult {
    let mut v = view_param(s, p)?;
    if v.icon_mode == IconMode::List {
        return Err(CmdError::Disabled("not available in the list arrangement".into()));
    }
    v.lined_up = true;
    store_view(s, &v);
    if let Some(f) = pim::current_folder(s, Kind::Note) {
        store_positions(s, f, &BTreeMap::new());
    }
    Ok(json!({ "view": v, "order": pim::ids_json(&grid_order(s)) }))
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, location, params, mutates, enabled, run| CommandSpec {
        id,
        label,
        location,
        shortcut: "",
        params,
        mutates,
        enabled,
        run,
    };
    vec![
        spec(
            "notes.iconView",
            "Icon Arrangement",
            "Notes › View › Arrangement",
            r#"{"mode": "large|small|list", "view"?: NotesViewState}"#,
            true,
            always,
            run_icon_view,
        ),
        spec(
            "notes.lineUpIcons",
            "Line Up Icons",
            "Notes › View › Arrangement",
            r#"{"view"?: NotesViewState}"#,
            true,
            notes_present,
            run_line_up,
        ),
        spec(
            "notes.setPosition",
            "Move Icon",
            "Notes › Icon view",
            r#"{"id": number, "x": number, "y": number} | {"id": number, "position": null} (back to the grid)"#,
            true,
            always,
            run_set_position,
        ),
        spec(
            "notes.new",
            "New Note",
            "Notes › Home › New",
            r#"{"text"?: string, "color"?: "blue|green|pink|yellow|white", "categories"?: [string], "folder"?: number}"#,
            true,
            always,
            run_new,
        ),
        spec(
            "notes.update",
            "Edit Note",
            "Notes › Item",
            r#"{"id": number | "ids": [number], "text"?, "color"?, "categories"?}"#,
            true,
            always,
            run_update,
        ),
        spec(
            "notes.delete",
            "Delete",
            "Notes › Home › Delete",
            r#"{"id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_delete,
        ),
        spec(
            "notes.copy",
            "Copy to Folder",
            "Notes › Item",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_copy,
        ),
        spec(
            "notes.move",
            "Move to Folder",
            "Notes › Home › Actions › Move",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_move,
        ),
        spec(
            "notes.categorize",
            "Categorize",
            "Notes › Home › Tags",
            r#"{"name": string, "on"?: bool, "clear"?: bool, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_categorize,
        ),
        spec(
            "notes.allCategories",
            "All Categories",
            "Notes › Home › Tags › Categorize",
            r#"{"ids"?: [number]}"#,
            false,
            note_selected,
            run_all_categories,
        ),
        spec(
            "notes.forward",
            "Forward",
            "Notes › Home › Actions",
            r#"{"ids"?: [number]}"#,
            false,
            note_selected,
            run_forward,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use crate::{Notice, Selection};
    use mailcraft_model::FolderRole;

    #[test]
    fn new_note_creates_and_opens() {
        let mut s = session();
        let r = s.run("notes.new", &json!({"text": "Groceries\nmilk", "color": "blue"})).unwrap();
        let id = ItemId(r["id"].as_u64().unwrap());
        let n = &s.mailbox.notes[&id];
        assert_eq!(n.folder, role(&s, FolderRole::Notes));
        assert_eq!(n.color, NoteColor::Blue);
        assert_eq!(r["title"], "Groceries");
        assert_eq!(r["dialog"], "note");
        assert!(
            matches!(s.notices.last(), Some(Notice::OpenDialog(name, p)) if name == "note" && p["id"] == id.0)
        );
        let r = s.run("notes.new", &json!({})).unwrap();
        assert_eq!(s.mailbox.notes[&ItemId(r["id"].as_u64().unwrap())].color, NoteColor::Yellow);
        assert!(matches!(s.run("notes.new", &json!({"color": "red"})), Err(CmdError::Params(_))));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn update_sets_modified() {
        let mut s = session();
        let id = s.run("notes.new", &json!({"text": "a"})).unwrap()["id"].as_u64().unwrap();
        s.fixed_now = s.fixed_now.map(|t| pim::add_secs(t, 3600));
        let r = s.run("notes.update", &json!({"id": id, "text": "b"})).unwrap();
        assert_eq!(r["updated"][0]["modified"], "2026-10-09T11:07");
        assert_eq!(r["updated"][0]["created"], "2026-10-09T10:07");
    }

    #[test]
    fn icon_view_and_line_up() {
        let mut s = session();
        let r = s.run("notes.iconView", &json!({"mode": "small"})).unwrap();
        assert_eq!(r["view"]["iconMode"], "small");
        assert!(matches!(s.run("notes.iconView", &json!({"mode": "huge"})), Err(CmdError::Params(_))));
        s.selection = Selection { folder: role(&s, FolderRole::Notes), items: vec![], focus: None };
        assert!(matches!(s.run("notes.lineUpIcons", &json!({})), Err(CmdError::Disabled(_))));
        let a = s.run("notes.new", &json!({"text": "a"})).unwrap()["id"].as_u64().unwrap();
        let r = s.run("notes.lineUpIcons", &json!({"view": {"iconMode": "large"}})).unwrap();
        assert_eq!(r["order"], json!([a]));
        assert_eq!(r["view"]["linedUp"], true);
        assert!(matches!(
            s.run("notes.lineUpIcons", &json!({"view": {"iconMode": "list"}})),
            Err(CmdError::Disabled(_))
        ));
    }

    #[test]
    fn forward_note_and_dialogs() {
        let mut s = session();
        let id =
            s.run("notes.new", &json!({"text": "Groceries\nmilk\neggs"})).unwrap()["id"].as_u64().unwrap();
        s.selection = Selection {
            folder: role(&s, FolderRole::Notes),
            items: vec![ItemId(id)],
            focus: Some(ItemId(id)),
        };
        let r = s.run("notes.forward", &json!({})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "FW: Groceries");
        assert!(c.body.to_plain_text().contains("eggs"));
        assert_eq!(s.run("notes.move", &json!({})).unwrap()["dialog"], "pimMove");
        assert_eq!(s.run("notes.allCategories", &json!({})).unwrap()["dialog"], "pimCategories");
        s.selection.items.clear();
        s.selection.focus = None;
        assert!(matches!(s.run("notes.forward", &json!({})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn delete_copy_move_categorize_notes() {
        let mut s = session();
        let id = s.run("notes.new", &json!({"text": "a"})).unwrap()["id"].as_u64().unwrap();
        let root = role(&s, FolderRole::Notes);
        let sub = s.mailbox.create_folder(root, "Ideas").unwrap();
        s.run("notes.copy", &json!({"id": id, "folder": sub.0})).unwrap();
        s.run("notes.move", &json!({"id": id, "folder": sub.0})).unwrap();
        assert_eq!(s.mailbox.notes[&ItemId(id)].folder, sub);
        s.run("notes.categorize", &json!({"id": id, "name": "Yellow category"})).unwrap();
        s.run("notes.categorize", &json!({"id": id, "clear": true})).unwrap();
        assert!(s.mailbox.notes[&ItemId(id)].categories.is_empty());
        s.run("notes.delete", &json!({"id": id})).unwrap();
        assert_eq!(s.mailbox.notes[&ItemId(id)].folder, role(&s, FolderRole::DeletedItems));
        assert!(matches!(s.run("notes.delete", &json!({"id": 424_242})), Err(CmdError::Params(_))));
    }

    #[test]
    fn dragged_icon_positions_are_kept() {
        let mut s = session();
        let folder = role(&s, FolderRole::Notes);
        s.selection = Selection { folder, items: vec![], focus: None };
        let a = s.run("notes.new", &json!({"text": "A"})).unwrap()["id"].as_u64().unwrap();
        let b = s.run("notes.new", &json!({"text": "B"})).unwrap()["id"].as_u64().unwrap();
        s.run("notes.lineUpIcons", &json!({})).unwrap();
        assert!(stored_view(&s).lined_up);
        s.run("notes.setPosition", &json!({"id": a, "x": 420.5, "y": 30})).unwrap();
        s.run("notes.setPosition", &json!({"id": b, "x": 0, "y": 900})).unwrap();
        assert_eq!(positions(&s, folder).get(&a), Some(&(420.5, 30.0)));
        assert!(!stored_view(&s).lined_up, "dragging breaks the lined-up grid");
        // The positions are not group labels and survive a settings round trip of the view state.
        assert!(s.view.collapsed_groups[&folder].iter().all(|e| !crate::cmd::view::is_group_label(e)));
        let view: crate::ViewState = serde_json::from_value(serde_json::to_value(&s.view).unwrap()).unwrap();
        s.view = view;
        assert_eq!(positions(&s, folder).len(), 2);
        // Back to the grid one at a time, or all with Line Up Icons.
        s.run("notes.setPosition", &json!({"id": b, "position": null})).unwrap();
        assert_eq!(positions(&s, folder).keys().copied().collect::<Vec<_>>(), vec![a]);
        s.run("notes.lineUpIcons", &json!({})).unwrap();
        assert!(positions(&s, folder).is_empty());
        // Hostile values.
        for bad in [
            json!({"id": a, "x": -1, "y": 0}),
            json!({"id": a, "x": 1e30, "y": 0}),
            json!({"id": a, "x": "1", "y": 0}),
            json!({"id": a}),
            json!({"id": u64::MAX, "x": 1, "y": 1}),
        ] {
            assert!(s.run("notes.setPosition", &bad).is_err(), "{bad}");
        }
        // A deleted note's position is dropped.
        s.run("notes.setPosition", &json!({"id": a, "x": 5, "y": 5})).unwrap();
        s.run("notes.delete", &json!({"id": a})).unwrap();
        assert!(positions(&s, folder).is_empty());
        // Damaged stored text reads as no positions.
        s.view.collapsed_groups.insert(folder, vec![format!("{NOTE_POS}{{not json")]);
        assert!(positions(&s, folder).is_empty());
    }
}
