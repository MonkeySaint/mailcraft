//! Installed fonts for the compose Font list (`mailcraft_ui_egui::compose::set_installed_fonts_reader`): a
//! background thread walks the platform's font folders once at start-up and reads each font file's family
//! name (`mailcraft_fonts::family_name`). Until it finishes the list holds only the fonts MailCraft knows.
//! Every failure (unreadable folder or file, not a font) just leaves that entry out. The walk is bounded:
//! folder depth, number of files and file size are capped.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Deepest folder level walked below a font folder.
const MAX_DEPTH: usize = 6;
/// Most font files read.
const MAX_FILES: usize = 4_000;

static FONTS: OnceLock<Vec<String>> = OnceLock::new();

/// The platform's font folders.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "windows") {
        if let Some(w) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(w).join("Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        if let Some(h) = &home {
            dirs.push(h.join("Library").join("Fonts"));
        }
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(d).join("fonts"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".local").join("share").join("fonts"));
            dirs.push(h.join(".fonts"));
        }
    }
    dirs
}

fn is_font_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["ttf", "otf", "ttc", "otc"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Font files under `dirs` (bounded walk, symbolic links not followed).
pub fn font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() && is_font_file(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out
}

/// Family names of the font files under `dirs`.
pub fn scan(dirs: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for path in font_files(dirs) {
        let small = std::fs::metadata(&path)
            .is_ok_and(|m| u64::try_from(mailcraft_fonts::MAX_FONT_FILE).is_ok_and(|max| m.len() <= max));
        if !small {
            continue;
        }
        if let Some(name) = std::fs::read(&path).ok().and_then(|d| mailcraft_fonts::family_name(&d)) {
            names.push(name);
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

fn read() -> Vec<String> {
    FONTS.get().cloned().unwrap_or_default()
}

/// Starts the scan and installs the reader (call once at start-up).
pub fn install() {
    mailcraft_ui_egui::compose::set_installed_fonts_reader(Some(read));
    let spawned = std::thread::Builder::new().name("mailcraft-font-scan".into()).spawn(|| {
        let _ = FONTS.set(scan(&font_dirs()));
    });
    if let Err(e) = spawned {
        log::debug!("font scan not started: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_font_folders_and_skips_other_files() {
        let dir = std::env::temp_dir().join(format!("mailcraft-fontscan-{}", std::process::id()));
        let sub = dir.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        // A real font (one of the bundled ones), a fake font and a text file.
        let inter =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Inter-Regular.ttf"))
                .unwrap();
        std::fs::write(sub.join("Inter.ttf"), inter).unwrap();
        std::fs::write(dir.join("broken.otf"), b"not a font").unwrap();
        std::fs::write(dir.join("readme.txt"), b"hello").unwrap();
        assert_eq!(font_files(std::slice::from_ref(&dir)).len(), 2);
        assert_eq!(scan(std::slice::from_ref(&dir)), vec!["Inter".to_owned()]);
        assert!(scan(&[dir.join("missing")]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
