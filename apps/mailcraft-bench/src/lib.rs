//! MailCraft storage benchmarks (storage ADR, section 6.1).
//!
//! A run builds a synthetic profile ([`mailcraft_testmail::synth`]) of the requested size on a storage
//! backend, opens it the way the app does and measures what grows with mailbox size: open time, memory, folder
//! switches, list windows, search, applying synchronised mail and the common commands. The result is one JSON
//! document ([`run`], `cargo xtask bench`).
//!
//! Only the `legacy` backend exists so far (the JSON snapshot, journal and blobs of `mailcraft-store`); the
//! `sqlite` backend arrives with the new store (stage S6). Nothing here contacts a server: synchronised mail
//! goes through [`mailcraft_engine::Session::queue_imap_results`].
//!
//! Timings are wall-clock milliseconds; memory figures are kilobytes from `/proc/self/status` (Linux only,
//! `null` elsewhere). Measuring in a fresh process ([`measure`], which `mailcraft-bench run` starts for
//! itself) keeps the generator's memory out of the figures.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mailcraft_engine::model::{
    Account, AccountKind, DateTime, FlagState, FocusClass, FolderId, FolderKind, FolderRole, ItemId,
    MailItem, Mailbox,
};
use mailcraft_engine::sync::SyntheticImap;
use mailcraft_engine::{Session, queries, search_index};
use mailcraft_net::sync::{FolderState, NewMessage, SyncDelta};
use mailcraft_testmail::synth::{self, BodyKind, Config, FOLDERS, Rng, Role};
use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, String>;

/// Version of the JSON layout (bump when a field changes meaning).
pub const SCHEMA: u64 = 1;
/// Rows in one message-list window (the visible part of a tall list).
pub const WINDOW_ROWS: usize = 50;
/// Messages per synchronised batch (the IMAP worker's `max_new_per_sync`).
pub const SYNC_BATCH: usize = 500;
/// Display name of the IMAP account the sync measurements fill (it has no server).
pub const SYNC_ACCOUNT: &str = "Bench IMAP (no server)";
/// Server name of the folder header-only downloads go to.
pub const HEADERS_FOLDER: &str = "Headers";
/// Seed offset of the synchronised messages (other Message-IDs than the profile's).
const SYNC_SEED: u64 = 0x5359_4e43;

/// The fixed search queries (ADR 6.1): rare and common terms, prefixes, people, phrases, dates, attachments.
pub const QUERIES: [&str; 20] = [
    synth::RARE_TERM,
    "meeting",
    "budget",
    "pr",
    "co",
    "from:ann",
    "from:ann.lee@example.com",
    "\"quarterly budget review\"",
    "\"please review\"",
    "received:\"last week\" meeting",
    "received:\"last month\"",
    "hasattachments:yes",
    "hasattachments:yes report",
    "isread:no",
    "isflagged:yes",
    "category:\"Red category\"",
    "subject:invoice",
    "to:bench.user@example.com",
    "garden OR bicycle",
    "project -budget",
];

/// Storage backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `mailbox.json` + `journal.jsonl` + `blobs/` (mailcraft-store today).
    Legacy,
    /// The SQLite store of the storage ADR (stage S6; not built yet).
    Sqlite,
}

impl Backend {
    pub fn parse(s: &str) -> Result<Backend> {
        match s {
            "legacy" => Ok(Backend::Legacy),
            "sqlite" => Ok(Backend::Sqlite),
            other => Err(format!("unknown backend `{other}` (legacy or sqlite)")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backend::Legacy => "legacy",
            Backend::Sqlite => "sqlite",
        }
    }

    fn check(self) -> Result<()> {
        match self {
            Backend::Legacy => Ok(()),
            Backend::Sqlite => Err("the sqlite backend does not exist yet (storage stage S6)".into()),
        }
    }
}

/// What one benchmark run does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Messages in the profile.
    pub messages: usize,
    /// Bodies of a few kilobytes ([`Config::small`]).
    pub small_bodies: bool,
    pub seed: u64,
    pub backend: Backend,
    /// Messages applied as synchronised mail, first whole, then as headers followed by their bodies.
    pub sync_messages: usize,
    /// Messages moved by the bulk move (then undone and redone); at most the largest folder less one.
    pub bulk_move: usize,
    /// List windows fetched at random offsets.
    pub windows: usize,
}

impl Options {
    pub fn new(messages: usize) -> Options {
        Options {
            messages,
            small_bodies: false,
            seed: synth::DEFAULT_SEED,
            backend: Backend::Legacy,
            sync_messages: 10_000,
            bulk_move: 10_000,
            windows: 50,
        }
    }

    /// The generator configuration of the profile.
    pub fn config(&self) -> Config {
        Config { seed: self.seed, small_bodies: self.small_bodies, ..Config::new(self.messages) }
    }

    fn now(&self) -> DateTime {
        DateTime { unix: self.config().now, offset_min: 0 }
    }
}

/// `10k`, `40k`, `1m`, `2000`… as a message count.
pub fn parse_size(s: &str) -> Result<usize> {
    let t = s.trim().to_ascii_lowercase();
    let (digits, mul) = if let Some(d) = t.strip_suffix('k') {
        (d, 1_000)
    } else if let Some(d) = t.strip_suffix('m') {
        (d, 1_000_000)
    } else {
        (t.as_str(), 1)
    };
    let n: usize = digits.parse().map_err(|_| format!("`{s}` is not a size (like 2000, 10k, 100k or 1m)"))?;
    let n = n.checked_mul(mul).ok_or_else(|| format!("`{s}` is too large"))?;
    if n == 0 || n > synth::MAX_MESSAGES {
        return Err(format!("`{s}`: the size must be 1 to {} messages", synth::MAX_MESSAGES));
    }
    Ok(n)
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1_000_000.0).round() / 1_000.0
}

fn per_sec(n: usize, d: Duration) -> f64 {
    let secs = d.as_secs_f64();
    if secs <= 0.0 { 0.0 } else { (n as f64 / secs).round() }
}

/// `VmRSS` and `VmHWM` in kilobytes (Linux; `None` elsewhere).
pub fn memory_kb() -> (Option<u64>, Option<u64>) {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else { return (None, None) };
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    };
    (field("VmRSS:"), field("VmHWM:"))
}

fn memory_json() -> Value {
    let (rss, hwm) = memory_kb();
    json!({"rss_kb": rss, "hwm_kb": hwm})
}

/// The machine the run measured (no names or addresses: CPU model, cores, memory, OS).
pub fn host() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("model name"))
            .map(|v| v.trim_start_matches([' ', '\t', ':']).to_owned())
    });
    let mem = std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    });
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "cpu": cpu,
        "mem_total_kb": mem,
        "debug_build": cfg!(debug_assertions),
    })
}

/// Today's date (UTC) as `YYYY-MM-DD`, for result file names.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0);
    let (y, m, d) = mailcraft_engine::model::dates::civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

// -------------------------------------------------------------------------------------------------------
// Profile generation
// -------------------------------------------------------------------------------------------------------

fn model_role(r: Role) -> FolderRole {
    match r {
        Role::Inbox => FolderRole::Inbox,
        Role::Drafts => FolderRole::Drafts,
        Role::Sent => FolderRole::SentItems,
        Role::Deleted => FolderRole::DeletedItems,
        Role::Junk => FolderRole::JunkEmail,
        Role::Archive => FolderRole::Archive,
    }
}

/// A new profile mailbox: Local Folders with the 30 synthetic folders (their ids in [`FOLDERS`] order) and
/// an IMAP account without a server for the sync measurements.
fn skeleton() -> Result<(Mailbox, Vec<FolderId>)> {
    let mut mb = Mailbox::new_local(mailcraft_engine::model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME);
    let local = mb.default_account().map(|a| a.id).ok_or("the new profile has no account")?;
    let mut ids: Vec<FolderId> = Vec::with_capacity(FOLDERS.len());
    for f in &FOLDERS {
        let id = match f.role {
            Some(r) => mb.role_folder(local, model_role(r)).ok_or_else(|| format!("no {} folder", f.name))?,
            None => {
                let parent = f.parent.and_then(|p| ids.get(p).copied());
                mb.create_folder_in(local, parent, f.name, FolderKind::Mail).map_err(|e| e.to_string())?
            }
        };
        ids.push(id);
    }
    let imap = mb.add_account(Account {
        kind: AccountKind::Imap,
        display_name: SYNC_ACCOUNT.to_owned(),
        address: "bench.sync@example.net".to_owned(),
        ..Default::default()
    });
    mb.create_default_folders(imap);
    let inbox = mb.role_folder(imap, FolderRole::Inbox).ok_or("no IMAP Inbox")?;
    if let Some(f) = mb.folders.get_mut(&inbox) {
        f.remote_name = Some("INBOX".to_owned());
    }
    let headers =
        mb.create_folder_in(imap, None, HEADERS_FOLDER, FolderKind::Mail).map_err(|e| e.to_string())?;
    if let Some(f) = mb.folders.get_mut(&headers) {
        f.remote_name = Some(HEADERS_FOLDER.to_owned());
    }
    Ok((mb, ids))
}

/// A mail item for a synthetic message (state from the generator, source in the store at `blob`).
fn item_of(m: &synth::SynthMessage, folder: FolderId, now: DateTime) -> MailItem {
    let mut item = mailcraft_store::import::item_from_raw(&m.raw, folder, now);
    item.raw = None;
    item.read = m.read;
    item.received = DateTime { unix: m.received, offset_min: 0 };
    if m.flagged {
        item.flag = FlagState::Flagged {
            label: mailcraft_store::import::FOLLOW_UP_LABEL.to_owned(),
            start: None,
            due: None,
            reminder: None,
        };
    }
    item.categories = m.categories.iter().map(|c| (*c).to_owned()).collect();
    item.focus = if m.focused { FocusClass::Focused } else { FocusClass::Other };
    item
}

/// Writes a fresh profile of `opts` into `dir` (emptied first) and describes what it holds.
pub fn generate(dir: &Path, opts: &Options) -> Result<Value> {
    opts.backend.check()?;
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("cannot empty {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let started = Instant::now();
    let mut store = mailcraft_store::Store::open(dir).map_err(|e| format!("cannot open the profile: {e}"))?;
    let (mut mb, ids) = skeleton()?;
    let now = opts.now();
    let mut counts = vec![0usize; FOLDERS.len()];
    let (mut source_bytes, mut unread, mut text, mut html, mut attach) =
        (0u64, 0usize, 0usize, 0usize, 0usize);
    let mut gen_time = Duration::ZERO;
    let mut messages = opts.config().generate();
    loop {
        let t = Instant::now();
        let Some(m) = messages.next() else { break };
        gen_time += t.elapsed();
        let folder = *ids.get(m.folder).ok_or("message in an unknown folder")?;
        let key = store.put_blob(&m.raw).map_err(|e| format!("cannot store a message: {e}"))?;
        let mut item = item_of(&m, folder, now);
        item.blob = Some(key);
        mb.add_mail(item);
        if let Some(c) = counts.get_mut(m.folder) {
            *c += 1;
        }
        source_bytes = source_bytes.saturating_add(m.raw.len() as u64);
        unread += usize::from(!m.read);
        match m.kind {
            BodyKind::Text => text += 1,
            BodyKind::Html => html += 1,
            BodyKind::Attachment => attach += 1,
        }
    }
    let approx = mb.approx_bytes();
    let t = Instant::now();
    store.checkpoint_mut(&mut mb, true).map_err(|e| format!("cannot write the profile: {e}"))?;
    let snapshot = t.elapsed();
    drop(store);
    let n = opts.messages.max(1);
    Ok(json!({
        "ms": ms(started.elapsed()),
        "generator_ms": ms(gen_time),
        "snapshot_ms": ms(snapshot),
        "messages": opts.messages,
        "source_bytes": source_bytes,
        "avg_source_bytes": source_bytes / n as u64,
        "unread": unread,
        "kinds": {"text": text, "html": html, "attachments": attach},
        "folders": counts
            .iter()
            .enumerate()
            .map(|(i, c)| (synth::folder_path(i), json!(c)))
            .collect::<serde_json::Map<String, Value>>(),
        "mailbox_approx_bytes": approx,
        "mail_item_size": std::mem::size_of::<MailItem>(),
        "disk": disk(dir),
    }))
}

/// Bytes per top-level entry of the profile folder (folders summed recursively) and the total.
pub fn disk(dir: &Path) -> Value {
    fn size(p: &Path, depth: usize) -> u64 {
        let Ok(meta) = std::fs::symlink_metadata(p) else { return 0 };
        if meta.is_file() {
            return meta.len();
        }
        if !meta.is_dir() || depth > 16 {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(p) else { return 0 };
        entries.flatten().map(|e| size(&e.path(), depth + 1)).fold(0u64, u64::saturating_add)
    }
    let mut parts = serde_json::Map::new();
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let n = size(&e.path(), 0);
            total = total.saturating_add(n);
            parts.insert(e.file_name().to_string_lossy().into_owned(), json!(n));
        }
    }
    json!({"total": total, "parts": parts})
}

// -------------------------------------------------------------------------------------------------------
// Measurements
// -------------------------------------------------------------------------------------------------------

fn run_cmd(s: &mut Session, id: &str, params: Value) -> Result<(Duration, Value)> {
    let t = Instant::now();
    let r = s.run(id, &params).map_err(|e| format!("{id}: {e}"))?;
    Ok((t.elapsed(), r))
}

/// The largest mail folder of the local account and its item count.
fn largest_folder(s: &Session) -> Option<(FolderId, usize)> {
    let local = s.mailbox.default_account()?.id;
    s.mailbox
        .folder_counts()
        .into_iter()
        .filter(|(id, _)| {
            s.mailbox.folder(*id).is_some_and(|f| f.account == local && f.kind == FolderKind::Mail)
        })
        .max_by_key(|(id, c)| (c.total, std::cmp::Reverse(id.0)))
        .map(|(id, c)| (id, c.total))
}

fn local_folder(s: &Session, name: &str) -> Option<FolderId> {
    let local = s.mailbox.default_account()?.id;
    s.mailbox.folders.values().find(|f| f.account == local && f.name == name).map(|f| f.id)
}

/// Opens the profile in `dir` and runs every measurement; the profile is changed (commands, synced mail).
pub fn measure(dir: &Path, opts: &Options) -> Result<Value> {
    opts.backend.check()?;
    let before_open = memory_json();
    let t = Instant::now();
    let mut s = Session::open(dir).map_err(|e| format!("cannot open the profile: {e}"))?;
    let open = t.elapsed();
    s.fixed_now = Some(opts.now());
    s.view.work_offline = true;
    let t = Instant::now();
    let rows = queries::list_rows(&s);
    let window: Vec<_> = rows.iter().take(WINDOW_ROWS).cloned().collect();
    let first_window = t.elapsed();
    let open_json = json!({
        "ms": ms(open),
        "first_window_ms": ms(first_window),
        "to_first_window_ms": ms(open + first_window),
        "first_folder_rows": rows.len(),
        "window_rows": window.len(),
        "memory_before": before_open,
        "memory_after": memory_json(),
        "mailbox_approx_bytes": s.mailbox.approx_bytes(),
        "mail_items": s.mailbox.mail.len(),
    });
    drop(window);
    drop(rows);
    let t = Instant::now();
    let folder_rows = queries::folder_rows(&s);
    let folder_pane = json!({"ms": ms(t.elapsed()), "rows": folder_rows.len()});
    let switch = folder_switch(&mut s)?;
    let scroll = scroll(&mut s, opts)?;
    let search = search(&mut s)?;
    let sync = sync_apply(&mut s, opts)?;
    let commands = commands(&mut s, opts)?;
    drop(s);
    Ok(json!({
        "open": open_json,
        "folder_pane": folder_pane,
        "folder_switch": switch,
        "scroll": scroll,
        "search": search,
        "sync": sync,
        "commands": commands,
        "memory_end": memory_json(),
        "disk_end": disk(dir),
    }))
}

fn list_ms(s: &Session) -> (f64, usize) {
    let t = Instant::now();
    let rows = queries::list_rows(s);
    let window: Vec<_> = rows.iter().take(WINDOW_ROWS).cloned().collect();
    let d = t.elapsed();
    drop(window);
    (ms(d), rows.len())
}

fn folder_switch(s: &mut Session) -> Result<Value> {
    let (big, items) = largest_folder(s).ok_or("the profile has no mail folder")?;
    let name = s.mailbox.folder(big).map(|f| f.name.clone()).unwrap_or_default();
    // Start elsewhere, so the switch really changes folder.
    if let Some(other) = local_folder(s, "Old stuff") {
        run_cmd(s, "nav.selectFolder", json!({"folder": other.0}))?;
        list_ms(s);
    }
    let (cmd, _) = run_cmd(s, "nav.selectFolder", json!({"folder": big.0}))?;
    let (date_ms, rows) = list_ms(s);
    let (arrange, _) = run_cmd(s, "view.arrangeBy", json!({"key": "from"}))?;
    let (from_ms, from_rows) = list_ms(s);
    run_cmd(s, "view.arrangeBy", json!({"key": "date"}))?;
    let (conv, _) = run_cmd(s, "view.conversations", json!({"on": true}))?;
    let (conv_ms, conv_rows) = list_ms(s);
    // The rows can be few (collapsed date groups); every conversation is built all the same.
    let conversations = queries::list_conversations(s).len();
    run_cmd(s, "view.conversations", json!({"on": false}))?;
    list_ms(s);
    Ok(json!({
        "folder": name,
        "items": items,
        "select_cmd_ms": ms(cmd),
        "date_sort_ms": date_ms,
        "date_rows": rows,
        "from_sort_cmd_ms": ms(arrange),
        "from_sort_ms": from_ms,
        "from_rows": from_rows,
        "conversations_cmd_ms": ms(conv),
        "conversations_ms": conv_ms,
        "conversation_rows": conv_rows,
        "conversations": conversations,
    }))
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// Windows of the largest folder's list at random offsets. The legacy UI builds every row once per change
/// (`rebuild_ms`) and slices the cached rows per frame (`window_*`).
fn scroll(s: &mut Session, opts: &Options) -> Result<Value> {
    let (big, _) = largest_folder(s).ok_or("the profile has no mail folder")?;
    run_cmd(s, "nav.selectFolder", json!({"folder": big.0}))?;
    let t = Instant::now();
    let rows = queries::list_rows(s);
    let rebuild = t.elapsed();
    let mut rng = Rng::new(opts.seed ^ 0x5c);
    let mut times = Vec::with_capacity(opts.windows);
    for _ in 0..opts.windows {
        let off = rng.below(rows.len().saturating_sub(WINDOW_ROWS).max(1));
        let t = Instant::now();
        let w: Vec<_> = rows.iter().skip(off).take(WINDOW_ROWS).cloned().collect();
        times.push(ms(t.elapsed()));
        drop(w);
    }
    let max = times.iter().copied().fold(0.0, f64::max);
    Ok(json!({
        "rows": rows.len(),
        "rebuild_ms": ms(rebuild),
        "windows": times.len(),
        "window_median_ms": median(&mut times),
        "window_max_ms": max,
    }))
}

fn search(s: &mut Session) -> Result<Value> {
    let before = memory_json();
    let t = Instant::now();
    s.search.ensure(&s.mailbox);
    let build = t.elapsed();
    let after_build = memory_json();
    let filter = mailcraft_search::Filter { folders: None, kinds: vec![mailcraft_search::DocKind::Mail] };
    let mut queries_json = Vec::with_capacity(QUERIES.len());
    for q in QUERIES {
        let t = Instant::now();
        let hits = search_index::hits(s, q, &filter);
        queries_json.push(json!({"query": q, "hits": hits.len(), "ms": ms(t.elapsed())}));
    }
    let (create, r) =
        run_cmd(s, "folder.newSearchFolder", json!({"name": "Unread (bench)", "query": "isread:no"}))?;
    let folder = r.get("folder").and_then(Value::as_u64).ok_or("the search folder has no id")?;
    let t = Instant::now();
    let c = search_index::search_folder_counts(s, FolderId(folder));
    let count = t.elapsed();
    let stats = s.search.stats(&s.mailbox);
    Ok(json!({
        "memory_before": before,
        "index_build_ms": ms(build),
        "memory_after_build": after_build,
        "index": format!("{stats:?}"),
        "queries": queries_json,
        "search_folder_create_ms": ms(create),
        "search_folder_count_ms": ms(count),
        "search_folder_total": c.total,
        "search_folder_unread": c.unread,
        "memory_after": memory_json(),
    }))
}

/// The header section of `raw` (through the empty line).
fn header_of(raw: &[u8]) -> &[u8] {
    raw.windows(4).position(|w| w == b"\r\n\r\n").and_then(|p| raw.get(..p.saturating_add(4))).unwrap_or(raw)
}

fn sync_messages(opts: &Options, uid_base: u32, headers_only: bool) -> impl Iterator<Item = NewMessage> {
    let cfg =
        Config { seed: opts.seed ^ SYNC_SEED, ..Config { messages: opts.sync_messages, ..opts.config() } };
    cfg.generate().enumerate().map(move |(i, m)| {
        let uid = uid_base.saturating_add(u32::try_from(i).unwrap_or(u32::MAX - uid_base));
        let size = Some(m.raw.len() as u64);
        let flags = if m.read { vec!["\\Seen".to_owned()] } else { Vec::new() };
        let raw = if headers_only { header_of(&m.raw).to_vec() } else { m.raw };
        NewMessage { uid, raw, flags, internal_date: None, size, headers_only }
    })
}

/// Applies `opts.sync_messages` synthetic messages as IMAP results in batches of [`SYNC_BATCH`]: whole
/// messages into INBOX, then headers into [`HEADERS_FOLDER`] followed by their bodies.
fn sync_apply(s: &mut Session, opts: &Options) -> Result<Value> {
    let account = s
        .mailbox
        .accounts
        .iter()
        .find(|a| a.display_name == SYNC_ACCOUNT)
        .map(|a| a.id)
        .ok_or("the profile has no sync account")?;
    let n = opts.sync_messages;
    let apply = |s: &mut Session, folder: &str, headers_only: bool, bodies: bool| -> Result<Duration> {
        let mut spent = Duration::ZERO;
        let mut it = sync_messages(opts, 1, headers_only && !bodies).peekable();
        let mut uidnext = 1u32;
        while it.peek().is_some() {
            let batch: Vec<NewMessage> = it.by_ref().take(SYNC_BATCH).collect();
            uidnext = batch.last().map_or(uidnext, |m| m.uid.saturating_add(1));
            let results = if bodies {
                SyntheticImap::Bodies { folder: folder.to_owned(), msgs: batch }
            } else {
                let state = FolderState { uidvalidity: 1, uidnext, highest_modseq: 0, uids: Vec::new() };
                SyntheticImap::Delta {
                    folder: folder.to_owned(),
                    delta: SyncDelta { new: batch, state, ..Default::default() },
                }
            };
            let t = Instant::now();
            if !s.queue_imap_results(account, results) {
                return Err("the sync account does not take results".into());
            }
            s.poll_sync();
            spent += t.elapsed();
        }
        Ok(spent)
    };
    let full = apply(s, "INBOX", false, false)?;
    let after_full = s.mailbox.mail.len();
    let headers = apply(s, HEADERS_FOLDER, true, false)?;
    let bodies = apply(s, HEADERS_FOLDER, false, true)?;
    let count = |name: &str| {
        s.mailbox
            .folders
            .values()
            .find(|f| f.account == account && f.remote_name.as_deref() == Some(name))
            .map_or(0, |f| s.mailbox.counts_of(f.id).total)
    };
    Ok(json!({
        "messages": n,
        "batch": SYNC_BATCH,
        "full_ms": ms(full),
        "full_per_sec": per_sec(n, full),
        "headers_ms": ms(headers),
        "headers_per_sec": per_sec(n, headers),
        "bodies_ms": ms(bodies),
        "bodies_per_sec": per_sec(n, bodies),
        "inbox_items": count("INBOX"),
        "headers_items": count(HEADERS_FOLDER),
        "mail_items_after_full": after_full,
        "memory_after": memory_json(),
    }))
}

fn commands(s: &mut Session, opts: &Options) -> Result<Value> {
    let (big, items) = largest_folder(s).ok_or("the profile has no mail folder")?;
    let target = local_folder(s, "Old stuff").ok_or("no target folder")?;
    let archive = local_folder(s, "Archive").ok_or("no Archive folder")?;
    run_cmd(s, "nav.selectFolder", json!({"folder": big.0}))?;
    let mut ids: Vec<ItemId> = s.mailbox.items_in(big).iter().map(|m| m.id).collect();
    ids.sort_unstable();
    let unread = s.mailbox.items_in(big).iter().find(|m| !m.read).map(|m| m.id);
    let mut out = serde_json::Map::new();
    if let Some(id) = unread {
        let (d, _) = run_cmd(s, "mail.markRead", json!({"ids": [id.0]}))?;
        out.insert("mark_read_1_ms".into(), json!(ms(d)));
    }
    let Some((&one, rest)) = ids.split_first() else {
        return Ok(Value::Object(out));
    };
    let (d, _) = run_cmd(s, "mail.move", json!({"ids": [one.0], "folder": archive.0}))?;
    out.insert("move_1_ms".into(), json!(ms(d)));
    let bulk: Vec<u64> = rest.iter().take(opts.bulk_move.min(items.saturating_sub(2))).map(|i| i.0).collect();
    let (d, _) = run_cmd(s, "mail.move", json!({"ids": bulk, "folder": target.0}))?;
    out.insert("bulk_move_items".into(), json!(bulk.len()));
    out.insert("bulk_move_ms".into(), json!(ms(d)));
    let (d, _) = run_cmd(s, "app.undo", json!({}))?;
    out.insert("undo_bulk_move_ms".into(), json!(ms(d)));
    let (d, _) = run_cmd(s, "app.redo", json!({}))?;
    out.insert("redo_bulk_move_ms".into(), json!(ms(d)));
    if let Some(&last) = rest.last()
        && s.mailbox.mail.get(last).is_some_and(|m| m.folder == big)
    {
        let (d, _) = run_cmd(s, "mail.delete", json!({"ids": [last.0]}))?;
        out.insert("delete_1_ms".into(), json!(ms(d)));
    }
    let journal = s.store.as_ref().map(mailcraft_store::Store::journal_bytes);
    out.insert("journal_bytes".into(), json!(journal));
    out.insert("undo_steps".into(), json!(s.undo.undo.len()));
    out.insert("memory_after".into(), memory_json());
    Ok(Value::Object(out))
}

/// The JSON document of a run: options, host, generation and the measurements.
pub fn report(opts: &Options, generated: Value, measured: Value) -> Value {
    json!({
        "schema": SCHEMA,
        "date": today(),
        "backend": opts.backend.name(),
        "messages": opts.messages,
        "small_bodies": opts.small_bodies,
        "seed": opts.seed,
        "sync_messages": opts.sync_messages,
        "bulk_move": opts.bulk_move,
        "host": host(),
        "generate": generated,
        "measure": measured,
    })
}

/// Generates the profile in `work/profile`, measures it in this process and returns the report. The
/// command-line tool measures in a child process instead (cleaner memory figures).
pub fn run(work: &Path, opts: &Options) -> Result<Value> {
    let dir = profile_dir(work);
    let generated = generate(&dir, opts)?;
    let measured = measure(&dir, opts)?;
    Ok(report(opts, generated, measured))
}

/// Where a run under `work` keeps its profile.
pub fn profile_dir(work: &Path) -> PathBuf {
    work.join("profile")
}
