//! Blocking TCP/TLS transport shared by every protocol client: connect with timeouts, implicit TLS or a
//! STARTTLS upgrade, and a small read buffer with line/exact reads under size caps.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::error::{NetError, Result};

/// How certificates are checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsOptions {
    /// Trust the operating system's root store.
    pub use_system_roots: bool,
    /// Extra trust anchors (DER), e.g. a self-signed certificate the user accepted, or a test server's.
    pub extra_roots_der: Vec<Vec<u8>>,
    /// Accept any certificate. Only for a server the user explicitly chose to trust anyway.
    pub accept_invalid_certs: bool,
}

impl Default for TlsOptions {
    fn default() -> Self {
        TlsOptions { use_system_roots: true, extra_roots_der: Vec::new(), accept_invalid_certs: false }
    }
}

/// Timeouts and TLS settings for one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectOptions {
    pub connect_timeout: Duration,
    /// Read/write timeout for ordinary commands.
    pub io_timeout: Duration,
    pub tls: TlsOptions,
    /// Allow passwords, tokens and other credentials over an unencrypted connection (IMAP/SMTP without
    /// TLS, `http://`). Off by default: only for a server the user explicitly chose to trust that way.
    pub allow_plaintext_auth: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            connect_timeout: Duration::from_secs(30),
            io_timeout: Duration::from_secs(120),
            tls: TlsOptions::default(),
            allow_plaintext_auth: false,
        }
    }
}

impl ConnectOptions {
    /// Options that trust exactly one extra certificate (DER) and not the system roots: for local test servers.
    pub fn trusting(cert_der: Vec<u8>) -> Self {
        ConnectOptions {
            tls: TlsOptions {
                use_system_roots: false,
                extra_roots_der: vec![cert_der],
                accept_invalid_certs: false,
            },
            ..ConnectOptions::default()
        }
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<ClientConnection, TcpStream>>),
    /// Transitional state during a STARTTLS upgrade that failed.
    Gone,
}

/// `now + d`, saturating far in the future instead of overflowing (durations may come from servers).
pub fn deadline_after(d: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(d)
        .or_else(|| now.checked_add(std::time::Duration::from_secs(10 * 365 * 24 * 3600)))
        .unwrap_or(now)
}

/// A connected socket with a read buffer.
pub struct Conn {
    stream: Stream,
    buf: Vec<u8>,
    start: usize,
    host: String,
    opts: ConnectOptions,
}

const READ_CHUNK: usize = 16 * 1024;

fn provider() -> Arc<CryptoProvider> {
    static P: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

fn system_roots() -> &'static [CertificateDer<'static>] {
    static R: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();
    R.get_or_init(|| {
        let res = rustls_native_certs::load_native_certs();
        for e in &res.errors {
            log::warn!("loading system root certificates: {e}");
        }
        res.certs
    })
}

/// The operating system's trusted root certificates (DER), loaded once; S/MIME grades signatures against them.
pub fn system_root_ders() -> Vec<Vec<u8>> {
    system_roots().iter().map(|c| c.as_ref().to_vec()).collect()
}

fn client_config(opts: &TlsOptions) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(NetError::from)?;
    let config = if opts.accept_invalid_certs {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        if opts.use_system_roots {
            let (_added, _ignored) = roots.add_parsable_certificates(system_roots().iter().cloned());
        }
        for der in &opts.extra_roots_der {
            roots
                .add(CertificateDer::from(der.clone()))
                .map_err(|e| NetError::Tls(format!("extra root certificate: {e}")))?;
        }
        if roots.is_empty() {
            return Err(NetError::Tls("no trusted root certificates are available".into()));
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    Ok(Arc::new(config))
}

/// Certificate verifier that accepts any certificate but still checks handshake signatures.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn server_name(host: &str) -> Result<ServerName<'static>> {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(h.to_owned()).map_err(|e| NetError::Invalid(format!("server name `{host}`: {e}")))
}

fn tls_wrap(sock: TcpStream, host: &str, opts: &TlsOptions) -> Result<Stream> {
    let config = client_config(opts)?;
    let mut conn = ClientConnection::new(config, server_name(host)?)?;
    let mut sock = sock;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(|e| match NetError::from(e) {
            NetError::Io(m) => NetError::Tls(m),
            other => other,
        })?;
    }
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, sock))))
}

impl Conn {
    /// Connects to `host:port`; `implicit_tls` starts TLS immediately (IMAPS 993, SMTPS 465, HTTPS).
    pub fn connect(host: &str, port: u16, implicit_tls: bool, opts: &ConnectOptions) -> Result<Conn> {
        if host.is_empty() {
            return Err(NetError::Invalid("empty host name".into()));
        }
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<_> =
            (bare, port).to_socket_addrs().map_err(|e| NetError::Connect(format!("{host}: {e}")))?.collect();
        let mut last = NetError::Connect(format!("{host}: no addresses"));
        let mut sock = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, opts.connect_timeout) {
                Ok(s) => {
                    sock = Some(s);
                    break;
                }
                Err(e) => {
                    last = match NetError::from(e) {
                        NetError::Timeout(m) => {
                            NetError::Timeout(format!("connecting to {host}:{port}: {m}"))
                        }
                        other => NetError::Connect(format!("{host}:{port}: {other}")),
                    }
                }
            }
        }
        let sock = sock.ok_or(last)?;
        sock.set_read_timeout(Some(opts.io_timeout))?;
        sock.set_write_timeout(Some(opts.io_timeout))?;
        let _ = sock.set_nodelay(true);
        let stream = if implicit_tls { tls_wrap(sock, host, &opts.tls)? } else { Stream::Plain(sock) };
        Ok(Conn { stream, buf: Vec::new(), start: 0, host: host.to_owned(), opts: opts.clone() })
    }

    /// Whether the connection is encrypted.
    pub fn is_tls(&self) -> bool {
        matches!(self.stream, Stream::Tls(_))
    }

    /// Host name this connection was made to.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Local socket address (for the SMTP EHLO address literal).
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.tcp().and_then(|t| t.local_addr().ok())
    }

    /// The options the connection was made with.
    pub fn options(&self) -> &ConnectOptions {
        &self.opts
    }

    /// Upgrades a plain connection to TLS (after a successful STARTTLS reply). Bytes the server sent after
    /// that reply but before the handshake would be a command-injection attempt, so they are an error.
    pub fn start_tls(&mut self) -> Result<()> {
        if self.has_buffered() {
            return Err(NetError::Protocol("the server sent data before the TLS handshake".into()));
        }
        match std::mem::replace(&mut self.stream, Stream::Gone) {
            Stream::Plain(sock) => {
                self.stream = tls_wrap(sock, &self.host, &self.opts.tls)?;
                self.buf.clear();
                self.start = 0;
                Ok(())
            }
            Stream::Tls(t) => {
                self.stream = Stream::Tls(t);
                Err(NetError::Protocol("TLS is already active".into()))
            }
            Stream::Gone => Err(NetError::Closed),
        }
    }

    fn tcp(&self) -> Option<&TcpStream> {
        match &self.stream {
            Stream::Plain(s) => Some(s),
            Stream::Tls(t) => Some(t.get_ref()),
            Stream::Gone => None,
        }
    }

    /// Sets the read timeout (`None` blocks forever; avoid that).
    pub fn set_read_timeout(&mut self, t: Option<Duration>) -> Result<()> {
        let t = t.map(|d| d.max(Duration::from_millis(1)));
        self.tcp().ok_or(NetError::Closed)?.set_read_timeout(t)?;
        Ok(())
    }

    /// Restores the configured I/O timeout.
    pub fn reset_read_timeout(&mut self) -> Result<()> {
        let t = self.opts.io_timeout;
        self.set_read_timeout(Some(t))
    }

    /// True when unread bytes are buffered.
    pub fn has_buffered(&self) -> bool {
        self.start < self.buf.len()
    }

    fn read_raw(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.stream {
            Stream::Plain(s) => s.read(out),
            Stream::Tls(t) => t.read(out),
            Stream::Gone => Err(std::io::ErrorKind::NotConnected.into()),
        }
    }

    /// Reads more bytes into the buffer; errors with `Closed` at end of stream.
    fn fill(&mut self) -> Result<()> {
        if self.start > 0 && self.start >= self.buf.len() / 2 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = [0u8; READ_CHUNK];
        let n = loop {
            match self.read_raw(&mut chunk) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        };
        if n == 0 {
            return Err(NetError::Closed);
        }
        self.buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
        Ok(())
    }

    /// Reads one line, returned without its line ending (CRLF or a bare LF). A line longer than `max`
    /// bytes is an error.
    pub fn read_line(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut scanned = 0usize;
        loop {
            let avail = self.buf.get(self.start..).unwrap_or_default();
            if let Some(i) = avail.get(scanned..).and_then(|a| a.iter().position(|&b| b == b'\n')) {
                let nl = scanned + i;
                let mut line = avail.get(..nl).unwrap_or_default().to_vec();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > max {
                    // The cap holds even when an over-long line arrives in a single read.
                    return Err(NetError::TooLarge(format!("line longer than {max} bytes")));
                }
                self.start += nl + 1;
                return Ok(line);
            }
            scanned = avail.len();
            if scanned > max {
                return Err(NetError::TooLarge(format!("line longer than {max} bytes")));
            }
            self.fill()?;
        }
    }

    /// Reads exactly `n` bytes.
    pub fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.buf.len().saturating_sub(self.start) < n {
            self.fill()?;
        }
        let end = self.start.saturating_add(n);
        let out = self.buf.get(self.start..end).unwrap_or_default().to_vec();
        self.start = end;
        Ok(out)
    }

    /// Reads until the peer closes the connection, up to `max` bytes.
    pub fn read_to_end(&mut self, max: usize) -> Result<Vec<u8>> {
        loop {
            if self.buf.len().saturating_sub(self.start) > max {
                return Err(NetError::TooLarge(format!("body larger than {max} bytes")));
            }
            match self.fill() {
                Ok(()) => {}
                Err(NetError::Closed) => break,
                Err(e) => return Err(e),
            }
        }
        let out = self.buf.get(self.start..).unwrap_or_default().to_vec();
        self.start = self.buf.len();
        Ok(out)
    }

    /// Writes all bytes and flushes.
    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        match &mut self.stream {
            Stream::Plain(s) => {
                s.write_all(data)?;
                s.flush()?;
            }
            Stream::Tls(t) => {
                t.write_all(data)?;
                t.flush()?;
            }
            Stream::Gone => return Err(NetError::Closed),
        }
        Ok(())
    }

    /// Closes the connection (best effort; sends a TLS close_notify first).
    pub fn shutdown(&mut self) {
        match &mut self.stream {
            Stream::Plain(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            Stream::Tls(t) => {
                t.conn.send_close_notify();
                let _ = t.flush();
                let _ = t.get_ref().shutdown(std::net::Shutdown::Both);
            }
            Stream::Gone => {}
        }
        self.stream = Stream::Gone;
    }
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn").field("host", &self.host).field("tls", &self.is_tls()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn line_and_exact_reads() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"hello\r\nbare\nxyz12345").unwrap();
        });
        let mut c = Conn::connect("127.0.0.1", port, false, &ConnectOptions::default()).unwrap();
        assert_eq!(c.read_line(100).unwrap(), b"hello");
        assert_eq!(c.read_line(100).unwrap(), b"bare");
        assert_eq!(c.read_exact(3).unwrap(), b"xyz");
        assert_eq!(c.read_to_end(100).unwrap(), b"12345");
        assert_eq!(c.read_line(100), Err(NetError::Closed));
        t.join().unwrap();
    }

    #[test]
    fn long_line_is_capped() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let _ = s.write_all(&vec![b'a'; 100_000]);
        });
        let mut c = Conn::connect("127.0.0.1", port, false, &ConnectOptions::default()).unwrap();
        assert!(matches!(c.read_line(1000), Err(NetError::TooLarge(_))));
        t.join().unwrap();
    }

    #[test]
    fn line_over_the_cap_is_refused_even_when_it_arrives_whole() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut data = vec![b'a'; 2000];
            data.extend_from_slice(b"\r\nok\r\n");
            s.write_all(&data).unwrap();
        });
        let mut c = Conn::connect("127.0.0.1", port, false, &ConnectOptions::default()).unwrap();
        assert!(matches!(c.read_line(1000), Err(NetError::TooLarge(_))));
        t.join().unwrap();
    }

    #[test]
    fn read_timeout_reports_timeout() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            let (_s, _) = l.accept().unwrap();
            let _ = rx.recv();
        });
        let mut c = Conn::connect("127.0.0.1", port, false, &ConnectOptions::default()).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        assert!(matches!(c.read_line(100), Err(NetError::Timeout(_))));
        tx.send(()).unwrap();
        t.join().unwrap();
    }

    #[test]
    fn refused_port_is_connect_error() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let e = Conn::connect("127.0.0.1", port, false, &ConnectOptions::default()).unwrap_err();
        assert!(matches!(e, NetError::Connect(_) | NetError::Timeout(_)), "{e:?}");
    }
}
