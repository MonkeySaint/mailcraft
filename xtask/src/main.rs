//! `cargo xtask <task>`: repository checks and chores (alias in `.cargo/config.toml`).
//!
//! Every task works on the repository root: the parent of this package's directory, or `--root DIR`.

mod assets;
mod bench;
mod ci;
mod ico;
mod layers;
mod licences;
mod metadata;
mod parity;
mod stats;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo xtask [--root DIR] <task> [options]

Tasks:
  ci                         fmt, clippy, tests, layers, assets, licences, parity --check, wasm build
  layers                     crates depend only on lower layers; nothing below L6 pulls in GUI crates;
                             test-support crates only as dev-dependencies or from apps
  assets                     every file under assets/ has an ATTRIBUTION.md row; icon SVGs are self-contained
  licences                   every dependency's licence is on the allowlist
  parity [--out FILE] [--check]
                             write docs/parity.md (--check: fail when stale or below xtask/parity-floor.txt)
  bench [--size 10k|50k | --size 100k|1m|2m --small-bodies] [--backend legacy|sqlite]
        [--out FILE] [--seed N] [--sync N] [--bulk-move N] [--keep] [--force-large]
                             storage benchmarks on a synthetic profile (apps/mailcraft-bench, release
                             build); JSON report in target/scratch/bench/ unless --out. Full bodies stop
                             at 50k messages, small bodies at 2m (--force-large lifts that)
  stats                      lines of Rust, tests and commands per crate
  version show | bump X.Y.Z | check
                             show or bump the workspace version; check it against the CI tag
  ico                        rasterise assets/app-icon/mailcraft.svg to PNG/ICO/ICNS
                             (into $MAILCRAFT_ICO_OUT instead of assets/app-icon when set)

Options:
  --root DIR                 repository to work on (default: the one this xtask lives in)
";

/// Result of a task: `Err` carries the message printed before exiting with status 1.
pub type TaskResult = Result<(), String>;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let root = match take_root(&mut args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&root, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Removes `--root DIR` / `--root=DIR` from `args`; returns the root (default: discovered).
fn take_root(args: &mut Vec<String>) -> Result<PathBuf, String> {
    let mut root = None;
    let mut i = 0;
    while let Some(a) = args.get(i).cloned() {
        if let Some(v) = a.strip_prefix("--root=") {
            root = Some(PathBuf::from(v));
            args.remove(i);
        } else if a == "--root" {
            args.remove(i);
            if i >= args.len() {
                return Err("`--root` needs a directory".into());
            }
            root = Some(PathBuf::from(args.remove(i)));
        } else {
            i += 1;
        }
    }
    let root = match root {
        Some(r) => r,
        None => discover_root()?,
    };
    if !root.join("Cargo.toml").is_file() {
        return Err(format!("{} has no Cargo.toml (is it the repository root?)", root.display()));
    }
    Ok(std::path::absolute(&root).unwrap_or(root))
}

/// The repository root: the parent of this package (`xtask/..`), else `git rev-parse --show-toplevel`.
fn discover_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(parent) = manifest_dir.parent()
        && parent.join("Cargo.toml").is_file()
    {
        return Ok(parent.to_path_buf());
    }
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("cannot find the repository root (git: {e}); pass --root DIR"))?;
    if !out.status.success() {
        return Err("cannot find the repository root; pass --root DIR".into());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn run(root: &Path, args: &[String]) -> TaskResult {
    let Some((task, rest)) = args.split_first() else {
        print!("{USAGE}");
        return Ok(());
    };
    let no_args = |rest: &[String]| match rest.first() {
        Some(a) => Err(format!("`{task}` takes no argument `{a}`")),
        None => Ok(()),
    };
    match task.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "ci" => {
            no_args(rest)?;
            ci::run(root)
        }
        "layers" => {
            no_args(rest)?;
            layers::run(root)
        }
        "assets" => {
            no_args(rest)?;
            assets::run(root)
        }
        "licences" | "licenses" => {
            no_args(rest)?;
            licences::run(root)
        }
        "parity" => {
            let mut out = None;
            let mut check = false;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--check" => check = true,
                    "--out" => out = Some(PathBuf::from(it.next().ok_or("`--out` needs a file")?)),
                    s if s.starts_with("--out=") => out = Some(PathBuf::from(s.trim_start_matches("--out="))),
                    other => return Err(format!("`parity` does not take `{other}`")),
                }
            }
            let out = out.unwrap_or_else(|| root.join("docs").join("parity.md"));
            if check { parity::check(&out) } else { parity::write(&out) }
        }
        "bench" => bench::run(root, rest),
        "stats" => {
            no_args(rest)?;
            stats::run(root)
        }
        "version" => version::run(root, rest),
        "ico" => {
            no_args(rest)?;
            ico::run(root)
        }
        other => Err(format!("unknown task `{other}`\n\n{USAGE}")),
    }
}

/// The `cargo` that runs this xtask (`$CARGO`), else `cargo` from PATH.
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Path relative to `root` with `/` separators (for messages and ATTRIBUTION.md rows).
pub fn rel(root: &Path, path: &Path) -> String {
    let p = path.strip_prefix(root).unwrap_or(path);
    p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Every file under `dir`, recursively, sorted (symlinks are not followed).
pub fn walk_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("cannot list {}: {e}", d.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", d.display()))?;
            let ty = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if ty.is_dir() {
                stack.push(entry.path());
            } else if ty.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}
