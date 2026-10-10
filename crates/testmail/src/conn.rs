//! A client connection: plain TCP or TLS (implicit or after STARTTLS), with a line reader that polls so
//! threads notice shutdown and so IDLE can interleave reads with change notifications.

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// Socket read timeout: how often a blocked reader re-checks the stop flag.
pub const POLL: Duration = Duration::from_millis(40);

enum Transport {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
    Closed,
}

pub enum Poll {
    Line(Vec<u8>),
    Pending,
    Eof,
}

pub struct Conn {
    t: Transport,
    buf: Vec<u8>,
    stop: Arc<AtomicBool>,
    tls: Arc<ServerConfig>,
    idle_timeout: Duration,
    last_activity: Instant,
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

impl Conn {
    pub fn new(
        tcp: TcpStream,
        stop: Arc<AtomicBool>,
        tls: Arc<ServerConfig>,
        implicit_tls: bool,
        idle_timeout: Duration,
    ) -> io::Result<Conn> {
        tcp.set_read_timeout(Some(POLL))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
        let _ = tcp.set_nodelay(true);
        let mut c = Conn {
            t: Transport::Plain(tcp),
            buf: Vec::new(),
            stop,
            tls,
            idle_timeout,
            last_activity: Instant::now(),
        };
        if implicit_tls {
            c.start_tls()?;
        }
        Ok(c)
    }

    pub fn is_tls(&self) -> bool {
        matches!(self.t, Transport::Tls(_))
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Switches to TLS. Any plaintext the client pipelined after the STARTTLS command is discarded (it must
    /// not be interpreted as protected data).
    pub fn start_tls(&mut self) -> io::Result<()> {
        self.buf.clear();
        let tcp = match std::mem::replace(&mut self.t, Transport::Closed) {
            Transport::Plain(tcp) => tcp,
            other => {
                self.t = other;
                return Err(io::Error::other("TLS already active"));
            }
        };
        let conn = ServerConnection::new(Arc::clone(&self.tls)).map_err(io::Error::other)?;
        let mut s = Box::new(StreamOwned::new(conn, tcp));
        // Finish the handshake here, tolerating the polling read timeout.
        let started = Instant::now();
        while s.conn.is_handshaking() {
            if self.stopping() || started.elapsed() > Duration::from_secs(30) {
                return Err(io::Error::from(ErrorKind::TimedOut));
            }
            match s.conn.complete_io(&mut s.sock) {
                Ok(_) => {}
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e),
            }
        }
        self.t = Transport::Tls(s);
        self.last_activity = Instant::now();
        Ok(())
    }

    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.t {
            Transport::Plain(s) => {
                s.write_all(data)?;
                s.flush()
            }
            Transport::Tls(s) => {
                s.write_all(data)?;
                s.flush()
            }
            Transport::Closed => Err(io::Error::from(ErrorKind::NotConnected)),
        }
    }

    /// One read attempt; Ok(None) on timeout, Ok(Some(0)) on EOF.
    fn fill(&mut self) -> io::Result<Option<usize>> {
        let mut tmp = [0u8; 16 * 1024];
        let r = match &mut self.t {
            Transport::Plain(s) => s.read(&mut tmp),
            Transport::Tls(s) => s.read(&mut tmp),
            Transport::Closed => return Ok(Some(0)),
        };
        match r {
            Ok(n) => {
                if n > 0 {
                    self.last_activity = Instant::now();
                }
                self.buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
                Ok(Some(n))
            }
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof || e.kind() == ErrorKind::ConnectionReset => {
                Ok(Some(0))
            }
            Err(e) => Err(e),
        }
    }

    fn take_line(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let rest = self.buf.split_off(i.saturating_add(1));
            let line = std::mem::replace(&mut self.buf, rest);
            if line.len() > max {
                return Err(io::Error::new(ErrorKind::InvalidData, "line too long"));
            }
            return Ok(Some(line));
        }
        if self.buf.len() > max {
            return Err(io::Error::new(ErrorKind::InvalidData, "line too long"));
        }
        Ok(None)
    }

    /// Returns a complete line (with its line ending) if one is available after at most one read.
    pub fn poll_line(&mut self, max: usize) -> io::Result<Poll> {
        if let Some(l) = self.take_line(max)? {
            return Ok(Poll::Line(l));
        }
        if self.stopping() {
            return Ok(Poll::Eof);
        }
        match self.fill()? {
            Some(0) => Ok(Poll::Eof),
            Some(_) => Ok(match self.take_line(max)? {
                Some(l) => Poll::Line(l),
                None => Poll::Pending,
            }),
            None => {
                if self.last_activity.elapsed() > self.idle_timeout {
                    return Ok(Poll::Eof);
                }
                Ok(Poll::Pending)
            }
        }
    }

    /// Blocks until a full line arrives; `None` on EOF, shutdown or inactivity timeout.
    pub fn read_line(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        loop {
            match self.poll_line(max)? {
                Poll::Line(l) => return Ok(Some(l)),
                Poll::Eof => return Ok(None),
                Poll::Pending => {}
            }
        }
    }

    /// Reads exactly `n` bytes; `None` on EOF/shutdown.
    pub fn read_bytes(&mut self, n: usize) -> io::Result<Option<Vec<u8>>> {
        while self.buf.len() < n {
            if self.stopping() {
                return Ok(None);
            }
            match self.fill()? {
                Some(0) => return Ok(None),
                Some(_) => {}
                None => {
                    if self.last_activity.elapsed() > self.idle_timeout {
                        return Ok(None);
                    }
                }
            }
        }
        let rest = self.buf.split_off(n);
        Ok(Some(std::mem::replace(&mut self.buf, rest)))
    }

    /// Whether unread client bytes are buffered (used to refuse pipelined data before STARTTLS).
    pub fn has_buffered(&self) -> bool {
        !self.buf.is_empty()
    }

    pub fn close(&mut self) {
        match std::mem::replace(&mut self.t, Transport::Closed) {
            Transport::Plain(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            Transport::Tls(mut s) => {
                s.conn.send_close_notify();
                let _ = s.conn.complete_io(&mut s.sock);
                let _ = s.sock.shutdown(std::net::Shutdown::Both);
            }
            Transport::Closed => {}
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.close();
    }
}
