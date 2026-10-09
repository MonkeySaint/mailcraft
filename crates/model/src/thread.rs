//! Conversation threading (Conversation view, `plan/outlook/reply-forward.md` › Conversation view).
//!
//! Messages join a conversation when they are linked through Message-ID / In-Reply-To / References (the
//! container idea from the classic threading algorithm: any two messages that mention the same id, even a
//! missing parent, belong together), and conversations whose normalised topics are equal are merged, so a
//! forward ("FW: …") without references still joins its original. Conversations sort by their newest message.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::subject::{conversation_topic, strip_prefixes};
use crate::{DateTime, ItemId, MailItem};

/// One conversation: header row data plus its messages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Subject without reply/forward prefixes, from the oldest message (header row text).
    pub topic: String,
    /// Member messages, newest first.
    pub items: Vec<ItemId>,
    /// Time of the newest message (the conversation sorts by this).
    pub latest: DateTime,
    /// Stable key for `MailItem::conversation` (FNV-1a of the normalised topic, or of the root message id
    /// when the topic is empty).
    #[serde(default)]
    pub key: u64,
    /// Number of unread members.
    #[serde(default)]
    pub unread: usize,
}

impl Conversation {
    /// Newest member.
    pub fn newest(&self) -> Option<ItemId> {
        self.items.first().copied()
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 64-bit FNV-1a (stable across runs and platforms, unlike `DefaultHasher`).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Conversation key for a subject (prefix-stripped, case-folded topic hash). Empty topics give 0.
pub fn topic_key(subject: &str) -> u64 {
    let topic = conversation_topic(subject);
    if topic.is_empty() { 0 } else { fnv1a(topic.as_bytes()) }
}

/// Union-find over item indices.
struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu { parent: (0..n).collect() }
    }
    fn find(&mut self, mut x: usize) -> usize {
        // Iterative with path halving; indices always come from 0..n so `get` never fails in practice.
        while let Some(&p) = self.parent.get(x) {
            if p == x {
                break;
            }
            let gp = self.parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = self.parent.get_mut(x) {
                *slot = gp;
            }
            x = gp;
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        // Keep the smaller index as root so results do not depend on union order.
        let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
        if let Some(slot) = self.parent.get_mut(child) {
            *slot = root;
        }
    }
}

/// Message ids are compared without brackets and surrounding whitespace (case kept: the local part is
/// case-sensitive).
fn norm_id(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>').trim()
}

/// Groups `items` into conversations, newest conversation first; each conversation lists its items newest
/// first. Duplicate item ids are kept once.
pub fn thread(items: &[&MailItem]) -> Vec<Conversation> {
    // Drop duplicate ids (same item passed twice).
    let mut seen = std::collections::HashSet::new();
    let items: Vec<&MailItem> = items.iter().copied().filter(|m| seen.insert(m.id)).collect();
    let n = items.len();
    let mut dsu = Dsu::new(n);

    // Link through ids: every id an item mentions (its own, its parents, its ancestors).
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let mentioned = item
            .message_id
            .iter()
            .chain(item.in_reply_to.iter())
            .chain(item.references.iter())
            .map(|s| norm_id(s))
            .filter(|s| !s.is_empty());
        for id in mentioned {
            match by_id.get(id) {
                Some(&j) => dsu.union(i, j),
                None => {
                    by_id.insert(id, i);
                }
            }
        }
    }

    // Fall back to the normalised subject.
    let topics: Vec<String> = items.iter().map(|m| conversation_topic(&m.subject)).collect();
    let mut by_topic: HashMap<&str, usize> = HashMap::new();
    for (i, topic) in topics.iter().enumerate() {
        if topic.is_empty() {
            continue;
        }
        match by_topic.get(topic.as_str()) {
            Some(&j) => dsu.union(i, j),
            None => {
                by_topic.insert(topic.as_str(), i);
            }
        }
    }

    // Collect members per root.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = dsu.find(i);
        groups.entry(r).or_default().push(i);
    }

    let mut out: Vec<Conversation> = groups
        .into_values()
        .filter_map(|members| {
            let mut members: Vec<&MailItem> = members.iter().filter_map(|&i| items.get(i).copied()).collect();
            // Newest first; ties broken by id (higher id = added later = newer).
            members.sort_by(|a, b| b.received.unix.cmp(&a.received.unix).then(b.id.cmp(&a.id)));
            let newest = members.first()?;
            let oldest = members.last()?;
            let topic = strip_prefixes(&oldest.subject).to_string();
            let norm = conversation_topic(&oldest.subject);
            let key = if !norm.is_empty() {
                fnv1a(norm.as_bytes())
            } else if let Some(id) = members.iter().rev().find_map(|m| m.message_id.as_deref()) {
                fnv1a(norm_id(id).as_bytes())
            } else {
                fnv1a(&oldest.id.0.to_le_bytes())
            };
            Some(Conversation {
                topic,
                latest: newest.received,
                unread: members.iter().filter(|m| !m.read).count(),
                items: members.iter().map(|m| m.id).collect(),
                key,
            })
        })
        .collect();
    out.sort_by(|a, b| b.latest.unix.cmp(&a.latest.unix).then_with(|| b.items.first().cmp(&a.items.first())));
    out
}
