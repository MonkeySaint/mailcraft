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
use crate::settings::Settings;
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_model::{Doc, ItemId};
use mailcraft_proof::{IssueKind, SpellOptions, Speller};

mod accessibility;

pub use accessibility::{A11yIssue, LARGE_AUDIENCE, Severity, auto_alt_text, check as accessibility_issues};

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
/// Options › Custom Dictionaries), checking with the Editor Options proofing settings ([`spell_options`]).
pub fn speller(s: &Session) -> Speller {
    let mut sp = BASE_SPELLER.get_or_init(Speller::bundled_en_us).clone();
    sp.set_user_words(s.mailbox.custom_dictionary.iter());
    sp.set_options(spell_options(&s.settings));
    sp
}

/// Options › Mail › Editor Options › Proofing as checker options: "Ignore words in UPPERCASE", "Ignore words
/// that contain numbers", "Ignore Internet and file addresses", "Flag repeated words" and "Suggest from main
/// dictionary only". The grammar checks always run: the Spelling and Grammar dialog has its own switch, and
/// "Mark grammar errors as you type" only decides what the editor underlines ([`live_proofing`]).
pub fn spell_options(st: &Settings) -> SpellOptions {
    SpellOptions {
        ignore_uppercase: st.bool("mail.editor.ignoreUppercase"),
        ignore_words_with_numbers: st.bool("mail.editor.ignoreNumbers"),
        ignore_internet_and_file_addresses: st.bool("mail.editor.ignoreInternet"),
        flag_repeated_words: st.bool("mail.editor.flagRepeated"),
        check_grammar: true,
        main_dictionary_only: st.bool("mail.editor.mainDictionaryOnly"),
    }
}

/// What the body editor marks while you type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveProofing {
    /// Squiggles under misspelled and repeated words ("Check spelling as you type"; "Flag repeated words" is
    /// a spelling option, and the Spelling dialog and the check before sending count them as spelling too).
    pub spelling: bool,
    /// Blue underlines under grammar issues: sentence capitals, extra spaces ("Mark grammar errors as you
    /// type").
    pub grammar: bool,
}

impl LiveProofing {
    /// Whether the editor checks the body as you type at all.
    pub fn any(self) -> bool {
        self.spelling || self.grammar
    }

    /// Whether an issue of `kind` (a [`SpellIssue::kind`]) is underlined.
    pub fn marks(self, kind: &str) -> bool {
        if matches!(kind, "spelling" | "repeated") { self.spelling } else { self.grammar }
    }
}

/// Options › Mail › Editor Options › "Check spelling as you type" / "Mark grammar errors as you type". F7 and
/// Review › Spelling and Grammar check either way.
pub fn live_proofing(s: &Session) -> LiveProofing {
    LiveProofing {
        spelling: s.settings.bool("mail.editor.spellAsYouType"),
        grammar: s.settings.bool("mail.editor.grammarAsYouType"),
    }
}

/// Fingerprint of checking options (cached issues found under other options are stale). Never 0, the key of
/// a compose that was never checked. Dictionary changes need no key: Add to Dictionary drops the word from
/// every compose's cached issues itself.
fn options_key(o: &SpellOptions) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    o.hash(&mut h);
    h.finish() | 1
}

/// Whether compose `c`'s cached spelling issues are out of date: the body changed since the last check, or
/// the Editor Options proofing settings did (cheap: no speller is built).
pub fn spelling_stale(s: &Session, c: &ComposeState) -> bool {
    c.spelling_stale() || c.spell_options_key != options_key(&spell_options(&s.settings))
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
/// A check under other proofing options (Options changed since) is always redone.
pub fn spell_check(sp: &Speller, c: &mut ComposeState, force: bool, min_edits: u64) -> bool {
    if proofing_off(c).is_some() {
        // Not checked: no squiggles, nothing for F7.
        let had = !c.spell_issues.is_empty();
        c.spell_issues.clear();
        c.spell_rev = Some(c.edit_rev);
        return had;
    }
    let key = options_key(sp.options());
    let due = force
        || c.spell_options_key != key
        || match c.spell_rev {
            None => true,
            Some(r) => c.edit_rev.wrapping_sub(r) >= min_edits.max(1),
        };
    if !due {
        return false;
    }
    c.spell_options_key = key;
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
    let drawings = drawing_spelling(&sp, c, grammar);
    Ok(json!({
        "compose": c.id,
        "issues": issues,
        "count": count,
        "drawings": drawings,
        "current": current,
        "grammar": grammar,
        "wordsChecked": words,
        "language": c.language.clone().unwrap_or_else(|| "en-US".into()),
        "notChecked": proofing_off(c),
    }))
}

/// Most issues reported in the text of drawings.
const MAX_DRAWING_ISSUES: usize = 200;

/// Spelling (and with `grammar`, grammar) issues in the text of the compose's text boxes, shapes and WordArt:
/// `[{drawing, word, kind, suggestions}]`, in drawing order.
fn drawing_spelling(sp: &Speller, c: &ComposeState, grammar: bool) -> Vec<Value> {
    if proofing_off(c).is_some() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in c.body.drawings.iter().filter(|d| !d.text.trim().is_empty()) {
        let chars: Vec<char> = d.text.chars().collect();
        for i in text_issues(sp, &d.text) {
            if out.len() >= MAX_DRAWING_ISSUES {
                return out;
            }
            if !grammar && i.kind != "spelling" && i.kind != "repeated" {
                continue;
            }
            let word: String =
                chars.get(i.start..i.end.min(chars.len())).unwrap_or_default().iter().collect();
            let suggestions = if i.suggestions.is_empty() && i.kind == "spelling" && out.len() < SUGGEST_FIRST
            {
                sp.suggest(&word, SUGGESTIONS)
            } else {
                i.suggestions
            };
            out.push(json!({"drawing": d.id, "word": word, "kind": i.kind, "suggestions": suggestions}));
        }
    }
    out
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
    // The text of text boxes, shapes and WordArt, counted on its own (the dialog adds it when asked).
    let mut dw = crate::compose::WordCount::default();
    if c.selection.is_collapsed() {
        for d in c.body.drawings.iter().filter(|d| !d.text.trim().is_empty()) {
            let lines: Vec<&str> = d.text.split('\n').filter(|l| !l.trim().is_empty()).collect();
            dw.paragraphs = dw.paragraphs.saturating_add(lines.len());
            dw.lines = dw.lines.saturating_add(lines.len());
            dw.words = dw.words.saturating_add(d.text.split_whitespace().count());
            dw.characters_no_spaces =
                dw.characters_no_spaces.saturating_add(d.text.chars().filter(|c| !c.is_whitespace()).count());
            dw.characters_with_spaces =
                dw.characters_with_spaces.saturating_add(d.text.chars().filter(|c| *c != '\n').count());
        }
    }
    let counts = json!({
        "compose": cid,
        "scope": scope,
        "words": wc.words,
        "charactersNoSpaces": wc.characters_no_spaces,
        "charactersWithSpaces": wc.characters_with_spaces,
        "paragraphs": wc.paragraphs,
        "lines": wc.lines,
        "drawings": {
            "words": dw.words,
            "charactersNoSpaces": dw.characters_no_spaces,
            "charactersWithSpaces": dw.characters_with_spaces,
            "paragraphs": dw.paragraphs,
            "lines": dw.lines,
        },
    });
    let _ = dialog(s, "wordCount", counts.clone())?;
    Ok(counts)
}

fn accessibility(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    let issues: Vec<Value> = accessibility::check(c).iter().map(A11yIssue::to_json).collect();
    let report = json!({"compose": cid, "issues": issues});
    let _ = dialog(s, "accessibility", report.clone())?;
    Ok(report)
}

/// The accessibility warning of a compose (Options › Accessibility › Accessibility checker options): whether
/// it shows now, why, and how many issues the checker finds.
fn accessibility_status(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(c) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    Ok(accessibility::status(s, c))
}

/// `review.accessibilityStatus` for the UI (no command run): `{"show", "checked", "reasons", "errors",
/// "warnings", "tips"}`.
pub fn accessibility_warning(s: &Session, cid: u64) -> Option<Value> {
    s.composes.get(&cid).map(|c| accessibility::status(s, c))
}

pub(crate) fn valid_lang(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 35
        && tag.split('-').all(|part| {
            !part.is_empty() && part.len() <= 8 && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// Languages the bundled dictionary checks (US English word lists; other English variants use them too).
pub fn has_dictionary(lang: &str) -> bool {
    let l = lang.trim().to_ascii_lowercase();
    l == "en" || l.starts_with("en-")
}

/// Why the message's spelling is not checked: "Do not check spelling or grammar", or a proofing language
/// without a bundled dictionary.
pub fn proofing_off(c: &ComposeState) -> Option<String> {
    if c.no_proof {
        return Some("Spelling and grammar are not checked in this message.".into());
    }
    match c.language.as_deref() {
        Some(l) if !has_dictionary(l) => {
            Some(format!("There is no dictionary for {l}; spelling is not checked."))
        }
        _ => None,
    }
}

/// Set Proofing Language: the message's language (sent as Content-Language, RFC 3282) and whether spelling
/// is checked. Languages without a bundled dictionary are not spell-checked.
fn language(s: &mut Session, p: &Value) -> CmdResult {
    let no_proof = opt_bool(p, "noProof");
    let Some(lang) = opt_str(p, "lang").map(str::to_owned) else {
        if let Some(off) = no_proof {
            let c = compose_mut(s, p)?;
            c.no_proof = off;
            c.spell_rev = None;
            return Ok(json!({"language": c.language, "noProof": off}));
        }
        let cid = compose_id(s, p)?;
        let (current, off) =
            s.composes.get(&cid).map(|c| (c.language.clone(), c.no_proof)).unwrap_or_default();
        return dialog(
            s,
            "language",
            json!({"compose": cid, "lang": current.unwrap_or_else(|| "en-US".into()), "noProof": off}),
        );
    };
    if !valid_lang(&lang) {
        return Err(CmdError::Params(format!("`{lang}` is not a language tag (e.g. en-US)")));
    }
    let c = compose_mut(s, p)?;
    c.language = Some(lang.clone());
    if let Some(off) = no_proof {
        c.no_proof = off;
    }
    // Check again under the new language.
    c.spell_rev = None;
    c.dirty = true;
    Ok(
        json!({"language": lang, "noProof": c.no_proof, "dictionary": has_dictionary(&lang), "checked": proofing_off(c).is_none()}),
    )
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

/// MailCraft's own everyday replacements, offered before the bundled thesaurus's.
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

/// Most synonyms offered.
const MAX_SYNONYMS: usize = 20;

/// Synonyms for `word` (case-insensitive; empty when unknown): MailCraft's short list of everyday
/// replacements first, then the bundled thesaurus (base forms too: "meetings" finds "meeting").
pub fn synonyms(word: &str) -> Vec<&'static str> {
    let w = word.trim().to_lowercase();
    let mut out: Vec<&'static str> =
        THESAURUS.iter().find(|(k, _)| *k == w).map(|(_, v)| v.to_vec()).unwrap_or_default();
    if let Some((_, more)) = mailcraft_proof::thesaurus::lookup(&w) {
        for m in more {
            if !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out.truncate(MAX_SYNONYMS);
    out
}

/// The head word the thesaurus used for `word` ("meeting" for "meetings"), when it differs.
pub fn thesaurus_headword(word: &str) -> Option<&'static str> {
    let (head, _) = mailcraft_proof::thesaurus::lookup(word)?;
    (!head.eq_ignore_ascii_case(word.trim())).then_some(head)
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
    let result =
        json!({"compose": cid, "word": word, "synonyms": list, "lookedUp": thesaurus_headword(&word)});
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
            "review.accessibilityStatus",
            "Accessibility Warnings",
            "Compose › status (File › Options › Accessibility)",
            "",
            r#"{"compose"?:number}"#,
            false,
            has_compose,
            accessibility_status,
        ),
        spec(
            "review.language",
            "Language",
            "Review › Language",
            "",
            r#"{"lang"?:string,"noProof"?:bool}"#,
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
        // A language without a dictionary is not checked; Do not check spelling turns it off too.
        let r = s.run("review.language", &json!({"lang": "fr-CA"})).unwrap();
        assert_eq!((r["dictionary"].as_bool(), r["checked"].as_bool()), (Some(false), Some(false)));
        let r = s.run("review.spelling", &json!({})).unwrap();
        assert_eq!(r["count"], 0);
        assert!(r["notChecked"].as_str().unwrap().contains("fr-CA"));
        s.run("review.language", &json!({"lang": "en-GB"})).unwrap();
        assert_eq!(
            s.run("review.spelling", &json!({})).unwrap()["count"],
            1,
            "English variants use the dictionary"
        );
        s.run("review.language", &json!({"noProof": true})).unwrap();
        assert_eq!(s.run("review.spelling", &json!({})).unwrap()["count"], 0);
        s.run("review.language", &json!({"lang": "en-US", "noProof": false})).unwrap();
        let b = crate::cmd::compose::build_message(&s, &s.composes[&id], None, false);
        assert!(b.extra_headers.contains(&("Content-Language".to_owned(), "en-US".to_owned())));
        assert!(s.run("review.language", &json!({"lang": "not a tag"})).is_err());
        assert_eq!(s.run("review.language", &json!({})).unwrap()["dialog"], "language");
        assert_eq!(crate::cmd::insert::tests::last_dialog(&s)["lang"], "en-US");
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
        // Shapes carry alt text (their gallery name); an image without alt is flagged.
        s.run("insert.picture", &json!({"name": "p.gif", "data_base64": "R0lGODlhAQABAAAAACw="})).unwrap();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        let kinds: Vec<&str> =
            r["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"missingAltText"), "{kinds:?}");
        assert!(kinds.contains(&"lowContrast"));
        assert_eq!(s.composes[&id].images().len(), 1, "the shape is a drawing object, not a picture");
        assert_eq!(s.composes[&id].body.drawings.len(), 1);
    }

    /// Regression: shapes stopped being pictures and the checks skipped them: no alt text warning, and the
    /// text of text boxes was neither spell checked nor counted.
    #[test]
    fn drawings_are_checked_and_counted() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Hello there"})).unwrap();
        s.run("insert.shape", &json!({"kind": "oval", "x": 0, "y": 0})).unwrap();
        s.run("insert.textBox", &json!({"text": "Thre wrold words", "x": 200, "y": 0})).unwrap();
        let ids: Vec<u32> = s.composes[&cid].body.drawings.iter().map(|d| d.id).collect();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        let about = |kind: &str| -> Vec<u64> {
            r["issues"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["kind"] == kind)
                .filter_map(|i| i["drawing"].as_u64())
                .collect()
        };
        assert_eq!(about("altTextIsShapeName"), [u64::from(ids[0])], "{r}");
        s.run("drawing.altText", &json!({"id": ids[0], "alt": ""})).unwrap();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        assert!(r["issues"].as_array().unwrap().iter().any(|i| i["kind"] == "missingAltText"
            && i["drawing"] == json!(ids[0])
            && i["severity"] == "error"));
        // Text that is too faint on the box.
        s.run("drawing.textColor", &json!({"id": ids[1], "color": "#f4f4f4"})).unwrap();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        assert!(
            r["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "lowContrast" && i["drawing"] == json!(ids[1])),
            "{r}"
        );
        // Spelling of the box's text.
        let r = s.run("review.spelling", &json!({"grammar": false})).unwrap();
        let words: Vec<&str> =
            r["drawings"].as_array().unwrap().iter().map(|i| i["word"].as_str().unwrap()).collect();
        assert_eq!(words, ["Thre", "wrold"], "{r}");
        assert_eq!(r["drawings"][0]["drawing"], json!(ids[1]));
        // Word Count counts the box's text on its own.
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 0}})).unwrap();
        let r = s.run("review.wordCount", &json!({})).unwrap();
        assert_eq!((r["words"].as_u64(), r["drawings"]["words"].as_u64()), (Some(2), Some(3)), "{r}");
    }

    fn set(s: &mut Session, key: &str, v: Value) {
        s.run("options.set", &json!({"key": key, "value": v})).unwrap();
    }

    fn words(r: &Value, kind: &str) -> Vec<String> {
        r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == kind)
            .map(|i| i["word"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn editor_proofing_options_change_what_is_flagged() {
        let mut s = session();
        open(&mut s);
        s.run(
            "compose.insertText",
            &json!({"text": "Send QZXV the the b4x to see www.qzexample.com/zzqq now"}),
        )
        .unwrap();
        let r = s.run("review.spelling", &json!({})).unwrap();
        assert!(words(&r, "spelling").is_empty(), "{r}");
        assert_eq!(words(&r, "repeated").len(), 1);
        // Each option, turned off, flags what it skipped; the cached check is redone without an edit.
        set(&mut s, "mail.editor.ignoreUppercase", json!(false));
        assert_eq!(words(&s.run("review.spelling", &json!({})).unwrap(), "spelling"), ["QZXV"]);
        set(&mut s, "mail.editor.ignoreNumbers", json!(false));
        assert_eq!(words(&s.run("review.spelling", &json!({})).unwrap(), "spelling"), ["QZXV", "b4x"]);
        set(&mut s, "mail.editor.ignoreInternet", json!(false));
        let r = s.run("review.spelling", &json!({})).unwrap();
        assert!(words(&r, "spelling").len() > 2, "{r}");
        set(&mut s, "mail.editor.flagRepeated", json!(false));
        assert!(words(&s.run("review.spelling", &json!({})).unwrap(), "repeated").is_empty());
        // Suggest from main dictionary only: dictionary words still pass but are not suggested.
        s.run("review.addToDictionary", &json!({"word": "zorbleflax"})).unwrap();
        assert!(speller(&s).suggest("zorbleflx", 5).contains(&"zorbleflax".to_owned()));
        set(&mut s, "mail.editor.mainDictionaryOnly", json!(true));
        assert!(!speller(&s).suggest("zorbleflx", 5).contains(&"zorbleflax".to_owned()));
        assert!(speller(&s).check("zorbleflax"));
    }

    #[test]
    fn live_proofing_follows_the_options() {
        let mut s = session();
        let id = open(&mut s);
        assert_eq!(live_proofing(&s), LiveProofing { spelling: true, grammar: true });
        set(&mut s, "mail.editor.spellAsYouType", json!(false));
        let live = live_proofing(&s);
        // Repeated words are spelling, as in the Spelling dialog and the check before sending.
        assert!(
            live.any() && !live.marks("spelling") && !live.marks("repeated") && live.marks("capitalization")
        );
        set(&mut s, "mail.editor.grammarAsYouType", json!(false));
        assert!(!live_proofing(&s).any());
        set(&mut s, "mail.editor.spellAsYouType", json!(true));
        let live = live_proofing(&s);
        assert!(live.marks("spelling") && live.marks("repeated") && !live.marks("capitalization"));
        // Staleness: an edit, or an option change since the last check.
        s.run("compose.insertText", &json!({"text": "Hi wrold"})).unwrap();
        assert!(spelling_stale(&s, &s.composes[&id]));
        let sp = speller(&s);
        spell_check(&sp, s.composes.get_mut(&id).unwrap(), false, 1);
        assert!(!spelling_stale(&s, &s.composes[&id]));
        set(&mut s, "mail.editor.ignoreUppercase", json!(false));
        assert!(spelling_stale(&s, &s.composes[&id]));
    }

    fn table(rows: Vec<Vec<(&str, bool)>>) -> mailcraft_model::Block {
        use mailcraft_model::rich::TableCell;
        mailcraft_model::Block::Table {
            borders: None,
            rows: rows
                .into_iter()
                .map(|r| {
                    r.into_iter()
                        .map(|(t, bold)| TableCell {
                            blocks: vec![mailcraft_model::Block::Paragraph {
                                style: Default::default(),
                                inlines: vec![mailcraft_model::Inline::Text {
                                    text: t.into(),
                                    style: mailcraft_model::TextStyle {
                                        bold: bold.then_some(true),
                                        ..Default::default()
                                    },
                                }],
                            }],
                            colspan: 1,
                            rowspan: 1,
                            background: None,
                            width_px: None,
                        })
                        .collect()
                })
                .collect(),
            border: true,
            width_px: None,
        }
    }

    fn kinds(r: &Value) -> Vec<String> {
        r["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn accessibility_checks_tables_links_headings_and_blanks() {
        use mailcraft_model::{Block, Inline};
        let mut s = session();
        let id = open(&mut s);
        let c = s.composes.get_mut(&id).unwrap();
        let link = |text: &str, href: &str| Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Link {
                title: None,
                href: href.into(),
                children: vec![Inline::Text { text: text.into(), style: Default::default() }],
            }],
        };
        let mut merged =
            table(vec![vec![("Name", true), ("Phone", true)], vec![("Ann", false), ("", false)]]);
        if let Block::Table { rows, .. } = &mut merged {
            rows[1][0].colspan = 2;
        }
        c.body.blocks = vec![
            table(vec![vec![("Name", false), ("Phone", false)], vec![("Ann", false), ("555", false)]]),
            table(vec![vec![("Name", true), ("Phone", true)], vec![("Ann", false), ("555", false)]]),
            merged,
            table(vec![vec![("Item", true), ("Cost", true)], vec![("", false), ("", false)]]),
            link("Click here", "https://example.com/a"),
            link("the release notes", "https://example.com/b"),
            link(
                "https://example.com/a/very/long/address/that/is/bare",
                "https://example.com/a/very/long/address/that/is/bare",
            ),
            link("@Ann", "mailto:ann@example.org"),
            Block::Heading {
                level: 1,
                style: Default::default(),
                inlines: vec![Inline::Text { text: "Top".into(), style: Default::default() }],
            },
            Block::Heading {
                level: 3,
                style: Default::default(),
                inlines: vec![Inline::Text { text: "Deep".into(), style: Default::default() }],
            },
            mailcraft_model::Block::text_paragraph(""),
            mailcraft_model::Block::text_paragraph(""),
            mailcraft_model::Block::text_paragraph("Lined   up"),
            Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Image {
                    src: "cid:x".into(),
                    alt: "IMG_0001.JPG".into(),
                    width: None,
                    height: None,
                }],
            },
        ];
        c.ensure_body();
        let r = s.run("review.accessibility", &json!({})).unwrap();
        let k = kinds(&r);
        let count = |kind: &str| k.iter().filter(|x| *x == kind).count();
        assert_eq!(count("tableHeader"), 1, "{k:?}");
        assert_eq!(count("mergedCells"), 1);
        assert_eq!(count("blankTableRows"), 1);
        assert_eq!(count("unclearLinkText"), 2, "click here and the bare address; not the mention");
        assert_eq!(count("headingOrder"), 1);
        assert_eq!(count("emptyParagraphs"), 1);
        assert_eq!(count("repeatedSpaces"), 1);
        assert_eq!(count("altTextIsFileName"), 1);
        // Most serious first; issues point at their place.
        let sev: Vec<&str> =
            r["issues"].as_array().unwrap().iter().map(|i| i["severity"].as_str().unwrap()).collect();
        let mut sorted = sev.clone();
        sorted.sort_by_key(|x| match *x {
            "error" => 0,
            "warning" => 1,
            _ => 2,
        });
        assert_eq!(sev, sorted);
        let header = r["issues"].as_array().unwrap().iter().find(|i| i["kind"] == "tableHeader").unwrap();
        assert_eq!(header["at"]["path"], json!([0]));
    }

    fn picture(s: &mut Session) {
        s.run("insert.picture", &json!({"name": "p.gif", "data_base64": "R0lGODlhAQABAAAAACw="})).unwrap();
    }

    #[test]
    fn accessibility_warnings_follow_the_checker_options() {
        let mut s = session();
        let id = open(&mut s);
        let status = |s: &mut Session| s.run("review.accessibilityStatus", &json!({})).unwrap();
        // Nothing to warn about.
        set(&mut s, "accessibility.warnings", json!("whileWorking"));
        assert_eq!(status(&mut s)["show"], false);
        picture(&mut s);
        let st = status(&mut s);
        assert_eq!((st["show"].clone(), st["errors"].clone()), (json!(true), json!(1)), "{st}");
        assert_eq!(st["checked"], true);
        // Only when I open the checker: the checker does not even run.
        set(&mut s, "accessibility.warnings", json!("checkerOnly"));
        let st = status(&mut s);
        assert_eq!(
            (st["show"].clone(), st["checked"].clone(), st["errors"].clone()),
            (json!(false), json!(false), json!(0))
        );
        // "When": only while a ticked condition holds (and only then is the body checked).
        set(&mut s, "accessibility.warnings", json!("when"));
        let st = status(&mut s);
        assert_eq!(
            (st["show"].clone(), st["checked"].clone()),
            (json!(false), json!(false)),
            "no condition holds"
        );
        s.composes.get_mut(&id).unwrap().importance = mailcraft_model::Importance::High;
        let st = status(&mut s);
        assert_eq!((st["show"].clone(), st["checked"].clone()), (json!(true), json!(true)));
        assert_eq!(st["reasons"], json!(["highImportance"]));
        set(&mut s, "accessibility.warnHighImportance", json!(false));
        assert_eq!(status(&mut s)["show"], false);
        // Outside my organization (the account is at example.com).
        s.run("compose.setTo", &json!({"value": "pat@mail.example.com"})).unwrap();
        assert_eq!(status(&mut s)["show"], false, "a subdomain is inside");
        s.run("compose.setTo", &json!({"value": "pat@mail.example.com; lee@example.org"})).unwrap();
        assert_eq!(status(&mut s)["reasons"], json!(["outsideOrganization"]));
        set(&mut s, "accessibility.warnOutsideOrganization", json!(false));
        assert_eq!(status(&mut s)["show"], false);
        // A large audience.
        let many: Vec<String> = (0..LARGE_AUDIENCE).map(|i| format!("p{i}@example.com")).collect();
        s.run("compose.setTo", &json!({"value": many.join("; ")})).unwrap();
        assert_eq!(status(&mut s)["reasons"], json!(["largeAudience"]));
        set(&mut s, "accessibility.warnLargeAudience", json!(false));
        assert_eq!(status(&mut s)["show"], false);
        // A dark background (dark mode, or a dark page colour).
        s.view.dark = true;
        assert_eq!(status(&mut s)["reasons"], json!(["darkBackground"]));
        s.composes.get_mut(&id).unwrap().light_canvas = true;
        assert_eq!(status(&mut s)["show"], false, "Switch Background shows it light");
        s.composes.get_mut(&id).unwrap().body.background = Some(mailcraft_geom::Color::rgb(20, 20, 30));
        assert_eq!(status(&mut s)["reasons"], json!(["darkBackground"]));
        set(&mut s, "accessibility.warnDarkBackground", json!(false));
        assert_eq!(status(&mut s)["show"], false);
        assert_eq!(status(&mut s)["mode"], "when");
    }

    #[test]
    fn automatic_alt_text_names_inserted_pictures() {
        let mut s = session();
        let id = open(&mut s);
        let gif = "R0lGODlhAQABAAAAACw=";
        // On by default, as observed.
        let r = s.run("insert.picture", &json!({"name": "team-lunch.gif", "data_base64": gif})).unwrap();
        assert_eq!(r["alt"], "Team lunch");
        let r = s.run("insert.picture", &json!({"name": "IMG_2041.gif", "data_base64": gif})).unwrap();
        assert_eq!(r["alt"], "", "no description from a camera name");
        let r =
            s.run("insert.picture", &json!({"name": "x.gif", "alt": "My own", "data_base64": gif})).unwrap();
        assert_eq!(r["alt"], "My own");
        // Turned off: the picture has no alternative text until I write one.
        set(&mut s, "accessibility.autoAltText", json!(false));
        let r = s.run("insert.picture", &json!({"name": "team-lunch.gif", "data_base64": gif})).unwrap();
        assert_eq!(r["alt"], "");
        let alts: Vec<String> = s.composes[&id].images().into_iter().map(|(_, a)| a).collect();
        assert_eq!(alts, ["Team lunch", "", "My own", ""]);
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
        // The bundled thesaurus covers words the short list does not, and base forms.
        assert!(synonyms("predicament").len() >= 5);
        assert!(synonyms("meetings").contains(&"gathering"));
        assert_eq!(thesaurus_headword("Meetings"), Some("meeting"));
        assert!(synonyms("big").len() <= MAX_SYNONYMS);
        assert_eq!(synonyms("big").first(), Some(&"large"), "the short list comes first");
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
