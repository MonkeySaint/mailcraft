//! What gets indexed: a flat, owned [`Doc`] per item (mail, appointment, contact, task, note), built from
//! the mailbox model by [`Doc::from_mail`] and friends.

use mailcraft_model::{Appointment, Contact, DateTime, FlagState, Importance, MailItem, Note, Task};

/// Item kind (the `kind:` key; All Outlook Items searches every kind).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocKind {
    #[default]
    Mail,
    Appointment,
    Contact,
    Task,
    Note,
}

impl DocKind {
    pub const ALL: [DocKind; 5] =
        [DocKind::Mail, DocKind::Appointment, DocKind::Contact, DocKind::Task, DocKind::Note];

    /// Name used by the `kind:` key.
    pub fn name(self) -> &'static str {
        match self {
            DocKind::Mail => "email",
            DocKind::Appointment => "meeting",
            DocKind::Contact => "contact",
            DocKind::Task => "task",
            DocKind::Note => "note",
        }
    }

    /// Parses a `kind:` value (folded).
    pub fn parse(v: &str) -> Option<DocKind> {
        Some(match v {
            "email" | "mail" | "message" | "messages" | "emails" => DocKind::Mail,
            "meeting" | "meetings" | "appointment" | "appointments" | "calendar" | "event" => {
                DocKind::Appointment
            }
            "contact" | "contacts" | "person" | "people" => DocKind::Contact,
            "task" | "tasks" | "todo" => DocKind::Task,
            "note" | "notes" => DocKind::Note,
            _ => return None,
        })
    }

    pub(crate) fn bit(self) -> u8 {
        match self {
            DocKind::Mail => 1,
            DocKind::Appointment => 2,
            DocKind::Contact => 4,
            DocKind::Task => 8,
            DocKind::Note => 16,
        }
    }
}

/// Importance level (the `importance:` key).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Level {
    Low,
    #[default]
    Normal,
    High,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Low => "low",
            Level::Normal => "normal",
            Level::High => "high",
        }
    }
}

impl From<Importance> for Level {
    fn from(i: Importance) -> Level {
        match i {
            Importance::Low => Level::Low,
            Importance::Normal => Level::Normal,
            Importance::High => Level::High,
        }
    }
}

/// A person on an item: display name and address (either may be empty).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub addr: String,
}

impl Person {
    pub fn new(name: impl Into<String>, addr: impl Into<String>) -> Person {
        Person { name: name.into(), addr: addr.into() }
    }
    fn of(m: &mailcraft_model::Address) -> Person {
        Person { name: m.name.clone().unwrap_or_default(), addr: m.addr.clone() }
    }
}

/// One searchable item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub id: u64,
    pub kind: DocKind,
    pub folder: u64,
    pub subject: String,
    /// From (and Sender, when different); organizer for meetings.
    pub from: Vec<Person>,
    /// To; attendees for meetings; the contact's own addresses for contacts.
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    /// Plain body text (notes, task and appointment bodies; contact details).
    pub body: String,
    /// Attachment file names.
    pub attachments: Vec<String>,
    pub categories: Vec<String>,
    /// Unix seconds: received (mail), start (appointment), due or start (task), modified (note).
    pub received: i64,
    /// Unix seconds sent (mail only).
    pub sent: Option<i64>,
    pub size: u64,
    pub read: bool,
    pub flagged: bool,
    pub importance: Level,
    pub has_attachments: bool,
    /// Unix seconds the item expires (mail: the `Expiry-Date` or `Expires` header, RFC 4021).
    pub expires: Option<i64>,
}

/// Most header bytes looked at for the expiry date.
const HEADER_SCAN: usize = 64 * 1024;

/// The expiry date in a message's header section (the first `Expiry-Date` or `Expires` header, RFC 4021),
/// unix seconds.
pub fn expiry_of(raw: &[u8]) -> Option<i64> {
    let head = raw.get(..raw.len().min(HEADER_SCAN))?;
    let text = String::from_utf8_lossy(head);
    let mut found: Option<String> = None;
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(v) = current.as_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some(v) = current.take()
            && found.is_none()
        {
            found = Some(v);
        }
        if let Some((name, rest)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("expiry-date") || name.eq_ignore_ascii_case("expires") {
                current = Some(rest.trim().to_owned());
            }
        }
    }
    let value = found.or(current)?;
    DateTime::parse_rfc5322(value.trim()).ok().map(|d| d.unix)
}

/// Longest body text indexed per item (bytes, cut on a char boundary); longer bodies are indexed in part.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

fn cap(mut s: String) -> String {
    if s.len() > MAX_BODY_BYTES {
        let mut end = MAX_BODY_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

impl Doc {
    /// A mail item (body text from the text part, else the HTML with tags stripped).
    pub fn from_mail(m: &MailItem) -> Doc {
        let mut from: Vec<Person> = m.from.iter().map(Person::of).collect();
        if let Some(s) = &m.sender
            && !m.from.iter().any(|f| f.addr.eq_ignore_ascii_case(&s.addr))
        {
            from.push(Person::of(s));
        }
        Doc {
            id: m.id.0,
            kind: DocKind::Mail,
            folder: m.folder.0,
            subject: m.subject.clone(),
            from,
            to: m.to.iter().map(Person::of).collect(),
            cc: m.cc.iter().map(Person::of).collect(),
            body: cap(m.body_text()),
            attachments: m
                .attachments
                .iter()
                .filter(|a| !a.filename.is_empty())
                .map(|a| a.filename.clone())
                .collect(),
            categories: m.categories.clone(),
            received: m.received.unix,
            sent: m.sent.map(|d| d.unix),
            size: m.size,
            read: m.read,
            flagged: matches!(m.flag, FlagState::Flagged { .. }),
            importance: m.importance.into(),
            has_attachments: m.has_attachments(),
            expires: m.raw.as_deref().and_then(expiry_of),
        }
    }

    /// A calendar item: subject + location as the subject, organizer as From, attendees as To.
    pub fn from_appointment(a: &Appointment) -> Doc {
        let subject = if a.location.trim().is_empty() {
            a.subject.clone()
        } else {
            format!("{} {}", a.subject, a.location)
        };
        Doc {
            id: a.id.0,
            kind: DocKind::Appointment,
            folder: a.folder.0,
            subject,
            from: a.organizer.iter().map(Person::of).collect(),
            to: a.attendees.iter().map(|x| Person::of(&x.mailbox)).collect(),
            body: cap(a.body.clone()),
            categories: a.categories.clone(),
            received: a.start.unix,
            read: true,
            ..Doc::default()
        }
    }

    /// A contact: names, company and job title as the subject, addresses as To, the rest as body.
    pub fn from_contact(c: &Contact) -> Doc {
        let subject =
            [&c.title, &c.first_name, &c.middle_name, &c.last_name, &c.suffix, &c.company, &c.job_title]
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        let mut body = String::new();
        for s in [&c.file_as, &c.department, &c.web_page, &c.im_address, &c.notes] {
            if !s.trim().is_empty() {
                body.push_str(s);
                body.push('\n');
            }
        }
        for (_, number) in &c.phones {
            body.push_str(number);
            body.push('\n');
        }
        Doc {
            id: c.id.0,
            kind: DocKind::Contact,
            folder: c.folder.0,
            subject,
            to: c.emails.iter().map(|e| Person::new("", e.clone())).collect(),
            body: cap(body),
            categories: c.categories.clone(),
            read: true,
            ..Doc::default()
        }
    }

    pub fn from_task(t: &Task) -> Doc {
        Doc {
            id: t.id.0,
            kind: DocKind::Task,
            folder: t.folder.0,
            subject: t.subject.clone(),
            body: cap(t.body.clone()),
            categories: t.categories.clone(),
            received: t.due.or(t.start).map_or(0, |d| d.unix),
            read: true,
            ..Doc::default()
        }
    }

    pub fn from_note(n: &Note) -> Doc {
        let subject = n.text.lines().next().unwrap_or_default().to_owned();
        Doc {
            id: n.id.0,
            kind: DocKind::Note,
            folder: n.folder.0,
            subject,
            body: cap(n.text.clone()),
            categories: n.categories.clone(),
            received: n.modified.unix,
            read: true,
            ..Doc::default()
        }
    }
}
