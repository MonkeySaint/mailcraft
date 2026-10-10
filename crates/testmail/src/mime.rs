//! Just enough RFC 5322 / MIME structure for IMAP: header fields, ENVELOPE, BODYSTRUCTURE and body sections.
//! Lenient and bounded: hostile input never panics, recursion and part counts are capped.

const MAX_DEPTH: usize = 16;
const MAX_PARTS: usize = 512;

/// Header block (including the blank separator line, if any) and body.
pub fn split_message(raw: &[u8]) -> (&[u8], &[u8]) {
    if raw.starts_with(b"\r\n") {
        return (raw.get(..2).unwrap_or(&[]), raw.get(2..).unwrap_or(&[]));
    }
    if raw.starts_with(b"\n") {
        return (raw.get(..1).unwrap_or(&[]), raw.get(1..).unwrap_or(&[]));
    }
    let mut i = 0;
    while let Some(off) = raw.get(i..).and_then(|r| r.iter().position(|&b| b == b'\n')) {
        let nl = i + off;
        let next = nl + 1;
        match raw.get(next) {
            Some(b'\n') => return (raw.get(..next + 1).unwrap_or(raw), raw.get(next + 1..).unwrap_or(&[])),
            Some(b'\r') if raw.get(next + 1) == Some(&b'\n') => {
                return (raw.get(..next + 2).unwrap_or(raw), raw.get(next + 2..).unwrap_or(&[]));
            }
            _ => {}
        }
        i = next;
    }
    (raw, &[])
}

/// A header field: name and the full field text (including folding and the final line ending).
pub struct Field<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
    pub full: &'a [u8],
}

pub fn fields(header: &[u8]) -> Vec<Field<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < header.len() {
        let start = pos;
        // Find the end of this field: a line ending not followed by WSP.
        let mut end = header.len();
        let mut i = pos;
        while let Some(off) = header.get(i..).and_then(|r| r.iter().position(|&b| b == b'\n')) {
            let nl = i + off + 1;
            match header.get(nl) {
                Some(b' ') | Some(b'\t') => i = nl,
                _ => {
                    end = nl;
                    break;
                }
            }
        }
        pos = end;
        let full = header.get(start..end).unwrap_or(&[]);
        if full == b"\r\n" || full == b"\n" {
            continue;
        }
        if let Some(colon) = full.iter().position(|&b| b == b':') {
            let name = trim(full.get(..colon).unwrap_or(&[]));
            if !name.is_empty() && !name.iter().any(|&b| b <= b' ') {
                out.push(Field { name, value: full.get(colon + 1..).unwrap_or(&[]), full });
            }
        }
        if out.len() > 10_000 {
            break;
        }
    }
    out
}

pub fn trim(b: &[u8]) -> &[u8] {
    let s = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let e = b.iter().rposition(|c| !c.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(s);
    b.get(s..e.max(s)).unwrap_or(&[])
}

/// Unfolded, trimmed value of the first field called `name`.
pub fn header_value(header: &[u8], name: &str) -> Option<Vec<u8>> {
    fields(header).into_iter().find(|f| f.name.eq_ignore_ascii_case(name.as_bytes())).map(|f| unfold(f.value))
}

pub fn unfold(v: &[u8]) -> Vec<u8> {
    let v: Vec<u8> = v.iter().copied().filter(|&b| b != b'\r' && b != b'\n').collect();
    trim(&v).to_vec()
}

// ---------------------------------------------------------------- IMAP string output

/// Writes an IMAP string: quoted when safe, otherwise a literal.
pub fn write_string(out: &mut Vec<u8>, s: &[u8]) {
    let quotable = s.len() < 1024 && s.iter().all(|&b| (0x20..0x7f).contains(&b) || b == b'\t');
    if quotable {
        out.push(b'"');
        for &b in s {
            if b == b'"' || b == b'\\' {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(format!("{{{}}}\r\n", s.len()).as_bytes());
        out.extend_from_slice(s);
    }
}

pub fn write_nstring(out: &mut Vec<u8>, s: Option<&[u8]>) {
    match s {
        Some(s) => write_string(out, s),
        None => out.extend_from_slice(b"NIL"),
    }
}

// ---------------------------------------------------------------- addresses and ENVELOPE

#[derive(Debug, PartialEq, Eq)]
pub enum Addr {
    Mailbox { name: Option<Vec<u8>>, adl: Option<Vec<u8>>, local: Vec<u8>, host: Vec<u8> },
    GroupStart(Vec<u8>),
    GroupEnd,
}

/// Parses an RFC 5322 address list (lenient).
pub fn parse_addresses(v: &[u8]) -> Vec<Addr> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let (mut in_quote, mut esc, mut angle, mut comment) = (false, false, 0u32, 0u32);
    let mut in_group = false;
    for &b in v {
        if esc {
            esc = false;
            cur.push(b);
            continue;
        }
        match b {
            b'\\' if in_quote || comment > 0 => {
                esc = true;
                cur.push(b);
                continue;
            }
            b'"' if comment == 0 => in_quote = !in_quote,
            b'(' if !in_quote => comment = comment.saturating_add(1),
            b')' if !in_quote => comment = comment.saturating_sub(1),
            b'<' if !in_quote && comment == 0 => angle = angle.saturating_add(1),
            b'>' if !in_quote && comment == 0 => angle = angle.saturating_sub(1),
            b':' if !in_quote && comment == 0 && angle == 0 && !in_group => {
                out.push(Addr::GroupStart(unquote(trim(&strip_comments(&cur)))));
                cur.clear();
                in_group = true;
                continue;
            }
            b',' if !in_quote && comment == 0 && angle == 0 => {
                push_mailbox(&mut out, &cur);
                cur.clear();
                continue;
            }
            b';' if !in_quote && comment == 0 && angle == 0 && in_group => {
                push_mailbox(&mut out, &cur);
                cur.clear();
                out.push(Addr::GroupEnd);
                in_group = false;
                continue;
            }
            _ => {}
        }
        cur.push(b);
        if out.len() > 1000 {
            break;
        }
    }
    push_mailbox(&mut out, &cur);
    if in_group {
        out.push(Addr::GroupEnd);
    }
    out
}

fn strip_comments(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut depth, mut in_quote, mut esc) = (0u32, false, false);
    for &b in v {
        if esc {
            esc = false;
            if depth == 0 {
                out.push(b);
            }
            continue;
        }
        match b {
            b'\\' => {
                esc = true;
                if depth == 0 {
                    out.push(b);
                }
                continue;
            }
            b'"' if depth == 0 => in_quote = !in_quote,
            b'(' if !in_quote => {
                depth = depth.saturating_add(1);
                continue;
            }
            b')' if !in_quote && depth > 0 => {
                depth -= 1;
                continue;
            }
            _ => {}
        }
        if depth == 0 {
            out.push(b);
        }
    }
    out
}

fn unquote(v: &[u8]) -> Vec<u8> {
    let v = trim(v);
    if v.len() >= 2 && v.first() == Some(&b'"') && v.last() == Some(&b'"') {
        let inner = v.get(1..v.len() - 1).unwrap_or(&[]);
        let mut out = Vec::with_capacity(inner.len());
        let mut esc = false;
        for &b in inner {
            if !esc && b == b'\\' {
                esc = true;
                continue;
            }
            esc = false;
            out.push(b);
        }
        return out;
    }
    v.to_vec()
}

fn push_mailbox(out: &mut Vec<Addr>, raw: &[u8]) {
    let text = strip_comments(raw);
    let text = trim(&text);
    if text.is_empty() {
        return;
    }
    let (name, spec) = match (text.iter().position(|&b| b == b'<'), text.iter().rposition(|&b| b == b'>')) {
        (Some(l), Some(r)) if l < r => {
            let name = unquote(text.get(..l).unwrap_or(&[]));
            (if name.is_empty() { None } else { Some(name) }, text.get(l + 1..r).unwrap_or(&[]).to_vec())
        }
        _ => (None, text.to_vec()),
    };
    let spec: Vec<u8> = spec.into_iter().filter(|b| !b.is_ascii_whitespace()).collect();
    // Obsolete source route: "@a,@b:local@host".
    let (adl, spec) = match spec.iter().position(|&b| b == b':') {
        Some(i) if spec.first() == Some(&b'@') => {
            (Some(spec.get(..i).unwrap_or(&[]).to_vec()), spec.get(i + 1..).unwrap_or(&[]).to_vec())
        }
        _ => (None, spec),
    };
    let (local, host) = match spec.iter().rposition(|&b| b == b'@') {
        Some(i) => (unquote(spec.get(..i).unwrap_or(&[])), spec.get(i + 1..).unwrap_or(&[]).to_vec()),
        None => (unquote(&spec), Vec::new()),
    };
    out.push(Addr::Mailbox { name, adl, local, host });
}

fn write_addresses(out: &mut Vec<u8>, header: &[u8], name: &str) -> bool {
    let Some(v) = header_value(header, name) else {
        return false;
    };
    let addrs = parse_addresses(&v);
    if addrs.is_empty() {
        return false;
    }
    out.push(b'(');
    for a in addrs {
        match a {
            Addr::Mailbox { name, adl, local, host } => {
                out.push(b'(');
                write_nstring(out, name.as_deref());
                out.push(b' ');
                write_nstring(out, adl.as_deref());
                out.push(b' ');
                write_string(out, &local);
                out.push(b' ');
                write_string(out, &host);
                out.push(b')');
            }
            Addr::GroupStart(n) => {
                out.extend_from_slice(b"(NIL NIL ");
                write_string(out, &n);
                out.extend_from_slice(b" NIL)");
            }
            Addr::GroupEnd => out.extend_from_slice(b"(NIL NIL NIL NIL)"),
        }
    }
    out.push(b')');
    true
}

/// RFC 3501 ENVELOPE for a message header.
pub fn write_envelope(out: &mut Vec<u8>, header: &[u8]) {
    out.push(b'(');
    write_nstring(out, header_value(header, "Date").as_deref());
    out.push(b' ');
    write_nstring(out, header_value(header, "Subject").as_deref());
    for (field, fallback) in [
        ("From", None),
        ("Sender", Some("From")),
        ("Reply-To", Some("From")),
        ("To", None),
        ("Cc", None),
        ("Bcc", None),
    ] {
        out.push(b' ');
        if !write_addresses(out, header, field) {
            let done = fallback.is_some_and(|f| write_addresses(out, header, f));
            if !done {
                out.extend_from_slice(b"NIL");
            }
        }
    }
    out.push(b' ');
    write_nstring(out, header_value(header, "In-Reply-To").as_deref());
    out.push(b' ');
    write_nstring(out, header_value(header, "Message-ID").as_deref());
    out.push(b')');
}

// ---------------------------------------------------------------- MIME parts

pub struct Part<'a> {
    pub header: &'a [u8],
    pub body: &'a [u8],
    pub mtype: String,
    pub subtype: String,
    pub params: Vec<(String, String)>,
    pub children: Vec<Part<'a>>,
    pub message: Option<Box<Part<'a>>>,
}

impl Part<'_> {
    pub fn is_multipart(&self) -> bool {
        self.mtype == "multipart"
    }
}

pub fn parse(raw: &[u8]) -> Part<'_> {
    let mut budget = MAX_PARTS;
    parse_part(raw, false, 0, &mut budget)
}

fn parse_part<'a>(raw: &'a [u8], in_digest: bool, depth: usize, budget: &mut usize) -> Part<'a> {
    *budget = budget.saturating_sub(1);
    let (header, body) = split_message(raw);
    let (mtype, subtype, params) = match header_value(header, "Content-Type") {
        Some(v) => parse_content_type(&v),
        None if in_digest => ("message".to_owned(), "rfc822".to_owned(), Vec::new()),
        None => ("text".to_owned(), "plain".to_owned(), vec![("charset".to_owned(), "us-ascii".to_owned())]),
    };
    let mut part = Part { header, body, mtype, subtype, params, children: Vec::new(), message: None };
    if depth >= MAX_DEPTH || *budget == 0 {
        return part;
    }
    if part.is_multipart() {
        if let Some(b) = param(&part.params, "boundary").filter(|b| !b.is_empty()) {
            let digest = part.subtype == "digest";
            for chunk in split_multipart(body, b.as_bytes()) {
                if *budget == 0 {
                    break;
                }
                part.children.push(parse_part(chunk, digest, depth + 1, budget));
            }
        }
    } else if part.mtype == "message" && (part.subtype == "rfc822" || part.subtype == "global") {
        part.message = Some(Box::new(parse_part(body, false, depth + 1, budget)));
    }
    part
}

pub fn param<'p>(params: &'p [(String, String)], name: &str) -> Option<&'p str> {
    params.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// `type/subtype; k=v; ...` → lowercase type, subtype and parameter names.
pub fn parse_content_type(v: &[u8]) -> (String, String, Vec<(String, String)>) {
    let s = String::from_utf8_lossy(&strip_comments(v)).into_owned();
    let mut segs = split_params(&s).into_iter();
    let ty = segs.next().unwrap_or_default();
    let (t, st) = match ty.split_once('/') {
        Some((t, st)) if !t.trim().is_empty() && !st.trim().is_empty() => {
            (t.trim().to_ascii_lowercase(), st.trim().to_ascii_lowercase())
        }
        _ => ("text".to_owned(), "plain".to_owned()),
    };
    (t, st, segs.filter_map(|p| parse_param(&p)).collect())
}

/// Splits on `;` outside quotes.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut q, mut esc) = (false, false);
    for c in s.chars() {
        if esc {
            esc = false;
            cur.push(c);
            continue;
        }
        match c {
            '\\' if q => esc = true,
            '"' => q = !q,
            ';' if !q => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
        if out.len() > 100 {
            break;
        }
    }
    out.push(cur);
    out
}

fn parse_param(p: &str) -> Option<(String, String)> {
    let (k, v) = p.split_once('=')?;
    let k = k.trim().to_ascii_lowercase();
    if k.is_empty() {
        return None;
    }
    Some((k, String::from_utf8_lossy(&unquote(v.trim().as_bytes())).into_owned()))
}

fn split_multipart<'a>(body: &'a [u8], boundary: &[u8]) -> Vec<&'a [u8]> {
    let mut delim = b"--".to_vec();
    delim.extend_from_slice(boundary);
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut pos = 0;
    while pos < body.len() {
        let line_end = body
            .get(pos..)
            .and_then(|r| r.iter().position(|&b| b == b'\n'))
            .map(|i| pos + i + 1)
            .unwrap_or(body.len());
        let line = body.get(pos..line_end).unwrap_or(&[]);
        if let Some(rest) = line.strip_prefix(delim.as_slice()) {
            let close = rest.starts_with(b"--");
            let tail = if close { rest.get(2..).unwrap_or(&[]) } else { rest };
            if tail.iter().all(|b| b.is_ascii_whitespace()) {
                if let Some(s) = start {
                    let mut e = pos;
                    if e > s && body.get(e - 1) == Some(&b'\n') {
                        e -= 1;
                        if e > s && body.get(e - 1) == Some(&b'\r') {
                            e -= 1;
                        }
                    }
                    parts.push(body.get(s..e).unwrap_or(&[]));
                }
                if close || parts.len() >= MAX_PARTS {
                    return parts;
                }
                start = Some(line_end);
            }
        }
        pos = line_end;
    }
    if let Some(s) = start {
        parts.push(body.get(s..).unwrap_or(&[]));
    }
    parts
}

/// Number of lines; a final line without a line ending counts too.
fn line_count(b: &[u8]) -> usize {
    let n = b.iter().filter(|&&c| c == b'\n').count();
    if b.is_empty() || b.ends_with(b"\n") { n } else { n + 1 }
}

fn write_params(out: &mut Vec<u8>, params: &[(String, String)]) {
    if params.is_empty() {
        out.extend_from_slice(b"NIL");
        return;
    }
    out.push(b'(');
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        write_string(out, k.to_ascii_uppercase().as_bytes());
        out.push(b' ');
        write_string(out, v.as_bytes());
    }
    out.push(b')');
}

fn write_disposition(out: &mut Vec<u8>, header: &[u8]) {
    match header_value(header, "Content-Disposition") {
        Some(v) => {
            let s = String::from_utf8_lossy(&v).into_owned();
            let mut segs = split_params(&s).into_iter();
            let kind = segs.next().unwrap_or_default().trim().to_ascii_uppercase();
            let params: Vec<(String, String)> = segs.filter_map(|p| parse_param(&p)).collect();
            out.push(b'(');
            write_string(out, kind.as_bytes());
            out.push(b' ');
            write_params(out, &params);
            out.push(b')');
        }
        None => out.extend_from_slice(b"NIL"),
    }
}

/// BODYSTRUCTURE (`ext` = true) or BODY (`ext` = false).
pub fn write_body_structure(out: &mut Vec<u8>, p: &Part<'_>, ext: bool) {
    out.push(b'(');
    if p.is_multipart() {
        if p.children.is_empty() {
            out.extend_from_slice(b"(\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 0 0)");
        }
        for c in &p.children {
            write_body_structure(out, c, ext);
        }
        out.push(b' ');
        write_string(out, p.subtype.to_ascii_uppercase().as_bytes());
        if ext {
            out.push(b' ');
            write_params(out, &p.params);
            out.push(b' ');
            write_disposition(out, p.header);
            out.extend_from_slice(b" NIL NIL");
        }
    } else {
        write_string(out, p.mtype.to_ascii_uppercase().as_bytes());
        out.push(b' ');
        write_string(out, p.subtype.to_ascii_uppercase().as_bytes());
        out.push(b' ');
        write_params(out, &p.params);
        out.push(b' ');
        write_nstring(out, header_value(p.header, "Content-ID").as_deref());
        out.push(b' ');
        write_nstring(out, header_value(p.header, "Content-Description").as_deref());
        out.push(b' ');
        let enc = header_value(p.header, "Content-Transfer-Encoding")
            .map(|e| String::from_utf8_lossy(&e).to_ascii_uppercase())
            .unwrap_or_else(|| "7BIT".to_owned());
        write_string(out, enc.as_bytes());
        out.extend_from_slice(format!(" {}", p.body.len()).as_bytes());
        if let Some(m) = &p.message {
            out.push(b' ');
            write_envelope(out, m.header);
            out.push(b' ');
            write_body_structure(out, m, ext);
            out.extend_from_slice(format!(" {}", line_count(p.body)).as_bytes());
        } else if p.mtype == "text" {
            out.extend_from_slice(format!(" {}", line_count(p.body)).as_bytes());
        }
        if ext {
            out.push(b' ');
            write_nstring(out, header_value(p.header, "Content-MD5").as_deref());
            out.push(b' ');
            write_disposition(out, p.header);
            out.extend_from_slice(b" NIL NIL");
        }
    }
    out.push(b')');
}

// ---------------------------------------------------------------- sections

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionText {
    Full,
    Header,
    HeaderFields { names: Vec<String>, not: bool },
    Text,
    Mime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub path: Vec<u32>,
    pub text: SectionText,
}

impl Section {
    /// The section spec as echoed in a FETCH response, e.g. `1.2.HEADER.FIELDS (SUBJECT)`.
    pub fn spec(&self) -> String {
        let mut s: Vec<String> = self.path.iter().map(|n| n.to_string()).collect();
        let text = match &self.text {
            SectionText::Full => None,
            SectionText::Header => Some("HEADER".to_owned()),
            SectionText::Text => Some("TEXT".to_owned()),
            SectionText::Mime => Some("MIME".to_owned()),
            SectionText::HeaderFields { names, not } => Some(format!(
                "HEADER.FIELDS{} ({})",
                if *not { ".NOT" } else { "" },
                names.iter().map(|n| n.to_ascii_uppercase()).collect::<Vec<_>>().join(" ")
            )),
        };
        if let Some(t) = text {
            s.push(t);
        }
        s.join(".")
    }
}

fn child<'p, 'a>(p: &'p Part<'a>, n: u32) -> Option<&'p Part<'a>> {
    let idx = usize::try_from(n.checked_sub(1)?).ok()?;
    if p.is_multipart() {
        return p.children.get(idx);
    }
    if let Some(m) = &p.message {
        return child(m, n);
    }
    if n == 1 { Some(p) } else { None }
}

fn filter_fields(header: &[u8], names: &[String], not: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fields(header) {
        let hit = names.iter().any(|n| f.name.eq_ignore_ascii_case(n.as_bytes()));
        if hit != not {
            out.extend_from_slice(f.full);
            if !f.full.ends_with(b"\n") {
                out.extend_from_slice(b"\r\n");
            }
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Bytes of a body section; `None` when the section does not exist.
pub fn extract(raw: &[u8], sec: &Section) -> Option<Vec<u8>> {
    let top = parse(raw);
    let (header, body, whole): (&[u8], &[u8], &[u8]) = if sec.path.is_empty() {
        (top.header, top.body, raw)
    } else {
        let first = *sec.path.first()?;
        let mut p: &Part<'_> = if top.is_multipart() {
            top.children.get(usize::try_from(first.checked_sub(1)?).ok()?)?
        } else if first == 1 {
            &top
        } else {
            return None;
        };
        for &n in sec.path.get(1..).unwrap_or(&[]) {
            p = child(p, n)?;
        }
        match sec.text {
            SectionText::Full => return Some(p.body.to_vec()),
            SectionText::Mime => return Some(p.header.to_vec()),
            _ => {
                let m = p.message.as_deref()?;
                (m.header, m.body, p.body)
            }
        }
    };
    Some(match &sec.text {
        SectionText::Full => whole.to_vec(),
        SectionText::Header => header.to_vec(),
        SectionText::Text => body.to_vec(),
        SectionText::HeaderFields { names, not } => filter_fields(header, names, *not),
        SectionText::Mime => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI: &[u8] = b"From: \"Doe, Jane\" <jane@example.com>\r\nTo: bob@example.org, Team: a@example.net, b@example.net;\r\nSubject: Hi\r\n there\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\npreamble\r\n--XX\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello\r\n--XX\r\nContent-Type: message/rfc822\r\n\r\nSubject: inner\r\n\r\ninner body\r\n--XX--\r\n";

    #[test]
    fn header_and_envelope() {
        let (h, b) = split_message(MULTI);
        assert!(h.ends_with(b"\r\n\r\n"));
        assert!(b.starts_with(b"preamble"));
        assert_eq!(header_value(h, "subject").unwrap(), b"Hi there");
        let mut out = Vec::new();
        write_envelope(&mut out, h);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("((\"Doe, Jane\" NIL \"jane\" \"example.com\"))"), "{s}");
        assert!(s.contains("(NIL NIL \"Team\" NIL)(NIL NIL \"a\" \"example.net\")"), "{s}");
        assert!(s.ends_with("(NIL NIL NIL NIL)) NIL NIL NIL NIL)"), "{s}");
    }

    #[test]
    fn structure_and_sections() {
        let p = parse(MULTI);
        assert_eq!(p.children.len(), 2);
        let mut out = Vec::new();
        write_body_structure(&mut out, &p, true);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 5 1"), "{s}");
        assert!(s.contains("\"MIXED\" (\"BOUNDARY\" \"XX\")"), "{s}");
        let sec = |path: Vec<u32>, text| extract(MULTI, &Section { path, text });
        assert_eq!(sec(vec![1], SectionText::Full).unwrap(), b"hello");
        assert_eq!(sec(vec![2, 1], SectionText::Full).unwrap(), b"inner body");
        assert_eq!(sec(vec![2], SectionText::Text).unwrap(), b"inner body");
        assert_eq!(sec(vec![2], SectionText::Header).unwrap(), b"Subject: inner\r\n\r\n");
        assert_eq!(
            sec(vec![1], SectionText::Mime).unwrap(),
            b"Content-Type: text/plain; charset=utf-8\r\n\r\n"
        );
        assert!(sec(vec![9], SectionText::Full).is_none());
        let hf =
            sec(vec![], SectionText::HeaderFields { names: vec!["subject".into()], not: false }).unwrap();
        assert_eq!(hf, b"Subject: Hi\r\n there\r\n\r\n");
    }

    #[test]
    fn hostile_input() {
        for raw in
            [&b""[..], b"\r\n", b"no colon here", b"Content-Type: multipart/mixed; boundary=\"\r\n\r\n--\r\n"]
        {
            let p = parse(raw);
            let mut out = Vec::new();
            write_body_structure(&mut out, &p, true);
            write_envelope(&mut out, p.header);
        }
        // Deep nesting is capped.
        let mut raw = Vec::new();
        for _ in 0..100 {
            raw.extend_from_slice(b"Content-Type: message/rfc822\r\n\r\n");
        }
        let p = parse(&raw);
        let mut out = Vec::new();
        write_body_structure(&mut out, &p, false);
    }
}
