//! Evaluation context (clock, the user's addresses, folder names) and direct evaluation of a [`Query`]
//! against one [`Doc`] without an index. The index ([`crate::Index`]) gives the same answers; this path is
//! for single items (rules, a freshly composed draft) and is the reference the index is tested against.

use mailcraft_model::dates::{civil_from_days, days_from_civil, days_in_month, weekday_of_day};

use crate::doc::Doc;
use crate::fold::fold;
use crate::query::{
    AgoUnit, Bound, CategoryMatch, DateField, DatePoint, DateRange, EXPIRING_SOON_DAYS, Expiry, Query,
    RelDate, Term, Text, field,
};
use crate::token::words;

const DAY_SECS: i64 = 86_400;

/// What a query is evaluated against besides the items.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// "Now" (unix seconds) for relative dates.
    pub now: i64,
    /// Local offset from UTC in minutes (dates compare by local day).
    pub tz_offset_min: i32,
    /// The user's own addresses (`from:me`), any case.
    pub me: Vec<String>,
    /// Folder id → display name, for `folder:`.
    pub folders: Vec<(u64, String)>,
}

impl Context {
    pub fn new(now: i64, tz_offset_min: i32) -> Context {
        Context { now, tz_offset_min, ..Context::default() }
    }

    /// Local day number of a unix time.
    pub fn day_of(&self, unix: i64) -> i64 {
        unix.saturating_add(i64::from(self.tz_offset_min).saturating_mul(60)).div_euclid(DAY_SECS)
    }

    pub fn today(&self) -> i64 {
        self.day_of(self.now)
    }

    /// Folded `me` addresses.
    pub(crate) fn me_folded(&self) -> Vec<String> {
        self.me.iter().map(|m| fold(m.trim())).filter(|m| !m.is_empty()).collect()
    }

    /// Ids of folders whose name matches `t`.
    pub(crate) fn folders_matching(&self, t: &Text) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .folders
            .iter()
            .filter(|(_, name)| text_in_segment(t, &words(name)))
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Inclusive local-day range `(first, last)` of a point.
    fn point_days(&self, p: DatePoint) -> (i64, i64) {
        let t = self.today();
        let (year, month, _) = civil_from_days(t);
        let month_range = |delta: i64| {
            let idx = year.saturating_mul(12).saturating_add(i64::from(month) - 1).saturating_add(delta);
            let (y, m) = (idx.div_euclid(12), u8::try_from(idx.rem_euclid(12) + 1).unwrap_or(1));
            let first = days_from_civil(y, m, 1).unwrap_or(t);
            (first, first.saturating_add(i64::from(days_in_month(y, m)).saturating_sub(1)))
        };
        let year_range = |delta: i64| {
            let y = year.saturating_add(delta);
            (days_from_civil(y, 1, 1).unwrap_or(t), days_from_civil(y, 12, 31).unwrap_or(t))
        };
        let week_start = t.saturating_sub(i64::from(weekday_of_day(t).0));
        let week = |delta: i64| {
            let s = week_start.saturating_add(delta.saturating_mul(7));
            (s, s.saturating_add(6))
        };
        match p {
            DatePoint::Day(d) => (d, d),
            DatePoint::Rel(r) => match r {
                RelDate::Today => (t, t),
                RelDate::Yesterday => (t - 1, t - 1),
                RelDate::Tomorrow => (t + 1, t + 1),
                RelDate::ThisWeek => week(0),
                RelDate::LastWeek => week(-1),
                RelDate::NextWeek => week(1),
                RelDate::ThisMonth => month_range(0),
                RelDate::LastMonth => month_range(-1),
                RelDate::NextMonth => month_range(1),
                RelDate::ThisYear => year_range(0),
                RelDate::LastYear => year_range(-1),
                RelDate::NextYear => year_range(1),
            },
            DatePoint::Ago(n, unit) => {
                let n = i64::from(n);
                let d = match unit {
                    AgoUnit::Days => t.saturating_sub(n),
                    AgoUnit::Weeks => t.saturating_sub(n.saturating_mul(7)),
                    AgoUnit::Months | AgoUnit::Years => {
                        let months = if unit == AgoUnit::Years { n.saturating_mul(12) } else { n };
                        let (_, _, day) = civil_from_days(t);
                        let idx = year
                            .saturating_mul(12)
                            .saturating_add(i64::from(month) - 1)
                            .saturating_sub(months);
                        let (y, m) = (idx.div_euclid(12), u8::try_from(idx.rem_euclid(12) + 1).unwrap_or(1));
                        days_from_civil(y, m, day.min(days_in_month(y, m))).unwrap_or(t)
                    }
                };
                (d, d)
            }
        }
    }

    /// Inclusive local-day bounds of a range (`i64::MIN`/`MAX` for open ends).
    pub fn day_bounds(&self, r: &DateRange) -> (i64, i64) {
        let lo = match r.lo {
            None => i64::MIN,
            Some(Bound { at, inclusive: true }) => self.point_days(at).0,
            Some(Bound { at, inclusive: false }) => self.point_days(at).1.saturating_add(1),
        };
        let hi = match r.hi {
            None => i64::MAX,
            Some(Bound { at, inclusive: true }) => self.point_days(at).1,
            Some(Bound { at, inclusive: false }) => self.point_days(at).0.saturating_sub(1),
        };
        (lo, hi)
    }
}

impl Context {
    /// True when an item expiring at `expires` (unix seconds) matches `e`.
    pub fn expiry_matches(&self, e: &Expiry, expires: Option<i64>) -> bool {
        match (e, expires) {
            (Expiry::Any, x) => x.is_some(),
            (Expiry::Never, x) => x.is_none(),
            (_, None) => false,
            (Expiry::Past, Some(x)) => x <= self.now,
            (Expiry::Soon, Some(x)) => {
                x > self.now && self.day_of(x) <= self.today().saturating_add(EXPIRING_SOON_DAYS)
            }
            (Expiry::Range(r), Some(x)) => {
                let (lo, hi) = self.day_bounds(r);
                let day = self.day_of(x);
                day >= lo && day <= hi
            }
        }
    }
}

/// True when `word` (query) matches `token` (indexed).
pub(crate) fn word_matches(word: &str, token: &str, prefix: bool) -> bool {
    if prefix { token.starts_with(word) } else { token == word }
}

/// True when the words of `t` occur in a row in `tokens`.
pub(crate) fn text_in_segment<S: AsRef<str>>(t: &Text, tokens: &[S]) -> bool {
    let k = t.words.len();
    if k == 0 || tokens.len() < k {
        return false;
    }
    (0..=tokens.len() - k).any(|i| {
        t.words.iter().enumerate().all(|(j, w)| {
            tokens.get(i + j).is_some_and(|tok| word_matches(w, tok.as_ref(), t.prefix && j + 1 == k))
        })
    })
}

/// Token segments of one field of a doc (people and attachment names are separate segments, so phrases
/// never run from one person into the next).
pub(crate) fn segments(doc: &Doc, bit: u8) -> Vec<String> {
    let people = |list: &[crate::doc::Person]| -> Vec<String> {
        list.iter().map(|p| format!("{} {}", p.name, p.addr)).collect()
    };
    match bit {
        field::SUBJECT => vec![doc.subject.clone()],
        field::FROM => people(&doc.from),
        field::TO => people(&doc.to),
        field::CC => people(&doc.cc),
        field::BODY => vec![doc.body.clone()],
        field::ATTACHMENTS => doc.attachments.clone(),
        _ => Vec::new(),
    }
}

fn text_in_doc(doc: &Doc, mask: u8, t: &Text) -> bool {
    field::ALL
        .iter()
        .filter(|b| mask & **b != 0)
        .any(|b| segments(doc, *b).iter().any(|seg| text_in_segment(t, &words(seg))))
}

/// The date a date term looks at.
pub(crate) fn date_of(doc_received: i64, doc_sent: Option<i64>, f: DateField) -> i64 {
    match f {
        DateField::Received => doc_received,
        DateField::Sent => doc_sent.unwrap_or(doc_received),
    }
}

fn term_matches(t: &Term, doc: &Doc, ctx: &Context) -> bool {
    match t {
        Term::Text(f, text) => text_in_doc(doc, f.mask(), text),
        Term::Me(f) => {
            let me = ctx.me_folded();
            let mask = f.mask();
            let lists = [(field::FROM, &doc.from), (field::TO, &doc.to), (field::CC, &doc.cc)];
            lists
                .iter()
                .filter(|(b, _)| mask & b != 0)
                .any(|(_, list)| list.iter().any(|p| me.contains(&fold(p.addr.trim()))))
        }
        Term::Category(CategoryMatch::Any) => !doc.categories.is_empty(),
        Term::Category(CategoryMatch::None) => doc.categories.is_empty(),
        Term::Category(CategoryMatch::Named(text)) => {
            doc.categories.iter().any(|c| text_in_segment(text, &words(c)))
        }
        Term::Folder(text) => ctx.folders_matching(text).binary_search(&doc.folder).is_ok(),
        Term::HasAttachments(b) => doc.has_attachments == *b,
        Term::Read(b) => doc.read == *b,
        Term::Flagged(b) => doc.flagged == *b,
        Term::Importance(l) => doc.importance == *l,
        Term::Date(f, r) => {
            let (lo, hi) = ctx.day_bounds(r);
            let day = ctx.day_of(date_of(doc.received, doc.sent, *f));
            day >= lo && day <= hi
        }
        Term::Size(s) => s.contains(doc.size),
        Term::Kind(k) => doc.kind == *k,
        Term::Expires(v) => Expiry::parse(v).is_some_and(|e| ctx.expiry_matches(&e, doc.expires)),
        Term::Prop(p) => p.matches(&doc.props),
        Term::Invalid(..) => false,
    }
}

/// True when `doc` matches `q`.
pub fn matches(q: &Query, doc: &Doc, ctx: &Context) -> bool {
    match q {
        Query::All => true,
        Query::Nothing => false,
        Query::And(v) => v.iter().all(|c| matches(c, doc, ctx)),
        Query::Or(v) => v.iter().any(|c| matches(c, doc, ctx)),
        Query::Not(c) => !matches(c, doc, ctx),
        Query::Term(t) => term_matches(t, doc, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Level, Person};
    use crate::query::parse;

    /// 2026-10-09 12:00 UTC, a Friday.
    const NOW: i64 = 1_791_547_200;

    fn ctx() -> Context {
        Context {
            now: NOW,
            tz_offset_min: 0,
            me: vec!["Me@Example.com".into()],
            folders: vec![(1, "Inbox".into()), (2, "Project Café".into())],
        }
    }

    fn doc() -> Doc {
        Doc {
            id: 7,
            folder: 2,
            subject: "Quarterly budget review".into(),
            from: vec![Person::new("Ann Boss", "ann.boss@example.com")],
            to: vec![Person::new("", "me@example.com"), Person::new("Bob Lee", "bob@example.org")],
            body: "Please read the résumé before Friday.".into(),
            attachments: vec!["Budget-2026.xlsx".into()],
            categories: vec!["Red category".into()],
            received: NOW - 3600,
            size: 120 * 1024,
            importance: Level::High,
            has_attachments: true,
            flagged: true,
            ..Doc::default()
        }
    }

    #[test]
    fn today_is_friday() {
        let c = ctx();
        let (y, m, d) = civil_from_days(c.today());
        assert_eq!((y, m, d), (2026, 10, 9));
        assert_eq!(weekday_of_day(c.today()).0, 5);
    }

    #[test]
    fn matching() {
        let (d, c) = (doc(), ctx());
        for q in [
            "",
            "budget",
            "bud",
            "BUDGET REVIEW",
            "\"budget review\"",
            "\"budg*\"",
            "resume",
            "from:ann",
            "from:\"ann boss\"",
            "from:ann.boss@example.com",
            "from:example.com",
            "to:me",
            "tocc:me",
            "participants:bob",
            "to:\"bob lee\"",
            "attachments:budget",
            "attachments:xlsx",
            "category:red",
            "category:\"red category\"",
            "category:any",
            "folder:cafe",
            "hasattachments:yes",
            "isread:no",
            "isflagged:yes",
            "importance:high",
            "received:today",
            "received:\"this week\"",
            "received:\"this month\"",
            "received:\"this year\"",
            "received:>=2026-10-01",
            "received:2026-10-09",
            "received:10/9/2026",
            "received:2026-10-01..2026-10-31",
            "received:>\"1 days ago\"",
            "sent:today",
            "size:large",
            "size:>100KB",
            "size:100KB..1MB",
            "kind:email",
            "budget OR nothing",
            "(lunch OR budget) -lunch",
            "NOT from:carol",
            "from:(carol OR ann)",
        ] {
            assert!(matches(&parse(q), &d, &c), "{q} should match");
        }
        for q in [
            "lunch",
            "\"budget quarterly\"",
            "\"budg\"",
            "from:bob",
            "from:me",
            "cc:me",
            "\"boss bob\"",
            "category:none",
            "category:blue",
            "folder:inbox",
            "hasattachments:no",
            "isread:yes",
            "importance:low",
            "received:yesterday",
            "received:\"last week\"",
            "received:\"last month\"",
            "received:<2026-10-09",
            "received:<\"1 days ago\"",
            "size:small",
            "size:huge",
            "kind:contact",
            "expires:soon",
            "isread:maybe",
            "-budget",
        ] {
            assert!(!matches(&parse(q), &d, &c), "{q} should not match");
        }
    }

    #[test]
    fn expiry_dates() {
        let c = ctx();
        let at = |days: i64| Doc { expires: Some(NOW + days * DAY_SECS), ..doc() };
        let none = doc();
        let m = |q: &str, d: &Doc| matches(&parse(q), d, &c);
        // Expiring Soon: later today up to a week from today; not what has expired or is far off.
        let later_today = Doc { expires: Some(NOW + 3600), ..doc() };
        assert!(m("expires:soon", &later_today) && m("expires:soon", &at(7)) && m("EXPIRES:Soon", &at(3)));
        assert!(!m("expires:soon", &at(0)), "expires right now: expired");
        assert!(!m("expires:soon", &at(8)) && !m("expires:soon", &at(-1)) && !m("expires:soon", &none));
        assert!(m("expires:expired", &at(-1)) && !m("expires:expired", &at(1)) && !m("expires:past", &none));
        assert!(m("expires:yes", &at(30)) && !m("expires:yes", &none));
        assert!(m("expires:no", &none) && !m("expires:no", &at(30)));
        assert!(m("expires:2026-10-12", &at(3)) && !m("expires:2026-10-12", &at(4)));
        assert!(m("expires:<2026-10-09", &at(-2)) && !m("expires:<2026-10-09", &none));
        assert!(!m("expires:maybe", &at(1)) && !m("-expires:yes", &at(1)) && m("-expires:yes", &none));
    }

    #[test]
    fn expiry_header() {
        use crate::doc::expiry_of;
        assert_eq!(expiry_of(b"Expiry-Date: Fri, 9 Oct 2026 12:00:00 +0000\r\n\r\n"), Some(NOW));
        assert_eq!(
            expiry_of(b"Subject: x\r\nExpires:\r\n Fri, 9 Oct 2026 12:00:00 +0000\r\nX: y\r\n\r\n"),
            Some(NOW)
        );
        assert_eq!(
            expiry_of(b"Subject: x\r\n\r\nExpires: Fri, 9 Oct 2026 12:00:00 +0000\r\n"),
            None,
            "body ignored"
        );
        assert_eq!(expiry_of(b"Expires: soon\r\n\r\n"), None);
        assert_eq!(expiry_of(&[0xff, 0xfe, b':', b'\n']), None);
        assert_eq!(expiry_of(b""), None);
        let m = mailcraft_model::MailItem {
            raw: Some(b"Expires: Fri, 9 Oct 2026 12:00:00 +0000\r\n\r\nHi".to_vec()),
            ..Default::default()
        };
        assert_eq!(Doc::from_mail(&m).expires, Some(NOW));
        assert_eq!(Doc::from_mail(&mailcraft_model::MailItem::default()).expires, None);
    }

    #[test]
    fn relative_ranges() {
        let c = ctx();
        let r = |q: &str| match parse(q) {
            Query::Term(Term::Date(_, r)) => c.day_bounds(&r),
            other => panic!("{other:?}"),
        };
        let day = |y, m, d| days_from_civil(y, m, d).unwrap();
        assert_eq!(r("received:\"this week\""), (day(2026, 10, 4), day(2026, 10, 10)));
        assert_eq!(r("received:\"last week\""), (day(2026, 9, 27), day(2026, 10, 3)));
        assert_eq!(r("received:\"last month\""), (day(2026, 9, 1), day(2026, 9, 30)));
        assert_eq!(r("received:\"next month\""), (day(2026, 11, 1), day(2026, 11, 30)));
        assert_eq!(r("received:\"last year\""), (day(2025, 1, 1), day(2025, 12, 31)));
        assert_eq!(r("received:\"1 months ago\""), (day(2026, 9, 9), day(2026, 9, 9)));
        assert_eq!(r("received:<\"7 days ago\"").1, day(2026, 10, 1));
        assert_eq!(r("received:>yesterday").0, day(2026, 10, 9));
    }
}
