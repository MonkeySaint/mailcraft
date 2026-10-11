//! RFC 5322 §3.4 addresses.

use serde::{Deserialize, Serialize};

/// One mailbox: optional display name + addr-spec.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mailbox {
    /// Decoded display name (RFC 2047 words decoded), `None` when absent or empty.
    pub name: Option<String>,
    /// `local@domain` as written (domain not lower-cased; compare with [`Mailbox::same_address`]).
    pub addr: String,
}

impl Mailbox {
    pub fn new(name: Option<&str>, addr: &str) -> Self {
        Self { name: name.map(str::to_owned).filter(|n| !n.is_empty()), addr: addr.to_owned() }
    }
    /// Display name if present, else the address.
    pub fn display(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.addr)
    }
    /// `Name <addr>` or `addr` (the form shown in quoted headers and recipient fields).
    pub fn to_display_string(&self) -> String {
        match &self.name {
            Some(n) => format!("{n} <{}>", self.addr),
            None => self.addr.clone(),
        }
    }
    /// Case-insensitive address comparison (local part compared case-insensitively too, as mail clients do).
    pub fn same_address(&self, other: &Mailbox) -> bool {
        self.addr.eq_ignore_ascii_case(&other.addr)
    }
}

/// A mailbox or a named group of mailboxes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Address {
    Mailbox(Mailbox),
    Group { name: String, members: Vec<Mailbox> },
}

impl Address {
    /// All mailboxes (a group expands to its members).
    pub fn mailboxes(&self) -> Vec<Mailbox> {
        match self {
            Address::Mailbox(m) => vec![m.clone()],
            Address::Group { members, .. } => members.clone(),
        }
    }
}

/// Most addresses returned from one list (the rest are ignored).
pub const MAX_ADDRESSES: usize = 10_000;
/// Most lexical tokens examined in one list.
const MAX_TOKENS: usize = 200_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Atom(String),
    Quoted(String),
    Comment(String),
    DomainLiteral(String),
    Special(char),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    /// Whitespace (or a comment) preceded this token.
    ws: bool,
}

fn is_special(c: char) -> bool {
    matches!(c, '<' | '>' | '@' | ',' | ';' | ':' | '.' | '[' | ']' | '(' | ')' | '"' | '\\')
}

fn lex(input: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut chars = input.chars().peekable();
    let mut ws = false;
    while let Some(&c) = chars.peek() {
        if out.len() >= MAX_TOKENS {
            break;
        }
        if c.is_whitespace() {
            ws = true;
            chars.next();
            continue;
        }
        match c {
            '(' => {
                chars.next();
                let mut depth = 1usize;
                let mut text = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                text.push(n);
                            }
                        }
                        '(' => {
                            depth += 1;
                            text.push(c);
                        }
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            text.push(c);
                        }
                        _ => text.push(c),
                    }
                }
                out.push(Token { tok: Tok::Comment(text), ws });
                ws = true;
            }
            '"' => {
                chars.next();
                let mut text = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                text.push(n);
                            }
                        }
                        '\r' | '\n' => {}
                        _ => text.push(c),
                    }
                }
                out.push(Token { tok: Tok::Quoted(text), ws });
                ws = false;
            }
            '[' => {
                chars.next();
                let mut text = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    if !c.is_whitespace() {
                        text.push(c);
                    }
                }
                out.push(Token { tok: Tok::DomainLiteral(text), ws });
                ws = false;
            }
            ')' | ']' | '\\' => {
                // Stray closers and backslashes carry no meaning outside their constructs.
                chars.next();
                ws = true;
            }
            _ if is_special(c) => {
                chars.next();
                out.push(Token { tok: Tok::Special(c), ws });
                ws = false;
            }
            _ => {
                let mut text = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || is_special(c) {
                        break;
                    }
                    text.push(c);
                    chars.next();
                }
                out.push(Token { tok: Tok::Atom(text), ws });
                ws = false;
            }
        }
    }
    out
}

fn is_sp(t: Option<&Token>, c: char) -> bool {
    matches!(t, Some(Token { tok: Tok::Special(x), .. }) if *x == c)
}

/// Parses an address-list header value (already unfolded; may contain RFC 2047 words, comments, groups,
/// obsolete syntax). Lenient: unparseable entries are skipped; bare `addr` and `Name <addr>` both accepted;
/// also accepts the `;`-separated form users type into recipient fields.
pub fn parse_address_list(input: &str) -> Vec<Address> {
    let toks = lex(input);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < toks.len() && out.len() < MAX_ADDRESSES {
        if is_sp(toks.get(i), ',') || is_sp(toks.get(i), ';') {
            i += 1;
            continue;
        }
        if let Some(colon) = group_colon(&toks, i) {
            let name = phrase_text(toks.get(i..colon).unwrap_or_default()).unwrap_or_default();
            i = colon + 1;
            let mut members = Vec::new();
            loop {
                let end = entry_end(&toks, i);
                if let Some(m) = parse_mailbox_tokens(toks.get(i..end).unwrap_or_default())
                    && members.len() < MAX_ADDRESSES
                {
                    members.push(m);
                }
                i = end;
                match toks.get(i) {
                    Some(Token { tok: Tok::Special(','), .. }) => i += 1,
                    Some(_) => {
                        i += 1;
                        break;
                    }
                    None => break,
                }
            }
            out.push(Address::Group { name, members });
        } else {
            let end = entry_end(&toks, i);
            if let Some(m) = parse_mailbox_tokens(toks.get(i..end).unwrap_or_default()) {
                out.push(Address::Mailbox(m));
            }
            i = end;
        }
    }
    out
}

/// Parses a single mailbox (`Name <addr>`, `addr`, `addr (Name)`); `None` when no address is found.
pub fn parse_mailbox(input: &str) -> Option<Mailbox> {
    parse_address_list(input).into_iter().flat_map(|a| a.mailboxes()).next()
}

/// Flattens an address list to its mailboxes (groups expanded).
pub fn mailboxes(list: &[Address]) -> Vec<Mailbox> {
    list.iter().flat_map(Address::mailboxes).collect()
}

/// If the entry starting at `i` is a group (`phrase :`), the index of its colon.
fn group_colon(toks: &[Token], i: usize) -> Option<usize> {
    let mut j = i;
    while let Some(t) = toks.get(j) {
        match &t.tok {
            Tok::Special(':') => return Some(j),
            Tok::Special(_) if !matches!(t.tok, Tok::Special('.')) => return None,
            _ => j += 1,
        }
    }
    None
}

/// End (exclusive) of the mailbox entry starting at `i`: the next `,` or `;` outside angle brackets. Commas inside
/// brackets that belong to an obsolete route (`<@a,@b:x@y>`) do not end the entry; other commas do (missing `>`).
fn entry_end(toks: &[Token], i: usize) -> usize {
    let mut in_angle = false;
    let mut j = i;
    while let Some(t) = toks.get(j) {
        match t.tok {
            Tok::Special('<') => in_angle = true,
            Tok::Special('>') => in_angle = false,
            Tok::Special(';') => return j,
            Tok::Special(',') => {
                let route = in_angle && (is_sp(toks.get(j + 1), '@') || is_sp(toks.get(j + 1), ','));
                if !route {
                    return j;
                }
            }
            _ => {}
        }
        j += 1;
    }
    j
}

fn parse_mailbox_tokens(entry: &[Token]) -> Option<Mailbox> {
    let comments: Vec<&str> = entry
        .iter()
        .filter_map(|t| match &t.tok {
            Tok::Comment(c) => Some(c.as_str()),
            _ => None,
        })
        .collect();
    let toks: Vec<Token> = entry.iter().filter(|t| !matches!(t.tok, Tok::Comment(_))).cloned().collect();
    if toks.is_empty() {
        return None;
    }
    if let Some(lt) = toks.iter().position(|t| t.tok == Tok::Special('<')) {
        let phrase = toks.get(..lt).unwrap_or_default();
        let rest = toks.get(lt + 1..).unwrap_or_default();
        let inner_end = rest.iter().position(|t| t.tok == Tok::Special('>')).unwrap_or(rest.len());
        let mut inner = rest.get(..inner_end).unwrap_or_default();
        // Obsolete route: drop everything up to the last ':'.
        if let Some(c) = inner.iter().rposition(|t| t.tok == Tok::Special(':')) {
            inner = inner.get(c + 1..).unwrap_or_default();
        }
        let addr = addr_spec(inner);
        if addr.is_empty() {
            return None;
        }
        let name = phrase_text(phrase).or_else(|| comment_name(&comments));
        return Some(Mailbox { name, addr });
    }
    let at = toks.iter().rposition(|t| t.tok == Tok::Special('@'))?;
    if is_addr_spec(&toks) {
        return Some(Mailbox { name: comment_name(&comments), addr: addr_spec(&toks) });
    }
    // Missing angle brackets (`John Doe john@example.com`): the address is the whitespace-free run of tokens
    // around the last '@'; whatever precedes it is the display name.
    let mut start = at.saturating_sub(1);
    if at == 0 {
        start = 0;
    }
    while start > 0 && toks.get(start).is_some_and(|t| !t.ws) {
        start -= 1;
    }
    let mut end = at + 1;
    while toks.get(end + 1).is_some_and(|t| !t.ws) {
        end += 1;
    }
    let addr = addr_spec(toks.get(start..=end.min(toks.len().saturating_sub(1))).unwrap_or_default());
    if !addr.contains('@') || addr.starts_with('@') || addr.ends_with('@') {
        return None;
    }
    let name = phrase_text(toks.get(..start).unwrap_or_default()).or_else(|| comment_name(&comments));
    Some(Mailbox { name, addr })
}

/// True when `toks` is exactly `word *("." word) "@" (word *("." word) / domain-literal)`.
fn is_addr_spec(toks: &[Token]) -> bool {
    let word = |t: Option<&Token>| matches!(t, Some(Token { tok: Tok::Atom(_) | Tok::Quoted(_), .. }));
    let mut i = 0usize;
    if !word(toks.get(i)) {
        return false;
    }
    i += 1;
    while is_sp(toks.get(i), '.') {
        if !word(toks.get(i + 1)) {
            return false;
        }
        i += 2;
    }
    if !is_sp(toks.get(i), '@') {
        return false;
    }
    i += 1;
    if matches!(toks.get(i), Some(Token { tok: Tok::DomainLiteral(_), .. })) {
        return i + 1 == toks.len();
    }
    if !matches!(toks.get(i), Some(Token { tok: Tok::Atom(_), .. })) {
        return false;
    }
    i += 1;
    while is_sp(toks.get(i), '.') {
        if !matches!(toks.get(i + 1), Some(Token { tok: Tok::Atom(_), .. })) {
            return false;
        }
        i += 2;
    }
    i == toks.len()
}

/// RFC 5322 atext (plus any non-ASCII character, RFC 6532).
fn is_atext(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~".contains(c) || !c.is_ascii()
}

/// Joins address tokens without whitespace. Quoted local parts stay quoted unless they are a plain dot-atom.
fn addr_spec(toks: &[Token]) -> String {
    let mut s = String::new();
    for t in toks {
        match &t.tok {
            Tok::Atom(a) => s.push_str(a),
            Tok::Quoted(q) => {
                let dot_atom =
                    !q.is_empty() && q.split('.').all(|p| !p.is_empty() && p.chars().all(is_atext));
                if dot_atom {
                    s.push_str(q);
                } else {
                    s.push('"');
                    s.push_str(&crate::params::quote_escape(q));
                    s.push('"');
                }
            }
            Tok::DomainLiteral(d) => {
                s.push('[');
                s.push_str(d);
                s.push(']');
            }
            Tok::Special(c @ ('.' | '@')) => s.push(*c),
            _ => {}
        }
    }
    s.trim().to_owned()
}

/// Display name from phrase tokens: words joined by single spaces, quoted strings verbatim, RFC 2047 words
/// decoded, surrounding single quotes (`'John Doe'`) removed from unquoted names. `None` when empty.
fn phrase_text(toks: &[Token]) -> Option<String> {
    let mut raw = String::new();
    let single_quoted = toks.len() == 1 && matches!(toks.first(), Some(Token { tok: Tok::Quoted(_), .. }));
    for t in toks {
        let piece: &str = match &t.tok {
            Tok::Atom(a) => a,
            Tok::Quoted(q) => q,
            Tok::Comment(_) => continue,
            Tok::DomainLiteral(d) => d,
            Tok::Special(c) => {
                if t.ws && !raw.is_empty() {
                    raw.push(' ');
                }
                raw.push(*c);
                continue;
            }
        };
        if t.ws && !raw.is_empty() {
            raw.push(' ');
        }
        raw.push_str(piece);
    }
    let mut trimmed = raw.trim();
    if !single_quoted && trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        trimmed = trimmed.get(1..trimmed.len() - 1).unwrap_or(trimmed).trim();
    }
    let decoded = crate::encoded::decode_words(trimmed);
    let decoded = decoded.trim();
    if decoded.is_empty() { None } else { Some(decoded.to_owned()) }
}

fn comment_name(comments: &[&str]) -> Option<String> {
    let c = comments.iter().map(|c| c.trim()).find(|c| !c.is_empty())?;
    let d = crate::encoded::decode_words(c);
    let d = d.trim();
    if d.is_empty() { None } else { Some(d.to_owned()) }
}

/// True when `name` can be written as a bare phrase (atoms separated by single spaces).
fn is_plain_phrase(name: &str, allow_utf8: bool) -> bool {
    !name.is_empty()
        && !name.contains("=?")
        && !(name.starts_with('\'') && name.ends_with('\''))
        && name
            .split(' ')
            .all(|w| !w.is_empty() && w.chars().all(|c| is_atext(c) && (allow_utf8 || c.is_ascii())))
}

/// Formats a display name for a header: bare when possible, quoted when it has specials, RFC 2047-encoded when
/// `encode` is true and it is not printable ASCII.
pub fn format_phrase(name: &str, encode: bool) -> String {
    if encode && crate::encoded::needs_encoding(name) {
        return crate::encoded::encode_phrase(name);
    }
    if is_plain_phrase(name, !encode) {
        return name.to_owned();
    }
    let clean: String = name.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    format!("\"{}\"", crate::params::quote_escape(&clean))
}

/// Formats one mailbox (`Name <addr>`, `addr`, or `<addr>` when the address alone would not read back as an
/// addr-spec, e.g. a bare local part such as `postmaster`).
pub fn format_mailbox(m: &Mailbox, encode: bool) -> String {
    let addr: String = m.addr.chars().filter(|c| !c.is_control()).collect();
    match m.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => format!("{} <{addr}>", format_phrase(n, encode)),
        _ if addr.is_empty() || is_addr_spec(&lex(&addr)) => addr,
        _ => format!("<{addr}>"),
    }
}

/// Formats addresses for a header value (quotes names when needed, RFC 2047-encodes non-ASCII names when
/// `encode` is true). Separator `", "`.
pub fn format_address_list(list: &[Address], encode: bool) -> String {
    list.iter()
        .map(|a| match a {
            Address::Mailbox(m) => format_mailbox(m, encode),
            Address::Group { name, members } => {
                let ms: Vec<String> = members.iter().map(|m| format_mailbox(m, encode)).collect();
                let label = if name.trim().is_empty() {
                    "undisclosed-recipients".to_owned()
                } else {
                    format_phrase(name.trim(), encode)
                };
                if ms.is_empty() { format!("{label}:;") } else { format!("{label}: {};", ms.join(", ")) }
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn mb(name: Option<&str>, addr: &str) -> Address {
        Address::Mailbox(Mailbox::new(name, addr))
    }

    #[test]
    fn simple_forms() {
        assert_eq!(parse_address_list("a@b.com"), vec![mb(None, "a@b.com")]);
        assert_eq!(parse_address_list("John Doe <john@x.com>"), vec![mb(Some("John Doe"), "john@x.com")]);
        assert_eq!(parse_address_list("<john@x.com>"), vec![mb(None, "john@x.com")]);
        assert_eq!(
            parse_address_list("\"Doe, John\" <john@x.com>"),
            vec![mb(Some("Doe, John"), "john@x.com")]
        );
        assert_eq!(parse_address_list("  "), vec![]);
        assert_eq!(parse_address_list(""), vec![]);
    }

    #[test]
    fn lists_and_separators() {
        let l = parse_address_list("a@x.com, \"B b\" <b@y.com>,c@z.com");
        assert_eq!(l, vec![mb(None, "a@x.com"), mb(Some("B b"), "b@y.com"), mb(None, "c@z.com")]);
        let l = parse_address_list("a@x.com; b@y.com;; Carl <c@z.com>;");
        assert_eq!(l, vec![mb(None, "a@x.com"), mb(None, "b@y.com"), mb(Some("Carl"), "c@z.com")]);
        let l = parse_address_list(",,a@x.com,,");
        assert_eq!(l, vec![mb(None, "a@x.com")]);
    }

    #[test]
    fn comments_and_rfc5322_appendix() {
        assert_eq!(parse_address_list("john@x.com (John Doe)"), vec![mb(Some("John Doe"), "john@x.com")]);
        assert_eq!(
            parse_address_list("Pete(A nice \\) chap) <pete(his account)@silly.test(his host)>"),
            vec![mb(Some("Pete"), "pete@silly.test")]
        );
        assert_eq!(
            parse_address_list("Mary Smith <mary@x.test>, jdoe@example.org, Who? <one@y.test>"),
            vec![
                mb(Some("Mary Smith"), "mary@x.test"),
                mb(None, "jdoe@example.org"),
                mb(Some("Who?"), "one@y.test")
            ]
        );
        assert_eq!(
            parse_address_list("<boss@nil.test>, \"Giant; \\\"Big\\\" Box\" <sysservices@example.net>"),
            vec![mb(None, "boss@nil.test"), mb(Some("Giant; \"Big\" Box"), "sysservices@example.net")]
        );
        assert_eq!(
            parse_address_list("Joe Q. Public <john.q.public@example.com>"),
            vec![mb(Some("Joe Q. Public"), "john.q.public@example.com")]
        );
    }

    #[test]
    fn groups() {
        let l = parse_address_list("A Group:Ed Jones <c@a.test>,joe@where.test,John <jdoe@one.test>;");
        assert_eq!(
            l,
            vec![Address::Group {
                name: "A Group".into(),
                members: vec![
                    Mailbox::new(Some("Ed Jones"), "c@a.test"),
                    Mailbox::new(None, "joe@where.test"),
                    Mailbox::new(Some("John"), "jdoe@one.test"),
                ],
            }]
        );
        let l = parse_address_list("Undisclosed recipients:;");
        assert_eq!(l, vec![Address::Group { name: "Undisclosed recipients".into(), members: vec![] }]);
        let l = parse_address_list("g: a@x.com; b@y.com");
        assert_eq!(l.len(), 2);
        assert_eq!(l.first().map(|a| a.mailboxes().len()), Some(1));
        let l = parse_address_list("g: a@x.com, b@y.com");
        assert_eq!(mailboxes(&l).len(), 2);
    }

    #[test]
    fn obsolete_syntax() {
        assert_eq!(parse_address_list("<@route1,@route2:joe@x.com>"), vec![mb(None, "joe@x.com")]);
        assert_eq!(parse_address_list("john . doe @ example . com"), vec![mb(None, "john.doe@example.com")]);
        assert_eq!(parse_address_list("\"john doe\"@x.com"), vec![mb(None, "\"john doe\"@x.com")]);
        assert_eq!(parse_address_list("\"john.doe\"@x.com"), vec![mb(None, "john.doe@x.com")]);
        assert_eq!(parse_address_list("x@[192.168.0.1]"), vec![mb(None, "x@[192.168.0.1]")]);
    }

    #[test]
    fn missing_brackets_and_junk() {
        assert_eq!(parse_address_list("John Doe john@x.com"), vec![mb(Some("John Doe"), "john@x.com")]);
        assert_eq!(parse_address_list("John <john@x.com"), vec![mb(Some("John"), "john@x.com")]);
        assert_eq!(
            parse_address_list("John <john@x.com, Mary <m@y.com>"),
            vec![mb(Some("John"), "john@x.com"), mb(Some("Mary"), "m@y.com")]
        );
        assert_eq!(parse_address_list("just words, a@b.c"), vec![mb(None, "a@b.c")]);
        assert_eq!(parse_address_list("<>"), vec![]);
        assert_eq!(parse_address_list("'John Doe' <j@x.com>"), vec![mb(Some("John Doe"), "j@x.com")]);
        assert_eq!(parse_address_list("j@x.com <j@x.com>"), vec![mb(Some("j@x.com"), "j@x.com")]);
        assert_eq!(parse_address_list("<postmaster>"), vec![mb(None, "postmaster")]);
        assert_eq!(parse_address_list("@"), vec![]);
    }

    #[test]
    fn encoded_names() {
        assert_eq!(parse_address_list("=?utf-8?q?Ren=C3=A9?= <r@x.com>"), vec![mb(Some("René"), "r@x.com")]);
        assert_eq!(
            parse_address_list("\"=?utf-8?q?Ren=C3=A9?=\" <r@x.com>"),
            vec![mb(Some("René"), "r@x.com")]
        );
        assert_eq!(
            parse_address_list("=?ISO-8859-1?Q?Keld_J=F8rn_Simonsen?= <keld@x.test>"),
            vec![mb(Some("Keld Jørn Simonsen"), "keld@x.test")]
        );
        assert_eq!(parse_address_list("René <r@x.com>"), vec![mb(Some("René"), "r@x.com")]);
    }

    #[test]
    fn formatting() {
        let l = vec![mb(Some("John Doe"), "j@x.com"), mb(None, "a@b.c"), mb(Some("Doe, John"), "d@x.com")];
        assert_eq!(format_address_list(&l, true), "John Doe <j@x.com>, a@b.c, \"Doe, John\" <d@x.com>");
        let l = vec![mb(Some("René"), "r@x.com")];
        assert_eq!(format_address_list(&l, false), "René <r@x.com>");
        let e = format_address_list(&l, true);
        assert!(e.starts_with("=?UTF-8?"), "{e}");
        assert_eq!(parse_address_list(&e), l);
        let g = vec![Address::Group { name: "Team".into(), members: vec![Mailbox::new(None, "a@x.com")] }];
        assert_eq!(format_address_list(&g, true), "Team: a@x.com;");
        let g = vec![Address::Group { name: String::new(), members: vec![] }];
        assert_eq!(format_address_list(&g, true), "undisclosed-recipients:;");
        assert_eq!(format_phrase("O'Brien", true), "O'Brien");
        assert_eq!(format_phrase("'quoted'", true), "\"'quoted'\"");
        assert_eq!(format_phrase("a\"b", true), "\"a\\\"b\"");
        assert_eq!(format_phrase("Dr. Who", true), "\"Dr. Who\"");
    }

    #[test]
    fn mailbox_helpers() {
        assert_eq!(parse_mailbox("X <x@y.z>"), Some(Mailbox::new(Some("X"), "x@y.z")));
        assert_eq!(parse_mailbox("nothing"), None);
        let a = Mailbox::new(None, "A@X.com");
        assert!(a.same_address(&Mailbox::new(Some("n"), "a@x.COM")));
        assert_eq!(a.display(), "A@X.com");
        assert_eq!(Mailbox::new(Some("N"), "a@x").to_display_string(), "N <a@x>");
    }

    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z0-9 ,.;:<>@\"'\\\\()éü日本✓\t-]{1,30}"
            .prop_map(|s| s.trim().to_owned())
            .prop_filter("non-empty", |s| !s.is_empty())
    }

    proptest! {
        #[test]
        fn format_parse_round_trip(
            entries in proptest::collection::vec((proptest::option::of(name_strategy()), "[a-z0-9]{1,8}(\\.[a-z0-9]{1,5})?@[a-z]{1,8}\\.[a-z]{2,3}"), 0..6),
            encode in any::<bool>(),
        ) {
            let list: Vec<Address> = entries.iter().map(|(n, a)| mb(n.as_deref(), a)).collect();
            let s = format_address_list(&list, encode);
            prop_assert_eq!(parse_address_list(&s), list);
        }

        #[test]
        fn never_panics(s in "\\PC{0,200}") {
            let _ = parse_address_list(&s);
        }

        #[test]
        fn never_panics_structured(s in "([a-z@.,;:<>\"()\\[\\] \\\\]|=\\?utf-8\\?q\\?|\\?=){0,60}") {
            let l = parse_address_list(&s);
            let _ = format_address_list(&l, true);
        }
    }
}
