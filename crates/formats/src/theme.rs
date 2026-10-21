//! Office theme files: the DrawingML theme part (ECMA-376 Part 1, §20.1.6 `a:theme`) on its own (`.xml`) or
//! inside an Office Open XML package (`.thmx`, ECMA-376 Part 2 / OPC: a ZIP archive holding the part at
//! `theme/theme/theme1.xml`). Read: the theme name, the twelve colour-scheme slots, the major (heading) and
//! minor (body) Latin fonts and the format scheme's name. Written: a package with those values and a plain
//! format scheme, so a theme saved by MailCraft opens in other programs that read theme files.
//!
//! Everything read is hostile: the archive, entry and XML sizes are capped, only stored and deflated entries
//! are read, the XML is parsed without DTDs and with a node limit, and nothing panics.

use std::fmt::Write as _;

/// Largest theme file read.
pub const MAX_THEME_FILE: usize = 16 * 1024 * 1024;
/// Largest theme part read (after decompression).
pub const MAX_THEME_XML: usize = 4 * 1024 * 1024;
/// Most archive entries looked at.
const MAX_ENTRIES: usize = 10_000;
/// Most XML nodes parsed.
const MAX_NODES: u32 = 200_000;

/// The colour-scheme slots in file order (dark 1, light 1, dark 2, light 2, accents 1–6, hyperlink, followed
/// hyperlink), which is also MailCraft's palette order.
pub const COLOR_SLOTS: [&str; 12] = [
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink",
    "folHlink",
];

/// What MailCraft uses from a theme file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfficeTheme {
    pub name: String,
    /// `#rrggbb` per [`COLOR_SLOTS`] entry.
    pub colors: Vec<String>,
    /// Heading (major) and body (minor) Latin typefaces.
    pub heading_font: String,
    pub body_font: String,
    /// The format scheme's name ("Office").
    pub effects: String,
}

/// Why a theme file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThemeError {
    #[error("the theme file is too large")]
    TooBig,
    #[error("the file is not a theme (no theme part found)")]
    NoTheme,
    #[error("the theme package is damaged: {0}")]
    Package(String),
    #[error("the theme is not valid XML: {0}")]
    Xml(String),
    #[error("the theme has no {0}")]
    Missing(&'static str),
}

const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

/// Reads a theme from a `.thmx` package or a theme `.xml` part.
pub fn read_theme(data: &[u8]) -> Result<OfficeTheme, ThemeError> {
    if data.len() > MAX_THEME_FILE {
        return Err(ThemeError::TooBig);
    }
    if data.starts_with(b"PK") {
        let xml = theme_part(data)?;
        return parse_theme_xml(&xml);
    }
    let text = std::str::from_utf8(data.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(data))
        .map_err(|_| ThemeError::NoTheme)?;
    parse_theme_xml(text)
}

fn u16_at(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_le_bytes([*b.first()?, *b.get(1)?])))
}

fn u32_at(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at.checked_add(4)?)?;
    let v = u32::from_le_bytes(<[u8; 4]>::try_from(b).ok()?);
    usize::try_from(v).ok()
}

/// The theme part of a package: the entry `theme/theme/theme1.xml` (or the first `theme/theme/*.xml`).
fn theme_part(zip: &[u8]) -> Result<String, ThemeError> {
    let bad = |m: &str| ThemeError::Package(m.to_owned());
    // End of central directory: signature, then the entry count at +10 and the directory offset at +16.
    let tail_start = zip.len().saturating_sub(65_557);
    let eocd = (tail_start..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip.get(i..i + 4) == Some(b"PK\x05\x06".as_slice()))
        .ok_or_else(|| bad("no central directory"))?;
    let count = u16_at(zip, eocd.saturating_add(10)).ok_or_else(|| bad("short directory"))?.min(MAX_ENTRIES);
    let mut at = u32_at(zip, eocd.saturating_add(16)).ok_or_else(|| bad("short directory"))?;
    let mut best: Option<(bool, usize, usize, usize)> = None;
    for _ in 0..count {
        if zip.get(at..at.saturating_add(4)) != Some(b"PK\x01\x02".as_slice()) {
            return Err(bad("damaged directory entry"));
        }
        let method = u16_at(zip, at.saturating_add(10)).ok_or_else(|| bad("short entry"))?;
        let comp = u32_at(zip, at.saturating_add(20)).ok_or_else(|| bad("short entry"))?;
        let name_len = u16_at(zip, at.saturating_add(28)).ok_or_else(|| bad("short entry"))?;
        let extra = u16_at(zip, at.saturating_add(30)).ok_or_else(|| bad("short entry"))?;
        let comment = u16_at(zip, at.saturating_add(32)).ok_or_else(|| bad("short entry"))?;
        let local = u32_at(zip, at.saturating_add(42)).ok_or_else(|| bad("short entry"))?;
        let name = zip
            .get(at.saturating_add(46)..at.saturating_add(46).saturating_add(name_len))
            .ok_or_else(|| bad("short entry name"))?;
        let name = String::from_utf8_lossy(name).to_ascii_lowercase();
        let name = name.trim_start_matches('/');
        if name.starts_with("theme/theme/") && name.ends_with(".xml") && !name.contains("/_rels/") {
            let first = name == "theme/theme/theme1.xml";
            if best.is_none_or(|b| first && !b.0) {
                best = Some((first, method, comp, local));
            }
        }
        at = at.saturating_add(46).saturating_add(name_len).saturating_add(extra).saturating_add(comment);
    }
    let (_, method, comp, local) = best.ok_or(ThemeError::NoTheme)?;
    if zip.get(local..local.saturating_add(4)) != Some(b"PK\x03\x04".as_slice()) {
        return Err(bad("damaged entry"));
    }
    let name_len = u16_at(zip, local.saturating_add(26)).ok_or_else(|| bad("short entry"))?;
    let extra = u16_at(zip, local.saturating_add(28)).ok_or_else(|| bad("short entry"))?;
    let start = local.saturating_add(30).saturating_add(name_len).saturating_add(extra);
    let raw = zip.get(start..start.saturating_add(comp)).ok_or_else(|| bad("entry runs past the end"))?;
    let bytes = match method {
        0 => {
            if raw.len() > MAX_THEME_XML {
                return Err(ThemeError::TooBig);
            }
            raw.to_vec()
        }
        8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, MAX_THEME_XML)
            .map_err(|_| bad("the theme part cannot be decompressed"))?,
        _ => return Err(bad("unsupported compression")),
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).map(<[u8]>::to_vec).unwrap_or(bytes);
    String::from_utf8(bytes).map_err(|_| bad("the theme part is not UTF-8"))
}

/// Reads a DrawingML theme part.
pub fn parse_theme_xml(xml: &str) -> Result<OfficeTheme, ThemeError> {
    if xml.len() > MAX_THEME_XML {
        return Err(ThemeError::TooBig);
    }
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES };
    let doc =
        roxmltree::Document::parse_with_options(xml, opts).map_err(|e| ThemeError::Xml(e.to_string()))?;
    let root = doc.root_element();
    if !is(&root, "theme") {
        return Err(ThemeError::NoTheme);
    }
    let elements = child(root, "themeElements").ok_or(ThemeError::Missing("theme elements"))?;
    let scheme = child(elements, "clrScheme").ok_or(ThemeError::Missing("colour scheme"))?;
    let mut colors = Vec::with_capacity(12);
    for slot in COLOR_SLOTS {
        let c = child(scheme, slot)
            .and_then(|n| n.children().find_map(|c| color_of(&c)))
            .ok_or(ThemeError::Missing("colour"))?;
        colors.push(c);
    }
    let fonts = child(elements, "fontScheme").ok_or(ThemeError::Missing("font scheme"))?;
    let latin = |which: &str| -> Result<String, ThemeError> {
        child(fonts, which)
            .and_then(|f| child(f, "latin"))
            .and_then(|l| l.attribute("typeface"))
            .map(clean)
            .filter(|t| !t.is_empty())
            .ok_or(ThemeError::Missing("Latin font"))
    };
    let effects =
        child(elements, "fmtScheme").and_then(|f| f.attribute("name")).map(clean).unwrap_or_default();
    let name = root
        .attribute("name")
        .map(clean)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Imported Theme".into());
    Ok(OfficeTheme {
        name,
        colors,
        heading_font: latin("majorFont")?,
        body_font: latin("minorFont")?,
        effects,
    })
}

/// Whether `n` is the DrawingML element `local` (or that name without a namespace).
fn is(n: &roxmltree::Node<'_, '_>, local: &str) -> bool {
    n.is_element()
        && n.tag_name().name() == local
        && n.tag_name().namespace().is_none_or(|ns| ns == DRAWINGML)
}

/// The first child element `local` of `n`.
fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, local: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| is(c, local))
}

/// A name from a file: control characters dropped, at most 64 characters, trimmed.
fn clean(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).take(64).collect();
    t.trim().to_owned()
}

fn hex6(v: &str) -> Option<String> {
    let v = v.trim();
    (v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", v.to_ascii_lowercase()))
}

/// The colour a DrawingML colour element names (`srgbClr`, `sysClr`, `scrgbClr`, `prstClr` for the basic
/// names), ignoring any transforms.
fn color_of(n: &roxmltree::Node<'_, '_>) -> Option<String> {
    if !n.is_element() {
        return None;
    }
    match n.tag_name().name() {
        "srgbClr" => n.attribute("val").and_then(hex6),
        "sysClr" => n.attribute("lastClr").and_then(hex6).or_else(|| match n.attribute("val")? {
            "window" | "highlightText" => Some("#ffffff".into()),
            _ => Some("#000000".into()),
        }),
        "scrgbClr" => {
            // Linear percentages in thousandths; converted with the sRGB transfer curve.
            let ch = |k: &str| -> Option<u8> {
                let v: f64 = n.attribute(k)?.trim().parse().ok()?;
                let lin = (v / 100_000.0).clamp(0.0, 1.0);
                let s = if lin <= 0.003_130_8 { 12.92 * lin } else { 1.055 * lin.powf(1.0 / 2.4) - 0.055 };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some((s * 255.0).round().clamp(0.0, 255.0) as u8)
            };
            Some(format!("#{:02x}{:02x}{:02x}", ch("r")?, ch("g")?, ch("b")?))
        }
        "prstClr" => match n.attribute("val")? {
            "black" => Some("#000000".into()),
            "white" => Some("#ffffff".into()),
            "red" => Some("#ff0000".into()),
            "green" => Some("#008000".into()),
            "blue" => Some("#0000ff".into()),
            "yellow" => Some("#ffff00".into()),
            "gray" | "grey" => Some("#808080".into()),
            _ => None,
        },
        _ => None,
    }
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The theme as a DrawingML theme part (with a plain format scheme: solid fills, thin lines, no effects).
pub fn theme_xml(t: &OfficeTheme) -> String {
    let mut x = String::new();
    let _ = write!(
        x,
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<a:theme xmlns:a=\"{DRAWINGML}\" name=\"{}\">\
         <a:themeElements><a:clrScheme name=\"{}\">",
        esc(&t.name),
        esc(&t.name)
    );
    for (slot, c) in COLOR_SLOTS.iter().zip(t.colors.iter().chain(std::iter::repeat(&String::new()))) {
        let hex = c.trim_start_matches('#');
        let hex = if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) { hex } else { "000000" };
        let _ = write!(x, "<a:{slot}><a:srgbClr val=\"{}\"/></a:{slot}>", hex.to_ascii_uppercase());
    }
    let font = |x: &mut String, tag: &str, face: &str| {
        let _ = write!(
            x,
            "<a:{tag}><a:latin typeface=\"{}\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:{tag}>",
            esc(face)
        );
    };
    let _ = write!(x, "</a:clrScheme><a:fontScheme name=\"{}\">", esc(&t.name));
    font(&mut x, "majorFont", &t.heading_font);
    font(&mut x, "minorFont", &t.body_font);
    let effects = if t.effects.is_empty() { "Office" } else { t.effects.as_str() };
    let _ = write!(x, "</a:fontScheme><a:fmtScheme name=\"{}\">", esc(effects));
    let fill = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>";
    let _ = write!(x, "<a:fillStyleLst>{fill}{fill}{fill}</a:fillStyleLst><a:lnStyleLst>");
    for w in [6350, 12700, 19050] {
        let _ = write!(x, "<a:ln w=\"{w}\">{fill}</a:ln>");
    }
    let eff = "<a:effectStyle><a:effectLst/></a:effectStyle>";
    let _ = write!(
        x,
        "</a:lnStyleLst><a:effectStyleLst>{eff}{eff}{eff}</a:effectStyleLst><a:bgFillStyleLst>{fill}{fill}{fill}</a:bgFillStyleLst>\
         </a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"
    );
    x
}

/// CRC-32 (IEEE 802.3, as ZIP uses it).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A `.thmx` package holding the theme (entries stored, not compressed).
pub fn write_thmx(t: &OfficeTheme) -> Vec<u8> {
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/theme/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>\
        </Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme/theme1.xml\"/>\
        </Relationships>";
    let theme = theme_xml(t);
    let entries: [(&str, &[u8]); 3] = [
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("theme/theme/theme1.xml", theme.as_bytes()),
    ];
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let le16 = |v: usize| u16::try_from(v).unwrap_or(u16::MAX).to_le_bytes();
    let le32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes();
    for (name, data) in entries {
        let offset = out.len();
        let crc = crc32(data).to_le_bytes();
        // Local file header: version 2.0, no flags, stored, no time stamp.
        out.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x21\x00");
        out.extend_from_slice(&crc);
        out.extend_from_slice(&le32(data.len()));
        out.extend_from_slice(&le32(data.len()));
        out.extend_from_slice(&le16(name.len()));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(b"PK\x01\x02\x14\x00\x14\x00\x00\x00\x00\x00\x00\x00\x21\x00");
        central.extend_from_slice(&crc);
        central.extend_from_slice(&le32(data.len()));
        central.extend_from_slice(&le32(data.len()));
        central.extend_from_slice(&le16(name.len()));
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&le32(offset));
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len();
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\x00\x00\x00\x00");
    out.extend_from_slice(&le16(entries.len()));
    out.extend_from_slice(&le16(entries.len()));
    out.extend_from_slice(&le32(central.len()));
    out.extend_from_slice(&le32(cd_offset));
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> OfficeTheme {
        OfficeTheme {
            name: "Harbour & Hills".into(),
            colors: vec![
                "#000000", "#ffffff", "#1f2a44", "#e8e4dc", "#2e75b6", "#c55a11", "#7f7f7f", "#bf9000",
                "#4472c4", "#70ad47", "#0563c1", "#954f72",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            heading_font: "Georgia".into(),
            body_font: "Verdana".into(),
            effects: "Office".into(),
        }
    }

    #[test]
    fn package_round_trips() {
        let t = sample();
        let thmx = write_thmx(&t);
        assert!(thmx.starts_with(b"PK\x03\x04"));
        assert_eq!(read_theme(&thmx).unwrap(), t);
        assert_eq!(read_theme(theme_xml(&t).as_bytes()).unwrap(), t);
    }

    #[test]
    fn reads_system_and_other_colour_kinds() {
        let xml = r#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Sample">
          <a:themeElements><a:clrScheme name="S">
            <a:dk1><a:sysClr val="windowText" lastClr="111111"/></a:dk1><a:lt1><a:sysClr val="window"/></a:lt1>
            <a:dk2><a:srgbClr val="0E2841"><a:lumMod val="50000"/></a:srgbClr></a:dk2><a:lt2><a:prstClr val="white"/></a:lt2>
            <a:accent1><a:scrgbClr r="100000" g="0" b="0"/></a:accent1><a:accent2><a:srgbClr val="E97132"/></a:accent2>
            <a:accent3><a:srgbClr val="196B24"/></a:accent3><a:accent4><a:srgbClr val="0F9ED5"/></a:accent4>
            <a:accent5><a:srgbClr val="A02B93"/></a:accent5><a:accent6><a:srgbClr val="4EA72E"/></a:accent6>
            <a:hlink><a:srgbClr val="467886"/></a:hlink><a:folHlink><a:srgbClr val="96607D"/></a:folHlink>
          </a:clrScheme>
          <a:fontScheme name="F"><a:majorFont><a:latin typeface="Aptos Display"/></a:majorFont><a:minorFont><a:latin typeface="Aptos"/></a:minorFont></a:fontScheme>
          <a:fmtScheme name="Office"/></a:themeElements></a:theme>"#;
        let t = parse_theme_xml(xml).unwrap();
        assert_eq!(t.name, "Sample");
        assert_eq!(&t.colors[..5], ["#111111", "#ffffff", "#0e2841", "#ffffff", "#ff0000"]);
        assert_eq!(
            (t.heading_font.as_str(), t.body_font.as_str(), t.effects.as_str()),
            ("Aptos Display", "Aptos", "Office")
        );
    }

    #[test]
    fn rejects_what_is_not_a_theme() {
        assert_eq!(read_theme(b"<html/>"), Err(ThemeError::NoTheme));
        assert!(matches!(read_theme(b"<a:theme"), Err(ThemeError::Xml(_))));
        assert!(matches!(read_theme(b"PK\x03\x04junk"), Err(ThemeError::Package(_))));
        assert_eq!(read_theme(&[0xff, 0xfe]), Err(ThemeError::NoTheme));
        let no_fonts = theme_xml(&sample()).replace("majorFont", "otherFont");
        assert_eq!(parse_theme_xml(&no_fonts), Err(ThemeError::Missing("Latin font")));
        let dtd = "<!DOCTYPE a [<!ENTITY x \"y\">]><a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"/>";
        assert!(matches!(parse_theme_xml(dtd), Err(ThemeError::Xml(_))));
        // A package without a theme part.
        let mut thmx = write_thmx(&sample());
        let at = thmx.windows(12).rposition(|w| w == b"theme/theme/").unwrap();
        thmx[at] = b'x';
        let first = thmx.windows(12).position(|w| w == b"theme/theme/").unwrap();
        thmx[first] = b'x';
        assert_eq!(read_theme(&thmx), Err(ThemeError::NoTheme));
    }

    #[test]
    fn escapes_names_when_writing() {
        let mut t = sample();
        t.name = "A <b> & \"c\"\u{7}".into();
        let back = read_theme(&write_thmx(&t)).unwrap();
        assert_eq!(back.name, "A <b> & \"c\"");
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn hostile_files_never_panic(data in proptest::collection::vec(any::<u8>(), 0..600), flip in 0usize..2000) {
            let _ = read_theme(&data);
            let mut z = write_thmx(&sample());
            if let Some(b) = z.get_mut(flip) {
                *b = b.wrapping_add(1);
            }
            let _ = read_theme(&z);
            let mut prefixed = b"PK".to_vec();
            prefixed.extend_from_slice(&data);
            let _ = read_theme(&prefixed);
        }
    }
}
