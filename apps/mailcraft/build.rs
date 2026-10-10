//! Build script for the desktop app.
//!
//! 1. Build details for `--version` and the About page: `MAILCRAFT_GIT_HASH` (short commit hash) and
//!    `MAILCRAFT_BUILD_DATE` (YYYY-MM-DD, UTC). Values already in the environment win, so release builds
//!    (`packaging/env.sh`) and builds from a source tarball without `.git` can set them; otherwise the hash
//!    comes from `git` and the date from `SOURCE_DATE_EPOCH` or the clock. Missing values become `unknown`.
//! 2. On Windows targets, the app icon (`assets/app-icon/mailcraft.ico`) as a Win32 resource, when a
//!    resource compiler (`rc.exe`, `llvm-rc`, `windres`) is on PATH. Without one the build goes on with a
//!    warning: the window icon is set at run time anyway, only Explorer's file icon is missing.
//!
//! A build script must never fail the build for these extras, so every step falls back quietly.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for var in ["MAILCRAFT_GIT_HASH", "MAILCRAFT_BUILD_DATE", "SOURCE_DATE_EPOCH"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());

    let hash = env_value("MAILCRAFT_GIT_HASH").or_else(|| git_hash(&manifest_dir)).unwrap_or_else(unknown);
    let date = env_value("MAILCRAFT_BUILD_DATE").or_else(source_date).unwrap_or_else(today);
    println!("cargo:rustc-env=MAILCRAFT_GIT_HASH={hash}");
    println!("cargo:rustc-env=MAILCRAFT_BUILD_DATE={date}");
    watch_git(&manifest_dir);

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_icon(&manifest_dir);
    }
}

fn unknown() -> String {
    "unknown".to_owned()
}

/// A non-empty environment value with only safe characters (it ends up in `rustc-env`).
fn env_value(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?;
    let v = v.trim();
    let ok =
        !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || "-+._:".contains(c));
    ok.then(|| v.to_owned())
}

fn run(cmd: &str, args: &[&str], dir: &Path) -> Option<String> {
    let out = Command::new(cmd).args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn git_hash(dir: &Path) -> Option<String> {
    run("git", &["rev-parse", "--short=10", "HEAD"], dir).filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Re-run when HEAD moves: `.git/HEAD`, the branch it points at and `packed-refs`.
fn watch_git(dir: &Path) {
    let Some(git_dir) = run("git", &["rev-parse", "--absolute-git-dir"], dir).map(PathBuf::from) else {
        return;
    };
    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    println!("cargo:rerun-if-changed={}", git_dir.join("packed-refs").display());
    if let Ok(text) = std::fs::read_to_string(&head)
        && let Some(r) = text.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
    }
}

/// `SOURCE_DATE_EPOCH` (reproducible builds) as a date.
fn source_date() -> Option<String> {
    let secs = std::env::var("SOURCE_DATE_EPOCH").ok()?.trim().parse::<u64>().ok()?;
    Some(ymd(secs))
}

fn today() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| ymd(d.as_secs()))
        .unwrap_or_else(|_| unknown())
}

/// Seconds since 1970-01-01 UTC to YYYY-MM-DD (proleptic Gregorian, days-to-civil).
fn ymd(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Compiles `1 ICON "mailcraft.ico"` into a resource and links it into the executable.
fn windows_icon(manifest_dir: &Path) {
    let ico = manifest_dir.join("../../assets/app-icon/mailcraft.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let Ok(ico) = ico.canonicalize() else {
        println!("cargo:warning=app icon not found; the executable gets no icon resource");
        return;
    };
    // Forward slashes and no `\\?\` prefix: both resource compilers accept that form.
    let ico_text = ico.display().to_string().replace('\\', "/");
    let ico_text = ico_text.trim_start_matches("//?/");
    let rc = out_dir.join("mailcraft.rc");
    if std::fs::write(&rc, format!("1 ICON \"{ico_text}\"\n")).is_err() {
        return;
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let rc_s = rc.display().to_string();
    let compiled = if msvc {
        let res = out_dir.join("mailcraft.res");
        let res_s = res.display().to_string();
        let fo = format!("/fo{res_s}");
        let ok = ["rc.exe", "rc", "llvm-rc"].iter().any(|tool| {
            Command::new(tool)
                .args(["/nologo", fo.as_str(), rc_s.as_str()])
                .status()
                .is_ok_and(|s| s.success())
        });
        ok.then_some(res)
    } else {
        let obj = out_dir.join("mailcraft-icon.o");
        let obj_s = obj.display().to_string();
        let target = std::env::var("TARGET").unwrap_or_default();
        let prefixed = format!("{}-w64-mingw32-windres", target.split('-').next().unwrap_or("x86_64"));
        let ok = [prefixed.as_str(), "windres"].iter().any(|tool| {
            Command::new(tool)
                .args(["-O", "coff", "-i", rc_s.as_str(), "-o", obj_s.as_str()])
                .status()
                .is_ok_and(|s| s.success())
        });
        ok.then_some(obj)
    };
    match compiled {
        Some(path) => println!("cargo:rustc-link-arg-bins={}", path.display()),
        None => {
            println!("cargo:warning=no resource compiler found (rc.exe/llvm-rc/windres); no icon resource")
        }
    }
}
