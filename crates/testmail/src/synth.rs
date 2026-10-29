//! Deterministic synthetic mailboxes for the storage benchmarks (`apps/mailcraft-bench`) and importer tests.
//!
//! [`Generator`] yields [`SynthMessage`]s: complete RFC 5322 sources plus the state a mail client keeps
//! beside them (folder, read, flag, categories, Focused/Other). The same [`Config`] always gives the same
//! bytes: the generator uses its own xorshift PRNG ([`Rng`]), never the clock or the system's randomness.
//!
//! What it models (storage ADR, section 6.1):
//! - 30 folders ([`FOLDERS`]) with a realistic skew: an Inbox with about a third of the mail, Archive and its
//!   yearly subfolders, Sent Items, mailing lists, newsletters and project folders;
//! - threads of 1–20 messages with Message-ID / In-Reply-To / References chains and `RE:` / `FW:` subjects;
//!   the user's own replies go to Sent Items;
//! - a body mix of 60% personal plain text (log-normal, 1–8 KB), 25% HTML newsletters (20–80 KB, with a
//!   plain-text alternative) and 15% messages with base64 attachments (log-normal, 50 KB – 5 MB), about
//!   67 KB of source per message on average;
//! - about 20% unread, some flags and categories, and a Focused/Other split.
//!
//! [`Config::small_bodies`] keeps the structure (threads, the body mix, attachments) with bodies of a few
//! kilobytes, so runs with a million messages fit a development machine's disk.
//!
//! Every address is at example.com, example.org or example.net. [`write_maildir`] and [`write_mbox`] write
//! the same messages as a Maildir tree or one MBOX file for importer tests.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use base64::Engine as _;

use crate::date::DateTime;

/// Default seed of [`Config::new`].
pub const DEFAULT_SEED: u64 = 0x6d61_696c_6372_6166;
/// Default "now" of [`Config::new`]: 2026-06-01 12:00:00 UTC. The newest message is a little older.
pub const DEFAULT_NOW: i64 = 1_780_315_200;
/// Time span the messages cover, ending at [`Config::now`] (about five years).
pub const SPAN_SECS: i64 = 5 * 365 * 86_400;
/// Most messages one [`Config`] describes (the benchmark's largest profile is 2 million).
pub const MAX_MESSAGES: usize = 10_000_000;
/// The user's own address (sender of every message in Sent Items and Drafts).
pub const ME: &str = "bench.user@example.com";
/// The user's display name.
pub const ME_NAME: &str = "Bench User";
/// A word that appears in about one message in a thousand: the benchmarks' rare-term query.
pub const RARE_TERM: &str = "zyxquorbit";
/// How often [`RARE_TERM`] appears: in messages whose index is a multiple of this.
pub const RARE_EVERY: usize = 997;

/// The special folders a mail client creates itself; the benchmark maps them onto the profile's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Inbox,
    Drafts,
    Sent,
    Deleted,
    Junk,
    Archive,
}

/// A folder of the synthetic mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderSpec {
    /// Display name (the last path segment).
    pub name: &'static str,
    /// Index of the parent folder in [`FOLDERS`] (`None` = top level).
    pub parent: Option<usize>,
    pub role: Option<Role>,
    /// Relative share of new threads filed here, among the folders of the same [`Style`] class (bulk or
    /// not). Sent Items and Drafts get the user's own messages instead.
    pub weight: u32,
    /// What its threads are like.
    pub style: Style,
}

/// The kind of mail a folder holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Conversations between people (any body kind; Focused).
    Personal,
    /// A mailing list: plain-text threads with many participants (Other).
    List,
    /// Newsletters, offers and receipts: single HTML messages (Other).
    Bulk,
}

const fn folder(
    name: &'static str,
    parent: Option<usize>,
    role: Option<Role>,
    weight: u32,
    style: Style,
) -> FolderSpec {
    FolderSpec { name, parent, role, weight, style }
}

/// The 30 folders, parents before their children.
pub const FOLDERS: [FolderSpec; 30] = [
    folder("Inbox", None, Some(Role::Inbox), 360, Style::Personal),
    folder("Drafts", None, Some(Role::Drafts), 0, Style::Personal),
    folder("Sent Items", None, Some(Role::Sent), 0, Style::Personal),
    folder("Deleted Items", None, Some(Role::Deleted), 25, Style::Personal),
    folder("Junk Email", None, Some(Role::Junk), 20, Style::Bulk),
    folder("Archive", None, Some(Role::Archive), 90, Style::Personal),
    folder("2022", Some(5), None, 30, Style::Personal),
    folder("2023", Some(5), None, 35, Style::Personal),
    folder("2024", Some(5), None, 45, Style::Personal),
    folder("Lists", None, None, 2, Style::List),
    folder("rust-users", Some(9), None, 60, Style::List),
    folder("release-announce", Some(9), None, 15, Style::List),
    folder("gardening-club", Some(9), None, 20, Style::List),
    folder("photo-forum", Some(9), None, 25, Style::List),
    folder("city-cycling", Some(9), None, 15, Style::List),
    folder("Newsletters", None, None, 5, Style::Bulk),
    folder("Weekly digest", Some(15), None, 40, Style::Bulk),
    folder("Deals", Some(15), None, 35, Style::Bulk),
    folder("Projects", None, None, 5, Style::Personal),
    folder("Website", Some(18), None, 25, Style::Personal),
    folder("Budget 2026", Some(18), None, 20, Style::Personal),
    folder("Office move", Some(18), None, 12, Style::Personal),
    folder("Family", None, None, 25, Style::Personal),
    folder("Travel", None, None, 15, Style::Personal),
    folder("Receipts", None, None, 20, Style::Bulk),
    folder("Bills", None, None, 15, Style::Bulk),
    folder("Clubs", None, None, 10, Style::Personal),
    folder("Recruiting", None, None, 8, Style::Personal),
    folder("Support tickets", None, None, 18, Style::Personal),
    folder("Old stuff", None, None, 15, Style::Personal),
];

/// Index of the folder with `role` in [`FOLDERS`].
pub fn role_index(role: Role) -> Option<usize> {
    FOLDERS.iter().position(|f| f.role == Some(role))
}

/// Path of folder `index` with `/` between levels ("Lists/rust-users").
pub fn folder_path(index: usize) -> String {
    let mut parts = Vec::new();
    let mut at = Some(index);
    // Parents come first in FOLDERS, so the walk ends; the bound guards against an edit that breaks that.
    for _ in 0..FOLDERS.len() {
        let Some(i) = at else { break };
        let Some(f) = FOLDERS.get(i) else { break };
        parts.push(f.name);
        at = f.parent;
    }
    parts.reverse();
    parts.join("/")
}

/// What a synthetic mailbox looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Number of messages (capped at [`MAX_MESSAGES`]).
    pub messages: usize,
    pub seed: u64,
    /// Bodies of a few kilobytes instead of the realistic sizes (same structure and mix).
    pub small_bodies: bool,
    /// Unix time the mailbox ends at: the newest message is just before it.
    pub now: i64,
}

impl Config {
    /// `messages` messages with realistic bodies, [`DEFAULT_SEED`] and [`DEFAULT_NOW`].
    pub fn new(messages: usize) -> Config {
        Config {
            messages: messages.min(MAX_MESSAGES),
            seed: DEFAULT_SEED,
            small_bodies: false,
            now: DEFAULT_NOW,
        }
    }

    /// The same with small bodies.
    pub fn small(messages: usize) -> Config {
        Config { small_bodies: true, ..Config::new(messages) }
    }

    /// A generator of this mailbox's messages, oldest first.
    pub fn generate(&self) -> Generator {
        Generator::new(*self)
    }
}

/// The kind of body a message has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BodyKind {
    /// Plain text.
    Text,
    /// An HTML newsletter with a plain-text alternative.
    Html,
    /// A short text with one or two base64 attachments.
    Attachment,
}

/// One generated message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SynthMessage {
    /// Position in the mailbox (0 = oldest).
    pub index: usize,
    /// Index of its folder in [`FOLDERS`].
    pub folder: usize,
    /// The RFC 5322 source (CRLF line ends).
    pub raw: Vec<u8>,
    /// Message-ID without brackets.
    pub message_id: String,
    /// Thread number (messages of one thread share it).
    pub thread: usize,
    /// Position in its thread (0 = the first message).
    pub depth: usize,
    /// Unix time it arrived (also its `Date:`).
    pub received: i64,
    pub read: bool,
    pub flagged: bool,
    /// Category names (the default category names of a new profile).
    pub categories: Vec<&'static str>,
    /// Focused (true) or Other.
    pub focused: bool,
    pub kind: BodyKind,
    /// Sent by the user (Sent Items, Drafts).
    pub from_me: bool,
}

/// Category names given to some messages.
pub const CATEGORIES: [&str; 6] = [
    "Red category",
    "Orange category",
    "Yellow category",
    "Green category",
    "Blue category",
    "Purple category",
];

/// xorshift64* pseudo-random numbers: deterministic, fast, and good enough for test data (not for anything
/// that needs real randomness).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // SplitMix64 once, so nearby seeds give unrelated streams; the state must never be zero.
        let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        Rng(if z == 0 { 0x2545_f491_4f6c_dd1d } else { z })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let n64 = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next_u64() % n64).unwrap_or(0)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// A standard normal deviate (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Log-normal with the given median and shape, clamped to `lo..=hi`.
    pub fn log_normal(&mut self, median: f64, sigma: f64, lo: usize, hi: usize) -> usize {
        let v = median * (sigma * self.normal()).exp();
        let v = if v.is_finite() { v } else { median };
        (v.max(0.0) as usize).clamp(lo, hi.max(lo))
    }

    /// An element of `items` (`None` when empty).
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }

    /// An index into `items`, skewed towards the front (Zipf-like): words and people are not equally common.
    pub fn skewed(&mut self, n: usize) -> usize {
        let u = self.unit();
        ((u * u * u) * n as f64) as usize % n.max(1)
    }
}

const FIRST: [&str; 40] = [
    "Ann", "Ben", "Carla", "Dev", "Elena", "Femi", "Grace", "Hiro", "Ines", "Jonas", "Kira", "Luis", "Maya",
    "Nils", "Olga", "Pablo", "Quinn", "Rosa", "Sami", "Tara", "Umar", "Vera", "Wen", "Ximena", "Yusuf",
    "Zoe", "Aaron", "Bianca", "Chen", "Dana", "Emil", "Fatima", "Goran", "Hanna", "Ivan", "Julia", "Karim",
    "Lena", "Marco", "Nadia",
];
const LAST: [&str; 30] = [
    "Lee",
    "Novak",
    "Okafor",
    "Silva",
    "Tanaka",
    "Weber",
    "Garcia",
    "Kowalski",
    "Haddad",
    "Larsen",
    "Moreau",
    "Rossi",
    "Schmidt",
    "Dubois",
    "Nguyen",
    "Patel",
    "Kim",
    "Jensen",
    "Ivanova",
    "Costa",
    "Fischer",
    "Murphy",
    "Ahmed",
    "Berg",
    "Castro",
    "Lindqvist",
    "Mendes",
    "Sato",
    "Varga",
    "Young",
];
const DOMAINS: [&str; 3] = ["example.com", "example.org", "example.net"];

/// Common words, roughly most frequent first (the generator favours the front of the list).
const WORDS: [&str; 159] = [
    "the",
    "and",
    "to",
    "of",
    "a",
    "in",
    "is",
    "for",
    "on",
    "that",
    "with",
    "we",
    "it",
    "this",
    "be",
    "you",
    "are",
    "as",
    "at",
    "will",
    "have",
    "from",
    "can",
    "our",
    "please",
    "meeting",
    "about",
    "by",
    "an",
    "not",
    "or",
    "if",
    "your",
    "all",
    "next",
    "week",
    "time",
    "team",
    "new",
    "update",
    "thanks",
    "project",
    "plan",
    "review",
    "today",
    "would",
    "should",
    "could",
    "also",
    "more",
    "some",
    "one",
    "two",
    "three",
    "there",
    "when",
    "what",
    "which",
    "just",
    "like",
    "know",
    "think",
    "send",
    "attached",
    "draft",
    "notes",
    "agenda",
    "budget",
    "report",
    "schedule",
    "deadline",
    "Friday",
    "Monday",
    "morning",
    "afternoon",
    "call",
    "office",
    "client",
    "design",
    "release",
    "version",
    "build",
    "test",
    "issue",
    "fix",
    "change",
    "question",
    "answer",
    "idea",
    "proposal",
    "contract",
    "invoice",
    "payment",
    "order",
    "delivery",
    "shipping",
    "address",
    "garden",
    "bicycle",
    "route",
    "photo",
    "camera",
    "lens",
    "trip",
    "flight",
    "hotel",
    "booking",
    "ticket",
    "dinner",
    "lunch",
    "weekend",
    "holiday",
    "family",
    "birthday",
    "party",
    "gift",
    "school",
    "course",
    "lesson",
    "book",
    "chapter",
    "library",
    "museum",
    "concert",
    "music",
    "film",
    "recipe",
    "kitchen",
    "market",
    "price",
    "discount",
    "offer",
    "member",
    "club",
    "event",
    "volunteer",
    "survey",
    "results",
    "quarter",
    "summary",
    "figures",
    "numbers",
    "chart",
    "slides",
    "website",
    "server",
    "backup",
    "password",
    "account",
    "support",
    "request",
    "status",
    "pending",
    "approved",
    "confirmed",
    "cancelled",
    "moved",
    "postponed",
    "soon",
];
const TOPICS: [&str; 40] = [
    "Quarterly budget review",
    "Website redesign",
    "Office move timeline",
    "Team offsite",
    "Release notes",
    "Garden plot rota",
    "Weekend ride",
    "Photo walk",
    "Contract renewal",
    "Invoice question",
    "Holiday plans",
    "Birthday dinner",
    "Server maintenance",
    "Backup schedule",
    "Course enrolment",
    "Library hours",
    "Concert tickets",
    "Recipe swap",
    "Volunteer shifts",
    "Survey results",
    "Project kickoff",
    "Design feedback",
    "Hiring panel",
    "Interview slots",
    "Support request",
    "Delivery delay",
    "Booking confirmation",
    "Travel itinerary",
    "Club newsletter draft",
    "Annual meeting",
    "Lens recommendations",
    "Route change",
    "Build failure",
    "Version upgrade",
    "Payment reminder",
    "Shipping address",
    "Slides for Friday",
    "Figures for the report",
    "Kitchen renovation",
    "Museum visit",
];
const BULK_TOPICS: [&str; 12] = [
    "This week's digest",
    "Deals of the day",
    "Your monthly statement",
    "Order receipt",
    "New arrivals",
    "Member update",
    "Event calendar",
    "Product news",
    "Your bill is ready",
    "Last chance offer",
    "Community highlights",
    "Tips and tricks",
];
const ATTACHMENTS: [(&str, &str); 6] = [
    ("report", "application/pdf"),
    ("slides", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
    ("photo", "image/jpeg"),
    ("figures", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    ("scan", "application/pdf"),
    ("archive", "application/zip"),
];
const EXTENSIONS: [&str; 6] = ["pdf", "pptx", "jpg", "xlsx", "pdf", "zip"];

/// A person in the synthetic address book.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Person {
    name: String,
    addr: String,
}

/// Number of distinct correspondents.
const PEOPLE: usize = 600;

fn person(i: usize) -> Person {
    let first = FIRST.get(i % FIRST.len()).copied().unwrap_or("Ann");
    let last = LAST.get((i / FIRST.len()) % LAST.len()).copied().unwrap_or("Lee");
    let domain = DOMAINS.get(i % DOMAINS.len()).copied().unwrap_or("example.com");
    let n = i / (FIRST.len() * LAST.len());
    let local = if n == 0 {
        format!("{}.{}", first.to_ascii_lowercase(), last.to_ascii_lowercase())
    } else {
        format!("{}.{}{n}", first.to_ascii_lowercase(), last.to_ascii_lowercase())
    };
    Person { name: format!("{first} {last}"), addr: format!("{local}@{domain}") }
}

/// A thread still taking replies.
#[derive(Clone, Debug)]
struct Thread {
    id: usize,
    folder: usize,
    style: Style,
    subject: String,
    /// Participants other than the user.
    people: Vec<usize>,
    /// Message-IDs so far (References keeps the last 20).
    ids: Vec<String>,
    len: usize,
    target: usize,
}

/// Most conversations open at once; a message continues one of them or starts another.
const OPEN_THREADS: usize = 48;
/// Share of messages that are bulk mail (newsletters, offers, receipts: one message, HTML, Other).
const BULK_SHARE: f64 = 0.15;
/// Weight of the Inbox among the folders bulk mail is filed in (the bulk folders have their own weights).
const BULK_INBOX_WEIGHT: u32 = 150;

/// Yields the messages of a [`Config`], oldest first. Deterministic: the same config gives the same bytes.
#[derive(Clone, Debug)]
pub struct Generator {
    cfg: Config,
    rng: Rng,
    next: usize,
    open: Vec<Thread>,
    threads: usize,
}

impl Generator {
    pub fn new(cfg: Config) -> Generator {
        let cfg = Config { messages: cfg.messages.min(MAX_MESSAGES), ..cfg };
        Generator { rng: Rng::new(cfg.seed), cfg, next: 0, open: Vec::new(), threads: 0 }
    }

    /// A folder for a new thread, by weight: bulk mail goes to the bulk folders or the Inbox, conversations
    /// to the other folders.
    fn pick_folder(&mut self, bulk: bool) -> usize {
        let weight = |f: &FolderSpec| match (bulk, f.style == Style::Bulk) {
            (true, true) | (false, false) => f.weight,
            (true, false) if f.role == Some(Role::Inbox) => BULK_INBOX_WEIGHT,
            _ => 0,
        };
        let total: u32 = FOLDERS.iter().map(weight).sum();
        let mut at = u32::try_from(self.rng.below(total as usize)).unwrap_or(0);
        for (i, f) in FOLDERS.iter().enumerate() {
            let w = weight(f);
            if at < w {
                return i;
            }
            at -= w;
        }
        0
    }

    fn new_thread(&mut self, bulk: bool) -> Thread {
        let folder = self.pick_folder(bulk);
        let style = if bulk { Style::Bulk } else { FOLDERS.get(folder).map_or(Style::Personal, |f| f.style) };
        let id = self.threads;
        self.threads = self.threads.saturating_add(1);
        let (subject, target, people) = match style {
            Style::Bulk => {
                let topic = self.rng.pick(&BULK_TOPICS).copied().unwrap_or("News");
                // Senders of bulk mail: a few dozen addresses at the end of the address book.
                let sender = PEOPLE - 1 - self.rng.below(40);
                (format!("{topic} #{}", 1 + self.rng.below(500)), 1, vec![sender])
            }
            Style::List => {
                let topic = self.rng.pick(&TOPICS).copied().unwrap_or("Question");
                let n = 2 + self.rng.below(6);
                let people = (0..n).map(|_| self.rng.skewed(PEOPLE)).collect();
                (format!("[list] {topic}"), 1 + self.thread_len(), people)
            }
            Style::Personal => {
                let topic = self.rng.pick(&TOPICS).copied().unwrap_or("Hello");
                let n = 1 + self.rng.below(3);
                let people = (0..n).map(|_| self.rng.skewed(PEOPLE)).collect();
                (topic.to_owned(), 1 + self.thread_len(), people)
            }
        };
        Thread { id, folder, style, subject, people, ids: Vec::new(), len: 0, target }
    }

    /// Replies a thread gets beyond its first message (0–19, mostly short).
    fn thread_len(&mut self) -> usize {
        let u = self.rng.unit();
        ((u * u * u) * 20.0) as usize % 20
    }

    fn next_message(&mut self) -> SynthMessage {
        let index = self.next;
        self.next = self.next.saturating_add(1);
        if self.rng.chance(BULK_SHARE) {
            let mut t = self.new_thread(true);
            return self.compose(index, &mut t);
        }
        // Continue an open conversation or start a new one.
        let start = self.open.is_empty() || (self.open.len() < OPEN_THREADS && self.rng.chance(0.3));
        let slot = self.rng.below(self.open.len());
        let mut thread = if start || slot >= self.open.len() {
            self.new_thread(false)
        } else {
            self.open.swap_remove(slot)
        };
        let msg = self.compose(index, &mut thread);
        if thread.len < thread.target {
            self.open.push(thread);
        }
        msg
    }

    fn compose(&mut self, index: usize, thread: &mut Thread) -> SynthMessage {
        let depth = thread.len;
        thread.len = thread.len.saturating_add(1);
        let style = thread.style;
        // The user answers about a quarter of personal replies; drafts are rare.
        let from_me = style == Style::Personal && depth > 0 && self.rng.chance(0.25);
        let folder = if from_me {
            if self.rng.chance(0.01) { role_index(Role::Drafts) } else { role_index(Role::Sent) }
                .unwrap_or(thread.folder)
        } else {
            thread.folder
        };
        let kind = match style {
            Style::Bulk => BodyKind::Html,
            Style::List => {
                if self.rng.chance(0.04) {
                    BodyKind::Attachment
                } else {
                    BodyKind::Text
                }
            }
            Style::Personal => {
                // With the bulk mail and the lists this makes the mailbox about 60/25/15.
                let u = self.rng.unit();
                if u < 0.66 {
                    BodyKind::Text
                } else if u < 0.80 {
                    BodyKind::Html
                } else {
                    BodyKind::Attachment
                }
            }
        };
        // Times rise steadily from `now - SPAN` to `now`, with jitter.
        let n = self.cfg.messages.max(1) as f64;
        let step = SPAN_SECS as f64 / n;
        let base = self.cfg.now - SPAN_SECS + (index as f64 * step) as i64;
        let jitter = (self.rng.unit() * step * 0.9) as i64;
        let received = base.saturating_add(jitter).min(self.cfg.now - 60);
        let message_id = format!("{index:x}.{:x}.{:x}@example.net", self.cfg.seed & 0xffff_ffff, thread.id);
        let me = Person { name: ME_NAME.to_owned(), addr: ME.to_owned() };
        let other = |i: &usize| person(*i);
        let lead =
            thread.people.get(depth % thread.people.len().max(1)).map(other).unwrap_or_else(|| person(0));
        let (from, mut to) = if from_me { (me.clone(), vec![lead]) } else { (lead, vec![me.clone()]) };
        let cc: Vec<Person> =
            thread.people.iter().skip(1).take(3).map(other).filter(|p| *p != from).collect();
        if style == Style::List {
            let list = FOLDERS.get(thread.folder).map_or("list", |f| f.name);
            to = vec![Person { name: format!("{list} list"), addr: format!("{list}@example.org") }];
        }
        let forward = depth > 0 && style == Style::Personal && self.rng.chance(0.08);
        let subject = match (depth, forward) {
            (0, _) => thread.subject.clone(),
            (_, true) => format!("FW: {}", thread.subject),
            _ => format!("RE: {}", thread.subject),
        };
        let date = DateTime::from_unix(received).to_rfc5322();
        let mut head = String::with_capacity(1024);
        head.push_str(&format!("Date: {date}\r\n"));
        head.push_str(&format!("From: {}\r\n", fmt_person(&from)));
        head.push_str(&format!("To: {}\r\n", fmt_list(&to)));
        if !cc.is_empty() {
            head.push_str(&format!("Cc: {}\r\n", fmt_list(&cc)));
        }
        head.push_str(&format!("Subject: {subject}\r\n"));
        head.push_str(&format!("Message-ID: <{message_id}>\r\n"));
        if let Some(parent) = thread.ids.last() {
            head.push_str(&format!("In-Reply-To: <{parent}>\r\n"));
            let refs: Vec<String> = thread
                .ids
                .iter()
                .skip(thread.ids.len().saturating_sub(20))
                .map(|r| format!("<{r}>"))
                .collect();
            head.push_str(&format!("References: {}\r\n", refs.join("\r\n ")));
        }
        if style == Style::List {
            let list = FOLDERS.get(thread.folder).map_or("list", |f| f.name);
            head.push_str(&format!("List-Id: <{list}.example.org>\r\n"));
        }
        if style == Style::Bulk {
            head.push_str("List-Unsubscribe: <https://www.example.org/unsubscribe>\r\n");
            head.push_str("Precedence: bulk\r\n");
        }
        let flagged = !from_me && self.rng.chance(0.03);
        let mut categories = Vec::new();
        if self.rng.chance(0.08) {
            categories
                .push(CATEGORIES.get(self.rng.skewed(CATEGORIES.len())).copied().unwrap_or("Red category"));
            if self.rng.chance(0.25)
                && let Some(c) = self.rng.pick(&CATEGORIES).copied()
                && !categories.contains(&c)
            {
                categories.push(c);
            }
        }
        if !categories.is_empty() {
            head.push_str(&format!("Keywords: {}\r\n", categories.join(", ")));
        }
        head.push_str("MIME-Version: 1.0\r\n");
        let body = self.body(index, kind, &subject, &from, depth);
        let mut raw = head.into_bytes();
        raw.extend_from_slice(&body);
        thread.ids.push(message_id.clone());
        // About 20% unread, mostly recent mail; the user's own messages are read.
        let age = (self.cfg.now - received) as f64 / SPAN_SECS as f64;
        let read = from_me || !self.rng.chance(0.46 * (1.0 - age).max(0.0) + 0.01);
        let focused = style == Style::Personal && folder != role_index(Role::Junk).unwrap_or(usize::MAX);
        SynthMessage {
            index,
            folder,
            raw,
            message_id,
            thread: thread.id,
            depth,
            received,
            read,
            flagged,
            categories,
            focused,
            kind,
            from_me,
        }
    }

    /// Words of running text, about `bytes` long.
    fn prose(&mut self, rare: bool, bytes: usize) -> String {
        let mut out = String::with_capacity(bytes.saturating_add(80));
        let mut line = 0usize;
        let mut sentence = 0usize;
        if rare {
            out.push_str(RARE_TERM);
            out.push(' ');
        }
        while out.len() < bytes {
            let w = WORDS.get(self.rng.skewed(WORDS.len())).copied().unwrap_or("the");
            // Mail vocabularies also hold ids, numbers and links.
            let roll = self.rng.below(40);
            let word: String = if roll == 0 {
                format!("#{}", 1000 + self.rng.below(90_000))
            } else if roll == 1 {
                format!("https://www.example.org/p/{:x}", self.rng.next_u64() & 0xff_ffff)
            } else {
                w.to_owned()
            };
            if sentence == 0 {
                let mut c = word.chars();
                if let Some(first) = c.next() {
                    out.extend(first.to_uppercase());
                    out.push_str(c.as_str());
                }
            } else {
                out.push_str(&word);
            }
            line = line.saturating_add(word.len().saturating_add(1));
            sentence += 1;
            if sentence > 6 && self.rng.chance(0.15) {
                out.push('.');
                sentence = 0;
            }
            if line > 70 {
                out.push_str("\r\n");
                line = 0;
                if self.rng.chance(0.12) {
                    out.push_str("\r\n");
                }
            } else {
                out.push(' ');
            }
        }
        out.push_str("\r\n");
        out
    }

    fn text_size(&mut self) -> usize {
        if self.cfg.small_bodies {
            self.rng.log_normal(500.0, 0.5, 150, 1_500)
        } else {
            self.rng.log_normal(2_800.0, 0.55, 1_000, 8_000)
        }
    }

    fn body(&mut self, index: usize, kind: BodyKind, subject: &str, from: &Person, depth: usize) -> Vec<u8> {
        let boundary = format!("=_synth_{index:x}");
        match kind {
            BodyKind::Text => {
                let size = self.text_size();
                let mut text = self.prose(index.is_multiple_of(RARE_EVERY), size);
                if depth > 0 {
                    text.push_str(&format!("\r\nOn an earlier day, {} wrote:\r\n", from.name));
                    let quoted = self.prose(false, size / 3);
                    for l in quoted.split("\r\n").filter(|l| !l.is_empty()) {
                        text.push_str("> ");
                        text.push_str(l);
                        text.push_str("\r\n");
                    }
                }
                let mut out = b"Content-Type: text/plain; charset=utf-8\r\n\
                    Content-Transfer-Encoding: 8bit\r\n\r\n"
                    .to_vec();
                out.extend_from_slice(text.as_bytes());
                out
            }
            BodyKind::Html => {
                let size = if self.cfg.small_bodies {
                    self.rng.log_normal(2_000.0, 0.4, 1_000, 4_000)
                } else {
                    self.rng.log_normal(38_000.0, 0.35, 20_000, 80_000)
                };
                let html = self.html(index, subject, size);
                let alt = self.prose(false, size / 4);
                let mut out = format!(
                    "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n\
                     --{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                     Content-Transfer-Encoding: 8bit\r\n\r\n"
                )
                .into_bytes();
                out.extend_from_slice(alt.as_bytes());
                out.extend_from_slice(
                    format!(
                        "\r\n--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n\
                         Content-Transfer-Encoding: 8bit\r\n\r\n"
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(html.as_bytes());
                out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                out
            }
            BodyKind::Attachment => {
                let size = self.text_size() / 2;
                let text = self.prose(index.is_multiple_of(RARE_EVERY), size.max(200));
                let mut out = format!(
                    "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n\
                     --{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                     Content-Transfer-Encoding: 8bit\r\n\r\n"
                )
                .into_bytes();
                out.extend_from_slice(text.as_bytes());
                let parts = if self.rng.chance(0.2) { 2 } else { 1 };
                for _ in 0..parts {
                    let bytes = if self.cfg.small_bodies {
                        self.rng.log_normal(2_000.0, 0.4, 1_000, 4_000)
                    } else {
                        self.rng.log_normal(165_000.0, 1.0, 50_000, 5_000_000) / parts
                    };
                    let which = self.rng.below(ATTACHMENTS.len());
                    let (stem, mime) = ATTACHMENTS.get(which).copied().unwrap_or(("file", "application/pdf"));
                    let ext = EXTENSIONS.get(which).copied().unwrap_or("bin");
                    let name = format!("{stem}-{}.{ext}", 1 + self.rng.below(999));
                    out.extend_from_slice(
                        format!(
                            "\r\n--{boundary}\r\nContent-Type: {mime}; name=\"{name}\"\r\n\
                             Content-Disposition: attachment; filename=\"{name}\"\r\n\
                             Content-Transfer-Encoding: base64\r\n\r\n"
                        )
                        .as_bytes(),
                    );
                    self.base64_noise(bytes, &mut out);
                }
                out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                out
            }
        }
    }

    fn html(&mut self, index: usize, subject: &str, bytes: usize) -> String {
        let mut out = String::with_capacity(bytes.saturating_add(512));
        out.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>");
        out.push_str(subject);
        out.push_str(
            "</title></head><body style=\"margin:0;font-family:sans-serif\">\r\n<table width=\"100%\" \
             cellpadding=\"0\" cellspacing=\"0\" role=\"presentation\">\r\n",
        );
        let mut section = 0usize;
        while out.len() < bytes {
            section += 1;
            let para = self.prose(section == 1 && index.is_multiple_of(RARE_EVERY), 600);
            out.push_str(&format!(
                "<tr><td style=\"padding:12px 24px;color:#333333;font-size:15px;line-height:22px\">\r\n\
                 <h2 style=\"font-size:18px;margin:0 0 8px\">Section {section}</h2>\r\n<p>{}</p>\r\n\
                 <a href=\"https://www.example.org/read/{index:x}/{section}?utm_source=newsletter\" \
                 style=\"color:#1a73e8\">Read more</a></td></tr>\r\n",
                para.replace("\r\n", " ")
            ));
        }
        out.push_str(
            "<tr><td style=\"padding:24px;font-size:12px;color:#888888\">You receive this because you \
             subscribed at www.example.org. <a href=\"https://www.example.org/unsubscribe\">Unsubscribe</a>\
             </td></tr></table></body></html>",
        );
        out
    }

    /// `bytes` of noise, base64-encoded in 76-column lines, appended to `out`.
    fn base64_noise(&mut self, bytes: usize, out: &mut Vec<u8>) {
        // 57 input bytes make one 76-character line.
        let mut chunk = [0u8; 57];
        let mut left = bytes;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut line = String::with_capacity(80);
        while left > 0 {
            let n = left.min(chunk.len());
            for b in chunk.iter_mut().take(n) {
                *b = (self.rng.next_u64() >> 56) as u8;
            }
            line.clear();
            engine.encode_string(chunk.get(..n).unwrap_or(&[]), &mut line);
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\r\n");
            left -= n;
        }
    }
}

impl Iterator for Generator {
    type Item = SynthMessage;

    fn next(&mut self) -> Option<SynthMessage> {
        (self.next < self.cfg.messages).then(|| self.next_message())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.cfg.messages.saturating_sub(self.next);
        (left, Some(left))
    }
}

fn fmt_person(p: &Person) -> String {
    format!("{} <{}>", p.name, p.addr)
}

fn fmt_list(people: &[Person]) -> String {
    people.iter().map(fmt_person).collect::<Vec<_>>().join(", ")
}

/// The separator before a Maildir file name's info (`2,FS`): `:` as the Maildir convention has it, `!` on
/// Windows, where `:` cannot be in a file name (the separator other Windows Maildir tools use).
pub const MAILDIR_INFO_SEP: char = if cfg!(windows) { '!' } else { ':' };

/// The `cur/` or `new/` subfolder and file name of message `m` in a Maildir. Unread, unflagged messages
/// go to `new/` without an info suffix (Maildir keeps the suffix for `cur/`); the others go to `cur/` with
/// `F` / `S` flags after [`MAILDIR_INFO_SEP`]`2,`.
pub fn maildir_name(m: &SynthMessage) -> (&'static str, String) {
    let unique = format!("{}.M{}.synth", m.received, m.index);
    if !m.read && !m.flagged {
        return ("new", unique);
    }
    let mut flags = String::new();
    if m.flagged {
        flags.push('F');
    }
    if m.read {
        flags.push('S');
    }
    ("cur", format!("{unique}{MAILDIR_INFO_SEP}2,{flags}"))
}

/// Writes the mailbox as a Maildir tree under `dir`: one Maildir per folder at its path (`Inbox`,
/// `Lists/rust-users`…), named by [`maildir_name`]. Returns the number of messages written.
pub fn write_maildir(dir: &Path, cfg: &Config) -> io::Result<usize> {
    for i in 0..FOLDERS.len() {
        let base = dir.join(folder_path(i));
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(base.join(sub))?;
        }
    }
    let mut n = 0usize;
    for m in cfg.generate() {
        let (sub, name) = maildir_name(&m);
        fs::write(dir.join(folder_path(m.folder)).join(sub).join(name), &m.raw)?;
        n += 1;
    }
    Ok(n)
}

/// Writes every message to one MBOX file (`mboxrd` quoting of `From ` lines) with a `Status:` header for
/// read messages. Returns the number of messages written.
pub fn write_mbox(path: &Path, cfg: &Config) -> io::Result<usize> {
    let mut w = BufWriter::new(fs::File::create(path)?);
    let mut n = 0usize;
    for m in cfg.generate() {
        let date = DateTime::from_unix(m.received).to_rfc5322();
        writeln!(w, "From {} {date}", if m.from_me { ME } else { "sender@example.org" })?;
        if m.read {
            w.write_all(b"Status: RO\n")?;
        }
        for line in m.raw.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let unquoted =
                line.iter().position(|&b| b != b'>').map_or(line, |p| line.get(p..).unwrap_or(&[]));
            if unquoted.starts_with(b"From ") {
                w.write_all(b">")?;
            }
            w.write_all(line)?;
            w.write_all(b"\n")?;
        }
        w.write_all(b"\n")?;
        n += 1;
    }
    w.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_config_gives_the_same_bytes() {
        let a: Vec<SynthMessage> = Config::new(300).generate().collect();
        let b: Vec<SynthMessage> = Config::new(300).generate().collect();
        assert_eq!(a, b);
        let c: Vec<SynthMessage> = Config { seed: 7, ..Config::new(300) }.generate().collect();
        assert_ne!(a.first().map(|m| &m.raw), c.first().map(|m| &m.raw));
    }

    #[test]
    fn folders_and_weights_are_consistent() {
        assert_eq!(FOLDERS.len(), 30);
        for (i, f) in FOLDERS.iter().enumerate() {
            assert!(f.parent.is_none_or(|p| p < i), "{} comes after its parent", f.name);
        }
        assert_eq!(folder_path(10), "Lists/rust-users");
        assert!(role_index(Role::Inbox).is_some() && role_index(Role::Sent).is_some());
        assert!(FOLDERS.iter().any(|f| f.style == Style::Bulk && f.weight > 0));
    }

    #[test]
    fn mailbox_has_the_planned_shape() {
        let msgs: Vec<SynthMessage> = Config::small(5_000).generate().collect();
        assert_eq!(msgs.len(), 5_000);
        let share = |f: &dyn Fn(&SynthMessage) -> bool| {
            msgs.iter().filter(|m| f(m)).count() as f64 / msgs.len() as f64
        };
        let inbox = role_index(Role::Inbox).unwrap();
        let sent = role_index(Role::Sent).unwrap();
        let inbox_share = share(&|m| m.folder == inbox);
        assert!((0.25..0.45).contains(&inbox_share), "inbox {inbox_share}");
        assert!(share(&|m| m.folder == sent) > 0.03);
        let text = share(&|m| m.kind == BodyKind::Text);
        let html = share(&|m| m.kind == BodyKind::Html);
        let att = share(&|m| m.kind == BodyKind::Attachment);
        assert!((0.5..0.7).contains(&text), "text {text}");
        assert!((0.18..0.32).contains(&html), "html {html}");
        assert!((0.1..0.2).contains(&att), "attachments {att}");
        let unread = share(&|m| !m.read);
        assert!((0.1..0.3).contains(&unread), "unread {unread}");
        assert!(share(&|m| !m.focused) > 0.1);
        assert!(share(&|m| m.flagged) > 0.0 && share(&|m| !m.categories.is_empty()) > 0.0);
        // Threads of 1–20 messages chained by References.
        let mut sizes = std::collections::BTreeMap::new();
        for m in &msgs {
            *sizes.entry(m.thread).or_insert(0usize) += 1;
        }
        assert!(sizes.values().all(|n| (1..=20).contains(n)));
        assert!(sizes.values().any(|n| *n >= 5));
        let reply = msgs.iter().find(|m| m.depth > 0).unwrap();
        let text = String::from_utf8_lossy(&reply.raw);
        assert!(text.contains("In-Reply-To: <") && text.contains("References: <"));
        assert!(text.contains("Subject: RE: ") || text.contains("Subject: FW: "));
        // Ascending dates, all in the span.
        assert!(msgs.windows(2).all(|w| w[0].received <= w[1].received + SPAN_SECS / 5_000));
        assert!(msgs.iter().all(|m| m.received < DEFAULT_NOW && m.received > DEFAULT_NOW - SPAN_SECS - 1));
        // Only the example domains.
        for m in &msgs {
            let text = String::from_utf8_lossy(&m.raw);
            for word in text.split(|c: char| c.is_whitespace() || "<>\",;()".contains(c)) {
                if let Some((_, domain)) = word.split_once('@') {
                    assert!(DOMAINS.contains(&domain), "{word}");
                }
            }
        }
        assert!(msgs.iter().any(|m| String::from_utf8_lossy(&m.raw).contains(RARE_TERM)));
    }

    #[test]
    fn full_bodies_average_about_67_kb() {
        let msgs: Vec<SynthMessage> = Config::new(1_500).generate().collect();
        let avg = msgs.iter().map(|m| m.raw.len()).sum::<usize>() / msgs.len();
        assert!((40_000..100_000).contains(&avg), "average {avg}");
        let small: Vec<SynthMessage> = Config::small(1_500).generate().collect();
        let avg_small = small.iter().map(|m| m.raw.len()).sum::<usize>() / small.len();
        assert!(avg_small < 5_000, "small average {avg_small}");
    }

    #[test]
    fn maildir_names_follow_the_flags() {
        let msgs: Vec<SynthMessage> = Config::small(400).generate().collect();
        let pick = |read: bool, flagged: bool| msgs.iter().find(|m| m.read == read && m.flagged == flagged);
        let sep = MAILDIR_INFO_SEP;
        if let Some(m) = pick(false, false) {
            let (sub, name) = maildir_name(m);
            assert_eq!(sub, "new");
            assert!(name.ends_with(".synth"), "{name}");
        }
        if let Some(m) = pick(true, false) {
            assert_eq!(maildir_name(m), ("cur", format!("{}.M{}.synth{sep}2,S", m.received, m.index)));
        }
        if let Some(m) = pick(false, true) {
            assert_eq!(maildir_name(m), ("cur", format!("{}.M{}.synth{sep}2,F", m.received, m.index)));
        }
        let m = pick(true, true).unwrap();
        assert_eq!(maildir_name(m).1, format!("{}.M{}.synth{sep}2,FS", m.received, m.index));
    }

    #[test]
    fn maildir_and_mbox_hold_every_message() {
        let dir = tempdir();
        let cfg = Config::small(120);
        assert_eq!(write_maildir(&dir.join("maildir"), &cfg).unwrap(), 120);
        let mut files = 0;
        for i in 0..FOLDERS.len() {
            for sub in ["cur", "new"] {
                files += fs::read_dir(dir.join("maildir").join(folder_path(i)).join(sub)).unwrap().count();
            }
        }
        assert_eq!(files, 120);
        // No `:` in names on Windows; nothing in `new/` carries the info suffix; `cur/` names do.
        for i in 0..FOLDERS.len() {
            let base = dir.join("maildir").join(folder_path(i));
            for e in fs::read_dir(base.join("new")).unwrap() {
                let name = e.unwrap().file_name().into_string().unwrap();
                assert!(!name.contains(":2,") && !name.contains("!2,"), "{name}");
            }
            for e in fs::read_dir(base.join("cur")).unwrap() {
                let name = e.unwrap().file_name().into_string().unwrap();
                assert!(name.contains(&format!("{MAILDIR_INFO_SEP}2,")), "{name}");
                assert!(!cfg!(windows) || !name.contains(':'), "{name}");
            }
        }
        let mbox = dir.join("all.mbox");
        assert_eq!(write_mbox(&mbox, &cfg).unwrap(), 120);
        let text = fs::read_to_string(&mbox).unwrap();
        assert_eq!(text.lines().filter(|l| l.starts_with("From ")).count(), 120);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A fresh directory under the system temp dir (`TMPDIR`).
    fn tempdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mailcraft-synth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
}
