//! IMAP4rev1 (RFC 3501) / IMAP4rev2 (RFC 9051) server session, with LITERAL+, SASL-IR, ENABLE, IDLE,
//! NAMESPACE, ID, UIDPLUS, MOVE, UNSELECT, CHILDREN, SPECIAL-USE, LIST-EXTENDED, LIST-STATUS, CONDSTORE and
//! ESEARCH (the subset a desktop client uses).

mod fetch;
mod parse;
mod search;
mod utf7;

use std::io;
use std::sync::Arc;

use base64::Engine;

use crate::conn::{Conn, Poll};
use crate::date::DateTime;
use crate::mime::write_string;
use crate::server::Ctx;
use crate::store::{self, DELIM, SpecialUse, canonical_flag, canonical_name};
use crate::{Error, store::append_msg};
use parse::{MAX_LITERAL, Parser, SeqSet, format_set, trailing_literal};

const MAX_LINE: usize = 64 * 1024;

#[derive(Debug)]
pub enum Fail {
    No(String),
    Bad(String),
    /// Close the connection after writing the pending output.
    Bye,
    /// The tagged response was already written.
    Sent,
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Bad(s)
    }
}

impl From<io::Error> for Fail {
    fn from(_: io::Error) -> Fail {
        Fail::Bye
    }
}

fn no(s: impl Into<String>) -> Fail {
    Fail::No(s.into())
}

fn bad(s: impl Into<String>) -> Fail {
    Fail::Bad(s.into())
}

fn store_fail(e: Error) -> Fail {
    match e {
        Error::UnknownMailbox(m) => no(format!("[NONEXISTENT] No such mailbox: {m}")),
        Error::MailboxExists(m) => no(format!("[ALREADYEXISTS] Mailbox exists: {m}")),
        Error::UnknownUser(_) => no("[UNAVAILABLE] User is gone"),
        Error::Invalid(s) => no(format!("[CANNOT] {s}")),
        other => no(other.to_string()),
    }
}

type Res = Result<String, Fail>;

pub struct Selected {
    pub name: String,
    pub read_only: bool,
    pub uidvalidity: u32,
    /// (uid, modseq) as last reported to the client; index + 1 = sequence number.
    pub known: Vec<(u32, u64)>,
}

impl Selected {
    pub fn seq_of(&self, uid: u32) -> Option<usize> {
        self.known.binary_search_by_key(&uid, |k| k.0).ok().map(|i| i + 1)
    }

    pub fn max_uid(&self) -> u32 {
        self.known.last().map(|k| k.0).unwrap_or(0)
    }

    /// (seq, uid) pairs addressed by `set`.
    pub fn targets(&self, set: &SeqSet, uid_mode: bool) -> Vec<(usize, u32)> {
        let max_uid = self.max_uid();
        let max_seq = u32::try_from(self.known.len()).unwrap_or(u32::MAX);
        self.known
            .iter()
            .enumerate()
            .filter(|(i, (uid, _))| {
                if uid_mode {
                    set.contains(*uid, max_uid)
                } else {
                    u32::try_from(i + 1).is_ok_and(|s| set.contains(s, max_seq))
                }
            })
            .map(|(i, (uid, _))| (i + 1, *uid))
            .collect()
    }
}

enum Cmd {
    Line(Vec<u8>),
    Skip,
    Eof,
}

pub struct Session<'c> {
    conn: Conn,
    ctx: &'c Ctx,
    user: Option<String>,
    sel: Option<Selected>,
    condstore: bool,
    rev2: bool,
}

pub fn serve(conn: Conn, ctx: &Ctx) {
    let mut s = Session { conn, ctx, user: None, sel: None, condstore: false, rev2: false };
    let _ = s.run();
    s.conn.close();
}

pub fn flags_list(flags: &[String]) -> String {
    format!("({})", flags.join(" "))
}

impl Session<'_> {
    fn shared(&self) -> &Arc<store::Shared> {
        &self.ctx.shared
    }

    fn auth_allowed(&self) -> bool {
        !self.ctx.config.auth_requires_tls || self.conn.is_tls()
    }

    /// A mailbox name (or LIST pattern) from the client: modified UTF-7 unless IMAP4rev2 is enabled.
    fn mbox_in(&self, wire: String) -> String {
        if self.rev2 { wire } else { utf7::decode(&wire) }
    }

    /// The next argument as a mailbox name.
    fn mailbox_arg(&self, p: &mut Parser<'_>) -> Result<String, Fail> {
        Ok(self.mbox_in(p.astring_utf8()?))
    }

    /// Writes a mailbox name in the session's wire form.
    fn write_mbox(&self, out: &mut Vec<u8>, name: &str) {
        if self.rev2 {
            write_string(out, name.as_bytes());
        } else {
            write_string(out, utf7::encode(name).as_bytes());
        }
    }

    fn capabilities(&self) -> String {
        let mut c = vec![
            "IMAP4rev1",
            "IMAP4rev2",
            "LITERAL+",
            "SASL-IR",
            "ENABLE",
            "IDLE",
            "NAMESPACE",
            "ID",
            "UIDPLUS",
            "MOVE",
            "UNSELECT",
            "CHILDREN",
            "SPECIAL-USE",
            "LIST-EXTENDED",
            "LIST-STATUS",
            "CONDSTORE",
            "ESEARCH",
        ];
        if !self.ctx.config.imap_idle {
            // IMAP4rev2 includes IDLE: a server without push offers neither.
            c.retain(|x| *x != "IDLE" && *x != "IMAP4rev2");
        }
        if !self.conn.is_tls() {
            c.push("STARTTLS");
        }
        if self.user.is_none() {
            c.push(if self.auth_allowed() { "AUTH=PLAIN" } else { "LOGINDISABLED" });
        }
        let mut s = c.join(" ");
        s.push_str(&format!(" APPENDLIMIT={}", self.ctx.config.max_message_size));
        s
    }

    fn run(&mut self) -> io::Result<()> {
        let greet = format!("* OK [CAPABILITY {}] MailCraft test IMAP server ready\r\n", self.capabilities());
        self.conn.write_all(greet.as_bytes())?;
        loop {
            let cmd = match self.read_command()? {
                Cmd::Line(c) => c,
                Cmd::Skip => continue,
                Cmd::Eof => return Ok(()),
            };
            let mut p = Parser::new(&cmd);
            let tag = match p.word() {
                Ok(t) if !t.contains(&b'+') => String::from_utf8_lossy(t).into_owned(),
                _ => {
                    if p.at_end() && cmd.iter().all(|b| b.is_ascii_whitespace()) {
                        continue;
                    }
                    self.conn.write_all(b"* BAD Missing or invalid tag\r\n")?;
                    continue;
                }
            };
            let mut out = Vec::new();
            let res = match p.sp().and_then(|_| p.word().map(|w| w.to_ascii_uppercase())) {
                Ok(name) => self.dispatch(&tag, &name, &mut p, &mut out),
                Err(_) => Err(bad("Missing command")),
            };
            match res {
                Ok(text) => out.extend_from_slice(format!("{tag} OK {text}\r\n").as_bytes()),
                Err(Fail::No(text)) => out.extend_from_slice(format!("{tag} NO {text}\r\n").as_bytes()),
                Err(Fail::Bad(text)) => out.extend_from_slice(format!("{tag} BAD {text}\r\n").as_bytes()),
                Err(Fail::Bye) => {
                    let _ = self.conn.write_all(&out);
                    return Ok(());
                }
                Err(Fail::Sent) => {}
            }
            if !out.is_empty() {
                self.conn.write_all(&out)?;
            }
        }
    }

    fn read_command(&mut self) -> io::Result<Cmd> {
        let mut cmd = Vec::new();
        let limit = self.ctx.config.max_message_size.saturating_add(MAX_LINE).min(MAX_LITERAL);
        loop {
            let line = match self.conn.read_line(MAX_LINE) {
                Ok(Some(l)) => l,
                Ok(None) => return Ok(Cmd::Eof),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    self.conn.write_all(b"* BYE Command line too long\r\n")?;
                    return Ok(Cmd::Eof);
                }
                Err(e) => return Err(e),
            };
            cmd.extend_from_slice(&line);
            let Some((n, sync)) = trailing_literal(&line) else {
                return Ok(Cmd::Line(cmd));
            };
            if n > limit || cmd.len().saturating_add(n) > limit.saturating_add(MAX_LINE) {
                if sync {
                    let tag = cmd.split(|&b| b == b' ').next().unwrap_or(b"*");
                    let msg = format!("{} BAD [TOOBIG] Literal too large\r\n", String::from_utf8_lossy(tag));
                    self.conn.write_all(msg.as_bytes())?;
                    return Ok(Cmd::Skip);
                }
                self.conn.write_all(b"* BYE [TOOBIG] Literal too large\r\n")?;
                return Ok(Cmd::Eof);
            }
            if sync {
                self.conn.write_all(b"+ Ready for literal data\r\n")?;
            }
            let Some(data) = self.conn.read_bytes(n)? else {
                return Ok(Cmd::Eof);
            };
            cmd.extend_from_slice(&data);
        }
    }

    fn need_auth(&self) -> Result<&str, Fail> {
        self.user.as_deref().ok_or_else(|| bad("Not authenticated"))
    }

    fn need_sel(&self) -> Result<&Selected, Fail> {
        self.sel.as_ref().ok_or_else(|| bad("No mailbox selected"))
    }

    fn dispatch(&mut self, tag: &str, name: &[u8], p: &mut Parser<'_>, out: &mut Vec<u8>) -> Res {
        let (uid, name) = if name == b"UID" {
            p.sp()?;
            (true, p.word()?.to_ascii_uppercase())
        } else {
            (false, name.to_vec())
        };
        let name = String::from_utf8_lossy(&name).into_owned();
        let selected_cmd = matches!(
            name.as_str(),
            "FETCH" | "STORE" | "SEARCH" | "COPY" | "MOVE" | "EXPUNGE" | "CHECK" | "NOOP" | "IDLE"
        );
        // Untagged updates before and after; no EXPUNGE around sequence-number FETCH/STORE/SEARCH/COPY.
        let allow_expunge = uid || !matches!(name.as_str(), "FETCH" | "STORE" | "SEARCH" | "COPY");
        if selected_cmd {
            self.sync(out, allow_expunge);
        }
        let res = match (uid, name.as_str()) {
            (false, "CAPABILITY") => {
                p.end()?;
                out.extend_from_slice(format!("* CAPABILITY {}\r\n", self.capabilities()).as_bytes());
                Ok("CAPABILITY completed".to_owned())
            }
            (false, "NOOP") | (false, "CHECK") => {
                p.end()?;
                Ok(format!("{name} completed"))
            }
            (false, "LOGOUT") => {
                p.end()?;
                out.extend_from_slice(b"* BYE MailCraft test server logging out\r\n");
                out.extend_from_slice(format!("{tag} OK LOGOUT completed\r\n").as_bytes());
                return Err(Fail::Bye);
            }
            (false, "ID") => {
                out.extend_from_slice(
                    b"* ID (\"name\" \"MailCraft test server\" \"vendor\" \"MailCraft\")\r\n",
                );
                Ok("ID completed".to_owned())
            }
            (false, "STARTTLS") => self.starttls(tag, p),
            (false, "LOGIN") => self.login(p),
            (false, "AUTHENTICATE") => self.authenticate(p),
            (false, "ENABLE") => self.enable(p, out),
            (false, "NAMESPACE") => {
                self.need_auth()?;
                out.extend_from_slice(b"* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n");
                Ok("NAMESPACE completed".to_owned())
            }
            (false, "SELECT") => self.select(p, out, false),
            (false, "EXAMINE") => self.select(p, out, true),
            (false, "CREATE") => self.create(p),
            (false, "DELETE") => self.delete(p),
            (false, "RENAME") => self.rename(p),
            (false, "SUBSCRIBE") => self.subscribe(p, true),
            (false, "UNSUBSCRIBE") => self.subscribe(p, false),
            (false, "LIST") => self.list(p, out, false),
            (false, "LSUB") => self.list(p, out, true),
            (false, "STATUS") => self.status(p, out),
            (false, "APPEND") => self.append(p),
            (false, "IDLE") => self.idle(p, out),
            (false, "UNSELECT") | (false, "CLOSE") => {
                p.end()?;
                self.need_sel()?;
                if name == "CLOSE" && self.sel.as_ref().is_some_and(|s| !s.read_only) {
                    let mut ignored = Vec::new();
                    let _ = self.expunge(None, &mut ignored);
                }
                self.sel = None;
                Ok(format!("{name} completed"))
            }
            (_, "EXPUNGE") => {
                let set = if uid {
                    p.sp()?;
                    Some(p.seq_set()?)
                } else {
                    None
                };
                p.end()?;
                self.need_sel()?;
                self.expunge(set.as_ref(), out)
            }
            (_, "FETCH") => fetch::fetch(self, p, out, uid),
            (_, "STORE") => fetch::store(self, p, out, uid),
            (_, "SEARCH") => search::search(self, tag, p, out, uid),
            (_, "COPY") => self.copy(p, out, uid, false),
            (_, "MOVE") => self.copy(p, out, uid, true),
            _ => Err(bad(format!("Unknown command {}{name}", if uid { "UID " } else { "" }))),
        };
        if selected_cmd && !matches!(res, Err(Fail::Bye)) {
            self.sync(out, allow_expunge);
        }
        res
    }

    // ------------------------------------------------------------------ state sync

    /// Reports changes made by other sessions (or SMTP delivery) to the selected mailbox.
    pub fn sync(&mut self, out: &mut Vec<u8>, allow_expunge: bool) {
        let (Some(user), Some(sel)) = (self.user.as_deref(), self.sel.as_mut()) else {
            return;
        };
        let st = self.ctx.shared.lock();
        let mb =
            st.user(user).and_then(|u| u.mailbox(&sel.name)).filter(|m| m.uidvalidity == sel.uidvalidity);
        let Some(mb) = mb else {
            drop(st);
            self.sel = None;
            out.extend_from_slice(b"* OK [CLOSED] Selected mailbox no longer exists\r\n");
            return;
        };
        if allow_expunge {
            let mut i = sel.known.len();
            while i > 0 {
                i -= 1;
                let gone = sel.known.get(i).is_some_and(|(uid, _)| mb.find(*uid).is_none());
                if gone {
                    sel.known.remove(i);
                    out.extend_from_slice(format!("* {} EXPUNGE\r\n", i + 1).as_bytes());
                }
            }
        }
        for (i, (uid, ms)) in sel.known.iter_mut().enumerate() {
            if let Some(m) = mb.find(*uid)
                && m.modseq != *ms
            {
                *ms = m.modseq;
                let modseq = if self.condstore { format!(" MODSEQ ({})", m.modseq) } else { String::new() };
                out.extend_from_slice(
                    format!("* {} FETCH (UID {} FLAGS {}{modseq})\r\n", i + 1, m.uid, flags_list(&m.flags))
                        .as_bytes(),
                );
            }
        }
        let max = sel.max_uid();
        let before = sel.known.len();
        for m in mb.messages.iter().filter(|m| m.uid > max) {
            sel.known.push((m.uid, m.modseq));
        }
        if sel.known.len() != before {
            out.extend_from_slice(format!("* {} EXISTS\r\n", sel.known.len()).as_bytes());
        }
    }

    // ------------------------------------------------------------------ not authenticated

    fn starttls(&mut self, tag: &str, p: &mut Parser<'_>) -> Res {
        p.end()?;
        if self.conn.is_tls() {
            return Err(bad("TLS already active"));
        }
        if self.user.is_some() {
            return Err(bad("STARTTLS only before authentication"));
        }
        self.conn.write_all(format!("{tag} OK Begin TLS negotiation now\r\n").as_bytes())?;
        if self.conn.start_tls().is_err() {
            return Err(Fail::Bye);
        }
        // The tagged response was already sent.
        Err(Fail::Sent)
    }

    fn login(&mut self, p: &mut Parser<'_>) -> Res {
        if self.user.is_some() {
            return Err(bad("Already authenticated"));
        }
        p.sp()?;
        let user = p.astring_utf8()?;
        p.sp()?;
        let pass = p.astring_utf8()?;
        p.end()?;
        if !self.auth_allowed() {
            return Err(no("[PRIVACYREQUIRED] LOGIN only after STARTTLS"));
        }
        self.finish_login(&user, &pass)
    }

    fn finish_login(&mut self, user: &str, pass: &str) -> Res {
        let found = self.shared().lock().check_password(user, pass);
        match found {
            Some(addr) => {
                self.user = Some(addr);
                Ok(format!("[CAPABILITY {}] Logged in", self.capabilities()))
            }
            None => Err(no("[AUTHENTICATIONFAILED] Invalid credentials")),
        }
    }

    fn authenticate(&mut self, p: &mut Parser<'_>) -> Res {
        if self.user.is_some() {
            return Err(bad("Already authenticated"));
        }
        p.sp()?;
        let mech = p.atom()?.to_ascii_uppercase();
        let initial = if p.eat(b' ') { Some(p.astring()?) } else { None };
        p.end()?;
        if mech != b"PLAIN" {
            return Err(no("[CANNOT] Unsupported authentication mechanism"));
        }
        if !self.auth_allowed() {
            return Err(no("[PRIVACYREQUIRED] Authentication only after STARTTLS"));
        }
        let resp = match initial {
            Some(r) => r,
            None => {
                self.conn.write_all(b"+ \r\n")?;
                let line = self.conn.read_line(MAX_LINE)?.ok_or(Fail::Bye)?;
                crate::mime::trim(&line).to_vec()
            }
        };
        if resp == b"*" {
            return Err(bad("Authentication cancelled"));
        }
        let decoded = if resp == b"=" {
            Vec::new()
        } else {
            base64::engine::general_purpose::STANDARD.decode(&resp).map_err(|_| bad("Invalid base64"))?
        };
        let mut parts = decoded.split(|&b| b == 0);
        let (Some(authz), Some(authc), Some(pass), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(no("[AUTHENTICATIONFAILED] Malformed PLAIN response"));
        };
        let authc = String::from_utf8_lossy(authc).into_owned();
        if !authz.is_empty() && !authz.eq_ignore_ascii_case(authc.as_bytes()) {
            return Err(no("[AUTHORIZATIONFAILED] Cannot act as another user"));
        }
        self.finish_login(&authc, &String::from_utf8_lossy(pass))
    }

    fn enable(&mut self, p: &mut Parser<'_>, out: &mut Vec<u8>) -> Res {
        self.need_auth()?;
        let mut enabled = Vec::new();
        while p.eat(b' ') {
            let cap = p.atom()?.to_ascii_uppercase();
            match cap.as_slice() {
                b"CONDSTORE" => {
                    self.condstore = true;
                    enabled.push("CONDSTORE");
                }
                b"IMAP4REV2" => {
                    self.rev2 = true;
                    enabled.push("IMAP4rev2");
                }
                _ => {}
            }
        }
        p.end()?;
        out.extend_from_slice(format!("* ENABLED {}\r\n", enabled.join(" ")).trim_end().as_bytes());
        out.extend_from_slice(b"\r\n");
        Ok("ENABLE completed".to_owned())
    }

    // ------------------------------------------------------------------ mailboxes

    fn select(&mut self, p: &mut Parser<'_>, out: &mut Vec<u8>, read_only: bool) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = canonical_name(&self.mailbox_arg(p)?);
        if p.eat(b' ') {
            p.expect(b'(')?;
            while !p.eat(b')') {
                p.eat(b' ');
                let param = p.atom()?.to_ascii_uppercase();
                if param == b"CONDSTORE" {
                    self.condstore = true;
                } else {
                    return Err(bad("Unsupported SELECT parameter"));
                }
            }
        }
        p.end()?;
        if self.sel.take().is_some() {
            out.extend_from_slice(b"* OK [CLOSED] Previous mailbox closed\r\n");
        }
        let st = self.shared().lock();
        let mb = st
            .user(&user)
            .and_then(|u| u.mailbox(&name))
            .ok_or_else(|| no("[NONEXISTENT] No such mailbox"))?;
        let mut keywords: Vec<String> = Vec::new();
        for m in &mb.messages {
            for f in m.flags.iter().filter(|f| !f.starts_with('\\')) {
                if !keywords.contains(f) && keywords.len() < 64 {
                    keywords.push(f.clone());
                }
            }
        }
        let mut flags = vec!["\\Answered", "\\Flagged", "\\Deleted", "\\Seen", "\\Draft"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        flags.extend(keywords);
        out.extend_from_slice(format!("* FLAGS {}\r\n", flags_list(&flags)).as_bytes());
        let perm = if read_only { "()".to_owned() } else { format!("({} \\*)", flags.join(" ")) };
        out.extend_from_slice(format!("* OK [PERMANENTFLAGS {perm}] Flags permitted\r\n").as_bytes());
        out.extend_from_slice(format!("* {} EXISTS\r\n", mb.messages.len()).as_bytes());
        if !self.rev2 {
            out.extend_from_slice(b"* 0 RECENT\r\n");
            if let Some(i) = mb.messages.iter().position(|m| !m.has_flag("\\Seen")) {
                out.extend_from_slice(format!("* OK [UNSEEN {}] First unseen\r\n", i + 1).as_bytes());
            }
        }
        out.extend_from_slice(format!("* OK [UIDVALIDITY {}] UIDs valid\r\n", mb.uidvalidity).as_bytes());
        out.extend_from_slice(format!("* OK [UIDNEXT {}] Predicted next UID\r\n", mb.uidnext).as_bytes());
        out.extend_from_slice(format!("* OK [HIGHESTMODSEQ {}] Highest\r\n", mb.highest_modseq).as_bytes());
        if self.rev2 {
            out.extend_from_slice(b"* LIST () \"/\" ");
            write_string(out, mb.name.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        let sel = Selected {
            name: mb.name.clone(),
            read_only,
            uidvalidity: mb.uidvalidity,
            known: mb.messages.iter().map(|m| (m.uid, m.modseq)).collect(),
        };
        drop(st);
        self.sel = Some(sel);
        Ok(format!(
            "[{}] {} completed",
            if read_only { "READ-ONLY" } else { "READ-WRITE" },
            if read_only { "EXAMINE" } else { "SELECT" }
        ))
    }

    fn create(&mut self, p: &mut Parser<'_>) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = self.mailbox_arg(p)?;
        let mut special = None;
        if p.eat(b' ') {
            // RFC 6154 CREATE-SPECIAL-USE: (USE (\Drafts))
            p.expect(b'(')?;
            if !p.keyword("USE") {
                return Err(bad("Unsupported CREATE parameter"));
            }
            p.sp()?;
            for f in p.flag_list()? {
                special =
                    Some(SpecialUse::from_attribute(&f).ok_or_else(|| no("[USEATTR] Unsupported use"))?);
            }
            p.expect(b')')?;
        }
        p.end()?;
        let name = canonical_name(&name);
        if name == "INBOX" {
            return Err(no("[ALREADYEXISTS] INBOX always exists"));
        }
        self.shared().lock().create_mailbox(&user, &name, special).map_err(store_fail)?;
        self.shared().notify();
        Ok("CREATE completed".to_owned())
    }

    fn delete(&mut self, p: &mut Parser<'_>) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = self.mailbox_arg(p)?;
        p.end()?;
        self.shared().lock().delete_mailbox(&user, &name).map_err(store_fail)?;
        self.shared().notify();
        if self.sel.as_ref().is_some_and(|s| s.name == canonical_name(&name)) {
            self.sel = None;
        }
        Ok("DELETE completed".to_owned())
    }

    fn rename(&mut self, p: &mut Parser<'_>) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let from = self.mailbox_arg(p)?;
        p.sp()?;
        let to = self.mailbox_arg(p)?;
        p.end()?;
        self.shared().lock().rename_mailbox(&user, &from, &to).map_err(store_fail)?;
        self.shared().notify();
        Ok("RENAME completed".to_owned())
    }

    fn subscribe(&mut self, p: &mut Parser<'_>, on: bool) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = self.mailbox_arg(p)?;
        p.end()?;
        let mut st = self.shared().lock();
        let mb = st
            .user_mut(&user)
            .and_then(|u| u.mailbox_mut(&name))
            .ok_or_else(|| no("[NONEXISTENT] No such mailbox"))?;
        mb.subscribed = on;
        Ok(format!("{} completed", if on { "SUBSCRIBE" } else { "UNSUBSCRIBE" }))
    }

    fn list(&mut self, p: &mut Parser<'_>, out: &mut Vec<u8>, lsub: bool) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let (mut sel_subscribed, mut sel_special) = (lsub, false);
        if !lsub && p.eat(b'(') {
            while !p.eat(b')') {
                p.eat(b' ');
                match p.atom()?.to_ascii_uppercase().as_slice() {
                    b"SUBSCRIBED" => sel_subscribed = true,
                    b"SPECIAL-USE" => sel_special = true,
                    b"REMOTE" | b"RECURSIVEMATCH" => {}
                    _ => return Err(bad("Unsupported LIST selection option")),
                }
            }
            p.sp()?;
        }
        let reference = self.mbox_in(p.list_mailbox()?);
        p.sp()?;
        let mut patterns = Vec::new();
        if !lsub && p.eat(b'(') {
            while !p.eat(b')') {
                p.eat(b' ');
                patterns.push(self.mbox_in(p.list_mailbox()?));
            }
        } else {
            patterns.push(self.mbox_in(p.list_mailbox()?));
        }
        let (mut ret_subscribed, mut status_items) = (sel_subscribed && !lsub, None);
        if !lsub && p.eat(b' ') {
            if !p.keyword("RETURN") {
                return Err(bad("Expected RETURN"));
            }
            p.sp()?;
            p.expect(b'(')?;
            while !p.eat(b')') {
                p.eat(b' ');
                match p.atom()?.to_ascii_uppercase().as_slice() {
                    b"SUBSCRIBED" => ret_subscribed = true,
                    b"CHILDREN" | b"SPECIAL-USE" => {}
                    b"STATUS" => {
                        p.sp()?;
                        status_items = Some(status_item_list(p)?);
                    }
                    _ => return Err(bad("Unsupported LIST return option")),
                }
            }
        }
        p.end()?;
        let verb = if lsub { "LSUB" } else { "LIST" };
        if patterns.len() == 1 && patterns.first().is_some_and(|s| s.is_empty()) {
            out.extend_from_slice(format!("* {verb} (\\Noselect) \"/\" \"\"\r\n").as_bytes());
            return Ok(format!("{verb} completed"));
        }
        let st = self.shared().lock();
        let u = st.user(&user).ok_or_else(|| no("[UNAVAILABLE] User is gone"))?;
        for mb in &u.mailboxes {
            let hit = patterns.iter().any(|pat| {
                let full = canonical_pattern(&format!("{reference}{pat}"));
                wildcard_match(&full, &mb.name)
            });
            if !hit || (sel_subscribed && !mb.subscribed) || (sel_special && mb.special.is_none()) {
                continue;
            }
            let mut attrs =
                vec![if u.has_children(&mb.name) { "\\HasChildren" } else { "\\HasNoChildren" }.to_owned()];
            if let Some(s) = mb.special {
                attrs.push(s.attribute().to_owned());
            }
            if ret_subscribed && mb.subscribed {
                attrs.push("\\Subscribed".to_owned());
            }
            out.extend_from_slice(format!("* {verb} ({}) \"{DELIM}\" ", attrs.join(" ")).as_bytes());
            self.write_mbox(out, &mb.name);
            out.extend_from_slice(b"\r\n");
            if let Some(items) = &status_items {
                write_status(out, mb, items, self.rev2);
            }
        }
        Ok(format!("{verb} completed"))
    }

    fn status(&mut self, p: &mut Parser<'_>, out: &mut Vec<u8>) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = self.mailbox_arg(p)?;
        p.sp()?;
        let items = status_item_list(p)?;
        p.end()?;
        let st = self.shared().lock();
        let mb = st
            .user(&user)
            .and_then(|u| u.mailbox(&name))
            .ok_or_else(|| no("[NONEXISTENT] No such mailbox"))?;
        write_status(out, mb, &items, self.rev2);
        Ok("STATUS completed".to_owned())
    }

    fn append(&mut self, p: &mut Parser<'_>) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let name = self.mailbox_arg(p)?;
        p.sp()?;
        let mut flags = Vec::new();
        if p.peek() == Some(b'(') {
            for f in p.flag_list()? {
                if f.eq_ignore_ascii_case("\\Recent") {
                    continue;
                }
                flags.push(canonical_flag(&f).ok_or_else(|| bad(format!("Invalid flag {f}")))?);
            }
            p.sp()?;
        }
        let date = if p.peek() == Some(b'"') {
            let d = p.date_time()?;
            p.sp()?;
            d
        } else {
            DateTime::now()
        };
        let data = p.string()?;
        p.end()?;
        if data.len() > self.ctx.config.max_message_size {
            return Err(no("[TOOBIG] Message too large"));
        }
        let res = self.shared().lock().append(&user, &name, &data, &flags, date);
        let (v, uid) = res.map_err(|e| match e {
            Error::UnknownMailbox(_) => no("[TRYCREATE] No such mailbox"),
            other => store_fail(other),
        })?;
        self.shared().notify();
        Ok(format!("[APPENDUID {v} {uid}] APPEND completed"))
    }

    fn idle(&mut self, p: &mut Parser<'_>, pending: &mut Vec<u8>) -> Res {
        self.need_auth()?;
        p.end()?;
        // Changes that happened before IDLE are reported now, not after DONE (RFC 2177: the client is waiting
        // for exactly these).
        if !pending.is_empty() {
            self.conn.write_all(pending)?;
            pending.clear();
        }
        self.conn.write_all(b"+ idling\r\n")?;
        // Check once at the start: a change between the update above and now must not be missed.
        let mut seen_gen = self.shared().generation().wrapping_sub(1);
        loop {
            let generation = self.shared().generation();
            if generation != seen_gen {
                seen_gen = generation;
                let mut out = Vec::new();
                self.sync(&mut out, true);
                if !out.is_empty() {
                    self.conn.write_all(&out)?;
                }
            }
            match self.conn.poll_line(MAX_LINE)? {
                Poll::Line(l) => {
                    return if crate::mime::trim(&l).eq_ignore_ascii_case(b"DONE") {
                        Ok("IDLE terminated".to_owned())
                    } else {
                        Err(bad("Expected DONE"))
                    };
                }
                Poll::Eof => return Err(Fail::Bye),
                Poll::Pending => {}
            }
        }
    }

    // ------------------------------------------------------------------ selected state

    fn expunge(&mut self, only: Option<&SeqSet>, out: &mut Vec<u8>) -> Res {
        let user = self.need_auth()?.to_owned();
        let sel = self.need_sel()?;
        if sel.read_only {
            return Err(no("[READ-ONLY] Mailbox is read-only"));
        }
        let name = sel.name.clone();
        let removed: Vec<u32> = {
            let mut st = self.shared().lock();
            let mb =
                st.user_mut(&user).and_then(|u| u.mailbox_mut(&name)).ok_or_else(|| no("Mailbox is gone"))?;
            let max = mb.messages.last().map(|m| m.uid).unwrap_or(0);
            let removed: Vec<u32> = mb
                .messages
                .iter()
                .filter(|m| m.has_flag("\\Deleted") && only.is_none_or(|s| s.contains(m.uid, max)))
                .map(|m| m.uid)
                .collect();
            if !removed.is_empty() {
                mb.messages.retain(|m| removed.binary_search(&m.uid).is_err());
                mb.next_modseq();
            }
            removed
        };
        if !removed.is_empty() {
            self.shared().notify();
        }
        self.report_expunged(&removed, out);
        Ok("EXPUNGE completed".to_owned())
    }

    /// Sends EXPUNGE responses (highest sequence first) for UIDs this session removed.
    fn report_expunged(&mut self, uids: &[u32], out: &mut Vec<u8>) {
        let Some(sel) = self.sel.as_mut() else { return };
        let mut seqs: Vec<usize> = uids.iter().filter_map(|u| sel.seq_of(*u)).collect();
        seqs.sort_unstable_by(|a, b| b.cmp(a));
        for s in seqs {
            if s >= 1 && s <= sel.known.len() {
                sel.known.remove(s - 1);
                out.extend_from_slice(format!("* {s} EXPUNGE\r\n").as_bytes());
            }
        }
    }

    fn copy(&mut self, p: &mut Parser<'_>, out: &mut Vec<u8>, uid_mode: bool, mv: bool) -> Res {
        let user = self.need_auth()?.to_owned();
        p.sp()?;
        let set = p.seq_set()?;
        p.sp()?;
        let dest = self.mailbox_arg(p)?;
        p.end()?;
        let sel = self.need_sel()?;
        if mv && sel.read_only {
            return Err(no("[READ-ONLY] Mailbox is read-only"));
        }
        let src_name = sel.name.clone();
        let targets = sel.targets(&set, uid_mode);
        let (src_uids, dst_uids, v) = {
            let mut st = self.shared().lock();
            let u = st.user_mut(&user).ok_or_else(|| no("[UNAVAILABLE] User is gone"))?;
            let src = u.mailbox(&src_name).ok_or_else(|| no("Mailbox is gone"))?;
            let msgs: Vec<store::Msg> =
                targets.iter().filter_map(|(_, uid)| src.find(*uid).cloned()).collect();
            let dst = u.mailbox_mut(&dest).ok_or_else(|| no("[TRYCREATE] No such mailbox"))?;
            let mut src_uids = Vec::new();
            let mut dst_uids = Vec::new();
            for m in &msgs {
                let (_, nu) = append_msg(dst, Arc::clone(&m.raw), m.flags.clone(), m.internal_date);
                src_uids.push(m.uid);
                dst_uids.push(nu);
            }
            let v = dst.uidvalidity;
            if mv
                && !src_uids.is_empty()
                && let Some(src) = u.mailbox_mut(&src_name)
            {
                src.messages.retain(|m| src_uids.binary_search(&m.uid).is_err());
                src.next_modseq();
            }
            (src_uids, dst_uids, v)
        };
        self.shared().notify();
        let code = if src_uids.is_empty() {
            String::new()
        } else {
            // Both lists ascend, so the compressed sets still pair up element by element (RFC 4315).
            format!("[COPYUID {v} {} {}] ", format_set(&src_uids), format_set(&dst_uids))
        };
        if mv {
            if !code.is_empty() {
                out.extend_from_slice(format!("* OK {code}Moved\r\n").as_bytes());
            }
            self.report_expunged(&src_uids, out);
            return Ok("MOVE completed".to_owned());
        }
        Ok(format!("{code}COPY completed"))
    }
}

fn status_item_list(p: &mut Parser<'_>) -> Result<Vec<String>, Fail> {
    p.expect(b'(')?;
    let mut items = Vec::new();
    while !p.eat(b')') {
        p.eat(b' ');
        let it = String::from_utf8_lossy(p.atom()?).to_ascii_uppercase();
        match it.as_str() {
            "MESSAGES" | "RECENT" | "UIDNEXT" | "UIDVALIDITY" | "UNSEEN" | "DELETED" | "SIZE"
            | "HIGHESTMODSEQ" => items.push(it),
            _ => return Err(bad(format!("Unknown STATUS item {it}"))),
        }
    }
    Ok(items)
}

fn write_status(out: &mut Vec<u8>, mb: &store::Mailbox, items: &[String], utf8_names: bool) {
    let vals: Vec<String> = items
        .iter()
        .map(|it| {
            let v = match it.as_str() {
                "MESSAGES" => mb.messages.len() as u64,
                "RECENT" => 0,
                "UIDNEXT" => u64::from(mb.uidnext),
                "UIDVALIDITY" => u64::from(mb.uidvalidity),
                "UNSEEN" => mb.unseen() as u64,
                "DELETED" => mb.messages.iter().filter(|m| m.has_flag("\\Deleted")).count() as u64,
                "SIZE" => mb.size(),
                _ => mb.highest_modseq,
            };
            format!("{it} {v}")
        })
        .collect();
    out.extend_from_slice(b"* STATUS ");
    if utf8_names {
        write_string(out, mb.name.as_bytes());
    } else {
        write_string(out, utf7::encode(&mb.name).as_bytes());
    }
    out.extend_from_slice(format!(" ({})\r\n", vals.join(" ")).as_bytes());
}

/// Upper-cases a leading `INBOX` in a LIST pattern.
fn canonical_pattern(p: &str) -> String {
    match p.get(..5) {
        Some(h) if h.eq_ignore_ascii_case("INBOX") => {
            let rest = p.get(5..).unwrap_or("");
            if rest.is_empty() || rest.starts_with(DELIM) || rest.starts_with('*') || rest.starts_with('%') {
                return format!("INBOX{rest}");
            }
            p.to_owned()
        }
        _ => p.to_owned(),
    }
}

/// LIST wildcard match: `*` matches anything, `%` anything but the hierarchy delimiter.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // dp[j] = pattern prefix (current i) matches name prefix j.
    let mut dp = vec![false; n.len() + 1];
    if let Some(d) = dp.first_mut() {
        *d = true;
    }
    for pc in &p {
        let mut next = vec![false; n.len() + 1];
        match pc {
            '*' | '%' => {
                let mut run = false;
                for j in 0..=n.len() {
                    let prev = dp.get(j).copied().unwrap_or(false);
                    if *pc == '%' && j > 0 && n.get(j - 1) == Some(&DELIM) {
                        run = false;
                    }
                    run = run || prev;
                    if let Some(x) = next.get_mut(j) {
                        *x = run;
                    }
                }
            }
            c => {
                for j in 1..=n.len() {
                    let ok = dp.get(j - 1).copied().unwrap_or(false) && n.get(j - 1) == Some(c);
                    if let Some(x) = next.get_mut(j) {
                        *x = ok;
                    }
                }
            }
        }
        dp = next;
    }
    dp.last().copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", "INBOX"));
        assert!(wildcard_match("*", "a/b/c"));
        assert!(wildcard_match("%", "Sent"));
        assert!(!wildcard_match("%", "a/b"));
        assert!(wildcard_match("a/%", "a/b"));
        assert!(!wildcard_match("a/%", "a/b/c"));
        assert!(wildcard_match("a/*", "a/b/c"));
        assert!(wildcard_match("S%t", "Sent"));
        assert!(!wildcard_match("Sent", "Sent2"));
        assert!(wildcard_match("", ""));
        assert_eq!(canonical_pattern("inbox*"), "INBOX*");
        assert_eq!(canonical_pattern("inboxes"), "inboxes");
    }
}
