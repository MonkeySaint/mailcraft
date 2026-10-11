//! A fake DNS responder on 127.0.0.1 (UDP) for discovery tests: answers SRV questions (RFC 2782) from the
//! records set with [`crate::TestServers::set_srv`], NXDOMAIN for every other name, and a truncated empty
//! answer for names set with [`crate::TestServers::set_srv_truncated`] (clients must retry over TCP, which
//! this responder does not offer, so such lookups fail). It never forwards questions anywhere.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::server::Ctx;

/// One SRV record: priority, weight, port, target ("" for ".", the service is not offered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Srv {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// Records by lower-case owner name; `None` = answer truncated.
pub type Zone = Arc<Mutex<BTreeMap<String, Option<Vec<Srv>>>>>;

/// Reads the question of a query: (id, name, qtype, end of the question section).
fn question(q: &[u8]) -> Option<(u16, String, u16, usize)> {
    let id = u16::from_be_bytes([*q.first()?, *q.get(1)?]);
    if u16::from_be_bytes([*q.get(4)?, *q.get(5)?]) != 1 {
        return None;
    }
    let mut pos = 12usize;
    let mut labels = Vec::new();
    loop {
        let len = usize::from(*q.get(pos)?);
        if len == 0 {
            pos += 1;
            break;
        }
        if len > 63 || labels.len() > 127 {
            return None;
        }
        labels.push(String::from_utf8_lossy(q.get(pos + 1..pos + 1 + len)?).to_ascii_lowercase());
        pos += 1 + len;
    }
    let qtype = u16::from_be_bytes([*q.get(pos)?, *q.get(pos + 1)?]);
    Some((id, labels.join("."), qtype, pos + 4))
}

fn push_name(out: &mut Vec<u8>, name: &str) {
    for l in name.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        let l = l.get(..63).unwrap_or(l);
        out.push(l.len() as u8);
        out.extend_from_slice(l.as_bytes());
    }
    out.push(0);
}

/// The answer to a query, or `None` for junk.
pub fn answer(q: &[u8], zone: &BTreeMap<String, Option<Vec<Srv>>>) -> Option<Vec<u8>> {
    let (id, name, qtype, qend) = question(q)?;
    let entry = zone.get(&name);
    let (flags, records): (u16, &[Srv]) = match entry {
        None => (0x8183, &[]),       // NXDOMAIN
        Some(None) => (0x8380, &[]), // TC
        Some(Some(r)) if qtype == 33 => (0x8180, r.as_slice()),
        Some(Some(_)) => (0x8180, &[]), // the name exists, no records of that type
    };
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(records.len().min(200) as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(q.get(12..qend)?);
    for r in records.iter().take(200) {
        out.extend_from_slice(&[0xC0, 12, 0, 33, 0, 1, 0, 0, 1, 44]);
        let mut rd = Vec::new();
        rd.extend_from_slice(&r.priority.to_be_bytes());
        rd.extend_from_slice(&r.weight.to_be_bytes());
        rd.extend_from_slice(&r.port.to_be_bytes());
        push_name(&mut rd, &r.target);
        out.extend_from_slice(&(rd.len() as u16).to_be_bytes());
        out.extend_from_slice(&rd);
    }
    Some(out)
}

/// Serves until the servers stop.
pub fn serve(sock: UdpSocket, ctx: Arc<Ctx>) {
    let _ = sock.set_read_timeout(Some(crate::conn::POLL));
    let mut buf = [0u8; 1500];
    while !ctx.stop.load(Ordering::SeqCst) {
        let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
        let reply = {
            let zone = ctx.dns.lock().unwrap_or_else(|e| e.into_inner());
            answer(buf.get(..n).unwrap_or_default(), &zone)
        };
        if let Some(r) = reply {
            let _ = sock.send_to(&r, peer);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn answers_and_junk() {
        let mut q = vec![0xAB, 0xCD, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        q.extend_from_slice(b"\x05_imap\x04_tcp\x07example\x03com\x00\x00\x21\x00\x01");
        let mut zone = BTreeMap::new();
        assert_eq!(answer(&q, &zone).unwrap()[3] & 0x0F, 3, "NXDOMAIN");
        zone.insert(
            "_imap._tcp.example.com".to_owned(),
            Some(vec![Srv { priority: 1, weight: 2, port: 143, target: "imap.example.com".into() }]),
        );
        let a = answer(&q, &zone).unwrap();
        assert_eq!(&a[..2], &[0xAB, 0xCD]);
        assert_eq!(a[7], 1, "one answer");
        for n in 0..q.len() {
            let _ = answer(&q[..n], &zone);
        }
    }
}
