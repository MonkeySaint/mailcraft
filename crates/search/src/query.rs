//! The instant-search query language, parsed into a [`Query`] tree.
//!
//! ```text
//! query   := or
//! or      := and ( "OR" and )*
//! and     := unary ( ["AND"] unary )*          (juxtaposition is AND)
//! unary   := ( "NOT" | "-" )* primary
//! primary := "(" query ")" | "\"phrase\"" | key ":" value | word
//! value   := word | "\"phrase\"" | "(" query ")"   (inside the parentheses bare words take the key)
//! ```
//!
//! - Operators are upper-case (`AND`, `OR`, `NOT`); lower-case `and`/`or`/`not` are ordinary words. NOT binds
//!   tighter than AND, AND tighter than OR.
//! - Bare words are prefix matches on word tokens (`bud` finds "budget"); `"quoted phrases"` match whole
//!   words in a row (a trailing `*` inside the quotes makes the last word a prefix). A bare word holding
//!   punctuation (`ann.lee@example.com`) is a phrase of its tokens. Case and diacritics never matter.
//! - Keys: `from`, `to`, `cc`, `tocc` (To or Cc), `participants` (From, To or Cc), `subject`, `body`,
//!   `attachments` (file names), `category` (`any`, `none` or a name), `folder` (a folder name),
//!   `hasattachments`, `isread`, `isflagged` (`yes`/`no`), `importance` (`high`/`normal`/`low`), `received`
//!   and `sent` (dates, see below), `size` (see below), `kind` (`email`, `meeting`, `contact`, `task`,
//!   `note`), `expires` (see below). People keys accept `me` (one of the user's own addresses).
//! - Dates: `today`, `yesterday`, `tomorrow`, `this|last|next week|month|year` (quoted or not),
//!   `N days|weeks|months|years ago`, `2026-10-09`, `2026/10/09`, `10/9/2026`; comparisons `>=x`, `<=x`,
//!   `>x`, `<x` and ranges `a..b` (either side may be missing). Unquoted multi-word dates also work inside
//!   `received:( … )` and on either side of a range (`received:last week..today`). Years have four digits.
//!   Weeks start on Sunday.
//! - Sizes: `tiny` (< 10 KB), `small` (10–25 KB), `medium` (25–100 KB), `large` (100–500 KB), `verylarge`
//!   (500 KB–1 MB), `huge` (> 1 MB), or byte counts with an optional `B`/`KB`/`MB`/`GB` unit
//!   (1 KB = 1024 bytes) as `>=x`, `<=x`, `>x`, `<x`, `a..b`; a bare number means "at least".
//! - Expiry (mail with an `Expiry-Date` / `Expires` header): `expires:soon` (not expired yet, and expiring
//!   by the end of the [`EXPIRING_SOON_DAYS`]th day from today: View Items Expiring Soon), `expires:yes`
//!   (any expiry date), `expires:no`, `expires:expired` (the date has passed), or a date / range as for
//!   `received:`.
//! - A key with an empty value (`from:""`, `subject:`) is a placeholder being typed and matches everything;
//!   a value a key does not understand (`isread:maybe`) matches nothing.
//!
//! Parsing never fails and never panics: unbalanced quotes and parentheses are closed at the end, input is
//! cut at [`MAX_QUERY_CHARS`], nesting at [`MAX_DEPTH`] and terms at [`MAX_TERMS`]. [`Query`]'s `Display`
//! writes canonical text that parses back to the same tree.

use std::fmt;

use mailcraft_model::dates::{civil_from_days, days_from_civil};

use crate::doc::{DocKind, Level};
use crate::token::words;

/// Longest query parsed (chars); the rest is ignored.
pub const MAX_QUERY_CHARS: usize = 2_000;
/// Deepest parenthesis nesting; deeper groups are flattened.
pub const MAX_DEPTH: usize = 24;
/// Most terms in one query; later terms are ignored.
pub const MAX_TERMS: usize = 128;

/// A parsed query.
#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    /// Matches every item (the empty query).
    All,
    /// Matches no item.
    Nothing,
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    Term(Term),
}

/// Text fields a word can be matched in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextField {
    /// Free text: subject, people, body and attachment names.
    Any,
    From,
    To,
    Cc,
    /// To or Cc.
    ToCc,
    /// From, To or Cc.
    Participants,
    Subject,
    Body,
    Attachments,
}

/// Bits of the indexed text fields.
pub mod field {
    pub const SUBJECT: u8 = 1;
    pub const FROM: u8 = 2;
    pub const TO: u8 = 4;
    pub const CC: u8 = 8;
    pub const BODY: u8 = 16;
    pub const ATTACHMENTS: u8 = 32;
    /// Every text field, in index order.
    pub const ALL: [u8; 6] = [SUBJECT, FROM, TO, CC, BODY, ATTACHMENTS];
}

impl TextField {
    /// Indexed fields this covers.
    pub fn mask(self) -> u8 {
        use field::*;
        match self {
            TextField::Any => SUBJECT | FROM | TO | CC | BODY | ATTACHMENTS,
            TextField::From => FROM,
            TextField::To => TO,
            TextField::Cc => CC,
            TextField::ToCc => TO | CC,
            TextField::Participants => FROM | TO | CC,
            TextField::Subject => SUBJECT,
            TextField::Body => BODY,
            TextField::Attachments => ATTACHMENTS,
        }
    }

    fn key(self) -> Option<Key> {
        Some(match self {
            TextField::Any => return None,
            TextField::From => Key::From,
            TextField::To => Key::To,
            TextField::Cc => Key::Cc,
            TextField::ToCc => Key::ToCc,
            TextField::Participants => Key::Participants,
            TextField::Subject => Key::Subject,
            TextField::Body => Key::Body,
            TextField::Attachments => Key::Attachments,
        })
    }
}

/// Words to find: folded tokens, in a row when more than one, the last a prefix when `prefix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    /// The text as written (without quotes).
    pub raw: String,
    pub quoted: bool,
    /// Folded word tokens (never empty).
    pub words: Vec<String>,
    /// The last word matches as a prefix.
    pub prefix: bool,
}

impl Text {
    /// `None` when `raw` holds no word characters.
    pub fn new(raw: &str, quoted: bool) -> Option<Text> {
        let raw = raw.trim();
        let words = words(raw);
        if words.is_empty() {
            return None;
        }
        let prefix = !quoted || raw.ends_with('*');
        Some(Text { raw: raw.to_owned(), quoted, words, prefix })
    }
}

/// Which categories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CategoryMatch {
    Any,
    None,
    Named(Text),
}

/// Date of an item a date term looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateField {
    Received,
    /// Sent (the received date for items without one).
    Sent,
}

/// Named relative date ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelDate {
    Today,
    Yesterday,
    Tomorrow,
    ThisWeek,
    LastWeek,
    NextWeek,
    ThisMonth,
    LastMonth,
    NextMonth,
    ThisYear,
    LastYear,
    NextYear,
}

const REL_NAMES: [(RelDate, &str); 12] = [
    (RelDate::Today, "today"),
    (RelDate::Yesterday, "yesterday"),
    (RelDate::Tomorrow, "tomorrow"),
    (RelDate::ThisWeek, "this week"),
    (RelDate::LastWeek, "last week"),
    (RelDate::NextWeek, "next week"),
    (RelDate::ThisMonth, "this month"),
    (RelDate::LastMonth, "last month"),
    (RelDate::NextMonth, "next month"),
    (RelDate::ThisYear, "this year"),
    (RelDate::LastYear, "last year"),
    (RelDate::NextYear, "next year"),
];

/// Units of `N … ago`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgoUnit {
    Days,
    Weeks,
    Months,
    Years,
}

/// A point (really a day range) in time, resolved against "now" at evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePoint {
    /// Days since 1970-01-01 (local).
    Day(i64),
    Rel(RelDate),
    /// The day `n` units before today.
    Ago(u32, AgoUnit),
}

/// One end of a date range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    pub at: DatePoint,
    pub inclusive: bool,
}

/// Local-day range; a missing end is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateRange {
    pub lo: Option<Bound>,
    pub hi: Option<Bound>,
}

/// Days ahead that `expires:soon` looks (today counts as day 0).
pub const EXPIRING_SOON_DAYS: i64 = 7;

/// What an `expires:` value asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    /// `soon`: not expired yet, and expiring within [`EXPIRING_SOON_DAYS`] days.
    Soon,
    /// `yes` / `any`: carries an expiry date.
    Any,
    /// `no` / `none` / `never`: carries none.
    Never,
    /// `expired` / `past`: its expiry date has passed.
    Past,
    /// Its expiry date falls in the range (local days).
    Range(DateRange),
}

impl Expiry {
    /// Parses an `expires:` value (any case); `None` when it means nothing.
    pub fn parse(v: &str) -> Option<Expiry> {
        let v = v.trim().to_lowercase();
        Some(match v.as_str() {
            "soon" | "expiringsoon" => Expiry::Soon,
            "yes" | "any" | "true" => Expiry::Any,
            "no" | "none" | "never" | "false" => Expiry::Never,
            "expired" | "past" => Expiry::Past,
            other => Expiry::Range(parse_date_range(other)?),
        })
    }
}

/// The observed size buckets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeBucket {
    Tiny,
    Small,
    Medium,
    Large,
    VeryLarge,
    Huge,
}

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * 1024 * 1024;

impl SizeBucket {
    const ALL: [(SizeBucket, &'static str); 6] = [
        (SizeBucket::Tiny, "tiny"),
        (SizeBucket::Small, "small"),
        (SizeBucket::Medium, "medium"),
        (SizeBucket::Large, "large"),
        (SizeBucket::VeryLarge, "verylarge"),
        (SizeBucket::Huge, "huge"),
    ];

    pub fn name(self) -> &'static str {
        SizeBucket::ALL.iter().find(|(b, _)| *b == self).map_or("", |(_, n)| n)
    }

    /// Inclusive byte range.
    pub fn range(self) -> (u64, u64) {
        match self {
            SizeBucket::Tiny => (0, 10 * KB - 1),
            SizeBucket::Small => (10 * KB, 25 * KB - 1),
            SizeBucket::Medium => (25 * KB, 100 * KB - 1),
            SizeBucket::Large => (100 * KB, 500 * KB - 1),
            SizeBucket::VeryLarge => (500 * KB, MB - 1),
            SizeBucket::Huge => (MB, u64::MAX),
        }
    }
}

/// A size condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeSpec {
    Bucket(SizeBucket),
    /// Inclusive byte range; a missing end is open.
    Range {
        lo: Option<u64>,
        hi: Option<u64>,
    },
}

impl SizeSpec {
    pub fn contains(self, size: u64) -> bool {
        let (lo, hi) = match self {
            SizeSpec::Bucket(b) => b.range(),
            SizeSpec::Range { lo, hi } => (lo.unwrap_or(0), hi.unwrap_or(u64::MAX)),
        };
        size >= lo && size <= hi
    }
}

/// One condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    Text(TextField, Text),
    /// A people field holds one of the user's own addresses.
    Me(TextField),
    Category(CategoryMatch),
    /// Items in folders whose name matches.
    Folder(Text),
    HasAttachments(bool),
    Read(bool),
    Flagged(bool),
    Importance(Level),
    Date(DateField, DateRange),
    Size(SizeSpec),
    Kind(DocKind),
    /// `expires:` (see [`Expiry`]; a value it does not understand matches nothing). Holds the value as
    /// written.
    Expires(String),
    /// A form-field condition on an item property (`prop:"Sensitivity=Private"`, `conversation:"Lunch"`).
    Prop(PropTest),
    /// A value the key does not understand (matches nothing). Holds the value as written.
    Invalid(Key, String),
}

/// Query keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    From,
    To,
    Cc,
    ToCc,
    Participants,
    Subject,
    Body,
    Attachments,
    Category,
    Folder,
    HasAttachments,
    IsRead,
    IsFlagged,
    Importance,
    Received,
    Sent,
    Size,
    Kind,
    Expires,
    Prop,
    Conversation,
}

const KEY_NAMES: [(&str, Key); 26] = [
    ("from", Key::From),
    ("to", Key::To),
    ("cc", Key::Cc),
    ("tocc", Key::ToCc),
    ("participants", Key::Participants),
    ("subject", Key::Subject),
    ("body", Key::Body),
    ("attachments", Key::Attachments),
    ("attachment", Key::Attachments),
    ("category", Key::Category),
    ("categories", Key::Category),
    ("folder", Key::Folder),
    ("hasattachments", Key::HasAttachments),
    ("hasattachment", Key::HasAttachments),
    ("isread", Key::IsRead),
    ("isflagged", Key::IsFlagged),
    ("importance", Key::Importance),
    ("received", Key::Received),
    ("sent", Key::Sent),
    ("size", Key::Size),
    ("messagesize", Key::Size),
    ("kind", Key::Kind),
    ("expires", Key::Expires),
    ("prop", Key::Prop),
    ("conversation", Key::Conversation),
    ("conversationtopic", Key::Conversation),
];

/// How a form-field condition compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropOp {
    /// `=`: equal (case-insensitive); with an empty value, "is empty".
    Is,
    /// `!=`: not equal; with an empty value, "is not empty".
    IsNot,
    /// `~`: contains.
    Contains,
    /// `!~`: does not contain.
    NotContains,
    /// `<`: less than (numbers, else text order, which fits `YYYY-MM-DD` dates).
    Less,
    /// `>`: more than.
    More,
}

impl PropOp {
    /// Operators in the order they are tried when parsing (longest first).
    const ALL: [(&'static str, PropOp); 6] = [
        ("!=", PropOp::IsNot),
        ("!~", PropOp::NotContains),
        ("=", PropOp::Is),
        ("~", PropOp::Contains),
        ("<", PropOp::Less),
        (">", PropOp::More),
    ];

    pub fn symbol(self) -> &'static str {
        PropOp::ALL.iter().find(|(_, o)| *o == self).map_or("=", |(s, _)| s)
    }
}

/// `name op value` on an item property (see `Doc::props`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropTest {
    /// Property id as written by [`crate::doc::prop_id`] ("messageclass").
    pub name: String,
    /// The name as written (kept for display).
    pub label: String,
    pub op: PropOp,
    pub value: String,
}

/// Longest property name or value in a condition.
const MAX_PROP_CHARS: usize = 256;

impl PropTest {
    /// Parses `Name<op>Value` (`Sensitivity=Private`, `Due Date<2026-11-01`, `Company!=`).
    pub fn parse(v: &str) -> Option<PropTest> {
        let (at, op, sym) = PropOp::ALL
            .iter()
            .filter_map(|(s, o)| v.find(s).map(|i| (i, *o, *s)))
            .min_by_key(|(i, _, s)| (*i, std::cmp::Reverse(s.len())))?;
        let label = v.get(..at)?.trim();
        let value = v.get(at.checked_add(sym.len())?..)?.trim();
        let name = crate::doc::prop_id(label);
        if name.is_empty() || label.chars().count() > MAX_PROP_CHARS || value.chars().count() > MAX_PROP_CHARS
        {
            return None;
        }
        Some(PropTest { name, label: label.to_owned(), op, value: value.to_owned() })
    }

    /// True when the property in `props` satisfies the condition (a missing property is empty).
    pub fn matches<K: AsRef<str>, V: AsRef<str>>(&self, props: &[(K, V)]) -> bool {
        let actual = props.iter().find(|(k, _)| k.as_ref() == self.name).map_or("", |(_, v)| v.as_ref());
        let a = crate::fold::fold(actual.trim());
        let want = crate::fold::fold(&self.value);
        match self.op {
            PropOp::Is => a == want,
            PropOp::IsNot => a != want,
            PropOp::Contains => !want.is_empty() && a.contains(&want),
            PropOp::NotContains => want.is_empty() || !a.contains(&want),
            PropOp::Less | PropOp::More => {
                if a.is_empty() || want.is_empty() {
                    return false;
                }
                let ord = match (a.parse::<f64>(), want.parse::<f64>()) {
                    (Ok(x), Ok(y)) => x.partial_cmp(&y),
                    _ => Some(a.as_str().cmp(want.as_str())),
                };
                match ord {
                    Some(std::cmp::Ordering::Less) => self.op == PropOp::Less,
                    Some(std::cmp::Ordering::Greater) => self.op == PropOp::More,
                    _ => false,
                }
            }
        }
    }
}

impl Key {
    /// Canonical name (first in the alias table).
    pub fn name(self) -> &'static str {
        KEY_NAMES.iter().find(|(_, k)| *k == self).map_or("", |(n, _)| n)
    }

    /// Parses a key name (ASCII case-insensitive).
    pub fn parse(name: &str) -> Option<Key> {
        KEY_NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, k)| *k)
    }

    fn text_field(self) -> Option<TextField> {
        Some(match self {
            Key::From => TextField::From,
            Key::To => TextField::To,
            Key::Cc => TextField::Cc,
            Key::ToCc => TextField::ToCc,
            Key::Participants => TextField::Participants,
            Key::Subject => TextField::Subject,
            Key::Body => TextField::Body,
            Key::Attachments => TextField::Attachments,
            _ => return None,
        })
    }

    fn is_people(self) -> bool {
        matches!(self, Key::From | Key::To | Key::Cc | Key::ToCc | Key::Participants)
    }

    fn is_date(self) -> bool {
        matches!(self, Key::Received | Key::Sent)
    }
}

// ---------------------------------------------------------------------------------------------------------
// Smart constructors (keep trees normalised so Display → parse is the identity)

/// AND of `parts` (flattened; `All` dropped; any `Nothing` wins).
pub fn and(parts: Vec<Query>) -> Query {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            Query::All => {}
            Query::Nothing => return Query::Nothing,
            Query::And(inner) => out.extend(inner),
            q => out.push(q),
        }
    }
    match out.len() {
        0 => Query::All,
        1 => out.pop().unwrap_or(Query::All),
        _ => Query::And(out),
    }
}

/// OR of `parts` (flattened; `Nothing` dropped; any `All` wins).
pub fn or(parts: Vec<Query>) -> Query {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            Query::Nothing => {}
            Query::All => return Query::All,
            Query::Or(inner) => out.extend(inner),
            q => out.push(q),
        }
    }
    match out.len() {
        0 => Query::Nothing,
        1 => out.pop().unwrap_or(Query::Nothing),
        _ => Query::Or(out),
    }
}

/// NOT `q` (double negation and constants folded).
pub fn not(q: Query) -> Query {
    match q {
        Query::All => Query::Nothing,
        Query::Nothing => Query::All,
        Query::Not(inner) => *inner,
        q => Query::Not(Box::new(q)),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Values

fn yes_no(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Some(true),
        "no" | "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn rel_of(v: &str) -> Option<RelDate> {
    REL_NAMES.iter().find(|(_, n)| *n == v).map(|(r, _)| *r)
}

fn rel_name(r: RelDate) -> &'static str {
    REL_NAMES.iter().find(|(x, _)| *x == r).map_or("today", |(_, n)| n)
}

/// Collapses runs of whitespace and lower-cases.
fn squash(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn parse_num(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_point(v: &str) -> Option<DatePoint> {
    let v = squash(v);
    if let Some(r) = rel_of(&v) {
        return Some(DatePoint::Rel(r));
    }
    let parts: Vec<&str> = v.split(' ').collect();
    if let [n, unit, "ago"] = parts.as_slice() {
        let n = u32::try_from(parse_num(n)?).ok()?;
        let unit = match *unit {
            "day" | "days" => AgoUnit::Days,
            "week" | "weeks" => AgoUnit::Weeks,
            "month" | "months" => AgoUnit::Months,
            "year" | "years" => AgoUnit::Years,
            _ => return None,
        };
        return Some(DatePoint::Ago(n.min(100_000), unit));
    }
    let nums: Vec<&str> = v.split(['-', '/', '.']).collect();
    let [a, b, c] = nums.as_slice() else { return None };
    let (y, m, d) = if a.len() == 4 {
        (parse_num(a)?, parse_num(b)?, parse_num(c)?)
    } else {
        // US order (M/D/YYYY, M/D/YY).
        let y = parse_num(c)?;
        let y = if c.len() <= 2 { 2000 + y } else { y };
        (y, parse_num(a)?, parse_num(b)?)
    };
    // Four-digit years only (what `Display` writes as YYYY-MM-DD).
    if y > 9_999 {
        return None;
    }
    let day = days_from_civil(y, u8::try_from(m).ok()?, u8::try_from(d).ok()?)?;
    Some(DatePoint::Day(day))
}

/// Splits a leading comparison operator: `(op, rest)`.
fn split_op(v: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = v.strip_prefix(op) {
            return (op, rest.trim_start());
        }
    }
    ("", v)
}

pub(crate) fn parse_date_range(v: &str) -> Option<DateRange> {
    let v = v.trim();
    if let Some((a, b)) = v.split_once("..") {
        let lo =
            if a.trim().is_empty() { None } else { Some(Bound { at: parse_point(a)?, inclusive: true }) };
        let hi =
            if b.trim().is_empty() { None } else { Some(Bound { at: parse_point(b)?, inclusive: true }) };
        if lo.is_none() && hi.is_none() {
            return None;
        }
        return Some(DateRange { lo, hi });
    }
    let (op, rest) = split_op(v);
    let at = parse_point(rest)?;
    let b = |inclusive| Some(Bound { at, inclusive });
    Some(match op {
        ">=" => DateRange { lo: b(true), hi: None },
        ">" => DateRange { lo: b(false), hi: None },
        "<=" => DateRange { lo: None, hi: b(true) },
        "<" => DateRange { lo: None, hi: b(false) },
        _ => DateRange { lo: b(true), hi: b(true) },
    })
}

fn parse_bytes(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let mult = match unit.trim() {
        "" | "b" | "bytes" => 1,
        "k" | "kb" => KB,
        "m" | "mb" => MB,
        "g" | "gb" => GB,
        _ => return None,
    };
    // Up to u64::MAX written out (what `Display` prints for a saturated size); `parse` rejects overflow.
    if num.is_empty() || num.len() > 24 || num.matches('.').count() > 1 || num == "." {
        return None;
    }
    let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let mut out = whole.saturating_mul(mult);
    // Fractions (1.5MB): up to 3 decimals, in integer arithmetic.
    let frac: String = frac.chars().take(3).collect();
    if !frac.is_empty() {
        let f: u64 = frac.parse().ok()?;
        let scale = 10u64.pow(u32::try_from(frac.len()).unwrap_or(3));
        out = out.saturating_add(f.saturating_mul(mult) / scale);
    }
    Some(out)
}

fn parse_size(v: &str) -> Option<SizeSpec> {
    let v = squash(v);
    let compact = v.replace(' ', "");
    if let Some((b, _)) = SizeBucket::ALL.iter().find(|(_, n)| *n == compact) {
        return Some(SizeSpec::Bucket(*b));
    }
    if let Some((a, b)) = v.split_once("..") {
        let lo = if a.trim().is_empty() { None } else { Some(parse_bytes(a)?) };
        let hi = if b.trim().is_empty() { None } else { Some(parse_bytes(b)?) };
        if lo.is_none() && hi.is_none() {
            return None;
        }
        return Some(SizeSpec::Range { lo, hi });
    }
    let (op, rest) = split_op(&v);
    let n = parse_bytes(rest)?;
    Some(match op {
        "<=" => SizeSpec::Range { lo: None, hi: Some(n) },
        "<" => match n.checked_sub(1) {
            Some(h) => SizeSpec::Range { lo: None, hi: Some(h) },
            None => SizeSpec::Range { lo: Some(1), hi: Some(0) },
        },
        ">" => SizeSpec::Range { lo: Some(n.saturating_add(1)), hi: None },
        _ => SizeSpec::Range { lo: Some(n), hi: None },
    })
}

/// The condition `key:value`.
pub fn make_term(key: Key, value: &str, quoted: bool) -> Query {
    let value = value.trim();
    if value.is_empty() {
        return Query::All;
    }
    let invalid = || Query::Term(Term::Invalid(key, value.to_owned()));
    if let Some(field) = key.text_field() {
        if key.is_people() && !quoted && value.eq_ignore_ascii_case("me") {
            return Query::Term(Term::Me(field));
        }
        return Text::new(value, quoted).map_or(Query::All, |t| Query::Term(Term::Text(field, t)));
    }
    let lower = squash(value);
    let term = match key {
        Key::Category => match lower.as_str() {
            "any" if !quoted => Term::Category(CategoryMatch::Any),
            "none" if !quoted => Term::Category(CategoryMatch::None),
            _ => match Text::new(value, quoted) {
                Some(t) => Term::Category(CategoryMatch::Named(t)),
                None => return Query::All,
            },
        },
        Key::Folder => match Text::new(value, quoted) {
            Some(t) => Term::Folder(t),
            None => return Query::All,
        },
        Key::HasAttachments => match yes_no(&lower) {
            Some(b) => Term::HasAttachments(b),
            None => return invalid(),
        },
        Key::IsRead => match yes_no(&lower) {
            Some(b) => Term::Read(b),
            None => return invalid(),
        },
        Key::IsFlagged => match yes_no(&lower) {
            Some(b) => Term::Flagged(b),
            None => return invalid(),
        },
        Key::Importance => match lower.as_str() {
            "high" => Term::Importance(Level::High),
            "normal" => Term::Importance(Level::Normal),
            "low" => Term::Importance(Level::Low),
            _ => return invalid(),
        },
        Key::Received | Key::Sent => match parse_date_range(&lower) {
            Some(r) => Term::Date(if key == Key::Sent { DateField::Sent } else { DateField::Received }, r),
            None => return invalid(),
        },
        Key::Size => match parse_size(&lower) {
            Some(s) => Term::Size(s),
            None => return invalid(),
        },
        Key::Kind => match DocKind::parse(&lower) {
            Some(k) => Term::Kind(k),
            None => return invalid(),
        },
        Key::Expires => Term::Expires(value.to_owned()),
        Key::Prop => match PropTest::parse(value) {
            Some(p) => Term::Prop(p),
            None => return invalid(),
        },
        Key::Conversation => Term::Prop(PropTest {
            name: "conversation".into(),
            label: "conversation".into(),
            op: PropOp::Is,
            value: mailcraft_model::subject::conversation_topic(value),
        }),
        // Text keys returned above.
        _ => return invalid(),
    };
    Query::Term(term)
}

// ---------------------------------------------------------------------------------------------------------
// Parser

struct Parser {
    s: Vec<char>,
    pos: usize,
    depth: usize,
    terms: usize,
}

fn is_break(c: char) -> bool {
    c.is_whitespace() || c == '(' || c == ')' || c == '"'
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<char> {
        self.s.get(self.pos.saturating_add(off)).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// Length of the word at the cursor.
    fn word_len(&self) -> usize {
        let mut n = 0;
        while self.peek_at(n).is_some_and(|c| !is_break(c)) {
            n += 1;
        }
        n
    }

    fn word_at(&self) -> String {
        let n = self.word_len();
        self.s.get(self.pos..self.pos.saturating_add(n)).map(|w| w.iter().collect()).unwrap_or_default()
    }

    fn read_word(&mut self) -> String {
        let w = self.word_at();
        self.pos = self.pos.saturating_add(w.chars().count());
        w
    }

    /// Reads `"…"` (cursor on the opening quote); an unclosed quote runs to the end.
    fn read_quoted(&mut self) -> String {
        self.pos += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '"' {
                break;
            }
            out.push(c);
        }
        out
    }

    /// The operator at the cursor (`AND`, `OR`, `NOT`), if any.
    fn op_at(&self) -> Option<&'static str> {
        let w = self.word_at();
        ["AND", "OR", "NOT"].into_iter().find(|op| *op == w)
    }

    fn parse_or(&mut self, ctx: Option<Key>) -> Option<Query> {
        let mut alts = Vec::new();
        loop {
            if let Some(a) = self.parse_and(ctx) {
                alts.push(a);
            }
            self.skip_ws();
            if self.op_at() == Some("OR") {
                self.pos += 2;
                continue;
            }
            break;
        }
        if alts.is_empty() { None } else { Some(or(alts)) }
    }

    fn parse_and(&mut self, ctx: Option<Key>) -> Option<Query> {
        let mut parts = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                // A stray closing parenthesis at the top level is ignored.
                Some(')') if self.depth == 0 => {
                    self.pos += 1;
                    continue;
                }
                Some(')') => break,
                _ => {}
            }
            match self.op_at() {
                Some("OR") => break,
                Some("AND") => {
                    self.pos += 3;
                    continue;
                }
                _ => {}
            }
            if let Some(u) = self.parse_unary(ctx) {
                parts.push(u);
            }
        }
        if parts.is_empty() { None } else { Some(and(parts)) }
    }

    /// Negations then a primary. Always consumes at least one char when not at end, `)`, AND or OR.
    fn parse_unary(&mut self, ctx: Option<Key>) -> Option<Query> {
        let mut negate = false;
        loop {
            self.skip_ws();
            if self.op_at() == Some("NOT") {
                self.pos += 3;
                negate = !negate;
                continue;
            }
            if self.peek() == Some('-') && self.peek_at(1).is_some_and(|c| !c.is_whitespace() && c != ')') {
                self.pos += 1;
                negate = !negate;
                continue;
            }
            break;
        }
        match self.peek() {
            None | Some(')') => return None,
            _ => {}
        }
        if matches!(self.op_at(), Some("AND" | "OR")) {
            return None;
        }
        let q = self.parse_primary(ctx)?;
        Some(if negate { not(q) } else { q })
    }

    fn count_term(&mut self) -> bool {
        self.terms = self.terms.saturating_add(1);
        self.terms <= MAX_TERMS
    }

    fn group(&mut self, ctx: Option<Key>) -> Option<Query> {
        // Cursor on '('.
        self.pos += 1;
        if self.depth >= MAX_DEPTH {
            // Too deep: the parenthesis is ignored and its contents join the enclosing group.
            return None;
        }
        self.depth += 1;
        let q = self.parse_or(ctx);
        self.depth -= 1;
        self.skip_ws();
        if self.peek() == Some(')') {
            self.pos += 1;
        }
        Some(q.unwrap_or(Query::All))
    }

    fn value_term(&mut self, ctx: Option<Key>, value: &str, quoted: bool) -> Option<Query> {
        if !self.count_term() {
            return None;
        }
        Some(match ctx {
            Some(key) => make_term(key, value, quoted),
            None => {
                Text::new(value, quoted).map_or(Query::All, |t| Query::Term(Term::Text(TextField::Any, t)))
            }
        })
    }

    fn parse_primary(&mut self, ctx: Option<Key>) -> Option<Query> {
        match self.peek()? {
            '(' => return self.group(ctx),
            '"' => {
                let v = self.read_quoted();
                return self.value_term(ctx, &v, true);
            }
            ')' => return None,
            _ => {}
        }
        let word = self.read_word();
        let keyed = word.split_once(':').and_then(|(k, rest)| Key::parse(k).map(|k| (k, rest.to_owned())));
        let Some((key, rest)) = keyed else {
            // Inside `received:( … )` bare words are dates: `received:(this week OR 3 days ago)`.
            let mut word = word;
            if ctx.is_some_and(Key::is_date) {
                self.merge_date_words(&mut word);
            }
            return self.value_term(ctx, &word, false);
        };
        if rest.is_empty() {
            return match self.peek() {
                Some('"') => {
                    let v = self.read_quoted();
                    self.value_term(Some(key), &v, true)
                }
                Some('(') => self.group(Some(key)),
                _ => self.value_term(Some(key), "", false),
            };
        }
        if rest.ends_with(['<', '>', '=', '.']) && self.peek() == Some('"') {
            let v = self.read_quoted();
            return self.value_term(Some(key), &format!("{rest}{v}"), true);
        }
        let mut value = rest;
        if key.is_date() {
            self.merge_date_words(&mut value);
        }
        self.value_term(Some(key), &value, false)
    }

    /// `received:this week`, `received:>=last month`, `received:3 days ago`, `received:2026-01-01..this week`,
    /// `received:last week..today`: joins the following words that belong to an unquoted date (on either
    /// side of a `..` range).
    fn merge_date_words(&mut self, value: &mut String) {
        const UNITS: [&str; 8] = ["day", "days", "week", "weeks", "month", "months", "year", "years"];
        // A range has two sides, each at most "N units ago": a handful of joins at most.
        for _ in 0..6 {
            let tail = value.rsplit(['>', '<', '=', '.', ' ']).next().unwrap_or("").to_ascii_lowercase();
            let wants: &[&str] = match tail.as_str() {
                "this" | "last" | "next" => &["week", "month", "year"],
                t if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => &UNITS,
                t if UNITS.contains(&t) && value.split(' ').count() >= 2 && !value.ends_with(" ago") => {
                    // "N days" is followed by "ago" — but "this week" is complete.
                    let before = value.rsplit(' ').nth(1).unwrap_or("");
                    let before = before.rsplit(['>', '<', '=', '.']).next().unwrap_or("");
                    if before.is_empty() || !before.bytes().all(|b| b.is_ascii_digit()) {
                        return;
                    }
                    &["ago"]
                }
                _ => return,
            };
            let save = self.pos;
            self.skip_ws();
            let next = self.word_at();
            // The word itself, or the word followed by the other side of a range (`week..today`).
            let head = next.split_once("..").map_or(next.as_str(), |(h, _)| h);
            if !wants.iter().any(|w| head.eq_ignore_ascii_case(w)) {
                self.pos = save;
                return;
            }
            self.read_word();
            value.push(' ');
            value.push_str(&next);
        }
    }
}

/// Parses a query (never fails; see the module docs).
pub fn parse(text: &str) -> Query {
    let s: Vec<char> = text.chars().take(MAX_QUERY_CHARS).collect();
    let mut p = Parser { s, pos: 0, depth: 0, terms: 0 };
    let mut parts = Vec::new();
    // `parse_or` stops only at the end (stray `)` are skipped at depth 0); loop defensively anyway.
    while p.pos < p.s.len() {
        let before = p.pos;
        if let Some(q) = p.parse_or(None) {
            parts.push(q);
        }
        if p.pos == before {
            p.pos += 1;
        }
    }
    and(parts)
}

// ---------------------------------------------------------------------------------------------------------
// Display

fn needs_quotes(v: &str) -> bool {
    v.is_empty()
        || v.chars().any(is_break)
        || matches!(v, "AND" | "OR" | "NOT")
        || v.starts_with('-')
        || v.contains(':')
}

fn write_value(f: &mut fmt::Formatter<'_>, v: &str, quoted: bool) -> fmt::Result {
    if quoted || needs_quotes(v) { write!(f, "\"{v}\"") } else { f.write_str(v) }
}

/// A text value: quoted as written, else the bare word (which by construction holds no break characters).
fn write_text(f: &mut fmt::Formatter<'_>, t: &Text) -> fmt::Result {
    if t.quoted { write!(f, "\"{}\"", t.raw) } else { f.write_str(&t.raw) }
}

fn point_text(p: DatePoint) -> String {
    match p {
        DatePoint::Day(d) => {
            let (y, m, dd) = civil_from_days(d);
            format!("{y:04}-{m:02}-{dd:02}")
        }
        DatePoint::Rel(r) => rel_name(r).to_owned(),
        DatePoint::Ago(n, u) => {
            let unit = match u {
                AgoUnit::Days => "days",
                AgoUnit::Weeks => "weeks",
                AgoUnit::Months => "months",
                AgoUnit::Years => "years",
            };
            format!("{n} {unit} ago")
        }
    }
}

fn range_text(r: &DateRange) -> String {
    match (r.lo, r.hi) {
        (Some(a), Some(b)) if a == b && a.inclusive => point_text(a.at),
        (Some(a), Some(b)) => format!("{}..{}", point_text(a.at), point_text(b.at)),
        (Some(a), None) => format!("{}{}", if a.inclusive { ">=" } else { ">" }, point_text(a.at)),
        (None, Some(b)) => format!("{}{}", if b.inclusive { "<=" } else { "<" }, point_text(b.at)),
        (None, None) => String::new(),
    }
}

fn bytes_text(n: u64) -> String {
    if n > 0 && n.is_multiple_of(MB) {
        format!("{}MB", n / MB)
    } else if n > 0 && n.is_multiple_of(KB) {
        format!("{}KB", n / KB)
    } else {
        n.to_string()
    }
}

fn size_text(s: SizeSpec) -> String {
    match s {
        SizeSpec::Bucket(b) => b.name().to_owned(),
        SizeSpec::Range { lo: Some(a), hi: None } => format!(">={}", bytes_text(a)),
        SizeSpec::Range { lo: None, hi: Some(b) } => format!("<={}", bytes_text(b)),
        SizeSpec::Range { lo, hi } => {
            format!("{}..{}", lo.map(bytes_text).unwrap_or_default(), hi.map(bytes_text).unwrap_or_default())
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Term::Text(field, t) => {
                if let Some(k) = field.key() {
                    write!(f, "{}:", k.name())?;
                }
                write_text(f, t)
            }
            Term::Me(field) => write!(f, "{}:me", field.key().map_or("participants", Key::name)),
            Term::Category(CategoryMatch::Any) => f.write_str("category:any"),
            Term::Category(CategoryMatch::None) => f.write_str("category:none"),
            Term::Category(CategoryMatch::Named(t)) => {
                f.write_str("category:")?;
                write_text(f, t)
            }
            Term::Folder(t) => {
                f.write_str("folder:")?;
                write_text(f, t)
            }
            Term::HasAttachments(b) => write!(f, "hasattachments:{}", if *b { "yes" } else { "no" }),
            Term::Read(b) => write!(f, "isread:{}", if *b { "yes" } else { "no" }),
            Term::Flagged(b) => write!(f, "isflagged:{}", if *b { "yes" } else { "no" }),
            Term::Importance(l) => write!(f, "importance:{}", l.name()),
            Term::Date(field, r) => {
                f.write_str(if *field == DateField::Sent { "sent:" } else { "received:" })?;
                write_value(f, &range_text(r), false)
            }
            Term::Size(s) => {
                f.write_str("size:")?;
                write_value(f, &size_text(*s), false)
            }
            Term::Kind(k) => write!(f, "kind:{}", k.name()),
            Term::Expires(v) => {
                f.write_str("expires:")?;
                write_value(f, v, true)
            }
            Term::Prop(p) if p.name == "conversation" && p.op == PropOp::Is => {
                f.write_str("conversation:")?;
                write_value(f, &p.value, true)
            }
            Term::Prop(p) => {
                f.write_str("prop:")?;
                write_value(f, &format!("{}{}{}", p.label, p.op.symbol(), p.value), true)
            }
            Term::Invalid(k, v) => {
                write!(f, "{}:", k.name())?;
                write_value(f, v, true)
            }
        }
    }
}

impl Query {
    fn fmt_atom(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Query::And(_) | Query::Or(_) => write!(f, "({self})"),
            q => write!(f, "{q}"),
        }
    }

    /// Every condition in the tree, with whether it is negated (odd number of NOTs above it).
    pub fn terms(&self) -> Vec<(&Term, bool)> {
        fn walk<'a>(q: &'a Query, neg: bool, out: &mut Vec<(&'a Term, bool)>) {
            match q {
                Query::All | Query::Nothing => {}
                Query::And(v) | Query::Or(v) => v.iter().for_each(|c| walk(c, neg, out)),
                Query::Not(c) => walk(c, !neg, out),
                Query::Term(t) => out.push((t, neg)),
            }
        }
        let mut out = Vec::new();
        walk(self, false, &mut out);
        out
    }

    /// True for the empty query.
    pub fn is_all(&self) -> bool {
        matches!(self, Query::All)
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Query::All => Ok(()),
            Query::Nothing => f.write_str("NOT ()"),
            Query::Term(t) => write!(f, "{t}"),
            Query::Not(q) => {
                f.write_str("NOT ")?;
                q.fmt_atom(f)
            }
            Query::And(v) => {
                for (i, q) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    q.fmt_atom(f)?;
                }
                Ok(())
            }
            Query::Or(v) => {
                for (i, q) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" OR ")?;
                    }
                    q.fmt_atom(f)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(field: TextField, raw: &str, quoted: bool) -> Query {
        Query::Term(Term::Text(field, Text::new(raw, quoted).unwrap()))
    }

    #[test]
    fn words_and_operators() {
        assert_eq!(parse(""), Query::All);
        assert_eq!(parse("   "), Query::All);
        assert_eq!(parse("budget"), t(TextField::Any, "budget", false));
        assert_eq!(
            parse("budget report"),
            Query::And(vec![t(TextField::Any, "budget", false), t(TextField::Any, "report", false)])
        );
        assert_eq!(parse("budget AND report"), parse("budget report"));
        assert_eq!(
            parse("a OR b c"),
            Query::Or(vec![
                t(TextField::Any, "a", false),
                Query::And(vec![t(TextField::Any, "b", false), t(TextField::Any, "c", false)])
            ])
        );
        assert_eq!(parse("NOT a"), not(t(TextField::Any, "a", false)));
        assert_eq!(parse("-a"), not(t(TextField::Any, "a", false)));
        assert_eq!(parse("NOT NOT a"), t(TextField::Any, "a", false));
        assert_eq!(parse("a or b"), parse("a or b"));
        assert!(matches!(parse("a or b"), Query::And(v) if v.len() == 3));
        assert_eq!(
            parse("(a OR b) c"),
            Query::And(vec![
                Query::Or(vec![t(TextField::Any, "a", false), t(TextField::Any, "b", false)]),
                t(TextField::Any, "c", false)
            ])
        );
    }

    #[test]
    fn keys_and_values() {
        assert_eq!(parse("from:ann"), t(TextField::From, "ann", false));
        assert_eq!(parse("FROM:\"Ann Boss\""), t(TextField::From, "Ann Boss", true));
        assert_eq!(parse("from:me"), Query::Term(Term::Me(TextField::From)));
        assert_eq!(parse("from:\"me\""), t(TextField::From, "me", true));
        assert_eq!(parse("from:\"\""), Query::All);
        assert_eq!(parse("subject:"), Query::All);
        assert_eq!(parse("-from:\"\""), Query::Nothing);
        assert_eq!(parse("hasattachments:yes"), Query::Term(Term::HasAttachments(true)));
        assert_eq!(parse("isread:NO"), Query::Term(Term::Read(false)));
        assert_eq!(parse("isread:maybe"), Query::Term(Term::Invalid(Key::IsRead, "maybe".into())));
        assert_eq!(parse("importance:high"), Query::Term(Term::Importance(Level::High)));
        assert_eq!(parse("category:any"), Query::Term(Term::Category(CategoryMatch::Any)));
        assert_eq!(parse("kind:contact"), Query::Term(Term::Kind(DocKind::Contact)));
        assert_eq!(
            parse("from:(ann OR bob)"),
            Query::Or(vec![t(TextField::From, "ann", false), t(TextField::From, "bob", false)])
        );
        assert_eq!(parse("foo:bar"), t(TextField::Any, "foo:bar", false));
        assert_eq!(parse("attachment:report.pdf"), t(TextField::Attachments, "report.pdf", false));
    }

    #[test]
    fn dates() {
        let rel = |r| DateRange {
            lo: Some(Bound { at: DatePoint::Rel(r), inclusive: true }),
            hi: Some(Bound { at: DatePoint::Rel(r), inclusive: true }),
        };
        let d = |q: &str| match parse(q) {
            Query::Term(Term::Date(_, r)) => r,
            other => panic!("{q}: {other:?}"),
        };
        assert_eq!(d("received:today"), rel(RelDate::Today));
        assert_eq!(d("received:this week"), rel(RelDate::ThisWeek));
        assert_eq!(d("received:\"Last  Month\""), rel(RelDate::LastMonth));
        assert_eq!(d("received:this year"), rel(RelDate::ThisYear));
        let day = days_from_civil(2026, 10, 9).unwrap();
        assert_eq!(d("received:2026-10-09").lo.unwrap().at, DatePoint::Day(day));
        assert_eq!(d("received:10/9/2026").lo.unwrap().at, DatePoint::Day(day));
        let r = d("received:>=2026-10-09");
        assert_eq!((r.lo.unwrap().inclusive, r.hi), (true, None));
        let r = d("sent:<2026-10-09");
        assert_eq!((r.lo, r.hi.unwrap().inclusive), (None, false));
        let r = d("received:2026-01-01..2026-10-09");
        assert_eq!(r.hi.unwrap().at, DatePoint::Day(day));
        let r = d("received:<7 days ago");
        assert_eq!(r.hi.unwrap().at, DatePoint::Ago(7, AgoUnit::Days));
        assert!(matches!(parse("received:someday"), Query::Term(Term::Invalid(Key::Received, _))));
        assert!(matches!(parse("received:2026-02-30"), Query::Term(Term::Invalid(..))));
        // "this" followed by something else stays two terms.
        assert!(matches!(parse("received:this budget"), Query::And(_)));
        // Unquoted multi-word dates inside a key's parentheses and on either side of a range.
        assert_eq!(
            parse("received:(this week OR last week)"),
            or(vec![parse("received:\"this week\""), parse("received:\"last week\"")])
        );
        assert_eq!(parse("received:(3 days ago)"), parse("received:\"3 days ago\""));
        assert_eq!(parse("received:this week..today"), parse("received:\"this week..today\""));
        assert_eq!(parse("received:last month..this week"), parse("received:\"last month..this week\""));
        assert_eq!(parse("received:3 days ago..today"), parse("received:\"3 days ago..today\""));
        assert_eq!(parse("received:2026-01-01..last week"), parse("received:\"2026-01-01..last week\""));
        assert!(matches!(parse("received:(this week)"), Query::Term(Term::Date(..))));
    }

    #[test]
    fn sizes() {
        let s = |q: &str| match parse(q) {
            Query::Term(Term::Size(s)) => s,
            other => panic!("{q}: {other:?}"),
        };
        assert_eq!(s("size:large"), SizeSpec::Bucket(SizeBucket::Large));
        assert_eq!(s("size:\"very large\""), SizeSpec::Bucket(SizeBucket::VeryLarge));
        assert_eq!(s("size:>1MB"), SizeSpec::Range { lo: Some(MB + 1), hi: None });
        assert_eq!(s("size:<=500KB"), SizeSpec::Range { lo: None, hi: Some(500 * KB) });
        assert_eq!(s("size:10kb..1.5mb"), SizeSpec::Range { lo: Some(10 * KB), hi: Some(MB + MB / 2) });
        assert_eq!(s("size:2048"), SizeSpec::Range { lo: Some(2048), hi: None });
        assert!(SizeSpec::Bucket(SizeBucket::Huge).contains(2 * MB));
        assert!(!SizeSpec::Bucket(SizeBucket::Tiny).contains(10 * KB));
        assert!(matches!(parse("size:big"), Query::Term(Term::Invalid(..))));
    }

    #[test]
    fn tolerant_of_junk() {
        for q in [
            "(",
            ")",
            "\"",
            "((((a",
            "a)))",
            "NOT",
            "-",
            "--",
            "AND OR NOT",
            "from:(",
            "\"unclosed",
            ":",
            "a:b:c",
        ] {
            let parsed = parse(q);
            assert_eq!(parse(&parsed.to_string()), parsed, "{q:?} → {parsed}");
        }
        let deep = "(".repeat(500) + "a";
        assert_eq!(parse(&deep), t(TextField::Any, "a", false));
        let many = "-".repeat(10_000) + "a";
        let _ = parse(&many);
        let terms = "x ".repeat(1_000);
        assert!(matches!(parse(&terms), Query::And(v) if v.len() == MAX_TERMS));
    }

    #[test]
    fn display_round_trips() {
        for q in [
            "budget report",
            "from:\"Ann Boss\" OR (subject:plan -body:draft)",
            "received:\"this week\" size:>=1MB hasattachments:no",
            "category:\"Red category\" folder:inbox kind:email",
            "received:2026-01-01..2026-02-01 sent:<=yesterday",
            "category:\"any\" expires:soon isread:maybe",
            "\"OR\" \"-x\" \"a:b\"",
            "to:me cc:me tocc:me participants:me",
            // Values at the edges of what parses: canonical text must still parse back.
            "size:>999999999999999",
            "size:999999999999999GB",
            "size:<=18446744073709551615",
            "received:1/1/99999",
            "received:12/31/9999",
        ] {
            let parsed = parse(q);
            let shown = parsed.to_string();
            assert_eq!(parse(&shown), parsed, "{q} → {shown}");
        }
        assert_eq!(parse("a  AND  b").to_string(), "a b");
        assert_eq!(parse("received:this week").to_string(), "received:\"this week\"");
    }
}
