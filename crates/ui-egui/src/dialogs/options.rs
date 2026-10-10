//! The Options window (plan/outlook/options.md): 14 categories in a left list, each page a list of sections
//! and settings. Settings the engine holds (theme, ribbon style, search scope, status bar, favourites, time
//! zone) are applied with `options.set` / commands on OK; the others are kept as UI settings
//! ([`super::ui_pref`]) until the engine grows them. Buttons open the matching dialogs.

use egui::{Context, RichText};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, primary, take_replies, window, with_state};
use crate::App;

/// One setting row.
enum S {
    Head(&'static str),
    Note(&'static str),
    Check(&'static str, &'static str, bool),
    Radio(&'static str, &'static str, &'static [&'static str], usize),
    Combo(&'static str, &'static str, &'static [&'static str], usize),
    Text(&'static str, &'static str, &'static str),
    Num(&'static str, &'static str, i64, i64, i64),
    /// Button: label and what it does.
    Button(&'static str, Act),
}

#[derive(Clone, Copy)]
enum Act {
    /// Open a dialog by id (params `{}`).
    Dialog(&'static str),
    /// Run a command with `{}`.
    Cmd(&'static str),
    /// Pick a folder into the given setting key.
    PickFolder(&'static str),
    /// Not available yet: show this note.
    Info(&'static str),
}

use Act::*;
use S::*;

const PAGES: [(&str, &str, &str); 14] = [
    ("general", "General", "General options for working with MailCraft."),
    ("mail", "Mail", "Change the settings for messages you create and receive."),
    ("calendar", "Calendar", "Change the settings for calendars, meetings and time zones."),
    ("groups", "Groups", "Options for working with groups."),
    ("people", "People", "Change how you work with your contacts."),
    ("tasks", "Tasks", "Change the settings that track your tasks and to-do items."),
    ("search", "Search", "Change how items are searched."),
    ("language", "Language", "Set the display, editing and proofing languages."),
    ("accessibility", "Accessibility", "Make MailCraft easier to use."),
    ("advanced", "Advanced", "More options for working with MailCraft."),
    ("customizeRibbon", "Customize Ribbon", "Choose how the ribbon looks."),
    ("quickAccessToolbar", "Quick Access Toolbar", "Choose the commands on the Quick Access Toolbar."),
    ("addIns", "Add-ins", "View and manage add-ins."),
    ("trustCenter", "Trust Center", "Keep your messages and computer safe."),
];

const ON_OFF_NEEDS_SERVER: &str = "Needs a mail server; not available for local data files.";

fn page(id: &str) -> Vec<S> {
    match id {
        "general" => vec![
            Head("User Interface options"),
            Radio(
                "general.displays",
                "When using multiple displays:",
                &["Optimize for best appearance", "Optimize for compatibility"],
                0,
            ),
            Check("general.miniToolbar", "Show Mini Toolbar on selection", true),
            Check("general.livePreview", "Enable Live Preview", true),
            Combo(
                "general.screenTips",
                "ScreenTip style:",
                &[
                    "Show feature descriptions in ScreenTips",
                    "Don't show feature descriptions in ScreenTips",
                    "Don't show ScreenTips",
                ],
                0,
            ),
            Head("Personalize your copy of MailCraft"),
            Text("general.userName", "User name:", ""),
            Text("general.initials", "Initials:", ""),
            Combo("engine:theme", "Theme:", &["Light", "Dark"], 0),
            Check("general.keepMessageBackground", "Never change the message background color", false),
            Head("Start up options"),
            Combo(
                "general.startup",
                "When MailCraft opens:",
                &[
                    "Ask me if I want to reopen previous items",
                    "Always reopen previous items",
                    "Never reopen previous items",
                ],
                0,
            ),
            Head("Attachment options"),
            Radio(
                "general.cloudAttach",
                "For files from cloud storage:",
                &[
                    "Ask me how I want to attach them every time",
                    "Always share them as links",
                    "Always attach them as copies",
                ],
                2,
            ),
        ],
        "mail" => vec![
            Head("Compose messages"),
            Button("Editor Options...", Info("Proofing and AutoCorrect options are not available yet.")),
            Combo(
                "mail.composeFormat",
                "Compose messages in this format:",
                &["HTML", "Rich Text", "Plain Text"],
                0,
            ),
            Check("mail.spellBeforeSend", "Always check spelling before sending", false),
            Check("mail.ignoreOriginal", "Ignore original message text in reply or forward", true),
            Button("Signatures...", Dialog("signatures")),
            Button("Stationery and Fonts...", Dialog("stationery")),
            Head("MailCraft panes"),
            Button("Reading Pane...", Dialog("readingPaneOptions")),
            Head("Message arrival"),
            Check("mail.arrivalSound", "Play a sound", true),
            Check("mail.arrivalPointer", "Briefly change the mouse pointer", false),
            Check("mail.arrivalEnvelope", "Show an envelope icon in the taskbar", true),
            Check("mail.desktopAlert", "Display a Desktop Alert", true),
            Head("Conversation Clean Up"),
            Text("mail.cleanupFolder", "Cleaned-up items go to this folder (empty = Deleted Items):", ""),
            Button("Browse...", PickFolder("mail.cleanupFolder")),
            Check(
                "mail.cleanupHierarchy",
                "When cleaning sub-folders, recreate the folder hierarchy in the destination folder",
                false,
            ),
            Check("mail.cleanupKeepUnread", "Don't move unread messages", false),
            Check("mail.cleanupKeepCategorized", "Don't move categorized messages", true),
            Check("mail.cleanupKeepFlagged", "Don't move flagged messages", true),
            Check("mail.cleanupKeepSigned", "Don't move digitally-signed messages", true),
            Check(
                "mail.cleanupKeepOriginal",
                "When a reply modifies a message, don't move the original",
                true,
            ),
            Head("Replies and forwards"),
            Check("mail.suggestedReplies", "Show suggested replies", false),
            Check("mail.repliesNewWindow", "Open replies and forwards in a new window", false),
            Check("mail.closeOriginal", "Close original message window when replying or forwarding", false),
            Combo(
                "mail.replyText",
                "When replying to a message:",
                &[
                    "Include original message text",
                    "Do not include original message",
                    "Attach original message",
                    "Prefix each line of the original message",
                ],
                0,
            ),
            Combo(
                "mail.forwardText",
                "When forwarding a message:",
                &[
                    "Include original message text",
                    "Attach original message",
                    "Prefix each line of the original message",
                ],
                0,
            ),
            Text("mail.plainPrefix", "Preface each line in a plain-text message with:", "> "),
            Head("Save messages"),
            Check("mail.autoSave", "Automatically save items that have not been sent", true),
            Num("mail.autoSaveMinutes", "Minutes between saves:", 3, 1, 99),
            Check("mail.saveForwarded", "Save forwarded messages", true),
            Check("mail.saveSent", "Save copies of messages in the Sent Items folder", true),
            Head("Send messages"),
            Combo("mail.defaultImportance", "Default Importance level:", &["Normal", "High", "Low"], 0),
            Combo(
                "mail.defaultSensitivity",
                "Default Sensitivity level:",
                &["Normal", "Personal", "Private", "Confidential"],
                0,
            ),
            Check("mail.commasSeparate", "Commas can be used to separate multiple message recipients", false),
            Check("mail.autoNameCheck", "Automatic name checking", true),
            Check("mail.ctrlEnterSends", "CTRL + ENTER sends a message", true),
            Check(
                "mail.autoComplete",
                "Use Auto-Complete List to suggest names in the To, Cc and Bcc lines",
                true,
            ),
            Check(
                "mail.attachmentReminder",
                "Warn me when I send a message that may be missing an attachment",
                true,
            ),
            Head("Tracking"),
            Check("mail.requestDelivery", "Request a delivery receipt for all messages sent", false),
            Check("mail.requestRead", "Request a read receipt for all messages sent", false),
            Radio(
                "mail.readReceiptReply",
                "For any message received that includes a read receipt request:",
                &[
                    "Always send a read receipt",
                    "Never send a read receipt",
                    "Ask each time whether to send a read receipt",
                ],
                2,
            ),
            Head("Message format"),
            Num("mail.wrapAt", "Automatically wrap text at character:", 76, 30, 132),
            Check("mail.removeLineBreaks", "Remove extra line breaks in plain text messages", true),
            Head("Other"),
            Check(
                "mail.pasteOptions",
                "Show Paste Options button when content is pasted into a message",
                true,
            ),
            Combo(
                "mail.afterMove",
                "After moving or deleting an open item:",
                &["return to the current folder", "open the previous item", "open the next item"],
                0,
            ),
            Check("mail.markReadOnDelete", "Mark messages as read when deleted", false),
            Check("mail.highlightFlagged", "Highlight flagged items in the message list", true),
        ],
        "calendar" => vec![
            Head("Work time"),
            Text("calendar.workStart", "Start time:", "08:00"),
            Text("calendar.workEnd", "End time:", "17:00"),
            Combo(
                "calendar.firstDay",
                "First day of week:",
                &["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
                0,
            ),
            Combo(
                "calendar.firstWeek",
                "First week of year:",
                &["Starts on Jan 1", "First 4-day week", "First full week"],
                0,
            ),
            Head("Calendar options"),
            Combo(
                "calendar.defaultDuration",
                "Default duration for new appointments and meetings:",
                &["30 minutes", "15 minutes", "45 minutes", "60 minutes", "90 minutes", "120 minutes"],
                0,
            ),
            Check("calendar.shorten", "Shorten appointments and meetings", false),
            Check("calendar.defaultReminders", "Default reminders", true),
            Combo(
                "calendar.reminderTime",
                "Reminder:",
                &[
                    "15 minutes",
                    "0 minutes",
                    "5 minutes",
                    "10 minutes",
                    "30 minutes",
                    "1 hour",
                    "2 hours",
                    "1 day",
                ],
                0,
            ),
            Check("calendar.proposeTimes", "Allow attendees to propose new times for meetings", true),
            Button("Add Holidays...", Info("Holiday calendars are not bundled yet.")),
            Check(
                "calendar.iCalendar",
                "Use the iCalendar format for meeting requests outside your organization",
                true,
            ),
            Check(
                "calendar.bell",
                "Show bell icon on the calendar for appointments and meetings with reminders",
                false,
            ),
            Head("Display options"),
            Check(
                "calendar.weekNumbers",
                "Show week numbers in the month view and in the Date Navigator",
                false,
            ),
            Check("calendar.scheduleFree", "When in Schedule View, show free appointments", false),
            Head("Time zones"),
            Text("calendar.tzLabel", "Label:", ""),
            Combo("engine:timeZone", "Time zone:", TIME_ZONES, 12),
            Check("calendar.secondZone", "Show a second time zone", false),
            Head("Automatic accept or decline"),
            Button("Auto Accept/Decline...", Info(ON_OFF_NEEDS_SERVER)),
        ],
        "groups" => vec![
            Head("User Interface options"),
            Check("groups.headerInSpace", "Show the group header in the group space", true),
            Check("groups.headerInInbox", "Show the group header in the Inbox", true),
            Note("Groups are a service of hosted mail systems; MailCraft has none to show yet."),
        ],
        "people" => vec![
            Head("Names and filing"),
            Combo(
                "people.fullNameOrder",
                "Default \"Full Name\" order:",
                &["First (Middle) Last", "Last First", "First Last1 Last2"],
                0,
            ),
            Combo(
                "people.fileAsOrder",
                "Default \"File As\" order:",
                &["Last, First", "First Last", "Company", "Last, First (Company)", "Company (Last, First)"],
                0,
            ),
            Check("people.checkDuplicates", "Check for duplicates when saving new contacts", true),
            Head("Contacts index"),
            Check("people.additionalIndex", "Show an additional index", false),
            Head("Online status and photographs"),
            Check("people.photos", "Show user photographs when available", true),
            Check("people.namesOnly", "Show only names in the People peek", false),
        ],
        "tasks" => vec![
            Head("Task options"),
            Check("tasks.reminders", "Set reminders on tasks with due dates", false),
            Combo(
                "tasks.reminderTime",
                "Default reminder time:",
                &["8:00 AM", "9:00 AM", "12:00 PM", "1:00 PM", "5:00 PM"],
                0,
            ),
            Check(
                "tasks.keepCopies",
                "Keep my task list updated with copies of tasks I assign to other people",
                true,
            ),
            Check("tasks.statusReport", "Send status report when I complete an assigned task", true),
            Button("Quick Click...", Cmd("mail.setQuickClickFlag")),
            Head("Work hours"),
            Num("tasks.hoursPerDay", "Task working hours per day:", 8, 1, 24),
            Num("tasks.hoursPerWeek", "Task working hours per week:", 40, 1, 168),
        ],
        "search" => vec![
            Head("Results"),
            Radio(
                "engine:searchScope",
                "Include results only from:",
                &[
                    "Current folder",
                    "Current folder and its subfolders",
                    "Current mailbox",
                    "All mailboxes",
                    "All MailCraft items",
                ],
                0,
            ),
            Check(
                "search.includeDeleted",
                "Include messages from the Deleted Items folder when searching in All Items",
                false,
            ),
            Check("search.highlight", "Highlight search terms in the results", true),
            Check("search.relevance", "Show most relevant search results on top", true),
            Button("Indexing Options...", Cmd("search.searchTools")),
        ],
        "language" => vec![
            Head("Display language"),
            Combo(
                "language.display",
                "Display language:",
                &[
                    "English (United States)",
                    "English (United Kingdom)",
                    "Français (Canada)",
                    "Español",
                    "Deutsch",
                ],
                0,
            ),
            Head("Authoring languages and proofing"),
            Combo(
                "language.authoring",
                "Preferred editing language:",
                &[
                    "English (United States)",
                    "English (Canada)",
                    "English (United Kingdom)",
                    "French (Canada)",
                    "French (France)",
                    "Spanish",
                    "German",
                ],
                0,
            ),
            Head("Translation"),
            Radio(
                "language.translate",
                "Messages received in other languages:",
                &["Always translate", "Ask me before translating", "Never translate"],
                1,
            ),
            Combo(
                "language.translateTo",
                "Translate messages into:",
                &["English", "French", "Spanish", "German"],
                0,
            ),
        ],
        "accessibility" => vec![
            Head("Accessibility checker options"),
            Radio(
                "access.warnings",
                "Show accessibility warnings:",
                &[
                    "While I work",
                    "When a recipient prefers accessible content, the message is high importance or goes to many people",
                    "Only when I open the accessibility checker",
                ],
                1,
            ),
            Head("Feedback options"),
            Check("access.sound", "Provide feedback with sound", false),
            Head("Application display options"),
            Check(
                "access.noAutoExpand",
                "Don't automatically expand conversations when using the keyboard to change messages",
                false,
            ),
            Check("access.readAloud", "Show Read Aloud", true),
            Head("Document display options"),
            Button("Stationery and Fonts...", Dialog("stationery")),
            Head("Automatic Alt Text"),
            Check("access.autoAlt", "Automatically generate alt text for me", false),
        ],
        "advanced" => vec![
            Head("MailCraft panes"),
            Button("Reading Pane...", Dialog("readingPaneOptions")),
            Check("engine:favorites", "Show Favorites in the folder pane", true),
            Check("engine:statusBar", "Show the status bar", true),
            Head("Start and exit"),
            Text("advanced.startFolder", "Start MailCraft in this folder:", "Inbox"),
            Button("Browse...", PickFolder("advanced.startFolder")),
            Check("advanced.emptyOnExit", "Empty Deleted Items folders when exiting MailCraft", false),
            Head("AutoArchive"),
            Button("AutoArchive Settings...", Cmd("folder.autoArchiveSettings")),
            Head("Reminders"),
            Check("advanced.showReminders", "Show reminders", true),
            Check("advanced.reminderSound", "Play reminder sound", true),
            Check("advanced.remindersOnTop", "Show reminders on top of other windows", false),
            Check("advanced.dismissPast", "Automatically dismiss reminders for past calendar events", false),
            Head("Export"),
            Button("Export", Cmd("file.openExport")),
            Head("RSS Feeds"),
            Check("advanced.rssAsNew", "Any RSS Feed item that is updated appears as new", false),
            Head("Send and receive"),
            Button("Send/Receive...", Cmd("sendReceive.showProgress")),
            Check("advanced.sendImmediately", "Send immediately when connected", true),
            Head("International options"),
            Check("advanced.englishFlags", "Use English for message flag labels", false),
            Check("advanced.englishHeaders", "Use English for message headers on replies and forwards", true),
            Check("advanced.autoEncoding", "Automatically select encoding for outgoing messages", true),
            Combo(
                "advanced.encoding",
                "Preferred encoding for outgoing messages:",
                &["Unicode (UTF-8)", "Western European (ISO)", "Western European (Windows)"],
                0,
            ),
            Check("advanced.idn", "Allow international domain names in email addresses", true),
            Head("Other"),
            Check(
                "advanced.confirmPermanentDelete",
                "Prompt for confirmation before permanently deleting items",
                true,
            ),
            Check(
                "advanced.logging",
                "Enable troubleshooting logging (requires restarting MailCraft)",
                false,
            ),
            Check("advanced.animations", "Use animations when expanding conversations and groups", true),
        ],
        "customizeRibbon" => vec![
            Head("Ribbon style"),
            Radio("engine:ribbonStyle", "Ribbon layout:", &["Simplified Ribbon", "Classic Ribbon"], 0),
            Check("engine:ribbonCollapsed", "Show tabs only (collapse the ribbon)", false),
            Note("Adding tabs, groups and commands to the ribbon is not available yet."),
            Button("Ribbon Display Options...", Cmd("app.toggleRibbonMode")),
        ],
        "quickAccessToolbar" => vec![
            Check("qat.show", "Show Quick Access Toolbar", true),
            Combo("qat.position", "Toolbar Position:", &["Above Ribbon", "Below Ribbon"], 0),
            Check("qat.labels", "Always show command labels", false),
            Button("Customize Quick Access Toolbar...", Cmd("app.quickAccess")),
        ],
        "addIns" => {
            vec![Head("Add-ins"), Note("No add-ins are installed. MailCraft does not load add-ins yet.")]
        }
        "trustCenter" => vec![
            Head("Security & more"),
            Note("MailCraft never loads remote content in messages unless you allow it."),
            Check("trust.blockImages", "Don't download pictures automatically in HTML email messages", true),
            Check("trust.plainText", "Read all standard mail in plain text", false),
            Check("trust.warnLinks", "Warn before opening links in messages from unknown senders", true),
            Button("Trust Center Settings...", Info("More privacy settings will live here.")),
        ],
        _ => vec![],
    }
}

/// UTC offsets offered for the time zone (label, minutes).
const TIME_ZONES: &[&str] = &[
    "(UTC-12:00)",
    "(UTC-11:00)",
    "(UTC-10:00) Hawaii",
    "(UTC-09:00) Alaska",
    "(UTC-08:00) Pacific Time",
    "(UTC-07:00) Mountain Time",
    "(UTC-06:00) Central Time",
    "(UTC-05:00) Eastern Time",
    "(UTC-04:00) Atlantic Time",
    "(UTC-03:30) Newfoundland",
    "(UTC-03:00) Brasilia",
    "(UTC-01:00) Azores",
    "(UTC) Coordinated Universal Time",
    "(UTC+01:00) Central Europe",
    "(UTC+02:00) Eastern Europe",
    "(UTC+03:00) Moscow",
    "(UTC+04:00) Gulf",
    "(UTC+05:30) India",
    "(UTC+07:00) Bangkok",
    "(UTC+08:00) Beijing, Perth",
    "(UTC+09:00) Tokyo",
    "(UTC+10:00) Sydney",
    "(UTC+12:00) Auckland",
];
const TZ_MINUTES: [i64; 23] = [
    -720, -660, -600, -540, -480, -420, -360, -300, -240, -210, -180, -60, 0, 60, 120, 180, 240, 330, 420,
    480, 540, 600, 720,
];

const SEARCH_SCOPES: [&str; 5] =
    ["currentFolder", "subfolders", "currentMailbox", "allMailboxes", "allOutlookItems"];

/// Current value of an engine-held setting.
fn engine_value(app: &App, key: &str) -> Value {
    let v = &app.s.view;
    match key {
        "engine:theme" => json!(if v.dark { "Dark" } else { "Light" }),
        "engine:timeZone" => {
            let i = TZ_MINUTES.iter().position(|m| *m == i64::from(app.s.tz_offset_min)).unwrap_or(12);
            json!(TIME_ZONES.get(i).copied().unwrap_or("(UTC) Coordinated Universal Time"))
        }
        "engine:searchScope" => {
            let name = scope_name(&app.s);
            json!(SEARCH_SCOPES.iter().position(|s| *s == name).unwrap_or(0))
        }
        "engine:favorites" => json!(v.favorites_visible),
        "engine:statusBar" => json!(v.status_bar),
        "engine:ribbonStyle" => json!(usize::from(!v.simplified_ribbon)),
        "engine:ribbonCollapsed" => json!(v.ribbon_collapsed),
        _ => Value::Null,
    }
}

/// Applies a changed engine-held setting.
fn apply_engine(app: &mut App, key: &str, v: &Value) {
    match key {
        "engine:theme" => {
            let dark = v.as_str() == Some("Dark");
            app.run("options.set", json!({"key": "view.darkMode", "value": dark}));
        }
        "engine:timeZone" => {
            let i = v.as_str().and_then(|s| TIME_ZONES.iter().position(|t| *t == s)).unwrap_or(12);
            let off = TZ_MINUTES.get(i).copied().unwrap_or(0);
            app.run("calendar.changeTimeZone", json!({"offsetMin": off}));
        }
        "engine:searchScope" => {
            let i = v.as_u64().and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
            app.run("options.set", json!({"key": "search.scope", "value": SEARCH_SCOPES.get(i).copied().unwrap_or("currentFolder")}));
        }
        "engine:favorites" => {
            app.run("options.set", json!({"key": "view.favorites", "value": v.as_bool().unwrap_or(true)}));
        }
        "engine:statusBar" => {
            app.run("options.set", json!({"key": "view.statusBar", "value": v.as_bool().unwrap_or(true)}));
        }
        "engine:ribbonStyle" => {
            let classic = v.as_u64() == Some(1);
            app.run("options.set", json!({"key": "view.simplifiedRibbon", "value": !classic}));
        }
        "engine:ribbonCollapsed" => {
            app.run(
                "options.set",
                json!({"key": "view.ribbonCollapsed", "value": v.as_bool().unwrap_or(false)}),
            );
        }
        _ => {}
    }
}

fn scope_name(s: &mailcraft_engine::Session) -> &'static str {
    use mailcraft_engine::SearchScope;
    match s.view.search_scope {
        SearchScope::CurrentFolder => "currentFolder",
        SearchScope::Subfolders => "subfolders",
        SearchScope::CurrentMailbox => "currentMailbox",
        SearchScope::AllMailboxes => "allMailboxes",
        SearchScope::AllOutlookItems => "allOutlookItems",
    }
}

#[derive(Clone, Default)]
struct OptState {
    page: usize,
    /// Edited values (key → value); untouched keys read their current value.
    edits: Map<String, Value>,
    note: String,
}

fn current(ctx: &Context, app: &App, st: &OptState, key: &str, default: Value) -> Value {
    if let Some(v) = st.edits.get(key) {
        return v.clone();
    }
    if key.starts_with("engine:") {
        let v = engine_value(app, key);
        if !v.is_null() {
            return v;
        }
    }
    common::pref(ctx, &format!("options.{key}")).unwrap_or(default)
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let want = d.str("page");
    let init = |_: &App| OptState {
        page: PAGES.iter().position(|(id, _, _)| *id == want).unwrap_or(0),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if let Some(key) = k.strip_prefix("folder:")
                && let Some(f) = v.as_u64()
            {
                st.edits
                    .insert(key.to_owned(), json!(app.s.mailbox.folder_path(mailcraft_model::FolderId(f))));
            }
        }
        window(ctx, app, d, "MailCraft Options", 820.0, |ui, app| {
            let mut out = Out::Keep;
            let h = (ctx.content_rect().height() - 170.0).clamp(240.0, 560.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    for (i, (_, title, _)) in PAGES.iter().enumerate() {
                        if ui
                            .add(
                                egui::Button::selectable(st.page == i, *title)
                                    .min_size(egui::vec2(166.0, 24.0)),
                            )
                            .clicked()
                        {
                            st.page = i;
                            st.note.clear();
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some((pid, _, desc)) = PAGES.get(st.page).copied() else { return };
                    ui.label(RichText::new(desc).strong().size(15.0));
                    egui::ScrollArea::vertical()
                        .id_salt(("opts", uid, pid))
                        .max_height(h)
                        .min_scrolled_height(h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(o) = page_ui(ctx, ui, app, uid, st, pid) {
                                out = o;
                            }
                        });
                });
            });
            if !st.note.is_empty() {
                muted(ui, app, &st.note);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, true, "OK") {
                    apply(ctx, app, st);
                    return Out::Close;
                }
                Out::Keep
            })
        })
    })
}

fn apply(ctx: &Context, app: &mut App, st: &OptState) {
    let mut prefs = common::prefs(ctx);
    for (k, v) in &st.edits {
        if k.starts_with("engine:") {
            if engine_value(app, k) != *v {
                apply_engine(app, k, v);
            }
        } else {
            prefs.values.insert(format!("options.{k}"), v.clone());
        }
    }
    common::set_prefs(ctx, prefs);
}

fn page_ui(
    ctx: &Context,
    ui: &mut egui::Ui,
    app: &mut App,
    uid: u64,
    st: &mut OptState,
    pid: &str,
) -> Option<Out> {
    let mut out = None;
    for s in page(pid) {
        match s {
            Head(t) => common::heading(ui, app, t),
            S::Note(t) => muted(ui, app, t),
            Check(key, label, def) => {
                let mut b = current(ctx, app, st, key, json!(def)).as_bool().unwrap_or(def);
                if ui.checkbox(&mut b, label).changed() {
                    st.edits.insert(key.to_owned(), json!(b));
                }
            }
            Radio(key, label, choices, def) => {
                ui.label(label);
                let cur = current(ctx, app, st, key, json!(def))
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(def);
                ui.indent((uid, key), |ui| {
                    for (i, c) in choices.iter().enumerate() {
                        if ui.radio(cur == i, *c).clicked() {
                            st.edits.insert(key.to_owned(), json!(i));
                        }
                    }
                });
            }
            Combo(key, label, choices, def) => {
                let fallback = choices.get(def).copied().unwrap_or_default();
                let mut cur =
                    current(ctx, app, st, key, json!(fallback)).as_str().unwrap_or(fallback).to_owned();
                ui.horizontal(|ui| {
                    ui.label(label);
                    if common::combo(ui, ("oc", uid, key), &mut cur, choices, 260.0) {
                        st.edits.insert(key.to_owned(), json!(cur));
                    }
                });
            }
            Text(key, label, def) => {
                let mut t = current(ctx, app, st, key, json!(def)).as_str().unwrap_or(def).to_owned();
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui.add(egui::TextEdit::singleline(&mut t).desired_width(220.0)).changed() {
                        st.edits.insert(key.to_owned(), json!(t));
                    }
                });
            }
            Num(key, label, def, min, max) => {
                let mut n = current(ctx, app, st, key, json!(def)).as_i64().unwrap_or(def);
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui.add(egui::DragValue::new(&mut n).range(min..=max)).changed() {
                        st.edits.insert(key.to_owned(), json!(n));
                    }
                });
            }
            Button(label, act) => {
                if ui.button(label).clicked() {
                    match act {
                        Dialog(name) => out = Some(Out::open(name, json!({}))),
                        Cmd(id) => {
                            app.run(id, json!({}));
                        }
                        PickFolder(key) => {
                            out = Some(Out::open(
                                "pickFolder",
                                common::child(uid, &format!("folder:{key}"), json!({"pick": "any"})),
                            ));
                        }
                        Act::Info(t) => st.note = t.to_owned(),
                    }
                }
            }
        }
    }
    out
}
