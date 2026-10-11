//! JMAP accounts (RFC 8620 core, RFC 8621 mail): folders from Mailbox/get and Mailbox/changes, mail from
//! Email/query + Email/get on the first run and Email/changes afterwards (whole messages downloaded as blobs),
//! local changes written back with Email/set (keywords, mailbox, destroy) and Email/import, and the Outbox
//! sent with EmailSubmission/set (filed in Sent by the server).
//!
//! Folder `remote_name` = mailbox id; item `remote_id` = email id. JMAP has no change queue on the mailbox
//! (its ids are strings): the account options keep a *shadow* of what the server last had for each email
//! (mailbox and keywords), and each run sends what differs from it. An email in several mailboxes is shown in
//! one folder (the first of its mailboxes in folder order).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use mailcraft_net::jmap::{JmapAuth, JmapClient, JmapMailbox};
use mailcraft_net::{ConnectOptions, Credentials, NetError};
use serde_json::{Map, Value, json};

use super::native::{Event, Events, Outgoing};
use super::*;
use crate::accounts::{JmapShadow, prefs};
use mailcraft_model::{Account, FolderRole};

/// Most emails downloaded by the first run (newest first); older mail stays on the server.
const MAX_INITIAL: u64 = 2_000;
/// Most changes asked for per Email/changes call.
const MAX_CHANGES: u64 = 500;
/// Emails per Email/get.
const GET_BATCH: usize = 100;
/// Properties read for every email.
const PROPS: [&str; 6] = ["id", "blobId", "mailboxIds", "keywords", "size", "receivedAt"];

/// How to reach a JMAP account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JmapConn {
    url: String,
    auth: JmapAuth,
    options: ConnectOptions,
}

impl JmapConn {
    pub(crate) fn new(url: &str, creds: &Credentials, options: ConnectOptions) -> JmapConn {
        let auth = match creds {
            Credentials::Password { user, password } => {
                JmapAuth::Basic { user: user.clone(), password: password.clone() }
            }
            Credentials::OAuth2 { access_token, .. } => JmapAuth::Bearer(access_token.clone()),
        };
        JmapConn { url: url.to_owned(), auth, options }
    }
    pub(crate) fn set_bearer(&mut self, token: &str) {
        if let JmapAuth::Bearer(t) = &mut self.auth {
            *t = token.to_owned();
        }
    }
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
    pub(crate) fn connect(&self) -> Result<JmapClient, NetError> {
        let http = mailcraft_net::http::HttpClient::new(self.options.clone());
        JmapClient::connect(&self.url, self.auth.clone(), http)
    }
}

/// Local changes for one run.
#[derive(Clone, Debug, Default)]
pub(crate) struct JmapJob {
    receive: bool,
    email_state: String,
    mailbox_state: String,
    /// (email id, patch, shadow after it).
    updates: Vec<(String, Value, JmapShadow)>,
    destroy: Vec<String>,
    /// (item, mailbox id, keywords, source).
    imports: Vec<(ItemId, String, Vec<String>, Vec<u8>)>,
}

/// One email as downloaded.
#[derive(Clone, Debug, Default)]
pub(crate) struct JEmail {
    id: String,
    mailboxes: Vec<String>,
    keywords: Vec<String>,
    size: Option<u64>,
    raw: Option<Vec<u8>>,
}

pub(crate) enum JmapEvent {
    Mailboxes {
        list: Vec<JmapMailbox>,
        state: String,
    },
    Emails {
        list: Vec<JEmail>,
    },
    Gone {
        ids: Vec<String>,
    },
    EmailState {
        state: String,
    },
    Updated {
        id: String,
        shadow: JmapShadow,
    },
    Destroyed {
        ids: Vec<String>,
    },
    Imported {
        item: ItemId,
        id: String,
        shadow: JmapShadow,
    },
    /// The server cannot report changes since our state: start over.
    Reset,
}

// ---------------------------------------------------------------------------------------------------
// Session side: building a job
// ---------------------------------------------------------------------------------------------------

/// The JMAP keywords mirroring an item's state.
fn keywords_of(m: &MailItem) -> Vec<String> {
    let mut k = Vec::new();
    if m.read {
        k.push("$seen".to_owned());
    }
    if m.is_flagged() {
        k.push("$flagged".to_owned());
    }
    match m.response {
        mailcraft_model::ResponseState::Replied(_) | mailcraft_model::ResponseState::RepliedAll(_) => {
            k.push("$answered".to_owned());
        }
        mailcraft_model::ResponseState::Forwarded(_) => k.push("$forwarded".to_owned()),
        mailcraft_model::ResponseState::None => {}
    }
    if m.is_draft {
        k.push("$draft".to_owned());
    }
    k.sort();
    k
}

fn kw_object(k: &[String]) -> Value {
    Value::Object(k.iter().map(|w| (w.clone(), json!(true))).collect())
}

pub(crate) fn build_job(s: &Session, a: &Account, plan: &Plan) -> Option<JmapJob> {
    let st = s.sync.prefs.data.jmap.get(&a.id.0).cloned().unwrap_or_default();
    let outbox = s.mailbox.role_folder(a.id, FolderRole::Outbox);
    let mut job = JmapJob {
        receive: plan.receive,
        email_state: st.email_state.clone(),
        mailbox_state: st.mailbox_state.clone(),
        ..Default::default()
    };
    let mut here: BTreeSet<&str> = BTreeSet::new();
    for m in s.mailbox.mail.values() {
        let Some(f) = s.mailbox.folder(m.folder).filter(|f| f.account == a.id && f.kind == FolderKind::Mail)
        else {
            continue;
        };
        if Some(f.id) == outbox {
            continue;
        }
        let Some(mailbox) = f.remote_name.as_deref() else { continue };
        let keywords = keywords_of(m);
        match m.remote_id.as_deref() {
            Some(eid) => {
                here.insert(eid);
                let Some(sh) = st.shadow.get(eid) else { continue };
                if sh.mailbox != mailbox || sh.keywords != keywords {
                    let mut patch = Map::new();
                    if sh.keywords != keywords {
                        patch.insert("keywords".into(), kw_object(&keywords));
                    }
                    if sh.mailbox != mailbox {
                        patch.insert("mailboxIds".into(), json!({ mailbox: true }));
                    }
                    let shadow = JmapShadow { mailbox: mailbox.to_owned(), keywords };
                    job.updates.push((eid.to_owned(), Value::Object(patch), shadow));
                }
            }
            None => job.imports.push((m.id, mailbox.to_owned(), keywords, item_source(s, m))),
        }
    }
    job.destroy = st.shadow.keys().filter(|id| !here.contains(id.as_str())).cloned().collect();
    if !job.receive && job.updates.is_empty() && job.destroy.is_empty() && job.imports.is_empty() {
        return None;
    }
    Some(job)
}

// ---------------------------------------------------------------------------------------------------
// Worker side
// ---------------------------------------------------------------------------------------------------

fn send(events: &Events, cancel: &AtomicBool, account: AccountId, ev: JmapEvent) {
    if !events.send(Event::Jmap { account, ev }) {
        cancel.store(true, Ordering::SeqCst);
    }
}

fn parse_email(v: &Value) -> Option<(JEmail, Option<String>)> {
    let id = v.get("id")?.as_str()?.to_owned();
    let mut mailboxes: Vec<String> = v
        .get("mailboxIds")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter(|(_, on)| on.as_bool() == Some(true)).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    mailboxes.sort();
    let mut keywords: Vec<String> = v
        .get("keywords")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(_, on)| on.as_bool() == Some(true))
                .map(|(k, _)| k.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    keywords.sort();
    let blob = v.get("blobId").and_then(Value::as_str).map(str::to_owned);
    let size = v.get("size").and_then(Value::as_u64);
    Some((JEmail { id, mailboxes, keywords, size, raw: None }, blob))
}

/// Email/get of `ids` in batches; downloads the source of those in `with_raw`.
fn get_emails(
    c: &JmapClient,
    ids: &[String],
    with_raw: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<(Vec<JEmail>, String), NetError> {
    let mut out = Vec::new();
    let mut state = String::new();
    for chunk in ids.chunks(GET_BATCH) {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let (list, st) = c.get_emails_with_state(chunk, &PROPS)?;
        state = st;
        for v in &list {
            let Some((mut e, blob)) = parse_email(v) else { continue };
            if with_raw.contains(&e.id)
                && let Some(b) = blob
            {
                e.raw = Some(c.download(&b, "message.eml", "message/rfc822")?);
            }
            out.push(e);
        }
    }
    Ok((out, state))
}

pub(crate) fn run(
    account: AccountId,
    conn: &JmapConn,
    job: Option<&JmapJob>,
    outgoing: &[Outgoing],
    events: &Events,
    cancel: &AtomicBool,
) -> Option<String> {
    let progress = |t: &str| {
        events.send(Event::Progress { account, text: t.to_owned() });
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    progress("Connecting…");
    let c = match conn.connect() {
        Ok(c) => c,
        Err(e) => return Some(e.to_string()),
    };
    let res: Result<(), NetError> = (|| {
        // 1. Local changes.
        if let Some(j) = job {
            if !j.updates.is_empty() {
                progress("Updating the server…");
                let update: Map<String, Value> =
                    j.updates.iter().map(|(id, p, _)| (id.clone(), p.clone())).collect();
                let r = c.set_emails(Map::new(), update, &[])?;
                for (id, _, shadow) in &j.updates {
                    if r.updated.contains(id) {
                        send(
                            events,
                            cancel,
                            account,
                            JmapEvent::Updated { id: id.clone(), shadow: shadow.clone() },
                        );
                    } else if let Some(e) = r.not_updated.get(id) {
                        log::warn!("jmap: update of {id} refused: {} {}", e.kind, e.description);
                        if e.kind == "notFound" {
                            send(events, cancel, account, JmapEvent::Destroyed { ids: vec![id.clone()] });
                        }
                    }
                }
            }
            if !j.destroy.is_empty() {
                let r = c.set_emails(Map::new(), Map::new(), &j.destroy)?;
                let mut gone: Vec<String> = r.destroyed.clone();
                gone.extend(
                    r.not_destroyed.iter().filter(|(_, e)| e.kind == "notFound").map(|(id, _)| id.clone()),
                );
                send(events, cancel, account, JmapEvent::Destroyed { ids: gone });
            }
            for (item, mailbox, keywords, raw) in &j.imports {
                if cancelled() {
                    break;
                }
                progress("Uploading…");
                let blob = c.upload(raw, "message/rfc822")?;
                let kws: Vec<&str> = keywords.iter().map(String::as_str).collect();
                let id = c.import_email(&blob, &[mailbox.as_str()], &kws)?;
                let shadow = JmapShadow { mailbox: mailbox.clone(), keywords: keywords.clone() };
                send(events, cancel, account, JmapEvent::Imported { item: *item, id, shadow });
            }
        }
        // 2. The Outbox.
        if !outgoing.is_empty() {
            progress("Sending…");
            let ids = c.identities()?;
            let (boxes, _) = c.mailboxes_with_state()?;
            let role = |r: &str| boxes.iter().find(|b| b.role.as_deref() == Some(r)).map(|b| b.id.clone());
            let drafts = role("drafts").or_else(|| boxes.first().map(|b| b.id.clone())).unwrap_or_default();
            let sent = role("sent").unwrap_or_else(|| drafts.clone());
            for m in outgoing {
                if cancelled() {
                    break;
                }
                let identity = ids
                    .iter()
                    .find(|i| i.email.eq_ignore_ascii_case(&m.from))
                    .or_else(|| ids.first())
                    .map(|i| i.id.clone());
                let result = match identity {
                    None => Err("the server lists no sending identity for this account".to_owned()),
                    Some(identity) => {
                        let rcpts: Vec<&str> = m.rcpts.iter().map(String::as_str).collect();
                        c.send_raw(&identity, &m.raw, &drafts, &sent, Some((&m.from, &rcpts)))
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    }
                };
                events.send(Event::Sent { account, item: m.item, result });
            }
        }
        // 3. Receive.
        let Some(j) = job.filter(|j| j.receive) else { return Ok(()) };
        progress("Updating the folder list…");
        let boxes_changed = j.mailbox_state.is_empty()
            || match c.changes("Mailbox", &j.mailbox_state, Some(MAX_CHANGES)) {
                Ok(ch) => {
                    ch.has_more_changes
                        || !ch.created.is_empty()
                        || !ch.updated.is_empty()
                        || !ch.destroyed.is_empty()
                }
                Err(_) => true,
            };
        if boxes_changed {
            let (list, state) = c.mailboxes_with_state()?;
            send(events, cancel, account, JmapEvent::Mailboxes { list, state });
        }
        progress("Updating messages…");
        let mut since = j.email_state.clone();
        if !since.is_empty() {
            for _ in 0..MAX_ROUNDS {
                if cancelled() {
                    return Ok(());
                }
                let ch = match c.changes("Email", &since, Some(MAX_CHANGES)) {
                    Ok(ch) => ch,
                    Err(NetError::Jmap { kind, .. }) if kind == "cannotCalculateChanges" => {
                        send(events, cancel, account, JmapEvent::Reset);
                        since.clear();
                        break;
                    }
                    Err(e) => return Err(e),
                };
                if !ch.destroyed.is_empty() {
                    send(events, cancel, account, JmapEvent::Gone { ids: ch.destroyed.clone() });
                }
                let mut ids = ch.created.clone();
                ids.extend(ch.updated.iter().cloned());
                let created: BTreeSet<String> = ch.created.iter().cloned().collect();
                let (list, _) = get_emails(&c, &ids, &created, cancel)?;
                if !list.is_empty() {
                    send(events, cancel, account, JmapEvent::Emails { list });
                }
                since = ch.new_state.clone();
                send(events, cancel, account, JmapEvent::EmailState { state: since.clone() });
                if !ch.has_more_changes {
                    break;
                }
            }
        }
        if since.is_empty() {
            // First run (or a reset): the newest mail, then the state to continue from.
            let (_, state) = c.get_emails_with_state(&[], &PROPS)?;
            let q = c.query_emails(
                json!({}),
                json!([{ "property": "receivedAt", "isAscending": false }]),
                0,
                MAX_INITIAL,
            )?;
            let all: BTreeSet<String> = q.ids.iter().cloned().collect();
            for chunk in q.ids.chunks(GET_BATCH) {
                if cancelled() {
                    return Ok(());
                }
                let (list, _) = get_emails(&c, chunk, &all, cancel)?;
                send(events, cancel, account, JmapEvent::Emails { list });
            }
            send(events, cancel, account, JmapEvent::EmailState { state });
        }
        Ok(())
    })();
    match res {
        Ok(()) if cancelled() => Some("Send/Receive was cancelled.".into()),
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Most Email/changes rounds in one run.
const MAX_ROUNDS: usize = 100;

// ---------------------------------------------------------------------------------------------------
// Session side: applying results
// ---------------------------------------------------------------------------------------------------

fn folder_of(s: &Session, account: AccountId, mailbox: &str) -> Option<FolderId> {
    s.mailbox
        .folders
        .values()
        .find(|f| {
            f.account == account && f.kind == FolderKind::Mail && f.remote_name.as_deref() == Some(mailbox)
        })
        .map(|f| f.id)
}

fn item_of(s: &Session, account: AccountId, id: &str) -> Option<ItemId> {
    s.mailbox
        .mail
        .values()
        .find(|m| {
            m.remote_id.as_deref() == Some(id)
                && s.mailbox.folder(m.folder).is_some_and(|f| f.account == account)
        })
        .map(|m| m.id)
}

fn apply_keywords(m: &mut MailItem, k: &[String], now: mailcraft_model::DateTime) {
    let has = |w: &str| k.iter().any(|x| x == w);
    let mut flags: Vec<String> = Vec::new();
    for (kw, flag) in [
        ("$seen", "\\Seen"),
        ("$flagged", "\\Flagged"),
        ("$answered", "\\Answered"),
        ("$forwarded", "$Forwarded"),
    ] {
        if has(kw) {
            flags.push(flag.to_owned());
        }
    }
    apply_server_flags(m, &flags, now);
    if !has("$answered") && !has("$forwarded") {
        m.response = mailcraft_model::ResponseState::None;
    }
    m.is_draft = has("$draft");
}

fn apply_mailboxes(s: &mut Session, account: AccountId, mut list: Vec<JmapMailbox>, state: String) {
    // Parents before children.
    let by_id: BTreeMap<String, Option<String>> =
        list.iter().map(|b| (b.id.clone(), b.parent_id.clone())).collect();
    let depth = |id: &str| {
        let mut n = 0usize;
        let mut cur = by_id.get(id).cloned().flatten();
        while let Some(p) = cur {
            n += 1;
            if n > by_id.len() {
                break;
            }
            cur = by_id.get(&p).cloned().flatten();
        }
        n
    };
    list.sort_by_cached_key(|b| (depth(&b.id), b.sort_order, b.name.clone()));
    let ids: BTreeSet<String> = list.iter().map(|b| b.id.clone()).collect();
    for b in &list {
        let display = local_folder_name(&b.name);
        let parent = b.parent_id.as_deref().and_then(|p| folder_of(s, account, p));
        if let Some(fid) = folder_of(s, account, &b.id) {
            if let Some(f) = s.mailbox.folders.get_mut(&fid)
                && f.role.is_none()
                && (f.name != display || f.parent != parent)
            {
                f.name = display;
                f.parent = parent;
            }
            continue;
        }
        if let Some(role) = b.folder_role()
            && let Some(fid) = s.mailbox.role_folder(account, role)
            && s.mailbox.folder(fid).is_some_and(|f| f.remote_name.is_none())
        {
            if let Some(f) = s.mailbox.folders.get_mut(&fid) {
                f.remote_name = Some(b.id.clone());
            }
            continue;
        }
        let id = FolderId(s.mailbox.alloc_id());
        s.mailbox.folders.insert(
            id,
            Folder {
                id,
                account,
                parent,
                name: display,
                kind: FolderKind::Mail,
                remote_name: Some(b.id.clone()),
                ..Default::default()
            },
        );
        s.sync.track.folders_created.insert(id);
    }
    let gone: Vec<(FolderId, bool)> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == account && f.kind == FolderKind::Mail)
        .filter(|f| f.remote_name.as_ref().is_some_and(|n| !ids.contains(n)))
        .map(|f| (f.id, f.role.is_some()))
        .collect();
    let mut remove = Vec::new();
    for (fid, is_role) in gone {
        if is_role {
            if let Some(f) = s.mailbox.folders.get_mut(&fid) {
                f.remote_name = None;
            }
        } else {
            remove.push(fid);
        }
    }
    if !remove.is_empty() {
        let items = remove_folders(&mut s.mailbox, &remove);
        s.sync.track.removed.extend(items);
    }
    prefs(s).jmap.entry(account.0).or_default().mailbox_state = state;
}

fn apply_emails(s: &mut Session, account: AccountId, list: Vec<JEmail>, new_inbox: &mut Vec<ItemId>) {
    let now = s.now();
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox);
    for e in list {
        // The first mailbox (in folder order) the email is in.
        let mut targets: Vec<FolderId> =
            e.mailboxes.iter().filter_map(|b| folder_of(s, account, b)).collect();
        targets.sort();
        let Some(folder) = targets.first().copied() else { continue };
        let mailbox = s.mailbox.folder(folder).and_then(|f| f.remote_name.clone()).unwrap_or_default();
        let shadow = JmapShadow { mailbox, keywords: e.keywords.clone() };
        match item_of(s, account, &e.id) {
            Some(id) => {
                if let Some(m) = s.mailbox.mail.get_mut(&id) {
                    if m.folder != folder {
                        m.folder = folder;
                    }
                    apply_keywords(m, &e.keywords, now);
                    s.sync.track.flags.insert(id);
                }
            }
            None => {
                let Some(raw) = &e.raw else { continue };
                let mut item = mailcraft_store::import::item_from_raw(raw, folder, now);
                // A local copy waiting for this message (sent here, filed by the server): adopt it.
                let adopt = item.message_id.as_ref().and_then(|mid| {
                    s.mailbox
                        .mail
                        .values()
                        .find(|m| {
                            m.folder == folder && m.remote_id.is_none() && m.message_id.as_ref() == Some(mid)
                        })
                        .map(|m| m.id)
                });
                if let Some(local) = adopt {
                    if let Some(m) = s.mailbox.mail.get_mut(&local) {
                        m.remote_id = Some(e.id.clone());
                    }
                } else {
                    item.remote_id = Some(e.id.clone());
                    apply_keywords(&mut item, &e.keywords, now);
                    if let Some(n) = e.size {
                        item.size = n;
                    }
                    let id = s.mailbox.add_mail(item);
                    s.sync.track.created.insert(id);
                    if Some(folder) == inbox {
                        new_inbox.push(id);
                    }
                }
            }
        }
        prefs(s).jmap.entry(account.0).or_default().shadow.insert(e.id, shadow);
    }
}

pub(crate) fn apply(s: &mut Session, account: AccountId, ev: JmapEvent, new_inbox: &mut Vec<ItemId>) {
    match ev {
        JmapEvent::Mailboxes { list, state } => apply_mailboxes(s, account, list, state),
        JmapEvent::Emails { list } => apply_emails(s, account, list, new_inbox),
        JmapEvent::Gone { ids } => {
            for id in &ids {
                if let Some(item) = item_of(s, account, id) {
                    s.mailbox.mail.remove(&item);
                    s.sync.track.removed.insert(item);
                }
            }
            let st = prefs(s).jmap.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        JmapEvent::EmailState { state } => prefs(s).jmap.entry(account.0).or_default().email_state = state,
        JmapEvent::Updated { id, shadow } => {
            prefs(s).jmap.entry(account.0).or_default().shadow.insert(id, shadow);
        }
        JmapEvent::Destroyed { ids } => {
            let st = prefs(s).jmap.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        JmapEvent::Imported { item, id, shadow } => {
            if let Some(m) = s.mailbox.mail.get_mut(&item) {
                m.remote_id = Some(id.clone());
                prefs(s).jmap.entry(account.0).or_default().shadow.insert(id, shadow);
            }
        }
        JmapEvent::Reset => {
            let st = prefs(s).jmap.entry(account.0).or_default();
            st.email_state.clear();
        }
    }
}

/// After a command: true when `before` → `after` changed mail of the JMAP account `account` in a way the
/// server should learn (folder, read, flag, response, new or removed items).
pub(crate) fn changed(before: &Mailbox, after: &Mailbox, account: AccountId) -> bool {
    let in_acct = |mb: &Mailbox, m: &MailItem| mb.folder(m.folder).is_some_and(|f| f.account == account);
    let same = |a: &MailItem, b: &MailItem| {
        a.folder == b.folder
            && a.read == b.read
            && a.flag == b.flag
            && a.response == b.response
            && a.is_draft == b.is_draft
    };
    before.mail.iter().any(|(id, b)| in_acct(before, b) && after.mail.get(id).is_none_or(|a| !same(a, b)))
        || after.mail.iter().any(|(id, a)| in_acct(after, a) && !before.mail.contains_key(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_email_parsing() {
        let mut m = MailItem { read: true, is_draft: true, ..Default::default() };
        assert_eq!(keywords_of(&m), vec!["$draft".to_owned(), "$seen".to_owned()]);
        apply_keywords(&mut m, &["$flagged".to_owned()], mailcraft_model::DateTime::default());
        assert!(!m.read && m.is_flagged() && !m.is_draft);
        let v = json!({"id": "e1", "blobId": "b1", "mailboxIds": {"m2": true, "m1": true, "m3": false},
            "keywords": {"$Seen": true}, "size": 42});
        let (e, blob) = parse_email(&v).unwrap();
        assert_eq!(e.mailboxes, vec!["m1".to_owned(), "m2".to_owned()]);
        assert_eq!(e.keywords, vec!["$seen".to_owned()]);
        assert_eq!((blob.as_deref(), e.size), (Some("b1"), Some(42)));
        assert!(parse_email(&json!({"blobId": "x"})).is_none());
    }
}
