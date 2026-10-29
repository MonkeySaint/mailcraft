//! Insert Hyperlink (web page, place in this message, e-mail address, ScreenTip) and bookmarks that travel
//! with the message as named anchors.

use serde_json::json;

use super::{mailto_url, percent_decode};
use crate::Session;
use crate::cmd::compose::tests::{open, session};
use crate::cmd::insert::tests::last_dialog;
use crate::compose::ComposeState;
use mailcraft_model::{Block, Inline};

fn setup(text: &str) -> (Session, u64) {
    let mut s = session();
    let id = open(&mut s);
    s.run("compose.insertText", &json!({"text": text})).unwrap();
    (s, id)
}

fn select(s: &mut Session, a: (&[usize], usize), b: (&[usize], usize)) {
    s.run(
        "compose.select",
        &json!({"anchor": {"path": a.0, "offset": a.1}, "focus": {"path": b.0, "offset": b.1}}),
    )
    .unwrap();
}

/// (href, title, text) of every link in the body.
fn links(c: &ComposeState) -> Vec<(String, Option<String>, String)> {
    let mut out = Vec::new();
    for b in &c.body.blocks {
        if let Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } = b {
            for i in inlines {
                if let Inline::Link { href, title, children } = i {
                    let text: String = children
                        .iter()
                        .filter_map(|c| match c {
                            Inline::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    out.push((href.clone(), title.clone(), text));
                }
            }
        }
    }
    out
}

fn draft_html(s: &mut Session, id: u64) -> String {
    let draft = crate::cmd::compose::save_draft(s, id).unwrap();
    s.mailbox.mail.get(draft).unwrap().body.html.clone().unwrap()
}

#[test]
fn screen_tips_go_on_the_link_and_into_the_message() {
    let (mut s, id) = setup("Read ");
    let r = s
        .run(
            "insert.link",
            &json!({"url": "https://example.com/plan", "text": "the plan", "screenTip": "Our plan"}),
        )
        .unwrap();
    assert_eq!(r["screenTip"], "Our plan");
    assert_eq!(
        links(&s.composes[&id]),
        vec![("https://example.com/plan".into(), Some("Our plan".into()), "the plan".into())]
    );
    // Typing inside the link keeps its ScreenTip; editing the link without one keeps it too.
    select(&mut s, (&[0], 7), (&[0], 7));
    s.run("compose.insertText", &json!({"text": "X"})).unwrap();
    assert_eq!(links(&s.composes[&id])[0].1.as_deref(), Some("Our plan"));
    assert!(draft_html(&mut s, id).contains("title=\"Our plan\""));
    // An empty ScreenTip removes it.
    select(&mut s, (&[0], 5), (&[0], 14));
    s.run("insert.link", &json!({"url": "https://example.com/plan", "screenTip": ""})).unwrap();
    assert_eq!(links(&s.composes[&id])[0].1, None);
    let long = "x".repeat(300);
    assert!(s.run("insert.link", &json!({"url": "https://example.com/", "screenTip": long})).is_err());
}

#[test]
fn e_mail_address_links_carry_the_subject() {
    let (mut s, id) = setup("");
    s.run("insert.link", &json!({"email": "team@example.com", "subject": "Plan & dates?", "text": "write"}))
        .unwrap();
    assert_eq!(links(&s.composes[&id])[0].0, "mailto:team@example.com?subject=Plan%20%26%20dates%3F");
    assert!(s.run("insert.link", &json!({"email": "not an address"})).is_err());
    assert_eq!(mailto_url("mailto:a@example.org", ""), Some("mailto:a@example.org".into()));
    assert_eq!(percent_decode("a%20b%zz%"), "a b%zz%");
    // The dialog shows the address and subject of the link at the caret.
    select(&mut s, (&[0], 2), (&[0], 2));
    s.run("insert.link", &json!({})).unwrap();
    let r = last_dialog(&s);
    assert_eq!((r["mode"].as_str(), r["email"].as_str()), (Some("email"), Some("team@example.com")));
    assert_eq!(r["subject"], "Plan & dates?");
}

#[test]
fn place_in_this_message_links_to_bookmarks_headings_and_the_top() {
    let (mut s, id) = setup("Intro\nDetails here\nMore");
    s.run("format.style", &json!({"name": "Heading 1"})).unwrap();
    select(&mut s, (&[1], 0), (&[1], 0));
    s.run("insert.bookmark", &json!({"name": "Details"})).unwrap();
    // The dialog lists the bookmarks and headings.
    assert_eq!(s.run("insert.link", &json!({})).unwrap()["dialog"], "insertLink");
    let r = last_dialog(&s);
    assert_eq!(r["bookmarks"], json!(["Details"]));
    assert_eq!(r["headings"][0]["text"], "More");
    select(&mut s, (&[0], 0), (&[0], 5));
    s.run("insert.link", &json!({"bookmark": "Details"})).unwrap();
    assert_eq!(links(&s.composes[&id])[0].0, "#Details");
    assert!(s.run("insert.link", &json!({"bookmark": "Nope"})).is_err());
    // A heading gets a hidden bookmark, not listed in the Bookmark dialog.
    s.run("insert.link", &json!({"heading": 1, "text": "see More"})).unwrap();
    let c = &s.composes[&id];
    assert!(c.bookmarks.iter().any(|b| b.name == "_More" && b.pos.path == vec![2]));
    assert!(links(c).iter().any(|(h, _, _)| h == "#_More"));
    s.run("insert.bookmark", &json!({})).unwrap();
    assert_eq!(last_dialog(&s)["bookmarks"], json!(["Details"]));
    // Linking to the same heading again reuses its bookmark.
    s.run("insert.link", &json!({"heading": 1, "text": "again"})).unwrap();
    assert_eq!(s.composes[&id].bookmarks.iter().filter(|b| b.name.starts_with("_More")).count(), 1);
    s.run("insert.link", &json!({"top": true, "text": "top"})).unwrap();
    assert!(s.composes[&id].bookmarks.iter().any(|b| b.name == "_top"));
    assert!(s.run("insert.link", &json!({"heading": 0})).is_err());
    assert!(s.run("insert.link", &json!({"heading": 9})).is_err());
}

#[test]
fn bookmarks_travel_as_anchors_and_come_back_from_a_draft() {
    let (mut s, id) = setup("First\nSecond part");
    select(&mut s, (&[1], 7), (&[1], 7));
    s.run("insert.bookmark", &json!({"name": "Part"})).unwrap();
    select(&mut s, (&[0], 0), (&[0], 5));
    s.run("insert.link", &json!({"bookmark": "Part"})).unwrap();
    let html = draft_html(&mut s, id);
    assert!(html.contains(r#"<a id="Part" name="Part"></a>"#), "{html}");
    assert!(html.contains("href=\"#Part\""), "{html}");
    // The anchor sits where the bookmark is (after "Second ").
    assert!(html.contains(r#"Second <a id="Part""#), "{html}");
    // The compose body itself has no anchors in it (bookmarks are kept beside it).
    assert!(!format!("{:?}", s.composes[&id].body).contains("Anchor"));
    // Opening the message again turns the anchors back into bookmarks.
    let doc = mailcraft_html::html_to_doc(&html);
    let cid = s.open_compose(ComposeState { body: doc, ..Default::default() });
    let c = &s.composes[&cid];
    assert_eq!(c.bookmarks.len(), 1);
    assert_eq!(
        (c.bookmarks[0].name.as_str(), c.bookmarks[0].pos.path.clone(), c.bookmarks[0].pos.offset),
        ("Part", vec![1], 7)
    );
    assert!(!format!("{:?}", c.body).contains("Anchor"));
    assert_eq!(
        s.run("format.goTo", &json!({"compose": cid, "target": "bookmark:Part"})).unwrap()["caret"]["offset"],
        7
    );
}

#[test]
fn junk_link_params_never_panic() {
    let (mut s, _) = setup("x");
    for p in [
        json!({"heading": -1}),
        json!({"heading": "one"}),
        json!({"bookmark": 5}),
        json!({"email": "", "subject": "\u{0}"}),
        json!({"email": "a@b\u{1}c"}),
        json!({"url": "https://example.com/", "screenTip": 7}),
        json!({"top": "yes"}),
        json!({"path": ""}),
    ] {
        let _ = s.run("insert.link", &p);
    }
}

#[test]
fn bookmarks_move_with_edits_above_them_and_with_sorting() {
    // Regression: bookmarks stayed at their old paragraph number and offset, so a paragraph typed above a
    // bookmarked heading put the anchor (and every `#link` to it) on the wrong paragraph.
    let (mut s, id) = setup("Intro\nPlan\nBudget");
    select(&mut s, (&[1], 0), (&[1], 0));
    s.run("format.style", &json!({"name": "Heading 1"})).unwrap();
    select(&mut s, (&[0], 5), (&[0], 5));
    s.run("insert.link", &json!({"heading": 1, "text": " see"})).unwrap();
    select(&mut s, (&[2], 3), (&[2], 3));
    s.run("insert.bookmark", &json!({"name": "Money"})).unwrap();
    // A new paragraph above the heading, typed at the start of the first paragraph.
    select(&mut s, (&[0], 0), (&[0], 0));
    s.run("compose.insertText", &json!({"text": "Hello all\n"})).unwrap();
    let at = |s: &Session, n: &str| {
        s.composes[&id]
            .current_bookmarks()
            .into_iter()
            .find(|b| b.name == n)
            .map(|b| (b.pos.path, b.pos.offset))
    };
    assert_eq!(at(&s, "_Plan"), Some((vec![2], 0)));
    assert_eq!(at(&s, "Money"), Some((vec![3], 3)));
    let html = draft_html(&mut s, id);
    assert!(html.contains(r#"<a id="_Plan" name="_Plan"></a>Plan"#), "{html}");
    assert!(html.contains(r#"Bud<a id="Money" name="Money"></a>get"#), "{html}");
    // Typing inside the bookmarked paragraph, before the bookmark, moves it along.
    select(&mut s, (&[3], 0), (&[3], 0));
    s.run("compose.insertText", &json!({"text": "The "})).unwrap();
    assert_eq!(s.run("format.goTo", &json!({"target": "Money"})).unwrap()["caret"]["offset"], 7);
    // Undo puts it back with the text.
    s.run("compose.editUndo", &json!({})).unwrap();
    assert_eq!(s.run("format.goTo", &json!({"target": "Money"})).unwrap()["caret"]["offset"], 3);
    // Sorting moves each bookmark with its paragraph.
    let (mut s, id) = setup("pear\napple\nfig");
    select(&mut s, (&[0], 2), (&[0], 2));
    s.run("insert.bookmark", &json!({"name": "Pear"})).unwrap();
    select(&mut s, (&[0], 0), (&[2], 3));
    s.run("format.sort", &json!({"order": "ascending"})).unwrap();
    let html = draft_html(&mut s, id);
    assert!(html.contains(r#"pe<a id="Pear" name="Pear"></a>ar"#), "{html}");
    let pear = s.composes[&id].current_bookmarks().into_iter().find(|b| b.name == "Pear").map(|b| b.pos);
    assert_eq!(pear.map(|p| (p.path, p.offset)), Some((vec![2], 2)));
}

#[test]
fn replies_leave_quoted_anchors_in_the_body() {
    // Regression: a reply or forward turned every `<a name>`/`<a id>` of the quoted original into one of the
    // user's bookmarks, and dropped the ones past the cap from the message.
    let html = r##"<p><a name="top"></a>Newsletter <a href="#sec2">jump</a></p><p><a id="sec2"></a>Two</p>"##;
    let doc = mailcraft_html::html_to_doc(html);
    let mut s = session();
    let cid = s.open_compose(ComposeState {
        kind: crate::compose::ComposeKind::Reply,
        body: doc,
        ..Default::default()
    });
    let c = &s.composes[&cid];
    assert!(c.bookmarks.is_empty());
    assert!(format!("{:?}", c.body).contains("Anchor"));
    let out = crate::cmd::compose::save_draft(&mut s, cid).unwrap();
    let sent = s.mailbox.mail.get(out).unwrap().body.html.clone().unwrap();
    assert!(sent.contains(r#"name="sec2""#) || sent.contains(r#"id="sec2""#), "{sent}");
    // A draft of the user's own adopts them, but those past the cap stay in the body.
    let mut many = String::new();
    for k in 0..(crate::compose::MAX_BOOKMARKS + 5) {
        many.push_str(&format!("<p><a name=\"b{k}\"></a>p{k}</p>"));
    }
    let doc = mailcraft_html::html_to_doc(&many);
    let cid = s.open_compose(ComposeState {
        kind: crate::compose::ComposeKind::Draft,
        body: doc,
        ..Default::default()
    });
    let c = &s.composes[&cid];
    assert_eq!(c.bookmarks.len(), crate::compose::MAX_BOOKMARKS);
    assert_eq!(format!("{:?}", c.body).matches("Anchor").count(), 5);
    let out = crate::cmd::compose::save_draft(&mut s, cid).unwrap();
    let sent = s.mailbox.mail.get(out).unwrap().body.html.clone().unwrap();
    let last = crate::compose::MAX_BOOKMARKS + 4;
    assert!(sent.contains(&format!("name=\"b{last}\"")), "an anchor past the cap was dropped");
}

/// Release check: the Bookmark dialog had no list to pick from, no sort and no hidden bookmarks; the
/// command now gives it every bookmark with its place, and lists them sorted on request.
#[test]
fn bookmarks_list_by_name_or_location_with_hidden_ones_on_request() {
    let (mut s, _id) = setup("First line\nSecond line\nThird line");
    select(&mut s, (&[2], 0), (&[2], 0));
    s.run("insert.bookmark", &json!({"name": "apple"})).unwrap();
    select(&mut s, (&[0], 3), (&[0], 3));
    s.run("insert.bookmark", &json!({"name": "Zebra"})).unwrap();
    select(&mut s, (&[1], 0), (&[1], 0));
    s.run("insert.bookmark", &json!({"name": "mango"})).unwrap();
    s.run("insert.link", &json!({"top": true, "text": "up"})).unwrap();
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["bookmarks"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap().to_owned()).collect()
    };
    // By name (ignoring case), hidden ones left out.
    let r = s.run("insert.bookmark", &json!({"action": "list"})).unwrap();
    assert_eq!(names(&r), ["apple", "mango", "Zebra"]);
    // By place in the message, top first.
    let r = s.run("insert.bookmark", &json!({"action": "list", "sort": "location"})).unwrap();
    assert_eq!(names(&r), ["Zebra", "mango", "apple"]);
    // Hidden bookmarks too: the one the top link made, at the very start.
    let r = s.run("insert.bookmark", &json!({"action": "list", "sort": "location", "hidden": true})).unwrap();
    assert_eq!(names(&r), ["_top", "Zebra", "mango", "apple"]);
    assert_eq!(r["bookmarks"][0]["hidden"], true);
    assert_eq!(r["bookmarks"][1]["location"], 2);
    assert!(s.run("insert.bookmark", &json!({"action": "list", "sort": "size"})).is_err());
    // The dialog gets them all with their places, plus the visible names.
    assert_eq!(s.run("insert.bookmark", &json!({})).unwrap()["dialog"], "bookmark");
    let d = last_dialog(&s);
    assert_eq!(d["entries"].as_array().unwrap().len(), 4);
    assert_eq!(d["bookmarks"].as_array().unwrap().len(), 3);
    // A hidden bookmark can be gone to and deleted by name, but not added.
    s.run("insert.bookmark", &json!({"name": "_top", "action": "goTo"})).unwrap();
    assert!(s.run("insert.bookmark", &json!({"name": "_mine"})).is_err());
    assert_eq!(
        s.run("insert.bookmark", &json!({"name": "_top", "action": "delete"})).unwrap()["deleted"],
        true
    );
}
