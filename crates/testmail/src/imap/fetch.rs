//! FETCH and STORE.

use std::sync::Arc;

use super::parse::{Parser, format_set};
use super::{Fail, Res, Session, bad, flags_list, no};
use crate::date::DateTime;
use crate::mime::{self, Section};
use crate::store::{add_flags, canonical_flag, remove_flags};

#[derive(Clone, Debug)]
enum Att {
    Uid,
    Flags,
    InternalDate,
    Size,
    Envelope,
    Body,
    BodyStructure,
    ModSeq,
    Rfc822,
    Rfc822Header,
    Rfc822Text,
    Section { sec: Section, peek: bool, partial: Option<(u64, u64)> },
}

impl Att {
    fn sets_seen(&self) -> bool {
        matches!(self, Att::Rfc822 | Att::Rfc822Text | Att::Section { peek: false, .. })
    }
}

fn parse_att(p: &mut Parser<'_>) -> Result<Att, Fail> {
    Ok(if p.keyword("BODY.PEEK") {
        let sec = p.section()?;
        Att::Section { sec, peek: true, partial: p.partial()? }
    } else if p.keyword("BODYSTRUCTURE") {
        Att::BodyStructure
    } else if p.keyword("BODY") {
        if p.peek() == Some(b'[') {
            let sec = p.section()?;
            Att::Section { sec, peek: false, partial: p.partial()? }
        } else {
            Att::Body
        }
    } else if p.keyword("RFC822.SIZE") {
        Att::Size
    } else if p.keyword("RFC822.HEADER") {
        Att::Rfc822Header
    } else if p.keyword("RFC822.TEXT") {
        Att::Rfc822Text
    } else if p.keyword("RFC822") {
        Att::Rfc822
    } else if p.keyword("ENVELOPE") {
        Att::Envelope
    } else if p.keyword("FLAGS") {
        Att::Flags
    } else if p.keyword("INTERNALDATE") {
        Att::InternalDate
    } else if p.keyword("UID") {
        Att::Uid
    } else if p.keyword("MODSEQ") {
        Att::ModSeq
    } else {
        return Err(bad("Unknown or unsupported FETCH attribute"));
    })
}

fn parse_atts(p: &mut Parser<'_>) -> Result<Vec<Att>, Fail> {
    if p.keyword("ALL") {
        return Ok(vec![Att::Flags, Att::InternalDate, Att::Size, Att::Envelope]);
    }
    if p.keyword("FAST") {
        return Ok(vec![Att::Flags, Att::InternalDate, Att::Size]);
    }
    if p.keyword("FULL") {
        return Ok(vec![Att::Flags, Att::InternalDate, Att::Size, Att::Envelope, Att::Body]);
    }
    if p.eat(b'(') {
        let mut atts = Vec::new();
        while !p.eat(b')') {
            if !atts.is_empty() {
                p.sp()?;
            }
            atts.push(parse_att(p)?);
            if atts.len() > 100 {
                return Err(bad("Too many FETCH attributes"));
            }
        }
        if atts.is_empty() {
            return Err(bad("Empty FETCH attribute list"));
        }
        return Ok(atts);
    }
    Ok(vec![parse_att(p)?])
}

struct View {
    seq: usize,
    uid: u32,
    flags: Vec<String>,
    date: DateTime,
    raw: Arc<[u8]>,
    modseq: u64,
    flags_changed: bool,
}

pub fn fetch(s: &mut Session<'_>, p: &mut Parser<'_>, out: &mut Vec<u8>, uid_mode: bool) -> Res {
    let user = s.need_auth()?.to_owned();
    p.sp()?;
    let set = p.seq_set()?;
    p.sp()?;
    let mut atts = parse_atts(p)?;
    let mut changed_since = None;
    if p.eat(b' ') {
        p.expect(b'(')?;
        while !p.eat(b')') {
            p.eat(b' ');
            if p.keyword("CHANGEDSINCE") {
                p.sp()?;
                changed_since = Some(p.number()?);
            } else {
                return Err(bad("Unsupported FETCH modifier"));
            }
        }
    }
    p.end()?;
    if changed_since.is_some() || atts.iter().any(|a| matches!(a, Att::ModSeq)) {
        s.condstore = true;
    }
    if changed_since.is_some() && !atts.iter().any(|a| matches!(a, Att::ModSeq)) {
        atts.push(Att::ModSeq);
    }
    if uid_mode && !atts.iter().any(|a| matches!(a, Att::Uid)) {
        atts.insert(0, Att::Uid);
    }
    let sel = s.need_sel()?;
    let targets = sel.targets(&set, uid_mode);
    let set_seen = !sel.read_only && atts.iter().any(Att::sets_seen);
    let name = sel.name.clone();
    let mut views = Vec::with_capacity(targets.len());
    let mut any_changed = false;
    {
        let mut st = s.shared().lock();
        let mb =
            st.user_mut(&user).and_then(|u| u.mailbox_mut(&name)).ok_or_else(|| no("Mailbox is gone"))?;
        for (seq, uid) in &targets {
            let need_seen = set_seen && mb.find(*uid).is_some_and(|m| !m.has_flag("\\Seen"));
            let new_modseq = if need_seen { Some(mb.next_modseq()) } else { None };
            let Some(m) = mb.find_mut(*uid) else { continue };
            if changed_since.is_some_and(|c| m.modseq <= c) {
                continue;
            }
            let mut flags_changed = false;
            if let Some(ms) = new_modseq {
                add_flags(&mut m.flags, &["\\Seen".to_owned()]);
                m.modseq = ms;
                flags_changed = true;
                any_changed = true;
            }
            views.push(View {
                seq: *seq,
                uid: m.uid,
                flags: m.flags.clone(),
                date: m.internal_date,
                raw: Arc::clone(&m.raw),
                modseq: m.modseq,
                flags_changed,
            });
        }
    }
    if any_changed {
        s.shared().notify();
        if let Some(sel) = s.sel.as_mut() {
            for v in views.iter().filter(|v| v.flags_changed) {
                if let Some(k) = sel.known.iter_mut().find(|k| k.0 == v.uid) {
                    k.1 = v.modseq;
                }
            }
        }
    }
    for v in &views {
        write_fetch(out, v, &atts);
    }
    Ok(format!("{}FETCH completed", if uid_mode { "UID " } else { "" }))
}

fn write_fetch(out: &mut Vec<u8>, v: &View, atts: &[Att]) {
    out.extend_from_slice(format!("* {} FETCH (", v.seq).as_bytes());
    let mut first = true;
    let mut sep = |out: &mut Vec<u8>| {
        if !first {
            out.push(b' ');
        }
        first = false;
    };
    let has_flags = atts.iter().any(|a| matches!(a, Att::Flags));
    for a in atts {
        sep(out);
        match a {
            Att::Uid => out.extend_from_slice(format!("UID {}", v.uid).as_bytes()),
            Att::Flags => out.extend_from_slice(format!("FLAGS {}", flags_list(&v.flags)).as_bytes()),
            Att::InternalDate => {
                out.extend_from_slice(format!("INTERNALDATE \"{}\"", v.date.to_imap()).as_bytes())
            }
            Att::Size => out.extend_from_slice(format!("RFC822.SIZE {}", v.raw.len()).as_bytes()),
            Att::ModSeq => out.extend_from_slice(format!("MODSEQ ({})", v.modseq).as_bytes()),
            Att::Envelope => {
                out.extend_from_slice(b"ENVELOPE ");
                mime::write_envelope(out, mime::split_message(&v.raw).0);
            }
            Att::Body | Att::BodyStructure => {
                out.extend_from_slice(if matches!(a, Att::Body) { b"BODY " } else { b"BODYSTRUCTURE " });
                mime::write_body_structure(out, &mime::parse(&v.raw), matches!(a, Att::BodyStructure));
            }
            Att::Rfc822 => {
                out.extend_from_slice(b"RFC822 ");
                write_literal(out, &v.raw);
            }
            Att::Rfc822Header => {
                out.extend_from_slice(b"RFC822.HEADER ");
                write_literal(out, mime::split_message(&v.raw).0);
            }
            Att::Rfc822Text => {
                out.extend_from_slice(b"RFC822.TEXT ");
                write_literal(out, mime::split_message(&v.raw).1);
            }
            Att::Section { sec, partial, .. } => {
                let data = mime::extract(&v.raw, sec).unwrap_or_default();
                out.extend_from_slice(format!("BODY[{}]", sec.spec()).as_bytes());
                match partial {
                    Some((origin, count)) => {
                        out.extend_from_slice(format!("<{origin}> ").as_bytes());
                        let start = usize::try_from(*origin).unwrap_or(usize::MAX).min(data.len());
                        let end = start
                            .saturating_add(usize::try_from(*count).unwrap_or(usize::MAX))
                            .min(data.len());
                        write_literal(out, data.get(start..end).unwrap_or(&[]));
                    }
                    None => {
                        out.push(b' ');
                        write_literal(out, &data);
                    }
                }
            }
        }
    }
    if v.flags_changed && !has_flags {
        sep(out);
        out.extend_from_slice(format!("FLAGS {}", flags_list(&v.flags)).as_bytes());
    }
    out.extend_from_slice(b")\r\n");
}

fn write_literal(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(format!("{{{}}}\r\n", data.len()).as_bytes());
    out.extend_from_slice(data);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StoreOp {
    Replace,
    Add,
    Remove,
}

pub fn store(s: &mut Session<'_>, p: &mut Parser<'_>, out: &mut Vec<u8>, uid_mode: bool) -> Res {
    let user = s.need_auth()?.to_owned();
    p.sp()?;
    let set = p.seq_set()?;
    p.sp()?;
    let mut unchanged_since = None;
    if p.eat(b'(') {
        if !p.keyword("UNCHANGEDSINCE") {
            return Err(bad("Unsupported STORE modifier"));
        }
        p.sp()?;
        unchanged_since = Some(p.number()?);
        p.expect(b')')?;
        p.sp()?;
        s.condstore = true;
    }
    let item = String::from_utf8_lossy(p.atom()?).to_ascii_uppercase();
    let (op, silent) = match item.as_str() {
        "FLAGS" => (StoreOp::Replace, false),
        "FLAGS.SILENT" => (StoreOp::Replace, true),
        "+FLAGS" => (StoreOp::Add, false),
        "+FLAGS.SILENT" => (StoreOp::Add, true),
        "-FLAGS" => (StoreOp::Remove, false),
        "-FLAGS.SILENT" => (StoreOp::Remove, true),
        _ => return Err(bad("Unknown STORE item")),
    };
    p.sp()?;
    let raw_flags = if p.peek() == Some(b'(') {
        p.flag_list()?
    } else {
        let mut v = vec![p.flag()?];
        while p.eat(b' ') {
            v.push(p.flag()?);
        }
        v
    };
    p.end()?;
    let mut flags = Vec::new();
    for f in raw_flags {
        if f.eq_ignore_ascii_case("\\Recent") {
            continue;
        }
        flags.push(canonical_flag(&f).ok_or_else(|| bad(format!("Invalid flag {f}")))?);
    }
    let sel = s.need_sel()?;
    if sel.read_only {
        return Err(no("[READ-ONLY] Mailbox is read-only"));
    }
    let targets = sel.targets(&set, uid_mode);
    let name = sel.name.clone();
    let mut results = Vec::new();
    let mut modified = Vec::new();
    {
        let mut st = s.shared().lock();
        let mb =
            st.user_mut(&user).and_then(|u| u.mailbox_mut(&name)).ok_or_else(|| no("Mailbox is gone"))?;
        for (seq, uid) in &targets {
            let Some(m) = mb.find(*uid) else { continue };
            if unchanged_since.is_some_and(|u| m.modseq > u) {
                modified.push(if uid_mode { *uid } else { u32::try_from(*seq).unwrap_or(u32::MAX) });
                continue;
            }
            let mut new_flags = m.flags.clone();
            let changed = match op {
                StoreOp::Replace => {
                    let mut f = Vec::new();
                    add_flags(&mut f, &flags);
                    let same = f.len() == new_flags.len()
                        && f.iter().all(|x| new_flags.iter().any(|y| y.eq_ignore_ascii_case(x)));
                    new_flags = f;
                    !same
                }
                StoreOp::Add => add_flags(&mut new_flags, &flags),
                StoreOp::Remove => remove_flags(&mut new_flags, &flags),
            };
            let ms = if changed { Some(mb.next_modseq()) } else { None };
            if let Some(m) = mb.find_mut(*uid) {
                if let Some(ms) = ms {
                    m.flags = new_flags;
                    m.modseq = ms;
                }
                results.push((*seq, m.uid, m.flags.clone(), m.modseq, changed));
            }
        }
    }
    if results.iter().any(|r| r.4) {
        s.shared().notify();
    }
    let condstore = s.condstore;
    if let Some(sel) = s.sel.as_mut() {
        for (_, uid, _, ms, _) in &results {
            if let Some(k) = sel.known.iter_mut().find(|k| k.0 == *uid) {
                k.1 = *ms;
            }
        }
    }
    for (seq, uid, flags, ms, changed) in &results {
        let uid_part = if uid_mode { format!("UID {uid} ") } else { String::new() };
        let ms_part = if condstore { format!(" MODSEQ ({ms})") } else { String::new() };
        if !silent {
            out.extend_from_slice(
                format!("* {seq} FETCH ({uid_part}FLAGS {}{ms_part})\r\n", flags_list(flags)).as_bytes(),
            );
        } else if condstore && *changed {
            out.extend_from_slice(format!("* {seq} FETCH ({uid_part}MODSEQ ({ms}))\r\n").as_bytes());
        }
    }
    if !modified.is_empty() {
        modified.sort_unstable();
        return Ok(format!("[MODIFIED {}] Conditional STORE failed", format_set(&modified)));
    }
    Ok(format!("{}STORE completed", if uid_mode { "UID " } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_attributes() {
        let atts =
            parse_atts(&mut Parser::new(b"(UID FLAGS BODY.PEEK[HEADER.FIELDS (Subject)] RFC822.SIZE)"))
                .unwrap();
        assert_eq!(atts.len(), 4);
        assert!(!atts.iter().any(Att::sets_seen));
        let atts = parse_atts(&mut Parser::new(b"BODY[]<0.10>")).unwrap();
        assert!(atts.iter().any(Att::sets_seen));
        assert_eq!(parse_atts(&mut Parser::new(b"FULL")).unwrap().len(), 5);
        assert!(parse_atts(&mut Parser::new(b"(BOGUS)")).is_err());
        assert!(parse_atts(&mut Parser::new(b"()")).is_err());
    }
}
