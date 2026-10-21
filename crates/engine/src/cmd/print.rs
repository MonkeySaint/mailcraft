//! `print.*`: Print and Save as PDF (File › Print, `plan/reference/backstage.md`). Builds print content from the
//! session (Memo Style for items, Table Style for the list, Calendar Daily / Weekly / Monthly, Card / Small
//! Booklet / Phone Directory for contacts), lays it out with `mailcraft_render::print` and writes a PDF with
//! `mailcraft_render::pdf`.
//!
//! Printers: [`SAVE_AS_PDF`] writes the PDF to `path`. Any other printer name sends the PDF to the operating
//! system's print service: on Unix (Linux, the BSDs, macOS) through CUPS `lp`, reading the document from stdin;
//! elsewhere printing is not available yet and returns an error (use Save as PDF).
//!
//! Page Setup travels with each call as `setup` (see [`setup_from`]); the UI keeps the user's choices.
//!
//! Parameters shared by the commands:
//! `{"ids"?: [number], "style"?: string, "setup"?: {...}, "start"?: "YYYY-MM-DD", "end"?: "YYYY-MM-DD",
//! "folder"?: number}`.

use base64::Engine as _;
use serde_json::{Value, json};

use crate::cmd::calendar::occurrences;
use crate::cmd::rules::{dialog, write_file};
use crate::cmd::util::{opt_bool, opt_str, opt_u64};
use crate::{CmdError, CmdResult, CommandSpec, Module, Notice, Session, always};
use mailcraft_fonts::FontDb;
use mailcraft_model::dates::{days_from_civil, days_in_month, local_day, weekday_of_day};
use mailcraft_model::{
    Appointment, BusyStatus, Contact, DateTime, Doc, FolderKind, ItemId, ListTime, MailItem, Note, Task,
    TaskPriority, TaskStatus,
};
use mailcraft_render::print::{
    CalendarData, CalendarEvent, ContactCard, MemoItem, TableColumn, TableData, paginate, render_page,
};
pub use mailcraft_render::print::{
    CalendarStyle, ContactStyle, FIELDS, Margins, Orientation, Page, PageSetup, Paper, PrintContent,
    PrintContext,
};

/// The built-in "printer" that writes a PDF file.
pub const SAVE_AS_PDF: &str = "Save as PDF";
/// Name for the operating system's default printer.
pub const DEFAULT_PRINTER: &str = "Default printer";

/// Print styles: (id, label).
pub const STYLES: [(&str, &str); 8] = [
    ("table", "Table Style"),
    ("memo", "Memo Style"),
    ("daily", "Daily Style"),
    ("weekly", "Weekly Agenda Style"),
    ("monthly", "Monthly Style"),
    ("card", "Card Style"),
    ("smallBooklet", "Small Booklet Style"),
    ("phoneDirectory", "Phone Directory Style"),
];

/// Most items printed in one job.
const MAX_ITEMS: usize = 5_000;
/// Most occurrences collected per appointment.
const MAX_OCCURRENCES: usize = 500;
/// Most days a calendar job spans.
const MAX_CAL_DAYS: i64 = 366 * 2;
/// Largest preview scale.
const MAX_PREVIEW_SCALE: f32 = 4.0;
/// Most copies sent to a printer.
const MAX_COPIES: u64 = 99;

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates: false, enabled: always, run }
}

/// Older print entry points (`file.print`, `mail.print`, `mail.quickPrint`) open the Print page; given a
/// style, printer or PDF path they print straight away through `print.print` with `ids` as the items.
/// `None` = open the Print page as before.
pub fn forward(s: &mut Session, p: &Value, ids: &[u64]) -> Option<CmdResult> {
    let direct =
        ["style", "printer", "path", "pdf_path"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    if !direct {
        return None;
    }
    let mut params = p.as_object().cloned().unwrap_or_default();
    params.entry("ids").or_insert_with(|| json!(ids));
    let params = Value::Object(params);
    Some(s.run("print.print", &params))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "print.print",
            "Print",
            "File › Print",
            r#"{"ids"?: [number], "style"?: "memo"|"table"|"daily"|"weekly"|"monthly"|"card"|"smallBooklet"|"phoneDirectory", "printer"?: "Save as PDF"|"Default printer"|string, "path"?: string (PDF file; also "pdf_path"), "copies"?: number, "pages"?: "1-3,5", "setup"?: {"paper"?: "Letter"|"Legal"|"A4"|"A5", "orientation"?: "portrait"|"landscape", "margins"?: {"top"?,"bottom"?,"left"?,"right"?: inches}, "header"?: [string;3], "footer"?: [string;3], "newPagePerItem"?: bool}, "start"?: "YYYY-MM-DD", "end"?: "YYYY-MM-DD", "folder"?: number}"#,
            print,
        ),
        spec(
            "print.preview",
            "Print Preview",
            "File › Print",
            r#"{(as print.print), "page"?: number (0-based), "scale"?: number, "png"?: bool (PNG, base64), "rgba"?: bool (straight RGBA8, base64)}"#,
            preview_cmd,
        ),
        spec("print.printers", "Which Printer", "File › Print", "", printers_cmd),
        spec("print.styles", "Print Styles", "File › Print › Settings", r#"{"ids"?: [number]}"#, styles_cmd),
        spec(
            "print.pageSetup",
            "Page Setup...",
            "File › Print › Print Options",
            r#"{"style"?: string, "setup"?: {...}}"#,
            page_setup,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Styles and Page Setup

/// Label of a style id.
pub fn style_label(id: &str) -> &'static str {
    STYLES.iter().find(|(i, _)| *i == id).map_or("", |(_, l)| *l)
}

fn parse_style(s: &str) -> Option<&'static str> {
    STYLES.iter().find(|(i, l)| i.eq_ignore_ascii_case(s) || l.eq_ignore_ascii_case(s)).map(|(i, _)| *i)
}

/// What kind of item an id is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mail,
    Appointment,
    Contact,
    Task,
    Note,
}

fn kind_of(s: &Session, id: ItemId) -> Option<Kind> {
    let mb = &s.mailbox;
    if mb.mail.contains_key(&id) {
        Some(Kind::Mail)
    } else if mb.appointments.contains_key(&id) {
        Some(Kind::Appointment)
    } else if mb.contacts.contains_key(&id) {
        Some(Kind::Contact)
    } else if mb.tasks.contains_key(&id) {
        Some(Kind::Task)
    } else if mb.notes.contains_key(&id) {
        Some(Kind::Note)
    } else {
        None
    }
}

/// The module the job is about: the kind of the first target item, else the current module.
fn context_module(s: &Session, p: &Value) -> Module {
    let ids = s.target_ids(p);
    match ids.first().and_then(|i| kind_of(s, *i)) {
        Some(Kind::Mail) => Module::Mail,
        Some(Kind::Appointment) => Module::Calendar,
        Some(Kind::Contact) => Module::People,
        Some(Kind::Task) => Module::Tasks,
        Some(Kind::Note) => Module::Notes,
        None => s.view.module,
    }
}

/// Styles offered for the current module or the target items: (id, label).
pub fn styles_for(s: &Session, p: &Value) -> Vec<(&'static str, &'static str)> {
    let ids: &[&str] = match context_module(s, p) {
        Module::Calendar => &["daily", "weekly", "monthly", "memo"],
        Module::People => &["card", "smallBooklet", "phoneDirectory", "memo"],
        _ => &["table", "memo"],
    };
    STYLES.iter().copied().filter(|(i, _)| ids.contains(i)).collect()
}

/// The style a job uses: `style` when valid, else the module default (Memo for selected items, Table for a
/// list without selection, Daily for the calendar, Card for contacts).
pub fn style_of(s: &Session, p: &Value) -> &'static str {
    if let Some(st) = opt_str(p, "style").and_then(parse_style) {
        return st;
    }
    match context_module(s, p) {
        Module::Calendar => "daily",
        Module::People => "card",
        Module::Tasks => "table",
        Module::Notes => "memo",
        _ if s.target_ids(p).is_empty() => "table",
        _ => "memo",
    }
}

/// Page Setup defaults of a style (Monthly prints landscape).
pub fn default_setup(style: &str) -> PageSetup {
    let mut setup = PageSetup::default();
    if style == "monthly" {
        setup.orientation = Orientation::Landscape;
    }
    if style == "smallBooklet" {
        setup.paper = Paper::A5;
    }
    setup
}

const PX_PER_IN: f64 = 96.0;

/// Page Setup from `p["setup"]` over the style's defaults. Unknown or malformed values keep the default.
pub fn setup_from(p: &Value, style: &str) -> PageSetup {
    let mut setup = default_setup(style);
    let Some(o) = p.get("setup").filter(|v| v.is_object()) else { return setup };
    if let Some(paper) = o.get("paper").and_then(Value::as_str).and_then(Paper::parse) {
        setup.paper = paper;
    }
    match o.get("orientation").and_then(Value::as_str) {
        Some(v) if v.eq_ignore_ascii_case("landscape") => setup.orientation = Orientation::Landscape,
        Some(v) if v.eq_ignore_ascii_case("portrait") => setup.orientation = Orientation::Portrait,
        _ => {}
    }
    if let Some(m) = o.get("margins") {
        let inch = |k: &str, d: f32| -> f32 {
            match m.get(k).and_then(Value::as_f64) {
                Some(v) if v.is_finite() && (0.0..=4.0).contains(&v) => (v * PX_PER_IN) as f32,
                _ => d,
            }
        };
        let d = setup.margins;
        setup.margins = Margins {
            top: inch("top", d.top),
            bottom: inch("bottom", d.bottom),
            left: inch("left", d.left),
            right: inch("right", d.right),
        };
    }
    let slots = |v: Option<&Value>, d: &[String; 3]| -> [String; 3] {
        let Some(a) = v.and_then(Value::as_array) else { return d.clone() };
        let get = |i: usize| -> String {
            a.get(i).and_then(Value::as_str).map(|s| s.chars().take(256).collect()).unwrap_or_default()
        };
        [get(0), get(1), get(2)]
    };
    setup.header = slots(o.get("header"), &setup.header);
    setup.footer = slots(o.get("footer"), &setup.footer);
    if let Some(b) = o.get("newPagePerItem").and_then(Value::as_bool) {
        setup.new_page_per_item = b;
    }
    setup
}

/// `setup` as the JSON [`setup_from`] reads (margins in inches).
pub fn setup_json(setup: &PageSetup) -> Value {
    let inch = |px: f32| (f64::from(px) / PX_PER_IN * 100.0).round() / 100.0;
    json!({
        "paper": setup.paper.name(),
        "orientation": if setup.orientation == Orientation::Landscape { "landscape" } else { "portrait" },
        "margins": {
            "top": inch(setup.margins.top),
            "bottom": inch(setup.margins.bottom),
            "left": inch(setup.margins.left),
            "right": inch(setup.margins.right),
        },
        "header": setup.header,
        "footer": setup.footer,
        "newPagePerItem": setup.new_page_per_item,
    })
}

// ---------------------------------------------------------------------------------------------------------
// Building the job

/// A laid-out job.
pub struct Job {
    pub style: &'static str,
    pub pages: Vec<Page>,
    pub ctx: PrintContext,
}

/// Lays out the job described by `p`.
pub fn build(s: &Session, p: &Value, fonts: &FontDb) -> Result<Job, CmdError> {
    let style = style_of(s, p);
    let setup = setup_from(p, style);
    let ctx = context(s, style);
    let content = content(s, p, style)?;
    let pages = paginate(&content, &setup, &ctx, fonts);
    Ok(Job { style, pages, ctx })
}

fn context(s: &Session, style: &str) -> PrintContext {
    let now = s.now();
    let tz = s.tz_offset_min;
    let user = s
        .mailbox
        .accounts
        .iter()
        .map(|a| a.display_name.trim())
        .find(|n| !n.is_empty())
        .or_else(|| s.me.first().map(String::as_str))
        .unwrap_or("")
        .to_owned();
    let title = match s.selection.folder {
        f if s.mailbox.folders.contains_key(&f) && style == "table" => {
            s.mailbox.folders.get(&f).map(|f| f.name.clone()).unwrap_or_default()
        }
        _ => style_label(style).to_owned(),
    };
    PrintContext {
        user_name: user,
        date_printed: ListTime::date(now, tz),
        time_printed: ListTime::clock(now, tz),
        title,
    }
}

fn content(s: &Session, p: &Value, style: &str) -> Result<PrintContent, CmdError> {
    Ok(match style {
        "table" => PrintContent::Table(table(s, p)),
        "daily" | "weekly" | "monthly" => PrintContent::Calendar(calendar(s, p, style)?),
        "card" | "smallBooklet" | "phoneDirectory" => {
            let st = match style {
                "smallBooklet" => ContactStyle::SmallBooklet,
                "phoneDirectory" => ContactStyle::PhoneDirectory,
                _ => ContactStyle::Card,
            };
            PrintContent::Contacts { style: st, cards: contact_cards(s, p) }
        }
        _ => {
            let items: Vec<MemoItem> =
                s.target_ids(p).into_iter().take(MAX_ITEMS).filter_map(|id| memo_item(s, id)).collect();
            if items.is_empty() {
                return Err(CmdError::Params("nothing to print: select an item first".into()));
            }
            PrintContent::Memo(items)
        }
    })
}

fn people(l: &[mailcraft_model::Address]) -> String {
    l.iter().map(mailcraft_model::Address::to_display_string).collect::<Vec<_>>().join("; ")
}

fn push(fields: &mut Vec<(String, String)>, label: &str, value: String) {
    if !value.trim().is_empty() {
        fields.push((label.to_owned(), value));
    }
}

fn memo_item(s: &Session, id: ItemId) -> Option<MemoItem> {
    let mb = &s.mailbox;
    let tz = s.tz_offset_min;
    if let Some(m) = mb.mail.get(&id) {
        return Some(mail_memo(m, tz));
    }
    if let Some(a) = mb.appointments.get(&id) {
        return Some(appointment_memo(a, tz));
    }
    if let Some(c) = mb.contacts.get(&id) {
        return Some(contact_memo(c));
    }
    if let Some(t) = mb.tasks.get(&id) {
        return Some(task_memo(t, tz));
    }
    mb.notes.get(&id).map(|n| note_memo(n, tz))
}

/// Body of a mail item as rich text (sanitised HTML, else plain text, else the preview line).
pub fn mail_doc(m: &MailItem) -> Doc {
    match (&m.body.html, &m.body.text) {
        (Some(h), _) => mailcraft_html::html_to_doc(h),
        (None, Some(t)) => Doc::from_plain_text(t),
        (None, None) => Doc::from_plain_text(&m.preview),
    }
}

fn mail_memo(m: &MailItem, tz: i32) -> MemoItem {
    let mut f = Vec::new();
    push(
        &mut f,
        "From",
        m.from.as_ref().map(mailcraft_model::Address::to_display_string).unwrap_or_default(),
    );
    push(&mut f, "Sent", ListTime::long(m.sent.unwrap_or(m.received), tz));
    push(&mut f, "To", people(&m.to));
    push(&mut f, "Cc", people(&m.cc));
    f.push(("Subject".into(), m.subject.clone()));
    let atts: Vec<String> = m.attachments.iter().filter(|a| !a.inline).map(|a| a.filename.clone()).collect();
    push(&mut f, "Attachments", atts.join("; "));
    MemoItem { fields: f, body: mail_doc(m) }
}

fn busy_label(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "Free",
        BusyStatus::WorkingElsewhere => "Working Elsewhere",
        BusyStatus::Tentative => "Tentative",
        BusyStatus::Busy => "Busy",
        BusyStatus::OutOfOffice => "Out of Office",
    }
}

fn appointment_memo(a: &Appointment, tz: i32) -> MemoItem {
    let mut f = Vec::new();
    f.push(("Subject".into(), a.subject.clone()));
    push(&mut f, "Location", a.location.clone());
    let when = |t: DateTime| if a.all_day { ListTime::long_date(t, tz) } else { ListTime::long(t, tz) };
    push(&mut f, "Start", when(a.start));
    push(&mut f, "End", when(a.end));
    push(&mut f, "Show Time As", busy_label(a.show_as).to_owned());
    if a.recurrence.is_some() {
        push(&mut f, "Recurrence", "Recurring".to_owned());
    }
    push(
        &mut f,
        "Organizer",
        a.organizer.as_ref().map(mailcraft_model::Address::to_display_string).unwrap_or_default(),
    );
    let att: Vec<mailcraft_model::Address> = a.attendees.iter().map(|x| x.mailbox.clone()).collect();
    push(&mut f, "Attendees", people(&att));
    push(&mut f, "Categories", a.categories.join(", "));
    MemoItem { fields: f, body: Doc::from_plain_text(&a.body) }
}

fn contact_fields(c: &Contact) -> Vec<(String, String)> {
    let mut f = Vec::new();
    push(&mut f, "Full Name", c.full_name());
    push(&mut f, "Company", c.company.clone());
    push(&mut f, "Job Title", c.job_title.clone());
    push(&mut f, "Department", c.department.clone());
    for (i, e) in c.emails.iter().enumerate().take(3) {
        let label = match i {
            0 => "E-mail",
            1 => "E-mail 2",
            _ => "E-mail 3",
        };
        push(&mut f, label, e.clone());
    }
    for (kind, number) in c.phones.iter().take(16) {
        let label = if kind.trim().is_empty() { "Phone" } else { kind.as_str() };
        push(&mut f, label, number.clone());
    }
    for a in c.addresses.iter().take(3) {
        let parts: Vec<&str> = [
            a.street.as_str(),
            a.city.as_str(),
            a.region.as_str(),
            a.postal_code.as_str(),
            a.country.as_str(),
        ]
        .into_iter()
        .filter(|x| !x.trim().is_empty())
        .collect();
        let label =
            if a.kind.trim().is_empty() { "Address".to_owned() } else { format!("{} Address", a.kind) };
        push(&mut f, &label, parts.join(", "));
    }
    push(&mut f, "Web Page", c.web_page.clone());
    push(&mut f, "IM Address", c.im_address.clone());
    push(&mut f, "Birthday", c.birthday.clone().unwrap_or_default());
    push(&mut f, "Categories", c.categories.join(", "));
    f
}

fn contact_memo(c: &Contact) -> MemoItem {
    MemoItem { fields: contact_fields(c), body: Doc::from_plain_text(&c.notes) }
}

fn status_label(st: TaskStatus) -> &'static str {
    match st {
        TaskStatus::NotStarted => "Not Started",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed => "Completed",
        TaskStatus::WaitingOnSomeoneElse => "Waiting on someone else",
        TaskStatus::Deferred => "Deferred",
    }
}

fn task_memo(t: &Task, tz: i32) -> MemoItem {
    let mut f = Vec::new();
    f.push(("Subject".into(), t.subject.clone()));
    push(&mut f, "Start Date", t.start.map(|d| ListTime::long_date(d, tz)).unwrap_or_default());
    push(&mut f, "Due Date", t.due.map(|d| ListTime::long_date(d, tz)).unwrap_or_default());
    push(&mut f, "Status", status_label(t.status).to_owned());
    let pri = match t.priority {
        TaskPriority::Low => "Low",
        TaskPriority::Normal => "Normal",
        TaskPriority::High => "High",
    };
    push(&mut f, "Priority", pri.to_owned());
    push(&mut f, "% Complete", format!("{}%", t.percent_complete.min(100)));
    push(&mut f, "Categories", t.categories.join(", "));
    MemoItem { fields: f, body: Doc::from_plain_text(&t.body) }
}

fn note_memo(n: &Note, tz: i32) -> MemoItem {
    let fields = vec![("Modified".to_owned(), ListTime::long(n.modified, tz))];
    MemoItem { fields, body: Doc::from_plain_text(&n.text) }
}

fn table(s: &Session, p: &Value) -> TableData {
    let col = |t: &str, w: f32| TableColumn { title: t.to_owned(), weight: w };
    let explicit = crate::cmd::util::opt_ids(p);
    let tz = s.tz_offset_min;
    let folder_kind = s.mailbox.folders.get(&s.selection.folder).map(|f| f.kind);
    let ids: Vec<ItemId> = match explicit {
        Some(ids) => ids,
        None if folder_kind == Some(FolderKind::Tasks) => {
            s.mailbox.tasks.values().filter(|t| t.folder == s.selection.folder).map(|t| t.id).collect()
        }
        None => crate::queries::visible_item_ids(s),
    };
    let ids: Vec<ItemId> = ids.into_iter().take(MAX_ITEMS * 10).collect();
    let title = s.mailbox.folders.get(&s.selection.folder).map(|f| f.name.clone()).unwrap_or_default();
    let is_task = ids.first().is_some_and(|i| s.mailbox.tasks.contains_key(i));
    if is_task {
        let rows: Vec<&Task> = ids.iter().filter_map(|i| s.mailbox.tasks.get(i)).collect();
        return TableData {
            title,
            columns: vec![
                col("Subject", 4.0),
                col("Due Date", 2.0),
                col("Status", 2.0),
                col("% Complete", 1.0),
                col("Categories", 2.0),
            ],
            bold: rows.iter().map(|_| false).collect(),
            rows: rows
                .iter()
                .map(|t| {
                    vec![
                        t.subject.clone(),
                        t.due.map(|d| ListTime::date(d, tz)).unwrap_or_else(|| "None".into()),
                        status_label(t.status).into(),
                        format!("{}%", t.percent_complete.min(100)),
                        t.categories.join(", "),
                    ]
                })
                .collect(),
        };
    }
    let mails: Vec<&MailItem> = ids.iter().filter_map(|i| s.mailbox.mail.get(i)).collect();
    TableData {
        title,
        columns: vec![
            col("From", 2.5),
            col("Subject", 4.5),
            col("Received", 2.2),
            col("Size", 1.0),
            col("Categories", 1.6),
        ],
        bold: mails.iter().map(|m| !m.read).collect(),
        rows: mails
            .iter()
            .map(|m| {
                vec![
                    m.from.as_ref().map(|f| f.display().to_owned()).unwrap_or_default(),
                    m.subject.clone(),
                    ListTime::short_date_time(m.received, tz),
                    crate::queries::size_text(m.size),
                    m.categories.join(", "),
                ]
            })
            .collect(),
    }
}

/// "YYYY-MM-DD" → civil day number.
fn parse_day(v: Option<&str>) -> Option<i64> {
    let v = v?.trim();
    let mut it = v.splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.trim().parse().ok()?;
    if !(1..=9999).contains(&y) {
        return None;
    }
    days_from_civil(y, m, d)
}

fn calendar(s: &Session, p: &Value, style: &str) -> Result<CalendarData, CmdError> {
    let tz = s.tz_offset_min;
    let today = local_day(s.now(), tz);
    let start = parse_day(opt_str(p, "start"));
    let end = parse_day(opt_str(p, "end"));
    let anchor = start.unwrap_or(today);
    let (cs, cy, cm) = {
        let (y, m, _) = mailcraft_model::dates::civil_from_days(anchor);
        (anchor, y, m)
    };
    let (first, last) = match (style, start, end) {
        (_, Some(a), Some(b)) => (a, b),
        ("weekly", _, _) => {
            let ws = cs - i64::from(weekday_of_day(cs).0);
            (ws, end.unwrap_or(ws + 6))
        }
        ("monthly", _, _) => {
            let f = days_from_civil(cy, cm, 1).unwrap_or(cs);
            (f, end.unwrap_or(f + i64::from(days_in_month(cy, cm)) - 1))
        }
        _ => (cs, end.unwrap_or(cs)),
    };
    if last < first {
        return Err(CmdError::Params("`end` is before `start`".into()));
    }
    let last = last.min(first.saturating_add(MAX_CAL_DAYS));
    // Window in UTC covering the local days (one extra day either side for offsets).
    let from = DateTime::from_unix(first.saturating_sub(1).saturating_mul(86_400), 0);
    let to = DateTime::from_unix(last.saturating_add(2).saturating_mul(86_400), 0);
    let folder = opt_u64(p, "folder").map(mailcraft_model::FolderId);
    let mut events = Vec::new();
    for a in s.mailbox.appointments.values() {
        if folder.is_some_and(|f| f != a.folder) {
            continue;
        }
        for (st, en) in occurrences(a, from, to, MAX_OCCURRENCES) {
            let lst = mailcraft_model::dates::LocalTime::of(st, tz);
            let len = mailcraft_model::dates::LocalTime::of(en, tz);
            let mins = |l: &mailcraft_model::dates::LocalTime| u32::from(l.hour) * 60 + u32::from(l.minute);
            events.push(CalendarEvent {
                start_day: lst.day_number,
                start_min: mins(&lst),
                end_day: len.day_number,
                end_min: mins(&len),
                all_day: a.all_day,
                subject: a.subject.clone(),
                location: a.location.clone(),
            });
            if events.len() >= 10_000 {
                break;
            }
        }
    }
    let title = folder
        .and_then(|f| s.mailbox.folders.get(&f))
        .map(|f| f.name.clone())
        .unwrap_or_else(|| "Calendar".to_owned());
    let style = match style {
        "weekly" => CalendarStyle::Weekly,
        "monthly" => CalendarStyle::Monthly,
        _ => CalendarStyle::Daily,
    };
    Ok(CalendarData { style, title, first_day: first, last_day: last, week_start: 0, events })
}

fn contact_cards(s: &Session, p: &Value) -> Vec<ContactCard> {
    let explicit: Vec<&Contact> = crate::cmd::util::opt_ids(p)
        .unwrap_or_default()
        .iter()
        .filter_map(|i| s.mailbox.contacts.get(i))
        .collect();
    let mut list: Vec<&Contact> = if explicit.is_empty() {
        let folder = s.selection.folder;
        let in_folder: Vec<&Contact> = s.mailbox.contacts.values().filter(|c| c.folder == folder).collect();
        if in_folder.is_empty() { s.mailbox.contacts.values().collect() } else { in_folder }
    } else {
        explicit
    };
    let name = |c: &Contact| if c.file_as.trim().is_empty() { c.full_name() } else { c.file_as.clone() };
    list.sort_by_cached_key(|c| name(c).to_lowercase());
    list.into_iter()
        .take(MAX_ITEMS * 10)
        .map(|c| ContactCard {
            name: name(c),
            fields: contact_fields(c).into_iter().filter(|(l, _)| l != "Full Name").collect(),
            phones: c.phones.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------
// Preview, PDF, printers

/// One rendered preview page (straight-alpha RGBA).
pub struct Preview {
    pub pages: usize,
    pub page: usize,
    /// Page size in logical px (1/96 in).
    pub page_size: (f32, f32),
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Renders page `page` (clamped) of the job at `scale` device px per logical px.
pub fn preview(s: &Session, p: &Value, page: usize, scale: f32) -> Result<Preview, CmdError> {
    let fonts = FontDb::bundled();
    let job = build(s, p, &fonts)?;
    let n = job.pages.len();
    let page = page.min(n.saturating_sub(1));
    let scale = if scale.is_finite() { scale.clamp(0.05, MAX_PREVIEW_SCALE) } else { 0.5 };
    let canvas = job.pages.get(page).map(|pg| render_page(pg, &fonts, scale)).unwrap_or_default();
    let page_size = job.pages.get(page).map_or((0.0, 0.0), |pg| (pg.width, pg.height));
    Ok(Preview {
        pages: n,
        page,
        page_size,
        width: canvas.width,
        height: canvas.height,
        rgba: canvas.to_rgba_unpremultiplied(),
    })
}

/// "1-3,5" → 0-based page indices below `n` (empty / unparsable = all pages).
fn page_range(spec: Option<&str>, n: usize) -> Vec<usize> {
    let all: Vec<usize> = (0..n).collect();
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else { return all };
    let mut out = Vec::new();
    for part in spec.split(',').take(256) {
        let part = part.trim();
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<usize>().ok(), b.trim().parse::<usize>().ok()),
            None => {
                let v = part.parse::<usize>().ok();
                (v, v)
            }
        };
        let (Some(a), Some(b)) = (a, b) else { continue };
        for i in a.max(1)..=b.min(n) {
            if !out.contains(&(i - 1)) {
                out.push(i - 1);
            }
        }
    }
    if out.is_empty() { all } else { out }
}

/// The PDF for the job described by `p` (honouring `pages`); returns (bytes, pages written, title).
pub fn pdf_bytes(s: &Session, p: &Value) -> Result<(Vec<u8>, usize, String), CmdError> {
    let fonts = FontDb::bundled();
    let job = build(s, p, &fonts)?;
    let keep = page_range(opt_str(p, "pages"), job.pages.len());
    let pages: Vec<Page> = keep.iter().filter_map(|i| job.pages.get(*i).cloned()).collect();
    let info =
        mailcraft_render::pdf::PdfInfo { title: job.ctx.title.clone(), author: job.ctx.user_name.clone() };
    let bytes = mailcraft_render::pdf::write_pdf(&pages, &fonts, &info)
        .map_err(|e| CmdError::Failed(e.to_string()))?;
    Ok((bytes, pages.len(), job.ctx.title))
}

/// The file `path` names, refused when its folder does not exist.
pub(crate) fn output_path(path: &str) -> Result<std::path::PathBuf, CmdError> {
    let path = std::path::PathBuf::from(path.trim());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && !dir.is_dir()
    {
        return Err(CmdError::Failed(format!("{}: folder not found", dir.display())));
    }
    Ok(path)
}

/// Sends laid-out `pages` to `printer`: [`SAVE_AS_PDF`] writes them as a PDF at `path`; any other name (or
/// [`DEFAULT_PRINTER`]) goes to the system print service. Returns `{"printer", "pages", "path"?}`.
pub(crate) fn output_pages(
    s: &mut Session,
    pages: &[Page],
    title: &str,
    printer: &str,
    path: Option<&str>,
) -> CmdResult {
    let fonts = FontDb::bundled();
    let info = mailcraft_render::pdf::PdfInfo { title: title.to_owned(), author: String::new() };
    let pdf = |pages: &[Page]| {
        mailcraft_render::pdf::write_pdf(pages, &fonts, &info).map_err(|e| CmdError::Failed(e.to_string()))
    };
    if printer.eq_ignore_ascii_case(SAVE_AS_PDF) {
        let path = path
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| CmdError::Params("`path` (the PDF file to write) is required".into()))?;
        let path = output_path(path)?;
        let bytes = pdf(pages)?;
        write_file(&path, &bytes)?;
        let shown = path.display().to_string();
        s.notices.push(Notice::Info(format!("Saved {} page(s) to {shown}.", pages.len())));
        return Ok(
            json!({"printer": SAVE_AS_PDF, "path": shown, "pages": pages.len(), "bytes": bytes.len()}),
        );
    }
    if printer.starts_with('-') || printer.chars().any(char::is_control) || printer.len() > 256 {
        return Err(CmdError::Params("bad printer name".into()));
    }
    let bytes = pdf(pages)?;
    let job = system_print(printer, title, 1, &bytes)?;
    s.notices.push(Notice::Info(format!("Sent {} page(s) to {printer}.", pages.len())));
    Ok(json!({"printer": printer, "pages": pages.len(), "job": job}))
}

fn print(s: &mut Session, p: &Value) -> CmdResult {
    let printer = opt_str(p, "printer").map(str::trim).filter(|v| !v.is_empty()).unwrap_or(SAVE_AS_PDF);
    if printer.eq_ignore_ascii_case(SAVE_AS_PDF) {
        let path = opt_str(p, "path")
            .or_else(|| opt_str(p, "pdf_path"))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                CmdError::Params("`path` (the PDF file to write) required for Save as PDF".into())
            })?;
        let path = output_path(path)?;
        let (bytes, pages, _) = pdf_bytes(s, p)?;
        write_file(&path, &bytes)?;
        let shown = path.display().to_string();
        s.notices.push(Notice::Info(format!("Saved {pages} page(s) to {shown}.")));
        return Ok(json!({"printer": SAVE_AS_PDF, "path": shown, "pages": pages, "bytes": bytes.len()}));
    }
    if printer.starts_with('-') || printer.chars().any(char::is_control) || printer.len() > 256 {
        return Err(CmdError::Params("bad printer name".into()));
    }
    let copies = opt_u64(p, "copies").unwrap_or(1).clamp(1, MAX_COPIES);
    let (bytes, pages, title) = pdf_bytes(s, p)?;
    let job = system_print(printer, &title, copies, &bytes)?;
    s.notices.push(Notice::Info(format!("Sent {pages} page(s) to {printer}.")));
    Ok(json!({"printer": printer, "pages": pages, "copies": copies, "job": job}))
}

/// Sends a PDF to the system print service with CUPS `lp` (document on stdin). Returns lp's job line.
#[cfg(all(unix, not(target_arch = "wasm32")))]
fn system_print(printer: &str, title: &str, copies: u64, pdf: &[u8]) -> Result<String, CmdError> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let title: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned();
    let mut cmd = Command::new("lp");
    if !printer.eq_ignore_ascii_case(DEFAULT_PRINTER) {
        cmd.arg("-d").arg(printer);
    }
    cmd.arg("-n").arg(copies.to_string());
    if !title.is_empty() {
        cmd.arg("-t").arg(&title);
    }
    cmd.arg("--").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        CmdError::Failed(format!("no print service found (lp: {e}); use {SAVE_AS_PDF} instead"))
    })?;
    let wrote = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(pdf),
        None => Ok(()),
    };
    let out = child.wait_with_output().map_err(|e| CmdError::Failed(format!("lp: {e}")))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).chars().take(500).collect::<String>().trim().to_owned();
    if !out.status.success() {
        return Err(CmdError::Failed(format!("printing failed: {}", text(&out.stderr))));
    }
    wrote.map_err(|e| CmdError::Failed(format!("lp: {e}")))?;
    Ok(text(&out.stdout))
}

/// Printing other than Save as PDF is not implemented on this platform yet.
#[cfg(not(all(unix, not(target_arch = "wasm32"))))]
fn system_print(_printer: &str, _title: &str, _copies: u64, _pdf: &[u8]) -> Result<String, CmdError> {
    Err(CmdError::Disabled(format!(
        "printing to a printer is not available on this platform yet; use {SAVE_AS_PDF}"
    )))
}

/// The system default destination named in `lpstat -d` output ("system default destination: NAME"), if
/// any ("no system default destination" and anything else: none).
pub fn parse_default_destination(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (head, name) = l.split_once(':')?;
        let name = name.trim();
        (head.trim().eq_ignore_ascii_case("system default destination")
            && !name.is_empty()
            && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
    })
}

/// True when the system has a default printer (CUPS `lpstat -d`; other platforms: no).
#[cfg(all(unix, not(target_arch = "wasm32")))]
pub fn has_default_printer() -> bool {
    use std::process::{Command, Stdio};
    let Ok(out) = Command::new("lpstat").arg("-d").stdin(Stdio::null()).stderr(Stdio::null()).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(out.stdout.get(..out.stdout.len().min(4096)).unwrap_or(&[]));
    out.status.success() && parse_default_destination(&text).is_some()
}

#[cfg(not(all(unix, not(target_arch = "wasm32"))))]
pub fn has_default_printer() -> bool {
    false
}

/// Printers: Save as PDF first, then the system's destinations (CUPS `lpstat -e`), if any.
pub fn printers() -> Vec<String> {
    let mut out = vec![SAVE_AS_PDF.to_owned()];
    out.extend(system_printers());
    out
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn system_printers() -> Vec<String> {
    use std::process::{Command, Stdio};
    let Ok(out) = Command::new("lpstat").arg("-e").stdin(Stdio::null()).stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(out.stdout.get(..out.stdout.len().min(64 * 1024)).unwrap_or(&[]));
    let mut v: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('-') && !l.chars().any(char::is_control))
        .take(256)
        .map(str::to_owned)
        .collect();
    if !v.is_empty() {
        v.insert(0, DEFAULT_PRINTER.to_owned());
    }
    v
}

#[cfg(not(all(unix, not(target_arch = "wasm32"))))]
fn system_printers() -> Vec<String> {
    Vec::new()
}

fn preview_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let page = usize::try_from(opt_u64(p, "page").unwrap_or(0)).unwrap_or(usize::MAX);
    let scale = p.get("scale").and_then(Value::as_f64).map_or(0.5, |v| v as f32);
    let pv = preview(s, p, page, scale)?;
    let mut out = json!({
        "pages": pv.pages,
        "page": pv.page,
        "width": pv.width,
        "height": pv.height,
        "pageWidth": pv.page_size.0,
        "pageHeight": pv.page_size.1,
        "style": style_of(s, p),
    });
    if opt_bool(p, "rgba").unwrap_or(false)
        && let Some(obj) = out.as_object_mut()
    {
        obj.insert("rgba".into(), json!(base64::engine::general_purpose::STANDARD.encode(&pv.rgba)));
    }
    if opt_bool(p, "png").unwrap_or(false)
        && let Some(obj) = out.as_object_mut()
    {
        let canvas =
            mailcraft_render::Canvas { width: pv.width, height: pv.height, pixels: premultiply(&pv.rgba) };
        obj.insert("png".into(), json!(base64::engine::general_purpose::STANDARD.encode(canvas.to_png())));
    }
    Ok(out)
}

/// Straight-alpha RGBA → premultiplied (what [`mailcraft_render::Canvas`] holds).
fn premultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for &[r, g, b, a] in rgba.as_chunks::<4>().0 {
        let k = u16::from(a);
        for c in [r, g, b] {
            out.push(u8::try_from((u16::from(c) * k + 127) / 255).unwrap_or(255));
        }
        out.push(a);
    }
    out
}

fn printers_cmd(_s: &mut Session, _p: &Value) -> CmdResult {
    Ok(json!({"printers": printers(), "default": SAVE_AS_PDF}))
}

fn styles_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let styles: Vec<Value> = styles_for(s, p).iter().map(|(i, l)| json!({"id": i, "label": l})).collect();
    Ok(json!({"styles": styles, "default": style_of(s, p), "fields": FIELDS}))
}

fn page_setup(s: &mut Session, p: &Value) -> CmdResult {
    let style = style_of(s, p);
    let setup = setup_from(p, style);
    dialog(s, "pageSetup", json!({"style": style, "label": style_label(style), "setup": setup_json(&setup)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_session;

    fn first_mail(s: &Session) -> u64 {
        s.mailbox.mail.keys().next().unwrap().0
    }

    fn parse_pdf_pages(b: &[u8]) -> usize {
        assert!(b.starts_with(b"%PDF-1.7"));
        assert!(String::from_utf8_lossy(&b[b.len() - 32..]).contains("%%EOF"));
        let all = String::from_utf8_lossy(b);
        all.match_indices("/Type")
            .filter(|(i, _)| {
                let rest = all[i + 5..].trim_start();
                rest.starts_with("/Page") && !rest.starts_with("/Pages")
            })
            .count()
    }

    fn tmp() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    #[test]
    fn save_as_pdf_writes_a_valid_file_for_every_style() {
        let mut s = sample_session();
        let dir = tmp();
        let id = first_mail(&s);
        for (style, _) in STYLES {
            let path = dir.path().join(format!("{style}.pdf"));
            let r = s
                .run("print.print", &json!({"ids": [id], "style": style, "path": path.to_str().unwrap()}))
                .unwrap_or_else(|e| panic!("{style}: {e}"));
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(parse_pdf_pages(&bytes), r["pages"].as_u64().unwrap() as usize, "{style}");
            assert!(r["pages"].as_u64().unwrap() >= 1);
        }
        assert!(s.notices.iter().any(|n| matches!(n, Notice::Info(t) if t.starts_with("Saved"))));
    }

    #[test]
    fn save_as_pdf_needs_a_path_and_an_existing_folder() {
        let mut s = sample_session();
        let id = first_mail(&s);
        assert!(matches!(s.run("print.print", &json!({"ids": [id]})), Err(CmdError::Params(_))));
        let dir = tmp();
        let bad = dir.path().join("missing/out.pdf");
        assert!(s.run("print.print", &json!({"ids": [id], "path": bad.to_str().unwrap()})).is_err());
        assert!(!bad.exists());
    }

    #[test]
    fn preview_and_page_ranges() {
        let mut s = sample_session();
        let id = first_mail(&s);
        let r = s.run("print.preview", &json!({"ids": [id], "scale": 0.25, "png": true})).unwrap();
        assert_eq!(r["pages"], 1);
        assert_eq!(r["width"], 204);
        assert!(r["png"].as_str().unwrap().len() > 100);
        let pv = preview(&s, &json!({"ids": [id]}), 99, 0.5).unwrap();
        assert_eq!(pv.page, 0, "page index clamps");
        assert_eq!(pv.rgba.len(), (pv.width * pv.height * 4) as usize);
        assert_eq!(page_range(Some("2-3, 5,9-1,x"), 6), vec![1, 2, 4]);
        assert_eq!(page_range(Some(""), 3), vec![0, 1, 2]);
        assert_eq!(page_range(Some("7"), 3), vec![0, 1, 2]);
    }

    #[test]
    fn styles_follow_the_module_and_setup_round_trips() {
        let mut s = sample_session();
        let r = s.run("print.styles", &json!({})).unwrap();
        assert!(r["styles"].as_array().unwrap().iter().any(|v| v["id"] == "table"));
        let setup = PageSetup {
            paper: Paper::A4,
            orientation: Orientation::Landscape,
            header: ["[User Name]".into(), String::new(), String::new()],
            ..PageSetup::default()
        };
        let back = setup_from(&json!({"setup": setup_json(&setup)}), "memo");
        assert_eq!(back, setup);
        let junk = setup_from(
            &json!({"setup": {"paper": 5, "margins": {"top": -3, "left": 1e300}, "header": "x"}}),
            "monthly",
        );
        assert_eq!(junk, default_setup("monthly"));
        assert_eq!(junk.orientation, Orientation::Landscape);
        let r = s.run("print.pageSetup", &json!({"style": "Card Style"})).unwrap();
        assert_eq!(r["dialog"], "pageSetup");
        assert!(s.run("print.printers", &json!({})).unwrap()["printers"][0] == SAVE_AS_PDF);
    }

    #[test]
    fn calendar_and_contacts_jobs_use_the_mailbox() {
        let s = sample_session();
        let fonts = FontDb::bundled();
        let job = build(&s, &json!({"style": "monthly", "start": "2026-10-01", "end": "2026-10-31"}), &fonts)
            .unwrap();
        assert_eq!(job.pages.len(), 1);
        let job = build(&s, &json!({"style": "weekly", "start": "2026-10-04", "end": "2026-10-17"}), &fonts)
            .unwrap();
        assert_eq!(job.pages.len(), 2);
        assert!(
            build(&s, &json!({"style": "daily", "start": "2026-10-05", "end": "2026-10-01"}), &fonts)
                .is_err()
        );
        let job = build(&s, &json!({"style": "phoneDirectory"}), &fonts).unwrap();
        assert!(!job.pages.is_empty());
        assert_eq!(parse_day(Some("2026-02-30")), None);
        assert_eq!(parse_day(Some("2026-10-12")), days_from_civil(2026, 10, 12));
    }

    #[test]
    fn bad_printer_names_are_refused_before_spawning() {
        let mut s = sample_session();
        let id = first_mail(&s);
        for name in ["-oops", "bad\nname"] {
            assert!(matches!(
                s.run("print.print", &json!({"ids": [id], "printer": name})),
                Err(CmdError::Params(_))
            ));
        }
    }

    #[test]
    fn default_destination_is_read_from_lpstat() {
        assert_eq!(
            parse_default_destination("system default destination: Office_Laser\n").as_deref(),
            Some("Office_Laser")
        );
        assert_eq!(parse_default_destination("no system default destination\n"), None);
        assert_eq!(parse_default_destination(""), None);
        assert_eq!(parse_default_destination("system default destination: \n"), None);
        // Quick Print without direct printing (tests, tools) opens the Print page even with a printer.
        let mut s = sample_session();
        let id = first_mail(&s);
        s.selection.items = vec![mailcraft_model::ItemId(id)];
        s.selection.focus = Some(mailcraft_model::ItemId(id));
        assert!(!s.direct_print);
        assert_eq!(s.run("mail.quickPrint", &json!({"ids": [id]})).unwrap()["dialog"], "print");
    }

    #[test]
    fn older_print_commands_print_directly_when_given_a_path() {
        let mut s = sample_session();
        let dir = tmp();
        let id = first_mail(&s);
        s.selection.items = vec![mailcraft_model::ItemId(id)];
        s.selection.focus = Some(mailcraft_model::ItemId(id));
        // Without style / printer / path they still open the Print page.
        assert_eq!(s.run("mail.print", &json!({"ids": [id]})).unwrap()["dialog"], "print");
        assert_eq!(s.run("mail.quickPrint", &json!({"ids": [id]})).unwrap()["dialog"], "print");
        assert_eq!(s.run("file.print", &json!({"ids": [id]})).unwrap()["dialog"], "print");
        for cmd in ["mail.print", "mail.quickPrint", "file.print"] {
            let path = dir.path().join(format!("{cmd}.pdf"));
            let r = s.run(cmd, &json!({"ids": [id], "path": path.to_str().unwrap()})).unwrap();
            assert_eq!(r["printer"], SAVE_AS_PDF, "{cmd}: {r}");
            assert!(parse_pdf_pages(&std::fs::read(&path).unwrap()) >= 1, "{cmd}");
        }
        // A style alone goes to print.print, which needs a PDF path for the default printer.
        assert!(s.run("file.print", &json!({"ids": [id], "style": "table"})).is_err());
    }
}
