//! MailCraft instant search (layer L3).
//!
//! - [`query`]: the search-box query language (free text, `from:`, `subject:`, `hasattachments:yes`,
//!   `received:"last week"`, `size:>1MB`, AND/OR/NOT, parentheses, quoted phrases…) parsed into a
//!   [`Query`] tree by [`parse`]; `Display` writes it back as canonical text.
//! - [`fold`] / [`token`]: Unicode case and diacritic folding and word tokens (prefix matching works on
//!   tokens: `bud` finds "Budget").
//! - [`Doc`]: what is indexed for an item (mail, appointment, contact, task, note).
//! - [`Index`]: the inverted index, updated one item at a time; [`Index::search`] returns ranked [`Hit`]s
//!   (field weights × term rarity, plus recency), [`Index::count`] total/unread counts (search folders).
//! - [`matches`]: the same query evaluated directly against one [`Doc`] (no index).
//! - [`highlights`]: match ranges for the list and reading pane.
//!
//! Nothing here panics on any input: the parser caps input, nesting and term counts; the index caps tokens
//! per field of an item and items per index.

pub mod doc;
pub mod eval;
pub mod fold;
pub mod highlight;
pub mod index;
pub mod query;
pub mod token;

pub use doc::{Doc, DocKind, Level, Person};
pub use eval::{Context, matches};
pub use fold::fold;
pub use highlight::{Field, highlights};
pub use index::{Counts, Filter, Hit, Index, Stats};
pub use query::{Query, Term, TextField, parse};
