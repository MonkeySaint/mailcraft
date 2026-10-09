use proptest::prelude::*;

use crate::text::{PREVIEW_CHARS, collapse_whitespace, contains_ci, preview_of, strip_html};

#[test]
fn strips_tags_and_hidden_elements() {
    let html = "<html><head><title>T</title><style>p{color:red}</style></head>\
                <body><p>Hello&nbsp;<b>world</b></p><script>alert(1)</script><!-- note --><div>Bye</div></body></html>";
    assert_eq!(collapse_whitespace(&strip_html(html)), "Hello world Bye");
}

#[test]
fn hidden_element_case_insensitive_and_unterminated() {
    assert_eq!(collapse_whitespace(&strip_html("a<STYLE type=x>b</Style >c")), "a c");
    assert_eq!(collapse_whitespace(&strip_html("a<script>never closed")), "a");
    assert_eq!(collapse_whitespace(&strip_html("a<!-- open comment")), "a");
    assert_eq!(collapse_whitespace(&strip_html("a<b unterminated")), "a");
}

#[test]
fn self_closing_hidden_tag_does_not_swallow() {
    assert_eq!(collapse_whitespace(&strip_html("a<style/>b")), "a b");
}

#[test]
fn entities_decode() {
    assert_eq!(
        strip_html("&lt;tag&gt; &amp; &quot;q&quot; &#39;s&#x27; &#8212; &euro;"),
        "<tag> & \"q\" 's' \u{2014} \u{20ac}"
    );
    assert_eq!(strip_html("AT&T &bogus; &#xZZ; &#0;"), "AT&T &bogus; &#xZZ; \u{fffd}");
    assert_eq!(strip_html("&#1114112;"), "\u{fffd}");
}

#[test]
fn preview_collapses_and_truncates() {
    assert_eq!(preview_of("  a \n\n b\t c  ", 255), "a b c");
    let long = "word ".repeat(200);
    let p = preview_of(&long, PREVIEW_CHARS);
    assert_eq!(p.chars().count(), PREVIEW_CHARS - 1); // never ends in a dangling space
    assert!(!p.ends_with(' '));
    assert_eq!(preview_of("ééééé", 3), "ééé");
    assert_eq!(preview_of("", 3), "");
}

#[test]
fn case_insensitive_contains() {
    assert!(contains_ci("Quarterly BUDGET review", "budget"));
    assert!(contains_ci("GRÜẞE AUS KÖLN", "grüße"));
    assert!(contains_ci("ÉCOLE", "école"));
    assert!(!contains_ci("école", "ecole"));
    assert!(!contains_ci("abc", ""));
    assert!(!contains_ci("abc", "   "));
}

proptest! {
    #[test]
    fn strip_never_panics(s in any::<String>()) {
        let out = strip_html(&s);
        let _ = preview_of(&out, PREVIEW_CHARS);
    }

    #[test]
    fn strip_hostile_markup(parts in prop::collection::vec(prop_oneof![
        Just("<"), Just(">"), Just("<style>"), Just("</style>"), Just("<!--"), Just("-->"), Just("&"),
        Just("&#"), Just(";"), Just("x"), Just("é"), Just("&amp;"), Just("<script"), Just("/>"),
    ], 0..200)) {
        let s = parts.concat();
        let out = strip_html(&s);
        prop_assert!(!out.contains("<style>"));
    }

    #[test]
    fn preview_bounded(s in any::<String>(), n in 0usize..300) {
        let p = preview_of(&s, n);
        prop_assert!(p.chars().count() <= n);
        prop_assert!(!p.starts_with(' ') && !p.ends_with(' '));
    }
}

#[test]
fn entity_followed_by_multibyte_text_decodes() {
    // The entity scan window must not depend on where the following characters' byte boundaries fall.
    assert_eq!(strip_html("&amp;中文字"), "&中文字");
    assert_eq!(strip_html("&lt;éééééé"), "<éééééé");
    assert_eq!(strip_html("&é€€€€€;"), "&é€€€€€;");
}

#[test]
fn hidden_element_needs_exact_close_tag() {
    // "</scripts>" is not the end of a script element; only "</script" followed by space, "/" or ">" is.
    assert_eq!(collapse_whitespace(&strip_html("a<script>x=\"</scripts>\";</script>b")), "a b");
    assert_eq!(collapse_whitespace(&strip_html("a<head><title>t</title></header>x</head>b")), "a b");
    assert_eq!(collapse_whitespace(&strip_html("a<style>p{}</style\n>b")), "a b");
}
