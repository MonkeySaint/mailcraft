//! A tiny DNS client for service (SRV) lookups: RFC 1035 messages over UDP with a TCP retry for truncated
//! answers (RFC 7766), EDNS(0) for larger UDP answers (RFC 6891), SRV records (RFC 2782). Used for mail
//! service discovery (RFC 6186, RFC 8314 section 5.1, RFC 8620 section 2.2).
//!
//! The resolver to ask comes from `/etc/resolv.conf` (Unix) or is given explicitly (tests use a local fake
//! responder). Where no resolver is known, lookups are skipped rather than guessed. Every answer is hostile
//! input: lengths are checked, name compression is bounded, and a malformed message is an error.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{NetError, Result};

/// RR type SRV.
pub const TYPE_SRV: u16 = 33;
/// RR type OPT (EDNS).
const TYPE_OPT: u16 = 41;
/// Class IN.
const CLASS_IN: u16 = 1;
/// UDP payload size advertised with EDNS(0) (the DNS flag day 2020 recommendation).
const EDNS_UDP_SIZE: u16 = 1232;
/// Largest message accepted over TCP.
const MAX_TCP_MESSAGE: usize = 65_535;
/// Most compression pointers followed while reading one name.
const MAX_POINTER_JUMPS: usize = 32;
/// Most records read from one section.
const MAX_RECORDS: usize = 512;
/// Largest resolv.conf read.
const MAX_RESOLV_CONF: u64 = 64 * 1024;

/// One SRV record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    /// Target host without the trailing dot; `"."` (RFC 2782: the service is decidedly not available) is
    /// kept as an empty string.
    pub target: String,
}

impl SrvRecord {
    /// RFC 2782: a target of "." means the service is not offered at this domain.
    pub fn is_unavailable(&self) -> bool {
        self.target.is_empty()
    }
}

/// Orders records by priority (lowest first), then by weight (heaviest first), then by target, so the
/// preferred server comes first. (RFC 2782's weighted random choice only matters for load spreading.)
pub fn sort_srv(records: &mut [SrvRecord]) {
    records.sort_by(|a, b| {
        a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)).then_with(|| a.target.cmp(&b.target))
    });
}

/// A DNS server to ask, with timeouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolver {
    pub server: SocketAddr,
    /// Time to wait for one answer.
    pub timeout: Duration,
    /// UDP tries per question.
    pub attempts: u8,
}

impl Resolver {
    pub fn new(server: SocketAddr) -> Resolver {
        Resolver { server, timeout: Duration::from_secs(3), attempts: 2 }
    }

    /// The first name server of `/etc/resolv.conf`, if any (always `None` where that file does not exist).
    pub fn system() -> Option<Resolver> {
        system_name_servers().into_iter().next().map(Resolver::new)
    }

    /// SRV records for `name` (for example `_imaps._tcp.example.com`), sorted with [`sort_srv`]. A name that
    /// does not exist, or has no SRV records, gives an empty list.
    pub fn lookup_srv(&self, name: &str) -> Result<Vec<SrvRecord>> {
        let id = random_id()?;
        let query = encode_query(id, name, TYPE_SRV)?;
        let reply = self.exchange_udp(id, &query)?;
        let msg = match parse_response(&reply, id, name, TYPE_SRV) {
            Err(NetError::TooLarge(_)) => {
                // Truncated over UDP: ask again over TCP.
                let reply = self.exchange_tcp(&query)?;
                parse_response(&reply, id, name, TYPE_SRV)?
            }
            other => other?,
        };
        let mut out = msg;
        sort_srv(&mut out);
        Ok(out)
    }

    fn exchange_udp(&self, id: u16, query: &[u8]) -> Result<Vec<u8>> {
        let bind: SocketAddr = match self.server.ip() {
            IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };
        let sock = UdpSocket::bind(bind).map_err(|e| NetError::Connect(format!("DNS socket: {e}")))?;
        sock.connect(self.server)
            .map_err(|e| NetError::Connect(format!("DNS server {}: {e}", self.server)))?;
        let timeout = self.timeout.max(Duration::from_millis(10));
        let mut buf = vec![0u8; 4096];
        for _ in 0..self.attempts.max(1) {
            sock.send(query).map_err(|e| NetError::Io(format!("DNS send: {e}")))?;
            let deadline = crate::transport::deadline_after(timeout);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                sock.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
                match sock.recv(&mut buf) {
                    Ok(n) => {
                        let msg = buf.get(..n).unwrap_or_default();
                        // Datagrams that answer something else (a late reply, a spoofing attempt) are skipped.
                        if let [a, b, ..] = msg
                            && u16::from_be_bytes([*a, *b]) == id
                        {
                            return Ok(msg.to_vec());
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(NetError::Io(format!("DNS receive: {e}"))),
                }
            }
        }
        Err(NetError::Timeout(format!("no answer from the DNS server {}", self.server)))
    }

    fn exchange_tcp(&self, query: &[u8]) -> Result<Vec<u8>> {
        let timeout = self.timeout.max(Duration::from_millis(10));
        let mut s = TcpStream::connect_timeout(&self.server, timeout)
            .map_err(|e| NetError::Connect(format!("DNS server {} (TCP): {e}", self.server)))?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        let len = u16::try_from(query.len()).map_err(|_| NetError::Invalid("DNS query too long".into()))?;
        let mut out = len.to_be_bytes().to_vec();
        out.extend_from_slice(query);
        s.write_all(&out)?;
        let mut lb = [0u8; 2];
        s.read_exact(&mut lb)?;
        let n = usize::from(u16::from_be_bytes(lb));
        if n > MAX_TCP_MESSAGE {
            return Err(NetError::TooLarge("DNS answer".into()));
        }
        let mut msg = vec![0u8; n];
        s.read_exact(&mut msg)?;
        Ok(msg)
    }
}

fn random_id() -> Result<u16> {
    let mut b = [0u8; 2];
    SystemRandom::new().fill(&mut b).map_err(|_| NetError::Io("no random numbers".into()))?;
    Ok(u16::from_be_bytes(b))
}

/// Name servers listed in `/etc/resolv.conf` (port 53).
pub fn system_name_servers() -> Vec<SocketAddr> {
    #[cfg(unix)]
    {
        let Ok(f) = std::fs::File::open("/etc/resolv.conf") else { return Vec::new() };
        let mut text = String::new();
        if f.take(MAX_RESOLV_CONF).read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        parse_resolv_conf(&text)
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// The `nameserver` lines of a resolv.conf, in order (IPv6 zone ids are dropped; bad lines skipped).
pub fn parse_resolv_conf(text: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split(['#', ';']).next().unwrap_or_default();
        let mut words = line.split_whitespace();
        if words.next() != Some("nameserver") {
            continue;
        }
        let Some(addr) = words.next() else { continue };
        let addr = addr.split('%').next().unwrap_or_default();
        if let Ok(ip) = addr.parse::<IpAddr>() {
            out.push(SocketAddr::new(ip, 53));
        }
    }
    out
}

/// Checks and normalises a DNS name: ASCII letters, digits, `-` and `_` labels of 1–63 octets, at most 253
/// octets in all; a trailing dot is dropped.
pub fn normalize_name(name: &str) -> Result<String> {
    let n = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let bad = || NetError::Invalid(format!("not a DNS name: `{name}`"));
    if n.is_empty() || n.len() > 253 {
        return Err(bad());
    }
    for label in n.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(bad());
        }
    }
    Ok(n)
}

fn push_name(out: &mut Vec<u8>, name: &str) -> Result<()> {
    let n = normalize_name(name)?;
    for label in n.split('.') {
        let len = u8::try_from(label.len()).map_err(|_| NetError::Invalid("DNS label too long".into()))?;
        out.push(len);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(())
}

/// A standard recursive query for (`name`, `qtype`, IN) with an EDNS(0) OPT record.
pub fn encode_query(id: u16, name: &str, qtype: u16) -> Result<Vec<u8>> {
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    q.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    q.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    q.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    q.extend_from_slice(&1u16.to_be_bytes()); // ARCOUNT (OPT)
    push_name(&mut q, name)?;
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    // OPT: root name, type 41, class = UDP payload size, TTL 0 (extended RCODE, version 0, no DO), no data.
    q.push(0);
    q.extend_from_slice(&TYPE_OPT.to_be_bytes());
    q.extend_from_slice(&EDNS_UDP_SIZE.to_be_bytes());
    q.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    Ok(q)
}

struct Reader<'a> {
    msg: &'a [u8],
    pos: usize,
}

fn malformed(why: &str) -> NetError {
    NetError::Protocol(format!("malformed DNS answer: {why}"))
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8> {
        let b = *self.msg.get(self.pos).ok_or_else(|| malformed("too short"))?;
        self.pos += 1;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes([self.u8()?, self.u8()?, self.u8()?, self.u8()?]))
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        let end =
            self.pos.checked_add(n).filter(|e| *e <= self.msg.len()).ok_or_else(|| malformed("too short"))?;
        self.pos = end;
        Ok(())
    }
    /// Reads a (possibly compressed) name, leaving the reader after it.
    fn name(&mut self) -> Result<String> {
        let (name, end) = read_name(self.msg, self.pos)?;
        self.pos = end;
        Ok(name)
    }
}

/// Reads the name at `pos`; returns it (lower case, no trailing dot, "" for the root) and the offset after
/// it in the original position (compression pointers are followed, at most [`MAX_POINTER_JUMPS`]).
fn read_name(msg: &[u8], mut pos: usize) -> Result<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut end: Option<usize> = None;
    let mut jumps = 0usize;
    loop {
        let len = *msg.get(pos).ok_or_else(|| malformed("name runs past the end"))?;
        match len & 0xC0 {
            0x00 => {
                if len == 0 {
                    let after = end.unwrap_or(pos + 1);
                    return Ok((labels.join("."), after));
                }
                let n = usize::from(len);
                let label =
                    msg.get(pos + 1..pos + 1 + n).ok_or_else(|| malformed("label runs past the end"))?;
                total = total.saturating_add(n + 1);
                if total > 255 {
                    return Err(malformed("name too long"));
                }
                labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
                pos += 1 + n;
            }
            0xC0 => {
                let lo = *msg.get(pos + 1).ok_or_else(|| malformed("pointer runs past the end"))?;
                if end.is_none() {
                    end = Some(pos + 2);
                }
                jumps += 1;
                if jumps > MAX_POINTER_JUMPS {
                    return Err(malformed("compression loop"));
                }
                pos = (usize::from(len & 0x3F) << 8) | usize::from(lo);
            }
            _ => return Err(malformed("unknown label type")),
        }
    }
}

/// Parses an answer to `encode_query(id, name, qtype)`: the SRV records of the answer section. NXDOMAIN or
/// no answers give an empty list; a truncated answer gives [`NetError::TooLarge`] (retry over TCP); a server
/// failure or refusal is an error.
pub fn parse_response(msg: &[u8], id: u16, name: &str, qtype: u16) -> Result<Vec<SrvRecord>> {
    let mut r = Reader { msg, pos: 0 };
    if r.u16()? != id {
        return Err(malformed("wrong id"));
    }
    let flags = r.u16()?;
    if flags & 0x8000 == 0 {
        return Err(malformed("not a response"));
    }
    if flags & 0x7800 != 0 {
        return Err(malformed("unexpected opcode"));
    }
    let qd = r.u16()?;
    let an = r.u16()?;
    let _ns = r.u16()?;
    let _ar = r.u16()?;
    if flags & 0x0200 != 0 {
        return Err(NetError::TooLarge("DNS answer truncated".into()));
    }
    match flags & 0x000F {
        0 => {}
        3 => return Ok(Vec::new()), // NXDOMAIN
        2 => return Err(NetError::Protocol("the DNS server failed (SERVFAIL)".into())),
        5 => return Err(NetError::Refused("the DNS server refused the query".into())),
        n => return Err(NetError::Protocol(format!("DNS error code {n}"))),
    }
    let want = normalize_name(name)?;
    if qd != 1 {
        return Err(malformed("question count"));
    }
    let qname = r.name()?;
    let qt = r.u16()?;
    let _qc = r.u16()?;
    if qname != want || qt != qtype {
        return Err(malformed("answers another question"));
    }
    let mut out = Vec::new();
    for _ in 0..usize::from(an).min(MAX_RECORDS) {
        let _owner = r.name()?;
        let rtype = r.u16()?;
        let class = r.u16()?;
        let _ttl = r.u32()?;
        let rdlen = usize::from(r.u16()?);
        let start = r.pos;
        if start.checked_add(rdlen).is_none_or(|e| e > msg.len()) {
            return Err(malformed("record data runs past the end"));
        }
        if rtype == TYPE_SRV && class == CLASS_IN && qtype == TYPE_SRV {
            if rdlen < 7 {
                return Err(malformed("short SRV record"));
            }
            let mut rd = Reader { msg, pos: start };
            let priority = rd.u16()?;
            let weight = rd.u16()?;
            let port = rd.u16()?;
            let target = rd.name()?;
            if rd.pos > start + rdlen {
                return Err(malformed("SRV target runs past its record"));
            }
            out.push(SrvRecord { priority, weight, port, target });
        }
        r.pos = start;
        r.skip(rdlen)?;
    }
    Ok(out)
}

/// Builds a response message (for tests and the local fake responder): the question plus SRV answers, or
/// NXDOMAIN when `records` is `None`.
pub fn encode_srv_response(query: &[u8], records: Option<&[SrvRecord]>) -> Result<Vec<u8>> {
    let mut r = Reader { msg: query, pos: 0 };
    let id = r.u16()?;
    let _flags = r.u16()?;
    if r.u16()? != 1 {
        return Err(malformed("question count"));
    }
    r.skip(6)?;
    let qstart = r.pos;
    r.name()?;
    let qtype = r.u16()?;
    let _qclass = r.u16()?;
    let question = query.get(qstart..r.pos).ok_or_else(|| malformed("question"))?;
    let answers: &[SrvRecord] = if qtype == TYPE_SRV { records.unwrap_or_default() } else { &[] };
    let rcode: u16 = if records.is_none() { 3 } else { 0 };
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(0x8180u16 | rcode).to_be_bytes()); // QR, RD, RA
    out.extend_from_slice(&1u16.to_be_bytes());
    let an = u16::try_from(answers.len()).map_err(|_| NetError::Invalid("too many records".into()))?;
    out.extend_from_slice(&an.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(question);
    for rec in answers {
        out.extend_from_slice(&[0xC0, 12]); // the question's name
        out.extend_from_slice(&TYPE_SRV.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&300u32.to_be_bytes());
        let mut rd = Vec::new();
        rd.extend_from_slice(&rec.priority.to_be_bytes());
        rd.extend_from_slice(&rec.weight.to_be_bytes());
        rd.extend_from_slice(&rec.port.to_be_bytes());
        if rec.target.is_empty() {
            rd.push(0);
        } else {
            push_name(&mut rd, &rec.target)?;
        }
        let len = u16::try_from(rd.len()).map_err(|_| NetError::Invalid("record too long".into()))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&rd);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn srv(p: u16, w: u16, port: u16, t: &str) -> SrvRecord {
        SrvRecord { priority: p, weight: w, port, target: t.into() }
    }

    #[test]
    fn query_layout() {
        let q = encode_query(0x1234, "_imaps._tcp.Example.COM.", TYPE_SRV).unwrap();
        assert_eq!(&q[..2], &[0x12, 0x34]);
        assert_eq!(&q[2..4], &[0x01, 0x00], "recursion desired");
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(&q[10..12], &[0, 1], "one OPT record");
        let name = b"\x06_imaps\x04_tcp\x07example\x03com\x00";
        assert_eq!(&q[12..12 + name.len()], name);
        assert!(encode_query(1, "bad name.example", TYPE_SRV).is_err());
        assert!(encode_query(1, &format!("{}.example", "a".repeat(64)), TYPE_SRV).is_err());
        assert!(encode_query(1, "", TYPE_SRV).is_err());
        assert!(encode_query(1, "bücher.example", TYPE_SRV).is_err(), "only ASCII (A-labels) names");
    }

    #[test]
    fn round_trip_and_ordering() {
        let name = "_submission._tcp.example.com";
        let q = encode_query(7, name, TYPE_SRV).unwrap();
        let recs = [srv(20, 0, 587, "backup.example.com"), srv(10, 5, 587, "mail.example.com")];
        let a = encode_srv_response(&q, Some(&recs)).unwrap();
        let mut got = parse_response(&a, 7, name, TYPE_SRV).unwrap();
        sort_srv(&mut got);
        assert_eq!(got, vec![srv(10, 5, 587, "mail.example.com"), srv(20, 0, 587, "backup.example.com")]);
        // NXDOMAIN and "." (not offered).
        let nx = encode_srv_response(&q, None).unwrap();
        assert!(parse_response(&nx, 7, name, TYPE_SRV).unwrap().is_empty());
        let dot = encode_srv_response(&q, Some(&[srv(0, 0, 0, "")])).unwrap();
        let got = parse_response(&dot, 7, name, TYPE_SRV).unwrap();
        assert!(got[0].is_unavailable());
        // Wrong id, wrong question.
        assert!(parse_response(&a, 8, name, TYPE_SRV).is_err());
        assert!(parse_response(&a, 7, "_imap._tcp.example.com", TYPE_SRV).is_err());
    }

    #[test]
    fn truncated_servfail_and_garbage() {
        let name = "_imap._tcp.example.org";
        let q = encode_query(9, name, TYPE_SRV).unwrap();
        let mut a = encode_srv_response(&q, Some(&[srv(0, 0, 143, "imap.example.org")])).unwrap();
        a[2] |= 0x02; // TC
        assert!(matches!(parse_response(&a, 9, name, TYPE_SRV), Err(NetError::TooLarge(_))));
        a[2] &= !0x02;
        a[3] = (a[3] & 0xF0) | 2;
        assert!(parse_response(&a, 9, name, TYPE_SRV).is_err());
        // Every prefix of a valid answer and some junk: errors, never panics.
        let good = encode_srv_response(&q, Some(&[srv(0, 0, 143, "imap.example.org")])).unwrap();
        for n in 0..good.len() {
            let _ = parse_response(&good[..n], 9, name, TYPE_SRV);
        }
        for b in 0..=255u8 {
            let mut x = good.clone();
            let i = usize::from(b) % x.len();
            x[i] = b;
            let _ = parse_response(&x, 9, name, TYPE_SRV);
        }
    }

    #[test]
    fn compression_loops_are_refused() {
        // Header + question whose name is a pointer to itself.
        let mut m = vec![0, 1, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        m.extend_from_slice(&[0xC0, 12, 0, 33, 0, 1]);
        assert!(parse_response(&m, 1, "x.example", TYPE_SRV).is_err());
        assert!(read_name(&m, 12).is_err());
    }

    #[test]
    fn resolv_conf() {
        let t = "# comment\nsearch example.com\nnameserver 192.0.2.53\nnameserver fe80::1%eth0 ; x\n\
                 nameserver nonsense\noptions edns0\n";
        let got = parse_resolv_conf(t);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], "192.0.2.53:53".parse().unwrap());
        assert_eq!(got[1].ip(), "fe80::1".parse::<IpAddr>().unwrap());
        assert!(parse_resolv_conf("").is_empty());
    }

    #[test]
    fn lookup_over_udp_with_tcp_retry() {
        use std::net::TcpListener;
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = udp.local_addr().unwrap();
        let tcp = TcpListener::bind(addr).unwrap();
        let recs = vec![srv(0, 1, 993, "imap.example.net")];
        let r2 = recs.clone();
        let h = std::thread::spawn(move || {
            // First a stray datagram with another id, then a truncated answer.
            let mut buf = [0u8; 512];
            let (n, peer) = udp.recv_from(&mut buf).unwrap();
            let q = buf[..n].to_vec();
            let mut stray = encode_srv_response(&q, Some(&r2)).unwrap();
            stray[0] ^= 0xFF;
            udp.send_to(&stray, peer).unwrap();
            let mut tc = encode_srv_response(&q, Some(&[])).unwrap();
            tc[2] |= 0x02;
            udp.send_to(&tc, peer).unwrap();
            let (mut s, _) = tcp.accept().unwrap();
            let mut lb = [0u8; 2];
            s.read_exact(&mut lb).unwrap();
            let mut tq = vec![0u8; usize::from(u16::from_be_bytes(lb))];
            s.read_exact(&mut tq).unwrap();
            let a = encode_srv_response(&tq, Some(&r2)).unwrap();
            let mut out = (a.len() as u16).to_be_bytes().to_vec();
            out.extend_from_slice(&a);
            s.write_all(&out).unwrap();
        });
        let res = Resolver::new(addr).lookup_srv("_imaps._tcp.example.net").unwrap();
        h.join().unwrap();
        assert_eq!(res, recs);
    }

    #[test]
    fn silent_server_times_out() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut r = Resolver::new(udp.local_addr().unwrap());
        r.timeout = Duration::from_millis(50);
        r.attempts = 1;
        assert!(matches!(r.lookup_srv("_imaps._tcp.example.com"), Err(NetError::Timeout(_))));
    }
}
