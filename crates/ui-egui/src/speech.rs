//! Read Aloud: a text-to-speech backend the desktop app installs with [`set_speech`] (the engine and the
//! web build cannot speak; without a backend the Read Aloud dialog shows the text and says why), and the
//! paragraph player the dialog drives: play, pause (stops after saving the place), resume, previous and next
//! paragraph, stop, and the reading speed.

use std::sync::{Arc, PoisonError, RwLock};

/// Platform speech. Implementations must not panic or block for long.
pub trait Speech: Send + Sync {
    /// Starts speaking `text` at `rate` (-5 slow … 0 normal … 5 fast), stopping anything still speaking.
    fn speak(&self, text: &str, rate: i8) -> Result<(), String>;
    /// Stops speaking.
    fn stop(&self);
    /// Whether it is still speaking what [`Speech::speak`] last started.
    fn speaking(&self) -> bool;
}

static BACKEND: RwLock<Option<Arc<dyn Speech>>> = RwLock::new(None);

/// Installs (or with `None` removes) the speech backend.
pub fn set_speech(backend: Option<Arc<dyn Speech>>) {
    *BACKEND.write().unwrap_or_else(PoisonError::into_inner) = backend;
}

/// The installed speech backend.
pub fn speech() -> Option<Arc<dyn Speech>> {
    BACKEND.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Longest piece handed to the backend at once (characters); longer paragraphs are split at sentence ends.
pub const MAX_PIECE: usize = 1500;

/// The pieces Read Aloud speaks one after another: paragraphs, long ones split after sentences.
pub fn pieces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
        let mut cur = String::new();
        for word in para.split_inclusive(['.', '!', '?']) {
            if !cur.is_empty() && cur.chars().count() + word.chars().count() > MAX_PIECE {
                out.push(std::mem::take(&mut cur).trim().to_owned());
            }
            if word.chars().count() > MAX_PIECE {
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(MAX_PIECE) {
                    out.push(chunk.iter().collect::<String>().trim().to_owned());
                }
                continue;
            }
            cur.push_str(word);
        }
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_owned());
        }
    }
    out.retain(|p| !p.is_empty());
    out
}

/// What the player is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// The Read Aloud player: which piece, and whether it plays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Player {
    pub pieces: Vec<String>,
    pub index: usize,
    pub state: PlayState,
    pub rate: i8,
    pub error: String,
}

impl Player {
    pub fn new(text: &str) -> Player {
        Player { pieces: pieces(text), ..Default::default() }
    }

    fn speak_current(&mut self, backend: &dyn Speech) {
        match self.pieces.get(self.index) {
            Some(p) => match backend.speak(p, self.rate) {
                Ok(()) => {
                    self.state = PlayState::Playing;
                    self.error.clear();
                }
                Err(e) => {
                    self.state = PlayState::Stopped;
                    self.error = e;
                }
            },
            None => {
                self.state = PlayState::Stopped;
                self.index = 0;
            }
        }
    }

    /// Play / Pause button.
    pub fn toggle(&mut self, backend: &dyn Speech) {
        if self.state == PlayState::Playing {
            backend.stop();
            self.state = PlayState::Paused;
        } else {
            self.speak_current(backend);
        }
    }

    /// Next (`+1`) or previous (`-1`) piece; keeps playing when it was.
    pub fn step(&mut self, backend: &dyn Speech, by: isize) {
        let last = self.pieces.len().saturating_sub(1);
        self.index = self.index.saturating_add_signed(by).min(last);
        if self.state == PlayState::Playing {
            self.speak_current(backend);
        }
    }

    pub fn stop(&mut self, backend: &dyn Speech) {
        backend.stop();
        self.state = PlayState::Stopped;
        self.index = 0;
    }

    /// Per frame: when the current piece is done, the next one starts (the end stops the player). Returns
    /// whether it is still playing (the dialog keeps repainting).
    pub fn tick(&mut self, backend: &dyn Speech) -> bool {
        if self.state != PlayState::Playing {
            return false;
        }
        if !backend.speaking() {
            self.index += 1;
            self.speak_current(backend);
        }
        self.state == PlayState::Playing
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A backend that records what it was asked to say; `busy` frames of speaking per piece.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub said: Mutex<Vec<(String, i8)>>,
        pub left: Mutex<u32>,
        pub stops: Mutex<u32>,
    }

    impl Speech for Fake {
        fn speak(&self, text: &str, rate: i8) -> Result<(), String> {
            self.said.lock().unwrap().push((text.to_owned(), rate));
            *self.left.lock().unwrap() = 2;
            Ok(())
        }
        fn stop(&self) {
            *self.stops.lock().unwrap() += 1;
            *self.left.lock().unwrap() = 0;
        }
        fn speaking(&self) -> bool {
            let mut l = self.left.lock().unwrap();
            *l = l.saturating_sub(1);
            *l > 0
        }
    }

    #[test]
    fn pieces_split_paragraphs_and_long_sentences() {
        assert_eq!(pieces("One.\n\n  Two!  \n"), vec!["One.", "Two!"]);
        let long = "Word word. ".repeat(300);
        let p = pieces(&long);
        assert!(p.len() > 1 && p.iter().all(|x| x.chars().count() <= MAX_PIECE));
        let blob = "x".repeat(MAX_PIECE * 2 + 3);
        assert_eq!(pieces(&blob).len(), 3, "text without sentence ends is cut");
        assert!(pieces("").is_empty());
    }

    #[test]
    fn player_plays_pauses_steps_and_ends() {
        let f = Fake::default();
        let mut p = Player::new("First.\nSecond.\nThird.");
        p.rate = 2;
        p.toggle(&f);
        assert_eq!(p.state, PlayState::Playing);
        assert!(p.tick(&f), "still speaking the first");
        assert!(p.tick(&f), "first done: the second starts");
        assert_eq!(p.index, 1);
        p.toggle(&f);
        assert_eq!(p.state, PlayState::Paused);
        p.toggle(&f);
        assert_eq!(
            f.said.lock().unwrap().last().unwrap(),
            &("Second.".to_owned(), 2),
            "resume repeats the paragraph"
        );
        p.step(&f, 1);
        assert_eq!(p.index, 2);
        p.step(&f, 5);
        assert_eq!(p.index, 2, "clamped at the last");
        p.step(&f, -9);
        assert_eq!(p.index, 0);
        p.stop(&f);
        p.index = 2;
        p.toggle(&f);
        p.tick(&f);
        assert!(!p.tick(&f), "after the last piece the player stops");
        assert_eq!((p.state, p.index), (PlayState::Stopped, 0));
        p.toggle(&f);
        p.stop(&f);
        assert_eq!(p.state, PlayState::Stopped);
        let mut empty = Player::new("");
        empty.toggle(&f);
        assert_eq!(empty.state, PlayState::Stopped);
    }
}
