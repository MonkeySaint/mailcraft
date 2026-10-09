//! Batched journaling with periodic checkpoints.

use mailcraft_model::Mailbox;

use crate::{Op, Result, Store};

/// Collects ops, writes them to the journal in batches (one fsync per batch) and folds the journal into a
/// fresh snapshot every [`Autosave::checkpoint_every`] ops or once the journal reaches
/// [`Autosave::checkpoint_bytes`].
///
/// Typical use: `push` after each engine command, `commit` once per UI frame / idle tick, `finish` on exit.
#[derive(Clone, Debug)]
pub struct Autosave {
    /// Journaled ops between checkpoints (0 = never checkpoint automatically).
    pub checkpoint_every: usize,
    /// Journal size in bytes that also triggers a checkpoint (0 = no size trigger), so a run of large ops
    /// (`Op::Replace` carries the whole mailbox) cannot grow the journal past what the store reads back.
    pub checkpoint_bytes: u64,
    buffer: Vec<Op>,
    since_checkpoint: usize,
    checkpoints: u64,
}

impl Default for Autosave {
    fn default() -> Self {
        Autosave::new(Autosave::DEFAULT_CHECKPOINT_EVERY)
    }
}

impl Autosave {
    pub const DEFAULT_CHECKPOINT_EVERY: usize = 500;
    pub const DEFAULT_CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;

    pub fn new(checkpoint_every: usize) -> Self {
        Autosave {
            checkpoint_every,
            checkpoint_bytes: Autosave::DEFAULT_CHECKPOINT_BYTES,
            buffer: Vec::new(),
            since_checkpoint: 0,
            checkpoints: 0,
        }
    }

    /// Queues an op (nothing is written until [`Autosave::flush`] / [`Autosave::commit`]).
    pub fn push(&mut self, op: Op) {
        self.buffer.push(op);
    }

    /// Ops queued but not yet journaled.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Ops journaled since the last checkpoint.
    pub fn since_checkpoint(&self) -> usize {
        self.since_checkpoint
    }

    /// Checkpoints written by this helper.
    pub fn checkpoints(&self) -> u64 {
        self.checkpoints
    }

    /// Writes queued ops to the journal. On error the queue is kept so the caller can retry.
    pub fn flush(&mut self, store: &mut Store) -> Result<usize> {
        if self.buffer.is_empty() {
            return Ok(0);
        }
        store.append(&self.buffer)?;
        let n = self.buffer.len();
        self.buffer.clear();
        self.since_checkpoint = self.since_checkpoint.saturating_add(n);
        Ok(n)
    }

    /// Flushes, then checkpoints `mb` when the threshold is reached. `mb` must be the state after every
    /// pushed op. Returns whether a checkpoint was written.
    ///
    /// Also keeps the store lock fresh, so call it on idle ticks too. When the journal refuses the batch, a
    /// checkpoint of `mb` is written instead (returns `true`).
    pub fn commit(&mut self, store: &mut Store, mb: &Mailbox) -> Result<bool> {
        store.keep_alive();
        if let Err(e) = self.flush(store) {
            // The journal could not take the batch (for one, it would pass the store's size cap). `mb` already
            // holds every queued op, so a snapshot saves the same state; report the original error if that
            // fails too.
            log::warn!("autosave: journal append failed ({e}); writing a checkpoint instead");
            self.checkpoint(store, mb).map_err(|_| e)?;
            self.buffer.clear();
            return Ok(true);
        }
        let by_count = self.checkpoint_every > 0 && self.since_checkpoint >= self.checkpoint_every;
        let by_size = self.checkpoint_bytes > 0 && store.journal_bytes() >= self.checkpoint_bytes;
        if by_count || by_size {
            self.checkpoint(store, mb)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Queues `ops` and commits in one call.
    pub fn record(&mut self, store: &mut Store, mb: &Mailbox, ops: &[Op]) -> Result<bool> {
        self.buffer.extend_from_slice(ops);
        self.commit(store, mb)
    }

    /// Flushes and checkpoints unconditionally (shutdown path).
    pub fn finish(&mut self, store: &mut Store, mb: &Mailbox) -> Result<()> {
        self.flush(store)?;
        self.checkpoint(store, mb)
    }

    fn checkpoint(&mut self, store: &mut Store, mb: &Mailbox) -> Result<()> {
        store.checkpoint(mb)?;
        self.since_checkpoint = 0;
        self.checkpoints = self.checkpoints.saturating_add(1);
        Ok(())
    }
}
