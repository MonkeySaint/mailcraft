//! Listener and connection threads.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rustls::ServerConfig;

use crate::conn::Conn;
use crate::store::Shared;
use crate::{Config, Result, imap, jmap, pop3, smtp};

/// Maximum simultaneous connections per listener.
const MAX_CONNECTIONS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Imap { tls: bool },
    Smtp { tls: bool },
    Pop3 { tls: bool },
    Jmap { tls: bool },
}

/// Everything a connection handler needs.
pub struct Ctx {
    pub config: Config,
    pub shared: Arc<Shared>,
    pub stop: Arc<AtomicBool>,
    pub tls: Arc<ServerConfig>,
    pub jmap: jmap::SharedJmap,
}

pub struct Running {
    pub imap: SocketAddr,
    pub imaps: SocketAddr,
    pub smtp: SocketAddr,
    pub smtps: SocketAddr,
    pub pop3: SocketAddr,
    pub pop3s: SocketAddr,
    pub jmap: SocketAddr,
    pub jmaps: SocketAddr,
    stop: Arc<AtomicBool>,
    accept_threads: Vec<JoinHandle<()>>,
    conn_threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

fn bind(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?)
}

impl Running {
    pub fn start(config: Config, shared: Arc<Shared>, tls: Arc<ServerConfig>) -> Result<Running> {
        let listeners = [
            (bind(config.imap_port)?, Kind::Imap { tls: false }),
            (bind(config.imaps_port)?, Kind::Imap { tls: true }),
            (bind(config.smtp_port)?, Kind::Smtp { tls: false }),
            (bind(config.smtps_port)?, Kind::Smtp { tls: true }),
            (bind(config.pop3_port)?, Kind::Pop3 { tls: false }),
            (bind(config.pop3s_port)?, Kind::Pop3 { tls: true }),
            (bind(config.jmap_port)?, Kind::Jmap { tls: false }),
            (bind(config.jmaps_port)?, Kind::Jmap { tls: true }),
        ];
        let mut addrs = Vec::new();
        for (l, _) in &listeners {
            addrs.push(l.local_addr()?);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx { config, shared, stop: Arc::clone(&stop), tls, jmap: Default::default() });
        let conn_threads: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let mut accept_threads = Vec::new();
        for (listener, kind) in listeners {
            let ctx = Arc::clone(&ctx);
            let threads = Arc::clone(&conn_threads);
            let h = std::thread::Builder::new()
                .name(format!("testmail-accept-{kind:?}"))
                .spawn(move || accept_loop(listener, kind, ctx, threads))?;
            accept_threads.push(h);
        }
        let addr = |i: usize| {
            addrs.get(i).copied().unwrap_or(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
        };
        Ok(Running {
            imap: addr(0),
            imaps: addr(1),
            smtp: addr(2),
            smtps: addr(3),
            pop3: addr(4),
            pop3s: addr(5),
            jmap: addr(6),
            jmaps: addr(7),
            stop,
            accept_threads,
            conn_threads,
        })
    }
}

fn accept_loop(listener: TcpListener, kind: Kind, ctx: Arc<Ctx>, threads: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for stream in listener.incoming() {
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let mut list = threads.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|h| !h.is_finished());
        if list.len() >= MAX_CONNECTIONS {
            drop(stream);
            continue;
        }
        let ctx2 = Arc::clone(&ctx);
        let spawned = std::thread::Builder::new()
            .name(format!("testmail-{kind:?}"))
            .spawn(move || handle(stream, kind, ctx2));
        if let Ok(h) = spawned {
            list.push(h);
        }
    }
}

fn handle(stream: TcpStream, kind: Kind, ctx: Arc<Ctx>) {
    let tls = match kind {
        Kind::Imap { tls } | Kind::Smtp { tls } | Kind::Pop3 { tls } | Kind::Jmap { tls } => tls,
    };
    let Ok(conn) =
        Conn::new(stream, Arc::clone(&ctx.stop), Arc::clone(&ctx.tls), tls, ctx.config.idle_timeout)
    else {
        return;
    };
    match kind {
        Kind::Imap { .. } => imap::serve(conn, &ctx),
        Kind::Smtp { .. } => smtp::serve(conn, &ctx),
        Kind::Pop3 { .. } => pop3::serve(conn, &ctx),
        Kind::Jmap { .. } => jmap::serve(conn, &ctx, &ctx.jmap),
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept() calls.
        for addr in
            [self.imap, self.imaps, self.smtp, self.smtps, self.pop3, self.pop3s, self.jmap, self.jmaps]
        {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(500));
        }
        for h in self.accept_threads.drain(..) {
            let _ = h.join();
        }
        let handles: Vec<JoinHandle<()>> =
            self.conn_threads.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for h in handles {
            let _ = h.join();
        }
    }
}
