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
            r#"{"lookFor"?:"messages|any|appointments|contacts|tasks|notes","words"?:string,"in"?:"subject|subjectBody|all","from"?:string,"sentTo"?:string,"time"?:"today|yesterday|this week|last week|this month|last month|this year","timeField"?:"received|sent","query"?:string,"folder"?:number,"subfolders"?:bool,"limit"?:number}"#,
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
            r#"{"on"?:bool,"refresh"?:bool} (default: toggle; on with a search in the box searches the servers of the accounts in scope that keep older mail there)"#,
            false,
            older_available,
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
            r#"{"kind"?:"from|to|cc|subject|body|attachments|hasAttachments|categorized|sentTo|unread|read|flagged|important|importance|thisWeek|received|sent|size|folder|expiringSoon|formField|refiners|field|removeField","value"?:string, "field"?:string, "condition"?:string, "replace"?:string, "term"?:string} (no kind = enter Search mode; from/to/cc/subject/body/attachments/folder without a value and sentTo "other" put key:"" in the box with the caret between the quotes; formField = the Add Form Field chooser; field adds a form-field condition (field, condition, value; replace = the term it replaces); removeField removes `term`)"#,
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
        "older": crate::sync::older::state_json(s),
    })
}

// ---------------------------------------------------------------------------------------------------------
// Enabled predicates

/// Include Older Results needs an online account that keeps older mail on its server (a download window).
fn older_available(s: &Session) -> Option<&'static str> {
    crate::sync::older::unavailable(s)
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
        // Include Older Results: a committed search also goes to the servers.
        if s.sync.older.on && crate::sync::older::unavailable(s).is_none() {
            let _ = crate::sync::older::start(s, false);
        }
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

fn include_older_results(s: &mut Session, p: &Value) -> CmdResult {
    crate::sync::older::toggle(s, p)
}

fn search_tools(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "tool") {
        None => Ok(dialog(s, "searchTools", json!({}))),
        Some("indexingStatus") => {
            let before = s.search.pending(&s.mailbox);
            let stats = s.search.stats(&s.mailbox);
            // `stats` indexed everything that was waiting; the window keeps reporting while mail arrives.
            let mut params = search_index::indexing_status(s);
            if let Some(o) = params.as_object_mut() {
                o.insert("before".into(), json!(before));
                o.insert("terms".into(), json!(stats.terms));
                o.insert("indexed".into(), json!(stats.items));
            }
            let mut v = dialog(s, "indexingStatus", params.clone());
            if let (Some(o), Some(p)) = (v.as_object_mut(), params.as_object()) {
                for (k, x) in p {
                    o.insert(k.clone(), x.clone());
                }
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
    let account = match store {
        Some(name) => Some(
            s.mailbox
                .accounts
                .iter()
                .find(|a| a.display_name.eq_ignore_ascii_case(name) || a.address.eq_ignore_ascii_case(name))
                .map(search_index::store_key)
                .ok_or_else(|| CmdError::Params(format!("no account or data file named `{name}`")))?,
        ),
        None => None,
    };
    if let Some(sc) = sc {
        s.view.search_scope = sc;
    }
    if let Some(d) = include_deleted {
        s.search.include_deleted = d;
    }
    // Locations to Search: the store is searched (on) or left out of All Mailboxes / All Items (off).
    if let Some(key) = &account {
        let on = opt_bool(p, "on").unwrap_or(true);
        let mut excluded: Vec<String> = s
            .settings
            .list(search_index::EXCLUDED_STORES_SETTING)
            .into_iter()
            .filter(|k| !k.eq_ignore_ascii_case(key))
            .collect();
        if !on {
            excluded.push(key.clone());
        }
        s.settings
            .set(search_index::EXCLUDED_STORES_SETTING, &json!(excluded))
            .map_err(|e| CmdError::Failed(e.to_string()))?;
    }
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut()
        && let Some(name) = store
    {
        let on = account.is_some_and(|k| {
            s.mailbox.accounts.iter().any(|a| search_index::store_key(a) == k && searched(s, a.id))
        });
        o.insert("store".into(), json!(name));
        o.insert("on".into(), json!(on));
    }
    Ok(out)
}

fn searched(s: &Session, account: mailcraft_model::AccountId) -> bool {
    search_index::searched_stores(s).contains(&account)
}

/// Locations to Search: every account and data file with whether All Mailboxes / All Items search it.
pub fn locations(s: &Session) -> Vec<(String, String, bool)> {
    let on = search_index::searched_stores(s);
    s.mailbox
        .accounts
        .iter()
        .map(|a| {
            let key = if a.address.trim().is_empty() { a.display_name.clone() } else { a.address.clone() };
            let label = if a.display_name.trim().is_empty() { key.clone() } else { a.display_name.clone() };
            (key, label, on.contains(&a.id))
        })
        .collect()
}

/// Char offset of term `index` in the space-joined `list`.
fn term_offset(list: &[String], index: usize) -> usize {
    list.iter().take(index).fold(0usize, |n, t| n.saturating_add(t.chars().count()).saturating_add(1))
}

/// A refiner that needs a value (From, Subject, To…, Sent to Another Recipient): `key:""` goes into the
/// query (once; an empty value filters nothing) and the search box takes focus with the caret between the
/// quotes, so typing fills it in. Returns the summary with `edit` = [start, end] char offsets of the value.
fn refine_placeholder(s: &mut Session, key: &str) -> CmdResult {
    let term = token(key, "");
    let mut list = terms(&s.view.search);
    let index = match list.iter().position(|t| t.eq_ignore_ascii_case(&term)) {
        Some(i) => i,
        None => {
            list.push(term.clone());
            list.len().saturating_sub(1)
        }
    };
    s.view.search = list.join(" ");
    // After `key:"`.
    let caret = term_offset(&list, index).saturating_add(key.chars().count()).saturating_add(2);
    let edit = json!([caret, caret]);
    s.notify(crate::Notice::OpenDialog(
        "searchBox".into(),
        json!({ "query": s.view.search, "select": edit, "scope": scope_name(s.view.search_scope) }),
    ));
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut() {
        o.insert("on".into(), json!(true));
        o.insert("edit".into(), edit);
        o.insert("dialog".into(), json!("searchBox"));
    }
    Ok(out)
}

fn refine(s: &mut Session, p: &Value) -> CmdResult {
    let Some(kind) = opt_str(p, "kind") else {
        // More Filters…: Search mode, with the Search tab's Refine controls.
        let params =
            json!({ "query": s.view.search, "scope": scope_name(s.view.search_scope), "tab": "Search" });
        return Ok(dialog(s, "searchBox", params));
    };
    if kind == "formField" {
        let folder = current_folder(s, &Value::Null).map(|f| f.0).unwrap_or_default();
        return Ok(dialog(
            s,
            "addFormField",
            json!({ "folder": folder, "forms": form_fields_json(), "conditions": field_conditions(&s.view.search), "query": s.view.search }),
        ));
    }
    if kind == "field" {
        return add_field_condition(s, p);
    }
    if kind == "removeField" {
        let term = opt_str(p, "term").map(str::trim).unwrap_or_default();
        let mut list = terms(&s.view.search);
        let before = list.len();
        list.retain(|t| !t.eq_ignore_ascii_case(term));
        if list.len() == before {
            return Err(CmdError::Params("that condition is not in the search".into()));
        }
        s.view.search = list.join(" ");
        return Ok(summary(s));
    }
    if kind == "refiners" {
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
    }
    let value = opt_str(p, "value").map(str::trim).unwrap_or_default();
    match (kind, value) {
        ("from" | "to" | "cc" | "subject" | "body" | "attachments" | "folder", "") => {
            return refine_placeholder(s, kind);
        }
        ("sentTo", "other") => return refine_placeholder(s, "to"),
        _ => {}
    }
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

/// Field types of Add Form Field: what conditions and values they take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Date,
    Number,
    /// One of the listed values.
    Choice(&'static [&'static str]),
}

const YES_NO: &[&str] = &["Yes", "No"];
const SENSITIVITIES: &[&str] = &["Normal", "Personal", "Private", "Confidential"];
const LEVELS: &[&str] = &["Low", "Normal", "High"];
const TASK_STATUSES: &[&str] =
    &["Not Started", "In Progress", "Completed", "Waiting on someone else", "Deferred"];
const FLAG_STATUSES: &[&str] = &["Flagged", "Completed"];
const CLASSES: &[&str] = &[
    "IPM.Note",
    "IPM.Schedule.Meeting.Request",
    "IPM.Appointment",
    "IPM.Contact",
    "IPM.Task",
    "IPM.StickyNote",
];

/// Add Form Field: the item properties a condition can test, grouped by form (group, field label, type).
/// The labels name the properties of `mailcraft_search::Doc::props` (compared without spaces).
pub const FORM_FIELDS: &[(&str, &str, FieldType)] = &[
    ("Frequently-used fields", "Subject", FieldType::Text),
    ("Frequently-used fields", "From", FieldType::Text),
    ("Frequently-used fields", "To", FieldType::Text),
    ("Frequently-used fields", "Categories", FieldType::Text),
    ("Frequently-used fields", "Importance", FieldType::Choice(LEVELS)),
    ("Frequently-used fields", "Sensitivity", FieldType::Choice(SENSITIVITIES)),
    ("Frequently-used fields", "Received", FieldType::Date),
    ("Frequently-used fields", "Due Date", FieldType::Date),
    ("Frequently-used fields", "Message Class", FieldType::Choice(CLASSES)),
    ("Frequently-used fields", "Size", FieldType::Number),
    ("All Mail fields", "Cc", FieldType::Text),
    ("All Mail fields", "Sent", FieldType::Date),
    ("All Mail fields", "Flag Status", FieldType::Choice(FLAG_STATUSES)),
    ("All Mail fields", "Start Date", FieldType::Date),
    ("All Mail fields", "Read", FieldType::Choice(YES_NO)),
    ("All Mail fields", "Has Attachments", FieldType::Choice(YES_NO)),
    ("All Mail fields", "Conversation", FieldType::Text),
    ("All Contact fields", "Full Name", FieldType::Text),
    ("All Contact fields", "First Name", FieldType::Text),
    ("All Contact fields", "Last Name", FieldType::Text),
    ("All Contact fields", "File As", FieldType::Text),
    ("All Contact fields", "Company", FieldType::Text),
    ("All Contact fields", "Job Title", FieldType::Text),
    ("All Contact fields", "Department", FieldType::Text),
    ("All Contact fields", "Office", FieldType::Text),
    ("All Contact fields", "Email", FieldType::Text),
    ("All Contact fields", "Business Phone", FieldType::Text),
    ("All Contact fields", "Mobile Phone", FieldType::Text),
    ("All Contact fields", "Home Phone", FieldType::Text),
    ("All Contact fields", "Birthday", FieldType::Text),
    ("All Contact fields", "Web Page", FieldType::Text),
    ("All Task fields", "Status", FieldType::Choice(TASK_STATUSES)),
    ("All Task fields", "Priority", FieldType::Choice(LEVELS)),
    ("All Task fields", "Percent Complete", FieldType::Number),
    ("All Task fields", "Complete", FieldType::Choice(YES_NO)),
    ("All Task fields", "Date Completed", FieldType::Date),
    ("All Task fields", "Owner", FieldType::Text),
    ("All Appointment fields", "Location", FieldType::Text),
    ("All Appointment fields", "Start", FieldType::Date),
    ("All Appointment fields", "End", FieldType::Date),
    ("All Appointment fields", "All Day Event", FieldType::Choice(YES_NO)),
    ("All Appointment fields", "Recurring", FieldType::Choice(YES_NO)),
    ("All Appointment fields", "Organizer", FieldType::Text),
    ("All Note fields", "Modified", FieldType::Date),
];

/// Conditions of a field type: (id, label, query operator); "is empty" / "is not empty" take no value.
pub fn conditions_of(t: FieldType) -> &'static [(&'static str, &'static str, &'static str)] {
    match t {
        FieldType::Text => &[
            ("contains", "contains", "~"),
            ("doesNotContain", "doesn't contain", "!~"),
            ("is", "is (exactly)", "="),
            ("isNot", "is not", "!="),
            ("isEmpty", "is empty", "="),
            ("isNotEmpty", "is not empty", "!="),
        ],
        FieldType::Date => &[
            ("on", "on", "="),
            ("before", "before", "<"),
            ("after", "after", ">"),
            ("isEmpty", "does not exist", "="),
            ("isNotEmpty", "exists", "!="),
        ],
        FieldType::Number => &[
            ("equals", "equals", "="),
            ("notEquals", "not equal to", "!="),
            ("lessThan", "less than", "<"),
            ("moreThan", "more than", ">"),
        ],
        FieldType::Choice(_) => &[("is", "equals", "="), ("isNot", "not equal to", "!=")],
    }
}

fn field_type_name(t: FieldType) -> &'static str {
    match t {
        FieldType::Text => "text",
        FieldType::Date => "date",
        FieldType::Number => "number",
        FieldType::Choice(_) => "choice",
    }
}

/// The Add Form Field chooser: forms with their fields, each with its conditions and choices.
pub fn form_fields_json() -> Value {
    let mut groups: Vec<(&str, Vec<Value>)> = Vec::new();
    for (group, label, t) in FORM_FIELDS {
        let conds: Vec<Value> =
            conditions_of(*t).iter().map(|(id, l, _)| json!({"id": id, "label": l})).collect();
        let choices: &[&str] = if let FieldType::Choice(c) = t { c } else { &[] };
        let field =
            json!({"label": label, "type": field_type_name(*t), "conditions": conds, "choices": choices});
        match groups.iter_mut().find(|(g, _)| g == group) {
            Some((_, list)) => list.push(field),
            None => groups.push((group, vec![field])),
        }
    }
    Value::Array(groups.into_iter().map(|(g, fields)| json!({"form": g, "fields": fields})).collect())
}

/// The form-field conditions (`prop:` terms) in a query, for the editable rows under the search box:
/// `{term, field, condition, value}`.
pub fn field_conditions(query: &str) -> Vec<Value> {
    terms(query)
        .into_iter()
        .filter_map(|t| {
            let mailcraft_search::Query::Term(mailcraft_search::Term::Prop(p)) = mailcraft_search::parse(&t)
            else {
                return None;
            };
            let field = FORM_FIELDS
                .iter()
                .find(|(_, l, _)| mailcraft_search::doc::prop_id(l) == p.name)
                .map_or(p.label.clone(), |(_, l, _)| (*l).to_owned());
            let ftype = FORM_FIELDS
                .iter()
                .find(|(_, l, _)| mailcraft_search::doc::prop_id(l) == p.name)
                .map_or(FieldType::Text, |(_, _, t)| *t);
            let sym = p.op.symbol();
            let cond = conditions_of(ftype)
                .iter()
                .find(|(id, _, op)| {
                    *op == sym && (p.value.is_empty() == matches!(*id, "isEmpty" | "isNotEmpty"))
                })
                .map_or("is", |(id, _, _)| id);
            Some(json!({"term": t, "field": field, "condition": cond, "value": p.value}))
        })
        .collect()
}

/// `search.refine {"kind": "field", "field", "condition", "value"}`: adds (or with `replace`, swaps in) a
/// form-field condition.
fn add_field_condition(s: &mut Session, p: &Value) -> CmdResult {
    let term = field_term(
        opt_str(p, "field").unwrap_or_default(),
        opt_str(p, "condition"),
        opt_str(p, "value").unwrap_or_default(),
    )?;
    let mut list = terms(&s.view.search);
    if let Some(old) = opt_str(p, "replace").map(str::trim).filter(|r| !r.is_empty()) {
        list.retain(|t| !t.eq_ignore_ascii_case(old));
    }
    if !list.iter().any(|t| t.eq_ignore_ascii_case(&term)) {
        list.push(term.clone());
    }
    s.view.search = list.join(" ");
    let mut out = summary(s);
    if let Some(o) = out.as_object_mut() {
        o.insert("term".into(), json!(term));
        o.insert("on".into(), json!(true));
    }
    Ok(out)
}

/// The query term of a form-field condition (`prop:"Field<op>value"`): field label from [`FORM_FIELDS`],
/// condition id from [`conditions_of`] (default: the type's first), value checked for the field's type.
/// Advanced Find's "Define more criteria" builds its terms with it too.
pub fn field_term(field: &str, condition: Option<&str>, value: &str) -> Result<String, CmdError> {
    let name = field.trim();
    let Some((_, label, ftype)) = FORM_FIELDS.iter().find(|(_, l, _)| {
        l.eq_ignore_ascii_case(name)
            || mailcraft_search::doc::prop_id(l) == mailcraft_search::doc::prop_id(name)
    }) else {
        return Err(CmdError::Params(format!(
            "unknown field `{}`",
            name.chars().take(60).collect::<String>()
        )));
    };
    let cond = condition.unwrap_or(match ftype {
        FieldType::Text => "contains",
        FieldType::Date => "on",
        FieldType::Number => "equals",
        FieldType::Choice(_) => "is",
    });
    let Some((cid, _, op)) = conditions_of(*ftype).iter().find(|(id, _, _)| *id == cond) else {
        return Err(CmdError::Params(format!("`{label}` has no condition `{cond}`")));
    };
    let no_value = matches!(*cid, "isEmpty" | "isNotEmpty");
    let raw = value.trim().replace('"', "");
    let value = if no_value { String::new() } else { raw };
    if !no_value && value.is_empty() {
        return Err(CmdError::Params(format!("`{label}` {cond} needs a value")));
    }
    let value = match ftype {
        FieldType::Date if !no_value => normal_date(&value)
            .ok_or_else(|| CmdError::Params(format!("`{value}` is not a date (YYYY-MM-DD)")))?,
        FieldType::Number if value.parse::<f64>().is_err() => {
            return Err(CmdError::Params(format!("`{value}` is not a number")));
        }
        FieldType::Choice(list) => match list.iter().find(|c| c.eq_ignore_ascii_case(&value)) {
            Some(c) => (*c).to_owned(),
            None => return Err(CmdError::Params(format!("`{value}` is not a choice of `{label}`"))),
        },
        _ => value,
    };
    Ok(format!("prop:\"{label}{op}{value}\""))
}

/// A date as `YYYY-MM-DD` (accepts `YYYY-M-D` and `YYYY/MM/DD`).
fn normal_date(v: &str) -> Option<String> {
    let parts: Vec<&str> = v.split(['-', '/']).collect();
    let [y, m, d] = parts.as_slice() else { return None };
    let (y, m, d): (i32, u8, u8) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || d == 0
        || i64::from(d) > i64::from(mailcraft_mime::date::days_in_month(i64::from(y), m))
    {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
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

/// Advanced Find › Look for: the item kinds a name stands for (`messages`, the default; `any`;
/// `appointments`, `contacts`, `tasks`, `notes`).
pub fn look_for_kinds(name: &str) -> Result<Vec<mailcraft_search::DocKind>, CmdError> {
    use mailcraft_search::DocKind;
    Ok(match name {
        "messages" | "" => vec![DocKind::Mail],
        "any" => DocKind::ALL.to_vec(),
        "appointments" => vec![DocKind::Appointment],
        "contacts" => vec![DocKind::Contact],
        "tasks" => vec![DocKind::Task],
        "notes" => vec![DocKind::Note],
        other => return Err(CmdError::Params(format!("unknown `lookFor` value `{other}`"))),
    })
}

/// The folder kind holding items of `kind`.
fn folder_kind_of(kind: mailcraft_search::DocKind) -> mailcraft_model::FolderKind {
    use mailcraft_model::FolderKind;
    use mailcraft_search::DocKind;
    match kind {
        DocKind::Mail => FolderKind::Mail,
        DocKind::Appointment => FolderKind::Calendar,
        DocKind::Contact => FolderKind::Contacts,
        DocKind::Task => FolderKind::Tasks,
        DocKind::Note => FolderKind::Notes,
    }
}

/// Advanced Find: without criteria opens the dialog; with criteria runs them over the folder (and its
/// subfolders) with the same query engine as the search box, leaving the main search alone. `lookFor` picks
/// the item kinds ([`look_for_kinds`]): messages search the folder; other kinds search the folder when it
/// holds that kind, else every folder that does (Any item: every folder).
fn advanced_find(s: &mut Session, p: &Value) -> CmdResult {
    let q = advanced_query(p)?;
    let folder = current_folder(s, p)?;
    let kinds = look_for_kinds(opt_str(p, "lookFor").unwrap_or("messages"))?;
    // No criteria and no Look for: the ribbon button, which opens the dialog. The dialog's Find Now always
    // names what to look for, and with no criteria it lists everything of that kind in the scope.
    if q.is_empty() && p.get("lookFor").is_none() {
        let params = json!({ "focus": "words", "folder": folder.0, "recent": s.search.recent });
        return Ok(dialog(s, "advancedFind", params));
    }
    let subtree: Vec<u64> = if opt_bool(p, "subfolders").unwrap_or(true) {
        s.mailbox.subtree(folder).into_iter().map(|f| f.0).collect()
    } else {
        vec![folder.0]
    };
    let mail_only = kinds == [mailcraft_search::DocKind::Mail];
    let folder_kind = s.mailbox.folder(folder).map(|f| f.kind);
    let holds = |k: &mailcraft_search::DocKind| Some(folder_kind_of(*k)) == folder_kind;
    let filter = if mail_only || (kinds.len() == 1 && kinds.iter().any(holds)) {
        mailcraft_search::Filter { folders: Some(subtree.into_iter().collect()), kinds: kinds.clone() }
    } else {
        let wanted: Vec<mailcraft_model::FolderKind> = kinds.iter().map(|k| folder_kind_of(*k)).collect();
        let folders = s.mailbox.folders.values().filter(|f| wanted.contains(&f.kind)).map(|f| f.id.0);
        mailcraft_search::Filter { folders: Some(folders.collect()), kinds: kinds.clone() }
    };
    let ids: Vec<ItemId> = if q.is_empty() {
        every_item(s, &filter)
    } else {
        search_index::hits(s, &q, &filter).iter().map(|h| ItemId(h.id)).collect()
    };
    let limit = opt_u64(p, "limit").and_then(|n| usize::try_from(n).ok()).unwrap_or(500).min(5_000);
    let rows: Vec<Value> = ids.iter().take(limit).filter_map(|id| row(s, *id)).collect();
    Ok(json!({ "query": q, "folder": folder.0, "results": ids.len(), "items": rows }))
}

/// Every item of `filter`'s kinds in its folders: messages newest first, then appointments, contacts,
/// tasks and notes (Advanced Find › Find Now with no criteria).
fn every_item(s: &Session, filter: &mailcraft_search::Filter) -> Vec<ItemId> {
    use mailcraft_search::DocKind;
    let mb = &s.mailbox;
    let wanted = |k: DocKind, folder: mailcraft_model::FolderId| {
        (filter.kinds.is_empty() || filter.kinds.contains(&k))
            && filter.folders.as_ref().is_none_or(|f| f.contains(&folder.0))
    };
    let mut mail: Vec<(i64, ItemId)> = mb
        .mail
        .values()
        .filter(|m| wanted(DocKind::Mail, m.folder))
        .map(|m| (m.received.unix, m.id))
        .collect();
    mail.sort_by(|a, b| b.cmp(a));
    let mut out: Vec<ItemId> = mail.into_iter().map(|(_, id)| id).collect();
    out.extend(mb.appointments.values().filter(|a| wanted(DocKind::Appointment, a.folder)).map(|a| a.id));
    out.extend(mb.contacts.values().filter(|c| wanted(DocKind::Contact, c.folder)).map(|c| c.id));
    out.extend(mb.tasks.values().filter(|t| wanted(DocKind::Task, t.folder)).map(|t| t.id));
    out.extend(mb.notes.values().filter(|n| wanted(DocKind::Note, n.folder)).map(|n| n.id));
    out
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
        // Locations to Search: an unchecked store is left out of All Mailboxes.
        run(&mut s, "search.scope", json!({"store": "Test", "on": false}));
        assert!(results(&s).is_empty());
        run(&mut s, "search.scope", json!({"store": "Test", "on": true}));
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
        // All Items too (regression: it searched Deleted Items whatever Include Deleted Items said).
        s.view.search_scope = SearchScope::AllItems;
        assert!(results(&s).is_empty());
        s.view.search_scope = SearchScope::CurrentMailbox;
        run(&mut s, "search.scope", json!({"includeDeleted": true, "scope": "currentMailbox"}));
        assert_eq!(results(&s).len(), 1);
        run(&mut s, "search.scope", json!({"scope": "allItems"}));
        assert_eq!(results(&s).len(), 1, "Include Deleted Items on: All Items finds it");
        run(&mut s, "search.scope", json!({"includeDeleted": false}));
        assert!(results(&s).is_empty());
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
        // More Filters… enters Search mode; Add Form Field… opens the refiner form.
        assert_eq!(run(&mut s, "search.refine", json!({}))["dialog"], "searchBox");
        assert_eq!(run(&mut s, "search.refine", json!({"kind": "formField"}))["dialog"], "addFormField");
        assert_eq!(run(&mut s, "search.refine", json!({"kind": "refiners"}))["dialog"], "searchRefine");
    }

    #[test]
    fn scope_current_mailbox_from_the_inbox() {
        let mut s = session();
        let inbox = s.selection.folder;
        let acct = s.mailbox.accounts[0].id;
        let sent = s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap();
        assert_eq!(
            crate::settings::def("search.scope").map(|d| d.default_value()),
            Some(json!("currentMailbox"))
        );
        run(&mut s, "options.set", json!({"key": "search.scope", "value": "folderInboxMailbox"}));
        assert_eq!(s.view.search_scope, SearchScope::CurrentMailbox, "the Inbox is selected");
        run(&mut s, "nav.selectFolder", json!({"folder": sent.0}));
        assert_eq!(s.view.search_scope, SearchScope::CurrentFolder);
        run(&mut s, "nav.selectFolder", json!({"folder": inbox.0}));
        assert_eq!(s.view.search_scope, SearchScope::CurrentMailbox);
        assert_eq!(s.settings.text("search.scope"), "folderInboxMailbox", "the rule stays the option");
        // Choosing another scope by hand replaces the rule.
        run(&mut s, "options.set", json!({"key": "search.scope", "value": "allMailboxes"}));
        run(&mut s, "nav.selectFolder", json!({"folder": sent.0}));
        assert_eq!(s.view.search_scope, SearchScope::AllMailboxes);
    }

    #[test]
    fn form_field_conditions() {
        let mut s = session();
        let private = add(
            &mut s,
            MailItem {
                subject: "Salary".into(),
                sensitivity: mailcraft_model::Sensitivity::Private,
                ..Default::default()
            },
        );
        add(&mut s, MailItem { subject: "Menu".into(), ..Default::default() });
        // The chooser lists the forms and their fields.
        run(&mut s, "search.refine", json!({"kind": "formField"}));
        let p = s
            .notices
            .iter()
            .rev()
            .find_map(|n| match n {
                crate::Notice::OpenDialog(d, p) if d == "addFormField" => Some(p.clone()),
                _ => None,
            })
            .unwrap();
        let forms = p["forms"].as_array().unwrap();
        assert_eq!(forms[0]["form"], "Frequently-used fields");
        let all: Vec<&str> = forms
            .iter()
            .flat_map(|f| f["fields"].as_array().unwrap().iter().filter_map(|x| x["label"].as_str()))
            .collect();
        for want in ["Sensitivity", "Due Date", "Message Class", "Company", "Status"] {
            assert!(all.contains(&want), "{want} offered");
        }
        let r = run(
            &mut s,
            "search.refine",
            json!({"kind": "field", "field": "sensitivity", "condition": "is", "value": "private"}),
        );
        assert_eq!(r["term"], "prop:\"Sensitivity=Private\"");
        assert_eq!(crate::search_index::results(&s), vec![private]);
        let rows = field_conditions(&s.view.search);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["field"], "Sensitivity");
        assert_eq!(rows[0]["condition"], "is");
        assert_eq!(rows[0]["value"], "Private");
        // Edit the row: is not.
        let term = rows[0]["term"].as_str().unwrap().to_owned();
        run(
            &mut s,
            "search.refine",
            json!({"kind": "field", "field": "Sensitivity", "condition": "isNot", "value": "Private", "replace": term}),
        );
        assert_eq!(s.view.search, "prop:\"Sensitivity!=Private\"");
        assert_eq!(crate::search_index::results(&s).len(), 1);
        assert!(!crate::search_index::results(&s).contains(&private));
        // Message class and dates; bad values are refused.
        run(&mut s, "search.refine", json!({"kind": "removeField", "term": "prop:\"Sensitivity!=Private\""}));
        assert!(s.view.search.is_empty());
        run(&mut s, "search.refine", json!({"kind": "field", "field": "Message Class", "value": "ipm.note"}));
        assert_eq!(crate::search_index::results(&s).len(), 2);
        run(
            &mut s,
            "search.refine",
            json!({"kind": "field", "field": "Received", "condition": "after", "value": "2000-1-2"}),
        );
        assert!(s.view.search.ends_with("prop:\"Received>2000-01-02\""));
        assert_eq!(crate::search_index::results(&s).len(), 2);
        for bad in [
            json!({"kind": "field", "field": "Shoe Size", "value": "9"}),
            json!({"kind": "field", "field": "Received", "condition": "after", "value": "soon"}),
            json!({"kind": "field", "field": "Size", "condition": "lessThan", "value": "big"}),
            json!({"kind": "field", "field": "Sensitivity", "value": "Secret"}),
            json!({"kind": "field", "field": "Subject", "condition": "contains"}),
            json!({"kind": "field", "field": "Subject", "condition": "before", "value": "x"}),
            json!({"kind": "removeField", "term": "nope"}),
        ] {
            assert!(s.run("search.refine", &bad).is_err(), "{bad}");
        }
        run(&mut s, "search.refine", json!({"kind": "field", "field": "Company", "condition": "isNotEmpty"}));
        assert!(s.view.search.ends_with("prop:\"Company!=\""));
        assert_eq!(field_conditions(&s.view.search).last().unwrap()["condition"], "isNotEmpty");
    }

    fn search_box(s: &Session) -> Value {
        s.notices
            .iter()
            .rev()
            .find_map(|n| match n {
                crate::Notice::OpenDialog(d, p) if d == "searchBox" => Some(p.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null)
    }

    #[test]
    fn value_refiners_put_the_caret_between_the_quotes() {
        let mut s = session();
        add(
            &mut s,
            MailItem { subject: "budget".into(), from: Some(addr("ann@example.com")), ..Default::default() },
        );
        run(&mut s, "search.query", json!({"text": "budget"}));
        let r = run(&mut s, "search.refine", json!({"kind": "from"}));
        assert_eq!(s.view.search, "budget from:\"\"");
        // "budget " is 7 chars, then `from:"`.
        assert_eq!(r["edit"], json!([13, 13]));
        assert_eq!(search_box(&s)["select"], json!([13, 13]));
        assert_eq!(r["results"], 1, "an empty value filters nothing");
        // Again: the empty term stays once and the caret goes back into it.
        let r = run(&mut s, "search.refine", json!({"kind": "from"}));
        assert_eq!(s.view.search, "budget from:\"\"");
        assert_eq!(r["edit"], json!([13, 13]));
        let r = run(&mut s, "search.refine", json!({"kind": "subject"}));
        assert_eq!(s.view.search, "budget from:\"\" subject:\"\"");
        assert_eq!(r["edit"], json!([24, 24]));
        let r = run(&mut s, "search.refine", json!({"kind": "sentTo", "value": "other"}));
        assert!(s.view.search.ends_with(" to:\"\""));
        let caret = r["edit"][0].as_u64().unwrap() as usize;
        assert_eq!(
            s.view.search.chars().skip(caret.saturating_sub(4)).take(5).collect::<String>(),
            "to:\"\""
        );
        // Non-ASCII text before the term: offsets count chars, not bytes.
        run(&mut s, "search.query", json!({"text": "café"}));
        let r = run(&mut s, "search.refine", json!({"kind": "cc"}));
        assert_eq!(r["edit"], json!([9, 9]));
        // A value given runs as before.
        run(&mut s, "search.refine", json!({"kind": "from", "value": "ann"}));
        assert!(s.view.search.ends_with("from:ann"));
    }

    #[test]
    fn locations_to_search_include_and_leave_out_stores() {
        let mut s = session();
        add(&mut s, MailItem { subject: "Budget".into(), ..Default::default() });
        let acct = s.mailbox.add_account(mailcraft_model::Account {
            kind: mailcraft_model::AccountKind::Local,
            display_name: "Archive file".into(),
            ..Default::default()
        });
        s.mailbox.create_default_folders(acct);
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "Budget 2019".into(), ..Default::default() });
        run(&mut s, "search.query", json!({"text": "budget"}));
        run(&mut s, "search.scope", json!({"scope": "allMailboxes"}));
        assert_eq!(results(&s).len(), 2);
        let names: Vec<(String, bool)> = locations(&s).into_iter().map(|(k, _, on)| (k, on)).collect();
        assert!(names.iter().all(|(_, on)| *on), "{names:?}");
        // Unchecked: left out of All Mailboxes and All Items, kept for the current mailbox.
        let key = locations(&s).last().map(|(k, _, _)| k.clone()).unwrap();
        let r = run(&mut s, "search.scope", json!({"store": key, "on": false}));
        assert_eq!(r["on"], false);
        assert_eq!(results(&s).len(), 1);
        s.view.search_scope = SearchScope::AllItems;
        assert_eq!(results(&s).len(), 1);
        assert!(!locations(&s).last().unwrap().2);
        // Saved with the profile settings.
        assert_eq!(s.settings.list(search_index::EXCLUDED_STORES_SETTING).len(), 1);
        let r = run(&mut s, "search.scope", json!({"store": key.to_uppercase(), "on": true}));
        assert_eq!(r["on"], true);
        assert_eq!(results(&s).len(), 2);
        assert!(s.settings.list(search_index::EXCLUDED_STORES_SETTING).is_empty());
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
        assert!(s.run("search.advancedFind", &json!({"words": "x", "lookFor": "spaceships"})).is_err());
    }

    #[test]
    fn advanced_find_looks_for_other_item_kinds() {
        let mut s = crate::sample::sample_session();
        let inbox = s.selection.folder;
        // Contacts from the Inbox: every contacts folder is searched.
        let name = s.mailbox.contacts.values().next().map(|c| c.last_name.clone()).unwrap();
        let r =
            run(&mut s, "search.advancedFind", json!({"words": name, "in": "all", "lookFor": "contacts"}));
        assert!(r["results"].as_u64().unwrap() >= 1, "{r}");
        assert!(r["items"].as_array().unwrap().iter().all(|i| i["kind"] == "contact"), "{r}");
        // Messages only look in the folder.
        let r = run(&mut s, "search.advancedFind", json!({"words": name, "in": "all", "folder": inbox.0}));
        assert!(r["items"].as_array().unwrap().iter().all(|i| i["kind"] == "email"), "{r}");
        // Any item: mail and the rest together.
        let subject = s.mailbox.appointments.values().next().map(|a| a.subject.clone()).unwrap();
        let word = subject.split_whitespace().next().unwrap().to_owned();
        let r = run(&mut s, "search.advancedFind", json!({"words": word, "in": "all", "lookFor": "any"}));
        assert!(r["items"].as_array().unwrap().iter().any(|i| i["kind"] == "meeting"), "{r}");
        let r = run(
            &mut s,
            "search.advancedFind",
            json!({"words": word, "in": "all", "lookFor": "appointments"}),
        );
        assert!(r["items"].as_array().unwrap().iter().all(|i| i["kind"] == "meeting"), "{r}");
        assert!(look_for_kinds("tasks").is_ok() && look_for_kinds("notes").is_ok());
    }

    #[test]
    fn find_now_without_criteria_lists_every_item_of_the_kind() {
        let mut s = crate::sample::sample_session();
        let inbox = s.selection.folder;
        s.notices.clear();
        // Look for: Contacts, nothing typed: every contact, and no second Advanced Find window.
        let r = run(
            &mut s,
            "search.advancedFind",
            json!({"query": "", "lookFor": "contacts", "folder": inbox.0}),
        );
        assert_eq!(r["results"].as_u64().unwrap() as usize, s.mailbox.contacts.len(), "{r}");
        assert!(r["items"].as_array().unwrap().iter().all(|i| i["kind"] == "contact"), "{r}");
        assert!(r.get("dialog").is_none(), "{r}");
        assert!(
            !s.notices
                .iter()
                .any(|n| matches!(n, crate::Notice::OpenDialog(name, _) if name == "advancedFind"))
        );
        // Messages: everything in the folder (and its subfolders), newest first.
        let r = run(
            &mut s,
            "search.advancedFind",
            json!({"query": "", "lookFor": "messages", "folder": inbox.0, "subfolders": false}),
        );
        assert_eq!(r["results"].as_u64().unwrap() as usize, s.mailbox.items_in(inbox).len());
        let when: Vec<i64> =
            r["items"].as_array().unwrap().iter().map(|i| i["received"].as_i64().unwrap()).collect();
        assert!(when.windows(2).all(|w| w[0] >= w[1]));
        // The ribbon button (no Look for) still opens the dialog.
        assert_eq!(run(&mut s, "search.advancedFind", json!({}))["dialog"], "advancedFind");
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
        // Per kind, not only the mail (regression: the window showed the message count alone).
        assert_eq!(r["kinds"][0], json!({"kind": "mail", "label": "Mail", "count": 1}));
        assert_eq!(r["kinds"].as_array().unwrap().len(), 5);
        assert_eq!(r["items"], r["indexed"]);
        assert!(r["downloading"].is_null(), "nothing is being downloaded");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "indexingStatus"));
        assert!(s.run("search.searchTools", &json!({"tool": "x"})).is_err());
        assert!(matches!(s.run("search.includeOlderResults", &json!({})), Err(CmdError::Disabled(_))));
        assert_eq!(s.registry.by_shortcut("Mod+E").map(|c| c.id), Some("search.focus"));
    }
}
