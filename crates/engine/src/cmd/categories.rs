//! `categories.*`: the master category list (Color Categories dialog: new, rename, delete, colour, shortcut
//! key) and the Quick Click category.

use serde_json::{Value, json};

use crate::cmd::rules::{clean_name, dialog};
use crate::cmd::util::{opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::{Category, CategoryColor};

/// Most categories kept in the master list.
const MAX_CATEGORIES: usize = 1_000;

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    const C: &str = "Home › Tags › Categorize › All Categories";
    vec![
        spec(
            "categories.new",
            "New",
            C,
            r#"{"name": string, "color"?: string, "shortcut"?: "Ctrl+F2"…"Ctrl+F12"}"#,
            true,
            always,
            new_category,
        ),
        spec(
            "categories.rename",
            "Rename",
            C,
            r#"{"name": string, "newName": string}"#,
            true,
            has_categories,
            rename,
        ),
        spec("categories.delete", "Delete", C, r#"{"name": string}"#, true, has_categories, delete),
        spec(
            "categories.color",
            "Color",
            C,
            r#"{"name": string, "color": string}"#,
            true,
            has_categories,
            color,
        ),
        spec(
            "categories.shortcut",
            "Shortcut Key",
            C,
            r#"{"name": string, "shortcut": "Ctrl+F2"…"Ctrl+F12"|""}"#,
            true,
            has_categories,
            shortcut,
        ),
        spec("categories.list", "All Categories", C, "", false, always, list),
        spec(
            "categories.quickClick",
            "Set Quick Click (category)",
            "Home › Tags › Categorize",
            r#"{"name": string}"#,
            false,
            has_categories,
            quick_click,
        ),
        spec(
            "categories.setQuickClick",
            "Set Quick Click Category",
            "Home › Tags › Categorize",
            r#"{"name"?: string} (no name = open the dialog)"#,
            false,
            has_categories,
            set_quick_click,
        ),
    ]
}

fn has_categories(s: &Session) -> Option<&'static str> {
    if s.mailbox.categories.is_empty() { Some("no categories") } else { None }
}

fn find(s: &Session, name: &str) -> Result<usize, CmdError> {
    let key = name.trim();
    s.mailbox
        .categories
        .iter()
        .position(|c| c.name.trim().eq_ignore_ascii_case(key))
        .ok_or_else(|| CmdError::Params(format!("no category named \"{key}\"")))
}

fn parse_color(v: &str) -> Result<CategoryColor, CmdError> {
    CategoryColor::from_name(v).ok_or_else(|| CmdError::Params(format!("unknown colour `{v}`")))
}

/// "Ctrl+F2" … "Ctrl+F12" (also "Mod+Fn"); empty clears.
fn parse_shortcut(v: &str) -> Result<String, CmdError> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(String::new());
    }
    let lower = t.to_ascii_lowercase();
    let n = lower
        .strip_prefix("ctrl+f")
        .or_else(|| lower.strip_prefix("mod+f"))
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|n| (2..=12).contains(n));
    n.map(|n| format!("Ctrl+F{n}"))
        .ok_or_else(|| CmdError::Params(format!("shortcut must be Ctrl+F2 … Ctrl+F12, got `{t}`")))
}

/// Gives `shortcut` to category `idx`, taking it away from any other category.
fn assign_shortcut(s: &mut Session, idx: usize, shortcut: String) {
    if !shortcut.is_empty() {
        for (i, c) in s.mailbox.categories.iter_mut().enumerate() {
            if i != idx && c.shortcut == shortcut {
                c.shortcut.clear();
            }
        }
    }
    if let Some(c) = s.mailbox.categories.get_mut(idx) {
        c.shortcut = shortcut;
    }
}

fn category_json(c: &Category) -> Value {
    json!({"name": c.name, "color": c.color.name(), "hex": c.color.rgb().to_hex(), "shortcut": c.shortcut})
}

fn new_category(s: &mut Session, p: &Value) -> CmdResult {
    let name = clean_name(req_str(p, "name")?)?;
    if find(s, &name).is_ok() {
        return Err(CmdError::Failed(format!("a category named \"{name}\" already exists")));
    }
    if s.mailbox.categories.len() >= MAX_CATEGORIES {
        return Err(CmdError::Failed(format!("at most {MAX_CATEGORIES} categories")));
    }
    let color = opt_str(p, "color").map(parse_color).transpose()?.unwrap_or(CategoryColor::None);
    let sc = opt_str(p, "shortcut").map(parse_shortcut).transpose()?.unwrap_or_default();
    s.mailbox.categories.push(Category { name: name.clone(), color, shortcut: String::new() });
    let idx = s.mailbox.categories.len().saturating_sub(1);
    assign_shortcut(s, idx, sc);
    Ok(json!({"name": name}))
}

fn rename(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find(s, req_str(p, "name")?)?;
    let new_name = clean_name(req_str(p, "newName")?)?;
    if let Ok(other) = find(s, &new_name)
        && other != idx
    {
        return Err(CmdError::Failed(format!("a category named \"{new_name}\" already exists")));
    }
    let Some(c) = s.mailbox.categories.get_mut(idx) else {
        return Err(CmdError::Params("no such category".into()));
    };
    let old = std::mem::replace(&mut c.name, new_name.clone());
    // Items carry category names: rename them along with the master list, on every kind of item.
    let old_key = old.trim().to_lowercase();
    let new_key = new_name.trim().to_lowercase();
    let items = for_each_item_categories(&mut s.mailbox, |cats| {
        if !cats.iter().any(|c| c.trim().to_lowercase() == old_key) {
            return false;
        }
        // Rename in place (first occurrence keeps its position) and drop duplicates the rename creates.
        let mut seen = false;
        cats.retain_mut(|c| {
            let key = c.trim().to_lowercase();
            if key != old_key && key != new_key {
                return true;
            }
            if seen {
                return false;
            }
            seen = true;
            c.clone_from(&new_name);
            true
        });
        true
    });
    Ok(json!({"name": new_name, "items": items}))
}

/// Runs `f` on the category list of every mail, appointment, contact, task and note; returns how many
/// lists `f` reported as changed.
fn for_each_item_categories(
    mb: &mut mailcraft_model::Mailbox,
    mut f: impl FnMut(&mut Vec<String>) -> bool,
) -> usize {
    let lists = mb
        .mail
        .values_mut()
        .map(|m| &mut m.categories)
        .chain(mb.appointments.values_mut().map(|a| &mut a.categories))
        .chain(mb.contacts.values_mut().map(|c| &mut c.categories))
        .chain(mb.tasks.values_mut().map(|t| &mut t.categories))
        .chain(mb.notes.values_mut().map(|n| &mut n.categories));
    let mut changed = 0usize;
    for cats in lists {
        if f(cats) {
            changed = changed.saturating_add(1);
        }
    }
    changed
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find(s, req_str(p, "name")?)?;
    if idx >= s.mailbox.categories.len() {
        return Err(CmdError::Params("no such category".into()));
    }
    let removed = s.mailbox.categories.remove(idx);
    let key = removed.name.trim().to_lowercase();
    let items = for_each_item_categories(&mut s.mailbox, |cats| {
        let before = cats.len();
        cats.retain(|c| c.trim().to_lowercase() != key);
        cats.len() != before
    });
    Ok(json!({"deleted": removed.name, "items": items}))
}

fn color(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find(s, req_str(p, "name")?)?;
    let color = parse_color(req_str(p, "color")?)?;
    let Some(c) = s.mailbox.categories.get_mut(idx) else {
        return Err(CmdError::Params("no such category".into()));
    };
    c.color = color;
    let out = category_json(c);
    Ok(out)
}

fn shortcut(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find(s, req_str(p, "name")?)?;
    let sc = parse_shortcut(req_str(p, "shortcut")?)?;
    assign_shortcut(s, idx, sc);
    Ok(s.mailbox.categories.get(idx).map(category_json).unwrap_or(Value::Null))
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    Ok(json!({"categories": s.mailbox.categories.iter().map(category_json).collect::<Vec<_>>()}))
}

/// Validates the Quick Click category and reports it. There is no session or mailbox field for it yet, so the
/// caller (UI profile) keeps the returned value.
fn quick_click(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find(s, req_str(p, "name")?)?;
    let name = s.mailbox.categories.get(idx).map(|c| c.name.clone()).unwrap_or_default();
    Ok(json!({"quickClick": name}))
}

fn set_quick_click(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("name").is_some() {
        return quick_click(s, p);
    }
    let cats: Vec<Value> = s.mailbox.categories.iter().map(category_json).collect();
    dialog(s, "setQuickClick", json!({"categories": cats}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use mailcraft_model::ItemId;

    #[test]
    fn new_rename_delete_follow_items() {
        let (mut s, inbox, _) = session();
        s.run("categories.new", &json!({"name": "Work", "color": "dark blue", "shortcut": "Mod+F3"}))
            .unwrap();
        assert!(matches!(s.run("categories.new", &json!({"name": "work"})), Err(CmdError::Failed(_))));
        let c = s.mailbox.categories.last().unwrap();
        assert_eq!((c.color, c.shortcut.as_str()), (CategoryColor::DarkBlue, "Ctrl+F3"));
        let ids: Vec<_> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        for id in &ids {
            s.mailbox.mail.get_mut(id).unwrap().categories = vec!["Work".into(), "Red category".into()];
        }
        let r = s.run("categories.rename", &json!({"name": "work", "newName": "Job"})).unwrap();
        assert_eq!(r["items"], 3);
        assert!(s.mailbox.items_in(inbox).iter().all(|m| m.categories[0] == "Job"));
        assert!(matches!(
            s.run("categories.rename", &json!({"name": "Job", "newName": "Red category"})),
            Err(CmdError::Failed(_))
        ));
        let r = s.run("categories.delete", &json!({"name": "Job"})).unwrap();
        assert_eq!(r["items"], 3);
        assert!(s.mailbox.items_in(inbox).iter().all(|m| m.categories == vec!["Red category".to_string()]));
        assert!(Category::find(&s.mailbox.categories, "Job").is_none());
    }

    #[test]
    fn rename_and_delete_follow_calendar_contact_task_and_note_items() {
        let (mut s, _, _) = session();
        let appt = s
            .run("calendar.newAppointment", &json!({"subject": "A", "categories": ["Red category"]}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let contact = s
            .run("people.new", &json!({"fullName": "Ann Lee", "categories": ["red category"]}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let task = s
            .run("tasks.new", &json!({"subject": "T", "categories": ["Red category", "Work"]}))
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let note = s.run("notes.new", &json!({"text": "N", "categories": ["Red category"]})).unwrap()["id"]
            .as_u64()
            .unwrap();
        let cats = |s: &Session| -> Vec<Vec<String>> {
            let mb = &s.mailbox;
            vec![
                mb.appointments[&ItemId(appt)].categories.clone(),
                mb.contacts[&ItemId(contact)].categories.clone(),
                mb.tasks[&ItemId(task)].categories.clone(),
                mb.notes[&ItemId(note)].categories.clone(),
            ]
        };
        let r = s.run("categories.rename", &json!({"name": "Red category", "newName": "Urgent"})).unwrap();
        assert_eq!(r["items"], 4);
        assert_eq!(
            cats(&s),
            vec![
                vec!["Urgent".to_string()],
                vec!["Urgent".to_string()],
                vec!["Urgent".to_string(), "Work".to_string()],
                vec!["Urgent".to_string()],
            ]
        );
        let r = s.run("categories.delete", &json!({"name": "urgent"})).unwrap();
        assert_eq!(r["items"], 4);
        assert_eq!(cats(&s), vec![vec![], vec![], vec!["Work".to_string()], vec![]]);
    }

    #[test]
    fn rename_onto_a_name_an_item_already_has_keeps_one() {
        let (mut s, inbox, _) = session();
        let id = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        // "Job" is on the item but not in the master list (e.g. imported mail).
        s.mailbox.mail.get_mut(&id).unwrap().categories = vec!["Red category".into(), "Job".into()];
        s.run("categories.rename", &json!({"name": "Red category", "newName": "Job"})).unwrap();
        assert_eq!(s.mailbox.mail[&id].categories, vec!["Job".to_string()]);
    }

    #[test]
    fn color_and_shortcut() {
        let (mut s, _, _) = session();
        s.run("categories.color", &json!({"name": "Red category", "color": "Teal"})).unwrap();
        assert_eq!(s.mailbox.categories[0].color, CategoryColor::Teal);
        assert!(matches!(
            s.run("categories.color", &json!({"name": "Red category", "color": "Mauve"})),
            Err(CmdError::Params(_))
        ));
        s.run("categories.shortcut", &json!({"name": "Red category", "shortcut": "Ctrl+F2"})).unwrap();
        s.run("categories.shortcut", &json!({"name": "Blue category", "shortcut": "ctrl+f2"})).unwrap();
        assert_eq!(s.mailbox.categories[0].shortcut, "");
        assert_eq!(s.mailbox.categories[4].shortcut, "Ctrl+F2");
        assert!(matches!(
            s.run("categories.shortcut", &json!({"name": "Red category", "shortcut": "Ctrl+F1"})),
            Err(CmdError::Params(_))
        ));
        s.run("categories.shortcut", &json!({"name": "Blue category", "shortcut": ""})).unwrap();
        assert_eq!(s.mailbox.categories[4].shortcut, "");
    }

    #[test]
    fn quick_click() {
        let (mut s, _, _) = session();
        assert_eq!(
            s.run("categories.quickClick", &json!({"name": "blue CATEGORY"})).unwrap()["quickClick"],
            "Blue category"
        );
        assert!(matches!(s.run("categories.quickClick", &json!({"name": "nope"})), Err(CmdError::Params(_))));
        assert_eq!(s.run("categories.setQuickClick", &json!({})).unwrap()["dialog"], "setQuickClick");
        assert_eq!(
            s.run("categories.setQuickClick", &json!({"name": "Red category"})).unwrap()["quickClick"],
            "Red category"
        );
        assert_eq!(s.run("categories.list", &json!({})).unwrap()["categories"].as_array().unwrap().len(), 6);
    }
}
