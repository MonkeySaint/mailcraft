//! `cargo xtask version show | bump X.Y.Z | check`.
//!
//! The version lives in `[workspace.package] version` of the root Cargo.toml; every crate inherits it.
//! `bump` edits that line and the matching Cargo.lock entries of the workspace crates (path packages, which
//! have no `source`). `check` compares it with the tag CI is building (GITHUB_REF / GITHUB_REF / GITHUB_REF
//! = `refs/tags/vX.Y.Z`).

use std::path::Path;

use crate::TaskResult;

/// Whether `v` looks like `X.Y.Z` with an optional `-pre` / `+build` suffix.
pub fn valid(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    let suffix_ok = v
        .get(core.len()..)
        .unwrap_or("")
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0'))
        })
        && suffix_ok
}

/// Line index and value of `version = "…"` inside `[workspace.package]`.
fn find_version(manifest: &str) -> Option<(usize, String)> {
    let mut in_section = false;
    for (i, line) in manifest.lines().enumerate() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t == "[workspace.package]";
            continue;
        }
        if in_section
            && let Some(rest) = t.strip_prefix("version")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            let value = value.trim();
            let value = value.split('#').next().unwrap_or("").trim().trim_matches('"');
            return Some((i, value.to_owned()));
        }
    }
    None
}

pub fn workspace_version(manifest: &str) -> Result<String, String> {
    find_version(manifest)
        .map(|(_, v)| v)
        .ok_or_else(|| "Cargo.toml has no `version` in [workspace.package]".to_owned())
}

fn join_lines(lines: &[String], original: &str) -> String {
    let mut out = lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Cargo.toml with the workspace version replaced.
pub fn bump_manifest(manifest: &str, new: &str) -> Result<String, String> {
    let (idx, old) = find_version(manifest).ok_or("Cargo.toml has no `version` in [workspace.package]")?;
    let lines: Vec<String> = manifest
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == idx { l.replacen(&format!("\"{old}\""), &format!("\"{new}\""), 1) } else { l.to_owned() }
        })
        .collect();
    Ok(join_lines(&lines, manifest))
}

/// Cargo.lock with every path package (no `source`) at version `old` moved to `new`; returns the text and
/// the package names changed.
pub fn bump_lock(lock: &str, old: &str, new: &str) -> (String, Vec<String>) {
    let mut lines: Vec<String> = lock.lines().map(str::to_owned).collect();
    let mut changed = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines.get(i).map(|l| l.trim()) != Some("[[package]]") {
            i += 1;
            continue;
        }
        let start = i.saturating_add(1);
        let mut end = start;
        while end < lines.len()
            && lines.get(end).is_some_and(|l| !l.trim().is_empty() && l.trim() != "[[package]]")
        {
            end += 1;
        }
        let block = lines.get(start..end).unwrap_or(&[]);
        let field = |key: &str| block.iter().position(|l| l.trim_start().starts_with(&format!("{key} = ")));
        let has_source = field("source").is_some();
        let name = field("name")
            .and_then(|p| block.get(p))
            .map(|l| l.trim().trim_start_matches("name = ").trim_matches('"').to_owned());
        if let (false, Some(name), Some(vpos)) = (has_source, name, field("version"))
            && let Some(line) = lines.get_mut(start.saturating_add(vpos))
            && line.trim() == format!("version = \"{old}\"")
        {
            *line = format!("version = \"{new}\"");
            changed.push(name);
        }
        i = end;
    }
    (join_lines(&lines, lock), changed)
}

/// The tag CI is building, from the first of GITHUB_REF / GITHUB_REF / GITHUB_REF that is a tag ref.
fn ci_tag() -> Option<(String, String)> {
    ["GITHUB_REF", "GITHUB_REF", "GITHUB_REF"].iter().find_map(|var| {
        let r = std::env::var(var).ok()?;
        let tag = r.strip_prefix("refs/tags/")?.to_owned();
        Some(((*var).to_owned(), tag))
    })
}

pub fn run(root: &Path, args: &[String]) -> TaskResult {
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let current = workspace_version(&manifest)?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["show"] => {
            println!("{current}");
            Ok(())
        }
        ["bump", new] => {
            let new = new.trim_start_matches('v');
            if !valid(new) {
                return Err(format!("`{new}` is not a version like 1.2.3"));
            }
            if new == current {
                println!("version is already {current}");
                return Ok(());
            }
            let manifest_new = bump_manifest(&manifest, new)?;
            let lock_path = root.join("Cargo.lock");
            let lock = std::fs::read_to_string(&lock_path).ok();
            let lock_new = lock.as_deref().map(|l| bump_lock(l, &current, new));
            std::fs::write(&manifest_path, manifest_new)
                .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
            match lock_new {
                Some((text, changed)) => {
                    std::fs::write(&lock_path, text)
                        .map_err(|e| format!("cannot write {}: {e}", lock_path.display()))?;
                    println!("version {current} → {new} (Cargo.toml; Cargo.lock: {})", changed.join(", "));
                }
                None => println!("version {current} → {new} (Cargo.toml; no Cargo.lock)"),
            }
            Ok(())
        }
        ["check"] => match ci_tag() {
            None => {
                println!(
                    "version {current}: not a tag build (GITHUB_REF/GITHUB_REF/GITHUB_REF), nothing to check"
                );
                Ok(())
            }
            Some((var, tag)) => {
                let want = tag.trim_start_matches('v');
                if want == current {
                    println!("version {current} matches tag {tag} ({var})");
                    Ok(())
                } else {
                    Err(format!(
                        "tag {tag} ({var}) does not match the workspace version {current}: run `cargo xtask version bump {want}`"
                    ))
                }
            }
        },
        other => Err(format!("usage: cargo xtask version show | bump X.Y.Z | check (got {other:?})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[workspace]\nmembers = []\n\n[workspace.package]\nedition = \"2024\"\nversion = \"0.1.0\" # x\n\n[workspace.dependencies]\nfoo = { version = \"0.1.0\" }\n";
    const LOCK: &str = "version = 4\n\n[[package]]\nname = \"foo\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"mailcraft-geom\"\nversion = \"0.1.0\"\ndependencies = [\n \"foo\",\n]\n\n[[package]]\nname = \"xtask\"\nversion = \"0.1.0\"\n";

    #[test]
    fn versions() {
        for v in ["0.1.0", "1.20.3", "1.0.0-rc.1", "2.0.0+build5"] {
            assert!(valid(v), "{v}");
        }
        for v in ["1.0", "1.0.0.0", "01.0.0", "a.b.c", "", "1..0", "1.0.0-r c"] {
            assert!(!valid(v), "{v}");
        }
    }

    #[test]
    fn bumps_manifest_and_lock() {
        assert_eq!(workspace_version(MANIFEST).unwrap(), "0.1.0");
        let m = bump_manifest(MANIFEST, "0.2.0").unwrap();
        assert_eq!(workspace_version(&m).unwrap(), "0.2.0");
        assert!(m.contains("foo = { version = \"0.1.0\" }"), "other sections untouched");
        assert!(m.contains("version = \"0.2.0\" # x"));
        let (l, changed) = bump_lock(LOCK, "0.1.0", "0.2.0");
        assert_eq!(changed, vec!["mailcraft-geom", "xtask"]);
        assert!(l.contains("name = \"foo\"\nversion = \"0.1.0\"\nsource"), "registry packages untouched");
        assert!(l.contains("name = \"xtask\"\nversion = \"0.2.0\"\n"));
        assert!(workspace_version("[package]\nversion = \"1.0.0\"\n").is_err());
    }

    #[test]
    fn bump_task_on_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), LOCK).unwrap();
        run(dir.path(), &["bump".into(), "v0.3.1".into()]).unwrap();
        let m = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert_eq!(workspace_version(&m).unwrap(), "0.3.1");
        assert!(
            std::fs::read_to_string(dir.path().join("Cargo.lock")).unwrap().contains("version = \"0.3.1\"")
        );
        assert!(run(dir.path(), &["bump".into(), "nope".into()]).is_err());
        assert!(run(dir.path(), &["frob".into()]).is_err());
        run(dir.path(), &["show".into()]).unwrap();
    }
}
