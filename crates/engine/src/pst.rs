//! PST import and Open Data File for PST files (issue #15).
//!
//! `file.openExport {"action": "importPst"}` imports a PST file into the folders of an account (the PST
//! file's default folders merge into the account's: its Inbox into the Inbox, and so on) or beneath a chosen
//! folder; `file.openDataFile` with a `.pst` file imports it into a new data file of its own, named after the
//! file, which Remove (`file.closeDataFile`) closes again like any other data file. MailCraft never writes
//! PST files: the PST file is only read, and what it holds is copied into the profile.
//!
//! **Never crash, never hang.** The file is read by `mailcraft_formats::pst` (see its documentation for
//! the caps and the panic guards) off the UI thread, and the results are applied a batch at a time by
//! [`poll`] from `Session::poll_sync`, so a large file never holds up the window and the user can stop it
//! (`file.importProgress {"cancel": true}`); what came in before the stop stays. In the desktop app and the
//! CLI the reader runs in a separate process ([`set_worker`], [`worker_main`]): a damaged file that makes
//! the reader fail in a way a thread cannot survive (running out of memory on a bogus length) ends only that
//! process, the import stops and reports it, and the app goes on. Without a worker program (tests, other
//! embedders) the reader runs on a thread under `catch_unwind`. The browser build has no files to read:
//! PST import is not offered there.
//!
//! Items that are already in the target folder are skipped unless `"duplicates": "allow"`, so an import
//! that was stopped can simply be run again: mail by Message-ID, calendar items by their meeting UID, and
//! anything else by what it holds (see [`is_repeat`]). The report lists what was skipped and why.
//!
//! What the import adds to folders that synchronise with a server is queued for the server like any other
//! local change (uploads and folder creates for IMAP, a push for JMAP), in the background as well as with
//! `wait`.

// The browser build keeps the types and the status queries but runs no import.
#![cfg_attr(target_arch = "wasm32", allow(unused_imports, dead_code))]

use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CmdError, Session};
use mailcraft_formats::pst::{PstFolder, PstItem, Report};
use mailcraft_model::{
    AccountId, Appointment, Body, Contact, FolderId, FolderKind, FolderRole, ItemId, MailItem, Note, Task,
};

/// Events a reader sends before its final report (folders and items in file order).
#[derive(Debug)]
pub(crate) enum Event {
    Folder(PstFolder),
    Item(usize, PstItem),
    /// Progress of a reader in another process (an in-process reader shares its counters directly).
    Progress(Progress),
    Done(Result<Report, String>),
}

/// How far a read is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub items_done: u64,
    pub items_total: u64,
    pub folders: u64,
    pub bytes_read: u64,
    pub file_bytes: u64,
}

/// Where the import goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The PST file's default folders merge into `account`'s; its other top folders go to the account's top.
    Account(AccountId),
    /// Every top folder of the PST file goes beneath this folder.
    Beneath(FolderId),
}

/// The state of the PST import that runs, or ran last.
#[derive(Debug, Default)]
pub struct PstJobs {
    next: u64,
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<Job>,
    /// The status of the last job, once it ended.
    last: Option<Value>,
}

/// A running import.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct Job {
    id: u64,
    path: PathBuf,
    target: Target,
    account: AccountId,
    /// A new data file was made for this import (Open Data File).
    new_data_file: bool,
    skip_duplicates: bool,
    rx: mpsc::Receiver<Event>,
    control: Arc<mailcraft_formats::pst::Control>,
    /// The reader process, when one runs.
    child: Arc<std::sync::Mutex<Option<std::process::Child>>>,
    progress: Progress,
    /// MailCraft folder of each PST folder (by index), or `None` when it could not be made.
    folders: Vec<Option<FolderId>>,
    imported: Counts,
    duplicates: usize,
    /// Fingerprints ([`Key`]) of what each target folder holds, gathered the first time an item goes there
    /// (and grown by what the import adds): items without a Message-ID are told apart by them.
    seen: std::collections::HashMap<FolderId, std::collections::HashSet<u64>>,
    /// What the import itself could not take (folders it could not make, items it could not store).
    notes: Vec<Value>,
    notes_count: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Job {
    fn drop(&mut self) {
        // A reader process must not outlive the import (or the app).
        cancel_job(self);
        if let Ok(mut g) = self.child.lock()
            && let Some(mut c) = g.take()
        {
            let _ = c.wait();
        }
    }
}

/// Whether an import into `account` runs (its data file cannot be removed meanwhile).
pub fn importing_into(s: &Session, account: AccountId) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.pst.job.as_ref().is_some_and(|j| j.account == account)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, account);
        false
    }
}

/// Items imported so far, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub mail: usize,
    pub appointments: usize,
    pub contacts: usize,
    pub tasks: usize,
    pub notes: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.mail
            .saturating_add(self.appointments)
            .saturating_add(self.contacts)
            .saturating_add(self.tasks)
            .saturating_add(self.notes)
    }
}

/// Most events applied per [`poll`] (the rest wait for the next frame).
const EVENTS_PER_POLL: usize = 200;
/// Events a reader may run ahead of [`poll`] (bounds the memory a large file takes in flight).
#[cfg(not(target_arch = "wasm32"))]
const CHANNEL_DEPTH: usize = 64;
/// Most notes kept in a job's report (the count goes on).
const MAX_NOTES: usize = 500;
/// Longest line read from a reader process (one message with its attachments, base64-encoded in JSON).
#[cfg(not(target_arch = "wasm32"))]
const MAX_WIRE_LINE: usize = 200 << 20;
/// The argument that runs a program as the PST reader ([`worker_main`]).
pub const WORKER_ARG: &str = "--pst-worker";

#[cfg(not(target_arch = "wasm32"))]
static WORKER: OnceLock<PathBuf> = OnceLock::new();

/// Makes PST imports read the file in a separate process: `exe` run with [`WORKER_ARG`] and the file's path
/// must call [`worker_main`]. Called once at start by the desktop app and the CLI.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_worker(exe: PathBuf) {
    let _ = WORKER.set(exe);
}

/// Whether `path` looks like a PST file (by its first bytes, else its extension).
pub fn is_pst(path: &Path) -> bool {
    let mut head = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head));
    match read {
        Ok(()) => head == mailcraft_formats::pst::MAGIC,
        Err(_) => path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pst")),
    }
}

/// Starts importing the PST file at `path`. With `wait` the import runs to its end (or `timeout`) before
/// this returns, and the result carries the report; otherwise it runs in the background and
/// `file.importProgress` reports on it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn start(
    s: &mut Session,
    path: &Path,
    target: Target,
    new_data_file: bool,
    skip_duplicates: bool,
    wait: Option<std::time::Duration>,
) -> Result<Value, CmdError> {
    if let Some(job) = &s.pst.job {
        return Err(CmdError::Disabled(format!(
            "a PST file is being imported already ({}); wait for it to finish or stop it",
            job.path.display()
        )));
    }
    let path = crate::datafile::absolute(path);
    let meta = std::fs::metadata(&path).map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(CmdError::Params(format!("{} is not a file", path.display())));
    }
    // Refuse what is plainly not a PST file, or a protected or unsupported one, before anything changes.
    let mut head = Vec::new();
    {
        use std::io::Read as _;
        let f =
            std::fs::File::open(&path).map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))?;
        f.take(600).read_to_end(&mut head).map_err(|e| CmdError::Failed(e.to_string()))?;
    }
    match mailcraft_formats::pst::sniff(&head) {
        None => return Err(CmdError::Failed(format!("{} is not a PST file", path.display()))),
        Some(Err(e)) => return Err(CmdError::Failed(e.to_string())),
        Some(Ok(_)) => {}
    }
    let account = match target {
        Target::Account(a) => a,
        Target::Beneath(f) => {
            s.mailbox.folder_account(f).ok_or_else(|| CmdError::Params(format!("no such folder {}", f.0)))?
        }
    };
    s.pst.next = s.pst.next.saturating_add(1);
    let id = s.pst.next;
    let control = Arc::new(mailcraft_formats::pst::Control::default());
    let child = Arc::new(std::sync::Mutex::new(None));
    let options = mailcraft_formats::pst::Options {
        now: s.now(),
        local_offset_min: s.tz_offset_min,
        ..Default::default()
    };
    let rx = match WORKER.get() {
        Some(exe) => spawn_process(exe, &path, options, &child, &control)?,
        None => spawn_thread(&path, options, &control)?,
    };
    s.pst.last = None;
    s.pst.job = Some(Job {
        id,
        path: path.clone(),
        target,
        account,
        new_data_file,
        skip_duplicates,
        rx,
        control,
        child,
        progress: Progress { file_bytes: meta.len(), ..Default::default() },
        folders: Vec::new(),
        imported: Counts::default(),
        duplicates: 0,
        seen: Default::default(),
        notes: Vec::new(),
        notes_count: 0,
    });
    let Some(timeout) = wait else {
        // The desktop app shows how far it is (and a Stop button) in a window of its own.
        s.notices.push(crate::Notice::OpenDialog("pstProgress".into(), json!({"job": id})));
        return Ok(
            json!({"job": id, "started": true, "path": path.display().to_string(), "account": account.0}),
        );
    };
    let deadline = std::time::Instant::now().checked_add(timeout);
    loop {
        // Inside the command that started it: the command's own change record takes the import.
        if !apply_ready(s, true, false) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        if s.pst.job.is_none() {
            break;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            cancel(s);
            // Take what arrived before the stop.
            while s.pst.job.is_some() {
                if !apply_ready(s, true, false) {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            break;
        }
    }
    Ok(s.pst.last.clone().unwrap_or_else(|| json!({"job": id})))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn start(
    _: &mut Session,
    _: &Path,
    _: Target,
    _: bool,
    _: bool,
    _: Option<std::time::Duration>,
) -> Result<Value, CmdError> {
    Err(CmdError::Disabled("PST files can be imported in the desktop app".into()))
}

/// A sink that sends what the reader reads to the session.
#[cfg(not(target_arch = "wasm32"))]
struct ToSession(mpsc::SyncSender<Event>);

#[cfg(not(target_arch = "wasm32"))]
impl mailcraft_formats::pst::Sink for ToSession {
    fn folder(&mut self, folder: PstFolder) -> bool {
        self.0.send(Event::Folder(folder)).is_ok()
    }
    fn item(&mut self, folder: usize, item: PstItem) -> bool {
        self.0.send(Event::Item(folder, item)).is_ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_thread(
    path: &Path,
    options: mailcraft_formats::pst::Options,
    control: &Arc<mailcraft_formats::pst::Control>,
) -> Result<mpsc::Receiver<Event>, CmdError> {
    let (tx, rx) = mpsc::sync_channel(CHANNEL_DEPTH);
    let path = path.to_path_buf();
    let control = Arc::clone(control);
    std::thread::Builder::new()
        .name("mailcraft-pst".into())
        .spawn(move || {
            let mut sink = ToSession(tx.clone());
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                mailcraft_formats::pst::read(
                    mailcraft_formats::pst::Source::Path(path),
                    &options,
                    &control,
                    &mut sink,
                )
            }));
            let done = match r {
                Ok(Ok(report)) => Ok(report),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("the PST reader failed on damaged data".to_owned()),
            };
            let _ = tx.send(Event::Done(done));
        })
        .map_err(|e| CmdError::Failed(format!("the import could not start: {e}")))?;
    Ok(rx)
}

/// One line of a reader process's output.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Wire {
    Folder {
        index: usize,
        parent: Option<usize>,
        name: String,
        class: String,
        kind: FolderKind,
        role: Option<FolderRole>,
        count: u32,
    },
    Mail {
        folder: usize,
        item: Box<MailItem>,
        /// The RFC 5322 source, base64.
        source: Option<String>,
        body: Option<Body>,
    },
    Appointment {
        folder: usize,
        item: Box<Appointment>,
    },
    Contact {
        folder: usize,
        item: Box<Contact>,
    },
    Task {
        folder: usize,
        item: Box<Task>,
    },
    Note {
        folder: usize,
        item: Box<Note>,
    },
    Progress(Progress),
    Done {
        report: WireReport,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WireReport {
    name: String,
    folders: usize,
    mail: usize,
    appointments: usize,
    contacts: usize,
    tasks: usize,
    notes: usize,
    skipped: Vec<(String, String, String)>,
    skipped_count: usize,
    stopped: Option<String>,
}

impl From<&Report> for WireReport {
    fn from(r: &Report) -> Self {
        WireReport {
            name: r.name.clone(),
            folders: r.folders,
            mail: r.mail,
            appointments: r.appointments,
            contacts: r.contacts,
            tasks: r.tasks,
            notes: r.notes,
            skipped: r.skipped.iter().map(|s| (s.folder.clone(), s.item.clone(), s.reason.clone())).collect(),
            skipped_count: r.skipped_count,
            stopped: r.stopped.clone(),
        }
    }
}

impl From<WireReport> for Report {
    fn from(w: WireReport) -> Self {
        Report {
            name: w.name,
            folders: w.folders,
            mail: w.mail,
            appointments: w.appointments,
            contacts: w.contacts,
            tasks: w.tasks,
            notes: w.notes,
            skipped: w
                .skipped
                .into_iter()
                .take(mailcraft_formats::pst::MAX_SKIPPED_LISTED)
                .map(|(folder, item, reason)| mailcraft_formats::pst::Skipped { folder, item, reason })
                .collect(),
            skipped_count: w.skipped_count,
            stopped: w.stopped,
        }
    }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl Wire {
    fn of_folder(f: PstFolder) -> Wire {
        Wire::Folder {
            index: f.index,
            parent: f.parent,
            name: f.name,
            class: f.class,
            kind: f.kind,
            role: f.role,
            count: f.count,
        }
    }

    fn of_item(folder: usize, item: PstItem) -> Wire {
        use base64::Engine as _;
        match item {
            PstItem::Mail(m) => {
                let (item, content) = *m;
                Wire::Mail {
                    folder,
                    item: Box::new(item),
                    source: content.source.map(|s| b64().encode(s)),
                    body: content.body,
                }
            }
            PstItem::Appointment(item) => Wire::Appointment { folder, item },
            PstItem::Contact(item) => Wire::Contact { folder, item },
            PstItem::Task(item) => Wire::Task { folder, item },
            PstItem::Note(item) => Wire::Note { folder, item },
        }
    }

    fn into_event(self) -> Result<Event, String> {
        use base64::Engine as _;
        Ok(match self {
            Wire::Folder { index, parent, name, class, kind, role, count } => {
                Event::Folder(PstFolder { index, parent, name, class, kind, role, count })
            }
            Wire::Mail { folder, item, source, body } => {
                let source = match source {
                    Some(s) => {
                        Some(b64().decode(s).map_err(|e| format!("the reader sent a damaged message: {e}"))?)
                    }
                    None => None,
                };
                Event::Item(
                    folder,
                    PstItem::Mail(Box::new((*item, mailcraft_store::Content { source, body }))),
                )
            }
            Wire::Appointment { folder, item } => Event::Item(folder, PstItem::Appointment(item)),
            Wire::Contact { folder, item } => Event::Item(folder, PstItem::Contact(item)),
            Wire::Task { folder, item } => Event::Item(folder, PstItem::Task(item)),
            Wire::Note { folder, item } => Event::Item(folder, PstItem::Note(item)),
            Wire::Progress(p) => Event::Progress(p),
            Wire::Done { report } => Event::Done(Ok(report.into())),
            Wire::Error { message } => Event::Done(Err(message)),
        })
    }
}

/// Runs the PST reader in this process and writes what it reads to standard output, one JSON object per
/// line (the desktop app and the CLI call this when started with [`WORKER_ARG`]). `args` holds the file's
/// path. Returns the exit code.
#[cfg(not(target_arch = "wasm32"))]
pub fn worker_main(args: &[String]) -> i32 {
    use std::io::Write as _;
    let Some(path) = args.first() else {
        eprintln!("{WORKER_ARG} needs the path of a PST file");
        return 2;
    };
    let offset = args.get(1).and_then(|o| o.parse::<i32>().ok()).unwrap_or(0).clamp(-24 * 60, 24 * 60);
    let now = args.get(2).and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    struct Lines<'a, W: std::io::Write> {
        out: &'a mut W,
        control: Arc<mailcraft_formats::pst::Control>,
        sent: u64,
    }
    impl<W: std::io::Write> Lines<'_, W> {
        fn line(&mut self, w: &Wire) -> bool {
            let Ok(text) = serde_json::to_string(w) else { return true };
            writeln!(self.out, "{text}").is_ok()
        }
        fn progress(&mut self) -> bool {
            use std::sync::atomic::Ordering::Relaxed;
            let c = &self.control;
            let p = Progress {
                items_done: c.items_done.load(Relaxed),
                items_total: c.items_total.load(Relaxed),
                folders: c.folders_done.load(Relaxed),
                bytes_read: c.bytes_read.load(Relaxed),
                file_bytes: c.file_bytes.load(Relaxed),
            };
            self.line(&Wire::Progress(p)) && self.out.flush().is_ok()
        }
    }
    impl<W: std::io::Write> mailcraft_formats::pst::Sink for Lines<'_, W> {
        fn folder(&mut self, folder: PstFolder) -> bool {
            self.line(&Wire::of_folder(folder)) && self.progress()
        }
        fn item(&mut self, folder: usize, item: PstItem) -> bool {
            self.sent = self.sent.saturating_add(1);
            self.line(&Wire::of_item(folder, item)) && (!self.sent.is_multiple_of(32) || self.progress())
        }
    }
    let control = Arc::new(mailcraft_formats::pst::Control::default());
    let options = mailcraft_formats::pst::Options {
        now: mailcraft_model::DateTime::from_unix(now, offset),
        local_offset_min: offset,
        ..Default::default()
    };
    let mut lines = Lines { out: &mut out, control: Arc::clone(&control), sent: 0 };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mailcraft_formats::pst::read(
            mailcraft_formats::pst::Source::Path(PathBuf::from(path)),
            &options,
            &control,
            &mut lines,
        )
    }));
    let last = match result {
        Ok(Ok(report)) => Wire::Done { report: WireReport::from(&report) },
        Ok(Err(e)) => Wire::Error { message: e.to_string() },
        Err(_) => Wire::Error { message: "the PST reader failed on damaged data".into() },
    };
    lines.progress();
    let ok = lines.line(&last);
    if !ok || out.flush().is_err() {
        return 1;
    }
    0
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_process(
    exe: &Path,
    path: &Path,
    options: mailcraft_formats::pst::Options,
    slot: &Arc<std::sync::Mutex<Option<std::process::Child>>>,
    control: &Arc<mailcraft_formats::pst::Control>,
) -> Result<mpsc::Receiver<Event>, CmdError> {
    use std::io::BufRead as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg(WORKER_ARG)
        .arg(path)
        .arg(options.local_offset_min.to_string())
        .arg(options.now.unix.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CmdError::Failed(format!("the PST reader could not start: {e}")))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(CmdError::Failed("the PST reader could not start".into()));
    };
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(child);
    }
    let (tx, rx) = mpsc::sync_channel(CHANNEL_DEPTH);
    let slot = Arc::clone(slot);
    let control = Arc::clone(control);
    std::thread::Builder::new()
        .name("mailcraft-pst-reader".into())
        .spawn(move || {
            let mut input = std::io::BufReader::new(stdout);
            let mut end = End::Closed;
            let mut folders: u64 = 0;
            let mut line = Vec::new();
            loop {
                line.clear();
                // Lines are capped: a reader that sends an endless line is stopped.
                let read = std::io::Read::take(&mut input, MAX_WIRE_LINE as u64).read_until(b'\n', &mut line);
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line.last() != Some(&b'\n') && line.len() >= MAX_WIRE_LINE {
                    end = End::Bad("the PST reader sent an item that is too large".into());
                    break;
                }
                let event = serde_json::from_slice::<Wire>(&line)
                    .map_err(|e| format!("the PST reader sent something unreadable: {e}"))
                    .and_then(Wire::into_event);
                match event {
                    Ok(ev) => {
                        if let Event::Progress(p) = &ev {
                            folders = p.folders;
                        }
                        let last = matches!(ev, Event::Done(_));
                        if tx.send(ev).is_err() {
                            end = End::Gone;
                            break;
                        }
                        if last {
                            end = End::Finished;
                            break;
                        }
                    }
                    Err(e) => {
                        end = End::Bad(e);
                        break;
                    }
                }
            }
            // A reader that sent its last line is waited for; any other is stopped first (one that sent a
            // bad line may still be writing, and would block on the full pipe for good).
            let status = slot.lock().ok().and_then(|mut g| g.take()).and_then(|mut c| {
                if !matches!(end, End::Finished) {
                    let _ = c.kill();
                }
                c.wait().ok()
            });
            let why = match end {
                End::Finished | End::Gone => return,
                // A stop kills the reader: whatever its output looks like then, the import was stopped.
                _ if control.cancelled() => {
                    let report = Report {
                        folders: usize::try_from(folders).unwrap_or(usize::MAX),
                        stopped: Some("cancelled".into()),
                        ..Default::default()
                    };
                    let _ = tx.send(Event::Done(Ok(report)));
                    return;
                }
                End::Bad(why) => why,
                End::Closed => match status {
                    Some(st) if st.success() => "the PST reader stopped before it finished".to_owned(),
                    Some(st) => format!(
                        "the PST reader stopped on damaged data ({st}); what was read before that was imported"
                    ),
                    None => "the import was stopped".to_owned(),
                },
            };
            let _ = tx.send(Event::Done(Err(why)));
        })
        .map_err(|e| CmdError::Failed(format!("the import could not start: {e}")))?;
    Ok(rx)
}

/// How a reader process's output ended.
#[cfg(not(target_arch = "wasm32"))]
enum End {
    /// Its last line (a report or an error) came.
    Finished,
    /// The import no longer listens.
    Gone,
    /// It sent a line it cannot have meant.
    Bad(String),
    /// Its output closed before the last line.
    Closed,
}

/// Asks the running import to stop. True when one was running.
pub fn cancel(s: &mut Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(job) = &s.pst.job {
        cancel_job(job);
        return true;
    }
    let _ = s;
    false
}

/// The running import's status, else the last one's (`None` when there was none).
pub fn status(s: &Session) -> Option<Value> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(job) = &s.pst.job {
        return Some(job_status(s, job, true, None));
    }
    s.pst.last.clone()
}

/// True while an import runs.
pub fn running(s: &Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.pst.job.is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn job_status(s: &Session, job: &Job, running: bool, report: Option<&Result<Report, String>>) -> Value {
    use std::sync::atomic::Ordering::Relaxed;
    let c = &job.control;
    // An in-process reader shares its counters; a reader process sends them.
    let p = if c.file_bytes.load(Relaxed) > 0 {
        Progress {
            items_done: c.items_done.load(Relaxed),
            items_total: c.items_total.load(Relaxed),
            folders: c.folders_done.load(Relaxed),
            bytes_read: c.bytes_read.load(Relaxed),
            file_bytes: c.file_bytes.load(Relaxed),
        }
    } else {
        job.progress
    };
    let account = s.mailbox.account(job.account).map(|a| a.display_name.clone()).unwrap_or_default();
    let mut v = json!({
        "job": job.id,
        "running": running,
        "path": job.path.display().to_string(),
        "account": job.account.0,
        "accountName": account,
        "newDataFile": job.new_data_file,
        "into": match job.target {
            Target::Account(_) => json!("account"),
            Target::Beneath(f) => json!(f.0),
        },
        "progress": p,
        "imported": job.imported.total(),
        "counts": job.imported,
        "duplicates": job.duplicates,
    });
    let mut skipped: Vec<Value> = job.notes.clone();
    let mut skipped_count = job.notes_count;
    match report {
        Some(Ok(r)) => {
            skipped.extend(
                r.skipped.iter().map(|k| json!({"folder": k.folder, "item": k.item, "reason": k.reason})),
            );
            skipped_count = skipped_count.saturating_add(r.skipped_count);
            put(&mut v, "name", json!(r.name));
            put(&mut v, "folders", json!(r.folders));
            if let Some(st) = &r.stopped {
                put(&mut v, "stopped", json!(st));
            }
        }
        Some(Err(e)) => put(&mut v, "error", json!(e)),
        None => {}
    }
    put(&mut v, "skipped", Value::Array(skipped));
    put(&mut v, "skippedCount", json!(skipped_count));
    v
}

/// Applies what the reader has sent (called by `Session::poll_sync`). True when the mailbox changed.
pub(crate) fn poll(s: &mut Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        apply_ready(s, false, true)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        false
    }
}

/// Applies up to [`EVENTS_PER_POLL`] events (all that are ready when `drain`). True when any were applied.
/// With `own_record` the changes are recorded and journaled here (a poll between commands); without, the
/// command that runs records them.
#[cfg(not(target_arch = "wasm32"))]
fn apply_ready(s: &mut Session, drain: bool, own_record: bool) -> bool {
    let Some(mut job) = s.pst.job.take() else { return false };
    let mut events = Vec::new();
    let mut lost = false;
    while drain || events.len() < EVENTS_PER_POLL {
        match job.rx.try_recv() {
            Ok(e) => events.push(e),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                lost = true;
                break;
            }
        }
        if drain && events.len() >= EVENTS_PER_POLL * 10 {
            break;
        }
    }
    if events.is_empty() && !lost {
        s.pst.job = Some(job);
        return false;
    }
    let recording = own_record.then(|| s.begin_changes());
    let mut finished: Option<Result<Report, String>> = None;
    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for ev in events {
            match ev {
                Event::Folder(f) => add_folder(s, &mut job, f),
                Event::Item(folder, item) => add_item(s, &mut job, folder, item),
                Event::Progress(p) => job.progress = p,
                Event::Done(r) => finished = Some(r),
            }
        }
    }));
    match (applied, recording) {
        (Err(payload), Some(recording)) => {
            log::error!("applying a PST import panicked; the batch was undone and the import stopped");
            s.rollback_changes(recording);
            s.mailbox_changed();
            cancel_job(&job);
            finished = Some(Err("the import stopped: MailCraft could not file what it read".into()));
            drop(payload);
        }
        (Err(payload), None) => {
            // Inside a command: what was filed before the failure stays (the command journals it).
            log::error!("applying a PST import panicked; the import stopped");
            cancel_job(&job);
            finished = Some(Err("the import stopped: MailCraft could not file what it read".into()));
            drop(payload);
            s.mailbox_changed();
        }
        (Ok(()), Some(recording)) => {
            let mut changes = s.end_changes(recording);
            // New folders and mail in synchronised folders become server changes (uploads, folder creates;
            // JMAP accounts are marked for a push), as after a command.
            crate::sync::queue_local_changes(s, &mut changes);
            let ops = changes.ops(&s.mailbox);
            s.journal(ops);
            s.mailbox_changed();
        }
        (Ok(()), None) => s.mailbox_changed(),
    }
    if finished.is_none() && lost {
        finished =
            Some(Err("the PST reader stopped unexpectedly; what was read before that was imported".into()));
    }
    match finished {
        Some(r) => {
            let st = job_status(s, &job, false, Some(&r));
            let text = summary(&st);
            let report = r.is_err() || st.get("skippedCount").and_then(Value::as_u64).unwrap_or(0) > 0;
            s.notices.push(match &r {
                Ok(_) => crate::Notice::Info(text),
                Err(_) => crate::Notice::Error(text),
            });
            // What was left out, and why, is in the progress window: it opens again (the window was maybe
            // hidden while the import ran) when there is something to read there.
            if report {
                s.notices.push(crate::Notice::OpenDialog("pstProgress".into(), json!({"job": job.id})));
            }
            s.pst.last = Some(st);
        }
        None => s.pst.job = Some(job),
    }
    s.tidy_selection();
    true
}

/// Stops a job's reader. The cancel flag is set first: a reader process that is killed ends its output
/// mid-way, and the flag tells its reading thread that this was a stop, not damage.
#[cfg(not(target_arch = "wasm32"))]
fn cancel_job(job: &Job) {
    job.control.cancel();
    if let Ok(mut g) = job.child.lock()
        && let Some(c) = g.as_mut()
    {
        let _ = c.kill();
    }
}

/// Sets `key` of a JSON object (no-op on anything else).
pub(crate) fn put(v: &mut Value, key: &str, value: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), value);
    }
}

/// One line saying what an import did.
pub fn summary(st: &Value) -> String {
    let n = |k: &str| st.get(k).and_then(Value::as_u64).unwrap_or(0);
    let c = |k: &str| st.get("counts").and_then(|c| c.get(k)).and_then(Value::as_u64).unwrap_or(0);
    let file = st
        .get("path")
        .and_then(Value::as_str)
        .map(|p| Path::new(p).file_name().and_then(|f| f.to_str()).unwrap_or(p).to_owned())
        .unwrap_or_default();
    let mut parts = Vec::new();
    for (k, one, many) in [
        ("mail", "message", "messages"),
        ("appointments", "calendar item", "calendar items"),
        ("contacts", "contact", "contacts"),
        ("tasks", "task", "tasks"),
        ("notes", "note", "notes"),
    ] {
        let v = c(k);
        if v > 0 {
            parts.push(format!("{v} {}", if v == 1 { one } else { many }));
        }
    }
    let what = if parts.is_empty() { "nothing".to_owned() } else { parts.join(", ") };
    let mut text = match st.get("error").and_then(Value::as_str) {
        Some(e) => format!("Importing {file} stopped: {e}. Imported {what}."),
        None if st.get("stopped").and_then(Value::as_str) == Some("cancelled") => {
            format!("Import of {file} stopped. Imported {what}.")
        }
        None => format!("Imported {what} from {file}."),
    };
    if n("duplicates") > 0 {
        text.push_str(&format!(" {} already there were left out.", n("duplicates")));
    }
    if n("skippedCount") > 0 {
        text.push_str(&format!(" {} could not be imported (see the report).", n("skippedCount")));
    }
    text
}

#[cfg(not(target_arch = "wasm32"))]
fn note(job: &mut Job, folder: &str, item: &str, reason: impl Into<String>) {
    job.notes_count = job.notes_count.saturating_add(1);
    if job.notes.len() < MAX_NOTES {
        job.notes.push(json!({"folder": folder, "item": item, "reason": reason.into()}));
    }
}

/// An existing child of `parent` (or of the account's top when `None`) named `name` holding `kind` items.
#[cfg(not(target_arch = "wasm32"))]
fn find_child(
    s: &Session,
    account: AccountId,
    parent: Option<FolderId>,
    name: &str,
    kind: FolderKind,
) -> Option<FolderId> {
    s.mailbox
        .folders
        .values()
        .find(|f| {
            f.account == account
                && f.parent == parent
                && f.kind == kind
                && f.name.eq_ignore_ascii_case(name.trim())
        })
        .map(|f| f.id)
}

/// Makes (or finds) the folder for PST folder `f`.
#[cfg(not(target_arch = "wasm32"))]
fn add_folder(s: &mut Session, job: &mut Job, f: PstFolder) {
    let index = f.index;
    let parent = match f.parent {
        Some(p) => match job.folders.get(p).copied().flatten() {
            Some(id) => Some(id),
            None => {
                note(job, &f.name, "", "its parent folder could not be made; it was left out");
                set_folder(job, index, None);
                return;
            }
        },
        None => match job.target {
            Target::Account(_) => None,
            Target::Beneath(id) => Some(id),
        },
    };
    let account = job.account;
    // The PST file's default folders go into the account's own.
    if f.parent.is_none()
        && matches!(job.target, Target::Account(_))
        && let Some(role) = f.role
        && let Some(id) = s.mailbox.role_folder(account, role)
    {
        set_folder(job, index, Some(id));
        return;
    }
    if let Some(id) = find_child(s, account, parent, &f.name, f.kind) {
        set_folder(job, index, Some(id));
        return;
    }
    let mut name = f.name.clone();
    let mut made = None;
    for k in 1u32..50 {
        if k > 1 {
            name = format!("{} ({k})", f.name);
        }
        let r = match parent {
            Some(p) => s.mailbox.create_folder_of_kind(p, &name, Some(f.kind)),
            None => s.mailbox.create_folder_in(account, None, &name, f.kind),
        };
        match r {
            Ok(id) => {
                made = Some(id);
                break;
            }
            Err(mailcraft_model::ModelError::Invalid(m)) if m.contains("already exists") => continue,
            Err(e) => {
                note(job, &f.name, "", format!("the folder could not be made ({e})"));
                break;
            }
        }
    }
    set_folder(job, index, made);
}

#[cfg(not(target_arch = "wasm32"))]
fn set_folder(job: &mut Job, index: usize, id: Option<FolderId>) {
    if index >= mailcraft_formats::pst::MAX_FOLDERS {
        return;
    }
    if job.folders.len() <= index {
        job.folders.resize(index.saturating_add(1), None);
    }
    if let Some(slot) = job.folders.get_mut(index) {
        *slot = id;
    }
}

/// The folder an item of `kind` goes to: its PST folder's, else the account's default folder of that kind.
#[cfg(not(target_arch = "wasm32"))]
fn folder_for(s: &Session, job: &Job, folder: usize, kind: FolderKind) -> Option<FolderId> {
    let mapped = job.folders.get(folder).copied().flatten();
    if let Some(id) = mapped
        && s.mailbox.folder(id).is_some_and(|f| f.kind == kind)
    {
        return Some(id);
    }
    let role = match kind {
        FolderKind::Calendar => FolderRole::Calendar,
        FolderKind::Contacts => FolderRole::Contacts,
        FolderKind::Tasks => FolderRole::Tasks,
        FolderKind::Notes => FolderRole::Notes,
        _ => FolderRole::Inbox,
    };
    s.mailbox.role_folder(job.account, role).or_else(|| {
        s.mailbox.folders.values().find(|f| f.account == job.account && f.kind == kind).map(|f| f.id)
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn add_item(s: &mut Session, job: &mut Job, folder: usize, item: PstItem) {
    let path =
        job.folders.get(folder).copied().flatten().map(|f| s.mailbox.folder_path(f)).unwrap_or_default();
    match item {
        PstItem::Mail(m) => {
            let (mut item, content) = *m;
            let Some(target) = folder_for(s, job, folder, FolderKind::Mail) else {
                note(job, &path, &item.subject, "there is no mail folder to put it in");
                return;
            };
            // Mail with a Message-ID is a repeat when the folder holds that Message-ID; other mail when the
            // folder holds one like it.
            let keys = if item.message_id.is_some() { Vec::new() } else { mail_keys(&item) };
            if job.skip_duplicates {
                let repeat = match item.message_id.as_deref() {
                    Some(mid) => s
                        .mailbox
                        .mail
                        .by_message_id(mid)
                        .iter()
                        .any(|id| s.mailbox.mail.get(*id).is_some_and(|m| m.folder == target)),
                    None => is_repeat(s, job, target, &keys),
                };
                if repeat {
                    job.duplicates = job.duplicates.saturating_add(1);
                    return;
                }
            }
            item.folder = target;
            if let Err(e) = s.store_content(&mut item, &content) {
                note(job, &path, &item.subject, format!("its content could not be stored ({e})"));
                return;
            }
            match mailcraft_store::import::insert_item(&mut s.mailbox, item) {
                Ok(_) => {
                    job.imported.mail = job.imported.mail.saturating_add(1);
                    remember(job, target, keys);
                }
                Err(e) => note(job, &path, "", format!("it could not be filed ({e})")),
            }
        }
        PstItem::Appointment(mut a) => {
            let Some(target) = folder_for(s, job, folder, FolderKind::Calendar) else {
                note(job, &path, &a.subject, "there is no calendar to put it in");
                return;
            };
            let keys = appointment_keys(&a);
            if job.skip_duplicates && is_repeat(s, job, target, &keys) {
                job.duplicates = job.duplicates.saturating_add(1);
                return;
            }
            let id = ItemId(s.mailbox.alloc_id());
            a.id = id;
            a.folder = target;
            s.mailbox.appointments.insert(id, *a);
            job.imported.appointments = job.imported.appointments.saturating_add(1);
            remember(job, target, keys);
        }
        PstItem::Contact(mut c) => {
            let Some(target) = folder_for(s, job, folder, FolderKind::Contacts) else {
                note(job, &path, &c.file_as, "there is no contacts folder to put it in");
                return;
            };
            let keys = contact_keys(&c);
            if job.skip_duplicates && is_repeat(s, job, target, &keys) {
                job.duplicates = job.duplicates.saturating_add(1);
                return;
            }
            let id = ItemId(s.mailbox.alloc_id());
            c.id = id;
            c.folder = target;
            s.mailbox.contacts.insert(id, *c);
            job.imported.contacts = job.imported.contacts.saturating_add(1);
            remember(job, target, keys);
        }
        PstItem::Task(mut t) => {
            let Some(target) = folder_for(s, job, folder, FolderKind::Tasks) else {
                note(job, &path, &t.subject, "there is no task list to put it in");
                return;
            };
            let keys = task_keys(&t);
            if job.skip_duplicates && is_repeat(s, job, target, &keys) {
                job.duplicates = job.duplicates.saturating_add(1);
                return;
            }
            let id = ItemId(s.mailbox.alloc_id());
            t.id = id;
            t.folder = target;
            s.mailbox.tasks.insert(id, *t);
            job.imported.tasks = job.imported.tasks.saturating_add(1);
            remember(job, target, keys);
        }
        PstItem::Note(mut n) => {
            let Some(target) = folder_for(s, job, folder, FolderKind::Notes) else {
                note(job, &path, "", "there is no notes folder to put it in");
                return;
            };
            let keys = note_keys(&n);
            if job.skip_duplicates && is_repeat(s, job, target, &keys) {
                job.duplicates = job.duplicates.saturating_add(1);
                return;
            }
            let id = ItemId(s.mailbox.alloc_id());
            n.id = id;
            n.folder = target;
            s.mailbox.notes.insert(id, *n);
            job.imported.notes = job.imported.notes.saturating_add(1);
            remember(job, target, keys);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Repeats: what tells an item that came in before apart from a new one. PST items carry no id that the
// profile keeps (calendar items made a UID of their own unless they had one), so items without a Message-ID
// or a UID of their own are compared by what they hold (the fields that a rerun reads the same way).

#[cfg(not(target_arch = "wasm32"))]
fn hash_of(v: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher as _;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[cfg(not(target_arch = "wasm32"))]
fn addr_list(list: &[mailcraft_mime::Mailbox]) -> Vec<String> {
    list.iter().map(|m| m.addr.to_ascii_lowercase()).collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn mail_keys(m: &MailItem) -> Vec<u64> {
    let attachments: Vec<(&str, u64)> = m.attachments.iter().map(|a| (a.filename.as_str(), a.size)).collect();
    vec![hash_of((
        "mail",
        m.subject.trim(),
        m.from.as_ref().map(|f| f.addr.to_ascii_lowercase()),
        m.sent.map(|d| d.unix),
        addr_list(&m.to),
        addr_list(&m.cc),
        attachments,
        m.is_draft,
        m.preview.trim(),
    ))]
}

#[cfg(not(target_arch = "wasm32"))]
fn appointment_keys(a: &Appointment) -> Vec<u64> {
    let mut keys = vec![hash_of((
        "appointment",
        a.subject.trim(),
        a.start.unix,
        a.end.unix,
        a.all_day,
        a.location.trim(),
        a.recurrence.as_ref().map(|r| r.rrule.as_str()),
    ))];
    // The same meeting (its UID came from the file) is the same item whatever was changed since.
    if !a.uid.is_empty() && !a.uid.ends_with("@mailcraft.invalid") {
        keys.push(hash_of(("uid", a.uid.as_str(), a.recurrence_id.map(|d| d.unix))));
    }
    keys
}

#[cfg(not(target_arch = "wasm32"))]
fn contact_keys(c: &Contact) -> Vec<u64> {
    let emails: Vec<String> = c.emails.iter().map(|e| e.to_ascii_lowercase()).collect();
    vec![hash_of((
        "contact",
        c.file_as.trim(),
        c.first_name.trim(),
        c.middle_name.trim(),
        c.last_name.trim(),
        c.company.trim(),
        emails,
    ))]
}

#[cfg(not(target_arch = "wasm32"))]
fn task_keys(t: &Task) -> Vec<u64> {
    vec![hash_of(("task", t.subject.trim(), t.start.map(|d| d.unix), t.due.map(|d| d.unix), t.body.trim()))]
}

#[cfg(not(target_arch = "wasm32"))]
fn note_keys(n: &Note) -> Vec<u64> {
    vec![hash_of(("note", n.text.trim()))]
}

/// Whether `target` holds an item with one of `keys` (its fingerprints are gathered on first use).
#[cfg(not(target_arch = "wasm32"))]
fn is_repeat(s: &Session, job: &mut Job, target: FolderId, keys: &[u64]) -> bool {
    if keys.is_empty() {
        return false;
    }
    let known = job.seen.entry(target).or_insert_with(|| {
        let mb = &s.mailbox;
        let mut set = std::collections::HashSet::new();
        for m in mb.items_in(target) {
            set.extend(mail_keys(m));
        }
        for a in mb.appointments.values().filter(|a| a.folder == target) {
            set.extend(appointment_keys(a));
        }
        for c in mb.contacts.values().filter(|c| c.folder == target) {
            set.extend(contact_keys(c));
        }
        for t in mb.tasks.values().filter(|t| t.folder == target) {
            set.extend(task_keys(t));
        }
        for n in mb.notes.values().filter(|n| n.folder == target) {
            set.extend(note_keys(n));
        }
        set
    });
    keys.iter().any(|k| known.contains(k))
}

/// Adds what was just imported into `target` to its fingerprints (when they were gathered).
#[cfg(not(target_arch = "wasm32"))]
fn remember(job: &mut Job, target: FolderId, keys: Vec<u64>) {
    if let Some(known) = job.seen.get_mut(&target) {
        known.extend(keys);
    }
}

#[cfg(test)]
#[path = "pst_tests.rs"]
mod tests;
