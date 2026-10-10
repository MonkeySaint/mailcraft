//! `mailcraft-testmail`: runs the local IMAP/SMTP test servers on 127.0.0.1 for manual end-to-end runs and
//! seeds a few synthetic messages (example.com/.org/.net addresses only). Never contacts a real server.
//!
//! ```text
//! mailcraft-testmail [--imap-port 1143] [--imaps-port 1993] [--smtp-port 1587] [--smtps-port 1465]
//!                    [--user alice@example.com:secret]... [--reject bounce@example.net]...
//!                    [--cert-out cert.pem] [--max-size BYTES] [--no-auth] [--tls-only-auth] [--no-seed]
//! ```
//! Runs until killed (Ctrl+C).

use std::process::ExitCode;
use std::time::Duration;

use mailcraft_testmail::{Config, DateTime, TestServers};

const USAGE: &str =
    "usage: mailcraft-testmail [--imap-port N] [--imaps-port N] [--smtp-port N] [--smtps-port N]
                          [--user ADDRESS:PASSWORD]... [--reject ADDRESS]... [--cert-out PATH]
                          [--max-size BYTES] [--no-auth] [--tls-only-auth] [--no-seed]

Runs local IMAP (plain+STARTTLS and implicit TLS) and SMTP submission servers on 127.0.0.1.
Defaults: IMAP 1143, IMAPS 1993, SMTP 1587, SMTPS 1465; users alice@example.com:alice and
bob@example.com:bob. Port 0 picks a free port. --no-auth lets SMTP accept mail without AUTH.";

struct Args {
    config: Config,
    cert_out: Option<String>,
    seed: bool,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut config =
        Config { imap_port: 1143, imaps_port: 1993, smtp_port: 1587, smtps_port: 1465, ..Config::default() };
    let mut users = Vec::new();
    let mut cert_out = None;
    let mut seed = true;
    let port = |v: Option<String>, flag: &str| -> Result<u16, String> {
        v.ok_or_else(|| format!("{flag} needs a value"))?.parse().map_err(|_| format!("{flag}: invalid port"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--imap-port" => config.imap_port = port(it.next(), &a)?,
            "--imaps-port" => config.imaps_port = port(it.next(), &a)?,
            "--smtp-port" => config.smtp_port = port(it.next(), &a)?,
            "--smtps-port" => config.smtps_port = port(it.next(), &a)?,
            "--user" => {
                let v = it.next().ok_or("--user needs ADDRESS:PASSWORD")?;
                let (u, p) = v.split_once(':').ok_or("--user needs ADDRESS:PASSWORD")?;
                users.push((u.to_owned(), p.to_owned()));
            }
            "--reject" => config.reject_recipients.push(it.next().ok_or("--reject needs an address")?),
            "--cert-out" => cert_out = Some(it.next().ok_or("--cert-out needs a path")?),
            "--max-size" => {
                config.max_message_size = it
                    .next()
                    .ok_or("--max-size needs a value")?
                    .parse()
                    .map_err(|_| "--max-size: invalid number".to_owned())?;
            }
            "--no-auth" => config.smtp_require_auth = false,
            "--tls-only-auth" => config.auth_requires_tls = true,
            "--no-seed" => seed = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if users.is_empty() {
        users = vec![
            ("alice@example.com".to_owned(), "alice".to_owned()),
            ("bob@example.com".to_owned(), "bob".to_owned()),
        ];
    }
    config.users = users;
    Ok(Args { config, cert_out, seed })
}

fn message(
    from: &str,
    to: &str,
    subject: &str,
    date: &DateTime,
    id: &str,
    extra: &str,
    body: &str,
) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {}\r\nMessage-ID: <{id}@example.com>\r\n\
         MIME-Version: 1.0\r\n{extra}\r\n{body}",
        date.to_rfc5322()
    )
    .into_bytes()
}

fn seed(servers: &TestServers, user: &str) -> Result<usize, mailcraft_testmail::Error> {
    let now = DateTime::now().unix;
    let at = |hours_ago: i64| DateTime::from_unix(now - hours_ago * 3600);
    let plain = "Content-Type: text/plain; charset=utf-8\r\n";
    let mut n = 0;
    let mut put = |mailbox: &str, raw: Vec<u8>, flags: &[&str], date: DateTime| {
        n += 1;
        servers.seed_message_at(user, mailbox, &raw, flags, date)
    };
    let d = at(50);
    put(
        "INBOX",
        message(
            "Dana Reyes <dana@example.org>",
            user,
            "Quarterly planning notes",
            &d,
            "seed-1",
            plain,
            "Hi,\r\n\r\nAttached are the planning notes from Tuesday. Let me know what you think.\r\n\r\nDana\r\n",
        ),
        &["\\Seen"],
        d,
    )?;
    let d = at(26);
    put(
        "INBOX",
        message(
            "Build Robot <ci@example.net>",
            user,
            "Nightly build passed",
            &d,
            "seed-2",
            "Content-Type: multipart/alternative; boundary=\"b1\"\r\n",
            "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nAll 1,024 tests passed.\r\n\
             --b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>All <b>1,024</b> tests passed.</p>\r\n--b1--\r\n",
        ),
        &[],
        d,
    )?;
    let d = at(5);
    put(
        "INBOX",
        message(
            "Sam Patel <sam@example.org>",
            &format!("{user}, Dana Reyes <dana@example.org>"),
            "Re: Quarterly planning notes",
            &d,
            "seed-3",
            &format!("{plain}In-Reply-To: <seed-1@example.com>\r\nReferences: <seed-1@example.com>\r\n"),
            "Looks good to me. One question about the timeline in section 2.\r\n\r\n> Attached are the planning notes.\r\n",
        ),
        &["\\Flagged"],
        d,
    )?;
    let d = at(1);
    put(
        "INBOX",
        message(
            "Office Manager <office@example.com>",
            user,
            "Kitchen schedule",
            &d,
            "seed-4",
            "Content-Type: multipart/mixed; boundary=\"m1\"\r\n",
            "--m1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSchedule attached.\r\n\
             --m1\r\nContent-Type: text/csv; name=\"schedule.csv\"\r\nContent-Disposition: attachment; filename=\"schedule.csv\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nV2VlayxQZXJzb24KMSxEYW5hCjIsU2FtCg==\r\n--m1--\r\n",
        ),
        &[],
        d,
    )?;
    let d = at(30);
    put(
        "Sent",
        message(
            user,
            "Dana Reyes <dana@example.org>",
            "Lunch on Friday?",
            &d,
            "seed-5",
            plain,
            "Are you free for lunch on Friday?\r\n",
        ),
        &["\\Seen"],
        d,
    )?;
    let d = at(2);
    put(
        "Drafts",
        message(
            user,
            "Sam Patel <sam@example.org>",
            "Timeline answer (draft)",
            &d,
            "seed-6",
            plain,
            "Section 2 assumes...\r\n",
        ),
        &["\\Seen", "\\Draft"],
        d,
    )?;
    Ok(n)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("mailcraft-testmail: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let users: Vec<(String, String)> = args.config.users.clone();
    let servers = match TestServers::start(args.config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mailcraft-testmail: cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(path) = &args.cert_out
        && let Err(e) = std::fs::write(path, &servers.cert_pem)
    {
        eprintln!("mailcraft-testmail: cannot write {path}: {e}");
        return ExitCode::FAILURE;
    }
    if args.seed
        && let Some((first, _)) = users.first()
    {
        match seed(&servers, first) {
            Ok(n) => println!("seeded {n} synthetic messages for {first}"),
            Err(e) => eprintln!("mailcraft-testmail: seeding failed: {e}"),
        }
    }
    println!("IMAP  127.0.0.1:{} (STARTTLS)", servers.imap_port);
    println!("IMAPS 127.0.0.1:{}", servers.imaps_port);
    println!("SMTP  127.0.0.1:{} (STARTTLS, submission)", servers.smtp_port);
    println!("SMTPS 127.0.0.1:{}", servers.smtps_port);
    for (u, p) in &users {
        println!("user  {u}  password {p}");
    }
    match &args.cert_out {
        Some(p) => println!("certificate (self-signed, localhost/127.0.0.1) written to {p}"),
        None => println!("certificate (self-signed): pass --cert-out PATH to save it"),
    }
    println!("running; press Ctrl+C to stop");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn parses_flags() {
        let a = args(&[
            "--imap-port",
            "0",
            "--user",
            "x@example.com:pw",
            "--no-auth",
            "--reject",
            "r@example.net",
        ])
        .unwrap();
        assert_eq!(a.config.imap_port, 0);
        assert_eq!(a.config.smtp_port, 1587);
        assert_eq!(a.config.users, vec![("x@example.com".to_owned(), "pw".to_owned())]);
        assert!(!a.config.smtp_require_auth);
        assert_eq!(a.config.reject_recipients, vec!["r@example.net".to_owned()]);
        assert!(args(&["--imap-port", "x"]).is_err());
        assert!(args(&["--user", "nocolon"]).is_err());
        assert!(args(&["--bogus"]).is_err());
        assert_eq!(args(&[]).unwrap().config.users.len(), 2);
    }

    #[test]
    fn seeds_on_ephemeral_ports() {
        let cfg = Config { users: vec![("alice@example.com".into(), "alice".into())], ..Config::default() };
        let s = TestServers::start(cfg).unwrap();
        assert_eq!(seed(&s, "alice@example.com").unwrap(), 6);
        assert_eq!(s.messages("alice@example.com", "INBOX").unwrap().len(), 4);
        assert_eq!(s.messages("alice@example.com", "Drafts").unwrap().len(), 1);
    }
}
