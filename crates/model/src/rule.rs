//! Rules (`plan/reference/rules.md`). Conditions/actions mirror the Rules Wizard lists; evaluation lives here
//! (pure), execution of actions (move, forward, alert) lives in the engine (`rules_exec`).
//!
//! A rule applies to messages the user receives unless its conditions contain the marker
//! [`RuleCondition::AfterSending`] ("Apply rule on messages I send"); the account a rule is limited to is its
//! [`RuleCondition::ThroughAccount`] condition. Both are conditions (not fields) so rule files stay one flat
//! list of conditions, actions and exceptions.

use serde::{Deserialize, Serialize};

use crate::{Address, AttachmentMeta, FolderId, FolderRole, Importance, MailItem, Sensitivity};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleCondition {
    /// "from people or public group": entries with a complete address match that address exactly, other
    /// entries (names) as text.
    From(Vec<String>),
    /// "with specific words in the subject" (any of)
    SubjectContains(Vec<String>),
    BodyContains(Vec<String>),
    SubjectOrBodyContains(Vec<String>),
    HeaderContains(Vec<String>),
    RecipientAddressContains(Vec<String>),
    SenderAddressContains(Vec<String>),
    /// "sent to people or public group"
    SentTo(Vec<String>),
    /// "through the specified account" (account address)
    ThroughAccount(String),
    SentOnlyToMe,
    MyNameInTo,
    MyNameInCc,
    MyNameInToOrCc,
    MyNameNotInTo,
    MarkedImportance(Importance),
    MarkedSensitivity(Sensitivity),
    FlaggedForAction,
    AssignedToCategory(String),
    HasAttachment,
    /// Size in KB, inclusive range.
    SizeBetween(u64, u64),
    /// Unix seconds, inclusive range.
    ReceivedBetween(i64, i64),
    /// "which is an automatic reply" (RFC 3834 `Auto-Submitted`, `X-Autoreply`, "Automatic reply:" subjects).
    AutomaticReply,
    /// "which is a meeting invitation or update" (an iCalendar REQUEST/CANCEL/REPLY part, RFC 5546).
    MeetingInvitationOrUpdate,
    /// "uses the <form> form": the form's display name ([`FORMS`]: Message, Meeting Request, …).
    UsesForm(String),
    /// "of the <type> form type": a message class prefix ("IPM.Schedule.Meeting", "REPORT.IPM.Note").
    FormType(String),
    /// "from RSS Feeds with <text> in the title" (the feed's folder name under RSS Feeds).
    FromRssFeedWithText(Vec<String>),
    /// "from any RSS Feed".
    FromAnyRssFeed,
    /// "suspected to be junk or from Junk Senders" (the item sits in a Junk Email folder).
    SuspectedJunk,
    /// "sender is in my Contacts".
    SenderInContacts,
    /// "on this computer only": always true here, and makes the rule client-only.
    OnThisComputerOnly,
    /// Marker, not a test: the rule applies to messages the user sends ("Apply rule on messages I send").
    AfterSending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    /// "display a specific message in the New Item Alert window"
    NewItemAlert(String),
    MoveTo(FolderId),
    CopyTo(FolderId),
    AssignCategory(String),
    Delete,
    PermanentlyDelete,
    ForwardTo(Vec<String>),
    ForwardAsAttachment(Vec<String>),
    RedirectTo(Vec<String>),
    /// Template file path.
    ReplyWithTemplate(String),
    /// Flag label ("Follow up", …) and days until due.
    FlagForFollowUp(String, u32),
    ClearFlag,
    ClearCategories,
    MarkImportance(Importance),
    Print,
    PlaySound(String),
    MarkAsRead,
    StopProcessingMoreRules,
    DesktopAlert,
    /// Messages I send: "defer delivery by N minutes" (held in the Outbox).
    DeferDelivery(u32),
    /// Messages I send: "Cc the message to people or public group".
    CcTo(Vec<String>),
    /// "mark it as <sensitivity>".
    MarkSensitivity(Sensitivity),
    /// Messages I send: "notify me when it is read" (RFC 8098 `Disposition-Notification-To`).
    NotifyWhenRead,
    /// Messages I send: "notify me when it is delivered" (`Return-Receipt-To`).
    NotifyWhenDelivered,
    /// "have server reply using a specific message": the reply's text.
    ServerReply(String),
}

/// Which messages an action can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionScope {
    /// Messages the user receives and messages the user sends.
    Both,
    Incoming,
    Outgoing,
}

impl RuleAction {
    /// Actions only the client can do make the rule "(client-only)".
    pub fn is_client_only(&self) -> bool {
        matches!(
            self,
            RuleAction::NewItemAlert(_)
                | RuleAction::PlaySound(_)
                | RuleAction::Print
                | RuleAction::DesktopAlert
                | RuleAction::ReplyWithTemplate(_)
                | RuleAction::DeferDelivery(_)
        )
    }
    /// Which messages this action applies to; the rule executor skips it for the others.
    pub fn scope(&self) -> ActionScope {
        match self {
            RuleAction::DeferDelivery(_)
            | RuleAction::CcTo(_)
            | RuleAction::NotifyWhenRead
            | RuleAction::NotifyWhenDelivered => ActionScope::Outgoing,
            RuleAction::NewItemAlert(_)
            | RuleAction::DesktopAlert
            | RuleAction::PlaySound(_)
            | RuleAction::ReplyWithTemplate(_)
            | RuleAction::ServerReply(_)
            | RuleAction::ForwardTo(_)
            | RuleAction::ForwardAsAttachment(_)
            | RuleAction::RedirectTo(_)
            | RuleAction::MoveTo(_)
            | RuleAction::Delete
            | RuleAction::PermanentlyDelete
            | RuleAction::MarkAsRead
            | RuleAction::Print => ActionScope::Incoming,
            RuleAction::CopyTo(_)
            | RuleAction::AssignCategory(_)
            | RuleAction::ClearCategories
            | RuleAction::FlagForFollowUp(..)
            | RuleAction::ClearFlag
            | RuleAction::MarkImportance(_)
            | RuleAction::MarkSensitivity(_)
            | RuleAction::StopProcessingMoreRules => ActionScope::Both,
        }
    }
    /// True when the action can run on a message going in the given direction.
    pub fn applies_to(&self, outgoing: bool) -> bool {
        match self.scope() {
            ActionScope::Both => true,
            ActionScope::Incoming => !outgoing,
            ActionScope::Outgoing => outgoing,
        }
    }
    /// Actions that show something to the user (the Manage Alerts tab lists rules with them).
    pub fn is_alert(&self) -> bool {
        matches!(self, RuleAction::NewItemAlert(_) | RuleAction::DesktopAlert | RuleAction::PlaySound(_))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// All must match.
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
    /// Any match cancels the rule.
    pub exceptions: Vec<RuleCondition>,
}

/// Context for "me" conditions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleContext {
    /// The user's own addresses (lower-case).
    pub my_addresses: Vec<String>,
    /// Address of the account the item arrived through.
    pub account_address: String,
}

/// Facts about where an item sits that conditions need beyond the item itself (filled by the engine).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleEnv {
    /// Role of the item's folder.
    pub folder_role: Option<FolderRole>,
    /// The RSS feed (folder under RSS Feeds) the item came from.
    pub rss_feed: Option<String>,
    /// E-mail addresses of the user's contacts (lower-case).
    pub contacts: Vec<String>,
}

impl Rule {
    /// "(client-only)": an action only the client can do, or the "on this computer only" condition.
    pub fn is_client_only(&self) -> bool {
        self.actions.iter().any(RuleAction::is_client_only)
            || self.conditions.contains(&RuleCondition::OnThisComputerOnly)
    }
    /// "Apply rule on messages I send".
    pub fn is_outgoing(&self) -> bool {
        self.conditions.contains(&RuleCondition::AfterSending)
    }
    /// Makes the rule apply to sent (`true`) or received messages.
    pub fn set_outgoing(&mut self, outgoing: bool) {
        self.conditions.retain(|c| *c != RuleCondition::AfterSending);
        if outgoing {
            self.conditions.insert(0, RuleCondition::AfterSending);
        }
    }
    /// The account address the rule is limited to ("through the specified account"), if any.
    pub fn account(&self) -> Option<&str> {
        self.conditions.iter().find_map(|c| match c {
            RuleCondition::ThroughAccount(a) if !a.trim().is_empty() => Some(a.trim()),
            _ => None,
        })
    }
    /// A copy limited to the account `address` (replacing any other account condition).
    pub fn for_account(&self, address: &str) -> Rule {
        let mut r = self.clone();
        r.conditions.retain(|c| !matches!(c, RuleCondition::ThroughAccount(_)));
        let at = usize::from(r.is_outgoing());
        r.conditions.insert(at.min(r.conditions.len()), RuleCondition::ThroughAccount(address.trim().into()));
        r
    }
    /// True when an action shows an alert (New Item Alert, Desktop Alert, sound).
    pub fn has_alert(&self) -> bool {
        self.actions.iter().any(RuleAction::is_alert)
    }
    /// True when every condition matches and no exception matches. A rule with no conditions matches all.
    pub fn matches(&self, item: &MailItem, ctx: &RuleContext) -> bool {
        self.matches_in(item, ctx, &RuleEnv::default())
    }
    /// [`Rule::matches`] with the item's surroundings (folder role, feed, contacts).
    pub fn matches_in(&self, item: &MailItem, ctx: &RuleContext, env: &RuleEnv) -> bool {
        self.conditions.iter().all(|c| c.matches_in(item, ctx, env))
            && !self
                .exceptions
                .iter()
                .filter(|c| **c != RuleCondition::AfterSending)
                .any(|c| c.matches_in(item, ctx, env))
    }
    /// Natural-language description as in the Rules and Alerts "Rule description" box, one clause per line:
    /// "Apply this rule after the message arrives", "with <words> in the subject", "move it to the <folder>
    /// folder", "except if …". `folder_name` resolves folder ids.
    ///
    /// Second and later conditions and actions start with "and "; exceptions start with "except if " and later
    /// ones with "or except if " (clauses that already read as a "where …" phrase take just "except "/"or
    /// except ", and "which has …" becomes "if it has …", so the line reads as English).
    pub fn describe(&self, folder_name: &dyn Fn(crate::FolderId) -> String) -> Vec<String> {
        let first = if self.is_outgoing() {
            "Apply this rule after I send the message"
        } else {
            "Apply this rule after the message arrives"
        };
        let mut lines = vec![first.to_string()];
        let marker = |c: &&RuleCondition| **c != RuleCondition::AfterSending;
        for (i, c) in self.conditions.iter().filter(marker).enumerate() {
            let clause = c.describe();
            lines.push(if i == 0 { clause } else { format!("and {clause}") });
        }
        for (i, a) in self.actions.iter().enumerate() {
            let clause = a.describe(folder_name);
            lines.push(if i == 0 { clause } else { format!("and {clause}") });
        }
        for (i, e) in self.exceptions.iter().filter(marker).enumerate() {
            let clause = e.describe();
            let clause = exception_clause(&clause);
            lines.push(if i == 0 { format!("except {clause}") } else { format!("or except {clause}") });
        }
        lines
    }
    /// Row label in the rules list: the name plus "  (client-only)" when applicable.
    pub fn list_label(&self) -> String {
        if self.is_client_only() { format!("{}  (client-only)", self.name) } else { self.name.clone() }
    }
}

impl RuleContext {
    /// True when `addr` is one of the user's own addresses (case-insensitive).
    pub fn is_me(&self, addr: &str) -> bool {
        let addr = addr.trim();
        !addr.is_empty() && self.my_addresses.iter().any(|m| m.trim().eq_ignore_ascii_case(addr))
    }
}

/// Condition clause rephrased to follow "except": "where my name …" stays, "which has an attachment" becomes
/// "if it has an attachment", anything else gets "if ".
fn exception_clause(clause: &str) -> String {
    if clause.starts_with("where ") {
        clause.to_string()
    } else if let Some(rest) = clause.strip_prefix("which ") {
        format!("if it {rest}")
    } else {
        format!("if {clause}")
    }
}

/// Words joined the way the description box lists alternatives: "a or b or c"; a placeholder when empty.
fn alternatives(words: &[String], placeholder: &str) -> String {
    let words: Vec<&str> = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() { placeholder.to_string() } else { words.join(" or ") }
}

/// People list as written in the description box: "a; b".
fn people(list: &[String]) -> String {
    let list: Vec<&str> = list.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    if list.is_empty() { "people or public group".to_string() } else { list.join("; ") }
}

fn importance_word(i: Importance) -> &'static str {
    match i {
        Importance::Low => "low",
        Importance::Normal => "normal",
        Importance::High => "high",
    }
}

fn sensitivity_word(s: Sensitivity) -> &'static str {
    match s {
        Sensitivity::Normal => "normal",
        Sensitivity::Personal => "personal",
        Sensitivity::Private => "private",
        Sensitivity::Confidential => "confidential",
    }
}

fn any_word_in(text: &str, words: &[String]) -> bool {
    if words.iter().all(|w| w.trim().is_empty()) {
        return false;
    }
    let hay = text.to_lowercase();
    words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).any(|w| hay.contains(&w.to_lowercase()))
}

/// "Name <addr>" (or just the address) for substring matching against people entries.
fn person_text(m: &Address) -> String {
    match &m.name {
        Some(n) => format!("{n} <{}>", m.addr),
        None => m.addr.clone(),
    }
}

/// The address a people entry names, when it holds a complete one ("a@b.example" or "Name <a@b.example>").
fn entry_address(entry: &str) -> Option<&str> {
    let e = entry.trim();
    let a = match (e.rfind('<'), e.rfind('>')) {
        (Some(l), Some(r)) if l < r => e.get(l + 1..r)?.trim(),
        _ => e,
    };
    let (local, domain) = a.split_once('@')?;
    let ok = !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && !a.chars().any(|c| c.is_whitespace() || c == '<' || c == '>');
    ok.then_some(a)
}

/// "from / sent to people or public group": an entry with a complete address names that person only (the
/// address must be equal); any other entry (a name, part of an address) matches as text.
fn person_matches(m: &Address, list: &[String]) -> bool {
    let text = person_text(m).to_lowercase();
    list.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).any(|w| match entry_address(w) {
        Some(a) => m.addr.trim().eq_ignore_ascii_case(a),
        None => text.contains(&w.to_lowercase()),
    })
}

/// Header block used for "with specific words in the message header": the raw source's header section
/// when kept, else a synthesised one from the item's fields.
fn header_text(item: &MailItem) -> String {
    if let Some(raw) = &item.raw {
        let limit = raw.len().min(256 * 1024);
        let head = raw.get(..limit).unwrap_or(&[]);
        // The header section ends at the first blank line, whichever line-end style it uses.
        let crlf = head.windows(4).position(|w| w == b"\r\n\r\n");
        let lf = head.windows(2).position(|w| w == b"\n\n");
        let end = match (crlf, lf) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(head.len()),
        };
        return String::from_utf8_lossy(head.get(..end).unwrap_or(head)).into_owned();
    }
    let list = |l: &[Address]| l.iter().map(person_text).collect::<Vec<_>>().join(", ");
    let mut h = String::new();
    if let Some(f) = &item.from {
        h.push_str(&format!("From: {}\n", person_text(f)));
    }
    if let Some(f) = &item.sender {
        h.push_str(&format!("Sender: {}\n", person_text(f)));
    }
    if !item.to.is_empty() {
        h.push_str(&format!("To: {}\n", list(&item.to)));
    }
    if !item.cc.is_empty() {
        h.push_str(&format!("Cc: {}\n", list(&item.cc)));
    }
    if !item.reply_to.is_empty() {
        h.push_str(&format!("Reply-To: {}\n", list(&item.reply_to)));
    }
    h.push_str(&format!("Subject: {}\n", item.subject));
    if let Some(id) = &item.message_id {
        h.push_str(&format!("Message-ID: <{id}>\n"));
    }
    if !item.in_reply_to.is_empty() {
        h.push_str(&format!("In-Reply-To: <{}>\n", item.in_reply_to.join("> <")));
    }
    if !item.references.is_empty() {
        h.push_str(&format!("References: <{}>\n", item.references.join("> <")));
    }
    h
}

impl RuleCondition {
    /// Evaluates this one condition against `item`. Text comparisons are case-insensitive substring
    /// matches; an empty word list matches nothing.
    pub fn matches(&self, item: &MailItem, ctx: &RuleContext) -> bool {
        self.matches_in(item, ctx, &RuleEnv::default())
    }

    /// [`RuleCondition::matches`] with the item's surroundings.
    pub fn matches_in(&self, item: &MailItem, ctx: &RuleContext, env: &RuleEnv) -> bool {
        match self {
            RuleCondition::From(list) => {
                item.from.iter().chain(item.sender.iter()).any(|m| person_matches(m, list))
            }
            RuleCondition::SubjectContains(words) => any_word_in(&item.subject, words),
            RuleCondition::BodyContains(words) => any_word_in(&item.body_text(), words),
            RuleCondition::SubjectOrBodyContains(words) => {
                any_word_in(&item.subject, words) || any_word_in(&item.body_text(), words)
            }
            RuleCondition::HeaderContains(words) => any_word_in(&header_text(item), words),
            RuleCondition::RecipientAddressContains(words) => {
                item.to.iter().chain(&item.cc).chain(&item.bcc).any(|m| any_word_in(&m.addr, words))
            }
            RuleCondition::SenderAddressContains(words) => {
                item.from.iter().chain(item.sender.iter()).any(|m| any_word_in(&m.addr, words))
            }
            RuleCondition::SentTo(list) => item.to.iter().chain(&item.cc).any(|m| person_matches(m, list)),
            RuleCondition::ThroughAccount(acct) => {
                let acct = acct.trim();
                !acct.is_empty() && ctx.account_address.trim().eq_ignore_ascii_case(acct)
            }
            RuleCondition::SentOnlyToMe => {
                item.cc.is_empty() && item.to.len() == 1 && item.to.iter().all(|m| ctx.is_me(&m.addr))
            }
            RuleCondition::MyNameInTo => item.to.iter().any(|m| ctx.is_me(&m.addr)),
            RuleCondition::MyNameInCc => item.cc.iter().any(|m| ctx.is_me(&m.addr)),
            RuleCondition::MyNameInToOrCc => item.to.iter().chain(&item.cc).any(|m| ctx.is_me(&m.addr)),
            RuleCondition::MyNameNotInTo => !item.to.iter().any(|m| ctx.is_me(&m.addr)),
            RuleCondition::MarkedImportance(i) => item.importance == *i,
            RuleCondition::MarkedSensitivity(s) => item.sensitivity == *s,
            RuleCondition::FlaggedForAction => item.is_flagged(),
            RuleCondition::AssignedToCategory(cat) => {
                let cat = cat.trim().to_lowercase();
                !cat.is_empty() && item.categories.iter().any(|c| c.trim().to_lowercase() == cat)
            }
            RuleCondition::HasAttachment => item.has_attachments(),
            RuleCondition::SizeBetween(a, b) => {
                let (lo, hi) = if a <= b { (*a, *b) } else { (*b, *a) };
                (lo..=hi).contains(&item.size_kb())
            }
            RuleCondition::ReceivedBetween(a, b) => {
                let (lo, hi) = if a <= b { (*a, *b) } else { (*b, *a) };
                (lo..=hi).contains(&item.received.unix)
            }
            RuleCondition::AutomaticReply => is_automatic_reply(item),
            RuleCondition::MeetingInvitationOrUpdate => {
                message_class(item).starts_with("IPM.Schedule.Meeting")
            }
            RuleCondition::UsesForm(name) => {
                let name = name.trim();
                !name.is_empty() && form_name(message_class(item)).eq_ignore_ascii_case(name)
            }
            RuleCondition::FormType(class) => {
                let class = class.trim().to_ascii_lowercase();
                !class.is_empty() && message_class(item).to_ascii_lowercase().starts_with(&class)
            }
            RuleCondition::FromRssFeedWithText(words) => {
                env.rss_feed.as_deref().is_some_and(|feed| any_word_in(feed, words))
            }
            RuleCondition::FromAnyRssFeed => env.rss_feed.is_some(),
            RuleCondition::SuspectedJunk => env.folder_role == Some(FolderRole::JunkEmail),
            RuleCondition::SenderInContacts => item.from.iter().chain(item.sender.iter()).any(|m| {
                let a = m.addr.trim();
                !a.is_empty() && env.contacts.iter().any(|c| c.trim().eq_ignore_ascii_case(a))
            }),
            RuleCondition::OnThisComputerOnly | RuleCondition::AfterSending => true,
        }
    }

    /// True for conditions that only make sense on messages the user receives.
    pub fn incoming_only(&self) -> bool {
        matches!(
            self,
            RuleCondition::AutomaticReply
                | RuleCondition::FromRssFeedWithText(_)
                | RuleCondition::FromAnyRssFeed
                | RuleCondition::SuspectedJunk
                | RuleCondition::SenderInContacts
                | RuleCondition::SentOnlyToMe
                | RuleCondition::MyNameInTo
                | RuleCondition::MyNameInCc
                | RuleCondition::MyNameInToOrCc
                | RuleCondition::MyNameNotInTo
                | RuleCondition::ReceivedBetween(..)
        )
    }

    /// One description clause, e.g. "with budget or forecast in the subject".
    pub fn describe(&self) -> String {
        const WORDS: &str = "specific words";
        match self {
            RuleCondition::From(list) => format!("from {}", people(list)),
            RuleCondition::SubjectContains(w) => format!("with {} in the subject", alternatives(w, WORDS)),
            RuleCondition::BodyContains(w) => format!("with {} in the body", alternatives(w, WORDS)),
            RuleCondition::SubjectOrBodyContains(w) => {
                format!("with {} in the subject or body", alternatives(w, WORDS))
            }
            RuleCondition::HeaderContains(w) => {
                format!("with {} in the message header", alternatives(w, WORDS))
            }
            RuleCondition::RecipientAddressContains(w) => {
                format!("with {} in the recipient's address", alternatives(w, WORDS))
            }
            RuleCondition::SenderAddressContains(w) => {
                format!("with {} in the sender's address", alternatives(w, WORDS))
            }
            RuleCondition::SentTo(list) => format!("sent to {}", people(list)),
            RuleCondition::ThroughAccount(a) => {
                let a = a.trim();
                format!("through the {} account", if a.is_empty() { "specified" } else { a })
            }
            RuleCondition::SentOnlyToMe => "sent only to me".to_string(),
            RuleCondition::MyNameInTo => "where my name is in the To box".to_string(),
            RuleCondition::MyNameInCc => "where my name is in the Cc box".to_string(),
            RuleCondition::MyNameInToOrCc => "where my name is in the To or Cc box".to_string(),
            RuleCondition::MyNameNotInTo => "where my name is not in the To box".to_string(),
            RuleCondition::MarkedImportance(i) => format!("marked as {} importance", importance_word(*i)),
            RuleCondition::MarkedSensitivity(s) => format!("marked as {} sensitivity", sensitivity_word(*s)),
            RuleCondition::FlaggedForAction => "flagged for any action".to_string(),
            RuleCondition::AssignedToCategory(c) => {
                let c = c.trim();
                format!("assigned to {} category", if c.is_empty() { "the chosen" } else { c })
            }
            RuleCondition::HasAttachment => "which has an attachment".to_string(),
            RuleCondition::SizeBetween(a, b) => {
                let (lo, hi) = if a <= b { (*a, *b) } else { (*b, *a) };
                format!("with a size between {lo} KB and {hi} KB")
            }
            RuleCondition::ReceivedBetween(a, b) => {
                let (lo, hi) = if a <= b { (*a, *b) } else { (*b, *a) };
                let d = |u: i64| crate::ListTime::date(crate::DateTime { unix: u, offset_min: 0 }, 0);
                format!("received between {} and {}", d(lo), d(hi))
            }
            RuleCondition::AutomaticReply => "which is an automatic reply".to_string(),
            RuleCondition::MeetingInvitationOrUpdate => "which is a meeting invitation or update".to_string(),
            RuleCondition::UsesForm(f) => {
                let f = f.trim();
                format!("uses the {} form", if f.is_empty() { "form name" } else { f })
            }
            RuleCondition::FormType(t) => {
                let t = t.trim();
                format!("of the {} form type", if t.is_empty() { "specific" } else { t })
            }
            RuleCondition::FromRssFeedWithText(w) => {
                format!("from RSS Feeds with {} in the title", alternatives(w, "specified text"))
            }
            RuleCondition::FromAnyRssFeed => "from any RSS Feed".to_string(),
            RuleCondition::SuspectedJunk => "suspected to be junk or from Junk Senders".to_string(),
            RuleCondition::SenderInContacts => "sender is in my Contacts".to_string(),
            RuleCondition::OnThisComputerOnly => "on this computer only".to_string(),
            RuleCondition::AfterSending => "after I send the message".to_string(),
        }
    }
}

impl RuleAction {
    /// One description clause, e.g. "move it to the Projects folder".
    pub fn describe(&self, folder_name: &dyn Fn(FolderId) -> String) -> String {
        match self {
            RuleAction::NewItemAlert(msg) => {
                let msg = msg.trim();
                format!(
                    "display {} in the New Item Alert window",
                    if msg.is_empty() { "a specific message" } else { msg }
                )
            }
            RuleAction::MoveTo(f) => format!("move it to the {} folder", folder_name(*f)),
            RuleAction::CopyTo(f) => format!("move a copy to the {} folder", folder_name(*f)),
            RuleAction::AssignCategory(c) => {
                let c = c.trim();
                format!("assign it to the {} category", if c.is_empty() { "category" } else { c })
            }
            RuleAction::Delete => "delete it".to_string(),
            RuleAction::PermanentlyDelete => "permanently delete it".to_string(),
            RuleAction::ForwardTo(l) => format!("forward it to {}", people(l)),
            RuleAction::ForwardAsAttachment(l) => format!("forward it to {} as an attachment", people(l)),
            RuleAction::RedirectTo(l) => format!("redirect it to {}", people(l)),
            RuleAction::ReplyWithTemplate(t) => {
                let t = t.trim();
                format!("reply using {}", if t.is_empty() { "a specific template" } else { t })
            }
            RuleAction::FlagForFollowUp(label, days) => {
                let label = label.trim();
                let label = if label.is_empty() { "Follow up" } else { label };
                match days {
                    0 => format!("flag message for {label} today"),
                    1 => format!("flag message for {label} in 1 day"),
                    n => format!("flag message for {label} in {n} days"),
                }
            }
            RuleAction::ClearFlag => "clear the Message Flag".to_string(),
            RuleAction::ClearCategories => "clear message's categories".to_string(),
            RuleAction::MarkImportance(i) => format!("mark it as {} importance", importance_word(*i)),
            RuleAction::Print => "print it".to_string(),
            RuleAction::PlaySound(s) => {
                let s = s.trim();
                format!("play {}", if s.is_empty() { "a sound" } else { s })
            }
            RuleAction::MarkAsRead => "mark it as read".to_string(),
            RuleAction::StopProcessingMoreRules => "stop processing more rules".to_string(),
            RuleAction::DesktopAlert => "display a Desktop Alert".to_string(),
            RuleAction::DeferDelivery(n) => match n {
                1 => "defer delivery by 1 minute".to_string(),
                n => format!("defer delivery by {n} minutes"),
            },
            RuleAction::CcTo(l) => format!("Cc the message to {}", people(l)),
            RuleAction::MarkSensitivity(x) => format!("mark it as {}", sensitivity_word(*x)),
            RuleAction::NotifyWhenRead => "notify me when it is read".to_string(),
            RuleAction::NotifyWhenDelivered => "notify me when it is delivered".to_string(),
            RuleAction::ServerReply(t) => {
                let t = t.trim();
                if t.is_empty() {
                    "have server reply using a specific message".to_string()
                } else {
                    format!("have server reply using \"{}\"", short(t, 40))
                }
            }
        }
    }
}

/// `text` cut to `max` characters with "…".
fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= max && !text.contains('\n') {
        return line.to_string();
    }
    let mut s: String = line.chars().take(max).collect();
    s.push('…');
    s
}

// ---------------------------------------------------------------------------------------------------------
// Message classes and forms

/// Form display names and the message class each stands for (MS-OXCMSG message classes; Message is the
/// default for ordinary mail).
pub const FORMS: [(&str, &str); 9] = [
    ("Message", "IPM.Note"),
    ("Meeting Request", "IPM.Schedule.Meeting.Request"),
    ("Meeting Cancellation", "IPM.Schedule.Meeting.Canceled"),
    ("Meeting Response", "IPM.Schedule.Meeting.Resp"),
    ("Signed Message", "IPM.Note.SMIME.MultipartSigned"),
    ("Encrypted Message", "IPM.Note.SMIME"),
    ("Delivery Report", "REPORT.IPM.Note.DR"),
    ("Undeliverable Report", "REPORT.IPM.Note.NDR"),
    ("Read Receipt", "REPORT.IPM.Note.IPNRN"),
];

/// Display name of a message class ("Message" for anything unknown).
pub fn form_name(class: &str) -> &'static str {
    FORMS.iter().find(|(_, c)| c.eq_ignore_ascii_case(class)).map_or("Message", |(n, _)| *n)
}

/// Most bytes of a raw message scanned for parts and headers.
const SCAN_LIMIT: usize = 1024 * 1024;

/// Lower-cased start of the raw source (or the synthesised header block when no source is kept).
fn scan_text(item: &MailItem) -> String {
    match &item.raw {
        Some(raw) => {
            let head = raw.get(..raw.len().min(SCAN_LIMIT)).unwrap_or(&[]);
            String::from_utf8_lossy(head).to_lowercase()
        }
        None => header_text(item).to_lowercase(),
    }
}

fn attachment_is(a: &AttachmentMeta, types: &[&str]) -> bool {
    let t = a.mime_type.trim().to_ascii_lowercase();
    types.iter().any(|x| t.starts_with(x))
}

/// The message class of an item, derived from its MIME structure: meeting requests/cancellations/responses
/// (an iCalendar part, RFC 5546 METHOD), S/MIME (RFC 8551), delivery and read reports (RFC 3464, RFC 8098),
/// else "IPM.Note".
pub fn message_class(item: &MailItem) -> &'static str {
    let text = scan_text(item);
    let calendar = text.contains("text/calendar")
        || item.attachments.iter().any(|a| attachment_is(a, &["text/calendar", "application/ics"]));
    if calendar {
        let method = |m: &str| text.contains(&format!("method:{m}")) || text.contains(&format!("method={m}"));
        if method("cancel") {
            return "IPM.Schedule.Meeting.Canceled";
        }
        if method("reply") || method("counter") {
            return "IPM.Schedule.Meeting.Resp";
        }
        if method("request") {
            return "IPM.Schedule.Meeting.Request";
        }
    }
    if text.contains("multipart/report") {
        if text.contains("report-type=disposition-notification")
            || text.contains("report-type=\"disposition-notification\"")
        {
            return "REPORT.IPM.Note.IPNRN";
        }
        if text.contains("delivery-status") {
            return if text.contains("action: failed") {
                "REPORT.IPM.Note.NDR"
            } else {
                "REPORT.IPM.Note.DR"
            };
        }
    }
    if text.contains("multipart/signed") {
        return "IPM.Note.SMIME.MultipartSigned";
    }
    if text.contains("application/pkcs7-mime") || text.contains("application/x-pkcs7-mime") {
        return "IPM.Note.SMIME";
    }
    "IPM.Note"
}

/// RFC 3834 automatic responses (`Auto-Submitted` other than "no"), the common `X-Autoreply` /
/// `X-Autorespond` markers, and subjects starting "Automatic reply:" / "Auto:" / "Out of Office".
pub fn is_automatic_reply(item: &MailItem) -> bool {
    let subject = item.subject.trim_start().to_lowercase();
    if ["automatic reply:", "auto:", "autoreply:", "out of office"].iter().any(|p| subject.starts_with(p)) {
        return true;
    }
    let head = header_text(item);
    let mut name = String::new();
    let mut value = String::new();
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        if !name.is_empty() {
            fields.push((std::mem::take(&mut name), std::mem::take(&mut value)));
        }
        if let Some((n, v)) = line.split_once(':') {
            name = n.trim().to_ascii_lowercase();
            value = v.trim().to_string();
        }
    }
    if !name.is_empty() {
        fields.push((name, value));
    }
    fields.iter().any(|(n, v)| {
        let v = v.trim().to_ascii_lowercase();
        match n.as_str() {
            "auto-submitted" => !v.is_empty() && v != "no",
            "x-autoreply" | "x-autorespond" => !v.is_empty() && v != "no",
            _ => false,
        }
    })
}

#[cfg(test)]
mod depth_tests {
    use super::*;
    use crate::{Body, DateTime};

    fn item(subject: &str, raw: &str) -> MailItem {
        MailItem {
            subject: subject.into(),
            from: Some(Address::new(Some("Ann"), "ann@example.org")),
            to: vec![Address::new(None, "me@example.net")],
            raw: (!raw.is_empty()).then(|| raw.as_bytes().to_vec()),
            body: Body { text: Some("hello".into()), ..Default::default() },
            received: DateTime { unix: 1_000, offset_min: 0 },
            ..Default::default()
        }
    }

    fn ctx() -> RuleContext {
        RuleContext { my_addresses: vec!["me@example.net".into()], account_address: "me@example.net".into() }
    }

    fn m(c: RuleCondition, i: &MailItem) -> bool {
        Rule { conditions: vec![c], ..Default::default() }.matches(i, &ctx())
    }

    const INVITE: &str = "From: ann@example.org\r\nSubject: Sync\r\nContent-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nx\r\n--b\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n--b--\r\n";

    #[test]
    fn message_classes() {
        assert_eq!(message_class(&item("Hi", "")), "IPM.Note");
        assert_eq!(message_class(&item("Sync", INVITE)), "IPM.Schedule.Meeting.Request");
        let cancel = INVITE.replace("REQUEST", "CANCEL");
        assert_eq!(message_class(&item("Sync", &cancel)), "IPM.Schedule.Meeting.Canceled");
        let reply = INVITE.replace("REQUEST", "REPLY");
        assert_eq!(message_class(&item("Sync", &reply)), "IPM.Schedule.Meeting.Resp");
        let ndr = "Content-Type: multipart/report; report-type=delivery-status; boundary=x\r\n\r\n--x\r\nContent-Type: message/delivery-status\r\n\r\nAction: failed\r\n--x--\r\n";
        assert_eq!(message_class(&item("Undeliverable", ndr)), "REPORT.IPM.Note.NDR");
        let dr = ndr.replace("failed", "delivered");
        assert_eq!(message_class(&item("Delivered", &dr)), "REPORT.IPM.Note.DR");
        let mdn = "Content-Type: multipart/report; report-type=disposition-notification; boundary=x\r\n\r\n";
        assert_eq!(message_class(&item("Read", mdn)), "REPORT.IPM.Note.IPNRN");
        let signed =
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=x\r\n\r\n";
        assert_eq!(message_class(&item("S", signed)), "IPM.Note.SMIME.MultipartSigned");
        assert_eq!(
            message_class(&item("E", "Content-Type: application/pkcs7-mime\r\n\r\n")),
            "IPM.Note.SMIME"
        );
        // An attached .ics without a METHOD is still ordinary mail.
        let mut with_ics = item("ics", "");
        with_ics.attachments.push(AttachmentMeta { mime_type: "text/calendar".into(), ..Default::default() });
        assert_eq!(message_class(&with_ics), "IPM.Note");
        assert_eq!(form_name("IPM.Schedule.Meeting.Request"), "Meeting Request");
        assert_eq!(form_name("whatever"), "Message");
    }

    #[test]
    fn form_and_meeting_conditions() {
        let invite = item("Sync", INVITE);
        let plain = item("Hi", "");
        assert!(m(RuleCondition::MeetingInvitationOrUpdate, &invite));
        assert!(!m(RuleCondition::MeetingInvitationOrUpdate, &plain));
        assert!(m(RuleCondition::UsesForm("meeting request".into()), &invite));
        assert!(m(RuleCondition::UsesForm("Message".into()), &plain));
        assert!(!m(RuleCondition::UsesForm(" ".into()), &plain));
        assert!(m(RuleCondition::FormType("ipm.schedule".into()), &invite));
        assert!(!m(RuleCondition::FormType("IPM.Schedule".into()), &plain));
        assert!(!m(RuleCondition::FormType(String::new()), &plain));
    }

    #[test]
    fn automatic_replies() {
        assert!(m(RuleCondition::AutomaticReply, &item("Automatic reply: Lunch", "")));
        assert!(m(RuleCondition::AutomaticReply, &item("Out of Office until Monday", "")));
        assert!(!m(RuleCondition::AutomaticReply, &item("Re: Lunch", "")));
        let auto = "From: a@example.org\r\nAuto-Submitted:\r\n auto-replied\r\nSubject: x\r\n\r\nbody";
        assert!(m(RuleCondition::AutomaticReply, &item("x", auto)));
        let no =
            "From: a@example.org\r\nAuto-Submitted: no\r\nSubject: x\r\n\r\nAuto-Submitted: auto-replied";
        assert!(!m(RuleCondition::AutomaticReply, &item("x", no)), "only the header section counts");
        assert!(m(RuleCondition::AutomaticReply, &item("x", "X-Autoreply: yes\r\n\r\n")));
    }

    #[test]
    fn environment_conditions_default_to_false_and_markers_to_true() {
        let i = item("Hi", "");
        for c in [
            RuleCondition::FromAnyRssFeed,
            RuleCondition::FromRssFeedWithText(vec!["x".into()]),
            RuleCondition::SuspectedJunk,
            RuleCondition::SenderInContacts,
        ] {
            assert!(!m(c, &i));
        }
        assert!(m(RuleCondition::OnThisComputerOnly, &i));
        assert!(m(RuleCondition::AfterSending, &i));
        let env = RuleEnv {
            folder_role: Some(FolderRole::JunkEmail),
            rss_feed: Some("Rust Blog".into()),
            contacts: vec!["ANN@example.org".into()],
        };
        for c in [
            RuleCondition::FromAnyRssFeed,
            RuleCondition::FromRssFeedWithText(vec!["blog".into()]),
            RuleCondition::SuspectedJunk,
            RuleCondition::SenderInContacts,
        ] {
            assert!(c.matches_in(&i, &ctx(), &env), "{c:?}");
        }
        assert!(!RuleCondition::FromRssFeedWithText(vec![]).matches_in(&i, &ctx(), &env));
        // The marker in exceptions never cancels a rule.
        let r = Rule { exceptions: vec![RuleCondition::AfterSending], ..Default::default() };
        assert!(r.matches(&i, &ctx()));
    }

    #[test]
    fn direction_account_and_client_only() {
        let mut r = Rule {
            conditions: vec![RuleCondition::SubjectContains(vec!["x".into()])],
            actions: vec![RuleAction::AssignCategory("Red".into())],
            ..Default::default()
        };
        assert!(!r.is_outgoing() && !r.is_client_only() && r.account().is_none());
        r.set_outgoing(true);
        r.set_outgoing(true);
        assert!(r.is_outgoing());
        assert_eq!(r.conditions.iter().filter(|c| **c == RuleCondition::AfterSending).count(), 1);
        let a = r.for_account(" work@example.com ");
        assert_eq!(a.account(), Some("work@example.com"));
        assert!(a.is_outgoing());
        let b = a.for_account("home@example.net");
        assert_eq!(b.conditions.iter().filter(|c| matches!(c, RuleCondition::ThroughAccount(_))).count(), 1);
        assert_eq!(b.account(), Some("home@example.net"));
        r.set_outgoing(false);
        assert!(!r.is_outgoing());
        r.conditions.push(RuleCondition::OnThisComputerOnly);
        assert!(r.is_client_only());
        let d = Rule { actions: vec![RuleAction::DeferDelivery(5)], ..Default::default() };
        assert!(d.is_client_only());
        assert!(Rule { actions: vec![RuleAction::PlaySound("x".into())], ..Default::default() }.has_alert());
    }

    #[test]
    fn action_scopes() {
        assert!(RuleAction::DeferDelivery(1).applies_to(true));
        assert!(!RuleAction::DeferDelivery(1).applies_to(false));
        assert!(!RuleAction::NewItemAlert(String::new()).applies_to(true));
        assert!(
            RuleAction::CopyTo(FolderId(1)).applies_to(true)
                && RuleAction::CopyTo(FolderId(1)).applies_to(false)
        );
        assert_eq!(RuleAction::CcTo(vec![]).scope(), ActionScope::Outgoing);
        assert_eq!(RuleAction::ServerReply(String::new()).scope(), ActionScope::Incoming);
        assert!(RuleCondition::SentOnlyToMe.incoming_only());
        assert!(!RuleCondition::SubjectContains(vec![]).incoming_only());
    }

    #[test]
    fn descriptions_of_new_clauses() {
        let names = |_: FolderId| "Inbox".to_string();
        let mut r = Rule {
            conditions: vec![
                RuleCondition::SubjectContains(vec!["budget".into()]),
                RuleCondition::OnThisComputerOnly,
            ],
            actions: vec![
                RuleAction::DeferDelivery(1),
                RuleAction::DeferDelivery(15),
                RuleAction::CcTo(vec!["boss@example.net".into()]),
                RuleAction::MarkSensitivity(Sensitivity::Private),
                RuleAction::NotifyWhenRead,
                RuleAction::NotifyWhenDelivered,
            ],
            exceptions: vec![
                RuleCondition::MeetingInvitationOrUpdate,
                RuleCondition::UsesForm("Meeting Request".into()),
            ],
            ..Default::default()
        };
        r.set_outgoing(true);
        assert_eq!(
            r.describe(&names),
            [
                "Apply this rule after I send the message",
                "with budget in the subject",
                "and on this computer only",
                "defer delivery by 1 minute",
                "and defer delivery by 15 minutes",
                "and Cc the message to boss@example.net",
                "and mark it as private",
                "and notify me when it is read",
                "and notify me when it is delivered",
                "except if it is a meeting invitation or update",
                "or except if uses the Meeting Request form",
            ]
        );
        let clauses: Vec<String> = [
            RuleCondition::AutomaticReply,
            RuleCondition::UsesForm(String::new()),
            RuleCondition::FormType(String::new()),
            RuleCondition::FormType("IPM.Note".into()),
            RuleCondition::FromRssFeedWithText(vec![]),
            RuleCondition::FromRssFeedWithText(vec!["Rust".into()]),
            RuleCondition::FromAnyRssFeed,
            RuleCondition::SuspectedJunk,
            RuleCondition::SenderInContacts,
        ]
        .iter()
        .map(RuleCondition::describe)
        .collect();
        assert_eq!(
            clauses,
            [
                "which is an automatic reply",
                "uses the form name form",
                "of the specific form type",
                "of the IPM.Note form type",
                "from RSS Feeds with specified text in the title",
                "from RSS Feeds with Rust in the title",
                "from any RSS Feed",
                "suspected to be junk or from Junk Senders",
                "sender is in my Contacts",
            ]
        );
        assert_eq!(
            RuleAction::ServerReply(String::new()).describe(&names),
            "have server reply using a specific message"
        );
        assert_eq!(
            RuleAction::ServerReply("Thanks, got it.\nMore".into()).describe(&names),
            "have server reply using \"Thanks, got it.…\""
        );
    }

    #[test]
    fn new_variants_roundtrip_through_json() {
        let r = Rule {
            conditions: vec![RuleCondition::AfterSending, RuleCondition::FormType("IPM.Note".into())],
            actions: vec![RuleAction::DeferDelivery(3), RuleAction::MarkSensitivity(Sensitivity::Personal)],
            exceptions: vec![RuleCondition::FromAnyRssFeed],
            ..Default::default()
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("\"AfterSending\""));
        assert_eq!(serde_json::from_str::<Rule>(&text).unwrap(), r);
    }
}
