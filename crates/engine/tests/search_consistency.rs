//! The session's search index stays in step with the mailbox: random command sequences (with real ids, undo
//! and redo, and direct edits followed by `invalidate`), then every query answered by the index is compared
//! with a brute-force scan that evaluates the query directly against every item.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;

use mailcraft_engine::Session;
use mailcraft_engine::sample::sample_session;
use mailcraft_engine::search_index::{self, context, search_folder_filter};
use mailcraft_model::{FolderKind, ItemId};
use mailcraft_search::{Doc, Filter, matches, parse};
use serde_json::{Map, Value, json};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n.max(1) as u64)) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        if v.is_empty() { None } else { v.get(self.below(v.len())) }
    }
}

const VOCAB: [&str; 11] = [
    "budget",
    "lunch",
    "review",
    "plan",
    "café",
    "report",
    "alpha",
    "gamma",
    "Budget review",
    "q3 numbers",
    "zeta",
];

const QUERIES: [&str; 31] = [
    "budget",
    "bud",
    "\"budget review\"",
    "lunch OR plan",
    "-budget",
    "from:alice",
    "from:me",
    "to:me",
    "tocc:me",
    "participants:example.com",
    "isread:no",
    "isread:yes",
    "isflagged:yes",
    "category:any",
    "category:none",
    "category:red",
    "hasattachments:yes",
    "importance:high",
    "importance:low",
    "received:\"this week\"",
    "received:<\"30 days ago\"",
    "kind:task",
    "kind:meeting",
    "kind:contact",
    "kind:note",
    "folder:inbox",
    "subject:re",
    "body:the",
    "size:tiny",
    "zeta OR alpha",
    "gamma",
];

/// Commands left out: they read or write files, talk to the network or open a browser.
fn excluded(id: &str) -> bool {
    ["file.", "print.", "sendReceive.", "help.", "options."].iter().any(|p| id.starts_with(p))
        || matches!(
            id,
            "mail.print" | "mail.quickPrint" | "compose.attachFile" | "rules.import" | "insert.picture"
        )
}

fn item_ids(s: &Session) -> Vec<u64> {
    let mb = &s.mailbox;
    mb.mail
        .keys()
        .chain(mb.appointments.keys())
        .chain(mb.contacts.keys())
        .chain(mb.tasks.keys())
        .chain(mb.notes.keys())
        .map(|i| i.0)
        .collect()
}

fn all_docs(s: &Session) -> Vec<Doc> {
    let mb = &s.mailbox;
    let mut out: Vec<Doc> = mb.mail.values().map(Doc::from_mail).collect();
    out.extend(mb.appointments.values().map(Doc::from_appointment));
    out.extend(mb.contacts.values().map(Doc::from_contact));
    out.extend(mb.tasks.values().map(Doc::from_task));
    out.extend(mb.notes.values().map(Doc::from_note));
    out
}

fn random_value(r: &mut Rng, key: &str, s: &Session) -> Value {
    let ids = item_ids(s);
    let folders: Vec<u64> = s.mailbox.folders.keys().map(|f| f.0).collect();
    let word = || VOCAB.to_vec();
    match key {
        "ids" | "items" => {
            let n = 1 + r.below(3);
            Value::Array((0..n).filter_map(|_| r.pick(&ids).copied()).map(Value::from).collect())
        }
        "id" | "item" => r.pick(&ids).copied().map_or(Value::Null, Value::from),
        "folder" | "parent" | "target" => r.pick(&folders).copied().map_or(Value::Null, Value::from),
        "on" | "enabled" | "read" | "all" | "commit" | "includeDeleted" => json!(r.chance(50)),
        "category" | "name" => {
            let names = ["Red category", "Blue category", "Green category", "Project X"];
            json!(r.pick(&names).copied().unwrap_or("Red category"))
        }
        "importance" | "level" => json!(r.pick(&["high", "low", "normal"]).copied().unwrap_or("high")),
        "template" => json!(r.pick(&["unread", "flagged", "words", "categorized", "large", "toMe"]).copied()),
        "kind" => json!(r.pick(&["unread", "flagged", "categorized", "from", "junk", "email"]).copied()),
        "query" => json!(r.pick(&QUERIES).copied().unwrap_or("budget")),
        "flag" | "due" => json!(r.pick(&["today", "tomorrow", "thisWeek", "nextWeek", "none"]).copied()),
        _ => json!(r.pick(&word()).copied().unwrap_or("budget")),
    }
}

const KEYS: [&str; 21] = [
    "ids",
    "id",
    "folder",
    "parent",
    "target",
    "on",
    "read",
    "category",
    "name",
    "importance",
    "subject",
    "text",
    "body",
    "template",
    "value",
    "kind",
    "query",
    "flag",
    "to",
    "location",
    "includeDeleted",
];

fn random_params(r: &mut Rng, s: &Session) -> Value {
    let mut obj = Map::new();
    let n = r.below(5);
    for _ in 0..n {
        let k = *r.pick(&KEYS).unwrap_or(&"ids");
        obj.insert(k.to_owned(), random_value(r, k, s));
    }
    Value::Object(obj)
}

fn randomize_selection(r: &mut Rng, s: &mut Session) {
    let folders: Vec<_> = s.mailbox.folders.keys().copied().collect();
    if let Some(f) = r.pick(&folders).copied()
        && r.chance(30)
    {
        s.selection.folder = f;
    }
    // Mostly mail (most commands act on selected mail), sometimes any item.
    let ids: Vec<u64> = if r.chance(70) { s.mailbox.mail.keys().map(|i| i.0).collect() } else { item_ids(s) };
    let n = r.below(3);
    s.selection.items = (0..n).filter_map(|_| r.pick(&ids).copied()).map(ItemId).collect();
    s.selection.focus = s.selection.items.first().copied();
}

/// A direct mailbox edit (bypassing the journal), as importers and tests do, followed by `invalidate`.
fn direct_edit(r: &mut Rng, s: &mut Session) {
    let ids: Vec<ItemId> = s.mailbox.mail.keys().copied().collect();
    let Some(id) = r.pick(&ids).copied() else { return };
    let w = *r.pick(&VOCAB).unwrap_or(&"zeta");
    if let Some(m) = s.mailbox.mail.get_mut(&id) {
        match r.below(4) {
            0 => m.subject = format!("{} {w}", m.subject),
            1 => m.read = !m.read,
            2 => m.categories = vec!["Red category".into()],
            _ => {
                // Same length, past the preview: only a fingerprint of the whole body notices.
                let filler = "x ".repeat(200);
                let tail = if m.body.text.as_deref().is_some_and(|t| t.ends_with("alpha")) {
                    "gamma"
                } else {
                    "alpha"
                };
                m.body.text = Some(format!("{filler}{tail}"));
            }
        }
    }
    s.search.invalidate();
}

fn sorted(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v
}

fn check(s: &Session, extra: &[String], step: &str) {
    let ctx = context(s);
    let docs = all_docs(s);
    let mut queries: Vec<String> = QUERIES.iter().map(|q| (*q).to_owned()).collect();
    queries.extend(extra.iter().cloned());
    for q in &queries {
        let parsed = parse(q);
        let want = sorted(docs.iter().filter(|d| matches(&parsed, d, &ctx)).map(|d| d.id).collect());
        let got = sorted(search_index::hits(s, q, &Filter::default()).into_iter().map(|h| h.id).collect());
        assert_eq!(got, want, "after {step}: query {q:?} (index vs brute force)");
        let c = search_index::count(s, q, &Filter::default());
        assert_eq!(c.total, want.len(), "after {step}: count of {q:?}");
        let unread = docs.iter().filter(|d| !d.read && matches(&parsed, d, &ctx)).count();
        assert_eq!(c.unread, unread, "after {step}: unread count of {q:?}");
    }
    // Search folders: live counts and items over their account's mail.
    for f in s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Search) {
        let filter = search_folder_filter(s, f.id);
        let folders = filter.folders.clone().unwrap_or_default();
        let q = parse(f.search_query.as_deref().unwrap_or(""));
        let want: BTreeSet<u64> = s
            .mailbox
            .mail
            .values()
            .filter(|m| folders.contains(&m.folder.0))
            .map(Doc::from_mail)
            .filter(|d| matches(&q, d, &ctx))
            .map(|d| d.id)
            .collect();
        let got: BTreeSet<u64> =
            search_index::search_folder_items(s, f.id).into_iter().map(|i| i.0).collect();
        assert_eq!(got, want, "after {step}: search folder {:?} ({:?})", f.name, f.search_query);
        assert_eq!(search_index::search_folder_counts(s, f.id).total, want.len());
    }
}

/// Words of the current items' subjects, so queries also hit text that commands just wrote.
fn live_words(r: &mut Rng, s: &Session) -> Vec<String> {
    let subjects: Vec<String> = s.mailbox.mail.values().map(|m| m.subject.clone()).collect();
    let mut out = Vec::new();
    for _ in 0..3 {
        if let Some(sub) = r.pick(&subjects) {
            let words: Vec<&str> = sub.split_whitespace().collect();
            if let Some(w) = r.pick(&words) {
                out.push(format!("\"{}\"", w.replace('"', "")));
            }
        }
    }
    out
}

fn run_seed(seed: u64, steps: usize) {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut s = sample_session();
    let mut ids: Vec<&'static str> =
        s.registry.specs.iter().map(|c| c.id).filter(|id| !excluded(id)).collect();
    ids.sort_unstable();
    // Commands that change items, picked more often than the rest.
    let mut changing: Vec<&'static str> = s
        .registry
        .specs
        .iter()
        .filter(|c| c.mutates && !excluded(c.id))
        .filter(|c| {
            ["mail.", "calendar.", "people.", "tasks.", "notes.", "folder.", "categories.", "quicksteps."]
                .iter()
                .any(|p| c.id.starts_with(p))
        })
        .map(|c| c.id)
        .collect();
    changing.sort_unstable();
    // A few search folders so their live counts are checked too.
    for t in ["unread", "flagged", "categorized"] {
        s.run("folder.newSearchFolder", &json!({ "template": t })).unwrap();
    }
    s.run("folder.newSearchFolder", &json!({ "template": "words", "value": "budget" })).unwrap();
    check(&s, &[], "setup");
    for step in 0..steps {
        let what = match r.below(10) {
            0 => {
                let _ = s.run("app.undo", &json!({}));
                "app.undo".to_owned()
            }
            1 => {
                let _ = s.run("app.redo", &json!({}));
                "app.redo".to_owned()
            }
            2 if r.chance(30) => {
                direct_edit(&mut r, &mut s);
                "direct edit".to_owned()
            }
            _ => {
                randomize_selection(&mut r, &mut s);
                let id = if r.chance(70) { *r.pick(&changing).unwrap() } else { *r.pick(&ids).unwrap() };
                let p = random_params(&mut r, &s);
                let _ = s.run(id, &p);
                format!("{id} {p}")
            }
        };
        // Drop compose windows and notices so they do not pile up.
        s.notices.clear();
        let extra = live_words(&mut r, &s);
        check(&s, &extra, &format!("seed {seed} step {step}: {what}"));
    }
}

#[test]
fn index_matches_brute_force_after_random_commands() {
    for seed in 1..=8 {
        run_seed(seed, 200);
    }
}

#[test]
fn every_command_keeps_the_index_in_step() {
    let ids: Vec<&'static str> = {
        let s = sample_session();
        let mut v: Vec<&'static str> =
            s.registry.specs.iter().map(|c| c.id).filter(|id| !excluded(id)).collect();
        v.sort_unstable();
        v
    };
    for (n, id) in ids.iter().enumerate() {
        let mut s = sample_session();
        let mut r = Rng(n as u64 + 17);
        s.run("folder.newSearchFolder", &json!({ "template": "unread" })).unwrap();
        // Warm the index so a missed change shows up as a stale answer.
        check(&s, &[], "setup");
        let mail: Vec<u64> = s.mailbox.mail.keys().map(|i| i.0).collect();
        let first = mail.first().copied().unwrap_or(0);
        let all_params = [
            json!({}),
            json!({ "ids": mail.iter().take(3).collect::<Vec<_>>() }),
            json!({ "id": first, "text": "zeta", "subject": "zeta", "name": "zeta", "value": "zeta" }),
            json!({ "id": first, "category": "Red category", "on": true, "read": true }),
        ];
        for p in &all_params {
            randomize_selection(&mut r, &mut s);
            let _ = s.run(id, p);
            s.notices.clear();
            check(&s, &["zeta".to_owned()], &format!("{id} {p}"));
        }
    }
}
