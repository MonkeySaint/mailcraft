//! Junk Email settings and the junk classifier (Home › Delete › Block › Junk E-mail Options…).
//!
//! [`JunkSettings`] holds the protection level (No Automatic Filtering / Low / High / Safe Lists Only), the
//! options (permanently delete suspected junk, disable links in junk, warn about suspicious domain names,
//! trust Contacts, add people I email to the Safe Senders List) and the five lists: Safe Senders, Safe
//! Recipients, Blocked Senders, Blocked Top-Level Domains and Blocked Encodings (International tab).
//!
//! [`classify`] decides whether a message is junk. Precedence: safe lists (and Contacts) → never junk; blocked
//! lists → junk at every level; then the level: Safe Lists Only → junk, Low / High → a small, transparent
//! points score ([`score`]) against the level's threshold. The signals and the phrase list are our own: no
//! copied corpora or filters.
//!
//! Everything here is pure; the engine applies verdicts (moves to Junk Email, permanent delete) and reads
//! the files for import/export. Input is hostile: entries are normalised and length/count capped, text scans
//! are bounded.

use serde::{Deserialize, Serialize};

use crate::{Address, MailItem};

/// Most entries kept in one list (import and add stop there).
pub const MAX_LIST_ENTRIES: usize = 10_000;
/// Longest list entry accepted (an RFC 5321 path is at most 256 octets; leave room).
pub const MAX_ENTRY_CHARS: usize = 320;
/// Longest stretch of a message body the scorer looks at (bytes).
pub const MAX_SCAN_BYTES: usize = 256 * 1024;

/// Points at or above which Low protection calls a message junk ("only the most obvious junk").
pub const LOW_THRESHOLD: i32 = 7;
/// Points at or above which High protection calls a message junk.
pub const HIGH_THRESHOLD: i32 = 4;

// ---------------------------------------------------------------------------------------------------------
// Settings

/// Level of junk email protection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JunkLevel {
    /// No Automatic Filtering: only blocked senders (and blocked domains/encodings) go to Junk Email.
    NoFiltering,
    /// Low: only the most obvious junk.
    #[default]
    Low,
    /// High: most junk is caught, some regular mail may be too.
    High,
    /// Safe Lists Only: only mail from the safe lists reaches the Inbox.
    SafeListsOnly,
}

impl JunkLevel {
    pub const ALL: [JunkLevel; 4] =
        [JunkLevel::NoFiltering, JunkLevel::Low, JunkLevel::High, JunkLevel::SafeListsOnly];

    /// Stable key used by commands (`noFiltering`, `low`, `high`, `safeListsOnly`).
    pub fn key(self) -> &'static str {
        match self {
            JunkLevel::NoFiltering => "noFiltering",
            JunkLevel::Low => "low",
            JunkLevel::High => "high",
            JunkLevel::SafeListsOnly => "safeListsOnly",
        }
    }
    /// Parses a key (case-insensitive) or the index 0–3.
    pub fn parse(s: &str) -> Option<JunkLevel> {
        let t = s.trim();
        if let Ok(i) = t.parse::<usize>() {
            return JunkLevel::ALL.get(i).copied();
        }
        JunkLevel::ALL.iter().copied().find(|l| l.key().eq_ignore_ascii_case(t)).or_else(|| {
            match t.to_ascii_lowercase().as_str() {
                "none" | "off" | "nofilter" | "no automatic filtering" => Some(JunkLevel::NoFiltering),
                "safelists" | "safe lists only" => Some(JunkLevel::SafeListsOnly),
                _ => None,
            }
        })
    }
    pub fn index(self) -> usize {
        JunkLevel::ALL.iter().position(|l| *l == self).unwrap_or(1)
    }
    /// Score threshold of the scoring levels (`None` for the others).
    pub fn threshold(self) -> Option<i32> {
        match self {
            JunkLevel::Low => Some(LOW_THRESHOLD),
            JunkLevel::High => Some(HIGH_THRESHOLD),
            JunkLevel::NoFiltering | JunkLevel::SafeListsOnly => None,
        }
    }
}

/// One of the five junk lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JunkList {
    SafeSenders,
    SafeRecipients,
    BlockedSenders,
    BlockedTlds,
    BlockedEncodings,
}

impl JunkList {
    pub const ALL: [JunkList; 5] = [
        JunkList::SafeSenders,
        JunkList::SafeRecipients,
        JunkList::BlockedSenders,
        JunkList::BlockedTlds,
        JunkList::BlockedEncodings,
    ];
    /// Stable key used by commands.
    pub fn key(self) -> &'static str {
        match self {
            JunkList::SafeSenders => "safeSenders",
            JunkList::SafeRecipients => "safeRecipients",
            JunkList::BlockedSenders => "blockedSenders",
            JunkList::BlockedTlds => "blockedTlds",
            JunkList::BlockedEncodings => "blockedEncodings",
        }
    }
    pub fn parse(s: &str) -> Option<JunkList> {
        let t = s.trim();
        JunkList::ALL.iter().copied().find(|l| l.key().eq_ignore_ascii_case(t))
    }
    /// Address lists hold `name@domain` or `@domain`; the others hold codes.
    pub fn is_address_list(self) -> bool {
        matches!(self, JunkList::SafeSenders | JunkList::SafeRecipients | JunkList::BlockedSenders)
    }
}

/// Junk Email Options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JunkSettings {
    pub level: JunkLevel,
    /// Permanently delete suspected junk email instead of moving it to the Junk Email folder.
    pub delete_suspected: bool,
    /// Disable links and other functionality in junk and phishing messages.
    pub disable_links: bool,
    /// Warn about suspicious domain names in email addresses.
    pub warn_suspicious_domains: bool,
    /// Also trust email from my Contacts.
    pub trust_contacts: bool,
    /// Automatically add people I email to the Safe Senders List.
    pub add_recipients_to_safe: bool,
    pub safe_senders: Vec<String>,
    pub safe_recipients: Vec<String>,
    pub blocked_senders: Vec<String>,
    /// Top-level domains without the dot (`ru`, `xyz`).
    pub blocked_tlds: Vec<String>,
    /// Lower-case charset names (`koi8-r`, `big5`).
    pub blocked_encodings: Vec<String>,
}

impl Default for JunkSettings {
    fn default() -> Self {
        JunkSettings {
            level: JunkLevel::Low,
            delete_suspected: false,
            disable_links: true,
            warn_suspicious_domains: true,
            trust_contacts: true,
            add_recipients_to_safe: false,
            safe_senders: Vec::new(),
            safe_recipients: Vec::new(),
            blocked_senders: Vec::new(),
            blocked_tlds: Vec::new(),
            blocked_encodings: Vec::new(),
        }
    }
}

impl JunkSettings {
    pub fn list(&self, l: JunkList) -> &Vec<String> {
        match l {
            JunkList::SafeSenders => &self.safe_senders,
            JunkList::SafeRecipients => &self.safe_recipients,
            JunkList::BlockedSenders => &self.blocked_senders,
            JunkList::BlockedTlds => &self.blocked_tlds,
            JunkList::BlockedEncodings => &self.blocked_encodings,
        }
    }
    pub fn list_mut(&mut self, l: JunkList) -> &mut Vec<String> {
        match l {
            JunkList::SafeSenders => &mut self.safe_senders,
            JunkList::SafeRecipients => &mut self.safe_recipients,
            JunkList::BlockedSenders => &mut self.blocked_senders,
            JunkList::BlockedTlds => &mut self.blocked_tlds,
            JunkList::BlockedEncodings => &mut self.blocked_encodings,
        }
    }
    /// Adds `raw` (normalised) to list `l`, kept sorted. `Ok(false)` when it was already there; an error when
    /// the entry is not valid for the list or the list is full.
    pub fn add(&mut self, l: JunkList, raw: &str) -> crate::Result<bool> {
        let Some(entry) = normalize_entry(l, raw) else {
            return Err(crate::ModelError::Invalid(format!("\"{}\" is not a valid entry", clip(raw, 80))));
        };
        let list = self.list_mut(l);
        match list.binary_search(&entry) {
            Ok(_) => Ok(false),
            Err(_) if list.len() >= MAX_LIST_ENTRIES => Err(crate::ModelError::NotAllowed(format!(
                "the list holds at most {MAX_LIST_ENTRIES} entries"
            ))),
            Err(pos) => {
                list.insert(pos, entry);
                Ok(true)
            }
        }
    }
    /// Removes `raw` (normalised, or verbatim) from list `l`; true when something was removed.
    pub fn remove(&mut self, l: JunkList, raw: &str) -> bool {
        let entry = normalize_entry(l, raw).unwrap_or_else(|| raw.trim().to_lowercase());
        let list = self.list_mut(l);
        let before = list.len();
        list.retain(|e| *e != entry);
        list.len() != before
    }
    /// Adds every valid line of a `.txt` list (one entry per line; blank lines and `#` comments skipped);
    /// returns how many entries were new.
    pub fn import_text(&mut self, l: JunkList, text: &str) -> usize {
        let mut added = 0usize;
        for line in text.lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match self.add(l, line) {
                Ok(true) => added = added.saturating_add(1),
                Ok(false) => {}
                Err(crate::ModelError::NotAllowed(_)) => break,
                Err(_) => {}
            }
        }
        added
    }
    /// The list as a `.txt` file: one entry per line (CRLF, as other mail clients write them).
    pub fn export_text(&self, l: JunkList) -> String {
        let mut out = String::new();
        for e in self.list(l) {
            out.push_str(e);
            out.push_str("\r\n");
        }
        out
    }
    /// Normalises, de-duplicates, sorts and caps every list (after loading or receiving settings from outside).
    pub fn sanitize(&mut self) {
        for l in JunkList::ALL {
            let mut v: Vec<String> =
                self.list(l).iter().filter_map(|e| normalize_entry(l, e)).take(MAX_LIST_ENTRIES).collect();
            v.sort();
            v.dedup();
            *self.list_mut(l) = v;
        }
    }

    /// The Safe Senders entry matching `addr`, if any.
    pub fn safe_sender_match(&self, addr: &str) -> Option<&str> {
        list_match(&self.safe_senders, addr)
    }
    /// The Blocked Senders entry matching `addr`, if any.
    pub fn blocked_sender_match(&self, addr: &str) -> Option<&str> {
        list_match(&self.blocked_senders, addr)
    }
    /// The Safe Recipients entry matching any of `recipients`, if any.
    pub fn safe_recipient_match<'a>(&'a self, recipients: &[&str]) -> Option<&'a str> {
        recipients.iter().find_map(|r| list_match(&self.safe_recipients, r))
    }
    /// The blocked top-level domain of `addr`, if it is on the list.
    pub fn blocked_tld_match(&self, addr: &str) -> Option<&str> {
        let tld = domain_of(addr)?.rsplit('.').next()?.trim_end_matches('.');
        self.blocked_tlds.iter().find(|t| t.eq_ignore_ascii_case(tld)).map(String::as_str)
    }
    /// The blocked encoding among `charsets`, if any.
    pub fn blocked_encoding_match(&self, charsets: &[String]) -> Option<&str> {
        charsets.iter().find_map(|c| {
            let c = normalize_charset(c)?;
            self.blocked_encodings.iter().find(|e| **e == c).map(String::as_str)
        })
    }
}

/// `s` cut to at most `n` chars.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Lower-case domain part of an address (`None` without one).
pub fn domain_of(addr: &str) -> Option<&str> {
    let (_, d) = addr.trim().rsplit_once('@')?;
    let d = d.trim().trim_end_matches('>').trim_end_matches('.');
    if d.is_empty() { None } else { Some(d) }
}

/// The first entry of `list` matching `addr`: a full address matches itself (case-insensitive), `@domain`
/// matches that domain and its subdomains.
fn list_match<'a>(list: &'a [String], addr: &str) -> Option<&'a str> {
    let a = addr.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase();
    if a.is_empty() {
        return None;
    }
    let dom = domain_of(&a).map(str::to_owned);
    list.iter()
        .find(|e| match e.strip_prefix('@') {
            Some(d) => dom.as_deref().is_some_and(|ad| ad == d || ad.ends_with(&format!(".{d}"))),
            None => **e == a,
        })
        .map(String::as_str)
}

/// Normalises one list entry; `None` when it is not valid for `list`.
///
/// Address lists: `name@domain` or a domain (`@example.com`, `example.com`, `*@example.com` → `@example.com`),
/// lower-case. Top-level domains: `ru`, `.ru`, `*.ru` → `ru`. Encodings: lower-case charset names.
pub fn normalize_entry(list: JunkList, raw: &str) -> Option<String> {
    let t = raw.trim().trim_matches(|c| c == '<' || c == '>' || c == '"').trim();
    if t.is_empty()
        || t.chars().count() > MAX_ENTRY_CHARS
        || t.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let t = t.to_lowercase();
    match list {
        JunkList::SafeSenders | JunkList::SafeRecipients | JunkList::BlockedSenders => {
            let t = t.strip_prefix('*').unwrap_or(&t);
            let (local, domain) = match t.rsplit_once('@') {
                Some((l, d)) => (l, d),
                None => ("", t),
            };
            if !valid_domain(domain) || local.contains('@') {
                return None;
            }
            Some(if local.is_empty() { format!("@{domain}") } else { format!("{local}@{domain}") })
        }
        JunkList::BlockedTlds => {
            let t = t.trim_start_matches('*').trim_start_matches('.');
            let ok = !t.is_empty()
                && t.len() <= 63
                && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !t.starts_with('-');
            ok.then(|| t.to_owned())
        }
        JunkList::BlockedEncodings => normalize_charset(&t),
    }
}

/// Lower-case charset name, or `None` when it has characters a charset name cannot have.
fn normalize_charset(raw: &str) -> Option<String> {
    let t = raw.trim().trim_matches('"').to_ascii_lowercase();
    let ok = !t.is_empty()
        && t.len() <= 40
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    ok.then_some(t)
}

/// A plausible domain: dot-separated labels of letters, digits, hyphens (or non-ASCII letters for IDNs).
fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.chars().count() <= 63
                && label.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        })
}

// ---------------------------------------------------------------------------------------------------------
// Suspicious domain names

/// Why the domain of `addr` looks suspicious (our wording), for the "Warn me about suspicious domain names"
/// info bar: punycode labels, non-ASCII characters (look-alike letters), a mix of scripts, or a raw IP.
pub fn suspicious_domain(addr: &str) -> Option<&'static str> {
    let d = domain_of(addr)?.to_lowercase();
    if d.split('.').any(|l| l.starts_with("xn--")) {
        return Some(
            "The sender's domain uses an encoded international name that may imitate another domain.",
        );
    }
    if !d.is_ascii() {
        return Some("The sender's domain contains characters that may imitate a familiar domain.");
    }
    let bare = d.trim_start_matches('[').trim_end_matches(']');
    if bare.split('.').count() == 4
        && bare.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return Some("The sender's address uses a numeric IP address instead of a domain name.");
    }
    if d.matches('-').count() >= 4 {
        return Some("The sender's domain name is unusually long and hyphenated.");
    }
    None
}

// ---------------------------------------------------------------------------------------------------------
// Scoring

/// One scoring signal that fired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// Stable key (`reply-to-mismatch`, `all-caps-subject`, …).
    pub key: String,
    pub points: i32,
    /// What was seen, for the transparency of the verdict.
    pub detail: String,
}

/// Our own short list of phrases typical of unsolicited bulk mail (matched case-insensitively, whole
/// phrase). Each distinct phrase scores [`PHRASE_POINTS`], at most [`PHRASE_CAP`] in total.
pub const SPAM_PHRASES: &[&str] = &[
    "act now",
    "you have won",
    "you've won",
    "claim your prize",
    "claim your reward",
    "congratulations, you",
    "100% free",
    "risk-free",
    "risk free",
    "no credit check",
    "lowest price",
    "miracle cure",
    "lose weight fast",
    "earn extra cash",
    "make money fast",
    "work from home and earn",
    "double your income",
    "cash bonus",
    "million dollars",
    "wire transfer fee",
    "inheritance fund",
    "next of kin",
    "dear beneficiary",
    "verify your account now",
    "your account will be suspended",
    "your account has been suspended",
    "confirm your bank details",
    "your bank details",
    "limited time offer",
    "this is not spam",
    "order now",
    "special promotion",
    "click here to claim",
    "once in a lifetime",
    "exclusive deal just for you",
    "cheap meds",
    "online pharmacy",
    "crypto investment opportunity",
    "guaranteed returns",
];
pub const PHRASE_POINTS: i32 = 2;
pub const PHRASE_CAP: i32 = 6;

/// What the scorer reads beyond the item itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreInput {
    /// The message carried a Message-ID (items without raw headers count as having one).
    pub has_message_id: bool,
}

/// Bounded prefix of `s` (cut on a char boundary).
fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// Links in a text: `http://`, `https://` and `www.` occurrences.
fn count_links(lower: &str) -> usize {
    lower.matches("http://").count() + lower.matches("https://").count()
}

/// Whether a link points at a bare IPv4 address (`http://192.0.2.7/…`).
fn has_ip_link(lower: &str) -> bool {
    for scheme in ["http://", "https://"] {
        for (pos, _) in lower.match_indices(scheme) {
            let rest = lower.get(pos.saturating_add(scheme.len())..).unwrap_or("");
            let host: String = rest
                .chars()
                .take_while(|c| !matches!(c, '/' | ':' | '"' | '\'' | '>' | '<' | ' ' | '?'))
                .collect();
            let parts: Vec<&str> = host.split('.').collect();
            if parts.len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
            {
                return true;
            }
        }
    }
    false
}

/// An address that appears inside a display name (`"paypal.com@service" <x@y>` style spoofing).
fn name_address(name: &str) -> Option<String> {
    name.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | '"' | '\'' | ',' | ';'))
        .find(|w| w.contains('@') && domain_of(w).is_some_and(|d| d.contains('.')))
        .map(str::to_lowercase)
}

/// The transparent junk score of `item`: every signal that fired, with its points.
pub fn score(item: &MailItem, input: &ScoreInput) -> Vec<Signal> {
    let mut out = Vec::new();
    let mut push = |key: &str, points: i32, detail: String| {
        out.push(Signal { key: key.to_owned(), points, detail });
    };
    let from: Option<&Address> = item.from.as_ref();
    let from_addr = from.map(|a| a.addr.to_lowercase()).unwrap_or_default();
    let from_domain = domain_of(&from_addr).map(str::to_owned);

    // Header signals.
    if from_addr.is_empty() {
        push("no-sender", 2, "The message has no sender address.".into());
    }
    if let (Some(fd), Some(rt)) = (from_domain.as_deref(), item.reply_to.first()) {
        let rd = domain_of(&rt.addr).map(str::to_lowercase);
        if let Some(rd) = rd
            && rd != fd
            && !rd.ends_with(&format!(".{fd}"))
            && !fd.ends_with(&format!(".{rd}"))
        {
            push("reply-to-mismatch", 2, format!("Replies go to {rd}, not the sender's domain {fd}."));
        }
    }
    if let Some(name) = from.and_then(|a| a.name.as_deref())
        && let Some(shown) = name_address(name)
        && shown != from_addr
    {
        push(
            "display-name-spoof",
            3,
            format!("The sender's name shows {shown} but the address is {from_addr}."),
        );
    }
    if suspicious_domain(&from_addr).is_some() {
        push(
            "suspicious-domain",
            2,
            format!("The sender's domain looks unusual: {}.", from_domain.unwrap_or_default()),
        );
    }
    if !input.has_message_id {
        push("no-message-id", 1, "The message has no Message-ID header.".into());
    }
    if item.sent.is_none() {
        push("no-date", 1, "The message has no Date header.".into());
    }
    if item.to.is_empty() && item.cc.is_empty() {
        push("no-recipients", 1, "You were not named in To or Cc.".into());
    }

    // Subject signals.
    let subject = item.subject.trim();
    if subject.is_empty() {
        push("empty-subject", 1, "The subject is empty.".into());
    }
    let letters: Vec<char> = subject.chars().filter(|c| c.is_alphabetic()).collect();
    let upper = letters.iter().filter(|c| c.is_uppercase()).count();
    if letters.len() >= 8 && upper.saturating_mul(10) >= letters.len().saturating_mul(8) {
        push("all-caps-subject", 2, "The subject is in capital letters.".into());
    }
    let bangs = subject.matches('!').count();
    if bangs >= 3 || subject.contains("$$") {
        push("shouting-subject", 1, "The subject has runs of !!! or $$.".into());
    }

    // Body signals (bounded scan).
    let text = item.body.text.as_deref().map(|t| head(t, MAX_SCAN_BYTES)).unwrap_or("");
    let html = item.body.html.as_deref().map(|t| head(t, MAX_SCAN_BYTES)).unwrap_or("");
    let text_lower =
        if text.is_empty() && html.is_empty() { item.preview.to_lowercase() } else { text.to_lowercase() };
    let html_lower = html.to_lowercase();
    let links = count_links(&text_lower).max(count_links(&html_lower));
    if links >= 25 {
        push("many-links", 3, format!("The message has {links} links."));
    } else if links >= 12 {
        push("many-links", 2, format!("The message has {links} links."));
    }
    if has_ip_link(&text_lower) || has_ip_link(&html_lower) {
        push("ip-link", 3, "A link points at a numeric IP address.".into());
    }
    let subject_lower = subject.to_lowercase();
    let mut phrase_points = 0;
    let mut seen = Vec::new();
    for p in SPAM_PHRASES {
        if phrase_points >= PHRASE_CAP {
            break;
        }
        if subject_lower.contains(p) || text_lower.contains(p) || html_lower.contains(p) {
            phrase_points += PHRASE_POINTS;
            seen.push(*p);
        }
    }
    if !seen.is_empty() {
        push(
            "spam-phrases",
            phrase_points.min(PHRASE_CAP),
            format!("Typical junk phrases: {}.", seen.join(", ")),
        );
    }
    out
}

/// Sum of the signals' points.
pub fn total(signals: &[Signal]) -> i32 {
    signals.iter().fold(0i32, |acc, s| acc.saturating_add(s.points))
}

// ---------------------------------------------------------------------------------------------------------
// Verdict

/// Why [`classify`] decided what it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JunkReason {
    /// Sender on the Safe Senders List (the entry).
    SafeSender(String),
    /// A recipient on the Safe Recipients List (the entry).
    SafeRecipient(String),
    /// Sender is in Contacts (and "Also trust email from my Contacts" is on).
    Contact,
    /// Sender on the Blocked Senders List (the entry).
    BlockedSender(String),
    /// Sender's top-level domain is blocked.
    BlockedTld(String),
    /// The message's encoding is blocked.
    BlockedEncoding(String),
    /// Safe Lists Only and the sender is on no safe list.
    NotOnSafeLists,
    /// The score reached the level's threshold.
    Score,
    /// Nothing marked it as junk (score below threshold, or No Automatic Filtering).
    Clean,
}

impl JunkReason {
    pub fn key(&self) -> &'static str {
        match self {
            JunkReason::SafeSender(_) => "safeSender",
            JunkReason::SafeRecipient(_) => "safeRecipient",
            JunkReason::Contact => "contact",
            JunkReason::BlockedSender(_) => "blockedSender",
            JunkReason::BlockedTld(_) => "blockedTld",
            JunkReason::BlockedEncoding(_) => "blockedEncoding",
            JunkReason::NotOnSafeLists => "notOnSafeLists",
            JunkReason::Score => "score",
            JunkReason::Clean => "clean",
        }
    }
    /// The list entry involved, when there is one.
    pub fn entry(&self) -> Option<&str> {
        match self {
            JunkReason::SafeSender(e)
            | JunkReason::SafeRecipient(e)
            | JunkReason::BlockedSender(e)
            | JunkReason::BlockedTld(e)
            | JunkReason::BlockedEncoding(e) => Some(e),
            _ => None,
        }
    }
}

/// The classifier's decision for one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub junk: bool,
    pub reason: JunkReason,
    /// Total points (0 when scoring did not run).
    pub score: i32,
    pub signals: Vec<Signal>,
}

/// What [`classify`] needs besides the item and the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JunkContext {
    /// Lower-case addresses of the user's contacts.
    pub contacts: Vec<String>,
    /// Charsets the message declares (Content-Type of the message and its parts).
    pub charsets: Vec<String>,
    /// Mailing-list address (List-Post / List-Id), checked against Safe Recipients.
    pub list_address: Option<String>,
    pub score: ScoreInput,
}

/// Decides whether `item` is junk under `settings`.
pub fn classify(settings: &JunkSettings, item: &MailItem, ctx: &JunkContext) -> Verdict {
    let verdict = |junk, reason| Verdict { junk, reason, score: 0, signals: Vec::new() };
    let from = item.from.as_ref().map(|a| a.addr.trim().to_lowercase()).unwrap_or_default();

    // Safe lists win over everything else.
    if !from.is_empty()
        && let Some(e) = settings.safe_sender_match(&from)
    {
        return verdict(false, JunkReason::SafeSender(e.to_owned()));
    }
    let recipients: Vec<&str> = item
        .to
        .iter()
        .chain(item.cc.iter())
        .map(|a| a.addr.as_str())
        .chain(ctx.list_address.as_deref())
        .collect();
    if let Some(e) = settings.safe_recipient_match(&recipients) {
        return verdict(false, JunkReason::SafeRecipient(e.to_owned()));
    }
    if settings.trust_contacts && !from.is_empty() && ctx.contacts.contains(&from) {
        return verdict(false, JunkReason::Contact);
    }

    // Blocked lists apply at every level.
    if !from.is_empty() {
        if let Some(e) = settings.blocked_sender_match(&from) {
            return verdict(true, JunkReason::BlockedSender(e.to_owned()));
        }
        if let Some(t) = settings.blocked_tld_match(&from) {
            return verdict(true, JunkReason::BlockedTld(t.to_owned()));
        }
    }
    if let Some(e) = settings.blocked_encoding_match(&ctx.charsets) {
        return verdict(true, JunkReason::BlockedEncoding(e.to_owned()));
    }

    match settings.level {
        JunkLevel::NoFiltering => verdict(false, JunkReason::Clean),
        JunkLevel::SafeListsOnly => verdict(true, JunkReason::NotOnSafeLists),
        JunkLevel::Low | JunkLevel::High => {
            let signals = score(item, &ctx.score);
            let points = total(&signals);
            let threshold = settings.level.threshold().unwrap_or(LOW_THRESHOLD);
            let junk = points >= threshold;
            Verdict {
                junk,
                reason: if junk { JunkReason::Score } else { JunkReason::Clean },
                score: points,
                signals,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Choices offered on the International tab

/// Common country-code and generic top-level domains offered on the Blocked Top-Level Domain List (any other
/// can be typed).
pub const COMMON_TLDS: &[&str] = &[
    "ae", "ar", "at", "au", "be", "bg", "biz", "br", "by", "ca", "ch", "cl", "cn", "co", "cz", "de", "dk",
    "ee", "eg", "es", "fi", "fr", "gr", "hk", "hr", "hu", "id", "ie", "il", "in", "info", "ir", "is", "it",
    "jp", "kp", "kr", "kz", "lt", "lv", "ma", "mx", "my", "ng", "nl", "no", "nz", "pe", "ph", "pk", "pl",
    "pt", "ro", "rs", "ru", "sa", "se", "sg", "sk", "su", "th", "tk", "top", "tr", "tw", "ua", "uk", "us",
    "uz", "ve", "vn", "xyz", "za",
];

/// Encodings offered on the Blocked Encodings List: (charset, description). Any other charset can be typed.
pub const COMMON_ENCODINGS: &[(&str, &str)] = &[
    ("iso-8859-6", "Arabic (ISO)"),
    ("windows-1256", "Arabic (Windows)"),
    ("iso-8859-4", "Baltic (ISO)"),
    ("windows-1257", "Baltic (Windows)"),
    ("iso-8859-2", "Central European (ISO)"),
    ("windows-1250", "Central European (Windows)"),
    ("gb2312", "Chinese Simplified (GB2312)"),
    ("gbk", "Chinese Simplified (GBK)"),
    ("gb18030", "Chinese Simplified (GB18030)"),
    ("hz-gb-2312", "Chinese Simplified (HZ)"),
    ("big5", "Chinese Traditional (Big5)"),
    ("iso-8859-5", "Cyrillic (ISO)"),
    ("koi8-r", "Cyrillic (KOI8-R)"),
    ("koi8-u", "Cyrillic (KOI8-U)"),
    ("windows-1251", "Cyrillic (Windows)"),
    ("iso-8859-7", "Greek (ISO)"),
    ("windows-1253", "Greek (Windows)"),
    ("iso-8859-8", "Hebrew (ISO)"),
    ("windows-1255", "Hebrew (Windows)"),
    ("euc-jp", "Japanese (EUC)"),
    ("iso-2022-jp", "Japanese (JIS)"),
    ("shift_jis", "Japanese (Shift-JIS)"),
    ("euc-kr", "Korean (EUC)"),
    ("iso-2022-kr", "Korean (ISO)"),
    ("ks_c_5601-1987", "Korean"),
    ("tis-620", "Thai (TIS-620)"),
    ("windows-874", "Thai (Windows)"),
    ("iso-8859-9", "Turkish (ISO)"),
    ("windows-1254", "Turkish (Windows)"),
    ("windows-1258", "Vietnamese (Windows)"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Address, Body, BodyFormat, DateTime};

    fn item(from: &str, subject: &str, text: &str) -> MailItem {
        MailItem {
            from: Some(Address::new(Some("Sender"), from)),
            to: vec![Address::new(None, "me@example.com")],
            subject: subject.into(),
            sent: Some(DateTime::default()),
            body: Body { format: BodyFormat::PlainText, text: Some(text.into()), html: None },
            ..Default::default()
        }
    }

    fn ctx() -> JunkContext {
        JunkContext { score: ScoreInput { has_message_id: true }, ..Default::default() }
    }

    #[test]
    fn levels_parse_and_round_trip() {
        for l in JunkLevel::ALL {
            assert_eq!(JunkLevel::parse(l.key()), Some(l));
            assert_eq!(JunkLevel::parse(&l.index().to_string()), Some(l));
        }
        assert_eq!(JunkLevel::parse("bogus"), None);
        assert_eq!(JunkLevel::parse("9"), None);
        assert_eq!(JunkLevel::default(), JunkLevel::Low);
        for l in JunkList::ALL {
            assert_eq!(JunkList::parse(l.key()), Some(l));
        }
    }

    #[test]
    fn entries_are_normalised_and_validated() {
        fn n(l: JunkList, s: &str) -> Option<String> {
            normalize_entry(l, s)
        }
        assert_eq!(n(JunkList::SafeSenders, " Alice@Example.COM "), Some("alice@example.com".into()));
        assert_eq!(n(JunkList::SafeSenders, "<bob@example.org>"), Some("bob@example.org".into()));
        assert_eq!(n(JunkList::BlockedSenders, "example.net"), Some("@example.net".into()));
        assert_eq!(n(JunkList::BlockedSenders, "*@Example.net"), Some("@example.net".into()));
        assert_eq!(n(JunkList::BlockedSenders, "@example.net"), Some("@example.net".into()));
        assert_eq!(n(JunkList::BlockedSenders, "two words@example.net"), None);
        assert_eq!(n(JunkList::BlockedSenders, "a@b@c"), None);
        assert_eq!(n(JunkList::BlockedSenders, "x@"), None);
        assert_eq!(n(JunkList::BlockedSenders, "x@bad..domain"), None);
        assert_eq!(n(JunkList::BlockedSenders, ""), None);
        assert_eq!(n(JunkList::BlockedSenders, &"a".repeat(400)), None);
        assert_eq!(n(JunkList::BlockedTlds, "*.RU"), Some("ru".into()));
        assert_eq!(n(JunkList::BlockedTlds, ".xyz"), Some("xyz".into()));
        assert_eq!(n(JunkList::BlockedTlds, "a.b"), None);
        assert_eq!(n(JunkList::BlockedEncodings, "KOI8-R"), Some("koi8-r".into()));
        assert_eq!(n(JunkList::BlockedEncodings, "ko i8"), None);
        assert_eq!(n(JunkList::BlockedEncodings, "\0"), None);
    }

    #[test]
    fn add_remove_import_export() {
        let mut s = JunkSettings::default();
        assert!(s.add(JunkList::SafeSenders, "Zed@example.com").unwrap());
        assert!(s.add(JunkList::SafeSenders, "amy@example.com").unwrap());
        assert!(!s.add(JunkList::SafeSenders, "AMY@example.com").unwrap());
        assert!(s.add(JunkList::SafeSenders, "not an address").is_err());
        assert_eq!(s.safe_senders, vec!["amy@example.com", "zed@example.com"]);
        assert!(s.remove(JunkList::SafeSenders, "Amy@Example.com"));
        assert!(!s.remove(JunkList::SafeSenders, "amy@example.com"));
        let text = "\u{feff}# exported\r\none@example.org\r\n\r\n@example.net\nbad entry\none@example.org\n";
        assert_eq!(s.import_text(JunkList::BlockedSenders, text), 2);
        assert_eq!(s.export_text(JunkList::BlockedSenders), "@example.net\r\none@example.org\r\n");
        let mut again = JunkSettings::default();
        assert_eq!(again.import_text(JunkList::BlockedSenders, &s.export_text(JunkList::BlockedSenders)), 2);
        assert_eq!(again.blocked_senders, s.blocked_senders);
    }

    #[test]
    fn list_is_capped() {
        let mut s = JunkSettings::default();
        let text: String = (0..MAX_LIST_ENTRIES + 50).map(|i| format!("u{i}@example.com\n")).collect();
        assert_eq!(s.import_text(JunkList::SafeSenders, &text), MAX_LIST_ENTRIES);
        assert!(s.add(JunkList::SafeSenders, "late@example.com").is_err());
    }

    #[test]
    fn sanitize_cleans_hostile_lists() {
        let mut s = JunkSettings {
            blocked_senders: vec!["B@Example.com".into(), "b@example.com".into(), "  ".into(), "x y".into()],
            blocked_tlds: vec![".RU".into(), "!!".into()],
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.blocked_senders, vec!["b@example.com"]);
        assert_eq!(s.blocked_tlds, vec!["ru"]);
    }

    #[test]
    fn domain_entries_match_subdomains_only_on_label_boundary() {
        let list = vec!["@example.com".to_owned(), "solo@example.org".to_owned()];
        assert_eq!(list_match(&list, "a@example.com"), Some("@example.com"));
        assert_eq!(list_match(&list, "a@mail.example.com"), Some("@example.com"));
        assert_eq!(list_match(&list, "a@badexample.com"), None);
        assert_eq!(list_match(&list, "SOLO@example.org"), Some("solo@example.org"));
        assert_eq!(list_match(&list, "other@example.org"), None);
        assert_eq!(list_match(&list, ""), None);
    }

    #[test]
    fn safe_lists_beat_blocked_lists() {
        let mut s = JunkSettings::default();
        s.add(JunkList::BlockedSenders, "@example.net").unwrap();
        s.add(JunkList::SafeSenders, "friend@example.net").unwrap();
        let v = classify(&s, &item("friend@example.net", "Hi", "hello"), &ctx());
        assert!(!v.junk);
        assert_eq!(v.reason, JunkReason::SafeSender("friend@example.net".into()));
        let v = classify(&s, &item("stranger@example.net", "Hi", "hello"), &ctx());
        assert!(v.junk);
        assert_eq!(v.reason, JunkReason::BlockedSender("@example.net".into()));
    }

    #[test]
    fn blocked_lists_apply_with_no_automatic_filtering() {
        let mut s = JunkSettings { level: JunkLevel::NoFiltering, ..Default::default() };
        let spam = item("x@example.ru", "YOU HAVE WON A PRIZE!!!", "claim your prize, act now, 100% free");
        assert!(!classify(&s, &spam, &ctx()).junk);
        s.add(JunkList::BlockedTlds, "ru").unwrap();
        assert_eq!(classify(&s, &spam, &ctx()).reason, JunkReason::BlockedTld("ru".into()));
        s.blocked_tlds.clear();
        s.add(JunkList::BlockedEncodings, "koi8-r").unwrap();
        let c = JunkContext { charsets: vec!["KOI8-R".into()], ..ctx() };
        assert_eq!(classify(&s, &spam, &c).reason, JunkReason::BlockedEncoding("koi8-r".into()));
    }

    #[test]
    fn safe_lists_only_and_safe_recipients_and_contacts() {
        let mut s = JunkSettings { level: JunkLevel::SafeListsOnly, ..Default::default() };
        let m = item("colleague@example.com", "Lunch", "See you at noon");
        assert_eq!(classify(&s, &m, &ctx()).reason, JunkReason::NotOnSafeLists);
        let c = JunkContext { contacts: vec!["colleague@example.com".into()], ..ctx() };
        assert_eq!(classify(&s, &m, &c).reason, JunkReason::Contact);
        s.trust_contacts = false;
        assert!(classify(&s, &m, &c).junk);
        s.add(JunkList::SafeRecipients, "team@lists.example.org").unwrap();
        let c = JunkContext { list_address: Some("team@lists.example.org".into()), ..ctx() };
        assert_eq!(classify(&s, &m, &c).reason, JunkReason::SafeRecipient("team@lists.example.org".into()));
        let mut to_list = m.clone();
        to_list.to.push(Address::new(None, "Team@Lists.Example.org"));
        assert!(!classify(&s, &to_list, &ctx()).junk);
    }

    #[test]
    fn scoring_low_and_high() {
        let low = JunkSettings::default();
        let high = JunkSettings { level: JunkLevel::High, ..Default::default() };
        let normal = item("alice@example.com", "Project kickoff", "Let's meet on Tuesday at 10.");
        for s in [&low, &high] {
            let v = classify(s, &normal, &ctx());
            assert!(!v.junk, "{v:?}");
            assert_eq!(v.score, 0);
        }
        // Moderately spammy: caught by High only.
        let mut mild = item("deals@example.net", "Limited time offer", "Order now while stocks last.");
        mild.reply_to = vec![Address::new(None, "replies@example.org")];
        let lv = classify(&low, &mild, &ctx());
        let hv = classify(&high, &mild, &ctx());
        assert!(!lv.junk && hv.junk, "low={lv:?} high={hv:?}");
        assert_eq!(hv.reason, JunkReason::Score);
        assert!(hv.signals.iter().any(|s| s.key == "reply-to-mismatch"));
        // Obvious junk: caught by Low too.
        let obvious = item(
            "winner@example.net",
            "YOU HAVE WON A PRIZE!!!",
            "Claim your prize: act now, 100% free. http://192.0.2.7/claim",
        );
        let v = classify(&low, &obvious, &ctx());
        assert!(v.junk, "{v:?}");
        assert!(v.score >= LOW_THRESHOLD);
        let keys: Vec<&str> = v.signals.iter().map(|s| s.key.as_str()).collect();
        assert!(
            keys.contains(&"all-caps-subject") && keys.contains(&"ip-link") && keys.contains(&"spam-phrases")
        );
        assert_eq!(v.score, total(&v.signals));
    }

    #[test]
    fn header_signals() {
        let mut m = item("billing@example.net", "Invoice", "Please see attached.");
        m.from = Some(Address::new(Some("service@bank.example.com"), "billing@example.net"));
        m.to.clear();
        m.sent = None;
        let sig = score(&m, &ScoreInput { has_message_id: false });
        let keys: Vec<&str> = sig.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"display-name-spoof"), "{keys:?}");
        assert!(
            keys.contains(&"no-message-id") && keys.contains(&"no-date") && keys.contains(&"no-recipients")
        );
        let links: String = (0..30).map(|i| format!("https://example.com/{i} ")).collect();
        let sig = score(&item("a@example.com", "News", &links), &ScoreInput { has_message_id: true });
        assert!(sig.iter().any(|s| s.key == "many-links" && s.points == 3));
        let mut empty = item("", "", "");
        empty.from = None;
        let sig = score(&empty, &ScoreInput::default());
        assert!(sig.iter().any(|s| s.key == "no-sender"));
    }

    #[test]
    fn phrase_points_are_capped() {
        let body = SPAM_PHRASES.join(" ");
        let sig = score(&item("a@example.com", "x", &body), &ScoreInput { has_message_id: true });
        let p = sig.iter().find(|s| s.key == "spam-phrases").unwrap();
        assert_eq!(p.points, PHRASE_CAP);
    }

    #[test]
    fn suspicious_domains() {
        assert!(suspicious_domain("a@xn--exmple-cua.com").is_some());
        assert!(suspicious_domain("a@exаmple.com").is_some()); // Cyrillic а
        assert!(suspicious_domain("a@[192.0.2.1]").is_some());
        assert!(suspicious_domain("a@secure-login-verify-account-update.example.com").is_some());
        assert!(suspicious_domain("a@example.com").is_none());
        assert!(suspicious_domain("no-at-sign").is_none());
    }

    #[test]
    fn hostile_text_never_panics() {
        let s = JunkSettings { level: JunkLevel::High, ..Default::default() };
        let weird = "é\u{1F600}\0http://".repeat(100_000);
        let mut m = item("\u{1F600}@\u{1F600}", &weird, &weird);
        m.body.html = Some(weird.clone());
        m.reply_to = vec![Address::new(Some(&weird), "@")];
        let _ = classify(&s, &m, &ctx());
        assert_eq!(head("éé", 1), "");
        assert_eq!(head("éé", 3), "é");
        assert!(!has_ip_link("http://"));
        assert!(has_ip_link("see http://10.0.0.1:8080/x"));
        assert!(!has_ip_link("http://1.2.3.4.example.com/"));
    }

    #[test]
    fn settings_deserialize_from_partial_json() {
        let s: JunkSettings = serde_json::from_str(r#"{"level":"high","blockedTlds":["ru"]}"#).unwrap();
        assert_eq!(s.level, JunkLevel::High);
        assert!(s.disable_links && s.trust_contacts);
        assert_eq!(s.blocked_tlds, vec!["ru"]);
        let back: JunkSettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
