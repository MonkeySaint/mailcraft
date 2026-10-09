//! Crate-level tests: import semantics, sanitising, CSS mapping, serialisation round trips, plain-text
//! conversions, remote resources and hostile input.

use crate::*;
use mailcraft_geom::Color;
use mailcraft_model::rich::{Align, Block, Inline, ListKind, ParaStyle, TableCell, TextStyle};
use proptest::prelude::*;

fn doc(html: &str) -> Doc {
    html_to_doc(html)
}

fn inline_plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text { text, .. } => s.push_str(text),
            Inline::Link { children, .. } => s.push_str(&inline_plain(children)),
            Inline::LineBreak => s.push('\n'),
            Inline::Image { alt, .. } => s.push_str(&format!("[img:{alt}]")),
        }
    }
    s
}

/// Text of each top-level paragraph/heading.
fn para_texts(d: &Doc) -> Vec<String> {
    d.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => Some(inline_plain(inlines)),
            _ => None,
        })
        .collect()
}

fn first_para(d: &Doc) -> (&ParaStyle, &[Inline]) {
    for b in &d.blocks {
        if let Block::Paragraph { style, inlines } | Block::Heading { style, inlines, .. } = b {
            return (style, inlines);
        }
    }
    panic!("no paragraph in {d:#?}");
}

/// All text runs in document order (descending into every block kind).
fn runs(d: &Doc) -> Vec<(String, TextStyle)> {
    fn inl(list: &[Inline], out: &mut Vec<(String, TextStyle)>) {
        for i in list {
            match i {
                Inline::Text { text, style } => out.push((text.clone(), style.clone())),
                Inline::Link { children, .. } => inl(children, out),
                _ => {}
            }
        }
    }
    fn blk(list: &[Block], out: &mut Vec<(String, TextStyle)>) {
        for b in list {
            match b {
                Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inl(inlines, out),
                Block::List { items, .. } => items.iter().for_each(|i| blk(i, out)),
                Block::Quote { blocks } | Block::Container { blocks, .. } => blk(blocks, out),
                Block::Table { rows, .. } => rows.iter().flatten().for_each(|c| blk(&c.blocks, out)),
                Block::Pre { .. } | Block::Rule => {}
            }
        }
    }
    let mut out = Vec::new();
    blk(&d.blocks, &mut out);
    out
}

fn run_style(d: &Doc, text: &str) -> TextStyle {
    runs(d)
        .into_iter()
        .find(|(t, _)| t.contains(text))
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("no run {text:?}"))
}

fn assert_round_trip(html: &str) -> Doc {
    let d1 = html_to_doc(html);
    let out = doc_to_html(&d1);
    let d2 = html_to_doc(&out);
    assert_eq!(d1, d2, "round trip changed the document\nHTML out:\n{out}");
    assert_eq!(doc_to_html(&d2), out, "serialisation not stable");
    d1
}

// ---------------------------------------------------------------------------------------------------------
// Import semantics

#[test]
fn inline_formatting() {
    let d = doc(
        "<p>a <b>b</b> <strong>st</strong> <i>i</i> <em>em</em> <u>u</u> <s>s</s> <strike>k</strike> \
                 <del>d</del> x<sup>2</sup> H<sub>2</sub>O <code>c</code> <tt>t</tt> <kbd>kb</kbd> <mark>m</mark></p>",
    );
    assert_eq!(run_style(&d, "b").bold, Some(true));
    assert_eq!(run_style(&d, "st").bold, Some(true));
    assert_eq!(run_style(&d, "i").italic, Some(true));
    assert_eq!(run_style(&d, "em").italic, Some(true));
    assert_eq!(run_style(&d, "u").underline, Some(true));
    for t in ["s", "k", "d"] {
        assert_eq!(runs(&d).iter().find(|(x, _)| x == t).and_then(|(_, s)| s.strike), Some(true), "{t}");
    }
    assert_eq!(run_style(&d, "2").superscript, Some(true));
    assert_eq!(runs(&d).iter().filter(|(_, s)| s.subscript == Some(true)).count(), 1);
    for t in ["c", "t", "kb"] {
        assert_eq!(runs(&d).iter().find(|(x, _)| x == t).and_then(|(_, s)| s.monospace), Some(true), "{t}");
    }
    assert_eq!(
        runs(&d).iter().find(|(x, _)| x == "m").and_then(|(_, s)| s.highlight),
        Some(Color::rgb(255, 255, 0))
    );
    assert_eq!(para_texts(&d), vec!["a b st i em u s k d x2 H2O c t kb m"]);
}

#[test]
fn whitespace_collapsing() {
    let d = doc("<div>  a \n\t b  <b> c </b> d   </div>\n\n<div>\n  e&nbsp;&nbsp;f  </div>");
    assert_eq!(para_texts(&d), vec!["a b c d", "e\u{a0}\u{a0}f"]);
    let r = runs(&d);
    assert_eq!(r.iter().find(|(_, s)| s.bold == Some(true)).map(|(t, _)| t.as_str()), Some("c "));
}

#[test]
fn line_breaks_and_empty_paragraphs() {
    let d = doc("<div>a <br> b<br></div><div><br></div><p></p><div>  </div><div>x<br><br></div>");
    assert_eq!(d.blocks.len(), 3, "{:#?}", d.blocks);
    let Block::Paragraph { inlines, .. } = &d.blocks[0] else { panic!() };
    assert_eq!(inline_plain(inlines), "a\nb");
    assert_eq!(d.blocks[1], Block::Paragraph { style: ParaStyle::default(), inlines: vec![] });
    let Block::Paragraph { inlines, .. } = &d.blocks[2] else { panic!() };
    assert_eq!(inline_plain(inlines), "x\n");
}

#[test]
fn paragraph_margins_and_alignment() {
    let d = doc("<p>plain</p><p style='margin:0'>tight</p><p align=center>c</p><center>cc</center>\
                 <div style='text-align:right;margin-left:20px;padding-left:1em'>r</div>\
                 <div style='text-align:justify'><p style='margin:4px 0 6px 0'>j</p></div>");
    let styles: Vec<&ParaStyle> = d
        .blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph { style, .. } => style,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(styles[0].space_after_px, 16.0);
    assert_eq!(styles[0].space_before_px, 0.0);
    assert_eq!(styles[1].space_after_px, 0.0);
    assert_eq!(styles[2].align, Align::Center);
    assert_eq!(styles[3].align, Align::Center);
    assert_eq!(styles[4].align, Align::End);
    assert_eq!(styles[4].indent_px, 36.0);
    assert_eq!(styles[5].align, Align::Justify);
    assert_eq!((styles[5].space_before_px, styles[5].space_after_px), (4.0, 6.0));
}

#[test]
fn headings_rule_quote_pre() {
    let d = doc("<h1>One</h1><h3 style='text-align:center'>Three</h3><hr><blockquote><p>quoted</p>\
                 <blockquote>nested</blockquote></blockquote><pre>\n  code\n    more &lt;x&gt;\n</pre>");
    assert!(
        matches!(&d.blocks[0], Block::Heading { level: 1, inlines, .. } if inline_plain(inlines) == "One")
    );
    assert!(matches!(&d.blocks[1], Block::Heading { level: 3, style, .. } if style.align == Align::Center));
    assert_eq!(d.blocks[2], Block::Rule);
    let Block::Quote { blocks } = &d.blocks[3] else { panic!("{:?}", d.blocks[3]) };
    assert_eq!(blocks.len(), 2);
    assert!(matches!(&blocks[1], Block::Quote { blocks } if blocks.len() == 1));
    assert_eq!(d.blocks[4], Block::Pre { text: "  code\n    more <x>\n".into() });
}

#[test]
fn lists() {
    let d = doc(
        "<ul><li>a<li>b<ul><li>b1</li></ul></li></ul><ol start=3><li>three</li><li><p>four</p>tail</li></ol>\
                 <ol start=-5><li>x</ol><ul>stray<li>after</li></ul>",
    );
    let Block::List { kind: ListKind::Bullet, items } = &d.blocks[0] else { panic!() };
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[1][1], Block::List { items, .. } if items.len() == 1));
    let Block::List { kind: ListKind::Numbered { start: 3 }, items } = &d.blocks[1] else { panic!() };
    assert_eq!(items[1].len(), 2);
    assert!(matches!(&d.blocks[2], Block::List { kind: ListKind::Numbered { start: 0 }, .. }));
    let Block::List { items, .. } = &d.blocks[3] else { panic!() };
    assert_eq!(items.len(), 2);
}

#[test]
fn tables() {
    let d = doc("<table border=1 width=600 bgcolor=#eeeeee><tr><th>H1<th colspan=2>H2</tr>\
                 <tr bgcolor=red><td rowspan=0 width=50%>a<td style='background:#00ff00;width:120px'>b<td>c</tr>\
                 junk</table>");
    assert_eq!(para_texts(&d), vec!["junk"]);
    let Block::Table { rows, border, width_px } = &d.blocks[1] else { panic!("{:#?}", d.blocks) };
    assert!(*border);
    assert_eq!(*width_px, Some(600.0));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][1].colspan, 2);
    assert_eq!(rows[0][0].background, Some(Color::hex(0xeeeeee)));
    let h1 = &rows[0][0].blocks[0];
    assert!(matches!(h1, Block::Paragraph { style, inlines }
        if style.align == Align::Center && matches!(&inlines[0], Inline::Text { style, .. } if style.bold == Some(true))));
    assert_eq!(rows[1][0].rowspan, 1);
    assert_eq!(rows[1][0].width_px, None);
    assert_eq!(rows[1][0].background, Some(Color::hex(0xff0000)));
    assert_eq!(rows[1][1].background, Some(Color::hex(0x00ff00)));
    assert_eq!(rows[1][1].width_px, Some(120.0));
    let d = doc("<table border=0><tr><td></td></tr></table><table><tr></tr></table>");
    assert_eq!(d.blocks.len(), 1);
    assert!(matches!(&d.blocks[0], Block::Table { border: false, rows, .. } if rows[0][0].blocks.is_empty()));
}

#[test]
fn links_and_images() {
    let d = doc(
        "<p><a href='https://x.test/a?b=1&amp;c=2'>go <b>now</b></a> <a href='javascript:alert(1)'>bad</a> \
                 <a name=top>anchor</a> <img src='cid:logo@x' alt='Logo' width=120 height='40px'> \
                 <img src='javascript:x'><img src='data:image/png;base64,AA' style='width:10px'>\
                 <img src='data:text/html,x'></p>",
    );
    let (_, inl) = first_para(&d);
    assert!(matches!(&inl[0], Inline::Link { href, children }
        if href == "https://x.test/a?b=1&c=2" && inline_plain(children) == "go now"));
    let images: Vec<&Inline> = inl.iter().filter(|i| matches!(i, Inline::Image { .. })).collect();
    assert_eq!(images.len(), 2);
    assert_eq!(
        images[0],
        &Inline::Image {
            src: "cid:logo@x".into(),
            alt: "Logo".into(),
            width: Some(120.0),
            height: Some(40.0)
        }
    );
    assert!(matches!(images[1], Inline::Image { width: Some(w), .. } if *w == 10.0));
    assert_eq!(inline_plain(inl), "go now bad anchor [img:Logo] [img:]");
    assert!(!inl.iter().any(|i| matches!(i, Inline::Link { href, .. } if href.contains("javascript"))));
}

#[test]
fn legacy_font_and_body() {
    let d = doc("<body bgcolor='#fafafa' text=navy style='font-family:Verdana;font-size:10pt'>\
                 <font face='Arial' size=4 color=red>f</font> <span style='font-size:150%'>big</span></body>");
    assert_eq!(d.background, Some(Color::hex(0xfafafa)));
    assert_eq!(d.base.font_family.as_deref(), Some("Verdana"));
    assert_eq!(d.base.size_pt, Some(10.0));
    assert_eq!(d.base.color, Some(Color::hex(0x000080)));
    let f = run_style(&d, "f");
    assert_eq!(
        (f.font_family.as_deref(), f.size_pt, f.color),
        (Some("Arial"), Some(13.5), Some(Color::hex(0xff0000)))
    );
    assert_eq!(run_style(&d, "big").size_pt, Some(15.0));
}

#[test]
fn css_mapping() {
    let d = doc(r#"<style>
        p.MsoNormal, li.MsoNormal { margin:0cm; font-size:11.0pt; font-family:"Calibri",sans-serif }
        .hl { background-color: yellow } #big { font-size: 24px !important }
        @media only screen and (max-width:600px) { p { color: red !important } }
        span { color: rgb(0, 0, 255) } div p { color: orange }
      </style>
      <p class=MsoNormal>Word <span class=hl>marked</span> <span id=big style="font-size:8px">huge</span></p>
      <p style="font:italic bold 12px/1.5 Georgia, serif; line-height:1.5; text-decoration:underline line-through">f</p>
      <p style="color:#123; font-weight:700; font-style:oblique; vertical-align:super">w</p>
      <div style="font-size:2em"><span style="font-size:50%">half</span></div>
      <p style="line-height:24px;margin-left:1in">lh</p>"#);
    let w = run_style(&d, "Word");
    assert_eq!(w.size_pt, Some(11.0));
    assert_eq!(w.font_family.as_deref(), Some("\"Calibri\",sans-serif"));
    assert_eq!(w.color, None, "@media and descendant rules are ignored");
    let (st, _) = first_para(&d);
    assert_eq!(st.space_after_px, 0.0);
    assert_eq!(run_style(&d, "marked").highlight, Some(Color::rgb(255, 255, 0)));
    assert_eq!(run_style(&d, "marked").color, Some(Color::rgb(0, 0, 255)));
    assert_eq!(run_style(&d, "huge").size_pt, Some(18.0));
    let f = run_style(&d, "f");
    assert_eq!(
        (f.italic, f.bold, f.size_pt, f.font_family.as_deref()),
        (Some(true), Some(true), Some(9.0), Some("Georgia, serif"))
    );
    assert_eq!((f.underline, f.strike), (Some(true), Some(true)));
    let w = run_style(&d, "w");
    assert_eq!((w.color, w.bold, w.italic), (Some(Color::hex(0x112233)), Some(true), Some(true)));
    assert_eq!(w.superscript, None, "vertical-align on a block is not superscript");
    assert_eq!(run_style(&d, "half").size_pt, Some(12.0));
    let lh = d.blocks.iter().rev().find_map(|b| match b {
        Block::Paragraph { style, .. } => Some(style.clone()),
        _ => None,
    });
    let lh = lh.unwrap();
    assert_eq!(lh.line_height, Some(1.5));
    assert_eq!(lh.indent_px, 96.0);
}

#[test]
fn hidden_content_dropped() {
    let d = doc(
        "<div style='display:none;max-height:0'>Preheader text</div><span style='mso-hide:all'>mso</span>\
                 <p hidden>h</p><style>.x{display:none}</style><p class=x>classy</p><p>visible</p>",
    );
    assert_eq!(html_to_text_of(&d), "visible");
}

fn html_to_text_of(d: &Doc) -> String {
    doc_to_text(d)
}

#[test]
fn container_for_backgrounds() {
    let d = doc(
        "<div style='background-color:#ffeeee;padding:10px;margin-bottom:8px;text-align:center'><p>in</p>x</div>\
                 <div style='background:transparent'>plain</div>",
    );
    let Block::Container { style, blocks } = &d.blocks[0] else { panic!("{:#?}", d.blocks) };
    assert_eq!(style.background, Some(Color::hex(0xffeeee)));
    assert_eq!(style.indent_px, 10.0);
    assert_eq!(style.space_after_px, 8.0);
    assert_eq!(blocks.len(), 2);
    assert!(
        matches!(&blocks[1], Block::Paragraph { style, .. } if style.align == Align::Center && style.indent_px == 0.0)
    );
    assert!(matches!(&d.blocks[1], Block::Paragraph { .. }));
}

#[test]
fn white_space_pre_wrap() {
    let d = doc("<div style='white-space:pre-wrap'>a  b\n  c</div><div>d  e</div>");
    // Preserved spaces are stored as alternating no-break spaces (the model has no white-space property).
    assert_eq!(para_texts(&d), vec!["a \u{a0}b\n\u{a0} c", "d e"]);
}

// ---------------------------------------------------------------------------------------------------------
// Sanitising

#[test]
fn dangerous_content_removed() {
    let html = r#"<html><head><meta http-equiv="refresh" content="0;url=http://evil"><base href="http://evil/">
        <link rel=stylesheet href="http://evil/x.css"><title>Title text</title>
        <script>alert('head')</script></head>
        <body onload="alert(1)"><p onclick="alert(2)" style="color:red">safe</p>
        <script type="text/javascript">document.write('<b>pwn</b>')</script>
        <iframe src="http://evil">iframe text</iframe><object data=x>object text</object><embed src=x>
        <form action="http://evil"><input value="secret"><button>Click</button><select><option>opt</select>
        <textarea>area</textarea>form text</form>
        <a href="  jAvAsCrIpT:alert(3)">js link</a> <a href="vbscript:x">vb</a> <a href="data:text/html,x">data</a>
        <img src="x" onerror="alert(4)"><svg><script>alert(5)</script></svg><noscript>no script</noscript>
        <style>p { color: blue }</style></body></html>"#;
    let d = doc(html);
    let text = doc_to_text(&d);
    for bad in
        ["alert", "Title text", "pwn", "iframe text", "object text", "secret", "Click", "opt", "area", "head"]
    {
        assert!(!text.contains(bad), "{bad} leaked: {text}");
    }
    for good in ["safe", "form text", "js link", "vb", "data", "no script"] {
        assert!(text.contains(good), "{good} missing: {text}");
    }
    let out = doc_to_html(&d);
    for bad in [
        "<script",
        "onclick",
        "onload",
        "onerror",
        "javascript",
        "vbscript",
        "iframe",
        "<form",
        "<input",
        "data:text",
        "evil",
        "<meta http",
        "<base",
        "<link",
    ] {
        assert!(!out.to_ascii_lowercase().contains(bad), "{bad} in output:\n{out}");
    }
    assert_eq!(run_style(&d, "safe").color, Some(Color::rgb(255, 0, 0)), "inline style beats the sheet");
}

#[test]
fn serializer_sanitises_hand_built_docs() {
    let d = Doc {
        blocks: vec![Block::Paragraph {
            style: ParaStyle::default(),
            inlines: vec![
                Inline::Link {
                    href: "javascript:alert(1)".into(),
                    children: vec![Inline::Text { text: "x".into(), style: TextStyle::default() }],
                },
                Inline::Image { src: "vbscript:y".into(), alt: "a".into(), width: None, height: None },
                Inline::Text {
                    text: "<script>\"&".into(),
                    style: TextStyle { font_family: Some("A\"B<".into()), ..TextStyle::default() },
                },
            ],
        }],
        ..Doc::default()
    };
    let out = doc_to_html(&d);
    assert!(!out.contains("javascript") && !out.contains("vbscript") && !out.contains("<script"), "{out}");
    assert!(out.contains("&lt;script&gt;\"&amp;"), "{out}");
    assert!(out.contains("font-family:A&quot;B&lt;"), "{out}");
}

// ---------------------------------------------------------------------------------------------------------
// Realistic messages

const NEWSLETTER: &str = r##"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">
<html xmlns="http://www.w3.org/1999/xhtml"><head>
<meta http-equiv="Content-Type" content="text/html; charset=UTF-8"><meta name="viewport" content="width=device-width">
<title>Weekly Digest</title>
<style type="text/css">
  body { margin:0; padding:0; background-color:#f4f4f4; font-family: Helvetica, Arial, sans-serif; }
  .wrapper { width:100%; } .content { background-color:#ffffff; }
  h1 { color:#333333; font-size:28px; } .btn { color:#ffffff; text-decoration:none; }
  .footer { font-size:11px; color:#999999; }
  @media only screen and (max-width: 480px) { .content { width:100% !important; } }
</style>
<!--[if mso]><style>td { font-family: Arial; }</style><![endif]-->
</head>
<body style="margin:0; padding:0;" bgcolor="#f4f4f4">
<div style="display:none;font-size:1px;color:#f4f4f4;max-height:0;overflow:hidden;">Hidden preheader: this week's top stories</div>
<table class="wrapper" width="100%" cellpadding="0" cellspacing="0" border="0" bgcolor="#f4f4f4">
 <tr><td align="center">
  <table class="content" width="600" cellpadding="20" cellspacing="0" border="0">
   <tr><td align="center"><a href="https://news.example.com/?utm=1"><img src="https://cdn.example.com/logo.png" alt="Example News" width="200" height="50" style="display:block;border:0"></a></td></tr>
   <tr><td style="background-image:url('https://cdn.example.com/hero.jpg')" background="https://cdn.example.com/hero-fallback.jpg">
     <h1>This week&rsquo;s highlights</h1>
     <p style="font-size:16px;line-height:24px;color:#555555">Hello reader &amp; friends,<br>here is what happened.</p>
     <ul><li>Story one &mdash; <a href="https://news.example.com/1">read more</a></li><li>Story two</li></ul>
     <table width="100%"><tr><td>Price</td><td>Qty</td></tr><tr><td>&euro;10</td><td>2</td></tr></table>
     <p align="center"><a class="btn" href="https://news.example.com/sub" style="background-color:#0078d4;padding:10px 20px">Subscribe&nbsp;now</a></p>
   </td></tr>
   <tr><td class="footer"><table><tr><td>You received this because you signed up. <a href="https://news.example.com/unsub">Unsubscribe</a><img src="https://track.example.com/open.gif" width="1" height="1" alt=""></td></tr></table></td></tr>
  </table>
 </td></tr>
</table>
</body></html>"##;

#[test]
fn newsletter() {
    let d = doc(NEWSLETTER);
    assert_eq!(d.background, Some(Color::hex(0xf4f4f4)));
    assert_eq!(d.base.font_family.as_deref(), Some("Helvetica, Arial, sans-serif"));
    let text = doc_to_text(&d);
    assert!(!text.contains("Hidden preheader"), "{text}");
    assert!(!text.contains("Weekly Digest"), "{text}");
    assert!(text.contains("This week\u{2019}s highlights"), "{text}");
    assert!(text.contains("Hello reader & friends,\nhere is what happened."), "{text}");
    assert!(text.contains("• Story one — read more\n• Story two"), "{text}");
    assert!(text.contains("Price\tQty\n€10\t2"), "{text}");
    assert!(text.contains("Subscribe now"), "{text}");
    assert!(text.contains("Unsubscribe"), "{text}");
    assert_eq!(run_style(&d, "Subscribe").underline, Some(false));
    assert_eq!(run_style(&d, "Subscribe").color, Some(Color::WHITE));
    assert_eq!(run_style(&d, "Subscribe").highlight, Some(Color::hex(0x0078d4)));
    assert_eq!(run_style(&d, "Hello reader").size_pt, Some(12.0));
    assert_eq!(run_style(&d, "You received").size_pt, Some(8.25));
    assert_eq!(run_style(&d, "highlights").size_pt, Some(21.0));
    let res = remote_resources(NEWSLETTER);
    assert_eq!(
        res,
        vec![
            "https://cdn.example.com/logo.png",
            "https://cdn.example.com/hero-fallback.jpg",
            "https://cdn.example.com/hero.jpg",
            "https://track.example.com/open.gif",
        ]
    );
    assert_round_trip(NEWSLETTER);
}

const OUTLOOK_REPLY: &str = r##"<html><head><style>p.MsoNormal{margin:0cm;font-size:12.0pt;font-family:"Aptos",sans-serif;}</style></head>
<body lang="EN-CA" link="#467886" vlink="#96607D" style="word-wrap:break-word">
<div class="WordSection1">
<p class="MsoNormal">Thanks, see you then.<o:p></o:p></p>
<p class="MsoNormal"><o:p>&nbsp;</o:p></p>
<div style="border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm">
<p class="MsoNormal"><b><span lang="EN-US" style="font-size:11.0pt;font-family:&quot;Calibri&quot;,sans-serif">From:</span></b><span lang="EN-US" style="font-size:11.0pt"> Jane Doe &lt;jane@example.com&gt; <br><b>Sent:</b> Monday, October 5, 2026 9:14 AM<br><b>To:</b> Sam &lt;sam@example.com&gt;<br><b>Subject:</b> Lunch?</span></p>
</div>
<p class="MsoNormal"><o:p>&nbsp;</o:p></p>
<p class="MsoNormal">Are you free <i>Tuesday</i>?</p>
</div></body></html>"##;

const OUTLOOK_REPLY_HR: &str = r##"<div dir="ltr"><div>Sounds good.</div><div><br></div>
<hr style="display:inline-block;width:98%" tabindex="-1">
<div id="divRplyFwdMsg" dir="ltr"><font face="Calibri, sans-serif" style="font-size:11pt" color="#000000"><b>From:</b> Jane Doe &lt;jane@example.com&gt;<br>
<b>Sent:</b> Monday, October 5, 2026 9:14 AM<br><b>To:</b> Sam<br><b>Subject:</b> Lunch?</font>
<div>&nbsp;</div></div><div>Are you free?</div></div>"##;

#[test]
fn outlook_reply_with_border_separator() {
    let d = doc(OUTLOOK_REPLY);
    assert!(d.blocks.contains(&Block::Rule), "{:#?}", d.blocks);
    let from = run_style(&d, "From:");
    assert_eq!(from.bold, Some(true));
    assert_eq!(from.font_family.as_deref(), Some("\"Calibri\",sans-serif"));
    assert_eq!(run_style(&d, "Sent:").bold, Some(true));
    assert_ne!(run_style(&d, "Jane Doe").bold, Some(true));
    assert_eq!(run_style(&d, "Thanks").size_pt, Some(12.0));
    let text = doc_to_text(&d);
    assert_eq!(
        text,
        "Thanks, see you then.\n\n------------------------------\nFrom: Jane Doe <jane@example.com>\n\
         Sent: Monday, October 5, 2026 9:14 AM\nTo: Sam <sam@example.com>\nSubject: Lunch?\n\nAre you free Tuesday?"
    );
    assert_round_trip(OUTLOOK_REPLY);
}

#[test]
fn outlook_reply_with_hr() {
    let d = doc(OUTLOOK_REPLY_HR);
    let i = d.blocks.iter().position(|b| *b == Block::Rule).expect("rule");
    assert_eq!(
        para_texts(&Doc { blocks: d.blocks[..i].to_vec(), ..Doc::default() }),
        vec!["Sounds good.", ""]
    );
    let labels: Vec<String> =
        runs(&d).into_iter().filter(|(_, s)| s.bold == Some(true)).map(|(t, _)| t).collect();
    assert_eq!(labels, vec!["From:", "Sent:", "To:", "Subject:"]);
    assert_eq!(run_style(&d, "From:").size_pt, Some(11.0));
    let text = doc_to_text(&d);
    assert_eq!(
        text,
        "Sounds good.\n\n------------------------------\nFrom: Jane Doe <jane@example.com>\n\
         Sent: Monday, October 5, 2026 9:14 AM\nTo: Sam\nSubject: Lunch?\n\nAre you free?"
    );
    assert_round_trip(OUTLOOK_REPLY_HR);
}

// ---------------------------------------------------------------------------------------------------------
// Round trips

#[test]
fn round_trips() {
    for html in [
        "",
        "plain text only",
        "<p>a <b>b <i>bi</i></b> <span style='color:red;font-size:9pt'>c</span></p>",
        "<p style='text-align:center;line-height:1.15;background:#eee'>x</p><p>y</p>",
        "<div style='text-align:center'><p>c</p><p style='text-align:left'>l</p></div>",
        "<div style='background:#fee;text-align:right'><p>r</p><p style='text-align:left'>l</p></div>",
        "<ul><li>a</li><li><p>b</p><ol start=4><li>c</li></ol></li></ul>",
        "<blockquote><p>q</p><blockquote>qq</blockquote></blockquote>",
        "<pre>\n\nleading newlines\n  kept</pre>",
        "<table border=1 width=400><tr><td colspan=2 bgcolor=red>a</td></tr><tr><th>h</th><td width=50>b</td></tr></table>",
        "<div>a<br><br>b<br><br></div><div><br></div>",
        "<p>&nbsp;</p><p>a&nbsp; &nbsp;b</p>",
        "<p><a href='https://x/?a=1&amp;b=2'>link <b>bold</b></a> <img src='cid:x' alt='A \"q\"' width=10></p>",
        "<p><span style='text-decoration:none'><u>u only</u></span> <span style='font-weight:normal'>n</span></p>",
        "<h2 style='margin-left:20px'>h</h2><hr><p style='margin:0 0 0 30px'>indented</p>",
        "<body style='font-family:Aptos;font-size:12pt;color:#333' bgcolor=white><p>base</p></body>",
        "<p>x<sup>2</sup> <span style='vertical-align:baseline'>b</span> <code>c</code></p>",
        "<div style='white-space:pre'>  two  spaces\n\tand a tab   </div><p>x</p>",
        "<p style='font-size:13.333333px'>odd size</p>",
    ] {
        assert_round_trip(html);
    }
}

#[test]
fn doc_to_html_shape() {
    let d = Doc {
        blocks: vec![
            Block::Heading {
                level: 9,
                style: ParaStyle::default(),
                inlines: vec![Inline::Text { text: "T".into(), style: TextStyle::default() }],
            },
            Block::Paragraph {
                style: ParaStyle { align: Align::Center, space_after_px: 12.0, ..ParaStyle::default() },
                inlines: vec![Inline::Text {
                    text: "  two  spaces ".into(),
                    style: TextStyle { bold: Some(true), size_pt: Some(14.0), ..TextStyle::default() },
                }],
            },
            Block::Table {
                rows: vec![vec![TableCell {
                    blocks: vec![],
                    colspan: 2,
                    rowspan: 1,
                    background: Some(Color::rgb(1, 2, 3)),
                    width_px: Some(80.0),
                }]],
                border: true,
                width_px: None,
            },
        ],
        base: TextStyle { font_family: Some("Aptos".into()), size_pt: Some(12.0), ..TextStyle::default() },
        background: None,
    };
    let out = doc_to_html(&d);
    assert!(out.starts_with("<!DOCTYPE html>"));
    assert!(out.contains("<body style=\"font-family:Aptos;font-size:12pt\">"), "{out}");
    assert!(out.contains("<h6 style=\"margin:0\">T</h6>"), "{out}");
    assert!(out.contains("<p style=\"margin:0 0 12px 0;text-align:center\"><span style=\"font-size:14pt\"><b>&nbsp; two &nbsp;spaces </b></span></p>"), "{out}");
    assert!(
        out.contains(
            "<td colspan=\"2\" style=\"vertical-align:top;background-color:#010203;width:80px\"></td>"
        ),
        "{out}"
    );
    assert!(!out.contains("<style"), "{out}");
    let back = html_to_doc(&out);
    assert_eq!(back.base, d.base);
    assert!(matches!(&back.blocks[0], Block::Heading { level: 6, .. }));
    assert!(matches!(&back.blocks[1], Block::Paragraph { style, inlines }
        if style.align == Align::Center && style.space_after_px == 12.0 && inline_plain(inlines) == "\u{a0} two \u{a0}spaces"));
}

// ---------------------------------------------------------------------------------------------------------
// Plain text

#[test]
fn html_to_text_rules() {
    let t = html_to_text(
        "<h1>Title</h1><p>First&nbsp;para with <a href='https://x'>a link</a>.</p><p>Second</p>\
         <div>one</div><div>two</div><div><br></div><div><br></div><div><br></div><div>after blanks</div>\
         <ul><li>apple</li><li>pear<ol start=9><li>nine</li><li>ten</li></ol></li></ul><hr>\
         <table><tr><td>a</td><td></td><td>c</td></tr><tr><td><p>multi</p><p>line</p></td><td>x</td></tr></table>\
         <blockquote>quoted</blockquote><pre>  pre\n  text</pre>",
    );
    assert_eq!(
        t,
        "Title\nFirst para with a link.\n\nSecond\n\none\ntwo\n\nafter blanks\n• apple\n• pear\n  9. nine\n  10. ten\n\
         ------------------------------\na\t\tc\nmulti\nline\nx\nquoted\n  pre\n  text"
    );
    assert_eq!(html_to_text(""), "");
    assert_eq!(html_to_text("<p></p><p> </p>"), "");
    assert_eq!(html_to_text("a &lt;b&gt; &amp; c"), "a <b> & c");
}

#[test]
fn text_html_text_round_trip() {
    let src =
        "Hello,\n\nPlease see https://example.com/report?id=7 and mail ops@example.org.\nThanks\n\n-- \nSam";
    let html = text_to_html(src);
    assert!(html.contains("<a href=\"https://example.com/report?id=7\">"), "{html}");
    assert!(html.contains("<a href=\"mailto:ops@example.org\">ops@example.org</a>."), "{html}");
    assert_eq!(
        html_to_text(&html),
        "Hello,\n\nPlease see https://example.com/report?id=7 and mail ops@example.org.\nThanks\n\n--\nSam"
    );
    let d = html_to_doc(&text_to_html("go to www.example.com now"));
    let (_, inl) = first_para(&d);
    assert!(inl.iter().any(|i| matches!(i, Inline::Link { href, .. } if href == "http://www.example.com")));
}

// ---------------------------------------------------------------------------------------------------------
// Remote resources

#[test]
fn remote_resource_detection() {
    let html = r#"<style>@import "https://s.example/a.css"; .x { background: url(//cdn.example/b.png) }</style>
        <link rel="Stylesheet" href="https://s.example/c.css"><link rel=icon href="https://s.example/fav.ico">
        <img src="cid:inline1"><img src="data:image/gif;base64,R0lG"><img src="relative.png">
        <img src=" HTTPS://img.example/d.png " srcset="https://img.example/d2.png 2x, https://img.example/d3.png 3x">
        <td background="http://bg.example/e.jpg"><div style="background-image:url(&quot;https://bg.example/f.png&quot;)">
        <video poster="https://v.example/p.jpg"></video><script src="https://evil.example/x.js"></script>
        <img src="https://img.example/d.png"><input type=image src="https://btn.example/g.png">
        <a href="https://not-a-resource.example/">link</a>"#;
    assert_eq!(
        remote_resources(html),
        vec![
            "https://s.example/a.css",
            "//cdn.example/b.png",
            "https://s.example/c.css",
            "HTTPS://img.example/d.png",
            "https://img.example/d2.png",
            "https://img.example/d3.png",
            "http://bg.example/e.jpg",
            "https://bg.example/f.png",
            "https://v.example/p.jpg",
            "https://img.example/d.png",
            "https://btn.example/g.png",
        ]
    );
    assert!(remote_resources("<p>no images</p>").is_empty());
}

// ---------------------------------------------------------------------------------------------------------
// Hostile input

#[test]
fn deep_nesting() {
    for tag in
        ["div", "b", "blockquote", "ul", "li", "table", "td", "span", "a href=x", "font size=7", "p", "h1"]
    {
        let name = tag.split(' ').next().unwrap();
        let html =
            format!("{}deep{}", format!("<{tag}>").repeat(10_000), format!("</{name}>").repeat(10_000));
        let d = html_to_doc(&html);
        assert!(doc_to_text(&d).contains("deep"), "{tag}");
        let _ = doc_to_html(&d);
        let _ = html_to_doc(&doc_to_html(&d));
    }
    let mixed = "<div><table><tr><td><ul><li><blockquote><b><i>".repeat(3_000);
    let d = html_to_doc(&(mixed + "x"));
    assert!(doc_to_text(&d).contains('x'));
}

#[test]
fn unterminated_everything() {
    for html in [
        "<",
        "<a",
        "<a href",
        "<a href=",
        "<a href=\"",
        "<a href='x",
        "</",
        "</a",
        "<!",
        "<!-",
        "<!--",
        "<!-- x",
        "<!DOCTYPE",
        "<![CDATA[",
        "<![CDATA[x",
        "<?",
        "&",
        "&#",
        "&#x",
        "&#x110000",
        "&amp",
        "<script>",
        "<script>x",
        "<style>p{color:red",
        "<style>@media{",
        "<p style='color:",
        "<p style=\"font:",
        "<table><tr><td",
        "<ul><li>",
        "<textarea>",
        "<title>x",
        "<plaintext>",
        "<b><i><u><s>",
        "<p>a<p>b<p>c",
        "<select><option>",
        "<img src=",
        "<div style=\"background:url(",
        "<x y=\"1\" z='2' w=3 /",
        "<p>&#9999999999999999999999;",
    ] {
        let d = html_to_doc(html);
        let _ = doc_to_html(&d);
        let _ = html_to_text(html);
        let _ = remote_resources(html);
        let _ = text_to_html(html);
    }
    assert_eq!(html_to_text("text<div class=\"x"), "text");
    assert_eq!(html_to_text("a<!-- never closed"), "a");
    assert_eq!(html_to_text("<b>bold<i>both"), "boldboth");
}

#[test]
fn misnested_markup() {
    let d = doc("<p><b>1<i>2</b>3</i>4</p>");
    let r = runs(&d);
    let get = |t: &str| r.iter().find(|(x, _)| x == t).map(|(_, s)| (s.bold, s.italic));
    assert_eq!(get("1"), Some((Some(true), None)));
    assert_eq!(get("2"), Some((Some(true), Some(true))));
    assert_eq!(get("3"), Some((None, Some(true))));
    assert_eq!(get("4"), Some((None, None)));
}

#[test]
fn large_inputs() {
    let big = "<p>word &amp; <b>bold</b></p>".repeat(50_000);
    let d = html_to_doc(&big);
    assert_eq!(d.blocks.len(), 50_000);
    let over = "a".repeat(MAX_INPUT_BYTES + 100);
    let d = html_to_doc(&over);
    assert_eq!(inline_plain(first_para(&d).1).len(), MAX_INPUT_BYTES);
    let multibyte = "é".repeat(MAX_INPUT_BYTES / 2 + 3);
    let _ = html_to_doc(&multibyte);
    let attrs: String = (0..100_000).map(|i| format!(" a{i}=\"{i}\"")).collect();
    let _ = html_to_doc(&format!("<p{attrs}>x</p>"));
    let css = format!(
        "<style>{}</style><p class=c5>x</p>",
        (0..20_000).map(|i| format!(".c{i}{{color:red}}")).collect::<String>()
    );
    assert_eq!(run_style(&html_to_doc(&css), "x").color, Some(Color::rgb(255, 0, 0)));
}

#[test]
fn hostile_workloads_finish_quickly() {
    let start = std::time::Instant::now();
    let _ = html_to_doc(&"&abcdefghijklmnopqrstuvwxyzabcdef".repeat(60_000));
    let _ = html_to_doc(&"&#99999999999999999999".repeat(60_000));
    let sheet: String = (0..10_000).map(|i| format!("*{{color:red;x{i}:1}} .a{{margin:{i}px}} ")).collect();
    let _ = html_to_doc(&format!("<style>{sheet}</style>{}", "<p class='a a a a'>x".repeat(50_000)));
    let _ = html_to_doc(&"<b><i></b></i>".repeat(100_000));
    let _ = html_to_doc(&"<table><tr><td>".repeat(50_000));
    let _ = html_to_doc(&"<<<<<<<<</</</<!<!--".repeat(100_000));
    let _ = remote_resources(&"<img src=http://a/".repeat(100_000));
    let _ = text_to_html(&"http://a.b/ x@y.zz www.q.rs ".repeat(50_000));
    assert!(start.elapsed().as_secs() < 60, "{:?}", start.elapsed());
}

#[test]
fn script_injection_variants() {
    for html in [
        "<scr<script>ipt>alert(1)</script>",
        "<script>alert(1)<!--</script>-->",
        "<SCRIPT SRC=//evil/x.js></SCRIPT>",
        "<img src=x onerror=alert(1)//",
        "<a href=\"jav&#x09;ascript:alert(1)\">x</a>",
        "<a href=\"&#106;&#97;&#118;&#97;&#115;&#99;&#114;&#105;&#112;&#116;&#58;alert(1)\">x</a>",
        "<a href=\"javascript&colon;alert(1)\">x</a>",
        "<div style=\"background:url(javascript:alert(1))\">x</div>",
        "<style>*{x:expression(alert(1))}</style>x",
        "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>",
        "<svg><a xlink:href=\"javascript:alert(1)\"><text>t</text></a></svg>",
        "<iframe srcdoc=\"<script>alert(1)</script>\"></iframe>",
        "<object data=\"javascript:alert(1)\"></object>",
        "<form><button formaction=javascript:alert(1)>x</button></form>",
        "<meta http-equiv=refresh content=\"0;javascript:alert(1)\">",
    ] {
        let out = doc_to_html(&html_to_doc(html)).to_ascii_lowercase();
        assert!(
            !out.contains("javascript") && !out.contains("<script") && !out.contains("onerror"),
            "{html} → {out}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------
// Property tests

fn html_soup() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("<p>".to_string()),
        Just("</p>".to_string()),
        Just("<div>".to_string()),
        Just("</div>".to_string()),
        Just("<b>".to_string()),
        Just("</b>".to_string()),
        Just("<i>".to_string()),
        Just("</i>".to_string()),
        Just("<br>".to_string()),
        Just("<hr>".to_string()),
        Just("<ul>".to_string()),
        Just("<ol start=2>".to_string()),
        Just("<li>".to_string()),
        Just("</ul>".to_string()),
        Just("<table border=1>".to_string()),
        Just("<tr>".to_string()),
        Just("<td colspan=2>".to_string()),
        Just("<th>".to_string()),
        Just("</table>".to_string()),
        Just("<blockquote>".to_string()),
        Just("</blockquote>".to_string()),
        Just("<pre>".to_string()),
        Just("</pre>".to_string()),
        Just("<h2>".to_string()),
        Just("</h2>".to_string()),
        Just("<a href='https://x/'>".to_string()),
        Just("</a>".to_string()),
        Just("<img src='cid:a' alt=x width=3>".to_string()),
        Just("<span style='color:red;font-size:9pt'>".to_string()),
        Just("</span>".to_string()),
        Just("<div style='background:#eee;text-align:center;margin:3px'>".to_string()),
        Just("<p style='margin-left:7px;line-height:1.2'>".to_string()),
        Just("<script>x</script>".to_string()),
        Just("<style>.c{color:blue}</style>".to_string()),
        Just("<font face=Arial size=2>".to_string()),
        Just("<span style='font-size:1.3em;text-decoration:none;font-weight:normal'>".to_string()),
        Just("<u style='text-decoration:none'>".to_string()),
        Just("<td style='background-color:rgba(0,0,0,0.5);width:33.3px' rowspan=3>".to_string()),
        Just("<p style='font-size:10pt;margin:1.5em 0'>".to_string()),
        Just("<div style='border-top:solid #ccc 1pt;padding-left:2mm'>".to_string()),
        Just("<div style='white-space:pre-wrap'>".to_string()),
        Just("<center>".to_string()),
        Just("<sup>".to_string()),
        Just("<code>".to_string()),
        Just("<mark>".to_string()),
        Just("<a href='javascript:x'>".to_string()),
        Just("<li style='margin-left:9px'>".to_string()),
        Just("<td align=right>".to_string()),
        Just("<p style='display:none'>".to_string()),
        Just("&nbsp;".to_string()),
        Just("\t".to_string()),
        Just("  ".to_string()),
        Just("&amp;".to_string()),
        Just(" ".to_string()),
        Just("\n".to_string()),
        "[a-z]{1,6}",
        "[ -~]{0,8}",
    ];
    prop::collection::vec(piece, 0..60).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn never_panics_on_arbitrary_strings(s in "\\PC{0,400}") {
        let d = html_to_doc(&s);
        let _ = doc_to_html(&d);
        let _ = html_to_text(&s);
        let _ = text_to_html(&s);
        let _ = remote_resources(&s);
        let _ = parse_css_color(&s);
    }

    #[test]
    fn never_panics_on_markup_like_bytes(s in "[<>/!\\-=\"' a-z&#;:\\[\\]{}()0-9\n\t\u{e9}\u{1F600}]{0,300}") {
        let d = html_to_doc(&s);
        let _ = html_to_doc(&doc_to_html(&d));
        let _ = html_to_text(&s);
        let _ = remote_resources(&s);
    }

    #[test]
    fn round_trip_is_stable(s in html_soup()) {
        let d1 = html_to_doc(&s);
        let out = doc_to_html(&d1);
        let d2 = html_to_doc(&out);
        prop_assert_eq!(&d1, &d2, "html out: {}", out);
    }

    #[test]
    fn text_to_html_preserves_text(s in "[ -~\n]{0,200}") {
        let html = text_to_html(&s);
        let back = html_to_text(&html);
        let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        prop_assert_eq!(norm(&back), norm(&s));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Regression tests from review: hostile workloads that used to be quadratic or amplify memory

fn timed<T>(label: &str, limit_ms: u128, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let out = f();
    let ms = start.elapsed().as_millis();
    assert!(ms < limit_ms, "{label} took {ms} ms");
    out
}

#[test]
fn autolink_failed_prefix_is_not_rescanned() {
    // Every "http://(" starts a candidate URL that fails; each attempt used to scan to the end of the line.
    let line = "http://(".repeat(200_000);
    let html = timed("failed prefixes", 2_000, || text_to_html(&line));
    assert!(!html.contains("<a "));
}

#[test]
fn autolink_trailing_brackets_are_linear() {
    let line = format!("http://a.b/{}", ")".repeat(200_000));
    let html = timed("trailing brackets", 2_000, || text_to_html(&line));
    assert!(html.contains("<a href=\"http://a.b/\">"), "{}", html.get(..200).unwrap_or(""));
    // Balanced brackets inside the URL are still kept.
    assert!(text_to_html("(see http://x.org/A_(b)).").contains("href=\"http://x.org/A_(b)\""));
}

#[test]
fn list_item_with_many_leading_blank_lines_is_linear() {
    let html = format!("<ul><li>{}x</li></ul>", "<p style=\"margin-top:10px\"><br></p>".repeat(100_000));
    let text = timed("leading blanks", 3_000, || html_to_text(&html));
    assert_eq!(text, "\u{2022} x");
}

#[test]
fn many_classes_on_one_element_are_bounded() {
    let rules: String = (0..64).map(|i| format!(".a{{margin-left:{i}px}}")).collect();
    let html = format!("<style>{rules}</style><p class=\"{}\">x</p>", "a ".repeat(2_000_000));
    let d = timed("class explosion", 3_000, || html_to_doc(&html));
    assert_eq!(first_para(&d).0.indent_px, 63.0);
}

#[test]
fn heavy_cascade_is_bounded() {
    let decls = "color:red;background:red;background-color:red;font-family:a;font-size:1px;\
                 font-weight:bold;font-style:italic;text-decoration:none;text-align:left;margin:0;\
                 margin-left:0;padding:0;padding-left:0;line-height:1;width:1px;height:1px;border:0";
    let mut css = String::new();
    for k in 0..40 {
        for _ in 0..64 {
            css.push_str(&format!(".c{k}{{{decls}}}"));
        }
    }
    let classes: String = (0..40).map(|k| format!("c{k} ")).collect();
    let body = format!("<b class=\"{classes}\"></b>").repeat(20_000);
    let html = format!("<style>{css}</style>{body}<p class=c3>x</p>");
    let d = timed("heavy cascade", 5_000, || html_to_doc(&html));
    assert_eq!(run_style(&d, "x").color, Some(Color::rgb(255, 0, 0)));
}

#[test]
fn misnested_reopen_does_not_amplify_large_attributes() {
    // 250 open formatting elements, then a span with a 4 MB attribute; each outer end tag re-opens a clone
    // of the span.
    let big = "x".repeat(4 * 1024 * 1024);
    let mut html = "<b>".repeat(250);
    html.push_str(&format!("<span title=\"{big}\" style=\"color:red\">"));
    html.push_str(&"</b>y".repeat(250));
    let d = timed("reopen amplification", 5_000, || crate::dom::parse(&html));
    let attr_bytes: usize = d
        .nodes
        .iter()
        .filter_map(|n| match &n.data {
            crate::dom::NodeData::Element { attrs, .. } => {
                Some(attrs.iter().map(|(a, v)| a.len() + v.len()).sum::<usize>())
            }
            _ => None,
        })
        .sum();
    assert!(attr_bytes <= 64 * 1024 * 1024, "{attr_bytes} attribute bytes");
}

#[test]
fn long_link_targets_do_not_amplify() {
    // One long href spread over many paragraphs used to be copied into every paragraph's Link inline.
    let href = format!("https://x.example/{}", "a".repeat(4 * 1024 * 1024));
    let html = format!("<a href=\"{href}\">{}</a>", "<div>x</div>".repeat(200_000));
    let d = timed("link amplification", 10_000, || html_to_doc(&html));
    let mut href_bytes = 0usize;
    for b in &d.blocks {
        if let Block::Paragraph { inlines, .. } = b {
            for i in inlines {
                if let Inline::Link { href, .. } = i {
                    href_bytes += href.len();
                }
            }
        }
    }
    assert!(href_bytes <= 64 * 1024 * 1024, "{href_bytes} href bytes");
    assert_eq!(d.blocks.len(), 200_000);
    // Ordinary documents keep their links.
    let d = doc("<a href='https://e.example/'><div>a</div><div>b</div></a>");
    assert!(d.blocks.iter().all(|b| matches!(b, Block::Paragraph { inlines, .. } if matches!(inlines.first(), Some(Inline::Link { .. })))));
}

#[test]
fn long_font_family_does_not_amplify() {
    let face = "f".repeat(4 * 1024 * 1024);
    let html = format!("<font face=\"{face}\">{}</font>", "<b>x</b><i>y</i>".repeat(100_000));
    let d = timed("font amplification", 10_000, || html_to_doc(&html));
    let total: usize = runs(&d).iter().map(|(_, s)| s.font_family.as_ref().map_or(0, String::len)).sum();
    assert!(total <= 64 * 1024 * 1024, "{total} font-family bytes");
}

#[test]
fn font_family_cannot_inject_css_into_output() {
    let d = doc("<font face=\"x;background-image:url(http://evil.example/t.gif)\">hi</font>");
    let out = doc_to_html(&d).to_ascii_lowercase();
    assert!(!out.contains("background-image") && !out.contains("url("), "{out}");
    // Hand-built documents too.
    let d = Doc {
        blocks: vec![Block::Paragraph {
            style: ParaStyle::default(),
            inlines: vec![Inline::Text {
                text: "t".into(),
                style: TextStyle { font_family: Some("a}b{c;color:red\\".into()), ..TextStyle::default() },
            }],
        }],
        base: TextStyle { font_family: Some("Aptos;background:url(x)".into()), ..TextStyle::default() },
        background: None,
    };
    let out = doc_to_html(&d);
    assert!(!out.contains(";color:red") && !out.contains(";background:url"), "{out}");
    assert!(!out.contains('{') && !out.contains('}') && !out.contains('\\'), "{out}");
}

#[test]
fn reopened_formatting_keeps_its_attributes() {
    // The reopen budget only bites on hostile input; ordinary mis-nesting keeps the clone's style.
    let d = doc("<p><b><span style='color:red'>1</b>2</span></p>");
    assert_eq!(run_style(&d, "2").color, Some(Color::rgb(255, 0, 0)));
    assert_eq!(run_style(&d, "2").bold, None);
}

#[test]
fn many_distinct_remote_resources_are_linear() {
    let prefix = "a".repeat(1_000);
    let html: String = (0..12_000).map(|i| format!("<img src=\"http://x.example/{prefix}/{i}\">")).collect();
    let urls = timed("resource dedupe", 3_000, || remote_resources(&html));
    assert_eq!(urls.len(), 10_000);
    assert!(urls.first().is_some_and(|u| u.ends_with("/0")));
}

#[test]
fn deeply_nested_list_text_is_not_reindented_per_level() {
    let lines = "x<br>".repeat(200_000);
    let html = format!("{}{lines}", "<ul><li>".repeat(120));
    let text = timed("nested list text", 5_000, || html_to_text(&html));
    // Indentation stops growing after a few levels instead of costing one copy of every line per level.
    assert!(text.len() < 200_000 * 64, "{} bytes", text.len());
    let second = text.lines().nth(1).unwrap_or("");
    assert!(second.trim() == "x" && second.len() > 2, "{second:?} / {:?}", text.lines().nth(2));
    // Shallow lists still indent continuation lines under their marker.
    assert_eq!(
        html_to_text("<ul><li>a<br>b<ul><li>c<br>d</ul></ul>"),
        "\u{2022} a\n  b\n  \u{2022} c\n    d"
    );
}

#[test]
fn stylesheet_work_is_budgeted_per_document() {
    // Distinct class combinations defeat memoisation; the per-document budget still bounds the work.
    let decls = "color:red;font-size:9pt;margin:0;padding:0;text-align:left;font-weight:bold";
    let css: String = (0..40).flat_map(|k| (0..64).map(move |_| format!(".c{k}{{{decls}}}"))).collect();
    let body: String = (0..20_000)
        .map(|i| {
            let classes: String = (0..40).map(|k| format!("c{k} ")).collect();
            format!("<b class=\"{classes}u{i}\">x</b>")
        })
        .collect();
    let d = timed("budgeted cascade", 10_000, || html_to_doc(&format!("<style>{css}</style>{body}")));
    assert!(!runs(&d).is_empty());
}
