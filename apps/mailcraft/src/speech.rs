//! Read Aloud through the operating system's speech engine, installed into the UI as
//! [`mailcraft_ui_egui::Speech`]: espeak-ng, espeak or Speech Dispatcher (`spd-say`) on Linux and FreeBSD,
//! `say` on macOS, the .NET speech synthesizer through PowerShell on Windows. The text goes to the engine on
//! its standard input, never through a shell or as a command-line argument, so message text cannot be read
//! as options or commands. One utterance runs at a time; a new one or Stop ends the one before.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use mailcraft_ui_egui::{App, Speech};

/// Longest text handed to the engine in one go (characters).
const MAX_UTTERANCE_CHARS: usize = 20_000;

/// A speech engine program and the arguments that make it read its standard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Engine {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(target_os = "windows")]
const ENGINES: &[Engine] = &[Engine {
    name: "Windows speech",
    program: "powershell",
    args: &[
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; \
         Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $s.Speak([Console]::In.ReadToEnd())",
    ],
}];

#[cfg(target_os = "macos")]
const ENGINES: &[Engine] = &[Engine { name: "macOS speech", program: "say", args: &[] }];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const ENGINES: &[Engine] = &[
    Engine { name: "eSpeak NG", program: "espeak-ng", args: &[] },
    Engine { name: "eSpeak", program: "espeak", args: &[] },
    Engine { name: "Speech Dispatcher", program: "spd-say", args: &["-e", "-w"] },
];

/// Speech through an engine program; holds the running utterance.
pub struct ProcessSpeech {
    engine: Engine,
    child: Mutex<Option<Child>>,
}

impl ProcessSpeech {
    /// The first engine found on this computer (it starts and accepts input), or `None`.
    pub fn detect() -> Option<ProcessSpeech> {
        ENGINES.iter().copied().find(probe).map(|engine| ProcessSpeech { engine, child: Mutex::new(None) })
    }

    fn kill_current(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut c) = guard.take()
        {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// True when `e`'s program exists: it can be started (with empty input it says nothing and exits).
fn probe(e: &Engine) -> bool {
    let spawned = Command::new(e.program)
        .args(e.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        Err(_) => false,
    }
}

impl Speech for ProcessSpeech {
    fn speak(&self, text: &str) -> Result<(), String> {
        self.kill_current();
        let text: String = text.chars().take(MAX_UTTERANCE_CHARS).collect();
        let mut child = Command::new(self.engine.program)
            .args(self.engine.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{} could not start: {e}", self.engine.name))?;
        if let Some(mut stdin) = child.stdin.take() {
            // Written from a short-lived thread: the text can be larger than the pipe buffer, and engines
            // read it while they speak, so writing it here would hold up the frame until speech catches
            // up. The pipe closes when `stdin` drops, which tells the engine the text is complete; once the
            // utterance is stopped (the engine killed) the write fails and the thread ends.
            let spawned =
                std::thread::Builder::new().name("mailcraft-speech-input".into()).spawn(move || {
                    let _ = stdin.write_all(text.as_bytes());
                });
            if let Err(e) = spawned {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} did not take the text: {e}", self.engine.name));
            }
        }
        if let Ok(mut guard) = self.child.lock() {
            *guard = Some(child);
        }
        Ok(())
    }

    fn stop(&self) {
        self.kill_current();
    }

    fn speaking(&self) -> bool {
        let Ok(mut guard) = self.child.lock() else { return false };
        match guard.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(_) => {
                *guard = None;
                false
            }
            None => false,
        }
    }

    fn name(&self) -> String {
        self.engine.name.to_owned()
    }
}

impl Drop for ProcessSpeech {
    fn drop(&mut self) {
        self.kill_current();
    }
}

/// Installs the speech engine found on this computer into `app` (none found: Read Aloud shows the text).
pub fn install(app: &mut App) {
    match ProcessSpeech::detect() {
        Some(sp) => {
            log::info!("Read Aloud uses {}", sp.engine.name);
            app.set_speech(Arc::new(sp));
        }
        None => log::info!("no speech engine found; Read Aloud shows the text only"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_engine_is_not_detected_and_never_panics() {
        let missing = Engine { name: "none", program: "mailcraft-no-such-speech-engine", args: &[] };
        assert!(!probe(&missing));
        let sp = ProcessSpeech { engine: missing, child: Mutex::new(None) };
        assert!(sp.speak("hello").is_err());
        assert!(!sp.speaking());
        sp.stop();
        assert_eq!(sp.name(), "none");
    }

    #[cfg(unix)]
    #[test]
    fn a_running_utterance_reports_speaking_until_stopped() {
        // `cat` stands in for an engine: it reads the text and exits; `sleep` keeps running until stopped.
        let sleeper = Engine { name: "sleep", program: "sleep", args: &["30"] };
        let sp = ProcessSpeech { engine: sleeper, child: Mutex::new(None) };
        if sp.speak("text").is_err() {
            return; // No `sleep` here.
        }
        assert!(sp.speaking());
        sp.stop();
        assert!(!sp.speaking());
        let cat = Engine { name: "cat", program: "cat", args: &[] };
        let sp = ProcessSpeech { engine: cat, child: Mutex::new(None) };
        if sp.speak("-rf --help $(echo nope)").is_ok() {
            for _ in 0..200 {
                if !sp.speaking() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(!sp.speaking(), "the engine ends once its input is read");
        }
    }

    /// Regression: the text was written to the engine on the UI thread; more than the pipe buffer holds
    /// (long non-ASCII text) blocked the frame until the engine had read it, forever for one that reads
    /// slowly or not at all.
    #[cfg(unix)]
    #[test]
    fn speaking_long_text_never_blocks_the_caller() {
        let sleeper = Engine { name: "sleep", program: "sleep", args: &["30"] };
        let sp = ProcessSpeech { engine: sleeper, child: Mutex::new(None) };
        let text = "\u{1d11e}".repeat(MAX_UTTERANCE_CHARS); // 80 KB of UTF-8, more than a pipe buffer.
        let started = std::time::Instant::now();
        if sp.speak(&text).is_err() {
            return; // No `sleep` here.
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "speak returned at once");
        assert!(sp.speaking());
        sp.stop();
        assert!(!sp.speaking());
    }
}
