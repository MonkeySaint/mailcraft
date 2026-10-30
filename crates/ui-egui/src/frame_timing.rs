//! Timings for the troubleshooting log (File › Options › Advanced › "Enable troubleshooting logging"): a
//! frame that blocks the main window for long, broken down by part, and the first frame of each new OS
//! window, split into the window's content and creating and showing the window. Debug level, so they are
//! only written when troubleshooting logging is on. No timings on the web (no monotonic clock there).

/// A frame at least this long is reported.
const SLOW_MS: f64 = 100.0;
/// Parts shorter than this are left out of a slow frame's report.
const PART_MS: f64 = 2.0;

/// A point in time (nothing on the web).
#[derive(Clone, Copy, Debug)]
pub struct Stamp(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);

impl Stamp {
    pub fn now() -> Stamp {
        Stamp(
            #[cfg(not(target_arch = "wasm32"))]
            std::time::Instant::now(),
        )
    }

    /// Milliseconds since this stamp (0 on the web).
    pub fn ms(self) -> f64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.0.elapsed().as_secs_f64() * 1000.0
        }
        #[cfg(target_arch = "wasm32")]
        {
            0.0
        }
    }
}

/// Times the parts of one main-window frame ([`crate::App::ui_root`]).
pub struct FrameTimer {
    start: Stamp,
    last: Stamp,
    parts: Vec<(&'static str, f64)>,
}

impl FrameTimer {
    pub fn start() -> FrameTimer {
        let now = Stamp::now();
        FrameTimer { start: now, last: now, parts: Vec::new() }
    }

    /// Ends the part called `name` (it began where the previous one ended).
    pub fn lap(&mut self, name: &'static str) {
        let ms = self.last.ms();
        self.last = Stamp::now();
        self.parts.push((name, ms));
    }

    /// Reports the frame when it was slow.
    pub fn finish(self) {
        let total = self.start.ms();
        if total >= SLOW_MS {
            log::debug!("{}", slow_frame_line(total, &self.parts));
        }
    }
}

fn slow_frame_line(total: f64, parts: &[(&'static str, f64)]) -> String {
    let parts: Vec<String> = parts
        .iter()
        .filter(|(_, ms)| *ms >= PART_MS)
        .map(|(name, ms)| format!("{name} {ms:.0} ms"))
        .collect();
    format!("slow frame: {total:.0} ms ({})", parts.join(", "))
}

/// Reports a frame of an item window (read or compose) with its own OS window: always its first frame, later
/// ones when slow. `total` is the whole time the main window waited for it, `content` the part spent in
/// MailCraft's drawing of it; the rest is creating the window and handing the frame to the system.
pub fn item_window_frame(title: &str, first: bool, total: f64, content: f64) {
    if first || total >= SLOW_MS {
        log::debug!("{}", item_window_line(title, first, total, content));
    }
}

fn item_window_line(title: &str, first: bool, total: f64, content: f64) -> String {
    let what = if first { "first frame" } else { "slow frame" };
    let rest = (total - content).max(0.0);
    format!(
        "window \"{title}\": {what} {total:.0} ms ({content:.0} ms content, {rest:.0} ms creating and showing the window)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_frames_list_their_long_parts() {
        let line = slow_frame_line(312.4, &[("main window", 40.2), ("sync", 0.5), ("item windows", 270.0)]);
        assert_eq!(line, "slow frame: 312 ms (main window 40 ms, item windows 270 ms)");
    }

    #[test]
    fn item_window_lines_split_content_and_window() {
        assert_eq!(
            item_window_line("Hello - Message (HTML)", true, 180.0, 30.0),
            "window \"Hello - Message (HTML)\": first frame 180 ms (30 ms content, 150 ms creating and showing the window)"
        );
        assert!(item_window_line("x", false, 10.0, 20.0).contains("0 ms creating"));
    }

    #[test]
    fn a_timer_measures_its_parts() {
        let mut t = FrameTimer::start();
        t.lap("one");
        t.lap("two");
        assert_eq!(t.parts.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["one", "two"]);
        assert!(t.parts.iter().all(|(_, ms)| *ms >= 0.0));
        t.finish();
    }
}
