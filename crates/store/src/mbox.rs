//! MBOX reading and writing (the `From `-line delimited family described in RFC 4155).
//!
//! Reading accepts mboxo and mboxrd: a message starts at a line beginning `From ` that is the first line of
//! the file or follows an empty line (or that carries a recognisable date even without the empty line). The
//! separating empty line before the next `From ` line is removed, and quoted `>From ` lines are unquoted one
//! level (mboxrd rule, which is also the correct reading of mboxo's single level). Writing produces mboxrd.

use std::io::{BufRead, Read, Write};

use mailcraft_model::DateTime;

use crate::time;

/// Longest physical line read in one piece; longer lines are read as several fragments.
pub const MAX_LINE_BYTES: u64 = 1 << 20;
/// Messages larger than this are skipped (the MIME parser's own cap).
pub const MAX_MESSAGE_BYTES: usize = mailcraft_mime::MAX_MESSAGE_BYTES;
/// Most messages imported from one file.
pub const MAX_MESSAGES: usize = 1_000_000;

/// One message split out of an MBOX stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MboxMessage {
    /// The `From ` line without its line ending (`None` for content before the first `From ` line).
    pub from_line: Option<String>,
    /// Message bytes, `>From` unquoted, separator line removed. Empty when `oversized`.
    pub data: Vec<u8>,
    /// The message exceeded [`MAX_MESSAGE_BYTES`]; its content was dropped.
    pub oversized: bool,
}

impl MboxMessage {
    /// Envelope sender and delivery date from the `From ` line.
    pub fn envelope(&self) -> (Option<String>, Option<DateTime>) {
        self.from_line.as_deref().map(parse_from_line).unwrap_or((None, None))
    }
}

/// Splits `From <sender> <date>` into its parts (both lenient).
pub fn parse_from_line(line: &str) -> (Option<String>, Option<DateTime>) {
    let Some(rest) = line.strip_prefix("From ") else { return (None, None) };
    let rest = rest.trim_start();
    let (sender, date) = match rest.split_once(char::is_whitespace) {
        Some((s, d)) => (s, d),
        None => (rest, ""),
    };
    let sender = (!sender.is_empty()).then(|| sender.chars().take(512).collect());
    (sender, time::parse_asctime(date))
}

fn is_from_line(frag: &[u8]) -> bool {
    frag.starts_with(b"From ")
}

fn is_blank(frag: &[u8]) -> bool {
    frag == b"\n" || frag == b"\r\n"
}

/// True for `>From `, `>>From `, ... (a quoted From line).
fn is_quoted_from(frag: &[u8]) -> bool {
    let quotes = frag.iter().take_while(|&&b| b == b'>').count();
    quotes > 0 && frag.get(quotes..).is_some_and(|r| r.starts_with(b"From "))
}

fn line_text(frag: &[u8]) -> String {
    let t = frag.strip_suffix(b"\n").unwrap_or(frag);
    let t = t.strip_suffix(b"\r").unwrap_or(t);
    String::from_utf8_lossy(t).into_owned()
}

/// Streaming MBOX splitter over any buffered reader.
pub struct MboxReader<R: BufRead> {
    reader: R,
    pending_from: Option<String>,
    at_line_start: bool,
    prev_blank: bool,
    seen_any: bool,
    done: bool,
    produced: usize,
}

impl<R: BufRead> MboxReader<R> {
    pub fn new(reader: R) -> Self {
        MboxReader {
            reader,
            pending_from: None,
            at_line_start: true,
            prev_blank: false,
            seen_any: false,
            done: false,
            produced: 0,
        }
    }

    fn read_fragment(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        buf.clear();
        (&mut self.reader).take(MAX_LINE_BYTES).read_until(b'\n', buf)
    }

    fn finish(mut msg: MboxMessage) -> MboxMessage {
        if msg.data == b"\n" || msg.data == b"\r\n" {
            msg.data.clear();
        } else if msg.data.ends_with(b"\r\n\r\n") {
            msg.data.truncate(msg.data.len().saturating_sub(2));
        } else if msg.data.ends_with(b"\n\n") {
            msg.data.truncate(msg.data.len().saturating_sub(1));
        }
        msg
    }

    /// True when reading stopped at [`MAX_MESSAGES`] with input left over.
    pub fn hit_message_limit(&self) -> bool {
        !self.done && self.produced >= MAX_MESSAGES
    }

    /// Next message, `Ok(None)` at end of input (or after [`MAX_MESSAGES`]).
    pub fn next_message(&mut self) -> std::io::Result<Option<MboxMessage>> {
        if self.done || self.produced >= MAX_MESSAGES {
            return Ok(None);
        }
        let mut current: Option<MboxMessage> =
            self.pending_from.take().map(|l| MboxMessage { from_line: Some(l), ..Default::default() });
        let mut frag = Vec::new();
        loop {
            let n = self.read_fragment(&mut frag)?;
            if n == 0 {
                self.done = true;
                return Ok(current.map(|m| {
                    self.produced += 1;
                    Self::finish(m)
                }));
            }
            let at_start = self.at_line_start;
            self.at_line_start = frag.last() == Some(&b'\n');
            let first = !self.seen_any;
            self.seen_any = true;
            if at_start && is_from_line(&frag) {
                let text = line_text(&frag);
                let separator = first || self.prev_blank || parse_from_line(&text).1.is_some();
                if separator {
                    self.prev_blank = false;
                    if let Some(msg) = current.take() {
                        self.pending_from = Some(text);
                        self.produced += 1;
                        return Ok(Some(Self::finish(msg)));
                    }
                    current = Some(MboxMessage { from_line: Some(text), ..Default::default() });
                    continue;
                }
            }
            self.prev_blank = at_start && is_blank(&frag);
            let msg = match current.as_mut() {
                Some(m) => m,
                None => {
                    if frag.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    log::warn!("mbox: content before the first From line; importing it as a message");
                    current.insert(MboxMessage::default())
                }
            };
            if msg.oversized {
                continue;
            }
            let body: &[u8] =
                if at_start && is_quoted_from(&frag) { frag.get(1..).unwrap_or(&[]) } else { &frag };
            if msg.data.len().saturating_add(body.len()) > MAX_MESSAGE_BYTES {
                msg.oversized = true;
                msg.data = Vec::new();
                continue;
            }
            msg.data.extend_from_slice(body);
        }
    }
}

/// Appends one message to an mboxrd stream: `From ` line, LF line endings, `>From` quoting, closing blank
/// line.
pub fn write_message<W: Write>(w: &mut W, sender: &str, date: DateTime, data: &[u8]) -> std::io::Result<()> {
    let sender: String =
        sender.chars().take(256).map(|c| if c.is_whitespace() || c.is_control() { '_' } else { c }).collect();
    let sender = if sender.is_empty() { "MAILER-DAEMON".to_owned() } else { sender };
    writeln!(w, "From {sender} {}", time::format_asctime(date))?;
    let mut ended_with_newline = true;
    for line in data.split_inclusive(|&b| b == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let has_nl = line.len() != content.len();
        let content = if has_nl { content.strip_suffix(b"\r").unwrap_or(content) } else { content };
        if is_from_line(content) || is_quoted_from(content) {
            w.write_all(b">")?;
        }
        w.write_all(content)?;
        if has_nl {
            w.write_all(b"\n")?;
        }
        ended_with_newline = has_nl;
    }
    if !ended_with_newline {
        w.write_all(b"\n")?;
    }
    w.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn split(input: &[u8]) -> Vec<MboxMessage> {
        let mut r = MboxReader::new(Cursor::new(input.to_vec()));
        let mut out = Vec::new();
        while let Ok(Some(m)) = r.next_message() {
            out.push(m);
        }
        out
    }

    #[test]
    fn splits_two_messages_and_unquotes() {
        let input = b"From a@x Thu Oct  8 04:30:00 2026\nSubject: one\n\n>From the start\n>>From deeper\n\nFrom b@y Fri Oct  9 05:00:00 2026\nSubject: two\n\nbody\n";
        let msgs = split(input);
        assert_eq!(msgs.len(), 2);
        let a = msgs.first().map(|m| m.data.clone()).unwrap_or_default();
        assert_eq!(a, b"Subject: one\n\nFrom the start\n>From deeper\n".to_vec());
        let b = msgs.get(1).map(|m| m.data.clone()).unwrap_or_default();
        assert_eq!(b, b"Subject: two\n\nbody\n".to_vec());
        let (sender, date) = msgs.get(1).map(MboxMessage::envelope).unwrap_or_default();
        assert_eq!(sender.as_deref(), Some("b@y"));
        assert!(date.is_some());
    }

    #[test]
    fn from_inside_paragraph_is_not_a_separator() {
        let input = b"From a@x Thu Oct  8 04:30:00 2026\nSubject: s\n\nline\nFrom here on, text\n";
        let msgs = split(input);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn dated_from_line_splits_without_blank() {
        let input = b"From a@x Thu Oct  8 04:30:00 2026\nSubject: s\n\nx\nFrom b@y Thu Oct  8 04:31:00 2026\nSubject: t\n\ny\n";
        assert_eq!(split(input).len(), 2);
    }

    #[test]
    fn crlf_mbox() {
        let input = b"From a@x Thu Oct  8 04:30:00 2026\r\nSubject: s\r\n\r\nx\r\n\r\nFrom b@y Thu Oct  8 04:31:00 2026\r\nSubject: t\r\n\r\ny\r\n";
        let msgs = split(input);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs.first().map(|m| m.data.clone()), Some(b"Subject: s\r\n\r\nx\r\n".to_vec()));
    }

    #[test]
    fn preamble_and_empty() {
        assert!(split(b"").is_empty());
        assert!(split(b"\n\n\n").is_empty());
        let msgs = split(b"Subject: loose\n\nbody\n");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs.first().and_then(|m| m.from_line.clone()), None);
    }

    #[test]
    fn write_then_read_round_trip() {
        let bodies: [&[u8]; 4] = [
            b"Subject: a\n\nFrom me\n>From you\n",
            b"Subject: b\r\n\r\nno trailing newline",
            b"Subject: c\n\nends with blank\n\n",
            b"",
        ];
        let mut out = Vec::new();
        for b in bodies {
            let _ = write_message(&mut out, "who@example.com", DateTime::default(), b);
        }
        let msgs = split(&out);
        assert_eq!(msgs.len(), 4);
        let got: Vec<Vec<u8>> = msgs.into_iter().map(|m| m.data).collect();
        assert_eq!(got.first().cloned(), Some(bodies[0].to_vec()));
        assert_eq!(got.get(1).cloned(), Some(b"Subject: b\n\nno trailing newline\n".to_vec()));
        assert_eq!(got.get(2).cloned(), Some(bodies[2].to_vec()));
        assert_eq!(got.get(3).cloned(), Some(Vec::new()));
    }

    #[test]
    fn sender_sanitised() {
        let mut out = Vec::new();
        let _ = write_message(&mut out, "a b\tc", DateTime::default(), b"x\n");
        assert!(out.starts_with(b"From a_b_c Thu Jan  1 00:00:00 1970\n"));
        out.clear();
        let _ = write_message(&mut out, "", DateTime::default(), b"x\n");
        assert!(out.starts_with(b"From MAILER-DAEMON "));
    }

    #[test]
    fn long_lines_are_fragmented_not_split() {
        let mut input = b"From a@x Thu Oct  8 04:30:00 2026\nSubject: s\n\n".to_vec();
        let long = vec![b'z'; usize::try_from(MAX_LINE_BYTES).unwrap_or(0) + 10];
        input.extend_from_slice(&long);
        input.extend_from_slice(b"From inside the long line\n");
        let msgs = split(&input);
        assert_eq!(msgs.len(), 1);
        assert!(msgs.first().map(|m| m.data.ends_with(b"From inside the long line\n")).unwrap_or(false));
    }

    proptest::proptest! {
        #[test]
        fn hostile_mbox_never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048)) {
            let msgs = split(&data);
            for m in &msgs {
                let _ = m.envelope();
            }
        }

        #[test]
        fn hostile_from_line_never_panics(s in "From \\PC{0,80}") {
            let _ = parse_from_line(&s);
        }

        #[test]
        fn round_trip_arbitrary_bodies(lines in proptest::collection::vec("[ -~]{0,20}", 0..12)) {
            let mut body = Vec::new();
            for l in &lines {
                body.extend_from_slice(l.as_bytes());
                body.push(b'\n');
            }
            let mut out = Vec::new();
            write_message(&mut out, "s@x", DateTime::default(), &body).ok();
            write_message(&mut out, "t@x", DateTime::default(), b"second\n").ok();
            let msgs = split(&out);
            proptest::prop_assert_eq!(msgs.len(), 2);
            proptest::prop_assert_eq!(msgs.first().map(|m| m.data.clone()), Some(body));
        }
    }
}
