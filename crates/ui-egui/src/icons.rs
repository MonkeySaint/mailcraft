//! Original SVG icons (assets/icons/*.svg, our own artwork, monochrome using currentColor), rasterised on
//! demand and tinted to the theme (icons area). Missing names fall back to a neutral glyph (never panics).
//!
//! Every SVG is embedded with `include_str!` through [`ICONS`]. At paint time an icon is rasterised once per
//! pixel size with `egui_extras::image` (resvg underneath) with `currentColor` turned white, uploaded as a
//! texture, and drawn with the caller's tint (egui multiplies texture colour by tint). This works without
//! `egui_extras::install_image_loaders`; [`Icons::uri`] / [`Icons::image`] are there for code that prefers
//! `egui::Image` widgets.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Color32, Rect, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};

macro_rules! icon_table {
    ($($name:literal),* $(,)?) => {
        /// All bundled icons as `(name, svg source)`, sorted by name.
        pub const ICONS: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../../../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icon_table![
    "account",
    "address-book",
    "align-center",
    "align-left",
    "align-right",
    "appointment",
    "archive",
    "archive-folder",
    "attach",
    "back",
    "bell",
    "block",
    "bold",
    "bullets",
    "calendar",
    "calendar-reminder",
    "categorize",
    "category-tag",
    "certificate",
    "check",
    "check-names",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "chevron-up",
    "clean-up",
    "close",
    "collapse-ribbon",
    "contact-group",
    "conversation",
    "copy",
    "cut",
    "day",
    "delete",
    "deleted",
    "draft",
    "drafts",
    "ellipsis",
    "email",
    "envelope-open",
    "error",
    "favorites",
    "filter",
    "flag",
    "flag-complete",
    "folder",
    "folder-open",
    "folder-pane",
    "folders",
    "font-color",
    "format-painter",
    "forward",
    "forwarded",
    "help",
    "highlight",
    "ignore",
    "importance-high",
    "importance-low",
    "inbox",
    "indent",
    "info",
    "italic",
    "junk",
    "junk-folder",
    "justify",
    "link",
    "location",
    "mail",
    "maximize",
    "meeting",
    "minimize",
    "minus",
    "month",
    "more",
    "more-apps",
    "move",
    "new-items",
    "new-mail",
    "note",
    "notes",
    "numbering",
    "outbox",
    "outdent",
    "overflow",
    "paperclip",
    "paste",
    "people",
    "person",
    "phone",
    "picture",
    "pin",
    "plus",
    "print",
    "private",
    "quick-steps",
    "read",
    "reading-pane",
    "recurrence",
    "redo",
    "replied",
    "reply",
    "reply-all",
    "restore",
    "ribbon-display",
    "rss",
    "rules",
    "save",
    "schedule",
    "search",
    "search-folder",
    "send",
    "sent",
    "settings",
    "shortcuts",
    "signature",
    "sort",
    "sort-down",
    "sort-up",
    "spelling",
    "strike",
    "sync",
    "table",
    "task",
    "tasks",
    "today",
    "underline",
    "undo",
    "unread",
    "view",
    "warning",
    "week",
    "work-week",
    "zoom-in",
    "zoom-out",
];

/// Rasterised textures kept before the cache is flushed (sizes change with zoom / DPI).
const CACHE_LIMIT: usize = 768;
/// Largest edge, in physical pixels, an icon is ever rasterised at.
const MAX_PX: u32 = 512;

#[derive(Default)]
pub struct Icons {
    pub textures: BTreeMap<(String, u32), egui::TextureHandle>,
    /// (name, px) pairs that failed to rasterise; painted with the fallback glyph without retrying.
    failed: BTreeSet<(String, u32)>,
}

impl std::fmt::Debug for Icons {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Icons").field("cached", &self.textures.len()).finish()
    }
}

impl Icons {
    /// Paints icon `name` into `rect`, tinted with `tint`.
    pub fn paint(&mut self, ui: &egui::Ui, rect: egui::Rect, name: &str, tint: egui::Color32) {
        if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 || !ui.is_rect_visible(rect) {
            return;
        }
        let ctx = ui.ctx();
        let ppp = ctx.pixels_per_point().max(0.25);
        let side = rect.width().min(rect.height());
        let px = px_for(side, ppp);
        // Square, centred, snapped to the physical pixel grid so 1:1 textures stay crisp.
        let edge = px as f32 / ppp;
        let min = rect.center() - vec2(edge, edge) * 0.5;
        let min = pos2((min.x * ppp).round() / ppp, (min.y * ppp).round() / ppp);
        let square = Rect::from_min_size(min, vec2(edge, edge));
        match self.texture(ctx, name, px) {
            Some(tex) => {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(tex.id(), square, uv, tint);
            }
            None => paint_fallback(ui.painter(), square, tint),
        }
    }

    /// Names of all bundled icons.
    pub fn names() -> Vec<&'static str> {
        ICONS.iter().map(|(n, _)| *n).collect()
    }

    /// True when `name` is a bundled icon.
    pub fn has(name: &str) -> bool {
        svg(name).is_some()
    }

    /// The raw SVG source of a bundled icon.
    pub fn svg(name: &str) -> Option<&'static str> {
        svg(name)
    }

    /// `bytes://` URI for use with `egui::Image::from_uri` once [`Icons::register`] has run and
    /// `egui_extras::install_image_loaders` is installed. The registered bytes are white, so tint the image.
    pub fn uri(name: &str) -> String {
        format!("bytes://icons/{name}.svg")
    }

    /// Registers every icon's (white) bytes with egui under [`Icons::uri`], for `egui::Image` users.
    pub fn register(ctx: &egui::Context) {
        for (name, src) in ICONS {
            ctx.include_bytes(Self::uri(name), white_svg(src).into_bytes());
        }
    }

    /// An `egui::Image` for `name` at `size` points, tinted (needs [`Icons::register`] + image loaders).
    pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
        egui::Image::from_uri(Self::uri(name)).fit_to_exact_size(vec2(size, size)).tint(tint)
    }

    /// Texture of `name` rasterised at `px`×`px` physical pixels (cached); `None` for unknown or broken icons.
    pub fn texture(&mut self, ctx: &egui::Context, name: &str, px: u32) -> Option<&TextureHandle> {
        let px = px.clamp(1, MAX_PX);
        let key = (name.to_owned(), px);
        if self.failed.contains(&key) {
            return None;
        }
        if !self.textures.contains_key(&key) {
            let Some(src) = svg(name) else {
                self.failed.insert(key);
                return None;
            };
            match rasterise(src, px) {
                Ok(img) => {
                    if self.textures.len() >= CACHE_LIMIT {
                        self.textures.clear();
                    }
                    let tex = ctx.load_texture(format!("icon:{name}@{px}"), img, TextureOptions::LINEAR);
                    self.textures.insert(key.clone(), tex);
                }
                Err(e) => {
                    log::warn!("icon {name:?} failed to rasterise at {px}px: {e}");
                    if self.failed.len() >= CACHE_LIMIT {
                        self.failed.clear();
                    }
                    self.failed.insert(key);
                    return None;
                }
            }
        }
        self.textures.get(&key)
    }
}

/// Physical pixel size for an icon `side` points tall at `ppp` pixels per point.
fn px_for(side: f32, ppp: f32) -> u32 {
    let px = (side * ppp).round();
    if px.is_finite() { px.clamp(1.0, MAX_PX as f32) as u32 } else { 16 }
}

fn svg(name: &str) -> Option<&'static str> {
    ICONS.binary_search_by(|(n, _)| (*n).cmp(name)).ok().and_then(|i| ICONS.get(i)).map(|(_, s)| *s)
}

/// The icon source with `currentColor` resolved to white so a texture tint gives the final colour.
fn white_svg(src: &str) -> String {
    src.replace("currentColor", "#ffffff")
}

/// Rasterises an SVG source to a white-on-transparent image of `px`×`px`.
fn rasterise(src: &str, px: u32) -> Result<egui::ColorImage, String> {
    let hint = egui::SizeHint::Size { width: px, height: px, maintain_aspect_ratio: true };
    egui_extras::image::load_svg_bytes_with_size(white_svg(src).as_bytes(), hint, &Default::default())
}

/// Neutral placeholder for unknown icons: a small rounded square with a centre dot.
fn paint_fallback(painter: &egui::Painter, rect: Rect, tint: Color32) {
    let r = rect.shrink(rect.width() * 0.22);
    let w = (rect.width() / 14.0).max(1.0);
    painter.rect_stroke(r, r.width() * 0.2, Stroke::new(w, tint), StrokeKind::Middle);
    painter.circle_filled(r.center(), w, tint);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared icon-name list every UI area may use (whitespace separated).
    const SHARED: &str = "\
        mail calendar people tasks notes folders shortcuts more-apps new-mail new-items delete archive \
        ignore clean-up junk block reply reply-all forward meeting more move rules copy categorize flag \
        flag-complete unread read search filter sort sort-up sort-down inbox drafts sent deleted \
        junk-folder outbox archive-folder rss search-folder folder folder-open favorites account attach \
        paperclip importance-high importance-low replied forwarded draft send save undo redo sync \
        settings help print close minimize maximize restore chevron-down chevron-up chevron-left \
        chevron-right back check plus minus zoom-in zoom-out address-book check-names signature link \
        picture table bold italic underline strike font-color highlight bullets numbering indent outdent \
        align-left align-center align-right justify paste cut format-painter spelling reading-pane \
        folder-pane view conversation quick-steps person contact-group appointment task note today day \
        work-week week month schedule info warning error pin overflow collapse-ribbon ribbon-display \
        envelope-open bell calendar-reminder location private recurrence category-tag phone email certificate \
        ellipsis";

    #[test]
    fn every_shared_name_exists() {
        let missing: Vec<_> = SHARED.split_whitespace().filter(|n| !Icons::has(n)).collect();
        assert!(missing.is_empty(), "missing icons: {missing:?}");
    }

    #[test]
    fn names_sorted_and_unique() {
        let names = Icons::names();
        assert!(names.windows(2).all(|w| w[0] < w[1]), "ICONS must be sorted with no duplicates");
        let shared: Vec<_> = SHARED.split_whitespace().collect();
        let set: BTreeSet<_> = shared.iter().collect();
        assert_eq!(set.len(), shared.len(), "duplicate in shared list");
        assert_eq!(shared.len(), 134);
    }

    #[test]
    fn table_matches_asset_dir() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".svg")).map(str::to_owned))
            .collect();
        on_disk.sort();
        let table: Vec<String> = Icons::names().into_iter().map(str::to_owned).collect();
        assert_eq!(on_disk, table);
    }

    #[test]
    fn sources_are_simple_monochrome_svg() {
        for (name, src) in ICONS {
            assert!(src.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""), "{name}");
            assert!(src.contains("viewBox=\"0 0 24 24\""), "{name}");
            assert!(src.trim_end().ends_with("</svg>"), "{name}");
            assert!(src.contains("currentColor"), "{name}");
            assert!(!src.contains("<text") && !src.contains("href") && !src.contains("<image"), "{name}");
            assert!(src.len() < 2048, "{name} is {} bytes", src.len());
        }
    }

    #[test]
    fn every_svg_parses_and_draws_white() {
        for (name, src) in ICONS {
            for px in [16, 24, 48] {
                let img = rasterise(src, px).unwrap_or_else(|e| panic!("{name}@{px}: {e}"));
                assert_eq!(img.size, [px as usize, px as usize], "{name}@{px}");
                let ink = img.pixels.iter().filter(|p| p.a() > 0).count();
                assert!(ink > 0, "{name}@{px} rendered nothing");
                // Monochrome white (premultiplied): colour channels equal alpha.
                assert!(
                    img.pixels.iter().all(|p| p.r() == p.a() && p.g() == p.a() && p.b() == p.a()),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn paint_known_unknown_and_degenerate() {
        let ctx = egui::Context::default();
        let mut icons = Icons::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(16.0, 16.0));
            icons.paint(ui, r, "mail", Color32::RED);
            icons.paint(ui, r, "no-such-icon", Color32::RED);
            icons.paint(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 5.0)), "mail", Color32::RED);
            icons.paint(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(f32::NAN, 5.0)), "mail", Color32::RED);
            icons.paint(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(1e9, 1e9)), "mail", Color32::RED);
        });
        out.textures_delta.clear();
        assert!(icons.textures.contains_key(&("mail".to_owned(), 16)));
        assert!(icons.textures.keys().all(|(_, px)| *px <= MAX_PX));
        assert!(icons.texture(&ctx, "no-such-icon", 16).is_none());
    }

    #[test]
    fn px_rounding() {
        assert_eq!(px_for(16.0, 1.0), 16);
        assert_eq!(px_for(16.0, 1.5), 24);
        assert_eq!(px_for(0.01, 1.0), 1);
        assert_eq!(px_for(f32::INFINITY, 1.0), 16);
    }
}
