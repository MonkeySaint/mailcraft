//! `compose.*`: the compose window / inline response (header fields, send, drafts, attachments, tracking and
//! delivery options, themes). Every command takes an optional `{"compose": id}`; without it the most recently
//! opened compose is used.
//!
//! Tracking and delivery options travel with the queued message as header fields: a read receipt request is
//! `Disposition-Notification-To` (RFC 8098); a delivery receipt and Save Sent Item To are submission-only
//! `X-MailCraft-Submit-*` fields that the SMTP client turns into RFC 3461 DSN parameters and strips
//! (`mailcraft_net::smtp`), and that [`file_sent_copy`] reads when the send completes; voting buttons are
//! MailCraft's own `X-MailCraft-Voting` field, answered with `X-MailCraft-Vote` replies (`compose.vote`) and
//! tallied on the sent item ([`tracking`], `compose.tracking`). `compose.sendReadReceipt` answers a received
//! read-receipt request with an RFC 8098 disposition notification. S/MIME encryption is not implemented:
//! `compose.encrypt` explains that and stays off.
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
use mailcraft_geom::Color;
use mailcraft_mime::{Address, AttachmentSpec, MessageBuilder, parse_address_list};
use mailcraft_model::{
    Account, Block, BodyFormat, DateTime, Doc, FlagState, FolderKind, FolderRole, Importance, ItemId,
    MailItem, ResponseState, Sensitivity,
};

/// Error shown when sending without an account that can send (plan/reference/reading-pane.md).
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

// ---------------------------------------------------------------------------------------------------------
// Recipient fields
//
// A recipient field (To, Cc, Bcc, Direct Replies To) holds entries separated by `;` outside quoted names and
// angle brackets, and each entry is one recipient: an address, "Name <address>", or a name still to be
// resolved (Check Names). How commas are read depends on where the text comes from ([`RecipientInput`]):
// - typed into the compose window's recipient boxes (`compose.setTo` / `setCc` / `setBcc` with
//   `"typed": true`): a comma separates recipients only while File › Options › Mail › Send messages ›
//   "Commas can be used to separate multiple message recipients" (`mail.send.commasSeparate`) is on. With it
//   off, a comma belongs to the entry: "Rivera, Alice" is one name to resolve, and "a@example.com,
//   b@example.com" is one entry that is not an address;
// - anything else (CLI, MCP, the control channel, mailto: links, the Address Book: no `typed`) is an RFC 5322
//   address list, so a comma always separates recipients.
// A separating comma is stored as `;` (same length, so a text cursor stays put), which means a stored field
// never uses commas as separators and [`parse_recipients`] reads one recipient per entry. One entry written
// as "Last, First <address>" stays one recipient either way.

/// Where the text of a recipient field comes from (see "Recipient fields" above).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecipientInput {
    /// Typed into a recipient box: commas follow `mail.send.commasSeparate`.
    Typed,
    /// An RFC 5322 address list: commas separate.
    List,
}

/// Byte offsets of the separators of a recipient field, in order: `;` outside quoted names and angle
/// brackets and, when `commas`, `,` likewise (except inside a "Last, First <address>" entry).
fn recipient_separators(field: &str, commas: bool) -> Vec<usize> {
    let mut semis = Vec::new();
    let mut comma_at = Vec::new();
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
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
            ';' if !quoted && !angle => semis.push(i),
            ',' if commas && !quoted && !angle => comma_at.push(i),
            _ => {}
        }
    }
    if comma_at.is_empty() {
        return semis;
    }
    // Commas inside an entry that is one comma-named mailbox stay part of its name.
    let mut out = semis.clone();
    let mut start = 0usize;
    let mut pending = comma_at.iter().copied().peekable();
    for end in semis.iter().copied().chain(std::iter::once(field.len())) {
        let whole = field.get(start..end).is_some_and(|e| comma_named_mailbox(e).is_some());
        while let Some(c) = pending.next_if(|&c| c < end) {
            if !whole {
                out.push(c);
            }
        }
        start = end.saturating_add(1);
    }
    out.sort_unstable();
    out
}

/// Splits a recipient field at the `;` separators that are outside quoted names and angle brackets; the
/// entries come back trimmed, empty ones dropped.
pub(crate) fn split_recipients(field: &str) -> Vec<&str> {
    split_recipients_with(field, false)
}

/// [`split_recipients`], also splitting at separating commas when `commas`.
pub(crate) fn split_recipients_with(field: &str, commas: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in recipient_separators(field, commas) {
        out.extend(field.get(start..i));
        start = i.saturating_add(1);
    }
    out.extend(field.get(start..));
    out.into_iter().map(str::trim).filter(|x| !x.is_empty()).collect()
}

/// Whether commas separate recipients in text that came in as `input`.
pub(crate) fn commas_separate(s: &Session, input: RecipientInput) -> bool {
    match input {
        RecipientInput::List => true,
        RecipientInput::Typed => s.settings.bool("mail.send.commasSeparate"),
    }
}

/// A recipient field as stored: its separating commas (when commas separate for `input`) turned into `;`.
pub(crate) fn store_recipients(s: &Session, text: &str, input: RecipientInput) -> String {
    if !commas_separate(s, input) || !text.contains(',') {
        return text.to_owned();
    }
    let commas: Vec<usize> = recipient_separators(text, true)
        .into_iter()
        .filter(|&i| text.as_bytes().get(i) == Some(&b','))
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut start = 0usize;
    for i in commas {
        out.push_str(text.get(start..i).unwrap_or(""));
        out.push(';');
        start = i.saturating_add(1);
    }
    out.push_str(text.get(start..).unwrap_or(""));
    out
}

/// The `typed` param of `compose.setTo` / `setCc` / `setBcc`.
fn recipient_input(p: &Value) -> Result<RecipientInput, CmdError> {
    match p.get("typed") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(RecipientInput::List),
        Some(Value::Bool(true)) => Ok(RecipientInput::Typed),
        Some(_) => Err(CmdError::Params("`typed` must be true or false".into())),
    }
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

/// Parses a stored recipient field ("a@b; Name <c@d>; Last, First <e@f>"): one recipient per `;` entry (see
/// "Recipient fields" above). Returns the addresses, or the entries that are not valid addresses (an entry
/// holding several addresses, "a@b, c@d" typed while commas do not separate, is one bad entry).
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
        let [a] = list.as_slice() else {
            bad.push(piece.to_owned());
            continue;
        };
        if a.mailboxes().iter().all(|m| valid_addr(&m.addr)) {
            out.push(a.clone());
        } else {
            for m in a.mailboxes().iter().filter(|m| !valid_addr(&m.addr)) {
                bad.push(m.to_display_string());
            }
        }
    }
    if bad.is_empty() { Ok(out) } else { Err(bad) }
}

/// [`parse_recipients`] for building a message (sending checks the fields first; drafts keep what the user
/// typed): an entry that is not one recipient is read as an address list, so "a@b, c@d" typed while commas
/// do not separate keeps both addresses, and a name waiting for Check Names keeps its words.
pub(crate) fn parse_recipients_lenient(field: &str) -> Vec<Address> {
    split_recipients(field)
        .into_iter()
        .flat_map(|piece| parse_recipients(piece).unwrap_or_else(|_| parse_address_list(piece)))
        .collect()
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
    let lenient = parse_recipients_lenient;
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
    let st = &s.settings;
    let mut text = c.plain_text_with_list_formats();
    if c.format == BodyFormat::PlainText && !for_draft {
        // Options › Mail › Message format › "Automatically wrap text at character".
        text = wrap_plain_text(&text, st.wrap_at());
    }
    b.text = Some(text);
    // Options › Mail › Message format › "When sending messages in Rich Text format to Internet recipients":
    // converted to HTML (MailCraft has no Rich Text transport, so "Send using Rich Text format" sends HTML
    // too) or to plain text.
    let rich_as_plain = c.format == BodyFormat::RichText
        && !for_draft
        && st.text("mail.format.richTextToInternet") == "plain";
    if c.format != BodyFormat::PlainText && !rich_as_plain {
        b.html = Some(c.apply_list_formats_html(&mailcraft_html::doc_to_html(&c.body)));
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
    if c.format != BodyFormat::PlainText && !rich_as_plain {
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
    if let Some(addr) = &notify_to
        && c.request_read_receipt
    {
        // RFC 8098 read receipt (message disposition notification) request.
        b.extra_headers.push(("Disposition-Notification-To".into(), addr.clone()));
    }
    if c.request_delivery_receipt {
        // RFC 3461 delivery status notification, turned into envelope parameters at submission (no legacy
        // Return-Receipt-To: servers ignore it and recipients' clients may answer it).
        b.extra_headers.push((DSN_HEADER.into(), DSN_REQUEST.into()));
    }
    match c.save_sent {
        SaveSentTo::Default => {}
        SaveSentTo::DoNotSave => b.extra_headers.push((SAVE_SENT_HEADER.into(), "none".into())),
        SaveSentTo::Folder(f) => b.extra_headers.push((SAVE_SENT_HEADER.into(), format!("folder={}", f.0))),
    }
    let choices = voting_choices(&c.voting);
    if !choices.is_empty() {
        b.extra_headers
            .push((VOTING_HEADER.into(), mailcraft_mime::encoded::encode_words(&choices.join(";"))));
    }
    if let Some(v) = c.vote_response.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        b.extra_headers.push((VOTE_HEADER.into(), mailcraft_mime::encoded::encode_words(v)));
    }
    match c.sensitivity {
        Sensitivity::Normal => {}
        Sensitivity::Personal => b.extra_headers.push(("Sensitivity".into(), "Personal".into())),
        Sensitivity::Private => b.extra_headers.push(("Sensitivity".into(), "Private".into())),
        Sensitivity::Confidential => {
            b.extra_headers.push(("Sensitivity".into(), "Company-Confidential".into()))
        }
    }
    if !for_draft && let Some(days) = st.expire_days() {
        // Options › Mail › Send messages › "Mark messages as expired after this many days": the expiry
        // travels with the message (RFC 4021 Expiry-Date).
        let sent = c.delay_until.unwrap_or_else(|| s.now());
        let at = DateTime {
            unix: sent.unix.saturating_add(i64::from(days).saturating_mul(86_400)),
            offset_min: sent.offset_min,
        };
        b.extra_headers.push(("Expiry-Date".into(), at.to_rfc5322()));
    }
    if !for_draft
        && c.format == BodyFormat::PlainText
        && st.bool("mail.format.uuencode")
        && !b.attachments.is_empty()
    {
        // Options › Mail › Message format › "Encode attachments in UUENCODE format when sending plain-text
        // messages": the files go into the text as uuencoded blocks instead of MIME parts.
        let mut text = b.text.take().unwrap_or_default();
        for a in std::mem::take(&mut b.attachments) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
            text.push_str(&uuencode(&a.filename, &a.data));
        }
        b.text = Some(text);
    }
    if for_draft {
        b.extra_headers.push(("X-Unsent".into(), "1".into()));
    } else {
        // Options › Advanced › International options: charsets, vCard encoding, ASCII domains.
        crate::intl::apply_outgoing(s, &mut b);
    }
    b.keep_bcc = for_draft;
    b
}

/// Wraps each line of plain text longer than `width` chars at the last space before the limit (a word longer
/// than the limit, like a link, is left whole; quoted "> " lines are wrapped like any other).
pub(crate) fn wrap_plain_text(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut rest: &str = line;
        while rest.chars().count() > width {
            // The first char that does not fit (a space there still ends the line at the limit).
            let Some((limit, ch)) = rest.char_indices().nth(width) else { break };
            let head = rest.get(..limit.saturating_add(ch.len_utf8())).unwrap_or(rest);
            // The last space up to the limit that has text before it (leading indentation is kept);
            // otherwise the first space after it (a long word or link stays whole).
            let cut = head
                .rfind(' ')
                .filter(|&i| head.get(..i).is_some_and(|h| !h.trim().is_empty()))
                .or_else(|| rest.get(limit..).and_then(|t| t.find(' ')).map(|i| i.saturating_add(limit)));
            let Some(cut) = cut else { break };
            out.push_str(rest.get(..cut).unwrap_or("").trim_end());
            out.push('\n');
            rest = rest.get(cut..).unwrap_or("").trim_start_matches(' ');
        }
        out.push_str(rest);
    }
    out
}

/// One file as a uuencoded block ("begin 644 name" … "end"; 45 bytes per line).
pub(crate) fn uuencode(name: &str, data: &[u8]) -> String {
    fn enc(v: u8) -> char {
        let v = v & 0x3f;
        if v == 0 { '`' } else { char::from(v.saturating_add(32)) }
    }
    let name: String = name.chars().map(|c| if c.is_control() { '_' } else { c }).collect();
    let name = if name.trim().is_empty() { "attachment".to_owned() } else { name.trim().to_owned() };
    let mut out = format!("begin 644 {name}\n");
    for chunk in data.chunks(45) {
        // A chunk holds at most 45 bytes, so its length always fits in a u8.
        out.push(enc(u8::try_from(chunk.len()).unwrap_or(45)));
        for group in chunk.chunks(3) {
            let b0 = group.first().copied().unwrap_or(0);
            let b1 = group.get(1).copied().unwrap_or(0);
            let b2 = group.get(2).copied().unwrap_or(0);
            out.push(enc(b0 >> 2));
            out.push(enc((b0 << 4) | (b1 >> 4)));
            out.push(enc((b1 << 2) | (b2 >> 6)));
            out.push(enc(b2));
        }
        out.push('\n');
    }
    out.push_str("`\nend\n");
    out
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
            // Options › Mail › Save messages › "Save to this folder" (Drafts unless changed); Drafts when the
            // account has no such folder.
            let role = s.settings.draft_folder_role();
            let find = |role: FolderRole| {
                account
                    .as_ref()
                    .and_then(|a| s.mailbox.role_folder(a.id, role))
                    .or_else(|| s.mailbox.folders.values().find(|f| f.role == Some(role)).map(|f| f.id))
            };
            find(role).or_else(|| find(FolderRole::Drafts))
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

/// Options › Mail › Send messages › "Automatic name checking": before a send, recipient entries that are
/// not addresses are resolved against the contacts; when one matches several, the Check Names dialog asks
/// which (and the message is not sent). `Some` = stop with this result.
fn auto_check_names(s: &mut Session, cid: u64, p: &Value) -> Result<Option<Value>, CmdError> {
    if !s.settings.bool("mail.send.autoNameCheck") || opt_bool(p, "checkNames") == Some(false) {
        return Ok(None);
    }
    let Some(c) = s.composes.get(&cid) else { return Ok(None) };
    if [&c.to, &c.cc, &c.bcc].iter().all(|f| parse_recipients(f).is_ok()) {
        return Ok(None);
    }
    let (_, unresolved) = resolve_names(s, cid)?;
    // Names with no match at all fall through to the "not valid" error below.
    let ambiguous: Vec<Value> = unresolved
        .into_iter()
        .filter(|u| u.get("candidates").and_then(Value::as_array).is_some_and(|a| !a.is_empty()))
        .collect();
    if ambiguous.is_empty() {
        return Ok(None);
    }
    s.notify(Notice::OpenDialog("checkNames".into(), json!({"compose": cid, "unresolved": ambiguous})));
    Ok(Some(json!({"sent": false, "compose": cid, "reason": "checkNames", "unresolved": ambiguous})))
}

/// Options › Mail › Compose messages › "Always check spelling before sending": a message with spelling
/// issues asks first (the confirm dialog sends with `"spellCheck": false`). `Some` = stop with this result.
fn spelling_before_send(s: &mut Session, cid: u64, p: &Value) -> Option<Value> {
    if !s.settings.bool("mail.compose.spellBeforeSend") || opt_bool(p, "spellCheck") == Some(false) {
        return None;
    }
    let sp = crate::cmd::review::speller(s);
    let c = s.composes.get_mut(&cid)?;
    crate::cmd::review::spell_check(&sp, c, false, 1);
    let count = c.spell_issues.iter().filter(|i| i.kind == "spelling" || i.kind == "repeated").count();
    if count == 0 {
        return None;
    }
    let what = if count == 1 { "1 spelling issue".to_owned() } else { format!("{count} spelling issues") };
    s.notify(Notice::OpenDialog(
        "confirm".into(),
        json!({
            "title": "Spelling",
            "text": format!("This message has {what}. Send it anyway?"),
            "command": "compose.send",
            "params": {"compose": cid, "spellCheck": false},
        }),
    ));
    Some(json!({"sent": false, "compose": cid, "reason": "spelling", "issues": count}))
}

fn send(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    if !s.composes.contains_key(&cid) {
        return Err(CmdError::Disabled(NO_COMPOSE.into()));
    }
    if let Some(stop) = auto_check_names(s, cid, p)? {
        return Ok(stop);
    }
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
    // Options › Advanced › "Allow international domain names in email addresses".
    if let Some(msg) = crate::intl::idn_problem(s, &[&c.to, &c.cc, &c.bcc, &c.reply_to]) {
        return fail(s, msg);
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
    if let Some(stop) = spelling_before_send(s, cid, p) {
        return Ok(stop);
    }
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
    // Rules for sent messages (Cc, defer delivery, sensitivity…) edit the Outbox copy before it goes out.
    crate::rules_exec::on_send(s, &[id]);
    // Delay Delivery or a deferring rule: the message waits in the Outbox until its send time.
    let deferred = s.mailbox.mail.get(&id).and_then(|m| m.sent).filter(|t| t.unix > now.unix);
    // "Send immediately when connected": the account's worker submits the Outbox now (deferred messages
    // wait); otherwise the next send/receive does.
    if deferred.is_none() {
        crate::sync::send_now(s, account.id);
    }
    Ok(
        json!({"sent": true, "item": id.0, "outbox": outbox.0, "deferredUntil": deferred.map(|d| d.to_rfc3339())}),
    )
}

fn save_draft_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let id = save_draft(s, cid)?;
    Ok(json!({"draft": id.0, "compose": cid}))
}

/// Options › Mail › Save messages › "Automatically save items that have not been sent after this many
/// minutes": saves every open compose whose changes have been unsaved that long (an untouched message is
/// never saved). Runs from `compose.autoSaveTick` (the window's timer) and after typing. The saved drafts are
/// written to the store like any change, without an undo step. Returns (compose, draft) pairs.
pub fn auto_save(s: &mut Session) -> Vec<(u64, ItemId)> {
    let Some(minutes) = s.settings.auto_save_minutes() else {
        for c in s.composes.values_mut() {
            c.dirty_since = None;
        }
        return Vec::new();
    };
    let now = s.now().unix;
    let period = i64::from(minutes).saturating_mul(60);
    let mut due = Vec::new();
    for (cid, c) in &mut s.composes {
        if !c.dirty {
            c.dirty_since = None;
            continue;
        }
        let since = *c.dirty_since.get_or_insert(now);
        if now.saturating_sub(since) >= period {
            due.push(*cid);
        }
    }
    if due.is_empty() {
        return Vec::new();
    }
    let before = s.mailbox.clone();
    let mut saved = Vec::new();
    for cid in due {
        let result = save_draft(s, cid);
        if let Some(c) = s.composes.get_mut(&cid) {
            // Saved: the clock restarts with the next change. Failed: try again after another period.
            c.dirty_since = if result.is_ok() { None } else { Some(now) };
        }
        match result {
            Ok(id) => saved.push((cid, id)),
            Err(e) => log::warn!("auto-save of compose {cid} failed: {e}"),
        }
    }
    if !saved.is_empty() {
        crate::sync::queue_local_changes(s, &before);
        let ops = crate::session::diff_ops(&before, &s.mailbox);
        s.journal(ops);
    }
    saved
}

fn auto_save_tick(s: &mut Session, _p: &Value) -> CmdResult {
    let saved = auto_save(s);
    let list: Vec<Value> = saved.iter().map(|(c, d)| json!({"compose": c, "draft": d.0})).collect();
    Ok(json!({"saved": list, "minutes": s.settings.auto_save_minutes()}))
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

/// A recipient field's new text from `value`, read by the `typed` rule (see "Recipient fields").
fn recipient_value(s: &Session, p: &Value) -> Result<String, CmdError> {
    let input = recipient_input(p)?;
    Ok(store_recipients(s, &field_value(p)?, input))
}

fn set_to(s: &mut Session, p: &Value) -> CmdResult {
    let v = recipient_value(s, p)?;
    let c = compose_mut(s, p)?;
    c.to = v;
    c.dirty = true;
    Ok(json!({"to": c.to}))
}

fn set_cc(s: &mut Session, p: &Value) -> CmdResult {
    let v = recipient_value(s, p)?;
    let c = compose_mut(s, p)?;
    c.cc = v;
    c.dirty = true;
    Ok(json!({"cc": c.cc}))
}

fn set_bcc(s: &mut Session, p: &Value) -> CmdResult {
    let v = recipient_value(s, p)?;
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
    let (resolved, unresolved) = resolve_names(s, cid)?;
    if !unresolved.is_empty() {
        s.notify(Notice::OpenDialog("checkNames".into(), json!({"compose": cid, "unresolved": unresolved})));
    }
    Ok(json!({"resolved": resolved, "unresolved": unresolved}))
}

/// Check Names without the dialog: replaces each recipient entry that is not an address but matches exactly
/// one contact (or Auto-Complete entry) with that contact's "Name <address>". Returns how many entries
/// resolve and, for the others, `{"name", "candidates"}`.
fn resolve_names(s: &mut Session, cid: u64) -> Result<(usize, Vec<Value>), CmdError> {
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
    Ok((resolved, unresolved))
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
    let norm = voting_choices(&choices).join(";");
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
    let addresses = store_recipients(s, addresses, RecipientInput::List);
    if let Err(bad) = parse_recipients(&addresses) {
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

/// Why Encrypt is unavailable (S/MIME needs certificates, which MailCraft cannot manage yet).
pub const ENCRYPT_UNAVAILABLE: &str = "Encryption is not available yet: MailCraft cannot use S/MIME certificates. \
This message will be sent without encryption; don't include anything that must stay confidential.";

/// Encrypt: S/MIME is not implemented, so the option explains that and stays off (Send keeps working).
/// Turning it off (for a draft saved with it on) works.
fn encrypt(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(m) = opt_str(p, "mode")
        && m != "smime"
    {
        return Err(CmdError::Params(format!("unknown encryption mode `{m}`")));
    }
    let c = compose_mut(s, p)?;
    let want = opt_bool(p, "on").unwrap_or(!c.encrypt);
    if c.encrypt && !want {
        c.encrypt = false;
        c.dirty = true;
    }
    if want {
        s.notify(Notice::Info(ENCRYPT_UNAVAILABLE.into()));
    }
    Ok(json!({"encrypt": false, "available": false, "message": if want { ENCRYPT_UNAVAILABLE } else { "" }}))
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

/// Largest theme file Browse for Themes reads.
const MAX_THEME_FILE: u64 = 64 * 1024;

/// Galleries for the Themes / Colors / Fonts / Effects menus and dialogs.
pub fn theme_galleries() -> Value {
    use crate::compose::{COLOR_GALLERY_LEN, THEME_COLOR_SETS, THEME_EFFECTS, THEME_FONT_SETS, THEMES};
    let colors: Vec<Value> = THEME_COLOR_SETS
        .iter()
        .take(COLOR_GALLERY_LEN)
        .map(|(n, v)| json!({"name": n, "colors": v.iter().map(|c| Color::hex(*c).to_hex()).collect::<Vec<_>>()}))
        .collect();
    let fonts: Vec<Value> =
        THEME_FONT_SETS.iter().map(|(n, h, b)| json!({"name": n, "heading": h, "body": b})).collect();
    let themes: Vec<Value> =
        THEMES.iter().map(|(n, c, f, e)| json!({"name": n, "colors": c, "fonts": f, "effects": e})).collect();
    json!({"themes": themes, "colors": colors, "fonts": fonts, "effects": THEME_EFFECTS})
}

fn theme_state_json(c: &ComposeState) -> Value {
    let r = c.theme_resolved();
    json!({
        "theme": c.theme.name,
        "colors": c.theme.colors,
        "fonts": c.theme.fonts,
        "effects": c.theme.effects,
        "palette": r.colors.iter().map(Color::to_hex).collect::<Vec<_>>(),
        "headingFont": r.heading_font,
        "bodyFont": r.body_font,
        "effectSet": r.effects,
    })
}

/// A theme file (Save Current Theme / Browse for Themes): MailCraft's own JSON format.
#[derive(serde::Serialize, serde::Deserialize)]
struct ThemeFile {
    #[serde(rename = "mailcraftTheme")]
    version: u32,
    name: String,
    colors: Vec<String>,
    #[serde(rename = "headingFont")]
    heading_font: String,
    #[serde(rename = "bodyFont")]
    body_font: String,
    effects: String,
}

fn clean_name(v: &str, fallback: &str) -> String {
    let n: String = v.chars().filter(|c| !c.is_control()).take(64).collect();
    let n = n.trim();
    if n.is_empty() { fallback.to_owned() } else { n.to_owned() }
}

fn font_name(v: &str) -> Option<String> {
    let n: String =
        v.chars().filter(|c| !c.is_control() && !matches!(c, ';' | '{' | '}' | '<' | '>')).take(64).collect();
    let n = n.trim();
    (!n.is_empty()).then(|| n.to_owned())
}

fn palette_param(v: &Value) -> Result<Vec<Color>, CmdError> {
    let arr = v.as_array().ok_or_else(|| CmdError::Params("`colors` must be a list of 12 colours".into()))?;
    if arr.len() != 12 {
        return Err(CmdError::Params("`colors` must hold 12 colours (dark 1, light 1, dark 2, light 2, accents 1-6, hyperlink, followed hyperlink)".into()));
    }
    arr.iter()
        .map(|c| {
            c.as_str()
                .and_then(Color::parse_hex)
                .ok_or_else(|| CmdError::Params(format!("`{c}` is not a colour (#rrggbb)")))
        })
        .collect()
}

fn theme(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("apply");
    let part = opt_str(p, "part").unwrap_or("all");
    if !matches!(part, "all" | "colors" | "fonts" | "effects") {
        return Err(CmdError::Params(format!("unknown theme part `{part}`")));
    }
    let cid = compose_id(s, p)?;
    match action {
        "list" => return Ok(theme_galleries()),
        "browse" if opt_str(p, "path").is_none() => {
            return dialog(s, "browseThemes", json!({"compose": cid}));
        }
        "save" => {
            let Some(path) = opt_str(p, "path").map(str::to_owned) else {
                return dialog(s, "saveTheme", json!({"compose": cid}));
            };
            let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
            let r = c.theme_resolved();
            let file = ThemeFile {
                version: 1,
                name: clean_name(
                    opt_str(p, "name").unwrap_or(""),
                    c.theme.name.as_deref().unwrap_or("Custom Theme"),
                ),
                colors: r.colors.iter().map(Color::to_hex).collect(),
                heading_font: r.heading_font,
                body_font: r.body_font,
                effects: r.effects.to_owned(),
            };
            let text = serde_json::to_string_pretty(&file).map_err(|e| CmdError::Failed(e.to_string()))?;
            std::fs::write(&path, text)
                .map_err(|e| CmdError::Failed(format!("the theme could not be saved: {e}")))?;
            return Ok(json!({"saved": path, "name": file.name}));
        }
        "customize" if p.get("colors").is_none() && p.get("heading").is_none() && p.get("body").is_none() => {
            let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
            let mut st = theme_state_json(c);
            if let Some(o) = st.as_object_mut() {
                o.insert("compose".into(), json!(cid));
                o.insert("part".into(), json!(part));
                o.insert("slots".into(), json!(crate::compose::THEME_COLOR_SLOTS));
            }
            let name = if part == "fonts" { "customizeFonts" } else { "customizeColors" };
            return dialog(s, name, st);
        }
        "apply" | "reset" | "browse" | "customize" => {}
        other => return Err(CmdError::Params(format!("unknown theme action `{other}`"))),
    }
    // Work out the new state first (errors leave the compose untouched).
    let mut next =
        s.composes.get(&cid).map(|c| c.theme.clone()).ok_or_else(|| CmdError::Disabled(NO_COMPOSE.into()))?;
    match action {
        "reset" => next = Default::default(),
        "browse" => {
            let path = opt_str(p, "path").unwrap_or_default();
            let (_, data) = read_file(path, MAX_THEME_FILE)?;
            let file: ThemeFile = serde_json::from_slice(&data)
                .map_err(|e| CmdError::Failed(format!("this is not a MailCraft theme file: {e}")))?;
            let colors = palette_param(&json!(file.colors))?;
            let name = clean_name(&file.name, "Custom Theme");
            let heading = font_name(&file.heading_font).unwrap_or_else(|| "Aptos Display".into());
            let body = font_name(&file.body_font).unwrap_or_else(|| "Aptos".into());
            next.name = Some(name.clone());
            next.colors = Some(name.clone());
            next.fonts = Some(name.clone());
            next.effects = crate::compose::effect_set(&file.effects).map(str::to_owned);
            next.custom_colors = Some(crate::compose::CustomColors { name: name.clone(), colors });
            next.custom_fonts = Some(crate::compose::CustomFonts { name, heading, body });
        }
        "customize" => {
            let name = clean_name(opt_str(p, "name").unwrap_or(""), "Custom 1");
            if part == "fonts" {
                let heading = opt_str(p, "heading").and_then(font_name);
                let body = opt_str(p, "body").and_then(font_name);
                let cur = next.resolve();
                next.custom_fonts = Some(crate::compose::CustomFonts {
                    name: name.clone(),
                    heading: heading.unwrap_or(cur.heading_font),
                    body: body.unwrap_or(cur.body_font),
                });
                next.fonts = Some(name);
            } else {
                let colors = palette_param(p.get("colors").unwrap_or(&Value::Null))?;
                next.custom_colors = Some(crate::compose::CustomColors { name: name.clone(), colors });
                next.colors = Some(name);
            }
        }
        _ => {
            let Some(name) = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()) else {
                return dialog(
                    s,
                    "themes",
                    json!({"compose": cid, "part": part, "galleries": theme_galleries()}),
                );
            };
            let known = match part {
                "colors" => {
                    crate::compose::color_set(name).is_some()
                        || next.custom_colors.as_ref().is_some_and(|c| c.name.eq_ignore_ascii_case(name))
                }
                "fonts" => {
                    crate::compose::font_set(name).is_some()
                        || next.custom_fonts.as_ref().is_some_and(|c| c.name.eq_ignore_ascii_case(name))
                }
                "effects" => crate::compose::effect_set(name).is_some(),
                _ => crate::compose::named_theme(name).is_some(),
            };
            if !known {
                return Err(CmdError::Params(format!(
                    "unknown {} `{name}`",
                    if part == "all" { "theme" } else { part }
                )));
            }
            let name = name.to_owned();
            match part {
                "all" => {
                    next.name = Some(name);
                    next.colors = None;
                    next.fonts = None;
                    next.effects = None;
                }
                "colors" => next.colors = Some(name),
                "fonts" => next.fonts = Some(name),
                _ => next.effects = Some(name),
            }
        }
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let before = c.theme_resolved();
    c.theme = next;
    c.retheme(&before);
    if c.style_set.is_some() {
        c.apply_style_set();
    }
    c.dirty = true;
    Ok(theme_state_json(c))
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
    c.run_edit(run, |c| c.type_text(&text));
    let r = json!({"caret": pos_json(&c.selection.focus)});
    auto_save(s);
    Ok(r)
}

fn delete_text(s: &mut Session, p: &Value) -> CmdResult {
    let dir = opt_str(p, "direction").unwrap_or("backward");
    let c = compose_mut(s, p)?;
    let before = c.selection.focus.clone();
    match dir {
        "backward" => {
            c.run_edit(Some(EditRun::DeleteBackward), ComposeState::delete_backward);
            // Backspace inside a prefaced comment keeps it going.
            c.follow_preface(&before);
        }
        "forward" => c.run_edit(Some(EditRun::DeleteForward), ComposeState::delete_forward),
        "selection" => c.run_edit(None, |c| {
            c.delete_selection();
        }),
        other => return Err(CmdError::Params(format!("unknown direction `{other}`"))),
    }
    let r = json!({"caret": pos_json(&c.selection.focus)});
    auto_save(s);
    Ok(r)
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

// =======================================================================================================
// Tracking: Save Sent Item To, delivery and read receipts, voting buttons
// =======================================================================================================

/// Submission-only header asking for a delivery status notification. The SMTP client removes it and sends
/// RFC 3461 envelope parameters instead (same name as `mailcraft_net::smtp::DSN_HEADER`).
pub const DSN_HEADER: &str = "X-MailCraft-Submit-DSN";
/// What a delivery receipt asks for: success and failure reports, headers only.
pub const DSN_REQUEST: &str = "NOTIFY=SUCCESS,FAILURE; RET=HDRS";
/// Submission-only header carrying Save Sent Item To: `none` or `folder=<id>`.
pub const SAVE_SENT_HEADER: &str = "X-MailCraft-Submit-Save-Sent";
/// Prefix of every submission-only header (never transmitted; see `mailcraft_net::smtp`).
pub const SUBMIT_PREFIX: &str = "X-MailCraft-Submit-";
/// Voting buttons a message offers ("Approve;Reject"). MailCraft's own header: the Exchange voting
/// properties are not used.
pub const VOTING_HEADER: &str = "X-MailCraft-Voting";
/// The choice a voting response carries.
pub const VOTE_HEADER: &str = "X-MailCraft-Vote";
/// Most voting choices kept.
const MAX_VOTING_CHOICES: usize = 20;
/// Longest voting choice (chars).
const MAX_CHOICE_CHARS: usize = 64;
/// Most report / response candidates parsed for one tracking query.
const MAX_REPORT_CANDIDATES: usize = 500;
/// Subject prefix of a read receipt.
const READ_PREFIX: &str = "Read: ";

/// Voting choices from "Approve; Reject;;approve": trimmed, control characters dropped, duplicates
/// (case-insensitive) and empties removed, at most 20 of at most 64 chars.
pub fn voting_choices(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in text.split(';') {
        let c: String = c.chars().filter(|ch| !ch.is_control()).take(MAX_CHOICE_CHARS).collect();
        let c = c.trim().to_owned();
        if !c.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&c)) {
            out.push(c);
        }
        if out.len() >= MAX_VOTING_CHOICES {
            break;
        }
    }
    out
}

/// Raw source of a stored item: kept bytes, else the blob store.
pub(crate) fn stored_raw(s: &Session, m: &MailItem) -> Option<Vec<u8>> {
    if let Some(r) = &m.raw {
        return Some(r.clone());
    }
    s.store.as_ref().and_then(|st| st.raw_of(m).ok().flatten())
}

/// The header section of `raw` (up to, not including, the first empty line).
fn header_block(raw: &[u8]) -> &[u8] {
    let mut line_start = 0usize;
    for (i, &b) in raw.iter().enumerate() {
        if b == b'\n' {
            let line = raw.get(line_start..i).unwrap_or_default();
            if line.is_empty() || line == b"\r" {
                return raw.get(..line_start).unwrap_or_default();
            }
            line_start = i.saturating_add(1);
        }
    }
    raw
}

/// Header fields of `raw` (only the header section is parsed).
pub fn raw_headers(raw: &[u8]) -> mailcraft_mime::Headers {
    let mut head = header_block(raw).to_vec();
    head.extend_from_slice(b"\r\n");
    mailcraft_mime::parse(&head).map(|m| m.headers).unwrap_or_default()
}

/// `raw` without its submission-only `X-MailCraft-Submit-*` header fields (folded lines included).
pub fn strip_submit_headers(raw: &[u8]) -> Vec<u8> {
    let head_len = header_block(raw).len().min(raw.len());
    let (head, body) = raw.split_at(head_len);
    let mut out = Vec::with_capacity(raw.len());
    let mut skipping = false;
    for line in head.split_inclusive(|&b| b == b'\n') {
        let folded = line.first().is_some_and(|&b| b == b' ' || b == b'\t');
        if !folded {
            skipping = line
                .get(..SUBMIT_PREFIX.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(SUBMIT_PREFIX.as_bytes()));
        }
        if !skipping {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(body);
    out
}

/// Files the sent copy of Outbox item `item` the way its Save Sent Item To choice says, once it has been
/// sent. Returns `false` for the default (the caller files it in Sent Items as usual); otherwise the item has
/// been removed (Do Not Save) or moved to the chosen folder without its submission-only headers.
///
/// Called by the send worker's completion (`sync::message_sent`) before it files the copy itself.
pub fn file_sent_copy(s: &mut Session, item: ItemId) -> bool {
    let Some(raw) = s.mailbox.mail.get(&item).and_then(|m| stored_raw(s, m)) else {
        return false;
    };
    let choice = raw_headers(&raw).get(SAVE_SENT_HEADER).map(|v| v.trim().to_ascii_lowercase());
    match choice.as_deref() {
        Some("none") => {
            s.mailbox.mail.remove(&item);
            true
        }
        Some(v) => {
            let folder = v
                .strip_prefix("folder=")
                .and_then(|n| n.trim().parse::<u64>().ok())
                .map(mailcraft_model::FolderId)
                .filter(|f| s.mailbox.folder(*f).is_some_and(|f| f.kind == FolderKind::Mail));
            let Some(folder) = folder else {
                return false;
            };
            let Some(m) = s.mailbox.mail.get_mut(&item) else {
                return false;
            };
            m.folder = folder;
            m.read = true;
            m.remote_id = None;
            m.raw = Some(strip_submit_headers(&raw));
            m.size = m.raw.as_ref().map_or(m.size, |r| u64::try_from(r.len()).unwrap_or(u64::MAX));
            true
        }
        None => false,
    }
}

/// One voting response received for a message the user sent.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct VoteResponse {
    pub item: u64,
    pub from: String,
    pub choice: String,
    pub when: DateTime,
}

/// Delivery / read receipt information for one recipient of a sent message.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ReceiptRow {
    pub recipient: String,
    /// "Delivered", "Failed", "Delayed" (DSN Action), when a report came back.
    pub delivery: Option<String>,
    pub delivered_at: Option<DateTime>,
    /// "Read" / "Deleted" (MDN disposition), when a read receipt came back.
    pub read: Option<String>,
    pub read_at: Option<DateTime>,
}

/// Tracking state of one message (reading-pane info bar, Tracking page).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Tracking {
    /// Voting choices the message offers.
    pub choices: Vec<String>,
    /// The user sent this message (Sent Items / Outbox, or From is one of the user's addresses).
    pub sent_by_me: bool,
    /// For a received voting message: the choice already sent and when.
    pub my_vote: Option<(String, DateTime)>,
    /// For a sent voting message: the responses received, oldest first (latest per responder).
    pub responses: Vec<VoteResponse>,
    /// Count per choice, in choice order.
    pub tally: Vec<(String, usize)>,
    /// For a sent message: delivery and read receipts that came back.
    pub receipts: Vec<ReceiptRow>,
    /// For a received message: the address asking for a read receipt.
    pub read_receipt_to: Option<String>,
    /// A read receipt for it has already been sent.
    pub read_receipt_sent: bool,
    /// The receipt may not be sent without asking, even when the user chose to always send them (RFC 8098
    /// section 2.1): the message has no Return-Path, its Return-Path differs from the address asking, or
    /// several addresses ask.
    pub read_receipt_ask: bool,
}

impl Tracking {
    /// Text of the voting info bar (`None` when the message has no voting buttons).
    pub fn voting_text(&self, tz_offset_min: i32) -> Option<String> {
        if self.choices.is_empty() {
            return None;
        }
        if self.sent_by_me {
            let totals: Vec<String> = self.tally.iter().map(|(c, n)| format!("{c} {n}")).collect();
            return Some(format!("Reply totals: {}", totals.join("; ")));
        }
        Some(match &self.my_vote {
            Some((choice, when)) => {
                let c = when.civil(tz_offset_min);
                format!(
                    "You responded {choice} on {:04}-{:02}-{:02} {:02}:{:02}.",
                    c.year, c.month, c.day, c.hour, c.minute
                )
            }
            None => "This message has voting buttons. Choose a response.".to_owned(),
        })
    }
}

fn is_mine(s: &Session, addr: &str) -> bool {
    let a = addr.trim().to_ascii_lowercase();
    !a.is_empty()
        && (s.me.contains(&a) || s.mailbox.accounts.iter().any(|x| x.address.eq_ignore_ascii_case(&a)))
}

fn item_sent_by_me(s: &Session, m: &MailItem) -> bool {
    let role = s.mailbox.folder(m.folder).and_then(|f| f.role);
    matches!(role, Some(FolderRole::SentItems | FolderRole::Outbox))
        || m.from.as_ref().is_some_and(|f| is_mine(s, &f.addr))
}

/// Field groups of a `message/delivery-status` or `message/disposition-notification` body (RFC 3464 /
/// RFC 8098): blank-line separated blocks of `Name: value` fields, folded lines joined.
fn report_groups(text: &str) -> Vec<Vec<(String, String)>> {
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
    let mut cur: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = cur.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            cur.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
        if cur.len() > 200 {
            break;
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
}

fn field<'a>(g: &'a [(String, String)], name: &str) -> Option<&'a str> {
    g.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// `rfc822;a@example.com` → `a@example.com`.
fn report_address(v: &str) -> String {
    let addr = v.split_once(';').map_or(v, |(_, a)| a).trim();
    addr.trim_start_matches('<').trim_end_matches('>').to_owned()
}

fn bare_id(v: &str) -> String {
    v.trim().trim_start_matches('<').trim_end_matches('>').trim().to_owned()
}

/// Delivery / read receipts for sent message `mid` found in `report` (a parsed report message).
fn read_report(msg: &mailcraft_mime::Message, mid: &str, when: DateTime, rows: &mut Vec<ReceiptRow>) {
    let envid = crate::cmd::compose::xtext(mid);
    let refers = msg.in_reply_to().iter().chain(msg.references().iter()).any(|r| bare_id(r) == mid);
    let parts = msg.parts();
    // Headers of the original returned with the report (RET=HDRS / message/rfc822).
    let returned_id = parts.iter().find_map(|p| match (&p.body, p.mime_type()) {
        (mailcraft_mime::PartBody::Message(m), _) => m.message_id().map(|i| bare_id(&i)),
        (_, "text/rfc822-headers") => {
            p.text().and_then(|t| raw_headers(t.as_bytes()).get("Message-ID").map(bare_id))
        }
        _ => None,
    });
    for p in &parts {
        let kind = p.mime_type().to_owned();
        if kind != "message/delivery-status" && kind != "message/disposition-notification" {
            continue;
        }
        let Some(text) = p.text() else { continue };
        let groups = report_groups(&text);
        let Some(first) = groups.first() else { continue };
        let matches = refers
            || returned_id.as_deref() == Some(mid)
            || field(first, "original-envelope-id").is_some_and(|e| e.trim() == envid || bare_id(e) == mid)
            || field(first, "original-message-id").is_some_and(|e| bare_id(e) == mid);
        if !matches {
            continue;
        }
        let mut upsert = |recipient: String, f: &mut dyn FnMut(&mut ReceiptRow)| {
            if recipient.is_empty() {
                return;
            }
            match rows.iter_mut().find(|r| r.recipient.eq_ignore_ascii_case(&recipient)) {
                Some(r) => f(r),
                None => {
                    let mut r = ReceiptRow { recipient, ..Default::default() };
                    f(&mut r);
                    rows.push(r);
                }
            }
        };
        if kind == "message/delivery-status" {
            for g in groups.iter().skip(1) {
                let rcpt = field(g, "original-recipient").or_else(|| field(g, "final-recipient"));
                let action = field(g, "action").unwrap_or("").trim().to_ascii_lowercase();
                let label = match action.as_str() {
                    "delivered" | "relayed" | "expanded" => "Delivered",
                    "failed" => "Failed",
                    "delayed" => "Delayed",
                    _ => continue,
                };
                upsert(report_address(rcpt.unwrap_or("")), &mut |r: &mut ReceiptRow| {
                    // A later "Delayed" does not hide a delivery or failure already reported.
                    if label != "Delayed" || r.delivery.is_none() {
                        r.delivery = Some(label.to_owned());
                        r.delivered_at = Some(when);
                    }
                });
            }
        } else {
            let rcpt = field(first, "original-recipient").or_else(|| field(first, "final-recipient"));
            let disp = field(first, "disposition").unwrap_or("").to_ascii_lowercase();
            let label = if disp.contains("deleted") { "Deleted" } else { "Read" };
            upsert(report_address(rcpt.unwrap_or("")), &mut |r: &mut ReceiptRow| {
                r.read = Some(label.to_owned());
                r.read_at = Some(when);
            });
        }
    }
}

/// The voting choice a response carries: its vote header, else a subject that starts with a choice and a
/// colon ("Approve: Budget").
fn vote_of(s: &Session, m: &MailItem, choices: &[String]) -> Option<String> {
    let from_header = stored_raw(s, m)
        .and_then(|raw| raw_headers(&raw).text(VOTE_HEADER))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let pick = |v: &str| choices.iter().find(|c| c.eq_ignore_ascii_case(v.trim())).cloned();
    if let Some(v) = from_header {
        return pick(&v).or(Some(v));
    }
    let (head, _) = m.subject.split_once(':')?;
    pick(head)
}

/// Records the voting response `response` on the message it answers (a message I sent with voting buttons),
/// so the tally keeps the vote after the response is deleted and Deleted Items is emptied: one record per
/// responder, the latest kept. Returns the original's id when the response was recorded.
pub(crate) fn record_vote(s: &mut Session, response: ItemId) -> Option<ItemId> {
    let x = s.mailbox.mail.get(&response)?;
    if item_sent_by_me(s, x) {
        return None;
    }
    let parents: Vec<String> = x.in_reply_to.iter().map(|r| bare_id(r)).filter(|r| !r.is_empty()).collect();
    if parents.is_empty() {
        return None;
    }
    let original = s
        .mailbox
        .mail
        .values()
        .filter(|m| m.id != response)
        .find(|m| m.message_id.as_deref().map(bare_id).is_some_and(|mid| parents.contains(&mid)))
        .filter(|m| item_sent_by_me(s, m))?;
    let choices = stored_raw(s, original)
        .and_then(|raw| raw_headers(&raw).text(VOTING_HEADER))
        .map(|v| voting_choices(&v))
        .unwrap_or_default();
    if choices.is_empty() {
        return None;
    }
    let choice = vote_of(s, x, &choices)?;
    let from = x.from.as_ref().map(|f| f.addr.trim().to_owned()).unwrap_or_default();
    let when = x.sent.unwrap_or(x.received);
    let id = original.id;
    let record = mailcraft_model::VoteRecord { from, choice, when, response: Some(response) };
    let o = s.mailbox.mail.get_mut(&id)?;
    let same = |v: &mailcraft_model::VoteRecord| {
        v.response == Some(response) || (!record.from.is_empty() && v.from.eq_ignore_ascii_case(&record.from))
    };
    match o.votes.iter_mut().find(|v| same(v)) {
        // An older response from the same person gives way; a newer one already recorded stays.
        Some(old) if old.when <= record.when => *old = record,
        Some(_) => {}
        None => {
            if o.votes.len() >= mailcraft_model::item::MAX_VOTE_RECORDS
                && let Some(oldest) = o.votes.iter().enumerate().min_by_key(|(_, v)| v.when).map(|(i, _)| i)
            {
                o.votes.remove(oldest);
            }
            o.votes.push(record);
        }
    }
    Some(id)
}

/// Tracking state of message `id` (voting buttons and responses, receipts, a pending read-receipt request).
pub fn tracking(s: &Session, id: ItemId) -> Option<Tracking> {
    let m = s.mailbox.mail.get(&id)?;
    let headers = stored_raw(s, m).map(|r| raw_headers(&r)).unwrap_or_default();
    let mut t = Tracking {
        choices: headers.text(VOTING_HEADER).map(|v| voting_choices(&v)).unwrap_or_default(),
        sent_by_me: item_sent_by_me(s, m),
        ..Default::default()
    };
    let mid = m.message_id.as_deref().map(bare_id).filter(|i| !i.is_empty());
    // Reports (read receipts, delivery notifications) are never answered with a read receipt.
    let is_report = headers
        .get("Content-Type")
        .is_some_and(|c| c.trim().to_ascii_lowercase().starts_with("multipart/report"));
    if !t.sent_by_me
        && !is_report
        && let Some(to) = headers.text("Disposition-Notification-To").filter(|v| !v.trim().is_empty())
    {
        t.read_receipt_ask = receipt_needs_asking(&to, headers.text("Return-Path").as_deref());
        t.read_receipt_to = Some(to.trim().to_owned());
    }
    let Some(mid) = mid else {
        t.tally = t.choices.iter().map(|c| (c.clone(), 0)).collect();
        return Some(t);
    };
    let since = m.sent.unwrap_or(m.received).unix.saturating_sub(86_400);
    let subject = m.subject.trim().to_lowercase();
    let mut candidates: Vec<&MailItem> = s
        .mailbox
        .mail
        .values()
        .filter(|x| x.id != id && x.received.unix >= since)
        .filter(|x| {
            let replies = x.in_reply_to.iter().chain(x.references.iter()).any(|r| bare_id(r) == mid);
            let from = x.from.as_ref().map(|f| f.addr.to_ascii_lowercase()).unwrap_or_default();
            let daemon = from.starts_with("mailer-daemon") || from.starts_with("postmaster");
            replies || daemon || (!subject.is_empty() && x.subject.to_lowercase().contains(&subject))
        })
        .collect();
    candidates.sort_by_key(|x| (x.received.unix, x.id));
    candidates.truncate(MAX_REPORT_CANDIDATES);
    // Options › Mail › Tracking › "Automatically update original sent item with receipt information": off,
    // a sent item shows no receipts or vote responses (the replies themselves stay in the Inbox).
    let update_sent = s.settings.bool("mail.tracking.updateSentItem");
    for x in candidates {
        let replies = x.in_reply_to.iter().any(|r| bare_id(r) == mid);
        let mine = item_sent_by_me(s, x);
        if replies
            && !t.choices.is_empty()
            && let Some(choice) = vote_of(s, x, &t.choices)
        {
            let when = x.sent.unwrap_or(x.received);
            if mine && !t.sent_by_me {
                t.my_vote = Some((choice, when));
                continue;
            }
            if t.sent_by_me && !mine {
                if !update_sent {
                    continue;
                }
                let from = x.from.as_ref().map(|f| f.addr.clone()).unwrap_or_default();
                t.responses.retain(|r| !r.from.eq_ignore_ascii_case(&from) || from.is_empty());
                t.responses.push(VoteResponse { item: x.id.0, from, choice, when });
                continue;
            }
        }
        if replies && mine && x.subject.starts_with(READ_PREFIX.trim_end()) {
            t.read_receipt_sent = true;
        }
        if t.sent_by_me
            && !mine
            && update_sent
            && let Some(raw) = stored_raw(s, x)
            && raw_headers(&raw)
                .get("Content-Type")
                .is_some_and(|c| c.to_ascii_lowercase().contains("report"))
            && let Ok(msg) = mailcraft_mime::parse(&raw)
        {
            read_report(&msg, &mid, x.received, &mut t.receipts);
        }
    }
    // Votes recorded on the message as they arrived count too, also once their responses are gone for good
    // (a response still in the mailbox and its record are one vote).
    if t.sent_by_me && update_sent && !t.choices.is_empty() {
        for v in m.votes.iter().take(mailcraft_model::item::MAX_VOTE_RECORDS) {
            let response = v.response.map_or(0, |i| i.0);
            let live = t.responses.iter_mut().find(|r| {
                (response != 0 && r.item == response)
                    || (!v.from.is_empty() && r.from.eq_ignore_ascii_case(&v.from))
            });
            let choice = t.choices.iter().find(|c| c.eq_ignore_ascii_case(&v.choice)).cloned();
            let Some(choice) = choice else { continue };
            let stored = VoteResponse { item: response, from: v.from.clone(), choice, when: v.when };
            match live {
                Some(r) if r.when < v.when => *r = stored,
                Some(_) => {}
                None => t.responses.push(stored),
            }
        }
        t.responses.sort_by_key(|r| (r.when, r.item));
    }
    t.tally = t
        .choices
        .iter()
        .map(|c| (c.clone(), t.responses.iter().filter(|r| r.choice.eq_ignore_ascii_case(c)).count()))
        .collect();
    Some(t)
}

/// RFC 8098 section 2.1: a receipt is not sent automatically when the message has no Return-Path, when the
/// Return-Path differs from the address in `Disposition-Notification-To`, or when that names more than one
/// distinct address.
fn receipt_needs_asking(notify_to: &str, return_path: Option<&str>) -> bool {
    let mut asking: Vec<String> = parse_address_list(notify_to)
        .iter()
        .flat_map(mailcraft_mime::Address::mailboxes)
        .map(|m| m.addr.trim().to_ascii_lowercase())
        .collect();
    asking.sort();
    asking.dedup();
    let [only] = asking.as_slice() else { return true };
    let Some(rp) = return_path else { return true };
    let rp = rp.trim().trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase();
    rp.is_empty() || rp != *only
}

/// RFC 3461 xtext (as the SMTP client writes ENVID), used to match `Original-Envelope-Id`.
pub(crate) fn xtext(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (33..=126).contains(&b) && b != b'+' && b != b'=' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("+{b:02X}"));
        }
    }
    out
}

/// The message a tracking command acts on: `params.id`, else the selected message.
fn tracked_item(s: &Session, p: &Value) -> Result<ItemId, CmdError> {
    if p.get("id").is_some() {
        return crate::cmd::util::req_mail(s, p);
    }
    crate::cmd::util::mail_targets(s, p)
        .first()
        .copied()
        .ok_or_else(|| CmdError::Disabled("select a message first".into()))
}

fn tracking_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let id = tracked_item(s, p)?;
    let t = tracking(s, id).ok_or_else(|| CmdError::Params(format!("no message with id {}", id.0)))?;
    let text = t.voting_text(s.tz_offset_min);
    let mut v = serde_json::to_value(&t).map_err(|e| CmdError::Failed(e.to_string()))?;
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), json!(id.0));
        o.insert("votingText".into(), json!(text));
    }
    Ok(v)
}

/// Queues `c` (not shown in a window) and sends it right away.
fn send_hidden(s: &mut Session, mut c: ComposeState) -> CmdResult {
    s.next_compose = s.next_compose.saturating_add(1);
    c.id = s.next_compose;
    let cid = c.id;
    s.composes.insert(cid, c);
    // Nothing to ask about in a message the user does not see (no name check or spelling prompt).
    let r = send(s, &json!({"compose": cid, "checkNames": false, "spellCheck": false}));
    // A refused send leaves no hidden compose behind.
    s.composes.remove(&cid);
    r
}

/// Vote: answers a voting message with one of its choices. `edit: true` opens the response for editing
/// (quoted original); otherwise it is sent right away with an empty body.
fn vote(s: &mut Session, p: &Value) -> CmdResult {
    let id = tracked_item(s, p)?;
    let choice = req_str(p, "choice")?.trim().to_owned();
    let t = tracking(s, id).ok_or_else(|| CmdError::Params(format!("no message with id {}", id.0)))?;
    if t.choices.is_empty() {
        return Err(CmdError::Failed("this message has no voting buttons".into()));
    }
    let Some(choice) = t.choices.iter().find(|c| c.eq_ignore_ascii_case(&choice)).cloned() else {
        return Err(CmdError::Params(format!(
            "`{choice}` is not one of the choices: {}",
            t.choices.join("; ")
        )));
    };
    if t.sent_by_me {
        return Err(CmdError::Failed("you cannot vote on a message you sent".into()));
    }
    let Some(mut c) = crate::cmd::respond::build_response(s, id, ComposeKind::Reply, false) else {
        return Err(CmdError::Params(format!("no message with id {}", id.0)));
    };
    let original = s.mailbox.mail.get(&id).map(|m| m.subject.clone()).unwrap_or_default();
    c.subject = format!("{choice}: {}", mailcraft_model::subject::strip_prefixes(&original));
    c.vote_response = Some(choice.clone());
    if opt_bool(p, "edit") == Some(true) {
        let cid = s.open_compose(c);
        return Ok(json!({"vote": choice, "compose": cid}));
    }
    c.body = Doc::default();
    c.inline_images.clear();
    // An original attached by "Attach original message" does not go with a vote either.
    c.attachments.clear();
    c.ensure_body();
    let r = send_hidden(s, c)?;
    Ok(json!({"vote": choice, "sent": r.get("sent").cloned().unwrap_or(json!(false)), "item": r.get("item")}))
}

/// An RFC 8098 message disposition notification ("displayed") for `m`, from `from` to `to`.
fn mdn_message(
    s: &Session,
    m: &MailItem,
    from: &mailcraft_mime::Mailbox,
    to: &str,
    now: DateTime,
    automatic: bool,
) -> Vec<u8> {
    use base64::Engine as _;
    let mid = m.message_id.as_deref().map(bare_id).unwrap_or_default();
    let mut b = MessageBuilder::new();
    b.from = Some(from.clone());
    b.subject = format!("{READ_PREFIX}{}", m.subject);
    let id = b.generate_message_id(now, now.unix.unsigned_abs());
    let to_list = parse_address_list(to);
    let sent = m.sent.unwrap_or(m.received).civil(s.tz_offset_min);
    let read = now.civil(s.tz_offset_min);
    let recipients: Vec<String> = m.to.iter().map(|x| x.addr.clone()).collect();
    let text = format!(
        "Your message\r\n\r\n  To: {}\r\n  Subject: {}\r\n  Sent: {:04}-{:02}-{:02} {:02}:{:02}\r\n\r\n\
was read on {:04}-{:02}-{:02} {:02}:{:02}.\r\n",
        recipients.join(", "),
        m.subject,
        sent.year,
        sent.month,
        sent.day,
        sent.hour,
        sent.minute,
        read.year,
        read.month,
        read.day,
        read.hour,
        read.minute
    );
    let boundary = format!("=_mdn_{:x}", now.unix.unsigned_abs() ^ 0x5bd1_e995);
    let mut out = String::new();
    let mut header = |k: &str, v: &str| {
        out.push_str(&mailcraft_mime::header::fold(k, v));
        out.push_str("\r\n");
    };
    header("From", &mailcraft_mime::addr::format_mailbox(from, true));
    header("To", &mailcraft_mime::format_address_list(&to_list, true));
    header("Subject", &mailcraft_mime::encoded::encode_words(&b.subject));
    header("Date", &now.to_rfc5322());
    header("Message-ID", &format!("<{id}>"));
    if !mid.is_empty() {
        header("In-Reply-To", &format!("<{mid}>"));
        header("References", &format!("<{mid}>"));
    }
    header("MIME-Version", "1.0");
    header(
        "Content-Type",
        &format!("multipart/report; report-type=disposition-notification; boundary=\"{boundary}\""),
    );
    out.push_str("\r\n");
    out.push_str(&format!(
        "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
    ));
    let enc = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    for chunk in enc.as_bytes().chunks(76) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\r\n");
    }
    let me = &from.addr;
    out.push_str(&format!("--{boundary}\r\nContent-Type: message/disposition-notification\r\n\r\n"));
    out.push_str("Reporting-UA: MailCraft\r\n");
    if me.is_ascii() {
        out.push_str(&format!("Final-Recipient: rfc822;{me}\r\n"));
    } else {
        out.push_str(&format!("Final-Recipient: utf-8;{me}\r\n"));
    }
    if !mid.is_empty() {
        out.push_str(&format!("Original-Message-ID: <{mid}>\r\n"));
    }
    // RFC 8098 section 3.2.6.1: who decided to send it.
    out.push_str(if automatic {
        "Disposition: automatic-action/MDN-sent-automatically; displayed\r\n"
    } else {
        "Disposition: manual-action/MDN-sent-manually; displayed\r\n"
    });
    out.push_str(&format!("\r\n--{boundary}--\r\n"));
    out.into_bytes()
}

/// Answers a read-receipt request on a received message: `send: true` sends the receipt (RFC 8098),
/// `send: false` declines (nothing is sent). `automatic: true` is the "Always send a read receipt" option
/// answering without the user: refused with `{"sent": false, "ask": true}` when the request must be confirmed
/// by the user (RFC 8098 section 2.1; see [`Tracking::read_receipt_ask`]).
fn send_read_receipt(s: &mut Session, p: &Value) -> CmdResult {
    let id = tracked_item(s, p)?;
    let t = tracking(s, id).ok_or_else(|| CmdError::Params(format!("no message with id {}", id.0)))?;
    let Some(to) = t.read_receipt_to.clone() else {
        return Err(CmdError::Failed("the sender did not ask for a read receipt".into()));
    };
    if opt_bool(p, "send") == Some(false) {
        return Ok(json!({"sent": false, "declined": true}));
    }
    if t.read_receipt_sent {
        return Ok(json!({"sent": false, "alreadySent": true}));
    }
    let automatic = opt_bool(p, "automatic") == Some(true);
    if automatic && t.read_receipt_ask {
        return Ok(json!({"sent": false, "ask": true}));
    }
    // The header is an RFC 5322 address list.
    if parse_recipients(&store_recipients(s, &to, RecipientInput::List)).map(|v| v.is_empty()).unwrap_or(true)
    {
        return Err(CmdError::Failed(format!("the read receipt address is not valid: {to}")));
    }
    let account = crate::cmd::mail::item_account(s, id)
        .and_then(|a| s.mailbox.account(a))
        .filter(|a| a.can_send())
        .or_else(|| s.mailbox.default_account().filter(|a| a.can_send()))
        .cloned();
    let Some(account) = account else {
        return Err(CmdError::Failed(NO_ACCOUNT.into()));
    };
    let Some(outbox) = s.mailbox.role_folder(account.id, FolderRole::Outbox) else {
        return Err(CmdError::Failed(format!("The account {} has no Outbox folder.", account.address)));
    };
    let Some(m) = s.mailbox.mail.get(&id).cloned() else {
        return Err(CmdError::Params(format!("no message with id {}", id.0)));
    };
    let name = if account.sender_name.trim().is_empty() {
        account.display_name.trim()
    } else {
        account.sender_name.trim()
    };
    let from = mailcraft_mime::Mailbox::new(Some(name), account.address.trim());
    let now = s.now();
    let raw = mdn_message(s, &m, &from, &to, now, automatic);
    let mut item = match mailcraft_mime::parse(&raw) {
        Ok(msg) => MailItem::from_message(&msg, &raw, now),
        Err(e) => return Err(CmdError::Failed(format!("the read receipt could not be built: {e}"))),
    };
    item.folder = outbox;
    item.read = true;
    item.received = now;
    item.sent = Some(now);
    let rid = s.mailbox.add_mail(item);
    crate::sync::send_now(s, account.id);
    Ok(json!({"sent": true, "item": rid.0, "to": to}))
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
            "compose.vote",
            "Vote",
            "Reading Pane › Voting info bar",
            "",
            r#"{"id"?:number,"choice":string,"edit"?:bool}"#,
            true,
            crate::always,
            vote,
        ),
        spec(
            "compose.sendReadReceipt",
            "Send a Read Receipt",
            "Reading Pane › Read receipt request",
            "",
            r#"{"id"?:number,"send"?:bool,"automatic"?:bool}"#,
            true,
            crate::always,
            send_read_receipt,
        ),
        spec(
            "compose.tracking",
            "Tracking",
            "Read Message › Show › Tracking",
            "",
            r#"{"id"?:number}"#,
            false,
            crate::always,
            tracking_cmd,
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
        // Options › Mail › Save messages: the compose window's auto-save timer calls this (cheap when nothing
        // is due); it journals the drafts it saves itself, so it records no undo step.
        spec(
            "compose.autoSaveTick",
            "AutoSave",
            "Compose › (timer)",
            "",
            "",
            false,
            crate::always,
            auto_save_tick,
        ),
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
            r#"{"value":string,"typed"?:bool (typed in the window: commas separate only with Options › Mail › "Commas can be used to separate multiple message recipients"; else an RFC 5322 list)}"#,
            false,
            has_compose,
            set_to,
        ),
        spec(
            "compose.setCc",
            "Cc",
            "Compose › Header",
            "",
            r#"{"value":string,"typed"?:bool}"#,
            false,
            has_compose,
            set_cc,
        ),
        spec(
            "compose.setBcc",
            "Bcc",
            "Compose › Header",
            "",
            r#"{"value":string,"typed"?:bool}"#,
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
        let r = s.run("compose.encrypt", &json!({})).unwrap();
        assert_eq!(r["encrypt"], false);
        assert!(s.notices.contains(&Notice::Info(ENCRYPT_UNAVAILABLE.into())));
        s.run("compose.votingButtons", &json!({"choices": "Yes;No"})).unwrap();
        s.run("compose.directRepliesTo", &json!({"addresses": "r@example.com"})).unwrap();
        assert!(s.run("compose.directRepliesTo", &json!({"addresses": "nope"})).is_err());
        s.run("compose.delayDelivery", &json!({"at": "2026-10-10T08:00"})).unwrap();
        s.run("compose.saveSentItemTo", &json!({"mode": "none"})).unwrap();
        s.run("compose.followUp", &json!({"kind": "tomorrow"})).unwrap();
        s.run("compose.sensitivity", &json!({"level": "private"})).unwrap();
        let c = &s.composes[&id];
        assert!(c.show_bcc && c.show_from && c.request_delivery_receipt && !c.encrypt);
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
        // Encrypt stays off, so Send works; a draft saved with Encrypt on is refused until it is turned off.
        s.run("compose.setTo", &json!({"value": "x@example.com"})).unwrap();
        s.composes.get_mut(&id).unwrap().encrypt = true;
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], false);
        s.run("compose.encrypt", &json!({"on": false})).unwrap();
        assert!(!s.composes[&id].encrypt);
        assert_eq!(s.run("compose.messageOptions", &json!({})).unwrap()["dialog"], "messageOptions");
        assert_eq!(s.run("compose.addressBook", &json!({})).unwrap()["dialog"], "addressBook");
    }

    fn outbox_raw(s: &Session) -> String {
        let m = folder_items(s, FolderRole::Outbox).into_iter().max_by_key(|m| m.id).unwrap();
        String::from_utf8_lossy(m.raw.as_ref().unwrap()).into_owned()
    }

    fn folder_of(s: &Session, role: FolderRole) -> mailcraft_model::FolderId {
        let acc = s.mailbox.accounts.iter().find(|a| a.kind == AccountKind::Imap).unwrap().id;
        s.mailbox.role_folder(acc, role).unwrap()
    }

    #[test]
    fn delivery_receipt_is_a_dsn_request_and_save_sent_is_honoured() {
        let mut s = session();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.deliveryReceipt", &json!({"on": true})).unwrap();
        s.run("compose.saveSentItemTo", &json!({"mode": "none"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
        let raw = outbox_raw(&s);
        assert!(raw.contains("X-MailCraft-Submit-DSN: NOTIFY=SUCCESS,FAILURE; RET=HDRS"), "{raw}");
        assert!(!raw.contains("Return-Receipt-To"));
        assert!(raw.contains("X-MailCraft-Submit-Save-Sent: none"));
        // After sending, Do Not Save drops the copy.
        let id = folder_items(&s, FolderRole::Outbox)[0].id;
        assert!(file_sent_copy(&mut s, id));
        assert!(!s.mailbox.mail.contains_key(&id));

        // Other Folder: the copy moves there without the submission-only headers.
        let inbox = folder_of(&s, FolderRole::Inbox);
        let archive = s.mailbox.create_folder(inbox, "Kept").unwrap();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.saveSentItemTo", &json!({"mode": "other", "folder": archive.0})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        assert!(outbox_raw(&s).contains(&format!("X-MailCraft-Submit-Save-Sent: folder={}", archive.0)));
        let id = folder_items(&s, FolderRole::Outbox)[0].id;
        assert!(file_sent_copy(&mut s, id));
        let m = &s.mailbox.mail[&id];
        assert_eq!(m.folder, archive);
        assert!(!String::from_utf8_lossy(m.raw.as_ref().unwrap()).contains("X-MailCraft-Submit"));

        // Default: the caller files it in Sent Items.
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let id = folder_items(&s, FolderRole::Outbox)[0].id;
        assert!(!outbox_raw(&s).contains("X-MailCraft-Submit"));
        assert!(!file_sent_copy(&mut s, id));
    }

    #[test]
    fn submit_headers_strip_with_folded_lines() {
        let raw = b"From: a@example.com\r\nX-MailCraft-Submit-DSN: NOTIFY=SUCCESS,\r\n FAILURE\r\nSubject: x\r\n\r\nX-MailCraft-Submit-DSN: body\r\n";
        assert_eq!(
            strip_submit_headers(raw),
            b"From: a@example.com\r\nSubject: x\r\n\r\nX-MailCraft-Submit-DSN: body\r\n".to_vec()
        );
        assert_eq!(DSN_HEADER, "X-MailCraft-Submit-DSN");
        assert_eq!(voting_choices(" Approve; Reject;;approve ;x\u{0}y"), vec!["Approve", "Reject", "xy"]);
        assert_eq!(xtext("a+b c"), "a+2Bb+20c");
    }

    /// A received message (raw) in the IMAP account's Inbox.
    fn receive(s: &mut Session, raw: &str) -> ItemId {
        let inbox = folder_of(s, FolderRole::Inbox);
        let raw = raw.replace('\n', "\r\n").into_bytes();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, s.now());
        item.folder = inbox;
        s.mailbox.add_mail(item)
    }

    #[test]
    fn voting_buttons_vote_and_tally() {
        // The sender's side: the choices travel in the voting header.
        let mut s = session();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "ann@example.org"})).unwrap();
        s.run("compose.setSubject", &json!({"value": "Lunch"})).unwrap();
        s.run("compose.votingButtons", &json!({"choices": "Approve;Reject"})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        assert!(outbox_raw(&s).contains("X-MailCraft-Voting: Approve;Reject"));
        let sent = folder_items(&s, FolderRole::Outbox)[0].id;
        let mid = s.mailbox.mail[&sent].message_id.clone().unwrap();

        // The recipient votes.
        let mut r = session();
        let got = receive(
            &mut r,
            &format!(
                "From: Me <me@example.net>\nTo: me@example.com\nSubject: Lunch\nMessage-ID: <{mid}>\nX-MailCraft-Voting: Approve;Reject\n\nWhere?\n"
            ),
        );
        r.selection.items = vec![got];
        r.selection.focus = Some(got);
        let t = r.run("compose.tracking", &json!({})).unwrap();
        assert_eq!(t["choices"], json!(["Approve", "Reject"]));
        assert!(t["votingText"].as_str().unwrap().contains("voting buttons"));
        assert!(r.run("compose.vote", &json!({"choice": "Maybe"})).is_err());
        let v = r.run("compose.vote", &json!({"choice": "approve"})).unwrap();
        assert_eq!(v["vote"], "Approve");
        assert_eq!(v["sent"], true);
        assert!(r.composes.is_empty(), "no compose window is left open");
        let raw = outbox_raw(&r);
        assert!(
            raw.contains("X-MailCraft-Vote: Approve") && raw.contains("Subject: Approve: Lunch"),
            "{raw}"
        );
        assert!(raw.contains(&format!("In-Reply-To: <{mid}>")));
        let t = tracking(&r, got).unwrap();
        assert_eq!(t.my_vote.as_ref().map(|v| v.0.as_str()), Some("Approve"));
        assert!(t.voting_text(0).unwrap().starts_with("You responded Approve"));
        // Edit the response before sending: a compose opens with the vote attached.
        let v = r.run("compose.vote", &json!({"id": got.0, "choice": "Reject", "edit": true})).unwrap();
        let cid = v["compose"].as_u64().unwrap();
        assert_eq!(r.composes[&cid].vote_response.as_deref(), Some("Reject"));
        assert_eq!(r.composes[&cid].subject, "Reject: Lunch");

        // Back on the sender's side the responses are tallied (latest per responder).
        let vote_raw = raw.replace("\r\n", "\n");
        receive(
            &mut s,
            &vote_raw.replace("From: Me Example <me@example.com>", "From: Ann <ann@example.org>"),
        );
        receive(
            &mut s,
            &format!(
                "From: Bo <bo@example.org>\nSubject: Reject: Lunch\nIn-Reply-To: <{mid}>\nMessage-ID: <b1@example.org>\n\nno\n"
            ),
        );
        let t = tracking(&s, sent).unwrap();
        assert!(t.sent_by_me);
        assert_eq!(t.tally, vec![("Approve".to_owned(), 1), ("Reject".to_owned(), 1)], "{t:?}");
        assert_eq!(t.voting_text(0).unwrap(), "Reply totals: Approve 1; Reject 1");
        assert!(s.run("compose.vote", &json!({"id": sent.0, "choice": "Approve"})).is_err(), "own message");
    }

    #[test]
    fn read_receipt_requests_are_answered_with_an_mdn() {
        let mut s = session();
        let got = receive(
            &mut s,
            "From: Ann <ann@example.org>\nTo: me@example.com\nSubject: Report\nMessage-ID: <r1@example.org>\nDisposition-Notification-To: Ann <ann@example.org>\n\nPlease confirm\n",
        );
        let t = tracking(&s, got).unwrap();
        assert_eq!(t.read_receipt_to.as_deref(), Some("Ann <ann@example.org>"));
        assert!(!t.read_receipt_sent);
        assert_eq!(
            s.run("compose.sendReadReceipt", &json!({"id": got.0, "send": false})).unwrap()["declined"],
            true
        );
        assert!(folder_items(&s, FolderRole::Outbox).is_empty());
        let r = s.run("compose.sendReadReceipt", &json!({"id": got.0})).unwrap();
        assert_eq!(r["sent"], true);
        let raw = outbox_raw(&s);
        assert!(raw.contains("multipart/report; report-type=disposition-notification"), "{raw}");
        assert!(raw.contains("Content-Type: message/disposition-notification"));
        assert!(raw.contains("Original-Message-ID: <r1@example.org>"));
        assert!(raw.contains("Disposition: manual-action/MDN-sent-manually; displayed"));
        assert!(raw.contains("Final-Recipient: rfc822;me@example.com"));
        assert!(raw.contains("Subject: Read: Report"));
        let msg = mailcraft_mime::parse(raw.as_bytes()).unwrap();
        assert!(msg.text_body().unwrap().contains("was read on"));
        assert!(tracking(&s, got).unwrap().read_receipt_sent);
        assert_eq!(s.run("compose.sendReadReceipt", &json!({"id": got.0})).unwrap()["alreadySent"], true);
        // A message without a request has nothing to answer.
        let plain = receive(&mut s, "From: b@example.org\nSubject: x\nMessage-ID: <x@example.org>\n\nx\n");
        assert!(s.run("compose.sendReadReceipt", &json!({"id": plain.0})).is_err());
    }

    #[test]
    fn automatic_read_receipts_follow_rfc_8098() {
        let mut s = session();
        // Return-Path matches the requester: sent automatically, marked as such.
        let ok = receive(
            &mut s,
            "Return-Path: <ann@example.org>\nFrom: Ann <ann@example.org>\nTo: me@example.com\nSubject: A\nMessage-ID: <a1@example.org>\nDisposition-Notification-To: Ann <ann@example.org>\n\nx\n",
        );
        assert!(!tracking(&s, ok).unwrap().read_receipt_ask);
        let r = s.run("compose.sendReadReceipt", &json!({"id": ok.0, "automatic": true})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        let raw = outbox_raw(&s);
        assert!(raw.contains("Disposition: automatic-action/MDN-sent-automatically; displayed"), "{raw}");
        // No Return-Path, a different one, or several addresses: the user is asked instead (section 2.1).
        for (i, head) in [
            "Disposition-Notification-To: ann@example.org\n",
            "Return-Path: <bounce@lists.example.net>\nDisposition-Notification-To: ann@example.org\n",
            "Return-Path: <ann@example.org>\nDisposition-Notification-To: ann@example.org, bo@example.org\n",
        ]
        .into_iter()
        .enumerate()
        {
            let id = receive(
                &mut s,
                &format!(
                    "{head}From: Ann <ann@example.org>\nSubject: B\nMessage-ID: <b{i}@example.org>\n\nx\n"
                ),
            );
            let t = tracking(&s, id).unwrap();
            assert!(t.read_receipt_to.is_some());
            assert!(t.read_receipt_ask, "{head}");
            let r = s.run("compose.sendReadReceipt", &json!({"id": id.0, "automatic": true})).unwrap();
            assert_eq!((r["sent"].clone(), r["ask"].clone()), (json!(false), json!(true)), "{head}");
            // Answered by the user, it goes out as a manual receipt.
            assert_eq!(s.run("compose.sendReadReceipt", &json!({"id": id.0})).unwrap()["sent"], true);
        }
        // Reports (receipts, delivery notifications) are never answered with a receipt.
        let report = receive(
            &mut s,
            "Return-Path: <ann@example.org>\nFrom: ann@example.org\nSubject: Read: Hi\nMessage-ID: <rep@example.org>\nDisposition-Notification-To: ann@example.org\nMIME-Version: 1.0\nContent-Type: multipart/report; report-type=disposition-notification; boundary=b\n\n--b\nContent-Type: text/plain\n\nread\n--b--\n",
        );
        assert_eq!(tracking(&s, report).unwrap().read_receipt_to, None);
        assert!(s.run("compose.sendReadReceipt", &json!({"id": report.0})).is_err());
    }

    #[test]
    fn delivery_and_read_reports_update_the_sent_item() {
        let mut s = session();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "ann@example.org; bo@example.org"})).unwrap();
        s.run("compose.setSubject", &json!({"value": "Plan"})).unwrap();
        s.run("compose.deliveryReceipt", &json!({"on": true})).unwrap();
        s.run("compose.readReceipt", &json!({"on": true})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let sent = folder_items(&s, FolderRole::Outbox)[0].id;
        let mid = s.mailbox.mail[&sent].message_id.clone().unwrap();
        receive(
            &mut s,
            &format!(
                "From: Mail Delivery System <MAILER-DAEMON@example.org>\nSubject: Successful Mail Delivery Report\n\
Message-ID: <d1@example.org>\nContent-Type: multipart/report; report-type=delivery-status; boundary=\"b\"\n\n\
--b\nContent-Type: text/plain\n\nDelivered.\n--b\nContent-Type: message/delivery-status\n\n\
Reporting-MTA: dns; mx.example.org\nOriginal-Envelope-Id: {}\n\nFinal-Recipient: rfc822; ann@example.org\n\
Original-Recipient: rfc822;ann@example.org\nAction: delivered\nStatus: 2.0.0\n\nFinal-Recipient: rfc822; bo@example.org\n\
Action: failed\nStatus: 5.1.1\n--b--\n",
                xtext(&mid)
            ),
        );
        receive(
            &mut s,
            &format!(
                "From: Ann <ann@example.org>\nSubject: Read: Plan\nMessage-ID: <m1@example.org>\nIn-Reply-To: <{mid}>\n\
Content-Type: multipart/report; report-type=disposition-notification; boundary=\"c\"\n\n--c\nContent-Type: text/plain\n\nRead.\n\
--c\nContent-Type: message/disposition-notification\n\nReporting-UA: x\nFinal-Recipient: rfc822;ann@example.org\n\
Original-Message-ID: <{mid}>\nDisposition: manual-action/MDN-sent-manually; displayed\n--c--\n"
            ),
        );
        let t = tracking(&s, sent).unwrap();
        let ann = t.receipts.iter().find(|r| r.recipient == "ann@example.org").unwrap();
        assert_eq!(ann.delivery.as_deref(), Some("Delivered"));
        assert_eq!(ann.read.as_deref(), Some("Read"));
        let bo = t.receipts.iter().find(|r| r.recipient == "bo@example.org").unwrap();
        assert_eq!(bo.delivery.as_deref(), Some("Failed"));
        assert_eq!(bo.read, None);
        // Reports about other messages are ignored.
        assert!(tracking(&s, ItemId(u64::MAX)).is_none());
    }

    #[test]
    fn themes_restyle_the_body() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Title\nBody text"})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 2}}),
        )
        .unwrap();
        s.run("format.style", &json!({"name": "Heading 1"})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [1], "offset": 0}, "focus": {"path": [1], "offset": 4}}),
        )
        .unwrap();
        let office_accent = crate::compose::ThemeState::default().resolve().color(crate::compose::ACCENT1);
        s.run("format.color", &json!({"color": office_accent.to_hex()})).unwrap();
        let r = s.run("compose.theme", &json!({"name": "Organic"})).unwrap();
        assert_eq!(r["headingFont"], "Garamond");
        assert_eq!(r["effectSet"], "Grunge Texture");
        let c = &s.composes[&id];
        assert_eq!(c.body.base.font_family.as_deref(), Some("Garamond"));
        let organic = crate::compose::color_set("Organic").unwrap();
        // The accent-coloured run follows the palette; the heading takes the heading font.
        let mut colors = Vec::new();
        let mut fonts = Vec::new();
        for b in &c.body.blocks {
            if let Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } = b {
                for i in inlines {
                    if let mailcraft_model::Inline::Text { style, .. } = i {
                        colors.push(style.color);
                        fonts.push(style.font_family.clone());
                    }
                }
            }
        }
        assert!(colors.contains(&Some(organic[crate::compose::ACCENT1])), "{colors:?}");
        assert!(fonts.contains(&Some("Garamond".to_owned())));
        // Colors / Fonts / Effects one part at a time, then undo.
        s.run("compose.theme", &json!({"part": "colors", "name": "Blue"})).unwrap();
        assert_eq!(s.composes[&id].theme_resolved().colors, crate::compose::color_set("Blue").unwrap());
        s.run("compose.theme", &json!({"part": "fonts", "name": "Georgia"})).unwrap();
        assert_eq!(s.composes[&id].body.base.font_family.as_deref(), Some("Georgia"));
        s.run("compose.theme", &json!({"part": "effects", "name": "Glossy"})).unwrap();
        assert_eq!(s.composes[&id].theme_resolved().effects, "Glossy");
        assert!(s.run("compose.theme", &json!({"part": "colors", "name": "Plaid"})).is_err());
        assert!(s.run("compose.theme", &json!({"name": "Nope"})).is_err());
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(
            s.composes[&id].body.base.font_family.as_deref(),
            Some("Georgia"),
            "undo restores the body"
        );
        // Galleries for the menus.
        let g = s.run("compose.theme", &json!({"action": "list"})).unwrap();
        assert_eq!(g["colors"].as_array().unwrap().len(), crate::compose::COLOR_GALLERY_LEN);
        assert_eq!(g["fonts"].as_array().unwrap().len(), 26);
        assert_eq!(g["effects"].as_array().unwrap().len(), 15);
        assert_eq!(g["themes"].as_array().unwrap().len(), 11);
        s.run("compose.theme", &json!({"action": "reset"})).unwrap();
        assert_eq!(s.composes[&id].body.base.font_family.as_deref(), Some("Aptos"));
    }

    #[test]
    fn custom_theme_colours_fonts_and_theme_files() {
        let mut s = session();
        let id = open(&mut s);
        assert_eq!(
            s.run("compose.theme", &json!({"action": "customize", "part": "colors"})).unwrap()["dialog"],
            "customizeColors"
        );
        assert_eq!(
            s.run("compose.theme", &json!({"action": "customize", "part": "fonts"})).unwrap()["dialog"],
            "customizeFonts"
        );
        let pal: Vec<String> = (0..12).map(|i| format!("#{:02x}0000", i * 20)).collect();
        assert!(
            s.run("compose.theme", &json!({"action": "customize", "part": "colors", "colors": ["#000000"]}))
                .is_err()
        );
        s.run(
            "compose.theme",
            &json!({"action": "customize", "part": "colors", "colors": pal, "name": "Mine"}),
        )
        .unwrap();
        assert_eq!(s.composes[&id].theme.colors.as_deref(), Some("Mine"));
        assert_eq!(s.composes[&id].theme_resolved().color(5).to_hex(), "#640000");
        s.run("compose.theme", &json!({"action": "customize", "part": "fonts", "heading": "Georgia", "body": "Verdana", "name": "Pair"})).unwrap();
        assert_eq!(s.composes[&id].body.base.font_family.as_deref(), Some("Verdana"));
        // Save Current Theme / Browse for Themes round trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        let path = path.to_str().unwrap();
        s.run("compose.theme", &json!({"action": "save", "path": path, "name": "My Theme"})).unwrap();
        let other = open(&mut s);
        s.run("compose.theme", &json!({"compose": other, "action": "browse", "path": path})).unwrap();
        assert_eq!(s.composes[&other].theme_resolved().colors, s.composes[&id].theme_resolved().colors);
        assert_eq!(s.composes[&other].theme_resolved().heading_font, "Georgia");
        assert_eq!(s.composes[&other].theme.name.as_deref(), Some("My Theme"));
        std::fs::write(dir.path().join("bad.json"), "{}").unwrap();
        assert!(
            s.run(
                "compose.theme",
                &json!({"action": "browse", "path": dir.path().join("bad.json").to_str().unwrap()})
            )
            .is_err()
        );
        assert_eq!(s.run("compose.theme", &json!({"action": "browse"})).unwrap()["dialog"], "browseThemes");
        assert_eq!(s.run("compose.theme", &json!({"action": "save"})).unwrap()["dialog"], "saveTheme");
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
        // A stored field has one recipient per entry: commas that separated were stored as `;`.
        assert_eq!(
            parse_recipients("a@example.com, Bob <b@example.com>"),
            Err(vec!["a@example.com, Bob <b@example.com>".to_owned()])
        );
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
    fn commas_in_recipient_fields_follow_the_rule() {
        let mut s = session();
        let id = open(&mut s);
        let to = |s: &mut Session, v: Value| {
            s.run("compose.setTo", &v).unwrap()["to"].as_str().unwrap().to_owned()
        };
        let list = "a@example.com, Bob <b@example.com>, Rivera, Alice <alice@example.com>; \"Doe, Jo\" <jo@example.org>";
        // CLI / MCP / mailto: an RFC 5322 list, commas always separate (quoted names keep theirs; an unquoted
        // name with a comma is whole only as an entry of its own).
        assert_eq!(
            to(&mut s, json!({"value": list})),
            "a@example.com; Bob <b@example.com>; Rivera; Alice <alice@example.com>; \"Doe, Jo\" <jo@example.org>"
        );
        assert_eq!(parse_recipients(&s.composes[&id].to), Err(vec!["Rivera".to_owned()]));
        assert_eq!(
            to(&mut s, json!({"value": "a@example.com,b@example.com"})),
            "a@example.com;b@example.com"
        );
        assert_eq!(parse_recipients(&s.composes[&id].to).unwrap().len(), 2);
        assert_eq!(
            to(&mut s, json!({"value": "Rivera, Alice <alice@example.com>"})),
            "Rivera, Alice <alice@example.com>"
        );
        // Typed in the window with the option off (the default): commas are part of the entry.
        let typed = json!({"value": "a@example.com, b@example.com; Smith, John", "typed": true});
        assert_eq!(to(&mut s, typed.clone()), "a@example.com, b@example.com; Smith, John");
        assert_eq!(
            parse_recipients(&s.composes[&id].to),
            Err(vec!["a@example.com, b@example.com".to_owned(), "Smith, John".to_owned()])
        );
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], false, "{r}");
        // With "Commas can be used to separate multiple message recipients" on, they separate.
        s.settings.set("mail.send.commasSeparate", &json!(true)).unwrap();
        assert_eq!(to(&mut s, typed), "a@example.com; b@example.com; Smith; John");
        assert_eq!(
            to(
                &mut s,
                json!({"value": "Rivera, Alice <alice@example.com>; c@example.net, d@example.org", "typed": true})
            ),
            "Rivera, Alice <alice@example.com>; c@example.net; d@example.org"
        );
        // Same length, so the text cursor stays where it was.
        assert_eq!(store_recipients(&s, "x@y.z ,w@v.u", RecipientInput::Typed).len(), "x@y.z ,w@v.u".len());
        assert!(s.run("compose.setCc", &json!({"value": "a@example.com", "typed": "yes"})).is_err());
        // Direct Replies To takes an RFC 5322 list.
        s.run("compose.directRepliesTo", &json!({"addresses": "r1@example.com, r2@example.com"})).unwrap();
        assert_eq!(s.composes[&id].reply_to, "r1@example.com; r2@example.com");
        // Splitting with commas, for callers that read text from elsewhere.
        assert_eq!(
            split_recipients_with("a@x.org, b@y.org; \"c, d\" <c@z.org>", true),
            ["a@x.org", "b@y.org", "\"c, d\" <c@z.org>"]
        );
        assert_eq!(split_recipients_with("a@x.org, b@y.org", false), ["a@x.org, b@y.org"]);
        // Hostile text: never panics, multi-byte characters are kept.
        for junk in ["\u{e9}, \"<,;", ",,,;;;", "<<<,>>>", "\"\\", "\u{1F600},\u{1F600}"] {
            let _ = parse_recipients(&store_recipients(&s, junk, RecipientInput::List));
            assert_eq!(store_recipients(&s, junk, RecipientInput::List).len(), junk.len());
        }
    }

    #[test]
    fn drafts_keep_recipients_that_are_not_resolved_yet() {
        // Commas do not separate (the default): "a, b" typed in To is one entry that is not one recipient.
        // Saving the draft must not lose its addresses.
        let mut s = session();
        open(&mut s);
        let typed = "a@example.com, b@example.com; Rivera, Alice <alice@example.com>";
        s.run("compose.setTo", &json!({"value": typed, "typed": true})).unwrap();
        s.run("compose.setCc", &json!({"value": "c@example.org, d@example.org", "typed": true})).unwrap();
        let d = s.run("compose.saveDraft", &json!({})).unwrap()["draft"].as_u64().unwrap();
        let addrs = |list: &[mailcraft_model::Address]| -> Vec<String> {
            list.iter().map(|m| m.addr.clone()).collect()
        };
        let draft = &s.mailbox.mail[&ItemId(d)];
        assert_eq!(addrs(&draft.to), ["a@example.com", "b@example.com", "alice@example.com"]);
        assert_eq!(draft.to.get(2).and_then(|m| m.name.as_deref()), Some("Rivera, Alice"));
        assert_eq!(addrs(&draft.cc), ["c@example.org", "d@example.org"]);
        // Close All Items › Save reads the fields the same way.
        s.run("compose.setSubject", &json!({"value": "changed"})).unwrap();
        let r = s.run("app.closeAllItems", &json!({"save": true})).unwrap();
        let d = r["saved"][0].as_u64().unwrap();
        let draft = &s.mailbox.mail[&ItemId(d)];
        assert_eq!(draft.subject, "changed");
        assert_eq!(addrs(&draft.to), ["a@example.com", "b@example.com", "alice@example.com"]);
        assert_eq!(addrs(&draft.cc), ["c@example.org", "d@example.org"]);
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

/// Options › Mail settings that act on composing and sending.
#[cfg(test)]
mod options_tests {
    use super::tests::{open, session};
    use super::*;
    use mailcraft_model::Contact;

    fn set(s: &mut Session, key: &str, value: Value) {
        s.run("options.set", &json!({"key": key, "value": value})).unwrap();
    }

    fn in_role(s: &Session, role: FolderRole) -> Vec<ItemId> {
        s.mailbox
            .mail
            .values()
            .filter(|m| s.mailbox.folder(m.folder).is_some_and(|f| f.role == Some(role)))
            .map(|m| m.id)
            .collect()
    }

    fn advance(s: &mut Session, secs: i64) {
        let now = s.now();
        s.fixed_now = Some(DateTime { unix: now.unix.saturating_add(secs), offset_min: now.offset_min });
    }

    fn outbox_raw(s: &Session) -> String {
        let id = in_role(s, FolderRole::Outbox).into_iter().max().unwrap();
        String::from_utf8_lossy(s.mailbox.mail[&id].raw.as_ref().unwrap()).into_owned()
    }

    #[test]
    fn auto_save_after_the_set_minutes() {
        let mut s = session();
        let id = open(&mut s);
        // An untouched message is never saved.
        advance(&mut s, 3600);
        assert_eq!(s.run("compose.autoSaveTick", &json!({})).unwrap()["saved"], json!([]));
        s.run("compose.insertText", &json!({"text": "Draft text"})).unwrap();
        advance(&mut s, 2 * 60);
        assert_eq!(s.run("compose.autoSaveTick", &json!({})).unwrap()["saved"], json!([]), "not yet");
        advance(&mut s, 60);
        let r = s.run("compose.autoSaveTick", &json!({})).unwrap();
        let draft = r["saved"][0]["draft"].as_u64().unwrap();
        assert_eq!(r["saved"][0]["compose"], id);
        assert_eq!(in_role(&s, FolderRole::Drafts), [ItemId(draft)]);
        assert!(!s.composes[&id].dirty);
        assert!(s.undo.undo.is_empty(), "auto-save is not an undo step");
        // Typing on starts a new period; the save also happens from typing itself.
        s.run("compose.insertText", &json!({"text": " more"})).unwrap();
        advance(&mut s, 3 * 60);
        s.run("compose.insertText", &json!({"text": "!"})).unwrap();
        assert!(!s.composes[&id].dirty, "saved while typing");
        assert!(s.mailbox.mail[&ItemId(draft)].body_text().contains("Draft text more!"));
        // Every minute when set to 1; never when off.
        set(&mut s, "mail.save.autoSaveMinutes", json!(1));
        s.run("compose.insertText", &json!({"text": "x"})).unwrap();
        advance(&mut s, 60);
        assert_eq!(s.run("compose.autoSaveTick", &json!({})).unwrap()["saved"].as_array().unwrap().len(), 1);
        set(&mut s, "mail.save.autoSave", json!(false));
        s.run("compose.insertText", &json!({"text": "y"})).unwrap();
        advance(&mut s, 3600);
        let r = s.run("compose.autoSaveTick", &json!({})).unwrap();
        assert_eq!((r["saved"].clone(), r["minutes"].clone()), (json!([]), Value::Null));
        assert!(s.composes[&id].dirty);
    }

    #[test]
    fn drafts_go_to_the_chosen_folder() {
        let mut s = session();
        set(&mut s, "mail.save.folder", json!("inbox"));
        open(&mut s);
        s.run("compose.insertText", &json!({"text": "Note to self"})).unwrap();
        let d = s.run("compose.saveDraft", &json!({})).unwrap()["draft"].as_u64().unwrap();
        assert_eq!(in_role(&s, FolderRole::Inbox), [ItemId(d)]);
        assert!(s.mailbox.mail[&ItemId(d)].is_draft);
    }

    #[test]
    fn expiry_date_travels_with_the_message() {
        let mut s = session();
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        assert!(!outbox_raw(&s).contains("Expiry-Date"), "off by default");
        set(&mut s, "mail.send.expire", json!(true));
        set(&mut s, "mail.send.expireDays", json!(2));
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        // The test clock is Fri 2026-10-09 12:00 UTC.
        assert!(
            outbox_raw(&s).contains("Expiry-Date: Sun, 11 Oct 2026 12:00:00 +0000"),
            "{}",
            outbox_raw(&s)
        );
    }

    #[test]
    fn plain_text_wraps_at_the_set_column() {
        assert_eq!(wrap_plain_text("aaa bbb ccc", 7), "aaa bbb\nccc");
        assert_eq!(wrap_plain_text("aaa bbb ccc", 3), "aaa\nbbb\nccc");
        // Long words stay whole; indentation stays; short lines and line ends are kept.
        assert_eq!(
            wrap_plain_text("https://example.com/a/very/long/link x", 10),
            "https://example.com/a/very/long/link\nx"
        );
        assert_eq!(wrap_plain_text("    indented words here", 12), "    indented\nwords here");
        assert_eq!(wrap_plain_text("one\n\ntwo", 2), "one\n\ntwo");
        assert_eq!(wrap_plain_text("éé éé éé", 5), "éé éé\néé");
        assert_eq!(wrap_plain_text("", 0), "");
        let mut s = session();
        set(&mut s, "mail.format.wrapAt", json!(30));
        open(&mut s);
        s.run("compose.format", &json!({"kind": "plain"})).unwrap();
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run(
            "compose.insertText",
            &json!({"text": "The quick brown fox jumps over the lazy dog again and again."}),
        )
        .unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let raw = outbox_raw(&s);
        assert!(raw.contains("The quick brown fox jumps over\r\nthe lazy dog again and again."), "{raw}");
    }

    #[test]
    fn uuencoded_attachments_in_plain_text() {
        assert_eq!(uuencode("cat.txt", b"Cat"), "begin 644 cat.txt\n#0V%T\n`\nend\n");
        let long = uuencode("x\n.bin", &[0u8; 50]);
        assert!(long.starts_with("begin 644 x_.bin\nM````"), "{long}");
        assert_eq!(long.lines().count(), 5, "45 + 5 bytes, then the end lines");
        let mut s = session();
        set(&mut s, "mail.format.uuencode", json!(true));
        let cid = open(&mut s);
        s.run("compose.format", &json!({"kind": "plain"})).unwrap();
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.insertText", &json!({"text": "File below."})).unwrap();
        s.composes
            .get_mut(&cid)
            .unwrap()
            .attach_bytes("cat.txt", Some("text/plain"), b"Cat".to_vec())
            .unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let raw = outbox_raw(&s);
        assert!(raw.contains("begin 644 cat.txt\r\n#0V%T\r\n`\r\nend"), "{raw}");
        assert!(!raw.contains("multipart/mixed"), "no MIME attachment part: {raw}");
    }

    #[test]
    fn rich_text_goes_out_as_html_or_plain_text() {
        let mut s = session();
        let send_rich = |s: &mut Session| {
            open(s);
            s.run("compose.format", &json!({"kind": "rich"})).unwrap();
            s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
            s.run("compose.insertText", &json!({"text": "Hello"})).unwrap();
            s.run("compose.send", &json!({})).unwrap();
            outbox_raw(s)
        };
        assert!(send_rich(&mut s).contains("text/html"));
        set(&mut s, "mail.format.richTextToInternet", json!("plain"));
        let raw = send_rich(&mut s);
        assert!(!raw.contains("text/html") && raw.contains("Hello"), "{raw}");
    }

    #[test]
    fn spelling_is_checked_before_sending() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.insertText", &json!({"text": "Teh recieved wrod"})).unwrap();
        set(&mut s, "mail.compose.spellBeforeSend", json!(true));
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!((r["sent"].clone(), r["reason"].clone()), (json!(false), json!("spelling")), "{r}");
        assert!(r["issues"].as_u64().unwrap() >= 2);
        let Some(Notice::OpenDialog(name, params)) = s.notices.last().cloned() else { panic!("no dialog") };
        assert_eq!((name.as_str(), params["command"].as_str()), ("confirm", Some("compose.send")));
        assert!(s.composes.contains_key(&cid));
        // "Send anyway" from the dialog.
        let r = s.run("compose.send", &params["params"]).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        // Off (the default): no check.
        set(&mut s, "mail.compose.spellBeforeSend", json!(false));
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.insertText", &json!({"text": "Teh wrod"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
    }

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
    fn names_are_checked_automatically_on_send() {
        let mut s = session();
        add_contact(&mut s, "Bob", "Chen", &["bob@example.org"]);
        add_contact(&mut s, "Ann", "Lee", &["ann@example.org", "ann.lee@example.com"]);
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "bob"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        assert!(outbox_raw(&s).contains("To: Bob Chen <bob@example.org>"));
        // Several matches: Check Names asks, nothing is sent.
        let cid = open(&mut s);
        s.run("compose.setTo", &json!({"value": "Ann Lee"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["reason"], "checkNames", "{r}");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "checkNames"));
        assert!(s.composes.contains_key(&cid));
        // No match: the usual error. Option off: no lookup, the name is refused.
        s.run("compose.setTo", &json!({"value": "nobody"})).unwrap();
        assert!(s.run("compose.send", &json!({})).unwrap()["error"].as_str().unwrap().contains("not valid"));
        set(&mut s, "mail.send.autoNameCheck", json!(false));
        s.run("compose.setTo", &json!({"value": "bob"})).unwrap();
        let r = s.run("compose.send", &json!({})).unwrap();
        assert_eq!(r["sent"], false);
        assert_eq!(s.composes[&cid].to, "bob");
    }

    #[test]
    fn prefaced_comments_in_the_original() {
        let mut s = session();
        let cid = s.open_compose(ComposeState {
            kind: ComposeKind::Reply,
            format: BodyFormat::Html,
            preface: Some("Kim".into()),
            body: Doc {
                blocks: vec![
                    Block::text_paragraph(""),
                    Block::Rule,
                    Block::text_paragraph("From: Ann <ann@example.org>"),
                    Block::text_paragraph("Can you come?"),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let type_at = |s: &mut Session, path: usize, offset: usize, t: &str| {
            s.run("compose.select", &json!({"compose": cid, "anchor": {"path": [path], "offset": offset}}))
                .unwrap();
            s.run("compose.insertText", &json!({"compose": cid, "text": t})).unwrap();
        };
        // Above the original: no preface.
        type_at(&mut s, 0, 0, "Hi");
        assert!(s.composes[&cid].body.to_plain_text().starts_with("Hi\n"));
        // Inside it: "[Kim] " once per comment, bold italic; the comment itself is not.
        type_at(&mut s, 3, 13, " Y");
        for ch in ["e", "s"] {
            s.run("compose.insertText", &json!({"compose": cid, "text": ch})).unwrap();
        }
        s.run("compose.deleteText", &json!({"compose": cid})).unwrap();
        s.run("compose.insertText", &json!({"compose": cid, "text": "s"})).unwrap();
        let c = &s.composes[&cid];
        assert_eq!(c.body.to_plain_text().lines().last(), Some("Can you come?[Kim]  Yes"));
        let Block::Paragraph { inlines, .. } = &c.body.blocks[3] else { panic!() };
        let styled: Vec<(String, Option<bool>, Option<bool>)> = inlines
            .iter()
            .filter_map(|i| match i {
                mailcraft_model::Inline::Text { text, style } => {
                    Some((text.clone(), style.bold, style.italic))
                }
                _ => None,
            })
            .collect();
        assert!(
            styled.iter().any(|(t, b, i)| t == "[Kim] " && *b == Some(true) && *i == Some(true)),
            "{styled:?}"
        );
        assert!(styled.iter().any(|(t, b, _)| t.contains("Yes") && *b != Some(true)), "{styled:?}");
        // Somewhere else in the original: a new preface.
        type_at(&mut s, 3, 0, "No. ");
        assert!(s.composes[&cid].body.to_plain_text().ends_with("[Kim] No. Can you come?[Kim]  Yes"));
        // Off: plain typing.
        s.composes.get_mut(&cid).unwrap().preface = None;
        type_at(&mut s, 2, 0, ">");
        assert!(s.composes[&cid].body.to_plain_text().contains(">From: Ann"));
    }

    #[test]
    fn sent_items_do_not_update_when_tracking_is_off() {
        let mut s = session();
        set(&mut s, "mail.tracking.updateSentItem", json!(false));
        open(&mut s);
        s.run("compose.setTo", &json!({"value": "ann@example.org"})).unwrap();
        s.run("compose.setSubject", &json!({"value": "Plan"})).unwrap();
        s.run("compose.readReceipt", &json!({"on": true})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let sent = in_role(&s, FolderRole::Outbox)[0];
        let mid = s.mailbox.mail[&sent].message_id.clone().unwrap();
        let raw = format!(
            "From: Ann <ann@example.org>\r\nSubject: Read: Plan\r\nMessage-ID: <m1@example.org>\r\nIn-Reply-To: <{mid}>\r\n\
Content-Type: multipart/report; report-type=disposition-notification; boundary=\"c\"\r\n\r\n--c\r\nContent-Type: text/plain\r\n\r\nRead.\r\n\
--c\r\nContent-Type: message/disposition-notification\r\n\r\nReporting-UA: x\r\nFinal-Recipient: rfc822;ann@example.org\r\n\
Original-Message-ID: <{mid}>\r\nDisposition: manual-action/MDN-sent-manually; displayed\r\n--c--\r\n"
        )
        .into_bytes();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, s.now());
        item.folder = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        s.mailbox.add_mail(item);
        assert!(tracking(&s, sent).unwrap().receipts.is_empty());
        set(&mut s, "mail.tracking.updateSentItem", json!(true));
        assert_eq!(tracking(&s, sent).unwrap().receipts.len(), 1);
    }
}
