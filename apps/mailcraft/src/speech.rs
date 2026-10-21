//! Read Aloud through the system's speech, installed into the UI as [`mailcraft_ui_egui::speech`]: `say` on
//! macOS, the .NET speech synthesizer through PowerShell on Windows, and on Linux and the BSDs Speech
//! Dispatcher (the voice the person set up, shared with screen readers) or else `espeak-ng` / `espeak`.
//!
//! - A program (`say`, PowerShell, espeak) speaks each piece as one child process; Stop ends that process,
//!   which ends its speech.
//! - Speech Dispatcher is spoken to directly over its socket (SSIP), on one connection of MailCraft's own:
//!   Stop cancels only that connection's messages (`CANCEL self`), never everyone's, so a screen reader
//!   using the same Speech Dispatcher (Orca) keeps talking. Nothing here waits for another program, so the
//!   UI thread is not held up. When Speech Dispatcher cannot be reached, espeak speaks instead if present.
//!
//! The text reaches a program as an argument after `--` or on standard input, never through a shell.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};

use mailcraft_ui_egui::speech::Speech;

/// Which program speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Engine {
    /// Speech Dispatcher over its socket; espeak when it cannot be reached.
    #[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
    Speechd {
        espeak: Option<PathBuf>,
    },
    Espeak(PathBuf),
    Say(PathBuf),
    PowerShell(PathBuf),
}

/// The system speech backend.
pub struct SystemSpeech {
    engine: Engine,
    child: Mutex<Option<Child>>,
    #[cfg(all(unix, not(target_os = "macos")))]
    speechd: Mutex<ssip::Slot>,
}

/// `name` on the PATH.
fn find(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

fn detect() -> Option<Engine> {
    if cfg!(target_os = "macos") {
        return find("say").map(Engine::Say);
    }
    if cfg!(target_os = "windows") {
        return find("powershell.exe").or_else(|| find("pwsh.exe")).map(Engine::PowerShell);
    }
    let espeak = find("espeak-ng").or_else(|| find("espeak"));
    #[cfg(all(unix, not(target_os = "macos")))]
    if find("spd-say").is_some()
        || find("speech-dispatcher").is_some()
        || ssip::socket_path().is_some_and(|p| p.exists())
    {
        return Some(Engine::Speechd { espeak });
    }
    espeak.map(Engine::Espeak)
}

impl SystemSpeech {
    fn new(engine: Engine) -> SystemSpeech {
        SystemSpeech {
            engine,
            child: Mutex::new(None),
            #[cfg(all(unix, not(target_os = "macos")))]
            speechd: Mutex::new(ssip::Slot::default()),
        }
    }
}

/// Installs the system speech backend when the system has a speech program.
pub fn install() {
    match detect() {
        Some(engine) => {
            mailcraft_ui_egui::speech::set_speech(Some(std::sync::Arc::new(SystemSpeech::new(engine))));
        }
        None => log::info!("no speech program found; Read Aloud shows the text only"),
    }
}

/// The command line for `text` at `rate` (-5..=5). Windows reads the text from standard input.
fn command(engine: &Engine, text: &str, rate: i8) -> Option<Command> {
    let rate = rate.clamp(-5, 5);
    Some(match engine {
        Engine::Speechd { espeak: Some(p) } | Engine::Espeak(p) => {
            let mut c = Command::new(p);
            c.args(["-s", &(175 + i32::from(rate) * 25).to_string(), "--", text]);
            c
        }
        Engine::Speechd { espeak: None } => return None,
        Engine::Say(p) => {
            let mut c = Command::new(p);
            c.args(["-r", &(180 + i32::from(rate) * 30).to_string(), "--", text]);
            c
        }
        Engine::PowerShell(p) => {
            let mut c = Command::new(p);
            let script = format!(
                "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
$s.Rate = {}; $s.Speak([Console]::In.ReadToEnd())",
                i32::from(rate) * 2
            );
            c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            c
        }
    })
}

/// Ends our own child process (only ours).
fn stop_child(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl SystemSpeech {
    /// Speaks `text` with a program (one child process).
    fn speak_child(&self, text: &str, rate: i8) -> Result<(), String> {
        let mut slot = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        stop_child(&mut slot);
        let windows = matches!(self.engine, Engine::PowerShell(_));
        let mut cmd =
            command(&self.engine, text, rate).ok_or_else(|| "No speech program was found.".to_owned())?;
        cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(if windows {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().map_err(|e| format!("The speech program could not start ({e})."))?;
        if windows && let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        *slot = Some(child);
        Ok(())
    }
}

impl Speech for SystemSpeech {
    fn speak(&self, text: &str, rate: i8) -> Result<(), String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Engine::Speechd { espeak } = &self.engine {
            let spoken = self.speechd.lock().unwrap_or_else(PoisonError::into_inner).speak(text, rate);
            return match spoken {
                Ok(()) => Ok(()),
                Err(_) if espeak.is_some() => self.speak_child(text, rate),
                Err(e) => Err(e),
            };
        }
        self.speak_child(text, rate)
    }

    fn stop(&self) {
        stop_child(&mut self.child.lock().unwrap_or_else(PoisonError::into_inner));
        #[cfg(all(unix, not(target_os = "macos")))]
        self.speechd.lock().unwrap_or_else(PoisonError::into_inner).cancel();
    }

    fn speaking(&self) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        if self.speechd.lock().unwrap_or_else(PoisonError::into_inner).speaking() {
            return true;
        }
        let mut slot = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(_) => {
                *slot = None;
                false
            }
            None => false,
        }
    }
}

/// A small Speech Dispatcher client (SSIP, the text protocol on its Unix socket): MailCraft's own
/// connection, so cancelling touches only MailCraft's messages.
#[cfg(all(unix, not(target_os = "macos")))]
mod ssip {
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    /// How long a reply may take (the server answers at once; only speech itself takes time).
    const REPLY_WAIT: Duration = Duration::from_secs(2);
    /// Longest reply line read.
    const MAX_LINE: u64 = 16 * 1024;
    /// Finished message ids remembered (to tell whether the current one ended).
    const MAX_ENDED: usize = 64;
    /// Starting the server is tried once per run.
    static SPAWNED: AtomicBool = AtomicBool::new(false);

    /// Where Speech Dispatcher listens: `SPEECHD_ADDRESS` (`unix_socket:/path`), else the default socket in
    /// the runtime directory.
    pub fn socket_path() -> Option<PathBuf> {
        if let Ok(addr) = std::env::var("SPEECHD_ADDRESS")
            && let Some(rest) = addr.trim().strip_prefix("unix_socket")
        {
            let p = rest.trim_start_matches(':');
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
        Some(PathBuf::from(runtime).join("speech-dispatcher").join("speechd.sock"))
    }

    /// A reply: its code, the data lines and the final text.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Reply {
        pub code: u16,
        pub lines: Vec<String>,
    }

    /// What the reader thread learnt.
    #[derive(Default)]
    struct State {
        alive: bool,
        current: Option<u64>,
        ended: VecDeque<u64>,
    }

    /// One connection.
    pub struct Client {
        stream: UnixStream,
        replies: Receiver<Reply>,
        state: Arc<Mutex<State>>,
    }

    /// Splits one protocol line into its code, whether more lines follow, and its text.
    pub fn parse_line(line: &str) -> Option<(u16, bool, &str)> {
        let line = line.trim_end_matches(['\r', '\n']);
        let code: u16 = line.get(..3)?.parse().ok()?;
        let more = match line.get(3..4) {
            Some("-") => true,
            Some(" ") | None => false,
            Some(_) => return None,
        };
        Some((code, more, line.get(4..).unwrap_or("")))
    }

    /// `text` as an SSIP message body: CRLF lines, a leading dot doubled, ended by a line with one dot.
    pub fn body(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 8);
        for line in text.split('\n') {
            let line = line.trim_end_matches('\r').replace('\r', " ");
            if line.starts_with('.') {
                out.push('.');
            }
            out.push_str(&line);
            out.push_str("\r\n");
        }
        out.push_str(".\r\n");
        out
    }

    fn read_loop(stream: UnixStream, tx: Sender<Reply>, state: Arc<Mutex<State>>) {
        let mut reader = BufReader::new(stream);
        let mut pending = Reply::default();
        loop {
            let mut line = String::new();
            match (&mut reader).take(MAX_LINE).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Some((code, more, text)) = parse_line(&line) else { continue };
            pending.code = code;
            pending.lines.push(text.to_owned());
            if more {
                if pending.lines.len() > 64 {
                    pending = Reply::default();
                }
                continue;
            }
            let reply = std::mem::take(&mut pending);
            if (700..800).contains(&reply.code) {
                event(&state, &reply);
            } else if tx.send(reply).is_err() {
                break;
            }
        }
        state.lock().unwrap_or_else(PoisonError::into_inner).alive = false;
    }

    /// A notification: an END or CANCELED message id is remembered as finished.
    fn event(state: &Mutex<State>, reply: &Reply) {
        let what = reply.lines.last().map(|l| l.to_ascii_uppercase()).unwrap_or_default();
        let finished = what.contains("END") || what.contains("CANCEL") || what.contains("STOP");
        let id = reply.lines.first().and_then(|l| l.trim().parse::<u64>().ok());
        if let (true, Some(id)) = (finished, id) {
            let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
            st.ended.push_back(id);
            while st.ended.len() > MAX_ENDED {
                st.ended.pop_front();
            }
        }
    }

    impl Client {
        pub fn connect(path: &Path) -> Result<Client, String> {
            let fail = |e: std::io::Error| format!("Speech Dispatcher cannot be reached ({e}).");
            let stream = UnixStream::connect(path).map_err(fail)?;
            let _ = stream.set_write_timeout(Some(REPLY_WAIT));
            let reader = stream.try_clone().map_err(fail)?;
            let (tx, replies) = channel();
            let state = Arc::new(Mutex::new(State { alive: true, ..State::default() }));
            let st = state.clone();
            std::thread::Builder::new()
                .name("mailcraft-speechd".into())
                .spawn(move || read_loop(reader, tx, st))
                .map_err(fail)?;
            let mut c = Client { stream, replies, state };
            c.command("SET self CLIENT_NAME user:mailcraft:read_aloud")?;
            c.command("SET self NOTIFICATION end on")?;
            c.command("SET self NOTIFICATION cancel on")?;
            Ok(c)
        }

        pub fn alive(&self) -> bool {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).alive
        }

        fn broken(&self) {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).alive = false;
        }

        fn send(&mut self, data: &str) -> Result<(), String> {
            self.stream.write_all(data.as_bytes()).map_err(|e| {
                self.broken();
                format!("Speech Dispatcher stopped answering ({e}).")
            })
        }

        fn reply(&self) -> Result<Reply, String> {
            self.replies.recv_timeout(REPLY_WAIT).map_err(|_| {
                // A late answer would be taken for the next command's: this connection is done.
                self.broken();
                "Speech Dispatcher did not answer.".to_owned()
            })
        }

        fn command(&mut self, line: &str) -> Result<Reply, String> {
            self.send(&format!("{line}\r\n"))?;
            let r = self.reply()?;
            if (200..300).contains(&r.code) {
                Ok(r)
            } else {
                Err(format!("Speech Dispatcher refused the request ({}).", r.code))
            }
        }

        /// Speaks `text` at `rate` (-5..=5), cancelling what this connection was still saying.
        pub fn speak(&mut self, text: &str, rate: i8) -> Result<(), String> {
            self.cancel();
            self.command(&format!("SET self RATE {}", i32::from(rate.clamp(-5, 5)) * 18))?;
            self.send("SPEAK\r\n")?;
            let r = self.reply()?;
            if r.code != 230 {
                return Err(format!("Speech Dispatcher refused the text ({}).", r.code));
            }
            self.send(&body(text))?;
            let r = self.reply()?;
            if !(200..300).contains(&r.code) {
                return Err(format!("Speech Dispatcher refused the text ({}).", r.code));
            }
            let id = r.lines.first().and_then(|l| l.trim().parse::<u64>().ok());
            self.state.lock().unwrap_or_else(PoisonError::into_inner).current = id;
            Ok(())
        }

        /// Cancels this connection's messages only (`CANCEL self`).
        pub fn cancel(&mut self) {
            let speaking = self.speaking();
            self.state.lock().unwrap_or_else(PoisonError::into_inner).current = None;
            if speaking && self.alive() {
                let _ = self.command("CANCEL self");
            }
        }

        pub fn speaking(&self) -> bool {
            let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.alive && st.current.is_some_and(|id| !st.ended.contains(&id))
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = self.stream.write_all(b"QUIT\r\n");
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The connection, made on first use and again after it broke.
    #[derive(Default)]
    pub struct Slot {
        client: Option<Client>,
        /// Socket used instead of [`socket_path`] (tests).
        pub path: Option<PathBuf>,
    }

    impl Slot {
        fn client(&mut self) -> Result<&mut Client, String> {
            if self.client.as_ref().is_some_and(|c| !c.alive()) {
                self.client = None;
            }
            if self.client.is_none() {
                let path = self
                    .path
                    .clone()
                    .or_else(socket_path)
                    .ok_or_else(|| "Speech Dispatcher's socket is not known.".to_owned())?;
                let c = match Client::connect(&path) {
                    Ok(c) => c,
                    Err(e) if self.path.is_none() && start_server() => {
                        Client::connect(&path).map_err(|_| e)?
                    }
                    Err(e) => return Err(e),
                };
                self.client = Some(c);
            }
            self.client.as_mut().ok_or_else(|| "Speech Dispatcher cannot be reached.".to_owned())
        }

        pub fn speak(&mut self, text: &str, rate: i8) -> Result<(), String> {
            self.client()?.speak(text, rate)
        }

        pub fn cancel(&mut self) {
            if let Some(c) = self.client.as_mut() {
                c.cancel();
            }
        }

        pub fn speaking(&self) -> bool {
            self.client.as_ref().is_some_and(Client::speaking)
        }
    }

    /// Starts Speech Dispatcher (`speech-dispatcher --spawn`, which returns once it listens), once per run;
    /// true when it was started now.
    fn start_server() -> bool {
        if SPAWNED.swap(true, Ordering::Relaxed) {
            return false;
        }
        let Some(program) = super::find("speech-dispatcher") else { return false };
        let Ok(mut child) = std::process::Command::new(program)
            .arg("--spawn")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return false;
        };
        for _ in 0..30 {
            if let Ok(Some(_)) = child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Still starting: leave it, it detaches on its own.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_an_argument_after_the_options() {
        let c = command(&Engine::Espeak("espeak-ng".into()), "-rf /", 2).unwrap();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-s", "225", "--", "-rf /"]);
        let c = command(&Engine::Say("say".into()), "Hi", -9).unwrap();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-r", "30", "--", "Hi"], "rate clamped to -5");
        let c = command(&Engine::PowerShell("pwsh.exe".into()), "Hi; rm x", 0).unwrap();
        assert!(
            c.get_args().all(|a| !a.to_string_lossy().contains("rm x")),
            "Windows gets the text on stdin"
        );
        assert!(command(&Engine::Speechd { espeak: None }, "Hi", 0).is_none(), "no program to run");
    }

    #[test]
    fn a_missing_program_reports_instead_of_panicking() {
        let s = SystemSpeech::new(Engine::Espeak("/nonexistent/espeak".into()));
        assert!(s.speak("hello", 0).is_err());
        assert!(!s.speaking());
        s.stop();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    mod speechd {
        use super::super::ssip::{body, parse_line};
        use super::*;
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        use std::sync::Arc;

        #[test]
        fn protocol_lines_and_bodies() {
            assert_eq!(parse_line("225-17\r\n"), Some((225, true, "17")));
            assert_eq!(parse_line("225 OK MESSAGE QUEUED\r\n"), Some((225, false, "OK MESSAGE QUEUED")));
            assert_eq!(parse_line("230"), Some((230, false, "")));
            assert_eq!(parse_line("xx"), None);
            assert_eq!(parse_line("2é5 x"), None);
            assert_eq!(parse_line("225+x"), None);
            assert_eq!(body("Hi\n.\n.x\r\nend"), "Hi\r\n..\r\n..x\r\nend\r\n.\r\n", "dots doubled");
        }

        /// A stand-in Speech Dispatcher: answers every command, records them, and reports the end of
        /// each message after `end_after` lines of data.
        fn fake_server(dir: &std::path::Path) -> (std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
            let path = dir.join("speechd.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut out = stream.try_clone().unwrap();
                let mut lines = BufReader::new(stream).lines();
                let mut id = 0;
                while let Some(Ok(line)) = lines.next() {
                    log.lock().unwrap().push(line.clone());
                    let reply = match line.as_str() {
                        "SPEAK" => {
                            out.write_all(b"230 OK RECEIVING DATA\r\n").unwrap();
                            for data in lines.by_ref() {
                                let data = data.unwrap();
                                log.lock().unwrap().push(data.clone());
                                if data == "." {
                                    break;
                                }
                            }
                            id += 1;
                            format!("225-{id}\r\n225 OK MESSAGE QUEUED\r\n")
                        }
                        "CANCEL self" => "210 OK CANCELED\r\n".to_owned(),
                        "QUIT" => break,
                        "FINISH" => String::new(),
                        l if l.starts_with("SET self") => "208 OK SET\r\n".to_owned(),
                        _ => "300 ERR\r\n".to_owned(),
                    };
                    out.write_all(reply.as_bytes()).unwrap();
                    // The first message ends at once (as a short one would).
                    if line == "SPEAK" && id == 1 {
                        out.write_all(format!("702-{id}\r\n702-1\r\n702 END\r\n").as_bytes()).unwrap();
                    }
                }
            });
            (path, seen)
        }

        fn wait_for(f: impl Fn() -> bool) -> bool {
            for _ in 0..200 {
                if f() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        }

        #[test]
        fn speech_dispatcher_cancels_only_its_own_messages() {
            // Regression: Stop and every new paragraph ran `spd-say -S`, which stops speech of every client,
            // the screen reader's too.
            let dir = tempfile::tempdir().unwrap();
            let (path, seen) = fake_server(dir.path());
            let s = SystemSpeech::new(Engine::Speechd { espeak: None });
            s.speechd.lock().unwrap().path = Some(path);
            s.speak("First.", 0).unwrap();
            assert!(wait_for(|| !s.speaking()), "the end of the message is noticed");
            s.speak("Second.\n.dot", 3).unwrap();
            assert!(s.speaking());
            s.speak("Third.", 0).unwrap();
            s.stop();
            assert!(!s.speaking());
            let log = seen.lock().unwrap().clone();
            assert!(log.iter().all(|l| !l.contains("all") && !l.contains("ALL")), "{log:?}");
            assert!(log.contains(&"SET self RATE 54".to_owned()), "{log:?}");
            assert!(log.contains(&"..dot".to_owned()), "a leading dot is doubled: {log:?}");
            assert_eq!(log.iter().filter(|l| *l == "CANCEL self").count(), 2, "{log:?}");
            assert_eq!(log.iter().filter(|l| *l == "SPEAK").count(), 3);
        }

        #[test]
        fn an_unreachable_speech_dispatcher_falls_back_or_reports() {
            let dir = tempfile::tempdir().unwrap();
            let s = SystemSpeech::new(Engine::Speechd { espeak: None });
            s.speechd.lock().unwrap().path = Some(dir.path().join("none.sock"));
            assert!(s.speak("Hi", 0).is_err());
            assert!(!s.speaking());
            s.stop();
            let s = SystemSpeech::new(Engine::Speechd { espeak: Some("/nonexistent/espeak".into()) });
            s.speechd.lock().unwrap().path = Some(dir.path().join("none.sock"));
            let e = s.speak("Hi", 0).unwrap_err();
            assert!(e.contains("speech program"), "espeak was tried: {e}");
        }
    }
}
