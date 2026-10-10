//! Shared text helpers: byte decoding, content lines (RFC 5545 §3.1 / RFC 6350 §3.3), folding, TEXT escapes.

/// Most content lines read from one iCalendar / vCard input.
pub const MAX_CONTENT_LINES: usize = 1_000_000;
/// Longest unfolded content line kept (longer lines are truncated at a char boundary).
pub const MAX_LINE_CHARS: usize = 1 << 20;
/// Most parameters kept on one content line.
pub const MAX_PARAMS: usize = 64;
/// Most values kept for one parameter.
pub const MAX_PARAM_VALUES: usize = 64;
/// Most categories kept per item.
pub const MAX_CATEGORIES: usize = 256;

/// A category list: trimmed, non-empty, unique ignoring ASCII case, first spelling kept, capped at
/// [`MAX_CATEGORIES`]. Duplicates are found through a set, so long hostile lists cost linear time.
#[derive(Default)]
pub struct CategoryList {
    list: Vec<String>,
    seen: std::collections::HashSet<String>,
}

impl CategoryList {
    /// True once the cap is reached (further values are ignored).
    pub fn is_full(&self) -> bool {
        self.list.len() >= MAX_CATEGORIES
    }

    pub fn add(&mut self, cat: &str) {
        let cat = cat.trim();
        if cat.is_empty() || self.is_full() {
            return;
        }
        if self.seen.insert(cat.to_ascii_lowercase()) {
            self.list.push(cat.to_owned());
        }
    }

    pub fn into_vec(self) -> Vec<String> {
        self.list
    }
}

/// Decodes text file bytes: UTF-8 (with or without BOM), UTF-16 with BOM, else Windows-1252.
pub fn decode_bytes(data: &[u8]) -> String {
    if let Some(rest) = data.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if data.starts_with(b"\xFF\xFE") || data.starts_with(b"\xFE\xFF") {
        let (text, _, _) = encoding_rs::UTF_16LE.decode(data);
        return text.into_owned();
    }
    match std::str::from_utf8(data) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            let (text, _, _) = encoding_rs::WINDOWS_1252.decode(data);
            text.into_owned()
        }
    }
}

/// Truncates to at most `max` chars.
pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_owned(),
        None => s.to_owned(),
    }
}

/// Splits into logical lines and unfolds continuation lines (a line break followed by a space or tab).
/// Accepts CRLF, LF and bare CR line breaks. Blank lines are dropped.
pub fn unfold(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let normal = input.replace("\r\n", "\n").replace('\r', "\n");
    for raw in normal.split('\n') {
        if let Some(cont) = raw.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            if last.len() < MAX_LINE_CHARS {
                last.push_str(cont);
            }
            continue;
        }
        if raw.trim().is_empty() {
            continue;
        }
        if out.len() >= MAX_CONTENT_LINES {
            break;
        }
        out.push(raw.to_owned());
    }
    for line in &mut out {
        if line.len() > MAX_LINE_CHARS {
            *line = clip(line, MAX_LINE_CHARS);
        }
    }
    out
}

/// Folds one content line at 75 octets (never inside a UTF-8 sequence) and appends it with CRLF.
pub fn fold_into(out: &mut String, line: &str) {
    let mut used = 0usize;
    for c in line.chars() {
        let len = c.len_utf8();
        if used + len > 75 {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used += len;
    }
    out.push_str("\r\n");
}

/// One parsed content line: `[group.]NAME;PARAM=V1,V2:value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentLine {
    /// vCard group (`item1` in `item1.EMAIL`), if any.
    pub group: Option<String>,
    /// Upper-cased property name.
    pub name: String,
    /// (upper-cased name, values). A bare vCard 2.1 parameter (`TEL;WORK:`) becomes `TYPE=WORK`.
    pub params: Vec<(String, Vec<String>)>,
    /// Raw value (escapes not yet decoded).
    pub value: String,
}

impl ContentLine {
    /// Parses a line; `None` when it has no `:` or no name.
    pub fn parse(line: &str) -> Option<ContentLine> {
        let mut chars = line.char_indices().peekable();
        // Name (with an optional group prefix): up to ';' or ':'.
        let mut name_end = None;
        for (i, c) in chars.by_ref() {
            if c == ';' || c == ':' {
                name_end = Some((i, c));
                break;
            }
        }
        let (end, mut sep) = name_end?;
        let full_name = line.get(..end)?.trim();
        if full_name.is_empty() {
            return None;
        }
        let (group, name) = match full_name.rsplit_once('.') {
            Some((g, n)) => (Some(g.to_owned()), n),
            None => (None, full_name),
        };
        let mut params: Vec<(String, Vec<String>)> = Vec::new();
        let mut pos = end + 1;
        while sep == ';' {
            // Parameter name.
            let rest = line.get(pos..)?;
            let mut pname_end = None;
            for (i, c) in rest.char_indices() {
                if c == '=' || c == ';' || c == ':' {
                    pname_end = Some((i, c));
                    break;
                }
            }
            let (i, c) = pname_end?;
            let pname = rest.get(..i)?.trim().to_ascii_uppercase();
            pos += i + 1;
            if c != '=' {
                // vCard 2.1 bare parameter.
                if !pname.is_empty() && params.len() < MAX_PARAMS {
                    if pname == "QUOTED-PRINTABLE" || pname == "BASE64" || pname == "B" {
                        params.push(("ENCODING".into(), vec![pname]));
                    } else {
                        params.push(("TYPE".into(), vec![pname]));
                    }
                }
                sep = c;
                continue;
            }
            // Values.
            let mut values = Vec::new();
            loop {
                let rest = line.get(pos..)?;
                let (value, consumed, next) = parse_param_value(rest)?;
                pos += consumed;
                if values.len() < MAX_PARAM_VALUES {
                    values.push(value);
                }
                match next {
                    ',' => {
                        pos += 1;
                        continue;
                    }
                    c => {
                        pos += 1;
                        sep = c;
                        break;
                    }
                }
            }
            if params.len() < MAX_PARAMS {
                params.push((pname, values));
            }
        }
        let value = line.get(pos..).unwrap_or("").to_owned();
        Some(ContentLine { group, name: name.to_ascii_uppercase(), params, value })
    }

    /// All values of parameter `name` (case-insensitive).
    pub fn param_values(&self, name: &str) -> Vec<&str> {
        self.params
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .flat_map(|(_, v)| v.iter().map(String::as_str))
            .collect()
    }

    /// First value of parameter `name`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.param_values(name).into_iter().next()
    }

    /// True when any TYPE value (split on commas too) equals `t` case-insensitively.
    pub fn has_type(&self, t: &str) -> bool {
        self.param_values("TYPE").iter().flat_map(|v| v.split(',')).any(|v| v.trim().eq_ignore_ascii_case(t))
    }

    /// Serialises `NAME;P=V:value` (the value must already be escaped) and folds it into `out`.
    pub fn write(&self, out: &mut String) {
        let mut line = String::new();
        if let Some(g) = &self.group {
            line.push_str(g);
            line.push('.');
        }
        line.push_str(&self.name);
        for (name, values) in &self.params {
            line.push(';');
            line.push_str(name);
            line.push('=');
            let vals: Vec<String> = values.iter().map(|v| quote_param(v)).collect();
            line.push_str(&vals.join(","));
        }
        line.push(':');
        line.push_str(&self.value);
        fold_into(out, &line);
    }
}

/// Parses one parameter value at the start of `s`; returns (decoded value, bytes consumed, next delimiter).
fn parse_param_value(s: &str) -> Option<(String, usize, char)> {
    if let Some(rest) = s.strip_prefix('"') {
        let close = rest.find('"')?;
        let value = rest.get(..close)?;
        let after = 1 + close + 1;
        let next = s.get(after..)?.chars().next()?;
        if next != ',' && next != ';' && next != ':' {
            // Junk after a quoted value: skip to the next delimiter.
            let tail = s.get(after..)?;
            let (j, c) = tail.char_indices().find(|(_, c)| matches!(c, ',' | ';' | ':'))?;
            return Some((caret_decode(value), after + j, c));
        }
        return Some((caret_decode(value), after, next));
    }
    let (i, c) = s.char_indices().find(|(_, c)| matches!(c, ',' | ';' | ':'))?;
    Some((caret_decode(s.get(..i)?.trim()), i, c))
}

/// RFC 6868 parameter value encoding: `^n` newline, `^^` caret, `^'` double quote.
fn caret_decode(s: &str) -> String {
    if !s.contains('^') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '^' {
            match it.peek() {
                Some('n') => {
                    out.push('\n');
                    it.next();
                }
                Some('^') => {
                    out.push('^');
                    it.next();
                }
                Some('\'') => {
                    out.push('"');
                    it.next();
                }
                _ => out.push('^'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Quotes a parameter value when it contains `,;:` or a quote/newline (encoded per RFC 6868).
pub fn quote_param(v: &str) -> String {
    let needs_caret = v.contains(['"', '\n', '\r', '^']);
    let enc: String = if needs_caret {
        v.chars()
            .filter(|&c| c != '\r')
            .map(|c| match c {
                '^' => "^^".to_owned(),
                '\n' => "^n".to_owned(),
                '"' => "^'".to_owned(),
                c => c.to_string(),
            })
            .collect()
    } else {
        v.to_owned()
    };
    let enc: String = enc.chars().filter(|c| !c.is_control()).collect();
    if enc.contains([',', ';', ':']) || enc.is_empty() { format!("\"{enc}\"") } else { enc }
}

/// Decodes TEXT escapes: `\\`, `\;`, `\,`, `\n`/`\N` (newline). Unknown escapes keep the character.
pub fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Encodes TEXT escapes (newlines become `\n`; CR dropped).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Splits on `sep` where not escaped by a backslash; pieces keep their escapes.
pub fn split_raw(s: &str, sep: char, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
        } else if c == '\\' {
            cur.push(c);
            escaped = true;
        } else if c == sep {
            if out.len() + 1 >= max {
                break;
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Splits on `sep` where not escaped by a backslash, then decodes escapes in each piece.
pub fn split_unescaped(s: &str, sep: char, max: usize) -> Vec<String> {
    split_raw(s, sep, max).iter().map(|p| unescape(p)).collect()
}

/// Decodes quoted-printable text (vCard 2.1 `ENCODING=QUOTED-PRINTABLE`), soft breaks removed.
pub fn decode_qp(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'=' {
            let h = bytes.get(i + 1).copied();
            let l = bytes.get(i + 2).copied();
            match (h.and_then(hex_val), l.and_then(hex_val)) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                _ => {
                    if h.is_none() {
                        // Trailing '=' (soft line break remnant).
                        i += 1;
                        continue;
                    }
                }
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

pub fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
