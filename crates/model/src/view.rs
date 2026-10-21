//! Per-folder view settings (`plan/reference/message-list.md` › Views, `layout.md` › Panes).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ViewLayout {
    /// 3-line rows (default).
    #[default]
    Compact,
    /// One row per message with columns.
    Single,
    /// Single + preview line.
    Preview,
}

/// The 13 "Arrange by" keys, in menu order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArrangeBy {
    #[default]
    Date,
    From,
    To,
    Categories,
    FlagStatus,
    FlagStartDate,
    FlagDueDate,
    Size,
    Subject,
    Type,
    Attachments,
    Account,
    Importance,
}

impl ArrangeBy {
    pub const ALL: [ArrangeBy; 13] = [
        ArrangeBy::Date,
        ArrangeBy::From,
        ArrangeBy::To,
        ArrangeBy::Categories,
        ArrangeBy::FlagStatus,
        ArrangeBy::FlagStartDate,
        ArrangeBy::FlagDueDate,
        ArrangeBy::Size,
        ArrangeBy::Subject,
        ArrangeBy::Type,
        ArrangeBy::Attachments,
        ArrangeBy::Account,
        ArrangeBy::Importance,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ArrangeBy::Date => "Date",
            ArrangeBy::From => "From",
            ArrangeBy::To => "To",
            ArrangeBy::Categories => "Categories",
            ArrangeBy::FlagStatus => "Flag Status",
            ArrangeBy::FlagStartDate => "Flag: Start Date",
            ArrangeBy::FlagDueDate => "Flag: Due Date",
            ArrangeBy::Size => "Size",
            ArrangeBy::Subject => "Subject",
            ArrangeBy::Type => "Type",
            ArrangeBy::Attachments => "Attachments",
            ArrangeBy::Account => "Account",
            ArrangeBy::Importance => "Importance",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortOrder {
    /// "Newest on Top" (descending) — default for Date.
    #[default]
    Descending,
    Ascending,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ListFilter {
    #[default]
    All,
    Unread,
    Flagged,
    Mentioned,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReadingPanePos {
    #[default]
    Right,
    Bottom,
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderPaneState {
    #[default]
    Normal,
    Minimized,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSettings {
    pub layout: ViewLayout,
    pub arrange_by: ArrangeBy,
    pub order: SortOrder,
    pub show_in_groups: bool,
    pub conversations: bool,
    /// Message Preview: 0 (Off) … 3 lines.
    pub preview_lines: u8,
    pub filter: ListFilter,
    /// Inbox only.
    pub focused_inbox: bool,
    pub tighter_spacing: bool,
    pub reading_pane: ReadingPanePos,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            layout: ViewLayout::Compact,
            arrange_by: ArrangeBy::Date,
            order: SortOrder::Descending,
            show_in_groups: true,
            conversations: false,
            preview_lines: 1,
            filter: ListFilter::All,
            focused_inbox: true,
            tighter_spacing: false,
            reading_pane: ReadingPanePos::Right,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Advanced View Settings shared by every table view: group levels, sort keys, Other Settings

/// Most fields Group By nests (Group items by, then by, then by, then by).
pub const MAX_GROUP_LEVELS: usize = 4;
/// Most keys Sort orders by (Sort items by, then by, then by, then by).
pub const MAX_SORT_KEYS: usize = 4;
/// Joins the labels of nested groups into the key a row's group is collapsed by ("Categories: Red › Status:
/// Not Started").
pub const GROUP_SEP: &str = " \u{203a} ";
/// Smallest and largest font size (points) Other Settings accepts for rows and column headings; 0 keeps
/// the view's own size.
pub const MIN_VIEW_FONT: u8 = 8;
pub const MAX_VIEW_FONT: u8 = 24;

/// Group By › Expand/collapse defaults: how the groups of a view show when the view is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExpandDefault {
    /// Every group open (each opening of the view forgets groups collapsed by hand).
    #[default]
    AllExpanded,
    /// Every group closed when the view opens.
    AllCollapsed,
    /// Groups stay as they were left.
    AsLastViewed,
}

impl ExpandDefault {
    pub const ALL: [ExpandDefault; 3] =
        [ExpandDefault::AllExpanded, ExpandDefault::AllCollapsed, ExpandDefault::AsLastViewed];

    pub fn is_default(&self) -> bool {
        *self == ExpandDefault::AllExpanded
    }

    pub fn key(self) -> &'static str {
        match self {
            ExpandDefault::AllExpanded => "expanded",
            ExpandDefault::AllCollapsed => "collapsed",
            ExpandDefault::AsLastViewed => "lastViewed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExpandDefault::AllExpanded => "All expanded",
            ExpandDefault::AllCollapsed => "All collapsed",
            ExpandDefault::AsLastViewed => "As last viewed",
        }
    }

    /// From a key or a label ("collapsed", "All collapsed").
    pub fn parse(s: &str) -> Option<ExpandDefault> {
        let want: String = s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase();
        ExpandDefault::ALL.into_iter().find(|e| {
            let label: String =
                e.label().chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase();
            e.key().to_lowercase() == want || label == want || label.trim_start_matches("all") == want
        })
    }
}

/// View Settings › Other Settings of a table view: fonts of the rows and column headings, grid lines and
/// AutoPreview.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TableOther {
    /// Row font size in points (0 = the view's own).
    pub row_font: u8,
    /// Column heading font size in points (0 = the view's own).
    pub column_font: u8,
    /// No grid lines between rows.
    pub no_grid: bool,
    /// AutoPreview: a few lines of each item's text under its row.
    pub auto_preview: bool,
}

impl TableOther {
    pub fn is_default(&self) -> bool {
        *self == TableOther::default()
    }

    /// Font sizes clamped into [`MIN_VIEW_FONT`]..=[`MAX_VIEW_FONT`] (0 stays 0).
    pub fn sanitized(mut self) -> TableOther {
        self.row_font = clamp_font(self.row_font);
        self.column_font = clamp_font(self.column_font);
        self
    }

    /// One-line description for View Settings.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.row_font > 0 {
            parts.push(format!("rows {} pt", self.row_font));
        }
        if self.column_font > 0 {
            parts.push(format!("headings {} pt", self.column_font));
        }
        if self.no_grid {
            parts.push("no grid lines".into());
        }
        if self.auto_preview {
            parts.push("AutoPreview".into());
        }
        if parts.is_empty() { "Default fonts and grid lines".to_owned() } else { parts.join(", ") }
    }
}

/// A font size Other Settings accepts: 0 (the view's own) or within [`MIN_VIEW_FONT`]..=[`MAX_VIEW_FONT`].
pub fn clamp_font(pt: u8) -> u8 {
    if pt == 0 { 0 } else { pt.clamp(MIN_VIEW_FONT, MAX_VIEW_FONT) }
}

// ---------------------------------------------------------------------------------------------------------
// Item views of the Calendar, People, Tasks and Notes modules: Filter and Conditional Formatting
// ---------------------------------------------------------------------------------------------------------

/// Longest text field of an [`ItemFilter`] or name of an [`ItemRule`] (characters).
pub const MAX_ITEM_TEXT: usize = 256;
/// Most Conditional Formatting rules a module view keeps.
pub const MAX_ITEM_RULES: usize = 32;
/// Most days an [`ItemFilter`] looks back.
pub const MAX_ITEM_DAYS: u32 = 36_500;

/// View Settings › Filter of a module table (notes, calendar items): an item shows when it meets every
/// condition that is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ItemFilter {
    /// Words that must all appear (in the subject, or also in the body with `in_body`).
    pub words: String,
    pub in_body: bool,
    /// A category the item carries.
    pub category: String,
    /// Changed (notes) or starting (calendar items) within this many days of now, before or after
    /// (0 = any time).
    pub days: u32,
}

fn clip_text(s: &str, max: usize) -> String {
    s.trim().chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Most of an item's subject and of its text (bytes) a word filter searches; the rest is ignored, so a huge
/// imported description costs a bounded amount per frame.
pub const MAX_FILTER_TEXT: usize = 64 * 1024;

/// `s` cut to at most `max` bytes on a character boundary.
fn clip_bytes(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or_default()
}

/// An item's subject and text lowercased once (and capped at [`MAX_FILTER_TEXT`]), to test it against any
/// number of filters and formatting rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterText {
    subject: String,
    body: String,
}

impl FilterText {
    /// `with_body`: keep the text too (some filter searches it, [`ItemFilter::needs_body`]); without it a
    /// filter that searches the text only sees the subject.
    pub fn new(subject: &str, body: &str, with_body: bool) -> FilterText {
        FilterText {
            subject: clip_bytes(subject, MAX_FILTER_TEXT).to_lowercase(),
            body: if with_body { clip_bytes(body, MAX_FILTER_TEXT).to_lowercase() } else { String::new() },
        }
    }
}

impl ItemFilter {
    /// True when no condition is set.
    pub fn is_empty(&self) -> bool {
        self.words.trim().is_empty() && self.category.trim().is_empty() && self.days == 0
    }

    /// Text capped, numbers clamped (stored or given values are not trusted).
    pub fn sanitized(mut self) -> ItemFilter {
        self.words = clip_text(&self.words, MAX_ITEM_TEXT);
        self.category = clip_text(&self.category, MAX_ITEM_TEXT);
        self.days = self.days.min(MAX_ITEM_DAYS);
        self
    }

    /// Whether the filter searches the item's text (not only its subject).
    pub fn needs_body(&self) -> bool {
        self.in_body && !self.words.trim().is_empty()
    }

    /// Whether an item meets every condition. `when` is the unix time the day limit compares (a note's
    /// modification time, an appointment's start); `now` the current unix time. To test one item against
    /// several filters, prepare its [`FilterText`] once and use [`ItemFilter::matches_text`].
    pub fn matches(&self, subject: &str, body: &str, categories: &[String], when: i64, now: i64) -> bool {
        self.matches_text(&FilterText::new(subject, body, self.needs_body()), categories, when, now)
    }

    /// [`ItemFilter::matches`] with the item's text already prepared.
    pub fn matches_text(&self, text: &FilterText, categories: &[String], when: i64, now: i64) -> bool {
        if !self.words.trim().is_empty()
            && !self.words.split_whitespace().all(|w| {
                let w = w.to_lowercase();
                text.subject.contains(&w) || (self.in_body && text.body.contains(&w))
            })
        {
            return false;
        }
        let cat = self.category.trim();
        if !cat.is_empty() && !categories.iter().any(|c| c.trim().eq_ignore_ascii_case(cat)) {
            return false;
        }
        if self.days > 0 {
            let span = i64::from(self.days).saturating_mul(86_400);
            if when < now.saturating_sub(span) || when > now.saturating_add(span) {
                return false;
            }
        }
        true
    }

    /// One-line description for View Settings ("Off" when empty).
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.words.trim().is_empty() {
            let place = if self.in_body { "subject or text" } else { "subject" };
            parts.push(format!("\"{}\" in the {place}", self.words.trim()));
        }
        if !self.category.trim().is_empty() {
            parts.push(format!("category {}", self.category.trim()));
        }
        if self.days > 0 {
            parts.push(format!("within {} days", self.days));
        }
        if parts.is_empty() { "Off".to_owned() } else { parts.join(", ") }
    }
}

/// A Conditional Formatting rule of a module table: rows meeting `filter` get the font effects.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ItemRule {
    pub name: String,
    pub on: bool,
    pub filter: ItemFilter,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    /// "#rrggbb", empty = the list's own colour.
    pub color: String,
}

impl ItemRule {
    /// Name and colour checked, filter cleaned; `None` for a rule without a name.
    pub fn sanitized(mut self) -> Option<ItemRule> {
        self.name = clip_text(&self.name, MAX_ITEM_TEXT);
        if self.name.is_empty() {
            return None;
        }
        self.filter = self.filter.sanitized();
        let c = self.color.trim().to_ascii_lowercase();
        let ok =
            c.strip_prefix('#').is_some_and(|h| h.len() == 6 && h.chars().all(|x| x.is_ascii_hexdigit()));
        self.color = if ok { c } else { String::new() };
        Some(self)
    }
}

/// Rules cleaned and capped ([`MAX_ITEM_RULES`]).
pub fn sanitize_rules(rules: Vec<ItemRule>) -> Vec<ItemRule> {
    rules.into_iter().filter_map(ItemRule::sanitized).take(MAX_ITEM_RULES).collect()
}

/// Font effects a row gets from Conditional Formatting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    /// "#rrggbb".
    pub color: Option<String>,
}

/// Font effects of the rules (on, with a condition) an item meets, applied top to bottom (a later colour
/// wins).
/// Whether any rule that is on searches the item's text ([`FilterText::new`]'s `with_body`).
pub fn rules_need_body(rules: &[ItemRule]) -> bool {
    rules.iter().any(|r| r.on && r.filter.needs_body())
}

/// Whether any rule that is on has a condition (else [`rule_style`] is always the default).
pub fn rules_active(rules: &[ItemRule]) -> bool {
    rules.iter().any(|r| r.on && !r.filter.is_empty())
}

pub fn rule_style(rules: &[ItemRule], hit: impl Fn(&ItemFilter) -> bool) -> ItemStyle {
    let mut out = ItemStyle::default();
    for r in rules.iter().filter(|r| r.on && !r.filter.is_empty()) {
        if !hit(&r.filter) {
            continue;
        }
        out.bold |= r.bold;
        out.italic |= r.italic;
        out.strike |= r.strike;
        if !r.color.is_empty() {
            out.color = Some(r.color.clone());
        }
    }
    out
}

#[cfg(test)]
mod item_view_tests {
    use super::*;

    #[test]
    fn filter_matches_and_describes() {
        let f = ItemFilter { words: "budget Q3".into(), ..Default::default() };
        let cats = vec!["Red category".to_owned()];
        assert!(f.matches("Q3 Budget review", "", &cats, 0, 0));
        assert!(!f.matches("Budget", "q3 inside", &cats, 0, 0));
        let body = ItemFilter { in_body: true, ..f.clone() };
        assert!(body.matches("Budget", "q3 inside", &cats, 0, 0));
        let cat = ItemFilter { category: " red CATEGORY ".into(), ..Default::default() };
        assert!(cat.matches("x", "", &cats, 0, 0));
        assert!(!cat.matches("x", "", &[], 0, 0));
        let days = ItemFilter { days: 7, ..Default::default() };
        let now = 1_000_000_000;
        assert!(days.matches("x", "", &[], now - 6 * 86_400, now));
        assert!(!days.matches("x", "", &[], now - 8 * 86_400, now));
        assert!(!days.matches("x", "", &[], now + 8 * 86_400, now));
        // Extreme times do not overflow.
        let _ = days.matches("x", "", &[], i64::MIN, i64::MAX);
        assert_eq!(ItemFilter::default().describe(), "Off");
        assert!(f.describe().contains("budget Q3"));
        assert!(ItemFilter::default().is_empty());
    }

    #[test]
    fn prepared_text_is_capped_and_shared() {
        let f = ItemFilter { words: "needle".into(), in_body: true, ..Default::default() };
        let near = format!("{}NEEDLE", "x".repeat(100));
        let t = FilterText::new("Subject", &near, f.needs_body());
        assert!(f.matches_text(&t, &[], 0, 0));
        // Only the start of a huge text is searched (cut on a character boundary).
        let far = format!("{}needle", "\u{e9}".repeat(MAX_FILTER_TEXT));
        assert!(!f.matches("Subject", &far, &[], 0, 0));
        assert!(f.matches("Subject", &format!("needle{far}"), &[], 0, 0));
        // Text left out of the prepared text is not searched.
        let subject_only = FilterText::new("Subject", "needle", false);
        assert!(!f.matches_text(&subject_only, &[], 0, 0));
        assert_eq!(clip_bytes("\u{e9}\u{e9}", 3), "\u{e9}");
        assert_eq!(clip_bytes("ab", 10), "ab");
        let rule = ItemRule { on: true, filter: f.clone(), ..Default::default() };
        assert!(rules_need_body(std::slice::from_ref(&rule)) && rules_active(std::slice::from_ref(&rule)));
        assert!(!rules_active(&[ItemRule { on: false, ..rule }]));
    }

    #[test]
    fn sanitizing_caps_and_checks() {
        let f = ItemFilter { words: "w".repeat(1000), days: u32::MAX, ..Default::default() }.sanitized();
        assert_eq!(f.words.chars().count(), MAX_ITEM_TEXT);
        assert_eq!(f.days, MAX_ITEM_DAYS);
        let r = ItemRule { name: "  ".into(), ..Default::default() };
        assert!(r.sanitized().is_none());
        let r = ItemRule { name: "Red".into(), color: "#ABCDEF".into(), ..Default::default() };
        assert_eq!(r.sanitized().unwrap().color, "#abcdef");
        let r = ItemRule { name: "Bad".into(), color: "red".into(), ..Default::default() };
        assert_eq!(r.sanitized().unwrap().color, "");
        let many: Vec<ItemRule> =
            (0..100).map(|i| ItemRule { name: format!("r{i}"), ..Default::default() }).collect();
        assert_eq!(sanitize_rules(many).len(), MAX_ITEM_RULES);
    }

    #[test]
    fn expand_defaults_and_other_settings() {
        assert_eq!(ExpandDefault::parse("collapsed"), Some(ExpandDefault::AllCollapsed));
        assert_eq!(ExpandDefault::parse("All expanded"), Some(ExpandDefault::AllExpanded));
        assert_eq!(ExpandDefault::parse("As last viewed"), Some(ExpandDefault::AsLastViewed));
        assert_eq!(ExpandDefault::parse("lastViewed"), Some(ExpandDefault::AsLastViewed));
        assert_eq!(ExpandDefault::parse("sideways"), None);
        assert!(ExpandDefault::default().is_default());
        let o = TableOther { row_font: 200, column_font: 1, ..Default::default() }.sanitized();
        assert_eq!((o.row_font, o.column_font), (MAX_VIEW_FONT, MIN_VIEW_FONT));
        assert_eq!(clamp_font(0), 0);
        assert!(o.describe().contains("24 pt"));
        assert_eq!(TableOther::default().describe(), "Default fonts and grid lines");
    }

    #[test]
    fn rule_style_applies_in_order() {
        let red = ItemRule {
            name: "a".into(),
            on: true,
            filter: ItemFilter { words: "x".into(), ..Default::default() },
            bold: true,
            color: "#ff0000".into(),
            ..Default::default()
        };
        let blue = ItemRule { name: "b".into(), color: "#0000ff".into(), bold: false, ..red.clone() };
        let off = ItemRule { name: "c".into(), on: false, strike: true, ..red.clone() };
        let st = rule_style(&[red.clone(), blue, off], |f| f.words == "x");
        assert_eq!(st, ItemStyle { bold: true, color: Some("#0000ff".into()), ..Default::default() });
        assert_eq!(rule_style(&[red], |_| false), ItemStyle::default());
    }
}
