//! `help.*`: bundled help, getting-started material, release notes, feedback and support information, about.

use serde_json::{Value, json};

use crate::cmd::rules::dialog;
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, CommandSpec, Session, always};

/// Version of the running build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Where Feedback and Contact Support send a note: a new issue on the project's public tracker.
pub const ISSUE_TRACKER: &str = "https://github.com/MonkeySaint/mailcraft/issues/new";
/// Longest issue body put in the link (browsers and the tracker cut long URLs).
const MAX_ISSUE_BODY: usize = 4000;

fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(b)),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A link that opens a new issue on the tracker with `title` and `body` filled in (the body cut to
/// [`MAX_ISSUE_BODY`] characters).
pub fn issue_url(title: &str, body: &str) -> String {
    let body: String = body.chars().take(MAX_ISSUE_BODY).collect();
    let title: String = title.chars().take(200).collect();
    format!("{ISSUE_TRACKER}?title={}&body={}", url_encode(&title), url_encode(&body))
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

fn help(s: &mut Session, p: &Value) -> CmdResult {
    let topic = opt_str(p, "topic").map(str::trim).filter(|t| !t.is_empty());
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
    dialog(s, "feedback", json!({"kind": kind, "about": about_json()}))
}

fn training(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "help", json!({"topic": "getting-started"}))
}

fn whats_new(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "whatsNew", json!({"version": VERSION}))
}

fn contact_support(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "contactSupport", json!({"about": about_json()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;

    #[test]
    fn issue_links_are_encoded_and_capped() {
        let u = issue_url("Crash & burn", "a b\n#1?");
        assert_eq!(u, format!("{ISSUE_TRACKER}?title=Crash%20%26%20burn&body=a%20b%0A%231%3F"));
        let long = issue_url("t", &"é".repeat(10_000));
        assert_eq!(long.matches("%C3%A9").count(), MAX_ISSUE_BODY);
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
        assert_eq!(s.run("help.showTraining", &json!({})).unwrap()["params"]["topic"], "getting-started");
        assert_eq!(s.run("help.whatsNew", &json!({})).unwrap()["params"]["version"], VERSION);
        assert_eq!(
            s.run("help.feedback", &json!({"kind": "suggestion"})).unwrap()["params"]["kind"],
            "suggestion"
        );
        assert!(matches!(s.run("help.feedback", &json!({"kind": "rant"})), Err(CmdError::Params(_))));
        assert_eq!(s.run("help.contactSupport", &json!({})).unwrap()["dialog"], "contactSupport");
    }
}
