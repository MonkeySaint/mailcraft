//! IMAP command grammar (RFC 9051 section 9, the parts this server needs). Commands arrive fully assembled
//! (literal data inline, in wire format), so literals are parsed from the same buffer.

use crate::date::{DateTime, parse_imap_date};
use crate::mime::{Section, SectionText};

pub type PResult<T> = Result<T, String>;

/// Largest literal accepted inside a command.
pub const MAX_LITERAL: usize = 64 * 1024 * 1024;

pub struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqNum {
    Num(u32),
    Star,
}

/// A sequence set: ranges with `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeqSet(pub Vec<(SeqNum, SeqNum)>);

impl SeqSet {
    /// Whether `n` is in the set, where `*` stands for `max`.
    pub fn contains(&self, n: u32, max: u32) -> bool {
        let v = |s: SeqNum| match s {
            SeqNum::Num(x) => x,
            SeqNum::Star => max,
        };
        self.0.iter().any(|&(a, b)| {
            let (lo, hi) = (v(a).min(v(b)), v(a).max(v(b)));
            (lo..=hi).contains(&n)
        })
    }
}

/// Compact `1:3,5` form of sorted numbers.
pub fn format_set(nums: &[u32]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(&start) = nums.get(i) {
        let mut end = start;
        let mut j = i + 1;
        while let Some(&n) = nums.get(j) {
            if Some(n) == end.checked_add(1) {
                end = n;
                j += 1;
            } else {
                break;
            }
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i = j;
    }
    out
}

fn is_atom_char(b: u8) -> bool {
    b > b' ' && b < 0x7f && !b"(){%*\"\\]".contains(&b)
}

impl<'a> Parser<'a> {
    pub fn new(b: &'a [u8]) -> Parser<'a> {
        Parser { b, pos: 0 }
    }

    pub fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    pub fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, c: u8) -> PResult<()> {
        if self.eat(c) { Ok(()) } else { Err(format!("expected `{}`", c as char)) }
    }

    pub fn sp(&mut self) -> PResult<()> {
        self.expect(b' ')
    }

    /// Whether only the line ending is left.
    pub fn at_end(&self) -> bool {
        matches!(self.b.get(self.pos..), Some(b"\r\n") | Some(b"\n") | Some(b""))
    }

    pub fn end(&self) -> PResult<()> {
        if self.at_end() { Ok(()) } else { Err("unexpected extra arguments".to_owned()) }
    }

    /// Case-insensitive keyword match; consumes it if present (must not be followed by an atom char).
    pub fn keyword(&mut self, kw: &str) -> bool {
        let end = self.pos + kw.len();
        match self.b.get(self.pos..end) {
            Some(s) if s.eq_ignore_ascii_case(kw.as_bytes()) => {
                if self.b.get(end).is_some_and(|&c| is_atom_char(c) && c != b'[') {
                    return false;
                }
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    /// Tag or atom-like word up to SP / end, allowing `]` and extra chars (tags, command names).
    pub fn word(&mut self) -> PResult<&'a [u8]> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c <= b' ' || c >= 0x7f || c == b'(' || c == b')' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected a word".to_owned());
        }
        Ok(self.b.get(start..self.pos).unwrap_or(&[]))
    }

    pub fn atom(&mut self) -> PResult<&'a [u8]> {
        let start = self.pos;
        while self.peek().is_some_and(is_atom_char) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected an atom".to_owned());
        }
        Ok(self.b.get(start..self.pos).unwrap_or(&[]))
    }

    pub fn number(&mut self) -> PResult<u64> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let s = std::str::from_utf8(self.b.get(start..self.pos).unwrap_or(&[])).unwrap_or("");
        s.parse().map_err(|_| "expected a number".to_owned())
    }

    pub fn nz_number32(&mut self) -> PResult<u32> {
        let n = self.number()?;
        match u32::try_from(n) {
            Ok(v) if v > 0 => Ok(v),
            _ => Err("number out of range".to_owned()),
        }
    }

    fn quoted(&mut self) -> PResult<Vec<u8>> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\r') | Some(b'\n') => return Err("unterminated quoted string".to_owned()),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = self.peek().ok_or("bad escape")?;
                    out.push(c);
                    self.pos += 1;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn literal(&mut self) -> PResult<Vec<u8>> {
        self.eat(b'~'); // literal8
        self.expect(b'{')?;
        let n = usize::try_from(self.number()?).map_err(|_| "literal too large".to_owned())?;
        if n > MAX_LITERAL {
            return Err("literal too large".to_owned());
        }
        self.eat(b'+');
        self.expect(b'}')?;
        self.eat(b'\r');
        self.expect(b'\n')?;
        let end = self.pos.checked_add(n).ok_or("literal too large")?;
        let data = self.b.get(self.pos..end).ok_or("truncated literal")?.to_vec();
        self.pos = end;
        Ok(data)
    }

    pub fn string(&mut self) -> PResult<Vec<u8>> {
        match self.peek() {
            Some(b'"') => self.quoted(),
            Some(b'{') | Some(b'~') => self.literal(),
            _ => Err("expected a string".to_owned()),
        }
    }

    pub fn astring(&mut self) -> PResult<Vec<u8>> {
        match self.peek() {
            Some(b'"') | Some(b'{') => self.string(),
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|c| is_atom_char(c) || c == b']') {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err("expected an astring".to_owned());
                }
                Ok(self.b.get(start..self.pos).unwrap_or(&[]).to_vec())
            }
        }
    }

    pub fn astring_utf8(&mut self) -> PResult<String> {
        String::from_utf8(self.astring()?).map_err(|_| "invalid UTF-8".to_owned())
    }

    /// `list-mailbox`: like astring but `%` and `*` allowed.
    pub fn list_mailbox(&mut self) -> PResult<String> {
        let v = match self.peek() {
            Some(b'"') | Some(b'{') => self.string()?,
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|c| is_atom_char(c) || c == b'%' || c == b'*' || c == b']') {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err("expected a mailbox pattern".to_owned());
                }
                self.b.get(start..self.pos).unwrap_or(&[]).to_vec()
            }
        };
        String::from_utf8(v).map_err(|_| "invalid UTF-8".to_owned())
    }

    pub fn seq_set(&mut self) -> PResult<SeqSet> {
        let mut ranges = Vec::new();
        loop {
            let a = self.seq_num()?;
            let b = if self.eat(b':') { self.seq_num()? } else { a };
            ranges.push((a, b));
            if ranges.len() > 10_000 {
                return Err("sequence set too long".to_owned());
            }
            if !self.eat(b',') {
                break;
            }
        }
        Ok(SeqSet(ranges))
    }

    fn seq_num(&mut self) -> PResult<SeqNum> {
        if self.eat(b'*') {
            return Ok(SeqNum::Star);
        }
        if self.peek() == Some(b'$') {
            return Err("search result references are not supported".to_owned());
        }
        Ok(SeqNum::Num(self.nz_number32()?))
    }

    /// `(flag flag ...)`.
    pub fn flag_list(&mut self) -> PResult<Vec<String>> {
        self.expect(b'(')?;
        let mut flags = Vec::new();
        while !self.eat(b')') {
            if !flags.is_empty() {
                self.sp()?;
            }
            flags.push(self.flag()?);
            if flags.len() > 100 {
                return Err("too many flags".to_owned());
            }
        }
        Ok(flags)
    }

    pub fn flag(&mut self) -> PResult<String> {
        let start = self.pos;
        self.eat(b'\\');
        if self.eat(b'*') {
            return Ok("\\*".to_owned());
        }
        self.atom()?;
        Ok(String::from_utf8_lossy(self.b.get(start..self.pos).unwrap_or(&[])).into_owned())
    }

    /// `date` (day number), quoted or bare.
    pub fn date(&mut self) -> PResult<i64> {
        let s = if self.peek() == Some(b'"') { self.quoted()? } else { self.atom()?.to_vec() };
        parse_imap_date(&String::from_utf8_lossy(&s)).ok_or_else(|| "invalid date".to_owned())
    }

    /// Quoted `date-time`.
    pub fn date_time(&mut self) -> PResult<DateTime> {
        let s = self.quoted()?;
        DateTime::parse_imap(&String::from_utf8_lossy(&s)).ok_or_else(|| "invalid date-time".to_owned())
    }

    /// Section spec after `[` up to and including `]`.
    pub fn section(&mut self) -> PResult<Section> {
        self.expect(b'[')?;
        let mut path = Vec::new();
        let mut text = SectionText::Full;
        if self.eat(b']') {
            return Ok(Section { path, text });
        }
        loop {
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                path.push(self.nz_number32()?);
                if path.len() > 32 {
                    return Err("section path too deep".to_owned());
                }
                if self.eat(b'.') {
                    continue;
                }
                break;
            }
            if self.keyword("HEADER.FIELDS.NOT") || self.keyword("HEADER.FIELDS") {
                let not = self
                    .b
                    .get(self.pos.saturating_sub(4)..self.pos)
                    .is_some_and(|s| s.eq_ignore_ascii_case(b".NOT"));
                self.sp()?;
                self.expect(b'(')?;
                let mut names = Vec::new();
                while !self.eat(b')') {
                    if !names.is_empty() {
                        self.sp()?;
                    }
                    names.push(String::from_utf8_lossy(&self.astring()?).into_owned());
                    if names.len() > 200 {
                        return Err("too many header names".to_owned());
                    }
                }
                text = SectionText::HeaderFields { names, not };
            } else if self.keyword("HEADER") {
                text = SectionText::Header;
            } else if self.keyword("TEXT") {
                text = SectionText::Text;
            } else if self.keyword("MIME") {
                if path.is_empty() {
                    return Err("MIME needs a part number".to_owned());
                }
                text = SectionText::Mime;
            } else {
                return Err("bad section".to_owned());
            }
            break;
        }
        self.expect(b']')?;
        Ok(Section { path, text })
    }

    /// Optional `<origin.count>`.
    pub fn partial(&mut self) -> PResult<Option<(u64, u64)>> {
        if !self.eat(b'<') {
            return Ok(None);
        }
        let o = self.number()?;
        self.expect(b'.')?;
        let n = self.number()?;
        if n == 0 {
            return Err("partial count must be non-zero".to_owned());
        }
        self.expect(b'>')?;
        Ok(Some((o, n)))
    }
}

/// If `line` ends with a literal announcement `{n}` / `{n+}` / `~{n}`, returns (n, synchronising).
pub fn trailing_literal(line: &[u8]) -> Option<(usize, bool)> {
    let line = line.strip_suffix(b"\n")?;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let inner = line.strip_suffix(b"}")?;
    let open = inner.iter().rposition(|&b| b == b'{')?;
    let spec = inner.get(open + 1..)?;
    let (digits, plus) = match spec.strip_suffix(b"+") {
        Some(d) => (d, true),
        None => (spec, false),
    };
    if digits.is_empty() || digits.len() > 12 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((n, !plus))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_sets() {
        let mut p = Parser::new(b"\"a\\\"b\" {3}\r\nxyz atom 1:3,5,7:* (\\Seen $Junk)\r\n");
        assert_eq!(p.astring().unwrap(), b"a\"b");
        p.sp().unwrap();
        assert_eq!(p.astring().unwrap(), b"xyz");
        p.sp().unwrap();
        assert_eq!(p.astring().unwrap(), b"atom");
        p.sp().unwrap();
        let s = p.seq_set().unwrap();
        assert!(s.contains(2, 10) && s.contains(5, 10) && s.contains(9, 10) && !s.contains(4, 10));
        p.sp().unwrap();
        assert_eq!(p.flag_list().unwrap(), vec!["\\Seen".to_owned(), "$Junk".to_owned()]);
        assert!(p.at_end());
        assert_eq!(format_set(&[1, 2, 3, 5, 7, 8]), "1:3,5,7:8");
        assert_eq!(trailing_literal(b"A1 APPEND INBOX {12}\r\n"), Some((12, true)));
        assert_eq!(trailing_literal(b"A1 APPEND INBOX {12+}\r\n"), Some((12, false)));
        assert_eq!(trailing_literal(b"A1 NOOP\r\n"), None);
    }

    #[test]
    fn sections() {
        let mut p = Parser::new(b"[1.2.HEADER.FIELDS.NOT (Subject From)]<0.100>");
        let s = p.section().unwrap();
        assert_eq!(s.path, vec![1, 2]);
        assert_eq!(s.spec(), "1.2.HEADER.FIELDS.NOT (SUBJECT FROM)");
        assert_eq!(p.partial().unwrap(), Some((0, 100)));
        assert!(Parser::new(b"[MIME]").section().is_err());
        assert!(Parser::new(b"[BOGUS]").section().is_err());
        assert_eq!(Parser::new(b"[]").section().unwrap().spec(), "");
    }

    #[test]
    fn junk_never_panics() {
        for junk in
            [&b""[..], b"{", b"{99999999999999999999}\r\n", b"\"", b"~{1}\r\n", b"[1.", b"<1.", b"*:*,"]
        {
            let mut p = Parser::new(junk);
            let _ = p.astring();
            let _ = p.seq_set();
            let _ = p.section();
            let _ = p.partial();
            let _ = p.flag_list();
            let _ = p.date();
        }
    }
}
