//! Window-size rules from plan/outlook/responsive.md, computed once per frame.
//!
//! Each observed break point is the largest window width at which that step was seen, so a step applies at or
//! below its width (`≤`), the same reading as the folder-pane (≤ 800) and single-pane (≤ 700) rules.

use crate::theme::size;

/// Pane visibility and ribbon degradation for the main window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub width: f32,
    pub height: f32,
    /// Folder pane auto-minimised (≤ 800 wide) regardless of the user's Normal setting.
    pub folder_auto_minimized: bool,
    /// Single-pane mode (≤ 700 wide, reading pane Right).
    pub single_pane: bool,
    /// Simplified ribbon degradation step 1..=7 (1 = everything labelled).
    pub simplified_step: u8,
    /// Classic ribbon degradation step 1..=6.
    pub classic_step: u8,
    /// Compose / read window this small: no ribbon, tab row or Quick Access Toolbar ([`inspector_tiny`]).
    pub inspector_tiny: bool,
}

/// Compose and read windows (responsive.md, both observed down to 300 × 200, which they accept): at 300 × 200
/// the ribbon, the tab row and the Quick Access Toolbar are gone and only the header fields and body remain;
/// at 500 × 400 everything was still there. The cut-off sits between the two: narrower than 360 or lower than
/// 260 counts as tiny.
pub const INSPECTOR_TINY: [f32; 2] = [360.0, 260.0];

/// Whether a compose / read window of this size hides its ribbon, tab row and Quick Access Toolbar. Junk
/// sizes (NaN, negative, a minimised 0 × 0) count as tiny: there is no room for chrome.
pub fn inspector_tiny(width: f32, height: f32) -> bool {
    let tiny = |v: f32, cut: f32| !v.is_finite() || v < cut;
    tiny(width, INSPECTOR_TINY[0]) || tiny(height, INSPECTOR_TINY[1])
}

/// Simplified ribbon (Home tab): widths at which steps 2..=7 were observed.
///
/// 2: Quick Steps narrows, Apps loses its label (1600) · 3: Move & Delete keeps only the first label, Speech to
/// overflow (1366) · 4: Respond icon-only (1280) · 5: Quick Steps and Apps to overflow (1024) · 6: Find to
/// overflow (900) · 7: Tags shrinks, then goes; Move & Delete down to Delete/Archive (700).
pub const SIMPLIFIED_STEPS: [f32; 6] = [1600.0, 1366.0, 1280.0, 1024.0, 900.0, 700.0];

/// Classic ribbon: widths at which steps 2..=6 were observed.
///
/// 2: small buttons lose labels (1366) · 3: Respond icon-only, Move and Tags collapse to dropdowns (1024) ·
/// 4: Quick Steps collapses (900) · 5: Find collapses (800) · 6: ribbon scrolls sideways (700).
pub const CLASSIC_STEPS: [f32; 5] = [1366.0, 1024.0, 900.0, 800.0, 700.0];

/// Step number (1-based) for `width` against descending break points.
fn step(width: f32, breaks: &[f32]) -> u8 {
    let passed = breaks.iter().filter(|&&b| width <= b).count();
    u8::try_from(passed).unwrap_or(u8::MAX).saturating_add(1)
}

impl Frame {
    pub fn compute(width: f32, height: f32) -> Frame {
        let inspector_tiny = inspector_tiny(width, height);
        // Non-finite or negative sizes (minimised windows report 0×0) count as the minimum window.
        let clean = |v: f32, min: f32| if v.is_finite() && v > 0.0 { v } else { min };
        let width = clean(width, size::MIN_WINDOW[0]);
        let height = clean(height, size::MIN_WINDOW[1]);
        Frame {
            width,
            height,
            folder_auto_minimized: width <= size::AUTO_MINIMIZE_W,
            single_pane: width <= size::SINGLE_PANE_W,
            simplified_step: step(width, &SIMPLIFIED_STEPS),
            classic_step: step(width, &CLASSIC_STEPS),
            inspector_tiny,
        }
    }

    /// Like [`Frame::compute`], with single-pane mode applied only when the reading pane is on the right
    /// (Bottom keeps both panes at every width; Off has no reading pane to switch to).
    pub fn compute_for(width: f32, height: f32, reading_right: bool) -> Frame {
        let mut f = Frame::compute(width, height);
        f.single_pane &= reading_right;
        f
    }

    /// Ribbon degradation step for the active ribbon style.
    pub fn ribbon_step(&self, simplified: bool) -> u8 {
        if simplified { self.simplified_step } else { self.classic_step }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_window_has_everything() {
        let f = Frame::compute(1920.0, 1040.0);
        assert!(!f.folder_auto_minimized && !f.single_pane);
        assert_eq!((f.simplified_step, f.classic_step), (1, 1));
    }

    #[test]
    fn documented_break_points() {
        let s = |w| Frame::compute(w, 900.0).simplified_step;
        let c = |w| Frame::compute(w, 900.0).classic_step;
        assert_eq!(
            [s(1700.0), s(1600.0), s(1366.0), s(1280.0), s(1024.0), s(900.0), s(700.0)],
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(s(400.0), 7);
        assert_eq!(
            [c(1440.0), c(1366.0), c(1024.0), c(900.0), c(800.0), c(700.0), c(400.0)],
            [1, 2, 3, 4, 5, 6, 6]
        );
        assert!(Frame::compute(800.0, 700.0).folder_auto_minimized);
        assert!(!Frame::compute(801.0, 700.0).folder_auto_minimized);
        assert!(Frame::compute(700.0, 700.0).single_pane);
        assert!(!Frame::compute(701.0, 700.0).single_pane);
    }

    #[test]
    fn single_pane_needs_right_reading_pane() {
        assert!(Frame::compute_for(600.0, 600.0, true).single_pane);
        assert!(!Frame::compute_for(600.0, 600.0, false).single_pane);
    }

    #[test]
    fn inspector_windows_lose_their_chrome_at_300_by_200() {
        // Observed: everything at 500 × 400, no ribbon / tab row / QAT at 300 × 200 (compose and read).
        assert!(!inspector_tiny(500.0, 400.0));
        assert!(!Frame::compute(500.0, 400.0).inspector_tiny);
        assert!(inspector_tiny(300.0, 200.0));
        assert!(Frame::compute(300.0, 200.0).inspector_tiny);
        // Either side alone is enough.
        assert!(inspector_tiny(300.0, 900.0) && inspector_tiny(1200.0, 200.0));
        assert!(!inspector_tiny(INSPECTOR_TINY[0], INSPECTOR_TINY[1]));
        // The main window never gets this small (400 × 400 minimum).
        let [w, h] = size::MIN_WINDOW;
        assert!(!Frame::compute(w, h).inspector_tiny);
        for junk in [f32::NAN, f32::NEG_INFINITY, -1.0, 0.0] {
            assert!(inspector_tiny(junk, 800.0) && inspector_tiny(800.0, junk));
        }
    }

    #[test]
    fn junk_sizes_do_not_panic() {
        for w in [f32::NAN, f32::INFINITY, -5.0, 0.0] {
            let f = Frame::compute(w, w);
            assert_eq!(f.width, size::MIN_WINDOW[0]);
            assert!(f.single_pane);
        }
    }
}
