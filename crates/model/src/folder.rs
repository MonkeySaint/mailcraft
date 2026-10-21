//! Folders. Default order and badge rules from `plan/reference/folders.md`.

use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId};

/// Well-known folder roles in the observed fixed display order (not alphabetical).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FolderRole {
    Inbox,
    Drafts,
    SentItems,
    DeletedItems,
    Archive,
    ConversationHistory,
    JunkEmail,
    Outbox,
    RssFeeds,
    SearchFolders,
    SyncIssues,
    Calendar,
    Contacts,
    Journal,
    Notes,
    Tasks,
}

impl FolderRole {
    /// Default display name (English).
    pub fn default_name(self) -> &'static str {
        match self {
            FolderRole::Inbox => "Inbox",
            FolderRole::Drafts => "Drafts",
            FolderRole::SentItems => "Sent Items",
            FolderRole::DeletedItems => "Deleted Items",
            FolderRole::Archive => "Archive",
            FolderRole::ConversationHistory => "Conversation History",
            FolderRole::JunkEmail => "Junk Email",
            FolderRole::Outbox => "Outbox",
            FolderRole::RssFeeds => "RSS Feeds",
            FolderRole::SearchFolders => "Search Folders",
            FolderRole::SyncIssues => "Sync Issues",
            FolderRole::Calendar => "Calendar",
            FolderRole::Contacts => "Contacts",
            FolderRole::Journal => "Journal",
            FolderRole::Notes => "Notes",
            FolderRole::Tasks => "Tasks",
        }
    }
    /// Sort rank among siblings; user folders sort after roles with rank < `USER_RANK` and before Search Folders.
    pub fn rank(self) -> u32 {
        self as u32 * 10
    }
    /// Drafts, Junk Email and Outbox show the total item count in brackets; others show unread.
    pub fn shows_total_count(self) -> bool {
        matches!(self, FolderRole::Drafts | FolderRole::JunkEmail | FolderRole::Outbox)
    }
    /// The folder kind items in this role hold.
    pub fn kind(self) -> FolderKind {
        match self {
            FolderRole::Calendar => FolderKind::Calendar,
            FolderRole::Contacts => FolderKind::Contacts,
            FolderRole::Journal => FolderKind::Journal,
            FolderRole::Notes => FolderKind::Notes,
            FolderRole::Tasks => FolderKind::Tasks,
            FolderRole::SearchFolders => FolderKind::SearchRoot,
            _ => FolderKind::Mail,
        }
    }
    /// Rank boundary: user folders sort between `RssFeeds` and `SearchFolders`.
    pub const USER_RANK: u32 = FolderRole::RssFeeds as u32 * 10 + 5;
    /// Roles created for every new mailbox, in display order.
    pub const DEFAULTS: [FolderRole; 15] = [
        FolderRole::Inbox,
        FolderRole::Drafts,
        FolderRole::SentItems,
        FolderRole::DeletedItems,
        FolderRole::Archive,
        FolderRole::ConversationHistory,
        FolderRole::JunkEmail,
        FolderRole::Outbox,
        FolderRole::RssFeeds,
        FolderRole::SearchFolders,
        FolderRole::Calendar,
        FolderRole::Contacts,
        FolderRole::Notes,
        FolderRole::Tasks,
        FolderRole::Journal,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderKind {
    #[default]
    Mail,
    Calendar,
    Contacts,
    Tasks,
    Notes,
    Journal,
    /// The "Search Folders" container.
    SearchRoot,
    /// A saved search (virtual folder); its query lives in `Folder::search_query`.
    Search,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account: AccountId,
    pub parent: Option<FolderId>,
    pub name: String,
    pub role: Option<FolderRole>,
    pub kind: FolderKind,
    /// Tree expansion state in the folder pane.
    pub expanded: bool,
    /// Position in Favorites (`None` = not a favourite).
    pub favorite_pos: Option<u32>,
    /// Manual order among user folders after "Move Up/Down" (`None` = alphabetical).
    pub manual_pos: Option<u32>,
    /// Query for search folders (search-crate syntax).
    pub search_query: Option<String>,
    /// Remote name/path on the server (IMAP mailbox name), if synced.
    pub remote_name: Option<String>,
    /// The folder's own AutoArchive properties (Properties › AutoArchive); `None` = the default settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_archive: Option<FolderArchive>,
}

/// How a folder takes part in AutoArchive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveMode {
    /// Archive items with the default settings (File › Options › Advanced › AutoArchive Settings).
    #[default]
    Default,
    /// Do not archive items in this folder.
    Never,
    /// Archive items with this folder's own age and action.
    Custom,
}

/// Unit of an AutoArchive age ("Clean out items older than 6 Months").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgeUnit {
    Days,
    Weeks,
    #[default]
    Months,
}

/// What AutoArchive does with old items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveAction {
    /// Move them to the archive folder.
    #[default]
    Move,
    /// Permanently delete them.
    Delete,
}

/// Longest AutoArchive age accepted in any unit.
pub const MAX_ARCHIVE_AGE: u32 = 999;

/// A folder's own AutoArchive properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FolderArchive {
    pub mode: ArchiveMode,
    /// Age past which items are old (in `unit`), 1..=[`MAX_ARCHIVE_AGE`].
    pub age: u32,
    pub unit: AgeUnit,
    pub action: ArchiveAction,
    /// Folder old items move to (`None` = the archive folder, mirroring the folder hierarchy).
    pub target: Option<FolderId>,
}

impl Default for FolderArchive {
    fn default() -> Self {
        FolderArchive {
            mode: ArchiveMode::Default,
            age: 6,
            unit: AgeUnit::Months,
            action: ArchiveAction::Move,
            target: None,
        }
    }
}

impl Folder {
    /// Default folders cannot be renamed, moved or deleted.
    pub fn is_default(&self) -> bool {
        self.role.is_some()
    }
    /// Sort key among siblings: (role rank or USER_RANK, manual position, lower-cased name).
    pub fn sort_key(&self) -> (u32, u32, String) {
        (
            self.role.map_or(FolderRole::USER_RANK, FolderRole::rank),
            self.manual_pos.unwrap_or(u32::MAX),
            self.name.to_lowercase(),
        )
    }
}
