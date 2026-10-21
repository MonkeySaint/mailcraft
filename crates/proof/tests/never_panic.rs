//! Hostile input: nothing in the crate panics, and every reported range is a valid char-boundary slice.

use mailcraft_proof::{SpellOptions, Speller, looks_like_address, tokenize};
use proptest::prelude::*;

fn speller() -> &'static Speller {
    static S: std::sync::OnceLock<Speller> = std::sync::OnceLock::new();
    S.get_or_init(Speller::bundled_en_us)
}

fn texty() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        "[a-zA-Z' .,!?\\-\n\t@/:]{0,200}",
        "([a-z]{1,8}[ .]{1,3}){0,20}",
        "[\\PC\u{2019}\u{2026}]{0,80}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn tokenize_ranges_are_valid(text in texty()) {
        for (s, e, w) in tokenize(&text) {
            prop_assert!(s < e);
            prop_assert_eq!(text.get(s..e), Some(w));
        }
    }

    #[test]
    fn check_and_suggest_never_panic(word in texty()) {
        let s = speller();
        let _ = s.check(&word);
        let _ = looks_like_address(&word);
        let got = s.suggest(&word, 5);
        prop_assert!(got.len() <= 5);
    }

    #[test]
    fn check_text_ranges_are_valid(text in texty(), grammar in any::<bool>(), caps in any::<bool>()) {
        let mut s = speller().clone();
        s.set_options(SpellOptions {
            check_grammar: grammar,
            ignore_uppercase: caps,
            ignore_words_with_numbers: !caps,
            ignore_internet_and_file_addresses: grammar,
            flag_repeated_words: true,
            main_dictionary_only: caps,
        });
        for issue in s.check_text_with_suggestions(&text, 2) {
            prop_assert!(issue.start <= issue.end);
            prop_assert!(text.get(issue.start..issue.end).is_some());
        }
    }

    #[test]
    fn user_dictionary_never_panics(words in proptest::collection::vec(any::<String>(), 0..20), probe in any::<String>()) {
        let mut s = Speller::from_word_lists("alpha\nbeta\n".to_owned(), "Gamma\n".to_owned(), "");
        s.set_user_words(&words);
        for w in &words {
            let _ = s.ignore(w);
            let _ = s.check(w);
        }
        let _ = s.check(&probe);
        let _ = s.suggest(&probe, 3);
        let _ = s.remove_word(&probe);
    }
}

#[test]
fn custom_lists_with_junk() {
    let junk = "\u{0}\n\r\n   \n#\n\u{FEFF}word\n".repeat(10) + &"x".repeat(10_000);
    let s = Speller::from_word_lists(junk.clone(), junk, "\n\n");
    let _ = s.check("word");
    let _ = s.suggest("wrod", 5);
    let _ = s.check_text("word wrod  word word.");
}
