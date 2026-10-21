//! Grammar rules (agreement, punctuation, confused words) and writing refinements: what is flagged, what is
//! left alone, and that the suggestion fixes the text.

use mailcraft_proof::{Issue, IssueKind, Refinements, SpellOptions, Speller};

fn sp() -> &'static Speller {
    static S: std::sync::OnceLock<Speller> = std::sync::OnceLock::new();
    S.get_or_init(Speller::bundled_en_us)
}

fn issues(text: &str, kind: IssueKind) -> Vec<Issue> {
    sp().check_text(text).into_iter().filter(|i| i.kind == kind).collect()
}

/// The text with the first issue of `kind` fixed by its first suggestion.
fn fixed(text: &str, kind: IssueKind) -> String {
    let found = issues(text, kind);
    let Some(i) = found.first() else { return format!("(no {kind:?} issue in {text:?})") };
    let fix = i.suggestions.first().cloned().unwrap_or_default();
    format!("{}{}{}", text.get(..i.start).unwrap_or(""), fix, text.get(i.end..).unwrap_or(""))
}

fn clean(text: &str, kind: IssueKind) {
    let found = issues(text, kind);
    assert!(found.is_empty(), "{text:?} flagged {kind:?}: {found:?}");
}

#[test]
fn articles_agree_with_the_next_sound() {
    assert_eq!(fixed("We had a apple.", IssueKind::Agreement), "We had an apple.");
    assert_eq!(fixed("It took an day.", IssueKind::Agreement), "It took a day.");
    assert_eq!(fixed("A orange is fine.", IssueKind::Agreement), "An orange is fine.");
    assert_eq!(fixed("Wait a hour.", IssueKind::Agreement), "Wait an hour.");
    for ok in [
        "It is a user guide.",
        "Send a one-time code.",
        "She is an honest person.",
        "He has an MBA.",
        "We use a URL here.",
        "It is a university.",
        "That was an unimportant detail.",
        "Plan A is ready.",
        "Pick a or b.",
        "It was a European trip.",
    ] {
        clean(ok, IssueKind::Agreement);
    }
}

#[test]
fn pronouns_agree_with_their_verbs() {
    assert_eq!(fixed("She don't agree.", IssueKind::Agreement), "She doesn't agree.");
    assert_eq!(fixed("They was late.", IssueKind::Agreement), "They were late.");
    assert_eq!(fixed("I is ready.", IssueKind::Agreement), "I am ready.");
    assert_eq!(fixed("We has a plan.", IssueKind::Agreement), "We have a plan.");
    assert_eq!(fixed("I think you is right.", IssueKind::Agreement), "I think you are right.");
    for ok in [
        "Did she have time?",
        "What it means to you is clear.",
        "If I were you, I would wait.",
        "Let it have a moment.",
        "The gift for you is here.",
        "He doesn't know.",
    ] {
        clean(ok, IssueKind::Agreement);
    }
}

#[test]
fn the_pronoun_i_is_capitalised() {
    assert_eq!(fixed("Yes, i think so.", IssueKind::Capitalization), "Yes, I think so.");
    clean("See section (i) below, i.e. the list.", IssueKind::Capitalization);
}

#[test]
fn commonly_confused_words() {
    let cases = [
        ("We could of won.", "We could have won."),
        ("Their is a problem.", "There is a problem."),
        ("This is better then that.", "This is better than that."),
        ("Since than it has worked.", "Since then it has worked."),
        ("Thanks! Your welcome.", "Thanks! You're welcome."),
        ("I think your right.", "I think you're right."),
        ("Its a good idea.", "It's a good idea."),
        ("The team has it's own room.", "The team has its own room."),
        ("Don't loose the key.", "Don't lose the key."),
        ("Lets meet tomorrow.", "Let's meet tomorrow."),
        ("The affect of the change was small.", "The effect of the change was small."),
        ("I am use to it.", "I am used to it."),
        ("We are suppose to finish.", "We are supposed to finish."),
        ("They have their own office.", "They have their own office."),
    ];
    for (text, want) in cases {
        if text == want {
            clean(text, IssueKind::ConfusedWord);
        } else {
            assert_eq!(fixed(text, IssueKind::ConfusedWord), want, "{text}");
        }
    }
    for ok in [
        "Your welcome pack is attached.",
        "Its colour is blue.",
        "The knot is loose.",
        "It will affect the plan.",
        "We met then left.",
        "It costs more than that.",
        "He lets them go home.",
        // Correct uses of the words the rules look for.
        "We defend your right to vote.",
        "It is your very own copy.",
        "I appreciate your being here.",
        "Your going-away party is on Friday.",
        "Its very existence is a surprise.",
        "Its true value is unknown.",
        "The shares hit its all-time high.",
        "Check its time zone.",
        "Its fine print is long.",
        "The cable is not loose.",
        "Try once more then send it.",
        "Wait a bit longer then retry.",
    ] {
        clean(ok, IssueKind::ConfusedWord);
    }
    assert_eq!(
        fixed("It took longer then expected.", IssueKind::ConfusedWord),
        "It took longer than expected."
    );
    assert_eq!(fixed("We need more then 20 seats.", IssueKind::ConfusedWord), "We need more than 20 seats.");
}

#[test]
fn confused_words_follow_their_option() {
    let mut s = sp().clone();
    let text = "We could of won.";
    assert!(s.check_text(text).iter().any(|i| i.kind == IssueKind::ConfusedWord));
    s.set_options(SpellOptions { confused_words: false, ..SpellOptions::default() });
    assert!(s.check_text(text).iter().all(|i| i.kind != IssueKind::ConfusedWord));
    // The other grammar checks still run.
    assert!(s.check_text("She don't know.").iter().any(|i| i.kind == IssueKind::Agreement));
}

#[test]
fn a_long_line_without_spaces_is_checked_quickly() {
    // 1 MB of an address-like run with commas and sentence ends: each chunk is measured once.
    let mut text = String::from("http://x/");
    while text.len() < 1_000_000 {
        text.push_str("a,b.Cd,");
    }
    let t = std::time::Instant::now();
    let found = sp().check_text(&text);
    assert!(found.iter().all(|i| i.kind != IssueKind::Punctuation), "an address is not punctuation");
    // The same without the address marker: issues are capped, the walk stays linear.
    let plain = text.replacen("http://x/", "x", 1);
    let found = sp().check_text(&plain);
    assert!(found.len() <= 10_000);
    assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
}

#[test]
fn punctuation_spacing_and_doubled_marks() {
    assert_eq!(fixed("Hello , world.", IssueKind::Punctuation), "Hello, world.");
    assert_eq!(fixed("Is it done ?", IssueKind::Punctuation), "Is it done?");
    assert_eq!(fixed("Apples,pears and plums.", IssueKind::Punctuation), "Apples, pears and plums.");
    assert_eq!(fixed("Wait,, then go.", IssueKind::Punctuation), "Wait, then go.");
    assert_eq!(fixed("It is done.. Thanks.", IssueKind::Punctuation), "It is done. Thanks.");
    assert_eq!(fixed("It works.Thanks again.", IssueKind::Punctuation), "It works. Thanks again.");
    assert_eq!(fixed("See ( below) for more.", IssueKind::Punctuation), "See (below) for more.");
    for ok in [
        "It costs 1,000 dollars.",
        "Well... maybe.",
        "Visit https://example.com/a,b today.",
        "Mail ann@example.com.Thanks",
        "Use .NET for this.",
        "Smile :) please.",
        "Time: 10:30.",
    ] {
        clean(ok, IssueKind::Punctuation);
    }
}

#[test]
fn refinements_flag_wordy_passive_cliche_informal_and_exclusive_wording() {
    assert_eq!(fixed("We met in order to plan.", IssueKind::Wordiness), "We met to plan.");
    assert_eq!(
        fixed("Due to the fact that it rained, we stayed.", IssueKind::Wordiness),
        "Because it rained, we stayed."
    );
    assert_eq!(fixed("Let's touch base on Monday.", IssueKind::Cliche), "Let's talk on Monday.");
    assert_eq!(
        fixed("We will revert back to the old version.", IssueKind::Wordiness),
        "We will revert to the old version."
    );
    assert_eq!(fixed("I'm gonna call.", IssueKind::Formality), "I'm going to call.");
    assert_eq!(fixed("Thanks!!!", IssueKind::Formality), "Thanks!");
    assert_eq!(fixed("Ask the chairman.", IssueKind::Inclusive), "Ask the chair.");
    assert_eq!(fixed("We need more manpower.", IssueKind::Inclusive), "We need more workforce.");
    assert_eq!(fixed("It is man-made.", IssueKind::Inclusive), "It is artificial.");
    let passive = issues("The report was written by the team.", IssueKind::PassiveVoice);
    assert_eq!(passive.len(), 1);
    assert!(passive[0].suggestions.is_empty(), "passive voice has no one-click rewording");
    assert_eq!(&"The report was written by the team."[passive[0].start..passive[0].end], "was written");
    let adverb = issues("It was quickly approved.", IssueKind::PassiveVoice);
    assert_eq!(adverb.len(), 1);
    for ok in ["I am interested in it.", "We are used to it.", "She was tired.", "They are based in Ottawa."]
    {
        clean(ok, IssueKind::PassiveVoice);
    }
    clean("UR-12 is the code.", IssueKind::Formality);
}

#[test]
fn refinements_follow_the_options() {
    let mut s = sp().clone();
    let text = "In order to win we must touch base. It was approved. Gonna go. Ask the chairman.";
    let refined = |s: &Speller| s.check_text(text).iter().filter(|i| i.kind.is_refinement()).count();
    assert_eq!(refined(&s), 5);
    s.set_options(SpellOptions { refinements: Refinements::NONE, ..SpellOptions::default() });
    assert_eq!(refined(&s), 0);
    s.set_options(SpellOptions {
        refinements: Refinements { inclusiveness: true, ..Refinements::NONE },
        ..SpellOptions::default()
    });
    let kinds: Vec<IssueKind> =
        s.check_text(text).iter().map(|i| i.kind).filter(|k| k.is_refinement()).collect();
    assert_eq!(kinds, [IssueKind::Inclusive]);
    // Grammar off: the new rules are off too.
    s.set_options(SpellOptions { check_grammar: false, ..SpellOptions::default() });
    assert!(s.check_text("She don't know.").iter().all(|i| i.kind != IssueKind::Agreement));
}

#[test]
fn kinds_have_ids_and_categories() {
    for k in IssueKind::ALL {
        assert_eq!(IssueKind::from_id(k.id()), Some(k));
        assert!(!k.category().is_empty());
        assert!(!(k.is_grammar() && k.is_refinement()));
    }
    assert_eq!(IssueKind::PassiveVoice.category(), "clarity");
    assert_eq!(IssueKind::Agreement.category(), "grammar");
    assert!(!IssueKind::Spelling.is_grammar());
}

#[test]
fn overlapping_issues_are_reported_once() {
    // "word  ," is both a double space and a space before a comma: one issue for the range.
    let all = sp().check_text("Hello  , world.");
    let grammar: Vec<&Issue> = all.iter().filter(|i| i.kind.is_grammar()).collect();
    assert_eq!(grammar.len(), 1, "{all:?}");
    // Every range is valid and issues come sorted.
    let text = "i think they was gonna loose it,, at the end of the day!!";
    let got = sp().check_text(text);
    assert!(got.windows(2).all(|w| w[0].start <= w[1].start));
    for i in &got {
        assert!(text.get(i.start..i.end).is_some());
    }
}
