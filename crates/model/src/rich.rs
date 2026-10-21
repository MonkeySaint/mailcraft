//! Rich-text document tree shared by the HTML importer (`mailcraft-html`), the layout engine
//! (`mailcraft-layout`) and the compose editor (engine). Deliberately small: what mail bodies need.

use mailcraft_geom::Color;
use serde::{Deserialize, Serialize};

/// Character formatting. `None` fields inherit (from the parent / document defaults).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub superscript: Option<bool>,
    pub subscript: Option<bool>,
    pub monospace: Option<bool>,
    /// Font family list as written ("Aptos, Calibri, sans-serif"); font matching is `mailcraft-fonts`' job.
    pub font_family: Option<String>,
    /// Size in points.
    pub size_pt: Option<f32>,
    pub color: Option<Color>,
    pub highlight: Option<Color>,
    /// How underlined text is underlined (`None` = a single line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline_kind: Option<UnderlineKind>,
    /// Underline colour (`None` = the text colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline_color: Option<Color>,
    /// Two lines through the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_strike: Option<bool>,
    /// Lower-case letters drawn as smaller capitals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_caps: Option<bool>,
    /// Letters drawn as capitals (the text itself keeps its case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_caps: Option<bool>,
    /// Extra space between characters in points (negative = condensed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing_pt: Option<f32>,
    /// Baseline shift in points (positive = raised, negative = lowered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_pt: Option<f32>,
}

/// Underline styles (Font dialog › Underline style). Each has a CSS `text-decoration-style` equivalent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnderlineKind {
    #[default]
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

impl UnderlineKind {
    /// Every kind with its name (as `format.fontDialog` takes it).
    pub const ALL: [(UnderlineKind, &'static str); 5] = [
        (UnderlineKind::Single, "single"),
        (UnderlineKind::Double, "double"),
        (UnderlineKind::Dotted, "dotted"),
        (UnderlineKind::Dashed, "dashed"),
        (UnderlineKind::Wavy, "wavy"),
    ];

    /// The kind called `name` (`single`, `double`, `dotted`, `dashed`, `wavy`; case-insensitive).
    pub fn from_name(name: &str) -> Option<UnderlineKind> {
        Self::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(name.trim())).map(|(k, _)| *k)
    }

    /// The kind's name.
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(k, _)| *k == self).map_or("single", |(_, n)| n)
    }
}

/// Line spacing rule of a paragraph with a fixed line height ([`ParaStyle::line_px`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineRule {
    /// Every line is exactly that tall.
    #[default]
    Exactly,
    /// Lines are at least that tall (taller text makes them taller).
    AtLeast,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

/// Paragraph formatting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParaStyle {
    pub align: Align,
    /// Left indent in px.
    pub indent_px: f32,
    pub space_before_px: f32,
    pub space_after_px: f32,
    /// Line height multiplier (1.0 = font default).
    pub line_height: Option<f32>,
    pub background: Option<Color>,
    /// Right indent in px.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub right_indent_px: f32,
    /// First-line indent in px from the left indent (negative = a hanging indent).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_line_px: f32,
    /// Fixed line height in px with its rule (Exactly / At least); wins over `line_height`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_px: Option<(f32, LineRule)>,
    /// Line and Page Breaks: keep with the next paragraph, keep the lines together, page break before,
    /// and widow/orphan control turned off (it is on by default). They matter when the message is printed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_with_next: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_together: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub page_break_before: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_widow_control: bool,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Inline {
    Text {
        text: String,
        style: TextStyle,
    },
    Link {
        href: String,
        children: Vec<Inline>,
    },
    LineBreak,
    /// Image by URL (`cid:…`, `data:…`, `https:…`); remote URLs are blocked by default at display time.
    Image {
        src: String,
        alt: String,
        width: Option<f32>,
        height: Option<f32>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ListKind {
    Bullet,
    Numbered { start: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub blocks: Vec<Block>,
    pub colspan: u32,
    pub rowspan: u32,
    pub background: Option<Color>,
    pub width_px: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Block {
    Paragraph {
        style: ParaStyle,
        inlines: Vec<Inline>,
    },
    /// 1..=6
    Heading {
        level: u8,
        style: ParaStyle,
        inlines: Vec<Inline>,
    },
    List {
        kind: ListKind,
        items: Vec<Vec<Block>>,
    },
    /// Blockquote (also used for quoted replies when the original is indented).
    Quote {
        blocks: Vec<Block>,
    },
    /// Preformatted text (plain-text bodies are imported as paragraphs, not Pre).
    Pre {
        text: String,
    },
    Table {
        rows: Vec<Vec<TableCell>>,
        border: bool,
        width_px: Option<f32>,
    },
    /// Horizontal rule (the HTML reply separator).
    Rule,
    /// Generic container with a background/border (HTML `div`/`td` boxes in newsletters).
    Container {
        style: ParaStyle,
        blocks: Vec<Block>,
    },
}

/// A rich-text document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// Document default text style (compose default: Aptos-like body font, 12 pt).
    pub base: TextStyle,
    pub background: Option<Color>,
}

impl Doc {
    /// Plain text → one paragraph per line (empty lines kept as empty paragraphs). URLs stay text here;
    /// auto-linking is the HTML/compose layer's job.
    ///
    /// CRLF and lone CR count as line ends; one terminating line end does not create a trailing empty
    /// paragraph, so `Doc::from_plain_text(s).to_plain_text()` reproduces `s` up to line-end style.
    pub fn from_plain_text(text: &str) -> Doc {
        let normalised;
        let text = if text.contains('\r') {
            normalised = text.replace("\r\n", "\n").replace('\r', "\n");
            normalised.as_str()
        } else {
            text
        };
        let body = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            return Doc::default();
        }
        let blocks = body.split('\n').map(Block::text_paragraph).collect();
        Doc { blocks, ..Default::default() }
    }
    /// Plain-text rendering (paragraph per line, list bullets "• " / "1. ", table cells tab-separated,
    /// rule as a line of dashes).
    ///
    /// Lines are joined with "\n" without a trailing line end. Nested lists indent their continuation lines
    /// under the marker; images contribute their alt text; links their visible text (or the URL when the
    /// text is empty). Nesting deeper than [`MAX_PLAIN_DEPTH`] is not rendered.
    pub fn to_plain_text(&self) -> String {
        let mut lines = Vec::new();
        blocks_to_lines(&self.blocks, 0, &mut lines);
        lines.join("\n")
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
    /// Plain text of all inline content, paragraphs joined by spaces (search snippets, previews).
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for line in self.to_plain_text().split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(line);
        }
        out
    }
}

impl Block {
    /// A paragraph holding one unstyled text run (no inlines for an empty string).
    pub fn text_paragraph(text: &str) -> Block {
        let inlines = if text.is_empty() {
            Vec::new()
        } else {
            vec![Inline::Text { text: text.to_string(), style: TextStyle::default() }]
        };
        Block::Paragraph { style: ParaStyle::default(), inlines }
    }
}

/// Maximum block nesting rendered by [`Doc::to_plain_text`] (guards the stack against hostile trees).
pub const MAX_PLAIN_DEPTH: usize = 64;

/// Width of the plain-text horizontal rule.
const RULE_WIDTH: usize = 30;

fn blocks_to_lines(blocks: &[Block], depth: usize, out: &mut Vec<String>) {
    if depth > MAX_PLAIN_DEPTH {
        return;
    }
    for block in blocks {
        block_to_lines(block, depth, out);
    }
}

fn block_to_lines(block: &Block, depth: usize, out: &mut Vec<String>) {
    match block {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
            let mut text = String::new();
            inlines_to_text(inlines, depth, &mut text);
            out.extend(text.split('\n').map(str::to_string));
        }
        Block::List { kind, items } => {
            let mut number = match kind {
                ListKind::Bullet => 0,
                ListKind::Numbered { start } => *start,
            };
            for item in items {
                let marker = match kind {
                    ListKind::Bullet => "\u{2022} ".to_string(),
                    ListKind::Numbered { .. } => format!("{number}. "),
                };
                number = number.saturating_add(1);
                let mut inner = Vec::new();
                blocks_to_lines(item, depth + 1, &mut inner);
                if inner.is_empty() {
                    inner.push(String::new());
                }
                let pad = " ".repeat(marker.chars().count());
                for (i, line) in inner.into_iter().enumerate() {
                    if i == 0 {
                        out.push(format!("{marker}{line}"));
                    } else if line.is_empty() {
                        out.push(line);
                    } else {
                        out.push(format!("{pad}{line}"));
                    }
                }
            }
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => blocks_to_lines(blocks, depth + 1, out),
        Block::Pre { text } => {
            let text = text.replace("\r\n", "\n");
            out.extend(text.split('\n').map(str::to_string));
        }
        Block::Table { rows, .. } => {
            for row in rows {
                let cells: Vec<String> = row
                    .iter()
                    .map(|cell| {
                        let mut inner = Vec::new();
                        blocks_to_lines(&cell.blocks, depth + 1, &mut inner);
                        inner
                            .iter()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                            .replace('\t', " ")
                    })
                    .collect();
                out.push(cells.join("\t"));
            }
        }
        Block::Rule => out.push("-".repeat(RULE_WIDTH)),
    }
}

fn inlines_to_text(inlines: &[Inline], depth: usize, out: &mut String) {
    if depth > MAX_PLAIN_DEPTH {
        return;
    }
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Link { href, children } => {
                let before = out.len();
                inlines_to_text(children, depth + 1, out);
                if out.len() == before {
                    out.push_str(href);
                }
            }
            Inline::LineBreak => out.push('\n'),
            Inline::Image { alt, .. } => out.push_str(alt),
        }
    }
}
