//! Read-only views the UI renders (and the CLI/MCP report). Pure functions of the session.

use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::cmd::view::{conversation_state, is_group_label};
use crate::{Module, Session};
use mailcraft_model::view::GROUP_SEP;
use mailcraft_model::{
    ArrangeBy, Category, DateGroup, DateTime, FlagState, FocusClass, FolderId, FolderKind, FolderRole,
    Importance, ItemId, ListFilter, ListTime, MailItem, ResponseState, SortOrder, ViewSettings,
    subject::{conversation_topic, strip_prefixes},
    thread::{ConversationSettings, norm_id, reply_tree},
};

/// One row of the folder pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderRow {
    pub id: Option<FolderId>,
    /// Section header rows: "Favorites", account roots (address).
    pub kind: FolderRowKind,
    pub name: String,
    pub depth: u32,
    pub has_children: bool,
    pub expanded: bool,
    /// Badge text ("778", "[8]") or empty.
    pub badge: String,
    /// Badge is a total count (brackets) rather than unread.
    pub badge_total: bool,
    pub selected: bool,
    /// Accessible name: "Inbox: 778 unread messages" / "Drafts: 8 items".
    pub accessible: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FolderRowKind {
    FavoritesHeader,
    Favorite,
    AccountRoot,
    Folder,
}

/// One row of the message list (an item or a group header). Nearly every row is an item, so boxing it would
/// only add an allocation per row.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ListRow {
    /// A group header. `label` is the key the group collapses by: with Group By fields the labels of the
    /// group and the groups around it, outermost first, joined with [`GROUP_SEP`] ([`group_title`] is the
    /// header text); `depth` is how deep the group is nested (0 = outermost).
    Group {
        label: String,
        count: usize,
        collapsed: bool,
        #[serde(default, skip_serializing_if = "is_zero_u8")]
        depth: u8,
    },
    Item(ItemRow),
}

fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

/// The header text of a group: its own label without the labels of the groups around it.
pub fn group_title(label: &str) -> &str {
    label.rsplit(GROUP_SEP).next().unwrap_or(label)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemRow {
    pub id: ItemId,
    /// Line 1: sender display (recipient in Drafts/Sent Items; "[Draft]" prefix flag separate).
    pub who: String,
    pub subject: String,
    /// Time column ("9:10 AM", "Thu 7:30 AM", "Wed 9/30", "9/19/2026"; "None" for drafts without date).
    pub time: String,
    pub preview: String,
    pub unread: bool,
    pub selected: bool,
    pub draft_marker: bool,
    pub has_attachments: bool,
    pub importance_high: bool,
    pub importance_low: bool,
    pub flagged: bool,
    pub flag_complete: bool,
    /// Category colours (hex) in order.
    pub categories: Vec<String>,
    pub replied: bool,
    pub forwarded: bool,
    /// Indent level of a conversation child row (1 = directly under the header; deeper in the Classic
    /// Indented View). 0 for header rows and plain rows.
    pub conversation_depth: u8,
    /// Accessible name as documented in plan/reference/reading-pane.md (conversation headers:
    /// "Conversation, From …, Subject …", `plan/reference/reply-forward.md` › Conversation view).
    pub accessible: String,
    /// Conversation view: what this row is within its conversation (`None` for plain rows, including
    /// conversations of a single message).
    #[serde(default)]
    pub conversation: Option<ConversationRow>,
    /// Font from the view's Conditional Formatting rules (`cmd::view::mail`).
    #[serde(default)]
    pub style: RowStyle,
    /// Pinned to the top of the list (Quick Action Pin).
    #[serde(default)]
    pub pinned: bool,
    /// Only the header was downloaded (Send / Receive › Download Headers).
    #[serde(default)]
    pub header_only: bool,
    /// Marked to download (`Some(true)`: Mark to Download Message Copy).
    #[serde(default)]
    pub marked: Option<bool>,
    /// How many earlier rows of the list show the same item (Arrange by Categories lists a message under
    /// each of its categories), so each row can have its own widget id.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeat: u16,
    /// A search result that is not a message (Search › All Items): the item window that opens it
    /// (`appointment`, `contact`, `task`, `note`). `None` for messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Font effects of a list row (Conditional Formatting). The default is plain text in the list's colours.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// "#rrggbb" text colour.
    pub color: Option<String>,
}

/// Conversation part of a message-list row (Show as Conversations).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRow {
    /// Conversation key (`mailcraft_model::thread::Conversation::key`).
    pub key: u64,
    /// Header row (true) or child row (false).
    pub header: bool,
    /// The conversation shows its child rows (◢); collapsed headers show ▷.
    pub expanded: bool,
    /// Messages shown in the conversation (this folder plus other folders when that setting is on).
    pub count: usize,
    /// Unread messages among them.
    pub unread: usize,
    /// Child rows of a message from another folder (Show Messages from Other Folders): that folder's name.
    pub other_folder: Option<String>,
    /// Show Senders Above the Subject (header rows put the senders on line 1, else the subject).
    pub senders_above_subject: bool,
    /// The selection is inside this conversation.
    pub contains_selection: bool,
}

/// Folder kinds shown in the folder pane of `module`.
fn kind_visible(kind: FolderKind, module: Module) -> bool {
    match module {
        Module::Folders => true,
        Module::Calendar => kind == FolderKind::Calendar,
        Module::People => kind == FolderKind::Contacts,
        Module::Tasks => kind == FolderKind::Tasks,
        Module::Notes => kind == FolderKind::Notes,
        Module::Mail | Module::Shortcuts => {
            matches!(kind, FolderKind::Mail | FolderKind::SearchRoot | FolderKind::Search)
        }
    }
}

/// Folders whose Properties › General › "Show total number of items" is on (kept with the window-wide
/// dialog settings, `state.uiPrefs`, as `folder.<id>.showTotal`).
pub fn show_total_folders(s: &Session) -> BTreeSet<u64> {
    let Some(serde_json::Value::Object(m)) = s.settings.get("state.uiPrefs") else {
        return BTreeSet::new();
    };
    m.iter()
        .filter(|(_, v)| v.as_bool() == Some(true))
        .filter_map(|(k, _)| k.strip_prefix("folder.")?.strip_suffix(".showTotal")?.parse().ok())
        .collect()
}

fn folder_row(
    s: &Session,
    id: FolderId,
    kind: FolderRowKind,
    depth: u32,
    has_children: bool,
    totals: &BTreeSet<u64>,
) -> Option<FolderRow> {
    let f = s.mailbox.folder(id)?;
    let show_total = totals.contains(&id.0) || f.role.is_some_and(FolderRole::shows_total_count);
    let (badge, accessible) = if f.kind == FolderKind::Search {
        // Search folders count their live results (unread), not the items filed in them (none).
        let unread = crate::search_index::search_folder_counts(s, id).unread;
        match unread {
            0 => (String::new(), f.name.clone()),
            1 => ("1".to_owned(), format!("{}: 1 unread message", f.name)),
            n => (n.to_string(), format!("{}: {n} unread messages", f.name)),
        }
    } else if show_total {
        // The folder's own choice: the number of items, unread or not.
        match s.mailbox.total_count(id) {
            0 => (String::new(), f.name.clone()),
            1 => ("[1]".to_owned(), format!("{}: 1 item", f.name)),
            n => (format!("[{n}]"), format!("{}: {n} items", f.name)),
        }
    } else {
        (s.mailbox.badge(id).unwrap_or_default(), s.mailbox.folder_accessible_name(id))
    };
    Some(FolderRow {
        id: Some(id),
        kind,
        name: f.name.clone(),
        depth,
        has_children,
        expanded: f.expanded,
        badge,
        badge_total: show_total && f.kind != FolderKind::Search,
        selected: s.selection.folder == id,
        accessible,
    })
}

/// Folder pane rows in display order (Favorites section, then each account tree), honouring expansion.
///
/// The Mail module shows mail and search folders; Calendar/People/Tasks/Notes show their own folder kind;
/// the Folders view (More Apps › Folders) shows every folder of every type. Favorites appear in Mail only.
pub fn folder_rows(s: &Session) -> Vec<FolderRow> {
    let mb = &s.mailbox;
    let module = s.view.module;
    let totals = show_total_folders(s);
    let mut rows = Vec::new();
    if s.view.favorites_visible && module == Module::Mail {
        let favs = mb.favorites();
        if !favs.is_empty() {
            rows.push(FolderRow {
                id: None,
                kind: FolderRowKind::FavoritesHeader,
                name: "Favorites".to_owned(),
                depth: 0,
                has_children: true,
                expanded: true,
                badge: String::new(),
                badge_total: false,
                selected: false,
                accessible: "Favorites".to_owned(),
            });
            rows.extend(
                favs.into_iter()
                    .filter_map(|id| folder_row(s, id, FolderRowKind::Favorite, 1, false, &totals)),
            );
        }
    }
    for account in &mb.accounts {
        let name = if account.address.trim().is_empty() {
            account.display_name.clone()
        } else {
            account.address.clone()
        };
        // Folder-pane order: Show All Folders A to Z sorts default folders in with the rest.
        let roots: Vec<FolderId> = crate::cmd::folder::pane_children(s, account.id, None)
            .into_iter()
            .filter(|id| mb.folder(*id).is_some_and(|f| kind_visible(f.kind, module)))
            .collect();
        rows.push(FolderRow {
            id: None,
            kind: FolderRowKind::AccountRoot,
            name: name.clone(),
            depth: 0,
            has_children: !roots.is_empty(),
            expanded: true,
            badge: String::new(),
            badge_total: false,
            selected: false,
            accessible: name,
        });
        // Depth-first walk with an explicit stack; `visited` guards against corrupted (cyclic) parent links.
        let mut visited: BTreeSet<FolderId> = BTreeSet::new();
        let mut stack: Vec<(FolderId, u32)> = roots.into_iter().rev().map(|id| (id, 1)).collect();
        while let Some((id, depth)) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            let kids: Vec<FolderId> = crate::cmd::folder::pane_children(s, account.id, Some(id))
                .into_iter()
                .filter(|k| mb.folder(*k).is_some_and(|f| kind_visible(f.kind, module)))
                .collect();
            let Some(row) = folder_row(s, id, FolderRowKind::Folder, depth, !kids.is_empty(), &totals) else {
                continue;
            };
            let expanded = row.expanded;
            rows.push(row);
            if expanded {
                let next = depth.saturating_add(1);
                stack.extend(kids.into_iter().rev().map(|k| (k, next)));
            }
        }
    }
    rows
}

/// The kind of item a message is, for Arrange By › Type: drafts, meeting requests, responses and
/// cancellations (an iCalendar part; the response kind from the subject the organizer's client gives it),
/// read receipts and delivery reports (RFC 8098 / RFC 3464 parts), signed and encrypted messages (S/MIME,
/// RFC 8551), else a plain message. Reads only the item's part list and subject, so grouping a large folder
/// stays cheap. Returns (group order, label).
pub fn item_type(m: &MailItem) -> (i64, &'static str) {
    if m.is_draft {
        return (1, "Draft");
    }
    let has = |types: &[&str]| {
        m.attachments.iter().any(|a| {
            let t = a.mime_type.trim().to_ascii_lowercase();
            types.iter().any(|x| t.starts_with(x))
        })
    };
    if has(&["text/calendar", "application/ics"]) {
        let subject = m.subject.trim_start().to_lowercase();
        let response = ["accepted:", "declined:", "tentative:", "new time proposed:"];
        if response.iter().any(|p| subject.starts_with(p)) {
            return (3, "Meeting Response");
        }
        if subject.starts_with("canceled:") || subject.starts_with("cancelled:") {
            return (4, "Meeting Cancellation");
        }
        return (2, "Meeting Request");
    }
    if has(&["message/disposition-notification"]) {
        return (5, "Read Receipt");
    }
    if has(&["message/delivery-status", "message/global-delivery-status"]) {
        return (6, "Delivery Report");
    }
    if has(&["application/pkcs7-mime", "application/x-pkcs7-mime"]) {
        return (8, "Encrypted Message");
    }
    if has(&["application/pkcs7-signature", "application/x-pkcs7-signature"]) {
        return (7, "Signed Message");
    }
    (0, "Message")
}

/// View settings of a folder (defaults when none were saved).
pub fn view_settings(s: &Session, folder: FolderId) -> ViewSettings {
    s.mailbox.views.get(&folder).cloned().unwrap_or_default()
}

/// What marks me as mentioned in a message, lower-case: my addresses (for mailto links) and the "@" tokens
/// that address me ("@" + each address's local part, "@" + each account's sender name).
#[derive(Clone, Debug, Default)]
pub struct MentionNeedles {
    pub addresses: Vec<String>,
    pub tokens: Vec<String>,
}

fn mention_needles(s: &Session) -> MentionNeedles {
    let mut out = MentionNeedles::default();
    for addr in &s.me {
        let addr = addr.trim().to_lowercase();
        if let Some((local, _)) = addr.split_once('@')
            && !local.is_empty()
        {
            out.tokens.push(format!("@{local}"));
        }
        if !addr.is_empty() {
            out.addresses.push(addr);
        }
    }
    for a in &s.mailbox.accounts {
        let addr = a.address.trim().to_lowercase();
        if !addr.is_empty() && !out.addresses.contains(&addr) {
            out.addresses.push(addr);
        }
        let name = a.sender_name.trim();
        if !name.is_empty() {
            out.tokens.push(format!("@{}", name.to_lowercase()));
        }
    }
    out
}

/// Most body bytes examined for mentions.
const MAX_MENTION_SCAN: usize = 256 * 1024;
/// Most mailto links examined per message, and how far after one its address and text are looked for.
const MAX_MENTION_LINKS: usize = 200;
const MENTION_LINK_WINDOW: usize = 1024;

/// The first `max` bytes of `s` or fewer, cut on a character boundary.
fn scan_head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or_default()
}

/// Where the quoted history of a reply or forward starts in plain text: a separator line ("-----Original
/// Message-----", "________"), a quoted header block line ("From: …" followed by "Sent:"/"Date:"/"To:"), an
/// "On … wrote:" line, or the first ">" quoted line. Mentions only count before it.
fn history_start(text: &str) -> usize {
    let lines: Vec<(usize, &str)> = {
        let mut out = Vec::new();
        let mut pos = 0usize;
        for line in text.split_inclusive('\n') {
            out.push((pos, line));
            pos = pos.saturating_add(line.len());
        }
        out
    };
    for (i, (pos, line)) in lines.iter().enumerate() {
        let t = line.trim();
        let lower = t.to_lowercase();
        let next = lines.get(i.saturating_add(1)).map(|(_, l)| l.trim().to_lowercase()).unwrap_or_default();
        let separator = (t.starts_with("-----") && lower.contains("original message"))
            || (t.len() >= 8 && t.chars().all(|c| c == '_'));
        let header_block = lower.starts_with("from:")
            && (next.starts_with("sent:") || next.starts_with("date:") || next.starts_with("to:"));
        let wrote = lower.starts_with("on ") && lower.ends_with("wrote:");
        if separator || header_block || wrote || t.starts_with('>') {
            return *pos;
        }
    }
    text.len()
}

/// True when `token` ("@name") occurs in `hay` as a word of its own: not glued to a letter or digit before
/// (an address such as "x@name.org" is not a mention) and not running on into one after.
fn has_token(hay: &str, token: &str) -> bool {
    if token.len() < 2 {
        return false;
    }
    let mut from = 0usize;
    while let Some(i) = hay.get(from..).and_then(|h| h.find(token)) {
        let at = from.saturating_add(i);
        let end = at.saturating_add(token.len());
        let before = hay.get(..at).and_then(|h| h.chars().next_back());
        let after = hay.get(end..).and_then(|h| h.chars().next());
        let glued_before = before.is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_');
        let glued_after = after.is_some_and(|c| c.is_alphanumeric() || c == '@');
        if !glued_before && !glued_after {
            return true;
        }
        // Step past the match on a char boundary.
        from = at.saturating_add(token.chars().next().map_or(1, char::len_utf8));
    }
    false
}

/// HTML mentions: a mailto link to one of my addresses whose text starts with "@" (how mail clients mark
/// an @mention), before the quoted history (a reply header block, a blockquote or a horizontal rule).
fn html_mentions(html: &str, addresses: &[String]) -> bool {
    // Capped before anything else, on a character boundary (a cut inside a character must not fall back to
    // the whole body).
    let lower = scan_head(html, MAX_MENTION_SCAN).to_lowercase();
    let lower = lower.as_str();
    let cut = ["id=\"divrplyfwdmsg\"", "<blockquote", "<hr", "-----original message-----"]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(lower.len());
    let own = lower.get(..cut).unwrap_or(lower);
    let mut from = 0usize;
    let mut links = 0usize;
    while let Some(i) = own.get(from..).and_then(|h| h.find("mailto:")) {
        links = links.saturating_add(1);
        if links > MAX_MENTION_LINKS {
            break;
        }
        let start = from.saturating_add(i).saturating_add("mailto:".len());
        // Everything about one link is looked for in a small window after it, so many links without a
        // closing tag stay linear.
        let rest = scan_head(own.get(start..).unwrap_or_default(), MENTION_LINK_WINDOW);
        let addr_end = rest.find(['"', '\'', '?', '>', ' ']).unwrap_or(rest.len());
        let addr = rest.get(..addr_end).unwrap_or_default().trim();
        let text = rest
            .find('>')
            .and_then(|gt| rest.get(gt.saturating_add(1)..))
            .map(|t| t.split("</a").next().unwrap_or_default())
            .unwrap_or_default();
        // The link text without tags inside it (<span>@Name</span>).
        let mut plain = String::new();
        let mut in_tag = false;
        for c in text.chars().take(200) {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => plain.push(c),
                _ => {}
            }
        }
        if addresses.iter().any(|a| a == addr) && plain.trim().starts_with('@') {
            return true;
        }
        from = start;
    }
    false
}

/// Mentioned Mail: the sender @mentioned me. In HTML, a mailto link to one of my addresses written as
/// "@Name"; in text, "@" + my name or my address's local part as a word of its own. Only what the sender
/// wrote counts: quoted reply history (where my address appears in every header block) is left out.
pub fn mentions_me(s: &Session, item: &MailItem) -> bool {
    mentions_with(&mention_needles(s), item)
}

fn mentions_with(needles: &MentionNeedles, item: &MailItem) -> bool {
    if let Some(html) = &item.body.html
        && html_mentions(html, &needles.addresses)
    {
        return true;
    }
    let text = item.body_text();
    let text = scan_head(&text, MAX_MENTION_SCAN);
    let own = text.get(..history_start(text)).unwrap_or(text).to_lowercase();
    needles.tokens.iter().any(|t| has_token(&own, t))
}

/// Items the message list shows for the selected folder after search, Focused/Other and the filter, sorted
/// (not yet grouped), plus the view settings used.
fn view_items(s: &Session) -> (ViewSettings, Vec<&MailItem>) {
    let mb = &s.mailbox;
    let folder = s.selection.folder;
    let vs = view_settings(s, folder);
    let Some(f) = mb.folder(folder) else {
        return (vs, Vec::new());
    };
    // Searches and search folders go through the search index (`search_index`); results are mail only here.
    let mut items: Vec<&MailItem> = if !s.view.search.trim().is_empty() {
        crate::search_index::results(s).iter().filter_map(|id| mb.mail.get(id)).collect()
    } else if f.kind == FolderKind::Search {
        crate::search_index::search_folder_items(s, folder).iter().filter_map(|id| mb.mail.get(id)).collect()
    } else {
        let mut v = mb.items_in(folder);
        if f.role == Some(FolderRole::Inbox) && vs.focused_inbox {
            let want = if s.view.focused_tab { FocusClass::Focused } else { FocusClass::Other };
            v.retain(|m| m.focus == want);
        }
        v
    };
    match vs.filter {
        ListFilter::All => {}
        ListFilter::Unread => items.retain(|m| !m.read),
        ListFilter::Flagged => items.retain(|m| m.is_flagged()),
        ListFilter::Mentioned => {
            let needles = mention_needles(s);
            items.retain(|m| mentions_with(&needles, m));
        }
    }
    // View Settings › Filter.
    if let Some(f) = crate::cmd::view::mail::custom_filter(s, folder) {
        items.retain(|m| f.matches(s, m));
    }
    sort_items(s, &vs, &mut items);
    // Options › Search › "Show most relevant search results on top": the best hits lead a search.
    let top = top_results(s);
    if !top.is_empty() {
        let (mut first, rest): (Vec<&MailItem>, Vec<&MailItem>) =
            items.into_iter().partition(|m| top.contains(&m.id));
        first.sort_by_key(|m| top.iter().position(|t| *t == m.id));
        first.extend(rest);
        items = first;
    }
    // Pinned messages lead the list (conversations keep their own order).
    if !vs.conversations {
        let pins = crate::cmd::view::mail::pinned(s);
        if !pins.is_empty() {
            let (mut top, rest): (Vec<&MailItem>, Vec<&MailItem>) =
                items.into_iter().partition(|m| pins.contains(&m.id));
            top.extend(rest);
            items = top;
        }
    }
    (vs, items)
}

/// Pinned messages of the list (empty in conversation view, which keeps its own order).
fn list_pins(s: &Session, vs: &ViewSettings) -> BTreeSet<ItemId> {
    if vs.conversations { BTreeSet::new() } else { crate::cmd::view::mail::pinned(s) }
}

/// Group header of the most relevant search results.
pub const TOP_RESULTS_LABEL: &str = "Top Results";
/// How many search results [`top_results`] puts on top.
pub const TOP_RESULTS: usize = 3;

/// Options › Search › "Show most relevant search results on top": the [`TOP_RESULTS`] best hits of the
/// active search (relevance: term frequency, rarity, recency), shown in their own group above the results
/// in the view's order. Empty while the option is off, without a search, when the search found no more than
/// that many messages, or in conversation view (which keeps its own order).
pub fn top_results(s: &Session) -> Vec<ItemId> {
    if s.view.search.trim().is_empty() || !s.settings.bool("search.relevance") {
        return Vec::new();
    }
    if view_settings(s, s.selection.folder).conversations {
        return Vec::new();
    }
    let hits = crate::search_index::results(s);
    if hits.len() <= TOP_RESULTS {
        return Vec::new();
    }
    hits.into_iter().take(TOP_RESULTS).collect()
}

/// Options › Search › "Notify me when results might be limited because search indexing is not complete":
/// while a search covers messages downloaded as headers only (their text is not on this computer, so it
/// cannot be indexed), the notice shown above the results. `None` without such messages, without a search
/// or when the option is off.
pub fn search_notice(s: &Session) -> Option<String> {
    if s.view.search.trim().is_empty() || !s.settings.bool("search.notifyIncomplete") {
        return None;
    }
    let folders: BTreeSet<FolderId> = crate::search_index::scope_folders(s).into_iter().collect();
    let n = s.mailbox.mail.values().filter(|m| folders.contains(&m.folder) && header_only(s, m)).count();
    match n {
        0 => None,
        1 => Some(
            "Results might be incomplete: 1 message here was downloaded as headers only, so its text isn't \
             searched."
                .to_owned(),
        ),
        n => Some(format!(
            "Results might be incomplete: {n} messages here were downloaded as headers only, so their text \
             isn't searched."
        )),
    }
}

/// Primary sort / group key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Num(i64),
    Text(String),
}

/// Size bucket index and label.
pub fn size_bucket(size: u64) -> (i64, &'static str) {
    const KB: u64 = 1024;
    match size {
        s if s < 10 * KB => (0, "Tiny (< 10 KB)"),
        s if s < 25 * KB => (1, "Small (10 - 25 KB)"),
        s if s < 100 * KB => (2, "Medium (25 - 100 KB)"),
        s if s < 500 * KB => (3, "Large (100 - 500 KB)"),
        s if s <= 1024 * KB => (4, "Very Large (500 KB - 1 MB)"),
        _ => (5, "Huge (> 1 MB)"),
    }
}

fn names(list: &[mailcraft_model::Address]) -> String {
    list.iter().map(|m| m.display().to_owned()).collect::<Vec<_>>().join("; ")
}

fn sender_name(m: &MailItem) -> String {
    m.from.as_ref().or(m.sender.as_ref()).map(|a| a.display().to_owned()).unwrap_or_default()
}

const NONE_LABEL: &str = "(none)";

/// Group key and header label of an item under `arrange`.
fn group_of(s: &Session, m: &MailItem, arrange: ArrangeBy) -> (Key, String) {
    let tz = s.tz_offset_min;
    let text_key = |label: String| {
        if label.trim().is_empty() {
            (Key::Text(String::new()), NONE_LABEL.to_owned())
        } else {
            (Key::Text(label.to_lowercase()), label)
        }
    };
    let flag_date = |d: Option<mailcraft_model::DateTime>| match d {
        Some(d) => (Key::Num(d.day_number(tz)), ListTime::long_date(d, tz)),
        None => (Key::Num(i64::MIN), NONE_LABEL.to_owned()),
    };
    match arrange {
        ArrangeBy::Date => {
            let g = DateGroup::of(m.received, s.now(), tz);
            (Key::Num(m.received.unix), g.label())
        }
        ArrangeBy::From => text_key(sender_name(m)),
        ArrangeBy::To => text_key(names(&m.to)),
        ArrangeBy::Categories => text_key(m.categories.join(", ")),
        ArrangeBy::FlagStatus => match m.flag {
            FlagState::Flagged { .. } => (Key::Num(2), "Flagged".to_owned()),
            FlagState::Complete { .. } => (Key::Num(1), "Completed".to_owned()),
            FlagState::None => (Key::Num(0), "Unflagged".to_owned()),
        },
        ArrangeBy::FlagStartDate => flag_date(match &m.flag {
            FlagState::Flagged { start, .. } => *start,
            _ => None,
        }),
        ArrangeBy::FlagDueDate => flag_date(match &m.flag {
            FlagState::Flagged { due, .. } => *due,
            _ => None,
        }),
        ArrangeBy::Size => {
            let (i, label) = size_bucket(m.size);
            (Key::Num(i), label.to_owned())
        }
        ArrangeBy::Subject => {
            let topic = strip_prefixes(&m.subject).trim().to_owned();
            if topic.is_empty() {
                (Key::Text(String::new()), NONE_LABEL.to_owned())
            } else {
                (Key::Text(conversation_topic(&m.subject)), topic)
            }
        }
        ArrangeBy::Type => {
            let (order, label) = item_type(m);
            (Key::Num(order), label.to_owned())
        }
        ArrangeBy::Attachments => {
            if m.has_attachments() {
                (Key::Num(1), "With Attachments".to_owned())
            } else {
                (Key::Num(0), "Without Attachments".to_owned())
            }
        }
        ArrangeBy::Account => {
            let name = s
                .mailbox
                .folder_account(m.folder)
                .and_then(|a| s.mailbox.account(a))
                .map(|a| if a.address.is_empty() { a.display_name.clone() } else { a.address.clone() })
                .unwrap_or_default();
            text_key(name)
        }
        ArrangeBy::Importance => match m.importance {
            Importance::High => (Key::Num(2), "High".to_owned()),
            Importance::Normal => (Key::Num(1), "Normal".to_owned()),
            Importance::Low => (Key::Num(0), "Low".to_owned()),
        },
    }
}

fn sort_items(s: &Session, vs: &ViewSettings, items: &mut [&MailItem]) {
    let arrange = vs.arrange_by;
    let descending = vs.order == SortOrder::Descending;
    // Sort › Then by: further keys after the arrangement.
    let then: Vec<crate::cmd::view::mail::MailField> =
        if crate::cmd::view::mail::mail_list_folder(s, s.selection.folder) {
            crate::cmd::view::mail::mail_view(s, s.selection.folder)
                .then_by
                .into_iter()
                .filter(|f| f.key != arrange)
                .collect()
        } else {
            Vec::new()
        };
    let mut keyed: Vec<(Key, Vec<Key>, i64, &MailItem)> = items
        .iter()
        .map(|m| {
            let (k, _) = group_of(s, m, arrange);
            let more: Vec<Key> = then.iter().map(|f| group_of(s, m, f.key).0).collect();
            let secondary = if arrange == ArrangeBy::Size {
                i64::try_from(m.size).unwrap_or(i64::MAX)
            } else {
                m.received.unix
            };
            (k, more, secondary, *m)
        })
        .collect();
    keyed.sort_by(|a, b| {
        let primary = a.0.cmp(&b.0);
        let primary = if descending { primary.reverse() } else { primary };
        let mut more = std::cmp::Ordering::Equal;
        for ((x, y), f) in a.1.iter().zip(b.1.iter()).zip(then.iter()) {
            let o = if f.descending { y.cmp(x) } else { x.cmp(y) };
            if o != std::cmp::Ordering::Equal {
                more = o;
                break;
            }
        }
        let secondary = match arrange {
            // Date and Size order every row by the sort direction.
            ArrangeBy::Date | ArrangeBy::Size if !descending => a.2.cmp(&b.2),
            // Elsewhere rows inside a group stay newest first.
            _ => b.2.cmp(&a.2),
        };
        primary.then(more).then(secondary).then(a.3.id.cmp(&b.3.id))
    });
    for (slot, (_, _, _, m)) in items.iter_mut().zip(keyed) {
        *slot = m;
    }
}

/// Group By fields of the selected folder's list (empty: the list groups by its arrangement).
fn manual_fields(s: &Session) -> Vec<crate::cmd::view::mail::MailField> {
    if !crate::cmd::view::mail::mail_list_folder(s, s.selection.folder) {
        return Vec::new();
    }
    crate::cmd::view::mail::mail_view(s, s.selection.folder).group_by
}

/// Most groups one message is listed under with Group By fields (many categories at several levels).
const MAX_ITEM_GROUPS: usize = 16;

/// Order of a date bucket for Group By on Date: newer buckets rank higher.
fn date_rank(g: &DateGroup) -> i64 {
    match g {
        DateGroup::Future => 20,
        DateGroup::Today => 19,
        DateGroup::Yesterday => 18,
        DateGroup::ThisWeek(w) => 10 + i64::from(w.0.min(6)),
        DateGroup::LastWeek => 9,
        DateGroup::TwoWeeksAgo => 8,
        DateGroup::ThreeWeeksAgo => 7,
        DateGroup::LastMonth => 6,
        DateGroup::Older => 5,
    }
}

/// Headers of message `m` for the Group By field `key` ("From: Alice Rivera"), with their order. Categories
/// give a header per category; Date groups by the list's date buckets.
fn field_headers(s: &Session, m: &MailItem, key: ArrangeBy) -> Vec<(Key, String)> {
    let name = key.label();
    match key {
        ArrangeBy::Date => {
            let g = DateGroup::of(m.received, s.now(), s.tz_offset_min);
            vec![(Key::Num(date_rank(&g)), format!("{name}: {}", g.label()))]
        }
        ArrangeBy::Categories => {
            let mut seen: Vec<String> = Vec::new();
            let mut out = Vec::new();
            for c in m.categories.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
                let k = c.to_lowercase();
                if seen.contains(&k) || out.len() >= MAX_ITEM_GROUPS {
                    continue;
                }
                seen.push(k.clone());
                out.push((Key::Text(k), format!("{name}: {c}")));
            }
            if out.is_empty() {
                out.push((Key::Text(String::new()), format!("{name}: {NONE_LABEL}")));
            }
            out
        }
        _ => {
            let (k, label) = group_of(s, m, key);
            vec![(k, format!("{name}: {label}"))]
        }
    }
}

/// Leaf groups of a list grouped by Group By fields: (labels outermost first, items) in list order. Items
/// keep their sort order inside a group; pinned messages and top search results keep their own groups at
/// the top.
type Nested<'a> = Vec<(Vec<String>, Vec<&'a MailItem>)>;

fn nested<'a>(
    s: &Session,
    fields: &[crate::cmd::view::mail::MailField],
    items: Vec<&'a MailItem>,
    pins: &BTreeSet<ItemId>,
) -> Nested<'a> {
    let top = top_results(s);
    let mut out: Nested<'a> = Vec::new();
    let mut entries: Vec<(Vec<(Key, String, bool)>, usize, &'a MailItem)> = Vec::new();
    for (i, m) in items.into_iter().enumerate() {
        let own = if top.contains(&m.id) && !pins.contains(&m.id) {
            Some(TOP_RESULTS_LABEL)
        } else if pins.contains(&m.id) {
            Some(crate::cmd::view::mail::PINNED_LABEL)
        } else {
            None
        };
        if let Some(label) = own {
            match out.last_mut() {
                Some((l, list)) if l.len() == 1 && l.first().is_some_and(|x| x == label) => list.push(m),
                _ => out.push((vec![label.to_owned()], vec![m])),
            }
            continue;
        }
        let mut paths: Vec<Vec<(Key, String, bool)>> = vec![Vec::new()];
        for f in fields {
            let options = field_headers(s, m, f.key);
            let mut next = Vec::new();
            for p in &paths {
                for (k, l) in &options {
                    if next.len() >= MAX_ITEM_GROUPS {
                        break;
                    }
                    let mut q = p.clone();
                    q.push((k.clone(), l.clone(), f.descending));
                    next.push(q);
                }
            }
            paths = next;
        }
        entries.extend(paths.into_iter().map(|p| (p, i, m)));
    }
    entries.sort_by(|a, b| {
        for (x, y) in a.0.iter().zip(b.0.iter()) {
            let o = x.0.cmp(&y.0).then_with(|| x.1.cmp(&y.1));
            let o = if x.2 { o.reverse() } else { o };
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        a.1.cmp(&b.1)
    });
    for (path, _, m) in entries {
        let labels: Vec<String> = path.into_iter().map(|(_, l, _)| l).collect();
        match out.last_mut() {
            Some((l, list)) if *l == labels => list.push(m),
            _ => out.push((labels, vec![m])),
        }
    }
    out
}

/// Rows of nested groups: a header for each group as it starts (none inside a collapsed group), then the
/// items of the leaf groups that are open (`items` pushes them, `count` counts a leaf group's rows).
fn push_nested<'a>(
    s: &Session,
    rows: &mut Vec<ListRow>,
    groups: &Nested<'a>,
    mut items: impl FnMut(&mut Vec<ListRow>, &[&'a MailItem]),
    count: impl Fn(&[&'a MailItem]) -> usize,
) {
    let collapsed = collapsed_labels(s);
    let shut: HashSet<&str> = collapsed.iter().map(String::as_str).collect();
    // An outer group counts the rows of the groups inside it.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (labels, list) in groups {
        let n = count(list);
        for d in 0..labels.len() {
            if let Some(prefix) = labels.get(..=d) {
                let e = counts.entry(prefix.join(GROUP_SEP)).or_insert(0);
                *e = e.saturating_add(n);
            }
        }
    }
    let mut prev: &[String] = &[];
    for (labels, list) in groups {
        let mut hidden = false;
        for d in 0..labels.len() {
            let Some(prefix) = labels.get(..=d) else { break };
            let key = prefix.join(GROUP_SEP);
            if !hidden && prev.get(..=d) != Some(prefix) {
                rows.push(ListRow::Group {
                    label: key.clone(),
                    count: counts.get(&key).copied().unwrap_or(0),
                    collapsed: shut.contains(key.as_str()),
                    depth: u8::try_from(d).unwrap_or(u8::MAX),
                });
            }
            hidden |= shut.contains(key.as_str());
        }
        if !hidden {
            items(rows, list);
        }
        prev = labels;
    }
}

/// Every group of nested groups with its items, outer groups first: (collapse key, item ids) in list order.
fn nested_ids(groups: &Nested<'_>, ids: impl Fn(&MailItem) -> Vec<ItemId>) -> Vec<(String, Vec<ItemId>)> {
    let mut out: Vec<(String, Vec<ItemId>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (labels, list) in groups {
        let mine: Vec<ItemId> = list.iter().flat_map(|m| ids(m)).collect();
        for d in 0..labels.len() {
            let Some(prefix) = labels.get(..=d) else { break };
            let key = prefix.join(GROUP_SEP);
            let i = *index.entry(key.clone()).or_insert_with(|| {
                out.push((key, Vec::new()));
                out.len().saturating_sub(1)
            });
            if let Some((_, v)) = out.get_mut(i) {
                v.extend(mine.iter().copied());
            }
        }
    }
    out
}

/// Splits sorted view items into list groups: `(header label, items)` in list order.
///
/// Items group by their sort key (so "Alice Rivera" and "alice rivera", or "Re: Plans" and "plans", share
/// one group); Date groups by its bucket. The header shows the label of the group's first item.
///
/// Pinned messages (`pins`, already first in `items`) form a group of their own at the top.
fn grouped<'a>(
    s: &Session,
    arrange: ArrangeBy,
    items: Vec<&'a MailItem>,
    pins: &BTreeSet<ItemId>,
) -> Vec<(String, Vec<&'a MailItem>)> {
    let mut out: Vec<(Key, String, Vec<&'a MailItem>)> = Vec::new();
    let mut categorized: Vec<&'a MailItem> = Vec::new();
    // Top search results (first in `items` after the pins) form a group of their own too.
    let top = top_results(s);
    for m in items {
        if top.contains(&m.id) && !pins.contains(&m.id) {
            let id = Key::Text("\u{1}top".to_owned());
            match out.last_mut() {
                Some((last, _, list)) if *last == id => list.push(m),
                _ => out.push((id, TOP_RESULTS_LABEL.to_owned(), vec![m])),
            }
            continue;
        }
        if pins.contains(&m.id) {
            let id = Key::Text("\u{1}pinned".to_owned());
            match out.last_mut() {
                Some((last, _, list)) if *last == id => list.push(m),
                _ => out.push((id, crate::cmd::view::mail::PINNED_LABEL.to_owned(), vec![m])),
            }
            continue;
        }
        if arrange == ArrangeBy::Categories {
            categorized.push(m);
            continue;
        }
        let (key, label) = group_of(s, m, arrange);
        let id = if arrange == ArrangeBy::Date { Key::Text(label.clone()) } else { key };
        match out.last_mut() {
            Some((last, _, list)) if *last == id => list.push(m),
            _ => out.push((id, label, vec![m])),
        }
    }
    let mut out: Vec<(String, Vec<&'a MailItem>)> =
        out.into_iter().map(|(_, label, list)| (label, list)).collect();
    if arrange == ArrangeBy::Categories {
        out.extend(category_groups(s, categorized));
    }
    out
}

/// Arrange by Categories: a message with several categories is listed under each of them, a message with
/// none under "(none)". Groups follow the sort direction by name; inside a group the newest come first.
fn category_groups<'a>(s: &Session, items: Vec<&'a MailItem>) -> Vec<(String, Vec<&'a MailItem>)> {
    let mut groups: BTreeMap<String, (String, Vec<&'a MailItem>)> = BTreeMap::new();
    for m in items {
        let mut seen: Vec<String> = Vec::new();
        for c in m.categories.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
            let key = c.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            groups.entry(key).or_insert_with(|| (c.to_owned(), Vec::new())).1.push(m);
        }
        if seen.is_empty() {
            groups.entry(String::new()).or_insert_with(|| (NONE_LABEL.to_owned(), Vec::new())).1.push(m);
        }
    }
    let mut out: Vec<(String, Vec<&'a MailItem>)> = groups
        .into_values()
        .map(|(label, mut list)| {
            list.sort_by(|a, b| b.received.unix.cmp(&a.received.unix).then(a.id.cmp(&b.id)));
            (label, list)
        })
        .collect();
    if view_settings(s, s.selection.folder).order == SortOrder::Descending {
        out.reverse();
    }
    out
}

/// List groups in list order: `(header label, item ids)`. In conversation view a group holds the view's
/// messages of its conversations (header message first).
fn group_ids(s: &Session) -> Vec<(String, Vec<ItemId>)> {
    let (vs, items) = view_items(s);
    let fields = manual_fields(s);
    if vs.conversations {
        let convs = build_conversations(s, &vs, items);
        let by_sort: HashMap<ItemId, &ListConversation> = convs.iter().map(|c| (c.sort_item, c)).collect();
        let reps: Vec<&MailItem> = convs.iter().filter_map(|c| s.mailbox.mail.get(&c.sort_item)).collect();
        if !fields.is_empty() {
            let groups = nested(s, &fields, reps, &BTreeSet::new());
            return nested_ids(&groups, |m| by_sort.get(&m.id).map(|c| c.view_members()).unwrap_or_default());
        }
        return grouped(s, vs.arrange_by, reps, &BTreeSet::new())
            .into_iter()
            .map(|(label, list)| {
                let ids = list
                    .iter()
                    .filter_map(|m| by_sort.get(&m.id))
                    .flat_map(|c| c.view_members())
                    .collect::<Vec<ItemId>>();
                (label, ids)
            })
            .collect();
    }
    let pins = list_pins(s, &vs);
    if !fields.is_empty() {
        return nested_ids(&nested(s, &fields, items, &pins), |m| vec![m.id]);
    }
    grouped(s, vs.arrange_by, items, &pins)
        .into_iter()
        .map(|(label, list)| (label, list.into_iter().map(|m| m.id).collect()))
        .collect()
}

/// Header labels of the list's groups, in list order (collapsed groups included).
pub fn group_labels(s: &Session) -> Vec<String> {
    group_ids(s).into_iter().map(|(label, _)| label).collect()
}

/// Status bar "Header Items in View": items of the view whose body was not downloaded yet.
pub fn header_items_in_view(s: &Session) -> usize {
    // Only server accounts download headers alone; skip the view walk for local profiles.
    if s.mailbox.accounts.iter().all(|a| a.kind == mailcraft_model::AccountKind::Local) {
        return 0;
    }
    view_items(s).1.into_iter().filter(|m| header_only(s, m)).count()
}

/// Only the header of `m` was downloaded (an item of a server account; local items never are).
fn header_only(s: &Session, m: &MailItem) -> bool {
    let server = s
        .mailbox
        .folder_account(m.folder)
        .and_then(|a| s.mailbox.account(a))
        .is_some_and(|a| a.kind != mailcraft_model::AccountKind::Local);
    server && crate::sync::cmds::is_header_only(s, m.id)
}

/// Header label of the group `item` falls in under the selected folder's arrangement.
pub fn group_label(s: &Session, item: &MailItem) -> String {
    let ids = group_ids(s);
    if !manual_fields(s).is_empty() {
        // Nested groups: the innermost group holding the item (the one its row sits in).
        return ids
            .into_iter()
            .filter(|(_, list)| list.contains(&item.id))
            .max_by_key(|(label, _)| label.matches(GROUP_SEP).count())
            .map(|(label, _)| label)
            .unwrap_or_default();
    }
    ids.into_iter()
        .find(|(_, list)| list.contains(&item.id))
        .map(|(label, _)| label)
        .unwrap_or_else(|| group_of(s, item, view_settings(s, s.selection.folder).arrange_by).1)
}

/// Item ids in a list group (by header label), in list order, including collapsed groups.
pub fn group_items(s: &Session, label: &str) -> Vec<ItemId> {
    group_ids(s).into_iter().filter(|(l, _)| l == label).flat_map(|(_, list)| list).collect()
}

/// Every item id in the current view, in list order (items inside collapsed groups included).
pub fn view_item_ids(s: &Session) -> Vec<ItemId> {
    view_items(s).1.into_iter().map(|m| m.id).collect()
}

/// All item ids the list shows, in order (collapsed groups and collapsed conversations excluded). A
/// conversation header stands for its newest message in the folder, which is also its first child row, so
/// each id is listed once (at its first row).
pub fn visible_item_ids(s: &Session) -> Vec<ItemId> {
    let mut seen = HashSet::new();
    list_rows(s)
        .into_iter()
        .filter_map(|r| match r {
            // Non-mail results open their own windows; the message keys do not walk onto them.
            ListRow::Item(i) if i.kind.is_none() => Some(i.id),
            ListRow::Item(_) | ListRow::Group { .. } => None,
        })
        .filter(|id| seen.insert(*id))
        .collect()
}

fn category_hex(s: &Session, name: &str) -> String {
    Category::find(&s.mailbox.categories, name)
        .map(|c| c.color.rgb().to_hex())
        .unwrap_or_else(|| "#ffffff".to_owned())
}

fn flag_status_text(m: &MailItem) -> &'static str {
    match m.flag {
        FlagState::None => "Unflagged",
        // Observed: a flagged row reads "Flag Status Red Flag" (follow-up flags are red).
        FlagState::Flagged { .. } => "Red Flag",
        FlagState::Complete { .. } => "Completed",
    }
}

/// Short weekday and numeric date as the row's accessible name shows flag dates: "Fri 10/9/2026".
fn short_day_date(t: mailcraft_model::DateTime, tz: i32) -> String {
    let lt = mailcraft_model::dates::LocalTime::of(t, tz);
    format!("{} {}", lt.weekday.short(), ListTime::date(t, tz))
}

fn item_row(s: &Session, m: &MailItem) -> ItemRow {
    let mb = &s.mailbox;
    let tz = s.tz_offset_min;
    let role = mb.folder(m.folder).and_then(|f| f.role);
    let outgoing = matches!(role, Some(FolderRole::Drafts | FolderRole::SentItems | FolderRole::Outbox));
    let who = if outgoing {
        let to = names(&m.to);
        if to.is_empty() { "(no recipients)".to_owned() } else { to }
    } else {
        let from = sender_name(m);
        if from.is_empty() { "(unknown sender)".to_owned() } else { from }
    };
    let subject = if m.subject.trim().is_empty() { "(no subject)".to_owned() } else { m.subject.clone() };
    let time = if m.is_draft { "None".to_owned() } else { ListTime::format(m.received, s.now(), tz) };
    let flag_dates = match &m.flag {
        FlagState::Flagged { start, due, .. } => {
            let mut out = String::new();
            if let Some(d) = start {
                out.push_str(&format!("Start Date {}, ", short_day_date(*d, tz)));
            }
            if let Some(d) = due {
                out.push_str(&format!("Due Date {}, ", short_day_date(*d, tz)));
            }
            out
        }
        _ => String::new(),
    };
    let draft_marker = s.composes.values().any(|c| c.source == Some(m.id));
    let mut accessible = String::new();
    if draft_marker {
        accessible.push_str("Associated Draft, ");
    }
    if m.has_attachments() {
        accessible.push_str("With Attachments, ");
    }
    match m.importance {
        Importance::High => accessible.push_str("Importance High, "),
        Importance::Low => accessible.push_str("Importance Low, "),
        Importance::Normal => {}
    }
    accessible.push_str(&format!(
        "From {}, Subject {}, Received {}, Size {}, ",
        sender_name(m),
        subject,
        time,
        m.size_label()
    ));
    if !m.categories.is_empty() {
        accessible.push_str(&format!("Categories {}, ", m.categories.join(", ")));
    }
    accessible.push_str(&flag_dates);
    accessible.push_str(&format!("Flag Status {}, Preview {}, ", flag_status_text(m), m.preview));
    ItemRow {
        id: m.id,
        who,
        subject,
        time,
        preview: m.preview.clone(),
        unread: !m.read,
        selected: s.selection.items.contains(&m.id),
        draft_marker,
        has_attachments: m.has_attachments(),
        importance_high: m.importance == Importance::High,
        importance_low: m.importance == Importance::Low,
        flagged: m.is_flagged(),
        flag_complete: matches!(m.flag, FlagState::Complete { .. }),
        categories: m.categories.iter().map(|c| category_hex(s, c)).collect(),
        replied: matches!(m.response, ResponseState::Replied(_) | ResponseState::RepliedAll(_)),
        forwarded: matches!(m.response, ResponseState::Forwarded(_)),
        conversation_depth: 0,
        accessible,
        conversation: None,
        style: RowStyle::default(),
        pinned: false,
        header_only: false,
        marked: None,
        repeat: 0,
        kind: None,
    }
}

/// Message list rows for the selected folder with the folder's view settings (filter, Focused/Other,
/// search, arrange/sort, groups and collapsed groups, conversations).
pub fn list_rows(s: &Session) -> Vec<ListRow> {
    let mut rows = undecorated_rows(s);
    decorate(s, &mut rows);
    other_item_rows(s, &mut rows);
    rows
}

/// Group header of the non-mail results of an All Items search.
pub const OTHER_ITEMS_LABEL: &str = "Other Items";

/// Most non-mail results listed under [`OTHER_ITEMS_LABEL`].
const MAX_OTHER_ITEMS: usize = 500;

/// Search › All Items finds appointments, contacts, tasks and notes too: they follow the messages under
/// their own header ([`OTHER_ITEMS_LABEL`]), best match first, each row saying what it is and which item
/// window opens it ([`ItemRow::kind`]).
fn other_item_rows(s: &Session, rows: &mut Vec<ListRow>) {
    if s.view.search.trim().is_empty() || s.view.search_scope != crate::SearchScope::AllItems {
        return;
    }
    let hits = crate::search_index::hits(s, &s.view.search, &crate::search_index::scope_filter(s));
    let others: Vec<ItemRow> = hits
        .iter()
        .map(|h| ItemId(h.id))
        .filter(|id| !s.mailbox.mail.contains_key(id))
        .filter_map(|id| pim_row(s, id))
        .take(MAX_OTHER_ITEMS)
        .collect();
    if others.is_empty() {
        return;
    }
    let collapsed = collapsed_labels(s).iter().any(|l| l == OTHER_ITEMS_LABEL);
    let grouped = view_settings(s, s.selection.folder).show_in_groups;
    if grouped {
        rows.push(ListRow::Group {
            label: OTHER_ITEMS_LABEL.to_owned(),
            count: others.len(),
            collapsed,
            depth: 0,
        });
    }
    if !(grouped && collapsed) {
        rows.extend(others.into_iter().map(ListRow::Item));
    }
}

/// A list row for a non-mail item (appointment, contact, task, note).
fn pim_row(s: &Session, id: ItemId) -> Option<ItemRow> {
    let mb = &s.mailbox;
    let tz = s.tz_offset_min;
    let (kind, who, subject, time, preview, categories) = if let Some(a) = mb.appointments.get(&id) {
        let who = if a.is_meeting { "Meeting" } else { "Appointment" };
        let when = ListTime::short_date_time(a.start, tz);
        ("appointment", who, a.subject.clone(), when, a.location.clone(), a.categories.clone())
    } else if let Some(c) = mb.contacts.get(&id) {
        let name = c.full_name();
        let name = if name.trim().is_empty() { c.file_as.clone() } else { name };
        let detail = c.emails.first().cloned().unwrap_or_else(|| c.company.clone());
        ("contact", "Contact", name, String::new(), detail, c.categories.clone())
    } else if let Some(t) = mb.tasks.get(&id) {
        let due = t.due.map(|d| format!("Due {}", ListTime::date(d, tz))).unwrap_or_default();
        ("task", "Task", t.subject.clone(), due, String::new(), t.categories.clone())
    } else {
        let n = mb.notes.get(&id)?;
        let first = n.text.lines().next().unwrap_or_default().trim().to_owned();
        let rest: String = n.text.lines().skip(1).collect::<Vec<_>>().join(" ").chars().take(255).collect();
        ("note", "Note", first, String::new(), rest, n.categories.clone())
    };
    let subject: String = subject.chars().take(255).collect();
    let accessible = format!("{who}, {subject}, {time}, ");
    Some(ItemRow {
        id,
        who: who.to_owned(),
        subject,
        time,
        preview: preview.chars().take(255).collect(),
        unread: false,
        selected: false,
        draft_marker: false,
        has_attachments: false,
        importance_high: false,
        importance_low: false,
        flagged: false,
        flag_complete: false,
        categories: categories.iter().map(|c| category_hex(s, c)).collect(),
        replied: false,
        forwarded: false,
        conversation_depth: 0,
        accessible,
        conversation: None,
        style: RowStyle::default(),
        pinned: false,
        header_only: false,
        marked: None,
        repeat: 0,
        kind: Some(kind.to_owned()),
    })
}

/// Conditional Formatting, pins and download state of every item row.
fn decorate(s: &Session, rows: &mut [ListRow]) {
    let rules = crate::cmd::view::mail::rules(s, s.selection.folder);
    let pins = crate::cmd::view::mail::pinned(s);
    for r in rows.iter_mut() {
        let ListRow::Item(row) = r else { continue };
        let Some(m) = s.mailbox.mail.get(&row.id) else { continue };
        row.style = crate::cmd::view::mail::row_style(s, &rules, m, row.unread);
        row.pinned = pins.contains(&m.id);
        row.header_only = header_only(s, m);
        row.marked = crate::sync::cmds::marked(s, m.id);
        if row.pinned {
            row.accessible.push_str("Pinned, ");
        }
        match row.marked {
            Some(true) => row.accessible.push_str("Marked to Download Message Copy, "),
            Some(false) => row.accessible.push_str("Marked to Download, "),
            None if row.header_only => row.accessible.push_str("Header Only, "),
            None => {}
        }
    }
}

fn undecorated_rows(s: &Session) -> Vec<ListRow> {
    let (vs, items) = view_items(s);
    if vs.conversations {
        return conversation_rows(s, &vs, items);
    }
    if !vs.show_in_groups {
        return items.into_iter().map(|m| ListRow::Item(item_row(s, m))).collect();
    }
    let collapsed = collapsed_labels(s);
    let pins = list_pins(s, &vs);
    let mut rows = Vec::new();
    let mut shown: HashMap<ItemId, u16> = HashMap::new();
    let fields = manual_fields(s);
    if !fields.is_empty() {
        // Group By fields: nested groups (a message under several categories gets a row in each).
        let groups = nested(s, &fields, items, &pins);
        push_nested(
            s,
            &mut rows,
            &groups,
            |rows, list| {
                rows.extend(list.iter().map(|m| {
                    let mut row = item_row(s, m);
                    let n = shown.entry(m.id).or_insert(0);
                    row.repeat = *n;
                    *n = n.saturating_add(1);
                    ListRow::Item(row)
                }));
            },
            <[&MailItem]>::len,
        );
        return rows;
    }
    for (label, list) in grouped(s, vs.arrange_by, items, &pins) {
        let is_collapsed = group_collapsed(s, &vs, &collapsed, &label, &list);
        rows.push(ListRow::Group { label, count: list.len(), collapsed: is_collapsed, depth: 0 });
        if !is_collapsed {
            rows.extend(list.into_iter().map(|m| {
                let mut row = item_row(s, m);
                let n = shown.entry(m.id).or_insert(0);
                row.repeat = *n;
                *n = n.saturating_add(1);
                ListRow::Item(row)
            }));
        }
    }
    rows
}

fn is_zero(n: &u16) -> bool {
    *n == 0
}

/// Collapsed group labels of the selected folder (conversation state entries left out).
fn collapsed_labels(s: &Session) -> Vec<String> {
    s.view
        .collapsed_groups
        .get(&s.selection.folder)
        .map(|v| v.iter().filter(|l| is_group_label(l)).cloned().collect())
        .unwrap_or_default()
}

/// Collapse state is stored by header label; the View commands (Collapse This Group / Collapse All Groups)
/// store their own per-item labels, so honour those too.
fn group_collapsed(
    s: &Session,
    vs: &ViewSettings,
    collapsed: &[String],
    label: &str,
    list: &[&MailItem],
) -> bool {
    !collapsed.is_empty()
        && (collapsed.iter().any(|c| c == label)
            // The pinned and top-results groups go by their own label only (their items belong to other
            // groups too).
            // Arrange by Categories lists a message under each of its categories: only the header's own label
            // collapses it.
            || vs.arrange_by != ArrangeBy::Categories
                && label != crate::cmd::view::mail::PINNED_LABEL && label != TOP_RESULTS_LABEL && list.iter().any(|m| {
                let other = crate::cmd::view::group_label(s, m, vs.arrange_by);
                collapsed.contains(&other)
            }))
}

// ---------------------------------------------------------------------------------------------------------
// Conversation view (`plan/reference/reply-forward.md` › Conversation view, `message-list.md` › Views)

/// One conversation of the message list in conversation view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListConversation {
    /// Conversation key (`thread::Conversation::key`).
    pub key: u64,
    /// Subject without RE:/FW: prefixes.
    pub topic: String,
    /// Header message: the newest member in the view (selecting the header selects it).
    pub header: ItemId,
    /// Member the conversation sorts and groups by: the newest member overall when arranged by date (the
    /// conversation sorts by its newest message), else the header message.
    pub sort_item: ItemId,
    /// Members in display order with their indent depth (1 = directly under the header): newest first, or
    /// the reply tree in the Classic Indented View.
    pub items: Vec<(ItemId, usize)>,
    /// Members from other folders (Show Messages from Other Folders).
    pub other: BTreeSet<ItemId>,
    /// Time of the newest member.
    pub latest: DateTime,
    pub unread: usize,
    /// The child rows are shown.
    pub expanded: bool,
    /// The focus or a selected item is in this conversation.
    pub selected: bool,
}

impl ListConversation {
    /// Members that belong to the view (header first, then list order), i.e. not from other folders.
    pub fn view_members(&self) -> Vec<ItemId> {
        let mut out = vec![self.header];
        out.extend(
            self.items.iter().map(|(i, _)| *i).filter(|i| *i != self.header && !self.other.contains(i)),
        );
        out
    }

    pub fn contains(&self, id: ItemId) -> bool {
        self.items.iter().any(|(i, _)| *i == id)
    }

    /// More than one message: drawn as a header row with child rows (a single message is a plain row).
    pub fn is_thread(&self) -> bool {
        self.items.len() > 1
    }
}

/// Conversation Settings of `folder`.
pub fn conversation_settings(s: &Session, folder: FolderId) -> ConversationSettings {
    conversation_state(s, folder).settings
}

/// Conversations of the selected folder's list in list order (empty when Show as Conversations is off).
pub fn list_conversations(s: &Session) -> Vec<ListConversation> {
    let (vs, items) = view_items(s);
    if !vs.conversations {
        return Vec::new();
    }
    build_conversations(s, &vs, items)
}

/// The list conversation holding message `id` (conversation view only).
pub fn conversation_of(s: &Session, id: ItemId) -> Option<ListConversation> {
    list_conversations(s).into_iter().find(|c| c.contains(id))
}

/// Messages of other mail folders of the selected folder's account (Deleted Items and Junk Email left out)
/// that may join a conversation of `items`: same topic, or linked through a message id.
fn other_folder_items<'a>(s: &'a Session, items: &[&MailItem]) -> Vec<&'a MailItem> {
    let mb = &s.mailbox;
    let folder = s.selection.folder;
    let Some(account) = mb.folder_account(folder) else {
        return Vec::new();
    };
    let topics: HashSet<String> =
        items.iter().map(|m| conversation_topic(&m.subject)).filter(|t| !t.is_empty()).collect();
    let ids: HashSet<&str> = items
        .iter()
        .flat_map(|m| m.message_id.iter().chain(m.in_reply_to.iter()).chain(m.references.iter()))
        .map(|i| norm_id(i))
        .filter(|i| !i.is_empty())
        .collect();
    if topics.is_empty() && ids.is_empty() {
        return Vec::new();
    }
    let excluded: Vec<FolderId> = [FolderRole::DeletedItems, FolderRole::JunkEmail]
        .into_iter()
        .filter_map(|r| mb.role_folder(account, r))
        .collect();
    let allowed: BTreeSet<FolderId> = mb
        .folders
        .values()
        .filter(|f| f.account == account && f.kind == FolderKind::Mail && f.id != folder)
        .filter(|f| !excluded.iter().any(|x| mb.is_in_subtree(f.id, *x)))
        .map(|f| f.id)
        .collect();
    mb.mail
        .values()
        .filter(|m| allowed.contains(&m.folder))
        .filter(|m| {
            m.message_id
                .iter()
                .chain(m.in_reply_to.iter())
                .chain(m.references.iter())
                .any(|i| ids.contains(norm_id(i)))
                || topics.contains(&conversation_topic(&m.subject))
        })
        .collect()
}

/// Threads the view's `items` (plus messages from other folders when that setting is on and no search is
/// active) into list conversations, ordered by the view's sort applied to each conversation's sort item.
fn build_conversations(s: &Session, vs: &ViewSettings, items: Vec<&MailItem>) -> Vec<ListConversation> {
    let folder = s.selection.folder;
    let state = conversation_state(s, folder);
    let in_view: HashSet<ItemId> = items.iter().map(|m| m.id).collect();
    let mut pool = items;
    if state.settings.from_other_folders && s.view.search.trim().is_empty() {
        let extra = other_folder_items(s, &pool);
        pool.extend(extra);
    }
    let focus = s.selection.focus;
    let mut out: Vec<ListConversation> = Vec::new();
    for t in mailcraft_model::thread::thread(&pool) {
        let Some(header) = t.items.iter().copied().find(|i| in_view.contains(i)) else { continue };
        let sort_item = if vs.arrange_by == ArrangeBy::Date {
            t.items.first().copied().unwrap_or(header)
        } else {
            header
        };
        let items: Vec<(ItemId, usize)> = if state.settings.classic_indent {
            let members: Vec<&MailItem> = t.items.iter().filter_map(|i| s.mailbox.mail.get(i)).collect();
            reply_tree(&members).into_iter().map(|(i, d)| (i, d.saturating_add(1))).collect()
        } else {
            t.items.iter().map(|i| (*i, 1)).collect()
        };
        let other: BTreeSet<ItemId> = t.items.iter().copied().filter(|i| !in_view.contains(i)).collect();
        let focus_in = focus.is_some_and(|f| t.items.contains(&f));
        let selected = focus_in || s.selection.items.iter().any(|i| t.items.contains(i));
        // The selected conversation expands by itself (unless collapsed by hand while its message kept the
        // focus); conversations expanded by hand stay open; Always Expand Selected Conversation overrides a
        // manual collapse of the selected conversation.
        let expanded = if focus_in && state.settings.always_expand {
            true
        } else if state.collapsed.is_some_and(|(k, i)| k == t.key && focus == Some(i)) {
            false
        } else {
            state.expanded.contains(&t.key) || selected
        };
        out.push(ListConversation {
            key: t.key,
            topic: t.topic,
            header,
            sort_item,
            items,
            other,
            latest: t.latest,
            unread: t.unread,
            expanded,
            selected,
        });
    }
    let mut reps: Vec<&MailItem> = out.iter().filter_map(|c| s.mailbox.mail.get(&c.sort_item)).collect();
    sort_items(s, vs, &mut reps);
    let pos: HashMap<ItemId, usize> = reps.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
    out.sort_by_key(|c| pos.get(&c.sort_item).copied().unwrap_or(usize::MAX));
    out
}

/// Rows of the conversation view: (date) groups of conversations, each a header row followed by its child
/// rows when expanded; a conversation of one message is a plain row.
fn conversation_rows(s: &Session, vs: &ViewSettings, items: Vec<&MailItem>) -> Vec<ListRow> {
    let settings = conversation_state(s, s.selection.folder).settings;
    let convs = build_conversations(s, vs, items);
    let mut rows = Vec::new();
    if !vs.show_in_groups {
        for c in &convs {
            push_conversation(&mut rows, s, c, &settings);
        }
        return rows;
    }
    let by_sort: HashMap<ItemId, &ListConversation> = convs.iter().map(|c| (c.sort_item, c)).collect();
    let reps: Vec<&MailItem> = convs.iter().filter_map(|c| s.mailbox.mail.get(&c.sort_item)).collect();
    let collapsed = collapsed_labels(s);
    let fields = manual_fields(s);
    if !fields.is_empty() {
        let groups = nested(s, &fields, reps, &BTreeSet::new());
        let members = |list: &[&MailItem]| -> usize {
            list.iter()
                .filter_map(|m| by_sort.get(&m.id))
                .map(|c| c.items.len().saturating_sub(c.other.len()))
                .sum()
        };
        push_nested(
            s,
            &mut rows,
            &groups,
            |rows, list| {
                for c in list.iter().filter_map(|m| by_sort.get(&m.id)) {
                    push_conversation(rows, s, c, &settings);
                }
            },
            members,
        );
        return rows;
    }
    for (label, list) in grouped(s, vs.arrange_by, reps, &BTreeSet::new()) {
        let group: Vec<&ListConversation> = list.iter().filter_map(|m| by_sort.get(&m.id).copied()).collect();
        let is_collapsed = group_collapsed(s, vs, &collapsed, &label, &list);
        let count = group.iter().map(|c| c.items.len().saturating_sub(c.other.len())).sum();
        rows.push(ListRow::Group { label, count, collapsed: is_collapsed, depth: 0 });
        if !is_collapsed {
            for c in group {
                push_conversation(&mut rows, s, c, &settings);
            }
        }
    }
    rows
}

fn push_conversation(
    rows: &mut Vec<ListRow>,
    s: &Session,
    c: &ListConversation,
    settings: &ConversationSettings,
) {
    let Some(head) = s.mailbox.mail.get(&c.header) else { return };
    if !c.is_thread() {
        rows.push(ListRow::Item(item_row(s, head)));
        return;
    }
    rows.push(ListRow::Item(conversation_header_row(s, c, head, settings)));
    if !c.expanded {
        return;
    }
    for (id, depth) in &c.items {
        let Some(m) = s.mailbox.mail.get(id) else { continue };
        let mut row = item_row(s, m);
        // Child rows name the sender, also for your own replies from Sent Items.
        let sender = sender_name(m);
        if !sender.is_empty() {
            row.who = sender;
        }
        row.conversation_depth = u8::try_from(*depth).unwrap_or(u8::MAX);
        let other_folder = if c.other.contains(id) {
            Some(s.mailbox.folder(m.folder).map(|f| f.name.clone()).unwrap_or_default())
        } else {
            None
        };
        if let Some(name) = &other_folder {
            row.accessible.push_str(&format!("Folder {name}, "));
        }
        row.conversation = Some(ConversationRow {
            key: c.key,
            header: false,
            expanded: true,
            count: c.items.len(),
            unread: c.unread,
            other_folder,
            senders_above_subject: settings.senders_above_subject,
            contains_selection: c.selected,
        });
        rows.push(ListRow::Item(row));
    }
}

/// Header row of a conversation: subject without prefixes, senders (newest first), latest time, unread
/// count, and attachments / flags / categories / importance rolled up from the members.
fn conversation_header_row(
    s: &Session,
    c: &ListConversation,
    head: &MailItem,
    settings: &ConversationSettings,
) -> ItemRow {
    let mut members: Vec<&MailItem> = c.items.iter().filter_map(|(i, _)| s.mailbox.mail.get(i)).collect();
    members.sort_by(|a, b| b.received.unix.cmp(&a.received.unix).then(b.id.cmp(&a.id)));
    let mut senders: Vec<String> = Vec::new();
    for m in &members {
        let n = sender_name(m);
        if !n.is_empty() && !senders.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
            senders.push(n);
        }
    }
    let mut categories: Vec<String> = Vec::new();
    for m in &members {
        for cat in &m.categories {
            if !categories.iter().any(|x| x.eq_ignore_ascii_case(cat)) {
                categories.push(cat.clone());
            }
        }
    }
    let mut row = item_row(s, head);
    row.who = if senders.is_empty() { "(unknown sender)".to_owned() } else { senders.join("; ") };
    let topic = c.topic.trim();
    row.subject = if topic.is_empty() { "(no subject)".to_owned() } else { topic.to_owned() };
    row.time = ListTime::format(c.latest, s.now(), s.tz_offset_min);
    row.unread = c.unread > 0;
    row.selected = c.selected && !c.expanded;
    row.draft_marker = members.iter().any(|m| s.composes.values().any(|cm| cm.source == Some(m.id)));
    row.has_attachments = members.iter().any(|m| m.has_attachments());
    row.importance_high = members.iter().any(|m| m.importance == Importance::High);
    row.importance_low = !row.importance_high && members.iter().all(|m| m.importance == Importance::Low);
    row.flagged = members.iter().any(|m| m.is_flagged());
    row.flag_complete = !row.flagged && members.iter().any(|m| matches!(m.flag, FlagState::Complete { .. }));
    row.categories = categories.iter().map(|n| category_hex(s, n)).collect();
    row.conversation_depth = 0;
    let n = c.items.len();
    let state = if c.expanded { "Expanded" } else { "Collapsed" };
    let mut acc = format!("Conversation, From {}, Subject {}, Received {}, ", row.who, row.subject, row.time);
    acc.push_str(&format!("{n} messages, {} unread, ", c.unread));
    if row.has_attachments {
        acc.push_str("With Attachments, ");
    }
    if row.flagged {
        acc.push_str("Flagged, ");
    }
    if !categories.is_empty() {
        acc.push_str(&format!("Categories {}, ", categories.join(", ")));
    }
    acc.push_str(&format!("{state}, "));
    row.accessible = acc;
    row.conversation = Some(ConversationRow {
        key: c.key,
        header: true,
        expanded: c.expanded,
        count: n,
        unread: c.unread,
        other_folder: None,
        senders_above_subject: settings.senders_above_subject,
        contains_selection: c.selected,
    });
    row
}

/// Status bar counts: (items in view, unread in view).
pub fn status_counts(s: &Session) -> (usize, usize) {
    let (_, items) = view_items(s);
    let unread = items.iter().filter(|m| !m.read).count();
    (items.len(), unread)
}

/// Status bar › Unseen Items in View: items in the current view that arrived after the previous time the
/// folder was opened (0 on a first visit).
pub fn unseen_in_view(s: &Session) -> usize {
    let Some((prev, _)) = s.view.folder_visits.get(&s.selection.folder).copied() else { return 0 };
    let (_, items) = view_items(s);
    items.iter().filter(|m| m.id.0 > prev).count()
}

/// Reading pane header model for an item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadingHeader {
    pub subject: String,
    pub from: String,
    pub from_initials: String,
    /// Persona colour index (stable per sender address).
    pub persona_color: u8,
    pub to: String,
    pub cc: String,
    /// "Thu 10/8/2026 7:30 AM".
    pub date: String,
    /// Info bar lines ("This message was sent with High importance.", follow-up text, "You replied to this
    /// message on …", "You forwarded this message on …").
    pub info: Vec<String>,
    pub attachments: Vec<(String, String)>,
    pub categories: Vec<String>,
    /// The contact whose picture stands in for the persona circle: a contact with a picture and the
    /// sender's address, while People › "Show user photographs when available" is on.
    #[serde(default)]
    pub photo: Option<ItemId>,
}

/// People › "Show user photographs when available": the contact with a picture for `addr` (any of its
/// e-mail addresses, case-insensitive), `None` when the option is off or no such contact exists.
pub fn photo_contact(s: &Session, addr: &str) -> Option<ItemId> {
    let addr = addr.trim();
    if addr.is_empty() || !s.settings.bool("people.photos") {
        return None;
    }
    s.mailbox
        .contacts
        .values()
        .filter(|c| c.picture.is_some())
        .find(|c| c.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(addr)))
        .map(|c| c.id)
}

/// Calendar › "Enable an alternate calendar": the date `day` (days since 1970-01-01) falls on in the chosen
/// alternate calendar (`calendar.alternateCalendarType`), `None` while the option is off.
pub fn alternate_date(s: &Session, day: i64) -> Option<mailcraft_model::altcal::AltDate> {
    if !s.settings.bool("calendar.alternateCalendar") {
        return None;
    }
    let cal = match s.settings.text("calendar.alternateCalendarType").as_str() {
        "hebrew" => mailcraft_model::altcal::AltCalendar::Hebrew,
        _ => mailcraft_model::altcal::AltCalendar::Hijri,
    };
    Some(mailcraft_model::altcal::to_alt(cal, day))
}

/// Initials for the persona circle: first letters of the first and last words of the name, else the first
/// letter of the address.
pub fn initials(name: Option<&str>, addr: &str) -> String {
    let words: Vec<&str> = name
        .unwrap_or("")
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| w.chars().next().is_some_and(char::is_alphanumeric))
        .collect();
    let first_char =
        |w: &str| w.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match (words.first(), words.last()) {
        (Some(a), Some(b)) if words.len() > 1 => format!("{}{}", first_char(a), first_char(b)),
        (Some(a), _) => first_char(a),
        _ => {
            addr.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default()
        }
    }
}

/// Persona colour index (0..16), stable per address.
pub fn persona_color(addr: &str) -> u8 {
    let h = mailcraft_model::thread::fnv1a(addr.trim().to_lowercase().as_bytes());
    u8::try_from(h % 16).unwrap_or(0)
}

/// Info bar lines for an item (importance, sensitivity, follow-up flag, last response, unsent draft).
pub fn info_lines(s: &Session, m: &MailItem) -> Vec<String> {
    let tz = s.tz_offset_min;
    let mut info = Vec::new();
    if m.is_draft {
        info.push("This message has not been sent.".to_owned());
    }
    match &m.flag {
        FlagState::Flagged { label, start, due, .. } => {
            let label = if label.trim().is_empty() { "Follow up" } else { label.trim() };
            let mut line = format!("{label}.");
            if let Some(d) = start {
                line.push_str(&format!(" Start by {}.", ListTime::long_date(*d, tz)));
            }
            if let Some(d) = due {
                line.push_str(&format!(" Due by {}.", ListTime::long_date(*d, tz)));
            }
            info.push(line);
        }
        FlagState::Complete { completed: Some(d) } => {
            info.push(format!("Completed on {}.", ListTime::long_date(*d, tz)));
        }
        FlagState::Complete { completed: None } => info.push("Completed.".to_owned()),
        FlagState::None => {}
    }
    match m.importance {
        Importance::High => info.push("This message was sent with High importance.".to_owned()),
        Importance::Low => info.push("This message was sent with Low importance.".to_owned()),
        Importance::Normal => {}
    }
    match m.sensitivity {
        mailcraft_model::Sensitivity::Normal => {}
        mailcraft_model::Sensitivity::Personal => info.push("This message is marked Personal.".to_owned()),
        mailcraft_model::Sensitivity::Private => info.push("This message is marked Private.".to_owned()),
        mailcraft_model::Sensitivity::Confidential => {
            info.push("This message is marked Confidential.".to_owned());
        }
    }
    match m.response {
        ResponseState::Replied(d) | ResponseState::RepliedAll(d) => {
            info.push(format!("You replied to this message on {}.", ListTime::short_date_time(d, tz)));
        }
        ResponseState::Forwarded(d) => {
            info.push(format!("You forwarded this message on {}.", ListTime::short_date_time(d, tz)));
        }
        ResponseState::None => {}
    }
    info
}

/// Most inline pictures handed over per message.
const MAX_INLINE_PICTURES: usize = 32;

/// The inline pictures of a message as (`cid:<Content-ID>`, bytes): image parts with a Content-ID, which an
/// HTML body shows with `<img src="cid:…">`. Empty when the message has none or its source is unavailable;
/// the parse only runs for messages whose attachment list names a Content-ID.
pub fn inline_pictures(s: &Session, id: ItemId) -> Vec<(String, Vec<u8>)> {
    // S/MIME messages show the pictures of the opened message.
    let Some(m) = crate::smime::display_item(s, id) else { return Vec::new() };
    if !m.attachments.iter().any(|a| a.content_id.is_some()) {
        return Vec::new();
    }
    let raw = crate::smime::open_source(s, &m);
    let Ok(msg) = mailcraft_mime::parse(&raw) else { return Vec::new() };
    msg.attachments()
        .into_iter()
        .filter(|a| a.mime_type.get(..6).is_some_and(|t| t.eq_ignore_ascii_case("image/")))
        .filter_map(|a| Some((format!("cid:{}", a.content_id?), a.data)))
        .take(MAX_INLINE_PICTURES)
        .collect()
}

pub fn reading_header(s: &Session, id: ItemId) -> Option<ReadingHeader> {
    // S/MIME messages list the attachments of the opened message (not the signature or the encrypted part).
    let shown = crate::smime::display_item(s, id)?;
    let m = shown.as_ref();
    let from_box = m.from.as_ref().or(m.sender.as_ref());
    let (from, from_initials, persona) = match from_box {
        Some(f) => (f.to_display_string(), initials(f.name.as_deref(), &f.addr), persona_color(&f.addr)),
        None => (String::new(), String::new(), 0),
    };
    let tz = s.tz_offset_min;
    let date = if m.is_draft { String::new() } else { ListTime::header(m.sent.unwrap_or(m.received), tz) };
    Some(ReadingHeader {
        subject: m.subject.clone(),
        from,
        from_initials,
        persona_color: persona,
        to: names(&m.to),
        cc: names(&m.cc),
        date,
        info: info_lines(s, m),
        attachments: m
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| (a.filename.clone(), size_text(a.size)))
            .collect(),
        categories: m.categories.clone(),
        photo: from_box.and_then(|f| photo_contact(s, &f.addr)),
    })
}

/// Attachment size chip text: "512 bytes", "12 KB", "1.4 MB".
pub fn size_text(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} bytes")
    } else if bytes < MB {
        format!("{} KB", bytes.div_ceil(KB))
    } else {
        let tenths = bytes.saturating_mul(10).div_ceil(MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

// ---------------------------------------------------------------------------------------------------------
// Recipient Auto-Complete (added in commit 6)
// ---------------------------------------------------------------------------------------------------------

/// Where an Auto-Complete suggestion comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionSource {
    /// A contact in People.
    Contact,
    /// The Auto-Complete list (recipients of sent mail).
    Recent,
    /// The user's own accounts (the local directory).
    Directory,
}

impl SuggestionSource {
    /// Lower-case name ("contact", "recent", "directory").
    pub fn label(self) -> &'static str {
        match self {
            SuggestionSource::Contact => "contact",
            SuggestionSource::Recent => "recent",
            SuggestionSource::Directory => "directory",
        }
    }
}

/// One Auto-Complete suggestion for a recipient field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Display name (the address when there is none).
    pub display: String,
    pub address: String,
    pub source: SuggestionSource,
}

impl Suggestion {
    /// The entry written into the field when the suggestion is picked ("Name <address>").
    pub fn entry(&self) -> String {
        let name = (!self.display.is_empty() && !self.display.eq_ignore_ascii_case(&self.address))
            .then_some(self.display.as_str());
        crate::cmd::compose::field_mailbox(&mailcraft_model::Address::new(name, &self.address))
    }
}

/// Whether `text` or one of its words starts with `prefix` (both compared lower-case).
fn word_prefix(text: &str, prefix: &str) -> bool {
    let t = text.to_lowercase();
    t.starts_with(prefix)
        || t.split(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '.' | '-' | '_' | '@' | '(' | ')' | '"' | '\'')
        })
        .any(|w| !w.is_empty() && w.starts_with(prefix))
}

/// Auto-Complete suggestions for what is typed in a recipient field: the Auto-Complete list first (most
/// recently used first), then contacts (every address of a contact whose name, file-as, company or address
/// matches; by name), then the user's own accounts. Matching is a case-insensitive prefix of the whole
/// name/address or of any word in it. Only the entry after the last `;` counts. One row per address.
pub fn address_suggestions(s: &Session, prefix: &str, limit: usize) -> Vec<Suggestion> {
    let typed = prefix.rsplit(';').next().unwrap_or(prefix).trim().to_lowercase();
    if typed.is_empty() || limit == 0 {
        return Vec::new();
    }
    let typed: String = typed.chars().take(256).collect();
    let mut out: Vec<Suggestion> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut push = |out: &mut Vec<Suggestion>, display: &str, address: &str, source: SuggestionSource| {
        let address = address.trim();
        if address.is_empty() || !seen.insert(address.to_lowercase()) {
            return;
        }
        let display = if display.trim().is_empty() { address } else { display.trim() };
        out.push(Suggestion { display: display.to_owned(), address: address.to_owned(), source });
    };
    // Contact names by address: a recent recipient written without a name shows its contact's name.
    let mut contact_names: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if s.mailbox.recent_recipients.iter().any(|r| r.name.trim().is_empty()) {
        for c in s.mailbox.contacts.values() {
            let full = c.full_name();
            let shown = if full.trim().is_empty() { c.file_as.trim().to_owned() } else { full };
            if shown.trim().is_empty() {
                continue;
            }
            for e in &c.emails {
                contact_names.entry(e.trim().to_lowercase()).or_insert_with(|| shown.clone());
            }
        }
    }
    for r in &s.mailbox.recent_recipients {
        if out.len() >= limit {
            return out;
        }
        let name = match r.name.trim() {
            "" => contact_names.get(&r.address.trim().to_lowercase()).cloned().unwrap_or_default(),
            n => n.to_owned(),
        };
        if word_prefix(&name, &typed) || word_prefix(&r.address, &typed) {
            push(&mut out, &name, &r.address, SuggestionSource::Recent);
        }
    }
    let mut contacts: Vec<(String, String)> = Vec::new();
    let books = crate::cmd::people::BookFilter::new(s);
    for c in s.mailbox.contacts.values().filter(|c| books.shows(c.folder)) {
        let full = c.full_name();
        let shown = if full.trim().is_empty() { c.file_as.trim().to_owned() } else { full.clone() };
        let name_hit = [&full, &c.file_as, &c.first_name, &c.last_name, &c.company]
            .iter()
            .any(|n| !n.trim().is_empty() && word_prefix(n, &typed));
        for e in c.emails.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            if name_hit || word_prefix(e, &typed) {
                contacts.push((shown.clone(), e.to_owned()));
            }
        }
    }
    contacts.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.1.cmp(&b.1)));
    for (name, addr) in &contacts {
        if out.len() >= limit {
            return out;
        }
        push(&mut out, name, addr, SuggestionSource::Contact);
    }
    for a in &s.mailbox.accounts {
        if out.len() >= limit {
            return out;
        }
        let name =
            if a.sender_name.trim().is_empty() { a.display_name.as_str() } else { a.sender_name.as_str() };
        if !a.address.trim().is_empty() && (word_prefix(name, &typed) || word_prefix(&a.address, &typed)) {
            push(&mut out, name, &a.address, SuggestionSource::Directory);
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    use mailcraft_model::{Contact, ItemId, Mailbox};

    #[test]
    fn suggestions_from_recent_contacts_and_accounts() {
        let mut mb = Mailbox::new_local("Local");
        mb.add_account(mailcraft_model::Account {
            kind: mailcraft_model::AccountKind::Imap,
            address: "me@example.com".into(),
            sender_name: "Me Example".into(),
            ..Default::default()
        });
        let id = mb.alloc_id();
        mb.contacts.insert(
            ItemId(id),
            Contact {
                id: ItemId(id),
                first_name: "Alice".into(),
                last_name: "Rivera".into(),
                company: "Contoso Labs".into(),
                emails: vec!["alice@example.com".into(), "a.rivera@example.org".into()],
                ..Default::default()
            },
        );
        let now = mailcraft_model::DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0).unwrap();
        mb.record_recipients(&[("Alan Ng".into(), "alan@example.net".into())], now);
        let s = Session::new(mb);
        let got = address_suggestions(&s, "al", 10);
        let addrs: Vec<&str> = got.iter().map(|g| g.address.as_str()).collect();
        assert_eq!(addrs, ["alan@example.net", "a.rivera@example.org", "alice@example.com"]);
        assert_eq!(got[0].source, SuggestionSource::Recent);
        assert_eq!(got[1].source, SuggestionSource::Contact);
        assert_eq!(got[1].entry(), "Alice Rivera <a.rivera@example.org>");
        // Company and surname words match; only the entry after the last ';' counts.
        assert_eq!(address_suggestions(&s, "bob@x.org; conto", 10).len(), 2);
        assert_eq!(address_suggestions(&s, "RIVERA", 10).len(), 2);
        assert_eq!(address_suggestions(&s, "me", 10)[0].source, SuggestionSource::Directory);
        assert_eq!(address_suggestions(&s, "al", 1).len(), 1);
        assert!(address_suggestions(&s, "  ", 10).is_empty());
        assert!(address_suggestions(&s, "zzz", 10).is_empty());
        assert!(address_suggestions(&s, &"\u{1F600}".repeat(10_000), 10).is_empty());
    }

    #[test]
    fn recent_recipients_without_a_name_take_the_contact_name() {
        let mut mb = Mailbox::new_local("Local");
        let id = mb.alloc_id();
        mb.contacts.insert(
            ItemId(id),
            Contact {
                id: ItemId(id),
                first_name: "Alice".into(),
                last_name: "Rivera".into(),
                emails: vec!["alice@example.com".into()],
                ..Default::default()
            },
        );
        let now = mailcraft_model::DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0).unwrap();
        mb.record_recipients(&[(String::new(), "ALICE@example.com".into())], now);
        let s = Session::new(mb);
        let got = address_suggestions(&s, "riv", 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].source, SuggestionSource::Recent);
        assert_eq!(got[0].entry(), "Alice Rivera <ALICE@example.com>");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{SAMPLE_USER_ADDR, sample_session};

    fn groups(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                ListRow::Group { label, .. } => Some(label.clone()),
                ListRow::Item(_) => None,
            })
            .collect()
    }

    fn items(rows: &[ListRow]) -> Vec<&ItemRow> {
        rows.iter()
            .filter_map(|r| match r {
                ListRow::Item(i) => Some(i),
                ListRow::Group { .. } => None,
            })
            .collect()
    }

    fn set_view(s: &mut Session, f: impl FnOnce(&mut ViewSettings)) {
        let folder = s.selection.folder;
        let mut vs = view_settings(s, folder);
        f(&mut vs);
        s.mailbox.views.insert(folder, vs);
    }

    /// The sample Inbox plus five "budget" messages; the oldest is by far the most relevant.
    fn budget_search() -> (Session, ItemId) {
        let mut s = sample_session();
        let inbox = s.selection.folder;
        let now = s.now();
        let mut best = ItemId(0);
        for n in 0..5i64 {
            let text = if n == 0 { "budget budget budget: the budget review" } else { "see the budget" };
            let m = MailItem {
                folder: inbox,
                subject: if n == 0 { "Budget budget".into() } else { format!("Note {n}") },
                body: mailcraft_model::Body {
                    format: mailcraft_model::BodyFormat::PlainText,
                    text: Some(text.into()),
                    html: None,
                },
                received: DateTime::from_unix(now.unix - 86_400 * (10 - n), 0),
                read: true,
                ..Default::default()
            };
            let id = s.mailbox.add_mail(m);
            if n == 0 {
                best = id;
            }
        }
        s.search.invalidate();
        s.run("search.query", &serde_json::json!({"text": "budget"})).unwrap();
        (s, best)
    }

    #[test]
    fn most_relevant_results_lead_a_search_when_the_option_is_on() {
        let (mut s, best) = budget_search();
        // On (default): the best hits in their own group first.
        let rows = list_rows(&s);
        assert_eq!(groups(&rows).first().map(String::as_str), Some(TOP_RESULTS_LABEL));
        assert_eq!(items(&rows).first().map(|r| r.id), Some(best));
        assert_eq!(top_results(&s).len(), TOP_RESULTS);
        assert_eq!(group_items(&s, TOP_RESULTS_LABEL).len(), TOP_RESULTS);
        // Off: date order only (the oldest, best match comes last).
        s.run("options.set", &serde_json::json!({"key": "search.relevance", "value": false})).unwrap();
        let rows = list_rows(&s);
        assert!(!groups(&rows).iter().any(|g| g == TOP_RESULTS_LABEL));
        assert_eq!(items(&rows).last().map(|r| r.id), Some(best));
        assert!(top_results(&s).is_empty());
    }

    #[test]
    fn incomplete_search_notice_follows_the_option() {
        let (mut s, best) = budget_search();
        assert_eq!(search_notice(&s), None, "every message has its text");
        // A server account's message downloaded as headers only.
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.kind = mailcraft_model::AccountKind::Imap;
        }
        if let Some(m) = s.mailbox.mail.get_mut(&best) {
            m.body = mailcraft_model::Body::default();
            m.raw = None;
        }
        let notice = search_notice(&s).unwrap();
        assert!(notice.contains("1 message here was downloaded as headers only"), "{notice}");
        s.run("options.set", &serde_json::json!({"key": "search.notifyIncomplete", "value": false})).unwrap();
        assert_eq!(search_notice(&s), None);
        s.run("options.set", &serde_json::json!({"key": "search.notifyIncomplete", "value": true})).unwrap();
        s.run("search.close", &serde_json::json!({})).unwrap();
        assert_eq!(search_notice(&s), None, "no search, no notice");
    }

    #[test]
    fn alternate_dates_follow_the_calendar_options() {
        let mut s = sample_session();
        let day = mailcraft_mime::date::days_from_civil(2026, 10, 8);
        assert_eq!(alternate_date(&s, day), None);
        s.run("options.set", &serde_json::json!({"key": "calendar.alternateCalendar", "value": true}))
            .unwrap();
        assert_eq!(alternate_date(&s, day).map(|d| d.long()).as_deref(), Some("25 Rabi\u{2bf} II 1448"));
        s.run(
            "options.set",
            &serde_json::json!({"key": "calendar.alternateCalendarType", "value": "hebrew"}),
        )
        .unwrap();
        assert_eq!(alternate_date(&s, day).map(|d| d.long()).as_deref(), Some("27 Tishri 5787"));
    }

    #[test]
    fn sender_photos_show_only_while_the_option_is_on() {
        let mut s = sample_session();
        let id = s.selection.focus.or_else(|| visible_item_ids(&s).first().copied()).unwrap();
        let addr = s.mailbox.mail[&id].from.as_ref().unwrap().addr.clone();
        assert_eq!(reading_header(&s, id).unwrap().photo, None, "no contact picture yet");
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0".to_vec();
        let cid = ItemId(s.mailbox.alloc_id());
        let contacts =
            s.mailbox.folders.values().find(|f| f.kind == FolderKind::Contacts).map(|f| f.id).unwrap();
        let c = mailcraft_model::Contact {
            id: cid,
            folder: contacts,
            emails: vec![addr.to_uppercase()],
            ..Default::default()
        };
        s.mailbox.contacts.insert(cid, c);
        assert_eq!(reading_header(&s, id).unwrap().photo, None, "a contact without a picture");
        s.mailbox.contacts.get_mut(&cid).unwrap().picture =
            mailcraft_model::contact::ContactPicture::sniff(&png);
        assert_eq!(reading_header(&s, id).unwrap().photo, Some(cid));
        s.run("options.set", &serde_json::json!({"key": "people.photos", "value": false})).unwrap();
        assert_eq!(reading_header(&s, id).unwrap().photo, None);
        assert_eq!(photo_contact(&s, ""), None);
    }

    #[test]
    fn inline_pictures_come_from_the_message_source() {
        let mut s = sample_session();
        let png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
        let b = mailcraft_mime::MessageBuilder {
            from: Some(mailcraft_mime::Mailbox::new(None, "ann@example.com")),
            subject: "Logo".into(),
            text: Some("Logo".into()),
            html: Some("<p><img src=\"cid:logo@example.com\"></p>".into()),
            attachments: vec![
                mailcraft_mime::AttachmentSpec {
                    filename: "logo.png".into(),
                    mime_type: "image/png".into(),
                    data: png.clone(),
                    content_id: Some("logo@example.com".into()),
                },
                mailcraft_mime::AttachmentSpec {
                    filename: "notes.txt".into(),
                    mime_type: "text/plain".into(),
                    data: b"plain".to_vec(),
                    content_id: Some("notes@example.com".into()),
                },
            ],
            ..Default::default()
        };
        let raw = b.build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, s.now());
        item.raw = Some(raw);
        item.folder = s.selection.folder;
        let id = s.mailbox.add_mail(item);
        assert_eq!(inline_pictures(&s, id), vec![("cid:logo@example.com".to_owned(), png)]);
        // Messages without Content-IDs, and unknown ids, have none.
        let plain = *s.mailbox.mail.keys().find(|k| **k != id).unwrap();
        assert!(inline_pictures(&s, plain).is_empty());
        assert!(inline_pictures(&s, ItemId(u64::MAX)).is_empty());
    }

    /// Top-level folder names of the first account's tree in the folder pane.
    fn first_account_roots(s: &Session) -> Vec<String> {
        folder_rows(s)
            .into_iter()
            .skip_while(|r| r.kind != FolderRowKind::AccountRoot)
            .skip(1)
            .take_while(|r| r.kind != FolderRowKind::AccountRoot)
            .filter(|r| r.kind == FolderRowKind::Folder && r.depth == 1)
            .map(|r| r.name)
            .collect()
    }

    #[test]
    fn folder_tree_follows_show_all_folders_a_to_z() {
        let mut s = sample_session();
        let account = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let pane_names = |s: &Session| -> Vec<String> {
            crate::cmd::folder::pane_children(s, account, None)
                .into_iter()
                .filter_map(|id| s.mailbox.folder(id))
                .filter(|f| kind_visible(f.kind, s.view.module))
                .map(|f| f.name.clone())
                .collect()
        };
        let before = first_account_roots(&s);
        assert!(before.len() > 2, "{before:?}");
        assert_eq!(before, pane_names(&s));
        let r = s.run("folder.showAllAZ", &serde_json::json!({"on": true})).unwrap();
        let after = first_account_roots(&s);
        assert_eq!(after, pane_names(&s), "the tree uses the folder pane's order");
        if r["saved"] == true {
            // Default folders are sorted in with the rest.
            let mut sorted = after.clone();
            sorted.sort_by_key(|n| n.to_lowercase());
            assert_eq!(after, sorted);
            assert_ne!(after, before);
        } else {
            // The setting is not kept by this build: nothing changes.
            assert_eq!(after, before);
        }
    }

    #[test]
    fn inbox_date_groups_newest_first() {
        let s = sample_session();
        let rows = list_rows(&s);
        let g = groups(&rows);
        assert_eq!(g.first().map(String::as_str), Some("Today"));
        assert!(g.contains(&"Yesterday".to_owned()));
        assert!(g.contains(&"Last Week".to_owned()));
        assert!(g.contains(&"Older".to_owned()) || g.contains(&"Last Month".to_owned()));
        // Focused tab: no "Other" newsletters.
        let mb = &s.mailbox;
        for i in items(&rows) {
            assert_eq!(mb.mail[&i.id].focus, FocusClass::Focused);
        }
        let times: Vec<i64> = items(&rows).iter().map(|i| mb.mail[&i.id].received.unix).collect();
        assert!(times.windows(2).all(|w| w[0] >= w[1]), "newest on top");
        let (n, unread) = status_counts(&s);
        assert_eq!(n, items(&rows).len());
        assert!(unread > 0 && unread < n);
    }

    #[test]
    fn other_tab_and_focused_off() {
        let mut s = sample_session();
        s.view.focused_tab = false;
        let other = items(&list_rows(&s)).len();
        assert!(other >= 3);
        set_view(&mut s, |v| v.focused_inbox = false);
        let all = items(&list_rows(&s)).len();
        s.view.focused_tab = true;
        set_view(&mut s, |v| v.focused_inbox = true);
        let focused = items(&list_rows(&s)).len();
        assert_eq!(all, other + focused);
    }

    #[test]
    fn every_arrangement_groups_and_sorts() {
        let mut s = sample_session();
        set_view(&mut s, |v| v.focused_inbox = false);
        let total = status_counts(&s).0;
        for key in ArrangeBy::ALL {
            for order in [SortOrder::Ascending, SortOrder::Descending] {
                set_view(&mut s, |v| {
                    v.arrange_by = key;
                    v.order = order;
                });
                let rows = list_rows(&s);
                // Arrange by Categories lists a message once per category: count each message once.
                let once: HashSet<ItemId> = items(&rows).iter().map(|i| i.id).collect();
                assert_eq!(once.len(), total, "{key:?}");
                if key != ArrangeBy::Categories {
                    assert_eq!(items(&rows).len(), total, "{key:?}");
                }
                let g = groups(&rows);
                let mut uniq = g.clone();
                uniq.sort();
                uniq.dedup();
                assert_eq!(uniq.len(), g.len(), "groups are contiguous for {key:?}: {g:?}");
            }
        }
        set_view(&mut s, |v| {
            v.arrange_by = ArrangeBy::Size;
            v.order = SortOrder::Descending;
        });
        let g = groups(&list_rows(&s));
        assert_eq!(g.first().map(String::as_str), Some("Large (100 - 500 KB)"));
        assert!(g.contains(&"Tiny (< 10 KB)".to_owned()));
        set_view(&mut s, |v| v.arrange_by = ArrangeBy::Attachments);
        assert_eq!(groups(&list_rows(&s)), vec!["With Attachments", "Without Attachments"]);
        set_view(&mut s, |v| v.arrange_by = ArrangeBy::Importance);
        assert_eq!(groups(&list_rows(&s)), vec!["High", "Normal", "Low"]);
        set_view(&mut s, |v| v.arrange_by = ArrangeBy::FlagStatus);
        assert_eq!(groups(&list_rows(&s)), vec!["Flagged", "Completed", "Unflagged"]);
        set_view(&mut s, |v| {
            v.arrange_by = ArrangeBy::From;
            v.order = SortOrder::Ascending;
        });
        assert_eq!(groups(&list_rows(&s)).first().map(String::as_str), Some("Alice Rivera"));
        set_view(&mut s, |v| v.arrange_by = ArrangeBy::Subject);
        let g = groups(&list_rows(&s));
        assert!(g.contains(&"Project kickoff".to_owned()), "RE: joins its topic: {g:?}");
        set_view(&mut s, |v| v.show_in_groups = false);
        assert!(groups(&list_rows(&s)).is_empty());
    }

    #[test]
    fn filters() {
        let mut s = sample_session();
        set_view(&mut s, |v| {
            v.focused_inbox = false;
            v.filter = ListFilter::Unread;
        });
        let unread = items(&list_rows(&s)).iter().map(|i| i.unread).collect::<Vec<_>>();
        assert!(!unread.is_empty() && unread.iter().all(|u| *u));
        set_view(&mut s, |v| v.filter = ListFilter::Flagged);
        assert!(items(&list_rows(&s)).iter().all(|i| i.flagged));
        set_view(&mut s, |v| v.filter = ListFilter::Mentioned);
        let mentioned = list_rows(&s);
        assert_eq!(items(&mentioned).len(), 1);
        assert!(items(&mentioned)[0].subject.contains("Slides"));
        assert!(s.me.contains(&SAMPLE_USER_ADDR.to_owned()));
    }

    #[test]
    fn search_terms_and_scopes() {
        let mut s = sample_session();
        s.view.search = "kickoff".into();
        let n_folder = items(&list_rows(&s)).len();
        assert!(n_folder >= 2, "search ignores Focused/Other and finds the thread");
        s.view.search_scope = crate::SearchScope::AllMailboxes;
        assert!(items(&list_rows(&s)).len() > n_folder, "Sent Items reply found across folders");
        s.view.search = "from:alice subject:budget".into();
        let r = list_rows(&s);
        assert_eq!(items(&r).len(), 1);
        s.view.search = "hasattachments:yes".into();
        assert!(items(&list_rows(&s)).iter().all(|i| i.has_attachments));
        s.view.search = "to:\"Jordan Lee\" BADGE".into();
        assert_eq!(items(&list_rows(&s)).len(), 1);
        s.view.search = "zzzz-nothing".into();
        assert!(list_rows(&s).is_empty());
    }

    #[test]
    fn collapsed_groups_hide_items() {
        let mut s = sample_session();
        let folder = s.selection.folder;
        s.view.collapsed_groups.insert(folder, vec!["Today".into()]);
        let rows = list_rows(&s);
        match rows.first() {
            Some(ListRow::Group { label, collapsed, count, .. }) => {
                assert_eq!(label, "Today");
                assert!(*collapsed && *count > 0);
            }
            other => panic!("expected the Today group first, got {other:?}"),
        }
        assert!(matches!(rows.get(1), Some(ListRow::Group { .. })));
        assert_eq!(status_counts(&s).0, view_item_ids(&s).len());
        assert!(visible_item_ids(&s).len() < view_item_ids(&s).len());
    }

    #[test]
    fn rows_and_accessible_names() {
        let s = sample_session();
        let rows = list_rows(&s);
        let urgent = items(&rows).into_iter().find(|i| i.subject.starts_with("URGENT")).unwrap();
        assert!(urgent.importance_high && urgent.unread);
        assert!(urgent.accessible.starts_with("Importance High, From Carol Okafor, Subject URGENT"));
        assert!(urgent.accessible.contains("Flag Status Unflagged, Preview "));
        let report = items(&rows).into_iter().find(|i| i.subject == "Quarterly report draft").unwrap();
        assert!(report.has_attachments && report.flagged);
        assert!(report.accessible.starts_with("With Attachments, From Dev Patel"));
        assert!(report.accessible.contains("Due Date "));
        let mut s2 = sample_session();
        let drafts = s2.mailbox.role_folder(s2.current_account().unwrap(), FolderRole::Drafts).unwrap();
        s2.selection.folder = drafts;
        let rows = list_rows(&s2);
        let d = items(&rows);
        assert!(d.iter().all(|i| i.time == "None"));
        assert!(d.iter().any(|i| i.who == "Alice Rivera"));
        assert!(d.iter().any(|i| i.subject == "(no subject)"));
    }

    #[test]
    fn mentions_ignore_quoted_history_and_plain_addresses() {
        let s = sample_session();
        let needles = mention_needles(&s);
        let mail = |text: &str, html: Option<&str>| MailItem {
            body: mailcraft_model::Body {
                text: Some(text.into()),
                html: html.map(str::to_owned),
                ..Default::default()
            },
            ..Default::default()
        };
        // A reply quoting a header block that names me is not a mention.
        let reply = mail(
            "Sounds good.\n\n-----Original Message-----\nFrom: Jordan Lee <jordan.lee@example.net>\nSent: Monday\n\n@Jordan Lee see below\n",
            None,
        );
        assert!(!mentions_with(&needles, &reply));
        let quoted = mail("Agreed.\n\nOn Mon, Grace wrote:\n> thanks @jordan.lee\n", None);
        assert!(!mentions_with(&needles, &quoted));
        // My address in the text is not an @mention; "@Jordan Lee" and "@jordan.lee" are.
        assert!(!mentions_with(&needles, &mail("Write to jordan.lee@example.net for access.", None)));
        assert!(mentions_with(&needles, &mail("Thanks, @Jordan Lee!", None)));
        assert!(mentions_with(&needles, &mail("cc @jordan.lee: please check", None)));
        assert!(!mentions_with(&needles, &mail("ask @jordan.leeson instead", None)));
        // HTML: a mailto link to me shown as "@Name", before the history.
        let html = mail(
            "",
            Some(r#"<p>Hi <a href="mailto:Jordan.Lee@example.net"><span>@Jordan</span></a>, see this.</p>"#),
        );
        assert!(mentions_with(&needles, &html));
        let plain_link = mail("", Some(r#"<p>Mail <a href="mailto:jordan.lee@example.net">Jordan</a>.</p>"#));
        assert!(!mentions_with(&needles, &plain_link));
        let in_history = mail(
            "",
            Some(
                r#"<p>Ok</p><div id="divRplyFwdMsg"><a href="mailto:jordan.lee@example.net">@Jordan</a></div>"#,
            ),
        );
        assert!(!mentions_with(&needles, &in_history));
        assert!(
            !has_token("", "@x") && !has_token("é@jordan", "@jordan") && has_token("(@jordan)", "@jordan")
        );
    }

    /// Regression: hostile HTML (a megabyte of `mailto:` with no closing tag, a multibyte character at the
    /// scan cap) made the Mentioned filter quadratic and let the cap fall back to the whole body.
    #[test]
    fn mention_scan_is_bounded_on_hostile_bodies() {
        let s = sample_session();
        let needles = mention_needles(&s);
        let mail = |text: &str, html: Option<String>| MailItem {
            body: mailcraft_model::Body { text: Some(text.into()), html, ..Default::default() },
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let flood = "<a href=\"mailto:x".repeat(1024 * 1024 / 17);
        assert!(!mentions_with(&needles, &mail("", Some(flood))));
        // A multibyte character straddling the cap, then a mention past it: not scanned, no panic.
        let mut padded = "a".repeat(MAX_MENTION_SCAN - 1);
        padded.push('é');
        padded.push_str(r#"<a href="mailto:jordan.lee@example.net">@Jordan</a>"#);
        assert!(!mentions_with(&needles, &mail("", Some(padded.clone()))));
        let mut text = "b".repeat(MAX_MENTION_SCAN - 1);
        text.push('é');
        text.push_str(" @jordan.lee");
        assert!(!mentions_with(&needles, &mail(&text, None)));
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "the scan is bounded");
        assert_eq!(scan_head("aé", 2), "a");
        assert_eq!(scan_head("aé", 3), "aé");
        assert_eq!(scan_head("é", 1), "");
    }

    #[test]
    fn arrange_by_type_tells_item_kinds_apart() {
        let with = |subject: &str, mime: &str| MailItem {
            subject: subject.into(),
            attachments: vec![mailcraft_model::AttachmentMeta {
                filename: "part".into(),
                mime_type: mime.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(item_type(&MailItem::default()).1, "Message");
        assert_eq!(item_type(&MailItem { is_draft: true, ..Default::default() }).1, "Draft");
        assert_eq!(item_type(&with("Planning", "text/calendar; method=REQUEST")).1, "Meeting Request");
        assert_eq!(item_type(&with("Accepted: Planning", "text/calendar")).1, "Meeting Response");
        assert_eq!(item_type(&with("Canceled: Planning", "text/calendar")).1, "Meeting Cancellation");
        assert_eq!(item_type(&with("Read: Hi", "message/disposition-notification")).1, "Read Receipt");
        assert_eq!(item_type(&with("Delivered", "message/delivery-status")).1, "Delivery Report");
        assert_eq!(item_type(&with("Hi", "application/pkcs7-signature")).1, "Signed Message");
        assert_eq!(item_type(&with("Hi", "application/pkcs7-mime")).1, "Encrypted Message");
        assert_eq!(item_type(&with("Hi", "application/pdf")).1, "Message");
    }

    #[test]
    fn folder_pane() {
        let s = sample_session();
        let rows = folder_rows(&s);
        assert_eq!(rows[0].kind, FolderRowKind::FavoritesHeader);
        assert_eq!(rows[1].name, "Inbox");
        assert!(rows[1].selected && !rows[1].badge.is_empty());
        let root = rows.iter().position(|r| r.kind == FolderRowKind::AccountRoot).unwrap();
        assert_eq!(rows[root].name, SAMPLE_USER_ADDR);
        let names: Vec<&str> = rows[root + 1..].iter().map(|r| r.name.as_str()).collect();
        assert_eq!(&names[..4], &["Inbox", "Projects", "Receipts", "Drafts"]);
        assert!(!names.contains(&"Calendar"));
        let drafts = rows[root + 1..].iter().find(|r| r.name == "Drafts").unwrap();
        assert!(drafts.badge_total && drafts.badge == "[2]");
        assert_eq!(drafts.accessible, "Drafts: 2 items");
        let projects = rows[root + 1..].iter().find(|r| r.name == "Projects").unwrap();
        assert_eq!(projects.depth, 2);
    }

    #[test]
    fn show_total_number_of_items_changes_the_folder_badge() {
        let mut s = sample_session();
        let rows = folder_rows(&s);
        let inbox_row = rows.iter().find(|r| r.name == "Inbox" && r.kind == FolderRowKind::Folder).unwrap();
        let inbox = inbox_row.id.unwrap();
        assert!(!inbox_row.badge_total);
        let key = format!("folder.{}.showTotal", inbox.0);
        s.settings
            .set("state.uiPrefs", &serde_json::json!({ key: true, "folder.x.showTotal": true }))
            .unwrap();
        assert_eq!(show_total_folders(&s), BTreeSet::from([inbox.0]));
        let rows = folder_rows(&s);
        let inbox_row = rows.iter().find(|r| r.id == Some(inbox) && r.kind == FolderRowKind::Folder).unwrap();
        assert!(inbox_row.badge_total);
        let n = s.mailbox.total_count(inbox);
        assert_eq!(inbox_row.badge, format!("[{n}]"));
        assert_eq!(inbox_row.accessible, format!("Inbox: {n} items"));
    }

    #[test]
    fn reading_header_info_bars() {
        let s = sample_session();
        let find = |subj: &str| s.mailbox.mail.values().find(|m| m.subject == subj).unwrap().id;
        let h = reading_header(&s, find("URGENT: server room access")).unwrap();
        assert_eq!(h.from, "Carol Okafor <carol@example.com>");
        assert_eq!(h.from_initials, "CO");
        assert!(h.info.contains(&"This message was sent with High importance.".to_owned()));
        assert_eq!(h.date, "Fri 10/9/2026 7:30 AM");
        let h = reading_header(&s, find("Quarterly report draft")).unwrap();
        assert_eq!(
            h.info[0],
            "Follow up. Start by Friday, October 9, 2026. Due by Monday, October 12, 2026."
        );
        assert_eq!(h.attachments.len(), 1);
        assert_eq!(h.attachments[0].0, "Quarterly report draft.pdf");
        let h = reading_header(&s, find("Password expires in 7 days")).unwrap();
        assert!(h.info.iter().any(|l| l.starts_with("You forwarded this message on ")));
        let h = reading_header(&s, find("Project kickoff")).unwrap();
        assert!(h.info.iter().any(|l| l.starts_with("You replied to this message on ")));
        assert_eq!(h.cc, "Bob Chen");
        assert!(reading_header(&s, ItemId(u64::MAX)).is_none());
        assert_eq!(initials(None, "zed@example.com"), "Z");
        assert_eq!(initials(Some("Example Weekly"), "x@example.org"), "EW");
        assert_eq!(size_text(100), "100 bytes");
        assert_eq!(size_text(140_000), "137 KB");
        assert_eq!(size_text(1_572_864), "1.5 MB");
        assert_eq!(size_bucket(5).1, "Tiny (< 10 KB)");
        assert_eq!(size_bucket(2 * 1024 * 1024).1, "Huge (> 1 MB)");
    }

    // ---- Conversation view -------------------------------------------------------------------------------

    mod conversations {
        use super::*;
        use mailcraft_model::{Address, Mailbox};
        use serde_json::json;

        const NOW: i64 = 1_791_000_000; // 04:00 UTC

        struct Fx {
            s: Session,
            lone: ItemId,
            a1: ItemId,
            s1: ItemId,
            f1: ItemId,
            b1: ItemId,
            old: ItemId,
        }

        fn who(name: &str) -> Option<Address> {
            Some(Address { name: Some(name.into()), addr: format!("{}@example.com", name.to_lowercase()) })
        }

        #[allow(clippy::too_many_arguments)]
        fn add(
            s: &mut Session,
            folder: FolderId,
            subject: &str,
            from: &str,
            mid: &str,
            refs: &[&str],
            ago: i64,
            read: bool,
        ) -> ItemId {
            s.mailbox.add_mail(MailItem {
                folder,
                subject: subject.into(),
                from: who(from),
                message_id: (!mid.is_empty()).then(|| mid.to_owned()),
                in_reply_to: refs.last().map(|r| vec![(*r).to_owned()]).unwrap_or_default(),
                references: refs.iter().map(|r| (*r).to_owned()).collect(),
                received: DateTime { unix: NOW - ago, offset_min: 0 },
                read,
                preview: format!("{subject} preview"),
                ..Default::default()
            })
        }

        fn fixture() -> Fx {
            let mut s = Session::new(Mailbox::new_local("Test"));
            let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
            let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
            let sent = s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap();
            s.selection.folder = inbox;
            s.fixed_now = Some(DateTime { unix: NOW, offset_min: 0 });
            let a1 = add(&mut s, inbox, "Budget plan", "Alice", "a1@x", &[], 3 * 86_400, true);
            let s1 = add(&mut s, sent, "RE: Budget plan", "Me", "s1@x", &["a1@x"], 2 * 86_400, true);
            let f1 = add(&mut s, inbox, "FW: budget plan", "Carol", "f1@x", &[], 86_400, true);
            let b1 = add(&mut s, inbox, "RE: Budget plan", "Bob", "b1@x", &["a1@x", "s1@x"], 3_600, false);
            let lone = add(&mut s, inbox, "Lunch", "Dave", "l1@x", &[], 1_800, true);
            let old = add(&mut s, inbox, "Old thing", "Eve", "o1@x", &[], 10 * 86_400, true);
            if let Some(m) = s.mailbox.mail.get_mut(&a1) {
                m.flag =
                    FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
                m.categories = vec!["Red category".into()];
            }
            s.run("view.conversations", &json!({})).unwrap();
            Fx { s, lone, a1, s1, f1, b1, old }
        }

        fn rows(s: &Session) -> Vec<ItemRow> {
            items(&list_rows(s)).into_iter().cloned().collect()
        }

        fn select(s: &mut Session, id: ItemId) {
            s.run("nav.selectItem", &json!({"id": id.0})).unwrap();
        }

        #[test]
        fn header_rows_roll_up_the_conversation() {
            let fx = fixture();
            let r = rows(&fx.s);
            let ids: Vec<ItemId> = r.iter().map(|i| i.id).collect();
            assert_eq!(
                ids,
                vec![fx.lone, fx.b1, fx.old],
                "collapsed: one row per conversation, newest first"
            );
            let lone = &r[0];
            assert!(lone.conversation.is_none(), "a single message is a plain row");
            let h = &r[1];
            let c = h.conversation.as_ref().unwrap();
            assert!(c.header && !c.expanded);
            assert_eq!(c.count, 4, "the forward and the Sent Items reply join");
            assert_eq!(c.unread, 1);
            assert_eq!(h.subject, "Budget plan");
            assert_eq!(h.who, "Bob; Carol; Me; Alice");
            assert!(h.unread && h.flagged && !h.selected);
            assert_eq!(h.categories.len(), 1);
            assert!(
                h.accessible.starts_with("Conversation, From Bob; Carol; Me; Alice, Subject Budget plan, ")
            );
            assert!(h.accessible.contains("Collapsed"));
            assert_eq!(visible_item_ids(&fx.s), vec![fx.lone, fx.b1, fx.old]);
            let g = groups(&list_rows(&fx.s));
            assert_eq!(g.first().map(String::as_str), Some("Today"));
        }

        #[test]
        fn selected_conversation_expands_with_children() {
            let mut fx = fixture();
            select(&mut fx.s, fx.b1);
            let r = rows(&fx.s);
            let ids: Vec<ItemId> = r.iter().map(|i| i.id).collect();
            assert_eq!(ids, vec![fx.lone, fx.b1, fx.b1, fx.f1, fx.s1, fx.a1, fx.old]);
            let header = &r[1];
            assert!(header.conversation.as_ref().unwrap().expanded);
            assert!(header.accessible.contains("Expanded"));
            assert!(!header.selected, "the selected child row carries the highlight");
            assert!(header.conversation.as_ref().unwrap().contains_selection);
            let first_child = &r[2];
            assert!(first_child.selected && first_child.conversation_depth == 1);
            assert_eq!(first_child.who, "Bob");
            let sent = r.iter().find(|i| i.id == fx.s1).unwrap();
            assert_eq!(sent.conversation.as_ref().unwrap().other_folder.as_deref(), Some("Sent Items"));
            assert_eq!(sent.who, "Me", "children name the sender");
            assert!(sent.accessible.contains("Folder Sent Items"));
            assert_eq!(visible_item_ids(&fx.s), vec![fx.lone, fx.b1, fx.f1, fx.s1, fx.a1, fx.old]);
            // Down walks the children (also the Sent Items one) without leaving the folder.
            let inbox = fx.s.selection.folder;
            for want in [fx.f1, fx.s1, fx.a1, fx.old] {
                fx.s.run("nav.nextItem", &json!({})).unwrap();
                assert_eq!(fx.s.selection.focus, Some(want));
                assert_eq!(fx.s.selection.folder, inbox);
            }
            // Selection left: the conversation collapses again.
            assert_eq!(visible_item_ids(&fx.s), vec![fx.lone, fx.b1, fx.old]);
        }

        #[test]
        fn settings_other_folders_and_classic_indent() {
            let mut fx = fixture();
            select(&mut fx.s, fx.b1);
            fx.s.run("view.conversationSettings", &json!({"setting": "fromOtherFolders", "on": false}))
                .unwrap();
            let r = rows(&fx.s);
            assert!(!r.iter().any(|i| i.id == fx.s1), "Sent Items reply hidden");
            assert_eq!(r[1].conversation.as_ref().unwrap().count, 3);
            fx.s.run("view.conversationSettings", &json!({"setting": "fromOtherFolders"})).unwrap();
            assert!(conversation_settings(&fx.s, fx.s.selection.folder).from_other_folders, "toggled back");
            fx.s.run("view.conversationSettings", &json!({"setting": "classicIndent", "on": true})).unwrap();
            let r = rows(&fx.s);
            let kids: Vec<(ItemId, u8)> = r[2..6].iter().map(|i| (i.id, i.conversation_depth)).collect();
            assert_eq!(kids, vec![(fx.a1, 1), (fx.s1, 2), (fx.b1, 3), (fx.f1, 1)]);
            fx.s.run("view.conversationSettings", &json!({"setting": "sendersAboveSubject", "on": false}))
                .unwrap();
            assert!(!rows(&fx.s)[1].conversation.as_ref().unwrap().senders_above_subject);
            assert!(fx.s.run("view.conversationSettings", &json!({"setting": "x"})).is_err());
            assert!(
                fx.s.run("view.conversationSettings", &json!({"setting": "classicIndent", "on": 3})).is_err()
            );
        }

        #[test]
        fn expand_and_collapse_commands() {
            let mut fx = fixture();
            let expanded = |s: &Session| conversation_of(s, fx.b1).unwrap().expanded;
            // Expanding a conversation that is not selected keeps it open.
            select(&mut fx.s, fx.lone);
            let r = fx.s.run("view.expandGroup", &json!({"conversation": true, "id": fx.a1.0})).unwrap();
            assert_eq!(r["expanded"], true);
            assert!(expanded(&fx.s));
            fx.s.run("view.collapseGroup", &json!({"conversation": true, "id": fx.b1.0})).unwrap();
            assert!(!expanded(&fx.s));
            // Collapsing the selected conversation from a child moves the focus to the header message.
            select(&mut fx.s, fx.f1);
            assert!(expanded(&fx.s));
            let r = fx.s.run("view.collapseGroup", &json!({"conversation": true})).unwrap();
            assert_eq!(r["expanded"], false);
            assert_eq!(fx.s.selection.focus, Some(fx.b1));
            assert_eq!(visible_item_ids(&fx.s), vec![fx.lone, fx.b1, fx.old]);
            fx.s.run("view.expandGroup", &json!({"conversation": true})).unwrap();
            assert!(expanded(&fx.s));
            // Always Expand Selected Conversation: a manual collapse does not stick.
            fx.s.run("view.conversationSettings", &json!({"setting": "alwaysExpand", "on": true})).unwrap();
            let r = fx.s.run("view.collapseGroup", &json!({"conversation": true})).unwrap();
            assert_eq!(r["expanded"], true);
            // Group commands still work and keep the conversation state.
            fx.s.run("view.collapseAll", &json!({})).unwrap();
            assert!(items(&list_rows(&fx.s)).is_empty());
            fx.s.run("view.expandAll", &json!({})).unwrap();
            assert!(conversation_settings(&fx.s, fx.s.selection.folder).always_expand);
            // Junk params.
            assert!(fx.s.run("view.expandGroup", &json!({"conversation": true, "id": "x"})).is_err());
            assert!(fx.s.run("view.expandGroup", &json!({"conversation": true, "id": 999_999})).is_err());
        }

        #[test]
        fn group_items_follow_conversations() {
            let mut fx = fixture();
            let today = group_items(&fx.s, "Today");
            assert_eq!(today, vec![fx.lone, fx.b1, fx.f1, fx.a1], "view members only, header first");
            let a1 = fx.s.mailbox.mail.get(&fx.a1).cloned().unwrap();
            assert_eq!(group_label(&fx.s, &a1), "Today", "an old member sits in its conversation's group");
            select(&mut fx.s, fx.a1);
            fx.s.run("view.collapseGroup", &json!({})).unwrap();
            assert_eq!(groups(&list_rows(&fx.s)).len(), 2);
            assert!(!visible_item_ids(&fx.s).contains(&fx.b1), "Today collapsed");
        }

        #[test]
        fn toggle_scope_and_flat_list() {
            let mut fx = fixture();
            let sent = {
                let acct = fx.s.current_account().unwrap();
                fx.s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap()
            };
            assert!(!view_settings(&fx.s, sent).conversations);
            let r = fx.s.run("view.conversations", &json!({"scope": "all", "on": true})).unwrap();
            assert_eq!(r["scope"], "all");
            assert!(view_settings(&fx.s, sent).conversations);
            assert!(fx.s.run("view.conversations", &json!({"scope": "galaxy"})).is_err());
            let r = fx.s.run("view.conversations", &json!({"scope": "folder"})).unwrap();
            assert_eq!(r["on"], false);
            assert!(list_conversations(&fx.s).is_empty());
            assert!(view_settings(&fx.s, sent).conversations, "this folder only");
            fx.s.run("view.conversations", &json!({"on": true})).unwrap();
            fx.s.run("view.showInGroups", &json!({})).unwrap();
            let r = list_rows(&fx.s);
            assert!(groups(&r).is_empty());
            assert_eq!(items(&r).len(), 3);
            // Unread filter: only the unread reply stays in the view, its conversation still shows the rest.
            fx.s.run("view.filter", &json!({"kind": "unread"})).unwrap();
            let r = rows(&fx.s);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].id, fx.b1);
            assert_eq!(r[0].conversation.as_ref().unwrap().count, 2, "only the Sent Items reply joins");
        }
    }

    #[test]
    fn search_folders_list_and_count_their_live_results() {
        let mut s = sample_session();
        let r = s.run("folder.newSearchFolder", &serde_json::json!({"template": "unread"})).unwrap();
        let folder = FolderId(r["folder"].as_u64().unwrap());
        let counts = crate::search_index::search_folder_counts(&s, folder);
        assert!(counts.unread > 0 && counts.unread == counts.total, "{counts:?}");
        let row = folder_row(&s, folder, FolderRowKind::Folder, 1, false, &BTreeSet::new()).unwrap();
        assert_eq!(row.badge, counts.unread.to_string());
        assert!(row.accessible.contains("unread"), "{}", row.accessible);
        s.selection.folder = folder;
        let rows = list_rows(&s);
        let shown: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                ListRow::Item(i) => Some(i),
                ListRow::Group { .. } => None,
            })
            .collect();
        assert!(!shown.is_empty() && shown.iter().all(|i| i.unread));
    }

    #[test]
    fn arrange_by_categories_lists_a_message_under_each_category() {
        let mut s = sample_session();
        set_view(&mut s, |v| v.focused_inbox = false);
        let inbox = s.selection.folder;
        let ids: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        for id in &ids {
            s.mailbox.mail.get_mut(id).unwrap().categories.clear();
        }
        let both = ids[0];
        let red = ids[1];
        s.mailbox.mail.get_mut(&both).unwrap().categories =
            vec!["Red category".into(), "Blue category".into()];
        s.mailbox.mail.get_mut(&red).unwrap().categories = vec!["red category".into()];
        s.run("view.arrangeBy", &serde_json::json!({"key": "categories"})).unwrap();
        s.run("view.sort", &serde_json::json!({"order": "ascending"})).ok();
        let groups = |s: &Session| -> Vec<(String, Vec<ItemId>, bool)> {
            let mut out: Vec<(String, Vec<ItemId>, bool)> = Vec::new();
            for r in list_rows(s) {
                match r {
                    ListRow::Group { label, collapsed, .. } => out.push((label, Vec::new(), collapsed)),
                    ListRow::Item(i) => {
                        if let Some(g) = out.last_mut() {
                            g.1.push(i.id);
                        }
                    }
                }
            }
            out
        };
        let g = groups(&s);
        let labels: Vec<&str> = g.iter().map(|x| x.0.as_str()).collect();
        assert!(labels.contains(&"Red category") && labels.contains(&"Blue category"), "{labels:?}");
        assert!(!labels.iter().any(|l| l.contains(", ")), "no joined groups: {labels:?}");
        let members = |name: &str| g.iter().find(|x| x.0 == name).map(|x| x.1.clone()).unwrap_or_default();
        assert!(members("Red category").contains(&both) && members("Red category").contains(&red));
        assert_eq!(members("Blue category"), vec![both]);
        assert!(members(NONE_LABEL).len() == ids.len() - 2);
        // The repeated row has its own repeat count; keys visit the message once.
        let repeats: Vec<u16> = list_rows(&s)
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Item(i) if i.id == both => Some(i.repeat),
                _ => None,
            })
            .collect();
        assert_eq!(repeats, vec![0, 1]);
        assert_eq!(visible_item_ids(&s).iter().filter(|i| **i == both).count(), 1);
        // Collapsing Red leaves Blue open even though its message's first category is Red.
        s.view.collapsed_groups.entry(inbox).or_default().push("Red category".into());
        let g = groups(&s);
        assert!(g.iter().find(|x| x.0 == "Red category").unwrap().2);
        assert!(!g.iter().find(|x| x.0 == "Blue category").unwrap().2);
        assert_eq!(g.iter().find(|x| x.0 == "Blue category").unwrap().1, vec![both]);
    }

    #[test]
    fn all_items_search_lists_appointments_contacts_tasks_and_notes() {
        let mut s = sample_session();
        let subject = s.mailbox.appointments.values().next().map(|a| a.subject.clone()).unwrap();
        let word = subject.split_whitespace().next().unwrap().to_owned();
        s.run("search.query", &serde_json::json!({"text": word})).unwrap();
        let kinds = |s: &Session| -> Vec<Option<String>> {
            list_rows(s)
                .into_iter()
                .filter_map(|r| match r {
                    ListRow::Item(i) => Some(i.kind),
                    ListRow::Group { .. } => None,
                })
                .collect()
        };
        // Mailbox scopes: messages only.
        s.run("search.scope", &serde_json::json!({"scope": "allMailboxes"})).unwrap();
        assert!(kinds(&s).iter().all(Option::is_none));
        // All Items: the other kinds follow under their own header, and keys skip them.
        s.run("search.scope", &serde_json::json!({"scope": "allItems"})).unwrap();
        let rows = list_rows(&s);
        assert!(rows.iter().any(|r| matches!(r, ListRow::Group { label, .. } if label == OTHER_ITEMS_LABEL)));
        let other: Vec<ItemRow> = rows
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Item(i) if i.kind.is_some() => Some(i),
                _ => None,
            })
            .collect();
        assert!(other.iter().any(|i| i.kind.as_deref() == Some("appointment") && i.subject == subject));
        assert!(other.iter().all(|i| !s.mailbox.mail.contains_key(&i.id)));
        let visible = visible_item_ids(&s);
        assert!(other.iter().all(|i| !visible.contains(&i.id)));
        // Every kind gets a row.
        let task = s.mailbox.tasks.keys().next().copied().unwrap();
        let note = s.mailbox.notes.keys().next().copied();
        let contact = s.mailbox.contacts.keys().next().copied().unwrap();
        assert_eq!(pim_row(&s, task).unwrap().kind.as_deref(), Some("task"));
        assert_eq!(pim_row(&s, contact).unwrap().kind.as_deref(), Some("contact"));
        if let Some(n) = note {
            assert_eq!(pim_row(&s, n).unwrap().kind.as_deref(), Some("note"));
        }
        assert!(pim_row(&s, ItemId(u64::MAX)).is_none());
    }
}
