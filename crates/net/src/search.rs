//! Server-side mail search: one criteria tree written out as IMAP `UID SEARCH` keys (RFC 3501 section 6.4.4,
//! RFC 9051 section 6.4.4) or as a JMAP `Email/query` filter (RFC 8621 section 4.4.1).
//!
//! The caller builds a [`Criterion`] from its own query language; this module only spells it for each
//! protocol. Strings are sent as atoms, quoted strings or literals (never spliced into the command text), and
//! a tree deeper than [`MAX_DEPTH`] or with more than [`MAX_NODES`] nodes is refused.

use serde_json::{Value, json};

use crate::error::{NetError, Result};
use crate::imap::client::Command;
use crate::sync::imap_date;

/// Deepest criteria tree accepted.
pub const MAX_DEPTH: usize = 16;
/// Most nodes in one criteria tree.
pub const MAX_NODES: usize = 256;
/// Longest text of one condition (bytes).
pub const MAX_TEXT: usize = 1_000;

/// Where a text condition looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextIn {
    /// Header and body (IMAP `TEXT`, JMAP `text`).
    Anywhere,
    From,
    To,
    Cc,
    Subject,
    Body,
}

/// A UTC calendar day (year, month, day).
pub type Day = (i64, u8, u8);

/// One search condition, or a combination of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Criterion {
    /// Every message.
    All,
    And(Vec<Criterion>),
    Or(Vec<Criterion>),
    Not(Box<Criterion>),
    /// The text appears (case-insensitive substring, as the server matches it).
    Text(TextIn, String),
    /// Read (`true`) or unread.
    Seen(bool),
    /// Flagged (`true`) or not.
    Flagged(bool),
    /// Received on or after the day.
    ReceivedSince(Day),
    /// Received before the day.
    ReceivedBefore(Day),
    /// Sent (Date header) on or after the day.
    SentSince(Day),
    /// Sent before the day.
    SentBefore(Day),
    /// Larger than this many bytes.
    Larger(u64),
    /// Smaller than this many bytes.
    Smaller(u64),
}

impl Criterion {
    /// Checks the tree's size and depth and the length of its texts.
    pub fn check(&self) -> Result<()> {
        let mut nodes = 0usize;
        self.walk(0, &mut nodes)
    }

    fn walk(&self, depth: usize, nodes: &mut usize) -> Result<()> {
        *nodes = nodes.saturating_add(1);
        if depth > MAX_DEPTH {
            return Err(NetError::Invalid("the search is nested too deeply".into()));
        }
        if *nodes > MAX_NODES {
            return Err(NetError::Invalid("the search has too many conditions".into()));
        }
        match self {
            Criterion::And(v) | Criterion::Or(v) => {
                for c in v {
                    c.walk(depth.saturating_add(1), nodes)?;
                }
                Ok(())
            }
            Criterion::Not(c) => c.walk(depth.saturating_add(1), nodes),
            Criterion::Text(_, t) if t.len() > MAX_TEXT => {
                Err(NetError::Invalid("a search word is too long".into()))
            }
            Criterion::Text(_, t) if t.chars().any(|c| c.is_control()) => {
                Err(NetError::Invalid("a search word holds control characters".into()))
            }
            _ => Ok(()),
        }
    }

    /// True when any text condition holds non-ASCII text.
    fn non_ascii(&self) -> bool {
        match self {
            Criterion::And(v) | Criterion::Or(v) => v.iter().any(Criterion::non_ascii),
            Criterion::Not(c) => c.non_ascii(),
            Criterion::Text(_, t) => !t.is_ascii(),
            _ => false,
        }
    }

    /// The `UID SEARCH` command for this criteria. `utf8_accept`: the server has `UTF8=ACCEPT` enabled
    /// (RFC 6855), so UTF-8 strings go as quoted strings and no `CHARSET` is named; otherwise non-ASCII text
    /// is searched with `CHARSET UTF-8` and sent as literals.
    pub fn imap_command(&self, utf8_accept: bool) -> Result<Command> {
        self.check()?;
        let mut cmd = Command::new("UID SEARCH");
        if !utf8_accept && self.non_ascii() {
            cmd = cmd.raw("CHARSET UTF-8");
        }
        imap_keys(self, cmd, utf8_accept)
    }

    /// The JMAP `Email/query` filter (a FilterCondition or a FilterOperator); `None` for [`Criterion::All`].
    pub fn jmap_filter(&self) -> Result<Option<Value>> {
        self.check()?;
        Ok(jmap(self))
    }
}

fn day_text(d: Day) -> Result<String> {
    imap_date(d.0, d.1, d.2).ok_or_else(|| NetError::Invalid("a search date is out of range".into()))
}

/// Appends the search keys of `c` to `cmd` (several keys in a row are ANDed, RFC 3501 section 6.4.4).
fn imap_keys(c: &Criterion, cmd: Command, utf8: bool) -> Result<Command> {
    Ok(match c {
        Criterion::All => cmd.raw("ALL"),
        Criterion::And(v) if v.is_empty() => cmd.raw("ALL"),
        Criterion::And(v) => {
            let mut cmd = cmd.open();
            for k in v {
                cmd = imap_keys(k, cmd, utf8)?;
            }
            cmd.close()
        }
        Criterion::Or(v) => match v.split_first() {
            None => cmd.raw("NOT ALL"),
            Some((first, [])) => imap_keys(first, cmd, utf8)?,
            Some((first, rest)) => {
                // OR a (OR b c): binary ORs nested to the right.
                let cmd = imap_keys(first, cmd.raw("OR"), utf8)?;
                imap_keys(&Criterion::Or(rest.to_vec()), cmd, utf8)?
            }
        },
        Criterion::Not(k) => imap_keys(k, cmd.raw("NOT"), utf8)?,
        Criterion::Text(field, text) => {
            let key = match field {
                TextIn::Anywhere => "TEXT",
                TextIn::From => "FROM",
                TextIn::To => "TO",
                TextIn::Cc => "CC",
                TextIn::Subject => "SUBJECT",
                TextIn::Body => "BODY",
            };
            let cmd = cmd.raw(key);
            if !utf8 && !text.is_ascii() {
                cmd.literal(text.as_bytes(), false)
            } else {
                cmd.string(text.as_bytes(), utf8)
            }
        }
        Criterion::Seen(true) => cmd.raw("SEEN"),
        Criterion::Seen(false) => cmd.raw("UNSEEN"),
        Criterion::Flagged(true) => cmd.raw("FLAGGED"),
        Criterion::Flagged(false) => cmd.raw("UNFLAGGED"),
        Criterion::ReceivedSince(d) => cmd.raw("SINCE").raw(&day_text(*d)?),
        Criterion::ReceivedBefore(d) => cmd.raw("BEFORE").raw(&day_text(*d)?),
        Criterion::SentSince(d) => cmd.raw("SENTSINCE").raw(&day_text(*d)?),
        Criterion::SentBefore(d) => cmd.raw("SENTBEFORE").raw(&day_text(*d)?),
        Criterion::Larger(n) => cmd.raw("LARGER").raw(&n.to_string()),
        Criterion::Smaller(n) => cmd.raw("SMALLER").raw(&n.to_string()),
    })
}

/// A JMAP UTCDate for the start of a day.
fn utc_day(d: Day) -> Option<String> {
    let _ = imap_date(d.0, d.1, d.2)?;
    Some(format!("{:04}-{:02}-{:02}T00:00:00Z", d.0, d.1, d.2))
}

fn jmap(c: &Criterion) -> Option<Value> {
    let op = |name: &str, list: Vec<Value>| json!({ "operator": name, "conditions": list });
    match c {
        Criterion::All => None,
        Criterion::And(v) => {
            let list: Vec<Value> = v.iter().filter_map(jmap).collect();
            match list.len() {
                0 => None,
                1 => list.into_iter().next(),
                _ => Some(op("AND", list)),
            }
        }
        Criterion::Or(v) => {
            if v.iter().any(|c| jmap(c).is_none()) {
                // One branch matches everything.
                return None;
            }
            let list: Vec<Value> = v.iter().filter_map(jmap).collect();
            match list.len() {
                // An empty OR matches nothing: NOT of everything.
                0 => Some(op("NOT", vec![json!({})])),
                1 => list.into_iter().next(),
                _ => Some(op("OR", list)),
            }
        }
        Criterion::Not(k) => Some(op("NOT", vec![jmap(k).unwrap_or_else(|| json!({}))])),
        Criterion::Text(field, text) => {
            let key = match field {
                TextIn::Anywhere => "text",
                TextIn::From => "from",
                TextIn::To => "to",
                TextIn::Cc => "cc",
                TextIn::Subject => "subject",
                TextIn::Body => "body",
            };
            Some(json!({ key: text }))
        }
        Criterion::Seen(true) => Some(json!({ "hasKeyword": "$seen" })),
        Criterion::Seen(false) => Some(json!({ "notKeyword": "$seen" })),
        Criterion::Flagged(true) => Some(json!({ "hasKeyword": "$flagged" })),
        Criterion::Flagged(false) => Some(json!({ "notKeyword": "$flagged" })),
        // JMAP filters by receivedAt only; sent dates use the received date instead.
        Criterion::ReceivedSince(d) | Criterion::SentSince(d) => utc_day(*d).map(|t| json!({ "after": t })),
        Criterion::ReceivedBefore(d) | Criterion::SentBefore(d) => {
            utc_day(*d).map(|t| json!({ "before": t }))
        }
        Criterion::Larger(n) => Some(json!({ "minSize": n.saturating_add(1) })),
        Criterion::Smaller(n) => Some(json!({ "maxSize": n })),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn imap(c: &Criterion, utf8: bool) -> String {
        String::from_utf8(c.imap_command(utf8).unwrap().wire_preview()).unwrap()
    }

    #[test]
    fn imap_keys_are_spelled_per_rfc() {
        let c = Criterion::And(vec![
            Criterion::Text(TextIn::Anywhere, "budget".into()),
            Criterion::Text(TextIn::From, "ann@example.com".into()),
            Criterion::Seen(false),
        ]);
        assert_eq!(imap(&c, false), "UID SEARCH (TEXT budget FROM ann@example.com UNSEEN)");
        let c = Criterion::Or(vec![
            Criterion::Text(TextIn::To, "a".into()),
            Criterion::Text(TextIn::Cc, "b".into()),
            Criterion::Text(TextIn::Subject, "two words".into()),
        ]);
        assert_eq!(imap(&c, false), "UID SEARCH OR TO a OR CC b SUBJECT \"two words\"");
        let c = Criterion::Not(Box::new(Criterion::Flagged(true)));
        assert_eq!(imap(&c, false), "UID SEARCH NOT FLAGGED");
        let c = Criterion::And(vec![
            Criterion::ReceivedSince((2025, 1, 5)),
            Criterion::SentBefore((2026, 10, 1)),
            Criterion::Larger(100),
        ]);
        assert_eq!(imap(&c, false), "UID SEARCH (SINCE 5-Jan-2025 SENTBEFORE 1-Oct-2026 LARGER 100)");
        assert_eq!(imap(&Criterion::All, false), "UID SEARCH ALL");
    }

    #[test]
    fn hostile_text_is_quoted_or_sent_as_a_literal() {
        let c = Criterion::Text(TextIn::Subject, "a\" ) OR ALL".into());
        assert_eq!(imap(&c, false), "UID SEARCH SUBJECT \"a\\\" ) OR ALL\"");
        // Non-ASCII without UTF8=ACCEPT: CHARSET UTF-8 and a literal.
        let c = Criterion::Text(TextIn::Anywhere, "café".into());
        assert_eq!(imap(&c, false), "UID SEARCH CHARSET UTF-8 TEXT {5}\r\ncafé");
        // With UTF8=ACCEPT: a quoted UTF-8 string, no CHARSET.
        assert_eq!(imap(&c, true), "UID SEARCH TEXT \"café\"");
        // Control characters and huge or deep trees are refused.
        assert!(Criterion::Text(TextIn::Anywhere, "a\r\nb".into()).imap_command(false).is_err());
        assert!(Criterion::Text(TextIn::Anywhere, "x".repeat(MAX_TEXT + 1)).imap_command(false).is_err());
        let mut deep = Criterion::Seen(true);
        for _ in 0..(MAX_DEPTH + 2) {
            deep = Criterion::Not(Box::new(deep));
        }
        assert!(deep.imap_command(false).is_err() && deep.jmap_filter().is_err());
        let wide = Criterion::Or(vec![Criterion::Seen(true); MAX_NODES + 1]);
        assert!(wide.check().is_err());
        assert!(Criterion::ReceivedSince((2026, 2, 30)).imap_command(false).is_err());
    }

    #[test]
    fn jmap_filters() {
        let c = Criterion::And(vec![
            Criterion::Text(TextIn::Anywhere, "budget".into()),
            Criterion::Not(Box::new(Criterion::Seen(true))),
            Criterion::ReceivedBefore((2025, 3, 1)),
        ]);
        assert_eq!(
            c.jmap_filter().unwrap().unwrap(),
            json!({"operator": "AND", "conditions": [
                {"text": "budget"},
                {"operator": "NOT", "conditions": [{"hasKeyword": "$seen"}]},
                {"before": "2025-03-01T00:00:00Z"},
            ]})
        );
        assert_eq!(Criterion::All.jmap_filter().unwrap(), None);
        let one = Criterion::And(vec![Criterion::All, Criterion::Text(TextIn::From, "ann".into())]);
        assert_eq!(one.jmap_filter().unwrap().unwrap(), json!({"from": "ann"}));
        let any = Criterion::Or(vec![Criterion::All, Criterion::Seen(true)]);
        assert_eq!(any.jmap_filter().unwrap(), None);
        assert_eq!(Criterion::Larger(10).jmap_filter().unwrap().unwrap(), json!({"minSize": 11}));
    }
}
