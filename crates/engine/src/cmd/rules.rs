//! `rules.*`: CRUD over [`Mailbox::rules`](mailcraft_model::Mailbox) (`plan/reference/rules.md`), the one-click
//! "Always Move Messages From" rule, Create Rule from a message, rule templates (Rules Wizard "Start from a
//! template"), Change Rule quick actions, Copy to other accounts, Run Rules Now, rules for sent and arriving
//! messages, and `.mcrules` export/import (JSON, folders written as paths so files move between mailboxes).
//!
//! The executor is [`exec`] (`src/rules_exec.rs`).
//! Also hosts small helpers shared by the admin command files (dialogs, selection, parsing, files). Changes are
//! journaled by `Session::run` (it diffs the mailbox around every mutating command).

use std::path::Path;

use serde_json::{Value, json};

use crate::cmd::util::{ok, opt_bool, opt_folder, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::rule::{ActionScope, RuleContext};
use mailcraft_model::{FolderId, FolderKind, ItemId, Rule, RuleAction, RuleCondition};

/// The rule executor (`crate::rules_exec`).
pub use crate::rules_exec as exec;

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
            r#"{"ids"?: [rule id], "folder"?: number, "subfolders"?: bool, "which"?: "all"|"read"|"unread"} (no ids = all enabled rules)"#,
            true,
            has_rules,
            run_now,
        ),
        spec(
            "rules.fromTemplate",
            "New Rule from Template",
            "Rules Wizard › Start from a template",
            r#"{"template": "moveFromSender"|"moveWithWords"|"moveSentToGroup"|"flagFromSender"|"moveRss"|"alertFromSender"|"soundFromSender"|"blankReceive"|"blankSend", "people"?: [string], "words"?: [string], "folder"?: number, "sound"?: string, "flag"?: string, "name"?: string}"#,
            true,
            always,
            from_template,
        ),
        spec(
            "rules.addAction",
            "Change Rule",
            "Rules and Alerts › Change Rule",
            r#"{"id": number|string, "action": RuleAction} (replaces an action of the same kind)"#,
            true,
            has_rules,
            add_action,
        ),
        spec(
            "rules.copyToAccounts",
            "Copy Rules To",
            M,
            r#"{"id": number|string, "accounts": [account id|address]} (folders map by path)"#,
            true,
            has_rules,
            copy_to_accounts,
        ),
        spec(
            "rules.applyToNewMail",
            "Run Rules on Arrived Mail",
            "Send/Receive",
            r#"{"ids": [number]} (local delivery: the enabled rules for received messages)"#,
            true,
            always,
            apply_to_new_mail,
        ),
        spec(
            "rules.applyOutgoing",
            "Run Rules on Sent Mail",
            "Compose › Send",
            r#"{"ids": [number]} (Outbox items: the enabled rules for sent messages)"#,
            true,
            always,
            apply_outgoing,
        ),
        spec("rules.export", "Export Rules", M, r#"{"path": string} (.mcrules)"#, false, has_rules, export),
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

/// Folders named by actions must exist and hold mail; send-only actions need a rule for sent messages and
/// receive-only ones a rule for received messages; values stay in range.
fn check_rule(s: &Session, rule: &Rule) -> Result<(), CmdError> {
    let outgoing = rule.is_outgoing();
    for a in &rule.actions {
        match (a.scope(), outgoing) {
            (ActionScope::Outgoing, false) => {
                return Err(CmdError::Params(format!(
                    "\"{}\" works only in a rule for messages you send",
                    a.describe(&|_| String::new())
                )));
            }
            (ActionScope::Incoming, true) => {
                return Err(CmdError::Params(format!(
                    "\"{}\" works only in a rule for messages you receive",
                    a.describe(&|_| String::new())
                )));
            }
            _ => {}
        }
        if let RuleAction::DeferDelivery(n) = a
            && (*n == 0 || *n > exec::MAX_DEFER_MINUTES)
        {
            return Err(CmdError::Params(format!(
                "defer delivery by 1 to {} minutes",
                exec::MAX_DEFER_MINUTES
            )));
        }
    }
    if outgoing && let Some(c) = rule.conditions.iter().find(|c| c.incoming_only()) {
        return Err(CmdError::Params(format!(
            "\"{}\" works only in a rule for messages you receive",
            c.describe()
        )));
    }
    check_actions(s, &rule.actions)
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
        "outgoing": r.is_outgoing(),
        "account": r.account(),
        "alert": r.has_alert(),
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

/// The rules with ids `only` (in list order; disabled ones included when named), or every enabled rule.
fn chosen_rules(s: &Session, only: Option<&[u64]>) -> Vec<Rule> {
    s.mailbox
        .rules
        .iter()
        .filter(|r| match only {
            Some(only) => only.contains(&r.id),
            None => r.enabled,
        })
        .cloned()
        .collect()
}

/// Runs the rules with ids `only` (all enabled rules when `None`) over `items`.
fn run_rules_on(s: &mut Session, only: Option<&[u64]>, items: &[ItemId]) -> usize {
    let rules = chosen_rules(s, only);
    exec::apply_rule_list(s, items, &rules)
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
    let mut rule = Rule {
        id: 0,
        name,
        enabled: opt_bool(p, "enabled").unwrap_or(true),
        conditions,
        actions,
        exceptions,
    };
    if let Some(out) = opt_bool(p, "outgoing") {
        rule.set_outgoing(out);
    }
    if let Some(acct) = opt_str(p, "account") {
        let addr = account_address(s, acct)?;
        rule = rule.for_account(&addr);
    }
    check_rule(s, &rule)?;
    Ok(rule)
}

/// Address of the account named by id, address or display name.
fn account_address(s: &Session, key: &str) -> Result<String, CmdError> {
    let key = key.trim();
    s.mailbox
        .accounts
        .iter()
        .find(|a| {
            a.id.0.to_string() == key
                || (!a.address.is_empty() && a.address.eq_ignore_ascii_case(key))
                || a.display_name.trim().eq_ignore_ascii_case(key)
        })
        .map(|a| {
            if a.address.trim().is_empty() {
                a.display_name.trim().to_owned()
            } else {
                a.address.trim().to_owned()
            }
        })
        .ok_or_else(|| CmdError::Params(format!("no such account {key}")))
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
    let fields = ["name", "conditions", "actions", "exceptions", "enabled", "outgoing"];
    if !fields.iter().any(|k| p.get(*k).is_some()) {
        let rule = s.mailbox.rules.get(i).map(|r| rule_json(s, r)).unwrap_or(Value::Null);
        return dialog(s, "rulesWizard", json!({"rule": rule}));
    }
    let name = opt_str(p, "name").map(clean_name).transpose()?;
    let conditions: Option<Vec<RuleCondition>> = parse_field(p, "conditions")?;
    let actions: Option<Vec<RuleAction>> = parse_field(p, "actions")?;
    let exceptions: Option<Vec<RuleCondition>> = parse_field(p, "exceptions")?;
    if actions.as_ref().is_some_and(Vec::is_empty) {
        return Err(CmdError::Params("a rule needs at least one action".into()));
    }
    let Some(mut r) = s.mailbox.rules.get(i).cloned() else {
        return Err(CmdError::Params("no such rule".into()));
    };
    let was_outgoing = r.is_outgoing();
    if let Some(n) = name {
        r.name = n;
    }
    if let Some(c) = conditions {
        // Conditions sent without the marker keep the rule's direction; with it, the rule applies to sent
        // messages. `outgoing` overrides both.
        let outgoing = was_outgoing || c.contains(&RuleCondition::AfterSending);
        r.conditions = c;
        r.set_outgoing(outgoing);
    }
    if let Some(out) = opt_bool(p, "outgoing") {
        r.set_outgoing(out);
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
    check_rule(s, &r)?;
    let id = r.id;
    if let Some(slot) = s.mailbox.rules.get_mut(i) {
        *slot = r;
    }
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
    let which = opt_str(p, "which").unwrap_or("all");
    let Some(which) = exec::Which::parse(which) else {
        return Err(CmdError::Params(format!("`which` must be all, read or unread, not {which}")));
    };
    let rules = chosen_rules(s, only.as_deref());
    let (scanned, report) =
        exec::run_now(s, &rules, folder, opt_bool(p, "subfolders").unwrap_or(false), which);
    Ok(json!({"applied": report.matched, "scanned": scanned, "report": report.to_json()}))
}

/// File extension of exported rules.
pub const RULES_EXTENSION: &str = "mcrules";

/// Folder ids named by an action list.
fn action_folders(actions: &[RuleAction]) -> Vec<FolderId> {
    actions
        .iter()
        .filter_map(|a| match a {
            RuleAction::MoveTo(f) | RuleAction::CopyTo(f) => Some(*f),
            _ => None,
        })
        .collect()
}

/// Rewrites the folder ids of an action list through `map`; false when one has no mapping.
fn remap_actions(actions: &mut [RuleAction], map: &dyn Fn(FolderId) -> Option<FolderId>) -> bool {
    let mut all = true;
    for a in actions.iter_mut() {
        if let RuleAction::MoveTo(f) | RuleAction::CopyTo(f) = a {
            match map(*f) {
                Some(n) => *f = n,
                None => all = false,
            }
        }
    }
    all
}

/// Mail folder of `account` (any account when `None`) whose path is `path`.
fn folder_by_path(s: &Session, account: Option<mailcraft_model::AccountId>, path: &str) -> Option<FolderId> {
    let want = path.trim().trim_matches('\\').to_lowercase();
    let mut cands: Vec<&mailcraft_model::Folder> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Mail && s.mailbox.folder_path(f.id).to_lowercase() == want)
        .collect();
    if let Some(a) = account {
        cands.retain(|f| f.account == a);
    } else if let Some(cur) = s.current_account() {
        cands.sort_by_key(|f| f.account != cur);
    }
    cands.first().map(|f| f.id)
}

fn export(s: &mut Session, p: &Value) -> CmdResult {
    let mut path = std::path::PathBuf::from(req_str(p, "path")?.trim());
    if path.extension().is_none() {
        path.set_extension(RULES_EXTENSION);
    }
    let mut folders = serde_json::Map::new();
    for r in &s.mailbox.rules {
        for f in action_folders(&r.actions) {
            if s.mailbox.folder(f).is_some() {
                folders.insert(f.0.to_string(), json!(s.mailbox.folder_path(f)));
            }
        }
    }
    let doc = json!({"mailcraft_rules": 2, "format": RULES_EXTENSION, "rules": s.mailbox.rules, "folders": folders});
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| CmdError::Failed(e.to_string()))?;
    write_file(&path, &bytes)?;
    Ok(json!({"exported": s.mailbox.rules.len(), "path": path.display().to_string()}))
}

fn import(s: &mut Session, p: &Value) -> CmdResult {
    let mut path = std::path::PathBuf::from(req_str(p, "path")?.trim());
    // "my-rules" finds "my-rules.mcrules", as export names it.
    if path.extension().is_none() && !path.exists() {
        path.set_extension(RULES_EXTENSION);
    }
    let bytes = read_file_capped(&path, MAX_IMPORT_BYTES)?;
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| CmdError::Failed(format!("not a rules file: {e}")))?;
    let (list, folders) = match &doc {
        Value::Array(_) => (doc.clone(), serde_json::Map::new()),
        Value::Object(o) => (
            o.get("rules").cloned().ok_or_else(|| CmdError::Failed("not a rules file".into()))?,
            o.get("folders").and_then(Value::as_object).cloned().unwrap_or_default(),
        ),
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
        // Folders travel as paths; ids from the file are used only when the file has no path for them.
        let mapped =
            remap_actions(&mut r.actions, &|f| match folders.get(&f.0.to_string()).and_then(Value::as_str) {
                Some(path) => folder_by_path(s, None, path),
                None => s.mailbox.folder(f).map(|_| f),
            });
        // A rule pointing at a folder this mailbox does not have, or otherwise invalid here, is kept but off.
        if r.enabled && (!mapped || check_rule(s, &r).is_err()) {
            r.enabled = false;
            disabled = disabled.saturating_add(1);
        }
        r.id = next_rule_id(s)?;
        s.mailbox.rules.push(r);
        imported = imported.saturating_add(1);
    }
    Ok(json!({"imported": imported, "disabled": disabled}))
}

/// Rules Wizard templates: (key, label, group).
pub const TEMPLATES: [(&str, &str, &str); 10] = [
    ("moveFromSender", "Move messages from someone to a folder", "Stay Organized"),
    ("moveWithWords", "Move messages with specific words in the subject to a folder", "Stay Organized"),
    ("moveSentToGroup", "Move messages sent to a public group to a folder", "Stay Organized"),
    ("flagFromSender", "Flag messages from someone for follow-up", "Stay Organized"),
    ("moveRss", "Move RSS items from a specific RSS Feed to a folder", "Stay Organized"),
    ("alertFromSender", "Display mail from someone in the New Item Alert window", "Stay Up to Date"),
    ("soundFromSender", "Play a sound when I get messages from someone", "Stay Up to Date"),
    ("mobileAlert", "Send an alert to my mobile device when I get messages from someone", "Stay Up to Date"),
    ("blankReceive", "Apply rule on messages I receive", "Start from a blank rule"),
    ("blankSend", "Apply rule on messages I send", "Start from a blank rule"),
];

fn str_list(p: &Value, key: &str) -> Vec<String> {
    match p.get(key) {
        Some(Value::String(t)) => {
            t.split([';', ',']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
        }
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The conditions and actions a template starts with, from the given values.
fn template_rule(s: &Session, key: &str, p: &Value) -> Result<Rule, CmdError> {
    let people = str_list(p, "people");
    let words = str_list(p, "words");
    let need = |what: &str, ok: bool| -> Result<(), CmdError> {
        if ok { Ok(()) } else { Err(CmdError::Params(format!("this template needs `{what}`"))) }
    };
    let folder = || opt_folder(p).ok_or_else(|| CmdError::Params("this template needs `folder`".into()));
    let mut rule = Rule { enabled: true, ..Default::default() };
    match key {
        "moveFromSender" => {
            need("people", !people.is_empty())?;
            rule.conditions = vec![RuleCondition::From(people)];
            rule.actions = vec![RuleAction::MoveTo(folder()?)];
        }
        "moveWithWords" => {
            need("words", !words.is_empty())?;
            rule.conditions = vec![RuleCondition::SubjectContains(words)];
            rule.actions = vec![RuleAction::MoveTo(folder()?)];
        }
        "moveSentToGroup" => {
            need("people", !people.is_empty())?;
            rule.conditions = vec![RuleCondition::SentTo(people)];
            rule.actions = vec![RuleAction::MoveTo(folder()?)];
        }
        "flagFromSender" => {
            need("people", !people.is_empty())?;
            let label = opt_str(p, "flag").map(str::trim).filter(|l| !l.is_empty()).unwrap_or("Follow up");
            rule.conditions = vec![RuleCondition::From(people)];
            rule.actions = vec![RuleAction::FlagForFollowUp(label.to_owned(), 0)];
        }
        "moveRss" => {
            rule.conditions = vec![if words.is_empty() {
                RuleCondition::FromAnyRssFeed
            } else {
                RuleCondition::FromRssFeedWithText(words)
            }];
            rule.actions = vec![RuleAction::MoveTo(folder()?)];
        }
        "alertFromSender" => {
            need("people", !people.is_empty())?;
            rule.conditions = vec![RuleCondition::From(people)];
            rule.actions = vec![RuleAction::NewItemAlert(opt_str(p, "text").unwrap_or("").trim().to_owned())];
        }
        "soundFromSender" => {
            need("people", !people.is_empty())?;
            let sound = opt_str(p, "sound").map(str::trim).unwrap_or("");
            need("sound", !sound.is_empty())?;
            rule.conditions = vec![RuleCondition::From(people)];
            rule.actions = vec![RuleAction::PlaySound(sound.to_owned())];
        }
        "mobileAlert" => {
            return Err(CmdError::Disabled(
                "alerts to a mobile device need a text messaging service, which MailCraft does not have"
                    .into(),
            ));
        }
        "blankReceive" | "blankSend" => {
            rule.conditions = parse_field(p, "conditions")?.unwrap_or_default();
            rule.actions = parse_field(p, "actions")?.unwrap_or_default();
            rule.exceptions = parse_field(p, "exceptions")?.unwrap_or_default();
            rule.set_outgoing(key == "blankSend");
            if rule.actions.is_empty() {
                return Err(CmdError::Params("a rule needs at least one action".into()));
            }
        }
        other => return Err(CmdError::Params(format!("unknown template `{other}`"))),
    }
    check_rule(s, &rule)?;
    Ok(rule)
}

fn from_template(s: &mut Session, p: &Value) -> CmdResult {
    let key = req_str(p, "template")?.trim().to_owned();
    let mut rule = template_rule(s, &key, p)?;
    let name_of =
        |f: FolderId| s.mailbox.folder(f).map(|x| x.name.clone()).unwrap_or_else(|| "specified".into());
    let default_name = rule
        .conditions
        .iter()
        .find(|c| **c != RuleCondition::AfterSending)
        .map(RuleCondition::describe)
        .or_else(|| rule.actions.first().map(|a| a.describe(&name_of)))
        .unwrap_or_else(|| "New rule".into());
    rule.name = match opt_str(p, "name") {
        Some(n) => clean_name(n)?,
        None => clean_name(&default_name).unwrap_or_else(|_| "New rule".into()),
    };
    finish_create(s, rule, p)
}

/// Change Rule quick actions: adds `action`, replacing one of the same kind.
fn add_action(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    let Some(action) = parse_field::<RuleAction>(p, "action")? else {
        return Err(CmdError::Params("`action` (RuleAction) required".into()));
    };
    let Some(mut r) = s.mailbox.rules.get(i).cloned() else {
        return Err(CmdError::Params("no such rule".into()));
    };
    r.actions.retain(|a| std::mem::discriminant(a) != std::mem::discriminant(&action));
    // Stop Processing More Rules stays last.
    let at =
        r.actions.iter().position(|a| *a == RuleAction::StopProcessingMoreRules).unwrap_or(r.actions.len());
    r.actions.insert(at, action);
    check_rule(s, &r)?;
    let id = r.id;
    if let Some(slot) = s.mailbox.rules.get_mut(i) {
        *slot = r;
    }
    Ok(json!({"id": id}))
}

/// Copy… to other accounts: one copy per account, limited to it, with its folders mapped by path (a copy
/// whose folder the account lacks is turned off).
fn copy_to_accounts(s: &mut Session, p: &Value) -> CmdResult {
    let i = find_rule(s, p)?;
    let Some(rule) = s.mailbox.rules.get(i).cloned() else {
        return Err(CmdError::Params("no such rule".into()));
    };
    let targets = match p.get("accounts") {
        Some(Value::Array(a)) if !a.is_empty() => a.clone(),
        _ => return Err(CmdError::Params("`accounts` (non-empty array) required".into())),
    };
    let mut created = Vec::new();
    let mut disabled = 0usize;
    for t in targets {
        let key = match &t {
            Value::Number(n) => n.to_string(),
            Value::String(x) => x.clone(),
            _ => return Err(CmdError::Params(format!("not an account: {t}"))),
        };
        let addr = account_address(s, &key)?;
        let Some(account) = s
            .mailbox
            .accounts
            .iter()
            .find(|a| a.address.trim().eq_ignore_ascii_case(&addr) || a.display_name.trim() == addr)
            .map(|a| a.id)
        else {
            return Err(CmdError::Params(format!("no such account {key}")));
        };
        if s.mailbox.rules.len() >= MAX_RULES {
            return Err(CmdError::Failed(format!("at most {MAX_RULES} rules")));
        }
        let mut copy = rule.for_account(&addr);
        let paths: Vec<(FolderId, String)> =
            action_folders(&copy.actions).into_iter().map(|f| (f, s.mailbox.folder_path(f))).collect();
        let mapped = remap_actions(&mut copy.actions, &|f| {
            let path = paths.iter().find(|(id, _)| *id == f).map(|(_, p)| p.as_str())?;
            folder_by_path(s, Some(account), path)
        });
        if !mapped || check_rule(s, &copy).is_err() {
            copy.enabled = false;
            disabled = disabled.saturating_add(1);
        }
        let base = rule.name.trim();
        copy.name = clean_name(&format!("{base} ({addr})")).unwrap_or_else(|_| base.to_owned());
        created.push(insert_rule(s, copy)?);
    }
    Ok(json!({"created": created, "disabled": disabled}))
}

fn ids_param(s: &Session, p: &Value) -> Vec<ItemId> {
    crate::cmd::util::opt_ids(p)
        .unwrap_or_default()
        .into_iter()
        .filter(|id| s.mailbox.mail.contains_key(id))
        .collect()
}

fn apply_to_new_mail(s: &mut Session, p: &Value) -> CmdResult {
    let ids = ids_param(s, p);
    let report = exec::on_new_mail(s, &ids);
    Ok(report.to_json())
}

fn apply_outgoing(s: &mut Session, p: &Value) -> CmdResult {
    let ids = ids_param(s, p);
    let report = exec::on_send(s, &ids);
    Ok(report.to_json())
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

    #[test]
    fn templates_create_rules() {
        let (mut s, inbox, proj) = session();
        let r = s
            .run(
                "rules.fromTemplate",
                &json!({"template": "moveFromSender", "people": "alice@example.com", "folder": proj.0}),
            )
            .unwrap();
        assert_eq!(r["rule"]["name"], "from alice@example.com");
        assert_eq!(s.mailbox.rules[0].actions, vec![RuleAction::MoveTo(proj)]);
        s.run(
            "rules.fromTemplate",
            &json!({"template": "flagFromSender", "people": ["bob@example.org"], "flag": "Reply"}),
        )
        .unwrap();
        assert_eq!(s.mailbox.rules[1].actions, vec![RuleAction::FlagForFollowUp("Reply".into(), 0)]);
        s.run("rules.fromTemplate", &json!({"template": "moveRss", "folder": proj.0})).unwrap();
        assert_eq!(s.mailbox.rules[2].conditions, vec![RuleCondition::FromAnyRssFeed]);
        s.run("rules.fromTemplate", &json!({"template": "alertFromSender", "people": "x@example.org"}))
            .unwrap();
        assert!(s.mailbox.rules[3].is_client_only());
        let r = s
            .run(
                "rules.fromTemplate",
                &json!({"template": "blankSend", "name": "Hold", "actions": [{"DeferDelivery": 5}]}),
            )
            .unwrap();
        assert_eq!(r["rule"]["outgoing"], true);
        assert_eq!(r["rule"]["description"][0], "Apply this rule after I send the message");
        // Values each template needs, and the one that is not available here.
        for p in [
            json!({"template": "moveFromSender", "folder": proj.0}),
            json!({"template": "moveWithWords", "words": "x"}),
            json!({"template": "soundFromSender", "people": "a@example.org"}),
            json!({"template": "blankReceive"}),
            json!({"template": "nope"}),
        ] {
            assert!(matches!(s.run("rules.fromTemplate", &p), Err(CmdError::Params(_))), "{p}");
        }
        assert!(matches!(
            s.run("rules.fromTemplate", &json!({"template": "mobileAlert", "people": "a@example.org"})),
            Err(CmdError::Disabled(_))
        ));
        assert_eq!(s.mailbox.total_count(inbox), 3);
    }

    #[test]
    fn direction_rules_validate_actions_and_conditions() {
        let (mut s, _, proj) = session();
        // Send-only actions need a rule for sent messages and the other way round.
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "actions": [{"DeferDelivery": 5}]})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "outgoing": true, "actions": [{"NewItemAlert": ""}]})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "outgoing": true, "conditions": ["SentOnlyToMe"], "actions": ["NotifyWhenRead"]})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("rules.create", &json!({"name": "x", "outgoing": true, "actions": [{"DeferDelivery": 0}]})),
            Err(CmdError::Params(_))
        ));
        let r = s
            .run("rules.create", &json!({"name": "Hold", "outgoing": true, "actions": [{"DeferDelivery": 30}, {"CopyTo": proj.0}]}))
            .unwrap();
        let id = r["id"].as_u64().unwrap();
        assert!(s.mailbox.rules[0].is_outgoing());
        // Editing the conditions keeps the direction.
        s.run("rules.edit", &json!({"id": id, "conditions": [{"SubjectContains": ["x"]}]})).unwrap();
        assert!(s.mailbox.rules[0].is_outgoing());
        assert!(matches!(
            s.run("rules.edit", &json!({"id": id, "outgoing": false})),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.rules[0].is_outgoing(), "a refused edit changes nothing");
        // Conditions that carry the "after I send" marker make a receive rule a send rule.
        let r = s.run("rules.create", &json!({"name": "In", "actions": [{"CopyTo": proj.0}]})).unwrap();
        let rid = r["id"].as_u64().unwrap();
        s.run("rules.edit", &json!({"id": rid, "conditions": ["AfterSending", {"SubjectContains": ["y"]}]}))
            .unwrap();
        let edited = s.mailbox.rules.iter().find(|r| r.id == rid).unwrap();
        assert!(edited.is_outgoing(), "{:?}", edited.conditions);
        assert_eq!(edited.conditions.iter().filter(|c| **c == RuleCondition::AfterSending).count(), 1);
        assert!(matches!(
            s.run(
                "rules.create",
                &json!({"name": "x", "account": "nobody@example.org", "actions": ["MarkAsRead"]})
            ),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn change_rule_quick_actions_replace_the_same_kind() {
        let (mut s, _, proj) = session();
        let r = s
            .run(
                "rules.create",
                &json!({"name": "A", "actions": [{"MarkImportance": "Low"}, "StopProcessingMoreRules"]}),
            )
            .unwrap();
        let id = r["id"].as_u64().unwrap();
        s.run("rules.addAction", &json!({"id": id, "action": {"MarkImportance": "High"}})).unwrap();
        s.run("rules.addAction", &json!({"id": id, "action": {"MoveTo": proj.0}})).unwrap();
        assert_eq!(
            s.mailbox.rules[0].actions,
            vec![
                RuleAction::MarkImportance(mailcraft_model::Importance::High),
                RuleAction::MoveTo(proj),
                RuleAction::StopProcessingMoreRules
            ]
        );
        assert!(s.run("rules.addAction", &json!({"id": id})).is_err());
        assert!(s.run("rules.addAction", &json!({"id": id, "action": {"MoveTo": 999_999}})).is_err());
        assert!(s.run("rules.addAction", &json!({"id": id, "action": {"DeferDelivery": 3}})).is_err());
    }

    #[test]
    fn copy_to_other_accounts_maps_folders_by_path() {
        let (mut s, _, proj) = session();
        let other = s.mailbox.add_account(mailcraft_model::Account {
            kind: mailcraft_model::AccountKind::Imap,
            display_name: "Work".into(),
            address: "work@example.com".into(),
            ..Default::default()
        });
        s.mailbox.create_default_folders(other);
        let r = s
            .run("rules.create", &json!({"name": "Budget", "conditions": [{"SubjectContains": ["budget"]}], "actions": [{"MoveTo": proj.0}]}))
            .unwrap();
        let id = r["id"].as_u64().unwrap();
        // The other account has no Inbox\Projects: the copy is kept but off.
        let c = s.run("rules.copyToAccounts", &json!({"id": id, "accounts": ["work@example.com"]})).unwrap();
        assert_eq!(c["disabled"], 1);
        let copy = s.mailbox.rules.last().unwrap();
        assert_eq!(copy.account(), Some("work@example.com"));
        assert!(!copy.enabled);
        assert_eq!(copy.name, "Budget (work@example.com)");
        let work_inbox = s.mailbox.role_folder(other, mailcraft_model::FolderRole::Inbox).unwrap();
        let work_proj = s.mailbox.create_folder(work_inbox, "Projects").unwrap();
        let c = s.run("rules.copyToAccounts", &json!({"id": id, "accounts": [other.0]})).unwrap();
        assert_eq!(c["disabled"], 0);
        let copy = s.mailbox.rules.last().unwrap();
        assert!(copy.enabled);
        assert_eq!(copy.actions, vec![RuleAction::MoveTo(work_proj)]);
        assert!(s.run("rules.copyToAccounts", &json!({"id": id, "accounts": []})).is_err());
        assert!(s.run("rules.copyToAccounts", &json!({"id": id, "accounts": ["x@example.org"]})).is_err());
    }

    #[test]
    fn mcrules_export_maps_folders_by_path() {
        let (mut s, _, proj) = session();
        s.run("rules.create", &json!({"name": "R1", "actions": [{"MoveTo": proj.0}]})).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let r = s.run("rules.export", &json!({"path": dir.path().join("mine").to_str().unwrap()})).unwrap();
        let path = r["path"].as_str().unwrap().to_owned();
        assert!(path.ends_with("mine.mcrules"), "{path}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Inbox\\\\Projects"), "{text}");
        // Another mailbox whose Projects folder has a different id gets it remapped.
        let mut t = Session::new(mailcraft_model::Mailbox::new_local("Other"));
        let acct = t.mailbox.accounts[0].id;
        let inbox = t.mailbox.role_folder(acct, mailcraft_model::FolderRole::Inbox).unwrap();
        t.mailbox.create_folder(inbox, "Filler").unwrap();
        let tproj = t.mailbox.create_folder(inbox, "Projects").unwrap();
        assert_ne!(tproj, proj);
        let r = t.run("rules.import", &json!({"path": path})).unwrap();
        assert_eq!(r["disabled"], 0);
        assert_eq!(t.mailbox.rules[0].actions, vec![RuleAction::MoveTo(tproj)]);
    }

    #[test]
    fn apply_commands_run_the_right_direction() {
        let (mut s, inbox, _) = session();
        s.run("rules.create", &json!({"name": "In", "actions": [{"AssignCategory": "Red category"}]}))
            .unwrap();
        let ids: Vec<u64> = s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        let r = s.run("rules.applyToNewMail", &json!({"ids": ids})).unwrap();
        assert_eq!(r["matched"], 3);
        let r = s.run("rules.applyOutgoing", &json!({"ids": ids})).unwrap();
        assert_eq!(r["matched"], 0, "not in an Outbox");
        let r = s.run("rules.applyToNewMail", &json!({"ids": [u64::MAX]})).unwrap();
        assert_eq!(r["matched"], 0);
    }

    #[test]
    fn run_now_which() {
        let (mut s, inbox, _) = session();
        s.run("rules.create", &json!({"name": "All", "actions": [{"AssignCategory": "Red category"}]}))
            .unwrap();
        let first = s.mailbox.items_in(inbox)[0].id;
        s.mailbox.set_read(&[first], true);
        let r = s.run("rules.runNow", &json!({"folder": inbox.0, "which": "read"})).unwrap();
        assert_eq!((r["scanned"].as_u64(), r["applied"].as_u64()), (Some(1), Some(1)));
        assert!(matches!(s.run("rules.runNow", &json!({"which": "some"})), Err(CmdError::Params(_))));
    }
}
