//! `compose.*`: the compose window / inline response (header fields, send, drafts, attachments, tracking and
//! delivery options, themes). Every command takes an optional `{"compose": id}`; without it the most recently
//! opened compose is used.
//!
//! Body-edit commands here (`compose.select`, `compose.insertText`, `compose.deleteText`, `compose.editUndo`,
//! `compose.editRedo`) drive the editor model in [`crate::compose`] for agents and tests; they change only the
//! compose (not the mailbox), so they do not record mailbox undo (`mutates: false`) and use the compose's own
//! editor undo instead.

use serde_json::{Value, json};

use crate::cmd::mail::{FlagKind, flag_dates};
use crate::cmd::util::{dialog, opt_bool, opt_str, opt_u64, req_str};
use crate::compose::{
    Bookmark, ComposeAttachment, ComposeKind, ComposeState, DocPos, EditRun, SaveSentTo, SignatureBlock,
};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session};
use mailcraft_mime::{Address, AttachmentSpec, MessageBuilder, parse_address_list};
use mailcraft_model::{
    Account, Block, BodyFormat, DateTime, Doc, FlagState, FolderKind, FolderRole, Importance, ItemId,
    MailItem, ResponseState, Sensitivity,
};

/// Error shown when sending without an account that can send (plan/outlook/reading-pane.md).
pub const NO_ACCOUNT: &str = "No valid email accounts are configured. Add an account to send email.";
/// Largest file Attach File accepts.
pub const MAX_ATTACHMENT_BYTES: u64 = 150 * 1024 * 1024;
/// Most attachments on one draft.
const MAX_ATTACHMENTS: usize = 500;

// =======================================================================================================
// Shared helpers (used by format/insert/review too)
// =======================================================================================================

/// Builds a [`CommandSpec`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    shortcut: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut, params, mutates, enabled, run }
}

/// The compose a command acts on: `params.compose` (number or numeric string) or the most recent one.
pub(crate) fn compose_id(s: &Session, p: &Value) -> Result<u64, CmdError> {
    match p.get("compose") {
        Some(v) if !v.is_null() => {
            let id = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))
                .ok_or_else(|| CmdError::Params("`compose` must be a compose id".into()))?;
            if s.composes.contains_key(&id) {
                Ok(id)
            } else {
                Err(CmdError::Params(format!("no open compose with id {id}")))
            }
        }
        _ => s.composes.keys().next_back().copied().ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into())),
    }
}

const NO_COMPOSE: &str = "no message is being composed";

/// The compose a command acts on, mutably, with its body normalised.
pub(crate) fn compose_mut<'a>(s: &'a mut Session, p: &Value) -> Result<&'a mut ComposeState, CmdError> {
    let id = compose_id(s, p)?;
    let c = s.composes.get_mut(&id).ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
    c.ensure_body();
    Ok(c)
}

/// The most recent compose.
pub(crate) fn latest(s: &Session) -> Option<&ComposeState> {
    s.composes.values().next_back()
}

/// Enabled: a compose is open.
pub(crate) fn has_compose(s: &Session) -> Option<&'static str> {
    if s.composes.is_empty() { Some(NO_COMPOSE) } else { None }
}

/// Enabled: a compose with a formatted (HTML / Rich Text) body is open.
pub(crate) fn rich_compose(s: &Session) -> Option<&'static str> {
    match latest(s) {
        None => Some(NO_COMPOSE),
        Some(c) if c.format == BodyFormat::PlainText => Some("formatting is not available in plain text"),
        Some(_) => None,
    }
}

/// Enabled: an open compose has selected body text (the command re-checks its target compose).
pub(crate) fn has_body_selection(s: &Session) -> Option<&'static str> {
    if s.composes.is_empty() {
        Some(NO_COMPOSE)
    } else if s.composes.values().all(|c| c.selection.is_collapsed()) {
        Some("select some text first")
    } else {
        None
    }
}

/// Enabled: dark mode is on and a compose is open.
fn dark_compose(s: &Session) -> Option<&'static str> {
    has_compose(s).or_else(|| (!s.view.dark).then_some("dark mode is off"))
}

/// `{"value": string}` or the first string-valued key among `keys`.
fn str_param<'a>(p: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| opt_str(p, k))
}

/// Parses `{"path": [..], "offset": n}` into a body position.
pub(crate) fn pos_param(v: &Value) -> Result<DocPos, CmdError> {
    let path: Vec<usize> = match v.get("path") {
        Some(Value::Array(a)) => {
            a.iter().map(|x| x.as_u64().and_then(|n| usize::try_from(n).ok())).collect::<Option<_>>()
        }
        None | Some(Value::Null) => Some(Vec::new()),
        Some(_) => None,
    }
    .ok_or_else(|| CmdError::Params("`path` must be an array of indices".into()))?;
    if path.len() > crate::compose::MAX_PATH_DEPTH {
        return Err(CmdError::Params("`path` is too deep".into()));
    }
    let offset = v.get("offset").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    Ok(DocPos::new(path, offset))
}

/// JSON form of a position.
pub(crate) fn pos_json(p: &DocPos) -> Value {
    json!({"path": p.path, "offset": p.offset})
}

/// Parses a date/time: RFC 3339 / ISO 8601 (`2026-10-09`, `2026-10-09T15:30`, with optional `Z` / `±hh:mm`;
/// no zone = the session's local offset) or RFC 5322.
pub(crate) fn parse_when(s: &Session, text: &str) -> Option<DateTime> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<u32> {
        let part = t.get(a..b)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    if t.len() >= 10 && t.get(4..5) == Some("-") && t.get(7..8) == Some("-") {
        let y = i32::try_from(num(0, 4)?).ok()?;
        let mo = u8::try_from(num(5, 7)?).ok()?;
        let d = u8::try_from(num(8, 10)?).ok()?;
        let rest = t.get(10..)?;
        let (mut h, mut mi, mut sec) = (0u8, 0u8, 0u8);
        let mut offset = s.tz_offset_min;
        if let Some(time) = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')) {
            let tn = |a: usize, b: usize| -> Option<u8> {
                let part = time.get(a..b)?;
                if !part.bytes().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                part.parse().ok()
            };
            h = tn(0, 2)?;
            if time.get(2..3) != Some(":") {
                return None;
            }
            mi = tn(3, 5)?;
            let mut zone = time.get(5..)?;
            if let Some(z) = zone.strip_prefix(':') {
                sec = tn(6, 8)?;
                zone = z.get(2..)?;
            }
            // Fractional seconds are ignored.
            if let Some(z) = zone.strip_prefix('.') {
                zone = z.trim_start_matches(|c: char| c.is_ascii_digit());
            }
            match zone {
                "" => {}
                "Z" | "z" => offset = 0,
                z => {
                    let sign = match z.get(0..1)? {
                        "+" => 1,
                        "-" => -1,
                        _ => return None,
                    };
                    let zh: i32 = z.get(1..3)?.parse().ok()?;
                    let zm: i32 = z.get(4..6).or_else(|| z.get(3..5))?.parse().ok()?;
                    offset = sign * (zh * 60 + zm);
                }
            }
        } else if !rest.is_empty() {
            return None;
        }
        return DateTime::from_civil(y, mo, d, h, mi, sec, offset);
    }
    DateTime::parse_rfc5322(t).ok()
}

/// The account that sends this compose: the one matching its From address, else the default account, else
/// the current folder's account, else any account that can send. With `need_send`, only sending accounts.
pub(crate) fn sending_account(s: &Session, c: &ComposeState, need_send: bool) -> Option<Account> {
    let from = parse_field_mailbox(&c.from);
    let ok = |a: &&Account| !need_send || a.can_send();
    let by_from = from.as_ref().and_then(|m| {
        s.mailbox
            .accounts
            .iter()
            .filter(ok)
            .find(|a| !a.address.is_empty() && a.address.eq_ignore_ascii_case(&m.addr))
    });
    by_from
        .or_else(|| s.mailbox.default_account().filter(ok))
        .or_else(|| s.current_account().and_then(|id| s.mailbox.account(id)).filter(ok))
        .or_else(|| s.mailbox.accounts.iter().find(ok))
        .cloned()
}

/// Whether `addr` looks like `local@domain`.
fn valid_addr(addr: &str) -> bool {
    let Some((local, domain)) = addr.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !addr.chars().any(char::is_whitespace)
        && !domain.starts_with('.')
}

/// Splits a recipient field at the `;` separators that are outside quoted names and angle brackets; the
/// entries come back trimmed, empty ones dropped.
pub(crate) fn split_recipients(field: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
    let mut start = 0usize;
    for (i, ch) in field.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ';' if !quoted && !angle => {
                out.extend(field.get(start..i));
                start = i.saturating_add(1);
            }
            _ => {}
        }
    }
    out.extend(field.get(start..));
    out.into_iter().map(str::trim).filter(|x| !x.is_empty()).collect()
}

/// One entry written as an unquoted display name with commas and an angle address ("Rivera, Alice
/// <alice@example.com>", the way directory names are shown): a single mailbox, not a comma-separated list.
fn comma_named_mailbox(entry: &str) -> Option<mailcraft_mime::Mailbox> {
    let (name, addr) = entry.trim().strip_suffix('>')?.rsplit_once('<')?;
    let name = name.trim();
    if !name.contains(',') || name.contains(['"', '@', '<', '>', ';']) {
        return None;
    }
    Some(mailcraft_mime::Mailbox::new(Some(name), addr.trim()))
}

/// A mailbox as written into a recipient field: "Name <address>" (or the bare address), with the name
/// quoted only when it holds a character that would split or break the entry (`;`, `"`, `<`, `>`).
pub(crate) fn field_mailbox(m: &mailcraft_mime::Mailbox) -> String {
    match m.name.as_deref().map(str::trim) {
        Some(n) if n.contains([';', '"', '<', '>']) => mailcraft_mime::addr::format_mailbox(m, false),
        Some(n) if !n.is_empty() => format!("{n} <{}>", m.addr),
        _ => m.addr.clone(),
    }
}

/// The first mailbox of a single-address field such as From (same rules as [`parse_recipients`]).
pub(crate) fn parse_field_mailbox(text: &str) -> Option<mailcraft_mime::Mailbox> {
    let first = split_recipients(text).into_iter().next()?;
    comma_named_mailbox(first).or_else(|| mailcraft_mime::addr::parse_mailbox(first))
}

/// Parses a recipient field ("a@b; Name <c@d>; Last, First <e@f>"). Returns the addresses, or the entries
/// that are not valid addresses.
pub(crate) fn parse_recipients(field: &str) -> Result<Vec<Address>, Vec<String>> {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for piece in split_recipients(field) {
        if let Some(m) = comma_named_mailbox(piece) {
            if valid_addr(&m.addr) {
                out.push(Address::Mailbox(m));
            } else {
                bad.push(piece.to_owned());
            }
            continue;
        }
        let list = parse_address_list(piece);
        if list.is_empty() {
            bad.push(piece.to_owned());
            continue;
        }
        for a in list {
            if a.mailboxes().iter().all(|m| valid_addr(&m.addr)) {
                out.push(a);
            } else {
                for m in a.mailboxes().iter().filter(|m| !valid_addr(&m.addr)) {
                    bad.push(m.to_display_string());
                }
            }
        }
    }
    if bad.is_empty() { Ok(out) } else { Err(bad) }
}

/// MIME type guessed from a file name's extension (table in [`crate::compose::mime_for_name`]).
pub(crate) fn mime_for(name: &str) -> &'static str {
    crate::compose::mime_for_name(name)
}

/// Reads a local file for attaching / inserting (size-capped). Returns (file name, bytes).
pub(crate) fn read_file(path: &str, cap: u64) -> Result<(String, Vec<u8>), CmdError> {
    let path = std::path::Path::new(path);
    let meta = std::fs::metadata(path)
        .map_err(|e| CmdError::Failed(format!("cannot read {}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(CmdError::Params(format!("{} is not a file", path.display())));
    }
    if meta.len() > cap {
        return Err(CmdError::Failed(format!(
            "{} is too large ({} MB; the limit is {} MB)",
            path.display(),
            meta.len() / (1024 * 1024),
            cap / (1024 * 1024)
        )));
    }
    let data =
        std::fs::read(path).map_err(|e| CmdError::Failed(format!("cannot read {}: {e}", path.display())))?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > cap {
        return Err(CmdError::Failed(format!("{} is too large", path.display())));
    }
    let name =
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "attachment".into());
    Ok((name, data))
}

/// File bytes from `{"path"}` or `{"name", "data_base64"}`; `None` when neither is given.
pub(crate) fn file_param(p: &Value, cap: u64) -> Result<Option<(String, Vec<u8>)>, CmdError> {
    if let Some(path) = opt_str(p, "path").filter(|x| !x.trim().is_empty()) {
        return read_file(path, cap).map(Some);
    }
    if let Some(b64) = opt_str(p, "data_base64") {
        let est = u64::try_from(b64.len()).unwrap_or(u64::MAX) / 4 * 3;
        if est > cap.saturating_add(3) {
            return Err(CmdError::Failed("the file is too large".into()));
        }
        let data = mailcraft_mime::transfer::decode_base64(b64.as_bytes());
        let name =
            opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("attachment").to_owned();
        return Ok(Some((name, data)));
    }
    Ok(None)
}

/// Escapes a vCard / iCalendar text value (RFC 6350 §3.4, RFC 5545 §3.3.11).
pub(crate) fn escape_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds a content line at 75 octets (RFC 5545 §3.1 / RFC 6350 §3.2) and appends CRLF.
pub(crate) fn fold_line(line: &str, out: &mut String) {
    let mut used = 0usize;
    for c in line.chars() {
        let n = c.len_utf8();
        if used.saturating_add(n) > 75 {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used = used.saturating_add(n);
    }
    out.push_str("\r\n");
}

/// vCard 3.0 (RFC 2426) for a contact.
pub(crate) fn vcard(c: &mailcraft_model::Contact) -> String {
    let mut out = String::new();
    let full = if c.full_name().is_empty() { c.file_as.clone() } else { c.full_name() };
    fold_line("BEGIN:VCARD", &mut out);
    fold_line("VERSION:3.0", &mut out);
    fold_line(
        &format!(
            "N:{};{};{};{};{}",
            escape_value(&c.last_name),
            escape_value(&c.first_name),
            escape_value(&c.middle_name),
            escape_value(&c.title),
            escape_value(&c.suffix)
        ),
        &mut out,
    );
    fold_line(&format!("FN:{}", escape_value(&full)), &mut out);
    if !c.company.is_empty() || !c.department.is_empty() {
        fold_line(&format!("ORG:{};{}", escape_value(&c.company), escape_value(&c.department)), &mut out);
    }
    if !c.job_title.is_empty() {
        fold_line(&format!("TITLE:{}", escape_value(&c.job_title)), &mut out);
    }
    for e in c.emails.iter().filter(|e| !e.is_empty()) {
        fold_line(&format!("EMAIL;TYPE=INTERNET:{}", escape_value(e)), &mut out);
    }
    for (kind, num) in &c.phones {
        let t = match kind.to_ascii_lowercase().as_str() {
            "business" | "work" => "WORK,VOICE",
            "home" => "HOME,VOICE",
            "mobile" | "cell" => "CELL",
            "fax" | "business fax" => "WORK,FAX",
            _ => "VOICE",
        };
        fold_line(&format!("TEL;TYPE={t}:{}", escape_value(num)), &mut out);
    }
    for a in &c.addresses {
        let t = match a.kind.to_ascii_lowercase().as_str() {
            "business" | "work" => "WORK",
            "home" => "HOME",
            _ => "POSTAL",
        };
        fold_line(
            &format!(
                "ADR;TYPE={t}:;;{};{};{};{};{}",
                escape_value(&a.street),
                escape_value(&a.city),
                escape_value(&a.region),
                escape_value(&a.postal_code),
                escape_value(&a.country)
            ),
            &mut out,
        );
    }
    if !c.web_page.is_empty() {
        fold_line(&format!("URL:{}", escape_value(&c.web_page)), &mut out);
    }
    if let Some(b) = &c.birthday {
        fold_line(&format!("BDAY:{}", escape_value(b)), &mut out);
    }
    if !c.notes.is_empty() {
        fold_line(&format!("NOTE:{}", escape_value(&c.notes)), &mut out);
    }
    fold_line("END:VCARD", &mut out);
    out
}

/// `YYYYMMDDTHHMMSSZ`.
fn ics_utc(t: DateTime) -> String {
    let c = t.civil(0);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// iCalendar (RFC 5545) for an appointment.
pub(crate) fn ics_event(a: &mailcraft_model::Appointment, now: DateTime) -> String {
    let mut out = String::new();
    fold_line("BEGIN:VCALENDAR", &mut out);
    fold_line("VERSION:2.0", &mut out);
    fold_line("PRODID:-//MailCraft//MailCraft//EN", &mut out);
    fold_line("BEGIN:VEVENT", &mut out);
    let uid = if a.uid.is_empty() { format!("{}@mailcraft", a.id.0) } else { a.uid.clone() };
    fold_line(&format!("UID:{}", escape_value(&uid)), &mut out);
    fold_line(&format!("DTSTAMP:{}", ics_utc(now)), &mut out);
    if a.all_day {
        let s = a.start.civil(a.start.offset_min);
        let e = a.end.civil(a.end.offset_min);
        fold_line(&format!("DTSTART;VALUE=DATE:{:04}{:02}{:02}", s.year, s.month, s.day), &mut out);
        fold_line(&format!("DTEND;VALUE=DATE:{:04}{:02}{:02}", e.year, e.month, e.day), &mut out);
    } else {
        fold_line(&format!("DTSTART:{}", ics_utc(a.start)), &mut out);
        fold_line(&format!("DTEND:{}", ics_utc(a.end)), &mut out);
    }
    fold_line(&format!("SUMMARY:{}", escape_value(&a.subject)), &mut out);
    if !a.location.is_empty() {
        fold_line(&format!("LOCATION:{}", escape_value(&a.location)), &mut out);
    }
    if !a.body.is_empty() {
        fold_line(&format!("DESCRIPTION:{}", escape_value(&a.body)), &mut out);
    }
    if let Some(r) = &a.recurrence
        && !r.rrule.is_empty()
    {
        fold_line(&format!("RRULE:{}", r.rrule), &mut out);
    }
    fold_line("END:VEVENT", &mut out);
    fold_line("END:VCALENDAR", &mut out);
    out
}

/// iCalendar VTODO for a task.
fn ics_task(t: &mailcraft_model::Task, now: DateTime) -> String {
    let mut out = String::new();
    fold_line("BEGIN:VCALENDAR", &mut out);
    fold_line("VERSION:2.0", &mut out);
    fold_line("PRODID:-//MailCraft//MailCraft//EN", &mut out);
    fold_line("BEGIN:VTODO", &mut out);
    fold_line(&format!("UID:{}@mailcraft", t.id.0), &mut out);
    fold_line(&format!("DTSTAMP:{}", ics_utc(now)), &mut out);
    fold_line(&format!("SUMMARY:{}", escape_value(&t.subject)), &mut out);
    if let Some(d) = t.start {
        fold_line(&format!("DTSTART:{}", ics_utc(d)), &mut out);
    }
    if let Some(d) = t.due {
        fold_line(&format!("DUE:{}", ics_utc(d)), &mut out);
    }
    fold_line(&format!("PERCENT-COMPLETE:{}", t.percent_complete.min(100)), &mut out);
    if !t.body.is_empty() {
        fold_line(&format!("DESCRIPTION:{}", escape_value(&t.body)), &mut out);
    }
    fold_line("END:VTODO", &mut out);
    fold_line("END:VCALENDAR", &mut out);
    out
}

/// A file name made safe from `base` (control chars and path separators replaced).
pub(crate) fn safe_name(base: &str, fallback: &str, ext: &str) -> String {
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    format!("{}.{ext}", if clean.is_empty() { fallback } else { clean })
}

/// RFC 5322 source of a stored message (kept raw source, else rebuilt from its fields).
pub(crate) fn message_source(m: &MailItem) -> Vec<u8> {
    if let Some(raw) = &m.raw {
        return raw.clone();
    }
    let mut b = MessageBuilder::new();
    b.from = m.from.clone();
    b.to = m.to.iter().cloned().map(Address::Mailbox).collect();
    b.cc = m.cc.iter().cloned().map(Address::Mailbox).collect();
    b.subject = m.subject.clone();
    b.date = m.sent.or(Some(m.received));
    b.message_id = m.message_id.clone();
    b.text = m.body.text.clone().or_else(|| m.body.html.as_deref().map(mailcraft_html::html_to_text));
    b.html = m.body.html.clone();
    b.keep_bcc = true;
    b.build()
}

// =======================================================================================================
// Building / saving / sending
// =======================================================================================================

/// The RFC 5322 message for a compose. `for_draft` keeps Bcc and marks it unsent.
pub(crate) fn build_message(
    s: &Session,
    c: &ComposeState,
    account: Option<&Account>,
    for_draft: bool,
) -> MessageBuilder {
    let lenient = |f: &str| match parse_recipients(f) {
        Ok(v) => v,
        Err(_) => parse_address_list(f),
    };
    let mut b = MessageBuilder::new();
    let from = account.filter(|a| !a.address.trim().is_empty()).map(|a| {
        let name = if a.sender_name.trim().is_empty() { a.display_name.trim() } else { a.sender_name.trim() };
        mailcraft_mime::Mailbox::new(Some(name), a.address.trim())
    });
    b.from = from.clone().or_else(|| parse_field_mailbox(&c.from));
    b.to = lenient(&c.to);
    b.cc = lenient(&c.cc);
    b.bcc = lenient(&c.bcc);
    b.reply_to = lenient(&c.reply_to);
    b.subject = c.subject.clone();
    b.date = Some(c.delay_until.unwrap_or_else(|| s.now()));
    if matches!(c.kind, ComposeKind::Reply | ComposeKind::ReplyAll)
        && let Some(src) = c.source.and_then(|id| s.mailbox.mail.get(&id))
        && let Some(mid) = &src.message_id
    {
        b.in_reply_to = vec![mid.clone()];
        let mut refs = src.references.clone();
        refs.push(mid.clone());
        b.references = refs;
    }
    b.importance = match c.importance {
        Importance::High => Some("high".into()),
        Importance::Low => Some("low".into()),
        Importance::Normal => None,
    };
    b.text = Some(c.body.to_plain_text());
    if c.format != BodyFormat::PlainText {
        b.html = Some(mailcraft_html::doc_to_html(&c.body));
    }
    b.attachments = c
        .attachments
        .iter()
        .map(|a| AttachmentSpec {
            filename: a.filename.clone(),
            mime_type: a.mime_type.clone(),
            data: a.data.clone(),
            content_id: None,
        })
        .collect();
    if c.format != BodyFormat::PlainText {
        let used: Vec<String> = c.images().into_iter().map(|(src, _)| src).collect();
        for img in &c.inline_images {
            // Content-IDs compare case-insensitively (as the parser does when it marks inline parts).
            let referenced = used.iter().any(|u| {
                u.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("cid:"))
                    && u.get(4..).is_some_and(|id| id.eq_ignore_ascii_case(&img.content_id))
            });
            if referenced {
                b.attachments.push(AttachmentSpec {
                    filename: img.filename.clone(),
                    mime_type: img.mime_type.clone(),
                    data: img.data.clone(),
                    content_id: Some(img.content_id.clone()),
                });
            }
        }
    }
    let notify_to = b.from.as_ref().map(|m| mailcraft_mime::addr::format_mailbox(m, true));
    if let Some(addr) = &notify_to {
        if c.request_read_receipt {
            b.extra_headers.push(("Disposition-Notification-To".into(), addr.clone()));
        }
        if c.request_delivery_receipt {
            b.extra_headers.push(("Return-Receipt-To".into(), addr.clone()));
        }
    }
    match c.sensitivity {
        Sensitivity::Normal => {}
        Sensitivity::Personal => b.extra_headers.push(("Sensitivity".into(), "Personal".into())),
        Sensitivity::Private => b.extra_headers.push(("Sensitivity".into(), "Private".into())),
        Sensitivity::Confidential => {
            b.extra_headers.push(("Sensitivity".into(), "Company-Confidential".into()))
        }
    }
    if for_draft {
        b.extra_headers.push(("X-Unsent".into(), "1".into()));
    }
    b.keep_bcc = for_draft;
    b
}

/// A stored item for a built message.
fn item_from(b: &MessageBuilder, now: DateTime) -> (MailItem, Vec<u8>) {
    let raw = b.build();
    let item = match mailcraft_mime::parse(&raw) {
        Ok(msg) => MailItem::from_message(&msg, &raw, now),
        Err(_) => MailItem {
            subject: b.subject.clone(),
            received: now,
            raw: Some(raw.clone()),
            ..Default::default()
        },
    };
    (item, raw)
}

/// Saves compose `cid` as a draft (updating its draft item when it has one). Returns the draft id.
pub fn save_draft(s: &mut Session, cid: u64) -> Result<ItemId, CmdError> {
    let c = s.composes.get(&cid).cloned().ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
    let account = sending_account(s, &c, false);
    let existing = c.draft.and_then(|d| s.mailbox.mail.get(&d)).map(|m| (m.id, m.folder));
    let drafts = match existing {
        Some((_, f)) => Some(f),
        None => {
            account.as_ref().and_then(|a| s.mailbox.role_folder(a.id, FolderRole::Drafts)).or_else(|| {
                s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Drafts)).map(|f| f.id)
            })
        }
    };
    let Some(drafts) = drafts else {
        return Err(CmdError::Failed("the draft could not be saved: there is no Drafts folder".into()));
    };
    let now = s.now();
    let b = build_message(s, &c, account.as_ref(), true);
    let (mut item, _) = item_from(&b, now);
    item.folder = drafts;
    item.is_draft = true;
    item.read = true;
    item.received = now;
    item.sensitivity = c.sensitivity;
    item.flag = c.flag.clone();
    item.body.format = c.format;
    let id = match existing {
        Some((id, _)) => {
            item.id = id;
            s.mailbox.mail.insert(id, item);
            id
        }
        None => s.mailbox.add_mail(item),
    };
    if let Some(cm) = s.composes.get_mut(&cid) {
        cm.draft = Some(id);
        cm.dirty = false;
    }
    Ok(id)
}

fn send(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).cloned().ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
    let fail = |s: &mut Session, msg: String| -> CmdResult {
        s.notify(Notice::Error(msg.clone()));
        Ok(json!({"sent": false, "compose": cid, "error": msg}))
    };
    let mut total = 0usize;
    let mut bad = Vec::new();
    for field in [&c.to, &c.cc, &c.bcc] {
        match parse_recipients(field) {
            Ok(v) => total = total.saturating_add(v.iter().map(|a| a.mailboxes().len()).sum::<usize>()),
            Err(b) => bad.extend(b),
        }
    }
    if !bad.is_empty() {
        return fail(s, format!("These recipients are not valid email addresses: {}", bad.join("; ")));
    }
    if total == 0 {
        return fail(s, "There must be at least one name or contact group in the To, Cc, or Bcc box.".into());
    }
    if let Err(b) = parse_recipients(&c.reply_to) {
        return fail(s, format!("The reply address is not valid: {}", b.join("; ")));
    }
    let Some(account) = sending_account(s, &c, true) else {
        return fail(s, NO_ACCOUNT.into());
    };
    if c.encrypt {
        return fail(
            s,
            "This message cannot be encrypted: no certificate is configured for the sending account.".into(),
        );
    }
    let Some(outbox) = s.mailbox.role_folder(account.id, FolderRole::Outbox) else {
        return fail(s, format!("The account {} has no Outbox folder.", account.address));
    };
    let now = s.now();
    let b = build_message(s, &c, Some(&account), false);
    let (mut item, _) = item_from(&b, now);
    item.folder = outbox;
    item.is_draft = false;
    item.read = true;
    item.received = now;
    item.sent = Some(c.delay_until.unwrap_or(now));
    item.bcc = b.bcc.iter().flat_map(Address::mailboxes).collect();
    item.sensitivity = c.sensitivity;
    item.flag = c.flag.clone();
    let id = s.mailbox.add_mail(item);
    // Auto-Complete list (most recently used first).
    s.mailbox.record_recipients(&c.recipients(), now);
    if let Some(d) = c.draft {
        s.mailbox.mail.remove(&d);
    }
    if let Some(src) = c.source.and_then(|sid| s.mailbox.mail.get_mut(&sid)) {
        match c.kind {
            ComposeKind::Reply => src.response = ResponseState::Replied(now),
            ComposeKind::ReplyAll => src.response = ResponseState::RepliedAll(now),
            ComposeKind::Forward => src.response = ResponseState::Forwarded(now),
            _ => {}
        }
    }
    s.composes.remove(&cid);
    // "Send immediately when connected": the account's worker submits the Outbox now (deferred messages
    // wait); otherwise the next send/receive does.
    if c.delay_until.is_none_or(|d| d.unix <= now.unix) {
        crate::sync::send_now(s, account.id);
    }
    Ok(
        json!({"sent": true, "item": id.0, "outbox": outbox.0, "deferredUntil": c.delay_until.map(|d| d.to_rfc3339())}),
    )
}

fn save_draft_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let id = save_draft(s, cid)?;
    Ok(json!({"draft": id.0, "compose": cid}))
}

fn discard(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let c = s.composes.remove(&cid);
    let draft = c.and_then(|c| c.draft);
    if let Some(d) = draft {
        s.mailbox.mail.remove(&d);
    }
    Ok(json!({"discarded": cid, "draftRemoved": draft.map(|d| d.0)}))
}

fn close(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let dirty = s.composes.get(&cid).is_some_and(|c| c.dirty);
    match opt_bool(p, "save") {
        None if dirty => dialog(s, "saveChanges", json!({"compose": cid})),
        Some(true) if dirty => {
            let id = save_draft(s, cid)?;
            s.composes.remove(&cid);
            Ok(json!({"closed": cid, "draft": id.0}))
        }
        _ => {
            s.composes.remove(&cid);
            Ok(json!({"closed": cid}))
        }
    }
}

fn pop_out(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    if let Some(c) = s.composes.get_mut(&cid) {
        c.inline = false;
    }
    s.notify(Notice::Compose(cid));
    Ok(json!({"compose": cid, "inline": false}))
}

// =======================================================================================================
// Header fields
// =======================================================================================================

fn field_value(p: &Value) -> Result<String, CmdError> {
    str_param(p, &["value", "text"])
        .map(str::to_owned)
        .ok_or_else(|| CmdError::Params("`value` (string) required".into()))
}

fn set_to(s: &mut Session, p: &Value) -> CmdResult {
    let v = field_value(p)?;
    let c = compose_mut(s, p)?;
    c.to = v;
    c.dirty = true;
    Ok(json!({"to": c.to}))
}

fn set_cc(s: &mut Session, p: &Value) -> CmdResult {
    let v = field_value(p)?;
    let c = compose_mut(s, p)?;
    c.cc = v;
    c.dirty = true;
    Ok(json!({"cc": c.cc}))
}

fn set_bcc(s: &mut Session, p: &Value) -> CmdResult {
    let v = field_value(p)?;
    let c = compose_mut(s, p)?;
    c.bcc = v;
    c.show_bcc = true;
    c.dirty = true;
    Ok(json!({"bcc": c.bcc}))
}

fn set_subject(s: &mut Session, p: &Value) -> CmdResult {
    let v: String = field_value(p)?.chars().filter(|c| !c.is_control()).take(4096).collect();
    let c = compose_mut(s, p)?;
    c.subject = v;
    c.dirty = true;
    Ok(json!({"subject": c.subject, "title": c.title()}))
}

fn set_from(s: &mut Session, p: &Value) -> CmdResult {
    let v = field_value(p)?;
    // An account id or address picks that account's sender line.
    let account = v
        .trim()
        .parse::<u32>()
        .ok()
        .and_then(|n| s.mailbox.account(mailcraft_model::AccountId(n)).cloned())
        .or_else(|| {
            let addr = parse_field_mailbox(&v).map(|m| m.addr).unwrap_or_default();
            s.mailbox
                .accounts
                .iter()
                .find(|a| !addr.is_empty() && a.address.eq_ignore_ascii_case(&addr))
                .cloned()
        });
    let line = match account {
        Some(a) if !a.address.trim().is_empty() => {
            let name =
                if a.sender_name.trim().is_empty() { a.display_name.trim() } else { a.sender_name.trim() };
            field_mailbox(&mailcraft_mime::Mailbox::new(Some(name), a.address.trim()))
        }
        _ => v,
    };
    let c = compose_mut(s, p)?;
    c.from = line;
    c.show_from = true;
    c.dirty = true;
    Ok(json!({"from": c.from}))
}

fn show_bcc(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.show_bcc = opt_bool(p, "on").unwrap_or(!c.show_bcc);
    Ok(json!({"showBcc": c.show_bcc}))
}

fn show_from(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.show_from = opt_bool(p, "on").unwrap_or(!c.show_from);
    Ok(json!({"showFrom": c.show_from}))
}

fn check_names(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid).cloned() else {
        return Err(CmdError::Disabled(NO_COMPOSE.into()));
    };
    let mut unresolved = Vec::new();
    let mut resolved = 0usize;
    let mut fields = [c.to.clone(), c.cc.clone(), c.bcc.clone()];
    for field in &mut fields {
        let mut parts = Vec::new();
        for piece in split_recipients(field) {
            if parse_recipients(piece).is_ok() {
                resolved = resolved.saturating_add(1);
                parts.push(piece.to_owned());
                continue;
            }
            let matches = lookup_contacts(s, piece);
            match matches.as_slice() {
                [one] => {
                    resolved = resolved.saturating_add(1);
                    parts.push(one.clone());
                }
                _ => {
                    unresolved.push(json!({"name": piece, "candidates": matches}));
                    parts.push(piece.to_owned());
                }
            }
        }
        *field = parts.join("; ");
    }
    if let Some(cm) = s.composes.get_mut(&cid) {
        let [to, cc, bcc] = fields;
        if cm.to != to || cm.cc != cc || cm.bcc != bcc {
            cm.dirty = true;
        }
        cm.to = to;
        cm.cc = cc;
        cm.bcc = bcc;
    }
    if !unresolved.is_empty() {
        s.notify(Notice::OpenDialog("checkNames".into(), json!({"compose": cid, "unresolved": unresolved})));
    }
    Ok(json!({"resolved": resolved, "unresolved": unresolved}))
}

/// Check Names candidates ("Name <address>") for a typed recipient: contacts whose name, file-as, company or
/// any address matches (case-insensitive; exact matches win over partial ones; a contact with several
/// addresses gives one candidate per address, so it stays ambiguous), then Auto-Complete entries.
fn lookup_contacts(s: &Session, query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut exact: Vec<(String, String)> = Vec::new();
    let mut partial: Vec<(String, String)> = Vec::new();
    let push = |v: &mut Vec<(String, String)>, name: &str, addr: &str| {
        if !v.iter().any(|(_, a)| a.eq_ignore_ascii_case(addr)) {
            let line = field_mailbox(&mailcraft_mime::Mailbox::new((!name.is_empty()).then_some(name), addr));
            v.push((line, addr.to_owned()));
        }
    };
    for c in s.mailbox.contacts.values() {
        let emails: Vec<&str> = c.emails.iter().map(|e| e.trim()).filter(|e| !e.is_empty()).collect();
        if emails.is_empty() {
            continue;
        }
        let full = c.full_name();
        let shown = if full.is_empty() { c.file_as.trim().to_owned() } else { full.clone() };
        if let Some(e) = emails.iter().find(|e| e.to_lowercase() == q) {
            push(&mut exact, &shown, e);
            continue;
        }
        let names = [
            full.to_lowercase(),
            c.file_as.trim().to_lowercase(),
            c.first_name.trim().to_lowercase(),
            c.last_name.trim().to_lowercase(),
            c.company.trim().to_lowercase(),
        ];
        let target = if names.iter().any(|n| !n.is_empty() && *n == q) {
            Some(&mut exact)
        } else if names.iter().any(|n| !n.is_empty() && n.contains(&q))
            || emails.iter().any(|e| e.to_lowercase().starts_with(&q))
        {
            Some(&mut partial)
        } else {
            None
        };
        if let Some(v) = target {
            for e in &emails {
                push(v, &shown, e);
            }
        }
    }
    for r in &s.mailbox.recent_recipients {
        let name = r.name.trim().to_lowercase();
        let addr = r.address.trim().to_lowercase();
        if addr.is_empty() {
            continue;
        }
        if name == q || addr == q {
            push(&mut exact, r.name.trim(), r.address.trim());
        } else if (!name.is_empty() && name.contains(&q)) || addr.starts_with(&q) {
            push(&mut partial, r.name.trim(), r.address.trim());
        }
    }
    let pick = if exact.is_empty() { partial } else { exact };
    pick.into_iter().map(|(line, _)| line).collect()
}

// =======================================================================================================
// Attachments and include
// =======================================================================================================

fn attach_file(s: &mut Session, p: &Value) -> CmdResult {
    let Some((name, data)) = file_param(p, MAX_ATTACHMENT_BYTES)? else {
        return dialog(s, "attachFile", json!({"compose": compose_id(s, p)?}));
    };
    let mime = opt_str(p, "mime_type").map(str::to_owned);
    let c = compose_mut(s, p)?;
    if c.attachments.len() >= MAX_ATTACHMENTS {
        return Err(CmdError::Failed("too many attachments".into()));
    }
    let size = data.len();
    let i = c.attach_bytes(&name, mime.as_deref(), data).map_err(CmdError::Failed)?;
    let (name, mime) =
        c.attachments.get(i).map(|a| (a.filename.clone(), a.mime_type.clone())).unwrap_or_default();
    Ok(json!({"attached": name, "mimeType": mime, "index": i, "size": size, "count": c.attachments.len()}))
}

/// Parses `itemId` (number or numeric string).
fn item_id_param(p: &Value, key: &str) -> Option<ItemId> {
    let v = p.get(key)?;
    v.as_u64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())).map(ItemId)
}

fn attach_item(s: &mut Session, p: &Value) -> CmdResult {
    let Some(id) = item_id_param(p, "itemId") else {
        return dialog(s, "attachItem", json!({"compose": compose_id(s, p)?}));
    };
    let now = s.now();
    let att = if let Some(m) = s.mailbox.mail.get(&id) {
        ComposeAttachment {
            filename: safe_name(&m.subject, "message", "eml"),
            mime_type: "message/rfc822".into(),
            data: message_source(m),
        }
    } else if let Some(c) = s.mailbox.contacts.get(&id) {
        let name = if c.full_name().is_empty() { c.file_as.clone() } else { c.full_name() };
        ComposeAttachment {
            filename: safe_name(&name, "contact", "vcf"),
            mime_type: "text/vcard".into(),
            data: vcard(c).into_bytes(),
        }
    } else if let Some(a) = s.mailbox.appointments.get(&id) {
        ComposeAttachment {
            filename: safe_name(&a.subject, "appointment", "ics"),
            mime_type: "text/calendar".into(),
            data: ics_event(a, now).into_bytes(),
        }
    } else if let Some(t) = s.mailbox.tasks.get(&id) {
        ComposeAttachment {
            filename: safe_name(&t.subject, "task", "ics"),
            mime_type: "text/calendar".into(),
            data: ics_task(t, now).into_bytes(),
        }
    } else if let Some(n) = s.mailbox.notes.get(&id) {
        let title = n.text.lines().next().unwrap_or_default();
        ComposeAttachment {
            filename: safe_name(title, "note", "txt"),
            mime_type: "text/plain".into(),
            data: n.text.clone().into_bytes(),
        }
    } else {
        return Err(CmdError::Params(format!("no item with id {}", id.0)));
    };
    let c = compose_mut(s, p)?;
    if c.attachments.len() >= MAX_ATTACHMENTS {
        return Err(CmdError::Failed("too many attachments".into()));
    }
    let name = att.filename.clone();
    c.attachments.push(att);
    c.dirty = true;
    Ok(json!({"attached": name, "count": c.attachments.len()}))
}

/// The attachment a command names: `{"index": n}` or `{"name": "file.txt"}`.
fn attachment_index(c: &ComposeState, p: &Value) -> Result<usize, CmdError> {
    let idx = match (opt_u64(p, "index"), opt_str(p, "name")) {
        (Some(i), _) => usize::try_from(i).ok().filter(|i| *i < c.attachments.len()),
        (None, Some(n)) => c.attachments.iter().position(|a| a.filename == n),
        (None, None) => return Err(CmdError::Params("`index` (number) or `name` (string) required".into())),
    };
    idx.ok_or_else(|| CmdError::Params("no such attachment".into()))
}

fn remove_attachment(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    let i = attachment_index(c, p)?;
    let removed = c.remove_attachment(i).ok_or_else(|| CmdError::Params("no such attachment".into()))?;
    Ok(json!({"removed": removed.filename, "count": c.attachments.len()}))
}

fn rename_attachment(s: &mut Session, p: &Value) -> CmdResult {
    let new_name = req_str(p, "newName")?.to_owned();
    let c = compose_mut(s, p)?;
    let i = attachment_index(c, p)?;
    let name = c.rename_attachment(i, &new_name).map_err(CmdError::Params)?;
    let mime = c.attachments.get(i).map(|a| a.mime_type.clone()).unwrap_or_default();
    Ok(json!({"index": i, "name": name, "mimeType": mime}))
}

fn signature(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.trim().to_owned();
    let html = opt_str(p, "html");
    let text = opt_str(p, "text");
    let format = {
        let cid = compose_id(s, p)?;
        s.composes.get(&cid).map(|c| c.format).unwrap_or_default()
    };
    // A saved signature (File › Options › Mail › Signatures) by name.
    let saved = s.mailbox.signature(&name).map(|sig| crate::compose::signature_blocks(&sig.body, format));
    let name = s.mailbox.signature(&name).map_or(name, |sig| sig.name.clone());
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let new_blocks: Option<Vec<Block>> = match (html, text) {
        (Some(h), _) => Some(mailcraft_html::html_to_doc(h).blocks),
        (None, Some(t)) => Some(Doc::from_plain_text(t).blocks),
        (None, None) if name.is_empty() || name.eq_ignore_ascii_case("none") || name == "(none)" => None,
        (None, None) => match saved {
            Some(blocks) => Some(blocks),
            None => return Err(CmdError::Params(format!("no signature named \"{name}\""))),
        },
    };
    // Find the current signature block: a container holding exactly the blocks inserted last time, else (the
    // user edited the signature in the message) the first container still holding one of its non-empty
    // paragraphs.
    let existing = c.signature.as_ref().and_then(|sig| {
        let exact = c
            .body
            .blocks
            .iter()
            .position(|b| matches!(b, Block::Container { blocks, .. } if *blocks == sig.blocks));
        exact.or_else(|| {
            let kept = |b: &Block| {
                !Doc { blocks: vec![b.clone()], ..Default::default() }.to_plain_text().trim().is_empty()
                    && sig.blocks.contains(b)
            };
            c.body
                .blocks
                .iter()
                .position(|b| matches!(b, Block::Container { blocks, .. } if blocks.iter().any(kept)))
        })
    });
    match (existing, new_blocks) {
        (Some(i), Some(blocks)) => {
            if let Some(b) = c.body.blocks.get_mut(i) {
                *b = Block::Container { style: Default::default(), blocks: blocks.clone() };
            }
            c.signature = Some(SignatureBlock { name: name.clone(), blocks });
        }
        (Some(i), None) => {
            if i < c.body.blocks.len() {
                c.body.blocks.remove(i);
            }
            c.signature = None;
        }
        (None, Some(blocks)) => {
            c.insert_blocks(vec![Block::Container { style: Default::default(), blocks: blocks.clone() }]);
            c.signature = Some(SignatureBlock { name: name.clone(), blocks });
        }
        (None, None) => {}
    }
    c.ensure_body();
    c.dirty = true;
    Ok(json!({"signature": c.signature.as_ref().map(|s| s.name.clone())}))
}

// =======================================================================================================
// Tags, tracking, delivery
// =======================================================================================================

fn importance(s: &mut Session, p: &Value) -> CmdResult {
    let level = match req_str(p, "level")? {
        "high" => Importance::High,
        "low" => Importance::Low,
        "normal" => Importance::Normal,
        other => return Err(CmdError::Params(format!("unknown importance `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    c.importance = if c.importance == level { Importance::Normal } else { level };
    c.dirty = true;
    let name = match c.importance {
        Importance::High => "high",
        Importance::Low => "low",
        Importance::Normal => "normal",
    };
    Ok(json!({"importance": name}))
}

fn sensitivity(s: &mut Session, p: &Value) -> CmdResult {
    let level = match req_str(p, "level")? {
        "normal" => Sensitivity::Normal,
        "personal" => Sensitivity::Personal,
        "private" => Sensitivity::Private,
        "confidential" => Sensitivity::Confidential,
        other => return Err(CmdError::Params(format!("unknown sensitivity `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    c.sensitivity = level;
    c.dirty = true;
    Ok(json!({"sensitivity": format!("{level:?}").to_lowercase()}))
}

fn follow_up(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    let label = opt_str(p, "label").unwrap_or("Follow up").to_owned();
    // Same dates as the message list's Follow Up menu (weeks start on Sunday, work week Monday–Friday).
    let (start, due) = match kind {
        "today" => flag_dates(s, FlagKind::Today),
        "tomorrow" => flag_dates(s, FlagKind::Tomorrow),
        "thisWeek" => flag_dates(s, FlagKind::ThisWeek),
        "nextWeek" => flag_dates(s, FlagKind::NextWeek),
        "noDate" => (None, None),
        "clear" => {
            let c = compose_mut(s, p)?;
            c.flag = FlagState::None;
            c.dirty = true;
            return Ok(json!({"flag": "none"}));
        }
        "custom" | "reminder" => {
            let at = opt_str(p, "at").or_else(|| opt_str(p, "due"));
            let Some(at) = at else {
                let name = if kind == "custom" { "customFlag" } else { "addReminder" };
                return dialog(s, name, json!({"compose": compose_id(s, p)?}));
            };
            let when =
                parse_when(s, at).ok_or_else(|| CmdError::Params(format!("cannot read the date `{at}`")))?;
            let c = compose_mut(s, p)?;
            c.flag = match (&c.flag, kind) {
                (FlagState::Flagged { label: l, start, due, .. }, "reminder") => {
                    FlagState::Flagged { label: l.clone(), start: *start, due: *due, reminder: Some(when) }
                }
                (_, "reminder") => FlagState::Flagged { label, start: None, due: None, reminder: Some(when) },
                _ => FlagState::Flagged { label, start: Some(when), due: Some(when), reminder: None },
            };
            c.dirty = true;
            return Ok(json!({"flag": kind, "at": when.to_rfc3339()}));
        }
        other => return Err(CmdError::Params(format!("unknown follow-up kind `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    c.flag = FlagState::Flagged { label, start, due, reminder: None };
    c.dirty = true;
    Ok(json!({"flag": kind, "start": start.map(|d| d.to_rfc3339()), "due": due.map(|d| d.to_rfc3339())}))
}

fn read_receipt(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.request_read_receipt = opt_bool(p, "on").unwrap_or(!c.request_read_receipt);
    c.dirty = true;
    Ok(json!({"readReceipt": c.request_read_receipt}))
}

fn delivery_receipt(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.request_delivery_receipt = opt_bool(p, "on").unwrap_or(!c.request_delivery_receipt);
    c.dirty = true;
    Ok(json!({"deliveryReceipt": c.request_delivery_receipt}))
}

fn voting_buttons(s: &mut Session, p: &Value) -> CmdResult {
    let choices = req_str(p, "choices")?.trim().to_owned();
    if choices.eq_ignore_ascii_case("custom") {
        return dialog(s, "messageOptions", json!({"compose": compose_id(s, p)?, "focus": "voting"}));
    }
    let norm: Vec<&str> = choices.split(';').map(str::trim).filter(|x| !x.is_empty()).collect();
    let norm = norm.join(";");
    let c = compose_mut(s, p)?;
    c.voting = if c.voting == norm { String::new() } else { norm };
    c.dirty = true;
    Ok(json!({"voting": c.voting}))
}

fn delay_delivery(s: &mut Session, p: &Value) -> CmdResult {
    match p.get("at") {
        None => dialog(s, "messageOptions", json!({"compose": compose_id(s, p)?, "focus": "delivery"})),
        Some(Value::Null) => {
            let c = compose_mut(s, p)?;
            c.delay_until = None;
            c.dirty = true;
            Ok(json!({"delayUntil": null}))
        }
        Some(v) => {
            let text =
                v.as_str().ok_or_else(|| CmdError::Params("`at` must be a date/time string".into()))?;
            let when = if text.trim().is_empty() {
                None
            } else {
                Some(
                    parse_when(s, text)
                        .ok_or_else(|| CmdError::Params(format!("cannot read the date `{text}`")))?,
                )
            };
            let c = compose_mut(s, p)?;
            c.delay_until = when;
            c.dirty = true;
            Ok(json!({"delayUntil": when.map(|w| w.to_rfc3339())}))
        }
    }
}

fn direct_replies_to(s: &mut Session, p: &Value) -> CmdResult {
    let Some(addresses) = opt_str(p, "addresses") else {
        return dialog(s, "messageOptions", json!({"compose": compose_id(s, p)?, "focus": "replies"}));
    };
    if let Err(bad) = parse_recipients(addresses) {
        return Err(CmdError::Params(format!("not valid email addresses: {}", bad.join("; "))));
    }
    let c = compose_mut(s, p)?;
    c.reply_to = addresses.trim().to_owned();
    c.dirty = true;
    Ok(json!({"replyTo": c.reply_to}))
}

fn save_sent_item_to(s: &mut Session, p: &Value) -> CmdResult {
    let mode = req_str(p, "mode")?;
    let target = match mode {
        "default" => SaveSentTo::Default,
        "none" => SaveSentTo::DoNotSave,
        "other" => {
            let Some(f) = p.get("folder").filter(|v| !v.is_null()) else {
                return dialog(
                    s,
                    "selectFolder",
                    json!({"compose": compose_id(s, p)?, "purpose": "saveSentItemTo"}),
                );
            };
            let id = f
                .as_u64()
                .or_else(|| f.as_str().and_then(|t| t.trim().parse().ok()))
                .map(mailcraft_model::FolderId)
                .ok_or_else(|| CmdError::Params("`folder` must be a folder id".into()))?;
            match s.mailbox.folder(id) {
                Some(f) if f.kind == FolderKind::Mail => SaveSentTo::Folder(id),
                Some(_) => {
                    return Err(CmdError::Params("sent items can only be saved to a mail folder".into()));
                }
                None => return Err(CmdError::Params(format!("no folder with id {}", id.0))),
            }
        }
        other => return Err(CmdError::Params(format!("unknown mode `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    c.save_sent = target;
    c.dirty = true;
    let folder = match target {
        SaveSentTo::Folder(f) => Some(f.0),
        _ => None,
    };
    Ok(json!({"mode": mode, "folder": folder}))
}

fn encrypt(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(m) = opt_str(p, "mode")
        && m != "smime"
    {
        return Err(CmdError::Params(format!("unknown encryption mode `{m}`")));
    }
    let c = compose_mut(s, p)?;
    c.encrypt = opt_bool(p, "on").unwrap_or(!c.encrypt);
    c.dirty = true;
    Ok(json!({"encrypt": c.encrypt}))
}

fn message_options(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled(NO_COMPOSE.into()));
    };
    let state = json!({
        "compose": cid,
        "importance": format!("{:?}", c.importance).to_lowercase(),
        "sensitivity": format!("{:?}", c.sensitivity).to_lowercase(),
        "readReceipt": c.request_read_receipt,
        "deliveryReceipt": c.request_delivery_receipt,
        "voting": c.voting,
        "replyTo": c.reply_to,
        "delayUntil": c.delay_until.map(|d| d.to_rfc3339()),
        "encrypt": c.encrypt,
    });
    dialog(s, "messageOptions", state)
}

// =======================================================================================================
// Format, themes, background
// =======================================================================================================

fn format_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    let target = match kind {
        "html" => BodyFormat::Html,
        "plain" => BodyFormat::PlainText,
        "rich" => BodyFormat::RichText,
        other => return Err(CmdError::Params(format!("unknown format `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    let mut converted = false;
    if target == BodyFormat::PlainText && c.format != BodyFormat::PlainText {
        c.checkpoint();
        let text = c.body.to_plain_text();
        c.body = Doc::from_plain_text(&text);
        c.inline_images.clear();
        c.signature = None;
        c.pending_style = None;
        c.extra_ranges.clear();
        c.selection = Default::default();
        c.ensure_body();
        converted = true;
    }
    if c.format != target {
        c.dirty = true;
    }
    c.format = target;
    Ok(json!({"format": kind, "converted": converted, "title": c.title()}))
}

/// Theme fonts by theme/font-set name: (body, headings).
fn theme_fonts(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "Office" => Some(("Aptos", "Aptos Display")),
        "Office 2013 - 2022" | "Office 2013 - 2022 Theme" => Some(("Calibri", "Calibri Light")),
        "Office 2007 - 2010" => Some(("Calibri", "Cambria")),
        _ => None,
    }
}

fn theme(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("apply");
    let part = opt_str(p, "part").unwrap_or("all");
    if !matches!(part, "all" | "colors" | "fonts" | "effects") {
        return Err(CmdError::Params(format!("unknown theme part `{part}`")));
    }
    match action {
        "browse" => return dialog(s, "browseThemes", json!({"compose": compose_id(s, p)?})),
        "save" => return dialog(s, "saveTheme", json!({"compose": compose_id(s, p)?})),
        "customize" => {
            return dialog(s, "customizeTheme", json!({"compose": compose_id(s, p)?, "part": part}));
        }
        "apply" | "reset" => {}
        other => return Err(CmdError::Params(format!("unknown theme action `{other}`"))),
    }
    let name = opt_str(p, "name").map(str::to_owned);
    if action == "apply" && name.is_none() {
        return dialog(s, "themes", json!({"compose": compose_id(s, p)?, "part": part}));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if action == "reset" {
        c.theme = Default::default();
        c.body.base.font_family = None;
    } else {
        let set = |slot: &mut Option<String>| *slot = name.clone();
        match part {
            "all" => {
                set(&mut c.theme.name);
                set(&mut c.theme.colors);
                set(&mut c.theme.fonts);
                set(&mut c.theme.effects);
            }
            "colors" => set(&mut c.theme.colors),
            "fonts" => set(&mut c.theme.fonts),
            _ => set(&mut c.theme.effects),
        }
        if matches!(part, "all" | "fonts")
            && let Some((body, _)) = name.as_deref().and_then(theme_fonts)
        {
            c.body.base.font_family = Some(body.to_owned());
        }
    }
    c.dirty = true;
    Ok(
        json!({"theme": c.theme.name, "colors": c.theme.colors, "fonts": c.theme.fonts, "effects": c.theme.effects}),
    )
}

fn page_color(s: &mut Session, p: &Value) -> CmdResult {
    let color = match p.get("color") {
        None => return dialog(s, "pageColor", json!({"compose": compose_id(s, p)?})),
        Some(Value::Null) => None,
        Some(v) => {
            let t = v
                .as_str()
                .ok_or_else(|| CmdError::Params("`color` must be a colour string or null".into()))?;
            if t.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(
                    mailcraft_html::parse_css_color(t)
                        .ok_or_else(|| CmdError::Params(format!("unknown colour `{t}`")))?,
                )
            }
        }
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.body.background = color;
    c.dirty = true;
    Ok(json!({"pageColor": color.map(|c| c.to_hex())}))
}

fn switch_background(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.light_canvas = !c.light_canvas;
    Ok(json!({"lightCanvas": c.light_canvas}))
}

fn address_book(s: &mut Session, p: &Value) -> CmdResult {
    let compose = compose_id(s, p).ok();
    let field = opt_str(p, "field").unwrap_or("to");
    dialog(s, "addressBook", json!({"focus": "search", "compose": compose, "field": field}))
}

// =======================================================================================================
// Body editing primitives
// =======================================================================================================

fn select(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    if opt_bool(p, "all") == Some(true) {
        c.select_all();
    } else {
        let anchor = match p.get("anchor") {
            Some(v) => pos_param(v)?,
            None => return Err(CmdError::Params("`anchor` ({path, offset}) or `all` required".into())),
        };
        let focus = match p.get("focus") {
            Some(v) => pos_param(v)?,
            None => anchor.clone(),
        };
        c.set_selection(anchor, focus);
    }
    Ok(
        json!({"anchor": pos_json(&c.selection.anchor), "focus": pos_json(&c.selection.focus), "text": c.selected_text()}),
    )
}

fn insert_text(s: &mut Session, p: &Value) -> CmdResult {
    let text = req_str(p, "text")?.to_owned();
    let c = compose_mut(s, p)?;
    // Typing is one undo step until the caret moves or another edit runs; Enter starts a new one.
    let run = (!text.contains(['\n', '\r'])).then_some(EditRun::Typing);
    c.run_edit(run, |c| c.insert_text(&text));
    Ok(json!({"caret": pos_json(&c.selection.focus)}))
}

fn delete_text(s: &mut Session, p: &Value) -> CmdResult {
    let dir = opt_str(p, "direction").unwrap_or("backward");
    let c = compose_mut(s, p)?;
    match dir {
        "backward" => c.run_edit(Some(EditRun::DeleteBackward), ComposeState::delete_backward),
        "forward" => c.run_edit(Some(EditRun::DeleteForward), ComposeState::delete_forward),
        "selection" => c.run_edit(None, |c| {
            c.delete_selection();
        }),
        other => return Err(CmdError::Params(format!("unknown direction `{other}`"))),
    }
    Ok(json!({"caret": pos_json(&c.selection.focus)}))
}

fn edit_undo(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    Ok(json!({"undone": c.undo_edit()}))
}

fn edit_redo(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    Ok(json!({"redone": c.redo_edit()}))
}

/// Keeps bookmarks typed into the compose (used by insert.bookmark / format.goTo).
pub(crate) fn add_bookmark(c: &mut ComposeState, name: &str) {
    let pos = c.selection.focus.clone();
    c.bookmarks.retain(|b| b.name != name);
    c.bookmarks.push(Bookmark { name: name.to_owned(), pos });
}

// =======================================================================================================
// Signatures (File › Options › Mail › Signatures), Auto-Complete list, tables
// =======================================================================================================

/// Largest signature body accepted (bytes of HTML or text).
const MAX_SIGNATURE_SOURCE: usize = 1024 * 1024;

/// Records mailbox changes the session's store diff does not cover (signatures, Auto-Complete list,
/// dictionary) as one undo step plus a journal write; does nothing when the diff already sees them.
pub(crate) fn commit_extras(s: &mut Session, before: mailcraft_model::Mailbox, label: &str) {
    if before == s.mailbox || !crate::session::diff_ops(&before, &s.mailbox).is_empty() {
        return;
    }
    s.undo.push(crate::undo::UndoEntry { label: label.to_owned(), before: Box::new(before) });
    s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
}

/// A rich body from `{"html"}` or `{"text"}`.
fn doc_param(p: &Value) -> Result<Option<Doc>, CmdError> {
    let (src, html) = match (opt_str(p, "html"), opt_str(p, "text")) {
        (Some(h), _) => (h, true),
        (None, Some(t)) => (t, false),
        (None, None) => return Ok(None),
    };
    if src.len() > MAX_SIGNATURE_SOURCE {
        return Err(CmdError::Params("the signature is too large".into()));
    }
    Ok(Some(if html { mailcraft_html::html_to_doc(src) } else { Doc::from_plain_text(src) }))
}

/// `{"key": "name"}` → Some(Some(name)), `{"key": null}` → Some(None), absent → None.
fn choice_param<'a>(p: &'a Value, key: &str) -> Result<Option<Option<&'a str>>, CmdError> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(t)) => Ok(Some(Some(t.as_str()))),
        Some(_) => Err(CmdError::Params(format!("`{key}` must be a signature name or null"))),
    }
}

fn signature_json(s: &Session) -> Value {
    let sigs: Vec<Value> = s
        .mailbox
        .signatures
        .iter()
        .map(|g| {
            json!({
                "name": g.name,
                "defaultNew": g.default_new,
                "defaultReply": g.default_reply,
                "text": g.body.to_plain_text(),
                "html": mailcraft_html::doc_to_html(&g.body),
            })
        })
        .collect();
    let accounts: Vec<Value> = s
        .mailbox
        .accounts
        .iter()
        .map(|a| {
            json!({
                "account": a.id.0,
                "address": a.address,
                "newMessages": s.mailbox.default_signature(Some(a.id), false).map(|g| g.name.clone()),
                "replies": s.mailbox.default_signature(Some(a.id), true).map(|g| g.name.clone()),
            })
        })
        .collect();
    json!({"signatures": sigs, "accounts": accounts})
}

fn signature_list(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(signature_json(s))
}

fn signature_create(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.to_owned();
    let body = doc_param(p)?.unwrap_or_default();
    let before = s.mailbox.clone();
    let name = s.mailbox.add_signature(&name, body)?;
    let new_default = opt_bool(p, "defaultNew") == Some(true);
    let reply_default = opt_bool(p, "defaultReply") == Some(true);
    if new_default || reply_default {
        s.mailbox.set_signature_defaults(
            None,
            new_default.then_some(Some(name.as_str())),
            reply_default.then_some(Some(name.as_str())),
        )?;
    }
    commit_extras(s, before, "New Signature");
    Ok(json!({"created": name, "count": s.mailbox.signatures.len()}))
}

fn signature_edit(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.to_owned();
    let new_name = opt_str(p, "newName").map(str::to_owned);
    let body = doc_param(p)?;
    let before = s.mailbox.clone();
    let final_name = s.mailbox.edit_signature(&name, new_name.as_deref(), body)?;
    commit_extras(s, before, "Edit Signature");
    Ok(json!({"name": final_name}))
}

fn signature_delete(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.to_owned();
    let before = s.mailbox.clone();
    let removed = s.mailbox.delete_signature(&name)?;
    commit_extras(s, before, "Delete Signature");
    Ok(json!({"deleted": removed.name, "count": s.mailbox.signatures.len()}))
}

fn signature_defaults(s: &mut Session, p: &Value) -> CmdResult {
    let account = match p.get("account") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| CmdError::Params("`account` must be an account id".into()))?;
            Some(mailcraft_model::AccountId(n))
        }
    };
    let new_messages = choice_param(p, "newMessages")?;
    let replies = choice_param(p, "replies")?;
    if new_messages.is_none() && replies.is_none() {
        return Err(CmdError::Params("`newMessages` and/or `replies` (name or null) required".into()));
    }
    let before = s.mailbox.clone();
    s.mailbox.set_signature_defaults(account, new_messages, replies)?;
    commit_extras(s, before, "Default Signature");
    Ok(json!({
        "account": account.map(|a| a.0),
        "newMessages": s.mailbox.default_signature(account, false).map(|g| g.name.clone()),
        "replies": s.mailbox.default_signature(account, true).map(|g| g.name.clone()),
    }))
}

fn suggest_recipients(s: &mut Session, p: &Value) -> CmdResult {
    let prefix = opt_str(p, "prefix").or_else(|| opt_str(p, "text")).unwrap_or("");
    let limit = opt_u64(p, "limit").and_then(|n| usize::try_from(n).ok()).unwrap_or(10).min(100);
    let list = crate::queries::address_suggestions(s, prefix, limit);
    let rows: Vec<Value> = list
        .iter()
        .map(|g| json!({"display": g.display, "address": g.address, "source": g.source.label(), "entry": g.entry()}))
        .collect();
    Ok(json!({"suggestions": rows}))
}

fn forget_recipient(s: &mut Session, p: &Value) -> CmdResult {
    let addr = req_str(p, "address")?.to_owned();
    let before = s.mailbox.clone();
    let removed = s.mailbox.forget_recipient(&addr);
    commit_extras(s, before, "Remove From Auto-Complete List");
    Ok(json!({"removed": removed}))
}

fn empty_auto_complete(s: &mut Session, _p: &Value) -> CmdResult {
    let before = s.mailbox.clone();
    let n = s.mailbox.recent_recipients.len();
    s.mailbox.recent_recipients.clear();
    commit_extras(s, before, "Empty Auto-Complete List");
    Ok(json!({"removed": n}))
}

fn table_cell(s: &mut Session, p: &Value) -> CmdResult {
    let dir = match opt_str(p, "direction").unwrap_or("next") {
        "next" => crate::compose::CellMove::Next,
        "previous" | "prev" => crate::compose::CellMove::Previous,
        "up" => crate::compose::CellMove::Up,
        "down" => crate::compose::CellMove::Down,
        other => return Err(CmdError::Params(format!("unknown direction `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    let moved = c.move_to_cell(dir);
    let cell = c.cell_at_caret().map(|(t, r, col)| json!({"table": t, "row": r, "col": col}));
    Ok(
        json!({"moved": moved, "cell": cell, "anchor": pos_json(&c.selection.anchor), "focus": pos_json(&c.selection.focus)}),
    )
}

fn table_edit(s: &mut Session, p: &Value) -> CmdResult {
    use crate::compose::TableEdit;
    let action = req_str(p, "action")?;
    let op = match action {
        "insertRowAbove" => TableEdit::InsertRowAbove,
        "insertRowBelow" => TableEdit::InsertRowBelow,
        "insertColumnLeft" => TableEdit::InsertColumnLeft,
        "insertColumnRight" => TableEdit::InsertColumnRight,
        "deleteRow" => TableEdit::DeleteRow,
        "deleteColumn" => TableEdit::DeleteColumn,
        "deleteTable" => TableEdit::DeleteTable,
        other => return Err(CmdError::Params(format!("unknown table action `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    if c.cell_at_caret().is_none() {
        return Err(CmdError::Failed("put the cursor in a table first".into()));
    }
    let changed = c.table_edit(op);
    Ok(json!({"changed": changed, "caret": pos_json(&c.selection.focus)}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "compose.addressBook",
            "Address Book",
            "Home › Find",
            "Mod+Shift+B",
            "",
            false,
            crate::always,
            address_book,
        ),
        spec(
            "compose.attachFile",
            "Attach File",
            "Message › Include; Insert › Include",
            "",
            r#"{"path"?:string} | {"name":string,"data_base64":string,"mime_type"?:string}"#,
            false,
            has_compose,
            attach_file,
        ),
        spec(
            "compose.attachItem",
            "Attach Item",
            "Message › Include › Attach File; Insert › Include",
            "",
            r#"{"itemId"?:string}"#,
            false,
            has_compose,
            attach_item,
        ),
        spec("compose.checkNames", "Check Names", "Message › Names", "", "", false, has_compose, check_names),
        spec(
            "compose.delayDelivery",
            "Delay Delivery",
            "Options › More Options",
            "",
            r#"{"at"?:string|null}"#,
            false,
            has_compose,
            delay_delivery,
        ),
        spec(
            "compose.deliveryReceipt",
            "Request a Delivery Receipt",
            "Options › Tracking",
            "",
            r#"{"on"?:bool}"#,
            false,
            has_compose,
            delivery_receipt,
        ),
        spec(
            "compose.directRepliesTo",
            "Direct Replies To",
            "Options › More Options",
            "",
            r#"{"addresses"?:string}"#,
            false,
            has_compose,
            direct_replies_to,
        ),
        spec(
            "compose.encrypt",
            "Encrypt",
            "Options › Encrypt",
            "",
            r#"{"mode"?:"smime","on"?:bool}"#,
            false,
            has_compose,
            encrypt,
        ),
        spec(
            "compose.followUp",
            "Follow Up",
            "Message › Tags",
            "",
            r#"{"kind":"today"|"tomorrow"|"thisWeek"|"nextWeek"|"noDate"|"custom"|"reminder"|"clear","at"?:string}"#,
            false,
            has_compose,
            follow_up,
        ),
        spec(
            "compose.format",
            "Message Format",
            "Format Text › Format",
            "",
            r#"{"kind":"html"|"plain"|"rich"}"#,
            false,
            has_compose,
            format_cmd,
        ),
        spec(
            "compose.importance",
            "Importance",
            "Message › Tags",
            "",
            r#"{"level":"high"|"normal"|"low"}"#,
            false,
            has_compose,
            importance,
        ),
        spec(
            "compose.messageOptions",
            "Message Options",
            "Message › Tags; Options › Tracking",
            "",
            "",
            false,
            has_compose,
            message_options,
        ),
        spec(
            "compose.pageColor",
            "Page Color",
            "Options › Themes",
            "",
            r#"{"color"?:string|null}"#,
            false,
            rich_compose,
            page_color,
        ),
        spec(
            "compose.readReceipt",
            "Request a Read Receipt",
            "Options › Tracking",
            "",
            r#"{"on"?:bool}"#,
            false,
            has_compose,
            read_receipt,
        ),
        spec(
            "compose.saveSentItemTo",
            "Save Sent Item To",
            "Options › More Options",
            "",
            r#"{"folder"?:string|null,"mode":"default"|"other"|"none"}"#,
            false,
            has_compose,
            save_sent_item_to,
        ),
        spec(
            "compose.showBcc",
            "Bcc",
            "Options › Show Fields",
            "",
            r#"{"on"?:bool}"#,
            false,
            has_compose,
            show_bcc,
        ),
        spec(
            "compose.showFrom",
            "From",
            "Options › Show Fields",
            "",
            r#"{"on"?:bool}"#,
            false,
            has_compose,
            show_from,
        ),
        spec(
            "compose.signature",
            "Signature",
            "Message › Include; Insert › Include",
            "",
            r#"{"name":string,"text"?:string,"html"?:string}  (name "none" removes)"#,
            false,
            has_compose,
            signature,
        ),
        spec(
            "compose.switchBackground",
            "Switch Background",
            "Message › Dark Mode",
            "",
            "",
            false,
            dark_compose,
            switch_background,
        ),
        spec(
            "compose.theme",
            "Themes",
            "Options › Themes; Format Text › Styles › Change Styles",
            "",
            r#"{"name"?:string,"part"?:"all"|"colors"|"fonts"|"effects","action"?:"apply"|"reset"|"browse"|"save"|"customize"}"#,
            false,
            rich_compose,
            theme,
        ),
        spec(
            "compose.votingButtons",
            "Use Voting Buttons",
            "Options › Tracking",
            "",
            r#"{"choices":string}"#,
            false,
            has_compose,
            voting_buttons,
        ),
        // Header / window commands (not ribbon rows).
        spec("compose.send", "Send", "Compose › Header", "Mod+Enter / Alt+S", "", true, has_compose, send),
        spec(
            "compose.saveDraft",
            "Save",
            "Compose › Quick Access; File › Save",
            "Mod+S",
            "",
            true,
            has_compose,
            save_draft_cmd,
        ),
        spec("compose.discard", "Discard", "Compose › Inline Response", "", "", true, has_compose, discard),
        spec(
            "compose.close",
            "Close",
            "Compose › Window; File › Close",
            "",
            r#"{"save"?:bool}"#,
            true,
            has_compose,
            close,
        ),
        spec("compose.popOut", "Pop Out", "Compose › Inline Response", "", "", false, has_compose, pop_out),
        spec(
            "compose.setTo",
            "To",
            "Compose › Header",
            "",
            r#"{"value":string}"#,
            false,
            has_compose,
            set_to,
        ),
        spec(
            "compose.setCc",
            "Cc",
            "Compose › Header",
            "",
            r#"{"value":string}"#,
            false,
            has_compose,
            set_cc,
        ),
        spec(
            "compose.setBcc",
            "Bcc",
            "Compose › Header",
            "",
            r#"{"value":string}"#,
            false,
            has_compose,
            set_bcc,
        ),
        spec(
            "compose.setSubject",
            "Subject",
            "Compose › Header",
            "",
            r#"{"value":string}"#,
            false,
            has_compose,
            set_subject,
        ),
        spec(
            "compose.setFrom",
            "From",
            "Compose › Header",
            "",
            r#"{"value":string}  (address, "Name <address>" or account id)"#,
            false,
            has_compose,
            set_from,
        ),
        spec(
            "compose.removeAttachment",
            "Remove Attachment",
            "Compose › Attachments",
            "",
            r#"{"index"?:number,"name"?:string}"#,
            false,
            has_compose,
            remove_attachment,
        ),
        spec(
            "compose.sensitivity",
            "Sensitivity",
            "Compose › Message Options",
            "",
            r#"{"level":"normal"|"personal"|"private"|"confidential"}"#,
            false,
            has_compose,
            sensitivity,
        ),
        spec(
            "compose.select",
            "Select",
            "Compose › Body",
            "",
            r#"{"anchor":{"path":[number],"offset":number},"focus"?:{..}} | {"all":true}"#,
            false,
            has_compose,
            select,
        ),
        spec(
            "compose.insertText",
            "Type Text",
            "Compose › Body",
            "",
            r#"{"text":string}  ("\n" = new paragraph)"#,
            false,
            has_compose,
            insert_text,
        ),
        spec(
            "compose.deleteText",
            "Delete",
            "Compose › Body",
            "",
            r#"{"direction"?:"backward"|"forward"|"selection"}"#,
            false,
            has_compose,
            delete_text,
        ),
        spec("compose.editUndo", "Undo Typing", "Compose › Body", "", "", false, has_compose, edit_undo),
        spec("compose.editRedo", "Redo Typing", "Compose › Body", "", "", false, has_compose, edit_redo),
        spec(
            "compose.renameAttachment",
            "Rename Attachment",
            "Compose › Attachments",
            "",
            r#"{"index"?:number,"name"?:string,"newName":string}"#,
            false,
            has_compose,
            rename_attachment,
        ),
        spec(
            "compose.signatureList",
            "Signatures",
            "File › Options › Mail › Signatures",
            "",
            "",
            false,
            crate::always,
            signature_list,
        ),
        spec(
            "compose.signatureCreate",
            "New Signature",
            "File › Options › Mail › Signatures",
            "",
            r#"{"name":string,"html"?:string,"text"?:string,"defaultNew"?:bool,"defaultReply"?:bool}"#,
            true,
            crate::always,
            signature_create,
        ),
        spec(
            "compose.signatureEdit",
            "Edit Signature",
            "File › Options › Mail › Signatures",
            "",
            r#"{"name":string,"newName"?:string,"html"?:string,"text"?:string}"#,
            true,
            crate::always,
            signature_edit,
        ),
        spec(
            "compose.signatureDelete",
            "Delete Signature",
            "File › Options › Mail › Signatures",
            "",
            r#"{"name":string}"#,
            true,
            crate::always,
            signature_delete,
        ),
        spec(
            "compose.signatureDefaults",
            "Choose Default Signature",
            "File › Options › Mail › Signatures",
            "",
            r#"{"account"?:number,"newMessages"?:string|null,"replies"?:string|null}"#,
            true,
            crate::always,
            signature_defaults,
        ),
        spec(
            "compose.suggestRecipients",
            "Auto-Complete",
            "Compose › Header",
            "",
            r#"{"prefix":string,"limit"?:number}"#,
            false,
            crate::always,
            suggest_recipients,
        ),
        spec(
            "compose.forgetRecipient",
            "Remove From Auto-Complete List",
            "Compose › Header",
            "",
            r#"{"address":string}"#,
            true,
            crate::always,
            forget_recipient,
        ),
        spec(
            "compose.emptyAutoComplete",
            "Empty Auto-Complete List",
            "File › Options › Mail › Send messages",
            "",
            "",
            true,
            crate::always,
            empty_auto_complete,
        ),
        spec(
            "compose.tableCell",
            "Move Between Cells",
            "Compose › Body › Table",
            "",
            r#"{"direction"?:"next"|"previous"|"up"|"down"}"#,
            false,
            has_compose,
            table_cell,
        ),
        spec(
            "compose.tableEdit",
            "Rows & Columns",
            "Table Layout › Rows & Columns; Table Layout › Delete",
            "",
            r#"{"action":"insertRowAbove"|"insertRowBelow"|"insertColumnLeft"|"insertColumnRight"|"deleteRow"|"deleteColumn"|"deleteTable"}"#,
            false,
            has_compose,
            table_edit,
        ),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mailcraft_model::{AccountKind, Mailbox, ServerSettings};

    /// Session with a local data file plus an IMAP account that can send, and a fixed clock
    /// (Fri 2026-10-09 12:00 UTC).
    pub(crate) fn session() -> Session {
        let mut mb = Mailbox::new_local("Local");
        let acc = mb.add_account(Account {
            kind: AccountKind::Imap,
            display_name: "me@example.com".into(),
            address: "me@example.com".into(),
            sender_name: "Me Example".into(),
            outgoing: Some(ServerSettings {
                host: "smtp.example.com".into(),
                port: 465,
                ..Default::default()
            }),
            ..Default::default()
        });
        mb.create_default_folders(acc);
        for a in &mut mb.accounts {
            a.is_default = a.id == acc;
        }
        let mut s = Session::new(mb);
        s.fixed_now = DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0);
        s
    }

    pub(crate) fn open(s: &mut Session) -> u64 {
        s.open_compose(ComposeState::default())
    }

    fn folder_items(s: &Session, role: FolderRole) -> Vec<&MailItem> {
        s.mailbox
            .mail
            .values()
            .filter(|m| s.mailbox.folder(m.folder).is_some_and(|f| f.role == Some(role)))
            .collect()
    }

    #[test]
    fn send_without_account_errors_and_keeps_compose() {
        let mut s = Session::new(Mailbox::new_local("Local"));
        let id = open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.com"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], false);
        assert!(s.composes.contains_key(&id));
        assert!(s.notices.contains(&Notice::Error(NO_ACCOUNT.into())));
    }

    #[test]
    fn send_validates_recipients() {
        let mut s = session();
        open(&mut s);
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], false);
        s.run("compose.setTo", &json!({"value": "bob"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], false);
        assert!(r["error"].as_str().unwrap().contains("bob"));
    }

    #[test]
    fn send_puts_message_in_outbox_and_closes() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.setTo", &json!({"value": "Ann <ann@example.org>; bo@example.net"})).unwrap();
        s.run("compose.setBcc", &json!({"value": "hidden@example.net"})).unwrap();
        s.run("compose.setSubject", &json!({"value": "Hello"})).unwrap();
        s.run("compose.insertText", &json!({"text": "Hi there"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.bold", &json!({})).unwrap();
        s.run("compose.saveDraft", &json!({})).unwrap();
        assert_eq!(folder_items(&s, FolderRole::Drafts).len(), 1);
        s.run("compose.readReceipt", &json!({})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        assert!(!s.composes.contains_key(&id));
        assert!(folder_items(&s, FolderRole::Drafts).is_empty(), "draft removed");
        let out = folder_items(&s, FolderRole::Outbox);
        assert_eq!(out.len(), 1);
        let m = out[0];
        assert!(!m.is_draft && m.read);
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.bcc.len(), 1);
        let raw = String::from_utf8_lossy(m.raw.as_ref().unwrap()).to_string();
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("Disposition-Notification-To"));
        assert!(!raw.to_ascii_lowercase().contains("\nbcc:"), "Bcc not transmitted");
        assert!(m.body.html.as_deref().unwrap_or("").contains("Hi there"));
    }

    #[test]
    fn plain_text_sends_text_only_and_reply_marks_source() {
        let mut s = session();
        let inbox = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        let src = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Q".into(),
            message_id: Some("q@x".into()),
            ..Default::default()
        });
        s.open_compose(ComposeState {
            kind: ComposeKind::Reply,
            source: Some(src),
            to: "q@example.com".into(),
            subject: "RE: Q".into(),
            ..Default::default()
        });
        s.run("compose.insertText", &json!({"text": "ok"})).unwrap();
        s.run("compose.format", &json!({"kind": "plain"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], true);
        let m = folder_items(&s, FolderRole::Outbox)[0];
        assert!(m.body.html.is_none());
        assert_eq!(m.in_reply_to, vec!["q@x".to_string()]);
        assert!(matches!(s.mailbox.mail[&src].response, ResponseState::Replied(_)));
    }

    #[test]
    fn draft_upsert_and_discard() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.setSubject", &json!({"value": "Draft"})).unwrap();
        let d1 = s.run("compose.saveDraft", &json!({})).unwrap()["draft"].as_u64().unwrap();
        s.run("compose.setSubject", &json!({"value": "Draft 2"})).unwrap();
        let d2 = s.run("compose.saveDraft", &json!({})).unwrap()["draft"].as_u64().unwrap();
        assert_eq!(d1, d2);
        assert_eq!(s.mailbox.mail[&ItemId(d1)].subject, "Draft 2");
        assert!(s.mailbox.mail[&ItemId(d1)].is_draft);
        assert!(s.composes.contains_key(&id), "save keeps compose open");
        s.run("compose.discard", &json!({})).unwrap();
        assert!(s.composes.is_empty());
        assert!(!s.mailbox.mail.contains_key(&ItemId(d1)));
    }

    #[test]
    fn close_prompts_when_dirty() {
        let mut s = session();
        open(&mut s);
        s.run("compose.setSubject", &json!({"value": "x"})).unwrap();
        assert_eq!(s.run("compose.close", &json!({})).unwrap()["dialog"], "saveChanges");
        let r = s.run("compose.close", &json!({"save": true})).unwrap();
        assert!(r["draft"].as_u64().is_some());
        assert!(s.composes.is_empty());
    }

    #[test]
    fn toggles_and_options() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.showBcc", &json!({})).unwrap();
        s.run("compose.showFrom", &json!({})).unwrap();
        s.run("compose.importance", &json!({"level": "high"})).unwrap();
        assert_eq!(s.composes[&id].importance, Importance::High);
        s.run("compose.importance", &json!({"level": "high"})).unwrap();
        assert_eq!(s.composes[&id].importance, Importance::Normal);
        s.run("compose.deliveryReceipt", &json!({})).unwrap();
        s.run("compose.encrypt", &json!({})).unwrap();
        s.run("compose.votingButtons", &json!({"choices": "Yes;No"})).unwrap();
        s.run("compose.directRepliesTo", &json!({"addresses": "r@example.com"})).unwrap();
        assert!(s.run("compose.directRepliesTo", &json!({"addresses": "nope"})).is_err());
        s.run("compose.delayDelivery", &json!({"at": "2026-10-10T08:00"})).unwrap();
        s.run("compose.saveSentItemTo", &json!({"mode": "none"})).unwrap();
        s.run("compose.followUp", &json!({"kind": "tomorrow"})).unwrap();
        s.run("compose.sensitivity", &json!({"level": "private"})).unwrap();
        let c = &s.composes[&id];
        assert!(c.show_bcc && c.show_from && c.request_delivery_receipt && c.encrypt);
        assert_eq!(c.voting, "Yes;No");
        assert_eq!(c.reply_to, "r@example.com");
        assert_eq!(c.delay_until.unwrap().civil(0).hour, 8);
        assert_eq!(c.save_sent, SaveSentTo::DoNotSave);
        match &c.flag {
            FlagState::Flagged { due: Some(d), .. } => assert_eq!(d.civil(0).day, 10),
            f => panic!("{f:?}"),
        }
        s.run("compose.followUp", &json!({"kind": "clear"})).unwrap();
        assert_eq!(s.composes[&id].flag, FlagState::None);
        // Encrypted sends are refused (no certificates yet).
        s.run("compose.setTo", &json!({"value": "x@example.com"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], false);
        assert_eq!(s.run("compose.messageOptions", &json!({})).unwrap()["dialog"], "messageOptions");
        assert_eq!(s.run("compose.addressBook", &json!({})).unwrap()["dialog"], "addressBook");
    }

    #[test]
    fn attachments_from_file_data_and_items() {
        let mut s = session();
        let id = open(&mut s);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        s.run("compose.attachFile", &json!({"path": path.to_string_lossy()})).unwrap();
        s.run("compose.attachFile", &json!({"name": "a.png", "data_base64": "iVBORw=="})).unwrap();
        assert_eq!(s.run("compose.attachFile", &json!({})).unwrap()["dialog"], "attachFile");
        let contact = s.mailbox.alloc_id();
        s.mailbox.contacts.insert(
            ItemId(contact),
            mailcraft_model::Contact {
                id: ItemId(contact),
                first_name: "Ann".into(),
                last_name: "Lee".into(),
                emails: vec!["ann@example.org".into()],
                ..Default::default()
            },
        );
        s.run("compose.attachItem", &json!({"itemId": contact.to_string()})).unwrap();
        let c = &s.composes[&id];
        assert_eq!(c.attachments.len(), 3);
        assert_eq!(c.attachments[0].mime_type, "text/plain");
        assert_eq!(c.attachments[1].mime_type, "image/png");
        assert!(String::from_utf8_lossy(&c.attachments[2].data).contains("FN:Ann Lee"));
        s.run("compose.removeAttachment", &json!({"name": "a.png"})).unwrap();
        assert_eq!(s.composes[&id].attachments.len(), 2);
        assert!(s.run("compose.attachItem", &json!({"itemId": "999999"})).is_err());
    }

    #[test]
    fn check_names_resolves_contacts() {
        let mut s = session();
        let id = open(&mut s);
        let cid = s.mailbox.alloc_id();
        s.mailbox.contacts.insert(
            ItemId(cid),
            mailcraft_model::Contact {
                id: ItemId(cid),
                first_name: "Ann".into(),
                last_name: "Lee".into(),
                emails: vec!["ann@example.org".into()],
                ..Default::default()
            },
        );
        s.run("compose.setTo", &json!({"value": "Ann Lee; zed"})).unwrap();
        let r = s.run("compose.checkNames", &json!({})).unwrap();
        assert_eq!(r["resolved"], 1);
        assert_eq!(r["unresolved"][0]["name"], "zed");
        assert!(s.composes[&id].to.starts_with("Ann Lee <ann@example.org>"));
    }

    #[test]
    fn signature_insert_replace_remove() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Body"})).unwrap();
        s.run("compose.signature", &json!({"name": "Work", "text": "-- \nMe"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("Me"));
        s.run("compose.signature", &json!({"name": "Home", "text": "Cheers"})).unwrap();
        let t = s.composes[&id].body.to_plain_text();
        assert!(t.contains("Cheers") && !t.contains("Me"), "{t}");
        s.run("compose.signature", &json!({"name": "none"})).unwrap();
        assert!(!s.composes[&id].body.to_plain_text().contains("Cheers"));
    }

    #[test]
    fn format_theme_page_color_background() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Hi"})).unwrap();
        s.run("compose.theme", &json!({"name": "Office 2007 - 2010", "part": "fonts"})).unwrap();
        assert_eq!(s.composes[&id].body.base.font_family.as_deref(), Some("Calibri"));
        s.run("compose.pageColor", &json!({"color": "#ffeecc"})).unwrap();
        assert!(s.composes[&id].body.background.is_some());
        s.run("compose.pageColor", &json!({"color": null})).unwrap();
        assert!(s.composes[&id].body.background.is_none());
        assert!(s.run("compose.switchBackground", &json!({})).is_err(), "dark mode off");
        s.view.dark = true;
        s.run("compose.switchBackground", &json!({})).unwrap();
        assert!(s.composes[&id].light_canvas);
        s.run("compose.format", &json!({"kind": "plain"})).unwrap();
        assert_eq!(s.composes[&id].format, BodyFormat::PlainText);
        assert_eq!(s.composes[&id].title(), "Untitled - Message (Plain Text)");
        assert!(s.run("compose.pageColor", &json!({"color": "red"})).is_err(), "disabled in plain text");
    }

    #[test]
    fn parse_when_forms() {
        let s = session();
        assert_eq!(parse_when(&s, "2026-10-09").unwrap().civil(0).day, 9);
        assert_eq!(parse_when(&s, "2026-10-09T10:30:15Z").unwrap().civil(0).minute, 30);
        assert_eq!(parse_when(&s, "2026-10-09T10:30-04:00").unwrap().civil(0).hour, 14);
        assert!(parse_when(&s, "Fri, 09 Oct 2026 10:00:00 +0000").is_some());
        assert!(parse_when(&s, "2026-13-01").is_none());
        assert!(parse_when(&s, "garbage").is_none());
    }

    #[test]
    fn recipients_with_separators_in_names() {
        let list = parse_recipients(
            "Rivera, Alice <alice@example.com>; \"Chen; Bob\" <bob@example.org>; c@example.net",
        )
        .unwrap();
        let boxes: Vec<_> = list.iter().flat_map(Address::mailboxes).collect();
        let got: Vec<(Option<&str>, &str)> =
            boxes.iter().map(|m| (m.name.as_deref(), m.addr.as_str())).collect();
        assert_eq!(
            got,
            [
                (Some("Rivera, Alice"), "alice@example.com"),
                (Some("Chen; Bob"), "bob@example.org"),
                (None, "c@example.net")
            ]
        );
        // Comma-separated addresses typed into one field still split.
        assert_eq!(parse_recipients("a@example.com, Bob <b@example.com>").unwrap().len(), 2);
        assert!(parse_recipients("Rivera, Alice").is_err());
        assert!(parse_recipients("\"unterminated; x@example.com").is_err());
        // Check Names keeps such entries as resolved.
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.setTo", &json!({"value": "Rivera, Alice <alice@example.com>"})).unwrap();
        let r = s.run("compose.checkNames", &json!({})).unwrap();
        assert_eq!(r["resolved"], 1, "{r}");
        assert_eq!(s.composes[&id].to, "Rivera, Alice <alice@example.com>");
    }

    #[test]
    fn follow_up_dates_match_message_flags() {
        use crate::cmd::mail::{FlagKind, flag_dates};
        // Sunday 2026-10-11 and Monday 2026-10-05 (weeks start on Sunday; work week Monday–Friday).
        for (y, mo, d) in [(2026, 10, 11), (2026, 10, 5), (2026, 10, 9), (2026, 10, 10)] {
            let mut s = session();
            s.fixed_now = DateTime::from_civil(y, mo, d, 12, 0, 0, 0);
            let id = open(&mut s);
            for (kind, fk) in [
                ("today", FlagKind::Today),
                ("tomorrow", FlagKind::Tomorrow),
                ("thisWeek", FlagKind::ThisWeek),
                ("nextWeek", FlagKind::NextWeek),
            ] {
                s.run("compose.followUp", &json!({"kind": kind})).unwrap();
                let (start, due) = flag_dates(&s, fk);
                match &s.composes[&id].flag {
                    FlagState::Flagged { start: a, due: b, .. } => {
                        assert_eq!((a, b), (&start, &due), "{kind} on {y}-{mo}-{d}")
                    }
                    f => panic!("{f:?}"),
                }
            }
        }
    }

    #[test]
    fn select_and_edit_primitives() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "abc"})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0], "offset": 1}, "focus": {"path": [0], "offset": 2}}),
        )
        .unwrap();
        s.run("compose.deleteText", &json!({"direction": "selection"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "ac");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "abc");
        s.run("compose.editRedo", &json!({})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "ac");
        assert!(s.run("compose.select", &json!({"anchor": {"path": "x"}})).is_err());
        assert!(s.run("compose.select", &json!({"compose": 999, "all": true})).is_err());
    }
}

#[cfg(test)]
mod depth_tests {
    use super::tests::{open, session};
    use super::*;
    use mailcraft_model::{Contact, MailItem};

    fn add_contact(s: &mut Session, first: &str, last: &str, emails: &[&str]) {
        let id = s.mailbox.alloc_id();
        s.mailbox.contacts.insert(
            ItemId(id),
            Contact {
                id: ItemId(id),
                first_name: first.into(),
                last_name: last.into(),
                emails: emails.iter().map(|e| (*e).to_owned()).collect(),
                ..Default::default()
            },
        );
    }

    #[test]
    fn signature_commands_persist_and_undo() {
        let mut s = session();
        let r = s
            .run(
                "compose.signatureCreate",
                &json!({"name": "Work", "html": "<p><b>Me</b></p>", "defaultNew": true}),
            )
            .unwrap();
        assert_eq!(r["created"], "Work");
        assert!(s.run("compose.signatureCreate", &json!({"name": "work"})).is_err(), "duplicate name");
        assert!(s.run("compose.signatureCreate", &json!({"name": ""})).is_err());
        s.run("compose.signatureCreate", &json!({"name": "Reply", "text": "-- me"})).unwrap();
        assert_eq!(s.mailbox.signatures.len(), 2);
        assert!(s.mailbox.signatures[0].default_new);
        // Journaled (the store diff does not cover signatures) and undoable.
        assert!(s.pending_ops.iter().any(|o| matches!(o, mailcraft_store::Op::Replace(_))));
        assert_eq!(s.undo.undo.last().map(|e| e.label.as_str()), Some("New Signature"));
        let acc = s.mailbox.default_account().map(|a| a.id.0).unwrap();
        let r = s.run("compose.signatureDefaults", &json!({"account": acc, "replies": "Reply"})).unwrap();
        assert_eq!(r["newMessages"], "Work");
        assert_eq!(r["replies"], "Reply");
        assert!(s.run("compose.signatureDefaults", &json!({"replies": "nope"})).is_err());
        assert!(s.run("compose.signatureDefaults", &json!({})).is_err());
        s.run(
            "compose.signatureEdit",
            &json!({"name": "reply", "newName": "Replies", "text": "-- me again"}),
        )
        .unwrap();
        let list = s.run("compose.signatureList", &json!({})).unwrap();
        assert_eq!(list["signatures"][1]["name"], "Replies");
        assert_eq!(list["signatures"][1]["text"], "-- me again");
        let me = list["accounts"].as_array().unwrap().iter().find(|a| a["account"] == acc).unwrap().clone();
        assert_eq!(me["replies"], "Replies");
        s.run("compose.signatureDelete", &json!({"name": "Replies"})).unwrap();
        assert_eq!(s.mailbox.signatures.len(), 1);
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.signatures.len(), 2);
        assert!(s.run("compose.signatureDelete", &json!({"name": "ghost"})).is_err());
        // Insert a saved signature by name into a compose.
        let id = open(&mut s);
        s.run("compose.signature", &json!({"name": "work"})).unwrap();
        assert_eq!(s.composes[&id].signature.as_ref().map(|g| g.name.as_str()), Some("Work"));
        assert!(s.composes[&id].body.to_plain_text().contains("Me"));
        assert!(s.run("compose.signature", &json!({"name": "ghost"})).is_err());
    }

    #[test]
    fn reply_gets_the_reply_signature() {
        let mut s = session();
        let acc = s.mailbox.default_account().map(|a| a.id).unwrap();
        let inbox = s.mailbox.role_folder(acc, FolderRole::Inbox).unwrap();
        let mid = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Hello".into(),
            from: Some(mailcraft_mime::Mailbox::new(Some("Ann"), "ann@example.org")),
            received: s.now(),
            ..Default::default()
        });
        s.run("compose.signatureCreate", &json!({"name": "R", "text": "-- R sig", "defaultReply": true}))
            .unwrap();
        s.selection.items = vec![mid];
        s.selection.focus = Some(mid);
        let r = s.run("mail.reply", &json!({})).unwrap();
        let cid = r["compose"].as_u64().unwrap();
        let c = &s.composes[&cid];
        assert!(matches!(c.body.blocks[1], Block::Container { .. }), "{:?}", c.body.blocks);
        let text = c.body.to_plain_text();
        let sig_at = text.find("-- R sig").unwrap();
        let header_at = text.find("From:").unwrap();
        assert!(sig_at < header_at, "{text}");
    }

    #[test]
    fn send_feeds_auto_complete() {
        let mut s = session();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "Ann Lee <ann@example.org>; bo@example.net"})).unwrap();
        s.run("compose.setCc", &json!({"value": "Cy <cy@example.com>"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
        let addrs: Vec<&str> = s.mailbox.recent_recipients.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(addrs, ["ann@example.org", "bo@example.net", "cy@example.com"]);
        let r = s.run("compose.suggestRecipients", &json!({"prefix": "an"})).unwrap();
        assert_eq!(r["suggestions"][0]["entry"], "Ann Lee <ann@example.org>");
        assert_eq!(r["suggestions"][0]["source"], "recent");
        add_contact(&mut s, "Andrea", "Ng", &["andrea@example.com"]);
        let r = s.run("compose.suggestRecipients", &json!({"prefix": "x@y.z; AN", "limit": 5})).unwrap();
        let got: Vec<&str> =
            r["suggestions"].as_array().unwrap().iter().map(|v| v["address"].as_str().unwrap()).collect();
        assert_eq!(got, ["ann@example.org", "andrea@example.com"]);
        assert_eq!(
            s.run("compose.forgetRecipient", &json!({"address": "ANN@example.org"})).unwrap()["removed"],
            true
        );
        assert_eq!(s.mailbox.recent_recipients.len(), 2);
        assert_eq!(s.run("compose.emptyAutoComplete", &json!({})).unwrap()["removed"], 2);
        assert!(s.mailbox.recent_recipients.is_empty());
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.recent_recipients.len(), 2);
    }

    #[test]
    fn check_names_ambiguity_and_auto_complete() {
        let mut s = session();
        let id = open(&mut s);
        add_contact(&mut s, "Ann", "Lee", &["ann@example.org", "ann.lee@example.com"]);
        add_contact(&mut s, "Bob", "Chen", &["bob@example.org"]);
        s.mailbox.record_recipients(&[("Zed Zulu".into(), "zed@example.net".into())], s.now());
        s.run("compose.setTo", &json!({"value": "Ann Lee; bob; Zed Zulu; nobody"})).unwrap();
        let r = s.run("compose.checkNames", &json!({})).unwrap();
        assert_eq!(r["resolved"], 2, "{r}");
        let unresolved = r["unresolved"].as_array().unwrap();
        assert_eq!(unresolved.len(), 2);
        assert_eq!(unresolved[0]["name"], "Ann Lee");
        assert_eq!(unresolved[0]["candidates"].as_array().unwrap().len(), 2, "two addresses = ambiguous");
        assert_eq!(unresolved[1]["candidates"].as_array().unwrap().len(), 0);
        assert_eq!(
            s.composes[&id].to,
            "Ann Lee; Bob Chen <bob@example.org>; Zed Zulu <zed@example.net>; nobody"
        );
        // An exact address picks that address of the contact.
        s.run("compose.setTo", &json!({"value": "ANN.LEE@example.com"})).unwrap();
        assert_eq!(s.run("compose.checkNames", &json!({})).unwrap()["resolved"], 1);
    }

    #[test]
    fn attachment_rename_types_and_forward_as_item() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.attachFile", &json!({"name": "photo.JPEG", "data_base64": "/9j/"})).unwrap();
        assert_eq!(s.composes[&id].attachments[0].mime_type, "image/jpeg");
        let r = s.run("compose.renameAttachment", &json!({"index": 0, "newName": "scan.pdf"})).unwrap();
        assert_eq!(r["mimeType"], "application/pdf");
        assert!(s.run("compose.renameAttachment", &json!({"index": 5, "newName": "x"})).is_err());
        assert!(s.run("compose.renameAttachment", &json!({"name": "scan.pdf"})).is_err(), "newName required");
        let acc = s.mailbox.default_account().map(|a| a.id).unwrap();
        let inbox = s.mailbox.role_folder(acc, FolderRole::Inbox).unwrap();
        let mid = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Report".into(),
            from: Some(mailcraft_mime::Mailbox::new(Some("Ann"), "ann@example.org")),
            body: mailcraft_model::Body { text: Some("Numbers inside.".into()), ..Default::default() },
            received: s.now(),
            ..Default::default()
        });
        s.run("compose.attachItem", &json!({"itemId": mid.0})).unwrap();
        let c = s.composes[&id].clone();
        assert_eq!(c.attachments[1].mime_type, "message/rfc822");
        assert_eq!(c.attachments[1].filename, "Report.eml");
        // The item travels as message/rfc822, not base64 (RFC 2046 §5.2.1).
        let raw = String::from_utf8_lossy(&build_message(&s, &c, None, false).build()).into_owned();
        let part = raw.find("Content-Type: message/rfc822").unwrap();
        assert!(!raw[part..].lines().take(4).any(|l| l.contains("base64")), "{raw}");
        assert!(raw.contains("Numbers inside."));
    }

    #[test]
    fn table_commands() {
        let mut s = session();
        let id = open(&mut s);
        assert!(s.run("compose.tableEdit", &json!({"action": "deleteRow"})).is_err(), "not in a table");
        s.run("insert.table", &json!({"rows": 1, "cols": 2})).unwrap();
        s.run("compose.insertText", &json!({"text": "a"})).unwrap();
        assert_eq!(s.run("compose.tableCell", &json!({})).unwrap()["cell"]["col"], 1);
        assert_eq!(s.run("compose.tableCell", &json!({"direction": "next"})).unwrap()["cell"]["row"], 1);
        assert!(s.run("compose.tableCell", &json!({"direction": "sideways"})).is_err());
        let r = s.run("compose.tableCell", &json!({"direction": "up"})).unwrap();
        assert_eq!(r["moved"], true);
        s.run("compose.tableEdit", &json!({"action": "insertColumnLeft"})).unwrap();
        match s.composes[&id].body.blocks.iter().find(|b| matches!(b, Block::Table { .. })) {
            Some(Block::Table { rows, .. }) => assert_eq!((rows.len(), rows[0].len()), (2, 3)),
            other => panic!("{other:?}"),
        }
        assert!(s.run("compose.tableEdit", &json!({"action": "explode"})).is_err());
        s.run("compose.tableEdit", &json!({"action": "deleteTable"})).unwrap();
        assert!(!s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Table { .. })));
        s.run("compose.editUndo", &json!({})).unwrap();
        assert!(s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Table { .. })));
    }
}

#[cfg(test)]
mod review_tests {
    use super::tests::{open, session};
    use super::*;

    fn text(s: &Session, id: u64) -> String {
        s.composes[&id].body.to_plain_text()
    }

    #[test]
    fn new_email_gets_the_default_signature() {
        let mut s = session();
        s.run("compose.signatureCreate", &json!({"name": "Work", "text": "Me Example", "defaultNew": true}))
            .unwrap();
        let r = s.run("mail.newEmail", &json!({})).unwrap();
        let id = r["compose"].as_u64().unwrap();
        let c = &s.composes[&id];
        assert_eq!(c.signature.as_ref().map(|g| g.name.as_str()), Some("Work"));
        assert_eq!(text(&s, id), "\nMe Example");
        assert_eq!(c.selection.focus, DocPos::new(vec![0], 0), "caret on the empty first line");
        assert!(!c.dirty, "an untouched new message closes without a save prompt");
    }

    #[test]
    fn switching_signature_after_editing_it_replaces_it() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Body"})).unwrap();
        s.run("compose.signature", &json!({"name": "Work", "text": "Me Example\nSales"})).unwrap();
        // Fix a word in the signature's second line, then pick another signature.
        assert!(
            matches!(&s.composes[&id].body.blocks[1], Block::Container { blocks, .. } if blocks.len() == 2)
        );
        s.run("compose.select", &json!({"anchor": {"path": [1, 1], "offset": 5}})).unwrap();
        s.run("compose.insertText", &json!({"text": " team"})).unwrap();
        assert!(text(&s, id).contains("Sales team"));
        s.run("compose.signature", &json!({"name": "Home", "text": "Cheers"})).unwrap();
        let t = text(&s, id);
        assert!(t.contains("Cheers") && !t.contains("Me Example") && !t.contains("Sales"), "{t}");
    }

    #[test]
    fn undoing_a_signature_switch_then_switching_again_keeps_one_signature() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Body"})).unwrap();
        s.run("compose.signature", &json!({"name": "Work", "text": "Me Example"})).unwrap();
        s.run("compose.signature", &json!({"name": "Home", "text": "Cheers"})).unwrap();
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id).trim_end(), "Body\nMe Example");
        assert_eq!(s.composes[&id].signature.as_ref().map(|g| g.name.as_str()), Some("Work"));
        s.run("compose.signature", &json!({"name": "Short", "text": "-- me"})).unwrap();
        assert_eq!(text(&s, id).trim_end(), "Body\n-- me", "the Work signature is replaced, not kept");
        // Redo after undo brings the later choice back with its name.
        s.run("compose.editUndo", &json!({})).unwrap();
        s.run("compose.editRedo", &json!({})).unwrap();
        assert_eq!(s.composes[&id].signature.as_ref().map(|g| g.name.as_str()), Some("Short"));
    }

    #[test]
    fn typing_is_one_undo_step_until_the_caret_moves() {
        let mut s = session();
        let id = open(&mut s);
        for ch in ["H", "e", "l", "l", "o"] {
            s.run("compose.insertText", &json!({"text": ch})).unwrap();
        }
        s.run("compose.insertText", &json!({"text": "\n"})).unwrap();
        for ch in ["W", "o"] {
            s.run("compose.insertText", &json!({"text": ch})).unwrap();
        }
        assert_eq!(text(&s, id), "Hello\nWo");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), "Hello\n", "Enter starts a new typing step");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), "Hello");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), "");
        s.run("compose.editRedo", &json!({})).unwrap();
        assert_eq!(text(&s, id), "Hello");
        // Moving the caret ends the step.
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 0}})).unwrap();
        s.run("compose.insertText", &json!({"text": ">"})).unwrap();
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 6}})).unwrap();
        s.run("compose.insertText", &json!({"text": "!"})).unwrap();
        assert_eq!(text(&s, id), ">Hello!");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), ">Hello");
        // Backspaces in a row are one step too.
        for _ in 0..3 {
            s.run("compose.deleteText", &json!({"direction": "backward"})).unwrap();
        }
        assert_eq!(text(&s, id), ">He");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), ">Hello");
    }

    #[test]
    fn edits_that_change_nothing_leave_no_undo_step() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "ab"})).unwrap();
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 0}})).unwrap();
        // Backspace at the start of the body does nothing.
        s.run("compose.deleteText", &json!({"direction": "backward"})).unwrap();
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(text(&s, id), "");
        // ...and does not throw away the redo history.
        s.run("compose.deleteText", &json!({"direction": "backward"})).unwrap();
        assert_eq!(s.run("compose.editRedo", &json!({})).unwrap()["redone"], true);
        assert_eq!(text(&s, id), "ab");
    }
}
