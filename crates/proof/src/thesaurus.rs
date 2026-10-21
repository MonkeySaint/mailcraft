//! The bundled English thesaurus (Review › Thesaurus): about 23,600 head words with their ten closest
//! related words, from the public-domain Moby Thesaurus II (`assets/dictionaries/en-thesaurus.txt`, built by
//! `examples/build_thesaurus.rs`).
//!
//! [`lookup`] finds a word case-insensitively, falling back to simple base forms (plural `-s`/`-es`/`-ies`,
//! `-ed`, `-ing`, `-ly`) so "meetings" finds "meeting".

use std::sync::OnceLock;

static DATA: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/dictionaries/en-thesaurus.txt"));

/// Longest word looked up (characters).
const MAX_LOOKUP: usize = 64;

/// The thesaurus lines (`word\trelated,related,…`), sorted by head word.
fn lines() -> &'static [&'static str] {
    static LINES: OnceLock<Vec<&'static str>> = OnceLock::new();
    LINES.get_or_init(|| {
        let mut v: Vec<&'static str> = DATA.lines().filter(|l| l.contains('\t')).collect();
        v.sort_by(|a, b| head(a).cmp(head(b)));
        v
    })
}

fn head(line: &str) -> &str {
    line.split('\t').next().unwrap_or_default()
}

fn exact(word: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let l = lines();
    let i = l.binary_search_by(|line| head(line).cmp(word)).ok()?;
    let line = l.get(i)?;
    let (h, rest) = line.split_once('\t')?;
    Some((h, rest.split(',').filter(|w| !w.is_empty()).collect()))
}

/// Base forms to try for `w` (already lower-case), most likely first.
fn bases(w: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |s: Option<&str>, tail: &str| {
        if let Some(s) = s.filter(|s| s.len() >= 2) {
            out.push(format!("{s}{tail}"));
        }
    };
    add(w.strip_suffix("ies"), "y");
    add(w.strip_suffix("es"), "");
    add(w.strip_suffix('s'), "");
    add(w.strip_suffix("ied"), "y");
    add(w.strip_suffix("ed"), "");
    add(w.strip_suffix("ed"), "e");
    add(w.strip_suffix('d'), "");
    add(w.strip_suffix("ing"), "");
    add(w.strip_suffix("ing"), "e");
    add(w.strip_suffix("ily"), "y");
    add(w.strip_suffix("ly"), "");
    out
}

/// The head word found for `word` and its related words (`None` when the thesaurus has nothing).
pub fn lookup(word: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let w: String = word.trim().chars().take(MAX_LOOKUP).collect::<String>().to_lowercase();
    if w.is_empty() {
        return None;
    }
    exact(&w).or_else(|| bases(&w).iter().find_map(|b| exact(b)))
}

/// Number of head words.
pub fn len() -> usize {
    lines().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_words_have_synonyms() {
        assert!(len() > 20_000);
        for w in ["good", "problem", "meeting", "help", "send", "happy"] {
            let (h, syn) = lookup(w).unwrap_or_else(|| panic!("{w}"));
            assert_eq!(h, w);
            assert!(!syn.is_empty() && syn.len() <= 10, "{w}: {syn:?}");
        }
        assert!(lookup("problem").unwrap().1.contains(&"difficulty"));
    }

    #[test]
    fn case_and_base_forms() {
        assert_eq!(lookup("  GOOD ").map(|(h, _)| h), Some("good"));
        assert_eq!(lookup("meetings").map(|(h, _)| h), Some("meeting"));
        assert_eq!(lookup("helped").map(|(h, _)| h), Some("help"));
        assert!(lookup("").is_none());
        assert!(lookup("zzzzqx").is_none());
        assert!(lookup(&"é".repeat(10_000)).is_none(), "long or odd input is fine");
    }
}
