//! Module tables: the columns, sort field, filter and Conditional Formatting of the People Phone and List
//! views and of the Tasks views (Add Columns / Show Columns, View Settings › Columns, Sort, Filter,
//! Conditional Formatting).
//!
//! **Per view.** Each built-in view of a contacts or tasks folder keeps its own [`ViewCustom`] inside the
//! folder's [`ModuleView`] (`custom`, keyed by the view name), so switching from Phone to List and back keeps
//! both column sets. Nothing is stored while a view keeps its defaults: [`default_columns`], the view's own
//! order, no filter and [`default_rules`].
//!
//! **Cells.** [`cell`] is the text of a column for a row (a contact in People; a task, a flagged message or
//! a flagged contact in Tasks); [`sort_key`] orders rows by a column; [`matches`] applies a filter;
//! [`row_style`] applies the Conditional Formatting rules. The UI draws icons for the glyph columns (Icon,
//! Complete, Flag Status) and the text of the others.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::mail::{self, ColumnFormat, CustomFilter, FormatRule, RuleFont};
use super::modules::{self, ModuleKind, ModuleView, RowKind};
use crate::cmd::people::display_name;
use crate::cmd::util::opt_bool;
use crate::queries::RowStyle;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::dates::LocalTime;
use mailcraft_model::{
    Contact, DateTime, FlagState, FolderId, Importance, ItemId, ListTime, MailItem, Task, TaskPriority,
    TaskStatus,
};

/// Most columns a module table shows.
pub const MAX_COLUMNS: usize = 24;

/// A column of a module table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableColumn {
    pub id: &'static str,
    pub label: &'static str,
    /// Preferred width in px; 0 = shares the free space (Full Name, Subject).
    pub width: u16,
    /// Kept when the table is too narrow for every column.
    pub essential: bool,
}

const fn col(id: &'static str, label: &'static str, width: u16, essential: bool) -> TableColumn {
    TableColumn { id, label, width, essential }
}

/// Columns a contacts table can show (Show Columns › Available columns), in the dialog's order.
pub const CONTACT_COLUMNS: [TableColumn; 30] = [
    col("icon", "Icon", 24, true),
    col("flagStatus", "Flag Status", 26, true),
    col("fullName", "Full Name", 0, true),
    col("fileAs", "File As", 150, false),
    col("company", "Company", 140, false),
    col("jobTitle", "Job Title", 130, false),
    col("department", "Department", 120, false),
    col("office", "Office", 100, false),
    col("businessPhone", "Business Phone", 120, false),
    col("businessFax", "Business Fax", 110, false),
    col("homePhone", "Home Phone", 110, false),
    col("mobilePhone", "Mobile Phone", 110, false),
    col("email", "E-mail", 180, false),
    col("email2", "E-mail 2", 160, false),
    col("email3", "E-mail 3", 160, false),
    col("businessAddress", "Business Address", 200, false),
    col("homeAddress", "Home Address", 200, false),
    col("city", "City", 100, false),
    col("country", "Country/Region", 110, false),
    col("webPage", "Web Page", 150, false),
    col("imAddress", "IM Address", 140, false),
    col("profession", "Profession", 110, false),
    col("manager", "Manager's Name", 120, false),
    col("assistant", "Assistant's Name", 120, false),
    col("nickname", "Nickname", 100, false),
    col("spouse", "Spouse/Partner", 110, false),
    col("birthday", "Birthday", 90, false),
    col("anniversary", "Anniversary", 90, false),
    col("categories", "Categories", 120, false),
    col("sensitivity", "Sensitivity", 80, false),
];

/// Columns a tasks table can show, in the dialog's order.
pub const TASK_COLUMNS: [TableColumn; 16] = [
    col("icon", "Icon", 24, true),
    col("complete", "Complete", 22, true),
    col("priority", "Priority", 64, false),
    col("subject", "Subject", 0, true),
    col("status", "Status", 100, false),
    col("dueDate", "Due Date", 100, true),
    col("startDate", "Start Date", 100, false),
    col("percentComplete", "% Complete", 76, false),
    col("dateCompleted", "Date Completed", 104, false),
    col("inFolder", "In Folder", 110, false),
    col("categories", "Categories", 120, false),
    col("assignedTo", "Assigned To", 140, false),
    col("reminder", "Reminder Time", 140, false),
    col("modified", "Modified", 140, false),
    col("sensitivity", "Sensitivity", 80, false),
    col("flagStatus", "Flag Status", 26, true),
];

const PHONE: [&str; 10] = [
    "icon",
    "fullName",
    "company",
    "fileAs",
    "businessPhone",
    "businessFax",
    "homePhone",
    "mobilePhone",
    "categories",
    "flagStatus",
];
const LIST: [&str; 10] = [
    "icon",
    "fullName",
    "jobTitle",
    "company",
    "fileAs",
    "department",
    "businessPhone",
    "email",
    "categories",
    "flagStatus",
];
const DETAILED: [&str; 9] = [
    "icon",
    "complete",
    "subject",
    "status",
    "dueDate",
    "percentComplete",
    "inFolder",
    "categories",
    "flagStatus",
];
const SIMPLE: [&str; 5] = ["icon", "complete", "subject", "dueDate", "flagStatus"];
const PRIORITIZED: [&str; 6] = ["icon", "complete", "priority", "subject", "dueDate", "flagStatus"];
const COMPLETED: [&str; 6] = ["icon", "complete", "subject", "dueDate", "dateCompleted", "flagStatus"];
const ASSIGNED: [&str; 7] = ["icon", "complete", "subject", "assignedTo", "dueDate", "status", "flagStatus"];

/// Columns a module offers (none for Notes).
pub fn available(kind: ModuleKind) -> &'static [TableColumn] {
    match kind {
        ModuleKind::People => &CONTACT_COLUMNS,
        ModuleKind::Tasks => &TASK_COLUMNS,
        ModuleKind::Notes => &[],
    }
}

/// The column `id` of the module (ids are case-insensitive; labels are accepted too).
pub fn column(kind: ModuleKind, id: &str) -> Option<&'static TableColumn> {
    let want = id.trim();
    available(kind).iter().find(|c| c.id.eq_ignore_ascii_case(want) || c.label.eq_ignore_ascii_case(want))
}

/// Whether `view` of the module is a table with columns (People: Phone and List; every Tasks view).
pub fn has_table(kind: ModuleKind, view: &str) -> bool {
    match kind {
        ModuleKind::People => matches!(view, "Phone" | "List"),
        ModuleKind::Tasks => true,
        ModuleKind::Notes => false,
    }
}

/// Columns of a view nobody changed.
pub fn default_columns(kind: ModuleKind, view: &str) -> &'static [&'static str] {
    match (kind, view) {
        (ModuleKind::People, "Phone") => &PHONE,
        (ModuleKind::People, _) => &LIST,
        (ModuleKind::Tasks, "Detailed") => &DETAILED,
        (ModuleKind::Tasks, "Prioritized") => &PRIORITIZED,
        (ModuleKind::Tasks, "Completed") => &COMPLETED,
        (ModuleKind::Tasks, "Assigned") => &ASSIGNED,
        (ModuleKind::Tasks, _) => &SIMPLE,
        (ModuleKind::Notes, _) => &[],
    }
}

/// Sort by a column (View Settings › Sort, a click on a column header).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SortField {
    pub column: String,
    pub descending: bool,
}

/// What View Settings changed in one built-in view of a module folder (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ViewCustom {
    /// Column ids in order (empty = [`default_columns`]).
    pub columns: Vec<String>,
    /// `None` = the view's own order (File As for contacts, due date then subject for tasks).
    pub sort: Option<SortField>,
    pub filter: Option<CustomFilter>,
    /// `None` = [`default_rules`].
    pub rules: Option<Vec<FormatRule>>,
    /// Format Columns: a column's label and alignment (column id → settings; the display format is unused).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, ColumnFormat>,
}

impl ViewCustom {
    pub fn is_empty(&self) -> bool {
        *self == ViewCustom::default()
    }

    /// Stored or given state cleaned up for `view` of `kind` (unknown columns dropped, text capped).
    /// `view` is the built-in view the columns belong to (`None` for a saved view's entry, whose columns are
    /// kept as they are).
    fn sanitized(mut self, kind: ModuleKind, view: Option<&str>) -> ViewCustom {
        let mut seen: Vec<&str> = Vec::new();
        self.columns = self
            .columns
            .iter()
            .filter_map(|c| column(kind, c))
            .filter(|c| {
                let fresh = !seen.contains(&c.id);
                seen.push(c.id);
                fresh
            })
            .take(MAX_COLUMNS)
            .map(|c| c.id.to_owned())
            .collect();
        if let Some(view) = view
            && self.columns.iter().map(String::as_str).eq(default_columns(kind, view).iter().copied())
        {
            self.columns.clear();
        }
        self.sort = self.sort.and_then(|f| {
            column(kind, &f.column).map(|c| SortField { column: c.id.to_owned(), descending: f.descending })
        });
        self.filter = self.filter.map(CustomFilter::sanitized).filter(|f| !f.is_empty());
        self.rules = self.rules.map(|r| {
            r.into_iter().filter_map(|r| sanitize_rule(kind, r)).take(mail::MAX_RULES).collect::<Vec<_>>()
        });
        if self.rules.as_ref().is_some_and(|r| *r == default_rules(kind)) {
            self.rules = None;
        }
        self.formats = std::mem::take(&mut self.formats)
            .into_iter()
            .filter_map(|(id, f)| {
                let c = column(kind, &id)?;
                let label = clip(f.label.trim(), mail::MAX_NAME);
                let f = ColumnFormat {
                    label: if label == c.label { String::new() } else { label },
                    format: String::new(),
                    align: if matches!(f.align.as_str(), "left" | "center" | "right") {
                        f.align
                    } else {
                        String::new()
                    },
                };
                (f != ColumnFormat::default()).then(|| (c.id.to_owned(), f))
            })
            .take(MAX_COLUMNS * 2)
            .collect();
        self
    }
}

/// Cleans up stored per-view customisations (keys must be built-in views of the module).
pub(super) fn sanitize_custom(
    kind: ModuleKind,
    custom: BTreeMap<String, ViewCustom>,
) -> BTreeMap<String, ViewCustom> {
    custom
        .into_iter()
        .filter_map(|(key, c)| {
            // "@name": the folder's changes to the saved view `name` while it is shown.
            if let Some(name) = key.strip_prefix('@') {
                let ok = !name.trim().is_empty()
                    && name.chars().count() <= modules::MAX_VIEW_NAME
                    && !name.chars().any(char::is_control);
                let c = c.sanitized(kind, None);
                return (ok && !c.is_empty()).then_some((key, c));
            }
            let v = modules::canonical_view(kind, &key)?;
            if !has_table(kind, v) {
                return None;
            }
            let c = c.sanitized(kind, Some(v));
            (!c.is_empty()).then(|| (v.to_owned(), c))
        })
        .collect()
}

fn sanitize_rule(kind: ModuleKind, mut r: FormatRule) -> Option<FormatRule> {
    if !matches!(r.kind.as_str(), "overdue" | "completed" | "custom") {
        return None;
    }
    // The mail rules accept "overdue" and "custom"; "completed" is a module kind.
    let completed = r.kind == "completed";
    if completed {
        r.kind = "custom".into();
    }
    let mut r = mail::sanitize_rule(r)?;
    if completed {
        r.kind = "completed".into();
    }
    if kind == ModuleKind::Notes {
        return None;
    }
    Some(r)
}

/// The customisation of the folder's current view.
pub fn custom(st: &ModuleView) -> ViewCustom {
    st.custom.get(&active_key(st)).cloned().unwrap_or_default()
}

/// Key of the customisation in effect: the built-in view's name, or "@name" while the saved view `name` is
/// shown (so a saved view's columns never overwrite the built-in view's).
pub fn active_key(st: &ModuleView) -> String {
    if st.name.is_empty() { st.view.clone() } else { format!("@{}", st.name) }
}

/// Columns of the current view of a module state (empty when the view is not a table).
pub fn columns_of(kind: ModuleKind, st: &ModuleView) -> Vec<&'static TableColumn> {
    if !has_table(kind, &st.view) {
        return Vec::new();
    }
    let c = custom(st);
    let ids: Vec<&str> = if c.columns.is_empty() {
        default_columns(kind, &st.view).to_vec()
    } else {
        c.columns.iter().map(String::as_str).collect()
    };
    ids.into_iter().filter_map(|id| column(kind, id)).collect()
}

/// Columns of the folder's current view.
pub fn columns(s: &Session, folder: FolderId) -> Vec<&'static TableColumn> {
    modules::current(s, folder).map(|(kind, st)| columns_of(kind, &st)).unwrap_or_default()
}

/// A column as a table draws it: its label (Format Columns may rename it) and alignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownColumn {
    pub col: &'static TableColumn,
    pub label: String,
    /// "left", "center" or "right".
    pub align: &'static str,
}

/// Default alignment of a column: glyphs centred, numbers right, text left.
pub fn default_align(c: &TableColumn) -> &'static str {
    match c.id {
        "percentComplete" => "right",
        _ if c.width > 0 && c.width <= 30 => "center",
        _ => "left",
    }
}

/// The shown columns of a module state with their labels and alignment.
pub fn shown_of(kind: ModuleKind, st: &ModuleView) -> Vec<ShownColumn> {
    let formats = custom(st).formats;
    columns_of(kind, st)
        .into_iter()
        .map(|col| {
            let f = formats.get(col.id);
            let label =
                f.map(|f| f.label.clone()).filter(|l| !l.is_empty()).unwrap_or_else(|| col.label.to_owned());
            let align = match f.map(|f| f.align.as_str()) {
                Some("left") => "left",
                Some("center") => "center",
                Some("right") => "right",
                _ => default_align(col),
            };
            ShownColumn { col, label, align }
        })
        .collect()
}

/// The shown columns of the folder's current view with their labels and alignment.
pub fn shown(s: &Session, folder: FolderId) -> Vec<ShownColumn> {
    modules::current(s, folder).map(|(kind, st)| shown_of(kind, &st)).unwrap_or_default()
}

/// Built-in rules of a module view nobody changed: in Tasks, overdue rows take Options › Tasks › Overdue
/// task color and completed rows the Completed task color, struck through (an empty rule colour means
/// those options).
pub fn default_rules(kind: ModuleKind) -> Vec<FormatRule> {
    let rule = |name: &str, kind: &str, font: RuleFont| FormatRule {
        name: name.to_owned(),
        on: true,
        kind: kind.to_owned(),
        condition: CustomFilter::default(),
        font,
    };
    match kind {
        ModuleKind::Tasks => vec![
            rule("Overdue tasks", "overdue", RuleFont::default()),
            rule("Completed tasks", "completed", RuleFont { strike: true, ..Default::default() }),
        ],
        ModuleKind::People => vec![rule("Overdue flagged contacts", "overdue", RuleFont::default())],
        ModuleKind::Notes => Vec::new(),
    }
}

/// Conditional Formatting rules of the current view.
pub fn rules_of(kind: ModuleKind, st: &ModuleView) -> Vec<FormatRule> {
    custom(st).rules.unwrap_or_else(|| default_rules(kind))
}

/// Conditional Formatting rules of the folder's current view.
pub fn rules(s: &Session, folder: FolderId) -> Vec<FormatRule> {
    modules::current(s, folder).map(|(kind, st)| rules_of(kind, &st)).unwrap_or_default()
}

/// The View Settings filter of the folder's current view.
pub fn filter(s: &Session, folder: FolderId) -> Option<CustomFilter> {
    modules::current(s, folder).and_then(|(_, st)| custom(&st).filter)
}

/// The sort field of the folder's current view.
pub fn sort(s: &Session, folder: FolderId) -> Option<SortField> {
    modules::current(s, folder).and_then(|(_, st)| custom(&st).sort)
}

// ---------------------------------------------------------------------------------------------------------
// Items and cells

/// What a module row shows.
#[derive(Clone, Copy, Debug)]
pub enum Item<'a> {
    Task(&'a Task),
    Mail(&'a MailItem),
    Contact(&'a Contact),
}

/// The item behind a module row.
pub fn item(s: &Session, kind: RowKind, id: ItemId) -> Option<Item<'_>> {
    match kind {
        RowKind::Task => s.mailbox.tasks.get(&id).map(Item::Task),
        RowKind::Mail => s.mailbox.mail.get(&id).map(Item::Mail),
        RowKind::Contact => s.mailbox.contacts.get(&id).map(Item::Contact),
        RowKind::Note => None,
    }
}

fn phone(c: &Contact, kind: &str) -> String {
    c.phones.iter().find(|(k, _)| k.eq_ignore_ascii_case(kind)).map(|(_, n)| n.clone()).unwrap_or_default()
}

fn address(c: &Contact, kind: &str) -> String {
    let Some(a) = c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case(kind)) else {
        return String::new();
    };
    let line2 = [a.city.trim(), a.region.trim(), a.postal_code.trim()]
        .iter()
        .filter(|x| !x.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    [a.street.trim(), line2.as_str(), a.country.trim()]
        .iter()
        .filter(|x| !x.is_empty())
        .map(|x| x.replace('\n', ", "))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The business address, else the first one.
fn main_address(c: &Contact) -> Option<&mailcraft_model::contact::PostalAddress> {
    c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case("Business")).or(c.addresses.first())
}

/// "Fri 10/9/2026", or "None".
fn day_text(t: Option<DateTime>, tz: i32) -> String {
    match t {
        Some(t) => format!("{} {}", LocalTime::of(t, tz).weekday.short(), ListTime::date(t, tz)),
        None => "None".to_owned(),
    }
}

fn status_label(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::NotStarted => "Not Started",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed => "Completed",
        TaskStatus::WaitingOnSomeoneElse => "Waiting on someone else",
        TaskStatus::Deferred => "Deferred",
    }
}

fn priority_label(p: TaskPriority) -> &'static str {
    match p {
        TaskPriority::High => "High",
        TaskPriority::Normal => "Normal",
        TaskPriority::Low => "Low",
    }
}

fn importance_priority(i: Importance) -> TaskPriority {
    match i {
        Importance::High => TaskPriority::High,
        Importance::Low => TaskPriority::Low,
        Importance::Normal => TaskPriority::Normal,
    }
}

fn flag_text(f: &FlagState) -> &'static str {
    match f {
        FlagState::None => "Unflagged",
        FlagState::Flagged { .. } => "Flagged",
        FlagState::Complete { .. } => "Completed",
    }
}

/// Text of the contact column `id` (the People tables).
pub fn contact_cell(c: &Contact, id: &str) -> String {
    match id {
        "icon" => if crate::cmd::people::is_group(c) { "Contact Group" } else { "Contact" }.to_owned(),
        "flagStatus" => flag_text(&c.flag).to_owned(),
        "fullName" if crate::cmd::people::is_group(c) => display_name(c),
        "fullName" => c.full_name(),
        "fileAs" => display_name(c),
        "company" => c.company.clone(),
        "jobTitle" => c.job_title.clone(),
        "department" => c.department.clone(),
        "office" => c.office.clone(),
        "businessPhone" => phone(c, "Business"),
        "businessFax" => phone(c, "Business Fax"),
        "homePhone" => phone(c, "Home"),
        "mobilePhone" => phone(c, "Mobile"),
        "email" | "email2" | "email3" => {
            let n = match id {
                "email2" => 1,
                "email3" => 2,
                _ => 0,
            };
            if crate::cmd::people::is_group(c) {
                return String::new();
            }
            c.emails.get(n).cloned().unwrap_or_default()
        }
        "businessAddress" => address(c, "Business"),
        "homeAddress" => address(c, "Home"),
        "city" => main_address(c).map(|a| a.city.clone()).unwrap_or_default(),
        "country" => main_address(c).map(|a| a.country.clone()).unwrap_or_default(),
        "webPage" => c.web_page.clone(),
        "imAddress" => c.im_address.clone(),
        "profession" => c.profession.clone(),
        "manager" => c.manager.clone(),
        "assistant" => c.assistant.clone(),
        "nickname" => c.nickname.clone(),
        "spouse" => c.spouse.clone(),
        "birthday" => c.birthday.clone().unwrap_or_default(),
        "anniversary" => c.anniversary.clone().unwrap_or_default(),
        "categories" => c.categories.join(", "),
        "sensitivity" => if c.private { "Private" } else { "Normal" }.to_owned(),
        _ => String::new(),
    }
}

/// Text of the tasks column `id` for a row of the Tasks module (a task, a flagged message or a flagged
/// contact).
pub fn task_cell(s: &Session, it: Item<'_>, id: &str) -> String {
    let tz = s.tz_offset_min;
    let folder_name = |f: FolderId| s.mailbox.folder(f).map(|x| x.name.clone()).unwrap_or_default();
    match it {
        Item::Task(t) => match id {
            "icon" => if t.is_assigned_by_me() { "Task Request" } else { "Task" }.to_owned(),
            "complete" => if t.is_complete() { "Complete" } else { "Not Complete" }.to_owned(),
            "priority" => priority_label(t.priority).to_owned(),
            "subject" => t.subject.clone(),
            "status" => status_label(t.status).to_owned(),
            "dueDate" => day_text(t.due, tz),
            "startDate" => day_text(t.start, tz),
            "percentComplete" => format!("{}%", t.percent_complete.min(100)),
            "dateCompleted" => day_text(t.completed, tz),
            "inFolder" => folder_name(t.folder),
            "categories" => t.categories.join(", "),
            "assignedTo" => t.assigned_to.join("; "),
            "reminder" => t.reminder.map_or_else(|| "None".to_owned(), |r| ListTime::short_date_time(r, tz)),
            "modified" => t.modified.map_or_else(String::new, |m| ListTime::short_date_time(m, tz)),
            "sensitivity" => if t.private { "Private" } else { "Normal" }.to_owned(),
            "flagStatus" => if t.is_complete() { "Completed" } else { "Flagged" }.to_owned(),
            _ => String::new(),
        },
        Item::Mail(m) => {
            let (start, due) = match &m.flag {
                FlagState::Flagged { start, due, .. } => (*start, *due),
                _ => (None, None),
            };
            match id {
                "icon" => "Flagged Message".to_owned(),
                "complete" => {
                    if matches!(m.flag, FlagState::Complete { .. }) { "Complete" } else { "Not Complete" }
                        .to_owned()
                }
                "priority" => priority_label(importance_priority(m.importance)).to_owned(),
                "subject" => m.subject.clone(),
                "dueDate" => day_text(due, tz),
                "startDate" => day_text(start, tz),
                "inFolder" => folder_name(m.folder),
                "categories" => m.categories.join(", "),
                "sensitivity" => match m.sensitivity {
                    mailcraft_model::Sensitivity::Normal => "Normal",
                    mailcraft_model::Sensitivity::Personal => "Personal",
                    mailcraft_model::Sensitivity::Private => "Private",
                    mailcraft_model::Sensitivity::Confidential => "Confidential",
                }
                .to_owned(),
                "flagStatus" => flag_text(&m.flag).to_owned(),
                _ => String::new(),
            }
        }
        Item::Contact(c) => {
            let (start, due) = match &c.flag {
                FlagState::Flagged { start, due, .. } => (*start, *due),
                _ => (None, None),
            };
            match id {
                "icon" => "Contact".to_owned(),
                "complete" => {
                    if matches!(c.flag, FlagState::Complete { .. }) { "Complete" } else { "Not Complete" }
                        .to_owned()
                }
                "priority" => "Normal".to_owned(),
                "subject" => display_name(c),
                "dueDate" => day_text(due, tz),
                "startDate" => day_text(start, tz),
                "inFolder" => folder_name(c.folder),
                "categories" => c.categories.join(", "),
                "sensitivity" => if c.private { "Private" } else { "Normal" }.to_owned(),
                "flagStatus" => flag_text(&c.flag).to_owned(),
                _ => String::new(),
            }
        }
    }
}

/// Text of column `id` for a row of a `kind` table.
pub fn cell(s: &Session, kind: ModuleKind, it: Item<'_>, id: &str) -> String {
    match (kind, it) {
        (ModuleKind::People, Item::Contact(c)) => contact_cell(c, id),
        (ModuleKind::People, _) => String::new(),
        _ => task_cell(s, it, id),
    }
}

/// Sort key of a cell.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    Num(i64),
    Text(String),
}

fn date_key(t: Option<DateTime>) -> SortKey {
    SortKey::Num(t.map_or(i64::MAX, |d| d.unix))
}

fn flag_rank(f: &FlagState) -> i64 {
    match f {
        FlagState::Flagged { .. } => 0,
        FlagState::Complete { .. } => 1,
        FlagState::None => 2,
    }
}

fn priority_rank(p: TaskPriority) -> i64 {
    match p {
        TaskPriority::High => 0,
        TaskPriority::Normal => 1,
        TaskPriority::Low => 2,
    }
}

/// How column `id` orders a row: dates by time (none last), numbers by value, the rest by text.
pub fn sort_key(s: &Session, kind: ModuleKind, it: Item<'_>, id: &str) -> SortKey {
    let flag_dates = |f: &FlagState| match f {
        FlagState::Flagged { start, due, .. } => (*start, *due),
        _ => (None, None),
    };
    match (it, id) {
        (Item::Task(t), "dueDate") => date_key(t.due),
        (Item::Task(t), "startDate") => date_key(t.start),
        (Item::Task(t), "dateCompleted") => date_key(t.completed),
        (Item::Task(t), "reminder") => date_key(t.reminder),
        (Item::Task(t), "modified") => date_key(t.modified),
        (Item::Task(t), "percentComplete") => SortKey::Num(i64::from(t.percent_complete)),
        (Item::Task(t), "priority") => SortKey::Num(priority_rank(t.priority)),
        (Item::Task(t), "complete" | "flagStatus") => SortKey::Num(i64::from(t.is_complete())),
        (Item::Mail(m), "dueDate") => date_key(flag_dates(&m.flag).1),
        (Item::Mail(m), "startDate") => date_key(flag_dates(&m.flag).0),
        (Item::Mail(m), "priority") => SortKey::Num(priority_rank(importance_priority(m.importance))),
        (Item::Mail(m), "flagStatus" | "complete") => SortKey::Num(flag_rank(&m.flag)),
        (Item::Contact(c), "dueDate") if kind == ModuleKind::Tasks => date_key(flag_dates(&c.flag).1),
        (Item::Contact(c), "startDate") if kind == ModuleKind::Tasks => date_key(flag_dates(&c.flag).0),
        (Item::Contact(c), "flagStatus" | "complete") => SortKey::Num(flag_rank(&c.flag)),
        (Item::Contact(_), "priority") if kind == ModuleKind::Tasks => SortKey::Num(1),
        (_, "dueDate" | "startDate" | "dateCompleted" | "reminder" | "modified") => SortKey::Num(i64::MAX),
        _ => SortKey::Text(cell(s, kind, it, id).to_lowercase()),
    }
}

/// Compares two rows by the sort field.
pub fn compare(s: &Session, kind: ModuleKind, f: &SortField, a: Item<'_>, b: Item<'_>) -> Ordering {
    let o = sort_key(s, kind, a, &f.column).cmp(&sort_key(s, kind, b, &f.column));
    if f.descending { o.reverse() } else { o }
}

fn categories_of<'a>(it: Item<'a>) -> &'a [String] {
    match it {
        Item::Task(t) => &t.categories,
        Item::Mail(m) => &m.categories,
        Item::Contact(c) => &c.categories,
    }
}

/// Whether a module row meets every condition of a View Settings filter. Module filters use the words
/// (subject or name; with `in_body` also the notes), the importance (task priority), Flagged (a due date or
/// a follow-up flag), the category and, for tasks, Modified in the last N days.
pub fn matches(s: &Session, it: Item<'_>, f: &CustomFilter) -> bool {
    if !f.words.trim().is_empty() {
        let mut hay = match it {
            Item::Task(t) => t.subject.to_lowercase(),
            Item::Mail(m) => m.subject.to_lowercase(),
            Item::Contact(c) => {
                format!("{}\n{}\n{}\n{}", display_name(c), c.full_name(), c.company, c.emails.join(" "))
                    .to_lowercase()
            }
        };
        if f.in_body {
            hay.push('\n');
            match it {
                Item::Task(t) => hay.push_str(&t.body.to_lowercase()),
                Item::Mail(m) => hay.push_str(&m.body_text().to_lowercase()),
                Item::Contact(c) => hay.push_str(&c.notes.to_lowercase()),
            }
        }
        if !f.words.split_whitespace().all(|w| hay.contains(&w.to_lowercase())) {
            return false;
        }
    }
    let priority = match it {
        Item::Task(t) => t.priority,
        Item::Mail(m) => importance_priority(m.importance),
        Item::Contact(_) => TaskPriority::Normal,
    };
    let importance = match priority {
        TaskPriority::High => "high",
        TaskPriority::Normal => "normal",
        TaskPriority::Low => "low",
    };
    if !f.importance.is_empty() && f.importance != importance {
        return false;
    }
    if f.flagged {
        let flagged = match it {
            Item::Task(t) => t.due.is_some() || t.start.is_some(),
            Item::Mail(m) => matches!(m.flag, FlagState::Flagged { .. }),
            Item::Contact(c) => matches!(c.flag, FlagState::Flagged { .. }),
        };
        if !flagged {
            return false;
        }
    }
    let cat = f.category.trim();
    if !cat.is_empty() && !categories_of(it).iter().any(|c| c.trim().eq_ignore_ascii_case(cat)) {
        return false;
    }
    if f.days > 0
        && let Item::Task(t) = it
    {
        let since = s.now().unix.saturating_sub(i64::from(f.days).saturating_mul(86_400));
        if t.modified.is_none_or(|m| m.unix < since) {
            return false;
        }
    }
    true
}

/// Open, and due before today (a task's due date, a message's or contact's follow-up due date).
pub fn is_overdue(s: &Session, it: Item<'_>) -> bool {
    let tz = s.tz_offset_min;
    let today = s.now().day_number(tz);
    let flag_due = |f: &FlagState| match f {
        FlagState::Flagged { due: Some(d), .. } => d.day_number(tz) < today,
        _ => false,
    };
    match it {
        Item::Task(t) => !t.is_complete() && t.due.is_some_and(|d| d.day_number(tz) < today),
        Item::Mail(m) => flag_due(&m.flag),
        Item::Contact(c) => flag_due(&c.flag),
    }
}

/// A completed task, or a follow-up flag marked complete.
pub fn is_completed(it: Item<'_>) -> bool {
    match it {
        Item::Task(t) => t.is_complete(),
        Item::Mail(m) => matches!(m.flag, FlagState::Complete { .. }),
        Item::Contact(c) => matches!(c.flag, FlagState::Complete { .. }),
    }
}

/// What the rules that match a row do to its font (top to bottom; a later colour wins). A built-in rule
/// with no colour leaves `color` empty and sets `overdue` / `completed` so the UI can use the Options
/// colours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleRowStyle {
    pub font: RowStyle,
    /// A matching Overdue rule without its own colour.
    pub overdue: bool,
    /// A matching Completed rule without its own colour.
    pub completed: bool,
}

/// Applies Conditional Formatting `rules` to a row.
pub fn row_style(s: &Session, rules: &[FormatRule], it: Item<'_>) -> ModuleRowStyle {
    let mut st = ModuleRowStyle::default();
    for r in rules.iter().filter(|r| r.on) {
        let hit = match r.kind.as_str() {
            "overdue" => is_overdue(s, it),
            "completed" => is_completed(it),
            "custom" => !r.condition.is_empty() && matches(s, it, &r.condition),
            _ => false,
        };
        if !hit {
            continue;
        }
        st.font.bold |= r.font.bold;
        st.font.italic |= r.font.italic;
        st.font.underline |= r.font.underline;
        st.font.strike |= r.font.strike;
        if r.font.color.is_empty() {
            match r.kind.as_str() {
                "overdue" => st.overdue = true,
                "completed" => st.completed = true,
                _ => {}
            }
        } else {
            st.font.color = Some(r.font.color.clone());
            st.overdue = false;
            st.completed = false;
        }
    }
    st
}

// ---------------------------------------------------------------------------------------------------------
// Commands (the `view.*` commands call these in contacts and tasks folders)

/// Whether the selected folder shows a module table (People Phone / List, every Tasks view).
pub fn table_here(s: &Session) -> Option<(FolderId, ModuleKind, ModuleView)> {
    let folder = s.selection.folder;
    let (kind, st) = modules::current(s, folder)?;
    has_table(kind, &st.view).then_some((folder, kind, st))
}

fn update(
    s: &mut Session,
    folder: FolderId,
    kind: ModuleKind,
    f: impl FnOnce(&mut ViewCustom),
) -> Result<(), CmdError> {
    let mut st = modules::state(s, folder, kind);
    let mut c = custom(&st);
    f(&mut c);
    let key = active_key(&st);
    let c = c.sanitized(kind, if st.name.is_empty() { Some(st.view.as_str()) } else { None });
    if c.is_empty() {
        st.custom.remove(&key);
    } else {
        st.custom.insert(key, c);
    }
    modules::store(s, folder, kind, &st);
    modules::sync_saved(s, folder)
}

fn clip(t: &str, max: usize) -> String {
    t.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Show Columns parameters: the shown ids and every column of the module.
pub fn columns_json(s: &Session, folder: FolderId, kind: ModuleKind) -> Value {
    let st = modules::state(s, folder, kind);
    json!({
        "folder": folder.0,
        "module": kind.name(),
        "view": st.view,
        "columns": columns_of(kind, &st).iter().map(|c| c.id).collect::<Vec<_>>(),
        "available": available(kind).iter().map(|c| json!({"id": c.id, "label": c.label})).collect::<Vec<_>>(),
    })
}

fn unknown(id: &str) -> CmdError {
    CmdError::Params(format!("unknown column `{}`", clip(id, 40)))
}

/// `view.columns` in a module table: `columns` (ids in order), `add` / `remove` one, `reset` (the view's
/// default columns). Widths are not kept for module tables.
pub(crate) fn set_columns(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let st = modules::state(s, folder, kind);
    let mut cols: Vec<String> = columns_of(kind, &st).iter().map(|c| c.id.to_owned()).collect();
    if opt_bool(p, "reset") == Some(true) {
        cols = default_columns(kind, &st.view).iter().map(|c| (*c).to_owned()).collect();
    }
    if let Some(list) = p.get("columns").filter(|x| !x.is_null()) {
        let list =
            list.as_array().ok_or_else(|| CmdError::Params("`columns` must be a list of ids".into()))?;
        let mut out: Vec<String> = Vec::new();
        for c in list {
            let id = c.as_str().ok_or_else(|| CmdError::Params("`columns` must be a list of ids".into()))?;
            let def = column(kind, id).ok_or_else(|| unknown(id))?;
            if !out.iter().any(|x| x == def.id) {
                out.push(def.id.to_owned());
            }
        }
        if out.is_empty() {
            return Err(CmdError::Params("a view shows at least one column".into()));
        }
        if out.len() > MAX_COLUMNS {
            return Err(CmdError::Params(format!("a view shows at most {MAX_COLUMNS} columns")));
        }
        cols = out;
    }
    if let Some(id) = p.get("add").and_then(Value::as_str) {
        let def = column(kind, id).ok_or_else(|| unknown(id))?;
        if !cols.iter().any(|x| x == def.id) {
            if cols.len() >= MAX_COLUMNS {
                return Err(CmdError::Params(format!("a view shows at most {MAX_COLUMNS} columns")));
            }
            cols.push(def.id.to_owned());
        }
    }
    if let Some(id) = p.get("remove").and_then(Value::as_str) {
        let def = column(kind, id).ok_or_else(|| unknown(id))?;
        if cols.len() <= 1 && cols.iter().any(|x| x == def.id) {
            return Err(CmdError::Params("a view shows at least one column".into()));
        }
        cols.retain(|x| x != def.id);
    }
    update(s, folder, kind, |c| c.columns = cols)?;
    Ok(columns_json(s, folder, kind))
}

/// `view.sortBy` in a module table: `field` (a column id; null = the view's own order) and `dir`.
pub(crate) fn sort_by(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let descending = match p.get("dir").and_then(Value::as_str) {
        None => false,
        Some("asc") => false,
        Some("desc") => true,
        Some(other) => {
            return Err(CmdError::Params(format!("`dir` must be asc or desc, not `{}`", clip(other, 20))));
        }
    };
    let field = match p.get("field") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => {
            let def = column(kind, id).ok_or_else(|| unknown(id))?;
            Some(SortField { column: def.id.to_owned(), descending })
        }
        Some(_) => return Err(CmdError::Params("`field` must be a column id or null".into())),
    };
    update(s, folder, kind, |c| c.sort = field.clone())?;
    Ok(json!({
        "field": field.as_ref().map(|f| f.column.clone()),
        "dir": if field.as_ref().is_some_and(|f| f.descending) { "desc" } else { "asc" },
        "module": kind.name(),
    }))
}

/// `view.customFilter` in a module table: without `filter` the Filter dialog opens.
pub(crate) fn set_filter(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let Some(f) = p.get("filter") else {
        let current = filter(s, folder).unwrap_or_default();
        return Ok(super::dialog(
            s,
            "viewFilter",
            json!({ "folder": folder.0, "filter": current, "module": kind.name() }),
        ));
    };
    let parsed = if f.is_null() {
        None
    } else {
        let f: CustomFilter = serde_json::from_value(f.clone())
            .map_err(|_| CmdError::Params("`filter` must be a filter object or null".into()))?;
        Some(f.sanitized()).filter(|f| !f.is_empty())
    };
    update(s, folder, kind, |c| c.filter = parsed)?;
    let now = filter(s, folder);
    Ok(json!({
        "on": now.is_some(),
        "description": describe_filter(kind, &now.clone().unwrap_or_default()),
        "filter": now.unwrap_or_default(),
    }))
}

/// `view.conditionalFormatting` in a module table: without `rules` (or `reset`) the dialog opens.
pub(crate) fn set_rules(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    if opt_bool(p, "reset") == Some(true) {
        update(s, folder, kind, |c| c.rules = None)?;
        return Ok(json!({ "rules": default_rules(kind), "reset": true }));
    }
    let Some(list) = p.get("rules").filter(|x| !x.is_null()) else {
        let current = rules(s, folder);
        return Ok(super::dialog(
            s,
            "viewFormat",
            json!({ "folder": folder.0, "kind": "rules", "rules": current, "module": kind.name() }),
        ));
    };
    let list = list.as_array().ok_or_else(|| CmdError::Params("`rules` must be a list".into()))?;
    if list.len() > mail::MAX_RULES {
        return Err(CmdError::Params(format!("a view has at most {} rules", mail::MAX_RULES)));
    }
    let mut out = Vec::new();
    for r in list {
        let rule: FormatRule = serde_json::from_value(r.clone())
            .map_err(|_| CmdError::Params("a rule needs a name, a kind and a font".into()))?;
        let rule = sanitize_rule(kind, rule)
            .ok_or_else(|| CmdError::Params("a rule's kind is overdue, completed or custom".into()))?;
        out.push(rule);
    }
    update(s, folder, kind, |c| c.rules = Some(out))?;
    Ok(json!({ "rules": rules(s, folder) }))
}

/// `view.formatColumns` in a module table: without `column` the Format Columns dialog opens; with it the
/// column's `label` and `align` change (empty strings back to the defaults). Module columns have one
/// display format, so `format` is ignored.
pub(crate) fn format_columns(s: &mut Session, folder: FolderId, kind: ModuleKind, p: &Value) -> CmdResult {
    let st = modules::state(s, folder, kind);
    let Some(id) = p.get("column").and_then(Value::as_str) else {
        let formats = custom(&st).formats;
        let list: Vec<Value> = shown_of(kind, &st)
            .iter()
            .map(|c| {
                json!({
                    "id": c.col.id,
                    "label": c.label,
                    "defaultLabel": c.col.label,
                    "format": "text",
                    "formats": [{"id": "text", "label": "Text"}],
                    "align": c.align,
                    "custom": formats.contains_key(c.col.id),
                })
            })
            .collect();
        return Ok(super::dialog(
            s,
            "viewFormat",
            json!({ "folder": folder.0, "kind": "columns", "columns": list, "module": kind.name() }),
        ));
    };
    let def = column(kind, id).ok_or_else(|| unknown(id))?;
    let mut f = custom(&st).formats.get(def.id).cloned().unwrap_or_default();
    if let Some(label) = p.get("label").and_then(Value::as_str) {
        if label.trim().chars().count() > mail::MAX_NAME {
            return Err(CmdError::Params(format!("column labels are at most {} characters", mail::MAX_NAME)));
        }
        f.label = label.trim().to_owned();
    }
    if let Some(align) = p.get("align").and_then(Value::as_str) {
        f.align = match align {
            "left" | "center" | "right" | "" => align.to_owned(),
            other => {
                return Err(CmdError::Params(format!(
                    "`align` must be left, center or right, not `{}`",
                    clip(other, 20)
                )));
            }
        };
    }
    update(s, folder, kind, |c| {
        c.formats.insert(def.id.to_owned(), f);
    })?;
    let now = shown(s, folder).into_iter().find(|c| c.col.id == def.id);
    Ok(json!({
        "column": def.id,
        "label": now.as_ref().map_or(def.label.to_owned(), |c| c.label.clone()),
        "align": now.as_ref().map_or(default_align(def), |c| c.align),
    }))
}

/// One-line description of a module filter ("Off" when empty).
pub fn describe_filter(kind: ModuleKind, f: &CustomFilter) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !f.words.is_empty() {
        let place = match (kind, f.in_body) {
            (ModuleKind::People, false) => "name",
            (ModuleKind::People, true) => "name or notes",
            (_, false) => "subject",
            (_, true) => "subject or notes",
        };
        parts.push(format!("\"{}\" in the {place}", f.words));
    }
    if !f.importance.is_empty() {
        parts.push(format!("{} priority", f.importance));
    }
    if f.flagged {
        parts.push("flagged".into());
    }
    if !f.category.is_empty() {
        parts.push(format!("category {}", f.category));
    }
    if f.days > 0 && kind == ModuleKind::Tasks {
        parts.push(format!("modified in the last {} days", f.days));
    }
    if parts.is_empty() { "Off".to_owned() } else { parts.join(", ") }
}

/// The View Settings summary of a module table (the Advanced View Settings dialog's descriptions).
pub fn summary(folder: FolderId, kind: ModuleKind, st: &ModuleView) -> Value {
    let c = custom(st);
    let cols: Vec<&str> = columns_of(kind, st).iter().map(|c| c.label).collect();
    let rules = rules_of(kind, st);
    let sort = c.sort.as_ref().and_then(|f| {
        column(kind, &f.column)
            .map(|d| format!("{} ({})", d.label, if f.descending { "descending" } else { "ascending" }))
    });
    json!({
        "folder": folder.0,
        "table": has_table(kind, &st.view),
        "columns": cols.join(", "),
        "sort": sort.unwrap_or_else(|| "The view's own order".to_owned()),
        "sortField": c.sort.as_ref().map(|f| f.column.clone()),
        "sortDescending": c.sort.as_ref().is_some_and(|f| f.descending),
        "filter": describe_filter(kind, &c.filter.unwrap_or_default()),
        "rules": format!("{} of {} rules on", rules.iter().filter(|r| r.on).count(), rules.len()),
        "formats": if c.formats.is_empty() { "Default formats".to_owned() } else { format!("{} columns formatted", c.formats.len()) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use crate::{Notice, Selection};
    use mailcraft_model::FolderRole;

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn select_folder(s: &mut Session, r: FolderRole) -> FolderId {
        let f = role(s, r);
        s.selection = Selection { folder: f, items: vec![], focus: None };
        f
    }

    fn ids(s: &Session, folder: FolderId) -> Vec<u64> {
        modules::rows(s, folder).iter().map(|r| r.id.0).collect()
    }

    #[test]
    fn people_tables_have_flag_and_category_columns_and_a_column_chooser() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Contacts);
        assert!(s.enabled("view.columns").is_some(), "the People view is not a table");
        run(&mut s, "view.changeView", json!({"name": "Phone"}));
        let ids_of = |s: &Session| columns(s, folder).iter().map(|c| c.id).collect::<Vec<_>>();
        assert!(ids_of(&s).contains(&"flagStatus") && ids_of(&s).contains(&"categories"));
        let r = run(&mut s, "view.addColumns", json!({}));
        assert_eq!(r["dialog"], "showColumns");
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["module"], "people");
        assert!(p["available"].as_array().unwrap().len() >= 20);
        run(
            &mut s,
            "view.columns",
            json!({"columns": ["fullName", "Mobile Phone", "flagStatus", "fullName"]}),
        );
        assert_eq!(ids_of(&s), ["fullName", "mobilePhone", "flagStatus"]);
        run(&mut s, "view.columns", json!({"add": "nickname"}));
        run(&mut s, "view.columns", json!({"remove": "mobilePhone"}));
        assert_eq!(ids_of(&s), ["fullName", "flagStatus", "nickname"]);
        assert!(s.run("view.columns", &json!({"columns": []})).is_err());
        assert!(s.run("view.columns", &json!({"add": "subject"})).is_err(), "a task column");
        // Each view keeps its own columns.
        run(&mut s, "view.changeView", json!({"name": "List"}));
        assert_eq!(ids_of(&s), LIST);
        run(&mut s, "view.changeView", json!({"name": "Phone"}));
        assert_eq!(ids_of(&s), ["fullName", "flagStatus", "nickname"]);
        run(&mut s, "view.columns", json!({"reset": true}));
        assert_eq!(ids_of(&s), PHONE);
        assert!(
            !modules::customized(&s, folder)
                || modules::state(&s, folder, ModuleKind::People).custom.is_empty()
        );
    }

    #[test]
    fn contact_cells() {
        let mut s = session();
        select_folder(&mut s, FolderRole::Contacts);
        let id = run(
            &mut s,
            "people.new",
            json!({"fullName": "Ann Lee", "company": "Example Co", "email": "ann@example.com",
                   "phones": {"Mobile": "555 0100"}, "categories": ["Red category"],
                   "businessAddress": {"street": "1 Main St", "city": "Springfield", "country": "Example"}}),
        )["id"]
            .as_u64()
            .unwrap();
        let c = s.mailbox.contacts[&ItemId(id)].clone();
        assert_eq!(contact_cell(&c, "fullName"), "Ann Lee");
        assert_eq!(contact_cell(&c, "mobilePhone"), "555 0100");
        assert_eq!(contact_cell(&c, "email"), "ann@example.com");
        assert_eq!(contact_cell(&c, "email2"), "");
        assert_eq!(contact_cell(&c, "categories"), "Red category");
        assert_eq!(contact_cell(&c, "flagStatus"), "Unflagged");
        assert_eq!(contact_cell(&c, "businessAddress"), "1 Main St, Springfield, Example");
        assert_eq!(contact_cell(&c, "country"), "Example");
        assert_eq!(contact_cell(&c, "sensitivity"), "Normal");
        assert_eq!(contact_cell(&c, "bogus"), "");
    }

    #[test]
    fn task_columns_sort_and_filter() {
        let mut s = session(); // Friday 2026-10-09
        let folder = select_folder(&mut s, FolderRole::Tasks);
        let task = |s: &mut Session, p: Value| run(s, "tasks.new", p)["id"].as_u64().unwrap();
        let a = task(&mut s, json!({"subject": "Alpha", "due": "2026-10-20", "priority": "low"}));
        let b =
            task(&mut s, json!({"subject": "Bravo", "due": "2026-10-12", "categories": ["Red category"]}));
        let c = task(&mut s, json!({"subject": "Charlie report", "priority": "high"}));
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        assert_eq!(ids(&s, folder), [b, a, c], "due date, none last");
        assert_eq!(columns(&s, folder).iter().map(|c| c.id).collect::<Vec<_>>(), SIMPLE);
        // Sort by a column.
        run(&mut s, "view.sortBy", json!({"field": "subject", "dir": "desc"}));
        assert_eq!(ids(&s, folder), [c, b, a]);
        run(&mut s, "view.sortBy", json!({"field": "Priority"}));
        assert_eq!(ids(&s, folder), [c, b, a]);
        assert!(s.run("view.sortBy", &json!({"field": "nope"})).is_err());
        assert!(s.run("view.sortBy", &json!({"field": "subject", "dir": "up"})).is_err());
        run(&mut s, "view.sortBy", json!({"field": null}));
        assert_eq!(ids(&s, folder), [b, a, c]);
        // Filter.
        let r = run(&mut s, "view.customFilter", json!({"filter": {"words": "report"}}));
        assert_eq!(r["on"], true);
        assert_eq!(ids(&s, folder), [c]);
        run(&mut s, "view.customFilter", json!({"filter": {"category": "red category"}}));
        assert_eq!(ids(&s, folder), [b]);
        run(&mut s, "view.customFilter", json!({"filter": {"importance": "low"}}));
        assert_eq!(ids(&s, folder), [a]);
        run(&mut s, "view.customFilter", json!({"filter": {"flagged": true}}));
        assert_eq!(ids(&s, folder), [b, a]);
        assert_eq!(run(&mut s, "view.customFilter", json!({}))["dialog"], "viewFilter");
        run(&mut s, "view.customFilter", json!({"filter": null}));
        assert_eq!(ids(&s, folder).len(), 3);
        // Cells.
        let t = s.mailbox.tasks[&ItemId(b)].clone();
        assert_eq!(task_cell(&s, Item::Task(&t), "dueDate"), "Mon 10/12/2026");
        assert_eq!(task_cell(&s, Item::Task(&t), "status"), "Not Started");
        assert_eq!(task_cell(&s, Item::Task(&t), "percentComplete"), "0%");
        assert_eq!(task_cell(&s, Item::Task(&t), "dateCompleted"), "None");
        assert!(!task_cell(&s, Item::Task(&t), "inFolder").is_empty());
        assert!(!task_cell(&s, Item::Task(&t), "modified").is_empty(), "new tasks record when they changed");
        // Columns of the Detailed view.
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        run(&mut s, "view.columns", json!({"columns": ["subject", "modified", "dateCompleted", "priority"]}));
        assert_eq!(columns(&s, folder).len(), 4);
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        assert_eq!(columns(&s, folder).len(), SIMPLE.len());
    }

    #[test]
    fn module_conditional_formatting() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Tasks);
        let late =
            run(&mut s, "tasks.new", json!({"subject": "Late", "due": "2026-10-01"}))["id"].as_u64().unwrap();
        let done =
            run(&mut s, "tasks.new", json!({"subject": "Done", "complete": true}))["id"].as_u64().unwrap();
        run(&mut s, "view.changeView", json!({"name": "Simple List"}));
        let style = |s: &Session, id: u64| {
            let t = s.mailbox.tasks[&ItemId(id)].clone();
            row_style(s, &rules(s, folder), Item::Task(&t))
        };
        assert!(style(&s, late).overdue);
        assert!(style(&s, done).completed && style(&s, done).font.strike);
        assert_eq!(run(&mut s, "view.conditionalFormatting", json!({}))["dialog"], "viewFormat");
        let mut list = serde_json::to_value(default_rules(ModuleKind::Tasks)).unwrap();
        list[0]["on"] = json!(false);
        list.as_array_mut().unwrap().push(json!({
            "name": "Reports", "on": true, "kind": "custom",
            "condition": {"words": "late"}, "font": {"bold": true, "color": "#107C10"}
        }));
        run(&mut s, "view.conditionalFormatting", json!({ "rules": list }));
        let st = style(&s, late);
        assert!(!st.overdue && st.font.bold);
        assert_eq!(st.font.color.as_deref(), Some("#107c10"));
        assert!(
            s.run("view.conditionalFormatting", &json!({"rules": [{"name": "x", "kind": "unread"}]}))
                .is_err()
        );
        run(&mut s, "view.conditionalFormatting", json!({"reset": true}));
        assert!(style(&s, late).overdue);
    }

    #[test]
    fn module_format_columns_renames_and_aligns() {
        let mut s = session();
        let folder = select_folder(&mut s, FolderRole::Tasks);
        run(&mut s, "view.changeView", json!({"name": "Detailed"}));
        assert_eq!(run(&mut s, "view.formatColumns", json!({}))["dialog"], "viewFormat");
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["module"], "tasks");
        assert!(
            p["columns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "percentComplete" && c["align"] == "right")
        );
        let r = run(
            &mut s,
            "view.formatColumns",
            json!({"column": "subject", "label": "Task", "align": "center"}),
        );
        assert_eq!((r["label"].as_str(), r["align"].as_str()), (Some("Task"), Some("center")));
        let subject = shown(&s, folder).into_iter().find(|c| c.col.id == "subject").unwrap();
        assert_eq!((subject.label.as_str(), subject.align), ("Task", "center"));
        assert!(s.run("view.formatColumns", &json!({"column": "subject", "align": "middle"})).is_err());
        assert!(s.run("view.formatColumns", &json!({"column": "size"})).is_err(), "not a task column");
        // Back to the defaults.
        run(&mut s, "view.formatColumns", json!({"column": "subject", "label": "", "align": ""}));
        let subject = shown(&s, folder).into_iter().find(|c| c.col.id == "subject").unwrap();
        assert_eq!((subject.label.as_str(), subject.align), ("Subject", "left"));
        assert!(modules::state(&s, folder, ModuleKind::Tasks).custom.is_empty());
    }

    #[test]
    fn hostile_custom_state_is_cleaned() {
        let mut s = session();
        let tasks = select_folder(&mut s, FolderRole::Tasks);
        s.view.collapsed_groups.insert(
            tasks,
            vec![
                "\u{1}mod.state={\"view\":\"Detailed\",\"custom\":{\"Detailed\":{\"columns\":[\"bogus\",\"subject\",\"subject\"],\"sort\":{\"column\":\"x\"},\"rules\":[{\"kind\":\"weird\"}]},\"Nope\":{\"columns\":[\"subject\"]}}}".into(),
            ],
        );
        let st = modules::state(&s, tasks, ModuleKind::Tasks);
        assert_eq!(st.custom.len(), 1);
        let c = custom(&st);
        assert_eq!(c.columns, ["subject"]);
        assert!(c.sort.is_none());
        assert_eq!(c.rules.as_deref(), Some(&[][..]));
        assert_eq!(columns(&s, tasks).len(), 1);
    }
}
