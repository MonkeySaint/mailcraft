//! A tiny DNS client for service (SRV) and mail exchanger (MX) lookups: RFC 1035 messages over UDP with a TCP
//! retry for truncated answers (RFC 7766), EDNS(0) for larger UDP answers (RFC 6891), SRV records (RFC 2782)
//! and MX records (RFC 1035 section 3.3.9, the null MX of RFC 7505). Used for mail service discovery
//! (RFC 6186, RFC 8314 section 5.1, RFC 8620 section 2.2) and to recognise well-known mail hosts by their MX.
//!
//! The resolver to ask comes from `/etc/resolv.conf` (Unix), the DNS servers of the active network adapters
//! (Windows), or is given explicitly (tests use a local fake responder). Where no resolver is known, lookups are skipped rather than guessed. Every answer is hostile
//! input: lengths are checked, name compression is bounded, and a malformed message is an error.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{NetError, Result};

/// RR type SRV.
pub const TYPE_SRV: u16 = 33;
/// RR type MX.
pub const TYPE_MX: u16 = 15;
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
#[cfg(unix)]
const MAX_RESOLV_CONF: u64 = 64 * 1024;
/// Most system name servers kept.
const MAX_NAME_SERVERS: usize = 16;

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

/// One MX record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MxRecord {
    pub preference: u16,
    /// Mail exchanger host without the trailing dot; `"."` (RFC 7505 null MX: the domain accepts no mail) is
    /// kept as an empty string.
    pub exchange: String,
}

impl MxRecord {
    /// RFC 7505: a "." exchange means the domain does not accept mail.
    pub fn is_null(&self) -> bool {
        self.exchange.is_empty()
    }
}

/// Orders MX records by preference (lowest first), then by exchange name.
pub fn sort_mx(records: &mut [MxRecord]) {
    records.sort_by(|a, b| a.preference.cmp(&b.preference).then_with(|| a.exchange.cmp(&b.exchange)));
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

    /// The first of [`system_name_servers`], if any.
    pub fn system() -> Option<Resolver> {
        system_name_servers().into_iter().next().map(Resolver::new)
    }

    /// SRV records for `name` (for example `_imaps._tcp.example.com`), sorted with [`sort_srv`]. A name that
    /// does not exist, or has no SRV records, gives an empty list.
    pub fn lookup_srv(&self, name: &str) -> Result<Vec<SrvRecord>> {
        let mut out = self.lookup(name, TYPE_SRV, parse_response)?;
        sort_srv(&mut out);
        Ok(out)
    }

    /// MX records for `name` (a mail domain), sorted with [`sort_mx`]. A name that does not exist, or has no
    /// MX records, gives an empty list.
    pub fn lookup_mx(&self, name: &str) -> Result<Vec<MxRecord>> {
        let mut out = self.lookup(name, TYPE_MX, parse_mx_response)?;
        sort_mx(&mut out);
        Ok(out)
    }

    /// Asks one question and reads the answer with `parse`, again over TCP when the UDP answer is truncated.
    fn lookup<T>(&self, name: &str, qtype: u16, parse: ParseFn<T>) -> Result<Vec<T>> {
        let id = random_id()?;
        let query = encode_query(id, name, qtype)?;
        let reply = self.exchange_udp(id, &query)?;
        match parse(&reply, id, name, qtype) {
            Err(NetError::TooLarge(_)) => {
                // Truncated over UDP: ask again over TCP.
                let reply = self.exchange_tcp(&query)?;
                parse(&reply, id, name, qtype)
            }
            other => other,
        }
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

/// Reads the records of an answer: (message, id, name, qtype).
type ParseFn<T> = fn(&[u8], u16, &str, u16) -> Result<Vec<T>>;

fn random_id() -> Result<u16> {
    let mut b = [0u8; 2];
    SystemRandom::new().fill(&mut b).map_err(|_| NetError::Io("no random numbers".into()))?;
    Ok(u16::from_be_bytes(b))
}

/// The system's name servers (port 53): the `nameserver` lines of `/etc/resolv.conf` on Unix, the DNS servers
/// of the active network adapters on Windows (see [`adapter_name_servers`]); empty elsewhere.
pub fn system_name_servers() -> Vec<SocketAddr> {
    #[cfg(unix)]
    {
        let Ok(f) = std::fs::File::open("/etc/resolv.conf") else { return Vec::new() };
        let mut text = String::new();
        if f.take(MAX_RESOLV_CONF).read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        let mut out = parse_resolv_conf(&text);
        out.truncate(MAX_NAME_SERVERS);
        out
    }
    #[cfg(windows)]
    {
        match ipconfig::get_adapters() {
            Ok(list) => {
                let adapters: Vec<AdapterDns> = list
                    .iter()
                    .map(|a| AdapterDns {
                        up: a.oper_status() == ipconfig::OperStatus::IfOperStatusUp,
                        has_gateway: !a.gateways().is_empty(),
                        metric: a.ipv4_metric().min(a.ipv6_metric()),
                        dns_servers: a.dns_servers().to_vec(),
                    })
                    .collect();
                adapter_name_servers(&adapters)
            }
            Err(e) => {
                log::warn!("cannot list the network adapters for their DNS servers: {e}");
                Vec::new()
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

/// What [`adapter_name_servers`] needs to know about one network adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdapterDns {
    /// Operationally up.
    pub up: bool,
    /// Has a default gateway (reaches beyond the local link).
    pub has_gateway: bool,
    /// Interface metric (lower is preferred).
    pub metric: u32,
    /// The adapter's DNS servers, in its order.
    pub dns_servers: Vec<IpAddr>,
}

/// The name servers of network adapters that are up, as port-53 addresses: adapters with a gateway first, then
/// by metric, each adapter's servers in its own order; duplicates dropped, at most [`MAX_NAME_SERVERS`].
/// Unusable addresses are skipped: unspecified, broadcast, multicast, IPv6 link-local (no zone id is known) and
/// the deprecated site-local `fec0::/10` placeholders Windows lists when no IPv6 DNS server is configured.
pub fn adapter_name_servers(adapters: &[AdapterDns]) -> Vec<SocketAddr> {
    let mut up: Vec<&AdapterDns> = adapters.iter().filter(|a| a.up).collect();
    // Stable: adapters of equal rank keep the system's order.
    up.sort_by_key(|a| (!a.has_gateway, a.metric));
    let mut out: Vec<SocketAddr> = Vec::new();
    for ip in up.iter().flat_map(|a| a.dns_servers.iter()) {
        if out.len() >= MAX_NAME_SERVERS {
            break;
        }
        let addr = SocketAddr::new(*ip, 53);
        if usable_name_server(ip) && !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

fn usable_name_server(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_multicast() && !v4.is_broadcast(),
        IpAddr::V6(v6) => {
            let first = v6.segments().first().copied().unwrap_or_default();
            // fe80::/10 link-local, fec0::/10 site-local (deprecated by RFC 3879).
            !v6.is_unspecified() && !v6.is_multicast() && first & 0xffc0 != 0xfe80 && first & 0xffc0 != 0xfec0
        }
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
    parse_records(msg, id, name, qtype, |msg, rtype, start, rdlen| {
        if rtype != TYPE_SRV || qtype != TYPE_SRV {
            return Ok(None);
        }
        if rdlen < 7 {
            return Err(malformed("short SRV record"));
        }
        let mut rd = Reader { msg, pos: start };
        let priority = rd.u16()?;
        let weight = rd.u16()?;
        let port = rd.u16()?;
        let target = rd.name()?;
        if rd.pos > start.saturating_add(rdlen) {
            return Err(malformed("SRV target runs past its record"));
        }
        Ok(Some(SrvRecord { priority, weight, port, target }))
    })
}

/// Parses an answer to `encode_query(id, name, TYPE_MX)`: the MX records of the answer section (records of
/// other types, such as the CNAME chain a resolver puts first, are skipped). Errors as [`parse_response`].
pub fn parse_mx_response(msg: &[u8], id: u16, name: &str, qtype: u16) -> Result<Vec<MxRecord>> {
    parse_records(msg, id, name, qtype, |msg, rtype, start, rdlen| {
        if rtype != TYPE_MX || qtype != TYPE_MX {
            return Ok(None);
        }
        if rdlen < 3 {
            return Err(malformed("short MX record"));
        }
        let mut rd = Reader { msg, pos: start };
        let preference = rd.u16()?;
        let exchange = rd.name()?;
        if rd.pos > start.saturating_add(rdlen) {
            return Err(malformed("MX exchange runs past its record"));
        }
        Ok(Some(MxRecord { preference, exchange }))
    })
}

/// Checks the header and the question of an answer, then hands each answer record of class IN to `read`
/// (message, record type, offset of the record data, its length); `None` skips the record.
fn parse_records<T>(
    msg: &[u8],
    id: u16,
    name: &str,
    qtype: u16,
    mut read: impl FnMut(&[u8], u16, usize, usize) -> Result<Option<T>>,
) -> Result<Vec<T>> {
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
        if class == CLASS_IN
            && let Some(rec) = read(msg, rtype, start, rdlen)?
        {
            out.push(rec);
        }
        r.pos = start;
        r.skip(rdlen)?;
    }
    Ok(out)
}

/// Builds a response message (for tests and the local fake responder): the question plus SRV answers, or
/// NXDOMAIN when `records` is `None`.
pub fn encode_srv_response(query: &[u8], records: Option<&[SrvRecord]>) -> Result<Vec<u8>> {
    let rdata = records
        .map(|recs| {
            recs.iter()
                .map(|rec| {
                    let mut rd = Vec::new();
                    rd.extend_from_slice(&rec.priority.to_be_bytes());
                    rd.extend_from_slice(&rec.weight.to_be_bytes());
                    rd.extend_from_slice(&rec.port.to_be_bytes());
                    push_target(&mut rd, &rec.target)?;
                    Ok(rd)
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    encode_response(query, TYPE_SRV, rdata)
}

/// Builds a response message with MX answers (or NXDOMAIN for `None`), like [`encode_srv_response`].
pub fn encode_mx_response(query: &[u8], records: Option<&[MxRecord]>) -> Result<Vec<u8>> {
    let rdata = records
        .map(|recs| {
            recs.iter()
                .map(|rec| {
                    let mut rd = rec.preference.to_be_bytes().to_vec();
                    push_target(&mut rd, &rec.exchange)?;
                    Ok(rd)
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    encode_response(query, TYPE_MX, rdata)
}

/// A target name in record data; "" is the root (".").
fn push_target(rd: &mut Vec<u8>, target: &str) -> Result<()> {
    if target.is_empty() {
        rd.push(0);
        Ok(())
    } else {
        push_name(rd, target)
    }
}

/// The question of `query` plus answers of type `rtype` (their record data given) when the query asks for
/// that type; NXDOMAIN for `None`.
fn encode_response(query: &[u8], rtype: u16, rdata: Option<Vec<Vec<u8>>>) -> Result<Vec<u8>> {
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
    let rcode: u16 = if rdata.is_none() { 3 } else { 0 };
    let answers: Vec<Vec<u8>> = if qtype == rtype { rdata.unwrap_or_default() } else { Vec::new() };
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(0x8180u16 | rcode).to_be_bytes()); // QR, RD, RA
    out.extend_from_slice(&1u16.to_be_bytes());
    let an = u16::try_from(answers.len()).map_err(|_| NetError::Invalid("too many records".into()))?;
    out.extend_from_slice(&an.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(question);
    for rd in answers {
        out.extend_from_slice(&[0xC0, 12]); // the question's name
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&300u32.to_be_bytes());
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
    fn mx_round_trip_null_mx_and_garbage() {
        let name = "example.org";
        let q = encode_query(11, name, TYPE_MX).unwrap();
        let recs = [
            MxRecord { preference: 20, exchange: "alt.example.org".into() },
            MxRecord { preference: 10, exchange: "Mail.Example.ORG".into() },
        ];
        let a = encode_mx_response(&q, Some(&recs)).unwrap();
        let mut got = parse_mx_response(&a, 11, name, TYPE_MX).unwrap();
        sort_mx(&mut got);
        assert_eq!(got[0], MxRecord { preference: 10, exchange: "mail.example.org".into() });
        assert_eq!(got.len(), 2);
        // An SRV answer read as MX (and the other way round) has no records of the wanted type.
        assert!(parse_response(&a, 11, name, TYPE_MX).unwrap().is_empty());
        let null =
            encode_mx_response(&q, Some(&[MxRecord { preference: 0, exchange: String::new() }])).unwrap();
        assert!(parse_mx_response(&null, 11, name, TYPE_MX).unwrap()[0].is_null());
        assert!(
            parse_mx_response(&encode_mx_response(&q, None).unwrap(), 11, name, TYPE_MX).unwrap().is_empty()
        );
        for n in 0..a.len() {
            let _ = parse_mx_response(&a[..n], 11, name, TYPE_MX);
        }
        for b in 0..=255u8 {
            let mut x = a.clone();
            let i = usize::from(b) % x.len();
            x[i] = b;
            let _ = parse_mx_response(&x, 11, name, TYPE_MX);
        }
    }

    #[test]
    fn mx_lookup_over_udp() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = udp.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            let (n, peer) = udp.recv_from(&mut buf).unwrap();
            let recs = [
                MxRecord { preference: 5, exchange: "b.example.net".into() },
                MxRecord { preference: 1, exchange: "a.example.net".into() },
            ];
            udp.send_to(&encode_mx_response(&buf[..n], Some(&recs)).unwrap(), peer).unwrap();
        });
        let got = Resolver::new(addr).lookup_mx("example.net").unwrap();
        h.join().unwrap();
        let names: Vec<&str> = got.iter().map(|r| r.exchange.as_str()).collect();
        assert_eq!(names, ["a.example.net", "b.example.net"]);
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
    fn adapter_servers_in_order_without_duplicates() {
        let adapter = |up, has_gateway, metric, dns: &[&str]| AdapterDns {
            up,
            has_gateway,
            metric,
            dns_servers: dns.iter().map(|s| s.parse::<IpAddr>().unwrap()).collect(),
        };
        let list = [
            // A virtual switch: up, no gateway.
            adapter(true, false, 5, &["192.0.2.200"]),
            // Down: ignored.
            adapter(false, true, 1, &["192.0.2.99"]),
            // Wi-Fi with a gateway, higher metric.
            adapter(true, true, 50, &["192.0.2.2", "192.0.2.1"]),
            // Ethernet with a gateway, preferred by metric; placeholders, link-local and junk skipped.
            adapter(
                true,
                true,
                10,
                &["192.0.2.1", "2001:db8::53", "fec0:0:0:ffff::1", "fe80::1", "0.0.0.0", "::", "ff02::1"],
            ),
        ];
        let got: Vec<String> = adapter_name_servers(&list).iter().map(ToString::to_string).collect();
        assert_eq!(got, ["192.0.2.1:53", "[2001:db8::53]:53", "192.0.2.2:53", "192.0.2.200:53"]);
        assert!(adapter_name_servers(&[]).is_empty());
        assert!(adapter_name_servers(&[adapter(false, true, 1, &["192.0.2.1"])]).is_empty());

        // Capped.
        let many: Vec<String> = (1..=40).map(|i| format!("192.0.2.{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(adapter_name_servers(&[adapter(true, true, 1, &many)]).len(), MAX_NAME_SERVERS);
    }

    #[test]
    fn lookup_over_udp_with_tcp_retry() {
        use std::net::TcpListener;
        // The same port for UDP and TCP; another socket (a busy machine's other tests) may hold the TCP one.
        let (udp, addr, tcp) = (0..20)
            .find_map(|_| {
                let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
                let addr = udp.local_addr().unwrap();
                TcpListener::bind(addr).ok().map(|tcp| (udp, addr, tcp))
            })
            .unwrap();
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
