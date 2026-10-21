# Dictionaries

Word lists for the spell checker (`crates/proof`, `mailcraft-proof`). They are plain UTF-8 text, one word per
line, sorted, `\n` line ends, and are embedded in the binary with `include_str!`.

| File | Contents | Source | Status |
|---|---|---|---|
| `en-US.txt` | 130,891 lower-case English words (letters, at most one inner apostrophe) | Moby Words II: `crosswd.txt`, `crswd-d.txt`, the lower-case entries of `common.txt`, `freq.txt`; plus `en-US-supplement.txt` | Public domain |
| `en-US-proper.txt` | 33,507 Capitalised proper nouns (given names, surnames, US places, other proper nouns), minus any whose lower-case form is in `en-US.txt` | Moby Words II: `names.txt`, `places.txt`, the Capitalised entries of `common.txt`; plus `en-US-supplement.txt` | Public domain |
| `en-US-freq.txt` | 1,202 frequent words, most frequent first, used only to rank suggestions | Moby Words II: `freq-int.txt`, then `freq.txt` | Public domain |
| `en-thesaurus.txt` | 23,641 English head words, each with its ten closest related words (`word\trelated,related,…`), for Review › Thesaurus | Moby Thesaurus II: `mthesaur.txt` (Project Gutenberg ebook #3202) | Public domain |
| `en-US-supplement.txt` | Everyday words the 1990s lists lack (`email`, `website`, `login`…) and some proper nouns (Canadian places…) | Written for MailCraft | MIT OR Apache-2.0 |

## Source and licence

The Moby Words II lists were compiled by Grady Ward (Moby Project) and placed in the **public domain** by
grant from the author (January 2001). They are distributed by Project Gutenberg as ebook #3201,
<https://www.gutenberg.org/ebooks/3201> (files at <https://www.gutenberg.org/files/3201/files/>); Project
Gutenberg's licence file for the ebook states it is "confirmed to be in the PUBLIC DOMAIN IN THE UNITED
STATES". The derived lists here are only filtered, merged and sorted; no other text from the ebook is
included.

## Rebuilding

Download `crosswd.txt`, `crswd-d.txt`, `common.txt`, `names.txt`, `places.txt`, `freq.txt` and
`freq-int.txt` from the address above into an empty folder, then run:

```sh
cargo run -p mailcraft-proof --example build_dictionary -- <that folder> assets/dictionaries
```

The builder keeps single words only: all-lower-case entries go to `en-US.txt`, Capitalised ones to
`en-US-proper.txt`, and everything else (phrases, hyphenated words, acronyms, non-ASCII names) is dropped.
It also merges `en-US-supplement.txt`. Edit the
supplement, not the generated files, to add words.

The thesaurus comes from the Moby Thesaurus II, by the same author and also placed in the public domain
(<https://www.gutenberg.org/ebooks/3202>). Download `mthesaur.txt` from
<https://www.gutenberg.org/files/3202/files/> into an empty folder and run:

```sh
cargo run -p mailcraft-proof --example build_thesaurus -- <that folder>/mthesaur.txt assets/dictionaries
```

It keeps head words that are single words of `en-US.txt` and, for each, the ten related words closest to it
(single words first, then by how many related words the two lists share, weighted by how common the word
is).
