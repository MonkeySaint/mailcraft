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
    /// Paragraph borders (Borders and Shading). Consecutive paragraphs with equal borders share one box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borders: Option<Box<ParaBorders>>,
}

/// Line style of a border (Borders and Shading › Style). Each has a CSS `border-style` equivalent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BorderLine {
    #[default]
    Single,
    Dotted,
    Dashed,
    Double,
    /// 3-D, carved in (CSS `groove`).
    Groove,
    /// 3-D, raised (CSS `ridge`).
    Ridge,
    Inset,
    Outset,
}

impl BorderLine {
    /// Every style with its name (as `format.borders` takes it) and its CSS keyword.
    pub const ALL: [(BorderLine, &'static str, &'static str); 8] = [
        (BorderLine::Single, "single", "solid"),
        (BorderLine::Dotted, "dotted", "dotted"),
        (BorderLine::Dashed, "dashed", "dashed"),
        (BorderLine::Double, "double", "double"),
        (BorderLine::Groove, "groove", "groove"),
        (BorderLine::Ridge, "ridge", "ridge"),
        (BorderLine::Inset, "inset", "inset"),
        (BorderLine::Outset, "outset", "outset"),
    ];

    /// The style called `name` (its own name or its CSS keyword; case-insensitive).
    pub fn from_name(name: &str) -> Option<BorderLine> {
        let n = name.trim();
        Self::ALL
            .iter()
            .find(|(_, a, b)| a.eq_ignore_ascii_case(n) || b.eq_ignore_ascii_case(n))
            .map(|(k, _, _)| *k)
    }

    /// The style's name.
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(k, _, _)| *k == self).map_or("single", |(_, n, _)| n)
    }

    /// The CSS `border-style` keyword.
    pub fn css(self) -> &'static str {
        Self::ALL.iter().find(|(k, _, _)| *k == self).map_or("solid", |(_, _, c)| c)
    }
}

/// One side of a border.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BorderEdge {
    pub line: BorderLine,
    /// `None` = Automatic (the text colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// Line width in points ([`BorderEdge::MIN_WIDTH_PT`]..=[`BorderEdge::MAX_WIDTH_PT`]).
    pub width_pt: f32,
}

impl Default for BorderEdge {
    fn default() -> Self {
        BorderEdge { line: BorderLine::Single, color: None, width_pt: BorderEdge::DEFAULT_WIDTH_PT }
    }
}

impl BorderEdge {
    pub const MIN_WIDTH_PT: f32 = 0.25;
    pub const MAX_WIDTH_PT: f32 = 6.0;
    pub const DEFAULT_WIDTH_PT: f32 = 0.5;
    /// The widths the Borders and Shading dialog offers (points).
    pub const WIDTHS_PT: [f32; 9] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.25, 3.0, 4.5, 6.0];

    /// An edge of `line` style, `width_pt` wide (kept within the allowed range) in `color`.
    pub fn new(line: BorderLine, color: Option<Color>, width_pt: f32) -> BorderEdge {
        BorderEdge { line, color, width_pt: Self::clamp_width(width_pt) }
    }

    /// `w` within the allowed widths (non-finite → the default).
    pub fn clamp_width(w: f32) -> f32 {
        if w.is_finite() { w.clamp(Self::MIN_WIDTH_PT, Self::MAX_WIDTH_PT) } else { Self::DEFAULT_WIDTH_PT }
    }

    /// The line width in px.
    pub fn width_px(&self) -> f32 {
        Self::clamp_width(self.width_pt) * 4.0 / 3.0
    }

    /// The room the edge takes in px (CSS `border-width`): a double line has room for both lines and the
    /// gap between them.
    pub fn thickness_px(&self) -> f32 {
        let w = self.width_px();
        match self.line {
            BorderLine::Double => w * 3.0,
            _ => w,
        }
    }

    /// The edge drawn by a CSS border `thickness` px wide (the inverse of [`BorderEdge::thickness_px`]).
    pub fn from_thickness(line: BorderLine, color: Option<Color>, thickness_px: f32) -> BorderEdge {
        let w = match line {
            BorderLine::Double => thickness_px / 3.0,
            _ => thickness_px,
        };
        // Points, rounded to the quarter point the dialog works in.
        let pt = (w * 0.75 * 4.0).round() / 4.0;
        BorderEdge::new(line, color, pt)
    }
}

/// Paragraph borders: each side may have its own line. `shadow` draws a shadow below and to the right of
/// a box (the Shadow setting).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParaBorders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<BorderEdge>,
    /// The line between paragraphs sharing one box (Inside Horizontal Border on paragraphs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub between: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub shadow: bool,
}

impl ParaBorders {
    /// Space between a border line and the paragraph text, above / below and left / right (px). The HTML
    /// writer puts it in the paragraph's `padding`; the importer takes it back out.
    pub const PAD_Y_PX: f32 = 1.0;
    pub const PAD_X_PX: f32 = 5.0;

    /// A box: every side drawn with `edge`.
    pub fn boxed(edge: &BorderEdge) -> ParaBorders {
        ParaBorders {
            top: Some(edge.clone()),
            bottom: Some(edge.clone()),
            left: Some(edge.clone()),
            right: Some(edge.clone()),
            between: None,
            shadow: false,
        }
    }

    /// No line drawn.
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
            && self.between.is_none()
    }

    /// Side by name (`top`, `bottom`, `left`, `right`, `between`); `None` for an unknown name.
    pub fn side_mut(&mut self, name: &str) -> Option<&mut Option<BorderEdge>> {
        match name {
            "top" => Some(&mut self.top),
            "bottom" => Some(&mut self.bottom),
            "left" => Some(&mut self.left),
            "right" => Some(&mut self.right),
            "between" | "insideH" => Some(&mut self.between),
            _ => None,
        }
    }
}

/// Table borders: the outside edges and the lines between rows (`inside_h`) and columns (`inside_v`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TableBorders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside_h: Option<BorderEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside_v: Option<BorderEdge>,
}

impl TableBorders {
    /// Every edge (a full grid) drawn with `edge`.
    pub fn all(edge: &BorderEdge) -> TableBorders {
        TableBorders {
            top: Some(edge.clone()),
            bottom: Some(edge.clone()),
            left: Some(edge.clone()),
            right: Some(edge.clone()),
            inside_h: Some(edge.clone()),
            inside_v: Some(edge.clone()),
        }
    }

    /// No edge drawn.
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
            && self.inside_h.is_none()
            && self.inside_v.is_none()
    }

    /// Edge by name (`top`, `bottom`, `left`, `right`, `insideH`, `insideV`); `None` for an unknown name.
    pub fn edge_mut(&mut self, name: &str) -> Option<&mut Option<BorderEdge>> {
        match name {
            "top" => Some(&mut self.top),
            "bottom" => Some(&mut self.bottom),
            "left" => Some(&mut self.left),
            "right" => Some(&mut self.right),
            "insideH" => Some(&mut self.inside_h),
            "insideV" => Some(&mut self.inside_v),
            _ => None,
        }
    }
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
        /// ScreenTip: the text shown when pointing at the link (HTML `title`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    LineBreak,
    /// Image by URL (`cid:…`, `data:…`, `https:…`); remote URLs are blocked by default at display time.
    Image {
        src: String,
        alt: String,
        width: Option<f32>,
        height: Option<f32>,
    },
    /// A named place in the text (a bookmark), the target of `#name` links: written as
    /// `<a id="name" name="name"></a>`. It takes no room and no caret position.
    Anchor {
        name: String,
    },
}

/// Longest anchor (bookmark) name kept.
pub const MAX_ANCHOR_NAME: usize = 100;

/// An anchor name as kept: without control characters or white space, at most [`MAX_ANCHOR_NAME`] chars;
/// `None` when nothing is left.
pub fn clean_anchor_name(name: &str) -> Option<String> {
    let n: String =
        name.chars().filter(|c| !c.is_control() && !c.is_whitespace()).take(MAX_ANCHOR_NAME).collect();
    (!n.is_empty()).then_some(n)
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
        /// A plain grid border (every edge a thin grey line); `borders` wins when set.
        border: bool,
        width_px: Option<f32>,
        /// Per-edge borders (Borders and Shading, the Borders menu in a table).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        borders: Option<Box<TableBorders>>,
    },
    /// Horizontal rule (the HTML reply separator).
    Rule,
    /// Generic container with a background/border (HTML `div`/`td` boxes in newsletters).
    Container {
        style: ParaStyle,
        blocks: Vec<Block>,
    },
}

/// What a floating drawing object is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DrawingKind {
    /// A shape from the Shapes gallery (its geometry is [`Drawing::shape`]); it can hold text.
    #[default]
    Shape,
    /// A text box: a rectangle meant for text.
    TextBox,
    /// Decorative text drawn in a WordArt style ([`Drawing::word_art`]).
    WordArt,
    /// A drawing canvas: a frame that keeps the drawings placed inside it together.
    Canvas,
    /// A group of drawings ([`Drawing::group`] of each member names it): it moves, sizes and is deleted as
    /// one object. Its box is the box around its members; it draws nothing itself.
    Group,
}

/// Prefix of the `src` of a picture in the text that holds the place of an In Line with Text drawing
/// (`drawing:<id>`); such pictures are drawn by the compose editor, never fetched.
pub const INLINE_DRAWING_PREFIX: &str = "drawing:";

/// The drawing id an In Line with Text placeholder `src` names.
pub fn inline_drawing_id(src: &str) -> Option<u32> {
    src.strip_prefix(INLINE_DRAWING_PREFIX)?.parse().ok()
}

/// How the body text flows around a floating drawing (Wrap Text). Behind Text is
/// [`Drawing::behind_text`] with the default [`WrapStyle::Front`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WrapStyle {
    /// In front of the text (or behind it): the text does not make room.
    #[default]
    Front,
    /// The text wraps around the drawing's box.
    Square,
    /// The text wraps close to the drawing's outline ([`Drawing::wrap_points`]).
    Tight,
    /// Like Tight, also into open areas of the outline.
    Through,
    /// The text stops above the drawing and goes on below it.
    TopBottom,
    /// The drawing sits in a line of text like a character: a `drawing:<id>` picture in the text holds its
    /// place.
    Inline,
}

impl WrapStyle {
    /// Whether the body text makes room for a drawing with this wrapping (Square, Tight, Through, Top and
    /// Bottom).
    pub fn wraps(self) -> bool {
        matches!(self, WrapStyle::Square | WrapStyle::Tight | WrapStyle::Through | WrapStyle::TopBottom)
    }
}

/// Kind of a drawing's shadow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShadowKind {
    #[default]
    Outer,
    Inner,
    /// Cast on the ground behind the drawing (squashed and slanted).
    Perspective,
}

/// A shadow effect (Shape Effects › Shadow, Shadow Options).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shadow {
    pub kind: ShadowKind,
    /// `None` = black.
    pub color: Option<Color>,
    /// 0 (opaque) to 1 (invisible).
    pub transparency: f32,
    /// Blur radius (px).
    pub blur: f32,
    /// Direction the shadow falls, degrees clockwise from the right (45 = down and to the right).
    pub angle: f32,
    /// How far it falls (px).
    pub distance: f32,
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow {
            kind: ShadowKind::Outer,
            color: None,
            transparency: 0.6,
            blur: 4.0,
            angle: 45.0,
            distance: 3.0,
        }
    }
}

/// A reflection below the drawing (Reflection Options).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Reflection {
    /// 0 (opaque) to 1 (invisible) where the reflection starts.
    pub transparency: f32,
    /// How much of the drawing is reflected (0..=1 of its height).
    pub size: f32,
    /// Gap between the drawing and its reflection (px).
    pub distance: f32,
    /// Blur (px).
    pub blur: f32,
}

impl Default for Reflection {
    fn default() -> Self {
        Reflection { transparency: 0.5, size: 0.5, distance: 0.0, blur: 0.5 }
    }
}

/// A glow around the drawing (Glow Options).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Glow {
    /// `None` = a light tint of the drawing's own colour.
    pub color: Option<Color>,
    /// Radius (px).
    pub size: f32,
    /// 0 (opaque) to 1 (invisible).
    pub transparency: f32,
}

impl Default for Glow {
    fn default() -> Self {
        Glow { color: None, size: 8.0, transparency: 0.4 }
    }
}

/// Shape Effects (or Text Effects) with their options. Any combination may be on at once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShapeEffects {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection: Option<Reflection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glow: Option<Glow>,
    /// Soft edges: the drawing fades out over this many px at its edge (0 = none).
    #[serde(skip_serializing_if = "is_zero")]
    pub soft_edges: f32,
    /// Bevel: `""` (none), `circle`, `relaxedInset`, `cross`, `coolSlant`, `angle`, `softRound`, `convex`,
    /// `slope`, `divot`, `riblet`, `hardEdge`, `artDeco`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bevel: String,
    /// Bevel width (px; 0 = the bevel's own).
    #[serde(skip_serializing_if = "is_zero")]
    pub bevel_size: f32,
    /// 3-D Rotation about the vertical axis (degrees).
    #[serde(skip_serializing_if = "is_zero")]
    pub rot_x: f32,
    /// 3-D Rotation about the horizontal axis (degrees).
    #[serde(skip_serializing_if = "is_zero")]
    pub rot_y: f32,
    /// Perspective (degrees of field of view; 0 = parallel, no perspective).
    #[serde(skip_serializing_if = "is_zero")]
    pub perspective: f32,
}

impl ShapeEffects {
    /// Whether no effect is on.
    pub fn is_empty(&self) -> bool {
        *self == ShapeEffects::default()
    }
}

/// A gradient stop: where along the gradient (0..=1) and its colour (alpha = opacity).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub pos: f32,
    pub color: Color,
}

/// A gradient fill set up stop by stop (Format Shape › Fill › Gradient fill, More Gradients).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Gradient {
    /// From the middle outwards instead of along a line.
    #[serde(skip_serializing_if = "is_false")]
    pub radial: bool,
    /// Direction of a linear gradient, degrees clockwise from left-to-right (90 = top to bottom).
    #[serde(skip_serializing_if = "is_zero")]
    pub angle: f32,
    pub stops: Vec<GradientStop>,
}

/// Vertical placement of the text inside a drawing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// A floating drawing object of the compose body (text box, shape, WordArt, drawing canvas). It sits over the
/// text ("in front of text", or behind it) at its own position instead of flowing with the paragraphs.
/// Positions and sizes are px at 100 % zoom, from the top-left corner of the body's text area.
///
/// Mail has no portable way to send floating objects, so the compose window turns each group of drawings into
/// a picture placed in the text when the message is sent; while composing they stay editable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Drawing {
    /// Unique within the document (selection, commands).
    pub id: u32,
    pub kind: DrawingKind,
    /// Geometry name (`rectangle`, `oval`, `rightArrow`, `scribble`, …); text boxes use `rectangle`.
    pub shape: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Clockwise rotation in degrees.
    #[serde(skip_serializing_if = "is_zero")]
    pub rotation: f32,
    /// Fill colour (`None` = no fill).
    pub fill: Option<Color>,
    /// Outline colour (`None` = no outline).
    pub outline: Option<Color>,
    /// Outline weight in px.
    pub outline_px: f32,
    /// Shape effect: `""` (the theme's effect style), `none`, `shadow`, `glow`, `reflection`, `bevel`,
    /// `softEdges`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub effect: String,
    /// The text inside (lines separated by `\n`).
    pub text: String,
    /// Formatting of the text (colour, font, size, bold, italic, underline).
    pub text_style: TextStyle,
    pub align: Align,
    pub valign: VAlign,
    /// WordArt style ("Fill: Orange, Accent color 2; Outline: …"; WordArt drawings only).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub word_art: String,
    /// Alternative text for people who use screen readers (and the sent picture's alt text).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub alt: String,
    /// Freeform / scribble outline, as fractions (0..=1) of the box.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<(f32, f32)>,
    /// Drawn behind the body text instead of in front of it.
    #[serde(skip_serializing_if = "is_false")]
    pub behind_text: bool,
    /// The drawing canvas this drawing was drawn in (or moved into): it moves and is deleted with that
    /// canvas. A canvas drawn over drawings that already exist does not take them in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas: Option<u32>,
    /// Gradient fill: `""` (solid), `down`, `up`, `right`, `left` (from a light tint of the fill to the fill,
    /// in that direction) or `center` (light in the middle).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub gradient: String,
    /// The text runs top to bottom (turned 90° clockwise) instead of left to right.
    #[serde(skip_serializing_if = "is_false")]
    pub vertical: bool,
    /// Outline drawn around the letters of the text (WordArt: instead of its style's outline).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_outline: Option<Color>,
    /// Effect on the letters of the text: `""` (WordArt: its style's), `none`, `shadow`, `glow`,
    /// `reflection` (WordArt only), `bevel`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text_effect: String,
    /// How the body text flows around the drawing.
    #[serde(skip_serializing_if = "is_front")]
    pub wrap: WrapStyle,
    /// Outline the text wraps to with Tight and Through wrapping, as fractions (0..=1) of the box (empty:
    /// the box).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wrap_points: Vec<(f32, f32)>,
    /// The group drawing ([`DrawingKind::Group`]) this drawing belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    /// The text box the text that does not fit in this one flows on into (Create Text Box Link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_next: Option<u32>,
    /// Name in the Selection pane (empty: made from its kind and text).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Mirrored left to right.
    #[serde(skip_serializing_if = "is_false")]
    pub flip_h: bool,
    /// Mirrored top to bottom.
    #[serde(skip_serializing_if = "is_false")]
    pub flip_v: bool,
    /// Fill transparency, 0 (opaque) to 1 (clear).
    #[serde(skip_serializing_if = "is_zero")]
    pub transparency: f32,
    /// Outline transparency, 0 (opaque) to 1 (clear).
    #[serde(skip_serializing_if = "is_zero")]
    pub line_transparency: f32,
    /// Outline dashes: `""` (solid), `roundDot`, `squareDot`, `dash`, `dashDot`, `longDash`, `longDashDot`,
    /// `longDashDotDot`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub line_dash: String,
    /// A gradient fill set up stop by stop; wins over [`Drawing::gradient`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gradient_fill: Option<Gradient>,
    /// Picture fill: a PNG (base64) stretched over the shape; wins over the fill colour.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fill_picture: String,
    /// Text box margins (left, top, right, bottom px); `None` = the default margins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inset: Option<[f32; 4]>,
    /// Resize Shape to Fit Text: the drawing grows or shrinks in height to its text.
    #[serde(skip_serializing_if = "is_false")]
    pub autofit: bool,
    /// Other text directions: `up` (turned 270°, bottom to top) or `stacked` (letters one under the other).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text_flow: String,
    /// Shape Effects with options (shadow, reflection, glow, soft edges, bevel, 3-D rotation); wins over
    /// [`Drawing::effect`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fx: Option<Box<ShapeEffects>>,
    /// Text Effects with options; wins over [`Drawing::text_effect`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_fx: Option<Box<ShapeEffects>>,
    /// Text Effects › Transform: the warp of the text (`""` none; see the engine's list of warps).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub warp: String,
    /// Weight of the text outline (px; 0 = thin).
    #[serde(skip_serializing_if = "is_zero")]
    pub text_outline_px: f32,
    /// Dashes of the text outline (as [`Drawing::line_dash`]).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text_outline_dash: String,
}

fn is_front(w: &WrapStyle) -> bool {
    *w == WrapStyle::Front
}

impl Default for Drawing {
    fn default() -> Self {
        Drawing {
            id: 0,
            kind: DrawingKind::Shape,
            shape: "rectangle".into(),
            x: 0.0,
            y: 0.0,
            w: 160.0,
            h: 100.0,
            rotation: 0.0,
            fill: None,
            outline: None,
            outline_px: 1.0,
            effect: String::new(),
            text: String::new(),
            text_style: TextStyle::default(),
            align: Align::Start,
            valign: VAlign::Top,
            word_art: String::new(),
            alt: String::new(),
            points: Vec::new(),
            behind_text: false,
            canvas: None,
            gradient: String::new(),
            vertical: false,
            text_outline: None,
            text_effect: String::new(),
            wrap: WrapStyle::Front,
            wrap_points: Vec::new(),
            group: None,
            link_next: None,
            name: String::new(),
            flip_h: false,
            flip_v: false,
            transparency: 0.0,
            line_transparency: 0.0,
            line_dash: String::new(),
            gradient_fill: None,
            fill_picture: String::new(),
            inset: None,
            autofit: false,
            text_flow: String::new(),
            fx: None,
            text_fx: None,
            warp: String::new(),
            text_outline_px: 0.0,
            text_outline_dash: String::new(),
        }
    }
}

impl Drawing {
    /// Whether the point (`px`, `py`) lies in the drawing's box (rotation ignored).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    /// The `src` of the picture in the text that holds an In Line with Text drawing's place.
    pub fn inline_src(&self) -> String {
        format!("{INLINE_DRAWING_PREFIX}{}", self.id)
    }

    /// Whether this drawing's box lies wholly inside `other`'s box.
    pub fn inside(&self, other: &Drawing) -> bool {
        self.x >= other.x
            && self.y >= other.y
            && self.x + self.w <= other.x + other.w
            && self.y + self.h <= other.y + other.h
    }
}

/// A rich-text document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// Document default text style (compose default: Aptos-like body font, 12 pt).
    pub base: TextStyle,
    pub background: Option<Color>,
    /// Floating drawing objects (compose bodies), back to front.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drawings: Vec<Drawing>,
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
        self.blocks.is_empty() && self.drawings.is_empty()
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

    /// Removes every anchor from the paragraphs and headings and returns where each was: (block path of
    /// the paragraph, char offset, name), in document order. Paths follow the layout convention (lists add
    /// `[item, block]`, quotes and containers `[child]`, tables `[row, cell, block]`); offsets count chars
    /// with a line break or a picture as one.
    pub fn take_anchors(&mut self) -> Vec<(Vec<usize>, usize, String)> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        take_block_anchors(&mut self.blocks, &mut path, &mut out);
        out
    }

    /// Puts an anchor called `name` into the paragraph or heading at `path`, `offset` chars in (past the
    /// end = at the end). False when `path` is not a paragraph or heading or the name is empty.
    pub fn insert_anchor(&mut self, path: &[usize], offset: usize, name: &str) -> bool {
        let Some(name) = clean_anchor_name(name) else {
            return false;
        };
        let Some(inlines) = leaf_inlines_mut(&mut self.blocks, path, 0) else {
            return false;
        };
        insert_inline_at(inlines, offset, Inline::Anchor { name }, 0);
        true
    }
}

/// Inline content of the paragraph or heading at `path` (layout path convention).
fn leaf_inlines_mut<'a>(
    blocks: &'a mut [Block],
    path: &[usize],
    depth: usize,
) -> Option<&'a mut Vec<Inline>> {
    if depth > MAX_PLAIN_DEPTH {
        return None;
    }
    let (&first, rest) = path.split_first()?;
    match blocks.get_mut(first)? {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } if rest.is_empty() => Some(inlines),
        Block::List { items, .. } => {
            let (&item, rest) = rest.split_first()?;
            leaf_inlines_mut(items.get_mut(item)?, rest, depth + 1)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => {
            leaf_inlines_mut(blocks, rest, depth + 1)
        }
        Block::Table { rows, .. } => {
            let (&r, rest) = rest.split_first()?;
            let (&c, rest) = rest.split_first()?;
            leaf_inlines_mut(&mut rows.get_mut(r)?.get_mut(c)?.blocks, rest, depth + 1)
        }
        _ => None,
    }
}

/// Chars an inline counts for in offsets (line break and picture: one; anchor: none).
fn inline_len(i: &Inline, depth: usize) -> usize {
    match i {
        Inline::Text { text, .. } => text.chars().count(),
        Inline::Link { children, .. } if depth <= MAX_PLAIN_DEPTH => {
            children.iter().map(|c| inline_len(c, depth + 1)).fold(0usize, usize::saturating_add)
        }
        Inline::Link { .. } | Inline::Anchor { .. } => 0,
        Inline::LineBreak | Inline::Image { .. } => 1,
    }
}

/// Inserts `new` `offset` chars into `inlines`, splitting a text run or going into a link when the offset
/// falls inside one; an offset past the end appends it.
fn insert_inline_at(inlines: &mut Vec<Inline>, offset: usize, new: Inline, depth: usize) {
    let mut left = offset;
    let mut i = 0usize;
    while i < inlines.len() {
        if left == 0 {
            inlines.insert(i, new);
            return;
        }
        let Some(cur) = inlines.get_mut(i) else { break };
        let len = inline_len(cur, depth);
        if left < len {
            match cur {
                Inline::Text { text, style } => {
                    let at = text.char_indices().nth(left).map_or(text.len(), |(b, _)| b);
                    let tail = text.split_off(at);
                    let style = style.clone();
                    inlines.insert(i.saturating_add(1), new);
                    inlines.insert(i.saturating_add(2), Inline::Text { text: tail, style });
                    return;
                }
                Inline::Link { children, .. } if depth < MAX_PLAIN_DEPTH => {
                    insert_inline_at(children, left, new, depth + 1);
                    return;
                }
                _ => {}
            }
        }
        left = left.saturating_sub(len);
        i = i.saturating_add(1);
    }
    inlines.push(new);
}

fn take_block_anchors(
    blocks: &mut [Block],
    path: &mut Vec<usize>,
    out: &mut Vec<(Vec<usize>, usize, String)>,
) {
    if path.len() > MAX_PLAIN_DEPTH {
        return;
    }
    for (i, b) in blocks.iter_mut().enumerate() {
        path.push(i);
        match b {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
                let mut offset = 0usize;
                take_inline_anchors(inlines, &mut offset, path, out, 0);
            }
            Block::List { items, .. } => {
                for (k, item) in items.iter_mut().enumerate() {
                    path.push(k);
                    take_block_anchors(item, path, out);
                    path.pop();
                }
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => {
                take_block_anchors(blocks, path, out)
            }
            Block::Table { rows, .. } => {
                for (r, row) in rows.iter_mut().enumerate() {
                    for (c, cell) in row.iter_mut().enumerate() {
                        path.push(r);
                        path.push(c);
                        take_block_anchors(&mut cell.blocks, path, out);
                        path.pop();
                        path.pop();
                    }
                }
            }
            Block::Pre { .. } | Block::Rule => {}
        }
        path.pop();
    }
}

fn take_inline_anchors(
    inlines: &mut Vec<Inline>,
    offset: &mut usize,
    path: &[usize],
    out: &mut Vec<(Vec<usize>, usize, String)>,
    depth: usize,
) {
    if depth > MAX_PLAIN_DEPTH {
        return;
    }
    let mut i = 0usize;
    while i < inlines.len() {
        match inlines.get_mut(i) {
            Some(Inline::Anchor { name }) => {
                out.push((path.to_vec(), *offset, std::mem::take(name)));
                inlines.remove(i);
                continue;
            }
            Some(Inline::Link { children, .. }) => {
                take_inline_anchors(children, offset, path, out, depth + 1)
            }
            Some(other) => *offset = offset.saturating_add(inline_len(other, depth)),
            None => break,
        }
        i = i.saturating_add(1);
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

    /// The borders of a paragraph or heading (`None` without any, and for other blocks).
    pub fn para_borders(&self) -> Option<&ParaBorders> {
        match self {
            Block::Paragraph { style, .. } | Block::Heading { style, .. } => {
                style.borders.as_deref().filter(|b| !b.is_empty())
            }
            _ => None,
        }
    }

    /// Whether this paragraph and `next` (the block after it) share one border box: both have the same
    /// borders and the same indents, so no line is drawn between them.
    pub fn shares_border_box(&self, next: &Block) -> bool {
        let indents = |b: &Block| match b {
            Block::Paragraph { style, .. } | Block::Heading { style, .. } => {
                Some((style.indent_px.to_bits(), style.right_indent_px.to_bits()))
            }
            _ => None,
        };
        self.para_borders().is_some()
            && self.para_borders() == next.para_borders()
            && indents(self) == indents(next)
    }
}

/// Where a paragraph's border box joins the paragraphs around it (see [`Block::shares_border_box`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BorderJoin {
    pub above: bool,
    pub below: bool,
}

impl BorderJoin {
    /// How block `i` of `blocks` joins its neighbours.
    pub fn of(blocks: &[Block], i: usize) -> BorderJoin {
        let Some(me) = blocks.get(i) else {
            return BorderJoin::default();
        };
        BorderJoin {
            above: i.checked_sub(1).and_then(|j| blocks.get(j)).is_some_and(|p| p.shares_border_box(me)),
            below: blocks.get(i.saturating_add(1)).is_some_and(|n| me.shares_border_box(n)),
        }
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
            Inline::Link { href, children, .. } => {
                let before = out.len();
                inlines_to_text(children, depth + 1, out);
                if out.len() == before {
                    out.push_str(href);
                }
            }
            Inline::LineBreak => out.push('\n'),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Anchor { .. } => {}
        }
    }
}
