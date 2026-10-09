//! `review.*`: proofing and reading tools. Spelling/grammar is a stub until the proofing engine lands
//! (commit 6) and reports no issues; Word Count, Accessibility, Thesaurus (small bundled list) and Language
//! work on the compose body. Read Aloud and Translate need platform services (speech, a translator backend):
//! the engine gathers the text and hands it to the UI through a dialog notice; a translator calls back with
//! `{"to", "text"}` to apply the translation.

use serde_json::{Value, json};

use super::compose::{compose_id, compose_mut, has_compose, rich_compose, spec};
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::{Doc, ItemId};

/// Minimum contrast ratio for body text (WCAG 2.x AA, normal text).
const MIN_CONTRAST: f32 = 4.5;
/// Paragraph count above which a body without headings is flagged.
const HEADINGS_AFTER_PARAGRAPHS: usize = 12;

/// Enabled: a compose is open or a message is selected.
fn compose_or_message(s: &Session) -> Option<&'static str> {
    let msg =
        s.selection.focus.iter().chain(s.selection.items.iter()).any(|id| s.mailbox.mail.contains_key(id));
    if msg || !s.composes.is_empty() { None } else { Some("select a message first") }
}

/// Text a reading tool acts on: the compose (when `compose` is given or no message is selected) or the
/// selected / given message. Returns (`{"compose": id}` or `{"item": id}`, text).
fn target_text(s: &Session, p: &Value) -> Result<(Value, String), CmdError> {
    let item = p
        .get("id")
        .and_then(Value::as_u64)
        .map(ItemId)
        .or(s.selection.focus)
        .or_else(|| s.selection.items.first().copied())
        .filter(|id| s.mailbox.mail.contains_key(id));
    if p.get("compose").is_some() || item.is_none() {
        let cid = compose_id(s, p)?;
        let c =
            s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no message is being composed".into()))?;
        let text = if c.selection.is_collapsed() { c.body.to_plain_text() } else { c.selected_text() };
        return Ok((json!({"compose": cid}), text));
    }
    let id = item.ok_or_else(|| CmdError::Disabled("select a message first".into()))?;
    let m = s.mailbox.mail.get(&id).ok_or_else(|| CmdError::Disabled("select a message first".into()))?;
    Ok((json!({"item": id.0}), m.body_text()))
}

fn spelling(s: &mut Session, p: &Value) -> CmdResult {
    let grammar = opt_bool(p, "grammar").unwrap_or(true);
    let c = compose_mut(s, p)?;
    let words = c.word_count().words;
    Ok(json!({"issues": [], "count": 0, "grammar": grammar, "wordsChecked": words, "language": c.language}))
}

fn word_count(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    let wc = c.word_count();
    let scope = if c.selection.is_collapsed() { "body" } else { "selection" };
    let counts = json!({
        "compose": cid,
        "scope": scope,
        "words": wc.words,
        "charactersNoSpaces": wc.characters_no_spaces,
        "charactersWithSpaces": wc.characters_with_spaces,
        "paragraphs": wc.paragraphs,
        "lines": wc.lines,
    });
    let _ = dialog(s, "wordCount", counts.clone())?;
    Ok(counts)
}

fn accessibility(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    let mut issues = Vec::new();
    for (src, alt) in c.images() {
        if alt.trim().is_empty() {
            issues.push(json!({"kind": "missingAltText", "severity": "error", "message": "A picture has no alternative text.", "target": src}));
        }
    }
    let paragraphs = c.word_count().paragraphs;
    let has_heading = c.body.blocks.iter().any(|b| matches!(b, mailcraft_model::Block::Heading { .. }));
    if paragraphs > HEADINGS_AFTER_PARAGRAPHS && !has_heading {
        issues.push(json!({"kind": "noHeadings", "severity": "tip", "message": "Long messages are easier to navigate with headings."}));
    }
    let bg = c.body.background.unwrap_or(Color::WHITE);
    for color in c.text_colors() {
        if color.contrast(&bg) < MIN_CONTRAST {
            issues.push(json!({"kind": "lowContrast", "severity": "warning", "message": "Some text is hard to read against the background.", "color": color.to_hex()}));
        }
    }
    let tables_without_border = c
        .body
        .blocks
        .iter()
        .filter(|b| matches!(b, mailcraft_model::Block::Table { rows, .. } if rows.len() > 1 && rows.first().is_some_and(|r| r.len() > 1)))
        .count();
    if tables_without_border > 0 {
        issues.push(json!({"kind": "tableHeader", "severity": "tip", "message": "Check that tables have a header row."}));
    }
    let report = json!({"compose": cid, "issues": issues});
    let _ = dialog(s, "accessibility", report.clone())?;
    Ok(report)
}

fn valid_lang(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 35
        && tag.split('-').all(|part| {
            !part.is_empty() && part.len() <= 8 && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

fn language(s: &mut Session, p: &Value) -> CmdResult {
    let Some(lang) = opt_str(p, "lang").map(str::to_owned) else {
        return dialog(s, "language", json!({"compose": compose_id(s, p)?}));
    };
    if !valid_lang(&lang) {
        return Err(CmdError::Params(format!("`{lang}` is not a language tag (e.g. en-US)")));
    }
    let c = compose_mut(s, p)?;
    c.language = Some(lang.clone());
    Ok(json!({"language": lang}))
}

fn read_aloud(s: &mut Session, p: &Value) -> CmdResult {
    let (target, text) = target_text(s, p)?;
    let mut payload = target;
    if let Some(o) = payload.as_object_mut() {
        o.insert("text".into(), Value::String(text));
        o.insert("controls".into(), json!(["play", "pause", "stop", "previous", "next"]));
    }
    s.notify(crate::Notice::OpenDialog("readAloud".into(), payload.clone()));
    Ok(json!({"dialog": "readAloud", "text": payload.get("text")}))
}

/// Small bundled thesaurus (common English words).
const THESAURUS: &[(&str, &[&str])] = &[
    ("good", &["fine", "great", "excellent", "decent", "positive"]),
    ("bad", &["poor", "awful", "inferior", "faulty", "unpleasant"]),
    ("big", &["large", "huge", "sizable", "substantial", "major"]),
    ("small", &["little", "minor", "compact", "slight", "modest"]),
    ("fast", &["quick", "rapid", "swift", "speedy", "prompt"]),
    ("slow", &["gradual", "unhurried", "sluggish", "leisurely"]),
    ("happy", &["glad", "pleased", "content", "cheerful", "delighted"]),
    ("sad", &["unhappy", "sorry", "downcast", "gloomy"]),
    ("important", &["significant", "key", "crucial", "essential", "major"]),
    ("help", &["assist", "support", "aid", "back"]),
    ("start", &["begin", "launch", "open", "commence", "kick off"]),
    ("end", &["finish", "close", "conclude", "complete", "wrap up"]),
    ("make", &["create", "build", "produce", "form", "prepare"]),
    ("use", &["apply", "employ", "utilize", "operate"]),
    ("show", &["display", "present", "reveal", "demonstrate"]),
    ("get", &["obtain", "receive", "acquire", "gain", "fetch"]),
    ("need", &["require", "want", "lack"]),
    ("change", &["alter", "modify", "adjust", "revise", "update"]),
    ("problem", &["issue", "difficulty", "trouble", "concern", "snag"]),
    ("idea", &["thought", "notion", "concept", "suggestion", "plan"]),
    ("easy", &["simple", "straightforward", "effortless", "painless"]),
    ("hard", &["difficult", "tough", "demanding", "challenging"]),
    ("new", &["fresh", "recent", "novel", "modern", "latest"]),
    ("old", &["former", "previous", "earlier", "aged", "outdated"]),
    ("meeting", &["session", "gathering", "conference", "call", "discussion"]),
    ("quickly", &["promptly", "rapidly", "swiftly", "soon"]),
    ("thanks", &["thank you", "much appreciated", "cheers"]),
    ("ask", &["request", "inquire", "query", "question"]),
    ("tell", &["inform", "notify", "advise", "let know"]),
    ("send", &["forward", "dispatch", "transmit", "mail"]),
    ("check", &["verify", "review", "confirm", "examine", "inspect"]),
    ("plan", &["schedule", "arrangement", "strategy", "proposal", "scheme"]),
    ("result", &["outcome", "effect", "consequence", "upshot"]),
    ("great", &["excellent", "superb", "wonderful", "terrific"]),
    ("nice", &["pleasant", "lovely", "kind", "agreeable"]),
    ("soon", &["shortly", "presently", "before long", "quickly"]),
    ("maybe", &["perhaps", "possibly", "potentially"]),
    ("very", &["extremely", "highly", "really", "truly"]),
    ("think", &["believe", "consider", "suppose", "reckon"]),
    ("want", &["wish", "desire", "would like", "need"]),
];

/// Synonyms for `word` (case-insensitive; empty when unknown).
pub fn synonyms(word: &str) -> Vec<&'static str> {
    let w = word.trim().to_lowercase();
    THESAURUS.iter().find(|(k, _)| *k == w).map(|(_, v)| v.to_vec()).unwrap_or_default()
}

fn thesaurus(s: &mut Session, p: &Value) -> CmdResult {
    let replace = opt_str(p, "replace").map(str::to_owned);
    let given = opt_str(p, "word").map(str::to_owned);
    let c = compose_mut(s, p)?;
    if c.selection.is_collapsed() {
        c.select_word_at_caret();
    }
    let word = given.unwrap_or_else(|| c.selected_text());
    let word = word.trim().to_owned();
    if word.is_empty() {
        return Err(CmdError::Params("select a word or give `word`".into()));
    }
    let list = synonyms(&word);
    if let Some(r) = replace {
        if c.selection.is_collapsed() || c.selected_text().trim().to_lowercase() != word.to_lowercase() {
            return Err(CmdError::Failed("select the word to replace first".into()));
        }
        c.checkpoint();
        c.insert_text(&r);
        return Ok(json!({"word": word, "replacedWith": r}));
    }
    let cid = c.id;
    let result = json!({"compose": cid, "word": word, "synonyms": list});
    let _ = dialog(s, "thesaurus", result.clone())?;
    Ok(result)
}

fn translate(s: &mut Session, p: &Value) -> CmdResult {
    // Show original / translated toggle for a compose that was translated.
    if opt_bool(p, "showOriginal").is_some() {
        let c = compose_mut(s, p)?;
        let Some(orig) = c.original_body.take() else {
            return Err(CmdError::Failed("this message has not been translated".into()));
        };
        c.original_body = Some(std::mem::replace(&mut c.body, orig));
        c.ensure_body();
        return Ok(json!({"swapped": true}));
    }
    let to = opt_str(p, "to").map(str::to_owned);
    let (target, text) = target_text(s, p)?;
    let Some(to) = to else {
        let mut payload = target;
        if let Some(o) = payload.as_object_mut() {
            o.insert("text".into(), Value::String(text));
        }
        return dialog(s, "translate", payload);
    };
    if !valid_lang(&to) {
        return Err(CmdError::Params(format!("`{to}` is not a language tag")));
    }
    let Some(translated) = opt_str(p, "text").map(str::to_owned) else {
        // Hand the text to the translator backend; it calls back with {"to", "text"}.
        let mut payload = target;
        if let Some(o) = payload.as_object_mut() {
            o.insert("text".into(), Value::String(text));
            o.insert("to".into(), Value::String(to.clone()));
        }
        return dialog(s, "translate", payload);
    };
    if target.get("compose").is_some() {
        let c = compose_mut(s, p)?;
        c.checkpoint();
        if c.original_body.is_none() {
            c.original_body = Some(c.body.clone());
        }
        c.body = Doc::from_plain_text(&translated);
        c.translated_to = Some(to.clone());
        c.selection = Default::default();
        c.ensure_body();
        c.dirty = true;
        return Ok(json!({"translated": true, "to": to}));
    }
    // A received message is not changed; the UI shows the translation beside it.
    Ok(json!({"translated": true, "to": to, "item": target.get("item"), "text": translated}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "review.accessibility",
            "Check Accessibility",
            "Review › Accessibility",
            "",
            "",
            false,
            rich_compose,
            accessibility,
        ),
        spec(
            "review.language",
            "Language",
            "Review › Language",
            "",
            r#"{"lang"?:string}"#,
            false,
            has_compose,
            language,
        ),
        spec(
            "review.readAloud",
            "Read Aloud",
            "Home › Speech",
            "",
            r#"{"id"?:number,"compose"?:number}"#,
            false,
            compose_or_message,
            read_aloud,
        ),
        spec(
            "review.spelling",
            "Spelling and Grammar",
            "Review › Editor; Message › Editor",
            "F7",
            r#"{"grammar"?:bool}"#,
            false,
            has_compose,
            spelling,
        ),
        spec(
            "review.thesaurus",
            "Thesaurus",
            "Review › Editor",
            "",
            r#"{"word"?:string,"replace"?:string}"#,
            false,
            has_compose,
            thesaurus,
        ),
        spec(
            "review.translate",
            "Translate Message",
            "Message › Language › Translate",
            "",
            r#"{"to"?:string,"text"?:string,"showOriginal"?:bool,"id"?:number,"compose"?:number}"#,
            false,
            compose_or_message,
            translate,
        ),
        spec("review.wordCount", "Word Count", "Review › Editor", "", "", false, has_compose, word_count),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::compose::tests::{open, session};
    use super::*;
    use crate::Notice;
    use mailcraft_model::{FolderRole, MailItem};

    #[test]
    fn word_count_body_and_selection() {
        let mut s = session();
        open(&mut s);
        s.run("compose.insertText", &json!({"text": "Hello brave world\n\nSecond line here"})).unwrap();
        let r = s.run("review.wordCount", &json!({})).unwrap();
        assert_eq!(r["words"], 6);
        assert_eq!(r["paragraphs"], 2);
        assert_eq!(r["lines"], 2);
        assert_eq!(r["charactersNoSpaces"], 29);
        assert_eq!(r["charactersWithSpaces"], 33);
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 5}}),
        )
        .unwrap();
        let r = s.run("review.wordCount", &json!({})).unwrap();
        assert_eq!(r["words"], 1);
        assert_eq!(r["scope"], "selection");
        assert!(s.notices.iter().any(|n| matches!(n, Notice::OpenDialog(d, _) if d == "wordCount")));
    }

    #[test]
    fn spelling_stub_and_language() {
        let mut s = session();
        open(&mut s);
        s.run("compose.insertText", &json!({"text": "teh cat"})).unwrap();
        let r = s.run("review.spelling", &json!({})).unwrap();
        assert_eq!(r["count"], 0);
        s.run("review.language", &json!({"lang": "fr-CA"})).unwrap();
        assert!(s.run("review.language", &json!({"lang": "not a tag"})).is_err());
        assert_eq!(s.run("review.language", &json!({})).unwrap()["dialog"], "language");
    }

    #[test]
    fn accessibility_finds_issues() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "faint"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.color", &json!({"color": "#eeeeee"})).unwrap();
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 5}})).unwrap();
        s.run("insert.shape", &json!({"kind": "oval"})).unwrap();
        // Shapes carry alt text; an image without alt is flagged.
        s.run("insert.picture", &json!({"name": "p.gif", "data_base64": "R0lGODlhAQABAAAAACw="})).unwrap();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        let kinds: Vec<&str> =
            r["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"missingAltText"), "{kinds:?}");
        assert!(kinds.contains(&"lowContrast"));
        assert_eq!(s.composes[&id].images().len(), 2);
    }

    #[test]
    fn thesaurus_lookup_and_replace() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "a big plan"})).unwrap();
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 3}})).unwrap();
        let r = s.run("review.thesaurus", &json!({})).unwrap();
        assert_eq!(r["word"], "big");
        assert!(r["synonyms"].as_array().unwrap().iter().any(|w| w == "large"));
        s.run("review.thesaurus", &json!({"replace": "large"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "a large plan");
        assert!(synonyms("zzz").is_empty());
    }

    #[test]
    fn read_aloud_and_translate() {
        let mut s = session();
        assert!(s.run("review.readAloud", &json!({})).is_err(), "nothing to read");
        let inbox = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        let mut m = MailItem { folder: inbox, subject: "Hola".into(), ..Default::default() };
        m.body.text = Some("Hola mundo".into());
        let mid = s.mailbox.add_mail(m);
        s.selection.focus = Some(mid);
        let r = s.run("review.readAloud", &json!({})).unwrap();
        assert_eq!(r["text"], "Hola mundo");
        assert_eq!(s.run("review.translate", &json!({"to": "en"})).unwrap()["dialog"], "translate");
        let r = s.run("review.translate", &json!({"to": "en", "text": "Hello world"})).unwrap();
        assert_eq!(r["item"], mid.0);
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"compose": id, "text": "Bonjour"})).unwrap();
        s.run("review.translate", &json!({"compose": id, "to": "en", "text": "Hello"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "Hello");
        s.run("review.translate", &json!({"compose": id, "showOriginal": true})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "Bonjour");
        assert!(s.run("review.translate", &json!({"compose": id, "to": "bad tag!"})).is_err());
    }
}
