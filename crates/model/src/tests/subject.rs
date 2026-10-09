use proptest::prelude::*;

use crate::subject::{conversation_topic, forward_subject, has_prefix, reply_subject, strip_prefixes};

#[test]
fn strips_common_prefixes() {
    let cases = [
        ("Re: Lunch", "Lunch"),
        ("RE: Lunch", "Lunch"),
        ("re:Lunch", "Lunch"),
        ("FW: Lunch", "Lunch"),
        ("Fw: Lunch", "Lunch"),
        ("Fwd: Lunch", "Lunch"),
        ("FWD: Lunch", "Lunch"),
        ("AW: Mittag", "Mittag"),
        ("WG: Mittag", "Mittag"),
        ("SV: Lunsj", "Lunsj"),
        ("VS: Lounas", "Lounas"),
        ("RE: RE: FW: Lunch", "Lunch"),
        ("Re:Re:Re:Lunch", "Lunch"),
        ("Re[2]: Lunch", "Lunch"),
        ("RE[12]: Re[3]: Lunch", "Lunch"),
        ("Re(4): Lunch", "Lunch"),
        ("Re : Lunch", "Lunch"),
        ("Re\u{FF1A}Lunch", "Lunch"),
        ("   Re:   Lunch   ", "Lunch"),
        ("Re:", ""),
        ("RE: ", ""),
        ("", ""),
    ];
    for (input, want) in cases {
        assert_eq!(strip_prefixes(input), want, "input {input:?}");
    }
}

#[test]
fn leaves_lookalikes_alone() {
    for s in [
        "Revenue: Q3",
        "Reply: needed",
        "Re",
        "Re[]: x",
        "Re[x]: x",
        "Re[2 x",
        "Forward planning",
        "Svar: hej",
        "Fwd Lunch",
        "Lunch Re: later",
        "[list] Re: topic",
    ] {
        assert_eq!(strip_prefixes(s), s.trim(), "input {s:?}");
    }
}

#[test]
fn reply_and_forward_subjects() {
    assert_eq!(reply_subject("Lunch on Friday?"), "RE: Lunch on Friday?");
    assert_eq!(reply_subject("Re: Lunch on Friday?"), "RE: Lunch on Friday?");
    assert_eq!(reply_subject("RE: RE: Lunch"), "RE: Lunch");
    assert_eq!(forward_subject("Lunch"), "FW: Lunch");
    assert_eq!(forward_subject("RE: Lunch"), "FW: Lunch");
    assert_eq!(forward_subject("Fwd: Lunch"), "FW: Lunch");
    assert_eq!(reply_subject(""), "RE: ");
}

#[test]
fn has_prefix_detects() {
    assert!(has_prefix("RE: x"));
    assert!(has_prefix("  fw: x"));
    assert!(!has_prefix("x"));
    assert!(!has_prefix("Revenue: x"));
}

#[test]
fn conversation_topic_normalises() {
    assert_eq!(conversation_topic("RE:  Hello   World "), "hello world");
    assert_eq!(conversation_topic("FW: hello\tworld"), "hello world");
    assert_eq!(conversation_topic("Hello World"), conversation_topic("re: fwd: HELLO  world"));
    assert_eq!(conversation_topic("RE:"), "");
}

proptest! {
    #[test]
    fn strip_is_suffix_and_idempotent(s in ".{0,80}") {
        let stripped = strip_prefixes(&s);
        prop_assert!(s.contains(stripped));
        prop_assert_eq!(strip_prefixes(stripped), stripped);
    }

    #[test]
    fn prefixed_subject_strips_to_topic(topic in "[A-Za-z0-9 ]{0,30}", n in 0usize..5) {
        let mut subject = topic.clone();
        for i in 0..n {
            let p = ["RE: ", "Fw: ", "AW:", "Re[2]: ", "FWD : "][i % 5];
            subject = format!("{p}{subject}");
        }
        // Only valid when the topic itself does not start with a prefix-looking word.
        prop_assume!(!has_prefix(&topic));
        prop_assert_eq!(strip_prefixes(&subject), topic.trim());
    }

    #[test]
    fn hostile_subjects(s in prop::collection::vec(prop_oneof![
        Just("re:".to_string()), Just("[".to_string()), Just("(9".to_string()), Just(" ".to_string()),
        Just("fw".to_string()), Just("\u{FF1A}".to_string()), "[a-z\\[\\]:()0-9 ]{0,4}"
    ], 0..60)) {
        let s = s.concat();
        let _ = strip_prefixes(&s);
        let _ = conversation_topic(&s);
        let _ = reply_subject(&s);
    }
}
