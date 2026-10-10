//! `review.*`: proofing and reading tools. Spelling and Grammar checks the compose body with
//! `mailcraft-proof` (bundled US English plus the user's dictionary); issues are cached on the compose and
//! refreshed when the body changed (edit counter), positions are body [`DocPos`]es. Word Count,
//! Accessibility, Thesaurus (small bundled list) and Language work on the compose body. Read Aloud and Translate need platform services (speech, a translator backend):
//! the engine gathers the text and hands it to the UI through a dialog notice; a translator calls back with
//! `{"to", "text"}` to apply the translation.

use serde_json::{Value, json};

use std::sync::OnceLock;

use super::compose::{
    commit_extras, compose_id, compose_mut, has_compose, pos_json, pos_param, rich_compose, spec,
};
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::{ComposeState, DocPos, SpellIssue, TextIssue};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::{Doc, ItemId};
use mailcraft_proof::{IssueKind, Speller};

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

/// Suggestions computed per spelling issue.
const SUGGESTIONS: usize = 5;
/// Issues that get suggestions in one `review.spelling` result (the rest get them when asked).
const SUGGEST_FIRST: usize = 50;

static BASE_SPELLER: OnceLock<Speller> = OnceLock::new();

/// The session's speller: bundled US English plus the custom dictionary (File › Options › Mail › Editor
/// Options › Custom Dictionaries).
pub fn speller(s: &Session) -> Speller {
    let mut sp = BASE_SPELLER.get_or_init(Speller::bundled_en_us).clone();
    sp.set_user_words(s.mailbox.custom_dictionary.iter());
    sp
}

fn kind_label(k: IssueKind) -> &'static str {
    match k {
        IssueKind::Spelling => "spelling",
        IssueKind::RepeatedWord => "repeated",
        IssueKind::Capitalization => "capitalization",
        IssueKind::DoubleSpace => "doubleSpace",
    }
}

/// Issues in one paragraph's text as char ranges (the checker reports byte ranges).
pub fn text_issues(sp: &Speller, text: &str) -> Vec<TextIssue> {
    let starts: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    let char_of = |byte: usize| -> Option<usize> {
        if byte == text.len() { Some(starts.len()) } else { starts.binary_search(&byte).ok() }
    };
    sp.check_text(text)
        .into_iter()
        .filter_map(|i| {
            Some(TextIssue {
                start: char_of(i.start)?,
                end: char_of(i.end)?,
                kind: kind_label(i.kind).to_owned(),
                suggestions: i.suggestions,
            })
        })
        .collect()
}

/// Re-checks a compose's body when it changed since the last check (or with `force`); `min_edits` debounces
/// checking while typing (re-check only after that many edits; 0 or 1 = after every edit). Returns whether
/// it re-checked.
pub fn spell_check(sp: &Speller, c: &mut ComposeState, force: bool, min_edits: u64) -> bool {
    let due = force
        || match c.spell_rev {
            None => true,
            Some(r) => c.edit_rev.wrapping_sub(r) >= min_edits.max(1),
        };
    if !due {
        return false;
    }
    c.refresh_spelling(true, &mut |t: &str| text_issues(sp, t))
}

/// Fills in suggestions for the first `n` spelling issues that have none.
fn fill_suggestions(sp: &Speller, c: &mut ComposeState, n: usize) {
    for issue in c.spell_issues.iter_mut().filter(|i| i.kind == "spelling").take(n) {
        if issue.suggestions.is_empty() {
            issue.suggestions = sp.suggest(&issue.word, SUGGESTIONS);
        }
    }
}

fn issue_json(i: &SpellIssue) -> Value {
    json!({"start": pos_json(&i.start), "end": pos_json(&i.end), "word": i.word, "kind": i.kind, "suggestions": i.suggestions})
}

/// Checks the target compose (when stale) and returns it.
fn checked<'a>(s: &'a mut Session, p: &Value) -> Result<(&'a mut ComposeState, Speller), CmdError> {
    let sp = speller(s);
    let force = opt_bool(p, "force") == Some(true);
    let c = compose_mut(s, p)?;
    spell_check(&sp, c, force, 1);
    Ok((c, sp))
}

fn spelling(s: &mut Session, p: &Value) -> CmdResult {
    let grammar = opt_bool(p, "grammar").unwrap_or(true);
    let next = opt_bool(p, "next") == Some(true);
    let (c, sp) = checked(s, p)?;
    fill_suggestions(&sp, c, SUGGEST_FIRST);
    let words = c.word_count().words;
    let shown: Vec<&SpellIssue> =
        c.spell_issues.iter().filter(|i| grammar || i.kind == "spelling" || i.kind == "repeated").collect();
    let issues: Vec<Value> = shown.iter().map(|i| issue_json(i)).collect();
    let count = issues.len();
    // F7: select the next issue after the selection.
    let current = if next {
        let from = c.selection.end().clone();
        let pick = shown.iter().find(|i| i.start >= from).or_else(|| shown.first()).map(|i| (*i).clone());
        if let Some(i) = &pick {
            if i.suggestions.is_empty() && i.kind == "spelling" {
                // Suggestions beyond the first batch.
                let sugg = sp.suggest(&i.word, SUGGESTIONS);
                if let Some(cached) = c.spell_issues.iter_mut().find(|x| x.start == i.start) {
                    cached.suggestions = sugg;
                }
            }
            c.selection = crate::compose::BodySelection { anchor: i.start.clone(), focus: i.end.clone() };
            c.pending_style = None;
        }
        pick.and_then(|i| c.spell_issues.iter().find(|x| x.start == i.start).map(issue_json))
    } else {
        None
    };
    Ok(json!({
        "compose": c.id,
        "issues": issues,
        "count": count,
        "current": current,
        "grammar": grammar,
        "wordsChecked": words,
        "language": c.language.clone().unwrap_or_else(|| "en-US".into()),
    }))
}

/// `{"at": {path, offset}}` when given.
fn at_param(p: &Value) -> Result<Option<DocPos>, CmdError> {
    match p.get("at") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => pos_param(v).map(Some),
    }
}

fn ignore(s: &mut Session, p: &Value) -> CmdResult {
    let all = opt_bool(p, "all") == Some(true);
    let word = opt_str(p, "word").map(str::to_owned);
    let at = at_param(p)?;
    let (c, _) = checked(s, p)?;
    if !all && word.is_none() && c.spell_issue_at(at.as_ref().unwrap_or(c.selection.start())).is_none() {
        return Err(CmdError::Failed("there is no spelling issue there".into()));
    }
    if all && word.is_none() && c.spell_issue_at(at.as_ref().unwrap_or(c.selection.start())).is_none() {
        return Err(CmdError::Params("`word` required (no spelling issue at the cursor)".into()));
    }
    let dropped = c.ignore_spelling(word.as_deref(), all, at.as_ref());
    Ok(json!({"ignored": dropped, "all": all, "remaining": c.spell_issues.len()}))
}

fn add_to_dictionary(s: &mut Session, p: &Value) -> CmdResult {
    let at = at_param(p)?;
    let word = match opt_str(p, "word") {
        Some(w) => w.trim().to_owned(),
        None => {
            let (c, _) = checked(s, p)?;
            let pos = at.unwrap_or_else(|| c.selection.start().clone());
            c.spell_issue_at(&pos).filter(|i| i.kind == "spelling").map(|i| i.word.clone()).ok_or_else(
                || CmdError::Params("`word` required (no misspelled word at the cursor)".into()),
            )?
        }
    };
    let before = s.mailbox.clone();
    let added = s.mailbox.add_to_dictionary(&word);
    if !added && !s.mailbox.in_dictionary(&word) {
        return Err(CmdError::Params(format!("`{word}` cannot be added to the dictionary")));
    }
    commit_extras(s, before, "Add to Dictionary");
    let mut dropped = 0usize;
    for c in s.composes.values_mut() {
        dropped = dropped.saturating_add(c.forget_spelling_word(&word));
    }
    Ok(json!({"word": word, "added": added, "dropped": dropped}))
}

fn correct(s: &mut Session, p: &Value) -> CmdResult {
    let replacement = opt_str(p, "replacement").map(str::to_owned);
    let all = opt_bool(p, "all") == Some(true);
    let at = at_param(p)?;
    let (c, sp) = checked(s, p)?;
    let pos = at.unwrap_or_else(|| c.selection.start().clone());
    let Some(issue) = c.spell_issue_at(&pos).cloned() else {
        return Err(CmdError::Failed("there is no spelling issue there".into()));
    };
    let replacement = match replacement {
        Some(r) => r,
        None => {
            let sugg = if issue.suggestions.is_empty() && issue.kind == "spelling" {
                sp.suggest(&issue.word, 1)
            } else {
                issue.suggestions.clone()
            };
            sugg.into_iter().next().ok_or_else(|| CmdError::Failed("there are no suggestions".into()))?
        }
    };
    let n = c.correct_spelling(&replacement, Some(&issue.start), all);
    Ok(json!({"replaced": n, "with": replacement, "caret": pos_json(&c.selection.focus)}))
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
            r#"{"grammar"?:bool,"next"?:bool,"force"?:bool}"#,
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
        spec(
            "review.ignore",
            "Ignore",
            "Review › Editor › Spelling and Grammar; Context › Spelling",
            "",
            r#"{"word"?:string,"all"?:bool,"at"?:{"path":[number],"offset":number}}"#,
            false,
            has_compose,
            ignore,
        ),
        spec(
            "review.addToDictionary",
            "Add to Dictionary",
            "Review › Editor › Spelling and Grammar; Context › Spelling",
            "",
            r#"{"word"?:string,"at"?:{"path":[number],"offset":number}}"#,
            true,
            crate::always,
            add_to_dictionary,
        ),
        spec(
            "review.correct",
            "Change",
            "Review › Editor › Spelling and Grammar; Context › Spelling",
            "",
            r#"{"replacement"?:string,"all"?:bool,"at"?:{"path":[number],"offset":number}}"#,
            false,
            has_compose,
            correct,
        ),
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
    fn spelling_and_language() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "The cat sat on teh mat."})).unwrap();
        let r = s.run("review.spelling", &json!({})).unwrap();
        assert_eq!(r["count"], 1, "{r}");
        let issue = &r["issues"][0];
        assert_eq!(issue["word"], "teh");
        assert_eq!(issue["start"], json!({"path": [0], "offset": 15}));
        assert_eq!(issue["end"], json!({"path": [0], "offset": 18}));
        assert!(issue["suggestions"].as_array().unwrap().iter().any(|v| v == "the"), "{issue}");
        assert_eq!(r["language"], "en-US");
        s.run("review.language", &json!({"lang": "fr-CA"})).unwrap();
        assert!(s.run("review.language", &json!({"lang": "not a tag"})).is_err());
        assert_eq!(s.run("review.language", &json!({})).unwrap()["dialog"], "language");
        // F7 selects the next issue; Change applies the first suggestion.
        let r = s.run("review.spelling", &json!({"next": true})).unwrap();
        assert_eq!(r["current"]["word"], "teh");
        assert_eq!(s.composes[&id].selected_text(), "teh");
        let r = s.run("review.correct", &json!({})).unwrap();
        assert_eq!(r["with"], "the");
        assert_eq!(s.composes[&id].body.to_plain_text(), "The cat sat on the mat.");
        assert_eq!(s.run("review.spelling", &json!({})).unwrap()["count"], 0);
        assert!(s.run("review.correct", &json!({})).is_err(), "nothing to change");
    }

    #[test]
    fn spelling_grammar_ignore_and_dictionary() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "this is is Zorbleflax.\nZorbleflax again"})).unwrap();
        let r = s.run("review.spelling", &json!({})).unwrap();
        let kinds: Vec<&str> =
            r["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"capitalization") && kinds.contains(&"repeated"), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| **k == "spelling").count(), 2);
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        assert!(r["issues"].as_array().unwrap().iter().all(|i| i["kind"] != "capitalization"));
        // Ignore once at a position, then the other occurrence is still flagged.
        s.run("review.ignore", &json!({"at": {"path": [0], "offset": 12}})).unwrap();
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        assert_eq!(r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "spelling").count(), 1);
        assert!(s.run("review.ignore", &json!({"at": {"path": [0], "offset": 6}})).is_err());
        // Add to Dictionary: persisted in the mailbox, journaled, gone from every compose.
        let r = s.run("review.addToDictionary", &json!({"at": {"path": [1], "offset": 2}})).unwrap();
        assert_eq!(r["word"], "Zorbleflax");
        assert!(s.mailbox.in_dictionary("zorbleflax"));
        assert!(s.pending_ops.iter().any(|o| matches!(o, mailcraft_store::Op::Replace(_))));
        s.run("compose.insertText", &json!({"text": " Zorbleflax"})).unwrap();
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        assert_eq!(
            r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "spelling").count(),
            0,
            "{r}"
        );
        assert!(s.run("review.addToDictionary", &json!({"word": "two words"})).is_err());
        // Ignore All for this message.
        s.run("compose.insertText", &json!({"text": " Qwzzrt Qwzzrt."})).unwrap();
        s.run("review.ignore", &json!({"word": "qwzzrt", "all": true})).unwrap();
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        assert!(r["issues"].as_array().unwrap().iter().all(|i| i["word"] != "Qwzzrt"), "{r}");
        assert!(s.composes[&id].ignored_words.contains(&"qwzzrt".to_owned()));
        // Undo of the dictionary addition.
        s.run("app.undo", &json!({})).unwrap();
        assert!(!s.mailbox.in_dictionary("zorbleflax"));
    }

    #[test]
    fn spelling_cache_follows_edits_and_debounce() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Hello wrold"})).unwrap();
        let sp = speller(&s);
        let c = s.composes.get_mut(&id).unwrap();
        assert!(spell_check(&sp, c, false, 3));
        assert_eq!(c.spell_issues.len(), 1);
        c.insert_text(" mroe");
        assert!(!spell_check(&sp, c, false, 3), "debounced: one edit < 3");
        c.insert_text(" x");
        c.insert_text(" y");
        assert!(spell_check(&sp, c, false, 3));
        assert_eq!(c.spell_issues.len(), 2);
        assert!(spell_check(&sp, c, true, 3), "forced");
        assert!(!spell_check(&sp, c, false, 1), "nothing changed");
        // Byte → char mapping with multi-byte text before the word.
        let issues = text_issues(&sp, "Ünïcødé wrold \u{FFFC} ok");
        assert!(issues.iter().any(|i| (i.start, i.end) == (8, 13)), "{issues:?}");
    }

    #[test]
    fn spelling_in_a_reply_skips_the_original() {
        let mut s = session();
        open(&mut s);
        let c = s.composes.values_mut().next_back().unwrap();
        c.kind = crate::compose::ComposeKind::Reply;
        c.body = mailcraft_model::Doc::from_plain_text(
            "Thanks, wrold\n\n-----Original Message-----\nFrom: x\nmispeled text",
        );
        c.ensure_body();
        c.mark_edited();
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        let words: Vec<&str> =
            r["issues"].as_array().unwrap().iter().map(|i| i["word"].as_str().unwrap()).collect();
        assert_eq!(words, ["wrold"]);
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
