//! The eframe adapter for the browser: a [`mailcraft_ui_egui::App`] plus the two things a browser supplies
//! differently from the desktop, the clock and storage, behind [`Host`].
//!
//! - **Clock.** `std::time::SystemTime::now()` is not available in the browser, so every frame sets the
//!   session's clock (`Session::fixed_now`) from [`Host::now_ms`] (JavaScript `Date.now()`) before the UI runs.
//! - **Storage.** Commands journal their changes into `Session::pending_ops`; with no store attached the shell
//!   takes them as "the mailbox changed", and writes the whole mailbox ([`crate::persist`]) to
//!   [`Host::save`] [`SAVE_DELAY_MS`] after the first unsaved change, or at once when the page loses focus.
//!
//! Target-independent so it is unit-tested natively with an in-memory host.

use mailcraft_engine::model::{DateTime, Mailbox};
use mailcraft_engine::{Notice, Session};
use mailcraft_ui_egui::App;

use crate::persist;

/// How long after the first unsaved change the mailbox is written (milliseconds).
pub const SAVE_DELAY_MS: f64 = 2_000.0;
/// Widest UTC offset accepted from the browser (minutes); matches the engine's time-zone command.
const MAX_TZ_MIN: i32 = 14 * 60;

/// What the page offers the shell.
pub trait Host {
    /// Milliseconds since the Unix epoch (JavaScript `Date.now()`).
    fn now_ms(&self) -> f64;
    /// Local offset from UTC in minutes, east positive (e.g. -240 for UTC-4).
    fn tz_offset_min(&self) -> i32;
    /// The stored mailbox document, if any.
    fn load(&self) -> Option<String>;
    /// Stores the mailbox document; the error text is shown to the user.
    fn save(&mut self, text: &str) -> Result<(), String>;
}

/// Converts a JavaScript timestamp to a [`DateTime`] (UTC). Non-finite or out-of-range values clamp.
pub fn datetime_from_ms(ms: f64) -> DateTime {
    let secs = if ms.is_finite() { (ms / 1000.0).floor() } else { 0.0 };
    // `as` saturates at the i64 range (and maps NaN to 0), which is what we want here.
    #[expect(clippy::cast_possible_truncation, reason = "saturating float-to-int conversion is intended")]
    let unix = secs as i64;
    DateTime { unix, offset_min: 0 }
}

/// Opens the stored mailbox, or the sample mailbox (with a notice saying why) when there is none or it cannot
/// be read.
pub fn open_session(host: &impl Host) -> Session {
    let now = datetime_from_ms(host.now_ms());
    let (mailbox, problem) = match host.load() {
        Some(text) => match persist::decode(&text) {
            Ok(mb) => (mb, None),
            Err(e) => (sample(now), Some(e)),
        },
        None => (sample(now), None),
    };
    let mut s = Session::new(mailbox);
    s.fixed_now = Some(now);
    s.tz_offset_min = host.tz_offset_min().clamp(-MAX_TZ_MIN, MAX_TZ_MIN);
    if let Some(e) = problem {
        s.notify(Notice::Error(format!("{e}. The sample mailbox was opened instead.")));
    }
    s
}

fn sample(now: DateTime) -> Mailbox {
    mailcraft_engine::sample::sample_mailbox(now)
}

/// The browser app: UI + host + autosave bookkeeping.
pub struct WebShell<H: Host> {
    pub app: App,
    pub host: H,
    /// `Host::now_ms` when the oldest unsaved change was seen; `None` = everything saved.
    dirty_since: Option<f64>,
    /// A save already failed and the user was told (repeat failures are only logged).
    save_failed: bool,
}

impl<H: Host> WebShell<H> {
    /// Opens the stored (or sample) mailbox.
    pub fn new(host: H) -> Self {
        let app = App::new(open_session(&host));
        WebShell { app, host, dirty_since: None, save_failed: false }
    }

    /// True while changes are waiting to be written.
    #[cfg(test)]
    pub fn dirty(&self) -> bool {
        self.dirty_since.is_some()
    }

    /// Before the UI runs: advance the session clock to the browser's.
    pub fn before_frame(&mut self) {
        self.app.s.fixed_now = Some(datetime_from_ms(self.host.now_ms()));
    }

    /// After the UI ran: note new changes and save when due. Returns how long until a save is due (for a
    /// repaint request), `None` when nothing is waiting.
    pub fn after_frame(&mut self, focused: bool) -> Option<f64> {
        let now = self.host.now_ms();
        if !self.app.s.pending_ops.is_empty() {
            // No store is attached: the queued ops only mean "something changed"; the whole mailbox is saved.
            self.app.s.pending_ops.clear();
            if self.dirty_since.is_none() {
                self.dirty_since = Some(now);
            }
        }
        let since = self.dirty_since?;
        let waited = now - since;
        if !focused || !waited.is_finite() || waited >= SAVE_DELAY_MS || waited < 0.0 {
            self.save_now();
            return None;
        }
        Some(SAVE_DELAY_MS - waited)
    }

    /// Writes the mailbox now (no-op when nothing changed).
    pub fn save_now(&mut self) {
        if self.dirty_since.is_none() {
            return;
        }
        // A failed save is not retried until the next change (a full storage quota will not empty itself).
        self.dirty_since = None;
        let result = persist::encode(&self.app.s.mailbox).and_then(|text| self.host.save(&text));
        match result {
            Ok(()) => self.save_failed = false,
            Err(e) => {
                log::error!("saving the mailbox failed: {e}");
                if !self.save_failed {
                    self.save_failed = true;
                    self.app
                        .s
                        .notify(Notice::Error(format!("Changes could not be saved in this browser: {e}")));
                }
            }
        }
    }
}

impl<H: Host + 'static> eframe::App for WebShell<H> {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        self.before_frame();
        self.app.ui_root(ui);
        let focused = ui.ctx().input(|i| i.focused);
        if let Some(ms) = self.after_frame(focused) {
            let secs = (ms / 1000.0).clamp(0.05, SAVE_DELAY_MS / 1000.0);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(secs));
        }
    }

    fn clear_color(&self, _visuals: &eframe::egui::Visuals) -> [f32; 4] {
        self.app.theme.backdrop.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct MemHost {
        now: Rc<Cell<f64>>,
        stored: Rc<std::cell::RefCell<Option<String>>>,
        fail: Rc<Cell<bool>>,
        saves: Rc<Cell<u32>>,
    }

    impl Host for MemHost {
        fn now_ms(&self) -> f64 {
            self.now.get()
        }
        fn tz_offset_min(&self) -> i32 {
            -240
        }
        fn load(&self) -> Option<String> {
            self.stored.borrow().clone()
        }
        fn save(&mut self, text: &str) -> Result<(), String> {
            self.saves.set(self.saves.get() + 1);
            if self.fail.get() {
                return Err("quota exceeded".into());
            }
            *self.stored.borrow_mut() = Some(text.to_owned());
            Ok(())
        }
    }

    fn replace(mb: &Mailbox) -> mailcraft_store::Op {
        mailcraft_store::Op::Replace(Box::new(mb.clone()))
    }

    const OCT_2026: f64 = 1_791_000_000_000.0;

    fn host() -> MemHost {
        let h = MemHost::default();
        h.now.set(OCT_2026);
        h
    }

    #[test]
    fn clock_conversion() {
        assert_eq!(datetime_from_ms(1_500.0).unix, 1);
        assert_eq!(datetime_from_ms(-1.0).unix, -1);
        assert_eq!(datetime_from_ms(f64::NAN).unix, 0);
        assert_eq!(datetime_from_ms(f64::INFINITY).unix, 0);
        assert_eq!(datetime_from_ms(1e300).unix, i64::MAX);
    }

    #[test]
    fn opens_sample_with_browser_clock_and_zone() {
        let h = host();
        let shell = WebShell::new(h.clone());
        assert!(!shell.app.s.mailbox.mail.is_empty());
        assert_eq!(shell.app.s.fixed_now.map(|d| d.unix), Some(1_791_000_000));
        assert_eq!(shell.app.s.tz_offset_min, -240);
        assert!(shell.app.s.notices.iter().all(|n| !matches!(n, Notice::Error(_))));
    }

    #[test]
    fn clock_follows_the_browser() {
        let h = host();
        let mut shell = WebShell::new(h.clone());
        h.now.set(OCT_2026 + 60_000.0);
        shell.before_frame();
        assert_eq!(shell.app.s.now().unix, 1_791_000_060);
    }

    #[test]
    fn changes_are_saved_after_a_delay_and_reload() {
        let h = host();
        let mut shell = WebShell::new(h.clone());
        assert_eq!(shell.after_frame(true), None, "nothing changed yet");
        let before = shell.app.s.mailbox.clone();
        shell.app.run("mail.markAllRead", serde_json::json!({}));
        assert_ne!(shell.app.s.mailbox, before, "the sample Inbox has unread mail");
        assert!(!shell.app.s.pending_ops.is_empty());
        let wait = shell.after_frame(true).unwrap();
        assert!(wait > 0.0 && wait <= SAVE_DELAY_MS);
        assert!(shell.dirty());
        assert_eq!(h.saves.get(), 0);
        h.now.set(OCT_2026 + SAVE_DELAY_MS);
        assert_eq!(shell.after_frame(true), None);
        assert_eq!(h.saves.get(), 1);
        assert!(!shell.dirty());

        // A new page load opens what was saved.
        let again = WebShell::new(h.clone());
        assert_eq!(again.app.s.mailbox, shell.app.s.mailbox);
    }

    #[test]
    fn losing_focus_saves_at_once() {
        let h = host();
        let mut shell = WebShell::new(h.clone());
        shell.app.s.journal(vec![replace(&shell.app.s.mailbox)]);
        assert_eq!(shell.after_frame(false), None);
        assert_eq!(h.saves.get(), 1);
        assert!(h.stored.borrow().is_some());
    }

    #[test]
    fn failed_save_tells_the_user_once() {
        let h = host();
        h.fail.set(true);
        let mut shell = WebShell::new(h.clone());
        for _ in 0..3 {
            shell.app.s.journal(vec![replace(&shell.app.s.mailbox)]);
            shell.after_frame(false);
        }
        assert_eq!(h.saves.get(), 3);
        let errors = shell.app.s.notices.iter().filter(|n| matches!(n, Notice::Error(_))).count();
        assert_eq!(errors, 1);
    }

    #[test]
    fn unreadable_storage_falls_back_to_the_sample() {
        let h = host();
        *h.stored.borrow_mut() = Some("{ broken".into());
        let shell = WebShell::new(h.clone());
        assert!(!shell.app.s.mailbox.mail.is_empty());
        assert!(shell.app.s.notices.iter().any(|n| matches!(n, Notice::Error(_))));
    }
}
