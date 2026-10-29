//! `cargo xtask licences`: every dependency (all platforms, dev and build included) must be usable under the
//! allowlist. SPDX expressions are parsed (`OR`, `AND`, `WITH`, parentheses, the legacy `/` = OR) and
//! satisfied by choosing allowed licences.

use std::path::Path;

use crate::TaskResult;
use crate::metadata::{self, Package};

/// Licences any dependency may use.
pub const ALLOWED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CC0-1.0",
    "BSL-1.0",
    "0BSD",
];
/// Allowed only for font packages: open font licences (SIL OFL; the Ubuntu Font Licence used by egui's
/// bundled fallback font). Both permit use, modification and redistribution.
pub const FONT_ONLY: &[&str] = &["OFL-1.1", "Ubuntu-font-1.0"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Id(String),
    With(Box<Expr>, String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    And,
    Or,
    With,
    Word(String),
}

fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Tok>| {
        if word.is_empty() {
            return;
        }
        let tok = match word.as_str() {
            w if w.eq_ignore_ascii_case("AND") => Tok::And,
            w if w.eq_ignore_ascii_case("OR") => Tok::Or,
            w if w.eq_ignore_ascii_case("WITH") => Tok::With,
            w => Tok::Word(w.to_owned()),
        };
        out.push(tok);
        word.clear();
    };
    for c in s.chars() {
        match c {
            '(' | ')' | '/' => {
                flush(&mut word, &mut out);
                out.push(match c {
                    '(' => Tok::Open,
                    ')' => Tok::Close,
                    _ => Tok::Or,
                });
            }
            c if c.is_whitespace() => flush(&mut word, &mut out),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut out);
    out
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 64;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos = self.pos.saturating_add(1);
        t
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.next();
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.with()?;
        while self.peek() == Some(&Tok::And) {
            self.next();
            left = Expr::And(Box::new(left), Box::new(self.with()?));
        }
        Ok(left)
    }
    fn with(&mut self) -> Result<Expr, String> {
        let base = self.primary()?;
        if self.peek() == Some(&Tok::With) {
            self.next();
            match self.next() {
                Some(Tok::Word(exc)) => return Ok(Expr::With(Box::new(base), exc)),
                _ => return Err("WITH needs an exception id".into()),
            }
        }
        Ok(base)
    }
    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(Expr::Id(w)),
            Some(Tok::Open) => {
                self.depth = self.depth.saturating_add(1);
                if self.depth > MAX_DEPTH {
                    return Err("nested too deeply".into());
                }
                let e = self.or()?;
                self.depth = self.depth.saturating_sub(1);
                match self.next() {
                    Some(Tok::Close) => Ok(e),
                    _ => Err("missing `)`".into()),
                }
            }
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// Parses an SPDX licence expression.
pub fn parse(s: &str) -> Result<Expr, String> {
    let mut p = Parser { toks: tokenize(s), pos: 0, depth: 0 };
    let e = p.or()?;
    match p.peek() {
        None => Ok(e),
        Some(t) => Err(format!("unexpected {t:?} after the expression")),
    }
}

/// Whether the expression can be satisfied with allowed licences (`font`: font licences allowed too).
pub fn satisfiable(e: &Expr, font: bool) -> bool {
    match e {
        Expr::Id(id) => {
            let id = id.trim_end_matches('+');
            ALLOWED.contains(&id) || (font && FONT_ONLY.contains(&id))
        }
        // An exception only adds permissions to the licence it modifies.
        Expr::With(base, _) => satisfiable(base, font),
        Expr::And(a, b) => satisfiable(a, font) && satisfiable(b, font),
        Expr::Or(a, b) => satisfiable(a, font) || satisfiable(b, font),
    }
}

fn is_font(p: &Package) -> bool {
    let has = |v: &[String]| v.iter().any(|k| k.to_lowercase().contains("font"));
    p.name.to_lowercase().contains("font") || has(&p.keywords) || has(&p.categories)
}

/// Why `p` is not acceptable (None = fine).
pub fn offence(p: &Package) -> Option<String> {
    let Some(expr) = p.license.as_deref().map(str::trim).filter(|l| !l.is_empty()) else {
        return Some(match &p.license_file {
            Some(f) => format!("no SPDX licence (license-file {f}): review by hand"),
            None => "no licence declared".to_owned(),
        });
    };
    match parse(expr) {
        Err(e) => Some(format!("cannot parse `{expr}`: {e}")),
        Ok(e) if satisfiable(&e, is_font(p)) => None,
        Ok(_) => Some(format!("`{expr}` is not satisfiable with the allowlist")),
    }
}

/// Dependencies compiled into the binaries whose licences require their text to ship with them, and lines
/// of that text `NOTICE` (which every package ships) must hold: the copyright line and the binary
/// redistribution or permission condition. Compared with runs of whitespace collapsed.
pub const NOTICE_TEXTS: &[(&str, &[&str])] = &[
    (
        "zstd-sys",
        &[
            "Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.",
            "Redistributions in binary form must reproduce the above copyright notice, this list of \
             conditions and the following disclaimer in the documentation and/or other materials provided \
             with the distribution.",
            "THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"",
        ],
    ),
    ("zstd", &["Copyright (c) 2026, Alexandre Bury", "Redistributions in binary form must reproduce"]),
    (
        "rusqlite",
        &[
            "Copyright (c) 2014 The rusqlite developers",
            "The above copyright notice and this permission notice shall be included in all copies or \
             substantial portions of the Software.",
        ],
    ),
    ("libsqlite3-sys", &["Copyright (c) 2014 The rusqlite developers"]),
];

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The licence lines `notice` lacks for the packages in `names` (empty when it holds them all).
pub fn notice_missing(notice: &str, names: impl Fn(&str) -> bool) -> Vec<String> {
    let text = squash(notice);
    let mut missing = Vec::new();
    for (krate, lines) in NOTICE_TEXTS {
        if !names(krate) {
            continue;
        }
        for line in *lines {
            if !text.contains(&squash(line)) {
                missing.push(format!("{krate}: NOTICE lacks \"{line}\""));
            }
        }
    }
    missing
}

pub fn run(root: &Path) -> TaskResult {
    let meta = metadata::load(root, true)?;
    let notice =
        std::fs::read_to_string(root.join("NOTICE")).map_err(|e| format!("licences: NOTICE: {e}"))?;
    let missing = notice_missing(&notice, |name| meta.packages.iter().any(|p| p.name == name));
    if !missing.is_empty() {
        for m in &missing {
            println!("  ✗ {m}");
        }
        return Err(format!(
            "licences: NOTICE must reproduce the licence text of {} bundled dependencies",
            missing.len()
        ));
    }
    let mut offenders = Vec::new();
    let mut checked = 0usize;
    for p in &meta.packages {
        if meta.workspace_members.contains(&p.id) {
            continue;
        }
        checked = checked.saturating_add(1);
        if let Some(why) = offence(p) {
            offenders.push(format!("{} {}: {why}", p.name, p.version));
        }
    }
    offenders.sort();
    if offenders.is_empty() {
        println!("licences: OK ({checked} dependencies)");
        return Ok(());
    }
    for o in &offenders {
        println!("  ✗ {o}");
    }
    Err(format!(
        "licences: {} of {checked} dependencies are not on the allowlist ({})",
        offenders.len(),
        ALLOWED.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        satisfiable(&parse(s).unwrap(), false)
    }

    #[test]
    fn expressions() {
        assert!(ok("MIT"));
        assert!(ok("MIT OR Apache-2.0"));
        assert!(ok("MIT/Apache-2.0"));
        assert!(ok("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"));
        assert!(ok("(MIT OR Apache-2.0) AND Unicode-3.0"));
        assert!(ok("MPL-2.0 OR MIT"));
        assert!(ok("Apache-2.0+"));
        assert!(!ok("mit or apache-2.0"), "ids are case-sensitive");
        assert!(!ok("MPL-2.0"));
        assert!(!ok("MIT AND LGPL-2.1-or-later"));
        assert!(!ok("GPL-3.0-only"));
        assert!(!ok("(MIT AND (GPL-2.0 OR AGPL-3.0))"));
        assert!(!ok("OFL-1.1"));
        assert!(satisfiable(&parse("OFL-1.1 OR GPL-3.0").unwrap(), true));
    }

    #[test]
    fn parse_errors() {
        for bad in ["", "(MIT", "MIT)", "MIT OR", "AND MIT", "MIT WITH", &"(".repeat(100)] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse("A OR B AND C").unwrap(),
            Expr::Or(
                Box::new(Expr::Id("A".into())),
                Box::new(Expr::And(Box::new(Expr::Id("B".into())), Box::new(Expr::Id("C".into()))))
            )
        );
    }

    #[test]
    fn offences() {
        let p = |license: Option<&str>, name: &str| Package {
            name: name.into(),
            license: license.map(str::to_owned),
            ..Package::default()
        };
        assert_eq!(offence(&p(Some("MIT"), "a")), None);
        assert!(offence(&p(None, "a")).unwrap().contains("no licence"));
        assert!(offence(&p(Some("MPL-2.0"), "a")).unwrap().contains("not satisfiable"));
        assert_eq!(offence(&p(Some("OFL-1.1"), "some-font")), None);
        assert!(offence(&p(Some("OFL-1.1"), "not-a-typeface")).is_some());
    }

    #[test]
    fn notice_reproduces_bundled_licences() {
        let notice =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("NOTICE")).unwrap();
        assert_eq!(notice_missing(&notice, |_| true), Vec::<String>::new());
        // Line breaks and indentation do not matter; a missing text does, for a dependency in the build only.
        let reflowed = notice.replace('\n', "  \n   ");
        assert!(notice_missing(&reflowed, |_| true).is_empty());
        let cut = notice.replace("Redistributions in binary form", "Redistributions");
        assert!(notice_missing(&cut, |n| n == "zstd-sys").iter().any(|m| m.starts_with("zstd-sys")));
        assert!(notice_missing(&cut, |n| n == "rusqlite").is_empty());
        assert!(notice_missing("", |_| false).is_empty());
    }
}
