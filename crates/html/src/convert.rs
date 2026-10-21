//! DOM → [`Doc`]: element semantics, the CSS cascade subset, whitespace collapsing and sanitising.
//!
//! Block structure is built with a [`Sink`] per block container: inline content accumulates into a pending
//! paragraph that is flushed whenever a nested block starts or the container ends. Whitespace follows CSS
//! `white-space: normal` (runs collapse to one space, leading/trailing spaces of a line vanish) unless a
//! `pre`-family element or `white-space: pre*` is in effect.

use crate::css::{self, CascadeKey, DEFAULT_PT, Length, Props, Stylesheet};
use crate::dom::{self, Dom, Node, NodeData, NodeId, attr};
use crate::sanitize;
use mailcraft_geom::Color;
use mailcraft_model::rich::{
    Align, Block, Doc, Inline, LineRule, ListKind, ParaStyle, TableCell, TextStyle, UnderlineKind,
};
use mailcraft_model::{BorderEdge, ParaBorders, TableBorders};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const MAX_TABLE_ROWS: usize = 10_000;
const MAX_TABLE_COLS: usize = 1_000;
/// Recursion guard (the tree itself is already capped at `MAX_DEPTH`).
const MAX_WALK_DEPTH: usize = crate::MAX_DEPTH + 64;
/// Largest text indent and fixed line height taken from mail (px).
const MAX_TEXT_INDENT: f32 = 2_000.0;
/// Largest letter spacing and baseline shift taken from mail (pt).
const MAX_SHIFT_PT: f32 = 200.0;
/// Longest link ScreenTip (`title`) kept, in chars.
const MAX_LINK_TITLE: usize = 1_000;
/// Inline `style` attributes longer than this are truncated.
const MAX_STYLE_ATTR: usize = 64 * 1024;
/// Total bytes of link targets copied into `Inline::Link`s. A link spanning many paragraphs needs one copy per
/// paragraph; past this budget further paragraphs keep their text unlinked.
const LINK_BUDGET: usize = crate::MAX_INPUT_BYTES;
/// Total stylesheet work (candidate rules plus declarations applied) per document; see
/// [`Stylesheet::cascade`]. Real mail needs a tiny fraction of this.
const CASCADE_BUDGET: usize = 20_000_000;
/// Distinct (tag, id, classes) combinations whose cascade result is memoised.
const MAX_CASCADE_CACHE: usize = 4096;

/// Inherited state while walking the tree.
#[derive(Clone, Debug, Default)]
struct Ctx {
    text: TextStyle,
    align: Align,
    indent: f32,
    line_height: Option<f32>,
    link: Option<Rc<str>>,
    /// ScreenTip of the enclosing link (`title`).
    link_title: Option<Rc<str>>,
    preserve_ws: bool,
}

impl Ctx {
    fn para(&self) -> ParaStyle {
        ParaStyle {
            align: self.align,
            indent_px: self.indent,
            line_height: self.line_height,
            ..ParaStyle::default()
        }
    }
}

/// Box-level properties of one element.
#[derive(Clone, Debug, Default)]
struct BoxStyle {
    align: Option<Align>,
    margin_top: Option<f32>,
    margin_bottom: Option<f32>,
    /// margin-left + padding-left.
    indent: f32,
    line_height: Option<Option<f32>>,
    background: Option<Color>,
    width: Option<f32>,
    height: Option<f32>,
    hidden: bool,
    top_rule: bool,
    border: bool,
    preserve_ws: Option<bool>,
    /// margin-right + padding-right.
    right: f32,
    /// text-indent (negative = hanging).
    text_indent: f32,
    /// An absolute line-height with `mso-line-height-rule` (exactly / at-least), as Word-made mail has it.
    line_px: Option<(f32, LineRule)>,
    keep_with_next: bool,
    keep_together: bool,
    page_break_before: bool,
    no_widow_control: bool,
    /// Border edges in CSS box order (top, right, bottom, left).
    edges: [Option<BorderEdge>; 4],
    /// Lines a shared paragraph box leaves out (`mso-border-top-alt`, `mso-border-bottom-alt`).
    alt_top: Option<Option<BorderEdge>>,
    alt_bottom: Option<Option<BorderEdge>>,
    between: Option<BorderEdge>,
    shadow: bool,
    /// Padding in CSS box order, and the left / right margins (a bordered paragraph's padding is the room
    /// between its lines and its text, not indent).
    pad: [f32; 4],
    margin_left: f32,
    margin_right: f32,
}

/// A bordered paragraph's borders and spacing with the border padding taken back out.
struct BorderedPara {
    borders: ParaBorders,
    /// Space added above / below (padding past the border padding: the space a shared box moves inside).
    before: f32,
    after: f32,
    indent: f32,
    right: f32,
}

impl BoxStyle {
    /// The borders of a `p` or heading, if it has any.
    fn bordered_para(&self) -> Option<BorderedPara> {
        let [top, right, bottom, left] = self.edges.clone();
        let drawn_top = top.is_some() && self.alt_top.is_none();
        let drawn_bottom = bottom.is_some() && self.alt_bottom.is_none();
        // A shared box keeps the lines it leaves out (and the bottom it draws a between line over) in
        // `mso-border-…-alt`.
        let borders = ParaBorders {
            top: self.alt_top.clone().unwrap_or(top),
            bottom: self.alt_bottom.clone().unwrap_or(bottom),
            left,
            right,
            between: self.between.clone(),
            shadow: self.shadow,
        };
        if borders.is_empty() {
            return None;
        }
        let less = |pad: f32, on: bool, by: f32| (pad - if on { by } else { 0.0 }).max(0.0);
        let [pt, pr, pb, pl] = self.pad;
        Some(BorderedPara {
            before: less(pt, drawn_top, ParaBorders::PAD_Y_PX),
            after: less(pb, drawn_bottom, ParaBorders::PAD_Y_PX),
            indent: (self.margin_left + less(pl, borders.left.is_some(), ParaBorders::PAD_X_PX)).max(0.0),
            right: (self.margin_right + less(pr, borders.right.is_some(), ParaBorders::PAD_X_PX)).max(0.0),
            borders,
        })
    }

    /// The paragraph properties of this box on top of `para`.
    fn para_extras(&self, para: ParaStyle) -> ParaStyle {
        ParaStyle {
            right_indent_px: self.right.max(0.0),
            first_line_px: self.text_indent,
            line_px: self.line_px,
            keep_with_next: self.keep_with_next,
            keep_together: self.keep_together,
            page_break_before: self.page_break_before,
            no_widow_control: self.no_widow_control,
            ..para
        }
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "center"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "legend"
            | "details"
            | "summary"
            | "dl"
            | "dt"
            | "dd"
            | "hgroup"
            | "li"
            | "noscript"
            | "dialog"
            | "search"
            | "tr"
            | "td"
            | "th"
    )
}

fn is_inline(name: &str) -> bool {
    !is_block(name)
        && !dom::is_heading(name)
        && !matches!(
            name,
            "ul" | "ol"
                | "menu"
                | "dir"
                | "blockquote"
                | "pre"
                | "listing"
                | "xmp"
                | "plaintext"
                | "table"
                | "hr"
        )
}

const BORDER_STYLES: &[&str] = &["solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset"];

fn has_border_style(v: &str) -> bool {
    let v = v.to_ascii_lowercase();
    v.split_ascii_whitespace().any(|t| BORDER_STYLES.contains(&t))
        && !v.split_ascii_whitespace().any(|t| t == "none" || t == "hidden" || t == "0" || t == "0px")
}

fn parse_align(v: &str) -> Option<Align> {
    match v.trim().to_ascii_lowercase().as_str() {
        "left" | "start" | "-webkit-left" => Some(Align::Start),
        "right" | "end" | "-webkit-right" => Some(Align::End),
        "center" | "middle" | "-webkit-center" | "-moz-center" => Some(Align::Center),
        "justify" => Some(Align::Justify),
        _ => None,
    }
}

/// HTML "non-negative integer" parsing: leading whitespace, then digits (anything after is ignored).
fn parse_uint(s: &str) -> Option<u32> {
    let s = s.trim_start();
    let digits: String = s.chars().take_while(char::is_ascii_digit).take(10).collect();
    digits.parse::<u64>().ok().map(|v| u32::try_from(v.min(u64::from(u32::MAX))).unwrap_or(u32::MAX))
}

/// `width="600"` → `600px`, `width="100%"` kept.
fn attr_dimension(v: &str) -> Option<String> {
    let v = v.trim();
    if v.ends_with('%') {
        return Some(v.to_owned());
    }
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').take(12).collect();
    num.parse::<f32>().ok().filter(|n| n.is_finite()).map(|n| format!("{n}px"))
}

fn normalize_ws(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn opaque(c: Color) -> Option<Color> {
    (c.a > 0).then_some(c)
}

/// Line height as a multiplier of the font size.
fn parse_line_height(v: &str, font_px: f32) -> Option<Option<f32>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("normal") {
        return Some(None);
    }
    if let Ok(n) = v.parse::<f32>() {
        return n.is_finite().then_some(Some(n.clamp(0.0, 100.0)));
    }
    let m = match css::parse_length(v, font_px)? {
        Length::Px(px) if font_px > 0.0 => px / font_px,
        Length::Px(_) => return None,
        Length::Percent(p) => p / 100.0,
    };
    m.is_finite().then_some(Some(m.clamp(0.0, 100.0)))
}

/// Four-value box shorthand (`margin`, `padding`) → (top, right, bottom, left).
fn box_sides(v: &str, font_px: f32) -> Option<[f32; 4]> {
    let vals: Vec<f32> = v.split_ascii_whitespace().filter_map(|t| css::length_px(t, font_px)).collect();
    match vals.as_slice() {
        [a] => Some([*a, *a, *a, *a]),
        [a, b] => Some([*a, *b, *a, *b]),
        [a, b, c] => Some([*a, *b, *c, *b]),
        [a, b, c, d, ..] => Some([*a, *b, *c, *d]),
        [] => None,
    }
}

fn box_style(p: &Props, font_px: f32) -> BoxStyle {
    let mut b = BoxStyle { align: p.get("text-align").and_then(parse_align), ..BoxStyle::default() };
    let len = |prop: &str| p.get(prop).and_then(|v| css::length_px(v, font_px));
    let (mut mt, mut mb, mut ml, mut pl) = (None, None, None, None);
    if let Some([t, _, bt, l]) = p.get("margin").and_then(|v| box_sides(v, font_px)) {
        (mt, mb, ml) = (Some(t), Some(bt), Some(l));
    }
    let (mut pt, mut pb) = (None, None);
    if let Some([t, _, bt, l]) = p.get("padding").and_then(|v| box_sides(v, font_px)) {
        (pt, pb, pl) = (Some(t), Some(bt), Some(l));
    }
    pt = len("padding-top").or(pt);
    pb = len("padding-bottom").or(pb);
    mt = len("margin-top").or(mt);
    mb = len("margin-bottom").or(mb);
    ml = len("margin-left").or(ml);
    pl = len("padding-left").or(pl);
    b.margin_top = mt;
    b.margin_bottom = mb;
    b.indent = ml.unwrap_or(0.0) + pl.unwrap_or(0.0);
    b.line_height = p.get("line-height").and_then(|v| parse_line_height(v, font_px));
    let (mut mr, mut pr) = (None, None);
    if let Some([_, r, _, _]) = p.get("margin").and_then(|v| box_sides(v, font_px)) {
        mr = Some(r);
    }
    if let Some([_, r, _, _]) = p.get("padding").and_then(|v| box_sides(v, font_px)) {
        pr = Some(r);
    }
    b.right =
        (len("margin-right").or(mr).unwrap_or(0.0) + len("padding-right").or(pr).unwrap_or(0.0)).max(0.0);
    let pos = |v: Option<f32>| v.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, MAX_TEXT_INDENT);
    b.pad = [pos(pt), pos(len("padding-right").or(pr)), pos(pb), pos(pl)];
    b.margin_left = ml.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-MAX_TEXT_INDENT, MAX_TEXT_INDENT);
    b.margin_right =
        len("margin-right").or(mr).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, MAX_TEXT_INDENT);
    b.edges = crate::borders::edges(p, font_px);
    b.alt_top = crate::borders::alt_edge(p, "top", font_px);
    b.alt_bottom = crate::borders::alt_edge(p, "bottom", font_px);
    b.between = crate::borders::between_edge(p, font_px);
    b.shadow = crate::borders::has_shadow(p);
    b.text_indent = p
        .get("text-indent")
        .and_then(|v| css::length_px(v, font_px))
        .filter(|v| v.is_finite())
        .map_or(0.0, |v| v.clamp(-MAX_TEXT_INDENT, MAX_TEXT_INDENT));
    let rule = p.get("mso-line-height-rule").map(|v| v.trim().to_ascii_lowercase());
    let rule = match rule.as_deref() {
        Some("exactly") => Some(LineRule::Exactly),
        Some("at-least") => Some(LineRule::AtLeast),
        _ => None,
    };
    if let (Some(rule), Some(Length::Px(px))) =
        (rule, p.get("line-height").and_then(|v| css::parse_length(v, font_px)))
        && px.is_finite()
        && px > 0.0
    {
        b.line_px = Some((px.min(MAX_TEXT_INDENT), rule));
    }
    let is = |props: &[&str], words: &[&str]| {
        props.iter().any(|k| {
            p.get(k).is_some_and(|v| {
                let v = v.trim().to_ascii_lowercase();
                words.iter().any(|w| v == *w)
            })
        })
    };
    b.keep_with_next = is(&["page-break-after", "break-after"], &["avoid", "avoid-page"]);
    b.keep_together = is(&["page-break-inside", "break-inside"], &["avoid", "avoid-page"]);
    b.page_break_before = is(&["page-break-before", "break-before"], &["always", "page"]);
    b.no_widow_control = is(&["widows"], &["1"]) && is(&["orphans"], &["1"]);
    b.background = p
        .get("background-color")
        .and_then(css::parse_color)
        .or_else(|| p.get("background").and_then(css::color_in_shorthand))
        .and_then(opaque);
    let dim = |prop: &str| match p.get(prop).and_then(|v| css::parse_length(v, font_px)) {
        Some(Length::Px(v)) if v >= 0.0 => Some(v),
        _ => None,
    };
    b.width = dim("width");
    b.height = dim("height");
    b.hidden = p.get("display").is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        || p.get("mso-hide").is_some_and(|v| v.trim().eq_ignore_ascii_case("all"));
    b.top_rule = p.get("border-top").is_some_and(has_border_style)
        || p.get("border-top-style").is_some_and(has_border_style);
    b.border = ["border", "border-style"].iter().any(|k| p.get(k).is_some_and(has_border_style));
    b.preserve_ws = p.get("white-space").and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
        "pre" | "pre-wrap" | "pre-line" | "break-spaces" => Some(true),
        "normal" | "nowrap" => Some(false),
        _ => None,
    });
    b
}

/// Semantics of formatting tags.
fn tag_text_style(name: &str, attrs: &[(String, String)], ts: &mut TextStyle, parent_pt: f32) {
    match name {
        "b" | "strong" | "th" => ts.bold = Some(true),
        "i" | "em" | "cite" | "var" | "dfn" => ts.italic = Some(true),
        "u" | "ins" => ts.underline = Some(true),
        "s" | "strike" | "del" => ts.strike = Some(true),
        "sup" => ts.superscript = Some(true),
        "sub" => ts.subscript = Some(true),
        "code" | "tt" | "kbd" | "samp" => ts.monospace = Some(true),
        "mark" => ts.highlight = Some(Color::rgb(255, 255, 0)),
        "big" => ts.size_pt = Some((parent_pt * 1.2).clamp(1.0, 400.0)),
        "small" => ts.size_pt = Some((parent_pt / 1.2).clamp(1.0, 400.0)),
        "font" => {
            if let Some(face) = attr(attrs, "face").and_then(css::clean_font_family) {
                ts.font_family = Some(face);
            }
            if let Some(size) = attr(attrs, "size").and_then(css::legacy_font_size_pt) {
                ts.size_pt = Some(size);
            }
            if let Some(color) = attr(attrs, "color").and_then(css::parse_legacy_color) {
                ts.color = Some(color);
            }
        }
        _ => {}
    }
}

fn apply_font_weight(ts: &mut TextStyle, v: &str) {
    match v.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => ts.bold = Some(true),
        "normal" | "lighter" => ts.bold = Some(false),
        n => {
            if let Ok(w) = n.parse::<f32>() {
                ts.bold = Some(w >= 600.0);
            }
        }
    }
}

fn apply_font_style(ts: &mut TextStyle, v: &str) {
    match v.trim().to_ascii_lowercase().as_str() {
        "italic" | "oblique" => ts.italic = Some(true),
        "normal" => ts.italic = Some(false),
        _ => {}
    }
}

/// The `font` shorthand: `[style] [variant] [weight] size[/line-height] family`.
fn apply_font_shorthand(ts: &mut TextStyle, v: &str, parent_pt: f32) {
    let tokens: Vec<&str> = v.split_ascii_whitespace().collect();
    let Some(size_idx) = tokens.iter().position(|t| {
        let size = t.split('/').next().unwrap_or("");
        css::font_size_pt(size, parent_pt).is_some()
    }) else {
        return;
    };
    for t in tokens.get(..size_idx).unwrap_or(&[]) {
        apply_font_style(ts, t);
        apply_font_weight(ts, t);
    }
    if let Some(size) =
        tokens.get(size_idx).and_then(|t| css::font_size_pt(t.split('/').next().unwrap_or(""), parent_pt))
    {
        ts.size_pt = Some(size);
    }
    if let Some(family) = css::clean_font_family(&tokens.get(size_idx + 1..).unwrap_or(&[]).join(" ")) {
        ts.font_family = Some(family);
    }
}

fn apply_text_props(ts: &mut TextStyle, p: &Props, tag: &str, parent_pt: f32, inline: bool) {
    if let Some(v) = p.get("font") {
        apply_font_shorthand(ts, v, parent_pt);
    }
    if let Some(v) = p.get("font-family").and_then(css::clean_font_family) {
        ts.font_family = Some(v);
    }
    if let Some(size) = p.get("font-size").and_then(|v| css::font_size_pt(v, parent_pt)) {
        ts.size_pt = Some(size);
    }
    if let Some(v) = p.get("font-weight") {
        apply_font_weight(ts, v);
    }
    if let Some(v) = p.get("font-style") {
        apply_font_style(ts, v);
    }
    // Which line this element itself draws: a decoration style belongs to that line, never to one
    // inherited from an ancestor (`<u><s style="text-decoration-style:double">` is a double strike).
    let (mut own_underline, mut own_strike) =
        (matches!(tag, "u" | "ins"), matches!(tag, "s" | "strike" | "del"));
    for key in ["text-decoration", "text-decoration-line"] {
        if let Some(v) = p.get(key) {
            let v = v.to_ascii_lowercase();
            if v.split_ascii_whitespace().any(|t| t == "none") {
                ts.underline = Some(false);
                ts.strike = Some(false);
            }
            if v.contains("underline") {
                ts.underline = Some(true);
                own_underline = true;
            }
            if v.contains("line-through") {
                ts.strike = Some(true);
                own_strike = true;
            }
        }
    }
    // Underline style and colour, double strikethrough: the longhands, or words of the shorthand.
    let deco = |k: &str| p.get(k).map(str::to_ascii_lowercase);
    let style_word = deco("text-decoration-style").or_else(|| deco("text-decoration")).and_then(|v| {
        v.split_ascii_whitespace().find_map(|t| UnderlineKind::from_name(t).filter(|_| t != "single"))
    });
    if let Some(kind) = style_word {
        let on_strike = if own_strike != own_underline {
            own_strike
        } else {
            // Neither (or both) declared here: the line that is on, the underline when both are.
            ts.strike == Some(true) && ts.underline != Some(true)
        };
        if on_strike {
            if kind == UnderlineKind::Double {
                ts.double_strike = Some(true);
                ts.strike = None;
            }
        } else {
            ts.underline_kind = Some(kind);
        }
    }
    let deco_color = p.get("text-decoration-color").and_then(css::parse_color).or_else(|| {
        p.get("text-decoration")
            .filter(|v| v.to_ascii_lowercase().contains("underline"))
            .and_then(css::color_in_shorthand)
    });
    if let Some(c) = deco_color.and_then(opaque) {
        ts.underline_color = Some(c);
    }
    match deco("font-variant").as_deref().map(str::trim) {
        Some("small-caps") => ts.small_caps = Some(true),
        Some("normal") => ts.small_caps = Some(false),
        _ => {}
    }
    match deco("text-transform").as_deref().map(str::trim) {
        Some("uppercase") => ts.all_caps = Some(true),
        Some("none") => ts.all_caps = Some(false),
        _ => {}
    }
    let font_px = ts.size_pt.unwrap_or(parent_pt) * 4.0 / 3.0;
    if let Some(v) = p.get("letter-spacing") {
        if v.trim().eq_ignore_ascii_case("normal") {
            ts.spacing_pt = None;
        } else if let Some(px) = css::length_px(v, font_px).filter(|v| v.is_finite()) {
            ts.spacing_pt = Some((px * 0.75).clamp(-MAX_SHIFT_PT, MAX_SHIFT_PT));
        }
    }
    if let Some(c) = p.get("color").and_then(css::parse_color) {
        ts.color = Some(c);
    }
    if inline {
        let bg = p
            .get("background-color")
            .and_then(css::parse_color)
            .or_else(|| p.get("background").and_then(css::color_in_shorthand));
        if let Some(bg) = bg {
            ts.highlight = opaque(bg);
        }
        match p.get("vertical-align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("super") => ts.superscript = Some(true),
            Some("sub") => ts.subscript = Some(true),
            Some("baseline") => {
                ts.superscript = Some(false);
                ts.subscript = Some(false);
            }
            // A length raises (or, negative, lowers) the text: Font › Advanced › Position.
            Some(v) => {
                if let Some(px) = css::length_px(v, font_px).filter(|v| v.is_finite() && *v != 0.0) {
                    ts.position_pt = Some((px * 0.75).clamp(-MAX_SHIFT_PT, MAX_SHIFT_PT));
                }
            }
            None => {}
        }
    }
}

/// Appends an inline, merging adjacent text runs with the same style.
fn push_merge(list: &mut Vec<Inline>, inline: Inline) {
    if let Inline::Text { text, style } = &inline
        && let Some(Inline::Text { text: last, style: last_style }) = list.last_mut()
        && last_style == style
    {
        last.push_str(text);
        return;
    }
    list.push(inline);
}

/// Removes trailing collapsible spaces (recursing into a trailing link).
fn trim_trailing(list: &mut Vec<Inline>) {
    loop {
        match list.last_mut() {
            Some(Inline::Text { text, .. }) => {
                let keep = text.trim_end_matches(' ').len();
                text.truncate(keep);
                if !text.is_empty() {
                    return;
                }
            }
            Some(Inline::Link { children, .. }) => {
                trim_trailing(children);
                if !children.is_empty() {
                    return;
                }
            }
            _ => return,
        }
        list.pop();
    }
}

/// Collects the blocks of one block container.
struct Sink<'a> {
    para: ParaStyle,
    blocks: Vec<Block>,
    inlines: Vec<Inline>,
    /// True at the start of a line or after a collapsible space.
    last_space: bool,
    /// Remaining [`LINK_BUDGET`], shared by every sink of the document.
    link_budget: &'a Cell<usize>,
}

impl<'a> Sink<'a> {
    fn new(para: ParaStyle, link_budget: &'a Cell<usize>) -> Self {
        Self { para, blocks: Vec::new(), inlines: Vec::new(), last_space: true, link_budget }
    }

    fn push_inline(&mut self, inline: Inline, ctx: &Ctx) {
        match &ctx.link {
            Some(href) => {
                let title = ctx.link_title.as_deref();
                if let Some(Inline::Link { href: h, children, title: t }) = self.inlines.last_mut()
                    && h.as_str() == &**href
                    && t.as_deref() == title
                {
                    push_merge(children, inline);
                    return;
                }
                let left = self.link_budget.get();
                let cost = href.len().saturating_add(title.map_or(0, str::len));
                if left >= cost {
                    self.link_budget.set(left - cost);
                    self.inlines.push(Inline::Link {
                        href: href.to_string(),
                        children: vec![inline],
                        title: title.map(str::to_owned),
                    });
                } else {
                    push_merge(&mut self.inlines, inline);
                }
            }
            None => push_merge(&mut self.inlines, inline),
        }
    }

    fn text(&mut self, raw: &str, ctx: &Ctx) {
        if ctx.preserve_ws {
            // The model has no white-space property, so preserved spaces that would otherwise collapse are
            // stored as no-break spaces (alternating, exactly as the serialiser writes them back).
            for (i, line) in raw.split('\n').enumerate() {
                if i > 0 {
                    self.line_break(ctx);
                }
                let mut out = String::with_capacity(line.len());
                for ch in line.chars() {
                    let spaces = match ch {
                        ' ' => 1,
                        '\t' => 4,
                        _ => 0,
                    };
                    if spaces == 0 {
                        out.push(ch);
                        self.last_space = false;
                        continue;
                    }
                    for _ in 0..spaces {
                        if self.last_space {
                            out.push('\u{A0}');
                            self.last_space = false;
                        } else {
                            out.push(' ');
                            self.last_space = true;
                        }
                    }
                }
                if !out.is_empty() {
                    self.push_inline(Inline::Text { text: out, style: ctx.text.clone() }, ctx);
                }
            }
            return;
        }
        let mut out = String::new();
        for ch in raw.chars() {
            if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                if !self.last_space {
                    out.push(' ');
                    self.last_space = true;
                }
            } else {
                out.push(ch);
                self.last_space = false;
            }
        }
        if !out.is_empty() {
            self.push_inline(Inline::Text { text: out, style: ctx.text.clone() }, ctx);
        }
    }

    fn line_break(&mut self, ctx: &Ctx) {
        trim_trailing(&mut self.inlines);
        self.push_inline(Inline::LineBreak, ctx);
        self.last_space = true;
    }

    fn flush(&mut self) {
        let mut inlines = std::mem::take(&mut self.inlines);
        self.last_space = true;
        trim_trailing(&mut inlines);
        if inlines.is_empty() {
            return;
        }
        // Bookmarks with no text of their own (`<a name="x"></a>` between blocks) go with the next
        // paragraph instead of making an empty one.
        if inlines.iter().all(|i| matches!(i, Inline::Anchor { .. })) {
            self.inlines = inlines;
            return;
        }
        if matches!(inlines.as_slice(), [Inline::LineBreak]) {
            inlines.clear();
        } else if matches!(inlines.last(), Some(Inline::LineBreak)) {
            inlines.pop();
        }
        self.blocks.push(Block::Paragraph { style: self.para.clone(), inlines });
    }

    fn push_block(&mut self, b: Block) {
        self.push_blocks(vec![b]);
    }

    fn push_blocks(&mut self, mut bs: Vec<Block>) {
        self.flush();
        // Bookmarks waiting for a paragraph go to the start of the first one pushed.
        if !self.inlines.is_empty()
            && let Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) = bs.first_mut()
        {
            let pending = std::mem::take(&mut self.inlines);
            inlines.splice(0..0, pending);
        }
        self.blocks.extend(bs);
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        // Bookmarks left at the end join the last paragraph.
        let pending = std::mem::take(&mut self.inlines);
        if !pending.is_empty()
            && let Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) =
                self.blocks.last_mut()
        {
            inlines.extend(pending);
        }
        self.blocks
    }
}

struct Conv<'a> {
    dom: &'a Dom,
    sheet: Stylesheet,
    base_pt: f32,
    /// Memoised stylesheet results (normal, important) per selector key.
    cascade_cache: RefCell<HashMap<CascadeKey, Rc<(Props, Props)>>>,
    cascade_budget: Cell<usize>,
    link_budget: Cell<usize>,
}

impl Conv<'_> {
    fn props(&self, name: &str, attrs: &[(String, String)]) -> Props {
        let mut p = Props::default();
        if let Some(c) = attr(attrs, "bgcolor").and_then(css::parse_legacy_color) {
            p.set("background-color", &c.to_hex());
        }
        if (matches!(name, "p" | "div" | "td" | "th" | "tr" | "center" | "legend") || dom::is_heading(name))
            && let Some(a) = attr(attrs, "align")
        {
            p.set("text-align", a);
        }
        if name == "body"
            && let Some(c) = attr(attrs, "text").and_then(css::parse_legacy_color)
        {
            p.set("color", &c.to_hex());
        }
        for dim in ["width", "height"] {
            if let Some(v) = attr(attrs, dim).and_then(attr_dimension) {
                p.set(dim, &v);
            }
        }
        if attr(attrs, "hidden").is_some() {
            p.set("display", "none");
        }
        let sheet = self.sheet_props(name, attrs);
        let (normal, important) = &*sheet;
        p.merge(normal);
        let inline = attr(attrs, "style")
            .map(|s| {
                let mut end = s.len().min(MAX_STYLE_ATTR);
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                css::parse_declarations(s.get(..end).unwrap_or(""))
            })
            .unwrap_or_default();
        let (inline_imp, inline_norm): (Vec<&css::Decl>, Vec<&css::Decl>) =
            inline.iter().partition(|d| d.important);
        p.apply(&inline_norm);
        p.merge(important);
        p.apply(&inline_imp);
        p
    }

    /// The stylesheet's (normal, important) properties for an element, memoised per selector key.
    fn sheet_props(&self, name: &str, attrs: &[(String, String)]) -> Rc<(Props, Props)> {
        if self.sheet.is_empty() {
            return Rc::default();
        }
        let key = Stylesheet::key(name, attr(attrs, "id"), attr(attrs, "class"));
        if let Some(hit) = self.cascade_cache.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let mut budget = self.cascade_budget.get();
        let result = Rc::new(self.sheet.cascade(&key, &mut budget));
        self.cascade_budget.set(budget);
        let mut cache = self.cascade_cache.borrow_mut();
        if cache.len() < MAX_CASCADE_CACHE {
            cache.insert(key, Rc::clone(&result));
        }
        result
    }

    /// Inherited context and box style for an element, or `None` if it is hidden.
    fn element_ctx(&self, name: &str, attrs: &[(String, String)], ctx: &Ctx) -> Option<(Ctx, BoxStyle)> {
        let parent_pt = ctx.text.size_pt.unwrap_or(self.base_pt);
        let props = self.props(name, attrs);
        let mut c = ctx.clone();
        tag_text_style(name, attrs, &mut c.text, parent_pt);
        apply_text_props(&mut c.text, &props, name, parent_pt, is_inline(name));
        let font_px = c.text.size_pt.unwrap_or(self.base_pt) * 4.0 / 3.0;
        let bx = box_style(&props, font_px);
        if bx.hidden {
            return None;
        }
        if let Some(p) = bx.preserve_ws {
            c.preserve_ws = p;
        }
        if !is_inline(name) {
            c.align = bx.align.unwrap_or(if name == "center" { Align::Center } else { c.align });
            if let Some(lh) = bx.line_height {
                c.line_height = lh;
            }
        }
        Some((c, bx))
    }

    fn font_px(&self, c: &Ctx) -> f32 {
        c.text.size_pt.unwrap_or(self.base_pt) * 4.0 / 3.0
    }

    fn walk_children(&self, id: NodeId, ctx: &Ctx, sink: &mut Sink, depth: usize) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        for &c in self.dom.children(id) {
            self.walk(c, ctx, sink, depth + 1);
        }
    }

    fn walk(&self, id: NodeId, ctx: &Ctx, sink: &mut Sink, depth: usize) {
        let dom = self.dom;
        let Some(node) = dom.node(id) else { return };
        match &node.data {
            NodeData::Root => self.walk_children(id, ctx, sink, depth),
            NodeData::Text(t) => sink.text(t, ctx),
            NodeData::Element { name, attrs } => self.element(id, name, attrs, ctx, sink, depth),
        }
    }

    /// Walks the children of `id` into a fresh sink with the given paragraph style.
    fn flow_with(&self, id: NodeId, ctx: &Ctx, para: ParaStyle, depth: usize) -> Vec<Block> {
        let mut sink = Sink::new(para, &self.link_budget);
        self.walk_children(id, ctx, &mut sink, depth);
        sink.finish()
    }

    fn element(
        &self,
        id: NodeId,
        name: &str,
        attrs: &[(String, String)],
        ctx: &Ctx,
        sink: &mut Sink,
        depth: usize,
    ) {
        if sanitize::is_dropped(name) {
            return;
        }
        let Some((mut c, bx)) = self.element_ctx(name, attrs, ctx) else { return };
        match name {
            "br" => sink.line_break(&c),
            "img" => {
                if let Some(src) = attr(attrs, "src").and_then(sanitize::safe_image_src) {
                    let alt = attr(attrs, "alt").map(normalize_ws).unwrap_or_default();
                    sink.push_inline(Inline::Image { src, alt, width: bx.width, height: bx.height }, &c);
                    sink.last_space = false;
                }
            }
            "a" => {
                // A named place (bookmark): `<a name>` or `<a id>`.
                if let Some(name) = attr(attrs, "name")
                    .or_else(|| attr(attrs, "id"))
                    .and_then(mailcraft_model::rich::clean_anchor_name)
                {
                    sink.push_inline(Inline::Anchor { name }, &Ctx { link: None, ..c.clone() });
                }
                if let Some(href) = attr(attrs, "href").and_then(sanitize::safe_href) {
                    c.link = Some(href.into());
                    c.link_title = attr(attrs, "title")
                        .map(|t| normalize_ws(t).chars().take(MAX_LINK_TITLE).collect::<String>())
                        .filter(|t| !t.is_empty())
                        .map(Into::into);
                }
                self.walk_children(id, &c, sink, depth);
            }
            "hr" => sink.push_block(Block::Rule),
            "ul" | "ol" | "menu" | "dir" => self.list(id, name, attrs, c, sink, depth),
            "blockquote" => {
                c.indent = 0.0;
                let blocks = self.flow_with(id, &c, c.para(), depth);
                if !blocks.is_empty() {
                    sink.push_block(Block::Quote { blocks });
                }
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                let mut text = String::new();
                self.collect_pre(id, &mut text, depth);
                if let Some(rest) = text.strip_prefix('\n') {
                    text = rest.to_owned();
                }
                if !text.is_empty() {
                    sink.push_block(Block::Pre { text });
                }
            }
            "table" => self.table(id, attrs, &bx, c, sink, depth),
            h if dom::is_heading(h) => {
                let level = h.strip_prefix('h').and_then(|d| d.parse::<u8>().ok()).unwrap_or(1).clamp(1, 6);
                let bordered = bx.bordered_para();
                c.indent = (c.indent + bordered.as_ref().map_or(bx.indent, |b| b.indent)).max(0.0);
                let mut para = bx.para_extras(ParaStyle {
                    space_before_px: bx.margin_top.unwrap_or(0.0).max(0.0),
                    space_after_px: bx.margin_bottom.unwrap_or(0.0).max(0.0),
                    background: bx.background,
                    ..c.para()
                });
                if let Some(b) = bordered {
                    b.apply(&mut para);
                }
                let blocks = self.flow_with(id, &c, para, depth);
                sink.push_blocks(
                    blocks
                        .into_iter()
                        .map(|b| match b {
                            Block::Paragraph { style, inlines } => Block::Heading { level, style, inlines },
                            other => other,
                        })
                        .collect(),
                );
            }
            _ if is_block(name) => self.block(id, name, &bx, c, sink, depth),
            _ => self.walk_children(id, &c, sink, depth),
        }
    }

    fn block(&self, id: NodeId, name: &str, bx: &BoxStyle, mut c: Ctx, sink: &mut Sink, depth: usize) {
        let bordered = if name == "p" { bx.bordered_para() } else { None };
        if bx.top_rule && name == "div" {
            sink.push_block(Block::Rule);
        }
        let space_before = bx.margin_top.unwrap_or(0.0).max(0.0);
        if name != "p"
            && let Some(bg) = bx.background
        {
            let style = ParaStyle {
                align: c.align,
                indent_px: bx.indent.max(0.0),
                space_before_px: space_before,
                space_after_px: bx.margin_bottom.unwrap_or(0.0).max(0.0),
                line_height: None,
                background: Some(bg),
                ..ParaStyle::default()
            };
            c.indent = 0.0;
            let blocks = self.flow_with(id, &c, c.para(), depth);
            if !blocks.is_empty() {
                sink.push_block(Block::Container { style, blocks });
            }
            return;
        }
        c.indent = (c.indent + bordered.as_ref().map_or(bx.indent, |b| b.indent)).max(0.0);
        let default_after = if name == "p" { self.font_px(&c) } else { 0.0 };
        let mut para = bx.para_extras(ParaStyle {
            space_before_px: space_before,
            space_after_px: bx.margin_bottom.unwrap_or(default_after).max(0.0),
            background: if name == "p" { bx.background } else { None },
            ..c.para()
        });
        if let Some(b) = bordered {
            b.apply(&mut para);
        }
        let blocks = self.flow_with(id, &c, para, depth);
        sink.push_blocks(blocks);
    }

    fn list(
        &self,
        id: NodeId,
        name: &str,
        attrs: &[(String, String)],
        mut c: Ctx,
        sink: &mut Sink,
        depth: usize,
    ) {
        let kind = if name == "ol" {
            let start = attr(attrs, "start")
                .and_then(|s| s.trim().parse::<i64>().ok())
                .map_or(1, |v| u32::try_from(v.clamp(0, i64::from(u32::MAX))).unwrap_or(1));
            ListKind::Numbered { start }
        } else {
            ListKind::Bullet
        };
        c.indent = 0.0;
        let mut items: Vec<Vec<Block>> = Vec::new();
        let attach = |blocks: Vec<Block>, items: &mut Vec<Vec<Block>>| {
            if blocks.is_empty() {
                return;
            }
            match items.last_mut() {
                Some(last) => last.extend(blocks),
                None => items.push(blocks),
            }
        };
        let mut stray = Sink::new(c.para(), &self.link_budget);
        for &ch in self.dom.children(id) {
            if let Some(Node { data: NodeData::Element { name: n, attrs: a }, .. }) = self.dom.node(ch)
                && n == "li"
            {
                attach(
                    std::mem::replace(&mut stray, Sink::new(c.para(), &self.link_budget)).finish(),
                    &mut items,
                );
                let Some((mut lc, bx)) = self.element_ctx(n, a, &c) else { continue };
                lc.indent = bx.indent.max(0.0);
                let para = ParaStyle {
                    space_before_px: bx.margin_top.unwrap_or(0.0).max(0.0),
                    space_after_px: bx.margin_bottom.unwrap_or(0.0).max(0.0),
                    ..lc.para()
                };
                items.push(self.flow_with(ch, &lc, para, depth + 1));
            } else {
                self.walk(ch, &c, &mut stray, depth + 1);
            }
        }
        attach(stray.finish(), &mut items);
        if !items.is_empty() {
            sink.push_block(Block::List { kind, items });
        }
    }

    fn collect_pre(&self, id: NodeId, out: &mut String, depth: usize) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        for &ch in self.dom.children(id) {
            match self.dom.node(ch).map(|n| &n.data) {
                Some(NodeData::Text(t)) => out.push_str(t),
                Some(NodeData::Element { name, attrs }) => {
                    if sanitize::is_dropped(name) || self.element_ctx(name, attrs, &Ctx::default()).is_none()
                    {
                        continue;
                    }
                    if name == "br" {
                        out.push('\n');
                    } else {
                        self.collect_pre(ch, out, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    fn table(
        &self,
        id: NodeId,
        attrs: &[(String, String)],
        bx: &BoxStyle,
        mut c: Ctx,
        sink: &mut Sink,
        depth: usize,
    ) {
        c.indent = 0.0;
        let border = attr(attrs, "border").is_some_and(|b| parse_uint(b).is_none_or(|v| v > 0)) || bx.border;
        let mut rows: Vec<Vec<(TableCell, CellEdges)>> = Vec::new();
        let mut foster = Sink::new(c.para(), &self.link_budget);
        let mut loose: Vec<(TableCell, CellEdges)> = Vec::new();
        for &ch in self.dom.children(id) {
            match self.dom.node(ch).map(|n| &n.data) {
                Some(NodeData::Element { name, attrs: a }) if name == "tr" => {
                    if let Some(row) = self.row(ch, a, &c, bx.background, &mut foster, depth + 1)
                        && rows.len() < MAX_TABLE_ROWS
                    {
                        rows.push(row);
                    }
                }
                Some(NodeData::Element { name, attrs: a }) if name == "td" || name == "th" => {
                    if let Some(cell) = self.cell(ch, name, a, &c, bx.background, depth + 1)
                        && loose.len() < MAX_TABLE_COLS
                    {
                        loose.push(cell);
                    }
                }
                _ => self.walk(ch, &c, &mut foster, depth + 1),
            }
        }
        if !loose.is_empty() && rows.len() < MAX_TABLE_ROWS {
            rows.push(loose);
        }
        sink.push_blocks(foster.finish());
        if !rows.is_empty() {
            let borders = if border { None } else { table_borders(&rows) };
            let rows = rows.into_iter().map(|r| r.into_iter().map(|(cell, _)| cell).collect()).collect();
            sink.push_block(Block::Table { rows, border, width_px: bx.width, borders });
        }
    }

    fn row(
        &self,
        id: NodeId,
        attrs: &[(String, String)],
        c: &Ctx,
        table_bg: Option<Color>,
        foster: &mut Sink,
        depth: usize,
    ) -> Option<Vec<(TableCell, CellEdges)>> {
        let (rc, bx) = self.element_ctx("tr", attrs, c)?;
        let bg = bx.background.or(table_bg);
        let mut cells = Vec::new();
        for &ch in self.dom.children(id) {
            match self.dom.node(ch).map(|n| &n.data) {
                Some(NodeData::Element { name, attrs: a }) if name == "td" || name == "th" => {
                    if cells.len() < MAX_TABLE_COLS
                        && let Some(cell) = self.cell(ch, name, a, &rc, bg, depth + 1)
                    {
                        cells.push(cell);
                    }
                }
                _ => self.walk(ch, &rc, foster, depth + 1),
            }
        }
        (!cells.is_empty()).then_some(cells)
    }

    fn cell(
        &self,
        id: NodeId,
        name: &str,
        attrs: &[(String, String)],
        c: &Ctx,
        bg: Option<Color>,
        depth: usize,
    ) -> Option<(TableCell, CellEdges)> {
        let (mut cc, bx) = self.element_ctx(name, attrs, c)?;
        if name == "th" && bx.align.is_none() {
            cc.align = Align::Center;
        }
        cc.indent = 0.0;
        let colspan = attr(attrs, "colspan").and_then(parse_uint).unwrap_or(1).clamp(1, 1000);
        let rowspan = attr(attrs, "rowspan").and_then(parse_uint).unwrap_or(1).clamp(1, 65_534);
        let blocks = self.flow_with(id, &cc, cc.para(), depth);
        let cell =
            TableCell { blocks, colspan, rowspan, background: bx.background.or(bg), width_px: bx.width };
        Some((cell, bx.edges))
    }
}

/// A table cell's border edges in CSS box order (top, right, bottom, left).
type CellEdges = [Option<BorderEdge>; 4];

/// Per-edge table borders read back from the cells' borders (as the serializer writes them: the outside
/// edges on the outer cells, the inside lines between cells); `None` when no cell has a border.
fn table_borders(rows: &[Vec<(TableCell, CellEdges)>]) -> Option<Box<TableBorders>> {
    if !rows.iter().flatten().any(|(_, e)| e.iter().any(Option::is_some)) {
        return None;
    }
    let edge = |r: Option<&Vec<(TableCell, CellEdges)>>, first: bool, side: usize| {
        let row = r?;
        let cell = if first { row.first() } else { row.last() };
        cell.and_then(|(_, e)| e.get(side).cloned().flatten())
    };
    let first = rows.first();
    let second = rows.get(1);
    let wide = first.is_some_and(|r| r.len() > 1);
    Some(Box::new(TableBorders {
        top: edge(first, true, 0),
        right: edge(first, false, 1),
        bottom: edge(rows.last(), true, 2),
        left: edge(first, true, 3),
        inside_h: if second.is_some() {
            edge(second, true, 0).or_else(|| edge(first, true, 2))
        } else {
            None
        },
        inside_v: if wide { edge(first, true, 1) } else { None },
    }))
}

impl BorderedPara {
    /// Puts the borders and their spacing on `para`.
    fn apply(self, para: &mut ParaStyle) {
        para.space_before_px += self.before;
        para.space_after_px += self.after;
        para.right_indent_px = self.right;
        para.borders = Some(Box::new(self.borders));
    }
}

/// Converts a parsed tree into a sanitised document.
pub(crate) fn dom_to_doc(dom: &Dom) -> Doc {
    let mut sheet = Stylesheet::default();
    for node in &dom.nodes {
        if matches!(&node.data, NodeData::Element { name, .. } if name == "style") {
            for &c in &node.children {
                if let Some(Node { data: NodeData::Text(t), .. }) = dom.node(c) {
                    sheet.add(t);
                }
            }
        }
    }
    let mut conv = Conv {
        dom,
        sheet,
        base_pt: DEFAULT_PT,
        cascade_cache: RefCell::new(HashMap::new()),
        cascade_budget: Cell::new(CASCADE_BUDGET),
        link_budget: Cell::new(LINK_BUDGET),
    };
    let body = conv.props("body", &dom.body_attrs);
    let mut base = TextStyle::default();
    apply_text_props(&mut base, &body, "body", DEFAULT_PT, false);
    conv.base_pt = base.size_pt.unwrap_or(DEFAULT_PT);
    let bx = box_style(&body, conv.base_pt * 4.0 / 3.0);
    let ctx = Ctx {
        align: bx.align.unwrap_or_default(),
        line_height: bx.line_height.flatten(),
        preserve_ws: bx.preserve_ws.unwrap_or(false),
        ..Ctx::default()
    };
    let blocks = conv.flow_with(Dom::ROOT, &ctx, ctx.para(), 0);
    Doc { blocks, base, background: bx.background, drawings: Vec::new() }
}
