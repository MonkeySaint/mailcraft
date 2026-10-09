//! Search commands (`search.*`): the query in `ViewState::search`, its scope, refine tokens, and the search
//! tools. Also the in-memory query matcher ([`item_matches`], [`results`]) the list and search folders use
//! until the dedicated search crate lands.
//!
//! Query syntax: whitespace-separated terms, all of which must match. A term is free text (matched in
//! subject, people, preview and body) or `key:value` with `value` optionally in double quotes; a leading
//! `-` negates a term. Keys: `from`, `to`, `cc`, `tocc` (value `me` = one of the user's addresses),
//! `subject`, `body`, `category` (`any` / `none` / a name), `hasattachments`, `isflagged`, `isread`
//! (`yes` / `no`), `importance` (`high` / `normal` / `low`), `received` (`today`, `yesterday`,
//! `this week`, `last week`, `this month`, `last month`), `expires` (items carry no expiry yet: matches
//! nothing). A key with an empty value (`from:""`) is a placeholder being filled in and matches everything.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::folder::{current_folder, dialog};
use super::util::{opt_bool, opt_str};
use crate::{CmdError, CmdResult, CommandSpec, SearchScope, Session, always};
use mailcraft_model::dates::{LocalTime, local_day, weekday_of_day};
use mailcraft_model::{FolderId, FolderKind, FolderRole, Importance, ItemId, MailItem};

#[allow(clippy::too_many_arguments)]
fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    shortcut: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut, params, mutates, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "search.advancedFind",
            "Advanced Find",
            "Search › Options › Search Tools",
            "Mod+Shift+F",
            "",
            false,
            always,
            advanced_find,
        ),
        spec("search.close", "Close Search", "Search › Close", "", "", false, always, close),
        spec("search.focus", "Search", "Title bar search box", "F3 / Mod+E", "", false, always, focus),
        spec(
            "search.includeOlderResults",
            "Include Older Results",
            "Search › Options › Search Tools",
            "",
            r#"{"on"?:bool}"#,
            false,
            all_local,
            include_older_results,
        ),
        spec(
            "search.query",
            "Search",
            "Title bar search box",
            "",
            r#"{"text":string}"#,
            false,
            always,
            query,
        ),
        spec(
            "search.recent",
            "Recent Searches",
            "Search › Options",
            "",
            r#"{"index"?:number}"#,
            false,
            always,
            recent,
        ),
        spec(
            "search.refine",
            "More Filters",
            "Home › Find › Filter Email",
            "",
            r#"{"kind"?:"from|to|cc|subject|body|hasAttachments|categorized|sentTo|unread|flagged|important|expiringSoon|received","value"?:string}"#,
            false,
            always,
            refine,
        ),
        spec(
            "search.scope",
            "Search Scope",
            "Search › Scope",
            "Mod+Alt+A",
            r#"{"scope"?:"allMailboxes|currentMailbox|currentFolder|subfolders|allItems","includeDeleted"?:bool,"store"?:string,"on"?:bool}"#,
            false,
            always,
            scope,
        ),
        spec(
            "search.searchTools",
            "Search Tools",
            "Search › Options",
            "",
            r#"{"tool"?:"indexingStatus"}"#,
            false,
            always,
            search_tools,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Query terms

/// `key:value`, quoting the value when it contains spaces or is empty.
pub fn token(key: &str, value: &str) -> String {
    if value.is_empty() || value.chars().any(char::is_whitespace) || value.contains('"') {
        format!("{key}:\"{}\"", value.replace('"', ""))
    } else {
        format!("{key}:{value}")
    }
}

/// Splits a query into terms, keeping double-quoted runs (with their quotes) inside one term.
pub fn terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in query.chars() {
        if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Adds `term` to `query`, or removes it when already present (case-insensitive). Returns the new query and
/// whether the term is now present.
pub fn toggle_token(query: &str, term: &str) -> (String, bool) {
    let mut list = terms(query);
    let before = list.len();
    list.retain(|t| !t.eq_ignore_ascii_case(term));
    let on = list.len() == before;
    if on {
        list.push(term.to_owned());
    }
    (list.join(" "), on)
}

/// One parsed term.
struct Term {
    negated: bool,
    key: Option<String>,
    value: String,
}

const KEYS: [&str; 14] = [
    "from",
    "to",
    "cc",
    "tocc",
    "subject",
    "body",
    "category",
    "hasattachments",
    "isflagged",
    "isread",
    "importance",
    "received",
    "expires",
    "attachments",
];

fn parse_term(raw: &str) -> Term {
    let (negated, rest) = match raw.strip_prefix('-') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, raw),
    };
    if let Some((k, v)) = rest.split_once(':') {
        let k = k.to_ascii_lowercase();
        if KEYS.contains(&k.as_str()) {
            return Term { negated, key: Some(k), value: v.trim_matches('"').trim().to_lowercase() };
        }
    }
    Term { negated, key: None, value: rest.trim_matches('"').trim().to_lowercase() }
}

fn yes(v: &str) -> Option<bool> {
    match v {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn person_text(a: &mailcraft_model::Address) -> String {
    match &a.name {
        Some(n) => format!("{n} <{}>", a.addr).to_lowercase(),
        None => a.addr.to_lowercase(),
    }
}

fn people_match(s: &Session, list: &[&mailcraft_model::Address], value: &str) -> bool {
    if value == "me" {
        return list.iter().any(|a| s.me.iter().any(|m| m.trim().eq_ignore_ascii_case(a.addr.trim())));
    }
    list.iter().any(|a| person_text(a).contains(value))
}

fn received_match(s: &Session, m: &MailItem, value: &str) -> bool {
    let tz = s.tz_offset_min;
    let now = s.now();
    let day = local_day(m.received, tz);
    let today = local_day(now, tz);
    let week_start = today.saturating_sub(i64::from(weekday_of_day(today).0));
    let (ly, lm) = {
        let l = LocalTime::of(m.received, tz);
        (l.year, l.month)
    };
    let (ny, nm) = {
        let l = LocalTime::of(now, tz);
        (l.year, l.month)
    };
    let (py, pm) = if nm <= 1 { (ny.saturating_sub(1), 12) } else { (ny, nm.saturating_sub(1)) };
    match value {
        "today" => day == today,
        "yesterday" => day == today.saturating_sub(1),
        "this week" => day >= week_start && day <= today,
        "last week" => day >= week_start.saturating_sub(7) && day < week_start,
        "this month" => (ly, lm) == (ny, nm),
        "last month" => (ly, lm) == (py, pm),
        _ => false,
    }
}

fn term_matches(s: &Session, m: &MailItem, t: &Term) -> bool {
    let v = t.value.as_str();
    let Some(key) = t.key.as_deref() else {
        if v.is_empty() {
            return true;
        }
        return m.subject.to_lowercase().contains(v)
            || m.from.iter().chain(&m.to).chain(&m.cc).any(|a| person_text(a).contains(v))
            || m.preview.to_lowercase().contains(v)
            || m.body_text().to_lowercase().contains(v);
    };
    if v.is_empty() {
        return true;
    }
    match key {
        "from" => people_match(s, &m.from.iter().chain(m.sender.iter()).collect::<Vec<_>>(), v),
        "to" => people_match(s, &m.to.iter().collect::<Vec<_>>(), v),
        "cc" => people_match(s, &m.cc.iter().collect::<Vec<_>>(), v),
        "tocc" => people_match(s, &m.to.iter().chain(&m.cc).collect::<Vec<_>>(), v),
        "subject" => m.subject.to_lowercase().contains(v),
        "body" => m.body_text().to_lowercase().contains(v),
        "category" => match v {
            "any" => !m.categories.is_empty(),
            "none" => m.categories.is_empty(),
            _ => m.categories.iter().any(|c| c.trim().to_lowercase() == v),
        },
        "hasattachments" | "attachments" => yes(v).is_some_and(|y| y == m.has_attachments()),
        "isflagged" => yes(v).is_some_and(|y| y == m.is_flagged()),
        "isread" => yes(v).is_some_and(|y| y == m.read),
        "importance" => match v {
            "high" => m.importance == Importance::High,
            "normal" => m.importance == Importance::Normal,
            "low" => m.importance == Importance::Low,
            _ => false,
        },
        "received" => received_match(s, m, v),
        _ => false,
    }
}

/// True when `m` matches every term of `query` (an empty query matches everything).
pub fn item_matches(s: &Session, m: &MailItem, query: &str) -> bool {
    terms(query)
        .iter()
        .filter(|t| !matches!(t.as_str(), "AND" | "OR"))
        .map(|t| parse_term(t))
        .all(|t| term_matches(s, m, &t) != t.negated)
}

/// Folders the current scope searches. Mailbox-wide scopes leave out Deleted Items (and its subfolders)
/// and non-mail folders, except All Outlook Items, which covers every folder.
pub fn scope_folders(s: &Session) -> Vec<FolderId> {
    let sel = s.selection.folder;
    if !s.mailbox.folders.contains_key(&sel) {
        return Vec::new();
    }
    let deleted: BTreeSet<FolderId> = s
        .mailbox
        .accounts
        .iter()
        .filter_map(|a| s.mailbox.role_folder(a.id, FolderRole::DeletedItems))
        .flat_map(|d| s.mailbox.subtree(d))
        .collect();
    let mail_like = |f: &mailcraft_model::Folder| f.kind == FolderKind::Mail && !deleted.contains(&f.id);
    match s.view.search_scope {
        SearchScope::CurrentFolder => vec![sel],
        SearchScope::Subfolders => s.mailbox.subtree(sel),
        SearchScope::CurrentMailbox => {
            let account = s.mailbox.folder_account(sel);
            s.mailbox
                .folders
                .values()
                .filter(|f| Some(f.account) == account && mail_like(f))
                .map(|f| f.id)
                .collect()
        }
        SearchScope::AllMailboxes => {
            s.mailbox.folders.values().filter(|f| mail_like(f)).map(|f| f.id).collect()
        }
        SearchScope::AllOutlookItems => s.mailbox.folders.keys().copied().collect(),
    }
}

/// Mail items matching the current query in the current scope (newest first).
pub fn results(s: &Session) -> Vec<ItemId> {
    let folders: BTreeSet<FolderId> = scope_folders(s).into_iter().collect();
    let mut hits: Vec<&MailItem> = s
        .mailbox
        .mail
        .values()
        .filter(|m| folders.contains(&m.folder) && item_matches(s, m, &s.view.search))
        .collect();
    hits.sort_by_key(|m| std::cmp::Reverse((m.received.unix, m.id)));
    hits.into_iter().map(|m| m.id).collect()
}

fn scope_name(sc: SearchScope) -> &'static str {
    match sc {
        SearchScope::AllMailboxes => "allMailboxes",
        SearchScope::CurrentMailbox => "currentMailbox",
        SearchScope::CurrentFolder => "currentFolder",
        SearchScope::Subfolders => "subfolders",
        SearchScope::AllOutlookItems => "allItems",
    }
}

fn summary(s: &Session) -> Value {
    json!({
        "query": s.view.search,
        "scope": scope_name(s.view.search_scope),
        "results": if s.view.search.trim().is_empty() { 0 } else { results(s).len() },
    })
}

// ---------------------------------------------------------------------------------------------------------
// Enabled predicates

/// Every item is held locally (no server-side window), so there are never older results to include.
fn all_local(_s: &Session) -> Option<&'static str> {
    Some("all items are already available locally")
}

// ---------------------------------------------------------------------------------------------------------
// Commands

fn query(s: &mut Session, p: &Value) -> CmdResult {
    let text = opt_str(p, "text").ok_or_else(|| CmdError::Params("`text` (string) required".into()))?;
    s.view.search = text.trim().to_owned();
    Ok(summary(s))
}

fn focus(s: &mut Session, _p: &Value) -> CmdResult {
    let params = json!({ "query": s.view.search, "scope": scope_name(s.view.search_scope), "recent": [] });
    Ok(dialog(s, "searchBox", params))
}

fn close(s: &mut Session, _p: &Value) -> CmdResult {
    s.view.search.clear();
    Ok(json!({ "query": "" }))
}

fn advanced_find(s: &mut Session, _p: &Value) -> CmdResult {
    let folder = s.selection.folder.0;
    Ok(dialog(s, "advancedFind", json!({ "focus": "words", "folder": folder })))
}

/// Recent searches are not kept by the session yet, so the list is always the empty placeholder.
fn recent(_s: &mut Session, p: &Value) -> CmdResult {
    let items: Vec<String> = Vec::new();
    if let Some(i) = p.get("index").filter(|v| !v.is_null()) {
        return Err(CmdError::Params(format!("no recent search at index {i}")));
    }
    Ok(json!({ "items": items, "placeholder": "No Recent Searches" }))
}

fn include_older_results(_s: &mut Session, _p: &Value) -> CmdResult {
    Ok(json!({ "on": false }))
}

fn search_tools(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "tool") {
        None => Ok(dialog(s, "searchTools", json!({}))),
        Some("indexingStatus") => {
            // Search runs over the in-memory mailbox, so nothing is ever waiting to be indexed.
            let mut v = dialog(s, "indexingStatus", json!({ "remaining": 0 }));
            if let Some(o) = v.as_object_mut() {
                o.insert("remaining".into(), json!(0));
            }
            Ok(v)
        }
        Some(other) => Err(CmdError::Params(format!("unknown search tool `{other}`"))),
    }
}

fn scope(s: &mut Session, p: &Value) -> CmdResult {
    let include_deleted = opt_bool(p, "includeDeleted");
    let store = opt_str(p, "store");
    let sc = match opt_str(p, "scope") {
        Some("allMailboxes") => Some(SearchScope::AllMailboxes),
        Some("currentMailbox") => Some(SearchScope::CurrentMailbox),
        Some("currentFolder") => Some(SearchScope::CurrentFolder),
        Some("subfolders") => Some(SearchScope::Subfolders),
        Some("allItems") => Some(SearchScope::AllOutlookItems),
        Some(other) => return Err(CmdError::Params(format!("unknown scope `{other}`"))),
        // The bare command is the All Mailboxes shortcut.
        None if include_deleted.is_none() && store.is_none() => Some(SearchScope::AllMailboxes),
        None => None,
    };
    // Validate everything before changing state: a rejected call leaves the scope as it was.
    if let Some(name) = store {
        let known = s
            .mailbox
            .accounts
            .iter()
            .any(|a| a.display_name.eq_ignore_ascii_case(name) || a.address.eq_ignore_ascii_case(name));
        if !known {
            return Err(CmdError::Params(format!("no account or data file named `{name}`")));
        }
    }
    if let Some(sc) = sc {
        s.view.search_scope = sc;
    }
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut() {
        if let Some(d) = include_deleted {
            o.insert("includeDeleted".into(), json!(d));
        }
        if let Some(name) = store {
            o.insert("store".into(), json!(name));
            o.insert("on".into(), json!(opt_bool(p, "on").unwrap_or(true)));
        }
    }
    Ok(out)
}

fn refine(s: &mut Session, p: &Value) -> CmdResult {
    let Some(kind) = opt_str(p, "kind") else {
        let folder = current_folder(s, &Value::Null).map(|f| f.0).unwrap_or_default();
        let kinds = [
            "from",
            "to",
            "cc",
            "subject",
            "body",
            "hasAttachments",
            "categorized",
            "sentTo",
            "unread",
            "flagged",
            "important",
            "expiringSoon",
            "received",
        ];
        return Ok(dialog(
            s,
            "searchRefine",
            json!({ "folder": folder, "kinds": kinds, "query": s.view.search }),
        ));
    };
    let value = opt_str(p, "value").map(str::trim).unwrap_or_default();
    let term = match kind {
        "from" | "to" | "cc" | "subject" | "body" => token(kind, value),
        "hasAttachments" => "hasattachments:yes".to_owned(),
        "unread" => "isread:no".to_owned(),
        "flagged" => "isflagged:yes".to_owned(),
        "important" => "importance:high".to_owned(),
        "expiringSoon" => "expires:soon".to_owned(),
        "categorized" => match value {
            "" | "any" => "category:any".to_owned(),
            "none" => "category:none".to_owned(),
            name => token("category", name),
        },
        "sentTo" => match value {
            "" | "me" => "tocc:me".to_owned(),
            "notMe" => "-to:me".to_owned(),
            "other" => token("to", ""),
            addr => token("to", addr),
        },
        "received" => {
            let when = if value.is_empty() { "this week" } else { value };
            if !["today", "yesterday", "this week", "last week", "this month", "last month"].contains(&when) {
                return Err(CmdError::Params(format!("unknown received range `{when}`")));
            }
            token("received", when)
        }
        other => return Err(CmdError::Params(format!("unknown refine kind `{other}`"))),
    };
    let (q, on) = toggle_token(&s.view.search, &term);
    s.view.search = q;
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut() {
        o.insert("on".into(), json!(on));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Notice;
    use mailcraft_model::{Address, AttachmentMeta, DateTime, FlagState, Mailbox};

    const NOW: i64 = 1_791_000_000;

    fn session() -> Session {
        let mut s = Session::new(Mailbox::new_local("Test"));
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.selection.folder = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.fixed_now = Some(DateTime { unix: NOW, offset_min: 0 });
        s.me = vec!["me@example.com".into()];
        s
    }

    fn addr(a: &str) -> Address {
        Address { name: None, addr: a.into() }
    }

    fn add(s: &mut Session, m: MailItem) -> ItemId {
        let folder = s.selection.folder;
        s.mailbox.add_mail(MailItem { folder, received: DateTime { unix: NOW, offset_min: 0 }, ..m })
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    #[test]
    fn all_ids_registered() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        for id in [
            "search.advancedFind",
            "search.close",
            "search.focus",
            "search.includeOlderResults",
            "search.recent",
            "search.refine",
            "search.scope",
            "search.searchTools",
            "search.query",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
    }

    #[test]
    fn terms_and_toggle() {
        assert_eq!(terms(r#"a  category:"Red category" -b"#), vec!["a", "category:\"Red category\"", "-b"]);
        let (q, on) = toggle_token("isread:no", "hasattachments:yes");
        assert!(on);
        assert_eq!(q, "isread:no hasattachments:yes");
        let (q, on) = toggle_token(&q, "ISREAD:NO");
        assert!(!on);
        assert_eq!(q, "hasattachments:yes");
        assert_eq!(token("from", ""), "from:\"\"");
        assert_eq!(token("received", "this week"), "received:\"this week\"");
    }

    #[test]
    fn matcher_keys() {
        let mut s = session();
        let a = add(
            &mut s,
            MailItem {
                subject: "Quarterly budget".into(),
                from: Some(Address { name: Some("Ann Boss".into()), addr: "ann@example.com".into() }),
                to: vec![addr("me@example.com")],
                categories: vec!["Red category".into()],
                attachments: vec![AttachmentMeta { filename: "x.pdf".into(), ..Default::default() }],
                importance: Importance::High,
                flag: FlagState::Flagged {
                    label: "Follow up".into(),
                    start: None,
                    due: None,
                    reminder: None,
                },
                ..Default::default()
            },
        );
        let m = s.mailbox.mail.get(&a).unwrap().clone();
        for q in [
            "",
            "budget",
            "from:ann",
            "from:\"Ann Boss\"",
            "to:me",
            "tocc:me",
            "subject:quarterly",
            "category:\"red category\"",
            "category:any",
            "hasattachments:yes",
            "isflagged:yes",
            "isread:no",
            "importance:high",
            "received:today",
            "received:\"this week\"",
            "received:\"this month\"",
            "-cc:me",
            "from:\"\"",
            "budget AND from:ann",
        ] {
            assert!(item_matches(&s, &m, q), "{q} should match");
        }
        for q in [
            "lunch",
            "from:bob",
            "category:none",
            "hasattachments:no",
            "isread:yes",
            "importance:low",
            "received:yesterday",
            "received:\"last month\"",
            "expires:soon",
            "-to:me",
        ] {
            assert!(!item_matches(&s, &m, q), "{q} should not match");
        }
    }

    #[test]
    fn query_close_and_scope() {
        let mut s = session();
        add(&mut s, MailItem { subject: "Budget".into(), ..Default::default() });
        add(&mut s, MailItem { subject: "Lunch".into(), ..Default::default() });
        let archive = {
            let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
            s.mailbox.role_folder(acct, FolderRole::Archive).unwrap()
        };
        s.mailbox.add_mail(MailItem { folder: archive, subject: "Budget old".into(), ..Default::default() });
        let r = run(&mut s, "search.query", json!({"text": " budget "}));
        assert_eq!(r["query"], "budget");
        assert_eq!(r["results"], 1);
        let r = run(&mut s, "search.scope", json!({"scope": "currentMailbox"}));
        assert_eq!(r["results"], 2);
        assert_eq!(s.view.search_scope, SearchScope::CurrentMailbox);
        run(&mut s, "search.scope", json!({}));
        assert_eq!(s.view.search_scope, SearchScope::AllMailboxes);
        let r = run(&mut s, "search.scope", json!({"includeDeleted": true}));
        assert_eq!(r["includeDeleted"], true);
        assert_eq!(s.view.search_scope, SearchScope::AllMailboxes);
        run(&mut s, "search.scope", json!({"store": "Test", "on": false}));
        assert!(s.run("search.scope", &json!({"store": "Nobody"})).is_err());
        assert!(s.run("search.scope", &json!({"scope": "galaxy"})).is_err());
        assert_eq!(results(&s).len(), 2);
        run(&mut s, "search.close", json!({}));
        assert!(s.view.search.is_empty());
        assert!(s.run("search.query", &json!({})).is_err());
    }

    #[test]
    fn rejected_scope_change_leaves_scope_alone() {
        let mut s = session();
        assert_eq!(s.view.search_scope, SearchScope::CurrentFolder);
        let r = s.run("search.scope", &json!({"scope": "allItems", "store": "Nobody"}));
        assert!(matches!(r, Err(CmdError::Params(_))));
        assert_eq!(s.view.search_scope, SearchScope::CurrentFolder);
    }

    #[test]
    fn deleted_items_left_out_of_mailbox_scope() {
        let mut s = session();
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let deleted = s.mailbox.role_folder(acct, FolderRole::DeletedItems).unwrap();
        s.mailbox.add_mail(MailItem { folder: deleted, subject: "Budget".into(), ..Default::default() });
        s.view.search = "budget".into();
        s.view.search_scope = SearchScope::CurrentMailbox;
        assert!(results(&s).is_empty());
        s.view.search_scope = SearchScope::AllOutlookItems;
        assert_eq!(results(&s).len(), 1);
    }

    #[test]
    fn refine_builds_terms() {
        let mut s = session();
        add(&mut s, MailItem { subject: "x".into(), ..Default::default() });
        let r = run(&mut s, "search.refine", json!({"kind": "unread"}));
        assert_eq!(r["on"], true);
        assert_eq!(r["results"], 1);
        run(&mut s, "search.refine", json!({"kind": "hasAttachments"}));
        run(&mut s, "search.refine", json!({"kind": "categorized", "value": "Red category"}));
        run(&mut s, "search.refine", json!({"kind": "received", "value": "this week"}));
        run(&mut s, "search.refine", json!({"kind": "from"}));
        assert_eq!(
            s.view.search,
            "isread:no hasattachments:yes category:\"Red category\" received:\"this week\" from:\"\""
        );
        let r = run(&mut s, "search.refine", json!({"kind": "unread"}));
        assert_eq!(r["on"], false);
        assert!(s.run("search.refine", &json!({"kind": "received", "value": "someday"})).is_err());
        assert!(s.run("search.refine", &json!({"kind": "zzz"})).is_err());
        assert_eq!(run(&mut s, "search.refine", json!({}))["dialog"], "searchRefine");
    }

    #[test]
    fn tools_recent_and_dialogs() {
        let mut s = session();
        assert_eq!(run(&mut s, "search.focus", json!({}))["dialog"], "searchBox");
        assert_eq!(run(&mut s, "search.advancedFind", json!({}))["dialog"], "advancedFind");
        assert_eq!(run(&mut s, "search.searchTools", json!({}))["dialog"], "searchTools");
        let r = run(&mut s, "search.searchTools", json!({"tool": "indexingStatus"}));
        assert_eq!(r["remaining"], 0);
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "indexingStatus"));
        assert!(s.run("search.searchTools", &json!({"tool": "x"})).is_err());
        let r = run(&mut s, "search.recent", json!({}));
        assert_eq!(r["placeholder"], "No Recent Searches");
        assert!(s.run("search.recent", &json!({"index": 0})).is_err());
        assert!(matches!(s.run("search.includeOlderResults", &json!({})), Err(CmdError::Disabled(_))));
        assert_eq!(s.registry.by_shortcut("Mod+E").map(|c| c.id), Some("search.focus"));
    }
}
