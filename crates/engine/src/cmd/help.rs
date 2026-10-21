//! `help.*`: bundled help, getting-started material, release notes, feedback and support information, about.

use serde_json::{Value, json};

use crate::cmd::rules::dialog;
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, CommandSpec, Session, always};

/// Version of the running build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The project's issue tracker (Feedback, Contact Support).
pub const ISSUES_URL: &str = "https://github.com/MonkeySaint/mailcraft/issues";
/// Page that starts a new issue (Feedback › Open a New Issue adds `title` and `body`).
pub const NEW_ISSUE_URL: &str = "https://github.com/MonkeySaint/mailcraft/issues/new";

/// The release notes bundled with the build (`CHANGELOG.md`).
pub const CHANGELOG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md"));

/// Most release sections and lines What's New shows.
const MAX_SECTIONS: usize = 3;
const MAX_LINES: usize = 40;

/// Release sections of a changelog: ("0.1.1", bullet lines), newest first. A bullet continues on indented
/// lines; text before the first `## ` heading is skipped.
pub fn release_notes(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines().take(5_000) {
        if let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().chars().take(80).collect(), Vec::new()));
            continue;
        }
        let Some((_, items)) = out.last_mut() else { continue };
        let t = line.trim();
        if let Some(item) = t.strip_prefix("- ") {
            items.push(item.trim().to_owned());
        } else if !t.is_empty()
            && line.starts_with(' ')
            && let Some(last) = items.last_mut()
        {
            last.push(' ');
            last.push_str(t);
        }
    }
    out.retain(|(_, items)| !items.is_empty());
    out
}

/// `%`-encodes `s` for a URL query value (RFC 3986 unreserved characters stay).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Longest issue text put into a new-issue link, in bytes once `%`-encoded (issue trackers refuse links much
/// past 8 KB). Longer text is cut to fit and the whole text goes to the clipboard ([`new_issue_link`]).
pub const MAX_ISSUE_TEXT: usize = 6_000;

/// Line ending a body that was cut to fit the link.
pub const ISSUE_TEXT_CUT: &str =
    "\n\n[The text was too long for the link. Paste the full text from the clipboard.]";

/// A new-issue link with `title` and `body` filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueLink {
    pub url: String,
    /// False when the body was cut to fit: the caller copies the whole text to the clipboard and says so.
    pub whole: bool,
}

/// A link that starts a new issue with `title` and `body` filled in. The title is cut to 120 characters and
/// the encoded body to [`MAX_ISSUE_TEXT`] bytes (on a character boundary, ending with [`ISSUE_TEXT_CUT`]).
pub fn new_issue_link(title: &str, body: &str) -> IssueLink {
    let title: String = title.chars().filter(|c| !c.is_control()).take(120).collect();
    let mut encoded = url_encode(body);
    let whole = encoded.len() <= MAX_ISSUE_TEXT;
    if !whole {
        let marker = url_encode(ISSUE_TEXT_CUT);
        let room = MAX_ISSUE_TEXT.saturating_sub(marker.len());
        encoded.clear();
        let mut buf = [0u8; 4];
        for c in body.chars() {
            let e = url_encode(c.encode_utf8(&mut buf));
            if encoded.len().saturating_add(e.len()) > room {
                break;
            }
            encoded.push_str(&e);
        }
        encoded.push_str(&marker);
    }
    IssueLink { url: format!("{NEW_ISSUE_URL}?title={}&body={encoded}", url_encode(&title)), whole }
}

fn spec(
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
    params: &'static str,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec {
        id, label, location: "Help › Help", shortcut, params, mutates: false, enabled: always, run
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec("help.help", "Help", "F1", r#"{"topic"?: string}"#, help),
        spec("help.about", "About MailCraft", "", "", about),
        spec("help.feedback", "Feedback", "", r#"{"kind"?: "feedback"|"suggestion"}"#, feedback),
        spec("help.showTraining", "Show Training", "", "", training),
        spec("help.whatsNew", "What's New", "", "", whats_new),
        spec("help.contactSupport", "Contact Support", "", "", contact_support),
    ]
}

fn about_json() -> Value {
    json!({
        "name": "MailCraft",
        "version": VERSION,
        "license": env!("CARGO_PKG_LICENSE"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

/// Help opens at `topic`, else at the topic of the module showing (F1 in the Calendar opens the calendar
/// topic).
fn help(s: &mut Session, p: &Value) -> CmdResult {
    let topic = opt_str(p, "topic")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(64).collect::<String>());
    let topic = topic.or_else(|| {
        match s.view.module {
            crate::Module::Calendar => Some("calendar"),
            crate::Module::People => Some("people"),
            crate::Module::Tasks => Some("tasks"),
            crate::Module::Notes => Some("notes"),
            _ => None,
        }
        .map(str::to_owned)
    });
    dialog(s, "help", json!({"topic": topic}))
}

fn about(s: &mut Session, _: &Value) -> CmdResult {
    let info = about_json();
    s.notify(crate::Notice::OpenDialog("about".into(), info.clone()));
    Ok(info)
}

fn feedback(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("feedback");
    if kind != "feedback" && kind != "suggestion" {
        return Err(CmdError::Params(format!("unknown feedback kind `{kind}`")));
    }
    dialog(
        s,
        "feedback",
        json!({"kind": kind, "about": about_json(), "url": ISSUES_URL, "newIssue": NEW_ISSUE_URL}),
    )
}

fn training(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "help", json!({"topic": "training"}))
}

/// What's New: the newest release sections of the bundled changelog.
fn whats_new(s: &mut Session, _: &Value) -> CmdResult {
    let notes: Vec<Value> = release_notes(CHANGELOG)
        .into_iter()
        .take(MAX_SECTIONS)
        .map(|(version, items)| json!({"version": version, "items": items.into_iter().take(MAX_LINES).collect::<Vec<_>>()}))
        .collect();
    dialog(s, "whatsNew", json!({"version": VERSION, "notes": notes, "url": ISSUES_URL}))
}

fn contact_support(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "contactSupport", json!({"about": about_json(), "url": ISSUES_URL, "newIssue": NEW_ISSUE_URL}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;

    #[test]
    fn issue_links_are_encoded_and_capped() {
        let u = new_issue_link("Crash & burn", "a b\n#1?");
        assert_eq!(u.url, format!("{NEW_ISSUE_URL}?title=Crash%20%26%20burn&body=a%20b%0A%231%3F"));
        assert!(u.whole);
        let long = new_issue_link("t", &"é".repeat(10_000));
        assert!(!long.whole);
        assert!(long.url.matches("%C3%A9").count().saturating_mul(6) <= MAX_ISSUE_TEXT);
    }

    #[test]
    fn about_reports_version() {
        let (mut s, _, _) = session();
        let r = s.run("help.about", &json!({})).unwrap();
        assert_eq!(r["version"], VERSION);
        assert_eq!(r["name"], "MailCraft");
    }

    #[test]
    fn dialogs() {
        let (mut s, _, _) = session();
        assert_eq!(s.registry.by_shortcut("F1").map(|c| c.id), Some("help.help"));
        assert_eq!(s.run("help.help", &json!({})).unwrap()["dialog"], "help");
        // Help opens at the topic of the module in view.
        s.view.module = crate::Module::People;
        s.run("help.help", &json!({})).unwrap();
        let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["topic"], "people");
        s.view.module = crate::Module::Mail;
        assert_eq!(s.run("help.showTraining", &json!({})).unwrap()["params"]["topic"], "training");
        let r = s.run("help.whatsNew", &json!({})).unwrap();
        assert_eq!(r["params"]["version"], VERSION);
        assert!(!r["params"]["notes"].as_array().unwrap().is_empty(), "release notes come from CHANGELOG.md");
        assert_eq!(s.run("help.contactSupport", &json!({})).unwrap()["params"]["url"], ISSUES_URL);
        assert_eq!(
            s.run("help.feedback", &json!({"kind": "suggestion"})).unwrap()["params"]["kind"],
            "suggestion"
        );
        assert!(matches!(s.run("help.feedback", &json!({"kind": "rant"})), Err(CmdError::Params(_))));
        assert_eq!(s.run("help.contactSupport", &json!({})).unwrap()["dialog"], "contactSupport");
    }

    #[test]
    fn help_opens_at_the_module_topic() {
        let (mut s, _, _) = session();
        assert!(s.run("help.help", &json!({})).unwrap()["params"]["topic"].is_null());
        s.view.module = crate::Module::Calendar;
        assert_eq!(s.run("help.help", &json!({})).unwrap()["params"]["topic"], "calendar");
        assert_eq!(s.run("help.help", &json!({"topic": "rules"})).unwrap()["params"]["topic"], "rules");
    }

    #[test]
    fn release_notes_parse() {
        let text = "# Changelog\nintro\n\n## 1.2\n\n- One\n  more\n- Two\n\n## 1.1\n- Old\n## Empty\n";
        let n = release_notes(text);
        assert_eq!(
            n,
            vec![
                ("1.2".to_owned(), vec!["One more".to_owned(), "Two".to_owned()]),
                ("1.1".to_owned(), vec!["Old".to_owned()]),
            ]
        );
        assert!(release_notes("").is_empty());
        assert!(!release_notes(CHANGELOG).is_empty());
    }

    #[test]
    fn issue_links_are_encoded() {
        assert_eq!(url_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        let link = new_issue_link("Crash\n on open", "short note");
        assert!(link.whole);
        assert!(link.url.starts_with(NEW_ISSUE_URL));
        assert!(link.url.contains("title=Crash%20on%20open"));
        assert!(link.url.ends_with("body=short%20note"));
    }

    /// Regression: long or non-ASCII feedback made links of 12-36 KB that trackers refuse, and the text past
    /// 4,000 characters was dropped without a word.
    #[test]
    fn long_issue_text_is_cut_to_a_bounded_link() {
        for body in ["x".repeat(10_000), "\u{1F600}".repeat(4_000), format!("{}\u{e9}", "a".repeat(5_999))] {
            let link = new_issue_link("Feedback", &body);
            assert!(!link.whole);
            assert!(link.url.len() <= NEW_ISSUE_URL.len() + 200 + MAX_ISSUE_TEXT, "{}", link.url.len());
            assert!(link.url.ends_with(&url_encode(ISSUE_TEXT_CUT)));
            // Cut on a character boundary: the encoded body decodes to valid UTF-8.
            let body_part = link.url.split("&body=").nth(1).unwrap();
            let bytes: Vec<u8> = body_part
                .split('%')
                .enumerate()
                .flat_map(|(i, p)| {
                    if i == 0 {
                        p.as_bytes().to_vec()
                    } else {
                        let mut v = vec![u8::from_str_radix(&p[..2], 16).unwrap()];
                        v.extend_from_slice(&p.as_bytes()[2..]);
                        v
                    }
                })
                .collect();
            assert!(String::from_utf8(bytes).is_ok());
        }
        let exact = "a".repeat(MAX_ISSUE_TEXT);
        assert!(new_issue_link("Feedback", &exact).whole);
    }
}
