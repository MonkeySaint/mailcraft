//! Bench-like test: index a synthetic mailbox and time queries. Release builds index 50,000 messages
//! (budget: 10 s to index, 50 ms per query); debug builds use 5,000 with looser limits.
//!
//! `cargo test --release -p mailcraft-search --test perf -- --nocapture` prints the timings.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_search::{Context, Doc, Filter, Index, Person, parse};

const NOW: i64 = 1_791_547_200;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn vocabulary(rng: &mut Rng) -> Vec<String> {
    const SYL: [&str; 24] = [
        "ka", "lo", "mi", "ne", "ro", "ta", "vu", "be", "co", "di", "fa", "gu", "ha", "je", "ki", "lu", "ma",
        "no", "pe", "qui", "ra", "se", "to", "zé",
    ];
    (0..8_000)
        .map(|_| {
            let n = 1 + rng.below(4);
            (0..n).map(|_| SYL[rng.below(SYL.len() as u64) as usize]).collect::<String>()
        })
        .collect()
}

fn words(rng: &mut Rng, vocab: &[String], n: u64) -> String {
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push(' ');
        }
        // Zipf-ish: small ids far more common.
        let r = rng.below(vocab.len() as u64);
        let idx = (r * rng.below(vocab.len() as u64)) / vocab.len() as u64;
        out.push_str(&vocab[idx as usize]);
    }
    out
}

fn synthetic(n: usize) -> Vec<Doc> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let vocab = vocabulary(&mut rng);
    let people: Vec<Person> = (0..500)
        .map(|i| {
            Person::new(format!("{} {}", vocab[i * 3], vocab[i * 3 + 1]), format!("user{i}@example.com"))
        })
        .collect();
    (0..n)
        .map(|i| {
            let len = 60 + rng.below(240);
            let subject_len = 3 + rng.below(6);
            Doc {
                id: i as u64 + 1,
                folder: rng.below(20),
                subject: words(&mut rng, &vocab, subject_len),
                from: vec![people[rng.below(500) as usize].clone()],
                to: vec![people[rng.below(500) as usize].clone(), people[rng.below(500) as usize].clone()],
                body: words(&mut rng, &vocab, len),
                attachments: if rng.below(5) == 0 {
                    vec![format!("{}.pdf", vocab[rng.below(100) as usize])]
                } else {
                    vec![]
                },
                categories: if rng.below(4) == 0 { vec!["Red category".into()] } else { vec![] },
                received: NOW - rng.below(400 * 86_400) as i64,
                size: 2_000 + rng.below(2_000_000),
                read: rng.below(3) != 0,
                flagged: rng.below(10) == 0,
                has_attachments: rng.below(5) == 0,
                ..Doc::default()
            }
        })
        .collect()
}

#[test]
fn index_and_query_budgets() {
    let release = !cfg!(debug_assertions);
    let n = if release { 50_000 } else { 5_000 };
    let (index_budget, query_budget) = if release {
        (Duration::from_secs(10), Duration::from_millis(50))
    } else {
        (Duration::from_secs(30), Duration::from_millis(500))
    };
    let docs = synthetic(n);
    let t = Instant::now();
    let mut ix = Index::new();
    for d in &docs {
        ix.upsert(d);
    }
    let took = t.elapsed();
    let stats = ix.stats();
    println!("indexed {n} items in {took:?}: {stats:?}");
    assert!(took < index_budget, "indexing {n} took {took:?}");
    assert_eq!(ix.len(), n);

    let ctx = Context { now: NOW, me: vec!["user7@example.com".into()], ..Context::default() };
    let filter = Filter::default();
    let mut worst = Duration::ZERO;
    for q in [
        "ka",
        "kalo",
        "\"kalo mi\"",
        "k",
        "from:user1 subject:ka",
        "ka OR lo OR mi",
        "ka -lo",
        "hasattachments:yes isread:no received:\"this month\"",
        "size:large category:any",
        "(kalo OR mine) AND NOT isflagged:yes",
        "to:me",
        "zzzzzz",
        "",
    ] {
        let parsed = parse(q);
        let t = Instant::now();
        let hits = ix.search(&parsed, &ctx, &filter);
        let took = t.elapsed();
        println!("{q:40} {:6} hits in {took:?}", hits.len());
        worst = worst.max(took);
    }
    assert!(worst < query_budget, "slowest query took {worst:?}");

    // Incremental updates stay cheap.
    let t = Instant::now();
    for d in docs.iter().take(500) {
        let mut d = d.clone();
        d.read = !d.read;
        ix.upsert(&d);
    }
    for d in docs.iter().skip(500).take(500) {
        ix.remove(d.id);
    }
    let took = t.elapsed();
    println!("500 updates + 500 removals in {took:?}");
    assert!(took < index_budget / 10);
    assert_eq!(ix.len(), n - 500);
}
