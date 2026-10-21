//! Proofing for the compose editor (layer L3): spelling (Review › Spelling and Grammar, F7, red squiggles)
//! and a few simple grammar checks (blue squiggles).
//!
//! - [`Speller::bundled_en_us`]: US English from the public-domain Moby word lists
//!   (`assets/dictionaries/`, about 131,000 words plus 33,000 proper nouns).
//! - [`Speller::check`]: one word, with case rules (Capitalised and ALL-CAPS forms of a word are accepted,
//!   proper nouns only capitalised), apostrophes (contractions, possessives, curly quotes), hyphenated parts,
//!   and the options to skip words in capitals, words with digits, and Internet/file addresses
//!   ([`SpellOptions`], which also switch repeated words, the grammar checks and user-dictionary suggestions).
//! - [`Speller::suggest`]: ranked replacements within two edits (optimal-string-alignment distance), weighted
//!   so keyboard slips, doubled letters and swapped letters come first, then by word frequency. A deletion
//!   index is built on first use (about 1.3 million entries, ~10 MB, well under a second).
//! - User dictionary ([`Speller::add_word`], [`Speller::user_words`], [`Speller::set_user_words`]) and the
//!   session ignore list ([`Speller::ignore`]); both are in memory and the engine persists them.
//! - [`tokenize`]: `(start, end, word)` byte ranges on Unicode word boundaries (UAX #29), with addresses kept
//!   whole.
//! - [`Speller::check_text`]: every [`Issue`] in a text: spelling, repeated words ("the the"), lower-case
//!   sentence starts, double spaces.
//!
//! Nothing here panics on any input: all offsets are byte offsets on char boundaries of the given text, word
//! lengths are capped at [`MAX_WORD_CHARS`] and issue counts at [`MAX_ISSUES`].

mod dict;
mod distance;
mod grammar;
mod speller;
mod tokenize;

pub use distance::keys_adjacent;
pub use grammar::{Issue, IssueKind, MAX_ISSUES};
pub use speller::{MAX_USER_WORDS, MAX_WORD_CHARS, SpellOptions, Speller};
pub use tokenize::{Token, looks_like_address, tokenize};
