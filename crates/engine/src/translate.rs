//! Machine translation (Message › Language › Translate) through an opt-in hook.
//!
//! MailCraft ships no translator and never sends message text anywhere by itself. An embedding app, or a
//! user's own build, installs a [`Translator`] on the session ([`Session::translator`]) — for example a
//! translation engine running on the same computer or a self-hosted service the user set up. Without one,
//! Translate Message opens the Translator window, which says that no translator is installed and that
//! nothing was sent.
//!
//! With a translator, `review.translate` translates into the language asked for (`to`), else Options ›
//! Language › "Translate messages into" (`language.translateTo`). A message being written gets the
//! translation as its body (Show Original swaps back); a received message is not changed: the translation
//! is kept on the session ([`Session::translations`]) and the reading pane and read windows show it in
//! place of the body, with Show Original to switch back and forth.

use std::sync::Arc;

use mailcraft_model::ItemId;

use crate::Session;

/// Longest text handed to a translator at once (characters).
pub const MAX_CHARS: usize = 100_000;
/// Longest translation kept (characters).
pub const MAX_RESULT_CHARS: usize = 400_000;

/// A translation engine. Implementations must not panic; they may take a while (the call blocks the
/// command), so they should give up on their own after a reasonable time.
pub trait Translator: Send + Sync {
    /// Name shown with a translation ("Translated by <name>").
    fn name(&self) -> String;
    /// Translates `text` into the language `to` (a BCP 47 tag such as "fr" or "pt-BR").
    fn translate(&self, text: &str, to: &str) -> Result<String, String>;
}

/// The translation of a received message, shown in place of its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Language it was translated into.
    pub to: String,
    /// The translated text.
    pub text: String,
    /// Who translated it.
    pub by: String,
    /// Show Original: the message's own body shows instead (the translation is kept).
    pub show_original: bool,
}

/// Languages offered by the Translator window: (tag, English name).
pub const LANGUAGES: [(&str, &str); 12] = [
    ("en", "English"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("sv", "Swedish"),
    ("pl", "Polish"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
    ("ko", "Korean"),
];

/// English name of a language tag ("fr" → "French"; the primary subtag decides), if known.
pub fn language_name(tag: &str) -> Option<&'static str> {
    let primary = tag.split(['-', '_']).next().unwrap_or_default();
    LANGUAGES.iter().find(|(t, _)| t.eq_ignore_ascii_case(primary)).map(|(_, n)| *n)
}

/// Installs (or with `None` removes) the session's translator.
pub fn set_translator(s: &mut Session, t: Option<Arc<dyn Translator>>) {
    s.translator = t;
}

/// The translation shown for message `id`, if any (not when Show Original is on).
pub fn shown(s: &Session, id: ItemId) -> Option<&Translation> {
    s.translations.get(&id).filter(|t| !t.show_original)
}

/// Runs the installed translator on `text` (capped at [`MAX_CHARS`]); `None` without a translator.
pub(crate) fn run(s: &Session, text: &str, to: &str) -> Option<Result<(String, String), String>> {
    let t = s.translator.clone()?;
    let clipped: String = text.chars().take(MAX_CHARS).collect();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.translate(&clipped, to)))
        .unwrap_or_else(|_| Err("the translator failed".to_owned()));
    Some(result.map(|out| (out.chars().take(MAX_RESULT_CHARS).collect(), t.name())))
}
