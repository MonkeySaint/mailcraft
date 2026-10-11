//! Search commands (`search.*`): the query in `ViewState::search`, its scope, the Search tab refiners,
//! Recent Searches, Search Tools (Indexing Status, Advanced Find) and match highlights.
//!
//! Queries use the `mailcraft-search` language (free text, `from:`, `to:`, `cc:`, `subject:`, `body:`,
//! `attachments:`, `hasattachments:`, `isread:`, `isflagged:`, `importance:`, `category:`, `folder:`,
//! `received:`/`sent:` with relative dates, dates and ranges, `size:`, `kind:`, AND/OR/NOT, parentheses,
//! quoted phrases; prefix matching, case and diacritics ignored). Evaluation goes through the session's
//! index ([`crate::search_index`]); [`item_matches`] evaluates one item directly.

use serde_json::{Value, json};

use super::folder::{current_folder, dialog};
use super::util::{opt_bool, opt_str, opt_u64};
use crate::search_index::{self, MAX_RECENT};
use crate::{CmdError, CmdResult, CommandSpec, SearchScope, Session, always};
use mailcraft_model::{FolderId, ItemId, MailItem};
use mailcraft_search::Field;

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
            r#"{"words"?:string,"in"?:"subject|subjectBody|all","from"?:string,"sentTo"?:string,"time"?:"today|yesterday|this week|last week|this month|last month|this year","timeField"?:"received|sent","query"?:string,"folder"?:number,"subfolders"?:bool,"limit"?:number}"#,
            false,
            always,
            advanced_find,
        ),
        spec("search.close", "Close Search", "Search › Close", "", "", false, always, close),
        spec("search.focus", "Search", "Title bar search box", "F3 / Mod+E", "", false, always, focus),
        spec(
            "search.highlights",
            "Search Highlights",
            "Message list / Reading pane",
            "",
            r#"{"text"?:string,"id"?:number,"field"?:"subject|from|to|cc|body|attachments","query"?:string}"#,
            false,
            always,
            highlights,
        ),
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
            r#"{"text":string,"commit"?:bool}"#,
            false,
            always,
            query,
        ),
        spec(
            "search.recent",
            "Recent Searches",
            "Search › Options",
            "",
            r#"{"index"?:number,"clear"?:bool}"#,
            false,
            always,
            recent,
        ),
        spec(
            "search.refine",
            "More Filters",
            "Home › Find › Filter Email",
            "",
            r#"{"kind"?:"from|to|cc|subject|body|attachments|hasAttachments|categorized|sentTo|unread|read|flagged|important|importance|thisWeek|received|sent|size|folder|expiringSoon","value"?:string}"#,
            false,
            always,
            refine,
        ),
        spec(
            "search.results",
            "Search Results",
            "Title bar search box",
            "",
            r#"{"limit"?:number,"query"?:string}"#,
            false,
            always,
            results_cmd,
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
// Query text helpers (shared with the View tab's filters and the Find dialog)

/// `key:value`, quoting the value when it contains spaces or is empty.
pub fn token(key: &str, value: &str) -> String {
    if value.is_empty() || value.chars().any(char::is_whitespace) || value.contains('"') {
        format!("{key}:\"{}\"", value.replace('"', ""))
    } else {
        format!("{key}:{value}")
    }
}

/// Splits a query into whitespace-separated terms, keeping double-quoted runs (with their quotes) inside one
/// term.
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

/// True when `m` matches `query` (an empty query matches everything).
pub fn item_matches(s: &Session, m: &MailItem, query: &str) -> bool {
    search_index::item_matches(s, m, query)
}

/// Folders the current scope searches (see [`search_index::scope_folders`]).
pub fn scope_folders(s: &Session) -> Vec<FolderId> {
    search_index::scope_folders(s)
}

/// Items matching the current query in the current scope, best first (empty when no search is active).
pub fn results(s: &Session) -> Vec<ItemId> {
    search_index::results(s)
}

fn scope_name(sc: SearchScope) -> &'static str {
    match sc {
        SearchScope::AllMailboxes => "allMailboxes",
        SearchScope::CurrentMailbox => "currentMailbox",
        SearchScope::CurrentFolder => "currentFolder",
        SearchScope::Subfolders => "subfolders",
        SearchScope::AllItems => "allItems",
    }
}

/// Hits shown in command results.
const SUMMARY_HITS: usize = 50;

fn summary(s: &Session) -> Value {
    let hits = if s.view.search.trim().is_empty() {
        Vec::new()
    } else {
        search_index::hits(s, &s.view.search, &search_index::scope_filter(s))
    };
    let top: Vec<Value> = hits
        .iter()
        .take(SUMMARY_HITS)
        .map(|h| json!({"id": h.id, "score": (h.score * 100.0).round() / 100.0}))
        .collect();
    json!({
        "query": s.view.search,
        "parsed": mailcraft_search::parse(&s.view.search).to_string(),
        "scope": scope_name(s.view.search_scope),
        "includeDeleted": s.search.include_deleted,
        "results": hits.len(),
        "hits": top,
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

/// Longest query accepted from the search box (chars).
const MAX_QUERY_CHARS: usize = 4_000;

fn query(s: &mut Session, p: &Value) -> CmdResult {
    let text = opt_str(p, "text").ok_or_else(|| CmdError::Params("`text` (string) required".into()))?;
    s.view.search = text.trim().chars().take(MAX_QUERY_CHARS).collect();
    if opt_bool(p, "commit").unwrap_or(false) {
        let q = s.view.search.clone();
        s.search.remember(&q);
    }
    Ok(summary(s))
}

fn focus(s: &mut Session, _p: &Value) -> CmdResult {
    let params = json!({ "query": s.view.search, "scope": scope_name(s.view.search_scope), "recent": s.search.recent });
    Ok(dialog(s, "searchBox", params))
}

fn close(s: &mut Session, _p: &Value) -> CmdResult {
    let q = std::mem::take(&mut s.view.search);
    s.search.remember(&q);
    Ok(json!({ "query": "" }))
}

fn recent(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "clear") == Some(true) {
        s.search.recent.clear();
        return Ok(json!({ "items": [], "placeholder": "No Recent Searches" }));
    }
    if let Some(v) = p.get("index").filter(|v| !v.is_null()) {
        let q = v
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| s.search.recent.get(i))
            .cloned()
            .ok_or_else(|| CmdError::Params(format!("no recent search at index {v}")))?;
        s.view.search = q.clone();
        s.search.remember(&q);
        return Ok(summary(s));
    }
    let items = &s.search.recent;
    let placeholder = if items.is_empty() { "No Recent Searches" } else { "" };
    Ok(json!({ "items": items, "placeholder": placeholder, "max": MAX_RECENT }))
}

fn include_older_results(_s: &mut Session, _p: &Value) -> CmdResult {
    Ok(json!({ "on": false }))
}

fn search_tools(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "tool") {
        None => Ok(dialog(s, "searchTools", json!({}))),
        Some("indexingStatus") => {
            let remaining = s.search.pending(&s.mailbox);
            let stats = s.search.stats(&s.mailbox);
            let params =
                json!({ "remaining": 0, "items": stats.items, "terms": stats.terms, "before": remaining });
            let mut v = dialog(s, "indexingStatus", params);
            if let Some(o) = v.as_object_mut() {
                // `stats` indexed everything that was waiting.
                o.insert("remaining".into(), json!(0));
                o.insert("items".into(), json!(stats.items));
                o.insert("terms".into(), json!(stats.terms));
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
        Some("allItems") => Some(SearchScope::AllItems),
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
    if let Some(d) = include_deleted {
        s.search.include_deleted = d;
    }
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut()
        && let Some(name) = store
    {
        o.insert("store".into(), json!(name));
        o.insert("on".into(), json!(opt_bool(p, "on").unwrap_or(true)));
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
            "attachments",
            "hasAttachments",
            "categorized",
            "sentTo",
            "unread",
            "read",
            "flagged",
            "important",
            "importance",
            "thisWeek",
            "received",
            "sent",
            "size",
            "folder",
            "expiringSoon",
        ];
        return Ok(dialog(
            s,
            "searchRefine",
            json!({ "folder": folder, "kinds": kinds, "query": s.view.search }),
        ));
    };
    let value = opt_str(p, "value").map(str::trim).unwrap_or_default();
    let date = |key: &str, default: &str| -> Result<String, CmdError> {
        let when = if value.is_empty() { default } else { value };
        let parsed = mailcraft_search::parse(&token(key, when));
        if matches!(parsed, mailcraft_search::Query::Term(mailcraft_search::Term::Invalid(..))) {
            return Err(CmdError::Params(format!("unknown date or range `{when}`")));
        }
        Ok(token(key, when))
    };
    let term = match kind {
        "from" | "to" | "cc" | "subject" | "body" | "attachments" | "folder" => token(kind, value),
        "hasAttachments" => "hasattachments:yes".to_owned(),
        "unread" => "isread:no".to_owned(),
        "read" => "isread:yes".to_owned(),
        "flagged" => "isflagged:yes".to_owned(),
        "important" => "importance:high".to_owned(),
        "importance" => match value {
            "" | "high" => "importance:high".to_owned(),
            "normal" | "low" => format!("importance:{value}"),
            other => return Err(CmdError::Params(format!("unknown importance `{other}`"))),
        },
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
        "thisWeek" => token("received", "this week"),
        "received" => date("received", "this week")?,
        "sent" => date("sent", "this week")?,
        "size" => {
            let v = if value.is_empty() { "large" } else { value };
            let parsed = mailcraft_search::parse(&token("size", v));
            if !matches!(parsed, mailcraft_search::Query::Term(mailcraft_search::Term::Size(_))) {
                return Err(CmdError::Params(format!("unknown size `{v}`")));
            }
            token("size", v)
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

/// Builds the query an Advanced Find form describes.
pub fn advanced_query(p: &Value) -> Result<String, CmdError> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = opt_str(p, "query").map(str::trim).filter(|q| !q.is_empty()) {
        parts.push(format!("({q})"));
    }
    if let Some(words) = opt_str(p, "words").map(str::trim).filter(|w| !w.is_empty()) {
        let quoted = format!("\"{}\"", words.replace('"', ""));
        match opt_str(p, "in").unwrap_or("subjectBody") {
            "subject" => parts.push(format!("subject:{quoted}")),
            "subjectBody" => parts.push(format!("(subject:{quoted} OR body:{quoted})")),
            "all" => parts.push(quoted),
            other => return Err(CmdError::Params(format!("unknown `in` value `{other}`"))),
        }
    }
    if let Some(from) = opt_str(p, "from").map(str::trim).filter(|v| !v.is_empty()) {
        parts.push(token("from", from));
    }
    if let Some(to) = opt_str(p, "sentTo").map(str::trim).filter(|v| !v.is_empty()) {
        parts.push(token("tocc", to));
    }
    if let Some(when) = opt_str(p, "time").map(str::trim).filter(|v| !v.is_empty() && *v != "anytime") {
        let key = match opt_str(p, "timeField").unwrap_or("received") {
            "received" => "received",
            "sent" => "sent",
            other => return Err(CmdError::Params(format!("unknown `timeField` `{other}`"))),
        };
        let t = token(key, when);
        if matches!(
            mailcraft_search::parse(&t),
            mailcraft_search::Query::Term(mailcraft_search::Term::Invalid(..))
        ) {
            return Err(CmdError::Params(format!("unknown time `{when}`")));
        }
        parts.push(t);
    }
    Ok(parts.join(" "))
}

/// Advanced Find: without criteria opens the dialog; with criteria runs them over the folder (and its
/// subfolders) with the same query engine as the search box, leaving the main search alone.
fn advanced_find(s: &mut Session, p: &Value) -> CmdResult {
    let q = advanced_query(p)?;
    let folder = current_folder(s, p)?;
    if q.is_empty() {
        let params = json!({ "focus": "words", "folder": folder.0, "recent": s.search.recent });
        return Ok(dialog(s, "advancedFind", params));
    }
    let folders: Vec<u64> = if opt_bool(p, "subfolders").unwrap_or(true) {
        s.mailbox.subtree(folder).into_iter().map(|f| f.0).collect()
    } else {
        vec![folder.0]
    };
    let filter = mailcraft_search::Filter::mail_in(folders);
    let hits = search_index::hits(s, &q, &filter);
    let limit = opt_u64(p, "limit").and_then(|n| usize::try_from(n).ok()).unwrap_or(500).min(5_000);
    let rows: Vec<Value> = hits.iter().take(limit).filter_map(|h| row(s, ItemId(h.id))).collect();
    Ok(json!({ "query": q, "folder": folder.0, "results": hits.len(), "items": rows }))
}

/// One result row: id, subject, from, received, folder.
fn row(s: &Session, id: ItemId) -> Option<Value> {
    let mb = &s.mailbox;
    if let Some(m) = mb.mail.get(&id) {
        let from =
            m.from.as_ref().map(|a| a.name.clone().unwrap_or_else(|| a.addr.clone())).unwrap_or_default();
        return Some(json!({
            "id": id.0,
            "kind": "email",
            "subject": m.subject,
            "from": from,
            "received": m.received.unix,
            "folder": m.folder.0,
            "read": m.read,
        }));
    }
    let (kind, subject, folder) = if let Some(a) = mb.appointments.get(&id) {
        ("meeting", a.subject.clone(), a.folder)
    } else if let Some(c) = mb.contacts.get(&id) {
        ("contact", format!("{} {}", c.first_name, c.last_name).trim().to_owned(), c.folder)
    } else if let Some(t) = mb.tasks.get(&id) {
        ("task", t.subject.clone(), t.folder)
    } else {
        let n = mb.notes.get(&id)?;
        ("note", n.text.lines().next().unwrap_or_default().to_owned(), n.folder)
    };
    Some(json!({ "id": id.0, "kind": kind, "subject": subject, "folder": folder.0 }))
}

/// Ranked results of the active search (or `query`) with row details, for the CLI, MCP and control channel.
fn results_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let q = opt_str(p, "query").map(str::to_owned).unwrap_or_else(|| s.view.search.clone());
    let hits = if q.trim().is_empty() {
        Vec::new()
    } else {
        search_index::hits(s, &q, &search_index::scope_filter(s))
    };
    let limit = opt_u64(p, "limit").and_then(|n| usize::try_from(n).ok()).unwrap_or(100).min(5_000);
    let rows: Vec<Value> = hits.iter().take(limit).filter_map(|h| row(s, ItemId(h.id))).collect();
    Ok(json!({ "query": q, "scope": scope_name(s.view.search_scope), "results": hits.len(), "items": rows }))
}

fn field_of(name: &str) -> Result<Field, CmdError> {
    Ok(match name {
        "subject" => Field::Subject,
        "from" => Field::From,
        "to" => Field::To,
        "cc" => Field::Cc,
        "body" => Field::Body,
        "attachments" => Field::Attachments,
        other => return Err(CmdError::Params(format!("unknown field `{other}`"))),
    })
}

/// Match ranges of the active search (or `query`) in `text`, or in a field of message `id`. Ranges are
/// byte offsets plus char offsets (for UIs that index by char).
fn highlights(s: &mut Session, p: &Value) -> CmdResult {
    let field = field_of(opt_str(p, "field").unwrap_or("subject"))?;
    let text = match (opt_str(p, "text"), opt_u64(p, "id")) {
        (Some(t), _) => t.to_owned(),
        (None, Some(id)) => {
            let m = s
                .mailbox
                .mail
                .get(&ItemId(id))
                .ok_or_else(|| CmdError::Params(format!("no message with id {id}")))?;
            let people = |list: &[mailcraft_model::Address]| {
                list.iter()
                    .map(|a| match &a.name {
                        Some(n) => format!("{n} <{}>", a.addr),
                        None => a.addr.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            match field {
                Field::Subject => m.subject.clone(),
                Field::From => people(m.from.as_slice()),
                Field::To => people(&m.to),
                Field::Cc => people(&m.cc),
                Field::Body => m.body_text(),
                Field::Attachments => {
                    m.attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>().join("; ")
                }
            }
        }
        (None, None) => return Err(CmdError::Params("`text` (string) or `id` (number) required".into())),
    };
    let q = opt_str(p, "query").map(str::to_owned).unwrap_or_else(|| s.view.search.clone());
    let ranges = mailcraft_search::highlights(&mailcraft_search::parse(&q), field, &text);
    let chars = |b: usize| text.get(..b).map_or(0, |t| t.chars().count());
    let out: Vec<Value> = ranges
        .iter()
        .map(|(a, b)| json!({ "start": a, "end": b, "charStart": chars(*a), "charEnd": chars(*b) }))
        .collect();
    Ok(json!({ "ranges": out }))
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Notice;
    use mailcraft_model::{Address, AttachmentMeta, DateTime, FlagState, FolderRole, Importance, Mailbox};

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
            "search.highlights",
            "search.includeOlderResults",
            "search.recent",
            "search.refine",
            "search.results",
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
            "BUDG",
            "from:ann",
            "from:\"Ann Boss\"",
            "to:me",
            "tocc:me",
            "subject:quarterly",
            "category:\"red category\"",
            "category:any",
            "hasattachments:yes",
            "attachments:pdf",
            "isflagged:yes",
            "isread:no",
            "importance:high",
            "received:today",
            "received:\"this week\"",
            "received:\"this month\"",
            "-cc:me",
            "from:\"\"",
            "budget AND from:ann",
            "lunch OR budget",
            "(lunch OR budget) NOT from:bob",
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
        assert!(s.search.include_deleted);
        assert_eq!(s.view.search_scope, SearchScope::AllMailboxes);
        run(&mut s, "search.scope", json!({"store": "Test", "on": false}));
        assert!(s.run("search.scope", &json!({"store": "Nobody"})).is_err());
        assert!(s.run("search.scope", &json!({"scope": "galaxy"})).is_err());
        assert_eq!(results(&s).len(), 2);
        run(&mut s, "search.close", json!({}));
        assert!(s.view.search.is_empty());
        assert_eq!(s.search.recent, ["budget"]);
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
        s.view.search_scope = SearchScope::AllItems;
        assert_eq!(results(&s).len(), 1);
        s.view.search_scope = SearchScope::CurrentMailbox;
        run(&mut s, "search.scope", json!({"includeDeleted": true, "scope": "currentMailbox"}));
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
        run(&mut s, "search.refine", json!({"kind": "thisWeek"}));
        assert!(s.view.search.ends_with("from:\"\""));
        run(&mut s, "search.refine", json!({"kind": "size", "value": "huge"}));
        assert!(s.view.search.ends_with("size:huge"));
        run(&mut s, "search.refine", json!({"kind": "received", "value": ">=2026-01-01"}));
        assert!(s.run("search.refine", &json!({"kind": "received", "value": "someday"})).is_err());
        assert!(s.run("search.refine", &json!({"kind": "size", "value": "enormous"})).is_err());
        assert!(s.run("search.refine", &json!({"kind": "importance", "value": "urgent"})).is_err());
        assert!(s.run("search.refine", &json!({"kind": "zzz"})).is_err());
        assert_eq!(run(&mut s, "search.refine", json!({}))["dialog"], "searchRefine");
    }

    #[test]
    fn recent_searches() {
        let mut s = session();
        add(&mut s, MailItem { subject: "Budget".into(), ..Default::default() });
        let r = run(&mut s, "search.recent", json!({}));
        assert_eq!(r["placeholder"], "No Recent Searches");
        assert!(s.run("search.recent", &json!({"index": 0})).is_err());
        run(&mut s, "search.query", json!({"text": "bud"}));
        run(&mut s, "search.query", json!({"text": "budget", "commit": true}));
        run(&mut s, "search.query", json!({"text": "lunch", "commit": true}));
        assert_eq!(s.search.recent, ["lunch", "budget"]);
        let r = run(&mut s, "search.recent", json!({"index": 1}));
        assert_eq!(r["query"], "budget");
        assert_eq!(r["results"], 1);
        assert_eq!(s.search.recent, ["budget", "lunch"]);
        assert_eq!(run(&mut s, "search.recent", json!({}))["items"][0], "budget");
        run(&mut s, "search.recent", json!({"clear": true}));
        assert!(s.search.recent.is_empty());
        assert!(s.run("search.recent", &json!({"index": "x"})).is_err());
    }

    #[test]
    fn advanced_find_uses_the_query_engine() {
        let mut s = session();
        let a = add(
            &mut s,
            MailItem {
                subject: "Budget plan".into(),
                from: Some(addr("ann@example.com")),
                to: vec![addr("bob@example.org")],
                ..Default::default()
            },
        );
        add(&mut s, MailItem { subject: "Lunch".into(), body: Default::default(), ..Default::default() });
        assert_eq!(run(&mut s, "search.advancedFind", json!({}))["dialog"], "advancedFind");
        let r =
            run(&mut s, "search.advancedFind", json!({"words": "budget", "in": "subject", "from": "ann"}));
        assert_eq!(r["results"], 1);
        assert_eq!(r["items"][0]["id"], a.0);
        let r = run(&mut s, "search.advancedFind", json!({"sentTo": "bob", "time": "today"}));
        assert_eq!(r["results"], 1);
        let r = run(&mut s, "search.advancedFind", json!({"words": "budget", "time": "yesterday"}));
        assert_eq!(r["results"], 0);
        assert!(s.view.search.is_empty());
        assert!(s.run("search.advancedFind", &json!({"words": "x", "in": "nowhere"})).is_err());
        assert!(s.run("search.advancedFind", &json!({"time": "someday"})).is_err());
    }

    #[test]
    fn results_and_highlights() {
        let mut s = session();
        let a = add(&mut s, MailItem { subject: "Budget review".into(), ..Default::default() });
        run(&mut s, "search.query", json!({"text": "bud"}));
        let r = run(&mut s, "search.results", json!({}));
        assert_eq!(r["items"][0]["id"], a.0);
        assert_eq!(r["items"][0]["subject"], "Budget review");
        let r = run(&mut s, "search.highlights", json!({"id": a.0, "field": "subject"}));
        assert_eq!(r["ranges"][0]["start"], 0);
        assert_eq!(r["ranges"][0]["end"], 6);
        let r = run(&mut s, "search.highlights", json!({"text": "Café budget", "query": "budget"}));
        assert_eq!(r["ranges"][0]["start"], 6);
        assert_eq!(r["ranges"][0]["charStart"], 5);
        assert!(s.run("search.highlights", &json!({})).is_err());
        assert!(s.run("search.highlights", &json!({"text": "x", "field": "nope"})).is_err());
        assert!(s.run("search.highlights", &json!({"id": 999_999})).is_err());
    }

    #[test]
    fn tools_and_dialogs() {
        let mut s = session();
        add(&mut s, MailItem { subject: "x".into(), ..Default::default() });
        assert_eq!(run(&mut s, "search.focus", json!({}))["dialog"], "searchBox");
        assert_eq!(run(&mut s, "search.searchTools", json!({}))["dialog"], "searchTools");
        let r = run(&mut s, "search.searchTools", json!({"tool": "indexingStatus"}));
        assert_eq!(r["remaining"], 0);
        assert_eq!(r["items"], 1);
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "indexingStatus"));
        assert!(s.run("search.searchTools", &json!({"tool": "x"})).is_err());
        assert!(matches!(s.run("search.includeOlderResults", &json!({})), Err(CmdError::Disabled(_))));
        assert_eq!(s.registry.by_shortcut("Mod+E").map(|c| c.id), Some("search.focus"));
    }
}
