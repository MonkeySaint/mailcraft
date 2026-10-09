//! `rules.*`: CRUD over [`Mailbox::rules`](mailcraft_model::Mailbox) (`plan/outlook/rules.md`), the one-click
//! "Always Move Messages From" rule, Create Rule from a message, Run Rules Now, and JSON export/import.
//!
//! Also hosts small helpers shared by the admin command files (dialogs, selection, parsing, files). Changes are
//! journaled by `Session::run` (it diffs the mailbox around every mutating command).

use std::path::Path;

use serde_json::{Value, json};

use crate::cmd::util::{ok, opt_bool, opt_folder, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::rule::RuleContext;
use mailcraft_model::{FolderId, FolderKind, ItemId, Rule, RuleAction, RuleCondition};

// ---------------------------------------------------------------------------------------------------------
// Shared helpers for the admin command files (rules, quicksteps, categories, sendReceive, file, options, help).

/// Queues `Notice::OpenDialog(name, params)` and returns `{"dialog": name, "params": params}`.
pub(crate) fn dialog(s: &mut Session, name: &str, params: Value) -> CmdResult {
    s.notify(Notice::OpenDialog(name.to_owned(), params.clone()));
    Ok(json!({"dialog": name, "params": params}))
}

/// Enabled predicate: something is selected in the list.
pub(crate) fn need_selection(s: &Session) -> Option<&'static str> {
    if s.target_ids(&Value::Null).iter().any(|id| s.mailbox.mail.contains_key(id)) {
        None
    } else {
        Some("no message selected")
    }
}

/// The first existing mail item among the targets (focus first when it is part of the selection).
pub(crate) fn focused_item(s: &Session, p: &Value) -> Option<ItemId> {
    let ids = s.target_ids(p);
    if crate::cmd::util::opt_ids(p).is_none()
        && let Some(f) = s.selection.focus
        && ids.contains(&f)
        && s.mailbox.mail.contains_key(&f)
    {
        return Some(f);
    }
    ids.into_iter().find(|id| s.mailbox.mail.contains_key(id))
}

/// Parses `p[key]` as JSON of type `T` (model enums use serde's external tagging, e.g. `{"MoveTo": 5}`).
pub(crate) fn parse_field<T: serde::de::DeserializeOwned>(
    p: &Value,
    key: &str,
) -> Result<Option<T>, CmdError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            serde_json::from_value(v.clone()).map(Some).map_err(|e| CmdError::Params(format!("`{key}`: {e}")))
        }
    }
}

/// Reads a file of at most `cap` bytes.
pub(crate) fn read_file_capped(path: &Path, cap: u64) -> Result<Vec<u8>, CmdError> {
    match mailcraft_store::journal::read_capped(path, cap) {
        Ok(Some(b)) => Ok(b),
        Ok(None) => Err(CmdError::Failed(format!("{} not found", path.display()))),
        Err(e) => Err(CmdError::Failed(format!("{}: {e}", path.display()))),
    }
}

/// Writes `data` to `path` atomically.
pub(crate) fn write_file(path: &Path, data: &[u8]) -> Result<(), CmdError> {
    use std::io::Write;
    mailcraft_store::journal::write_atomic(path, |w| w.write_all(data))
        .map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))
}

/// Longest rule / quick step / category name accepted.
pub(crate) const MAX_NAME_CHARS: usize = 255;

/// Trimmed, non-empty name of at most [`MAX_NAME_CHARS`] characters.
pub(crate) fn clean_name(name: &str) -> Result<String, CmdError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(CmdError::Params("name must not be empty".into()));
    }
    if n.chars().count() > MAX_NAME_CHARS {
        return Err(CmdError::Params(format!("name longer than {MAX_NAME_CHARS} characters")));
    }
    if n.chars().any(char::is_control) {
        return Err(CmdError::Params("name must not contain control characters".into()));
    }
    Ok(n.to_owned())
}

// ---------------------------------------------------------------------------------------------------------

/// Largest rules export file read by `rules.import`.
const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
/// Most rules kept in a mailbox.
const MAX_RULES: usize = 2_000;

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
    const R: &str = "Home › Move › Rules";
    const M: &str = "Rules and Alerts";
    vec![
        spec(
            "rules.create",
            "Create Rule",
            R,
            r#"{"name": string, "conditions": [RuleCondition], "actions": [RuleAction], "exceptions"?: [RuleCondition], "enabled"?: bool, "runNow"?: bool}"#,
            true,
            always,
            create,
        ),
        spec(
            "rules.createFromMessage",
            "Create Rule",
            R,
            r#"{"actions"?: [RuleAction], "from"?: bool, "subject"?: bool|string, "sentTo"?: bool, "name"?: string, "runNow"?: bool} (no actions = open the dialog prefilled)"#,
            true,
            need_selection,
            create_from_message,
        ),
        spec(
            "rules.alwaysMoveFrom",
            "Always Move Messages From",
            "Context › Message › Rules",
            r#"{"folder"?: number} (no folder = pick one)"#,
            true,
            need_selection,
            always_move_from,
        ),
        spec("rules.manage", "Manage Rules & Alerts", R, "", false, always, manage),
        spec("rules.list", "List Rules", M, "", false, always, list),
        spec(
            "rules.new",
            "New Rule",
            M,
            r#"{"name"?: string, ...as rules.create} (no name = open the wizard)"#,
            true,
            always,
            new_rule,
        ),
        spec(
            "rules.edit",
            "Edit Rule Settings",
            M,
            r#"{"id": number|string, "name"?: string, "conditions"?: [..], "actions"?: [..], "exceptions"?: [..], "enabled"?: bool}"#,
            true,
            has_rules,
            edit,
        ),
        spec(
            "rules.rename",
            "Rename Rule",
            M,
            r#"{"id": number|string, "name": string}"#,
            true,
            has_rules,
            rename,
        ),
        spec("rules.delete", "Delete", M, r#"{"id": number|string}"#, true, has_rules, delete),
        spec("rules.copy", "Copy", M, r#"{"id": number|string, "name"?: string}"#, true, has_rules, copy),
        spec("rules.moveUp", "Move Up", M, r#"{"id": number|string}"#, true, has_rules, move_up),
        spec("rules.moveDown", "Move Down", M, r#"{"id": number|string}"#, true, has_rules, move_down),
        spec(
            "rules.enable",
            "Turn on this rule",
            M,
            r#"{"id": number|string, "enabled"?: bool} (no enabled = toggle)"#,
            true,
            has_rules,
            enable,
        ),
        spec(
            "rules.runNow",
            "Run Rules Now",
            M,
            r#"{"ids"?: [rule id], "folder"?: number, "subfolders"?: bool} (no ids = all enabled rules)"#,
            true,
            has_rules,
            run_now,
        ),
        spec("rules.export", "Export Rules", M, r#"{"path": string}"#, false, has_rules, export),
        spec(
            "rules.import",
            "Import Rules",
            M,
            r#"{"path": string, "replace"?: bool}"#,
            true,
            always,
            import,
        ),
    ]
}

fn has_rules(s: &Session) -> Option<&'static str> {
    if s.mailbox.rules.is_empty() { Some("no rules") } else { None }
}

/// Index of the rule named by `p.id` (number, numeric string, or rule name).
fn find_rule(s: &Session, p: &Value) -> Result<usize, CmdError> {
    let v = p.get("id").ok_or_else(|| CmdError::Params("`id` (rule id or name) required".into()))?;
    let pos = match v {
        Value::Number(n) => n.as_u64().and_then(|id| s.mailbox.rules.iter().position(|r| r.id == id)),
        Value::String(name) => {
            let key = name.trim();
            key.parse::<u64>()
                .ok()
                .and_then(|id| s.mailbox.rules.iter().position(|r| r.id == id))
                .or_else(|| s.mailbox.rules.iter().position(|r| r.name.trim().eq_ignore_ascii_case(key)))
        }
        _ => None,
    };
    pos.ok_or_else(|| CmdError::Params(format!("no such rule {v}")))
}

fn next_rule_id(s: &Session) -> Result<u64, CmdError> {
    match s.mailbox.rules.iter().map(|r| r.id).max() {
        None => Ok(1),
        Some(m) => m.checked_add(1).ok_or_else(|| CmdError::Failed("rule ids exhausted".into())),
    }
}

/// Folders named by actions must exist and hold mail.
fn check_actions(s: &Session, actions: &[RuleAction]) -> Result<(), CmdError> {
    for a in actions {
        if let RuleAction::MoveTo(f) | RuleAction::CopyTo(f) = a {
            match s.mailbox.folder(*f) {
                Some(folder) if folder.kind == FolderKind::Mail => {}
                Some(folder) => {
                    return Err(CmdError::Params(format!("\"{}\" is not a mail folder", folder.name)));
                }
                None => return Err(CmdError::Params(format!("no such folder {}", f.0))),
            }
        }
    }
    Ok(())
}

fn rule_json(s: &Session, r: &Rule) -> Value {
    let name_of =
        |f: FolderId| s.mailbox.folder(f).map(|x| x.name.clone()).unwrap_or_else(|| "specified".into());
    json!({
        "id": r.id,
        "name": r.name,
        "label": r.list_label(),
        "enabled": r.enabled,
        "clientOnly": r.is_client_only(),
        "conditions": r.conditions,
        "actions": r.actions,
        "exceptions": r.exceptions,
        "description": r.describe(&name_of),
    })
}

fn insert_rule(s: &mut Session, mut rule: Rule) -> Result<u64, CmdError> {
    if s.mailbox.rules.len() >= MAX_RULES {
        return Err(CmdError::Failed(format!("at most {MAX_RULES} rules")));
    }
    rule.id = next_rule_id(s)?;
    let id = rule.id;
    s.mailbox.rules.push(rule);
    Ok(id)
}

/// Mail items in `folder` (and its subtree when `subfolders`).
fn items_in_scope(s: &Session, folder: FolderId, subfolders: bool) -> Vec<ItemId> {
    let folders: Vec<FolderId> = if subfolders { s.mailbox.subtree(folder) } else { vec![folder] };
    s.mailbox
        .mail
        .values()
        .filter(|m| folders.contains(&m.folder) || m.folder == folder)
        .map(|m| m.id)
        .collect()
}

/// Runs the rules with ids `only` (all enabled rules when `None`, disabled ones included when named) over
/// `items` through the shared rule engine.
fn run_rules_on(s: &mut Session, only: Option<&[u64]>, items: &[ItemId]) -> usize {
    let saved = s.mailbox.rules.clone();
    if let Some(only) = only {
        s.mailbox.rules = saved
            .iter()
            .filter(|r| only.contains(&r.id))
            .cloned()
            .map(|mut r| {
                r.enabled = true;
                r
            })
            .collect();
    }
    let n = crate::cmd::folder::apply_rules(s, items);
    s.mailbox.rules = saved;
    n
}

fn rule_from_params(s: &Session, p: &Value, require_name: bool) -> Result<Rule, CmdError> {
    let name = match opt_str(p, "name") {
        Some(n) => clean_name(n)?,
        None if require_name => return Err(CmdError::Params("`name` (string) required".into())),
        None => String::new(),
    };
    let conditions: Vec<RuleCondition> = parse_field(p, "conditions")?.unwrap_or_default();
    let actions: Vec<RuleAction> = parse_field(p, "actions")?.unwrap_or_default();
    let exceptions: Vec<RuleCondition> = parse_field(p, "exceptions")?.unwrap_or_default();
    if actions.is_empty() {
        return Err(CmdError::Params("a rule needs at least one action".into()));
    }
    check_actions(s, &actions)?;
    Ok(Rule { id: 0, name, enabled: opt_bool(p, "enabled").unwrap_or(true), conditions, actions, exceptions })
}

fn finish_create(s: &mut Session, rule: Rule, p: &Value) -> CmdResult {
    let id = insert_rule(s, rule)?;
    let mut applied = 0;
    if opt_bool(p, "runNow").unwrap_or(false) {
        let folder = s.target_folder(p);
        let items = items_in_scope(s, folder, false);
        applied = run_rules_on(s, Some(&[id]), &items);
    }
    let rule = s.mailbox.rules.iter().find(|r| r.id == id).map(|r| rule_json(s, r)).unwrap_or(Value::Null);
    Ok(json!({"id": id, "rule": rule, "applied": applied}))
}

fn create(s: &mut Session, p: &Value) -> CmdResult {
    let rule = rule_from_params(s, p, true)?;
    finish_create(s, rule, p)
}

fn new_rule(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("name").is_none() {
        return dialog(s, "rulesWizard", json!({}));
    }
    create(s, p)
}

/// Prefill for the Create Rule dialog from an item: sender, first recipient, subject.
fn prefill(s: &Session, id: ItemId) -> Option<Value> {
    let m = s.mailbox.mail.get(&id)?;
    let from = m.from.as_ref().or(m.sender.as_ref());
    Some(json!({
        "item": id.0,
        "from": from.map(|f| f.addr.clone()),
        "fromName": from.map(|f| f.display().to_owned()),
        "subject": m.subject,
        "sentTo": m.to.first().map(|t| t.addr.clone()),
    }))
}

fn create_from_message(s: &mut Session, p: &Value) -> CmdResult {
    let id = focused_item(s, p).ok_or_else(|| CmdError::Disabled("no message selected".into()))?;
    let pre = prefill(s, id).unwrap_or(Value::Null);
    if p.get("actions").is_none() {
        return dialog(s, "createRule", pre);
    }
    let Some(m) = s.mailbox.mail.get(&id) else {
        return Err(CmdError::Disabled("no message selected".into()));
    };
    let from = m.from.as_ref().or(m.sender.as_ref()).map(|f| f.addr.clone());
    let mut conditions = Vec::new();
    if opt_bool(p, "from").unwrap_or(false)
        && let Some(f) = &from
    {
        conditions.push(RuleCondition::From(vec![f.clone()]));
    }
    let subject = match p.get("subject") {
        Some(Value::Bool(true)) => Some(m.subject.trim().to_owned()),
        Some(Value::String(t)) => Some(t.trim().to_owned()),
        _ => None,
    };
    if let Some(sub) = subject.as_ref().filter(|t| !t.is_empty()) {
        conditions.push(RuleCondition::SubjectContains(vec![sub.clone()]));
    }
    if opt_bool(p, "sentTo").unwrap_or(false)
        && let Some(t) = m.to.first()
    {
        conditions.push(RuleCondition::SentTo(vec![t.addr.clone()]));
    }
    if conditions.is_empty() {
        return Err(CmdError::Params("tick at least one condition (from, subject, sentTo)".into()));
    }
    let default_name = subject.filter(|t| !t.is_empty()).or(from).unwrap_or_else(|| m.subject.clone());
    let name = match opt_str(p, "name") {
        Some(n) => clean_name(n)?,
        None => clean_name(&default_name).unwrap_or_else(|_| format!("Rule {}", id.0)),
    };
    let actions: Vec<RuleAction> = parse_field(p, "actions")?.unwrap_or_default();
    if actions.is_empty() {
        return Err(CmdError::Params("a rule needs at least one action".into()));
    }
    check_actions(s, &actions)?;
    let rule = Rule { id: 0, name, enabled: true, conditions, actions, exceptions: Vec::new() };
    finish_create(s, rule, p)
}

fn always_move_from(s: &mut Session, p: &Value) -> CmdResult {
    let id = focused_item(s, p).ok_or_else(|| CmdError::Disabled("no message selected".into()))?;
    let Some(m) = s.mailbox.mail.get(&id) else {
        return Err(CmdError::Disabled("no message selected".into()));
    };
    let Some(sender) = m.from.clone().or_else(|| m.sender.clone()) else {
        return Err(CmdError::Disabled("the message has no sender".into()));
    };
    let source_folder = m.folder;
    let Some(folder) = opt_folder(p) else {
        return dialog(
            s,
            "selectFolder",
            json!({"purpose": "alwaysMoveFrom", "from": sender.addr, "item": id.0}),
        );
    };
    let actions = vec![RuleAction::MoveTo(folder)];
    check_actions(s, &actions)?;
    let name = clean_name(sender.display()).or_else(|_| clean_name(&sender.addr))?;
    let rule = Rule {
        id: 0,
        name,
        enabled: true,
        conditions: vec![RuleCondition::From(vec![sender.addr.clone()])],
        actions,
        exceptions: Vec::new(),
    };
    // Messages already in the source folder from this sender move now too.
    let ctx = RuleContext { my_addresses: s.me.clone(), account_address: String::new() };
    let existing: Vec<ItemId> = s
        .mailbox
        .items_in(source_folder)
        .into_iter()
        .filter(|m| rule.matches(m, &ctx))
        .map(|m| m.id)
        .collect();
    let rule_id = insert_rule(s, rule)?;
    let moved = s.mailbox.move_items(&existing, folder)?;
    Ok(json!({"id": rule_id, "moved": moved.len()}))
}

fn manage(s: &mut Session, _: &Value) -> CmdResult {
    let rules: Vec<Value> = s.mailbox.rules.iter().map(|r| rule_json(s, r)).collect();
    dialog(s, "rulesAndAlerts", json!({"rules": rules}))
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    let rules: Vec<Value> = s.mailbox.rules.iter().map(|r| rule_json(s, r)).collect();
    Ok(json!({"rules": rules}))
}

fn edit(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    let fields = ["name", "conditions", "actions", "exceptions", "enabled"];
    if !fields.iter().any(|k| p.get(*k).is_some()) {
        let rule = s.mailbox.rules.get(i).map(|r| rule_json(s, r)).unwrap_or(Value::Null);
        return dialog(s, "rulesWizard", json!({"rule": rule}));
    }
    let name = opt_str(p, "name").map(clean_name).transpose()?;
    let conditions: Option<Vec<RuleCondition>> = parse_field(p, "conditions")?;
    let actions: Option<Vec<RuleAction>> = parse_field(p, "actions")?;
    let exceptions: Option<Vec<RuleCondition>> = parse_field(p, "exceptions")?;
    if let Some(a) = &actions {
        if a.is_empty() {
            return Err(CmdError::Params("a rule needs at least one action".into()));
        }
        check_actions(s, a)?;
    }
    let Some(r) = s.mailbox.rules.get_mut(i) else {
        return Err(CmdError::Params("no such rule".into()));
    };
    if let Some(n) = name {
        r.name = n;
    }
    if let Some(c) = conditions {
        r.conditions = c;
    }
    if let Some(a) = actions {
        r.actions = a;
    }
    if let Some(e) = exceptions {
        r.exceptions = e;
    }
    if let Some(en) = opt_bool(p, "enabled") {
        r.enabled = en;
    }
    let id = r.id;
    Ok(json!({"id": id}))
}

fn rename(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    let name = clean_name(req_str(p, "name")?)?;
    let Some(r) = s.mailbox.rules.get_mut(i) else {
        return Err(CmdError::Params("no such rule".into()));
    };
    r.name = name;
    Ok(ok())
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    if i < s.mailbox.rules.len() {
        let r = s.mailbox.rules.remove(i);
        return Ok(json!({"deleted": r.id}));
    }
    Err(CmdError::Params("no such rule".into()))
}

fn copy(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    if s.mailbox.rules.len() >= MAX_RULES {
        return Err(CmdError::Failed(format!("at most {MAX_RULES} rules")));
    }
    let Some(mut r) = s.mailbox.rules.get(i).cloned() else {
        return Err(CmdError::Params("no such rule".into()));
    };
    if let Some(n) = opt_str(p, "name") {
        r.name = clean_name(n)?;
    }
    r.id = next_rule_id(s)?;
    let id = r.id;
    let at = i.saturating_add(1).min(s.mailbox.rules.len());
    s.mailbox.rules.insert(at, r);
    Ok(json!({"id": id}))
}

fn shift(s: &mut Session, p: &Value, up: bool) -> CmdResult {
    let i = find_rule(s, p)?;
    let j = if up { i.checked_sub(1) } else { i.checked_add(1).filter(|j| *j < s.mailbox.rules.len()) };
    let Some(j) = j else {
        return Ok(json!({"moved": false}));
    };
    s.mailbox.rules.swap(i, j);
    Ok(json!({"moved": true, "index": j}))
}

fn move_up(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, true)
}

fn move_down(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, false)
}

fn enable(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    let Some(r) = s.mailbox.rules.get_mut(i) else {
        return Err(CmdError::Params("no such rule".into()));
    };
    r.enabled = opt_bool(p, "enabled").unwrap_or(!r.enabled);
    let en = r.enabled;
    Ok(json!({"enabled": en}))
}

fn run_now(s: &mut Session, p: &Value) -> CmdResult {
    let only: Option<Vec<u64>> = match p.get("ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            let mut ids = Vec::new();
            for v in a {
                let i = find_rule(s, &json!({"id": v}))?;
                if let Some(r) = s.mailbox.rules.get(i) {
                    ids.push(r.id);
                }
            }
            Some(ids)
        }
        Some(_) => return Err(CmdError::Params("`ids` must be an array of rule ids".into())),
    };
    let folder = s.target_folder(p);
    if s.mailbox.folder(folder).is_none() {
        return Err(CmdError::Params(format!("no such folder {}", folder.0)));
    }
    let items = items_in_scope(s, folder, opt_bool(p, "subfolders").unwrap_or(false));
    let applied = run_rules_on(s, only.as_deref(), &items);
    Ok(json!({"applied": applied, "scanned": items.len()}))
}

fn export(s: &mut Session, p: &Value) -> CmdResult {
    let path = Path::new(req_str(p, "path")?);
    let doc = json!({"mailcraft_rules": 1, "rules": s.mailbox.rules});
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| CmdError::Failed(e.to_string()))?;
    write_file(path, &bytes)?;
    Ok(json!({"exported": s.mailbox.rules.len(), "path": path.display().to_string()}))
}

fn import(s: &mut Session, p: &Value) -> CmdResult {
    let path = Path::new(req_str(p, "path")?);
    let bytes = read_file_capped(path, MAX_IMPORT_BYTES)?;
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| CmdError::Failed(format!("not a rules file: {e}")))?;
    let list = match &doc {
        Value::Array(_) => doc.clone(),
        Value::Object(o) => {
            o.get("rules").cloned().ok_or_else(|| CmdError::Failed("not a rules file".into()))?
        }
        _ => return Err(CmdError::Failed("not a rules file".into())),
    };
    let rules: Vec<Rule> =
        serde_json::from_value(list).map_err(|e| CmdError::Failed(format!("not a rules file: {e}")))?;
    if opt_bool(p, "replace").unwrap_or(false) {
        s.mailbox.rules.clear();
    }
    let mut imported = 0usize;
    let mut disabled = 0usize;
    for mut r in rules {
        if s.mailbox.rules.len() >= MAX_RULES {
            break;
        }
        r.name = clean_name(&r.name).unwrap_or_else(|_| "Imported rule".into());
        // A rule pointing at a folder this mailbox does not have is kept but turned off.
        if r.enabled && check_actions(s, &r.actions).is_err() {
            r.enabled = false;
            disabled = disabled.saturating_add(1);
        }
        r.id = next_rule_id(s)?;
        s.mailbox.rules.push(r);
        imported = imported.saturating_add(1);
    }
    Ok(json!({"imported": imported, "disabled": disabled}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mailcraft_model::{Address, FolderRole, MailItem, Mailbox};

    pub(crate) fn session() -> (Session, FolderId, FolderId) {
        let mb = Mailbox::new_local("Test");
        let mut s = Session::new(mb);
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap_or_default();
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        let proj = s.mailbox.create_folder(inbox, "Projects").unwrap();
        for (i, (from, subj)) in
            [("alice@example.com", "Budget"), ("bob@example.org", "Lunch"), ("alice@example.com", "Forecast")]
                .iter()
                .enumerate()
        {
            s.mailbox.add_mail(MailItem {
                folder: inbox,
                subject: (*subj).into(),
                from: Some(Address::new(Some(&format!("Person {i}")), from)),
                to: vec![Address::new(None, "me@example.net")],
                ..Default::default()
            });
        }
        s.selection.folder = inbox;
        let first = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        s.selection.items = vec![first];
        s.selection.focus = Some(first);
        (s, inbox, proj)
    }

    #[test]
    fn create_rename_enable_delete() {
        let (mut s, _, proj) = session();
        let r = s
            .run(
                "rules.create",
                &json!({"name": " Budget ", "conditions": [{"SubjectContains": ["budget"]}], "actions": [{"MoveTo": proj.0}]}),
            )
            .unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(s.mailbox.rules.len(), 1);
        assert_eq!(s.mailbox.rules[0].name, "Budget");
        assert!(r["rule"]["description"].as_array().unwrap().len() >= 3);
        s.run("rules.rename", &json!({"id": id, "name": "Money"})).unwrap();
        assert_eq!(s.mailbox.rules[0].name, "Money");
        let e = s.run("rules.enable", &json!({"id": "Money"})).unwrap();
        assert_eq!(e["enabled"], false);
        s.run("rules.delete", &json!({"id": id})).unwrap();
        assert!(s.mailbox.rules.is_empty());
        assert!(matches!(s.run("rules.delete", &json!({"id": id})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn create_validates() {
        let (mut s, _, _) = session();
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "conditions": [], "actions": []})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "actions": [{"MoveTo": 999999}]})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "actions": [{"Bogus": 1}]})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "  ", "actions": ["MarkAsRead"]})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn copy_and_reorder() {
        let (mut s, _, _) = session();
        for n in ["A", "B"] {
            s.run("rules.create", &json!({"name": n, "actions": ["MarkAsRead"]})).unwrap();
        }
        let c = s.run("rules.copy", &json!({"id": "A", "name": "A2"})).unwrap();
        let names: Vec<&str> = s.mailbox.rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["A", "A2", "B"]);
        assert_ne!(c["id"].as_u64().unwrap(), s.mailbox.rules[0].id);
        s.run("rules.moveDown", &json!({"id": "A"})).unwrap();
        s.run("rules.moveDown", &json!({"id": "A"})).unwrap();
        assert_eq!(s.run("rules.moveDown", &json!({"id": "A"})).unwrap()["moved"], false);
        s.run("rules.moveUp", &json!({"id": "B"})).unwrap();
        let names: Vec<&str> = s.mailbox.rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["B", "A2", "A"]);
        assert_eq!(s.run("rules.moveUp", &json!({"id": "B"})).unwrap()["moved"], false);
    }

    #[test]
    fn create_from_message_dialog_and_direct() {
        let (mut s, _, proj) = session();
        let r = s.run("rules.createFromMessage", &json!({})).unwrap();
        assert_eq!(r["dialog"], "createRule");
        assert_eq!(r["params"]["from"], "alice@example.com");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "createRule"));
        let r = s
            .run(
                "rules.createFromMessage",
                &json!({"from": true, "subject": true, "actions": [{"MoveTo": proj.0}]}),
            )
            .unwrap();
        let rule = &s.mailbox.rules[0];
        assert_eq!(rule.conditions.len(), 2);
        assert_eq!(r["id"].as_u64(), Some(rule.id));
        assert!(matches!(
            s.run("rules.createFromMessage", &json!({"actions": ["MarkAsRead"]})),
            Err(CmdError::Params(_))
        ));
        s.selection.items.clear();
        s.selection.focus = None;
        assert!(matches!(s.run("rules.createFromMessage", &json!({})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn always_move_from_moves_existing() {
        let (mut s, inbox, proj) = session();
        let r = s.run("rules.alwaysMoveFrom", &json!({})).unwrap();
        assert_eq!(r["dialog"], "selectFolder");
        let r = s.run("rules.alwaysMoveFrom", &json!({"folder": proj.0})).unwrap();
        assert_eq!(r["moved"], 2);
        assert_eq!(s.mailbox.total_count(proj), 2);
        assert_eq!(s.mailbox.total_count(inbox), 1);
        assert_eq!(s.mailbox.rules[0].actions, vec![RuleAction::MoveTo(proj)]);
    }

    #[test]
    fn export_import_roundtrip() {
        let (mut s, _, proj) = session();
        s.run("rules.create", &json!({"name": "R1", "actions": [{"MoveTo": proj.0}]})).unwrap();
        s.run("rules.create", &json!({"name": "R2", "actions": ["MarkAsRead"], "enabled": false})).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");
        let r = s.run("rules.export", &json!({"path": path.to_str().unwrap()})).unwrap();
        assert_eq!(r["exported"], 2);
        // Import into a mailbox without the Projects folder: R1 comes back disabled.
        let (mut t, _, _) = session();
        t.mailbox.delete_folder(proj).ok();
        t.mailbox.folders.remove(&proj);
        let r = t.run("rules.import", &json!({"path": path.to_str().unwrap()})).unwrap();
        assert_eq!(r["imported"], 2);
        assert_eq!(r["disabled"], 1);
        assert!(!t.mailbox.rules.iter().any(|r| r.enabled));
        // Replace keeps only the imported ones; append adds again with fresh ids.
        s.run("rules.import", &json!({"path": path.to_str().unwrap()})).unwrap();
        assert_eq!(s.mailbox.rules.len(), 4);
        let ids: std::collections::BTreeSet<u64> = s.mailbox.rules.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 4);
        s.run("rules.import", &json!({"path": path.to_str().unwrap(), "replace": true})).unwrap();
        assert_eq!(s.mailbox.rules.len(), 2);
        std::fs::write(&path, b"garbage").unwrap();
        assert!(matches!(
            s.run("rules.import", &json!({"path": path.to_str().unwrap()})),
            Err(CmdError::Failed(_))
        ));
    }

    #[test]
    fn run_now_uses_engine() {
        let (mut s, inbox, proj) = session();
        s.run(
            "rules.create",
            &json!({"name": "Alice", "conditions": [{"From": ["alice@"]}], "actions": [{"MoveTo": proj.0}], "enabled": false}),
        )
        .unwrap();
        let r = s.run("rules.runNow", &json!({"ids": ["Alice"], "folder": inbox.0})).unwrap();
        assert_eq!(r["scanned"], 3);
        // The disabled rule ran because it was named, and stayed disabled afterwards.
        assert!(!s.mailbox.rules[0].enabled);
        assert_eq!(r["applied"], 2);
        assert_eq!(s.mailbox.total_count(proj), 2);
        assert_eq!(s.mailbox.total_count(inbox), 1);
    }

    #[test]
    fn manage_and_new_open_dialogs() {
        let (mut s, _, _) = session();
        assert_eq!(s.run("rules.manage", &json!({})).unwrap()["dialog"], "rulesAndAlerts");
        assert_eq!(s.run("rules.new", &json!({})).unwrap()["dialog"], "rulesWizard");
        s.run("rules.new", &json!({"name": "N", "actions": ["MarkAsRead"]})).unwrap();
        assert_eq!(s.run("rules.edit", &json!({"id": "N"})).unwrap()["dialog"], "rulesWizard");
        s.run("rules.edit", &json!({"id": "N", "conditions": ["HasAttachment"]})).unwrap();
        assert_eq!(s.mailbox.rules[0].conditions, vec![RuleCondition::HasAttachment]);
        assert_eq!(s.run("rules.list", &json!({})).unwrap()["rules"].as_array().unwrap().len(), 1);
    }
}
