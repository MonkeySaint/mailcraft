use proptest::prelude::*;

use super::{addr, mail};
use crate::rule::RuleContext;
use crate::{
    AttachmentMeta, Body, DateTime, FlagState, FolderId, Importance, MailItem, Rule, RuleAction,
    RuleCondition, Sensitivity,
};

fn ctx() -> RuleContext {
    RuleContext {
        my_addresses: vec!["me@example.net".into(), "alias@example.net".into()],
        account_address: "me@example.net".into(),
    }
}

fn sample() -> MailItem {
    MailItem {
        from: Some(addr("Alice Rivera", "alice@example.com")),
        to: vec![addr("Test User", "Me@Example.net")],
        cc: vec![addr("Bob Chen", "bob.chen@example.org")],
        bcc: vec![addr("", "hidden@example.org")],
        body: Body { text: Some("The badge reader is down.".into()), ..Default::default() },
        size: 5000,
        received: DateTime { unix: 1_000, offset_min: 0 },
        categories: vec!["Red category".into()],
        ..mail("URGENT: server room access")
    }
}

fn cond(c: RuleCondition) -> bool {
    Rule { conditions: vec![c], ..Default::default() }.matches(&sample(), &ctx())
}

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_rule_matches_everything() {
    assert!(Rule::default().matches(&sample(), &ctx()));
    assert!(Rule::default().matches(&MailItem::default(), &RuleContext::default()));
}

#[test]
fn text_conditions() {
    assert!(cond(RuleCondition::SubjectContains(words(&["server ROOM"]))));
    assert!(cond(RuleCondition::SubjectContains(words(&["nope", "urgent"]))));
    assert!(!cond(RuleCondition::SubjectContains(words(&["nope"]))));
    assert!(!cond(RuleCondition::SubjectContains(vec![])));
    assert!(!cond(RuleCondition::SubjectContains(words(&["", "  "]))));
    assert!(cond(RuleCondition::BodyContains(words(&["badge"]))));
    assert!(!cond(RuleCondition::BodyContains(words(&["server"]))));
    assert!(cond(RuleCondition::SubjectOrBodyContains(words(&["badge"]))));
    assert!(cond(RuleCondition::SubjectOrBodyContains(words(&["server"]))));
    assert!(!cond(RuleCondition::SubjectOrBodyContains(words(&["lunch"]))));
}

#[test]
fn body_contains_uses_html_when_no_text() {
    let item = MailItem {
        body: Body {
            html: Some("<p>Quarterly <b>numbers</b></p><style>.secret{}</style>".into()),
            ..Default::default()
        },
        ..mail("x")
    };
    let rule =
        |w: &str| Rule { conditions: vec![RuleCondition::BodyContains(words(&[w]))], ..Default::default() };
    assert!(rule("numbers").matches(&item, &ctx()));
    assert!(!rule("secret").matches(&item, &ctx()));
    assert!(!rule("<b>").matches(&item, &ctx()));
}

#[test]
fn header_contains_synthesised_and_raw() {
    assert!(cond(RuleCondition::HeaderContains(words(&["alice@example.com"]))));
    assert!(cond(RuleCondition::HeaderContains(words(&["subject: urgent"]))));
    let raw_item = MailItem {
        raw: Some(b"X-Spam-Flag: YES\r\nSubject: hi\r\n\r\nbody mentions X-Mailer-Body".to_vec()),
        ..mail("hi")
    };
    let r =
        |w: &str| Rule { conditions: vec![RuleCondition::HeaderContains(words(&[w]))], ..Default::default() };
    assert!(r("x-spam-flag: yes").matches(&raw_item, &ctx()));
    assert!(!r("X-Mailer-Body").matches(&raw_item, &ctx()));
}

#[test]
fn people_conditions() {
    assert!(cond(RuleCondition::From(words(&["alice rivera"]))));
    assert!(cond(RuleCondition::From(words(&["ALICE@example.com"]))));
    assert!(!cond(RuleCondition::From(words(&["bob"]))));
    assert!(cond(RuleCondition::SentTo(words(&["bob chen"]))));
    assert!(!cond(RuleCondition::SentTo(words(&["hidden@"])))); // Bcc is not "sent to"
    assert!(cond(RuleCondition::RecipientAddressContains(words(&["hidden@"]))));
    assert!(cond(RuleCondition::RecipientAddressContains(words(&["example.org"]))));
    assert!(!cond(RuleCondition::RecipientAddressContains(words(&["Test User"]))));
    assert!(cond(RuleCondition::SenderAddressContains(words(&["@example.com"]))));
    assert!(!cond(RuleCondition::SenderAddressContains(words(&["Rivera"]))));
}

#[test]
fn sender_header_counts_as_from() {
    let item = MailItem { sender: Some(addr("Assistant", "pa@example.com")), ..sample() };
    let r = Rule { conditions: vec![RuleCondition::From(words(&["pa@example.com"]))], ..Default::default() };
    assert!(r.matches(&item, &ctx()));
}

#[test]
fn account_condition() {
    assert!(cond(RuleCondition::ThroughAccount("ME@example.net".into())));
    assert!(!cond(RuleCondition::ThroughAccount("other@example.net".into())));
    assert!(!cond(RuleCondition::ThroughAccount(String::new())));
}

#[test]
fn me_conditions() {
    let c = ctx();
    let only_me = MailItem { to: vec![addr("", "alias@example.net")], cc: vec![], ..mail("x") };
    let r = |cond: RuleCondition, item: &MailItem| {
        Rule { conditions: vec![cond], ..Default::default() }.matches(item, &c)
    };
    assert!(r(RuleCondition::SentOnlyToMe, &only_me));
    assert!(!r(RuleCondition::SentOnlyToMe, &sample())); // has Cc
    let two_to = MailItem { to: vec![addr("", "me@example.net"), addr("", "x@example.net")], ..mail("x") };
    assert!(!r(RuleCondition::SentOnlyToMe, &two_to));
    let not_me = MailItem { to: vec![addr("", "x@example.net")], ..mail("x") };
    assert!(!r(RuleCondition::SentOnlyToMe, &not_me));

    assert!(r(RuleCondition::MyNameInTo, &sample()));
    assert!(!r(RuleCondition::MyNameInCc, &sample()));
    let in_cc =
        MailItem { to: vec![addr("", "x@example.net")], cc: vec![addr("", "me@example.net")], ..mail("x") };
    assert!(r(RuleCondition::MyNameInCc, &in_cc));
    assert!(r(RuleCondition::MyNameInToOrCc, &in_cc));
    assert!(r(RuleCondition::MyNameNotInTo, &in_cc));
    assert!(!r(RuleCondition::MyNameNotInTo, &sample()));
    assert!(!r(RuleCondition::MyNameInToOrCc, &not_me));
    assert!(r(RuleCondition::MyNameNotInTo, &MailItem::default()));
}

#[test]
fn property_conditions() {
    let mut item = sample();
    item.importance = Importance::High;
    item.sensitivity = Sensitivity::Confidential;
    item.flag = FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
    item.attachments = vec![AttachmentMeta { filename: "a.pdf".into(), ..Default::default() }];
    let r = |c: RuleCondition| Rule { conditions: vec![c], ..Default::default() }.matches(&item, &ctx());
    assert!(r(RuleCondition::MarkedImportance(Importance::High)));
    assert!(!r(RuleCondition::MarkedImportance(Importance::Low)));
    assert!(r(RuleCondition::MarkedSensitivity(Sensitivity::Confidential)));
    assert!(!r(RuleCondition::MarkedSensitivity(Sensitivity::Private)));
    assert!(r(RuleCondition::FlaggedForAction));
    assert!(r(RuleCondition::HasAttachment));
    assert!(r(RuleCondition::AssignedToCategory("red CATEGORY".into())));
    assert!(!r(RuleCondition::AssignedToCategory("Blue category".into())));
    assert!(!r(RuleCondition::AssignedToCategory(String::new())));

    assert!(!cond(RuleCondition::FlaggedForAction));
    assert!(!cond(RuleCondition::HasAttachment));
    let inline_only =
        MailItem { attachments: vec![AttachmentMeta { inline: true, ..Default::default() }], ..sample() };
    let has_att = Rule { conditions: vec![RuleCondition::HasAttachment], ..Default::default() };
    assert!(!has_att.matches(&inline_only, &ctx()));
    let complete = MailItem { flag: FlagState::Complete { completed: None }, ..sample() };
    let flagged = Rule { conditions: vec![RuleCondition::FlaggedForAction], ..Default::default() };
    assert!(!flagged.matches(&complete, &ctx()));
}

#[test]
fn ranges() {
    // 5000 bytes = 5 KB (rounded up).
    assert!(cond(RuleCondition::SizeBetween(5, 5)));
    assert!(cond(RuleCondition::SizeBetween(1, 10)));
    assert!(cond(RuleCondition::SizeBetween(10, 1))); // reversed bounds are normalised
    assert!(!cond(RuleCondition::SizeBetween(6, 100)));
    assert!(cond(RuleCondition::ReceivedBetween(1_000, 1_000)));
    assert!(cond(RuleCondition::ReceivedBetween(2_000, 0)));
    assert!(!cond(RuleCondition::ReceivedBetween(1_001, 5_000)));
}

#[test]
fn all_conditions_must_match_and_exceptions_cancel() {
    let mut rule = Rule {
        conditions: vec![
            RuleCondition::SubjectContains(words(&["urgent"])),
            RuleCondition::From(words(&["alice"])),
        ],
        ..Default::default()
    };
    assert!(rule.matches(&sample(), &ctx()));
    rule.conditions.push(RuleCondition::HasAttachment);
    assert!(!rule.matches(&sample(), &ctx()));
    rule.conditions.pop();
    rule.exceptions = vec![RuleCondition::HasAttachment, RuleCondition::MyNameInCc];
    assert!(rule.matches(&sample(), &ctx()));
    rule.exceptions.push(RuleCondition::BodyContains(words(&["badge"])));
    assert!(!rule.matches(&sample(), &ctx()));
}

#[test]
fn describe_matches_observed_example() {
    let rule = Rule {
        name: "MailCraft test".into(),
        enabled: true,
        conditions: vec![RuleCondition::SubjectContains(words(&["[MailCraft test]"]))],
        actions: vec![RuleAction::NewItemAlert("[MailCraft test]".into()), RuleAction::MarkAsRead],
        ..Default::default()
    };
    let names = |_: FolderId| String::new();
    assert_eq!(
        rule.describe(&names),
        [
            "Apply this rule after the message arrives",
            "with [MailCraft test] in the subject",
            "display [MailCraft test] in the New Item Alert window",
            "and mark it as read",
        ]
    );
    assert!(rule.is_client_only());
    assert_eq!(rule.list_label(), "MailCraft test  (client-only)");
}

#[test]
fn describe_conditions_actions_exceptions() {
    let rule = Rule {
        conditions: vec![
            RuleCondition::From(words(&["alice@example.com", "bob@example.com"])),
            RuleCondition::SubjectContains(words(&["budget", "forecast"])),
            RuleCondition::SizeBetween(100, 10),
        ],
        actions: vec![RuleAction::MoveTo(FolderId(7)), RuleAction::StopProcessingMoreRules],
        exceptions: vec![RuleCondition::MyNameInCc, RuleCondition::MarkedImportance(Importance::Low)],
        ..Default::default()
    };
    let names = |id: FolderId| if id == FolderId(7) { "Projects".to_string() } else { "?".to_string() };
    assert_eq!(
        rule.describe(&names),
        [
            "Apply this rule after the message arrives",
            "from alice@example.com; bob@example.com",
            "and with budget or forecast in the subject",
            "and with a size between 10 KB and 100 KB",
            "move it to the Projects folder",
            "and stop processing more rules",
            "except where my name is in the Cc box",
            "or except if marked as low importance",
        ]
    );
    assert!(!rule.is_client_only());
    assert_eq!(rule.list_label(), "");
}

fn all_conditions() -> Vec<RuleCondition> {
    vec![
        RuleCondition::From(vec![]),
        RuleCondition::SubjectContains(vec![]),
        RuleCondition::BodyContains(vec![]),
        RuleCondition::SubjectOrBodyContains(vec![]),
        RuleCondition::HeaderContains(vec![]),
        RuleCondition::RecipientAddressContains(vec![]),
        RuleCondition::SenderAddressContains(vec![]),
        RuleCondition::SentTo(vec![]),
        RuleCondition::ThroughAccount(String::new()),
        RuleCondition::SentOnlyToMe,
        RuleCondition::MyNameInTo,
        RuleCondition::MyNameInCc,
        RuleCondition::MyNameInToOrCc,
        RuleCondition::MyNameNotInTo,
        RuleCondition::MarkedImportance(Importance::High),
        RuleCondition::MarkedSensitivity(Sensitivity::Private),
        RuleCondition::FlaggedForAction,
        RuleCondition::AssignedToCategory(String::new()),
        RuleCondition::HasAttachment,
        RuleCondition::SizeBetween(1, 2),
        RuleCondition::ReceivedBetween(0, 86_400 * 365),
    ]
}

fn all_actions() -> Vec<RuleAction> {
    vec![
        RuleAction::NewItemAlert(String::new()),
        RuleAction::MoveTo(FolderId(1)),
        RuleAction::CopyTo(FolderId(1)),
        RuleAction::AssignCategory("Red category".into()),
        RuleAction::Delete,
        RuleAction::PermanentlyDelete,
        RuleAction::ForwardTo(vec![]),
        RuleAction::ForwardAsAttachment(vec!["a@b.c".into()]),
        RuleAction::RedirectTo(vec!["a@b.c".into()]),
        RuleAction::ReplyWithTemplate(String::new()),
        RuleAction::FlagForFollowUp(String::new(), 0),
        RuleAction::FlagForFollowUp("Reply".into(), 1),
        RuleAction::FlagForFollowUp("Reply".into(), 3),
        RuleAction::ClearFlag,
        RuleAction::ClearCategories,
        RuleAction::MarkImportance(Importance::High),
        RuleAction::Print,
        RuleAction::PlaySound(String::new()),
        RuleAction::MarkAsRead,
        RuleAction::StopProcessingMoreRules,
        RuleAction::DesktopAlert,
    ]
}

#[test]
fn every_clause_has_text_with_placeholders() {
    let descs: Vec<String> = all_conditions().iter().map(RuleCondition::describe).collect();
    assert!(descs.iter().all(|d| !d.is_empty() && !d.contains("  ")));
    assert_eq!(descs[0], "from people or public group");
    assert_eq!(descs[1], "with specific words in the subject");
    assert_eq!(descs[8], "through the specified account");
    assert_eq!(descs[20], "received between 1/1/1970 and 1/1/1971");
    let names = |_: FolderId| "Inbox".to_string();
    let acts: Vec<String> = all_actions().iter().map(|a| a.describe(&names)).collect();
    assert!(acts.iter().all(|d| !d.is_empty()));
    assert_eq!(acts[1], "move it to the Inbox folder");
    assert_eq!(acts[2], "move a copy to the Inbox folder");
    assert_eq!(acts[7], "forward it to a@b.c as an attachment");
    assert_eq!(acts[10], "flag message for Follow up today");
    assert_eq!(acts[11], "flag message for Reply in 1 day");
    assert_eq!(acts[12], "flag message for Reply in 3 days");
}

proptest! {
    #[test]
    fn matching_never_panics(subject in any::<String>(), body in any::<String>(), word in any::<String>(),
                             size in any::<u64>(), unix in any::<i64>(), a in any::<i64>(), b in any::<i64>()) {
        let item = MailItem {
            body: Body { html: Some(body), ..Default::default() },
            size,
            received: DateTime { unix, offset_min: 0 },
            raw: Some(subject.clone().into_bytes()),
            ..mail(&subject)
        };
        for c in all_conditions().into_iter().chain([
            RuleCondition::SubjectOrBodyContains(vec![word.clone()]),
            RuleCondition::HeaderContains(vec![word.clone()]),
            RuleCondition::ReceivedBetween(a, b),
            RuleCondition::SizeBetween(a.unsigned_abs(), b.unsigned_abs()),
        ]) {
            let _ = c.matches(&item, &ctx());
            let _ = c.describe();
        }
    }
}

#[test]
fn header_contains_stops_at_first_blank_line() {
    // LF-only source whose body happens to contain a CRLF blank line: the body is not header text.
    let item = MailItem { raw: Some(b"Subject: hi\nX-A: 1\n\nbody-word\r\n\r\nmore".to_vec()), ..mail("hi") };
    let r =
        |w: &str| Rule { conditions: vec![RuleCondition::HeaderContains(words(&[w]))], ..Default::default() };
    assert!(r("x-a: 1").matches(&item, &ctx()));
    assert!(!r("body-word").matches(&item, &ctx()));
}

#[test]
fn exception_clauses_read_naturally() {
    let rule = Rule {
        exceptions: vec![
            RuleCondition::HasAttachment,
            RuleCondition::MyNameNotInTo,
            RuleCondition::SubjectContains(words(&["lunch"])),
        ],
        ..Default::default()
    };
    let lines = rule.describe(&|_| String::new());
    assert_eq!(
        lines[1..],
        [
            "except if it has an attachment",
            "or except where my name is not in the To box",
            "or except if with lunch in the subject",
        ]
    );
}
