//! Colour and size tokens. Our own palette (light default, dark variant); geometry from plan/reference/layout.md.

use egui::Color32;

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Title bar, tab row and module rail.
    pub chrome: Color32,
    pub chrome_text: Color32,
    /// Background behind the rounded panes (the gaps).
    pub backdrop: Color32,
    pub ribbon: Color32,
    pub pane: Color32,
    pub reading: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    /// Text and marks drawn on an `accent` fill (primary buttons, today in the date navigators): at least
    /// 4.5:1 against it.
    pub on_accent: Color32,
    /// Fill of text boxes outside dialogs (the search boxes in the People and Tasks bars): set apart from the
    /// pane they sit on.
    pub field: Color32,
    /// Unread bar, unread subject/time.
    pub unread: Color32,
    pub selected: Color32,
    pub selected_folder: Color32,
    pub hover: Color32,
    pub group_header: Color32,
    pub separator: Color32,
    pub flagged_row: Color32,
    pub flag: Color32,
    pub complete: Color32,
    pub importance_high: Color32,
    pub importance_low: Color32,
    pub draft: Color32,
    pub link: Color32,
    pub danger: Color32,
    pub info_bar: Color32,
    /// Background behind search words in the message list and reading pane.
    pub search_hit: Color32,
}

/// Fixed geometry (logical px) from the observations.
pub mod size {
    pub const TITLE_BAR: f32 = 48.0;
    pub const TAB_ROW: f32 = 30.0;
    pub const RIBBON_SIMPLIFIED: f32 = 49.0;
    pub const RIBBON_CLASSIC: f32 = 100.0;
    pub const STATUS_BAR: f32 = 22.0;
    pub const RAIL: f32 = 48.0;
    pub const PANE_GAP: f32 = 14.0;
    pub const FOLDER_PANE: f32 = 230.0;
    pub const FOLDER_PANE_MIN: f32 = 26.0;
    pub const FOLDER_ROW: f32 = 24.0;
    pub const ACCOUNT_ROW: f32 = 34.0;
    pub const LIST: f32 = 300.0;
    /// Narrowest mail area kept beside the To-Do Bar (below it the bar hides).
    pub const TODO_BAR_MIN_MAIL: f32 = 560.0;
    pub const LIST_HEADER: f32 = 97.0;
    pub const ROW_COMPACT: f32 = 72.0;
    pub const GROUP_HEADER: f32 = 24.0;
    pub const UNREAD_BAR: f32 = 3.0;
    pub const SEARCH_W: f32 = 305.0;
    pub const SEARCH_H: f32 = 34.0;
    pub const MIN_WINDOW: [f32; 2] = [400.0, 400.0];
    /// Folder pane auto-minimises at or below this window width.
    pub const AUTO_MINIMIZE_W: f32 = 800.0;
    /// Single-pane mode at or below this width (reading pane Right).
    pub const SINGLE_PANE_W: f32 = 700.0;
    /// Gap between neighbouring panes (holds the splitter).
    pub const PANE_SPLIT: f32 = 8.0;
    /// Corner radius of the content panes.
    pub const PANE_RADIUS: u8 = 8;
    /// Tab row: File button width.
    pub const FILE_BUTTON_W: f32 = 54.0;
    /// Module rail item (square) and its icon.
    pub const RAIL_ITEM: f32 = 48.0;
    pub const RAIL_ICON: f32 = 20.0;
    /// Title bar account button.
    pub const ACCOUNT_BUTTON_W: f32 = 50.0;
    /// Smallest widths the splitters allow.
    pub const FOLDER_PANE_DRAG_MIN: f32 = 140.0;
    pub const LIST_MIN: f32 = 220.0;
    pub const READING_MIN: f32 = 260.0;
    /// Default list share of the content height when the reading pane is at the bottom (≈395 of 891 observed).
    pub const BOTTOM_LIST_FRACTION: f32 = 0.44;
    /// Toast lifetime, seconds.
    pub const TOAST_SECS: f64 = 5.0;
}

impl Theme {
    /// The light or dark palette.
    pub fn for_dark(dark: bool) -> Theme {
        if dark { Theme::dark() } else { Theme::light() }
    }

    /// egui visuals derived from the palette (widgets, selection, windows, menus).
    pub fn visuals(&self) -> egui::Visuals {
        use egui::{CornerRadius, Stroke};
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.link;
        v.panel_fill = self.pane;
        v.window_fill = self.pane;
        v.extreme_bg_color = if self.dark { self.backdrop } else { Color32::WHITE };
        // Text boxes on a pane (the People and Tasks bars) need a fill of their own: idle widgets are frameless.
        v.text_edit_bg_color = Some(self.field);
        v.faint_bg_color = self.group_header;
        v.code_bg_color = self.info_bar;
        v.window_stroke = Stroke::new(1.0, self.separator);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.selection.bg_fill = self.selected;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.error_fg_color = self.danger;
        v.warn_fg_color = self.flag;
        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.pane;
        w.noninteractive.weak_bg_fill = self.pane;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.separator);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.noninteractive.corner_radius = radius;
        w.inactive.bg_fill = self.ribbon;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = self.hover;
        w.hovered.weak_bg_fill = self.hover;
        w.hovered.bg_stroke = Stroke::new(1.0, self.separator);
        w.hovered.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = self.selected;
        w.active.weak_bg_fill = self.selected;
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(1.0, self.text);
        w.active.corner_radius = radius;
        w.open.bg_fill = self.selected;
        w.open.weak_bg_fill = self.selected;
        w.open.bg_stroke = Stroke::new(1.0, self.separator);
        w.open.fg_stroke = Stroke::new(1.0, self.text);
        w.open.corner_radius = radius;
        v
    }

    /// Installs the visuals plus our text sizes and spacing into `ctx`.
    pub fn apply(&self, ctx: &egui::Context) {
        use egui::{FontFamily, FontId, TextStyle};
        let visuals = self.visuals();
        ctx.all_styles_mut(|style| {
            style.visuals = visuals.clone();
            style.text_styles = [
                (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
            ]
            .into();
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(6.0, 3.0);
            style.spacing.interact_size.y = 22.0;
            style.spacing.menu_margin = egui::Margin::same(6);
            style.interaction.selectable_labels = false;
        });
    }

    pub fn light() -> Theme {
        Theme {
            dark: false,
            chrome: Color32::from_rgb(0x1f, 0x4e, 0x5f),
            chrome_text: Color32::WHITE,
            backdrop: Color32::from_rgb(0xec, 0xee, 0xf1),
            ribbon: Color32::from_rgb(0xf7, 0xf8, 0xfa),
            pane: Color32::WHITE,
            reading: Color32::WHITE,
            text: Color32::from_rgb(0x1d, 0x1f, 0x23),
            text_muted: Color32::from_rgb(0x5f, 0x66, 0x70),
            accent: Color32::from_rgb(0x0e, 0x7c, 0x86),
            on_accent: Color32::WHITE,
            field: Color32::from_rgb(0xee, 0xf0, 0xf3),
            unread: Color32::from_rgb(0x0a, 0x64, 0xae),
            selected: Color32::from_rgb(0xd5, 0xe6, 0xf2),
            selected_folder: Color32::from_rgb(0xdf, 0xe6, 0xec),
            hover: Color32::from_rgb(0xee, 0xf2, 0xf5),
            group_header: Color32::from_rgb(0xf3, 0xf4, 0xf6),
            separator: Color32::from_rgb(0xdc, 0xdf, 0xe4),
            flagged_row: Color32::from_rgb(0xfb, 0xec, 0xec),
            flag: Color32::from_rgb(0xc4, 0x31, 0x2f),
            complete: Color32::from_rgb(0x2e, 0x8b, 0x46),
            importance_high: Color32::from_rgb(0xc4, 0x31, 0x2f),
            importance_low: Color32::from_rgb(0x2f, 0x6f, 0xc4),
            draft: Color32::from_rgb(0xc4, 0x31, 0x2f),
            link: Color32::from_rgb(0x0b, 0x6a, 0xb8),
            danger: Color32::from_rgb(0xb0, 0x22, 0x22),
            info_bar: Color32::from_rgb(0xf1, 0xf3, 0xf6),
            search_hit: Color32::from_rgb(0xff, 0xf1, 0x9e),
        }
    }
    pub fn dark() -> Theme {
        Theme {
            dark: true,
            chrome: Color32::from_rgb(0x0c, 0x0e, 0x10),
            chrome_text: Color32::from_rgb(0xe8, 0xea, 0xed),
            backdrop: Color32::from_rgb(0x15, 0x17, 0x1a),
            ribbon: Color32::from_rgb(0x26, 0x29, 0x2d),
            pane: Color32::from_rgb(0x1d, 0x20, 0x23),
            reading: Color32::from_rgb(0x22, 0x25, 0x29),
            text: Color32::from_rgb(0xe8, 0xea, 0xed),
            text_muted: Color32::from_rgb(0x9a, 0xa1, 0xab),
            accent: Color32::from_rgb(0x3f, 0xb8, 0xc2),
            on_accent: Color32::from_rgb(0x0c, 0x0e, 0x10),
            field: Color32::from_rgb(0x15, 0x17, 0x1a),
            unread: Color32::from_rgb(0x6c, 0xb8, 0xf6),
            selected: Color32::from_rgb(0x34, 0x45, 0x52),
            selected_folder: Color32::from_rgb(0x33, 0x38, 0x3e),
            hover: Color32::from_rgb(0x2a, 0x2e, 0x33),
            group_header: Color32::from_rgb(0x26, 0x29, 0x2d),
            separator: Color32::from_rgb(0x33, 0x37, 0x3c),
            flagged_row: Color32::from_rgb(0x33, 0x24, 0x26),
            flag: Color32::from_rgb(0xe5, 0x5b, 0x58),
            complete: Color32::from_rgb(0x5c, 0xc0, 0x76),
            importance_high: Color32::from_rgb(0xe5, 0x5b, 0x58),
            importance_low: Color32::from_rgb(0x6c, 0xa0, 0xf0),
            draft: Color32::from_rgb(0xe5, 0x5b, 0x58),
            link: Color32::from_rgb(0x6c, 0xb8, 0xf6),
            danger: Color32::from_rgb(0xe5, 0x5b, 0x58),
            info_bar: Color32::from_rgb(0x2a, 0x2e, 0x33),
            search_hit: Color32::from_rgb(0x6b, 0x5a, 0x12),
        }
    }
}

/// Relative luminance of an opaque colour (WCAG 2.x).
pub fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio of two opaque colours (1 to 21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg`, moved toward white on a dark `bg` (toward black on a light one) just far enough to reach `min`
/// contrast against `bg`: keeps a calendar or category colour recognisable while it stays visible.
pub fn legible(fg: Color32, bg: Color32, min: f32) -> Color32 {
    let to = if luminance(bg) < 0.18 { Color32::WHITE } else { Color32::BLACK };
    let mix = |t: f32| {
        let m =
            |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgb(m(fg.r(), to.r()), m(fg.g(), to.g()), m(fg.b(), to.b()))
    };
    (0..=20).map(|i| mix(i as f32 / 20.0)).find(|c| contrast(*c, bg) >= min).unwrap_or(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Release check: dark-theme primary buttons drew white text on the light teal accent (about 2.4:1).
    #[test]
    fn text_on_the_accent_reaches_aa_in_both_themes() {
        for t in [Theme::light(), Theme::dark()] {
            let c = contrast(t.on_accent, t.accent);
            assert!(c >= 4.5, "dark={}: on_accent {c:.2}:1", t.dark);
        }
    }

    /// Release check (dark theme): the blue unread subject on a highlighted (flagged) row sat on a strong dark
    /// red. The ratio passed (6.8:1), but the two hues clashed. The tint is now a muted rose that keeps the
    /// ratio and keeps flagged rows apart from the pane. The selected row is checked too.
    #[test]
    fn unread_text_on_tinted_rows_reaches_aa() {
        for t in [Theme::light(), Theme::dark()] {
            for (name, bg) in [("flagged", t.flagged_row), ("selected", t.selected), ("pane", t.pane)] {
                let c = contrast(t.unread, bg);
                assert!(c >= 4.5, "dark={}: unread on {name} {c:.2}:1", t.dark);
            }
        }
        let d = Theme::dark();
        assert!(contrast(d.unread, d.flagged_row) >= 6.0);
        let f = d.flagged_row;
        let chroma = f.r().max(f.g()).max(f.b()) - f.r().min(f.g()).min(f.b());
        assert!(chroma <= 0x12, "dark flagged tint too saturated: {f:?}");
        assert_ne!(f, d.pane, "flagged rows still stand out");
        assert!(f.r() > f.g(), "still a warm tint");
    }

    /// Release check: in the light theme the People and Tasks search boxes had the pane's own white fill and
    /// no border, so they read as plain labels.
    #[test]
    fn text_boxes_stand_apart_from_the_pane() {
        for t in [Theme::light(), Theme::dark()] {
            let fill = t.visuals().text_edit_bg_color();
            assert_ne!(fill, t.pane, "dark={}", t.dark);
            assert!(contrast(fill, t.pane) >= 1.08, "dark={}: field {:.2}:1", t.dark, contrast(fill, t.pane));
            assert!(contrast(t.text, fill) >= 4.5);
        }
    }

    #[test]
    fn legible_lifts_dark_colours_on_dark_fills() {
        let navy = Color32::from_rgb(0x1f, 0x3a, 0x68);
        let cell = Theme::dark().selected;
        assert!(contrast(navy, cell) < 3.0);
        let lifted = legible(navy, cell, 3.0);
        assert!(contrast(lifted, cell) >= 3.0);
        assert!(lifted.b() >= lifted.r(), "still bluish: {lifted:?}");
        // Already legible colours are kept as they are.
        let light = Theme::light();
        assert_eq!(legible(light.accent, light.pane, 3.0), light.accent);
        assert_eq!(contrast(Color32::BLACK, Color32::WHITE).round(), 21.0);
    }
}
