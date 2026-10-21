//! Rebuilds the bundled English thesaurus `assets/dictionaries/en-thesaurus.txt` from the public-domain Moby
//! Thesaurus II (Project Gutenberg ebook #3202, `mthesaur.txt`).
//!
//! ```text
//! # download into an empty folder: https://www.gutenberg.org/files/3202/files/mthesaur.txt
//! cargo run -p mailcraft-proof --example build_thesaurus -- <folder>/mthesaur.txt assets/dictionaries
//! ```
//!
//! Each Moby line is `root,word,word,…` with the related words in alphabetical order. The builder keeps
//! roots that are single lower-case words of the spelling dictionary (`en-US.txt`) and, for each, the
//! [`KEEP`] closest related words: single words before phrases, then by how many related words the two
//! share (cosine of their lists; a word that lists the root back counts extra) weighted by how common the word
//! is (the log of how many lists it is in), then shorter words. Output:
//! `root\tword,word,…` per line, sorted by root, `\n` line ends.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Related words kept per root.
const KEEP: usize = 10;
/// Longest root kept (characters).
const MAX_ROOT: usize = 16;

fn main() {
    if let Err(e) = run() {
        eprintln!("build_thesaurus: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(out)) = (args.next(), args.next()) else {
        return Err("usage: build_thesaurus <mthesaur.txt> <assets/dictionaries>".into());
    };
    let out = PathBuf::from(out);
    let bytes = std::fs::read(&src).map_err(|e| format!("{src}: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let read = |name: &str| std::fs::read_to_string(out.join(name)).map_err(|e| format!("{name}: {e}"));
    let dict: HashSet<String> = read("en-US.txt")?.lines().map(str::to_owned).collect();
    let entries: Vec<(String, Vec<String>)> = text
        .lines()
        .filter_map(|l| {
            let mut parts = l.split(',').map(str::trim).filter(|p| !p.is_empty());
            let root = parts.next()?.to_owned();
            Some((root, parts.map(str::to_owned).collect()))
        })
        .collect();
    let lists: HashMap<&str, HashSet<&str>> =
        entries.iter().map(|(r, ws)| (r.as_str(), ws.iter().map(String::as_str).collect())).collect();
    // How many lists a word is in: a stand-in for how common it is.
    let mut degree: HashMap<&str, usize> = HashMap::new();
    for (_, ws) in &entries {
        for w in ws {
            *degree.entry(w.as_str()).or_default() += 1;
        }
    }
    let mut lines = Vec::new();
    for (root, words) in &entries {
        let single = |w: &str| w.bytes().all(|b| b.is_ascii_lowercase());
        if !single(root) || root.len() > MAX_ROOT || !dict.contains(root) {
            continue;
        }
        // Closeness: how many related words the two share (cosine of the two lists), so near-synonyms
        // come before loosely related words; single words before phrases.
        let mine = lists.get(root.as_str());
        let mut ranked: Vec<(bool, i64, usize, &String)> = words
            .iter()
            .filter(|w| w.as_str() != root && !w.contains(['|', '\t']))
            .map(|w| {
                let theirs = lists.get(w.as_str());
                let score = match (mine, theirs) {
                    (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => {
                        let shared = a.iter().filter(|x| b.contains(*x)).count() as f64;
                        let mutual = if b.contains(root.as_str()) { 1.0 } else { 0.0 };
                        (shared + 3.0 * mutual) / ((a.len() * b.len()) as f64).sqrt()
                    }
                    _ => 0.0,
                };
                let common = (1.0 + degree.get(w.as_str()).copied().unwrap_or(0) as f64).ln();
                (!single(w), -((score * common * 1_000_000.0) as i64), w.len(), w)
            })
            .collect();
        ranked.sort();
        let kept: Vec<&str> = ranked.iter().take(KEEP).map(|(.., w)| w.as_str()).collect();
        if !kept.is_empty() {
            lines.push(format!("{root}\t{}", kept.join(",")));
        }
    }
    lines.sort();
    lines.dedup_by(|a, b| a.split('\t').next() == b.split('\t').next());
    let path = out.join("en-thesaurus.txt");
    std::fs::write(&path, lines.join("\n") + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{}: {} entries", path.display(), lines.len());
    Ok(())
}
