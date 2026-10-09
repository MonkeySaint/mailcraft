//! `options.*`: open the Options window at a page, and read/write the options the session actually holds
//! (view state). Unknown keys are refused rather than silently stored.

use serde_json::{Value, json};

use crate::cmd::rules::dialog;
use crate::cmd::util::{opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, SearchScope, Session, always};
use mailcraft_model::{FolderPaneState, ReadingPanePos};

/// Options pages in window order (`plan/outlook/options.md`): (id, title).
pub const PAGES: [(&str, &str); 14] = [
    ("general", "General"),
    ("mail", "Mail"),
    ("calendar", "Calendar"),
    ("groups", "Groups"),
    ("people", "People"),
    ("tasks", "Tasks"),
    ("search", "Search"),
    ("language", "Language"),
    ("accessibility", "Accessibility"),
    ("advanced", "Advanced"),
    ("customizeRibbon", "Customize Ribbon"),
    ("quickAccessToolbar", "Quick Access Toolbar"),
    ("addIns", "Add-ins"),
    ("trustCenter", "Trust Center"),
];

/// Pages opened from elsewhere that land on a section of a page: (alias, page, section).
const ALIASES: [(&str, &str, &str); 4] = [
    ("signatures", "mail", "signatures"),
    ("editorAdvanced", "mail", "editorOptions.advanced"),
    ("readingPane", "mail", "readingPane"),
    ("sendReceive", "advanced", "sendReceive"),
];

/// Option keys `options.set` / `options.get` understand.
pub const KEYS: [&str; 9] = [
    "view.darkMode",
    "view.zoom",
    "view.statusBar",
    "view.simplifiedRibbon",
    "view.ribbonCollapsed",
    "view.favorites",
    "view.readingPane",
    "view.folderPane",
    "search.scope",
];

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec {
        id,
        label,
        location: "File › Options",
        shortcut: "",
        params,
        mutates: false,
        enabled: always,
        run,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "options.open",
            "Options",
            r#"{"page"?: "general"|"mail"|…|"signatures"|"editorAdvanced"|"language"}"#,
            open,
        ),
        spec("options.set", "Set Option", r#"{"key": string, "value": any}"#, set),
        spec("options.get", "Get Option", r#"{"key"?: string} (no key = all)"#, get),
    ]
}

fn open(s: &mut Session, p: &Value) -> CmdResult {
    let want = opt_str(p, "page").unwrap_or("general").trim();
    let (page, section) = if let Some((_, page, section)) =
        ALIASES.iter().find(|(a, _, _)| a.eq_ignore_ascii_case(want))
    {
        (*page, Some(*section))
    } else if let Some((id, _)) =
        PAGES.iter().find(|(id, title)| id.eq_ignore_ascii_case(want) || title.eq_ignore_ascii_case(want))
    {
        (*id, None)
    } else {
        let known: Vec<&str> =
            PAGES.iter().map(|(id, _)| *id).chain(ALIASES.iter().map(|(a, _, _)| *a)).collect();
        return Err(CmdError::Params(format!("unknown options page `{want}` (known: {})", known.join(", "))));
    };
    dialog(s, "options", json!({"page": page, "section": section}))
}

fn reading_pane_name(r: ReadingPanePos) -> &'static str {
    match r {
        ReadingPanePos::Right => "right",
        ReadingPanePos::Bottom => "bottom",
        ReadingPanePos::Off => "off",
    }
}

fn folder_pane_name(f: FolderPaneState) -> &'static str {
    match f {
        FolderPaneState::Normal => "normal",
        FolderPaneState::Minimized => "minimized",
        FolderPaneState::Off => "off",
    }
}

const SCOPES: [(SearchScope, &str); 5] = [
    (SearchScope::CurrentFolder, "currentFolder"),
    (SearchScope::Subfolders, "subfolders"),
    (SearchScope::CurrentMailbox, "currentMailbox"),
    (SearchScope::AllMailboxes, "allMailboxes"),
    (SearchScope::AllOutlookItems, "allOutlookItems"),
];

fn value_of(s: &Session, key: &str) -> Option<Value> {
    let v = &s.view;
    Some(match key {
        "view.darkMode" => json!(v.dark),
        "view.zoom" => json!(v.zoom),
        "view.statusBar" => json!(v.status_bar),
        "view.simplifiedRibbon" => json!(v.simplified_ribbon),
        "view.ribbonCollapsed" => json!(v.ribbon_collapsed),
        "view.favorites" => json!(v.favorites_visible),
        "view.readingPane" => json!(reading_pane_name(v.reading_pane)),
        "view.folderPane" => json!(folder_pane_name(v.folder_pane)),
        "search.scope" => {
            json!(SCOPES.iter().find(|(sc, _)| *sc == v.search_scope).map_or("currentFolder", |(_, n)| *n))
        }
        _ => return None,
    })
}

fn want_bool(key: &str, v: &Value) -> Result<bool, CmdError> {
    v.as_bool().ok_or_else(|| CmdError::Params(format!("`{key}` takes true/false")))
}

fn want_str<'a>(key: &str, v: &'a Value) -> Result<&'a str, CmdError> {
    v.as_str().ok_or_else(|| CmdError::Params(format!("`{key}` takes a string")))
}

fn set(s: &mut Session, p: &Value) -> CmdResult {
    let key = req_str(p, "key")?;
    let v = p.get("value").ok_or_else(|| CmdError::Params("`value` required".into()))?;
    match key {
        "view.darkMode" => s.view.dark = want_bool(key, v)?,
        "view.statusBar" => s.view.status_bar = want_bool(key, v)?,
        "view.simplifiedRibbon" => s.view.simplified_ribbon = want_bool(key, v)?,
        "view.ribbonCollapsed" => s.view.ribbon_collapsed = want_bool(key, v)?,
        "view.favorites" => s.view.favorites_visible = want_bool(key, v)?,
        "view.zoom" => {
            s.view.zoom = v
                .as_u64()
                .filter(|z| (10..=500).contains(z))
                .and_then(|z| u16::try_from(z).ok())
                .ok_or_else(|| {
                CmdError::Params("`view.zoom` takes a percentage from 10 to 500".into())
            })?;
        }
        "view.readingPane" => {
            s.view.reading_pane = match want_str(key, v)? {
                "right" => ReadingPanePos::Right,
                "bottom" => ReadingPanePos::Bottom,
                "off" => ReadingPanePos::Off,
                o => return Err(CmdError::Params(format!("`view.readingPane`: unknown value `{o}`"))),
            };
        }
        "view.folderPane" => {
            s.view.folder_pane = match want_str(key, v)? {
                "normal" => FolderPaneState::Normal,
                "minimized" => FolderPaneState::Minimized,
                "off" => FolderPaneState::Off,
                o => return Err(CmdError::Params(format!("`view.folderPane`: unknown value `{o}`"))),
            };
        }
        "search.scope" => {
            let name = want_str(key, v)?;
            s.view.search_scope = SCOPES
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                .map(|(sc, _)| *sc)
                .ok_or_else(|| CmdError::Params(format!("`search.scope`: unknown value `{name}`")))?;
        }
        _ => return Err(CmdError::Disabled(format!("option `{key}` is not supported yet"))),
    }
    Ok(json!({"key": key, "value": value_of(s, key)}))
}

fn get(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "key") {
        Some(key) => value_of(s, key)
            .map(|v| json!({"key": key, "value": v}))
            .ok_or_else(|| CmdError::Disabled(format!("option `{key}` is not supported yet"))),
        None => {
            let all: serde_json::Map<String, Value> =
                KEYS.iter().filter_map(|k| value_of(s, k).map(|v| ((*k).to_owned(), v))).collect();
            Ok(Value::Object(all))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Notice;
    use crate::cmd::rules::tests::session;

    #[test]
    fn open_pages_and_aliases() {
        let (mut s, _, _) = session();
        let r = s.run("options.open", &json!({})).unwrap();
        assert_eq!(r["params"]["page"], "general");
        let r = s.run("options.open", &json!({"page": "signatures"})).unwrap();
        assert_eq!(
            (r["params"]["page"].as_str(), r["params"]["section"].as_str()),
            (Some("mail"), Some("signatures"))
        );
        let r = s.run("options.open", &json!({"page": "Trust Center"})).unwrap();
        assert_eq!(r["params"]["page"], "trustCenter");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "options"));
        assert!(matches!(s.run("options.open", &json!({"page": "nope"})), Err(CmdError::Params(_))));
        // File › Options goes through the same command.
        assert_eq!(
            s.run("file.options", &json!({"page": "language"})).unwrap()["params"]["page"],
            "language"
        );
    }

    #[test]
    fn set_and_get() {
        let (mut s, _, _) = session();
        s.run("options.set", &json!({"key": "view.darkMode", "value": true})).unwrap();
        assert!(s.view.dark);
        s.run("options.set", &json!({"key": "view.readingPane", "value": "bottom"})).unwrap();
        assert_eq!(s.view.reading_pane, ReadingPanePos::Bottom);
        s.run("options.set", &json!({"key": "search.scope", "value": "allMailboxes"})).unwrap();
        assert_eq!(s.view.search_scope, SearchScope::AllMailboxes);
        assert!(matches!(
            s.run("options.set", &json!({"key": "view.zoom", "value": 5})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("options.set", &json!({"key": "view.darkMode", "value": "yes"})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("options.set", &json!({"key": "mail.bogus", "value": 1})),
            Err(CmdError::Disabled(_))
        ));
        assert_eq!(s.run("options.get", &json!({"key": "view.readingPane"})).unwrap()["value"], "bottom");
        let all = s.run("options.get", &json!({})).unwrap();
        assert_eq!(all.as_object().unwrap().len(), KEYS.len());
    }
}
