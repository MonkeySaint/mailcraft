//! Remote resources referenced by a message body (for the "pictures not downloaded" info bar).

use crate::css;
use crate::dom::attr;
use crate::sanitize::is_remote;
use crate::tokenizer::{Token, Tokenizer};
use std::collections::HashSet;

const MAX_RESOURCES: usize = 10_000;

fn push(out: &mut Vec<String>, seen: &mut HashSet<String>, url: &str) {
    let url = url.trim();
    if is_remote(url) && out.len() < MAX_RESOURCES && !seen.contains(url) {
        seen.insert(url.to_owned());
        out.push(url.to_owned());
    }
}

/// Remote URLs from `img src`/`srcset`, `background` attributes, `poster`, stylesheet links, inline styles
/// and `<style>` blocks, in document order without duplicates.
pub(crate) fn remote_resources(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut in_style = false;
    for tok in Tokenizer::new(html) {
        match tok {
            Token::Start { name, attrs, .. } => {
                in_style = name == "style";
                match name.as_str() {
                    "img" | "image" => {
                        if let Some(src) = attr(&attrs, "src") {
                            push(&mut out, &mut seen, src);
                        }
                        if let Some(set) = attr(&attrs, "srcset") {
                            for candidate in set.split(',') {
                                if let Some(u) = candidate.split_ascii_whitespace().next() {
                                    push(&mut out, &mut seen, u);
                                }
                            }
                        }
                    }
                    "input" if attr(&attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("image")) => {
                        if let Some(src) = attr(&attrs, "src") {
                            push(&mut out, &mut seen, src);
                        }
                    }
                    "link" => {
                        let rel = attr(&attrs, "rel").unwrap_or("").to_ascii_lowercase();
                        if rel.split_ascii_whitespace().any(|r| r == "stylesheet")
                            && let Some(href) = attr(&attrs, "href")
                        {
                            push(&mut out, &mut seen, href);
                        }
                    }
                    _ => {}
                }
                for key in ["background", "poster"] {
                    if let Some(u) = attr(&attrs, key) {
                        push(&mut out, &mut seen, u);
                    }
                }
                if let Some(style) = attr(&attrs, "style") {
                    for u in css::urls(style) {
                        push(&mut out, &mut seen, &u);
                    }
                }
            }
            Token::Text(t) if in_style => {
                for u in css::urls(&t) {
                    push(&mut out, &mut seen, &u);
                }
            }
            Token::End { .. } => in_style = false,
            _ => {}
        }
    }
    out
}
