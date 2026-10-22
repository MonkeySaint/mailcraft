//! Server copies kept across an edit: a vCard (RFC 6350 / 2426) or iCalendar object (RFC 5545) that came
//! from a CardDAV / CalDAV server holds more than MailCraft models (properties it does not read, a fourth
//! e-mail address, extra alarms, attachments by URI, other clients' `X-` properties). When MailCraft uploads
//! its own copy of an edited item, it starts from the server's copy instead and replaces only what changed:
//!
//! - `server`: the copy last read from the server (as [`kept_calendar`] / [`kept_card`] keep it);
//! - `base`: what MailCraft writes for that copy as it read it;
//! - `mine`: what MailCraft writes for the item now.
//!
//! A part MailCraft writes the same in `base` and `mine` was not changed here, so the server's lines for it
//! go back as they were (with whatever MailCraft does not model about them); a changed part takes
//! MailCraft's lines; a part only the server has is kept.
//!
//! - Cards are merged by contact field: every line the reader takes a field from is that field's, whatever
//!   its name (`X-EVOLUTION-SPOUSE`, `X-MS-SPOUSE`, `RELATED;TYPE=spouse` are all the spouse), so a changed
//!   field replaces all of them and no other client keeps showing the old value. In lists, only the lines
//!   read are the field's: a fourth e-mail address, a second web page or a `RELATED;TYPE=child` stays when
//!   the ones MailCraft shows are edited. MailCraft's lines go where the server's first line of the field
//!   was, so the card reads the same here and elsewhere.
//! - Calendar components are merged by property name. Start, end, duration and due change together, and so
//!   do the recurrence rule, dates and exceptions, so a kept part never contradicts a changed one. The
//!   organizer and attendees are always MailCraft's when it writes any (they say SCHEDULE-AGENT=CLIENT, RFC
//!   6638: MailCraft mails invitations, the server must not); attendees of an item MailCraft does not model
//!   them for stay, marked SCHEDULE-AGENT=CLIENT. Sub-components (alarms) are kept when MailCraft's are
//!   unchanged; time zone definitions are merged by TZID; the events and tasks of a calendar object are
//!   matched by component name and RECURRENCE-ID instant. Inline files of an event are the item's own
//!   (MailCraft holds them), plus the server's inline files it did not take (over its limits) and files by
//!   URI.
//!
//! Stamps (`DTSTAMP`, `LAST-MODIFIED`, `REV`, `PRODID`, `VERSION`) are always MailCraft's. Every step groups
//! lines and components by name once, so a merge takes time in proportion to the size of the copies.

use std::collections::{HashMap, HashSet};

use crate::ics::map::{Ctx, held_attachments};
use crate::ics::parse::{self, Component};
use crate::ics::tz::{TimeZone, Zones};
use crate::text::ContentLine;

/// Largest server copy kept (bytes); a larger one is not kept (the item then goes up as MailCraft writes it).
pub const MAX_KEPT_BYTES: usize = crate::ics::MAX_ICS_BYTES;

/// Components of a calendar object matched one to one by RECURRENCE-ID.
const INSTANCES: [&str; 3] = ["VEVENT", "VTODO", "VJOURNAL"];

/// Properties that change together: when one of a group changed, MailCraft's lines are used for all.
const UNITS: [&[&str]; 2] =
    [&["DTSTART", "DTEND", "DURATION", "DUE"], &["RRULE", "RDATE", "EXDATE", "EXRULE"]];

/// Properties always written as MailCraft has them.
const STAMPS: [&str; 5] = ["DTSTAMP", "LAST-MODIFIED", "REV", "PRODID", "VERSION"];

/// The people of a scheduled item (RFC 6638 implicit scheduling acts on them).
const PEOPLE: [&str; 2] = ["ORGANIZER", "ATTENDEE"];

/// What to keep of a server iCalendar object: its components without the inline files an item takes from
/// them (they go back from the item). The files held are those of the events the calendar reader takes
/// for one item, as a calendar folder reads them: the first event's UID, at most `max_events` events (0 for
/// a tasks folder, which holds no event's files). Empty when it is too large to keep or holds no component.
pub fn kept_calendar(server: &str, max_events: usize) -> String {
    if server.len() > MAX_KEPT_BYTES {
        return String::new();
    }
    let mut top = parse::parse(server);
    for (t, child) in events_held(&top, max_events) {
        let event = match child {
            Some(i) => top.get_mut(t).and_then(|c| c.children.get_mut(i)),
            None => top.get_mut(t),
        };
        if let Some(event) = event {
            drop_held(event);
        }
    }
    let mut out = String::new();
    for c in &top {
        c.write(&mut out);
    }
    out
}

/// The events whose inline files an item holds, as (top-level index, child index): in the order and
/// within the limits of [`crate::ics::read_str`] (the VCALENDARs' events, else bare ones; a start it can
/// read; at most [`crate::ics::MAX_ITEMS`] events and tasks), the ones sharing the first one's UID, at most
/// `max`.
fn events_held(top: &[Component], max: usize) -> Vec<(usize, Option<usize>)> {
    if max == 0 {
        return Vec::new();
    }
    let item = |c: &Component| matches!(c.name.as_str(), "VEVENT" | "VTODO");
    let mut list: Vec<(usize, Option<usize>, &Component)> = Vec::new();
    let mut zones = Zones::default();
    let mut zone = |z: &Component| {
        if zones.defined.len() < crate::ics::MAX_ZONES
            && let Some(tz) = TimeZone::from_component(z)
            && !zones.defined.iter().any(|d| d.tzid.eq_ignore_ascii_case(&tz.tzid))
        {
            zones.defined.push(tz);
        }
    };
    if top.iter().any(|c| c.name == "VCALENDAR") {
        for (t, cal) in top.iter().enumerate().filter(|(_, c)| c.name == "VCALENDAR") {
            cal.children_named("VTIMEZONE").for_each(&mut zone);
            list.extend(
                cal.children.iter().enumerate().filter(|(_, c)| item(c)).map(|(j, c)| (t, Some(j), c)),
            );
        }
    } else {
        top.iter().filter(|c| c.name == "VTIMEZONE").for_each(&mut zone);
        list.extend(top.iter().enumerate().filter(|(_, c)| item(c)).map(|(t, c)| (t, None, c)));
    }
    let ctx = Ctx::new(&zones, 0);
    let mut out = Vec::new();
    // The first event's UID (`Some(None)`: it has none, so only it is the item's).
    let mut first: Option<Option<String>> = None;
    for (t, j, c) in list.into_iter().take(crate::ics::MAX_ITEMS) {
        if c.name != "VEVENT" || ctx.time(c, "DTSTART").is_none() {
            continue;
        }
        let uid = c.text("UID").map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        let same = match &first {
            None => {
                first = Some(uid);
                true
            }
            Some(f) => f.is_some() && *f == uid,
        };
        if same {
            out.push((t, j));
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// What to keep of a server vCard: the text as it came (empty when it is too large to keep).
pub fn kept_card(server: &str) -> String {
    if server.len() > MAX_KEPT_BYTES { String::new() } else { server.to_owned() }
}

/// Removes the inline files the reader takes from `event`.
fn drop_held(event: &mut Component) {
    let held: HashSet<usize> = held_attachments(event).into_iter().map(|(i, _)| i).collect();
    if held.is_empty() {
        return;
    }
    let props = std::mem::take(&mut event.props);
    event.props = props.into_iter().enumerate().filter(|(i, _)| !held.contains(i)).map(|(_, p)| p).collect();
}

/// What a card line is merged as: the contact field it feeds, else its property name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key<'a> {
    Field(&'static str),
    Name(&'a str),
}

/// The merge key of every line of a card.
fn card_keys(card: &Component) -> Vec<Key<'_>> {
    let fields = crate::vcf::line_fields(&card.props);
    card.props
        .iter()
        .enumerate()
        .map(|(i, p)| match fields.get(i).copied().flatten() {
            Some(f) => Key::Field(f),
            None => Key::Name(p.name.as_str()),
        })
        .collect()
}

/// Lines grouped by key, in order.
fn by_key<'a>(lines: &'a [ContentLine], keys: &[Key<'a>]) -> HashMap<Key<'a>, Vec<&'a ContentLine>> {
    let mut out: HashMap<Key<'a>, Vec<&'a ContentLine>> = HashMap::new();
    for (line, k) in lines.iter().zip(keys) {
        out.entry(*k).or_default().push(line);
    }
    out
}

/// The lines of key `k` (none when it has none).
fn lines_of<'m, 'a>(g: &'m HashMap<Key<'a>, Vec<&'a ContentLine>>, k: &Key<'a>) -> &'m [&'a ContentLine] {
    g.get(k).map(Vec::as_slice).unwrap_or(&[])
}

/// The vCard to upload: `mine` rebuilt on the server's card (see the module docs). `None` when the server
/// copy has no card or is a vCard 2.1 (its quoted-printable lines cannot be kept as they are): upload
/// `mine` then. `base` and `mine` should be written in the server card's version ([`card_version`]).
pub fn patch_card(server: &str, base: &str, mine: &str) -> Option<String> {
    let server_top = parse::parse(server);
    let s = server_top.iter().find(|c| c.name == "VCARD")?;
    if s.prop("VERSION").is_some_and(|v| v.value.trim().starts_with("2.")) {
        return None;
    }
    let b = parse::parse(base).into_iter().find(|c| c.name == "VCARD").unwrap_or_default();
    let m = parse::parse(mine).into_iter().find(|c| c.name == "VCARD")?;
    let (sk, bk, mk) = (card_keys(s), card_keys(&b), card_keys(&m));
    let (bg, mg) = (by_key(&b.props, &bk), by_key(&m.props, &mk));
    let take_mine =
        |k: &Key<'_>| matches!(k, Key::Name(n) if STAMPS.contains(n)) || lines_of(&bg, k) != lines_of(&mg, k);
    let mut out: Vec<ContentLine> = Vec::new();
    let mut done: HashSet<Key<'_>> = HashSet::new();
    // VERSION comes right after BEGIN (RFC 6350 section 6.7.9).
    let version = Key::Name("VERSION");
    out.extend(lines_of(&mg, &version).iter().map(|l| (*l).clone()));
    done.insert(version);
    // Groups of server lines left out (their X-ABLABEL goes with them).
    let mut dropped: HashSet<String> = HashSet::new();
    for (line, k) in s.props.iter().zip(&sk) {
        if take_mine(k) {
            if done.insert(*k) {
                out.extend(lines_of(&mg, k).iter().map(|l| (*l).clone()));
            }
            if let Some(g) = &line.group {
                dropped.insert(g.to_ascii_lowercase());
            }
        } else {
            out.push(line.clone());
        }
    }
    // What MailCraft changed or added that the server's card has no line for.
    let on_server: HashSet<&Key<'_>> = sk.iter().collect();
    for (line, k) in m.props.iter().zip(&mk) {
        if !done.contains(k) && !on_server.contains(k) && take_mine(k) {
            out.push(line.clone());
        }
    }
    if !dropped.is_empty() {
        let label = |l: &ContentLine| l.name == "X-ABLABEL";
        let in_use: HashSet<String> = out
            .iter()
            .filter(|l| !label(l))
            .filter_map(|l| l.group.as_ref().map(|g| g.to_ascii_lowercase()))
            .collect();
        out.retain(|l| {
            let g = l.group.as_ref().map(|g| g.to_ascii_lowercase());
            !(label(l) && g.is_some_and(|g| dropped.contains(&g) && !in_use.contains(&g)))
        });
    }
    let merged = Component { name: m.name.clone(), props: out, children: Vec::new() };
    let mut text = String::new();
    merged.write(&mut text);
    Some(text)
}

/// The vCard version of a server card: 3.0 cards are kept as 3.0, everything else is written as 4.0.
pub fn card_version(server: &str) -> crate::vcf::Version {
    let v3 = parse::parse(server)
        .iter()
        .find(|c| c.name == "VCARD")
        .and_then(|c| c.prop("VERSION"))
        .is_some_and(|v| v.value.trim() == "3.0");
    if v3 { crate::vcf::Version::V3 } else { crate::vcf::Version::V4 }
}

/// The iCalendar object to upload: `mine` rebuilt on the server's object (see the module docs). `None` when
/// the server copy has no VCALENDAR or MailCraft's copy none: upload `mine` then. `local_offset_min` places
/// floating RECURRENCE-ID times, as when reading.
pub fn patch_calendar(server: &str, base: &str, mine: &str, local_offset_min: i32) -> Option<String> {
    let server_top = parse::parse(server);
    let s = server_top.iter().find(|c| c.name == "VCALENDAR")?;
    let b = parse::parse(base).into_iter().find(|c| c.name == "VCALENDAR").unwrap_or_default();
    let m = parse::parse(mine).into_iter().find(|c| c.name == "VCALENDAR")?;
    let (sz, bz, mz) = (zones_of(s), zones_of(&b), zones_of(&m));
    let ctx =
        [Ctx::new(&sz, local_offset_min), Ctx::new(&bz, local_offset_min), Ctx::new(&mz, local_offset_min)];
    let mut cal = Component {
        name: m.name.clone(),
        props: merge_props(s, &b, &m),
        children: merge_children(s, &b, &m, |c| !INSTANCES.contains(&c.name.as_str())),
    };
    // Events and tasks, matched by name and instant: the i-th of a key on each side go together.
    let keyed = |c: &Component, ctx: &Ctx<'_>| -> Vec<(String, usize)> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        c.children
            .iter()
            .enumerate()
            .filter(|(_, x)| INSTANCES.contains(&x.name.as_str()))
            .map(|(i, x)| {
                let k = instance_key(x, ctx);
                let n = seen.entry(k.clone()).or_insert(0);
                let key = format!("{k}\u{0}{n}");
                *n = n.saturating_add(1);
                (key, i)
            })
            .collect()
    };
    let [s_ctx, b_ctx, m_ctx] = &ctx;
    let (sk, bk, mk) = (keyed(s, s_ctx), keyed(&b, b_ctx), keyed(&m, m_ctx));
    let server_at: HashMap<&str, usize> = sk.iter().map(|(k, i)| (k.as_str(), *i)).collect();
    let base_at: HashMap<&str, usize> = bk.iter().map(|(k, i)| (k.as_str(), *i)).collect();
    let mine_keys: HashSet<&str> = mk.iter().map(|(k, _)| k.as_str()).collect();
    for (k, i) in &mk {
        let Some(mc) = m.children.get(*i) else { continue };
        let base_c = base_at.get(k.as_str()).and_then(|j| b.children.get(*j));
        match server_at.get(k.as_str()).and_then(|j| s.children.get(*j)) {
            Some(on_server) => {
                cal.children.push(merge(on_server, base_c.unwrap_or(&Component::default()), mc))
            }
            None => cal.children.push(mc.clone()),
        }
    }
    // Server instances MailCraft did not read (another UID, more than it keeps) go back; the ones it read
    // and no longer has were deleted here.
    for (k, i) in &sk {
        if mine_keys.contains(k.as_str()) || base_at.contains_key(k.as_str()) {
            continue;
        }
        if let Some(c) = s.children.get(*i) {
            cal.children.push(c.clone());
        }
    }
    let mut out = String::new();
    cal.write(&mut out);
    Some(out)
}

/// The time zones a calendar defines.
fn zones_of(cal: &Component) -> Zones {
    let mut z = Zones::default();
    for c in cal.children_named("VTIMEZONE").take(crate::ics::MAX_ZONES) {
        if let Some(tz) = TimeZone::from_component(c) {
            z.defined.push(tz);
        }
    }
    z
}

/// Component name and RECURRENCE-ID (as an instant when it can be read, else as written; empty for a series
/// or a lone item).
fn instance_key(c: &Component, ctx: &Ctx<'_>) -> String {
    let rid = match c.prop("RECURRENCE-ID") {
        None => String::new(),
        Some(p) => match ctx.time(c, "RECURRENCE-ID") {
            Some((_, at)) => at.unix.to_string(),
            None => format!("?{}", p.value.trim()),
        },
    };
    format!("{}\u{0}{rid}", c.name)
}

/// `server` with `mine`'s changes since `base` (props and sub-components; see the module docs).
fn merge(server: &Component, base: &Component, mine: &Component) -> Component {
    Component {
        name: mine.name.clone(),
        props: merge_props(server, base, mine),
        children: merge_children(server, base, mine, |_| true),
    }
}

/// Lines grouped by property name, in order.
fn by_name(c: &Component) -> HashMap<&str, Vec<&ContentLine>> {
    let mut out: HashMap<&str, Vec<&ContentLine>> = HashMap::new();
    for p in &c.props {
        out.entry(p.name.as_str()).or_default().push(p);
    }
    out
}

/// Property names in first-seen order: `mine`'s, then those only `server` has.
fn prop_names<'a>(server: &'a Component, mine: &'a Component) -> Vec<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for p in mine.props.iter().chain(&server.props) {
        if seen.insert(p.name.as_str()) {
            out.push(p.name.as_str());
        }
    }
    out
}

/// `line` marked as scheduled by the client (RFC 6638 section 7.1): the server sends no invitation for it.
fn client_scheduled(line: &ContentLine) -> ContentLine {
    let mut l = line.clone();
    l.params.retain(|(n, _)| {
        !n.eq_ignore_ascii_case("SCHEDULE-AGENT") && !n.eq_ignore_ascii_case("SCHEDULE-FORCE-SEND")
    });
    l.params.push(("SCHEDULE-AGENT".to_owned(), vec!["CLIENT".to_owned()]));
    l
}

fn merge_props(server: &Component, base: &Component, mine: &Component) -> Vec<ContentLine> {
    let (s, b, m) = (by_name(server), by_name(base), by_name(mine));
    let empty: Vec<&ContentLine> = Vec::new();
    let of = |g: &HashMap<&str, Vec<&ContentLine>>, n: &str| -> Vec<ContentLine> {
        g.get(n).unwrap_or(&empty).iter().map(|l| (*l).clone()).collect()
    };
    let changed = |n: &str| b.get(n).unwrap_or(&empty) != m.get(n).unwrap_or(&empty);
    let instance = INSTANCES.contains(&mine.name.as_str());
    // MailCraft models the people of this item: its lines (with SCHEDULE-AGENT=CLIENT) are the ones to send.
    let people_here = PEOPLE.iter().any(|n| b.contains_key(n) || m.contains_key(n));
    let mut out: Vec<ContentLine> = Vec::new();
    for n in prop_names(server, mine) {
        if n == "ATTACH" && mine.name == "VEVENT" {
            // The item's files, then the server's that MailCraft did not take (kept copies hold no others).
            out.extend(of(&m, n));
            out.extend(of(&s, n));
        } else if instance && PEOPLE.contains(&n) {
            if people_here {
                out.extend(of(&m, n));
            } else {
                out.extend(s.get(n).unwrap_or(&empty).iter().map(|l| client_scheduled(l)));
            }
        } else {
            let take_mine = match UNITS.iter().find(|u| u.contains(&n)) {
                _ if STAMPS.contains(&n) => true,
                Some(unit) => unit.iter().any(|u| changed(u)),
                None => changed(n),
            };
            out.extend(if take_mine { of(&m, n) } else { of(&s, n) });
        }
    }
    out
}

/// Sub-components passing `pick`, grouped by name, with the names in first-seen order.
fn children_by_name<'a>(
    c: &'a Component,
    pick: &impl Fn(&Component) -> bool,
) -> (Vec<&'a str>, HashMap<&'a str, Vec<&'a Component>>) {
    let mut order = Vec::new();
    let mut out: HashMap<&str, Vec<&Component>> = HashMap::new();
    for x in c.children.iter().filter(|x| pick(x)) {
        let list = out.entry(x.name.as_str()).or_default();
        if list.is_empty() {
            order.push(x.name.as_str());
        }
        list.push(x);
    }
    (order, out)
}

/// Sub-components passing `pick`, by name: time zones merged by TZID (MailCraft's, then the server's other
/// definitions), others the server's when MailCraft's are unchanged since `base`, else MailCraft's.
fn merge_children(
    server: &Component,
    base: &Component,
    mine: &Component,
    pick: impl Fn(&Component) -> bool,
) -> Vec<Component> {
    let (mine_names, m) = children_by_name(mine, &pick);
    let (server_names, s) = children_by_name(server, &pick);
    let (_, b) = children_by_name(base, &pick);
    let empty: Vec<&Component> = Vec::new();
    let of = |g: &HashMap<&str, Vec<&Component>>, n: &str| -> Vec<Component> {
        g.get(n).unwrap_or(&empty).iter().map(|c| (*c).clone()).collect()
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for n in mine_names.into_iter().chain(server_names) {
        if !seen.insert(n) {
            continue;
        }
        if n == "VTIMEZONE" {
            let tzid = |c: &Component| c.prop("TZID").map(|p| p.value.trim().to_ascii_lowercase());
            let mine_z = of(&m, n);
            let have: HashSet<String> = mine_z.iter().filter_map(tzid).collect();
            out.extend(mine_z);
            out.extend(of(&s, n).into_iter().filter(|z| tzid(z).is_none_or(|t| !have.contains(&t))));
        } else if b.get(n).unwrap_or(&empty) == m.get(n).unwrap_or(&empty) {
            out.extend(of(&s, n));
        } else {
            out.extend(of(&m, n));
        }
    }
    out
}
