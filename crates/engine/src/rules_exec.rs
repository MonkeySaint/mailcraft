//! The rule executor (`plan/reference/rules.md`): runs [`Rule`]s over mail items and carries out their actions.
//!
//! Entry points:
//! - [`on_new_mail`]: enabled receive rules over newly arrived mail (the send/receive hook in `sync.rs` and any
//!   local delivery call it).
//! - [`on_send`]: enabled send rules ("Apply rule on messages I send") over messages just filed in an Outbox.
//! - [`run_now`] / [`apply_rule_list`]: Run Rules Now over a folder, with explicitly chosen rules.
//!
//! Local actions (move, copy, categories, flags, importance, delete) change the mailbox directly; the session
//! queues their server side afterwards like any command's changes. Mail-sending actions (forward, redirect,
//! reply using a template, server reply) build a message, file it in the sending account's Outbox and hand the
//! Outbox to the account's worker — never more than [`MAX_SENDS_PER_RUN`] per run, never to the user's own
//! addresses' automatic replies or to mailing lists (RFC 3834 §2). Client actions (New Item Alert window,
//! sound, print, desktop alert) are collected over the run and reach the UI once at the end as dialogs or
//! notices.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::compose::ComposeAttachment;
use crate::{CmdError, Notice, Session};
use mailcraft_model::rule::{RuleContext, RuleEnv};
use mailcraft_model::{
    Account, AccountId, Address, BodyFormat, DateTime, Doc, FlagState, FolderId, FolderKind, FolderRole,
    ItemId, ListTime, MailItem, Rule, RuleAction, Sensitivity,
};

const DAY_SECS: i64 = 86_400;
/// Largest reply template (.eml) read.
pub const MAX_TEMPLATE_BYTES: u64 = 4 * 1024 * 1024;
/// Most messages one run sends (forwards, redirects, replies).
pub const MAX_SENDS_PER_RUN: usize = 100;
/// Most entries one run adds to the New Item Alert window.
const MAX_ALERTS: usize = 500;
/// Longest deferral accepted (four weeks).
pub const MAX_DEFER_MINUTES: u32 = 40_320;

/// Which messages Run Rules Now looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    All,
    Read,
    Unread,
}

impl Which {
    pub fn parse(s: &str) -> Option<Which> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "" => Some(Which::All),
            "read" => Some(Which::Read),
            "unread" => Some(Which::Unread),
            _ => None,
        }
    }
    fn wants(self, m: &MailItem) -> bool {
        match self {
            Which::All => true,
            Which::Read => m.read,
            Which::Unread => !m.read,
        }
    }
}

/// What one run produced besides mailbox changes.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    /// Items that matched at least one rule.
    pub matched: usize,
    /// Messages filed in an Outbox (forwards, redirects, replies).
    pub sent: Vec<ItemId>,
    /// Lines for the New Item Alert window.
    pub alerts: Vec<Value>,
    /// Items queued for printing.
    pub printed: Vec<ItemId>,
    /// Sound files played.
    pub sounds: Vec<String>,
    /// Actions that could not be carried out.
    pub errors: Vec<String>,
}

impl RunReport {
    pub fn to_json(&self) -> Value {
        json!({
            "matched": self.matched,
            "sent": self.sent.iter().map(|i| i.0).collect::<Vec<_>>(),
            "alerts": self.alerts.len(),
            "printed": self.printed.iter().map(|i| i.0).collect::<Vec<_>>(),
            "sounds": self.sounds,
            "errors": self.errors,
        })
    }
}

/// State of one run.
#[derive(Default)]
struct Run {
    report: RunReport,
    /// Senders already answered automatically in this run (one reply per sender).
    replied: BTreeSet<String>,
    /// Accounts whose Outbox got a message (sent at the end of the run).
    outboxes: BTreeSet<AccountId>,
    /// Contacts' addresses, for "sender is in my Contacts".
    contacts: Vec<String>,
}

// ---------------------------------------------------------------------------------------------------------
// Context

/// Rule context for an item: the user's addresses and the address of the account it lives in.
pub fn rule_context(s: &Session, item: &MailItem) -> RuleContext {
    let account_address = s
        .mailbox
        .folder_account(item.folder)
        .and_then(|a| s.mailbox.account(a))
        .map(|a| a.address.to_lowercase())
        .unwrap_or_default();
    RuleContext { my_addresses: s.me.clone(), account_address }
}

/// Where an item sits: its folder role, and the feed when it is under RSS Feeds.
pub fn rule_env(s: &Session, item: &MailItem, contacts: &[String]) -> RuleEnv {
    let role = s.mailbox.folder(item.folder).and_then(|f| f.role);
    // Walk up (bounded) looking for the RSS Feeds folder.
    let mut rss_feed = None;
    let mut cur = Some(item.folder);
    for _ in 0..64 {
        let Some(f) = cur.and_then(|id| s.mailbox.folder(id)) else { break };
        if f.role == Some(FolderRole::RssFeeds) {
            rss_feed = s.mailbox.folder(item.folder).map(|x| x.name.clone());
            break;
        }
        cur = f.parent;
    }
    RuleEnv { folder_role: role, rss_feed, contacts: contacts.to_vec(), source_head: None, body: None }
}

/// The start of `item`'s source read from the body store, for conditions that scan it. `None` when it has
/// none.
fn stored_source_head(s: &Session, item: &MailItem) -> Option<Vec<u8>> {
    s.source_head(item, mailcraft_model::rule::SOURCE_SCAN_LIMIT)
}

fn contact_addresses(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = s
        .mailbox
        .contacts
        .values()
        .flat_map(|c| c.emails.iter())
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn role_of(s: &Session, item: ItemId) -> Option<FolderRole> {
    s.mailbox.mail.get(item).and_then(|m| s.mailbox.folder(m.folder)).and_then(|f| f.role)
}

// ---------------------------------------------------------------------------------------------------------
// Entry points

/// Applies `rules` in order to the mail items `ids` (a rule's Stop Processing More Rules, or the item going
/// away, ends the list for that item); returns how many items matched at least one rule. Alerts, sounds and
/// print jobs reach the UI at the end.
pub fn apply_rule_list(s: &mut Session, ids: &[ItemId], rules: &[Rule]) -> usize {
    run_rules(s, ids, rules).matched
}

/// [`apply_rule_list`] returning the whole report.
pub fn run_rules(s: &mut Session, ids: &[ItemId], rules: &[Rule]) -> RunReport {
    let mut run = Run { contacts: contact_addresses(s), ..Default::default() };
    for &id in ids {
        let mut hit = false;
        // The stored source, read once per item and only for rules whose conditions scan it.
        let mut head: Option<Option<Vec<u8>>> = None;
        for rule in rules {
            let Some(item) = s.mailbox.mail.get(id) else { break };
            let ctx = rule_context(s, item);
            let mut env = rule_env(s, item, &run.contacts);
            if rule.needs_source() {
                env.source_head = head.take().unwrap_or_else(|| stored_source_head(s, item));
            }
            if rule.needs_body() {
                // Read on demand (through the body cache), only for rules whose conditions read it.
                env.body = s.body_of(item).ok().flatten();
            }
            let matched = rule.matches_in(item, &ctx, &env);
            if rule.needs_source() {
                head = Some(env.source_head.take());
            }
            if !matched {
                continue;
            }
            hit = true;
            if execute_actions(s, &mut run, rule, id) {
                break;
            }
        }
        if hit {
            run.report.matched = run.report.matched.saturating_add(1);
        }
    }
    finish(s, run)
}

/// Applies every enabled receive rule (in list order) to the mail items `ids`; returns how many items matched.
pub fn apply_rules(s: &mut Session, ids: &[ItemId]) -> usize {
    on_new_mail(s, ids).matched
}

/// Newly arrived mail (send/receive, local delivery): the enabled rules for received messages.
pub fn on_new_mail(s: &mut Session, ids: &[ItemId]) -> RunReport {
    let rules: Vec<Rule> =
        s.mailbox.rules.iter().filter(|r| r.enabled && !r.is_outgoing()).cloned().collect();
    if rules.is_empty() {
        return RunReport::default();
    }
    run_rules(s, ids, &rules)
}

/// Messages just filed in an Outbox: the enabled rules for sent messages ("Apply rule on messages I send").
/// Call it before the Outbox is handed to the account's worker so deferrals and added recipients count.
pub fn on_send(s: &mut Session, ids: &[ItemId]) -> RunReport {
    let rules: Vec<Rule> = s.mailbox.rules.iter().filter(|r| r.enabled && r.is_outgoing()).cloned().collect();
    let ids: Vec<ItemId> =
        ids.iter().copied().filter(|i| role_of(s, *i) == Some(FolderRole::Outbox)).collect();
    if rules.is_empty() || ids.is_empty() {
        return RunReport::default();
    }
    run_rules(s, &ids, &rules)
}

/// Run Rules Now: `rules` over the mail in `folder` (and its subfolders), limited to read/unread messages.
/// Returns (items scanned, report).
pub fn run_now(
    s: &mut Session,
    rules: &[Rule],
    folder: FolderId,
    subfolders: bool,
    which: Which,
) -> (usize, RunReport) {
    let folders: BTreeSet<FolderId> = if subfolders {
        s.mailbox.subtree(folder).into_iter().chain([folder]).collect()
    } else {
        [folder].into()
    };
    let ids: Vec<ItemId> = s
        .mailbox
        .mail
        .items()
        .filter(|m| folders.contains(&m.folder) && which.wants(m))
        .map(|m| m.id)
        .collect();
    let report = run_rules(s, &ids, rules);
    (ids.len(), report)
}

/// Ends a run: client actions go to the UI, Outboxes that got mail are sent.
fn finish(s: &mut Session, mut run: Run) -> RunReport {
    crate::cmd::folder::prune_selection(s);
    if !run.report.alerts.is_empty() {
        s.notify(Notice::OpenDialog("newItemAlerts".into(), json!({"alerts": run.report.alerts})));
    }
    if !run.report.sounds.is_empty() {
        s.notify(Notice::OpenDialog("ruleSound".into(), json!({"paths": run.report.sounds})));
    }
    // A later rule may have permanently deleted an item queued for printing.
    run.report.printed.retain(|id| s.mailbox.mail.contains(*id));
    if !run.report.printed.is_empty() {
        let ids: Vec<u64> = run.report.printed.iter().map(|i| i.0).collect();
        match invoke(s, "file.print", &json!({"ids": ids, "quick": true, "rule": true})) {
            Ok(_) => {}
            Err(CmdError::Unknown(_)) => s.notify(Notice::Info(format!(
                "A rule asked to print {} message(s); printing is not available.",
                ids.len()
            ))),
            Err(e) => run.report.errors.push(format!("print: {e}")),
        }
    }
    for account in std::mem::take(&mut run.outboxes) {
        crate::sync::send_now(s, account);
    }
    if !run.report.errors.is_empty() {
        let shown: Vec<&str> = run.report.errors.iter().take(3).map(String::as_str).collect();
        s.notify(Notice::Error(format!("Some rule actions could not be carried out: {}", shown.join("; "))));
    }
    run.report
}

// ---------------------------------------------------------------------------------------------------------
// Actions

/// Whether moving item `id` to `to` (or copying it: `None`) would leave a message without its body: only
/// its header is on this computer, and the move takes it out of its server account (its server copy would
/// be expunged) or the copy would have no body.
fn leaves_without_body(s: &Session, id: ItemId, to: Option<FolderId>) -> bool {
    if !crate::sync::cmds::rest_on_server(s, id) {
        return false;
    }
    let Some(to) = to else { return true };
    match s.mailbox.mail.get(id).and_then(|m| crate::sync::queue_account(&s.mailbox, m)) {
        Some(src) => crate::sync::synced_account(&s.mailbox, to) != Some(src),
        None => false,
    }
}

/// Runs the actions of `rule` on item `id`; returns true when no further rules should run for it (Stop
/// Processing More Rules, or the item was deleted).
fn execute_actions(s: &mut Session, run: &mut Run, rule: &Rule, id: ItemId) -> bool {
    let outgoing_rule = rule.is_outgoing();
    let mut stop = false;
    for action in &rule.actions {
        if !s.mailbox.mail.contains(id) {
            return true;
        }
        if !action.applies_to(outgoing_rule) {
            continue;
        }
        match action {
            RuleAction::MoveTo(f) => {
                if leaves_without_body(s, id, Some(*f)) {
                    run.report.errors.push(format!(
                        "{}: a message only whose header is on this computer is not moved off its server",
                        rule.name
                    ));
                } else if s.mailbox.folder(*f).is_some_and(|t| t.kind == FolderKind::Mail) {
                    let _ = s.mailbox.move_items(&[id], *f);
                } else {
                    run.report.errors.push(format!("{}: the folder to move to is gone", rule.name));
                }
            }
            RuleAction::CopyTo(f) => {
                if leaves_without_body(s, id, None) {
                    run.report.errors.push(format!(
                        "{}: a message only whose header is on this computer is not copied",
                        rule.name
                    ));
                } else if s.mailbox.folder(*f).is_some_and(|t| t.kind == FolderKind::Mail)
                    && let Some(mut copy) = s.mailbox.mail.get(id).cloned()
                {
                    copy.folder = *f;
                    copy.remote_id = None;
                    s.mailbox.add_mail(copy);
                }
            }
            RuleAction::AssignCategory(c) => {
                let c = c.trim();
                if let Some(m) = s.mailbox.mail.get_mut(id)
                    && !c.is_empty()
                    && !m.categories.iter().any(|x| x.eq_ignore_ascii_case(c))
                {
                    m.categories.push(c.to_owned());
                }
            }
            RuleAction::Delete => {
                let _ = s.mailbox.delete_items(&[id]);
                stop = true;
            }
            RuleAction::PermanentlyDelete => {
                s.mailbox.mail.remove(id);
                return true;
            }
            RuleAction::MarkAsRead => {
                s.mailbox.set_read(&[id], true);
            }
            RuleAction::FlagForFollowUp(label, days) => {
                let now = s.now();
                let due = i64::from(*days)
                    .checked_mul(DAY_SECS)
                    .and_then(|d| now.unix.checked_add(d))
                    .map(|unix| DateTime { unix, offset_min: now.offset_min });
                let label =
                    if label.trim().is_empty() { "Follow up".to_owned() } else { label.trim().to_owned() };
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    m.flag = FlagState::Flagged { label, start: Some(now), due, reminder: None };
                }
            }
            RuleAction::ClearFlag => {
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    m.flag = FlagState::None;
                }
            }
            RuleAction::ClearCategories => {
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    m.categories.clear();
                }
            }
            RuleAction::MarkImportance(i) => {
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    m.importance = *i;
                }
            }
            RuleAction::MarkSensitivity(x) => mark_sensitivity(s, id, *x),
            RuleAction::StopProcessingMoreRules => stop = true,
            RuleAction::NewItemAlert(msg) => {
                if run.report.alerts.len() < MAX_ALERTS {
                    let alert = alert_json(s, id, msg, &rule.name);
                    run.report.alerts.push(alert);
                }
            }
            RuleAction::DesktopAlert => {
                if let Some(m) = s.mailbox.mail.get(id) {
                    let from = m.from.as_ref().map(|f| f.display().to_owned()).unwrap_or_default();
                    let text =
                        if from.is_empty() { m.subject.clone() } else { format!("{from}: {}", m.subject) };
                    s.notify(Notice::NewItemAlert(text, id));
                }
            }
            RuleAction::PlaySound(path) => {
                let p = path.trim();
                if !p.is_empty() && !run.report.sounds.iter().any(|x| x == p) {
                    run.report.sounds.push(p.to_owned());
                }
            }
            RuleAction::Print => {
                if !run.report.printed.contains(&id) {
                    run.report.printed.push(id);
                }
            }
            RuleAction::ForwardTo(to) => forward(s, run, rule, id, to, false),
            RuleAction::ForwardAsAttachment(to) => forward(s, run, rule, id, to, true),
            RuleAction::RedirectTo(to) => redirect(s, run, rule, id, to),
            RuleAction::ReplyWithTemplate(path) => auto_reply(s, run, rule, id, Reply::Template(path)),
            RuleAction::ServerReply(text) => auto_reply(s, run, rule, id, Reply::Text(text)),
            RuleAction::DeferDelivery(minutes) => defer(s, id, *minutes),
            RuleAction::CcTo(list) => cc_to(s, run, rule, id, list),
            RuleAction::NotifyWhenRead => notify_header(s, id, "Disposition-Notification-To"),
            RuleAction::NotifyWhenDelivered => request_dsn(s, id),
        }
    }
    stop
}

/// One line of the New Item Alert window.
fn alert_json(s: &Session, id: ItemId, msg: &str, rule: &str) -> Value {
    let Some(m) = s.mailbox.mail.get(id) else { return Value::Null };
    let from = m.from.as_ref().or(m.sender.as_ref());
    json!({
        "item": id.0,
        "from": from.map(|f| f.display().to_owned()).unwrap_or_default(),
        "fromAddress": from.map(|f| f.addr.clone()).unwrap_or_default(),
        "subject": m.subject,
        "received": m.received.unix,
        "receivedText": ListTime::short_date_time(m.received, s.tz_offset_min),
        "text": msg.trim(),
        "rule": rule,
    })
}

/// Runs another command's implementation (skipping its enabled check, which looks at the selection) as part
/// of the enclosing command or sync step, which journals the whole effect.
pub(crate) fn invoke(s: &mut Session, id: &str, params: &Value) -> crate::CmdResult {
    let Some(run) = s.registry.get(id).map(|spec| spec.run) else {
        return Err(CmdError::Unknown(id.to_owned()));
    };
    run(s, params)
}

fn in_outbox(s: &Session, id: ItemId) -> bool {
    role_of(s, id) == Some(FolderRole::Outbox)
}

/// The item's source: its stored source, or one rebuilt from its fields and body.
fn source_of(s: &Session, m: &MailItem) -> Vec<u8> {
    crate::cmd::file::raw_source(s, m)
}

/// Rewrites the source of Outbox item `id` with `edit` (which gets and returns the raw bytes). The new
/// source goes to the body store; a failure there leaves the item as it was (logged).
fn edit_source(s: &mut Session, id: ItemId, edit: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let Some(m) = s.mailbox.mail.get(id) else { return };
    let raw = edit(source_of(s, m));
    let Some(mut item) = s.mailbox.mail.get(id).cloned() else { return };
    if let Err(e) = s.store_source(&mut item, &raw) {
        log::error!("a rule could not change message {}: {e}", id.0);
        return;
    }
    item.size = u64::try_from(raw.len()).unwrap_or(u64::MAX);
    s.mailbox.mail.insert(id, item);
}

fn mark_sensitivity(s: &mut Session, id: ItemId, x: Sensitivity) {
    if let Some(m) = s.mailbox.mail.get_mut(id) {
        m.sensitivity = x;
    }
    if in_outbox(s, id) {
        let value = match x {
            Sensitivity::Normal => None,
            Sensitivity::Personal => Some("Personal"),
            Sensitivity::Private => Some("Private"),
            Sensitivity::Confidential => Some("Company-Confidential"),
        };
        edit_source(s, id, |raw| set_header(&raw, "Sensitivity", value.map(str::to_owned)));
    }
}

/// "defer delivery by N minutes": the Outbox holds the message until then (`sent` is the hold time, as for
/// Delay Delivery).
fn defer(s: &mut Session, id: ItemId, minutes: u32) {
    if !in_outbox(s, id) {
        return;
    }
    let now = s.now();
    let secs = i64::from(minutes.min(MAX_DEFER_MINUTES)).saturating_mul(60);
    let until = now.unix.saturating_add(secs);
    if let Some(m) = s.mailbox.mail.get_mut(id) {
        let current = m.sent.map_or(now.unix, |t| t.unix);
        m.sent = Some(DateTime { unix: until.max(current), offset_min: now.offset_min });
    }
}

/// Parses rule recipients ("Name <a@b>", "a@b"; one entry may hold several separated by ; or ,).
fn recipients(list: &[String]) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    for entry in list {
        for m in mailcraft_mime::addr::parse_address_list(entry).iter().flat_map(|a| a.mailboxes()) {
            let addr = m.addr.trim();
            if addr.contains('@') && !out.iter().any(|o| o.addr.eq_ignore_ascii_case(addr)) {
                out.push(m);
            }
        }
    }
    out
}

/// Messages I send: "Cc the message to …" adds the people to the Cc header and to the recipients.
fn cc_to(s: &mut Session, run: &mut Run, rule: &Rule, id: ItemId, list: &[String]) {
    if !in_outbox(s, id) {
        return;
    }
    let extra = recipients(list);
    if extra.is_empty() {
        run.report.errors.push(format!("{}: no valid address to Cc", rule.name));
        return;
    }
    let mut added = Vec::new();
    if let Some(m) = s.mailbox.mail.get_mut(id) {
        for a in extra {
            let known = m.to.iter().chain(&m.cc).chain(&m.bcc).any(|x| x.addr.eq_ignore_ascii_case(&a.addr));
            if !known {
                m.cc.push(a.clone());
                added.push(a);
            }
        }
    }
    if added.is_empty() {
        return;
    }
    let text: Vec<String> = added.iter().map(|a| mailcraft_mime::addr::format_mailbox(a, true)).collect();
    edit_source(s, id, |raw| {
        let current = header_value(&raw, "Cc");
        let value = match current.as_deref().map(str::trim) {
            Some(c) if !c.is_empty() => format!("{c}, {}", text.join(", ")),
            _ => text.join(", "),
        };
        set_header(&raw, "Cc", Some(value))
    });
}

/// Messages I send: asks for a read / delivery notification to the sender.
fn notify_header(s: &mut Session, id: ItemId, header: &str) {
    if !in_outbox(s, id) {
        return;
    }
    let Some(from) = s.mailbox.mail.get(id).and_then(|m| m.from.clone()) else { return };
    let value = mailcraft_mime::addr::format_mailbox(&from, true);
    edit_source(s, id, |raw| set_header(&raw, header, Some(value)));
}

/// "Notify me when it is delivered": asks the SMTP client for an RFC 3461 delivery status notification
/// through the submission-only header (removed before the message is transmitted).
fn request_dsn(s: &mut Session, id: ItemId) {
    if !in_outbox(s, id) {
        return;
    }
    edit_source(s, id, |raw| {
        set_header(&raw, crate::cmd::compose::DSN_HEADER, Some(crate::cmd::compose::DSN_REQUEST.to_owned()))
    });
}

/// An account that can send for item `id`: the item's own account, else the default one, else any.
fn sending_account(s: &Session, id: ItemId) -> Option<Account> {
    let own = s
        .mailbox
        .mail
        .get(id)
        .and_then(|m| s.mailbox.folder_account(m.folder))
        .and_then(|a| s.mailbox.account(a))
        .filter(|a| a.can_send());
    own.or_else(|| s.mailbox.default_account().filter(|a| a.can_send()))
        .or_else(|| s.mailbox.accounts.iter().find(|a| a.can_send()))
        .cloned()
}

/// True when the sender of `m` is the user (never forward or answer one's own mail automatically: loops).
fn from_me(s: &Session, m: &MailItem) -> bool {
    m.from.iter().chain(m.sender.iter()).any(|f| {
        let a = f.addr.trim();
        s.me.iter().any(|x| x.trim().eq_ignore_ascii_case(a))
            || s.mailbox
                .accounts
                .iter()
                .any(|acc| !acc.address.is_empty() && acc.address.eq_ignore_ascii_case(a))
    })
}

fn budget(run: &mut Run, rule: &Rule) -> bool {
    if run.report.sent.len() >= MAX_SENDS_PER_RUN {
        run.report
            .errors
            .push(format!("{}: at most {MAX_SENDS_PER_RUN} messages are sent per run", rule.name));
        return false;
    }
    true
}

/// Sends compose `cid` without handing the Outbox to the worker yet (the run does that at the end, after any
/// header edits). Returns the Outbox item.
fn send_compose(s: &mut Session, cid: u64) -> Result<ItemId, String> {
    let immediate = std::mem::replace(&mut s.view.send_immediately, false);
    let r = invoke(s, "compose.send", &json!({"compose": cid}));
    s.view.send_immediately = immediate;
    s.notices.retain(|n| *n != Notice::Compose(cid));
    let sent = match r {
        Ok(v) if v.get("sent").and_then(Value::as_bool) == Some(true) => {
            v.get("item").and_then(Value::as_u64).map(ItemId).ok_or_else(|| "no message was filed".to_owned())
        }
        Ok(v) => Err(v.get("error").and_then(Value::as_str).unwrap_or("not sent").to_owned()),
        Err(e) => Err(e.to_string()),
    };
    if sent.is_err() {
        s.composes.remove(&cid);
    }
    sent
}

/// Opens a response compose for `id` through the mail commands; returns its id.
fn open_response(s: &mut Session, cmd: &str, id: ItemId) -> Result<u64, String> {
    let r = invoke(s, cmd, &json!({"ids": [id.0], "window": true})).map_err(|e| e.to_string())?;
    let cid = r.get("compose").and_then(Value::as_u64).ok_or_else(|| "no message was created".to_owned())?;
    s.notices.retain(|n| *n != Notice::Compose(cid));
    Ok(cid)
}

fn filed(s: &mut Session, run: &mut Run, item: ItemId) {
    run.report.sent.push(item);
    if let Some(a) = s.mailbox.mail.get(item).and_then(|m| s.mailbox.folder_account(m.folder)) {
        run.outboxes.insert(a);
    }
}

/// "forward it to …" (as an attachment when `attach`): a forward through the account's SMTP server.
fn forward(s: &mut Session, run: &mut Run, rule: &Rule, id: ItemId, to: &[String], attach: bool) {
    let rcpts = recipients(to);
    if rcpts.is_empty() {
        run.report.errors.push(format!("{}: no valid address to forward to", rule.name));
        return;
    }
    if s.mailbox.mail.get(id).is_none_or(|m| from_me(s, m)) || !budget(run, rule) {
        return;
    }
    if sending_account(s, id).is_none() {
        run.report.errors.push(format!("{}: no account can send mail", rule.name));
        return;
    }
    let cmd = if attach { "mail.forwardAsAttachment" } else { "mail.forward" };
    let result = open_response(s, cmd, id).and_then(|cid| {
        let line: Vec<String> =
            rcpts.iter().map(|a| mailcraft_mime::addr::format_mailbox(a, false)).collect();
        if let Some(c) = s.composes.get_mut(&cid) {
            c.to = line.join("; ");
        }
        send_compose(s, cid)
    });
    match result {
        Ok(item) => filed(s, run, item),
        Err(e) => run.report.errors.push(format!("{}: {e}", rule.name)),
    }
}

/// "redirect it to …": the original message, unchanged, to new recipients, with RFC 5322 §3.6.6 Resent-*
/// fields prepended.
fn redirect(s: &mut Session, run: &mut Run, rule: &Rule, id: ItemId, to: &[String]) {
    let rcpts = recipients(to);
    if rcpts.is_empty() {
        run.report.errors.push(format!("{}: no valid address to redirect to", rule.name));
        return;
    }
    let Some(m) = s.mailbox.mail.get(id).cloned() else { return };
    if from_me(s, &m) || !budget(run, rule) {
        return;
    }
    let Some(account) = sending_account(s, id) else {
        run.report.errors.push(format!("{}: no account can send mail", rule.name));
        return;
    };
    let Some(outbox) = s.mailbox.role_folder(account.id, FolderRole::Outbox) else {
        run.report.errors.push(format!("{}: the account {} has no Outbox", rule.name, account.address));
        return;
    };
    let now = s.now();
    let domain = account.address.rsplit_once('@').map_or("localhost", |(_, d)| d).trim().to_owned();
    let resent_id =
        format!("{}.{}.rule@{}", now.unix, id.0, if domain.is_empty() { "localhost" } else { &domain });
    let resent_to: Vec<String> =
        rcpts.iter().map(|a| mailcraft_mime::addr::format_mailbox(a, true)).collect();
    let from = mailcraft_mime::addr::format_mailbox(
        &Address::new(Some(account.sender_name.trim()).filter(|n| !n.is_empty()), account.address.trim()),
        true,
    );
    let mut raw = format!(
        "Resent-From: {from}\r\nResent-Date: {}\r\nResent-To: {}\r\nResent-Message-ID: <{resent_id}>\r\n",
        now.to_rfc5322(),
        resent_to.join(", ")
    )
    .into_bytes();
    raw.extend_from_slice(&source_of(s, &m));
    let mut item = MailItem {
        id: ItemId::default(),
        folder: outbox,
        to: rcpts,
        cc: Vec::new(),
        bcc: Vec::new(),
        sent: None,
        received: now,
        size: u64::try_from(raw.len()).unwrap_or(u64::MAX),
        read: true,
        is_draft: false,
        flag: FlagState::None,
        categories: Vec::new(),
        remote_id: None,
        blob: None,
        body_key: None,
        ..m
    };
    if let Err(e) = s.store_source(&mut item, &raw) {
        run.report.errors.push(format!("{}: the message could not be redirected: {e}", rule.name));
        return;
    }
    let new_id = s.mailbox.add_mail(item);
    filed(s, run, new_id);
}

enum Reply<'a> {
    /// Path of an .eml template.
    Template(&'a str),
    /// Text of the reply.
    Text(&'a str),
}

/// True for mail that must not be answered automatically (RFC 3834 §2): automatic replies, list and bulk
/// mail.
fn no_auto_reply(s: &Session, m: &MailItem) -> bool {
    let env = RuleEnv { source_head: stored_source_head(s, m), ..Default::default() };
    let source = env.source_head.as_deref();
    if mailcraft_model::rule::is_automatic_reply_in(m, source) {
        return true;
    }
    let head = mailcraft_model::rule::RuleCondition::HeaderContains(vec!["list-id:".into()]);
    let ctx = RuleContext::default();
    if head.matches_in(m, &ctx, &env) {
        return true;
    }
    ["precedence: bulk", "precedence: list", "precedence: junk"].iter().any(|p| {
        mailcraft_model::rule::RuleCondition::HeaderContains(vec![(*p).into()]).matches_in(m, &ctx, &env)
    })
}

/// A reply template read from an .eml file: subject, body and attachments.
struct Template {
    subject: String,
    body: Doc,
    format: BodyFormat,
    attachments: Vec<ComposeAttachment>,
}

/// Reads an .eml reply template (as saved by Save As › template).
pub fn read_template(path: &str, now: DateTime) -> Result<(String, Doc, BodyFormat, usize), String> {
    load_template(path, now).map(|t| (t.subject, t.body, t.format, t.attachments.len()))
}

fn load_template(path: &str, now: DateTime) -> Result<Template, String> {
    let p = std::path::Path::new(path.trim());
    if path.trim().is_empty() {
        return Err("no template chosen".into());
    }
    let bytes = match mailcraft_store::journal::read_capped(p, MAX_TEMPLATE_BYTES) {
        Ok(Some(b)) => b,
        Ok(None) => return Err(format!("the template {} was not found", p.display())),
        Err(e) => return Err(format!("the template {} could not be read: {e}", p.display())),
    };
    let msg = mailcraft_mime::parse(&bytes).map_err(|e| format!("the template is not a message: {e}"))?;
    let item = MailItem::from_message(&msg, &bytes, now);
    let content = mailcraft_model::Body::from_message(&msg);
    let (body, format) = match (&content.html, &content.text) {
        (Some(h), _) if !h.trim().is_empty() => (mailcraft_html::html_to_doc(h), BodyFormat::Html),
        (_, Some(t)) => (Doc::from_plain_text(t), BodyFormat::PlainText),
        _ => (Doc::from_plain_text(""), BodyFormat::PlainText),
    };
    let attachments = msg
        .attachments()
        .into_iter()
        .filter(|a| !a.inline)
        .map(|a| ComposeAttachment { filename: a.filename, mime_type: a.mime_type, data: a.data })
        .collect();
    Ok(Template { subject: item.subject.trim().to_owned(), body, format, attachments })
}

/// "reply using a specific template" / "have server reply using a specific message": a reply to the sender
/// with the template's (or the given) text instead of the quoted original; once per sender per run, never
/// to the user, to automatic replies or to lists.
fn auto_reply(s: &mut Session, run: &mut Run, rule: &Rule, id: ItemId, reply: Reply<'_>) {
    let Some(m) = s.mailbox.mail.get(id) else { return };
    let Some(sender) = m.reply_to.first().or(m.from.as_ref()).or(m.sender.as_ref()) else { return };
    let key = sender.addr.trim().to_lowercase();
    if key.is_empty() || from_me(s, m) || no_auto_reply(s, m) || run.replied.contains(&key) {
        return;
    }
    if !budget(run, rule) {
        return;
    }
    if sending_account(s, id).is_none() {
        run.report.errors.push(format!("{}: no account can send mail", rule.name));
        return;
    }
    let now = s.now();
    let tpl = match reply {
        Reply::Template(path) => match load_template(path, now) {
            Ok(t) => t,
            Err(e) => {
                run.report.errors.push(format!("{}: {e}", rule.name));
                return;
            }
        },
        Reply::Text(text) => Template {
            subject: String::new(),
            body: Doc::from_plain_text(text),
            format: BodyFormat::PlainText,
            attachments: Vec::new(),
        },
    };
    let result = open_response(s, "mail.reply", id).and_then(|cid| {
        if let Some(c) = s.composes.get_mut(&cid) {
            if !tpl.subject.is_empty() {
                c.subject = tpl.subject.clone();
            }
            c.body = tpl.body.clone();
            c.format = tpl.format;
            c.signature = None;
            c.attachments = tpl.attachments;
            c.selection = Default::default();
            c.extra_ranges.clear();
        }
        send_compose(s, cid)
    });
    match result {
        Ok(item) => {
            // RFC 3834 §5: mark the answer as automatic so other responders leave it alone.
            edit_source(s, item, |raw| set_header(&raw, "Auto-Submitted", Some("auto-replied".into())));
            run.replied.insert(key);
            filed(s, run, item);
        }
        Err(e) => run.report.errors.push(format!("{}: {e}", rule.name)),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Header editing (RFC 5322 §2.2: fields up to the first empty line; folded lines start with WSP)

/// Byte offset where the header section ends (the start of the empty line), and the line ending used.
fn header_end(raw: &[u8]) -> (usize, &'static str) {
    let crlf = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 2);
    let lf = raw.windows(2).position(|w| w == b"\n\n").map(|p| p + 1);
    match (crlf, lf) {
        (Some(a), Some(b)) if a <= b => (a, "\r\n"),
        (Some(a), None) => (a, "\r\n"),
        (_, Some(b)) => (b, "\n"),
        (None, None) => (raw.len(), "\r\n"),
    }
}

/// Spans (start, end incl. line ending) of the fields named `name`.
fn field_spans(raw: &[u8], end: usize, name: &str) -> Vec<(usize, usize)> {
    let head = raw.get(..end).unwrap_or(raw);
    let mut spans = Vec::new();
    let mut pos = 0usize;
    let mut current: Option<usize> = None;
    while pos < head.len() {
        let next = head
            .get(pos..)
            .and_then(|r| r.iter().position(|b| *b == b'\n'))
            .map_or(head.len(), |i| pos + i + 1);
        let line = head.get(pos..next).unwrap_or(&[]);
        let folded = matches!(line.first(), Some(b' ' | b'\t'));
        if !folded {
            if let Some(start) = current.take() {
                spans.push((start, pos));
            }
            let colon = line.iter().position(|b| *b == b':');
            let field = colon
                .and_then(|c| line.get(..c))
                .map(|n| String::from_utf8_lossy(n).trim().to_ascii_lowercase());
            if field.as_deref() == Some(&name.to_ascii_lowercase()) {
                current = Some(pos);
            }
        }
        pos = next;
    }
    if let Some(start) = current {
        spans.push((start, head.len()));
    }
    spans
}

/// The unfolded value of the first `name` field.
pub(crate) fn header_value(raw: &[u8], name: &str) -> Option<String> {
    let (end, _) = header_end(raw);
    let (a, b) = *field_spans(raw, end, name).first()?;
    let text = String::from_utf8_lossy(raw.get(a..b)?).into_owned();
    let (_, value) = text.split_once(':')?;
    Some(value.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "))
}

/// Sets (`Some`) or removes (`None`) field `name`: existing ones are removed and the new one goes at the end
/// of the header section. Values are written as given (callers pass encoded text); control characters are
/// dropped so a value cannot start a new field.
pub(crate) fn set_header(raw: &[u8], name: &str, value: Option<String>) -> Vec<u8> {
    let (end, nl) = header_end(raw);
    let spans = field_spans(raw, end, name);
    let mut out = Vec::with_capacity(raw.len().saturating_add(128));
    let mut pos = 0usize;
    for (a, b) in &spans {
        out.extend_from_slice(raw.get(pos..*a).unwrap_or(&[]));
        pos = *b;
    }
    out.extend_from_slice(raw.get(pos..end).unwrap_or(&[]));
    // A source without a header/body separator ends its last field without a line break.
    if end == raw.len() && !raw.is_empty() && !raw.ends_with(b"\n") {
        out.extend_from_slice(nl.as_bytes());
    }
    if let Some(v) = value {
        let v: String = v.chars().filter(|c| !c.is_control()).collect();
        out.extend_from_slice(format!("{name}: {v}{nl}").as_bytes());
    }
    if end == raw.len() {
        out.extend_from_slice(nl.as_bytes());
    }
    out.extend_from_slice(raw.get(end..).unwrap_or(&[]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Contact, Importance, Mailbox, ServerSettings};

    /// A local data file plus an IMAP account that can send (no password: nothing connects), fixed clock.
    fn session() -> (Session, AccountId) {
        let mut mb = Mailbox::new_local("Local");
        let acc = mb.add_account(Account {
            kind: mailcraft_model::AccountKind::Imap,
            display_name: "me@example.com".into(),
            address: "me@example.com".into(),
            sender_name: "Me Example".into(),
            outgoing: Some(ServerSettings {
                host: "smtp.example.com".into(),
                port: 465,
                ..Default::default()
            }),
            ..Default::default()
        });
        mb.create_default_folders(acc);
        for a in &mut mb.accounts {
            a.is_default = a.id == acc;
        }
        let mut s = Session::new(mb);
        s.fixed_now = DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0);
        s.refresh_me();
        (s, acc)
    }

    fn folder(s: &Session, acc: AccountId, role: FolderRole) -> FolderId {
        s.mailbox.role_folder(acc, role).unwrap()
    }

    fn raw_message(from: &str, subject: &str, extra: &str) -> Vec<u8> {
        format!(
            "From: {from}\r\nTo: me@example.com\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
             Message-ID: <{}@example.org>\r\n{extra}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             Hello there.\r\n",
            subject.len()
        )
        .into_bytes()
    }

    /// Delivers a parsed message into `folder`.
    fn deliver(s: &mut Session, folder: FolderId, raw: &[u8]) -> ItemId {
        let msg = mailcraft_mime::parse(raw).unwrap();
        let mut item = MailItem::from_message(&msg, raw, s.now());
        item.folder = folder;
        s.add_message(item, &mailcraft_store::Content::source(raw.to_vec())).unwrap()
    }

    fn rule(conditions: Vec<mailcraft_model::RuleCondition>, actions: Vec<RuleAction>) -> Rule {
        Rule { id: 1, name: "R".into(), enabled: true, conditions, actions, exceptions: Vec::new() }
    }

    fn outbox_items(s: &Session, acc: AccountId) -> Vec<&MailItem> {
        let outbox = folder(s, acc, FolderRole::Outbox);
        s.mailbox.items_in(outbox)
    }

    fn dialogs(s: &Session) -> Vec<(String, Value)> {
        s.notices
            .iter()
            .filter_map(|n| match n {
                Notice::OpenDialog(name, p) => Some((name.clone(), p.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn env_conditions_junk_rss_contacts() {
        use mailcraft_model::RuleCondition as C;
        let (mut s, acc) = session();
        let junk = folder(&s, acc, FolderRole::JunkEmail);
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let rss = folder(&s, acc, FolderRole::RssFeeds);
        let feed = s.mailbox.create_folder(rss, "Rust Blog").unwrap();
        let a = deliver(&mut s, junk, &raw_message("spam@example.net", "Win", ""));
        let b = deliver(&mut s, feed, &raw_message("feed@example.org", "Release notes", ""));
        let c = deliver(&mut s, inbox, &raw_message("Pat <pat@example.org>", "Hi", ""));
        let cid = ItemId(9_999_999);
        s.mailbox
            .contacts
            .insert(cid, Contact { id: cid, emails: vec!["PAT@example.org".into()], ..Default::default() });
        let contacts = contact_addresses(&s);
        let check = |s: &Session, id: ItemId, cond: C| {
            let item = s.mailbox.mail.get(id).unwrap();
            rule(vec![cond], vec![]).matches_in(item, &rule_context(s, item), &rule_env(s, item, &contacts))
        };
        assert!(check(&s, a, C::SuspectedJunk));
        assert!(!check(&s, c, C::SuspectedJunk));
        assert!(check(&s, b, C::FromAnyRssFeed));
        assert!(!check(&s, c, C::FromAnyRssFeed));
        assert!(check(&s, b, C::FromRssFeedWithText(vec!["rust".into()])));
        assert!(!check(&s, b, C::FromRssFeedWithText(vec!["python".into()])));
        assert!(check(&s, c, C::SenderInContacts));
        assert!(!check(&s, a, C::SenderInContacts));
        assert!(check(&s, c, C::ThroughAccount("ME@example.com".into())));
    }

    #[test]
    fn client_actions_reach_the_ui_once_per_run() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "One", ""));
        let b = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "Two", ""));
        let r = rule(
            vec![],
            vec![
                RuleAction::NewItemAlert("From Bob".into()),
                RuleAction::PlaySound("/sounds/ding.wav".into()),
                RuleAction::DesktopAlert,
                RuleAction::Print,
                RuleAction::MarkSensitivity(Sensitivity::Private),
            ],
        );
        s.notices.clear();
        let report = run_rules(&mut s, &[a, b], &[r]);
        assert_eq!(report.matched, 2);
        let d = dialogs(&s);
        let alerts: Vec<&Value> = d.iter().filter(|(n, _)| n == "newItemAlerts").map(|(_, p)| p).collect();
        assert_eq!(alerts.len(), 1, "one alert window per run");
        let lines = alerts[0]["alerts"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["from"], "Bob");
        assert_eq!(lines[0]["subject"], "One");
        assert_eq!(lines[0]["text"], "From Bob");
        assert!(lines[0]["receivedText"].as_str().unwrap().contains("2026"));
        let sound: Vec<&Value> = d.iter().filter(|(n, _)| n == "ruleSound").map(|(_, p)| p).collect();
        assert_eq!(sound[0]["paths"], json!(["/sounds/ding.wav"]), "the same sound plays once");
        // Print goes to the print command (file.print opens the print dialog with both messages).
        let print = d.iter().find(|(n, _)| n == "print").map(|(_, p)| p).unwrap();
        assert_eq!(print["ids"], json!([a.0, b.0]));
        let toasts =
            s.notices.iter().filter(|n| matches!(n, Notice::NewItemAlert(t, _) if t == "Bob: One")).count();
        assert_eq!(toasts, 1);
        assert_eq!(s.mailbox.mail.get(a).unwrap().sensitivity, Sensitivity::Private);
        // Without a print command the run says so instead.
        s.registry.specs.retain(|c| c.id != "file.print");
        s.notices.clear();
        run_rules(&mut s, &[a], &[rule(vec![], vec![RuleAction::Print])]);
        assert!(
            s.notices.iter().any(|n| matches!(n, Notice::Info(t) if t.contains("printing is not available")))
        );
    }

    /// "print it" queues the item for the end of the run; a later rule that permanently deletes it leaves
    /// nothing to print, so no Print page opens for a missing item.
    #[test]
    fn print_skips_items_a_later_rule_removed() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "One", ""));
        let b = deliver(&mut s, inbox, &raw_message("ann@example.org", "Two", ""));
        let print = rule(vec![], vec![RuleAction::Print]);
        let purge = rule(
            vec![mailcraft_model::RuleCondition::SubjectContains(vec!["one".into()])],
            vec![RuleAction::PermanentlyDelete],
        );
        s.notices.clear();
        let report = run_rules(&mut s, &[a, b], &[print.clone(), purge.clone()]);
        assert_eq!(report.printed, vec![b]);
        let d = dialogs(&s);
        let p = d.iter().find(|(n, _)| n == "print").map(|(_, p)| p).unwrap();
        assert_eq!(p["ids"], json!([b.0]));
        // Only removed items: no Print page at all.
        let c = deliver(&mut s, inbox, &raw_message("bob@example.org", "One more", ""));
        s.notices.clear();
        let report = run_rules(&mut s, &[c], &[print, purge]);
        assert!(report.printed.is_empty());
        assert!(!dialogs(&s).iter().any(|(n, _)| n == "print"));
    }

    #[test]
    fn forward_and_forward_as_attachment_go_to_the_outbox() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "Report", ""));
        let fwd = rule(vec![], vec![RuleAction::ForwardTo(vec!["Carol <carol@example.net>".into()])]);
        let report = run_rules(&mut s, &[a], &[fwd]);
        assert_eq!(report.sent.len(), 1, "{:?}", report.errors);
        let out = outbox_items(&s, acc);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].subject, "FW: Report");
        assert_eq!(out[0].to[0].addr, "carol@example.net");
        assert!(s.composes.is_empty(), "no compose window left open");
        assert!(!s.notices.iter().any(|n| matches!(n, Notice::Compose(_))));
        // The original is still in the Inbox.
        assert_eq!(s.mailbox.mail.get(a).unwrap().folder, inbox);
        let att = rule(vec![], vec![RuleAction::ForwardAsAttachment(vec!["carol@example.net".into()])]);
        run_rules(&mut s, &[a], &[att]);
        let out = outbox_items(&s, acc);
        assert_eq!(out.len(), 2);
        let raw = out.iter().map(|m| s.source_text(m.id)).collect::<Vec<_>>();
        assert!(raw.iter().any(|r| r.contains("message/rfc822")), "attached as .eml");
    }

    #[test]
    fn redirect_keeps_the_original_and_adds_resent_fields() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "Invoice", ""));
        run_rules(&mut s, &[a], &[rule(vec![], vec![RuleAction::RedirectTo(vec!["ap@example.net".into()])])]);
        let out = outbox_items(&s, acc);
        assert_eq!(out.len(), 1);
        let m = out[0];
        assert_eq!(m.subject, "Invoice");
        assert_eq!(m.from.as_ref().unwrap().addr, "bob@example.org", "the sender stays the original one");
        assert_eq!(m.to.len(), 1);
        assert_eq!(m.to[0].addr, "ap@example.net");
        let raw = s.source_text(m.id);
        assert!(raw.starts_with("Resent-From: Me Example <me@example.com>\r\nResent-Date: "), "{raw}");
        assert!(raw.contains("Resent-To: ap@example.net\r\n"));
        assert!(raw.contains("Resent-Message-ID: <"));
        assert!(raw.contains("From: Bob <bob@example.org>"));
        assert!(mailcraft_mime::parse(raw.as_bytes()).is_ok());
    }

    #[test]
    fn no_automatic_mail_to_myself_or_without_valid_addresses() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let mine = deliver(&mut s, inbox, &raw_message("me@example.com", "Note to self", ""));
        let actions = vec![
            RuleAction::ForwardTo(vec!["carol@example.net".into()]),
            RuleAction::RedirectTo(vec!["carol@example.net".into()]),
            RuleAction::ServerReply("Thanks".into()),
        ];
        let report = run_rules(&mut s, &[mine], &[rule(vec![], actions)]);
        assert!(report.sent.is_empty());
        let other = deliver(&mut s, inbox, &raw_message("bob@example.org", "x", ""));
        let report = run_rules(
            &mut s,
            &[other],
            &[rule(vec![], vec![RuleAction::ForwardTo(vec!["not an address".into()])])],
        );
        assert!(report.sent.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert!(s.notices.iter().any(|n| matches!(n, Notice::Error(_))));
        assert!(outbox_items(&s, acc).is_empty());
    }

    #[test]
    fn no_sending_account_is_reported() {
        let mut s = Session::new(Mailbox::new_local("Local"));
        let acc = s.mailbox.accounts[0].id;
        let inbox = s.mailbox.role_folder(acc, FolderRole::Inbox).unwrap();
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "x", ""));
        let report = run_rules(
            &mut s,
            &[a],
            &[rule(vec![], vec![RuleAction::ForwardTo(vec!["c@example.net".into()])])],
        );
        assert!(report.sent.is_empty());
        assert!(report.errors[0].contains("no account can send"), "{:?}", report.errors);
    }

    fn template_file(dir: &std::path::Path) -> String {
        let path = dir.join("thanks.eml");
        std::fs::write(
            &path,
            b"From: me@example.com\r\nSubject: Thanks for writing\r\nMIME-Version: 1.0\r\n\
              Content-Type: text/plain; charset=utf-8\r\n\r\nI am away until Monday.\r\n",
        )
        .unwrap();
        path.display().to_string()
    }

    #[test]
    fn reply_using_a_template_once_per_sender() {
        let dir = tempfile::tempdir().unwrap();
        let tpl = template_file(dir.path());
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "Question", ""));
        let b = deliver(&mut s, inbox, &raw_message("Bob <bob@example.org>", "Another", ""));
        let auto = deliver(
            &mut s,
            inbox,
            &raw_message("Ann <ann@example.org>", "Away", "Auto-Submitted: auto-replied\r\n"),
        );
        let list = deliver(
            &mut s,
            inbox,
            &raw_message("list@example.org", "News", "List-Id: <news.example.org>\r\n"),
        );
        let r = rule(vec![], vec![RuleAction::ReplyWithTemplate(tpl)]);
        let report = run_rules(&mut s, &[a, b, auto, list], &[r]);
        assert_eq!(
            report.sent.len(),
            1,
            "one reply per sender, none to automatic or list mail: {:?}",
            report.errors
        );
        let out = outbox_items(&s, acc);
        let m = out[0];
        assert_eq!(m.subject, "Thanks for writing");
        assert_eq!(m.to[0].addr, "bob@example.org");
        let raw = s.source_text(m.id);
        assert!(raw.contains("I am away until Monday."), "{raw}");
        assert!(!raw.contains("Hello there."), "the original is not quoted");
        assert!(raw.contains("Auto-Submitted: auto-replied"));
        assert!(raw.contains("In-Reply-To:"), "threaded as a reply");
        // A missing template is reported, nothing is sent.
        let c = deliver(&mut s, inbox, &raw_message("dan@example.org", "Hi", ""));
        let r = rule(vec![], vec![RuleAction::ReplyWithTemplate("/no/such/dir/x.eml".into())]);
        let report = run_rules(&mut s, &[c], &[r]);
        assert!(report.sent.is_empty());
        assert!(report.errors[0].contains("not found"), "{:?}", report.errors);
    }

    #[test]
    fn server_reply_uses_the_text() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "Order", ""));
        run_rules(&mut s, &[a], &[rule(vec![], vec![RuleAction::ServerReply("We got your order.".into())])]);
        let out = outbox_items(&s, acc);
        assert_eq!(out[0].subject, "RE: Order");
        assert!(s.body_text(out[0]).contains("We got your order."));
    }

    #[test]
    fn send_rules_defer_cc_and_notify() {
        let (mut s, acc) = session();
        let mut r = rule(
            vec![mailcraft_model::RuleCondition::SubjectContains(vec!["budget".into()])],
            vec![
                RuleAction::DeferDelivery(10),
                RuleAction::CcTo(vec!["Boss <boss@example.net>".into()]),
                RuleAction::NotifyWhenRead,
                RuleAction::NotifyWhenDelivered,
                RuleAction::MarkSensitivity(Sensitivity::Confidential),
                RuleAction::AssignCategory("Red category".into()),
                RuleAction::MarkImportance(Importance::High),
            ],
        );
        r.set_outgoing(true);
        s.mailbox.rules = vec![r];
        let cid = s.open_compose(crate::compose::ComposeState {
            to: "ann@example.org".into(),
            subject: "Budget 2027".into(),
            ..Default::default()
        });
        s.view.send_immediately = false;
        let sent = s.run("compose.send", &json!({"compose": cid})).unwrap();
        let id = ItemId(sent["item"].as_u64().unwrap());
        // compose.send already ran the send rules: the message waits in the Outbox.
        assert!(sent["deferredUntil"].is_string(), "{sent}");
        // Receive rules ignore it; send rules act on it (running them again changes nothing).
        assert_eq!(on_new_mail(&mut s, &[id]).matched, 0);
        let report = on_send(&mut s, &[id]);
        assert_eq!(report.matched, 1);
        let m = s.mailbox.mail.get(id).unwrap();
        assert_eq!(m.sent.unwrap().unix, s.now().unix + 600, "held 10 minutes");
        assert!(m.cc.iter().any(|a| a.addr == "boss@example.net"));
        assert_eq!(m.categories, vec!["Red category".to_string()]);
        assert_eq!(m.importance, Importance::High);
        assert_eq!(m.sensitivity, Sensitivity::Confidential);
        let raw = s.source(m.id).unwrap().unwrap();
        assert_eq!(header_value(&raw, "Cc").as_deref(), Some("Boss <boss@example.net>"));
        assert_eq!(
            header_value(&raw, "Disposition-Notification-To").as_deref(),
            Some("Me Example <me@example.com>")
        );
        assert!(header_value(&raw, "Return-Receipt-To").is_none());
        assert_eq!(
            header_value(&raw, crate::cmd::compose::DSN_HEADER).as_deref(),
            Some("NOTIFY=SUCCESS,FAILURE; RET=HDRS")
        );
        assert_eq!(header_value(&raw, "Sensitivity").as_deref(), Some("Company-Confidential"));
        let parsed = mailcraft_mime::parse(&raw).unwrap();
        let again = MailItem::from_message(&parsed, &raw, s.now());
        assert_eq!(again.cc.len(), 1);
        // Items outside an Outbox are not touched by send rules.
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let x = deliver(&mut s, inbox, &raw_message("bob@example.org", "budget", ""));
        assert_eq!(on_send(&mut s, &[x]).matched, 0);
        assert!(s.mailbox.mail.get(x).unwrap().categories.is_empty());
    }

    #[test]
    fn receive_rules_skip_send_only_actions() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "x", ""));
        let before = s.mailbox.mail.get(a).unwrap().clone();
        run_rules(
            &mut s,
            &[a],
            &[rule(
                vec![],
                vec![RuleAction::DeferDelivery(5), RuleAction::CcTo(vec!["c@example.net".into()])],
            )],
        );
        assert_eq!(s.mailbox.mail.get(a).unwrap(), &before);
    }

    #[test]
    fn run_now_which_and_subfolders() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let sub = s.mailbox.create_folder(inbox, "Sub").unwrap();
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "a", ""));
        let b = deliver(&mut s, sub, &raw_message("bob@example.org", "b", ""));
        s.mailbox.set_read(&[a], true);
        let r = rule(vec![], vec![RuleAction::AssignCategory("Blue category".into())]);
        let (n, rep) = run_now(&mut s, std::slice::from_ref(&r), inbox, false, Which::Unread);
        assert_eq!((n, rep.matched), (0, 0));
        let (n, rep) = run_now(&mut s, std::slice::from_ref(&r), inbox, true, Which::Unread);
        assert_eq!((n, rep.matched), (1, 1));
        assert_eq!(s.mailbox.mail.get(b).unwrap().categories.len(), 1);
        let (n, _) = run_now(&mut s, &[r], inbox, true, Which::All);
        assert_eq!(n, 2);
        assert_eq!(Which::parse("READ"), Some(Which::Read));
        assert_eq!(Which::parse("bogus"), None);
    }

    #[test]
    fn stop_processing_and_deleted_items_end_the_list() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "x", ""));
        let r1 = rule(vec![], vec![RuleAction::MarkAsRead, RuleAction::StopProcessingMoreRules]);
        let r2 = rule(vec![], vec![RuleAction::AssignCategory("Red category".into())]);
        run_rules(&mut s, &[a], &[r1, r2.clone()]);
        assert!(s.mailbox.mail.get(a).unwrap().read && s.mailbox.mail.get(a).unwrap().categories.is_empty());
        let r0 = rule(vec![], vec![RuleAction::PermanentlyDelete]);
        assert_eq!(run_rules(&mut s, &[a, ItemId(u64::MAX)], &[r0, r2]).matched, 1);
        assert!(!s.mailbox.mail.contains(a));
    }

    /// Rules on arriving mail run outside `Session::run`: what they delete for good must still reach the
    /// recoverable items.
    #[test]
    fn mail_a_rule_deletes_for_good_on_arrival_stays_recoverable() {
        let (mut s, acc) = session();
        let inbox = folder(&s, acc, FolderRole::Inbox);
        let a = deliver(&mut s, inbox, &raw_message("bob@example.org", "Clearance offer", ""));
        let kept = deliver(&mut s, inbox, &raw_message("ann@example.org", "Agenda", ""));
        s.mailbox.rules = vec![rule(
            vec![mailcraft_model::RuleCondition::SubjectContains(vec!["offer".into()])],
            vec![RuleAction::PermanentlyDelete],
        )];
        crate::sync::arrival::on_new_mail(&mut s, &[a, kept]);
        assert!(!s.mailbox.mail.contains(a));
        let listed: Vec<ItemId> = crate::recover::listed(&s).iter().map(|e| e.item.id()).collect();
        assert_eq!(listed, vec![a]);
        assert_eq!(crate::recover::listed(&s)[0].folder_path, "Inbox");
    }

    #[test]
    fn header_editing() {
        let raw = b"From: a@example.org\r\nCc: x@example.org,\r\n y@example.org\r\nSubject: s\r\n\r\nbody\r\nCc: not a header\r\n";
        assert_eq!(header_value(raw, "cc").as_deref(), Some("x@example.org, y@example.org"));
        let out = set_header(raw, "Cc", Some("z@example.org".into()));
        let text = String::from_utf8(out.clone()).unwrap();
        assert_eq!(
            text,
            "From: a@example.org\r\nSubject: s\r\nCc: z@example.org\r\n\r\nbody\r\nCc: not a header\r\n"
        );
        let removed = String::from_utf8(set_header(&out, "cc", None)).unwrap();
        assert_eq!(removed, "From: a@example.org\r\nSubject: s\r\n\r\nbody\r\nCc: not a header\r\n");
        // LF-only sources keep LF; a value cannot inject a field.
        let lf = set_header(b"Subject: s\n\nbody\n", "X-Test", Some("a\r\nBcc: evil@example.org".into()));
        assert_eq!(String::from_utf8(lf).unwrap(), "Subject: s\nX-Test: aBcc: evil@example.org\n\nbody\n");
        // Header-only and empty sources.
        assert_eq!(
            String::from_utf8(set_header(b"Subject: s", "A", Some("1".into()))).unwrap(),
            "Subject: s\r\nA: 1\r\n\r\n"
        );
        assert_eq!(String::from_utf8(set_header(b"", "A", Some("1".into()))).unwrap(), "A: 1\r\n\r\n");
        assert_eq!(header_value(b"", "A"), None);
        assert_eq!(header_value(b"\xff\xfe: \xff\r\n\r\n", "A"), None);
    }

    proptest::proptest! {
        #[test]
        fn header_editing_never_panics(raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400),
                                       name in "[A-Za-z-]{1,12}", value in ".{0,40}") {
            let out = set_header(&raw, &name, Some(value));
            let _ = header_value(&out, &name);
            let _ = set_header(&out, &name, None);
        }
    }

    /// End to end against the local test servers: a rule on newly received mail forwards it over SMTP and
    /// shows it in the New Item Alert window (the send/receive hook).
    #[test]
    fn sync_hook_runs_rules_on_new_mail_and_forwards_over_smtp() {
        use mailcraft_testmail::{Config, TestServers};
        let srv = TestServers::start(Config {
            users: vec![
                ("alice@example.com".into(), "alice-pw".into()),
                ("bob@example.org".into(), "bob-pw".into()),
            ],
            ..Config::default()
        })
        .unwrap();
        let mut s = Session::new(Mailbox::new_local("Local"));
        s.view.auto_send_receive_minutes = 0;
        let r = s
            .run(
                "accounts.add",
                &json!({
                    "kind": "imap",
                    "address": "alice@example.com",
                    "senderName": "Alice Example",
                    "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
                    "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
                    "password": "alice-pw",
                }),
            )
            .unwrap();
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        s.run(
            "rules.create",
            &json!({"name": "Tickets", "conditions": [{"SubjectContains": ["ticket"]}],
                    "actions": [{"NewItemAlert": "Ticket"}, {"ForwardTo": ["bob@example.org"]}]}),
        )
        .unwrap();
        let raw = b"From: Carol <carol@example.net>\r\nTo: alice@example.com\r\nSubject: Ticket 42\r\n\
                    Date: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <t42@example.net>\r\n\
                    Content-Type: text/plain; charset=utf-8\r\n\r\nPrinter on fire.\r\n";
        srv.seed_message("alice@example.com", "INBOX", raw, &[]).unwrap();
        s.notices.clear();
        s.run("sendReceive.all", &json!({})).unwrap();
        let wait = std::time::Duration::from_secs(30);
        assert!(s.wait_for_sync(wait), "{:?}", s.sync_status());
        assert!(s.wait_for_sync(wait), "{:?}", s.sync_status());
        let alerts = dialogs(&s).into_iter().find(|(n, _)| n == "newItemAlerts").map(|(_, p)| p).unwrap();
        assert_eq!(alerts["alerts"][0]["subject"], "Ticket 42");
        let delivered: Vec<String> = srv
            .messages("bob@example.org", "INBOX")
            .unwrap()
            .iter()
            .filter_map(|m| mailcraft_mime::parse(&m.raw).ok().map(|x| x.subject()))
            .collect();
        assert!(delivered.contains(&"FW: Ticket 42".to_owned()), "{delivered:?}");
        let outbox = s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap();
        assert_eq!(s.mailbox.total_count(outbox), 0, "the forward left the Outbox");
    }
}
