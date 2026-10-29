//! Search › Search Tools › Include Older Results: search the mail that is still only on the server.
//!
//! An IMAP or JMAP account with a download window ("download mail from the past N months", 12 by default)
//! keeps older mail on the server. With Include Older Results on, a search typed in the search box is also
//! sent to the servers of the accounts in the search scope: IMAP `UID SEARCH` (RFC 3501/9051 section
//! 6.4.4) in each folder searched, JMAP `Email/query` (RFC 8621 section 4.4) over the account. Matches that
//! are not on this computer yet come back and join the results:
//!
//! - IMAP matches arrive as headers only (like Download Headers) in their folder; opening one downloads the
//!   whole message (`sync::cmds::download_now`).
//! - JMAP matches arrive whole (JMAP has no header-only download here).
//!
//! At most [`MAX_PER_FOLDER`] matches per folder and [`MAX_PER_ACCOUNT`] per account come in per search (the
//! newest first); [`OlderResults::found`] says how many the servers had. The query is translated into server
//! criteria as closely as the protocols allow (words, people, subject, body, read, flagged, dates, size);
//! conditions a server cannot test (categories, attachments, importance…) are left out, so a server may
//! return a few more messages than the local search would show, never fewer. A query with nothing the
//! server can test is not sent.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::accounts::account_options;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::{AccountId, AccountKind, ItemId};

/// Most matches one search brings in per server folder, and per account.
pub const MAX_PER_FOLDER: usize = 100;
pub const MAX_PER_ACCOUNT: usize = 300;

/// The Include Older Results toggle and the server matches of the current search.
#[derive(Clone, Debug, Default)]
pub struct OlderResults {
    /// Include Older Results is on.
    pub on: bool,
    /// The search the server matches belong to.
    pub query: String,
    /// Bumped per server search; results of an older one only add messages, not results.
    pub generation: u64,
    /// Accounts whose server is still searching.
    pub pending: BTreeSet<AccountId>,
    /// Items the servers found for [`OlderResults::query`].
    pub hits: BTreeSet<ItemId>,
    /// Matches the servers reported that were not on this computer (more than [`OlderResults::hits`] when a
    /// cap was reached).
    pub found: usize,
    /// Why the last server search did not run or failed.
    pub error: Option<String>,
    /// Matches already asked to download in full when opened (each is asked once).
    pub fetched: BTreeSet<ItemId>,
}

/// A server search handed to an account's next run.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) generation: u64,
    pub(crate) criterion: mailcraft_net::search::Criterion,
    /// The folders searched (the search scope when the search started).
    pub(crate) folders: BTreeSet<mailcraft_model::FolderId>,
}

/// Server accounts that keep older mail on the server: IMAP (not POP3) or JMAP with an incoming server and
/// a download window.
pub fn accounts_with_window(s: &Session) -> Vec<AccountId> {
    s.mailbox
        .accounts
        .iter()
        .filter(|a| matches!(a.kind, AccountKind::Imap | AccountKind::Jmap) && a.incoming.is_some())
        .filter(|a| !crate::accounts::is_pop3(s, a.id))
        .filter(|a| account_options(s, a.id).download_months.is_some())
        .map(|a| a.id)
        .collect()
}

/// Why Include Older Results is unavailable (`None` = available).
pub fn unavailable(s: &Session) -> Option<&'static str> {
    if cfg!(target_arch = "wasm32") {
        return Some("server search is not available in the browser");
    }
    if accounts_with_window(s).is_empty() {
        return Some("all mail is already on this computer (no account keeps older mail on its server)");
    }
    if s.view.work_offline {
        return Some("working offline");
    }
    None
}

/// The accounts the current search scope covers that keep older mail on the server.
fn accounts_in_scope(s: &Session) -> Vec<AccountId> {
    let scoped: BTreeSet<AccountId> = crate::search_index::scope_folders(s)
        .into_iter()
        .filter_map(|f| s.mailbox.folder(f).map(|f| f.account))
        .collect();
    accounts_with_window(s).into_iter().filter(|a| scoped.contains(a)).collect()
}

/// Server matches to add to the results of the active search (empty unless Include Older Results is on and
/// they belong to the query in the search box).
pub fn extra_results(s: &Session) -> Vec<ItemId> {
    let o = &s.sync.older;
    if !o.on || o.query.trim() != s.view.search.trim() || s.view.search.trim().is_empty() {
        return Vec::new();
    }
    o.hits.iter().copied().filter(|i| s.mailbox.mail.contains(*i)).collect()
}

/// State for the command result and the UI.
pub fn state_json(s: &Session) -> Value {
    let o = &s.sync.older;
    json!({
        "on": o.on,
        "query": o.query,
        "searching": !o.pending.is_empty(),
        "hits": extra_results(s).len(),
        "found": o.found,
        "error": o.error,
    })
}

/// `search.includeOlderResults`: `on` sets the toggle (default: flip it). Turning it on with a search in the
/// box searches the servers at once; `refresh` searches again.
pub fn toggle(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(why) = unavailable(s) {
        return Err(CmdError::Disabled(why.into()));
    }
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(!s.sync.older.on);
    let refresh = p.get("refresh").and_then(Value::as_bool).unwrap_or(false);
    let was = s.sync.older.on;
    s.sync.older.on = on;
    let mut started = 0usize;
    if on && (!was || refresh) {
        started = start(s, refresh)?;
    }
    if !on {
        stop(s);
    }
    let mut v = state_json(s);
    if let Some(o) = v.as_object_mut() {
        o.insert("started".into(), json!(started));
    }
    Ok(v)
}

/// Include Older Results was turned off: searches still waiting for an account's next run are dropped, and
/// one already running is disowned (its results no longer join the search; turning the option on again
/// searches anew).
fn stop(s: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    let queued = !s.sync.older_requests.is_empty();
    #[cfg(target_arch = "wasm32")]
    let queued = false;
    #[cfg(not(target_arch = "wasm32"))]
    s.sync.older_requests.clear();
    if queued || !s.sync.older.pending.is_empty() {
        s.sync.older.generation = s.sync.older.generation.wrapping_add(1);
        s.sync.older.query.clear();
    }
    s.sync.older.pending.clear();
}

/// Sends the search in the box to the servers (when Include Older Results is on). Returns how many
/// accounts started searching. `again`: search even when the same query already ran.
pub fn start(s: &mut Session, again: bool) -> Result<usize, CmdError> {
    let query = s.view.search.trim().to_owned();
    if !s.sync.older.on || query.is_empty() {
        return Ok(0);
    }
    if !again && s.sync.older.query == query && s.sync.older.generation > 0 {
        return Ok(0);
    }
    if s.sync.older.query != query {
        s.sync.older.hits.clear();
    }
    s.sync.older.query = query.clone();
    s.sync.older.error = None;
    s.sync.older.found = 0;
    s.sync.older.generation = s.sync.older.generation.wrapping_add(1);
    start_native(s, &query)
}

#[cfg(target_arch = "wasm32")]
fn start_native(s: &mut Session, _query: &str) -> Result<usize, CmdError> {
    s.sync.older.error = Some("Server search is not available in the browser.".into());
    Ok(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn start_native(s: &mut Session, query: &str) -> Result<usize, CmdError> {
    let Some(criterion) = criterion(s, query) else {
        s.sync.older.error = Some("Nothing in this search can be looked up on the server.".into());
        return Ok(0);
    };
    let accounts = accounts_in_scope(s);
    if accounts.is_empty() {
        s.sync.older.error = Some("No account in the search scope keeps older mail on its server.".into());
        return Ok(0);
    }
    let generation = s.sync.older.generation;
    let folders: BTreeSet<mailcraft_model::FolderId> =
        crate::search_index::scope_folders(s).into_iter().collect();
    for a in &accounts {
        let req = Request { generation, criterion: criterion.clone(), folders: folders.clone() };
        s.sync.older_requests.insert(*a, req);
    }
    s.sync.older.pending = accounts.iter().copied().collect();
    let plan = super::Plan { send: false, receive: false, folders: Some(Vec::new()), user: true };
    let started = match s.send_receive(&accounts, plan) {
        Ok(n) => n,
        Err(e) => {
            for a in &accounts {
                s.sync.older_requests.remove(a);
            }
            s.sync.older.error = Some(e.to_string());
            return Err(e);
        }
    };
    // An account busy with another run searches when that run ends (its request waits); one that could not
    // start is not searching.
    let waiting: Vec<AccountId> = accounts
        .iter()
        .copied()
        .filter(|a| !s.sync.older_requests.contains_key(a) || s.sync.busy(*a))
        .collect();
    s.sync.older.pending.retain(|a| waiting.contains(a));
    for a in &accounts {
        if !waiting.contains(a) {
            s.sync.older_requests.remove(a);
        }
    }
    Ok(started)
}

/// A message was opened (read window or reading pane): a server match that came as headers only downloads
/// in full, once. Returns true when a download started.
pub fn opened(s: &mut Session, id: ItemId) -> bool {
    if !s.sync.older.hits.contains(&id)
        || s.sync.older.fetched.contains(&id)
        || !super::cmds::is_header_only(s, id)
        || s.view.work_offline
    {
        return false;
    }
    s.sync.older.fetched.insert(id);
    super::cmds::download_now(s, &[id]).is_ok_and(|n| n > 0)
}

/// Matches of server search `generation` arrived for `account` (already added to the mailbox as `ids`);
/// `total`: matches the server had that were not on this computer.
pub(crate) fn found(s: &mut Session, generation: u64, ids: &[ItemId], total: usize) {
    let o = &mut s.sync.older;
    if generation != o.generation {
        return;
    }
    o.hits.extend(ids.iter().copied());
    o.found = o.found.saturating_add(total);
}

/// An account's run ended: its server search (if any) is over.
pub(crate) fn run_ended(s: &mut Session, account: AccountId, error: Option<&str>) {
    // A search still waiting runs with the account's next run (it was busy when the search started).
    #[cfg(not(target_arch = "wasm32"))]
    if s.sync.older_requests.contains_key(&account) {
        return;
    }
    if s.sync.older.pending.remove(&account)
        && let Some(e) = error
    {
        s.sync.older.error = Some(e.to_owned());
    }
}

// ---------------------------------------------------------------------------------------------------------
// The query as server criteria

/// The server criteria for `query` (`None` when nothing in it can be tested on a server).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn criterion(s: &Session, query: &str) -> Option<mailcraft_net::search::Criterion> {
    use mailcraft_net::search::Criterion;
    let q = mailcraft_search::parse(query);
    let ctx = crate::search_index::context(s);
    match conv(&q, &ctx, false, 0)? {
        Criterion::All => None,
        c => c.check().is_ok().then_some(c),
    }
}

/// Deepest query converted.
#[cfg(not(target_arch = "wasm32"))]
const MAX_DEPTH: usize = 12;

/// `q` as server criteria. `None`: it cannot be tested on a server. In a loose conversion (`exact` false)
/// such parts of an AND are left out (more matches, never fewer); under a NOT the conversion must be exact.
#[cfg(not(target_arch = "wasm32"))]
fn conv(
    q: &mailcraft_search::Query,
    ctx: &mailcraft_search::Context,
    exact: bool,
    depth: usize,
) -> Option<mailcraft_net::search::Criterion> {
    use mailcraft_net::search::Criterion;
    use mailcraft_search::Query;
    if depth > MAX_DEPTH {
        return None;
    }
    let d = depth.saturating_add(1);
    match q {
        Query::All => Some(Criterion::All),
        Query::Nothing => Some(Criterion::Or(Vec::new())),
        Query::And(v) => {
            let mut kids = Vec::new();
            for x in v {
                match conv(x, ctx, exact, d) {
                    Some(Criterion::All) => {}
                    Some(c) => kids.push(c),
                    None if exact => return None,
                    None => {}
                }
            }
            Some(match kids.len() {
                0 => Criterion::All,
                1 => kids.pop().unwrap_or(Criterion::All),
                _ => Criterion::And(kids),
            })
        }
        Query::Or(v) => {
            let kids: Option<Vec<Criterion>> = v.iter().map(|x| conv(x, ctx, exact, d)).collect();
            let kids = kids?;
            if kids.contains(&Criterion::All) {
                return Some(Criterion::All);
            }
            Some(Criterion::Or(kids))
        }
        Query::Not(x) => conv(x, ctx, true, d).map(|c| Criterion::Not(Box::new(c))),
        Query::Term(t) => term(t, ctx),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn term(
    t: &mailcraft_search::Term,
    ctx: &mailcraft_search::Context,
) -> Option<mailcraft_net::search::Criterion> {
    use mailcraft_net::search::{Criterion, TextIn};
    use mailcraft_search::doc::DocKind;
    use mailcraft_search::query::{DateField, SizeSpec};
    use mailcraft_search::{Term, TextField};
    let text = |fields: &[TextIn], value: &str| {
        let v = value.trim();
        if v.is_empty() {
            return None;
        }
        let mut list: Vec<Criterion> = fields.iter().map(|f| Criterion::Text(*f, v.to_owned())).collect();
        Some(if list.len() == 1 { list.pop().unwrap_or(Criterion::All) } else { Criterion::Or(list) })
    };
    let fields = |f: TextField| -> Option<&'static [TextIn]> {
        Some(match f {
            TextField::Any => &[TextIn::Anywhere],
            TextField::From => &[TextIn::From],
            TextField::To => &[TextIn::To],
            TextField::Cc => &[TextIn::Cc],
            TextField::ToCc => &[TextIn::To, TextIn::Cc],
            TextField::Participants => &[TextIn::From, TextIn::To, TextIn::Cc],
            TextField::Subject => &[TextIn::Subject],
            TextField::Body => &[TextIn::Body],
            TextField::Attachments => return None,
        })
    };
    let day = |n: i64| mailcraft_model::dates::civil_from_days(n);
    match t {
        Term::Text(f, words) => text(fields(*f)?, &words.raw),
        Term::Me(f) => {
            let fs = fields(*f)?;
            let list: Vec<Criterion> = ctx
                .me
                .iter()
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .take(8)
                .filter_map(|a| text(fs, a))
                .collect();
            (!list.is_empty()).then_some(Criterion::Or(list))
        }
        Term::Read(b) => Some(Criterion::Seen(*b)),
        Term::Flagged(b) => Some(Criterion::Flagged(*b)),
        Term::Date(field, range) => {
            let (lo, hi) = ctx.day_bounds(range);
            let sent = *field == DateField::Sent;
            let mut list = Vec::new();
            if lo != i64::MIN {
                let d = day(lo);
                list.push(if sent { Criterion::SentSince(d) } else { Criterion::ReceivedSince(d) });
            }
            if hi != i64::MAX {
                let d = day(hi.saturating_add(1));
                list.push(if sent { Criterion::SentBefore(d) } else { Criterion::ReceivedBefore(d) });
            }
            Some(match list.len() {
                0 => Criterion::All,
                1 => list.pop().unwrap_or(Criterion::All),
                _ => Criterion::And(list),
            })
        }
        Term::Size(spec) => {
            let (lo, hi) = match *spec {
                SizeSpec::Bucket(b) => b.range(),
                SizeSpec::Range { lo, hi } => (lo.unwrap_or(0), hi.unwrap_or(u64::MAX)),
            };
            let mut list = Vec::new();
            if lo > 0 {
                list.push(Criterion::Larger(lo.saturating_sub(1)));
            }
            if hi < u64::MAX {
                list.push(Criterion::Smaller(hi.saturating_add(1)));
            }
            Some(match list.len() {
                0 => Criterion::All,
                1 => list.pop().unwrap_or(Criterion::All),
                _ => Criterion::And(list),
            })
        }
        Term::Kind(DocKind::Mail) => Some(Criterion::All),
        Term::Kind(_) | Term::Invalid(..) => Some(Criterion::Or(Vec::new())),
        Term::Category(_)
        | Term::Folder(_)
        | Term::HasAttachments(_)
        | Term::Importance(_)
        | Term::Expires(_)
        | Term::Prop(_) => None,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use mailcraft_net::search::{Criterion, TextIn};

    fn crit(q: &str) -> Option<Criterion> {
        let s = Session::new(mailcraft_model::Mailbox::new_local("Local Folders"));
        criterion(&s, q)
    }

    #[test]
    fn queries_become_server_criteria() {
        assert_eq!(crit("budget"), Some(Criterion::Text(TextIn::Anywhere, "budget".into())));
        assert_eq!(
            crit("from:ann subject:\"q3 plan\""),
            Some(Criterion::And(vec![
                Criterion::Text(TextIn::From, "ann".into()),
                Criterion::Text(TextIn::Subject, "q3 plan".into()),
            ]))
        );
        assert_eq!(
            crit("tocc:bo"),
            Some(Criterion::Or(vec![
                Criterion::Text(TextIn::To, "bo".into()),
                Criterion::Text(TextIn::Cc, "bo".into()),
            ]))
        );
        assert_eq!(crit("isread:no"), Some(Criterion::Seen(false)));
        // What a server cannot test is left out of an AND…
        assert_eq!(
            crit("budget hasattachments:yes"),
            Some(Criterion::Text(TextIn::Anywhere, "budget".into()))
        );
        // …a query with nothing left is not sent, and a NOT over it is not either.
        assert_eq!(crit("hasattachments:yes"), None);
        assert_eq!(crit(""), None);
        assert_eq!(crit("budget -category:red"), Some(Criterion::Text(TextIn::Anywhere, "budget".into())));
        assert_eq!(
            crit("budget -lunch"),
            Some(Criterion::And(vec![
                Criterion::Text(TextIn::Anywhere, "budget".into()),
                Criterion::Not(Box::new(Criterion::Text(TextIn::Anywhere, "lunch".into()))),
            ]))
        );
        // Dates become SINCE / BEFORE days; sizes LARGER / SMALLER.
        match crit("received:2025-01-05..2025-01-31").unwrap() {
            Criterion::And(v) => {
                assert_eq!(v[0], Criterion::ReceivedSince((2025, 1, 5)));
                assert_eq!(v[1], Criterion::ReceivedBefore((2025, 2, 1)));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(crit("size:>1mb"), Some(Criterion::Larger(_))));
    }

    /// Regression: turning the option off left searches queued for busy accounts; they still ran (and filed
    /// server matches) on that account's next Send/Receive.
    #[test]
    fn turning_off_drops_queued_searches() {
        let mut s = Session::new(mailcraft_model::Mailbox::new_local("Local Folders"));
        let a = s.mailbox.add_account(mailcraft_model::Account {
            kind: AccountKind::Imap,
            address: "me@example.net".into(),
            display_name: "me@example.net".into(),
            incoming: Some(mailcraft_model::ServerSettings::default()),
            ..Default::default()
        });
        s.mailbox.create_default_folders(a);
        assert!(unavailable(&s).is_none());
        s.view.search = "budget".into();
        s.sync.older.on = true;
        s.sync.older.query = "budget".into();
        s.sync.older.generation = 4;
        s.sync.older_requests.insert(
            a,
            Request { generation: 4, criterion: crit("budget").unwrap(), folders: BTreeSet::new() },
        );
        s.sync.older.pending.insert(a);

        let r = toggle(&mut s, &json!({"on": false})).unwrap();
        assert_eq!(r["on"], false);
        assert!(s.sync.older_requests.is_empty(), "the queued search is dropped");
        assert!(s.sync.older.pending.is_empty());
        assert_ne!(s.sync.older.generation, 4);
        // Late results of the disowned search no longer join the results.
        found(&mut s, 4, &[ItemId(1)], 1);
        assert!(s.sync.older.hits.is_empty() && s.sync.older.found == 0);
    }
}
