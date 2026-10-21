//! Writing refinements (the Editor pane's suggestions beyond corrections): wordy phrases (conciseness),
//! passive voice (clarity), clichés, informal words (formality) and wording that leaves people out
//! (inclusiveness). The phrase lists are MailCraft's own. A refinement is advice, not an error: most carry a
//! suggested rewording, some only a note (passive voice, clichés without an obvious plain form).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::grammar::{Issue, IssueKind, MAX_ISSUES};
use crate::rules::{match_case, norm, plain_gap};
use crate::tokenize::Token;

/// Which refinement checks run ([`crate::SpellOptions::refinements`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Refinements {
    /// Passive voice.
    pub clarity: bool,
    /// Wordy phrases.
    pub conciseness: bool,
    /// Clichés and stock phrases.
    pub cliches: bool,
    /// Informal words and texting abbreviations.
    pub formality: bool,
    /// Gendered job titles and other wording that leaves people out.
    pub inclusiveness: bool,
}

impl Refinements {
    /// Every check on.
    pub const ALL: Refinements =
        Refinements { clarity: true, conciseness: true, cliches: true, formality: true, inclusiveness: true };
    /// Every check off.
    pub const NONE: Refinements = Refinements {
        clarity: false,
        conciseness: false,
        cliches: false,
        formality: false,
        inclusiveness: false,
    };

    /// Whether any check is on.
    pub fn any(self) -> bool {
        self.clarity || self.conciseness || self.cliches || self.formality || self.inclusiveness
    }
}

impl Default for Refinements {
    fn default() -> Self {
        Refinements::ALL
    }
}

/// Wordy phrase → shorter wording.
const WORDY: &[(&str, &str)] = &[
    ("in order to", "to"),
    ("in order for", "for"),
    ("at this point in time", "now"),
    ("at the present time", "now"),
    ("at this moment in time", "now"),
    ("due to the fact that", "because"),
    ("owing to the fact that", "because"),
    ("in spite of the fact that", "although"),
    ("despite the fact that", "although"),
    ("in light of the fact that", "because"),
    ("for the purpose of", "to"),
    ("in the event that", "if"),
    ("a large number of", "many"),
    ("a great number of", "many"),
    ("a small number of", "a few"),
    ("the majority of", "most"),
    ("in the near future", "soon"),
    ("prior to", "before"),
    ("subsequent to", "after"),
    ("with regard to", "about"),
    ("with regards to", "about"),
    ("in regard to", "about"),
    ("with respect to", "about"),
    ("in reference to", "about"),
    ("on a daily basis", "daily"),
    ("on a weekly basis", "weekly"),
    ("on a monthly basis", "monthly"),
    ("on a regular basis", "regularly"),
    ("each and every", "every"),
    ("first and foremost", "first"),
    ("completely finished", "finished"),
    ("past history", "history"),
    ("end result", "result"),
    ("free gift", "gift"),
    ("advance planning", "planning"),
    ("close proximity", "nearness"),
    ("in close proximity to", "near"),
    ("absolutely essential", "essential"),
    ("whether or not", "whether"),
    ("has the ability to", "can"),
    ("have the ability to", "can"),
    ("is able to", "can"),
    ("are able to", "can"),
    ("make a decision", "decide"),
    ("made a decision", "decided"),
    ("take into consideration", "consider"),
    ("give consideration to", "consider"),
    ("at all times", "always"),
    ("until such time as", "until"),
    ("in the process of", "currently"),
    ("for the most part", "mostly"),
    ("in a timely manner", "promptly"),
    ("as a means of", "to"),
    ("in addition to", "besides"),
    ("a number of", "several"),
    ("is indicative of", "indicates"),
    ("reach out to", "contact"),
    ("revert back", "revert"),
    ("please do not hesitate to", "please"),
];

/// Clichés and stock phrases → plainer wording ("" = no suggestion, only the note).
const CLICHES: &[(&str, &str)] = &[
    ("at the end of the day", "ultimately"),
    ("think outside the box", "think creatively"),
    ("low-hanging fruit", "easy wins"),
    ("touch base", "talk"),
    ("circle back", "follow up"),
    ("move the needle", "make a difference"),
    ("game changer", "breakthrough"),
    ("par for the course", "typical"),
    ("few and far between", "rare"),
    ("in a nutshell", "in short"),
    ("last but not least", "finally"),
    ("better late than never", ""),
    ("only time will tell", ""),
    ("like the plague", ""),
    ("perfect storm", ""),
    ("at the drop of a hat", "immediately"),
    ("silver lining", "upside"),
    ("hit the ground running", "start quickly"),
    ("going forward", "from now on"),
    ("it goes without saying", ""),
    ("needless to say", ""),
    ("push the envelope", "go further"),
    ("paradigm shift", "major change"),
    ("elephant in the room", "obvious problem"),
    ("when all is said and done", "in the end"),
    ("at this juncture", "now"),
    ("win-win", "good for everyone"),
    ("best of breed", "best"),
    ("on the same page", "in agreement"),
    ("ballpark figure", "estimate"),
    ("bring to the table", "offer"),
    ("deep dive", "close look"),
    ("take it offline", "discuss it separately"),
];

/// Informal words → more formal wording ("" = no suggestion).
const INFORMAL: &[(&str, &str)] = &[
    ("gonna", "going to"),
    ("wanna", "want to"),
    ("gotta", "have to"),
    ("kinda", "kind of"),
    ("sorta", "sort of"),
    ("dunno", "don't know"),
    ("yeah", "yes"),
    ("yep", "yes"),
    ("yup", "yes"),
    ("nope", "no"),
    ("thx", "thanks"),
    ("thanx", "thanks"),
    ("pls", "please"),
    ("plz", "please"),
    ("btw", "by the way"),
    ("fyi", "for your information"),
    ("imo", "in my opinion"),
    ("imho", "in my opinion"),
    ("tbh", "to be honest"),
    ("lol", ""),
    ("omg", ""),
    ("ain't", "isn't"),
    ("u", "you"),
    ("ur", "your"),
    ("cuz", "because"),
    ("coz", "because"),
    ("tho", "though"),
    ("gimme", "give me"),
    ("lemme", "let me"),
    ("y'all", "you all"),
    ("awesome", "excellent"),
];

/// Wording that leaves people out → inclusive wording.
const INCLUSIVE: &[(&str, &str)] = &[
    ("chairman", "chair"),
    ("chairwoman", "chair"),
    ("chairmen", "chairs"),
    ("manpower", "workforce"),
    ("mankind", "humanity"),
    ("man-hours", "person-hours"),
    ("man hours", "person-hours"),
    ("man-made", "artificial"),
    ("manmade", "artificial"),
    ("policeman", "police officer"),
    ("policemen", "police officers"),
    ("fireman", "firefighter"),
    ("firemen", "firefighters"),
    ("salesman", "salesperson"),
    ("salesmen", "salespeople"),
    ("businessman", "businessperson"),
    ("businessmen", "businesspeople"),
    ("spokesman", "spokesperson"),
    ("spokesmen", "spokespeople"),
    ("mailman", "mail carrier"),
    ("postman", "mail carrier"),
    ("middleman", "intermediary"),
    ("cameraman", "camera operator"),
    ("foreman", "supervisor"),
    ("layman", "layperson"),
    ("laymen", "laypeople"),
    ("housewife", "homemaker"),
    ("stewardess", "flight attendant"),
    ("waitress", "server"),
    ("manned", "staffed"),
    ("unmanned", "uncrewed"),
    ("blacklist", "blocklist"),
    ("blacklisted", "blocked"),
    ("whitelist", "allowlist"),
    ("whitelisted", "allowed"),
    ("sanity check", "quick check"),
    ("dummy value", "placeholder value"),
    ("grandfathered", "exempt"),
    ("you guys", "you all"),
    ("hi guys", "hi everyone"),
    ("hey guys", "hi everyone"),
    ("thanks guys", "thanks everyone"),
];

/// One phrase list entry, split into words.
struct Entry {
    words: Vec<&'static str>,
    fix: &'static str,
}

type Table = HashMap<&'static str, Vec<Entry>>;

fn table(list: &'static [(&'static str, &'static str)]) -> Table {
    let mut t: Table = HashMap::new();
    for &(phrase, fix) in list {
        let words: Vec<&'static str> = phrase.split([' ', '-']).filter(|w| !w.is_empty()).collect();
        if let Some(first) = words.first().copied() {
            t.entry(first).or_default().push(Entry { words, fix });
        }
    }
    // Longest phrase first, so "in close proximity to" wins over "close proximity".
    for v in t.values_mut() {
        v.sort_by_key(|e| std::cmp::Reverse(e.words.len()));
    }
    t
}

fn tables() -> &'static [(IssueKind, Table); 4] {
    static T: OnceLock<[(IssueKind, Table); 4]> = OnceLock::new();
    T.get_or_init(|| {
        [
            (IssueKind::Wordiness, table(WORDY)),
            (IssueKind::Cliche, table(CLICHES)),
            (IssueKind::Formality, table(INFORMAL)),
            (IssueKind::Inclusive, table(INCLUSIVE)),
        ]
    })
}

fn message(kind: IssueKind, fix: &str) -> String {
    match (kind, fix.is_empty()) {
        (IssueKind::Wordiness, _) => "Wordy: a shorter phrase says the same.".to_owned(),
        (IssueKind::Cliche, true) => "Cliché: consider saying it in your own words.".to_owned(),
        (IssueKind::Cliche, false) => "Cliché: plainer wording may be clearer.".to_owned(),
        (IssueKind::Formality, true) => "Informal: consider leaving it out of business mail.".to_owned(),
        (IssueKind::Formality, false) => "Informal: a more formal word may suit business mail.".to_owned(),
        (IssueKind::Inclusive, _) => "Inclusive language: this wording may leave people out.".to_owned(),
        _ => String::new(),
    }
}

/// Whether the gap between two words of a phrase matches the phrase's own separator (a hyphenated phrase
/// matches "man-made" and "man made"; a spaced one matches spacing only).
fn phrase_gap(gap: &str) -> bool {
    plain_gap(gap) || gap == "-"
}

/// Runs the refinement checks switched on in `on` over `text`.
pub(crate) fn check(text: &str, tokens: &[Token<'_>], on: Refinements, issues: &mut Vec<Issue>) {
    if !on.any() {
        return;
    }
    let lower: Vec<String> = tokens.iter().map(|(_, _, w)| norm(w)).collect();
    let gap_ok = |i: usize| -> bool {
        match (tokens.get(i), tokens.get(i.saturating_add(1))) {
            (Some(&(_, a, _)), Some(&(b, _, _))) if a <= b => text.get(a..b).is_some_and(phrase_gap),
            _ => false,
        }
    };
    for (kind, table) in tables() {
        let enabled = match kind {
            IssueKind::Wordiness => on.conciseness,
            IssueKind::Cliche => on.cliches,
            IssueKind::Formality => on.formality,
            IssueKind::Inclusive => on.inclusiveness,
            _ => false,
        };
        if !enabled {
            continue;
        }
        let mut i = 0usize;
        while i < tokens.len() {
            if issues.len() >= MAX_ISSUES {
                return;
            }
            let first = lower.get(i).map_or("", String::as_str);
            let hit = table.get(first).and_then(|entries| {
                entries.iter().find(|e| {
                    e.words.iter().enumerate().all(|(k, w)| {
                        lower.get(i + k).is_some_and(|l| l == w) && (k == 0 || gap_ok(i + k - 1))
                    })
                })
            });
            let Some(e) = hit else {
                i += 1;
                continue;
            };
            let n = e.words.len().max(1);
            let (Some(&(start, _, src)), Some(&(_, end, _))) = (tokens.get(i), tokens.get(i + n - 1)) else {
                break;
            };
            // Texting abbreviations: only in lower case ("U" and "UR" are often names or codes).
            let skip = *kind == IssueKind::Formality && matches!(first, "u" | "ur") && src != first;
            if !skip {
                let suggestions = if e.fix.is_empty() { Vec::new() } else { vec![match_case(src, e.fix)] };
                issues.push(Issue { start, end, kind: *kind, message: message(*kind, e.fix), suggestions });
            }
            i += n;
        }
    }
    if on.formality {
        exclamations(text, issues);
    }
    if on.clarity {
        passive_voice(text, tokens, &lower, issues);
    }
}

/// "!!" and longer runs: one exclamation mark reads as more professional.
fn exclamations(text: &str, issues: &mut Vec<Issue>) {
    let mut run: Option<usize> = None;
    let mut count = 0usize;
    let flush = |run: &mut Option<usize>, count: &mut usize, end: usize, issues: &mut Vec<Issue>| {
        if let Some(s) = run.take()
            && *count >= 2
            && issues.len() < MAX_ISSUES
        {
            issues.push(Issue {
                start: s,
                end,
                kind: IssueKind::Formality,
                message: "Informal: one exclamation mark is enough.".to_owned(),
                suggestions: vec!["!".to_owned()],
            });
        }
        *count = 0;
    };
    for (i, c) in text.char_indices() {
        if c == '!' {
            if run.is_none() {
                run = Some(i);
            }
            count += 1;
        } else {
            flush(&mut run, &mut count, i, issues);
        }
    }
    flush(&mut run, &mut count, text.len(), issues);
}

const BE: &[&str] = &["am", "is", "are", "was", "were", "be", "been", "being"];
/// Irregular past participles.
const IRREGULAR: &[&str] = &[
    "written",
    "given",
    "taken",
    "made",
    "done",
    "seen",
    "known",
    "shown",
    "sent",
    "built",
    "held",
    "told",
    "found",
    "kept",
    "left",
    "paid",
    "sold",
    "brought",
    "bought",
    "caught",
    "taught",
    "thought",
    "chosen",
    "driven",
    "eaten",
    "forgotten",
    "broken",
    "spoken",
    "stolen",
    "frozen",
    "hidden",
    "begun",
    "drawn",
    "grown",
    "thrown",
    "worn",
    "torn",
    "sworn",
    "led",
    "met",
    "won",
    "spent",
    "lost",
    "meant",
    "understood",
    "forgiven",
    "overlooked",
    "undertaken",
    "withdrawn",
    "shaken",
    "woken",
    "set",
    "put",
    "cut",
    "hit",
    "shut",
];
/// "-ed" words that usually describe a state after "be" ("I am interested"), not a passive action.
const STATE_ED: &[&str] = &[
    "interested",
    "tired",
    "excited",
    "pleased",
    "concerned",
    "worried",
    "supposed",
    "used",
    "married",
    "bored",
    "scared",
    "surprised",
    "satisfied",
    "disappointed",
    "confused",
    "embarrassed",
    "annoyed",
    "amazed",
    "delighted",
    "ashamed",
    "prepared",
    "closed",
    "finished",
    "located",
    "based",
    "involved",
    "related",
    "qualified",
    "experienced",
    "detailed",
    "advanced",
    "complicated",
    "dedicated",
    "limited",
    "talented",
    "need",
    "red",
    "bed",
    "shed",
    "fed",
    "led",
    "engaged",
    "exhausted",
    "thrilled",
    "relieved",
    "stressed",
    "overwhelmed",
    "honored",
    "honoured",
    "convinced",
    "determined",
    "focused",
    "organized",
    "organised",
    "allowed",
    "aged",
    "beloved",
    "crowded",
    "armed",
    "skilled",
    "noted",
    "united",
    "unrelated",
    "unsupported",
    "outdated",
    "booked",
    "done",
    "set",
];

fn participle(word: &str) -> bool {
    if STATE_ED.contains(&word) {
        return false;
    }
    IRREGULAR.contains(&word)
        || (word.len() >= 5 && word.ends_with("ed") && word.chars().all(char::is_alphabetic))
}

/// "was sent", "is being reviewed", "were quickly approved by": a form of "be", an optional "-ly" adverb or
/// "being", then a past participle.
fn passive_voice(text: &str, tokens: &[Token<'_>], lower: &[String], issues: &mut Vec<Issue>) {
    let word = |i: usize| lower.get(i).map_or("", String::as_str);
    let gap_plain = |i: usize| match (tokens.get(i), tokens.get(i.saturating_add(1))) {
        (Some(&(_, a, _)), Some(&(b, _, _))) if a <= b => text.get(a..b).is_some_and(plain_gap),
        _ => false,
    };
    let mut i = 0usize;
    while i < tokens.len() {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        if !BE.contains(&word(i)) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        // At most two words between: an adverb and / or "being".
        for _ in 0..2 {
            let w = word(j);
            if gap_plain(j - 1) && ((w.ends_with("ly") && w.len() > 3) || w == "being") {
                j += 1;
            } else {
                break;
            }
        }
        let w = word(j);
        let follows_to = gap_plain(j) && word(j + 1) == "to";
        if gap_plain(j - 1) && participle(w) && !follows_to {
            if let (Some(&(start, _, _)), Some(&(_, end, _))) = (tokens.get(i), tokens.get(j)) {
                issues.push(Issue {
                    start,
                    end,
                    kind: IssueKind::PassiveVoice,
                    message: "Passive voice: saying who does the action is often clearer.".to_owned(),
                    suggestions: Vec::new(),
                });
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participles() {
        assert!(participle("approved"));
        assert!(participle("written"));
        assert!(!participle("interested"));
        assert!(!participle("bed"));
        assert!(!participle("need"));
    }

    #[test]
    fn tables_are_lower_case_and_split() {
        for (_, t) in tables().iter() {
            for (k, v) in t {
                assert_eq!(*k, k.to_lowercase());
                for e in v {
                    assert!(!e.words.is_empty());
                }
            }
        }
    }
}
