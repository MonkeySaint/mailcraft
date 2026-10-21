//! Options › Advanced › Other › "Enable troubleshooting logging (requires restarting MailCraft)": the
//! desktop app calls [`start`] once at start-up; with the option on it installs a [`log`] logger that writes
//! `logs/mailcraft.log` in the profile folder.
//!
//! - One line per record: UTC time, level, target, message. MailCraft's own crates log from Debug up, other
//!   crates (TLS, windowing) from Info up.
//! - Size cap: past [`MAX_BYTES`] the file rotates (`mailcraft.log.1` … `.3`, the oldest dropped), so the
//!   folder never holds more than about 4 × [`MAX_BYTES`].
//! - No secrets: [`redact`] blanks values after password, token, secret and authorization words and the
//!   arguments of IMAP `LOGIN` / SMTP `AUTH` lines before anything is written. Account code never logs
//!   secrets in the first place; this is the second line of defence.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::settings::Settings;

/// Folder below the profile that holds the log files.
pub const DIR: &str = "logs";
/// The current log file's name.
pub const FILE: &str = "mailcraft.log";
/// Size at which the log rotates.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// Rotated files kept (`mailcraft.log.1` is the newest).
pub const KEEP: usize = 3;
/// Longest line written (longer messages are cut).
const MAX_LINE: usize = 4096;

/// An append-only, size-capped log file.
pub struct LogFile {
    dir: PathBuf,
    max: u64,
    inner: Mutex<Option<(std::fs::File, u64)>>,
}

impl LogFile {
    /// Opens (or creates) `dir/mailcraft.log` for appending, rotating past `max` bytes.
    pub fn open(dir: &Path, max: u64) -> std::io::Result<LogFile> {
        std::fs::create_dir_all(dir)?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(FILE))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(LogFile { dir: dir.to_path_buf(), max: max.max(256), inner: Mutex::new(Some((f, len))) })
    }

    /// The current log file.
    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Appends one line (redacted, cut to a sane length), rotating first when it would pass the cap. I/O
    /// errors are swallowed: logging must never take the app down.
    pub fn write_line(&self, line: &str) {
        let mut text: String =
            redact(line).chars().filter(|c| *c == '\t' || !c.is_control()).take(MAX_LINE).collect();
        text.push('\n');
        let Ok(mut guard) = self.inner.lock() else { return };
        let len = guard.as_ref().map_or(0, |(_, n)| *n);
        if len > 0 && len.saturating_add(text.len() as u64) > self.max {
            *guard = None;
            self.rotate();
            *guard =
                std::fs::OpenOptions::new().create(true).append(true).open(self.path()).ok().map(|f| (f, 0));
        }
        if let Some((f, n)) = guard.as_mut()
            && f.write_all(text.as_bytes()).is_ok()
        {
            *n = n.saturating_add(text.len() as u64);
        }
    }

    /// `mailcraft.log.(k)` → `.(k+1)` from the oldest down, then the current file → `.1`.
    fn rotate(&self) {
        let name = |k: usize| self.dir.join(format!("{FILE}.{k}"));
        let _ = std::fs::remove_file(name(KEEP));
        for k in (1..KEEP).rev() {
            let _ = std::fs::rename(name(k), name(k.saturating_add(1)));
        }
        let _ = std::fs::rename(self.path(), name(1));
    }

    pub fn flush(&self) {
        if let Ok(mut g) = self.inner.lock()
            && let Some((f, _)) = g.as_mut()
        {
            let _ = f.flush();
        }
    }
}

/// Words whose value is a secret.
const SECRET_KEYS: [&str; 8] =
    ["password", "passwd", "secret", "token", "authorization", "bearer", "apikey", "api_key"];

/// `text` with secret values replaced by `[redacted]`: what follows a secret word (`password=…`,
/// `"refresh_token": "…"`, `Authorization: Bearer …`) and the arguments of IMAP `LOGIN` and SMTP `AUTH`.
pub fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    // IMAP `a1 LOGIN user pass`, SMTP `AUTH PLAIN <base64>`: everything after the verb goes.
    for verb in [" login ", "auth plain", "auth login", "auth xoauth2", "authenticate "] {
        if let Some(i) = lower.find(verb) {
            let cut = i.saturating_add(verb.len());
            return format!("{} [redacted]", text.get(..cut).unwrap_or("").trim_end());
        }
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        let hit = SECRET_KEYS.iter().find(|k| lower.get(i..).is_some_and(|r| r.starts_with(*k)));
        let Some(key) = hit else {
            let ch = text.get(i..).and_then(|r| r.chars().next()).unwrap_or(' ');
            out.push(ch);
            i = i.saturating_add(ch.len_utf8());
            continue;
        };
        let end = i.saturating_add(key.len());
        out.push_str(text.get(i..end).unwrap_or(""));
        // The rest of a longer word (`tokens`, `refresh_token_url`) stays with the key.
        let mut j = end;
        while bytes.get(j).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-') {
            j = j.saturating_add(1);
        }
        // Separators: quotes, `:`, `=`, blanks.
        let mut k = j;
        while bytes.get(k).is_some_and(|b| matches!(b, b'"' | b'\'' | b':' | b'=' | b' ' | b'\t')) {
            k = k.saturating_add(1);
        }
        out.push_str(text.get(end..k).unwrap_or(""));
        if !bytes.get(j..k).is_some_and(|sep| sep.iter().any(|b| matches!(b, b':' | b'='))) {
            // No `:` or `=`: just a word ("the token expired").
            i = k;
            continue;
        }
        // "Authorization: Bearer abc": the scheme word goes too.
        let mut v = k;
        while bytes
            .get(v)
            .is_some_and(|b| !matches!(b, b'"' | b'\'' | b',' | b';' | b'&' | b'}' | b'\n' | b'\r'))
            && !(bytes.get(v) == Some(&b' ')
                && !lower.get(k..v).is_some_and(|w| w == "bearer" || w == "basic"))
        {
            v = v.saturating_add(1);
        }
        if v > k {
            out.push_str("[redacted]");
        }
        i = v;
    }
    out
}

/// The installed logger: a [`LogFile`] plus the level filter.
struct FileLogger {
    file: LogFile,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        let ours = m.target().starts_with("mailcraft");
        m.level() <= if ours { log::Level::Debug } else { log::Level::Info }
    }

    fn log(&self, r: &log::Record<'_>) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let when = mailcraft_model::DateTime::now_utc().to_rfc3339();
        self.file.write_line(&format!("{when} {:<5} {}: {}", r.level(), r.target(), r.args()));
    }

    fn flush(&self) {
        self.file.flush();
    }
}

/// Starts troubleshooting logging when the option is on: installs the file logger for `profile` and returns
/// the log file's path. `Ok(None)` when the option is off; an error when the file cannot be opened or a
/// logger is already installed.
pub fn start(settings: &Settings, profile: &Path) -> Result<Option<PathBuf>, String> {
    if !settings.bool("advanced.logging") {
        return Ok(None);
    }
    let file =
        LogFile::open(&profile.join(DIR), MAX_BYTES).map_err(|e| format!("cannot open the log: {e}"))?;
    let path = file.path();
    // Installed once for the whole process (the logger lives as long as the app).
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger { file }));
    log::set_logger(logger).map_err(|e| format!("cannot install the log: {e}"))?;
    log::set_max_level(log::LevelFilter::Debug);
    log::info!("troubleshooting log started (MailCraft {})", env!("CARGO_PKG_VERSION"));
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    #[test]
    fn secrets_never_reach_the_log() {
        let cases = [
            ("password=hunter2&user=ann", "password=[redacted]&user=ann"),
            (
                r#"{"refresh_token": "abc.def", "expires": 5}"#,
                r#"{"refresh_token": "[redacted]", "expires": 5}"#,
            ),
            ("Authorization: Bearer eyJhbGci", "Authorization: [redacted]"),
            ("a1 LOGIN ann@example.org s3cret", "a1 LOGIN [redacted]"),
            ("C: AUTH PLAIN AGFubgBzM2NyZXQ=", "C: AUTH PLAIN [redacted]"),
            ("the token expired", "the token expired"),
            ("client_secret:xyz", "client_secret:[redacted]"),
            ("no secrets here", "no secrets here"),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
        // Hostile text never panics.
        let _ = redact("password=\u{1F600}\u{0}token:");
        let _ = redact(&"password ".repeat(1000));
    }

    #[test]
    fn the_log_rotates_at_its_cap() {
        let dir = scratch();
        let log = LogFile::open(dir.path(), 1000).unwrap();
        for n in 0..100 {
            log.write_line(&format!("line {n:03} password=topsecret {}", "x".repeat(40)));
        }
        log.flush();
        let cur = std::fs::read_to_string(log.path()).unwrap();
        assert!(cur.len() <= 1000, "{}", cur.len());
        assert!(cur.contains("line 099"));
        assert!(!cur.contains("topsecret"));
        for k in 1..=KEEP {
            assert!(dir.path().join(format!("{FILE}.{k}")).exists(), "rotated file {k}");
        }
        assert!(!dir.path().join(format!("{FILE}.{}", KEEP + 1)).exists());
        let total: u64 =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert!(total <= 1000 * (KEEP as u64 + 1));
    }

    #[test]
    fn logging_starts_only_when_the_option_is_on() {
        let dir = scratch();
        let mut st = Settings::default();
        assert_eq!(start(&st, dir.path()), Ok(None));
        assert!(!dir.path().join(DIR).exists(), "nothing written while off");
        st.set("advanced.logging", &serde_json::json!(true)).unwrap();
        // The test binary may already have a logger; either way the log file is created and named.
        match start(&st, dir.path()) {
            Ok(Some(p)) => assert_eq!(p, dir.path().join(DIR).join(FILE)),
            Ok(None) => panic!("option on"),
            Err(e) => assert!(e.contains("install"), "{e}"),
        }
        assert!(dir.path().join(DIR).join(FILE).exists());
    }
}
