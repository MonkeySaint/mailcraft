//! SEARCH / UID SEARCH, with ESEARCH (RFC 4731) results for `RETURN (...)` or IMAP4rev2 clients.

use std::sync::Arc;

use super::parse::{Parser, SeqSet, format_set};
use super::{Fail, Res, Session, bad, no};
use crate::date::DateTime;
use crate::mime;

const MAX_DEPTH: usize = 32;

#[derive(Debug)]
enum Key {
    All,
    Seq(SeqSet),
    Uid(SeqSet),
    /// Flag present (`true`) or absent.
    Flag(String, bool),
    Never,
    Since(i64),
    Before(i64),
    On(i64),
    SentSince(i64),
    SentBefore(i64),
    SentOn(i64),
    Larger(u64),
    Smaller(u64),
    Header(String, Vec<u8>),
    Body(Vec<u8>),
    Text(Vec<u8>),
    ModSeq(u64),
    Not(Box<Key>),
    Or(Box<Key>, Box<Key>),
    And(Vec<Key>),
}

fn parse_key(p: &mut Parser<'_>, depth: usize) -> Result<Key, Fail> {
    if depth > MAX_DEPTH {
        return Err(bad("Search too deeply nested"));
    }
    if p.eat(b'(') {
        let mut keys = Vec::new();
        while !p.eat(b')') {
            if !keys.is_empty() {
                p.sp()?;
            }
            keys.push(parse_key(p, depth + 1)?);
        }
        return Ok(Key::And(keys));
    }
    if p.peek().is_some_and(|c| c.is_ascii_digit() || c == b'*') {
        return Ok(Key::Seq(p.seq_set()?));
    }
    let word = String::from_utf8_lossy(p.atom()?).to_ascii_uppercase();
    let flag = |f: &str, has: bool| Ok(Key::Flag(f.to_owned(), has));
    match word.as_str() {
        "ALL" => Ok(Key::All),
        "ANSWERED" => flag("\\Answered", true),
        "DELETED" => flag("\\Deleted", true),
        "DRAFT" => flag("\\Draft", true),
        "FLAGGED" => flag("\\Flagged", true),
        "SEEN" | "OLD" => {
            if word == "OLD" {
                Ok(Key::All)
            } else {
                flag("\\Seen", true)
            }
        }
        "UNANSWERED" => flag("\\Answered", false),
        "UNDELETED" => flag("\\Deleted", false),
        "UNDRAFT" => flag("\\Draft", false),
        "UNFLAGGED" => flag("\\Flagged", false),
        "UNSEEN" => flag("\\Seen", false),
        "NEW" | "RECENT" => Ok(Key::Never),
        "KEYWORD" | "UNKEYWORD" => {
            p.sp()?;
            let f = p.flag()?;
            Ok(Key::Flag(f, word == "KEYWORD"))
        }
        "BCC" | "CC" | "FROM" | "TO" | "SUBJECT" => {
            p.sp()?;
            let name = word.chars().next().map(|c| c.to_string()).unwrap_or_default()
                + &word.get(1..).unwrap_or("").to_ascii_lowercase();
            Ok(Key::Header(name, p.astring()?))
        }
        "HEADER" => {
            p.sp()?;
            let name = p.astring_utf8()?;
            p.sp()?;
            Ok(Key::Header(name, p.astring()?))
        }
        "BODY" => {
            p.sp()?;
            Ok(Key::Body(p.astring()?))
        }
        "TEXT" => {
            p.sp()?;
            Ok(Key::Text(p.astring()?))
        }
        "SINCE" | "BEFORE" | "ON" | "SENTSINCE" | "SENTBEFORE" | "SENTON" => {
            p.sp()?;
            let d = p.date()?;
            Ok(match word.as_str() {
                "SINCE" => Key::Since(d),
                "BEFORE" => Key::Before(d),
                "ON" => Key::On(d),
                "SENTSINCE" => Key::SentSince(d),
                "SENTBEFORE" => Key::SentBefore(d),
                _ => Key::SentOn(d),
            })
        }
        "LARGER" | "SMALLER" => {
            p.sp()?;
            let n = p.number()?;
            Ok(if word == "LARGER" { Key::Larger(n) } else { Key::Smaller(n) })
        }
        "UID" => {
            p.sp()?;
            Ok(Key::Uid(p.seq_set()?))
        }
        "NOT" => {
            p.sp()?;
            Ok(Key::Not(Box::new(parse_key(p, depth + 1)?)))
        }
        "OR" => {
            p.sp()?;
            let a = parse_key(p, depth + 1)?;
            p.sp()?;
            let b = parse_key(p, depth + 1)?;
            Ok(Key::Or(Box::new(a), Box::new(b)))
        }
        "MODSEQ" => {
            p.sp()?;
            if p.peek() == Some(b'"') {
                // entry-name entry-type-req: accepted and ignored (one modseq per message here).
                p.astring()?;
                p.sp()?;
                p.atom()?;
                p.sp()?;
            }
            Ok(Key::ModSeq(p.number()?))
        }
        _ => Err(bad(format!("Unknown search key {word}"))),
    }
}

struct View {
    seq: u32,
    uid: u32,
    flags: Vec<String>,
    date: DateTime,
    raw: Arc<[u8]>,
    modseq: u64,
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

fn sent_day(v: &View) -> i64 {
    mime::header_value(mime::split_message(&v.raw).0, "Date")
        .and_then(|d| DateTime::parse_rfc5322(&String::from_utf8_lossy(&d)))
        .unwrap_or(v.date)
        .local_day()
}

fn eval(k: &Key, v: &View, max_seq: u32, max_uid: u32) -> bool {
    match k {
        Key::All => true,
        Key::Never => false,
        Key::Seq(s) => s.contains(v.seq, max_seq),
        Key::Uid(s) => s.contains(v.uid, max_uid),
        Key::Flag(f, has) => v.flags.iter().any(|x| x.eq_ignore_ascii_case(f)) == *has,
        Key::Since(d) => v.date.local_day() >= *d,
        Key::Before(d) => v.date.local_day() < *d,
        Key::On(d) => v.date.local_day() == *d,
        Key::SentSince(d) => sent_day(v) >= *d,
        Key::SentBefore(d) => sent_day(v) < *d,
        Key::SentOn(d) => sent_day(v) == *d,
        Key::Larger(n) => v.raw.len() as u64 > *n,
        Key::Smaller(n) => (v.raw.len() as u64) < *n,
        Key::Header(name, needle) => mime::fields(mime::split_message(&v.raw).0)
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(name.as_bytes()))
            .any(|f| contains_ci(&mime::unfold(f.value), needle)),
        Key::Body(n) => contains_ci(mime::split_message(&v.raw).1, n),
        Key::Text(n) => contains_ci(&v.raw, n),
        Key::ModSeq(m) => v.modseq >= *m,
        Key::Not(k) => !eval(k, v, max_seq, max_uid),
        Key::Or(a, b) => eval(a, v, max_seq, max_uid) || eval(b, v, max_seq, max_uid),
        Key::And(ks) => ks.iter().all(|k| eval(k, v, max_seq, max_uid)),
    }
}

fn uses_modseq(k: &Key) -> bool {
    match k {
        Key::ModSeq(_) => true,
        Key::Not(k) => uses_modseq(k),
        Key::Or(a, b) => uses_modseq(a) || uses_modseq(b),
        Key::And(ks) => ks.iter().any(uses_modseq),
        _ => false,
    }
}

pub fn search(s: &mut Session<'_>, tag: &str, p: &mut Parser<'_>, out: &mut Vec<u8>, uid_mode: bool) -> Res {
    let user = s.need_auth()?.to_owned();
    p.sp()?;
    let mut ret: Option<Vec<String>> = None;
    if p.keyword("RETURN") {
        p.sp()?;
        p.expect(b'(')?;
        let mut opts = Vec::new();
        while !p.eat(b')') {
            p.eat(b' ');
            let o = String::from_utf8_lossy(p.atom()?).to_ascii_uppercase();
            if !matches!(o.as_str(), "MIN" | "MAX" | "COUNT" | "ALL") {
                return Err(bad(format!("Unsupported RETURN option {o}")));
            }
            opts.push(o);
        }
        p.sp()?;
        ret = Some(opts);
    }
    if p.keyword("CHARSET") {
        p.sp()?;
        let cs = p.astring_utf8()?.to_ascii_uppercase();
        if !matches!(cs.as_str(), "US-ASCII" | "UTF-8") {
            return Err(no("[BADCHARSET (US-ASCII UTF-8)] Unsupported charset"));
        }
        p.sp()?;
    }
    let mut keys = vec![parse_key(p, 0)?];
    while p.eat(b' ') {
        keys.push(parse_key(p, 0)?);
        if keys.len() > 256 {
            return Err(bad("Too many search keys"));
        }
    }
    p.end()?;
    let key = Key::And(keys);
    let with_modseq = uses_modseq(&key);
    if with_modseq {
        s.condstore = true;
    }
    let sel = s.need_sel()?;
    let name = sel.name.clone();
    let known = sel.known.clone();
    let views: Vec<View> = {
        let st = s.shared().lock();
        let mb = st.mbox(&user, &name).ok_or_else(|| no("Mailbox is gone"))?;
        known
            .iter()
            .enumerate()
            .filter_map(|(i, (uid, _))| {
                let m = mb.find(*uid)?;
                Some(View {
                    seq: u32::try_from(i + 1).ok()?,
                    uid: m.uid,
                    flags: m.flags.clone(),
                    date: m.internal_date,
                    raw: Arc::clone(&m.raw),
                    modseq: m.modseq,
                })
            })
            .collect()
    };
    let max_seq = u32::try_from(known.len()).unwrap_or(u32::MAX);
    let max_uid = known.last().map(|k| k.0).unwrap_or(0);
    let hits: Vec<&View> = views.iter().filter(|v| eval(&key, v, max_seq, max_uid)).collect();
    let nums: Vec<u32> = hits.iter().map(|v| if uid_mode { v.uid } else { v.seq }).collect();
    let top_modseq = hits.iter().map(|v| v.modseq).max();
    let ret = match ret {
        Some(r) => Some(r),
        None if s.rev2 => Some(vec!["ALL".to_owned()]),
        None => None,
    };
    match ret {
        Some(mut opts) => {
            if opts.is_empty() {
                opts.push("ALL".to_owned());
            }
            let mut line = format!("* ESEARCH (TAG \"{tag}\")");
            if uid_mode {
                line.push_str(" UID");
            }
            for o in &opts {
                match o.as_str() {
                    "MIN" => {
                        if let Some(n) = nums.iter().min() {
                            line.push_str(&format!(" MIN {n}"));
                        }
                    }
                    "MAX" => {
                        if let Some(n) = nums.iter().max() {
                            line.push_str(&format!(" MAX {n}"));
                        }
                    }
                    "COUNT" => line.push_str(&format!(" COUNT {}", nums.len())),
                    _ => {
                        if !nums.is_empty() {
                            line.push_str(&format!(" ALL {}", format_set(&nums)));
                        }
                    }
                }
            }
            if with_modseq && let Some(m) = top_modseq {
                line.push_str(&format!(" MODSEQ {m}"));
            }
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        None => {
            let mut line = "* SEARCH".to_owned();
            for n in &nums {
                line.push_str(&format!(" {n}"));
            }
            if with_modseq && let Some(m) = top_modseq {
                line.push_str(&format!(" (MODSEQ {m})"));
            }
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
    }
    Ok(format!("{}SEARCH completed", if uid_mode { "UID " } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let k = parse_key(&mut Parser::new(b"(OR SEEN FLAGGED NOT FROM \"jane\")"), 0).unwrap();
        let v = View {
            seq: 1,
            uid: 7,
            flags: vec!["\\Seen".into()],
            date: DateTime::now(),
            raw: Arc::from(&b"From: bob@example.com\r\n\r\nhi"[..]),
            modseq: 3,
        };
        assert!(eval(&k, &v, 1, 7));
        let k = parse_key(&mut Parser::new(b"FROM BOB"), 0).unwrap();
        assert!(eval(&k, &v, 1, 7));
        let k = parse_key(&mut Parser::new(b"UID 8:*"), 0).unwrap();
        assert!(eval(&k, &v, 1, 7), "* is the largest UID");
        assert!(parse_key(&mut Parser::new(b"BOGUS"), 0).is_err());
        let deep = format!("{}ALL{}", "(".repeat(100), ")".repeat(100));
        assert!(parse_key(&mut Parser::new(deep.as_bytes()), 0).is_err());
    }
}
