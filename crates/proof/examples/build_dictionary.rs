//! Rebuilds the bundled en-US word lists in `assets/dictionaries/` from the public-domain Moby Words II lists
//! (Project Gutenberg ebook #3201) plus the MailCraft supplement.
//!
//! ```text
//! # download into an empty folder:
//! #   https://www.gutenberg.org/files/3201/files/{crosswd,crswd-d,common,names,places,freq,freq-int}.txt
//! cargo run -p mailcraft-proof --example build_dictionary -- <moby-folder> assets/dictionaries
//! ```
//!
//! Output (all sorted, one word per line, `\n` line ends):
//! - `en-US.txt`: lower-case single words (letters, optionally one inner apostrophe) from `crosswd.txt`,
//!   `crswd-d.txt`, the lower-case entries of `common.txt`, `freq.txt` and the supplement.
//! - `en-US-proper.txt`: Capitalised single words (names, places, proper nouns) from `names.txt`,
//!   `places.txt`, the Capitalised entries of `common.txt` and the supplement, minus any whose lower-case form
//!   is already in `en-US.txt` (a Capitalised common word is accepted anyway).
//! - `en-US-freq.txt`: the words of `freq-int.txt` (1992 Usenet frequencies) then the remaining words of
//!   `freq.txt` (general texts, base forms), most frequent first (used to rank suggestions).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

type Res<T> = Result<T, String>;

fn read(dir: &Path, name: &str) -> Res<String> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The Moby files are ASCII except for a few Latin-1 bytes in names.txt; those lines are dropped below.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// `[a-z]+` with at most one inner apostrophe (`o'clock`).
fn is_common_word(w: &str) -> bool {
    let mut parts = w.split('\'');
    let ok = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()));
    let first = parts.next();
    let second = parts.next();
    ok(first) && (second.is_none() || ok(second)) && parts.next().is_none()
}

/// `[A-Z][a-z]+`.
fn is_proper_word(w: &str) -> bool {
    let mut b = w.bytes();
    b.next().is_some_and(|c| c.is_ascii_uppercase()) && w.len() > 1 && b.all(|c| c.is_ascii_lowercase())
}

fn write(path: PathBuf, words: impl IntoIterator<Item = String>) -> Res<()> {
    let mut out = String::new();
    for w in words {
        out.push_str(&w);
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
}

fn run() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(src), Some(dst)) = (args.get(1), args.get(2)) else {
        return Err("usage: build_dictionary <moby-folder> <assets/dictionaries>".to_owned());
    };
    let src = Path::new(src);
    let dst = Path::new(dst);

    let mut common = BTreeSet::new();
    let mut proper = BTreeSet::new();
    for name in ["crosswd.txt", "crswd-d.txt", "common.txt", "freq.txt"] {
        let text = read(src, name)?;
        for w in lines(&text) {
            if is_common_word(w) {
                common.insert(w.to_owned());
            } else if is_proper_word(w) {
                proper.insert(w.to_owned());
            }
        }
    }
    for name in ["names.txt", "places.txt"] {
        let text = read(src, name)?;
        proper.extend(lines(&text).filter(|w| is_proper_word(w)).map(str::to_owned));
    }
    let supplement = read(dst, "en-US-supplement.txt")?;
    for w in lines(&supplement) {
        if is_common_word(w) {
            common.insert(w.to_owned());
        } else if is_proper_word(w) {
            proper.insert(w.to_owned());
        }
    }
    proper.retain(|w| !common.contains(&w.to_ascii_lowercase()));

    let mut seen = BTreeSet::new();
    let mut freq = Vec::new();
    for name in ["freq-int.txt", "freq.txt"] {
        let text = read(src, name)?;
        // freq-int.txt lines are "<percent> <word>"; freq.txt lines are "<word>".
        for line in lines(&text) {
            let Some(w) = line.split_whitespace().last() else { continue };
            let w = w.to_ascii_lowercase();
            if w.bytes().all(|b| b.is_ascii_lowercase()) && seen.insert(w.clone()) {
                freq.push(w);
            }
        }
    }
    freq.retain(|w| w == "i" || common.contains(w));

    println!(
        "en-US.txt: {} words, en-US-proper.txt: {} words, en-US-freq.txt: {} words",
        common.len(),
        proper.len(),
        freq.len()
    );
    write(dst.join("en-US.txt"), common)?;
    write(dst.join("en-US-proper.txt"), proper)?;
    write(dst.join("en-US-freq.txt"), freq)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("build_dictionary: {e}");
        std::process::exit(1);
    }
}
