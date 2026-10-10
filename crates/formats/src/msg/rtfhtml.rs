//! RTF bodies: HTML de-encapsulation per [MS-OXRTFEX] and plain-text extraction.
//!
//! Encapsulated HTML is marked by `\fromhtml1` in the RTF header. The original HTML is the content of
//! `{\*\htmltagN …}` groups plus the text outside them that is not suppressed by `\htmlrtf` (which turns RTF-only
//! content on and off; `\htmlrtf0` ends it). Inside HTML, `\par` is a line break and `\tab` a tab; `\'hh` bytes
//! are decoded in the `\ansicpg` code page and `\uN` gives Unicode characters (skipping `\ucN` fallbacks).

/// Deepest group nesting followed (deeper groups are skipped).
const MAX_GROUP_DEPTH: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Html,
    Text,
}

#[derive(Clone, Copy)]
struct State {
    /// Inside a destination whose content is ignored.
    skip: bool,
    /// Inside `{\*\htmltagN …}`.
    html_tag: bool,
    /// `\htmlrtf` is on (RTF-only content).
    suppressed: bool,
    /// `\ucN`: fallback characters after `\uN`.
    uc: u32,
}

/// Collects output text; raw bytes are decoded in the document code page.
struct Sink {
    out: String,
    pending: Vec<u8>,
    enc: &'static encoding_rs::Encoding,
    /// High surrogate of a `\uN` pair waiting for its low half.
    high: Option<u32>,
}

impl Sink {
    /// One `\uN` UTF-16 code unit: surrogate pairs combine; unpaired surrogates are dropped.
    fn unit(&mut self, code: u32) {
        match code {
            0xD800..=0xDBFF => {
                self.flush();
                self.high = Some(code);
            }
            0xDC00..=0xDFFF => {
                let pair = self.high.take().map(|h| 0x10000 + ((h - 0xD800) << 10) + (code - 0xDC00));
                if let Some(c) = pair.and_then(char::from_u32) {
                    self.text(&c.to_string());
                }
            }
            _ => {
                if let Some(c) = char::from_u32(code) {
                    self.text(&c.to_string());
                }
            }
        }
    }
    fn flush(&mut self) {
        if !self.pending.is_empty() {
            let (t, _) = self.enc.decode_without_bom_handling(&self.pending);
            self.out.push_str(&t);
            self.pending.clear();
        }
    }
    fn byte(&mut self, b: u8) {
        self.high = None;
        self.pending.push(b);
    }
    fn text(&mut self, s: &str) {
        self.high = None;
        self.flush();
        self.out.push_str(s);
    }
}

/// True when the RTF declares encapsulated HTML (`\fromhtml1`).
pub fn is_html_encapsulated(rtf: &[u8]) -> bool {
    let head = rtf.get(..rtf.len().min(4096)).unwrap_or(rtf);
    head.windows(10).any(|w| w == b"\\fromhtml1")
}

/// The original HTML of an encapsulated-HTML RTF body; `None` when not encapsulated.
pub fn extract_html(rtf: &[u8]) -> Option<String> {
    if !is_html_encapsulated(rtf) {
        return None;
    }
    Some(walk(rtf, Mode::Html))
}

/// Plain text of an RTF body (paragraphs as newlines).
pub fn extract_text(rtf: &[u8]) -> String {
    let t = walk(rtf, Mode::Text);
    t.trim_end().to_owned()
}

/// Destinations whose content is never text.
fn is_skipped_destination(word: &str) -> bool {
    matches!(
        word,
        "fonttbl"
            | "colortbl"
            | "stylesheet"
            | "info"
            | "pict"
            | "object"
            | "header"
            | "footer"
            | "headerl"
            | "headerr"
            | "footerl"
            | "footerr"
            | "footnote"
            | "fldinst"
            | "themedata"
            | "colorschememapping"
            | "datastore"
            | "latentstyles"
            | "listtable"
            | "listoverridetable"
            | "rsidtbl"
            | "generator"
            | "xmlnstbl"
            | "filetbl"
            | "revtbl"
    )
}

fn walk(rtf: &[u8], mode: Mode) -> String {
    let mut sink =
        Sink { out: String::new(), pending: Vec::new(), enc: encoding_rs::WINDOWS_1252, high: None };
    let mut stack: Vec<State> = Vec::new();
    let mut st = State { skip: false, html_tag: false, suppressed: false, uc: 1 };
    // Overflow depth beyond MAX_GROUP_DEPTH (those groups are skipped wholesale).
    let mut overflow = 0usize;
    // Set right after '{': the next control word may name the group's destination.
    let mut group_start = false;
    let mut ignorable = false;
    // Fallback characters still to skip after \uN.
    let mut skip_fallback = 0u32;
    let mut i = 0usize;
    let emit_ok = |st: &State| -> bool {
        if st.skip {
            return false;
        }
        match mode {
            Mode::Html => st.html_tag || !st.suppressed,
            Mode::Text => !st.html_tag,
        }
    };
    while let Some(&b) = rtf.get(i) {
        match b {
            b'{' => {
                i += 1;
                if overflow > 0 || stack.len() >= MAX_GROUP_DEPTH {
                    overflow += 1;
                    continue;
                }
                stack.push(st);
                group_start = true;
                ignorable = false;
                skip_fallback = 0;
            }
            b'}' => {
                i += 1;
                if overflow > 0 {
                    overflow -= 1;
                    continue;
                }
                if let Some(prev) = stack.pop() {
                    st = prev;
                }
                group_start = false;
                skip_fallback = 0;
            }
            _ if overflow > 0 => i += 1,
            b'\\' => {
                let Some(&next) = rtf.get(i + 1) else { break };
                if next.is_ascii_alphabetic() {
                    // Control word.
                    let mut j = i + 1;
                    while rtf.get(j).is_some_and(u8::is_ascii_alphabetic) && j - i < 64 {
                        j += 1;
                    }
                    let word = String::from_utf8_lossy(rtf.get(i + 1..j).unwrap_or(&[])).into_owned();
                    let mut k = j;
                    if rtf.get(k) == Some(&b'-') {
                        k += 1;
                    }
                    while rtf.get(k).is_some_and(u8::is_ascii_digit) && k - j < 12 {
                        k += 1;
                    }
                    let param: Option<i64> =
                        std::str::from_utf8(rtf.get(j..k).unwrap_or(&[])).ok().and_then(|s| s.parse().ok());
                    if rtf.get(k) == Some(&b' ') {
                        k += 1;
                    }
                    i = k;
                    if word == "bin" {
                        // \binN: N bytes of binary data follow the delimiter (they may contain braces).
                        let n = param.and_then(|p| usize::try_from(p).ok()).unwrap_or(0);
                        i = i.saturating_add(n).min(rtf.len());
                        group_start = false;
                        continue;
                    }
                    let first_in_group = group_start;
                    group_start = false;
                    if first_in_group || ignorable {
                        if word == "htmltag" && ignorable {
                            st.html_tag = true;
                            ignorable = false;
                            continue;
                        }
                        if ignorable || is_skipped_destination(&word) {
                            st.skip = true;
                            ignorable = false;
                            continue;
                        }
                    }
                    if skip_fallback > 0 && word != "u" {
                        skip_fallback -= 1;
                        continue;
                    }
                    match word.as_str() {
                        "ansicpg" => {
                            if let Some(cp) = param.and_then(|p| u32::try_from(p).ok()) {
                                sink.flush();
                                sink.enc = super::encoding_for_codepage(cp);
                            }
                        }
                        "htmlrtf" => st.suppressed = param != Some(0),
                        "uc" => st.uc = param.and_then(|p| u32::try_from(p).ok()).unwrap_or(1).min(16),
                        "u" => {
                            if let Some(p) = param {
                                // Signed 16-bit in practice: negative values are code units above 0x7FFF.
                                let code = if p < 0 { p.saturating_add(65_536) } else { p };
                                if emit_ok(&st)
                                    && let Ok(code) = u32::try_from(code)
                                {
                                    sink.unit(code);
                                }
                                skip_fallback = st.uc;
                            }
                        }
                        "par" | "line" => {
                            if emit_ok(&st) {
                                sink.text(if mode == Mode::Html { "\r\n" } else { "\n" });
                            }
                        }
                        "tab" => {
                            if emit_ok(&st) {
                                sink.text("\t");
                            }
                        }
                        // Plain text keeps table cells apart (tab) and rows on their own lines.
                        "cell" | "nestcell" if mode == Mode::Text && emit_ok(&st) => sink.text("\t"),
                        "row" | "nestrow" if mode == Mode::Text && emit_ok(&st) => {
                            sink.flush();
                            while sink.out.ends_with('\t') {
                                sink.out.pop();
                            }
                            sink.text("\n");
                        }
                        "lquote" | "rquote" | "ldblquote" | "rdblquote" | "bullet" | "endash" | "emdash"
                            if emit_ok(&st) =>
                        {
                            let s = match word.as_str() {
                                "lquote" => "\u{2018}",
                                "rquote" => "\u{2019}",
                                "ldblquote" => "\u{201C}",
                                "rdblquote" => "\u{201D}",
                                "bullet" => "\u{2022}",
                                "endash" => "\u{2013}",
                                _ => "\u{2014}",
                            };
                            sink.text(s);
                        }
                        _ => {}
                    }
                } else {
                    // Control symbol.
                    i += 2;
                    let first_in_group = group_start;
                    group_start = false;
                    match next {
                        b'*' => {
                            if first_in_group {
                                ignorable = true;
                                group_start = true;
                            }
                        }
                        b'\'' => {
                            let h = rtf.get(i).copied().and_then(crate::text::hex_val);
                            let l = rtf.get(i + 1).copied().and_then(crate::text::hex_val);
                            if let (Some(h), Some(l)) = (h, l) {
                                i += 2;
                                if skip_fallback > 0 {
                                    skip_fallback -= 1;
                                } else if emit_ok(&st) {
                                    sink.byte(h << 4 | l);
                                }
                            }
                        }
                        b'\\' | b'{' | b'}' => {
                            if skip_fallback > 0 {
                                skip_fallback -= 1;
                            } else if emit_ok(&st) {
                                sink.byte(next);
                            }
                        }
                        b'~' => {
                            if emit_ok(&st) {
                                sink.text("\u{00A0}");
                            }
                        }
                        b'_' => {
                            if emit_ok(&st) {
                                sink.text("\u{2011}");
                            }
                        }
                        b'\r' | b'\n' if emit_ok(&st) => {
                            sink.text(if mode == Mode::Html { "\r\n" } else { "\n" });
                        }
                        _ => {}
                    }
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                i += 1;
                group_start = false;
                if skip_fallback > 0 {
                    skip_fallback -= 1;
                    continue;
                }
                if emit_ok(&st) {
                    sink.byte(b);
                }
            }
        }
    }
    sink.flush();
    sink.out
}
