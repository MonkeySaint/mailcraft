//! `options.*`: open the Options window at a page, and read / write / reset every setting of the profile
//! settings store ([`settings`]) plus the few the session keeps in its view state. Unknown keys and invalid
//! values are refused, never stored. Changes are saved to the profile's `settings.json` by the session.

pub use crate::settings;

use serde_json::{Value, json};

use crate::cmd::rules::dialog;
use crate::cmd::util::{opt_bool, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::{FolderPaneState, ReadingPanePos};

pub use settings::{Def, Effect, Kind, Settings};

/// Options pages in window order (`plan/reference/options.md`): (id, title).
pub const PAGES: [(&str, &str); 14] = settings::PAGES;

/// Pages opened from elsewhere that land on a section of a page: (alias, page, section). Set Default Paste
/// (Message › Paste) opens `editorAdvanced`: the paste defaults.
const ALIASES: [(&str, &str, &str); 9] = [
    ("signatures", "mail", "signatures"),
    ("editorAdvanced", "mail", settings::EDITOR_PASTE),
    ("setDefaultPaste", "mail", settings::EDITOR_PASTE),
    ("editorProofing", "mail", settings::EDITOR_PROOFING),
    ("writingStyle", "mail", settings::EDITOR_WRITING_STYLE),
    ("readingPane", "mail", "readingPane"),
    ("sendReceive", "advanced", "sendReceive"),
    ("workHours", "calendar", "Work time"),
    ("translation", "language", "Translation"),
];

/// Keys held only by the session's view state (not listed in the Options window).
pub const VIEW_KEYS: [&str; 5] =
    ["view.darkMode", "view.zoom", "view.readingPane", "view.folderPane", "sendReceive.workOffline"];

/// Older key names still accepted: (old, current).
const RENAMED: [(&str, &str); 2] = [
    ("sendReceive.autoMinutes", "advanced.sendReceiveMinutes"),
    ("sendReceive.sendImmediately", "advanced.sendImmediately"),
];

/// Every key `options.get` lists: the view keys, then every setting.
pub fn all_keys() -> Vec<&'static str> {
    VIEW_KEYS.iter().copied().chain(settings::DEFS.iter().map(|d| d.key)).collect()
}

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
            r#"{"page"?: "general"|"mail"|…|"signatures"|"editorAdvanced"|"setDefaultPaste"|"editorProofing"|"writingStyle"|"language"}"#,
            open,
        ),
        spec("options.set", "Set Option", r#"{"key": string, "value": any}"#, set),
        spec(
            "options.get",
            "Get Option",
            r#"{"key"?: string, "page"?: string, "meta"?: bool} (no key = all, `meta` adds labels, defaults, kinds)"#,
            get,
        ),
        spec("options.reset", "Reset Options", r#"{"key"?: string} (no key = every option)"#, reset),
        spec(
            "options.translator",
            "Translation Server",
            r#"{"url"?: string ("" = none), "apiKey"?: string|null (null or "" removes it), "test"?: bool (translate a sample through `url`/`apiKey` as given, else the stored ones; stores nothing)}"#,
            translator,
        ),
    ]
}

/// Longest translation server API key (characters).
const MAX_API_KEY: usize = 512;

/// Options › Language › Translation server. Without `test`: `url` sets the server address (checked first;
/// "" means none) and `apiKey` stores the API key in the secret store (`null` or "" removes it). With
/// `test`: translates a short sample through the server given (`url`, `apiKey`), else the stored one, and
/// reports the result; nothing is stored. The key itself is never returned.
fn translator(s: &mut Session, p: &Value) -> CmdResult {
    let url = opt_str(p, "url").map(|u| u.trim().to_owned());
    let key: Option<Option<String>> = match p.get("apiKey") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(k)) => {
            let k = k.trim();
            if k.chars().count() > MAX_API_KEY || k.chars().any(char::is_control) {
                return Err(CmdError::Params("the API key is not valid".into()));
            }
            Some((!k.is_empty()).then(|| k.to_owned()))
        }
        Some(_) => return Err(CmdError::Params("`apiKey` must be a string or null".into())),
    };
    if let Some(u) = &url
        && !u.is_empty()
    {
        check_translator_url(s, u)?;
    }
    if opt_bool(p, "test") == Some(true) {
        return test_translator(s, url.as_deref(), key);
    }
    // A key needs a server to belong to: refused before anything changes, so the call is all or nothing.
    let server = url.clone().unwrap_or_else(|| s.settings.text(crate::translate::URL_SETTING));
    if matches!(key, Some(Some(_))) && server.trim().is_empty() {
        return Err(CmdError::Params("enter the address of the translation server first".into()));
    }
    if let Some(u) = &url {
        set_option(s, crate::translate::URL_SETTING, &json!(u))?;
    }
    if let Some(k) = key {
        // The key belongs to the server now entered, and is only ever sent there.
        let server = s.settings.text(crate::translate::URL_SETTING);
        let r = match k {
            Some(k) => crate::translate::store_key(s, &server, &k),
            None => s.secrets.remove(crate::translate::KEY_SECRET),
        };
        r.map_err(|e| CmdError::Failed(format!("the API key could not be stored: {e}")))?;
    }
    Ok(json!({
        "url": s.settings.text(crate::translate::URL_SETTING),
        "apiKey": crate::translate::has_key(s),
        "keyStore": s.secrets.location(),
    }))
}

/// The translation server address changed: a stored API key that was not entered for the new server is
/// removed (it would never be sent there anyway, see [`crate::translate::key_for`]); the user enters the new
/// server's key if it needs one. This also catches a key left behind by a path that cleared the address
/// without removing it.
fn forget_key_of_old_server(s: &mut Session) {
    let now = s.settings.text(crate::translate::URL_SETTING);
    if !crate::translate::has_foreign_key(s, &now) {
        return;
    }
    match s.secrets.remove(crate::translate::KEY_SECRET) {
        Ok(()) => s.notify(crate::Notice::Info(
            "The translation server changed, so the API key kept for the old one was removed. Enter the new \
             server's key if it needs one."
                .into(),
        )),
        Err(e) => s.notify(crate::Notice::Error(format!(
            "The translation server changed but the old server's API key could not be removed: {e}"
        ))),
    }
}

/// Refuses a translation server address that cannot be used (not http(s), or plain http to another computer).
#[cfg(not(target_arch = "wasm32"))]
fn check_translator_url(_s: &Session, url: &str) -> Result<(), CmdError> {
    let o = mailcraft_net::translate::LibreTranslate::default_options();
    mailcraft_net::translate::LibreTranslate::new(url, None, o)
        .map(|_| ())
        .map_err(|e| CmdError::Params(format!("translation server: {e}")))
}

#[cfg(target_arch = "wasm32")]
fn check_translator_url(_s: &Session, _url: &str) -> Result<(), CmdError> {
    Err(CmdError::Disabled("a translation server cannot be used in the browser".into()))
}

/// A sample sentence the Test button translates.
#[cfg(not(target_arch = "wasm32"))]
const TEST_TEXT: &str = "Thank you for your message. I will reply tomorrow.";

#[cfg(not(target_arch = "wasm32"))]
fn test_translator(s: &mut Session, url: Option<&str>, key: Option<Option<String>>) -> CmdResult {
    let stored = s.settings.text(crate::translate::URL_SETTING);
    let url = url.map_or_else(|| stored.clone(), str::to_owned);
    if url.trim().is_empty() {
        return Err(CmdError::Params("enter the address of a translation server first".into()));
    }
    // The stored key belongs to the server it was entered for: a different address is tested with the key
    // typed for it (or none), never with that one.
    let key = match key {
        Some(k) => k,
        None => crate::translate::key_for(s, &url),
    };
    let options = s
        .translator_options
        .clone()
        .unwrap_or_else(mailcraft_net::translate::LibreTranslate::default_options);
    let lt = mailcraft_net::translate::LibreTranslate::new(&url, key.as_deref(), options)
        .map_err(|e| CmdError::Params(format!("translation server: {e}")))?;
    let mut to = s.settings.text("language.translateTo");
    if to.split(['-', '_']).next().is_some_and(|p| p.eq_ignore_ascii_case("en")) {
        to = "fr".into();
    }
    let work = move || {
        let (text, _) = lt
            .translate(TEST_TEXT, &to)
            .map_err(|e| format!("the translation server did not translate: {e}"))?;
        Ok(
            json!({"ok": true, "to": to, "sample": TEST_TEXT, "text": text.chars().take(500).collect::<String>()}),
        )
    };
    if s.background_translate {
        // The desktop app: the answer comes later (`translate::take_test_result`), the window stays usable.
        crate::translate::start_test(s, work).map_err(CmdError::Failed)?;
        return Ok(json!({"testing": true}));
    }
    work().map_err(CmdError::Failed)
}

#[cfg(target_arch = "wasm32")]
fn test_translator(_s: &mut Session, _url: Option<&str>, _key: Option<Option<String>>) -> CmdResult {
    Err(CmdError::Disabled("a translation server cannot be used in the browser".into()))
}

fn open(s: &mut Session, p: &Value) -> CmdResult {
    let want = opt_str(p, "page").unwrap_or("general").trim();
    let (page, section) =
        if let Some((_, page, section)) = ALIASES.iter().find(|(a, _, _)| a.eq_ignore_ascii_case(want)) {
            (*page, Some(*section))
        } else if let Some((id, _)) =
            PAGES.iter().find(|(id, title)| id.eq_ignore_ascii_case(want) || title.eq_ignore_ascii_case(want))
        {
            (*id, None)
        } else {
            let known: Vec<&str> =
                PAGES.iter().map(|(id, _)| *id).chain(ALIASES.iter().map(|(a, _, _)| *a)).collect();
            let shown: String = want.chars().take(40).collect();
            return Err(CmdError::Params(format!(
                "unknown options page `{shown}` (known: {})",
                known.join(", ")
            )));
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

fn canonical(key: &str) -> &str {
    RENAMED.iter().find(|(old, _)| *old == key).map_or(key, |(_, new)| new)
}

/// Current value of an option (view-held keys from the session, mirrored settings from where the session
/// keeps them, the rest from the settings store); `None` for unknown keys.
pub fn option_value(s: &Session, key: &str) -> Option<Value> {
    let v = &s.view;
    let key = canonical(key);
    Some(match key {
        "view.darkMode" => json!(v.dark),
        "view.zoom" => json!(v.zoom),
        "view.statusBar" => json!(v.status_bar),
        "view.simplifiedRibbon" => json!(v.simplified_ribbon),
        "view.ribbonCollapsed" => json!(v.ribbon_collapsed),
        "view.favorites" => json!(v.favorites_visible),
        "view.readingPane" => json!(reading_pane_name(v.reading_pane)),
        "view.folderPane" => json!(folder_pane_name(v.folder_pane)),
        "sendReceive.workOffline" => json!(v.work_offline),
        "advanced.sendImmediately" => json!(v.send_immediately),
        "advanced.sendReceiveMinutes" => json!(v.auto_send_receive_minutes),
        "search.includeDeleted" => json!(s.search.include_deleted),
        "calendar.timeZone" => json!(s.tz_offset_min),
        "search.scope" => json!(
            crate::session::SCOPE_NAMES
                .iter()
                .find(|(sc, _)| *sc == v.search_scope)
                .map_or("currentFolder", |(_, n)| *n)
        ),
        _ => return s.settings.get(key),
    })
}

fn want_bool(key: &str, v: &Value) -> Result<bool, CmdError> {
    v.as_bool().ok_or_else(|| CmdError::Params(format!("`{key}` takes true/false")))
}

fn want_str<'a>(key: &str, v: &'a Value) -> Result<&'a str, CmdError> {
    v.as_str().ok_or_else(|| CmdError::Params(format!("`{key}` takes a string")))
}

/// Sets an option (validated), applies it where the session holds it and saves the settings file. Returns
/// the stored (normalised) value.
pub fn set_option(s: &mut Session, key: &str, v: &Value) -> Result<Value, CmdError> {
    let key = canonical(key);
    match key {
        "view.darkMode" => {
            let dark = want_bool(key, v)?;
            s.view.dark = dark;
            s.settings.set("general.theme", &json!(if dark { "dark" } else { "light" }))?;
        }
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
                o => {
                    return Err(CmdError::Params(format!("`view.readingPane`: unknown value `{}`", clip(o))));
                }
            };
        }
        "view.folderPane" => {
            s.view.folder_pane = match want_str(key, v)? {
                "normal" => FolderPaneState::Normal,
                "minimized" => FolderPaneState::Minimized,
                "off" => FolderPaneState::Off,
                o => return Err(CmdError::Params(format!("`view.folderPane`: unknown value `{}`", clip(o)))),
            };
        }
        "sendReceive.workOffline" => s.view.work_offline = want_bool(key, v)?,
        _ => {
            if settings::def(key).is_none() {
                return Err(CmdError::Disabled(format!("option `{}` is not supported", clip(key))));
            }
            let translator_changed = key == crate::translate::URL_SETTING;
            if key == crate::translate::URL_SETTING
                && let Some(u) = v.as_str().map(str::trim).filter(|u| !u.is_empty())
            {
                check_translator_url(s, u)?;
            }
            s.settings.set(key, v)?;
            s.apply_setting(key);
            if translator_changed {
                forget_key_of_old_server(s);
            }
        }
    }
    if let Err(e) = s.save_settings() {
        log::error!("{e}");
        s.notify(crate::Notice::Error(format!("The setting was changed but could not be saved: {e}")));
    }
    option_value(s, key).ok_or_else(|| CmdError::Failed(format!("option `{}` has no value", clip(key))))
}

fn clip(t: &str) -> String {
    t.chars().take(60).collect()
}

fn set(s: &mut Session, p: &Value) -> CmdResult {
    let key = req_str(p, "key")?;
    let v = p.get("value").ok_or_else(|| CmdError::Params("`value` required".into()))?;
    let value = set_option(s, key, v)?;
    Ok(json!({"key": canonical(key), "value": value}))
}

fn kind_json(k: &Kind) -> Value {
    match k {
        Kind::Bool(_) => json!({"type": "bool"}),
        Kind::Int(_, min, max) => json!({"type": "int", "min": min, "max": max}),
        Kind::Choice(_, choices) => {
            json!({"type": "choice", "choices": choices.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()})
        }
        Kind::Text(_, max) => json!({"type": "text", "maxChars": max}),
        Kind::Time(_) => json!({"type": "time"}),
        Kind::TimeRange(_) => json!({"type": "timeRange"}),
        Kind::Days(_) => {
            json!({"type": "days", "choices": settings::WEEKDAYS.iter().map(|(id, _)| *id).collect::<Vec<_>>()})
        }
        Kind::List(max) => json!({"type": "list", "maxEntries": max}),
        Kind::Object(max) => json!({"type": "object", "maxBytes": max}),
    }
}

fn meta(s: &Session, d: &Def) -> Value {
    json!({
        "key": d.key,
        "page": d.page,
        "section": d.section,
        "label": d.label,
        "kind": kind_json(&d.kind),
        "default": d.default_value(),
        "value": option_value(s, d.key),
        "effect": d.effect,
    })
}

fn get(s: &mut Session, p: &Value) -> CmdResult {
    let with_meta = opt_bool(p, "meta").unwrap_or(false);
    if let Some(key) = opt_str(p, "key") {
        let key = canonical(key);
        let value = option_value(s, key)
            .ok_or_else(|| CmdError::Disabled(format!("option `{}` is not supported", clip(key))))?;
        return Ok(match settings::def(key) {
            Some(d) if with_meta => meta(s, d),
            _ => json!({"key": key, "value": value}),
        });
    }
    if let Some(page) = opt_str(p, "page") {
        let defs = settings::page_defs(page);
        if defs.is_empty() {
            return Err(CmdError::Params(format!("no options on page `{}`", clip(page))));
        }
        let rows: Vec<Value> = defs.iter().map(|(_, d)| meta(s, d)).collect();
        return Ok(json!({"page": page, "options": rows}));
    }
    if with_meta {
        let rows: Vec<Value> = settings::DEFS.iter().map(|d| meta(s, d)).collect();
        let (applied, total) = settings::effect_counts();
        return Ok(json!({"options": rows, "applied": applied, "total": total}));
    }
    let all: serde_json::Map<String, Value> =
        all_keys().into_iter().filter_map(|k| option_value(s, k).map(|v| (k.to_owned(), v))).collect();
    Ok(Value::Object(all))
}

fn reset(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "key") {
        Some(key) => {
            let key = canonical(key);
            let d = settings::def(key)
                .ok_or_else(|| CmdError::Disabled(format!("option `{}` is not supported", clip(key))))?;
            let value = set_option(s, key, &d.default_value())?;
            Ok(json!({"key": key, "value": value}))
        }
        None => {
            s.settings.reset_all();
            // The translation server address is gone, and with it the key kept for that server.
            if let Err(e) = s.secrets.remove(crate::translate::KEY_SECRET) {
                log::error!("the translation server's API key could not be removed: {e}");
            }
            for d in settings::DEFS.iter().filter(|d| d.view) {
                set_option(s, d.key, &d.default_value())?;
            }
            s.apply_settings();
            if let Err(e) = s.save_settings() {
                log::error!("{e}");
            }
            Ok(json!({"reset": true}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use crate::compose::ComposeState;
    use crate::{Notice, SearchScope};
    use mailcraft_model::{BodyFormat, Doc, FolderRole, Importance};

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
    fn set_default_paste_opens_the_paste_defaults() {
        let (mut s, _, _) = session();
        for alias in ["editorAdvanced", "setDefaultPaste"] {
            let r = s.run("options.open", &json!({"page": alias})).unwrap();
            assert_eq!(r["params"]["page"], "mail");
            assert_eq!(r["params"]["section"], settings::EDITOR_PASTE);
        }
        // The section holds the three paste defaults.
        let paste: Vec<&str> = settings::page_defs("mail")
            .into_iter()
            .filter(|(section, _)| *section == settings::EDITOR_PASTE)
            .map(|(_, d)| d.key)
            .collect();
        assert_eq!(
            paste,
            [
                "mail.editor.pasteWithinMessage",
                "mail.editor.pasteBetweenMessages",
                "mail.editor.pasteFromOtherPrograms"
            ]
        );
        let r = s.run("options.open", &json!({"page": "editorProofing"})).unwrap();
        assert_eq!(r["params"]["section"], settings::EDITOR_PROOFING);
    }

    #[test]
    fn set_and_get() {
        let (mut s, _, _) = session();
        s.run("options.set", &json!({"key": "view.darkMode", "value": true})).unwrap();
        assert!(s.view.dark);
        assert_eq!(s.settings.text("general.theme"), "dark");
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
        assert_eq!(all.as_object().unwrap().len(), all_keys().len());
        s.run("options.set", &json!({"key": "sendReceive.autoMinutes", "value": 5})).unwrap();
        assert_eq!(s.view.auto_send_receive_minutes, 5);
        assert_eq!(s.settings.int("advanced.sendReceiveMinutes"), 5);
        assert!(matches!(
            s.run("options.set", &json!({"key": "sendReceive.autoMinutes", "value": 99_999})),
            Err(CmdError::Params(_))
        ));
        assert_eq!(
            s.run("options.get", &json!({"key": "sendReceive.sendImmediately"})).unwrap()["value"],
            true
        );
        let r = s.run("options.set", &json!({"key": "calendar.workWeek", "value": "mon,tue"})).unwrap();
        assert_eq!(r["value"], json!(["mon", "tue"]));
        let m = s.run("options.get", &json!({"key": "calendar.workWeek", "meta": true})).unwrap();
        assert_eq!(m["page"], "calendar");
        assert_eq!(m["default"], json!(["mon", "tue", "wed", "thu", "fri"]));
        let page = s.run("options.get", &json!({"page": "people"})).unwrap();
        assert!(page["options"].as_array().unwrap().iter().any(|o| o["key"] == "people.fileAs"));
        let all = s.run("options.get", &json!({"meta": true})).unwrap();
        assert!(all["applied"].as_u64().unwrap() >= 60, "{}", all["applied"]);
    }

    #[test]
    fn reset_one_and_all() {
        let (mut s, _, _) = session();
        s.run("options.set", &json!({"key": "people.fileAs", "value": "company"})).unwrap();
        s.run("options.set", &json!({"key": "view.statusBar", "value": false})).unwrap();
        s.run("options.reset", &json!({"key": "people.fileAs"})).unwrap();
        assert_eq!(s.settings.file_as(), "lastFirst");
        s.run("options.set", &json!({"key": "advanced.sendImmediately", "value": false})).unwrap();
        assert!(!s.view.send_immediately);
        s.run("options.reset", &json!({})).unwrap();
        assert!(s.view.status_bar && s.view.send_immediately);
        assert!(s.settings.overrides().is_empty());
        assert!(s.run("options.reset", &json!({"key": "x.y"})).is_err());
    }

    #[test]
    fn mirrored_settings_follow_commands() {
        let (mut s, _, _) = session();
        // Commands that change the session directly keep the settings in step.
        s.view.send_immediately = false;
        s.tz_offset_min = 60;
        s.run("options.get", &json!({})).unwrap();
        assert!(!s.settings.bool("advanced.sendImmediately"));
        assert_eq!(s.settings.time_zone_offset_min(), 60);
        s.run("options.set", &json!({"key": "calendar.timeZone", "value": -300})).unwrap();
        assert_eq!(s.tz_offset_min, -300);
        s.run("options.set", &json!({"key": "general.theme", "value": "system"})).unwrap();
        s.view.dark = true;
        s.run("options.get", &json!({})).unwrap();
        assert_eq!(s.settings.text("general.theme"), "system", "system theme is not overwritten");
    }

    #[test]
    fn compose_defaults_follow_the_options() {
        let (mut s, _, _) = session();
        for (k, v) in [
            ("mail.compose.defaultFormat", json!("plain")),
            ("mail.send.defaultImportance", json!("high")),
            ("mail.compose.showBcc", json!(true)),
            ("mail.tracking.requestRead", json!(true)),
            ("mail.save.sentCopies", json!(false)),
            ("mail.compose.fontFamily", json!("Carlito")),
            ("mail.compose.fontSizePt", json!(14)),
            ("language.authoring", json!("fr-CA")),
        ] {
            s.run("options.set", &json!({"key": k, "value": v})).unwrap();
        }
        let cid = s.run("mail.newEmail", &json!({})).unwrap()["compose"].as_u64().unwrap();
        let c = &s.composes[&cid];
        assert_eq!(c.format, BodyFormat::PlainText);
        assert_eq!(c.importance, Importance::High);
        assert!(c.show_bcc && c.request_read_receipt);
        assert_eq!(c.save_sent, crate::compose::SaveSentTo::DoNotSave);
        assert_eq!(c.body.base.font_family.as_deref(), Some("Carlito"));
        assert_eq!(c.body.base.size_pt, Some(14.0));
        assert_eq!(c.language.as_deref(), Some("fr-CA"));
        // Replies open in their own window when asked to.
        s.run("options.set", &json!({"key": "mail.reply.newWindow", "value": true})).unwrap();
        let reply = s.open_compose(ComposeState {
            kind: crate::compose::ComposeKind::Reply,
            inline: true,
            ..Default::default()
        });
        assert!(!s.composes[&reply].inline);
        // Turning Bcc off in a message sticks too.
        s.run("compose.showBcc", &json!({"compose": cid, "on": false})).unwrap();
        assert!(!s.settings.bool("mail.compose.showBcc"));
    }

    #[test]
    fn attachment_reminder_asks_before_sending() {
        let (mut s, _, _) = session();
        let cid = s.open_compose(ComposeState {
            to: "ann@example.com".into(),
            subject: "Report".into(),
            body: Doc::from_plain_text("Please see the attached report."),
            ..Default::default()
        });
        let r = s.run("compose.send", &json!({"compose": cid})).unwrap();
        assert_eq!(r["reason"], "attachmentReminder");
        assert!(s.composes.contains_key(&cid), "not sent");
        let Some(Notice::OpenDialog(name, params)) = s.notices.last().cloned() else { panic!("no dialog") };
        assert_eq!(name, "confirm");
        assert_eq!(params["command"], "compose.send");
        // "Yes" in the dialog sends with the reminder off: it gets past the check.
        let r = s.run("compose.send", &params["params"]);
        assert!(r.map(|v| v["reason"] != "attachmentReminder").unwrap_or(true));
        // Option off: no reminder.
        let cid = s.open_compose(ComposeState {
            to: "ann@example.com".into(),
            body: Doc::from_plain_text("attached"),
            ..Default::default()
        });
        s.run("options.set", &json!({"key": "mail.send.attachmentReminder", "value": false})).unwrap();
        let r = s.run("compose.send", &json!({"compose": cid}));
        assert!(r.map(|v| v["reason"] != "attachmentReminder").unwrap_or(true));
    }

    #[test]
    fn mark_read_on_delete() {
        let mut s = crate::sample::sample_session();
        let order = crate::queries::visible_item_ids(&s);
        let a = order[0];
        s.mailbox.set_read(&[a], false);
        s.run("options.set", &json!({"key": "mail.other.markReadOnDelete", "value": true})).unwrap();
        s.run("nav.selectItem", &json!({"id": a.0})).unwrap();
        s.mailbox.set_read(&[a], false);
        s.run("mail.delete", &json!({"ids": [a.0]})).unwrap();
        assert!(s.mailbox.mail.get(&a).is_none_or(|m| m.read));
        let deleted =
            s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::DeletedItems)).unwrap().id;
        assert!(s.mailbox.mail.get(&a).is_none_or(|m| m.folder == deleted));
    }

    #[test]
    fn profile_settings_persist_and_apply_on_open() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        {
            let mut s = Session::open(dir.path()).unwrap();
            s.run("options.set", &json!({"key": "mail.send.ctrlEnterSends", "value": false})).unwrap();
            s.run("options.set", &json!({"key": "view.zoom", "value": 150})).unwrap();
            s.run("options.set", &json!({"key": "advanced.sendImmediately", "value": false})).unwrap();
            s.run("options.set", &json!({"key": "advanced.startFolder", "value": "Sent Items"})).unwrap();
            s.run("options.set", &json!({"key": "advanced.emptyDeletedOnExit", "value": true})).unwrap();
            s.search.recent = vec!["invoice".into()];
            s.view.collapsed_groups.insert(s.selection.folder, vec!["Older".into()]);
            s.run("app.toggleRibbonMode", &json!({"mode": "classic"})).unwrap();
            // Something in Deleted Items, emptied on exit.
            let deleted =
                s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::DeletedItems)).map(|f| f.id);
            if let Some(d) = deleted {
                s.mailbox.create_folder(d, "Old stuff").unwrap();
            }
            s.run("file.exit", &json!({})).unwrap();
        }
        assert!(dir.path().join("settings.json").exists());
        let s = Session::open(dir.path()).unwrap();
        assert!(!s.settings.bool("mail.send.ctrlEnterSends"));
        assert_eq!(s.view.zoom, 150);
        assert!(!s.view.send_immediately, "mirrored into the view state");
        assert!(!s.view.simplified_ribbon, "view state persisted");
        assert_eq!(s.search.recent, ["invoice"]);
        assert_eq!(s.view.collapsed_groups.values().next().map(Vec::len), Some(1));
        assert_eq!(s.mailbox.folder(s.selection.folder).and_then(|f| f.role), Some(FolderRole::SentItems));
        let deleted =
            s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::DeletedItems)).unwrap().id;
        assert!(s.mailbox.folders.values().all(|f| f.parent != Some(deleted)), "emptied on exit");
        drop(s);
        // A broken file is set aside; defaults are used.
        std::fs::write(dir.path().join("settings.json"), b"{\"settings\": 5").unwrap();
        let s = Session::open(dir.path()).unwrap();
        assert!(s.settings.bool("mail.send.ctrlEnterSends"));
        assert!(dir.path().join("settings.json.bad").exists());
        assert!(s.notices.iter().any(|n| matches!(n, Notice::Error(t) if t.contains("settings"))));
    }

    #[test]
    fn module_views_and_quick_click_persist() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let tasks = {
            let mut s = Session::open(dir.path()).unwrap();
            let tasks = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Tasks)).unwrap().id;
            s.run("nav.selectFolder", &json!({"folder": tasks.0})).unwrap();
            s.run("view.changeView", &json!({"name": "Overdue"})).unwrap();
            s.run("mail.setQuickClickFlag", &json!({"kind": "nextWeek"})).unwrap();
            s.run("options.set", &json!({"key": "people.fileAs", "value": "firstLast"})).unwrap();
            tasks
        };
        let s = Session::open(dir.path()).unwrap();
        let v = crate::cmd::view::modules::state(&s, tasks, crate::cmd::view::modules::ModuleKind::Tasks);
        assert_eq!(v.view, "Overdue");
        assert_eq!(s.option("tasks.quickClickFlag"), Some(json!("nextWeek")));
        assert_eq!(s.settings.file_as(), "firstLast");
    }

    #[test]
    fn hostile_values_are_refused() {
        let (mut s, _, _) = session();
        for v in [
            json!(null),
            json!([]),
            json!({}),
            json!("\u{0}"),
            json!(1e300),
            json!(-1),
            json!("x".repeat(100_000)),
        ] {
            for d in settings::DEFS {
                // Errors are fine, panics are not; whatever is stored validates.
                let _ = s.run("options.set", &json!({"key": d.key, "value": v}));
                if let Some(cur) = s.option(d.key)
                    && !d.view
                {
                    assert_eq!(settings::normalize(d, &cur).unwrap(), cur, "{}", d.key);
                }
            }
        }
    }
}
