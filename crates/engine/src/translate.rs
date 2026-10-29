//! Machine translation (Message › Language › Translate) through an opt-in translator.
//!
//! MailCraft never sends message text anywhere by itself. A translator is either one an embedding app
//! installed on the session ([`Session::translator`]) or the translation server the user entered in Options ›
//! Language › Translation ([`URL_SETTING`], with an optional API key kept in the secret store under
//! [`KEY_SECRET`]): a LibreTranslate-compatible server (`mailcraft_net::translate`), for example one running
//! on the same computer. Nothing is configured by default; without a translator Translate Message opens the
//! Translator window, which says that no translator is set up and that nothing was sent.
//!
//! With a translator, `review.translate` translates into the language asked for (`to`), else Options ›
//! Language › "Translate messages into" (`language.translateTo`). A message being written gets the
//! translation as its body (Show Original swaps back); a received message is not changed: the translation
//! is kept on the session ([`Session::translations`]) and the reading pane and read windows show it in
//! place of the body, with Show Original to switch back and forth.
//!
//! **Messages in other languages.** [`detect`] guesses a message's language on this computer (common words
//! of the languages in [`LANGUAGES`], writing systems for Japanese, Korean and Chinese). With a translator
//! and "Choose how to handle messages received in other languages" set to Ask, the reading pane offers to
//! translate a message whose language differs from "Translate messages into" ([`offer`]); set to Always,
//! the message is translated when it is first shown ([`auto_translate`], once per message and session).

use std::sync::{Arc, mpsc};

use mailcraft_model::ItemId;
use serde_json::Value;

use crate::Session;

/// Setting holding the translation server's address (empty = none).
pub const URL_SETTING: &str = "language.translatorUrl";
/// Secret-store key of the translation server's API key. The value is bound to the server it was entered
/// for (`{"server": <address>, "key": <key>}`, see [`store_key`]): it is only ever sent to that server, so
/// a path that changes or clears the address without removing the key (Reset All, an unreadable
/// settings.json) cannot hand it to the next server entered.
pub const KEY_SECRET: &str = "translator/api-key";

/// A translation server address as the key is bound to it (surrounding spaces and trailing slashes do not
/// count).
pub(crate) fn server_id(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// The stored API key with the server it belongs to, if one is stored in the bound form.
fn bound_key(s: &Session) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(&s.secrets.get(KEY_SECRET)?).ok()?;
    let server = v.get("server")?.as_str()?.to_owned();
    let key = v.get("key")?.as_str()?.to_owned();
    (!key.is_empty()).then_some((server, key))
}

/// The stored API key, when it was entered for the server at `url` (else `None`: it is never sent to
/// another one).
pub(crate) fn key_for(s: &Session, url: &str) -> Option<String> {
    let url = server_id(url);
    bound_key(s).filter(|(server, _)| !url.is_empty() && server == url).map(|(_, k)| k)
}

/// True when an API key is stored for the translation server in Options › Language.
pub fn has_key(s: &Session) -> bool {
    key_for(s, &s.settings.text(URL_SETTING)).is_some()
}

/// True when a key is stored that does not belong to the server at `url` (an older server's, or one in an
/// older unbound form).
pub(crate) fn has_foreign_key(s: &Session, url: &str) -> bool {
    s.secrets.get(KEY_SECRET).is_some() && key_for(s, url).is_none()
}

/// Stores `key` as the API key of the server at `url`.
pub(crate) fn store_key(s: &mut Session, url: &str, key: &str) -> Result<(), String> {
    let v = serde_json::json!({"server": server_id(url), "key": key});
    s.secrets.set(KEY_SECRET, &v.to_string())
}

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

/// The translator in use: the one installed on the session, else the server entered in Options (`None`
/// without either).
pub fn translator(s: &Session) -> Option<Arc<dyn Translator>> {
    s.translator.clone().or_else(|| configured(s))
}

/// True when Translate Message has a translator to use.
pub fn available(s: &Session) -> bool {
    s.translator.is_some()
        || (!cfg!(target_arch = "wasm32") && !s.settings.text(URL_SETTING).trim().is_empty())
}

/// The translation server entered in Options › Language, as a translator.
#[cfg(not(target_arch = "wasm32"))]
fn configured(s: &Session) -> Option<Arc<dyn Translator>> {
    let url = s.settings.text(URL_SETTING);
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let key = key_for(s, url);
    let options = s
        .translator_options
        .clone()
        .unwrap_or_else(mailcraft_net::translate::LibreTranslate::default_options);
    match mailcraft_net::translate::LibreTranslate::new(url, key.as_deref(), options) {
        Ok(lt) => Some(Arc::new(ServerTranslator(lt))),
        Err(e) => Some(Arc::new(Broken(e.to_string()))),
    }
}

#[cfg(target_arch = "wasm32")]
fn configured(_s: &Session) -> Option<Arc<dyn Translator>> {
    None
}

/// A LibreTranslate-compatible server as a [`Translator`].
#[cfg(not(target_arch = "wasm32"))]
struct ServerTranslator(mailcraft_net::translate::LibreTranslate);

#[cfg(not(target_arch = "wasm32"))]
impl Translator for ServerTranslator {
    fn name(&self) -> String {
        format!("the translation server {}", self.0.server().trim_end_matches('/'))
    }
    fn translate(&self, text: &str, to: &str) -> Result<String, String> {
        self.0.translate(text, to).map(|(t, _)| t).map_err(|e| e.to_string())
    }
}

/// A translation server entered with an address that cannot be used: every translation fails with why.
#[cfg(not(target_arch = "wasm32"))]
struct Broken(String);

#[cfg(not(target_arch = "wasm32"))]
impl Translator for Broken {
    fn name(&self) -> String {
        "the translation server".into()
    }
    fn translate(&self, _text: &str, _to: &str) -> Result<String, String> {
        Err(self.0.clone())
    }
}

/// Runs the translator on `text` (capped at [`MAX_CHARS`]); `None` without a translator.
pub(crate) fn run(s: &Session, text: &str, to: &str) -> Option<Result<(String, String), String>> {
    let t = translator(s)?;
    let clipped: String = text.chars().take(MAX_CHARS).collect();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.translate(&clipped, to)))
        .unwrap_or_else(|_| Err("the translator failed".to_owned()));
    Some(result.map(|out| (out.chars().take(MAX_RESULT_CHARS).collect(), t.name())))
}

// ---------------------------------------------------------------------------------------------------------
// Language detection

/// Common words of each language [`detect`] tells apart by words (lower case).
const WORDS: [(&str, &[&str]); 9] = [
    (
        "en",
        &[
            "the", "and", "is", "are", "you", "that", "with", "for", "this", "have", "will", "your", "not",
            "of", "to", "we", "be", "on", "it", "please",
        ],
    ),
    (
        "fr",
        &[
            "le", "la", "les", "et", "est", "vous", "nous", "une", "des", "pour", "avec", "dans", "que",
            "pas", "sur", "je", "ce", "merci", "du", "au",
        ],
    ),
    (
        "es",
        &[
            "el", "los", "las", "y", "es", "usted", "una", "para", "con", "que", "por", "del", "como",
            "pero", "gracias", "muy", "hola", "nosotros", "se", "su",
        ],
    ),
    (
        "de",
        &[
            "der", "die", "das", "und", "ist", "sie", "wir", "ein", "eine", "mit", "nicht", "auf", "für",
            "ich", "zu", "den", "dem", "bitte", "danke", "auch",
        ],
    ),
    (
        "it",
        &[
            "il", "lo", "gli", "e", "è", "che", "per", "con", "non", "una", "sono", "della", "del", "grazie",
            "ciao", "anche", "come", "questo", "ma", "siamo",
        ],
    ),
    (
        "pt",
        &[
            "o", "os", "as", "e", "é", "você", "uma", "para", "com", "que", "não", "do", "da", "obrigado",
            "obrigada", "mas", "como", "isso", "nós", "em",
        ],
    ),
    (
        "nl",
        &[
            "de", "het", "een", "en", "is", "niet", "met", "voor", "van", "ik", "je", "u", "wij", "dat",
            "op", "zijn", "bedankt", "ook", "maar", "graag",
        ],
    ),
    (
        "sv",
        &[
            "och", "är", "att", "det", "som", "en", "ett", "för", "med", "inte", "jag", "vi", "du", "på",
            "av", "tack", "hej", "till", "men", "har",
        ],
    ),
    (
        "pl",
        &[
            "i",
            "jest",
            "nie",
            "się",
            "na",
            "że",
            "to",
            "w",
            "z",
            "do",
            "dla",
            "jak",
            "ale",
            "dziękuję",
            "proszę",
            "czy",
            "jestem",
            "są",
            "tak",
            "mnie",
        ],
    ),
];

/// Most characters of a message [`detect`] looks at.
const DETECT_CHARS: usize = 4_000;

/// The language `text` is most likely written in (a tag of [`LANGUAGES`]), or `None` when it cannot tell
/// (too little text, or no clear winner). Runs on this computer; nothing is sent anywhere.
pub fn detect(text: &str) -> Option<&'static str> {
    let sample: String = text.chars().take(DETECT_CHARS).collect();
    // Writing systems first: kana means Japanese, Hangul Korean, Han without kana Chinese.
    let (mut kana, mut hangul, mut han, mut letters) = (0usize, 0usize, 0usize, 0usize);
    for c in sample.chars() {
        match u32::from(c) {
            0x3040..=0x30FF => kana += 1,
            0xAC00..=0xD7AF | 0x1100..=0x11FF => hangul += 1,
            0x4E00..=0x9FFF => han += 1,
            _ if c.is_alphabetic() => letters += 1,
            _ => {}
        }
    }
    let cjk = kana + hangul + han;
    if cjk >= 4 && cjk >= letters / 4 {
        return Some(if kana > 0 && kana * 10 >= cjk {
            "ja"
        } else if hangul >= han {
            "ko"
        } else {
            "zh"
        });
    }
    let lower = sample.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.len() < 4 {
        return None;
    }
    let mut scores: Vec<(usize, &'static str)> =
        WORDS.iter().map(|(tag, list)| (words.iter().filter(|w| list.contains(w)).count(), *tag)).collect();
    scores.sort_by_key(|x| std::cmp::Reverse(x.0));
    let (best, tag) = scores.first().copied()?;
    let second = scores.get(1).map_or(0, |x| x.0);
    // At least three common words, and clearly ahead of the next language.
    (best >= 3 && best.saturating_mul(2) > second.saturating_mul(3)).then_some(tag)
}

/// The primary language subtag, lower case ("pt-BR" → "pt").
fn primary(tag: &str) -> String {
    tag.split(['-', '_']).next().unwrap_or_default().to_ascii_lowercase()
}

/// Most bytes of an HTML-only body read to find the start of its text for [`detect`].
const DETECT_HTML_BYTES: usize = 128 * 1024;

/// The longest prefix of `t` of at most `max` bytes that ends on a character boundary.
fn prefix(t: &str, max: usize) -> &str {
    let mut end = max.min(t.len());
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    t.get(..end).unwrap_or_default()
}

/// The start of a message's text, enough for [`detect`]: the subject and the first [`DETECT_CHARS`]
/// characters of the body. Only a bounded part of the body is read (the offer bar asks every frame), never
/// the whole text or HTML of a large message.
fn detect_sample(m: &mailcraft_model::MailItem, body: &mailcraft_model::Body) -> String {
    let mut out = String::with_capacity(m.subject.len().saturating_add(DETECT_CHARS));
    out.push_str(&m.subject);
    out.push('\n');
    let text: String = body.text.as_deref().unwrap_or_default().chars().take(DETECT_CHARS).collect();
    if !text.trim().is_empty() {
        out.push_str(&text);
    } else if let Some(h) = &body.html {
        let stripped = mailcraft_model::text::strip_html(prefix(h, DETECT_HTML_BYTES));
        out.extend(stripped.chars().take(DETECT_CHARS));
    } else {
        out.push_str(&text);
    }
    out
}

/// What the reading pane offers for a received message: (the language it seems to be in, the language it
/// would be translated into). `None` when Options say Never, no translator is set up, the message is
/// translated already (or being translated), or it seems to be in the "Translate messages into" language
/// (or cannot be told).
pub fn offer(s: &Session, id: ItemId) -> Option<(&'static str, String)> {
    if s.settings.text("language.translate") == "never"
        || !available(s)
        || s.translations.contains_key(&id)
        || pending(s, id)
    {
        return None;
    }
    let m = s.mailbox.mail.get(id)?;
    // Through the body cache: the offer bar asks every frame.
    let from = detect(&detect_sample(m, &s.body_or_empty(m)))?;
    let to = s.settings.text("language.translateTo");
    (primary(from) != primary(&to)).then_some((from, to))
}

/// Options say Always translate: translates a message in another language the first time it is shown in
/// this session (once per message, whether it works or not). Returns true when it was translated now or
/// the translation started in the background ([`Session::background_translate`]).
pub fn auto_translate(s: &mut Session, id: ItemId) -> bool {
    if s.settings.text("language.translate") != "always" || s.translate_tried.contains(&id) {
        return false;
    }
    if offer(s, id).is_none() {
        return false;
    }
    s.translate_tried.insert(id);
    s.run("review.translate", &serde_json::json!({ "id": id.0 })).is_ok_and(|r| {
        let flag = |k: &str| r.get(k).and_then(serde_json::Value::as_bool) == Some(true);
        flag("translated") || flag("translating")
    })
}

// ---------------------------------------------------------------------------------------------------------
// Translating in the background

/// Translations and translation tests running on worker threads ([`Session::background_translate`]).
#[derive(Default)]
pub struct Jobs {
    running: Vec<Job>,
    test: Option<mpsc::Receiver<Result<Value, String>>>,
    test_result: Option<Result<Value, String>>,
}

/// One translation on a worker thread.
struct Job {
    /// `{"item": id}` (a received message) or `{"compose": id}` (a message being written).
    target: Value,
    to: String,
    by: String,
    /// The text of the message being written when the translation started: a draft changed since is not
    /// replaced by the translation.
    compose_text: Option<String>,
    rx: mpsc::Receiver<Result<String, String>>,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs")
            .field("running", &self.running.len())
            .field("test", &self.test.is_some())
            .finish()
    }
}

/// True while message `id` is being translated in the background.
pub fn pending(s: &Session, id: ItemId) -> bool {
    s.translate_jobs.running.iter().any(|j| j.target.get("item").and_then(Value::as_u64) == Some(id.0))
}

/// True while a translation or a translation test is running in the background.
pub fn busy(s: &Session) -> bool {
    !s.translate_jobs.running.is_empty() || s.translate_jobs.test.is_some()
}

/// Runs `work` on a worker thread; its result comes back through the receiver (a panic becomes an error).
fn spawn<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<mpsc::Receiver<Result<T, String>>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("the translator failed".to_owned()));
            let _ = tx.send(r);
        })
        .map_err(|e| format!("the translation could not start: {e}"))?;
    Ok(rx)
}

/// Starts translating `text` into `to` with translator `t` on a worker thread, for `target` (see [`Job`]).
/// The result is applied by [`poll`]. A translation already running for the same target is not started
/// again.
pub(crate) fn start(
    s: &mut Session,
    t: Arc<dyn Translator>,
    target: Value,
    text: &str,
    to: &str,
) -> Result<(), String> {
    if s.translate_jobs.running.iter().any(|j| j.target == target) {
        return Ok(());
    }
    let compose_text = target
        .get("compose")
        .and_then(Value::as_u64)
        .and_then(|cid| s.composes.get(&cid))
        .map(|c| c.body.to_plain_text());
    let clipped: String = text.chars().take(MAX_CHARS).collect();
    let by = t.name();
    let lang = to.to_owned();
    let rx = spawn("translate", move || {
        t.translate(&clipped, &lang).map(|out| out.chars().take(MAX_RESULT_CHARS).collect())
    })?;
    s.translate_jobs.running.push(Job { target, to: to.to_owned(), by, compose_text, rx });
    Ok(())
}

/// Starts the Options translation test on a worker thread; [`take_test_result`] gives its result.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn start_test(
    s: &mut Session,
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<(), String> {
    s.translate_jobs.test_result = None;
    s.translate_jobs.test = Some(spawn("translate-test", work)?);
    Ok(())
}

/// The result of the last translation test run in the background, once (`None` while it runs or when
/// there is none).
pub fn take_test_result(s: &mut Session) -> Option<Result<Value, String>> {
    s.translate_jobs.test_result.take()
}

/// True while the Options translation test runs in the background.
pub fn testing(s: &Session) -> bool {
    s.translate_jobs.test.is_some()
}

/// Applies the translations that finished on worker threads (a failure becomes an error notice). Returns
/// true when something changed. Called from [`Session::poll_sync`].
pub(crate) fn poll(s: &mut Session) -> bool {
    let mut changed = false;
    if let Some(rx) = &s.translate_jobs.test {
        match rx.try_recv() {
            Ok(r) => {
                s.translate_jobs.test_result = Some(r);
                s.translate_jobs.test = None;
                changed = true;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                s.translate_jobs.test_result = Some(Err("the translation test stopped".into()));
                s.translate_jobs.test = None;
                changed = true;
            }
        }
    }
    if s.translate_jobs.running.is_empty() {
        return changed;
    }
    let mut done = Vec::new();
    let mut i = 0;
    while i < s.translate_jobs.running.len() {
        let state = s.translate_jobs.running.get(i).map(|j| j.rx.try_recv());
        match state {
            Some(Err(mpsc::TryRecvError::Empty)) => i += 1,
            Some(r) => {
                let job = s.translate_jobs.running.remove(i);
                let r = r.unwrap_or_else(|_| Err("the translator stopped".to_owned()));
                done.push((job, r));
            }
            None => break,
        }
    }
    for (job, r) in done {
        changed = true;
        apply(s, job, r);
    }
    changed
}

fn apply(s: &mut Session, job: Job, r: Result<String, String>) {
    let text = match r {
        Ok(t) => t,
        Err(e) => {
            s.notify(crate::Notice::Error(format!("The translator could not translate: {e}")));
            return;
        }
    };
    if let Some(cid) = job.target.get("compose").and_then(Value::as_u64) {
        let Some(c) = s.composes.get(&cid) else { return };
        if job.compose_text.as_deref() != Some(c.body.to_plain_text().as_str()) {
            s.notify(crate::Notice::Info(
                "The message changed while it was being translated, so the translation was not put in. \
                 Translate it again to use it."
                    .into(),
            ));
            return;
        }
    } else if !job
        .target
        .get("item")
        .and_then(Value::as_u64)
        .is_some_and(|i| s.mailbox.mail.contains(ItemId(i)))
    {
        return;
    }
    if let Err(e) = crate::cmd::review::apply_translation(s, &job.target, &job.to, &text, job.by) {
        s.notify(crate::Notice::Error(format!("The translation could not be shown: {e}")));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use mailcraft_model::{FolderRole, MailItem, Mailbox};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A fake LibreTranslate server on 127.0.0.1 for `n` requests: `/translate` answers the text upper-cased
    /// (403 for the key "bad"); every request body is passed on through the channel.
    fn fake_server(n: usize) -> (String, mpsc::Receiver<(String, Value)>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((sock, _)) = l.accept() else { return };
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let req: Value = serde_json::from_slice(&body).unwrap_or_default();
                let (status, out) = if req["api_key"] == "bad" {
                    (403, json!({"error": "Invalid API key"}))
                } else {
                    let q = req["q"].as_str().unwrap_or_default().to_uppercase();
                    (
                        200,
                        json!({"translatedText": q, "detectedLanguage": {"language": "fr", "confidence": 90}}),
                    )
                };
                let _ = tx.send((path, req));
                let text = out.to_string();
                let mut w = sock;
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    text.len()
                );
                let _ = w.write_all(format!("{head}{text}").as_bytes());
            }
        });
        (url, rx)
    }

    fn session_with(subject: &str, body: &str) -> (Session, ItemId) {
        let mut s = Session::new(Mailbox::new_local("Local Folders"));
        let inbox = s.mailbox.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        let m = MailItem { folder: inbox, subject: subject.into(), ..Default::default() };
        let body = mailcraft_model::Body { text: Some(body.into()), ..Default::default() };
        let id = s.add_message(m, &mailcraft_store::Content::body(body)).unwrap();
        s.selection.focus = Some(id);
        (s, id)
    }

    const FRENCH: &str = "Bonjour, merci pour votre message. Nous avons reçu les documents et nous vous \
                          répondrons dans la semaine avec une proposition pour le projet.";
    const ENGLISH: &str = "Thank you for the report. We will review it and get back to you this week.";

    #[test]
    fn detects_languages_on_this_computer() {
        assert_eq!(detect(ENGLISH), Some("en"));
        assert_eq!(detect(FRENCH), Some("fr"));
        assert_eq!(
            detect("Vielen Dank für die Nachricht. Wir sind nicht im Büro und melden uns bald, bitte."),
            Some("de")
        );
        assert_eq!(
            detect(
                "Hola, gracias por tu mensaje. Te escribo para confirmar la reunión del lunes con el equipo."
            ),
            Some("es")
        );
        assert_eq!(detect("会議の資料をお送りします。ご確認ください。"), Some("ja"));
        assert_eq!(detect("会议资料已经发送，请查收。"), Some("zh"));
        assert_eq!(detect("회의 자료를 보내 드립니다. 확인해 주세요."), Some("ko"));
        assert_eq!(detect("Hi"), None, "too little text");
        assert_eq!(detect("12345 67890 !!! ???"), None);
        assert_eq!(detect(&"le ".repeat(100_000)), Some("fr"), "only the start is looked at");
    }

    #[test]
    fn nothing_is_configured_or_sent_by_default() {
        let (mut s, id) = session_with("Projet", FRENCH);
        assert!(!available(&s));
        assert!(translator(&s).is_none());
        assert_eq!(offer(&s, id), None, "no translator: nothing to offer");
        let r = s.run("review.translate", &json!({"id": id.0})).unwrap();
        assert_eq!(r["dialog"], "translate");
        assert_eq!(s.settings.text(URL_SETTING), "");
    }

    #[test]
    fn the_server_in_options_translates_and_the_key_stays_secret() {
        let (url, seen) = fake_server(4);
        let (mut s, id) = session_with("Projet", FRENCH);
        // Plain http is refused for another computer, accepted for this one.
        assert!(s.run("options.translator", &json!({"url": "http://translate.example.org"})).is_err());
        let far = json!({"key": URL_SETTING, "value": "http://translate.example.org"});
        assert!(s.run("options.set", &far).is_err());
        let r = s.run("options.translator", &json!({"url": url, "apiKey": "k-1"})).unwrap();
        assert_eq!(r["apiKey"], true);
        assert!(!r.to_string().contains("k-1"), "the key is never returned");
        assert_eq!(stored(&s).as_deref(), Some("k-1"));
        assert!(s.option(URL_SETTING).unwrap().as_str().unwrap().starts_with("http://127.0.0.1"));
        // Test: a sample through the server, nothing stored.
        let t = s.run("options.translator", &json!({"test": true, "apiKey": "k-2"})).unwrap();
        assert_eq!(t["ok"], true);
        assert_eq!(t["to"], "fr", "English readers test with French");
        let (path, body) = seen.recv().unwrap();
        assert_eq!(path, "/translate");
        assert_eq!(body["api_key"], "k-2");
        assert_eq!(stored(&s).as_deref(), Some("k-1"));
        // Translate Message goes to the server with the stored key, into "Translate messages into".
        let r = s.run("review.translate", &json!({"id": id.0})).unwrap();
        assert_eq!(r["text"], FRENCH.to_uppercase());
        let (_, body) = seen.recv().unwrap();
        assert_eq!((body["api_key"].as_str(), body["target"].as_str()), (Some("k-1"), Some("en")));
        assert!(shown(&s, id).unwrap().by.starts_with("the translation server http://127.0.0.1"));
        // A refused key is reported.
        s.run("options.translator", &json!({"apiKey": "bad"})).unwrap();
        let e = s.run("review.translate", &json!({"id": id.0})).unwrap_err();
        assert!(e.to_string().contains("Invalid API key"), "{e}");
        // Removing the key and the address: back to nothing configured.
        s.run("options.translator", &json!({"apiKey": null, "url": ""})).unwrap();
        assert!(s.secrets.get(KEY_SECRET).is_none() && !available(&s));
    }

    /// Regression: testing or saving another server address reused the API key stored for the old server,
    /// sending it to a server it was not meant for.
    #[test]
    fn the_stored_key_never_goes_to_another_server() {
        let (old, _old_seen) = fake_server(0);
        let (new, seen) = fake_server(2);
        let (mut s, _) = session_with("Projet", FRENCH);
        s.run("options.translator", &json!({"url": old, "apiKey": "k-old"})).unwrap();
        // Test with a newly typed address and no newly typed key: no key goes along.
        s.run("options.translator", &json!({"test": true, "url": new})).unwrap();
        let (_, body) = seen.recv().unwrap();
        assert!(body.get("api_key").is_none(), "{body}");
        assert_eq!(stored(&s).as_deref(), Some("k-old"), "a test stores nothing");
        // Saving the new address removes the old server's key, and says so.
        s.notices.clear();
        s.run("options.set", &json!({"key": URL_SETTING, "value": new})).unwrap();
        assert!(s.secrets.get(KEY_SECRET).is_none());
        assert!(s.notices.iter().any(|n| matches!(n, crate::Notice::Info(t) if t.contains("API key"))));
        s.run("review.translate", &json!({"id": s.selection.focus.unwrap().0})).unwrap();
        let (_, body) = seen.recv().unwrap();
        assert!(body.get("api_key").is_none(), "{body}");
        // A key typed with the new address is kept; setting the same address again keeps it.
        s.run("options.translator", &json!({"url": format!("{new}/"), "apiKey": "k-new"})).unwrap();
        assert_eq!(stored(&s).as_deref(), Some("k-new"));
        s.run("options.set", &json!({"key": URL_SETTING, "value": new})).unwrap();
        assert_eq!(stored(&s).as_deref(), Some("k-new"));
    }

    fn stored(s: &Session) -> Option<String> {
        bound_key(s).map(|(_, k)| k)
    }

    /// Regression: Reset All (and an unreadable settings.json) cleared the server address but kept its API
    /// key, and the next server entered was sent that key. The key is bound to its server now, and both
    /// paths remove it too.
    #[test]
    fn a_key_left_behind_never_goes_to_the_next_server() {
        let (a, _a_seen) = fake_server(0);
        let (b, seen) = fake_server(3);
        let (mut s, id) = session_with("Projet", FRENCH);
        s.run("options.translator", &json!({"url": a, "apiKey": "k-a"})).unwrap();
        assert_eq!(key_for(&s, &a).as_deref(), Some("k-a"));
        s.run("options.reset", &json!({})).unwrap();
        assert_eq!(s.settings.text(URL_SETTING), "");
        assert!(s.secrets.get(KEY_SECRET).is_none(), "Reset All removes the key");
        s.run("options.set", &json!({"key": URL_SETTING, "value": b})).unwrap();
        s.run("review.translate", &json!({"id": id.0})).unwrap();
        let (_, body) = seen.recv().unwrap();
        assert!(body.get("api_key").is_none(), "{body}");
        // A key that is still stored for server A (whatever path left it) is not sent to server B …
        store_key(&mut s, &a, "k-a").unwrap();
        assert!(!has_key(&s) && key_for(&s, &b).is_none());
        s.translations.clear();
        s.run("review.translate", &json!({"id": id.0})).unwrap();
        let (_, body) = seen.recv().unwrap();
        assert!(body.get("api_key").is_none(), "{body}");
        let r = s.run("options.translator", &json!({})).unwrap();
        assert_eq!(r["apiKey"], false, "B has no key");
        // … nor is one in the older form that names no server.
        s.secrets.set(KEY_SECRET, "k-legacy").unwrap();
        s.translations.clear();
        s.run("review.translate", &json!({"id": id.0})).unwrap();
        let (_, body) = seen.recv().unwrap();
        assert!(body.get("api_key").is_none(), "{body}");
        // A key needs a server to belong to.
        s.run("options.set", &json!({"key": URL_SETTING, "value": ""})).unwrap();
        assert!(s.secrets.get(KEY_SECRET).is_none(), "clearing the address removes a stray key");
        assert!(matches!(
            s.run("options.translator", &json!({"apiKey": "k"})),
            Err(crate::CmdError::Params(_))
        ));
    }

    /// Regression: a key sent with an address is stored only when the address is accepted; nothing changes
    /// when either is refused, so a key never ends up bound to the server that was there before.
    #[test]
    fn a_refused_address_stores_neither_it_nor_its_key() {
        let (mut s, _) = session_with("Projet", FRENCH);
        s.run("options.translator", &json!({"url": "https://a.example.org", "apiKey": "k-a"})).unwrap();
        let far = json!({"url": "http://b.example.org", "apiKey": "k-b"});
        assert!(matches!(s.run("options.translator", &far), Err(crate::CmdError::Params(_))));
        assert_eq!(s.settings.text(URL_SETTING), "https://a.example.org");
        assert_eq!(key_for(&s, "https://a.example.org").as_deref(), Some("k-a"));
        // An empty address with a key is refused before the address is cleared.
        let none = json!({"url": "", "apiKey": "k-b"});
        assert!(matches!(s.run("options.translator", &none), Err(crate::CmdError::Params(_))));
        assert_eq!(s.settings.text(URL_SETTING), "https://a.example.org");
        assert_eq!(key_for(&s, "https://a.example.org").as_deref(), Some("k-a"));
    }

    /// Regression: settings.json that cannot be read starts the app with no translation server, but the
    /// key stayed in the secret store.
    #[test]
    fn unreadable_settings_drop_the_translation_key() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        {
            let mut s = Session::open(dir.path()).unwrap();
            s.run("options.translator", &json!({"url": "http://127.0.0.1:9", "apiKey": "k-a"})).unwrap();
            assert_eq!(stored(&s).as_deref(), Some("k-a"));
        }
        std::fs::write(dir.path().join(crate::settings::FILE_NAME), b"{ not json").unwrap();
        let s = Session::open(dir.path()).unwrap();
        assert_eq!(s.settings.text(URL_SETTING), "");
        assert!(s.secrets.get(KEY_SECRET).is_none());
    }

    /// Regression: the desktop app's Test button waited for the server on the UI thread.
    #[test]
    fn the_test_runs_in_the_background() {
        let (url, seen) = fake_server(1);
        let (mut s, _) = session_with("Projet", FRENCH);
        s.background_translate = true;
        let r = s.run("options.translator", &json!({"test": true, "url": url})).unwrap();
        assert_eq!(r["testing"], true);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while testing(&s) && std::time::Instant::now() < end {
            s.poll_sync();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let r = take_test_result(&mut s).expect("the test finished").unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(r["text"], "THANK YOU FOR YOUR MESSAGE. I WILL REPLY TOMORROW.");
        assert!(take_test_result(&mut s).is_none(), "given once");
        assert_eq!(seen.try_iter().count(), 1);
    }

    /// Regression: the offer bar asks for the language of the shown message every frame, and each time the
    /// whole body was copied (or a whole HTML body stripped). Only the start of the body is read now.
    #[test]
    fn detection_reads_only_the_start_of_a_message() {
        let m = MailItem { subject: "Projet".into(), ..Default::default() };
        let html = Some(format!("<p>{FRENCH}</p>{}", "<div>12345</div>".repeat(200_000)));
        let mut b = mailcraft_model::Body { html, ..Default::default() };
        let sample = detect_sample(&m, &b);
        assert!(sample.chars().count() <= DETECT_CHARS + 10, "{}", sample.len());
        assert_eq!(detect(&sample), Some("fr"));
        b.html = None;
        b.text = Some(format!("{FRENCH} {}", "é".repeat(3_000_000)));
        let sample = detect_sample(&m, &b);
        assert!(sample.chars().count() <= DETECT_CHARS + 10);
        assert_eq!(detect(&sample), Some("fr"));
        // Byte limits never split a character.
        assert_eq!(prefix("aé", 2), "a");
        assert_eq!(prefix("aé", 3), "aé");
        assert_eq!(prefix("", 5), "");
    }

    #[test]
    fn ask_offers_and_always_translates_once() {
        let (url, seen) = fake_server(2);
        let (mut s, fr) = session_with("Projet", FRENCH);
        let inbox = s.mailbox.mail.get(fr).unwrap().folder;
        let m = MailItem { folder: inbox, subject: "Report".into(), ..Default::default() };
        let body = mailcraft_model::Body { text: Some(ENGLISH.into()), ..Default::default() };
        let en = s.add_message(m, &mailcraft_store::Content::body(body)).unwrap();
        s.run("options.translator", &json!({"url": url})).unwrap();
        // Ask (the default): an offer for the French message only.
        assert_eq!(offer(&s, fr), Some(("fr", "en".to_owned())));
        assert_eq!(offer(&s, en), None);
        assert!(!auto_translate(&mut s, fr), "Ask never translates by itself");
        // Never: no offer.
        s.run("options.set", &json!({"key": "language.translate", "value": "never"})).unwrap();
        assert_eq!(offer(&s, fr), None);
        // Always: translated when shown, once per session.
        s.run("options.set", &json!({"key": "language.translate", "value": "always"})).unwrap();
        assert!(!auto_translate(&mut s, en), "already in the reader's language");
        assert!(auto_translate(&mut s, fr));
        assert!(shown(&s, fr).is_some());
        assert_eq!(offer(&s, fr), None, "translated: nothing more to offer");
        s.translations.clear();
        assert!(!auto_translate(&mut s, fr), "tried once already");
        assert_eq!(seen.try_iter().count(), 1);
    }
}
