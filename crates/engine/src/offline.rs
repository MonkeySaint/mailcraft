//! The offline window of server accounts (owner decision, `plan/storage-adr.md`): how far back message
//! bodies are kept on this computer.
//!
//! Each IMAP account keeps the bodies of its messages for a period chosen in its settings (the account's
//! settings page: a check box to keep mail on this computer and a slider with 11 stops, 3 days to All;
//! 1 year by default). Headers are always kept. A message received before the window loses its source and
//! body from the profile's body store (the item stays, with its headers, flags and preview line, marked
//! [`MailItem::body_evicted`]); it is downloaded again from the server when it is opened while the account
//! is online ([`opened`]), and the reading pane says so while it is not ([`placeholder`]). With the check
//! box off, no bodies are kept: each one is downloaded when it is opened, and taken off again by the next
//! pass.
//!
//! What is never taken off: drafts, mail in folders that are not on the server (Outbox, local folders),
//! mail without a server id, a body changed here (a body record), the messages selected or open, and
//! content the undo history still refers to (its files stay until the step goes). POP3 and JMAP accounts
//! keep everything: POP3 servers may delete what was downloaded, and the JMAP worker cannot fetch one
//! message again yet.
//!
//! A pass runs after each successful send/receive of the account ([`note_run_done`], at most once an hour
//! per account) and at once when the window changes (`accounts.options`, or `accounts.applyOfflineWindow`).
//! It is bounded ([`MAX_EVICT_PER_PASS`] messages); the next pass continues. Its changes are journaled
//! like a sync batch's, without an undo step and without server changes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CmdError, CmdResult, Session};
use mailcraft_model::{AccountId, AccountKind, ItemId, MailItem};

/// Most messages one pass takes off this computer.
pub const MAX_EVICT_PER_PASS: usize = 5_000;
/// Least time between two automatic passes of one account (seconds).
pub const PASS_INTERVAL_SECS: i64 = 3_600;

const DAY: i64 = 86_400;

/// How far back an account keeps message bodies on this computer: the 11 stops of the settings slider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfflineWindow {
    ThreeDays,
    OneWeek,
    TwoWeeks,
    OneMonth,
    ThreeMonths,
    SixMonths,
    #[default]
    OneYear,
    TwoYears,
    ThreeYears,
    FiveYears,
    All,
}

impl OfflineWindow {
    /// Every stop, shortest first (the slider's order).
    pub const ALL: [OfflineWindow; 11] = [
        OfflineWindow::ThreeDays,
        OfflineWindow::OneWeek,
        OfflineWindow::TwoWeeks,
        OfflineWindow::OneMonth,
        OfflineWindow::ThreeMonths,
        OfflineWindow::SixMonths,
        OfflineWindow::OneYear,
        OfflineWindow::TwoYears,
        OfflineWindow::ThreeYears,
        OfflineWindow::FiveYears,
        OfflineWindow::All,
    ];

    /// Position on the slider (0 = 3 days … 10 = All).
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(6)
    }

    /// The stop at slider position `i`.
    pub fn from_index(i: usize) -> Option<OfflineWindow> {
        Self::ALL.get(i).copied()
    }

    /// The period as shown next to the slider.
    pub fn label(self) -> &'static str {
        match self {
            OfflineWindow::ThreeDays => "3 days",
            OfflineWindow::OneWeek => "1 week",
            OfflineWindow::TwoWeeks => "2 weeks",
            OfflineWindow::OneMonth => "1 month",
            OfflineWindow::ThreeMonths => "3 months",
            OfflineWindow::SixMonths => "6 months",
            OfflineWindow::OneYear => "1 year",
            OfflineWindow::TwoYears => "2 years",
            OfflineWindow::ThreeYears => "3 years",
            OfflineWindow::FiveYears => "5 years",
            OfflineWindow::All => "All",
        }
    }

    /// The short name commands take ("3d", "1w", "2w", "1m", "3m", "6m", "1y", "2y", "3y", "5y", "all").
    pub fn key(self) -> &'static str {
        match self {
            OfflineWindow::ThreeDays => "3d",
            OfflineWindow::OneWeek => "1w",
            OfflineWindow::TwoWeeks => "2w",
            OfflineWindow::OneMonth => "1m",
            OfflineWindow::ThreeMonths => "3m",
            OfflineWindow::SixMonths => "6m",
            OfflineWindow::OneYear => "1y",
            OfflineWindow::TwoYears => "2y",
            OfflineWindow::ThreeYears => "3y",
            OfflineWindow::FiveYears => "5y",
            OfflineWindow::All => "all",
        }
    }

    /// A stop from its short name or its label (case and spaces ignored).
    pub fn parse(s: &str) -> Option<OfflineWindow> {
        let t: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        Self::ALL.into_iter().find(|w| {
            t == w.key()
                || t == w
                    .label()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    .to_ascii_lowercase()
        })
    }

    /// The length of the period in days (`None` = All). Months and years are counted in days (30, 91, 183,
    /// 365, 730, 1096, 1826).
    pub fn days(self) -> Option<i64> {
        Some(match self {
            OfflineWindow::ThreeDays => 3,
            OfflineWindow::OneWeek => 7,
            OfflineWindow::TwoWeeks => 14,
            OfflineWindow::OneMonth => 30,
            OfflineWindow::ThreeMonths => 91,
            OfflineWindow::SixMonths => 183,
            OfflineWindow::OneYear => 365,
            OfflineWindow::TwoYears => 730,
            OfflineWindow::ThreeYears => 1096,
            OfflineWindow::FiveYears => 1826,
            OfflineWindow::All => return None,
        })
    }
}

/// Per-session bookkeeping of the passes and of the downloads opening a message started.
#[derive(Debug, Default)]
pub struct OfflineState {
    /// Accounts whose last send/receive succeeded since their last pass.
    due: BTreeSet<AccountId>,
    /// When each account's last pass ran (Unix seconds).
    last: BTreeMap<AccountId, i64>,
    /// Messages opening downloaded again this session (once each until they arrive).
    fetching: BTreeSet<ItemId>,
    /// Content a pass took off whose change the store has not taken yet (a full disk): its files stay until
    /// the change is saved, so the store never names content that is gone.
    unsaved: BTreeSet<String>,
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassReport {
    /// Messages whose body was taken off this computer.
    pub evicted: usize,
    /// Content files (or in-memory entries) removed from the body store.
    pub removed: usize,
    /// More messages are past the window than one pass takes.
    pub more: bool,
}

/// Whether the offline window applies to account `id`: an IMAP account with an incoming server (not POP3).
pub fn applies(s: &Session, id: AccountId) -> bool {
    s.mailbox.account(id).is_some_and(|a| a.kind == AccountKind::Imap && a.incoming.is_some())
        && !crate::accounts::is_pop3(s, id)
}

/// The receive time before which account `id` keeps no bodies (`None`: it keeps all, or the window does not
/// apply to it).
pub fn cutoff(s: &Session, id: AccountId) -> Option<i64> {
    if !applies(s, id) {
        return None;
    }
    let o = crate::accounts::account_options(s, id);
    if !o.keep_offline {
        return Some(i64::MAX);
    }
    let days = o.offline_window.days()?;
    Some(s.now().unix.saturating_sub(days.saturating_mul(DAY)))
}

/// Messages that must keep their body whatever the window says: the selection, open messages and composes.
fn pinned(s: &Session) -> BTreeSet<ItemId> {
    let mut out: BTreeSet<ItemId> = s.selection.items.iter().copied().collect();
    for c in s.composes.values() {
        out.extend(c.source);
        out.extend(c.draft);
    }
    out.extend(s.read_zoom.keys().copied());
    out.extend(s.open_messages.iter().copied());
    out
}

/// Whether `m` may lose its body to the window: a server copy (in a folder on the server, with a server
/// id) whose content is exactly its source.
fn evictable(s: &Session, m: &MailItem) -> bool {
    m.blob.is_some()
        && m.body_key.is_none()
        && m.remote_id.is_some()
        && !m.is_draft
        && s.mailbox.folder(m.folder).is_some_and(|f| f.remote_name.is_some())
}

/// Takes the bodies of account `id`'s messages received before its window off this computer (see the
/// module docs). The changes are journaled without an undo step.
pub fn pass(s: &mut Session, id: AccountId) -> PassReport {
    let mut report = PassReport::default();
    let Some(cut) = cutoff(s, id) else { return report };
    report.removed = remove_saved(s, BTreeSet::new());
    s.sync.offline.last.insert(id, s.now().unix);
    s.sync.offline.due.remove(&id);
    let keep = pinned(s);
    let folders: Vec<mailcraft_model::FolderId> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == id && f.remote_name.is_some())
        .map(|f| f.id)
        .collect();
    // Oldest first over the account's server folders; messages already taken off (or never downloaded) are
    // not candidates, so a window that moves on finds the newly old ones however many went before.
    let mut found: Vec<(i64, ItemId)> = Vec::new();
    for f in folders {
        found.extend(
            s.mailbox
                .mail
                .in_folder(f)
                .filter(|m| m.received.unix < cut && !keep.contains(&m.id) && evictable(s, m))
                .map(|m| (m.received.unix, m.id)),
        );
    }
    found.sort_unstable();
    if found.len() > MAX_EVICT_PER_PASS {
        report.more = true;
        found.truncate(MAX_EVICT_PER_PASS);
    }
    let victims: Vec<ItemId> = found.into_iter().map(|(_, id)| id).collect();
    if victims.is_empty() {
        return report;
    }
    let recording = s.begin_changes();
    let mut freed: BTreeSet<String> = BTreeSet::new();
    for mid in &victims {
        let mut key = None;
        s.mailbox.mail.update(*mid, |m| {
            key = m.blob.take();
            m.body_evicted = true;
        });
        if let Some(k) = key {
            freed.insert(k);
            report.evicted = report.evicted.saturating_add(1);
        }
        s.sync.offline.fetching.remove(mid);
    }
    let ops = s.end_changes(recording).ops(&s.mailbox);
    s.journal(ops);
    report.removed = report.removed.saturating_add(remove_saved(s, freed));
    log::info!(
        "offline window of account {}: {} message bodies taken off this computer ({} files)",
        id.0,
        report.evicted,
        report.removed
    );
    report
}

/// Removes the content `freed` (and what earlier passes left, see [`OfflineState::unsaved`]) unless something
/// still refers to it (a copy, the undo history, the bin), once the store holds the change that took it off:
/// while the change is still queued (the journal refused it) the files stay, for a later pass. Returns how
/// many entries were removed.
fn remove_saved(s: &mut Session, freed: BTreeSet<String>) -> usize {
    s.sync.offline.unsaved.extend(freed);
    if s.sync.offline.unsaved.is_empty() || (s.store.is_some() && !s.pending_ops.is_empty()) {
        return 0;
    }
    let keys = std::mem::take(&mut s.sync.offline.unsaved);
    let live = s.content_keys_in_use();
    match s.bodies.remove_unreferenced(&keys, &live) {
        Ok(n) => n,
        Err(e) => {
            log::warn!("could not remove the content of messages past the offline window: {e}");
            0
        }
    }
}

/// A send/receive of `account` finished without an error: its next [`tick`] may run a pass.
#[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "send/receive runs on native targets only"))]
pub(crate) fn note_run_done(s: &mut Session, account: AccountId) {
    if applies(s, account) {
        s.sync.offline.due.insert(account);
    }
}

/// Runs the passes that are due (called by `Session::poll_sync`): accounts whose last send/receive
/// succeeded and whose last pass is at least [`PASS_INTERVAL_SECS`] old.
pub(crate) fn tick(s: &mut Session) {
    if s.sync.offline.due.is_empty() {
        return;
    }
    let now = s.now().unix;
    let due: Vec<AccountId> = s
        .sync
        .offline
        .due
        .iter()
        .copied()
        .filter(|a| s.sync.offline.last.get(a).is_none_or(|t| now.saturating_sub(*t) >= PASS_INTERVAL_SECS))
        .collect();
    for a in due {
        pass(s, a);
    }
}

/// Item `id` got its content again (downloaded, or stored here): a later eviction (a pass, or undo putting
/// back a state without its body) downloads it again when it is opened.
pub(crate) fn arrived(s: &mut Session, id: ItemId) {
    s.sync.offline.fetching.remove(&id);
}

/// Whether item `id` lost its body to the offline window and has not been downloaded again.
pub fn is_evicted(s: &Session, id: ItemId) -> bool {
    s.mailbox.mail.get(id).is_some_and(|m| m.body_evicted && m.blob.is_none() && m.body_key.is_none())
}

/// Message `id` was opened (reading pane or read window; called every frame it shows): a body the offline
/// window took off is downloaded again (asked once until it arrives) unless MailCraft works offline (a
/// download that failed is retried from the reading pane's bar). Returns true when a download started.
pub fn opened(s: &mut Session, id: ItemId) -> bool {
    if !is_evicted(s, id) || s.view.work_offline || s.sync.offline.fetching.contains(&id) {
        return false;
    }
    s.sync.offline.fetching.insert(id);
    crate::sync::cmds::download_now(s, &[id]).is_ok_and(|n| n > 0)
}

/// The reading pane's text for a message whose body the offline window took off: what happened and what
/// happens next (`None` for any other message).
pub fn placeholder(s: &Session, id: ItemId) -> Option<String> {
    if !is_evicted(s, id) {
        return None;
    }
    let m = s.mailbox.mail.get(id)?;
    let account = s.mailbox.folder(m.folder).map(|f| f.account)?;
    let o = crate::accounts::account_options(s, account);
    let why = if o.keep_offline {
        format!(
            "This message is older than the {} of mail this account keeps on this computer, so only its \
             header is here.",
            o.offline_window.label().to_lowercase()
        )
    } else {
        "This account keeps no mail on this computer, so only the header of this message is here.".to_owned()
    };
    let next = if s.view.work_offline {
        "MailCraft is working offline: the message is downloaded when you open it again online.".to_owned()
    } else if s.sync.offline.fetching.contains(&id) {
        match s.last_run_error(account) {
            Some(e) => format!("It could not be downloaded ({e}). Download it now to try again."),
            None => "Downloading it from the server…".to_owned(),
        }
    } else {
        "It is downloaded from the server when you open it while the account is connected.".to_owned()
    };
    Some(format!("{why} {next}"))
}

/// `accounts.applyOfflineWindow`: runs a pass now for one account (`account`) or every account the window
/// applies to.
pub fn apply_cmd(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let ids: Vec<AccountId> = match p.get("account") {
        None | Some(Value::Null) => {
            s.mailbox.accounts.iter().map(|a| a.id).filter(|a| applies(s, *a)).collect()
        }
        Some(_) => {
            let a = crate::accounts::find_account(s, p, "account")?;
            if !applies(s, a) {
                return Err(CmdError::Disabled(
                    "the offline window applies to IMAP accounts only; this account keeps all its mail"
                        .into(),
                ));
            }
            vec![a]
        }
    };
    let mut out = Vec::new();
    for a in ids {
        let r = pass(s, a);
        let o = crate::accounts::account_options(s, a);
        out.push(json!({
            "account": a.0,
            "keepOffline": o.keep_offline,
            "window": o.offline_window.key(),
            "evicted": r.evicted,
            "removed": r.removed,
            "more": r.more,
        }));
    }
    Ok(json!({"accounts": out}))
}

/// Enabled check of `accounts.applyOfflineWindow`: an account the window applies to exists.
pub fn has_window_account(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.iter().any(|a| applies(s, a.id)) {
        None
    } else {
        Some("No IMAP account: the offline window applies to IMAP accounts.")
    }
}

#[cfg(test)]
mod tests;
