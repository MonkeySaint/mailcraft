//! Server-sent events (the `text/event-stream` format of the WHATWG HTML standard, section 9.2), as used by
//! JMAP push (RFC 8620 section 7.3): `event`, `data`, `id` and `retry` fields, comments, and a blank line
//! ending each event. Read from an [`HttpStream`] with a time limit per call, so a worker can check its stop
//! flag between reads.

use std::time::Duration;

use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpStream};

/// Largest data of one event.
const MAX_EVENT_DATA: usize = 1024 * 1024;
/// Longest line of the stream.
const MAX_LINE: usize = 1024 * 1024;

/// One dispatched event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field ("message" when absent).
    pub event: String,
    /// The `data` lines joined with LF.
    pub data: String,
    /// The last event id seen so far.
    pub id: Option<String>,
}

/// Incremental parser state (also usable on its own, line by line).
#[derive(Clone, Debug, Default)]
pub struct SseParser {
    event: String,
    data: String,
    has_data: bool,
    last_id: Option<String>,
    /// The `retry` field: reconnection time asked for by the server.
    pub retry: Option<Duration>,
    first_line: bool,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser { first_line: true, ..Default::default() }
    }

    /// Feeds one line (without its line ending). Returns an event when the line ends one.
    pub fn line(&mut self, raw: &[u8]) -> Result<Option<SseEvent>> {
        let mut text = String::from_utf8_lossy(raw).into_owned();
        if std::mem::take(&mut self.first_line)
            && let Some(t) = text.strip_prefix('\u{feff}')
        {
            text = t.to_owned();
        }
        if text.is_empty() {
            let event = std::mem::take(&mut self.event);
            let data = std::mem::take(&mut self.data);
            if !std::mem::take(&mut self.has_data) {
                return Ok(None);
            }
            let data = data.strip_suffix('\n').map(str::to_owned).unwrap_or(data);
            let event = if event.is_empty() { "message".to_owned() } else { event };
            return Ok(Some(SseEvent { event, data, id: self.last_id.clone() }));
        }
        if text.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text.as_str(), ""),
        };
        match field {
            "event" => value.clone_into(&mut self.event),
            "data" => {
                if self.data.len().saturating_add(value.len()) > MAX_EVENT_DATA {
                    return Err(NetError::TooLarge("event data".into()));
                }
                self.data.push_str(value);
                self.data.push('\n');
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.last_id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry = Some(Duration::from_millis(ms));
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

/// An open event stream.
#[derive(Debug)]
pub struct EventStream {
    http: HttpStream,
    parser: SseParser,
}

impl EventStream {
    /// Opens `url` with `Accept: text/event-stream` plus `headers` (e.g. `Authorization`).
    pub fn open(client: &HttpClient, url: &str, headers: &[(&str, &str)]) -> Result<EventStream> {
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        h.push(("Accept", "text/event-stream"));
        h.push(("Cache-Control", "no-cache"));
        let mut http = client.open_stream(url, &h)?;
        let ctype = http.header("content-type").unwrap_or_default().to_ascii_lowercase();
        if !ctype.starts_with("text/event-stream") {
            http.close();
            return Err(NetError::Protocol(format!("not an event stream (Content-Type `{ctype}`)")));
        }
        http.max_line = MAX_LINE;
        Ok(EventStream { http, parser: SseParser::new() })
    }

    /// The next event, waiting at most `wait` in all (`Ok(None)`: nothing complete arrived in time).
    /// [`NetError::Closed`] when the server ended the stream.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<SseEvent>> {
        let deadline = crate::transport::deadline_after(wait);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let Some(line) = self.http.read_line(left)? else { return Ok(None) };
            if let Some(ev) = self.parser.line(&line)? {
                return Ok(Some(ev));
            }
        }
    }

    /// The reconnection delay the server asked for, if any.
    pub fn retry(&self) -> Option<Duration> {
        self.parser.retry
    }

    pub fn close(&mut self) {
        self.http.close();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::http::testserver;

    fn feed(p: &mut SseParser, text: &str) -> Vec<SseEvent> {
        text.split('\n').filter_map(|l| p.line(l.trim_end_matches('\r').as_bytes()).unwrap()).collect()
    }

    #[test]
    fn parser_follows_the_format() {
        let mut p = SseParser::new();
        let evs = feed(
            &mut p,
            "\u{feff}: comment\nevent: state\ndata: {\"a\":\ndata:1}\nid: 7\n\nretry: 1500\ndata\n\n\
             event: ping\n\nfield-without-colon\ndata: x\n\n",
        );
        assert_eq!(evs.len(), 3);
        assert_eq!((evs[0].event.as_str(), evs[0].data.as_str()), ("state", "{\"a\":\n1}"));
        assert_eq!(evs[0].id.as_deref(), Some("7"));
        assert_eq!((evs[1].event.as_str(), evs[1].data.as_str()), ("message", ""));
        assert_eq!(evs[2].data, "x", "an event without data is not dispatched; its type is dropped");
        assert_eq!(evs[2].event, "message");
        assert_eq!(p.retry, Some(Duration::from_millis(1500)));
    }

    #[test]
    fn stream_over_chunked_http_with_timeouts() {
        let (base, t, _rx) = testserver::serve(1, |_| {
            let body = "event: state\r\ndata: {\"changed\":{}}\r\n\r\n";
            let half = body.len() / 2;
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n\
                 {:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                half,
                &body[..half],
                body.len() - half,
                &body[half..]
            )
            .into_bytes()
        });
        let mut http = HttpClient::default();
        http.options.allow_plaintext_auth = true;
        let mut s = EventStream::open(&http, &format!("{base}/events"), &[]).unwrap();
        let ev = s.next_event(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(ev.event, "state");
        assert!(matches!(s.next_event(Duration::from_secs(5)), Err(NetError::Closed)));
        t.join().unwrap();
    }

    #[test]
    fn wrong_type_and_errors() {
        let (base, t, _rx) = testserver::serve(2, |seen| {
            if seen.path == "/a" { testserver::json(200, "{}") } else { testserver::json(401, "{}") }
        });
        let http = HttpClient::default();
        assert!(matches!(EventStream::open(&http, &format!("{base}/a"), &[]), Err(NetError::Protocol(_))));
        assert!(matches!(EventStream::open(&http, &format!("{base}/b"), &[]), Err(NetError::Auth(_))));
        t.join().unwrap();
    }

    #[test]
    fn a_quiet_stream_times_out_without_losing_data() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: st").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            s.write_all(b"ate\ndata: 1\n\n").unwrap();
            std::thread::sleep(Duration::from_millis(100));
        });
        let http = HttpClient::default();
        let mut s = EventStream::open(&http, &format!("http://127.0.0.1:{port}/"), &[]).unwrap();
        assert_eq!(s.next_event(Duration::from_millis(50)).unwrap(), None);
        let mut got = None;
        for _ in 0..50 {
            if let Some(e) = s.next_event(Duration::from_millis(100)).unwrap() {
                got = Some(e);
                break;
            }
        }
        assert_eq!(got.unwrap().event, "state");
        t.join().unwrap();
    }
}
