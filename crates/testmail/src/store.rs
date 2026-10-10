//! Shared in-memory mail state: users, mailboxes, messages, UIDs, flags, mod-sequences and SMTP log.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::date::DateTime;
use crate::{Error, Result};

/// Mailbox hierarchy delimiter.
pub const DELIM: char = '/';
/// Caps against runaway tests or hostile clients.
const MAX_MAILBOXES: usize = 1000;
const MAX_MESSAGES: usize = 200_000;
const MAX_FLAGS: usize = 64;
const MAX_FLAG_LEN: usize = 128;

/// RFC 6154 special-use attribute of a mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialUse {
    All,
    Archive,
    Drafts,
    Flagged,
    Junk,
    Sent,
    Trash,
}

impl SpecialUse {
    pub fn attribute(self) -> &'static str {
        match self {
            SpecialUse::All => "\\All",
            SpecialUse::Archive => "\\Archive",
            SpecialUse::Drafts => "\\Drafts",
            SpecialUse::Flagged => "\\Flagged",
            SpecialUse::Junk => "\\Junk",
            SpecialUse::Sent => "\\Sent",
            SpecialUse::Trash => "\\Trash",
        }
    }

    pub fn from_attribute(s: &str) -> Option<SpecialUse> {
        [
            SpecialUse::All,
            SpecialUse::Archive,
            SpecialUse::Drafts,
            SpecialUse::Flagged,
            SpecialUse::Junk,
            SpecialUse::Sent,
            SpecialUse::Trash,
        ]
        .into_iter()
        .find(|u| u.attribute().eq_ignore_ascii_case(s))
    }
}

/// A message as stored (API snapshot).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internal_date: DateTime,
    pub modseq: u64,
    pub raw: Vec<u8>,
}

/// One message accepted by the SMTP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    /// Authenticated user, if any.
    pub auth_user: Option<String>,
    /// Reverse path (MAIL FROM), empty for the null sender.
    pub mail_from: String,
    /// Accepted forward paths (RCPT TO).
    pub rcpt_to: Vec<String>,
    /// Message as received (after dot-unstuffing, before trace fields).
    pub data: Vec<u8>,
    /// Recipients that were local users and got the message in their INBOX.
    pub delivered_to: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internal_date: DateTime,
    pub raw: Arc<[u8]>,
    pub modseq: u64,
}

impl Msg {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
    }
}

#[derive(Clone, Debug)]
pub struct Mailbox {
    pub name: String,
    pub special: Option<SpecialUse>,
    pub subscribed: bool,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: u64,
    pub messages: Vec<Msg>,
}

impl Mailbox {
    pub fn find(&self, uid: u32) -> Option<&Msg> {
        self.messages.binary_search_by_key(&uid, |m| m.uid).ok().and_then(|i| self.messages.get(i))
    }

    pub fn find_mut(&mut self, uid: u32) -> Option<&mut Msg> {
        match self.messages.binary_search_by_key(&uid, |m| m.uid) {
            Ok(i) => self.messages.get_mut(i),
            Err(_) => None,
        }
    }

    pub fn unseen(&self) -> usize {
        self.messages.iter().filter(|m| !m.has_flag("\\Seen")).count()
    }

    pub fn size(&self) -> u64 {
        self.messages.iter().map(|m| m.raw.len() as u64).sum()
    }

    pub fn next_modseq(&mut self) -> u64 {
        self.highest_modseq = self.highest_modseq.saturating_add(1);
        self.highest_modseq
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub address: String,
    pub password: String,
    pub mailboxes: Vec<Mailbox>,
}

impl User {
    pub fn mailbox(&self, name: &str) -> Option<&Mailbox> {
        let name = canonical_name(name);
        self.mailboxes.iter().find(|m| m.name == name)
    }

    pub fn mailbox_mut(&mut self, name: &str) -> Option<&mut Mailbox> {
        let name = canonical_name(name);
        self.mailboxes.iter_mut().find(|m| m.name == name)
    }

    pub fn has_children(&self, name: &str) -> bool {
        let prefix = format!("{name}{DELIM}");
        self.mailboxes.iter().any(|m| m.name.starts_with(&prefix))
    }
}

/// `INBOX` is case-insensitive (also as the first hierarchy level); other names are case-sensitive.
pub fn canonical_name(name: &str) -> String {
    let name = name.trim_end_matches(DELIM);
    match name.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("INBOX") => {
            let rest = name.get(5..).unwrap_or("");
            if rest.is_empty() || rest.starts_with(DELIM) {
                return format!("INBOX{rest}");
            }
            name.to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Normalises a flag: system flags get their canonical spelling; keywords are kept.
pub fn canonical_flag(f: &str) -> Option<String> {
    const SYSTEM: [&str; 6] = ["\\Seen", "\\Answered", "\\Flagged", "\\Deleted", "\\Draft", "\\Recent"];
    if f.is_empty() || f.len() > MAX_FLAG_LEN {
        return None;
    }
    if let Some(s) = SYSTEM.iter().find(|s| s.eq_ignore_ascii_case(f)) {
        return Some((*s).to_owned());
    }
    if f.starts_with('\\') {
        return None; // unknown system flag
    }
    if f.bytes().any(|b| b <= b' ' || b >= 0x7f || b"(){%*\"\\]".contains(&b)) {
        return None;
    }
    Some(f.to_owned())
}

/// Adds `add` to `flags` (case-insensitive set semantics). Returns whether anything changed.
pub fn add_flags(flags: &mut Vec<String>, add: &[String]) -> bool {
    let mut changed = false;
    for f in add {
        if f == "\\Recent" {
            continue;
        }
        if !flags.iter().any(|x| x.eq_ignore_ascii_case(f)) && flags.len() < MAX_FLAGS {
            flags.push(f.clone());
            changed = true;
        }
    }
    changed
}

pub fn remove_flags(flags: &mut Vec<String>, rem: &[String]) -> bool {
    let before = flags.len();
    flags.retain(|x| !rem.iter().any(|r| r.eq_ignore_ascii_case(x)));
    before != flags.len()
}

pub struct Store {
    users: BTreeMap<String, User>,
    next_uidvalidity: u32,
    pub submissions: Vec<Submission>,
}

/// The store plus a change counter (for IDLE).
pub struct Shared {
    inner: Mutex<Store>,
    generation: AtomicU64,
}

impl Shared {
    pub fn new() -> Shared {
        // Seed UIDVALIDITY from the clock so restarts of the manual server look like new mailboxes to clients.
        let seed = (DateTime::now().unix & 0x3fff_ffff) as u32;
        Shared { inner: Mutex::new(Store::new(seed)), generation: AtomicU64::new(0) }
    }

    pub fn lock(&self) -> MutexGuard<'_, Store> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a change; IDLE sessions poll the generation and report it.
    pub fn notify(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

impl Store {
    pub fn new(uidvalidity_seed: u32) -> Store {
        Store { users: BTreeMap::new(), next_uidvalidity: uidvalidity_seed.max(1), submissions: Vec::new() }
    }

    fn key(address: &str) -> String {
        address.trim().to_ascii_lowercase()
    }

    pub fn user(&self, address: &str) -> Option<&User> {
        self.users.get(&Self::key(address))
    }

    pub fn user_mut(&mut self, address: &str) -> Option<&mut User> {
        self.users.get_mut(&Self::key(address))
    }

    fn user_or_err(&mut self, address: &str) -> Result<&mut User> {
        self.users.get_mut(&Self::key(address)).ok_or_else(|| Error::UnknownUser(address.to_owned()))
    }

    pub fn check_password(&self, address: &str, password: &str) -> Option<String> {
        self.user(address).filter(|u| u.password == password).map(|u| u.address.clone())
    }

    fn new_uidvalidity(&mut self) -> u32 {
        let v = self.next_uidvalidity;
        self.next_uidvalidity = self.next_uidvalidity.checked_add(1).unwrap_or(1);
        v
    }

    fn new_mailbox(&mut self, name: String, special: Option<SpecialUse>) -> Mailbox {
        Mailbox {
            name,
            special,
            subscribed: true,
            uidvalidity: self.new_uidvalidity(),
            uidnext: 1,
            highest_modseq: 1,
            messages: Vec::new(),
        }
    }

    pub fn add_user(&mut self, address: &str, password: &str) -> Result<()> {
        let addr = address.trim();
        if addr.is_empty() || !addr.contains('@') || addr.bytes().any(|b| b <= b' ' || b == b'"') {
            return Err(Error::Invalid(format!("user address `{address}`")));
        }
        if let Some(u) = self.user_mut(addr) {
            u.password = password.to_owned();
            return Ok(());
        }
        let mut mailboxes = Vec::new();
        for (name, special) in [
            ("INBOX", None),
            ("Drafts", Some(SpecialUse::Drafts)),
            ("Sent", Some(SpecialUse::Sent)),
            ("Trash", Some(SpecialUse::Trash)),
            ("Junk", Some(SpecialUse::Junk)),
            ("Archive", Some(SpecialUse::Archive)),
        ] {
            let mb = self.new_mailbox(name.to_owned(), special);
            mailboxes.push(mb);
        }
        self.users.insert(
            Self::key(addr),
            User { address: addr.to_owned(), password: password.to_owned(), mailboxes },
        );
        Ok(())
    }

    pub fn create_mailbox(&mut self, user: &str, name: &str, special: Option<SpecialUse>) -> Result<()> {
        let name = canonical_name(name);
        validate_mailbox_name(&name)?;
        let u = self.user_or_err(user)?;
        if u.mailbox(&name).is_some() {
            return Err(Error::MailboxExists(name));
        }
        if u.mailboxes.len() >= MAX_MAILBOXES {
            return Err(Error::Invalid("too many mailboxes".to_owned()));
        }
        // Create missing superiors first.
        let mut wanted = Vec::new();
        let mut acc = String::new();
        for (i, part) in name.split(DELIM).enumerate() {
            if i > 0 {
                acc.push(DELIM);
            }
            acc.push_str(part);
            wanted.push(acc.clone());
        }
        let missing: Vec<String> = wanted.into_iter().filter(|n| u.mailbox(n).is_none()).collect();
        for n in missing {
            let sp = if n == name { special } else { None };
            let mb = self.new_mailbox(n, sp);
            self.user_or_err(user)?.mailboxes.push(mb);
        }
        Ok(())
    }

    pub fn delete_mailbox(&mut self, user: &str, name: &str) -> Result<()> {
        let name = canonical_name(name);
        if name == "INBOX" {
            return Err(Error::Invalid("INBOX cannot be deleted".to_owned()));
        }
        let u = self.user_or_err(user)?;
        let before = u.mailboxes.len();
        u.mailboxes.retain(|m| m.name != name);
        if u.mailboxes.len() == before {
            return Err(Error::UnknownMailbox(name));
        }
        Ok(())
    }

    pub fn rename_mailbox(&mut self, user: &str, from: &str, to: &str) -> Result<()> {
        let from = canonical_name(from);
        let to = canonical_name(to);
        validate_mailbox_name(&to)?;
        {
            let u = self.user_or_err(user)?;
            if u.mailbox(&from).is_none() {
                return Err(Error::UnknownMailbox(from));
            }
            if u.mailbox(&to).is_some() {
                return Err(Error::MailboxExists(to));
            }
            if to.starts_with(&format!("{from}{DELIM}")) {
                return Err(Error::Invalid("cannot rename a mailbox into itself".to_owned()));
            }
        }
        if from == "INBOX" {
            // RFC 3501 6.3.5: move all messages to the new mailbox, leave INBOX empty.
            self.create_mailbox(user, &to, None)?;
            let u = self.user_or_err(user)?;
            let msgs = u.mailbox_mut("INBOX").map(|m| std::mem::take(&mut m.messages)).unwrap_or_default();
            if let Some(m) = u.mailbox_mut("INBOX") {
                m.next_modseq();
            }
            if let Some(dst) = u.mailbox_mut(&to) {
                for mut msg in msgs {
                    msg.uid = dst.uidnext;
                    dst.uidnext = dst.uidnext.saturating_add(1);
                    msg.modseq = dst.next_modseq();
                    dst.messages.push(msg);
                }
            }
            return Ok(());
        }
        // Parents of the new name.
        if let Some((parent, _)) = to.rsplit_once(DELIM)
            && self.user(user).and_then(|u| u.mailbox(parent)).is_none()
        {
            self.create_mailbox(user, parent, None)?;
        }
        let prefix = format!("{from}{DELIM}");
        let mut new_vals = Vec::new();
        let u = self.user_or_err(user)?;
        for m in &mut u.mailboxes {
            let new_name = if m.name == from {
                Some(to.clone())
            } else {
                m.name.strip_prefix(&prefix).map(|rest| format!("{to}{DELIM}{rest}"))
            };
            if let Some(n) = new_name {
                m.name = n;
                new_vals.push(m.name.clone());
            }
        }
        // A renamed mailbox is a different mailbox to clients that cached the old name: new UIDVALIDITY.
        for n in new_vals {
            let v = self.new_uidvalidity();
            if let Some(m) = self.user_or_err(user)?.mailbox_mut(&n) {
                m.uidvalidity = v;
            }
        }
        Ok(())
    }

    /// Appends a message. Returns (uidvalidity, uid).
    pub fn append(
        &mut self,
        user: &str,
        mailbox: &str,
        raw: &[u8],
        flags: &[String],
        date: DateTime,
    ) -> Result<(u32, u32)> {
        let mut clean = Vec::new();
        for f in flags {
            match canonical_flag(f) {
                Some(c) => {
                    clean.push(c);
                }
                None => return Err(Error::Invalid(format!("flag `{f}`"))),
            }
        }
        let u = self.user_or_err(user)?;
        let mb = u.mailbox_mut(mailbox).ok_or_else(|| Error::UnknownMailbox(mailbox.to_owned()))?;
        Ok(append_msg(mb, Arc::from(raw), clean, date))
    }

    pub fn messages(&self, user: &str, mailbox: &str) -> Result<Vec<StoredMessage>> {
        let u = self.user(user).ok_or_else(|| Error::UnknownUser(user.to_owned()))?;
        let mb = u.mailbox(mailbox).ok_or_else(|| Error::UnknownMailbox(mailbox.to_owned()))?;
        Ok(mb
            .messages
            .iter()
            .map(|m| StoredMessage {
                uid: m.uid,
                flags: m.flags.clone(),
                internal_date: m.internal_date,
                modseq: m.modseq,
                raw: m.raw.to_vec(),
            })
            .collect())
    }

    pub fn mailbox_names(&self, user: &str) -> Result<Vec<String>> {
        let u = self.user(user).ok_or_else(|| Error::UnknownUser(user.to_owned()))?;
        Ok(u.mailboxes.iter().map(|m| m.name.clone()).collect())
    }
}

/// Appends into a mailbox (shared by APPEND, COPY, MOVE and delivery). Returns (uidvalidity, uid).
pub fn append_msg(mb: &mut Mailbox, raw: Arc<[u8]>, mut flags: Vec<String>, date: DateTime) -> (u32, u32) {
    flags.retain(|f| f != "\\Recent");
    let mut dedup = Vec::new();
    add_flags(&mut dedup, &flags);
    let uid = mb.uidnext;
    mb.uidnext = mb.uidnext.saturating_add(1);
    let modseq = mb.next_modseq();
    if mb.messages.len() < MAX_MESSAGES {
        mb.messages.push(Msg { uid, flags: dedup, internal_date: date, raw, modseq });
    }
    (mb.uidvalidity, uid)
}

fn validate_mailbox_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 512
        || name.starts_with(DELIM)
        || name.contains("//")
        || name.chars().any(|c| c.is_control() || c == '*' || c == '%')
    {
        return Err(Error::Invalid(format!("mailbox name `{name}`")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut st = Store::new(100);
        st.add_user("alice@example.com", "pw").unwrap();
        st
    }

    #[test]
    fn names_and_flags() {
        assert_eq!(canonical_name("inbox"), "INBOX");
        assert_eq!(canonical_name("Inbox/Sub"), "INBOX/Sub");
        assert_eq!(canonical_name("Inboxes"), "Inboxes");
        assert_eq!(canonical_flag("\\seen").as_deref(), Some("\\Seen"));
        assert_eq!(canonical_flag("$Label1").as_deref(), Some("$Label1"));
        assert_eq!(canonical_flag("\\Bogus"), None);
        assert_eq!(canonical_flag("a b"), None);
    }

    #[test]
    fn mailbox_lifecycle() {
        let mut s = store();
        s.create_mailbox("ALICE@example.com", "Projects/2026/Q4", None).unwrap();
        let names = s.mailbox_names("alice@example.com").unwrap();
        assert!(names.contains(&"Projects".to_owned()) && names.contains(&"Projects/2026".to_owned()));
        assert!(matches!(
            s.create_mailbox("alice@example.com", "Projects", None),
            Err(Error::MailboxExists(_))
        ));
        s.rename_mailbox("alice@example.com", "Projects", "Work").unwrap();
        let names = s.mailbox_names("alice@example.com").unwrap();
        assert!(names.contains(&"Work/2026/Q4".to_owned()));
        s.delete_mailbox("alice@example.com", "Work/2026/Q4").unwrap();
        assert!(s.delete_mailbox("alice@example.com", "inbox").is_err());
        let (v, uid) =
            s.append("alice@example.com", "inbox", b"x", &["\\seen".into()], DateTime::now()).unwrap();
        assert_eq!(uid, 1);
        assert!(v > 0);
        s.rename_mailbox("alice@example.com", "INBOX", "Old").unwrap();
        assert!(s.messages("alice@example.com", "INBOX").unwrap().is_empty());
        assert_eq!(s.messages("alice@example.com", "Old").unwrap().len(), 1);
    }
}
