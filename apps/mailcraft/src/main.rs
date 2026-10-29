//! MailCraft desktop app: an eframe window around [`mailcraft_ui_egui::App`].
//!
//! ```text
//! mailcraft [--profile <dir>] [--sample] [--control-port <N>] [--version] [--help] [mailto:…]
//! ```
//!
//! A `mailto:` URL (RFC 6068) opens a new message to it (`mail.newFromMailto`). With File › Options ›
//! Advanced › "Enable troubleshooting logging" on, the profile's `logs/mailcraft.log` records what the app
//! does (`mailcraft_engine::troubleshoot`; size-capped, no secrets).
//!
//! The profile (mail store) defaults to the platform data folder: `$XDG_DATA_HOME/MailCraft` or
//! `~/.local/share/MailCraft` on Linux and other Unix systems, `~/Library/Application Support/MailCraft` on
//! macOS, `%APPDATA%\MailCraft` on Windows.

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use mailcraft_engine::Session;
use mailcraft_ui_egui::App;

mod capture;
mod clipboard;
mod file_dialogs;
mod installed_fonts;
mod speech;

const USAGE: &str = "\
Usage: mailcraft [OPTIONS]

Options:
  --profile <DIR>       Data folder to open (created when missing)
  --sample              Open an in-memory sample mailbox instead of a profile
  --control-port <N>    Accept control commands on 127.0.0.1:<N> (for tests and automation)
  --version             Print the version and exit
  -h, --help            Print this help and exit

Arguments:
  mailto:<address>      Open a new message (RFC 6068 mailto: link)
";

/// `git <hash>, <date>` from `build.rs` (`unknown` when the build had neither git nor the variables).
const BUILD_INFO: &str = concat!("git ", env!("MAILCRAFT_GIT_HASH"), ", ", env!("MAILCRAFT_BUILD_DATE"));

/// `--version` output: `mailcraft X.Y.Z (git <hash>, <date>)`.
fn version_line() -> String {
    format!("mailcraft {} ({BUILD_INFO})", env!("CARGO_PKG_VERSION"))
}

/// Parsed command line.
#[derive(Debug, Default, PartialEq)]
struct Args {
    profile: Option<PathBuf>,
    sample: bool,
    control_port: Option<u16>,
    version: bool,
    help: bool,
    /// A `mailto:` link to open as a new message.
    mailto: Option<String>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (a.clone(), None),
        };
        let mut value =
            |name: &str| inline.clone().or_else(|| it.next()).ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--profile" => out.profile = Some(PathBuf::from(value("--profile")?)),
            "--sample" => out.sample = true,
            "--control-port" => {
                let v = value("--control-port")?;
                let port =
                    v.parse::<u16>().map_err(|_| format!("--control-port: `{v}` is not a port number"))?;
                if port == 0 {
                    return Err("--control-port: port must be 1-65535".into());
                }
                out.control_port = Some(port);
            }
            "--version" | "-V" => out.version = true,
            "--help" | "-h" => out.help = true,
            m if m.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:")) => {
                out.mailto = Some(a.clone())
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(out)
}

/// Default profile folder from the platform's environment variables (no extra crates).
fn default_profile() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "windows") {
        return env("APPDATA")
            .or_else(|| env("USERPROFILE").map(|h| h.join("AppData").join("Roaming")))
            .map(|d| d.join("MailCraft"));
    }
    if cfg!(target_os = "macos") {
        return env("HOME").map(|h| h.join("Library").join("Application Support").join("MailCraft"));
    }
    env("XDG_DATA_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| env("HOME").map(|h| h.join(".local").join("share")))
        .map(|d| d.join("MailCraft"))
}

/// Opens the session; a profile that cannot be opened falls back to an empty in-memory mailbox with the
/// reason shown as an error notice.
fn open_session(args: &Args) -> Session {
    if args.sample {
        let now = mailcraft_engine::model::DateTime::now_utc();
        return mailcraft_engine::sample::sample_session_at(now);
    }
    let Some(dir) = args.profile.clone().or_else(default_profile) else {
        let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local(
            mailcraft_engine::model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME,
        ));
        s.notify(mailcraft_engine::Notice::Error(
            "No data folder could be found (HOME is not set); changes will not be saved. Use --profile <dir>.".into(),
        ));
        return s;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("cannot create {}: {e}", dir.display());
    }
    match Session::open(&dir) {
        Ok(mut s) => {
            // File › Options › Advanced › "Enable troubleshooting logging" (read at start-up).
            match mailcraft_engine::troubleshoot::start(&s.settings, &dir) {
                Ok(Some(path)) => log::info!("troubleshooting log: {}", path.display()),
                Ok(None) => {}
                Err(e) => eprintln!("mailcraft: {e}"),
            }
            // Account secrets live in the OS credential store where there is one; otherwise the profile's
            // unencrypted secrets file stays in use (Account Settings says so).
            match s.use_os_keyring() {
                Ok(at) => log::info!("account secrets: {at}"),
                Err(e) => log::warn!("OS keyring not used, secrets stay in the profile file: {e}"),
            }
            s
        }
        Err(e) => {
            let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local(
                mailcraft_engine::model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME,
            ));
            s.notify(mailcraft_engine::Notice::Error(format!("{e}. Changes will not be saved.")));
            s
        }
    }
}

/// Writes everything that outlives the window: queued changes folded into the profile's snapshot, and the
/// data files kept in files of their own. A session without a profile (the sample, or a profile that could
/// not be opened) still writes those data files, or what was added to them would be lost.
fn save_on_exit(s: &mut Session) {
    let own_files = s.mailbox.accounts.iter().any(|a| a.data_file.is_some());
    if (s.store.is_some() || own_files)
        && let Err(e) = s.run("file.exit", &serde_json::json!({}))
    {
        log::error!("saving on exit failed: {e}");
    }
}

/// eframe adapter.
struct MailCraft {
    app: App,
}

impl eframe::App for MailCraft {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui_root(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        save_on_exit(&mut self.app.s);
    }

    fn clear_color(&self, _visuals: &eframe::egui::Visuals) -> [f32; 4] {
        self.app.theme.backdrop.to_normalized_gamma_f32()
    }
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mailcraft: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("{}", version_line());
        return ExitCode::SUCCESS;
    }
    mailcraft_ui_egui::backstage::set_build_info(BUILD_INFO);

    let mut session = open_session(&args);
    // Quick Print goes straight to the default printer in the desktop app.
    session.direct_print = true;
    let mut app = App::new(session);
    if let Some(url) = &args.mailto {
        app.run("mail.newFromMailto", serde_json::json!({"url": url}));
    }
    file_dialogs::install(&mut app);
    clipboard::install();
    installed_fonts::install();
    capture::install();
    speech::install();
    app.ui.control_port = args.control_port.or_else(|| {
        std::env::var("MAILCRAFT_CONTROL_PORT")
            .ok()
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
    });

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(mailcraft_ui_egui::compose::MAIN_WINDOW_TITLE)
        .with_app_id("saint.dev.MailCraft")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size(mailcraft_ui_egui::theme::size::MIN_WINDOW)
        .with_decorations(true);
    if let Some((rgba, width, height)) = mailcraft_ui_egui::app_icon_rgba(256) {
        viewport = viewport.with_icon(eframe::egui::IconData { rgba, width, height });
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        "MailCraft",
        options,
        Box::new(move |cc| {
            App::install_fonts(&cc.egui_ctx);
            Ok(Box::new(MailCraft { app }))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailcraft: could not open the window: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn flags() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
        let a = parse(&["--profile", "/tmp/x", "--sample", "--control-port", "4321"]).unwrap();
        assert_eq!(a.profile, Some(PathBuf::from("/tmp/x")));
        assert!(a.sample);
        assert_eq!(a.control_port, Some(4321));
        assert_eq!(parse(&["--control-port=99"]).unwrap().control_port, Some(99));
        assert!(parse(&["--version"]).unwrap().version);
        assert!(parse(&["--control-port", "0"]).is_err());
        assert!(parse(&["--control-port", "x"]).is_err());
        assert!(parse(&["--profile"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        let m = parse(&["--sample", "mailto:ann@example.org?subject=Hi"]).unwrap();
        assert_eq!(m.mailto.as_deref(), Some("mailto:ann@example.org?subject=Hi"));
        assert!(parse(&["ann@example.org"]).is_err(), "only mailto: links are arguments");
    }

    #[test]
    fn version_line_names_version_and_build() {
        let v = version_line();
        assert!(v.starts_with(&format!("mailcraft {} (git ", env!("CARGO_PKG_VERSION"))), "{v}");
        assert!(v.ends_with(')'), "{v}");
    }

    #[test]
    fn closing_the_window_writes_data_files_of_their_own_without_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Projects.mcdata");
        let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local("Data"));
        assert!(s.store.is_none());
        let r = s.run("file.newDataFile", &serde_json::json!({"path": file.to_str().unwrap()})).unwrap();
        let id = mailcraft_engine::model::AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(id, mailcraft_engine::model::FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(mailcraft_engine::model::MailItem {
            folder: inbox,
            subject: "Added after it was made".into(),
            ..Default::default()
        });
        save_on_exit(&mut s);
        let saved = String::from_utf8(std::fs::read(&file).unwrap()).unwrap();
        assert!(saved.contains("Added after it was made"), "what was added reaches the file");
    }

    #[test]
    fn default_profile_is_named_mailcraft() {
        if let Some(p) = default_profile() {
            assert!(p.ends_with("MailCraft"));
        }
    }
}
