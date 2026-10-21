//! Advanced View Settings across the modules: Group By on several fields, multi-key Sort, Expand/collapse
//! defaults, Other Settings, the People card views, Notes groups and Format Columns, mail Group By and Then by.

use serde_json::{Value, json};

use super::modules::{self, ModuleKind, ViewRow};
use super::table;
use crate::cmd::calendar::tests::{role, session};
use crate::queries::{ListRow, list_rows};
use crate::{CmdError, Selection, Session};
use mailcraft_model::view::GROUP_SEP;
use mailcraft_model::{Address, DateTime, FolderId, FolderRole, Importance, ItemId, MailItem};

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn select_folder(s: &mut Session, r: FolderRole) -> FolderId {
    let f = role(s, r);
    s.selection = Selection { folder: f, items: vec![], focus: None };
    f
}

fn task(s: &mut Session, p: Value) -> u64 {
    run(s, "tasks.new", p)["id"].as_u64().unwrap()
}

/// Selects item `id` (the group commands need a selected item, as the UI's header click makes sure).
fn pick(s: &mut Session, id: u64) {
    s.selection.items = vec![ItemId(id)];
    s.selection.focus = Some(ItemId(id));
}

fn ids(rows: &[ViewRow]) -> Vec<u64> {
    rows.iter().map(|r| r.id.0).collect()
}

fn group_list(rows: &[ViewRow]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in rows.iter().filter_map(|r| r.group.clone()) {
        if !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

#[test]
fn tasks_group_by_two_fields_nests_the_rows() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Tasks);
    let a = task(&mut s, json!({"subject": "A", "priority": "high", "categories": ["Red category"]}));
    let b = task(
        &mut s,
        json!({"subject": "B", "priority": "low", "categories": ["Red category", "Blue category"]}),
    );
    let c = task(&mut s, json!({"subject": "C", "priority": "high"}));
    run(&mut s, "view.changeView", json!({"name": "Simple List"}));
    assert!(modules::rows(&s, folder).iter().all(|r| r.group.is_none()), "Simple List has no arrangement");
    let r = run(
        &mut s,
        "view.viewSettings",
        json!({"groupBy": [{"field": "categories"}, {"field": "priority", "dir": "desc"}]}),
    );
    assert_eq!(r["table"]["group"], "Categories (ascending), Priority (descending)");
    let rows = modules::rows(&s, folder);
    // B is filed under both categories, so it has a row in each.
    assert_eq!(rows.iter().filter(|r| r.id.0 == b).count(), 2);
    let sep = GROUP_SEP;
    assert_eq!(
        group_list(&rows),
        vec![
            format!("Categories: (none){sep}Priority: High"),
            format!("Categories: Blue category{sep}Priority: Low"),
            format!("Categories: Red category{sep}Priority: Low"),
            format!("Categories: Red category{sep}Priority: High"),
        ]
    );
    // Priority sorts High first ascending (as a header click does), so descending puts Low first.
    assert_eq!(ids(&rows), vec![c, b, b, a]);
    let first = rows.first().unwrap();
    assert_eq!(first.levels, vec!["Categories: (none)".to_owned(), "Priority: High".to_owned()]);
    assert_eq!(first.group_key(0).as_deref(), Some("Categories: (none)"));
    // Every outer group is a group of its own (Expand/Collapse names it).
    let all = modules::groups(&s, folder);
    assert!(all.contains(&"Categories: Red category".to_owned()));
    assert!(all.contains(&format!("Categories: Red category{sep}Priority: Low")));
    pick(&mut s, a);
    run(&mut s, "view.collapseGroup", json!({"label": "Categories: Red category"}));
    let collapsed = s.view.collapsed_groups.get(&folder).cloned().unwrap_or_default();
    let hidden: Vec<&ViewRow> = rows
        .iter()
        .filter(|r| modules::hidden_by(&r.levels, &|k| collapsed.iter().any(|c| c == k)))
        .collect();
    assert_eq!(hidden.len(), 2, "collapsing the outer group hides the rows of every group inside it");
    // Bad fields are refused; null goes back to the arrangement.
    assert!(matches!(s.run("view.viewSettings", &json!({"groupBy": ["nope"]})), Err(CmdError::Params(_))));
    assert!(s.run("view.viewSettings", &json!({"groupBy": ["subject", "subject"]})).is_err());
    assert!(
        s.run(
            "view.viewSettings",
            &json!({"groupBy": ["subject", "status", "priority", "dueDate", "owner"]})
        )
        .is_err(),
        "at most four levels"
    );
    run(&mut s, "view.viewSettings", json!({"groupBy": null}));
    assert!(modules::rows(&s, folder).iter().all(|r| r.group.is_none()));
    // Show in Groups off ignores the fields.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["priority"]}));
    run(&mut s, "view.showInGroups", json!({}));
    assert!(modules::rows(&s, folder).iter().all(|r| r.group.is_none()));
}

#[test]
fn tasks_sort_on_several_keys_and_expand_default() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Tasks);
    let a = task(&mut s, json!({"subject": "Alpha", "priority": "high", "due": "2026-10-20"}));
    let b = task(&mut s, json!({"subject": "Bravo", "priority": "high", "due": "2026-10-12"}));
    let c = task(&mut s, json!({"subject": "Charlie", "priority": "low", "due": "2026-10-12"}));
    run(&mut s, "view.changeView", json!({"name": "Simple List"}));
    let r = run(
        &mut s,
        "view.sortBy",
        json!({"fields": [{"field": "priority"}, {"field": "subject", "dir": "desc"}]}),
    );
    assert_eq!(r["fields"].as_array().map(Vec::len), Some(2));
    assert_eq!(ids(&modules::rows(&s, folder)), vec![b, a, c]);
    let st = modules::state(&s, folder, ModuleKind::Tasks);
    assert_eq!(table::custom(&st).then_by.len(), 1);
    // A header click is one key again.
    run(&mut s, "view.sortBy", json!({"field": "dueDate"}));
    assert!(table::custom(&modules::state(&s, folder, ModuleKind::Tasks)).then_by.is_empty());
    // The same through View Settings, with a hostile list refused.
    run(&mut s, "view.viewSettings", json!({"sortBy": ["dueDate", {"field": "priority", "dir": "desc"}]}));
    assert_eq!(ids(&modules::rows(&s, folder)), vec![c, b, a]);
    assert!(s.run("view.viewSettings", &json!({"sortBy": [{"field": "subject", "dir": "up"}]})).is_err());
    assert!(s.run("view.viewSettings", &json!({"sortBy": "subject"})).is_err());
    // Expand/collapse defaults: All collapsed closes every group when it is set and when the view opens.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["priority"], "expandGroups": "collapsed"}));
    let groups = modules::groups(&s, folder);
    assert_eq!(groups, vec!["Priority: High".to_owned(), "Priority: Low".to_owned()]);
    let shut = |s: &Session| s.view.collapsed_groups.get(&folder).cloned().unwrap_or_default();
    assert_eq!(shut(&s).iter().filter(|e| crate::cmd::view::is_group_label(e)).count(), 2);
    run(&mut s, "view.expandAll", json!({}));
    run(&mut s, "view.changeView", json!({"name": "Detailed"}));
    run(&mut s, "view.changeView", json!({"name": "Simple List"}));
    assert_eq!(shut(&s).iter().filter(|e| crate::cmd::view::is_group_label(e)).count(), 2);
    assert!(s.run("view.viewSettings", &json!({"expandGroups": "sideways"})).is_err());
    let summary = table::summary(folder, ModuleKind::Tasks, &modules::state(&s, folder, ModuleKind::Tasks));
    assert_eq!(summary["expandGroups"], "collapsed");
}

#[test]
fn other_settings_fonts_grid_lines_and_auto_preview() {
    let mut s = session();
    let tasks = select_folder(&mut s, FolderRole::Tasks);
    run(&mut s, "view.changeView", json!({"name": "Detailed"}));
    run(&mut s, "view.viewSettings", json!({"rowFont": 14, "columnFont": 200, "gridLines": false}));
    let o = table::other(&s, tasks);
    assert_eq!((o.row_font, o.column_font, o.no_grid), (14, 24, true), "fonts clamped to 8–24 pt");
    // Tasks show AutoPreview as their message preview.
    run(&mut s, "view.viewSettings", json!({"autoPreview": true}));
    assert_eq!(modules::state(&s, tasks, ModuleKind::Tasks).preview_lines, 3);
    run(&mut s, "view.viewSettings", json!({"autoPreview": false}));
    assert_eq!(modules::state(&s, tasks, ModuleKind::Tasks).preview_lines, 0);
    assert!(s.run("view.viewSettings", &json!({"rowFont": "big"})).is_err());
    assert!(s.run("view.viewSettings", &json!({"gridLines": 1})).is_err());
    // Notes-only keys are refused in a tasks folder.
    assert!(s.run("view.viewSettings", &json!({"columns": ["subject"]})).is_err());
    // People tables keep AutoPreview in their Other Settings.
    let people = select_folder(&mut s, FolderRole::Contacts);
    run(&mut s, "view.changeView", json!({"name": "Phone"}));
    run(&mut s, "view.viewSettings", json!({"autoPreview": true, "rowFont": 0}));
    assert!(table::other(&s, people).auto_preview);
    // Reset View puts the defaults back.
    run(&mut s, "view.resetView", json!({}));
    assert_eq!(table::other(&s, people), mailcraft_model::view::TableOther::default());
}

#[test]
fn new_task_columns_have_cells() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Tasks);
    let id = task(&mut s, json!({"subject": "Report", "body": "\n  First line of notes\nsecond"}));
    if let Some(t) = s.mailbox.tasks.get_mut(&ItemId(id)) {
        t.requested_by = Some("Boss <boss@example.com>".into());
        t.request_state = mailcraft_model::task::TaskRequestState::Accepted;
    }
    let t = s.mailbox.tasks[&ItemId(id)].clone();
    let cell = |c: &str| table::task_cell(&s, table::Item::Task(&t), c);
    assert_eq!(cell("notes"), "First line of notes");
    assert_eq!(cell("recurring"), "No");
    assert_eq!(cell("reminderOn"), "No");
    assert_eq!(cell("requestedBy"), "Boss <boss@example.com>");
    assert_eq!(cell("requestStatus"), "Accepted");
    assert_eq!(cell("owner"), "Me Myself");
    assert_eq!(cell("lastUpdate"), "");
    run(&mut s, "view.changeView", json!({"name": "Detailed"}));
    run(&mut s, "view.columns", json!({"add": "Owner"}));
    assert!(table::columns(&s, folder).iter().any(|c| c.id == "owner"));
    assert!(table::TASK_COLUMNS.len() >= 23);
}

#[test]
fn people_arranged_by_categories_list_a_contact_under_each_category() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Contacts);
    let both = run(
        &mut s,
        "people.new",
        json!({"fullName": "Ann Lee", "categories": ["Red category", "Blue category", "red category"]}),
    )["id"]
        .as_u64()
        .unwrap();
    let none = run(&mut s, "people.new", json!({"fullName": "Bo Day"}))["id"].as_u64().unwrap();
    run(&mut s, "view.changeView", json!({"name": "Phone"}));
    run(&mut s, "view.arrangeBy", json!({"key": "categories"}));
    let rows = modules::rows(&s, folder);
    assert_eq!(rows.iter().filter(|r| r.id.0 == both).count(), 2, "one row per category (case-insensitive)");
    assert_eq!(group_list(&rows), vec!["Blue category", "Red category", "(none)"]);
    assert!(rows.iter().any(|r| r.id.0 == none && r.group.as_deref() == Some("(none)")));
    // Collapsing one category hides only that header's row.
    pick(&mut s, both);
    run(&mut s, "view.collapseGroup", json!({"label": "Red category"}));
    assert!(modules::groups(&s, folder).contains(&"Blue category".to_owned()));
}

#[test]
fn people_card_views_take_sort_filter_rules_and_card_fields() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Contacts);
    let ann =
        run(&mut s, "people.new", json!({"fullName": "Ann Lee", "company": "Zeta"}))["id"].as_u64().unwrap();
    let bo =
        run(&mut s, "people.new", json!({"fullName": "Bo Day", "company": "Acme"}))["id"].as_u64().unwrap();
    for view in ["People", "Business Card", "Card"] {
        run(&mut s, "view.changeView", json!({"name": view}));
        assert!(s.enabled("view.sortBy").is_none());
        assert!(s.enabled("view.customFilter").is_none(), "{view}: Filter");
        assert!(s.enabled("view.conditionalFormatting").is_none(), "{view}: Conditional Formatting");
        run(&mut s, "view.sortBy", json!({"fields": [{"field": "company"}]}));
        assert_eq!(ids(&modules::rows(&s, folder)), vec![bo, ann], "{view}: sorted by company");
        run(&mut s, "view.customFilter", json!({"filter": {"words": "ann"}}));
        assert_eq!(ids(&modules::rows(&s, folder)), vec![ann], "{view}: filtered");
        run(&mut s, "view.customFilter", json!({"filter": null}));
        let rules = json!([{"name": "Zeta", "on": true, "kind": "custom", "condition": {"words": "lee"},
            "font": {"bold": true}}]);
        run(&mut s, "view.conditionalFormatting", json!({ "rules": rules }));
        let c = s.mailbox.contacts[&ItemId(ann)].clone();
        assert!(table::row_style(&s, &table::rules(&s, folder), table::Item::Contact(&c)).font.bold);
    }
    // Each card view keeps its own settings.
    run(&mut s, "view.changeView", json!({"name": "People"}));
    assert_eq!(table::sort(&s, folder).map(|f| f.column), Some("company".into()));
    // The Card view shows the columns chosen as the card's fields; the other card views have none.
    run(&mut s, "view.changeView", json!({"name": "Card"}));
    assert!(s.enabled("view.addColumns").is_none());
    assert_eq!(table::columns(&s, folder).len(), 8);
    run(&mut s, "view.columns", json!({"columns": ["fullName", "company"]}));
    assert_eq!(table::columns(&s, folder).iter().map(|c| c.id).collect::<Vec<_>>(), ["fullName", "company"]);
    run(&mut s, "view.changeView", json!({"name": "Business Card"}));
    assert!(table::columns(&s, folder).is_empty());
    assert!(s.enabled("view.formatColumns").is_some(), "no columns to format in a card view");
}

fn note(s: &mut Session, text: &str, color: &str, cats: &[&str]) -> u64 {
    let id = run(s, "notes.new", json!({"text": text, "color": color}))["id"].as_u64().unwrap();
    if let Some(n) = s.mailbox.notes.get_mut(&ItemId(id)) {
        n.categories = cats.iter().map(|c| (*c).to_owned()).collect();
    }
    id
}

#[test]
fn notes_group_by_sort_keys_and_format_columns() {
    let mut s = session();
    let folder = select_folder(&mut s, FolderRole::Notes);
    let a = note(&mut s, "Alpha", "blue", &["Red category"]);
    let b = note(&mut s, "Bravo", "yellow", &[]);
    let c = note(&mut s, "Charlie", "blue", &["Red category", "Green category"]);
    run(&mut s, "view.changeView", json!({"name": "Notes List"}));
    assert!(s.enabled("view.collapseAll").is_some(), "no groups yet");
    run(&mut s, "view.viewSettings", json!({"groupBy": [{"field": "icon"}], "sortBy": ["subject"]}));
    let rows = modules::rows(&s, folder);
    assert_eq!(group_list(&rows), vec!["Color: Blue", "Color: Yellow"]);
    assert_eq!(ids(&rows), vec![a, c, b]);
    assert!(s.enabled("view.collapseAll").is_none(), "grouped Notes tables open and close their groups");
    pick(&mut s, a);
    run(&mut s, "view.collapseGroup", json!({"label": "Color: Blue"}));
    let shut = s.view.collapsed_groups.get(&folder).cloned().unwrap_or_default();
    assert!(shut.contains(&"Color: Blue".to_owned()));
    // Categories list a note under each of them; several sort keys order the rows inside.
    run(
        &mut s,
        "view.viewSettings",
        json!({"groupBy": ["categories"], "sortBy": [{"field": "icon"}, {"field": "subject", "dir": "desc"}]}),
    );
    let rows = modules::rows(&s, folder);
    assert_eq!(
        group_list(&rows),
        vec!["Categories: (none)", "Categories: Green category", "Categories: Red category"]
    );
    assert_eq!(ids(&rows), vec![b, c, c, a]);
    let st = modules::state(&s, folder, ModuleKind::Notes);
    assert_eq!(st.then_sort.len(), 1);
    // Format Columns renames and aligns a Notes column.
    assert_eq!(run(&mut s, "view.formatColumns", json!({}))["dialog"], "viewFormat");
    run(&mut s, "view.formatColumns", json!({"column": "subject", "label": "Title", "align": "center"}));
    let shown = modules::note_shown(&modules::state(&s, folder, ModuleKind::Notes));
    assert!(shown.iter().any(|(k, l, al)| *k == "subject" && l == "Title" && *al == "center"));
    assert!(s.run("view.formatColumns", &json!({"column": "nope"})).is_err());
    assert!(s.run("view.viewSettings", &json!({"formats": {"bogus": {"label": "x"}}})).is_err());
    // Fonts.
    run(&mut s, "view.viewSettings", json!({"rowFont": 12, "columnFont": 10}));
    let st = modules::state(&s, folder, ModuleKind::Notes);
    assert_eq!((st.other.row_font, st.other.column_font), (12, 10));
    // Reset View clears it all and stays on Notes List.
    run(&mut s, "view.resetView", json!({}));
    let st = modules::state(&s, folder, ModuleKind::Notes);
    assert_eq!(st.view, "Notes List");
    assert!(st.group_by.is_empty() && st.then_sort.is_empty() && st.formats.is_empty());
    // The Icon view never groups.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["categories"]}));
    run(&mut s, "view.changeView", json!({"name": "Icon"}));
    assert!(modules::rows(&s, folder).iter().all(|r| r.group.is_none()));
}

fn mail(
    s: &mut Session,
    from: &str,
    subject: &str,
    imp: Importance,
    minutes_ago: i64,
    cats: &[&str],
) -> ItemId {
    let folder = s.selection.folder;
    let now = s.now().unix;
    s.mailbox.add_mail(MailItem {
        folder,
        subject: subject.into(),
        from: Some(Address::new(Some(from), &format!("{}@example.com", from.to_lowercase()))),
        received: DateTime { unix: now - minutes_ago * 60, offset_min: 0 },
        importance: imp,
        categories: cats.iter().map(|c| (*c).to_owned()).collect(),
        read: true,
        ..Default::default()
    })
}

fn headers(s: &Session) -> Vec<(String, u8, usize)> {
    list_rows(s)
        .into_iter()
        .filter_map(|r| match r {
            ListRow::Group { label, depth, count, .. } => Some((label, depth, count)),
            ListRow::Item(_) => None,
        })
        .collect()
}

fn item_ids(s: &Session) -> Vec<ItemId> {
    list_rows(s)
        .into_iter()
        .filter_map(|r| match r {
            ListRow::Item(i) => Some(i.id),
            ListRow::Group { .. } => None,
        })
        .collect()
}

#[test]
fn mail_group_by_fields_nest_and_sort_then_by() {
    let mut s = session();
    let inbox = select_folder(&mut s, FolderRole::Inbox);
    run(&mut s, "view.focusedInbox", json!({}));
    let a1 = mail(&mut s, "Ann", "One", Importance::High, 10, &[]);
    let b1 = mail(&mut s, "Bo", "Two", Importance::Normal, 20, &["Red category"]);
    let a2 = mail(&mut s, "Ann", "Three", Importance::Normal, 30, &[]);
    let a3 = mail(&mut s, "Ann", "Four", Importance::High, 40, &[]);
    // Sort by From, then by Importance (high first), then the arrangement's own order.
    run(
        &mut s,
        "view.sortBy",
        json!({"fields": [{"field": "from"}, {"field": "importance", "dir": "desc"}]}),
    );
    run(&mut s, "view.showInGroups", json!({}));
    assert_eq!(item_ids(&s), vec![a1, a3, a2, b1]);
    let keys = super::mail::sort_keys(&s, inbox);
    assert_eq!(keys.len(), 2);
    run(&mut s, "view.showInGroups", json!({}));
    // Group By From, then Importance.
    let r = run(
        &mut s,
        "view.viewSettings",
        json!({"groupBy": [{"field": "from"}, {"field": "importance", "dir": "desc"}]}),
    );
    assert_eq!(r["group"], "From (ascending), Importance (descending)");
    let sep = GROUP_SEP;
    assert_eq!(
        headers(&s),
        vec![
            ("From: Ann".to_owned(), 0, 3),
            (format!("From: Ann{sep}Importance: High"), 1, 2),
            (format!("From: Ann{sep}Importance: Normal"), 1, 1),
            ("From: Bo".to_owned(), 0, 1),
            (format!("From: Bo{sep}Importance: Normal"), 1, 1),
        ]
    );
    assert_eq!(crate::queries::group_title(&format!("From: Ann{sep}Importance: High")), "Importance: High");
    // Collapse This Group closes the innermost group of the selected message; an outer group hides all.
    s.selection = Selection { folder: inbox, items: vec![a2], focus: Some(a2) };
    let m = s.mailbox.mail[&a2].clone();
    assert_eq!(crate::queries::group_label(&s, &m), format!("From: Ann{sep}Importance: Normal"));
    run(&mut s, "view.collapseGroup", json!({"label": "From: Ann"}));
    assert_eq!(item_ids(&s), vec![b1]);
    assert_eq!(headers(&s).len(), 3, "the groups inside a collapsed group have no headers");
    run(&mut s, "view.expandAll", json!({}));
    // Group By Categories lists a message under each of its categories; All collapsed closes them all.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["categories"], "expandGroups": "collapsed"}));
    assert!(item_ids(&s).is_empty());
    assert!(headers(&s).iter().any(|h| h.0 == "Categories: Red category"));
    // Back to the arrangement.
    run(&mut s, "view.viewSettings", json!({"groupBy": null, "expandGroups": "expanded"}));
    assert!(headers(&s).iter().all(|h| h.1 == 0 && !h.0.contains(':')));
    // Hostile values are refused.
    assert!(s.run("view.viewSettings", &json!({"groupBy": ["shoeSize"]})).is_err());
    assert!(s.run("view.sortBy", &json!({"fields": [{"field": "from", "dir": 3}]})).is_err());
    assert!(
        s.run("view.viewSettings", &json!({"sortBy": ["from", "to", "size", "subject", "date"]})).is_err()
    );
    // Reset View drops the fields.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["from"], "sortBy": ["from", "size"]}));
    run(&mut s, "view.resetView", json!({}));
    let v = super::mail::mail_view(&s, inbox);
    assert!(v.group_by.is_empty() && v.then_by.is_empty());
}

#[test]
fn mail_group_by_in_conversation_view_counts_members() {
    let mut s = session();
    select_folder(&mut s, FolderRole::Inbox);
    run(&mut s, "view.focusedInbox", json!({}));
    mail(&mut s, "Ann", "Plans", Importance::Normal, 10, &[]);
    mail(&mut s, "Ann", "RE: Plans", Importance::Normal, 20, &[]);
    mail(&mut s, "Bo", "Lunch", Importance::Normal, 30, &[]);
    run(&mut s, "view.conversations", json!({"on": true}));
    run(&mut s, "view.viewSettings", json!({"groupBy": ["from"]}));
    let h = headers(&s);
    assert_eq!(h.len(), 2);
    assert_eq!(h.iter().map(|x| x.2).sum::<usize>(), 3);
}

#[test]
fn a_header_click_in_a_mail_folder_sorts_by_that_one_column() {
    let mut s = session();
    let inbox = select_folder(&mut s, FolderRole::Inbox);
    run(&mut s, "view.focusedInbox", json!({}));
    let a = mail(&mut s, "Ann", "Same", Importance::Normal, 10, &[]);
    let b = mail(&mut s, "Bo", "Same", Importance::Normal, 20, &[]);
    run(
        &mut s,
        "view.sortBy",
        json!({"fields": [{"field": "importance"}, {"field": "from", "dir": "desc"}]}),
    );
    assert_eq!(super::mail::mail_view(&s, inbox).then_by.len(), 1);
    run(&mut s, "view.sortBy", json!({"field": "subject", "dir": "asc"}));
    assert!(super::mail::mail_view(&s, inbox).then_by.is_empty(), "the old Then by keys go");
    assert_eq!(super::mail::sort_keys(&s, inbox).len(), 1);
    // Ties fall back to the list's own order (newest first), not to the old From key.
    assert_eq!(item_ids(&s), vec![a, b]);
}

#[test]
fn expand_collapse_defaults_apply_each_time_a_folder_is_opened() {
    let mut s = session();
    let inbox = select_folder(&mut s, FolderRole::Inbox);
    let sent = role(&s, FolderRole::SentItems);
    run(&mut s, "view.focusedInbox", json!({}));
    let ann = mail(&mut s, "Ann", "Plans", Importance::Normal, 10, &[]);
    let bo = mail(&mut s, "Bo", "Lunch", Importance::Normal, 30, &[]);
    let collapsed = |s: &Session, f: FolderId| -> usize {
        s.view.collapsed_groups.get(&f).map_or(0, |g| g.iter().filter(|e| super::is_group_label(e)).count())
    };
    let open = |s: &mut Session, f: FolderId| {
        run(s, "nav.selectFolder", json!({"folder": f.0}));
    };
    // All expanded (the default): a group collapsed by hand opens again the next time the folder opens.
    run(&mut s, "view.viewSettings", json!({"groupBy": ["from"]}));
    pick(&mut s, ann.0);
    run(&mut s, "view.collapseGroup", json!({"label": "From: Ann"}));
    assert_eq!(collapsed(&s, inbox), 1);
    open(&mut s, sent);
    open(&mut s, inbox);
    assert_eq!(collapsed(&s, inbox), 0, "each opening of the view forgets groups collapsed by hand");
    // All collapsed: groups that appear while the folder is away are closed too when it opens again.
    run(&mut s, "view.viewSettings", json!({"expandGroups": "collapsed"}));
    assert_eq!(collapsed(&s, inbox), 2);
    run(&mut s, "view.expandAll", json!({}));
    open(&mut s, sent);
    let now = s.now().unix;
    s.mailbox.add_mail(MailItem {
        folder: inbox,
        subject: "New".into(),
        from: Some(Address::new(Some("Cy"), "cy@example.com")),
        received: DateTime { unix: now, offset_min: 0 },
        ..Default::default()
    });
    open(&mut s, inbox);
    assert_eq!(collapsed(&s, inbox), 3, "every group closes when the view opens");
    assert!(item_ids(&s).is_empty());
    // As last viewed: groups stay as they were left.
    run(&mut s, "view.viewSettings", json!({"expandGroups": "lastViewed"}));
    run(&mut s, "view.expandAll", json!({}));
    pick(&mut s, bo.0);
    run(&mut s, "view.collapseGroup", json!({"label": "From: Bo"}));
    open(&mut s, sent);
    open(&mut s, inbox);
    assert_eq!(collapsed(&s, inbox), 1);
    // Module folders too.
    let tasks = role(&s, FolderRole::Tasks);
    open(&mut s, tasks);
    run(&mut s, "view.changeView", json!({"name": "Simple List"}));
    task(&mut s, json!({"subject": "A", "priority": "high"}));
    task(&mut s, json!({"subject": "B", "priority": "low"}));
    run(&mut s, "view.viewSettings", json!({"groupBy": ["priority"], "expandGroups": "collapsed"}));
    run(&mut s, "view.expandAll", json!({}));
    assert_eq!(collapsed(&s, tasks), 0);
    open(&mut s, inbox);
    open(&mut s, tasks);
    assert_eq!(collapsed(&s, tasks), 2, "a task folder's groups close when it opens");
}
