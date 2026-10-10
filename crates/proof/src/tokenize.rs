//! Word tokenisation on Unicode word boundaries (UAX #29), with Internet and file addresses kept whole.

use unicode_segmentation::UnicodeSegmentation;

/// One token: byte range in the source text and the word itself.
pub type Token<'a> = (usize, usize, &'a str);

/// Splits `text` into words on Unicode word boundaries (UAX #29) and yields `(start, end, word)` with byte
/// offsets into `text`. Only segments containing a letter or digit are yielded, so spaces and punctuation are
/// skipped. Apostrophes inside a word stay in it (`don't`), hyphens split (`well-known` → `well`, `known`).
/// Internet and file addresses (`https://…`, `www.…`, `name@example.com`, `example.com`, `report.pdf`,
/// `/usr/bin`, `C:\…`) are yielded whole, so the spell checker can skip them.
pub fn tokenize(text: &str) -> impl Iterator<Item = Token<'_>> {
    let mut out = Vec::new();
    for (chunk_start, chunk) in whitespace_chunks(text) {
        let (core_off, core) = trim_address_punct(chunk);
        if !core.is_empty() && looks_like_address(core) {
            let start = chunk_start + core_off;
            out.push((start, start + core.len(), core));
            continue;
        }
        for (off, seg) in chunk.split_word_bound_indices() {
            if seg.chars().any(char::is_alphanumeric) {
                let start = chunk_start + off;
                out.push((start, start + seg.len(), seg));
            }
        }
    }
    out.into_iter()
}

/// Maximal runs of non-whitespace characters with their byte offsets.
fn whitespace_chunks(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start: Option<usize> = None;
    let mut chunks = Vec::new();
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                if let Some(chunk) = text.get(s..i) {
                    chunks.push((s, chunk));
                }
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start
        && let Some(chunk) = text.get(s..)
    {
        chunks.push((s, chunk));
    }
    chunks.into_iter()
}

/// Strips the punctuation that commonly wraps an address in prose (`(see https://x.org/a).` → the URL).
fn trim_address_punct(chunk: &str) -> (usize, &str) {
    const LEAD: &[char] = &['(', '[', '<', '"', '\'', '\u{2018}', '\u{201C}', '{'];
    const TRAIL: &[char] =
        &[')', ']', '>', '"', '\'', '\u{2019}', '\u{201D}', '}', '.', ',', ';', ':', '!', '?'];
    let trimmed = chunk.trim_start_matches(LEAD);
    let off = chunk.len() - trimmed.len();
    (off, trimmed.trim_end_matches(TRAIL))
}

const TLDS: &[&str] = &[
    "com",
    "org",
    "net",
    "edu",
    "gov",
    "mil",
    "int",
    "io",
    "co",
    "ca",
    "uk",
    "us",
    "de",
    "fr",
    "nl",
    "be",
    "ch",
    "at",
    "au",
    "nz",
    "jp",
    "cn",
    "in",
    "br",
    "es",
    "it",
    "se",
    "no",
    "dk",
    "fi",
    "ie",
    "pl",
    "ru",
    "info",
    "biz",
    "dev",
    "app",
    "ai",
    "me",
    "tv",
    "eu",
    "example",
    "test",
    "local",
    "localhost",
    "invalid",
];

const FILE_EXTENSIONS: &[&str] = &[
    "txt", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "csv", "tsv",
    "zip", "gz", "tar", "7z", "rar", "png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "tif", "tiff", "mp3",
    "mp4", "wav", "mov", "avi", "html", "htm", "xml", "json", "yaml", "yml", "toml", "md", "eml", "msg",
    "ics", "vcf", "exe", "msi", "dll", "sh", "bat", "ps1", "py", "rs", "js", "ts", "c", "h", "cpp", "java",
    "log", "ini", "cfg", "iso", "dmg", "deb", "rpm", "apk",
];

/// Whether `s` looks like a URL, an e-mail address, a domain or host name, a file name with a known
/// extension, or a file path.
pub fn looks_like_address(s: &str) -> bool {
    if s.chars().any(char::is_whitespace) || s.is_empty() {
        return false;
    }
    let lower = s.to_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.starts_with("mailto:") {
        return true;
    }
    // Paths: /usr/bin, ~/notes, ./a, C:\dir, \\server\share.
    let mut chars = s.chars();
    let first = chars.next();
    let second = chars.next();
    let third = chars.next();
    if (first == Some('/') && s.len() > 1 && s.get(1..).is_some_and(|r| r.contains('/')))
        || s.starts_with("~/")
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with("\\\\")
        || (first.is_some_and(|c| c.is_ascii_alphabetic()) && second == Some(':') && third == Some('\\'))
    {
        return true;
    }
    if let Some((local, domain)) = lower.rsplit_once('@') {
        return !local.is_empty() && domain.contains('.') && is_hostname(domain);
    }
    if lower.contains('.') {
        if is_hostname(&lower) {
            return true;
        }
        if let Some((stem, ext)) = lower.rsplit_once('.')
            && !stem.is_empty()
            && FILE_EXTENSIONS.contains(&ext)
            && !stem.ends_with('.')
        {
            return true;
        }
    }
    false
}

/// `label(.label)+` where labels are ASCII alphanumerics or hyphens and the last label is a known TLD; an
/// optional `:port` or `/path` tail is allowed.
fn is_hostname(s: &str) -> bool {
    let host = s.split(['/', ':', '?', '#']).next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let valid_labels =
        labels.iter().all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    valid_labels && labels.last().is_some_and(|tld| TLDS.contains(tld))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<&str> {
        tokenize(s).map(|(_, _, w)| w).collect()
    }

    #[test]
    fn basic_words() {
        assert_eq!(words("Hello, world! Don't stop."), ["Hello", "world", "Don't", "stop"]);
        assert_eq!(words("well-known  co-op"), ["well", "known", "co", "op"]);
        assert_eq!(words("it’s 3.5 km"), ["it’s", "3.5", "km"]);
        assert_eq!(words(""), Vec::<&str>::new());
    }

    #[test]
    fn offsets_are_bytes() {
        let text = "héllo wörld";
        for (s, e, w) in tokenize(text) {
            assert_eq!(&text[s..e], w);
        }
    }

    #[test]
    fn addresses_whole() {
        assert_eq!(words("see https://example.org/a?b=c."), ["see", "https://example.org/a?b=c"]);
        assert_eq!(words("mail bob@example.com now"), ["mail", "bob@example.com", "now"]);
        assert_eq!(words("(www.example.net)"), ["www.example.net"]);
        assert_eq!(words("open report.pdf"), ["open", "report.pdf"]);
        assert_eq!(words("in /usr/local/bin"), ["in", "/usr/local/bin"]);
        assert_eq!(words("at C:\\Users\\me"), ["at", "C:\\Users\\me"]);
        assert!(looks_like_address("example.com"));
        assert!(!looks_like_address("end.Start"));
        assert!(!looks_like_address("e.g"));
    }
}
