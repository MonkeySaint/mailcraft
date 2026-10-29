//! Undo/redo. Mutating commands snapshot the parts of the mailbox they touch.
//!
//! The history is capped twice: by steps ([`DEFAULT_LIMIT`]) and by the estimated memory of the snapshots it
//! holds ([`DEFAULT_MAX_BYTES`], issue #25), whichever comes first. The newest step is always kept, so Undo
//! works even for a mailbox larger than the byte cap.

use mailcraft_model::Mailbox;

/// Default undo depth.
pub const DEFAULT_LIMIT: usize = 100;

/// Default cap on the estimated memory of the undo (or redo) history. Each step holds a whole mailbox for
/// now, so a large mailbox keeps fewer steps; once steps hold only what a command changed (storage stage S3)
/// this cap can come down.
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024 * 1024;

/// One undoable step: label + full mailbox snapshot before the command (v0; diff-based later).
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    pub before: Box<Mailbox>,
    /// Estimated memory of `before` ([`Mailbox::approx_bytes`]), worked out once when the step is made.
    pub bytes: usize,
}

impl UndoEntry {
    /// A step restoring `before`, with its size estimated.
    pub fn new(label: impl Into<String>, before: Box<Mailbox>) -> UndoEntry {
        let bytes = before.approx_bytes();
        UndoEntry { label: label.into(), before, bytes }
    }
}

#[derive(Clone, Debug)]
pub struct UndoStack {
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// Maximum depth (oldest dropped).
    pub limit: usize,
    /// Most estimated bytes each history holds (oldest dropped, the newest step always kept). 0 = default.
    pub max_bytes: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: DEFAULT_LIMIT, max_bytes: DEFAULT_MAX_BYTES }
    }
}

impl UndoStack {
    /// Effective limit (0 is treated as the default, never as "keep nothing").
    fn cap(&self) -> usize {
        if self.limit == 0 { DEFAULT_LIMIT } else { self.limit }
    }

    /// Effective byte cap (0 is treated as the default).
    fn byte_cap(&self) -> usize {
        if self.max_bytes == 0 { DEFAULT_MAX_BYTES } else { self.max_bytes }
    }

    fn trim(list: &mut Vec<UndoEntry>, cap: usize, byte_cap: usize) {
        if list.len() > cap {
            let excess = list.len().saturating_sub(cap);
            list.drain(..excess);
        }
        // Newest first: keep steps while they fit, always at least the newest one.
        let mut total = 0usize;
        let mut keep = 0usize;
        for e in list.iter().rev() {
            total = total.saturating_add(e.bytes);
            if keep > 0 && total > byte_cap {
                break;
            }
            keep = keep.saturating_add(1);
        }
        let excess = list.len().saturating_sub(keep);
        if excess > 0 {
            list.drain(..excess);
        }
    }

    /// Estimated bytes held by the undo and redo histories.
    pub fn bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(|e| e.bytes).fold(0usize, usize::saturating_add)
    }

    /// Records a new step (a fresh command): clears redo, drops the oldest beyond the limits.
    pub fn push(&mut self, entry: UndoEntry) {
        self.redo.clear();
        self.push_keep_redo(entry);
    }

    /// Records a step without touching redo (used by redo).
    pub fn push_keep_redo(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
        let (cap, bytes) = (self.cap(), self.byte_cap());
        Self::trim(&mut self.undo, cap, bytes);
    }

    /// Records a redo step (used by undo).
    pub fn push_redo(&mut self, entry: UndoEntry) {
        self.redo.push(entry);
        let (cap, bytes) = (self.cap(), self.byte_cap());
        Self::trim(&mut self.redo, cap, bytes);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step Undo would revert ("Delete", "Move to Folder", …).
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Label of the step Redo would reapply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: usize) -> UndoEntry {
        UndoEntry::new(format!("step {n}"), Box::default())
    }

    fn sized(n: usize, bytes: usize) -> UndoEntry {
        UndoEntry { bytes, ..entry(n) }
    }

    #[test]
    fn limit_drops_oldest() {
        let mut u = UndoStack::default();
        for i in 0..150 {
            u.push(entry(i));
        }
        assert_eq!(u.undo.len(), DEFAULT_LIMIT);
        assert_eq!(u.undo.first().map(|e| e.label.as_str()), Some("step 50"));
        assert_eq!(u.undo_label(), Some("step 149"));
    }

    #[test]
    fn push_clears_redo_but_redo_push_keeps_it() {
        let mut u = UndoStack { limit: 0, ..Default::default() };
        u.push_redo(entry(1));
        u.push_keep_redo(entry(2));
        assert!(u.can_redo());
        u.push(entry(3));
        assert!(!u.can_redo());
        assert!(u.can_undo());
    }

    /// Issue #25: the history is capped by bytes too; the oldest steps go first and the newest stays even
    /// when it alone is over the cap.
    #[test]
    fn byte_cap_drops_oldest_but_keeps_the_newest() {
        let mut u = UndoStack { max_bytes: 1_000, ..Default::default() };
        for i in 0..10 {
            u.push(sized(i, 300));
        }
        assert_eq!(u.undo.len(), 3, "3 × 300 fit in 1000 bytes");
        assert_eq!(u.undo.first().map(|e| e.label.as_str()), Some("step 7"));
        assert!(u.bytes() <= 1_000);
        u.push(sized(10, 5_000));
        assert_eq!(u.undo.len(), 1, "a step over the cap on its own is still kept");
        assert_eq!(u.undo_label(), Some("step 10"));
        u.push_redo(sized(11, 600));
        u.push_redo(sized(12, 600));
        assert_eq!(u.redo.len(), 1, "redo is capped the same way");
        assert_eq!(u.redo_label(), Some("step 12"));
    }

    /// The estimate grows with what the mailbox holds, so a big mailbox keeps fewer steps.
    #[test]
    fn steps_are_sized_from_the_mailbox() {
        let mut mb = Mailbox::default();
        let small = UndoEntry::new("small", Box::new(mb.clone())).bytes;
        for i in 0..20u64 {
            let item = mailcraft_model::MailItem {
                id: mailcraft_model::ItemId(i),
                body: mailcraft_model::Body { text: Some("x".repeat(10_000)), ..Default::default() },
                raw: Some(vec![b'y'; 5_000]),
                ..Default::default()
            };
            mb.mail.insert(item.id, item);
        }
        let big = UndoEntry::new("big", Box::new(mb)).bytes;
        assert!(big >= small + 20 * 15_000, "{small} → {big}");
        let mut u = UndoStack { max_bytes: big * 3, ..Default::default() };
        for _ in 0..10 {
            u.push(UndoEntry { bytes: big, ..entry(0) });
        }
        assert_eq!(u.undo.len(), 3);
    }
}
