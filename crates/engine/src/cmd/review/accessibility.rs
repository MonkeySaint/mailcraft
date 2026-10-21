//! The accessibility checker (Review › Check Accessibility) and its warnings while composing (File › Options ›
//! Accessibility › Accessibility checker options).
//!
//! [`check`] walks the compose body and reports, most serious first:
//! - errors: pictures without alternative text, and drawings (shapes, text boxes) with neither alternative
//!   text nor text of their own;
//! - warnings: text that is hard to read against what is behind it (WCAG 2.x contrast: 4.5:1, 3:1 for large
//!   text), tables without a header row (the first row in bold or as headings), merged table cells, link text
//!   that does not say where the link goes ("click here", a bare long address), alternative text that is
//!   just a file name or a drawing's shape name, a drawing's text hard to read against its fill;
//! - tips: long messages without headings, skipped heading levels, empty table rows used for spacing,
//!   repeated blank characters (runs of spaces, several empty paragraphs).
//!
//! [`status`] decides whether the compose shows an accessibility warning now, from "Accessibility warnings:"
//! (`whileWorking`: whenever the checker finds errors or warnings; `when`: only when one of the ticked
//! conditions holds; `checkerOnly`: never, only Check Accessibility) and the conditions: composing on a dark
//! background, to a large audience, to someone outside my organization, or with high importance.
//!
//! "At least one recipient prefers accessible content" needs a directory service that publishes that
//! preference; MailCraft has none, so that condition never holds.
//!
//! [`auto_alt_text`] is "Automatically generate alt text for me" without a cloud service: a description made
//! from a descriptive file name ("team-lunch_by_the-lake.jpg" → "Team lunch by the lake"), or nothing for
//! camera and counter names, so the checker still asks for one.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::insert::{drawing, shapes};
use crate::compose::{ComposeState, DocPos};
use mailcraft_geom::Color;
use mailcraft_model::rich::{Drawing, DrawingKind, TableCell};
use mailcraft_model::{Block, Doc, Importance, Inline, ParaStyle, TextStyle};

/// Minimum contrast ratio for body text (WCAG 2.x AA, normal text).
pub const MIN_CONTRAST: f32 = 4.5;
/// Minimum contrast ratio for large text (18 pt, or 14 pt bold).
pub const MIN_CONTRAST_LARGE: f32 = 3.0;
/// Paragraph count above which a body without headings is flagged.
pub const HEADINGS_AFTER_PARAGRAPHS: usize = 12;
/// Recipients from which a message goes to a "large audience".
pub const LARGE_AUDIENCE: usize = 25;
/// Spaces in a row reported as repeated blank characters.
const REPEATED_SPACES: usize = 3;
/// Empty paragraphs in a row reported as repeated blank characters.
const REPEATED_EMPTY_PARAGRAPHS: usize = 2;
/// Link text longer than this that is just the address is "unclear".
const BARE_URL_CHARS: usize = 30;
/// Most issues reported.
pub const MAX_ISSUES: usize = 500;
/// Deepest nesting walked.
const MAX_DEPTH: usize = 64;

/// Link text that does not say where a link goes (compared lower-case, trimmed of punctuation).
const VAGUE_LINK_TEXT: [&str; 12] = [
    "click here",
    "here",
    "click",
    "link",
    "this link",
    "this",
    "more",
    "read more",
    "learn more",
    "more info",
    "go",
    "url",
];

/// Picture file extensions (alternative text that is a file name).
const PICTURE_EXTENSIONS: [&str; 9] =
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tif", ".tiff", ".svg"];

/// How serious an issue is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Tip,
}

impl Severity {
    pub fn id(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Tip => "tip",
        }
    }
}

/// One accessibility issue.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yIssue {
    /// `missingAltText`, `altTextIsFileName`, `altTextIsShapeName`, `lowContrast`, `tableHeader`, `mergedCells`,
    /// `unclearLinkText`, `noHeadings`, `headingOrder`, `blankTableRows`, `emptyParagraphs`, `repeatedSpaces`.
    pub kind: &'static str,
    pub severity: Severity,
    pub message: String,
    /// Where in the body (the start of the paragraph or table it is in), when it has one place.
    pub at: Option<DocPos>,
    /// What it is about (picture source, colour, link address).
    pub target: Option<String>,
    /// The drawing (text box, shape, WordArt) it is about.
    pub drawing: Option<u32>,
}

impl A11yIssue {
    pub fn to_json(&self) -> Value {
        let mut v = json!({"kind": self.kind, "severity": self.severity.id(), "message": self.message});
        if let Some(o) = v.as_object_mut() {
            if let Some(at) = &self.at {
                o.insert("at".into(), super::super::compose::pos_json(at));
            }
            if let Some(t) = &self.target {
                o.insert("target".into(), json!(t));
            }
            if let Some(d) = self.drawing {
                o.insert("drawing".into(), json!(d));
            }
        }
        v
    }
}

struct Walk<'a> {
    doc: &'a Doc,
    out: Vec<A11yIssue>,
    /// Colour pairs (text, background) already reported (at most [`MAX_ISSUES`]).
    contrast_seen: HashSet<(Color, Color)>,
    /// Level of the previous heading.
    last_heading: Option<u8>,
    paragraphs: usize,
    headings: usize,
    /// Empty top-level paragraphs in a row, and whether that run was reported.
    empty_run: usize,
    empty_reported: bool,
}

impl Walk<'_> {
    fn push(
        &mut self,
        kind: &'static str,
        severity: Severity,
        message: &str,
        at: &[usize],
        target: Option<String>,
    ) {
        if self.out.len() < MAX_ISSUES {
            self.out.push(A11yIssue {
                kind,
                severity,
                message: message.to_owned(),
                at: Some(DocPos::new(at.to_vec(), 0)),
                target,
                drawing: None,
            });
        }
    }

    fn blocks(&mut self, blocks: &[Block], path: &mut Vec<usize>, bg: Color, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let top = depth == 0;
        for (i, b) in blocks.iter().enumerate() {
            path.push(i);
            let empty_para = matches!(b, Block::Paragraph { inlines, .. } if inline_text(inlines).trim().is_empty() && !has_image(inlines));
            if top {
                if empty_para {
                    self.empty_run = self.empty_run.saturating_add(1);
                    if self.empty_run >= REPEATED_EMPTY_PARAGRAPHS && !self.empty_reported {
                        self.empty_reported = true;
                        self.push(
                            "emptyParagraphs",
                            Severity::Tip,
                            "Several empty paragraphs in a row: use paragraph spacing instead.",
                            path,
                            None,
                        );
                    }
                } else {
                    self.empty_run = 0;
                    self.empty_reported = false;
                }
            }
            match b {
                Block::Paragraph { style, inlines } => {
                    if !empty_para {
                        self.paragraphs = self.paragraphs.saturating_add(1);
                    }
                    self.inlines(inlines, path, para_bg(style, bg), false);
                }
                Block::Heading { level, style, inlines } => {
                    self.headings = self.headings.saturating_add(1);
                    if let Some(prev) = self.last_heading
                        && *level > prev.saturating_add(1)
                    {
                        self.push(
                            "headingOrder",
                            Severity::Tip,
                            "A heading level is skipped: headings should go in order (1, 2, 3).",
                            path,
                            None,
                        );
                    }
                    self.last_heading = Some(*level);
                    self.inlines(inlines, path, para_bg(style, bg), true);
                }
                Block::List { items, .. } => {
                    for (k, item) in items.iter().enumerate() {
                        path.push(k);
                        self.blocks(item, path, bg, depth.saturating_add(1));
                        path.pop();
                    }
                }
                Block::Quote { blocks } => self.blocks(blocks, path, bg, depth.saturating_add(1)),
                Block::Container { style, blocks } => {
                    self.blocks(blocks, path, para_bg(style, bg), depth.saturating_add(1));
                }
                Block::Table { rows, .. } => self.table(rows, path, bg, depth),
                Block::Pre { .. } | Block::Rule => {}
            }
            path.pop();
        }
    }

    fn table(&mut self, rows: &[Vec<TableCell>], path: &mut Vec<usize>, bg: Color, depth: usize) {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if rows.len() > 1 && cols > 1 && !rows.first().is_some_and(|r| header_row(r)) {
            self.push(
                "tableHeader",
                Severity::Warning,
                "A table has no header row: put the column names in its first row, in bold.",
                path,
                None,
            );
        }
        if rows.iter().flatten().any(|c| c.colspan > 1 || c.rowspan > 1) {
            self.push(
                "mergedCells",
                Severity::Warning,
                "A table has merged cells, which are hard to follow with a screen reader.",
                path,
                None,
            );
        }
        if rows.len() > 1 && rows.iter().any(|r| !r.is_empty() && r.iter().all(cell_empty)) {
            self.push(
                "blankTableRows",
                Severity::Tip,
                "A table has empty rows: avoid blank rows for spacing.",
                path,
                None,
            );
        }
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                path.push(r);
                path.push(c);
                self.blocks(&cell.blocks, path, cell.background.unwrap_or(bg), depth.saturating_add(1));
                path.pop();
                path.pop();
            }
        }
    }

    fn inlines(&mut self, inlines: &[Inline], path: &[usize], bg: Color, heading: bool) {
        for i in inlines {
            match i {
                Inline::Text { text, style } => self.text(text, style, path, bg, heading),
                Inline::Anchor { .. } => {}
                Inline::Link { title: _, href, children } => {
                    let shown = inline_text(children);
                    let plain = shown.trim().trim_matches(|c: char| c.is_ascii_punctuation()).to_lowercase();
                    let bare = shown.trim() == href.trim() && shown.trim().chars().count() > BARE_URL_CHARS;
                    let mailto =
                        href.trim_start().get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:"));
                    if !mailto && (VAGUE_LINK_TEXT.contains(&plain.as_str()) || bare) {
                        self.push(
                            "unclearLinkText",
                            Severity::Warning,
                            "Link text should say where the link goes.",
                            path,
                            Some(href.clone()),
                        );
                    }
                    for ch in children {
                        if let Inline::Text { text, style } = ch {
                            self.text(text, style, path, bg, heading);
                        }
                    }
                }
                Inline::Image { src, alt, .. } => {
                    let a = alt.trim();
                    if a.is_empty() {
                        self.push(
                            "missingAltText",
                            Severity::Error,
                            "A picture has no alternative text.",
                            path,
                            Some(src.clone()),
                        );
                    } else if looks_like_file_name(a) {
                        self.push(
                            "altTextIsFileName",
                            Severity::Warning,
                            "A picture's alternative text is just its file name: describe the picture.",
                            path,
                            Some(src.clone()),
                        );
                    }
                }
                Inline::LineBreak => {}
            }
        }
    }

    fn text(&mut self, text: &str, style: &TextStyle, path: &[usize], bg: Color, heading: bool) {
        // Nothing more is reported once the list is full (a body of many thousands of runs, each in its own
        // near-background colour, stays a linear walk).
        if self.out.len() >= MAX_ISSUES || text.trim().is_empty() {
            return;
        }
        if text.contains(&" ".repeat(REPEATED_SPACES)) && !self.out.iter().any(|i| i.kind == "repeatedSpaces")
        {
            self.push(
                "repeatedSpaces",
                Severity::Tip,
                "Several spaces in a row: use tabs, indents or a table to line text up.",
                path,
                None,
            );
        }
        let base = &self.doc.base;
        let Some(fg) = style.color.or(base.color) else {
            return;
        };
        let behind = style.highlight.unwrap_or(bg);
        let pt = style.size_pt.or(base.size_pt).unwrap_or(11.0);
        let bold = heading || style.bold.or(base.bold) == Some(true);
        let large = pt >= 18.0 || (bold && pt >= 14.0);
        let min = if large { MIN_CONTRAST_LARGE } else { MIN_CONTRAST };
        if fg.contrast(&behind) < min && self.contrast_seen.insert((fg, behind)) {
            self.push(
                "lowContrast",
                Severity::Warning,
                "Some text is hard to read against its background.",
                path,
                Some(fg.to_hex()),
            );
        }
    }
}

fn para_bg(style: &ParaStyle, bg: Color) -> Color {
    style.background.unwrap_or(bg)
}

fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Link { children, .. } => out.push_str(&inline_text(children)),
            Inline::LineBreak => out.push(' '),
            Inline::Image { .. } | Inline::Anchor { .. } => {}
        }
    }
    out
}

fn has_image(inlines: &[Inline]) -> bool {
    inlines.iter().any(|i| match i {
        Inline::Image { .. } => true,
        Inline::Link { children, .. } => has_image(children),
        _ => false,
    })
}

fn cell_empty(c: &TableCell) -> bool {
    c.blocks.iter().all(|b| match b {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
            inline_text(inlines).trim().is_empty() && !has_image(inlines)
        }
        Block::Rule => true,
        _ => false,
    })
}

/// A header row: every cell with text is a heading or all bold.
fn header_row(row: &[TableCell]) -> bool {
    let mut any = false;
    for cell in row {
        for b in &cell.blocks {
            match b {
                Block::Heading { .. } => any = true,
                Block::Paragraph { inlines, .. } => {
                    let runs: Vec<&TextStyle> = inlines
                        .iter()
                        .filter_map(|i| match i {
                            Inline::Text { text, style } if !text.trim().is_empty() => Some(style),
                            _ => None,
                        })
                        .collect();
                    if runs.is_empty() {
                        continue;
                    }
                    if runs.iter().any(|s| s.bold != Some(true)) {
                        return false;
                    }
                    any = true;
                }
                _ => {}
            }
        }
    }
    any
}

fn looks_like_file_name(alt: &str) -> bool {
    let lower = alt.to_lowercase();
    !lower.contains(' ') && PICTURE_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

/// Every accessibility issue in compose `c`'s body, errors first, then warnings, then tips (each in body
/// order).
pub fn check(c: &ComposeState) -> Vec<A11yIssue> {
    let mut out = walk(&c.body).out;
    drawing_issues(c, &mut out);
    out.sort_by_key(|i| i.severity);
    out
}

/// The walk behind [`check`], unsorted.
fn walk(doc: &Doc) -> Walk<'_> {
    let mut w = Walk {
        doc,
        out: Vec::new(),
        contrast_seen: HashSet::new(),
        last_heading: None,
        paragraphs: 0,
        headings: 0,
        empty_run: 0,
        empty_reported: false,
    };
    let mut path = Vec::new();
    w.blocks(&doc.blocks, &mut path, doc.background.unwrap_or(Color::WHITE), 0);
    if w.paragraphs > HEADINGS_AFTER_PARAGRAPHS && w.headings == 0 && w.out.len() < MAX_ISSUES {
        w.out.push(A11yIssue {
            kind: "noHeadings",
            severity: Severity::Tip,
            message: "Long messages are easier to navigate with headings.".into(),
            at: None,
            target: None,
            drawing: None,
        });
    }
    w
}

/// Issues of the drawings floating over the body: a drawing that says nothing to a screen reader (no
/// alternative text and no text of its own, or only its shape's name), and text hard to read against the
/// drawing's fill (or the page behind a drawing without one).
fn drawing_issues(c: &ComposeState, out: &mut Vec<A11yIssue>) {
    if c.body.drawings.is_empty() {
        return;
    }
    let theme = c.theme_resolved();
    let page = c.body.background.unwrap_or(Color::WHITE);
    let mut push = |kind, severity, message: &str, d: &Drawing, target: Option<String>| {
        if out.len() < MAX_ISSUES {
            out.push(A11yIssue {
                kind,
                severity,
                message: message.to_owned(),
                at: None,
                target,
                drawing: Some(d.id),
            });
        }
    };
    for d in &c.body.drawings {
        let alt = d.alt.trim();
        let has_text = !d.text.trim().is_empty();
        // A canvas is a frame: what is drawn in it is checked on its own.
        if d.kind != DrawingKind::Canvas && !has_text {
            if alt.is_empty() {
                push("missingAltText", Severity::Error, "A drawing has no alternative text.", d, None);
            } else if alt.eq_ignore_ascii_case(shapes::label(&d.shape)) || alt.eq_ignore_ascii_case(&d.shape)
            {
                push(
                    "altTextIsShapeName",
                    Severity::Warning,
                    "A drawing's alternative text is just its shape's name: say what it shows.",
                    d,
                    Some(alt.to_owned()),
                );
            }
        }
        if has_text && d.kind != DrawingKind::WordArt {
            let fg = d.text_style.color.unwrap_or_else(|| drawing::default_text_color(d, &theme));
            let bg = d.fill.unwrap_or(page);
            let pt = d.text_style.size_pt.unwrap_or(11.0);
            let large = pt >= 18.0 || (d.text_style.bold == Some(true) && pt >= 14.0);
            let min = if large { MIN_CONTRAST_LARGE } else { MIN_CONTRAST };
            if fg.contrast(&bg) < min {
                push(
                    "lowContrast",
                    Severity::Warning,
                    "The text of a drawing is hard to read against its fill.",
                    d,
                    Some(fg.to_hex()),
                );
            }
        }
    }
}

/// Accessibility checker options › "Accessibility warnings:" (`whileWorking`, `when`, `checkerOnly`).
pub fn warnings_mode(s: &Session) -> String {
    match s.settings.text("accessibility.warnings").as_str() {
        m @ ("whileWorking" | "checkerOnly") => m.to_owned(),
        _ => "when".to_owned(),
    }
}

/// The domains of the user's own accounts ("my organization").
fn own_domains(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = s
        .mailbox
        .accounts
        .iter()
        .filter_map(|a| a.address.trim().rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()))
        .filter(|d| !d.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn inside(domain: &str, own: &[String]) -> bool {
    let d = domain.to_ascii_lowercase();
    own.iter().any(|o| d == *o || d.strip_suffix(o.as_str()).is_some_and(|rest| rest.ends_with('.')))
}

/// Which of the ticked "Show me accessibility warnings when" conditions hold for compose `c`.
pub fn conditions(s: &Session, c: &ComposeState) -> Vec<&'static str> {
    let st = &s.settings;
    let mut out = Vec::new();
    let bg = c.body.background.unwrap_or(Color::WHITE);
    let dark_bg = bg.contrast(&Color::WHITE) > bg.contrast(&Color::BLACK);
    if st.bool("accessibility.warnDarkBackground") && ((s.view.dark && !c.light_canvas) || dark_bg) {
        out.push("darkBackground");
    }
    let recipients = c.recipients();
    if st.bool("accessibility.warnLargeAudience") && recipients.len() >= LARGE_AUDIENCE {
        out.push("largeAudience");
    }
    if st.bool("accessibility.warnOutsideOrganization") {
        let own = own_domains(s);
        let outside = !own.is_empty()
            && recipients.iter().any(|(_, a)| a.rsplit_once('@').is_some_and(|(_, d)| !inside(d, &own)));
        if outside {
            out.push("outsideOrganization");
        }
    }
    if st.bool("accessibility.warnHighImportance") && c.importance == Importance::High {
        out.push("highImportance");
    }
    out
}

/// Whether compose `c` shows an accessibility warning now, with the conditions that hold and the counts.
/// The checker runs only when the warning could show (`checked`): not for `checkerOnly`, nor for `when`
/// while no ticked condition holds, so the counts are then 0 (the UI asks again after every edit).
pub fn status(s: &Session, c: &ComposeState) -> Value {
    let mode = warnings_mode(s);
    let reasons = conditions(s, c);
    let could_show = match mode.as_str() {
        "whileWorking" => true,
        "checkerOnly" => false,
        _ => !reasons.is_empty(),
    };
    let issues = if could_show { check(c) } else { Vec::new() };
    let count = |sev: Severity| issues.iter().filter(|i| i.severity == sev).count();
    let (errors, warnings, tips) = (count(Severity::Error), count(Severity::Warning), count(Severity::Tip));
    let show = could_show && errors.saturating_add(warnings) > 0;
    json!({
        "compose": c.id,
        "mode": mode,
        "show": show,
        "checked": could_show,
        "reasons": reasons,
        "errors": errors,
        "warnings": warnings,
        "tips": tips,
    })
}

/// Words of a file name that say nothing about the picture.
const GENERIC_NAME_WORDS: [&str; 14] = [
    "img", "image", "images", "dsc", "dscn", "dcim", "pic", "picture", "photo", "pxl", "scan", "untitled",
    "copy", "file",
];

/// Alternative text made from a picture's file name: the words of a descriptive name in sentence case
/// ("team-lunch_by_the-lake.jpg" → "Team lunch by the lake", "OfficeFloorPlan.png" → "Office floor plan"),
/// "Screenshot" for screenshots, `None` for camera and counter names ("IMG_2041.JPG", "image001.png").
pub fn auto_alt_text(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = match base.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && ext.len() <= 5 => s,
        _ => base,
    };
    // Split at separators, digits and lower→upper case changes.
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in stem.chars().take(256) {
        if ch.is_alphabetic() {
            if ch.is_uppercase() && prev_lower && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            cur.extend(ch.to_lowercase());
            prev_lower = ch.is_lowercase();
        } else {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    // A screenshot named only by its date: "Screenshot".
    let screenshot_words = ["screenshot", "screen", "shot", "at", "from"];
    if words.iter().any(|w| w == "screenshot" || w == "screen")
        && words
            .iter()
            .all(|w| GENERIC_NAME_WORDS.contains(&w.as_str()) || screenshot_words.contains(&w.as_str()))
    {
        return Some("Screenshot".to_owned());
    }
    let meaningful: Vec<&String> =
        words.iter().filter(|w| !GENERIC_NAME_WORDS.contains(&w.as_str()) && w.chars().count() > 1).collect();
    if meaningful.is_empty() || !meaningful.iter().any(|w| w.chars().count() >= 3) {
        return None;
    }
    let text = words
        .iter()
        .filter(|w| !GENERIC_NAME_WORDS.contains(&w.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = text.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_text_from_file_names() {
        assert_eq!(auto_alt_text("team-lunch_by_the-lake.jpg").as_deref(), Some("Team lunch by the lake"));
        assert_eq!(auto_alt_text("C:\\Pictures\\OfficeFloorPlan.png").as_deref(), Some("Office floor plan"));
        assert_eq!(
            auto_alt_text("/home/u/Screenshot from 2026-10-20 14-02-11.png").as_deref(),
            Some("Screenshot")
        );
        assert_eq!(auto_alt_text("budget chart 2026.gif").as_deref(), Some("Budget chart"));
        for camera in [
            "IMG_2041.JPG",
            "image001.png",
            "DSC00042.jpeg",
            "PXL_20261020_1.jpg",
            "x.png",
            "",
            "...",
            "a1b2.png",
        ] {
            assert_eq!(auto_alt_text(camera), None, "{camera}");
        }
    }

    #[test]
    fn many_low_contrast_colours_stay_a_bounded_walk() {
        // Hidden-text spam: tens of thousands of runs, each in its own near-white colour on white.
        let ch = |n: u32| u8::try_from(255 - n % 40).unwrap();
        let runs: Vec<Inline> = (0..30_000u32)
            .map(|i| Inline::Text {
                text: "x ".into(),
                style: TextStyle {
                    color: Some(Color::rgb(ch(i), ch(i / 40), ch(i / 1600))),
                    ..Default::default()
                },
            })
            .collect();
        let para = |inlines: Vec<Inline>| Doc {
            blocks: vec![Block::Paragraph { style: Default::default(), inlines }],
            ..Doc::default()
        };
        let doc = para(runs);
        let w = walk(&doc);
        assert_eq!(w.out.len(), MAX_ISSUES);
        assert!(w.out.iter().all(|i| i.kind == "lowContrast"));
        assert!(w.contrast_seen.len() <= MAX_ISSUES, "{} pairs kept", w.contrast_seen.len());
        // The same colour pair is reported once.
        let same = |n: usize| Inline::Text {
            text: format!("run {n}"),
            style: TextStyle { color: Some(Color::rgb(230, 230, 230)), ..Default::default() },
        };
        assert_eq!(walk(&para((0..50).map(same).collect())).out.len(), 1);
    }

    #[test]
    fn domains_inside_the_organization() {
        let own = vec!["example.com".to_owned()];
        assert!(inside("example.com", &own) && inside("Mail.Example.com", &own));
        assert!(!inside("example.org", &own) && !inside("badexample.com", &own));
    }
}
