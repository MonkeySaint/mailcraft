//! `cargo xtask stats`: lines of Rust per crate (src / tests), `#[test]` counts, registered commands.

use std::path::Path;

use crate::{TaskResult, metadata, walk_files};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub src_lines: usize,
    pub test_lines: usize,
    pub tests: usize,
}

/// Counts for one package directory: `src/` lines, `tests/` + `benches/` + `examples/` lines, and `#[test]`
/// attributes anywhere in its Rust files.
pub fn count(dir: &Path) -> Result<Counts, String> {
    let mut c = Counts::default();
    for (sub, is_src) in [("src", true), ("tests", false), ("benches", false), ("examples", false)] {
        let d = dir.join(sub);
        if !d.is_dir() {
            continue;
        }
        for f in walk_files(&d)? {
            if f.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let lines = text.lines().count();
            if is_src {
                c.src_lines = c.src_lines.saturating_add(lines);
            } else {
                c.test_lines = c.test_lines.saturating_add(lines);
            }
            c.tests = c.tests.saturating_add(count_tests(&text));
        }
    }
    Ok(c)
}

/// `#[test]` attributes (also `#[tokio::test]`-style `…::test]`) at the start of a line.
pub fn count_tests(text: &str) -> usize {
    text.lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("#[test]") || (l.starts_with("#[") && l.contains("::test]")))
        .count()
}

pub fn run(root: &Path) -> TaskResult {
    let meta = metadata::load(root, false)?;
    let mut members = meta.members();
    members.sort_by_key(|p| p.name.clone());
    println!("{:<20} {:>9} {:>11} {:>6}", "CRATE", "SRC LINES", "TEST LINES", "TESTS");
    let mut total = Counts::default();
    for p in members {
        let Some(dir) = p.manifest_path.parent() else { continue };
        let c = count(dir)?;
        println!("{:<20} {:>9} {:>11} {:>6}", p.name, c.src_lines, c.test_lines, c.tests);
        total.src_lines = total.src_lines.saturating_add(c.src_lines);
        total.test_lines = total.test_lines.saturating_add(c.test_lines);
        total.tests = total.tests.saturating_add(c.tests);
    }
    println!("{:<20} {:>9} {:>11} {:>6}", "total", total.src_lines, total.test_lines, total.tests);
    let reg = mailcraft_engine::cmd::registry();
    let rows = mailcraft_engine::catalog::catalog_rows().len();
    println!("commands registered: {}; Outlook catalog rows: {rows}", reg.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tests_and_lines() {
        assert_eq!(count_tests("#[test]\nfn a() {}\n    #[test]\n// #[test]\n#[tokio::test]\n"), 3);
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "fn a() {}\n#[cfg(test)]\nmod t {\n#[test]\nfn b() {}\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/sub/x.rs"), "// x\n").unwrap();
        std::fs::write(dir.path().join("src/notes.txt"), "#[test]\n").unwrap();
        std::fs::write(dir.path().join("tests/t.rs"), "#[test]\nfn c() {}\n").unwrap();
        assert_eq!(count(dir.path()).unwrap(), Counts { src_lines: 7, test_lines: 2, tests: 2 });
    }
}
