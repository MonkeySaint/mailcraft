//! Conversation threading (Conversation view, `plan/reference/reply-forward.md` › Conversation view).
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
pub fn norm_id(id: &str) -> &str {
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

/// Conversation Settings of a folder view (View › Messages › Conversation Settings). Defaults: messages
/// from other folders and senders above the subject on, the other two off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConversationSettings {
    /// Show Messages from Other Folders (e.g. your replies in Sent Items) inside the conversation.
    pub from_other_folders: bool,
    /// Show Senders Above the Subject on the conversation header.
    pub senders_above_subject: bool,
    /// Always Expand Selected Conversation (the selected conversation cannot be collapsed).
    pub always_expand: bool,
    /// Use Classic Indented View (children oldest first, indented by reply depth).
    pub classic_indent: bool,
}

impl Default for ConversationSettings {
    fn default() -> Self {
        ConversationSettings {
            from_other_folders: true,
            senders_above_subject: true,
            always_expand: false,
            classic_indent: false,
        }
    }
}

impl ConversationSettings {
    /// Setting names as the `view.conversationSettings` command takes them.
    pub const NAMES: [&'static str; 4] =
        ["fromOtherFolders", "sendersAboveSubject", "alwaysExpand", "classicIndent"];

    /// Canonical name for `name` (case-insensitive), if it is a setting.
    pub fn canonical(name: &str) -> Option<&'static str> {
        Self::NAMES.iter().copied().find(|n| n.eq_ignore_ascii_case(name.trim()))
    }

    /// Value of the setting `name` (any case).
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match Self::canonical(name)? {
            "fromOtherFolders" => self.from_other_folders,
            "sendersAboveSubject" => self.senders_above_subject,
            "alwaysExpand" => self.always_expand,
            _ => self.classic_indent,
        })
    }

    /// Sets the setting `name` (any case); false when the name is unknown.
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        let slot = match Self::canonical(name) {
            Some("fromOtherFolders") => &mut self.from_other_folders,
            Some("sendersAboveSubject") => &mut self.senders_above_subject,
            Some("alwaysExpand") => &mut self.always_expand,
            Some(_) => &mut self.classic_indent,
            None => return false,
        };
        *slot = on;
        true
    }
}

/// Deepest indent the reply tree reports (deeper replies stay at this level).
pub const MAX_REPLY_DEPTH: usize = 32;

/// Classic indented order of one conversation's `items`: a reply sits under the message it answers
/// (In-Reply-To, else the nearest References entry that is a member), siblings and roots oldest first.
/// Returns `(item, depth)` in display order; roots have depth 0. A parent must be older than its reply
/// (ties broken by id), so loops in hostile headers cannot form.
pub fn reply_tree(items: &[&MailItem]) -> Vec<(ItemId, usize)> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<&MailItem> = items.iter().copied().filter(|m| seen.insert(m.id)).collect();
    // Oldest first: the order roots and siblings take.
    items.sort_by(|a, b| a.received.unix.cmp(&b.received.unix).then(a.id.cmp(&b.id)));
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, m) in items.iter().enumerate() {
        if let Some(id) = m.message_id.as_deref().map(norm_id).filter(|s| !s.is_empty()) {
            by_id.entry(id).or_insert(i);
        }
    }
    let n = items.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots: Vec<usize> = Vec::new();
    for (i, m) in items.iter().enumerate() {
        let parent = m
            .in_reply_to
            .iter()
            .rev()
            .chain(m.references.iter().rev())
            .filter_map(|r| by_id.get(norm_id(r)).copied())
            // Sorted oldest first, so an older parent has a smaller index.
            .find(|&j| j < i);
        match parent.and_then(|j| children.get_mut(j)) {
            Some(list) => list.push(i),
            None => roots.push(i),
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|r| (r, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let Some(m) = items.get(i) else { continue };
        out.push((m.id, depth.min(MAX_REPLY_DEPTH)));
        if let Some(kids) = children.get(i) {
            let next = depth.saturating_add(1);
            stack.extend(kids.iter().rev().map(|&k| (k, next)));
        }
    }
    out
}

#[cfg(test)]
mod conversation_view_tests {
    use super::*;
    use crate::DateTime;

    fn msg(id: u64, subject: &str, mid: &str, parent: &[&str], t: i64) -> MailItem {
        MailItem {
            id: ItemId(id),
            subject: subject.into(),
            message_id: (!mid.is_empty()).then(|| mid.to_string()),
            in_reply_to: parent.last().map(|p| vec![p.to_string()]).unwrap_or_default(),
            references: parent.iter().map(|p| p.to_string()).collect(),
            received: DateTime { unix: t, offset_min: 0 },
            ..Default::default()
        }
    }

    #[test]
    fn reply_tree_indents_by_reply_depth() {
        let a = msg(1, "Plan", "a@x", &[], 100);
        let b = msg(2, "RE: Plan", "b@x", &["a@x"], 200);
        let c = msg(3, "RE: Plan", "c@x", &["a@x", "b@x"], 300);
        let d = msg(4, "FW: Plan", "d@x", &["a@x"], 250);
        let lone = msg(5, "FW: Plan", "", &[], 400);
        let tree = reply_tree(&[&c, &lone, &b, &d, &a]);
        let ids: Vec<(u64, usize)> = tree.iter().map(|(i, d)| (i.0, *d)).collect();
        assert_eq!(ids, vec![(1, 0), (2, 1), (3, 2), (4, 1), (5, 0)]);
    }

    #[test]
    fn reply_tree_survives_loops_and_duplicates() {
        // Each claims to answer the other: the older one becomes the root.
        let a = msg(1, "x", "a@x", &["b@x"], 100);
        let b = msg(2, "x", "b@x", &["a@x"], 200);
        let tree = reply_tree(&[&a, &b, &a]);
        assert_eq!(tree, vec![(ItemId(1), 0), (ItemId(2), 1)]);
        assert!(reply_tree(&[]).is_empty());
    }

    #[test]
    fn forward_without_references_joins_original() {
        let a = msg(1, "Budget", "a@x", &[], 100);
        let fw = msg(2, "FW: budget", "", &[], 200);
        let convs = thread(&[&a, &fw]);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs.first().map(|c| c.topic.as_str()), Some("Budget"));
    }

    #[test]
    fn settings_by_name() {
        let mut s = ConversationSettings::default();
        assert_eq!(s.get("FromOtherFolders"), Some(true));
        assert_eq!(s.get("classicIndent"), Some(false));
        assert!(s.set("alwaysexpand", true));
        assert!(s.always_expand);
        assert!(!s.set("nope", true));
        assert_eq!(s.get("nope"), None);
        assert_eq!(ConversationSettings::canonical(" CLASSICINDENT "), Some("classicIndent"));
    }
}
