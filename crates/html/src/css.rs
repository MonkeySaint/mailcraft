//! The CSS subset mail needs: declaration lists (inline `style`), `<style>` sheets with simple compound
//! selectors (`tag`, `.class`, `#id`, `tag.class`, `*`, comma lists; anything with combinators or pseudo
//! classes is ignored, as are at-rules such as `@media` and `@import`), colours and lengths.

use mailcraft_geom::Color;
use std::collections::HashMap;

/// Caps for hostile stylesheets.
const MAX_CSS_BYTES: usize = 1024 * 1024;
const MAX_RULES: usize = 10_000;
const MAX_DECLS: usize = 256;
/// Rules kept per selector key (id, class, tag or universal).
const MAX_RULES_PER_KEY: usize = 64;

/// Properties the converter understands; everything else is discarded at parse time, which also bounds the
/// work hostile stylesheets can cause.
fn is_known_property(p: &str) -> bool {
    matches!(
        p,
        "color"
            | "background"
            | "background-color"
            | "font"
            | "font-family"
            | "font-size"
            | "font-weight"
            | "font-style"
            | "text-decoration"
            | "text-decoration-line"
            | "text-decoration-style"
            | "text-decoration-color"
            | "font-variant"
            | "text-transform"
            | "letter-spacing"
            | "text-indent"
            | "margin-right"
            | "padding-right"
            | "page-break-before"
            | "page-break-after"
            | "page-break-inside"
            | "break-before"
            | "break-after"
            | "break-inside"
            | "widows"
            | "orphans"
            | "mso-line-height-rule"
            | "text-align"
            | "vertical-align"
            | "margin"
            | "margin-top"
            | "margin-bottom"
            | "margin-left"
            | "padding"
            | "padding-left"
            | "line-height"
            | "width"
            | "height"
            | "display"
            | "mso-hide"
            | "border"
            | "border-style"
            | "border-top"
            | "border-top-style"
            | "white-space"
    )
}

/// Default font size (CSS `medium`) in points.
pub(crate) const DEFAULT_PT: f32 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Decl {
    pub prop: String,
    pub value: String,
    pub important: bool,
}

/// Removes `/* … */` comments (an unterminated comment runs to the end).
pub(crate) fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("/*") {
        out.push_str(rest.get(..p).unwrap_or(""));
        out.push(' ');
        rest = match rest.get(p + 2..).and_then(|r| r.find("*/").map(|e| (r, e))) {
            Some((r, e)) => r.get(e + 2..).unwrap_or(""),
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Splits on `sep` outside quotes and parentheses.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quote: Option<char> = None;
    let mut depth = 0u32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth = depth.saturating_add(1),
            (None, ')') => depth = depth.saturating_sub(1),
            (None, c) if c == sep && depth == 0 => {
                parts.push(s.get(start..i).unwrap_or(""));
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(s.get(start..).unwrap_or(""));
    parts
}

/// Parses a declaration list such as an inline `style` attribute.
pub(crate) fn parse_declarations(s: &str) -> Vec<Decl> {
    let cleaned = strip_comments(s);
    let mut out = Vec::new();
    for part in split_top(&cleaned, ';') {
        if out.len() >= MAX_DECLS {
            break;
        }
        let Some((prop, value)) = part.split_once(':') else { continue };
        let prop = prop.trim().to_ascii_lowercase();
        if !is_known_property(&prop) {
            continue;
        }
        let mut value = value.trim();
        let mut important = false;
        if let Some(bang) = value.rfind('!')
            && value.get(bang + 1..).is_some_and(|t| t.trim().eq_ignore_ascii_case("important"))
        {
            important = true;
            value = value.get(..bang).unwrap_or("").trim();
        }
        if value.is_empty() {
            continue;
        }
        out.push(Decl { prop, value: value.to_owned(), important });
    }
    out
}

/// Longest `font-family` value kept (real font stacks are well under this). Text runs each carry their own
/// copy, so an unbounded family would multiply memory by the number of runs.
pub(crate) const MAX_FONT_FAMILY: usize = 256;

/// Makes a font-family list safe to store and to write back into a `style` attribute: it stops at the first
/// top-level `;`, `{` or `}` (so it cannot smuggle further declarations), drops backslashes and control
/// characters, collapses whitespace, closes an unterminated quote and keeps at most [`MAX_FONT_FAMILY`]
/// bytes. Returns `None` when nothing is left.
pub(crate) fn clean_font_family(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut space = false;
    for c in s.chars() {
        if out.len() + usize::from(space) + c.len_utf8() > MAX_FONT_FAMILY {
            break;
        }
        match (quote, c) {
            (_, '\\') => continue,
            (None, ';' | '{' | '}') => break,
            (_, c) if c.is_whitespace() || c.is_control() => {
                space = !out.is_empty();
                continue;
            }
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            _ => {}
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    if let Some(q) = quote {
        out.push(q);
    }
    (!out.is_empty()).then_some(out)
}

/// Classes of one element looked at by the cascade (more are ignored).
const MAX_CLASSES: usize = 64;

/// A compound selector: optional type plus classes and ids.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Compound {
    tag: Option<String>,
    ids: Vec<String>,
    classes: Vec<String>,
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

impl Compound {
    pub(crate) fn parse(s: &str) -> Option<Compound> {
        let s = s.trim();
        let mut sel = Compound::default();
        let mut chars = s.char_indices().peekable();
        let ident = |chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>| -> Option<String> {
            let mut id = String::new();
            while let Some(&(_, c)) = chars.peek() {
                if !is_ident_char(c) {
                    break;
                }
                id.push(c);
                chars.next();
            }
            (!id.is_empty()).then_some(id)
        };
        match chars.peek() {
            None => return None,
            Some(&(_, '*')) => {
                chars.next();
            }
            Some(&(_, c)) if is_ident_char(c) => sel.tag = Some(ident(&mut chars)?.to_ascii_lowercase()),
            _ => {}
        }
        while let Some((_, c)) = chars.next() {
            match c {
                '.' => sel.classes.push(ident(&mut chars)?.to_lowercase()),
                '#' => sel.ids.push(ident(&mut chars)?.to_lowercase()),
                _ => return None,
            }
        }
        Some(sel)
    }

    fn specificity(&self) -> (usize, usize, usize) {
        (self.ids.len(), self.classes.len(), usize::from(self.tag.is_some()))
    }

    /// `id` and `classes` must already be lowercase; `classes` must be sorted.
    fn matches(&self, tag: &str, id: Option<&str>, classes: &[String]) -> bool {
        self.tag.as_deref().is_none_or(|t| t == tag)
            && self.ids.iter().all(|i| Some(i.as_str()) == id)
            && self.classes.iter().all(|c| classes.binary_search(c).is_ok())
    }
}

#[derive(Clone, Debug)]
struct Rule {
    sel: Compound,
    spec: (usize, usize, usize),
    order: usize,
    block: usize,
}

/// A parsed set of `<style>` rules, bucketed by their most specific key.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stylesheet {
    rules: Vec<Rule>,
    blocks: Vec<Vec<Decl>>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    bytes: usize,
}

/// Index of the `}` closing the block whose `{` is at `open` (string-aware, nesting-aware), or the input
/// length when the block is unterminated.
fn block_end(b: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while let Some(&c) = b.get(i) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), b'\\') => i += 1,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'{') => depth += 1,
            (None, b'}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// First top-level `;` or `{` from `from`.
fn find_stop(b: &[u8], from: usize) -> Option<(usize, u8)> {
    let mut quote: Option<u8> = None;
    let mut i = from;
    while let Some(&c) = b.get(i) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b';' | b'{' | b'}') => return Some((i, c)),
            _ => {}
        }
        i += 1;
    }
    None
}

impl Stylesheet {
    pub(crate) fn add(&mut self, css: &str) {
        let budget = MAX_CSS_BYTES.saturating_sub(self.bytes);
        if budget == 0 {
            return;
        }
        let mut end = css.len().min(budget);
        while !css.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes += end;
        let css = strip_comments(css.get(..end).unwrap_or("")).replace("<!--", " ").replace("-->", " ");
        let b = css.as_bytes();
        let mut pos = 0;
        while pos < b.len() && self.rules.len() < MAX_RULES {
            while b.get(pos).is_some_and(|c| c.is_ascii_whitespace() || *c == b'}' || *c == b';') {
                pos += 1;
            }
            if pos >= b.len() {
                break;
            }
            let at_rule = b.get(pos) == Some(&b'@');
            let Some((stop, kind)) = find_stop(b, pos) else { break };
            if kind != b'{' {
                // `@import …;` or a stray declaration: skip it.
                pos = stop + 1;
                continue;
            }
            let close = block_end(b, stop);
            if !at_rule {
                let prelude = css.get(pos..stop).unwrap_or("");
                let body = css.get(stop + 1..close).unwrap_or("");
                self.add_rule(prelude, body);
            }
            pos = close + 1;
        }
    }

    fn add_rule(&mut self, prelude: &str, body: &str) {
        let mut decls: Vec<Decl> = Vec::new();
        // Keep only the last declaration per (property, importance); earlier ones can never win.
        for d in parse_declarations(body) {
            decls.retain(|e| !(e.prop == d.prop && e.important == d.important));
            decls.push(d);
        }
        if decls.is_empty() {
            return;
        }
        let block = self.blocks.len();
        let mut used = false;
        for sel in prelude.split(',') {
            let Some(sel) = Compound::parse(sel) else { continue };
            if self.rules.len() >= MAX_RULES {
                break;
            }
            let idx = self.rules.len();
            let bucket = if let Some(id) = sel.ids.first() {
                self.by_id.entry(id.clone()).or_default()
            } else if let Some(c) = sel.classes.first() {
                self.by_class.entry(c.clone()).or_default()
            } else if let Some(t) = &sel.tag {
                self.by_tag.entry(t.clone()).or_default()
            } else {
                &mut self.universal
            };
            if bucket.len() >= MAX_RULES_PER_KEY {
                continue;
            }
            bucket.push(idx);
            let spec = sel.specificity();
            self.rules.push(Rule { sel, spec, order: idx, block });
            used = true;
        }
        if used {
            self.blocks.push(decls);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Normalised selector key of an element: lowercase id, and its lowercase classes sorted, deduplicated
    /// and capped at [`MAX_CLASSES`].
    pub(crate) fn key(tag: &str, id: Option<&str>, class_attr: Option<&str>) -> CascadeKey {
        let id = id.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let mut classes: Vec<String> = class_attr
            .map(|c| c.split_ascii_whitespace().take(MAX_CLASSES).map(str::to_lowercase).collect())
            .unwrap_or_default();
        classes.sort_unstable();
        classes.dedup();
        CascadeKey { tag: tag.to_owned(), id, classes }
    }

    /// The stylesheet's contribution to an element: the winning value of each property, split into normal and
    /// `!important` declarations. `budget` is decremented by the work done (candidate rules plus
    /// declarations); once it reaches zero the remaining rules are skipped, which bounds the total cascade
    /// work a hostile document can cause.
    pub(crate) fn cascade(&self, key: &CascadeKey, budget: &mut usize) -> (Props, Props) {
        let mut normal = Props::default();
        let mut important = Props::default();
        if self.rules.is_empty() || *budget == 0 {
            return (normal, important);
        }
        let mut cand: Vec<usize> = Vec::new();
        if let Some(id) = &key.id
            && let Some(v) = self.by_id.get(id)
        {
            cand.extend(v);
        }
        for c in &key.classes {
            if let Some(v) = self.by_class.get(c) {
                cand.extend(v);
            }
        }
        if let Some(v) = self.by_tag.get(&key.tag) {
            cand.extend(v);
        }
        cand.extend(&self.universal);
        cand.sort_unstable();
        cand.dedup();
        *budget = budget.saturating_sub(cand.len());
        let mut matched: Vec<&Rule> = cand
            .iter()
            .filter_map(|&i| self.rules.get(i))
            .filter(|r| r.sel.matches(&key.tag, key.id.as_deref(), &key.classes))
            .collect();
        matched.sort_by_key(|r| (r.spec, r.order));
        for r in matched {
            let decls = self.blocks.get(r.block).map(Vec::as_slice).unwrap_or(&[]);
            if *budget < decls.len() {
                *budget = 0;
                break;
            }
            *budget -= decls.len();
            for d in decls {
                if d.important {
                    important.set(&d.prop, &d.value);
                } else {
                    normal.set(&d.prop, &d.value);
                }
            }
        }
        (normal, important)
    }
}

/// What the cascade depends on for one element (see [`Stylesheet::key`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CascadeKey {
    tag: String,
    id: Option<String>,
    classes: Vec<String>,
}

/// Resolved property map for one element (later `set` wins).
#[derive(Clone, Debug, Default)]
pub(crate) struct Props(Vec<(String, String)>);

impl Props {
    pub(crate) fn set(&mut self, prop: &str, value: &str) {
        if let Some(slot) = self.0.iter_mut().find(|(p, _)| p == prop) {
            slot.1 = value.to_owned();
        } else {
            self.0.push((prop.to_owned(), value.to_owned()));
        }
    }

    pub(crate) fn get(&self, prop: &str) -> Option<&str> {
        self.0.iter().find(|(p, _)| p == prop).map(|(_, v)| v.as_str())
    }

    pub(crate) fn apply(&mut self, decls: &[&Decl]) {
        for d in decls {
            self.set(&d.prop, &d.value);
        }
    }

    /// Sets every property of `other` (its values win).
    pub(crate) fn merge(&mut self, other: &Props) {
        for (p, v) in &other.0 {
            self.set(p, v);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Px(f32),
    Percent(f32),
}

fn finite(v: f32) -> Option<f32> {
    v.is_finite().then_some(v.clamp(-100_000.0, 100_000.0))
}

/// Splits a leading CSS number from its unit.
fn split_number(s: &str) -> Option<(f32, &str)> {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0)))
        .map_or(s.len(), |(i, _)| i);
    let num: f32 = s.get(..end)?.parse().ok()?;
    Some((finite(num)?, s.get(end..)?.trim()))
}

/// Parses a length; `font_px` is the element's font size for `em`/`ex`.
pub(crate) fn parse_length(s: &str, font_px: f32) -> Option<Length> {
    let (v, unit) = split_number(s)?;
    let px = match unit.to_ascii_lowercase().as_str() {
        "" | "px" => v,
        "pt" => v * 4.0 / 3.0,
        "pc" => v * 16.0,
        "in" => v * 96.0,
        "cm" => v * 96.0 / 2.54,
        "mm" => v * 96.0 / 25.4,
        "q" => v * 96.0 / 101.6,
        "em" => v * font_px,
        "ex" | "ch" => v * font_px / 2.0,
        "rem" => v * DEFAULT_PT * 4.0 / 3.0,
        "%" => return Some(Length::Percent(v)),
        _ => return None,
    };
    finite(px).map(Length::Px)
}

/// A length in px, with percentages and `auto` treated as zero.
pub(crate) fn length_px(s: &str, font_px: f32) -> Option<f32> {
    if s.trim().eq_ignore_ascii_case("auto") {
        return Some(0.0);
    }
    match parse_length(s, font_px)? {
        Length::Px(v) => Some(v),
        Length::Percent(_) => Some(0.0),
    }
}

/// `font-size` to points. `parent_pt` is the inherited size.
pub(crate) fn font_size_pt(s: &str, parent_pt: f32) -> Option<f32> {
    let s = s.trim().to_ascii_lowercase();
    let px = match s.as_str() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => return Some((parent_pt / 1.2).clamp(1.0, 400.0)),
        "larger" => return Some((parent_pt * 1.2).clamp(1.0, 400.0)),
        _ => match parse_length(&s, parent_pt * 4.0 / 3.0)? {
            Length::Px(v) => v,
            Length::Percent(p) => parent_pt * p / 100.0 * 4.0 / 3.0,
        },
    };
    let pt = px * 0.75;
    (pt > 0.0).then_some(pt.clamp(1.0, 400.0))
}

/// Legacy `<font size>`: 1..=7 or relative `+n`/`-n` from 3.
pub(crate) fn legacy_font_size_pt(s: &str) -> Option<f32> {
    const SIZES: [f32; 7] = [7.5, 10.0, 12.0, 13.5, 18.0, 24.0, 36.0];
    let s = s.trim();
    let n: i32 = if let Some(r) = s.strip_prefix('+') {
        3i32.saturating_add(r.trim().parse::<i32>().ok()?)
    } else if let Some(r) = s.strip_prefix('-') {
        3i32.saturating_sub(r.trim().parse::<i32>().ok()?)
    } else {
        s.parse().ok()?
    };
    let idx = usize::try_from(n.clamp(1, 7) - 1).ok()?;
    SIZES.get(idx).copied()
}

/// CSS named colours (CSS Color Module Level 4), plus common system colour keywords as light-theme values.
#[rustfmt::skip]
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff), ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff), ("beige", 0xf5f5dc), ("bisque", 0xffe4c4), ("black", 0x000000),
    ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff), ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00), ("chocolate", 0xd2691e),
    ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c),
    ("cyan", 0x00ffff), ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f), ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc), ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b), ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1), ("darkviolet", 0x9400d3), ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969), ("dimgrey", 0x696969), ("dodgerblue", 0x1e90ff), ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22), ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xadff2f), ("grey", 0x808080), ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c), ("indigo", 0x4b0082), ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c), ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6), ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3), ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1), ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de), ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000), ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3), ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee), ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc), ("mediumvioletred", 0xc71585), ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead),
    ("navy", 0x000080), ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6), ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee), ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9), ("peru", 0xcd853f), ("pink", 0xffc0cb),
    ("plum", 0xdda0dd), ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1), ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072), ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d), ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd),
    ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa), ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4), ("tan", 0xd2b48c), ("teal", 0x008080), ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347), ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3),
    ("white", 0xffffff), ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
    ("windowtext", 0x000000), ("window", 0xffffff), ("buttonface", 0xf0f0f0), ("graytext", 0x6d6d6d),
];

/// One `rgb()`/`hsl()` component: number or percentage scaled to `pct_scale`.
fn component(s: &str, pct_scale: f32) -> Option<f32> {
    let s = s.trim();
    let v = if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? * pct_scale / 100.0
    } else {
        s.trim_end_matches("deg").parse::<f32>().ok()?
    };
    finite(v)
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0) * 255.0, hue(h) * 255.0, hue(h - 1.0 / 3.0) * 255.0)
}

/// Parses a CSS colour: `#hex`, `rgb[a]()`, `hsl[a]()`, named colours and `transparent`.
pub(crate) fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().trim_end_matches(';').trim();
    if s.starts_with('#') {
        return Color::parse_hex(s);
    }
    let lower = s.to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    if let Some(open) = lower.find('(') {
        let func = lower.get(..open)?.trim();
        let inner = lower.get(open + 1..)?.trim_end_matches(')');
        let (main, slash_alpha) = match inner.split_once('/') {
            Some((m, a)) => (m, Some(a)),
            None => (inner, None),
        };
        let parts: Vec<&str> = main.split([',', ' ']).filter(|p| !p.trim().is_empty()).collect();
        let (a, b, c) = (parts.first()?, parts.get(1)?, parts.get(2)?);
        let alpha = slash_alpha.or(parts.get(3).copied()).map_or(Some(1.0), |a| component(a, 1.0))?;
        let alpha = to_u8(alpha.clamp(0.0, 1.0) * 255.0);
        return match func {
            "rgb" | "rgba" => Some(Color::rgba(
                to_u8(component(a, 255.0)?),
                to_u8(component(b, 255.0)?),
                to_u8(component(c, 255.0)?),
                alpha,
            )),
            "hsl" | "hsla" => {
                let (r, g, bl) = hsl_to_rgb(component(a, 360.0)?, component(b, 1.0)?, component(c, 1.0)?);
                Some(Color::rgba(to_u8(r), to_u8(g), to_u8(bl), alpha))
            }
            _ => None,
        };
    }
    NAMED.iter().find(|(n, _)| *n == lower).map(|(_, v)| Color::hex(*v))
}

/// Presentational colour attributes (`bgcolor`, `color`, `text`): CSS colours plus bare hex digits.
pub(crate) fn parse_legacy_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    parse_color(s).or_else(|| {
        (matches!(s.len(), 3 | 6) && s.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| Color::parse_hex(s))
            .flatten()
    })
}

/// Finds a colour token inside a shorthand such as `background: #fff url(x) no-repeat`.
pub(crate) fn color_in_shorthand(s: &str) -> Option<Color> {
    if let Some(c) = parse_color(s) {
        return Some(c);
    }
    split_top(s, ' ').into_iter().filter(|t| !t.trim().is_empty()).find_map(parse_color)
}

/// `url(...)` references inside CSS text, plus `@import "…"` strings, in source order.
pub(crate) fn urls(css: &str) -> Vec<String> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let lower = css.to_ascii_lowercase();
    let mut i = 0;
    while let Some(p) = lower.get(i..).and_then(|s| s.find("url(")) {
        let start = i + p + 4;
        let rest = css.get(start..).unwrap_or("");
        let close = rest.find(')').unwrap_or(rest.len());
        let raw = rest.get(..close).unwrap_or("").trim().trim_matches(['"', '\'']).trim();
        if !raw.is_empty() {
            out.push((start, raw.to_owned()));
        }
        i = start + close;
    }
    let mut i = 0;
    while let Some(p) = lower.get(i..).and_then(|s| s.find("@import")) {
        let start = i + p + 7;
        let rest = css.get(start..).unwrap_or("").trim_start();
        if let Some(q @ ('"' | '\'')) = rest.chars().next() {
            let body = rest.get(1..).unwrap_or("");
            if let Some(e) = body.find(q) {
                out.push((start, body.get(..e).unwrap_or("").to_owned()));
            }
        }
        i = start;
    }
    out.sort_by_key(|(pos, _)| *pos);
    out.into_iter().map(|(_, u)| u).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations() {
        let d = parse_declarations(
            "color: red; FONT-family: \"A;B\", serif ; /* c */ margin:0 !important;;bad;x:",
        );
        assert_eq!(d.len(), 3);
        assert_eq!(d[0], Decl { prop: "color".into(), value: "red".into(), important: false });
        assert_eq!(d[1].value, "\"A;B\", serif");
        assert_eq!(d[1].prop, "font-family");
        assert!(d[2].important);
        assert_eq!(d[2].value, "0");
        assert!(
            parse_declarations("background:url(a;b.png)").first().is_some_and(|d| d.value == "url(a;b.png)")
        );
    }

    #[test]
    fn selectors() {
        assert_eq!(Compound::parse("p.MsoNormal").map(|c| c.specificity()), Some((0, 1, 1)));
        assert_eq!(Compound::parse("#x.a.b").map(|c| c.specificity()), Some((1, 2, 0)));
        assert!(Compound::parse("*").is_some());
        assert!(Compound::parse("div p").is_none());
        assert!(Compound::parse("a:hover").is_none());
        assert!(Compound::parse("td > p").is_none());
        assert!(Compound::parse("[type=x]").is_none());
        assert!(Compound::parse("").is_none());
        assert!(Compound::parse(".").is_none());
    }

    #[test]
    fn stylesheet_cascade() {
        let mut s = Stylesheet::default();
        s.add(
            "<!-- @import url(x.css); @media screen { p { color: blue } } @font-face { font-family: X }
             p { color: red; margin: 0 } .big, h1 { font-size: 20pt } p.note { color: green }
             #main { color: purple !important } div p { color: orange } a:hover { color: pink } -->",
        );
        let mut budget = usize::MAX;
        let (n, i) = s.cascade(&Stylesheet::key("p", None, Some("note other")), &mut budget);
        let mut props = Props::default();
        props.merge(&n);
        props.merge(&i);
        assert_eq!(props.get("color"), Some("green"));
        assert_eq!(props.get("margin"), Some("0"));
        let (n, i) = s.cascade(&Stylesheet::key("p", Some("MAIN"), Some("BIG")), &mut budget);
        assert_eq!(n.get("font-size"), Some("20pt"));
        assert_eq!(i.get("color"), Some("purple"));
        assert_eq!(n.get("color"), Some("red"), "the normal declaration is still reported separately");
        let (n, i) = s.cascade(&Stylesheet::key("span", None, None), &mut budget);
        assert!(n.get("color").is_none() && i.get("color").is_none());
        // An exhausted budget skips the stylesheet.
        let mut empty = 0;
        assert!(s.cascade(&Stylesheet::key("p", None, Some("note")), &mut empty).0.get("color").is_none());
    }

    #[test]
    fn stylesheet_hostile() {
        let mut s = Stylesheet::default();
        s.add("p { color: red");
        assert!(!s.is_empty());
        let mut s = Stylesheet::default();
        s.add("}}}{{{ ;;; @media { { { p { color: red } ");
        s.add(&"a{color:red}".repeat(20_000));
        assert!(s.rules.len() <= MAX_RULES);
    }

    #[test]
    fn lengths_and_sizes() {
        assert_eq!(parse_length("12px", 16.0), Some(Length::Px(12.0)));
        assert_eq!(parse_length("9pt", 16.0), Some(Length::Px(12.0)));
        assert_eq!(parse_length("2em", 16.0), Some(Length::Px(32.0)));
        assert_eq!(parse_length("50%", 16.0), Some(Length::Percent(50.0)));
        assert_eq!(parse_length("1in", 16.0), Some(Length::Px(96.0)));
        assert_eq!(parse_length("-.5px", 16.0), Some(Length::Px(-0.5)));
        assert_eq!(parse_length("abc", 16.0), None);
        assert_eq!(parse_length("1e999px", 16.0), None);
        assert_eq!(font_size_pt("16px", 12.0), Some(12.0));
        assert_eq!(font_size_pt("11pt", 12.0), Some(11.0));
        assert_eq!(font_size_pt("150%", 10.0), Some(15.0));
        assert_eq!(font_size_pt("2em", 10.0), Some(20.0));
        assert_eq!(font_size_pt("small", 12.0), Some(9.75));
        assert_eq!(font_size_pt("0", 12.0), None);
        assert_eq!(legacy_font_size_pt("2"), Some(10.0));
        assert_eq!(legacy_font_size_pt("+1"), Some(13.5));
        assert_eq!(legacy_font_size_pt("-9"), Some(7.5));
        assert_eq!(legacy_font_size_pt("x"), None);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#f00"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("RED"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(parse_color("rgba(255,0,0,0.5)"), Some(Color::rgba(255, 0, 0, 128)));
        assert_eq!(parse_color("rgb(100% 0% 0% / 50%)"), Some(Color::rgba(255, 0, 0, 128)));
        assert_eq!(parse_color("hsl(120, 100%, 50%)"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(parse_color("transparent"), Some(Color::TRANSPARENT));
        assert_eq!(parse_color("notacolor"), None);
        assert_eq!(parse_color("rgb(1)"), None);
        assert_eq!(parse_legacy_color("ff8800"), Some(Color::hex(0xff8800)));
        assert_eq!(parse_legacy_color("#FFF"), Some(Color::WHITE));
        assert_eq!(color_in_shorthand("url(x.png) #123456 no-repeat"), Some(Color::hex(0x123456)));
    }

    #[test]
    fn selector_keys_dedupe_and_cap_classes() {
        let k = Stylesheet::key("p", Some(" X "), Some("b A a  B"));
        assert_eq!(k.id.as_deref(), Some("x"));
        assert_eq!(k.classes, vec!["a".to_owned(), "b".to_owned()]);
        let many: String = (0..1000).map(|i| format!("c{i} ")).collect();
        assert_eq!(Stylesheet::key("p", None, Some(&many)).classes.len(), MAX_CLASSES);
    }

    #[test]
    fn font_family_cleaning() {
        assert_eq!(clean_font_family("\"Segoe  UI\",\n Arial ").as_deref(), Some("\"Segoe UI\", Arial"));
        assert_eq!(clean_font_family("x;background:url(y)").as_deref(), Some("x"));
        assert_eq!(clean_font_family("\"A;B\", serif").as_deref(), Some("\"A;B\", serif"));
        assert_eq!(clean_font_family("a}b{c").as_deref(), Some("a"));
        assert_eq!(clean_font_family("\"open").as_deref(), Some("\"open\""));
        assert_eq!(clean_font_family("a\\b").as_deref(), Some("ab"));
        assert_eq!(clean_font_family(" ; x"), None);
        let long = clean_font_family(&"é".repeat(1000)).unwrap_or_default();
        assert!(long.len() <= MAX_FONT_FAMILY && long.chars().all(|c| c == 'é'));
        for s in ["\"a b", "x , \"y;z\"", &"\"q".repeat(200)] {
            let once = clean_font_family(s);
            assert_eq!(once.as_deref().and_then(clean_font_family), once, "{s:?}");
        }
    }

    #[test]
    fn css_urls() {
        let u = urls(
            "a{background:url( 'http://x/a.png' )} @import \"http://x/s.css\"; b{background:URL(http://y/b.gif)}",
        );
        assert_eq!(u, vec!["http://x/a.png", "http://x/s.css", "http://y/b.gif"]);
    }
}
