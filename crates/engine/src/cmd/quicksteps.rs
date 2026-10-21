//! `quicksteps.*`: run a Quick Step's action list over the selection, and manage the steps (new, edit, delete,
//! duplicate, reorder, reset to defaults).
//!
//! A step whose folder or recipients are not chosen yet ("Move to: ?", "To Manager") asks for them on first use
//! (`quickStepFirstUse`); running it again with `folder` / `to` (and optionally a new `name`) completes the
//! setup. `Flag` actions take the Follow Up menu's keys ("today", "tomorrow", "thisWeek", "nextWeek",
//! "noDate"), which set the flag's dates; any other text is the flag's label.

use serde_json::{Value, json};

use crate::cmd::rules::{clean_name, dialog, parse_field};
use crate::cmd::util::{opt_folder, opt_str};
use crate::compose::{ComposeKind, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::subject::{forward_subject, reply_subject};
use mailcraft_model::{FlagState, FolderKind, ItemId, QuickStep, QuickStepAction};

/// Most quick steps kept.
const MAX_STEPS: usize = 200;

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
    const Q: &str = "Home › Quick Steps";
    const M: &str = "Manage Quick Steps";
    vec![
        spec(
            "quicksteps.run",
            "Run Quick Step",
            Q,
            r#"{"id": number|string, "ids"?: [number], "group"?: bool, "folder"?: number, "to"?: [string], "name"?: string} (folder/to/name complete a first-use setup; group = the items of a list group)"#,
            true,
            has_steps,
            run,
        ),
        spec(
            "quicksteps.new",
            "New Quick Step",
            "Home › Quick Steps › New Quick Step",
            r#"{"template"?: "moveToFolder"|"categorizeMove"|"flagMove"|"newEmailTo"|"forwardTo"|"newMeeting"|"custom", "name"?: string, "actions"?: [QuickStepAction], "shortcut"?: string, "tooltip"?: string} (no name = open the editor)"#,
            true,
            always,
            new_step,
        ),
        spec("quicksteps.manage", "Manage Quick Steps", Q, "", false, always, manage),
        spec("quicksteps.list", "List Quick Steps", M, "", false, always, list),
        spec(
            "quicksteps.edit",
            "Edit",
            M,
            r#"{"id": number|string, "name"?: string, "actions"?: [QuickStepAction], "shortcut"?: string, "tooltip"?: string}"#,
            true,
            has_steps,
            edit,
        ),
        spec("quicksteps.delete", "Delete", M, r#"{"id": number|string}"#, true, has_steps, delete),
        spec("quicksteps.duplicate", "Duplicate", M, r#"{"id": number|string}"#, true, has_steps, duplicate),
        spec("quicksteps.moveUp", "Move Up", M, r#"{"id": number|string}"#, true, has_steps, move_up),
        spec("quicksteps.moveDown", "Move Down", M, r#"{"id": number|string}"#, true, has_steps, move_down),
        spec("quicksteps.reset", "Reset to Defaults", M, "", true, always, reset),
    ]
}

fn has_steps(s: &Session) -> Option<&'static str> {
    if s.mailbox.quick_steps.is_empty() { Some("no quick steps") } else { None }
}

/// Index of the step named by `p.id` (number, numeric string, or step name).
fn find_step(s: &Session, p: &Value) -> Result<usize, CmdError> {
    let v = p.get("id").ok_or_else(|| CmdError::Params("`id` (quick step id or name) required".into()))?;
    let steps = &s.mailbox.quick_steps;
    let pos = match v {
        Value::Number(n) => n.as_u64().and_then(|id| steps.iter().position(|q| q.id == id)),
        Value::String(name) => {
            let key = name.trim();
            key.parse::<u64>()
                .ok()
                .and_then(|id| steps.iter().position(|q| q.id == id))
                .or_else(|| steps.iter().position(|q| q.name.trim().eq_ignore_ascii_case(key)))
        }
        _ => None,
    };
    pos.ok_or_else(|| CmdError::Params(format!("no such quick step {v}")))
}

fn next_step_id(s: &Session) -> Result<u64, CmdError> {
    match s.mailbox.quick_steps.iter().map(|q| q.id).max() {
        None => Ok(1),
        Some(m) => m.checked_add(1).ok_or_else(|| CmdError::Failed("quick step ids exhausted".into())),
    }
}

/// "Ctrl+Shift+1" … "Ctrl+Shift+9" (also accepts "Mod+Shift+n"); empty clears.
fn parse_shortcut(v: &str) -> Result<String, CmdError> {
    let t = v.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let lower = t.to_ascii_lowercase();
    let digit = lower.strip_prefix("ctrl+shift+").or_else(|| lower.strip_prefix("mod+shift+"));
    match digit {
        Some(d) if d.len() == 1 && d.chars().all(|c| ('1'..='9').contains(&c)) => {
            Ok(format!("Ctrl+Shift+{d}"))
        }
        _ => Err(CmdError::Params(format!("shortcut must be Ctrl+Shift+1 … Ctrl+Shift+9, got `{t}`"))),
    }
}

/// Assigns `shortcut` to step `idx`, taking it away from any other step.
fn set_shortcut(s: &mut Session, idx: usize, shortcut: String) {
    if !shortcut.is_empty() {
        for (i, q) in s.mailbox.quick_steps.iter_mut().enumerate() {
            if i != idx && q.shortcut == shortcut {
                q.shortcut.clear();
            }
        }
    }
    if let Some(q) = s.mailbox.quick_steps.get_mut(idx) {
        q.shortcut = shortcut;
    }
}

fn check_actions(s: &Session, actions: &[QuickStepAction]) -> Result<(), CmdError> {
    for a in actions {
        if let QuickStepAction::MoveTo(Some(f)) | QuickStepAction::CopyTo(Some(f)) = a {
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

fn step_json(q: &QuickStep) -> Value {
    json!({
        "id": q.id,
        "name": q.name,
        "actions": q.actions,
        "shortcut": q.shortcut,
        "tooltip": q.tooltip,
        "needsSetup": q.needs_setup(),
    })
}

/// `p.to` as a list of addresses (array of strings, or one "a; b" string).
fn opt_recipients(p: &Value) -> Option<Vec<String>> {
    let list: Vec<String> = match p.get("to")? {
        Value::String(t) => {
            t.split([';', ',']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
        }
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => return None,
    };
    (!list.is_empty()).then_some(list)
}

/// Fills unset folders / recipients of a step from `p.folder` / `p.to` (the first-use setup). Returns true
/// when anything was filled.
fn complete_setup(step: &mut QuickStep, p: &Value) -> bool {
    let folder = opt_folder(p);
    let to = opt_recipients(p);
    let mut changed = false;
    for a in &mut step.actions {
        match a {
            QuickStepAction::MoveTo(f @ None) | QuickStepAction::CopyTo(f @ None) => {
                if let Some(folder) = folder {
                    *f = Some(folder);
                    changed = true;
                }
            }
            QuickStepAction::Forward(list) | QuickStepAction::NewMessageTo(list) if list.is_empty() => {
                if let Some(to) = &to {
                    *list = to.clone();
                    changed = true;
                }
            }
            _ => {}
        }
    }
    changed
}

/// Why step `step` cannot run on `count` items (`None` = it can). A step that replies works on one message
/// at a time (it would otherwise open a reply per message and then act on all of them), and a step that
/// forwards is unavailable on a list group's header; a plain multi-selection is forwarded as one message
/// with the messages attached. The gallery and the context menus grey such steps out.
pub fn blocked_for(step: &QuickStep, count: usize, group: bool) -> Option<&'static str> {
    let replies =
        step.actions.iter().any(|a| matches!(a, QuickStepAction::Reply | QuickStepAction::ReplyAll));
    let forwards = step.actions.iter().any(|a| matches!(a, QuickStepAction::Forward(_)));
    if replies && (count > 1 || group) {
        return Some("this quick step replies, so it works on one message at a time");
    }
    if forwards && group {
        return Some("this quick step forwards, so it works on selected messages, not a group");
    }
    // Refused before anything runs, so the step's other actions never act on messages it did not forward.
    if forwards && count > crate::cmd::respond::MAX_FORWARD_ITEMS {
        return Some("this quick step forwards, so it works on at most 100 messages at a time");
    }
    None
}

fn needs_items(a: &QuickStepAction) -> bool {
    !matches!(a, QuickStepAction::NewMessageTo(_) | QuickStepAction::NewMeeting)
}

/// Runs another command's implementation as part of this quick step. Unlike [`Session::run`] it skips the
/// command's enabled check (which looks at the selection, while the step may act on explicit `ids`) and
/// records no undo entry or journal ops of its own: the quick step is one undoable command and the
/// enclosing `Session::run` journals its whole effect.
fn invoke(s: &mut Session, id: &str, params: &Value) -> CmdResult {
    let Some(run) = s.registry.get(id).map(|spec| spec.run) else {
        return Err(CmdError::Unknown(id.to_owned()));
    };
    run(s, params)
}

/// Opens a reply/forward for `id` through the mail commands (falls back to a plain compose state when that
/// command is not registered). Returns the compose id opened, if any.
fn respond(
    s: &mut Session,
    cmd: &str,
    kind: ComposeKind,
    ids: &[ItemId],
    to: &[String],
) -> Result<Option<u64>, CmdError> {
    let Some(id) = ids.first().copied() else {
        return Ok(None);
    };
    let before = s.next_compose;
    let list: Vec<u64> = ids.iter().map(|i| i.0).collect();
    match invoke(s, cmd, &json!({"ids": list})) {
        Ok(_) => {}
        Err(CmdError::Unknown(_)) => {
            let Some(m) = s.mailbox.mail.get(&id) else {
                return Ok(None);
            };
            let sender =
                m.reply_to.first().or(m.from.as_ref()).map(|a| a.to_display_string()).unwrap_or_default();
            let (subject, to_line) = match kind {
                ComposeKind::Forward => (forward_subject(&m.subject), String::new()),
                _ => (reply_subject(&m.subject), sender),
            };
            let mut c = ComposeState { kind, source: Some(id), to: to_line, subject, ..Default::default() };
            // Options › Mail › Signatures: the replies/forwards signature, as the mail commands add it.
            crate::compose::insert_default_signature(&s.mailbox, &mut c);
            s.open_compose(c);
        }
        Err(e) => return Err(e),
    }
    if s.next_compose == before {
        return Ok(None);
    }
    let cid = s.next_compose;
    if !to.is_empty()
        && let Some(c) = s.composes.get_mut(&cid)
    {
        let extra = to.join("; ");
        c.to = if c.to.trim().is_empty() { extra } else { format!("{}; {extra}", c.to) };
    }
    Ok(Some(cid))
}

fn run(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find_step(s, p)?;
    let Some(mut step) = s.mailbox.quick_steps.get(idx).cloned() else {
        return Err(CmdError::Params("no such quick step".into()));
    };
    if step.actions.is_empty() {
        // An empty step ("Create New") stays in the gallery: it opens the editor on a new step.
        let (name, actions) = template(s, "custom")?;
        return dialog(s, "editQuickStep", json!({"template": "custom", "name": name, "actions": actions}));
    }
    if step.needs_setup() && complete_setup(&mut step, p) {
        check_actions(s, &step.actions)?;
        if let Some(n) = opt_str(p, "name") {
            step.name = clean_name(n)?;
        }
        if !step.needs_setup()
            && let Some(q) = s.mailbox.quick_steps.get_mut(idx)
        {
            *q = step.clone();
        }
    }
    if step.needs_setup() {
        // The items it was run on go with the setup, so the step acts on them once it is set up (a read
        // window's message, a group's items), not on whatever the list selects by then.
        let mut params = json!({"step": step_json(&step)});
        for key in ["ids", "group"] {
            if let (Some(v), Some(o)) = (p.get(key).filter(|v| !v.is_null()), params.as_object_mut()) {
                o.insert(key.to_owned(), v.clone());
            }
        }
        return dialog(s, "quickStepFirstUse", params);
    }
    let ids: Vec<ItemId> = s.target_ids(p).into_iter().filter(|id| s.mailbox.mail.contains_key(id)).collect();
    if ids.is_empty() && step.actions.iter().any(needs_items) {
        return Err(CmdError::Disabled("no message selected".into()));
    }
    let group = p.get("group").and_then(Value::as_bool).unwrap_or(false);
    if let Some(why) = blocked_for(&step, ids.len(), group) {
        return Err(CmdError::Disabled(why.into()));
    }
    let mut touched: Vec<ItemId> = Vec::new();
    let mut composes: Vec<u64> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let now = s.now();
    for action in &step.actions {
        let live: Vec<ItemId> = ids.iter().copied().filter(|id| s.mailbox.mail.contains_key(id)).collect();
        match action {
            QuickStepAction::MoveTo(Some(f)) => match s.mailbox.move_items(&live, *f) {
                Ok(moved) => touched.extend(moved),
                Err(e) => errors.push(e.to_string()),
            },
            QuickStepAction::CopyTo(Some(f)) => {
                if s.mailbox.folder(*f).is_some_and(|x| x.kind == FolderKind::Mail) {
                    for id in &live {
                        if let Some(mut copy) = s.mailbox.mail.get(id).cloned() {
                            copy.folder = *f;
                            copy.remote_id = None;
                            touched.push(s.mailbox.add_mail(copy));
                        }
                    }
                } else {
                    errors.push(format!("no such mail folder {}", f.0));
                }
            }
            QuickStepAction::MoveTo(None) | QuickStepAction::CopyTo(None) => {}
            QuickStepAction::Delete => match s.mailbox.delete_items(&live) {
                Ok(_) => touched.extend(live.iter().copied()),
                Err(e) => errors.push(e.to_string()),
            },
            QuickStepAction::PermanentlyDelete => {
                for id in &live {
                    s.mailbox.mail.remove(id);
                    touched.push(*id);
                }
            }
            QuickStepAction::MarkRead => touched.extend(s.mailbox.set_read(&live, true)),
            QuickStepAction::MarkUnread => touched.extend(s.mailbox.set_read(&live, false)),
            QuickStepAction::SetImportance(i) => {
                for id in &live {
                    if let Some(m) = s.mailbox.mail.get_mut(id) {
                        m.importance = *i;
                        touched.push(*id);
                    }
                }
            }
            QuickStepAction::Categorize(name) => {
                let name = name.trim();
                if !name.is_empty() {
                    for id in &live {
                        if let Some(m) = s.mailbox.mail.get_mut(id)
                            && !m.categories.iter().any(|c| c.trim().eq_ignore_ascii_case(name))
                        {
                            m.categories.push(name.to_owned());
                            touched.push(*id);
                        }
                    }
                }
            }
            QuickStepAction::ClearCategories => {
                for id in &live {
                    if let Some(m) = s.mailbox.mail.get_mut(id)
                        && !m.categories.is_empty()
                    {
                        m.categories.clear();
                        touched.push(*id);
                    }
                }
            }
            QuickStepAction::Flag(label) => {
                let (label, start, due) = flag_of(s, label);
                for id in &live {
                    if let Some(m) = s.mailbox.mail.get_mut(id) {
                        m.flag = FlagState::Flagged { label: label.clone(), start, due, reminder: None };
                        touched.push(*id);
                    }
                }
            }
            QuickStepAction::ClearFlag => {
                for id in &live {
                    if let Some(m) = s.mailbox.mail.get_mut(id) {
                        m.flag = FlagState::None;
                        touched.push(*id);
                    }
                }
            }
            QuickStepAction::MarkComplete => {
                for id in &live {
                    if let Some(m) = s.mailbox.mail.get_mut(id) {
                        m.flag = FlagState::Complete { completed: Some(now) };
                        touched.push(*id);
                    }
                }
            }
            QuickStepAction::NewMessageTo(to) => {
                let c = ComposeState { kind: ComposeKind::New, to: to.join("; "), ..Default::default() };
                let cid = s.open_compose(crate::compose::with_default_signature(&s.mailbox, c));
                composes.push(cid);
            }
            QuickStepAction::Reply | QuickStepAction::ReplyAll | QuickStepAction::Forward(_) => {
                let (cmd, kind, to) = match action {
                    QuickStepAction::Reply => ("mail.reply", ComposeKind::Reply, &[][..]),
                    QuickStepAction::ReplyAll => ("mail.replyAll", ComposeKind::ReplyAll, &[][..]),
                    QuickStepAction::Forward(to) => ("mail.forward", ComposeKind::Forward, to.as_slice()),
                    _ => continue,
                };
                // A reply goes to one message (several are refused above); a forward of several messages is
                // one new message with all of them attached (more than the forward cap are refused above).
                // When the response cannot be made, the step stops: the run fails and is rolled back, so a
                // later Delete or Move never acts on messages that were not answered or forwarded.
                if !live.is_empty()
                    && let Some(cid) = respond(s, cmd, kind, &live, to)?
                {
                    composes.push(cid);
                }
            }
            QuickStepAction::NewMeeting => match invoke(s, "calendar.newMeeting", &json!({})) {
                Ok(_) => {}
                Err(CmdError::Unknown(_)) => {
                    dialog(s, "newMeeting", json!({}))?;
                }
                Err(e) => errors.push(e.to_string()),
            },
        }
    }
    touched.sort();
    touched.dedup();
    Ok(json!({"step": step.id, "items": touched.len(), "composes": composes, "errors": errors}))
}

/// Follow Up keys a `Flag` action may hold, with their menu labels.
pub const FLAG_KEYS: [(&str, &str); 5] = [
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("thisWeek", "This Week"),
    ("nextWeek", "Next Week"),
    ("noDate", "No Date"),
];

/// Label and dates for a `Flag` action: a Follow Up key sets the dates (label "Follow up"), other text is
/// the label of a flag without dates.
fn flag_of(
    s: &Session,
    key: &str,
) -> (String, Option<mailcraft_model::DateTime>, Option<mailcraft_model::DateTime>) {
    use crate::cmd::mail::{FlagKind, flag_dates};
    let kind = match key.trim() {
        "today" => Some(FlagKind::Today),
        "tomorrow" => Some(FlagKind::Tomorrow),
        "thisWeek" => Some(FlagKind::ThisWeek),
        "nextWeek" => Some(FlagKind::NextWeek),
        "noDate" => Some(FlagKind::NoDate),
        _ => None,
    };
    match kind {
        Some(k) => {
            let (start, due) = flag_dates(s, k);
            ("Follow up".to_owned(), start, due)
        }
        None => {
            let label = if key.trim().is_empty() { "Follow up".to_owned() } else { key.trim().to_owned() };
            (label, None, None)
        }
    }
}

/// Action list and name for a New Quick Step template.
fn template(s: &Session, name: &str) -> Result<(&'static str, Vec<QuickStepAction>), CmdError> {
    let first_category = s.mailbox.categories.first().map(|c| c.name.clone()).unwrap_or_default();
    Ok(match name {
        "moveToFolder" => ("Move to folder", vec![QuickStepAction::MoveTo(None), QuickStepAction::MarkRead]),
        "categorizeMove" => (
            "Categorize & Move",
            vec![QuickStepAction::Categorize(first_category), QuickStepAction::MoveTo(None)],
        ),
        "flagMove" => {
            ("Flag & Move", vec![QuickStepAction::Flag("today".into()), QuickStepAction::MoveTo(None)])
        }
        "newEmailTo" => ("New email to", vec![QuickStepAction::NewMessageTo(Vec::new())]),
        "forwardTo" => ("Forward to", vec![QuickStepAction::Forward(Vec::new())]),
        "newMeeting" => ("New meeting", vec![QuickStepAction::NewMeeting]),
        "custom" => ("My Quick Step", Vec::new()),
        other => return Err(CmdError::Params(format!("unknown template `{other}`"))),
    })
}

fn new_step(s: &mut Session, p: &Value) -> CmdResult {
    let tpl = opt_str(p, "template").unwrap_or("custom");
    let (tpl_name, tpl_actions) = template(s, tpl)?;
    let Some(name) = opt_str(p, "name") else {
        return dialog(
            s,
            "editQuickStep",
            json!({"template": tpl, "name": tpl_name, "actions": tpl_actions}),
        );
    };
    if s.mailbox.quick_steps.len() >= MAX_STEPS {
        return Err(CmdError::Failed(format!("at most {MAX_STEPS} quick steps")));
    }
    let name = clean_name(name)?;
    let actions: Vec<QuickStepAction> = parse_field(p, "actions")?.unwrap_or(tpl_actions);
    check_actions(s, &actions)?;
    let shortcut = opt_str(p, "shortcut").map(parse_shortcut).transpose()?.unwrap_or_default();
    let tooltip = opt_str(p, "tooltip").unwrap_or("").trim().to_owned();
    let id = next_step_id(s)?;
    s.mailbox.quick_steps.push(QuickStep { id, name, actions, shortcut: String::new(), tooltip });
    let idx = s.mailbox.quick_steps.len().saturating_sub(1);
    set_shortcut(s, idx, shortcut);
    Ok(json!({"id": id}))
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    let steps: Vec<Value> = s.mailbox.quick_steps.iter().map(step_json).collect();
    Ok(json!({"steps": steps}))
}

fn manage(s: &mut Session, _: &Value) -> CmdResult {
    let steps: Vec<Value> = s.mailbox.quick_steps.iter().map(step_json).collect();
    dialog(s, "manageQuickSteps", json!({"steps": steps}))
}

fn edit(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find_step(s, p)?;
    let fields = ["name", "actions", "shortcut", "tooltip"];
    if !fields.iter().any(|k| p.get(*k).is_some()) {
        let step = s.mailbox.quick_steps.get(idx).map(step_json).unwrap_or(Value::Null);
        return dialog(s, "editQuickStep", json!({"step": step}));
    }
    let name = opt_str(p, "name").map(clean_name).transpose()?;
    let actions: Option<Vec<QuickStepAction>> = parse_field(p, "actions")?;
    if let Some(a) = &actions {
        check_actions(s, a)?;
    }
    let shortcut = opt_str(p, "shortcut").map(parse_shortcut).transpose()?;
    let tooltip = opt_str(p, "tooltip").map(|t| t.trim().to_owned());
    let Some(q) = s.mailbox.quick_steps.get_mut(idx) else {
        return Err(CmdError::Params("no such quick step".into()));
    };
    if let Some(n) = name {
        q.name = n;
    }
    if let Some(a) = actions {
        q.actions = a;
    }
    if let Some(t) = tooltip {
        q.tooltip = t;
    }
    let id = q.id;
    if let Some(sc) = shortcut {
        set_shortcut(s, idx, sc);
    }
    Ok(json!({"id": id}))
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find_step(s, p)?;
    if idx >= s.mailbox.quick_steps.len() {
        return Err(CmdError::Params("no such quick step".into()));
    }
    let q = s.mailbox.quick_steps.remove(idx);
    Ok(json!({"deleted": q.id}))
}

fn duplicate(s: &mut Session, p: &Value) -> CmdResult {
    let idx = find_step(s, p)?;
    if s.mailbox.quick_steps.len() >= MAX_STEPS {
        return Err(CmdError::Failed(format!("at most {MAX_STEPS} quick steps")));
    }
    let Some(mut q) = s.mailbox.quick_steps.get(idx).cloned() else {
        return Err(CmdError::Params("no such quick step".into()));
    };
    q.id = next_step_id(s)?;
    q.name = clean_name(&format!("Copy of {}", q.name)).unwrap_or(q.name);
    q.shortcut.clear();
    let id = q.id;
    let at = idx.saturating_add(1).min(s.mailbox.quick_steps.len());
    s.mailbox.quick_steps.insert(at, q);
    Ok(json!({"id": id}))
}

fn shift(s: &mut Session, p: &Value, up: bool) -> CmdResult {
    let i = find_step(s, p)?;
    let len = s.mailbox.quick_steps.len();
    let j = if up { i.checked_sub(1) } else { i.checked_add(1).filter(|j| *j < len) };
    let Some(j) = j else {
        return Ok(json!({"moved": false}));
    };
    s.mailbox.quick_steps.swap(i, j);
    Ok(json!({"moved": true, "index": j}))
}

fn move_up(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, true)
}

fn move_down(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, false)
}

fn reset(s: &mut Session, _: &Value) -> CmdResult {
    s.mailbox.quick_steps = QuickStep::defaults();
    Ok(json!({"steps": s.mailbox.quick_steps.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use mailcraft_model::Importance;

    #[test]
    fn first_use_setup_then_run() {
        let (mut s, inbox, proj) = session();
        let r = s.run("quicksteps.run", &json!({"id": "Move to: ?"})).unwrap();
        assert_eq!(r["dialog"], "quickStepFirstUse");
        let r = s.run("quicksteps.run", &json!({"id": 1, "folder": proj.0})).unwrap();
        assert_eq!(r["items"], 1);
        assert_eq!(s.mailbox.total_count(proj), 1);
        assert_eq!(s.mailbox.total_count(inbox), 2);
        assert!(s.mailbox.items_in(proj).iter().all(|m| m.read));
        // Setup is remembered.
        assert!(!s.mailbox.quick_steps[0].needs_setup());
    }

    #[test]
    fn first_use_setup_keeps_the_items_it_was_run_on() {
        let (mut s, inbox, _) = session();
        let other = s.mailbox.items_in(inbox)[2].id;
        s.run("quicksteps.run", &json!({"id": 1, "ids": [other.0]})).unwrap();
        let Some(crate::Notice::OpenDialog(name, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(name, "quickStepFirstUse");
        assert_eq!(p["ids"], json!([other.0]));
    }

    #[test]
    fn custom_actions() {
        let (mut s, inbox, _) = session();
        s.run(
            "quicksteps.new",
            &json!({"name": "Tidy", "actions": [{"Categorize": "Blue category"}, {"SetImportance": "High"}, {"Flag": "Reply"}, "MarkRead"], "shortcut": "Mod+Shift+3"}),
        )
        .unwrap();
        s.selection.items = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        let r = s.run("quicksteps.run", &json!({"id": "tidy"})).unwrap();
        assert_eq!(r["items"], 3);
        for m in s.mailbox.items_in(inbox) {
            assert_eq!(m.categories, vec!["Blue category".to_string()]);
            assert_eq!(m.importance, Importance::High);
            assert!(matches!(&m.flag, FlagState::Flagged { label, .. } if label == "Reply"));
            assert!(m.read);
        }
        assert_eq!(s.mailbox.quick_steps.last().unwrap().shortcut, "Ctrl+Shift+3");
    }

    #[test]
    fn reply_and_delete_opens_compose() {
        let (mut s, inbox, _) = session();
        let r = s.run("quicksteps.run", &json!({"id": "Reply & Delete"})).unwrap();
        assert_eq!(r["composes"].as_array().unwrap().len(), 1);
        assert_eq!(s.composes.len(), 1);
        assert_eq!(s.mailbox.total_count(inbox), 2);
    }

    #[test]
    fn reply_and_delete_by_ids_without_selection_opens_the_reply() {
        let (mut s, inbox, _) = session();
        let target = s.selection.items[0];
        s.selection.items.clear();
        s.selection.focus = None;
        let undo_before = s.undo.undo.len();
        let r = s.run("quicksteps.run", &json!({"id": "Reply & Delete", "ids": [target.0]})).unwrap();
        // The reply must open before the original is deleted; a nested command must not fail on the
        // (empty) selection.
        assert_eq!(r["errors"], json!([]), "{r}");
        assert_eq!(r["composes"].as_array().unwrap().len(), 1);
        assert_eq!(s.composes.values().next().and_then(|c| c.source), Some(target));
        assert_eq!(s.mailbox.total_count(inbox), 2);
        // One run of a quick step is one undo step.
        assert_eq!(s.undo.undo.len(), undo_before + 1);
    }

    #[test]
    fn new_meeting_step_opens_the_meeting_form() {
        let (mut s, _, _) = session();
        s.run("quicksteps.new", &json!({"name": "Meet", "actions": ["NewMeeting"]})).unwrap();
        let undo_before = s.undo.undo.len();
        let appointments_before = s.mailbox.appointments.len();
        let r = s.run("quicksteps.run", &json!({"id": "Meet"})).unwrap();
        assert_eq!(r["errors"], json!([]), "{r}");
        // New Meeting opens the meeting form; nothing is saved (and nothing to undo) until it is sent.
        assert!(
            s.notices.iter().any(|n| matches!(n, crate::Notice::OpenDialog(_, p) if p["meeting"] == true)),
            "{:?}",
            s.notices
        );
        assert_eq!(s.mailbox.appointments.len(), appointments_before);
        assert_eq!(s.undo.undo.len(), undo_before);
    }

    #[test]
    fn new_email_to_needs_no_selection() {
        let (mut s, _, _) = session();
        s.selection.items.clear();
        s.selection.focus = None;
        let r = s
            .run("quicksteps.run", &json!({"id": "Team Email", "to": "a@example.com; b@example.com"}))
            .unwrap();
        let cid = r["composes"][0].as_u64().unwrap();
        assert_eq!(s.composes[&cid].to, "a@example.com; b@example.com");
        assert!(s.run("quicksteps.run", &json!({"id": "Done", "folder": 1})).is_err());
    }

    /// A session whose mailbox has a "New" signature for new messages and a "Re" one for replies/forwards.
    fn signed_session() -> (Session, mailcraft_model::FolderId, mailcraft_model::FolderId) {
        let (mut s, inbox, proj) = session();
        s.mailbox.add_signature("New", mailcraft_model::Doc::from_plain_text("-- New")).unwrap();
        s.mailbox.add_signature("Re", mailcraft_model::Doc::from_plain_text("-- Re")).unwrap();
        s.mailbox.set_signature_defaults(None, Some(Some("New")), Some(Some("Re"))).unwrap();
        (s, inbox, proj)
    }

    fn signature_name(s: &Session, cid: &Value) -> Option<String> {
        s.composes.get(&cid.as_u64().unwrap()).and_then(|c| c.signature.as_ref()).map(|g| g.name.clone())
    }

    #[test]
    fn created_messages_get_the_default_signature() {
        let (mut s, _, _) = signed_session();
        let r = s.run("quicksteps.run", &json!({"id": "Team Email", "to": "a@example.com"})).unwrap();
        assert_eq!(signature_name(&s, &r["composes"][0]).as_deref(), Some("New"));
        let r = s.run("quicksteps.run", &json!({"id": "Reply & Delete"})).unwrap();
        assert_eq!(signature_name(&s, &r["composes"][0]).as_deref(), Some("Re"));
        // Exactly one signature block, even though the reply passed through two builders.
        let c = &s.composes[&r["composes"][0].as_u64().unwrap()];
        let text = c.body.to_plain_text();
        assert_eq!(text.matches("-- Re").count(), 1, "{text}");
    }

    #[test]
    fn fallback_reply_gets_the_reply_signature() {
        let (mut s, _, _) = signed_session();
        // Without the mail commands the quick step builds the reply itself.
        s.registry.specs.retain(|c| c.id != "mail.reply");
        let target = s.selection.items[0];
        let cid = respond(&mut s, "mail.reply", ComposeKind::Reply, &[target], &[]).unwrap().unwrap();
        let c = &s.composes[&cid];
        assert_eq!(c.source, Some(target));
        assert_eq!(c.signature.as_ref().map(|g| g.name.as_str()), Some("Re"));
        assert_eq!(c.selection.focus.path, vec![0], "the caret sits above the signature");
    }

    #[test]
    fn manage_edit_duplicate_delete_reset() {
        let (mut s, _, _) = session();
        assert_eq!(s.run("quicksteps.manage", &json!({})).unwrap()["dialog"], "manageQuickSteps");
        assert_eq!(
            s.run("quicksteps.new", &json!({"template": "flagMove"})).unwrap()["dialog"],
            "editQuickStep"
        );
        assert!(matches!(s.run("quicksteps.new", &json!({"template": "nope"})), Err(CmdError::Params(_))));
        s.run("quicksteps.edit", &json!({"id": "Done", "name": "Finished", "shortcut": "Ctrl+Shift+1"}))
            .unwrap();
        s.run("quicksteps.edit", &json!({"id": "Team Email", "shortcut": "Ctrl+Shift+1"})).unwrap();
        assert_eq!(s.mailbox.quick_steps[3].shortcut, "");
        assert_eq!(s.mailbox.quick_steps[2].shortcut, "Ctrl+Shift+1");
        assert!(matches!(
            s.run("quicksteps.edit", &json!({"id": 1, "shortcut": "F5"})),
            Err(CmdError::Params(_))
        ));
        let d = s.run("quicksteps.duplicate", &json!({"id": "Finished"})).unwrap();
        assert_eq!(s.mailbox.quick_steps[4].name, "Copy of Finished");
        assert_eq!(s.mailbox.quick_steps[4].id, d["id"].as_u64().unwrap());
        s.run("quicksteps.moveUp", &json!({"id": "Copy of Finished"})).unwrap();
        assert_eq!(s.mailbox.quick_steps[3].name, "Copy of Finished");
        s.run("quicksteps.delete", &json!({"id": "Copy of Finished"})).unwrap();
        assert_eq!(s.mailbox.quick_steps.len(), 6);
        s.run("quicksteps.reset", &json!({})).unwrap();
        assert_eq!(s.mailbox.quick_steps, QuickStep::defaults());
    }

    #[test]
    fn create_new_opens_editor() {
        let (mut s, _, _) = session();
        let r = s.run("quicksteps.run", &json!({"id": "Create New"})).unwrap();
        assert_eq!(r["dialog"], "editQuickStep");
        // The editor opens on a new step (no step id), so saving adds a step and keeps "Create New".
        let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert!(p.get("step").is_none(), "{p}");
        assert_eq!(p["template"], "custom");
        assert_eq!(p["name"], "My Quick Step");
        let before = s.mailbox.quick_steps.len();
        s.run("quicksteps.new", &json!({"template": "custom", "name": "Mine", "actions": ["MarkRead"]}))
            .unwrap();
        assert_eq!(s.mailbox.quick_steps.len(), before + 1);
        assert!(s.mailbox.quick_steps.iter().any(|q| q.name == "Create New"));
    }

    #[test]
    fn reply_steps_refuse_several_messages_and_delete_nothing() {
        let (mut s, inbox, _) = session();
        let all: Vec<u64> = s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        assert_eq!(all.len(), 3);
        let r = s.run("quicksteps.run", &json!({"id": "Reply & Delete", "ids": all}));
        assert!(matches!(r, Err(CmdError::Disabled(_))), "{r:?}");
        assert_eq!(s.mailbox.total_count(inbox), 3, "nothing may be deleted");
        assert!(s.composes.is_empty());
        // A group header refuses it even for one item.
        let one = json!({"id": "Reply & Delete", "ids": [all[0]], "group": true});
        assert!(matches!(s.run("quicksteps.run", &one), Err(CmdError::Disabled(_))));
        let step = s.mailbox.quick_steps.iter().find(|q| q.name == "Reply & Delete").unwrap().clone();
        assert!(blocked_for(&step, 1, false).is_none());
        assert!(blocked_for(&step, 2, false).is_some());
    }

    #[test]
    fn forward_step_sends_several_messages_as_one_forward_with_attachments() {
        let (mut s, inbox, _) = session();
        let all: Vec<u64> = s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        let r = s
            .run("quicksteps.run", &json!({"id": "To Manager", "ids": all, "to": "boss@example.com"}))
            .unwrap();
        let composes = r["composes"].as_array().unwrap();
        assert_eq!(composes.len(), 1, "{r}");
        let c = &s.composes[&composes[0].as_u64().unwrap()];
        assert_eq!(c.attachments.len(), 3);
        assert!(c.to.contains("boss@example.com"));
        // Not on a list group's header.
        let group = json!({"id": "To Manager", "ids": all, "group": true});
        assert!(matches!(s.run("quicksteps.run", &group), Err(CmdError::Disabled(_))));
    }

    /// Regression: a Forward + Delete step on more than 20 messages forwarded the first 20 and deleted all.
    #[test]
    fn forward_and_delete_forwards_every_message_it_deletes() {
        let (mut s, inbox, _) = session();
        for i in 0..25 {
            s.mailbox.add_mail(mailcraft_model::MailItem {
                folder: inbox,
                subject: format!("Extra {i}"),
                ..Default::default()
            });
        }
        let r = s
            .run(
                "quicksteps.new",
                &json!({"name": "Forward & Delete", "actions": [{"Forward": ["boss@example.com"]}, "Delete"]}),
            )
            .unwrap();
        let step = r["id"].clone();
        let all: Vec<u64> = s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        assert_eq!(all.len(), 28);
        let r = s.run("quicksteps.run", &json!({"id": step, "ids": all})).unwrap();
        let cid = r["composes"][0].as_u64().unwrap();
        assert_eq!(s.composes[&cid].attachments.len(), 28, "every deleted message was forwarded");
        assert_eq!(s.mailbox.total_count(inbox), 0);
        // Over the forward cap: refused before anything runs.
        for i in 0..=crate::cmd::respond::MAX_FORWARD_ITEMS {
            s.mailbox.add_mail(mailcraft_model::MailItem {
                folder: inbox,
                subject: format!("Many {i}"),
                ..Default::default()
            });
        }
        let many: Vec<u64> = s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        let composes = s.composes.len();
        let r = s.run("quicksteps.run", &json!({"id": step, "ids": many}));
        assert!(matches!(r, Err(CmdError::Disabled(_))), "{r:?}");
        assert_eq!(s.mailbox.total_count(inbox), many.len(), "nothing was deleted");
        assert_eq!(s.composes.len(), composes);
        // A forward that cannot be built (too large) stops the step: nothing is deleted.
        let big = s.mailbox.add_mail(mailcraft_model::MailItem {
            folder: inbox,
            subject: "Huge".into(),
            size: 60 * 1024 * 1024,
            ..Default::default()
        });
        let r = s.run("quicksteps.run", &json!({"id": step, "ids": [big.0, many[0]]}));
        assert!(r.is_err(), "{r:?}");
        assert!(s.mailbox.mail.contains_key(&big));
        assert_eq!(s.mailbox.mail[&big].folder, inbox);
        assert_eq!(s.composes.len(), composes);
        // The cap and the message in `blocked_for` agree.
        assert!(
            blocked_for(
                &s.mailbox.quick_steps.iter().find(|q| q.name == "Forward & Delete").unwrap().clone(),
                100,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn run_acts_on_the_given_ids_not_the_selection() {
        let (mut s, inbox, proj) = session();
        let items: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        s.selection.items = vec![items[0]];
        s.selection.focus = Some(items[0]);
        s.run("quicksteps.run", &json!({"id": 1, "folder": proj.0, "ids": [items[2].0]})).unwrap();
        assert_eq!(s.mailbox.mail[&items[2]].folder, proj);
        assert_eq!(s.mailbox.mail[&items[0]].folder, inbox);
    }

    #[test]
    fn first_use_setup_can_rename_the_step() {
        let (mut s, _, proj) = session();
        s.run("quicksteps.run", &json!({"id": 1, "folder": proj.0, "name": "Projects"})).unwrap();
        assert_eq!(s.mailbox.quick_steps[0].name, "Projects");
        assert_eq!(s.mailbox.quick_steps[0].actions[0], QuickStepAction::MoveTo(Some(proj)));
        // A bad name leaves the step as it was.
        s.run("quicksteps.reset", &json!({})).unwrap();
        assert!(s.run("quicksteps.run", &json!({"id": 1, "folder": proj.0, "name": "  "})).is_err());
        assert!(s.mailbox.quick_steps[0].needs_setup());
    }

    #[test]
    fn flag_keys_set_follow_up_dates() {
        let (mut s, inbox, _) = session();
        s.fixed_now = mailcraft_model::DateTime::from_civil(2026, 10, 7, 9, 0, 0, 0); // a Wednesday
        s.run("quicksteps.new", &json!({"name": "Tomorrow", "actions": [{"Flag": "tomorrow"}]})).unwrap();
        s.run("quicksteps.new", &json!({"name": "Undated", "actions": [{"Flag": "noDate"}]})).unwrap();
        s.selection.items = s.mailbox.items_in(inbox).iter().map(|m| m.id).take(1).collect();
        let id = s.selection.items[0];
        s.run("quicksteps.run", &json!({"id": "Tomorrow"})).unwrap();
        match &s.mailbox.mail[&id].flag {
            FlagState::Flagged { label, due: Some(due), .. } => {
                assert_eq!(label, "Follow up");
                assert_eq!(mailcraft_model::ListTime::date(*due, 0), "10/8/2026");
            }
            other => panic!("{other:?}"),
        }
        s.run("quicksteps.run", &json!({"id": "Undated"})).unwrap();
        assert!(matches!(&s.mailbox.mail[&id].flag, FlagState::Flagged { due: None, start: None, .. }));
        assert_eq!(s.run("quicksteps.list", &json!({})).unwrap()["steps"].as_array().unwrap().len(), 8);
    }
}
