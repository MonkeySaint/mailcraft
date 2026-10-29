//! Window-level commands: undo/redo, copy/paste of items, window state, ribbon display, Quick Access Toolbar,
//! Close All Items.

use serde_json::{Value, json};

use crate::cmd::util::{dialog, mail_targets, opt_folder, opt_str};
use crate::queries;
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::{FolderKind, ItemId, MailItem};

/// Longest text accepted by Copy (characters); longer text is truncated.
const MAX_CLIPBOARD_CHARS: usize = 1_000_000;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "app.closeAllItems",
            label: "Close All Items",
            location: "View › Window",
            shortcut: "",
            params: r#"{"save"?: bool}  (absent: unsaved drafts stay open and a save prompt is requested; read and item windows close)"#,
            mutates: true,
            enabled: always,
            run: close_all_items,
        },
        CommandSpec {
            id: "app.copy",
            label: "Copy",
            location: "Message list context menu / group header context menu",
            shortcut: "Mod+C",
            params: r#"{"ids"?: [number], "group"?: string, "text"?: string}"#,
            mutates: false,
            enabled: always,
            run: copy,
        },
        CommandSpec {
            id: "app.paste",
            label: "Paste",
            location: "Message list",
            shortcut: "",
            params: r#"{"folder"?: number}"#,
            mutates: true,
            enabled: |s| if s.clipboard.is_empty() { Some("nothing to paste") } else { None },
            run: paste,
        },
        CommandSpec {
            id: "app.minimize",
            label: "Minimize",
            location: "Window caption",
            shortcut: "",
            params: "",
            mutates: false,
            enabled: always,
            run: minimize,
        },
        CommandSpec {
            id: "app.quickAccess",
            label: "Customize Quick Access Toolbar",
            location: "Quick Access Toolbar",
            shortcut: "",
            params: r#"{"add"?: string (command id), "remove"?: string (command id)}"#,
            mutates: false,
            enabled: always,
            run: quick_access,
        },
        CommandSpec {
            id: "app.toggleMaximize",
            label: "Restore Down / Maximize",
            location: "Window caption",
            shortcut: "",
            params: "",
            mutates: false,
            enabled: always,
            run: toggle_maximize,
        },
        CommandSpec {
            id: "app.toggleRibbonMode",
            label: "Ribbon Display Options",
            location: "Ribbon bottom-right corner",
            shortcut: "",
            params: r#"{"mode": "classic"|"simplified"|"tabsOnly"|"autoHide"|"always"}"#,
            mutates: false,
            enabled: always,
            run: ribbon_mode,
        },
        CommandSpec {
            id: "app.undo",
            label: "Undo",
            location: "Quick Access Toolbar",
            shortcut: "Mod+Z",
            params: "",
            mutates: true,
            enabled: |s| if s.undo.can_undo() { None } else { Some("nothing to undo") },
            run: undo,
        },
        CommandSpec {
            id: "app.redo",
            label: "Redo",
            location: "Quick Access Toolbar",
            shortcut: "",
            params: "",
            mutates: true,
            enabled: |s| if s.undo.can_redo() { None } else { Some("nothing to redo") },
            run: redo,
        },
    ]
}

fn undo(s: &mut Session, _: &Value) -> CmdResult {
    let label = s.undo_last()?;
    Ok(json!({"undone": label}))
}

fn redo(s: &mut Session, _: &Value) -> CmdResult {
    let label = s.redo_last()?;
    Ok(json!({"redone": label}))
}

fn minimize(s: &mut Session, _: &Value) -> CmdResult {
    s.view.minimized = true;
    Ok(json!({"minimized": true}))
}

fn toggle_maximize(s: &mut Session, _: &Value) -> CmdResult {
    s.view.minimized = false;
    s.view.maximized = !s.view.maximized;
    Ok(json!({"maximized": s.view.maximized}))
}

fn ribbon_state(s: &Session) -> Value {
    let display = if s.view.ribbon_auto_hide {
        "autoHide"
    } else if s.view.ribbon_collapsed {
        "tabsOnly"
    } else {
        "always"
    };
    json!({
        "style": if s.view.simplified_ribbon { "simplified" } else { "classic" },
        "display": display,
    })
}

fn ribbon_mode(s: &mut Session, p: &Value) -> CmdResult {
    let Some(mode) = p.get("mode").filter(|v| !v.is_null()) else {
        let state = ribbon_state(s);
        return dialog(s, "ribbonDisplayOptions", state);
    };
    let mode = mode.as_str().ok_or_else(|| CmdError::Params("`mode` must be a string".into()))?;
    match mode {
        "classic" => s.view.simplified_ribbon = false,
        "simplified" => s.view.simplified_ribbon = true,
        "tabsOnly" => {
            s.view.ribbon_collapsed = true;
            s.view.ribbon_auto_hide = false;
        }
        "autoHide" | "fullScreen" => {
            s.view.ribbon_auto_hide = true;
            s.view.ribbon_collapsed = false;
        }
        "always" => {
            s.view.ribbon_auto_hide = false;
            s.view.ribbon_collapsed = false;
        }
        other => {
            let shown: String = other.chars().take(40).collect();
            return Err(CmdError::Params(format!(
                "unknown ribbon mode `{shown}` (classic, simplified, tabsOnly, autoHide, always)"
            )));
        }
    }
    Ok(ribbon_state(s))
}

fn quick_access(s: &mut Session, p: &Value) -> CmdResult {
    let add = p.get("add").filter(|v| !v.is_null());
    let remove = p.get("remove").filter(|v| !v.is_null());
    if add.is_none() && remove.is_none() {
        let items = s.view.quick_access.clone();
        s.notify(crate::Notice::OpenDialog("quickAccess".into(), json!({"items": items})));
        return Ok(json!({"dialog": "quickAccess", "items": items}));
    }
    // Validate both before changing anything.
    let add = match add {
        Some(v) => {
            let id =
                v.as_str().ok_or_else(|| CmdError::Params("`add` must be a command id string".into()))?;
            if s.registry.get(id).is_none() {
                let shown: String = id.chars().take(60).collect();
                return Err(CmdError::Params(format!("unknown command `{shown}`")));
            }
            Some(id.to_owned())
        }
        None => None,
    };
    let remove = match remove {
        Some(v) => {
            Some(v.as_str().ok_or_else(|| CmdError::Params("`remove` must be a command id string".into()))?)
        }
        None => None,
    };
    let mut removed = false;
    if let Some(id) = remove {
        let before = s.view.quick_access.len();
        s.view.quick_access.retain(|q| q != id);
        removed = s.view.quick_access.len() != before;
    }
    let mut added = false;
    if let Some(id) = add
        && !s.view.quick_access.contains(&id)
    {
        s.view.quick_access.push(id);
        added = true;
    }
    Ok(json!({"items": s.view.quick_access, "added": added, "removed": removed}))
}

fn copy(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(text) = p.get("text").filter(|v| !v.is_null()) {
        let text = text.as_str().ok_or_else(|| CmdError::Params("`text` must be a string".into()))?;
        let text: String = text.chars().take(MAX_CLIPBOARD_CHARS).collect();
        let chars = text.chars().count();
        // Copies from the reading pane join the Clipboard pane's history too.
        crate::cmd::format::collect_clip(s, &text, opt_str(p, "html"), false);
        s.clipboard_text = Some(text);
        return Ok(json!({"copied_text": chars}));
    }
    let ids: Vec<ItemId> = if let Some(group) = p.get("group").filter(|v| !v.is_null()) {
        let label = group.as_str().ok_or_else(|| CmdError::Params("`group` must be a group label".into()))?;
        queries::group_items(s, label)
    } else {
        mail_targets(s, p)
    };
    if ids.is_empty() {
        return Err(CmdError::Disabled("nothing selected to copy".into()));
    }
    s.clipboard = ids;
    let text = clipboard_text(s, &s.clipboard);
    let ids: Vec<u64> = s.clipboard.iter().map(|i| i.0).collect();
    Ok(json!({"copied": ids.len(), "ids": ids, "text": text}))
}

/// Most items a copy renders as text for the system clipboard.
const MAX_CLIPBOARD_ITEMS: usize = 500;

/// Copied messages as text for other programs: a From / Sent / To / Subject block per message.
pub fn clipboard_text(s: &Session, ids: &[ItemId]) -> String {
    let tz = s.tz_offset_min;
    let mut out = String::new();
    for m in ids.iter().take(MAX_CLIPBOARD_ITEMS).filter_map(|id| s.mailbox.mail.get(*id)) {
        if !out.is_empty() {
            out.push('\n');
        }
        let who = |a: &mailcraft_model::Address| match &a.name {
            Some(n) if !n.trim().is_empty() => format!("{n} <{}>", a.addr),
            _ => a.addr.clone(),
        };
        if let Some(f) = &m.from {
            out.push_str(&format!("From: {}\n", who(f)));
        }
        out.push_str(&format!("Received: {}\n", mailcraft_model::ListTime::long_date(m.received, tz)));
        if !m.to.is_empty() {
            out.push_str(&format!("To: {}\n", m.to.iter().map(who).collect::<Vec<_>>().join("; ")));
        }
        out.push_str(&format!("Subject: {}\n", m.subject));
    }
    out
}

fn paste(s: &mut Session, p: &Value) -> CmdResult {
    let folder = opt_folder(p).unwrap_or(s.selection.folder);
    let target = s
        .mailbox
        .folder(folder)
        .ok_or_else(|| CmdError::Params(format!("no folder with id {}", folder.0)))?;
    if target.kind != FolderKind::Mail {
        return Err(CmdError::Failed(format!("messages cannot be pasted into \"{}\"", target.name)));
    }
    let sources: Vec<MailItem> =
        s.clipboard.iter().filter_map(|id| s.mailbox.mail.get(*id).cloned()).collect();
    if sources.is_empty() {
        return Err(CmdError::Disabled("nothing to paste".into()));
    }
    let mut new_ids = Vec::new();
    for mut item in sources {
        item.folder = folder;
        item.remote_id = None;
        new_ids.push(s.mailbox.add_mail(item).0);
    }
    Ok(json!({"pasted": new_ids.len(), "ids": new_ids, "folder": folder.0}))
}

/// Dialog request that asks the UI to close its read windows and item windows (Close All Items); compose
/// windows are closed here.
pub const CLOSE_ITEM_WINDOWS: &str = "closeAllItems";

/// Close All Items: closes every compose window (asking about unsaved ones unless `save` says what to do) and
/// asks the UI to close its read and item windows ([`CLOSE_ITEM_WINDOWS`]).
fn close_all_items(s: &mut Session, p: &Value) -> CmdResult {
    let save = match p.get("save").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| CmdError::Params("`save` must be true or false".into()))?),
    };
    // Read windows and item windows hold nothing unsaved that the engine knows of: they close right away.
    s.notify(crate::Notice::OpenDialog(CLOSE_ITEM_WINDOWS.into(), json!({})));
    let dirty: Vec<u64> = s.composes.values().filter(|c| c.dirty).map(|c| c.id).collect();
    let mut saved = Vec::new();
    match save {
        None if !dirty.is_empty() => {
            // Close the clean windows; ask about the rest.
            let before = s.composes.len();
            s.composes.retain(|_, c| c.dirty);
            let closed = before.saturating_sub(s.composes.len());
            s.notify(crate::Notice::OpenDialog("saveChanges".into(), json!({"composes": dirty})));
            return Ok(json!({"closed": closed, "pending": dirty, "dialog": "saveChanges"}));
        }
        Some(true) => {
            for cid in &dirty {
                // Saved like any draft (the draft of an encrypted message stays encrypted).
                saved.push(crate::cmd::compose::save_draft(s, *cid)?.0);
            }
        }
        _ => {}
    }
    let closed = s.composes.len();
    s.composes.clear();
    Ok(json!({"closed": closed, "saved": saved}))
}

#[cfg(test)]
mod tests {
    use crate::compose::ComposeState;
    use crate::sample::sample_session;
    use serde_json::json;

    #[test]
    fn window_and_ribbon_state() {
        let mut s = sample_session();
        assert!(s.view.maximized);
        s.run("app.toggleMaximize", &json!({})).unwrap();
        assert!(!s.view.maximized);
        s.run("app.minimize", &json!(null)).unwrap();
        assert!(s.view.minimized);
        s.run("app.toggleMaximize", &json!({})).unwrap();
        assert!(s.view.maximized && !s.view.minimized);
        s.run("app.toggleRibbonMode", &json!({"mode": "classic"})).unwrap();
        assert!(!s.view.simplified_ribbon);
        s.run("app.toggleRibbonMode", &json!({"mode": "tabsOnly"})).unwrap();
        assert!(s.view.ribbon_collapsed);
        s.run("app.toggleRibbonMode", &json!({"mode": "autoHide"})).unwrap();
        assert!(s.view.ribbon_auto_hide && !s.view.ribbon_collapsed);
        assert!(s.run("app.toggleRibbonMode", &json!({"mode": "sideways"})).is_err());
        let r = s.run("app.toggleRibbonMode", &json!({})).unwrap();
        assert_eq!(r["dialog"], "ribbonDisplayOptions");
    }

    #[test]
    fn quick_access_add_remove() {
        let mut s = sample_session();
        s.run("app.quickAccess", &json!({"add": "app.copy"})).unwrap();
        assert!(s.view.quick_access.iter().any(|q| q == "app.copy"));
        s.run("app.quickAccess", &json!({"remove": "app.undo"})).unwrap();
        assert!(!s.view.quick_access.iter().any(|q| q == "app.undo"));
        assert!(s.run("app.quickAccess", &json!({"add": "no.such"})).is_err());
        let r = s.run("app.quickAccess", &json!({})).unwrap();
        assert_eq!(r["dialog"], "quickAccess");
    }

    #[test]
    fn copy_and_paste_items() {
        let mut s = sample_session();
        let first = *s.mailbox.items_in(s.selection.folder).first().map(|m| &m.id).unwrap();
        s.selection.items = vec![first];
        let r = s.run("app.copy", &json!({})).unwrap();
        assert_eq!(r["copied"], 1);
        // A text rendering for other programs (the system clipboard).
        let subject = s.mailbox.mail.get(first).unwrap().subject.clone();
        let text = r["text"].as_str().unwrap();
        assert!(text.contains(&format!("Subject: {subject}")), "{text}");
        assert!(text.contains("Received: "), "{text}");
        let before = s.mailbox.mail.len();
        let archive = s.mailbox.folders.values().find(|f| f.name == "Archive").map(|f| f.id.0).unwrap();
        s.run("app.paste", &json!({"folder": archive})).unwrap();
        assert_eq!(s.mailbox.mail.len(), before + 1);
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail.len(), before);
        let r = s.run("app.copy", &json!({"text": "hello"})).unwrap();
        assert_eq!(r["copied_text"], 5);
        assert_eq!(s.clipboard_text.as_deref(), Some("hello"));
        let r = s.run("app.copy", &json!({"group": "Today"})).unwrap();
        assert!(r["copied"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn close_all_items_prompts_then_saves() {
        let mut s = sample_session();
        let clean = s.open_compose(ComposeState::default());
        let dirty = s.open_compose(ComposeState {
            subject: "Unsaved".into(),
            to: "Alice Rivera <alice@example.com>".into(),
            dirty: true,
            ..Default::default()
        });
        let r = s.run("app.closeAllItems", &json!({})).unwrap();
        assert_eq!(r["dialog"], "saveChanges");
        // The UI is asked to close read and item windows as well.
        assert!(
            s.notices
                .iter()
                .any(|n| matches!(n, crate::Notice::OpenDialog(d, _) if d == super::CLOSE_ITEM_WINDOWS))
        );
        assert!(!s.composes.contains_key(&clean));
        assert!(s.composes.contains_key(&dirty));
        let drafts_before = s.mailbox.mail.items().filter(|m| m.is_draft).count();
        s.run("app.closeAllItems", &json!({"save": true})).unwrap();
        assert!(s.composes.is_empty());
        let drafts: Vec<_> = s.mailbox.mail.items().filter(|m| m.is_draft).collect();
        assert_eq!(drafts.len(), drafts_before + 1);
        assert!(
            drafts.iter().any(
                |m| m.subject == "Unsaved" && m.to.first().is_some_and(|a| a.addr == "alice@example.com")
            )
        );
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut s = sample_session();
        assert!(s.run("app.undo", &json!({})).is_err());
        let first = *s.mailbox.items_in(s.selection.folder).first().map(|m| &m.id).unwrap();
        s.clipboard = vec![first];
        let before = s.mailbox.clone();
        s.run("app.paste", &json!({})).unwrap();
        let after = s.mailbox.clone();
        assert_ne!(before, after);
        s.run("app.undo", &json!({})).unwrap();
        // Everything comes back but the id counter: the pasted copy's id is not handed out again.
        assert_eq!(s.mailbox.next_id, after.next_id);
        let mut undone = s.mailbox.clone();
        undone.next_id = before.next_id;
        assert_eq!(undone, before);
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox, after);
        assert!(s.run("app.redo", &json!({})).is_err());
    }
}
