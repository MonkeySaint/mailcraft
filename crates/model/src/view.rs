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
