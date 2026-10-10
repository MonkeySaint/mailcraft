//! Behaviour of the bundled en-US speller and the text checks.

use std::time::Instant;

use mailcraft_proof::{IssueKind, SpellOptions, Speller, tokenize};

fn sp() -> Speller {
    Speller::bundled_en_us()
}

#[test]
fn bundled_dictionary_is_large() {
    assert!(sp().dictionary_len() > 100_000, "{}", sp().dictionary_len());
}

#[test]
fn common_words_pass() {
    let s = sp();
    for w in [
        "the",
        "receive",
        "separate",
        "colour",
        "color",
        "email",
        "website",
        "running",
        "happily",
        "a",
        "I",
        "accommodate",
        "necessary",
        "tomorrow",
        "government",
    ] {
        assert!(s.check(w), "{w}");
    }
}

#[test]
fn misspellings_fail() {
    let s = sp();
    for w in
        ["teh", "recieve", "seperate", "definately", "accomodate", "tommorow", "goverment", "becuase", "adn"]
    {
        assert!(!s.check(w), "{w}");
    }
}

#[test]
fn case_rules() {
    let s = sp();
    assert!(s.check("Hello"));
    assert!(s.check("HELLO"));
    assert!(!s.check("hELLO"));
    assert!(s.check("Monday"));
    assert!(!s.check("monday"));
    assert!(s.check("MONDAY"));
    assert!(s.check("Ottawa"));
    // ALL-CAPS typos are skipped by default, checked when the option is off.
    assert!(s.check("TEH"));
    let mut strict = sp();
    strict.options_mut().ignore_uppercase = false;
    assert!(!strict.check("TEH"));
    assert!(strict.check("THE"));
    assert!(strict.check("OTTAWA"));
}

#[test]
fn apostrophes() {
    let s = sp();
    for w in [
        "don't",
        "Don't",
        "can't",
        "won't",
        "I'm",
        "I'll",
        "they've",
        "it's",
        "John's",
        "company's",
        "o'clock",
    ] {
        assert!(s.check(w), "{w}");
    }
    assert!(s.check("don\u{2019}t"));
    assert!(s.check("company\u{2019}s"));
    assert!(!s.check("i'm"));
    assert!(!s.check("dont't"));
    assert!(!s.check("teh's"));
    assert!(s.check("'hello'"));
}

#[test]
fn hyphens_and_dots() {
    let s = sp();
    assert!(s.check("well-known"));
    assert!(s.check("e-mail"));
    assert!(s.check("e.g"));
    assert!(!s.check("well-knwon"));
}

#[test]
fn numbers_and_addresses_ignored() {
    let s = sp();
    for w in [
        "3rd",
        "B2B",
        "abc123",
        "2024",
        "https://exmaple.org/xyzzy",
        "bob@exmaple.com",
        "www.qqq.net",
        "rpt.pdf",
    ] {
        assert!(s.check(w), "{w}");
    }
    let mut strict = sp();
    strict.options_mut().ignore_words_with_numbers = false;
    strict.options_mut().ignore_internet_and_file_addresses = false;
    assert!(!strict.check("abcxq123"));
    assert!(!strict.check("bob@exmaple.com"));
}

#[test]
fn user_dictionary_and_ignore() {
    let mut s = sp();
    assert!(!s.check("mailcraft"));
    assert!(s.add_word("mailcraft"));
    assert!(s.check("mailcraft"));
    assert!(s.check("Mailcraft"));
    assert!(s.check("MAILCRAFT"));
    assert!(s.add_word("McTavish"));
    assert!(s.check("McTavish"));
    assert!(!s.check("mctavish"));
    assert!(s.check("MCTAVISH"));
    assert_eq!(s.user_words().collect::<Vec<_>>(), ["McTavish", "mailcraft"]);
    assert!(s.remove_word("mailcraft"));
    assert!(!s.check("mailcraft"));
    assert!(!s.add_word(""));
    assert!(!s.add_word("two words"));

    assert!(s.ignore("Zorblax"));
    assert!(s.check("zorblax"));
    assert!(s.check("ZORBLAX"));
    s.clear_ignored();
    assert!(!s.check("zorblax"));

    s.set_user_words(["alpha", "", "bad word", "betaxq"]);
    assert_eq!(s.user_words().collect::<Vec<_>>(), ["alpha", "betaxq"]);
    // User words are suggested.
    assert!(s.suggest("betaxw", 5).contains(&"betaxq".to_owned()));
}

#[test]
fn suggestions_rank_the_obvious_fix_first() {
    let s = sp();
    let cases = [
        ("teh", "the"),
        ("recieve", "receive"),
        ("seperate", "separate"),
        ("definately", "definitely"),
        ("accomodate", "accommodate"),
        ("tommorow", "tomorrow"),
        ("goverment", "government"),
        ("becuase", "because"),
        ("helo", "hello"),
        ("wrold", "world"),
        ("adn", "and"),
        ("thier", "their"),
        ("wierd", "weird"),
        ("occured", "occurred"),
        ("Jhon", "John"),
    ];
    for (typo, fix) in cases {
        let got = s.suggest(typo, 5);
        assert!(got.iter().take(3).any(|g| g == fix), "{typo}: {got:?}");
    }
    assert_eq!(s.suggest("teh", 1), ["the"]);
}

#[test]
fn suggestions_cover_two_substitutions_and_two_deletions() {
    let s = sp();
    // two substitutions: "elephant" with two letters replaced
    assert!(s.suggest("elxphbnt", 10).contains(&"elephant".to_owned()));
    // two letters missing
    assert!(s.suggest("elphnt", 10).contains(&"elephant".to_owned()));
}

#[test]
fn suggestions_follow_case() {
    let s = sp();
    assert_eq!(s.suggest("Teh", 1), ["The"]);
    assert_eq!(s.suggest("TEH", 1), ["THE"]);
    assert!(s.suggest("monday", 3).contains(&"Monday".to_owned()));
    assert!(s.suggest("Otawa", 3).contains(&"Ottawa".to_owned()));
    assert!(s.suggest("hELLO", 3).contains(&"hello".to_owned()));
}

#[test]
fn suggestions_edge_cases() {
    let s = sp();
    assert!(s.suggest("", 5).is_empty());
    assert!(s.suggest("teh", 0).is_empty());
    assert!(s.suggest(&"x".repeat(500), 5).is_empty());
    assert!(s.suggest("qqqqqqqqqqqqqqqqqqqqqqqqqqqqqq", 5).is_empty());
    assert!(s.suggest("teh", 3).len() <= 3);
    let _ = s.suggest("ñandú", 3);
    let _ = s.suggest("日本語", 3);
}

#[test]
fn suggest_is_fast_after_index_build() {
    let s = sp();
    let _ = s.suggest("warmup", 5);
    let t = Instant::now();
    for w in ["teh", "recieve", "seperate", "definately", "accomodate", "tommorow", "goverment", "becuase"] {
        let _ = s.suggest(w, 8);
    }
    // Generous bound for unoptimised debug builds; release is a few ms per word.
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}

#[test]
fn tokenize_ranges() {
    let text = "Hi Bob, see https://example.org/x and don't  forget!";
    let words: Vec<_> = tokenize(text).map(|(_, _, w)| w).collect();
    assert_eq!(words, ["Hi", "Bob", "see", "https://example.org/x", "and", "don't", "forget"]);
    for (s, e, w) in tokenize(text) {
        assert_eq!(&text[s..e], w);
    }
}

fn kinds(issues: &[mailcraft_proof::Issue]) -> Vec<IssueKind> {
    issues.iter().map(|i| i.kind).collect()
}

#[test]
fn check_text_finds_everything() {
    let s = sp();
    let text = "hello there. this is teh the  test. Good  luck";
    let issues = s.check_text_with_suggestions(text, 3);
    let k = kinds(&issues);
    assert_eq!(
        k,
        [
            IssueKind::Capitalization, // hello
            IssueKind::Capitalization, // this
            IssueKind::Spelling,       // teh
            IssueKind::DoubleSpace,    // "the  test"
            IssueKind::DoubleSpace,    // "Good  luck"
        ],
        "{issues:?}"
    );
    let cap = &issues[0];
    assert_eq!(&text[cap.start..cap.end], "hello");
    assert_eq!(cap.suggestions, ["Hello"]);
    let teh = &issues[2];
    assert_eq!(&text[teh.start..teh.end], "teh");
    assert_eq!(teh.suggestions.first().map(String::as_str), Some("the"));
    let dbl = &issues[3];
    assert_eq!(&text[dbl.start..dbl.end], "  ");
    assert_eq!(dbl.suggestions, [" "]);
}

#[test]
fn repeated_words() {
    let s = sp();
    let text = "Send the the file. I had had enough. Bye bye.\nOK";
    let issues = s.check_text(text);
    let rep: Vec<_> = issues.iter().filter(|i| i.kind == IssueKind::RepeatedWord).collect();
    assert_eq!(rep.len(), 2, "{issues:?}");
    let first = rep[0];
    assert_eq!(&text[first.start..first.end], " the");
    // Applying the fix removes the second word.
    let fixed = format!("{}{}{}", &text[..first.start], first.suggestions[0], &text[first.end..]);
    assert!(fixed.starts_with("Send the file."));
    // Punctuation between them: not a repeat.
    assert!(s.check_text("It was this, this one.").iter().all(|i| i.kind != IssueKind::RepeatedWord));

    let mut off = sp();
    off.options_mut().flag_repeated_words = false;
    assert!(off.check_text("the the").iter().all(|i| i.kind != IssueKind::RepeatedWord));
}

#[test]
fn sentence_starts() {
    let s = sp();
    let caps = |t: &str| -> Vec<String> {
        s.check_text(t)
            .into_iter()
            .filter(|i| i.kind == IssueKind::Capitalization)
            .map(|i| t[i.start..i.end].to_owned())
            .collect()
    };
    assert_eq!(caps("Done. next one! and? Yes"), ["next", "and"]);
    assert_eq!(caps("See e.g. the list, etc. and more"), Vec::<String>::new());
    assert_eq!(caps("Wait... what"), Vec::<String>::new());
    assert_eq!(caps("Hi Bob,\nthanks for that"), Vec::<String>::new());
    assert_eq!(caps("Hi Bob,\n\nthanks for that"), ["thanks"]);
    assert_eq!(caps("Done. iPhone works. 3 apples"), Vec::<String>::new());
    assert_eq!(caps("Go to example.com now"), Vec::<String>::new());
    assert_eq!(caps("  \"quoted start"), ["quoted"]);
}

#[test]
fn double_spaces_only_between_words() {
    let s = sp();
    let dbl = |t: &str| s.check_text(t).into_iter().filter(|i| i.kind == IssueKind::DoubleSpace).count();
    assert_eq!(dbl("One  two"), 1);
    assert_eq!(dbl("One   two    three"), 2);
    assert_eq!(dbl("  indented"), 0);
    assert_eq!(dbl("trailing  \nNext"), 0);
    assert_eq!(dbl("One\t\ttwo"), 0);
    let mut off = sp();
    off.set_options(SpellOptions { check_grammar: false, ..SpellOptions::default() });
    assert_eq!(off.check_text("One  two. three").len(), 0);
}

#[test]
fn misspellings_iterator() {
    let s = sp();
    let text = "Thsi is a tset of bob@example.com";
    let bad: Vec<_> = s.misspellings(text).map(|(_, _, w)| w).collect();
    assert_eq!(bad, ["Thsi", "tset"]);
}

#[test]
fn custom_word_lists() {
    let s = Speller::from_word_lists(
        "zebra\nApple\nbanana\n\n# comment\n".to_owned(),
        "Zed\n".to_owned(),
        "banana\n",
    );
    assert!(s.check("zebra"));
    assert!(s.check("apple"));
    assert!(s.check("Zed"));
    assert!(!s.check("zed"));
    assert!(!s.check("comment"));
    assert_eq!(s.suggest("banan", 2), ["banana"]);
}

#[test]
fn misspellings_listed_in_the_word_lists_are_rejected() {
    // The 1990s source lists carry a few common misspellings as words; they must still be flagged and get the
    // right fix.
    let s = sp();
    for (typo, fix) in [
        ("occurence", "occurrence"),
        ("occurences", "occurrences"),
        ("responsiblity", "responsibility"),
        ("questionniare", "questionnaire"),
        ("belive", "believe"),
        ("maintainance", "maintenance"),
    ] {
        assert!(!s.check(typo), "{typo} is accepted");
        assert!(!s.suggest(&typo[..typo.len() - 1], 10).iter().any(|w| w == typo), "{typo} is suggested");
        let got = s.suggest(typo, 3);
        assert!(got.iter().any(|w| w == fix), "{typo}: {got:?}");
    }
}

#[test]
fn day_and_month_names_need_their_capital() {
    let s = sp();
    for (lower, proper) in [("wednesday", "Wednesday"), ("september", "September")] {
        assert!(!s.check(lower), "{lower}");
        assert!(s.check(proper), "{proper}");
        assert_eq!(s.suggest(lower, 1), [proper]);
        assert_eq!(s.suggest(&format!("{}x", &proper[..proper.len() - 1]), 1), [proper]);
    }
    assert_eq!(s.suggest("wendesday", 1), ["Wednesday"]);
}

#[test]
fn suggestions_prefer_swaps_and_omitted_letters() {
    // Swapped neighbours and a forgotten letter are the commonest slips; they rank above an adjacent-key
    // substitution that happens to make another word.
    let s = sp();
    for (typo, fix) in [
        ("waht", "what"),
        ("gaurd", "guard"),
        ("foward", "forward"),
        ("nad", "and"),
        ("ot", "to"),
        ("fo", "of"),
        ("suprise", "surprise"),
        ("regads", "regards"),
        ("garantee", "guarantee"),
    ] {
        assert_eq!(s.suggest(typo, 1), [fix], "{typo}: {:?}", s.suggest(typo, 5));
    }
}

#[test]
fn words_run_together_get_split_suggestions() {
    let s = sp();
    assert_eq!(s.suggest("alot", 1), ["a lot"]);
    assert_eq!(s.suggest("Alot", 1), ["A lot"]);
    assert!(s.suggest("thankyou", 3).contains(&"thank you".to_owned()), "{:?}", s.suggest("thankyou", 3));
    assert!(s.suggest("infact", 3).contains(&"in fact".to_owned()), "{:?}", s.suggest("infact", 3));
    // Single letters other than "a" are not split off; the usual fixes still come first.
    assert!(s.suggest("helo", 5).iter().all(|w| !w.contains(' ')), "{:?}", s.suggest("helo", 5));
    assert_eq!(s.suggest("teh", 1), ["the"]);
}
