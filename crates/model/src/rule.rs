//! Rules (`plan/outlook/rules.md`). Conditions/actions mirror the Rules Wizard lists; evaluation lives here
//! (pure), execution of actions (move, forward, alert) lives in the engine.

use serde::{Deserialize, Serialize};

use crate::{Address, FolderId, Importance, MailItem, Sensitivity};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleCondition {
    /// "from people or public group"
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
        )
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

impl Rule {
    pub fn is_client_only(&self) -> bool {
        self.actions.iter().any(RuleAction::is_client_only)
    }
    /// True when every condition matches and no exception matches. A rule with no conditions matches all.
    pub fn matches(&self, item: &MailItem, ctx: &RuleContext) -> bool {
        self.conditions.iter().all(|c| c.matches(item, ctx))
            && !self.exceptions.iter().any(|c| c.matches(item, ctx))
    }
    /// Natural-language description as in the Rules and Alerts "Rule description" box, one clause per line:
    /// "Apply this rule after the message arrives", "with <words> in the subject", "move it to the <folder>
    /// folder", "except if …". `folder_name` resolves folder ids.
    ///
    /// Second and later conditions and actions start with "and "; exceptions start with "except if " and later
    /// ones with "or except if " (clauses that already read as a "where …" phrase take just "except "/"or
    /// except ", and "which has …" becomes "if it has …", so the line reads as English).
    pub fn describe(&self, folder_name: &dyn Fn(crate::FolderId) -> String) -> Vec<String> {
        let mut lines = vec!["Apply this rule after the message arrives".to_string()];
        for (i, c) in self.conditions.iter().enumerate() {
            let clause = c.describe();
            lines.push(if i == 0 { clause } else { format!("and {clause}") });
        }
        for (i, a) in self.actions.iter().enumerate() {
            let clause = a.describe(folder_name);
            lines.push(if i == 0 { clause } else { format!("and {clause}") });
        }
        for (i, e) in self.exceptions.iter().enumerate() {
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
        match self {
            RuleCondition::From(list) => {
                item.from.iter().chain(item.sender.iter()).any(|m| any_word_in(&person_text(m), list))
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
            RuleCondition::SentTo(list) => {
                item.to.iter().chain(&item.cc).any(|m| any_word_in(&person_text(m), list))
            }
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
        }
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
            RuleAction::AssignCategory(c) => format!("assign it to the {} category", c.trim()),
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
        }
    }
}
