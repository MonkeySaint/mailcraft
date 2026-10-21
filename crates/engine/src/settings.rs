//! Profile settings: every File › Options setting (`plan/reference/options.md`, all 14 pages), the settings
//! of the option dialogs reached from them (Reading Pane, Junk Email Options, Stationery and Fonts,
//! AutoArchive) and UI state that outlives a session, as typed key/value pairs with defaults.
//!
//! - [`DEFS`] is the schema: key, Options page and section, label, kind (with default and range) and how far
//!   the setting reaches today ([`Effect`]). The Options window is generated from it.
//! - [`Settings`] holds the values that differ from the defaults; getters return the default otherwise.
//!   Writes are validated and normalised ([`Settings::set`]); hostile values are refused, never stored.
//! - The session saves `settings.json` in the profile folder (atomic write: temporary file + rename) together
//!   with the persisted [`crate::ViewState`] and recent searches, and loads it in `Session::open`
//!   ([`SettingsFile`]).
//!
//! Keys are `<page>.<name>` (`mail.send.ctrlEnterSends`, `calendar.workWeek`); a few keys are backed by the
//! session's view state instead of this store (`view.statusBar`, `view.favorites`, `view.simplifiedRibbon`,
//! `view.ribbonCollapsed`; [`Def::view`]), read and written through `options.get` / `options.set` like the
//! rest.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::CmdError;

/// What kind of value a setting holds, with its default and limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Bool(bool),
    /// Default, min, max.
    Int(i64, i64, i64),
    /// Default id and the choices as (id, label).
    Choice(&'static str, &'static [(&'static str, &'static str)]),
    /// Default and the longest accepted text in characters.
    Text(&'static str, usize),
    /// A time of day, "HH:MM" (24-hour).
    Time(&'static str),
    /// A time range, "HH:MM-HH:MM", start before end.
    TimeRange(&'static str),
    /// A non-empty set of weekdays (ids of [`WEEKDAYS`]), default given.
    Days(&'static [&'static str]),
    /// A list of strings (e.g. junk sender lists), at most this many entries; default empty.
    List(usize),
    /// A JSON object (UI state such as print page setup), at most this many bytes; default `{}`.
    Object(usize),
}

/// How far a setting reaches (counted for the honest parity figure).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Effect {
    /// Changes behaviour today.
    Applied,
    /// Stored, with a getter for the area that owns the behaviour (not wired yet).
    Exposed,
    /// Stored and shown; nothing reads it yet.
    Stored,
    /// Not applicable to MailCraft (cloud services, Office-wide options); kept so the page is complete.
    NotApplicable,
}

/// One setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Def {
    pub key: &'static str,
    /// Options page id (`general`, `mail`… as in `options.open`), or a dialog reached from one
    /// (`readingPane`, `stationery`, `autoArchive`), or `state` (UI state, not shown in Options). Junk Email
    /// Options live in the mailbox (`mail.junk`), not here.
    pub page: &'static str,
    /// Section heading on the page.
    pub section: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub effect: Effect,
    /// Backed by the session's view state, not by [`Settings`].
    pub view: bool,
}

impl Def {
    /// The default value as JSON.
    pub fn default_value(&self) -> Value {
        match self.kind {
            Kind::Bool(b) => json!(b),
            Kind::Int(d, _, _) => json!(d),
            Kind::Choice(d, _) | Kind::Text(d, _) | Kind::Time(d) | Kind::TimeRange(d) => json!(d),
            Kind::Days(d) => json!(d),
            Kind::List(_) => json!([]),
            Kind::Object(_) => json!({}),
        }
    }
}

/// Weekday ids, Sunday first (index = days from Sunday).
pub const WEEKDAYS: [(&str, &str); 7] = [
    ("sun", "Sunday"),
    ("mon", "Monday"),
    ("tue", "Tuesday"),
    ("wed", "Wednesday"),
    ("thu", "Thursday"),
    ("fri", "Friday"),
    ("sat", "Saturday"),
];

/// Options pages in window order (id, title), as `options.open` names them.
pub const PAGES: [(&str, &str); 14] = [
    ("general", "General"),
    ("mail", "Mail"),
    ("calendar", "Calendar"),
    ("groups", "Groups"),
    ("people", "People"),
    ("tasks", "Tasks"),
    ("search", "Search"),
    ("language", "Language"),
    ("accessibility", "Accessibility"),
    ("advanced", "Advanced"),
    ("customizeRibbon", "Customize Ribbon"),
    ("quickAccessToolbar", "Quick Access Toolbar"),
    ("addIns", "Add-ins"),
    ("trustCenter", "Trust Center"),
];

/// Settings also shown on a second page: (page, section, key).
pub const ALSO_ON: [(&str, &str, &str); 3] = [
    ("accessibility", "Application display options", "general.screenTips"),
    ("accessibility", "Application display options", "mail.other.noAutoExpandConversations"),
    ("accessibility", "Application display options", "mail.send.mentions"),
];

const fn d(
    page: &'static str,
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: Kind,
    effect: Effect,
) -> Def {
    Def { key, page, section, label, kind, effect, view: false }
}

const fn v(
    page: &'static str,
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: Kind,
) -> Def {
    Def { key, page, section, label, kind, effect: Effect::Applied, view: true }
}

use Effect::{Applied as A, Exposed as E, NotApplicable as N, Stored as S};
use Kind::{Bool, Choice, Days, Int, List, Object, Text, Time, TimeRange};

const THEMES: &[(&str, &str)] = &[("light", "Light"), ("dark", "Dark"), ("system", "Use system setting")];
const SCREEN_TIPS: &[(&str, &str)] = &[
    ("descriptions", "Show feature descriptions in ScreenTips"),
    ("noDescriptions", "Don't show feature descriptions in ScreenTips"),
    ("none", "Don't show ScreenTips"),
];
const DISPLAYS: &[(&str, &str)] =
    &[("appearance", "Optimize for best appearance"), ("compatibility", "Optimize for compatibility")];
const BACKGROUNDS: &[(&str, &str)] =
    &[("none", "No Background"), ("circles", "Circles and Stripes"), ("geometry", "Geometry")];
const STARTUP: &[(&str, &str)] = &[
    ("ask", "Ask me if I want to reopen previous items"),
    ("always", "Always reopen previous items"),
    ("never", "Never reopen previous items"),
];
const CLOUD_ATTACH: &[(&str, &str)] = &[
    ("ask", "Ask me how I want to attach them every time"),
    ("link", "Always share them as links"),
    ("copy", "Always attach them as copies"),
];
const FORMATS: &[(&str, &str)] = &[("html", "HTML"), ("rich", "Rich Text"), ("plain", "Plain Text")];
const REPLY_TEXT: &[(&str, &str)] = &[
    ("include", "Include original message text"),
    ("none", "Do not include original message"),
    ("attach", "Attach original message"),
    ("indent", "Include and indent original message text"),
    ("prefix", "Prefix each line of the original message"),
];
const FORWARD_TEXT: &[(&str, &str)] = &[
    ("include", "Include original message text"),
    ("attach", "Attach original message"),
    ("indent", "Include and indent original message text"),
    ("prefix", "Prefix each line of the original message"),
];
const SAVE_FOLDERS: &[(&str, &str)] =
    &[("drafts", "Drafts"), ("inbox", "Inbox"), ("sentItems", "Sent Items"), ("outbox", "Outbox")];
const IMPORTANCE: &[(&str, &str)] = &[("low", "Low"), ("normal", "Normal"), ("high", "High")];
const SENSITIVITY: &[(&str, &str)] = &[
    ("normal", "Normal"),
    ("personal", "Personal"),
    ("private", "Private"),
    ("confidential", "Confidential"),
];
const READ_RECEIPT: &[(&str, &str)] = &[
    ("always", "Always send a read receipt"),
    ("never", "Never send a read receipt"),
    ("ask", "Ask each time whether to send a read receipt"),
];
const RTF_TO_INTERNET: &[(&str, &str)] = &[
    ("html", "Convert to HTML format"),
    ("plain", "Convert to Plain Text format"),
    ("rich", "Send using Rich Text format"),
];
const AFTER_MOVE: &[(&str, &str)] = &[
    ("returnToFolder", "return to the current folder"),
    ("openPrevious", "open the previous item"),
    ("openNext", "open the next item"),
];
const FIRST_WEEK: &[(&str, &str)] =
    &[("jan1", "Starts on Jan 1"), ("first4", "First 4-day week"), ("firstFull", "First full week")];
const DAYS_CHOICE: &[(&str, &str)] = &WEEKDAYS;
const PROPOSE_RESPONSE: &[(&str, &str)] =
    &[("tentative", "Tentative"), ("accept", "Accept"), ("decline", "Decline")];
const GROUP_ATTACH: &[(&str, &str)] = &[
    ("ask", "Ask me how I want to attach them every time"),
    ("upload", "Always upload to the group's files"),
    ("copy", "Always attach them as copies"),
];
const FULL_NAME: &[(&str, &str)] = &[
    ("firstMiddleLast", "First (Middle) Last"),
    ("lastFirst", "Last First"),
    ("firstLast1Last2", "First Last1 Last2"),
];
const FILE_AS: &[(&str, &str)] = &[
    ("lastFirst", "Last, First"),
    ("firstLast", "First Last"),
    ("company", "Company"),
    ("lastFirstCompany", "Last, First (Company)"),
    ("companyLastFirst", "Company (Last, First)"),
];
const INDEX_LANGUAGES: &[(&str, &str)] = &[
    ("arabic", "Arabic"),
    ("cyrillic", "Cyrillic"),
    ("greek", "Greek"),
    ("hebrew", "Hebrew"),
    ("thai", "Thai"),
    ("vietnamese", "Vietnamese"),
];
const QUICK_CLICK_FLAG: &[(&str, &str)] = &[
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("thisWeek", "This Week"),
    ("nextWeek", "Next Week"),
    ("noDate", "No Date"),
    ("complete", "Mark Complete"),
];
const SEARCH_SCOPES: &[(&str, &str)] = &[
    ("currentFolder", "Current folder"),
    ("subfolders", "Current folder and its subfolders"),
    ("currentMailbox", "Current mailbox"),
    ("allMailboxes", "All mailboxes"),
    ("allItems", "All items"),
];
const DISPLAY_LANGUAGES: &[(&str, &str)] = &[("en-US", "English (United States)")];
const AUTHORING_LANGUAGES: &[(&str, &str)] = &[
    ("en-US", "English (United States)"),
    ("en-CA", "English (Canada)"),
    ("en-GB", "English (United Kingdom)"),
    ("fr-CA", "French (Canada)"),
    ("fr-FR", "French (France)"),
    ("es-ES", "Spanish"),
    ("de-DE", "German"),
];
const TRANSLATE: &[(&str, &str)] =
    &[("always", "Always translate"), ("ask", "Ask me before translating"), ("never", "Never translate")];
const TRANSLATE_TO: &[(&str, &str)] =
    &[("en", "English"), ("fr", "French"), ("es", "Spanish"), ("de", "German")];
const ACCESS_WARNINGS: &[(&str, &str)] = &[
    ("whileWorking", "Show me accessibility warnings while I work"),
    ("when", "Show me accessibility warnings when"),
    ("checkerOnly", "Show me accessibility warnings only when I open the accessibility checker"),
];
const ALT_CALENDARS: &[(&str, &str)] = &[("hijri", "Hijri (tabular)"), ("hebrew", "Hebrew lunar")];
const SOUND_SCHEMES: &[(&str, &str)] = &[("modern", "Modern"), ("classic", "Classic")];
const OPEN_FILES: &[(&str, &str)] = &[("browser", "Browser"), ("desktop", "Desktop app")];
const OPEN_LINKS: &[(&str, &str)] = &[("default", "Default browser")];
const ENCODINGS: &[(&str, &str)] = &[
    ("utf-8", "Unicode (UTF-8)"),
    ("iso-8859-1", "Western European (ISO)"),
    ("windows-1252", "Western European (Windows)"),
];
const PASTE_MODES: &[(&str, &str)] = &[
    ("keepSource", "Keep Source Formatting"),
    ("merge", "Merge Formatting"),
    ("textOnly", "Keep Text Only"),
];
/// Section of the Editor Options proofing settings (Mail page).
pub const EDITOR_PROOFING: &str = "Editor Options › Proofing";
/// Section of the Editor Options paste defaults (Mail page; Set Default Paste opens it).
pub const EDITOR_PASTE: &str = "Editor Options › Cut, copy, and paste";
const QAT_POSITION: &[(&str, &str)] = &[("above", "Above Ribbon"), ("below", "Below Ribbon")];
const AUTO_ARCHIVE_ACTION: &[(&str, &str)] =
    &[("move", "Move old items to the archive folder"), ("delete", "Permanently delete old items")];

/// Every setting. Labels name the feature (MailCraft wording); the order is the page order of the window.
pub const DEFS: &[Def] = &[
    // ---------------------------------------------------------------- General
    d(
        "general",
        "Cloud storage options",
        "general.cloudSettings",
        "Store my MailCraft settings in the cloud",
        Bool(false),
        N,
    ),
    // N: MailCraft always lays itself out again at each display's scale, keeping its physical size (egui and
    // winit are per-monitor scale aware). "Optimize for compatibility" asks the system to stretch a bitmap
    // drawn at one scale instead; MailCraft has no such mode, and shrinking or growing the UI is not one.
    d(
        "general",
        "User Interface options",
        "general.multipleDisplays",
        "When using multiple displays:",
        Choice("appearance", DISPLAYS),
        N,
    ),
    d(
        "general",
        "User Interface options",
        "general.miniToolbar",
        "Show Mini Toolbar on selection",
        Bool(true),
        A,
    ),
    d("general", "User Interface options", "general.livePreview", "Enable Live Preview", Bool(true), A),
    d(
        "general",
        "User Interface options",
        "general.screenTips",
        "ScreenTip style:",
        Choice("descriptions", SCREEN_TIPS),
        E,
    ),
    d("general", "Personalize your copy of MailCraft", "general.userName", "User name:", Text("", 128), A),
    d("general", "Personalize your copy of MailCraft", "general.initials", "Initials:", Text("", 16), E),
    d(
        "general",
        "Personalize your copy of MailCraft",
        "general.alwaysUseValues",
        "Always use these values regardless of sign in",
        Bool(false),
        N,
    ),
    d(
        "general",
        "Personalize your copy of MailCraft",
        "general.background",
        "Background:",
        Choice("none", BACKGROUNDS),
        A,
    ),
    d("general", "Personalize your copy of MailCraft", "general.theme", "Theme:", Choice("light", THEMES), A),
    d(
        "general",
        "Personalize your copy of MailCraft",
        "general.keepMessageBackground",
        "Never change the message background color",
        Bool(false),
        A,
    ),
    d("general", "Start up options", "general.startup", "When MailCraft opens:", Choice("ask", STARTUP), S),
    d(
        "general",
        "Attachment options",
        "general.cloudAttach",
        "For files I choose from cloud storage:",
        Choice("copy", CLOUD_ATTACH),
        N,
    ),
    d("general", "LinkedIn Features", "general.linkedIn", "Enable LinkedIn features", Bool(false), N),
    // ---------------------------------------------------------------- Mail
    d(
        "mail",
        "Compose messages",
        "mail.compose.defaultFormat",
        "Compose messages in this format:",
        Choice("html", FORMATS),
        A,
    ),
    d(
        "mail",
        "Compose messages",
        "mail.compose.spellBeforeSend",
        "Always check spelling before sending",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Compose messages",
        "mail.compose.ignoreOriginal",
        "Ignore original message text in reply or forward",
        Bool(true),
        A,
    ),
    d("mail", "Compose messages", "mail.compose.showBcc", "Always show the Bcc box", Bool(false), A),
    d("mail", "Compose messages", "mail.compose.showFrom", "Always show the From box", Bool(false), A),
    d("mail", "Message arrival", "mail.arrival.playSound", "Play a sound", Bool(true), E),
    d(
        "mail",
        "Message arrival",
        "mail.arrival.changePointer",
        "Briefly change the mouse pointer",
        Bool(false),
        S,
    ),
    d(
        "mail",
        "Message arrival",
        "mail.arrival.envelope",
        "Show an envelope icon in the taskbar",
        Bool(true),
        A,
    ),
    d("mail", "Message arrival", "mail.arrival.desktopAlert", "Display a Desktop Alert", Bool(true), E),
    d(
        "mail",
        "Message arrival",
        "mail.arrival.rightsProtectedPreview",
        "Enable preview for Rights Protected messages",
        Bool(false),
        N,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.folder",
        "Cleaned-up items will go to this folder (empty = Deleted Items):",
        Text("", 512),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.recreateHierarchy",
        "When cleaning sub-folders, recreate the folder hierarchy in the destination folder",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.keepUnread",
        "Don't move unread messages",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.keepCategorized",
        "Don't move categorized messages",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.keepFlagged",
        "Don't move flagged messages",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.keepSigned",
        "Don't move digitally-signed messages",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Conversation Clean Up",
        "mail.cleanup.keepOriginal",
        "When a reply modifies a message, don't move the original",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.suggestedReplies",
        "Show suggested replies",
        Bool(false),
        N,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.newWindow",
        "Open replies and forwards in a new window",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.closeOriginal",
        "Close original message window when replying or forwarding",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.prefaceComments",
        "Preface comments with my name",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.prefaceName",
        "Name used to preface comments:",
        Text("", 128),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.replyText",
        "When replying to a message:",
        Choice("include", REPLY_TEXT),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.forwardText",
        "When forwarding a message:",
        Choice("include", FORWARD_TEXT),
        A,
    ),
    d(
        "mail",
        "Replies and forwards",
        "mail.reply.plainPrefix",
        "Preface each line in a plain-text message with:",
        Text("> ", 8),
        A,
    ),
    d(
        "mail",
        "Save messages",
        "mail.save.autoSave",
        "Automatically save items that have not been sent",
        Bool(true),
        A,
    ),
    d("mail", "Save messages", "mail.save.autoSaveMinutes", "Minutes between saves:", Int(3, 1, 99), A),
    d("mail", "Save messages", "mail.save.folder", "Save to this folder:", Choice("drafts", SAVE_FOLDERS), A),
    d(
        "mail",
        "Save messages",
        "mail.save.replyInSameFolder",
        "When replying to a message that is not in the Inbox, save the reply in the same folder",
        Bool(false),
        A,
    ),
    d("mail", "Save messages", "mail.save.forwarded", "Save forwarded messages", Bool(true), A),
    d(
        "mail",
        "Save messages",
        "mail.save.sentCopies",
        "Save copies of messages in the Sent Items folder",
        Bool(true),
        A,
    ),
    d("mail", "Save messages", "mail.save.unicode", "Use Unicode format", Bool(true), S),
    d(
        "mail",
        "Send messages",
        "mail.send.defaultImportance",
        "Default Importance level:",
        Choice("normal", IMPORTANCE),
        A,
    ),
    d(
        "mail",
        "Send messages",
        "mail.send.defaultSensitivity",
        "Default Sensitivity level:",
        Choice("normal", SENSITIVITY),
        A,
    ),
    d(
        "mail",
        "Send messages",
        "mail.send.expire",
        "Mark messages as expired after a number of days",
        Bool(false),
        A,
    ),
    d("mail", "Send messages", "mail.send.expireDays", "Days before messages expire:", Int(0, 0, 999), A),
    d(
        "mail",
        "Send messages",
        "mail.send.alwaysDefaultAccount",
        "Always use the default account when composing new messages",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Send messages",
        "mail.send.commasSeparate",
        "Commas can be used to separate multiple message recipients",
        Bool(false),
        A,
    ),
    d("mail", "Send messages", "mail.send.autoNameCheck", "Automatic name checking", Bool(true), A),
    d(
        "mail",
        "Send messages",
        "mail.send.deleteMeetingRequests",
        "Delete meeting requests and notifications from Inbox after responding",
        Bool(true),
        E,
    ),
    d("mail", "Send messages", "mail.send.ctrlEnterSends", "CTRL + ENTER sends a message", Bool(true), A),
    d(
        "mail",
        "Send messages",
        "mail.autoComplete",
        "Use Auto-Complete List to suggest names in the To, Cc and Bcc lines",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Send messages",
        "mail.send.attachmentReminder",
        "Warn me when I send a message that may be missing an attachment",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Send messages",
        "mail.send.mentions",
        "Suggest names to mention when I use the @ symbol in a message",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.requestDelivery",
        "Request a delivery receipt for all messages sent",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.requestRead",
        "Request a read receipt for all messages sent",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.readReceiptResponse",
        "For any message received that includes a read receipt request:",
        Choice("ask", READ_RECEIPT),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.autoProcessMeetings",
        "Automatically process meeting requests and responses to meeting requests and polls",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.updateSentItem",
        "Automatically update original sent item with receipt information",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.deleteWithoutComments",
        "Update tracking information, and then delete responses that don't contain comments",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Tracking",
        "mail.tracking.moveReceipts",
        "After updating tracking information, move receipt to a folder",
        Bool(false),
        A,
    ),
    d("mail", "Tracking", "mail.tracking.moveReceiptsTo", "Move receipts to:", Text("Deleted Items", 512), A),
    d(
        "mail",
        "Message format",
        "mail.format.css",
        "Use Cascading Style Sheets (CSS) for appearance of messages",
        Bool(true),
        S,
    ),
    d(
        "mail",
        "Message format",
        "mail.format.reduceSize",
        "Reduce message size by removing format information not necessary to display the message",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Message format",
        "mail.format.uuencode",
        "Encode attachments in UUENCODE format when sending plain-text messages",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Message format",
        "mail.format.wrapAt",
        "Automatically wrap text at character:",
        Int(76, 30, 132),
        A,
    ),
    d(
        "mail",
        "Message format",
        "mail.format.removeExtraLineBreaks",
        "Remove extra line breaks in plain text messages",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Message format",
        "mail.format.richTextToInternet",
        "When sending messages in Rich Text format to Internet recipients:",
        Choice("html", RTF_TO_INTERNET),
        A,
    ),
    d(
        "mail",
        "Other",
        "mail.other.pasteOptions",
        "Show Paste Options button when content is pasted into a message",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Other",
        "mail.other.nextPreviousLinks",
        "Show Next and Previous links in the message headers when reading messages",
        Bool(true),
        A,
    ),
    d(
        "mail",
        "Other",
        "mail.other.noAutoExpandConversations",
        "Don't automatically expand conversations when using the keyboard to change messages",
        Bool(false),
        A,
    ),
    d(
        "mail",
        "Other",
        "mail.other.afterMoveOrDelete",
        "After moving or deleting an open item:",
        Choice("returnToFolder", AFTER_MOVE),
        A,
    ),
    d("mail", "Other", "mail.other.markReadOnDelete", "Mark messages as read when deleted", Bool(false), A),
    d(
        "mail",
        "Other",
        "mail.other.highlightFlagged",
        "Highlight flagged items in the message list",
        Bool(true),
        E,
    ),
    // Mail › Editor Options... (Proofing; Advanced › Cut, copy, and paste)
    d("mail", EDITOR_PROOFING, "mail.editor.spellAsYouType", "Check spelling as you type", Bool(true), A),
    d(
        "mail",
        EDITOR_PROOFING,
        "mail.editor.grammarAsYouType",
        "Mark grammar errors as you type",
        Bool(true),
        A,
    ),
    d("mail", EDITOR_PROOFING, "mail.editor.ignoreUppercase", "Ignore words in UPPERCASE", Bool(true), A),
    d(
        "mail",
        EDITOR_PROOFING,
        "mail.editor.ignoreNumbers",
        "Ignore words that contain numbers",
        Bool(true),
        A,
    ),
    d(
        "mail",
        EDITOR_PROOFING,
        "mail.editor.ignoreInternet",
        "Ignore Internet and file addresses",
        Bool(true),
        A,
    ),
    d("mail", EDITOR_PROOFING, "mail.editor.flagRepeated", "Flag repeated words", Bool(true), A),
    d(
        "mail",
        EDITOR_PROOFING,
        "mail.editor.mainDictionaryOnly",
        "Suggest from main dictionary only",
        Bool(false),
        A,
    ),
    d(
        "mail",
        EDITOR_PASTE,
        "mail.editor.pasteWithinMessage",
        "Pasting within the same message:",
        Choice("keepSource", PASTE_MODES),
        A,
    ),
    d(
        "mail",
        EDITOR_PASTE,
        "mail.editor.pasteBetweenMessages",
        "Pasting between messages:",
        Choice("keepSource", PASTE_MODES),
        A,
    ),
    d(
        "mail",
        EDITOR_PASTE,
        "mail.editor.pasteFromOtherPrograms",
        "Pasting from other programs:",
        Choice("keepSource", PASTE_MODES),
        A,
    ),
    // Mail › Reading Pane... (dialog)
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.markReadWhenViewed",
        "Mark items as read when viewed in the Reading Pane",
        Bool(false),
        A,
    ),
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.markReadAfterSeconds",
        "Seconds before marking item as read",
        Int(5, 0, 999),
        A,
    ),
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.markReadOnSelectionChange",
        "Mark item as read when selection changes",
        Bool(true),
        A,
    ),
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.spaceBarReading",
        "Single key reading using space bar",
        Bool(true),
        A,
    ),
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.portraitFullScreen",
        "Turn on automatic full-screen reading in portrait orientation",
        Bool(false),
        A,
    ),
    d(
        "readingPane",
        "Reading Pane options",
        "mail.readingPane.alwaysPreview",
        "Always preview messages",
        Bool(true),
        A,
    ),
    // Mail › Stationery and Fonts... (dialog)
    d(
        "stationery",
        "New mail messages",
        "mail.compose.fontFamily",
        "Font for new messages (empty = default):",
        Text("", 128),
        A,
    ),
    d(
        "stationery",
        "New mail messages",
        "mail.compose.fontSizePt",
        "Font size for new messages in points (0 = default):",
        Int(0, 0, 72),
        A,
    ),
    // ---------------------------------------------------------------- Calendar
    d("calendar", "Work time", "calendar.workHours", "Work hours (start-end):", TimeRange("08:00-17:00"), A),
    d(
        "calendar",
        "Work time",
        "calendar.workWeek",
        "Work week:",
        Days(&["mon", "tue", "wed", "thu", "fri"]),
        A,
    ),
    d(
        "calendar",
        "Work time",
        "calendar.firstDayOfWeek",
        "First day of week:",
        Choice("sun", DAYS_CHOICE),
        A,
    ),
    d(
        "calendar",
        "Work time",
        "calendar.firstWeekOfYear",
        "First week of year:",
        Choice("jan1", FIRST_WEEK),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.onlineMeetings",
        "Add online meeting to all meetings",
        Bool(false),
        N,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.defaultDuration",
        "Default duration for new appointments and meetings (minutes):",
        Int(30, 5, 1440),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.shorten",
        "Shorten appointments and meetings",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.shortenUnderHour",
        "Shorten meetings under an hour by (minutes):",
        Int(5, 0, 30),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.shortenHourOrLonger",
        "Shorten meetings of an hour or longer by (minutes):",
        Int(10, 0, 60),
        A,
    ),
    d("calendar", "Calendar options", "calendar.defaultReminders", "Default reminders", Bool(true), A),
    d(
        "calendar",
        "Calendar options",
        "calendar.reminderMinutes",
        "Default reminder (minutes before start):",
        Int(15, 0, 20_160),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.allowProposeNewTime",
        "Allow attendees to propose new times for meetings",
        Bool(true),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.proposeResponse",
        "Use this response when proposing a new meeting time:",
        Choice("tentative", PROPOSE_RESPONSE),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.alternateCalendar",
        "Enable an alternate calendar",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.alternateCalendarType",
        "Alternate calendar:",
        Choice("hijri", ALT_CALENDARS),
        A,
    ),
    // N: MailCraft always sends meeting requests as iCalendar (RFC 5545/5546); the other choice is a
    // proprietary rich-text encoding it does not write.
    d(
        "calendar",
        "Calendar options",
        "calendar.iCalendarOutside",
        "When sending meeting requests outside of your organization, use the iCalendar format",
        Bool(true),
        N,
    ),
    d(
        "calendar",
        "Calendar options",
        "calendar.bellIcon",
        "Show bell icon on the calendar for appointments and meetings with reminders",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.defaultColor",
        "Default calendar color (#rrggbb, empty = automatic):",
        Text("", 16),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.useColorOnAll",
        "Use this color on all calendars",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.weekNumbers",
        "Show week numbers in the month view and in the Date Navigator",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.scheduleShowFree",
        "When in Schedule View, show free appointments",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.autoScheduleView",
        "Automatically switch from vertical layout to schedule view when many calendars are shown",
        Bool(true),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.autoScheduleViewAt",
        "Switch to schedule view at this many calendars:",
        Int(5, 2, 30),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.autoVerticalLayout",
        "Automatically switch from schedule view to vertical layout when few calendars are shown",
        Bool(true),
        A,
    ),
    d(
        "calendar",
        "Display options",
        "calendar.autoVerticalLayoutAt",
        "Switch to vertical layout at this many calendars:",
        Int(1, 1, 29),
        A,
    ),
    d("calendar", "Time zones", "calendar.timeZoneLabel", "Label:", Text("", 64), A),
    d("calendar", "Time zones", "calendar.timeZone", "Time zone:", Int(0, -720, 840), A),
    d("calendar", "Time zones", "calendar.secondTimeZone", "Show a second time zone", Bool(false), A),
    d("calendar", "Time zones", "calendar.secondTimeZoneLabel", "Second time zone label:", Text("", 64), A),
    d("calendar", "Time zones", "calendar.secondTimeZoneOffset", "Second time zone:", Int(0, -720, 840), A),
    d("calendar", "Time zones", "calendar.thirdTimeZone", "Show a third time zone", Bool(false), E),
    d("calendar", "Time zones", "calendar.thirdTimeZoneLabel", "Third time zone label:", Text("", 64), E),
    d("calendar", "Time zones", "calendar.thirdTimeZoneOffset", "Third time zone:", Int(0, -720, 840), E),
    d(
        "calendar",
        "Scheduling assistant",
        "calendar.screenTipDetails",
        "Show calendar details in ScreenTip",
        Bool(true),
        E,
    ),
    d(
        "calendar",
        "Scheduling assistant",
        "calendar.gridDetails",
        "Show calendar details in the scheduling grid",
        Bool(true),
        A,
    ),
    d(
        "calendar",
        "Automatic accept or decline",
        "calendar.autoAccept",
        "Automatically accept meeting requests and remove canceled meetings",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Automatic accept or decline",
        "calendar.declineConflicts",
        "Automatically decline meeting requests that conflict with an existing appointment or meeting",
        Bool(false),
        A,
    ),
    d(
        "calendar",
        "Automatic accept or decline",
        "calendar.declineRecurring",
        "Automatically decline recurring meeting requests",
        Bool(false),
        A,
    ),
    d("calendar", "Meeting Insights", "calendar.meetingInsights", "Show Meeting Insights", Bool(false), N),
    // ---------------------------------------------------------------- Groups
    d(
        "groups",
        "User Interface options",
        "groups.headerInGroupSpace",
        "Show the group header in the group space",
        Bool(true),
        N,
    ),
    d(
        "groups",
        "User Interface options",
        "groups.headerInInbox",
        "Show the group header in the Inbox",
        Bool(true),
        N,
    ),
    d("groups", "Notifications", "groups.alertLikes", "Someone Likes my messages", Bool(true), N),
    d(
        "groups",
        "Attachment options",
        "groups.attach",
        "For files I choose from my computer:",
        Choice("copy", GROUP_ATTACH),
        N,
    ),
    // ---------------------------------------------------------------- People
    d(
        "people",
        "Names and filing",
        "people.fullNameOrder",
        "Default \"Full Name\" order:",
        Choice("firstMiddleLast", FULL_NAME),
        A,
    ),
    d(
        "people",
        "Names and filing",
        "people.fileAs",
        "Default \"File As\" order:",
        Choice("lastFirst", FILE_AS),
        A,
    ),
    d(
        "people",
        "Names and filing",
        "people.checkDuplicates",
        "Check for duplicates when saving new contacts",
        Bool(true),
        A,
    ),
    d("people", "Contacts index", "people.additionalIndex", "Show an additional index", Bool(false), A),
    d(
        "people",
        "Contacts index",
        "people.additionalIndexLanguage",
        "Additional contact index:",
        Choice("arabic", INDEX_LANGUAGES),
        A,
    ),
    d(
        "people",
        "Online status and photographs",
        "people.onlineStatus",
        "Display online status next to name",
        Bool(true),
        N,
    ),
    d(
        "people",
        "Online status and photographs",
        "people.photos",
        "Show user photographs when available",
        Bool(true),
        A,
    ),
    d(
        "people",
        "Online status and photographs",
        "people.namesOnlyInPeek",
        "Show only names in the People peek",
        Bool(false),
        A,
    ),
    // ---------------------------------------------------------------- Tasks
    d("tasks", "Task options", "tasks.openInToDo", "Open tasks in the To Do app", Bool(false), N),
    d(
        "tasks",
        "Task options",
        "tasks.remindersOnDueDates",
        "Set reminders on tasks with due dates",
        Bool(false),
        A,
    ),
    d("tasks", "Task options", "tasks.reminderTime", "Default reminder time:", Time("08:00"), A),
    d(
        "tasks",
        "Task options",
        "tasks.keepCopies",
        "Keep my task list updated with copies of tasks I assign to other people",
        Bool(true),
        A,
    ),
    d(
        "tasks",
        "Task options",
        "tasks.statusReport",
        "Send status report when I complete an assigned task",
        Bool(true),
        A,
    ),
    d("tasks", "Task options", "tasks.overdueColor", "Overdue task color (#rrggbb):", Text("#c00000", 16), A),
    d(
        "tasks",
        "Task options",
        "tasks.completedColor",
        "Completed task color (#rrggbb):",
        Text("#808080", 16),
        A,
    ),
    d(
        "tasks",
        "Task options",
        "tasks.quickClickFlag",
        "Quick Click flag:",
        Choice("today", QUICK_CLICK_FLAG),
        A,
    ),
    d("tasks", "Work hours", "tasks.hoursPerDay", "Task working hours per day:", Int(8, 1, 24), E),
    d("tasks", "Work hours", "tasks.hoursPerWeek", "Task working hours per week:", Int(40, 1, 168), E),
    // ---------------------------------------------------------------- Search
    d(
        "search",
        "Results",
        "search.scope",
        "Include results only from:",
        Choice("currentFolder", SEARCH_SCOPES),
        A,
    ),
    d(
        "search",
        "Results",
        "search.includeDeleted",
        "Include messages from the Deleted Items folder in each data file when searching in All Items",
        Bool(false),
        A,
    ),
    d("search", "Results", "search.highlight", "Highlight search terms in the results", Bool(true), A),
    d(
        "search",
        "Results",
        "search.highlightColor",
        "Highlight color (#rrggbb, empty = theme):",
        Text("", 16),
        E,
    ),
    d(
        "search",
        "Results",
        "search.notifyIncomplete",
        "Notify me when results might be limited because search indexing is not complete",
        Bool(true),
        A,
    ),
    d("search", "Results", "search.relevance", "Show most relevant search results on top", Bool(true), A),
    // ---------------------------------------------------------------- Language
    d(
        "language",
        "Display language",
        "language.display",
        "Display language:",
        Choice("en-US", DISPLAY_LANGUAGES),
        S,
    ),
    d(
        "language",
        "Authoring languages and proofing",
        "language.authoring",
        "Preferred authoring language:",
        Choice("en-US", AUTHORING_LANGUAGES),
        A,
    ),
    d(
        "language",
        "Authoring languages and proofing",
        "language.cloudAuthoring",
        "Store my authoring languages in the cloud",
        Bool(false),
        N,
    ),
    d(
        "language",
        "Translation",
        "language.translate",
        "Choose how to handle messages received in other languages:",
        Choice("ask", TRANSLATE),
        // N: machine translation is a cloud service and MailCraft bundles no offline translator.
        N,
    ),
    d(
        "language",
        "Translation",
        "language.translateTo",
        "Translate messages into:",
        Choice("en", TRANSLATE_TO),
        // N: see language.translate (no offline translator).
        N,
    ),
    // ---------------------------------------------------------------- Accessibility
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnings",
        "Accessibility warnings:",
        Choice("when", ACCESS_WARNINGS),
        A,
    ),
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnPrefersAccessible",
        "At least one recipient prefers accessible content",
        Bool(true),
        // N: a recipient's accessible-content preference is published by a cloud directory service;
        // MailCraft has no source for it.
        N,
    ),
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnDarkBackground",
        "Using a dark background to compose a message",
        Bool(true),
        A,
    ),
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnLargeAudience",
        "Composing a message to a large audience",
        Bool(true),
        A,
    ),
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnOutsideOrganization",
        "At least one recipient is outside my organization",
        Bool(true),
        A,
    ),
    d(
        "accessibility",
        "Accessibility checker options",
        "accessibility.warnHighImportance",
        "Composing a message with high importance",
        Bool(true),
        A,
    ),
    // N: MailCraft has no audio output (no sound library in its dependencies), so feedback sounds cannot play.
    d(
        "accessibility",
        "Feedback options",
        "accessibility.sound",
        "Provide feedback with sound",
        Bool(false),
        N,
    ),
    // N: see accessibility.sound.
    d(
        "accessibility",
        "Feedback options",
        "accessibility.soundScheme",
        "Sound Scheme:",
        Choice("modern", SOUND_SCHEMES),
        N,
    ),
    d(
        "accessibility",
        "Application display options",
        "accessibility.readAloud",
        "Show Read Aloud",
        Bool(true),
        A,
    ),
    d(
        "accessibility",
        "Automatic Alt Text",
        "accessibility.autoAltText",
        "Automatically generate alt text for me",
        Bool(true),
        A,
    ),
    // ---------------------------------------------------------------- Advanced
    d("advanced", "MailCraft panes", "advanced.showApps", "Show Apps in MailCraft", Bool(true), N),
    v("advanced", "MailCraft panes", "view.favorites", "Show Favorites in the folder pane", Bool(true)),
    v("advanced", "MailCraft panes", "view.statusBar", "Show the status bar", Bool(true)),
    d(
        "advanced",
        "Start and exit",
        "advanced.startFolder",
        "Start MailCraft in this folder (empty = Inbox):",
        Text("", 512),
        A,
    ),
    d(
        "advanced",
        "Start and exit",
        "advanced.emptyDeletedOnExit",
        "Empty Deleted Items folders when exiting MailCraft",
        Bool(false),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.run",
        "Run AutoArchive every number of days",
        Bool(false),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.everyDays",
        "Days between AutoArchive runs:",
        Int(14, 1, 60),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.prompt",
        "Prompt before AutoArchive runs",
        Bool(false),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.deleteExpired",
        "Delete expired items (email folders only)",
        Bool(false),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.olderThanMonths",
        "Clean out items older than (months):",
        Int(6, 1, 60),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.action",
        "When cleaning out old items:",
        Choice("move", AUTO_ARCHIVE_ACTION),
        A,
    ),
    d(
        "autoArchive",
        "AutoArchive",
        "advanced.autoArchive.folder",
        "Move old items to this folder (empty = Archives):",
        Text("", 512),
        A,
    ),
    d(
        "state",
        "AutoArchive",
        "state.autoArchive.lastRun",
        "Last AutoArchive run (unix seconds, 0 = never)",
        Int(0, 0, 32_503_680_000),
        A,
    ),
    d(
        "advanced",
        "File and browser preferences",
        "advanced.openOfficeFiles",
        "Open documents attached to messages using:",
        Choice("desktop", OPEN_FILES),
        N,
    ),
    // N: links always open in the system's default browser, the only choice MailCraft has (the other
    // observed choice names one vendor's browser).
    d(
        "advanced",
        "File and browser preferences",
        "advanced.openLinksIn",
        "Open hyperlinks from MailCraft in:",
        Choice("default", OPEN_LINKS),
        N,
    ),
    d("advanced", "Reminders", "advanced.showReminders", "Show reminders", Bool(true), A),
    d("advanced", "Reminders", "advanced.reminderSound", "Play reminder sound", Bool(true), E),
    d(
        "advanced",
        "Reminders",
        "advanced.reminderSoundFile",
        "Reminder sound file:",
        Text("reminder.wav", 512),
        E,
    ),
    d(
        "advanced",
        "Reminders",
        "advanced.remindersOnTop",
        "Show reminders on top of other windows",
        Bool(false),
        A,
    ),
    d(
        "advanced",
        "Reminders",
        "advanced.dismissPastReminders",
        "Automatically dismiss reminders for past calendar events",
        Bool(false),
        A,
    ),
    // N: MailCraft has no RSS feeds.
    d(
        "advanced",
        "RSS Feeds",
        "advanced.rssUpdatedAsNew",
        "Any RSS Feed item that is updated appears as new",
        Bool(false),
        N,
    ),
    d(
        "advanced",
        "RSS Feeds",
        "advanced.rssCommonFeedList",
        "Synchronize RSS Feeds to the system's common feed list",
        Bool(false),
        N,
    ),
    d(
        "advanced",
        "Send and receive",
        "advanced.sendImmediately",
        "Send immediately when connected",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "Send and receive",
        "advanced.sendReceiveMinutes",
        "Schedule an automatic send/receive every (minutes, 0 = off):",
        Int(crate::sync::DEFAULT_AUTO_MINUTES as i64, 0, 1440),
        A,
    ),
    d(
        "advanced",
        "Send and receive",
        "advanced.sendReceiveOnExit",
        "Perform an automatic send/receive when exiting",
        Bool(false),
        A,
    ),
    d("advanced", "Developers", "advanced.addInErrors", "Show add-in user interface errors", Bool(false), N),
    d(
        "advanced",
        "International options",
        "advanced.englishFlagLabels",
        "Use English for message flag labels",
        Bool(false),
        S,
    ),
    d(
        "advanced",
        "International options",
        "advanced.englishHeaders",
        "Use English for message headers on replies and forwards and for forward notifications",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.autoEncoding",
        "Automatically select encoding for outgoing messages",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.encoding",
        "Preferred encoding for outgoing messages:",
        Choice("utf-8", ENCODINGS),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.autoVcardEncoding",
        "Automatically select encoding for outgoing vCards",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.vcardEncoding",
        "Preferred encoding for outgoing vCards:",
        Choice("utf-8", ENCODINGS),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.idn",
        "Allow international domain names in email addresses",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "International options",
        "advanced.mailtoUtf8",
        "Allow UTF-8 support for the mailto: protocol",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "Other",
        "advanced.quickClickCategory",
        "Quick Click category (empty = the first category):",
        Text("", 255),
        A,
    ),
    d(
        "advanced",
        "Other",
        "advanced.confirmPermanentDelete",
        "Prompt for confirmation before permanently deleting items",
        Bool(true),
        A,
    ),
    d(
        "advanced",
        "Other",
        "advanced.sentMailAnalysis",
        "Allow analysis of sent emails to identify people you commonly email",
        Bool(false),
        N,
    ),
    d(
        "advanced",
        "Other",
        "advanced.logging",
        "Enable troubleshooting logging (requires restarting MailCraft)",
        Bool(false),
        A,
    ),
    d(
        "advanced",
        "Other",
        "advanced.animations",
        "Use animations when expanding conversations and groups",
        Bool(true),
        A,
    ),
    // ---------------------------------------------------------------- Customize Ribbon
    v(
        "customizeRibbon",
        "Ribbon",
        "view.simplifiedRibbon",
        "Use the Simplified Ribbon (off: Classic Ribbon)",
        Bool(true),
    ),
    v(
        "customizeRibbon",
        "Ribbon",
        "view.ribbonCollapsed",
        "Show tabs only (collapse the ribbon)",
        Bool(false),
    ),
    // ---------------------------------------------------------------- Quick Access Toolbar
    d("quickAccessToolbar", "Quick Access Toolbar", "qat.show", "Show Quick Access Toolbar", Bool(true), A),
    d(
        "quickAccessToolbar",
        "Toolbar Position",
        "qat.position",
        "Toolbar Position:",
        Choice("above", QAT_POSITION),
        A,
    ),
    d(
        "quickAccessToolbar",
        "Toolbar Position",
        "qat.showLabels",
        "Always show command labels",
        Bool(false),
        A,
    ),
    // ---------------------------------------------------------------- Trust Center
    d(
        "trustCenter",
        "Security & more",
        "trust.blockImages",
        "Don't download pictures automatically in HTML email messages",
        Bool(true),
        A,
    ),
    d(
        "trustCenter",
        "Security & more",
        "trust.plainText",
        "Read all standard mail in plain text",
        Bool(false),
        A,
    ),
    d(
        "trustCenter",
        "Security & more",
        "trust.warnLinks",
        "Ask before opening web links in messages",
        Bool(false),
        A,
    ),
    // Email Security: S/MIME defaults for new messages (smime.rs). Labels are MailCraft's own.
    d(
        "trustCenter",
        "Email Security",
        "trust.smime.encrypt",
        "Encrypt the contents and attachments of messages I send",
        Bool(false),
        A,
    ),
    d("trustCenter", "Email Security", "trust.smime.sign", "Sign messages I send digitally", Bool(false), A),
    d(
        "trustCenter",
        "Email Security",
        "trust.smime.clearSigned",
        "Send signed messages as clear text, readable without S/MIME",
        Bool(true),
        A,
    ),
    // ---------------------------------------------------------------- UI state (not in Options)
    d("state", "Panes", "state.folderPaneWidth", "Folder pane width (0 = default)", Int(0, 0, 4000), A),
    d("state", "Panes", "state.listWidth", "Message list width (0 = default)", Int(0, 0, 8000), A),
    d(
        "state",
        "Panes",
        "state.listFractionPermille",
        "Message list share with the reading pane at the bottom (0 = default)",
        Int(0, 0, 1000),
        A,
    ),
    d("state", "Files", "state.recentFiles", "Files recently opened or saved (newest first)", List(50), E),
    d("state", "Print", "state.printSetup", "Print page setup per print style", Object(64 * 1024), E),
    d(
        "state",
        "Focused Inbox",
        "mail.focused.otherSenders",
        "Senders whose mail always goes to Other (Always Move to Other)",
        List(1000),
        A,
    ),
    d("state", "Folder pane", "state.showAllFoldersAZ", "Show all folders A to Z", Bool(false), A),
    d(
        "state",
        "Dialogs",
        "state.uiPrefs",
        "Settings kept by dialogs (columns, folder descriptions…)",
        Object(1024 * 1024),
        A,
    ),
];

/// Looks up a setting.
pub fn def(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// Settings shown on an Options page (its own, then those [`ALSO_ON`] it), in order.
pub fn page_defs(page: &str) -> Vec<(&'static str, &'static Def)> {
    let mut out: Vec<(&'static str, &'static Def)> =
        DEFS.iter().filter(|d| d.page == page).map(|d| (d.section, d)).collect();
    for (p, section, key) in ALSO_ON {
        if p == page
            && let Some(d) = def(key)
        {
            out.push((section, d));
        }
    }
    out
}

/// (applied, total) over the settings of the Options window and its dialogs (UI state not counted).
pub fn effect_counts() -> (usize, usize) {
    let counted = DEFS.iter().filter(|d| d.page != "state");
    let total = counted.clone().count();
    let applied = counted.filter(|d| d.effect == Effect::Applied).count();
    (applied, total)
}

// ---------------------------------------------------------------------------------------------------------
// Validation

fn choice_id(choices: &'static [(&'static str, &'static str)], v: &Value) -> Option<&'static str> {
    if let Some(i) = v.as_u64() {
        return usize::try_from(i).ok().and_then(|i| choices.get(i)).map(|(id, _)| *id);
    }
    let s = v.as_str()?.trim();
    choices
        .iter()
        .find(|(id, label)| id.eq_ignore_ascii_case(s) || label.eq_ignore_ascii_case(s))
        .map(|(id, _)| *id)
}

/// "H:MM", "HH:MM", optionally with AM/PM → minutes after midnight.
pub fn parse_time(text: &str) -> Option<u32> {
    let t = text.trim().to_ascii_lowercase();
    let (t, pm) = if let Some(r) = t.strip_suffix("pm") {
        (r.trim(), Some(true))
    } else if let Some(r) = t.strip_suffix("am") {
        (r.trim(), Some(false))
    } else {
        (t.as_str(), None)
    };
    let (h, m) = t.split_once(':').unwrap_or((t, "0"));
    if h.is_empty() || h.len() > 2 || m.len() > 2 || m.is_empty() {
        return None;
    }
    let mut h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if m > 59 {
        return None;
    }
    match pm {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h %= 12;
            if pm {
                h += 12;
            }
        }
        None if h > 23 => return None,
        None => {}
    }
    Some(h * 60 + m)
}

fn fmt_time(min: u32) -> String {
    format!("{:02}:{:02}", (min / 60) % 24, min % 60)
}

/// "HH:MM-HH:MM" → (start, end) minutes, start before end.
pub fn parse_time_range(text: &str) -> Option<(u32, u32)> {
    let (a, b) = text.split_once('-')?;
    let (a, b) = (parse_time(a)?, parse_time(b)?);
    (a < b).then_some((a, b))
}

fn want_err(def: &Def, what: &str) -> CmdError {
    CmdError::Params(format!("`{}` takes {what}", def.key))
}

/// Validates and normalises `v` for `def`.
pub fn normalize(def: &Def, v: &Value) -> Result<Value, CmdError> {
    match def.kind {
        Kind::Bool(_) => match v {
            Value::Bool(b) => Ok(json!(b)),
            Value::String(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("on") => {
                Ok(json!(true))
            }
            Value::String(s) if s.eq_ignore_ascii_case("false") || s.eq_ignore_ascii_case("off") => {
                Ok(json!(false))
            }
            _ => Err(want_err(def, "true/false")),
        },
        Kind::Int(_, min, max) => {
            let n = match v {
                Value::Number(n) => n
                    .as_i64()
                    .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15).map(|f| f as i64)),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
            match n {
                Some(n) if (min..=max).contains(&n) => Ok(json!(n)),
                _ => Err(want_err(def, &format!("a whole number from {min} to {max}"))),
            }
        }
        Kind::Choice(_, choices) => choice_id(choices, v).map(|id| json!(id)).ok_or_else(|| {
            let ids: Vec<&str> = choices.iter().map(|(id, _)| *id).collect();
            want_err(def, &format!("one of {}", ids.join(", ")))
        }),
        Kind::Text(_, max) => {
            let s = v.as_str().ok_or_else(|| want_err(def, "a string"))?;
            if s.chars().count() > max || s.chars().any(|c| c.is_control()) {
                return Err(want_err(def, &format!("a single line of at most {max} characters")));
            }
            Ok(json!(s))
        }
        Kind::Time(_) => v
            .as_str()
            .and_then(parse_time)
            .map(|m| json!(fmt_time(m)))
            .ok_or_else(|| want_err(def, "a time of day like \"08:00\"")),
        Kind::TimeRange(_) => v
            .as_str()
            .and_then(parse_time_range)
            .map(|(a, b)| json!(format!("{}-{}", fmt_time(a), fmt_time(b))))
            .ok_or_else(|| want_err(def, "a range like \"08:00-17:00\" (start before end)")),
        Kind::Days(_) => {
            let items: Vec<Value> = match v {
                Value::Array(a) => a.clone(),
                Value::String(s) => s.split(',').map(|p| json!(p.trim())).collect(),
                _ => return Err(want_err(def, "a list of weekdays")),
            };
            if items.len() > 7 {
                return Err(want_err(def, "at most seven weekdays"));
            }
            let mut on = [false; 7];
            for it in &items {
                let id =
                    choice_id(&WEEKDAYS, it).ok_or_else(|| want_err(def, "weekdays (sun, mon, …, sat)"))?;
                if let Some(i) = WEEKDAYS.iter().position(|(d, _)| *d == id)
                    && let Some(slot) = on.get_mut(i)
                {
                    *slot = true;
                }
            }
            let days: Vec<&str> = WEEKDAYS.iter().zip(on).filter(|(_, o)| *o).map(|((d, _), _)| *d).collect();
            if days.is_empty() {
                return Err(want_err(def, "at least one weekday"));
            }
            Ok(json!(days))
        }
        Kind::List(max) => {
            let a = v.as_array().ok_or_else(|| want_err(def, "a list of strings"))?;
            if a.len() > max {
                return Err(want_err(def, &format!("at most {max} entries")));
            }
            let mut out: Vec<String> = Vec::new();
            for it in a {
                let s = it.as_str().ok_or_else(|| want_err(def, "a list of strings"))?.trim();
                if s.chars().count() > 1024 || s.chars().any(|c| c.is_control()) {
                    return Err(want_err(def, "entries of at most 1024 characters on one line"));
                }
                if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
                    out.push(s.to_owned());
                }
            }
            Ok(json!(out))
        }
        Kind::Object(max) => {
            if !v.is_object() {
                return Err(want_err(def, "an object"));
            }
            let size = serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX);
            if size > max {
                return Err(want_err(def, &format!("an object of at most {max} bytes")));
            }
            Ok(v.clone())
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// The store

/// Setting values that differ from their defaults.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    values: BTreeMap<String, Value>,
    rev: u64,
}

impl Settings {
    /// Revision counter, bumped by every change (lets the UI notice changes cheaply).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Values that differ from the defaults.
    pub fn overrides(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// Current value of a store-backed setting (its default when unset); `None` for unknown or view-backed
    /// keys.
    pub fn get(&self, key: &str) -> Option<Value> {
        let def = def(key).filter(|d| !d.view)?;
        Some(self.values.get(key).cloned().unwrap_or_else(|| def.default_value()))
    }

    /// Sets a store-backed setting (validated); returns the normalised value. Setting the default removes the
    /// override.
    pub fn set(&mut self, key: &str, v: &Value) -> Result<Value, CmdError> {
        let def = def(key).ok_or_else(|| CmdError::Params(format!("unknown option `{}`", short(key))))?;
        if def.view {
            return Err(CmdError::Params(format!("`{key}` is held by the session view state")));
        }
        let norm = normalize(def, v)?;
        let changed = if norm == def.default_value() {
            self.values.remove(key).is_some()
        } else {
            self.values.insert(key.to_owned(), norm.clone()).as_ref() != Some(&norm)
        };
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
        Ok(norm)
    }

    /// Back to the default.
    pub fn reset(&mut self, key: &str) -> Result<(), CmdError> {
        if def(key).is_none() {
            return Err(CmdError::Params(format!("unknown option `{}`", short(key))));
        }
        if self.values.remove(key).is_some() {
            self.rev = self.rev.wrapping_add(1);
        }
        Ok(())
    }

    /// Every setting back to its default.
    pub fn reset_all(&mut self) {
        if !self.values.is_empty() {
            self.values.clear();
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// Loads overrides from JSON, keeping only known keys with valid values; returns what was dropped.
    pub fn from_json(v: &Value) -> (Settings, Vec<String>) {
        let mut s = Settings::default();
        let mut dropped = Vec::new();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Err(e) = s.set(k, val) {
                    dropped.push(format!("{}: {e}", short(k)));
                }
            }
        }
        s.rev = 0;
        (s, dropped)
    }

    /// Overrides as a JSON object.
    pub fn to_json(&self) -> Value {
        Value::Object(self.values.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Map<String, Value>>())
    }

    // -------------------------------------------------------------------------------- typed getters

    /// A boolean setting (false for unknown keys).
    pub fn bool(&self, key: &str) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
    }

    /// A whole-number setting (0 for unknown keys).
    pub fn int(&self, key: &str) -> i64 {
        self.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
    }

    /// A text / choice / time setting ("" for unknown keys).
    pub fn text(&self, key: &str) -> String {
        self.get(key).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }

    /// A list / weekday-set setting.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .and_then(|v| {
                v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            })
            .unwrap_or_default()
    }

    /// Calendar › Work week: working days, index 0 = Sunday (`calendar.workWeek`).
    pub fn work_week(&self) -> [bool; 7] {
        let days = self.list("calendar.workWeek");
        let mut out = [false; 7];
        for (slot, (id, _)) in out.iter_mut().zip(WEEKDAYS) {
            *slot = days.iter().any(|d| d == id);
        }
        out
    }

    /// Calendar › Work hours as minutes after midnight, start before end (`calendar.workHours`).
    pub fn work_hours(&self) -> (u32, u32) {
        parse_time_range(&self.text("calendar.workHours")).unwrap_or((8 * 60, 17 * 60))
    }

    /// Calendar › First day of week, days from Sunday (`calendar.firstDayOfWeek`).
    pub fn first_day_of_week(&self) -> u8 {
        let id = self.text("calendar.firstDayOfWeek");
        WEEKDAYS.iter().position(|(d, _)| *d == id).and_then(|i| u8::try_from(i).ok()).unwrap_or(0)
    }

    /// Calendar › Default reminder in minutes, `None` when default reminders are off.
    pub fn default_reminder_minutes(&self) -> Option<u32> {
        self.bool("calendar.defaultReminders")
            .then(|| u32::try_from(self.int("calendar.reminderMinutes")).unwrap_or(15))
    }

    /// Calendar › Default duration of new appointments and meetings, minutes.
    pub fn default_duration_minutes(&self) -> u32 {
        u32::try_from(self.int("calendar.defaultDuration")).unwrap_or(30)
    }

    /// Calendar › Time zone as a UTC offset in minutes (`calendar.timeZone`).
    pub fn time_zone_offset_min(&self) -> i32 {
        i32::try_from(self.int("calendar.timeZone")).unwrap_or(0)
    }

    /// People › Default "File As" order id (`lastFirst`, `firstLast`, `company`, `lastFirstCompany`,
    /// `companyLastFirst`).
    pub fn file_as(&self) -> String {
        self.text("people.fileAs")
    }

    /// People › Default "Full Name" order id (`firstMiddleLast`, `lastFirst`, `firstLast1Last2`).
    pub fn full_name_order(&self) -> String {
        self.text("people.fullNameOrder")
    }

    /// Tasks › Default reminder time, minutes after midnight (`tasks.reminderTime`).
    pub fn task_reminder_minutes(&self) -> u32 {
        parse_time(&self.text("tasks.reminderTime")).unwrap_or(8 * 60)
    }

    /// Tasks › Set reminders on tasks with due dates.
    pub fn task_reminders_on_due(&self) -> bool {
        self.bool("tasks.remindersOnDueDates")
    }

    /// Mail › Compose messages in this format.
    pub fn compose_format(&self) -> mailcraft_model::BodyFormat {
        match self.text("mail.compose.defaultFormat").as_str() {
            "plain" => mailcraft_model::BodyFormat::PlainText,
            "rich" => mailcraft_model::BodyFormat::RichText,
            _ => mailcraft_model::BodyFormat::Html,
        }
    }

    /// Mail › Default Importance level.
    pub fn default_importance(&self) -> mailcraft_model::Importance {
        match self.text("mail.send.defaultImportance").as_str() {
            "low" => mailcraft_model::Importance::Low,
            "high" => mailcraft_model::Importance::High,
            _ => mailcraft_model::Importance::Normal,
        }
    }

    /// Mail › Default Sensitivity level.
    pub fn default_sensitivity(&self) -> mailcraft_model::Sensitivity {
        match self.text("mail.send.defaultSensitivity").as_str() {
            "personal" => mailcraft_model::Sensitivity::Personal,
            "private" => mailcraft_model::Sensitivity::Private,
            "confidential" => mailcraft_model::Sensitivity::Confidential,
            _ => mailcraft_model::Sensitivity::Normal,
        }
    }

    /// Mail › Replies and forwards › "When replying to a message" (`forward = false`) / "When forwarding a
    /// message" (`forward = true`).
    pub fn original_text(&self, forward: bool) -> OriginalText {
        let key = if forward { "mail.reply.forwardText" } else { "mail.reply.replyText" };
        match self.text(key).as_str() {
            "none" if !forward => OriginalText::Omit,
            "attach" => OriginalText::Attach,
            "indent" => OriginalText::Indent,
            "prefix" => OriginalText::Prefix,
            _ => OriginalText::Include,
        }
    }

    /// Mail › Replies and forwards › "Preface each line in a plain-text message with" (default "> ").
    pub fn plain_prefix(&self) -> String {
        self.text("mail.reply.plainPrefix")
    }

    /// Mail › Replies and forwards › "Preface comments with": `None` when off; otherwise the name typed there,
    /// else General › User name, else `Some("")` (the caller then uses the account's name).
    pub fn preface_name(&self) -> Option<String> {
        if !self.bool("mail.reply.prefaceComments") {
            return None;
        }
        let name = self.text("mail.reply.prefaceName");
        let name = if name.trim().is_empty() { self.text("general.userName") } else { name };
        Some(name.trim().to_owned())
    }

    /// Mail › Save messages › "Automatically save items that have not been sent after this many minutes":
    /// the period in minutes, `None` when off.
    pub fn auto_save_minutes(&self) -> Option<u32> {
        self.bool("mail.save.autoSave")
            .then(|| u32::try_from(self.int("mail.save.autoSaveMinutes")).unwrap_or(3).max(1))
    }

    /// Mail › Save messages › "Save to this folder": the folder unsent items are saved to.
    pub fn draft_folder_role(&self) -> mailcraft_model::FolderRole {
        match self.text("mail.save.folder").as_str() {
            "inbox" => mailcraft_model::FolderRole::Inbox,
            "sentItems" => mailcraft_model::FolderRole::SentItems,
            "outbox" => mailcraft_model::FolderRole::Outbox,
            _ => mailcraft_model::FolderRole::Drafts,
        }
    }

    /// Mail › Send messages › "Mark messages as expired after this many days": the days, `None` when off.
    pub fn expire_days(&self) -> Option<u32> {
        self.bool("mail.send.expire").then(|| u32::try_from(self.int("mail.send.expireDays")).unwrap_or(0))
    }

    /// Mail › Message format › "Automatically wrap text at character" (plain-text messages).
    pub fn wrap_at(&self) -> usize {
        usize::try_from(self.int("mail.format.wrapAt")).unwrap_or(76).max(1)
    }

    /// Mail › Conversation Clean Up options.
    pub fn clean_up(&self) -> CleanUpOptions {
        CleanUpOptions {
            folder: self.text("mail.cleanup.folder").trim().to_owned(),
            recreate_hierarchy: self.bool("mail.cleanup.recreateHierarchy"),
            keep_unread: self.bool("mail.cleanup.keepUnread"),
            keep_categorized: self.bool("mail.cleanup.keepCategorized"),
            keep_flagged: self.bool("mail.cleanup.keepFlagged"),
            keep_signed: self.bool("mail.cleanup.keepSigned"),
            keep_original: self.bool("mail.cleanup.keepOriginal"),
        }
    }
}

/// What a reply or forward does with the original message (Options › Mail › Replies and forwards).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalText {
    /// Include original message text (below a header block).
    Include,
    /// Do not include original message (replies only).
    Omit,
    /// Attach original message (as an .eml item).
    Attach,
    /// Include and indent original message text.
    Indent,
    /// Prefix each line of the original message (the plain-text prefix; a quote bar in HTML).
    Prefix,
}

impl OriginalText {
    /// Whether the original's text goes into the body.
    pub fn quotes(self) -> bool {
        matches!(self, OriginalText::Include | OriginalText::Indent | OriginalText::Prefix)
    }
}

/// Conversation Clean Up options (Options › Mail): where cleaned-up items go and which are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanUpOptions {
    /// Folder path for cleaned-up items; empty = the account's Deleted Items.
    pub folder: String,
    pub recreate_hierarchy: bool,
    pub keep_unread: bool,
    pub keep_categorized: bool,
    pub keep_flagged: bool,
    pub keep_signed: bool,
    /// When a reply modifies a message, don't move the original.
    pub keep_original: bool,
}

impl CleanUpOptions {
    /// Whether these options keep `m` in place whatever its conversation holds (unread, categorized,
    /// flagged or digitally signed, as ticked). `signed` says whether `m` carries a signature part.
    pub fn keeps(&self, m: &mailcraft_model::MailItem, signed: bool) -> bool {
        (self.keep_unread && !m.read)
            || (self.keep_categorized && !m.categories.is_empty())
            || (self.keep_flagged && m.flag != mailcraft_model::FlagState::None)
            || (self.keep_signed && signed)
    }
}

fn short(key: &str) -> String {
    key.chars().take(80).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Missing attachment check

/// Whether `text` mentions an attachment ("attach", "attached", "attachment"…), as a whole word start.
pub fn mentions_attachment(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut prev_alpha = false;
    for (i, c) in lower.char_indices() {
        if !prev_alpha && c == 'a' && lower.get(i..).is_some_and(|r| r.starts_with("attach")) {
            return true;
        }
        prev_alpha = c.is_alphanumeric();
    }
    false
}

/// The part of a reply/forward body the sender wrote (text above the quoted original).
pub fn new_text_of_reply(body: &str) -> &str {
    let mut end = body.len();
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let t = line.trim_start();
        if crate::intl::is_header_start(t)
            || crate::intl::is_original_separator(t)
            || t.starts_with("-----Original Message")
            || t.starts_with('>')
            || (t.starts_with("On ") && t.trim_end().ends_with("wrote:"))
        {
            end = offset;
            break;
        }
        offset = offset.saturating_add(line.len());
    }
    body.get(..end).unwrap_or(body)
}

// ---------------------------------------------------------------------------------------------------------
// settings.json

/// File name of the settings file in the profile folder.
pub const FILE_NAME: &str = "settings.json";
/// Largest settings file read (anything bigger is refused, defaults are used).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const VERSION: u32 = 1;

/// The settings file: options, persisted view state and recent searches.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsFile {
    pub version: u32,
    pub settings: Value,
    pub view: Value,
    #[serde(rename = "recentSearches")]
    pub recent_searches: Vec<String>,
}

impl SettingsFile {
    pub fn new(settings: &Settings, view: Value, recent: &[String]) -> SettingsFile {
        SettingsFile {
            version: VERSION,
            settings: settings.to_json(),
            view,
            recent_searches: recent.to_vec(),
        }
    }

    /// Reads `dir/settings.json`: `Ok(None)` when it does not exist.
    pub fn read(dir: &Path) -> Result<Option<SettingsFile>, String> {
        let path = dir.join(FILE_NAME);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("{} is larger than {} bytes", path.display(), MAX_FILE_BYTES));
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        serde_json::from_slice::<SettingsFile>(&bytes).map(Some).map_err(|e| e.to_string())
    }

    /// Serialised file contents.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec_pretty(self).map_err(|e| e.to_string())
    }

    /// Writes `bytes` to `dir/settings.json` atomically (temporary file, flush, rename).
    pub fn write_bytes(dir: &Path, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let path = dir.join(FILE_NAME);
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        let res = (|| -> std::io::Result<()> {
            {
                // Closed at the end of this block, before the rename.
                let mut f = std::fs::File::create(&tmp)?;
                f.write_all(bytes)?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp, &path)
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res.map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_defaults_valid() {
        let mut seen = std::collections::BTreeSet::new();
        for d in DEFS {
            assert!(seen.insert(d.key), "duplicate key {}", d.key);
            assert!(
                d.page == "state"
                    || d.page == "readingPane"
                    || d.page == "autoArchive"
                    || d.page == "stationery"
                    || PAGES.iter().any(|(p, _)| *p == d.page),
                "{}: unknown page {}",
                d.key,
                d.page
            );
            assert_eq!(normalize(d, &d.default_value()).unwrap(), d.default_value(), "{}", d.key);
        }
        for (page, _, key) in ALSO_ON {
            assert!(def(key).is_some() && PAGES.iter().any(|(p, _)| *p == page));
        }
        // Every Options page has settings or (Add-ins) a note in the window.
        for (p, _) in PAGES {
            assert!(p == "addIns" || !page_defs(p).is_empty(), "{p} has no settings");
        }
        let (applied, total) = effect_counts();
        assert!(total >= 200, "{total}");
        assert!(applied >= 110, "{applied}");
    }

    #[test]
    fn set_validates_and_normalises() {
        let mut s = Settings::default();
        assert_eq!(s.get("mail.send.ctrlEnterSends"), Some(json!(true)));
        assert_eq!(s.set("mail.send.ctrlEnterSends", &json!(false)).unwrap(), json!(false));
        assert!(!s.bool("mail.send.ctrlEnterSends"));
        assert_eq!(s.overrides().len(), 1);
        // Back to the default drops the override.
        s.set("mail.send.ctrlEnterSends", &json!("on")).unwrap();
        assert!(s.overrides().is_empty());
        assert!(s.set("mail.readingPane.markReadAfterSeconds", &json!(1000)).is_err());
        assert!(s.set("mail.readingPane.markReadAfterSeconds", &json!(-1)).is_err());
        assert_eq!(s.set("mail.readingPane.markReadAfterSeconds", &json!("12")).unwrap(), json!(12));
        assert_eq!(s.set("mail.compose.defaultFormat", &json!("Plain Text")).unwrap(), json!("plain"));
        assert_eq!(s.set("mail.compose.defaultFormat", &json!(1)).unwrap(), json!("rich"));
        assert!(s.set("mail.compose.defaultFormat", &json!(9)).is_err());
        assert!(s.set("nope.nope", &json!(1)).is_err());
        assert!(s.set("view.statusBar", &json!(true)).is_err());
        assert!(s.set("general.userName", &json!("a\u{0}b")).is_err());
        assert!(s.set("general.userName", &json!("x".repeat(129))).is_err());
        assert_eq!(s.set("tasks.reminderTime", &json!("9:30 PM")).unwrap(), json!("21:30"));
        assert_eq!(s.task_reminder_minutes(), 21 * 60 + 30);
        assert!(s.set("tasks.reminderTime", &json!("25:00")).is_err());
        assert_eq!(s.set("calendar.workHours", &json!("9:00-17:30")).unwrap(), json!("09:00-17:30"));
        assert_eq!(s.work_hours(), (540, 1050));
        assert!(s.set("calendar.workHours", &json!("17:00-09:00")).is_err());
        assert_eq!(
            s.set("calendar.workWeek", &json!(["Sunday", "sat", "mon", "mon"])).unwrap(),
            json!(["sun", "mon", "sat"])
        );
        assert_eq!(s.work_week(), [true, true, false, false, false, false, true]);
        assert!(s.set("calendar.workWeek", &json!([])).is_err());
        assert!(s.set("calendar.workWeek", &json!(vec!["sun"; 8])).is_err());
        assert_eq!(s.set("state.recentFiles", &json!([" a.eml", "A.eml", ""])).unwrap(), json!(["a.eml"]));
        assert!(s.set("state.recentFiles", &json!([1])).is_err());
        assert!(s.set("state.printSetup", &json!([1])).is_err());
        assert!(s.set("state.printSetup", &json!({"x": "y".repeat(70_000)})).is_err());
        let rev = s.rev();
        s.set("tasks.reminderTime", &json!("21:30")).unwrap();
        assert_eq!(s.rev(), rev, "same value: no change");
        s.reset("tasks.reminderTime").unwrap();
        assert_eq!(s.task_reminder_minutes(), 480);
        assert!(s.rev() > rev);
        s.reset_all();
        assert!(s.overrides().is_empty());
    }

    #[test]
    fn focused_inbox_senders_and_folder_order_are_kept() {
        let mut s = Settings::default();
        assert!(s.list("mail.focused.otherSenders").is_empty());
        assert_eq!(
            s.set(
                "mail.focused.otherSenders",
                &json!(["ann@example.com", "ANN@example.com", " bo@example.org"])
            )
            .unwrap(),
            json!(["ann@example.com", "bo@example.org"])
        );
        let too_many: Vec<String> = (0..1001).map(|i| format!("p{i}@example.com")).collect();
        assert!(s.set("mail.focused.otherSenders", &json!(too_many)).is_err());
        assert!(s.set("mail.focused.otherSenders", &json!(["a\nb@example.com"])).is_err());
        assert!(!s.bool("state.showAllFoldersAZ"));
        s.set("state.showAllFoldersAZ", &json!(true)).unwrap();
        assert!(s.bool("state.showAllFoldersAZ"));
        // UI state: not shown in Options, not counted.
        for key in ["mail.focused.otherSenders", "state.showAllFoldersAZ"] {
            assert_eq!(def(key).map(|d| d.page), Some("state"));
        }
        let (back, dropped) = Settings::from_json(&s.to_json());
        assert!(dropped.is_empty());
        assert_eq!(back.list("mail.focused.otherSenders").len(), 2);
    }

    #[test]
    fn json_round_trip_drops_junk() {
        let mut s = Settings::default();
        s.set("people.fileAs", &json!("company")).unwrap();
        s.set("calendar.firstDayOfWeek", &json!("mon")).unwrap();
        let mut j = s.to_json();
        j.as_object_mut().unwrap().insert("bogus.key".into(), json!(1));
        j.as_object_mut().unwrap().insert("mail.format.wrapAt".into(), json!("wide"));
        let (back, dropped) = Settings::from_json(&j);
        assert_eq!(back.file_as(), "company");
        assert_eq!(back.first_day_of_week(), 1);
        assert_eq!(dropped.len(), 2);
        assert_eq!(Settings::from_json(&json!("garbage")).0, Settings::default());
    }

    #[test]
    fn times_and_attachment_words() {
        assert_eq!(parse_time("08:00"), Some(480));
        assert_eq!(parse_time("12:00 AM"), Some(0));
        assert_eq!(parse_time("12:15 pm"), Some(735));
        assert_eq!(parse_time("7"), Some(420));
        for bad in ["", ":", "1:2:3", "99:00", "10:60", "13:00 pm", "x", "123:00", "\u{1F600}"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert!(mentions_attachment("Please see the Attached report"));
        assert!(mentions_attachment("attachment"));
        assert!(!mentions_attachment("reattach nothing"));
        assert!(!mentions_attachment("no files here"));
        assert_eq!(new_text_of_reply("Thanks!\nFrom: Ann\nSee attached"), "Thanks!\n");
        assert_eq!(new_text_of_reply("Hi\n> attached"), "Hi\n");
        assert_eq!(new_text_of_reply("Only new text"), "Only new text");
    }

    #[test]
    fn file_round_trip_and_atomic_write() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        assert_eq!(SettingsFile::read(dir.path()).unwrap(), None);
        let mut s = Settings::default();
        s.set("search.highlight", &json!(false)).unwrap();
        let f = SettingsFile::new(&s, json!({"zoom": 120}), &["invoice".to_owned()]);
        SettingsFile::write_bytes(dir.path(), &f.to_bytes().unwrap()).unwrap();
        let back = SettingsFile::read(dir.path()).unwrap().unwrap();
        assert_eq!(back, f);
        assert!(!dir.path().join("settings.json.tmp").exists());
        std::fs::write(dir.path().join(FILE_NAME), b"{not json").unwrap();
        assert!(SettingsFile::read(dir.path()).is_err());
    }

    #[test]
    fn mail_option_getters() {
        let mut s = Settings::default();
        assert_eq!(
            (s.original_text(false), s.original_text(true)),
            (OriginalText::Include, OriginalText::Include)
        );
        s.set("mail.reply.replyText", &json!("none")).unwrap();
        s.set("mail.reply.forwardText", &json!("prefix")).unwrap();
        assert_eq!(
            (s.original_text(false), s.original_text(true)),
            (OriginalText::Omit, OriginalText::Prefix)
        );
        assert!(
            !OriginalText::Omit.quotes() && !OriginalText::Attach.quotes() && OriginalText::Indent.quotes()
        );
        assert_eq!(s.plain_prefix(), "> ");
        assert_eq!(s.preface_name(), None);
        s.set("mail.reply.prefaceComments", &json!(true)).unwrap();
        assert_eq!(s.preface_name().as_deref(), Some(""));
        s.set("mail.reply.prefaceName", &json!("  Kim ")).unwrap();
        assert_eq!(s.preface_name().as_deref(), Some("Kim"));
        assert_eq!(s.auto_save_minutes(), Some(3));
        s.set("mail.save.autoSave", &json!(false)).unwrap();
        assert_eq!(s.auto_save_minutes(), None);
        assert_eq!(s.draft_folder_role(), mailcraft_model::FolderRole::Drafts);
        s.set("mail.save.folder", &json!("sentItems")).unwrap();
        assert_eq!(s.draft_folder_role(), mailcraft_model::FolderRole::SentItems);
        assert_eq!(s.expire_days(), None);
        s.set("mail.send.expire", &json!(true)).unwrap();
        s.set("mail.send.expireDays", &json!(7)).unwrap();
        assert_eq!(s.expire_days(), Some(7));
        assert_eq!(s.wrap_at(), 76);
        assert!(s.set("mail.format.wrapAt", &json!(5)).is_err(), "below the range");
    }

    #[test]
    fn clean_up_options_keep_what_is_ticked() {
        let mut s = Settings::default();
        let o = s.clean_up();
        assert!(
            o.folder.is_empty() && !o.keep_unread && o.keep_categorized && o.keep_flagged && o.keep_signed
        );
        let mut m = mailcraft_model::MailItem { read: true, ..Default::default() };
        assert!(!o.keeps(&m, false));
        assert!(o.keeps(&m, true), "signed");
        m.categories = vec!["Red".into()];
        assert!(o.keeps(&m, false));
        m.categories.clear();
        m.read = false;
        assert!(!o.keeps(&m, false));
        s.set("mail.cleanup.keepUnread", &json!(true)).unwrap();
        s.set("mail.cleanup.folder", &json!("Archive/Old")).unwrap();
        let o = s.clean_up();
        assert!(o.keeps(&m, false));
        assert_eq!(o.folder, "Archive/Old");
    }
}
