//! STARTTLS against a scripted TLS-capable server on 127.0.0.1: nothing a man in the middle injects before
//! the handshake may survive it (RFC 9051 section 6.2.1, RFC 3207 section 4.2).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use mailcraft_net::imap::ImapClient;
use mailcraft_net::{ConnectOptions, Security};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Reads one line byte by byte (so nothing past it is consumed), without its line ending.
fn read_line<R: Read>(r: &mut R) -> String {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = r.read(&mut b) {
        if b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
    }
    String::from_utf8_lossy(&out).trim_end_matches('\r').to_owned()
}

fn server_config() -> (Arc<rustls::ServerConfig>, Vec<u8>) {
    let ck =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    let cert_der = ck.cert.der().to_vec();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert_der.clone())], key)
        .unwrap();
    (Arc::new(cfg), cert_der)
}

#[test]
fn state_injected_before_imap_starttls_is_discarded() {
    let (cfg, cert_der) = server_config();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS ENABLE UTF8=ACCEPT] hi\r\n").unwrap();
        assert_eq!(read_line(&mut s), "M0001 STARTTLS");
        // Plaintext slipped in before the tagged OK: it must not change the session.
        s.write_all(b"* ENABLED UTF8=ACCEPT\r\nM0001 OK begin TLS\r\n").unwrap();
        let mut tls = rustls::StreamOwned::new(rustls::ServerConnection::new(cfg).unwrap(), s);
        let mut seen = vec![read_line(&mut tls)];
        tls.write_all(b"* CAPABILITY IMAP4rev1 ENABLE AUTH=PLAIN\r\nM0002 OK done\r\n").unwrap();
        seen.push(read_line(&mut tls));
        tls.write_all(b"M0003 OK created\r\n").unwrap();
        tls.flush().unwrap();
        seen
    });
    let opts = ConnectOptions { io_timeout: Duration::from_secs(10), ..ConnectOptions::trusting(cert_der) };
    let mut c = ImapClient::connect("127.0.0.1", port, Security::StartTls, &opts).unwrap();
    assert!(c.is_tls());
    assert!(!c.is_enabled("UTF8=ACCEPT"), "ENABLED from before TLS survived the handshake");
    c.create("Café").unwrap();
    assert_eq!(t.join().unwrap(), vec!["M0002 CAPABILITY".to_owned(), "M0003 CREATE Caf&AOk-".to_owned()]);
}
