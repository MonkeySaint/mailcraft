//! `MemRepo`: the in-memory [`crate::MailRepo`] (storage ADR §3.1), for the browser build, the sample
//! mailbox and tests.
//!
//! It is the [`MailTable`] a [`mailcraft_model::Mailbox`] already holds, which implements the trait with the
//! behaviour every caller relies on (`repo.rs`). The SQLite repository ([`crate::SqlRepo`], native builds)
//! answers the same: `tests/repo_contract.rs` runs one suite against both.

pub use mailcraft_model::MailTable;

/// The in-memory mail repository.
pub type MemRepo = MailTable;
