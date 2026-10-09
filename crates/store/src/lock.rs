//! Single-writer lock file.
//!
//! The lock is a small text file created with `create_new` (atomic on every supported platform):
//!
//! ```text
//! mailcraft-lock 1
//! pid 4242
//! time 1791433800
//! token 4242-1791433800123456789-7
//! boot ec57f87b-e28b-4ee7-a860-cdd8aeedbdec
//! ```
//!
//! A lock written during a different boot (Linux `boot_id`) is stale: its pid may since have been reused by an
//! unrelated process. Otherwise a lock is stale when its process is gone (checked through `/proc/<pid>` where
//! procfs exists) or, when
//! liveness cannot be checked, when its timestamp is older than [`STALE_AFTER_SECS`]. The holder refreshes the
//! timestamp every [`REFRESH_EVERY_SECS`] while it writes, so a live holder is never mistaken for a stale one.
//! A lock file that cannot be parsed (a crash between create and write) is stale once its mtime is older than
//! [`GARBLED_GRACE_SECS`].

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{Result, StoreError, time};

pub const LOCK_FILE: &str = "lock";
/// Age after which a lock whose owner cannot be checked is considered abandoned.
pub const STALE_AFTER_SECS: i64 = 3600;
/// How often a holder rewrites its timestamp.
pub const REFRESH_EVERY_SECS: i64 = 600;
/// Grace period for an unreadable lock file.
pub const GARBLED_GRACE_SECS: i64 = 10;
const MAX_LOCK_BYTES: u64 = 4096;

static NONCE: AtomicU64 = AtomicU64::new(0);

/// Tokens of the locks held (or being taken) by guards in this process.
static HELD: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn held() -> MutexGuard<'static, BTreeSet<String>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// True when a guard in this process holds the lock carrying `token`.
pub fn held_by_this_process(token: &str) -> bool {
    held().contains(token)
}

/// Parsed lock-file contents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: Option<u32>,
    pub time: Option<i64>,
    pub token: Option<String>,
    /// Kernel boot id at the time the lock was written (Linux), to detect pid reuse across reboots.
    pub boot: Option<String>,
}

impl LockInfo {
    /// Lenient parse of `key value` lines; unknown keys are ignored.
    pub fn parse(text: &str) -> LockInfo {
        let mut info = LockInfo::default();
        for line in text.lines().take(32) {
            let mut it = line.splitn(2, ' ');
            let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
            let v = v.trim();
            match k {
                "pid" => info.pid = v.parse().ok(),
                "time" => info.time = v.parse().ok(),
                "token" if !v.is_empty() && v.len() <= 128 => info.token = Some(v.to_owned()),
                "boot" if valid_boot_id(v) => info.boot = Some(v.to_owned()),
                _ => {}
            }
        }
        info
    }

    fn render(&self) -> String {
        let mut s = format!(
            "mailcraft-lock 1\npid {}\ntime {}\ntoken {}\n",
            self.pid.unwrap_or(0),
            self.time.unwrap_or(0),
            self.token.as_deref().unwrap_or("")
        );
        if let Some(boot) = self.boot.as_deref().filter(|b| valid_boot_id(b)) {
            s.push_str("boot ");
            s.push_str(boot);
            s.push('\n');
        }
        s
    }
}

fn valid_boot_id(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The running kernel's boot id (Linux), `None` where unavailable.
pub fn current_boot_id() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let mut buf = Vec::new();
    File::open("/proc/sys/kernel/random/boot_id").ok()?.take(128).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8(buf).ok()?;
    let id = text.trim();
    valid_boot_id(id).then(|| id.to_owned())
}

/// Whether process `pid` exists; `None` when this platform offers no cheap check.
pub fn pid_alive(pid: u32) -> Option<bool> {
    if pid == std::process::id() {
        return Some(true);
    }
    if pid == 0 {
        return Some(false);
    }
    let proc_root = Path::new("/proc");
    if cfg!(target_os = "linux") && proc_root.join("self").exists() {
        return Some(proc_root.join(pid.to_string()).exists());
    }
    None
}

/// Decides whether an existing lock may be taken over.
pub fn is_stale(info: &LockInfo, mtime_age_secs: Option<i64>, now: i64) -> bool {
    is_stale_with(info, mtime_age_secs, now, current_boot_id().as_deref())
}

/// [`is_stale`] with the current boot id supplied by the caller.
pub fn is_stale_with(
    info: &LockInfo,
    mtime_age_secs: Option<i64>,
    now: i64,
    current_boot: Option<&str>,
) -> bool {
    if info.pid.is_none() && info.time.is_none() {
        // Unreadable: give a writer that is mid-create a moment, then take over.
        return mtime_age_secs.is_none_or(|age| age > GARBLED_GRACE_SECS);
    }
    if let (Some(theirs), Some(ours)) = (info.boot.as_deref(), current_boot)
        && theirs != ours
    {
        // Written before the last reboot: whatever process now has that pid is not the writer.
        return true;
    }
    if info.pid == Some(std::process::id()) {
        // Our own pid but no guard here holds it: an earlier process had the same pid (pid namespaces such as
        // Flatpak sandboxes or containers hand out the same low pid on every run).
        return !info.token.as_deref().is_some_and(held_by_this_process);
    }
    if let Some(pid) = info.pid
        && let Some(alive) = pid_alive(pid)
    {
        return !alive;
    }
    match info.time {
        Some(t) => now.saturating_sub(t) > STALE_AFTER_SECS,
        None => mtime_age_secs.is_none_or(|age| age > STALE_AFTER_SECS),
    }
}

/// A held lock; released on drop.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    info: LockInfo,
    last_refresh: i64,
}

fn read_lock(path: &Path) -> std::io::Result<String> {
    let f = File::open(path)?;
    let mut s = String::new();
    let mut buf = Vec::new();
    f.take(MAX_LOCK_BYTES).read_to_end(&mut buf)?;
    s.push_str(&String::from_utf8_lossy(&buf));
    Ok(s)
}

fn mtime_age(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    i64::try_from(age.as_secs()).ok()
}

fn new_token() -> String {
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{nanos}-{n}", std::process::id())
}

fn try_create(path: &Path, info: &LockInfo) -> std::io::Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            f.write_all(info.render().as_bytes())?;
            f.sync_all()?;
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

impl LockGuard {
    /// Takes the lock in `dir`, taking over a stale one.
    pub fn acquire(dir: &Path) -> Result<LockGuard> {
        let path = dir.join(LOCK_FILE);
        let now = time::now_secs();
        let info = LockInfo {
            pid: Some(std::process::id()),
            time: Some(now),
            token: Some(new_token()),
            boot: current_boot_id(),
        };
        // Registered before the file exists so a concurrent open in this process never judges it stale.
        let token = info.token.clone().unwrap_or_default();
        held().insert(token.clone());
        let res = Self::acquire_with(dir, path, info, now);
        if res.is_err() {
            held().remove(&token);
        }
        res
    }

    fn acquire_with(dir: &Path, path: PathBuf, info: LockInfo, now: i64) -> Result<LockGuard> {
        for _attempt in 0..3 {
            if try_create(&path, &info)? {
                return Ok(LockGuard { path, info, last_refresh: now });
            }
            let existing = match read_lock(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let parsed = LockInfo::parse(&existing);
            if !is_stale(&parsed, mtime_age(&path), now) {
                return Err(StoreError::Locked);
            }
            // Take over: move the stale file aside (only one contender can win the rename), confirm we moved
            // the file we judged, then retry the atomic create.
            let aside = dir.join(format!("{LOCK_FILE}.stale-{}", info.token.as_deref().unwrap_or("x")));
            match fs::rename(&path, &aside) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }
            let moved = read_lock(&aside).unwrap_or_default();
            if moved != existing {
                // Someone else replaced it between our read and rename; put theirs back without clobbering a
                // lock created in the meantime (a hard link fails when the target exists).
                if fs::hard_link(&aside, &path).is_ok() {
                    let _ = fs::remove_file(&aside);
                } else if !path.exists() {
                    let _ = fs::rename(&aside, &path);
                } else {
                    let _ = fs::remove_file(&aside);
                }
                return Err(StoreError::Locked);
            }
            log::warn!("taking over stale store lock (pid {:?}, time {:?})", parsed.pid, parsed.time);
            let _ = fs::remove_file(&aside);
        }
        Err(StoreError::Locked)
    }

    /// Rewrites the timestamp when due (only while the file is still ours).
    pub fn refresh_if_due(&mut self) {
        let now = time::now_secs();
        if now.saturating_sub(self.last_refresh) < REFRESH_EVERY_SECS {
            return;
        }
        self.last_refresh = now;
        if !self.still_ours() {
            log::warn!("store lock file was replaced by another process");
            return;
        }
        self.info.time = Some(now);
        let tmp = self.path.with_extension("refresh");
        let res = fs::write(&tmp, self.info.render()).and_then(|()| fs::rename(&tmp, &self.path));
        if let Err(e) = res {
            log::warn!("could not refresh store lock: {e}");
        }
    }

    #[cfg(test)]
    pub(crate) fn backdate_refresh(&mut self) {
        self.last_refresh = self.last_refresh.saturating_sub(REFRESH_EVERY_SECS.saturating_mul(2));
    }

    fn still_ours(&self) -> bool {
        read_lock(&self.path).map(|s| LockInfo::parse(&s).token == self.info.token).unwrap_or(false)
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.still_ours() {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(token) = &self.info.token {
            held().remove(token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trip() {
        let info = LockInfo { pid: Some(12), time: Some(34), token: Some("t-1".into()), boot: None };
        assert_eq!(LockInfo::parse(&info.render()), info);
        assert_eq!(LockInfo::parse("garbage\n\0\0"), LockInfo::default());
        assert_eq!(
            LockInfo::parse("pid notanumber\ntime 5"),
            LockInfo { pid: None, time: Some(5), token: None, boot: None }
        );
    }

    #[test]
    fn staleness_rules() {
        let now = 1_000_000;
        let token = new_token();
        held().insert(token.clone());
        let me =
            LockInfo { pid: Some(std::process::id()), time: Some(0), token: Some(token.clone()), boot: None };
        assert!(!is_stale(&me, Some(99_999), now), "a lock held in this process is never stale");
        held().remove(&token);
        assert!(is_stale(&me, Some(99_999), now), "our pid without a live guard: left by an earlier process");
        let untokened = LockInfo { token: None, ..me.clone() };
        assert!(is_stale(&untokened, Some(0), now));
        let garbled = LockInfo::default();
        assert!(!is_stale(&garbled, Some(1), now));
        assert!(is_stale(&garbled, Some(60), now));
        assert!(is_stale(&garbled, None, now));
        let old = LockInfo { pid: None, time: Some(now - STALE_AFTER_SECS - 1), token: None, boot: None };
        assert!(is_stale(&old, None, now));
        let fresh = LockInfo { pid: None, time: Some(now - 5), token: None, boot: None };
        assert!(!is_stale(&fresh, None, now));
        if pid_alive(u32::MAX - 1).is_some() {
            let dead = LockInfo { pid: Some(u32::MAX - 1), time: Some(now), token: None, boot: None };
            assert!(is_stale(&dead, None, now));
        }
    }

    #[test]
    fn hostile_lock_text_never_panics() {
        let samples = ["", "pid", "pid -1", "time 99999999999999999999999", "token ", "\u{feff}pid 1\r\n"];
        for s in samples {
            let info = LockInfo::parse(s);
            let _ = is_stale(&info, Some(i64::MAX), i64::MIN);
            let _ = is_stale(&info, Some(i64::MIN), i64::MAX);
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn lock_from_another_boot_is_stale_even_if_pid_is_reused() {
        let now = 1_000_000;
        // The pid is alive (pid 1 always is), but the lock was written during a different boot: it was reused.
        let other_boot = LockInfo {
            pid: Some(1),
            time: Some(now),
            token: None,
            boot: Some("11111111-2222-3333-4444-555555555555".into()),
        };
        assert!(is_stale_with(&other_boot, None, now, Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")));
        let same_boot = LockInfo { boot: Some("b".into()), ..other_boot.clone() };
        assert!(!is_stale_with(&same_boot, None, now, Some("b")));
        // Unknown current boot id: fall back to the pid check.
        if pid_alive(1) == Some(true) {
            assert!(!is_stale_with(&other_boot, None, now, None));
        }
        let parsed = LockInfo::parse(&same_boot.render());
        assert_eq!(parsed, same_boot);
    }
}
