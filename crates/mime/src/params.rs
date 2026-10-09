//! MIME header parameters (RFC 2045 §5.1, RFC 2183, RFC 2231).
//!
//! Parses `value; name=token; name="quoted"` with comments anywhere, joins RFC 2231 continuations
//! (`name*0`, `name*1*`), decodes `charset'language'%XX` extended values, and (leniently) decodes RFC 2047 words
//! that many clients put inside quoted `name` / `filename` values. Also formats parameters for output.

/// Most parameters kept from one header (the rest are ignored).
pub const MAX_PARAMS: usize = 256;
/// Highest RFC 2231 continuation index honoured.
pub const MAX_SECTION: u32 = 999;

/// A small cursor over a header value.
struct Cursor<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos..).and_then(|r| r.chars().next())
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    /// Skips whitespace and (nested) comments.
    fn skip_cfws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump();
            } else if c == '(' {
                self.skip_comment();
            } else {
                break;
            }
        }
    }
    /// At `(`: skips to the matching `)` (or the end).
    fn skip_comment(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    self.bump();
                }
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
    /// At `"`: reads a quoted string, unescaping quoted-pairs. An unterminated string runs to the end.
    fn quoted(&mut self) -> String {
        let mut out = String::new();
        self.bump();
        while let Some(c) = self.bump() {
            match c {
                '"' => break,
                '\\' => {
                    if let Some(n) = self.bump() {
                        out.push(n);
                    }
                }
                '\r' | '\n' => {}
                _ => out.push(c),
            }
        }
        out
    }
    /// Reads unquoted text up to `stop` (outside comments), dropping comments; trimmed.
    fn until(&mut self, stop: &[char]) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if stop.contains(&c) {
                break;
            }
            if c == '(' {
                self.skip_comment();
                out.push(' ');
                continue;
            }
            out.push(c);
            self.bump();
        }
        out.trim().to_owned()
    }
}

/// One raw `name=value` before RFC 2231 assembly.
struct RawParam {
    name: String,
    value: String,
}

/// Parses `main; a=b; c="d"` → (`main` with whitespace and comments removed, decoded parameters).
/// Parameter names are lower-cased; the first occurrence of a name wins.
pub fn parse_header_params(value: &str) -> (String, Vec<(String, String)>) {
    let mut cur = Cursor { s: value, pos: 0 };
    cur.skip_cfws();
    let main: String = cur.until(&[';']).chars().filter(|c| !c.is_whitespace()).collect();
    let mut raw: Vec<RawParam> = Vec::new();
    while cur.peek().is_some() && raw.len() < MAX_PARAMS {
        // At ';' (or junk): skip it.
        cur.bump();
        cur.skip_cfws();
        let name = cur.until(&[';', '=']);
        if cur.peek() != Some('=') {
            continue;
        }
        cur.bump();
        cur.skip_cfws();
        let v = if cur.peek() == Some('"') {
            let q = cur.quoted();
            // Ignore anything between the closing quote and the next ';'.
            let _ = cur.until(&[';']);
            q
        } else {
            cur.until(&[';'])
        };
        let name: String =
            name.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        if !name.is_empty() {
            raw.push(RawParam { name, value: v });
        }
    }
    (main, assemble(raw))
}

/// Joins RFC 2231 sections and decodes extended values.
fn assemble(raw: Vec<RawParam>) -> Vec<(String, String)> {
    struct Section {
        index: u32,
        extended: bool,
        value: String,
    }
    // (base name, sections, plain value) in first-appearance order.
    let mut groups: Vec<(String, Vec<Section>, Option<String>)> = Vec::new();
    for p in raw {
        let (base, section) = split_name(&p.name);
        let idx = match groups.iter().position(|(b, _, _)| *b == base) {
            Some(i) => i,
            None => {
                groups.push((base.clone(), Vec::new(), None));
                groups.len() - 1
            }
        };
        let Some(group) = groups.get_mut(idx) else { continue };
        match section {
            None => {
                if group.2.is_none() {
                    group.2 = Some(p.value);
                }
            }
            Some((index, extended)) => {
                if !group.1.iter().any(|s| s.index == index) {
                    group.1.push(Section { index, extended, value: p.value });
                }
            }
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (base, mut sections, plain) in groups {
        let value = if sections.is_empty() {
            let v = plain.unwrap_or_default();
            if is_name_param(&base) && v.contains("=?") { crate::encoded::decode_words(&v) } else { v }
        } else {
            sections.sort_by_key(|s| s.index);
            let mut charset = String::new();
            let mut bytes: Vec<u8> = Vec::new();
            for (k, s) in sections.iter().enumerate() {
                if s.extended {
                    let mut text = s.value.as_str();
                    if k == 0 {
                        let mut parts = text.splitn(3, '\'');
                        if let (Some(cs), Some(_lang), Some(rest)) =
                            (parts.next(), parts.next(), parts.next())
                        {
                            charset = cs.to_owned();
                            text = rest;
                        }
                    }
                    bytes.extend_from_slice(&percent_decode(text));
                } else {
                    bytes.extend_from_slice(s.value.as_bytes());
                }
            }
            if charset.is_empty() {
                crate::charset::decode_unlabelled(&bytes)
            } else {
                crate::charset::decode(&bytes, &charset)
            }
        };
        out.push((base, value));
    }
    out
}

fn is_name_param(name: &str) -> bool {
    name == "name" || name == "filename"
}

/// `name` → (`name`, None); `name*` → (`name`, (0, ext)); `name*3` → (`name`, (3, plain)); `name*3*` → ext.
fn split_name(name: &str) -> (String, Option<(u32, bool)>) {
    let Some(star) = name.find('*') else { return (name.to_owned(), None) };
    let base = name.get(..star).unwrap_or(name).to_owned();
    let rest = name.get(star + 1..).unwrap_or_default();
    if rest.is_empty() {
        return (base, Some((0, true)));
    }
    let (digits, extended) = match rest.strip_suffix('*') {
        Some(d) => (d, true),
        None => (rest, false),
    };
    match digits.parse::<u32>() {
        Ok(n) if n <= MAX_SECTION && digits.bytes().all(|b| b.is_ascii_digit()) => {
            (base, Some((n, extended)))
        }
        // Malformed section: keep the parameter under its literal name.
        _ => (name.to_owned(), None),
    }
}

/// Decodes `%XX` escapes; malformed escapes are kept literally.
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let Some(v) = crate::transfer::hex_pair(b, i + 1)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// RFC 2045 token character.
pub fn is_token_char(c: char) -> bool {
    c.is_ascii_graphic() && !"()<>@,;:\\\"/[]?=".contains(c)
}

/// RFC 2231 attribute-char (may appear unescaped in an extended value).
fn is_attr_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b)
}

/// Longest parameter section emitted before splitting into RFC 2231 continuations.
const SECTION_LEN: usize = 60;

/// Formats one parameter (`name=value`, `name="value"`, or RFC 2231 `name*=utf-8''…` / continuations).
/// Multiple sections are joined with `"; "` so the result can be appended after `"; "`.
pub fn format_param(name: &str, value: &str) -> String {
    // A `name`/`filename` that literally contains `=?` would be taken for RFC 2047 words by lenient readers
    // (including this crate's parser), so it goes out in the RFC 2231 extended form, which is never word-decoded.
    let word_lookalike = is_name_param(&name.to_ascii_lowercase()) && value.contains("=?");
    let printable = !word_lookalike && value.chars().all(|c| (' '..='~').contains(&c));
    if printable && !value.is_empty() && value.len() <= SECTION_LEN && value.chars().all(is_token_char) {
        return format!("{name}={value}");
    }
    if printable && value.len() <= SECTION_LEN {
        return format!("{name}=\"{}\"", quote_escape(value));
    }
    if printable {
        // Long ASCII: plain continuations of quoted strings.
        let mut out = Vec::new();
        let chars: Vec<char> = value.chars().collect();
        for (k, chunk) in chars.chunks(SECTION_LEN).enumerate() {
            let s: String = chunk.iter().collect();
            out.push(format!("{name}*{k}=\"{}\"", quote_escape(&s)));
        }
        return out.join("; ");
    }
    // Non-ASCII: percent-encoded UTF-8, split between escapes.
    let mut pieces: Vec<String> = vec![String::new()];
    for c in value.chars() {
        let mut buf = [0u8; 4];
        let enc: String = c
            .encode_utf8(&mut buf)
            .bytes()
            .map(|b| if is_attr_char(b) { char::from(b).to_string() } else { format!("%{b:02X}") })
            .collect();
        let need_new = pieces.last().is_some_and(|l| l.len() + enc.len() > SECTION_LEN && !l.is_empty());
        if need_new {
            pieces.push(String::new());
        }
        if let Some(last) = pieces.last_mut() {
            last.push_str(&enc);
        }
    }
    if pieces.len() == 1 {
        return format!("{name}*=utf-8''{}", pieces.first().map(String::as_str).unwrap_or_default());
    }
    pieces
        .iter()
        .enumerate()
        .map(|(k, p)| if k == 0 { format!("{name}*0*=utf-8''{p}") } else { format!("{name}*{k}*={p}") })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Escapes `\` and `"` for a quoted-string.
pub fn quote_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn p(s: &str) -> (String, Vec<(String, String)>) {
        parse_header_params(s)
    }

    fn get<'a>(v: &'a [(String, String)], k: &str) -> Option<&'a str> {
        v.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn basic() {
        let (m, ps) = p("text/plain; charset=utf-8; format=flowed");
        assert_eq!(m, "text/plain");
        assert_eq!(get(&ps, "charset"), Some("utf-8"));
        assert_eq!(get(&ps, "format"), Some("flowed"));
    }

    #[test]
    fn quoted_and_comments() {
        let (m, ps) = p("text/plain (Plain text) ; Charset = \"us-ascii\" (comment); x=\"a;b \\\"c\\\"\"");
        assert_eq!(m, "text/plain");
        assert_eq!(get(&ps, "charset"), Some("us-ascii"));
        assert_eq!(get(&ps, "x"), Some("a;b \"c\""));
        let (m, _) = p("multipart/mixed(c);boundary=x");
        assert_eq!(m, "multipart/mixed");
        let (m, _) = p("text / html");
        assert_eq!(m, "text/html");
    }

    #[test]
    fn lenient_values() {
        let (_, ps) = p("attachment; filename=my file.pdf; size=10");
        assert_eq!(get(&ps, "filename"), Some("my file.pdf"));
        assert_eq!(get(&ps, "size"), Some("10"));
        let (_, ps) = p("attachment;; ;novalue; filename=\"unterminated");
        assert_eq!(get(&ps, "filename"), Some("unterminated"));
        let (_, ps) = p("a; x=1; x=2");
        assert_eq!(get(&ps, "x"), Some("1"));
        let (m, ps) = p("");
        assert!(m.is_empty() && ps.is_empty());
    }

    #[test]
    fn rfc2231_examples() {
        let (_, ps) = p(
            "message/external-body; access-type=URL; URL*0=\"ftp://\"; URL*1=\"cs.utk.edu/pub/moore/bulk-mailer/bulk-mailer.tar\"",
        );
        assert_eq!(get(&ps, "url"), Some("ftp://cs.utk.edu/pub/moore/bulk-mailer/bulk-mailer.tar"));
        let (_, ps) = p("application/x-stuff; title*=us-ascii'en-us'This%20is%20%2A%2A%2Afun%2A%2A%2A");
        assert_eq!(get(&ps, "title"), Some("This is ***fun***"));
        let (_, ps) = p(
            "application/x-stuff; title*0*=us-ascii'en'This%20is%20even%20more%20; title*1*=%2A%2A%2Afun%2A%2A%2A%20; title*2=\"isn't it!\"",
        );
        assert_eq!(get(&ps, "title"), Some("This is even more ***fun*** isn't it!"));
    }

    #[test]
    fn rfc2231_charsets_and_order() {
        let (_, ps) = p("attachment; filename*=UTF-8''caf%C3%A9.txt");
        assert_eq!(get(&ps, "filename"), Some("café.txt"));
        let (_, ps) = p("attachment; filename*=iso-8859-1'fr'caf%E9.txt");
        assert_eq!(get(&ps, "filename"), Some("café.txt"));
        let (_, ps) = p("attachment; filename*1=\"b.txt\"; filename*0=\"a\"");
        assert_eq!(get(&ps, "filename"), Some("ab.txt"));
        let (_, ps) = p("attachment; filename=\"fallback.txt\"; filename*=utf-8''real.txt");
        assert_eq!(get(&ps, "filename"), Some("real.txt"));
        let (_, ps) = p("attachment; filename*=noquotes%20here");
        assert_eq!(get(&ps, "filename"), Some("noquotes here"));
        let (_, ps) = p("attachment; filename*0*=utf-8''%E6%97; filename*1*=%A5.txt");
        assert_eq!(get(&ps, "filename"), Some("日.txt"));
        let (_, ps) = p("a; x*abc=1; y*99999=2");
        assert_eq!(get(&ps, "x*abc"), Some("1"));
        assert_eq!(get(&ps, "y*99999"), Some("2"));
    }

    #[test]
    fn encoded_words_in_names() {
        let (_, ps) = p("image/png; name=\"=?utf-8?B?Y2Fmw6kucG5n?=\"");
        assert_eq!(get(&ps, "name"), Some("café.png"));
        let (_, ps) = p("multipart/mixed; boundary=\"=?not-decoded?=\"");
        assert_eq!(get(&ps, "boundary"), Some("=?not-decoded?="));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_param("charset", "utf-8"), "charset=utf-8");
        assert_eq!(format_param("filename", "a b.txt"), "filename=\"a b.txt\"");
        assert_eq!(format_param("filename", "q\"x"), "filename=\"q\\\"x\"");
        assert_eq!(format_param("filename", ""), "filename=\"\"");
        assert_eq!(format_param("filename", "café.txt"), "filename*=utf-8''caf%C3%A9.txt");
        let long = format_param("filename", &"é".repeat(40));
        assert!(long.starts_with("filename*0*=utf-8''"));
        assert!(long.contains("; filename*1*="));
        assert_eq!(format_param("filename", "=?a?q?b?="), "filename*=utf-8''%3D%3Fa%3Fq%3Fb%3F%3D");
        assert_eq!(format_param("boundary", "=?b?="), "boundary=\"=?b?=\"");
    }

    proptest! {
        #[test]
        fn format_parse_round_trip(v in "\\PC{0,150}") {
            let s = format!("attachment; {}", format_param("filename", &v));
            let (_, ps) = parse_header_params(&s);
            // Quoted values are not trimmed, extended values are exact, and `=?` look-alikes are never decoded.
            prop_assert_eq!(get(&ps, "filename"), Some(v.as_str()));
        }

        #[test]
        fn never_panics(s in "\\PC{0,200}") {
            let _ = parse_header_params(&s);
        }

        #[test]
        fn never_panics_structured(s in "([a-z*0-9]{0,5}=?(\"[^\"]{0,5}\"?)?[;( )'%]{0,3}){0,12}") {
            let _ = parse_header_params(&s);
        }
    }
}
