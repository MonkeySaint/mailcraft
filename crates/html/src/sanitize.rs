//! Sanitising policy: which elements are dropped (with their content) and which URLs survive.
//!
//! Event-handler attributes (`on*`) and everything else not explicitly read by the converter never reach the
//! document, so the policy here is about elements and URL schemes.

/// Elements removed together with everything inside them.
pub(crate) fn is_dropped(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "title"
            | "head"
            | "iframe"
            | "frame"
            | "frameset"
            | "object"
            | "embed"
            | "applet"
            | "noembed"
            | "noframes"
            | "input"
            | "button"
            | "select"
            | "option"
            | "optgroup"
            | "textarea"
            | "datalist"
            | "keygen"
            | "output"
            | "progress"
            | "meter"
            | "meta"
            | "link"
            | "base"
            | "basefont"
            | "bgsound"
            | "template"
            | "svg"
            | "math"
            | "audio"
            | "video"
            | "source"
            | "track"
            | "canvas"
            | "param"
            | "map"
            | "area"
            | "xml"
            | "portal"
    )
}

/// Strips the characters browsers ignore when reading a URL scheme (ASCII whitespace and C0 controls).
fn scheme_of(url: &str) -> Option<String> {
    let cleaned: String = url.chars().filter(|c| !c.is_ascii_control() && *c != ' ').take(64).collect();
    let colon = cleaned.find(':')?;
    let scheme = cleaned.get(..colon)?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// Link targets that are safe to keep. Relative URLs and fragments are allowed; `javascript:`,
/// `vbscript:`, `data:` and unknown schemes are not.
pub(crate) fn safe_href(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    match scheme_of(href) {
        None => Some(href.to_owned()),
        Some(s)
            if matches!(
                s.as_str(),
                "http" | "https" | "mailto" | "ftp" | "tel" | "sms" | "news" | "cid" | "webcal"
            ) =>
        {
            Some(href.to_owned())
        }
        Some(_) => None,
    }
}

/// Image sources that are safe to keep: http(s), `cid:` and `data:image/*`.
pub(crate) fn safe_image_src(src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    match scheme_of(src).as_deref() {
        None => Some(src.to_owned()),
        Some("http" | "https" | "cid") => Some(src.to_owned()),
        Some("data") => {
            let rest: String =
                src.chars().filter(|c| !c.is_ascii_control() && *c != ' ').skip(5).take(6).collect();
            rest.eq_ignore_ascii_case("image/").then(|| src.to_owned())
        }
        Some(_) => None,
    }
}

/// `http:`, `https:` or protocol-relative `//host/…` URLs.
pub(crate) fn is_remote(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("//") || matches!(scheme_of(u).as_deref(), Some("http" | "https"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs() {
        assert_eq!(safe_href("https://example.com/?a=1").as_deref(), Some("https://example.com/?a=1"));
        assert_eq!(safe_href("MAILTO:a@b.c").as_deref(), Some("MAILTO:a@b.c"));
        assert_eq!(safe_href("#top").as_deref(), Some("#top"));
        assert_eq!(safe_href("page.html").as_deref(), Some("page.html"));
        for bad in [
            "javascript:alert(1)",
            " JaVaScRiPt:x",
            "java\tscript:x",
            "jav\nascript:x",
            "vbscript:msgbox",
            "data:text/html,<script>",
            "\u{1}javascript:x",
            "file:///etc/passwd",
            "livescript:x",
        ] {
            assert_eq!(safe_href(bad), None, "{bad:?}");
        }
        assert_eq!(safe_href("  "), None);
    }

    #[test]
    fn image_sources() {
        assert!(safe_image_src("cid:image001.png@01D").is_some());
        assert!(safe_image_src("https://x/y.png").is_some());
        assert!(safe_image_src("data:image/png;base64,AAAA").is_some());
        assert!(safe_image_src("DATA: Image/GIF;base64,AAAA").is_some());
        assert!(safe_image_src("data:text/html;base64,AAAA").is_none());
        assert!(safe_image_src("javascript:alert(1)").is_none());
        assert!(safe_image_src("").is_none());
    }

    #[test]
    fn remote() {
        assert!(is_remote("http://a"));
        assert!(is_remote(" HTTPS://a"));
        assert!(is_remote("//cdn.example.com/x.png"));
        assert!(!is_remote("cid:x"));
        assert!(!is_remote("data:image/png;base64,"));
        assert!(!is_remote("img/x.png"));
    }

    #[test]
    fn dropped() {
        for n in [
            "script", "style", "iframe", "object", "embed", "input", "button", "select", "textarea", "meta",
            "link", "base", "form-not",
        ] {
            assert_eq!(is_dropped(n), n != "form-not", "{n}");
        }
        assert!(!is_dropped("form"));
        assert!(!is_dropped("noscript"));
    }
}
