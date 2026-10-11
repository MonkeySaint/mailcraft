//! Mail views (`plan/reference/message-list.md` › Views, `dialogs.md` › view-settings and manage-views): what
//! the list of a mail or search folder shows beyond its [`ViewSettings`].
//!
//! **Per folder** ([`MailView`], stored like the module views as one reserved entry of
//! `ViewState::collapsed_groups`, persisted with the profile and dropped by Reset View):
//! - the columns of the column layout (Single, Preview, and Compact with the reading pane at the bottom or
//!   off) in order, with the widths set by hand (Add Columns / Show Columns, dragging a header edge);
//! - Format Columns: a column's label, display format and alignment;
//! - Conditional Formatting: rules (unread, overdue, expired, or a filter condition) that set a row's font
//!   (bold, italic, underline, strikethrough, colour), applied top to bottom;
//! - the View Settings filter ([`CustomFilter`]: words in the subject or body, from, sent to, read state,
//!   importance, attachments, flag, category, received within N days);
//! - the saved view the folder shows (Change View).
//!
//! **Window wide** (the profile settings' `state.uiPrefs` object, keys starting with [`PREF_PREFIX`]; the UI
//! keeps its own dialog settings in the same object and leaves these keys alone): saved views (Save Current
//! View As a New View, Manage Views), the Quick Actions shown on a hovered row, and the status bar items
//! hidden from the Customize Status Bar menu.
//!
//! **Pinned messages** (Quick Action Pin / Unpin, `view.pin`) are kept per folder (a reserved entry that Reset
//! View keeps) and listed at the top of the folder's list.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{dialog, parse_layout, settings};
use crate::cmd::util::{mail_targets, opt_bool, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_model::{
    ArrangeBy, DateTime, FlagState, FolderId, FolderKind, FolderRole, Importance, ItemId, ListTime, MailItem,
    Sensitivity, ViewLayout, ViewSettings,
};

/// Keys of `state.uiPrefs` that belong to the engine (the UI's own dialog settings use other names).
pub const PREF_PREFIX: &str = "view.";
const KEY_STATUS: &str = "view.statusBar.hidden";
const KEY_QUICK: &str = "view.quickActions";
const KEY_SAVED: &str = "view.savedViews";

const MAIL_STATE: &str = "\u{1}mail.view=";
const PINNED: &str = "\u{1}mail.pinned=";

/// Most columns a view shows.
pub const MAX_COLUMNS: usize = 24;
/// Most Conditional Formatting rules per view.
pub const MAX_RULES: usize = 32;
/// Most saved mail views.
pub const MAX_SAVED: usize = 50;
/// Longest view, rule or column label (characters).
pub const MAX_NAME: usize = 64;
/// Longest filter text field (characters).
pub const MAX_TEXT: usize = 256;
/// Most pinned messages per folder.
pub const MAX_PINNED: usize = 200;
/// Column widths set by hand are kept within these bounds (px).
pub const MIN_WIDTH: u16 = 16;
pub const MAX_WIDTH: u16 = 1200;
/// Group header of pinned messages.
pub const PINNED_LABEL: &str = "Pinned";
/// Built-in views of a mail folder.
pub const BUILT_IN: [&str; 3] = ["Compact", "Single", "Preview"];
/// "Can be used on" text of views kept for every mail folder.
pub const ALL_FOLDERS: &str = "All Mail and Post folders";

// ---------------------------------------------------------------------------------------------------------
// Columns

/// How a column's cells are drawn and which display formats it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    /// A glyph (or its name with the "text" format).
    Icon,
    Text,
    Date,
    Size,
    /// Names or addresses.
    Person,
}

impl ColumnKind {
    /// Display formats (id, label); the first is the default.
    pub fn formats(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ColumnKind::Icon => &[("icon", "Icon"), ("text", "Text")],
            ColumnKind::Text => &[("text", "Text")],
            ColumnKind::Date => &[
                ("bestFit", "Best Fit"),
                ("shortDate", "Short date"),
                ("shortDateTime", "Short date and time"),
                ("longDateTime", "Long date and time"),
            ],
            ColumnKind::Size => &[("kb", "Kilobytes"), ("bytes", "Bytes")],
            ColumnKind::Person => {
                &[("name", "Name"), ("address", "Address"), ("nameAndAddress", "Name and address")]
            }
        }
    }
}

/// A column the mail list can show.
#[derive(Clone, Copy, Debug)]
pub struct ColumnDef {
    pub id: &'static str,
    pub label: &'static str,
    /// Default width in px; 0 = shares the free space (From, To, Subject).
    pub width: u16,
    /// Clicking the header arranges by this (`None` = not sortable).
    pub arrange: Option<ArrangeBy>,
    pub kind: ColumnKind,
    /// Kept on a narrow list.
    pub essential: bool,
}

const fn col(
    id: &'static str,
    label: &'static str,
    width: u16,
    arrange: Option<ArrangeBy>,
    kind: ColumnKind,
    essential: bool,
) -> ColumnDef {
    ColumnDef { id, label, width, arrange, kind, essential }
}

/// Every column (Show Columns › Available columns), in the dialog's order.
pub const COLUMNS: [ColumnDef; 18] = [
    col("importance", "Importance", 22, Some(ArrangeBy::Importance), ColumnKind::Icon, true),
    col("reminder", "Reminder", 22, None, ColumnKind::Icon, false),
    col("icon", "Icon", 24, Some(ArrangeBy::Type), ColumnKind::Icon, true),
    col("attachment", "Attachment", 22, Some(ArrangeBy::Attachments), ColumnKind::Icon, true),
    col("from", "From", 0, Some(ArrangeBy::From), ColumnKind::Person, true),
    col("to", "To", 0, Some(ArrangeBy::To), ColumnKind::Person, true),
    col("cc", "Cc", 120, None, ColumnKind::Person, false),
    col("subject", "Subject", 0, Some(ArrangeBy::Subject), ColumnKind::Text, true),
    col("received", "Received", 150, Some(ArrangeBy::Date), ColumnKind::Date, true),
    col("sent", "Sent", 150, None, ColumnKind::Date, false),
    col("size", "Size", 72, Some(ArrangeBy::Size), ColumnKind::Size, false),
    col("categories", "Categories", 110, Some(ArrangeBy::Categories), ColumnKind::Text, false),
    col("mention", "Mention", 22, None, ColumnKind::Icon, false),
    col("flagStatus", "Flag Status", 26, Some(ArrangeBy::FlagStatus), ColumnKind::Icon, true),
    col("flagStart", "Start Date", 110, Some(ArrangeBy::FlagStartDate), ColumnKind::Date, false),
    col("flagDue", "Due Date", 110, Some(ArrangeBy::FlagDueDate), ColumnKind::Date, false),
    col("sensitivity", "Sensitivity", 90, None, ColumnKind::Text, false),
    col("inFolder", "In Folder", 110, None, ColumnKind::Text, false),
];

/// Columns of a view nobody changed (the bottom reading pane layout of `plan/reference/responsive.md`).
pub const DEFAULT_COLUMNS: [&str; 11] = [
    "importance",
    "reminder",
    "icon",
    "attachment",
    "from",
    "subject",
    "received",
    "size",
    "categories",
    "mention",
    "flagStatus",
];

/// The column with this id (ids are case-insensitive; labels are accepted too).
pub fn column(id: &str) -> Option<&'static ColumnDef> {
    let want = id.trim();
    COLUMNS.iter().find(|c| c.id.eq_ignore_ascii_case(want) || c.label.eq_ignore_ascii_case(want))
}

/// Format Columns settings of one column (empty strings = the defaults).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColumnFormat {
    pub label: String,
    pub format: String,
    /// "left", "center" or "right".
    pub align: String,
}

/// A column as the list draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSpec {
    pub id: &'static str,
    pub label: String,
    /// Width set by hand (`None` = the column's default, or a share of the free space).
    pub width: Option<u16>,
    pub default_width: u16,
    pub format: &'static str,
    pub align: &'static str,
    pub arrange: Option<ArrangeBy>,
    pub kind: ColumnKind,
    pub essential: bool,
}

impl Serialize for ColumnKind {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(match self {
            ColumnKind::Icon => "icon",
            ColumnKind::Text => "text",
            ColumnKind::Date => "date",
            ColumnKind::Size => "size",
            ColumnKind::Person => "person",
        })
    }
}

// ---------------------------------------------------------------------------------------------------------
// Filter and Conditional Formatting

/// View Settings › Filter: an item shows when it meets every condition that is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomFilter {
    /// Words that must all appear (in the subject, or also in the body with `in_body`).
    pub words: String,
    pub in_body: bool,
    /// Sender names or addresses, `;`-separated (any may match).
    pub from: String,
    /// Recipient (To or Cc) names or addresses, `;`-separated (any may match).
    pub sent_to: String,
    /// "", "unread" or "read".
    pub read: String,
    /// "", "high", "normal" or "low".
    pub importance: String,
    pub has_attachments: bool,
    pub flagged: bool,
    /// A category the item carries.
    pub category: String,
    /// Received in the last this many days (0 = any time).
    pub days: u32,
}

impl CustomFilter {
    /// True when no condition is set.
    pub fn is_empty(&self) -> bool {
        *self == CustomFilter::default()
    }

    /// Lengths capped, choices checked (stored or given text is not trusted).
    fn sanitized(mut self) -> CustomFilter {
        for t in [&mut self.words, &mut self.from, &mut self.sent_to, &mut self.category] {
            *t = clip(t.trim(), MAX_TEXT);
        }
        if !matches!(self.read.as_str(), "unread" | "read") {
            self.read.clear();
        }
        if !matches!(self.importance.as_str(), "high" | "normal" | "low") {
            self.importance.clear();
        }
        self.days = self.days.min(36_500);
        self
    }

    /// Whether `m` meets every condition.
    pub fn matches(&self, s: &Session, m: &MailItem) -> bool {
        if !self.words.trim().is_empty() {
            let mut hay = m.subject.to_lowercase();
            if self.in_body {
                hay.push('\n');
                hay.push_str(&m.body_text().to_lowercase());
            }
            if !self.words.split_whitespace().all(|w| hay.contains(&w.to_lowercase())) {
                return false;
            }
        }
        if !self.from.trim().is_empty() {
            let who: Vec<&mailcraft_model::Address> = m.from.iter().chain(m.sender.iter()).collect();
            if !any_person(&self.from, &who) {
                return false;
            }
        }
        if !self.sent_to.trim().is_empty() {
            let who: Vec<&mailcraft_model::Address> = m.to.iter().chain(m.cc.iter()).collect();
            if !any_person(&self.sent_to, &who) {
                return false;
            }
        }
        match self.read.as_str() {
            "unread" if m.read => return false,
            "read" if !m.read => return false,
            _ => {}
        }
        let importance = match m.importance {
            Importance::High => "high",
            Importance::Normal => "normal",
            Importance::Low => "low",
        };
        if !self.importance.is_empty() && self.importance != importance {
            return false;
        }
        if self.has_attachments && !m.has_attachments() {
            return false;
        }
        if self.flagged && !m.is_flagged() {
            return false;
        }
        let cat = self.category.trim();
        if !cat.is_empty() && !m.categories.iter().any(|c| c.eq_ignore_ascii_case(cat)) {
            return false;
        }
        if self.days > 0 {
            let since = s.now().unix.saturating_sub(i64::from(self.days).saturating_mul(86_400));
            if m.received.unix < since {
                return false;
            }
        }
        true
    }

    /// One-line description for View Settings ("Off" when empty).
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.words.is_empty() {
            let place = if self.in_body { "subject or body" } else { "subject" };
            parts.push(format!("\"{}\" in the {place}", self.words));
        }
        if !self.from.is_empty() {
            parts.push(format!("from {}", self.from));
        }
        if !self.sent_to.is_empty() {
            parts.push(format!("sent to {}", self.sent_to));
        }
        if !self.read.is_empty() {
            parts.push(self.read.clone());
        }
        if !self.importance.is_empty() {
            parts.push(format!("{} importance", self.importance));
        }
        if self.has_attachments {
            parts.push("with attachments".into());
        }
        if self.flagged {
            parts.push("flagged".into());
        }
        if !self.category.is_empty() {
            parts.push(format!("category {}", self.category));
        }
        if self.days > 0 {
            parts.push(format!("received in the last {} days", self.days));
        }
        if parts.is_empty() { "Off".to_owned() } else { parts.join(", ") }
    }
}

/// `list` (`;` or `,` separated) names someone in `who` (name or address contains the text).
fn any_person(list: &str, who: &[&mailcraft_model::Address]) -> bool {
    list.split([';', ',']).map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).any(|p| {
        who.iter().any(|a| {
            a.addr.to_lowercase().contains(&p)
                || a.name.as_deref().is_some_and(|n| n.to_lowercase().contains(&p))
        })
    })
}

/// Font a Conditional Formatting rule gives matching rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleFont {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// "#rrggbb", empty = the list's own colour.
    pub color: String,
}

/// A Conditional Formatting rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FormatRule {
    pub name: String,
    pub on: bool,
    /// "unread", "overdue", "expired" (the built-in rules) or "custom" (`condition`).
    pub kind: String,
    pub condition: CustomFilter,
    pub font: RuleFont,
}

/// The rules of a view nobody changed: unread rows bold, overdue rows red, expired rows struck through.
pub fn default_rules() -> Vec<FormatRule> {
    let rule = |name: &str, kind: &str, font: RuleFont| FormatRule {
        name: name.to_owned(),
        on: true,
        kind: kind.to_owned(),
        condition: CustomFilter::default(),
        font,
    };
    vec![
        rule("Unread messages", "unread", RuleFont { bold: true, ..Default::default() }),
        rule("Overdue messages", "overdue", RuleFont { color: "#c42b1c".into(), ..Default::default() }),
        rule(
            "Expired messages",
            "expired",
            RuleFont { strike: true, color: "#8a8886".into(), ..Default::default() },
        ),
    ]
}

fn sanitize_rule(mut r: FormatRule) -> Option<FormatRule> {
    r.name = clip(r.name.trim(), MAX_NAME);
    if !matches!(r.kind.as_str(), "unread" | "overdue" | "expired" | "custom") {
        return None;
    }
    r.condition = r.condition.sanitized();
    r.font.color = valid_color(&r.font.color).unwrap_or_default();
    if r.name.is_empty() {
        r.name = "Untitled".to_owned();
    }
    Some(r)
}

/// "#rrggbb" (lower case) or `None`.
fn valid_color(c: &str) -> Option<String> {
    let c = c.trim();
    let hex = c.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|ch| ch.is_ascii_hexdigit())).then(|| c.to_ascii_lowercase())
}

/// The follow-up due date has passed (due before today) and the flag is not complete.
pub fn is_overdue(s: &Session, m: &MailItem) -> bool {
    let tz = s.tz_offset_min;
    match &m.flag {
        FlagState::Flagged { due: Some(d), .. } => d.day_number(tz) < s.now().day_number(tz),
        _ => false,
    }
}

/// Longest header block scanned for an expiry date.
const HEADER_SCAN: usize = 64 * 1024;

/// The message's expiry date (`Expiry-Date` or `Expires`, RFC 4021) from its kept source, if any.
pub fn expiry(m: &MailItem) -> Option<DateTime> {
    let raw = m.raw.as_deref()?;
    let head = raw.get(..raw.len().min(HEADER_SCAN))?;
    let text = String::from_utf8_lossy(head);
    let mut value: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = value.as_mut() {
            if line.starts_with([' ', '\t']) {
                v.push(' ');
                v.push_str(line.trim());
                continue;
            }
            break;
        }
        if let Some((name, rest)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("expiry-date") || name.eq_ignore_ascii_case("expires") {
                value = Some(rest.trim().to_owned());
            }
        }
    }
    DateTime::parse_rfc5322(value?.trim()).ok()
}

/// The message carries an expiry date that has passed.
pub fn is_expired(s: &Session, m: &MailItem) -> bool {
    expiry(m).is_some_and(|d| d.unix <= s.now().unix)
}

/// Font effects of the rules that match `m` (rules apply top to bottom; a later colour wins). `unread` is the
/// row's unread state (a conversation header is unread while any of its messages is).
pub fn row_style(s: &Session, rules: &[FormatRule], m: &MailItem, unread: bool) -> crate::queries::RowStyle {
    let mut st = crate::queries::RowStyle::default();
    for r in rules.iter().filter(|r| r.on) {
        let hit = match r.kind.as_str() {
            "unread" => unread,
            "overdue" => is_overdue(s, m),
            "expired" => is_expired(s, m),
            "custom" => !r.condition.is_empty() && r.condition.matches(s, m),
            _ => false,
        };
        if !hit {
            continue;
        }
        st.bold |= r.font.bold;
        st.italic |= r.font.italic;
        st.underline |= r.font.underline;
        st.strike |= r.font.strike;
        if !r.font.color.is_empty() {
            st.color = Some(r.font.color.clone());
        }
    }
    st
}

// ---------------------------------------------------------------------------------------------------------
// Per-folder state

/// What a mail folder's list shows beyond its [`ViewSettings`] (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MailView {
    /// Column ids in order (empty = [`DEFAULT_COLUMNS`]).
    pub columns: Vec<String>,
    /// Widths set by hand (column id → px).
    pub widths: BTreeMap<String, u16>,
    /// Format Columns (column id → settings).
    pub formats: BTreeMap<String, ColumnFormat>,
    /// Conditional Formatting (`None` = [`default_rules`]).
    pub rules: Option<Vec<FormatRule>>,
    /// View Settings › Filter.
    pub filter: Option<CustomFilter>,
    /// Saved view the folder shows (empty = a built-in view).
    pub name: String,
}

impl MailView {
    /// Stored or given state cleaned up: unknown columns and formats dropped, numbers clamped, text capped.
    fn sanitized(mut self) -> MailView {
        let mut seen = BTreeSet::new();
        self.columns = self
            .columns
            .iter()
            .filter_map(|c| column(c))
            .filter(|c| seen.insert(c.id))
            .take(MAX_COLUMNS)
            .map(|c| c.id.to_owned())
            .collect();
        self.widths = std::mem::take(&mut self.widths)
            .into_iter()
            .filter_map(|(k, w)| column(&k).map(|c| (c.id.to_owned(), w.clamp(MIN_WIDTH, MAX_WIDTH))))
            .collect();
        self.formats = std::mem::take(&mut self.formats)
            .into_iter()
            .filter_map(|(k, f)| {
                let c = column(&k)?;
                let f = ColumnFormat {
                    label: clip(f.label.trim(), MAX_NAME),
                    format: if c.kind.formats().iter().any(|(id, _)| *id == f.format) {
                        f.format
                    } else {
                        String::new()
                    },
                    align: if matches!(f.align.as_str(), "left" | "center" | "right") {
                        f.align
                    } else {
                        String::new()
                    },
                };
                (f != ColumnFormat::default()).then(|| (c.id.to_owned(), f))
            })
            .collect();
        self.rules = self.rules.map(|r| r.into_iter().filter_map(sanitize_rule).take(MAX_RULES).collect());
        self.filter = self.filter.map(CustomFilter::sanitized).filter(|f| !f.is_empty());
        self.name = clip(self.name.trim(), MAX_NAME);
        if self.columns.iter().map(String::as_str).eq(DEFAULT_COLUMNS) {
            self.columns.clear();
        }
        self
    }
}

fn reserved_json<'a>(s: &'a Session, folder: FolderId, mark: &str) -> Option<&'a str> {
    s.view.collapsed_groups.get(&folder)?.iter().find_map(|e| e.strip_prefix(mark))
}

fn store_reserved(s: &mut Session, folder: FolderId, mark: &str, json: Option<String>) {
    let list = s.view.collapsed_groups.entry(folder).or_default();
    list.retain(|e| !e.starts_with(mark));
    if let Some(j) = json {
        list.push(format!("{mark}{j}"));
    }
    if list.is_empty() {
        s.view.collapsed_groups.remove(&folder);
    }
}

/// The mail view of `folder` (defaults when nothing valid is stored).
pub fn mail_view(s: &Session, folder: FolderId) -> MailView {
    reserved_json(s, folder, MAIL_STATE)
        .and_then(|j| serde_json::from_str::<MailView>(j).ok())
        .map(MailView::sanitized)
        .unwrap_or_default()
}

/// Stores the mail view of `folder` (dropped when it equals the defaults, so Reset View's state shows).
pub(crate) fn store_view(s: &mut Session, folder: FolderId, v: &MailView) {
    let v = v.clone().sanitized();
    let json = if v == MailView::default() { None } else { serde_json::to_string(&v).ok() };
    store_reserved(s, folder, MAIL_STATE, json);
}

/// True when the folder's mail view differs from the defaults.
pub fn customized(s: &Session, folder: FolderId) -> bool {
    reserved_json(s, folder, MAIL_STATE).is_some()
}

/// True for the reserved entries Reset View keeps (pinned messages).
pub(crate) fn kept_on_reset(entry: &str) -> bool {
    entry.starts_with(PINNED)
}

/// Columns of the folder's list, resolved (labels, formats, widths).
pub fn columns(s: &Session, folder: FolderId) -> Vec<ColumnSpec> {
    let v = mail_view(s, folder);
    let ids: Vec<&str> = if v.columns.is_empty() {
        DEFAULT_COLUMNS.to_vec()
    } else {
        v.columns.iter().map(String::as_str).collect()
    };
    ids.into_iter()
        .filter_map(column)
        .map(|c| {
            let f = v.formats.get(c.id).cloned().unwrap_or_default();
            let formats = c.kind.formats();
            let format =
                formats.iter().find(|(id, _)| *id == f.format).or(formats.first()).map_or("", |x| x.0);
            let align = match f.align.as_str() {
                "center" => "center",
                "right" => "right",
                "left" => "left",
                _ if c.kind == ColumnKind::Size => "right",
                _ if c.kind == ColumnKind::Icon && format == "icon" => "center",
                _ => "left",
            };
            ColumnSpec {
                id: c.id,
                label: if f.label.is_empty() { c.label.to_owned() } else { f.label },
                width: v.widths.get(c.id).copied(),
                default_width: c.width,
                format,
                align,
                arrange: c.arrange,
                kind: c.kind,
                essential: c.essential,
            }
        })
        .collect()
}

/// Conditional Formatting rules of the folder's view.
pub fn rules(s: &Session, folder: FolderId) -> Vec<FormatRule> {
    mail_view(s, folder).rules.unwrap_or_else(default_rules)
}

/// The View Settings filter of the folder (`None` = off).
pub fn custom_filter(s: &Session, folder: FolderId) -> Option<CustomFilter> {
    mail_view(s, folder).filter
}

// ---------------------------------------------------------------------------------------------------------
// Cell text

fn person_text(list: &[&mailcraft_model::Address], format: &str) -> String {
    list.iter()
        .map(|a| match format {
            "address" => a.addr.clone(),
            "nameAndAddress" => a.to_display_string(),
            _ => a.display().to_owned(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn date_text(s: &Session, t: Option<DateTime>, format: &str) -> String {
    let Some(t) = t else { return "None".to_owned() };
    let tz = s.tz_offset_min;
    match format {
        "shortDate" => ListTime::date(t, tz),
        "shortDateTime" => ListTime::short_date_time(t, tz),
        "longDateTime" => ListTime::long(t, tz),
        _ => ListTime::format(t, s.now(), tz),
    }
}

/// `12345` → `"12,345"`.
fn grouped_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Text of a cell of column `id` for `m` in the given display `format` (see [`ColumnKind::formats`]). Icon
/// columns give their state as a word ("High", "Flagged") for the "text" format, empty otherwise when
/// nothing is set.
pub fn cell_text(s: &Session, m: &MailItem, id: &str, format: &str) -> String {
    let outgoing = s
        .mailbox
        .folder(m.folder)
        .and_then(|f| f.role)
        .is_some_and(|r| matches!(r, FolderRole::Drafts | FolderRole::SentItems | FolderRole::Outbox));
    match id {
        "from" if outgoing => person_text(&m.to.iter().collect::<Vec<_>>(), format),
        "from" => person_text(&m.from.iter().chain(m.sender.iter()).take(1).collect::<Vec<_>>(), format),
        "to" => person_text(&m.to.iter().collect::<Vec<_>>(), format),
        "cc" => person_text(&m.cc.iter().collect::<Vec<_>>(), format),
        "subject" => {
            if m.subject.trim().is_empty() {
                "(no subject)".to_owned()
            } else {
                m.subject.clone()
            }
        }
        "received" if m.is_draft => "None".to_owned(),
        "received" => date_text(s, Some(m.received), format),
        "sent" => date_text(s, m.sent, format),
        "flagStart" => date_text(
            s,
            match &m.flag {
                FlagState::Flagged { start, .. } => *start,
                _ => None,
            },
            format,
        ),
        "flagDue" => date_text(
            s,
            match &m.flag {
                FlagState::Flagged { due, .. } => *due,
                _ => None,
            },
            format,
        ),
        "size" if format == "bytes" => format!("{} bytes", grouped_digits(m.size)),
        "size" => m.size_label(),
        "categories" => m.categories.join(", "),
        "sensitivity" => match m.sensitivity {
            Sensitivity::Normal => "Normal",
            Sensitivity::Personal => "Personal",
            Sensitivity::Private => "Private",
            Sensitivity::Confidential => "Confidential",
        }
        .to_owned(),
        "inFolder" => s.mailbox.folder(m.folder).map(|f| f.name.clone()).unwrap_or_default(),
        "importance" if format == "text" => match m.importance {
            Importance::High => "High",
            Importance::Normal => "",
            Importance::Low => "Low",
        }
        .to_owned(),
        "attachment" if format == "text" && m.has_attachments() => "Attachment".to_owned(),
        "flagStatus" if format == "text" => match m.flag {
            FlagState::None => "",
            FlagState::Flagged { .. } => "Flagged",
            FlagState::Complete { .. } => "Completed",
        }
        .to_owned(),
        "reminder" if format == "text" && matches!(m.flag, FlagState::Flagged { reminder: Some(_), .. }) => {
            "Reminder".to_owned()
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pinned messages

fn pins_of(s: &Session, folder: FolderId) -> BTreeSet<u64> {
    reserved_json(s, folder, PINNED)
        .and_then(|j| serde_json::from_str::<BTreeSet<u64>>(j).ok())
        .unwrap_or_default()
        .into_iter()
        .take(MAX_PINNED)
        .collect()
}

/// Every pinned message (of every folder).
pub fn pinned(s: &Session) -> BTreeSet<ItemId> {
    let mut out = BTreeSet::new();
    for (f, list) in &s.view.collapsed_groups {
        if list.iter().any(|e| e.starts_with(PINNED)) {
            out.extend(pins_of(s, *f).into_iter().map(ItemId).filter(|i| s.mailbox.mail.contains_key(i)));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Window-wide settings (`state.uiPrefs`)

fn ui_prefs(s: &Session) -> Map<String, Value> {
    match s.settings.get("state.uiPrefs") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn ui_pref(s: &Session, key: &str) -> Option<Value> {
    ui_prefs(s).remove(key)
}

fn set_ui_pref(s: &mut Session, key: &str, v: Option<Value>) -> Result<(), CmdError> {
    let mut m = ui_prefs(s);
    match v {
        Some(v) => m.insert(key.to_owned(), v),
        None => m.remove(key),
    };
    s.set_option("state.uiPrefs", &Value::Object(m)).map(|_| ())
}

/// Status bar elements (Customize Status Bar, `plan/reference/message-list.md`): (key, label).
pub const STATUS_ITEMS: [(&str, &str); 11] = [
    ("quota", "Quota Information"),
    ("filter", "Filter"),
    ("itemsInView", "Items in View"),
    ("headerItems", "Header Items in View"),
    ("unreadItems", "Unread Items in View"),
    ("unseenItems", "Unseen Items in View"),
    ("reminders", "Reminders"),
    ("groupNotifications", "Group Notifications"),
    ("viewShortcuts", "View Shortcuts"),
    ("zoomSlider", "Zoom Slider"),
    ("zoom", "Zoom"),
];

/// Hidden by default: Quota Information (observed Off); everything else shows.
const STATUS_HIDDEN_DEFAULT: [&str; 1] = ["quota"];

/// Status bar elements hidden from the Customize Status Bar menu.
pub fn status_hidden(s: &Session) -> BTreeSet<String> {
    match ui_pref(s, KEY_STATUS) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|k| STATUS_ITEMS.iter().any(|(id, _)| id == k))
            .map(str::to_owned)
            .collect(),
        _ => STATUS_HIDDEN_DEFAULT.iter().map(|k| (*k).to_owned()).collect(),
    }
}

/// Whether one status bar element shows.
pub fn status_item_shown(s: &Session, key: &str) -> bool {
    !status_hidden(s).contains(key)
}

/// Quick Actions (id, label) offered for the buttons on a hovered row.
pub const QUICK_ACTIONS: [(&str, &str); 8] = [
    ("none", "None"),
    ("flag", "Flag / Unflag"),
    ("delete", "Delete"),
    ("archive", "Archive"),
    ("markRead", "Mark as Read / Unread"),
    ("move", "Move"),
    ("categorize", "Categorize"),
    ("pin", "Pin / Unpin"),
];

/// The default Quick Actions, nearest the row's right edge first.
pub const DEFAULT_QUICK_ACTIONS: [&str; 2] = ["flag", "delete"];

fn quick_action_id(v: &str) -> Option<&'static str> {
    let v = v.trim();
    QUICK_ACTIONS
        .iter()
        .find(|(id, label)| id.eq_ignore_ascii_case(v) || label.eq_ignore_ascii_case(v))
        .map(|x| x.0)
}

/// The two Quick Actions, nearest the row's right edge first ("none" for an empty slot).
pub fn quick_actions(s: &Session) -> [&'static str; 2] {
    let mut out = DEFAULT_QUICK_ACTIONS;
    if let Some(Value::Array(a)) = ui_pref(s, KEY_QUICK) {
        for (slot, v) in out.iter_mut().zip(a.iter()) {
            if let Some(id) = v.as_str().and_then(quick_action_id) {
                *slot = id;
            }
        }
    }
    out
}

/// A view saved with Save Current View As a New View (or New / Copy in Manage Views).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedView {
    pub name: String,
    /// The folder the view was created for (`None`: every mail folder).
    pub folder: Option<u64>,
    pub settings: ViewSettings,
    pub view: MailView,
}

/// Saved mail views, in the order they were made.
pub fn saved_views(s: &Session) -> Vec<SavedView> {
    let Some(Value::Array(a)) = ui_pref(s, KEY_SAVED) else { return Vec::new() };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    a.into_iter()
        .filter_map(|v| serde_json::from_value::<SavedView>(v).ok())
        .filter_map(|mut v| {
            v.name = clip(v.name.trim(), MAX_NAME);
            v.view = v.view.sanitized();
            v.view.name.clear();
            let ok = !v.name.is_empty() && !is_built_in(&v.name) && seen.insert(v.name.to_lowercase());
            ok.then_some(v)
        })
        .take(MAX_SAVED)
        .collect()
}

fn store_saved(s: &mut Session, views: &[SavedView]) -> Result<(), CmdError> {
    if views.is_empty() {
        return set_ui_pref(s, KEY_SAVED, None);
    }
    let v = serde_json::to_value(views).map_err(|e| CmdError::Failed(e.to_string()))?;
    set_ui_pref(s, KEY_SAVED, Some(v))
}

fn is_built_in(name: &str) -> bool {
    parse_layout(name).is_some()
}

/// Saved views usable in `folder` (made for it or for every mail folder).
pub fn views_for(s: &Session, folder: FolderId) -> Vec<SavedView> {
    saved_views(s).into_iter().filter(|v| v.folder.is_none_or(|f| f == folder.0)).collect()
}

/// Name of the view the folder shows: its saved view, else the built-in view of its layout.
pub fn current_view_name(s: &Session, folder: FolderId) -> String {
    let name = mail_view(s, folder).name;
    if !name.is_empty() && views_for(s, folder).iter().any(|v| v.name.eq_ignore_ascii_case(&name)) {
        return name;
    }
    super::layout_name(settings(s, folder).layout).to_owned()
}

// ---------------------------------------------------------------------------------------------------------
// Commands

#[allow(clippy::too_many_arguments)]
fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates, enabled, run }
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "view.columns",
            "Show Columns",
            "View › Current View › View Settings › Columns",
            r#"{"columns"?:[string],"add"?:string,"remove"?:string,"widths"?:{string:number|null},"reset"?:bool}"#,
            false,
            mail_list,
            set_columns,
        ),
        spec(
            "view.formatColumns",
            "Format Columns",
            "View › Current View › View Settings › Format Columns",
            r#"{"column"?:string,"label"?:string,"format"?:string,"align"?:"left"|"center"|"right"}"#,
            false,
            mail_list,
            format_columns,
        ),
        spec(
            "view.conditionalFormatting",
            "Conditional Formatting",
            "View › Current View › View Settings › Conditional Formatting",
            r#"{"rules"?:[{"name":string,"on":bool,"kind":"unread"|"overdue"|"expired"|"custom","condition"?:object,"font"?:{"bold"?:bool,"italic"?:bool,"underline"?:bool,"strike"?:bool,"color"?:string}}],"reset"?:bool}"#,
            false,
            mail_list,
            conditional_formatting,
        ),
        spec(
            "view.customFilter",
            "Filter",
            "View › Current View › View Settings › Filter",
            r#"{"filter"?:{"words"?:string,"inBody"?:bool,"from"?:string,"sentTo"?:string,"read"?:"unread"|"read","importance"?:"high"|"normal"|"low","hasAttachments"?:bool,"flagged"?:bool,"category"?:string,"days"?:number}|null}"#,
            false,
            mail_list,
            custom_filter_cmd,
        ),
        spec(
            "view.pin",
            "Pin / Unpin",
            "Message list › Quick Actions",
            r#"{"ids"?:[number],"on"?:bool}"#,
            false,
            crate::always,
            pin,
        ),
    ]
}

/// Mail and search folders (the views of this module); module folders and calendars have their own.
pub(super) fn mail_list(s: &Session) -> Option<&'static str> {
    match s.mailbox.folder(s.selection.folder).map(|f| f.kind) {
        None => Some("no folder is selected"),
        Some(FolderKind::Mail | FolderKind::Search) => None,
        Some(_) => Some("not available in this view"),
    }
}

/// `folder` is a mail or search folder (it has a mail view).
pub fn mail_list_folder(s: &Session, folder: FolderId) -> bool {
    s.mailbox.folder(folder).is_some_and(|f| matches!(f.kind, FolderKind::Mail | FolderKind::Search))
}

fn mail_folder(s: &Session) -> Result<FolderId, CmdError> {
    if let Some(why) = mail_list(s) {
        return Err(CmdError::Disabled(why.into()));
    }
    Ok(s.selection.folder)
}

fn clip(t: &str, max: usize) -> String {
    t.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn columns_json(s: &Session, folder: FolderId) -> Value {
    let v = mail_view(s, folder);
    let shown: Vec<&str> = columns(s, folder).iter().map(|c| c.id).collect();
    json!({
        "columns": shown,
        "widths": v.widths,
        "available": COLUMNS.iter().map(|c| json!({"id": c.id, "label": c.label})).collect::<Vec<_>>(),
    })
}

/// `view.columns`: sets the folder's columns (`columns`: the shown ids in order; `add` / `remove` one;
/// `widths`: px per column, null back to automatic; `reset`: the default columns and widths).
fn set_columns(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let mut v = mail_view(s, folder);
    if opt_bool(p, "reset") == Some(true) {
        v.columns.clear();
        v.widths.clear();
    }
    v.columns = if v.columns.is_empty() {
        DEFAULT_COLUMNS.iter().map(|c| (*c).to_owned()).collect()
    } else {
        v.columns
    };
    if let Some(list) = p.get("columns").filter(|x| !x.is_null()) {
        let list =
            list.as_array().ok_or_else(|| CmdError::Params("`columns` must be a list of ids".into()))?;
        let mut out: Vec<String> = Vec::new();
        for c in list {
            let id = c.as_str().ok_or_else(|| CmdError::Params("`columns` must be a list of ids".into()))?;
            let def =
                column(id).ok_or_else(|| CmdError::Params(format!("unknown column `{}`", clip(id, 40))))?;
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
        v.columns = out;
    }
    if let Some(id) = opt_str(p, "add") {
        let def = column(id).ok_or_else(|| CmdError::Params(format!("unknown column `{}`", clip(id, 40))))?;
        if !v.columns.iter().any(|x| x == def.id) {
            if v.columns.len() >= MAX_COLUMNS {
                return Err(CmdError::Params(format!("a view shows at most {MAX_COLUMNS} columns")));
            }
            v.columns.push(def.id.to_owned());
        }
    }
    if let Some(id) = opt_str(p, "remove") {
        let def = column(id).ok_or_else(|| CmdError::Params(format!("unknown column `{}`", clip(id, 40))))?;
        if v.columns.len() <= 1 && v.columns.iter().any(|x| x == def.id) {
            return Err(CmdError::Params("a view shows at least one column".into()));
        }
        v.columns.retain(|x| x != def.id);
    }
    if let Some(w) = p.get("widths").filter(|x| !x.is_null()) {
        let w = w.as_object().ok_or_else(|| CmdError::Params("`widths` must map column ids to px".into()))?;
        for (k, px) in w {
            let def =
                column(k).ok_or_else(|| CmdError::Params(format!("unknown column `{}`", clip(k, 40))))?;
            if px.is_null() {
                v.widths.remove(def.id);
                continue;
            }
            let px = px
                .as_f64()
                .filter(|f| f.is_finite())
                .ok_or_else(|| CmdError::Params("column widths are numbers of px".into()))?;
            let px = px.round().clamp(f64::from(MIN_WIDTH), f64::from(MAX_WIDTH));
            // Clamped to MIN_WIDTH..=MAX_WIDTH above, so the cast cannot truncate.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            v.widths.insert(def.id.to_owned(), px as u16);
        }
    }
    store_view(s, folder, &v);
    Ok(columns_json(s, folder))
}

fn formats_json(s: &Session, folder: FolderId) -> Value {
    let v = mail_view(s, folder);
    let list: Vec<Value> = columns(s, folder)
        .iter()
        .filter_map(|c| {
            let def = column(c.id)?;
            Some(json!({
                "id": c.id,
                "label": c.label,
                "defaultLabel": def.label,
                "format": c.format,
                "formats": def.kind.formats().iter().map(|(id, l)| json!({"id": id, "label": l})).collect::<Vec<_>>(),
                "align": c.align,
                "custom": v.formats.contains_key(c.id),
            }))
        })
        .collect();
    json!({ "folder": folder.0, "kind": "columns", "columns": list })
}

/// `view.formatColumns`: without `column` the Format Columns dialog opens; with it the column's label,
/// format and alignment change (empty strings back to the defaults).
fn format_columns(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let Some(id) = opt_str(p, "column") else {
        let params = formats_json(s, folder);
        return Ok(dialog(s, "viewFormat", params));
    };
    let def = column(id).ok_or_else(|| CmdError::Params(format!("unknown column `{}`", clip(id, 40))))?;
    let mut v = mail_view(s, folder);
    let mut f = v.formats.get(def.id).cloned().unwrap_or_default();
    if let Some(label) = opt_str(p, "label") {
        let label = label.trim();
        if label.chars().count() > MAX_NAME {
            return Err(CmdError::Params(format!("column labels are at most {MAX_NAME} characters")));
        }
        f.label = if label == def.label { String::new() } else { clip(label, MAX_NAME) };
    }
    if let Some(format) = opt_str(p, "format") {
        let formats = def.kind.formats();
        let Some((fid, _)) =
            formats.iter().find(|(id, l)| id.eq_ignore_ascii_case(format) || l.eq_ignore_ascii_case(format))
        else {
            return Err(CmdError::Params(format!(
                "the {} column has no format `{}`",
                def.label,
                clip(format, 40)
            )));
        };
        f.format =
            if formats.first().is_some_and(|(d, _)| d == fid) { String::new() } else { (*fid).to_owned() };
    }
    if let Some(align) = opt_str(p, "align") {
        f.align = match align {
            "left" | "center" | "right" => align.to_owned(),
            "" => String::new(),
            other => {
                return Err(CmdError::Params(format!(
                    "`align` must be left, center or right, not `{}`",
                    clip(other, 20)
                )));
            }
        };
    }
    if f == ColumnFormat::default() {
        v.formats.remove(def.id);
    } else {
        v.formats.insert(def.id.to_owned(), f);
    }
    store_view(s, folder, &v);
    let spec = columns(s, folder).into_iter().find(|c| c.id == def.id);
    Ok(json!({
        "column": def.id,
        "label": spec.as_ref().map_or(def.label.to_owned(), |c| c.label.clone()),
        "format": spec.as_ref().map_or("", |c| c.format),
        "align": spec.as_ref().map_or("", |c| c.align),
    }))
}

/// `view.conditionalFormatting`: without `rules` (or `reset`) the Conditional Formatting dialog opens;
/// `rules` replaces the folder's rules, `reset` restores the defaults.
fn conditional_formatting(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let mut v = mail_view(s, folder);
    if opt_bool(p, "reset") == Some(true) {
        v.rules = None;
        store_view(s, folder, &v);
        return Ok(json!({ "rules": default_rules(), "reset": true }));
    }
    let Some(list) = p.get("rules").filter(|x| !x.is_null()) else {
        let rules = rules(s, folder);
        return Ok(dialog(s, "viewFormat", json!({ "folder": folder.0, "kind": "rules", "rules": rules })));
    };
    let list = list.as_array().ok_or_else(|| CmdError::Params("`rules` must be a list".into()))?;
    if list.len() > MAX_RULES {
        return Err(CmdError::Params(format!("a view has at most {MAX_RULES} rules")));
    }
    let mut rules = Vec::new();
    for r in list {
        let rule: FormatRule = serde_json::from_value(r.clone())
            .map_err(|_| CmdError::Params("a rule needs a name, a kind and a font".into()))?;
        let rule = sanitize_rule(rule)
            .ok_or_else(|| CmdError::Params("a rule's kind is unread, overdue, expired or custom".into()))?;
        rules.push(rule);
    }
    v.rules = if rules == default_rules() { None } else { Some(rules) };
    store_view(s, folder, &v);
    Ok(json!({ "rules": self::rules(s, folder) }))
}

/// `view.customFilter`: without `filter` the Filter dialog opens; `filter` sets the View Settings filter
/// (null or an empty filter turns it off).
fn custom_filter_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let Some(f) = p.get("filter") else {
        let current = custom_filter(s, folder).unwrap_or_default();
        return Ok(dialog(s, "viewFilter", json!({ "folder": folder.0, "filter": current })));
    };
    let filter = if f.is_null() {
        None
    } else {
        let f: CustomFilter = serde_json::from_value(f.clone())
            .map_err(|_| CmdError::Params("`filter` must be a filter object or null".into()))?;
        Some(f.sanitized()).filter(|f| !f.is_empty())
    };
    let mut v = mail_view(s, folder);
    v.filter = filter;
    store_view(s, folder, &v);
    let f = custom_filter(s, folder);
    Ok(
        json!({ "on": f.is_some(), "filter": f.unwrap_or_default(), "description": custom_filter(s, folder).unwrap_or_default().describe() }),
    )
}

/// `view.pin`: pins the target messages to the top of their folder's list, or unpins them when all are
/// pinned (`on` sets the state).
fn pin(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> =
        mail_targets(s, p).into_iter().filter(|i| s.mailbox.mail.contains_key(i)).collect();
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let all = pinned(s);
    let on = match p.get("on") {
        None | Some(Value::Null) => !ids.iter().all(|i| all.contains(i)),
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`on` must be true or false".into())),
    };
    let mut by_folder: BTreeMap<FolderId, Vec<u64>> = BTreeMap::new();
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get(id) {
            by_folder.entry(m.folder).or_default().push(id.0);
        }
    }
    for (folder, list) in by_folder {
        // Pins of messages that left the folder are dropped on the way.
        let mut pins: BTreeSet<u64> = pins_of(s, folder)
            .into_iter()
            .filter(|i| s.mailbox.mail.get(&ItemId(*i)).is_some_and(|m| m.folder == folder))
            .collect();
        for id in list {
            if on {
                pins.insert(id);
            } else {
                pins.remove(&id);
            }
        }
        if pins.len() > MAX_PINNED {
            return Err(CmdError::Params(format!("a folder keeps at most {MAX_PINNED} pinned messages")));
        }
        let json = if pins.is_empty() { None } else { serde_json::to_string(&pins).ok() };
        store_reserved(s, folder, PINNED, json);
    }
    Ok(json!({ "on": on, "ids": ids.iter().map(|i| i.0).collect::<Vec<_>>() }))
}

/// `view.statusBar`: shows or hides one status bar element (saved with the profile).
pub(super) fn status_bar(s: &mut Session, p: &Value) -> CmdResult {
    let item = req_str(p, "item")?;
    let Some((key, _)) = STATUS_ITEMS.iter().find(|(k, _)| *k == item) else {
        return Err(CmdError::Params(format!("unknown status bar item `{}`", clip(item, 40))));
    };
    let show = opt_bool(p, "show").ok_or_else(|| CmdError::Params("`show` (bool) required".into()))?;
    let mut hidden = status_hidden(s);
    if show {
        hidden.remove(*key);
    } else {
        hidden.insert((*key).to_owned());
    }
    let default: BTreeSet<String> = STATUS_HIDDEN_DEFAULT.iter().map(|k| (*k).to_owned()).collect();
    let value = if hidden == default { None } else { Some(json!(hidden)) };
    set_ui_pref(s, KEY_STATUS, value)?;
    Ok(json!({ "item": key, "show": show, "hidden": status_hidden(s) }))
}

/// `view.quickActions`: without `actions` the Quick Actions dialog opens; `actions` (two ids or labels,
/// nearest the row's right edge first) sets them.
pub(super) fn quick_actions_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let Some(list) = p.get("actions").filter(|x| !x.is_null()) else {
        let current = quick_actions(s);
        let choices: Vec<Value> = QUICK_ACTIONS.iter().map(|(id, l)| json!({"id": id, "label": l})).collect();
        return Ok(dialog(s, "quickActions", json!({ "actions": current, "choices": choices })));
    };
    let list = list.as_array().ok_or_else(|| CmdError::Params("`actions` must be a list".into()))?;
    if list.is_empty() || list.len() > 2 {
        return Err(CmdError::Params("`actions` lists one or two Quick Actions".into()));
    }
    let mut out = ["none"; 2];
    for (slot, v) in out.iter_mut().zip(list) {
        let text = v.as_str().unwrap_or_default();
        *slot = quick_action_id(text)
            .ok_or_else(|| CmdError::Params(format!("unknown Quick Action `{}`", clip(text, 40))))?;
    }
    let value = if out == DEFAULT_QUICK_ACTIONS { None } else { Some(json!(out)) };
    set_ui_pref(s, KEY_QUICK, value)?;
    Ok(json!({ "actions": quick_actions(s) }))
}

// ---------------------------------------------------------------------------------------------------------
// Saved views

fn check_name(name: &str) -> Result<String, CmdError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CmdError::Params("the view needs a name".into()));
    }
    if name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(CmdError::Params(format!("view names are at most {MAX_NAME} printable characters")));
    }
    if is_built_in(name) {
        return Err(CmdError::Params(format!("a view named \"{name}\" already exists")));
    }
    Ok(name.to_owned())
}

fn scope_of(p: &Value, folder: FolderId) -> Result<Option<u64>, CmdError> {
    match opt_str(p, "scope").map(str::trim) {
        None | Some("" | "all") => Ok(None),
        Some("folder") => Ok(Some(folder.0)),
        Some(other) => {
            Err(CmdError::Params(format!("`scope` must be all or folder, not `{}`", clip(other, 20))))
        }
    }
}

fn used_on(s: &Session, v: &SavedView) -> String {
    match v.folder {
        None => ALL_FOLDERS.to_owned(),
        Some(f) => {
            s.mailbox.folder(FolderId(f)).map_or_else(|| "(deleted folder)".to_owned(), |x| x.name.clone())
        }
    }
}

/// The folder's current view as a saved view named `name`.
fn snapshot(s: &Session, folder: FolderId, name: &str, scope: Option<u64>) -> SavedView {
    let mut view = mail_view(s, folder);
    view.name.clear();
    SavedView { name: name.to_owned(), folder: scope, settings: settings(s, folder), view }
}

/// `view.saveViewAs` in a mail folder: without `confirm` the Save View dialog opens; with
/// `{"name", "scope"?: "all"|"folder", "confirm": true}` the folder's view is saved (replacing a saved view of
/// the same name) and becomes the folder's current view.
pub(super) fn save_view_as(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let raw = opt_str(p, "name").map(str::trim).unwrap_or_default().to_owned();
    if is_built_in(&raw) {
        return Err(CmdError::Params(format!("a view named \"{raw}\" already exists")));
    }
    if opt_bool(p, "confirm") != Some(true) {
        let current = serde_json::to_value(settings(s, folder)).unwrap_or(Value::Null);
        return Ok(dialog(s, "saveViewAs", json!({ "name": raw, "folder": folder.0, "settings": current })));
    }
    let name = check_name(&raw)?;
    let scope = scope_of(p, folder)?;
    let mut list = saved_views(s);
    list.retain(|v| !v.name.eq_ignore_ascii_case(&name));
    if list.len() >= MAX_SAVED {
        return Err(CmdError::Failed(format!("at most {MAX_SAVED} views can be saved")));
    }
    list.push(snapshot(s, folder, &name, scope));
    store_saved(s, &list)?;
    let mut v = mail_view(s, folder);
    v.name = name.clone();
    store_view(s, folder, &v);
    Ok(json!({ "saved": name, "usedOn": if scope.is_some() { "folder" } else { "all" } }))
}

/// Applies a saved view to `folder`: its settings (Focused Inbox and the window-wide spacing stay) and its
/// columns, formats, rules and filter.
fn apply_saved(s: &mut Session, folder: FolderId, saved: &SavedView) {
    let keep = settings(s, folder);
    let mut vs = saved.settings.clone();
    vs.focused_inbox = keep.focused_inbox;
    vs.tighter_spacing = keep.tighter_spacing;
    if vs == ViewSettings::default() {
        s.mailbox.views.remove(&folder);
    } else {
        s.mailbox.views.insert(folder, vs);
    }
    let mut v = saved.view.clone();
    v.name = saved.name.clone();
    store_view(s, folder, &v);
    super::modules::clear_groups(s, folder);
}

/// `view.changeView` with the name of a saved view usable in this folder (`None` = no such view).
pub(super) fn change_to_saved(s: &mut Session, name: &str) -> Option<CmdResult> {
    let folder = s.selection.folder;
    let saved = views_for(s, folder).into_iter().find(|v| v.name.eq_ignore_ascii_case(name.trim()))?;
    apply_saved(s, folder, &saved);
    Some(Ok(json!({ "view": saved.name, "saved": true })))
}

/// A built-in view was chosen: the folder no longer shows a saved view.
pub(super) fn clear_view_name(s: &mut Session, folder: FolderId) {
    let mut v = mail_view(s, folder);
    if !v.name.is_empty() {
        v.name.clear();
        store_view(s, folder, &v);
    }
}

/// `view.manageViews` in a mail folder: without `op` the Manage All Views dialog opens. Operations (by view
/// `name`): `new` (`base`: compact / single / preview, `scope`), `copy` (`newName`, `scope`), `rename`
/// (`newName`), `delete`, `reset` (a built-in view: the folder's view back to its defaults), `apply`, and
/// `update` (the saved view takes the folder's current settings: OK after Modify).
pub(super) fn manage_views(s: &mut Session, p: &Value) -> CmdResult {
    let folder = mail_folder(s)?;
    let Some(op) = opt_str(p, "op") else {
        let saved: Vec<Value> = saved_views(s)
            .iter()
            .map(|v| json!({ "name": v.name, "usedOn": used_on(s, v), "folder": v.folder }))
            .collect();
        let current = current_view_name(s, folder);
        return Ok(dialog(
            s,
            "manageViews",
            json!({ "views": BUILT_IN, "saved": saved, "current": current, "folder": folder.0, "mail": true }),
        ));
    };
    let name = req_str(p, "name")?.trim().to_owned();
    let mut list = saved_views(s);
    let pos = list.iter().position(|v| v.name.eq_ignore_ascii_case(&name));
    match op {
        "new" => {
            let name = check_name(&name)?;
            if pos.is_some() {
                return Err(CmdError::Params(format!("a view named \"{name}\" already exists")));
            }
            if list.len() >= MAX_SAVED {
                return Err(CmdError::Failed(format!("at most {MAX_SAVED} views can be saved")));
            }
            let base = opt_str(p, "base").and_then(parse_layout).unwrap_or(ViewLayout::Compact);
            let settings = ViewSettings { layout: base, ..ViewSettings::default() };
            let scope = scope_of(p, folder)?;
            list.push(SavedView { name: name.clone(), folder: scope, settings, view: MailView::default() });
            store_saved(s, &list)?;
            Ok(json!({ "op": "new", "name": name }))
        }
        "copy" => {
            let new_name = check_name(opt_str(p, "newName").unwrap_or_default())?;
            if list.iter().any(|v| v.name.eq_ignore_ascii_case(&new_name)) {
                return Err(CmdError::Params(format!("a view named \"{new_name}\" already exists")));
            }
            if list.len() >= MAX_SAVED {
                return Err(CmdError::Failed(format!("at most {MAX_SAVED} views can be saved")));
            }
            let scope = scope_of(p, folder)?;
            let copy = match pos.and_then(|i| list.get(i)) {
                Some(v) => SavedView { name: new_name.clone(), folder: scope, ..v.clone() },
                None => {
                    let layout = parse_layout(&name).ok_or_else(|| {
                        CmdError::Params(format!("no view named \"{}\"", clip(&name, MAX_NAME)))
                    })?;
                    // A built-in view: the folder's current settings when it shows it, else its defaults.
                    let mut snap = snapshot(s, folder, &new_name, scope);
                    if current_view_name(s, folder) != super::layout_name(layout) {
                        snap.settings = ViewSettings { layout, ..ViewSettings::default() };
                        snap.view = MailView::default();
                    }
                    snap
                }
            };
            list.push(copy);
            store_saved(s, &list)?;
            Ok(json!({ "op": "copy", "name": new_name }))
        }
        "rename" => {
            let i = pos.ok_or_else(|| CmdError::Disabled("built-in views cannot be renamed".into()))?;
            let new_name = check_name(opt_str(p, "newName").unwrap_or_default())?;
            if list.iter().enumerate().any(|(j, v)| j != i && v.name.eq_ignore_ascii_case(&new_name)) {
                return Err(CmdError::Params(format!("a view named \"{new_name}\" already exists")));
            }
            if let Some(v) = list.get_mut(i) {
                v.name = new_name.clone();
            }
            store_saved(s, &list)?;
            rename_in_folders(s, &name, &new_name);
            Ok(json!({ "op": "rename", "name": new_name }))
        }
        "delete" => {
            let i = pos.ok_or_else(|| CmdError::Disabled("built-in views cannot be deleted".into()))?;
            list.remove(i);
            store_saved(s, &list)?;
            rename_in_folders(s, &name, "");
            Ok(json!({ "op": "delete", "name": name }))
        }
        "reset" => {
            let layout = parse_layout(&name)
                .ok_or_else(|| CmdError::Disabled("only built-in views can be reset".into()))?;
            s.mailbox.views.remove(&folder);
            store_view(s, folder, &MailView::default());
            if layout != ViewLayout::Compact {
                s.mailbox.views.insert(folder, ViewSettings { layout, ..ViewSettings::default() });
            }
            super::modules::clear_groups(s, folder);
            Ok(json!({ "op": "reset", "name": super::layout_name(layout) }))
        }
        "apply" => match parse_layout(&name) {
            Some(_) => super::change_view_by_name(s, &name),
            None => change_to_saved(s, &name).unwrap_or_else(|| {
                Err(CmdError::Params(format!("no view named \"{}\"", clip(&name, MAX_NAME))))
            }),
        },
        "update" => {
            let i = pos.ok_or_else(|| CmdError::Disabled("built-in views keep their definition".into()))?;
            if let Some(v) = list.get_mut(i) {
                let scope = v.folder;
                *v = snapshot(s, folder, &v.name.clone(), scope);
            }
            store_saved(s, &list)?;
            let mut v = mail_view(s, folder);
            v.name = list.get(i).map(|x| x.name.clone()).unwrap_or_default();
            store_view(s, folder, &v);
            Ok(json!({ "op": "update", "name": name }))
        }
        other => Err(CmdError::Params(format!(
            "`op` must be new, copy, rename, delete, reset, apply or update, not `{}`",
            clip(other, 20)
        ))),
    }
}

/// Folders showing saved view `old` show `new` instead (empty: a built-in view again).
fn rename_in_folders(s: &mut Session, old: &str, new: &str) {
    let folders: Vec<FolderId> = s.view.collapsed_groups.keys().copied().collect();
    for f in folders {
        let mut v = mail_view(s, f);
        if !v.name.is_empty() && v.name.eq_ignore_ascii_case(old) {
            v.name = new.to_owned();
            store_view(s, f, &v);
        }
    }
}

/// View Settings summary of the folder's mail view (the dialog's descriptions).
pub(super) fn summary(s: &Session, folder: FolderId) -> Value {
    let cols: Vec<String> = columns(s, folder).into_iter().map(|c| c.label).collect();
    let rules = rules(s, folder);
    let on = rules.iter().filter(|r| r.on).count();
    json!({
        "columns": cols,
        "filter": custom_filter(s, folder).unwrap_or_default().describe(),
        "rules": format!("{on} of {} rules on", rules.len()),
        "formats": mail_view(s, folder).formats.len(),
        "view": current_view_name(s, folder),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Address, Mailbox};

    fn session() -> Session {
        let mut s = Session::new(Mailbox::new_local("Test"));
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.selection.folder = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.fixed_now = Some(DateTime { unix: 1_791_000_000, offset_min: 0 });
        s
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn add(s: &mut Session, m: MailItem) -> ItemId {
        let folder = s.selection.folder;
        s.mailbox.add_mail(MailItem { folder, ..m })
    }

    #[test]
    fn columns_default_set_and_change() {
        let mut s = session();
        let f = s.selection.folder;
        let ids: Vec<&str> = columns(&s, f).iter().map(|c| c.id).collect();
        assert_eq!(ids, DEFAULT_COLUMNS);
        let r = run(&mut s, "view.columns", json!({"columns": ["subject", "From", "received", "subject"]}));
        assert_eq!(r["columns"], json!(["subject", "from", "received"]));
        run(&mut s, "view.columns", json!({"add": "cc"}));
        run(&mut s, "view.columns", json!({"remove": "received"}));
        let ids: Vec<&str> = columns(&s, f).iter().map(|c| c.id).collect();
        assert_eq!(ids, ["subject", "from", "cc"]);
        run(&mut s, "view.columns", json!({"widths": {"cc": 5, "from": 9999}}));
        let cols = columns(&s, f);
        assert_eq!(cols.iter().find(|c| c.id == "cc").and_then(|c| c.width), Some(MIN_WIDTH));
        assert_eq!(cols.iter().find(|c| c.id == "from").and_then(|c| c.width), Some(MAX_WIDTH));
        run(&mut s, "view.columns", json!({"widths": {"cc": null}}));
        assert_eq!(columns(&s, f).iter().find(|c| c.id == "cc").and_then(|c| c.width), None);
        assert!(s.run("view.columns", &json!({"columns": []})).is_err());
        assert!(s.run("view.columns", &json!({"columns": ["bogus"]})).is_err());
        assert!(s.run("view.columns", &json!({"add": "nope"})).is_err());
        // Reset View drops the columns.
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(columns(&s, f).len(), DEFAULT_COLUMNS.len());
        // Not offered outside mail folders.
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.selection.folder = s.mailbox.role_folder(acct, FolderRole::Contacts).unwrap();
        assert!(matches!(s.run("view.columns", &json!({"add": "cc"})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn format_columns_label_format_align() {
        let mut s = session();
        let f = s.selection.folder;
        assert_eq!(run(&mut s, "view.formatColumns", json!({}))["dialog"], "viewFormat");
        run(
            &mut s,
            "view.formatColumns",
            json!({"column": "received", "format": "longDateTime", "label": "Arrived"}),
        );
        run(&mut s, "view.formatColumns", json!({"column": "size", "format": "Bytes", "align": "left"}));
        let cols = columns(&s, f);
        let rec = cols.iter().find(|c| c.id == "received").unwrap();
        assert_eq!((rec.label.as_str(), rec.format), ("Arrived", "longDateTime"));
        let size = cols.iter().find(|c| c.id == "size").unwrap();
        assert_eq!((size.format, size.align), ("bytes", "left"));
        assert!(s.run("view.formatColumns", &json!({"column": "size", "format": "longDateTime"})).is_err());
        assert!(s.run("view.formatColumns", &json!({"column": "size", "align": "middle"})).is_err());
        // Back to the defaults removes the entry.
        run(&mut s, "view.formatColumns", json!({"column": "size", "format": "kb", "align": ""}));
        run(
            &mut s,
            "view.formatColumns",
            json!({"column": "received", "format": "bestFit", "label": "Received"}),
        );
        assert!(mail_view(&s, f).formats.is_empty());
        let m = MailItem { size: 12_345, received: s.now(), ..Default::default() };
        assert_eq!(cell_text(&s, &m, "size", "bytes"), "12,345 bytes");
        assert_eq!(cell_text(&s, &m, "size", "kb"), "13 KB");
        assert_eq!(cell_text(&s, &m, "sent", "bestFit"), "None");
        assert!(cell_text(&s, &m, "received", "longDateTime").contains("2026"));
    }

    #[test]
    fn person_and_icon_cells() {
        let s = session();
        let m = MailItem {
            from: Some(Address { name: Some("Ann Lee".into()), addr: "ann@example.com".into() }),
            to: vec![Address { name: None, addr: "bo@example.org".into() }],
            importance: Importance::High,
            ..Default::default()
        };
        assert_eq!(cell_text(&s, &m, "from", "name"), "Ann Lee");
        assert_eq!(cell_text(&s, &m, "from", "address"), "ann@example.com");
        assert_eq!(cell_text(&s, &m, "from", "nameAndAddress"), "Ann Lee <ann@example.com>");
        assert_eq!(cell_text(&s, &m, "to", "name"), "bo@example.org");
        assert_eq!(cell_text(&s, &m, "importance", "text"), "High");
        assert_eq!(cell_text(&s, &m, "importance", "icon"), "");
    }

    #[test]
    fn conditional_formatting_rules() {
        let mut s = session();
        let f = s.selection.folder;
        let now = s.now();
        let unread = add(&mut s, MailItem { subject: "a".into(), ..Default::default() });
        let overdue = add(
            &mut s,
            MailItem {
                read: true,
                flag: FlagState::Flagged {
                    label: String::new(),
                    start: None,
                    due: Some(DateTime { unix: now.unix - 3 * 86_400, offset_min: 0 }),
                    reminder: None,
                },
                ..Default::default()
            },
        );
        let expired = add(
            &mut s,
            MailItem {
                read: true,
                raw: Some(b"Subject: x\r\nExpiry-Date: Mon, 1 Jan 2024 10:00:00 +0000\r\n\r\nbody".to_vec()),
                ..Default::default()
            },
        );
        let boss = add(
            &mut s,
            MailItem {
                read: true,
                from: Some(Address { name: Some("Boss".into()), addr: "boss@example.com".into() }),
                ..Default::default()
            },
        );
        let style = |s: &Session, id: ItemId| {
            let m = s.mailbox.mail.get(&id).unwrap();
            row_style(s, &rules(s, f), m, !m.read)
        };
        assert!(style(&s, unread).bold);
        assert_eq!(style(&s, overdue).color.as_deref(), Some("#c42b1c"));
        assert!(style(&s, expired).strike);
        assert_eq!(style(&s, boss), crate::queries::RowStyle::default());
        assert_eq!(run(&mut s, "view.conditionalFormatting", json!({}))["dialog"], "viewFormat");
        let mut list = default_rules();
        if let Some(r) = list.first_mut() {
            r.on = false;
        }
        list.push(FormatRule {
            name: "From the boss".into(),
            on: true,
            kind: "custom".into(),
            condition: CustomFilter { from: "boss@".into(), ..Default::default() },
            font: RuleFont { italic: true, color: "#00FF00".into(), ..Default::default() },
        });
        run(&mut s, "view.conditionalFormatting", json!({ "rules": list }));
        assert!(!style(&s, unread).bold, "the unread rule is off");
        let b = style(&s, boss);
        assert!(b.italic);
        assert_eq!(b.color.as_deref(), Some("#00ff00"));
        assert!(
            s.run("view.conditionalFormatting", &json!({"rules": [{"name": "x", "kind": "weird"}]})).is_err()
        );
        assert!(s.run("view.conditionalFormatting", &json!({"rules": "x"})).is_err());
        run(&mut s, "view.conditionalFormatting", json!({"reset": true}));
        assert_eq!(rules(&s, f), default_rules());
    }

    #[test]
    fn custom_filter_limits_the_list() {
        let mut s = session();
        let f = s.selection.folder;
        let now = s.now();
        add(&mut s, MailItem { subject: "Budget review".into(), received: now, ..Default::default() });
        add(
            &mut s,
            MailItem {
                subject: "Lunch".into(),
                received: DateTime { unix: now.unix - 40 * 86_400, offset_min: 0 },
                read: true,
                ..Default::default()
            },
        );
        assert_eq!(crate::queries::view_item_ids(&s).len(), 2);
        assert_eq!(run(&mut s, "view.customFilter", json!({}))["dialog"], "viewFilter");
        let r = run(&mut s, "view.customFilter", json!({"filter": {"words": "budget"}}));
        assert_eq!(r["on"], true);
        assert_eq!(crate::queries::view_item_ids(&s).len(), 1);
        run(&mut s, "view.customFilter", json!({"filter": {"days": 7, "read": "read"}}));
        assert!(crate::queries::view_item_ids(&s).is_empty());
        run(&mut s, "view.customFilter", json!({"filter": {"read": "read"}}));
        assert_eq!(crate::queries::view_item_ids(&s).len(), 1);
        assert_eq!(custom_filter(&s, f).map(|x| x.describe()).as_deref(), Some("read"));
        run(&mut s, "view.customFilter", json!({"filter": null}));
        assert!(custom_filter(&s, f).is_none());
        assert!(s.run("view.customFilter", &json!({"filter": 3})).is_err());
    }

    #[test]
    fn status_bar_items_persist_in_settings() {
        let mut s = session();
        assert!(!status_item_shown(&s, "quota"));
        assert!(status_item_shown(&s, "zoomSlider"));
        run(&mut s, "view.statusBar", json!({"item": "zoomSlider", "show": false}));
        assert!(!status_item_shown(&s, "zoomSlider"));
        assert!(s.settings.get("state.uiPrefs").unwrap().get(KEY_STATUS).is_some());
        run(&mut s, "view.statusBar", json!({"item": "zoomSlider", "show": true}));
        assert!(
            s.settings.get("state.uiPrefs").unwrap().get(KEY_STATUS).is_none(),
            "default state stores nothing"
        );
        run(&mut s, "view.statusBar", json!({"item": "quota", "show": true}));
        assert!(status_item_shown(&s, "quota"));
        assert!(s.run("view.statusBar", &json!({"item": "clock", "show": true})).is_err());
    }

    #[test]
    fn quick_actions_setting() {
        let mut s = session();
        assert_eq!(quick_actions(&s), DEFAULT_QUICK_ACTIONS);
        assert_eq!(run(&mut s, "view.quickActions", json!({}))["dialog"], "quickActions");
        run(&mut s, "view.quickActions", json!({"actions": ["archive", "Pin / Unpin"]}));
        assert_eq!(quick_actions(&s), ["archive", "pin"]);
        run(&mut s, "view.quickActions", json!({"actions": ["markRead"]}));
        assert_eq!(quick_actions(&s), ["markRead", "none"]);
        assert!(s.run("view.quickActions", &json!({"actions": ["explode"]})).is_err());
        assert!(s.run("view.quickActions", &json!({"actions": []})).is_err());
    }

    #[test]
    fn pin_puts_messages_on_top() {
        let mut s = session();
        let now = s.now();
        let old = add(
            &mut s,
            MailItem {
                subject: "old".into(),
                received: DateTime { unix: now.unix - 86_400 * 9, offset_min: 0 },
                ..Default::default()
            },
        );
        let new = add(&mut s, MailItem { subject: "new".into(), received: now, ..Default::default() });
        assert_eq!(crate::queries::visible_item_ids(&s), vec![new, old]);
        run(&mut s, "view.pin", json!({"ids": [old.0]}));
        assert!(pinned(&s).contains(&old));
        assert_eq!(crate::queries::visible_item_ids(&s), vec![old, new]);
        let rows = crate::queries::list_rows(&s);
        assert!(
            matches!(rows.first(), Some(crate::queries::ListRow::Group { label, .. }) if label == PINNED_LABEL)
        );
        // Reset View keeps pins; unpinning toggles.
        run(&mut s, "view.changeView", json!({"name": "Single"}));
        run(&mut s, "view.resetView", json!({}));
        assert!(pinned(&s).contains(&old));
        run(&mut s, "view.pin", json!({"ids": [old.0]}));
        assert!(pinned(&s).is_empty());
        assert!(s.run("view.pin", &json!({"ids": [123_456]})).is_err());
    }

    #[test]
    fn saved_views_save_apply_manage() {
        let mut s = session();
        let f = s.selection.folder;
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let sent = s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap();
        run(&mut s, "view.changeView", json!({"name": "Single"}));
        run(&mut s, "view.columns", json!({"columns": ["from", "subject"]}));
        assert_eq!(run(&mut s, "view.saveViewAs", json!({"name": "Mine"}))["dialog"], "saveViewAs");
        run(&mut s, "view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        assert_eq!(current_view_name(&s, f), "Mine");
        assert!(s.run("view.saveViewAs", &json!({"name": "preview", "confirm": true})).is_err());
        assert!(s.run("view.saveViewAs", &json!({"name": " ", "confirm": true})).is_err());
        // Apply it elsewhere from Change View.
        s.selection.folder = sent;
        run(&mut s, "view.changeView", json!({"name": "mine"}));
        assert_eq!(settings(&s, sent).layout, ViewLayout::Single);
        assert_eq!(columns(&s, sent).len(), 2);
        assert_eq!(current_view_name(&s, sent), "Mine");
        // Manage: copy, rename, delete; built-ins refuse rename/delete.
        assert_eq!(run(&mut s, "view.manageViews", json!({}))["dialog"], "manageViews");
        run(
            &mut s,
            "view.manageViews",
            json!({"op": "copy", "name": "Mine", "newName": "Other", "scope": "folder"}),
        );
        run(&mut s, "view.manageViews", json!({"op": "rename", "name": "Mine", "newName": "Renamed"}));
        assert_eq!(current_view_name(&s, sent), "Renamed");
        let names: Vec<String> = saved_views(&s).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["Renamed", "Other"]);
        assert_eq!(views_for(&s, f).len(), 1, "Other was made for Sent Items only");
        assert!(matches!(
            s.run("view.manageViews", &json!({"op": "delete", "name": "Compact"})),
            Err(CmdError::Disabled(_))
        ));
        run(&mut s, "view.manageViews", json!({"op": "new", "name": "Fresh", "base": "preview"}));
        run(&mut s, "view.manageViews", json!({"op": "apply", "name": "Fresh"}));
        assert_eq!(settings(&s, sent).layout, ViewLayout::Preview);
        assert_eq!(columns(&s, sent).len(), DEFAULT_COLUMNS.len());
        // Modify then OK stores the folder's settings into the view.
        run(&mut s, "view.reverseSort", json!({}));
        run(&mut s, "view.manageViews", json!({"op": "update", "name": "Fresh"}));
        let fresh = saved_views(&s).into_iter().find(|v| v.name == "Fresh").unwrap();
        assert_eq!(fresh.settings.order, mailcraft_model::SortOrder::Ascending);
        run(&mut s, "view.manageViews", json!({"op": "delete", "name": "Fresh"}));
        assert_eq!(current_view_name(&s, sent), "Preview");
        run(&mut s, "view.manageViews", json!({"op": "reset", "name": "Single"}));
        assert_eq!(settings(&s, sent).layout, ViewLayout::Single);
        assert!(s.run("view.manageViews", &json!({"op": "fly", "name": "x"})).is_err());
        // A built-in name picks the layout and leaves the saved view.
        s.selection.folder = f;
        run(&mut s, "view.changeView", json!({"name": "Compact"}));
        assert_eq!(current_view_name(&s, f), "Compact");
    }

    #[test]
    fn apply_view_copies_columns_and_rules() {
        let mut s = session();
        run(&mut s, "view.columns", json!({"columns": ["subject", "size"]}));
        run(&mut s, "view.customFilter", json!({"filter": {"flagged": true}}));
        run(&mut s, "view.applyViewTo", json!({"folders": ["Archive"]}));
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let archive = s.mailbox.role_folder(acct, FolderRole::Archive).unwrap();
        assert_eq!(columns(&s, archive).len(), 2);
        assert!(custom_filter(&s, archive).is_some_and(|f| f.flagged));
    }

    #[test]
    fn hostile_stored_state_falls_back() {
        let mut s = session();
        let f = s.selection.folder;
        s.view.collapsed_groups.insert(
            f,
            vec![
                format!("{MAIL_STATE}{{\"columns\":[\"nope\",\"subject\",\"subject\"],\"widths\":{{\"subject\":0,\"x\":5}},\"rules\":[{{\"kind\":\"bad\"}}],\"name\":\"\\u0000\"}}"),
                format!("{PINNED}not json"),
            ],
        );
        let v = mail_view(&s, f);
        assert_eq!(v.columns, ["subject"]);
        assert_eq!(v.widths.get("subject"), Some(&MIN_WIDTH));
        assert_eq!(v.rules, Some(Vec::new()));
        assert!(pinned(&s).is_empty());
        s.view.collapsed_groups.insert(f, vec![format!("{MAIL_STATE}[[[")]);
        assert_eq!(mail_view(&s, f), MailView::default());
        let _ = crate::queries::list_rows(&s);
    }

    #[test]
    fn expiry_header_parsing() {
        let m = |raw: &[u8]| MailItem { raw: Some(raw.to_vec()), ..Default::default() };
        assert!(expiry(&m(b"Expires: Tue, 2 Jan 2024 10:00:00 +0000\r\n\r\n")).is_some());
        assert!(expiry(&m(b"Expiry-Date:\r\n Tue, 2 Jan 2024 10:00:00 +0000\r\nX: y\r\n\r\n")).is_some());
        assert!(
            expiry(&m(b"Subject: x\r\n\r\nExpires: Tue, 2 Jan 2024 10:00:00 +0000")).is_none(),
            "body ignored"
        );
        assert!(expiry(&m(b"Expires: soon\r\n\r\n")).is_none());
        assert!(expiry(&m(&[0xff, 0xfe, b':', b'\n'])).is_none());
        assert!(expiry(&MailItem::default()).is_none());
    }
}
