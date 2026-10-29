//! JMAP client (RFC 8620 core, RFC 8621 mail) over the crate's HTTP/1.1 client: session discovery,
//! Mailbox/get, Email/query + Email/get (with result references), Email/changes, Email/set, blob upload and
//! download, Email/import, Identity/get and EmailSubmission/set.

use std::collections::BTreeMap;

use mailcraft_model::FolderRole;
use serde_json::{Map, Value, json};

use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpResponse, Url, percent_encode};
use crate::smtp::{DsnRequest, take_submit_options, xtext};

pub const CORE: &str = "urn:ietf:params:jmap:core";
pub const MAIL: &str = "urn:ietf:params:jmap:mail";
pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
/// RFC 9425 quotas.
pub const QUOTA: &str = "urn:ietf:params:jmap:quota";
/// RFC 9610 contacts (JSContact cards in address books).
pub const CONTACTS: &str = "urn:ietf:params:jmap:contacts";
/// JMAP for Calendars (JSCalendar events in calendars).
pub const CALENDARS: &str = "urn:ietf:params:jmap:calendars";

/// Sets `key` of the JSON object `v` (nothing when `v` is not an object).
fn set(v: &mut Value, key: &str, x: Value) {
    if let Some(m) = v.as_object_mut() {
        m.insert(key.to_owned(), x);
    }
}

/// How to authenticate HTTP requests.
#[derive(Clone, PartialEq, Eq)]
pub enum JmapAuth {
    Bearer(String),
    Basic { user: String, password: String },
}

impl std::fmt::Debug for JmapAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JmapAuth::Bearer(_) => f.write_str("Bearer(..)"),
            JmapAuth::Basic { user, .. } => write!(f, "Basic {{ user: {user:?}, .. }}"),
        }
    }
}

impl JmapAuth {
    fn header(&self) -> String {
        match self {
            JmapAuth::Bearer(t) => format!("Bearer {t}"),
            JmapAuth::Basic { user, password } => {
                format!("Basic {}", crate::sasl::b64(format!("{user}:{password}").as_bytes()))
            }
        }
    }
}

/// The JMAP Session resource (RFC 8620 §2).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JmapSession {
    pub username: String,
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    pub event_source_url: String,
    pub state: String,
    /// Primary account for `urn:ietf:params:jmap:mail`.
    pub account_id: String,
    pub capabilities: Vec<String>,
    /// The whole document.
    pub raw: Value,
}

impl JmapSession {
    /// Parses a session document.
    pub fn from_json(v: &Value) -> Result<JmapSession> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let account_id = v
            .get("primaryAccounts")
            .and_then(|p| p.get(MAIL))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| NetError::Protocol("JMAP session has no primary mail account".into()))?;
        let api_url = s("apiUrl");
        if api_url.is_empty() {
            return Err(NetError::Protocol("JMAP session has no apiUrl".into()));
        }
        Ok(JmapSession {
            username: s("username"),
            api_url,
            download_url: s("downloadUrl"),
            upload_url: s("uploadUrl"),
            event_source_url: s("eventSourceUrl"),
            state: s("state"),
            account_id,
            capabilities: v
                .get("capabilities")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            raw: v.clone(),
        })
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

/// One method call or response: `[name, arguments, call id]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub name: String,
    pub args: Value,
    pub id: String,
}

impl Invocation {
    pub fn new(name: &str, args: Value, id: &str) -> Self {
        Invocation { name: name.into(), args, id: id.into() }
    }
    fn to_json(&self) -> Value {
        json!([self.name, self.args, self.id])
    }
    fn from_json(v: &Value) -> Option<Invocation> {
        let a = v.as_array()?;
        Some(Invocation {
            name: a.first()?.as_str()?.to_owned(),
            args: a.get(1)?.clone(),
            id: a.get(2)?.as_str()?.to_owned(),
        })
    }
    /// Turns an `error` response into [`NetError::Jmap`].
    pub fn into_result(self) -> Result<Invocation> {
        if self.name == "error" { Err(method_error(&self.args)) } else { Ok(self) }
    }
}

/// The tightest mail quotas of a Quota/get list: octets give the storage, counts the messages.
pub fn quota_from_list(list: &[Value]) -> Option<crate::sync::Quota> {
    let mut storage: Option<(u64, u64)> = None;
    let mut count: Option<(u64, u64)> = None;
    for q in list.iter().take(256) {
        let mail = q.get("types").and_then(Value::as_array).is_none_or(|t| {
            t.is_empty() || t.iter().any(|x| x.as_str().is_some_and(|s| s.eq_ignore_ascii_case("Mail")))
        });
        let (Some(used), Some(limit)) =
            (q.get("used").and_then(Value::as_u64), q.get("hardLimit").and_then(Value::as_u64))
        else {
            continue;
        };
        if !mail || limit == 0 {
            continue;
        }
        let slot = match q.get("resourceType").and_then(Value::as_str) {
            Some("octets") => &mut storage,
            Some("count") => &mut count,
            _ => continue,
        };
        let tighter = match *slot {
            Some((u, l)) => u128::from(used) * u128::from(l) > u128::from(u) * u128::from(limit),
            None => true,
        };
        if tighter {
            *slot = Some((used, limit));
        }
    }
    if storage.is_none() && count.is_none() {
        return None;
    }
    let (used, limit) = storage.unwrap_or((0, 0));
    Some(crate::sync::Quota { used, limit, messages: count })
}

fn method_error(v: &Value) -> NetError {
    NetError::Jmap {
        kind: v.get("type").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
        description: v.get("description").and_then(Value::as_str).unwrap_or_default().to_owned(),
    }
}

/// A mailbox (RFC 8621 §2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JmapMailbox {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
    pub sort_order: u64,
    pub total_emails: u64,
    pub unread_emails: u64,
    pub is_subscribed: bool,
}

impl JmapMailbox {
    /// The folder role for this mailbox's JMAP role.
    pub fn folder_role(&self) -> Option<FolderRole> {
        role_to_folder(self.role.as_deref()?)
    }
    fn from_json(v: &Value) -> Option<JmapMailbox> {
        Some(JmapMailbox {
            id: v.get("id")?.as_str()?.to_owned(),
            name: v.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            parent_id: v.get("parentId").and_then(Value::as_str).map(str::to_owned),
            role: v.get("role").and_then(Value::as_str).map(str::to_owned),
            sort_order: v.get("sortOrder").and_then(Value::as_u64).unwrap_or(0),
            total_emails: v.get("totalEmails").and_then(Value::as_u64).unwrap_or(0),
            unread_emails: v.get("unreadEmails").and_then(Value::as_u64).unwrap_or(0),
            is_subscribed: v.get("isSubscribed").and_then(Value::as_bool).unwrap_or(true),
        })
    }
}

/// Maps a JMAP mailbox role (IANA "IMAP Mailbox Name Attributes" registry, lower-case) to a folder role.
pub fn role_to_folder(role: &str) -> Option<FolderRole> {
    match role.to_ascii_lowercase().as_str() {
        "inbox" => Some(FolderRole::Inbox),
        "drafts" => Some(FolderRole::Drafts),
        "sent" => Some(FolderRole::SentItems),
        "trash" => Some(FolderRole::DeletedItems),
        "junk" => Some(FolderRole::JunkEmail),
        "archive" => Some(FolderRole::Archive),
        _ => None,
    }
}

/// Email/query result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailQuery {
    pub ids: Vec<String>,
    pub query_state: String,
    pub position: u64,
    pub total: Option<u64>,
}

/// Foo/changes result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub old_state: String,
    pub new_state: String,
    pub has_more_changes: bool,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

/// A per-object error from Foo/set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetError {
    pub kind: String,
    pub description: String,
}

/// Foo/set result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetResult {
    pub new_state: String,
    /// Creation id → created object (server-set properties, including `id`).
    pub created: BTreeMap<String, Value>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub not_created: BTreeMap<String, SetError>,
    pub not_updated: BTreeMap<String, SetError>,
    pub not_destroyed: BTreeMap<String, SetError>,
}

impl SetResult {
    /// Reads a Foo/set response.
    pub fn from_json(v: &Value) -> SetResult {
        let errors = |k: &str| -> BTreeMap<String, SetError> {
            v.get(k)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(id, e)| {
                            let kind = e.get("type").and_then(Value::as_str).unwrap_or("unknown").to_owned();
                            let description =
                                e.get("description").and_then(Value::as_str).unwrap_or_default().to_owned();
                            (id.clone(), SetError { kind, description })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let keys = |k: &str| -> Vec<String> {
            match v.get(k) {
                Some(Value::Object(m)) => m.keys().cloned().collect(),
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                _ => Vec::new(),
            }
        };
        SetResult {
            new_state: v.get("newState").and_then(Value::as_str).unwrap_or_default().to_owned(),
            created: v
                .get("created")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            updated: keys("updated"),
            destroyed: keys("destroyed"),
            not_created: errors("notCreated"),
            not_updated: errors("notUpdated"),
            not_destroyed: errors("notDestroyed"),
        }
    }

    /// The first per-object error, if any.
    pub fn first_error(&self) -> Option<(&String, &SetError)> {
        self.not_created.iter().chain(self.not_updated.iter()).chain(self.not_destroyed.iter()).next()
    }

    fn ok(self) -> Result<SetResult> {
        match self.first_error() {
            Some((id, e)) => {
                Err(NetError::Jmap { kind: e.kind.clone(), description: format!("{id}: {}", e.description) })
            }
            None => Ok(self),
        }
    }
}

/// A sending identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// A JMAP connection (stateless HTTP; the session is cached).
#[derive(Debug)]
pub struct JmapClient {
    http: HttpClient,
    auth: JmapAuth,
    session: JmapSession,
}

impl JmapClient {
    /// Fetches the session. `url` is the session URL, or a bare origin (`https://host`) to use
    /// `/.well-known/jmap` (RFC 8620 §2.2).
    pub fn connect(url: &str, auth: JmapAuth, http: HttpClient) -> Result<JmapClient> {
        let mut u = Url::parse(url)?;
        if u.path == "/" {
            u.path = "/.well-known/jmap".into();
        }
        let auth_header = auth.header();
        let resp =
            http.get(&u.to_string(), &[("Authorization", &auth_header), ("Accept", "application/json")])?;
        let resp = check_auth(resp)?;
        let session = JmapSession::from_json(&resp.json()?)?;
        // Resolve relative URLs against where the session came from.
        let base = Url::parse(&resp.url)?;
        let abs = |s: &str| -> String {
            if s.is_empty() || s.contains("://") {
                s.to_owned()
            } else {
                base.join(s).map(|u| u.to_string()).unwrap_or_else(|_| s.to_owned())
            }
        };
        let session = JmapSession {
            api_url: abs(&session.api_url),
            download_url: abs(&session.download_url),
            upload_url: abs(&session.upload_url),
            event_source_url: abs(&session.event_source_url),
            ..session
        };
        Ok(JmapClient { http, auth, session })
    }

    pub fn session(&self) -> &JmapSession {
        &self.session
    }

    pub fn account_id(&self) -> &str {
        &self.session.account_id
    }

    /// Sends a request with several method calls; returns the method responses in order. The capabilities
    /// the request uses follow from the method names.
    pub fn call(&self, calls: &[Invocation]) -> Result<Vec<Invocation>> {
        self.call_using(&[], calls)
    }

    /// One method call that also uses the capabilities `extra`; returns its arguments, or the method error.
    pub fn call_with(&self, extra: &[&str], name: &str, args: Value) -> Result<Value> {
        let responses = self.call_using(extra, &[Invocation::new(name, args, "c0")])?;
        let r = responses
            .into_iter()
            .find(|r| r.id == "c0")
            .ok_or_else(|| NetError::Protocol(format!("no response to {name}")))?;
        Ok(r.into_result()?.args)
    }

    fn call_using(&self, extra: &[&str], calls: &[Invocation]) -> Result<Vec<Invocation>> {
        let mut using = vec![CORE, MAIL];
        let any = |prefixes: &[&str]| calls.iter().any(|c| prefixes.iter().any(|p| c.name.starts_with(p)));
        if any(&["EmailSubmission/", "Identity/"]) {
            using.push(SUBMISSION);
        }
        if any(&["Quota/"]) {
            using.push(QUOTA);
        }
        if any(&["AddressBook/", "ContactCard/"]) {
            using.push(CONTACTS);
        }
        if any(&["Calendar/", "CalendarEvent/", "ParticipantIdentity/", "CalendarEventNotification/"]) {
            using.push(CALENDARS);
        }
        for e in extra {
            if !using.contains(e) {
                using.push(e);
            }
        }
        let body = json!({
            "using": using,
            "methodCalls": calls.iter().map(Invocation::to_json).collect::<Vec<_>>(),
        });
        let auth = self.auth.header();
        let resp = self.http.post_json(
            &self.session.api_url,
            &[("Authorization", &auth), ("Accept", "application/json")],
            &body,
        )?;
        let resp = check_auth(resp)?;
        if !resp.is_success() {
            // RFC 7807 problem details for request-level errors.
            if let Ok(v) = resp.json()
                && v.get("type").is_some()
            {
                return Err(NetError::Jmap {
                    kind: v.get("type").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    description: v.get("detail").and_then(Value::as_str).unwrap_or_default().to_owned(),
                });
            }
            return Err(NetError::Http { status: resp.status, body: resp.text_snippet() });
        }
        let v = resp.json()?;
        let list = v
            .get("methodResponses")
            .and_then(Value::as_array)
            .ok_or_else(|| NetError::Protocol("JMAP response without methodResponses".into()))?;
        Ok(list.iter().filter_map(Invocation::from_json).collect())
    }

    /// One method call; returns its arguments, or the method error.
    pub fn call_one(&self, name: &str, args: Value) -> Result<Value> {
        let responses = self.call(&[Invocation::new(name, args, "c0")])?;
        let r = responses
            .into_iter()
            .find(|r| r.id == "c0")
            .ok_or_else(|| NetError::Protocol(format!("no response to {name}")))?;
        Ok(r.into_result()?.args)
    }

    /// The primary account for capability `cap` (the mail account when the session names none).
    pub fn primary_account(&self, cap: &str) -> String {
        self.session
            .raw
            .get("primaryAccounts")
            .and_then(|p| p.get(cap))
            .and_then(Value::as_str)
            .unwrap_or(&self.session.account_id)
            .to_owned()
    }

    /// Foo/get in `account` of `ids` (`None`: all objects), with the type's state string.
    pub fn get_objects(
        &self,
        method: &str,
        account: &str,
        ids: Option<&[String]>,
        properties: Option<&[&str]>,
    ) -> Result<(Vec<Value>, String)> {
        let mut args = json!({ "accountId": account, "ids": ids });
        if let Some(p) = properties {
            set(&mut args, "properties", json!(p));
        }
        let r = self.call_one(method, args)?;
        Ok((
            r.get("list").and_then(Value::as_array).cloned().unwrap_or_default(),
            r.get("state").and_then(Value::as_str).unwrap_or_default().to_owned(),
        ))
    }

    /// Foo/set in `account`; `extra` holds further arguments (e.g. `onDestroyRemoveEmails`).
    pub fn set_objects(
        &self,
        method: &str,
        account: &str,
        create: Map<String, Value>,
        update: Map<String, Value>,
        destroy: &[String],
        extra: Map<String, Value>,
    ) -> Result<SetResult> {
        let mut args = extra;
        args.insert("accountId".into(), Value::String(account.to_owned()));
        if !create.is_empty() {
            args.insert("create".into(), Value::Object(create));
        }
        if !update.is_empty() {
            args.insert("update".into(), Value::Object(update));
        }
        if !destroy.is_empty() {
            args.insert("destroy".into(), json!(destroy));
        }
        let r = self.call_one(method, Value::Object(args))?;
        Ok(SetResult::from_json(&r))
    }

    fn with_account(&self, mut args: Value) -> Value {
        if let Some(m) = args.as_object_mut() {
            m.insert("accountId".into(), Value::String(self.session.account_id.clone()));
        }
        args
    }

    /// Quota/get (RFC 9425) for the mail account: `Ok(None)` when the server has no quota capability or
    /// reports no mail limits.
    pub fn quota(&self) -> Result<Option<crate::sync::Quota>> {
        if !self.session.has_capability(QUOTA) {
            return Ok(None);
        }
        let account = self
            .session
            .raw
            .get("primaryAccounts")
            .and_then(|p| p.get(QUOTA))
            .and_then(Value::as_str)
            .unwrap_or(&self.session.account_id)
            .to_owned();
        let r = self.call_one("Quota/get", json!({ "accountId": account, "ids": null }))?;
        let list = r.get("list").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(quota_from_list(&list))
    }

    /// Mailbox/get for all mailboxes.
    pub fn mailboxes(&self) -> Result<Vec<JmapMailbox>> {
        let r = self.call_one("Mailbox/get", self.with_account(json!({ "ids": null })))?;
        Ok(r.get("list")
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(JmapMailbox::from_json).collect())
            .unwrap_or_default())
    }

    /// Mailbox/get for all mailboxes, with the state string for Mailbox/changes.
    pub fn mailboxes_with_state(&self) -> Result<(Vec<JmapMailbox>, String)> {
        let r = self.call_one("Mailbox/get", self.with_account(json!({ "ids": null })))?;
        let list = r
            .get("list")
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(JmapMailbox::from_json).collect())
            .unwrap_or_default();
        Ok((list, r.get("state").and_then(Value::as_str).unwrap_or_default().to_owned()))
    }

    /// Email/get (even of no ids, to learn the state), with the state string for Email/changes.
    pub fn get_emails_with_state(&self, ids: &[String], properties: &[&str]) -> Result<(Vec<Value>, String)> {
        let r =
            self.call_one("Email/get", self.with_account(json!({ "ids": ids, "properties": properties })))?;
        Ok((
            r.get("list").and_then(Value::as_array).cloned().unwrap_or_default(),
            r.get("state").and_then(Value::as_str).unwrap_or_default().to_owned(),
        ))
    }

    /// Email/query.
    pub fn query_emails(&self, filter: Value, sort: Value, position: u64, limit: u64) -> Result<EmailQuery> {
        let args = self.with_account(json!({
            "filter": filter, "sort": sort, "position": position, "limit": limit, "calculateTotal": true
        }));
        Ok(parse_query(&self.call_one("Email/query", args)?))
    }

    /// Email/get (`properties` `None` = server default set).
    pub fn get_emails(&self, ids: &[String], properties: Option<&[&str]>) -> Result<Vec<Value>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = json!({ "ids": ids });
        if let (Some(p), Some(m)) = (properties, args.as_object_mut()) {
            m.insert("properties".into(), json!(p));
        }
        let r = self.call_one("Email/get", self.with_account(args))?;
        Ok(r.get("list").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Email/query then Email/get of the result in one request (RFC 8620 §3.7 result reference).
    pub fn query_and_get(
        &self,
        filter: Value,
        sort: Value,
        limit: u64,
        properties: &[&str],
    ) -> Result<(EmailQuery, Vec<Value>)> {
        let q = Invocation::new(
            "Email/query",
            self.with_account(
                json!({ "filter": filter, "sort": sort, "limit": limit, "calculateTotal": true }),
            ),
            "q",
        );
        let g = Invocation::new(
            "Email/get",
            self.with_account(json!({
                "#ids": { "resultOf": "q", "name": "Email/query", "path": "/ids" },
                "properties": properties,
            })),
            "g",
        );
        let mut query = None;
        let mut list = Vec::new();
        for r in self.call(&[q, g])? {
            let r = r.into_result()?;
            match r.id.as_str() {
                "q" => query = Some(parse_query(&r.args)),
                "g" => list = r.args.get("list").and_then(Value::as_array).cloned().unwrap_or_default(),
                _ => {}
            }
        }
        Ok((query.ok_or_else(|| NetError::Protocol("no Email/query response".into()))?, list))
    }

    /// Foo/changes (`kind` = "Email", "Mailbox", …).
    pub fn changes(&self, kind: &str, since_state: &str, max: Option<u64>) -> Result<Changes> {
        let mut args = json!({ "sinceState": since_state });
        if let (Some(n), Some(m)) = (max, args.as_object_mut()) {
            m.insert("maxChanges".into(), json!(n));
        }
        let r = self.call_one(&format!("{kind}/changes"), self.with_account(args))?;
        let ids = |k: &str| -> Vec<String> {
            r.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        Ok(Changes {
            old_state: r.get("oldState").and_then(Value::as_str).unwrap_or_default().to_owned(),
            new_state: r.get("newState").and_then(Value::as_str).unwrap_or_default().to_owned(),
            has_more_changes: r.get("hasMoreChanges").and_then(Value::as_bool).unwrap_or(false),
            created: ids("created"),
            updated: ids("updated"),
            destroyed: ids("destroyed"),
        })
    }

    /// Email/set with raw create/update/destroy arguments. Per-object failures are in the result.
    pub fn set_emails(
        &self,
        create: Map<String, Value>,
        update: Map<String, Value>,
        destroy: &[String],
    ) -> Result<SetResult> {
        let mut args = Map::new();
        if !create.is_empty() {
            args.insert("create".into(), Value::Object(create));
        }
        if !update.is_empty() {
            args.insert("update".into(), Value::Object(update));
        }
        if !destroy.is_empty() {
            args.insert("destroy".into(), json!(destroy));
        }
        let r = self.call_one("Email/set", self.with_account(Value::Object(args)))?;
        Ok(SetResult::from_json(&r))
    }

    /// Sets or clears one keyword (`$seen`, `$flagged`, …) on emails.
    pub fn set_keyword(&self, ids: &[String], keyword: &str, on: bool) -> Result<()> {
        let mut update = Map::new();
        for id in ids {
            update.insert(id.clone(), json!({ format!("keywords/{}", pointer_escape(keyword)): if on { json!(true) } else { Value::Null } }));
        }
        self.set_emails(Map::new(), update, &[])?.ok().map(|_| ())
    }

    /// Moves emails from one mailbox to another.
    pub fn move_emails(&self, ids: &[String], from: &str, to: &str) -> Result<()> {
        let mut update = Map::new();
        for id in ids {
            update.insert(
                id.clone(),
                json!({
                    format!("mailboxIds/{}", pointer_escape(from)): null,
                    format!("mailboxIds/{}", pointer_escape(to)): true,
                }),
            );
        }
        self.set_emails(Map::new(), update, &[])?.ok().map(|_| ())
    }

    /// Destroys emails (permanent delete).
    pub fn destroy_emails(&self, ids: &[String]) -> Result<()> {
        self.set_emails(Map::new(), Map::new(), ids)?.ok().map(|_| ())
    }

    fn expand(&self, template: &str, vars: &[(&str, &str)]) -> String {
        let mut s = template.to_owned();
        for (k, v) in vars {
            s = s.replace(&format!("{{{k}}}"), &percent_encode(v));
        }
        s
    }

    /// The push URL (RFC 8620 §7.3) for `types` ("*" = all) with a ping every `ping_secs` seconds, or `None`
    /// when the server offers no event source.
    pub fn event_source_url(&self, types: &[&str], ping_secs: u32) -> Option<String> {
        let t = &self.session.event_source_url;
        if t.trim().is_empty() {
            return None;
        }
        let types = if types.is_empty() { "*".to_owned() } else { types.join(",") };
        let ping = ping_secs.to_string();
        Some(self.expand(t, &[("types", &types), ("closeafter", "no"), ("ping", &ping)]))
    }

    /// Opens the push event stream (see [`Self::event_source_url`]); [`NetError::Unsupported`] when the
    /// server has none.
    pub fn open_events(&self, types: &[&str], ping_secs: u32) -> Result<crate::eventsource::EventStream> {
        let url = self
            .event_source_url(types, ping_secs)
            .ok_or_else(|| NetError::Unsupported("JMAP push (no eventSourceUrl)".into()))?;
        let auth = self.auth.header();
        crate::eventsource::EventStream::open(&self.http, &url, &[("Authorization", &auth)])
    }

    /// Uploads a blob; returns its id.
    pub fn upload(&self, data: &[u8], content_type: &str) -> Result<String> {
        let url = self.expand(&self.session.upload_url, &[("accountId", &self.session.account_id)]);
        let auth = self.auth.header();
        let resp = self.http.request(
            "POST",
            &url,
            &[("Authorization", &auth), ("Content-Type", content_type), ("Accept", "application/json")],
            Some(data),
        )?;
        let v = check_auth(resp)?.error_for_status()?.json()?;
        v.get("blobId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| NetError::Protocol("upload response without blobId".into()))
    }

    /// Downloads a blob.
    pub fn download(&self, blob_id: &str, name: &str, content_type: &str) -> Result<Vec<u8>> {
        let url = self.expand(
            &self.session.download_url,
            &[
                ("accountId", &self.session.account_id),
                ("blobId", blob_id),
                ("name", name),
                ("type", content_type),
            ],
        );
        let auth = self.auth.header();
        Ok(check_auth(self.http.get(&url, &[("Authorization", &auth)])?)?.error_for_status()?.body)
    }

    /// Email/import of an uploaded RFC 5322 blob; returns the new email id.
    pub fn import_email(&self, blob_id: &str, mailbox_ids: &[&str], keywords: &[&str]) -> Result<String> {
        let mb: Map<String, Value> = mailbox_ids.iter().map(|m| ((*m).to_owned(), json!(true))).collect();
        let kw: Map<String, Value> = keywords.iter().map(|k| ((*k).to_owned(), json!(true))).collect();
        let args = self.with_account(json!({
            "emails": { "i0": { "blobId": blob_id, "mailboxIds": mb, "keywords": kw } }
        }));
        let r = self.call_one("Email/import", args)?;
        let set = SetResult::from_json(&r).ok()?;
        set.created
            .get("i0")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| NetError::Protocol("Email/import did not return an id".into()))
    }

    /// Identity/get.
    pub fn identities(&self) -> Result<Vec<Identity>> {
        let r = self.call_one("Identity/get", self.with_account(json!({ "ids": null })))?;
        Ok(r.get("list")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|i| {
                        Some(Identity {
                            id: i.get("id")?.as_str()?.to_owned(),
                            name: i.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            email: i.get("email").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// True when the server's submission capability lists SMTP extension `name` (RFC 8621 section 1.3.2,
    /// `submissionExtensions`), e.g. `DSN`.
    pub fn submission_extension(&self, name: &str) -> bool {
        self.session
            .raw
            .get("accounts")
            .and_then(|a| a.get(&self.session.account_id))
            .and_then(|a| a.get("accountCapabilities"))
            .and_then(|c| c.get(SUBMISSION))
            .and_then(|c| c.get("submissionExtensions"))
            .and_then(Value::as_object)
            .is_some_and(|m| m.keys().any(|k| k.eq_ignore_ascii_case(name)))
    }

    /// EmailSubmission/set create. `envelope` overrides the server-derived envelope; `on_success_move`
    /// (from drafts, to sent) files the email and clears `$draft` after sending.
    pub fn submit(
        &self,
        identity_id: &str,
        email_id: &str,
        envelope: Option<(&str, &[&str])>,
        on_success_move: Option<(&str, &str)>,
    ) -> Result<String> {
        self.submit_with(identity_id, email_id, envelope, None, on_success_move)
    }

    /// [`JmapClient::submit`] with a delivery status notification request: with an explicit envelope it
    /// becomes the envelope's SMTP parameters (RFC 8621 section 7: `RET`/`ENVID` on `mailFrom`,
    /// `NOTIFY`/`ORCPT` on each `rcptTo`, as RFC 3461 writes them).
    pub fn submit_with(
        &self,
        identity_id: &str,
        email_id: &str,
        envelope: Option<(&str, &[&str])>,
        dsn: Option<&DsnRequest>,
        on_success_move: Option<(&str, &str)>,
    ) -> Result<String> {
        let mut sub = json!({ "identityId": identity_id, "emailId": email_id });
        if let (Some((from, rcpts)), Some(m)) = (envelope, sub.as_object_mut()) {
            let mail_from = match dsn {
                Some(d) => {
                    let mut params = Map::new();
                    params.insert("RET".into(), json!(if d.headers_only { "HDRS" } else { "FULL" }));
                    if let Some(id) = &d.envid {
                        params.insert("ENVID".into(), json!(id));
                    }
                    json!({ "email": from, "parameters": params })
                }
                None => json!({ "email": from }),
            };
            let rcpt_to: Vec<Value> = rcpts
                .iter()
                .map(|r| match dsn {
                    Some(d) if r.is_ascii() => json!({ "email": r, "parameters": {
                        "NOTIFY": d.notify(),
                        "ORCPT": format!("rfc822;{}", xtext(r)),
                    } }),
                    Some(d) => json!({ "email": r, "parameters": { "NOTIFY": d.notify() } }),
                    None => json!({ "email": r }),
                })
                .collect();
            m.insert("envelope".into(), json!({ "mailFrom": mail_from, "rcptTo": rcpt_to }));
        }
        let mut args = self.with_account(json!({ "create": { "s0": sub } }));
        if let (Some((from, to)), Some(m)) = (on_success_move, args.as_object_mut()) {
            m.insert(
                "onSuccessUpdateEmail".into(),
                json!({ "#s0": {
                    format!("mailboxIds/{}", pointer_escape(from)): null,
                    format!("mailboxIds/{}", pointer_escape(to)): true,
                    "keywords/$draft": null,
                } }),
            );
        }
        let responses = self.call(&[Invocation::new("EmailSubmission/set", args, "c0")])?;
        let mut id = None;
        for r in responses {
            let r = r.into_result()?;
            if r.name == "EmailSubmission/set" {
                let set = SetResult::from_json(&r.args).ok()?;
                id = set
                    .created
                    .get("s0")
                    .and_then(|c| c.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        id.ok_or_else(|| NetError::Protocol("EmailSubmission/set did not return an id".into()))
    }

    /// Sends a raw RFC 5322 message: upload, import into `drafts_id` as a draft, submit, and on success
    /// move it to `sent_id`. Returns (email id, submission id).
    ///
    /// Submission-only `X-MailCraft-Submit-*` header fields are removed first, as the SMTP client does
    /// ([`crate::smtp::take_submit_options`]), so neither the recipients nor the server's sent copy see
    /// them; a delivery receipt request among them becomes DSN envelope parameters when the server lists
    /// the DSN extension and an envelope is given.
    pub fn send_raw(
        &self,
        identity_id: &str,
        raw: &[u8],
        drafts_id: &str,
        sent_id: &str,
        envelope: Option<(&str, &[&str])>,
    ) -> Result<(String, String)> {
        let (dsn, clean) = take_submit_options(raw);
        let dsn = dsn.filter(|_| self.submission_extension("DSN"));
        let blob = self.upload(&clean, "message/rfc822")?;
        let email = self.import_email(&blob, &[drafts_id], &["$draft", "$seen"])?;
        let sub =
            self.submit_with(identity_id, &email, envelope, dsn.as_ref(), Some((drafts_id, sent_id)))?;
        Ok((email, sub))
    }
}

/// A push `StateChange` (RFC 8620 §7.1): per account id, the new state of each changed type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateChange {
    pub changed: BTreeMap<String, BTreeMap<String, String>>,
}

impl StateChange {
    /// Parses the `data` of a `state` event. `None` for anything else.
    pub fn parse(data: &str) -> Option<StateChange> {
        let v: Value = serde_json::from_str(data).ok()?;
        if v.get("@type").and_then(Value::as_str).is_some_and(|t| t != "StateChange") {
            return None;
        }
        let mut changed = BTreeMap::new();
        for (acct, types) in v.get("changed")?.as_object()? {
            let Some(types) = types.as_object() else { continue };
            let m: BTreeMap<String, String> =
                types.iter().filter_map(|(k, st)| Some((k.clone(), st.as_str()?.to_owned()))).collect();
            changed.insert(acct.clone(), m);
        }
        Some(StateChange { changed })
    }

    /// The new state of `kind` ("Email", "Mailbox", …) in `account`, if it changed.
    pub fn state_of(&self, account: &str, kind: &str) -> Option<&str> {
        self.changed.get(account)?.get(kind).map(String::as_str)
    }
}

fn check_auth(resp: HttpResponse) -> Result<HttpResponse> {
    if resp.status == 401 || resp.status == 403 {
        return Err(NetError::Auth(format!("HTTP {}: {}", resp.status, resp.text_snippet())));
    }
    Ok(resp)
}

fn parse_query(v: &Value) -> EmailQuery {
    EmailQuery {
        ids: v
            .get("ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
        query_state: v.get("queryState").and_then(Value::as_str).unwrap_or_default().to_owned(),
        position: v.get("position").and_then(Value::as_u64).unwrap_or(0),
        total: v.get("total").and_then(Value::as_u64),
    }
}

/// Escapes a JSON Pointer reference token (RFC 6901) for patch paths.
fn pointer_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testserver::{Seen, json as json_resp, serve};

    fn plaintext_ok() -> HttpClient {
        HttpClient::new(crate::ConnectOptions {
            allow_plaintext_auth: true,
            ..crate::ConnectOptions::default()
        })
    }

    fn session_json(base: &str) -> String {
        json!({
            "capabilities": { CORE: {}, MAIL: {}, SUBMISSION: {} },
            "accounts": { "A1": { "name": "user@example.com" } },
            "primaryAccounts": { MAIL: "A1", SUBMISSION: "A1" },
            "username": "user@example.com",
            "apiUrl": "/jmap/api",
            "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
            "uploadUrl": "/jmap/upload/{accountId}",
            "eventSourceUrl": "/jmap/events",
            "state": "s1"
        })
        .to_string()
    }

    #[test]
    fn session_and_methods() {
        let (base, t, rx) = serve(10, |s| {
            let body: Value = serde_json::from_slice(&s.body).unwrap_or(Value::Null);
            let calls = body.get("methodCalls").and_then(Value::as_array).cloned().unwrap_or_default();
            let first =
                calls.first().and_then(|c| c.get(0)).and_then(Value::as_str).unwrap_or_default().to_owned();
            match (s.method.as_str(), s.path.as_str()) {
                ("GET", "/.well-known/jmap") => {
                    b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /jmap/session\r\nContent-Length: 0\r\n\r\n"
                        .to_vec()
                }
                ("GET", "/jmap/session") => {
                    let port = s.header("host").unwrap().to_owned();
                    json_resp(200, &session_json(&format!("http://{port}")))
                }
                ("POST", "/jmap/upload/A1") => {
                    json_resp(201, r#"{"accountId":"A1","blobId":"B9","type":"message/rfc822","size":5}"#)
                }
                ("GET", p) if p.starts_with("/jmap/download/A1/B9/") => {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec()
                }
                ("POST", "/jmap/api") => {
                    let resp = match first.as_str() {
                        "Mailbox/get" => {
                            json!({"methodResponses": [["Mailbox/get", {"accountId": "A1", "state": "m1", "list": [
                            {"id": "M1", "name": "Inbox", "role": "inbox", "totalEmails": 3, "unreadEmails": 1, "sortOrder": 1},
                            {"id": "M2", "name": "Sent", "role": "sent", "parentId": null},
                            {"id": "M3", "name": "Projects", "parentId": "M1"},
                        ], "notFound": []}, "c0"]]})
                        }
                        "Email/query" => {
                            let get = &calls[1][1];
                            assert_eq!(get["#ids"]["resultOf"], "q");
                            json!({"methodResponses": [
                                ["Email/query", {"ids": ["E1", "E2"], "queryState": "q1", "position": 0, "total": 2}, "q"],
                                ["Email/get", {"list": [{"id": "E1", "subject": "Hi"}, {"id": "E2", "subject": "Yo"}]}, "g"],
                            ]})
                        }
                        "Email/set" => {
                            let upd = &calls[0][1]["update"];
                            assert_eq!(upd["E1"]["keywords/$seen"], true);
                            json!({"methodResponses": [["Email/set", {"newState": "e2", "updated": {"E1": null}, "notUpdated": {"E2": {"type": "notFound"}}}, "c0"]]})
                        }
                        "Email/import" => {
                            json!({"methodResponses": [["Email/import", {"created": {"i0": {"id": "E7", "blobId": "B9"}}}, "c0"]]})
                        }
                        "EmailSubmission/set" => {
                            let a = &calls[0][1];
                            assert_eq!(a["onSuccessUpdateEmail"]["#s0"]["mailboxIds/M2"], true);
                            assert!(body["using"].as_array().unwrap().iter().any(|u| u == SUBMISSION));
                            json!({"methodResponses": [
                                ["EmailSubmission/set", {"created": {"s0": {"id": "S1"}}}, "c0"],
                                ["Email/set", {"updated": {"E7": null}}, "c0"],
                            ]})
                        }
                        _ => json!({"methodResponses": [["error", {"type": "unknownMethod"}, "c0"]]}),
                    };
                    json_resp(200, &resp.to_string())
                }
                _ => json_resp(404, "{}"),
            }
        });
        let e =
            JmapClient::connect(&base, JmapAuth::Bearer("tok".into()), HttpClient::default()).unwrap_err();
        assert!(matches!(e, NetError::Insecure(_)), "{e:?}");
        let c = JmapClient::connect(&base, JmapAuth::Bearer("tok".into()), plaintext_ok()).unwrap();
        assert_eq!(c.account_id(), "A1");
        assert_eq!(c.session().api_url, format!("{base}/jmap/api"));
        assert!(c.session().has_capability(SUBMISSION));
        let seen: Vec<_> = (0..2).map(|_| rx.recv().unwrap()).collect();
        assert_eq!(seen[1].header("authorization"), Some("Bearer tok"));

        let mbs = c.mailboxes().unwrap();
        assert_eq!(mbs.len(), 3);
        assert_eq!(mbs[0].folder_role(), Some(FolderRole::Inbox));
        assert_eq!(mbs[1].folder_role(), Some(FolderRole::SentItems));
        assert_eq!(mbs[2].parent_id.as_deref(), Some("M1"));
        assert_eq!(mbs[0].unread_emails, 1);

        let (q, list) = c
            .query_and_get(
                json!({"inMailbox": "M1"}),
                json!([{"property": "receivedAt", "isAscending": false}]),
                50,
                &["id", "subject"],
            )
            .unwrap();
        assert_eq!(q.ids, vec!["E1", "E2"]);
        assert_eq!(q.total, Some(2));
        assert_eq!(list[1]["subject"], "Yo");

        let err = c.set_keyword(&["E1".into(), "E2".into()], "$seen", true).unwrap_err();
        assert!(matches!(err, NetError::Jmap { ref kind, .. } if kind == "notFound"), "{err:?}");

        let (email, sub) = c.send_raw("I1", b"hello", "M9", "M2", None).unwrap();
        assert_eq!((email.as_str(), sub.as_str()), ("E7", "S1"));
        assert_eq!(c.download("B9", "msg.eml", "message/rfc822").unwrap(), b"hello");
        let err = c.call_one("Foo/get", json!({})).unwrap_err();
        assert!(matches!(err, NetError::Jmap { ref kind, .. } if kind == "unknownMethod"));
        t.join().unwrap();
    }

    /// Sends one message with a DSN request through a server whose submission capability lists
    /// `extensions`; returns (uploaded message, EmailSubmission/set create arguments).
    fn send_with_dsn(extensions: Value) -> (String, Value) {
        let (base, t, rx) = serve(4, move |s| {
            let body: Value = serde_json::from_slice(&s.body).unwrap_or(Value::Null);
            let first = body["methodCalls"][0][0].as_str().unwrap_or_default().to_owned();
            match (s.method.as_str(), s.path.as_str()) {
                ("GET", "/jmap/session") => {
                    let mut sess: Value = serde_json::from_str(&session_json("")).unwrap();
                    sess["accounts"]["A1"]["accountCapabilities"] =
                        json!({ SUBMISSION: { "submissionExtensions": extensions.clone() } });
                    json_resp(200, &sess.to_string())
                }
                ("POST", "/jmap/upload/A1") => json_resp(201, r#"{"blobId":"B9","type":"message/rfc822","size":5}"#),
                ("POST", "/jmap/api") if first == "Email/import" => json_resp(
                    200,
                    &json!({"methodResponses": [["Email/import", {"created": {"i0": {"id": "E7"}}}, "c0"]]})
                        .to_string(),
                ),
                ("POST", "/jmap/api") => json_resp(
                    200,
                    &json!({"methodResponses": [["EmailSubmission/set", {"created": {"s0": {"id": "S1"}}}, "c0"]]})
                        .to_string(),
                ),
                _ => json_resp(404, "{}"),
            }
        });
        let c = JmapClient::connect(
            &format!("{base}/jmap/session"),
            JmapAuth::Bearer("t".into()),
            plaintext_ok(),
        )
        .unwrap();
        let raw = b"From: a@example.com\r\nTo: b@example.org\r\nMessage-ID: <m1@example.com>\r\n\
X-MailCraft-Submit-DSN: NOTIFY=SUCCESS,FAILURE; RET=HDRS\r\nX-MailCraft-Submit-Save-Sent: none\r\nSubject: s\r\n\r\nbody\r\n";
        c.send_raw("I1", raw, "M9", "M2", Some(("a@example.com", &["b@example.org"]))).unwrap();
        t.join().unwrap();
        let seen: Vec<Seen> = rx.try_iter().collect();
        let upload = seen.iter().find(|s| s.path.starts_with("/jmap/upload")).unwrap();
        let submit = seen
            .iter()
            .filter_map(|s| serde_json::from_slice::<Value>(&s.body).ok())
            .find(|b| b["methodCalls"][0][0] == "EmailSubmission/set")
            .unwrap();
        (
            String::from_utf8_lossy(&upload.body).into_owned(),
            submit["methodCalls"][0][1]["create"]["s0"].clone(),
        )
    }

    #[test]
    fn send_raw_strips_submission_fields_and_asks_for_dsn() {
        let (msg, sub) = send_with_dsn(json!({ "DSN": [], "SIZE": ["0"] }));
        assert!(!msg.contains("X-MailCraft-Submit"), "{msg}");
        assert!(msg.contains("Subject: s\r\n") && msg.contains("Message-ID: <m1@example.com>"));
        let env = &sub["envelope"];
        assert_eq!(
            env["mailFrom"],
            json!({"email": "a@example.com", "parameters": {"RET": "HDRS", "ENVID": "m1@example.com"}})
        );
        assert_eq!(
            env["rcptTo"][0],
            json!({"email": "b@example.org", "parameters": {"NOTIFY": "SUCCESS,FAILURE", "ORCPT": "rfc822;b@example.org"}})
        );
        // Without the server's DSN extension: no parameters, still no submission-only fields.
        let (msg, sub) = send_with_dsn(json!({}));
        assert!(!msg.contains("X-MailCraft-Submit"), "{msg}");
        assert_eq!(sub["envelope"]["mailFrom"], json!({"email": "a@example.com"}));
        assert_eq!(sub["envelope"]["rcptTo"][0], json!({"email": "b@example.org"}));
    }

    #[test]
    fn auth_failure_and_bad_session() {
        let (base, t, _rx) = serve(2, |s| {
            if s.header("authorization") == Some("Bearer bad") {
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                json_resp(200, r#"{"apiUrl": "/x"}"#)
            }
        });
        let e = JmapClient::connect(&base, JmapAuth::Bearer("bad".into()), plaintext_ok()).unwrap_err();
        assert!(matches!(e, NetError::Auth(_)));
        let e = JmapClient::connect(
            &format!("{base}/s"),
            JmapAuth::Basic { user: "u".into(), password: "p".into() },
            plaintext_ok(),
        )
        .unwrap_err();
        assert!(matches!(e, NetError::Protocol(_)));
        assert_eq!(format!("{:?}", JmapAuth::Bearer("secret".into())), "Bearer(..)");
        t.join().unwrap();
    }

    #[test]
    fn roles_and_pointers() {
        assert_eq!(role_to_folder("Trash"), Some(FolderRole::DeletedItems));
        assert_eq!(role_to_folder("junk"), Some(FolderRole::JunkEmail));
        assert_eq!(role_to_folder("all"), None);
        assert_eq!(pointer_escape("a/b~c"), "a~1b~0c");
    }

    #[test]
    fn push_url_and_state_changes() {
        let (base, t, _rx) = serve(1, |s| {
            let port = s.header("host").unwrap().to_owned();
            let mut doc: Value = serde_json::from_str(&session_json(&format!("http://{port}"))).unwrap();
            doc["eventSourceUrl"] = json!("/jmap/events?types={types}&closeafter={closeafter}&ping={ping}");
            json_resp(200, &doc.to_string())
        });
        let c = JmapClient::connect(&base, JmapAuth::Bearer("t".into()), plaintext_ok()).unwrap();
        t.join().unwrap();
        let url = c.event_source_url(&["Email", "Mailbox"], 60).unwrap();
        assert!(url.ends_with("/jmap/events?types=Email%2CMailbox&closeafter=no&ping=60"), "{url}");
        let sc = StateChange::parse(
            r#"{"@type":"StateChange","changed":{"A1":{"Email":"e9","Mailbox":"m3"},"A2":{"Email":"x"}}}"#,
        )
        .unwrap();
        assert_eq!(sc.state_of("A1", "Email"), Some("e9"));
        assert_eq!(sc.state_of("A1", "Thread"), None);
        assert!(StateChange::parse(r#"{"@type":"Other","changed":{}}"#).is_none());
        assert!(StateChange::parse("not json").is_none());
        assert!(StateChange::parse(r#"{"changed":{"A":5}}"#).unwrap().changed.is_empty());
    }
}
