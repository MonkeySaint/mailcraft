//! `cargo xtask ci`: the quality gate, in order, stopping at the first failure.

use std::path::Path;
use std::process::Command;

use crate::{TaskResult, assets, cargo, layers, licences, parity};

/// Runs `cargo <args>` in `root`; Err names the step.
fn cargo_step(root: &Path, name: &str, args: &[&str]) -> TaskResult {
    println!("==> {name}: cargo {}", args.join(" "));
    let status = Command::new(cargo())
        .current_dir(root)
        .args(args)
        .status()
        .map_err(|e| format!("ci: cannot run cargo for `{name}`: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("ci: `{name}` failed ({status})")) }
}

fn step(name: &str, f: impl FnOnce() -> TaskResult) -> TaskResult {
    println!("==> {name}");
    f().map_err(|e| format!("ci: `{name}` failed: {e}"))
}

/// Whether rustc has the standard library for `target` installed.
pub fn target_installed(target: &str) -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    Command::new(rustc)
        .args(["--print", "target-libdir", "--target", target])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .is_some_and(|dir| !dir.is_empty() && Path::new(&dir).is_dir())
}

pub fn run(root: &Path) -> TaskResult {
    match std::env::var("CARGO_BUILD_JOBS") {
        Ok(j) => println!("ci: CARGO_BUILD_JOBS={j} (passed through to every cargo step)"),
        Err(_) => {
            println!("ci: CARGO_BUILD_JOBS is not set (cargo uses every core; set it on small machines)")
        }
    }
    cargo_step(root, "fmt", &["fmt", "--all", "--", "--check"])?;
    cargo_step(root, "clippy", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
    cargo_step(root, "test", &["test", "--workspace"])?;
    step("layers", || layers::run(root))?;
    step("assets", || assets::run(root))?;
    step("licences", || licences::run(root))?;
    step("parity --check", || parity::check(&root.join("docs").join("parity.md")))?;
    const WASM: &str = "wasm32-unknown-unknown";
    if target_installed(WASM) {
        cargo_step(root, "wasm check", &["check", "-p", "mailcraft-engine", "--target", WASM])?;
    } else {
        println!(
            "==> wasm check: skipped (warning: target {WASM} is not installed; `rustup target add {WASM}`)"
        );
    }
    println!("ci: all checks passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_target_is_not_installed() {
        assert!(!super::target_installed("no-such-target-triple"));
    }
}
