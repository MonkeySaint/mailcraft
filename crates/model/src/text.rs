//! Small text helpers shared by previews and rule matching. Deliberately crude: real HTML handling is
//! `mailcraft-html`'s job (a higher layer); here we only need something readable for a one-line preview.

/// Characters kept in the list preview line.
pub const PREVIEW_CHARS: usize = 255;

/// Bytes of HTML examined when stripping for a preview or rule match (bounds the work on huge bodies).
pub const MAX_STRIP_INPUT: usize = 4 * 1024 * 1024;

/// Elements whose content is never visible text.
const HIDDEN_ELEMENTS: [&str; 5] = ["head", "style", "script", "title", "template"];

/// Collapses runs of whitespace (including NBSP) to single spaces and trims the ends.
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(1 << 20));
    for word in s.split(|c: char| c.is_whitespace() || c == '\u{a0}') {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Collapsed text truncated to at most `max_chars` characters.
pub fn preview_of(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '\u{a0}' {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            // Room is needed for the space and at least one more character.
            if count.saturating_add(1) >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= max_chars {
            break;
        }
        out.push(c);
        count += 1;
    }
    out
}

/// Removes tags, comments and invisible elements from HTML and decodes common entities. Block-level tags
/// become spaces so words do not run together. Input beyond [`MAX_STRIP_INPUT`] bytes is ignored.
pub fn strip_html(html: &str) -> String {
    let html = truncate_at_char(html, MAX_STRIP_INPUT);
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_decoded(rest.get(..lt).unwrap_or(""), &mut out);
        let tail = rest.get(lt..).unwrap_or("");
        if let Some(after) = tail.strip_prefix("<!--") {
            rest = match after.find("-->") {
                Some(end) => after.get(end + 3..).unwrap_or(""),
                None => "",
            };
            continue;
        }
        let Some(gt) = tail.find('>') else {
            // Unterminated tag: treat the rest as text-free.
            rest = "";
            break;
        };
        let tag = tail.get(1..gt).unwrap_or("");
        rest = tail.get(gt + 1..).unwrap_or("");
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if !tag.starts_with('/') && !tag.ends_with('/') && HIDDEN_ELEMENTS.contains(&name.as_str()) {
            // Skip to the matching close tag.
            out.push(' ');
            rest = match find_close_tag(rest, &name) {
                Some(pos) => {
                    let after = rest.get(pos..).unwrap_or("");
                    match after.find('>') {
                        Some(g) => after.get(g + 1..).unwrap_or(""),
                        None => "",
                    }
                }
                None => "",
            };
            continue;
        }
        if !name.is_empty() {
            out.push(' ');
        }
    }
    push_decoded(rest, &mut out);
    out
}

/// Byte offset of the first end tag for `name` ("</name" compared ASCII case-insensitively and followed by
/// whitespace, "/", ">" or the end of input, so "</scripts>" does not close a script). Cost is linear in the
/// offset.
fn find_close_tag(hay: &str, name: &str) -> Option<usize> {
    let bytes = hay.as_bytes();
    let n = name.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = bytes.get(from..)?.windows(2).position(|w| w == b"</") {
        let start = from.saturating_add(rel);
        let name_at = start.saturating_add(2);
        let name_end = name_at.saturating_add(n.len());
        let name_matches = bytes.get(name_at..name_end).is_some_and(|w| w.eq_ignore_ascii_case(n));
        let boundary = match bytes.get(name_end) {
            None => true,
            Some(b) => b.is_ascii_whitespace() || *b == b'/' || *b == b'>',
        };
        if name_matches && boundary {
            return Some(start);
        }
        from = name_at;
    }
    None
}

/// The first `max` bytes of `s`, cut back to a character boundary.
pub fn truncate_at_char(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// Appends `text` with HTML character references decoded (named basics plus numeric forms).
fn push_decoded(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or(""));
        let tail = rest.get(amp..).unwrap_or("");
        // Look for the ';' within a short byte window; scanning bytes keeps this independent of where the
        // following characters' UTF-8 boundaries fall (';' is ASCII, so any hit is a char boundary).
        let semi = tail.as_bytes().iter().take(12).position(|&b| b == b';');
        let decoded = semi.and_then(|end| decode_entity(tail.get(1..end).unwrap_or("")).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = tail.get(end + 1..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
}

fn decode_entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return Some(char::from_u32(code).filter(|c| *c != '\0').unwrap_or('\u{fffd}'));
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "hellip" => '\u{2026}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "euro" => '\u{20ac}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "shy" => '\u{ad}',
        _ => return None,
    })
}

/// Case-insensitive substring test (Unicode lower-casing). An empty needle never matches.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}
