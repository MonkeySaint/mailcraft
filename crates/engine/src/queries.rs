//! Read-only views the UI renders (and the CLI/MCP report). Pure functions of the session.

use serde::{Deserialize, Serialize};

use std::collections::BTreeSet;

use crate::{Module, SearchScope, Session};
use mailcraft_model::{
    ArrangeBy, Category, DateGroup, FlagState, FocusClass, FolderId, FolderKind, FolderRole, Importance,
    ItemId, ListFilter, ListTime, MailItem, ResponseState, SortOrder, ViewSettings,
    subject::{conversation_topic, strip_prefixes},
    text::contains_ci,
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

/// One row of the message list (an item or a group header).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ListRow {
    Group { label: String, count: usize, collapsed: bool },
    Item(ItemRow),
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
    /// Conversation header/child rows (commit 7).
    pub conversation_depth: u8,
    /// Accessible name as documented in plan/outlook/reading-pane.md.
    pub accessible: String,
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

fn folder_row(
    s: &Session,
    id: FolderId,
    kind: FolderRowKind,
    depth: u32,
    has_children: bool,
) -> Option<FolderRow> {
    let f = s.mailbox.folder(id)?;
    Some(FolderRow {
        id: Some(id),
        kind,
        name: f.name.clone(),
        depth,
        has_children,
        expanded: f.expanded,
        badge: s.mailbox.badge(id).unwrap_or_default(),
        badge_total: f.role.is_some_and(FolderRole::shows_total_count),
        selected: s.selection.folder == id,
        accessible: s.mailbox.folder_accessible_name(id),
    })
}

/// Folder pane rows in display order (Favorites section, then each account tree), honouring expansion.
///
/// The Mail module shows mail and search folders; Calendar/People/Tasks/Notes show their own folder kind;
/// the Folders view (More Apps › Folders) shows every folder of every type. Favorites appear in Mail only.
pub fn folder_rows(s: &Session) -> Vec<FolderRow> {
    let mb = &s.mailbox;
    let module = s.view.module;
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
                favs.into_iter().filter_map(|id| folder_row(s, id, FolderRowKind::Favorite, 1, false)),
            );
        }
    }
    for account in &mb.accounts {
        let name = if account.address.trim().is_empty() {
            account.display_name.clone()
        } else {
            account.address.clone()
        };
        let roots: Vec<FolderId> = mb
            .children(account.id, None)
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
            let kids: Vec<FolderId> = mb
                .children(account.id, Some(id))
                .into_iter()
                .filter(|k| mb.folder(*k).is_some_and(|f| kind_visible(f.kind, module)))
                .collect();
            let Some(row) = folder_row(s, id, FolderRowKind::Folder, depth, !kids.is_empty()) else {
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

/// View settings of a folder (defaults when none were saved).
pub fn view_settings(s: &Session, folder: FolderId) -> ViewSettings {
    s.mailbox.views.get(&folder).cloned().unwrap_or_default()
}

/// One parsed search term.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Term {
    Any(String),
    From(String),
    To(String),
    Subject(String),
    HasAttachments(bool),
}

/// Splits a query into terms (double quotes group words; `from:`, `to:`, `subject:`, `hasattachments:`
/// prefixes are recognised case-insensitively).
fn parse_query(q: &str) -> Vec<Term> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .into_iter()
        .filter_map(|w| {
            let (key, value) = match w.split_once(':') {
                Some((k, v)) => (k.to_lowercase(), v.to_owned()),
                None => (String::new(), w.clone()),
            };
            let term = match key.as_str() {
                "from" => Term::From(value),
                "to" => Term::To(value),
                "subject" => Term::Subject(value),
                "hasattachments" | "hasattachment" => {
                    Term::HasAttachments(!matches!(value.to_lowercase().as_str(), "no" | "false" | "0"))
                }
                _ => Term::Any(w),
            };
            match &term {
                Term::Any(v) | Term::From(v) | Term::To(v) | Term::Subject(v) if v.trim().is_empty() => None,
                _ => Some(term),
            }
        })
        .collect()
}

fn people_match(list: &[mailcraft_model::Address], needle: &str) -> bool {
    list.iter()
        .any(|m| contains_ci(&m.addr, needle) || m.name.as_deref().is_some_and(|n| contains_ci(n, needle)))
}

/// Simple search: every term must match. Plain words match the subject, sender, recipients or body text
/// (case-insensitive substring); `from:`, `to:`, `subject:` restrict the field; `hasattachments:yes|no`.
/// An empty query matches everything.
pub fn search_matches(item: &MailItem, query: &str) -> bool {
    let terms = parse_query(query);
    let mut body: Option<String> = None;
    terms.iter().all(|t| match t {
        Term::From(v) => people_match(item.from.as_slice(), v),
        Term::To(v) => people_match(&item.to, v) || people_match(&item.cc, v),
        Term::Subject(v) => contains_ci(&item.subject, v),
        Term::HasAttachments(want) => item.has_attachments() == *want,
        Term::Any(v) => {
            contains_ci(&item.subject, v)
                || people_match(item.from.as_slice(), v)
                || people_match(&item.to, v)
                || people_match(&item.cc, v)
                || contains_ci(body.get_or_insert_with(|| item.body_text()), v)
        }
    })
}

/// Lower-case strings whose presence in a body means "I was mentioned": my addresses, "@" + their local part
/// and "@" + each account's sender name.
fn mention_needles(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for addr in &s.me {
        out.push(addr.clone());
        if let Some((local, _)) = addr.split_once('@')
            && !local.is_empty()
        {
            out.push(format!("@{local}"));
        }
    }
    for a in &s.mailbox.accounts {
        let name = a.sender_name.trim();
        if !name.is_empty() {
            out.push(format!("@{}", name.to_lowercase()));
        }
    }
    out
}

/// Mentioned Mail: the body contains one of my addresses (or "@my name").
pub fn mentions_me(s: &Session, item: &MailItem) -> bool {
    let needles = mention_needles(s);
    let body = item.body_text().to_lowercase();
    needles.iter().any(|n| body.contains(n.as_str()))
}

/// Mail folders the active search covers (`None` when no search is active).
fn search_folders(s: &Session) -> Option<Vec<FolderId>> {
    if s.view.search.trim().is_empty() {
        return None;
    }
    let mb = &s.mailbox;
    let mail_kind = |id: &FolderId| mb.folder(*id).is_some_and(|f| f.kind == FolderKind::Mail);
    let folders: Vec<FolderId> = match s.view.search_scope {
        SearchScope::CurrentFolder => vec![s.selection.folder],
        SearchScope::Subfolders => mb.subtree(s.selection.folder),
        SearchScope::CurrentMailbox => {
            let account = mb.folder_account(s.selection.folder).or_else(|| s.current_account());
            mb.folders
                .values()
                .filter(|f| Some(f.account) == account)
                .map(|f| f.id)
                .filter(mail_kind)
                .collect()
        }
        SearchScope::AllMailboxes | SearchScope::AllOutlookItems => {
            mb.folders.keys().copied().filter(mail_kind).collect()
        }
    };
    Some(folders)
}

/// Items of a search folder: its query over the account's mail folders except Deleted Items and Junk Email.
fn search_folder_items<'a>(s: &'a Session, folder: FolderId, query: &str) -> Vec<&'a MailItem> {
    let mb = &s.mailbox;
    let Some(account) = mb.folder_account(folder) else {
        return Vec::new();
    };
    let excluded: Vec<FolderId> = [FolderRole::DeletedItems, FolderRole::JunkEmail]
        .into_iter()
        .filter_map(|r| mb.role_folder(account, r))
        .collect();
    let included: BTreeSet<FolderId> = mb
        .folders
        .values()
        .filter(|f| f.account == account && f.kind == FolderKind::Mail)
        .filter(|f| !excluded.iter().any(|x| mb.is_in_subtree(f.id, *x)))
        .map(|f| f.id)
        .collect();
    mb.mail.values().filter(|m| included.contains(&m.folder) && search_matches(m, query)).collect()
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
    let mut items: Vec<&MailItem> = if let Some(folders) = search_folders(s) {
        let set: BTreeSet<FolderId> = folders.into_iter().collect();
        let q = s.view.search.as_str();
        mb.mail.values().filter(|m| set.contains(&m.folder) && search_matches(m, q)).collect()
    } else if f.kind == FolderKind::Search {
        search_folder_items(s, folder, f.search_query.as_deref().unwrap_or(""))
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
            items.retain(|m| {
                let body = m.body_text().to_lowercase();
                needles.iter().any(|n| body.contains(n.as_str()))
            });
        }
    }
    sort_items(s, &vs, &mut items);
    (vs, items)
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
            if m.is_draft {
                (Key::Num(1), "Draft".to_owned())
            } else {
                (Key::Num(0), "Message".to_owned())
            }
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
    let mut keyed: Vec<(Key, i64, &MailItem)> = items
        .iter()
        .map(|m| {
            let (k, _) = group_of(s, m, arrange);
            let secondary = if arrange == ArrangeBy::Size {
                i64::try_from(m.size).unwrap_or(i64::MAX)
            } else {
                m.received.unix
            };
            (k, secondary, *m)
        })
        .collect();
    keyed.sort_by(|a, b| {
        let primary = a.0.cmp(&b.0);
        let primary = if descending { primary.reverse() } else { primary };
        let secondary = match arrange {
            // Date and Size order every row by the sort direction.
            ArrangeBy::Date | ArrangeBy::Size if !descending => a.1.cmp(&b.1),
            // Elsewhere rows inside a group stay newest first.
            _ => b.1.cmp(&a.1),
        };
        primary.then(secondary).then(a.2.id.cmp(&b.2.id))
    });
    for (slot, (_, _, m)) in items.iter_mut().zip(keyed) {
        *slot = m;
    }
}

/// Splits sorted view items into list groups: `(header label, items)` in list order.
///
/// Items group by their sort key (so "Alice Rivera" and "alice rivera", or "Re: Plans" and "plans", share
/// one group); Date groups by its bucket. The header shows the label of the group's first item.
fn grouped<'a>(
    s: &Session,
    arrange: ArrangeBy,
    items: Vec<&'a MailItem>,
) -> Vec<(String, Vec<&'a MailItem>)> {
    let mut out: Vec<(Key, String, Vec<&'a MailItem>)> = Vec::new();
    for m in items {
        let (key, label) = group_of(s, m, arrange);
        let id = if arrange == ArrangeBy::Date { Key::Text(label.clone()) } else { key };
        match out.last_mut() {
            Some((last, _, list)) if *last == id => list.push(m),
            _ => out.push((id, label, vec![m])),
        }
    }
    out.into_iter().map(|(_, label, list)| (label, list)).collect()
}

/// Header label of the group `item` falls in under the selected folder's arrangement.
pub fn group_label(s: &Session, item: &MailItem) -> String {
    let (vs, items) = view_items(s);
    grouped(s, vs.arrange_by, items)
        .into_iter()
        .find(|(_, list)| list.iter().any(|m| m.id == item.id))
        .map(|(label, _)| label)
        .unwrap_or_else(|| group_of(s, item, vs.arrange_by).1)
}

/// Item ids in a list group (by header label), in list order, including collapsed groups.
pub fn group_items(s: &Session, label: &str) -> Vec<ItemId> {
    let (vs, items) = view_items(s);
    grouped(s, vs.arrange_by, items)
        .into_iter()
        .filter(|(l, _)| l == label)
        .flat_map(|(_, list)| list.into_iter().map(|m| m.id))
        .collect()
}

/// Every item id in the current view, in list order (items inside collapsed groups included).
pub fn view_item_ids(s: &Session) -> Vec<ItemId> {
    view_items(s).1.into_iter().map(|m| m.id).collect()
}

/// All item ids the list shows, in order (collapsed groups excluded).
pub fn visible_item_ids(s: &Session) -> Vec<ItemId> {
    list_rows(s)
        .into_iter()
        .filter_map(|r| match r {
            ListRow::Item(i) => Some(i.id),
            ListRow::Group { .. } => None,
        })
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
    }
}

/// Message list rows for the selected folder with the folder's view settings (filter, Focused/Other,
/// search, arrange/sort, groups and collapsed groups).
pub fn list_rows(s: &Session) -> Vec<ListRow> {
    let (vs, items) = view_items(s);
    if !vs.show_in_groups {
        return items.into_iter().map(|m| ListRow::Item(item_row(s, m))).collect();
    }
    let collapsed: &[String] =
        s.view.collapsed_groups.get(&s.selection.folder).map(Vec::as_slice).unwrap_or(&[]);
    let mut rows = Vec::new();
    for (label, list) in grouped(s, vs.arrange_by, items) {
        // Collapse state is stored by header label; the View commands (Collapse This Group / Collapse All
        // Groups) store their own per-item labels, so honour those too.
        let is_collapsed = !collapsed.is_empty()
            && (collapsed.contains(&label)
                || list.iter().any(|m| {
                    let other = crate::cmd::view::group_label(s, m, vs.arrange_by);
                    collapsed.contains(&other)
                }));
        rows.push(ListRow::Group { label, count: list.len(), collapsed: is_collapsed });
        if !is_collapsed {
            rows.extend(list.into_iter().map(|m| ListRow::Item(item_row(s, m))));
        }
    }
    rows
}

/// Status bar counts: (items in view, unread in view).
pub fn status_counts(s: &Session) -> (usize, usize) {
    let (_, items) = view_items(s);
    let unread = items.iter().filter(|m| !m.read).count();
    (items.len(), unread)
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

pub fn reading_header(s: &Session, id: ItemId) -> Option<ReadingHeader> {
    let m = s.mailbox.mail.get(&id)?;
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
                assert_eq!(items(&rows).len(), total, "{key:?}");
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
        s.view.search_scope = SearchScope::AllMailboxes;
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
        let m = s.mailbox.mail.values().next().unwrap();
        assert!(search_matches(m, ""));
        assert!(search_matches(m, "   "));
    }

    #[test]
    fn collapsed_groups_hide_items() {
        let mut s = sample_session();
        let folder = s.selection.folder;
        s.view.collapsed_groups.insert(folder, vec!["Today".into()]);
        let rows = list_rows(&s);
        match rows.first() {
            Some(ListRow::Group { label, collapsed, count }) => {
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
}
