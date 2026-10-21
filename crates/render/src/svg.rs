//! SVG pictures to PNG: many mail programs do not show `image/svg+xml` inline, so the compose window turns
//! the SVG pictures of a message (bundled icons, pictures pasted or inserted as SVG) into PNG before sending.
//!
//! Only plain SVG is drawn (shapes, groups, paths, text, gradients): at most [`MAX_SVG_BYTES`] of UTF-8 text,
//! no DTD, at most [`MAX_SVG_NODES`] nodes, nothing outside the file (no files, network or embedded
//! pictures), and no element or attribute that draws through offscreen layers or repeats work (filters,
//! masks, `use`, markers, patterns, `style`). Compressed SVG (svgz) is refused. Text needs fonts this
//! rasteriser does not load, so SVG text is not drawn.

use resvg::usvg::roxmltree;

/// Largest SVG accepted (bytes).
pub const MAX_SVG_BYTES: usize = 1024 * 1024;
/// Most XML nodes in an accepted SVG.
pub const MAX_SVG_NODES: u32 = 20_000;
/// Longest side of the PNG (device pixels).
pub const MAX_SVG_SIDE: f32 = 4096.0;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const ELEMENTS: [&str; 18] = [
    "svg",
    "g",
    "defs",
    "title",
    "desc",
    "metadata",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "path",
    "text",
    "tspan",
    "linearGradient",
    "radialGradient",
    "stop",
];
const DENIED_ATTRIBUTES: [&str; 11] = [
    "style",
    "filter",
    "mask",
    "clip-path",
    "mix-blend-mode",
    "isolation",
    "stroke-dasharray",
    "marker-start",
    "marker-mid",
    "marker-end",
    "href",
];

/// A rasterised SVG: PNG bytes and the picture's own size (logical px, before `scale`).
#[derive(Clone, Debug, PartialEq)]
pub struct SvgPng {
    pub png: Vec<u8>,
    pub width: f32,
    pub height: f32,
}

fn plain(doc: &roxmltree::Document) -> bool {
    doc.descendants().filter(|n| n.is_element()).all(|n| {
        let tag = n.tag_name();
        tag.namespace() == Some(SVG_NS)
            && ELEMENTS.contains(&tag.name())
            && n.attributes().all(|a| !DENIED_ATTRIBUTES.contains(&a.name()))
    })
}

/// Draws `data` (SVG text) at `scale` device pixels per SVG unit (clamped so no side exceeds
/// [`MAX_SVG_SIDE`]) and encodes it as PNG. `None` for anything that is not plain SVG (see the module
/// documentation) or has no size.
pub fn svg_to_png(data: &[u8], scale: f32) -> Option<SvgPng> {
    if data.is_empty() || data.len() > MAX_SVG_BYTES || data.starts_with(&[0x1f, 0x8b]) {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    if !text.get(..text.len().min(4096))?.to_ascii_lowercase().contains("<svg") {
        return None;
    }
    let xml = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_SVG_NODES };
    let doc = roxmltree::Document::parse_with_options(text, xml).ok()?;
    if !plain(&doc) {
        return None;
    }
    let opt = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..resvg::usvg::Options::default()
    };
    let parse = std::panic::AssertUnwindSafe(|| resvg::usvg::Tree::from_xmltree(&doc, &opt).ok());
    let tree = std::panic::catch_unwind(parse).ok().flatten()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let scale = scale.min(MAX_SVG_SIDE / w.max(h));
    let pw = (w * scale).round().clamp(1.0, MAX_SVG_SIDE);
    let ph = (h * scale).round().clamp(1.0, MAX_SVG_SIDE);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw as u32, ph as u32)?;
    let transform = resvg::tiny_skia::Transform::from_scale(pw / w, ph / h);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resvg::render(&tree, transform, &mut pixmap.as_mut())
    }))
    .ok()?;
    let png = pixmap.encode_png().ok()?;
    Some(SvgPng { png, width: w, height: h })
}

/// Decodes PNG bytes into a premultiplied RGBA [`crate::Canvas`] (`None` when they are not a PNG or the
/// picture is larger than the canvas limits).
pub fn decode_png(data: &[u8]) -> Option<crate::Canvas> {
    if !data.starts_with(b"\x89PNG") {
        return None;
    }
    let pm = tiny_skia::Pixmap::decode_png(data).ok()?;
    crate::byte_len(pm.width(), pm.height())?;
    Some(crate::Canvas { width: pm.width(), height: pm.height(), pixels: pm.take() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> Vec<u8> {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5" viewBox="0 0 10 5">{body}</svg>"#
        )
        .into_bytes()
    }

    #[test]
    fn plain_svg_becomes_png_at_scale() {
        let out = svg_to_png(&svg(r##"<rect width="10" height="5" fill="#ff0000"/>"##), 2.0).unwrap();
        assert!(out.png.starts_with(b"\x89PNG"));
        assert_eq!((out.width, out.height), (10.0, 5.0));
        let decoded = resvg::tiny_skia::Pixmap::decode_png(&out.png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (20, 10));
        assert_eq!(decoded.pixel(5, 5).map(|p| p.red()), Some(255));
    }

    #[test]
    fn hostile_or_odd_svg_is_refused() {
        assert!(svg_to_png(b"", 1.0).is_none());
        assert!(svg_to_png(b"not svg", 1.0).is_none());
        assert!(svg_to_png(&[0x1f, 0x8b, 1, 2], 1.0).is_none());
        assert!(
            svg_to_png(&svg(r#"<image href="file:///etc/passwd" width="4" height="4"/>"#), 1.0).is_none()
        );
        assert!(svg_to_png(&svg(r#"<rect width="4" height="4" style="fill:red"/>"#), 1.0).is_none());
        assert!(
            svg_to_png(b"<!DOCTYPE svg [<!ENTITY a 'x'>]><svg xmlns='http://www.w3.org/2000/svg'/>", 1.0)
                .is_none()
        );
        let huge = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'>{}</svg>",
            "<g/>".repeat(30_000)
        );
        assert!(svg_to_png(huge.as_bytes(), 1.0).is_none());
        // A giant declared size is drawn no larger than the cap.
        let big = b"<svg xmlns='http://www.w3.org/2000/svg' width='100000' height='10'><rect width='5' height='5'/></svg>";
        let out = svg_to_png(big, 4.0).unwrap();
        let decoded = resvg::tiny_skia::Pixmap::decode_png(&out.png).unwrap();
        assert!(decoded.width() <= MAX_SVG_SIDE as u32);
        assert!(svg_to_png(&svg(""), f32::NAN).is_some());
    }
}
