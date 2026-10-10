//! `cargo xtask assets`: every file under `assets/` has a row in ATTRIBUTION.md, icon SVGs are well-formed
//! and self-contained (no external references, no embedded images or scripts), and the generated app icon
//! files exist next to `assets/app-icon/mailcraft.svg`.

use std::path::Path;

use crate::{TaskResult, rel, walk_files};

/// Path patterns from the first column of the ATTRIBUTION.md table (`assets/...`, backticks stripped).
pub fn attribution_rows(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .filter_map(|l| l.trim_start_matches('|').split('|').next())
        .map(|cell| cell.trim().trim_matches('`').trim().to_owned())
        .filter(|cell| cell.starts_with("assets/"))
        .collect()
}

/// Glob match: `*` = any run without `/`, `**` = any run including `/`, `?` = one character except `/`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    // Iterative DP over (pattern index, path index): O(len(p) × len(s)), no recursion.
    let (np, ns) = (p.len(), s.len());
    let mut cur = vec![false; ns.saturating_add(1)];
    if let Some(c) = cur.get_mut(0) {
        *c = true;
    }
    let mut i = 0;
    while i < np {
        let c = p.get(i).copied().unwrap_or('\0');
        let double = c == '*' && p.get(i.saturating_add(1)) == Some(&'*');
        let mut next = vec![false; ns.saturating_add(1)];
        for j in 0..=ns {
            let here = cur.get(j).copied().unwrap_or(false);
            let ch = s.get(j).copied();
            match c {
                '*' => {
                    // `*`/`**` may match empty, or extend a match by one character.
                    let prev =
                        if j > 0 { next.get(j.saturating_sub(1)).copied().unwrap_or(false) } else { false };
                    let prev_ch = if j > 0 { s.get(j.saturating_sub(1)).copied() } else { None };
                    let extend = prev && (double || prev_ch != Some('/'));
                    if let Some(n) = next.get_mut(j) {
                        *n = here || extend;
                    }
                }
                _ => {
                    if here
                        && let Some(ch) = ch
                        && (ch == c || (c == '?' && ch != '/'))
                        && let Some(n) = next.get_mut(j.saturating_add(1))
                    {
                        *n = true;
                    }
                }
            }
        }
        cur = next;
        i = i.saturating_add(if double { 2 } else { 1 });
    }
    cur.get(ns).copied().unwrap_or(false)
}

/// Problems with one SVG file's text (empty = OK).
pub fn check_svg(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let body = text.trim_start_matches('\u{feff}');
    let mut stack: Vec<String> = Vec::new();
    let mut roots = 0usize;
    let mut rest = body;
    let lower = body.to_lowercase();
    if lower.contains("@import") {
        problems.push("uses CSS @import".to_owned());
    }
    for (i, _) in lower.match_indices("url(") {
        let after = lower.get(i.saturating_add(4)..).unwrap_or("").trim_start_matches([' ', '"', '\'']);
        if !after.starts_with('#') {
            problems.push("has a url(...) reference that is not a local #fragment".to_owned());
            break;
        }
    }
    loop {
        let Some(lt) = rest.find('<') else {
            if !rest.trim().is_empty() && stack.is_empty() {
                problems.push("has text outside the root element".to_owned());
            }
            break;
        };
        if stack.is_empty() && !rest.get(..lt).unwrap_or("").trim().is_empty() {
            problems.push("has text outside the root element".to_owned());
        }
        rest = rest.get(lt..).unwrap_or("");
        let skip_to = |rest: &str, open: &str, close: &str| -> Option<usize> {
            rest.starts_with(open).then(|| rest.find(close).map(|e| e.saturating_add(close.len())))?
        };
        if let Some(end) = skip_to(rest, "<?", "?>")
            .or_else(|| skip_to(rest, "<!--", "-->"))
            .or_else(|| skip_to(rest, "<![CDATA[", "]]>"))
        {
            rest = rest.get(end..).unwrap_or("");
            continue;
        }
        if rest.starts_with("<!") {
            problems.push(
                "has a DOCTYPE/ENTITY declaration (not allowed: entities can reference files)".to_owned(),
            );
            match rest.find('>') {
                Some(e) => rest = rest.get(e.saturating_add(1)..).unwrap_or(""),
                None => break,
            }
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!--") || rest.starts_with("<![CDATA[") {
            problems.push("has an unterminated comment, CDATA section or processing instruction".to_owned());
            break;
        }
        // A tag: find its end, honouring quoted attribute values.
        let mut quote: Option<char> = None;
        let mut end = None;
        for (i, ch) in rest.char_indices().skip(1) {
            match (quote, ch) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(ch),
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                (None, '<') => break,
                _ => {}
            }
        }
        let Some(end) = end else {
            problems.push("has an unterminated tag".to_owned());
            break;
        };
        let tag = rest.get(1..end).unwrap_or("");
        rest = rest.get(end.saturating_add(1)..).unwrap_or("");
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            match stack.pop() {
                Some(open) if open == name => {}
                Some(open) => {
                    problems.push(format!("closes </{name}> while <{open}> is open"));
                    break;
                }
                None => {
                    problems.push(format!("closes </{name}> that was never opened"));
                    break;
                }
            }
            continue;
        }
        let self_closing = tag.trim_end().ends_with('/');
        let tag = tag.trim_end().trim_end_matches('/');
        let name: String = tag.chars().take_while(|c| !c.is_whitespace()).collect();
        if name.is_empty() {
            problems.push("has an empty tag".to_owned());
            break;
        }
        let local = name.rsplit(':').next().unwrap_or(&name).to_lowercase();
        if stack.is_empty() {
            roots = roots.saturating_add(1);
            if local != "svg" {
                problems.push(format!("root element is <{name}>, not <svg>"));
            }
            if roots > 1 {
                problems.push("has more than one root element".to_owned());
            }
        }
        if matches!(local.as_str(), "image" | "script" | "foreignobject" | "iframe" | "audio" | "video") {
            problems.push(format!("contains <{name}> (icons must be plain vector shapes)"));
        }
        for (attr, value) in attributes(tag.get(name.len()..).unwrap_or("")) {
            let a = attr.to_lowercase();
            if (a == "href" || a.ends_with(":href")) && !value.trim().starts_with('#') {
                problems.push(format!("{attr}=\"{value}\" is an external reference"));
            }
        }
        if !self_closing {
            if stack.len() >= 256 {
                problems.push("nests elements deeper than 256 levels".to_owned());
                break;
            }
            stack.push(name);
        }
    }
    if let Some(open) = stack.last() {
        problems.push(format!("<{open}> is never closed"));
    }
    if roots == 0 {
        problems.push("has no <svg> root element".to_owned());
    }
    problems.dedup();
    problems
}

/// `name="value"` pairs of a tag's attribute text.
fn attributes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(eq) = rest.find('=') {
        let name = rest.get(..eq).unwrap_or("").split_whitespace().last().unwrap_or("").to_owned();
        let after = rest.get(eq.saturating_add(1)..).unwrap_or("").trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
        let inner = after.get(1..).unwrap_or("");
        let Some(close) = inner.find(q) else { break };
        out.push((name, inner.get(..close).unwrap_or("").to_owned()));
        rest = inner.get(close.saturating_add(1)..).unwrap_or("");
    }
    out
}

/// All problems under `root` (empty = OK).
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let assets = root.join("assets");
    let mut problems = Vec::new();
    if !assets.is_dir() {
        return Ok(vec!["there is no assets/ folder".to_owned()]);
    }
    let attribution = std::fs::read_to_string(root.join("ATTRIBUTION.md"))
        .map_err(|e| format!("cannot read ATTRIBUTION.md: {e}"))?;
    let rows = attribution_rows(&attribution);
    let files = walk_files(&assets)?;
    for f in &files {
        let path = rel(root, f);
        if !rows.iter().any(|r| glob_match(r, &path)) {
            problems.push(format!("{path} has no row in ATTRIBUTION.md"));
        }
    }
    for r in &rows {
        if !files.iter().any(|f| glob_match(r, &rel(root, f))) {
            println!("  warning: ATTRIBUTION.md row `{r}` matches no file");
        }
    }
    let svgs = files.iter().filter(|f| {
        let p = rel(root, f);
        f.extension().is_some_and(|e| e == "svg")
            && (glob_match("assets/icons/**", &p) || glob_match("assets/app-icon/*.svg", &p))
    });
    for f in svgs {
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                problems.push(format!("{}: cannot read: {e}", rel(root, f)));
                continue;
            }
        };
        problems.extend(check_svg(&text).into_iter().map(|p| format!("{} {p}", rel(root, f))));
    }
    let icon_dir = root.join("assets").join("app-icon");
    if icon_dir.join(crate::ico::SOURCE).is_file() {
        for out in crate::ico::outputs(&icon_dir) {
            if !out.is_file() {
                problems.push(format!("{} is missing: run `cargo xtask ico`", rel(root, &out)));
            }
        }
    }
    Ok(problems)
}

pub fn run(root: &Path) -> TaskResult {
    let problems = check(root)?;
    if problems.is_empty() {
        println!("assets: OK");
        return Ok(());
    }
    for p in &problems {
        println!("  ✗ {p}");
    }
    Err(format!("assets: {} problem(s)", problems.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("assets/icons/*.svg", "assets/icons/mail.svg"));
        assert!(!glob_match("assets/icons/*.svg", "assets/icons/sub/mail.svg"));
        assert!(!glob_match("assets/icons/*.svg", "assets/icons/README.md"));
        assert!(glob_match("assets/**", "assets/a/b/c.png"));
        assert!(glob_match(
            "assets/app-icon/hicolor/**/mailcraft.png",
            "assets/app-icon/hicolor/16x16/apps/mailcraft.png"
        ));
        assert!(glob_match("assets/fonts/Inter-?egular.ttf", "assets/fonts/Inter-Regular.ttf"));
        assert!(glob_match("a", "a"));
        assert!(!glob_match("a", "ab"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn attribution_table() {
        let md = "# A\n\n| Path | Author |\n|---|---|\n| assets/x.ttf | Me |\n| `assets/icons/*.svg` | Us |\n| docs/y | no |\n";
        assert_eq!(attribution_rows(md), vec!["assets/x.ttf", "assets/icons/*.svg"]);
    }

    #[test]
    fn good_svgs() {
        let ok = r##"<?xml version="1.0"?><!-- c --><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><defs><linearGradient id="g"/></defs><path d="M0 0" fill="url(#g)"/><use href="#g"/></svg>"##;
        assert!(check_svg(ok).is_empty(), "{:?}", check_svg(ok));
        let quoted = r#"<svg a="x > y"><g><title>a &lt; b</title></g></svg>"#;
        assert!(check_svg(quoted).is_empty(), "{:?}", check_svg(quoted));
    }

    #[test]
    fn bad_svgs() {
        let cases = [
            ("<svg><image href=\"a.png\"/></svg>", "<image>"),
            ("<svg><use xlink:href=\"http://x/y.svg#a\"/></svg>", "external reference"),
            ("<svg><path style=\"fill:url(http://x)\"/></svg>", "url("),
            ("<svg><g></svg>", "closes </svg>"),
            ("<svg>", "never closed"),
            ("<html></html>", "not <svg>"),
            ("", "no <svg>"),
            ("<svg/><svg/>", "more than one root"),
            ("<!DOCTYPE svg [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><svg/>", "DOCTYPE"),
            ("<svg><script>alert(1)</script></svg>", "<script>"),
            ("<svg", "unterminated tag"),
            ("text<svg/>", "outside the root"),
        ];
        for (svg, want) in cases {
            let p = check_svg(svg);
            assert!(p.iter().any(|m| m.contains(want)), "{svg}: {p:?}");
        }
    }
}
