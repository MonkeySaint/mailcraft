//! Undo/redo. Mutating commands snapshot the parts of the mailbox they touch.

use mailcraft_model::Mailbox;

/// Default undo depth.
pub const DEFAULT_LIMIT: usize = 100;

/// One undoable step: label + full mailbox snapshot before the command (v0; diff-based later).
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    pub before: Box<Mailbox>,
}

#[derive(Clone, Debug)]
pub struct UndoStack {
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// Maximum depth (oldest dropped).
    pub limit: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: DEFAULT_LIMIT }
    }
}

impl UndoStack {
    /// Effective limit (0 is treated as the default, never as "keep nothing").
    fn cap(&self) -> usize {
        if self.limit == 0 { DEFAULT_LIMIT } else { self.limit }
    }

    fn trim(list: &mut Vec<UndoEntry>, cap: usize) {
        if list.len() > cap {
            let excess = list.len().saturating_sub(cap);
            list.drain(..excess);
        }
    }

    /// Records a new step (a fresh command): clears redo, drops the oldest beyond the limit.
    pub fn push(&mut self, entry: UndoEntry) {
        self.redo.clear();
        self.push_keep_redo(entry);
    }

    /// Records a step without touching redo (used by redo).
    pub fn push_keep_redo(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
        let cap = self.cap();
        Self::trim(&mut self.undo, cap);
    }

    /// Records a redo step (used by undo).
    pub fn push_redo(&mut self, entry: UndoEntry) {
        self.redo.push(entry);
        let cap = self.cap();
        Self::trim(&mut self.redo, cap);
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
        UndoEntry { label: format!("step {n}"), before: Box::default() }
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
}
